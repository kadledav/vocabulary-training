//! Robustness of the audio worker against a misbehaving TTS service
//! (design §10.3): a text that kills the service, a clip that is too long,
//! a full cache or disk, and a TTS version change. The fake service here
//! speaks raw HTTP/1.1 so it can drop a connection mid-request. Synthetic
//! texts only.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{START, env_with_audio, word};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vocab_server::audio::{Audio, CacheLimits, Step, TtsEndpoint};
use vocab_server::clock::Clock;

/// How the fake answers one text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Clip {
        duration_ms: u32,
    },
    /// Read the request, then close the connection without an answer (the
    /// service died on it).
    Crash,
    /// Read the request, close the connection and go down (the service was
    /// stopped or redeployed while it worked on the text).
    Vanish,
    /// `503` with this error code (the service is up but cannot work).
    Unavailable(&'static str),
    /// `500 engine_crashed` (the engine process died on the text).
    EngineCrashed,
}

struct Fake {
    frontend: Mutex<String>,
    /// Texts that get a special answer; every other text gets a 900 ms clip.
    special: Mutex<Vec<(String, Answer)>>,
    spoken: Mutex<Vec<String>>,
    connections: AtomicUsize,
    /// Every connection is closed at once (the service is gone).
    down: AtomicBool,
    /// Every text gets this answer (else [`Fake::special`] decides).
    all: Mutex<Option<Answer>>,
}

impl Fake {
    fn answer(&self, text: &str) -> Answer {
        if let Some(answer) = *self.all.lock().expect("lock") {
            return answer;
        }
        self.special
            .lock()
            .expect("lock")
            .iter()
            .find(|(t, _)| t == text)
            .map_or(Answer::Clip { duration_ms: 900 }, |(_, a)| *a)
    }

    fn times_spoken(&self, text: &str) -> usize {
        self.spoken
            .lock()
            .expect("lock")
            .iter()
            .filter(|t| *t == text)
            .count()
    }
}

fn mp3(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xF3, 0x84, 0xC0];
    bytes.extend(std::iter::repeat_n(0u8, 188));
    bytes.extend(text.bytes().map(|b| b ^ 0x5A));
    bytes
}

async fn handle(fake: Arc<Fake>, mut stream: tokio::net::TcpStream) {
    fake.connections.fetch_add(1, Ordering::SeqCst);
    if fake.down.load(Ordering::SeqCst) {
        return;
    }
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_len, length) = loop {
        let Ok(n) = stream.read(&mut chunk).await else {
            return;
        };
        if n == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buffer[..end]).to_lowercase();
            let length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (end + 4, length);
        }
    };
    while buffer.len() < head_len + length {
        let Ok(n) = stream.read(&mut chunk).await else {
            return;
        };
        if n == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buffer[..head_len]).into_owned();
    let (status, content_type, extra, body) = if head.starts_with("GET /v1/voices") {
        let frontend = fake.frontend.lock().expect("lock").clone();
        let json = format!(r#"{{"voices": [], "frontend": "{frontend}", "engine": "fake-1"}}"#);
        (
            "200 OK",
            "application/json",
            String::new(),
            json.into_bytes(),
        )
    } else {
        let json: serde_json::Value =
            serde_json::from_slice(&buffer[head_len..]).expect("json body");
        let text = json["text"].as_str().expect("text").to_owned();
        fake.spoken.lock().expect("lock").push(text.clone());
        match fake.answer(&text) {
            Answer::Crash => return, // dropped without a word
            Answer::Vanish => {
                fake.down.store(true, Ordering::SeqCst);
                return;
            }
            Answer::Unavailable(code) => (
                "503 Service Unavailable",
                "application/json",
                "Retry-After: 60\r\n".to_owned(),
                format!(r#"{{"error": {{"code": "{code}", "message": "retry later"}}}}"#)
                    .into_bytes(),
            ),
            Answer::EngineCrashed => (
                "500 Internal Server Error",
                "application/json",
                String::new(),
                br#"{"error": {"code": "engine_crashed", "message": "died"}}"#.to_vec(),
            ),
            Answer::Clip { duration_ms } => (
                "200 OK",
                "audio/mpeg",
                format!("X-Duration-Ms: {duration_ms}\r\nX-Approximate: 0\r\n"),
                mp3(&text),
            ),
        }
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(&body).await;
}

async fn fake_tts() -> (Arc<Fake>, TtsEndpoint) {
    let fake = Arc::new(Fake {
        frontend: Mutex::new("g2p-a".into()),
        special: Mutex::new(Vec::new()),
        spoken: Mutex::new(Vec::new()),
        connections: AtomicUsize::new(0),
        down: AtomicBool::new(false),
        all: Mutex::new(None),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let shared = Arc::clone(&fake);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(handle(Arc::clone(&shared), stream));
        }
    });
    let endpoint = TtsEndpoint::parse(&format!("http://127.0.0.1:{port}")).expect("endpoint");
    (fake, endpoint)
}

/// Work until nothing is due now; returns the steps that worked.
async fn drain_now(audio: &Audio) -> usize {
    let mut worked = 0;
    loop {
        match audio.work_one().await.expect("work") {
            Step::Worked => worked += 1,
            _ => return worked,
        }
        assert!(worked < 1000, "runaway");
    }
}

fn words() -> Vec<serde_json::Value> {
    ["glimmerwick", "brindlefoot", "thornquill", "mossberry"]
        .iter()
        .map(|w| word(w, 7, 0))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_a_text_that_crashes_the_service_is_quarantined_and_never_blocks_the_queue() {
    let (fake, endpoint) = fake_tts().await;
    fake.special
        .lock()
        .expect("lock")
        .push(("glimmerwick".into(), Answer::Crash));
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    env.import("test-audio-book", words()).await;
    audio.sync().await.expect("sync");
    // The crash counts as an attempt; the other clips are made meanwhile.
    assert_eq!(drain_now(&audio).await, 4);
    assert_eq!(fake.times_spoken("glimmerwick"), 1);
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready'"),
        3
    );
    assert_eq!(
        env.int("SELECT crashes FROM audio_clip WHERE text = 'glimmerwick'"),
        1
    );
    // Two more crashes (after the backoff) and it fails for good.
    for at in ["2026-11-20T10:00:00Z", "2026-11-20T12:00:00Z"] {
        env.set_time(at);
        assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
    }
    assert_eq!(fake.times_spoken("glimmerwick"), 3);
    let (status, error): (String, String) = env
        .sql()
        .query_row(
            "SELECT status, last_error FROM audio_clip WHERE text = 'glimmerwick'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row");
    assert_eq!(
        (status.as_str(), error.as_str()),
        ("failed", "crashed the audio service")
    );
    env.set_time("2026-11-25T12:00:00Z");
    assert_eq!(audio.work_one().await.expect("work"), Step::Idle);
    assert_eq!(fake.times_spoken("glimmerwick"), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_a_failing_clip_goes_behind_the_others() {
    let (fake, endpoint) = fake_tts().await;
    fake.special
        .lock()
        .expect("lock")
        .push(("glimmerwick".into(), Answer::Crash));
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    env.import("test-audio-book", words()).await;
    audio.sync().await.expect("sync");
    assert_eq!(drain_now(&audio).await, 4);
    assert_eq!(fake.times_spoken("glimmerwick"), 1);
    // Long after its backoff, a new clip that became due later still goes
    // first: the failed one waits behind it.
    env.set_time("2026-11-20T11:00:00Z");
    env.import("test-audio-book-2", vec![word("quillsprout", 7, 0)])
        .await;
    audio.sync().await.expect("sync");
    let before = fake.times_spoken("glimmerwick");
    assert_eq!(drain_now(&audio).await, 2);
    let spoken = fake.spoken.lock().expect("lock").clone();
    assert_eq!(
        spoken[spoken.len() - 2..],
        ["quillsprout".to_owned(), "glimmerwick".to_owned()]
    );
    assert_eq!(fake.times_spoken("glimmerwick"), before + 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r84_a_clip_longer_than_60_seconds_fails_at_once() {
    let (fake, endpoint) = fake_tts().await;
    fake.special.lock().expect("lock").push((
        "glimmerwick".into(),
        Answer::Clip {
            duration_ms: 60_001,
        },
    ));
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    env.import("test-audio-book", vec![word("glimmerwick", 7, 0)])
        .await;
    audio.sync().await.expect("sync");
    assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
    let (status, error): (String, String) = env
        .sql()
        .query_row("SELECT status, last_error FROM audio_clip", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("row");
    assert_eq!(
        (status.as_str(), error.as_str()),
        ("failed", "clip longer than 60 s")
    );
    assert_eq!(audio.work_one().await.expect("work"), Step::Idle);
}

fn audio_with(env: &common::Env, endpoint: TtsEndpoint, limits: CacheLimits) -> Audio {
    Audio::with_limits(
        env.db.clone(),
        &env.dir.path().join("cache"),
        endpoint,
        Arc::clone(&env.clock) as Arc<dyn Clock>,
        limits,
    )
    .expect("audio")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r85_a_full_cache_stops_new_clips_and_says_so() {
    let (fake, endpoint) = fake_tts().await;
    let env = env_with_audio(
        tempfile::tempdir().expect("tempdir"),
        START,
        Some(endpoint.clone()),
    );
    let audio = audio_with(
        &env,
        endpoint,
        CacheLimits {
            total_bytes: 300,
            reserve_bytes: 0,
        },
    );
    env.import("test-audio-book", words()).await;
    audio.sync().await.expect("sync");
    // Each fake clip is about 200 bytes: the second fills the cache.
    assert_eq!(drain_now(&audio).await, 2);
    assert_eq!(audio.work_one().await.expect("work"), Step::Paused);
    assert_eq!(fake.spoken.lock().expect("lock").len(), 2);
    let status = serde_json::to_value(audio.status().await.expect("status")).expect("json");
    assert_eq!(status["storage"], "cache_full", "{status}");
    assert_eq!(status["cache_limit_bytes"], 300);
    assert!(status["cache_bytes"].as_i64().expect("bytes") >= 300);
    assert_eq!(status["pending"], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r85_no_clip_is_written_when_the_disk_is_nearly_full() {
    let (fake, endpoint) = fake_tts().await;
    let env = env_with_audio(
        tempfile::tempdir().expect("tempdir"),
        START,
        Some(endpoint.clone()),
    );
    // A reserve larger than any disk: the disk is always "nearly full".
    let audio = audio_with(
        &env,
        endpoint,
        CacheLimits {
            total_bytes: 2 << 30,
            reserve_bytes: u64::MAX / 4,
        },
    );
    env.import("test-audio-book", words()).await;
    audio.sync().await.expect("sync");
    assert_eq!(audio.work_one().await.expect("work"), Step::Paused);
    assert!(fake.spoken.lock().expect("lock").is_empty());
    let status = serde_json::to_value(audio.status().await.expect("status")).expect("json");
    assert_eq!(status["storage"], "disk_low", "{status}");
    // The default limits leave room on the test disk.
    let (_, status) = env.get("/api/audio/status").await;
    assert_eq!(status["storage"], "ok", "{status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r40_after_a_version_change_old_clips_play_until_replaced_one_by_one() {
    let (fake, endpoint) = fake_tts().await;
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    env.import("test-audio-book", words()).await;
    audio.sync().await.expect("sync");
    assert_eq!(drain_now(&audio).await, 4);
    let id = env.word_id("glimmerwick").await;
    let uri = format!("/api/audio/words/{}", id.get());
    let (_, before) = env.get(&uri).await;
    let old_url = before["word"]["url"].as_str().expect("url").to_owned();

    *fake.frontend.lock().expect("lock") = "g2p-b".into();
    audio.sync().await.expect("sync");
    // The old clips are still there and still played.
    assert_eq!(env.int("SELECT COUNT(*) FROM audio_clip"), 8);
    let (_, during) = env.get(&uri).await;
    assert_eq!(during["word"]["url"], old_url.as_str(), "{during}");
    assert_eq!(during["word"]["ready"], true);
    let (status, _) = env.get(&old_url).await;
    assert_eq!(status, axum::http::StatusCode::OK);

    // Each new clip replaces its old one as soon as it is ready.
    assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
    assert_eq!(env.int("SELECT COUNT(*) FROM audio_clip"), 7);
    assert_eq!(drain_now(&audio).await, 3);
    assert_eq!(env.int("SELECT COUNT(*) FROM audio_clip"), 4);
    let (_, after) = env.get(&uri).await;
    assert_ne!(after["word"]["url"], old_url.as_str());
    let (status, _) = env.get(&old_url).await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    // A later sync keeps nothing old either.
    assert_eq!(audio.sync().await.expect("sync"), (0, 0));
}

/// Attempts and crashes of every clip, by text.
fn tries(env: &common::Env) -> Vec<(String, i64, i64)> {
    let conn = env.sql();
    let mut statement = conn
        .prepare("SELECT text, attempts, crashes FROM audio_clip ORDER BY text")
        .expect("query");
    statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("rows")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_a_service_that_cannot_work_costs_no_attempts_and_is_not_flooded() {
    for code in ["engine_unavailable", "shutting_down"] {
        let (fake, endpoint) = fake_tts().await;
        *fake.all.lock().expect("lock") = Some(Answer::Unavailable(code));
        let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
        let audio = env.audio.clone().expect("audio");
        env.import("test-audio-book", words()).await;
        audio.sync().await.expect("sync");
        // One request, then the worker waits: not the whole queue.
        for _ in 0..3 {
            assert_eq!(audio.work_one().await.expect("work"), Step::Unavailable);
        }
        assert_eq!(fake.spoken.lock().expect("lock").len(), 3, "{code}");
        assert!(
            tries(&env).iter().all(|(_, a, c)| (*a, *c) == (0, 0)),
            "{code}: {:?}",
            tries(&env)
        );
        // The service recovers: every clip is made, none was charged.
        *fake.all.lock().expect("lock") = None;
        assert_eq!(drain_now(&audio).await, 4);
        assert_eq!(
            env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready' AND attempts = 0 AND crashes = 0"),
            4,
            "{code}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_a_service_stopped_while_it_speaks_does_not_charge_the_clip() {
    let (fake, endpoint) = fake_tts().await;
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    env.import("test-audio-book", words()).await;
    audio.sync().await.expect("sync");
    // Three times: the connection drops and the service is gone (a
    // redeploy that killed it), then it is back.
    for _ in 0..3 {
        *fake.all.lock().expect("lock") = Some(Answer::Vanish);
        assert_eq!(audio.work_one().await.expect("work"), Step::Unreachable);
        fake.down.store(false, Ordering::SeqCst);
    }
    assert!(
        tries(&env).iter().all(|(_, a, c)| (*a, *c) == (0, 0)),
        "{:?}",
        tries(&env)
    );
    *fake.all.lock().expect("lock") = None;
    assert_eq!(drain_now(&audio).await, 4);
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready' AND attempts = 0 AND crashes = 0"),
        4
    );
    // A drop while the service stays up is still a crash of that text, as
    // is the service's own `engine_crashed`.
    for (answer, text) in [
        (Answer::Crash, "thornquill"),
        (Answer::EngineCrashed, "mossberry"),
    ] {
        env.exec(&format!(
            "UPDATE audio_clip SET status = 'pending', attempts = 0 WHERE text = '{text}'"
        ));
        fake.special
            .lock()
            .expect("lock")
            .push((text.into(), answer));
        assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
        assert_eq!(
            env.int(&format!(
                "SELECT crashes FROM audio_clip WHERE text = '{text}'"
            )),
            1,
            "{answer:?}"
        );
    }
}
