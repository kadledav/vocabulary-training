//! Progress view (R-72) and the leech actions (R-55), through the HTTP API
//! (design §6.5, §7.1 screen 9, §8). The fixture histories are written
//! straight into the database, so every expected number below is worked
//! out by hand from what the test stores.

mod common;

use axum::http::StatusCode;
use common::{Env, env, word};
use serde_json::{Value, json};

const BOOK_A: &str = "test-progress-a";
const BOOK_B: &str = "test-progress-b";

/// Set the Understand card of `headword`.
#[allow(clippy::too_many_arguments)] // a test fixture row, named at the call
fn understand(
    env: &Env,
    headword: &str,
    stability: f64,
    due: &str,
    introduced: &str,
    reps: u32,
    lapses: u32,
    leech: &str,
) {
    env.sql()
        .execute(
            "UPDATE card SET state = 'review', format = 'sentence', stability = ?1, \
             difficulty = 5.0, due_day = ?2, last_review_day = '2026-11-19', introduced_day = ?3, \
             reps = ?4, lapses = ?5, leech = ?6 \
             WHERE kind = 'understand' AND word_id = (SELECT id FROM word WHERE headword = ?7)",
            rusqlite::params![stability, due, introduced, reps, lapses, leech, headword],
        )
        .expect("update card");
}

fn card_id(env: &Env, headword: &str, kind: &str) -> i64 {
    env.sql()
        .query_row(
            "SELECT c.id FROM card c JOIN word w ON w.id = c.word_id \
             WHERE w.headword = ?1 AND c.kind = ?2",
            [headword, kind],
            |row| row.get(0),
        )
        .expect("card id")
}

/// One review row: `before` is the card's state before it.
fn review(env: &Env, card: i64, day: &str, grade: u8, elapsed: u32, before: &str, undone: bool) {
    let n: i64 = env.int("SELECT COUNT(*) FROM review_log");
    env.sql()
        .execute(
            "INSERT INTO review_log (client_review_id, card_id, reviewed_at, review_day, raw_grade, \
             fsrs_rating, format, elapsed_days, before_json, undone) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'sentence', ?7, ?8, ?9)",
            rusqlite::params![
                format!("test-fixture-review-{n}"),
                card,
                format!("{day}T09:00:00Z"),
                day,
                grade,
                if grade >= 4 { 1 } else { 3 },
                elapsed,
                json!({ "state": before }).to_string(),
                undone,
            ],
        )
        .expect("insert review");
}

/// Six words in two sources with hand-made histories (study day
/// 2026-11-20; the 30-day window is 2026-10-22 … 2026-11-20).
async fn fixture() -> Env {
    let env = env();
    env.import(
        BOOK_A,
        vec![
            word("alpha", 5, 3),
            word("bravo", 6, 3),
            word("charlie", 8, 3),
            word("delta", 8, 3),
            word("echo", 7, 3),
            word("foxtrot", 4, 3),
        ],
    )
    .await;
    env.import(BOOK_B, vec![word("bravo", 6, 3), word("charlie", 8, 3)])
        .await;
    // alpha: new (untouched). bravo: learning. charlie: familiar. delta:
    // known, and a Produce card remembered for 8 days. echo: a problem
    // word, due today. foxtrot: suspended.
    understand(&env, "bravo", 3.0, "2026-11-21", "2026-11-15", 2, 0, "none");
    understand(
        &env,
        "charlie",
        10.0,
        "2026-11-23",
        "2026-10-01",
        6,
        1,
        "none",
    );
    understand(
        &env,
        "delta",
        30.0,
        "2026-11-30",
        "2026-10-01",
        5,
        0,
        "none",
    );
    understand(
        &env,
        "echo",
        2.0,
        "2026-11-20",
        "2026-11-02",
        9,
        4,
        "flagged",
    );
    understand(
        &env,
        "foxtrot",
        50.0,
        "2026-11-22",
        "2026-10-01",
        5,
        0,
        "none",
    );
    env.exec("UPDATE word SET suspended = 1 WHERE headword = 'foxtrot'");
    env.exec(
        "INSERT INTO card (word_id, kind, format, state, stability, difficulty, due_day, \
         last_review_day, introduced_day, reps, created_at) \
         SELECT id, 'produce', 'cloze', 'review', 8.0, 5.0, '2026-11-21', '2026-11-18', \
         '2026-11-14', 3, '2026-11-13T09:00:00Z' FROM word WHERE headword = 'delta'",
    );
    let (bravo, charlie, delta, delta_p) = (
        card_id(&env, "bravo", "understand"),
        card_id(&env, "charlie", "understand"),
        card_id(&env, "delta", "understand"),
        card_id(&env, "delta", "produce"),
    );
    // charlie: 4 recall tests (2, 4, 2, 3 → 3 passed), a same-day repeat,
    // an undone rating and one before the window.
    review(&env, charlie, "2026-10-21", 5, 3, "review", false);
    review(&env, charlie, "2026-11-01", 2, 5, "review", false);
    review(&env, charlie, "2026-11-06", 4, 5, "review", false);
    review(&env, charlie, "2026-11-06", 2, 0, "relearning", false);
    review(&env, charlie, "2026-11-07", 2, 1, "relearning", false);
    review(&env, charlie, "2026-11-12", 3, 5, "review", false);
    review(&env, charlie, "2026-11-13", 5, 1, "review", true);
    // delta: 2 tests, both passed.
    review(&env, delta, "2026-11-10", 1, 9, "review", false);
    review(&env, delta, "2026-11-19", 2, 9, "review", false);
    // bravo: its first review (no test), then a failed test.
    review(&env, bravo, "2026-11-15", 3, 0, "new", false);
    review(&env, bravo, "2026-11-18", 5, 3, "review", false);
    // delta's Produce card: 3 tests, 1 passed.
    review(&env, delta_p, "2026-11-14", 2, 4, "review", false);
    review(&env, delta_p, "2026-11-17", 4, 3, "review", false);
    review(&env, delta_p, "2026-11-18", 5, 1, "relearning", false);
    env
}

fn counts(value: &Value) -> [i64; 8] {
    [
        "words",
        "new",
        "learning",
        "familiar",
        "known",
        "problem",
        "suspended",
        "can_produce",
    ]
    .map(|k| {
        value[k]
            .as_i64()
            .unwrap_or_else(|| panic!("{k} in {value}"))
    })
}

#[tokio::test]
async fn r72_progress_numbers_match_the_hand_computed_fixture() {
    let env = fixture().await;
    let (status, p) = env.get("/api/progress").await;
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(p["study_day"], "2026-11-20");

    // Due: echo today; bravo and delta's Produce card tomorrow, charlie on
    // the 23rd, delta on the 30th; the suspended foxtrot never.
    assert_eq!(p["due_now"], 1);
    let forecast: Vec<(String, i64)> = p["forecast"]
        .as_array()
        .expect("forecast")
        .iter()
        .map(|f| {
            (
                f["day"].as_str().expect("day").to_owned(),
                f["due"].as_i64().expect("due"),
            )
        })
        .collect();
    assert_eq!(forecast.len(), 14);
    assert_eq!(forecast[0], ("2026-11-21".to_owned(), 2));
    assert_eq!(forecast[2], ("2026-11-23".to_owned(), 1));
    assert_eq!(forecast[9], ("2026-11-30".to_owned(), 1));
    assert_eq!(forecast.iter().map(|f| f.1).sum::<i64>(), 4);
    assert_eq!(p["due_next_7_days"], 3);

    // Statuses partition the words.
    assert_eq!(counts(&p["totals"]), [6, 1, 1, 1, 1, 1, 1, 1]);
    let sources = p["sources"].as_array().expect("sources");
    let source = |slug: &str| {
        sources
            .iter()
            .find(|s| s["title"] == format!("Title of {slug}"))
            .unwrap_or_else(|| panic!("{slug} in {sources:?}"))
    };
    assert_eq!(counts(source(BOOK_A)), [6, 1, 1, 1, 1, 1, 1, 1]);
    assert_eq!(counts(source(BOOK_B)), [2, 0, 1, 1, 0, 0, 0, 0]);
    assert_eq!(source(BOOK_A)["reading_position"], "Chapter 3");

    // Retention: Understand 7 tests (charlie 4, delta 2, bravo 1), 5
    // passed; target (6 × 0.93 + 0.90) / 7 = 0.926. Produce 3 tests, 1
    // passed, target 0.93. No Spelling cards: no Spelling line.
    let r = &p["retention"];
    assert_eq!(r["from"], "2026-10-22");
    assert_eq!(r["to"], "2026-11-20");
    let kinds = r["kinds"].as_array().expect("kinds");
    assert_eq!(kinds.len(), 2, "{kinds:?}");
    assert_eq!(kinds[0]["kind"], "understand");
    assert_eq!(kinds[0]["tested"], 7);
    assert_eq!(kinds[0]["passed"], 5);
    assert!((kinds[0]["measured"].as_f64().expect("measured") - 5.0 / 7.0).abs() < 1e-9);
    assert_eq!(kinds[0]["target"], 0.926);
    assert_eq!(kinds[0]["few"], true);
    assert_eq!(kinds[1]["kind"], "produce");
    assert_eq!(kinds[1]["tested"], 3);
    assert_eq!(kinds[1]["passed"], 1);
    assert_eq!(kinds[1]["target"], 0.93);

    // Activity: every day of the window; 12 ratings (the undone one and
    // the one before the window left out), 2 new words (echo on 11-02,
    // bravo on 11-15).
    let a = &p["activity"];
    let days = a["days"].as_array().expect("days");
    assert_eq!(days.len(), 30);
    assert_eq!(days[0]["day"], "2026-10-22");
    assert_eq!(days[29]["day"], "2026-11-20");
    assert_eq!(a["reviews"], 12);
    assert_eq!(a["new_words"], 2);
    let on = |d: &str| days.iter().find(|x| x["day"] == d).expect("day");
    assert_eq!(
        on("2026-11-06")["reviews"],
        2,
        "the same-day repeat is a rating"
    );
    assert_eq!(on("2026-11-13")["reviews"], 0, "undone");
    assert_eq!(on("2026-11-18")["reviews"], 2);
    assert_eq!(on("2026-11-02")["new_words"], 1);
    assert_eq!(on("2026-11-15")["new_words"], 1);

    // The problem word, why it is one, and nothing else.
    let leeches = p["leeches"].as_array().expect("leeches");
    assert_eq!(leeches.len(), 1);
    assert_eq!(leeches[0]["headword"], "echo");
    assert_eq!(leeches[0]["flagged"], true);
    assert_eq!(leeches[0]["goal"], "active");
    assert_eq!(leeches[0]["hint"], Value::Null);
    assert_eq!(
        leeches[0]["cards"],
        json!([{ "kind": "understand", "reason": { "kind": "lapses", "lapses": 4 },
                 "lapses": 4, "stability": 2.0, "last_review_day": "2026-11-19" }])
    );
    assert_eq!(p["conflicts"], json!([]));

    // Today links to the list (one problem word).
    let (_, today) = env.get("/api/today").await;
    assert_eq!(today["problem_words"], 1);
}

#[tokio::test]
async fn r72_progress_of_an_empty_app() {
    let env = env();
    let (status, p) = env.get("/api/progress").await;
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(counts(&p["totals"]), [0; 8]);
    assert_eq!(p["sources"], json!([]));
    assert_eq!(p["retention"]["kinds"], json!([]));
    assert_eq!(p["activity"]["days"].as_array().map(Vec::len), Some(30));
    assert_eq!(p["leeches"], json!([]));
}

#[tokio::test]
async fn r72_open_pack_suggestions_are_listed() {
    let env = env();
    env.import(BOOK_A, vec![word("golf", 5, 3)]).await;
    let mut changed = word("golf", 5, 3);
    changed["meaning"] = json!("Another invented meaning.");
    env.import(BOOK_A, vec![changed]).await;
    let (_, p) = env.get("/api/progress").await;
    let conflicts = p["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 1, "{p}");
    assert_eq!(conflicts[0]["headword"], "golf");
    assert_eq!(conflicts[0]["open"], 1);
}

// ── leeches (R-55, design §6.5) ─────────────────────────────────────────────

async fn flagged_word(env: &Env, headword: &str, usefulness: u8) -> i64 {
    env.import(BOOK_A, vec![word(headword, usefulness, 3)])
        .await;
    understand(
        env,
        headword,
        2.0,
        "2026-11-20",
        "2026-11-01",
        8,
        5,
        "flagged",
    );
    env.word_id(headword).await.get()
}

async fn action(env: &Env, id: i64, body: Value) -> (StatusCode, Value) {
    env.post(&format!("/api/words/{id}/actions"), &body).await
}

fn understand_card(view: &Value) -> &Value {
    view["cards"]
        .as_array()
        .expect("cards")
        .iter()
        .find(|c| c["kind"] == "understand")
        .expect("understand card")
}

async fn leech_list(env: &Env) -> Vec<Value> {
    let (_, p) = env.get("/api/progress").await;
    p["leeches"].as_array().expect("leeches").clone()
}

#[tokio::test]
async fn r55_keep_practising_acknowledges_and_counts_lapses_from_now() {
    let env = env();
    let id = flagged_word(&env, "hotel", 7).await;
    let (status, view) = action(&env, id, json!({ "action": "leech_keep" })).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let card = understand_card(&view);
    assert_eq!(card["leech"], "acknowledged");
    assert_eq!(card["leech_lapses_at_ack"], 5);
    assert!(view["undo"].is_string(), "an acknowledgement can be undone");
    // Still listed (not yet remembered for three weeks), not flagged.
    let list = leech_list(&env).await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["flagged"], false);
    assert_eq!(
        list[0]["cards"][0]["reason"],
        json!({ "kind": "acknowledged" })
    );
    // Undo brings the flag back.
    let token = view["undo"].as_str().expect("token").to_owned();
    let (status, view) = env
        .post(&format!("/api/words/{id}/undo"), &json!({ "token": token }))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(understand_card(&view)["leech"], "flagged");
    // Remembered for three weeks: off the list (the flag stays recorded).
    action(&env, id, json!({ "action": "leech_keep" })).await;
    env.exec("UPDATE card SET stability = 21.0");
    assert!(leech_list(&env).await.is_empty());
    let (_, today) = env.get("/api/today").await;
    assert_eq!(today["problem_words"], 0);
}

#[tokio::test]
async fn r55_recognise_only_suspends_the_produce_card() {
    let env = env();
    let id = flagged_word(&env, "india", 8).await;
    env.exec(
        "INSERT INTO card (word_id, kind, format, state, stability, difficulty, due_day, reps, created_at) \
         SELECT id, 'produce', 'cloze', 'review', 3.0, 6.0, '2026-11-25', 2, '2026-11-10T09:00:00Z' \
         FROM word WHERE headword = 'india'",
    );
    let (status, view) = action(&env, id, json!({ "action": "leech_recognise" })).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["effective_goal"], "recognise");
    assert!(
        view["overridden"]
            .as_array()
            .expect("overridden")
            .contains(&json!("goal"))
    );
    let produce = view["cards"]
        .as_array()
        .expect("cards")
        .iter()
        .find(|c| c["kind"] == "produce")
        .expect("produce");
    assert_eq!(produce["suspended"], true);
    assert_eq!(understand_card(&view)["leech"], "acknowledged");
}

#[tokio::test]
async fn r55_lower_usefulness_only_lowers() {
    let env = env();
    let id = flagged_word(&env, "juliett", 7).await;
    for bad in [json!(7), json!(8), json!(0)] {
        let (status, body) = action(
            &env,
            id,
            json!({ "action": "leech_usefulness", "usefulness": bad }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {body}");
    }
    assert_eq!(
        env.int("SELECT COUNT(*) FROM card WHERE leech = 'flagged'"),
        1,
        "a refused choice acknowledges nothing"
    );
    let (status, view) = action(
        &env,
        id,
        json!({ "action": "leech_usefulness", "usefulness": 3 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["usefulness"], 3);
    assert_eq!(understand_card(&view)["leech"], "acknowledged");
    // Already at the bottom.
    env.exec("UPDATE word SET usefulness = 1");
    let (status, body) = action(
        &env,
        id,
        json!({ "action": "leech_usefulness", "usefulness": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // A usefulness only goes with this action.
    let (status, _) = action(&env, id, json!({ "action": "leech_keep", "usefulness": 2 })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = action(&env, id, json!({ "action": "leech_usefulness" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn r55_suspend_takes_the_word_off_the_list() {
    let env = env();
    let id = flagged_word(&env, "kilo", 3).await;
    let (status, view) = action(&env, id, json!({ "action": "leech_suspend" })).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["suspended"], true);
    assert_eq!(understand_card(&view)["leech"], "acknowledged");
    assert!(leech_list(&env).await.is_empty());
    let (_, p) = env.get("/api/progress").await;
    assert_eq!(p["totals"]["suspended"], 1);
    assert_eq!(p["totals"]["problem"], 0);
}

#[tokio::test]
async fn r55_two_fives_reason_and_unknown_word() {
    let env = env();
    let id = flagged_word(&env, "lima", 6).await;
    env.exec("UPDATE card SET lapses = 1");
    let list = leech_list(&env).await;
    assert_eq!(
        list[0]["cards"][0]["reason"],
        json!({ "kind": "forgot_when_known" })
    );
    let (status, _) = action(&env, id + 1000, json!({ "action": "leech_keep" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn r55_practice_item_carries_the_flag_until_acknowledged() {
    let env = env();
    let id = flagged_word(&env, "mike", 6).await;
    let (status, start) = env.post("/api/sessions", &json!({ "kind": "start" })).await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let item = &start["session"]["items"][0];
    assert_eq!(item["word"]["headword"], "mike");
    assert_eq!(item["card"]["leech_flagged"], true);
    action(&env, id, json!({ "action": "leech_keep" })).await;
    let (_, current) = env.get("/api/sessions/current").await;
    assert_eq!(current["items"][0]["card"]["leech_flagged"], false);
}
