//! Neural audio end to end against a fake TTS service (design §10.3; R-40,
//! R-41, R-42, R-85): import → queue → clips → served with ranges;
//! retries, refusals, an unreachable service, garbage collection, restart
//! reconciliation, the voice setting, and the app without TTS.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use common::{START, env, env_with_audio, word};
use serde_json::{Value, json};
use tower::ServiceExt;
use vocab_server::audio::{Audio, Step, TtsEndpoint};

/// A fake MP3: two MPEG-2 layer III frame headers and filler.
fn fake_mp3(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xF3, 0x84, 0xC0];
    bytes.extend(std::iter::repeat_n(0u8, 188));
    bytes.extend([0xFF, 0xF3, 0x84, 0xC0]);
    bytes.extend(text.bytes().map(|b| b ^ 0x5A));
    bytes
}

/// Behaviour of the fake service.
#[derive(Default)]
struct Fake {
    /// Requests to `/v1/speak` so far.
    speaks: AtomicUsize,
    /// The first this-many speak requests answer 500.
    fail_first: AtomicUsize,
    /// Voices of the speak requests, in order.
    voices: std::sync::Mutex<Vec<String>>,
}

async fn voices() -> Json<Value> {
    Json(json!({ "voices": [], "frontend": "g2p-test", "engine": "fake-1" }))
}

async fn speak(State(fake): State<Arc<Fake>>, Json(body): Json<Value>) -> Response {
    let n = fake.speaks.fetch_add(1, Ordering::SeqCst);
    let text = body["text"].as_str().unwrap_or_default().to_owned();
    fake.voices
        .lock()
        .expect("lock")
        .push(body["voice"].as_str().unwrap_or_default().to_owned());
    if n < fake.fail_first.load(Ordering::SeqCst) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"code": "synthesis_failed"}})),
        )
            .into_response();
    }
    if text.contains("crashing") {
        // The service's engine process died on this text (e.g. OOM-killed).
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"code": "engine_crashed"}})),
        )
            .into_response();
    }
    if text.contains("refuse") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"code": "invalid_text"}})),
        )
            .into_response();
    }
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/mpeg"));
    headers.insert("x-duration-ms", HeaderValue::from(1234));
    let approximate = u32::from(text.contains("zxq"));
    headers.insert("x-approximate", HeaderValue::from(approximate));
    (headers, fake_mp3(&text)).into_response()
}

/// Start the fake on a free local port.
async fn fake_tts() -> (Arc<Fake>, TtsEndpoint) {
    let fake = Arc::new(Fake::default());
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

/// Sync, then work until nothing is due.
async fn drain(audio: &Audio) -> usize {
    audio.sync().await.expect("sync");
    let mut worked = 0;
    while audio.work_one().await.expect("work") == Step::Worked {
        worked += 1;
        assert!(worked < 1000, "runaway");
    }
    worked
}

async fn get_raw(
    env: &common::Env,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = env
        .router
        .clone()
        .oneshot(builder.body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body")
        .to_vec();
    (status, headers, body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r40_imported_words_and_sentences_get_clips_served_with_ranges() {
    let (fake, endpoint) = fake_tts().await;
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    let mut glimmer = word("glimmerwick", 7, 2);
    glimmer["speak"] = json!("glimmer wick");
    env.import("test-audio-book", vec![glimmer, word("brindlefoot", 6, 1)])
        .await;
    let id = env.word_id("glimmerwick").await;

    // Before the worker ran: nothing ready, the client falls back (R-41).
    let (status, before) = env.get(&format!("/api/audio/words/{}", id.get())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(before["enabled"], true);
    assert_eq!(before["word"]["ready"], false);
    assert!(before["word"]["url"].is_null());

    // 2 words + 3 sentences; the word is spoken from `speak`.
    assert_eq!(drain(&audio).await, 5);
    assert_eq!(fake.speaks.load(Ordering::SeqCst), 5);
    let (_, clips) = env.get(&format!("/api/audio/words/{}", id.get())).await;
    assert_eq!(clips["word"]["ready"], true, "{clips}");
    assert_eq!(clips["sentences"].as_array().expect("sentences").len(), 2);
    assert!(
        clips["sentences"]
            .as_array()
            .expect("s")
            .iter()
            .all(|s| s["ready"] == true)
    );
    let url = clips["word"]["url"].as_str().expect("url").to_owned();
    assert!(
        url.starts_with("/api/audio/clips/") && url.ends_with(".mp3"),
        "{url}"
    );

    let (status, headers, body) = get_raw(&env, &url, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "audio/mpeg");
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
    assert_eq!(body, fake_mp3("glimmer wick"));
    let etag = headers[header::ETAG].to_str().expect("etag").to_owned();

    // Ranges (iOS Safari): a slice, an open end, a suffix, beyond the end.
    let len = body.len();
    let (status, headers, part) = get_raw(&env, &url, &[("range", "bytes=0-9")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(part, body[..10]);
    assert_eq!(
        headers[header::CONTENT_RANGE],
        format!("bytes 0-9/{len}").as_str()
    );
    let (status, _, part) = get_raw(&env, &url, &[("range", "bytes=190-")]).await;
    assert_eq!(
        (status, part.as_slice()),
        (StatusCode::PARTIAL_CONTENT, &body[190..])
    );
    let (status, _, part) = get_raw(&env, &url, &[("range", "bytes=-4")]).await;
    assert_eq!(
        (status, part.as_slice()),
        (StatusCode::PARTIAL_CONTENT, &body[len - 4..])
    );
    let (status, headers, _) = get_raw(&env, &url, &[("range", &format!("bytes={len}-"))]).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        headers[header::CONTENT_RANGE],
        format!("bytes */{len}").as_str()
    );
    let (status, _, _) = get_raw(&env, &url, &[("if-none-match", &etag)]).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);

    // Unknown or malformed clip names are 404, never a file system path.
    for bad in [
        "/api/audio/clips/0123456789abcdef0123456789abcdef.mp3",
        "/api/audio/clips/..%2Fx.mp3",
        "/api/audio/clips/xyz",
    ] {
        assert_eq!(
            get_raw(&env, bad, &[]).await.0,
            StatusCode::NOT_FOUND,
            "{bad}"
        );
    }

    // Files are owner-only (0600) under <cache>/audio.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let hash = url
            .trim_start_matches("/api/audio/clips/")
            .trim_end_matches(".mp3");
        let path = env
            .dir
            .path()
            .join("cache/audio")
            .join(&hash[..2])
            .join(format!("{hash}.mp3"));
        let mode = std::fs::metadata(&path)
            .expect("clip file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    let (_, status) = env.get("/api/audio/status").await;
    assert_eq!(status["ready"], 5, "{status}");
    assert_eq!(
        (status["pending"].clone(), status["failed"].clone()),
        (json!(0), json!(0))
    );
    assert_eq!(status["connected"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_clips_need_a_session() {
    let (_fake, endpoint) = fake_tts().await;
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    env.import("test-audio-book", vec![word("glimmerwick", 7, 0)])
        .await;
    drain(env.audio.as_ref().expect("audio")).await;
    let id = env.word_id("glimmerwick").await;
    let (_, clips) = env.get(&format!("/api/audio/words/{}", id.get())).await;
    let url = clips["word"]["url"].as_str().expect("url").to_owned();
    // Straight to the inner app (no session cookie): refused.
    let db_free = vocab_server::http::app(
        vocab_server::http::AppState::new(env.db.clone(), common::prague(), common::auth().0)
            .with_audio(env.audio.clone().expect("audio")),
        &vocab_server::config::Limits::default(),
    );
    for uri in [
        url.as_str(),
        "/api/audio/status",
        &format!("/api/audio/words/{}", id.get()),
    ] {
        let response = db_free
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r40_failures_are_retried_with_backoff_and_refusals_fail_at_once() {
    let (fake, endpoint) = fake_tts().await;
    fake.fail_first.store(2, Ordering::SeqCst);
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    let mut refused = word("refusenik", 5, 0);
    refused["speak"] = json!("please refuse this");
    env.import("test-audio-book", vec![word("glimmerwick", 7, 0), refused])
        .await;

    audio.sync().await.expect("sync");
    // First attempt fails (500): attempt 1, due again in 30 s.
    assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
    assert_eq!(
        env.int("SELECT attempts FROM audio_clip WHERE status = 'pending' AND attempts = 1"),
        1
    );
    // The other clip is next (the failed one is not due yet); it fails too.
    assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
    assert_eq!(audio.work_one().await.expect("work"), Step::Idle);
    env.set_time("2026-11-20T09:00:31Z");
    // Due again: one succeeds, the refusal (400) fails for good.
    assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
    assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready'"),
        1
    );
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'failed' AND last_error LIKE '400 invalid_text'"),
        1
    );
    let (_, status) = env.get("/api/audio/status").await;
    assert_eq!(
        (status["ready"].clone(), status["failed"].clone()),
        (json!(1), json!(1)),
        "{status}"
    );
    // "Retry failed" in Settings queues it again.
    let (code, retried) = env.post("/api/audio/retry", &json!({})).await;
    assert_eq!(
        (code, retried["retried"].clone()),
        (StatusCode::OK, json!(1))
    );
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'pending' AND attempts = 0"),
        1
    );
    // Book text never reaches the error column: only status and code.
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE last_error LIKE '%refuse%'"),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_a_text_that_crashes_the_engine_process_fails_after_three_crashes() {
    let (_fake, endpoint) = fake_tts().await;
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    let mut crashing = word("crashword", 5, 0);
    crashing["speak"] = json!("a crashing text");
    env.import("test-audio-book", vec![crashing]).await;
    audio.sync().await.expect("sync");
    for (crashes, later) in [(1, "2026-11-20T09:00:31Z"), (2, "2026-11-20T09:02:32Z")] {
        assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
        assert_eq!(
            env.int(&format!(
                "SELECT COUNT(*) FROM audio_clip WHERE status = 'pending' AND crashes = {crashes}"
            )),
            1
        );
        assert_eq!(audio.work_one().await.expect("work"), Step::Idle);
        env.set_time(later);
    }
    assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'failed' AND crashes = 3 AND last_error = 'crashed the audio service'"),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r40_an_unreachable_service_costs_no_attempts() {
    // A port nothing listens on.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let endpoint = TtsEndpoint::parse(&format!("http://127.0.0.1:{port}")).expect("endpoint");
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    env.import("test-audio-book", vec![word("glimmerwick", 7, 1)])
        .await;
    assert!(audio.sync().await.is_err());
    // Nothing queued without the versions; the app answers "not ready".
    assert_eq!(env.int("SELECT COUNT(*) FROM audio_clip"), 0);
    let id = env.word_id("glimmerwick").await;
    let (_, clips) = env.get(&format!("/api/audio/words/{}", id.get())).await;
    assert_eq!(clips["word"]["ready"], false);
    let (_, status) = env.get("/api/audio/status").await;
    assert_eq!(status["connected"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r85_unused_clips_are_collected_and_restart_reconciles_files() {
    let (fake, endpoint) = fake_tts().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let env = env_with_audio(dir, START, Some(endpoint.clone()));
    let audio = env.audio.clone().expect("audio");
    env.import(
        "test-audio-book",
        vec![word("glimmerwick", 7, 1), word("brindlefoot", 6, 0)],
    )
    .await;
    assert_eq!(drain(&audio).await, 3);
    let files = |env: &common::Env| -> usize { walk(&env.dir.path().join("cache/audio")) };
    assert_eq!(files(&env), 3);

    // A text no word uses any more (word deleted): row and file go.
    env.exec("DELETE FROM card; DELETE FROM word WHERE headword = 'brindlefoot';");
    let (queued, deleted) = audio.sync().await.expect("sync");
    assert_eq!((queued, deleted), (0, 1));
    assert_eq!(files(&env), 2);
    assert_eq!(env.int("SELECT COUNT(*) FROM audio_clip"), 2);

    // Restart: a lost file is made again, a stray file is removed.
    let hash: String = env
        .sql()
        .query_row("SELECT hash FROM audio_clip WHERE kind = 'word'", [], |r| {
            r.get(0)
        })
        .expect("hash");
    std::fs::remove_file(
        env.dir
            .path()
            .join("cache/audio")
            .join(&hash[..2])
            .join(format!("{hash}.mp3")),
    )
    .expect("remove");
    let stray = env.dir.path().join("cache/audio/ab");
    std::fs::create_dir_all(&stray).expect("dir");
    std::fs::write(stray.join("abababababababababababababababab.mp3"), b"x").expect("stray");
    let common::Env { dir, .. } = env;
    let env = env_with_audio(dir, START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    assert_eq!(audio.reconcile().await.expect("reconcile"), (1, 1));
    let before = fake.speaks.load(Ordering::SeqCst);
    assert_eq!(drain(&audio).await, 1);
    assert_eq!(fake.speaks.load(Ordering::SeqCst), before + 1);
    assert_eq!(files(&env), 2);
}

fn walk(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| {
                    if e.path().is_dir() {
                        walk(&e.path())
                    } else {
                        1
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r42_the_voice_setting_selects_the_voice_and_keeps_the_other() {
    let (fake, endpoint) = fake_tts().await;
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    env.import("test-audio-book", vec![word("glimmerwick", 7, 0)])
        .await;
    drain(&audio).await;
    let (code, _) = env
        .request("PATCH", "/api/settings", Some(&json!({ "voice": "uk" })))
        .await;
    assert_eq!(code, StatusCode::OK);
    let id = env.word_id("glimmerwick").await;
    // Until the UK clip exists the US one is offered, marked as such.
    let (_, clips) = env.get(&format!("/api/audio/words/{}", id.get())).await;
    assert_eq!(
        (
            clips["word"]["ready"].clone(),
            clips["word"]["other_voice"].clone()
        ),
        (json!(true), json!(true))
    );
    drain(&audio).await;
    let (_, clips) = env.get(&format!("/api/audio/words/{}", id.get())).await;
    assert_eq!(clips["word"]["other_voice"], false);
    assert_eq!(*fake.voices.lock().expect("lock"), ["af_heart", "bf_emma"]);
    // Both voices' clips are kept while the text exists.
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready'"),
        2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r40_approximate_pronunciations_are_reported() {
    let (_fake, endpoint) = fake_tts().await;
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let mut odd = word("zxqoid", 6, 0);
    odd["speak"] = json!("zxqoid");
    env.import("test-audio-book", vec![odd, word("glimmerwick", 7, 0)])
        .await;
    drain(env.audio.as_ref().expect("audio")).await;
    let id = env.word_id("zxqoid").await;
    let (_, clips) = env.get(&format!("/api/audio/words/{}", id.get())).await;
    assert_eq!(clips["word"]["approximate"], true);
    let (_, status) = env.get("/api/audio/status").await;
    assert_eq!(status["approximate"], 1, "{status}");
}

#[tokio::test]
async fn r41_without_tts_the_app_reports_audio_off() {
    let env = env();
    env.import("test-audio-book", vec![word("glimmerwick", 7, 1)])
        .await;
    let id = env.word_id("glimmerwick").await;
    let (code, status) = env.get("/api/audio/status").await;
    assert_eq!(
        (code, status),
        (StatusCode::OK, json!({ "enabled": false }))
    );
    let (_, clips) = env.get(&format!("/api/audio/words/{}", id.get())).await;
    assert_eq!(clips, json!({ "enabled": false }));
    let (code, _) = env
        .get("/api/audio/clips/0123456789abcdef0123456789abcdef.mp3")
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (code, _) = env.post("/api/audio/retry", &json!({})).await;
    assert_eq!(code, StatusCode::CONFLICT);
}
