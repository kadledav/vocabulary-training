//! The `auth_session` table (design §4.2, §9.1). Only this module touches it.
//!
//! A row holds the SHA-256 of the session token (never the token itself),
//! the fingerprint of the password hash that was current at login, when
//! the session was created and last renewed, its expiry, and how many
//! wrong passwords were typed inside it (migration 0005). Timestamps are
//! `format_timestamp` text, so they compare correctly as strings.

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

/// The times of a stored session.
pub struct Times {
    pub created_at: String,
    /// When it was created or last renewed.
    pub renewed_at: String,
    pub expires_at: String,
}

/// A session of the current password that has not expired yet (the caller
/// also applies the absolute lifetime, from `created_at`).
pub fn live(
    conn: &Connection,
    token_sha256: &str,
    fingerprint: &str,
    now: &str,
) -> rusqlite::Result<Option<Times>> {
    conn.query_row(
        "SELECT created_at, last_seen_at, expires_at FROM auth_session \
         WHERE token_sha256 = ?1 AND password_fingerprint = ?2 AND expires_at > ?3",
        params![token_sha256, fingerprint, now],
        |row| {
            Ok(Times {
                created_at: row.get(0)?,
                renewed_at: row.get(1)?,
                expires_at: row.get(2)?,
            })
        },
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

/// End every session (setup, password change, logout everywhere,
/// `reset-password`); returns how many there were.
pub fn delete_all(tx: &Transaction<'_>) -> rusqlite::Result<usize> {
    tx.execute("DELETE FROM auth_session", [])
}

/// Count a wrong password typed inside the session; returns the new count
/// (0 when the session does not exist).
pub fn record_password_failure(tx: &Transaction<'_>, token_sha256: &str) -> rusqlite::Result<u32> {
    tx.execute(
        "UPDATE auth_session SET password_failures = password_failures + 1 WHERE token_sha256 = ?1",
        params![token_sha256],
    )?;
    Ok(tx
        .query_row(
            "SELECT password_failures FROM auth_session WHERE token_sha256 = ?1",
            params![token_sha256],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// The session typed the right password: its count starts over.
pub fn clear_password_failures(tx: &Transaction<'_>, token_sha256: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE auth_session SET password_failures = 0 \
         WHERE token_sha256 = ?1 AND password_failures <> 0",
        params![token_sha256],
    )?;
    Ok(())
}
