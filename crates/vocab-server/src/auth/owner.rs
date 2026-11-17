//! The `auth_owner` table (migration 0004). Only this module touches it.
//!
//! One row (`id = 1`, made by the migration): the password's PHC string
//! (NULL until setup) and the SHA-256 of the connector token (NULL when
//! there is none).

use rusqlite::{Connection, Transaction, params};

/// The stored row.
pub struct Row {
    pub password_phc: Option<String>,
    pub connector_token_sha256: Option<String>,
    pub connector_token_created_at: Option<String>,
}

pub fn load(conn: &Connection) -> rusqlite::Result<Row> {
    conn.query_row(
        "SELECT password_phc, connector_token_sha256, connector_token_created_at \
         FROM auth_owner WHERE id = 1",
        [],
        |row| {
            Ok(Row {
                password_phc: row.get(0)?,
                connector_token_sha256: row.get(1)?,
                connector_token_created_at: row.get(2)?,
            })
        },
    )
}

/// Store the first password. `false` when a password is already set (a
/// concurrent setup won): nothing is changed then.
pub fn set_first_password(tx: &Transaction<'_>, phc: &str, now: &str) -> rusqlite::Result<bool> {
    let changed = tx.execute(
        "UPDATE auth_owner SET password_phc = ?1, password_set_at = ?2 \
         WHERE id = 1 AND password_phc IS NULL",
        params![phc, now],
    )?;
    Ok(changed == 1)
}

/// Replace the password. `false` when the stored one is not `current_phc`
/// any more: nothing is changed then.
pub fn replace_password(
    tx: &Transaction<'_>,
    current_phc: &str,
    phc: &str,
    now: &str,
) -> rusqlite::Result<bool> {
    let changed = tx.execute(
        "UPDATE auth_owner SET password_phc = ?2, password_set_at = ?3 \
         WHERE id = 1 AND password_phc = ?1",
        params![current_phc, phc, now],
    )?;
    Ok(changed == 1)
}

/// Back to setup state without a connector token (`reset-password`).
pub fn clear_credentials(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE auth_owner SET password_phc = NULL, password_set_at = NULL, \
         connector_token_sha256 = NULL, connector_token_created_at = NULL WHERE id = 1",
        [],
    )?;
    Ok(())
}

pub fn set_connector_token(tx: &Transaction<'_>, sha256: &str, now: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE auth_owner SET connector_token_sha256 = ?1, connector_token_created_at = ?2 \
         WHERE id = 1",
        params![sha256, now],
    )?;
    Ok(())
}
