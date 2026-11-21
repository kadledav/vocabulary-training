//! Practice blocks through the HTTP API with a fake clock (design §6.6,
//! §6.7, §8; R-52, R-54, R-59, R-60, R-61). All words are synthetic.

mod common;

use axum::http::StatusCode;
use common::{Env, env, env_in, word};
use serde_json::{Value, json};

// ── helpers ─────────────────────────────────────────────────────────────────

async fn start(env: &Env, body: Value) -> Value {
    let (status, result) = env.post("/api/sessions", &body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result
}

/// Start a block in all sources; panics when there is nothing to practise.
async fn start_block(env: &Env) -> Value {
    let result = start(env, json!({ "kind": "start" })).await;
    assert!(!result["session"].is_null(), "no block: {result}");
    result["session"].clone()
}

fn session_id(session: &Value) -> String {
    session["id"].as_str().expect("id").to_owned()
}

/// The next unanswered item.
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

fn origins(session: &Value) -> String {
    session["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| match i["origin"].as_str().expect("origin") {
            "due" => 'D',
            "new" => 'N',
            _ => 'R',
        })
        .collect()
}

static CLIENT_NO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn client_id() -> String {
    let n = CLIENT_NO.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("00000000-0000-4000-8000-{n:012}")
}

async fn post_review(
    env: &Env,
    session: &str,
    item: i64,
    raw: u8,
    id: &str,
) -> (StatusCode, Value) {
    env.post(
        &format!("/api/sessions/{session}/reviews"),
        &json!({
            "client_review_id": id,
            "item_id": item,
            "raw_grade": raw,
            "hint_used": false,
            "duration_ms": 2500,
            "client_ts": "2026-11-20T10:00:00+01:00",
        }),
    )
    .await
}

/// Rate the next item of `session` with `raw`; returns the rate result.
async fn rate_next(env: &Env, session: &Value, raw: u8) -> Value {
    let item = next_item(session);
    let (status, result) = post_review(
        env,
        &session_id(session),
        item["item_id"].as_i64().expect("item id"),
        raw,
        &client_id(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result
}

/// Rate every remaining item with `raw`.
async fn rate_all(env: &Env, mut session: Value, raw: u8) -> Value {
    while session["remaining"].as_u64().expect("remaining") > 0 {
        session = rate_next(env, &session, raw).await["session"].clone();
    }
    session
}

async fn finish(env: &Env, session: &Value) -> Value {
    let (status, summary) = env
        .post(
            &format!("/api/sessions/{}/finish", session_id(session)),
            &json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{summary}");
    summary
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

async fn set_settings(env: &Env, patch: Value) {
    let (status, body) = env.request("PATCH", "/api/settings", Some(&patch)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Seed `n`-word cards as reviewed long ago and due (state normally built
/// up by many past reviews).
fn make_due(env: &Env, headword_like: &str, due_day: &str) {
    env.exec(&format!(
        "UPDATE card SET state = 'review', stability = 5.0, difficulty = 5.0, due_day = '{due_day}', \
         last_review_day = '2026-11-10', introduced_day = '2026-11-01', reps = 2, format = 'sentence' \
         WHERE word_id IN (SELECT id FROM word WHERE headword LIKE '{headword_like}')"
    ));
}

// ── the main flow (§17 M3 "done when") ──────────────────────────────────────

#[tokio::test]
async fn r54_import_start_rate_and_undo_restores_everything() {
    let env = env();
    let mut keen = word("keen", 8, 3);
    keen["spelling"] = json!(true);
    env.import("test-novel", vec![keen]).await;

    // Day 1: the new word is tested cold (R-52).
    let session = start_block(&env).await;
    assert_eq!(session["total"], 1);
    let item = next_item(&session);
    assert_eq!(item["origin"], "new");
    assert_eq!(item["card"]["kind"], "understand");
    assert_eq!(item["card"]["is_new"], true);
    assert_eq!(item["card"]["format"], "sentence");
    assert!(item["sentence"]["en"]["target"] == "keen");
    assert_eq!(item["other_sentences"].as_array().expect("others").len(), 2);
    assert!(
        item["accept"]
            .as_array()
            .expect("accept")
            .contains(&json!("keen"))
    );
    let result = rate_next(&env, &session, 2).await;
    assert_eq!(result["duplicate"], false);
    assert_eq!(result["review"]["raw_grade"], 2);
    assert_eq!(result["review"]["fsrs_rating"], 3);
    assert_eq!(result["review"]["card"]["state"], "review");
    assert_eq!(result["review"]["card"]["introduced_day"], "2026-11-20");
    let expected_due = {
        let next = fsrs::FSRS::default()
            .next_states(None, 0.93, 0)
            .expect("fsrs")
            .good;
        common::day("2026-11-20").add_days(next.interval.round() as i32)
    };
    assert_eq!(
        result["review"]["card"]["due_day"],
        json!(expected_due.to_string())
    );
    assert_eq!(result["session"]["remaining"], 0);
    let summary = finish(&env, &result["session"]).await;
    assert_eq!(summary["ratings"], json!([0, 1, 0, 0, 0]));
    assert_eq!(summary["new_words"], 1);

    // Following due days until a rating unlocks Produce/Spelling.
    for _ in 0..12 {
        let due = env
            .sql()
            .query_row(
                "SELECT due_day FROM card WHERE kind = 'understand'",
                [],
                |r| r.get::<_, String>(0),
            )
            .expect("due");
        env.set_day(&due);
        let session = start_block(&env).await;
        let item = next_item(&session);
        assert_eq!(item["origin"], "due");
        assert_eq!(item["card"]["is_new"], false);
        let sid = session_id(&session);
        let cards_before = env.rows("card", "1");
        let log_before = env.rows("review_log", "undone = 0");
        let items_before = env.rows("session_item", &format!("session_id = '{sid}'"));
        let result = rate_next(&env, &session, 2).await;
        let unlocked = result["review"]["unlocked"].as_array().expect("unlocked");
        if unlocked.is_empty() {
            finish(&env, &result["session"]).await;
            continue;
        }
        assert!(env.int("SELECT COUNT(*) FROM card") > 1);
        let (status, undo) = env
            .post(&format!("/api/sessions/{sid}/undo"), &json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{undo}");
        assert_eq!(
            undo["removed_cards"].as_array().expect("removed").len(),
            unlocked.len()
        );
        assert_eq!(
            env.rows("card", "1"),
            cards_before,
            "cards restored exactly"
        );
        assert_eq!(env.rows("review_log", "undone = 0"), log_before);
        assert_eq!(
            env.rows("session_item", &format!("session_id = '{sid}'")),
            items_before
        );
        assert_eq!(undo["session"]["remaining"], 1);
        assert_eq!(undo["session"]["can_undo"], false);
        // Rating again works and unlocks again.
        let again = rate_next(&env, &undo["session"], 2).await;
        assert_eq!(
            again["review"]["unlocked"]
                .as_array()
                .expect("unlocked")
                .len(),
            unlocked.len()
        );
        return;
    }
    panic!("nothing was unlocked");
}

// ── idempotency ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn r54_review_posts_are_idempotent_by_client_review_id() {
    let env = env();
    env.import("test-novel", vec![word("alpha", 5, 3), word("beta", 5, 3)])
        .await;
    let session = start_block(&env).await;
    let sid = session_id(&session);
    let first = next_item(&session)["item_id"].as_i64().expect("id");
    let uuid = client_id();
    let (s1, r1) = post_review(&env, &sid, first, 2, &uuid).await;
    let (s2, r2) = post_review(&env, &sid, first, 2, &uuid).await;
    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK));
    assert_eq!(r1["duplicate"], false);
    assert_eq!(r2["duplicate"], true);
    assert_eq!(r1["review"]["review_id"], r2["review"]["review_id"]);
    assert_eq!(r2["review"]["card"], r1["review"]["card"]);
    assert_eq!(env.int("SELECT COUNT(*) FROM review_log"), 1);
    assert_eq!(r2["session"]["remaining"], 1);

    // The same id for another item is a conflict, not a second review.
    let second = next_item(&r2["session"])["item_id"].as_i64().expect("id");
    let (status, body) = post_review(&env, &sid, second, 3, &uuid).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "review_id_reused");

    // After undo the old id cannot silently re-apply the rating.
    let (status, _) = env
        .post(&format!("/api/sessions/{sid}/undo"), &json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post_review(&env, &sid, first, 2, &uuid).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "review_undone");
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE undone = 0"),
        0
    );
}

// ── resume (design §6.7 step 8) ─────────────────────────────────────────────

#[tokio::test]
async fn r54_block_resumes_after_a_restart() {
    let first = env();
    first
        .import(
            "test-novel",
            vec![word("alpha", 5, 3), word("beta", 5, 3), word("gamma", 5, 3)],
        )
        .await;
    let session = start_block(&first).await;
    let sid = session_id(&session);
    rate_next(&first, &session, 2).await;
    let Env { dir, .. } = first;

    // A new process on the same database.
    let second = env_in(dir, "2026-11-20T15:00:00Z");
    let (status, current) = second.get("/api/sessions/current").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session_id(&current), sid);
    assert_eq!(current["total"], 3);
    assert_eq!(current["remaining"], 2);
    assert_eq!(current["can_undo"], true);
    assert!(!current["items"][0]["answered"].is_null());
    // Start resumes the same block instead of building another one.
    let resumed = start(&second, json!({ "kind": "start" })).await;
    assert_eq!(resumed["resumed"], true);
    assert_eq!(session_id(&resumed["session"]), sid);
    let today = today(&second, None).await;
    assert_eq!(today["active_session"]["remaining"], 2);
    let done = rate_all(&second, current, 2).await;
    assert_eq!(done["remaining"], 0);
}

// ── day rollover (design §2, §6.7 step 8) ───────────────────────────────────

#[tokio::test]
async fn r54_block_expires_at_four_on_a_daylight_saving_change_day() {
    // 2026-10-25: Prague falls back from CEST to CET at 03:00 local
    // (01:00Z); the study day still rolls over at 04:00 CET = 03:00Z.
    let dir = tempfile::tempdir().expect("tempdir");
    let env = env_in(dir, "2026-10-25T02:30:00Z");
    env.import("test-novel", vec![word("alpha", 5, 3), word("beta", 5, 3)])
        .await;
    let session = start_block(&env).await;
    assert_eq!(session["study_day"], "2026-10-24");
    let sid = session_id(&session);
    env.set_time("2026-10-25T02:59:59Z");
    let (status, _) = env.get("/api/sessions/current").await;
    assert_eq!(status, StatusCode::OK);
    let after_first = rate_next(&env, &session, 2).await["session"].clone();
    assert_eq!(after_first["study_day"], "2026-10-24");

    env.set_time("2026-10-25T03:00:00Z");
    let (status, body) = env.get("/api/sessions/current").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let item = next_item(&after_first)["item_id"].as_i64().expect("id");
    let (status, body) = post_review(&env, &sid, item, 2, &client_id()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "session_expired");
    let (status, body) = env
        .post(&format!("/api/sessions/{sid}/undo"), &json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // The next start is a new day: the old block is expired, the review of
    // yesterday stays.
    let next = start_block(&env).await;
    assert_eq!(next["study_day"], "2026-10-25");
    assert_eq!(next["block_no"], 1);
    assert_eq!(
        env.int(&format!(
            "SELECT COUNT(*) FROM practice_session WHERE id = '{sid}' AND status = 'expired'"
        )),
        1
    );
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE undone = 0"),
        1
    );
    // "New per day" counts per study day: yesterday's word does not count.
    let today = today(&env, None).await;
    assert_eq!(today["study_day"], "2026-10-25");
    assert_eq!(today["new_words"]["introduced_today"], 0);
}

#[tokio::test]
async fn r54_study_day_starts_at_four_on_the_spring_change_day() {
    // 2026-03-29: 02:00 CET → 03:00 CEST at 01:00Z; 04:00 CEST = 02:00Z.
    let dir = tempfile::tempdir().expect("tempdir");
    let env = env_in(dir, "2026-03-29T01:59:00Z");
    env.import("test-novel", vec![word("alpha", 5, 3)]).await;
    assert_eq!(today(&env, None).await["study_day"], "2026-03-28");
    env.set_time("2026-03-29T02:00:00Z");
    assert_eq!(today(&env, None).await["study_day"], "2026-03-29");
}

// ── new-word limit and backlog pause (R-54, design §6.6) ────────────────────

#[tokio::test]
async fn r54_new_words_follow_the_daily_limit_and_usefulness_order() {
    let env = env();
    env.import(
        "test-novel",
        vec![
            word("n1", 4, 3),
            word("n2", 9, 3),
            word("n3", 6, 3),
            word("n4", 9, 3),
            word("n5", 2, 3),
            word("n6", 6, 3),
            word("n7", 6, 3),
        ],
    )
    .await;
    let view = today(&env, None).await;
    assert_eq!(view["new_words"]["per_day"], 5);
    assert_eq!(view["new_words"]["quota"], 5);
    assert_eq!(view["new_available"], 7);
    let session = start_block(&env).await;
    // Usefulness desc → source order (seq) → id.
    assert_eq!(headwords(&session), ["n2", "n4", "n3", "n6", "n7"]);
    let session = rate_all(&env, session, 2).await;
    finish(&env, &session).await;
    let view = today(&env, None).await;
    assert_eq!(view["new_words"]["introduced_today"], 5);
    assert_eq!(view["new_words"]["left_today"], 0);
    // Nothing due, quota used up: nothing to practise.
    let result = start(&env, json!({ "kind": "continue" })).await;
    assert!(result["session"].is_null(), "{result}");
    // Raising the limit allows more today.
    set_settings(&env, json!({ "new_per_day": 6 })).await;
    let more = start(&env, json!({ "kind": "continue" })).await;
    assert_eq!(headwords(&more["session"]), ["n1"]);
    assert_eq!(more["session"]["block_no"], 2);
}

#[tokio::test]
async fn r54_new_cards_take_every_fourth_slot_among_due_cards() {
    let env = env();
    let due: Vec<Value> = (1..=20)
        .map(|i| word(&format!("due{i:02}"), 5, 3))
        .collect();
    env.import("test-novel", due).await;
    make_due(&env, "due%", "2026-11-19");
    let fresh: Vec<Value> = (1..=6).map(|i| word(&format!("new{i}"), 5, 3)).collect();
    env.import("test-novel", fresh).await;
    let session = start_block(&env).await;
    assert_eq!(origins(&session), "DDDNDDDNDDDNDDDNDDDN");
    assert_eq!(session["block_size"], 20);
}

#[tokio::test]
async fn r54_backlog_pauses_new_words_until_added_anyway() {
    let env = env();
    let backlog: Vec<Value> = (1..=41)
        .map(|i| word(&format!("due{i:02}"), 5, 3))
        .collect();
    env.import("test-novel", backlog).await;
    make_due(&env, "due%", "2026-11-18");
    env.import(
        "test-novel",
        vec![word("fresh1", 5, 3), word("fresh2", 5, 3)],
    )
    .await;
    // 41 due > 2 × normal load (floor 20) → paused.
    let view = today(&env, None).await;
    assert_eq!(view["due_now"], 41);
    assert_eq!(view["new_words"]["due_now"], 41);
    assert_eq!(view["new_words"]["normal_load"], 20.0);
    assert_eq!(view["new_words"]["backlog"], true);
    assert_eq!(view["new_words"]["paused"], true);
    assert_eq!(view["new_words"]["quota"], 0);
    let session = start_block(&env).await;
    assert_eq!(origins(&session), "DDDDDDDDDDDDDDDDDDDD");
    finish(&env, &session).await;

    // "Add new words anyway (today)".
    let session = start(&env, json!({ "kind": "continue", "add_new_anyway": true })).await;
    assert_eq!(origins(&session["session"]), "DDDNDDDNDDDDDDDDDDDD");
    let view = today(&env, None).await;
    assert_eq!(view["new_words"]["anyway_today"], true);
    assert_eq!(view["new_words"]["paused"], false);
    // The override lasts only for this study day.
    env.set_day("2026-11-21");
    let view = today(&env, None).await;
    assert_eq!(view["new_words"]["anyway_today"], false);
    assert_eq!(view["new_words"]["paused"], true);
}

#[tokio::test]
async fn r54_no_pause_at_exactly_twice_the_normal_load() {
    let env = env();
    let backlog: Vec<Value> = (1..=40)
        .map(|i| word(&format!("due{i:02}"), 5, 3))
        .collect();
    env.import("test-novel", backlog).await;
    make_due(&env, "due%", "2026-11-20");
    env.import("test-novel", vec![word("fresh", 5, 3)]).await;
    let view = today(&env, None).await;
    assert_eq!(view["due_now"], 40);
    assert_eq!(view["new_words"]["paused"], false);
    assert_eq!(view["new_words"]["quota"], 5);
}

// ── interference rule (design §6.7 step 4, §15) ─────────────────────────────

#[tokio::test]
async fn r54_confusable_words_are_not_introduced_on_the_same_day() {
    let env = env();
    let mut affect = word("affect", 5, 3);
    affect["confusables"] = json!(["Effect"]);
    // `seq` order: affect, effect, other, infect; `infect` lists `affect`
    // (the rule works in both directions).
    let mut infect = word("infect", 5, 3);
    infect["confusables"] = json!(["affect"]);
    env.import(
        "test-novel",
        vec![affect, word("effect", 5, 3), word("other", 5, 3), infect],
    )
    .await;
    set_settings(&env, json!({ "new_per_day": 1 })).await;
    let first = start_block(&env).await;
    assert_eq!(headwords(&first), ["affect"]);
    let first = rate_all(&env, first, 2).await;
    finish(&env, &first).await;
    // Later the same day: `affect` was introduced today, so `effect` and
    // `infect` wait.
    set_settings(&env, json!({ "new_per_day": 4 })).await;
    let second = start(&env, json!({ "kind": "continue" })).await;
    assert_eq!(headwords(&second["session"]), ["other"]);
    let second = rate_all(&env, second["session"].clone(), 2).await;
    finish(&env, &second).await;
    // The next day `affect` no longer counts as introduced today; `effect`
    // and `infect` are not confusable with each other, so both come.
    env.set_day("2026-11-21");
    let third = start_block(&env).await;
    let words = headwords(&third);
    assert!(words.contains(&"effect".to_owned()), "{words:?}");
    assert!(words.contains(&"infect".to_owned()), "{words:?}");
}

// ── scope by source (R-59) ──────────────────────────────────────────────────

#[tokio::test]
async fn r59_a_block_can_be_limited_to_one_source() {
    let env = env();
    env.import("test-novel", vec![word("alpha", 5, 3), word("beta", 5, 3)])
        .await;
    env.import(
        "test-general",
        vec![word("gamma", 5, 3), word("beta", 5, 3)],
    )
    .await;
    make_due(&env, "alpha", "2026-11-20");
    make_due(&env, "gamma", "2026-11-20");
    let (_, sources) = env.get("/api/sources").await;
    let source_id = |slug: &str| {
        sources
            .as_array()
            .expect("sources")
            .iter()
            .find(|s| s["slug"] == slug)
            .expect("source")["id"]
            .as_i64()
            .expect("id")
    };
    let general = source_id("test-general");
    let view = today(&env, Some(general)).await;
    assert_eq!(view["scope_source_id"], general);
    assert_eq!(view["due_now"], 1);
    assert_eq!(view["new_available"], 1);
    assert_eq!(today(&env, None).await["due_now"], 2);
    let sources_in_today = view["sources"].as_array().expect("sources");
    assert_eq!(sources_in_today.len(), 2);
    assert_eq!(sources_in_today[0]["reading_position"], "Chapter 3");

    let result = start(&env, json!({ "kind": "start", "scope_source_id": general })).await;
    let session = &result["session"];
    assert_eq!(session["scope_source_id"], general);
    let mut words = headwords(session);
    words.sort();
    assert_eq!(words, ["beta", "gamma"]);

    let (status, body) = env
        .post(
            "/api/sessions",
            &json!({ "kind": "start", "scope_source_id": 999 }),
        )
        .await;
    // G8: an unknown source is refused even while a block is active.
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "unknown_source");
    // A known other scope resumes the active block (whatever its scope).
    let novel = source_id("test-novel");
    let resumed = start(&env, json!({ "kind": "start", "scope_source_id": novel })).await;
    assert_eq!(resumed["resumed"], true);
    assert_eq!(resumed["session"]["scope_source_id"], general);
    finish(&env, session).await;
    let (status, body) = env
        .post(
            "/api/sessions",
            &json!({ "kind": "start", "scope_source_id": 999 }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "unknown_source");
    let (status, _) = env.get("/api/today?scope=999").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ── requeue, repeats, relearning (design §6.7 steps 2, 6, 7) ────────────────

#[tokio::test]
async fn r54_failed_items_are_repeated_after_three_or_seven_more_items() {
    let env = env();
    let words: Vec<Value> = (1..=12).map(|i| word(&format!("w{i:02}"), 5, 3)).collect();
    env.import("test-novel", words).await;
    set_settings(&env, json!({ "new_per_day": 12 })).await;
    let session = start_block(&env).await;
    assert_eq!(session["total"], 12);
    let first = next_item(&session);
    let result = rate_next(&env, &session, 5).await;
    let repeat_id = result["repeat_item_id"].as_i64().expect("repeat");
    let items = result["session"]["items"]
        .as_array()
        .expect("items")
        .clone();
    // Answered item, then three more, then the repeat.
    assert_eq!(items[4]["item_id"], repeat_id);
    assert_eq!(items[4]["origin"], "repeat");
    assert_eq!(items[4]["card"]["is_repeat"], true);
    assert_eq!(items[4]["card"]["id"], first["card"]["id"]);
    assert_ne!(
        items[4]["sentence"]["id"], first["sentence"]["id"],
        "a repeat shows another sentence"
    );
    assert_eq!(result["review"]["card"]["state"], "relearning");
    assert_eq!(result["session"]["total"], 13);

    let second = next_item(&result["session"]);
    let result = rate_next(&env, &result["session"], 4).await;
    let repeat_id = result["repeat_item_id"].as_i64().expect("repeat");
    let items = result["session"]["items"]
        .as_array()
        .expect("items")
        .clone();
    let pos = items
        .iter()
        .position(|i| i["item_id"] == repeat_id)
        .expect("repeat");
    // Item 1 at 0, item 2 at 1, then seven items (one of them the first
    // repeat) before the new repeat.
    assert_eq!(pos, 9);
    assert_eq!(items[pos]["card"]["id"], second["card"]["id"]);

    // 3 (Shaky) is a pass: no repeat.
    let result = rate_next(&env, &result["session"], 3).await;
    assert!(result["repeat_item_id"].is_null());
}

#[tokio::test]
async fn r54_a_card_is_repeated_at_most_three_times_per_block() {
    let env = env();
    env.import("test-novel", vec![word("hard", 5, 3), word("easy", 5, 3)])
        .await;
    let mut session = start_block(&env).await;
    let hard_card = session["items"][0]["card"]["id"].clone();
    let mut repeats = 0;
    for _ in 0..4 {
        // Fail every attempt of the hard card, pass anything else.
        loop {
            let item = next_item(&session);
            let raw = if item["card"]["id"] == hard_card {
                5
            } else {
                2
            };
            let result = rate_next(&env, &session, raw).await;
            session = result["session"].clone();
            if raw == 5 {
                if !result["repeat_item_id"].is_null() {
                    repeats += 1;
                }
                break;
            }
        }
        if session["remaining"] == 0 {
            break;
        }
    }
    assert_eq!(repeats, 3);
    assert_eq!(session["remaining"], 0);
    assert_eq!(session["total"], 5);
    // It stays relearning and due today.
    let state: String = env
        .sql()
        .query_row(
            "SELECT state || ' ' || due_day FROM card WHERE id = ?1",
            [hard_card.as_i64().expect("id")],
            |r| r.get(0),
        )
        .expect("card");
    assert_eq!(state, "relearning 2026-11-20");
}

#[tokio::test]
async fn r54_relearning_cards_come_first_in_the_next_block() {
    let env = env();
    let due: Vec<Value> = (1..=5).map(|i| word(&format!("due{i}"), 9, 3)).collect();
    env.import("test-novel", due).await;
    make_due(&env, "due%", "2026-11-19");
    env.import("test-novel", vec![word("weak", 2, 3)]).await;
    make_due(&env, "weak", "2026-11-19");
    let session = start_block(&env).await;
    // Low usefulness → lowest priority → last of the due cards.
    let words = headwords(&session);
    assert_eq!(words.last().map(String::as_str), Some("weak"));
    // Answer everything else, fail `weak`, abandon the block before its repeat.
    let mut session = session;
    loop {
        let item = next_item(&session);
        let raw = if item["word"]["headword"] == "weak" {
            4
        } else {
            2
        };
        let result = rate_next(&env, &session, raw).await;
        session = result["session"].clone();
        if raw == 4 {
            break;
        }
    }
    let summary = finish(&env, &session).await;
    assert_eq!(summary["look_again"][0]["headword"], "weak");
    assert_eq!(summary["unanswered"], 1);
    let next = start(&env, json!({ "kind": "continue" })).await;
    let first = &next["session"]["items"][0];
    assert_eq!(first["word"]["headword"], "weak");
    assert_eq!(first["origin"], "due");
}

// ── summary, errors, settings ───────────────────────────────────────────────

#[tokio::test]
async fn r54_summary_counts_ratings_and_lists_words_to_look_at_again() {
    let env = env();
    env.import(
        "test-novel",
        vec![word("a1", 5, 3), word("a2", 5, 3), word("a3", 5, 3)],
    )
    .await;
    let session = start_block(&env).await;
    let r = rate_next(&env, &session, 1).await;
    let r = rate_next(&env, &r["session"], 5).await;
    let summary = finish(&env, &r["session"]).await;
    assert_eq!(summary["status"], "finished");
    assert_eq!(summary["answered"], 2);
    assert_eq!(summary["unanswered"], 2);
    assert_eq!(summary["ratings"], json!([1, 0, 0, 0, 1]));
    assert_eq!(summary["new_words"], 2);
    assert_eq!(
        summary["look_again"],
        json!([{ "word_id": summary["look_again"][0]["word_id"], "headword": "a2" }])
    );
    // Finishing again returns the same summary; rating is refused.
    assert_eq!(finish(&env, &r["session"]).await, summary);
    let item = next_item(&r["session"])["item_id"].as_i64().expect("id");
    let (status, body) = post_review(&env, &session_id(&session), item, 2, &client_id()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "session_finished");
}

#[tokio::test]
async fn r54_ratings_are_validated_and_given_in_order() {
    let env = env();
    env.import("test-novel", vec![word("a1", 5, 3), word("a2", 5, 3)])
        .await;
    let session = start_block(&env).await;
    let sid = session_id(&session);
    let items = session["items"].as_array().expect("items").clone();
    let first = items[0]["item_id"].as_i64().expect("id");
    let second = items[1]["item_id"].as_i64().expect("id");

    let (status, body) = env
        .post(&format!("/api/sessions/{sid}/undo"), &json!({}))
        .await;
    assert_eq!(
        (status, &body["error"]["code"]),
        (StatusCode::CONFLICT, &json!("nothing_to_undo"))
    );
    let (status, body) = post_review(&env, &sid, second, 2, &client_id()).await;
    assert_eq!(
        (status, &body["error"]["code"]),
        (StatusCode::CONFLICT, &json!("not_next_item"))
    );
    for raw in [0, 6] {
        let (status, body) = post_review(&env, &sid, first, raw, &client_id()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    let (status, _) = post_review(&env, &sid, first, 2, "not a uuid!").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post_review(&env, &sid, 99999, 2, &client_id()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = post_review(
        &env,
        "ffffffffffffffffffffffffffffffff",
        first,
        2,
        &client_id(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = post_review(&env, "../etc", first, 2, &client_id()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = env
        .post(
            &format!("/api/sessions/{sid}/reviews"),
            &json!({ "client_review_id": client_id(), "item_id": first, "raw_grade": 2, "extra": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = post_review(&env, &sid, first, 2, &client_id()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post_review(&env, &sid, first, 2, &client_id()).await;
    assert_eq!(
        (status, &body["error"]["code"]),
        (StatusCode::CONFLICT, &json!("already_answered"))
    );
    assert_eq!(env.int("SELECT COUNT(*) FROM review_log"), 1);
    let stored: (String, i64, i64) = env
        .sql()
        .query_row(
            "SELECT client_ts, duration_ms, session_id = ?1 FROM review_log",
            [&sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row");
    assert_eq!(stored, ("2026-11-20T10:00:00+01:00".to_owned(), 2500, 1));
}

#[tokio::test]
async fn r54_settings_can_be_read_and_changed() {
    let env = env();
    let (status, settings) = env.get("/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["new_per_day"], 5);
    assert_eq!(settings["block_size"], 20);
    assert_eq!(settings["theme"], "dark");
    let (status, settings) = env
        .request(
            "PATCH",
            "/api/settings",
            Some(&json!({ "block_size": 10, "legend": "always" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["block_size"], 10);
    assert_eq!(settings["legend"], "always");
    let (status, body) = env
        .request(
            "PATCH",
            "/api/settings",
            Some(&json!({ "new_per_day": 101 })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, _) = env
        .request("PATCH", "/api/settings", Some(&json!({ "unknown": 1 })))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Block size applies to the next block.
    let words: Vec<Value> = (1..=12).map(|i| word(&format!("d{i:02}"), 5, 3)).collect();
    env.import("test-novel", words).await;
    make_due(&env, "d%", "2026-11-20");
    assert_eq!(start_block(&env).await["total"], 10);
}

#[tokio::test]
async fn r54_today_shows_forecast_and_the_active_block() {
    let env = env();
    env.import("test-novel", vec![word("a1", 5, 3), word("a2", 5, 3)])
        .await;
    make_due(&env, "a1", "2026-11-22");
    let view = today(&env, None).await;
    assert_eq!(view["study_day"], "2026-11-20");
    assert_eq!(view["due_now"], 0);
    let forecast = view["forecast"].as_array().expect("forecast");
    assert_eq!(forecast.len(), 7);
    assert_eq!(forecast[0], json!({ "day": "2026-11-21", "due": 0 }));
    assert_eq!(forecast[1], json!({ "day": "2026-11-22", "due": 1 }));
    assert!(view["active_session"].is_null());
    let session = start_block(&env).await;
    let view = today(&env, None).await;
    assert_eq!(view["active_session"]["id"], session["id"]);
    assert_eq!(view["active_session"]["remaining"], 1);
}

#[tokio::test]
async fn r54_undo_goes_back_step_by_step_and_removes_queued_repeats() {
    let env = env();
    env.import(
        "test-novel",
        vec![word("a1", 5, 3), word("a2", 5, 3), word("a3", 5, 3)],
    )
    .await;
    let session = start_block(&env).await;
    let sid = session_id(&session);
    let items = |env: &Env| env.rows("session_item", &format!("session_id = '{sid}'"));
    let cards = |env: &Env| env.rows("card", "1");
    let state0 = (items(&env), cards(&env));
    let r1 = rate_next(&env, &session, 2).await;
    let state1 = (items(&env), cards(&env));
    let r2 = rate_next(&env, &r1["session"], 5).await;
    assert!(!r2["repeat_item_id"].is_null());
    assert_eq!(r2["session"]["total"], 4);
    let today_view = today(&env, None).await;
    assert_eq!(today_view["new_words"]["introduced_today"], 2);

    let (status, undo) = env
        .post(&format!("/api/sessions/{sid}/undo"), &json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{undo}");
    assert_eq!(undo["item_id"], next_item(&r1["session"])["item_id"]);
    assert_eq!(
        (items(&env), cards(&env)),
        state1,
        "repeat removed, card restored"
    );
    assert_eq!(undo["session"]["total"], 3);
    let (status, undo) = env
        .post(&format!("/api/sessions/{sid}/undo"), &json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{undo}");
    assert_eq!((items(&env), cards(&env)), state0);
    // Undone introductions give the new-word quota back.
    let today_view = today(&env, None).await;
    assert_eq!(today_view["new_words"]["introduced_today"], 0);
    let (status, body) = env
        .post(&format!("/api/sessions/{sid}/undo"), &json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "nothing_to_undo");
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE undone = 1"),
        2
    );
}

#[tokio::test]
async fn r51_low_priority_and_suspended_words_are_left_out() {
    let env = env();
    let mut minor = word("minor", 9, 3);
    minor["tag"] = json!({ "kind": "low-priority" });
    env.import(
        "test-novel",
        vec![
            minor,
            word("paused", 5, 3),
            word("normal", 5, 3),
            word("dueword", 5, 3),
        ],
    )
    .await;
    make_due(&env, "dueword", "2026-11-20");
    // Owner actions are M7; the stored flags are set directly here.
    env.exec("UPDATE word SET suspended = 1 WHERE headword IN ('paused', 'dueword')");
    let view = today(&env, None).await;
    assert_eq!(view["new_available"], 1);
    assert_eq!(view["due_now"], 0);
    let session = start_block(&env).await;
    assert_eq!(headwords(&session), ["normal"]);
    let session = rate_all(&env, session, 2).await;
    finish(&env, &session).await;
    // Included low-priority words are introduced (recognise-only, R-51).
    env.exec("UPDATE word SET included = 1 WHERE headword = 'minor'");
    let next = start(&env, json!({ "kind": "continue" })).await;
    assert_eq!(headwords(&next["session"]), ["minor"]);
}

// ── M3 fix round (guarantor decisions G1–G10) ───────────────────────────────

/// Seed `headword`'s Understand card as reviewed long ago and due today.
fn seed_reviewed(env: &Env, headword: &str, stability: f64) {
    env.exec(&format!(
        "UPDATE card SET state = 'review', stability = {stability}, difficulty = 5.0, \
         due_day = '2026-11-20', last_review_day = '2026-11-05', introduced_day = '2026-11-01', \
         reps = 2, format = 'sentence' \
         WHERE word_id IN (SELECT id FROM word WHERE headword = '{headword}')"
    ));
}

fn card_kinds(env: &Env) -> Vec<String> {
    let conn = env.sql();
    let mut stmt = conn
        .prepare("SELECT kind FROM card ORDER BY id")
        .expect("select");
    stmt.query_map([], |r| r.get(0))
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect()
}

#[tokio::test]
async fn g1_undo_removes_a_produce_card_an_import_unlocked_on_the_undone_rating() {
    let env = env();
    // Usefulness 5 → recognise; no Produce card.
    env.import("test-novel", vec![word("mild", 5, 3)]).await;
    seed_reviewed(&env, "mild", 5.0);
    let session = start_block(&env).await;
    let rated = rate_next(&env, &session, 2).await;
    assert!(rated["review"]["card"]["stability"].as_f64().expect("s") >= 7.0);
    assert!(
        rated["review"]["unlocked"]
            .as_array()
            .expect("u")
            .is_empty()
    );
    // A pack sets the goal to active: the import unlocks Produce.
    let mut active = word("mild", 5, 3);
    active["goal"] = json!("active");
    env.import("test-novel", vec![active]).await;
    assert_eq!(card_kinds(&env), ["understand", "produce"]);
    let (status, undo) = env
        .post(
            &format!("/api/sessions/{}/undo", session_id(&session)),
            &json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{undo}");
    // Stability 5 again: the condition (≥ 7) no longer holds.
    assert_eq!(card_kinds(&env), ["understand"]);
    assert_eq!(undo["removed_cards"].as_array().expect("removed").len(), 1);
}

#[tokio::test]
async fn g1_undo_removes_a_spelling_card_an_import_unlocked_on_the_undone_rating() {
    let env = env();
    env.import("test-novel", vec![word("tricky", 3, 3)]).await;
    seed_reviewed(&env, "tricky", 5.0);
    let session = start_block(&env).await;
    // A pass on a day after the introduction day.
    rate_next(&env, &session, 2).await;
    let mut spelled = word("tricky", 3, 3);
    spelled["spelling"] = json!(true);
    env.import("test-novel", vec![spelled]).await;
    assert_eq!(card_kinds(&env), ["understand", "spelling"]);
    let (status, undo) = env
        .post(
            &format!("/api/sessions/{}/undo", session_id(&session)),
            &json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{undo}");
    // Without the undone pass there is no passing review after introduction.
    assert_eq!(card_kinds(&env), ["understand"]);
}

#[tokio::test]
async fn g1_undo_keeps_unlocked_cards_whose_condition_still_holds() {
    let env = env();
    let mut keen = word("keen", 8, 3);
    keen["spelling"] = json!(true);
    env.import("test-novel", vec![keen]).await;
    // Mature already: Produce and Spelling exist independently of today's rating.
    seed_reviewed(&env, "keen", 30.0);
    env.exec(
        "INSERT INTO review_log (client_review_id, card_id, reviewed_at, review_day, raw_grade, \
         fsrs_rating, format, elapsed_days, before_json) SELECT 'test-old-pass', id, \
         '2026-11-05T09:00:00Z', '2026-11-05', 2, 3, 'sentence', 4, '{\"state\":\"review\"}' FROM card",
    );
    env.import("test-novel", vec![word("keen", 8, 4)]).await;
    assert_eq!(card_kinds(&env), ["understand", "produce", "spelling"]);
    let session = start_block(&env).await;
    rate_next(&env, &session, 2).await;
    let (status, _) = env
        .post(
            &format!("/api/sessions/{}/undo", session_id(&session)),
            &json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(card_kinds(&env), ["understand", "produce", "spelling"]);
}

#[tokio::test]
async fn g2_the_format_shown_is_the_format_the_first_review_fixes() {
    let env = env();
    env.import("test-novel", vec![word("bare", 8, 0)]).await;
    let session = start_block(&env).await;
    let shown = next_item(&session);
    assert_eq!(shown["card"]["format"], "word_only");
    assert!(shown["sentence"].is_null());
    // A pack adds three sentences while the item is on screen.
    env.import("test-novel", vec![word("bare", 8, 3)]).await;
    let (status, current) = env.get("/api/sessions/current").await;
    assert_eq!(status, StatusCode::OK);
    let resumed = next_item(&current);
    assert_eq!(resumed["card"]["format"], "word_only", "view unchanged");
    assert!(resumed["sentence"].is_null());
    let rated = rate_next(&env, &current, 2).await;
    assert_eq!(rated["review"]["card"]["format"], "word_only");
    let stored: (String, Option<i64>) = env
        .sql()
        .query_row("SELECT format, sentence_id FROM review_log", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("review");
    assert_eq!(stored, ("word_only".to_owned(), None));
    assert_eq!(
        env.int("SELECT COUNT(*) FROM session_item WHERE format = 'word_only'"),
        1
    );
}

#[tokio::test]
async fn g2_the_review_log_keeps_the_sentence_and_format_that_were_shown() {
    let env = env();
    env.import("test-novel", vec![word("rich", 8, 3)]).await;
    let session = start_block(&env).await;
    let shown = next_item(&session);
    assert_eq!(shown["card"]["format"], "sentence");
    // The shown sentence is hidden meanwhile (owner action, M7).
    env.exec(&format!(
        "UPDATE sentence SET hidden = 1 WHERE id = {}",
        shown["sentence"]["id"]
    ));
    rate_next(&env, &session, 2).await;
    let stored: (String, i64) = env
        .sql()
        .query_row("SELECT format, sentence_id FROM review_log", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("review");
    assert_eq!(
        stored,
        (
            "sentence".to_owned(),
            shown["sentence"]["id"].as_i64().expect("id")
        )
    );
}

#[tokio::test]
async fn g3_a_scoped_block_shows_sentences_of_its_source() {
    let env = env();
    let shared = word("shared", 5, 3);
    env.import("test-b", vec![shared.clone()]).await;
    let mut from_a = shared.clone();
    from_a["sentences"] = json!([
        { "en": "Only in A [[shared]].", "cs": "Jen v A [[shared]].", "origin": "book" }
    ]);
    env.import("test-a", vec![from_a]).await;
    let a = env.int("SELECT id FROM source WHERE slug = 'test-a'");
    let b = env.int("SELECT id FROM source WHERE slug = 'test-b'");
    let result = start(&env, json!({ "kind": "start", "scope_source_id": a })).await;
    let item = next_item(&result["session"]);
    assert_eq!(item["sentence"]["en"]["pre"], "Only in A ");
    assert_eq!(item["sentence"]["source_id"], a);
    assert!(
        item["other_sentences"]
            .as_array()
            .expect("others")
            .is_empty()
    );
    // A failed rating's repeat stays in scope too (only one sentence there).
    let failed = rate_next(&env, &result["session"], 5).await;
    let repeat = next_item(&failed["session"]);
    assert_eq!(repeat["sentence"]["source_id"], a);
    finish(&env, &failed["session"]).await;

    // Scope B: B's own sentences; an unscoped block may use any.
    env.set_day("2026-11-21");
    let result = start(&env, json!({ "kind": "start", "scope_source_id": b })).await;
    let item = next_item(&result["session"]);
    assert_eq!(item["sentence"]["source_id"], b);
    for other in item["other_sentences"].as_array().expect("others") {
        assert_eq!(other["source_id"], b);
    }
}

#[tokio::test]
async fn g3_a_scoped_block_falls_back_to_other_sources_sentences() {
    let env = env();
    env.import("test-b", vec![word("shared", 5, 3)]).await;
    let mut bare = word("shared", 5, 0);
    bare["sentences"] = json!([]);
    env.import("test-a", vec![bare]).await;
    let a = env.int("SELECT id FROM source WHERE slug = 'test-a'");
    let b = env.int("SELECT id FROM source WHERE slug = 'test-b'");
    let result = start(&env, json!({ "kind": "start", "scope_source_id": a })).await;
    let item = next_item(&result["session"]);
    assert_eq!(item["sentence"]["source_id"], b);
    assert_eq!(item["other_sentences"].as_array().expect("others").len(), 2);
}

#[tokio::test]
async fn g4_a_built_block_keeps_its_new_items_when_the_limit_is_lowered() {
    let env = env();
    let words: Vec<Value> = (1..=5).map(|i| word(&format!("n{i}"), 5, 3)).collect();
    env.import("test-novel", words).await;
    let session = start_block(&env).await;
    assert_eq!(session["total"], 5);
    set_settings(&env, json!({ "new_per_day": 1 })).await;
    let done = rate_all(&env, session, 2).await;
    assert_eq!(done["answered"], 5);
    let view = today(&env, None).await;
    assert_eq!(view["new_words"]["introduced_today"], 5);
    assert_eq!(view["new_words"]["left_today"], 0);
}

#[tokio::test]
async fn g6_add_new_anyway_is_kept_when_a_block_is_resumed() {
    let env = env();
    let backlog: Vec<Value> = (1..=41)
        .map(|i| word(&format!("due{i:02}"), 5, 3))
        .collect();
    env.import("test-novel", backlog).await;
    make_due(&env, "due%", "2026-11-18");
    env.import("test-novel", vec![word("fresh", 5, 3)]).await;
    let session = start_block(&env).await;
    let resumed = start(&env, json!({ "kind": "start", "add_new_anyway": true })).await;
    assert_eq!(resumed["resumed"], true);
    assert_eq!(session_id(&resumed["session"]), session_id(&session));
    let view = today(&env, None).await;
    assert_eq!(view["new_words"]["anyway_today"], true);
    assert_eq!(view["new_words"]["paused"], false);
    finish(&env, &session).await;
    let next = start(&env, json!({ "kind": "continue" })).await;
    assert!(headwords(&next["session"]).contains(&"fresh".to_owned()));
}

#[tokio::test]
async fn g7_empty_optional_query_parameters_mean_all() {
    let env = env();
    env.import("test-novel", vec![word("a1", 5, 3)]).await;
    let (status, view) = env.get("/api/today?scope=").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(view["scope_source_id"].is_null());
    assert_eq!(view["new_available"], 1);
    let (status, rows) = env.get("/api/words?source=&q=").await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    assert_eq!(rows.as_array().expect("rows").len(), 1);
    let (status, _) = env.get("/api/today?scope=abc").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn g8_a_block_of_an_earlier_day_finishes_as_expired_and_cannot_be_undone() {
    let env = env();
    env.import("test-novel", vec![word("x1", 5, 3), word("x2", 5, 3)])
        .await;
    let session = start_block(&env).await;
    let rated = rate_next(&env, &session, 2).await;
    assert_eq!(rated["session"]["can_undo"], true);
    env.set_day("2026-11-21");
    let summary = finish(&env, &session).await;
    assert_eq!(summary["status"], "expired");
    assert_eq!(summary["answered"], 1);
    assert_eq!(
        env.int(
            "SELECT COUNT(*) FROM practice_session WHERE status = 'expired' AND finished_at IS NULL"
        ),
        1
    );
    let (status, body) = env
        .post(
            &format!("/api/sessions/{}/undo", session_id(&session)),
            &json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "session_expired");
}

#[tokio::test]
async fn g8_a_stale_active_block_is_shown_as_expired_without_undo() {
    let env = env();
    env.import("test-novel", vec![word("x1", 5, 3), word("x2", 5, 3)])
        .await;
    let session = start_block(&env).await;
    let sid = session_id(&session);
    let rated = rate_next(&env, &session, 2).await;
    let item = next_item(&rated["session"])["item_id"]
        .as_i64()
        .expect("id");
    env.set_day("2026-11-21");
    // The stored status is still `active`; a repeated post of the rating
    // returns the block as it is seen today.
    let replay_id = env
        .sql()
        .query_row("SELECT client_review_id FROM review_log", [], |r| {
            r.get::<_, String>(0)
        })
        .expect("id");
    let first = next_item(&session)["item_id"].as_i64().expect("id");
    let (status, replay) = post_review(&env, &sid, first, 2, &replay_id).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["duplicate"], true);
    assert_eq!(replay["session"]["status"], "expired");
    assert_eq!(replay["session"]["can_undo"], false);
    let (status, body) = post_review(&env, &sid, item, 2, &client_id()).await;
    assert_eq!(
        (status, &body["error"]["code"]),
        (StatusCode::CONFLICT, &json!("session_expired"))
    );
}

#[tokio::test]
async fn g10_client_review_ids_are_uuids_stored_lowercase() {
    let env = env();
    env.import("test-novel", vec![word("a1", 5, 3), word("a2", 5, 3)])
        .await;
    let session = start_block(&env).await;
    let sid = session_id(&session);
    let item = next_item(&session)["item_id"].as_i64().expect("id");
    for bad in [
        "nope",
        "0000000000004000800000000000000a",
        "00000000-0000-4000-8000-00000000000g",
        "00000000-0000-4000-8000-0000000000000",
        "{00000000-0000-4000-8000-000000000000}",
    ] {
        let (status, body) = post_review(&env, &sid, item, 2, bad).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {body}");
        assert_eq!(body["error"]["code"], "invalid");
    }
    let upper = "ABCDEF00-0000-4000-8000-0000000000AA";
    let (status, first) = post_review(&env, &sid, item, 2, upper).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let stored: String = env
        .sql()
        .query_row("SELECT client_review_id FROM review_log", [], |r| r.get(0))
        .expect("id");
    assert_eq!(stored, upper.to_ascii_lowercase());
    // The same UUID in another case is the same review.
    let (status, again) = post_review(&env, &sid, item, 2, &upper.to_ascii_lowercase()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["duplicate"], true);
    assert_eq!(env.int("SELECT COUNT(*) FROM review_log"), 1);
}

// ── what the practice UI needs (M4) ─────────────────────────────────────────

#[tokio::test]
async fn r62_items_tell_the_client_when_the_last_rating_was_a_fail() {
    let env = env();
    env.import(
        "test-novel",
        vec![word("alpha", 9, 3), word("beta", 8, 3), word("gamma", 7, 3)],
    )
    .await;
    let session = start_block(&env).await;
    assert!(
        session["items"]
            .as_array()
            .expect("items")
            .iter()
            .all(|i| i["card"]["last_failed"] == false),
        "{session}"
    );
    // alpha rated 5 → its in-session repeat carries the flag (slip guard).
    let rated = rate_next(&env, &session, 5).await;
    let repeat_id = rated["repeat_item_id"].as_i64().expect("repeat item");
    let repeat = rated["session"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["item_id"] == repeat_id)
        .cloned()
        .expect("repeat in queue");
    assert_eq!(repeat["word"]["headword"], "alpha");
    assert_eq!(repeat["card"]["last_failed"], true);
    assert_eq!(next_item(&rated["session"])["card"]["last_failed"], false);
}

#[tokio::test]
async fn r41_items_carry_the_text_for_speech() {
    let env = env();
    let mut spoken = word("alpha", 9, 3);
    spoken["speak"] = json!("alpha speech");
    env.import("test-novel", vec![spoken, word("beta", 8, 3)])
        .await;
    let session = start_block(&env).await;
    let items = session["items"].as_array().expect("items");
    assert_eq!(items[0]["word"]["speak"], "alpha speech");
    assert!(items[1]["word"]["speak"].is_null());
}

#[tokio::test]
async fn r62_today_counts_finished_blocks_for_the_legend_and_continue() {
    let env = env();
    env.import(
        "test-novel",
        vec![word("alpha", 9, 3), word("beta", 8, 3), word("gamma", 7, 3)],
    )
    .await;
    set_settings(&env, json!({ "new_per_day": 1 })).await;
    let fresh = today(&env, None).await;
    assert_eq!(fresh["finished_blocks"], json!({ "total": 0, "today": 0 }));

    let session = rate_all(&env, start_block(&env).await, 2).await;
    // A block that is not finished yet does not count.
    assert_eq!(today(&env, None).await["finished_blocks"]["total"], 0);
    finish(&env, &session).await;
    assert_eq!(
        today(&env, None).await["finished_blocks"],
        json!({ "total": 1, "today": 1 })
    );

    // A block ended without a rating, or whose only rating was undone,
    // does not count.
    let empty = start(&env, json!({ "kind": "continue" })).await;
    if !empty["session"].is_null() {
        finish(&env, &empty["session"]).await;
    }
    set_settings(&env, json!({ "new_per_day": 3 })).await;
    let block = start_block(&env).await;
    let rated = rate_next(&env, &block, 2).await;
    let (status, _) = env
        .post(
            &format!("/api/sessions/{}/undo", session_id(&rated["session"])),
            &json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    finish(&env, &block).await;
    let unrated = start_block(&env).await;
    finish(&env, &unrated).await;
    assert_eq!(
        today(&env, None).await["finished_blocks"],
        json!({ "total": 1, "today": 1 })
    );

    // Next study day: the total stays, today starts at zero.
    env.set_day("2026-11-21");
    assert_eq!(
        today(&env, None).await["finished_blocks"],
        json!({ "total": 1, "today": 0 })
    );
}

// ── fix round R1 (A3, review probe G) ───────────────────────────────────────

/// A3 (R-54): failing the in-session repeats of a card again still queues
/// the next repeat and is logged with its raw grade, but only the first
/// failure of the day lowered the card's memory state; undo steps back
/// through those ratings one by one.
#[tokio::test]
async fn r54_repeated_failures_in_a_block_requeue_but_lower_the_memory_only_once() {
    let env = env();
    env.import("test-novel", vec![word("hard", 5, 3)]).await;
    let session = start_block(&env).await;
    let first = rate_next(&env, &session, 5).await;
    let memory = |result: &Value| {
        (
            result["review"]["card"]["stability"].clone(),
            result["review"]["card"]["difficulty"].clone(),
        )
    };
    let after_first = memory(&first);
    assert!(after_first.0.is_number());
    assert!(first["repeat_item_id"].is_i64());

    let second = rate_next(&env, &first["session"], 5).await;
    assert!(second["repeat_item_id"].is_i64(), "still requeued");
    assert_eq!(memory(&second), after_first, "memory not lowered again");
    assert_eq!(second["review"]["raw_grade"], 5);
    assert_eq!(second["review"]["card"]["state"], "relearning");
    assert_eq!(
        second["review"]["card"]["lapses"], 0,
        "a new card: no lapse"
    );
    let third = rate_next(&env, &second["session"], 4).await;
    assert_eq!(memory(&third), after_first);
    assert!(third["repeat_item_id"].is_i64());
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE memory_updated = 0 AND raw_grade >= 4"),
        2
    );
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE memory_updated = 1"),
        1
    );

    // Undo the third rating: its repeat goes, the card is as after the second.
    let sid = session_id(&session);
    let (status, undone) = env
        .post(&format!("/api/sessions/{sid}/undo"), &json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{undone}");
    assert_eq!(undone["session"]["remaining"], 1);
    // A pass on the repeat applies as usual and ends the repeats.
    let passed = rate_next(&env, &undone["session"], 2).await;
    assert_ne!(memory(&passed), after_first);
    assert_eq!(passed["review"]["card"]["state"], "review");
    assert!(passed["repeat_item_id"].is_null());
    assert_eq!(passed["session"]["remaining"], 0);
}

// ── extra practice and more new words (R-63, owner feedback after v0.1.0) ──

/// Give the words matching `headword_like` a reviewed card that is not due
/// before `due_day`, last reviewed on 2026-11-15 with `stability`.
fn seed_seen(env: &Env, headword_like: &str, stability: f64, due_day: &str) {
    env.exec(&format!(
        "UPDATE card SET state = 'review', stability = {stability}, difficulty = 5.0, \
         due_day = '{due_day}', last_review_day = '2026-11-15', introduced_day = '2026-11-01', \
         reps = 3, format = 'sentence' \
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
        .and_then(|s| s["id"].as_i64())
        .unwrap_or_else(|| panic!("{slug} not found"))
}

#[tokio::test]
async fn r63_extra_practice_serves_seen_cards_that_are_not_due() {
    let env = env();
    env.import(
        "test-novel",
        ["xa", "xb", "xc", "xd", "xt", "xdue", "xnew"]
            .iter()
            .map(|h| word(h, 5, 3))
            .collect(),
    )
    .await;
    // Lower stability → lower retrievability today.
    seed_seen(&env, "xa", 1.0, "2026-11-23");
    seed_seen(&env, "xb", 40.0, "2026-12-30");
    seed_seen(&env, "xc", 8.0, "2026-11-25");
    seed_seen(&env, "xd", 3.0, "2026-11-22");
    seed_seen(&env, "xt", 20.0, "2026-11-21");
    make_due(&env, "xdue", "2026-11-20");
    // No new words in a regular block today, so `xnew` stays unseen.
    set_settings(&env, json!({ "new_per_day": 0 })).await;

    // While a card is due, extra practice is not offered.
    let view = today(&env, None).await;
    assert_eq!(view["extra_available"], 0, "{view}");
    assert_eq!(view["due_tomorrow"], 1);
    assert_eq!(view["due_tomorrow"], view["forecast"][0]["due"]);
    assert_eq!(
        start(&env, json!({ "kind": "extra" })).await,
        json!({ "session": null, "resumed": false })
    );
    // Practising the due card (rated 2: known) makes it available.
    let session = start_block(&env).await;
    assert_eq!(headwords(&session), ["xdue"]);
    let session = rate_all(&env, session, 2).await;
    finish(&env, &session).await;
    assert_eq!(today(&env, None).await["extra_available"], 5);

    // Not due, already seen, lowest retrievability first; neither the card
    // just rated 2 nor the unseen word.
    let result = start(&env, json!({ "kind": "extra" })).await;
    let session = result["session"].clone();
    assert_eq!(session["kind"], "extra");
    assert_eq!(result["resumed"], false);
    assert_eq!(headwords(&session), ["xa", "xd", "xc", "xt", "xb"]);
    assert_eq!(origins(&session), "DDDDD");
    assert_eq!(
        today(&env, None).await["active_session"]["kind"],
        "extra",
        "Today tells what kind of block can be resumed"
    );

    // Ratings are ordinary reviews with the real elapsed time.
    let session = rate_next(&env, &session, 2).await["session"].clone(); // xa
    assert_eq!(
        env.int(
            "SELECT elapsed_days FROM review_log WHERE card_id = \
             (SELECT c.id FROM card c JOIN word w ON w.id = c.word_id WHERE w.headword = 'xa')"
        ),
        5
    );
    assert_eq!(
        env.int(
            "SELECT reps FROM card WHERE word_id = (SELECT id FROM word WHERE headword = 'xa')"
        ),
        4
    );
    let session = rate_next(&env, &session, 3).await["session"].clone(); // xd
    let session = rate_next(&env, &session, 5).await["session"].clone(); // xc fails
    let session = rate_next(&env, &session, 1).await["session"].clone(); // xt
    let session = rate_next(&env, &session, 3).await["session"].clone(); // xb
    // The failed card is repeated in the block; passing the repeat sends it
    // to a later day.
    assert_eq!(next_item(&session)["word"]["headword"], "xc");
    let session = rate_all(&env, session, 3).await;
    finish(&env, &session).await;

    // Next extra block: rated 3–5 today first (xb, xc, xd), then the rest
    // (none: xa and xt were last rated 1–2 today and stay out).
    let result = start(&env, json!({ "kind": "extra" })).await;
    let mut words = headwords(&result["session"]);
    words.sort();
    assert_eq!(words, ["xb", "xc", "xd"]);
}

#[tokio::test]
async fn r63_extra_practice_with_nothing_to_practise_is_an_empty_start() {
    let env = env();
    env.import("test-novel", vec![word("ynew", 5, 3), word("ydue", 5, 3)])
        .await;
    env.import("test-general", vec![word("yseen", 5, 3)]).await;
    seed_seen(&env, "yseen", 5.0, "2026-11-25");
    make_due(&env, "ydue", "2026-11-20");
    let novel = source_id(&env, "test-novel").await;
    let general = source_id(&env, "test-general").await;
    let empty = json!({ "session": null, "resumed": false });

    // A due card blocks extra practice in its scope (and in "all"), not
    // in another source.
    assert_eq!(today(&env, None).await["extra_available"], 0);
    assert_eq!(today(&env, Some(novel)).await["extra_available"], 0);
    assert_eq!(start(&env, json!({ "kind": "extra" })).await, empty);
    let body = json!({ "kind": "extra", "scope_source_id": novel });
    assert_eq!(start(&env, body).await, empty);
    assert_eq!(today(&env, Some(general)).await["extra_available"], 1);
    let body = json!({ "kind": "extra", "scope_source_id": general });
    let result = start(&env, body.clone()).await;
    assert_eq!(headwords(&result["session"]), ["yseen"]);

    // A card just rated 1–2 today is left out: nothing qualifies.
    let session = rate_all(&env, result["session"].clone(), 2).await;
    finish(&env, &session).await;
    assert_eq!(today(&env, Some(general)).await["extra_available"], 0);
    assert_eq!(start(&env, body).await, empty);
}

#[tokio::test]
async fn r54_more_new_words_go_beyond_the_daily_limit() {
    let env = env();
    env.import(
        "test-novel",
        (1..=12).map(|i| word(&format!("m{i:02}"), 5, 3)).collect(),
    )
    .await;
    let view = today(&env, None).await;
    assert_eq!(view["new_waiting"], 12);
    assert_eq!(view["new_left_today"], 5);
    assert_eq!(view["new_paused_reason"], Value::Null);

    // Today's five.
    let session = start_block(&env).await;
    assert_eq!(session["kind"], "regular");
    assert_eq!(origins(&session), "NNNNN");
    let session = rate_all(&env, session, 2).await;
    finish(&env, &session).await;
    let view = today(&env, None).await;
    assert_eq!(view["new_left_today"], 0);
    assert_eq!(view["new_waiting"], 7);
    assert_eq!(
        start(&env, json!({ "kind": "continue" })).await["session"],
        Value::Null
    );

    // Bad counts are refused before anything is built.
    for body in [
        json!({ "kind": "more_new", "count": 0 }),
        json!({ "kind": "more_new", "count": 51 }),
        json!({ "kind": "start", "count": 5 }),
        json!({ "kind": "more_new", "count": "5" }),
    ] {
        let (status, error) = env.post("/api/sessions", &body).await;
        assert!(status.is_client_error(), "{body} -> {status} {error}");
    }

    // Default: the new-per-day setting, at least 5.
    let result = start(&env, json!({ "kind": "more_new" })).await;
    let session = result["session"].clone();
    assert_eq!(session["kind"], "more_new");
    assert_eq!(session["block_size"], 5);
    assert_eq!(headwords(&session), ["m06", "m07", "m08", "m09", "m10"]);
    assert_eq!(origins(&session), "NNNNN");
    let session = rate_all(&env, session, 2).await;
    finish(&env, &session).await;
    // They count toward today's introduced words.
    let view = today(&env, None).await;
    assert_eq!(view["new_words"]["introduced_today"], 10);
    assert_eq!(view["new_left_today"], 0);
    assert_eq!(view["new_waiting"], 2);

    // An explicit count takes what is left; then there is nothing.
    let result = start(&env, json!({ "kind": "more_new", "count": 50 })).await;
    assert_eq!(headwords(&result["session"]), ["m11", "m12"]);
    let session = rate_all(&env, result["session"].clone(), 2).await;
    finish(&env, &session).await;
    let result = start(&env, json!({ "kind": "more_new", "count": 3 })).await;
    assert_eq!(result, json!({ "session": null, "resumed": false }));
}

#[tokio::test]
async fn r54_more_new_words_ignore_the_backlog_pause() {
    let env = env();
    let backlog: Vec<Value> = (1..=41)
        .map(|i| word(&format!("due{i:02}"), 5, 3))
        .collect();
    env.import("test-novel", backlog).await;
    make_due(&env, "due%", "2026-11-18");
    env.import(
        "test-novel",
        vec![word("fresh1", 5, 3), word("fresh2", 5, 3)],
    )
    .await;
    let view = today(&env, None).await;
    assert_eq!(view["new_paused_reason"], "backlog");
    assert_eq!(view["new_left_today"], 0);
    assert_eq!(view["new_waiting"], 2);
    let result = start(&env, json!({ "kind": "more_new", "count": 1 })).await;
    assert_eq!(headwords(&result["session"]), ["fresh1"]);
    assert_eq!(origins(&result["session"]), "N");
    // The pause itself is not lifted.
    let view = today(&env, None).await;
    assert_eq!(view["new_words"]["paused"], true);
    assert_eq!(view["new_words"]["anyway_today"], false);
}
