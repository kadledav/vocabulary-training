//! Authentication and request protection (design §9, R-82, R-83).
//!
//! * One password, stored as an argon2id hash in `VOCAB_PASSWORD_HASH_B64`.
//!   A successful login creates a session: 32 random bytes in the
//!   `__Host-vocab_session` cookie; the database keeps only the token's
//!   SHA-256 and the fingerprint of the configured hash, so a new password
//!   ends every session. Sessions last 180 days and slide: a session used
//!   more than 7 days after its last renewal is renewed.
//! * Password checks run off the async runtime, one at a time, behind the
//!   global login throttle ([`throttle`]).
//! * The connector's import token (`VOCAB_IMPORT_TOKEN`) is a separate bearer
//!   secret; which routes accept it is decided by `http`.
//! * CSRF: every state-changing request must come from `public_origin` and
//!   carry a JSON body type (§9.3).
//!
//! Live sessions are also kept in memory (digest → expiry), so a request
//! needs the database only for the first check after a start and for
//! renewals; the table stays the source of truth across restarts.
//!
//! The module owns the `auth_session` table, the session cache and the
//! throttle state. `http`
//! decides which routes need what and maps the outcomes to responses.

mod password;
mod session;
mod throttle;
mod token;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, Method, header};
use jiff::SignedDuration;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::clock::{Clock, Timestamp, format_timestamp};
use crate::db::{Db, DbError};

pub use password::{MAX_PASSWORD_BYTES, MIN_PASSWORD_CHARS, PasswordError, hash_password};
pub use token::TokenError;

use password::PasswordHashConfig;
use throttle::LoginThrottle;
use token::ImportToken;

/// Name of the session cookie. `__Host-`: the browser only accepts it with
/// `Secure`, `Path=/` and no `Domain`, so no sibling host can set it.
pub const SESSION_COOKIE: &str = "__Host-vocab_session";
/// Environment variable with the base64 of the argon2id PHC string.
pub const PASSWORD_HASH_ENV: &str = "VOCAB_PASSWORD_HASH_B64";
/// Environment variable with the connector's import token.
pub const IMPORT_TOKEN_ENV: &str = "VOCAB_IMPORT_TOKEN";

/// A session older than this since its last renewal is renewed when used.
const RENEW_AFTER: SignedDuration = SignedDuration::from_hours(7 * 24);
/// Random bytes of a session token.
const TOKEN_BYTES: usize = 32;
/// Longest stored `User-Agent` (it only helps the owner recognise sessions).
const MAX_USER_AGENT_CHARS: usize = 256;
/// The session cache is emptied when it grows past this (sessions only come
/// from successful logins, so this is never reached in practice).
const MAX_CACHED_SESSIONS: usize = 1024;

/// Why the secrets from the environment cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
    #[error(
        "{0} is not set; the app does not run without it (see deploy/README.md: hash-password)"
    )]
    Missing(&'static str),
    #[error("{PASSWORD_HASH_ENV} {0}")]
    Password(#[from] PasswordError),
    #[error("{IMPORT_TOKEN_ENV} {0}")]
    Token(#[from] TokenError),
}

/// The secrets of the app. `Debug` never shows them.
#[derive(Debug, Clone)]
pub struct Credentials {
    password: PasswordHashConfig,
    import_token: ImportToken,
}

impl Credentials {
    /// From the base64 PHC string and the import token.
    pub fn new(password_hash_b64: &str, import_token: &str) -> Result<Self, CredentialsError> {
        Ok(Self {
            password: PasswordHashConfig::from_b64(password_hash_b64)?,
            import_token: ImportToken::new(import_token)?,
        })
    }

    /// From `VOCAB_PASSWORD_HASH_B64` and `VOCAB_IMPORT_TOKEN`; both are
    /// required (there is no "open" mode).
    pub fn from_env() -> Result<Self, CredentialsError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    fn from_lookup(env: impl Fn(&str) -> Option<String>) -> Result<Self, CredentialsError> {
        let non_empty = |key: &'static str| {
            env(key)
                .filter(|value| !value.trim().is_empty())
                .ok_or(CredentialsError::Missing(key))
        };
        let hash = non_empty(PASSWORD_HASH_ENV)?;
        let token = non_empty(IMPORT_TOKEN_ENV)?;
        Self::new(&hash, &token)
    }
}

/// Outcome of a login attempt that did not succeed.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("wrong password")]
    WrongPassword,
    #[error("too many failed logins; locked for {retry_after_secs} s")]
    Locked { retry_after_secs: u64 },
    #[error("login failed: {0}")]
    Internal(String),
}

impl From<DbError> for LoginError {
    fn from(err: DbError) -> Self {
        Self::Internal(err.to_string())
    }
}

/// Outcome of the session check of a request.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionCheck {
    /// A live session. `renewal` is a `Set-Cookie` value to send back when
    /// the session was renewed by this request.
    Valid { renewal: Option<String> },
    /// No cookie, an unknown or expired session, or one of an old password.
    Invalid,
}

/// Why a state-changing request is refused (§9.3).
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CsrfRejection {
    #[error("the request does not come from the app's origin")]
    Origin,
    #[error("the request body must be JSON")]
    ContentType,
}

/// Authentication state shared by all requests.
#[derive(Debug)]
pub struct Auth {
    credentials: Credentials,
    public_origin: String,
    session_lifetime: SignedDuration,
    /// Password checks are memory heavy: one at a time.
    verify_slot: Semaphore,
    throttle: Mutex<LoginThrottle>,
    /// Live sessions seen by this process: token digest → expiry. Every
    /// change of a session happens with this lock held, so the cache and
    /// the table never disagree about a session this process knows.
    sessions: tokio::sync::Mutex<HashMap<String, Timestamp>>,
}

impl Auth {
    pub fn new(credentials: Credentials, public_origin: &str, session_days: u32) -> Self {
        Self {
            credentials,
            public_origin: public_origin.to_owned(),
            session_lifetime: SignedDuration::from_hours(i64::from(session_days) * 24),
            verify_slot: Semaphore::new(1),
            throttle: Mutex::new(LoginThrottle::default()),
            sessions: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Check `password`; on success create a session and return its
    /// `Set-Cookie` value.
    pub async fn login(
        &self,
        db: &Db,
        clock: &Arc<dyn Clock>,
        password: String,
        user_agent: Option<&str>,
    ) -> Result<String, LoginError> {
        let _slot = self
            .verify_slot
            .acquire()
            .await
            .map_err(|err| LoginError::Internal(err.to_string()))?;
        let now = clock.now();
        if let Err(retry_after) = self.with_throttle(|t| t.check(now)) {
            return Err(locked(retry_after));
        }
        let config = self.credentials.password.clone();
        let verified = tokio::task::spawn_blocking(move || config.verify(&password))
            .await
            .map_err(|err| LoginError::Internal(err.to_string()))?;
        if !verified {
            return Err(match self.with_throttle(|t| t.record_failure(now)) {
                Some(lockout) => {
                    tracing::warn!(minutes = lockout.as_mins(), "login locked after failures");
                    locked(lockout)
                }
                None => LoginError::WrongPassword,
            });
        }
        self.with_throttle(LoginThrottle::record_success);

        let mut raw = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut raw).map_err(|err| LoginError::Internal(err.to_string()))?;
        let token = hex::encode(raw);
        let token_sha256 = token_digest(&raw);
        let fingerprint = self.credentials.password.fingerprint().to_owned();
        let now_text = format_timestamp(now);
        let expiry = self.expiry(now);
        let expires = format_timestamp(expiry);
        let user_agent: Option<String> =
            user_agent.map(|ua| ua.chars().take(MAX_USER_AGENT_CHARS).collect());
        let mut sessions = self.sessions.lock().await;
        let digest = token_sha256.clone();
        db.write(move |tx| {
            session::purge(tx, &fingerprint, &now_text)?;
            session::insert(
                tx,
                &session::NewSession {
                    token_sha256: &token_sha256,
                    fingerprint: &fingerprint,
                    now: &now_text,
                    expires_at: &expires,
                    user_agent: user_agent.as_deref(),
                },
            )?;
            Ok::<_, DbError>(())
        })
        .await?;
        sessions.retain(|_, until| *until > now);
        cache(&mut sessions, digest, expiry);
        tracing::info!("login");
        Ok(self.session_cookie(&token))
    }

    /// Is the request's session cookie a live session? Renews it when due.
    pub async fn check_session(
        &self,
        db: &Db,
        headers: &HeaderMap,
        now: Timestamp,
    ) -> Result<SessionCheck, DbError> {
        let Some(raw) = session_token(headers) else {
            return Ok(SessionCheck::Invalid);
        };
        let digest = token_digest(&raw);
        let mut sessions = self.sessions.lock().await;
        let expiry = match sessions.get(&digest) {
            Some(expiry) => *expiry,
            None => {
                let fingerprint = self.credentials.password.fingerprint().to_owned();
                let now_text = format_timestamp(now);
                let key = digest.clone();
                let stored = db
                    .read(move |conn| {
                        Ok::<_, DbError>(session::live_expiry(conn, &key, &fingerprint, &now_text)?)
                    })
                    .await?;
                let Some(expiry) = stored.and_then(|text| text.parse::<Timestamp>().ok()) else {
                    return Ok(SessionCheck::Invalid);
                };
                cache(&mut sessions, digest.clone(), expiry);
                expiry
            }
        };
        if expiry <= now {
            sessions.remove(&digest);
            return Ok(SessionCheck::Invalid);
        }
        let renewed_at = expiry.checked_sub(self.session_lifetime).unwrap_or(expiry);
        if now.duration_since(renewed_at) <= RENEW_AFTER {
            return Ok(SessionCheck::Valid { renewal: None });
        }
        let new_expiry = self.expiry(now);
        let now_text = format_timestamp(now);
        let expires = format_timestamp(new_expiry);
        let key = digest.clone();
        db.write(move |tx| Ok::<_, DbError>(session::renew(tx, &key, &now_text, &expires)?))
            .await?;
        sessions.insert(digest, new_expiry);
        Ok(SessionCheck::Valid {
            renewal: Some(self.session_cookie(&hex::encode(raw))),
        })
    }

    /// End the request's session (if any); returns the `Set-Cookie` value
    /// that removes the cookie from the browser.
    pub async fn logout(&self, db: &Db, headers: &HeaderMap) -> Result<String, DbError> {
        if let Some(raw) = session_token(headers) {
            let digest = token_digest(&raw);
            let mut sessions = self.sessions.lock().await;
            let key = digest.clone();
            db.write(move |tx| Ok::<_, DbError>(session::delete(tx, &key)?))
                .await?;
            sessions.remove(&digest);
            tracing::info!("logout");
        }
        Ok(format!(
            "{SESSION_COOKIE}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict"
        ))
    }

    /// Does the request carry the import token (`Authorization: Bearer …`)?
    pub fn has_import_token(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(header::AUTHORIZATION).iter();
        let (Some(value), None) = (values.next(), values.next()) else {
            return false;
        };
        let Some((scheme, token)) = value.to_str().ok().and_then(|v| v.split_once(' ')) else {
            return false;
        };
        scheme.eq_ignore_ascii_case("bearer") && self.credentials.import_token.matches(token.trim())
    }

    /// CSRF guard (§9.3): a state-changing request (anything but GET/HEAD)
    /// must carry `Origin: <public_origin>` and a JSON content type.
    pub fn check_csrf(&self, method: &Method, headers: &HeaderMap) -> Result<(), CsrfRejection> {
        if method == Method::GET || method == Method::HEAD {
            return Ok(());
        }
        let mut origins = headers.get_all(header::ORIGIN).iter();
        let same_origin = match (origins.next(), origins.next()) {
            (Some(origin), None) => origin.as_bytes() == self.public_origin.as_bytes(),
            _ => false,
        };
        if !same_origin {
            return Err(CsrfRejection::Origin);
        }
        let mut types = headers.get_all(header::CONTENT_TYPE).iter();
        let json = match (types.next(), types.next()) {
            (Some(value), None) => value
                .to_str()
                .ok()
                .and_then(|v| v.split(';').next())
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json")),
            _ => false,
        };
        if json {
            Ok(())
        } else {
            Err(CsrfRejection::ContentType)
        }
    }

    fn with_throttle<T>(&self, f: impl FnOnce(&mut LoginThrottle) -> T) -> T {
        let mut guard = self
            .throttle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    fn expiry(&self, now: Timestamp) -> Timestamp {
        now.saturating_add(self.session_lifetime).unwrap_or(now)
    }

    fn session_cookie(&self, token: &str) -> String {
        format!(
            "{SESSION_COOKIE}={token}; Path=/; Max-Age={}; Secure; HttpOnly; SameSite=Strict",
            self.session_lifetime.as_secs()
        )
    }
}

fn locked(retry_after: SignedDuration) -> LoginError {
    LoginError::Locked {
        retry_after_secs: u64::try_from(retry_after.as_secs()).unwrap_or(0).max(1),
    }
}

fn cache(sessions: &mut HashMap<String, Timestamp>, digest: String, expiry: Timestamp) {
    if sessions.len() >= MAX_CACHED_SESSIONS {
        sessions.clear();
    }
    sessions.insert(digest, expiry);
}

fn token_digest(raw: &[u8]) -> String {
    hex::encode(Sha256::digest(raw))
}

/// The raw session token from the `Cookie` header(s): the value of the
/// first `__Host-vocab_session` cookie, which must be 64 hex digits.
fn session_token(headers: &HeaderMap) -> Option<[u8; TOKEN_BYTES]> {
    let value = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == SESSION_COOKIE).then_some(value)
        })?;
    let mut raw = [0u8; TOKEN_BYTES];
    hex::decode_to_slice(value, &mut raw).ok()?;
    Some(raw)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).expect("header value"));
        }
        map
    }

    #[test]
    fn r82_session_cookie_is_read_from_any_cookie_header() {
        let token = "ab".repeat(32);
        let found = session_token(&headers(&[(
            "cookie",
            &format!("theme=dark; {SESSION_COOKIE}={token}; other=1"),
        )]));
        assert_eq!(found.map(hex::encode), Some(token.clone()));
        let found = session_token(&headers(&[
            ("cookie", "theme=dark"),
            ("cookie", &format!("{SESSION_COOKIE}={token}")),
        ]));
        assert!(found.is_some());
        for bad in [
            format!("{SESSION_COOKIE}={}", &token[..62]),
            format!("{SESSION_COOKIE}={token}00"),
            format!("{SESSION_COOKIE}={}", "zz".repeat(32)),
            format!("vocab_session={token}"),
            format!("x{SESSION_COOKIE}={token}"),
        ] {
            assert_eq!(session_token(&headers(&[("cookie", &bad)])), None, "{bad}");
        }
    }

    #[test]
    fn r82_missing_credentials_refuse_to_start() {
        let lookup = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let err = Credentials::from_lookup(lookup(&[])).expect_err("missing");
        assert!(err.to_string().contains(PASSWORD_HASH_ENV), "{err}");
        let err =
            Credentials::from_lookup(lookup(&[(PASSWORD_HASH_ENV, "  ")])).expect_err("blank");
        assert!(err.to_string().contains(PASSWORD_HASH_ENV), "{err}");
        let err = Credentials::from_lookup(lookup(&[(PASSWORD_HASH_ENV, "bm90IGEgaGFzaA==")]))
            .expect_err("no token");
        assert!(err.to_string().contains(IMPORT_TOKEN_ENV), "{err}");
    }
}
