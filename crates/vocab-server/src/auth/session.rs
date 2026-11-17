//! The `auth_session` table (design §4.2, §9.1). Only this module touches it.
//!
//! A row holds the SHA-256 of the session token (never the token itself),
//! the fingerprint of the password hash that was configured at login, and
//! the expiry. Timestamps are `format_timestamp` text, so they compare
//! correctly as strings.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

/// A new session row.
pub struct NewSession<'a> {
    pub token_sha256: &'a str,
    pub fingerprint: &'a str,
    pub now: &'a str,
    pub expires_at: &'a str,
    pub user_agent: Option<&'a str>,
}

pub fn insert(tx: &Transaction<'_>, session: &NewSession<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO auth_session \
         (token_sha256, password_fingerprint, created_at, last_seen_at, expires_at, user_agent) \
         VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
        params![
            session.token_sha256,
            session.fingerprint,
            session.now,
            session.expires_at,
            session.user_agent
        ],
    )?;
    Ok(())
}

/// Expiry of a live session: same password, not yet expired.
pub fn live_expiry(
    conn: &Connection,
    token_sha256: &str,
    fingerprint: &str,
    now: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT expires_at FROM auth_session \
         WHERE token_sha256 = ?1 AND password_fingerprint = ?2 AND expires_at > ?3",
        params![token_sha256, fingerprint, now],
        |row| row.get(0),
    )
    .optional()
}

/// Sliding renewal: push the expiry out and note when it happened.
pub fn renew(
    tx: &Transaction<'_>,
    token_sha256: &str,
    now: &str,
    expires_at: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE auth_session SET last_seen_at = ?2, expires_at = ?3 WHERE token_sha256 = ?1",
        params![token_sha256, now, expires_at],
    )?;
    Ok(())
}

pub fn delete(tx: &Transaction<'_>, token_sha256: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM auth_session WHERE token_sha256 = ?1",
        params![token_sha256],
    )?;
    Ok(())
}

/// Remove sessions that can never be used again: expired ones and those of
/// an earlier password.
pub fn purge(tx: &Transaction<'_>, fingerprint: &str, now: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM auth_session WHERE expires_at <= ?2 OR password_fingerprint <> ?1",
        params![fingerprint, now],
    )?;
    Ok(())
}
