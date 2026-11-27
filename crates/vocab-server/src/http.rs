//! HTTP router and cross-cutting request handling (design §3.2, §8, §9.4).
//!
//! Every response carries the security headers (strict CSP and friends); every
//! request gets a body size limit, a timeout and a slot in a bounded
//! concurrency pool, and is logged with method, path template, status and
//! duration only (never bodies or query values, R-24). Handlers stay thin:
//! parse, call a module, map errors.
//!
//! Body limits and timeouts are set per route group: the default (64 KiB,
//! 30 s) for everything, larger ones for `/api/import/*` (2.5 MiB; the
//! commit may take 120 s). A timeout only stops waiting: database work that
//! already started finishes in the background (see `workflows`), so the
//! timeout answer of an import tells the owner that importing again is safe.
//!
//! Nobody without a session can keep the app from answering (R-83):
//!
//! * a request waits only briefly for a slot and is then refused (503 with
//!   `Retry-After`); a few slots are reserved for requests that carry the
//!   cookie of a known live session ([`Slots`]);
//! * the body of a request to a public route (login, setup) must arrive
//!   within a few seconds; everything else is refused before its body is
//!   read (no session → 401);
//! * `/healthz` is served outside the slot pool and never touches the
//!   database, so the container's health check answers under any load;
//! * one import is processed at a time, its body included, so imports
//!   cannot pile up in memory.
//!
//! Access control (design §9) is one layer over every route: each route of
//! the table in [`routes`] is public, needs a session, or needs the import
//! token; anything else needs a session. State-changing requests must pass
//! the CSRF guard (Origin + JSON), and API requests a browser labels as
//! cross-site or same-site are refused. The `auth` module does the checks.
//!
//! [`serve`] owns the connection level: a header read timeout (which also
//! closes idle keep-alive connections), a cap on open connections and a
//! lower cap per peer address, so a client that opens sockets or trickles
//! bytes cannot hold them all (slowloris).

use std::collections::HashMap;
use std::future::Future;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
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
use hyper_util::rt::TokioIo;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use jiff::SignedDuration;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::audio::Audio;
use crate::auth::{self, Auth, AuthError, CsrfRejection, SessionCheck};
use crate::catalog::{self, EditError, ListKind, SourceId, WordId, WordQuery};
use crate::clock::{Clock, DayRule, Moment, SystemClock};
use crate::config::Limits;
use crate::db::{Db, DbError};
use crate::exports::{self, ExportError};
use crate::importer::Via;
use crate::keep_alive;
use crate::rate_limit::RateLimit;
use crate::scheduler::{AnswerMatch, CardKind, RawGrade};
use crate::session::{self, RateInput, SessionError, SessionId, StartKind, StartRequest};
use crate::settings::{self, SettingsError, SettingsPatch};
use crate::stats::{self, StatsError, WordListQuery};
use crate::undo::UndoToken;
use crate::web_assets;
use crate::workflows::{self, LeechAction, WordAction, WorkflowError};

const HEALTHZ_PATH: &str = "/healthz";

/// `require-trusted-types-for`: the page builds its DOM with nodes and text
/// only, so every string-to-code sink (`innerHTML` and friends) is simply
/// forbidden; no policy is defined, so none can be used.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
     img-src 'self' data:; media-src 'self'; connect-src 'self'; object-src 'none'; \
     base-uri 'none'; frame-ancestors 'none'; form-action 'self'; \
     require-trusted-types-for 'script'";

/// Header names are lowercase, as `HeaderName::from_static` requires.
const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("content-security-policy", CONTENT_SECURITY_POLICY),
    ("x-content-type-options", "nosniff"),
    // Not `no-referrer`: under it Firefox sends `Origin: null` on same-origin
    // POSTs, which the CSRF guard (§9.3) must refuse. Nothing leaves the site.
    ("referrer-policy", "same-origin"),
    ("cross-origin-opener-policy", "same-origin"),
    // No other origin may embed or read anything the app serves.
    ("cross-origin-resource-policy", "same-origin"),
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
/// What a client that was refused for load should wait, in seconds.
const RETRY_AFTER_BUSY_SECS: u64 = 5;
/// Most request log lines per second; the rest are counted, not written
/// (a flood of requests must not turn into a flood of log, R-83).
const LOG_LINES_PER_SECOND: u32 = 20;

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
    /// One import (preview or commit) at a time, its body included.
    import_turn: Arc<Semaphore>,
    /// How long an import waits for its turn before it is refused.
    import_wait: Duration,
    /// Time the body of an import request may take to arrive.
    body_read_timeout: Duration,
    /// Neural audio; `None` without `VOCAB_TTS_URL` (browser speech only).
    audio: Option<Audio>,
    /// One full export at a time (it streams after its request slot is
    /// given back, design §8.1).
    export_turn: Arc<Semaphore>,
    /// A full export download that takes no chunk for this long is broken
    /// off (set by [`app`] from the limits).
    export_idle: Duration,
}

impl AppState {
    pub fn new(db: Db, day_rule: DayRule, auth: Arc<Auth>) -> Self {
        Self::with_clock(db, Arc::new(SystemClock), day_rule, auth)
    }

    /// With a controllable clock (tests).
    pub fn with_clock(db: Db, clock: Arc<dyn Clock>, day_rule: DayRule, auth: Arc<Auth>) -> Self {
        let limits = Limits::default();
        Self {
            db,
            clock,
            day_rule,
            auth,
            import_body_bytes: limits.import_body_bytes,
            import_turn: Arc::new(Semaphore::new(1)),
            import_wait: limits.slot_wait(),
            body_read_timeout: limits.request_timeout(),
            audio: None,
            export_turn: Arc::new(Semaphore::new(1)),
            export_idle: limits.export_idle(),
        }
    }

    /// With neural audio (`VOCAB_TTS_URL` set).
    #[must_use]
    pub fn with_audio(mut self, audio: Audio) -> Self {
        self.audio = Some(audio);
        self
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
    Delete,
}

impl Verb {
    fn method(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
            Self::Patch => Method::PATCH,
            Self::Delete => Method::DELETE,
        }
    }

    fn filter(self) -> MethodFilter {
        match self {
            Self::Get => MethodFilter::GET,
            Self::Post => MethodFilter::POST,
            Self::Patch => MethodFilter::PATCH,
            Self::Delete => MethodFilter::DELETE,
        }
    }
}

/// Route groups share a body limit, a timeout and possibly a rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    /// `/healthz`: outside the slot pool, no body, no database.
    Health,
    Standard,
    /// Small bodies that hold a password, sent inside a session.
    Login,
    /// Login and setup: the same small bodies from anyone, so the body
    /// must also arrive quickly.
    PublicLogin,
    ImportPreview,
    ImportCommit,
    Connector,
}

/// The groups served from the slot pool ([`Group::Health`] is not).
const POOLED_GROUPS: [Group; 6] = [
    Group::Standard,
    Group::Login,
    Group::PublicLogin,
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
    use Group::{Connector, Health, ImportCommit, ImportPreview, Login, PublicLogin, Standard};
    use Verb::{Delete, Get, Patch, Post};
    vec![
        endpoint(Health, Public, Get, HEALTHZ_PATH, healthz),
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
        endpoint(PublicLogin, Public, Post, "/api/auth/setup", setup),
        endpoint(PublicLogin, Public, Post, "/api/auth/login", login),
        endpoint(Standard, Session, Post, "/api/auth/logout", logout),
        endpoint(Standard, Session, Post, "/api/auth/logout-all", logout_all),
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
        endpoint(Standard, Session, Patch, "/api/words/{id}", patch_word),
        endpoint(Standard, Session, Delete, "/api/words/{id}", delete_word),
        endpoint(
            Standard,
            Session,
            Post,
            "/api/words/{id}/actions",
            word_action,
        ),
        endpoint(
            Standard,
            Session,
            Post,
            "/api/words/{id}/undo",
            undo_word_change,
        ),
        endpoint(Standard, Session, Post, "/api/words/{id}/items", add_item),
        endpoint(
            Standard,
            Session,
            Delete,
            "/api/words/{id}/items",
            remove_item,
        ),
        endpoint(
            Standard,
            Session,
            Delete,
            "/api/words/{id}/spellings",
            remove_spelling,
        ),
        endpoint(
            Standard,
            Session,
            Post,
            "/api/conflicts/{id}",
            resolve_conflict,
        ),
        endpoint(
            Standard,
            Session,
            Patch,
            "/api/sentences/{id}",
            patch_sentence,
        ),
        endpoint(
            Standard,
            Session,
            Post,
            "/api/sentences/{id}/hidden",
            hide_sentence,
        ),
        endpoint(Standard, Session, Get, "/api/today", today),
        endpoint(Standard, Session, Get, "/api/progress", progress),
        endpoint(
            Standard,
            Session,
            Get,
            "/api/export/feedback",
            export_feedback,
        ),
        endpoint(Standard, Session, Get, "/api/export/full", export_full),
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
        endpoint(Standard, Session, Get, "/api/audio/status", audio_status),
        endpoint(Standard, Session, Post, "/api/audio/retry", audio_retry),
        endpoint(Standard, Session, Get, "/api/audio/words/{id}", audio_word),
        endpoint(
            Standard,
            Session,
            Get,
            "/api/audio/clips/{file}",
            audio_clip,
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
        import_wait: limits.slot_wait(),
        body_read_timeout: limits.request_timeout(),
        export_idle: limits.export_idle(),
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
    let group_router = |group: Group| {
        let mut router: Router<AppState> = Router::new();
        for e in endpoints.iter().filter(|e| e.group == group) {
            router = router.route(e.path, e.handler.clone());
        }
        router.with_state(state.clone())
    };
    let mut routes = Router::new();
    for group in POOLED_GROUPS {
        let router = group_router(group);
        let (body_bytes, timeout, rate) = match group {
            Group::Health | Group::Standard => (limits.body_bytes, standard, None),
            Group::Login | Group::PublicLogin => (LOGIN_BODY_BYTES, standard, None),
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
        if group == Group::PublicLogin {
            // Added last, so it runs first: the body has its own short
            // deadline, and the request timeout covers what follows.
            router = router.layer(middleware::from_fn_with_state(
                BodyDeadline {
                    within: limits.public_body_read_timeout(),
                    max_bytes: body_bytes,
                },
                buffer_body_in_time,
            ));
        }
        if let Some(limit) = rate {
            router = router.layer(middleware::from_fn_with_state(
                (Arc::clone(limit), Arc::clone(&state.clock)),
                limit_rate,
            ));
        }
        routes = routes.merge(router);
    }
    let slots = Slots::new(
        limits,
        Some((Arc::clone(&state.auth), Arc::clone(&state.clock))),
    );
    let health = group_router(Group::Health);
    let guard = Guard {
        state,
        access: Arc::new(access),
    };
    harden(
        routes.layer(middleware::from_fn_with_state(guard, protect)),
        health,
        slots,
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

/// Wrap `routes` with the concurrency limit, then add the `health` routes
/// (which therefore never wait for a slot), logging and security headers.
/// The last layer added is the outermost, so the headers are set on every
/// response, including timeouts, overload answers and 404s.
fn harden(routes: Router, health: Router, slots: Slots) -> Router {
    routes
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(slots, limit_concurrency))
        .merge(health)
        .layer(middleware::from_fn_with_state(
            Arc::new(LogBudget::new(LOG_LINES_PER_SECOND)),
            log_request,
        ))
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

/// Liveness: the process accepts connections and serves requests. It asks
/// nothing of the database (whose single connection a long import may hold)
/// and needs no request slot, so it answers at once whatever else is going
/// on; a container that is merely busy is never restarted for it. The
/// database is checked when the app starts, and a failing one shows in
/// every request's log line.
async fn healthz() -> Response {
    Json(serde_json::json!({ "status": "ok" })).into_response()
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
        AuthError::SessionEnded => {
            let mut response = error_response(
                StatusCode::UNAUTHORIZED,
                "session_ended",
                "Too many wrong passwords: this device was logged out. Log in again.",
            );
            set_cookie(&mut response, &auth::expired_session_cookie());
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
        // Every session ended; this device continues with a new one.
        Ok(cookie) => with_session_cookie(&cookie),
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
    headers: HeaderMap,
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
        .create_connector_token(&state.db, &state.clock, &headers, body.current_password)
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

/// `POST /api/auth/logout-all`: "log out everywhere". Every session ends,
/// this device's too; the answer removes its cookie.
async fn logout_all(State(state): State<AppState>) -> Response {
    match state.auth.logout_all(&state.db).await {
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

fn body_timeout() -> Response {
    error_response(
        StatusCode::REQUEST_TIMEOUT,
        "timeout",
        "The request body did not arrive in time.",
    )
}

/// The server has no room for the request right now.
fn busy(code: &str, message: &str) -> Response {
    let mut response = error_response(StatusCode::SERVICE_UNAVAILABLE, code, message);
    retry_after(&mut response, RETRY_AFTER_BUSY_SECS);
    response
}

/// Read the body of an import request: JSON `{"text": "…"}` of at most
/// `limit` bytes, which must arrive within `within`. An oversized body is
/// still read to its end (up to [`DRAIN_FACTOR`] × `limit`) and discarded,
/// so the client receives the JSON 413 instead of a connection reset.
async fn read_import_request(
    headers: &HeaderMap,
    body: Body,
    limit: usize,
    within: Duration,
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
    let read = async {
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
        Ok(bytes)
    };
    let bytes = match tokio::time::timeout(within, read).await {
        Ok(bytes) => bytes?,
        Err(_) => return Err(Box::new(body_timeout())),
    };
    // Straight into the one string: no JSON tree of the body is built.
    serde_json::from_slice(&bytes).map_err(|_| {
        Box::new(error_response(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "The request body must be JSON of the form {\"text\": \"…\"}.",
        ))
    })
}

impl AppState {
    /// Wait (briefly) for the import turn, then read the request. The turn
    /// is taken first, so at most one import body is in memory at a time.
    async fn import_request(
        &self,
        headers: &HeaderMap,
        body: Body,
    ) -> Result<(OwnedSemaphorePermit, ImportRequest), Box<Response>> {
        let turn = Arc::clone(&self.import_turn).acquire_owned();
        let Ok(Ok(turn)) = tokio::time::timeout(self.import_wait, turn).await else {
            return Err(Box::new(busy(
                "busy",
                "Another import is being processed. Try again in a moment.",
            )));
        };
        let request = read_import_request(
            headers,
            body,
            self.import_body_bytes,
            self.body_read_timeout,
        )
        .await?;
        Ok((turn, request))
    }
}

/// Run an import in its own task that keeps the import turn until the work
/// is really done, also when the request stops waiting for it (timeout).
async fn run_import<F>(turn: OwnedSemaphorePermit, work: F) -> Response
where
    F: Future<Output = Result<crate::importer::ImportReport, WorkflowError>> + Send + 'static,
{
    let task = tokio::spawn(async move {
        let _turn = turn;
        work.await
    });
    match task.await {
        Ok(Ok(report)) => Json(report).into_response(),
        Ok(Err(err)) => workflow_error(&err),
        Err(err) => internal_error(&err),
    }
}

async fn import_preview(State(state): State<AppState>, headers: HeaderMap, body: Body) -> Response {
    let (turn, request) = match state.import_request(&headers, body).await {
        Ok(ready) => ready,
        Err(response) => return *response,
    };
    let now = state.moment();
    run_import(turn, async move {
        workflows::preview_import(&state.db, &request.text, now).await
    })
    .await
}

async fn import_commit(State(state): State<AppState>, headers: HeaderMap, body: Body) -> Response {
    let (turn, request) = match state.import_request(&headers, body).await {
        Ok(ready) => ready,
        Err(response) => return *response,
    };
    let now = state.moment();
    let audio = state.audio.clone();
    let response = run_import(turn, async move {
        workflows::commit_import(&state.db, &request.text, Via::Paste, now).await
    })
    .await;
    // New words and sentences get their clips (the worker syncs).
    if let Some(audio) = audio {
        audio.wake();
    }
    response
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

/// Longest search text of the word list.
const WORD_SEARCH_MAX_CHARS: usize = 200;

#[derive(Debug, Deserialize)]
struct WordsParams {
    q: Option<String>,
    #[serde(default, deserialize_with = "optional_number")]
    source: Option<i64>,
    #[serde(default, deserialize_with = "optional_number")]
    min_u: Option<i64>,
    #[serde(default, deserialize_with = "optional_number")]
    max_u: Option<i64>,
    /// `alpha`, `usefulness`, `added`, `due`, `status`, `source`.
    sort: Option<String>,
    /// `new`, `learning`, `familiar`, `known`, `leech`.
    status: Option<String>,
    /// Comma-separated: `conflicts`, `suspended`, `low_priority`.
    flags: Option<String>,
}

/// A query value naming an enum variant (`None` when absent or empty).
fn named<T: serde::de::DeserializeOwned>(text: Option<&str>) -> Result<Option<T>, ()> {
    match text.map(str::trim) {
        None | Some("") => Ok(None),
        Some(name) => serde_json::from_value(serde_json::Value::String(name.to_owned()))
            .map(Some)
            .map_err(|_| ()),
    }
}

impl WordsParams {
    fn into_query(self) -> Result<WordListQuery, &'static str> {
        let usefulness = |u: Option<i64>| match u {
            Some(n) if !(1..=10).contains(&n) => Err("min_u and max_u must be 1 to 10."),
            other => Ok(other),
        };
        if self
            .q
            .as_ref()
            .is_some_and(|q| q.chars().count() > WORD_SEARCH_MAX_CHARS)
        {
            return Err("The search text is too long (at most 200 characters).");
        }
        let mut filter = WordQuery {
            text: self.q,
            source: self.source,
            min_usefulness: usefulness(self.min_u)?,
            max_usefulness: usefulness(self.max_u)?,
            ..WordQuery::default()
        };
        for flag in self.flags.as_deref().unwrap_or("").split(',') {
            match flag.trim() {
                "" => {}
                "conflicts" => filter.with_conflicts = true,
                "suspended" => filter.suspended = true,
                "low_priority" => filter.low_priority = true,
                _ => return Err("flags may be conflicts, suspended, low_priority."),
            }
        }
        Ok(WordListQuery {
            filter,
            status: named(self.status.as_deref())
                .map_err(|()| "status may be new, learning, familiar, known, leech.")?,
            sort: named(self.sort.as_deref())
                .map_err(|()| "sort may be alpha, usefulness, added, due, status, source.")?,
        })
    }
}

/// `GET /api/words`: the word list (design §7.1 screen 6, R-73).
async fn list_words(
    State(state): State<AppState>,
    params: Result<Query<WordsParams>, QueryRejection>,
) -> Response {
    let Ok(Query(params)) = params else {
        return bad_request("Invalid query parameters.");
    };
    let query = match params.into_query() {
        Ok(query) => query,
        Err(message) => return bad_request(message),
    };
    respond(
        state
            .db
            .read(move |conn| Ok::<_, ApiError>(stats::word_list(conn, &query)?))
            .await,
    )
}

/// The word's detail (design §7.1 screen 7), or 404. After an owner
/// change, `undo` is the token of its Undo (`null`: nothing changed).
async fn word_detail(state: &AppState, id: WordId, undo: Option<Option<UndoToken>>) -> Response {
    let day = state.moment().day;
    match state
        .db
        .read(move |conn| Ok::<_, ApiError>(stats::word_detail(conn, day, id)?))
        .await
    {
        Ok(Some(detail)) => match undo {
            None => Json(detail).into_response(),
            Some(token) => match serde_json::to_value(&detail) {
                Ok(mut value) => {
                    value["undo"] = serde_json::json!(token);
                    Json(value).into_response()
                }
                Err(err) => internal_error(&err),
            },
        },
        Ok(None) => not_found().await,
        Err(err) => api_error(&err),
    }
}

/// `GET /api/words/{id}`.
async fn show_word(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    word_detail(&state, WordId::new(id), None).await
}

/// Answer for an owner edit that was refused.
fn edit_error(err: &WorkflowError) -> Response {
    let answer = |status: StatusCode, code: &str| error_response(status, code, &err.to_string());
    match err {
        WorkflowError::Edit(edit) => match edit {
            EditError::WordNotFound
            | EditError::SentenceNotFound
            | EditError::ConflictNotFound
            | EditError::ItemNotFound => answer(StatusCode::NOT_FOUND, "not_found"),
            EditError::Invalid(_) => answer(StatusCode::UNPROCESSABLE_ENTITY, "invalid"),
            EditError::LastCzech => answer(StatusCode::UNPROCESSABLE_ENTITY, "last_czech"),
            EditError::TooManyItems => answer(StatusCode::UNPROCESSABLE_ENTITY, "too_many_items"),
            EditError::ItemExists(_) => answer(StatusCode::CONFLICT, "item_exists"),
            EditError::HeadwordTaken(_) | EditError::SpellingTaken { .. } => {
                answer(StatusCode::CONFLICT, "headword_taken")
            }
            EditError::DuplicateSentence => answer(StatusCode::CONFLICT, "duplicate_sentence"),
            EditError::ConflictClosed => answer(StatusCode::CONFLICT, "conflict_closed"),
            EditError::Sqlite(_) => internal_error(err),
        },
        WorkflowError::NoSuchCard(_) => answer(StatusCode::UNPROCESSABLE_ENTITY, "no_such_card"),
        WorkflowError::Undo(_) | WorkflowError::ChangedSince => {
            answer(StatusCode::CONFLICT, "undo_unavailable")
        }
        other => internal_error(other),
    }
}

/// What an owner edit did.
struct Edited {
    word: WordId,
    /// Spoken text may have changed (the headword or `speak`).
    spoken: bool,
    /// `Some`: the change offers an Undo (`Some(None)`: nothing changed).
    undo: Option<Option<UndoToken>>,
}

impl Edited {
    /// A change with an Undo (see [`workflows::with_undo`]).
    fn undoable(word: WordId, spoken: bool, undo: Option<UndoToken>) -> Self {
        Self {
            word,
            spoken,
            undo: Some(undo),
        }
    }
}

/// Run an owner edit in one transaction; then answer with the word's
/// detail. `speech` = spoken text may have changed: the audio module syncs
/// (new clips are queued, clips no text needs any more are dropped; a clip
/// is named by its text, so an old one is never played for a new text).
async fn edit_then_show<F>(state: &AppState, speech: bool, edit: F) -> Response
where
    F: FnOnce(&rusqlite::Transaction<'_>, &Moment) -> Result<Edited, WorkflowError>
        + Send
        + 'static,
{
    let now = state.moment();
    match state.db.write(move |tx| edit(tx, &now)).await {
        Ok(edited) => {
            if (speech || edited.spoken)
                && let Some(audio) = &state.audio
            {
                audio.wake();
            }
            word_detail(state, edited.word, edited.undo).await
        }
        Err(err) => edit_error(&err),
    }
}

/// `PATCH /api/words/{id}` `{field: value, …}`: edit fields (R-04; the
/// changed ones become the owner's, design §5.4 rule 4).
async fn patch_word(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    let edit = match body {
        Ok(Json(edit)) => edit,
        Err(rejection) => {
            return rejected_body(&rejection, "The body must be a JSON object of word fields.");
        }
    };
    let id = WordId::new(id);
    edit_then_show(&state, false, move |tx, now| {
        let (changed, undo) =
            workflows::with_undo(tx, id, now, |tx| workflows::edit_word(tx, id, &edit, now))?;
        let spoken = changed.iter().any(|f| *f == "headword" || *f == "speak");
        Ok(Edited::undoable(id, spoken, undo))
    })
    .await
}

/// `DELETE /api/words/{id}`: delete the word, its cards and history.
async fn delete_word(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    let id = WordId::new(id);
    match state
        .db
        .write(move |tx| workflows::delete_word(tx, id))
        .await
    {
        Ok(()) => {
            if let Some(audio) = &state.audio {
                audio.wake();
            }
            Json(serde_json::json!({ "deleted": id })).into_response()
        }
        Err(err) => edit_error(&err),
    }
}

/// Body of `POST /api/words/{id}/undo`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UndoBody {
    /// The `undo` token of the change's answer.
    token: String,
}

/// `POST /api/words/{id}/undo` `{token}`: undo the owner's latest change
/// of the word, exactly (`409 undo_unavailable` when it is gone, too old,
/// or the word changed since) → detail.
async fn undo_word_change(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<UndoBody>, JsonRejection>,
) -> Response {
    const SHAPE: &str = "The body must be {\"token\": \"…\"} with the undo token of the change.";
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    let token = match body {
        Ok(Json(body)) => body.token,
        Err(rejection) => return rejected_body(&rejection, SHAPE),
    };
    let Some(token) = UndoToken::parse(&token) else {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "invalid", SHAPE);
    };
    let id = WordId::new(id);
    edit_then_show(&state, true, move |tx, now| {
        workflows::undo_change(tx, id, &token, now)?;
        Ok(Edited {
            word: id,
            spoken: true,
            undo: None,
        })
    })
    .await
}

/// Body of `POST /api/words/{id}/actions`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionBody {
    /// `suspend`, `unsuspend`, `include`, `exclude`, `reset_card`, and the
    /// leech screen's `leech_keep`, `leech_recognise`, `leech_usefulness`,
    /// `leech_suspend` (design §6.5).
    action: String,
    /// `reset_card` only: `understand`, `produce` or `spelling`.
    kind: Option<String>,
    /// `leech_usefulness` only: the new, lower usefulness.
    usefulness: Option<i64>,
}

impl ActionBody {
    fn action(&self) -> Option<WordAction> {
        let kind = self.kind.as_deref();
        if let Some(value) = self.usefulness {
            return (self.action == "leech_usefulness" && kind.is_none())
                .then_some(WordAction::Leech(LeechAction::LowerUsefulness(value)));
        }
        match (self.action.as_str(), kind) {
            ("leech_keep", None) => Some(WordAction::Leech(LeechAction::Keep)),
            ("leech_recognise", None) => Some(WordAction::Leech(LeechAction::RecogniseOnly)),
            ("leech_suspend", None) => Some(WordAction::Leech(LeechAction::Suspend)),
            ("suspend", None) => Some(WordAction::Suspend),
            ("unsuspend", None) => Some(WordAction::Unsuspend),
            ("include", None) => Some(WordAction::Include),
            ("exclude", None) => Some(WordAction::Exclude),
            ("reset_card", Some("understand")) => Some(WordAction::ResetCard(CardKind::Understand)),
            ("reset_card", Some("produce")) => Some(WordAction::ResetCard(CardKind::Produce)),
            ("reset_card", Some("spelling")) => Some(WordAction::ResetCard(CardKind::Spelling)),
            _ => None,
        }
    }
}

/// `POST /api/words/{id}/actions`: suspend, include, reset a card.
async fn word_action(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ActionBody>, JsonRejection>,
) -> Response {
    const SHAPE: &str = "The body must be {\"action\": \"suspend\"|\"unsuspend\"|\"include\"|\
                         \"exclude\"|\"reset_card\"|\"leech_keep\"|\"leech_recognise\"|\
                         \"leech_usefulness\"|\"leech_suspend\", \"kind\"?: \"understand\"|\
                         \"produce\"|\"spelling\" (reset_card only), \"usefulness\"?: a number \
                         (leech_usefulness only)}.";
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    let action = match body {
        Ok(Json(body)) => match body.action() {
            Some(action) => action,
            None => return error_response(StatusCode::UNPROCESSABLE_ENTITY, "invalid", SHAPE),
        },
        Err(rejection) => return rejected_body(&rejection, SHAPE),
    };
    let id = WordId::new(id);
    edit_then_show(&state, false, move |tx, now| {
        // "Reset card" asks first and has no Undo.
        if let WordAction::ResetCard(_) = action {
            workflows::word_action(tx, id, action, now)?;
            return Ok(Edited {
                word: id,
                spoken: false,
                undo: None,
            });
        }
        let ((), undo) = workflows::with_undo(tx, id, now, |tx| {
            workflows::word_action(tx, id, action, now)
        })?;
        Ok(Edited::undoable(id, false, undo))
    })
    .await
}

/// Body of `POST /api/words/{id}/items`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemBody {
    /// `czech`, `accepted`, `confusable`, `form`.
    kind: String,
    value: String,
    /// Forms only (default true).
    common: Option<bool>,
}

/// `POST /api/words/{id}/items`: add a Czech equivalent, an accepted
/// answer (the owner's own), a confusable or a form.
async fn add_item(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ItemBody>, JsonRejection>,
) -> Response {
    const SHAPE: &str = "The body must be {\"kind\": \"czech\"|\"accepted\"|\"confusable\"|\"form\", \
                         \"value\": \"…\", \"common\"?: true|false}.";
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => return rejected_body(&rejection, SHAPE),
    };
    let Some(kind) = ListKind::parse(&body.kind) else {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "invalid", SHAPE);
    };
    let id = WordId::new(id);
    edit_then_show(&state, false, move |tx, now| {
        let ((), undo) = workflows::with_undo(tx, id, now, |tx| {
            workflows::add_word_item(tx, id, kind, &body.value, body.common, now)
        })?;
        Ok(Edited::undoable(id, false, undo))
    })
    .await
}

/// Body of `DELETE /api/words/{id}/items`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveItemBody {
    /// `czech`, `accepted`, `confusable`, `form`.
    kind: String,
    /// The item's `key` from the word detail. Sent in the body, never in
    /// the path: a key may be `.` or `..`, which URL normalisation removes.
    key: String,
}

/// `DELETE /api/words/{id}/items` `{kind, key}`: remove a list item (by its
/// key) for good: no later pack adds it again.
async fn remove_item(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<RemoveItemBody>, JsonRejection>,
) -> Response {
    const SHAPE: &str = "The body must be {\"kind\": \"czech\"|\"accepted\"|\"confusable\"|\"form\", \
                         \"key\": \"…\"}.";
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => return rejected_body(&rejection, SHAPE),
    };
    let Some(kind) = ListKind::parse(&body.kind) else {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "invalid", SHAPE);
    };
    let id = WordId::new(id);
    edit_then_show(&state, false, move |tx, now| {
        let ((), undo) = workflows::with_undo(tx, id, now, |tx| {
            workflows::remove_word_item(tx, id, kind, &body.key, now)
        })?;
        Ok(Edited::undoable(id, false, undo))
    })
    .await
}

/// Body of `DELETE /api/words/{id}/spellings`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpellingBody {
    /// The spelling's `key` from the word detail's `pack_spellings`.
    key: String,
}

/// `DELETE /api/words/{id}/spellings` `{key}`: forget a spelling a pack
/// used for the word; a later pack with it adds a new word.
async fn remove_spelling(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<SpellingBody>, JsonRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    let key = match body {
        Ok(Json(body)) => body.key,
        Err(rejection) => {
            return rejected_body(&rejection, "The body must be {\"key\": \"…\"}.");
        }
    };
    let id = WordId::new(id);
    edit_then_show(&state, false, move |tx, now| {
        let ((), undo) = workflows::with_undo(tx, id, now, |tx| {
            workflows::remove_pack_spelling(tx, id, &key, now)
        })?;
        Ok(Edited::undoable(id, false, undo))
    })
    .await
}

/// Body of `POST /api/conflicts/{id}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConflictBody {
    #[serde(rename = "use")]
    choice: ConflictChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConflictChoice {
    /// *Use new*: the pack's value.
    Incoming,
    /// *Keep*: the stored value; the suggestion is dismissed.
    Keep,
}

/// `POST /api/conflicts/{id}` `{use: "incoming"|"keep"}` (R-03).
async fn resolve_conflict(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ConflictBody>, JsonRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    let choice = match body {
        Ok(Json(body)) => body.choice,
        Err(rejection) => {
            return rejected_body(
                &rejection,
                "The body must be {\"use\": \"incoming\"|\"keep\"}.",
            );
        }
    };
    edit_then_show(&state, true, move |tx, now| {
        let word = workflows::conflict_word(tx, id)?;
        let (_, undo) = workflows::with_undo(tx, word, now, |tx| {
            workflows::resolve_conflict(tx, id, choice == ConflictChoice::Incoming, now)
        })?;
        Ok(Edited::undoable(word, false, undo))
    })
    .await
}

/// Body of `PATCH /api/sentences/{id}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SentenceBody {
    en: String,
    cs: String,
}

/// `PATCH /api/sentences/{id}` `{en, cs}`: correct a sentence (R-25),
/// checked with the `[[…]]` rule; its audio is made again.
async fn patch_sentence(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<SentenceBody>, JsonRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    let body = match body {
        Ok(Json(body)) => body,
        Err(rejection) => {
            return rejected_body(
                &rejection,
                "The body must be {\"en\": \"… [[word]] …\", \"cs\": \"… [[slovo]] …\"}.",
            );
        }
    };
    edit_then_show(&state, true, move |tx, now| {
        let word = workflows::sentence_word(tx, id)?;
        let (_, undo) = workflows::with_undo(tx, word, now, |tx| {
            workflows::edit_sentence(tx, id, &body.en, &body.cs, now)
        })?;
        Ok(Edited::undoable(word, false, undo))
    })
    .await
}

/// Body of `POST /api/sentences/{id}/hidden`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HiddenBody {
    hidden: bool,
}

/// `POST /api/sentences/{id}/hidden` `{hidden}`: hide a sentence from
/// practice, or show it again (R-25).
async fn hide_sentence(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<HiddenBody>, JsonRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    let hidden = match body {
        Ok(Json(body)) => body.hidden,
        Err(rejection) => {
            return rejected_body(&rejection, "The body must be {\"hidden\": true|false}.");
        }
    };
    edit_then_show(&state, true, move |tx, now| {
        let word = workflows::sentence_word(tx, id)?;
        let (_, undo) = workflows::with_undo(tx, word, now, |tx| {
            workflows::hide_sentence(tx, id, hidden, now)
        })?;
        Ok(Edited::undoable(word, false, undo))
    })
    .await
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
    #[error(transparent)]
    Export(#[from] ExportError),
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
            SessionError::CardChangedSince => conflict("card_changed"),
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
        ApiError::Db(_) | ApiError::Settings(_) | ApiError::Stats(_) | ApiError::Export(_) => {
            internal_error(err)
        }
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

/// `GET /api/progress`: the Progress view (R-72, design §7.1 screen 9).
async fn progress(State(state): State<AppState>) -> Response {
    let day = state.moment().day;
    respond(
        state
            .db
            .read(move |conn| Ok::<_, ApiError>(stats::progress(conn, day)?))
            .await,
    )
}

/// Headers of an export download: a file to save (never shown inline,
/// never cached: it holds the owner's data).
fn download_headers(response: &mut Response, file_name: &str) {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{file_name}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
}

/// `GET /api/export/feedback`: the feedback for Claude (R-57, design §8.1)
/// as a JSON download (`vocab-feedback-<study day>.json`).
async fn export_feedback(State(state): State<AppState>) -> Response {
    let day = state.moment().day;
    let result = state
        .db
        .read(move |conn| Ok::<_, ApiError>(exports::feedback(conn, day)?))
        .await;
    match result.and_then(|export| {
        serde_json::to_vec_pretty(&export).map_err(|e| ApiError::Export(e.into()))
    }) {
        Ok(mut bytes) => {
            bytes.push(b'\n');
            let mut response = Body::from(bytes).into_response();
            download_headers(&mut response, &format!("vocab-feedback-{day}.json"));
            response
        }
        Err(err) => api_error(&err),
    }
}

/// Bytes of the full export sent to the client at a time.
const EXPORT_CHUNK_BYTES: usize = 64 * 1024;
/// Chunks waiting for a slow client (memory per export: about this many
/// chunks, whatever the export's size).
const EXPORT_CHUNKS_QUEUED: usize = 4;

/// Send the spooled export `file` to the body `sender` chunk by chunk
/// (blocking: it runs on the blocking pool). A chunk the client does not
/// take within `idle` (a stalled download) stops the download; a download
/// that keeps moving is never cut off, however long it takes.
fn send_spooled(
    mut file: std::fs::File,
    sender: &mut http_body_util::channel::Sender<axum::body::Bytes, std::io::Error>,
    runtime: &tokio::runtime::Handle,
    idle: Duration,
) -> std::io::Result<()> {
    let mut buffer = vec![0; EXPORT_CHUNK_BYTES];
    loop {
        let n = match std::io::Read::read(&mut file, &mut buffer) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        let chunk = axum::body::Bytes::copy_from_slice(&buffer[..n]);
        match runtime.block_on(tokio::time::timeout(idle, sender.send_data(chunk))) {
            Ok(Ok(())) => {}
            // The client went away.
            Ok(Err(_)) => return Err(std::io::ErrorKind::BrokenPipe.into()),
            Err(_) => return Err(std::io::ErrorKind::TimedOut.into()),
        }
    }
}

/// `GET /api/export/full`: everything as one JSON download (R-86, design
/// §8.1). A separate read-only connection writes one consistent snapshot
/// to an anonymous file next to the database (`507 no_space` when that
/// would leave too little free), the snapshot is given back at once, and
/// the file is then streamed: a slow download holds neither the snapshot
/// (WAL checkpoints go on) nor the shared connection. One export at a time
/// (`503 busy` otherwise); a download that stalls for the idle timeout is
/// broken off (never a complete-looking file) and gives the turn back.
async fn export_full(State(state): State<AppState>) -> Response {
    let Ok(turn) = Arc::clone(&state.export_turn).try_acquire_owned() else {
        return busy(
            "busy",
            "An export is already running. Try again when it has finished.",
        );
    };
    let now = state.moment();
    let db = state.db.clone();
    let spooled = state
        .db
        .read_detached(move |conn| {
            let (file, free) = db.scratch_file().map_err(ExportError::from)?;
            exports::spool_full(conn, file, free, &now).map_err(ApiError::from)
        })
        .await;
    let spooled = match spooled {
        Ok(spooled) => spooled,
        Err(ApiError::Export(ExportError::NoSpace)) => {
            return error_response(
                StatusCode::INSUFFICIENT_STORAGE,
                "no_space",
                "The server has too little free disk space to prepare the export.",
            );
        }
        Err(err) => return api_error(&err),
    };
    let (mut sender, body) =
        http_body_util::channel::Channel::<axum::body::Bytes, std::io::Error>::new(
            EXPORT_CHUNKS_QUEUED,
        );
    let runtime = tokio::runtime::Handle::current();
    let idle = state.export_idle;
    tokio::task::spawn_blocking(move || {
        let _turn = turn;
        match send_spooled(spooled.file, &mut sender, &runtime, idle) {
            // Dropping the sender ends the body normally.
            Ok(()) => drop(sender),
            Err(err) => {
                tracing::warn!(error = %err, "full export download stopped");
                // The client sees a broken download, never a complete-looking
                // file.
                sender.abort(err);
            }
        }
    });
    let mut response = Body::new(body).into_response();
    if let Ok(len) = HeaderValue::from_str(&spooled.len.to_string()) {
        response.headers_mut().insert(header::CONTENT_LENGTH, len);
    }
    download_headers(&mut response, &format!("vocab-export-{}.json", now.day));
    response
}

/// Body of `POST /api/sessions`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartBody {
    scope_source_id: Option<i64>,
    kind: StartKind,
    #[serde(default)]
    add_new_anyway: bool,
    /// `more_new` only.
    count: Option<u32>,
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
                "The body must be JSON {\"kind\": \"start\"|\"continue\"|\"extra\"|\"more_new\", \
                 \"scope_source_id\"?, \"add_new_anyway\"?, \"count\"?}.",
            );
        }
    };
    let request = StartRequest {
        scope: body.scope_source_id.map(SourceId::new),
        kind: body.kind,
        add_new_anyway: body.add_new_anyway,
        count: body.count,
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
    let voice_changed = patch.voice.is_some();
    let result = state
        .db
        .write(move |tx| Ok::<_, ApiError>(settings::update(tx, &patch)?))
        .await;
    if voice_changed
        && result.is_ok()
        && let Some(audio) = &state.audio
    {
        audio.wake();
    }
    respond(result)
}

// ── audio (design §10.3) ────────────────────────────────────────────────────

/// `GET /api/audio/status`: clip counts for Settings.
async fn audio_status(State(state): State<AppState>) -> Response {
    let Some(audio) = &state.audio else {
        return Json(serde_json::json!({ "enabled": false })).into_response();
    };
    match audio.status().await {
        Ok(status) => {
            let mut value = serde_json::to_value(status).unwrap_or_default();
            value["enabled"] = serde_json::Value::Bool(true);
            Json(value).into_response()
        }
        Err(err) => internal_error(&err),
    }
}

/// `POST /api/audio/retry`: failed clips are queued again.
async fn audio_retry(State(state): State<AppState>) -> Response {
    let Some(audio) = &state.audio else {
        return error_response(
            StatusCode::CONFLICT,
            "audio_disabled",
            "Neural audio is not configured.",
        );
    };
    match audio.retry_failed().await {
        Ok(count) => Json(serde_json::json!({ "retried": count })).into_response(),
        Err(err) => internal_error(&err),
    }
}

/// `GET /api/audio/words/{id}`: the clips of a word and its sentences.
async fn audio_word(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> Response {
    let Ok(Path(id)) = id else {
        return not_found().await;
    };
    let Some(audio) = &state.audio else {
        return Json(serde_json::json!({ "enabled": false })).into_response();
    };
    match audio.word_clips(WordId::new(id)).await {
        Ok(Some(clips)) => {
            let mut value = serde_json::to_value(clips).unwrap_or_default();
            value["enabled"] = serde_json::Value::Bool(true);
            Json(value).into_response()
        }
        Ok(None) => not_found().await,
        Err(err) => internal_error(&err),
    }
}

/// A byte range of a clip: `Range: bytes=a-b | a- | -n` (one range only;
/// anything else is answered with the whole clip, as HTTP allows).
fn byte_range(header: Option<&HeaderValue>, len: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = header?.to_str().ok()?.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let range = match (start.trim(), end.trim()) {
        ("", suffix) => {
            let n: u64 = suffix.parse().ok()?;
            if n == 0 || len == 0 {
                return Some(Err(()));
            }
            (len.saturating_sub(n), len - 1)
        }
        (start, "") => (start.parse().ok()?, len.saturating_sub(1)),
        (start, end) => {
            let (start, end): (u64, u64) = (start.parse().ok()?, end.parse().ok()?);
            if end < start {
                return None;
            }
            (start, end.min(len.saturating_sub(1)))
        }
    };
    Some(if range.0 < len { Ok(range) } else { Err(()) })
}

/// `GET /api/audio/clips/{hash}.mp3`: a clip file. Clips never change (the
/// hash names the content): private, immutable caching, ETag = hash, and
/// byte ranges (iOS Safari plays `<audio>` only with them).
async fn audio_clip(
    State(state): State<AppState>,
    file: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
) -> Response {
    let Ok(Path(file)) = file else {
        return not_found().await;
    };
    let (Some(audio), Some(hash)) = (&state.audio, file.strip_suffix(".mp3")) else {
        return not_found().await;
    };
    if !Audio::is_clip_hash(hash) {
        return not_found().await;
    }
    let etag = format!("\"{hash}\"");
    let cache = |response: &mut Response| {
        let headers = response.headers_mut();
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=31536000, immutable"),
        );
        if let Ok(value) = HeaderValue::from_str(&etag) {
            headers.insert(header::ETAG, value);
        }
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    };
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        cache(&mut response);
        return response;
    }
    let audio = audio.clone();
    let owned = hash.to_owned();
    let bytes = tokio::task::spawn_blocking(move || -> std::io::Result<Option<Vec<u8>>> {
        let Some(mut file) = audio.open_clip(&owned)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    })
    .await;
    let bytes = match bytes {
        Ok(Ok(Some(bytes))) => bytes,
        Ok(Ok(None)) => return not_found().await,
        Ok(Err(err)) => return internal_error(&err),
        Err(err) => return internal_error(&err),
    };
    let len = bytes.len() as u64;
    let mut response = match byte_range(headers.get(header::RANGE), len) {
        None => (StatusCode::OK, bytes).into_response(),
        Some(Ok((start, end))) => {
            let part = bytes
                [usize::try_from(start).unwrap_or(0)..=usize::try_from(end).unwrap_or(0)]
                .to_vec();
            let mut response = (StatusCode::PARTIAL_CONTENT, part).into_response();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")) {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            response
        }
        Some(Err(())) => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{len}")) {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            return response;
        }
    };
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/mpeg"));
    cache(&mut response);
    response
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
    let forbidden_origin = || {
        error_response(
            StatusCode::FORBIDDEN,
            "forbidden_origin",
            "The request does not come from this app.",
        )
    };
    if request.uri().path().starts_with("/api/")
        && Auth::check_fetch_site(request.headers()).is_err()
    {
        return forbidden_origin();
    }
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
        Err(CsrfRejection::Origin) => return forbidden_origin(),
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

/// At most `per_second` request log lines a second; what is over is counted
/// and reported in one line when the next second starts.
#[derive(Debug)]
struct LogBudget {
    per_second: u32,
    state: Mutex<LogWindow>,
}

#[derive(Debug)]
struct LogWindow {
    started: Instant,
    written: u32,
    suppressed: u64,
}

/// What [`LogBudget::admit`] decided for one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Admission {
    /// Write the line.
    write: bool,
    /// Lines not written in the second that just ended (to report once).
    suppressed_before: u64,
}

impl LogBudget {
    fn new(per_second: u32) -> Self {
        Self {
            per_second,
            state: Mutex::new(LogWindow {
                started: Instant::now(),
                written: 0,
                suppressed: 0,
            }),
        }
    }

    fn admit(&self, now: Instant) -> Admission {
        let mut window = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut suppressed_before = 0;
        if now.duration_since(window.started) >= Duration::from_secs(1) {
            suppressed_before = window.suppressed;
            *window = LogWindow {
                started: now,
                written: 0,
                suppressed: 0,
            };
        }
        let write = window.written < self.per_second;
        if write {
            window.written += 1;
        } else {
            window.suppressed += 1;
        }
        Admission {
            write,
            suppressed_before,
        }
    }
}

async fn log_request(
    State(budget): State<Arc<LogBudget>>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    // The route template (e.g. `/api/words/{id}`), never the concrete path or query.
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "(unmatched)".to_owned(), |path| path.as_str().to_owned());
    let response = next.run(request).await;
    let status = response.status().as_u16();
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if route == HEALTHZ_PATH {
        // Asked every few seconds by the container runtime: noise at info.
        tracing::debug!(method = %method, route = %route, status, duration_ms, "request");
        return response;
    }
    let admission = budget.admit(Instant::now());
    if admission.suppressed_before > 0 {
        tracing::warn!(
            suppressed = admission.suppressed_before,
            "suppressed {} request log lines",
            admission.suppressed_before
        );
    }
    if admission.write {
        tracing::info!(method = %method, route = %route, status, duration_ms, "request");
    }
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

/// How the body of a request to a public route must arrive.
#[derive(Debug, Clone, Copy)]
struct BodyDeadline {
    within: Duration,
    max_bytes: usize,
}

/// Read the whole (small) body before the handler runs, within the
/// deadline: an unauthenticated client that sends a request head and then
/// stalls loses its slot after seconds, not after the request timeout.
async fn buffer_body_in_time(
    State(deadline): State<BodyDeadline>,
    request: Request,
    next: Next,
) -> Response {
    let (parts, body) = request.into_parts();
    let read = axum::body::to_bytes(body, deadline.max_bytes);
    match tokio::time::timeout(deadline.within, read).await {
        Ok(Ok(bytes)) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        // Over the limit (or a broken body): the same answer as from the
        // body limit of the other routes.
        Ok(Err(_)) => error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "The request is too large.",
        ),
        Err(_) => body_timeout(),
    }
}

/// The request slots (R-83): `general` ones for anyone and a few
/// `reserved` ones that only requests of a known live session may take, so
/// requests without a session can never occupy every slot.
#[derive(Debug, Clone)]
struct Slots {
    general: Arc<Semaphore>,
    reserved: Arc<Semaphore>,
    /// How long a request waits for a slot before it is refused.
    wait: Duration,
    /// Who knows the live sessions (`None`: nobody gets a reserved slot).
    sessions: Option<(Arc<Auth>, Arc<dyn Clock>)>,
}

impl Slots {
    fn new(limits: &Limits, sessions: Option<(Arc<Auth>, Arc<dyn Clock>)>) -> Self {
        let reserved = limits
            .reserved_session_slots
            .min(limits.max_concurrent_requests.saturating_sub(1));
        Self {
            general: Arc::new(Semaphore::new(limits.max_concurrent_requests - reserved)),
            reserved: Arc::new(Semaphore::new(reserved)),
            wait: limits.slot_wait(),
            sessions,
        }
    }

    /// A slot for a request with these headers, or `None` when none became
    /// free in time.
    async fn acquire(&self, headers: &HeaderMap) -> Option<OwnedSemaphorePermit> {
        if let Ok(permit) = Arc::clone(&self.general).try_acquire_owned() {
            return Some(permit);
        }
        let known_session = self
            .sessions
            .as_ref()
            .is_some_and(|(auth, clock)| auth.has_known_session(headers, clock.now()));
        let general = Arc::clone(&self.general).acquire_owned();
        let permit = async {
            if known_session {
                tokio::select! {
                    permit = general => permit,
                    permit = Arc::clone(&self.reserved).acquire_owned() => permit,
                }
            } else {
                general.await
            }
        };
        tokio::time::timeout(self.wait, permit).await.ok()?.ok()
    }
}

async fn limit_concurrency(State(slots): State<Slots>, request: Request, next: Next) -> Response {
    match slots.acquire(request.headers()).await {
        Some(_permit) => next.run(request).await,
        None => busy("overloaded", "The server is busy. Try again in a moment."),
    }
}

/// Open connections per peer address (R-83).
#[derive(Debug)]
struct PeerLimit {
    max: usize,
    open: Mutex<HashMap<IpAddr, usize>>,
}

/// One counted connection of a peer; dropping it frees the place.
#[derive(Debug)]
struct PeerConnection {
    limit: Arc<PeerLimit>,
    peer: IpAddr,
}

impl PeerLimit {
    fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            max,
            open: Mutex::new(HashMap::new()),
        })
    }

    fn counts(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, usize>> {
        self.open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Count a new connection of `peer`, unless it has `max` open already.
    fn admit(self: &Arc<Self>, peer: IpAddr) -> Option<PeerConnection> {
        // An IPv4 client on a dual-stack socket is the same peer.
        let peer = peer.to_canonical();
        let mut open = self.counts();
        let count = open.entry(peer).or_insert(0);
        if *count >= self.max {
            return None;
        }
        *count += 1;
        Some(PeerConnection {
            limit: Arc::clone(self),
            peer,
        })
    }
}

impl Drop for PeerConnection {
    fn drop(&mut self) {
        let mut open = self.limit.counts();
        if let Some(count) = open.get_mut(&self.peer) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.peer);
            }
        }
    }
}

/// What a connection over the per-peer cap is told before it is closed.
const TOO_MANY_CONNECTIONS: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 5\r\n\
Connection: close\r\nContent-Length: 0\r\n\r\n";

/// Serve `app` on `listener` until `shutdown` resolves; then stop accepting
/// and give open requests up to the request timeout to finish.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    limits: &Limits,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let connections = Arc::new(Semaphore::new(limits.max_connections));
    let peers = PeerLimit::new(limits.max_connections_per_peer);
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
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(err) => {
                    // E.g. EMFILE: back off instead of spinning or exiting.
                    tracing::warn!(error = %err, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let Some(peer_connection) = peers.admit(peer.ip()) else {
            // One address may not hold every connection. Best effort: the
            // answer fits the socket buffer of a fresh connection.
            let _ = stream.try_write(TOO_MANY_CONNECTIONS);
            continue;
        };
        // Per connection: a head must arrive within the head deadline of its
        // first byte; between requests the connection may idle for the
        // keep-alive time (`keep_alive`). Idle connections of one address
        // stay bounded by the per-peer cap.
        let (stream, timer) = keep_alive::wrap(
            stream,
            limits.header_read_timeout(),
            limits.keep_alive_idle(),
        );
        let mut http1 = hyper::server::conn::http1::Builder::new();
        http1
            .timer(timer)
            .header_read_timeout(limits.header_read_timeout())
            .keep_alive(true);
        let connection =
            http1.serve_connection(TokioIo::new(stream), TowerToHyperService::new(app.clone()));
        let connection = graceful.watch(connection);
        tokio::spawn(async move {
            if let Err(err) = connection.await {
                tracing::debug!(error = %err, "connection closed with error");
            }
            drop(peer_connection);
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
        harden(
            with_limits(routes, body_bytes, timeout),
            Router::new(),
            Slots::new(limits, None),
        )
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

    /// B4: at most `per_second` request lines a second; the rest are
    /// counted and reported once when the next second starts.
    #[test]
    fn r83_request_log_lines_are_rate_limited() {
        let budget = LogBudget::new(20);
        let start = Instant::now();
        let mut written = 0;
        for i in 0..1000u64 {
            let admission = budget.admit(start + Duration::from_micros(i * 900));
            assert_eq!(admission.suppressed_before, 0);
            written += u32::from(admission.write);
        }
        assert_eq!(written, 20, "20 of 1000 lines within one second");
        // The first line of the next second reports what was left out, once.
        let next = budget.admit(start + Duration::from_millis(1000));
        assert_eq!(
            next,
            Admission {
                write: true,
                suppressed_before: 980
            }
        );
        let after = budget.admit(start + Duration::from_millis(1001));
        assert_eq!(
            after,
            Admission {
                write: true,
                suppressed_before: 0
            }
        );
        // A quiet second has nothing to report.
        let quiet = budget.admit(start + Duration::from_millis(5000));
        assert_eq!(quiet.suppressed_before, 0);
    }

    /// B1: the per-peer connection cap counts per address.
    #[test]
    fn r83_connections_are_counted_per_peer_address() {
        let limit = PeerLimit::new(2);
        let proxy: IpAddr = "192.0.2.7".parse().expect("ip");
        let mapped: IpAddr = "::ffff:192.0.2.7".parse().expect("ip");
        let local: IpAddr = Ipv4Addr::LOCALHOST.into();
        let first = limit.admit(proxy).expect("first");
        let second = limit
            .admit(mapped)
            .expect("the same peer over a dual-stack socket");
        assert!(limit.admit(proxy).is_none(), "the peer is at its cap");
        // Another address (the health check on loopback) is not affected.
        let health = limit.admit(local).expect("another peer");
        drop(first);
        let third = limit.admit(proxy).expect("a place is free again");
        assert!(limit.admit(proxy).is_none());
        drop((second, third, health));
        assert!(limit.counts().is_empty(), "nothing is kept for gone peers");
    }

    /// B1: a request waits only briefly for a slot; routes outside the
    /// pool (the health check) never wait.
    #[tokio::test]
    async fn r83_requests_beyond_the_slots_are_refused_after_a_short_wait() {
        let limits = Limits {
            max_concurrent_requests: 2,
            reserved_session_slots: 1,
            slot_wait_ms: 100,
            request_timeout_secs: 5,
            ..Limits::default()
        };
        let health = Router::new().route("/health", get(|| async { "ok" }));
        let routes = Router::new().route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(600)).await;
                "done"
            }),
        );
        let timeout = TimeoutPolicy {
            duration: limits.request_timeout(),
            message: TIMEOUT_MESSAGE,
        };
        // Nobody is known as a session here: one general slot in all.
        let router = harden(
            with_limits(routes, limits.body_bytes, timeout),
            health,
            Slots::new(&limits, None),
        );
        let request = |uri: &str| {
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request")
        };
        let slow = tokio::spawn(router.clone().oneshot(request("/slow")));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let started = Instant::now();
        let refused = router
            .clone()
            .oneshot(request("/slow"))
            .await
            .expect("response");
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            refused
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("5")
        );
        assert!(
            refused
                .headers()
                .contains_key(header::CONTENT_SECURITY_POLICY)
        );
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(90) && waited < Duration::from_millis(400),
            "{waited:?}"
        );
        let body = to_bytes(refused.into_body(), 1024).await.expect("body");
        assert!(String::from_utf8_lossy(&body).contains("\"overloaded\""));

        let started = Instant::now();
        let healthy = router
            .clone()
            .oneshot(request("/health"))
            .await
            .expect("response");
        assert_eq!(healthy.status(), StatusCode::OK);
        assert!(started.elapsed() < Duration::from_millis(50));

        assert_eq!(
            slow.await.expect("join").expect("response").status(),
            StatusCode::OK
        );
        // The slot is free again.
        let again = router.oneshot(request("/slow")).await.expect("response");
        assert_eq!(again.status(), StatusCode::OK);
    }
}
