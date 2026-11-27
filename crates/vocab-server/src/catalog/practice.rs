//! What the scheduler and the session builder need to know about words
//! (design §3.2 `catalog::traits` / `catalog::presentation`, §6.3, §6.7).
//!
//! These are narrow read functions: the other modules never query catalog
//! tables themselves (R-92).

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use vocab_pack::keys::item_key;
use vocab_pack::types::Goal;

use super::{SourceId, WordId};

/// The facts about a word that drive card creation, retention and session
/// building.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordTraits {
    pub id: WordId,
    /// `item_key` of the headword: compared with other words' confusables
    /// (interference rule, design §6.7 step 4).
    pub headword_item_key: String,
    /// Effective usefulness (owner edits are stored in place).
    pub usefulness: i64,
    /// Stored goal (pack or owner); `None` = derive from usefulness.
    pub goal: Option<Goal>,
    pub low_priority: bool,
    /// The owner included this low-priority word (R-51).
    pub included: bool,
    /// The word wants a Spelling card (NULL in the database = false).
    pub spelling: bool,
    pub suspended: bool,
    /// Sentences that are not hidden.
    pub visible_sentences: u32,
    /// `item_key`s of the word's confusables (not removed).
    pub confusable_keys: Vec<String>,
}

impl WordTraits {
    /// A low-priority word the owner explicitly included (R-51).
    pub fn included_low_priority(&self) -> bool {
        self.low_priority && self.included
    }

    /// Design §6.3: owner/pack goal, else derived from usefulness; an
    /// included low-priority word is always `recognise`.
    pub fn effective_goal(&self) -> Goal {
        if self.included_low_priority() {
            return Goal::Recognise;
        }
        self.goal.unwrap_or(if self.usefulness >= 6 {
            Goal::Active
        } else {
            Goal::Recognise
        })
    }

    /// May the word be introduced as a new word (low-priority words only
    /// when included, R-51)?
    pub fn introducible(&self) -> bool {
        !self.low_priority || self.included
    }

    /// Interference rule (design §6.7 step 4): one word lists the other's
    /// headword among its confusables, in either direction.
    pub fn confusable_with(&self, other: &WordTraits) -> bool {
        self.confusable_keys.contains(&other.headword_item_key)
            || other.confusable_keys.contains(&self.headword_item_key)
    }
}

const TRAITS_SELECT: &str = "SELECT w.id, w.headword, w.usefulness, w.goal, w.low_priority, \
     w.included, w.spelling, w.suspended, \
     (SELECT COUNT(*) FROM sentence s WHERE s.word_id = w.id AND s.hidden = 0), \
     (SELECT json_group_array(value_key) FROM word_item i \
        WHERE i.word_id = w.id AND i.kind = 'confusable' AND i.removed = 0) \
     FROM word w";

fn traits_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WordTraits> {
    let headword: String = row.get(1)?;
    let goal: Option<String> = row.get(3)?;
    let confusables: String = row.get(9)?;
    Ok(WordTraits {
        id: WordId(row.get(0)?),
        headword_item_key: item_key(&headword),
        usefulness: row.get(2)?,
        goal: goal.as_deref().and_then(Goal::parse),
        low_priority: row.get(4)?,
        included: row.get(5)?,
        spelling: row.get::<_, Option<bool>>(6)?.unwrap_or(false),
        suspended: row.get(7)?,
        visible_sentences: row.get(8)?,
        confusable_keys: serde_json::from_str(&confusables).unwrap_or_default(),
    })
}

/// Traits of one word (`None` if it does not exist).
pub fn word_traits(conn: &Connection, id: WordId) -> rusqlite::Result<Option<WordTraits>> {
    conn.query_row(
        &format!("{TRAITS_SELECT} WHERE w.id = ?1"),
        [id.0],
        traits_row,
    )
    .optional()
}

/// Traits of the given words (missing ids are skipped), keyed by id.
pub fn traits(conn: &Connection, ids: &[WordId]) -> rusqlite::Result<HashMap<WordId, WordTraits>> {
    let mut stmt = conn.prepare(&format!("{TRAITS_SELECT} WHERE w.id = ?1"))?;
    let mut out = HashMap::with_capacity(ids.len());
    for id in ids {
        if let Some(t) = stmt.query_row([id.0], traits_row).optional()? {
            out.insert(*id, t);
        }
    }
    Ok(out)
}

/// Traits of every word (startup backfill of cards).
pub fn all_traits(conn: &Connection) -> rusqlite::Result<Vec<WordTraits>> {
    let mut stmt = conn.prepare(&format!("{TRAITS_SELECT} ORDER BY w.id"))?;
    stmt.query_map([], traits_row)?.collect()
}

/// A word that may be practised in a scope, with its order value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PracticeWord {
    pub traits: WordTraits,
    /// `seq` within the scope's source; for all sources the smallest `seq`
    /// of the word's links (design §6.7 step 4 ordering).
    pub seq: i64,
}

/// Words that may be practised: not suspended and, when `scope` is a
/// source, linked to that source (R-59). Keyed by word id.
pub fn practice_words(
    conn: &Connection,
    scope: Option<SourceId>,
) -> rusqlite::Result<HashMap<WordId, PracticeWord>> {
    // TRAITS_SELECT's columns are 0..=9; the joined `seq` is appended as 10.
    let select = TRAITS_SELECT.replacen(" FROM word w", ", l.seq FROM word w", 1);
    let mut stmt = conn.prepare(&format!(
        "{select} JOIN (SELECT word_id, MIN(seq) AS seq FROM word_source \
           WHERE ?1 IS NULL OR source_id = ?1 GROUP BY word_id) l ON l.word_id = w.id \
         WHERE w.suspended = 0"
    ))?;
    stmt.query_map(params![scope.map(SourceId::get)], |row| {
        let traits = traits_row(row)?;
        Ok((
            traits.id,
            PracticeWord {
                traits,
                seq: row.get(10)?,
            },
        ))
    })?
    .collect()
}

/// Is any word that may be practised marked for spelling practice (it then
/// gets a Spelling card, R-58)? Today names the typed cards by what exists.
pub fn spelling_practice(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM word WHERE spelling = 1 AND suspended = 0)",
        [],
        |row| row.get(0),
    )
}

/// Does a source with this id exist?
pub fn source_exists(conn: &Connection, id: SourceId) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM source WHERE id = ?1)",
        [id.0],
        |row| row.get(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn traits(usefulness: i64) -> WordTraits {
        WordTraits {
            id: WordId(1),
            headword_item_key: "affect".to_owned(),
            usefulness,
            goal: None,
            low_priority: false,
            included: false,
            spelling: false,
            suspended: false,
            visible_sentences: 3,
            confusable_keys: Vec::new(),
        }
    }

    #[test]
    fn r52_effective_goal_derives_from_usefulness() {
        assert_eq!(traits(6).effective_goal(), Goal::Active);
        assert_eq!(traits(10).effective_goal(), Goal::Active);
        assert_eq!(traits(5).effective_goal(), Goal::Recognise);
        assert_eq!(traits(1).effective_goal(), Goal::Recognise);
    }

    #[test]
    fn r52_stored_goal_wins_over_usefulness() {
        let mut t = traits(9);
        t.goal = Some(Goal::Recognise);
        assert_eq!(t.effective_goal(), Goal::Recognise);
        let mut t = traits(2);
        t.goal = Some(Goal::Active);
        assert_eq!(t.effective_goal(), Goal::Active);
    }

    #[test]
    fn r51_included_low_priority_word_is_recognise_only() {
        let mut t = traits(9);
        t.goal = Some(Goal::Active);
        t.low_priority = true;
        assert!(!t.introducible());
        t.included = true;
        assert!(t.introducible());
        assert!(t.included_low_priority());
        assert_eq!(t.effective_goal(), Goal::Recognise);
    }

    #[test]
    fn r54_confusables_interfere_in_both_directions() {
        let mut a = traits(5);
        a.headword_item_key = "affect".to_owned();
        a.confusable_keys = vec!["effect".to_owned()];
        let mut b = traits(5);
        b.headword_item_key = "effect".to_owned();
        let mut c = traits(5);
        c.headword_item_key = "infect".to_owned();
        assert!(a.confusable_with(&b));
        assert!(b.confusable_with(&a));
        assert!(!a.confusable_with(&c));
        assert!(!c.confusable_with(&a));
    }
}
