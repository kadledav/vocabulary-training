//! Owner word changes against practice (design §6.4, §6.6, §6.7; R-04,
//! R-51, R-54): a suspended (or excluded) word leaves the open block and
//! comes back with Undo or unsuspend, Today's counts follow, "Reset card"
//! frees no new-word slot and cannot be undone by the practice Undo.
//! Synthetic words only.

mod common;

use axum::http::StatusCode;
use common::{Env, env, word};
use serde_json::{Value, json};

const BOOK: &str = "test-practice-book";

async fn id_of(env: &Env, headword: &str) -> i64 {
    env.word_id(headword).await.get()
}

async fn start(env: &Env, kind: &str) -> Value {
    let (status, body) = env.post("/api/sessions", &json!({ "kind": kind })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session"].clone()
}

async fn current(env: &Env) -> Value {
    let (status, body) = env.get("/api/sessions/current").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn headwords(session: &Value) -> Vec<String> {
    session["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|i| i["answered"].is_null())
        .map(|i| i["word"]["headword"].as_str().expect("h").to_owned())
        .collect()
}

async fn rate(env: &Env, session: &Value, n: u32, grade: u8) -> (StatusCode, Value) {
    let item = session["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["answered"].is_null())
        .expect("an unanswered item")["item_id"]
        .clone();
    env.post(
        &format!(
            "/api/sessions/{}/reviews",
            session["id"].as_str().expect("id")
        ),
        &json!({
            "client_review_id": format!("00000000-0000-4000-8000-{n:012}"),
            "item_id": item,
            "raw_grade": grade,
            "hint_used": false,
        }),
    )
    .await
}

async fn act(env: &Env, id: i64, body: Value) -> Value {
    let (status, view) = env.post(&format!("/api/words/{id}/actions"), &body).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view
}

async fn settings(env: &Env, body: Value) {
    let (status, answer) = env.request("PATCH", "/api/settings", Some(&body)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
}

/// Finding 5: suspending the word of an unanswered item takes the item out
/// of the open block (not shown, not rated, not counted); Undo and
/// unsuspend bring it back. Excluding a low-priority word does the same
/// for its new item.
#[tokio::test]
async fn r04_a_suspended_word_leaves_the_open_block_and_comes_back() {
    let env = env();
    let mut skip = word("dwindle", 4, 3);
    skip["tag"] = json!({ "kind": "low-priority" });
    env.import(BOOK, vec![word("lull", 9, 3), word("sift", 8, 3), skip])
        .await;
    let dwindle = id_of(&env, "dwindle").await;
    act(&env, dwindle, json!({ "action": "include" })).await;
    let session = start(&env, "start").await;
    assert_eq!(headwords(&session), ["lull", "sift", "dwindle"]);
    let (status, _) = rate(&env, &session, 1, 2).await;
    assert_eq!(status, StatusCode::OK);

    let sift = id_of(&env, "sift").await;
    let (status, view) = env
        .post(
            &format!("/api/words/{sift}/actions"),
            &json!({ "action": "suspend" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let token = view["undo"].as_str().expect("undo").to_owned();
    let block = current(&env).await;
    assert_eq!(headwords(&block), ["dwindle"]);
    assert_eq!(block["remaining"], 1);
    assert_eq!(block["total"], 2);
    let (_, today) = env.get("/api/today").await;
    assert_eq!(today["active_session"]["remaining"], 1, "{today}");
    assert_eq!(today["active_session"]["total"], 2);

    // The suspended word's item cannot be rated (it is not the next one).
    let sift_item = session["items"][1]["item_id"].clone();
    let (status, _) = env
        .post(
            &format!(
                "/api/sessions/{}/reviews",
                block["id"].as_str().expect("id")
            ),
            &json!({
                "client_review_id": "00000000-0000-4000-8000-000000000099",
                "item_id": sift_item,
                "raw_grade": 1,
                "hint_used": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Undo brings it back, in its place.
    let (status, view) = env
        .post(
            &format!("/api/words/{sift}/undo"),
            &json!({ "token": token }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(headwords(&current(&env).await), ["sift", "dwindle"]);

    // Excluding the low-priority word takes its new item out.
    act(&env, dwindle, json!({ "action": "exclude" })).await;
    assert_eq!(headwords(&current(&env).await), ["sift"]);
    act(&env, dwindle, json!({ "action": "include" })).await;
    assert_eq!(headwords(&current(&env).await), ["sift", "dwindle"]);

    // Suspend then unsuspend: practised as before.
    act(&env, sift, json!({ "action": "suspend" })).await;
    let (_, rows) = env.get("/api/words?sort=due").await;
    let rows = rows.as_array().expect("rows");
    assert_eq!(rows.last().expect("row")["headword"], "sift");
    let lull = rows.iter().find(|r| r["headword"] == "lull").expect("lull");
    assert!(!lull["next_due"].is_null());
    act(
        &env,
        id_of(&env, "lull").await,
        json!({ "action": "suspend" }),
    )
    .await;
    let (_, rows) = env.get("/api/words?sort=due").await;
    assert!(
        rows.as_array()
            .expect("rows")
            .iter()
            .all(|r| r["next_due"].is_null()),
        "a suspended word has no next due day: {rows}"
    );
    act(&env, sift, json!({ "action": "unsuspend" })).await;
    let block = current(&env).await;
    assert_eq!(headwords(&block), ["sift", "dwindle"]);
    let (status, _) = rate(&env, &block, 2, 2).await;
    assert_eq!(status, StatusCode::OK);
}

/// Finding 7: "Reset card" keeps the day's new-word slot the card used.
#[tokio::test]
async fn r54_resetting_a_card_frees_no_new_word_slot() {
    let env = env();
    settings(&env, json!({ "new_per_day": 3, "block_size": 10 })).await;
    env.import(
        BOOK,
        ["lull", "sift", "dwindle", "brontle"]
            .iter()
            .map(|h| word(h, 7, 3))
            .collect(),
    )
    .await;
    let session = start(&env, "start").await;
    assert_eq!(headwords(&session).len(), 3);
    for n in 1..=3 {
        let block = current(&env).await;
        let (status, _) = rate(&env, &block, n, 2).await;
        assert_eq!(status, StatusCode::OK);
    }
    let first = id_of(&env, "lull").await;
    act(
        &env,
        first,
        json!({ "action": "reset_card", "kind": "understand" }),
    )
    .await;
    let (_, today) = env.get("/api/today").await;
    assert_eq!(today["new_words"]["introduced_today"], 3, "{today}");
    assert_eq!(today["new_left_today"], 0, "{today}");
    let next = start(&env, "continue").await;
    assert!(
        next.is_null() || !headwords(&next).iter().any(|h| h == "brontle"),
        "no fourth new word today: {next}"
    );

    // The next day the reset word is new again and counts then.
    env.set_day("2026-11-21");
    let (_, today) = env.get("/api/today").await;
    assert_eq!(today["new_left_today"], 3, "{today}");
    let block = start(&env, "start").await;
    let lull_at = headwords(&block)
        .iter()
        .position(|h| h == "lull")
        .expect("lull is new again");
    assert!(lull_at < 3);
}

/// Author's edge case (a): resetting a card that was rated in the open
/// block, then the practice Undo: refused, the reset stays.
#[tokio::test]
async fn r54_practice_undo_does_not_undo_a_reset() {
    let env = env();
    env.import(BOOK, vec![word("lull", 9, 3), word("sift", 8, 3)])
        .await;
    let session = start(&env, "start").await;
    let (status, _) = rate(&env, &session, 1, 2).await;
    assert_eq!(status, StatusCode::OK);
    let id = id_of(&env, "lull").await;
    let view = act(
        &env,
        id,
        json!({ "action": "reset_card", "kind": "understand" }),
    )
    .await;
    assert_eq!(view["cards"][0]["state"], "new");
    let (status, body) = env
        .post(
            &format!("/api/sessions/{}/undo", session["id"].as_str().expect("id")),
            &json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "card_changed");
    let (_, view) = env.get(&format!("/api/words/{id}")).await;
    assert_eq!(view["cards"][0]["state"], "new");
    assert_eq!(view["cards"][0]["reps"], 0);
}

/// "Reset card" of a card with an unanswered item in the open block takes
/// the item out (it was built for the old state).
#[tokio::test]
async fn r04_a_reset_card_leaves_the_open_block() {
    let env = env();
    env.import(BOOK, vec![word("lull", 9, 3), word("sift", 8, 3)])
        .await;
    let session = start(&env, "start").await;
    let (status, _) = rate(&env, &session, 1, 5).await;
    assert_eq!(status, StatusCode::OK);
    // A repeat of lull is queued now; resetting the card withdraws it.
    let block = current(&env).await;
    assert_eq!(headwords(&block), ["sift", "lull"]);
    act(
        &env,
        id_of(&env, "lull").await,
        json!({ "action": "reset_card", "kind": "understand" }),
    )
    .await;
    assert_eq!(headwords(&current(&env).await), ["sift"]);
}
