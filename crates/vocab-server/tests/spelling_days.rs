//! Many study days with a fake clock and several blocks a day (design §6.6,
//! §6.7, §17 M10; R-52, R-54, R-58): synthetic words with Understand,
//! Produce and Spelling cards — reviews of all three kinds, a pile of
//! first-time Produce and Spelling cards unlocked at once, a few unseen
//! words — practised block after block with "Continue". Every day: what
//! Today says before each block is what Start builds, the day's blocks
//! serve exactly Today's due and new counts, no block has more than three
//! Spelling cards, no word gets two different cards, a first-time card
//! never comes before a due review, and the day ends with nothing due (no
//! dead end). In the end every first-time card has come.

mod common;

use std::collections::{HashMap, HashSet};

use axum::http::StatusCode;
use common::{Env, env, word};
use serde_json::{Value, json};

const ACTIVE: usize = 40;
const SPELLING_EVERY: usize = 2;
const UNSEEN: usize = 8;
const CAP: u64 = 10;
const BLOCK: u64 = 20;
const DAYS: usize = 25;

static CLIENT_NO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn client_id() -> String {
    let n = CLIENT_NO.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("00000000-0000-4000-9000-{n:012}")
}

async fn today(env: &Env) -> Value {
    let (status, today) = env.get("/api/today").await;
    assert_eq!(status, StatusCode::OK, "{today}");
    today
}

async fn start(env: &Env, kind: &str) -> Value {
    let (status, result) = env.post("/api/sessions", &json!({ "kind": kind })).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result["session"].clone()
}

fn items(session: &Value) -> Vec<Value> {
    session["items"].as_array().expect("items").clone()
}

fn number(value: &Value) -> u64 {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("not a number: {value}"))
}

fn id(value: &Value) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| panic!("not an id: {value}"))
}

/// Produce/Spelling cards that still wait for their first review.
fn first_time_cards(env: &Env) -> HashSet<i64> {
    let conn = env.sql();
    let mut stmt = conn
        .prepare("SELECT id FROM card WHERE kind != 'understand' AND state = 'new'")
        .expect("prepare");
    stmt.query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// A spread of ratings: mostly 2, some 1 and 3, and a 4 now and then on a
/// review (which repeats it in the block; the repeat passes).
fn grade(item: &Value, day_no: usize) -> u8 {
    let card = usize::try_from(id(&item["card"]["id"])).expect("id");
    if item["card"]["is_repeat"] == true {
        return 2;
    }
    match (card + day_no) % 9 {
        0 => 1,
        1 => 3,
        2 if !item["card"]["is_new"].as_bool().unwrap_or(false) => 4,
        _ => 2,
    }
}

/// Rate every item of the block (repeats included); typed cards send a
/// typed answer that did not match.
async fn rate_block(env: &Env, mut session: Value, day_no: usize) {
    while number(&session["remaining"]) > 0 {
        let item = items(&session)
            .into_iter()
            .find(|i| i["answered"].is_null())
            .expect("next");
        let mut body = json!({
            "client_review_id": client_id(),
            "item_id": item["item_id"],
            "raw_grade": grade(&item, day_no),
            "duration_ms": 3000,
        });
        if item["card"]["kind"] != "understand" {
            body["typed_answer"] = json!("test-answer");
            body["answer_match"] = json!("none");
            body["suggested_grade"] = json!(4);
        }
        let (status, result) = env
            .post(
                &format!(
                    "/api/sessions/{}/reviews",
                    session["id"].as_str().expect("id")
                ),
                &body,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        session = result["session"].clone();
    }
    let (status, body) = env
        .post(
            &format!(
                "/api/sessions/{}/finish",
                session["id"].as_str().expect("id")
            ),
            &json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn setup(env: &Env) {
    // Known words: Understand reviews spread over the next 30 days; every
    // other word is marked for spelling practice. Every known word's
    // Produce card and half of the Spelling cards wait for their first
    // review since the first day; the other Spelling cards are reviews, 8
    // of them overdue on the first day (more than two blocks' worth of the
    // cap of three).
    env.exec(
        "UPDATE card SET state = 'review', stability = 40.0, difficulty = 5.0, \
         due_day = date('2026-11-20', '+' || (word_id % 30) || ' days'), \
         last_review_day = '2026-10-20', introduced_day = '2026-09-01', reps = 5, \
         format = 'sentence' WHERE kind = 'understand' AND word_id IN \
         (SELECT id FROM word WHERE headword LIKE 'act%'); \
         INSERT INTO card (word_id, kind, state, due_day, created_at) \
         SELECT id, 'produce', 'new', '2026-11-20', '2026-11-20T00:00:00Z' FROM word \
         WHERE headword LIKE 'act%'; \
         INSERT INTO card (word_id, kind, state, due_day, created_at) \
         SELECT id, 'spelling', 'new', '2026-11-20', '2026-11-20T00:00:00Z' FROM word \
         WHERE spelling = 1 AND id % 4 = 0; \
         INSERT INTO card (word_id, kind, format, state, stability, difficulty, due_day, \
         last_review_day, introduced_day, reps, created_at) \
         SELECT id, 'spelling', 'spelling', 'review', 15.0, 5.0, \
         CASE WHEN id % 4 = 2 AND id < 34 THEN '2026-11-18' \
              ELSE date('2026-11-21', '+' || (id % 20) || ' days') END, \
         '2026-11-01', '2026-10-01', 3, '2026-10-01T00:00:00Z' FROM word \
         WHERE spelling = 1 AND id % 4 != 0;",
    );
}

#[tokio::test]
async fn r58_understand_produce_and_spelling_blocks_over_many_days_match_today_without_a_dead_end()
{
    let env = env();
    let mut words: Vec<Value> = (1..=ACTIVE)
        .map(|i| {
            let mut w = word(
                &format!("act{i:03}"),
                4 + u8::try_from(i % 6).expect("u8"),
                3,
            );
            w["goal"] = json!("active");
            if i % SPELLING_EVERY == 0 {
                w["spelling"] = json!(true);
            }
            w
        })
        .collect();
    words.extend((1..=UNSEEN).map(|i| word(&format!("new{i:03}"), 7, 3)));
    env.import("test-novel", words).await;
    setup(&env);
    let overdue_spelling =
        env.int("SELECT COUNT(*) FROM card WHERE kind = 'spelling' AND due_day < '2026-11-20'");
    assert_eq!(overdue_spelling, 8, "the setup has the cap to work against");
    let (status, body) = env
        .request(
            "PATCH",
            "/api/settings",
            Some(&json!({ "block_size": BLOCK, "new_typed_per_day": CAP, "new_per_day": 3 })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let mut log = Vec::new();
    let mut day = env.today();
    let mut spelling_served = 0u64;
    for day_no in 0..DAYS {
        let before = today(&env).await;
        let due_now = number(&before["due_now"]);
        let typed_today = number(&before["typed_today"]);
        let new_today = number(&before["new_left_today"]).min(number(&before["new_waiting"]));
        assert!(typed_today <= CAP, "{before}");
        assert!(
            !before["new_words"]["paused"].as_bool().unwrap_or(false),
            "the test keeps the backlog small: {before}"
        );
        let first_time = first_time_cards(&env);
        let mut served_due = 0u64;
        let mut served_new = 0u64;
        let mut served_first = 0u64;
        let mut first_seen = false;
        let mut card_of_word: HashMap<i64, i64> = HashMap::new();
        let mut blocks = 0;
        loop {
            let plan = today(&env).await;
            let planned = number(&plan["next_block"]["due"]) + number(&plan["next_block"]["new"]);
            let session = start(&env, if blocks == 0 { "start" } else { "continue" }).await;
            if planned == 0 {
                assert!(
                    session.is_null(),
                    "day {day}: Today said nothing, Start built {session}"
                );
                break;
            }
            assert!(
                !session.is_null(),
                "day {day}: Today said {planned}, Start built nothing"
            );
            blocks += 1;
            assert!(blocks <= 10, "day {day}: blocks never end");
            let served = items(&session);
            assert_eq!(served.len() as u64, planned, "day {day}: {plan}");
            let spelling = served.iter().filter(|i| i["card"]["kind"] == "spelling");
            assert!(spelling.count() <= 3, "day {day}: {session}");
            for item in &served {
                let card = id(&item["card"]["id"]);
                let word = id(&item["word"]["id"]);
                let known = *card_of_word.entry(word).or_insert(card);
                assert_eq!(known, card, "day {day}: two cards of one word");
                let is_first = first_time.contains(&card);
                match item["origin"].as_str() {
                    Some("new") => served_new += 1,
                    Some("due") if is_first => {
                        served_first += 1;
                        first_seen = true;
                        served_due += 1;
                    }
                    Some("due") => {
                        assert!(
                            !first_seen,
                            "day {day}: a review came after a first-time card"
                        );
                        served_due += 1;
                    }
                    other => panic!("day {day}: unexpected origin {other:?}"),
                }
                if item["card"]["kind"] == "spelling" {
                    spelling_served += 1;
                }
            }
            assert_eq!(
                number(&plan["next_block"]["new"]),
                served.iter().filter(|i| i["origin"] == "new").count() as u64,
                "day {day}"
            );
            rate_block(&env, session, day_no).await;
        }
        assert_eq!(
            served_due, due_now,
            "day {day}: Today's due count: {before}"
        );
        assert_eq!(served_first, typed_today, "day {day}: first-time count");
        assert_eq!(served_new, new_today, "day {day}: new words: {before}");
        let after = today(&env).await;
        assert_eq!(number(&after["due_now"]), 0, "day {day}: {after}");
        assert_eq!(
            after["next_block"],
            json!({ "due": 0, "new": 0 }),
            "day {day}"
        );
        assert_eq!(
            number(&after["new_typed"]["introduced_today"]),
            typed_today,
            "day {day}"
        );
        let extra = start(&env, "extra").await;
        assert_eq!(
            !extra.is_null(),
            number(&after["extra_available"]) > 0,
            "day {day}: {after}"
        );
        if !extra.is_null() {
            let spelling = items(&extra)
                .iter()
                .filter(|i| i["card"]["kind"] == "spelling")
                .count();
            assert!(spelling <= 3, "day {day}: extra {extra}");
            // Leave the extra block unanswered: it expires at day rollover.
        }
        log.push(format!(
            "{day}: blocks {blocks} due {due_now} first-time {typed_today} new {new_today} waiting {}",
            after["typed_waiting"]
        ));
        day = day.add_days(1);
        env.set_day(&day.to_string());
    }
    println!("{log:#?}");
    assert!(
        first_time_cards(&env).is_empty(),
        "every first-time card came: {log:#?}"
    );
    assert!(spelling_served >= 8 + 10, "{log:#?}");
}
