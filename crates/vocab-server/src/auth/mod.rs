//! Authentication and request protection (design §9, R-82, R-83).
//!
//! * One password, stored in the database (`auth_owner`) as an argon2id
//!   hash. Until it is set the app is in *setup state*: every start makes a
//!   one-time setup code ([`Auth::new`]; the caller logs it), and
//!   [`Auth::setup`] with that code sets the first password. Once a password
//!   exists, setup is refused for good; `reset-password` ([`reset_password`])
//!   goes back to setup state.
//! * A successful login (or setup) creates a session: 32 random bytes in the
//!   `__Host-vocab_session` cookie; the database keeps only the token's
//!   SHA-256 and the fingerprint of the password hash, so a new password
//!   ends every session of the old one (a password change keeps only the
//!   session that made it). Sessions last 180 days and slide: a session used
//!   more than 7 days after its last renewal is renewed.
//! * Password and setup-code checks run off the async runtime, one at a
//!   time, behind the global login throttle ([`throttle`]); everything that
//!   changes the stored credentials runs in that same single slot.
//! * The connector token is a separate bearer secret the owner creates in
//!   the app; only its SHA-256 is stored. Which routes accept it is decided
//!   by `http`.
//! * CSRF: every state-changing request must come from `public_origin` and
//!   carry a JSON body type (§9.3).
//!
//! The owner row and live sessions are also kept in memory (sessions as
//! digest → expiry), so a request needs the database only for the first
//! check after a start and for renewals; the tables stay the source of truth
//! across restarts.
//!
//! The module owns the `auth_owner` and `auth_session` tables, the session
//! cache, the setup code and the throttle state. `http` decides which routes
//! need what and maps the outcomes to responses.

mod owner;
mod password;
mod session;
mod throttle;
mod token;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::http::{HeaderMap, Method, header};
use jiff::SignedDuration;
use sha2::{Digest, Sha256};
use tokio::sync::{OnceCell, Semaphore, SemaphorePermit};

use crate::clock::{Clock, Timestamp, format_timestamp};
use crate::db::{Db, DbError};

pub use password::{MIN_PASSWORD_CHARS, PasswordError};
pub use token::SetupCode;

use password::PasswordHashConfig;
use throttle::LoginThrottle;
use token::ConnectorToken;

/// Name of the session cookie. `__Host-`: the browser only accepts it with
/// `Secure`, `Path=/` and no `Domain`, so no sibling host can set it.
pub const SESSION_COOKIE: &str = "__Host-vocab_session";

/// A session older than this since its last renewal is renewed when used.
const RENEW_AFTER: SignedDuration = SignedDuration::from_hours(7 * 24);
/// Random bytes of a session token.
const TOKEN_BYTES: usize = 32;
/// Longest stored `User-Agent` (it only helps the owner recognise sessions).
const MAX_USER_AGENT_CHARS: usize = 256;
/// The session cache is emptied when it grows past this (sessions only come
/// from successful logins, so this is never reached in practice).
const MAX_CACHED_SESSIONS: usize = 1024;

/// Why a login, setup, password change or token creation did not succeed.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("wrong password")]
    WrongPassword,
    #[error("wrong setup code")]
    WrongSetupCode,
    #[error("too many failed attempts; locked for {retry_after_secs} s")]
    Locked { retry_after_secs: u64 },
    #[error("no password is set yet; the app needs to be set up")]
    SetupRequired,
    #[error("the app is already set up")]
    AlreadySetUp,
    #[error("{0}")]
    NewPassword(PasswordError),
    #[error("authentication failed: {0}")]
    Internal(String),
}

impl From<DbError> for AuthError {
    fn from(err: DbError) -> Self {
        Self::Internal(err.to_string())
    }
}

fn internal(err: impl std::fmt::Display) -> AuthError {
    AuthError::Internal(err.to_string())
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

/// The owner row, as loaded and validated.
#[derive(Debug, Clone, Default)]
struct Owner {
    password: Option<PasswordHashConfig>,
    connector: Option<ConnectorToken>,
    connector_created_at: Option<String>,
}

impl Owner {
    fn from_row(row: owner::Row) -> rusqlite::Result<Self> {
        let invalid = |err: Box<dyn std::error::Error + Send + Sync>| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, err)
        };
        let password = row
            .password_phc
            .map(|phc| PasswordHashConfig::from_phc(&phc).map_err(|err| invalid(Box::new(err))))
            .transpose()?;
        let connector = row
            .connector_token_sha256
            .map(|hex| {
                ConnectorToken::from_digest_hex(&hex)
                    .ok_or_else(|| invalid("stored connector token digest is not hex".into()))
            })
            .transpose()?;
        Ok(Self {
            password,
            connector,
            connector_created_at: row.connector_token_created_at,
        })
    }
}

/// Authentication state shared by all requests.
pub struct Auth {
    public_origin: String,
    session_lifetime: SignedDuration,
    /// Password checks are memory heavy, and changes of the stored
    /// credentials must not interleave: one at a time.
    verify_slot: Semaphore,
    throttle: Mutex<LoginThrottle>,
    /// Digest of this start's setup code; `None` once a password is set.
    setup_code: Mutex<Option<[u8; 32]>>,
    /// The owner row, loaded on first use.
    owner: OnceCell<Mutex<Owner>>,
    /// Live sessions seen by this process: token digest → expiry. Every
    /// change of a session happens with this lock held, so the cache and
    /// the table never disagree about a session this process knows.
    sessions: tokio::sync::Mutex<HashMap<String, Timestamp>>,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("public_origin", &self.public_origin)
            .finish_non_exhaustive()
    }
}

impl Auth {
    /// The auth state of one start, and this start's one-time setup code.
    /// The code only works while no password is set; the caller logs it
    /// when [`Auth::setup_required`] says so, and drops it otherwise.
    pub fn new(public_origin: &str, session_days: u32) -> std::io::Result<(Self, SetupCode)> {
        let (code, digest) = SetupCode::generate().map_err(std::io::Error::other)?;
        let auth = Self {
            public_origin: public_origin.to_owned(),
            session_lifetime: SignedDuration::from_hours(i64::from(session_days) * 24),
            verify_slot: Semaphore::new(1),
            throttle: Mutex::new(LoginThrottle::default()),
            setup_code: Mutex::new(Some(digest)),
            owner: OnceCell::new(),
            sessions: tokio::sync::Mutex::new(HashMap::new()),
        };
        Ok((auth, code))
    }

    /// Is the app in setup state (no password yet)?
    pub async fn setup_required(&self, db: &Db) -> Result<bool, DbError> {
        let required = self.owner(db).await?.password.is_none();
        if !required {
            *lock(&self.setup_code) = None;
        }
        Ok(required)
    }

    /// First run: check the setup code, store `password` and log the owner
    /// in; returns the session's `Set-Cookie` value. A wrong code counts as
    /// a failed login. Refused for good once a password exists.
    pub async fn setup(
        &self,
        db: &Db,
        clock: &Arc<dyn Clock>,
        code: String,
        password: String,
        user_agent: Option<&str>,
    ) -> Result<String, AuthError> {
        let _slot = self.slot().await?;
        let now = clock.now();
        if self.owner(db).await?.password.is_some() {
            return Err(AuthError::AlreadySetUp);
        }
        password::check_new(&password).map_err(AuthError::NewPassword)?;
        let digest = *lock(&self.setup_code);
        self.throttled(
            now,
            move || digest.is_some_and(|d| token::setup_code_matches(&code, &d)),
            AuthError::WrongSetupCode,
        )
        .await?;
        let config = hash_new(password).await?;
        let phc = config.phc().to_owned();
        let fingerprint = config.fingerprint().to_owned();
        let now_text = format_timestamp(now);
        let mut sessions = self.sessions.lock().await;
        let stored = db
            .write(move |tx| {
                if !owner::set_first_password(tx, &phc, &now_text)? {
                    return Ok(false);
                }
                session::delete_all(tx)?;
                Ok::<_, DbError>(true)
            })
            .await?;
        if !stored {
            return Err(AuthError::AlreadySetUp);
        }
        sessions.clear();
        *lock(&self.setup_code) = None;
        self.update_owner(|owner| owner.password = Some(config));
        drop(sessions);
        tracing::info!("setup complete: the password is set");
        self.new_session(db, now, fingerprint, user_agent).await
    }

    /// Check `password`; on success create a session and return its
    /// `Set-Cookie` value.
    pub async fn login(
        &self,
        db: &Db,
        clock: &Arc<dyn Clock>,
        password: String,
        user_agent: Option<&str>,
    ) -> Result<String, AuthError> {
        let _slot = self.slot().await?;
        let now = clock.now();
        let Some(config) = self.owner(db).await?.password else {
            return Err(AuthError::SetupRequired);
        };
        let fingerprint = config.fingerprint().to_owned();
        self.throttled(
            now,
            move || config.verify(&password),
            AuthError::WrongPassword,
        )
        .await?;
        let cookie = self.new_session(db, now, fingerprint, user_agent).await?;
        tracing::info!("login");
        Ok(cookie)
    }

    /// Replace the password after checking the current one (a wrong one
    /// counts as a failed login). Every other session ends; the request's
    /// own session stays logged in.
    pub async fn change_password(
        &self,
        db: &Db,
        clock: &Arc<dyn Clock>,
        headers: &HeaderMap,
        current: String,
        new: String,
    ) -> Result<(), AuthError> {
        let _slot = self.slot().await?;
        let now = clock.now();
        let Some(old) = self.owner(db).await?.password else {
            return Err(AuthError::SetupRequired);
        };
        password::check_new(&new).map_err(AuthError::NewPassword)?;
        let old_phc = old.phc().to_owned();
        self.throttled(now, move || old.verify(&current), AuthError::WrongPassword)
            .await?;
        let config = hash_new(new).await?;
        let phc = config.phc().to_owned();
        let fingerprint = config.fingerprint().to_owned();
        let keep = session_token(headers)
            .map(|raw| token_digest(&raw))
            .unwrap_or_default();
        let now_text = format_timestamp(now);
        let mut sessions = self.sessions.lock().await;
        let kept = keep.clone();
        let replaced = db
            .write(move |tx| {
                if !owner::replace_password(tx, &old_phc, &phc, &now_text)? {
                    return Ok(false);
                }
                session::keep_only(tx, &kept, &fingerprint)?;
                Ok::<_, DbError>(true)
            })
            .await?;
        if !replaced {
            return Err(internal("the stored password changed meanwhile"));
        }
        sessions.retain(|digest, _| *digest == keep);
        self.update_owner(|owner| owner.password = Some(config));
        tracing::info!("password changed; every other session ended");
        Ok(())
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
                let Some(password) = self.owner(db).await?.password else {
                    return Ok(SessionCheck::Invalid);
                };
                let fingerprint = password.fingerprint().to_owned();
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

    /// Does the request carry the connector token (`Authorization: Bearer …`)?
    /// Always `false` while the owner has not created one.
    pub async fn has_connector_token(&self, db: &Db, headers: &HeaderMap) -> Result<bool, DbError> {
        let mut values = headers.get_all(header::AUTHORIZATION).iter();
        let (Some(value), None) = (values.next(), values.next()) else {
            return Ok(false);
        };
        let Some((scheme, presented)) = value.to_str().ok().and_then(|v| v.split_once(' ')) else {
            return Ok(false);
        };
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Ok(false);
        }
        Ok(self
            .owner(db)
            .await?
            .connector
            .is_some_and(|token| token.matches(presented.trim())))
    }

    /// When the current connector token was created; `None`: there is none.
    pub async fn connector_token_created_at(&self, db: &Db) -> Result<Option<String>, DbError> {
        Ok(self.owner(db).await?.connector_created_at)
    }

    /// Make a new connector token, replacing any old one; returns it (the
    /// only time it is ever shown). Needs the current password (a wrong one
    /// counts as a failed login), so a stolen session cookie alone cannot
    /// mint a long-lived credential.
    pub async fn create_connector_token(
        &self,
        db: &Db,
        clock: &Arc<dyn Clock>,
        current_password: String,
    ) -> Result<String, AuthError> {
        let _slot = self.slot().await?;
        let now = clock.now();
        let Some(password) = self.owner(db).await?.password else {
            return Err(AuthError::SetupRequired);
        };
        self.throttled(
            now,
            move || password.verify(&current_password),
            AuthError::WrongPassword,
        )
        .await?;
        let (token, stored) = ConnectorToken::generate().map_err(internal)?;
        let digest = stored.digest_hex();
        let now_text = format_timestamp(now);
        let created_at = now_text.clone();
        db.write(move |tx| Ok::<_, DbError>(owner::set_connector_token(tx, &digest, &now_text)?))
            .await?;
        self.update_owner(|owner| {
            owner.connector = Some(stored);
            owner.connector_created_at = Some(created_at);
        });
        tracing::info!("new connector token created");
        Ok(token)
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

    /// A copy of the owner row (loaded from the database on first use).
    async fn owner(&self, db: &Db) -> Result<Owner, DbError> {
        let cell = self
            .owner
            .get_or_try_init(|| async {
                let owner = db
                    .read(|conn| Ok::<_, DbError>(owner::load(conn).and_then(Owner::from_row)?))
                    .await?;
                Ok::<_, DbError>(Mutex::new(owner))
            })
            .await?;
        Ok(lock(cell).clone())
    }

    /// Change the in-memory owner row after the database was changed (the
    /// row is always loaded by then).
    fn update_owner(&self, f: impl FnOnce(&mut Owner)) {
        if let Some(cell) = self.owner.get() {
            f(&mut lock(cell));
        }
    }

    async fn slot(&self) -> Result<SemaphorePermit<'_>, AuthError> {
        self.verify_slot.acquire().await.map_err(internal)
    }

    /// Run `check` off the async runtime behind the login throttle. A failed
    /// check counts as a failed login and answers `wrong` (or `Locked`).
    async fn throttled(
        &self,
        now: Timestamp,
        check: impl FnOnce() -> bool + Send + 'static,
        wrong: AuthError,
    ) -> Result<(), AuthError> {
        if let Err(retry_after) = self.with_throttle(|t| t.check(now)) {
            return Err(locked(retry_after));
        }
        let passed = tokio::task::spawn_blocking(check).await.map_err(internal)?;
        if !passed {
            return Err(match self.with_throttle(|t| t.record_failure(now)) {
                Some(lockout) => {
                    tracing::warn!(minutes = lockout.as_mins(), "login locked after failures");
                    locked(lockout)
                }
                None => wrong,
            });
        }
        self.with_throttle(LoginThrottle::record_success);
        Ok(())
    }

    /// Create a session bound to `fingerprint`; returns its `Set-Cookie`.
    async fn new_session(
        &self,
        db: &Db,
        now: Timestamp,
        fingerprint: String,
        user_agent: Option<&str>,
    ) -> Result<String, AuthError> {
        let mut raw = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut raw).map_err(internal)?;
        let token = hex::encode(raw);
        let token_sha256 = token_digest(&raw);
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
        Ok(self.session_cookie(&token))
    }

    fn with_throttle<T>(&self, f: impl FnOnce(&mut LoginThrottle) -> T) -> T {
        f(&mut lock(&self.throttle))
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

/// Back to setup state: clear the stored password, end every session and
/// revoke the connector token (a reset may follow a compromise). Only for a
/// stopped app: `reset-password` holds the data-directory lock, so it cannot
/// run next to a server and its in-memory state.
pub async fn reset_password(db: &Db) -> Result<(), DbError> {
    db.write(|tx| {
        owner::clear_credentials(tx)?;
        session::delete_all(tx)?;
        Ok::<_, DbError>(())
    })
    .await
}

/// Hash a new password off the async runtime.
async fn hash_new(password: String) -> Result<PasswordHashConfig, AuthError> {
    tokio::task::spawn_blocking(move || password::hash_password(&password))
        .await
        .map_err(internal)?
        .map_err(|err| match err {
            PasswordError::Length => AuthError::NewPassword(err),
            other => internal(other),
        })
}

/// Lock a std mutex; a poisoned one is still usable (its data is plain).
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn locked(retry_after: SignedDuration) -> AuthError {
    AuthError::Locked {
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
    fn r82_auth_debug_shows_no_secrets() {
        let (auth, code) = Auth::new("https://vocab.example.home", 180).expect("auth");
        let debug = format!("{auth:?} {code:?}");
        assert!(!debug.contains(&code.to_string()), "{debug}");
        assert!(
            !debug.contains(&code.to_string().replace('-', "")),
            "{debug}"
        );
    }
}
