//! The feedback export (R-57, D-12) and the full export (R-86, D-16)
//! through the HTTP API (design §8.1): their documented shape, nothing
//! beyond it, the download headers, the round trip of the full export into
//! a fresh database, and its streaming limits with a large synthetic
//! history (5,000 words, 100,000 reviews), which also times Progress.

mod common;

use std::collections::BTreeMap;
use std::time::Instant;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{Env, env, word};
use rusqlite::Connection;
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use tower::ServiceExt;
use vocab_server::config::Limits;

const BOOK: &str = "test-export-book";

/// GET `uri`: status, headers and the whole body.
async fn download(env: &Env, uri: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let response = env
        .router
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 256 << 20)
        .await
        .expect("body");
    (status, headers, bytes.to_vec())
}

fn assert_download_headers(headers: &axum::http::HeaderMap, prefix: &str) {
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "application/json; charset=utf-8"
    );
    let disposition = headers[header::CONTENT_DISPOSITION]
        .to_str()
        .expect("ascii");
    assert!(
        disposition.starts_with(&format!("attachment; filename=\"{prefix}-2026-11-20")),
        "{disposition}"
    );
}

fn flag(env: &Env, headword: &str) {
    env.sql()
        .execute(
            "UPDATE card SET state = 'review', stability = 2.0, difficulty = 6.0, \
             due_day = '2026-11-21', last_review_day = '2026-11-19', reps = 8, lapses = 4, \
             leech = 'flagged' WHERE word_id = (SELECT id FROM word WHERE headword = ?1)",
            [headword],
        )
        .expect("flag");
}

/// Every key anywhere in `value`.
fn keys(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                keys(v, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| keys(v, out)),
        _ => {}
    }
}

#[tokio::test]
async fn r57_feedback_lists_what_the_next_pack_should_fix_and_nothing_else() {
    let env = env();
    let mut hinted = word("papa", 7, 3);
    hinted["hint"] = json!("A test memory hook.");
    hinted["confusables"] = json!(["pupa"]);
    env.import(
        BOOK,
        vec![
            word("oscar", 6, 3),
            hinted,
            word("quebec", 5, 3),
            word("romeo", 8, 4),
        ],
    )
    .await;
    flag(&env, "oscar");
    flag(&env, "papa");
    let quebec = env.word_id("quebec").await.get();
    let romeo = env.word_id("romeo").await.get();
    // Owner changes: usefulness and goal, a hidden and an edited sentence,
    // an accepted answer; and a later pack that disagrees.
    let (status, view) = env
        .request(
            "PATCH",
            &format!("/api/words/{quebec}"),
            Some(&json!({ "usefulness": 9, "goal": "active" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let (_, view) = env.get(&format!("/api/words/{romeo}")).await;
    let hidden = view["sentences"][0]["id"].as_i64().expect("id");
    let edited = view["sentences"][1]["id"].as_i64().expect("id");
    let (status, _) = env
        .post(
            &format!("/api/sentences/{hidden}/hidden"),
            &json!({ "hidden": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = env
        .request(
            "PATCH",
            &format!("/api/sentences/{edited}"),
            Some(&json!({ "en": "A corrected test line with [[romeo]].", "cs": "Opravený řádek s [[romeo]]." })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = env
        .post(
            &format!("/api/words/{romeo}/items"),
            &json!({ "kind": "accepted", "value": "romeos" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut changed = word("romeo", 8, 4);
    changed["meaning"] = json!("A different invented meaning.");
    env.import(BOOK, vec![changed]).await;

    let (status, headers, bytes) = download(&env, "/api/export/feedback").await;
    assert_eq!(status, StatusCode::OK);
    assert_download_headers(&headers, "vocab-feedback");
    let f: Value = serde_json::from_slice(&bytes).expect("json");

    let top: Vec<&str> = f
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        top,
        [
            "edited_sentences",
            "format",
            "format_version",
            "generated_on",
            "hard_words",
            "hidden_sentences",
            "instructions_for_claude",
            "needs_hint",
            "open_conflicts",
            "overrides",
            "owner_accepted",
        ]
    );
    assert_eq!(f["format"], "vocab-feedback");
    assert_eq!(f["format_version"], 1);
    assert_eq!(f["generated_on"], "2026-11-20");
    assert!(
        f["instructions_for_claude"]
            .as_str()
            .expect("text")
            .contains("needs_hint")
    );
    assert_eq!(
        f["needs_hint"],
        json!([{ "word": "oscar", "sources": [BOOK], "czech": ["oscar-cz"] }])
    );
    assert_eq!(
        f["hard_words"],
        json!([{ "word": "papa", "sources": [BOOK], "czech": ["papa-cz"],
                 "hint": "A test memory hook.", "confusables": ["pupa"] }])
    );
    assert_eq!(
        f["hidden_sentences"],
        json!([{ "word": "romeo", "sources": [BOOK],
                 "en": "Test line 1 with the [[romeo]] in it.",
                 "cs": "Testovací řádek 1 s [[romeo]] v něm." }])
    );
    assert_eq!(
        f["edited_sentences"],
        json!([{ "word": "romeo", "sources": [BOOK],
                 "en": "A corrected test line with [[romeo]].", "cs": "Opravený řádek s [[romeo]]." }])
    );
    assert_eq!(
        f["owner_accepted"],
        json!([{ "word": "romeo", "sources": [BOOK], "answers": ["romeos"] }])
    );
    assert_eq!(
        f["overrides"],
        json!([
            { "word": "quebec", "sources": [BOOK], "field": "goal", "value": "active" },
            { "word": "quebec", "sources": [BOOK], "field": "usefulness", "value": 9 },
        ])
    );
    assert_eq!(
        f["open_conflicts"],
        json!([{ "word": "romeo", "sources": [BOOK], "field": "meaning",
                 "current": "Invented test meaning of romeo.",
                 "incoming": "A different invented meaning." }])
    );
    // Vocabulary only: no ids, no learning history, nothing of the account.
    let mut all = Vec::new();
    keys(&f, &mut all);
    for forbidden in [
        "id",
        "word_id",
        "card_id",
        "stability",
        "lapses",
        "reviewed_at",
        "review_day",
        "password",
        "token",
        "settings",
    ] {
        assert!(
            !all.iter().any(|k| k == forbidden),
            "{forbidden} in the feedback"
        );
    }
}

#[tokio::test]
async fn r57_r86_exports_need_a_session_and_refuse_cross_site_requests() {
    let env = env();
    for uri in ["/api/export/feedback", "/api/export/full", "/api/progress"] {
        let request = Request::get(uri)
            .header(header::COOKIE, "test-other=1")
            .body(Body::empty())
            .expect("request");
        let (status, _) = env.send(request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        let request = Request::get(uri)
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .expect("request");
        let (status, _) = env.send(request).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
}

// ── the full export ─────────────────────────────────────────────────────────

/// Exported table → database table.
const TABLES: [(&str, &str); 13] = [
    ("sources", "source"),
    ("words", "word"),
    ("word_sources", "word_source"),
    ("word_items", "word_item"),
    ("word_forms", "word_form"),
    ("sentences", "sentence"),
    ("word_overrides", "word_override"),
    ("field_conflicts", "field_conflict"),
    ("word_pack_spellings", "word_pack_spelling"),
    ("pack_imports", "pack_import"),
    ("cards", "card"),
    ("reviews", "review_log"),
    ("fsrs_params", "fsrs_params"),
];

/// Columns the export leaves out on purpose (design §8.1), and the value a
/// restore gives them.
const LEFT_OUT: [(&str, &str, &str); 2] = [
    ("review_log", "before_json", "'{}'"),
    ("pack_import", "report_json", "'{}'"),
];

/// Rows of `table`, only the `columns`, in a stable order.
fn table_rows(conn: &Connection, table: &str, columns: &[String]) -> Vec<Vec<SqlValue>> {
    let list = columns.join(", ");
    let mut stmt = conn
        .prepare(&format!("SELECT {list} FROM {table}"))
        .expect("select");
    let mut rows: Vec<Vec<SqlValue>> = stmt
        .query_map([], |row| {
            (0..columns.len())
                .map(|i| row.get::<_, SqlValue>(i))
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    // Tables keyed by their columns keep no insertion order.
    rows.sort_by_key(|r| format!("{r:?}"));
    rows
}

fn columns_of(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .expect("columns");
    stmt.query_map([], |row| row.get(0))
        .expect("columns")
        .map(|c| c.expect("column"))
        .collect()
}

fn sql_value(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => n.as_i64().map_or_else(
            || SqlValue::Real(n.as_f64().expect("number")),
            SqlValue::Integer,
        ),
        Value::String(s) => SqlValue::Text(s.clone()),
        other => panic!("unexpected cell {other}"),
    }
}

/// Restore `export` into the fresh database of `env` and check that every
/// exported table holds exactly what `source` holds.
fn assert_round_trip(source: &Env, export: &Value, target: &Env) {
    let to = target.sql();
    to.execute_batch("PRAGMA foreign_keys = OFF; DELETE FROM id_floor;")
        .expect("prepare");
    for (name, table) in TABLES {
        let exported: Vec<String> = export[name]["columns"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} columns"))
            .iter()
            .map(|c| c.as_str().expect("column name").to_owned())
            .collect();
        let rows = export[name]["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} rows"));
        for row in rows {
            let row = row.as_array().expect("row array");
            assert_eq!(row.len(), exported.len(), "{name}: one value per column");
            let mut columns = exported.clone();
            let mut values: Vec<String> = (1..=columns.len()).map(|i| format!("?{i}")).collect();
            for (t, column, default) in LEFT_OUT {
                if t == table {
                    columns.push(column.to_owned());
                    values.push(default.to_owned());
                }
            }
            let params: Vec<SqlValue> = row.iter().map(sql_value).collect();
            to.execute(
                &format!(
                    "INSERT INTO {table} ({}) VALUES ({})",
                    columns.join(", "),
                    values.join(", ")
                ),
                rusqlite::params_from_iter(params),
            )
            .unwrap_or_else(|e| panic!("restore {table}: {e}"));
        }
    }
    to.execute_batch("PRAGMA foreign_keys = ON;")
        .expect("fk on");
    let violations: i64 = to
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .expect("fk check");
    assert_eq!(violations, 0, "the restored rows reference each other");
    let from = source.sql();
    for (_, table) in TABLES {
        let columns: Vec<String> = columns_of(&from, table)
            .into_iter()
            .filter(|c| !LEFT_OUT.iter().any(|(t, col, _)| *t == table && col == c))
            .collect();
        assert_eq!(
            table_rows(&from, table, &columns),
            table_rows(&to, table, &columns),
            "{table} after the round trip"
        );
    }
}

#[tokio::test]
async fn r86_full_export_round_trip_into_a_fresh_database() {
    let env = env();
    let mut sierra = word("sierra", 7, 3);
    sierra["forms"] = json!([{ "form": "sierras", "common": true }]);
    sierra["confusables"] = json!(["tierra"]);
    env.import(
        BOOK,
        vec![sierra, word("tango", 5, 2), word("uniform", 8, 4)],
    )
    .await;
    // Real practice: a block, two ratings (one undone), an owner edit.
    let (status, start) = env.post("/api/sessions", &json!({ "kind": "start" })).await;
    assert_eq!(status, StatusCode::OK, "{start}");
    let session = start["session"]["id"].as_str().expect("id").to_owned();
    for (n, grade) in [(0, 2), (1, 4)] {
        let item = start["session"]["items"][n]["item_id"].clone();
        let (status, body) = env
            .post(
                &format!("/api/sessions/{session}/reviews"),
                &json!({ "client_review_id": format!("00000000-0000-4000-8000-00000000000{n}"),
                         "item_id": item, "raw_grade": grade, "hint_used": false,
                         "duration_ms": 900, "client_ts": "2026-11-20T09:00:00Z" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, _) = env
        .post(&format!("/api/sessions/{session}/undo"), &json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let tango = env.word_id("tango").await.get();
    env.request(
        "PATCH",
        &format!("/api/words/{tango}"),
        Some(&json!({ "note": "Test note." })),
    )
    .await;
    env.request("PATCH", "/api/settings", Some(&json!({ "block_size": 15 })))
        .await;

    let (status, headers, bytes) = download(&env, "/api/export/full").await;
    assert_eq!(status, StatusCode::OK);
    assert_download_headers(&headers, "vocab-export");
    let text = String::from_utf8(bytes).expect("utf-8");
    let export: Value = serde_json::from_str(&text).expect("complete JSON");

    let top: Vec<&str> = export
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    let mut expected = vec![
        "app_version",
        "exported_at",
        "format",
        "format_version",
        "schema_version",
        "settings",
        "study_day",
    ];
    expected.extend(TABLES.iter().map(|(name, _)| *name));
    expected.sort_unstable();
    assert_eq!(top, expected, "documented members only");
    assert_eq!(export["format"], "vocab-full-export");
    assert_eq!(export["format_version"], 1);
    assert_eq!(export["app_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        export["schema_version"],
        vocab_server::db::Db::schema_version()
    );
    assert_eq!(export["study_day"], "2026-11-20");
    assert_eq!(export["settings"]["block_size"], 15);
    let reviews = &export["reviews"];
    assert_eq!(reviews["rows"].as_array().map(Vec::len), Some(2));
    let column = |name: &str| {
        reviews["columns"]
            .as_array()
            .expect("columns")
            .iter()
            .position(|c| c == name)
    };
    let undone = column("undone").expect("undone column");
    assert_eq!(
        reviews["rows"][1][undone], 1,
        "undone ratings are kept, marked"
    );
    assert_eq!(column("before_json"), None, "the undo snapshot is left out");
    // Nothing of the account, the audio cache or open practice blocks.
    for secret in [
        "password",
        "token",
        "auth_",
        "setup",
        "audio",
        "session_item",
    ] {
        assert!(
            !text.contains(&format!("\"{secret}")),
            "{secret} in the export"
        );
    }

    let target = env_fresh();
    assert_round_trip(&env, &export, &target);
}

fn env_fresh() -> Env {
    common::env()
}

// ── large data: Progress timing and the streaming limits ───────────────────

/// 5,000 words in 5 sources (15,000 sentences), Understand cards for all
/// and Produce cards for a third, 100,000 reviews over 400 days.
fn large_history(env: &Env) {
    env.exec(
        "INSERT INTO pack_import (id, pack_id, content_sha256, format_version, source_slug, \
           part_index, part_total, via, imported_at, report_json) \
           VALUES (1, 'test-big--2026-01-01--1', 'test', 1, 'test-big-1', 1, 1, 'paste', \
           '2026-01-01T00:00:00Z', '{}');
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5)
           INSERT INTO source (id, slug, type, title, reading_position, created_at, updated_at)
           SELECT i, 'test-big-' || i, 'book', 'Test book ' || i, 'Chapter ' || i,
             '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z' FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000)
           INSERT INTO word (id, headword, headword_key, meaning, usefulness, created_at, updated_at)
           SELECT i, 'test-w' || i, 'test-w' || i, 'Invented meaning ' || i, 1 + i % 10,
             '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z' FROM n;
         INSERT INTO word_source (word_id, source_id, seq, first_pack_import_id, last_pack_import_id)
           SELECT id, 1 + id % 5, id, 1, 1 FROM word;
         INSERT INTO word_item (word_id, kind, value, value_key, position, origin)
           SELECT id, 'czech', 'cz-' || id, 'cz-' || id, 0, 'pack' FROM word;
         WITH RECURSIVE k(j) AS (SELECT 1 UNION ALL SELECT j + 1 FROM k WHERE j < 3)
           INSERT INTO sentence (word_id, en_marked, cs_marked, en_key, origin, position,
             created_at, updated_at)
           SELECT w.id, 'Test line ' || j || ' with [[' || w.headword || ']].',
             'Řádek ' || j || ' s [[' || w.headword || ']].', 'test line ' || j, 'example', j,
             '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z' FROM word w, k;
         INSERT INTO card (word_id, kind, format, state, stability, difficulty, due_day,
             last_review_day, introduced_day, reps, lapses, leech, created_at)
           SELECT id, 'understand', 'sentence', 'review', 0.5 + id % 40, 5.0,
             date('2026-11-20', '+' || (id % 30) || ' days'), '2026-11-19',
             date('2026-11-20', '-' || (id % 300) || ' days'), 10, id % 6,
             CASE WHEN id % 97 = 0 THEN 'flagged' ELSE 'none' END, '2026-01-01T00:00:00Z'
           FROM word;
         INSERT INTO card (word_id, kind, format, state, stability, difficulty, due_day,
             last_review_day, introduced_day, reps, created_at)
           SELECT id, 'produce', 'cloze', 'review', 1.0 + id % 20, 5.0,
             date('2026-11-21', '+' || (id % 20) || ' days'), '2026-11-19', '2026-06-01', 4,
             '2026-06-01T00:00:00Z' FROM word WHERE id % 3 = 0;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000)
           INSERT INTO review_log (client_review_id, card_id, reviewed_at, review_day, raw_grade,
             fsrs_rating, format, elapsed_days, before_json)
           SELECT 'test-big-' || i, 1 + i % 6666, '2026-01-01T00:00:00Z',
             date('2026-11-20', '-' || (i % 400) || ' days'), 1 + i % 5, 3, 'sentence', i % 7,
             '{\"state\":\"review\"}' FROM n;",
    );
}

#[tokio::test]
async fn r72_r86_large_history_progress_is_fast_and_the_export_streams_one_at_a_time() {
    let env = env();
    large_history(&env);
    assert_eq!(env.int("SELECT COUNT(*) FROM review_log"), 100_000);

    // Progress (and Today) on 5,000 words and 100,000 reviews.
    let started = Instant::now();
    let (status, p) = env.get("/api/progress").await;
    let progress_ms = started.elapsed().as_millis();
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(p["totals"]["words"], 5000);
    assert_eq!(p["leeches"].as_array().map(Vec::len), Some(51));
    let started = Instant::now();
    let (status, _) = env.get("/api/today").await;
    let today_ms = started.elapsed().as_millis();
    assert_eq!(status, StatusCode::OK);
    eprintln!(
        "timing: progress {progress_ms} ms, today {today_ms} ms (5000 words, 100000 reviews)"
    );
    assert!(progress_ms < 3000, "progress took {progress_ms} ms");

    // The full export streams: while its body is not read, the export is
    // still running (its bounded queue is full) and a second one is refused.
    let first = env
        .router
        .clone()
        .oneshot(
            Request::get("/api/export/full")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(first.status(), StatusCode::OK);
    let (status, body) = env.get("/api/export/full").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "busy");
    let started = Instant::now();
    let bytes = to_bytes(first.into_body(), 512 << 20).await.expect("body");
    let export_ms = started.elapsed().as_millis();
    eprintln!(
        "timing: full export {} bytes in {export_ms} ms",
        bytes.len()
    );
    let export: Value = serde_json::from_slice(&bytes).expect("complete JSON");
    assert_eq!(
        export["reviews"]["rows"].as_array().map(Vec::len),
        Some(100_000)
    );
    assert_eq!(export["words"]["rows"].as_array().map(Vec::len), Some(5000));
    // Done: the next export may run.
    let mut counts = BTreeMap::new();
    for _ in 0..50 {
        let (status, _, bytes) = download(&env, "/api/export/full").await;
        if status == StatusCode::OK {
            counts.insert("ok", bytes.len());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(counts.contains_key("ok"), "a later export runs");
}

/// `PRAGMA wal_checkpoint(PASSIVE)` on a connection of its own: (busy,
/// frames in the WAL, frames checkpointed).
fn checkpoint(env: &Env) -> (i64, i64, i64) {
    env.sql()
        .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .expect("checkpoint")
}

#[tokio::test]
async fn r86_a_stalled_download_is_broken_off_and_never_holds_the_snapshot() {
    let idle_ms = 300;
    let env = common::env_with_limits(&Limits {
        export_idle_ms: idle_ms,
        ..Limits::default()
    });
    large_history(&env);

    // A client that never reads its download.
    let stalled = env
        .router
        .clone()
        .oneshot(
            Request::get("/api/export/full")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(stalled.status(), StatusCode::OK);
    let length: usize = stalled.headers()[header::CONTENT_LENGTH]
        .to_str()
        .ok()
        .and_then(|v| v.parse().ok())
        .expect("Content-Length");
    let (status, body) = env.get("/api/export/full").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

    // The database snapshot is already given back while the download
    // stalls: what is written now can be checkpointed completely.
    env.exec("UPDATE card SET reps = reps + 1");
    let (busy, frames, done) = checkpoint(&env);
    assert_eq!(busy, 0);
    assert!(frames > 0);
    assert_eq!(frames, done, "every WAL frame checkpointed");

    // The stalled download is broken off after the idle timeout and the
    // next export runs.
    let started = Instant::now();
    let next = loop {
        let (status, _, bytes) = download(&env, "/api/export/full").await;
        if status == StatusCode::OK {
            break bytes;
        }
        assert!(
            started.elapsed().as_millis() < u128::from(idle_ms) + 2000,
            "still busy after {} ms",
            started.elapsed().as_millis()
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };
    let export: Value = serde_json::from_slice(&next).expect("complete JSON");
    assert_eq!(export["words"]["rows"].as_array().map(Vec::len), Some(5000));

    // The stalled client gets a broken body, never a complete-looking file.
    let broken = to_bytes(stalled.into_body(), 512 << 20).await;
    assert!(broken.is_err(), "the stalled download must not end cleanly");
    assert!(length > 0);
}

#[tokio::test]
async fn r86_a_slow_but_steady_download_is_never_cut_off() {
    use http_body_util::BodyExt;
    let idle_ms = 200;
    let env = common::env_with_limits(&Limits {
        export_idle_ms: idle_ms,
        ..Limits::default()
    });
    large_history(&env);
    let response = env
        .router
        .clone()
        .oneshot(
            Request::get("/api/export/full")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    let started = Instant::now();
    let mut frames = 0;
    while let Some(frame) = body.frame().await {
        let frame = frame.expect("no error while the client keeps reading");
        if let Ok(data) = frame.into_data() {
            bytes.extend_from_slice(&data);
        }
        frames += 1;
        // Several idle timeouts in all, each step well within one.
        if frames <= 25 {
            tokio::time::sleep(std::time::Duration::from_millis(idle_ms / 3)).await;
        }
    }
    assert!(started.elapsed().as_millis() > u128::from(idle_ms) * 4);
    let export: Value = serde_json::from_slice(&bytes).expect("complete JSON");
    assert_eq!(
        export["reviews"]["rows"].as_array().map(Vec::len),
        Some(100_000)
    );
}
