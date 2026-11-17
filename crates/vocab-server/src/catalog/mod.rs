//! Vocabulary: sources, words, list items, forms, sentences, owner
//! overrides and pack conflicts (design §3.2, §4.2, §5.4).
//!
//! Owned tables: `source`, `word`, `word_source`, `word_item`, `word_form`,
//! `sentence`, `word_override`, `field_conflict`. All SQL touching them is in
//! this module (R-92). Other modules use the functions re-exported here.
//!
//! Merging (R-03, R-22) is split into a read-only *plan* and its
//! application. The import preview runs the real [`merge_word`] inside a
//! transaction that is always rolled back (`workflows`).

mod merge;
mod read;
mod source;

use serde::Serialize;

pub use merge::{ConflictReport, ImportLink, ItemsAdded, MergeOutcome, WordMerge, merge_word};
pub use read::{
    ConflictView, FormView, SentenceView, SourceLinkView, SourceSummary, TextParts, WordQuery,
    WordRow, WordView, sources, word_view, words,
};
pub use source::{ReadingUpdate, SourceUpsert, source_titles, upsert_source};

/// Id of a `source` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SourceId(i64);

impl SourceId {
    pub fn get(self) -> i64 {
        self.0
    }
}

/// Id of a `word` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
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
