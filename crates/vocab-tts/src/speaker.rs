//! The speaking pipeline: text → phonemes → engine → clip, and the queue
//! in front of it.
//!
//! [`Speaker`] is the queue: one synthesis at a time (the engine uses all
//! its threads for one request); a few more requests may wait, anything
//! beyond is refused at once as busy, so a flood cannot queue unbounded
//! work. A request keeps its slot until its work is finished, also when the
//! client has gone (a timed-out request does not free capacity that is
//! still in use); the work itself is cancelled.
//!
//! Where the work runs is a [`Backend`]: [`Local`] runs the pipeline in this
//! process (the engine process itself, and the tests), `crate::worker` in
//! an engine process that is started on demand and ends when idle.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::engine::{Cancel, EngineError, Synthesizer};
use crate::phonemize::{Phonemizer, Tally};
use crate::postprocess::{self, Clip, PostError};
use crate::speech::{SpeakText, Speed, Voice};

/// The queue in front of a [`Backend`], shared by all requests.
pub struct Speaker {
    backend: Arc<dyn Backend>,
    slots: Arc<Semaphore>,
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

    /// Whether the model is in memory now (or being loaded or used).
    fn loaded(&self) -> bool {
        true
    }
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
    #[error("the text would take too long to speak ({0} phoneme symbols, at most {MAX_SYMBOLS})")]
    TooLong(usize),
    #[error("nobody waits for the audio any more")]
    Cancelled,
    /// The engine process failed (it did not start, or it answered nonsense).
    #[error("engine process: {0}")]
    Worker(String),
    /// The engine process died while it worked on this text.
    #[error("the engine process died while speaking ({0})")]
    Crashed(String),
}

/// Most phoneme symbols one request may make (≈ 60 s of speech; a normal
/// 600-character sentence has about 600). Spelled-out numbers or letters
/// can multiply the symbols of a text: such a text is refused instead of
/// keeping the engine busy for minutes.
pub const MAX_SYMBOLS: usize = 900;

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

    /// See [`Backend::loaded`].
    pub fn loaded(&self) -> bool {
        self.backend.loaded()
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

    /// The pipeline, on the calling thread, never cancelled.
    pub fn speak_now(
        &self,
        text: &SpeakText,
        voice: Voice,
        speed: Speed,
    ) -> Result<Spoken, SpeakError> {
        self.speak_cancellable(text, voice, speed, &Cancel::new())
    }

    fn speak_cancellable(
        &self,
        text: &SpeakText,
        voice: Voice,
        speed: Speed,
        cancel: &Cancel,
    ) -> Result<Spoken, SpeakError> {
        if cancel.is_cancelled() {
            return Err(SpeakError::Cancelled);
        }
        let phonemes = self.phonemizer.phonemize(text, voice.accent());
        if phonemes.is_empty() {
            return Err(SpeakError::NothingToSay);
        }
        if phonemes.symbol_count() > MAX_SYMBOLS {
            return Err(SpeakError::TooLong(phonemes.symbol_count()));
        }
        let started = Instant::now();
        let pcm = {
            // A panic inside the engine aborts the process (release profile),
            // so a poisoned lock only happens in tests; the engine has no
            // state a failed call could leave half-changed.
            let mut engine = self.engine.lock().unwrap_or_else(PoisonError::into_inner);
            // Waited for the engine: is anybody still waiting for this?
            if cancel.is_cancelled() {
                return Err(SpeakError::Cancelled);
            }
            engine
                .synthesize(&phonemes, voice, speed, cancel)
                .map_err(|err| match err {
                    EngineError::Cancelled => SpeakError::Cancelled,
                    other => SpeakError::Engine(other),
                })?
        };
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
                self.speak_cancellable(&text, voice, speed, &cancel)
            })
            .await
            .map_err(|_| SpeakError::Task)?
        })
    }
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
