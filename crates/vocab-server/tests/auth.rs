//! Authentication and request protection (design §9; R-82, R-83) through the
//! real router: first-run setup, sessions, cookie attributes, password
//! change, the connector token's scope, the CSRF guard, the login throttle
//! and the per-endpoint limits. Secrets are synthetic (`test-…`) or made by
//! the app itself.

mod common;

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use serde_json::{Value, json};
use tower::ServiceExt;
use vocab_server::auth::{self, Auth, SetupCode};
use vocab_server::clock::{Clock, ManualClock};
use vocab_server::config::Limits;
use vocab_server::db::Db;
use vocab_server::http::{self, Access, AppState, RouteInfo};

const START: &str = "2026-11-20T09:00:00Z";
const COOKIE: &str = "__Host-vocab_session";

struct App {
    dir: tempfile::TempDir,
    clock: Arc<ManualClock>,
    router: axum::Router,
    db: Db,
    /// This start's setup code.
    code: SetupCode,
}

/// The app on the database in `dir` (a restart when it was used before).
fn app_with(dir: tempfile::TempDir, (auth, code): (Arc<Auth>, SetupCode)) -> App {
    let db = Db::open(dir.path()).expect("db");
    let clock = Arc::new(ManualClock::new(START.parse().expect("ts")));
    let state = AppState::with_clock(
        db.clone(),
        Arc::clone(&clock) as Arc<dyn Clock>,
        common::prague(),
        auth,
    );
    App {
        dir,
        clock,
        router: http::app(state, &Limits::default()),
        db,
        code,
    }
}

/// A first start: setup state.
fn fresh() -> App {
    app_with(tempfile::tempdir().expect("tempdir"), common::auth())
}

/// A set-up app (password [`common::PASSWORD`]) without any session.
async fn app() -> App {
    let app = fresh();
    let response = app.setup(&app.code.to_string(), common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::OK, "setup");
    let cookie = cookie_pair(&response).expect("cookie");
    app.send(request(
        &Method::POST,
        "/api/auth/logout",
        "{}",
        &[("cookie", &cookie)],
    ))
    .await;
    app
}

/// A request as the app's own page would send it (Origin, JSON) plus `extra` headers.
fn request(method: &Method, uri: &str, body: &str, extra: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method(method.clone()).uri(uri);
    if method != Method::GET {
        builder = builder
            .header(header::ORIGIN, common::ORIGIN)
            .header(header::CONTENT_TYPE, "application/json");
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::from(body.to_owned())).expect("request")
}

/// A request without any default headers.
fn bare(method: &Method, uri: &str, body: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method(method.clone()).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::from(body.to_owned())).expect("request")
}

impl App {
    fn advance(&self, duration: jiff::SignedDuration) {
        let now = self.clock.now();
        self.clock.set(now.checked_add(duration).expect("in range"));
    }

    async fn send(&self, request: Request<Body>) -> Response {
        self.router
            .clone()
            .oneshot(request)
            .await
            .expect("response")
    }

    async fn setup(&self, code: &str, password: &str) -> Response {
        self.send(request(
            &Method::POST,
            "/api/auth/setup",
            &json!({ "code": code, "password": password }).to_string(),
            &[],
        ))
        .await
    }

    async fn change_password(&self, cookie: &str, current: &str, new: &str) -> Response {
        self.send(request(
            &Method::POST,
            "/api/auth/password",
            &json!({ "current_password": current, "new_password": new }).to_string(),
            &[("cookie", cookie)],
        ))
        .await
    }

    /// Log in and create a connector token; returns it.
    async fn connector_token(&self) -> String {
        let cookie = self.login().await;
        common::connector_token(&self.router, &cookie).await
    }

    async fn login_with(&self, password: &str) -> Response {
        self.send(request(
            &Method::POST,
            "/api/auth/login",
            &json!({ "password": password }).to_string(),
            &[],
        ))
        .await
    }

    /// Log in; returns the `name=value` cookie pair.
    async fn login(&self) -> String {
        let response = self.login_with(common::PASSWORD).await;
        assert_eq!(response.status(), StatusCode::OK);
        cookie_pair(&response).expect("cookie")
    }

    async fn get_with_cookie(&self, uri: &str, cookie: &str) -> Response {
        self.send(request(&Method::GET, uri, "", &[("cookie", cookie)]))
            .await
    }
}

fn set_cookie(response: &Response) -> Option<String> {
    response
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().expect("ascii").to_owned())
}

fn cookie_pair(response: &Response) -> Option<String> {
    set_cookie(response).map(|c| c.split(';').next().expect("pair").to_owned())
}

async fn json_body(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1 << 20).await.expect("body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn error_code(response: Response) -> String {
    json_body(response).await["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// A concrete URI for a route template.
fn concrete(path: &str) -> String {
    path.replace("{id}", "1")
        .replace("{kind}", "czech")
        .replace("{key}", "test-key")
        .replace("{build}", "0")
        .replace("{*path}", "js/main.js")
}

fn days(n: i64) -> jiff::SignedDuration {
    jiff::SignedDuration::from_hours(24 * n)
}

fn minutes(n: i64) -> jiff::SignedDuration {
    jiff::SignedDuration::from_mins(n)
}

// ── which route needs what ─────────────────────────────────────────────────

#[test]
fn r82_only_health_setup_login_and_the_shell_are_public() {
    let routes = http::routes();
    let public: Vec<(Method, &str)> = routes
        .iter()
        .filter(|r| r.access == Access::Public)
        .map(|r| (r.method.clone(), r.path))
        .collect();
    assert_eq!(
        public,
        [
            (Method::GET, "/healthz"),
            (Method::GET, "/"),
            (Method::GET, "/assets/{build}/{*path}"),
            (Method::GET, "/api/auth/setup"),
            (Method::POST, "/api/auth/setup"),
            (Method::POST, "/api/auth/login"),
        ]
    );
    for RouteInfo {
        method,
        path,
        access,
    } in &routes
    {
        let connector = path.starts_with("/api/connector/v1/");
        assert_eq!(
            *access == Access::ImportToken,
            connector,
            "{method} {path}: the connector token is for connector routes only"
        );
        if path.starts_with("/api/") && !["/api/auth/login", "/api/auth/setup"].contains(path) {
            assert_ne!(*access, Access::Public, "{method} {path}");
        }
    }
    assert!(routes.iter().any(|r| r.access == Access::ImportToken));
}

#[tokio::test]
async fn r82_every_session_route_answers_401_without_a_valid_session() {
    let app = app().await;
    let stale = format!("{COOKIE}={}", "ab".repeat(32));
    let bearer = format!("Bearer {}", app.connector_token().await);
    let mut checked = 0;
    for route in http::routes()
        .into_iter()
        .filter(|r| r.access == Access::Session)
    {
        let uri = concrete(route.path);
        for extra in [
            vec![],
            vec![("cookie", stale.as_str())],
            vec![("authorization", bearer.as_str())],
        ] {
            let response = app.send(request(&route.method, &uri, "{}", &extra)).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{} {uri} with {extra:?}",
                route.method
            );
            assert!(set_cookie(&response).is_none());
            assert_eq!(error_code(response).await, "unauthenticated");
        }
        checked += 1;
    }
    assert!(checked >= 15, "only {checked} session routes");
    // Methods a path does not have, too: no hint that the path exists.
    for (method, uri) in [
        (Method::DELETE, "/api/settings"),
        (Method::PUT, "/api/today"),
    ] {
        let response = app.send(request(&method, uri, "{}", &[])).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri}"
        );
    }
}

#[tokio::test]
async fn r82_public_routes_work_without_a_session() {
    let app = app().await;
    for uri in ["/healthz", "/"] {
        let response = app.send(request(&Method::GET, uri, "", &[])).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
}

// ── login, cookie, logout ──────────────────────────────────────────────────

#[tokio::test]
async fn r82_login_sets_a_host_only_secure_http_only_strict_cookie() {
    let app = app().await;
    let response = app.login_with(common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = set_cookie(&response).expect("Set-Cookie");
    let mut parts = cookie.split("; ");
    let pair = parts.next().expect("pair");
    let token = pair
        .strip_prefix(&format!("{COOKIE}="))
        .expect("cookie name");
    assert_eq!(token.len(), 64, "32 random bytes as hex");
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    let mut attributes: Vec<&str> = parts.collect();
    attributes.sort_unstable();
    assert_eq!(
        attributes,
        [
            "HttpOnly",
            "Max-Age=15552000",
            "Path=/",
            "SameSite=Strict",
            "Secure"
        ]
    );
    assert!(!cookie.to_ascii_lowercase().contains("domain"));

    // The database keeps only the token's SHA-256, never the token.
    let conn = rusqlite::Connection::open(app.dir.path().join("vocabulary.sqlite")).expect("open");
    let stored: String = conn
        .query_row("SELECT token_sha256 FROM auth_session", [], |r| r.get(0))
        .expect("one session");
    assert_ne!(stored, token);
    assert_eq!(stored.len(), 64);

    // Two logins give two different sessions.
    let other = app.login().await;
    assert_ne!(other, pair);
}

#[tokio::test]
async fn r82_wrong_password_gets_401_and_no_cookie() {
    let app = app().await;
    for password in ["test-password-wrong", "", &format!("{} ", common::PASSWORD)] {
        let response = app.login_with(password).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{password:?}");
        assert!(set_cookie(&response).is_none());
        assert_eq!(error_code(response).await, "invalid_password");
    }
    // Not JSON of the expected shape.
    let response = app
        .send(request(
            &Method::POST,
            "/api/auth/login",
            r#"{"pass": "x"}"#,
            &[],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn r82_a_session_opens_the_api_and_logout_ends_it() {
    let app = app().await;
    let cookie = app.login().await;
    for uri in [
        "/api/today",
        "/api/auth/me",
        "/api/settings",
        "/api/sources",
    ] {
        let response = app.get_with_cookie(uri, &cookie).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
    let response = app
        .send(request(
            &Method::POST,
            "/api/auth/logout",
            "{}",
            &[("cookie", &cookie)],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cleared = set_cookie(&response).expect("clearing cookie");
    assert!(cleared.starts_with(&format!("{COOKIE}=;")), "{cleared}");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    let response = app.get_with_cookie("/api/today", &cookie).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn r82_sessions_last_180_days_and_slide_when_older_than_7_days() {
    let app = app().await;
    let cookie = app.login().await;
    app.advance(days(7));
    let response = app.get_with_cookie("/api/today", &cookie).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(set_cookie(&response).is_none(), "not renewed within 7 days");

    app.advance(days(1));
    let response = app.get_with_cookie("/api/today", &cookie).await;
    assert_eq!(response.status(), StatusCode::OK);
    let renewed = set_cookie(&response).expect("renewed after 7 days");
    assert!(
        renewed.starts_with(&format!("{cookie};")),
        "same token: {renewed}"
    );
    assert!(renewed.contains("Max-Age=15552000"), "{renewed}");

    // 179 days after the renewal: still valid (it would not be without it).
    app.advance(days(179));
    let response = app.get_with_cookie("/api/today", &cookie).await;
    assert_eq!(response.status(), StatusCode::OK);

    // A session left alone for 180 days expires; also after a restart.
    let fresh = app.login().await;
    app.advance(days(180));
    let response = app.get_with_cookie("/api/today", &fresh).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn r82_a_session_survives_a_restart() {
    let first = app().await;
    let cookie = first.login().await;
    let restarted = app_with(first.dir, common::auth());
    let response = restarted.get_with_cookie("/api/today", &cookie).await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// B2 (review probe p3): a password change ends every session, the
/// changing one included; the device that made it gets a new token, so a
/// stolen copy of its old cookie dies too.
#[tokio::test]
async fn r82_changing_the_password_ends_every_session_and_reissues_this_device() {
    let app = app().await;
    let other = app.login().await;
    let mine = app.login().await;
    let new_password = "test-password-after-the-change";

    // A wrong current password is refused; a short new one too.
    let response = app
        .change_password(&mine, "test-password-wrong", new_password)
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(response).await, "invalid_password");
    let response = app
        .change_password(&mine, common::PASSWORD, "test-short")
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_code(response).await, "invalid_new_password");
    assert_eq!(
        app.get_with_cookie("/api/today", &other).await.status(),
        StatusCode::OK
    );
    // Without a session: 401.
    let response = app
        .send(request(
            &Method::POST,
            "/api/auth/password",
            &json!({ "current_password": common::PASSWORD, "new_password": new_password })
                .to_string(),
            &[],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let stolen = mine.clone();
    let response = app
        .change_password(&mine, common::PASSWORD, new_password)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let reissued = set_cookie(&response).expect("a new session cookie");
    assert!(reissued.contains("HttpOnly") && reissued.contains("SameSite=Strict"));
    let mine = cookie_pair(&response).expect("cookie");
    assert_ne!(mine, stolen, "a new token");
    assert_eq!(
        app.get_with_cookie("/api/today", &mine).await.status(),
        StatusCode::OK
    );
    for (label, dead) in [
        ("the old token of this device", &stolen),
        ("another device", &other),
    ] {
        assert_eq!(
            app.get_with_cookie("/api/auth/me", dead).await.status(),
            StatusCode::UNAUTHORIZED,
            "{label}"
        );
    }
    let conn = rusqlite::Connection::open(app.dir.path().join("vocabulary.sqlite")).expect("open");
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM auth_session", [], |r| r.get(0))
        .expect("count");
    assert_eq!(rows, 1, "every other row is gone");
    let response = app.login_with(common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "old password");
    assert_eq!(app.login_with(new_password).await.status(), StatusCode::OK);

    // After a restart (sessions bound to the new password's fingerprint).
    let restarted = app_with(app.dir, common::auth());
    assert_eq!(
        restarted
            .get_with_cookie("/api/today", &mine)
            .await
            .status(),
        StatusCode::OK
    );
    for dead in [&stolen, &other] {
        assert_eq!(
            restarted.get_with_cookie("/api/today", dead).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        restarted.login_with(new_password).await.status(),
        StatusCode::OK
    );
}

/// B2: a session never lives longer than 365 days from its creation,
/// however often it is renewed.
#[tokio::test]
async fn r82_a_session_ends_365_days_after_its_creation_even_when_renewed() {
    let app = app().await;
    let cookie = app.login().await;
    let mut last_max_age = None;
    // Used every 30 days: renewed each time.
    for step in 1..=12 {
        app.advance(days(30));
        let response = app.get_with_cookie("/api/today", &cookie).await;
        assert_eq!(response.status(), StatusCode::OK, "day {}", step * 30);
        let renewed = set_cookie(&response).expect("renewed");
        let max_age: i64 = renewed
            .split("Max-Age=")
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .and_then(|n| n.parse().ok())
            .expect("Max-Age");
        last_max_age = Some(max_age);
    }
    // Day 360: the cookie is only good for the five days that are left.
    assert_eq!(last_max_age, Some(5 * 24 * 3600));
    app.advance(days(4));
    assert_eq!(
        app.get_with_cookie("/api/today", &cookie).await.status(),
        StatusCode::OK,
        "day 364"
    );
    app.advance(days(1));
    assert_eq!(
        app.get_with_cookie("/api/today", &cookie).await.status(),
        StatusCode::UNAUTHORIZED,
        "day 365"
    );
    // The same after a restart, from the stored creation time.
    let other = app.login().await;
    app.advance(days(170));
    assert_eq!(
        app.get_with_cookie("/api/today", &other).await.status(),
        StatusCode::OK
    );
    app.advance(days(170));
    assert_eq!(
        app.get_with_cookie("/api/today", &other).await.status(),
        StatusCode::OK
    );
    let start = app.clock.now();
    let restarted = app_with(app.dir, common::auth());
    restarted.clock.set(start);
    restarted.advance(days(24));
    assert_eq!(
        restarted
            .get_with_cookie("/api/today", &other)
            .await
            .status(),
        StatusCode::OK,
        "day 364"
    );
    restarted.advance(days(1));
    assert_eq!(
        restarted
            .get_with_cookie("/api/today", &other)
            .await
            .status(),
        StatusCode::UNAUTHORIZED,
        "day 365"
    );
}

/// B2: "log out everywhere" ends every session, this device's included.
#[tokio::test]
async fn r82_logout_all_ends_every_session() {
    let app = app().await;
    let phone = app.login().await;
    let laptop = app.login().await;
    // Needs a session and the app's origin, like every state change.
    let response = app
        .send(request(&Method::POST, "/api/auth/logout-all", "{}", &[]))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .send(bare(
            &Method::POST,
            "/api/auth/logout-all",
            "{}",
            &[
                ("cookie", &laptop),
                ("content-type", "application/json"),
                ("origin", "https://other.example.home"),
            ],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        app.get_with_cookie("/api/auth/me", &phone).await.status(),
        StatusCode::OK
    );

    let response = app
        .send(request(
            &Method::POST,
            "/api/auth/logout-all",
            "{}",
            &[("cookie", &laptop)],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cleared = set_cookie(&response).expect("cookie removed");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    for cookie in [&phone, &laptop] {
        assert_eq!(
            app.get_with_cookie("/api/auth/me", cookie).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let restarted = app_with(app.dir, common::auth());
    assert_eq!(
        restarted
            .get_with_cookie("/api/auth/me", &phone)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // Logging in again works; the password is unchanged.
    assert_eq!(
        restarted.login_with(common::PASSWORD).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn r83_password_change_body_is_limited_to_2_kib() {
    let app = app().await;
    let cookie = app.login().await;
    let body = format!(
        r#"{{"current_password":"x","new_password":"{}"}}"#,
        "x".repeat(2100)
    );
    let response = app
        .send(request(
            &Method::POST,
            "/api/auth/password",
            &body,
            &[("cookie", &cookie)],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

// ── first-run setup (§9.1) ─────────────────────────────────────────────────

async fn setup_required(app: &App) -> bool {
    let response = app
        .send(request(&Method::GET, "/api/auth/setup", "", &[]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await["required"]
        .as_bool()
        .expect("required")
}

#[tokio::test]
async fn r82_first_start_asks_for_the_setup_code_and_a_new_password() {
    let app = fresh();
    let code = app.code.to_string();
    assert!(setup_required(&app).await);
    let response = app.login_with(common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(response).await, "setup_required");
    assert_eq!(
        app.get_with_cookie("/api/today", "x=y").await.status(),
        StatusCode::UNAUTHORIZED
    );

    // Wrong code; right code with a short password: nothing is set up.
    let response = app.setup("ABCD-EFGH-JK", common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(set_cookie(&response).is_none());
    assert_eq!(error_code(response).await, "invalid_setup_code");
    let response = app.setup(&code, "test-short1").await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_code(response).await, "invalid_new_password");
    assert!(setup_required(&app).await);

    // The code as typed on a phone: lower case, without dashes.
    let typed = code.replace('-', "").to_lowercase();
    let response = app.setup(&typed, common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = cookie_pair(&response).expect("logged in");
    assert_eq!(
        app.get_with_cookie("/api/today", &cookie).await.status(),
        StatusCode::OK
    );
    assert!(!setup_required(&app).await);
    let conn = rusqlite::Connection::open(app.dir.path().join("vocabulary.sqlite")).expect("open");
    let stored: String = conn
        .query_row("SELECT password_phc FROM auth_owner", [], |r| r.get(0))
        .expect("stored hash");
    // Built, so that no literal hash prefix is in the source (secret scanner).
    let prefix = ["", "argon2id", "v=19", "m=19456,t=2,p=1", ""].join("$");
    assert!(stored.starts_with(&prefix), "{stored}");

    // Once set up, setup is refused for good: same code, and after a restart.
    let response = app.setup(&code, "test-password-of-an-intruder").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(response).await, "already_set_up");
    let restarted = app_with(app.dir, common::auth());
    let code = restarted.code.to_string();
    let response = restarted.setup(&code, "test-password-of-an-intruder").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(!setup_required(&restarted).await);
    assert_eq!(
        restarted.login_with(common::PASSWORD).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn r82_two_concurrent_setups_exactly_one_wins() {
    let app = fresh();
    let code = app.code.to_string();
    let (a, b) = tokio::join!(
        app.setup(&code, "test-password-first-of-two"),
        app.setup(&code, "test-password-second-of-two"),
    );
    let mut statuses = [a.status(), b.status()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    let winner = if a.status() == StatusCode::OK {
        "test-password-first-of-two"
    } else {
        "test-password-second-of-two"
    };
    assert_eq!(app.login_with(winner).await.status(), StatusCode::OK);
}

/// B3(a) (review probe p3): wrong setup codes lock nothing. Whoever can
/// reach the app during setup cannot keep the owner from setting it up;
/// each wrong code only costs a fixed delay.
#[tokio::test]
async fn r82_wrong_setup_codes_never_lock_setup_or_login() {
    let app = fresh();
    let started = std::time::Instant::now();
    for attempt in 0..8 {
        let response = app.setup("0000-0000-00", common::PASSWORD).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "attempt {attempt}"
        );
        assert!(response.headers().get(header::RETRY_AFTER).is_none());
        assert_eq!(error_code(response).await, "invalid_setup_code");
    }
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(8 * 250),
        "each wrong code is delayed: {:?}",
        started.elapsed()
    );
    // The right code works at once, without any waiting period.
    let response = app.setup(&app.code.to_string(), common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::OK);
    // … and the wrong codes did not count as failed logins.
    assert_eq!(
        app.login_with(common::PASSWORD).await.status(),
        StatusCode::OK
    );
    let state: String = rusqlite::Connection::open(app.dir.path().join("vocabulary.sqlite"))
        .expect("open")
        .query_row("SELECT state_json FROM auth_throttle", [], |r| r.get(0))
        .expect("state");
    assert_eq!(state, "{}");
}

#[tokio::test]
async fn r83_setup_body_is_limited_to_2_kib() {
    let app = fresh();
    let body = format!(r#"{{"code":"x","password":"{}"}}"#, "x".repeat(2100));
    let response = app
        .send(request(&Method::POST, "/api/auth/setup", &body, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(setup_required(&app).await);
}

#[tokio::test]
async fn r82_reset_password_returns_to_setup_and_ends_every_session() {
    let app = app().await;
    let cookie = app.login().await;
    let token = app.connector_token().await;
    auth::reset_password(&app.db).await.expect("reset");
    let restarted = app_with(app.dir, common::auth());
    assert!(setup_required(&restarted).await);
    assert_eq!(
        restarted
            .get_with_cookie("/api/today", &cookie)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = restarted.login_with(common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    // The connector token is revoked too (a reset may follow a compromise).
    let response = restarted
        .send(bare(
            &Method::GET,
            "/api/connector/v1/sources",
            "",
            &[("authorization", &format!("Bearer {token}"))],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let code = restarted.code.to_string();
    let response = restarted
        .setup(&code, "test-password-after-the-reset")
        .await;
    assert_eq!(response.status(), StatusCode::OK);
}

// ── CSRF guard (§9.3) ──────────────────────────────────────────────────────

fn state_changing_routes() -> Vec<RouteInfo> {
    http::routes()
        .into_iter()
        .filter(|r| r.method != Method::GET && r.access != Access::ImportToken)
        .collect()
}

#[tokio::test]
async fn r82_state_changing_requests_from_another_origin_are_refused() {
    let app = app().await;
    let cookie = app.login().await;
    let routes = state_changing_routes();
    assert!(routes.iter().any(|r| r.path == "/api/auth/login"));
    for route in routes {
        let uri = concrete(route.path);
        for origin in [
            Some("https://other.example.home"),
            Some("http://vocab.example.home"),
            Some("https://vocab.example.home:8443"),
            Some("null"),
            None,
        ] {
            let mut headers = vec![
                ("cookie", cookie.as_str()),
                ("content-type", "application/json"),
            ];
            if let Some(origin) = origin {
                headers.push(("origin", origin));
            }
            let response = app.send(bare(&route.method, &uri, "{}", &headers)).await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{} {uri} from {origin:?}",
                route.method
            );
            assert_eq!(error_code(response).await, "forbidden_origin");
        }
    }
    // Nothing was logged in or changed by those requests.
    let conn = rusqlite::Connection::open(app.dir.path().join("vocabulary.sqlite")).expect("open");
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM auth_session", [], |r| r.get(0))
        .expect("count");
    assert_eq!(sessions, 1);
}

#[tokio::test]
async fn r82_state_changing_requests_need_a_json_content_type() {
    let app = app().await;
    let cookie = app.login().await;
    for route in state_changing_routes() {
        let uri = concrete(route.path);
        for content_type in [
            Some("application/x-www-form-urlencoded"),
            Some("multipart/form-data; boundary=x"),
            Some("text/plain"),
            Some("application/json-seq"),
            None,
        ] {
            let mut headers = vec![("cookie", cookie.as_str()), ("origin", common::ORIGIN)];
            if let Some(content_type) = content_type {
                headers.push(("content-type", content_type));
            }
            let response = app.send(bare(&route.method, &uri, "{}", &headers)).await;
            assert_eq!(
                response.status(),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "{} {uri} as {content_type:?}",
                route.method
            );
        }
    }
    // A charset parameter is fine.
    let response = app
        .send(bare(
            &Method::PATCH,
            "/api/settings",
            "{}",
            &[
                ("cookie", &cookie),
                ("origin", common::ORIGIN),
                ("content-type", "application/json; charset=utf-8"),
            ],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
}

// ── connector token (§9.2) ─────────────────────────────────────────────────

#[tokio::test]
async fn r82_without_a_created_token_the_connector_routes_refuse_everything() {
    let app = app().await;
    let cookie = app.login().await;
    let connector = "/api/connector/v1/sources";
    for value in ["Bearer ", "Bearer x", &format!("Bearer {}", "0".repeat(64))] {
        let response = app
            .send(bare(
                &Method::GET,
                connector,
                "",
                &[("authorization", value)],
            ))
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{value}");
    }
    let response = app
        .get_with_cookie("/api/auth/connector-token", &cookie)
        .await;
    assert_eq!(
        json_body(response).await,
        json!({ "exists": false, "created_at": null })
    );
}

#[tokio::test]
async fn r82_a_new_connector_token_is_shown_once_and_replaces_the_old_one() {
    let app = app().await;
    let cookie = app.login().await;
    // Creating one needs a session.
    let response = app
        .send(request(
            &Method::POST,
            "/api/auth/connector-token",
            "{}",
            &[],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // ...and the current password again.
    for body in [
        json!({ "current_password": "test-password-wrong" }),
        json!({}),
    ] {
        let response = app
            .send(request(
                &Method::POST,
                "/api/auth/connector-token",
                &body.to_string(),
                &[("cookie", &cookie)],
            ))
            .await;
        assert!(response.status().is_client_error(), "{body}");
        assert_ne!(response.status(), StatusCode::OK, "{body}");
    }
    let response = app
        .get_with_cookie("/api/auth/connector-token", &cookie)
        .await;
    assert_eq!(
        json_body(response).await["exists"],
        false,
        "nothing created"
    );

    let first = common::connector_token(&app.router, &cookie).await;
    assert_eq!(first.len(), 64);
    let response = app
        .get_with_cookie("/api/auth/connector-token", &cookie)
        .await;
    let status = json_body(response).await;
    assert_eq!(status["exists"], true, "{status}");
    assert!(status["created_at"].is_string(), "{status}");
    assert!(status.get("token").is_none(), "{status}");
    let conn = rusqlite::Connection::open(app.dir.path().join("vocabulary.sqlite")).expect("open");
    let stored: String = conn
        .query_row("SELECT connector_token_sha256 FROM auth_owner", [], |r| {
            r.get(0)
        })
        .expect("digest");
    assert_eq!(stored.len(), 64);
    assert_ne!(stored, first, "only the digest is stored");

    let second = common::connector_token(&app.router, &cookie).await;
    assert_ne!(first, second);
    let get = |token: String| {
        app.send(bare(
            &Method::GET,
            "/api/connector/v1/sources",
            "",
            &[("authorization", &format!("Bearer {token}"))],
        ))
    };
    assert_eq!(get(first).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(get(second.clone()).await.status(), StatusCode::OK);
    // Also after a restart.
    let restarted = app_with(app.dir, common::auth());
    let response = restarted
        .send(bare(
            &Method::GET,
            "/api/connector/v1/sources",
            "",
            &[("authorization", &format!("Bearer {second}"))],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn r82_the_connector_token_works_only_on_connector_routes() {
    let app = app().await;
    let cookie = app.login().await;
    let token = common::connector_token(&app.router, &cookie).await;
    let bearer = format!("Bearer {token}");
    let connector = "/api/connector/v1/sources";

    for scheme in ["Bearer", "bearer"] {
        let value = format!("{scheme} {token}");
        let response = app
            .send(bare(
                &Method::GET,
                connector,
                "",
                &[("authorization", &value)],
            ))
            .await;
        assert_eq!(response.status(), StatusCode::OK, "{scheme}");
        assert!(json_body(response).await.is_array());
    }
    let wrong = format!("Bearer {token}x");
    for headers in [
        vec![("cookie", cookie.as_str())],
        vec![("authorization", wrong.as_str())],
        vec![("authorization", token.as_str())],
        vec![],
    ] {
        let response = app.send(bare(&Method::GET, connector, "", &headers)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{headers:?}");
        assert_eq!(
            response
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .map(|v| v.to_str().expect("ascii")),
            Some("Bearer")
        );
    }
    // The token opens nothing else.
    for (method, uri) in [
        (Method::GET, "/api/today"),
        (Method::GET, "/api/sources"),
        (Method::POST, "/api/import/commit"),
    ] {
        let response = app
            .send(request(&method, uri, "{}", &[("authorization", &bearer)]))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri}"
        );
    }
}

// ── login throttle (§9.4) ──────────────────────────────────────────────────

async fn fail_logins(app: &App, n: usize) -> Response {
    let mut last = None;
    for _ in 0..n {
        last = Some(app.login_with("test-password-wrong").await);
    }
    last.expect("at least one attempt")
}

fn retry_after(response: &Response) -> u64 {
    response
        .headers()
        .get(header::RETRY_AFTER)
        .expect("Retry-After")
        .to_str()
        .expect("ascii")
        .parse()
        .expect("seconds")
}

#[tokio::test]
async fn r82_five_failed_logins_lock_logins_for_15_minutes_doubling() {
    let app = app().await;
    let existing = app.login().await;

    let response = fail_logins(&app, 4).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = fail_logins(&app, 1).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&response), 15 * 60);
    assert_eq!(error_code(response).await, "locked");

    // Even the right password is refused during the lockout...
    app.advance(minutes(14));
    let response = app.login_with(common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&response), 60);
    // ...while existing sessions keep working.
    let response = app.get_with_cookie("/api/today", &existing).await;
    assert_eq!(response.status(), StatusCode::OK);

    // The second lockout lasts twice as long.
    app.advance(minutes(1));
    let response = fail_logins(&app, 5).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&response), 30 * 60);
    app.advance(minutes(30));

    // A successful login resets the throttle: the next lockout is 15 minutes.
    assert_eq!(
        app.login_with(common::PASSWORD).await.status(),
        StatusCode::OK
    );
    let response = fail_logins(&app, 5).await;
    assert_eq!(retry_after(&response), 15 * 60);
}

/// B3(b): more than 20 failed logins within 24 hours lock logins for 24
/// hours, also when they come slowly enough never to trip the first rule.
#[tokio::test]
async fn r82_more_than_twenty_failed_logins_in_a_day_lock_for_24_hours() {
    let app = app().await;
    let existing = app.login().await;
    for n in 1..=20 {
        let response = fail_logins(&app, 1).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "failure {n}");
        app.advance(minutes(16));
    }
    let response = fail_logins(&app, 1).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&response), 24 * 3600);
    app.advance(minutes(24 * 60 - 1));
    let response = app.login_with(common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    // Existing sessions never notice.
    assert_eq!(
        app.get_with_cookie("/api/today", &existing).await.status(),
        StatusCode::OK
    );
    app.advance(minutes(1));
    assert_eq!(
        app.login_with(common::PASSWORD).await.status(),
        StatusCode::OK
    );
}

/// B3(c) (review probe p3): a password typed inside a session is counted
/// against that session, never against logins, and is never blocked by
/// the login lock. Five wrong ones end the session.
#[tokio::test]
async fn r82_password_checks_inside_a_session_are_separate_from_the_login_lock() {
    let app = app().await;
    let mine = app.login().await;
    let other = app.login().await;
    let token_request = |cookie: &str, password: &str| {
        request(
            &Method::POST,
            "/api/auth/connector-token",
            &json!({ "current_password": password }).to_string(),
            &[("cookie", cookie)],
        )
    };

    // Somebody without a session locks logins …
    let response = fail_logins(&app, 5).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        app.login_with(common::PASSWORD).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // … and the logged-in owner still manages his account.
    let response = app.send(token_request(&mine, common::PASSWORD)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "connector token during the lock"
    );
    let response = app
        .change_password(
            &mine,
            common::PASSWORD,
            "test-password-changed-while-locked",
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "password change during the lock"
    );
    let mine = cookie_pair(&response).expect("new cookie");
    let password = "test-password-changed-while-locked";
    // (The change ended the other session; get a second one after the lock.)
    assert_eq!(
        app.get_with_cookie("/api/auth/me", &other).await.status(),
        StatusCode::UNAUTHORIZED
    );
    app.advance(minutes(15));
    let response = app.login_with(password).await;
    assert_eq!(response.status(), StatusCode::OK);
    let other = cookie_pair(&response).expect("cookie");

    // Four wrong passwords inside a session: refused, the session stays,
    // and logins are not affected at all.
    for attempt in 1..=4 {
        let response = app.send(token_request(&mine, "test-password-wrong")).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "attempt {attempt}"
        );
        assert_eq!(error_code(response).await, "invalid_password");
    }
    assert_eq!(app.login_with(password).await.status(), StatusCode::OK);
    // A right one in between starts the count again.
    assert_eq!(
        app.send(token_request(&mine, password)).await.status(),
        StatusCode::OK
    );
    for _ in 1..=4 {
        app.change_password(&mine, "test-password-wrong", "test-password-never-set")
            .await;
    }
    assert_eq!(
        app.get_with_cookie("/api/auth/me", &mine).await.status(),
        StatusCode::OK
    );
    // The count survives a restart (it is stored with the session) …
    let restarted = app_with(app.dir, common::auth());
    restarted.clock.set(app.clock.now());
    let app = restarted;
    // … and the fifth wrong password ends this session, and only this one.
    let response = app.send(token_request(&mine, "test-password-wrong")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let cleared = set_cookie(&response).expect("cookie removed");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    assert_eq!(error_code(response).await, "session_ended");
    assert_eq!(
        app.get_with_cookie("/api/auth/me", &mine).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.get_with_cookie("/api/auth/me", &other).await.status(),
        StatusCode::OK
    );
    assert_eq!(app.login_with(password).await.status(), StatusCode::OK);
}

/// B3(d): failed logins and a running lock survive a restart;
/// `reset-password` (here: its library function) clears them.
#[tokio::test]
async fn r82_the_login_throttle_survives_a_restart() {
    let app = app().await;
    assert_eq!(
        fail_logins(&app, 3).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let now = app.clock.now();
    let app = app_with(app.dir, common::auth());
    app.clock.set(now);
    // Failures three and four were before the restart; the fifth locks.
    assert_eq!(
        fail_logins(&app, 1).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let response = fail_logins(&app, 1).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&response), 15 * 60);

    // Restarting does not lift the lock …
    let now = app.clock.now();
    let app = app_with(app.dir, common::auth());
    app.clock.set(now);
    app.advance(minutes(5));
    let response = app.login_with(common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&response), 10 * 60);
    // … nor the lock level: the second lock is 30 minutes.
    app.advance(minutes(10));
    let now = app.clock.now();
    let app = app_with(app.dir, common::auth());
    app.clock.set(now);
    assert_eq!(retry_after(&fail_logins(&app, 5).await), 30 * 60);

    // A successful login clears the stored state.
    app.advance(minutes(30));
    assert_eq!(
        app.login_with(common::PASSWORD).await.status(),
        StatusCode::OK
    );
    let state = |app: &App| -> String {
        rusqlite::Connection::open(app.dir.path().join("vocabulary.sqlite"))
            .expect("open")
            .query_row("SELECT state_json FROM auth_throttle", [], |r| r.get(0))
            .expect("state")
    };
    assert_eq!(LoginState::of(&state(&app)), LoginState::Clear);

    // reset-password forgets failed logins too.
    assert_eq!(retry_after(&fail_logins(&app, 5).await), 15 * 60);
    assert_eq!(LoginState::of(&state(&app)), LoginState::Locked);
    vocab_server::auth::reset_password(&app.db)
        .await
        .expect("reset");
    assert_eq!(state(&app), "{}");
    let now = app.clock.now();
    let app = app_with(app.dir, common::auth());
    app.clock.set(now);
    let response = app.setup(&app.code.to_string(), common::PASSWORD).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "setup right after the reset"
    );
    assert_eq!(
        app.login_with(common::PASSWORD).await.status(),
        StatusCode::OK
    );
}

/// What the stored throttle state says (without depending on its layout
/// beyond the two facts the tests need).
#[derive(Debug, PartialEq, Eq)]
enum LoginState {
    Clear,
    Locked,
    Counting,
}

impl LoginState {
    fn of(json: &str) -> Self {
        let state: Value = serde_json::from_str(json).expect("json");
        let empty = |key: &str| state[key].as_array().is_none_or(Vec::is_empty);
        if state["locked_until"].is_string() {
            Self::Locked
        } else if empty("failures") && empty("day_failures") {
            Self::Clear
        } else {
            Self::Counting
        }
    }
}

#[tokio::test]
async fn r82_the_lockout_doubling_starts_over_after_a_quiet_day() {
    let app = app().await;
    assert_eq!(retry_after(&fail_logins(&app, 5).await), 15 * 60);
    app.advance(minutes(15));
    assert_eq!(retry_after(&fail_logins(&app, 5).await), 30 * 60);
    // No failed or refused login for 24 hours: the next lockout is 15 minutes again.
    app.advance(days(1));
    assert_eq!(retry_after(&fail_logins(&app, 5).await), 15 * 60);
}

#[tokio::test]
async fn r82_failures_spread_over_more_than_15_minutes_do_not_lock() {
    let app = app().await;
    for _ in 0..4 {
        assert_eq!(
            fail_logins(&app, 1).await.status(),
            StatusCode::UNAUTHORIZED
        );
        app.advance(minutes(5));
    }
    // The first failure is now 20 minutes old: only 4 in the window.
    assert_eq!(
        fail_logins(&app, 1).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.login_with(common::PASSWORD).await.status(),
        StatusCode::OK
    );
}

// ── limits (§9.4) ──────────────────────────────────────────────────────────

#[tokio::test]
async fn r83_login_body_is_limited_to_2_kib() {
    let app = app().await;
    let fits = format!(r#"{{"password":"{}"}}"#, "x".repeat(2048 - 15));
    assert_eq!(fits.len(), 2048);
    let response = app
        .send(request(&Method::POST, "/api/auth/login", &fits, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let too_big = format!(r#"{{"password":"{}"}}"#, "x".repeat(2049 - 15));
    let response = app
        .send(request(&Method::POST, "/api/auth/login", &too_big, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn r83_paste_import_is_limited_to_30_requests_per_10_minutes() {
    let app = app().await;
    let cookie = app.login().await;
    let body = json!({ "text": "not a pack" }).to_string();
    let send =
        |uri: &'static str| app.send(request(&Method::POST, uri, &body, &[("cookie", &cookie)]));
    for i in 0..30 {
        let uri = if i % 2 == 0 {
            "/api/import/preview"
        } else {
            "/api/import/commit"
        };
        let response = send(uri).await;
        assert_ne!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "request {i}"
        );
    }
    for uri in ["/api/import/preview", "/api/import/commit"] {
        let response = send(uri).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS, "{uri}");
        assert_eq!(retry_after(&response), 600);
        assert_eq!(error_code(response).await, "rate_limited");
    }
    // Other routes are not affected.
    let response = app.get_with_cookie("/api/import/recent", &cookie).await;
    assert_eq!(response.status(), StatusCode::OK);
    app.advance(minutes(10));
    assert_ne!(
        send("/api/import/preview").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn r83_connector_is_limited_to_30_requests_per_10_minutes() {
    let app = app().await;
    let bearer = format!("Bearer {}", app.connector_token().await);
    let send = || {
        app.send(bare(
            &Method::GET,
            "/api/connector/v1/sources",
            "",
            &[("authorization", &bearer)],
        ))
    };
    for _ in 0..30 {
        assert_eq!(send().await.status(), StatusCode::OK);
    }
    assert_eq!(send().await.status(), StatusCode::TOO_MANY_REQUESTS);
    // Requests without the token do not use up the budget.
    app.advance(minutes(10));
    for _ in 0..40 {
        let response = app
            .send(bare(&Method::GET, "/api/connector/v1/sources", "", &[]))
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(send().await.status(), StatusCode::OK);
}

/// B4: while the app waits to be set up, the caller is asked to announce
/// the setup code again and again; that stops once a password is set.
#[tokio::test]
async fn r82_the_setup_code_is_announced_again_until_the_app_is_set_up() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let (auth, code) = common::auth();
    let app = app_with(
        tempfile::tempdir().expect("tempdir"),
        (Arc::clone(&auth), code),
    );
    let announced = Arc::new(AtomicU32::new(0));
    let reminder = {
        let (auth, db, announced) = (Arc::clone(&auth), app.db.clone(), Arc::clone(&announced));
        tokio::spawn(async move {
            auth.remind_setup(&db, std::time::Duration::from_millis(40), || {
                announced.fetch_add(1, Ordering::Relaxed);
            })
            .await;
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let before_setup = announced.load(Ordering::Relaxed);
    assert!(before_setup >= 3, "announced {before_setup} times");
    assert!(!reminder.is_finished());

    let response = app.setup(&app.code.to_string(), common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::timeout(std::time::Duration::from_secs(2), reminder)
        .await
        .expect("the reminder ends once the app is set up")
        .expect("join");
    let after_setup = announced.load(Ordering::Relaxed);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        announced.load(Ordering::Relaxed),
        after_setup,
        "no more reminders"
    );
}
