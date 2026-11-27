//! The Undo of the owner's latest change to a word (design §7.1 screen 7,
//! §8 `POST /api/words/{id}/undo`).
//!
//! A change stores the word's state before and after it (opaque texts from
//! `workflows`) under a random token, which the answer carries. The Undo
//! names the token and is applied at most once, within [`VALID_FOR`], and
//! only while the word is still exactly as that change left it (the caller
//! compares). A newer change to the word replaces the stored one, so an
//! Undo from another tab, a second Undo, or an Undo after a later edit is
//! refused instead of guessing.
//!
//! Owned table: `word_undo` (one row per word; expired rows are dropped
//! whenever a change is stored, and the row goes with its word).

use jiff::SignedDuration;
use rusqlite::{OptionalExtension, Transaction, params};

use crate::catalog::WordId;
use crate::clock::{Moment, format_timestamp};

/// How long after a change its Undo is accepted. The page offers it for a
/// few seconds; the margin covers a slow network and a background tab.
pub const VALID_FOR: SignedDuration = SignedDuration::from_secs(10 * 60);

/// Why an Undo was not applied (the message is for the owner).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UndoError {
    /// Unknown token: already undone, replaced by a newer change, or never
    /// given for this word.
    #[error(
        "This change can no longer be undone: it was already undone, or the word was changed again since."
    )]
    Gone,
    #[error("This change can no longer be undone: it was made too long ago.")]
    Expired,
}

/// A token naming one stored Undo: 128 random bits as 32 hex digits.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct UndoToken(String);

impl UndoToken {
    /// Accept only the canonical form (anything else cannot exist).
    pub fn parse(text: &str) -> Option<Self> {
        (text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| Self(text.to_owned()))
    }

    fn random() -> rusqlite::Result<Self> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(format!("random source failed: {e}").into())
        })?;
        Ok(Self(hex::encode(bytes)))
    }
}

/// What a change left: the word's state before and after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub before: String,
    pub after: String,
}

/// Store the Undo of a change to `word` (replacing an earlier one) and
/// return its token.
pub fn record(
    tx: &Transaction<'_>,
    word: WordId,
    before: &str,
    after: &str,
    now: &Moment,
) -> rusqlite::Result<UndoToken> {
    tx.execute(
        "DELETE FROM word_undo WHERE expires_at <= ?1",
        [now.stamp()],
    )?;
    let token = UndoToken::random()?;
    let expires = now.at.checked_add(VALID_FOR).unwrap_or(now.at);
    tx.execute(
        "INSERT INTO word_undo (word_id, token, expires_at, before_json, after_json) \
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (word_id) DO UPDATE SET token = excluded.token, \
         expires_at = excluded.expires_at, before_json = excluded.before_json, \
         after_json = excluded.after_json",
        params![
            word.get(),
            token.0,
            format_timestamp(expires),
            before,
            after
        ],
    )?;
    Ok(token)
}

/// Take the Undo `token` of `word` (it cannot be used again).
pub fn take(
    tx: &Transaction<'_>,
    word: WordId,
    token: &UndoToken,
    now: &Moment,
) -> rusqlite::Result<Result<Stored, UndoError>> {
    let row: Option<(String, String, String)> = tx
        .query_row(
            "SELECT expires_at, before_json, after_json FROM word_undo \
             WHERE word_id = ?1 AND token = ?2",
            params![word.get(), token.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((expires, before, after)) = row else {
        return Ok(Err(UndoError::Gone));
    };
    tx.execute("DELETE FROM word_undo WHERE word_id = ?1", [word.get()])?;
    // Both are stored in the same fixed format: text order is time order.
    if expires <= now.stamp() {
        return Ok(Err(UndoError::Expired));
    }
    Ok(Ok(Stored { before, after }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r04_tokens_are_canonical_hex() {
        let token = UndoToken::random().expect("random");
        assert_eq!(UndoToken::parse(&token.0), Some(token.clone()));
        assert_eq!(UndoToken::parse("ABC"), None);
        assert_eq!(UndoToken::parse(&token.0.to_uppercase()), None);
        assert_ne!(UndoToken::random().expect("random"), token);
    }
}
