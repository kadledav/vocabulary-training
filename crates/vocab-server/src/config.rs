//! Non-secret configuration (design §3.2, §12.1, R-85).
//!
//! Sources:
//! * `config.json` at `$VOCAB_CONFIG` (default `/config/config.json`, mounted
//!   read-only). Unknown keys are a startup error; every key except
//!   `public_origin` has a default.
//! * Directories from the environment: `$VOCAB_DATA_DIR` (default `/data`,
//!   the SQLite file set) and `$VOCAB_CACHE_DIR` (default `/cache`, audio).
//!
//! Secrets (password hash, import token, TTS token) never live in this file;
//! they come from the environment and are added with the modules that use
//! them (M5 `auth`, M6 `audio`).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::clock::{ClockError, DayRule};

const DEFAULT_CONFIG_PATH: &str = "/config/config.json";
const DEFAULT_DATA_DIR: &str = "/data";
const DEFAULT_CACHE_DIR: &str = "/cache";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("config file {path} not found; it must at least set \"public_origin\"")]
    Missing { path: PathBuf },
    #[error("invalid config value {key}: {message}")]
    Invalid { key: &'static str, message: String },
    #[error("invalid config value timezone/day_rollover_hour: {0}")]
    DayRule(#[from] ClockError),
}

/// Effective configuration of the server.
#[derive(Debug, Clone)]
pub struct Config {
    /// Exact origin the app is served from, e.g. `https://vocab.example.home`
    /// (no path, no trailing slash). Used by the Origin check (§9.3).
    pub public_origin: String,
    /// Study-day definition built from `timezone` and `day_rollover_hour`.
    pub day_rule: DayRule,
    pub listen: SocketAddr,
    pub tts_url: String,
    pub log_level: tracing::Level,
    pub session_days: u32,
    pub limits: Limits,
    pub paths: Paths,
}

/// Request limits (§9.4). Route-specific limits (import, connector, login)
/// are set by the routes that need them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// Default maximum request body size in bytes.
    pub body_bytes: usize,
    /// Default time a request may take before the server answers 503.
    pub request_timeout_secs: u64,
    /// Requests handled at the same time; further requests wait.
    pub max_concurrent_requests: usize,
    /// Time a client has to send a complete request head; also the idle
    /// timeout of keep-alive connections.
    pub header_read_timeout_secs: u64,
    /// Open client connections; further connections wait in the backlog.
    pub max_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            body_bytes: 64 * 1024,
            request_timeout_secs: 30,
            max_concurrent_requests: 64,
            header_read_timeout_secs: 10,
            max_connections: 256,
        }
    }
}

impl Limits {
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }

    pub fn header_read_timeout(&self) -> Duration {
        Duration::from_secs(self.header_read_timeout_secs)
    }
}

/// Directories the app writes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

/// The JSON file as written by the owner.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    public_origin: Option<String>,
    #[serde(default = "default_timezone")]
    timezone: String,
    #[serde(default = "default_rollover")]
    day_rollover_hour: u8,
    #[serde(default = "default_listen")]
    listen: String,
    #[serde(default = "default_tts_url")]
    tts_url: String,
    #[serde(default = "default_log_level")]
    log_level: String,
    #[serde(default = "default_session_days")]
    session_days: u32,
    #[serde(default)]
    limits: Limits,
}

fn default_timezone() -> String {
    "Europe/Prague".to_owned()
}
fn default_rollover() -> u8 {
    4
}
fn default_listen() -> String {
    "0.0.0.0:8080".to_owned()
}
fn default_tts_url() -> String {
    "http://vocabulary-tts:8090".to_owned()
}
fn default_log_level() -> String {
    "info".to_owned()
}
fn default_session_days() -> u32 {
    180
}

impl Config {
    /// Load the configuration from the process environment and the config file.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_with(|key| std::env::var_os(key).map(PathBuf::from))
    }

    fn load_with(env: impl Fn(&str) -> Option<PathBuf>) -> Result<Self, ConfigError> {
        let path = env("VOCAB_CONFIG").unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));
        let paths = Paths {
            data_dir: env("VOCAB_DATA_DIR").unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR)),
            cache_dir: env("VOCAB_CACHE_DIR").unwrap_or_else(|| PathBuf::from(DEFAULT_CACHE_DIR)),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(ConfigError::Missing { path });
            }
            Err(source) => return Err(ConfigError::Read { path, source }),
        };
        Self::from_json(&text, paths).map_err(|err| match err {
            ParseError::Json(source) => ConfigError::Parse { path, source },
            ParseError::Config(err) => err,
        })
    }

    fn from_json(text: &str, paths: Paths) -> Result<Self, ParseError> {
        let file: FileConfig = serde_json::from_str(text).map_err(ParseError::Json)?;
        let public_origin = file
            .public_origin
            .ok_or_else(|| invalid("public_origin", "missing (required)"))?;
        validate_origin(&public_origin)?;
        let day_rule = DayRule::new(&file.timezone, file.day_rollover_hour)?;
        let listen = file
            .listen
            .parse()
            .map_err(|_| invalid("listen", "must be an IP:port address such as 0.0.0.0:8080"))?;
        let log_level = file.log_level.parse().map_err(|_| {
            invalid(
                "log_level",
                "must be one of error, warn, info, debug, trace",
            )
        })?;
        if !(file.tts_url.starts_with("http://") || file.tts_url.starts_with("https://")) {
            return Err(invalid("tts_url", "must be an http(s) URL").into());
        }
        if file.session_days == 0 {
            return Err(invalid("session_days", "must be at least 1").into());
        }
        let limits = file.limits;
        if limits.body_bytes == 0
            || limits.request_timeout_secs == 0
            || limits.max_concurrent_requests == 0
            || limits.header_read_timeout_secs == 0
            || limits.max_connections == 0
        {
            return Err(invalid("limits", "every limit must be greater than zero").into());
        }
        Ok(Self {
            public_origin,
            day_rule,
            listen,
            tts_url: file.tts_url,
            log_level,
            session_days: file.session_days,
            limits,
            paths,
        })
    }
}

enum ParseError {
    Json(serde_json::Error),
    Config(ConfigError),
}

impl From<ConfigError> for ParseError {
    fn from(err: ConfigError) -> Self {
        Self::Config(err)
    }
}

impl From<ClockError> for ParseError {
    fn from(err: ClockError) -> Self {
        Self::Config(err.into())
    }
}

fn invalid(key: &'static str, message: &str) -> ConfigError {
    ConfigError::Invalid {
        key,
        message: message.to_owned(),
    }
}

/// An origin as browsers send it in the `Origin` header: lowercase
/// `scheme://host[:port]`, nothing else.
fn validate_origin(origin: &str) -> Result<(), ConfigError> {
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .ok_or_else(|| invalid("public_origin", "must start with https:// or http://"))?;
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (rest, None),
    };
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
    let port_ok = port.is_none_or(|p| p.parse::<u16>().is_ok_and(|n| n > 0));
    if host_ok && port_ok {
        Ok(())
    } else {
        Err(invalid(
            "public_origin",
            "must be scheme://host[:port] in lowercase, without path or trailing slash",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Paths {
        Paths {
            data_dir: PathBuf::from("/data"),
            cache_dir: PathBuf::from("/cache"),
        }
    }

    fn parse(text: &str) -> Result<Config, String> {
        Config::from_json(text, paths()).map_err(|err| match err {
            ParseError::Json(e) => format!("json: {e}"),
            ParseError::Config(e) => format!("config: {e}"),
        })
    }

    #[test]
    fn r85_minimal_config_gets_documented_defaults() {
        let config = parse(r#"{"public_origin": "https://vocab.example.home"}"#).expect("valid");
        assert_eq!(config.public_origin, "https://vocab.example.home");
        assert_eq!(config.listen, "0.0.0.0:8080".parse().expect("addr"));
        assert_eq!(config.day_rule.time_zone_name(), Some("Europe/Prague"));
        assert_eq!(config.tts_url, "http://vocabulary-tts:8090");
        assert_eq!(config.log_level, tracing::Level::INFO);
        assert_eq!(config.session_days, 180);
        assert_eq!(config.limits, Limits::default());
        assert_eq!(config.limits.body_bytes, 64 * 1024);
        assert_eq!(config.paths, paths());
    }

    #[test]
    fn r85_design_example_config_is_accepted() {
        let config = parse(
            r#"{
              "public_origin": "https://vocab.example.home",
              "timezone": "Europe/Prague",
              "day_rollover_hour": 4,
              "tts_url": "http://vocabulary-tts:8090",
              "log_level": "info"
            }"#,
        );
        assert!(config.is_ok(), "{config:?}");
    }

    #[test]
    fn r85_unknown_keys_are_a_startup_error() {
        let err = parse(r#"{"public_origin": "https://a.example", "password": "x"}"#)
            .expect_err("unknown key");
        assert!(err.contains("unknown field"), "{err}");
        let err = parse(r#"{"public_origin": "https://a.example", "limits": {"body": 1}}"#)
            .expect_err("unknown key");
        assert!(err.contains("unknown field"), "{err}");
    }

    #[test]
    fn r85_public_origin_is_required_and_must_be_an_origin() {
        assert!(parse("{}").expect_err("missing").contains("public_origin"));
        for bad in [
            "vocab.example.home",
            "https://vocab.example.home/",
            "https://vocab.example.home/app",
            "https://Vocab.example.home",
            "https://vocab.example.home:0",
            "https://vocab.example.home:99999",
            "https://user@vocab.example.home",
            "ftp://vocab.example.home",
        ] {
            let json = format!(r#"{{"public_origin": "{bad}"}}"#);
            assert!(parse(&json).is_err(), "{bad} should be rejected");
        }
        for good in [
            "https://vocab.example.home",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
        ] {
            let json = format!(r#"{{"public_origin": "{good}"}}"#);
            assert!(parse(&json).is_ok(), "{good} should be accepted");
        }
    }

    #[test]
    fn r85_invalid_values_are_rejected() {
        for bad in [
            r#""timezone": "Nowhere/City""#,
            r#""day_rollover_hour": 24"#,
            r#""listen": "8080""#,
            r#""log_level": "loud""#,
            r#""tts_url": "vocabulary-tts:8090""#,
            r#""session_days": 0"#,
            r#""limits": {"body_bytes": 0}"#,
            r#""limits": {"header_read_timeout_secs": 0}"#,
            r#""limits": {"max_connections": 0}"#,
        ] {
            let json = format!(r#"{{"public_origin": "https://a.example", {bad}}}"#);
            assert!(parse(&json).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn r85_paths_and_config_file_come_from_the_environment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("config.json");
        std::fs::write(&file, r#"{"public_origin": "http://localhost:8080"}"#).expect("write");
        let env = |key: &str| match key {
            "VOCAB_CONFIG" => Some(file.clone()),
            "VOCAB_DATA_DIR" => Some(dir.path().join("data")),
            _ => None,
        };
        let config = Config::load_with(env).expect("loads");
        assert_eq!(config.paths.data_dir, dir.path().join("data"));
        assert_eq!(config.paths.cache_dir, PathBuf::from("/cache"));

        let missing = |key: &str| (key == "VOCAB_CONFIG").then(|| dir.path().join("nope.json"));
        assert!(matches!(
            Config::load_with(missing),
            Err(ConfigError::Missing { .. })
        ));
    }
}
