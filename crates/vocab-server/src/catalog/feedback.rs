//! What the catalog gives the Progress screen and the exports (design §3.2
//! `catalog::feedback_items` / `export_all`, §8.1; R-57, R-72, R-86):
//! the facts of problem words, the owner's changes a next pack should know
//! about, the source links of every word, and the catalog's tables for the
//! full export.
//!
//! Each function is one statement per list (grouped joins), never one per
//! word.

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;

use super::{SourceId, WordId};
use crate::db::{TableError, TableSink, write_table};

/// The slugs (pack `source.id`) of a word's sources as a JSON array, for
/// a query whose word table is aliased `w`.
const SOURCES_OF_W: &str = "(SELECT json_group_array(slug) FROM \
     (SELECT s.slug FROM word_source ws JOIN source s ON s.id = ws.source_id \
      WHERE ws.word_id = w.id ORDER BY s.slug))";

/// The visible items of one kind of a word as a JSON array, in order.
fn items_of_w(kind: &str) -> String {
    format!(
        "(SELECT json_group_array(value) FROM (SELECT value FROM word_item \
          WHERE word_id = w.id AND kind = '{kind}' AND removed = 0 ORDER BY position))"
    )
}

fn json_list(text: &str) -> Vec<String> {
    serde_json::from_str(text).unwrap_or_default()
}

/// A problem word as the Progress screen and the feedback export show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProblemWord {
    pub id: WordId,
    pub headword: String,
    pub czech: Vec<String>,
    pub usefulness: i64,
    /// The pack's (or the owner's) memory hook; `None`: none yet.
    pub hint: Option<String>,
    pub confusables: Vec<String>,
    /// Slugs of the word's sources.
    pub sources: Vec<String>,
}

/// The given words (missing ids skipped), by headword.
pub fn problem_words(conn: &Connection, ids: &[WordId]) -> rusqlite::Result<Vec<ProblemWord>> {
    let ids = Value::from(ids.iter().map(|id| id.get()).collect::<Vec<_>>()).to_string();
    let sql = format!(
        "SELECT w.id, w.headword, w.usefulness, w.hint, {czech}, {confusables}, {SOURCES_OF_W} \
         FROM word w WHERE w.id IN (SELECT value FROM json_each(?1)) \
         ORDER BY w.headword_key, w.id",
        czech = items_of_w("czech"),
        confusables = items_of_w("confusable"),
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map([ids], |row| {
        let hint: Option<String> = row.get(3)?;
        Ok(ProblemWord {
            id: WordId::new(row.get(0)?),
            headword: row.get(1)?,
            usefulness: row.get(2)?,
            hint: hint.filter(|h| !h.trim().is_empty()),
            czech: json_list(&row.get::<_, String>(4)?),
            confusables: json_list(&row.get::<_, String>(5)?),
            sources: json_list(&row.get::<_, String>(6)?),
        })
    })?
    .collect()
}

/// Every (word, source) link (per-source progress, design §7.1 screen 9).
pub fn word_source_links(conn: &Connection) -> rusqlite::Result<Vec<(WordId, SourceId)>> {
    let mut stmt = conn.prepare("SELECT word_id, source_id FROM word_source ORDER BY rowid")?;
    stmt.query_map([], |row| {
        Ok((WordId::new(row.get(0)?), SourceId::new(row.get(1)?)))
    })?
    .collect()
}

/// A word field the owner set in the app, with its value now.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedbackOverride {
    pub word: String,
    pub sources: Vec<String>,
    pub field: String,
    pub value: Value,
}

/// A sentence the owner hid or corrected (marked text, `[[…]]` around the
/// target as in a pack).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedbackSentence {
    pub word: String,
    pub sources: Vec<String>,
    pub en: String,
    pub cs: String,
}

/// Answers the owner accepted for a word in Produce ("Also accept").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedbackAccepted {
    pub word: String,
    pub sources: Vec<String>,
    pub answers: Vec<String>,
}

/// A pack's suggestion the owner has not answered yet.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedbackConflict {
    pub word: String,
    pub sources: Vec<String>,
    pub field: String,
    /// The value the app keeps.
    pub current: Value,
    /// What the later pack suggested.
    pub incoming: Value,
}

/// The catalog's part of the feedback export (R-57, D-12).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CatalogFeedback {
    pub overrides: Vec<FeedbackOverride>,
    pub hidden_sentences: Vec<FeedbackSentence>,
    pub edited_sentences: Vec<FeedbackSentence>,
    pub owner_accepted: Vec<FeedbackAccepted>,
    pub open_conflicts: Vec<FeedbackConflict>,
}

/// A JSON-encoded scalar as stored in `field_conflict`.
fn stored_json(text: Option<String>) -> Value {
    text.and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

fn sql_json(value: SqlValue, field: &str) -> Value {
    match value {
        SqlValue::Null | SqlValue::Blob(_) => Value::Null,
        SqlValue::Integer(i) if field == "spelling" => Value::Bool(i != 0),
        SqlValue::Integer(i) => Value::from(i),
        SqlValue::Real(r) => Value::from(r),
        SqlValue::Text(t) => Value::String(t),
    }
}

/// Everything the owner changed that a next pack should react to.
pub fn feedback_items(conn: &Connection) -> rusqlite::Result<CatalogFeedback> {
    // The override's current value, from the column it names (the scalar
    // fields of `scalar::FIELDS`; anything else has no value to report).
    let value_case = format!(
        "CASE o.field {} END",
        super::scalar::FIELDS
            .iter()
            .map(|f| format!("WHEN '{f}' THEN w.{f}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut stmt = conn.prepare(&format!(
        "SELECT w.headword, {SOURCES_OF_W}, o.field, {value_case} \
         FROM word_override o JOIN word w ON w.id = o.word_id \
         ORDER BY w.headword_key, w.id, o.field"
    ))?;
    let overrides = stmt
        .query_map([], |row| {
            let field: String = row.get(2)?;
            Ok(FeedbackOverride {
                word: row.get(0)?,
                sources: json_list(&row.get::<_, String>(1)?),
                value: sql_json(row.get(3)?, &field),
                field,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let sentences = |flag: &str| -> rusqlite::Result<Vec<FeedbackSentence>> {
        let mut stmt = conn.prepare(&format!(
            "SELECT w.headword, {SOURCES_OF_W}, s.en_marked, s.cs_marked \
             FROM sentence s JOIN word w ON w.id = s.word_id WHERE s.{flag} = 1 \
             ORDER BY w.headword_key, w.id, s.position, s.id"
        ))?;
        stmt.query_map([], |row| {
            Ok(FeedbackSentence {
                word: row.get(0)?,
                sources: json_list(&row.get::<_, String>(1)?),
                en: row.get(2)?,
                cs: row.get(3)?,
            })
        })?
        .collect()
    };
    let hidden_sentences = sentences("hidden")?;
    // A hidden sentence is reported as hidden, also when it was edited.
    let edited_sentences = sentences("edited")?
        .into_iter()
        .filter(|e| {
            !hidden_sentences
                .iter()
                .any(|h| h.word == e.word && h.en == e.en)
        })
        .collect();

    let mut stmt = conn.prepare(&format!(
        "SELECT w.headword, {SOURCES_OF_W}, {answers} FROM word w \
         WHERE EXISTS (SELECT 1 FROM word_item i WHERE i.word_id = w.id \
                       AND i.kind = 'owner_accepted' AND i.removed = 0) \
         ORDER BY w.headword_key, w.id",
        answers = items_of_w("owner_accepted"),
    ))?;
    let owner_accepted = stmt
        .query_map([], |row| {
            Ok(FeedbackAccepted {
                word: row.get(0)?,
                sources: json_list(&row.get::<_, String>(1)?),
                answers: json_list(&row.get::<_, String>(2)?),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut stmt = conn.prepare(&format!(
        "SELECT w.headword, {SOURCES_OF_W}, c.field, c.current_value, c.incoming_value \
         FROM field_conflict c JOIN word w ON w.id = c.word_id WHERE c.status = 'open' \
         ORDER BY w.headword_key, w.id, c.id"
    ))?;
    let open_conflicts = stmt
        .query_map(params![], |row| {
            Ok(FeedbackConflict {
                word: row.get(0)?,
                sources: json_list(&row.get::<_, String>(1)?),
                field: row.get(2)?,
                current: stored_json(row.get(3)?),
                incoming: stored_json(row.get(4)?),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(CatalogFeedback {
        overrides,
        hidden_sentences,
        edited_sentences,
        owner_accepted,
        open_conflicts,
    })
}

/// The catalog's tables for the full export (design §8.1), every column,
/// in id order: the vocabulary as stored, the owner's changes included.
pub fn export(conn: &Connection, sink: &mut dyn TableSink) -> Result<(), TableError> {
    const TABLES: [(&str, &str); 9] = [
        ("sources", "SELECT * FROM source ORDER BY id"),
        ("words", "SELECT * FROM word ORDER BY id"),
        (
            "word_sources",
            "SELECT * FROM word_source ORDER BY word_id, source_id",
        ),
        (
            "word_items",
            "SELECT * FROM word_item ORDER BY word_id, kind, position, value_key",
        ),
        (
            "word_forms",
            "SELECT * FROM word_form ORDER BY word_id, position, form_key",
        ),
        ("sentences", "SELECT * FROM sentence ORDER BY id"),
        (
            "word_overrides",
            "SELECT * FROM word_override ORDER BY word_id, field",
        ),
        (
            "field_conflicts",
            "SELECT * FROM field_conflict ORDER BY id",
        ),
        (
            "word_pack_spellings",
            "SELECT * FROM word_pack_spelling ORDER BY word_id, key",
        ),
    ];
    for (name, sql) in TABLES {
        write_table(conn, sink, name, sql)?;
    }
    Ok(())
}
