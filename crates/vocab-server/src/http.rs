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
//! [`serve`] owns the connection level: a header read timeout (which also
//! closes idle keep-alive connections) and a cap on open connections, so a
//! client that trickles bytes cannot hold sockets forever (slowloris).

use std::future::Future;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, MatchedPath, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_body_util::BodyExt;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::catalog::{self, SourceId, WordId, WordQuery};
use crate::clock::{Clock, DayRule, Moment, SystemClock};
use crate::config::Limits;
use crate::db::{Db, DbError};
use crate::importer::Via;
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
    ("referrer-policy", "no-referrer"),
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

/// Most rows of the import log returned by `GET /api/import/recent`.
const RECENT_IMPORTS: u32 = 20;

/// What the handlers need.
#[derive(Debug, Clone)]
pub struct AppState {
    db: Db,
    clock: Arc<dyn Clock>,
    /// Where study days begin (config `timezone`, `day_rollover_hour`).
    day_rule: DayRule,
    /// Body limit of the import routes (set by [`app`] from the limits).
    import_body_bytes: usize,
}

impl AppState {
    pub fn new(db: Db, day_rule: DayRule) -> Self {
        Self::with_clock(db, Arc::new(SystemClock), day_rule)
    }

    /// With a controllable clock (tests).
    pub fn with_clock(db: Db, clock: Arc<dyn Clock>, day_rule: DayRule) -> Self {
        Self {
            db,
            clock,
            day_rule,
            import_body_bytes: Limits::default().import_body_bytes,
        }
    }

    /// Now, with its study day.
    fn moment(&self) -> Moment {
        Moment::of(self.clock.now(), &self.day_rule)
    }
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
    let default_routes = Router::new()
        .route(HEALTHZ_PATH, get(healthz))
        .route("/api/import/recent", get(import_recent))
        .route("/api/sources", get(list_sources))
        .route("/api/words", get(list_words))
        .route("/api/words/{id}", get(show_word))
        .route("/api/today", get(today))
        .route("/api/sessions", post(start_session))
        .route("/api/sessions/current", get(current_session))
        .route("/api/sessions/{id}/reviews", post(rate_item))
        .route("/api/sessions/{id}/undo", post(undo_rating))
        .route("/api/sessions/{id}/finish", post(finish_session))
        .route("/api/settings", get(get_settings).patch(patch_settings))
        .with_state(state.clone())
        .merge(web_assets::router());
    let preview = Router::new()
        .route("/api/import/preview", post(import_preview))
        .with_state(state.clone());
    let commit = Router::new()
        .route("/api/import/commit", post(import_commit))
        .with_state(state);
    let routes = with_limits(default_routes, limits.body_bytes, standard)
        .merge(with_limits(
            preview,
            limits.import_body_bytes,
            TimeoutPolicy {
                message: PREVIEW_TIMEOUT_MESSAGE,
                ..standard
            },
        ))
        .merge(with_limits(
            commit,
            limits.import_body_bytes,
            TimeoutPolicy {
                duration: limits.import_commit_timeout(),
                message: IMPORT_TIMEOUT_MESSAGE,
            },
        ));
    harden(routes, limits)
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
    let Ok(Json(body)) = body else {
        return bad_request(
            "The body must be JSON {\"kind\": \"start\"|\"continue\", \"scope_source_id\"?, \"add_new_anyway\"?}.",
        );
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
    let Ok(Json(body)) = body else {
        return bad_request("The body must be a JSON review object (design §8).");
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
    let Ok(Json(patch)) = body else {
        return bad_request("The body must be a JSON object of known settings.");
    };
    respond(
        state
            .db
            .write(move |tx| Ok::<_, ApiError>(settings::update(tx, &patch)?))
            .await,
    )
}

// ── middleware ──────────────────────────────────────────────────────────────

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
    use axum::routing::post;
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
}
