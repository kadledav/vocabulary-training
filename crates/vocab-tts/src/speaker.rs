//! The speaking pipeline: text → phonemes → engine → clip.
//!
//! One synthesis at a time (the engine uses all its threads for one
//! request); a few more requests may wait, anything beyond is refused at
//! once as busy, so a flood cannot queue unbounded work. The work runs on
//! the blocking pool and keeps its slot until it is finished, also when the
//! client has gone (a timed-out request does not free capacity that is
//! still in use).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use tokio::sync::Semaphore;

use crate::engine::{Cancel, EngineError, Synthesizer};
use crate::phonemize::{Phonemizer, Tally};
use crate::postprocess::{self, Clip, PostError};
use crate::speech::{SpeakText, Speed, Voice};

/// Pipeline state shared by all requests.
pub struct Speaker {
    phonemizer: Phonemizer,
    engine: Mutex<Box<dyn Synthesizer>>,
    slots: Arc<Semaphore>,
}

impl std::fmt::Debug for Speaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Speaker")
            .field("phonemizer", &self.phonemizer)
            .field("free_slots", &self.slots.available_permits())
            .finish_non_exhaustive()
    }
}

/// A spoken text with what the log may say about it.
#[derive(Debug)]
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
    pub fn new(phonemizer: Phonemizer, engine: Box<dyn Synthesizer>, waiting: usize) -> Arc<Self> {
        Arc::new(Self {
            phonemizer,
            engine: Mutex::new(engine),
            slots: Arc::new(Semaphore::new(waiting + 1)),
        })
    }

    /// Speak `text`; `Busy` at once when all slots (one running, `waiting`
    /// waiting) are taken. If this future is dropped (request timeout,
    /// client gone), the work is cancelled: skipped if it has not reached
    /// the engine, stopped after the current chunk if it has.
    pub async fn speak(
        self: &Arc<Self>,
        text: SpeakText,
        voice: Voice,
        speed: Speed,
    ) -> Result<Spoken, SpeakError> {
        let permit = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| SpeakError::Busy)?;
        let this = Arc::clone(self);
        let cancel = Cancel::new();
        let _guard = CancelOnDrop(cancel.clone());
        tokio::task::spawn_blocking(move || {
            let result = this.speak_cancellable(&text, voice, speed, &cancel);
            drop(permit);
            result
        })
        .await
        .map_err(|_| SpeakError::Task)?
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

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
