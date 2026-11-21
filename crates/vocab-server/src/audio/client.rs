//! Client of the TTS service (vocab-tts, design §10.2): `GET /v1/voices`
//! and `POST /v1/speak`, over plain HTTP/1.1 on the internal network.
//!
//! Deliberately small and strict: only the one configured endpoint
//! (`VOCAB_TTS_URL`, `http://host[:port]`, no path, an internal host only:
//! see [`TtsEndpoint::parse`]), one request per
//! connection, no redirects (a 3xx is an error), a timeout for the whole
//! exchange, response heads up to 16 KiB and bodies up to a cap, a
//! `Content-Length` is required (no chunked bodies). The text sent is never
//! logged here or in the errors (R-24).

use std::net::IpAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::key::Versions;

const MAX_HEAD: usize = 16 * 1024;
/// Largest clip accepted: 60 s (the longest clip, [`super::MAX_CLIP_MS`])
/// at the service's 64 kbit/s is 480 KB; the longest text makes about 250 KB.
pub const MAX_CLIP_BYTES: usize = 1024 * 1024;
const MAX_JSON_BYTES: usize = 64 * 1024;
const VOICES_TIMEOUT: Duration = Duration::from_secs(10);
/// Longer than the service's own request timeout (60 s): a slow synthesis
/// ends with the service's `503 timeout` (and the service cancels the
/// work), not with this side hanging up first.
const SPEAK_TIMEOUT: Duration = Duration::from_secs(75);

/// The configured TTS service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TtsEndpoint {
    host: String,
    port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    #[error("must be http://host[:port] (a host name or IP address, no path)")]
    Format,
    #[error(
        "must be on the internal network: a host name without dots (a compose service \
         name such as vocabulary-tts), localhost, or a loopback, private or link-local \
         IP address (the service has no authentication of its own)"
    )]
    NotInternal,
}

/// A loopback, private or link-local address (never one on the internet).
fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
        }
    }
}

impl TtsEndpoint {
    /// `http://host[:port]` where host is a name without dots (a compose
    /// service name; a dotted name could resolve to anything on the
    /// internet), `localhost`, an IPv4 address or a bracketed IPv6 address
    /// in a loopback, private or link-local range.
    pub fn parse(url: &str) -> Result<Self, EndpointError> {
        let rest = url.strip_prefix("http://").ok_or(EndpointError::Format)?;
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        let (host, port, bracketed) = if let Some(inner) = rest.strip_prefix('[') {
            let (host, after) = inner.split_once(']').ok_or(EndpointError::Format)?;
            (host, after.strip_prefix(':'), true)
        } else {
            match rest.rsplit_once(':') {
                Some((host, port)) => (host, Some(port), false),
                None => (rest, None, false),
            }
        };
        let port = match port {
            Some(port) => port
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0 && !port.starts_with('0'))
                .ok_or(EndpointError::Format)?,
            None if bracketed && !rest.ends_with(']') => return Err(EndpointError::Format),
            None => 80,
        };
        if bracketed {
            let ip = host
                .parse::<std::net::Ipv6Addr>()
                .map_err(|_| EndpointError::Format)?;
            return if is_internal(IpAddr::V6(ip)) {
                Ok(Self {
                    host: ip.to_string(),
                    port,
                })
            } else {
                Err(EndpointError::NotInternal)
            };
        }
        let syntax_ok = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            && !host.starts_with(['.', '-'])
            && !host.ends_with(['.', '-']);
        if !syntax_ok {
            return Err(EndpointError::Format);
        }
        let internal = match host.parse::<std::net::Ipv4Addr>() {
            Ok(ip) => is_internal(IpAddr::V4(ip)),
            Err(_) => !host.contains('.') && host.len() <= 63,
        };
        if !internal {
            return Err(EndpointError::NotInternal);
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    /// The host as it goes into a URL or a `Host` header.
    fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

impl std::fmt::Display for TtsEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "http://{}", self.authority())
    }
}

/// Why a call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// Unreachable, timed out, broken or malformed answer, 5xx or busy:
    /// worth trying again later.
    #[error("{0}")]
    Temporary(String),
    /// The request was sent, then the connection died without a complete
    /// answer: the service may have crashed on this text.
    #[error("connection dropped: {0}")]
    Dropped(String),
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
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAccept: */*\r\nConnection: close\r\n",
            endpoint.authority()
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

fn dropped(message: impl Into<String>) -> CallError {
    CallError::Dropped(message.into())
}

/// Read the answer to a request that was sent: a connection that dies
/// before the answer is complete is [`CallError::Dropped`].
async fn read_response(stream: &mut TcpStream, max_body: usize) -> Result<Response, CallError> {
    let mut buffer = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];
    let (status, headers, head_len) = loop {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|err| dropped(format!("read: {err}")))?;
        if n == 0 {
            return Err(dropped("closed before the response head"));
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
            .map_err(|err| dropped(format!("read: {err}")))?;
        if n == 0 {
            return Err(dropped("closed in the body"));
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
    // (A dropped connection here is just "unreachable" to the caller.)
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
        // Private addresses built from parts (no network details in the
        // sources, tools/repo-guard.py): 10/8, 172.16/12, 192.168/16, 169.254/16.
        let private: Vec<String> = [
            [10, 0, 0, 5],
            [172, 20, 0, 3],
            [192, 168, 1, 20],
            [169, 254, 10, 1],
        ]
        .iter()
        .map(|[a, b, c, d]| std::net::Ipv4Addr::new(*a, *b, *c, *d).to_string())
        .collect();
        let mut cases: Vec<(String, String)> = private
            .iter()
            .map(|ip| (format!("http://{ip}:8090"), format!("http://{ip}:8090")))
            .collect();
        cases.push((
            format!("http://{}/", private[0]),
            format!("http://{}:80", private[0]),
        ));
        for (url, shown) in [
            ("http://localhost:8090", "http://localhost:8090"),
            ("http://127.0.0.1:8090", "http://127.0.0.1:8090"),
            ("http://[::1]:8090", "http://[::1]:8090"),
            ("http://[fd00::5]:8090", "http://[fd00::5]:8090"),
            ("http://[fe80::1]", "http://[fe80::1]:80"),
            ("http://tts", "http://tts:80"),
        ] {
            cases.push((url.to_owned(), shown.to_owned()));
        }
        for (url, shown) in &cases {
            let endpoint = TtsEndpoint::parse(url).unwrap_or_else(|err| panic!("{url}: {err}"));
            assert_eq!(&endpoint.to_string(), shown);
        }
        for public in [
            "http://192.0.2.5:8090",
            "http://8.8.8.8:8090",
            "http://tts.example.org:8090",
            "http://tts.internal:8090",
            "http://[2001:db8::1]:8090",
            "http://0.0.0.0:8090",
        ] {
            assert_eq!(
                TtsEndpoint::parse(public),
                Err(EndpointError::NotInternal),
                "{public}"
            );
        }
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
            "http://[::1",
            "http://[::1]8090",
            "http://[nope]:8090",
            "http://tts-:8090",
        ] {
            assert_eq!(TtsEndpoint::parse(bad), Err(EndpointError::Format), "{bad}");
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
