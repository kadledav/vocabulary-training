//! Ids of catalog rows that appear in URLs are never reused (design §4.2,
//! migration 0007): `word`, `sentence` and `field_conflict` rows are named
//! by their id in `/api/words/{id}`, `/api/sentences/{id}` and
//! `/api/conflicts/{id}`. SQLite gives a new row the highest stored id + 1,
//! so after deleting the newest word a stale page (or an Undo) would reach
//! the next imported word. Triggers keep the highest deleted id per table
//! in `id_floor`; a new row gets an id above both.

use rusqlite::Connection;

/// A table whose ids are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdTable {
    Word,
    Sentence,
    FieldConflict,
}

impl IdTable {
    fn name(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Sentence => "sentence",
            Self::FieldConflict => "field_conflict",
        }
    }
}

/// The id for the next row of `table`: above every stored and every deleted
/// id. Call right before the insert, inside the same transaction.
pub fn next_id(conn: &Connection, table: IdTable) -> rusqlite::Result<i64> {
    let name = table.name();
    // `name` is one of the three fixed table names above.
    conn.query_row(
        &format!(
            "SELECT MAX(COALESCE((SELECT MAX(id) FROM {name}), 0), \
             COALESCE((SELECT last_id FROM id_floor WHERE name = ?1), 0)) + 1"
        ),
        [name],
        |row| row.get(0),
    )
}
