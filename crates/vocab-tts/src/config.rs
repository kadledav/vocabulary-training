//! Configuration from the environment (design §10.2).
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `VOCAB_TTS_LISTEN` | `0.0.0.0:8090` | listen address |
//! | `VOCAB_TTS_MODEL_DIR` | `/model` | `model.onnx`, `voices.bin`, `tokens.txt`, the two lexicons |
//! | `VOCAB_TTS_THREADS` | `2` | inference threads (1–16) |
//! | `VOCAB_TTS_LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug`, `trace` |
//!
//! Any other `VOCAB_TTS_*` variable (a typo) and any invalid value is a
//! startup error naming the variable. There is nothing secret: the service
//! has no credentials and is reachable only on the internal network.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;

const LISTEN_ENV: &str = "VOCAB_TTS_LISTEN";
const MODEL_DIR_ENV: &str = "VOCAB_TTS_MODEL_DIR";
const THREADS_ENV: &str = "VOCAB_TTS_THREADS";
const LOG_LEVEL_ENV: &str = "VOCAB_TTS_LOG_LEVEL";
const KNOWN: [&str; 4] = [LISTEN_ENV, MODEL_DIR_ENV, THREADS_ENV, LOG_LEVEL_ENV];
const PREFIX: &str = "VOCAB_TTS_";

const DEFAULT_LISTEN: &str = "0.0.0.0:8090";
const DEFAULT_MODEL_DIR: &str = "/model";
const DEFAULT_THREADS: u16 = 2;
const MAX_THREADS: u16 = 16;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("invalid {key}: {message}")]
    Invalid {
        key: &'static str,
        message: &'static str,
    },
    #[error("unknown setting {0}: the service reads only {known}", known = KNOWN.join(", "))]
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub listen: SocketAddr,
    pub model_dir: PathBuf,
    pub threads: u16,
    pub log_level: tracing::Level,
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
            if name.starts_with(PREFIX) && !KNOWN.contains(&name.as_ref()) {
                return Err(ConfigError::Unknown(name.into_owned()));
            }
        }
        let text = |key: &'static str| -> Result<Option<String>, ConfigError> {
            env(key)
                .map(|value| {
                    value.into_string().map_err(|_| ConfigError::Invalid {
                        key,
                        message: "is not valid UTF-8",
                    })
                })
                .transpose()
                .map(|v| v.filter(|v| !v.is_empty()))
        };
        let listen = text(LISTEN_ENV)?
            .as_deref()
            .unwrap_or(DEFAULT_LISTEN)
            .parse()
            .map_err(|_| ConfigError::Invalid {
                key: LISTEN_ENV,
                message: "must be an IP:port address such as 0.0.0.0:8090",
            })?;
        let model_dir = PathBuf::from(text(MODEL_DIR_ENV)?.as_deref().unwrap_or(DEFAULT_MODEL_DIR));
        let threads = match text(THREADS_ENV)? {
            None => DEFAULT_THREADS,
            Some(v) => v
                .parse::<u16>()
                .ok()
                .filter(|n| (1..=MAX_THREADS).contains(n))
                .ok_or(ConfigError::Invalid {
                    key: THREADS_ENV,
                    message: "must be a whole number from 1 to 16",
                })?,
        };
        let log_level = match text(LOG_LEVEL_ENV)? {
            None => tracing::Level::INFO,
            Some(v) => v.parse().map_err(|_| ConfigError::Invalid {
                key: LOG_LEVEL_ENV,
                message: "must be one of error, warn, info, debug, trace",
            })?,
        };
        Ok(Self {
            listen,
            model_dir,
            threads,
            log_level,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let names: Vec<OsString> = vars.iter().map(|(k, _)| OsString::from(k)).collect();
        Config::from_env(&names, |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        })
    }

    #[test]
    fn r83_defaults_need_no_configuration() {
        let config = load(&[
            ("VOCAB_PUBLIC_ORIGIN", "https://x.example.home"),
            ("PATH", "/bin"),
        ])
        .expect("test setup");
        assert_eq!(config.listen, "0.0.0.0:8090".parse().expect("test setup"));
        assert_eq!(config.model_dir, PathBuf::from("/model"));
        assert_eq!(config.threads, 2);
        assert_eq!(config.log_level, tracing::Level::INFO);
    }

    #[test]
    fn r83_every_setting_is_read() {
        let config = load(&[
            ("VOCAB_TTS_LISTEN", "127.0.0.1:9000"),
            ("VOCAB_TTS_MODEL_DIR", "/opt/model"),
            ("VOCAB_TTS_THREADS", "4"),
            ("VOCAB_TTS_LOG_LEVEL", "debug"),
        ])
        .expect("test setup");
        assert_eq!(config.listen, "127.0.0.1:9000".parse().expect("test setup"));
        assert_eq!(config.model_dir, PathBuf::from("/opt/model"));
        assert_eq!(config.threads, 4);
        assert_eq!(config.log_level, tracing::Level::DEBUG);
    }

    #[test]
    fn r83_invalid_or_unknown_settings_are_startup_errors() {
        assert_eq!(
            load(&[("VOCAB_TTS_TOKEN", "x")]),
            Err(ConfigError::Unknown("VOCAB_TTS_TOKEN".to_owned()))
        );
        for (key, value) in [
            ("VOCAB_TTS_LISTEN", "localhost"),
            ("VOCAB_TTS_THREADS", "0"),
            ("VOCAB_TTS_THREADS", "17"),
            ("VOCAB_TTS_THREADS", "two"),
            ("VOCAB_TTS_LOG_LEVEL", "loud"),
        ] {
            assert!(
                matches!(load(&[(key, value)]), Err(ConfigError::Invalid { key: k, .. }) if k == key),
                "{key}={value}"
            );
        }
    }
}
