//! Vocabulary: sources, words, list items, forms, sentences, owner
//! overrides and pack conflicts (design §3.2, §4.2, §5.4).
//!
//! Practice reads ([`traits`], [`practice_words`], [`presentation`]) give
//! the scheduler and the session builder what they need without touching
//! catalog tables (design §3.2).
//!
//! Owned tables: `source`, `word`, `word_source`, `word_item`, `word_form`,
//! `sentence`, `word_override`, `field_conflict`. The `audio` module reads
//! what can be spoken through [`speak_sources`]. All SQL touching them is in
//! this module (R-92). Other modules use the functions re-exported here.
//!
//! Merging (R-03, R-22) is split into a read-only *plan* and its
//! application. The import preview runs the real [`merge_word`] inside a
//! transaction that is always rolled back (`workflows`).

mod edit;
mod merge;
mod practice;
mod read;
mod scalar;
mod source;
mod speak;

use serde::Serialize;

pub use edit::{
    EditError, ListKind, add_item, delete_word, remove_item, resolve_conflict, set_included,
    set_sentence_hidden, set_suspended, update_sentence, update_word,
};
pub use merge::{ConflictReport, ImportLink, ItemsAdded, MergeOutcome, WordMerge, merge_word};
pub use practice::{
    PracticeWord, WordTraits, all_traits, practice_words, source_exists, traits, word_traits,
};
pub use read::{
    ConflictView, FormItemView, FormView, ItemLists, ItemView, PresentedForm, PresentedSentence,
    PresentedTag, SentenceView, SourceLinkView, SourceSummary, TextParts, WordPresentation,
    WordQuery, WordRow, WordView, presentation, sources, word_view, words,
};
pub use source::{ReadingUpdate, SourceUpsert, source_titles, upsert_source};
pub use speak::{SpeakSource, speak_source, speak_sources};

/// Hard caps on what the catalog holds (R-84): far above any real use (a
/// reader collects a few thousand words in years), low enough that a
/// runaway or hostile import cannot fill the disk.
pub const MAX_WORDS: i64 = 20_000;
pub const MAX_SENTENCES: i64 = 200_000;

/// The catalog holds more than a cap allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CapacityError {
    #[error("the app holds at most {MAX_WORDS} words")]
    Words,
    #[error("the app holds at most {MAX_SENTENCES} sentences")]
    Sentences,
}

/// Check the hard caps. Called by the import after merging a pack, inside
/// its transaction, so a pack that would pass a cap is not imported at all.
pub fn check_capacity(conn: &rusqlite::Connection) -> rusqlite::Result<Result<(), CapacityError>> {
    let count = |table: &str| {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })
    };
    Ok(if count("word")? > MAX_WORDS {
        Err(CapacityError::Words)
    } else if count("sentence")? > MAX_SENTENCES {
        Err(CapacityError::Sentences)
    } else {
        Ok(())
    })
}

/// Id of a `source` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SourceId(i64);

impl SourceId {
    pub fn new(id: i64) -> Self {
        Self(id)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

/// Id of a `word` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct WordId(i64);

impl WordId {
    pub fn new(id: i64) -> Self {
        Self(id)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}
