//! Shared by the API tests: a fake engine behind the `Synthesizer` trait
//! and a tiny synthetic lexicon, so no model is needed.

// Each test binary uses a different subset of the helpers.
#![allow(dead_code)]

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;
use vocab_tts::engine::{EngineError, Synthesizer};
use vocab_tts::http;
use vocab_tts::phonemize::{Lexicon, Phonemizer, parse_symbols};
use vocab_tts::postprocess::Pcm;
use vocab_tts::speaker::{Local, Speaker};
use vocab_tts::speech::{Speed, Voice};

pub const RATE: u32 = 24_000;
/// Synthetic lexicons (misaki-style symbols), not the real ones.
pub const US: &str =
    "hello h ə l ˈ O\nworld w ˈ ɜ ɹ l d\ntomato t ə m ˈ A t O\nsecret s ˈ i k ɹ ə t\n";
pub const GB: &str = "hello h ə l ˈ Q\nworld w ˈ ɜ ː l d\ntomato t ə m ˈ ɑ ː t Q\n";
pub const SYMBOLS: &str = "; 1\n: 2\n, 3\n. 4\n! 5\n? 6\n  16\nA 24\nO 31\nQ 33\nd 46\nh 50\ni 51\nk 53\nl 54\nm 55\nt 62\nw 65\nɑ 69\nə 83\nɜ 87\nɹ 123\nˈ 156\nː 158\ns 61\n";

/// What the fake engine was asked to say (one chunk per call).
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub voice: Voice,
    pub chunk: String,
    pub speed: f32,
}

/// A fake engine: 80 ms of a 220 Hz tone per phoneme symbol.
pub struct FakeEngine {
    pub calls: Arc<Mutex<Vec<Call>>>,
    /// When set: report the start, then wait for a release (busy tests).
    pub gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
}

impl Synthesizer for FakeEngine {
    fn synthesize(&mut self, chunk: &str, voice: Voice, speed: Speed) -> Result<Pcm, EngineError> {
        if let Some((started, release)) = &self.gate {
            started.send(()).ok();
            release.recv().ok();
        }
        self.calls.lock().expect("test setup").push(Call {
            voice,
            chunk: chunk.to_owned(),
            speed: speed.get(),
        });
        let n = chunk.chars().count() * RATE as usize * 80 / 1000;
        let samples = (0..n)
            .map(|i| {
                (0.3 * (2.0 * std::f64::consts::PI * 220.0 * i as f64 / f64::from(RATE)).sin())
                    as f32
            })
            .collect();
        Ok(Pcm::new(samples, RATE))
    }
}

pub fn phonemizer() -> Phonemizer {
    Phonemizer::new(
        Lexicon::parse(US).expect("test setup"),
        Lexicon::parse(GB).expect("test setup"),
        parse_symbols(SYMBOLS).expect("test setup"),
    )
}

pub fn app_with(engine: FakeEngine, waiting: usize) -> Router {
    http::app(Speaker::new(
        Local::new(phonemizer(), Box::new(engine)),
        waiting,
    ))
}

pub fn app() -> (Router, Arc<Mutex<Vec<Call>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = FakeEngine {
        calls: Arc::clone(&calls),
        gate: None,
    };
    (app_with(engine, 4), calls)
}

pub fn speak_request(body: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/speak")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .expect("test setup")
}

pub async fn send(
    app: &Router,
    request: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("test setup");
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 1 << 22)
        .await
        .expect("test setup")
        .to_vec();
    (status, headers, body)
}

pub fn error_code(body: &[u8]) -> String {
    let value: serde_json::Value = serde_json::from_slice(body).expect("test setup");
    value["error"]["code"]
        .as_str()
        .expect("test setup")
        .to_owned()
}
