//! Contract tests for authentication and request protection, written from
//! the requirements (R-82, R-83; design §9) and not from the implementation:
//! every route's protection, the session cookie, sliding renewal, logout,
//! password rotation, the login throttle, the CSRF guard, the import token,
//! rate and body limits, security headers and the start-up refusals.
//! Secrets are synthetic (`test-…`).

mod common;

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use serde_json::{Value, json};
use tower::ServiceExt;
use vocab_server::auth::{Auth, Credentials, hash_password};
use vocab_server::clock::{Clock, ManualClock};
use vocab_server::config::Limits;
use vocab_server::db::Db;
use vocab_server::http::{self, Access, AppState, RouteInfo};

const COOKIE_NAME: &str = "__Host-vocab_session";
const MAX_AGE: &str = "Max-Age=15552000";
const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

struct App {
    dir: tempfile::TempDir,
    clock: Arc<ManualClock>,
    router: axum::Router,
}

fn app_with(dir: tempfile::TempDir, auth: Auth) -> App {
    let db = Db::open(dir.path()).expect("db");
    let clock = Arc::new(ManualClock::new(common::ts(common::START)));
    let state = AppState::with_clock(
        db,
        Arc::clone(&clock) as Arc<dyn Clock>,
        common::prague(),
        auth,
    );
    App {
        dir,
        clock,
        router: http::app(state, &Limits::default()),
    }
}

fn app() -> App {
    app_with(tempfile::tempdir().expect("tempdir"), common::auth())
}

/// A request with exactly `headers`.
fn req(method: &Method, uri: &str, body: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method(method.clone()).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::from(body.to_owned())).expect("request")
}

/// A state-changing request as the app's page sends it: right Origin, JSON.
fn page(method: &Method, uri: &str, body: &str, extra: &[(&str, &str)]) -> Request<Body> {
    let mut headers = vec![
        ("origin", common::ORIGIN),
        ("content-type", "application/json"),
    ];
    headers.extend_from_slice(extra);
    req(method, uri, body, &headers)
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

    async fn login_with(&self, password: &str) -> Response {
        let body = json!({ "password": password }).to_string();
        self.send(page(&Method::POST, "/api/auth/login", &body, &[]))
            .await
    }

    /// Log in with the right password; returns the `name=value` pair.
    async fn login(&self) -> String {
        let response = self.login_with(common::PASSWORD).await;
        assert_eq!(response.status(), StatusCode::OK);
        cookie_pair(&response).expect("session cookie")
    }

    async fn get(&self, uri: &str, cookie: &str) -> Response {
        self.send(req(&Method::GET, uri, "", &[("cookie", cookie)]))
            .await
    }

    async fn fail_logins(&self, n: usize) -> Response {
        let mut last = None;
        for _ in 0..n {
            last = Some(self.login_with("test-password-wrong").await);
        }
        last.expect("at least one attempt")
    }

    fn sessions_in_db(&self) -> i64 {
        rusqlite::Connection::open(self.dir.path().join("vocabulary.sqlite"))
            .expect("open")
            .query_row("SELECT COUNT(*) FROM auth_session", [], |r| r.get(0))
            .expect("count")
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

fn retry_after(response: &Response) -> u64 {
    response
        .headers()
        .get(header::RETRY_AFTER)
        .expect("Retry-After on a refusal")
        .to_str()
        .expect("ascii")
        .parse()
        .expect("seconds")
}

async fn json_body(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 4 * MIB).await.expect("body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// A concrete URI for a route template.
fn concrete(path: &str) -> String {
    path.replace("{id}", "1")
        .replace("{build}", "0")
        .replace("{*path}", "js/main.js")
}

fn days(n: i64) -> jiff::SignedDuration {
    jiff::SignedDuration::from_hours(24 * n)
}

fn minutes(n: i64) -> jiff::SignedDuration {
    jiff::SignedDuration::from_mins(n)
}

fn with_access(access: Access) -> Vec<RouteInfo> {
    http::routes()
        .into_iter()
        .filter(|r| r.access == access)
        .collect()
}

/// Routes a browser changes state through (CSRF guard applies).
fn state_changing_routes() -> Vec<RouteInfo> {
    http::routes()
        .into_iter()
        .filter(|r| r.method != Method::GET && r.access != Access::ImportToken)
        .collect()
}

// ── every route needs a session ────────────────────────────────────────────

#[tokio::test]
async fn r82_every_non_public_route_refuses_requests_without_credentials() {
    let app = app();
    let stale = format!("{COOKIE_NAME}={}", "cd".repeat(32));
    let mut session_routes = 0;
    for route in http::routes() {
        let uri = concrete(route.path);
        match route.access {
            Access::Public => continue,
            Access::Session => {
                session_routes += 1;
                for headers in [
                    vec![],
                    vec![("cookie", stale.as_str())],
                    vec![("cookie", "")],
                ] {
                    let mut all = vec![
                        ("origin", common::ORIGIN),
                        ("content-type", "application/json"),
                    ];
                    all.extend(headers.iter().copied());
                    let response = app.send(req(&route.method, &uri, "{}", &all)).await;
                    assert_eq!(
                        response.status(),
                        StatusCode::UNAUTHORIZED,
                        "{} {uri} with {headers:?}",
                        route.method
                    );
                    assert!(set_cookie(&response).is_none());
                }
            }
            Access::ImportToken => {
                let response = app.send(req(&route.method, &uri, "{}", &[])).await;
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "{} {uri}",
                    route.method
                );
            }
        }
    }
    assert!(session_routes > 0);
    assert_eq!(app.sessions_in_db(), 0);
}

#[tokio::test]
async fn r82_a_valid_session_passes_the_gate_of_every_session_route() {
    let app = app();
    let cookie = app.login().await;
    for route in with_access(Access::Session) {
        if route.path == "/api/auth/logout" {
            continue;
        }
        let uri = concrete(route.path);
        let response = app
            .send(page(&route.method, &uri, "{}", &[("cookie", &cookie)]))
            .await;
        let status = response.status();
        assert!(
            status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
            "{} {uri}: {status}",
            route.method
        );
    }
}

#[tokio::test]
async fn r82_only_the_login_and_what_it_needs_are_public() {
    for route in with_access(Access::Public) {
        let api = route.path.starts_with("/api/");
        assert!(
            !api || route.path == "/api/auth/login",
            "{} {} must not be public",
            route.method,
            route.path
        );
    }
    let app = app();
    for uri in ["/healthz", "/"] {
        let response = app.send(req(&Method::GET, uri, "", &[])).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
}

// ── login, cookie, logout ──────────────────────────────────────────────────

#[tokio::test]
async fn r82_login_with_the_right_password_opens_a_session() {
    let app = app();
    let response = app.login_with(common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = cookie_pair(&response).expect("cookie");
    assert_eq!(
        app.get("/api/auth/me", &cookie).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn r82_the_session_cookie_is_host_prefixed_with_the_hardened_attributes() {
    let app = app();
    let response = app.login_with(common::PASSWORD).await;
    let cookie = set_cookie(&response).expect("Set-Cookie");
    let mut parts = cookie.split("; ");
    let (name, value) = parts
        .next()
        .and_then(|pair| pair.split_once('='))
        .expect("pair");
    assert_eq!(name, COOKIE_NAME);
    assert!(name.starts_with("__Host-"));
    assert_eq!(value.len(), 64, "32 random bytes as hex");
    assert!(value.bytes().all(|b| b.is_ascii_hexdigit()));
    let attributes: Vec<&str> = parts.collect();
    for expected in ["Path=/", "Secure", "HttpOnly", "SameSite=Strict", MAX_AGE] {
        assert!(attributes.contains(&expected), "{expected} in {cookie}");
    }
    assert_eq!(
        attributes.len(),
        5,
        "no Domain or other attribute: {cookie}"
    );
    // Random per login.
    assert_ne!(app.login().await, format!("{name}={value}"));
}

#[tokio::test]
async fn r82_wrong_passwords_are_refused_without_a_cookie() {
    let app = app();
    for password in [
        "test-password-wrong",
        "",
        "x",
        &common::PASSWORD.to_uppercase(),
    ] {
        let response = app.login_with(password).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{password:?}");
        assert!(set_cookie(&response).is_none());
    }
    assert_eq!(app.sessions_in_db(), 0);
}

#[tokio::test]
async fn r82_the_session_slides_when_older_than_7_days_and_ends_after_180() {
    let app = app();
    let cookie = app.login().await;
    app.advance(days(6));
    let response = app.get("/api/today", &cookie).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(set_cookie(&response).is_none(), "younger than 7 days");

    app.advance(days(2));
    let response = app.get("/api/today", &cookie).await;
    assert_eq!(response.status(), StatusCode::OK);
    let renewed = set_cookie(&response).expect("renewed cookie");
    assert!(renewed.contains(&cookie), "{renewed}");
    assert!(renewed.contains(MAX_AGE), "{renewed}");

    // 179 days after the renewal it is still valid ...
    app.advance(days(179));
    assert_eq!(
        app.get("/api/today", &cookie).await.status(),
        StatusCode::OK
    );
    // ... and a session nobody uses for 180 days is gone.
    let idle = app.login().await;
    app.advance(days(181));
    assert_eq!(
        app.get("/api/today", &idle).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn r82_logout_revokes_only_its_own_session() {
    let app = app();
    let first = app.login().await;
    let second = app.login().await;
    let response = app
        .send(page(
            &Method::POST,
            "/api/auth/logout",
            "{}",
            &[("cookie", &first)],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cleared = set_cookie(&response).expect("cookie is cleared");
    assert!(
        cleared.starts_with(&format!("{COOKIE_NAME}=;")),
        "{cleared}"
    );
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    assert_eq!(
        app.get("/api/today", &first).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.get("/api/today", &second).await.status(),
        StatusCode::OK
    );
    assert_eq!(app.sessions_in_db(), 1, "revoked in the database too");
}

#[tokio::test]
async fn r82_changing_the_configured_password_hash_invalidates_existing_sessions() {
    let first = app();
    let cookie = first.login().await;
    // Same password, new salt: a different hash string is a rotation too.
    let rehash = hash_password(common::PASSWORD).expect("hash");
    assert_ne!(
        rehash,
        common::password_hash_b64(),
        "salted hashes differ per call"
    );
    let rotated = Auth::new(
        Credentials::new(&rehash, common::IMPORT_TOKEN).expect("credentials"),
        common::ORIGIN,
        180,
    );
    let second = app_with(first.dir, rotated);
    assert_eq!(
        second.get("/api/today", &cookie).await.status(),
        StatusCode::UNAUTHORIZED
    );
    // A restart with the unchanged hash keeps sessions.
    let fresh = second.login().await;
    let third = app_with(second.dir, {
        Auth::new(
            Credentials::new(&rehash, common::IMPORT_TOKEN).expect("credentials"),
            common::ORIGIN,
            180,
        )
    });
    assert_eq!(
        third.get("/api/today", &fresh).await.status(),
        StatusCode::OK
    );
}

// ── login throttle ─────────────────────────────────────────────────────────

#[tokio::test]
async fn r82_the_fifth_failure_within_15_minutes_locks_logins() {
    let app = app();
    assert_eq!(
        app.fail_logins(4).await.status(),
        StatusCode::UNAUTHORIZED,
        "four failures do not lock"
    );
    let response = app.fail_logins(1).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after(&response), 15 * 60);
    // The right password does not help during the lockout, and creates no session.
    let response = app.login_with(common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(set_cookie(&response).is_none());
    app.advance(minutes(15));
    assert_eq!(
        app.login_with(common::PASSWORD).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn r82_failures_older_than_15_minutes_do_not_count() {
    let app = app();
    for _ in 0..4 {
        assert_eq!(app.fail_logins(1).await.status(), StatusCode::UNAUTHORIZED);
        app.advance(minutes(5));
    }
    assert_eq!(app.fail_logins(1).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn r82_lockouts_double_from_15_minutes_up_to_24_hours_while_attempts_continue() {
    let app = app();
    let expected_minutes: [i64; 10] = [15, 30, 60, 120, 240, 480, 960, 1440, 1440, 1440];
    for expected in expected_minutes {
        let response = app.fail_logins(5).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            retry_after(&response),
            expected.cast_unsigned() * 60,
            "lockout {expected} min"
        );
        // Just before the end: still locked; at the end: attempts are allowed again.
        // That refused attempt also keeps the doubling level: it only starts
        // over after 24 hours without any failed or refused login.
        app.advance(minutes(expected - 1));
        assert_eq!(
            app.login_with("test-password-wrong").await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        app.advance(minutes(1));
    }
}

#[tokio::test]
async fn r82_a_day_without_failed_logins_resets_the_lockout_to_15_minutes() {
    let app = app();
    for expected in [15, 30, 60] {
        let response = app.fail_logins(5).await;
        assert_eq!(retry_after(&response), expected * 60);
        app.advance(minutes(expected.cast_signed()));
    }
    app.advance(minutes(24 * 60));
    let response = app.fail_logins(5).await;
    assert_eq!(retry_after(&response), 15 * 60);
}

#[tokio::test]
async fn r82_a_successful_login_resets_the_throttle_and_sessions_survive_lockouts() {
    let app = app();
    let existing = app.login().await;
    for _ in 0..2 {
        assert_eq!(
            app.fail_logins(5).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        // Existing sessions keep working during the lockout.
        assert_eq!(
            app.get("/api/today", &existing).await.status(),
            StatusCode::OK
        );
        app.advance(minutes(30));
    }
    assert_eq!(
        app.login_with(common::PASSWORD).await.status(),
        StatusCode::OK
    );
    let response = app.fail_logins(5).await;
    assert_eq!(retry_after(&response), 15 * 60, "back to the first step");
    // Failures before a success do not carry over either.
    app.advance(minutes(15));
    app.fail_logins(4).await;
    assert_eq!(
        app.login_with(common::PASSWORD).await.status(),
        StatusCode::OK
    );
    assert_eq!(app.fail_logins(4).await.status(), StatusCode::UNAUTHORIZED);
}

// ── CSRF: Origin and content type ──────────────────────────────────────────

#[tokio::test]
async fn r82_every_state_changing_request_needs_the_exact_public_origin() {
    let app = app();
    let cookie = app.login().await;
    let routes = state_changing_routes();
    assert!(routes.iter().any(|r| r.path == "/api/auth/login"));
    assert!(routes.iter().any(|r| r.path == "/api/auth/logout"));
    assert!(routes.iter().any(|r| r.path == "/api/import/commit"));
    let bad: [Option<&str>; 9] = [
        None,
        Some("null"),
        Some(""),
        Some("https://other.example.home"),
        Some("https://sub.vocab.example.home"),
        Some("https://vocab.example.home.evil.example"),
        Some("http://vocab.example.home"),
        Some("https://vocab.example.home:8443"),
        Some("https://vocab.example.home/"),
    ];
    for route in &routes {
        let uri = concrete(route.path);
        for origin in bad {
            let mut headers = vec![
                ("cookie", cookie.as_str()),
                ("content-type", "application/json"),
            ];
            if let Some(origin) = origin {
                headers.push(("origin", origin));
            }
            let response = app.send(req(&route.method, &uri, "{}", &headers)).await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{} {uri} from {origin:?}",
                route.method
            );
        }
    }
    assert_eq!(app.sessions_in_db(), 1, "no login happened");
    // A Referer is no substitute for the Origin.
    let response = app
        .send(req(
            &Method::PATCH,
            "/api/settings",
            "{}",
            &[
                ("cookie", &cookie),
                ("content-type", "application/json"),
                ("referer", "https://vocab.example.home/"),
            ],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    // The right origin works.
    let response = app
        .send(page(
            &Method::PATCH,
            "/api/settings",
            "{}",
            &[("cookie", &cookie)],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn r82_reading_does_not_need_an_origin() {
    let app = app();
    let cookie = app.login().await;
    assert_eq!(
        app.get("/api/today", &cookie).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn r82_state_changing_requests_need_a_json_content_type() {
    let app = app();
    let cookie = app.login().await;
    for route in state_changing_routes() {
        let uri = concrete(route.path);
        for content_type in [
            Some("application/x-www-form-urlencoded"),
            Some("multipart/form-data; boundary=x"),
            Some("text/plain"),
            None,
        ] {
            let mut headers = vec![("cookie", cookie.as_str()), ("origin", common::ORIGIN)];
            if let Some(content_type) = content_type {
                headers.push(("content-type", content_type));
            }
            let response = app.send(req(&route.method, &uri, "{}", &headers)).await;
            assert_eq!(
                response.status(),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "{} {uri} as {content_type:?}",
                route.method
            );
        }
    }
}

// ── import token ───────────────────────────────────────────────────────────

#[tokio::test]
async fn r82_the_import_token_opens_connector_routes_and_nothing_else() {
    let app = app();
    let bearer = format!("Bearer {}", common::IMPORT_TOKEN);
    let connector = with_access(Access::ImportToken);
    assert!(!connector.is_empty());
    for route in &connector {
        assert!(
            route.path.starts_with("/api/connector/v1/"),
            "{}",
            route.path
        );
    }
    let response = app
        .send(req(
            &Method::GET,
            "/api/connector/v1/sources",
            "",
            &[("authorization", &bearer)],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    // The token is accepted nowhere else: every other route says 401.
    for route in http::routes() {
        if route.access == Access::Public || route.access == Access::ImportToken {
            continue;
        }
        let uri = concrete(route.path);
        let response = app
            .send(page(
                &route.method,
                &uri,
                "{}",
                &[("authorization", &bearer)],
            ))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{} {uri}",
            route.method
        );
    }
}

#[tokio::test]
async fn r82_connector_routes_do_not_accept_a_session_cookie() {
    let app = app();
    let cookie = app.login().await;
    for route in with_access(Access::ImportToken) {
        let uri = concrete(route.path);
        let response = app
            .send(page(&route.method, &uri, "{}", &[("cookie", &cookie)]))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{} {uri}",
            route.method
        );
    }
}

#[tokio::test]
async fn r82_wrong_or_short_import_tokens_are_refused() {
    let app = app();
    let short = &common::IMPORT_TOKEN[..31];
    let longer = format!("{}x", common::IMPORT_TOKEN);
    let upper = common::IMPORT_TOKEN.to_uppercase();
    let values = [
        format!("Bearer {short}"),
        format!("Bearer {longer}"),
        format!("Bearer {upper}"),
        "Bearer ".to_owned(),
        "Bearer".to_owned(),
        format!("Basic {}", common::IMPORT_TOKEN),
        common::IMPORT_TOKEN.to_owned(),
    ];
    for value in &values {
        let response = app
            .send(req(
                &Method::GET,
                "/api/connector/v1/sources",
                "",
                &[("authorization", value)],
            ))
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{value:?}");
    }
    // Nor does the token in the query string or in a cookie.
    let uri = format!("/api/connector/v1/sources?token={}", common::IMPORT_TOKEN);
    let response = app.send(req(&Method::GET, &uri, "", &[])).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn r82_the_app_does_not_start_without_a_password_hash_or_with_a_short_import_token() {
    let token = common::IMPORT_TOKEN;
    let hash = common::password_hash_b64();
    assert!(Credentials::new(hash, token).is_ok());
    // A 32-character token is the minimum.
    let exactly_32 = "t".repeat(32);
    assert_eq!(exactly_32.len(), 32);
    assert!(Credentials::new(hash, &exactly_32).is_ok());
    for short in ["", "t", &"t".repeat(31)] {
        assert!(
            Credentials::new(hash, short).is_err(),
            "{} chars",
            short.len()
        );
    }
    for bad_hash in ["", "   ", "dGVzdC1ub3QtYS1oYXNo", "not base64 at all!"] {
        assert!(Credentials::new(bad_hash, token).is_err(), "{bad_hash:?}");
    }
    // A plaintext password is no hash.
    assert!(Credentials::new(common::PASSWORD, token).is_err());
}

// ── rate limits ────────────────────────────────────────────────────────────

#[tokio::test]
async fn r83_paste_import_allows_30_requests_per_10_minutes() {
    let app = app();
    let cookie = app.login().await;
    let body = json!({ "text": "not a pack" }).to_string();
    let send =
        |uri: &'static str| app.send(page(&Method::POST, uri, &body, &[("cookie", &cookie)]));
    for i in 0..30 {
        let uri = ["/api/import/preview", "/api/import/commit"][i % 2];
        assert_ne!(
            send(uri).await.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "{i}"
        );
    }
    let response = send("/api/import/preview").await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(retry_after(&response) <= 600);
    assert_eq!(
        send("/api/import/commit").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    app.advance(minutes(10));
    assert_ne!(
        send("/api/import/preview").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn r83_connector_allows_30_requests_per_10_minutes() {
    let app = app();
    let bearer = format!("Bearer {}", common::IMPORT_TOKEN);
    let send = || {
        app.send(req(
            &Method::GET,
            "/api/connector/v1/sources",
            "",
            &[("authorization", &bearer)],
        ))
    };
    for i in 0..30 {
        assert_eq!(send().await.status(), StatusCode::OK, "{i}");
    }
    let response = send().await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(retry_after(&response) <= 600);
    app.advance(minutes(10));
    assert_eq!(send().await.status(), StatusCode::OK);
}

// ── body limits ────────────────────────────────────────────────────────────

/// A JSON object `{"<key>":"xxx…"}` of exactly `total` bytes.
fn json_of_size(key: &str, total: usize) -> String {
    let overhead = format!(r#"{{"{key}":""}}"#).len();
    format!(r#"{{"{key}":"{}"}}"#, "x".repeat(total - overhead))
}

#[tokio::test]
async fn r83_login_bodies_are_limited_to_2_kib() {
    let app = app();
    let fits = json_of_size("password", 2 * KIB);
    let response = app
        .send(page(&Method::POST, "/api/auth/login", &fits, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "2 KiB is read");
    let too_big = json_of_size("password", 2 * KIB + 1);
    let response = app
        .send(page(&Method::POST, "/api/auth/login", &too_big, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn r83_other_bodies_are_limited_to_64_kib() {
    let app = app();
    let cookie = app.login().await;
    let padded = |total: usize| format!("{{}}{}", " ".repeat(total - 2));
    let response = app
        .send(page(
            &Method::PATCH,
            "/api/settings",
            &padded(64 * KIB),
            &[("cookie", &cookie)],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK, "64 KiB is read");
    let response = app
        .send(page(
            &Method::PATCH,
            "/api/settings",
            &padded(64 * KIB + 1),
            &[("cookie", &cookie)],
        ))
        .await;
    // Every body limit answers 413, like the import routes do.
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn r83_import_accepts_a_2_mib_paste_and_refuses_much_more() {
    let app = app();
    let cookie = app.login().await;
    // 2 MiB of text is read (and then rejected as not being a pack).
    let fits = json_of_size("text", 2 * MIB);
    let response = app
        .send(page(
            &Method::POST,
            "/api/import/preview",
            &fits,
            &[("cookie", &cookie)],
        ))
        .await;
    assert_ne!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    // Text above 2 MiB is refused (413, or 422 from the paste check itself).
    let over = json_of_size("text", 2 * MIB + 2 * KIB);
    let response = app
        .send(page(
            &Method::POST,
            "/api/import/preview",
            &over,
            &[("cookie", &cookie)],
        ))
        .await;
    assert!(
        response.status() == StatusCode::PAYLOAD_TOO_LARGE
            || response.status() == StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        response.status()
    );
    // A body several times larger is refused before it is read in full.
    let huge = json_of_size("text", 8 * MIB);
    let response = app
        .send(page(
            &Method::POST,
            "/api/import/commit",
            &huge,
            &[("cookie", &cookie)],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

// ── security headers ───────────────────────────────────────────────────────

fn assert_security_headers(response: &Response, what: &str) {
    let get = |name: &str| {
        response
            .headers()
            .get(name)
            .unwrap_or_else(|| panic!("{what}: missing {name}"))
            .to_str()
            .expect("ascii")
            .to_owned()
    };
    assert_eq!(
        get("content-security-policy"),
        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
         media-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; \
         frame-ancestors 'none'; form-action 'self'",
        "{what}"
    );
    assert_eq!(get("x-content-type-options"), "nosniff", "{what}");
    assert_eq!(get("referrer-policy"), "same-origin", "{what}");
    assert_eq!(get("cross-origin-opener-policy"), "same-origin", "{what}");
    assert_eq!(
        get("permissions-policy"),
        "camera=(), microphone=(), geolocation=()",
        "{what}"
    );
    assert_eq!(
        get("strict-transport-security"),
        "max-age=31536000",
        "{what}"
    );
}

#[tokio::test]
async fn r83_security_headers_are_on_every_kind_of_response() {
    let app = app();
    let cookie = app.login().await;
    let bearer = format!("Bearer {}", common::IMPORT_TOKEN);
    let cases: Vec<(&str, Request<Body>, StatusCode)> = vec![
        ("shell", req(&Method::GET, "/", "", &[]), StatusCode::OK),
        (
            "health",
            req(&Method::GET, "/healthz", "", &[]),
            StatusCode::OK,
        ),
        (
            "api 200",
            req(&Method::GET, "/api/today", "", &[("cookie", &cookie)]),
            StatusCode::OK,
        ),
        (
            "connector 200",
            req(
                &Method::GET,
                "/api/connector/v1/sources",
                "",
                &[("authorization", &bearer)],
            ),
            StatusCode::OK,
        ),
        (
            "401",
            req(&Method::GET, "/api/today", "", &[]),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "401 wrong password",
            page(
                &Method::POST,
                "/api/auth/login",
                r#"{"password":"test-wrong"}"#,
                &[],
            ),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "403",
            req(
                &Method::POST,
                "/api/auth/logout",
                "{}",
                &[("cookie", &cookie), ("content-type", "application/json")],
            ),
            StatusCode::FORBIDDEN,
        ),
        (
            "404 logged out",
            req(&Method::GET, "/no/such/path", "", &[]),
            StatusCode::NOT_FOUND,
        ),
        (
            "404 logged in",
            req(
                &Method::GET,
                "/api/no-such-route",
                "",
                &[("cookie", &cookie)],
            ),
            StatusCode::NOT_FOUND,
        ),
        (
            "413",
            page(
                &Method::POST,
                "/api/auth/login",
                &json_of_size("password", 4 * KIB),
                &[],
            ),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        (
            "415",
            req(
                &Method::PATCH,
                "/api/settings",
                "{}",
                &[("cookie", &cookie), ("origin", common::ORIGIN)],
            ),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
    ];
    for (what, request, status) in cases {
        let response = app.send(request).await;
        assert_eq!(response.status(), status, "{what}");
        assert_security_headers(&response, what);
    }
}

#[tokio::test]
async fn r83_security_headers_are_on_rate_limit_and_lockout_refusals() {
    let app = app();
    let response = app.fail_logins(5).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_security_headers(&response, "lockout 429");
    let bearer = format!("Bearer {}", common::IMPORT_TOKEN);
    let mut last = None;
    for _ in 0..31 {
        last = Some(
            app.send(req(
                &Method::GET,
                "/api/connector/v1/sources",
                "",
                &[("authorization", &bearer)],
            ))
            .await,
        );
    }
    let response = last.expect("responses");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_security_headers(&response, "connector 429");
    // Unused here but keeps the body readable for the error shape.
    assert!(json_body(response).await.is_object());
}
