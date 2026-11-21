//! Contract tests of the TTS service API, written from the requirements
//! (R-40 server-side audio, R-24 no book text in logs, R-83/R-84 hardening)
//! and design §10.2 (`POST /v1/speak`, `GET /v1/voices`, `GET /healthz`),
//! against the fake engine. No model, no real synthesis quality.
//!
//! This binary installs the process-wide log subscriber (one test only).

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use common::{app, error_code, phonemizer, send, speak_request};

fn request(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("test setup")
}

fn json_request(content_type: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder().method(Method::POST).uri("/v1/speak");
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    builder
        .body(Body::from(body.to_owned()))
        .expect("test setup")
}

fn text_body(text: &str) -> String {
    serde_json::json!({ "text": text }).to_string()
}

/// 600 characters of speakable words.
fn text_of(chars: usize) -> String {
    let mut text = "hello ".repeat(chars / 6 + 1);
    text.truncate(chars);
    text.trim_end().to_owned() + &"!".repeat(chars - text.trim_end().chars().count())
}

#[tokio::test]
async fn r84_the_text_limit_is_600_characters_not_bytes() {
    let (app, calls) = app();
    // Exactly 600 is accepted; 601 is not (design §10.2: > 600 chars → 400).
    let at_limit = text_of(600);
    assert_eq!(at_limit.chars().count(), 600);
    let (status, headers, _) = send(&app, speak_request(&text_body(&at_limit))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "audio/mpeg");
    let (status, _, body) = send(&app, speak_request(&text_body(&text_of(601)))).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (StatusCode::BAD_REQUEST, "invalid_text")
    );
    // 600 two-byte characters are 1200 bytes: still within the limit.
    let accented = format!("{}!", "é".repeat(599));
    let (status, _, _) = send(&app, speak_request(&text_body(&accented))).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::UNPROCESSABLE_ENTITY,
        "characters are counted, not bytes: {status}"
    );
    let (status, _, body) = send(&app, speak_request(&text_body(&"é".repeat(601)))).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (StatusCode::BAD_REQUEST, "invalid_text")
    );
    assert!(!calls.lock().expect("test setup").is_empty());
}

#[tokio::test]
async fn r84_control_characters_are_refused_before_the_engine() {
    let (app, calls) = app();
    for text in [
        "hello\u{7f}world",
        "hello\u{1b}[31mworld",
        "hello\u{0}world",
        "hello\u{9b}world",
    ] {
        let (status, _, body) = send(&app, speak_request(&text_body(text))).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::BAD_REQUEST, "invalid_text"),
            "{text:?}"
        );
    }
    assert!(calls.lock().expect("test setup").is_empty());
}

#[tokio::test]
async fn r84_whitespace_is_normalised_and_invisible_characters_removed() {
    let (app, calls) = app();
    for text in [
        "hello\tworld",
        "hello\r\nworld",
        "hello\u{85}world",
        "hello\u{2028}world",
        "hello\u{200e} world",
        "hel\u{00AD}lo wor\u{200B}ld",
    ] {
        let (status, headers, _) = send(&app, speak_request(&text_body(text))).await;
        assert_eq!(status, StatusCode::OK, "{text:?}");
        assert_eq!(headers["x-approximate"], "0", "{text:?}");
    }
    let calls = calls.lock().expect("test setup");
    assert_eq!(calls.len(), 6);
    assert!(calls.iter().all(|c| c.chunks == calls[0].chunks));
}

#[tokio::test]
async fn r84_overlong_words_and_numbers_are_unprocessable() {
    let (app, calls) = app();
    let compound = vec!["internationalization"; 20].join("-");
    let digits = "9".repeat(590);
    let sixteen = format!("call {}", "1".repeat(16));
    for (text, code) in [
        (compound.as_str(), "token_too_long"),
        (digits.as_str(), "token_too_long"),
        (sixteen.as_str(), "number_too_long"),
    ] {
        let (status, _, body) = send(&app, speak_request(&text_body(text))).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, code),
            "{code}"
        );
    }
    assert!(calls.lock().expect("test setup").is_empty());
}

#[tokio::test]
async fn r83_only_post_json_is_accepted_on_speak() {
    let (app, calls) = app();
    for method in [
        Method::GET,
        Method::PUT,
        Method::DELETE,
        Method::PATCH,
        Method::HEAD,
    ] {
        let (status, _, _) = send(&app, request(method.clone(), "/v1/speak")).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
    }
    for (method, uri) in [
        (Method::POST, "/v1/voices"),
        (Method::POST, "/healthz"),
        (Method::DELETE, "/healthz"),
    ] {
        let (status, _, _) = send(&app, request(method.clone(), uri)).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
    }
    let body = text_body("hello");
    for content_type in [
        None,
        Some("text/plain"),
        Some("application/x-www-form-urlencoded"),
        Some("multipart/form-data; boundary=x"),
        Some("application/xml"),
    ] {
        let (status, _, response) = send(&app, json_request(content_type, &body)).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type"),
            "{content_type:?}"
        );
    }
    assert!(calls.lock().expect("test setup").is_empty());
    // JSON with a charset parameter is still JSON.
    let (status, _, _) = send(
        &app,
        json_request(Some("application/json; charset=utf-8"), &body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn r40_a_valid_request_returns_audio_with_the_documented_headers() {
    let (app, calls) = app();
    let body = r#"{"text": "Hello world", "voice": "bf_emma", "speed": 1.25}"#;
    let (status, headers, clip) = send(&app, speak_request(body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "audio/mpeg");
    // The documented headers the app's clip identity and Settings rely on.
    for name in [
        "x-duration-ms",
        "x-approximate",
        "x-voice",
        "x-frontend",
        "x-engine",
    ] {
        assert!(headers.contains_key(name), "missing {name}");
    }
    assert_eq!(headers["x-voice"], "bf_emma");
    assert!(
        headers["x-duration-ms"]
            .to_str()
            .expect("test setup")
            .parse::<u64>()
            .is_ok_and(|ms| ms > 0)
    );
    // Hardening headers on the success path (R-83): never cached, no sniffing.
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    // Content-Length matches the MP3 bytes.
    if let Some(length) = headers.get(header::CONTENT_LENGTH) {
        assert_eq!(length.to_str().expect("test setup"), clip.len().to_string());
    }
    // An MP3 frame sync, mono at 24 kHz (MPEG-2 layer III): 0xFFF? with the
    // layer III bits and a 24 kHz sampling-rate index.
    assert_eq!(clip[0], 0xFF);
    assert_eq!(clip[1] & 0xFE, 0xF2, "MPEG-2 layer III");
    assert_eq!((clip[2] >> 2) & 0b11, 0b01, "24 kHz sampling rate");
    // Mono: channel-mode bits 11.
    assert_eq!(clip[3] >> 6, 0b11, "mono");
    let calls = calls.lock().expect("test setup");
    assert_eq!(calls.len(), 1);
    assert!((calls[0].speed - 1.25).abs() < 1e-6);
}

#[tokio::test]
async fn r83_errors_are_json_with_the_hardening_headers() {
    let (app, _) = app();
    let (status, headers, body) = send(&app, speak_request(r#"{"text": ""}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .expect("test setup")
            .starts_with("application/json")
    );
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("test setup");
    assert!(value["error"]["code"].is_string() && value["error"]["message"].is_string());
    // An unknown route answers the same shape.
    let (status, headers, body) = send(&app, request(Method::GET, "/v1/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(error_code(&body), "not_found");
}

#[tokio::test]
async fn r84_speed_bounds_are_inclusive_0_5_to_1_5() {
    let (app, calls) = app();
    for speed in ["0.5", "1.0", "1.5"] {
        let body = format!(r#"{{"text": "hello", "speed": {speed}}}"#);
        let (status, _, _) = send(&app, speak_request(&body)).await;
        assert_eq!(status, StatusCode::OK, "{speed}");
    }
    for speed in ["0.49", "1.51", "0", "-1", "1e9", "\"fast\"", "null"] {
        let body = format!(r#"{{"text": "hello", "speed": {speed}}}"#);
        let (status, _, _) = send(&app, speak_request(&body)).await;
        // `null` may be read as "absent"; everything else is refused.
        let allowed = if speed == "null" {
            [StatusCode::OK, StatusCode::BAD_REQUEST].as_slice()
        } else {
            [StatusCode::BAD_REQUEST].as_slice()
        };
        assert!(allowed.contains(&status), "{speed}: {status}");
    }
    assert!(calls.lock().expect("test setup").len() >= 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_healthcheck_succeeds_on_a_live_listener_and_fails_on_a_dead_one() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test setup");
    let address = listener.local_addr().expect("test setup");
    let (app, _) = app();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        vocab_tts::http::serve(listener, app, async {
            stopped.await.ok();
        })
        .await
    });
    let live = tokio::task::spawn_blocking(move || {
        vocab_tts::http::healthcheck(address, Duration::from_secs(3))
    })
    .await
    .expect("test setup");
    assert_eq!(live, Ok(()));
    stop.send(()).ok();
    server.await.expect("test setup").expect("test setup");
    let dead = tokio::task::spawn_blocking(move || {
        vocab_tts::http::healthcheck(address, Duration::from_secs(1))
    })
    .await
    .expect("test setup");
    assert!(dead.is_err(), "nothing listens any more");
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("test setup").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Success, every refusal, a busy answer, an engine failure: none of them
/// logs the text (R-24), at the most verbose log level.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r24_no_text_in_the_logs_on_any_path() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("test setup");

    let (app, _) = app();
    let secret = "zorblax quuxington";
    let bodies = [
        text_body(&format!("Hello {secret}.")),
        text_body(&format!("{secret}\n")),
        text_body(&format!("<speak>{secret}</speak>")),
        format!(r#"{{"text": "{secret}", "voice": "{secret}"}}"#),
        format!(r#"{{"text": "{secret}", "speed": "{secret}"}}"#),
        format!(r#"{{"text": "{secret}", "{secret}": 1}}"#),
        format!(r#"{{"text": ["{secret}"]"#),
        text_body(&"zorblax ".repeat(100)),
        text_body("!!! ..."),
    ];
    for body in &bodies {
        send(&app, speak_request(body)).await;
    }
    let wrong_type = json_request(Some("text/plain"), &text_body(secret));
    send(&app, wrong_type).await;
    send(
        &app,
        request(Method::GET, &format!("/v1/{}", secret.replace(' ', "-"))),
    )
    .await;

    // An engine failure.
    struct Failing;
    impl vocab_tts::engine::Synthesizer for Failing {
        fn synthesize(
            &mut self,
            _: &vocab_tts::phonemize::Phonemes,
            _: vocab_tts::speech::Voice,
            _: vocab_tts::speech::Speed,
            _: &vocab_tts::engine::Cancel,
        ) -> Result<vocab_tts::postprocess::Pcm, vocab_tts::engine::EngineError> {
            Err(vocab_tts::engine::EngineError::NoAudio)
        }
    }
    let failing = vocab_tts::http::app(vocab_tts::speaker::Speaker::new(
        phonemizer(),
        Box::new(Failing),
        1,
    ));
    let (status, _, _) = send(
        &failing,
        speak_request(&text_body(&format!("Hello {secret}"))),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let logs = String::from_utf8(captured.0.lock().expect("test setup").clone()).expect("utf-8");
    assert!(
        logs.contains("route=/v1/speak"),
        "requests are logged: {logs}"
    );
    let lower = logs.to_lowercase();
    for fragment in ["zorblax", "quuxington", "hello"] {
        assert!(!lower.contains(fragment), "{fragment} in the logs:\n{logs}");
    }
}
