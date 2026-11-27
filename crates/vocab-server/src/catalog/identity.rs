//! Which word a headword key belongs to (design §5.3, §5.4): the word with
//! that key, or the word that had it before the owner changed its headword
//! (`word_alias`, R-04). Shared by the import merge and the owner's
//! headword edit, so both see the same identity.

use rusqlite::{Connection, OptionalExtension};

use super::WordId;

/// The word `key` (a `headword_key`) belongs to, if any.
pub fn find_word(conn: &Connection, key: &str) -> rusqlite::Result<Option<WordId>> {
    conn.query_row(
        "SELECT id FROM word WHERE headword_key = ?1 \
         UNION ALL SELECT word_id FROM word_alias WHERE key = ?1 LIMIT 1",
        [key],
        |row| row.get(0).map(WordId::new),
    )
    .optional()
}
