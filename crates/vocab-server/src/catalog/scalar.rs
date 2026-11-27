//! The scalar columns of `word` as JSON values: read, compare, write. Shared
//! by the import merge (§5.4) and the owner's edits (R-04), so both treat a
//! value the same way.

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, params};
use serde_json::Value;

use super::WordId;

/// The scalar word columns a pack fills and the owner may edit (the
/// headword is the identity and is handled apart). Field names are column
/// names; a name used in SQL is always checked against this list first.
pub const FIELDS: [&str; 11] = [
    "grammar",
    "pronunciation",
    "speak",
    "meaning",
    "usefulness",
    "goal",
    "hint",
    "spelling",
    "spelling_note",
    "note",
    "icon",
];

/// The `'static` name of a scalar field, if `name` is one.
pub fn field(name: &str) -> Option<&'static str> {
    FIELDS.into_iter().find(|f| *f == name)
}

/// Current value of a scalar column as JSON (`spelling` as a boolean).
pub fn current(conn: &Connection, word: WordId, field: &'static str) -> rusqlite::Result<Value> {
    debug_assert!(FIELDS.contains(&field));
    let sql = format!("SELECT {field} FROM word WHERE id = ?1");
    let value: SqlValue = conn.query_row(&sql, [word.get()], |row| row.get(0))?;
    Ok(match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(i) if field == "spelling" => Value::Bool(i != 0),
        SqlValue::Integer(i) => Value::from(i),
        SqlValue::Real(r) => Value::from(r),
        SqlValue::Text(t) => Value::String(t),
        SqlValue::Blob(_) => Value::Null,
    })
}

/// Store `value` in a scalar column (`note` has no NULL: cleared = empty).
pub fn write(
    conn: &Connection,
    word: WordId,
    field: &'static str,
    value: &Value,
    now: &str,
) -> rusqlite::Result<()> {
    debug_assert!(FIELDS.contains(&field));
    let value = match (field, value) {
        ("note", Value::Null) => SqlValue::Text(String::new()),
        _ => to_sql(value),
    };
    let sql = format!("UPDATE word SET {field} = ?1, updated_at = ?2 WHERE id = ?3");
    conn.execute(&sql, params![value, now, word.get()])?;
    Ok(())
}

/// Nothing stored: NULL or a blank text.
pub fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

/// The same value (texts compared trimmed; an empty text equals NULL).
pub fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x.trim() == y.trim(),
        _ if is_empty(a) && is_empty(b) => true,
        _ => a == b,
    }
}

pub fn to_sql(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => n.as_i64().map_or(SqlValue::Null, SqlValue::Integer),
        Value::String(s) => SqlValue::Text(s.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}
