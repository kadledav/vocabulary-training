//! Non-secret configuration from the environment (design §3.2, §12; R-85).
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `VOCAB_PUBLIC_ORIGIN` | — (required) | origin the app is served from, e.g. `https://vocab.example.home` |
//! | `VOCAB_TIMEZONE` | `Europe/Prague` | IANA time zone of the study day |
//! | `VOCAB_DAY_ROLLOVER_HOUR` | `4` | local hour (0-23) a study day starts |
//! | `VOCAB_LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug`, `trace` |
//! | `VOCAB_LISTEN` | `0.0.0.0:8080` | listen address (development and tests) |
//! | `VOCAB_DATA_DIR` | `/data` | the SQLite file set |
//! | `VOCAB_CACHE_DIR` | `/cache` | audio files |
//!
//! Any other `VOCAB_*` variable (a typo, a removed setting) and any invalid
//! value is a startup error naming the variable. Secrets
//! (`VOCAB_PASSWORD_HASH_B64`, `VOCAB_IMPORT_TOKEN`) are read by `auth`.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::auth;
use crate::clock::{ClockError, DayRule};

const DEFAULT_DATA_DIR: &str = "/data";
const DEFAULT_CACHE_DIR: &str = "/cache";
const DEFAULT_TIMEZONE: &str = "Europe/Prague";
const DEFAULT_ROLLOVER_HOUR: u8 = 4;
const DEFAULT_LISTEN: &str = "0.0.0.0:8080";
/// Session lifetime (design §9.1).
const SESSION_DAYS: u32 = 180;

pub const PUBLIC_ORIGIN_ENV: &str = "VOCAB_PUBLIC_ORIGIN";
const TIMEZONE_ENV: &str = "VOCAB_TIMEZONE";
const ROLLOVER_ENV: &str = "VOCAB_DAY_ROLLOVER_HOUR";
const LOG_LEVEL_ENV: &str = "VOCAB_LOG_LEVEL";
const LISTEN_ENV: &str = "VOCAB_LISTEN";
const DATA_DIR_ENV: &str = "VOCAB_DATA_DIR";
const CACHE_DIR_ENV: &str = "VOCAB_CACHE_DIR";

/// Every `VOCAB_*` variable the app reads (here or in `auth`).
const KNOWN: [&str; 9] = [
    PUBLIC_ORIGIN_ENV,
    TIMEZONE_ENV,
    ROLLOVER_ENV,
    LOG_LEVEL_ENV,
    LISTEN_ENV,
    DATA_DIR_ENV,
    CACHE_DIR_ENV,
    auth::PASSWORD_HASH_ENV,
    auth::IMPORT_TOKEN_ENV,
];

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not set; it is required (e.g. https://vocab.example.home)")]
    Missing(&'static str),
    #[error("invalid {key}: {message}")]
    Invalid { key: &'static str, message: String },
    #[error("unknown setting {0}: the app reads only {known}", known = KNOWN.join(", "))]
    Unknown(String),
    #[error("invalid {TIMEZONE_ENV}/{ROLLOVER_ENV}: {0}")]
    DayRule(#[from] ClockError),
}

/// Effective configuration of the server.
#[derive(Debug, Clone)]
pub struct Config {
    /// Exact origin the app is served from, e.g. `https://vocab.example.home`
    /// (no path, no trailing slash, no default port). Used by the Origin
    /// check (§9.3).
    pub public_origin: String,
    /// Study-day definition built from the time zone and rollover hour.
    pub day_rule: DayRule,
    pub listen: SocketAddr,
    pub log_level: tracing::Level,
    pub session_days: u32,
    pub limits: Limits,
    pub paths: Paths,
}

/// Request limits (§9.4). The import routes have their own body limit and
/// commit timeout; the connector and login limits arrive with M11/M5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Body limit of `/api/import/*`: room for a 2 MiB paste inside a JSON
    /// string, where quotes, backslashes and line breaks double in size (the
    /// 2 MiB paste limit itself is checked on the decoded text).
    pub import_body_bytes: usize,
    /// Time an import commit may take before the server answers 503 (the
    /// commit itself may still finish; see `workflows`).
    pub import_commit_timeout_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            body_bytes: 64 * 1024,
            request_timeout_secs: 30,
            max_concurrent_requests: 64,
            header_read_timeout_secs: 10,
            max_connections: 256,
            import_body_bytes: 5 * 1024 * 1024,
            import_commit_timeout_secs: 120,
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

    pub fn import_commit_timeout(&self) -> Duration {
        Duration::from_secs(self.import_commit_timeout_secs)
    }
}

/// Directories the app writes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Config {
    /// Load the configuration from the process environment.
    pub fn load() -> Result<Self, ConfigError> {
        let names: Vec<OsString> = std::env::vars_os().map(|(name, _)| name).collect();
        Self::from_env(&names, |key| std::env::var_os(key))
    }

    fn from_env(
        names: &[OsString],
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self, ConfigError> {
        for name in names {
            let name = name.to_string_lossy();
            if name.starts_with("VOCAB_") && !KNOWN.contains(&name.as_ref()) {
                return Err(ConfigError::Unknown(name.into_owned()));
            }
        }
        let text = |key: &'static str| -> Result<Option<String>, ConfigError> {
            match env(key) {
                None => Ok(None),
                Some(value) => value
                    .into_string()
                    .map(Some)
                    .map_err(|_| invalid(key, "is not valid UTF-8")),
            }
        };
        let public_origin = text(PUBLIC_ORIGIN_ENV)?
            .filter(|v| !v.is_empty())
            .ok_or(ConfigError::Missing(PUBLIC_ORIGIN_ENV))?;
        validate_origin(&public_origin)?;
        let timezone = text(TIMEZONE_ENV)?.unwrap_or_else(|| DEFAULT_TIMEZONE.to_owned());
        let rollover = match text(ROLLOVER_ENV)? {
            None => DEFAULT_ROLLOVER_HOUR,
            Some(v) => v
                .parse::<u8>()
                .ok()
                .filter(|h| *h <= 23)
                .ok_or_else(|| invalid(ROLLOVER_ENV, "must be a whole hour 0-23"))?,
        };
        let day_rule = DayRule::new(&timezone, rollover)?;
        let log_level = match text(LOG_LEVEL_ENV)? {
            None => tracing::Level::INFO,
            Some(v) => v.parse().map_err(|_| {
                invalid(
                    LOG_LEVEL_ENV,
                    "must be one of error, warn, info, debug, trace",
                )
            })?,
        };
        let listen = text(LISTEN_ENV)?
            .unwrap_or_else(|| DEFAULT_LISTEN.to_owned())
            .parse()
            .map_err(|_| {
                invalid(
                    LISTEN_ENV,
                    "must be an IP:port address such as 0.0.0.0:8080",
                )
            })?;
        let dir = |key: &'static str, default: &str| -> Result<PathBuf, ConfigError> {
            Ok(text(key)?
                .filter(|v| !v.is_empty())
                .map_or_else(|| PathBuf::from(default), PathBuf::from))
        };
        Ok(Self {
            public_origin,
            day_rule,
            listen,
            log_level,
            session_days: SESSION_DAYS,
            limits: Limits::default(),
            paths: Paths {
                data_dir: dir(DATA_DIR_ENV, DEFAULT_DATA_DIR)?,
                cache_dir: dir(CACHE_DIR_ENV, DEFAULT_CACHE_DIR)?,
            },
        })
    }
}

fn invalid(key: &'static str, message: &str) -> ConfigError {
    ConfigError::Invalid {
        key,
        message: message.to_owned(),
    }
}

/// An origin as browsers send it in the `Origin` header: lowercase
/// `scheme://host[:port]`, nothing else. A default port (`:443` for https,
/// `:80` for http) is refused: browsers never send it, so the Origin check
/// could never match.
fn validate_origin(origin: &str) -> Result<(), ConfigError> {
    let (rest, default_port) = if let Some(rest) = origin.strip_prefix("https://") {
        (rest, "443")
    } else if let Some(rest) = origin.strip_prefix("http://") {
        (rest, "80")
    } else {
        return Err(invalid(
            PUBLIC_ORIGIN_ENV,
            "must start with https:// or http://",
        ));
    };
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (rest, None),
    };
    if port == Some(default_port) {
        return Err(invalid(
            PUBLIC_ORIGIN_ENV,
            &format!("leave out the default port :{default_port}"),
        ));
    }
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
    let port_ok = port.is_none_or(|p| {
        !p.starts_with('0')
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u16>().is_ok_and(|n| n > 0)
    });
    if host_ok && port_ok {
        Ok(())
    } else {
        Err(invalid(
            PUBLIC_ORIGIN_ENV,
            "must be scheme://host[:port] in lowercase, without path or trailing slash",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(vars: &[(&str, &str)]) -> Result<Config, String> {
        let names: Vec<OsString> = vars.iter().map(|(k, _)| OsString::from(k)).collect();
        Config::from_env(&names, |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        })
        .map_err(|err| err.to_string())
    }

    const ORIGIN: (&str, &str) = (PUBLIC_ORIGIN_ENV, "https://vocab.example.home");

    #[test]
    fn r85_only_the_origin_is_required_and_the_rest_has_documented_defaults() {
        let config = load(&[ORIGIN]).expect("valid");
        assert_eq!(config.public_origin, "https://vocab.example.home");
        assert_eq!(config.listen, "0.0.0.0:8080".parse().expect("addr"));
        assert_eq!(config.day_rule.time_zone_name(), Some("Europe/Prague"));
        assert_eq!(config.log_level, tracing::Level::INFO);
        assert_eq!(config.session_days, 180);
        assert_eq!(config.limits, Limits::default());
        assert_eq!(config.limits.body_bytes, 64 * 1024);
        assert_eq!(config.limits.import_body_bytes, 5 * 1024 * 1024);
        assert_eq!(config.paths.data_dir, PathBuf::from("/data"));
        assert_eq!(config.paths.cache_dir, PathBuf::from("/cache"));
        let err = load(&[]).expect_err("missing");
        assert!(err.contains("VOCAB_PUBLIC_ORIGIN is not set"), "{err}");
        let err = load(&[(PUBLIC_ORIGIN_ENV, "")]).expect_err("empty");
        assert!(err.contains("VOCAB_PUBLIC_ORIGIN is not set"), "{err}");
    }

    #[test]
    fn r85_every_setting_comes_from_its_variable() {
        let config = load(&[
            ORIGIN,
            (TIMEZONE_ENV, "Europe/London"),
            (ROLLOVER_ENV, "0"),
            (LOG_LEVEL_ENV, "warn"),
            (LISTEN_ENV, "127.0.0.1:9000"),
            (DATA_DIR_ENV, "/srv/data"),
            (CACHE_DIR_ENV, "/srv/cache"),
            (auth::PASSWORD_HASH_ENV, "x"),
            (auth::IMPORT_TOKEN_ENV, "y"),
            ("PATH", "/usr/bin"),
        ])
        .expect("valid");
        assert_eq!(config.day_rule.time_zone_name(), Some("Europe/London"));
        assert_eq!(config.log_level, tracing::Level::WARN);
        assert_eq!(config.listen, "127.0.0.1:9000".parse().expect("addr"));
        assert_eq!(config.paths.data_dir, PathBuf::from("/srv/data"));
        assert_eq!(config.paths.cache_dir, PathBuf::from("/srv/cache"));
    }

    #[test]
    fn r85_invalid_values_and_unknown_variables_name_the_variable() {
        for (var, value) in [
            (TIMEZONE_ENV, "Nowhere/City"),
            (ROLLOVER_ENV, "24"),
            (ROLLOVER_ENV, "four"),
            (LOG_LEVEL_ENV, "loud"),
            (LISTEN_ENV, "8080"),
        ] {
            let err = load(&[ORIGIN, (var, value)]).expect_err(value);
            assert!(err.contains(var), "{var}={value}: {err}");
        }
        for unknown in ["VOCAB_CONFIG", "VOCAB_TIMEZON", "VOCAB_TTS_URL"] {
            let err = load(&[ORIGIN, (unknown, "x")]).expect_err(unknown);
            assert!(err.contains(&format!("unknown setting {unknown}")), "{err}");
        }
    }

    #[test]
    fn r85_public_origin_must_be_exactly_what_browsers_send() {
        for bad in [
            "vocab.example.home",
            "https://vocab.example.home/",
            "https://vocab.example.home/app",
            "https://Vocab.example.home",
            "https://vocab.example.home:0",
            "https://vocab.example.home:099",
            "https://vocab.example.home:99999",
            "https://vocab.example.home:+8443",
            "https://user@vocab.example.home",
            "ftp://vocab.example.home",
            // Default ports: browsers leave them out of the Origin header.
            "https://vocab.example.home:443",
            "http://localhost:80",
        ] {
            assert!(
                load(&[(PUBLIC_ORIGIN_ENV, bad)]).is_err(),
                "{bad} should be rejected"
            );
        }
        for good in [
            "https://vocab.example.home",
            "https://vocab.example.home:8443",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://vocab.example.home:443",
        ] {
            assert!(
                load(&[(PUBLIC_ORIGIN_ENV, good)]).is_ok(),
                "{good} should be accepted"
            );
        }
    }
}
