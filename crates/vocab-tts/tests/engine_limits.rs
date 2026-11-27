//! The real engine with the real model on the worst texts the API accepts
//! (R-84): every token at its limit (64 characters, 15 digits), as many as
//! fit 600 characters, and texts dense with pauses, at normal and at the
//! slowest speed. Each is spoken within the chunk limit and makes a clip of
//! at most 50 s (the app keeps up to 60 s), or is refused as too long to
//! speak — never an engine failure or a crash (Kokoro's context is 510
//! symbols; a longer chunk used to abort the process). The memory of these
//! texts is measured by tools/tts-smoke.py.
//!
//! Needs the model files and an engine build, so it is ignored by default;
//! the image's test stage runs it (docker/Dockerfile.tts):
//!   VOCAB_TTS_MODEL_DIR=<model dir> RUSTFLAGS="--cfg vocab_tts_engine" \
//!     SHERPA_ONNX_LIB_DIR=<libs> cargo test -p vocab-tts --test engine_limits -- --ignored

use std::path::PathBuf;
use std::time::Instant;

use vocab_tts::engine;
use vocab_tts::phonemize::Phonemizer;
use vocab_tts::speaker::{Local, MAX_AUDIO_MS, SpeakError};
use vocab_tts::speech::{SpeakText, Speed, Voice};

/// `token` repeated, separated by spaces, as often as fits 600 characters.
fn up_to_600(token: &str) -> String {
    vec![token; 601 / (token.len() + 1)].join(" ")
}

#[test]
#[ignore = "needs the model files and an engine build (VOCAB_TTS_MODEL_DIR)"]
fn r84_the_worst_accepted_texts_never_break_the_engine() {
    let dir = PathBuf::from(std::env::var("VOCAB_TTS_MODEL_DIR").expect("VOCAB_TTS_MODEL_DIR"));
    let phonemizer = Phonemizer::load(&dir).expect("phonemizer");
    let engine = engine::load(&dir, 1).expect("engine");
    let speaker = Local::new(phonemizer, engine);
    let compound = ["internationalization"; 3].join("-");
    let spelled = "zxqvbnmkjhgfdcwplrtsxqvbnmkjhgfdcwplrtszxqvbnmkjhgfdcwplrtszxqvb";
    let texts = [
        up_to_600(&compound),
        "999999999999999".to_owned(),
        up_to_600("999999999999999"),
        spelled.to_owned(),
        up_to_600(spelled),
        "The quiet river wandered past the old mill, and the miller watched it.".to_owned(),
        up_to_600("a,"),
        up_to_600("Oh."),
    ];
    let mut spoken = 0;
    for text in &texts {
        let text = SpeakText::new(text).expect("accepted by validation");
        for (voice, speed) in [
            (Voice::AfHeart, 1.0),
            (Voice::BfEmma, 1.0),
            (Voice::AfHeart, Speed::MIN),
        ] {
            let speed = Speed::new(speed.into()).expect("valid speed");
            let voice_speed = format!("{voice:?} at {}", speed.get());
            let started = Instant::now();
            match speaker.speak_now(&text, voice, speed, &mut || {}) {
                Ok(clip) => {
                    spoken += 1;
                    // The limit plus the padding.
                    assert!(
                        clip.clip.duration_ms() <= MAX_AUDIO_MS + 250,
                        "{voice_speed}: {} ms",
                        clip.clip.duration_ms()
                    );
                    println!(
                        "{} chars, {voice_speed}: {} ms of audio in {} ms",
                        text.as_str().len(),
                        clip.clip.duration_ms(),
                        started.elapsed().as_millis()
                    );
                }
                Err(SpeakError::TooLong(audio_ms)) => {
                    println!(
                        "{} chars, {voice_speed}: refused, ~{audio_ms} ms of audio",
                        text.as_str().len()
                    );
                }
                Err(other) => panic!("{} chars, {voice_speed}: {other}", text.as_str().len()),
            }
        }
    }
    assert!(spoken >= 6, "most of the worst texts are speakable");
}
