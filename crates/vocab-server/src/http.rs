//! HTTP router and cross-cutting request handling (design §3.2, §8, §9.4).
//!
//! Every response carries the security headers (strict CSP and friends); every
//! request gets a body size limit, a timeout and a slot in a bounded
//! concurrency pool, and is logged with method, path template, status and
//! duration only (never bodies or query values, R-24). Handlers stay thin:
//! parse, call a module, map errors.
//!
//! [`serve`] owns the connection level: a header read timeout (which also
//! closes idle keep-alive connections) and a cap on open connections, so a
//! client that trickles bytes cannot hold sockets forever (slowloris).

use std::future::Future;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::config::Limits;
use crate::db::Db;
use crate::web_assets;

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

/// What the handlers need.
#[derive(Debug, Clone)]
pub struct AppState {
    db: Db,
}

impl AppState {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

/// The complete application router.
pub fn app(state: AppState, limits: &Limits) -> Router {
    let api = Router::new()
        .route(HEALTHZ_PATH, get(healthz))
        .with_state(state);
    harden(Router::new().merge(web_assets::router()).merge(api), limits)
}

/// Wrap `routes` with the request limits, logging and security headers.
/// The last layer added is the outermost, so the headers are set on every
/// response, including timeouts, overload answers and 404s.
fn harden(routes: Router, limits: &Limits) -> Router {
    let slots = Arc::new(Semaphore::new(limits.max_concurrent_requests));
    routes
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(limits.body_bytes))
        .layer(middleware::from_fn_with_state(slots, limit_concurrency))
        .layer(middleware::from_fn_with_state(
            limits.request_timeout(),
            limit_time,
        ))
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

async fn limit_time(State(timeout): State<Duration>, request: Request, next: Next) -> Response {
    match tokio::time::timeout(timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "timeout",
            "The request took too long",
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
    if tokio::time::timeout(limits.request_timeout(), graceful.shutdown())
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
    use axum::body::{Body, Bytes, to_bytes};
    use axum::http::Method;
    use axum::routing::post;
    use tower::ServiceExt;

    use super::*;

    fn test_router(limits: &Limits) -> Router {
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
        harden(routes, limits)
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
