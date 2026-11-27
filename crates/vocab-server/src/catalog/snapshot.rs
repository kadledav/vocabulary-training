//! Everything the catalog holds about one word, as one value: taken before
//! and after an owner change, and written back by its Undo (design §8
//! `POST /api/words/{id}/undo`). The rows are kept column by column as
//! stored, so a restore puts back exactly what was there (override rows,
//! `edited`/`hidden` flags, removed items, suggestions and their status,
//! pack spellings, timestamps).
//!
//! Only the word's own rows are covered. The owner's changes never add or
//! delete sentences, so sentences are written back in place (their ids are
//! referenced by reviews and practice items); the other tables are
//! replaced where they differ.

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, Transaction, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::WordId;
use super::edit::EditError;
use super::identity::{KeyHolder, holder};

/// Tables with a `word_id` whose rows are replaced as a whole.
const CHILD_TABLES: [&str; 6] = [
    "word_source",
    "word_item",
    "word_form",
    "word_override",
    "field_conflict",
    "word_pack_spelling",
];

/// Rows of one table: column names and values (JSON scalars).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Rows {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
}

/// The catalog's state of one word (opaque to other modules).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordSnapshot {
    id: i64,
    word: Rows,
    sentences: Rows,
    /// In the order of [`CHILD_TABLES`].
    children: Vec<Rows>,
}

fn to_json(value: SqlValue) -> Value {
    match value {
        SqlValue::Null | SqlValue::Blob(_) => Value::Null,
        SqlValue::Integer(i) => Value::from(i),
        SqlValue::Real(r) => Value::from(r),
        SqlValue::Text(t) => Value::String(t),
    }
}

fn to_sql(value: &Value) -> SqlValue {
    match value {
        Value::Number(n) => n
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| n.as_f64().map(SqlValue::Real))
            .unwrap_or(SqlValue::Null),
        Value::String(s) => SqlValue::Text(s.clone()),
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        _ => SqlValue::Null,
    }
}

/// `SELECT *` rows of `table` where `column = id`, ordered by rowid.
fn rows(conn: &Connection, table: &str, column: &str, id: i64) -> rusqlite::Result<Rows> {
    // `table` and `column` are fixed names of this module.
    let mut stmt = conn.prepare(&format!(
        "SELECT * FROM {table} WHERE {column} = ?1 ORDER BY rowid"
    ))?;
    let columns: Vec<String> = stmt.column_names().into_iter().map(str::to_owned).collect();
    let n = columns.len();
    let rows = stmt
        .query_map([id], |row| {
            (0..n)
                .map(|i| row.get::<_, SqlValue>(i).map(to_json))
                .collect::<rusqlite::Result<Vec<Value>>>()
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Rows { columns, rows })
}

/// The word's state (`None` if it does not exist).
pub fn snapshot(conn: &Connection, id: WordId) -> rusqlite::Result<Option<WordSnapshot>> {
    let word = rows(conn, "word", "id", id.get())?;
    if word.rows.is_empty() {
        return Ok(None);
    }
    let children = CHILD_TABLES
        .iter()
        .map(|table| rows(conn, table, "word_id", id.get()))
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(WordSnapshot {
        id: id.get(),
        word,
        sentences: rows(conn, "sentence", "word_id", id.get())?,
        children,
    }))
}

/// Write one stored row back in place (`UPDATE … WHERE id = ?`).
fn update_row(
    tx: &Transaction<'_>,
    table: &str,
    rows: &Rows,
    row: &[Value],
) -> rusqlite::Result<()> {
    let id_at = rows.columns.iter().position(|c| c == "id");
    let Some(id_at) = id_at else {
        return Ok(());
    };
    let sets: Vec<String> = rows
        .columns
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != id_at)
        .map(|(i, c)| format!("{c} = ?{}", i + 1))
        .collect();
    let sql = format!(
        "UPDATE {table} SET {} WHERE id = ?{}",
        sets.join(", "),
        id_at + 1
    );
    tx.execute(&sql, params_from_iter(row.iter().map(to_sql)))?;
    Ok(())
}

/// Replace the rows of a child table of the word.
fn replace_rows(tx: &Transaction<'_>, table: &str, id: i64, rows: &Rows) -> rusqlite::Result<()> {
    tx.execute(&format!("DELETE FROM {table} WHERE word_id = ?1"), [id])?;
    if rows.rows.is_empty() {
        return Ok(());
    }
    let marks: Vec<String> = (1..=rows.columns.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        rows.columns.join(", "),
        marks.join(", ")
    );
    let mut stmt = tx.prepare(&sql)?;
    for row in &rows.rows {
        stmt.execute(params_from_iter(row.iter().map(to_sql)))?;
    }
    Ok(())
}

/// The keys `snapshot` needs to own again: its headword key and its pack
/// spellings. One another word holds now blocks the restore.
fn check_keys(tx: &Transaction<'_>, snapshot: &WordSnapshot) -> Result<(), EditError> {
    let column = |rows: &Rows, name: &str| rows.columns.iter().position(|c| c == name);
    let mut keys: Vec<String> = Vec::new();
    if let (Some(at), Some(row)) = (
        column(&snapshot.word, "headword_key"),
        snapshot.word.rows.first(),
    ) && let Some(key) = row[at].as_str()
    {
        keys.push(key.to_owned());
    }
    let spellings = CHILD_TABLES
        .iter()
        .position(|t| *t == "word_pack_spelling")
        .and_then(|i| snapshot.children.get(i));
    if let Some(spellings) = spellings
        && let Some(at) = column(spellings, "key")
    {
        keys.extend(
            spellings
                .rows
                .iter()
                .filter_map(|r| r[at].as_str().map(str::to_owned)),
        );
    }
    for key in keys {
        match holder(tx, &key)? {
            Some(other) if other.word().get() != snapshot.id => {
                return Err(match other {
                    KeyHolder::Headword { headword, .. } => EditError::HeadwordTaken(headword),
                    KeyHolder::PackSpelling {
                        spelling, headword, ..
                    } => EditError::SpellingTaken { spelling, headword },
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Put the word back to `before`. The caller has checked that the word is
/// still exactly as the change left it (so the same sentences exist).
pub fn restore(tx: &Transaction<'_>, before: &WordSnapshot) -> Result<(), EditError> {
    let id = WordId::new(before.id);
    let current = snapshot(tx, id)?.ok_or(EditError::WordNotFound)?;
    check_keys(tx, before)?;
    if current.word != before.word
        && let Some(row) = before.word.rows.first()
    {
        update_row(tx, "word", &before.word, row)?;
    }
    if current.sentences != before.sentences {
        for row in &before.sentences.rows {
            update_row(tx, "sentence", &before.sentences, row)?;
        }
    }
    for ((table, now), then) in CHILD_TABLES
        .iter()
        .zip(&current.children)
        .zip(&before.children)
    {
        if now != then {
            replace_rows(tx, table, before.id, then)?;
        }
    }
    Ok(())
}

/// The word a sentence belongs to (`None`: no such sentence).
pub fn word_of_sentence(conn: &Connection, sentence: i64) -> rusqlite::Result<Option<WordId>> {
    conn.query_row(
        "SELECT word_id FROM sentence WHERE id = ?1",
        [sentence],
        |row| row.get(0).map(WordId::new),
    )
    .optional()
}

/// The word a suggestion belongs to (`None`: no such suggestion).
pub fn word_of_conflict(conn: &Connection, conflict: i64) -> rusqlite::Result<Option<WordId>> {
    conn.query_row(
        "SELECT word_id FROM field_conflict WHERE id = ?1",
        [conflict],
        |row| row.get(0).map(WordId::new),
    )
    .optional()
}
