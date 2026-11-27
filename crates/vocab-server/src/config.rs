//! Non-secret configuration from the environment (design §3.2, §12; R-85).
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `VOCAB_PUBLIC_ORIGIN` | — (required) | origin the app is served from, e.g. `https://vocab.example.home` |
//! | `VOCAB_TIMEZONE` | `Europe/Prague` | IANA time zone of the study day |
//! | `VOCAB_DAY_ROLLOVER_HOUR` | `4` | local hour (0-23) a study day starts |
//! | `VOCAB_LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug`, `trace` |
//! | `VOCAB_LISTEN` | `0.0.0.0:8080` | listen address (development and tests) |
//! | `VOCAB_DATA_DIR` | `/data` | the SQLite file set (and the cache below it) |
//! | `VOCAB_CACHE_DIR` | `<data dir>/cache` | audio files |
//! | `VOCAB_TTS_URL` | — (no neural audio) | the TTS service, `http://host[:port]` on the internal network: a host name without dots (the compose service name), `localhost`, or a loopback, private or link-local IP address; anything else is refused at start (the service has no authentication) |
//!
//! Any other `VOCAB_*` variable (a typo, a removed setting such as the M5
//! secrets) and any invalid value is a startup error naming the variable.
//! There are no secrets in the environment: the password and the connector
//! token are set up in the app and stored in the database (`auth`).

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::audio::TtsEndpoint;
use crate::clock::{ClockError, DayRule};

const DEFAULT_DATA_DIR: &str = "/data";
/// The cache directory inside the data directory, unless `VOCAB_CACHE_DIR`.
const CACHE_SUBDIR: &str = "cache";
/// Prefix of the file written and removed again at start to prove a
/// directory is writable (a random suffix follows).
const WRITE_PROBE: &str = ".vocab-write-check-";
const DEFAULT_TIMEZONE: &str = "Europe/Prague";
const DEFAULT_ROLLOVER_HOUR: u8 = 4;
const DEFAULT_LISTEN: &str = "0.0.0.0:8080";
/// Session lifetime (design §9.1).
const SESSION_DAYS: u32 = 180;
/// Absolute session lifetime in days (§9.1).
const SESSION_MAX_DAYS: u32 = 365;

pub const PUBLIC_ORIGIN_ENV: &str = "VOCAB_PUBLIC_ORIGIN";
const TIMEZONE_ENV: &str = "VOCAB_TIMEZONE";
const ROLLOVER_ENV: &str = "VOCAB_DAY_ROLLOVER_HOUR";
const LOG_LEVEL_ENV: &str = "VOCAB_LOG_LEVEL";
const LISTEN_ENV: &str = "VOCAB_LISTEN";
const DATA_DIR_ENV: &str = "VOCAB_DATA_DIR";
const CACHE_DIR_ENV: &str = "VOCAB_CACHE_DIR";
const TTS_URL_ENV: &str = "VOCAB_TTS_URL";

/// Every `VOCAB_*` variable the app reads.
const KNOWN: [&str; 8] = [
    PUBLIC_ORIGIN_ENV,
    TIMEZONE_ENV,
    ROLLOVER_ENV,
    LOG_LEVEL_ENV,
    LISTEN_ENV,
    DATA_DIR_ENV,
    CACHE_DIR_ENV,
    TTS_URL_ENV,
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
    /// Sliding session lifetime (§9.1).
    pub session_days: u32,
    /// Longest life of a session, however often it is renewed (§9.1).
    pub session_max_days: u32,
    pub limits: Limits,
    pub paths: Paths,
    /// The TTS service; `None`: no neural audio, the client speaks with the
    /// browser (R-41).
    pub tts: Option<TtsEndpoint>,
}

/// Request limits (§9.4). The import routes have their own body limit and
/// commit timeout; the connector and login limits arrive with M11/M5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Default maximum request body size in bytes.
    pub body_bytes: usize,
    /// Default time a request may take before the server answers 503.
    pub request_timeout_secs: u64,
    /// Requests handled at the same time; further requests wait for
    /// `slot_wait_ms` and are then refused (503).
    pub max_concurrent_requests: usize,
    /// Of those, slots only requests of a logged-in browser may use, so
    /// that unauthenticated traffic can never take every slot.
    pub reserved_session_slots: usize,
    /// How long a request waits for a free slot before it is refused.
    pub slot_wait_ms: u64,
    /// Time a client has to send the body of a request to a public route
    /// (login, setup).
    pub public_body_read_secs: u64,
    /// Time a client has to send a complete request head, counted from its
    /// first byte (and the wait for the first request of a connection).
    pub header_read_timeout_secs: u64,
    /// How long a connection that has answered a request may sit idle
    /// before the next one. Longer than a reverse proxy keeps idle upstream
    /// connections (about 90 s), so the proxy never reuses a connection the
    /// app is closing (a sporadic 502).
    pub keep_alive_idle_secs: u64,
    /// Open client connections; further connections wait in the backlog.
    pub max_connections: usize,
    /// Open connections from one peer address; further ones are refused at
    /// once. Behind the reverse proxy every browser shares the proxy's
    /// address, so this is generous: it only keeps one address from taking
    /// every connection (the health check comes from another one).
    pub max_connections_per_peer: usize,
    /// Body limit of `/api/import/*`: a 2 MiB paste inside a JSON string,
    /// with some room for the escaping of quotes and line breaks (the 2 MiB
    /// paste limit itself is checked on the decoded text).
    pub import_body_bytes: usize,
    /// Time an import commit may take before the server answers 503 (the
    /// commit itself may still finish; see `workflows`).
    pub import_commit_timeout_secs: u64,
    /// A full export download that takes no data for this long (a stalled
    /// client) is broken off and gives the one export turn back.
    pub export_idle_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            body_bytes: 64 * 1024,
            request_timeout_secs: 30,
            max_concurrent_requests: 64,
            reserved_session_slots: 8,
            slot_wait_ms: 2000,
            public_body_read_secs: 5,
            header_read_timeout_secs: 10,
            keep_alive_idle_secs: 120,
            max_connections: 256,
            max_connections_per_peer: 128,
            import_body_bytes: 5 * 1024 * 1024 / 2,
            import_commit_timeout_secs: 120,
            export_idle_ms: 30_000,
        }
    }
}

impl Limits {
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }

    pub fn export_idle(&self) -> Duration {
        Duration::from_millis(self.export_idle_ms)
    }

    pub fn slot_wait(&self) -> Duration {
        Duration::from_millis(self.slot_wait_ms)
    }

    pub fn public_body_read_timeout(&self) -> Duration {
        Duration::from_secs(self.public_body_read_secs)
    }

    pub fn header_read_timeout(&self) -> Duration {
        Duration::from_secs(self.header_read_timeout_secs)
    }

    pub fn keep_alive_idle(&self) -> Duration {
        Duration::from_secs(self.keep_alive_idle_secs)
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

/// A directory the app cannot use.
#[derive(Debug, thiserror::Error)]
#[error(
    "cannot {action} the {what} directory {}: {source}; the user the app runs as \
     must be able to create and write it (deploy/README.md: storage)",
    .path.display()
)]
pub struct PathError {
    what: &'static str,
    path: PathBuf,
    action: &'static str,
    source: std::io::Error,
}

impl Paths {
    /// Create the data and cache directories when missing (mode 0700) and
    /// check that both are writable, so a wrong mount or owner is a clear
    /// startup error.
    pub fn prepare(&self) -> Result<(), PathError> {
        prepare_dir("data", &self.data_dir)?;
        prepare_dir("cache", &self.cache_dir)
    }
}

fn prepare_dir(what: &'static str, path: &Path) -> Result<(), PathError> {
    let fail = |action, source| PathError {
        what,
        path: path.to_owned(),
        action,
        source,
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path).map_err(|err| fail("create", err))?;
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|err| fail("write to", std::io::Error::other(err)))?;
    let probe = path.join(format!("{WRITE_PROBE}{}", hex::encode(suffix)));
    // A new file only (never an existing one, never through a symlink).
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        rustix::fs::OFlags::NOFOLLOW.bits().cast_signed(),
    );
    options.open(&probe).map_err(|err| fail("write to", err))?;
    std::fs::remove_file(&probe).map_err(|err| fail("write to", err))
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
        let dir = |key: &'static str| -> Result<Option<PathBuf>, ConfigError> {
            Ok(text(key)?.filter(|v| !v.is_empty()).map(PathBuf::from))
        };
        let data_dir = dir(DATA_DIR_ENV)?.unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));
        let cache_dir = dir(CACHE_DIR_ENV)?.unwrap_or_else(|| data_dir.join(CACHE_SUBDIR));
        let tts = match text(TTS_URL_ENV)?.filter(|v| !v.is_empty()) {
            None => None,
            Some(url) => Some(
                TtsEndpoint::parse(&url).map_err(|err| invalid(TTS_URL_ENV, &err.to_string()))?,
            ),
        };
        Ok(Self {
            public_origin,
            day_rule,
            listen,
            log_level,
            session_days: SESSION_DAYS,
            session_max_days: SESSION_MAX_DAYS,
            limits: Limits::default(),
            paths: Paths {
                data_dir,
                cache_dir,
            },
            tts,
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
        assert_eq!(config.session_max_days, 365);
        assert_eq!(config.limits, Limits::default());
        assert_eq!(config.limits.body_bytes, 64 * 1024);
        assert_eq!(config.limits.import_body_bytes, 2_621_440, "2.5 MiB");
        assert_eq!(
            (
                config.limits.max_concurrent_requests,
                config.limits.reserved_session_slots,
                config.limits.max_connections,
                config.limits.max_connections_per_peer
            ),
            (64, 8, 256, 128)
        );
        assert_eq!(config.paths.data_dir, PathBuf::from("/data"));
        assert_eq!(config.paths.cache_dir, PathBuf::from("/data/cache"));
        let config = load(&[ORIGIN, (DATA_DIR_ENV, "/srv/vocab")]).expect("valid");
        assert_eq!(config.paths.cache_dir, PathBuf::from("/srv/vocab/cache"));
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
            (TTS_URL_ENV, "https://vocabulary-tts:8090"),
            (TTS_URL_ENV, "http://vocabulary-tts:8090/v1/speak"),
        ] {
            let err = load(&[ORIGIN, (var, value)]).expect_err(value);
            assert!(err.contains(var), "{var}={value}: {err}");
        }
        for unknown in [
            "VOCAB_CONFIG",
            "VOCAB_TIMEZON",
            "VOCAB_TTS_TOKEN",
            // The M5 secrets: no secrets in the environment any more.
            "VOCAB_PASSWORD_HASH_B64",
            "VOCAB_IMPORT_TOKEN",
        ] {
            let err = load(&[ORIGIN, (unknown, "x")]).expect_err(unknown);
            assert!(err.contains(&format!("unknown setting {unknown}")), "{err}");
        }
    }

    #[test]
    fn r40_tts_url_is_optional() {
        let config = load(&[ORIGIN]).expect("valid");
        assert_eq!(config.tts, None);
        let config = load(&[ORIGIN, (TTS_URL_ENV, "http://vocabulary-tts:8090")]).expect("valid");
        assert_eq!(
            config.tts.map(|t| t.to_string()).as_deref(),
            Some("http://vocabulary-tts:8090")
        );
    }

    #[test]
    fn r83_tts_url_must_be_on_the_internal_network() {
        for public in [
            "http://tts.example.org:8090",
            "http://203.0.113.9:8090",
            "http://[2001:db8::5]:8090",
        ] {
            let err = load(&[ORIGIN, (TTS_URL_ENV, public)]).expect_err(public);
            assert!(
                err.contains(TTS_URL_ENV) && err.contains("internal network"),
                "{public}: {err}"
            );
        }
        for internal in [
            "http://localhost:8090",
            &format!("http://{}:8090", std::net::Ipv4Addr::new(10, 1, 2, 3)),
            "http://[::1]:8090",
        ] {
            assert!(
                load(&[ORIGIN, (TTS_URL_ENV, internal)]).is_ok(),
                "{internal}"
            );
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

    #[test]
    fn r85_missing_directories_are_created_private_and_checked_writable() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            data_dir: root.path().join("dataset"),
            cache_dir: root.path().join("dataset").join(CACHE_SUBDIR),
        };
        paths.prepare().expect("created");
        assert!(paths.cache_dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [&paths.data_dir, &paths.cache_dir] {
                let mode = std::fs::metadata(dir).expect("meta").permissions().mode();
                assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
            }
        }
        // Existing directories are fine; nothing is left behind.
        paths.prepare().expect("again");
        let names: Vec<_> = std::fs::read_dir(&paths.data_dir)
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(CACHE_SUBDIR)]);
        // A path below a regular file can never be created.
        let file = root.path().join("file");
        std::fs::write(&file, b"x").expect("file");
        let bad = Paths {
            data_dir: file.join("data"),
            cache_dir: file.join("cache"),
        };
        let err = bad.prepare().expect_err("not creatable").to_string();
        assert!(
            err.starts_with("cannot create the data directory "),
            "{err}"
        );
    }
}
