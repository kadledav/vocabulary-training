//! Which word a headword key belongs to (design §5.3, §5.4): the word with
//! that key, or the word a pack once spelled that way (`word_pack_spelling`,
//! R-04: the owner may have changed its headword since). Shared by the
//! import merge and the owner's headword edit, so both see the same
//! identity.
//!
//! Only packs add spellings ([`record_pack_spelling`]); the owner's own
//! spellings never do, so a spelling the owner typed and changed again
//! leaves nothing behind. Invariant: a key belongs to at most one word,
//! either as its current `headword_key` or as one of its pack spellings.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::WordId;

/// The word `key` (a `headword_key`) belongs to, if any.
pub fn find_word(conn: &Connection, key: &str) -> rusqlite::Result<Option<WordId>> {
    conn.query_row(
        "SELECT id FROM word WHERE headword_key = ?1 \
         UNION ALL SELECT word_id FROM word_pack_spelling WHERE key = ?1 LIMIT 1",
        [key],
        |row| row.get(0).map(WordId::new),
    )
    .optional()
}

/// Who holds a key, as the owner is told when he wants it for another word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyHolder {
    /// The word's current headword.
    Headword { word: WordId, headword: String },
    /// A pack's spelling of a word now shown as `headword`.
    PackSpelling {
        word: WordId,
        spelling: String,
        headword: String,
    },
}

impl KeyHolder {
    pub fn word(&self) -> WordId {
        match self {
            Self::Headword { word, .. } | Self::PackSpelling { word, .. } => *word,
        }
    }
}

/// Who holds `key` (see [`find_word`]), with the texts to name it.
pub fn holder(conn: &Connection, key: &str) -> rusqlite::Result<Option<KeyHolder>> {
    let own = conn
        .query_row(
            "SELECT id, headword FROM word WHERE headword_key = ?1",
            [key],
            |row| {
                Ok(KeyHolder::Headword {
                    word: WordId::new(row.get(0)?),
                    headword: row.get(1)?,
                })
            },
        )
        .optional()?;
    if own.is_some() {
        return Ok(own);
    }
    conn.query_row(
        "SELECT w.id, p.headword, w.headword FROM word_pack_spelling p \
         JOIN word w ON w.id = p.word_id WHERE p.key = ?1",
        [key],
        |row| {
            Ok(KeyHolder::PackSpelling {
                word: WordId::new(row.get(0)?),
                spelling: row.get(1)?,
                headword: row.get(2)?,
            })
        },
    )
    .optional()
}

/// Remember that a pack spelled `word` as `headword` (key `key`). The first
/// spelling of a key is kept.
pub fn record_pack_spelling(
    conn: &Connection,
    word: WordId,
    key: &str,
    headword: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO word_pack_spelling (key, word_id, headword) VALUES (?1, ?2, ?3) \
         ON CONFLICT (key) DO NOTHING",
        params![key, word.get(), headword],
    )?;
    Ok(())
}

/// A pack spelling of a word other than its current headword: the owner
/// sees it, and may remove it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackSpelling {
    pub headword: String,
    /// Names it in `DELETE /api/words/{id}/spellings`.
    pub key: String,
}

/// The pack spellings of `word` the word still answers to besides its
/// current headword, in the order they arrived.
pub fn pack_spellings(conn: &Connection, word: WordId) -> rusqlite::Result<Vec<PackSpelling>> {
    let mut stmt = conn.prepare(
        "SELECT p.headword, p.key FROM word_pack_spelling p JOIN word w ON w.id = p.word_id \
         WHERE p.word_id = ?1 AND p.key != w.headword_key ORDER BY p.rowid",
    )?;
    stmt.query_map([word.get()], |row| {
        Ok(PackSpelling {
            headword: row.get(0)?,
            key: row.get(1)?,
        })
    })?
    .collect()
}

/// Forget a pack spelling of `word` (not its current headword): a later
/// pack with it adds a new word. Returns whether one was removed.
pub fn remove_pack_spelling(conn: &Connection, word: WordId, key: &str) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "DELETE FROM word_pack_spelling WHERE word_id = ?1 AND key = ?2 \
         AND key != (SELECT headword_key FROM word WHERE id = ?1)",
        params![word.get(), key],
    )?;
    Ok(n > 0)
}
