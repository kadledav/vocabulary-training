//! Authentication and request protection (design §9; R-82, R-83) through the
//! real router: sessions, cookie attributes, password rotation, the import
//! token's scope, the CSRF guard, the login throttle and the per-endpoint
//! limits. Secrets are synthetic (`test-…`).

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

const START: &str = "2026-11-20T09:00:00Z";
const COOKIE: &str = "__Host-vocab_session";

struct App {
    dir: tempfile::TempDir,
    clock: Arc<ManualClock>,
    router: axum::Router,
}

fn app_with(dir: tempfile::TempDir, auth: Auth) -> App {
    let db = Db::open(dir.path()).expect("db");
    let clock = Arc::new(ManualClock::new(START.parse().expect("ts")));
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
fn r82_only_health_login_and_the_shell_are_public() {
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
            "{method} {path}: the import token is for connector routes only"
        );
        if path.starts_with("/api/") && *path != "/api/auth/login" {
            assert_ne!(*access, Access::Public, "{method} {path}");
        }
    }
    assert!(routes.iter().any(|r| r.access == Access::ImportToken));
}

#[tokio::test]
async fn r82_every_session_route_answers_401_without_a_valid_session() {
    let app = app();
    let stale = format!("{COOKIE}={}", "ab".repeat(32));
    let bearer = format!("Bearer {}", common::IMPORT_TOKEN);
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
    let app = app();
    for uri in ["/healthz", "/"] {
        let response = app.send(request(&Method::GET, uri, "", &[])).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
}

// ── login, cookie, logout ──────────────────────────────────────────────────

#[tokio::test]
async fn r82_login_sets_a_host_only_secure_http_only_strict_cookie() {
    let app = app();
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
    let app = app();
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
    let app = app();
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
    let app = app();
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
    let first = app();
    let cookie = first.login().await;
    let restarted = app_with(first.dir, common::auth());
    let response = restarted.get_with_cookie("/api/today", &cookie).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn r82_a_new_password_ends_every_session() {
    let first = app();
    let cookie = first.login().await;
    // The owner sets a new hash in .env and restarts.
    let new_password = "test-password-after-rotation";
    let hash = hash_password(new_password).expect("hash");
    let rotated = Auth::new(
        Credentials::new(&hash, common::IMPORT_TOKEN).expect("credentials"),
        common::ORIGIN,
        180,
    );
    let app = app_with(first.dir, rotated);
    let response = app.get_with_cookie("/api/today", &cookie).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app.login_with(common::PASSWORD).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "old password");
    let response = app.login_with(new_password).await;
    assert_eq!(response.status(), StatusCode::OK);
    let new_cookie = cookie_pair(&response).expect("cookie");
    assert_eq!(
        app.get_with_cookie("/api/today", &new_cookie)
            .await
            .status(),
        StatusCode::OK
    );
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
    let app = app();
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
    let app = app();
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

// ── import token (§9.2) ────────────────────────────────────────────────────

#[tokio::test]
async fn r82_the_import_token_works_only_on_connector_routes() {
    let app = app();
    let cookie = app.login().await;
    let bearer = format!("Bearer {}", common::IMPORT_TOKEN);
    let connector = "/api/connector/v1/sources";

    for scheme in ["Bearer", "bearer"] {
        let value = format!("{scheme} {}", common::IMPORT_TOKEN);
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
    let wrong = format!("Bearer {}x", common::IMPORT_TOKEN);
    for headers in [
        vec![("cookie", cookie.as_str())],
        vec![("authorization", wrong.as_str())],
        vec![("authorization", common::IMPORT_TOKEN)],
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
    let app = app();
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

#[tokio::test]
async fn r82_the_lockout_doubling_starts_over_after_a_quiet_day() {
    let app = app();
    assert_eq!(retry_after(&fail_logins(&app, 5).await), 15 * 60);
    app.advance(minutes(15));
    assert_eq!(retry_after(&fail_logins(&app, 5).await), 30 * 60);
    // No failed or refused login for 24 hours: the next lockout is 15 minutes again.
    app.advance(days(1));
    assert_eq!(retry_after(&fail_logins(&app, 5).await), 15 * 60);
}

#[tokio::test]
async fn r82_failures_spread_over_more_than_15_minutes_do_not_lock() {
    let app = app();
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
    let app = app();
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
    let app = app();
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
    let app = app();
    let bearer = format!("Bearer {}", common::IMPORT_TOKEN);
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
