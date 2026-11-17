//! HTTP skeleton: the embedded shell, security headers, `/healthz`.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use tower::ServiceExt;
use vocab_server::config::Limits;
use vocab_server::db::Db;
use vocab_server::http::{self, AppState};

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   media-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; \
                   frame-ancestors 'none'; form-action 'self'";

struct TestApp {
    _dir: tempfile::TempDir,
    router: axum::Router,
}

fn app() -> TestApp {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(dir.path()).expect("db");
    let router = http::app(AppState::new(db), &Limits::default());
    TestApp { _dir: dir, router }
}

async fn get(app: &TestApp, uri: &str) -> Response {
    app.router
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 1 << 20).await.expect("body");
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

fn header<'a>(response: &'a Response, name: &str) -> &'a str {
    response
        .headers()
        .get(name)
        .unwrap_or_else(|| panic!("missing header {name}"))
        .to_str()
        .expect("ascii header")
}

fn assert_security_headers(response: &Response) {
    assert_eq!(header(response, "content-security-policy"), CSP);
    assert_eq!(header(response, "x-content-type-options"), "nosniff");
    assert_eq!(header(response, "referrer-policy"), "no-referrer");
    assert_eq!(
        header(response, "cross-origin-opener-policy"),
        "same-origin"
    );
    assert_eq!(
        header(response, "permissions-policy"),
        "camera=(), microphone=(), geolocation=()"
    );
    assert_eq!(
        header(response, "strict-transport-security"),
        "max-age=31536000"
    );
}

/// Asset URLs referenced by the shell, e.g. `/assets/<build>/js/main.js`.
fn asset_urls(html: &str) -> Vec<String> {
    html.split('"')
        .filter(|part| part.starts_with("/assets/"))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn r84_get_root_returns_the_shell_with_the_csp_header() {
    let app = app();
    let response = get(&app, "/").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_security_headers(&response);
    assert_eq!(
        header(&response, "content-type"),
        "text/html; charset=utf-8"
    );
    assert_eq!(header(&response, "cache-control"), "no-cache");
    let html = body_text(response).await;
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains(r#"<main id="app""#));
    assert!(
        !html.contains("__BUILD_ID__"),
        "build id placeholder is replaced"
    );
    // CSP has no 'unsafe-inline': the shell must not contain inline scripts or styles.
    assert!(!html.contains("<script>") && !html.contains("<style") && !html.contains(" style="));
    for tag in html.split("<script").skip(1) {
        assert!(
            tag.trim_start().contains("src=\"/assets/"),
            "external module script only"
        );
    }
}

#[tokio::test]
async fn r84_shell_assets_are_served_with_versioned_immutable_urls() {
    let app = app();
    let html = body_text(get(&app, "/").await).await;
    let urls = asset_urls(&html);
    assert!(urls.iter().any(|u| u.ends_with("/js/main.js")), "{urls:?}");
    assert!(urls.iter().any(|u| u.ends_with("/css/app.css")), "{urls:?}");
    for url in &urls {
        let response = get(&app, url).await;
        assert_eq!(response.status(), StatusCode::OK, "{url}");
        assert_security_headers(&response);
        assert_eq!(
            header(&response, "cache-control"),
            "public, max-age=31536000, immutable"
        );
        let expected = if url.ends_with(".js") {
            "text/javascript"
        } else {
            "text/css"
        };
        assert!(
            header(&response, "content-type").starts_with(expected),
            "{url}"
        );
    }
    // Module imports resolve relative to main.js.
    let main = urls
        .iter()
        .find(|u| u.ends_with("/js/main.js"))
        .expect("main.js");
    let dom = main.replace("main.js", "dom.js");
    assert_eq!(get(&app, &dom).await.status(), StatusCode::OK);

    // Another build id, unknown files and traversal attempts are not found.
    let stale = main.replacen(main.split('/').nth(2).expect("build id"), "000000000000", 1);
    for uri in [
        stale.as_str(),
        &main.replace("main.js", "missing.js"),
        &main.replace("js/main.js", "index.html"),
        &main.replace("js/main.js", "js/../css/app.css"),
    ] {
        assert_eq!(
            get(&app, uri).await.status(),
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn r83_healthz_reports_ok_without_details() {
    let app = app();
    let response = get(&app, "/healthz").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_security_headers(&response);
    assert_eq!(header(&response, "cache-control"), "no-store");
    assert_eq!(body_text(response).await, r#"{"status":"ok"}"#);
}

#[tokio::test]
async fn r84_unknown_paths_get_a_json_404_with_security_headers() {
    let app = app();
    let response = get(&app, "/api/nothing-here?q=secret").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_security_headers(&response);
    assert_eq!(
        header(&response, header::CONTENT_TYPE.as_str()),
        "application/json"
    );
    assert_eq!(
        body_text(response).await,
        r#"{"error":{"code":"not_found","message":"Not found"}}"#
    );
}

// ── connection level (`http::serve`) ─────────────────────────────────────────

mod served {
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;
    use vocab_server::config::Limits;
    use vocab_server::db::Db;
    use vocab_server::http::{self, AppState};

    struct Server {
        addr: SocketAddr,
        stop: Option<oneshot::Sender<()>>,
        task: tokio::task::JoinHandle<std::io::Result<()>>,
        _dir: tempfile::TempDir,
    }

    impl Server {
        async fn start(limits: Limits) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let db = Db::open(dir.path()).expect("db");
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().expect("addr");
            let (stop, stopped) = oneshot::channel::<()>();
            let app = http::app(AppState::new(db), &limits);
            let task = tokio::spawn(async move {
                http::serve(listener, app, &limits, async {
                    let _ = stopped.await;
                })
                .await
            });
            Self {
                addr,
                stop: Some(stop),
                task,
                _dir: dir,
            }
        }

        async fn stop(mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            let result = tokio::time::timeout(Duration::from_secs(10), &mut self.task)
                .await
                .expect("server stops");
            result.expect("join").expect("serve result");
        }
    }

    fn limits() -> Limits {
        Limits {
            header_read_timeout_secs: 1,
            ..Limits::default()
        }
    }

    /// Read until the server closes the connection; returns what was read
    /// and how long it took. Panics if the connection is still open after `max`.
    async fn read_until_closed(stream: &mut TcpStream, max: Duration) -> (String, Duration) {
        let started = Instant::now();
        let mut buf = Vec::new();
        let result = tokio::time::timeout(max, stream.read_to_end(&mut buf)).await;
        assert!(result.is_ok(), "connection still open after {max:?}");
        (
            String::from_utf8_lossy(&buf).into_owned(),
            started.elapsed(),
        )
    }

    #[tokio::test]
    async fn r83_partial_request_head_is_closed_after_the_header_timeout() {
        let server = Server::start(limits()).await;
        let mut stream = TcpStream::connect(server.addr).await.expect("connect");
        // Slowloris: start a request but never finish its head.
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nX-Slow: 1\r\n")
            .await
            .expect("send");
        let (_, waited) = read_until_closed(&mut stream, Duration::from_secs(5)).await;
        assert!(
            waited >= Duration::from_millis(800),
            "closed too early: {waited:?}"
        );
        server.stop().await;
    }

    #[tokio::test]
    async fn r83_idle_keep_alive_connection_is_closed() {
        let server = Server::start(limits()).await;
        let mut stream = TcpStream::connect(server.addr).await.expect("connect");
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("send");
        let (response, _) = read_until_closed(&mut stream, Duration::from_secs(5)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(
            response
                .to_ascii_lowercase()
                .contains("content-security-policy:")
        );
        server.stop().await;
    }

    #[tokio::test]
    async fn r83_open_connections_are_capped() {
        let server = Server::start(Limits {
            max_connections: 1,
            ..limits()
        })
        .await;
        // The only slot is taken by a client that sends nothing...
        let _hog = TcpStream::connect(server.addr).await.expect("connect");
        tokio::time::sleep(Duration::from_millis(100)).await;
        // ...so the next client is only served once the header timeout frees it.
        let started = Instant::now();
        let mut stream = TcpStream::connect(server.addr).await.expect("connect");
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .expect("send");
        let (response, _) = read_until_closed(&mut stream, Duration::from_secs(5)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(
            started.elapsed() >= Duration::from_millis(700),
            "served before the slot was free: {:?}",
            started.elapsed()
        );
        server.stop().await;
    }
}
