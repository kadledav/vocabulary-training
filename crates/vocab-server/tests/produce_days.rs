//! Many study days with a fake clock (design §17 M8, R-52, R-54): 100
//! synthetic words unlock their Produce card on the same day; with a cap of
//! 10 first-time cards a day and one block a day, every day's Today counts
//! equal what the block serves, no day ends in a dead end, due reviews
//! always come before first-time Produce cards, and all 100 are introduced
//! within the expected days.

mod common;

use std::collections::HashSet;

use axum::http::StatusCode;
use common::{Env, env, word};
use serde_json::{Value, json};

const WORDS: usize = 100;
const CAP: u64 = 10;
/// Ten days at ten a day, plus two days for cards held back because their
/// word had a review that day (one card per word a day).
const EXPECTED_DAYS: usize = 12;

static CLIENT_NO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn client_id() -> String {
    let n = CLIENT_NO.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("00000000-0000-4000-b000-{n:012}")
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

/// Produce cards that still wait for their first review.
fn first_time_cards(env: &Env) -> HashSet<i64> {
    let conn = env.sql();
    let mut stmt = conn
        .prepare("SELECT id FROM card WHERE kind = 'produce' AND state = 'new'")
        .expect("prepare");
    stmt.query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// A spread of ratings: mostly 2, some 1 and 3, and a 4 now and then on a
/// review (which repeats it in the block).
fn grade(item: &Value, day_no: usize) -> u8 {
    let card = usize::try_from(number(&item["card"]["id"])).expect("id");
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

/// Rate every item of the block (repeats included).
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
        if item["card"]["kind"] == "produce" {
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
}

#[tokio::test]
async fn r52_a_hundred_produce_cards_unlocked_on_one_day_come_ten_a_day_without_a_dead_end() {
    let env = env();
    let words: Vec<Value> = (1..=WORDS)
        .map(|i| {
            let mut w = word(
                &format!("sim{i:03}"),
                4 + u8::try_from(i % 6).expect("u8"),
                3,
            );
            w["goal"] = json!("active");
            w
        })
        .collect();
    env.import("test-novel", words).await;
    // Every Understand card is known, with reviews spread over the next
    // 30 days (some due on the unlock day itself); every Produce card is
    // unlocked on 2026-11-20 and waits for its first review.
    env.exec(
        "UPDATE card SET state = 'review', stability = 40.0, difficulty = 5.0, \
         due_day = date('2026-11-20', '+' || (word_id % 30) || ' days'), \
         last_review_day = '2026-10-20', introduced_day = '2026-09-01', reps = 5, format = 'sentence' \
         WHERE kind = 'understand'; \
         INSERT INTO card (word_id, kind, state, due_day, created_at) \
         SELECT id, 'produce', 'new', '2026-11-20', '2026-11-20T00:00:00Z' FROM word;",
    );
    let (status, body) = env
        .request(
            "PATCH",
            "/api/settings",
            Some(&json!({ "block_size": 50, "new_typed_per_day": CAP })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let mut introduced = 0u64;
    let mut log = Vec::new();
    let mut day = env.today();
    for day_no in 0..20 {
        let before = today(&env).await;
        let due_now = number(&before["due_now"]);
        let planned = number(&before["next_block"]["due"]) + number(&before["next_block"]["new"]);
        let promised_first = number(&before["typed_today"]);
        assert!(due_now <= 50, "the test needs one block a day: {before}");
        assert!(promised_first <= CAP, "{before}");
        let first_time = first_time_cards(&env);
        let session = start(&env, "start").await;
        if planned == 0 {
            assert_eq!(due_now, 0, "day {day}: {before}");
            assert!(
                session.is_null(),
                "day {day}: Today said nothing, Start built {session}"
            );
        } else {
            assert!(
                !session.is_null(),
                "day {day}: Today said {planned}, Start built nothing"
            );
            let served = items(&session);
            assert_eq!(served.len() as u64, planned, "day {day}: {before}");
            assert_eq!(
                due_now, planned,
                "day {day}: no new words, all due: {before}"
            );
            let is_first = |i: &Value| {
                i["card"]["kind"] == "produce"
                    && first_time.contains(&i["card"]["id"].as_i64().expect("id"))
            };
            let firsts = served.iter().filter(|i| is_first(i)).count() as u64;
            assert_eq!(
                firsts, promised_first,
                "day {day}: Today's first-time count"
            );
            if let Some(first) = served.iter().position(is_first) {
                assert!(
                    served[first..].iter().all(is_first),
                    "day {day}: a review came after a first-time card"
                );
            }
            rate_block(&env, session, day_no).await;
        }
        let after = today(&env).await;
        let came = number(&after["new_typed"]["introduced_today"]);
        assert_eq!(came, promised_first, "day {day}: {after}");
        introduced += came;
        // The day is done: nothing due, Start builds nothing, and extra
        // practice is there exactly when Today says so.
        assert_eq!(number(&after["due_now"]), 0, "day {day}: {after}");
        assert_eq!(
            after["next_block"],
            json!({ "due": 0, "new": 0 }),
            "day {day}"
        );
        assert!(start(&env, "continue").await.is_null(), "day {day}");
        let extra = start(&env, "extra").await;
        assert_eq!(
            !extra.is_null(),
            number(&after["extra_available"]) > 0,
            "day {day}: {after}"
        );
        log.push(format!(
            "{day}: due {due_now} first-time {came} waiting {}",
            after["typed_waiting"]
        ));
        // While enough wait, the cap is used in full (a few may be held
        // back because their word has a review that day).
        if number(&before["typed_waiting"]) >= CAP + 5 {
            assert_eq!(came, CAP, "day {day}: {before}");
        }
        day = day.add_days(1);
        env.set_day(&day.to_string());
    }
    println!("{log:#?}");
    assert_eq!(introduced, WORDS as u64, "{log:#?}");
    let first_day_all_done = env.int(
        "SELECT CAST(julianday(MAX(introduced_day)) - julianday('2026-11-20') AS INTEGER) \
         FROM card WHERE kind = 'produce'",
    );
    assert!(
        usize::try_from(first_day_all_done).expect("days") < EXPECTED_DAYS,
        "all introduced within {EXPECTED_DAYS} days: {log:#?}"
    );
}
