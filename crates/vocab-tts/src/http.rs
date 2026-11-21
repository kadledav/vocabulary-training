//! HTTP API of the TTS service (design §10.2; docs/design/tts-spike.md has
//! the differences to the design).
//!
//! * `POST /v1/speak` `{"text": "…", "voice": "af_heart", "speed": 1.0}`
//!   (`voice` and `speed` optional) → `200 audio/mpeg` with `X-Duration-Ms`,
//!   `X-Loudness-Lufs` (of the normalised audio before encoding) and
//!   `X-Voice`, `X-Approximate` (words whose pronunciation is a guess: from
//!   the fallback dictionary or spelled letter by letter), `X-Words-Spelled`,
//!   `X-Frontend` and `X-Engine` (versions, for the app's clip identity);
//!   `400` invalid request/text/voice/speed, `413` body over
//!   4 KiB, `415` not JSON, `422` nothing speakable, `500`
//!   `synthesis_failed` or `engine_crashed` (the engine process died on
//!   this text), `503` busy or timeout.
//! * `GET /v1/voices` → the voices and their accents, and the front-end and
//!   engine versions (the app's clip identity).
//! * `GET /healthz` → `200` once the start self-test spoke (before the
//!   listener opens), `503` `engine_failed` while the engine process cannot
//!   be started (until a start succeeds).
//!
//! No authentication: the container is reachable only from the app, on an
//! internal network without a route anywhere else (R-83). Every input is
//! validated anyway; every request has a body limit and a timeout, there is
//! a cap on concurrent requests and on open connections, and a header read
//! timeout. Request logs carry method, route, status and duration; a speak
//! log carries sizes and the text's fingerprint, never the text (R-24).

use std::future::Future;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::engine::ENGINE_VERSION;
use crate::phonemize::FRONTEND_VERSION;
use crate::speaker::{SpeakError, Speaker};
use crate::speech::{Accent, SpeakText, Speed, Voice};

const HEALTHZ_PATH: &str = "/healthz";
const SPEAK_PATH: &str = "/v1/speak";
const VOICES_PATH: &str = "/v1/voices";
/// A 600-character text is at most 2400 bytes of UTF-8 (JSON escapes of
/// control characters cannot occur in a valid text).
const BODY_BYTES: usize = 4 * 1024;
/// The longest accepted text takes about 25 s at one thread of a fast CPU
/// (int8 model), and a NAS CPU limited to half a core may need ten times
/// that, plus loading the model: slow but working synthesis must finish.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
/// Open requests get this long to finish when the service stops.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECTIONS: usize = 32;
const MAX_CONCURRENT_REQUESTS: usize = 16;
/// `Retry-After` of a busy answer, seconds.
const BUSY_RETRY_SECS: u64 = 5;

/// The application router.
pub fn app(speaker: Arc<Speaker>) -> Router {
    let slots = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));
    Router::new()
        .route(HEALTHZ_PATH, get(healthz))
        .route(VOICES_PATH, get(voices))
        .route(SPEAK_PATH, post(speak))
        .with_state(speaker)
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(BODY_BYTES))
        .layer(middleware::from_fn(limit_time))
        .layer(middleware::from_fn_with_state(slots, limit_concurrency))
        .layer(middleware::from_fn(log_request))
        .layer(middleware::from_fn(response_headers))
}

/// JSON error body: `{"error": {"code", "message"}}` (as vocab-server).
fn error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = serde_json::json!({ "error": { "code": code, "message": message } });
    (status, Json(body)).into_response()
}

async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not_found", "Not found")
}

async fn healthz(State(speaker): State<Arc<Speaker>>) -> Response {
    if speaker.healthy() {
        Json(serde_json::json!({ "status": "ok" })).into_response()
    } else {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "engine_failed",
            "the engine cannot be started (see the log)",
        )
    }
}

async fn voices() -> Response {
    let voices: Vec<_> = Voice::ALL
        .into_iter()
        .map(|voice| {
            let accent = match voice.accent() {
                Accent::Us => "en-US",
                Accent::Gb => "en-GB",
            };
            serde_json::json!({ "id": voice.id(), "accent": accent, "default": voice == Voice::DEFAULT })
        })
        .collect();
    Json(serde_json::json!({
        "voices": voices,
        "frontend": FRONTEND_VERSION,
        "engine": ENGINE_VERSION,
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpeakRequest {
    text: String,
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    speed: Option<f64>,
}

/// The body must be a JSON object (serde would also accept an array for a
/// struct), with exactly the fields of [`SpeakRequest`].
async fn speak(
    State(speaker): State<Arc<Speaker>>,
    body: Result<Json<serde_json::Map<String, serde_json::Value>>, JsonRejection>,
) -> Response {
    let request =
        match body.map(|Json(object)| serde_json::from_value::<SpeakRequest>(object.into())) {
            Ok(Ok(request)) => request,
            Ok(Err(_)) => return invalid_request(),
            Err(rejection) => return rejected(&rejection),
        };
    let text = match SpeakText::new(&request.text) {
        Ok(text) => text,
        Err(err) => {
            let status = if err.unprocessable() {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::BAD_REQUEST
            };
            return error(status, err.code(), &err.to_string());
        }
    };
    let voice = match request.voice.as_deref() {
        None => Voice::DEFAULT,
        Some(id) => match Voice::parse(id) {
            Some(voice) => voice,
            None => {
                let known: Vec<&str> = Voice::ALL.iter().map(|v| v.id()).collect();
                let message = format!("unknown voice; use one of {}", known.join(", "));
                return error(StatusCode::BAD_REQUEST, "unknown_voice", &message);
            }
        },
    };
    let speed = match request.speed.map_or(Ok(Speed::NORMAL), Speed::new) {
        Ok(speed) => speed,
        Err(err) => return error(StatusCode::BAD_REQUEST, "invalid_speed", &err.to_string()),
    };
    let fingerprint = text.fingerprint();
    let chars = text.char_count();
    match speaker.speak(text, voice, speed).await {
        Ok(spoken) => {
            tracing::info!(
                text_chars = chars,
                text_sha256 = %fingerprint,
                voice = voice.id(),
                phonemes = spoken.phoneme_symbols,
                derived = spoken.tally.derived,
                fallback = spoken.tally.fallback,
                spelled_out = spoken.tally.spelled,
                audio_ms = spoken.clip.duration_ms(),
                loudness_lufs = format!("{:.1}", spoken.clip.loudness_lufs()),
                mp3_bytes = spoken.clip.mp3().len(),
                synthesis_ms = spoken.synthesis_ms,
                postprocess_ms = spoken.postprocess_ms,
                "spoke"
            );
            let duration = spoken.clip.duration_ms();
            let loudness = format!("{:.1}", spoken.clip.loudness_lufs());
            let tally = spoken.tally;
            let mut response = spoken.clip.into_mp3().into_response();
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/mpeg"));
            headers.insert("x-duration-ms", HeaderValue::from(duration));
            if let Ok(value) = HeaderValue::from_str(&loudness) {
                headers.insert("x-loudness-lufs", value);
            }
            headers.insert("x-voice", HeaderValue::from_static(voice.id()));
            headers.insert("x-approximate", HeaderValue::from(tally.approximate()));
            headers.insert("x-words-spelled", HeaderValue::from(tally.spelled));
            headers.insert("x-frontend", HeaderValue::from_static(FRONTEND_VERSION));
            headers.insert("x-engine", HeaderValue::from_static(ENGINE_VERSION));
            response
        }
        Err(SpeakError::NothingToSay) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "nothing_to_say",
            "nothing in the text can be spoken",
        ),
        Err(SpeakError::TooLong(_)) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "too_long_to_speak",
            "the text would take too long to speak (numbers or letters spelled out)",
        ),
        Err(SpeakError::Cancelled) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "timeout",
            "the request took too long",
        ),
        Err(SpeakError::Busy) => {
            let mut response = error(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                "the engine is busy; retry later",
            );
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(BUSY_RETRY_SECS));
            response
        }
        Err(err @ SpeakError::Crashed(_)) => {
            tracing::error!(error = %err, text_chars = chars, text_sha256 = %fingerprint, voice = voice.id(), "synthesis crashed");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "engine_crashed",
                "the engine process died while speaking this text",
            )
        }
        Err(err) => {
            tracing::error!(error = %err, text_chars = chars, text_sha256 = %fingerprint, voice = voice.id(), "synthesis failed");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "synthesis_failed",
                "synthesis failed",
            )
        }
    }
}

/// A body that is not a valid request. The message never echoes the body
/// (serde's messages can quote values, i.e. the text).
fn rejected(rejection: &JsonRejection) -> Response {
    match rejection.status() {
        StatusCode::PAYLOAD_TOO_LARGE => error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "the request body is larger than 4 KiB",
        ),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "send Content-Type: application/json",
        ),
        _ => invalid_request(),
    }
}

fn invalid_request() -> Response {
    error(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "expected a JSON object {\"text\": string, \"voice\"?: string, \"speed\"?: number} without other fields",
    )
}

async fn response_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    response
}

async fn log_request(request: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
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

async fn limit_time(request: Request, next: Next) -> Response {
    match tokio::time::timeout(REQUEST_TIMEOUT, next.run(request)).await {
        Ok(response) => response,
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "timeout",
            "the request took too long",
        ),
    }
}

async fn limit_concurrency(
    State(slots): State<Arc<Semaphore>>,
    request: Request,
    next: Next,
) -> Response {
    match slots.acquire().await {
        Ok(_permit) => next.run(request).await,
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded",
            "shutting down",
        ),
    }
}

/// Serve `app` on `listener` until `shutdown` resolves, with a header read
/// timeout (which also closes idle keep-alive connections) and a cap on
/// open connections; then give open requests a few seconds.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let mut http1 = hyper::server::conn::http1::Builder::new();
    http1
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT)
        .keep_alive(true);
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let graceful = GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
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
                    tracing::warn!(error = %err, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let connection = graceful.watch(
            http1.serve_connection(TokioIo::new(stream), TowerToHyperService::new(app.clone())),
        );
        tokio::spawn(async move {
            if let Err(err) = connection.await {
                tracing::debug!(error = %err, "connection closed with error");
            }
            drop(permit);
        });
    }
    drop(listener);
    if tokio::time::timeout(SHUTDOWN_WAIT, graceful.shutdown())
        .await
        .is_err()
    {
        tracing::warn!("open connections did not finish in time; closing them");
    }
    Ok(())
}

/// Client side of the container health check: `GET /healthz` on the local
/// listener (the image has no shell or curl). Synchronous, no runtime.
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
