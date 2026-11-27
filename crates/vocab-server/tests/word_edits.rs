//! The owner's edits and the import merge (design §5.4, §7.1 screen 7;
//! R-03, R-04, R-22, R-25, R-51, R-52): every edit survives a re-import of
//! the same pack, pack suggestions are kept as conflicts and answered with
//! *Use new* / *Keep*. Synthetic words only.

mod common;

use axum::http::StatusCode;
use common::{Env, env, word};
use serde_json::{Value, json};

const BOOK: &str = "test-edits-book";

/// The word as the synthetic pack carries it, with extra fields.
fn lull() -> Value {
    let mut w = word("lull", 7, 3);
    w["hint"] = json!("Think of a lullaby.");
    w["grammar"] = json!("verb");
    w["czech"] = json!(["ukolébat", "utišit"]);
    w["accepted"] = json!(["lulls"]);
    w["confusables"] = json!(["loll"]);
    w["forms"] =
        json!([{ "form": "lulled", "common": true }, { "form": "lulling", "common": false }]);
    w
}

async fn detail(env: &Env, id: i64) -> Value {
    let (status, view) = env.get(&format!("/api/words/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view
}

async fn patch(env: &Env, id: i64, body: Value) -> (StatusCode, Value) {
    env.request("PATCH", &format!("/api/words/{id}"), Some(&body))
        .await
}

async fn patch_ok(env: &Env, id: i64, body: Value) -> Value {
    let (status, view) = patch(env, id, body).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view
}

async fn id_of(env: &Env, headword: &str) -> i64 {
    env.word_id(headword).await.get()
}

fn conflicts(view: &Value, field: &str) -> Vec<Value> {
    view["conflicts"]
        .as_array()
        .expect("conflicts")
        .iter()
        .filter(|c| c["field"] == field)
        .cloned()
        .collect()
}

fn values(list: &Value) -> Vec<String> {
    list.as_array()
        .expect("list")
        .iter()
        .map(|i| {
            i.get("value")
                .or_else(|| i.get("form"))
                .and_then(Value::as_str)
                .expect("value")
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn r04_field_edits_survive_a_reimport_and_the_pack_only_suggests() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    let view = patch_ok(
        &env,
        id,
        json!({
            "meaning": "The owner's own meaning.",
            "usefulness": 9,
            "goal": "recognise",
            "hint": null,
            "grammar": "verb, noun",
        }),
    )
    .await;
    assert_eq!(
        view["overridden"],
        json!(["goal", "grammar", "hint", "meaning", "usefulness"])
    );

    // The same pack again (a new pack id, same content).
    env.import(BOOK, vec![lull()]).await;
    let view = detail(&env, id).await;
    assert_eq!(view["meaning"], "The owner's own meaning.");
    assert_eq!(view["usefulness"], 9);
    assert_eq!(view["goal"], "recognise");
    assert_eq!(view["effective_goal"], "recognise");
    assert!(
        view["hint"].is_null(),
        "a field the owner cleared is not refilled"
    );
    assert_eq!(view["grammar"], "verb, noun");
    // Rule 4: what the pack says is kept as a suggestion.
    for (field, incoming) in [
        ("meaning", json!("Invented test meaning of lull.")),
        ("usefulness", json!(7)),
        ("hint", json!("Think of a lullaby.")),
        ("grammar", json!("verb")),
    ] {
        let found = conflicts(&view, field);
        assert_eq!(found.len(), 1, "{field}: {view}");
        assert_eq!(found[0]["incoming"], incoming, "{field}");
    }
    assert!(conflicts(&view, "goal").is_empty(), "the pack has no goal");
    let open = view["conflicts"].as_array().expect("c").len();

    // A third import adds no second copy of the same suggestions.
    env.import(BOOK, vec![lull()]).await;
    let view = detail(&env, id).await;
    assert_eq!(view["conflicts"].as_array().expect("c").len(), open);
    assert_eq!(env.int("SELECT COUNT(*) FROM word"), 1);
}

#[tokio::test]
async fn r03_use_new_takes_the_pack_value_and_the_field_follows_packs_again() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    patch_ok(&env, id, json!({ "meaning": "Owner meaning." })).await;
    env.import(BOOK, vec![lull()]).await;
    let view = detail(&env, id).await;
    let conflict = conflicts(&view, "meaning")[0].clone();
    assert_eq!(conflict["now"], "Owner meaning.");

    let (status, view) = env
        .post(
            &format!("/api/conflicts/{}", conflict["id"]),
            &json!({ "use": "incoming" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["meaning"], "Invented test meaning of lull.");
    assert!(conflicts(&view, "meaning").is_empty());
    assert!(
        !view["overridden"]
            .as_array()
            .expect("o")
            .contains(&json!("meaning"))
    );
    // Answered once: a second answer is refused.
    let (status, body) = env
        .post(
            &format!("/api/conflicts/{}", conflict["id"]),
            &json!({ "use": "keep" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "conflict_closed");

    // Rule 5: a later, different meaning is again only a suggestion.
    let mut changed = lull();
    changed["meaning"] = json!("A rephrased test meaning.");
    env.import(BOOK, vec![changed]).await;
    let view = detail(&env, id).await;
    assert_eq!(view["meaning"], "Invented test meaning of lull.");
    assert_eq!(
        conflicts(&view, "meaning")[0]["incoming"],
        "A rephrased test meaning."
    );
}

#[tokio::test]
async fn r03_keep_dismisses_and_the_same_suggestion_does_not_come_back() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    patch_ok(&env, id, json!({ "usefulness": 3 })).await;
    env.import(BOOK, vec![lull()]).await;
    let conflict = conflicts(&detail(&env, id).await, "usefulness")[0].clone();
    let (status, view) = env
        .post(
            &format!("/api/conflicts/{}", conflict["id"]),
            &json!({ "use": "keep" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["usefulness"], 3);
    assert!(conflicts(&view, "usefulness").is_empty());

    env.import(BOOK, vec![lull()]).await;
    assert!(conflicts(&detail(&env, id).await, "usefulness").is_empty());
    let mut changed = lull();
    changed["usefulness"] = json!(8);
    env.import(BOOK, vec![changed]).await;
    let found = conflicts(&detail(&env, id).await, "usefulness");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["incoming"], 8);
}

#[tokio::test]
async fn r03_editing_to_the_suggested_value_settles_the_suggestion() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    let mut changed = lull();
    changed["meaning"] = json!("A better test meaning.");
    env.import(BOOK, vec![changed]).await;
    assert_eq!(conflicts(&detail(&env, id).await, "meaning").len(), 1);
    let view = patch_ok(&env, id, json!({ "meaning": "A better test meaning." })).await;
    assert!(conflicts(&view, "meaning").is_empty());
    assert_eq!(
        env.int("SELECT COUNT(*) FROM field_conflict WHERE status = 'applied'"),
        1
    );
}

#[tokio::test]
async fn r04_a_changed_headword_is_still_found_by_the_pack() {
    let env = env();
    env.import(BOOK, vec![lull(), word("sift", 6, 3)]).await;
    let id = id_of(&env, "lull").await;
    let view = patch_ok(&env, id, json!({ "headword": "lull (sb) to sleep" })).await;
    assert_eq!(view["headword"], "lull (sb) to sleep");
    assert_eq!(
        view["pack_spellings"],
        json!([{ "headword": "lull", "key": "lull" }]),
        "the word still answers to the pack's spelling"
    );

    env.import(BOOK, vec![lull()]).await;
    assert_eq!(env.int("SELECT COUNT(*) FROM word"), 2, "no second lull");
    let view = detail(&env, id).await;
    assert_eq!(view["headword"], "lull (sb) to sleep");
    assert_eq!(view["sentences"].as_array().expect("s").len(), 3);

    // Another word's headword (or a pack's spelling of another word) is
    // refused, naming what collides.
    let (status, body) = patch(&env, id, json!({ "headword": "Sift" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "headword_taken");
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("m")
            .contains("\"sift\"")
    );
    let sift = id_of(&env, "sift").await;
    let (status, body) = patch(&env, sift, json!({ "headword": "Lull" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let message = body["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("\"lull\"") && message.contains("\"lull (sb) to sleep\""),
        "names the pack spelling and the word as shown: {message}"
    );

    // Back to the pack's spelling (in another case): nothing else is kept.
    let view = patch_ok(&env, id, json!({ "headword": "Lull" })).await;
    assert_eq!(view["pack_spellings"], json!([]));
    assert_eq!(
        env.int("SELECT COUNT(*) FROM word_pack_spelling WHERE key = 'lull (sb) to sleep'"),
        0
    );
    env.import(BOOK, vec![lull()]).await;
    assert_eq!(env.int("SELECT COUNT(*) FROM word"), 2);
    assert_eq!(detail(&env, id).await["headword"], "Lull");
}

/// Finding: an owner's intermediate spelling (a typo, or a change he took
/// back) never stays attached to the word, so a later, genuinely
/// different pack word with that spelling is a word of its own.
#[tokio::test]
async fn r04_the_owners_own_spellings_leave_nothing_behind() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    patch_ok(&env, id, json!({ "headword": "brontlex" })).await;
    patch_ok(&env, id, json!({ "headword": "lul" })).await;
    let view = patch_ok(&env, id, json!({ "headword": "lull" })).await;
    assert_eq!(view["pack_spellings"], json!([]));
    assert_eq!(env.int("SELECT COUNT(*) FROM word_pack_spelling"), 1);

    env.import(BOOK, vec![word("brontlex", 4, 3)]).await;
    assert_eq!(env.int("SELECT COUNT(*) FROM word"), 2, "a word of its own");
    assert_eq!(detail(&env, id).await["headword"], "lull");
    assert_eq!(
        detail(&env, id).await["sentences"]
            .as_array()
            .expect("s")
            .len(),
        3
    );
    // Another word may take a spelling the owner gave up.
    let other = id_of(&env, "brontlex").await;
    patch_ok(&env, other, json!({ "headword": "lul" })).await;
}

/// The owner sees the pack spellings a renamed word answers to and can
/// remove one; a pack with it then brings a new word.
#[tokio::test]
async fn r04_a_pack_spelling_can_be_removed() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    patch_ok(&env, id, json!({ "headword": "lull to sleep" })).await;
    let uri = format!("/api/words/{id}/spellings");
    // The current headword is not a removable spelling; unknown keys 404.
    for key in ["lull to sleep", "nothing"] {
        let (status, _) = env
            .request("DELETE", &uri, Some(&json!({ "key": key })))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{key}");
    }
    let (status, view) = env
        .request("DELETE", &uri, Some(&json!({ "key": "lull" })))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["pack_spellings"], json!([]));
    env.import(BOOK, vec![lull()]).await;
    assert_eq!(env.int("SELECT COUNT(*) FROM word"), 2);
    assert_eq!(detail(&env, id).await["headword"], "lull to sleep");
}

/// One pack with both the old (pack) spelling and the new headword of a
/// renamed word: both entries are merged into it and the report says so.
#[tokio::test]
async fn r04_a_pack_with_both_spellings_of_a_word_is_reported() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    patch_ok(&env, id, json!({ "headword": "lull to sleep" })).await;
    let mut renamed = word("lull to sleep", 7, 0);
    renamed["czech"] = json!(["uspat"]);
    renamed["sentences"] = json!([{
        "en": "They [[lull to sleep]] the test baby.",
        "cs": "[[Uspávají]] testovací miminko.",
        "origin": "example",
    }]);
    let report = env.import(BOOK, vec![lull(), renamed]).await;
    assert_eq!(env.int("SELECT COUNT(*) FROM word"), 1);
    let pack = &report["packs"][0];
    let warnings = pack["warnings"].as_array().expect("warnings");
    let warning = warnings
        .iter()
        .find(|w| w["pointer"] == "/words/1/headword")
        .unwrap_or_else(|| panic!("a warning for the second entry: {pack}"));
    let message = warning["message"].as_str().expect("message");
    assert!(
        message.contains("\"lull\"") && message.contains("\"lull to sleep\""),
        "{message}"
    );
    // Nothing of either entry is lost.
    let view = detail(&env, id).await;
    assert!(values(&view["items"]["czech"]).contains(&"uspat".to_owned()));
    assert_eq!(view["sentences"].as_array().expect("s").len(), 4);
}

#[tokio::test]
async fn r04_removed_items_and_forms_never_come_back_but_can_be_added_again() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    for (kind, key) in [
        ("czech", "utišit"),
        ("accepted", "lulls"),
        ("confusable", "loll"),
        ("form", "lulling"),
    ] {
        let uri = format!("/api/words/{id}/items/{kind}/{}", encode(key));
        let (status, view) = env.request("DELETE", &uri, Some(&json!({}))).await;
        assert_eq!(status, StatusCode::OK, "{kind}: {view}");
    }
    env.import(BOOK, vec![lull()]).await;
    let view = detail(&env, id).await;
    assert_eq!(values(&view["items"]["czech"]), ["ukolébat"]);
    assert!(view["items"]["accepted"].as_array().expect("a").is_empty());
    assert!(
        view["items"]["confusables"]
            .as_array()
            .expect("c")
            .is_empty()
    );
    assert_eq!(values(&view["items"]["forms"]), ["lulled"]);
    assert_eq!(view["czech"], json!(["ukolébat"]));

    // The last Czech equivalent stays.
    let uri = format!("/api/words/{id}/items/czech/{}", encode("ukolébat"));
    let (status, body) = env.request("DELETE", &uri, Some(&json!({}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "last_czech");

    // Added back: the pack's item returns; a new one is the owner's.
    for (kind, value) in [
        ("czech", "Utišit"),
        ("czech", "konejšit"),
        ("accepted", "lulled"),
        ("form", "lulling"),
    ] {
        let (status, view) = env
            .post(
                &format!("/api/words/{id}/items"),
                &json!({ "kind": kind, "value": value }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{kind} {value}: {view}");
    }
    let view = detail(&env, id).await;
    let czech = view["items"]["czech"].as_array().expect("czech");
    assert_eq!(
        values(&view["items"]["czech"]),
        ["ukolébat", "utišit", "konejšit"]
    );
    assert_eq!(czech[1]["mine"], false, "the pack's item came back");
    assert_eq!(czech[2]["mine"], true);
    assert_eq!(view["items"]["accepted"][0]["value"], "lulled");
    assert_eq!(view["items"]["accepted"][0]["mine"], true);
    assert_eq!(
        env.int("SELECT COUNT(*) FROM word_item WHERE kind = 'owner_accepted'"),
        1
    );
    assert_eq!(values(&view["items"]["forms"]), ["lulled", "lulling"]);
    assert_eq!(
        view["items"]["forms"][1]["common"], true,
        "the owner's flag"
    );
    // Adding what is already there changes nothing.
    let (status, _) = env
        .post(
            &format!("/api/words/{id}/items"),
            &json!({ "kind": "czech", "value": "UKOLÉBAT" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        detail(&env, id).await["items"]["czech"]
            .as_array()
            .expect("c")
            .len(),
        3
    );
}

/// Percent-encode a path segment (the item keys are Czech words).
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[tokio::test]
async fn r25_edited_and_hidden_sentences_stay_as_the_owner_left_them() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    let view = detail(&env, id).await;
    let sentences = view["sentences"].as_array().expect("s").clone();
    let first = sentences[0]["id"].as_i64().expect("id");
    let third = sentences[2]["id"].as_i64().expect("id");

    let (status, view) = env
        .request(
            "PATCH",
            &format!("/api/sentences/{first}"),
            Some(&json!({
                "en": "Test line 1, corrected, with the [[lull]] in it.",
                "cs": "Opravený testovací řádek s [[ukolébáním]].",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["sentences"][0]["edited"], true);
    assert_eq!(view["sentences"][0]["en"]["target"], "lull");
    let (status, view) = env
        .post(
            &format!("/api/sentences/{third}/hidden"),
            &json!({ "hidden": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["sentences"][2]["hidden"], true);

    // The pack corrects every Czech translation: only sentence 2 takes it.
    let mut corrected = lull();
    for s in corrected["sentences"].as_array_mut().expect("s") {
        let cs = s["cs"].as_str().expect("cs").replace("řádek", "řádeček");
        s["cs"] = json!(cs);
    }
    let report = env.import(BOOK, vec![corrected]).await;
    assert_eq!(report["packs"][0]["summary"]["updated_sentences"], 1);
    let view = detail(&env, id).await;
    let sentences = view["sentences"].as_array().expect("s");
    assert_eq!(sentences.len(), 3, "no copy of the edited sentence");
    assert_eq!(
        sentences[0]["cs"]["pre"], "Opravený testovací řádek s ",
        "the owner's text stays"
    );
    assert_eq!(sentences[1]["cs"]["pre"], "Testovací řádeček 2 s ");
    assert_eq!(sentences[2]["hidden"], true);
    assert_eq!(sentences[2]["cs"]["pre"], "Testovací řádek 3 s ");
}

#[tokio::test]
async fn r25_a_sentence_edit_is_checked_like_an_imported_sentence() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    let view = detail(&env, id).await;
    let first = view["sentences"][0]["id"].as_i64().expect("id");
    let second_en = "Test line 2 with the [[lull]] in it.";
    for (en, cs, status, code) in [
        (
            "No marker here.",
            "Bez [[značky]].",
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid",
        ),
        (
            "Two [[marks]] [[here]].",
            "[[Dvě]] značky.",
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid",
        ),
        (
            "Fine [[lull]].",
            "[[123]]",
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid",
        ),
        (
            second_en,
            "Jiný [[překlad]].",
            StatusCode::CONFLICT,
            "duplicate_sentence",
        ),
    ] {
        let (got, body) = env
            .request(
                "PATCH",
                &format!("/api/sentences/{first}"),
                Some(&json!({ "en": en, "cs": cs })),
            )
            .await;
        assert_eq!(got, status, "{en}: {body}");
        assert_eq!(body["error"]["code"], code, "{en}");
    }
    let long = format!("[[a]]{}", "b".repeat(600));
    let (status, _) = env
        .request(
            "PATCH",
            &format!("/api/sentences/{first}"),
            Some(&json!({ "en": long, "cs": "[[a]]" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = env
        .request(
            "PATCH",
            "/api/sentences/99999",
            Some(&json!({ "en": "A [[b]].", "cs": "A [[b]]." })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Nothing of the refused edits was stored.
    assert_eq!(env.int("SELECT COUNT(*) FROM sentence WHERE edited = 1"), 0);
}

#[tokio::test]
async fn r52_goal_switch_suspends_and_brings_back_the_produce_card() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    // A Produce card as an earlier unlock left it (design §6.4).
    env.exec(&format!(
        "INSERT INTO card (word_id, kind, state, due_day, created_at) \
         VALUES ({id}, 'produce', 'new', '2026-11-21', '2026-11-20T09:00:00Z')"
    ));
    let produce_suspended = || {
        env.int(&format!(
            "SELECT suspended FROM card WHERE word_id = {id} AND kind = 'produce'"
        ))
    };
    let view = patch_ok(&env, id, json!({ "goal": "recognise" })).await;
    assert_eq!(view["effective_goal"], "recognise");
    assert_eq!(produce_suspended(), 1);
    patch_ok(&env, id, json!({ "goal": "active" })).await;
    assert_eq!(produce_suspended(), 0);
    // Back to "from usefulness" (7 → active).
    let view = patch_ok(&env, id, json!({ "goal": null })).await;
    assert!(view["goal"].is_null());
    assert_eq!(view["effective_goal"], "active");
}

async fn new_waiting(env: &Env) -> i64 {
    let (_, today) = env.get("/api/today").await;
    today["new_waiting"].as_i64().expect("new_waiting")
}

async fn act(env: &Env, id: i64, body: Value) -> (StatusCode, Value) {
    env.post(&format!("/api/words/{id}/actions"), &body).await
}

#[tokio::test]
async fn r51_an_included_low_priority_word_is_practised_recognise_only() {
    let env = env();
    let mut skip = word("dwindle", 9, 3);
    skip["tag"] = json!({ "kind": "low-priority" });
    skip["goal"] = json!("active");
    env.import(BOOK, vec![skip, lull()]).await;
    let id = id_of(&env, "dwindle").await;
    assert_eq!(new_waiting(&env).await, 1, "low priority is not introduced");

    let (status, view) = act(&env, id, json!({ "action": "include" })).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["included"], true);
    assert_eq!(view["effective_goal"], "recognise");
    assert_eq!(view["cards"][0]["kind"], "understand");
    let retention = view["cards"][0]["desired_retention"].as_f64().expect("r");
    assert!((retention - 0.82).abs() < 1e-6, "{retention}");
    assert_eq!(new_waiting(&env).await, 2);

    // Included survives a re-import that tags it low priority again.
    let mut again = word("dwindle", 9, 3);
    again["tag"] = json!({ "kind": "low-priority" });
    env.import(BOOK, vec![again]).await;
    assert_eq!(new_waiting(&env).await, 2);

    let (_, view) = act(&env, id, json!({ "action": "exclude" })).await;
    assert_eq!(view["included"], false);
    assert_eq!(new_waiting(&env).await, 1);
}

#[tokio::test]
async fn r04_a_suspended_word_is_not_practised_until_it_is_unsuspended() {
    let env = env();
    env.import(BOOK, vec![lull(), word("sift", 6, 3)]).await;
    let id = id_of(&env, "lull").await;
    let (status, view) = act(&env, id, json!({ "action": "suspend" })).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["suspended"], true);
    assert_eq!(new_waiting(&env).await, 1);
    let (_, rows) = env.get("/api/words?flags=suspended").await;
    assert_eq!(rows.as_array().expect("rows").len(), 1);
    assert_eq!(rows[0]["headword"], "lull");
    // Re-importing does not lift the suspension.
    env.import(BOOK, vec![lull()]).await;
    assert_eq!(new_waiting(&env).await, 1);
    act(&env, id, json!({ "action": "unsuspend" })).await;
    assert_eq!(new_waiting(&env).await, 2);
}

#[tokio::test]
async fn r04_reset_card_forgets_the_learning_state_but_keeps_the_history() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    let (_, start) = env.post("/api/sessions", &json!({ "kind": "start" })).await;
    let session = &start["session"];
    let item = session["items"][0]["item_id"].as_i64().expect("item");
    let (status, body) = env
        .post(
            &format!(
                "/api/sessions/{}/reviews",
                session["id"].as_str().expect("sid")
            ),
            &json!({
                "client_review_id": "00000000-0000-4000-8000-000000000001",
                "item_id": item,
                "raw_grade": 2,
                "hint_used": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let view = detail(&env, id).await;
    assert_eq!(view["cards"][0]["state"], "review");
    assert!(view["cards"][0]["recall"].as_f64().is_some());
    assert_eq!(view["history"].as_array().expect("h").len(), 1);
    assert_eq!(view["history"][0]["raw_grade"], 2);
    assert_eq!(view["status"], "learning");

    let (status, view) = act(
        &env,
        id,
        json!({ "action": "reset_card", "kind": "understand" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let card = &view["cards"][0];
    assert_eq!(card["state"], "new");
    assert!(card["stability"].is_null() && card["due_day"].is_null());
    assert!(card["introduced_day"].is_null() && card["format"].is_null());
    assert_eq!(card["reps"], 0);
    assert_eq!(view["status"], "new");
    assert_eq!(
        view["history"].as_array().expect("h").len(),
        1,
        "history stays"
    );

    let (status, body) = act(
        &env,
        id,
        json!({ "action": "reset_card", "kind": "produce" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "no_such_card");
}

#[tokio::test]
async fn r04_deleting_a_word_removes_it_with_its_cards_and_history() {
    let env = env();
    env.import(BOOK, vec![lull(), word("sift", 6, 3)]).await;
    let id = id_of(&env, "lull").await;
    let (_, start) = env.post("/api/sessions", &json!({ "kind": "start" })).await;
    assert!(!start["session"].is_null());
    patch_ok(&env, id, json!({ "meaning": "Owner meaning." })).await;

    let (status, body) = env
        .request("DELETE", &format!("/api/words/{id}"), Some(&json!({})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deleted"], id);
    let (status, _) = env.get(&format!("/api/words/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    for table in [
        "card",
        "sentence",
        "word_item",
        "word_form",
        "word_source",
        "word_override",
    ] {
        assert_eq!(
            env.int(&format!(
                "SELECT COUNT(*) FROM {table} WHERE word_id = {id}"
            )),
            0,
            "{table}"
        );
    }
    // The block in progress goes on without it.
    let (status, current) = env.get("/api/sessions/current").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        current["items"]
            .as_array()
            .expect("items")
            .iter()
            .all(|i| i["word"]["headword"] != "lull")
    );
    let (status, _) = env
        .request("DELETE", &format!("/api/words/{id}"), Some(&json!({})))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A later pack with the word adds it as a new word.
    env.import(BOOK, vec![lull()]).await;
    assert_ne!(id_of(&env, "lull").await, id);
}

#[tokio::test]
async fn r04_single_line_fields_and_the_spelling_note_follow_the_import_rules() {
    let env = env();
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    for edit in [
        json!({ "headword": "lull\nto sleep" }),
        json!({ "grammar": "verb\nnoun" }),
        json!({ "spelling_note": "Double l." }),
        json!({ "spelling_note": "Double l.", "spelling": false }),
    ] {
        let (status, body) = patch(&env, id, edit.clone()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{edit}: {body}");
        assert_eq!(body["error"]["code"], "invalid");
    }
    for (kind, value) in [("czech", "uko\nlébat"), ("form", "lul\nled")] {
        let (status, body) = env
            .post(
                &format!("/api/words/{id}/items"),
                &json!({ "kind": kind, "value": value }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{kind}: {body}");
    }
    let view = detail(&env, id).await;
    assert_eq!(view["headword"], "lull");
    assert!(view["spelling_note"].is_null());
    // With spelling practice the note is stored; turning spelling off later
    // keeps it, as a pack would.
    let view = patch_ok(
        &env,
        id,
        json!({ "spelling": true, "spelling_note": "Double l." }),
    )
    .await;
    assert_eq!(view["spelling_note"], "Double l.");
    let view = patch_ok(&env, id, json!({ "spelling": false })).await;
    assert_eq!(view["spelling_note"], "Double l.");
}

/// A deleted word's ids (word, sentences, suggestions) are never given to
/// a new row: a stale page or an Undo for it must not reach another word.
#[tokio::test]
async fn r04_ids_of_a_deleted_word_are_never_reused() {
    let env = env();
    env.import(BOOK, vec![word("sift", 6, 3)]).await;
    env.import(BOOK, vec![lull()]).await;
    let id = id_of(&env, "lull").await;
    patch_ok(&env, id, json!({ "meaning": "Owner meaning." })).await;
    env.import(BOOK, vec![lull()]).await;
    let view = detail(&env, id).await;
    let sentence = view["sentences"][2]["id"].as_i64().expect("sentence");
    let conflict = conflicts(&view, "meaning")[0]["id"].as_i64().expect("c");
    assert_eq!(id, env.int("SELECT MAX(id) FROM word"), "the newest word");
    let (status, _) = env
        .request("DELETE", &format!("/api/words/{id}"), Some(&json!({})))
        .await;
    assert_eq!(status, StatusCode::OK);

    let mut other = word("brontle", 5, 3);
    other["meaning"] = json!("Another invented meaning.");
    env.import(BOOK, vec![other]).await;
    env.import(BOOK, vec![word("brontle", 6, 3)]).await;
    let new = id_of(&env, "brontle").await;
    assert!(new > id, "word id {new} after deleted {id}");
    assert!(
        env.int("SELECT MIN(id) FROM sentence WHERE word_id = (SELECT MAX(id) FROM word)")
            > sentence
    );
    assert!(env.int("SELECT COALESCE(MAX(id), 0) FROM field_conflict") > conflict);
    // The stale requests find nothing.
    let (status, _) = patch(&env, id, json!({ "usefulness": 3 })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = env
        .request(
            "PATCH",
            &format!("/api/sentences/{sentence}"),
            Some(&json!({ "en": "The [[brontle]] test.", "cs": "Test [[brontle]]." })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = env
        .post(
            &format!("/api/conflicts/{conflict}"),
            &json!({ "use": "keep" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(detail(&env, new).await["usefulness"], 5);
}
