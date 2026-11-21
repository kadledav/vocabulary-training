//! The service's logs never contain the text (R-24). A test binary of its
//! own: the log subscriber is process-wide.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use common::{app, send, speak_request};

/// Everything the service logs.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("test setup").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn r24_logs_carry_sizes_and_fingerprints_not_the_text() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("test setup");
    let (app, _) = app();
    let (status, _, _) = send(&app, speak_request(r#"{"text": "Secret tomato world."}"#)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&app, speak_request(r#"{"text": "secret <b>"}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let logs =
        String::from_utf8(captured.0.lock().expect("test setup").clone()).expect("test setup");
    assert!(
        logs.contains("spoke") && logs.contains("text_chars=20") && logs.contains("text_sha256="),
        "{logs}"
    );
    assert!(logs.contains("route=/v1/speak status=200"), "{logs}");
    let lower = logs.to_lowercase();
    assert!(
        !lower.contains("secret") && !lower.contains("tomato"),
        "{logs}"
    );
}
