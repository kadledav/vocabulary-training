//! Contract tests of the neural audio, written from the requirements
//! (R-40 server-side audio for headwords and sentences, R-41 browser speech
//! only as the fallback, R-42 voice, R-24 no book text in logs, R-83/R-84
//! hardening) and design §10.3 (pipeline, queue, serving) against a
//! scriptable fake TTS service. No real synthesis.

mod audio_fake;
mod common;

use std::time::Duration;

use audio_fake::{Mode, fake_mp3};
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use common::{START, env, env_with_audio, word};
use serde_json::{Value, json};
use tower::ServiceExt;
use vocab_server::audio::{Audio, Step, TtsEndpoint};

/// Sync, then work until nothing is due; returns how many clips were worked.
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

/// (range header, expected status, expected inclusive byte window).
type RangeCase = (String, StatusCode, Option<(usize, usize)>);

fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(files_under(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

fn audio_files(env: &common::Env) -> Vec<std::path::PathBuf> {
    files_under(&env.dir.path().join("cache/audio"))
}

async fn clips_of(env: &common::Env, headword: &str) -> Value {
    let id = env.word_id(headword).await;
    let (status, clips) = env.get(&format!("/api/audio/words/{}", id.get())).await;
    assert_eq!(status, StatusCode::OK);
    clips
}

async fn started(audio_tmp: tempfile::TempDir, endpoint: TtsEndpoint) -> (common::Env, Audio) {
    let env = env_with_audio(audio_tmp, START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    (env, audio)
}

/// A word with sentences with markers, an ellipsis, and a `speak` text.
fn rich_words() -> Vec<Value> {
    let mut glimmer = word("glimmerwick", 7, 3);
    glimmer["speak"] = json!("glimmer wick");
    let mut brindle = word("brindlefoot", 6, 0);
    brindle["sentences"] = json!([{
        "en": "…the [[brindlefoot]] ran off into the dark...",
        "cs": "…[[brindlefoot]] utekl do tmy…",
        "origin": "example",
    }]);
    vec![glimmer, brindle]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r40_clips_cover_the_speak_text_and_visible_sentences_without_markers() {
    let (fake, endpoint) = audio_fake::start().await;
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
    env.import("test-audio-book", rich_words()).await;
    // Hide the second sentence of glimmerwick (an owner's choice).
    let id = env.word_id("glimmerwick").await;
    env.exec(&format!(
        "UPDATE sentence SET hidden = 1 WHERE word_id = {} AND position = 1",
        id.get()
    ));
    drain(&audio).await;
    let mut expected = vec![
        // The pack's `speak` text, not the headword.
        "glimmer wick".to_owned(),
        "brindlefoot".to_owned(),
        // Visible sentences, `[[ ]]` stripped.
        "Test line 1 with the glimmerwick in it.".to_owned(),
        "Test line 3 with the glimmerwick in it.".to_owned(),
        // Leading/trailing ellipsis trimmed (design §10.3 step 1).
        "the brindlefoot ran off into the dark".to_owned(),
    ];
    expected.sort();
    assert_eq!(fake.texts(), expected);
    assert!(
        fake.texts()
            .iter()
            .all(|t| !t.contains("[[") && !t.contains("]]"))
    );
    // Czech is never spoken.
    assert!(
        fake.texts()
            .iter()
            .all(|t| !t.contains("řádek") && !t.contains("utekl"))
    );

    // The API offers clips for the word and the two visible sentences only.
    let clips = clips_of(&env, "glimmerwick").await;
    assert_eq!(clips["word"]["ready"], true, "{clips}");
    let sentences = clips["sentences"].as_array().expect("sentences");
    assert_eq!(sentences.len(), 2, "hidden sentence has no clip: {clips}");
    assert!(sentences.iter().all(|s| s["ready"] == true));
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip"),
        5,
        "no clip for the hidden sentence"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r40_identical_texts_share_one_clip() {
    let (fake, endpoint) = audio_fake::start().await;
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
    let mut first = word("glimmerwick", 7, 0);
    first["speak"] = json!("shared sound");
    let mut second = word("brindlefoot", 6, 0);
    second["speak"] = json!("shared sound");
    env.import("test-audio-book", vec![first, second]).await;
    assert_eq!(drain(&audio).await, 1);
    assert_eq!(fake.speaks(), 1);
    for headword in ["glimmerwick", "brindlefoot"] {
        assert_eq!(clips_of(&env, headword).await["word"]["ready"], true);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r40_a_reimport_without_changes_queues_nothing() {
    let (fake, endpoint) = audio_fake::start().await;
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
    env.import("test-audio-book", rich_words()).await;
    assert_eq!(drain(&audio).await, 6);
    let rows = env.rows("audio_clip", "1 = 1");
    let speaks = fake.speaks();

    let report = env.import("test-audio-book", rich_words()).await;
    assert_eq!(report["packs"][0]["status"], "imported", "{report}");
    assert_eq!(audio.sync().await.expect("sync"), (0, 0));
    assert_eq!(audio.work_one().await.expect("work"), Step::Idle);
    assert_eq!(fake.speaks(), speaks, "nothing was synthesised again");
    assert_eq!(env.rows("audio_clip", "1 = 1"), rows, "queue untouched");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r40_an_edited_sentence_gets_a_new_clip_and_the_old_one_is_collected() {
    let (fake, endpoint) = audio_fake::start().await;
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
    env.import("test-audio-book", vec![word("glimmerwick", 7, 2)])
        .await;
    drain(&audio).await;
    let before = clips_of(&env, "glimmerwick").await;
    let old_url = before["sentences"][0]["url"]
        .as_str()
        .expect("url")
        .to_owned();
    assert_eq!(get_raw(&env, &old_url, &[]).await.0, StatusCode::OK);
    let id = env.word_id("glimmerwick").await;

    // The owner corrects the first sentence (R-25).
    env.exec(&format!(
        "UPDATE sentence SET en_marked = 'The corrected line holds the [[glimmerwick]].', edited = 1 WHERE word_id = {} AND position = 0",
        id.get()
    ));
    // Old text gone, new text queued; the unchanged clips stay.
    assert_eq!(audio.sync().await.expect("sync"), (1, 1));
    assert_eq!(drain(&audio).await, 1);
    assert!(
        fake.texts()
            .contains(&"The corrected line holds the glimmerwick.".to_owned())
    );
    let after = clips_of(&env, "glimmerwick").await;
    let new_url = after["sentences"][0]["url"].as_str().expect("url");
    assert_ne!(new_url, old_url);
    assert_eq!(after["sentences"][1], before["sentences"][1]);
    assert_eq!(get_raw(&env, new_url, &[]).await.0, StatusCode::OK);
    // The old clip is garbage: row, file and route are gone.
    assert_eq!(get_raw(&env, &old_url, &[]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(audio_files(&env).len(), 3);
    assert_eq!(env.int("SELECT COUNT(*) FROM audio_clip"), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r40_hiding_a_sentence_collects_its_clip_and_unhiding_makes_it_again() {
    let (fake, endpoint) = audio_fake::start().await;
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
    env.import("test-audio-book", vec![word("glimmerwick", 7, 2)])
        .await;
    drain(&audio).await;
    let id = env.word_id("glimmerwick").await;
    env.exec(&format!(
        "UPDATE sentence SET hidden = 1 WHERE word_id = {} AND position = 0",
        id.get()
    ));
    assert_eq!(audio.sync().await.expect("sync"), (0, 1));
    assert_eq!(audio_files(&env).len(), 2);
    env.exec(&format!(
        "UPDATE sentence SET hidden = 0 WHERE word_id = {} AND position = 0",
        id.get()
    ));
    let speaks = fake.speaks();
    assert_eq!(drain(&audio).await, 1);
    assert_eq!(fake.speaks(), speaks + 1);
    assert_eq!(audio_files(&env).len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_the_clip_route_needs_a_session() {
    let (_fake, endpoint) = audio_fake::start().await;
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
    env.import("test-audio-book", vec![word("glimmerwick", 7, 0)])
        .await;
    drain(&audio).await;
    let url = clips_of(&env, "glimmerwick").await["word"]["url"]
        .as_str()
        .expect("url")
        .to_owned();
    // With the session: served.
    assert_eq!(get_raw(&env, &url, &[]).await.0, StatusCode::OK);
    // The same app without the session cookie.
    let bare = vocab_server::http::app(
        vocab_server::http::AppState::new(env.db.clone(), common::prague(), common::auth().0)
            .with_audio(audio.clone()),
        &vocab_server::config::Limits::default(),
    );
    for (uri, extra) in [
        (url.as_str(), None),
        (url.as_str(), Some(("range", "bytes=0-3"))),
        (url.as_str(), Some(("cookie", "vocab_session=forged"))),
        (url.as_str(), Some(("cookie", "session=forged"))),
    ] {
        let mut builder = Request::builder().uri(uri);
        if let Some((name, value)) = extra {
            builder = builder.header(name, value);
        }
        let response = bare
            .clone()
            .oneshot(builder.body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{uri} {extra:?}"
        );
        let body = to_bytes(response.into_body(), 1 << 16).await.expect("body");
        assert!(
            !body.windows(4).any(|w| w == [0xFF, 0xF3, 0x84, 0xC0]),
            "no audio bytes without a session"
        );
    }
    // State-changing audio routes need one too.
    let response = bare
        .clone()
        .oneshot(common::post_json("/api/audio/retry", &json!({}), None))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r41_clip_responses_carry_type_private_caching_and_byte_ranges() {
    let (_fake, endpoint) = audio_fake::start().await;
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
    let mut w = word("glimmerwick", 7, 0);
    w["speak"] = json!("glimmer wick");
    env.import("test-audio-book", vec![w]).await;
    drain(&audio).await;
    let url = clips_of(&env, "glimmerwick").await["word"]["url"]
        .as_str()
        .expect("url")
        .to_owned();
    let full = fake_mp3("glimmer wick");
    let len = full.len();

    let (status, headers, body) = get_raw(&env, &url, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, full);
    assert_eq!(headers[header::CONTENT_TYPE], "audio/mpeg");
    let cache = headers[header::CACHE_CONTROL].to_str().expect("cache");
    assert!(cache.contains("private"), "never a shared cache: {cache}");
    assert!(!cache.contains("public"), "{cache}");
    assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
    assert_eq!(headers[header::CONTENT_LENGTH], len.to_string().as_str());

    // (range header, expected status, expected inclusive byte window).
    let last = len - 1;
    let cases: Vec<RangeCase> = vec![
        (
            "bytes=0-0".into(),
            StatusCode::PARTIAL_CONTENT,
            Some((0, 0)),
        ),
        (
            "bytes=0-99".into(),
            StatusCode::PARTIAL_CONTENT,
            Some((0, 99)),
        ),
        (
            format!("bytes=10-{}", len + 500),
            StatusCode::PARTIAL_CONTENT,
            Some((10, last)),
        ),
        (
            format!("bytes={last}-"),
            StatusCode::PARTIAL_CONTENT,
            Some((last, last)),
        ),
        (
            "bytes=-1".into(),
            StatusCode::PARTIAL_CONTENT,
            Some((last, last)),
        ),
        (
            format!("bytes=-{}", len + 10),
            StatusCode::PARTIAL_CONTENT,
            Some((0, last)),
        ),
        (
            format!("bytes={len}-"),
            StatusCode::RANGE_NOT_SATISFIABLE,
            None,
        ),
        (
            format!("bytes={}-{}", len + 5, len + 9),
            StatusCode::RANGE_NOT_SATISFIABLE,
            None,
        ),
    ];
    for (range, expected, window) in cases {
        let (status, headers, body) = get_raw(&env, &url, &[("range", &range)]).await;
        assert_eq!(status, expected, "{range}");
        match window {
            Some((from, to)) => {
                assert_eq!(body, full[from..=to], "{range}");
                assert_eq!(
                    headers[header::CONTENT_RANGE],
                    format!("bytes {from}-{to}/{len}").as_str(),
                    "{range}"
                );
                assert_eq!(
                    headers[header::CONTENT_LENGTH],
                    (to - from + 1).to_string().as_str()
                );
                assert_eq!(headers[header::CONTENT_TYPE], "audio/mpeg");
            }
            None => assert_eq!(
                headers[header::CONTENT_RANGE],
                format!("bytes */{len}").as_str(),
                "{range}"
            ),
        }
    }
    // Malformed or unusable ranges never produce a wrong slice: either the
    // whole clip or 416 (RFC 9110 lets a server ignore an invalid Range).
    for range in ["bytes=9-5", "bytes=abc", "items=0-3", "bytes=", "bytes=-"] {
        let (status, _, body) = get_raw(&env, &url, &[("range", range)]).await;
        assert!(
            (status == StatusCode::OK && body == full)
                || status == StatusCode::RANGE_NOT_SATISFIABLE
                || status == StatusCode::BAD_REQUEST,
            "{range}: {status}"
        );
    }
}

#[tokio::test]
async fn r40_without_the_tts_url_nothing_is_queued_and_the_app_works_as_before() {
    let env = env();
    env.import("test-audio-book", rich_words()).await;
    let id = env.word_id("glimmerwick").await;
    // No queue, no files, the audio API says "off" (R-41: browser speech).
    assert_eq!(env.int("SELECT COUNT(*) FROM audio_clip"), 0);
    assert!(audio_files(&env).is_empty());
    let (code, clips) = env.get(&format!("/api/audio/words/{}", id.get())).await;
    assert_eq!((code, clips), (StatusCode::OK, json!({ "enabled": false })));
    let (_, status) = env.get("/api/audio/status").await;
    assert_eq!(status, json!({ "enabled": false }));
    // Importing again and changing settings queue nothing either.
    env.import("test-audio-book", rich_words()).await;
    let (code, _) = env
        .request("PATCH", "/api/settings", Some(&json!({ "voice": "uk" })))
        .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(env.int("SELECT COUNT(*) FROM audio_clip"), 0);
    // Practice is the v0.1.1 behaviour.
    let (code, started) = env.post("/api/sessions", &json!({ "kind": "start" })).await;
    assert_eq!(code, StatusCode::OK, "{started}");
    assert!(!started["session"].is_null(), "{started}");
    let session = started["session"].clone();
    assert!(
        session["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    let (code, _) = env.get("/api/words").await;
    assert_eq!(code, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r41_a_down_tts_never_blocks_import_or_practice_and_is_retried_later() {
    let (fake, endpoint) = audio_fake::start().await;
    fake.down.store(true, std::sync::atomic::Ordering::SeqCst);
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
    // The real worker runs against the dead service.
    let worker = audio.spawn();
    let imported = tokio::time::timeout(
        Duration::from_secs(10),
        env.import("test-audio-book", rich_words()),
    )
    .await;
    assert!(imported.is_ok(), "import must not wait for the TTS service");
    let (code, started) = tokio::time::timeout(
        Duration::from_secs(10),
        env.post("/api/sessions", &json!({ "kind": "start" })),
    )
    .await
    .expect("practice must not wait for the TTS service");
    assert_eq!(code, StatusCode::OK, "{started}");
    assert!(!started["session"].is_null());
    let clips = clips_of(&env, "glimmerwick").await;
    assert_eq!(clips["word"]["ready"], false, "{clips}");
    let (_, status) = env.get("/api/audio/status").await;
    assert_eq!(status["connected"], false, "{status}");
    assert_eq!(fake.speaks(), 0);
    worker.abort();

    // It comes back: the very same clips are made, nothing was lost.
    fake.down.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(drain(&audio).await, 6);
    assert_eq!(clips_of(&env, "glimmerwick").await["word"]["ready"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r41_a_service_that_goes_down_mid_queue_costs_no_attempts_and_loses_nothing() {
    let (fake, endpoint) = audio_fake::start().await;
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
    env.import("test-audio-book", rich_words()).await;
    audio.sync().await.expect("sync");
    assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
    fake.down.store(true, std::sync::atomic::Ordering::SeqCst);
    fake.set_mode(Mode::Server500);
    assert_eq!(audio.work_one().await.expect("work"), Step::Unreachable);
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE attempts > 0"),
        0
    );
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'failed'"),
        0
    );
    fake.down.store(false, std::sync::atomic::Ordering::SeqCst);
    fake.set_mode(Mode::Ok);
    assert_eq!(drain(&audio).await, 5);
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready'"),
        6
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r84_a_non_audio_or_oversize_answer_stores_nothing_and_never_becomes_a_clip() {
    for mode in [Mode::Html, Mode::NotMp3, Mode::WrongType, Mode::Oversize] {
        let (fake, endpoint) = audio_fake::start().await;
        fake.set_mode(mode);
        let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
        env.import("test-audio-book", vec![word("glimmerwick", 7, 0)])
            .await;
        audio.sync().await.expect("sync");
        assert_eq!(
            audio.work_one().await.expect("work"),
            Step::Worked,
            "{mode:?}"
        );
        assert_eq!(fake.speaks(), 1);
        assert!(
            audio_files(&env).is_empty(),
            "{mode:?}: {:?}",
            audio_files(&env)
        );
        assert_eq!(
            env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready'"),
            0
        );
        let clips = clips_of(&env, "glimmerwick").await;
        assert_eq!(clips["word"]["ready"], false, "{mode:?}");
        assert!(clips["word"]["url"].is_null());
        let (_, status) = env.get("/api/audio/status").await;
        assert_eq!(status["ready"], 0, "{mode:?}");
        // Retried with backoff, then failed for good: never ready, never stored.
        for day in 21..31 {
            env.set_time(&format!("2026-11-{day:02}T09:00:00Z"));
            let _ = audio.work_one().await.expect("work");
        }
        assert_eq!(
            env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'failed'"),
            1,
            "{mode:?} ends failed after the last attempt"
        );
        assert!(audio_files(&env).is_empty(), "{mode:?}");
        let (_, status) = env.get("/api/audio/status").await;
        assert_eq!(
            (status["failed"].clone(), status["ready"].clone()),
            (json!(1), json!(0))
        );
        // The owner's "Retry failed" recovers it once the service is right.
        fake.set_mode(Mode::Ok);
        let (code, retried) = env.post("/api/audio/retry", &json!({})).await;
        assert_eq!(
            (code, retried["retried"].clone()),
            (StatusCode::OK, json!(1))
        );
        assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
        assert_eq!(audio_files(&env).len(), 1);
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r83_clip_files_and_directories_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let (_fake, endpoint) = audio_fake::start().await;
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
    env.import("test-audio-book", rich_words()).await;
    drain(&audio).await;
    let mode = |path: &std::path::Path| {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };
    let files = audio_files(&env);
    assert_eq!(files.len(), 6);
    for file in &files {
        assert_eq!(mode(file), 0o600, "{}", file.display());
        assert_eq!(
            file.extension().and_then(|e| e.to_str()),
            Some("mp3"),
            "no .tmp left over"
        );
        let mut dir = file.parent();
        while let Some(d) = dir {
            if d == env.dir.path().join("cache") {
                break;
            }
            assert_eq!(
                mode(d) & 0o077,
                0,
                "{} is closed to group and others",
                d.display()
            );
            dir = d.parent();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r85_a_restart_keeps_ready_clips_and_resumes_the_queue() {
    let (fake, endpoint) = audio_fake::start().await;
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint.clone()).await;
    env.import("test-audio-book", rich_words()).await;
    audio.sync().await.expect("sync");
    assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
    assert_eq!(audio.work_one().await.expect("work"), Step::Worked);
    let done: Vec<String> = {
        let conn = env.sql();
        let mut stmt = conn
            .prepare("SELECT hash FROM audio_clip WHERE status = 'ready' ORDER BY hash")
            .expect("prepare");
        stmt.query_map([], |r| r.get(0))
            .expect("rows")
            .map(|r| r.expect("row"))
            .collect()
    };
    assert_eq!(done.len(), 2);

    // Restart: same data directory, a fresh process.
    let common::Env { dir, .. } = env;
    let (env, audio) = started(dir, endpoint).await;
    assert_eq!(audio.reconcile().await.expect("reconcile"), (0, 0));
    assert_eq!(
        env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready'"),
        2
    );
    assert_eq!(audio_files(&env).len(), 2);
    // The clips that were ready are served right away.
    let mut served = 0;
    for headword in ["glimmerwick", "brindlefoot"] {
        let clips = clips_of(&env, headword).await;
        for url in std::iter::once(&clips["word"]).chain(clips["sentences"].as_array().expect("s"))
        {
            if let Some(url) = url["url"].as_str().or(url["clip"]["url"].as_str()) {
                assert_eq!(get_raw(&env, url, &[]).await.0, StatusCode::OK);
                served += 1;
            }
        }
    }
    assert_eq!(served, 2);
    // The queue resumes with exactly the missing clips.
    let before = fake.speaks();
    assert_eq!(drain(&audio).await, 4);
    assert_eq!(fake.speaks(), before + 4);
    assert_eq!(audio_files(&env).len(), 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r42_a_voice_change_queues_the_other_voice_in_the_background() {
    let (fake, endpoint) = audio_fake::start().await;
    let (env, audio) = started(tempfile::tempdir().expect("tempdir"), endpoint).await;
    let worker = audio.spawn();
    env.import("test-audio-book", rich_words()).await;
    wait_for(|| async { env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready'") == 6 })
        .await;
    assert!(
        fake.voices
            .lock()
            .expect("lock")
            .iter()
            .all(|v| v == "af_heart")
    );

    // An unknown voice is refused and queues nothing.
    let (code, _) = env
        .request(
            "PATCH",
            "/api/settings",
            Some(&json!({ "voice": "klingon" })),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    let (code, settings) = env
        .request("PATCH", "/api/settings", Some(&json!({ "voice": "uk" })))
        .await;
    assert_eq!(code, StatusCode::OK, "{settings}");
    wait_for(|| async { env.int("SELECT COUNT(*) FROM audio_clip WHERE status = 'ready'") == 12 })
        .await;
    let voices = fake.voices.lock().expect("lock").clone();
    assert_eq!(voices.iter().filter(|v| *v == "bf_emma").count(), 6);
    assert_eq!(voices.iter().filter(|v| *v == "af_heart").count(), 6);
    // The UK clip is what the word offers now.
    let clips = clips_of(&env, "glimmerwick").await;
    assert_eq!(clips["word"]["ready"], true);
    assert_eq!(clips["word"]["other_voice"], false, "{clips}");
    worker.abort();
}

async fn wait_for<F, Fut>(mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if condition().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("condition not reached within 20 s");
}
