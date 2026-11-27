//! Contract of the Words API (design §8, §7.1 screens 6 and 7; R-04, R-25,
//! R-73): list filters and sorts, request validation and limits, audio
//! after an edit (R-40), and the list's speed with 5 000 words. Synthetic
//! words only.

mod audio_fake;
mod common;

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{START, env, env_with_audio, word};
use serde_json::{Value, json};
use vocab_server::audio::{Audio, Step};

const BOOK: &str = "test-words-book";
const ARTICLE: &str = "test-words-article";

fn headwords(rows: &Value) -> Vec<String> {
    rows.as_array()
        .unwrap_or_else(|| panic!("not a list: {rows}"))
        .iter()
        .map(|r| r["headword"].as_str().expect("headword").to_owned())
        .collect()
}

async fn list(env: &common::Env, query: &str) -> Value {
    let (status, rows) = env.get(&format!("/api/words{query}")).await;
    assert_eq!(status, StatusCode::OK, "{query}: {rows}");
    rows
}

#[tokio::test]
async fn r73_the_list_filters_sorts_and_tells_each_words_status() {
    let env = env();
    let mut sift = word("sift", 4, 3);
    sift["tag"] = json!({ "kind": "low-priority" });
    env.import(BOOK, vec![word("lull", 9, 3), sift, word("dwindle", 6, 1)])
        .await;
    env.import(ARTICLE, vec![word("bristle", 2, 3)]).await;
    // A conflict for dwindle, a review for lull.
    let mut changed = word("dwindle", 6, 1);
    changed["meaning"] = json!("Another invented meaning.");
    env.import(BOOK, vec![changed]).await;
    let lull = env.word_id("lull").await.get();
    env.exec(&format!(
        "UPDATE card SET state = 'review', stability = 9.5, difficulty = 5.0, \
         due_day = '2026-11-25', last_review_day = '2026-11-18', reps = 1 WHERE word_id = {lull}"
    ));

    let all = list(&env, "").await;
    assert_eq!(headwords(&all), ["bristle", "dwindle", "lull", "sift"]);
    let row = all
        .as_array()
        .expect("rows")
        .iter()
        .find(|r| r["headword"] == "lull")
        .expect("lull");
    assert_eq!(row["status"], "familiar");
    assert_eq!(row["next_due"], "2026-11-25");
    assert_eq!(row["czech"], json!(["lull-cz"]));
    assert_eq!(row["sentence_count"], 3);
    assert!(row["created_at"].as_str().is_some());

    for (query, expected) in [
        (
            "?sort=usefulness",
            vec!["lull", "dwindle", "sift", "bristle"],
        ),
        ("?sort=due", vec!["lull", "bristle", "dwindle", "sift"]),
        ("?sort=status", vec!["lull", "bristle", "dwindle", "sift"]),
        ("?sort=added", vec!["bristle", "dwindle", "sift", "lull"]),
        ("?status=new", vec!["bristle", "dwindle", "sift"]),
        ("?status=familiar", vec!["lull"]),
        ("?status=known", vec![]),
        ("?min_u=5&max_u=9", vec!["dwindle", "lull"]),
        ("?flags=conflicts", vec!["dwindle"]),
        ("?flags=low_priority", vec!["sift"]),
        ("?flags=suspended", vec![]),
        ("?flags=conflicts,low_priority", vec![]),
        ("?q=LULL-CZ", vec!["lull"]),
        ("?q=test%20meaning%20of%20s", vec!["sift"]),
        (
            "?q=&sort=&status=&flags=",
            vec!["bristle", "dwindle", "lull", "sift"],
        ),
    ] {
        assert_eq!(headwords(&list(&env, query).await), expected, "{query}");
    }
    // With a source: source order by default, any sort on request.
    let (_, sources) = env.get("/api/sources").await;
    let book = sources
        .as_array()
        .expect("sources")
        .iter()
        .find(|s| s["slug"] == BOOK)
        .expect("book")["id"]
        .clone();
    assert_eq!(
        headwords(&list(&env, &format!("?source={book}")).await),
        ["lull", "sift", "dwindle"]
    );
    assert_eq!(
        headwords(&list(&env, &format!("?source={book}&sort=alpha")).await),
        ["dwindle", "lull", "sift"]
    );
}

#[tokio::test]
async fn r73_bad_list_parameters_are_refused_in_plain_words() {
    let env = env();
    for query in [
        "?sort=random",
        "?status=forgotten",
        "?flags=starred",
        "?min_u=0",
        "?max_u=11",
        "?min_u=five",
        "?source=book",
    ] {
        let (status, body) = env.get(&format!("/api/words{query}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        assert_eq!(body["error"]["code"], "bad_request", "{query}");
    }
    let long = "x".repeat(201);
    let (status, body) = env.get(&format!("/api/words?q={long}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("message")
            .contains("too long")
    );
}

#[tokio::test]
async fn r04_edits_are_validated_like_pack_fields() {
    let env = env();
    env.import(BOOK, vec![word("lull", 7, 3), word("sift", 6, 3)])
        .await;
    let id = env.word_id("lull").await.get();
    let uri = format!("/api/words/{id}");
    for (body, needle) in [
        (json!({}), "Nothing to change"),
        (json!({ "seq": 2 }), "not a field that can be edited"),
        (json!({ "meaning": "  " }), "blank"),
        (json!({ "meaning": "x".repeat(1501) }), "too long"),
        (json!({ "usefulness": 0 }), "from 1 to 10"),
        (json!({ "goal": "produce" }), "goal must be"),
        (json!({ "headword": "" }), "headword"),
        (json!({ "note": "bell\u{7}" }), "control characters"),
        (json!({ "spelling": "yes" }), "true or false"),
        (json!(["meaning"]), "JSON object"),
    ] {
        let (status, answer) = env.request("PATCH", &uri, Some(&body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {answer}");
        assert_eq!(answer["error"]["code"], "invalid");
        let message = answer["error"]["message"].as_str().expect("message");
        assert!(message.contains(needle), "{body}: {message}");
    }
    // Several problems are all named; nothing was stored.
    let (status, answer) = env
        .request(
            "PATCH",
            &uri,
            Some(&json!({ "usefulness": 0, "meaning": "Fine.", "goal": "x" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let message = answer["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("usefulness") && message.contains("goal"),
        "{message}"
    );
    assert_eq!(env.int("SELECT COUNT(*) FROM word_override"), 0);
    assert_eq!(
        env.int(&format!("SELECT usefulness FROM word WHERE id = {id}")),
        7
    );

    let (status, _) = env
        .request(
            "PATCH",
            "/api/words/99999",
            Some(&json!({ "usefulness": 3 })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = env
        .request("PATCH", "/api/words/abc", Some(&json!({ "usefulness": 3 })))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Limits: a note of 6000 characters is fine, the body limit holds.
    let (status, _) = env
        .request("PATCH", &uri, Some(&json!({ "note": "n".repeat(6000) })))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = env
        .request("PATCH", &uri, Some(&json!({ "note": "n".repeat(70_000) })))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let request = Request::builder()
        .method("PATCH")
        .uri(&uri)
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .expect("request");
    let (status, body) = env.send(request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn r04_items_actions_and_conflicts_check_their_input() {
    let env = env();
    env.import(BOOK, vec![word("lull", 7, 3)]).await;
    let id = env.word_id("lull").await.get();
    let items = format!("/api/words/{id}/items");
    for body in [
        json!({ "kind": "meaning", "value": "x" }),
        json!({ "kind": "czech", "value": "   " }),
        json!({ "kind": "czech", "value": "x".repeat(201) }),
        json!({ "kind": "form", "value": "x".repeat(101) }),
    ] {
        let (status, answer) = env.post(&items, &body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {answer}");
    }
    let (status, _) = env.post(&items, &json!({ "kind": "czech" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // At most 50 items per list.
    for n in 1..50 {
        let (status, body) = env
            .post(
                &items,
                &json!({ "kind": "confusable", "value": format!("test-c{n}") }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{n}: {body}");
    }
    let (status, _) = env
        .post(
            &items,
            &json!({ "kind": "confusable", "value": "test-c50" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = env
        .post(
            &items,
            &json!({ "kind": "confusable", "value": "test-c51" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "too_many_items");
    for (uri, status) in [
        (
            format!("/api/words/{id}/items/czech/nothing"),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("/api/words/{id}/items/bogus/x"),
            StatusCode::NOT_FOUND,
        ),
        (
            "/api/words/99999/items/czech/x".to_owned(),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (got, body) = env.request("DELETE", &uri, Some(&json!({}))).await;
        assert_eq!(got, status, "{uri}: {body}");
    }

    let actions = format!("/api/words/{id}/actions");
    for body in [
        json!({ "action": "explode" }),
        json!({ "action": "suspend", "kind": "understand" }),
        json!({ "action": "reset_card" }),
        json!({ "action": "reset_card", "kind": "everything" }),
    ] {
        let (status, answer) = env.post(&actions, &body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {answer}");
    }
    let (status, _) = env
        .post("/api/words/99999/actions", &json!({ "action": "suspend" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = env
        .post("/api/conflicts/99999", &json!({ "use": "keep" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = env
        .post("/api/conflicts/1", &json!({ "use": "both" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = env
        .post("/api/sentences/99999/hidden", &json!({ "hidden": true }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = env
        .post("/api/sentences/1/hidden", &json!({ "hidden": "yes" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

async fn drain(audio: &Audio) -> usize {
    audio.sync().await.expect("sync");
    let mut worked = 0;
    while audio.work_one().await.expect("work") == Step::Worked {
        worked += 1;
        assert!(worked < 1000, "runaway");
    }
    worked
}

/// R-40 + R-25: an edited sentence (or headword) gets a new clip; the old
/// clip is never offered for the new text, and it is collected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r40_an_edited_sentence_or_headword_gets_a_new_clip_and_never_the_old_one() {
    let (fake, endpoint) = audio_fake::start().await;
    let env = env_with_audio(tempfile::tempdir().expect("tempdir"), START, Some(endpoint));
    let audio = env.audio.clone().expect("audio");
    env.import(BOOK, vec![word("lull", 7, 2)]).await;
    let id = env.word_id("lull").await.get();
    assert_eq!(drain(&audio).await, 3);
    let (_, before) = env.get(&format!("/api/audio/words/{id}")).await;
    let old_url = before["sentences"][0]["url"]
        .as_str()
        .expect("url")
        .to_owned();
    let sentence = before["sentences"][0]["id"].as_i64().expect("id");

    let (status, view) = env
        .request(
            "PATCH",
            &format!("/api/sentences/{sentence}"),
            Some(&json!({
                "en": "A brand new test line with the [[lull]] in it.",
                "cs": "Zcela nový testovací řádek s [[lull]].",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let (_, after) = env.get(&format!("/api/audio/words/{id}")).await;
    assert_eq!(after["sentences"][0]["ready"], false, "{after}");
    assert!(after["sentences"][0]["url"].is_null(), "not the old clip");
    assert_eq!(drain(&audio).await, 1);
    assert!(
        fake.texts()
            .contains(&"A brand new test line with the lull in it.".to_owned())
    );
    let (_, after) = env.get(&format!("/api/audio/words/{id}")).await;
    let new_url = after["sentences"][0]["url"].as_str().expect("url");
    assert_ne!(new_url, old_url);
    let (status, _) = env.get(&old_url).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the old clip was collected");

    // A changed headword is spoken anew too (no `speak` text).
    let old_word = after["word"]["url"].as_str().expect("url").to_owned();
    let (status, _) = env
        .request(
            "PATCH",
            &format!("/api/words/{id}"),
            Some(&json!({ "headword": "lull to sleep" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, after) = env.get(&format!("/api/audio/words/{id}")).await;
    assert!(after["word"]["url"].is_null());
    assert_eq!(drain(&audio).await, 1);
    let (_, after) = env.get(&format!("/api/audio/words/{id}")).await;
    assert_ne!(after["word"]["url"].as_str().expect("url"), old_word);
    // A hidden sentence's clip is no longer wanted.
    let other = after["sentences"][1]["id"].as_i64().expect("id");
    let hidden_url = after["sentences"][1]["url"]
        .as_str()
        .expect("url")
        .to_owned();
    env.post(
        &format!("/api/sentences/{other}/hidden"),
        &json!({ "hidden": true }),
    )
    .await;
    drain(&audio).await;
    let (status, _) = env.get(&hidden_url).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The list stays fast with 5 000 words (design §1 scale): one query per
/// module, joined in memory. The measured times are printed.
#[tokio::test]
async fn r73_the_list_of_5000_words_is_fast() {
    let env = env();
    env.import(BOOK, vec![word("lull", 7, 3)]).await;
    env.exec(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000)
         INSERT INTO word (id, headword, headword_key, meaning, usefulness, created_at, updated_at)
           SELECT 1000 + i, 'test-word-' || i, 'test-word-' || i,
                  'Invented test meaning number ' || i || ' for the speed test.',
                  i % 10 + 1, printf('2026-11-%02dT08:00:00Z', i % 28 + 1),
                  '2026-11-20T08:00:00Z' FROM n;
         INSERT INTO word_source (word_id, source_id, seq, first_pack_import_id, last_pack_import_id)
           SELECT id, 1, id, 1, 1 FROM word WHERE id > 1000;
         INSERT INTO word_item (word_id, kind, value, value_key, position, origin)
           SELECT id, 'czech', 'testovací slovo ' || id, 'testovací slovo ' || id, 0, 'pack'
           FROM word WHERE id > 1000;
         INSERT INTO word_item (word_id, kind, value, value_key, position, origin)
           SELECT id, 'czech', 'jiné ' || id, 'jiné ' || id, 1, 'pack' FROM word WHERE id > 1000;
         WITH RECURSIVE k(j) AS (SELECT 1 UNION ALL SELECT j + 1 FROM k WHERE j < 3)
         INSERT INTO sentence (word_id, en_marked, cs_marked, en_key, origin, position,
                               created_at, updated_at)
           SELECT w.id, 'Line ' || k.j || ' with the [[' || w.headword || ']] in it.',
                  'Řádek ' || k.j || ' se [[slovem]].', 'line ' || k.j || ' ' || w.id, 'example',
                  k.j, w.created_at, w.created_at FROM word w, k WHERE w.id > 1000;
         INSERT INTO card (word_id, kind, state, stability, difficulty, due_day,
                           last_review_day, reps, created_at)
           SELECT id,
                  'understand',
                  CASE WHEN id % 3 = 0 THEN 'new' ELSE 'review' END,
                  CASE WHEN id % 3 = 0 THEN NULL ELSE (id % 40) + 0.5 END,
                  CASE WHEN id % 3 = 0 THEN NULL ELSE 5.0 END,
                  CASE WHEN id % 3 = 0 THEN NULL ELSE date('2026-11-20', '+' || (id % 30) || ' days') END,
                  CASE WHEN id % 3 = 0 THEN NULL ELSE '2026-11-15' END,
                  CASE WHEN id % 3 = 0 THEN 0 ELSE 1 END,
                  created_at
           FROM word WHERE id > 1000;
         INSERT INTO card (word_id, kind, state, due_day, created_at)
           SELECT id, 'produce', 'new', '2026-11-21', created_at FROM word
           WHERE id > 1000 AND id % 2 = 0;",
    );
    assert_eq!(env.int("SELECT COUNT(*) FROM word"), 5001);

    let mut report = Vec::new();
    for query in [
        "",
        "?sort=due",
        "?sort=status&status=familiar",
        "?q=testovac%C3%AD%20slovo%20499",
        "?min_u=5&max_u=8&sort=usefulness",
        "?source=1",
    ] {
        // Best of three: the first call also warms SQLite's page cache.
        let mut best = Duration::MAX;
        let mut rows = Value::Null;
        for _ in 0..3 {
            let started = Instant::now();
            rows = list(&env, query).await;
            best = best.min(started.elapsed());
        }
        let count = rows.as_array().expect("rows").len();
        report.push(format!("GET /api/words{query}: {count} rows in {best:?}"));
        assert!(
            best < Duration::from_secs(2),
            "GET /api/words{query} took {best:?}"
        );
        if query.is_empty() {
            assert_eq!(count, 5001);
            assert_eq!(rows[0]["headword"], "lull");
            assert_eq!(
                rows[1]["czech"],
                json!(["testovací slovo 1001", "jiné 1001"])
            );
        }
    }
    eprintln!("5000-word list timings:\n  {}", report.join("\n  "));
    // The detail of one word among them is fast too.
    let started = Instant::now();
    let (status, _) = env.get("/api/words/3000").await;
    assert_eq!(status, StatusCode::OK);
    eprintln!("  GET /api/words/3000 in {:?}", started.elapsed());
}
