//! A scriptable stand-in for the TTS service (design §10.2): records the
//! texts and voices it is asked for and misbehaves on demand. Shared by the
//! audio contract tests; synthetic content only.

// Each test binary that includes this module uses a different subset.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use vocab_server::audio::TtsEndpoint;

/// How `/v1/speak` answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// A valid MP3-looking clip.
    Ok,
    /// 500.
    Server500,
    /// 400 whose message echoes the text (the app must never log it).
    Refuse,
    /// 200 `text/html`.
    Html,
    /// 200 `audio/mpeg` that is not an MP3 stream.
    NotMp3,
    /// 200 `audio/wav` with MP3 bytes.
    WrongType,
    /// 200 `audio/mpeg`, a valid start, 6 MiB long.
    Oversize,
}

pub struct Fake {
    pub speaks: AtomicUsize,
    pub texts: Mutex<Vec<String>>,
    pub voices: Mutex<Vec<String>>,
    pub mode: Mutex<Mode>,
    /// Every route answers 503.
    pub down: AtomicBool,
}

impl Fake {
    pub fn set_mode(&self, mode: Mode) {
        *self.mode.lock().expect("lock") = mode;
    }

    pub fn texts(&self) -> Vec<String> {
        let mut texts = self.texts.lock().expect("lock").clone();
        texts.sort();
        texts
    }

    pub fn speaks(&self) -> usize {
        self.speaks.load(Ordering::SeqCst)
    }
}

/// A fake MP3: frame headers and filler derived from `text`.
pub fn fake_mp3(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xF3, 0x84, 0xC0];
    bytes.extend(std::iter::repeat_n(0u8, 188));
    bytes.extend([0xFF, 0xF3, 0x84, 0xC0]);
    bytes.extend(text.bytes().map(|b| b ^ 0x5A));
    bytes
}

async fn voices(State(fake): State<Arc<Fake>>) -> Response {
    if fake.down.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(json!({ "voices": [], "frontend": "g2p-test", "engine": "fake-1" })).into_response()
}

async fn speak(State(fake): State<Arc<Fake>>, Json(body): Json<Value>) -> Response {
    if fake.down.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    fake.speaks.fetch_add(1, Ordering::SeqCst);
    let text = body["text"].as_str().unwrap_or_default().to_owned();
    fake.texts.lock().expect("lock").push(text.clone());
    fake.voices
        .lock()
        .expect("lock")
        .push(body["voice"].as_str().unwrap_or_default().to_owned());
    let mode = *fake.mode.lock().expect("lock");
    let mut headers = HeaderMap::new();
    headers.insert("x-duration-ms", HeaderValue::from(900));
    headers.insert("x-approximate", HeaderValue::from(0));
    let (content_type, bytes): (&'static str, Vec<u8>) = match mode {
        Mode::Server500 => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"code": "synthesis_failed"}})),
            )
                .into_response();
        }
        Mode::Refuse => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"code": "invalid_text", "message": format!("bad text: {text}")}})),
            )
                .into_response();
        }
        Mode::Ok => ("audio/mpeg", fake_mp3(&text)),
        Mode::Html => ("text/html", b"<html>not audio</html>".to_vec()),
        Mode::NotMp3 => ("audio/mpeg", b"RIFF0000WAVEfmt this is not mp3".to_vec()),
        Mode::WrongType => ("audio/wav", fake_mp3(&text)),
        Mode::Oversize => {
            let mut bytes = fake_mp3(&text);
            bytes.resize(6 << 20, 0);
            ("audio/mpeg", bytes)
        }
    };
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    (headers, bytes).into_response()
}

/// Start the fake on a free local port.
pub async fn start() -> (Arc<Fake>, TtsEndpoint) {
    let fake = Arc::new(Fake {
        speaks: AtomicUsize::new(0),
        texts: Mutex::new(Vec::new()),
        voices: Mutex::new(Vec::new()),
        mode: Mutex::new(Mode::Ok),
        down: AtomicBool::new(false),
    });
    let app = Router::new()
        .route("/v1/voices", get(voices))
        .route("/v1/speak", post(speak))
        .with_state(Arc::clone(&fake));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let limits = vocab_server::config::Limits::default();
        vocab_server::http::serve(listener, app, &limits, std::future::pending())
            .await
            .expect("serve");
    });
    let endpoint = TtsEndpoint::parse(&format!("http://127.0.0.1:{port}")).expect("endpoint");
    (fake, endpoint)
}
