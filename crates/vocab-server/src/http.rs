//! HTTP router and cross-cutting request handling (design §3.2, §8, §9.4).
//!
//! Every response carries the security headers (strict CSP and friends); every
//! request gets a body size limit, a timeout and a slot in a bounded
//! concurrency pool, and is logged with method, path template, status and
//! duration only (never bodies or query values, R-24). Handlers stay thin:
//! parse, call a module, map errors.
//!
//! Body limits and timeouts are set per route group: the default (64 KiB,
//! 30 s) for everything, larger ones for `/api/import/*` (2 MiB; the commit
//! may take 120 s). A timeout only stops waiting: database work that already
//! started finishes in the background (see `workflows`), so the timeout
//! answer of an import tells the owner that importing again is safe.
//!
//! Access control (design §9) is one layer over every route: each route of
//! the table in [`routes`] is public, needs a session, or needs the import
//! token; anything else needs a session. State-changing requests must pass
//! the CSRF guard (Origin + JSON). The `auth` module does the checks.
//!
//! [`serve`] owns the connection level: a header read timeout (which also
//! closes idle keep-alive connections) and a cap on open connections, so a
//! client that trickles bytes cannot hold sockets forever (slowloris).

use std::collections::HashMap;
use std::future::Future;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, MatchedPath, Path, Query, Request, State};
use axum::handler::Handler;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter, on};
use axum::{Json, Router};
use http_body_util::BodyExt;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use jiff::SignedDuration;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::auth::{Auth, AuthError, CsrfRejection, SessionCheck};
use crate::catalog::{self, SourceId, WordId, WordQuery};
use crate::clock::{Clock, DayRule, Moment, SystemClock};
use crate::config::Limits;
use crate::db::{Db, DbError};
use crate::importer::Via;
use crate::rate_limit::RateLimit;
use crate::scheduler::{AnswerMatch, RawGrade};
use crate::session::{self, RateInput, SessionError, SessionId, StartKind, StartRequest};
use crate::settings::{self, SettingsError, SettingsPatch};
use crate::stats::{self, StatsError};
use crate::web_assets;
use crate::workflows::{self, WorkflowError};

const HEALTHZ_PATH: &str = "/healthz";

const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
     img-src 'self' data:; media-src 'self'; connect-src 'self'; object-src 'none'; \
     base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

/// Header names are lowercase, as `HeaderName::from_static` requires.
const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("content-security-policy", CONTENT_SECURITY_POLICY),
    ("x-content-type-options", "nosniff"),
    // Not `no-referrer`: under it Firefox sends `Origin: null` on same-origin
    // POSTs, which the CSRF guard (§9.3) must refuse. Nothing leaves the site.
    ("referrer-policy", "same-origin"),
    ("cross-origin-opener-policy", "same-origin"),
    (
        "permissions-policy",
        "camera=(), microphone=(), geolocation=()",
    ),
    // This host only: no includeSubDomains, other *.home services are not ours.
    ("strict-transport-security", "max-age=31536000"),
];

/// Answer of a timed-out commit. The commit runs in its own task
/// (`workflows::commit_import`), so it does finish; repeating it is safe
/// because every pack is imported at most once.
const IMPORT_TIMEOUT_MESSAGE: &str = "The import is taking long and is still being imported \
     in the background; re-pasting the same text is safe (packs that were already imported \
     are skipped).";
/// Answer of a timed-out preview (which never stores anything).
const PREVIEW_TIMEOUT_MESSAGE: &str =
    "The preview took too long; nothing was stored. Try again or preview fewer packs.";
/// An oversized import body is read and discarded up to this many times
/// the limit before answering 413, so the client gets the JSON error rather
/// than a connection reset; anything larger is cut off.
const DRAIN_FACTOR: usize = 4;
const TIMEOUT_MESSAGE: &str = "The request took too long";

/// Body limit of login, setup and password change (§9.4).
const LOGIN_BODY_BYTES: usize = 2 * 1024;
/// Body limit of the connector routes (§9.4).
const CONNECTOR_BODY_BYTES: usize = 1024 * 1024;
/// Paste import (preview and commit together) and connector: at most this
/// many requests per [`RATE_WINDOW`] (§9.4).
const IMPORT_RATE_MAX: usize = 30;
const CONNECTOR_RATE_MAX: usize = 30;
const RATE_WINDOW: SignedDuration = SignedDuration::from_mins(10);

/// Most rows of the import log returned by `GET /api/import/recent`.
const RECENT_IMPORTS: u32 = 20;

/// What the handlers need.
#[derive(Debug, Clone)]
pub struct AppState {
    db: Db,
    clock: Arc<dyn Clock>,
    /// Where study days begin (config `timezone`, `day_rollover_hour`).
    day_rule: DayRule,
    /// Password, setup, sessions, connector token, CSRF guard.
    auth: Arc<Auth>,
    /// Body limit of the import routes (set by [`app`] from the limits).
    import_body_bytes: usize,
}

impl AppState {
    pub fn new(db: Db, day_rule: DayRule, auth: Arc<Auth>) -> Self {
        Self::with_clock(db, Arc::new(SystemClock), day_rule, auth)
    }

    /// With a controllable clock (tests).
    pub fn with_clock(db: Db, clock: Arc<dyn Clock>, day_rule: DayRule, auth: Arc<Auth>) -> Self {
        Self {
            db,
            clock,
            day_rule,
            auth,
            import_body_bytes: Limits::default().import_body_bytes,
        }
    }

    /// Now, with its study day.
    fn moment(&self) -> Moment {
        Moment::of(self.clock.now(), &self.day_rule)
    }
}

// ── routes ─────────────────────────────────────────────────────────────────

/// Who may call a route (design §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Anyone: health check, setup, login, and the shell needed to render
    /// the login and setup screens.
    Public,
    /// A logged-in browser (session cookie). Also the rule for a method a
    /// known path does not have; unknown paths only get a 404.
    Session,
    /// The connector (`Authorization: Bearer <connector token>`), never a
    /// session cookie; only `/api/connector/v1/*`.
    ImportToken,
}

/// One route of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteInfo {
    pub method: Method,
    /// Path template as registered, e.g. `/api/words/{id}`.
    pub path: &'static str,
    pub access: Access,
}

/// Every route of the app with its access rule. The router is built from
/// the same table, so tests can check each route's protection.
pub fn routes() -> Vec<RouteInfo> {
    endpoints()
        .into_iter()
        .map(|e| RouteInfo {
            method: e.verb.method(),
            path: e.path,
            access: e.access,
        })
        .collect()
}

/// HTTP methods the app uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Get,
    Post,
    Patch,
}

impl Verb {
    fn method(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
            Self::Patch => Method::PATCH,
        }
    }

    fn filter(self) -> MethodFilter {
        match self {
            Self::Get => MethodFilter::GET,
            Self::Post => MethodFilter::POST,
            Self::Patch => MethodFilter::PATCH,
        }
    }
}

/// Route groups share a body limit, a timeout and possibly a rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Standard,
    Login,
    ImportPreview,
    ImportCommit,
    Connector,
}

const GROUPS: [Group; 5] = [
    Group::Standard,
    Group::Login,
    Group::ImportPreview,
    Group::ImportCommit,
    Group::Connector,
];

struct Endpoint {
    verb: Verb,
    path: &'static str,
    access: Access,
    group: Group,
    handler: MethodRouter<AppState>,
}

fn endpoint<H, T>(
    group: Group,
    access: Access,
    verb: Verb,
    path: &'static str,
    handler: H,
) -> Endpoint
where
    H: Handler<T, AppState>,
    T: 'static,
{
    Endpoint {
        verb,
        path,
        access,
        group,
        handler: on(verb.filter(), handler),
    }
}

/// The route table (design §8).
fn endpoints() -> Vec<Endpoint> {
    use Access::{ImportToken, Public, Session};
    use Group::{Connector, ImportCommit, ImportPreview, Login, Standard};
    use Verb::{Get, Patch, Post};
    vec![
        endpoint(Standard, Public, Get, HEALTHZ_PATH, healthz),
        endpoint(
            Standard,
            Public,
            Get,
            web_assets::INDEX_PATH,
            web_assets::index,
        ),
        endpoint(
            Standard,
            Public,
            Get,
            web_assets::ASSET_PATH,
            web_assets::asset,
        ),
        endpoint(Standard, Public, Get, "/api/auth/setup", setup_status),
        endpoint(Login, Public, Post, "/api/auth/setup", setup),
        endpoint(Login, Public, Post, "/api/auth/login", login),
        endpoint(Standard, Session, Post, "/api/auth/logout", logout),
        endpoint(Standard, Session, Get, "/api/auth/me", me),
        endpoint(Login, Session, Post, "/api/auth/password", change_password),
        endpoint(
            Standard,
            Session,
            Get,
            "/api/auth/connector-token",
            connector_token_status,
        ),
        endpoint(
            Login,
            Session,
            Post,
            "/api/auth/connector-token",
            create_connector_token,
        ),
        endpoint(Standard, Session, Get, "/api/import/recent", import_recent),
        endpoint(
            ImportPreview,
            Session,
            Post,
            "/api/import/preview",
            import_preview,
        ),
        endpoint(
            ImportCommit,
            Session,
            Post,
            "/api/import/commit",
            import_commit,
        ),
        endpoint(Standard, Session, Get, "/api/sources", list_sources),
        endpoint(Standard, Session, Get, "/api/words", list_words),
        endpoint(Standard, Session, Get, "/api/words/{id}", show_word),
        endpoint(Standard, Session, Get, "/api/today", today),
        endpoint(Standard, Session, Post, "/api/sessions", start_session),
        endpoint(
            Standard,
            Session,
            Get,
            "/api/sessions/current",
            current_session,
        ),
        endpoint(
            Standard,
            Session,
            Post,
            "/api/sessions/{id}/reviews",
            rate_item,
        ),
        endpoint(
            Standard,
            Session,
            Post,
            "/api/sessions/{id}/undo",
            undo_rating,
        ),
        endpoint(
            Standard,
            Session,
            Post,
            "/api/sessions/{id}/finish",
            finish_session,
        ),
        endpoint(Standard, Session, Get, "/api/settings", get_settings),
        endpoint(Standard, Session, Patch, "/api/settings", patch_settings),
        endpoint(
            Connector,
            ImportToken,
            Get,
            "/api/connector/v1/sources",
            connector_sources,
        ),
    ]
}

/// The complete application router.
pub fn app(state: AppState, limits: &Limits) -> Router {
    let state = AppState {
        import_body_bytes: limits.import_body_bytes,
        ..state
    };
    let standard = TimeoutPolicy {
        duration: limits.request_timeout(),
        message: TIMEOUT_MESSAGE,
    };
    // Preview and commit share one budget: one paste is usually both.
    let import_limit = Arc::new(RateLimit::new(IMPORT_RATE_MAX, RATE_WINDOW));
    let connector_limit = Arc::new(RateLimit::new(CONNECTOR_RATE_MAX, RATE_WINDOW));
    let endpoints = endpoints();
    let mut access: HashMap<&'static str, Vec<(Method, Access)>> = HashMap::new();
    for e in &endpoints {
        access
            .entry(e.path)
            .or_default()
            .push((e.verb.method(), e.access));
    }
    let mut routes = Router::new();
    for group in GROUPS {
        let mut router: Router<AppState> = Router::new();
        for e in endpoints.iter().filter(|e| e.group == group) {
            router = router.route(e.path, e.handler.clone());
        }
        let router = router.with_state(state.clone());
        let (body_bytes, timeout, rate) = match group {
            Group::Standard => (limits.body_bytes, standard, None),
            Group::Login => (LOGIN_BODY_BYTES, standard, None),
            Group::ImportPreview => (
                limits.import_body_bytes,
                TimeoutPolicy {
                    message: PREVIEW_TIMEOUT_MESSAGE,
                    ..standard
                },
                Some(&import_limit),
            ),
            Group::ImportCommit => (
                limits.import_body_bytes,
                TimeoutPolicy {
                    duration: limits.import_commit_timeout(),
                    message: IMPORT_TIMEOUT_MESSAGE,
                },
                Some(&import_limit),
            ),
            Group::Connector => (CONNECTOR_BODY_BYTES, standard, Some(&connector_limit)),
        };
        let mut router = with_limits(router, body_bytes, timeout);
        if let Some(limit) = rate {
            router = router.layer(middleware::from_fn_with_state(
                (Arc::clone(limit), Arc::clone(&state.clock)),
                limit_rate,
            ));
        }
        routes = routes.merge(router);
    }
    let guard = Guard {
        state,
        access: Arc::new(access),
    };
    harden(
        routes.layer(middleware::from_fn_with_state(guard, protect)),
        limits,
    )
}

/// How long a route group may take, and what to answer when it is too slow.
#[derive(Debug, Clone, Copy)]
struct TimeoutPolicy {
    duration: Duration,
    message: &'static str,
}

/// Give the routes of `routes` a body limit and a timeout.
fn with_limits(routes: Router, body_bytes: usize, timeout: TimeoutPolicy) -> Router {
    routes
        .layer(DefaultBodyLimit::max(body_bytes))
        .layer(middleware::from_fn_with_state(timeout, limit_time))
}

/// Wrap `routes` with the concurrency limit, logging and security headers.
/// The last layer added is the outermost, so the headers are set on every
/// response, including timeouts, overload answers and 404s.
fn harden(routes: Router, limits: &Limits) -> Router {
    let slots = Arc::new(Semaphore::new(limits.max_concurrent_requests));
    routes
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(slots, limit_concurrency))
        .layer(middleware::from_fn(log_request))
        .layer(middleware::from_fn(security_headers))
}

/// JSON error body used by every endpoint: `{"error": {"code", "message"}}`.
fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body = serde_json::json!({ "error": { "code": code, "message": message } });
    (status, Json(body)).into_response()
}

async fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "Not found")
}

/// Liveness: the process serves requests and its database connection answers.
async fn healthz(State(state): State<AppState>) -> Response {
    match state.db.ping().await {
        Ok(()) => Json(serde_json::json!({ "status": "ok" })).into_response(),
        Err(err) => {
            tracing::error!(error = %err, "health check: database unavailable");
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "Unavailable",
            )
        }
    }
}

// ── authentication (design §9) ─────────────────────────────────────────────

/// `Debug` of the request bodies with a password or setup code: the field
/// names only, never the values.
macro_rules! redacted_debug {
    ($($body:ident),+) => {$(
        impl std::fmt::Debug for $body {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($body), "(***)"))
            }
        }
    )+};
}

redacted_debug!(LoginBody, SetupBody, PasswordChangeBody, ConnectorTokenBody);

/// Body of `POST /api/auth/login`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginBody {
    password: String,
}

fn ok_status() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

fn set_cookie(response: &mut Response, cookie: &str) {
    if let Ok(value) = HeaderValue::from_str(cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

fn retry_after(response: &mut Response, secs: u64) {
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(secs));
}

/// "N minutes" (rounded up) for messages about waiting.
fn minutes_text(secs: u64) -> String {
    let minutes = secs.div_ceil(60).max(1);
    if minutes == 1 {
        "1 minute".to_owned()
    } else {
        format!("{minutes} minutes")
    }
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<LoginBody>, JsonRejection>,
) -> Response {
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return rejected_body(&rejection, "The body must be JSON {\"password\": \"…\"}.");
        }
    };
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    match state
        .auth
        .login(&state.db, &state.clock, body.password, user_agent)
        .await
    {
        Ok(cookie) => with_session_cookie(&cookie),
        Err(err) => auth_error(&err),
    }
}

fn with_session_cookie(cookie: &str) -> Response {
    let mut response = ok_status().into_response();
    set_cookie(&mut response, cookie);
    response
}

/// The answer to a failed login, setup, password change or token creation.
fn auth_error(err: &AuthError) -> Response {
    match err {
        AuthError::WrongPassword => error_response(
            StatusCode::UNAUTHORIZED,
            "invalid_password",
            "Wrong password.",
        ),
        AuthError::WrongSetupCode => error_response(
            StatusCode::UNAUTHORIZED,
            "invalid_setup_code",
            "Wrong setup code.",
        ),
        AuthError::Locked { retry_after_secs } => {
            let mut response = error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "locked",
                &format!(
                    "Too many failed attempts. Try again in {}.",
                    minutes_text(*retry_after_secs)
                ),
            );
            retry_after(&mut response, *retry_after_secs);
            response
        }
        AuthError::SetupRequired => error_response(
            StatusCode::CONFLICT,
            "setup_required",
            "The app is not set up yet: set a password with the setup code first.",
        ),
        AuthError::AlreadySetUp => error_response(
            StatusCode::CONFLICT,
            "already_set_up",
            "The app is already set up. Log in instead.",
        ),
        AuthError::NewPassword(reason) => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_new_password",
            &format!("The new password is not accepted: {reason}."),
        ),
        AuthError::Internal(_) => internal_error(err),
    }
}

/// Body of `POST /api/auth/setup`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupBody {
    code: String,
    password: String,
}

/// `GET /api/auth/setup`: whether the app still needs to be set up (tells
/// the page to show the setup screen instead of the login).
async fn setup_status(State(state): State<AppState>) -> Response {
    match state.auth.setup_required(&state.db).await {
        Ok(required) => Json(serde_json::json!({ "required": required })).into_response(),
        Err(err) => internal_error(&err),
    }
}

async fn setup(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<SetupBody>, JsonRejection>,
) -> Response {
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return rejected_body(
                &rejection,
                "The body must be JSON {\"code\": \"…\", \"password\": \"…\"}.",
            );
        }
    };
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    match state
        .auth
        .setup(
            &state.db,
            &state.clock,
            body.code,
            body.password,
            user_agent,
        )
        .await
    {
        Ok(cookie) => with_session_cookie(&cookie),
        Err(err) => auth_error(&err),
    }
}

/// Body of `POST /api/auth/password`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordChangeBody {
    current_password: String,
    new_password: String,
}

async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<PasswordChangeBody>, JsonRejection>,
) -> Response {
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return rejected_body(
                &rejection,
                "The body must be JSON {\"current_password\": \"…\", \"new_password\": \"…\"}.",
            );
        }
    };
    match state
        .auth
        .change_password(
            &state.db,
            &state.clock,
            &headers,
            body.current_password,
            body.new_password,
        )
        .await
    {
        Ok(()) => ok_status().into_response(),
        Err(err) => auth_error(&err),
    }
}

/// `GET /api/auth/connector-token`: whether a connector token exists (never
/// the token itself).
async fn connector_token_status(State(state): State<AppState>) -> Response {
    match state.auth.connector_token_created_at(&state.db).await {
        Ok(created_at) => Json(serde_json::json!({
            "exists": created_at.is_some(),
            "created_at": created_at,
        }))
        .into_response(),
        Err(err) => internal_error(&err),
    }
}

/// Body of `POST /api/auth/connector-token`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectorTokenBody {
    current_password: String,
}

/// `POST /api/auth/connector-token`: a new token, shown this once; the old
/// one stops working. Needs the current password.
async fn create_connector_token(
    State(state): State<AppState>,
    body: Result<Json<ConnectorTokenBody>, JsonRejection>,
) -> Response {
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return rejected_body(
                &rejection,
                "The body must be JSON {\"current_password\": \"…\"}.",
            );
        }
    };
    match state
        .auth
        .create_connector_token(&state.db, &state.clock, body.current_password)
        .await
    {
        Ok(token) => Json(serde_json::json!({ "token": token })).into_response(),
        Err(err) => auth_error(&err),
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match state.auth.logout(&state.db, &headers).await {
        Ok(cookie) => {
            let mut response = ok_status().into_response();
            set_cookie(&mut response, &cookie);
            response
        }
        Err(err) => internal_error(&err),
    }
}

/// `GET /api/auth/me`: answers 200 only with a live session (the guard
/// answers 401 otherwise).
async fn me() -> Response {
    Json(serde_json::json!({ "authenticated": true })).into_response()
}

// ── API handlers ────────────────────────────────────────────────────────────

/// Body of `POST /api/import/preview` and `POST /api/import/commit`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportRequest {
    text: String,
}

fn internal_error(err: &dyn std::fmt::Display) -> Response {
    tracing::error!(error = %err, "request failed");
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "Something went wrong on the server.",
    )
}

fn workflow_error(err: &WorkflowError) -> Response {
    match err {
        WorkflowError::Paste(paste) => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_paste",
            &paste.to_string(),
        ),
        other => internal_error(other),
    }
}

fn too_large() -> Response {
    error_response(
        StatusCode::PAYLOAD_TOO_LARGE,
        "too_large",
        "The request is too large. Import fewer packs at a time.",
    )
}

/// Read the body of an import request: JSON `{"text": "…"}` of at most
/// `limit` bytes. An oversized body is still read to its end (up to
/// [`DRAIN_FACTOR`] × `limit`) and discarded, so the client receives the
/// JSON 413 instead of a connection reset.
async fn read_import_request(
    headers: &HeaderMap,
    body: Body,
    limit: usize,
) -> Result<ImportRequest, Box<Response>> {
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return Err(Box::new(error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Send JSON with Content-Type: application/json.",
        )));
    }
    let drain_limit = limit.saturating_mul(DRAIN_FACTOR);
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > drain_limit) {
        return Err(Box::new(too_large()));
    }
    let mut body = body;
    let mut bytes: Vec<u8> = Vec::new();
    let mut total: usize = 0;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return Err(Box::new(error_response(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "The request body could not be read.",
            )));
        };
        if let Ok(data) = frame.into_data() {
            total = total.saturating_add(data.len());
            if total > drain_limit {
                return Err(Box::new(too_large()));
            }
            if total <= limit {
                bytes.extend_from_slice(&data);
            }
        }
    }
    if total > limit {
        return Err(Box::new(too_large()));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        Box::new(error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "The request body must be JSON of the form {\"text\": \"…\"}.",
        ))
    })
}

async fn import_preview(State(state): State<AppState>, headers: HeaderMap, body: Body) -> Response {
    let request = match read_import_request(&headers, body, state.import_body_bytes).await {
        Ok(request) => request,
        Err(response) => return *response,
    };
    match workflows::preview_import(&state.db, &request.text, state.moment()).await {
        Ok(report) => Json(report).into_response(),
        Err(err) => workflow_error(&err),
    }
}

async fn import_commit(State(state): State<AppState>, headers: HeaderMap, body: Body) -> Response {
    let request = match read_import_request(&headers, body, state.import_body_bytes).await {
        Ok(request) => request,
        Err(response) => return *response,
    };
    match workflows::commit_import(&state.db, &request.text, Via::Paste, state.moment()).await {
        Ok(report) => Json(report).into_response(),
        Err(err) => workflow_error(&err),
    }
}

async fn import_recent(State(state): State<AppState>) -> Response {
    match workflows::recent_imports(&state.db, RECENT_IMPORTS).await {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => internal_error(&err),
    }
}

async fn list_sources(State(state): State<AppState>) -> Response {
    match state
        .db
        .read(|conn| catalog::sources(conn).map_err(DbError::from))
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => internal_error(&err),
    }
}

/// `GET /api/connector/v1/sources` (connector token only): the sources with
/// their reading position and word count, for the connector (R-12).
async fn connector_sources(state: State<AppState>) -> Response {
    list_sources(state).await
}

/// An optional numeric query parameter: absent or empty (`?scope=`) means
/// "not given"; anything else must be an integer (G7).
fn optional_number<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<i64>, D::Error> {
    let text: Option<String> = Option::deserialize(deserializer)?;
    match text.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(number) => number.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, Deserialize)]
struct WordsParams {
    q: Option<String>,
    #[serde(default, deserialize_with = "optional_number")]
    source: Option<i64>,
}

async fn list_words(
    State(state): State<AppState>,
    params: Result<Query<WordsParams>, QueryRejection>,
) -> Response {
    let Ok(Query(params)) = params else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "Invalid query parameters.",
        );
    };
    let query = WordQuery {
        text: params.q,
        source: params.source,
    };
    match state
        .db
        .read(move |conn| catalog::words(conn, &query).map_err(DbError::from))
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => internal_error(&err),
    }
}

async fn show_word(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    match state
        .db
        .read(move |conn| catalog::word_view(conn, WordId::new(id)).map_err(DbError::from))
        .await
    {
        Ok(Some(view)) => Json(view).into_response(),
        Ok(None) => not_found().await,
        Err(err) => internal_error(&err),
    }
}

// ── practice API (design §8) ───────────────────────────────────────────────

/// Errors of the practice and settings endpoints.
#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Stats(#[from] StatsError),
}

impl From<rusqlite::Error> for ApiError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(DbError::from(err))
    }
}

/// Answer for a JSON body that could not be read: 413 when it is over the
/// route's body limit (the same answer as the import routes give), 400 when
/// it is not the expected JSON.
fn rejected_body(rejection: &JsonRejection, message: &str) -> Response {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "The request is too large.",
        )
    } else {
        bad_request(message)
    }
}

fn bad_request(message: &str) -> Response {
    error_response(StatusCode::BAD_REQUEST, "bad_request", message)
}

fn api_error(err: &ApiError) -> Response {
    let conflict = |code: &str| error_response(StatusCode::CONFLICT, code, &err.to_string());
    match err {
        ApiError::Session(e) => match e {
            SessionError::NotFound | SessionError::ItemNotFound => {
                error_response(StatusCode::NOT_FOUND, "not_found", &e.to_string())
            }
            SessionError::UnknownSource => error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "unknown_source",
                &e.to_string(),
            ),
            SessionError::Invalid(message) => {
                error_response(StatusCode::UNPROCESSABLE_ENTITY, "invalid", message)
            }
            SessionError::Expired => conflict("session_expired"),
            SessionError::NotActive(_) => conflict("session_finished"),
            SessionError::AlreadyAnswered => conflict("already_answered"),
            SessionError::NotNextItem => conflict("not_next_item"),
            SessionError::ReviewIdReused => conflict("review_id_reused"),
            SessionError::ReviewUndone => conflict("review_undone"),
            SessionError::NothingToUndo => conflict("nothing_to_undo"),
            SessionError::Random(_) | SessionError::Scheduler(_) | SessionError::Sqlite(_) => {
                internal_error(err)
            }
        },
        ApiError::Settings(SettingsError::Invalid(message)) => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, "invalid", message)
        }
        ApiError::Stats(StatsError::UnknownSource) => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_source",
            &err.to_string(),
        ),
        ApiError::Db(_) | ApiError::Settings(_) | ApiError::Stats(_) => internal_error(err),
    }
}

fn respond<T: serde::Serialize>(result: Result<T, ApiError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(err) => api_error(&err),
    }
}

#[derive(Debug, Deserialize)]
struct TodayParams {
    #[serde(default, deserialize_with = "optional_number")]
    scope: Option<i64>,
}

async fn today(
    State(state): State<AppState>,
    params: Result<Query<TodayParams>, QueryRejection>,
) -> Response {
    let Ok(Query(params)) = params else {
        return bad_request("Invalid query parameters.");
    };
    let day = state.moment().day;
    let scope = params.scope.map(SourceId::new);
    respond(
        state
            .db
            .read(move |conn| Ok::<_, ApiError>(stats::today(conn, day, scope)?))
            .await,
    )
}

/// Body of `POST /api/sessions`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartBody {
    scope_source_id: Option<i64>,
    kind: StartKind,
    #[serde(default)]
    add_new_anyway: bool,
}

async fn start_session(
    State(state): State<AppState>,
    body: Result<Json<StartBody>, JsonRejection>,
) -> Response {
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return rejected_body(
                &rejection,
                "The body must be JSON {\"kind\": \"start\"|\"continue\", \"scope_source_id\"?, \"add_new_anyway\"?}.",
            );
        }
    };
    let request = StartRequest {
        scope: body.scope_source_id.map(SourceId::new),
        kind: body.kind,
        add_new_anyway: body.add_new_anyway,
    };
    let now = state.moment();
    respond(
        state
            .db
            .write(move |tx| Ok::<_, ApiError>(session::start_block(tx, &now, &request)?))
            .await,
    )
}

async fn current_session(State(state): State<AppState>) -> Response {
    let day = state.moment().day;
    match state
        .db
        .read(move |conn| Ok::<_, ApiError>(session::current(conn, day)?))
        .await
    {
        Ok(Some(view)) => Json(view).into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => api_error(&err),
    }
}

/// Body of `POST /api/sessions/{id}/reviews` (design §8).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewBody {
    client_review_id: String,
    item_id: i64,
    raw_grade: u8,
    suggested_grade: Option<u8>,
    typed_answer: Option<String>,
    answer_match: Option<AnswerMatch>,
    #[serde(default)]
    hint_used: bool,
    duration_ms: Option<u32>,
    client_ts: Option<String>,
}

impl ReviewBody {
    fn into_input(self) -> Result<RateInput, &'static str> {
        let raw_grade = RawGrade::new(self.raw_grade).ok_or("raw_grade must be 1-5")?;
        let suggested_grade = match self.suggested_grade {
            Some(g) => Some(RawGrade::new(g).ok_or("suggested_grade must be 1-5")?),
            None => None,
        };
        Ok(RateInput {
            client_review_id: self.client_review_id,
            item_id: self.item_id,
            raw_grade,
            suggested_grade,
            typed_answer: self.typed_answer,
            answer_match: self.answer_match,
            hint_used: self.hint_used,
            duration_ms: self.duration_ms,
            client_ts: self.client_ts,
        })
    }
}

fn session_id(id: Result<Path<String>, PathRejection>) -> Option<SessionId> {
    id.ok().and_then(|Path(id)| SessionId::parse(&id))
}

async fn rate_item(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    body: Result<Json<ReviewBody>, JsonRejection>,
) -> Response {
    let Some(id) = session_id(id) else {
        return not_found().await;
    };
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return rejected_body(
                &rejection,
                "The body must be a JSON review object (design §8).",
            );
        }
    };
    let input = match body.into_input() {
        Ok(input) => input,
        Err(message) => {
            return error_response(StatusCode::UNPROCESSABLE_ENTITY, "invalid", message);
        }
    };
    let now = state.moment();
    respond(
        state
            .db
            .write(move |tx| Ok::<_, ApiError>(session::rate(tx, &id, &input, &now)?))
            .await,
    )
}

async fn undo_rating(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    let Some(id) = session_id(id) else {
        return not_found().await;
    };
    let now = state.moment();
    respond(
        state
            .db
            .write(move |tx| Ok::<_, ApiError>(session::undo_last(tx, &id, &now)?))
            .await,
    )
}

async fn finish_session(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    let Some(id) = session_id(id) else {
        return not_found().await;
    };
    let now = state.moment();
    respond(
        state
            .db
            .write(move |tx| Ok::<_, ApiError>(session::finish(tx, &id, &now)?))
            .await,
    )
}

async fn get_settings(State(state): State<AppState>) -> Response {
    respond(
        state
            .db
            .read(|conn| Ok::<_, ApiError>(settings::get(conn)?))
            .await,
    )
}

async fn patch_settings(
    State(state): State<AppState>,
    body: Result<Json<SettingsPatch>, JsonRejection>,
) -> Response {
    let patch = match body {
        Ok(Json(patch)) => patch,
        Err(rejection) => {
            return rejected_body(
                &rejection,
                "The body must be a JSON object of known settings.",
            );
        }
    };
    respond(
        state
            .db
            .write(move |tx| Ok::<_, ApiError>(settings::update(tx, &patch)?))
            .await,
    )
}

// ── middleware ──────────────────────────────────────────────────────────────

/// State of the access-control layer: the app and the access rule of every
/// (path template, method) of the route table.
#[derive(Debug, Clone)]
struct Guard {
    state: AppState,
    access: Arc<HashMap<&'static str, Vec<(Method, Access)>>>,
}

impl Guard {
    /// The access rule of the matched route; [`Access::Session`] for
    /// anything not in the table (secure by default).
    fn access_of(&self, request: &Request) -> Access {
        let method = if request.method() == Method::HEAD {
            &Method::GET
        } else {
            request.method()
        };
        request
            .extensions()
            .get::<MatchedPath>()
            .and_then(|path| self.access.get(path.as_str()))
            .and_then(|rules| rules.iter().find(|(m, _)| m == method))
            .map_or(Access::Session, |(_, access)| *access)
    }
}

fn unauthenticated() -> Response {
    error_response(StatusCode::UNAUTHORIZED, "unauthenticated", "Log in first.")
}

/// Access control of every route (design §9): connector token for the
/// connector; CSRF guard and (unless public) a live session for the rest.
async fn protect(State(guard): State<Guard>, request: Request, next: Next) -> Response {
    let auth = &guard.state.auth;
    let access = guard.access_of(&request);
    if access == Access::ImportToken {
        match auth
            .has_connector_token(&guard.state.db, request.headers())
            .await
        {
            Ok(true) => return next.run(request).await,
            Ok(false) => {}
            Err(err) => return internal_error(&err),
        }
        let mut response = error_response(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "A valid connector token is required.",
        );
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    match auth.check_csrf(request.method(), request.headers()) {
        Ok(()) => {}
        Err(CsrfRejection::Origin) => {
            return error_response(
                StatusCode::FORBIDDEN,
                "forbidden_origin",
                "The request does not come from this app.",
            );
        }
        Err(CsrfRejection::ContentType) => {
            return error_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "Send JSON with Content-Type: application/json.",
            );
        }
    }
    if access == Access::Public {
        return next.run(request).await;
    }
    let now = guard.state.clock.now();
    match auth
        .check_session(&guard.state.db, request.headers(), now)
        .await
    {
        Ok(SessionCheck::Valid { renewal }) => {
            let mut response = next.run(request).await;
            // A handler that sets the cookie itself (logout) wins.
            if let Some(cookie) = renewal
                && !response.headers().contains_key(header::SET_COOKIE)
            {
                set_cookie(&mut response, &cookie);
            }
            response
        }
        Ok(SessionCheck::Invalid) => unauthenticated(),
        Err(err) => internal_error(&err),
    }
}

/// Per-group request budget (§9.4).
async fn limit_rate(
    State((limit, clock)): State<(Arc<RateLimit>, Arc<dyn Clock>)>,
    request: Request,
    next: Next,
) -> Response {
    match limit.hit(clock.now()) {
        Ok(()) => next.run(request).await,
        Err(wait) => {
            let secs = u64::try_from(wait.as_secs()).unwrap_or(0).max(1);
            let mut response = error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                &format!("Too many requests. Try again in {}.", minutes_text(secs)),
            );
            retry_after(&mut response, secs);
            response
        }
    }
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers: &mut HeaderMap = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    // Nothing is cacheable unless a handler says so (static assets do).
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
}

async fn log_request(request: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    // The route template (e.g. `/api/words/{id}`), never the concrete path or query.
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "(unmatched)".to_owned(), |path| path.as_str().to_owned());
    let response = next.run(request).await;
    tracing::info!(
        method = %method,
        route = %route,
        status = response.status().as_u16(),
        duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "request"
    );
    response
}

async fn limit_time(
    State(timeout): State<TimeoutPolicy>,
    request: Request,
    next: Next,
) -> Response {
    match tokio::time::timeout(timeout.duration, next.run(request)).await {
        Ok(response) => response,
        Err(_) => error_response(StatusCode::SERVICE_UNAVAILABLE, "timeout", timeout.message),
    }
}

async fn limit_concurrency(
    State(slots): State<Arc<Semaphore>>,
    request: Request,
    next: Next,
) -> Response {
    match slots.acquire().await {
        Ok(_permit) => next.run(request).await,
        Err(_) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded",
            "Server is shutting down",
        ),
    }
}

/// Serve `app` on `listener` until `shutdown` resolves; then stop accepting
/// and give open requests up to the request timeout to finish.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    limits: &Limits,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let mut http1 = hyper::server::conn::http1::Builder::new();
    http1
        .timer(TokioTimer::new())
        // Applies to every request head, including the wait for the next
        // request on a keep-alive connection: idle connections are closed too.
        .header_read_timeout(limits.header_read_timeout())
        .keep_alive(true);
    let connections = Arc::new(Semaphore::new(limits.max_connections));
    let graceful = GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown);

    loop {
        // Wait for a free connection slot before accepting more sockets.
        let permit = tokio::select! {
            permit = Arc::clone(&connections).acquire_owned() => {
                permit.map_err(|_| std::io::Error::other("connection limiter closed"))?
            }
            () = &mut shutdown => break,
        };
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => stream,
                Err(err) => {
                    // E.g. EMFILE: back off instead of spinning or exiting.
                    tracing::warn!(error = %err, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let connection =
            http1.serve_connection(TokioIo::new(stream), TowerToHyperService::new(app.clone()));
        let connection = graceful.watch(connection);
        tokio::spawn(async move {
            if let Err(err) = connection.await {
                tracing::debug!(error = %err, "connection closed with error");
            }
            drop(permit);
        });
    }

    drop(listener);
    let longest = limits.request_timeout().max(limits.import_commit_timeout());
    if tokio::time::timeout(longest, graceful.shutdown())
        .await
        .is_err()
    {
        tracing::warn!("open connections did not finish in time; closing them");
    }
    Ok(())
}

/// Client side of the container health check: `GET /healthz` on the local
/// listener. Kept dependency-free and synchronous (no runtime needed); the
/// runtime image has no shell or curl, so the binary checks itself.
pub fn healthcheck(listen: SocketAddr, timeout: Duration) -> Result<(), String> {
    let target = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), listen.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), listen.port())
        }
        _ => listen,
    };
    let mut stream = TcpStream::connect_timeout(&target, timeout)
        .map_err(|e| format!("connect {target}: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|e| e.to_string())?;
    write!(
        stream,
        "GET {HEALTHZ_PATH} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| format!("send: {e}"))?;
    let mut head = [0u8; 12];
    stream
        .read_exact(&mut head)
        .map_err(|e| format!("read: {e}"))?;
    if head.starts_with(b"HTTP/1.1 200") || head.starts_with(b"HTTP/1.0 200") {
        Ok(())
    } else {
        Err(format!(
            "unexpected response: {}",
            String::from_utf8_lossy(&head)
        ))
    }
}

#[cfg(test)]
mod tests {
    use axum::body::{Bytes, to_bytes};
    use axum::http::Method;
    use axum::routing::{get, post};
    use tower::ServiceExt;

    use super::*;

    fn test_router(limits: &Limits) -> Router {
        test_router_with(limits, limits.body_bytes, TIMEOUT_MESSAGE)
    }

    fn test_router_with(limits: &Limits, body_bytes: usize, message: &'static str) -> Router {
        let routes = Router::new()
            .route(
                "/echo",
                post(|body: Bytes| async move { body.len().to_string() }),
            )
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    "done"
                }),
            );
        let timeout = TimeoutPolicy {
            duration: limits.request_timeout(),
            message,
        };
        harden(with_limits(routes, body_bytes, timeout), limits)
    }

    fn post_bytes(len: usize) -> Request {
        Request::builder()
            .method(Method::POST)
            .uri("/echo")
            .body(Body::from(vec![b'a'; len]))
            .expect("request")
    }

    #[tokio::test]
    async fn r83_request_body_over_the_default_limit_is_rejected() {
        let limits = Limits::default();
        let ok = test_router(&limits)
            .oneshot(post_bytes(limits.body_bytes))
            .await
            .expect("response");
        assert_eq!(ok.status(), StatusCode::OK);
        let too_big = test_router(&limits)
            .oneshot(post_bytes(limits.body_bytes + 1))
            .await
            .expect("response");
        assert_eq!(too_big.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            too_big
                .headers()
                .contains_key(header::CONTENT_SECURITY_POLICY)
        );
    }

    #[tokio::test]
    async fn r83_slow_requests_time_out() {
        let limits = Limits {
            request_timeout_secs: 1,
            ..Limits::default()
        };
        let response = test_router(&limits)
            .oneshot(
                Request::builder()
                    .uri("/slow")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            response
                .headers()
                .contains_key(header::CONTENT_SECURITY_POLICY)
        );
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert!(String::from_utf8_lossy(&body).contains("\"timeout\""));
    }

    #[tokio::test]
    async fn r84_route_groups_have_their_own_body_limit_and_timeout_message() {
        let limits = Limits {
            request_timeout_secs: 1,
            ..Limits::default()
        };
        let router = test_router_with(&limits, limits.import_body_bytes, IMPORT_TIMEOUT_MESSAGE);
        let big = router
            .clone()
            .oneshot(post_bytes(limits.body_bytes * 4))
            .await
            .expect("response");
        assert_eq!(big.status(), StatusCode::OK);
        let too_big = router
            .clone()
            .oneshot(post_bytes(limits.import_body_bytes + 1))
            .await
            .expect("response");
        assert_eq!(too_big.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let slow = router
            .oneshot(
                Request::builder()
                    .uri("/slow")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(slow.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(slow.into_body(), 4096).await.expect("body");
        assert!(String::from_utf8_lossy(&body).contains("re-pasting the same text is safe"));
    }

    #[test]
    fn r83_healthcheck_fails_when_nothing_listens() {
        // Bind and drop to get a port that is (almost certainly) closed.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("free port")
            .port();
        let addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port);
        assert!(healthcheck(addr, Duration::from_secs(1)).is_err());
    }

    #[test]
    fn r82_bodies_with_secrets_never_show_them_in_debug() {
        let secret = "test-password-never-logged";
        let shown = [
            format!(
                "{:?}",
                LoginBody {
                    password: secret.to_owned()
                }
            ),
            format!(
                "{:?}",
                SetupBody {
                    code: secret.to_owned(),
                    password: secret.to_owned()
                }
            ),
            format!(
                "{:?}",
                PasswordChangeBody {
                    current_password: secret.to_owned(),
                    new_password: secret.to_owned()
                }
            ),
            format!(
                "{:?}",
                ConnectorTokenBody {
                    current_password: secret.to_owned()
                }
            ),
        ];
        for text in shown {
            assert!(!text.contains(secret), "{text}");
            assert!(text.ends_with("(***)"), "{text}");
        }
    }
}
