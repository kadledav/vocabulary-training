//! Read-only views of the catalog: sources, the word list and the word view
//! (design §7.1 Words/Word detail, minimal version for M2).

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use vocab_pack::keys::item_key;
use vocab_pack::marked;

use super::{SourceId, WordId};

/// Most rows returned by [`words`].
const WORD_LIST_LIMIT: i64 = 5000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceSummary {
    pub id: SourceId,
    pub slug: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub author: Option<String>,
    pub series: Option<String>,
    pub series_number: Option<i64>,
    pub url: Option<String>,
    /// Latest received reading position (R-59).
    pub reading_position: Option<String>,
    pub reading_position_exported_at: Option<String>,
    pub word_count: i64,
}

/// All sources, by title.
pub fn sources(conn: &Connection) -> rusqlite::Result<Vec<SourceSummary>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.slug, s.type, s.title, s.author, s.series, s.series_number, s.url, \
         s.reading_position, s.reading_position_exported_at, \
         (SELECT COUNT(*) FROM word_source ws WHERE ws.source_id = s.id) \
         FROM source s ORDER BY s.title COLLATE NOCASE, s.id",
    )?;
    stmt.query_map([], |row| {
        Ok(SourceSummary {
            id: SourceId(row.get(0)?),
            slug: row.get(1)?,
            kind: row.get(2)?,
            title: row.get(3)?,
            author: row.get(4)?,
            series: row.get(5)?,
            series_number: row.get(6)?,
            url: row.get(7)?,
            reading_position: row.get(8)?,
            reading_position_exported_at: row.get(9)?,
            word_count: row.get(10)?,
        })
    })?
    .collect()
}

/// Filter of the word list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WordQuery {
    /// Plain substring of the headword, a Czech equivalent or the meaning
    /// (compared on mechanical keys: case and whitespace folded).
    pub text: Option<String>,
    /// Only words linked to this source, in source order (`seq`; equal
    /// values in the order the words were linked).
    pub source: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WordRow {
    pub id: WordId,
    pub headword: String,
    pub meaning: String,
    pub czech: Vec<String>,
    pub usefulness: i64,
    pub low_priority: bool,
    pub sentence_count: i64,
    pub open_conflicts: i64,
    /// Position within the filtered source.
    pub seq: Option<i64>,
}

/// The word list, alphabetical (or in source order when filtered by source).
pub fn words(conn: &Connection, query: &WordQuery) -> rusqlite::Result<Vec<WordRow>> {
    let needle = query
        .text
        .as_deref()
        .map(item_key)
        .filter(|t| !t.is_empty());
    let mut stmt = conn.prepare(
        "SELECT w.id, w.headword, w.meaning, w.usefulness, w.low_priority, \
         (SELECT COUNT(*) FROM sentence s WHERE s.word_id = w.id AND s.hidden = 0), \
         (SELECT COUNT(*) FROM field_conflict c WHERE c.word_id = w.id AND c.status = 'open'), \
         ws.seq, \
         (SELECT json_group_array(value) FROM (SELECT value FROM word_item i \
            WHERE i.word_id = w.id AND i.kind = 'czech' AND i.removed = 0 ORDER BY position)) \
         FROM word w \
         LEFT JOIN word_source ws ON ws.word_id = w.id AND ws.source_id = ?1 \
         WHERE (?1 IS NULL OR ws.source_id IS NOT NULL) \
           AND (?2 IS NULL OR instr(w.headword_key, ?2) > 0 OR instr(lower(w.meaning), ?2) > 0 \
                OR EXISTS (SELECT 1 FROM word_item i WHERE i.word_id = w.id AND i.kind = 'czech' \
                           AND i.removed = 0 AND instr(i.value_key, ?2) > 0)) \
         ORDER BY CASE WHEN ?1 IS NULL THEN 0 ELSE ws.seq END, \
                  CASE WHEN ?1 IS NULL THEN 0 ELSE ws.rowid END, w.headword_key, w.id \
         LIMIT ?3",
    )?;
    stmt.query_map(params![query.source, needle, WORD_LIST_LIMIT], |row| {
        let czech: String = row.get(8)?;
        Ok(WordRow {
            id: WordId(row.get(0)?),
            headword: row.get(1)?,
            meaning: row.get(2)?,
            usefulness: row.get(3)?,
            low_priority: row.get(4)?,
            sentence_count: row.get(5)?,
            open_conflicts: row.get(6)?,
            seq: row.get(7)?,
            czech: serde_json::from_str(&czech).unwrap_or_default(),
        })
    })?
    .collect()
}

/// A marked text split for display; the client renders `target` highlighted
/// as text, never as markup (R-84).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextParts {
    pub pre: String,
    pub target: String,
    pub post: String,
}

impl TextParts {
    fn of(text: &str) -> Self {
        match marked::split(text) {
            Ok(m) => Self {
                pre: m.pre,
                target: m.target,
                post: m.post,
            },
            // Stored text always passed the scanner; show it plain otherwise.
            Err(_) => Self {
                pre: text.to_owned(),
                target: String::new(),
                post: String::new(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SentenceView {
    pub id: i64,
    pub en: TextParts,
    pub cs: TextParts,
    pub origin: String,
    pub location: Option<String>,
    pub source_title: Option<String>,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceLinkView {
    pub source_id: SourceId,
    pub title: String,
    pub seq: i64,
    pub tag_kind: Option<String>,
    pub tag_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormView {
    pub form: String,
    pub common: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConflictView {
    pub id: i64,
    pub field: String,
    pub current: Value,
    pub incoming: Value,
    pub created_at: String,
}

/// Everything stored about one word.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WordView {
    pub id: WordId,
    pub headword: String,
    pub grammar: Option<String>,
    pub pronunciation: Option<String>,
    pub speak: Option<String>,
    pub meaning: String,
    pub usefulness: i64,
    pub goal: Option<String>,
    pub hint: Option<String>,
    pub spelling: bool,
    pub spelling_note: Option<String>,
    pub note: String,
    pub icon: Option<String>,
    pub low_priority: bool,
    pub suspended: bool,
    pub czech: Vec<String>,
    pub accepted: Vec<String>,
    pub confusables: Vec<String>,
    pub forms: Vec<FormView>,
    pub sources: Vec<SourceLinkView>,
    pub sentences: Vec<SentenceView>,
    pub conflicts: Vec<ConflictView>,
    pub created_at: String,
    pub updated_at: String,
}

/// One word with its lists, sources, sentences (in order) and open conflicts.
pub fn word_view(conn: &Connection, id: WordId) -> rusqlite::Result<Option<WordView>> {
    let base = conn
        .query_row(
            "SELECT headword, grammar, pronunciation, speak, meaning, usefulness, goal, hint, spelling, \
             spelling_note, note, icon, low_priority, suspended, created_at, updated_at \
             FROM word WHERE id = ?1",
            [id.0],
            |row| {
                Ok(WordView {
                    id,
                    headword: row.get(0)?,
                    grammar: row.get(1)?,
                    pronunciation: row.get(2)?,
                    speak: row.get(3)?,
                    meaning: row.get(4)?,
                    usefulness: row.get(5)?,
                    goal: row.get(6)?,
                    hint: row.get(7)?,
                    // NULL: no pack said anything yet = no spelling card.
                    spelling: row.get::<_, Option<bool>>(8)?.unwrap_or(false),
                    spelling_note: row.get(9)?,
                    note: row.get(10)?,
                    icon: row.get(11)?,
                    low_priority: row.get(12)?,
                    suspended: row.get(13)?,
                    created_at: row.get(14)?,
                    updated_at: row.get(15)?,
                    czech: Vec::new(),
                    accepted: Vec::new(),
                    confusables: Vec::new(),
                    forms: Vec::new(),
                    sources: Vec::new(),
                    sentences: Vec::new(),
                    conflicts: Vec::new(),
                })
            },
        )
        .optional()?;
    let Some(mut view) = base else {
        return Ok(None);
    };
    let items = |kind: &str| -> rusqlite::Result<Vec<String>> {
        let mut stmt = conn.prepare(
            "SELECT value FROM word_item WHERE word_id = ?1 AND kind = ?2 AND removed = 0 ORDER BY position",
        )?;
        stmt.query_map(params![id.0, kind], |row| row.get(0))?
            .collect()
    };
    view.czech = items("czech")?;
    view.accepted = items("accepted")?;
    view.confusables = items("confusable")?;

    let mut stmt =
        conn.prepare("SELECT form, common FROM word_form WHERE word_id = ?1 ORDER BY position")?;
    view.forms = stmt
        .query_map([id.0], |row| {
            Ok(FormView {
                form: row.get(0)?,
                common: row.get(1)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    let mut stmt = conn.prepare(
        "SELECT ws.source_id, s.title, ws.seq, ws.tag_kind, ws.tag_text FROM word_source ws \
         JOIN source s ON s.id = ws.source_id WHERE ws.word_id = ?1 ORDER BY ws.first_pack_import_id, s.id",
    )?;
    view.sources = stmt
        .query_map([id.0], |row| {
            Ok(SourceLinkView {
                source_id: SourceId(row.get(0)?),
                title: row.get(1)?,
                seq: row.get(2)?,
                tag_kind: row.get(3)?,
                tag_text: row.get(4)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    let mut stmt = conn.prepare(
        "SELECT se.id, se.en_marked, se.cs_marked, se.origin, se.location, s.title, se.hidden \
         FROM sentence se LEFT JOIN source s ON s.id = se.source_id \
         WHERE se.word_id = ?1 ORDER BY se.position, se.id",
    )?;
    view.sentences = stmt
        .query_map([id.0], |row| {
            let en: String = row.get(1)?;
            let cs: String = row.get(2)?;
            Ok(SentenceView {
                id: row.get(0)?,
                en: TextParts::of(&en),
                cs: TextParts::of(&cs),
                origin: row.get(3)?,
                location: row.get(4)?,
                source_title: row.get(5)?,
                hidden: row.get(6)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    let mut stmt = conn.prepare(
        "SELECT id, field, current_value, incoming_value, created_at FROM field_conflict \
         WHERE word_id = ?1 AND status = 'open' ORDER BY id",
    )?;
    view.conflicts = stmt
        .query_map([id.0], |row| {
            let current: Option<String> = row.get(2)?;
            let incoming: String = row.get(3)?;
            Ok(ConflictView {
                id: row.get(0)?,
                field: row.get(1)?,
                current: current
                    .and_then(|c| serde_json::from_str(&c).ok())
                    .unwrap_or(Value::Null),
                incoming: serde_json::from_str(&incoming).unwrap_or(Value::Null),
                created_at: row.get(4)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(Some(view))
}

// ── practice view (design §8 item payload) ─────────────────────────────────

/// A visible sentence as practice shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresentedSentence {
    pub id: i64,
    pub en: TextParts,
    pub cs: TextParts,
    pub origin: String,
    pub location: Option<String>,
    /// The pack source the sentence arrived with (scoped blocks prefer
    /// their source's sentences, R-59).
    pub source_id: Option<SourceId>,
    pub source_title: Option<String>,
    /// Order within the word (lowest first on ties, design §6.7 step 6).
    #[serde(skip)]
    pub position: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresentedForm {
    pub form: String,
    pub common: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PresentedTag {
    pub kind: String,
    pub text: Option<String>,
}

/// Everything a practice item shows about its word (design §8 item payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WordPresentation {
    pub id: WordId,
    pub headword: String,
    pub pronunciation: Option<String>,
    /// Text for speech when it differs from the headword (pack `speak`);
    /// the browser-speech fallback reads it (R-41).
    pub speak: Option<String>,
    pub grammar: Option<String>,
    pub czech: Vec<String>,
    pub meaning: String,
    pub forms: Vec<PresentedForm>,
    pub note: String,
    pub hint: Option<String>,
    pub icon: Option<String>,
    pub spelling_note: Option<String>,
    pub confusables: Vec<String>,
    /// Tag of the most recently imported source link, if any.
    pub tag: Option<PresentedTag>,
    /// Pack `accepted` answers (Produce, design §6.8).
    #[serde(skip)]
    pub accepted: Vec<String>,
    /// Owner-accepted answers (Produce, design §6.8).
    #[serde(skip)]
    pub owner_accepted: Vec<String>,
    /// Visible sentences in order.
    #[serde(skip)]
    pub sentences: Vec<PresentedSentence>,
}

/// The practice view of one word (`None` if it does not exist).
pub fn presentation(conn: &Connection, id: WordId) -> rusqlite::Result<Option<WordPresentation>> {
    let base = conn
        .query_row(
            "SELECT headword, pronunciation, grammar, meaning, note, hint, icon, spelling, spelling_note, \
             speak FROM word WHERE id = ?1",
            [id.0],
            |row| {
                let spelling = row.get::<_, Option<bool>>(7)?.unwrap_or(false);
                Ok(WordPresentation {
                    id,
                    headword: row.get(0)?,
                    pronunciation: row.get(1)?,
                    speak: row.get(9)?,
                    grammar: row.get(2)?,
                    meaning: row.get(3)?,
                    note: row.get(4)?,
                    hint: row.get(5)?,
                    icon: row.get(6)?,
                    spelling_note: if spelling { row.get(8)? } else { None },
                    czech: Vec::new(),
                    forms: Vec::new(),
                    confusables: Vec::new(),
                    tag: None,
                    accepted: Vec::new(),
                    owner_accepted: Vec::new(),
                    sentences: Vec::new(),
                })
            },
        )
        .optional()?;
    let Some(mut view) = base else {
        return Ok(None);
    };
    let mut items = conn.prepare(
        "SELECT value FROM word_item WHERE word_id = ?1 AND kind = ?2 AND removed = 0 ORDER BY position",
    )?;
    let mut list = |kind: &str| -> rusqlite::Result<Vec<String>> {
        items
            .query_map(params![id.0, kind], |row| row.get(0))?
            .collect()
    };
    view.czech = list("czech")?;
    view.confusables = list("confusable")?;
    view.accepted = list("accepted")?;
    view.owner_accepted = list("owner_accepted")?;

    let mut stmt =
        conn.prepare("SELECT form, common FROM word_form WHERE word_id = ?1 ORDER BY position")?;
    view.forms = stmt
        .query_map([id.0], |row| {
            Ok(PresentedForm {
                form: row.get(0)?,
                common: row.get(1)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    view.tag = conn
        .query_row(
            "SELECT tag_kind, tag_text FROM word_source WHERE word_id = ?1 AND tag_kind IS NOT NULL \
             ORDER BY last_pack_import_id DESC, source_id DESC LIMIT 1",
            [id.0],
            |row| {
                Ok(PresentedTag {
                    kind: row.get(0)?,
                    text: row.get(1)?,
                })
            },
        )
        .optional()?;

    let mut stmt = conn.prepare(
        "SELECT se.id, se.en_marked, se.cs_marked, se.origin, se.location, s.title, se.position, se.source_id \
         FROM sentence se LEFT JOIN source s ON s.id = se.source_id \
         WHERE se.word_id = ?1 AND se.hidden = 0 ORDER BY se.position, se.id",
    )?;
    view.sentences = stmt
        .query_map([id.0], |row| {
            let en: String = row.get(1)?;
            let cs: String = row.get(2)?;
            Ok(PresentedSentence {
                id: row.get(0)?,
                en: TextParts::of(&en),
                cs: TextParts::of(&cs),
                origin: row.get(3)?,
                location: row.get(4)?,
                source_title: row.get(5)?,
                position: row.get(6)?,
                source_id: row.get::<_, Option<i64>>(7)?.map(SourceId),
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(Some(view))
}
