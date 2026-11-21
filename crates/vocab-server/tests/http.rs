//! HTTP skeleton: the embedded shell, security headers, `/healthz`.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use tower::ServiceExt;
use vocab_server::config::Limits;
use vocab_server::db::Db;
use vocab_server::http::{self, AppState};

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   media-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; \
                   frame-ancestors 'none'; form-action 'self'; require-trusted-types-for 'script'";

struct TestApp {
    _dir: tempfile::TempDir,
    router: axum::Router,
}

fn app() -> TestApp {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(dir.path()).expect("db");
    let (auth, code) = common::auth();
    let router = common::logged_in(
        http::app(AppState::new(db, prague(), auth), &Limits::default()),
        code,
    );
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
    assert_eq!(header(response, "referrer-policy"), "same-origin");
    assert_eq!(
        header(response, "cross-origin-opener-policy"),
        "same-origin"
    );
    assert_eq!(
        header(response, "cross-origin-resource-policy"),
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
        let expected = match url.rsplit('.').next() {
            Some("js") => "text/javascript",
            Some("css") => "text/css",
            Some("png") => "image/png",
            Some("webmanifest") => "application/manifest+json",
            other => panic!("unexpected asset type {other:?}: {url}"),
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

/// B1 (review probe p4): the health check does not wait for the database,
/// whose single connection a long import may hold.
#[tokio::test]
async fn r83_healthz_answers_while_the_database_is_busy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(dir.path()).expect("db");
    let (auth, _code) = common::auth();
    let router = http::app(
        AppState::new(db.clone(), prague(), auth),
        &Limits::default(),
    );
    let busy = tokio::spawn(async move {
        db.write(|_tx| {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            Ok::<_, vocab_server::db::DbError>(())
        })
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    let response = router
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "waited {:?}",
        started.elapsed()
    );
    busy.await.expect("join").expect("write");
}

/// B6: an API request the browser itself labels as started by another
/// site, or by a sibling host, is refused, whatever the method and even
/// with a valid session; without the header (the connector, scripts)
/// nothing changes.
#[tokio::test]
async fn r82_api_requests_labelled_cross_site_or_same_site_are_refused() {
    let app = app();
    let fetch = |uri: &'static str, site: Option<&'static str>| {
        let mut builder = Request::builder().uri(uri);
        if let Some(site) = site {
            builder = builder.header("sec-fetch-site", site);
        }
        app.router
            .clone()
            .oneshot(builder.body(Body::empty()).expect("request"))
    };
    for foreign in ["cross-site", "same-site"] {
        for uri in ["/api/auth/me", "/api/today", "/api/auth/setup"] {
            let response = fetch(uri, Some(foreign)).await.expect("response");
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{foreign} {uri}");
            assert_security_headers(&response);
            assert!(
                body_text(response).await.contains("\"forbidden_origin\""),
                "{foreign} {uri}"
            );
        }
        // Pages and the health check are not the API: a link from another
        // site may open the app.
        for uri in ["/", "/healthz"] {
            let response = fetch(uri, Some(foreign)).await.expect("response");
            assert_eq!(response.status(), StatusCode::OK, "{foreign} {uri}");
        }
    }
    for fine in [Some("same-origin"), Some("none"), None] {
        let response = fetch("/api/auth/me", fine).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK, "{fine:?}");
    }
    // A cross-site POST is refused before anything else is looked at.
    let response = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout-all")
                .header("sec-fetch-site", "cross-site")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = fetch("/api/auth/me", None).await.expect("response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the session is still there"
    );
}

/// B5: the transport limit of the import routes is 2.5 MiB.
#[tokio::test]
async fn r84_import_bodies_are_limited_to_2_5_mib() {
    let app = app();
    let post = |len: usize| {
        let mut body = Vec::with_capacity(len);
        body.extend_from_slice(b"{\"text\": \"");
        body.resize(len - 2, b' ');
        body.extend_from_slice(b"\"}");
        app.router.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/import/preview")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
    };
    let limit = 5 * 1024 * 1024 / 2;
    let response = post(limit + 1).await.expect("response");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    // At the limit the body is read; its text is over the 2 MiB paste limit.
    let response = post(limit).await.expect("response");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_text(response).await.contains("\"invalid_paste\""));
}

/// B5 (review probes p5/p6): about 2 MiB of tiny JSON values is answered
/// from a scan of the text; no pack is built from it.
#[tokio::test]
async fn r84_a_paste_of_a_million_tiny_values_is_refused_without_being_built() {
    let app = app();
    let preview = |text: String| {
        let body = serde_json::json!({ "text": text }).to_string();
        app.router.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/import/preview")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
    };
    let unit_count = 1_000_000;
    for unit in ["0,", "{},"] {
        let text = format!("[{}0]", unit.repeat(unit_count * 2 / unit.len()));
        assert!(text.len() <= 2 * 1024 * 1024);
        let response = preview(text).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let report: serde_json::Value =
            serde_json::from_str(&body_text(response).await).expect("json");
        assert_eq!(report["packs"].as_array().expect("packs").len(), 1);
        assert_eq!(report["packs"][0]["status"], "unreadable", "{unit}");
        assert_eq!(report["packs"][0]["words"], serde_json::json!([]));
    }
    // Many values spread over several packs: the paste as a whole.
    let pack = format!("[{}0]", "0,".repeat(55_000));
    let response = preview(format!("{pack}\n").repeat(4))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body_text(response).await.contains("JSON values"));
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
        /// `name=value` of a session cookie.
        cookie: String,
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
            let (auth, code) = super::common::auth();
            let app = http::app(AppState::new(db, super::prague(), auth), &limits);
            let cookie = super::common::login(&app, &code).await;
            let task = tokio::spawn(async move {
                http::serve(listener, app, &limits, async {
                    let _ = stopped.await;
                })
                .await
            });
            Self {
                addr,
                cookie,
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
            keep_alive_idle_secs: 3,
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
        let (response, waited) = read_until_closed(&mut stream, Duration::from_secs(8)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        // Closed after the keep-alive idle time (3 s here), not the 1 s head
        // deadline.
        assert!(
            waited >= Duration::from_millis(2500),
            "closed too early: {waited:?}"
        );
        assert!(
            response
                .to_ascii_lowercase()
                .contains("content-security-policy:")
        );
        server.stop().await;
    }

    /// Read one response with a `Content-Length` body from a keep-alive
    /// connection.
    async fn read_response(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte))
                .await
                .expect("response in time")
                .expect("read");
            assert_eq!(n, 1, "closed early: {}", String::from_utf8_lossy(&buf));
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buf).into_owned();
        let length: usize = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().to_owned())
            })
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).await.expect("body");
        head
    }

    /// A reverse proxy reuses idle upstream connections for about 90 s: an
    /// idle keep-alive connection outlives the head deadline (120 s by
    /// default, 3 s here), while a head that has started must still be
    /// complete within the head deadline (1 s here) of its first byte.
    #[tokio::test]
    async fn r83_keep_alive_connections_may_idle_longer_than_the_head_deadline() {
        assert_eq!(Limits::default().keep_alive_idle_secs, 120);
        let server = Server::start(limits()).await;
        let mut stream = TcpStream::connect(server.addr).await.expect("connect");
        let request = b"GET /healthz HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(request).await.expect("send");
        assert!(read_response(&mut stream).await.starts_with("HTTP/1.1 200"));
        // Idle for twice the head deadline, then reused.
        tokio::time::sleep(Duration::from_secs(2)).await;
        stream.write_all(request).await.expect("send again");
        assert!(read_response(&mut stream).await.starts_with("HTTP/1.1 200"));
        // Idle a little, then a head that never ends: closed about one head
        // deadline after its first byte, long before the idle time is up.
        tokio::time::sleep(Duration::from_millis(500)).await;
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nX-Slow: 1\r\n")
            .await
            .expect("send slow head");
        let (rest, waited) = read_until_closed(&mut stream, Duration::from_secs(5)).await;
        assert!(
            waited >= Duration::from_millis(800) && waited < Duration::from_millis(2400),
            "closed after {waited:?}: {rest}"
        );
        server.stop().await;
    }

    #[tokio::test]
    async fn r84_oversized_import_body_gets_a_json_413_not_a_reset() {
        let server = Server::start(Limits::default()).await;
        let mut stream = TcpStream::connect(server.addr).await.expect("connect");
        let body_len = 6 * 1024 * 1024; // over the 2.5 MiB import body limit
        let head = format!(
            "POST /api/import/preview HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Origin: {}\r\nCookie: {}\r\nContent-Length: {body_len}\r\nConnection: close\r\n\r\n",
            super::common::ORIGIN,
            server.cookie,
        );
        stream.write_all(head.as_bytes()).await.expect("head");
        let mut body = Vec::with_capacity(body_len);
        body.extend_from_slice(b"{\"text\": \"");
        body.resize(body_len - 2, b'x');
        body.extend_from_slice(b"\"}");
        // The whole body is accepted (read and discarded) by the server.
        stream
            .write_all(&body)
            .await
            .expect("body sent without a reset");
        let (response, _) = read_until_closed(&mut stream, Duration::from_secs(10)).await;
        assert!(response.starts_with("HTTP/1.1 413"), "{response}");
        assert!(response.contains("\"too_large\""), "{response}");
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

    // ── fix round R1: nobody without a session can stop the app (B1, B5) ────

    /// One request on a fresh connection; the whole response.
    async fn exchange(addr: SocketAddr, request: String) -> (String, Duration) {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(request.as_bytes()).await.expect("send");
        read_until_closed(&mut stream, Duration::from_secs(10)).await
    }

    fn get_request(path: &str, cookie: Option<&str>) -> String {
        let cookie = cookie
            .map(|c| format!("Cookie: {c}\r\n"))
            .unwrap_or_default();
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{cookie}Connection: close\r\n\r\n")
    }

    /// The head of a JSON POST from the app's origin that announces
    /// `content_length` bytes, followed by `sent` (possibly fewer bytes).
    fn post_head(path: &str, cookie: Option<&str>, content_length: usize, sent: &str) -> String {
        let cookie = cookie
            .map(|c| format!("Cookie: {c}\r\n"))
            .unwrap_or_default();
        format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nOrigin: {}\r\n\
             Content-Type: application/json\r\n{cookie}Content-Length: {content_length}\r\n\
             Connection: close\r\n\r\n{sent}",
            super::common::ORIGIN
        )
    }

    /// B1 (review probe p2): unauthenticated clients that send a login head
    /// and then stall their bodies take every general request slot. The
    /// health check, and the owner with his session, are still served at
    /// once; further anonymous requests are refused quickly with
    /// `Retry-After`; the stalled requests lose their slots after the short
    /// body deadline of public routes.
    #[tokio::test]
    async fn r83_stalled_unauthenticated_bodies_cannot_starve_the_app() {
        let server = Server::start(Limits {
            max_concurrent_requests: 4,
            reserved_session_slots: 1,
            slot_wait_ms: 300,
            public_body_read_secs: 1,
            ..Limits::default()
        })
        .await;
        let mut stalled = Vec::new();
        for _ in 0..6 {
            let mut stream = TcpStream::connect(server.addr).await.expect("connect");
            stream
                .write_all(post_head("/api/auth/login", None, 100, "{").as_bytes())
                .await
                .expect("send");
            stalled.push(stream);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        // The health check needs no slot.
        let (response, took) = exchange(server.addr, get_request("/healthz", None)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(took < Duration::from_millis(250), "healthz waited {took:?}");
        // The owner's session gets the reserved slot.
        let (response, took) = exchange(
            server.addr,
            get_request("/api/auth/me", Some(&server.cookie)),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(
            took < Duration::from_millis(250),
            "the owner waited {took:?}"
        );
        // Anyone else is told to come back, after the short wait for a slot.
        let (response, took) = exchange(server.addr, get_request("/api/auth/setup", None)).await;
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");
        let lower = response.to_ascii_lowercase();
        assert!(lower.contains("retry-after: 5"), "{response}");
        assert!(lower.contains("\"overloaded\""), "{response}");
        assert!(lower.contains("content-security-policy:"), "{response}");
        assert!(took < Duration::from_secs(2), "refused only after {took:?}");

        // The stalled requests are answered (408 for those that had a slot,
        // 503 for those that never got one) well before the 30 s request
        // timeout, and the app is free again.
        let started = Instant::now();
        let mut answers = Vec::new();
        for mut stream in stalled {
            let (response, _) = read_until_closed(&mut stream, Duration::from_secs(5)).await;
            answers.push(response.chars().take(12).collect::<String>());
        }
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(
            answers.iter().filter(|a| *a == "HTTP/1.1 408").count(),
            3,
            "{answers:?}"
        );
        assert_eq!(
            answers.iter().filter(|a| *a == "HTTP/1.1 503").count(),
            3,
            "{answers:?}"
        );
        let (response, _) = exchange(server.addr, get_request("/api/auth/setup", None)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        server.stop().await;
    }

    /// B1 (review probe p8): one address may hold only its share of the
    /// connections; what is over is refused at once instead of waiting for
    /// the header timeout, and a closed connection frees its place.
    #[tokio::test]
    async fn r83_one_peer_cannot_hold_every_connection() {
        let server = Server::start(Limits {
            max_connections: 8,
            max_connections_per_peer: 3,
            header_read_timeout_secs: 20,
            ..Limits::default()
        })
        .await;
        // Three connections that never send a byte.
        let mut silent = Vec::new();
        for _ in 0..3 {
            silent.push(TcpStream::connect(server.addr).await.expect("connect"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        for _ in 0..5 {
            let (response, took) = exchange(server.addr, get_request("/healthz", None)).await;
            assert!(response.starts_with("HTTP/1.1 503"), "{response}");
            assert!(response.to_ascii_lowercase().contains("retry-after: 5"));
            assert!(took < Duration::from_secs(1), "refused only after {took:?}");
        }
        // One of them goes away: its place is free again.
        silent.pop();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (response, _) = exchange(server.addr, get_request("/healthz", None)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        drop(silent);
        server.stop().await;
    }

    /// B5 (review probe p5): one import at a time, its body included. While
    /// one import request is still sending its body, a second one waits
    /// briefly and is then refused; nothing of it is read into memory.
    #[tokio::test]
    async fn r84_only_one_import_is_processed_at_a_time() {
        let server = Server::start(Limits {
            slot_wait_ms: 300,
            ..Limits::default()
        })
        .await;
        let paste = r#"{"text": "1"}"#;
        let mut first = TcpStream::connect(server.addr).await.expect("connect");
        first
            .write_all(
                post_head(
                    "/api/import/preview",
                    Some(&server.cookie),
                    paste.len(),
                    &paste[..5],
                )
                .as_bytes(),
            )
            .await
            .expect("send");
        tokio::time::sleep(Duration::from_millis(100)).await;

        for path in ["/api/import/preview", "/api/import/commit"] {
            let (response, took) = exchange(
                server.addr,
                post_head(path, Some(&server.cookie), paste.len(), paste),
            )
            .await;
            assert!(response.starts_with("HTTP/1.1 503"), "{path}: {response}");
            assert!(response.contains("\"busy\""), "{response}");
            assert!(response.to_ascii_lowercase().contains("retry-after: 5"));
            assert!(
                took >= Duration::from_millis(250) && took < Duration::from_secs(2),
                "{path}: waited {took:?}"
            );
        }
        // Other requests are not held up by the import.
        let (response, _) =
            exchange(server.addr, get_request("/api/today", Some(&server.cookie))).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");

        // The first import finishes; then it is the next one's turn.
        first.write_all(&paste.as_bytes()[5..]).await.expect("rest");
        let (response, _) = read_until_closed(&mut first, Duration::from_secs(10)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("\"mode\":\"preview\""), "{response}");
        let (response, _) = exchange(
            server.addr,
            post_head(
                "/api/import/preview",
                Some(&server.cookie),
                paste.len(),
                paste,
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        server.stop().await;
    }

    /// B1: the body of an import must arrive within the request timeout;
    /// a stalled one gets a 408 and gives the import turn back.
    #[tokio::test]
    async fn r83_a_stalled_import_body_times_out_and_frees_the_turn() {
        let server = Server::start(Limits {
            request_timeout_secs: 1,
            ..Limits::default()
        })
        .await;
        let mut stalled = TcpStream::connect(server.addr).await.expect("connect");
        stalled
            .write_all(
                post_head(
                    "/api/import/commit",
                    Some(&server.cookie),
                    1000,
                    "{\"text\":",
                )
                .as_bytes(),
            )
            .await
            .expect("send");
        let (response, took) = read_until_closed(&mut stalled, Duration::from_secs(5)).await;
        assert!(response.starts_with("HTTP/1.1 408"), "{response}");
        assert!(response.contains("\"timeout\""), "{response}");
        assert!(took >= Duration::from_millis(800), "{took:?}");
        let paste = r#"{"text": "1"}"#;
        let (response, _) = exchange(
            server.addr,
            post_head(
                "/api/import/preview",
                Some(&server.cookie),
                paste.len(),
                paste,
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        server.stop().await;
    }
}

fn prague() -> vocab_server::clock::DayRule {
    vocab_server::clock::DayRule::new("Europe/Prague", 4).expect("bundled tzdb")
}
