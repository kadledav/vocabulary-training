//! Contract tests of the session engine and its API (R-51, R-52, R-54,
//! R-59, R-60, R-61; design §6.6, §6.7, §8; D-1, D-3, D-11). All words are
//! synthetic.

mod common;

use axum::http::StatusCode;
use common::{Env, env, env_in, word};
use serde_json::{Value, json};

/// raw, applied, suggested, typed, match, hint, duration, session, has sentence.
type StoredReview = (i64, i64, i64, String, String, i64, i64, String, i64);

// ── helpers ─────────────────────────────────────────────────────────────────

async fn start(env: &Env, body: Value) -> Value {
    let (status, result) = env.post("/api/sessions", &body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result
}

async fn start_block(env: &Env) -> Value {
    let result = start(env, json!({ "kind": "start" })).await;
    assert!(!result["session"].is_null(), "no block: {result}");
    result["session"].clone()
}

async fn continue_block(env: &Env) -> Value {
    start(env, json!({ "kind": "continue" })).await["session"].clone()
}

fn sid(session: &Value) -> String {
    session["id"].as_str().expect("id").to_owned()
}

fn next_item(session: &Value) -> Value {
    session["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["answered"].is_null())
        .cloned()
        .unwrap_or_else(|| panic!("no unanswered item in {session}"))
}

fn headwords(session: &Value) -> Vec<String> {
    session["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["word"]["headword"].as_str().expect("headword").to_owned())
        .collect()
}

static CLIENT_NO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn client_id() -> String {
    let n = CLIENT_NO.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("00000000-0000-4000-8000-{n:012}")
}

async fn post_review(env: &Env, session: &str, item: i64, raw: u8) -> (StatusCode, Value) {
    env.post(
        &format!("/api/sessions/{session}/reviews"),
        &json!({
            "client_review_id": client_id(),
            "item_id": item,
            "raw_grade": raw,
            "hint_used": false,
            "duration_ms": 1000,
            "client_ts": "2026-11-20T10:00:00+01:00",
        }),
    )
    .await
}

async fn rate_next(env: &Env, session: &Value, raw: u8) -> Value {
    let item = next_item(session);
    let (status, result) = post_review(
        env,
        &sid(session),
        item["item_id"].as_i64().expect("id"),
        raw,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result
}

async fn rate_all(env: &Env, mut session: Value, raw: u8) -> Value {
    while session["remaining"].as_u64().expect("remaining") > 0 {
        session = rate_next(env, &session, raw).await["session"].clone();
    }
    session
}

async fn finish(env: &Env, session: &Value) {
    let (status, body) = env
        .post(
            &format!("/api/sessions/{}/finish", sid(session)),
            &json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn undo(env: &Env, session: &Value) -> (StatusCode, Value) {
    env.post(&format!("/api/sessions/{}/undo", sid(session)), &json!({}))
        .await
}

async fn today(env: &Env, scope: Option<i64>) -> Value {
    let uri = match scope {
        Some(s) => format!("/api/today?scope={s}"),
        None => "/api/today".to_owned(),
    };
    let (status, view) = env.get(&uri).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view
}

async fn settings(env: &Env, patch: Value) -> (StatusCode, Value) {
    env.request("PATCH", "/api/settings", Some(&patch)).await
}

fn make_due(env: &Env, headword_like: &str, due_day: &str) {
    env.exec(&format!(
        "UPDATE card SET state = 'review', stability = 5.0, difficulty = 5.0, due_day = '{due_day}', \
         last_review_day = '2026-11-10', introduced_day = '2026-11-01', reps = 2, format = 'sentence' \
         WHERE word_id IN (SELECT id FROM word WHERE headword LIKE '{headword_like}')"
    ));
}

async fn source_id(env: &Env, slug: &str) -> i64 {
    let (_, sources) = env.get("/api/sources").await;
    sources
        .as_array()
        .expect("sources")
        .iter()
        .find(|s| s["slug"] == slug)
        .expect("source")["id"]
        .as_i64()
        .expect("id")
}

// ── R-54 / R-51: new words per study day ────────────────────────────────────

#[tokio::test]
async fn r54_new_words_per_day_hold_across_blocks_restarts_and_days() {
    let env = env();
    let lows: Vec<Value> = (1..=6).map(|i| word(&format!("l{i}"), 5, 3)).collect();
    let highs: Vec<Value> = (1..=8).map(|i| word(&format!("h{i}"), 9, 3)).collect();
    env.import("test-novel", lows).await;
    env.import("test-novel", highs).await;
    assert_eq!(
        settings(&env, json!({ "new_per_day": 8, "block_size": 5 }))
            .await
            .0,
        StatusCode::OK
    );

    // Day 1: most useful first, 8 in all although blocks hold 5.
    let first = start_block(&env).await;
    assert_eq!(headwords(&first), ["h1", "h2", "h3", "h4", "h5"]);
    finish(&env, &rate_all(&env, first, 2).await).await;
    let second = continue_block(&env).await;
    assert_eq!(headwords(&second), ["h6", "h7", "h8"]);
    finish(&env, &rate_all(&env, second, 2).await).await;
    assert!(continue_block(&env).await.is_null());

    // A restart does not reset the day's count.
    let Env { dir, .. } = env;
    let env = env_in(dir, "2026-11-20T15:00:00Z");
    assert!(continue_block(&env).await.is_null());
    assert_eq!(today(&env, None).await["new_words"]["introduced_today"], 8);

    // Next day: the rest of the words, still within the limit (the first
    // day's cards are due again and mixed in).
    env.set_day("2026-11-21");
    assert_eq!(today(&env, None).await["new_words"]["introduced_today"], 0);
    let mut introduced = Vec::new();
    let mut block = start(&env, json!({ "kind": "start" })).await["session"].clone();
    while !block.is_null() {
        for item in block["items"].as_array().expect("items") {
            if item["origin"] == "new" {
                introduced.push(item["word"]["headword"].as_str().expect("h").to_owned());
            }
        }
        finish(&env, &rate_all(&env, block, 2).await).await;
        block = continue_block(&env).await;
    }
    introduced.sort();
    assert_eq!(introduced, ["l1", "l2", "l3", "l4", "l5", "l6"]);
}

#[tokio::test]
async fn r54_zero_new_words_per_day_introduces_nothing() {
    let env = env();
    env.import("test-novel", vec![word("a1", 9, 3)]).await;
    assert_eq!(
        settings(&env, json!({ "new_per_day": 0 })).await.0,
        StatusCode::OK
    );
    let result = start(&env, json!({ "kind": "start" })).await;
    assert!(result["session"].is_null(), "{result}");
    assert_eq!(today(&env, None).await["new_words"]["quota"], 0);
}

#[tokio::test]
async fn r51_low_priority_words_are_never_introduced_automatically() {
    let env = env();
    let mut minor = word("minor", 10, 3);
    minor["tag"] = json!({ "kind": "low-priority" });
    env.import(
        "test-novel",
        vec![minor, word("n1", 3, 3), word("n2", 3, 3), word("n3", 3, 3)],
    )
    .await;
    assert_eq!(
        settings(&env, json!({ "new_per_day": 1 })).await.0,
        StatusCode::OK
    );
    let mut seen = Vec::new();
    for offset in 0..6 {
        env.set_day(&format!("2026-11-{}", 20 + offset));
        let view = today(&env, None).await;
        assert!(view["new_available"].as_u64().expect("n") <= 3);
        let result = start(&env, json!({ "kind": "start" })).await;
        if let Some(session) = result["session"].as_object() {
            let session = Value::Object(session.clone());
            seen.extend(headwords(&session));
            finish(&env, &rate_all(&env, session, 2).await).await;
        }
    }
    assert!(!seen.contains(&"minor".to_owned()), "{seen:?}");
    assert!(seen.contains(&"n1".to_owned()));
    assert_eq!(
        env.int("SELECT COUNT(*) FROM card WHERE state = 'new' AND word_id IN (SELECT id FROM word WHERE headword = 'minor')"),
        1
    );
}

#[tokio::test]
async fn r51_ties_in_usefulness_follow_source_order() {
    let env = env();
    env.import(
        "test-novel",
        vec![word("zed", 7, 3), word("amy", 7, 3), word("mid", 7, 3)],
    )
    .await;
    assert_eq!(headwords(&start_block(&env).await), ["zed", "amy", "mid"]);
}

// ── R-59: scope by source ───────────────────────────────────────────────────

#[tokio::test]
async fn r59_a_scoped_block_never_contains_words_of_other_sources() {
    let env = env();
    env.import(
        "test-novel",
        vec![word("a1", 5, 3), word("a2", 5, 3), word("both", 5, 3)],
    )
    .await;
    env.import(
        "test-general",
        vec![word("b1", 5, 3), word("b2", 5, 3), word("both", 5, 3)],
    )
    .await;
    make_due(&env, "a1", "2026-11-19");
    make_due(&env, "b1", "2026-11-19");
    let a = source_id(&env, "test-novel").await;
    let b = source_id(&env, "test-general").await;

    // Counts in the Today view follow the scope; a shared word counts in both.
    let view_a = today(&env, Some(a)).await;
    let view_b = today(&env, Some(b)).await;
    assert_eq!(view_a["due_now"], 1);
    assert_eq!(view_a["new_available"], 2);
    assert_eq!(view_b["due_now"], 1);
    assert_eq!(view_b["new_available"], 2);
    assert_eq!(today(&env, None).await["new_available"], 3);

    let block =
        start(&env, json!({ "kind": "start", "scope_source_id": a })).await["session"].clone();
    let mut words = headwords(&block);
    words.sort();
    assert_eq!(words, ["a1", "a2", "both"]);
    finish(&env, &rate_all(&env, block, 2).await).await;

    // The shared word was introduced under A; B offers only its own rest.
    let block_b =
        start(&env, json!({ "kind": "continue", "scope_source_id": b })).await["session"].clone();
    let mut words = headwords(&block_b);
    words.sort();
    assert_eq!(words, ["b1", "b2"]);
    assert!(!headwords(&block_b).contains(&"a1".to_owned()));
}

#[tokio::test]
async fn r59_the_daily_new_word_limit_is_global_across_scopes() {
    let env = env();
    env.import("test-novel", vec![word("a1", 5, 3), word("a2", 5, 3)])
        .await;
    env.import("test-general", vec![word("b1", 5, 3), word("b2", 5, 3)])
        .await;
    assert_eq!(
        settings(&env, json!({ "new_per_day": 2 })).await.0,
        StatusCode::OK
    );
    let a = source_id(&env, "test-novel").await;
    let b = source_id(&env, "test-general").await;
    let block =
        start(&env, json!({ "kind": "start", "scope_source_id": a })).await["session"].clone();
    assert_eq!(block["total"], 2);
    finish(&env, &rate_all(&env, block, 2).await).await;
    let result = start(&env, json!({ "kind": "continue", "scope_source_id": b })).await;
    assert!(result["session"].is_null(), "{result}");
    assert_eq!(today(&env, Some(b)).await["new_words"]["left_today"], 0);
}

#[tokio::test]
async fn r59_unknown_scope_values_are_rejected() {
    let env = env();
    let (status, _) = env.get("/api/today?scope=abc").await;
    assert!(status.is_client_error());
    let (status, _) = env.get("/api/today?scope=-1").await;
    assert!(status.is_client_error());
}

// ── R-54: study day rollover at 04:00 local ─────────────────────────────────

#[tokio::test]
async fn r54_the_study_day_rolls_over_exactly_at_four_local() {
    // Prague in November is UTC+1: 03:59:59 local = 02:59:59Z.
    let dir = tempfile::tempdir().expect("tempdir");
    let env = env_in(dir, "2026-11-21T02:59:58Z");
    let mut words: Vec<Value> = (1..=6).map(|i| word(&format!("n{i}"), 5, 3)).collect();
    words.push(word("tomorrow", 5, 3));
    env.import("test-novel", words).await;
    make_due(&env, "tomorrow", "2026-11-21");
    assert_eq!(
        settings(&env, json!({ "new_per_day": 15, "block_size": 5 }))
            .await
            .0,
        StatusCode::OK
    );

    env.set_time("2026-11-21T02:59:59Z");
    let view = today(&env, None).await;
    assert_eq!(view["study_day"], "2026-11-20");
    assert_eq!(view["due_now"], 0);
    let block = start_block(&env).await;
    assert_eq!(block["study_day"], "2026-11-20");
    let block = rate_all(&env, block, 2).await;
    assert!(
        today(&env, None).await["new_words"]["introduced_today"]
            .as_u64()
            .expect("n")
            >= 5
    );

    env.set_time("2026-11-21T03:00:00Z");
    let view = today(&env, None).await;
    assert_eq!(view["study_day"], "2026-11-21");
    assert_eq!(view["due_now"], 1);
    assert_eq!(view["new_words"]["introduced_today"], 0);
    assert!(view["active_session"].is_null());
    let (status, _) = env.get("/api/sessions/current").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Unfinished yesterday block (all answered here) can no longer be finished
    // as the active one but a new block starts for the new day.
    let _ = block;
    let next = start_block(&env).await;
    assert_eq!(next["study_day"], "2026-11-21");
    assert_eq!(next["block_no"], 1);
}

// ── R-54: undo ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn r54_undo_of_the_first_rating_of_the_day_restores_a_due_card_exactly() {
    let env = env();
    env.import("test-novel", vec![word("alpha", 5, 3)]).await;
    make_due(&env, "alpha", "2026-11-18");
    let before = env.rows("card", "1");
    let block = start_block(&env).await;
    let done = rate_next(&env, &block, 5).await;
    assert_ne!(env.rows("card", "1"), before);
    let (status, body) = undo(&env, &done["session"]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(env.rows("card", "1"), before);
    assert_eq!(today(&env, None).await["due_now"], 1);
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE undone = 0"),
        0
    );
    // The block is intact and can be answered again.
    assert_eq!(body["session"]["remaining"], 1);
    rate_next(&env, &body["session"], 2).await;
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE undone = 0"),
        1
    );
}

#[tokio::test]
async fn r54_rating_again_after_undoing_twice_works_and_counts_once() {
    let env = env();
    env.import(
        "test-novel",
        vec![word("a1", 9, 3), word("a2", 8, 3), word("a3", 7, 3)],
    )
    .await;
    let block = start_block(&env).await;
    let r1 = rate_next(&env, &block, 2).await;
    let r2 = rate_next(&env, &r1["session"], 3).await;
    let (status, u1) = undo(&env, &r2["session"]).await;
    assert_eq!(status, StatusCode::OK, "{u1}");
    let (status, u2) = undo(&env, &u1["session"]).await;
    assert_eq!(status, StatusCode::OK, "{u2}");
    assert_eq!(u2["session"]["remaining"], 3);
    assert_eq!(today(&env, None).await["new_words"]["introduced_today"], 0);
    let again = rate_next(&env, &u2["session"], 3).await;
    assert_eq!(today(&env, None).await["new_words"]["introduced_today"], 1);
    let done = rate_all(&env, again["session"].clone(), 2).await;
    assert_eq!(done["remaining"], 0);
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE undone = 0"),
        3
    );
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE undone = 1"),
        2
    );
    // Every card was introduced exactly once.
    assert_eq!(env.int("SELECT SUM(reps) FROM card"), 3);
}

#[tokio::test]
async fn r54_undo_after_a_requeue_then_rating_again_leaves_one_repeat() {
    let env = env();
    env.import("test-novel", vec![word("a1", 9, 3), word("a2", 8, 3)])
        .await;
    let block = start_block(&env).await;
    let failed = rate_next(&env, &block, 5).await;
    assert_eq!(failed["session"]["total"], 3);
    let (status, back) = undo(&env, &failed["session"]).await;
    assert_eq!(status, StatusCode::OK, "{back}");
    assert_eq!(back["session"]["total"], 2);
    let failed = rate_next(&env, &back["session"], 4).await;
    assert_eq!(failed["session"]["total"], 3, "one repeat, not two");
    let repeats = failed["session"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|i| i["origin"] == "repeat")
        .count();
    assert_eq!(repeats, 1);
}

#[tokio::test]
async fn r54_undo_is_refused_for_a_finished_block() {
    let env = env();
    env.import("test-novel", vec![word("a1", 9, 3)]).await;
    let block = start_block(&env).await;
    let done = rate_all(&env, block, 2).await;
    finish(&env, &done).await;
    let (status, body) = undo(&env, &done).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE undone = 1"),
        0
    );
}

// ── R-60 / R-61 through the API ─────────────────────────────────────────────

#[tokio::test]
async fn r61_the_api_stores_raw_grade_applied_rating_and_answer_details() {
    let env = env();
    let words: Vec<Value> = (1..=5).map(|i| word(&format!("w{i}"), 9, 3)).collect();
    env.import("test-novel", words).await;
    assert_eq!(
        settings(&env, json!({ "block_size": 5 })).await.0,
        StatusCode::OK
    );
    let mut block = start_block(&env).await;
    // Rate in queue order with raw 1..5; repeats of 4 and 5 are left aside.
    for raw in 1..=5u8 {
        let item = next_item(&block);
        let (status, result) = env
            .post(
                &format!("/api/sessions/{}/reviews", sid(&block)),
                &json!({
                    "client_review_id": client_id(),
                    "item_id": item["item_id"],
                    "raw_grade": raw,
                    "suggested_grade": 4,
                    "typed_answer": "abc",
                    "answer_match": "none",
                    "hint_used": true,
                    "duration_ms": 777,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        block = result["session"].clone();
    }
    let conn = env.sql();
    let mut stmt = conn
        .prepare(
            "SELECT raw_grade, fsrs_rating, suggested_grade, typed_answer, answer_match, hint_used, \
             duration_ms, session_id, sentence_id IS NOT NULL FROM review_log ORDER BY id",
        )
        .expect("select");
    let rows: Vec<StoredReview> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    let applied: Vec<(i64, i64)> = rows.iter().map(|r| (r.0, r.1)).collect();
    assert_eq!(applied, [(1, 3), (2, 3), (3, 2), (4, 1), (5, 1)]);
    for r in &rows {
        assert_eq!(
            (r.2, r.3.as_str(), r.4.as_str(), r.5, r.6, r.7.as_str(), r.8),
            (4, "abc", "none", 1, 777, sid(&block).as_str(), 1)
        );
    }
}

#[tokio::test]
async fn r60_a_three_is_a_pass_and_four_five_are_repeated() {
    let env = env();
    env.import(
        "test-novel",
        vec![word("a1", 9, 3), word("a2", 8, 3), word("a3", 7, 3)],
    )
    .await;
    let block = start_block(&env).await;
    let r = rate_next(&env, &block, 3).await;
    assert!(r["repeat_item_id"].is_null());
    let r = rate_next(&env, &r["session"], 4).await;
    assert!(!r["repeat_item_id"].is_null());
    let r = rate_next(&env, &r["session"], 5).await;
    assert!(!r["repeat_item_id"].is_null());
}

// ── Today and served kinds (accepted deviation) ─────────────────────────────

#[tokio::test]
async fn r52_today_and_blocks_leave_out_card_kinds_that_are_not_served_yet() {
    let env = env();
    env.import("test-novel", vec![word("alpha", 9, 3)]).await;
    make_due(&env, "alpha", "2026-11-25");
    env.exec(
        "INSERT INTO card (word_id, kind, format, state, stability, difficulty, due_day, \
         last_review_day, introduced_day, reps, created_at) \
         SELECT id, 'produce', 'cloze', 'review', 5.0, 5.0, '2026-11-19', '2026-11-10', \
         '2026-11-01', 2, '2026-11-01T00:00:00Z' FROM word",
    );
    assert_eq!(today(&env, None).await["due_now"], 0);
    let result = start(&env, json!({ "kind": "start" })).await;
    assert!(result["session"].is_null(), "{result}");
}

// ── sentence rotation (design §6.7 step 6, R-53) ────────────────────────────

#[tokio::test]
async fn r53_each_review_of_a_word_shows_a_different_sentence() {
    let env = env();
    env.import("test-novel", vec![word("alpha", 9, 3)]).await;
    let block = start_block(&env).await;
    let first = next_item(&block)["sentence"]["id"].clone();
    let failed = rate_next(&env, &block, 5).await;
    let repeat = next_item(&failed["session"])["sentence"]["id"].clone();
    assert_ne!(first, repeat);
    let passed = rate_next(&env, &failed["session"], 2).await;
    finish(&env, &passed["session"]).await;
    let due: String = env
        .sql()
        .query_row("SELECT due_day FROM card", [], |r| r.get(0))
        .expect("due");
    env.set_day(&due);
    let next = start_block(&env).await;
    let third = next_item(&next)["sentence"]["id"].clone();
    assert_ne!(third, first);
    assert_ne!(third, repeat, "the unseen sentence comes first");
}

// ── API validation ──────────────────────────────────────────────────────────

#[tokio::test]
async fn r54_start_requests_are_validated() {
    let env = env();
    for body in [
        json!({}),
        json!({ "kind": "begin" }),
        json!({ "kind": 1 }),
        json!({ "kind": "start", "add_new_anyway": "yes" }),
        json!({ "kind": "start", "scope_source_id": "x" }),
        json!({ "kind": "start", "surprise": true }),
    ] {
        let (status, response) = env.post("/api/sessions", &body).await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "{body} -> {status} {response}"
        );
    }
    let (status, _) = env.request("POST", "/api/sessions", None).await;
    assert!(status.is_client_error());
}

#[tokio::test]
async fn r60_review_requests_are_validated() {
    let env = env();
    env.import("test-novel", vec![word("a1", 9, 3)]).await;
    let block = start_block(&env).await;
    let item = next_item(&block)["item_id"].clone();
    let uri = format!("/api/sessions/{}/reviews", sid(&block));
    for body in [
        json!({ "client_review_id": client_id(), "item_id": item, "raw_grade": 0 }),
        json!({ "client_review_id": client_id(), "item_id": item, "raw_grade": 6 }),
        json!({ "client_review_id": client_id(), "item_id": item, "raw_grade": -1 }),
        json!({ "client_review_id": client_id(), "item_id": item, "raw_grade": 2.5 }),
        json!({ "client_review_id": client_id(), "item_id": item, "raw_grade": "2" }),
        json!({ "client_review_id": client_id(), "item_id": item }),
        json!({ "item_id": item, "raw_grade": 2 }),
        json!({ "client_review_id": client_id(), "raw_grade": 2 }),
        json!({ "client_review_id": client_id(), "item_id": item, "raw_grade": 2, "suggested_grade": 9 }),
        json!({ "client_review_id": client_id(), "item_id": item, "raw_grade": 2, "answer_match": "maybe" }),
        json!({ "client_review_id": client_id(), "item_id": item, "raw_grade": 2, "bogus": 1 }),
    ] {
        let (status, response) = env.post(&uri, &body).await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "{body} -> {status} {response}"
        );
    }
    assert_eq!(env.int("SELECT COUNT(*) FROM review_log"), 0);
    // Errors have the documented shape.
    let (status, body) = env
        .post(
            &uri,
            &json!({ "client_review_id": client_id(), "item_id": item, "raw_grade": 7 }),
        )
        .await;
    assert!(status.is_client_error());
    assert!(body["error"]["code"].is_string(), "{body}");
    assert!(body["error"]["message"].is_string(), "{body}");
}

#[tokio::test]
async fn r60_client_review_id_must_be_a_uuid() {
    let env = env();
    env.import("test-novel", vec![word("a1", 9, 3)]).await;
    let block = start_block(&env).await;
    let item = next_item(&block)["item_id"].clone();
    let (status, body) = env
        .post(
            &format!("/api/sessions/{}/reviews", sid(&block)),
            &json!({ "client_review_id": "nope", "item_id": item, "raw_grade": 2 }),
        )
        .await;
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "{status} {body}"
    );
}

#[tokio::test]
async fn r54_settings_ranges_are_inclusive() {
    let env = env();
    for (patch, ok) in [
        (json!({ "new_per_day": 0 }), true),
        (json!({ "new_per_day": 15 }), true),
        (json!({ "new_per_day": 16 }), false),
        (json!({ "new_per_day": -1 }), false),
        (json!({ "new_per_day": 2.5 }), false),
        (json!({ "block_size": 5 }), true),
        (json!({ "block_size": 50 }), true),
        (json!({ "block_size": 4 }), false),
        (json!({ "block_size": 51 }), false),
        (json!({ "block_size": "20" }), false),
    ] {
        let (status, body) = settings(&env, patch.clone()).await;
        if ok {
            assert_eq!(status, StatusCode::OK, "{patch}: {body}");
        } else {
            assert!(status.is_client_error(), "{patch} -> {status} {body}");
        }
    }
    // A rejected patch changes nothing.
    let (_, s) = env.get("/api/settings").await;
    assert_eq!(
        (s["new_per_day"].clone(), s["block_size"].clone()),
        (json!(15), json!(50))
    );
    let before = s.clone();
    let (status, _) = settings(&env, json!({ "new_per_day": 3, "block_size": 99 })).await;
    assert!(status.is_client_error());
    assert_eq!(env.get("/api/settings").await.1, before);
}

#[tokio::test]
async fn r54_unknown_sessions_and_malformed_ids_are_not_found() {
    let env = env();
    for uri in [
        "/api/sessions/ffffffffffffffffffffffffffffffff/undo",
        "/api/sessions/ffffffffffffffffffffffffffffffff/finish",
        "/api/sessions/not-a-session/undo",
    ] {
        let (status, body) = env.post(uri, &json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {body}");
        assert!(body["error"]["code"].is_string());
    }
    let (status, _) = env.get("/api/sessions/current").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
