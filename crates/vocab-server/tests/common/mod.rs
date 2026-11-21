//! Shared harness of the integration tests: a temporary database, a manual
//! clock, the HTTP router (set up through the first-run setup with the
//! start's setup code and a synthetic `test-` password, then logged in) and
//! synthetic packs (`test-` ids, invented sentences only).

// Each test binary that includes this module uses a different subset of it.
#![allow(dead_code)]

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, header};
use rusqlite::Connection;
use serde_json::{Value, json};
use tower::ServiceExt;
use vocab_server::auth::{Auth, SessionLifetime, SetupCode};
use vocab_server::catalog::{self, WordId, WordTraits};
use vocab_server::clock::{Clock, DayRule, ManualClock, Moment, StudyDay, Timestamp};
use vocab_server::config::Limits;
use vocab_server::db::{Db, DbError};
use vocab_server::http::{self, AppState};

pub fn ts(text: &str) -> Timestamp {
    text.parse().expect("timestamp")
}

pub fn day(text: &str) -> StudyDay {
    text.parse().expect("day")
}

pub fn prague() -> DayRule {
    DayRule::new("Europe/Prague", 4).expect("bundled tzdb")
}

// ── authentication (synthetic secrets only) ────────────────────────────────

/// `public_origin` of the test apps (placeholder domain of the design).
pub const ORIGIN: &str = "https://vocab.example.home";
pub const PASSWORD: &str = "test-password-for-the-integration-tests";

/// Auth with the design's sessions (180 days sliding, 365 at most), and
/// its start's setup code.
pub fn auth() -> (Arc<Auth>, SetupCode) {
    let lifetime = SessionLifetime {
        sliding_days: 180,
        max_days: 365,
    };
    let (auth, code) = Auth::new(ORIGIN, lifetime).expect("auth");
    (Arc::new(auth), code)
}

/// Auth whose sessions outlive any clock jump of a test (100 years), for
/// tests about other things than sessions.
pub fn long_auth() -> (Arc<Auth>, SetupCode) {
    let lifetime = SessionLifetime {
        sliding_days: 36_500,
        max_days: 36_500,
    };
    let (auth, code) = Auth::new(ORIGIN, lifetime).expect("auth");
    (Arc::new(auth), code)
}

/// A JSON POST as the app's page sends it, with an optional cookie.
pub fn post_json(uri: &str, body: &Value, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

/// The `name=value` pair of a response's session cookie.
pub fn cookie_pair_of(response: &axum::response::Response) -> Option<String> {
    let cookie = response.headers().get(header::SET_COOKIE)?.to_str().ok()?;
    cookie.split(';').next().map(str::to_owned)
}

/// Sign in through `router`: the first-run setup with `code` and
/// [`PASSWORD`] when the app is not set up yet, a login otherwise. Returns
/// the `name=value` cookie pair.
pub async fn login(router: &axum::Router, code: &SetupCode) -> String {
    let setup = json!({ "code": code.to_string(), "password": PASSWORD });
    let response = router
        .clone()
        .oneshot(post_json("/api/auth/setup", &setup, None))
        .await
        .expect("response");
    let response = if response.status() == StatusCode::CONFLICT {
        router
            .clone()
            .oneshot(post_json(
                "/api/auth/login",
                &json!({ "password": PASSWORD }),
                None,
            ))
            .await
            .expect("response")
    } else {
        response
    };
    assert_eq!(response.status(), StatusCode::OK, "test setup or login");
    cookie_pair_of(&response).expect("session cookie")
}

/// Create a connector token with the session `cookie` (and the password,
/// which creating one asks for again); returns the token.
pub async fn connector_token(router: &axum::Router, cookie: &str) -> String {
    let response = router
        .clone()
        .oneshot(post_json(
            "/api/auth/connector-token",
            &json!({ "current_password": PASSWORD }),
            Some(cookie),
        ))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK, "create connector token");
    let bytes = to_bytes(response.into_body(), 1 << 16).await.expect("body");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    body["token"].as_str().expect("token").to_owned()
}

/// `router` as a logged-in browser of the app's origin would use it: sets
/// the app up (or logs in) on the first request and adds the session cookie
/// and the `Origin` header to every request that does not set them itself.
pub fn logged_in(router: axum::Router, code: SetupCode) -> axum::Router {
    let cookie: Arc<tokio::sync::OnceCell<String>> = Arc::new(tokio::sync::OnceCell::new());
    let inner = router;
    axum::Router::new().fallback_service(tower::service_fn(move |mut request: Request<Body>| {
        let inner = inner.clone();
        let cookie = Arc::clone(&cookie);
        let code = code.clone();
        async move {
            let pair = cookie.get_or_init(|| login(&inner, &code)).await;
            let headers = request.headers_mut();
            if !headers.contains_key(header::COOKIE) {
                headers.insert(header::COOKIE, HeaderValue::from_str(pair).expect("cookie"));
            }
            if !headers.contains_key(header::ORIGIN) {
                headers.insert(header::ORIGIN, HeaderValue::from_static(ORIGIN));
            }
            inner.oneshot(request).await
        }
    }))
}

pub struct Env {
    pub dir: tempfile::TempDir,
    pub db: Db,
    pub clock: Arc<ManualClock>,
    pub router: axum::Router,
    pack_no: std::cell::Cell<u32>,
}

/// Default start: Friday 2026-11-20, 10:00 in Prague (study day 2026-11-20).
pub const START: &str = "2026-11-20T09:00:00Z";

pub fn env() -> Env {
    let dir = tempfile::tempdir().expect("tempdir");
    env_in(dir, START)
}

/// A fresh app on the database in `dir` (a "restart" when the directory
/// was used before).
pub fn env_in(dir: tempfile::TempDir, now: &str) -> Env {
    let db = Db::open(dir.path()).expect("db");
    let clock = Arc::new(ManualClock::new(ts(now)));
    let (auth, code) = long_auth();
    let state = AppState::with_clock(
        db.clone(),
        Arc::clone(&clock) as Arc<dyn Clock>,
        prague(),
        auth,
    );
    let router = logged_in(http::app(state, &Limits::default()), code);
    Env {
        dir,
        db,
        clock,
        router,
        pack_no: std::cell::Cell::new(0),
    }
}

impl Env {
    pub fn set_time(&self, text: &str) {
        self.clock.set(ts(text));
    }

    /// 10:00 Prague time (09:00Z in winter) on `day`.
    pub fn set_day(&self, day: &str) {
        self.set_time(&format!("{day}T09:00:00Z"));
    }

    pub fn moment(&self) -> Moment {
        Moment::of(self.clock.now(), &prague())
    }

    pub fn today(&self) -> StudyDay {
        self.moment().day
    }

    // ── HTTP ────────────────────────────────────────────────────────────────

    pub async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
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
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        (status, value)
    }

    pub async fn request(
        &self,
        method: &str,
        uri: &str,
        body: Option<&Value>,
    ) -> (StatusCode, Value) {
        let builder = Request::builder().method(method).uri(uri);
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())),
            None => builder.body(Body::empty()),
        }
        .expect("request");
        self.send(request).await
    }

    pub async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.request("GET", uri, None).await
    }

    pub async fn post(&self, uri: &str, body: &Value) -> (StatusCode, Value) {
        self.request("POST", uri, Some(body)).await
    }

    // ── import ──────────────────────────────────────────────────────────────

    /// Import `words` into source `source` (one pack, committed).
    pub async fn import(&self, source: &str, words: Vec<Value>) -> Value {
        let n = self.pack_no.get() + 1;
        self.pack_no.set(n);
        let pack = json!({
            "format": "vocab-pack",
            "format_version": 1,
            "pack_id": format!("{source}--2026-11-17--{n}"),
            "exported_at": "2026-11-17",
            "source": { "id": source, "type": "book", "title": format!("Title of {source}") },
            "reading_position": "Chapter 3",
            "words": words,
        });
        let text = format!("```json\n{pack}\n```");
        let (status, report) = self
            .post("/api/import/commit", &json!({ "text": text }))
            .await;
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["packs"][0]["status"], "imported", "{report}");
        report
    }

    pub async fn word_id(&self, headword: &str) -> WordId {
        let headword = headword.to_owned();
        self.db
            .read(move |conn| {
                let rows = catalog::words(
                    conn,
                    &catalog::WordQuery {
                        text: Some(headword.clone()),
                        source: None,
                    },
                )?;
                Ok::<_, DbError>(
                    rows.into_iter()
                        .find(|r| r.headword == headword)
                        .unwrap_or_else(|| panic!("{headword} not found"))
                        .id,
                )
            })
            .await
            .expect("word id")
    }

    pub async fn traits(&self, id: WordId) -> WordTraits {
        self.db
            .read(move |conn| Ok::<_, DbError>(catalog::word_traits(conn, id)?))
            .await
            .expect("traits")
            .expect("word exists")
    }

    // ── direct inspection (and setup of state later milestones own) ────────

    pub fn sql(&self) -> Connection {
        Connection::open(self.dir.path().join("vocabulary.sqlite")).expect("open")
    }

    pub fn int(&self, sql: &str) -> i64 {
        self.sql().query_row(sql, [], |row| row.get(0)).expect(sql)
    }

    pub fn exec(&self, sql: &str) {
        self.sql().execute_batch(sql).expect(sql);
    }

    /// Rows of `table` (all columns, debug-formatted), for exact comparisons.
    pub fn rows(&self, table: &str, condition: &str) -> Vec<String> {
        let conn = self.sql();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT * FROM {table} WHERE {condition} ORDER BY rowid"
            ))
            .expect("select");
        let columns = stmt.column_count();
        stmt.query_map([], |row| {
            (0..columns)
                .map(|i| {
                    row.get::<_, rusqlite::types::Value>(i)
                        .map(|v| format!("{v:?}"))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|cols| cols.join(" | "))
        })
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect()
    }
}

// ── synthetic words ─────────────────────────────────────────────────────────

/// `n` invented sentences for `stem`.
pub fn sentences(stem: &str, n: usize) -> Vec<Value> {
    (1..=n)
        .map(|i| {
            json!({
                "en": format!("Test line {i} with the [[{stem}]] in it."),
                "cs": format!("Testovací řádek {i} s [[{stem}]] v něm."),
                "origin": "example",
            })
        })
        .collect()
}

/// A word with `n` sentences.
pub fn word(headword: &str, usefulness: u8, n: usize) -> Value {
    json!({
        "headword": headword,
        "meaning": format!("Invented test meaning of {headword}."),
        "czech": [format!("{headword}-cz")],
        "usefulness": usefulness,
        "sentences": sentences(headword, n),
    })
}
