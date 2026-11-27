//! API tests of the TTS service (fake engine, synthetic lexicon).

mod common;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{Call, FakeEngine, SYMBOLS, app, app_with, error_code, send, speak_request};
use vocab_tts::phonemize::{Lexicon, Phonemizer, parse_symbols};
use vocab_tts::speaker::{Local, Speaker};
use vocab_tts::speech::Voice;

#[tokio::test]
async fn r40_speak_returns_an_mp3_clip() {
    let (app, calls) = app();
    let (status, headers, body) = send(&app, speak_request(r#"{"text": "Hello, world."}"#)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "audio/mpeg");
    assert_eq!(headers["x-voice"], "af_heart");
    assert_eq!(headers["x-approximate"], "0");
    assert_eq!(headers["x-words-spelled"], "0");
    assert_eq!(
        headers["x-frontend"],
        vocab_tts::phonemize::FRONTEND_VERSION
    );
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    // MPEG-2 layer III frame sync at the start.
    assert!(
        body.len() > 1000 && body[0] == 0xFF && body[1] & 0xE0 == 0xE0,
        "{} bytes",
        body.len()
    );
    // 14 symbols × 80 ms + 210 ms padding.
    let duration: u64 = headers["x-duration-ms"]
        .to_str()
        .expect("test setup")
        .parse()
        .expect("test setup");
    assert!(duration.abs_diff(14 * 80 + 210) <= 5, "{duration} ms");
    let lufs: f64 = headers["x-loudness-lufs"]
        .to_str()
        .expect("test setup")
        .parse()
        .expect("test setup");
    assert!((lufs + 16.0).abs() < 0.5, "{lufs} LUFS");
    let calls = calls.lock().expect("test setup");
    assert_eq!(
        *calls,
        [Call {
            voice: Voice::AfHeart,
            chunk: "həlˈO, wˈɜɹld.".to_owned(),
            speed: 1.0
        }]
    );
}

#[tokio::test]
async fn r42_the_uk_voice_speaks_with_the_uk_lexicon() {
    let (app, calls) = app();
    let (status, headers, _) = send(
        &app,
        speak_request(r#"{"text": "tomato", "voice": "bf_emma", "speed": 0.8}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-voice"], "bf_emma");
    let (status, _, _) = send(
        &app,
        speak_request(r#"{"text": "tomato", "voice": "af_heart"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let calls = calls.lock().expect("test setup");
    assert_eq!(calls[0].voice, Voice::BfEmma);
    assert_eq!(calls[0].chunk, "təmˈɑːtQ");
    assert!((calls[0].speed - 0.8).abs() < 1e-6);
    assert_eq!(calls[1].voice, Voice::AfHeart);
    assert_eq!(calls[1].chunk, "təmˈAtO");
}

#[tokio::test]
async fn r42_voices_are_listed() {
    let (app, _) = app();
    let request = Request::get("/v1/voices")
        .body(Body::empty())
        .expect("test setup");
    let (status, _, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("test setup");
    assert_eq!(
        value,
        serde_json::json!({ "voices": [
            { "id": "af_heart", "accent": "en-US", "default": true },
            { "id": "bf_emma", "accent": "en-GB", "default": false },
        ], "frontend": vocab_tts::phonemize::FRONTEND_VERSION, "engine": "kokoro-int8-multi-lang-v1_0+sherpa-onnx-2.0.1+audio-2"})
    );
}

#[tokio::test]
async fn r83_healthz_and_unknown_routes() {
    let (app, _) = app();
    let (status, _, body) = send(
        &app,
        Request::get("/healthz")
            .body(Body::empty())
            .expect("test setup"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The in-process pipeline runs all the time.
    assert_eq!(body, br#"{"engine":"running","status":"ok"}"#);
    let (status, _, body) = send(
        &app,
        Request::get("/v1/other")
            .body(Body::empty())
            .expect("test setup"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");
    let (status, _, _) = send(
        &app,
        Request::get("/v1/speak")
            .body(Body::empty())
            .expect("test setup"),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn r84_invalid_requests_are_rejected_before_the_engine() {
    let (app, calls) = app();
    let long = format!(r#"{{"text": "{}"}}"#, "a".repeat(601));
    let cases: Vec<(String, StatusCode, &str)> = vec![
        (
            r#"{"text": ""}"#.into(),
            StatusCode::BAD_REQUEST,
            "invalid_text",
        ),
        (
            r#"{"text": "   "}"#.into(),
            StatusCode::BAD_REQUEST,
            "invalid_text",
        ),
        (long, StatusCode::BAD_REQUEST, "invalid_text"),
        (
            r#"{"text": "hello\u001b[31mworld"}"#.into(),
            StatusCode::BAD_REQUEST,
            "invalid_text",
        ),
        (
            r#"{"text": "hello\u0000"}"#.into(),
            StatusCode::BAD_REQUEST,
            "invalid_text",
        ),
        (
            "{\"text\": \"\\u202e\\u200b \\u00ad\"}".into(),
            StatusCode::BAD_REQUEST,
            "invalid_text",
        ),
        (
            r#"{"text": "<speak>hello</speak>"}"#.into(),
            StatusCode::BAD_REQUEST,
            "invalid_text",
        ),
        (
            r#"{"text": "hello", "voice": "am_adam"}"#.into(),
            StatusCode::BAD_REQUEST,
            "unknown_voice",
        ),
        (
            r#"{"text": "hello", "speed": 2.0}"#.into(),
            StatusCode::BAD_REQUEST,
            "invalid_speed",
        ),
        (
            r#"{"text": "hello", "speed": 0.1}"#.into(),
            StatusCode::BAD_REQUEST,
            "invalid_speed",
        ),
        (
            r#"{"text": "hello", "ssml": true}"#.into(),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            r#"{"text": 5}"#.into(),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            r#"{"voice": "af_heart"}"#.into(),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            r#"["hello"]"#.into(),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            "{not json".into(),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
    ];
    for (body, status, code) in cases {
        let (got, _, response) = send(&app, speak_request(&body)).await;
        assert_eq!(
            (got, error_code(&response).as_str()),
            (status, code),
            "{body}"
        );
    }
    assert!(calls.lock().expect("test setup").is_empty());
}

#[tokio::test]
async fn r84_body_limit_and_content_type() {
    let (app, calls) = app();
    let huge = format!(r#"{{"text": "{}"}}"#, "a".repeat(5000));
    let (status, _, body) = send(&app, speak_request(&huge)).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "too_large")
    );
    let request = Request::post("/v1/speak")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(r#"{"text": "hello"}"#))
        .expect("test setup");
    let (status, _, body) = send(&app, request).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type")
    );
    assert!(calls.lock().expect("test setup").is_empty());
}

#[tokio::test]
async fn r24_error_answers_never_echo_the_text() {
    let (app, _) = app();
    for body in [
        r#"{"text": "secret <b>"}"#,
        r#"{"text": ["secret"]}"#,
        r#"{"text": "secret", "extra": "secret"}"#,
        r#"{"text": "secret", "voice": "secret"}"#,
    ] {
        let (status, _, response) = send(&app, speak_request(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            !String::from_utf8_lossy(&response).contains("secret"),
            "{body}"
        );
    }
}

#[tokio::test]
async fn r40_text_without_speakable_words_is_unprocessable() {
    let (app, calls) = app();
    let (status, _, body) = send(&app, speak_request(r#"{"text": "!!! ... ¶"}"#)).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "nothing_to_say")
    );
    assert!(calls.lock().expect("test setup").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_one_synthesis_at_a_time_and_busy_beyond_the_queue() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let engine = FakeEngine {
        calls: Arc::new(Mutex::new(Vec::new())),
        gate: Some((started_tx, release_rx)),
    };
    // No waiting room: one request in the engine, the next is busy.
    let app = app_with(engine, 0);
    let first = tokio::spawn({
        let app = app.clone();
        async move { send(&app, speak_request(r#"{"text": "hello"}"#)).await.0 }
    });
    tokio::task::spawn_blocking(move || started_rx.recv().expect("test setup"))
        .await
        .expect("test setup");
    let (status, headers, body) = send(&app, speak_request(r#"{"text": "world"}"#)).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "busy")
    );
    assert_eq!(headers[header::RETRY_AFTER], "5");
    release_tx.send(()).expect("test setup");
    assert_eq!(first.await.expect("test setup"), StatusCode::OK);
    // The slot is free again.
    release_tx.send(()).expect("test setup");
    let (status, _, _) = send(&app, speak_request(r#"{"text": "world"}"#)).await;
    assert_eq!(status, StatusCode::OK);
}

/// Starts a gated request and waits until it is inside the engine.
async fn hold_engine(
    app: &axum::Router,
    started_rx: mpsc::Receiver<()>,
    text: &str,
) -> tokio::task::JoinHandle<StatusCode> {
    let body = format!(r#"{{"text": "{text}"}}"#);
    let first = tokio::spawn({
        let app = app.clone();
        async move { send(&app, speak_request(&body)).await.0 }
    });
    tokio::task::spawn_blocking(move || started_rx.recv().expect("test setup"))
        .await
        .expect("test setup");
    first
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_abandoned_requests_never_reach_the_engine() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = FakeEngine {
        calls: Arc::clone(&calls),
        gate: Some((started_tx, release_rx)),
    };
    let app = app_with(engine, 1);
    let first = hold_engine(&app, started_rx, "hello").await;
    // The second waits for the engine; its client gives up (timeout).
    let second = tokio::spawn({
        let app = app.clone();
        async move { send(&app, speak_request(r#"{"text": "world"}"#)).await.0 }
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    second.abort();
    assert!(second.await.is_err());
    release_tx.send(()).expect("test setup");
    assert_eq!(first.await.expect("test setup"), StatusCode::OK);
    // Give the abandoned work its chance to (wrongly) run.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let said: Vec<_> = calls
        .lock()
        .expect("test setup")
        .iter()
        .map(|c| c.chunk.clone())
        .collect();
    assert_eq!(said, ["həlˈO"]);
    // Nothing is stuck: the next request is served.
    release_tx.send(()).expect("test setup");
    let (status, _, _) = send(&app, speak_request(r#"{"text": "world"}"#)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_work_in_the_engine_stops_when_the_request_is_gone() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = FakeEngine {
        calls: Arc::clone(&calls),
        gate: Some((started_tx, release_rx)),
    };
    let app = app_with(engine, 0);
    let first = hold_engine(&app, started_rx, "hello. hello.").await;
    first.abort();
    assert!(first.await.is_err());
    release_tx.send(()).expect("test setup");
    // The engine saw the cancel flag and produced nothing.
    let mut free = false;
    for _ in 0..50 {
        release_tx.send(()).ok();
        let (status, _, _) = send(&app, speak_request(r#"{"text": "world"}"#)).await;
        if status == StatusCode::OK {
            free = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(free);
    let said: Vec<_> = calls
        .lock()
        .expect("test setup")
        .iter()
        .map(|c| c.chunk.clone())
        .collect();
    // The first chunk of the abandoned request was under way; the second
    // never started.
    assert_eq!(said, ["həlˈO.", "wˈɜɹld"]);
}

#[tokio::test]
async fn r84_text_that_would_take_too_long_to_speak_is_unprocessable() {
    // A synthetic word worth 100 symbols: 20 of them are 2000 symbols.
    let long = format!("zz {}\n", vec!["s"; 100].join(" "));
    let phonemizer = Phonemizer::new(
        Lexicon::parse(&long).expect("test setup"),
        Lexicon::parse(&long).expect("test setup"),
        parse_symbols(SYMBOLS).expect("test setup"),
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = FakeEngine {
        calls: Arc::clone(&calls),
        gate: None,
    };
    let app = vocab_tts::http::app(Speaker::new(Local::new(phonemizer, Box::new(engine)), 1));
    let text = vec!["zz"; 20].join(" ");
    let (status, _, body) = send(&app, speak_request(&format!(r#"{{"text": "{text}"}}"#))).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "too_long_to_speak")
    );
    assert!(calls.lock().expect("test setup").is_empty());
    let (status, _, _) = send(&app, speak_request(r#"{"text": "zz zz"}"#)).await;
    assert_eq!(status, StatusCode::OK);
}

/// An app whose only word "zz" is 100 symbols (one chunk at normal speed).
fn app_with_long_word() -> (axum::Router, Arc<Mutex<Vec<Call>>>) {
    let long = format!("zz {}\n", vec!["s"; 100].join(" "));
    let phonemizer = Phonemizer::new(
        Lexicon::parse(&long).expect("test setup"),
        Lexicon::parse(&long).expect("test setup"),
        parse_symbols(SYMBOLS).expect("test setup"),
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = FakeEngine {
        calls: Arc::clone(&calls),
        gate: None,
    };
    let app = vocab_tts::http::app(Speaker::new(Local::new(phonemizer, Box::new(engine)), 1));
    (app, calls)
}

#[tokio::test]
async fn r84_slower_speech_gets_shorter_chunks_and_a_smaller_budget() {
    let (app, calls) = app_with_long_word();
    // Five words: ~31 s at normal speed; twice that at half speed is more
    // than the 50 s a clip may have, so it is refused before the engine.
    let text = ["zz"; 5].join(" ");
    let (status, _, _) = send(&app, speak_request(&format!(r#"{{"text": "{text}"}}"#))).await;
    assert_eq!(status, StatusCode::OK);
    calls.lock().expect("test setup").clear();
    let body = format!(r#"{{"text": "{text}", "speed": 0.5}}"#);
    let (status, _, body) = send(&app, speak_request(&body)).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "too_long_to_speak")
    );
    assert!(calls.lock().expect("test setup").is_empty());
    // Accepted at half speed, in chunks of at most half the length.
    let (status, _, _) = send(&app, speak_request(r#"{"text": "zz zz", "speed": 0.5}"#)).await;
    assert_eq!(status, StatusCode::OK);
    let calls = calls.lock().expect("test setup");
    assert_eq!(calls.len(), 4);
    assert!(calls.iter().all(|c| c.chunk.chars().count() <= 50));
}

#[tokio::test]
async fn r84_audio_beyond_the_limit_stops_the_engine_and_is_unprocessable() {
    let (app, calls) = app_with_long_word();
    // Eight words are estimated just under 50 s; the fake engine makes 8 s
    // per word, so the limit is passed during the seventh: no eighth.
    let text = ["zz"; 8].join(" ");
    let (status, _, body) = send(&app, speak_request(&format!(r#"{{"text": "{text}"}}"#))).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "too_long_to_speak")
    );
    assert_eq!(calls.lock().expect("test setup").len(), 7);
}
