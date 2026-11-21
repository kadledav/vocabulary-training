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

use crate::engine::{EngineError, Synthesizer};
use crate::phonemize::Phonemizer;
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
    /// Words not in the lexicon, spoken letter by letter.
    pub spelled_out: usize,
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

    /// Speak `text`; `Busy` at once when all slots are taken.
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
        tokio::task::spawn_blocking(move || {
            let result = this.speak_now(&text, voice, speed);
            drop(permit);
            result
        })
        .await
        .map_err(|_| SpeakError::Task)?
    }

    /// The pipeline, on the calling thread.
    pub fn speak_now(
        &self,
        text: &SpeakText,
        voice: Voice,
        speed: Speed,
    ) -> Result<Spoken, SpeakError> {
        let phonemes = self.phonemizer.phonemize(text, voice.accent());
        if phonemes.is_empty() {
            return Err(SpeakError::NothingToSay);
        }
        let started = Instant::now();
        let pcm = {
            // A panic inside the engine aborts the process (release profile),
            // so a poisoned lock only happens in tests; the engine has no
            // state a failed call could leave half-changed.
            let mut engine = self.engine.lock().unwrap_or_else(PoisonError::into_inner);
            engine.synthesize(&phonemes, voice, speed)?
        };
        let synthesis_ms = elapsed_ms(started);
        let started = Instant::now();
        let clip = postprocess::to_mp3(pcm)?;
        Ok(Spoken {
            clip,
            spelled_out: phonemes.spelled_out(),
            phoneme_symbols: phonemes.symbol_count(),
            synthesis_ms,
            postprocess_ms: elapsed_ms(started),
        })
    }
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
