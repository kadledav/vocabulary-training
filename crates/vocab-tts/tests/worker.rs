//! The front's side of the engine process (`vocab_tts::worker`), with
//! small shell scripts standing in for the engine process: started on
//! demand, reused, stopped when idle, killed when the work is abandoned,
//! and every way it can fail is an error, never a hang.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use common::{error_code, send, speak_request};
use vocab_tts::speaker::{Backend, SpeakError, Speaker};
use vocab_tts::speech::{SpeakText, Speed, Voice};
use vocab_tts::worker::{Worker, WorkerCommand};

const READY: &str = r#"{"ready":{"lexicons_ms":1,"model_ms":2}}"#;

/// A reply with a tiny clip (one MPEG frame header is enough here).
fn spoken_reply() -> String {
    serde_json::json!({ "spoken": {
        "clip": { "mp3": "fff3e4c4", "duration_ms": 640, "loudness_lufs": -16.1 },
        "tally": { "lexicon": 1, "derived": 0, "fallback": 0, "spelled": 0, "dropped": 0 },
        "phoneme_symbols": 5, "synthesis_ms": 3, "postprocess_ms": 1
    }})
    .to_string()
}

/// `sh -c script` with the ready line as $1 and a reply as $2.
fn fake(script: &str, idle: Duration) -> Arc<Worker> {
    let reply = spoken_reply();
    let worker = Worker::new(
        WorkerCommand::new("sh", &["-c", script, "fake-engine", READY, &reply]),
        idle,
    );
    worker.watch_idle();
    worker
}

/// Says ready, then answers every line (after a line that is not
/// protocol, which is skipped).
const ANSWERS: &str =
    r#"printf '%s\n' "$1"; while read -r line; do printf 'noise\n%s\n' "$2"; done"#;

async fn say(speaker: &Arc<Speaker>) -> Result<vocab_tts::speaker::Spoken, SpeakError> {
    let text = SpeakText::new("hello").expect("test setup");
    speaker.speak(text, Voice::AfHeart, Speed::NORMAL).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_started_on_demand_reused_and_stopped_when_idle() {
    let worker = fake(ANSWERS, Duration::from_secs(2));
    let speaker = Speaker::new(Arc::clone(&worker) as _, 0);
    assert!(!worker.is_running().await, "nothing runs before a request");
    for _ in 0..3 {
        let spoken = say(&speaker).await.expect("spoken");
        assert_eq!(spoken.clip.duration_ms(), 640);
        assert_eq!(spoken.clip.mp3(), [0xff, 0xf3, 0xe4, 0xc4]);
        assert!(worker.is_running().await, "kept for the next request");
    }
    let mut stopped = false;
    for _ in 0..80 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !worker.is_running().await {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "stopped after the idle time");
    let app = vocab_tts::http::app(Arc::clone(&speaker));
    let request = axum::http::Request::get("/healthz")
        .body(axum::body::Body::empty())
        .expect("test setup");
    let (status, _, body) = send(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).expect("json"),
        serde_json::json!({ "status": "ok", "engine": "stopped" })
    );
    // And started again for the next request.
    say(&speaker).await.expect("spoken again");
    assert!(worker.healthy());
}

#[tokio::test]
async fn r83_idle_zero_stops_after_every_request() {
    let worker = fake(ANSWERS, Duration::ZERO);
    let speaker = Speaker::new(Arc::clone(&worker) as _, 0);
    say(&speaker).await.expect("spoken");
    assert!(!worker.is_running().await);
}

#[tokio::test]
async fn r83_a_failed_start_is_an_error_and_unhealthy_not_a_hang() {
    for script in [
        // The engine process reports why it cannot load.
        r#"printf '%s\n' '{"failed":"cannot load the model: test"}'; exit 1"#,
        // It exits without a word.
        "exit 3",
        // It says something else.
        r#"printf '{"hello":1}\n'"#,
    ] {
        let worker = fake(script, Duration::from_secs(60));
        let speaker = Speaker::new(Arc::clone(&worker) as _, 0);
        let result = tokio::time::timeout(Duration::from_secs(10), say(&speaker))
            .await
            .expect("no hang");
        assert!(
            matches!(result, Err(SpeakError::Worker(_))),
            "{script}: {result:?}"
        );
        assert!(!worker.healthy(), "{script}");
        assert!(!worker.is_running().await, "{script}");
        // The HTTP side: the request fails, /healthz says so.
        let app = vocab_tts::http::app(Arc::clone(&speaker));
        let (status, _, body) = send(&app, speak_request(r#"{"text": "hello"}"#)).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::INTERNAL_SERVER_ERROR, "synthesis_failed")
        );
        let request = axum::http::Request::get("/healthz")
            .body(axum::body::Body::empty())
            .expect("test setup");
        let (status, _, body) = send(&app, request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, "engine_failed")
        );
    }
}

#[tokio::test]
async fn r83_healthy_again_once_a_start_succeeds() {
    let marker = std::env::temp_dir().join(format!("vocab-tts-worker-test-{}", std::process::id()));
    std::fs::remove_file(&marker).ok();
    // Fails the first time (and leaves a marker), works the second time.
    let script = format!(
        r#"if [ -e '{0}' ]; then {ANSWERS}; else : > '{0}'; exit 1; fi"#,
        marker.display()
    );
    let worker = fake(&script, Duration::from_secs(60));
    let speaker = Speaker::new(Arc::clone(&worker) as _, 0);
    assert!(say(&speaker).await.is_err());
    assert!(!worker.healthy());
    say(&speaker).await.expect("spoken");
    assert!(worker.healthy());
    std::fs::remove_file(&marker).ok();
}

#[tokio::test]
async fn r83_an_engine_process_that_dies_while_speaking_is_a_crash() {
    // Ready, then killed (as by the kernel's OOM killer) on the request.
    let worker = fake(
        r#"printf '%s\n' "$1"; read -r line; kill -9 $$"#,
        Duration::from_secs(60),
    );
    let speaker = Speaker::new(Arc::clone(&worker) as _, 0);
    let result = say(&speaker).await;
    assert!(
        matches!(&result, Err(SpeakError::Crashed(status)) if status.contains("signal 9")),
        "{result:?}"
    );
    assert!(!worker.is_running().await);
    assert!(worker.healthy(), "it started fine; the text crashed it");
    // Over HTTP: a distinct code, so the app counts it as a crash.
    let app = vocab_tts::http::app(speaker);
    let (status, _, body) = send(&app, speak_request(r#"{"text": "hello"}"#)).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (StatusCode::INTERNAL_SERVER_ERROR, "engine_crashed")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_abandoned_work_stops_the_engine_process() {
    // Ready, then never answers.
    let worker = fake(
        r#"printf '%s\n' "$1"; read -r line; sleep 60"#,
        Duration::from_secs(60),
    );
    let speaker = Speaker::new(Arc::clone(&worker) as _, 0);
    let abandoned = tokio::time::timeout(Duration::from_millis(500), say(&speaker)).await;
    assert!(abandoned.is_err(), "the caller gave up");
    let mut stopped = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !worker.is_running().await {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "the engine process was killed");
    // The slot is free again: the next request is not busy.
    let next = tokio::time::timeout(Duration::from_millis(500), say(&speaker)).await;
    assert!(next.is_err(), "it reached the (new, silent) engine process");
}
