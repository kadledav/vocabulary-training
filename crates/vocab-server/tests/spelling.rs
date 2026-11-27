//! Spelling cards in practice (M10; R-58, design §6.2, §6.4, §6.7, §6.8):
//! served by the session builder, at most three a block (regular and extra
//! blocks), a held-back review still before first-time cards, no sentence
//! on the item, the answer details of a strict typed review, and the whole
//! way from a word marked for spelling practice to its first Spelling
//! review. All words are synthetic.

mod common;

use axum::http::StatusCode;
use common::{Env, env, word};
use serde_json::{Value, json};

// ── helpers ─────────────────────────────────────────────────────────────────

/// Typed answer, match, hint used, suggested grade, raw grade, format and
/// sentence of a stored review.
type Stored = (
    Option<String>,
    Option<String>,
    i64,
    Option<i64>,
    i64,
    String,
    Option<i64>,
);

static CLIENT_NO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn client_id() -> String {
    let n = CLIENT_NO.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("00000000-0000-4000-a000-{n:012}")
}

async fn start(env: &Env, kind: &str) -> Value {
    let (status, result) = env.post("/api/sessions", &json!({ "kind": kind })).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result["session"].clone()
}

async fn start_block(env: &Env) -> Value {
    let session = start(env, "start").await;
    assert!(!session.is_null(), "no block");
    session
}

fn sid(session: &Value) -> String {
    session["id"].as_str().expect("id").to_owned()
}

fn items(session: &Value) -> Vec<Value> {
    session["items"].as_array().expect("items").clone()
}

fn next_item(session: &Value) -> Value {
    items(session)
        .into_iter()
        .find(|i| i["answered"].is_null())
        .unwrap_or_else(|| panic!("no unanswered item in {session}"))
}

fn kinds(session: &Value) -> Vec<String> {
    items(session)
        .iter()
        .map(|i| i["card"]["kind"].as_str().expect("kind").to_owned())
        .collect()
}

fn count_kind(session: &Value, kind: &str) -> usize {
    kinds(session).iter().filter(|k| *k == kind).count()
}

async fn today(env: &Env) -> Value {
    let (status, today) = env.get("/api/today").await;
    assert_eq!(status, StatusCode::OK, "{today}");
    today
}

async fn settings(env: &Env, patch: Value) {
    let (status, body) = env.request("PATCH", "/api/settings", Some(&patch)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Post a review of the next item; `extra` carries the answer details.
async fn review(env: &Env, session: &Value, raw: u8, extra: Value) -> (StatusCode, Value) {
    let item = next_item(session);
    let mut body = json!({
        "client_review_id": client_id(),
        "item_id": item["item_id"],
        "raw_grade": raw,
        "duration_ms": 4000,
    });
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            body.insert(k.clone(), v.clone());
        }
    }
    env.post(&format!("/api/sessions/{}/reviews", sid(session)), &body)
        .await
}

/// Rate the next item: an exact typed answer for typed cards.
async fn rate_next(env: &Env, session: &Value, raw: u8) -> Value {
    let item = next_item(session);
    let extra = match item["card"]["kind"].as_str() {
        Some("produce" | "spelling") => {
            json!({ "typed_answer": item["word"]["headword"], "answer_match": "exact", "suggested_grade": 2 })
        }
        _ => json!({}),
    };
    let (status, result) = review(env, session, raw, extra).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result["session"].clone()
}

async fn rate_all(env: &Env, mut session: Value, raw: u8) -> Value {
    while session["remaining"].as_u64().expect("remaining") > 0 {
        session = rate_next(env, &session, raw).await;
    }
    session
}

/// `n` words marked for spelling practice.
fn spelling_words(stem: &str, n: usize) -> Vec<Value> {
    (1..=n)
        .map(|i| {
            let mut w = word(&format!("{stem}{i:02}"), 8, 3);
            w["goal"] = json!("recognise");
            w["spelling"] = json!(true);
            w["spelling_note"] = json!("Two letters where you hear one.");
            w
        })
        .collect()
}

/// Every Understand card is known and due on `understand_due`; every word
/// gets a Spelling card in state `state` due on `spelling_due` (a review
/// with a memory state, or one waiting for its first review).
fn spelling_cards(env: &Env, understand_due: &str, spelling_due: &str, first_time: bool) {
    let card = if first_time {
        format!(
            "INSERT INTO card (word_id, kind, state, due_day, created_at) \
             SELECT id, 'spelling', 'new', '{spelling_due}', '2026-11-01T00:00:00Z' FROM word;"
        )
    } else {
        format!(
            "INSERT INTO card (word_id, kind, format, state, stability, difficulty, due_day, \
             last_review_day, introduced_day, reps, created_at) \
             SELECT id, 'spelling', 'spelling', 'review', 12.0, 5.0, '{spelling_due}', \
             '2026-11-01', '2026-10-15', 2, '2026-10-15T00:00:00Z' FROM word;"
        )
    };
    env.exec(&format!(
        "UPDATE card SET state = 'review', stability = 30.0, difficulty = 5.0, \
         due_day = '{understand_due}', last_review_day = '2026-11-01', \
         introduced_day = '2026-10-01', reps = 4, format = 'sentence' WHERE kind = 'understand'; \
         {card}"
    ));
}

// ── the cap of three a block (R-58, design §6.7 step 3) ─────────────────────

#[tokio::test]
async fn r58_at_most_three_spelling_cards_a_block_and_the_rest_in_the_next() {
    let env = env();
    env.import("test-novel", spelling_words("sp", 7)).await;
    spelling_cards(&env, "2026-12-30", "2026-11-19", false);
    let t = today(&env).await;
    assert_eq!(t["due_now"], 7, "Today counts every due Spelling card: {t}");
    assert_eq!(t["next_block"], json!({ "due": 3, "new": 0 }), "{t}");
    let mut served = 0;
    let mut blocks = 0;
    loop {
        let before = today(&env).await;
        let session = start(&env, "continue").await;
        if session.is_null() {
            assert_eq!(before["due_now"], 0, "{before}");
            break;
        }
        blocks += 1;
        let spelling = count_kind(&session, "spelling");
        assert!(spelling <= 3, "{session}");
        assert_eq!(
            items(&session).len() as u64,
            before["next_block"]["due"].as_u64().expect("due"),
            "Today's next block is what Start builds"
        );
        served += spelling;
        let done = rate_all(&env, session, 2).await;
        env.post(&format!("/api/sessions/{}/finish", sid(&done)), &json!({}))
            .await;
    }
    assert_eq!(served, 7);
    assert_eq!(blocks, 3, "3 + 3 + 1");
}

#[tokio::test]
async fn r58_a_review_held_back_by_the_cap_still_goes_before_first_time_cards() {
    let env = env();
    let mut words = spelling_words("sh", 4);
    // Two more words whose Produce cards wait for their first review.
    for i in 1..=2 {
        let mut w = word(&format!("pr{i:02}"), 8, 3);
        w["goal"] = json!("active");
        words.push(w);
    }
    env.import("test-novel", words).await;
    env.exec(
        "UPDATE card SET state = 'review', stability = 30.0, difficulty = 5.0, \
         due_day = '2026-12-30', last_review_day = '2026-11-01', introduced_day = '2026-10-01', \
         reps = 4, format = 'sentence' WHERE kind = 'understand'; \
         INSERT INTO card (word_id, kind, format, state, stability, difficulty, due_day, \
         last_review_day, introduced_day, reps, created_at) \
         SELECT id, 'spelling', 'spelling', 'review', 12.0, 5.0, '2026-11-19', '2026-11-01', \
         '2026-10-15', 2, '2026-10-15T00:00:00Z' FROM word WHERE headword LIKE 'sh%'; \
         INSERT INTO card (word_id, kind, state, due_day, created_at) \
         SELECT id, 'produce', 'new', '2026-11-20', '2026-11-01T00:00:00Z' FROM word \
         WHERE headword LIKE 'pr%';",
    );
    let t = today(&env).await;
    assert_eq!(t["due_now"], 6, "{t}");
    assert_eq!(t["typed_today"], 2, "{t}");
    // Four Spelling reviews: three fit, one waits — and so do the
    // first-time Produce cards, which never go ahead of a due review.
    assert_eq!(t["next_block"], json!({ "due": 3, "new": 0 }), "{t}");
    let first = start_block(&env).await;
    assert_eq!(kinds(&first), ["spelling"; 3]);
    let done = rate_all(&env, first, 2).await;
    env.post(&format!("/api/sessions/{}/finish", sid(&done)), &json!({}))
        .await;
    let second = start(&env, "continue").await;
    assert_eq!(kinds(&second), ["spelling", "produce", "produce"]);
}

#[tokio::test]
async fn r58_extra_practice_keeps_the_cap_too() {
    let env = env();
    env.import("test-novel", spelling_words("se", 5)).await;
    // Nothing due: Understand later, Spelling cards reviewed, due later.
    spelling_cards(&env, "2026-12-30", "2026-12-10", false);
    let t = today(&env).await;
    assert_eq!(t["due_now"], 0, "{t}");
    assert_eq!(
        t["extra_available"],
        3 + 2,
        "three Spelling and two Understand cards: {t}"
    );
    let extra = start(&env, "extra").await;
    assert_eq!(items(&extra).len(), 5);
    assert_eq!(count_kind(&extra, "spelling"), 3, "{extra}");
}

// ── the item (design §7.1 screen 3b) ────────────────────────────────────────

#[tokio::test]
async fn r58_a_spelling_item_and_its_repeat_carry_no_sentence() {
    let env = env();
    env.import("test-novel", spelling_words("si", 1)).await;
    spelling_cards(&env, "2026-12-30", "2026-11-19", false);
    let block = start_block(&env).await;
    let item = next_item(&block);
    assert_eq!(item["card"]["kind"], "spelling");
    assert_eq!(item["card"]["format"], "spelling");
    assert!(item["sentence"].is_null(), "{item}");
    assert_eq!(
        item["word"]["spelling_note"],
        "Two letters where you hear one."
    );
    // The answer side shows an example from the word's sentences.
    assert_eq!(item["other_sentences"].as_array().map(Vec::len), Some(3));
    let (status, r) = review(
        &env,
        &block,
        5,
        json!({ "typed_answer": "si1", "answer_match": "none", "suggested_grade": 4 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert!(!r["repeat_item_id"].is_null());
    let repeat = next_item(&r["session"]);
    assert_eq!(repeat["card"]["is_repeat"], true);
    assert!(repeat["sentence"].is_null(), "{repeat}");
    // No sentence was shown, so none is logged as shown.
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE sentence_id IS NOT NULL"),
        0
    );
}

// ── answer details of a strict typed review (design §6.8, §8) ───────────────

#[tokio::test]
async fn r58_spelling_review_details_are_validated() {
    let env = env();
    env.import("test-novel", spelling_words("sv", 1)).await;
    spelling_cards(&env, "2026-12-30", "2026-11-19", false);
    let block = start_block(&env).await;
    assert_eq!(next_item(&block)["card"]["kind"], "spelling");
    for extra in [
        // A typed card says how it was answered.
        json!({}),
        json!({ "typed_answer": "sv01" }),
        // Spelling has no hint.
        json!({ "answer_match": "exact", "typed_answer": "sv01", "hint_used": true }),
        json!({ "answer_match": "none", "typed_answer": "sv1", "hint_used": true, "suggested_grade": 4 }),
        json!({ "answer_match": "skipped", "hint_used": true }),
        // Strict: an exact spelling suggests 2, never 3.
        json!({ "answer_match": "exact", "typed_answer": "sv01", "suggested_grade": 3 }),
        json!({ "answer_match": "none", "typed_answer": "sv1", "suggested_grade": 3 }),
        json!({ "answer_match": "dont_know", "suggested_grade": 4 }),
        json!({ "answer_match": "exact", "typed_answer": "  " }),
    ] {
        let (status, body) = review(&env, &block, 2, extra.clone()).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{extra} -> {body}"
        );
    }
    assert_eq!(env.int("SELECT COUNT(*) FROM review_log"), 0);
    for extra in [
        json!({ "answer_match": "exact", "typed_answer": "sv01", "suggested_grade": 2 }),
        json!({ "answer_match": "none", "typed_answer": "sv1", "suggested_grade": 4 }),
        json!({ "answer_match": "dont_know", "suggested_grade": 5 }),
        json!({ "answer_match": "skipped" }),
    ] {
        let (status, body) = review(&env, &block, 3, extra.clone()).await;
        assert_eq!(status, StatusCode::OK, "{extra} -> {body}");
        let (status, body) = env
            .post(&format!("/api/sessions/{}/undo", sid(&block)), &json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}

#[tokio::test]
async fn r58_what_was_typed_and_the_suggestion_are_stored_with_the_review() {
    let env = env();
    env.import("test-novel", spelling_words("ss", 2)).await;
    spelling_cards(&env, "2026-12-30", "2026-11-19", false);
    let block = start_block(&env).await;
    let (status, r) = review(
        &env,
        &block,
        3,
        json!({ "typed_answer": "Ss01", "answer_match": "exact", "suggested_grade": 2 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    let (status, _) = review(
        &env,
        &r["session"],
        4,
        json!({ "typed_answer": "ss2", "answer_match": "none", "suggested_grade": 4 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let stored: Vec<Stored> = {
        let conn = env.sql();
        let mut stmt = conn
            .prepare(
                "SELECT typed_answer, answer_match, hint_used, suggested_grade, raw_grade, format, \
                 sentence_id FROM review_log ORDER BY id",
            )
            .expect("select");
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect()
    };
    let row = |typed: &str, answer: &str, suggested, raw| {
        (
            Some(typed.to_owned()),
            Some(answer.to_owned()),
            0,
            Some(suggested),
            raw,
            "spelling".to_owned(),
            None,
        )
    };
    assert_eq!(
        stored,
        [row("Ss01", "exact", 2, 3), row("ss2", "none", 4, 4)]
    );
}

// ── the whole way: spelling practice on → first Spelling review ─────────────

#[tokio::test]
async fn r58_a_word_marked_for_spelling_comes_back_as_a_spelling_card_at_retention_080() {
    let env = env();
    env.import("test-novel", spelling_words("sw", 1)).await;
    let mut spelling_day = None;
    for _ in 0..12 {
        let session = start(&env, "start").await;
        if !session.is_null() {
            if kinds(&session).contains(&"spelling".to_owned()) {
                spelling_day = Some(env.today());
                let item = next_item(&session);
                assert_eq!(item["card"]["kind"], "spelling");
                assert!(item["sentence"].is_null());
                // One card per word a day: no Understand card next to it.
                assert_eq!(kinds(&session), ["spelling"]);
                rate_next(&env, &session, 2).await;
                break;
            }
            rate_all(&env, session, 2).await;
        }
        let next: Option<String> = env
            .sql()
            .query_row("SELECT MIN(due_day) FROM card", [], |r| r.get(0))
            .expect("due");
        env.set_day(&next.expect("a due day"));
    }
    assert!(spelling_day.is_some(), "the Spelling card came up");
    let t = today(&env).await;
    assert_eq!(
        t["new_typed"]["introduced_today"], 1,
        "a first-time typed card"
    );
    let id = env.int("SELECT id FROM word");
    let (status, detail) = env.get(&format!("/api/words/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    let spelling = detail["cards"]
        .as_array()
        .expect("cards")
        .iter()
        .find(|c| c["kind"] == "spelling")
        .cloned()
        .expect("a Spelling card");
    let retention = spelling["desired_retention"].as_f64().expect("retention");
    assert!((retention - 0.80).abs() < 1e-6, "{spelling}");
}

#[tokio::test]
async fn r58_first_time_spelling_cards_share_the_daily_cap_with_produce() {
    let env = env();
    env.import("test-novel", spelling_words("sc", 4)).await;
    spelling_cards(&env, "2026-12-30", "2026-11-20", true);
    settings(&env, json!({ "new_typed_per_day": 2 })).await;
    let t = today(&env).await;
    assert_eq!(t["typed_waiting"], 4, "{t}");
    assert_eq!(t["typed_today"], 2, "{t}");
    let block = start_block(&env).await;
    assert_eq!(kinds(&block), ["spelling"; 2]);
    rate_all(&env, block, 2).await;
    let t = today(&env).await;
    assert_eq!(t["new_typed"]["introduced_today"], 2, "{t}");
    assert_eq!(t["due_now"], 0, "{t}");
}
