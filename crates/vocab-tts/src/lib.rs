//! `vocab-tts`: the neural text-to-speech service of the vocabulary app
//! (design §3.3, §10; R-40, R-42, R-83).
//!
//! A small HTTP service in its own hardened container: it turns a headword
//! or a sentence into a loudness-normalised MP3 clip with Kokoro-82M v1.0
//! (sherpa-onnx). The app calls it on the internal network and stores the
//! clips; this service keeps nothing.
//!
//! Modules: `config` (environment), `speech` (validated request values),
//! `phonemize` (text to phonemes, US/UK lexicons), `engine` (Kokoro behind
//! the `Synthesizer` trait), `postprocess` (trim, loudness, MP3), `speaker`
//! (the pipeline, one synthesis at a time), `worker` (the engine process:
//! the model is in memory only while there is work), `http` (API, limits,
//! logs).

pub mod config;
pub mod engine;
pub mod http;
pub mod phonemize;
pub mod postprocess;
pub mod speaker;
pub mod speech;
pub mod worker;
