//! Kokoro-82M v1.0 through the sherpa-onnx C API (only with
//! `--cfg vocab_tts_engine`, see the parent module).

use std::path::Path;

use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};

use super::{EngineError, MODEL, SPEAKERS, SYMBOLS, Synthesizer, VOICES};
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

/// Kokoro, loaded once.
pub struct Kokoro {
    tts: OfflineTts,
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
        Ok(Self { tts })
    }
}

impl Synthesizer for Kokoro {
    fn synthesize(
        &mut self,
        phonemes: &Phonemes,
        voice: Voice,
        speed: Speed,
    ) -> Result<Pcm, EngineError> {
        let codepoints = phonemes
            .chunks()
            .iter()
            .map(|chunk| {
                chunk
                    .chars()
                    .filter_map(|c| i32::try_from(u32::from(c)).ok())
                    .collect()
            })
            .collect();
        let config = GenerationConfig {
            sid: speaker_id(voice),
            speed: speed.get(),
            silence_scale: SILENCE_SCALE,
            phoneme_codepoints: Some(codepoints),
            ..Default::default()
        };
        let audio = self
            .tts
            .generate_with_config::<fn(&[f32], f32) -> bool>("", &config, None)
            .ok_or(EngineError::NoAudio)?;
        let rate = u32::try_from(audio.sample_rate()).map_err(|_| EngineError::NoAudio)?;
        if audio.samples().is_empty() {
            return Err(EngineError::NoAudio);
        }
        Ok(Pcm::new(audio.samples().to_vec(), rate))
    }
}
