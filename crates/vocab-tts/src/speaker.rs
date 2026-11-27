//! The speaking pipeline: text → phonemes → engine → clip, and the queue
//! in front of it.
//!
//! [`Speaker`] is the queue: one synthesis at a time (the engine uses all
//! its threads for one request); a few more requests may wait, anything
//! beyond is refused at once as busy, so a flood cannot queue unbounded
//! work. A request keeps its slot until its work is finished, also when the
//! client has gone (a timed-out request does not free capacity that is
//! still in use); the work itself is cancelled. When the service stops
//! ([`Speaker::shut_down`]), no new work starts and the work in progress is
//! stopped at once.
//!
//! The pipeline ([`Local`]) bounds what one request may cost before the
//! engine starts: the audio a text would make (estimated from its phonemes
//! and the speed) must fit [`MAX_AUDIO_MS`], so every accepted text makes a
//! clip the app keeps; while speaking, the audio made is checked against
//! the same limit. The engine's memory is bounded by the chunk length
//! (`phonemize`), which shrinks for slower speech.
//!
//! Where the work runs is a [`Backend`]: [`Local`] runs the pipeline in this
//! process (the engine process itself, and the tests), `crate::worker` in
//! an engine process that is started on demand and ends when idle.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::engine::{self, Cancel, EngineError, Synthesizer};
use crate::phonemize::{Cut, Phonemes, Phonemizer, Tally};
use crate::postprocess::{self, Clip, PostError, Seam};
use crate::speech::{SpeakText, Speed, Voice};

/// The queue in front of a [`Backend`], shared by all requests.
pub struct Speaker {
    backend: Arc<dyn Backend>,
    slots: Arc<Semaphore>,
    stopping: AtomicBool,
}

impl std::fmt::Debug for Speaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Speaker")
            .field("backend", &self.backend)
            .field("free_slots", &self.slots.available_permits())
            .finish()
    }
}

/// The work of one request, running until it ends.
pub type Work = Pin<Box<dyn Future<Output = Result<Spoken, SpeakError>> + Send>>;

/// Where the pipeline runs. One piece of work at a time is enough (the
/// [`Speaker`] allows only one to run); work must end soon after `cancel`.
pub trait Backend: Send + Sync + std::fmt::Debug {
    fn speak(self: Arc<Self>, text: SpeakText, voice: Voice, speed: Speed, cancel: Cancel) -> Work;

    /// Whether the engine works (as far as it is known): false after the
    /// engine failed to start, until it starts again.
    fn healthy(&self) -> bool {
        true
    }

    /// Whether the engine is running now (in memory, or busy).
    fn running(&self) -> bool {
        true
    }

    /// The service stops: work in progress ends at once with
    /// [`SpeakError::ShuttingDown`], and so does any work after it.
    fn shut_down(&self) {}
}

/// A spoken text with what the log may say about it.
#[derive(Debug, Serialize, Deserialize)]
pub struct Spoken {
    pub clip: Clip,
    /// Where the words' pronunciations came from.
    pub tally: Tally,
    pub phoneme_symbols: usize,
    /// Time spent in the engine and in post-processing.
    pub synthesis_ms: u64,
    pub postprocess_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SpeakError {
    #[error("nothing in the text can be spoken")]
    NothingToSay,
    #[error("the engine is busy")]
    Busy,
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Post(#[from] PostError),
    #[error("the synthesis task failed")]
    Task,
    /// The text would make (estimated before, or made while speaking) more
    /// than [`MAX_AUDIO_MS`] of audio.
    #[error("the text would take too long to speak (~{0} ms of audio, at most {MAX_AUDIO_MS})")]
    TooLong(u64),
    #[error("nobody waits for the audio any more")]
    Cancelled,
    /// The engine process failed while speaking (it answered nonsense, or
    /// gave no sign of life for too long).
    #[error("engine process: {0}")]
    Worker(String),
    /// The engine cannot be started now (the model does not load, or the
    /// engine process dies while loading, e.g. for lack of memory): nothing
    /// to do with the text; worth trying again later.
    #[error("the engine cannot be started: {0}")]
    Unavailable(String),
    /// The service is stopping: the work was stopped (nothing to do with
    /// the text).
    #[error("the service is shutting down")]
    ShuttingDown,
    /// The engine process died while it worked on this text.
    #[error("the engine process died while speaking ({0})")]
    Crashed(String),
}

/// Most audio one request may make, ms: the app keeps clips of up to 60 s;
/// this leaves room for the padding and for the estimate being low.
pub const MAX_AUDIO_MS: u64 = 50_000;
/// The estimate of a text's audio before it is spoken, at normal speed:
/// per symbol (a pause weighted, `Phonemes::weight`) and per chunk (the
/// silence at its start and end). Measured with the int8 model (600
/// characters of words, compounds, numbers, commas, ellipses, dashes,
/// one-word sentences): 26–55 ms per weighted symbol and ~370 ms per chunk;
/// the estimate is above every measured text (by 6–60 %). Whatever the
/// estimate misses is caught while speaking.
const ESTIMATE_MS_PER_SYMBOL: u64 = 58;
const ESTIMATE_MS_PER_CHUNK: u64 = 400;

/// The audio `phonemes` will make at `speed`, estimated (a little high).
fn estimated_audio_ms(phonemes: &Phonemes, speed: Speed) -> u64 {
    let normal = phonemes.weight() as u64 * ESTIMATE_MS_PER_SYMBOL
        + phonemes.chunks().len() as u64 * ESTIMATE_MS_PER_CHUNK;
    (normal as f64 / f64::from(speed.get())).ceil() as u64
}

/// How the engine's audio of two chunks is joined: tightly where the text
/// runs on.
fn seam(cut: Cut) -> Seam {
    match cut {
        Cut::Pause => Seam::Pause,
        Cut::Words => Seam::Words,
    }
}

/// The identity of the audio this service makes for a given phoneme
/// sequence: engine, model and voices, and the post-processing. Part of the
/// app's clip keys (`X-Engine`): a change makes the app generate its clips
/// again.
pub fn engine_version() -> String {
    format!("{}+{}", engine::ENGINE_VERSION, postprocess::VERSION)
}

/// Cancels the work when the request that wants it is gone (dropped on
/// timeout or disconnect; harmless after the work finished).
struct CancelOnDrop(Cancel);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl Speaker {
    /// `waiting`: requests that may wait while one is being synthesised.
    pub fn new(backend: Arc<dyn Backend>, waiting: usize) -> Arc<Self> {
        Arc::new(Self {
            backend,
            slots: Arc::new(Semaphore::new(waiting + 1)),
            stopping: AtomicBool::new(false),
        })
    }

    /// Speak `text`; `Busy` at once when all slots (one running, `waiting`
    /// waiting) are taken. If this future is dropped (request timeout,
    /// client gone), the work is cancelled: skipped if it has not reached
    /// the engine, stopped as soon as possible if it has.
    pub async fn speak(
        self: &Arc<Self>,
        text: SpeakText,
        voice: Voice,
        speed: Speed,
    ) -> Result<Spoken, SpeakError> {
        if self.stopping.load(Ordering::SeqCst) {
            return Err(SpeakError::ShuttingDown);
        }
        let permit = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| SpeakError::Busy)?;
        let cancel = Cancel::new();
        let _guard = CancelOnDrop(cancel.clone());
        let work = Arc::clone(&self.backend).speak(text, voice, speed, cancel);
        tokio::spawn(async move {
            let result = work.await;
            drop(permit);
            result
        })
        .await
        .map_err(|_| SpeakError::Task)?
    }

    /// See [`Backend::healthy`].
    pub fn healthy(&self) -> bool {
        self.backend.healthy()
    }

    /// See [`Backend::running`].
    pub fn engine_running(&self) -> bool {
        self.backend.running()
    }

    /// The service stops: refuse new work, stop the work in progress
    /// (see [`Backend::shut_down`]).
    pub fn shut_down(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.backend.shut_down();
    }
}

/// The pipeline in this process: the lexicons and the loaded engine.
pub struct Local {
    phonemizer: Phonemizer,
    engine: Mutex<Box<dyn Synthesizer>>,
}

impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Local")
            .field("phonemizer", &self.phonemizer)
            .finish_non_exhaustive()
    }
}

impl Local {
    pub fn new(phonemizer: Phonemizer, engine: Box<dyn Synthesizer>) -> Arc<Self> {
        Arc::new(Self {
            phonemizer,
            engine: Mutex::new(engine),
        })
    }

    /// The pipeline, on the calling thread, never cancelled;
    /// `after_chunk` is called after each chunk the engine spoke (a sign of
    /// life for whoever waits).
    pub fn speak_now(
        &self,
        text: &SpeakText,
        voice: Voice,
        speed: Speed,
        after_chunk: &mut dyn FnMut(),
    ) -> Result<Spoken, SpeakError> {
        self.speak_cancellable(text, voice, speed, &Cancel::new(), after_chunk)
    }

    fn speak_cancellable(
        &self,
        text: &SpeakText,
        voice: Voice,
        speed: Speed,
        cancel: &Cancel,
        after_chunk: &mut dyn FnMut(),
    ) -> Result<Spoken, SpeakError> {
        if cancel.is_cancelled() {
            return Err(SpeakError::Cancelled);
        }
        let phonemes = self.phonemizer.phonemize_at(text, voice.accent(), speed);
        if phonemes.is_empty() {
            return Err(SpeakError::NothingToSay);
        }
        let estimate = estimated_audio_ms(&phonemes, speed);
        if estimate > MAX_AUDIO_MS {
            return Err(SpeakError::TooLong(estimate));
        }
        let started = Instant::now();
        let mut parts = Vec::with_capacity(phonemes.chunks().len());
        let mut audio_ms = 0;
        {
            // A panic inside the engine aborts the process (release profile),
            // so a poisoned lock only happens in tests; the engine has no
            // state a failed call could leave half-changed.
            let mut engine = self.engine.lock().unwrap_or_else(PoisonError::into_inner);
            for chunk in phonemes.chunks() {
                // Between chunks: is anybody still waiting for this?
                if cancel.is_cancelled() {
                    return Err(SpeakError::Cancelled);
                }
                let part = engine.synthesize(chunk, voice, speed)?;
                audio_ms += part.duration_ms();
                if audio_ms > MAX_AUDIO_MS {
                    return Err(SpeakError::TooLong(audio_ms));
                }
                parts.push(part);
                after_chunk();
            }
        }
        if cancel.is_cancelled() {
            return Err(SpeakError::Cancelled);
        }
        let seams: Vec<Seam> = phonemes.cuts().iter().map(|cut| seam(*cut)).collect();
        let pcm = postprocess::join(parts, &seams)?;
        let synthesis_ms = elapsed_ms(started);
        let started = Instant::now();
        let clip = postprocess::to_mp3(pcm)?;
        Ok(Spoken {
            clip,
            tally: phonemes.tally(),
            phoneme_symbols: phonemes.symbol_count(),
            synthesis_ms,
            postprocess_ms: elapsed_ms(started),
        })
    }
}

impl Backend for Local {
    /// On the blocking pool (the engine computes for seconds).
    fn speak(self: Arc<Self>, text: SpeakText, voice: Voice, speed: Speed, cancel: Cancel) -> Work {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                self.speak_cancellable(&text, voice, speed, &cancel, &mut || {})
            })
            .await
            .map_err(|_| SpeakError::Task)?
        })
    }
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
