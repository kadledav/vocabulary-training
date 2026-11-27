//! Produce cards in practice (M8; R-52, R-56, design §6.4, §6.7, §6.8,
//! §17 M8): served by the session builder, the daily cap and neutral
//! priority of first-time Produce cards, the backlog pause, the accept set
//! and the answer details of a typed review. All words are synthetic.

mod common;

use axum::http::StatusCode;
use common::{Env, env, word};
use serde_json::{Value, json};

// ── helpers ─────────────────────────────────────────────────────────────────

/// Typed answer, match, hint used, suggested grade and format of a review.
type StoredDetails = (Option<String>, Option<String>, i64, Option<i64>, String);

static CLIENT_NO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn client_id() -> String {
    let n = CLIENT_NO.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("00000000-0000-4000-8000-{n:012}")
}

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

fn headwords(session: &Value) -> Vec<String> {
    items(session)
        .iter()
        .map(|i| i["word"]["headword"].as_str().expect("headword").to_owned())
        .collect()
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

/// Rate the next item: a typed exact match for Produce, a plain rating
/// for Understand.
async fn rate_next(env: &Env, session: &Value, raw: u8) -> Value {
    let item = next_item(session);
    let extra = if item["card"]["kind"] == "produce" {
        json!({ "typed_answer": "x", "answer_match": "exact", "suggested_grade": 2 })
    } else {
        json!({})
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

/// Every word's Understand card is known and due on `understand_due`; every
/// word gets a Produce card that waits for its first review since
/// `produce_due` (as an unlock leaves it).
fn waiting_produce(env: &Env, understand_due: &str, produce_due: &str) {
    env.exec(&format!(
        "UPDATE card SET state = 'review', stability = 30.0, difficulty = 5.0, \
         due_day = '{understand_due}', last_review_day = '2026-11-01', \
         introduced_day = '2026-10-01', reps = 4, format = 'sentence' WHERE kind = 'understand'; \
         INSERT INTO card (word_id, kind, state, due_day, created_at) \
         SELECT id, 'produce', 'new', '{produce_due}', '2026-11-01T00:00:00Z' FROM word;"
    ));
}

fn words(stem: &str, n: usize) -> Vec<Value> {
    (1..=n)
        .map(|i| {
            let mut w = word(&format!("{stem}{i:02}"), 8, 3);
            w["goal"] = json!("active");
            w
        })
        .collect()
}

// ── the daily cap (design §17 M8) ───────────────────────────────────────────

#[tokio::test]
async fn r52_first_time_produce_cards_are_capped_per_day_and_the_rest_wait() {
    let env = env();
    env.import("test-novel", words("tp", 25)).await;
    waiting_produce(&env, "2026-12-30", "2026-11-19");
    settings(&env, json!({ "block_size": 50 })).await;

    let t = today(&env).await;
    assert_eq!(t["typed_waiting"], 25, "{t}");
    assert_eq!(t["new_typed"]["per_day"], 10);
    assert_eq!(t["new_typed"]["left_today"], 10);
    assert_eq!(t["new_typed"]["quota"], 10);
    assert_eq!(t["due_now"], 10, "only what today's cap lets in is due");
    // The waiting cards are no review backlog and not in the forecast.
    assert_eq!(t["new_words"]["due_now"], 0);
    assert!(
        t["forecast"]
            .as_array()
            .expect("forecast")
            .iter()
            .all(|f| f["due"] == 0)
    );

    let block = start_block(&env).await;
    assert_eq!(kinds(&block), vec!["produce"; 10]);
    // Waiting since the same day: the order is by card id (usefulness ties).
    let first: Vec<String> = headwords(&block);
    assert_eq!(first[0], "tp01");
    let block = rate_all(&env, block, 2).await;
    assert_eq!(block["remaining"], 0);

    let t = today(&env).await;
    assert_eq!(t["new_typed"]["introduced_today"], 10);
    assert_eq!(t["new_typed"]["quota"], 0);
    assert_eq!(t["typed_waiting"], 15);
    assert_eq!(t["due_now"], 0);
    // Nothing more today, so extra practice is offered instead.
    let result = start(&env, json!({ "kind": "continue" })).await;
    assert!(result["session"].is_null(), "{result}");
    assert!(t["extra_available"].as_u64().expect("extra") > 0, "{t}");

    // The next day the next ten come, then the last five.
    env.set_day("2026-11-21");
    let block = start_block(&env).await;
    // Besides reviews of yesterday's first ten (if due), ten new ones.
    let second: Vec<String> = headwords(&block)
        .into_iter()
        .filter(|h| !first.contains(h))
        .collect();
    assert_eq!(second.len(), 10, "{block}");
    rate_all(&env, block, 2).await;
    env.set_day("2026-11-22");
    let block = start_block(&env).await;
    let produce: Vec<String> = items(&block)
        .iter()
        .filter(|i| i["card"]["kind"] == "produce" && i["origin"] == "due")
        .map(|i| i["word"]["headword"].as_str().expect("hw").to_owned())
        .filter(|h| !first.contains(h) && !second.contains(h))
        .collect();
    assert_eq!(produce.len(), 5);
}

#[tokio::test]
async fn r52_the_cap_is_a_setting_and_zero_holds_every_first_time_card() {
    let env = env();
    env.import("test-novel", words("tz", 4)).await;
    waiting_produce(&env, "2026-12-30", "2026-11-20");
    settings(&env, json!({ "new_typed_per_day": 0 })).await;
    let t = today(&env).await;
    assert_eq!(t["typed_waiting"], 4);
    assert_eq!(t["new_typed"]["quota"], 0);
    let result = start(&env, json!({ "kind": "start" })).await;
    assert!(result["session"].is_null(), "{result}");

    settings(&env, json!({ "new_typed_per_day": 3 })).await;
    let block = start_block(&env).await;
    assert_eq!(kinds(&block), vec!["produce"; 3]);
    // Out of range is refused.
    let (status, _) = env
        .request(
            "PATCH",
            "/api/settings",
            Some(&json!({ "new_typed_per_day": 101 })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn r52_undoing_a_first_review_gives_the_slot_back() {
    let env = env();
    env.import("test-novel", words("tu", 3)).await;
    waiting_produce(&env, "2026-12-30", "2026-11-20");
    settings(&env, json!({ "new_typed_per_day": 1 })).await;
    let block = start_block(&env).await;
    assert_eq!(kinds(&block), vec!["produce"]);
    let after = rate_next(&env, &block, 2).await;
    assert_eq!(today(&env).await["new_typed"]["left_today"], 0);
    let (status, _) = env
        .post(&format!("/api/sessions/{}/undo", sid(&after)), &json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let t = today(&env).await;
    assert_eq!(t["new_typed"]["introduced_today"], 0);
    assert_eq!(t["new_typed"]["left_today"], 1);
}

// ── neutral priority and the sibling rule ───────────────────────────────────

#[tokio::test]
async fn r52_a_due_review_wins_over_a_first_time_card_of_the_same_word() {
    let env = env();
    env.import("test-novel", words("ts", 2)).await;
    // ts01: Understand due today (on time) and Produce waiting.
    // ts02: Understand not due, Produce waiting.
    waiting_produce(&env, "2026-12-30", "2026-11-19");
    env.exec(
        "UPDATE card SET due_day = '2026-11-20', last_review_day = '2026-10-21', stability = 30.0 \
         WHERE kind = 'understand' AND word_id = (SELECT id FROM word WHERE headword = 'ts01')",
    );
    let block = start_block(&env).await;
    let pairs: Vec<(String, String)> = items(&block)
        .iter()
        .map(|i| {
            (
                i["word"]["headword"].as_str().expect("hw").to_owned(),
                i["card"]["kind"].as_str().expect("kind").to_owned(),
            )
        })
        .collect();
    assert!(
        pairs.contains(&("ts01".to_owned(), "understand".to_owned())),
        "{pairs:?}"
    );
    assert!(
        pairs.contains(&("ts02".to_owned(), "produce".to_owned())),
        "{pairs:?}"
    );
    assert_eq!(pairs.len(), 2, "one card per word: {pairs:?}");
}

#[tokio::test]
async fn r52_first_time_cards_do_not_push_ahead_of_overdue_reviews() {
    let env = env();
    env.import("test-novel", words("to", 6)).await;
    waiting_produce(&env, "2026-12-30", "2026-11-19");
    // to01–to03 Understand cards are long overdue; their Produce cards wait
    // too but the reviews win. to04–to06 only have the waiting Produce card.
    env.exec(
        "UPDATE card SET due_day = '2026-11-01', last_review_day = '2026-09-01', stability = 20.0 \
         WHERE kind = 'understand' AND word_id IN \
         (SELECT id FROM word WHERE headword IN ('to01', 'to02', 'to03'))",
    );
    let block = start_block(&env).await;
    let order: Vec<String> = items(&block)
        .iter()
        .map(|i| {
            format!(
                "{}:{}",
                i["word"]["headword"].as_str().expect("hw"),
                i["card"]["kind"].as_str().expect("k")
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            "to01:understand",
            "to02:understand",
            "to03:understand",
            "to04:produce",
            "to05:produce",
            "to06:produce"
        ],
        "overdue reviews first, first-time cards after them"
    );
}

/// "word:kind" of every item of a block.
fn order(session: &Value) -> Vec<String> {
    items(session)
        .iter()
        .map(|i| {
            format!(
                "{}:{}",
                i["word"]["headword"].as_str().expect("hw"),
                i["card"]["kind"].as_str().expect("k")
            )
        })
        .collect()
}

#[tokio::test]
async fn r52_first_time_cards_never_go_ahead_of_on_time_or_overdue_reviews() {
    let env = env();
    let mut list = words("rv", 6);
    list.extend(words("fp", 10));
    env.import("test-novel", list).await;
    waiting_produce(&env, "2026-12-30", "2026-11-19");
    settings(&env, json!({ "block_size": 5 })).await;
    // rv01–rv03 are due exactly today with memory to spare (retrievability
    // above the target: a negative priority); rv04–rv06 are overdue. Their
    // own Produce cards are not unlocked; fp01–fp10 have only a waiting
    // Produce card.
    env.exec(
        "DELETE FROM card WHERE kind = 'produce' AND word_id IN (SELECT id FROM word WHERE headword LIKE 'rv%'); \
         UPDATE card SET due_day = '2026-11-20', last_review_day = '2026-11-12', stability = 30.0 \
         WHERE kind = 'understand' AND word_id IN (SELECT id FROM word WHERE headword IN ('rv01', 'rv02', 'rv03')); \
         UPDATE card SET due_day = '2026-11-10', last_review_day = '2026-10-01', stability = 20.0 \
         WHERE kind = 'understand' AND word_id IN (SELECT id FROM word WHERE headword IN ('rv04', 'rv05', 'rv06'));",
    );
    let t = today(&env).await;
    assert_eq!(t["due_now"], 16, "6 reviews + 10 first-time cards: {t}");
    let first = start_block(&env).await;
    assert!(
        order(&first).iter().all(|o| o.ends_with(":understand")),
        "the reviews come first: {:?}",
        order(&first)
    );
    rate_all(&env, first, 2).await;
    let second = start(&env, json!({ "kind": "continue" })).await["session"].clone();
    let second = order(&second);
    assert_eq!(second.len(), 5);
    assert!(second[0].ends_with(":understand"), "{second:?}");
    assert!(
        second[1..].iter().all(|o| o.ends_with(":produce")),
        "first-time cards fill what is left: {second:?}"
    );
}

#[tokio::test]
async fn r52_today_never_offers_a_start_that_yields_nothing() {
    let env = env();
    env.import("test-novel", words("pd", 2)).await;
    waiting_produce(&env, "2026-12-30", "2026-11-19");
    // pd01: Understand due today and a waiting Produce card (blocked today
    // by the sibling rule); pd02: known, nothing due.
    env.exec(
        "UPDATE card SET due_day = '2026-11-20', last_review_day = '2026-10-21' \
         WHERE kind = 'understand' AND word_id = (SELECT id FROM word WHERE headword = 'pd01'); \
         DELETE FROM card WHERE kind = 'produce' AND word_id = (SELECT id FROM word WHERE headword = 'pd02');",
    );
    let t = today(&env).await;
    assert_eq!(t["due_now"], 1, "only the review comes today: {t}");
    assert_eq!(t["typed_today"], 0, "{t}");
    assert_eq!(t["next_block"], json!({ "due": 1, "new": 0 }), "{t}");
    let block = start_block(&env).await;
    assert_eq!(order(&block), ["pd01:understand"]);
    rate_all(&env, block, 2).await;

    let t = today(&env).await;
    assert_eq!(t["due_now"], 0, "{t}");
    assert_eq!(t["typed_waiting"], 1, "the card still waits: {t}");
    assert_eq!(t["typed_today"], 0, "{t}");
    assert_eq!(t["next_block"], json!({ "due": 0, "new": 0 }), "{t}");
    let result = start(&env, json!({ "kind": "continue" })).await;
    assert!(result["session"].is_null(), "{result}");
    // The day is done, so extra practice is there.
    assert!(t["extra_available"].as_u64().expect("extra") > 0, "{t}");
    let extra = start(&env, json!({ "kind": "extra" })).await;
    assert!(!extra["session"].is_null(), "{extra}");

    // Tomorrow the Produce card comes.
    env.set_day("2026-11-21");
    let t = today(&env).await;
    assert_eq!(t["typed_today"], 1, "{t}");
    let block = start_block(&env).await;
    assert!(order(&block).contains(&"pd01:produce".to_owned()));
}

#[tokio::test]
async fn r52_two_due_cards_of_one_word_count_once() {
    let env = env();
    env.import("test-novel", words("tw", 1)).await;
    // Both cards of tw01 are due reviews today: one comes, the other waits
    // for another day (one card per word per day).
    env.exec(
        "UPDATE card SET state = 'review', stability = 30.0, difficulty = 5.0, due_day = '2026-11-20', \
         last_review_day = '2026-10-21', introduced_day = '2026-10-01', reps = 4, format = 'sentence'; \
         INSERT INTO card (word_id, kind, state, stability, difficulty, due_day, last_review_day, \
         introduced_day, reps, format, created_at) SELECT id, 'produce', 'review', 30.0, 5.0, \
         '2026-11-20', '2026-10-21', '2026-10-21', 3, 'cloze', '2026-11-01T00:00:00Z' FROM word;",
    );
    let t = today(&env).await;
    assert_eq!(t["due_now"], 1, "{t}");
    let block = start_block(&env).await;
    assert_eq!(items(&block).len(), 1);
    rate_all(&env, block, 2).await;
    let t = today(&env).await;
    assert_eq!(t["due_now"], 0, "{t}");
    assert_eq!(t["next_block"]["due"], 0, "{t}");
}

#[tokio::test]
async fn r54_resetting_a_produce_card_reviewed_today_keeps_its_slot_used() {
    let env = env();
    env.import("test-novel", words("tr", 3)).await;
    waiting_produce(&env, "2026-12-30", "2026-11-20");
    settings(&env, json!({ "new_typed_per_day": 1 })).await;
    let block = start_block(&env).await;
    let first = headwords(&block)[0].clone();
    rate_all(&env, block, 2).await;
    let id = env.word_id(&first).await.get();
    let (status, body) = env
        .post(
            &format!("/api/words/{id}/actions"),
            &json!({ "action": "reset_card", "kind": "produce" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let t = today(&env).await;
    assert_eq!(t["new_typed"]["introduced_today"], 1, "{t}");
    assert_eq!(t["new_typed"]["left_today"], 0, "{t}");
    let result = start(&env, json!({ "kind": "continue" })).await;
    assert!(
        result["session"].is_null(),
        "no second first-time card today: {result}"
    );
}

#[tokio::test]
async fn r54_the_backlog_pause_holds_first_time_cards_too_until_added_anyway() {
    let env = env();
    let mut list = words("tb", 3);
    list.extend((1..=45).map(|i| word(&format!("tbk{i:02}"), 5, 3)));
    env.import("test-novel", list).await;
    waiting_produce(&env, "2026-12-30", "2026-11-19");
    // 45 reviews due (> 2 × the floor of 20): the backlog pause.
    env.exec(
        "DELETE FROM card WHERE kind = 'produce' AND word_id IN (SELECT id FROM word WHERE headword LIKE 'tbk%'); \
         UPDATE card SET due_day = '2026-11-20', last_review_day = '2026-09-01' \
         WHERE kind = 'understand' AND word_id IN (SELECT id FROM word WHERE headword LIKE 'tbk%');",
    );
    let t = today(&env).await;
    assert_eq!(t["new_words"]["paused"], true, "{t}");
    assert_eq!(t["new_typed"]["paused"], true);
    assert_eq!(t["new_typed"]["quota"], 0);
    assert_eq!(t["typed_waiting"], 3);
    assert_eq!(t["due_now"], 45);
    settings(&env, json!({ "block_size": 50 })).await;
    let block = start(&env, json!({ "kind": "start", "add_new_anyway": true })).await;
    let session = &block["session"];
    let produce = kinds(session).iter().filter(|k| *k == "produce").count();
    assert_eq!(produce, 3, "added anyway: they come after the reviews");
    assert_eq!(kinds(session).last().map(String::as_str), Some("produce"));
}

// ── the item: accept set (design §6.8, D-5) ─────────────────────────────────

#[tokio::test]
async fn r56_a_produce_item_carries_the_accept_set_and_a_cloze_sentence() {
    let env = env();
    let mut w = word("test-ravel", 8, 3);
    w["goal"] = json!("active");
    w["accepted"] = json!(["test-ravelled", "test-raveled"]);
    w["sentences"][0]["en"] = json!("The thread had [[test-ravelled]] overnight.");
    w["sentences"][1]["en"] = json!("They [[test-ravelled]] it again.");
    w["sentences"][2]["en"] = json!("Never [[test-ravel]] it.");
    env.import("test-novel", vec![w]).await;
    let id = env.word_id("test-ravel").await.get();
    let (status, _) = env
        .post(
            &format!("/api/words/{id}/items"),
            &json!({ "kind": "accepted", "value": "test-unknot" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    waiting_produce(&env, "2026-12-30", "2026-11-20");
    let block = start_block(&env).await;
    let item = next_item(&block);
    assert_eq!(item["card"]["kind"], "produce");
    assert_eq!(item["card"]["format"], "cloze");
    let accept: Vec<&str> = item["accept"]
        .as_array()
        .expect("accept")
        .iter()
        .map(|v| v.as_str().expect("text"))
        .collect();
    let target = item["sentence"]["en"]["target"].as_str().expect("target");
    for expected in [
        "test-ravelled",
        "test-raveled",
        "test-ravel",
        "test-unknot",
        target,
    ] {
        assert!(accept.contains(&expected), "{expected} not in {accept:?}");
    }
    assert!(
        !item["sentence"]["cs"]["target"]
            .as_str()
            .expect("cs")
            .is_empty()
    );
}

// ── answer details of a typed review (design §6.8, §8) ──────────────────────

#[tokio::test]
async fn r56_typed_answer_hint_and_suggestion_are_stored_with_the_review() {
    let env = env();
    env.import("test-novel", words("ta", 2)).await;
    waiting_produce(&env, "2026-12-30", "2026-11-20");
    let block = start_block(&env).await;
    let (status, r) = review(
        &env,
        &block,
        3,
        json!({ "typed_answer": "  ta01 ", "answer_match": "exact", "suggested_grade": 3, "hint_used": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    let (status, r) = review(
        &env,
        &r["session"],
        5,
        json!({ "answer_match": "dont_know", "suggested_grade": 5 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    let rows = env.rows("review_log", "1");
    assert_eq!(rows.len(), 2);
    let stored: Vec<StoredDetails> = {
        let conn = env.sql();
        let mut stmt = conn
            .prepare(
                "SELECT typed_answer, answer_match, hint_used, suggested_grade, format \
                 FROM review_log ORDER BY id",
            )
            .expect("select");
        stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect()
    };
    assert_eq!(
        stored,
        vec![
            (
                Some("  ta01 ".to_owned()),
                Some("exact".to_owned()),
                1,
                Some(3),
                "cloze".to_owned()
            ),
            (
                None,
                Some("dont_know".to_owned()),
                0,
                Some(5),
                "cloze".to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn r56_typed_review_details_are_validated() {
    let env = env();
    env.import("test-novel", words("tv", 1)).await;
    waiting_produce(&env, "2026-12-30", "2026-11-20");
    let block = start_block(&env).await;
    assert_eq!(next_item(&block)["card"]["kind"], "produce");
    let long = "a".repeat(301);
    for extra in [
        json!({}),
        json!({ "typed_answer": "tv01" }),
        json!({ "answer_match": "exact" }),
        json!({ "answer_match": "none" }),
        json!({ "answer_match": "dont_know", "typed_answer": "tv01" }),
        json!({ "answer_match": "skipped", "typed_answer": "tv01" }),
        json!({ "answer_match": "exact", "typed_answer": "   " }),
        json!({ "answer_match": "exact", "typed_answer": "tv\u{0}01" }),
        json!({ "answer_match": "none", "typed_answer": "tv\n01" }),
        json!({ "answer_match": "none", "typed_answer": long }),
        json!({ "answer_match": "fuzzy", "typed_answer": "tv01" }),
        json!({ "answer_match": "exact", "typed_answer": 5 }),
    ] {
        let (status, body) = review(&env, &block, 2, extra.clone()).await;
        assert!(
            status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
            "{extra} -> {status} {body}"
        );
    }
    assert_eq!(env.int("SELECT COUNT(*) FROM review_log"), 0);
    // 300 characters are fine.
    let (status, body) = review(
        &env,
        &block,
        4,
        json!({ "answer_match": "none", "typed_answer": "b".repeat(300), "suggested_grade": 4 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn r56_an_understand_review_takes_no_typed_details() {
    let env = env();
    env.import("test-novel", vec![word("tw01", 8, 3)]).await;
    let block = start_block(&env).await;
    assert_eq!(next_item(&block)["card"]["kind"], "understand");
    for extra in [
        json!({ "typed_answer": "tw01", "answer_match": "exact" }),
        json!({ "answer_match": "skipped" }),
        json!({ "hint_used": true }),
        json!({ "suggested_grade": 2 }),
    ] {
        let (status, body) = review(&env, &block, 2, extra.clone()).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{extra} -> {body}"
        );
    }
    let (status, body) = review(&env, &block, 2, json!({ "hint_used": false })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

// ── the whole way: unlock → first Produce review ────────────────────────────

#[tokio::test]
async fn r52_a_word_learnt_to_stability_seven_comes_back_as_produce() {
    let env = env();
    env.import("test-novel", words("tf", 1)).await;
    let mut unlocked_on = None;
    for _ in 0..12 {
        let result = start(&env, json!({ "kind": "start" })).await;
        if !result["session"].is_null() {
            let session = result["session"].clone();
            if kinds(&session).contains(&"produce".to_owned()) {
                unlocked_on = Some(env.today());
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
    let day = unlocked_on.expect("Produce came up");
    let block = start_block(&env).await;
    let item = next_item(&block);
    assert_eq!(item["card"]["kind"], "produce");
    // The same word's Understand card is not in that day's block.
    assert!(kinds(&block).iter().all(|k| k == "produce"));
    let after = rate_next(&env, &block, 2).await;
    assert_eq!(after["remaining"], 0);
    let t = today(&env).await;
    assert_eq!(t["new_typed"]["introduced_today"], 1, "{day}");
}
