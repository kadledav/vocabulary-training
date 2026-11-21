//! Neural audio (design §10.3; R-40, R-41, R-42): which clips exist, the
//! queue that makes them with the TTS service, the clip files, and what
//! the client may play.
//!
//! Owned state: tables `audio_clip` and `audio_engine` (migration 0006),
//! the directory `<cache dir>/audio`. Reads the catalog's speakable texts
//! ([`catalog::speak_sources`]) and the voice setting; writes nothing else.
//!
//! * **What should exist:** the word text (pack `speak`, else the headword)
//!   and every visible sentence of every word, in the voice of the setting.
//!   A clip is named by the hash of text, voice and the service's front-end
//!   and engine versions ([`key`]); identical texts share one clip.
//! * **Sync** (at start, after an import or a voice change, every 10
//!   minutes): queues missing clips and deletes clips and files no current
//!   text needs in any voice (garbage collection); clips of the other voice
//!   are kept while their text exists, so switching voices back is free.
//!   After a TTS version change the old clip of a text keeps playing until
//!   its replacement is ready, which then deletes it (replaced one by one).
//! * **Worker:** one task, one clip at a time (words first). Unreachable
//!   service: the worker waits (30 s doubling to 10 min) without spending a
//!   clip's attempts. A clip that fails while the service is up (5xx, an
//!   answer that is not audio or too large, a timeout) is retried after
//!   30 s, 2 min, 10 min, 1 h, 6 h, 24 h, then marked failed; a refusal
//!   (4xx) or a clip over 60 s fails at once. A connection that dies after
//!   the request was sent counts as an attempt and a crash: three crashes
//!   and the clip fails ("crashed the audio service"). A clip that failed
//!   waits behind the others, so one clip never blocks the queue. Settings
//!   can retry failed clips.
//! * **Storage:** at most [`CacheLimits::total_bytes`] of clips (2 GiB), and
//!   never closer than [`CacheLimits::reserve_bytes`] to a full disk (the
//!   database shares it): beyond either the worker makes no new clips,
//!   warns once, and Settings shows why.
//! * **Reconciliation** at start: a ready clip without its file goes back to
//!   the queue, a file without a clip row is deleted (R-85: the cache is
//!   regenerable).
//! * Without `VOCAB_TTS_URL` there is no `Audio` at all: the client keeps
//!   using the browser's speech (R-41).
//!
//! Logs carry hash prefixes, kinds, sizes and durations, never text (R-24).

mod client;
mod key;
mod queue;
mod store;

use std::collections::HashSet;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::Notify;

pub use client::{EndpointError, TtsEndpoint};

use crate::catalog::{self, SpeakSource, WordId};
use crate::clock::{Clock, Timestamp, format_timestamp};
use crate::db::{Db, DbError};
use crate::settings::{self, Voice};
use client::CallError;
use key::Versions;
use queue::{ClipKind, Wanted};
use store::Store;

/// Retry delays of a clip that failed while the service was up.
const RETRY_DELAYS: [Duration; 6] = [
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
    Duration::from_secs(3600),
    Duration::from_secs(6 * 3600),
    Duration::from_secs(24 * 3600),
];
const SYNC_INTERVAL: Duration = Duration::from_secs(600);
const IDLE_WAIT: Duration = Duration::from_secs(30);
const UNREACHABLE_FIRST_WAIT: Duration = Duration::from_secs(30);
const UNREACHABLE_MAX_WAIT: Duration = Duration::from_secs(600);
/// Route prefix of clip files (the router serves `/api/audio/clips/{hash}.mp3`).
const CLIP_URL_PREFIX: &str = "/api/audio/clips/";
/// Longest clip kept (the service caps a text at 600 characters, about
/// 40 s of speech; longer means something went wrong).
pub const MAX_CLIP_MS: i64 = 60_000;
/// Dropped connections while making one clip before it fails for good.
const MAX_CRASHES: i64 = 3;
/// What a clip that keeps killing the service fails with.
const CRASHED: &str = "crashed the audio service";

/// Storage limits of the clip cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLimits {
    /// Most bytes of clips altogether.
    pub total_bytes: u64,
    /// Free space always left on the disk (the database lives there too).
    pub reserve_bytes: u64,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            total_bytes: 2 << 30,
            reserve_bytes: 256 << 20,
        }
    }
}

/// Whether new clips can be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Storage {
    Ok,
    /// The clips reached [`CacheLimits::total_bytes`].
    CacheFull,
    /// The disk is nearly full.
    DiskLow,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("audio files: {0}")]
    Io(#[from] std::io::Error),
}

impl From<rusqlite::Error> for AudioError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(DbError::from(err))
    }
}

/// What the client may play for one text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClipRef {
    /// Set when a clip is ready (in the chosen voice, or else the other one).
    pub url: Option<String>,
    pub ready: bool,
    /// The clip is in the other voice (the chosen one is not ready yet).
    pub other_voice: bool,
    /// Some word of it is pronounced by a guess, not from a dictionary.
    pub approximate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SentenceClip {
    pub id: i64,
    #[serde(flatten)]
    pub clip: ClipRef,
}

/// The clips of one word: its word text and its visible sentences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WordClips {
    pub word: ClipRef,
    pub sentences: Vec<SentenceClip>,
}

/// Audio state for Settings (counts are for the chosen voice).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AudioStatus {
    /// The service's versions are known (it answered at least once).
    pub connected: bool,
    pub voice: Voice,
    #[serde(flatten)]
    pub counts: queue::Counts,
    pub storage: Storage,
    /// Bytes of all clips (every voice).
    pub cache_bytes: i64,
    pub cache_limit_bytes: u64,
}

/// What one worker step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A clip was made, or its attempt recorded.
    Worked,
    /// Nothing is due.
    Idle,
    /// The service cannot be reached.
    Unreachable,
    /// No room for new clips ([`Storage`]).
    Paused,
}

/// The audio service; cheap to clone.
#[derive(Debug, Clone)]
pub struct Audio {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    db: Db,
    store: Store,
    endpoint: TtsEndpoint,
    clock: Arc<dyn Clock>,
    wake: Notify,
    sync_requested: AtomicBool,
    limits: CacheLimits,
    /// The storage warning was logged (once per pause).
    storage_warned: AtomicBool,
}

fn now_plus(now: Timestamp, delay: Duration) -> String {
    let later = now
        .checked_add(jiff::SignedDuration::try_from(delay).unwrap_or(jiff::SignedDuration::ZERO))
        .unwrap_or(now);
    format_timestamp(later)
}

/// The wanted clips of the catalog in `voice` (`None`: nothing speakable).
fn wanted_for(sources: &[SpeakSource], versions: &Versions, voice: Voice) -> Vec<Wanted> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for source in sources {
        let texts = std::iter::once((ClipKind::Word, source.word.as_str())).chain(
            source
                .sentences
                .iter()
                .map(|(_, t)| (ClipKind::Sentence, t.as_str())),
        );
        for (kind, raw) in texts {
            let Some(text) = key::spoken(raw) else {
                continue;
            };
            let hash = key::clip_hash(versions, voice, &text);
            if seen.insert(hash.clone()) {
                out.push(Wanted {
                    hash,
                    text,
                    voice: key::tts_voice(voice),
                    kind,
                });
            }
        }
    }
    out
}

impl Audio {
    /// The service for `endpoint`, its files in `<cache_dir>/audio`, with
    /// the default [`CacheLimits`].
    pub fn new(
        db: Db,
        cache_dir: &Path,
        endpoint: TtsEndpoint,
        clock: Arc<dyn Clock>,
    ) -> std::io::Result<Self> {
        Self::with_limits(db, cache_dir, endpoint, clock, CacheLimits::default())
    }

    /// [`Audio::new`] with other storage limits.
    pub fn with_limits(
        db: Db,
        cache_dir: &Path,
        endpoint: TtsEndpoint,
        clock: Arc<dyn Clock>,
        limits: CacheLimits,
    ) -> std::io::Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                store: Store::open(cache_dir)?,
                endpoint,
                clock,
                wake: Notify::new(),
                sync_requested: AtomicBool::new(true),
                limits,
                storage_warned: AtomicBool::new(false),
            }),
        })
    }

    pub fn endpoint(&self) -> &TtsEndpoint {
        &self.inner.endpoint
    }

    fn now(&self) -> Timestamp {
        self.inner.clock.now()
    }

    /// Something changed (an import, the voice): sync and work soon.
    pub fn wake(&self) {
        self.inner.sync_requested.store(true, Ordering::SeqCst);
        self.inner.wake.notify_one();
    }

    /// Start the worker (reconciliation first).
    pub fn spawn(&self) -> tokio::task::JoinHandle<()> {
        let audio = self.clone();
        tokio::spawn(async move { audio.run().await })
    }

    async fn run(self) {
        match self.reconcile().await {
            Ok((requeued, removed)) if requeued + removed > 0 => {
                tracing::info!(requeued, removed, "audio files reconciled");
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "audio reconciliation failed"),
        }
        let mut unreachable_wait = UNREACHABLE_FIRST_WAIT;
        let mut last_sync: Option<tokio::time::Instant> = None;
        loop {
            let sync_due = self.inner.sync_requested.swap(false, Ordering::SeqCst)
                || last_sync.is_none_or(|t| t.elapsed() >= SYNC_INTERVAL);
            let step = if sync_due {
                match self.sync().await {
                    Ok(_) => {
                        last_sync = Some(tokio::time::Instant::now());
                        self.work_one().await
                    }
                    Err(SyncError::Unreachable(message)) => {
                        tracing::warn!(error = %message, endpoint = %self.inner.endpoint, "TTS service unreachable");
                        self.inner.sync_requested.store(true, Ordering::SeqCst);
                        Ok(Step::Unreachable)
                    }
                    Err(SyncError::Audio(err)) => Err(err),
                }
            } else {
                self.work_one().await
            };
            let wait = match step {
                Ok(Step::Worked) => {
                    unreachable_wait = UNREACHABLE_FIRST_WAIT;
                    continue;
                }
                Ok(Step::Idle) => {
                    unreachable_wait = UNREACHABLE_FIRST_WAIT;
                    self.idle_wait().await
                }
                Ok(Step::Unreachable) => {
                    let wait = unreachable_wait;
                    unreachable_wait = (unreachable_wait * 2).min(UNREACHABLE_MAX_WAIT);
                    wait
                }
                // An import or a voice change (wake) may free space.
                Ok(Step::Paused) => SYNC_INTERVAL,
                Err(err) => {
                    tracing::error!(error = %err, "audio worker step failed");
                    UNREACHABLE_FIRST_WAIT
                }
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = self.inner.wake.notified() => {}
            }
        }
    }

    /// How long to sleep with nothing due: until the next retry, at most
    /// [`IDLE_WAIT`].
    async fn idle_wait(&self) -> Duration {
        let next = self
            .inner
            .db
            .read(|conn| queue::next_due_at(conn).map_err(DbError::from))
            .await
            .ok()
            .flatten();
        let until = next
            .and_then(|t| t.parse::<Timestamp>().ok())
            .and_then(|t| t.duration_since(self.now()).try_into().ok())
            .unwrap_or(IDLE_WAIT);
        until.clamp(Duration::from_secs(1), IDLE_WAIT)
    }

    /// Ready clips without their file go back to the queue; files without
    /// a clip row are deleted. Returns (requeued, removed).
    pub async fn reconcile(&self) -> Result<(usize, usize), AudioError> {
        let store = self.inner.store.clone();
        let files: HashSet<String> = tokio::task::spawn_blocking(move || store.scan())
            .await
            .map_err(|err| std::io::Error::other(err.to_string()))??
            .into_iter()
            .collect();
        let now = format_timestamp(self.now());
        let present = files.clone();
        let (requeued, known) = self
            .inner
            .db
            .write(move |tx| {
                let mut requeued = 0;
                for hash in queue::ready_hashes(tx)? {
                    if !present.contains(&hash) {
                        queue::requeue(tx, &hash, &now)?;
                        requeued += 1;
                    }
                }
                let known: HashSet<String> = queue::all_hashes(tx)?.into_iter().collect();
                Ok::<_, AudioError>((requeued, known))
            })
            .await?;
        let mut removed = 0;
        for hash in files.difference(&known) {
            self.inner.store.remove(hash)?;
            removed += 1;
        }
        Ok((requeued, removed))
    }

    /// Ask the service for its versions, queue missing clips, delete clips
    /// no text needs any more. Returns (queued, deleted).
    pub async fn sync(&self) -> Result<(usize, usize), SyncError> {
        let versions = client::versions(&self.inner.endpoint)
            .await
            .map_err(|err| SyncError::Unreachable(err.to_string()))?;
        let now = format_timestamp(self.now());
        let (queued, deleted) = self
            .inner
            .db
            .write(move |tx| {
                let known = queue::versions(tx)?;
                if known.as_ref() != Some(&versions) {
                    queue::set_versions(tx, &versions, &now)?;
                }
                let sources = catalog::speak_sources(tx)?;
                let voice = settings::get(tx)?.voice;
                let wanted = wanted_for(&sources, &versions, voice);
                let every_voice: Vec<Wanted> = key::VOICES
                    .iter()
                    .flat_map(|v| wanted_for(&sources, &versions, *v))
                    .collect();
                // Current clips, and older clips standing in until their
                // replacement is ready.
                let mut keep = queue::stand_ins(tx, &every_voice)?;
                keep.extend(every_voice.into_iter().map(|w| w.hash));
                let queued = queue::ensure(tx, &wanted, &now)?;
                let deleted = queue::delete_except(tx, &keep)?;
                Ok::<_, AudioError>((queued, deleted))
            })
            .await
            .map_err(SyncError::Audio)?;
        for hash in &deleted {
            self.inner
                .store
                .remove(hash)
                .map_err(|err| SyncError::Audio(err.into()))?;
        }
        if queued + deleted.len() > 0 {
            tracing::info!(queued, deleted = deleted.len(), "audio clips synced");
        }
        Ok((queued, deleted.len()))
    }

    /// Whether new clips can be stored now (logs once when that changes).
    async fn storage(&self) -> Result<(Storage, i64), AudioError> {
        let used = self
            .inner
            .db
            .read(|conn| queue::total_bytes(conn).map_err(DbError::from))
            .await?;
        let free = self.inner.store.free_bytes()?;
        let limits = self.inner.limits;
        let storage = if u64::try_from(used).unwrap_or(0) >= limits.total_bytes {
            Storage::CacheFull
        } else if free < limits.reserve_bytes + client::MAX_CLIP_BYTES as u64 {
            Storage::DiskLow
        } else {
            Storage::Ok
        };
        Ok((storage, used))
    }

    fn note_storage(&self, storage: Storage, used: i64) {
        let paused = storage != Storage::Ok;
        let was = self.inner.storage_warned.swap(paused, Ordering::SeqCst);
        if paused && !was {
            tracing::warn!(
                ?storage,
                cache_bytes = used,
                limit_bytes = self.inner.limits.total_bytes,
                "no room for audio clips; no new clips until there is"
            );
        } else if !paused && was {
            tracing::info!(cache_bytes = used, "room for audio clips again");
        }
    }

    /// Record a failed attempt: retried later with backoff (behind the
    /// other clips), or failed for good after the last retry or the last
    /// allowed crash.
    async fn record_failure(
        &self,
        job: &queue::Job,
        tries: queue::Tries,
        error: &str,
        now: String,
    ) -> Result<(), AudioError> {
        let hash = job.hash.clone();
        let error = error.chars().take(200).collect::<String>();
        let next = (tries.crashes < MAX_CRASHES)
            .then(|| usize::try_from(tries.attempts - 1).ok())
            .flatten()
            .and_then(|i| RETRY_DELAYS.get(i))
            .map(|delay| now_plus(self.now(), *delay));
        let error = if tries.crashes >= MAX_CRASHES {
            CRASHED.to_owned()
        } else {
            error
        };
        self.inner
            .db
            .write(move |tx| {
                match &next {
                    Some(next) => queue::mark_retry(tx, &hash, tries, next, &error, &now)?,
                    None => queue::mark_failed(tx, &hash, tries, &error, &now)?,
                }
                Ok::<_, DbError>(())
            })
            .await?;
        Ok(())
    }

    async fn fail_now(&self, job: &queue::Job, error: &str, now: String) -> Result<(), AudioError> {
        let hash = job.hash.clone();
        let tries = queue::Tries {
            attempts: job.attempts + 1,
            crashes: job.crashes,
        };
        let error = error.to_owned();
        self.inner
            .db
            .write(move |tx| {
                queue::mark_failed(tx, &hash, tries, &error, &now).map_err(DbError::from)
            })
            .await?;
        Ok(())
    }

    /// Make the next due clip.
    pub async fn work_one(&self) -> Result<Step, AudioError> {
        let (storage, used) = self.storage().await?;
        self.note_storage(storage, used);
        if storage != Storage::Ok {
            return Ok(Step::Paused);
        }
        let now = format_timestamp(self.now());
        let job = {
            let now = now.clone();
            self.inner
                .db
                .read(move |conn| queue::next_due(conn, &now).map_err(DbError::from))
                .await?
        };
        let Some(job) = job else {
            return Ok(Step::Idle);
        };
        let started = std::time::Instant::now();
        let result = client::speak(&self.inner.endpoint, &job.text, &job.voice).await;
        let short = job.hash[..8].to_owned();
        match result {
            Ok(clip) if clip.duration_ms > MAX_CLIP_MS => {
                let error = format!("clip longer than {} s", MAX_CLIP_MS / 1000);
                self.fail_now(&job, &error, now).await?;
                tracing::warn!(clip = %short, audio_ms = clip.duration_ms, "audio clip too long; failed");
                Ok(Step::Worked)
            }
            Ok(clip) => {
                let store = self.inner.store.clone();
                let hash = job.hash.clone();
                let bytes = clip.mp3.len();
                tokio::task::spawn_blocking(move || store.write(&hash, &clip.mp3))
                    .await
                    .map_err(|err| std::io::Error::other(err.to_string()))??;
                let (duration, approximate) = (clip.duration_ms, clip.approximate);
                let hash = job.hash.clone();
                let size = i64::try_from(bytes).unwrap_or(i64::MAX);
                let replaced = self
                    .inner
                    .db
                    .write(move |tx| {
                        queue::mark_ready(tx, &hash, size, duration, approximate, &now)?;
                        queue::delete_replaced(tx, &hash).map_err(DbError::from)
                    })
                    .await?;
                for old in &replaced {
                    self.inner.store.remove(old)?;
                }
                tracing::info!(
                    clip = %short,
                    bytes,
                    audio_ms = duration,
                    approximate,
                    replaced = replaced.len(),
                    took_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    "audio clip ready"
                );
                Ok(Step::Worked)
            }
            Err(CallError::Refused(code)) => {
                self.fail_now(&job, &code, now).await?;
                tracing::warn!(clip = %short, error = %code, "audio clip refused by the TTS service");
                Ok(Step::Worked)
            }
            Err(CallError::Dropped(message)) => {
                // The service took the text and the connection died: it may
                // have crashed on it (and be restarting now). An attempt and
                // a crash, whatever the service says afterwards.
                let tries = queue::Tries {
                    attempts: job.attempts + 1,
                    crashes: job.crashes + 1,
                };
                self.record_failure(&job, tries, &message, now).await?;
                if tries.crashes >= MAX_CRASHES {
                    tracing::error!(clip = %short, crashes = tries.crashes, "audio clip failed: it crashed the TTS service");
                } else {
                    tracing::warn!(clip = %short, crashes = tries.crashes, error = %message, "TTS service dropped the connection; retry later");
                }
                Ok(Step::Worked)
            }
            Err(CallError::Temporary(message)) => {
                // A service that is down does not cost the clip an attempt.
                if client::versions(&self.inner.endpoint).await.is_err() {
                    tracing::warn!(error = %message, "TTS service unreachable");
                    return Ok(Step::Unreachable);
                }
                let tries = queue::Tries {
                    attempts: job.attempts + 1,
                    crashes: job.crashes,
                };
                self.record_failure(&job, tries, &message, now).await?;
                tracing::warn!(clip = %short, attempts = tries.attempts, error = %message, "audio clip failed; retry later");
                Ok(Step::Worked)
            }
        }
    }

    /// The clips of one word (`None`: no such word).
    pub async fn word_clips(&self, word: WordId) -> Result<Option<WordClips>, AudioError> {
        self.inner
            .db
            .read(move |conn| {
                let Some(source) = catalog::speak_source(conn, word)? else {
                    return Ok(None);
                };
                let versions = queue::versions(conn)?;
                let voice = settings::get(conn)?.voice;
                let clip = |raw: &str| -> Result<ClipRef, AudioError> {
                    let none = ClipRef {
                        url: None,
                        ready: false,
                        other_voice: false,
                        approximate: false,
                    };
                    let (Some(versions), Some(text)) = (versions.as_ref(), key::spoken(raw)) else {
                        return Ok(none);
                    };
                    for (candidate, other) in [(voice, false), (other_voice(voice), true)] {
                        let hash = key::clip_hash(versions, candidate, &text);
                        let current = queue::clip_state(conn, &hash)?
                            .filter(|state| state.ready)
                            .map(|state| queue::Playable {
                                hash,
                                approximate: state.approximate,
                            });
                        // Else an older version's clip until this one is made.
                        let playable = match current {
                            Some(current) => Some(current),
                            None => queue::ready_by_text(conn, key::tts_voice(candidate), &text)?,
                        };
                        if let Some(playable) = playable {
                            return Ok(ClipRef {
                                url: Some(format!("{CLIP_URL_PREFIX}{}.mp3", playable.hash)),
                                ready: true,
                                other_voice: other,
                                approximate: playable.approximate,
                            });
                        }
                    }
                    Ok(none)
                };
                let word_clip = clip(&source.word)?;
                let sentences = source
                    .sentences
                    .iter()
                    .map(|(id, text)| {
                        Ok(SentenceClip {
                            id: *id,
                            clip: clip(text)?,
                        })
                    })
                    .collect::<Result<Vec<_>, AudioError>>()?;
                Ok(Some(WordClips {
                    word: word_clip,
                    sentences,
                }))
            })
            .await
    }

    /// Counts for Settings.
    pub async fn status(&self) -> Result<AudioStatus, AudioError> {
        let (storage, cache_bytes) = self.storage().await?;
        let cache_limit_bytes = self.inner.limits.total_bytes;
        self.inner
            .db
            .read(move |conn| {
                let voice = settings::get(conn)?.voice;
                let versions = queue::versions(conn)?;
                let counts = match &versions {
                    Some(versions) => {
                        let sources = catalog::speak_sources(conn)?;
                        let hashes: HashSet<String> = wanted_for(&sources, versions, voice)
                            .into_iter()
                            .map(|w| w.hash)
                            .collect();
                        queue::counts(conn, &hashes)?
                    }
                    None => queue::Counts::default(),
                };
                Ok(AudioStatus {
                    connected: versions.is_some(),
                    voice,
                    counts,
                    storage,
                    cache_bytes,
                    cache_limit_bytes,
                })
            })
            .await
    }

    /// Failed clips get a fresh start; returns how many.
    pub async fn retry_failed(&self) -> Result<usize, AudioError> {
        let now = format_timestamp(self.now());
        let count = self
            .inner
            .db
            .write(move |tx| queue::retry_failed(tx, &now).map_err(DbError::from))
            .await?;
        self.wake();
        Ok(count)
    }

    /// A clip's file, by its hash (from the URL; validated).
    pub fn open_clip(&self, hash: &str) -> std::io::Result<Option<File>> {
        self.inner.store.open_clip(hash)
    }

    /// Whether `hash` is a well-formed clip hash.
    pub fn is_clip_hash(hash: &str) -> bool {
        key::is_clip_hash(hash)
    }
}

fn other_voice(voice: Voice) -> Voice {
    match voice {
        Voice::Us => Voice::Uk,
        Voice::Uk => Voice::Us,
    }
}

/// Why a sync did not happen.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("TTS service unreachable: {0}")]
    Unreachable(String),
    #[error(transparent)]
    Audio(AudioError),
}
