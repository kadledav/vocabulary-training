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
//!   ends every session of the old one. A password change ends **every**
//!   session and gives the device that made it a new token (a copy of the
//!   old cookie is worthless afterwards). Sessions last 180 days and slide
//!   (a session used more than 7 days after its last renewal is renewed),
//!   but never live longer than 365 days from their creation.
//!   [`Auth::logout_all`] ends them all.
//! * Three kinds of secret checks, kept apart on purpose:
//!   - **login**: behind the global login throttle ([`throttle`]), whose
//!     state is stored so a restart does not reset it;
//!   - **password typed inside a session** (change password, create a
//!     connector token): counted per session, five wrong ones end that
//!     session; never touches and is never blocked by the login throttle,
//!     so nobody without a session can keep the owner from managing his
//!     account;
//!   - **setup code**: not throttled at all (50 random bits); every wrong
//!     code only costs a fixed delay while the single check slot is held.
//!
//!   All of them run one at a time; password hashing runs off the async
//!   runtime, and everything that changes the stored credentials runs in
//!   that same single slot.
//! * The connector token is a separate bearer secret the owner creates in
//!   the app; only its SHA-256 is stored. Which routes accept it is decided
//!   by `http`.
//! * CSRF: every state-changing request must come from `public_origin` and
//!   carry a JSON body type (§9.3); a request the browser itself labels as
//!   cross-site or same-site (`Sec-Fetch-Site`) is refused outright.
//!
//! The owner row and live sessions are also kept in memory, so a request
//! needs the database only for the first check after a start and for
//! renewals; the tables stay the source of truth across restarts.
//!
//! The module owns the `auth_owner`, `auth_session` and `auth_throttle`
//! tables, the session cache, the setup code and the throttle state. `http`
//! decides which routes need what and maps the outcomes to responses.

mod owner;
mod password;
mod session;
mod throttle;
mod token;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

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
/// Wrong passwords typed inside one session before that session is ended.
pub const MAX_SESSION_PASSWORD_FAILURES: u32 = 5;
/// What a wrong setup code costs: the single check slot stays taken this
/// long, so at most four codes a second can be tried against 2^50.
const WRONG_SETUP_CODE_DELAY: Duration = Duration::from_millis(250);
/// The browser's own statement of where a request comes from.
const SEC_FETCH_SITE: &str = "sec-fetch-site";

/// How long sessions live (design §9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLifetime {
    /// Sliding window: a session not used for this long expires.
    pub sliding_days: u32,
    /// Absolute limit from the creation of the session.
    pub max_days: u32,
}

/// Why a login, setup, password change or token creation did not succeed.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("wrong password")]
    WrongPassword,
    #[error("wrong setup code")]
    WrongSetupCode,
    #[error("too many failed attempts; locked for {retry_after_secs} s")]
    Locked { retry_after_secs: u64 },
    /// Too many wrong passwords inside this session: it was logged out.
    #[error("too many wrong passwords in this session; it was ended")]
    SessionEnded,
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

/// A live session as the cache knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Live {
    created: Timestamp,
    /// Created or last renewed.
    renewed: Timestamp,
    expires: Timestamp,
}

/// A freshly made session token.
struct Issued {
    /// The cookie value (hex of the random bytes).
    token: String,
    /// What the database keeps.
    digest: String,
    live: Live,
}

/// Authentication state shared by all requests.
pub struct Auth {
    public_origin: String,
    session_lifetime: SignedDuration,
    session_max_lifetime: SignedDuration,
    /// Password checks are memory heavy, and changes of the stored
    /// credentials must not interleave: one at a time.
    verify_slot: Semaphore,
    /// The login throttle, loaded from its table on first use.
    throttle: OnceCell<Mutex<LoginThrottle>>,
    /// Digest of this start's setup code; `None` once a password is set.
    setup_code: Mutex<Option<[u8; 32]>>,
    /// The owner row, loaded on first use.
    owner: OnceCell<Mutex<Owner>>,
    /// Live sessions seen by this process, by token digest. Every change of
    /// a session happens with this lock held, so the cache and the table
    /// never disagree about a session this process knows.
    sessions: tokio::sync::Mutex<HashMap<String, Live>>,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("public_origin", &self.public_origin)
            .finish_non_exhaustive()
    }
}

fn days(n: u32) -> SignedDuration {
    SignedDuration::from_hours(i64::from(n) * 24)
}

impl Auth {
    /// The auth state of one start, and this start's one-time setup code.
    /// The code only works while no password is set; the caller logs it
    /// when [`Auth::setup_required`] says so.
    pub fn new(
        public_origin: &str,
        lifetime: SessionLifetime,
    ) -> std::io::Result<(Self, SetupCode)> {
        let (code, digest) = SetupCode::generate().map_err(std::io::Error::other)?;
        let auth = Self {
            public_origin: public_origin.to_owned(),
            session_lifetime: days(lifetime.sliding_days),
            session_max_lifetime: days(lifetime.max_days),
            verify_slot: Semaphore::new(1),
            throttle: OnceCell::new(),
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

    /// As long as the app is in setup state, call `announce` once per
    /// `every` (the caller logs the setup code again, so it is still to be
    /// found when the log has rotated). Returns once a password is set.
    pub async fn remind_setup(&self, db: &Db, every: Duration, mut announce: impl FnMut()) {
        loop {
            tokio::time::sleep(every).await;
            match self.setup_required(db).await {
                Ok(true) => announce(),
                Ok(false) => return,
                // Try again next time; the request path reports such errors.
                Err(_) => {}
            }
        }
    }

    /// First run: check the setup code, store `password` and log the owner
    /// in; returns the session's `Set-Cookie` value. Wrong codes are not
    /// counted against anything (the code has 50 random bits); each one
    /// just holds the single check slot for a fixed delay. Refused for good
    /// once a password exists.
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
        if !digest.is_some_and(|d| token::setup_code_matches(&code, &d)) {
            tracing::warn!("setup refused: wrong setup code");
            tokio::time::sleep(WRONG_SETUP_CODE_DELAY).await;
            return Err(AuthError::WrongSetupCode);
        }
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

    /// Check `password` behind the login throttle; on success create a
    /// session and return its `Set-Cookie` value.
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
        let throttle = self.throttle(db).await?;
        let refused = {
            let mut throttle = lock(throttle);
            throttle
                .check(now)
                .err()
                .map(|(retry_after, store)| (retry_after, store.then(|| throttle.clone())))
        };
        if let Some((retry_after, state)) = refused {
            // The refused attempt keeps the decay going after a restart.
            if let Some(state) = state {
                db.write(move |tx| Ok::<_, DbError>(throttle::store(tx, &state)?))
                    .await?;
            }
            return Err(locked(retry_after));
        }
        let passed = tokio::task::spawn_blocking(move || config.verify(&password))
            .await
            .map_err(internal)?;
        if !passed {
            let (lockout, state) = {
                let mut throttle = lock(throttle);
                (throttle.record_failure(now), throttle.clone())
            };
            // Stored before answering: a restart must not forget it.
            db.write(move |tx| Ok::<_, DbError>(throttle::store(tx, &state)?))
                .await?;
            tracing::warn!("failed login: wrong password");
            return Err(match lockout {
                Some(lockout) => {
                    tracing::warn!(
                        minutes = lockout.as_mins(),
                        "logins locked after repeated failed logins"
                    );
                    locked(lockout)
                }
                None => AuthError::WrongPassword,
            });
        }
        if lock(throttle).record_success() {
            db.write(|tx| Ok::<_, DbError>(throttle::clear(tx)?))
                .await?;
        }
        let cookie = self.new_session(db, now, fingerprint, user_agent).await?;
        tracing::info!("login");
        Ok(cookie)
    }

    /// Replace the password after checking the current one (counted against
    /// the request's session, see [`MAX_SESSION_PASSWORD_FAILURES`]). Every
    /// session ends, the request's own included; the device gets a new one,
    /// whose `Set-Cookie` value is returned.
    pub async fn change_password(
        &self,
        db: &Db,
        clock: &Arc<dyn Clock>,
        headers: &HeaderMap,
        current: String,
        new: String,
    ) -> Result<String, AuthError> {
        let _slot = self.slot().await?;
        let now = clock.now();
        let Some(old) = self.owner(db).await?.password else {
            return Err(AuthError::SetupRequired);
        };
        password::check_new(&new).map_err(AuthError::NewPassword)?;
        let old_phc = old.phc().to_owned();
        self.confirm_password(db, headers, move || old.verify(&current))
            .await?;
        let config = hash_new(new).await?;
        let phc = config.phc().to_owned();
        let fingerprint = config.fingerprint().to_owned();
        let issued = self.issue(now)?;
        let user_agent = user_agent_of(headers);
        let now_text = format_timestamp(now);
        let (digest, expires) = (issued.digest.clone(), format_timestamp(issued.live.expires));
        let mut sessions = self.sessions.lock().await;
        let replaced = db
            .write(move |tx| {
                if !owner::replace_password(tx, &old_phc, &phc, &now_text)? {
                    return Ok(false);
                }
                session::delete_all(tx)?;
                session::insert(
                    tx,
                    &session::NewSession {
                        token_sha256: &digest,
                        fingerprint: &fingerprint,
                        now: &now_text,
                        expires_at: &expires,
                        user_agent: user_agent.as_deref(),
                    },
                )?;
                Ok::<_, DbError>(true)
            })
            .await?;
        if !replaced {
            return Err(internal("the stored password changed meanwhile"));
        }
        sessions.clear();
        cache(&mut sessions, issued.digest.clone(), issued.live);
        self.update_owner(|owner| owner.password = Some(config));
        tracing::info!("password changed; every session ended, this device got a new one");
        Ok(self.session_cookie(&issued.token, issued.live.expires, now))
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
        let live = match sessions.get(&digest) {
            Some(live) => *live,
            None => {
                let Some(password) = self.owner(db).await?.password else {
                    return Ok(SessionCheck::Invalid);
                };
                let fingerprint = password.fingerprint().to_owned();
                let now_text = format_timestamp(now);
                let key = digest.clone();
                let stored = db
                    .read(move |conn| {
                        Ok::<_, DbError>(session::live(conn, &key, &fingerprint, &now_text)?)
                    })
                    .await?;
                let Some(live) = stored.and_then(|times| Live::parse(&times)) else {
                    return Ok(SessionCheck::Invalid);
                };
                cache(&mut sessions, digest.clone(), live);
                live
            }
        };
        let end = live.expires.min(self.absolute_end(live.created));
        if end <= now {
            sessions.remove(&digest);
            return Ok(SessionCheck::Invalid);
        }
        if now.duration_since(live.renewed) <= RENEW_AFTER {
            return Ok(SessionCheck::Valid { renewal: None });
        }
        let renewed = Live {
            created: live.created,
            renewed: now,
            expires: self.expiry(now, live.created),
        };
        let now_text = format_timestamp(now);
        let expires = format_timestamp(renewed.expires);
        let key = digest.clone();
        db.write(move |tx| Ok::<_, DbError>(session::renew(tx, &key, &now_text, &expires)?))
            .await?;
        sessions.insert(digest, renewed);
        Ok(SessionCheck::Valid {
            renewal: Some(self.session_cookie(&hex::encode(raw), renewed.expires, now)),
        })
    }

    /// Does the request carry the cookie of a session this process already
    /// knows to be live? Never waits and never touches the database (it is
    /// asked before a request gets a slot), so it can say `false` for a
    /// live session: one not used since the start, or while the session
    /// table is busy.
    pub fn has_known_session(&self, headers: &HeaderMap, now: Timestamp) -> bool {
        let Some(raw) = session_token(headers) else {
            return false;
        };
        let Ok(sessions) = self.sessions.try_lock() else {
            return false;
        };
        sessions
            .get(&token_digest(&raw))
            .is_some_and(|live| live.expires.min(self.absolute_end(live.created)) > now)
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
        Ok(expired_session_cookie())
    }

    /// "Log out everywhere": end every session, the request's own
    /// included; returns the `Set-Cookie` value that removes the cookie.
    pub async fn logout_all(&self, db: &Db) -> Result<String, DbError> {
        let mut sessions = self.sessions.lock().await;
        db.write(|tx| Ok::<_, DbError>(session::delete_all(tx)?))
            .await?;
        sessions.clear();
        tracing::info!("logout everywhere: every session ended");
        Ok(expired_session_cookie())
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
    /// only time it is ever shown). Needs the current password (counted
    /// against the request's session), so a stolen session cookie alone
    /// cannot mint a long-lived credential.
    pub async fn create_connector_token(
        &self,
        db: &Db,
        clock: &Arc<dyn Clock>,
        headers: &HeaderMap,
        current_password: String,
    ) -> Result<String, AuthError> {
        let _slot = self.slot().await?;
        let now = clock.now();
        let Some(password) = self.owner(db).await?.password else {
            return Err(AuthError::SetupRequired);
        };
        self.confirm_password(db, headers, move || password.verify(&current_password))
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

    /// Fetch-metadata guard for the API (§9.3): a browser that says the
    /// request was started by another site (`cross-site`) or by a sibling
    /// host (`same-site`) is refused, whatever the method. No header (the
    /// connector, scripts, old browsers) is fine; so are `same-origin` and
    /// `none` (typed into the address bar).
    pub fn check_fetch_site(headers: &HeaderMap) -> Result<(), CsrfRejection> {
        let foreign = headers.get_all(SEC_FETCH_SITE).iter().any(|value| {
            let value = value.as_bytes().trim_ascii();
            value.eq_ignore_ascii_case(b"cross-site") || value.eq_ignore_ascii_case(b"same-site")
        });
        if foreign {
            Err(CsrfRejection::Origin)
        } else {
            Ok(())
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

    /// The login throttle (its stored state is loaded on first use).
    async fn throttle(&self, db: &Db) -> Result<&Mutex<LoginThrottle>, DbError> {
        self.throttle
            .get_or_try_init(|| async {
                let state = db
                    .read(|conn| Ok::<_, DbError>(throttle::load(conn)?))
                    .await?;
                Ok::<_, DbError>(Mutex::new(state))
            })
            .await
    }

    async fn slot(&self) -> Result<SemaphorePermit<'_>, AuthError> {
        self.verify_slot.acquire().await.map_err(internal)
    }

    /// Run `verify` (the check of the current password) off the async
    /// runtime for a request made inside a session. A wrong password is
    /// counted against that session only; the
    /// [`MAX_SESSION_PASSWORD_FAILURES`]th ends it. The global login
    /// throttle is neither asked nor told.
    async fn confirm_password(
        &self,
        db: &Db,
        headers: &HeaderMap,
        verify: impl FnOnce() -> bool + Send + 'static,
    ) -> Result<(), AuthError> {
        let Some(raw) = session_token(headers) else {
            return Err(internal("a password check outside a session"));
        };
        let digest = token_digest(&raw);
        let passed = tokio::task::spawn_blocking(verify)
            .await
            .map_err(internal)?;
        let mut sessions = self.sessions.lock().await;
        let key = digest.clone();
        if passed {
            db.write(move |tx| Ok::<_, DbError>(session::clear_password_failures(tx, &key)?))
                .await?;
            return Ok(());
        }
        let ended = db
            .write(move |tx| {
                let failures = session::record_password_failure(tx, &key)?;
                // No row any more (0): the session ended meanwhile.
                let ended = failures == 0 || failures >= MAX_SESSION_PASSWORD_FAILURES;
                if ended {
                    session::delete(tx, &key)?;
                }
                Ok::<_, DbError>(ended)
            })
            .await?;
        if ended {
            sessions.remove(&digest);
            tracing::warn!("session ended after repeated wrong passwords");
            Err(AuthError::SessionEnded)
        } else {
            tracing::warn!("wrong password in a logged-in session");
            Err(AuthError::WrongPassword)
        }
    }

    /// A new random session token that starts now.
    fn issue(&self, now: Timestamp) -> Result<Issued, AuthError> {
        let mut raw = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut raw).map_err(internal)?;
        Ok(Issued {
            token: hex::encode(raw),
            digest: token_digest(&raw),
            live: Live {
                created: now,
                renewed: now,
                expires: self.expiry(now, now),
            },
        })
    }

    /// Create a session bound to `fingerprint`; returns its `Set-Cookie`.
    async fn new_session(
        &self,
        db: &Db,
        now: Timestamp,
        fingerprint: String,
        user_agent: Option<&str>,
    ) -> Result<String, AuthError> {
        let issued = self.issue(now)?;
        let now_text = format_timestamp(now);
        let expires = format_timestamp(issued.live.expires);
        let user_agent: Option<String> =
            user_agent.map(|ua| ua.chars().take(MAX_USER_AGENT_CHARS).collect());
        let mut sessions = self.sessions.lock().await;
        let digest = issued.digest.clone();
        db.write(move |tx| {
            session::purge(tx, &fingerprint, &now_text)?;
            session::insert(
                tx,
                &session::NewSession {
                    token_sha256: &digest,
                    fingerprint: &fingerprint,
                    now: &now_text,
                    expires_at: &expires,
                    user_agent: user_agent.as_deref(),
                },
            )?;
            Ok::<_, DbError>(())
        })
        .await?;
        sessions.retain(|_, live| live.expires > now);
        cache(&mut sessions, issued.digest, issued.live);
        Ok(self.session_cookie(&issued.token, issued.live.expires, now))
    }

    /// The latest moment a session created at `created` may live to.
    fn absolute_end(&self, created: Timestamp) -> Timestamp {
        created
            .saturating_add(self.session_max_lifetime)
            .unwrap_or(created)
    }

    /// Expiry of a session (re)newed at `now`: the sliding window, cut at
    /// the absolute lifetime.
    fn expiry(&self, now: Timestamp, created: Timestamp) -> Timestamp {
        now.saturating_add(self.session_lifetime)
            .unwrap_or(now)
            .min(self.absolute_end(created))
    }

    fn session_cookie(&self, token: &str, expires: Timestamp, now: Timestamp) -> String {
        format!(
            "{SESSION_COOKIE}={token}; Path=/; Max-Age={}; Secure; HttpOnly; SameSite=Strict",
            expires.duration_since(now).as_secs().max(0)
        )
    }
}

impl Live {
    fn parse(times: &session::Times) -> Option<Self> {
        Some(Self {
            created: times.created_at.parse().ok()?,
            renewed: times.renewed_at.parse().ok()?,
            expires: times.expires_at.parse().ok()?,
        })
    }
}

/// The `Set-Cookie` value that removes the session cookie from the browser.
pub fn expired_session_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict")
}

fn user_agent_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|ua| ua.chars().take(MAX_USER_AGENT_CHARS).collect())
}

/// Back to setup state: clear the stored password, end every session,
/// revoke the connector token (a reset may follow a compromise) and forget
/// every failed login. Only for a stopped app: `reset-password` holds the
/// data-directory lock, so it cannot run next to a server and its in-memory
/// state.
pub async fn reset_password(db: &Db) -> Result<(), DbError> {
    db.write(|tx| {
        owner::clear_credentials(tx)?;
        session::delete_all(tx)?;
        throttle::clear(tx)?;
        Ok::<_, DbError>(())
    })
    .await
}

/// End every session; returns how many there were. Only for a stopped app
/// (`logout-all` holds the data-directory lock); a running one uses
/// [`Auth::logout_all`].
pub async fn logout_everywhere(db: &Db) -> Result<usize, DbError> {
    db.write(|tx| Ok::<_, DbError>(session::delete_all(tx)?))
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

fn cache(sessions: &mut HashMap<String, Live>, digest: String, live: Live) {
    if sessions.len() >= MAX_CACHED_SESSIONS {
        sessions.clear();
    }
    sessions.insert(digest, live);
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
        let (auth, code) = Auth::new(
            "https://vocab.example.home",
            SessionLifetime {
                sliding_days: 180,
                max_days: 365,
            },
        )
        .expect("auth");
        let debug = format!("{auth:?} {code:?}");
        assert!(!debug.contains(&code.to_string()), "{debug}");
        assert!(
            !debug.contains(&code.to_string().replace('-', "")),
            "{debug}"
        );
    }

    /// B6: the browser's own word about where a request comes from.
    #[test]
    fn r82_requests_labelled_cross_site_or_same_site_are_refused() {
        for foreign in ["cross-site", "same-site", "Cross-Site", " same-site "] {
            assert_eq!(
                Auth::check_fetch_site(&headers(&[("sec-fetch-site", foreign)])),
                Err(CsrfRejection::Origin),
                "{foreign}"
            );
        }
        for fine in ["same-origin", "none"] {
            assert_eq!(
                Auth::check_fetch_site(&headers(&[("sec-fetch-site", fine)])),
                Ok(()),
                "{fine}"
            );
        }
        // No header: a non-browser client such as the connector.
        assert_eq!(Auth::check_fetch_site(&headers(&[])), Ok(()));
        // Two headers, one of them foreign: refused.
        assert!(
            Auth::check_fetch_site(&headers(&[
                ("sec-fetch-site", "same-origin"),
                ("sec-fetch-site", "cross-site")
            ]))
            .is_err()
        );
    }
}
