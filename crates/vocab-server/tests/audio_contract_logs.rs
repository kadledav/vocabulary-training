//! R-24 for the audio path: book text (headwords, `speak` texts, sentences)
//! never reaches the server's log, on any path of the pipeline. A test
//! binary of its own: the log subscriber is process-wide.

mod audio_fake;
mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use audio_fake::Mode;
use axum::http::StatusCode;
use common::{START, env_with_audio};
use serde_json::{Value, json};
use vocab_server::audio::Step;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn words() -> Vec<Value> {
    let mut word = common::word("zorblaxwick", 7, 0);
    word["speak"] = json!("zorblax wick");
    word["sentences"] = json!([
        {
            "en": "The quuxington [[zorblaxwick]] hummed sneebly.",
            "cs": "Quuxingtonský [[zorblaxwick]] bzučel.",
            "origin": "example",
        },
        {
            "en": "A flimsnorp sentence about [[zorblaxwick]].",
            "cs": "Flimsnorpová věta o [[zorblaxwick]].",
            "origin": "example",
        },
    ]);
    vec![word]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r24_no_pack_text_in_the_server_log_on_any_audio_path() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("one subscriber");

    let (fake, endpoint) = audio_fake::start().await;
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    let worker = audio.spawn();
    env.import("test-audio-book", words()).await;
    let id = env.word_id("zorblaxwick").await;
    // Wait for the background worker to finish the queue.
    for _ in 0..200 {
        if env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready'") == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready'"),
        3
    );
    worker.abort();

    // Serving, status, settings, voice change.
    let (_, clips) = env.get(&format!("/api/audio/words/{}", id.get())).await;
    let url = clips["word"]["url"].as_str().expect("url").to_owned();
    assert_eq!(env.get(&url).await.0, StatusCode::OK);
    env.get("/api/audio/status").await;
    env.request("PATCH", "/api/settings", Some(&json!({ "voice": "uk" })))
        .await;

    // Failure paths: 500, a refusal that echoes the text, non-audio,
    // oversize, a service that is down.
    for mode in [
        Mode::Server500,
        Mode::Refuse,
        Mode::Html,
        Mode::NotMp3,
        Mode::Oversize,
    ] {
        fake.set_mode(mode);
        audio.sync().await.expect("sync");
        for _ in 0..4 {
            if audio.work_one().await.expect("work") != Step::Worked {
                break;
            }
        }
        env.set_time("2026-11-25T09:00:00Z");
    }
    fake.down.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = audio.sync().await;
    let _ = audio.work_one().await;
    // A word edit and a hide (the sync paths that collect clips).
    fake.down.store(false, std::sync::atomic::Ordering::SeqCst);
    fake.set_mode(Mode::Ok);
    env.exec("UPDATE sentence SET en_marked = 'An edited flimsnorp [[zorblaxwick]].', edited = 1 WHERE position = 0");
    env.exec("UPDATE sentence SET hidden = 1 WHERE position = 1");
    let _ = audio.sync().await;
    env.post("/api/audio/retry", &json!({})).await;

    let logs = String::from_utf8_lossy(&captured.0.lock().expect("lock")).to_lowercase();
    assert!(
        logs.contains("audio clip ready"),
        "audio was logged at all:\n{logs}"
    );
    for fragment in [
        "zorblax",
        "quuxington",
        "sneebly",
        "flimsnorp",
        "hummed",
        "bzučel",
        "bzucel",
        "bad text",
    ] {
        assert!(
            !logs.contains(fragment),
            "{fragment:?} reached the log:\n{logs}"
        );
    }
}
