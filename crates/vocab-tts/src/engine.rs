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

#[cfg(vocab_tts_engine)]
mod kokoro;

use crate::phonemize::Phonemes;
use crate::postprocess::Pcm;
use crate::speech::{Speed, Voice};

/// File names in the model directory (the Dockerfile copies them there).
const MODEL: &str = "model.onnx";
const VOICES: &str = "voices.bin";
const SYMBOLS: &str = "tokens.txt";
/// Voices of `kokoro-multi-lang-v1_0` (its `speaker_names` metadata).
const SPEAKERS: usize = 54;
/// Identifies engine, model and voices in clip keys of the app: a change
/// here makes the app generate its clips again.
pub const ENGINE_VERSION: &str = "kokoro-multi-lang-v1_0+sherpa-onnx-2.0.1";

/// Set when nobody waits for the audio any more (the request timed out or
/// the client went away): work that has not started is skipped, and the
/// engine stops after the chunk it is on.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Turns phonemes into samples. Implementations need not be thread-safe:
/// callers serialise them (one synthesis at a time, see `speaker`).
pub trait Synthesizer: Send {
    /// Speak `phonemes` (not empty) with `voice` at `speed`; stop between
    /// chunks once `cancel` is set (then [`EngineError::Cancelled`]).
    fn synthesize(
        &mut self,
        phonemes: &Phonemes,
        voice: Voice,
        speed: Speed,
        cancel: &Cancel,
    ) -> Result<Pcm, EngineError>;
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("model file {} is missing (VOCAB_TTS_MODEL_DIR must hold {MODEL}, {VOICES} and {SYMBOLS})", .0.display())]
    Missing(PathBuf),
    #[error("sherpa-onnx could not load the Kokoro model from {} (see the lines above from sherpa-onnx)", .0.display())]
    Load(PathBuf),
    #[error("the model has {found} voices, expected {SPEAKERS} (kokoro-multi-lang-v1_0)")]
    WrongModel { found: i32 },
    #[error("the engine produced no audio")]
    NoAudio,
    #[error("nobody waits for the audio any more")]
    Cancelled,
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
