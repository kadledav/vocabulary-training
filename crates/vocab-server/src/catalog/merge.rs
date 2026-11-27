//! Merging a pack word into the catalog (design §5.4, R-03, R-22).
//!
//! A pack word is matched to an existing word by `headword_key` or by a
//! spelling an earlier pack used for it (`identity`). The merge
//! never touches learning state and never deletes anything:
//!
//! | Fields | Rule |
//! |---|---|
//! | scalars | incoming absent → nothing; equal (trim) → nothing; owner override → keep + conflict; existing empty → take incoming; else keep + conflict (not again when the owner dismissed the same suggestion) |
//! | `czech`, `accepted`, `confusables` | union by item key; owner-removed items are not re-added |
//! | `forms` | union by form key; an existing form keeps its `common` flag |
//! | sentences | dedupe by sentence key; new ones appended; a known one whose `cs` or marked English span differs is updated to the pack's version, unless the owner edited or hid it (English that differs only in what the key folds, e.g. how a quote was cut, is not a correction) |
//! | source link | created with the next `seq` (or the pack's explicit `seq`); an existing link keeps its `seq`; its tag changes only when the pack gives a tag object (no `tag`, or `null`, keeps it) |
//! | `low_priority` | owner override, else the most recently imported link's tag |
//!
//! The override check runs before "existing empty → take incoming" so a
//! field the owner deliberately cleared is not refilled by a pack.
//! `spelling` is NULL until a pack says something, so it can be filled;
//! `spelling_note` is only filled when the word's resulting `spelling` is
//! true. A field the pack leaves out never changes anything.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::Value;
use vocab_pack::keys::{item_key, span_key};
use vocab_pack::types::{PackSentence, PackWord, TagKind};

use super::identity::{find_word, record_pack_spelling};
use super::ids::{IdTable, next_id};
use super::{SourceId, WordId, scalar};

/// Where an imported word comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportLink<'a> {
    pub source_id: SourceId,
    /// `pack_import.id` of the pack being imported (owned by `importer`).
    pub pack_import_id: i64,
    /// RFC 3339 timestamp of the import.
    pub now: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeOutcome {
    /// A new word.
    New,
    /// An existing word that gained something (or a new conflict record).
    Merged,
    /// An existing word with nothing new.
    Unchanged,
}

/// A scalar field where the pack disagrees with the stored value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConflictReport {
    pub field: &'static str,
    pub current: Value,
    pub incoming: Value,
    /// The same suggestion is already waiting for the owner; not recorded again.
    pub already_open: bool,
}

/// List items added to an existing word.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ItemsAdded {
    pub czech: u32,
    pub accepted: u32,
    pub confusables: u32,
    pub forms: u32,
}

impl ItemsAdded {
    fn total(self) -> u32 {
        self.czech + self.accepted + self.confusables + self.forms
    }
}

/// Outcome of merging one pack word (also the dry-run result).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WordMerge {
    /// `None` in a preview of a new word.
    pub word_id: Option<WordId>,
    pub outcome: MergeOutcome,
    /// Scalar fields that were empty and were filled from the pack.
    pub filled: Vec<&'static str>,
    pub conflicts: Vec<ConflictReport>,
    pub items_added: ItemsAdded,
    pub sentences_added: u32,
    /// Known sentences whose text or marked spans the pack corrected.
    pub sentences_updated: u32,
    /// Sentences of the pack the word already had (nothing to do), plus
    /// those that differ but are kept because the owner edited or hid them.
    pub sentences_known: u32,
    /// The word was not yet linked to this source.
    pub source_linked: bool,
    /// The tag of an existing link changed.
    pub tag_changed: bool,
}

// ── plan ────────────────────────────────────────────────────────────────────

/// Kinds of `word_item` a pack fills, with the pack list they come from.
const LIST_KINDS: [&str; 3] = ["czech", "accepted", "confusable"];

struct Plan {
    existing: Option<WordId>,
    fills: Vec<(&'static str, Value)>,
    conflicts: Vec<ConflictReport>,
    /// (kind, value, key)
    new_items: Vec<(&'static str, String, String)>,
    /// Indices into `word.forms`.
    new_forms: Vec<usize>,
    /// Indices into `word.sentences`.
    new_sentences: Vec<usize>,
    /// (`sentence.id`, index into `word.sentences`): known sentences the
    /// pack corrects.
    updated_sentences: Vec<(i64, usize)>,
    sentences_known: u32,
    link: LinkPlan,
}

enum LinkPlan {
    New,
    Existing { tag_changed: bool },
}

impl Plan {
    fn report(&self, word_id: Option<WordId>) -> WordMerge {
        let mut items = ItemsAdded::default();
        for (kind, _, _) in &self.new_items {
            match *kind {
                "czech" => items.czech += 1,
                "accepted" => items.accepted += 1,
                _ => items.confusables += 1,
            }
        }
        items.forms = count(self.new_forms.len());
        let (source_linked, tag_changed) = match self.link {
            LinkPlan::New => (true, false),
            LinkPlan::Existing { tag_changed } => (false, tag_changed),
        };
        let outcome = if self.existing.is_none() {
            MergeOutcome::New
        } else if !self.fills.is_empty()
            || self.conflicts.iter().any(|c| !c.already_open)
            || items.total() > 0
            || !self.new_sentences.is_empty()
            || !self.updated_sentences.is_empty()
            || source_linked
            || tag_changed
        {
            MergeOutcome::Merged
        } else {
            MergeOutcome::Unchanged
        };
        WordMerge {
            word_id,
            outcome,
            filled: self.fills.iter().map(|(field, _)| *field).collect(),
            conflicts: self.conflicts.clone(),
            items_added: items,
            sentences_added: count(self.new_sentences.len()),
            sentences_updated: count(self.updated_sentences.len()),
            sentences_known: self.sentences_known,
            source_linked,
            tag_changed,
        }
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The scalar fields of a pack word as JSON values (`None` = the pack says
/// nothing). Field names are the `word` column names.
fn incoming_scalars(word: &PackWord) -> [(&'static str, Option<Value>); 11] {
    let text = |v: &Option<String>| v.clone().map(Value::String);
    [
        ("grammar", text(&word.grammar)),
        ("pronunciation", text(&word.pronunciation)),
        ("speak", text(&word.speak)),
        ("meaning", Some(Value::String(word.meaning.clone()))),
        ("usefulness", Some(Value::from(word.usefulness))),
        (
            "goal",
            word.goal.map(|g| Value::String(g.as_str().to_owned())),
        ),
        ("hint", text(&word.hint)),
        ("spelling", word.spelling.map(Value::Bool)),
        ("spelling_note", text(&word.spelling_note)),
        ("note", text(&word.note)),
        ("icon", text(&word.icon)),
    ]
}

fn plan(conn: &Connection, source: SourceId, word: &PackWord) -> rusqlite::Result<Plan> {
    let Some(id) = find_word(conn, &word.key)? else {
        return Ok(Plan {
            existing: None,
            fills: Vec::new(),
            conflicts: Vec::new(),
            new_items: new_items(word, &HashSet::new()),
            new_forms: new_forms(word, &HashSet::new()),
            new_sentences: (0..word.sentences.len()).collect(),
            updated_sentences: Vec::new(),
            sentences_known: 0,
            link: LinkPlan::New,
        });
    };

    let overrides: HashSet<String> = collect(
        conn,
        "SELECT field FROM word_override WHERE word_id = ?1",
        id,
    )?;
    let mut fills: Vec<(&'static str, Value)> = Vec::new();
    let mut conflicts = Vec::new();
    let mut spelling_after = None;
    for (field, incoming) in incoming_scalars(word) {
        let current = scalar::current(conn, id, field)?;
        if field == "spelling" {
            spelling_after = incoming.clone().or_else(|| Some(current.clone()));
        }
        let Some(incoming) = incoming else { continue };
        if scalar::same(&current, &incoming) {
            continue;
        }
        if !overrides.contains(field) && scalar::is_empty(&current) {
            if field == "spelling" {
                spelling_after = Some(incoming.clone());
            }
            let note_without_spelling =
                field == "spelling_note" && spelling_after != Some(Value::Bool(true));
            if !note_without_spelling {
                fills.push((field, incoming));
            }
            continue;
        }
        if field == "spelling" {
            spelling_after = Some(current.clone());
        }
        let earlier: Option<String> = conn
            .query_row(
                "SELECT status FROM field_conflict WHERE word_id = ?1 AND field = ?2 \
                 AND incoming_value = ?3 AND status IN ('open', 'dismissed') \
                 ORDER BY status = 'dismissed' DESC LIMIT 1",
                params![id.0, field, incoming.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if earlier.as_deref() == Some("dismissed") {
            // The owner already answered "keep" to this very suggestion.
            continue;
        }
        let already_open = earlier.is_some();
        conflicts.push(ConflictReport {
            field,
            current,
            incoming,
            already_open,
        });
    }

    // Lists: union by key; any existing row (also removed ones) blocks re-adding.
    let known_items: HashSet<(String, String)> = {
        let mut stmt = conn.prepare("SELECT kind, value_key FROM word_item WHERE word_id = ?1")?;
        stmt.query_map([id.0], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?
    };
    let new_items = new_items(word, &known_items);
    let known_forms: HashSet<String> = collect(
        conn,
        "SELECT form_key FROM word_form WHERE word_id = ?1",
        id,
    )?;
    let new_forms = new_forms(word, &known_forms);

    let known_sentences = known_sentences(conn, id)?;
    let mut new_sentences = Vec::new();
    let mut updated_sentences = Vec::new();
    let mut sentences_known = 0;
    for (i, sentence) in word.sentences.iter().enumerate() {
        match known_sentences.get(&sentence.key) {
            None => new_sentences.push(i),
            // A correction (another translation, another span) reaches the
            // app; what the owner edited or hid stays his.
            Some(known) if !known.owner_changed && known.corrected_by(sentence) => {
                updated_sentences.push((known.id, i));
            }
            Some(_) => sentences_known += 1,
        }
    }

    let link = conn
        .query_row(
            "SELECT tag_kind, tag_text FROM word_source WHERE word_id = ?1 AND source_id = ?2",
            params![id.0, source.0],
            |row| {
                let kind: Option<String> = row.get(0)?;
                let text: Option<String> = row.get(1)?;
                let tag_changed = match &word.tag {
                    None => false,
                    Some(tag) => kind.as_deref() != Some(tag.kind.as_str()) || text != tag.text,
                };
                Ok(LinkPlan::Existing { tag_changed })
            },
        )
        .optional()?
        .unwrap_or(LinkPlan::New);

    Ok(Plan {
        existing: Some(id),
        fills,
        conflicts,
        new_items,
        new_forms,
        new_sentences,
        updated_sentences,
        sentences_known,
        link,
    })
}

/// List items of `word` whose (kind, key) is not in `known`, deduplicated,
/// in pack order.
fn new_items(
    word: &PackWord,
    known: &HashSet<(String, String)>,
) -> Vec<(&'static str, String, String)> {
    let mut out: Vec<(&'static str, String, String)> = Vec::new();
    for (kind, list) in LIST_KINDS
        .into_iter()
        .zip([&word.czech, &word.accepted, &word.confusables])
    {
        for value in list {
            let key = item_key(value);
            let seen = known.contains(&(kind.to_owned(), key.clone()))
                || out.iter().any(|(k, _, nk)| *k == kind && *nk == key);
            if !seen {
                out.push((kind, value.clone(), key));
            }
        }
    }
    out
}

/// Indices of forms of `word` whose key is not in `known`, deduplicated.
fn new_forms(word: &PackWord, known: &HashSet<String>) -> Vec<usize> {
    let mut seen = HashSet::new();
    word.forms
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            let key = item_key(&f.form);
            !known.contains(&key) && seen.insert(key)
        })
        .map(|(i, _)| i)
        .collect()
}

fn collect(conn: &Connection, sql: &str, id: WordId) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare(sql)?;
    stmt.query_map([id.0], |row| row.get(0))?.collect()
}

/// A stored sentence of a word, as far as the merge needs it.
struct KnownSentence {
    id: i64,
    en: String,
    cs: String,
    /// The owner edited or hid it: a pack never changes it (design §5.4).
    owner_changed: bool,
}

impl KnownSentence {
    /// Does the pack's version of this sentence correct it? Yes when the
    /// Czech text (translation or its marked span) differs, or when another
    /// part of the English sentence is marked. The English text itself is
    /// the same by definition (same key); how it was cut or punctuated this
    /// time is not a reason to replace it.
    fn corrected_by(&self, incoming: &PackSentence) -> bool {
        self.cs != incoming.cs || span_key(&self.en) != span_key(&incoming.en)
    }
}

fn known_sentences(
    conn: &Connection,
    id: WordId,
) -> rusqlite::Result<HashMap<String, KnownSentence>> {
    let mut stmt = conn.prepare(
        "SELECT en_key, id, en_marked, cs_marked, edited OR hidden FROM sentence WHERE word_id = ?1",
    )?;
    stmt.query_map([id.0], |row| {
        Ok((
            row.get(0)?,
            KnownSentence {
                id: row.get(1)?,
                en: row.get(2)?,
                cs: row.get(3)?,
                owner_changed: row.get(4)?,
            },
        ))
    })?
    .collect()
}

// ── public operations ───────────────────────────────────────────────────────

/// Merge `word` into the catalog and link it to the pack's source.
pub fn merge_word(
    tx: &Transaction<'_>,
    link: &ImportLink<'_>,
    word: &PackWord,
) -> rusqlite::Result<WordMerge> {
    let plan = plan(tx, link.source_id, word)?;
    let id = match plan.existing {
        Some(id) => {
            apply_existing(tx, id, link, &plan)?;
            id
        }
        None => insert_word(tx, link, word)?,
    };
    // How this pack spelled the word: a later pack with that spelling finds
    // it, whatever the owner calls it by then.
    record_pack_spelling(tx, id, &word.key, &word.headword)?;
    apply_lists(tx, id, word, &plan)?;
    apply_sentences(tx, id, link, word, &plan)?;
    apply_link(tx, id, link, word)?;
    update_low_priority(tx, id, link.now)?;
    Ok(plan.report(Some(id)))
}

// ── apply ───────────────────────────────────────────────────────────────────

fn insert_word(
    tx: &Transaction<'_>,
    link: &ImportLink<'_>,
    word: &PackWord,
) -> rusqlite::Result<WordId> {
    let id = next_id(tx, IdTable::Word)?;
    tx.execute(
        "INSERT INTO word (headword, headword_key, grammar, pronunciation, speak, meaning, usefulness, \
         goal, hint, spelling, spelling_note, note, icon, created_at, updated_at, id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15)",
        params![
            word.headword,
            word.key,
            word.grammar,
            word.pronunciation,
            word.speak,
            word.meaning,
            word.usefulness,
            word.goal.map(|g| g.as_str()),
            word.hint,
            word.spelling,
            // A note about a spelling trap only makes sense with `spelling: true`.
            word.spelling_note.as_ref().filter(|_| word.spelling == Some(true)),
            word.note.as_deref().unwrap_or(""),
            word.icon,
            link.now,
            id,
        ],
    )?;
    Ok(WordId(id))
}

fn apply_existing(
    tx: &Transaction<'_>,
    id: WordId,
    link: &ImportLink<'_>,
    plan: &Plan,
) -> rusqlite::Result<()> {
    for (field, value) in &plan.fills {
        scalar::write(tx, id, field, value, link.now)?;
    }
    for conflict in plan.conflicts.iter().filter(|c| !c.already_open) {
        let current = (!conflict.current.is_null()).then(|| conflict.current.to_string());
        tx.execute(
            "INSERT INTO field_conflict (word_id, field, current_value, incoming_value, \
             pack_import_id, status, created_at, id) VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7)",
            params![
                id.0,
                conflict.field,
                current,
                conflict.incoming.to_string(),
                link.pack_import_id,
                link.now,
                next_id(tx, IdTable::FieldConflict)?,
            ],
        )?;
    }
    Ok(())
}

fn apply_lists(
    tx: &Transaction<'_>,
    id: WordId,
    word: &PackWord,
    plan: &Plan,
) -> rusqlite::Result<()> {
    for (kind, value, key) in &plan.new_items {
        tx.execute(
            "INSERT INTO word_item (word_id, kind, value, value_key, position, origin) \
             VALUES (?1, ?2, ?3, ?4, \
             (SELECT COALESCE(MAX(position) + 1, 0) FROM word_item WHERE word_id = ?1 AND kind = ?2), 'pack')",
            params![id.0, kind, value, key],
        )?;
    }
    for &i in &plan.new_forms {
        let form = &word.forms[i];
        tx.execute(
            "INSERT INTO word_form (word_id, form, form_key, common, position) \
             VALUES (?1, ?2, ?3, ?4, (SELECT COALESCE(MAX(position) + 1, 0) FROM word_form WHERE word_id = ?1))",
            params![id.0, form.form, item_key(&form.form), form.common],
        )?;
    }
    Ok(())
}

fn apply_sentences(
    tx: &Transaction<'_>,
    id: WordId,
    link: &ImportLink<'_>,
    word: &PackWord,
    plan: &Plan,
) -> rusqlite::Result<()> {
    for &i in &plan.new_sentences {
        let s = &word.sentences[i];
        tx.execute(
            "INSERT INTO sentence (word_id, en_marked, cs_marked, en_key, origin, location, source_id, \
             position, pack_import_id, created_at, updated_at, id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, \
             (SELECT COALESCE(MAX(position) + 1, 0) FROM sentence WHERE word_id = ?1), ?8, ?9, ?9, ?10)",
            params![
                id.0,
                s.en,
                s.cs,
                s.key,
                s.origin.as_str(),
                s.location,
                link.source_id.0,
                link.pack_import_id,
                link.now,
                next_id(tx, IdTable::Sentence)?,
            ],
        )?;
    }
    for &(sentence_id, i) in &plan.updated_sentences {
        let s = &word.sentences[i];
        // Same row, same key: reviews that showed the sentence keep it.
        tx.execute(
            "UPDATE sentence SET en_marked = ?2, cs_marked = ?3, origin = ?4, \
             location = COALESCE(?5, location), pack_import_id = ?6, updated_at = ?7 WHERE id = ?1",
            params![
                sentence_id,
                s.en,
                s.cs,
                s.origin.as_str(),
                s.location,
                link.pack_import_id,
                link.now,
            ],
        )?;
    }
    Ok(())
}

/// Create the word ↔ source link (with the pack's explicit `seq`, or the
/// next one), or update an existing link: its tag when the pack gives a tag
/// (it describes this source relationship; a pack without one, or with
/// `"tag": null`, says nothing and the stored tag stays) and its last import. The
/// `seq` of an existing link never changes (design §5.4); explicit and
/// derived values may collide, and ties are ordered by link creation.
fn apply_link(
    tx: &Transaction<'_>,
    id: WordId,
    link: &ImportLink<'_>,
    word: &PackWord,
) -> rusqlite::Result<()> {
    let tag = word
        .tag
        .as_ref()
        .map(|tag| (tag.kind.as_str(), tag.text.as_deref()));
    let updated = match &tag {
        Some((tag_kind, tag_text)) => tx.execute(
            "UPDATE word_source SET tag_kind = ?3, tag_text = ?4, last_pack_import_id = ?5 \
             WHERE word_id = ?1 AND source_id = ?2",
            params![
                id.0,
                link.source_id.0,
                tag_kind,
                tag_text,
                link.pack_import_id
            ],
        )?,
        None => tx.execute(
            "UPDATE word_source SET last_pack_import_id = ?3 WHERE word_id = ?1 AND source_id = ?2",
            params![id.0, link.source_id.0, link.pack_import_id],
        )?,
    };
    if updated == 0 {
        let (tag_kind, tag_text) = tag.unzip();
        let tag_text = tag_text.flatten();
        let explicit_seq = word.seq.map(i64::from);
        tx.execute(
            "INSERT INTO word_source (word_id, source_id, seq, tag_kind, tag_text, \
             first_pack_import_id, last_pack_import_id) VALUES (?1, ?2, \
             COALESCE(?5, (SELECT COALESCE(MAX(seq), 0) + 1 FROM word_source WHERE source_id = ?2)), \
             ?3, ?4, ?6, ?6)",
            params![id.0, link.source_id.0, tag_kind, tag_text, explicit_seq, link.pack_import_id],
        )?;
    }
    Ok(())
}

/// `word.low_priority` = owner override if set, else "the most recently
/// imported link has tag `low-priority`" (design §5.4).
fn update_low_priority(tx: &Transaction<'_>, id: WordId, now: &str) -> rusqlite::Result<()> {
    let overridden = tx
        .query_row(
            "SELECT 1 FROM word_override WHERE word_id = ?1 AND field = 'low_priority'",
            [id.0],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if overridden {
        return Ok(());
    }
    let latest: Option<String> = tx
        .query_row(
            "SELECT tag_kind FROM word_source WHERE word_id = ?1 \
             ORDER BY last_pack_import_id DESC, source_id DESC LIMIT 1",
            [id.0],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let low = latest.as_deref() == Some(TagKind::LowPriority.as_str());
    tx.execute(
        "UPDATE word SET low_priority = ?2, updated_at = ?3 WHERE id = ?1 AND low_priority != ?2",
        params![id.0, low, now],
    )?;
    Ok(())
}
