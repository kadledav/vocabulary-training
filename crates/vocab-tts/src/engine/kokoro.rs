//! Kokoro-82M v1.0 through the sherpa-onnx C API (only with
//! `--cfg vocab_tts_engine`, see the parent module).

use std::path::Path;

use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};

use super::{Cancel, EngineError, MODEL, SPEAKERS, SYMBOLS, Synthesizer, VOICES};
use crate::phonemize::Phonemes;
use crate::postprocess::Pcm;
use crate::speech::{Speed, Voice};

/// Pause between chunks, relative to the model's own sentence pause.
const SILENCE_SCALE: f32 = 0.2;

/// Speaker index in `voices.bin` (the model's `speaker_names`, in order).
fn speaker_id(voice: Voice) -> i32 {
    match voice {
        Voice::AfHeart => 3,
        Voice::BfEmma => 21,
    }
}

/// Kokoro: the model's configuration, and an ONNX Runtime session for
/// one chunk at a time.
///
/// A session keeps the working memory of the chunks it has run in an arena
/// that only grows (by doubling regions, and fragmenting with every new
/// shape; sherpa-onnx offers no session options against that). Measured
/// with the int8 model: a 600-character text of sentence-long chunks peaked
/// 130 MiB above one such sentence, and creating the session anew for a
/// chunk only when it was longer than any before did not help. So every
/// chunk gets a fresh session, dropped right after it: the memory is that
/// of the model and one chunk, also between clips. Creating a session (the
/// model load) takes about 0.35 s on a fast core: a word takes ~50 % longer,
/// a sentence ~15 %.
pub struct Kokoro {
    config: OfflineTtsConfig,
    /// The session of the load check, used for the first chunk.
    first: Option<OfflineTts>,
}

impl std::fmt::Debug for Kokoro {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Kokoro")
    }
}

impl Kokoro {
    /// Load the model from `model_dir` with `threads` inference threads.
    pub fn load(model_dir: &Path, threads: u16) -> Result<Self, EngineError> {
        let file = |name: &str| -> Result<String, EngineError> {
            let path = model_dir.join(name);
            if !path.is_file() {
                return Err(EngineError::Missing(path));
            }
            Ok(path.to_string_lossy().into_owned())
        };
        let config = OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                kokoro: OfflineTtsKokoroModelConfig {
                    model: Some(file(MODEL)?),
                    voices: Some(file(VOICES)?),
                    tokens: Some(file(SYMBOLS)?),
                    // No lexicon: phonemes come from `phonemize`.
                    ..Default::default()
                },
                num_threads: i32::from(threads),
                provider: Some("cpu".to_owned()),
                ..Default::default()
            },
            max_num_sentences: 1,
            silence_scale: SILENCE_SCALE,
            ..Default::default()
        };
        let tts =
            OfflineTts::create(&config).ok_or_else(|| EngineError::Load(model_dir.to_owned()))?;
        let found = tts.num_speakers();
        if usize::try_from(found).ok() != Some(SPEAKERS) {
            return Err(EngineError::WrongModel { found });
        }
        Ok(Self {
            config,
            first: Some(tts),
        })
    }

    /// A session that has run nothing yet.
    fn fresh_session(&mut self) -> Result<OfflineTts, EngineError> {
        if let Some(tts) = self.first.take() {
            return Ok(tts);
        }
        OfflineTts::create(&self.config).ok_or_else(|| {
            let model = self.config.model.kokoro.model.clone().unwrap_or_default();
            EngineError::Load(std::path::PathBuf::from(model))
        })
    }
}

impl Synthesizer for Kokoro {
    fn synthesize(
        &mut self,
        phonemes: &Phonemes,
        voice: Voice,
        speed: Speed,
        cancel: &Cancel,
    ) -> Result<Pcm, EngineError> {
        let mut samples = Vec::new();
        let mut rate = None;
        // One chunk per call (the same audio as one call with all chunks:
        // each ends with the same pause), each with a fresh session that is
        // dropped before the next one is made (never two models in memory);
        // work stops between chunks once cancelled.
        for chunk in phonemes.chunks() {
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let codepoints: Vec<i32> = chunk
                .chars()
                .filter_map(|c| i32::try_from(u32::from(c)).ok())
                .collect();
            let tts = self.fresh_session()?;
            let config = GenerationConfig {
                sid: speaker_id(voice),
                speed: speed.get(),
                silence_scale: SILENCE_SCALE,
                phoneme_codepoints: Some(vec![codepoints]),
                ..Default::default()
            };
            let audio = tts
                .generate_with_config("", &config, None::<fn(&[f32], f32) -> bool>)
                .ok_or(EngineError::NoAudio)?;
            rate = Some(u32::try_from(audio.sample_rate()).map_err(|_| EngineError::NoAudio)?);
            samples.extend_from_slice(audio.samples());
        }
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        match rate {
            Some(rate) if !samples.is_empty() => Ok(Pcm::new(samples, rate)),
            _ => Err(EngineError::NoAudio),
        }
    }
}
