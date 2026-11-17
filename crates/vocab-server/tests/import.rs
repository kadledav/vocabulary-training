//! Import end to end through the HTTP router (design §5, §8; R-01–R-03,
//! R-10, R-11, R-13, R-15, R-20, R-22–R-24, R-59, R-84). All packs are
//! synthetic: `test-…` ids and sentences written for these tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use rusqlite::Connection;
use serde_json::{Value, json};
use tower::ServiceExt;
use vocab_server::clock::{Clock, ManualClock, Timestamp};
use vocab_server::config::Limits;
use vocab_server::db::Db;
use vocab_server::http::{self, AppState};

// ── harness ─────────────────────────────────────────────────────────────────

struct TestApp {
    dir: tempfile::TempDir,
    clock: Arc<ManualClock>,
    router: axum::Router,
}

fn ts(text: &str) -> Timestamp {
    text.parse().expect("timestamp")
}

fn app() -> TestApp {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(dir.path()).expect("db");
    let clock = Arc::new(ManualClock::new(ts("2026-11-20T09:00:00Z")));
    let state = AppState::with_clock(db, Arc::clone(&clock) as Arc<dyn Clock>);
    let router = http::app(state, &Limits::default());
    TestApp { dir, clock, router }
}

impl TestApp {
    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 16 << 20)
            .await
            .expect("body");
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, value)
    }

    async fn post(&self, uri: &str, body: &Value) -> (StatusCode, Value) {
        self.send(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.send(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
    }

    async fn preview(&self, text: &str) -> Value {
        let (status, report) = self
            .post("/api/import/preview", &json!({ "text": text }))
            .await;
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["mode"], "preview");
        report
    }

    async fn commit(&self, text: &str) -> Value {
        let (status, report) = self
            .post("/api/import/commit", &json!({ "text": text }))
            .await;
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["mode"], "commit");
        report
    }

    /// A second connection for inspecting (and, for test setup only,
    /// seeding state that later milestones' modules will own).
    fn sql(&self) -> Connection {
        Connection::open(self.dir.path().join("vocabulary.sqlite")).expect("open")
    }

    fn int(&self, sql: &str) -> i64 {
        self.sql().query_row(sql, [], |row| row.get(0)).expect(sql)
    }

    fn text(&self, sql: &str) -> Option<String> {
        self.sql().query_row(sql, [], |row| row.get(0)).expect(sql)
    }

    /// Every row of every table, for "nothing changed" checks.
    fn dump(&self) -> String {
        let conn = self.sql();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .expect("tables")
            .query_map([], |row| row.get(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        let mut out = String::new();
        for table in tables {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .or_else(|_| conn.prepare(&format!("SELECT * FROM {table}")))
                .expect("select");
            let columns = stmt.column_count();
            let rows = stmt
                .query_map([], |row| {
                    (0..columns)
                        .map(|i| {
                            row.get::<_, rusqlite::types::Value>(i)
                                .map(|v| format!("{v:?}"))
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .expect("rows");
            for row in rows {
                out.push_str(&format!("{table}: {}\n", row.expect("row").join(" | ")));
            }
        }
        out
    }

    async fn word_id(&self, headword: &str) -> i64 {
        let (status, rows) = self
            .get(&format!("/api/words?q={}", headword.replace(' ', "%20")))
            .await;
        assert_eq!(status, StatusCode::OK);
        rows.as_array()
            .expect("rows")
            .iter()
            .find(|r| r["headword"] == headword)
            .unwrap_or_else(|| panic!("{headword} not in {rows}"))["id"]
            .as_i64()
            .expect("id")
    }

    async fn word(&self, headword: &str) -> Value {
        let id = self.word_id(headword).await;
        let (status, view) = self.get(&format!("/api/words/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        view
    }
}

// ── synthetic packs ─────────────────────────────────────────────────────────

/// An invented sentence pair with `target` marked.
fn sentence(en: &str, cs: &str) -> Value {
    json!({ "en": en, "cs": cs, "origin": "example" })
}

fn hum_sentences() -> Vec<Value> {
    vec![
        sentence(
            "The old fridge [[hummed]] all night long.",
            "Stará lednice celou noc [[bzučela]].",
        ),
        sentence(
            "A low [[hum]] came from the engine room.",
            "Ze strojovny se ozýval tichý [[bzukot]].",
        ),
        sentence(
            "She [[hummed]] a tune while she painted the fence.",
            "Při natírání plotu si [[broukala]] melodii.",
        ),
    ]
}

fn word(headword: &str, usefulness: u8, sentences: Vec<Value>) -> Value {
    json!({
        "headword": headword,
        "meaning": format!("Invented test meaning of {headword}."),
        "czech": [format!("{headword}-cz")],
        "usefulness": usefulness,
        "sentences": sentences,
    })
}

fn three_sentences(stem: &str) -> Vec<Value> {
    (1..=3)
        .map(|i| {
            sentence(
                &format!("Test line {i} about the [[{stem}]] here."),
                &format!("Testovací řádek {i} o [[{stem}]] tady."),
            )
        })
        .collect()
}

fn pack(source: &str, date: &str, n: u32, part: (u32, u32), words: Vec<Value>) -> Value {
    json!({
        "format": "vocab-pack",
        "format_version": 1,
        "pack_id": format!("{source}--{date}--{n}"),
        "exported_at": date,
        "part": { "index": part.0, "total": part.1 },
        "source": { "id": source, "type": "book", "title": "Test Novel", "author": "Test Author" },
        "reading_position": "Chapter 10",
        "words": words,
    })
}

/// Claude-style answer: each pack in its own ```json block, prose around.
fn paste(packs: &[Value]) -> String {
    let mut out = String::from("Here are your packs for the training app.\n\n");
    for (i, p) in packs.iter().enumerate() {
        out.push_str(&format!(
            "Part {}:\n\n```json\n{}\n```\n\n",
            i + 1,
            serde_json::to_string_pretty(p).expect("json")
        ));
    }
    out.push_str("That is all; I was unsure about one word.\n");
    out
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn statuses(report: &Value) -> Vec<String> {
    report["packs"]
        .as_array()
        .expect("packs")
        .iter()
        .map(|p| p["status"].as_str().expect("status").to_owned())
        .collect()
}

// ── documented example, preview, idempotency ───────────────────────────────

#[tokio::test]
async fn r11_example_pack_from_the_export_instructions_imports() {
    let app = app();
    // The whole Markdown file is pasted: prose around one ```json block.
    let doc = std::fs::read_to_string(repo_root().join("docs/claude-export-instructions.md"))
        .expect("doc");
    let preview = app.preview(&doc).await;
    assert_eq!(statuses(&preview), ["ready"]);
    assert_eq!(preview["totals"]["new_words"], 1);
    assert_eq!(preview["packs"][0]["errors"], json!([]));
    assert_eq!(preview["packs"][0]["warnings"], json!([]));

    let report = app.commit(&doc).await;
    assert_eq!(statuses(&report), ["imported"]);
    assert_eq!(report["totals"]["new_words"], 1);
    assert_eq!(report["totals"]["new_sentences"], 3);

    let word = app.word("verbatim").await;
    assert_eq!(
        word["czech"],
        json!(["doslova", "slovo od slova", "doslovný"])
    );
    assert_eq!(word["usefulness"], 7);
    assert_eq!(word["goal"], "active");
    assert_eq!(word["spelling"], true);
    assert_eq!(word["spelling_note"], "verb-a-tim: ends in -im, not -em");
    assert_eq!(word["accepted"], json!(["verbatim"]));
    assert_eq!(word["confusables"], json!(["literally"]));
    assert_eq!(word["sentences"].as_array().expect("sentences").len(), 3);
    assert_eq!(word["sentences"][0]["en"]["target"], "verbatim");
    assert_eq!(word["sentences"][1]["cs"]["target"], "doslovný");
    assert_eq!(word["sources"][0]["title"], "General Vocabulary");
    assert_eq!(word["sources"][0]["seq"], 1);

    let (_, sources) = app.get("/api/sources").await;
    assert_eq!(sources[0]["slug"], "general");
    assert_eq!(sources[0]["type"], "general");
    assert_eq!(sources[0]["word_count"], 1);

    let (_, recent) = app.get("/api/import/recent").await;
    assert_eq!(recent[0]["pack_id"], "general--2026-11-17--1");
    assert_eq!(recent[0]["via"], "paste");
    assert_eq!(recent[0]["summary"]["new_words"], 1);
}

#[tokio::test]
async fn r11_preview_never_writes_and_matches_the_commit() {
    let app = app();
    let first = pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, hum_sentences())],
    );
    app.commit(&paste(&[first])).await;
    let before = app.dump();

    let mut changed = word("hum", 6, hum_sentences());
    changed["meaning"] = json!("A rephrased invented meaning.");
    changed["sentences"]
        .as_array_mut()
        .expect("sentences")
        .push(sentence(
            "The bees [[hummed]] around the hive.",
            "Včely [[bzučely]] kolem úlu.",
        ));
    let second = paste(&[pack(
        "test-novel",
        "2026-11-18",
        1,
        (1, 1),
        vec![changed, word("glint", 5, three_sentences("glint"))],
    )]);
    let preview = app.preview(&second).await;
    assert_eq!(app.dump(), before, "preview must not write");

    let commit = app.commit(&second).await;
    assert_ne!(app.dump(), before);
    // The preview predicted the commit exactly (apart from ids/status).
    for (p, c) in preview["packs"][0]["words"]
        .as_array()
        .expect("words")
        .iter()
        .zip(commit["packs"][0]["words"].as_array().expect("words"))
    {
        assert_eq!(p["outcome"], c["outcome"]);
        assert_eq!(p["merge"]["conflicts"], c["merge"]["conflicts"]);
        assert_eq!(p["merge"]["sentences_added"], c["merge"]["sentences_added"]);
        assert_eq!(p["merge"]["sentences_known"], c["merge"]["sentences_known"]);
    }
    assert_eq!(preview["totals"], commit["totals"]);
    assert_eq!(commit["totals"]["new_words"], 1);
    assert_eq!(commit["totals"]["merged_words"], 1);
    assert_eq!(commit["totals"]["new_sentences"], 4);
    assert_eq!(commit["totals"]["conflicts"], 1);
}

#[tokio::test]
async fn r11_second_import_of_the_same_pack_is_a_no_op() {
    let app = app();
    let text = paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, hum_sentences())],
    )]);
    app.commit(&text).await;
    let before = app.dump();
    app.clock.set(ts("2026-11-21T09:00:00Z"));

    // Same pack, re-indented and with keys in another order: same content hash.
    let value: Value = serde_json::from_str(
        text.split("```json\n")
            .nth(1)
            .and_then(|s| s.split("\n```").next())
            .expect("block"),
    )
    .expect("json");
    let compact = value.to_string();
    for again in [text.clone(), compact] {
        let preview = app.preview(&again).await;
        assert_eq!(statuses(&preview), ["already_imported"]);
        assert_eq!(
            preview["packs"][0]["already_imported_at"],
            "2026-11-20T09:00:00Z"
        );
        let report = app.commit(&again).await;
        assert_eq!(statuses(&report), ["already_imported"]);
        assert_eq!(report["totals"]["new_words"], 0);
        assert_eq!(app.dump(), before, "re-import changed the database");
    }
}

#[tokio::test]
async fn r11_same_pack_twice_in_one_paste_is_imported_once() {
    let app = app();
    let p = pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, hum_sentences())],
    );
    let text = paste(&[p.clone(), p]);
    let preview = app.preview(&text).await;
    assert_eq!(statuses(&preview), ["ready", "already_imported"]);
    let report = app.commit(&text).await;
    assert_eq!(statuses(&report), ["imported", "already_imported"]);
    assert_eq!(app.int("SELECT COUNT(*) FROM pack_import"), 1);
}

#[tokio::test]
async fn r11_concurrent_commits_of_the_same_paste_import_each_pack_once() {
    let app = app();
    let text = paste(&[
        pack(
            "test-novel",
            "2026-11-17",
            1,
            (1, 2),
            vec![word("hum", 6, hum_sentences())],
        ),
        pack(
            "test-novel",
            "2026-11-17",
            1,
            (2, 2),
            vec![word("glint", 5, three_sentences("glint"))],
        ),
    ]);
    let (a, b) = tokio::join!(app.commit(&text), app.commit(&text));
    for report in [&a, &b] {
        assert!(
            report["packs"]
                .as_array()
                .expect("packs")
                .iter()
                .all(|p| p["notes"] == json!([])),
            "parts sharing a pack_id are not 'reused': {report}"
        );
    }
    let mut all: Vec<String> = statuses(&a).into_iter().chain(statuses(&b)).collect();
    all.sort();
    assert_eq!(
        all,
        [
            "already_imported",
            "already_imported",
            "imported",
            "imported"
        ]
    );
    assert_eq!(app.int("SELECT COUNT(*) FROM pack_import"), 2);
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 2);
    assert_eq!(app.int("SELECT COUNT(*) FROM sentence"), 6);
}

#[tokio::test]
async fn r11_reused_pack_id_with_other_content_is_imported_with_a_note() {
    let app = app();
    let a = pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, hum_sentences())],
    );
    let b = pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("glint", 5, three_sentences("glint"))],
    );
    app.commit(&paste(&[a])).await;
    let preview = app.preview(&paste(std::slice::from_ref(&b))).await;
    assert_eq!(statuses(&preview), ["ready"]);
    let report = app.commit(&paste(&[b])).await;
    assert_eq!(statuses(&report), ["imported"]);
    let notes = report["packs"][0]["notes"].to_string();
    assert!(notes.contains("same pack_id"), "{notes}");
    assert_eq!(app.int("SELECT COUNT(*) FROM pack_import"), 2);
}

// ── errors are scoped and listed (R-23, R-84) ──────────────────────────────

#[tokio::test]
async fn r23_paste_two_packs_one_with_a_bad_sentence_preview_shows_it_then_import() {
    let app = app();
    let mut bad = hum_sentences();
    bad[1] = sentence(
        "A low [[hum]] and a [[buzz]] came.",
        "Ozval se tichý [[bzukot]].",
    );
    let text = paste(&[
        pack(
            "test-novel",
            "2026-11-17",
            1,
            (1, 2),
            vec![word("hum", 6, bad)],
        ),
        pack(
            "test-novel",
            "2026-11-17",
            1,
            (2, 2),
            vec![word("glint", 5, three_sentences("glint"))],
        ),
    ]);
    let preview = app.preview(&text).await;
    assert_eq!(statuses(&preview), ["ready", "ready"]);
    let hum = &preview["packs"][0]["words"][0];
    assert_eq!(hum["outcome"], "new");
    assert_eq!(
        hum["sentence_errors"][0]["pointer"],
        "/words/0/sentences/1/en"
    );
    assert_eq!(hum["sentence_errors"][0]["code"], "marker");
    assert_eq!(preview["totals"]["sentence_errors"], 1);
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 0);

    let report = app.commit(&text).await;
    assert_eq!(statuses(&report), ["imported", "imported"]);
    assert_eq!(
        report["packs"][0]["words"][0]["sentence_errors"][0]["code"],
        "marker"
    );
    let word = app.word("hum").await;
    assert_eq!(
        word["sentences"].as_array().expect("sentences").len(),
        2,
        "bad one dropped"
    );
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 2);
}

#[tokio::test]
async fn r23_rejected_and_unreadable_packs_do_not_block_the_others() {
    let app = app();
    let mut unknown_version = pack(
        "test-novel",
        "2026-11-17",
        2,
        (1, 1),
        vec![word("glint", 5, vec![])],
    );
    unknown_version["format_version"] = json!(9);
    let mut invalid_word = word("wobble", 0, vec![]);
    invalid_word["usefulness"] = json!(0);
    let text = format!(
        "{}\n```json\n{{\"format\": \"vocab-pack\",}}\n```\n",
        paste(&[
            unknown_version,
            pack(
                "test-novel",
                "2026-11-17",
                1,
                (1, 1),
                vec![word("hum", 6, hum_sentences()), invalid_word]
            ),
        ])
    );
    let report = app.commit(&text).await;
    assert_eq!(statuses(&report), ["rejected", "imported", "unreadable"]);
    assert_eq!(report["packs"][0]["errors"][0]["code"], "unknown_version");
    assert!(
        report["packs"][0]["errors"][0]["message"]
            .as_str()
            .expect("message")
            .contains("format 9")
    );
    assert!(
        report["packs"][2]["errors"][0]["message"]
            .as_str()
            .expect("message")
            .contains("not valid JSON")
    );
    let words = &report["packs"][1]["words"];
    assert_eq!(words[0]["outcome"], "new");
    assert_eq!(words[1]["outcome"], "invalid");
    assert_eq!(words[1]["errors"][0]["pointer"], "/words/1/usefulness");
    assert_eq!(report["totals"]["invalid_words"], 1);
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 1);
    assert_eq!(app.int("SELECT COUNT(*) FROM pack_import"), 1);
}

#[tokio::test]
async fn r84_request_limits_and_bad_requests_get_json_errors() {
    let app = app();
    // Empty paste and a paste with nothing but prose.
    for text in ["", "   \n", "just some prose\n```json\n```\n"] {
        let (status, body) = app
            .post("/api/import/preview", &json!({ "text": text }))
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{text:?}");
        assert_eq!(body["error"]["code"], "invalid_paste");
    }
    // Over the 2 MiB paste limit (checked on the decoded text).
    let huge = "x".repeat(2 * 1024 * 1024 + 1);
    let (status, body) = app
        .post("/api/import/commit", &json!({ "text": huge }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "invalid_paste");
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("message")
            .contains("too large")
    );
    // Over the 5 MiB import body limit: a JSON 413.
    let huge = "x".repeat(5 * 1024 * 1024);
    let (status, body) = app
        .post("/api/import/commit", &json!({ "text": huge }))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["error"]["code"], "too_large");
    // Larger than the 64 KiB default, but fine for the import routes.
    let big_prose = format!(
        "{}\n{}",
        "y".repeat(200 * 1024),
        paste(&[pack(
            "test-novel",
            "2026-11-17",
            1,
            (1, 1),
            vec![word("hum", 6, hum_sentences())],
        )])
    );
    let (status, report) = app
        .post("/api/import/preview", &json!({ "text": big_prose }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(statuses(&report), ["ready"]);
    // Not JSON / wrong content type / unknown body field.
    let (status, body) = app
        .send(
            Request::builder()
                .method("POST")
                .uri("/api/import/commit")
                .header("content-type", "text/plain")
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(body["error"]["code"], "unsupported_media_type");
    let (status, body) = app
        .post(
            "/api/import/commit",
            &json!({ "text": "{}", "dry_run": true }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");
    assert_eq!(app.int("SELECT COUNT(*) FROM pack_import"), 0);
}

#[tokio::test]
async fn r84_markup_in_pack_text_is_stored_and_returned_as_plain_text() {
    let app = app();
    let mut w = word("hum", 6, hum_sentences());
    w["note"] = json!("<img src=x onerror=alert(1)> **bold**");
    w["sentences"][0] = sentence(
        "The <b>fridge</b> [[hummed]] & buzzed.",
        "Lednice <i>[[bzučela]]</i>.",
    );
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![w],
    )]))
    .await;
    let view = app.word("hum").await;
    assert_eq!(view["note"], "<img src=x onerror=alert(1)> **bold**");
    assert_eq!(view["sentences"][0]["en"]["pre"], "The <b>fridge</b> ");
    assert_eq!(view["sentences"][0]["cs"]["post"], "</i>.");
}

#[tokio::test]
async fn r24_import_log_holds_no_sentence_text() {
    let app = app();
    let mut w = word("hum", 6, hum_sentences());
    w["sentences"][1]["en"] = json!("A low [[hum]] and a [[buzz]] came.");
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![w],
    )]))
    .await;
    let stored = app
        .text("SELECT report_json FROM pack_import")
        .expect("report");
    assert!(stored.contains("\"hum\""), "headwords are kept: {stored}");
    for fragment in [
        "fridge",
        "engine room",
        "painted",
        "buzz",
        "bzučela",
        "Invented test meaning",
    ] {
        assert!(
            !stored.contains(fragment),
            "{fragment} leaked into {stored}"
        );
    }
    let (_, recent) = app.get("/api/import/recent").await;
    assert_eq!(recent[0]["summary"]["sentence_errors"], 1);
}

// ── merge rules (R-01, R-02, R-03, R-15, R-22) ──────────────────────────────

#[tokio::test]
async fn r22_update_the_sentences_reexport_merges_and_keeps_learning_state() {
    let app = app();
    let mut glint = word("glint", 5, three_sentences("glint"));
    glint["grammar"] = json!("noun");
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, hum_sentences()), glint],
    )]))
    .await;
    let hum_id = app.word_id("hum").await;

    // Learning state that a merge must never touch (owned by the scheduler
    // from M3; seeded directly here).
    let conn = app.sql();
    conn.execute(
        "INSERT INTO card (word_id, kind, state, stability, difficulty, due_day, reps, lapses, created_at) \
         VALUES (?1, 'understand', 'review', 12.5, 5.1, '2026-12-01', 4, 1, '2026-11-17T10:00:00Z')",
        [hum_id],
    )
    .expect("card");
    conn.execute(
        "INSERT INTO review_log (client_review_id, card_id, reviewed_at, review_day, raw_grade, fsrs_rating, \
         format, elapsed_days, before_json) VALUES ('test-review-1', 1, '2026-11-18T08:00:00Z', '2026-11-18', \
         2, 3, 'sentence', 1, '{}')",
        [],
    )
    .expect("review");
    let learning = |app: &TestApp| {
        app.sql()
            .query_row(
                "SELECT state, stability, difficulty, due_day, reps, lapses FROM card WHERE word_id = ?1",
                [hum_id],
                |r| {
                    Ok(format!(
                        "{:?}",
                        (
                            r.get::<_, String>(0)?,
                            r.get::<_, f64>(1)?,
                            r.get::<_, f64>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, i64>(4)?,
                            r.get::<_, i64>(5)?
                        )
                    ))
                },
            )
            .expect("card")
    };
    let before = learning(&app);

    // "I'm now at chapter 20, update the sentences": same words, earlier
    // sentences repeated (cut and typed slightly differently), new ones
    // added, and one scalar rephrased by Claude.
    let mut hum = word("hum", 6, vec![]);
    hum["meaning"] = json!("A rephrased invented meaning of hum.");
    hum["czech"] = json!(["hum-cz", "brumendo"]);
    hum["sentences"] = json!([
        sentence(
            "…the old fridge [[hummed]]  all night long.",
            "Stará lednice celou noc [[bzučela]]."
        ),
        sentence(
            "A low hum came from the engine [[room]].",
            "Ze strojovny se ozýval tichý bzukot, [[jinak]]."
        ),
        sentence(
            "The cables [[hummed]] in the rain.",
            "Kabely v dešti [[bzučely]]."
        ),
        sentence(
            "A faint [[hum]] filled the empty hall.",
            "Prázdnou halou se nesl slabý [[bzukot]]."
        ),
    ]);
    let mut glint2 = word("glint", 5, three_sentences("glint"));
    glint2["grammar"] = json!("  noun ");
    let reexport = paste(&[pack(
        "test-novel",
        "2026-11-25",
        1,
        (1, 1),
        vec![hum, glint2],
    )]);
    let report = app.commit(&reexport).await;

    assert_eq!(statuses(&report), ["imported"]);
    let totals = &report["totals"];
    assert_eq!(totals["new_words"], 0);
    assert_eq!(totals["merged_words"], 1);
    assert_eq!(
        totals["unchanged_words"], 1,
        "glint: same content, grammar equal after trim"
    );
    assert_eq!(totals["new_sentences"], 2);
    assert_eq!(totals["words_with_new_sentences"], 1);
    assert_eq!(totals["conflicts"], 1);
    let merge = &report["packs"][0]["words"][0]["merge"];
    assert_eq!(merge["sentences_known"], 2);
    assert_eq!(merge["items_added"]["czech"], 1);
    assert_eq!(merge["conflicts"][0]["field"], "meaning");
    assert_eq!(
        merge["conflicts"][0]["current"],
        "Invented test meaning of hum."
    );
    assert_eq!(
        merge["conflicts"][0]["incoming"],
        "A rephrased invented meaning of hum."
    );

    let view = app.word("hum").await;
    assert_eq!(
        view["meaning"], "Invented test meaning of hum.",
        "existing value kept"
    );
    assert_eq!(view["czech"], json!(["hum-cz", "brumendo"]));
    let sentences = view["sentences"].as_array().expect("sentences");
    assert_eq!(sentences.len(), 5);
    // Existing sentences unchanged (even where the re-export marked another span).
    assert_eq!(sentences[1]["en"]["target"], "hum");
    assert_eq!(sentences[3]["en"]["target"], "hummed");
    assert_eq!(sentences[3]["cs"]["target"], "bzučely");
    assert_eq!(view["conflicts"][0]["field"], "meaning");
    assert_eq!(learning(&app), before, "learning state untouched");
    assert_eq!(app.int("SELECT COUNT(*) FROM review_log"), 1);

    // The same re-export again: nothing at all happens.
    let dump = app.dump();
    let again = app.commit(&reexport).await;
    assert_eq!(statuses(&again), ["already_imported"]);
    assert_eq!(app.dump(), dump);
}

#[tokio::test]
async fn r03_scalar_rules_fill_equal_conflict_and_dedupe() {
    let app = app();
    let mut first = word("hum", 6, hum_sentences());
    first["hint"] = json!("Think of a fridge.");
    first["spelling"] = json!(false);
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![first],
    )]))
    .await;

    // Fill empty fields; equal after trim → nothing; different → conflict.
    let mut second = word("hum", 6, hum_sentences());
    second["grammar"] = json!("verb and noun");
    second["pronunciation"] = json!("HUM");
    second["goal"] = json!("active");
    second["hint"] = json!("  Think of a fridge.  ");
    second["usefulness"] = json!(8);
    second["spelling"] = json!(true);
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-18",
            1,
            (1, 1),
            vec![second.clone()],
        )]))
        .await;
    let merge = &report["packs"][0]["words"][0]["merge"];
    assert_eq!(merge["filled"], json!(["grammar", "pronunciation", "goal"]));
    let fields: Vec<&str> = merge["conflicts"]
        .as_array()
        .expect("conflicts")
        .iter()
        .map(|c| c["field"].as_str().expect("field"))
        .collect();
    assert_eq!(fields, ["usefulness", "spelling"]);
    let view = app.word("hum").await;
    assert_eq!(view["grammar"], "verb and noun");
    assert_eq!(view["goal"], "active");
    assert_eq!(view["usefulness"], 6, "kept");
    assert_eq!(view["spelling"], false, "kept");
    assert_eq!(view["conflicts"].as_array().expect("conflicts").len(), 2);
    assert_eq!(view["conflicts"][0]["incoming"], 8);

    // Same suggestion in another pack: the open conflict is not duplicated.
    let mut third = second.clone();
    third["note"] = json!("A new invented note.");
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-19",
            1,
            (1, 1),
            vec![third],
        )]))
        .await;
    let merge = &report["packs"][0]["words"][0]["merge"];
    assert_eq!(merge["filled"], json!(["note"]));
    assert!(
        merge["conflicts"]
            .as_array()
            .expect("c")
            .iter()
            .all(|c| c["already_open"] == true)
    );
    assert_eq!(report["totals"]["conflicts"], 0);
    assert_eq!(app.int("SELECT COUNT(*) FROM field_conflict"), 2);

    // A different suggestion for the same field is recorded.
    let mut fourth = second;
    fourth["usefulness"] = json!(9);
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-20",
        1,
        (1, 1),
        vec![fourth],
    )]))
    .await;
    assert_eq!(
        app.int("SELECT COUNT(*) FROM field_conflict WHERE field = 'usefulness'"),
        2
    );
}

#[tokio::test]
async fn r03_owner_overrides_are_kept_even_when_empty() {
    let app = app();
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, hum_sentences())],
    )]))
    .await;
    let id = app.word_id("hum").await;
    // The owner set usefulness and cleared the hint in the app (M7 writes
    // these; seeded directly here).
    let conn = app.sql();
    conn.execute(
        "UPDATE word SET usefulness = 3, hint = NULL WHERE id = ?1",
        [id],
    )
    .expect("update");
    conn.execute(
        "INSERT INTO word_override (word_id, field, set_at) VALUES (?1, 'usefulness', 'x'), (?1, 'hint', 'x')",
        [id],
    )
    .expect("override");
    let mut again = word("hum", 6, hum_sentences());
    again["hint"] = json!("A pack hint.");
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-18",
            1,
            (1, 1),
            vec![again],
        )]))
        .await;
    let merge = &report["packs"][0]["words"][0]["merge"];
    assert_eq!(merge["filled"], json!([]));
    let fields: Vec<&str> = merge["conflicts"]
        .as_array()
        .expect("conflicts")
        .iter()
        .map(|c| c["field"].as_str().expect("field"))
        .collect();
    assert_eq!(fields, ["usefulness", "hint"]);
    let view = app.word("hum").await;
    assert_eq!(view["usefulness"], 3);
    assert_eq!(view["hint"], Value::Null);
}

#[tokio::test]
async fn r03_lists_union_forms_keep_flag_and_removed_items_stay_removed() {
    let app = app();
    let mut first = word("hum", 6, hum_sentences());
    first["czech"] = json!(["bzučet", "hučet"]);
    first["accepted"] = json!(["hum", "hummed"]);
    first["confusables"] = json!(["hymn"]);
    first["forms"] =
        json!([{ "form": "hum", "common": true }, { "form": "hummed", "common": true }]);
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![first],
    )]))
    .await;
    let id = app.word_id("hum").await;
    app.sql()
        .execute(
            "UPDATE word_item SET removed = 1 WHERE word_id = ?1 AND value = 'hučet'",
            [id],
        )
        .expect("remove");

    let mut second = word("hum", 6, hum_sentences());
    second["czech"] = json!(["Bzučet ", "hučet", "brumlat"]);
    second["accepted"] = json!(["humming"]);
    second["confusables"] = json!(["HYMN", "hem"]);
    second["forms"] =
        json!([{ "form": "Hummed", "common": false }, { "form": "humming", "common": true }]);
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-18",
            1,
            (1, 1),
            vec![second],
        )]))
        .await;
    let added = &report["packs"][0]["words"][0]["merge"]["items_added"];
    assert_eq!(
        added,
        &json!({ "czech": 1, "accepted": 1, "confusables": 1, "forms": 1 })
    );
    let view = app.word("hum").await;
    assert_eq!(
        view["czech"],
        json!(["bzučet", "brumlat"]),
        "removed item not re-added"
    );
    assert_eq!(
        view["accepted"],
        json!(["hum", "hummed", "humming"]),
        "nothing deleted"
    );
    assert_eq!(view["confusables"], json!(["hymn", "hem"]));
    assert_eq!(
        view["forms"],
        json!([
            { "form": "hum", "common": true },
            { "form": "hummed", "common": true },
            { "form": "humming", "common": true }
        ])
    );
}

#[tokio::test]
async fn r22_hidden_and_edited_sentences_are_kept_as_they_are() {
    let app = app();
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, hum_sentences())],
    )]))
    .await;
    let id = app.word_id("hum").await;
    app.sql()
        .execute(
            "UPDATE sentence SET hidden = 1 WHERE word_id = ?1 AND position = 0",
            [id],
        )
        .expect("hide");
    app.sql()
        .execute(
            "UPDATE sentence SET cs_marked = 'Upravený [[překlad]].', edited = 1 WHERE word_id = ?1 AND position = 1",
            [id],
        )
        .expect("edit");
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-18",
            1,
            (1, 1),
            vec![word("hum", 6, hum_sentences())],
        )]))
        .await;
    assert_eq!(report["packs"][0]["words"][0]["outcome"], "unchanged");
    let view = app.word("hum").await;
    assert_eq!(view["sentences"][0]["hidden"], true);
    assert_eq!(view["sentences"][1]["cs"]["target"], "překlad");
    assert_eq!(view["sentences"].as_array().expect("s").len(), 3);
}

#[tokio::test]
async fn r02_a_word_from_a_second_source_is_linked_not_duplicated() {
    let app = app();
    let mut tagged = word("hum", 6, hum_sentences());
    tagged["tag"] = json!({ "kind": "low-priority", "text": "rare here" });
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("glint", 5, vec![]), tagged],
    )]))
    .await;
    assert_eq!(
        app.int("SELECT low_priority FROM word WHERE headword = 'hum'"),
        1
    );

    let mut film = pack(
        "test-film",
        "2026-11-18",
        1,
        (1, 1),
        vec![word(
            "Hum",
            6,
            vec![sentence(
                "The robot [[hummed]] happily.",
                "Robot spokojeně [[bzučel]].",
            )],
        )],
    );
    film["source"] = json!({ "id": "test-film", "type": "film", "title": "Test Film" });
    film.as_object_mut()
        .expect("pack")
        .remove("reading_position");
    let report = app.commit(&paste(&[film])).await;
    let merge = &report["packs"][0]["words"][0]["merge"];
    assert_eq!(merge["source_linked"], true);
    assert_eq!(report["packs"][0]["words"][0]["outcome"], "merged");

    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 2);
    let view = app.word("hum").await;
    assert_eq!(
        view["headword"], "hum",
        "displayed spelling of the existing word kept"
    );
    let sources: Vec<(String, i64)> = view["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .map(|s| {
            (
                s["title"].as_str().expect("t").to_owned(),
                s["seq"].as_i64().expect("seq"),
            )
        })
        .collect();
    assert_eq!(
        sources,
        [("Test Novel".to_owned(), 2), ("Test Film".to_owned(), 1)]
    );
    assert_eq!(view["sentences"].as_array().expect("s").len(), 4);
    assert_eq!(view["sentences"][3]["source_title"], "Test Film");
    // Seen again without the tag: promoted (R-51 data side).
    assert_eq!(view["low_priority"], false);

    // The tag of an existing link follows the latest import of that source.
    let mut retag = word("hum", 6, hum_sentences());
    retag["tag"] = json!({ "kind": "seen-before" });
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-19",
            1,
            (1, 1),
            vec![retag],
        )]))
        .await;
    assert_eq!(report["packs"][0]["words"][0]["merge"]["tag_changed"], true);
    assert_eq!(
        app.text("SELECT tag_kind FROM word_source ws JOIN source s ON s.id = ws.source_id WHERE s.slug = 'test-novel' AND ws.word_id = (SELECT id FROM word WHERE headword = 'hum')"),
        Some("seen-before".to_owned())
    );

    // Words listed per source, in source order.
    let (_, sources) = app.get("/api/sources").await;
    let novel = sources
        .as_array()
        .expect("sources")
        .iter()
        .find(|s| s["slug"] == "test-novel")
        .expect("novel");
    let (_, rows) = app.get(&format!("/api/words?source={}", novel["id"])).await;
    let order: Vec<&str> = rows
        .as_array()
        .expect("rows")
        .iter()
        .map(|r| r["headword"].as_str().expect("h"))
        .collect();
    assert_eq!(order, ["glint", "hum"]);
}

#[tokio::test]
async fn r15_low_priority_override_wins_over_tags() {
    let app = app();
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, hum_sentences())],
    )]))
    .await;
    let id = app.word_id("hum").await;
    app.sql()
        .execute(
            "INSERT INTO word_override (word_id, field, set_at) VALUES (?1, 'low_priority', 'x')",
            [id],
        )
        .expect("override");
    let mut tagged = word("hum", 6, hum_sentences());
    tagged["tag"] = json!({ "kind": "low-priority" });
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-18",
        1,
        (1, 1),
        vec![tagged],
    )]))
    .await;
    assert_eq!(app.int("SELECT low_priority FROM word"), 0);
}

#[tokio::test]
async fn r02_source_record_is_filled_but_never_overwritten() {
    let app = app();
    let mut first = pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, vec![])],
    );
    first["source"] = json!({ "id": "test-novel", "type": "book", "title": "Test Novel" });
    app.commit(&paste(&[first])).await;
    let mut second = pack(
        "test-novel",
        "2026-11-18",
        1,
        (1, 1),
        vec![word("glint", 5, vec![])],
    );
    second["source"] = json!({
        "id": "test-novel", "type": "film", "title": "Another Title",
        "author": "Test Author", "series": "Test Series", "series_number": 2
    });
    app.commit(&paste(&[second])).await;
    let (_, sources) = app.get("/api/sources").await;
    let s = &sources[0];
    assert_eq!(s["title"], "Test Novel");
    assert_eq!(s["type"], "book");
    assert_eq!(s["author"], "Test Author");
    assert_eq!(s["series"], "Test Series");
    assert_eq!(s["series_number"], 2);
    assert_eq!(s["word_count"], 2);
}

// ── order within a source (R-01, R-13) and reading position (R-59) ─────────

#[tokio::test]
async fn r13_multi_part_paste_in_any_order_gives_the_right_seq() {
    let app = app();
    let part = |i: u32, words: &[&str]| {
        pack(
            "test-novel",
            "2026-11-17",
            1,
            (i, 3),
            words.iter().map(|w| word(w, 5, vec![])).collect(),
        )
    };
    let p1 = part(1, &["alpha", "bravo"]);
    let p2 = part(2, &["charlie"]);
    let p3 = part(3, &["delta", "echo"]);
    let report = app.commit(&paste(&[p3, p1, p2])).await;
    assert_eq!(statuses(&report), ["imported", "imported", "imported"]);
    // A later export of the same source continues after them.
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-18",
        1,
        (1, 1),
        vec![word("foxtrot", 5, vec![])],
    )]))
    .await;
    let (_, rows) = app.get("/api/words?source=1").await;
    let order: Vec<(String, i64)> = rows
        .as_array()
        .expect("rows")
        .iter()
        .map(|r| {
            (
                r["headword"].as_str().expect("h").to_owned(),
                r["seq"].as_i64().expect("seq"),
            )
        })
        .collect();
    let expected: Vec<(String, i64)> = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
        .iter()
        .zip(1..)
        .map(|(w, s)| ((*w).to_owned(), s))
        .collect();
    assert_eq!(order, expected);
}

#[tokio::test]
async fn r01_explicit_seq_orders_words_of_converted_pages() {
    let app = app();
    let with_seq = |headword: &str, seq: u32| {
        let mut w = word(headword, 5, vec![]);
        w["seq"] = json!(seq);
        w
    };
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![
            with_seq("third", 30),
            with_seq("first", 10),
            with_seq("second", 20),
        ],
    )]))
    .await;
    // Without seq the next word goes after the highest one.
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-18",
        1,
        (1, 1),
        vec![word("later", 5, vec![])],
    )]))
    .await;
    let (_, rows) = app.get("/api/words?source=1").await;
    let order: Vec<(String, i64)> = rows
        .as_array()
        .expect("rows")
        .iter()
        .map(|r| {
            (
                r["headword"].as_str().expect("h").to_owned(),
                r["seq"].as_i64().expect("seq"),
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            ("first".to_owned(), 10),
            ("second".to_owned(), 20),
            ("third".to_owned(), 30),
            ("later".to_owned(), 31)
        ]
    );
}

#[tokio::test]
async fn r59_reading_position_follows_exported_at() {
    let app = app();
    let at = |date: &str, n: u32, position: &str| {
        let mut p = pack(
            "test-novel",
            date,
            n,
            (1, 1),
            vec![word(&format!("w{date}{n}"), 5, vec![])],
        );
        p["reading_position"] = json!(position);
        p
    };
    let position = |app: &TestApp| app.text("SELECT reading_position FROM source");

    app.commit(&paste(&[at("2026-11-20", 1, "Chapter 50")]))
        .await;
    assert_eq!(position(&app).as_deref(), Some("Chapter 50"));

    // An older export imported later does not move it back.
    let report = app
        .commit(&paste(&[at("2026-11-10", 1, "Chapter 30")]))
        .await;
    assert_eq!(report["packs"][0]["reading_position"]["updated"], false);
    assert_eq!(position(&app).as_deref(), Some("Chapter 50"));

    // Same date: the later import wins.
    let preview = app
        .preview(&paste(&[at("2026-11-20", 2, "Chapter 51")]))
        .await;
    assert_eq!(preview["packs"][0]["reading_position"]["updated"], true);
    app.commit(&paste(&[at("2026-11-20", 2, "Chapter 51")]))
        .await;
    assert_eq!(position(&app).as_deref(), Some("Chapter 51"));

    // No reading position: nothing changes.
    let mut silent = at("2026-11-25", 1, "x");
    silent["reading_position"] = Value::Null;
    app.commit(&paste(&[silent])).await;
    assert_eq!(position(&app).as_deref(), Some("Chapter 51"));

    // No exported_at: the import date counts (clock: 2026-11-20 → later than nothing newer).
    app.clock.set(ts("2026-11-30T09:00:00Z"));
    let mut undated = at("2026-11-26", 1, "Chapter 60");
    undated.as_object_mut().expect("pack").remove("exported_at");
    app.commit(&paste(&[undated])).await;
    assert_eq!(position(&app).as_deref(), Some("Chapter 60"));
    assert_eq!(
        app.text("SELECT reading_position_exported_at FROM source")
            .as_deref(),
        Some("2026-11-30")
    );

    // Two packs in one paste: the newer export wins regardless of paste order.
    let fresh = self::app();
    fresh
        .commit(&paste(&[
            at("2026-12-02", 1, "Chapter 70"),
            at("2026-12-01", 1, "Chapter 65"),
        ]))
        .await;
    assert_eq!(position(&fresh).as_deref(), Some("Chapter 70"));
    let (_, sources) = fresh.get("/api/sources").await;
    assert_eq!(sources[0]["reading_position"], "Chapter 70");
}

// ── word list and view ─────────────────────────────────────────────────────

#[tokio::test]
async fn r73_word_list_search_and_unknown_word() {
    let app = app();
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, hum_sentences()), word("glint", 5, vec![])],
    )]))
    .await;
    let (_, all) = app.get("/api/words").await;
    let names: Vec<&str> = all
        .as_array()
        .expect("rows")
        .iter()
        .map(|r| r["headword"].as_str().expect("h"))
        .collect();
    assert_eq!(names, ["glint", "hum"]);
    assert_eq!(all[1]["sentence_count"], 3);
    assert_eq!(all[1]["czech"], json!(["hum-cz"]));
    let (_, found) = app.get("/api/words?q=GLINT-cz").await;
    assert_eq!(found.as_array().expect("rows").len(), 1);
    let (_, none) = app.get("/api/words?q=zzz").await;
    assert_eq!(none, json!([]));
    let (status, _) = app.get("/api/words/999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app.get("/api/words/abc").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app.get("/api/words?source=abc").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ── M2 fix round (review findings F1–F12) ──────────────────────────────────

fn view_sources(view: &Value) -> Vec<(String, i64, Value)> {
    view["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .map(|s| {
            (
                s["title"].as_str().expect("title").to_owned(),
                s["seq"].as_i64().expect("seq"),
                s["tag_kind"].clone(),
            )
        })
        .collect()
}

#[tokio::test]
async fn r84_many_tiny_json_values_give_one_small_error() {
    // F1: 2 MB of `1 1 1 …` must not become one report entry per value.
    let app = app();
    let text = "1 ".repeat(1024 * 1024);
    let response = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/import/preview")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "text": text }).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = to_bytes(response.into_body(), 1 << 20).await.expect("body");
    assert!(bytes.len() < 1024, "response of {} bytes", bytes.len());
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["error"]["code"], "invalid_paste");
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("m")
            .contains("more than 20")
    );
}

#[tokio::test]
async fn r58_spelling_left_out_can_be_filled_later() {
    // F2: absent `spelling` is stored as unknown, not as false.
    let app = app();
    let mut first = word("hum", 6, hum_sentences());
    first["spelling_note"] = json!("double m");
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![first],
    )]))
    .await;
    assert_eq!(app.int("SELECT spelling IS NULL FROM word"), 1);
    let view = app.word("hum").await;
    assert_eq!(view["spelling"], false);
    assert_eq!(
        view["spelling_note"],
        Value::Null,
        "note without spelling: true not stored"
    );

    // A note alone still does not fill (spelling stays unknown).
    let mut note_only = word("hum", 6, hum_sentences());
    note_only["spelling_note"] = json!("double m");
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-18",
            1,
            (1, 1),
            vec![note_only],
        )]))
        .await;
    assert_eq!(report["packs"][0]["words"][0]["merge"]["filled"], json!([]));

    let mut second = word("hum", 6, hum_sentences());
    second["spelling"] = json!(true);
    second["spelling_note"] = json!("double m");
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-19",
            1,
            (1, 1),
            vec![second],
        )]))
        .await;
    let merge = &report["packs"][0]["words"][0]["merge"];
    assert_eq!(merge["filled"], json!(["spelling", "spelling_note"]));
    assert_eq!(merge["conflicts"], json!([]));
    let view = app.word("hum").await;
    assert_eq!(view["spelling"], true);
    assert_eq!(view["spelling_note"], "double m");
}

#[tokio::test]
async fn r01_explicit_seq_collisions_keep_link_creation_order() {
    // F3: an explicit seq only applies when the link is created; equal seq
    // values are ordered by link creation.
    let app = app();
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![
            word("alpha", 5, vec![]),
            word("bravo", 5, vec![]),
            word("charlie", 5, vec![]),
        ],
    )]))
    .await;
    let mut delta = word("delta", 5, vec![]);
    delta["seq"] = json!(2);
    let mut bravo = word("bravo", 5, vec![]);
    bravo["seq"] = json!(40);
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-18",
            1,
            (1, 1),
            vec![delta, bravo],
        )]))
        .await;
    assert_eq!(report["packs"][0]["words"][1]["outcome"], "unchanged");
    let (_, rows) = app.get("/api/words?source=1").await;
    let order: Vec<(String, i64)> = rows
        .as_array()
        .expect("rows")
        .iter()
        .map(|r| {
            (
                r["headword"].as_str().expect("h").to_owned(),
                r["seq"].as_i64().expect("s"),
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            ("alpha".to_owned(), 1),
            ("bravo".to_owned(), 2),
            ("delta".to_owned(), 2),
            ("charlie".to_owned(), 3)
        ]
    );
}

#[tokio::test]
async fn r11_preview_equals_commit_when_packs_of_one_paste_share_words() {
    // F4: the reviewer's hush/brandnew scenario.
    let app = app();
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-01",
        1,
        (1, 1),
        vec![word("hush", 5, three_sentences("hush"))],
    )]))
    .await;
    let extra = || sentence("New [[hush]] one.", "Nový [[ticho]].");
    let mut hush1 = word("hush", 5, vec![extra()]);
    hush1["hint"] = json!("H1");
    let mut hush2 = word("hush", 5, vec![extra()]);
    hush2["hint"] = json!("H2");
    let mut brandnew2 = word("brandnew", 5, three_sentences("brandnew"));
    brandnew2["meaning"] = json!("other meaning");
    let text = paste(&[
        pack(
            "test-novel",
            "2026-11-02",
            1,
            (1, 1),
            vec![hush1, word("brandnew", 5, three_sentences("brandnew"))],
        ),
        pack(
            "test-novel",
            "2026-11-03",
            1,
            (1, 1),
            vec![hush2, brandnew2],
        ),
    ]);
    let before = app.dump();
    let preview = app.preview(&text).await;
    assert_eq!(app.dump(), before, "preview stored nothing");
    let commit = app.commit(&text).await;
    let summary = |report: &Value| -> Vec<Value> {
        report["packs"]
            .as_array()
            .expect("packs")
            .iter()
            .flat_map(|p| p["words"].as_array().expect("words").clone())
            .map(|w| {
                json!([
                    w["headword"],
                    w["outcome"],
                    w["merge"]["filled"],
                    w["merge"]["conflicts"],
                    w["merge"]["sentences_added"],
                    w["merge"]["sentences_known"]
                ])
            })
            .collect()
    };
    assert_eq!(summary(&preview), summary(&commit));
    assert_eq!(preview["totals"], commit["totals"]);
    assert_eq!(statuses(&preview), ["ready", "ready"]);
    // Second pack: brandnew is merged (added by pack 1), hush's new sentence known.
    assert_eq!(commit["packs"][1]["words"][1]["outcome"], "merged");
    assert_eq!(
        commit["packs"][1]["words"][0]["merge"]["sentences_known"],
        1
    );
}

#[tokio::test]
async fn r11_a_timed_out_commit_still_imports_every_pack() {
    // F5: the request times out after 1 s while the database is busy; the
    // commit keeps running and finishes all packs.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(dir.path()).expect("db");
    let limits = Limits {
        import_commit_timeout_secs: 1,
        ..Limits::default()
    };
    let router = http::app(AppState::new(db.clone()), &limits);
    let blocker = {
        let db = db.clone();
        tokio::spawn(async move {
            db.read(|_| {
                std::thread::sleep(std::time::Duration::from_millis(1800));
                Ok::<_, vocab_server::db::DbError>(())
            })
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let text = paste(&[
        pack(
            "test-novel",
            "2026-11-17",
            1,
            (1, 3),
            vec![word("alpha", 5, vec![])],
        ),
        pack(
            "test-novel",
            "2026-11-17",
            1,
            (2, 3),
            vec![word("bravo", 5, vec![])],
        ),
        pack(
            "test-novel",
            "2026-11-17",
            1,
            (3, 3),
            vec![word("charlie", 5, vec![])],
        ),
    ]);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/import/commit")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "text": text }).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 16).await.expect("body"))
            .expect("json");
    let message = body["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("still being imported in the background"),
        "{message}"
    );
    assert!(
        message.contains("re-pasting the same text is safe"),
        "{message}"
    );
    blocker.await.expect("join").expect("blocker");

    let count = || {
        Connection::open(dir.path().join("vocabulary.sqlite"))
            .expect("open")
            .query_row("SELECT COUNT(*) FROM pack_import", [], |r| {
                r.get::<_, i64>(0)
            })
            .expect("count")
    };
    let mut waited = 0;
    while count() < 3 && waited < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 1;
    }
    assert_eq!(count(), 3, "every pack imported after the timeout");
    // Re-pasting is a no-op.
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/import/commit")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "text": text }).to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    let report: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.expect("body"))
            .expect("json");
    assert_eq!(
        statuses(&report),
        ["already_imported", "already_imported", "already_imported"]
    );
}

#[tokio::test]
async fn r01_absent_tag_keeps_the_tag_and_null_clears_it() {
    // F6
    let app = app();
    let mut tagged = word("loiter", 5, vec![]);
    tagged["tag"] = json!({ "kind": "low-priority", "text": "rare" });
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![tagged],
    )]))
    .await;
    // A re-export without a `tag` key: tag and low priority stay.
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-18",
            1,
            (1, 1),
            vec![word(
                "loiter",
                5,
                vec![sentence("More [[loitering]] here.", "Víc [[postávání]].")],
            )],
        )]))
        .await;
    assert_eq!(
        report["packs"][0]["words"][0]["merge"]["tag_changed"],
        false
    );
    let view = app.word("loiter").await;
    assert_eq!(view["low_priority"], true);
    assert_eq!(view_sources(&view)[0].2, "low-priority");
    // Explicit null clears it.
    let mut cleared = word("loiter", 5, vec![]);
    cleared["tag"] = Value::Null;
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-19",
            1,
            (1, 1),
            vec![cleared],
        )]))
        .await;
    assert_eq!(report["packs"][0]["words"][0]["merge"]["tag_changed"], true);
    let view = app.word("loiter").await;
    assert_eq!(view["low_priority"], false);
    assert_eq!(view_sources(&view)[0].2, Value::Null);
}

#[tokio::test]
async fn r03_invisible_characters_do_not_create_duplicate_words() {
    // F7
    let app = app();
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("loiter", 5, vec![]), word("wellbeing", 5, vec![])],
    )]))
    .await;
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-18",
            1,
            (1, 1),
            vec![
                word("loi\u{200B}ter", 5, vec![]),
                word("well\u{AD}being", 5, vec![]),
            ],
        )]))
        .await;
    assert_eq!(report["totals"]["new_words"], 0);
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 2);
}

#[tokio::test]
async fn r03_a_dismissed_suggestion_does_not_come_back() {
    // F11b
    let app = app();
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, hum_sentences())],
    )]))
    .await;
    let mut other = word("hum", 6, hum_sentences());
    other["meaning"] = json!("Suggestion A.");
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-18",
        1,
        (1, 1),
        vec![other.clone()],
    )]))
    .await;
    // The owner pressed "Keep" (M7 writes this; seeded directly here).
    app.sql()
        .execute(
            "UPDATE field_conflict SET status = 'dismissed', resolved_at = 'x'",
            [],
        )
        .expect("dismiss");
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-19",
            1,
            (1, 1),
            vec![other],
        )]))
        .await;
    assert_eq!(report["packs"][0]["words"][0]["outcome"], "unchanged");
    assert_eq!(
        report["packs"][0]["words"][0]["merge"]["conflicts"],
        json!([])
    );
    assert_eq!(app.int("SELECT COUNT(*) FROM field_conflict"), 1);
    // Another suggestion is new again.
    let mut third = word("hum", 6, hum_sentences());
    third["meaning"] = json!("Suggestion B.");
    let report = app
        .commit(&paste(&[pack(
            "test-novel",
            "2026-11-20",
            1,
            (1, 1),
            vec![third],
        )]))
        .await;
    assert_eq!(report["totals"]["conflicts"], 1);
    assert_eq!(
        app.int("SELECT COUNT(*) FROM field_conflict WHERE status = 'open'"),
        1
    );
}

#[tokio::test]
async fn r02_a_different_source_type_is_kept_with_a_warning() {
    // F11c
    let app = app();
    app.commit(&paste(&[pack(
        "test-novel",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, vec![])],
    )]))
    .await;
    let mut film = pack(
        "test-novel",
        "2026-11-18",
        1,
        (1, 1),
        vec![word("glint", 5, vec![])],
    );
    film["source"]["type"] = json!("film");
    film.as_object_mut()
        .expect("pack")
        .remove("reading_position");
    let report = app.commit(&paste(&[film])).await;
    let warnings = report["packs"][0]["warnings"].to_string();
    assert!(
        warnings.contains("/source/type") && warnings.contains("book"),
        "{warnings}"
    );
    assert_eq!(app.text("SELECT type FROM source").as_deref(), Some("book"));
}

#[tokio::test]
async fn r02_a_source_without_title_gets_a_default_title() {
    // F12: the exact shape of the owner's real general export.
    let app = app();
    let mut general = pack(
        "general",
        "2026-11-17",
        1,
        (1, 1),
        vec![word("hum", 6, hum_sentences())],
    );
    general["source"] = json!({ "id": "general", "type": "general" });
    general
        .as_object_mut()
        .expect("pack")
        .remove("reading_position");
    let preview = app.preview(&paste(&[general.clone()])).await;
    assert_eq!(statuses(&preview), ["ready"]);
    let report = app.commit(&paste(&[general])).await;
    assert_eq!(statuses(&report), ["imported"]);
    let warnings = report["packs"][0]["warnings"].to_string();
    assert!(
        warnings.contains("Source title missing; using \\\"General Vocabulary\\\""),
        "{warnings}"
    );
    assert_eq!(
        app.text("SELECT title FROM source").as_deref(),
        Some("General Vocabulary")
    );

    // An existing source keeps its title, without a warning.
    let mut again = pack(
        "general",
        "2026-11-18",
        1,
        (1, 1),
        vec![word("glint", 5, vec![])],
    );
    again["source"] = json!({ "id": "general", "type": "general" });
    again
        .as_object_mut()
        .expect("pack")
        .remove("reading_position");
    let report = app.commit(&paste(&[again])).await;
    assert_eq!(report["packs"][0]["warnings"], json!([]));
    let (_, recent) = app.get("/api/import/recent").await;
    assert_eq!(recent[0]["source_title"], "General Vocabulary");

    // Another type without a title is named after its id.
    let mut book = pack(
        "test-untitled",
        "2026-11-18",
        1,
        (1, 1),
        vec![word("wobble", 5, vec![])],
    );
    book["source"] = json!({ "id": "test-untitled", "type": "book" });
    let report = app.commit(&paste(&[book])).await;
    assert!(
        report["packs"][0]["warnings"]
            .to_string()
            .contains("test-untitled")
    );
    assert_eq!(
        app.text("SELECT title FROM source WHERE slug = 'test-untitled'")
            .as_deref(),
        Some("test-untitled")
    );
}
