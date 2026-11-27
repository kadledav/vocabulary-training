//! Undo of the owner's word changes (design §8 `POST /api/words/{id}/undo`;
//! R-03, R-04, R-25): the server restores the exact state before the change
//! (override rows, `edited` flags, removed items, suggestions, cards), so a
//! later pack is treated as if the change had never happened; an Undo that
//! would have to guess is refused with 409. Synthetic words only.

mod common;

use axum::http::StatusCode;
use common::{Env, env, word};
use serde_json::{Value, json};

const BOOK: &str = "test-undo-book";

/// Every row the catalog and the scheduler hold about word `id`.
fn state(env: &Env, id: i64) -> Vec<String> {
    let mut out = env.rows("word", &format!("id = {id}"));
    for table in [
        "word_source",
        "word_item",
        "word_form",
        "word_override",
        "field_conflict",
        "word_pack_spelling",
        "sentence",
        "card",
    ] {
        out.push(format!("── {table}"));
        out.extend(env.rows(table, &format!("word_id = {id}")));
    }
    out
}

async fn id_of(env: &Env, headword: &str) -> i64 {
    env.word_id(headword).await.get()
}

/// Send a change; it must succeed and offer an Undo. Returns the token.
async fn change(env: &Env, method: &str, uri: &str, body: Value) -> String {
    let (status, view) = env.request(method, uri, Some(&body)).await;
    assert_eq!(status, StatusCode::OK, "{method} {uri}: {view}");
    view["undo"]
        .as_str()
        .unwrap_or_else(|| panic!("{method} {uri} offers an undo: {view}"))
        .to_owned()
}

async fn undo(env: &Env, id: i64, token: &str) -> (StatusCode, Value) {
    env.post(&format!("/api/words/{id}/undo"), &json!({ "token": token }))
        .await
}

async fn undo_ok(env: &Env, id: i64, token: &str) -> Value {
    let (status, view) = undo(env, id, token).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(view.get("undo").is_none(), "an undo is not undone again");
    view
}

fn lull() -> Value {
    let mut w = word("lull", 7, 3);
    w["czech"] = json!(["ukolébat", "utišit"]);
    w["accepted"] = json!(["lulls"]);
    w["forms"] = json!([{ "form": "lulled", "common": true }]);
    w
}

/// Every change that offers an Undo comes back to the exact state before.
#[tokio::test]
async fn r04_undo_restores_the_exact_state_of_every_change() {
    let env = env();
    env.import(BOOK, vec![lull(), word("sift", 6, 3)]).await;
    let id = id_of(&env, "lull").await;
    let mut pack = lull();
    pack["meaning"] = json!("A later pack's meaning.");
    patch_ok(&env, id, json!({ "meaning": "Owner meaning." })).await;
    env.import(BOOK, vec![pack]).await;
    let (_, view) = env.get(&format!("/api/words/{id}")).await;
    let conflict = view["conflicts"][0]["id"].as_i64().expect("conflict");
    let sentence = view["sentences"][0]["id"].as_i64().expect("sentence");
    let word = format!("/api/words/{id}");
    let changes: Vec<(&str, String, Value)> = vec![
        (
            "PATCH",
            word.clone(),
            json!({ "headword": "lull to sleep" }),
        ),
        (
            "PATCH",
            word.clone(),
            json!({ "usefulness": 9, "hint": "A hint." }),
        ),
        ("PATCH", word.clone(), json!({ "goal": "recognise" })),
        ("PATCH", word.clone(), json!({ "spelling": true })),
        (
            "POST",
            format!("{word}/actions"),
            json!({ "action": "suspend" }),
        ),
        (
            "POST",
            format!("{word}/items"),
            json!({ "kind": "accepted", "value": "lulling" }),
        ),
        (
            "POST",
            format!("{word}/items"),
            json!({ "kind": "form", "value": "lulled", "common": false }),
        ),
        (
            "DELETE",
            format!("{word}/items"),
            json!({ "kind": "czech", "key": "utišit" }),
        ),
        (
            "DELETE",
            format!("{word}/items"),
            json!({ "kind": "form", "key": "lulled" }),
        ),
        (
            "PATCH",
            format!("/api/sentences/{sentence}"),
            json!({ "en": "An [[edited]] test line.", "cs": "[[Upravený]] testovací řádek." }),
        ),
        (
            "POST",
            format!("/api/sentences/{sentence}/hidden"),
            json!({ "hidden": true }),
        ),
        (
            "POST",
            format!("/api/conflicts/{conflict}"),
            json!({ "use": "incoming" }),
        ),
        (
            "POST",
            format!("/api/conflicts/{conflict}"),
            json!({ "use": "keep" }),
        ),
    ];
    for (method, uri, body) in changes {
        let before = state(&env, id);
        let token = change(&env, method, &uri, body.clone()).await;
        assert_ne!(
            state(&env, id),
            before,
            "{method} {uri} {body} changed something"
        );
        undo_ok(&env, id, &token).await;
        assert_eq!(state(&env, id), before, "{method} {uri} {body}");
    }
    // Unchanged: no Undo token (and the previous one stays usable).
    let token = change(&env, "PATCH", &word, json!({ "usefulness": 3 })).await;
    let (status, view) = env
        .request("PATCH", &word, Some(&json!({ "usefulness": 3 })))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(view["undo"].is_null(), "{view}");
    undo_ok(&env, id, &token).await;
}

async fn patch_ok(env: &Env, id: i64, body: Value) -> Value {
    let (status, view) = env
        .request("PATCH", &format!("/api/words/{id}"), Some(&body))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view
}

/// Finding 3: after an undone sentence edit, a pack correction of the
/// sentence applies again; after an undone field set, a pack fills the
/// empty field instead of only suggesting; an undone usefulness edit
/// leaves no "edited" mark.
#[tokio::test]
async fn r03_after_an_undo_later_packs_are_treated_as_before() {
    let env = env();
    env.import(BOOK, vec![word("lull", 7, 3)]).await;
    let id = id_of(&env, "lull").await;
    let (_, view) = env.get(&format!("/api/words/{id}")).await;
    let sentence = view["sentences"][0]["id"].as_i64().expect("sentence");

    let token = change(
        &env,
        "PATCH",
        &format!("/api/sentences/{sentence}"),
        json!({ "en": "Test line 1 with the [[lull]] in it.", "cs": "Opravený řádek s [[lull]]." }),
    )
    .await;
    undo_ok(&env, id, &token).await;
    let token = change(
        &env,
        "PATCH",
        &format!("/api/words/{id}"),
        json!({ "grammar": "verb" }),
    )
    .await;
    undo_ok(&env, id, &token).await;
    let token = change(
        &env,
        "PATCH",
        &format!("/api/words/{id}"),
        json!({ "usefulness": 2 }),
    )
    .await;
    let view = undo_ok(&env, id, &token).await;
    assert_eq!(view["overridden"], json!([]));
    assert!(view["grammar"].is_null());
    assert_eq!(view["sentences"][0]["edited"], false);

    let mut pack = word("lull", 7, 3);
    pack["grammar"] = json!("noun");
    pack["sentences"][0]["cs"] = json!("Opravený testovací řádek 1 s [[lull]].");
    let report = env.import(BOOK, vec![pack]).await;
    assert_eq!(
        report["packs"][0]["summary"]["updated_sentences"], 1,
        "{report}"
    );
    let (_, view) = env.get(&format!("/api/words/{id}")).await;
    assert_eq!(view["grammar"], "noun", "the empty field is filled");
    assert_eq!(view["conflicts"], json!([]), "{view}");
    assert_eq!(
        view["sentences"][0]["cs"]["pre"],
        "Opravený testovací řádek 1 s "
    );
}

/// An Undo that would have to guess is refused with 409: after another
/// change (also from another tab), twice, after an import or a deletion,
/// and when it is too old.
#[tokio::test]
async fn r04_an_undo_that_would_guess_is_refused() {
    let env = env();
    env.import(BOOK, vec![lull(), word("sift", 6, 3)]).await;
    let id = id_of(&env, "lull").await;
    let uri = format!("/api/words/{id}");
    let refused = |status: StatusCode, body: &Value| {
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"]["code"], "undo_unavailable", "{body}");
    };

    // Another change after it (this tab or another one).
    let first = change(&env, "PATCH", &uri, json!({ "usefulness": 9 })).await;
    let second = change(&env, "PATCH", &uri, json!({ "grammar": "verb" })).await;
    let (status, body) = undo(&env, id, &first).await;
    refused(status, &body);
    // Twice.
    undo_ok(&env, id, &second).await;
    let (status, body) = undo(&env, id, &second).await;
    refused(status, &body);

    // The word changed by an import since.
    let token = change(&env, "PATCH", &uri, json!({ "usefulness": 8 })).await;
    let mut pack = lull();
    pack["czech"] = json!(["ukolébat", "uspat"]);
    env.import(BOOK, vec![pack]).await;
    let (status, body) = undo(&env, id, &token).await;
    refused(status, &body);
    assert_eq!(
        env.int(&format!("SELECT usefulness FROM word WHERE id = {id}")),
        8
    );

    // Too old.
    let token = change(&env, "PATCH", &uri, json!({ "usefulness": 5 })).await;
    env.set_time("2026-11-20T09:11:00Z");
    let (status, body) = undo(&env, id, &token).await;
    refused(status, &body);

    // Of a deleted word (and its id is never reused).
    let token = change(&env, "PATCH", &uri, json!({ "usefulness": 4 })).await;
    let (status, _) = env.request("DELETE", &uri, Some(&json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    env.import(BOOK, vec![word("brontle", 5, 3)]).await;
    let (status, body) = undo(&env, id, &token).await;
    refused(status, &body);

    // Another word's token, and a token that is no token.
    let sift = id_of(&env, "sift").await;
    let token = change(
        &env,
        "PATCH",
        &format!("/api/words/{sift}"),
        json!({ "usefulness": 9 }),
    )
    .await;
    let brontle = id_of(&env, "brontle").await;
    let (status, body) = undo(&env, brontle, &token).await;
    refused(status, &body);
    let (status, _) = undo(&env, sift, "not-a-token").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    undo_ok(&env, sift, &token).await;
}

/// An Undo that would give a word back a spelling another word took since
/// is refused, naming the word that has it.
#[tokio::test]
async fn r04_an_undo_does_not_take_a_spelling_another_word_has_now() {
    let env = env();
    env.import(BOOK, vec![lull(), word("sift", 6, 3)]).await;
    let id = id_of(&env, "lull").await;
    let sift = id_of(&env, "sift").await;
    patch_ok(&env, id, json!({ "headword": "lul" })).await;
    let token = change(
        &env,
        "PATCH",
        &format!("/api/words/{id}"),
        json!({ "headword": "lull to sleep" }),
    )
    .await;
    patch_ok(&env, sift, json!({ "headword": "lul" })).await;
    let (status, body) = undo(&env, id, &token).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "headword_taken");
    let (_, view) = env.get(&format!("/api/words/{id}")).await;
    assert_eq!(view["headword"], "lull to sleep");
}
