//! Upgrade from v0.1.0 (R-85): a database exactly as v0.1.0 leaves it —
//! migrations 0001–0004 and data the app wrote: imported words, reviews, a
//! practice block in progress, the owner's password and two logged-in
//! devices, plus sentences that the wider sentence key of 0005 merges — is
//! opened by the current app. Nothing is lost, the foreign keys and
//! `quick_check` hold, the old sessions keep working and then behave as
//! specified (365-day absolute lifetime, password change), login works.
//!
//! The data is written through the current app's HTTP API into a scratch
//! database and copied, column by column, into a fresh v0.1.0 schema: the
//! columns v0.1.0 has are exactly the ones the copy takes.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{PASSWORD, auth, cookie_pair_of, login, post_json, prague, ts, word};
use rusqlite::Connection;
use serde_json::{Value, json};
use tower::ServiceExt;
use vocab_server::auth::SetupCode;
use vocab_server::clock::{Clock, ManualClock};
use vocab_server::config::Limits;
use vocab_server::db::Db;
use vocab_server::http::{self, AppState};

const DB_FILE: &str = "vocabulary.sqlite";

/// The schema of v0.1.0.
const V0_1_0_MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_word_spelling_nullable.sql"),
    include_str!("../migrations/0003_session_item_format.sql"),
    include_str!("../migrations/0004_auth_owner.sql"),
];

struct App {
    clock: Arc<ManualClock>,
    router: axum::Router,
    code: SetupCode,
}

/// The app on the database in `dir`, with the design's session lifetimes.
fn app_in(dir: &Path, now: &str) -> App {
    let db = Db::open(dir).expect("db");
    let clock = Arc::new(ManualClock::new(ts(now)));
    let (auth, code) = auth();
    let state = AppState::with_clock(db, Arc::clone(&clock) as Arc<dyn Clock>, prague(), auth);
    App {
        clock,
        router: http::app(state, &Limits::default()),
        code,
    }
}

impl App {
    async fn send(&self, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let cookie = cookie_pair_of(&response);
        let bytes = to_bytes(response.into_body(), 16 << 20)
            .await
            .expect("body");
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value, cookie)
    }

    async fn get(&self, uri: &str, cookie: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .uri(uri)
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .expect("request");
        let (status, value, _) = self.send(request).await;
        (status, value)
    }

    async fn post(&self, uri: &str, body: &Value, cookie: &str) -> (StatusCode, Value) {
        let (status, value, _) = self.send(post_json(uri, body, Some(cookie))).await;
        (status, value)
    }

    /// Rate the next unanswered item of `session` with `raw`.
    async fn rate_next(&self, session: &Value, raw: u8, n: u32, cookie: &str) -> Value {
        let item = session["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|i| i["answered"].is_null())
            .expect("an unanswered item");
        let (status, result) = self
            .post(
                &format!(
                    "/api/sessions/{}/reviews",
                    session["id"].as_str().expect("id")
                ),
                &json!({
                    "client_review_id": format!("00000000-0000-4000-8000-{n:012}"),
                    "item_id": item["item_id"],
                    "raw_grade": raw,
                    "hint_used": false,
                    "duration_ms": 2000,
                }),
                cookie,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        result["session"].clone()
    }
}

/// Two days of use: setup, a second device, an import, a finished block on
/// 2026-11-10 and a block left half done on 2026-11-11. Returns the cookies
/// of the two devices.
async fn use_the_app(dir: &Path) -> (String, String) {
    let app = app_in(dir, "2026-11-10T09:00:00Z");
    let phone = login(&app.router, &app.code).await;
    let laptop = login(&app.router, &app.code).await;
    let mut words: Vec<Value> = (1..=9)
        .map(|i| word(&format!("test-up{i}"), 5, 3))
        .collect();
    words.push(word("test-uponly", 5, 2));
    let pack = json!({
        "format": "vocab-pack", "format_version": 1, "pack_id": "test-upgrade--2026-11-09--1",
        "exported_at": "2026-11-09", "reading_position": "Chapter 2",
        "source": { "id": "test-upgrade", "type": "book", "title": "Test Upgrade Book" },
        "words": words,
    });
    let (status, report) = app
        .post(
            "/api/import/commit",
            &json!({ "text": format!("```json\n{pack}\n```") }),
            &phone,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let mut n = 0;
    let (_, started) = app
        .post("/api/sessions", &json!({ "kind": "start" }), &phone)
        .await;
    let mut session = started["session"].clone();
    for raw in [2, 3, 5, 1, 2] {
        n += 1;
        session = app.rate_next(&session, raw, n, &phone).await;
    }
    while session["remaining"].as_u64().expect("remaining") > 0 {
        n += 1;
        session = app.rate_next(&session, 3, n, &phone).await;
    }
    let (status, _) = app
        .post(
            &format!(
                "/api/sessions/{}/finish",
                session["id"].as_str().expect("id")
            ),
            &json!({}),
            &phone,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // Next day: a block in progress (one item rated).
    app.clock.set(ts("2026-11-11T08:00:00Z"));
    let (_, started) = app
        .post("/api/sessions", &json!({ "kind": "start" }), &laptop)
        .await;
    let session = started["session"].clone();
    assert!(session["total"].as_u64().expect("total") >= 3, "{session}");
    app.rate_next(&session, 2, 100, &laptop).await;
    (phone, laptop)
}

/// A v0.1.0 database in `dir` holding the rows of the scratch database
/// `scratch` (the columns v0.1.0 has).
fn v0_1_0_database(dir: &Path, scratch: &Path) {
    let conn = Connection::open(dir.join(DB_FILE)).expect("create");
    for sql in V0_1_0_MIGRATIONS {
        conn.execute_batch(sql).expect("v0.1.0 schema");
    }
    conn.pragma_update(None, "user_version", 4)
        .expect("version");
    conn.execute(
        "ATTACH DATABASE ?1 AS used",
        [scratch.join(DB_FILE).to_str().expect("utf-8 path")],
    )
    .expect("attach");
    // Tables are copied in name order; the upgrade checks the keys.
    conn.pragma_update(None, "foreign_keys", "OFF").expect("fk");
    conn.execute_batch("DELETE FROM main.auth_owner")
        .expect("empty");
    for (table, columns) in v0_1_0_columns(&conn) {
        let list = columns.join(", ");
        conn.execute_batch(&format!(
            "INSERT INTO main.{table} ({list}) SELECT {list} FROM used.{table} ORDER BY rowid"
        ))
        .unwrap_or_else(|e| panic!("copy {table}: {e}"));
    }
    conn.execute_batch("DETACH DATABASE used").expect("detach");
}

/// Every table of the database at `conn` with its columns, as v0.1.0 has
/// them (call on a v0.1.0 database).
fn v0_1_0_columns(conn: &Connection) -> Vec<(String, Vec<String>)> {
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM main.sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .expect("tables")
        .query_map([], |r| r.get(0))
        .expect("tables")
        .map(|r| r.expect("table"))
        .collect();
    tables
        .into_iter()
        .map(|table| {
            let columns = conn
                .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .expect("columns")
                .query_map([], |r| r.get(0))
                .expect("columns")
                .map(|r| r.expect("column"))
                .collect();
            (table, columns)
        })
        .collect()
}

/// All rows of the v0.1.0 `columns` of every table, debug-formatted.
fn snapshot(path: &Path, tables: &[(String, Vec<String>)]) -> BTreeMap<String, Vec<String>> {
    let conn = Connection::open(path).expect("open");
    tables
        .iter()
        .map(|(table, columns)| {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {} FROM {table} ORDER BY rowid",
                    columns.join(", ")
                ))
                .expect("select");
            let rows = stmt
                .query_map([], |row| {
                    (0..columns.len())
                        .map(|i| {
                            row.get::<_, rusqlite::types::Value>(i)
                                .map(|v| format!("{v:?}"))
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(|cols| cols.join(" | "))
                })
                .expect("rows")
                .map(|r| r.expect("row"))
                .collect();
            (table.clone(), rows)
        })
        .collect()
}

/// A login with the password set before the upgrade; returns the cookie.
async fn log_in(app: &App) -> String {
    let (status, body, cookie) = app
        .send(post_json(
            "/api/auth/login",
            &json!({ "password": PASSWORD }),
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    cookie.expect("cookie")
}

fn int(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).expect(sql)
}

#[tokio::test]
async fn r85_a_v0_1_0_database_upgrades_without_loss_and_sessions_keep_working() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let (phone, laptop) = use_the_app(scratch.path()).await;
    let data = tempfile::tempdir().expect("tempdir");
    v0_1_0_database(data.path(), scratch.path());

    // What v0.1.0 may have stored besides: a copy of a sentence whose
    // English differs only in its closing punctuation (v0.1.0's key kept
    // it), shown in a review and in a practice item.
    let (original, card, dup_review, dup_item) = {
        let conn = Connection::open(data.path().join(DB_FILE)).expect("open");
        let original = int(
            &conn,
            "SELECT MIN(s.id) FROM sentence s JOIN word w ON w.id = s.word_id \
             JOIN card c ON c.word_id = w.id WHERE c.reps > 0",
        );
        let card = int(
            &conn,
            &format!(
                "SELECT c.id FROM card c JOIN sentence s ON s.word_id = c.word_id \
                 WHERE s.id = {original} AND c.kind = 'understand'"
            ),
        );
        conn.execute_batch(&format!(
            "INSERT INTO sentence (id, word_id, en_marked, cs_marked, en_key, origin, location, \
               source_id, position, pack_import_id, created_at, updated_at) \
             SELECT 900, word_id, replace(en_marked, '.', '\u{2026}'), cs_marked, \
               replace(en_key, '.', '\u{2026}') || ' (v0.1.0 key)', origin, location, source_id, \
               99, pack_import_id, created_at, updated_at FROM sentence WHERE id = {original};
             INSERT INTO review_log (client_review_id, card_id, session_id, reviewed_at, client_ts, \
               review_day, raw_grade, fsrs_rating, suggested_grade, format, sentence_id, \
               answer_match, hint_used, elapsed_days, duration_ms, before_json, undone) \
             SELECT '00000000-0000-4000-8000-000000000900', card_id, session_id, reviewed_at, \
               client_ts, review_day, raw_grade, fsrs_rating, suggested_grade, format, 900, \
               answer_match, hint_used, elapsed_days, duration_ms, before_json, 1 \
               FROM review_log WHERE card_id = {card} ORDER BY id LIMIT 1;
             UPDATE session_item SET sentence_id = 900 \
               WHERE id = (SELECT MIN(id) FROM session_item WHERE card_id = {card});"
        ))
        .expect("a duplicate sentence as v0.1.0 kept it");
        let dup_review = int(&conn, "SELECT MAX(id) FROM review_log");
        let dup_item = int(
            &conn,
            &format!("SELECT MIN(id) FROM session_item WHERE card_id = {card}"),
        );
        (original, card, dup_review, dup_item)
    };
    assert!(card > 0);

    // The expected result: the same rows, the copy merged into the oldest.
    let expected_file = data.path().join("expected.sqlite");
    std::fs::copy(data.path().join(DB_FILE), &expected_file).expect("copy");
    let tables = v0_1_0_columns(&Connection::open(&expected_file).expect("open"));
    {
        let conn = Connection::open(&expected_file).expect("open");
        conn.execute_batch(&format!(
            "UPDATE review_log SET sentence_id = {original} WHERE sentence_id = 900;
             UPDATE session_item SET sentence_id = {original} WHERE sentence_id = 900;
             DELETE FROM sentence WHERE id = 900;"
        ))
        .expect("expected merge");
    }
    let expected = snapshot(&expected_file, &tables);
    std::fs::remove_file(&expected_file).expect("remove");
    let rows: usize = expected.values().map(Vec::len).sum();
    assert!(rows > 80, "realistic data: {rows} rows");

    // The upgrade (what `vocab-server check` and `serve` do first).
    drop(Db::open(data.path()).expect("migrates"));
    let actual = snapshot(&data.path().join(DB_FILE), &tables);
    for (table, rows) in &expected {
        let mut rows = rows.clone();
        let mut got = actual[table].clone();
        if table == "sentence" {
            // Re-keyed: compare without `en_key` (checked below).
            let strip = |r: &String| {
                let mut cols: Vec<&str> = r.split(" | ").collect();
                cols.remove(4);
                cols.join(" | ")
            };
            rows = rows.iter().map(strip).collect();
            got = got.iter().map(strip).collect();
        }
        assert_eq!(got, rows, "table {table} after the upgrade");
    }
    {
        let conn = Connection::open(data.path().join(DB_FILE)).expect("open");
        conn.pragma_update(None, "foreign_keys", "ON").expect("fk");
        assert_eq!(int(&conn, "PRAGMA user_version"), 7);
        let check: String = conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .expect("quick_check");
        assert_eq!(check, "ok");
        assert_eq!(
            int(&conn, "SELECT COUNT(*) FROM pragma_foreign_key_check"),
            0
        );
        let keys_unique = int(
            &conn,
            "SELECT COUNT(*) = COUNT(DISTINCT word_id || '|' || en_key) FROM sentence",
        );
        assert_eq!(keys_unique, 1);
        assert_eq!(
            int(
                &conn,
                &format!("SELECT sentence_id FROM review_log WHERE id = {dup_review}")
            ),
            original
        );
        assert_eq!(
            int(
                &conn,
                &format!("SELECT sentence_id FROM session_item WHERE id = {dup_item}")
            ),
            original
        );
        assert_eq!(
            int(
                &conn,
                "SELECT COUNT(*) FROM practice_session WHERE kind <> 'regular'"
            ),
            0
        );
        assert_eq!(
            int(
                &conn,
                "SELECT COUNT(*) FROM auth_session WHERE password_failures <> 0"
            ),
            0
        );
    }

    // The app on the upgraded database, later on the day the block was
    // left half done.
    let app = app_in(data.path(), "2026-11-11T15:00:00Z");
    let (status, setup) = app.get("/api/auth/setup", &phone).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(setup["required"], false, "{setup}");
    // Both devices are still logged in; the block resumes where it was.
    for cookie in [&phone, &laptop] {
        assert_eq!(app.get("/api/auth/me", cookie).await.0, StatusCode::OK);
    }
    let (status, current) = app.get("/api/sessions/current", &laptop).await;
    assert_eq!(status, StatusCode::OK, "{current}");
    assert_eq!(current["answered"], 1);
    assert_eq!(current["kind"], "regular");
    let (_, again) = app
        .post("/api/sessions", &json!({ "kind": "start" }), &phone)
        .await;
    assert_eq!(again["resumed"], true);
    assert_eq!(again["session"]["id"], current["id"]);
    let after = app.rate_next(&current, 2, 200, &laptop).await;
    assert_eq!(after["answered"], 2);
    let (status, view) = app.get("/api/today", &phone).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(view["new_waiting"].is_u64() && view["due_tomorrow"].is_u64());
    assert_eq!(view["sources"][0]["reading_position"], "Chapter 2");

    // Login with the password set under v0.1.0 still works.
    let fresh = log_in(&app).await;
    assert_eq!(app.get("/api/auth/me", &fresh).await.0, StatusCode::OK);

    // A session made under v0.1.0 ends 365 days after its creation
    // (2026-11-10T09:00Z), however often it was renewed.
    for (now, alive) in [
        ("2027-04-01T09:00:00Z", true),
        ("2027-09-01T09:00:00Z", true),
        ("2027-11-10T09:00:01Z", false),
    ] {
        app.clock.set(ts(now));
        let (status, _) = app.get("/api/auth/me", &phone).await;
        assert_eq!(status == StatusCode::OK, alive, "{now}: {status}");
    }

    // Changing the password: this device continues with a new session,
    // every other session ends.
    let fresh = log_in(&app).await;
    let (status, _, renewed) = app
        .send(post_json(
            "/api/auth/password",
            &json!({
                "current_password": PASSWORD,
                "new_password": "test-password-after-the-upgrade",
            }),
            Some(&fresh),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let renewed = renewed.expect("new cookie");
    assert_ne!(renewed, fresh);
    assert_eq!(app.get("/api/auth/me", &renewed).await.0, StatusCode::OK);
    for old in [&fresh, &phone, &laptop] {
        assert_eq!(
            app.get("/api/auth/me", old).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
}
