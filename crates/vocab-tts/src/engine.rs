//! The TTS engine: Kokoro-82M v1.0 through the sherpa-onnx C API
//! (statically linked; design §10.1/§10.2).
//!
//! [`Synthesizer`] is the seam: the HTTP tests use a fake behind it, so
//! `cargo test` needs no model.
//!
//! The Kokoro engine itself (`kokoro`, which links the sherpa-onnx static
//! libraries) is compiled only with `RUSTFLAGS="--cfg vocab_tts_engine"`
//! (docker/Dockerfile.tts sets it together with `SHERPA_ONNX_LIB_DIR`).
//! Without it the crate has no sherpa-onnx dependency at all, so the
//! workspace builds, lints and tests offline, and the sherpa-onnx-sys build
//! script, which would download the libraries without a checksum, never
//! runs; [`load`] then fails with [`EngineError::NotBuilt`]. The engine gets phonemes, never text
//! ([`crate::phonemize`]), so nothing it logs can contain a word of a book.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[cfg(vocab_tts_engine)]
mod kokoro;

#[cfg(doc)]
use crate::phonemize::Phonemes;
use crate::postprocess::Pcm;
use crate::speech::{Speed, Voice};

/// File names in the model directory (the Dockerfile copies them there).
/// The int8 model: a third of the fp32 model's memory, same voices, about
/// 2.4 times slower (docs/design/tts-spike.md, "Small memory").
const MODEL: &str = "model.int8.onnx";
const VOICES: &str = "voices.bin";
const SYMBOLS: &str = "tokens.txt";
/// Voices of `kokoro-int8-multi-lang-v1_0` (its `speaker_names` metadata,
/// the same as the fp32 model's).
const SPEAKERS: usize = 54;
/// Identifies engine, model (with its variant: int8, not fp32) and voices
/// in clip keys of the app: a change here makes the app generate its clips
/// again.
pub const ENGINE_VERSION: &str = "kokoro-int8-multi-lang-v1_0+sherpa-onnx-2.0.1";

/// Set when nobody waits for the audio any more (the request timed out or
/// the client went away), or when the service stops: work that has not
/// started is skipped, and the pipeline stops after the chunk it is on (an
/// engine process is stopped at once).
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<CancelState>);

#[derive(Debug, Default)]
struct CancelState {
    cancelled: AtomicBool,
    wake: Notify,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        self.0.wake.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves once [`Cancel::cancel`] was called.
    pub async fn cancelled(&self) {
        let woken = self.0.wake.notified();
        // Registered before the check: a cancel in between still wakes it.
        if !self.is_cancelled() {
            woken.await;
        }
    }
}

/// Turns phonemes into samples, one chunk ([`Phonemes::chunks`]) at a
/// time: the pipeline (`speaker`) goes through the chunks, can stop between
/// them, and joins their audio. Implementations need not be thread-safe:
/// callers serialise them (one synthesis at a time).
pub trait Synthesizer: Send {
    /// Speak one chunk of phoneme symbols (not empty) with `voice` at
    /// `speed`, as a whole utterance (with the silence the model puts at its
    /// start and end).
    fn synthesize(&mut self, chunk: &str, voice: Voice, speed: Speed) -> Result<Pcm, EngineError>;
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("model file {} is missing (VOCAB_TTS_MODEL_DIR must hold {MODEL}, {VOICES} and {SYMBOLS})", .0.display())]
    Missing(PathBuf),
    #[error("sherpa-onnx could not load the Kokoro model from {} (see the lines above from sherpa-onnx)", .0.display())]
    Load(PathBuf),
    #[error("the model has {found} voices, expected {SPEAKERS} (kokoro-int8-multi-lang-v1_0)")]
    WrongModel { found: i32 },
    #[error("the engine produced no audio")]
    NoAudio,
    #[error(
        "this build has no TTS engine (build with RUSTFLAGS=\"--cfg vocab_tts_engine\" and SHERPA_ONNX_LIB_DIR, see docker/Dockerfile.tts)"
    )]
    NotBuilt,
}

/// Load the engine from `model_dir` with `threads` inference threads.
#[cfg(vocab_tts_engine)]
pub fn load(model_dir: &Path, threads: u16) -> Result<Box<dyn Synthesizer>, EngineError> {
    Ok(Box::new(kokoro::Kokoro::load(model_dir, threads)?))
}

/// This build has no engine (see the module documentation).
#[cfg(not(vocab_tts_engine))]
pub fn load(model_dir: &Path, threads: u16) -> Result<Box<dyn Synthesizer>, EngineError> {
    let _ = (model_dir, threads);
    Err(EngineError::NotBuilt)
}
