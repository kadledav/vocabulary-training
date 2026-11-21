//! Import contract tests written from the requirements and
//! `docs/design/architecture.md` §5/§8 (not from the implementation).
//! Covers R-01, R-02, R-03, R-10, R-11, R-13, R-15, R-20, R-22, R-23, R-24,
//! R-59 and R-84 end to end through the HTTP router. All data is synthetic:
//! `test-…` ids and sentences invented for these tests (R-24, R-95).

mod common;

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
    let (auth, code) = common::long_auth();
    let state = AppState::with_clock(db, Arc::clone(&clock) as Arc<dyn Clock>, prague(), auth);
    let router = common::logged_in(http::app(state, &Limits::default()), code);
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
            // The test client's setup and login (auth_*) are not part of what an import may touch.
            .prepare(
                "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'auth\\_%' ESCAPE '\\' \
                 ORDER BY name",
            )
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

    /// The id of the word with this (NFC-normalised, lowercase) key.
    fn word_id_by_key(&self, key: &str) -> i64 {
        self.sql()
            .query_row("SELECT id FROM word WHERE headword_key = ?1", [key], |r| {
                r.get(0)
            })
            .unwrap_or_else(|e| panic!("no word with key {key:?}: {e}"))
    }

    async fn word_by_key(&self, key: &str) -> Value {
        let id = self.word_id_by_key(key);
        let (status, view) = self.get(&format!("/api/words/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        view
    }

    async fn sources(&self) -> Value {
        let (status, sources) = self.get("/api/sources").await;
        assert_eq!(status, StatusCode::OK);
        sources
    }
}

// ── synthetic packs ─────────────────────────────────────────────────────────

fn sentence(en: &str, cs: &str) -> Value {
    json!({ "en": en, "cs": cs, "origin": "example" })
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

fn word(headword: &str) -> Value {
    json!({
        "headword": headword,
        "meaning": format!("Invented test meaning of {headword}."),
        "czech": [format!("{headword}-cz")],
        "usefulness": 6,
        "sentences": three_sentences(headword),
    })
}

fn word_with(headword: &str, sentences: Vec<Value>) -> Value {
    let mut w = word(headword);
    w["sentences"] = Value::Array(sentences);
    w
}

fn pack(source: &str, date: &str, n: u32, part: (u32, u32), words: Vec<Value>) -> Value {
    json!({
        "format": "vocab-pack",
        "format_version": 1,
        "pack_id": format!("{source}--{date}--{n}"),
        "exported_at": date,
        "part": { "index": part.0, "total": part.1 },
        "source": { "id": source, "type": "book", "title": format!("Title of {source}") },
        "reading_position": "Chapter 10",
        "words": words,
    })
}

fn simple_pack(date: &str, n: u32, words: Vec<Value>) -> Value {
    pack("test-novel", date, n, (1, 1), words)
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
    out.push_str("That is all.\n");
    out
}

fn compact(pack: &Value) -> String {
    serde_json::to_string(pack).expect("json")
}

fn statuses(report: &Value) -> Vec<String> {
    report["packs"]
        .as_array()
        .expect("packs")
        .iter()
        .map(|p| p["status"].as_str().expect("status").to_owned())
        .collect()
}

fn sentence_texts(view: &Value) -> Vec<String> {
    view["sentences"]
        .as_array()
        .expect("sentences")
        .iter()
        .map(|s| {
            format!(
                "{}{}{}",
                s["en"]["pre"].as_str().expect("pre"),
                s["en"]["target"].as_str().expect("target"),
                s["en"]["post"].as_str().expect("post")
            )
        })
        .collect()
}

fn codes(issues: &Value) -> Vec<String> {
    issues
        .as_array()
        .expect("issues")
        .iter()
        .map(|i| i["code"].as_str().expect("code").to_owned())
        .collect()
}

// ── R-11: realistic pastes ─────────────────────────────────────────────────

#[tokio::test]
async fn r11_realistic_answer_with_prose_and_mixed_fences_previews_and_commits_alike() {
    let app = app();
    let p1 = simple_pack("2026-11-17", 1, vec![word("alpha"), word("beta")]);
    let p2 = simple_pack("2026-11-17", 2, vec![word("gamma")]);
    let p3 = pack("test-film", "2026-11-17", 1, (1, 1), vec![word("delta")]);
    let text = format!(
        "Sure! I exported 4 words in 3 packs. {{not json}} [1]\n\n**Pack 1**\n```json\n{}\n```\n\nNote: *gamma* was tricky.\n\n```\n{}\n```\n\n~~~json\n{}\n~~~\n\nShall I export more? {{\"x\": 1}}\n",
        serde_json::to_string_pretty(&p1).expect("json"),
        compact(&p2),
        serde_json::to_string_pretty(&p3).expect("json"),
    );
    let preview = app.preview(&text).await;
    assert_eq!(statuses(&preview), ["ready", "ready", "ready"]);
    assert_eq!(preview["totals"]["new_words"], 4);
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 0);

    let commit = app.commit(&text).await;
    assert_eq!(statuses(&commit), ["imported", "imported", "imported"]);
    assert_eq!(commit["totals"], preview["totals"]);
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 4);
    assert_eq!(app.int("SELECT COUNT(*) FROM source"), 2);
    assert_eq!(app.int("SELECT COUNT(*) FROM sentence"), 12);
}

#[tokio::test]
async fn r11_crlf_paste_and_bare_stream_of_packs_without_fences() {
    let app = app();
    let a = simple_pack("2026-11-17", 1, vec![word("alpha")]);
    let b = simple_pack("2026-11-17", 2, vec![word("beta")]);
    let text = format!(
        "Here:\r\n```json\r\n{}\r\n```\r\n",
        serde_json::to_string_pretty(&a)
            .expect("json")
            .replace('\n', "\r\n")
    );
    let report = app.commit(&text).await;
    assert_eq!(statuses(&report), ["imported"]);

    let bare = format!("{}\n\n{}", compact(&a), compact(&b));
    let report = app.commit(&bare).await;
    assert_eq!(statuses(&report), ["already_imported", "imported"]);
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 2);
}

#[tokio::test]
async fn r11_pack_text_with_triple_backticks_inside_a_string_survives_the_fence_scan() {
    let app = app();
    let mut w = word("alpha");
    w["note"] = json!("Wrap code in ``` fences.\nSecond line ```");
    let text = paste(&[simple_pack("2026-11-17", 1, vec![w])]);
    let report = app.commit(&text).await;
    assert_eq!(statuses(&report), ["imported"]);
    let view = app.word_by_key("alpha").await;
    assert_eq!(view["note"], "Wrap code in ``` fences.\nSecond line ```");
}

#[tokio::test]
async fn r23_paste_of_only_prose_or_only_whitespace_is_a_clean_error() {
    let app = app();
    for text in ["", "   \n\t ", "Sorry, I could not export anything."] {
        let (status, body) = app
            .post("/api/import/preview", &json!({ "text": text }))
            .await;
        // Either a JSON error or a report whose only pack is unreadable; never a 5xx.
        assert!(
            status.is_client_error() || status == StatusCode::OK,
            "{text:?}: {status} {body}"
        );
        if status.is_client_error() {
            assert!(body["error"]["code"].is_string(), "{body}");
        } else {
            assert_eq!(statuses(&body), ["unreadable"], "{body}");
        }
    }
    assert_eq!(app.int("SELECT COUNT(*) FROM pack_import"), 0);
}

#[tokio::test]
async fn r84_a_paste_of_exactly_2_mib_is_accepted() {
    // design §5.2: "Total paste <= 2 MiB". The HTTP body that carries it is
    // the paste inside a JSON string, so the body limit must leave room for it.
    let app = app();
    let body = compact(&simple_pack("2026-11-17", 1, vec![word("alpha")]));
    let limit = 2 * 1024 * 1024;
    let text = format!("{body}{}", " ".repeat(limit - body.len()));
    assert_eq!(text.len(), limit);
    let (status, report) = app
        .post("/api/import/preview", &json!({ "text": text }))
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(statuses(&report), ["ready"]);
}

#[tokio::test]
async fn r84_a_large_pretty_printed_paste_below_2_mib_is_accepted() {
    // A realistic big export: pretty-printed, so quotes and newlines are
    // escaped in the request body. 1.6 MiB of text is well under the paste
    // limit even after escaping.
    let app = app();
    let mut packs = Vec::new();
    for n in 1..=3u32 {
        let words: Vec<Value> = (0..100)
            .map(|i| {
                let mut w = word(&format!("w{n}x{i}"));
                w["note"] = json!("n".repeat(5000));
                w
            })
            .collect();
        packs.push(simple_pack("2026-11-17", n, words));
    }
    let text = paste(&packs);
    assert!(
        text.len() > 1_500_000 && text.len() < 2 * 1024 * 1024,
        "{}",
        text.len()
    );
    let (status, report) = app
        .post("/api/import/preview", &json!({ "text": text }))
        .await;
    assert_eq!(status, StatusCode::OK, "{status} {report}");
    assert_eq!(report["totals"]["new_words"], 300);
}

// ── R-10 / R-23: version, unknown fields, warnings ─────────────────────────

#[tokio::test]
async fn r10_format_version_2_is_rejected_with_advice_and_writes_nothing() {
    let app = app();
    let mut future = simple_pack("2026-11-17", 1, vec![word("alpha")]);
    future["format_version"] = json!(2);
    let good = pack("test-film", "2026-11-17", 1, (1, 1), vec![word("beta")]);
    let before = app.dump();
    let preview = app.preview(&paste(&[future.clone(), good.clone()])).await;
    assert_eq!(statuses(&preview), ["rejected", "ready"]);
    assert_eq!(app.dump(), before, "preview writes nothing");

    let report = app.commit(&paste(&[future, good])).await;
    assert_eq!(statuses(&report), ["rejected", "imported"]);
    let error = &report["packs"][0]["errors"][0];
    assert_eq!(error["code"], "unknown_version");
    let message = error["message"].as_str().expect("message").to_lowercase();
    assert!(message.contains("format 2"), "{message}");
    assert!(message.contains("update the app"), "{message}");
    assert!(message.contains("re-paste"), "{message}");
    // The rejected pack left no trace: no source, no word, no import row.
    assert_eq!(
        app.int("SELECT COUNT(*) FROM source WHERE slug = 'test-novel'"),
        0
    );
    assert_eq!(
        app.int("SELECT COUNT(*) FROM word WHERE headword = 'alpha'"),
        0
    );
    assert_eq!(app.int("SELECT COUNT(*) FROM pack_import"), 1);
}

#[tokio::test]
async fn r10_unknown_fields_are_warnings_and_the_pack_still_imports() {
    let app = app();
    let mut p = simple_pack("2026-11-17", 1, vec![word("alpha")]);
    p["future_pack_field"] = json!("x");
    p["source"]["future_source_field"] = json!(1);
    p["words"][0]["future_word_field"] = json!([1]);
    p["words"][0]["sentences"][0]["future_sentence_field"] = json!(true);
    let preview = app.preview(&paste(std::slice::from_ref(&p))).await;
    assert_eq!(statuses(&preview), ["ready"]);
    assert_eq!(preview["packs"][0]["errors"], json!([]));
    let pack_codes = codes(&preview["packs"][0]["warnings"]);
    assert!(
        pack_codes.contains(&"unknown_property".to_owned()),
        "{pack_codes:?}"
    );
    let word_report = &preview["packs"][0]["words"][0];
    assert_eq!(word_report["outcome"], "new");
    assert!(codes(&word_report["warnings"]).contains(&"unknown_property".to_owned()));
    assert!(preview["totals"]["warnings"].as_u64().expect("n") >= 3);

    let report = app.commit(&paste(&[p])).await;
    assert_eq!(statuses(&report), ["imported"]);
    assert_eq!(app.int("SELECT COUNT(*) FROM sentence"), 3);
}

#[tokio::test]
async fn r20_words_with_fewer_than_three_sentences_import_with_a_warning() {
    let app = app();
    let w0 = word_with("none", vec![]);
    let w1 = word_with("one", three_sentences("one")[..1].to_vec());
    let w2 = word_with("two", three_sentences("two")[..2].to_vec());
    let mut broken = three_sentences("broken");
    broken[2]["en"] = json!("There is no span here.");
    let w3 = word_with("broken", broken);
    let w4 = word("fine");
    let p = simple_pack("2026-11-17", 1, vec![w0, w1, w2, w3, w4]);
    let report = app.commit(&paste(&[p])).await;
    let words = report["packs"][0]["words"].as_array().expect("words");
    for (i, expected) in [true, true, true, true, false].into_iter().enumerate() {
        let has = codes(&words[i]["warnings"]).contains(&"few_sentences".to_owned());
        assert_eq!(has, expected, "word {i}: {}", words[i]);
        assert_eq!(words[i]["outcome"], "new");
    }
    // The broken sentence is reported (not silently dropped) and the word imported.
    assert_eq!(
        words[3]["sentence_errors"].as_array().expect("errs").len(),
        1
    );
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 5);
    assert_eq!(app.int("SELECT COUNT(*) FROM sentence"), 1 + 2 + 2 + 3);
}

#[tokio::test]
async fn r23_sentence_and_word_errors_are_listed_and_do_not_block_the_rest() {
    let app = app();
    let mut bad_sentences = three_sentences("alpha");
    bad_sentences[0]["en"] = json!("Two [[spans]] and [[more]].");
    bad_sentences[1]["cs"] = json!("Bez značky.");
    bad_sentences[2]["origin"] = json!("invented");
    let mut bad_word = word("beta");
    bad_word["usefulness"] = json!(11);
    let p = simple_pack(
        "2026-11-17",
        1,
        vec![word_with("alpha", bad_sentences), bad_word, word("gamma")],
    );
    let preview = app.preview(&paste(std::slice::from_ref(&p))).await;
    let words = preview["packs"][0]["words"].as_array().expect("words");
    assert_eq!(words[0]["sentence_errors"].as_array().expect("e").len(), 3);
    assert_eq!(words[1]["outcome"], "invalid");
    assert_eq!(words[2]["outcome"], "new");
    assert_eq!(preview["totals"]["sentence_errors"], 3);
    assert_eq!(preview["totals"]["invalid_words"], 1);
    app.commit(&paste(&[p])).await;
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 2);
    assert_eq!(app.int("SELECT COUNT(*) FROM sentence"), 3);
}

#[tokio::test]
async fn r23_a_repeated_headword_in_one_pack_reports_the_second_and_imports_the_first() {
    let app = app();
    let p = simple_pack(
        "2026-11-17",
        1,
        vec![word("Pore over"), word("alpha"), word("  pore   OVER ")],
    );
    let report = app.commit(&paste(&[p])).await;
    let words = report["packs"][0]["words"].as_array().expect("words");
    assert_eq!(words[0]["outcome"], "new");
    assert_eq!(words[2]["outcome"], "invalid");
    assert_eq!(codes(&words[2]["errors"]), ["duplicate_word"]);
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 2);
}

// ── R-02 / R-03: merging the same word from different sources ──────────────

#[tokio::test]
async fn r03_same_headword_from_two_sources_is_one_word_with_sentences_from_both() {
    let app = app();
    let mut book_word = word_with(
        "Pore over",
        vec![
            sentence(
                "She would [[pore over]] the maps for hours.",
                "Celé hodiny [[studovala]] mapy.",
            ),
            sentence(
                "He began to [[pore over]] the ledger.",
                "Začal se [[pročítat]] účetní knihou.",
            ),
        ],
    );
    book_word["czech"] = json!(["pročítat", "studovat"]);
    book_word["tag"] = json!({ "kind": "low-priority", "text": "rare here" });
    let book = simple_pack("2026-11-17", 1, vec![book_word]);
    app.commit(&paste(&[book])).await;

    let mut article_word = word_with(
        "pore  OVER",
        vec![sentence(
            "Readers [[pore over]] the article.",
            "Čtenáři článek [[pečlivě studují]].",
        )],
    );
    article_word["czech"] = json!(["Studovat ", "pečlivě zkoumat"]);
    article_word["meaning"] = json!("A different invented meaning.");
    let mut article = pack("test-article", "2026-11-18", 1, (1, 1), vec![article_word]);
    article["source"] = json!({ "id": "test-article", "type": "article", "title": "Test Article" });
    article
        .as_object_mut()
        .expect("o")
        .remove("reading_position");
    let preview = app.preview(&paste(&[article.clone()])).await;
    assert_eq!(preview["packs"][0]["words"][0]["outcome"], "merged");
    let report = app.commit(&paste(&[article])).await;
    assert_eq!(report["totals"]["new_words"], 0);
    assert_eq!(report["totals"]["merged_words"], 1);
    assert_eq!(report["totals"]["new_sentences"], 1);

    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 1, "one word, not two");
    let view = app.word_by_key("pore over").await;
    assert_eq!(view["headword"], "Pore over", "first spelling kept");
    assert_eq!(view["sources"].as_array().expect("s").len(), 2);
    assert_eq!(view["sentences"].as_array().expect("s").len(), 3);
    let titles: Vec<&str> = view["sentences"]
        .as_array()
        .expect("s")
        .iter()
        .map(|s| s["source_title"].as_str().expect("t"))
        .collect();
    assert_eq!(
        titles,
        ["Title of test-novel", "Title of test-novel", "Test Article"]
    );
    // Czech: union by key, existing first, new appended, nothing lost.
    assert_eq!(
        view["czech"],
        json!(["pročítat", "studovat", "pečlivě zkoumat"])
    );
    // Conflicting scalar: existing kept, conflict recorded.
    assert_eq!(view["meaning"], "Invented test meaning of Pore over.");
    let conflicts = view["conflicts"].as_array().expect("c");
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["field"], "meaning");
    assert_eq!(conflicts[0]["incoming"], "A different invented meaning.");
    // Per-source link: tag only on the book link.
    let by_title = |t: &str| {
        view["sources"]
            .as_array()
            .expect("s")
            .iter()
            .find(|s| s["title"] == t)
            .unwrap_or_else(|| panic!("{t}"))
            .clone()
    };
    assert_eq!(by_title("Title of test-novel")["tag_kind"], "low-priority");
    assert_eq!(by_title("Test Article")["tag_kind"], Value::Null);
    // Two source rows, each counting the word once.
    let sources = app.sources().await;
    for s in sources.as_array().expect("s") {
        assert_eq!(s["word_count"], 1, "{s}");
    }
}

#[tokio::test]
async fn r03_headword_variants_case_whitespace_nfd_apostrophe_hyphen_are_one_word() {
    let app = app();
    let first = [
        "Café au lait",
        "don't",
        "carpet-bomb",
        "pore over",
        "Zlutoucky",
    ];
    app.commit(&paste(&[simple_pack(
        "2026-11-17",
        1,
        first.iter().map(|h| word(h)).collect(),
    )]))
    .await;
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 5);

    let again = [
        "CAFE\u{301} AU  LAIT", // NFD + case + inner whitespace
        "don\u{2019}t",         // typographic apostrophe
        "carpet\u{2010}bomb",   // U+2010 hyphen
        "\tPore\u{a0}Over\n",   // tab, NBSP, newline
        "zlutoucky",
    ];
    let report = app
        .commit(&paste(&[simple_pack(
            "2026-11-18",
            1,
            again.iter().map(|h| word_with(h, vec![])).collect(),
        )]))
        .await;
    assert_eq!(report["totals"]["new_words"], 0, "{}", report["totals"]);
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 5);
    // But these are different words (needs judgement, not merged).
    let report = app
        .commit(&paste(&[simple_pack(
            "2026-11-19",
            1,
            vec![
                word_with("carpet bomb", vec![]),
                word_with("café", vec![]),
                word_with("cafe au lait", vec![]),
            ],
        )]))
        .await;
    assert_eq!(report["totals"]["new_words"], 3);
    assert_eq!(app.int("SELECT COUNT(*) FROM word"), 8);
}

#[tokio::test]
async fn r03_nfd_text_is_stored_as_nfc_and_merges_with_nfc() {
    let app = app();
    let mut nfd = word_with(
        "cafe\u{301}",
        vec![sentence(
            "A cafe\u{301} on the [[corner]].",
            "Kavárna na [[rohu]], zlu\u{165}ou\u{10d}ký.",
        )],
    );
    nfd["czech"] = json!(["kavarna", "z\u{30c}lutý"]);
    app.commit(&paste(&[simple_pack("2026-11-17", 1, vec![nfd])]))
        .await;
    let stored = app.text("SELECT headword FROM word").expect("headword");
    assert_eq!(stored, "caf\u{e9}", "headword stored NFC");
    let view = app.word_by_key("caf\u{e9}").await;
    assert_eq!(view["czech"][1], "žlutý");
    assert_eq!(sentence_texts(&view)[0], "A caf\u{e9} on the corner.");

    // The same word and sentence arrive in NFC: nothing new.
    let mut nfc = word_with(
        "caf\u{e9}",
        vec![sentence("A caf\u{e9} on the [[corner]].", "Jiná [[věta]].")],
    );
    nfc["czech"] = json!(["kavarna", "žlutý"]);
    let report = app
        .commit(&paste(&[simple_pack("2026-11-18", 1, vec![nfc])]))
        .await;
    let merge = &report["packs"][0]["words"][0]["merge"];
    assert_eq!(merge["sentences_added"], 0, "{merge}");
    assert_eq!(merge["items_added"]["czech"], 0, "{merge}");
    assert_eq!(app.int("SELECT COUNT(*) FROM sentence"), 1);
    assert_eq!(
        app.int("SELECT COUNT(*) FROM word_item WHERE kind = 'czech'"),
        2
    );
}

// ── R-22: sentence dedupe ──────────────────────────────────────────────────

#[tokio::test]
async fn r22_sentences_that_differ_only_in_typography_are_not_added_again() {
    let app = app();
    let first = word_with(
        "hum",
        vec![
            sentence("He said, \u{201c}[[hum]] it.\u{201d}", "Řekl: „[[Bzuč]].“"),
            sentence("\u{2026}and the [[hum]] stopped.", "…a [[bzukot]] ustal."),
            sentence(
                "It\u{2019}s a low [[hum]] today.",
                "Dnes je slyšet [[bzukot]].",
            ),
            sentence("A plain [[hum]] sound.", "Obyčejný [[bzukot]]."),
        ],
    );
    app.commit(&paste(&[simple_pack("2026-11-17", 1, vec![first])]))
        .await;

    let second = word_with(
        "hum",
        vec![
            sentence("He said, \"[[hum]] it.\"", "Řekl: \"[[Bzukej]].\""),
            sentence("...and the [[hum]] stopped.", "...a [[hučení]] ustalo."),
            sentence("It's a low [[hum]] today.", "Dnes je [[hluk]]."),
            sentence("  a PLAIN   [[hum]] sound. ", "Něco [[jiného]]."),
            sentence("and the [[hum]] stopped.\u{2026}", "x [[y]]"),
            sentence("Hum[[ming]] is a new sentence.", "Zcela [[nová]] věta."),
        ],
    );
    let preview = app
        .preview(&paste(&[simple_pack(
            "2026-11-18",
            1,
            vec![second.clone()],
        )]))
        .await;
    assert_eq!(
        preview["totals"]["new_sentences"], 1,
        "{}",
        preview["totals"]
    );
    let report = app
        .commit(&paste(&[simple_pack("2026-11-18", 1, vec![second])]))
        .await;
    assert_eq!(report["totals"]["new_sentences"], 1);
    assert_eq!(app.int("SELECT COUNT(*) FROM sentence"), 5);

    // No sentence was added twice; the ones whose Czech the second pack
    // changed carry the pack's version now (R-22: corrections reach the
    // app), in the same rows.
    assert_eq!(report["totals"]["updated_sentences"], 4);
    let view = app.word_by_key("hum").await;
    assert_eq!(view["sentences"][0]["cs"]["target"], "Bzukej");
    assert_eq!(view["sentences"][0]["en"]["pre"], "He said, \"");
    assert_eq!(view["sentences"][1]["cs"]["target"], "hučení");
    assert_eq!(view["sentences"][3]["cs"]["target"], "jiného");
    assert_eq!(view["sentences"][4]["en"]["target"], "ming");
}

#[tokio::test]
async fn r22_the_marked_span_position_does_not_make_a_sentence_new() {
    let app = app();
    let a = word_with(
        "hum",
        vec![sentence(
            "The engine [[hummed]] softly.",
            "Motor tiše [[bzučel]].",
        )],
    );
    app.commit(&paste(&[simple_pack("2026-11-17", 1, vec![a])]))
        .await;
    let b = word_with(
        "hum",
        vec![sentence(
            "The engine hum[[med]] softly.",
            "Motor tiše [[bzučel]].",
        )],
    );
    let report = app
        .commit(&paste(&[simple_pack("2026-11-18", 1, vec![b])]))
        .await;
    assert_eq!(report["totals"]["new_sentences"], 0);
    let view = app.word_by_key("hum").await;
    assert_eq!(view["sentences"].as_array().expect("s").len(), 1);
    // The same sentence, with the span as the newer pack marks it (R-22).
    assert_eq!(report["totals"]["updated_sentences"], 1);
    assert_eq!(view["sentences"][0]["en"]["target"], "med");
}

#[tokio::test]
async fn r22_two_sentences_with_the_same_key_in_one_pack_import_once() {
    let app = app();
    let w = word_with(
        "hum",
        vec![
            sentence("The [[hum]] stopped.", "[[Bzukot]] ustal."),
            sentence("The [[hum]] stopped.\u{2026}", "Jiný [[překlad]]."),
            sentence("Another [[hum]] began.", "Začal další [[bzukot]]."),
        ],
    );
    let report = app
        .commit(&paste(&[simple_pack("2026-11-17", 1, vec![w])]))
        .await;
    let word_report = &report["packs"][0]["words"][0];
    assert!(codes(&word_report["warnings"]).contains(&"duplicate_sentence".to_owned()));
    assert_eq!(report["totals"]["new_sentences"], 2);
    assert_eq!(app.int("SELECT COUNT(*) FROM sentence"), 2);
}

// ── R-03: lists and owner removals ─────────────────────────────────────────

#[tokio::test]
async fn r03_owner_removed_items_are_not_re_added_even_in_another_spelling() {
    let app = app();
    let mut first = word("hum");
    first["czech"] = json!(["bzučet", "hučet", "brumlat"]);
    first["accepted"] = json!(["hum", "hummed"]);
    first["confusables"] = json!(["hymn", "him"]);
    app.commit(&paste(&[simple_pack("2026-11-17", 1, vec![first])]))
        .await;
    let id = app.word_id_by_key("hum");
    for (kind, value) in [
        ("czech", "hučet"),
        ("accepted", "hummed"),
        ("confusable", "hymn"),
    ] {
        let n = app
            .sql()
            .execute(
                "UPDATE word_item SET removed = 1 WHERE word_id = ?1 AND kind = ?2 AND value = ?3",
                rusqlite::params![id, kind, value],
            )
            .expect("remove");
        assert_eq!(n, 1, "{kind} {value}");
    }
    let mut second = word("hum");
    second["czech"] = json!(["HUČET", " Hu\u{10d}et", "nový"]);
    second["accepted"] = json!(["Hummed", "humming"]);
    second["confusables"] = json!(["HYMN", "hem"]);
    app.commit(&paste(&[simple_pack("2026-11-18", 1, vec![second])]))
        .await;
    let view = app.word_by_key("hum").await;
    assert_eq!(view["czech"], json!(["bzučet", "brumlat", "nový"]));
    assert_eq!(view["accepted"], json!(["hum", "humming"]));
    assert_eq!(view["confusables"], json!(["him", "hem"]));
}

#[tokio::test]
async fn r03_a_pack_never_deletes_list_items_or_forms() {
    let app = app();
    let mut first = word("hum");
    first["czech"] = json!(["a", "b", "c"]);
    first["forms"] = json!([{ "form": "hums", "common": true }]);
    app.commit(&paste(&[simple_pack("2026-11-17", 1, vec![first])]))
        .await;
    let mut second = word("hum");
    second["czech"] = json!(["d"]);
    second["forms"] = json!([]);
    app.commit(&paste(&[simple_pack("2026-11-18", 1, vec![second])]))
        .await;
    let view = app.word_by_key("hum").await;
    assert_eq!(view["czech"], json!(["a", "b", "c", "d"]));
    assert_eq!(view["forms"], json!([{ "form": "hums", "common": true }]));
}

#[tokio::test]
async fn r03_absent_or_null_fields_in_a_later_pack_change_nothing() {
    let app = app();
    let mut first = word("hum");
    first["hint"] = json!("sounds like a bee");
    first["note"] = json!("a note");
    first["goal"] = json!("active");
    first["icon"] = json!("🐝");
    app.commit(&paste(&[simple_pack("2026-11-17", 1, vec![first])]))
        .await;
    let mut second = word("hum");
    second["hint"] = Value::Null;
    second["note"] = json!("");
    second["meaning"] = json!("Invented test meaning of hum.");
    let report = app
        .commit(&paste(&[simple_pack("2026-11-18", 1, vec![second])]))
        .await;
    assert_eq!(report["totals"]["conflicts"], 0, "{}", report["totals"]);
    let view = app.word_by_key("hum").await;
    assert_eq!(view["hint"], "sounds like a bee");
    assert_eq!(view["note"], "a note");
    assert_eq!(view["goal"], "active");
    assert_eq!(view["icon"], "🐝");
}

// ── R-13 / R-01: order, parts, seq ─────────────────────────────────────────

#[tokio::test]
async fn r13_parts_of_two_sources_pasted_in_a_scrambled_order_keep_each_sources_order() {
    let app = app();
    let part = |source: &str, n: u32, idx: u32, names: [&str; 2]| {
        pack(
            source,
            "2026-11-17",
            n,
            (idx, 3),
            names.iter().map(|h| word_with(h, vec![])).collect(),
        )
    };
    let text = paste(&[
        part("test-b", 1, 3, ["b5", "b6"]),
        part("test-a", 1, 2, ["a3", "a4"]),
        part("test-b", 1, 1, ["b1", "b2"]),
        part("test-a", 1, 3, ["a5", "a6"]),
        part("test-b", 1, 2, ["b3", "b4"]),
        part("test-a", 1, 1, ["a1", "a2"]),
    ]);
    app.commit(&text).await;
    for (slug, prefix) in [("test-a", "a"), ("test-b", "b")] {
        let rows: Vec<(String, i64)> = app
            .sql()
            .prepare(
                "SELECT w.headword, ws.seq FROM word_source ws \
                 JOIN word w ON w.id = ws.word_id JOIN source s ON s.id = ws.source_id \
                 WHERE s.slug = ?1 ORDER BY ws.seq",
            )
            .expect("stmt")
            .query_map([slug], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("rows")
            .map(|r| r.expect("row"))
            .collect();
        let expected: Vec<(String, i64)> = (1..=6)
            .map(|i| (format!("{prefix}{i}"), i64::from(i)))
            .collect();
        assert_eq!(rows, expected, "{slug}");
    }
}

#[tokio::test]
async fn r13_pack_number_and_export_date_order_packs_of_one_source() {
    let app = app();
    let at = |date: &str, n: u32, name: &str| {
        pack("test-a", date, n, (1, 1), vec![word_with(name, vec![])])
    };
    // Pasted: newest date first, then number 2 before 1.
    app.commit(&paste(&[
        at("2026-11-19", 1, "third"),
        at("2026-11-17", 2, "second"),
        at("2026-11-17", 1, "first"),
    ]))
    .await;
    let order: Vec<String> = app
        .sql()
        .prepare("SELECT w.headword FROM word_source ws JOIN word w ON w.id = ws.word_id ORDER BY ws.seq")
        .expect("stmt")
        .query_map([], |r| r.get(0))
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(order, ["first", "second", "third"]);
}

#[tokio::test]
async fn r01_explicit_and_implicit_seq_mix_within_one_pack() {
    let app = app();
    let mut explicit = word_with("explicit", vec![]);
    explicit["seq"] = json!(10);
    let p = simple_pack(
        "2026-11-17",
        1,
        vec![
            word_with("first", vec![]),
            explicit,
            word_with("after", vec![]),
        ],
    );
    app.commit(&paste(&[p])).await;
    let seq = |h: &str| {
        app.int(&format!(
            "SELECT ws.seq FROM word_source ws JOIN word w ON w.id = ws.word_id WHERE w.headword = '{h}'"
        ))
    };
    assert_eq!(seq("first"), 1);
    assert_eq!(seq("explicit"), 10);
    assert!(
        seq("after") > 10,
        "implicit order continues after the highest seq"
    );
}

#[tokio::test]
async fn r01_a_word_already_in_a_source_keeps_its_seq_on_reimport() {
    let app = app();
    let mut a = word_with("alpha", vec![]);
    a["seq"] = json!(5);
    app.commit(&paste(&[simple_pack("2026-11-17", 1, vec![a])]))
        .await;
    let mut again = word_with("alpha", vec![]);
    again["seq"] = json!(99);
    let report = app
        .commit(&paste(&[simple_pack("2026-11-18", 1, vec![again])]))
        .await;
    assert_eq!(
        report["packs"][0]["words"][0]["merge"]["source_linked"],
        false
    );
    assert_eq!(
        app.int("SELECT seq FROM word_source"),
        5,
        "design §5.4: an existing link keeps its seq"
    );
}

#[tokio::test]
async fn r13_converted_old_page_keeps_tags_icons_and_order() {
    // An old page converted by Claude: explicit seq, all three tag kinds, a tag with text.
    let app = app();
    let mut w1 = word_with("alpha", vec![]);
    w1["seq"] = json!(3);
    w1["tag"] = json!({ "kind": "low-priority", "text": "rare" });
    w1["icon"] = json!("sparkles");
    let mut w2 = word_with("beta", vec![]);
    w2["seq"] = json!(1);
    w2["tag"] = json!({ "kind": "seen-before", "text": "also in book 1" });
    w2["icon"] = json!("🐝");
    let mut w3 = word_with("gamma", vec![]);
    w3["seq"] = json!(2);
    w3["tag"] = json!({ "kind": "not-from-source" });
    let w4 = word_with("delta", vec![]);
    app.commit(&paste(&[simple_pack(
        "2026-11-17",
        1,
        vec![w1, w2, w3, w4],
    )]))
    .await;
    let rows: Vec<(String, i64, Option<String>, Option<String>)> = app
        .sql()
        .prepare(
            "SELECT w.headword, ws.seq, ws.tag_kind, ws.tag_text FROM word_source ws \
             JOIN word w ON w.id = ws.word_id ORDER BY ws.seq",
        )
        .expect("stmt")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(rows[0].0, "beta");
    assert_eq!(rows[0].2.as_deref(), Some("seen-before"));
    assert_eq!(rows[0].3.as_deref(), Some("also in book 1"));
    assert_eq!(rows[1].0, "gamma");
    assert_eq!(rows[1].2.as_deref(), Some("not-from-source"));
    assert_eq!(rows[2].0, "alpha");
    assert_eq!(rows[2].3.as_deref(), Some("rare"));
    assert_eq!(rows[3].0, "delta");
    assert_eq!(rows[3].2, None);
    let view = app.word_by_key("beta").await;
    assert_eq!(view["icon"], "🐝");
}

// ── R-11: idempotency and preview ──────────────────────────────────────────

#[tokio::test]
async fn r11_reindented_or_reordered_json_is_the_same_pack() {
    let app = app();
    let p = simple_pack("2026-11-17", 1, vec![word("alpha"), word("beta")]);
    app.commit(&paste(std::slice::from_ref(&p))).await;
    let before = app.dump();

    // Compact, tab-indented, and with the keys of the object reversed.
    let pretty = serde_json::to_string_pretty(&p).expect("json");
    let tabs = pretty.replace("  ", "\t");
    let mut reordered = serde_json::Map::new();
    for (k, v) in p.as_object().expect("o").iter().rev() {
        reordered.insert(k.clone(), v.clone());
    }
    let reversed = serde_json::to_string_pretty(&Value::Object(reordered)).expect("json");
    for variant in [compact(&p), tabs, reversed] {
        let preview = app.preview(&variant).await;
        assert_eq!(statuses(&preview), ["already_imported"]);
        let report = app.commit(&format!("```json\n{variant}\n```")).await;
        assert_eq!(statuses(&report), ["already_imported"]);
        assert_eq!(report["totals"]["new_words"], 0);
    }
    assert_eq!(app.dump(), before, "no-op: not one row changed");
}

#[tokio::test]
async fn r11_changed_content_under_the_same_pack_id_is_imported_again_with_a_note() {
    let app = app();
    let p = simple_pack("2026-11-17", 1, vec![word("alpha")]);
    app.commit(&paste(std::slice::from_ref(&p))).await;
    let mut q = p.clone();
    q["words"] = json!([word("alpha"), word("beta")]);
    let report = app.commit(&paste(&[q])).await;
    assert_eq!(statuses(&report), ["imported"]);
    assert_eq!(report["totals"]["new_words"], 1);
    let notes = report["packs"][0]["notes"].as_array().expect("notes");
    assert!(
        notes
            .iter()
            .any(|n| n.as_str().expect("n").contains("pack_id")),
        "{notes:?}"
    );
    assert_eq!(app.int("SELECT COUNT(*) FROM pack_import"), 2);
}

#[tokio::test]
async fn r11_preview_on_a_populated_database_never_writes() {
    let app = app();
    let mut first = word("hum");
    first["hint"] = json!("a hint");
    app.commit(&paste(&[
        simple_pack("2026-11-17", 1, vec![first, word("glint")]),
        pack(
            "test-film",
            "2026-11-17",
            1,
            (1, 1),
            vec![word("film word")],
        ),
    ]))
    .await;
    let before = app.dump();

    let mut changed = word("hum");
    changed["meaning"] = json!("Another meaning.");
    changed["hint"] = json!("another hint");
    changed["czech"] = json!(["nové"]);
    changed["sentences"] = json!([sentence("A [[new]] one.", "Nová [[věta]].")]);
    let mut rejected = simple_pack("2026-11-18", 2, vec![word("x")]);
    rejected["format_version"] = json!(2);
    let mut newer = simple_pack("2026-11-25", 1, vec![changed, word("brand new")]);
    newer["reading_position"] = json!("Chapter 99");
    let text = format!(
        "{}\n\nnot json at all {{",
        paste(&[
            newer,
            rejected,
            simple_pack("2026-11-17", 1, vec![word("hum"), word("glint")]),
        ])
    );
    app.clock.set(ts("2026-11-26T09:00:00Z"));
    let one = app.preview(&text).await;
    let two = app.preview(&text).await;
    assert_eq!(one, two, "a preview is repeatable");
    assert_eq!(app.dump(), before);
    assert_eq!(one["packs"][0]["reading_position"]["updated"], true);
    assert_eq!(
        app.text("SELECT reading_position FROM source WHERE slug = 'test-novel'")
            .as_deref(),
        Some("Chapter 10")
    );
    let (_, recent) = app.get("/api/import/recent").await;
    assert_eq!(recent.as_array().expect("r").len(), 2);

    // And the commit does what the preview said.
    let commit = app.commit(&text).await;
    assert_eq!(commit["totals"]["new_words"], one["totals"]["new_words"]);
    assert_eq!(
        commit["totals"]["new_sentences"],
        one["totals"]["new_sentences"]
    );
    assert_eq!(commit["totals"]["conflicts"], one["totals"]["conflicts"]);
}

// ── R-59: reading position ─────────────────────────────────────────────────

#[tokio::test]
async fn r59_an_older_export_imported_later_adds_words_but_does_not_move_the_position_back() {
    let app = app();
    let at = |date: &str, position: &str, name: &str| {
        let mut p = simple_pack(date, 1, vec![word_with(name, vec![])]);
        p["reading_position"] = json!(position);
        p
    };
    app.commit(&paste(&[at("2026-11-20", "Chapter 50", "newer")]))
        .await;
    let (_, before) = app.get("/api/sources").await;
    assert_eq!(before[0]["reading_position"], "Chapter 50");

    let older = at("2026-11-05", "Chapter 12", "older");
    let preview = app.preview(&paste(std::slice::from_ref(&older))).await;
    assert_eq!(preview["packs"][0]["reading_position"]["updated"], false);
    let report = app.commit(&paste(&[older])).await;
    assert_eq!(report["packs"][0]["reading_position"]["updated"], false);
    assert_eq!(
        report["totals"]["new_words"], 1,
        "its words are still imported"
    );
    let (_, after) = app.get("/api/sources").await;
    assert_eq!(after[0]["reading_position"], "Chapter 50");
    assert_eq!(after[0]["word_count"], 2);
}

#[tokio::test]
async fn r59_reading_position_is_kept_per_source() {
    let app = app();
    let mut a = pack(
        "test-a",
        "2026-11-17",
        1,
        (1, 1),
        vec![word_with("one", vec![])],
    );
    a["reading_position"] = json!("Chapter 3");
    let mut b = pack(
        "test-b",
        "2026-11-16",
        1,
        (1, 1),
        vec![word_with("two", vec![])],
    );
    b["reading_position"] = json!("Chapter 8");
    app.commit(&paste(&[a, b])).await;
    let sources = app.sources().await;
    let position = |slug: &str| {
        sources
            .as_array()
            .expect("s")
            .iter()
            .find(|s| s["slug"] == slug)
            .expect("source")["reading_position"]
            .clone()
    };
    assert_eq!(position("test-a"), "Chapter 3");
    assert_eq!(position("test-b"), "Chapter 8");
}

#[tokio::test]
async fn r59_reading_position_on_a_non_book_source_is_a_warning() {
    let app = app();
    let mut p = pack("test-film", "2026-11-17", 1, (1, 1), vec![word("alpha")]);
    p["source"] = json!({ "id": "test-film", "type": "film", "title": "Test Film" });
    p["reading_position"] = json!("Minute 40");
    let preview = app.preview(&paste(&[p])).await;
    assert_eq!(statuses(&preview), ["ready"]);
    assert!(
        !preview["packs"][0]["warnings"]
            .as_array()
            .expect("w")
            .is_empty(),
        "{preview}"
    );
}

// ── R-02: all source types ─────────────────────────────────────────────────

#[tokio::test]
async fn r02_every_source_type_and_the_optional_source_fields_are_stored() {
    let app = app();
    let mut packs = Vec::new();
    for (i, kind) in ["book", "film", "series", "article", "general"]
        .into_iter()
        .enumerate()
    {
        let slug = format!("test-{kind}");
        let mut p = pack(
            &slug,
            "2026-11-17",
            1,
            (1, 1),
            vec![word_with(&format!("w{i}"), vec![])],
        );
        p["source"] = json!({ "id": slug, "type": kind, "title": format!("Test {kind}") });
        if kind == "book" {
            p["source"]["author"] = json!("Test Author");
            p["source"]["series"] = json!("Test Series");
            p["source"]["series_number"] = json!(2);
            p["source"]["url"] = json!("https://example.test/book");
        } else {
            p.as_object_mut().expect("o").remove("reading_position");
        }
        packs.push(p);
    }
    app.commit(&paste(&packs)).await;
    let sources = app.sources().await;
    assert_eq!(sources.as_array().expect("s").len(), 5);
    let book = sources
        .as_array()
        .expect("s")
        .iter()
        .find(|s| s["slug"] == "test-book")
        .expect("book");
    assert_eq!(book["type"], "book");
    assert_eq!(book["author"], "Test Author");
    assert_eq!(book["series"], "Test Series");
    assert_eq!(book["series_number"], 2);
    assert_eq!(book["url"], "https://example.test/book");
    for kind in ["film", "series", "article", "general"] {
        assert!(
            sources
                .as_array()
                .expect("s")
                .iter()
                .any(|s| s["slug"] == format!("test-{kind}") && s["type"] == kind),
            "{kind}"
        );
    }
}

// ── R-20 / R-15: stored fields ─────────────────────────────────────────────

#[tokio::test]
async fn r20_spans_at_the_edges_and_with_multibyte_text_are_split_correctly() {
    let app = app();
    let w = word_with(
        "hum",
        vec![
            json!({ "en": "[[Hum]] loudly.", "cs": "[[Bzuč]] hlasitě.", "origin": "book", "location": "Chapter 3" }),
            json!({ "en": "It went [[hum]]", "cs": "Šlo to [[bzuč]]", "origin": "source", "location": "p. 40" }),
            json!({ "en": "[[hum]]", "cs": "[[žluťoučký kůň]]", "origin": "example" }),
            json!({ "en": "Ve městě „[[žlutý]]“ 🐴 [1] arr[0].", "cs": "Ve [[městě]] ž[1].", "origin": "example" }),
        ],
    );
    app.commit(&paste(&[simple_pack("2026-11-17", 1, vec![w])]))
        .await;
    let view = app.word_by_key("hum").await;
    let s = &view["sentences"];
    assert_eq!(
        s[0]["en"],
        json!({ "pre": "", "target": "Hum", "post": " loudly." })
    );
    assert_eq!(s[0]["origin"], "book");
    assert_eq!(s[0]["location"], "Chapter 3");
    assert_eq!(
        s[1]["en"],
        json!({ "pre": "It went ", "target": "hum", "post": "" })
    );
    assert_eq!(s[1]["origin"], "source");
    assert_eq!(
        s[2]["en"],
        json!({ "pre": "", "target": "hum", "post": "" })
    );
    assert_eq!(s[2]["cs"]["target"], "žluťoučký kůň");
    assert_eq!(s[2]["location"], Value::Null);
    assert_eq!(s[3]["en"]["pre"], "Ve městě „");
    assert_eq!(s[3]["en"]["post"], "“ 🐴 [1] arr[0].");
}

#[tokio::test]
async fn r15_pack_decisions_are_stored_and_a_missing_goal_stays_derivable() {
    let app = app();
    let mut full = word("alpha");
    full["goal"] = json!("recognise");
    full["accepted"] = json!(["alfa"]);
    full["hint"] = json!("first letter");
    full["spelling"] = json!(true);
    full["spelling_note"] = json!("ph");
    full["confusables"] = json!(["beta"]);
    let bare = word("bare");
    app.commit(&paste(&[simple_pack("2026-11-17", 1, vec![full, bare])]))
        .await;
    let v = app.word_by_key("alpha").await;
    assert_eq!(v["goal"], "recognise");
    assert_eq!(v["accepted"], json!(["alfa"]));
    assert_eq!(v["hint"], "first letter");
    assert_eq!(v["spelling"], true);
    assert_eq!(v["spelling_note"], "ph");
    assert_eq!(v["confusables"], json!(["beta"]));
    let v = app.word_by_key("bare").await;
    assert_eq!(
        v["goal"],
        Value::Null,
        "R-15 default is derived, not stored"
    );
    assert_eq!(v["spelling"], false);
}

// ── R-84: untrusted text ───────────────────────────────────────────────────

#[tokio::test]
async fn r84_markup_in_every_text_field_is_stored_and_returned_verbatim() {
    let app = app();
    let evil = "<script>alert(1)</script><img src=x onerror=alert(2)> & \"q\" 'a' **b** [l](javascript:x) {{t}} ${j}";
    let mut w = word_with(
        "hum",
        vec![json!({
            "en": format!("<b>pre</b> [[{evil}]] <i>post</i>"),
            "cs": format!("<u>cs</u> [[{evil}]]"),
            "origin": "example",
            "location": evil,
        })],
    );
    w["grammar"] = json!(evil);
    w["pronunciation"] = json!(evil);
    w["speak"] = json!(evil);
    w["meaning"] = json!(evil);
    w["czech"] = json!([evil]);
    w["accepted"] = json!([evil]);
    w["confusables"] = json!([evil]);
    w["hint"] = json!(evil);
    w["spelling"] = json!(true);
    w["spelling_note"] = json!(evil);
    w["note"] = json!(evil);
    w["icon"] = json!(evil[..40].to_owned());
    w["forms"] = json!([{ "form": evil, "common": true }]);
    w["tag"] = json!({ "kind": "seen-before", "text": evil });
    let mut p = simple_pack("2026-11-17", 1, vec![w]);
    p["source"]["title"] = json!(evil);
    p["source"]["author"] = json!(evil);
    p["source"]["series"] = json!(evil);
    p["reading_position"] = json!(evil);
    let report = app.commit(&paste(&[p])).await;
    assert_eq!(statuses(&report), ["imported"], "{report}");
    assert_eq!(report["totals"]["invalid_words"], 0, "{report}");

    let view = app.word_by_key("hum").await;
    assert_eq!(view["grammar"], evil);
    assert_eq!(view["pronunciation"], evil);
    assert_eq!(view["speak"], evil);
    assert_eq!(view["meaning"], evil);
    assert_eq!(view["czech"], json!([evil]));
    assert_eq!(view["accepted"], json!([evil]));
    assert_eq!(view["confusables"], json!([evil]));
    assert_eq!(view["hint"], evil);
    assert_eq!(view["spelling_note"], evil);
    assert_eq!(view["note"], evil);
    assert_eq!(view["icon"], evil[..40]);
    assert_eq!(view["forms"][0]["form"], evil);
    assert_eq!(view["sources"][0]["title"], evil);
    assert_eq!(view["sources"][0]["tag_text"], evil);
    let s = &view["sentences"][0];
    assert_eq!(s["en"]["pre"], "<b>pre</b> ");
    assert_eq!(s["en"]["target"], evil);
    assert_eq!(s["en"]["post"], " <i>post</i>");
    assert_eq!(s["cs"]["pre"], "<u>cs</u> ");
    assert_eq!(s["location"], evil);
    let sources = app.sources().await;
    assert_eq!(sources[0]["title"], evil);
    assert_eq!(sources[0]["author"], evil);
    assert_eq!(sources[0]["series"], evil);
    assert_eq!(sources[0]["reading_position"], evil);
}

#[tokio::test]
async fn r84_json_responses_are_served_as_json_not_html() {
    let app = app();
    let mut w = word("hum");
    w["note"] = json!("<script>alert(1)</script>");
    app.commit(&paste(&[simple_pack("2026-11-17", 1, vec![w])]))
        .await;
    let id = app.word_id_by_key("hum");
    let response = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/words/{id}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type")
        .to_str()
        .expect("str")
        .to_owned();
    assert!(
        content_type.starts_with("application/json"),
        "{content_type}"
    );
    let nosniff = response
        .headers()
        .get("x-content-type-options")
        .map(|v| v.to_str().expect("str").to_owned());
    assert_eq!(nosniff.as_deref(), Some("nosniff"));
}

// ── R-24: no sentence text in reports or logs ──────────────────────────────

#[tokio::test]
async fn r24_reports_of_invalid_sentences_do_not_quote_them_and_nothing_stored_holds_text() {
    let app = app();
    let secret = "zebrafinch-quartz-lantern";
    let w = word_with(
        "hum",
        vec![
            sentence(
                &format!("Two [[{secret}]] and [[more]] spans."),
                "Dvě [[věty]].",
            ),
            sentence(
                &format!("Fine [[hum]] {secret} sentence."),
                "Pěkná [[věta]] bez textu.",
            ),
            sentence(&format!("No span for {secret}."), "Bez značky."),
        ],
    );
    let p = simple_pack("2026-11-17", 1, vec![w]);
    let preview = app.preview(&paste(std::slice::from_ref(&p))).await;
    assert!(
        !preview.to_string().contains(secret),
        "preview report quotes sentence text: {preview}"
    );
    let report = app.commit(&paste(&[p])).await;
    assert!(
        !report.to_string().contains(secret),
        "commit report quotes sentence text"
    );
    let (_, recent) = app.get("/api/import/recent").await;
    assert!(!recent.to_string().contains(secret));
    let stored: String = app.int("SELECT COUNT(*) FROM pack_import").to_string();
    assert_eq!(stored, "1");
    let report_json = app
        .text("SELECT report_json FROM pack_import")
        .unwrap_or_default();
    assert!(!report_json.contains(secret), "{report_json}");
    // The sentence itself is, of course, in the owner's catalog.
    assert_eq!(app.int("SELECT COUNT(*) FROM sentence"), 1);
}

fn prague() -> vocab_server::clock::DayRule {
    vocab_server::clock::DayRule::new("Europe/Prague", 4).expect("bundled tzdb")
}
