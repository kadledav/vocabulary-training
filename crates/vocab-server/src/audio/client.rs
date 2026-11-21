//! Client of the TTS service (vocab-tts, design §10.2): `GET /v1/voices`
//! and `POST /v1/speak`, over plain HTTP/1.1 on the internal network.
//!
//! Deliberately small and strict: only the one configured endpoint
//! (`VOCAB_TTS_URL`, `http://host[:port]`, no path), one request per
//! connection, no redirects (a 3xx is an error), a timeout for the whole
//! exchange, response heads up to 16 KiB and bodies up to a cap, a
//! `Content-Length` is required (no chunked bodies). The text sent is never
//! logged here or in the errors (R-24).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::key::Versions;

const MAX_HEAD: usize = 16 * 1024;
/// A clip of the longest text is about 250 KB; far below this.
const MAX_CLIP_BYTES: usize = 4 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 64 * 1024;
const VOICES_TIMEOUT: Duration = Duration::from_secs(10);
/// The service's own request timeout is 60 s.
const SPEAK_TIMEOUT: Duration = Duration::from_secs(75);

/// The configured TTS service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TtsEndpoint {
    host: String,
    port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("must be http://host[:port] (an internal host name or IP address, no path)")]
pub struct EndpointError;

impl TtsEndpoint {
    pub fn parse(url: &str) -> Result<Self, EndpointError> {
        let rest = url.strip_prefix("http://").ok_or(EndpointError)?;
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) => {
                let port = port
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p > 0 && !port.starts_with('0'))
                    .ok_or(EndpointError)?;
                (host, port)
            }
            None => (rest, 80),
        };
        let host_ok = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            && !host.starts_with(['.', '-']);
        if !host_ok {
            return Err(EndpointError);
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }
}

impl std::fmt::Display for TtsEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "http://{}:{}", self.host, self.port)
    }
}

/// Why a call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// Unreachable, timed out, broken or malformed answer, 5xx or busy:
    /// worth trying again later.
    #[error("{0}")]
    Temporary(String),
    /// The service refused this text for good (4xx): its error code.
    #[error("refused: {0}")]
    Refused(String),
}

/// A spoken clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpokenClip {
    pub mp3: Vec<u8>,
    pub duration_ms: i64,
    /// Words whose pronunciation is a guess (`X-Approximate`).
    pub approximate: i64,
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn temporary(message: impl Into<String>) -> CallError {
    CallError::Temporary(message.into())
}

/// One request on a fresh connection; the response must arrive in time.
async fn exchange(
    endpoint: &TtsEndpoint,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    max_body: usize,
    timeout: Duration,
) -> Result<Response, CallError> {
    let work = async {
        let mut stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
            .await
            .map_err(|err| temporary(format!("connect {endpoint}: {err}")))?;
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}:{}\r\nAccept: */*\r\nConnection: close\r\n",
            endpoint.host, endpoint.port
        );
        if let Some(body) = body {
            request.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                body.len()
            ));
        }
        request.push_str("\r\n");
        let mut bytes = request.into_bytes();
        bytes.extend_from_slice(body.unwrap_or_default());
        stream
            .write_all(&bytes)
            .await
            .map_err(|err| temporary(format!("send: {err}")))?;
        read_response(&mut stream, max_body).await
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| temporary(format!("no answer within {} s", timeout.as_secs())))?
}

async fn read_response(stream: &mut TcpStream, max_body: usize) -> Result<Response, CallError> {
    let mut buffer = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];
    let (status, headers, head_len) = loop {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|err| temporary(format!("read: {err}")))?;
        if n == 0 {
            return Err(temporary("connection closed before the response head"));
        }
        buffer.extend_from_slice(&chunk[..n]);
        let mut slots = [httparse::EMPTY_HEADER; 32];
        let mut parsed = httparse::Response::new(&mut slots);
        match parsed.parse(&buffer) {
            Ok(httparse::Status::Complete(len)) => {
                let status = parsed.code.unwrap_or(0);
                let headers = parsed
                    .headers
                    .iter()
                    .map(|h| {
                        (
                            h.name.to_owned(),
                            String::from_utf8_lossy(h.value).into_owned(),
                        )
                    })
                    .collect::<Vec<_>>();
                break (status, headers, len);
            }
            Ok(httparse::Status::Partial) if buffer.len() < MAX_HEAD => {}
            Ok(httparse::Status::Partial) => return Err(temporary("response head too large")),
            Err(err) => return Err(temporary(format!("malformed response: {err}"))),
        }
    };
    let length = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .ok_or_else(|| temporary("response without Content-Length"))?;
    if length > max_body {
        return Err(temporary(format!(
            "response of {length} bytes is too large"
        )));
    }
    let mut body = buffer.split_off(head_len);
    if body.len() > length {
        return Err(temporary("more data than Content-Length"));
    }
    while body.len() < length {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|err| temporary(format!("read: {err}")))?;
        if n == 0 {
            return Err(temporary("connection closed in the body"));
        }
        if body.len() + n > length {
            return Err(temporary("more data than Content-Length"));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Ok(Response {
        status,
        headers,
        body,
    })
}

/// The error code of a JSON error body (`{"error": {"code": …}}`), as a
/// short safe token.
fn error_code(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["code"].as_str().map(str::to_owned))
        .filter(|c| c.len() <= 40 && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn check_status(response: &Response) -> Result<(), CallError> {
    match response.status {
        200 => Ok(()),
        400..=499 if response.status != 408 && response.status != 429 => Err(CallError::Refused(
            format!("{} {}", response.status, error_code(&response.body)),
        )),
        status => Err(temporary(format!(
            "status {status} {}",
            error_code(&response.body)
        ))),
    }
}

/// A version string as the service sends it.
fn version_token(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 64
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        })
        .map(str::to_owned)
}

/// The service's front-end and engine versions.
pub async fn versions(endpoint: &TtsEndpoint) -> Result<Versions, CallError> {
    let response = exchange(
        endpoint,
        "GET",
        "/v1/voices",
        None,
        MAX_JSON_BYTES,
        VOICES_TIMEOUT,
    )
    .await?;
    check_status(&response).map_err(|err| match err {
        CallError::Refused(code) => temporary(code),
        other => other,
    })?;
    let json: serde_json::Value =
        serde_json::from_slice(&response.body).map_err(|_| temporary("/v1/voices: not JSON"))?;
    match (
        version_token(&json["frontend"]),
        version_token(&json["engine"]),
    ) {
        (Some(frontend), Some(engine)) => Ok(Versions { frontend, engine }),
        _ => Err(temporary("/v1/voices: no front-end/engine versions")),
    }
}

/// Speak `text` with the TTS voice `voice`.
pub async fn speak(
    endpoint: &TtsEndpoint,
    text: &str,
    voice: &str,
) -> Result<SpokenClip, CallError> {
    let body = serde_json::to_vec(&serde_json::json!({ "text": text, "voice": voice }))
        .map_err(|err| temporary(err.to_string()))?;
    let response = exchange(
        endpoint,
        "POST",
        "/v1/speak",
        Some(&body),
        MAX_CLIP_BYTES,
        SPEAK_TIMEOUT,
    )
    .await?;
    check_status(&response)?;
    if response.header("content-type") != Some("audio/mpeg") {
        return Err(temporary("speak: not audio/mpeg"));
    }
    // An MPEG audio frame starts with 11 set sync bits.
    let mp3 = response.body;
    if mp3.len() < 4 || mp3[0] != 0xFF || mp3[1] & 0xE0 != 0xE0 {
        return Err(temporary("speak: not an MP3 stream"));
    }
    let number = |name: &str| -> Option<i64> {
        response_header(&response.headers, name)?
            .trim()
            .parse()
            .ok()
    };
    let duration_ms = number("x-duration-ms")
        .filter(|d| *d > 0)
        .ok_or_else(|| temporary("speak: no X-Duration-Ms"))?;
    let approximate = number("x-approximate").unwrap_or(0).max(0);
    Ok(SpokenClip {
        mp3,
        duration_ms,
        approximate,
    })
}

fn response_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r83_only_a_plain_internal_http_endpoint_is_accepted() {
        let ok = TtsEndpoint::parse("http://vocabulary-tts:8090").expect("valid");
        assert_eq!(ok.to_string(), "http://vocabulary-tts:8090");
        assert_eq!(
            TtsEndpoint::parse("http://192.0.2.5/")
                .expect("valid")
                .to_string(),
            "http://192.0.2.5:80"
        );
        for bad in [
            "https://vocabulary-tts:8090",
            "vocabulary-tts:8090",
            "http://vocabulary-tts:8090/v1",
            "http://user@vocabulary-tts:8090",
            "http://Vocabulary-TTS:8090",
            "http://vocabulary-tts:0",
            "http://vocabulary-tts:99999",
            "http://:8090",
            "http://tts:8090?x=1",
            "http://[::1]:8090",
        ] {
            assert_eq!(TtsEndpoint::parse(bad), Err(EndpointError), "{bad}");
        }
    }

    #[test]
    fn r84_error_codes_are_short_safe_tokens() {
        assert_eq!(
            error_code(br#"{"error":{"code":"invalid_text"}}"#),
            "invalid_text"
        );
        assert_eq!(
            error_code(br#"{"error":{"code":"a secret sentence"}}"#),
            "unknown"
        );
        assert_eq!(error_code(b"<html>"), "unknown");
    }
}
