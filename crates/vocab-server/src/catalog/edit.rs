//! The owner's changes to a word in the app (design §7.1 screen 7; R-03,
//! R-04, R-25, R-51).
//!
//! Every value is checked with the pack's own rules (`vocab-pack`
//! `owner_*` functions), so what the owner types can always be stored and
//! shown like imported text (R-84).
//!
//! What an edit leaves behind for the next import (design §5.4):
//!
//! * A scalar field the owner changes is recorded in `word_override`: a
//!   pack that disagrees later only adds a suggestion (`field_conflict`),
//!   it never overwrites the owner's value (rule 4). An open suggestion of
//!   exactly the value the owner now chose is settled (`applied`).
//! * A changed headword keeps its former key as an alias (`word_alias`), so
//!   the pack's spelling still finds this word.
//! * A removed list item or form stays as a `removed` row: a pack never adds
//!   it again. Adding it back revives that row.
//! * An edited or hidden sentence is never changed by a pack (`edited`,
//!   `hidden`). Its `en_key` stays the key the pack's text has, so a
//!   re-import recognises it and leaves it alone.
//! * *Use new* on a suggestion stores the pack's value and drops the
//!   override (the field follows the packs again); *Keep* dismisses it, and
//!   the same suggestion is not raised again.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;
use vocab_pack::keys::{headword_key, item_key, sentence_key};
use vocab_pack::types::Issue;
use vocab_pack::validate::{
    MAX_LIST_ITEMS, owner_form, owner_list_item, owner_sentence, owner_word_edit,
};

use super::identity::find_word;
use super::{WordId, scalar};

/// Why an owner change was not made. The messages are for the owner.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("This word no longer exists.")]
    WordNotFound,
    #[error("This sentence no longer exists.")]
    SentenceNotFound,
    #[error("This suggestion no longer exists.")]
    ConflictNotFound,
    #[error("This item is not in the list (any more).")]
    ItemNotFound,
    /// The value breaks a rule of the pack format; the message says which.
    #[error("{0}")]
    Invalid(String),
    #[error("Another word already has the headword \"{0}\".")]
    HeadwordTaken(String),
    #[error("This word already has the same sentence.")]
    DuplicateSentence,
    #[error("This suggestion was already answered.")]
    ConflictClosed,
    #[error("A word keeps at least one Czech equivalent; add another one first.")]
    LastCzech,
    #[error("A list holds at most {MAX_LIST_ITEMS} items.")]
    TooManyItems,
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

fn invalid(issues: &[Issue]) -> EditError {
    let message = issues
        .iter()
        .map(|issue| issue.message.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    EditError::Invalid(if message.is_empty() {
        "The value is not valid.".to_owned()
    } else {
        message
    })
}

fn word_exists(conn: &Connection, id: WordId) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM word WHERE id = ?1)",
        [id.get()],
        |row| row.get(0),
    )
}

fn touch(tx: &Transaction<'_>, id: WordId, now: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE word SET updated_at = ?2 WHERE id = ?1",
        params![id.get(), now],
    )?;
    Ok(())
}

// ── scalar fields and the headword ──────────────────────────────────────────

/// Change word fields (R-04). `edit` is the owner's JSON object
/// `{field: value, …}` (see `vocab_pack::validate::owner_word_edit`).
/// Returns the fields whose stored value changed (an unchanged value is
/// not recorded as the owner's).
pub fn update_word(
    tx: &Transaction<'_>,
    id: WordId,
    edit: &Value,
    now: &str,
) -> Result<Vec<&'static str>, EditError> {
    let edits = owner_word_edit(edit).map_err(|issues| invalid(&issues))?;
    if !word_exists(tx, id)? {
        return Err(EditError::WordNotFound);
    }
    // As the import: a spelling note is only stored with spelling practice.
    let spelling_after = match edits.iter().find(|(name, _)| *name == "spelling") {
        Some((_, value)) => value.clone(),
        None => scalar::current(tx, id, "spelling")?,
    };
    let note_given = edits
        .iter()
        .any(|(name, value)| *name == "spelling_note" && !value.is_null());
    if note_given && spelling_after != Value::Bool(true) {
        return Err(EditError::Invalid(
            "A spelling note needs spelling practice; turn spelling on first.".to_owned(),
        ));
    }
    let mut changed = Vec::new();
    for (name, value) in edits {
        if name == "headword" {
            if let Value::String(headword) = &value
                && change_headword(tx, id, headword, now)?
            {
                changed.push(name);
            }
            continue;
        }
        let Some(field) = scalar::field(name) else {
            continue;
        };
        let current = scalar::current(tx, id, field)?;
        if scalar::same(&current, &value) {
            continue;
        }
        scalar::write(tx, id, field, &value, now)?;
        tx.execute(
            "INSERT INTO word_override (word_id, field, set_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT (word_id, field) DO UPDATE SET set_at = excluded.set_at",
            params![id.get(), field, now],
        )?;
        // A suggestion the owner has now taken himself is settled.
        tx.execute(
            "UPDATE field_conflict SET status = 'applied', resolved_at = ?3 \
             WHERE word_id = ?1 AND field = ?2 AND status = 'open' AND incoming_value = ?4",
            params![id.get(), field, now, value.to_string()],
        )?;
        changed.push(field);
    }
    Ok(changed)
}

/// Store a new spelling of the headword. A different key keeps the old one
/// as an alias (a pack with the old spelling still finds the word); a key
/// that belongs to another word is refused.
fn change_headword(
    tx: &Transaction<'_>,
    id: WordId,
    headword: &str,
    now: &str,
) -> Result<bool, EditError> {
    let (old, old_key): (String, String) = tx.query_row(
        "SELECT headword, headword_key FROM word WHERE id = ?1",
        [id.get()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if old == headword {
        return Ok(false);
    }
    let key = headword_key(headword);
    if key != old_key {
        if let Some(other) = find_word(tx, &key)?
            && other != id
        {
            let taken: String = tx.query_row(
                "SELECT headword FROM word WHERE id = ?1",
                [other.get()],
                |row| row.get(0),
            )?;
            return Err(EditError::HeadwordTaken(taken));
        }
        // Back to a former spelling: that key is the word's own again.
        tx.execute("DELETE FROM word_alias WHERE key = ?1", [&key])?;
        tx.execute(
            "INSERT OR IGNORE INTO word_alias (key, word_id) VALUES (?1, ?2)",
            params![old_key, id.get()],
        )?;
    }
    tx.execute(
        "UPDATE word SET headword = ?2, headword_key = ?3, updated_at = ?4 WHERE id = ?1",
        params![id.get(), headword, key, now],
    )?;
    Ok(true)
}

// ── list items and forms ────────────────────────────────────────────────────

/// The list fields the owner edits item by item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Czech,
    /// Accepted answers: the pack's (`accepted`) and the owner's own
    /// (`owner_accepted`, design §6.8).
    Accepted,
    Confusable,
    Form,
}

impl ListKind {
    /// From the API name (`czech`, `accepted`, `confusable`, `form`).
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "czech" => Some(Self::Czech),
            "accepted" => Some(Self::Accepted),
            "confusable" => Some(Self::Confusable),
            "form" => Some(Self::Form),
            _ => None,
        }
    }

    /// `word_item.kind`s of the list (`Form` lives in `word_form`).
    fn item_kinds(self) -> &'static [&'static str] {
        match self {
            Self::Czech => &["czech"],
            Self::Accepted => &["accepted", "owner_accepted"],
            Self::Confusable => &["confusable"],
            Self::Form => &[],
        }
    }

    /// The kind an item the owner adds is stored as.
    fn owner_kind(self) -> &'static str {
        match self {
            Self::Czech => "czech",
            Self::Accepted => "owner_accepted",
            Self::Confusable => "confusable",
            Self::Form => "form",
        }
    }
}

/// Add an item to a list (`common` only for forms, default true). An item
/// that is already there is left as it is; one the owner removed earlier
/// comes back.
pub fn add_item(
    tx: &Transaction<'_>,
    id: WordId,
    kind: ListKind,
    value: &str,
    common: Option<bool>,
    now: &str,
) -> Result<(), EditError> {
    if !word_exists(tx, id)? {
        return Err(EditError::WordNotFound);
    }
    if kind == ListKind::Form {
        return add_form(tx, id, value, common.unwrap_or(true), now);
    }
    let value = owner_list_item(value).map_err(|issue| invalid(&[issue]))?;
    let key = item_key(&value);
    let kinds = kind.item_kinds();
    let mut stmt = tx.prepare(
        "SELECT kind, removed FROM word_item WHERE word_id = ?1 AND value_key = ?2 \
         ORDER BY removed, kind",
    )?;
    let rows: Vec<(String, bool)> = stmt
        .query_map(params![id.get(), key], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let rows: Vec<(String, bool)> = rows
        .into_iter()
        .filter(|(k, _)| kinds.contains(&k.as_str()))
        .collect();
    match rows.first() {
        Some((_, false)) => return Ok(()),
        Some((stored, true)) => {
            tx.execute(
                "UPDATE word_item SET removed = 0 WHERE word_id = ?1 AND kind = ?2 AND value_key = ?3",
                params![id.get(), stored, key],
            )?;
        }
        None => {
            if active_items(tx, id, kinds)? >= MAX_LIST_ITEMS {
                return Err(EditError::TooManyItems);
            }
            tx.execute(
                "INSERT INTO word_item (word_id, kind, value, value_key, position, origin) \
                 VALUES (?1, ?2, ?3, ?4, \
                 (SELECT COALESCE(MAX(position) + 1, 0) FROM word_item WHERE word_id = ?1 AND kind = ?2), \
                 'owner')",
                params![id.get(), kind.owner_kind(), value, key],
            )?;
        }
    }
    touch(tx, id, now)?;
    Ok(())
}

fn active_items(conn: &Connection, id: WordId, kinds: &[&str]) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare("SELECT kind FROM word_item WHERE word_id = ?1 AND removed = 0")?;
    let all: Vec<String> = stmt
        .query_map([id.get()], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(all
        .iter()
        .filter(|kind| kinds.contains(&kind.as_str()))
        .count())
}

fn add_form(
    tx: &Transaction<'_>,
    id: WordId,
    form: &str,
    common: bool,
    now: &str,
) -> Result<(), EditError> {
    let form = owner_form(form).map_err(|issue| invalid(&[issue]))?;
    let key = item_key(&form);
    let stored: Option<bool> = tx
        .query_row(
            "SELECT removed FROM word_form WHERE word_id = ?1 AND form_key = ?2",
            params![id.get(), key],
            |row| row.get(0),
        )
        .optional()?;
    match stored {
        // Already there: the owner's `common` flag wins.
        Some(_) => {
            tx.execute(
                "UPDATE word_form SET removed = 0, common = ?3 WHERE word_id = ?1 AND form_key = ?2",
                params![id.get(), key, common],
            )?;
        }
        None => {
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM word_form WHERE word_id = ?1 AND removed = 0",
                [id.get()],
                |row| row.get(0),
            )?;
            if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_LIST_ITEMS {
                return Err(EditError::TooManyItems);
            }
            tx.execute(
                "INSERT INTO word_form (word_id, form, form_key, common, position) VALUES (?1, ?2, ?3, ?4, \
                 (SELECT COALESCE(MAX(position) + 1, 0) FROM word_form WHERE word_id = ?1))",
                params![id.get(), form, key, common],
            )?;
        }
    }
    touch(tx, id, now)?;
    Ok(())
}

/// Remove an item (by its `item_key`) from a list. It stays stored as
/// removed, so no pack brings it back. The last Czech equivalent stays.
pub fn remove_item(
    tx: &Transaction<'_>,
    id: WordId,
    kind: ListKind,
    key: &str,
    now: &str,
) -> Result<(), EditError> {
    if !word_exists(tx, id)? {
        return Err(EditError::WordNotFound);
    }
    let removed = if kind == ListKind::Form {
        tx.execute(
            "UPDATE word_form SET removed = 1 WHERE word_id = ?1 AND form_key = ?2 AND removed = 0",
            params![id.get(), key],
        )?
    } else {
        let kinds = kind.item_kinds();
        if kind == ListKind::Czech && active_items(tx, id, kinds)? <= 1 {
            let present: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM word_item WHERE word_id = ?1 AND kind = 'czech' \
                 AND value_key = ?2 AND removed = 0)",
                params![id.get(), key],
                |row| row.get(0),
            )?;
            if present {
                return Err(EditError::LastCzech);
            }
        }
        let mut n = 0;
        for item_kind in kinds {
            n += tx.execute(
                "UPDATE word_item SET removed = 1 WHERE word_id = ?1 AND kind = ?2 \
                 AND value_key = ?3 AND removed = 0",
                params![id.get(), item_kind, key],
            )?;
        }
        n
    };
    if removed == 0 {
        return Err(EditError::ItemNotFound);
    }
    touch(tx, id, now)?;
    Ok(())
}

// ── sentences (R-25) ────────────────────────────────────────────────────────

/// Correct a sentence's English and Czech text (both with their `[[…]]`
/// span). Returns its word. A sentence the same as another one of the word
/// is refused; an unchanged text changes nothing.
pub fn update_sentence(
    tx: &Transaction<'_>,
    sentence: i64,
    en: &str,
    cs: &str,
    now: &str,
) -> Result<WordId, EditError> {
    let (en, cs) = owner_sentence(en, cs).map_err(|issues| invalid(&issues))?;
    let (word, old_en, old_cs): (i64, String, String) = tx
        .query_row(
            "SELECT word_id, en_marked, cs_marked FROM sentence WHERE id = ?1",
            [sentence],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or(EditError::SentenceNotFound)?;
    let word = WordId::new(word);
    if old_en == en && old_cs == cs {
        return Ok(word);
    }
    let key = sentence_key(&en);
    let mut stmt =
        tx.prepare("SELECT en_marked, en_key FROM sentence WHERE word_id = ?1 AND id != ?2")?;
    let others: Vec<(String, String)> = stmt
        .query_map(params![word.get(), sentence], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<_, _>>()?;
    if others
        .iter()
        .any(|(text, stored)| *stored == key || sentence_key(text) == key)
    {
        return Err(EditError::DuplicateSentence);
    }
    tx.execute(
        "UPDATE sentence SET en_marked = ?2, cs_marked = ?3, edited = 1, updated_at = ?4 WHERE id = ?1",
        params![sentence, en, cs, now],
    )?;
    touch(tx, word, now)?;
    Ok(word)
}

/// Hide a sentence from practice (or show it again). Returns its word.
pub fn set_sentence_hidden(
    tx: &Transaction<'_>,
    sentence: i64,
    hidden: bool,
    now: &str,
) -> Result<WordId, EditError> {
    let word: i64 = tx
        .query_row(
            "SELECT word_id FROM sentence WHERE id = ?1",
            [sentence],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(EditError::SentenceNotFound)?;
    tx.execute(
        "UPDATE sentence SET hidden = ?2, updated_at = ?3 WHERE id = ?1 AND hidden != ?2",
        params![sentence, hidden, now],
    )?;
    Ok(WordId::new(word))
}

// ── pack suggestions (R-03) ─────────────────────────────────────────────────

/// Answer a pack's suggestion: `use_incoming` stores the pack's value (the
/// field then follows the packs again: its override is dropped); otherwise
/// the stored value stays and this suggestion is not raised again.
/// Returns the word.
pub fn resolve_conflict(
    tx: &Transaction<'_>,
    conflict: i64,
    use_incoming: bool,
    now: &str,
) -> Result<WordId, EditError> {
    let (word, field, incoming, status): (i64, String, String, String) = tx
        .query_row(
            "SELECT word_id, field, incoming_value, status FROM field_conflict WHERE id = ?1",
            [conflict],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or(EditError::ConflictNotFound)?;
    if status != "open" {
        return Err(EditError::ConflictClosed);
    }
    let word = WordId::new(word);
    if !use_incoming {
        tx.execute(
            "UPDATE field_conflict SET status = 'dismissed', resolved_at = ?2 WHERE id = ?1",
            params![conflict, now],
        )?;
        return Ok(word);
    }
    let field = scalar::field(&field)
        .ok_or_else(|| EditError::Invalid(format!("The field {field} cannot be set.")))?;
    let value: Value = serde_json::from_str(&incoming)
        .map_err(|_| EditError::Invalid("The suggested value cannot be read.".to_owned()))?;
    scalar::write(tx, word, field, &value, now)?;
    tx.execute(
        "DELETE FROM word_override WHERE word_id = ?1 AND field = ?2",
        params![word.get(), field],
    )?;
    // This suggestion and any other one of the same value are settled.
    tx.execute(
        "UPDATE field_conflict SET status = 'applied', resolved_at = ?4 WHERE word_id = ?1 \
         AND field = ?2 AND status = 'open' AND (id = ?3 OR incoming_value = ?5)",
        params![word.get(), field, conflict, now, incoming],
    )?;
    Ok(word)
}

// ── word flags and deletion ─────────────────────────────────────────────────

/// Suspend a word (no card of it is practised) or bring it back.
pub fn set_suspended(
    tx: &Transaction<'_>,
    id: WordId,
    suspended: bool,
    now: &str,
) -> Result<(), EditError> {
    set_flag(tx, id, "suspended", suspended, now)
}

/// Include a low-priority word in practice (R-51), or not any more.
pub fn set_included(
    tx: &Transaction<'_>,
    id: WordId,
    included: bool,
    now: &str,
) -> Result<(), EditError> {
    set_flag(tx, id, "included", included, now)
}

fn set_flag(
    tx: &Transaction<'_>,
    id: WordId,
    column: &'static str,
    value: bool,
    now: &str,
) -> Result<(), EditError> {
    // `column` is one of the two fixed names above.
    let sql = format!("UPDATE word SET {column} = ?2, updated_at = ?3 WHERE id = ?1");
    if tx.execute(&sql, params![id.get(), value, now])? == 0 {
        return Err(EditError::WordNotFound);
    }
    Ok(())
}

/// Delete a word with everything the catalog holds about it (sources
/// links, lists, forms, sentences, overrides, suggestions, aliases). The
/// caller removes the word's cards first (`scheduler`).
pub fn delete_word(tx: &Transaction<'_>, id: WordId) -> Result<(), EditError> {
    if tx.execute("DELETE FROM word WHERE id = ?1", [id.get()])? == 0 {
        return Err(EditError::WordNotFound);
    }
    Ok(())
}
