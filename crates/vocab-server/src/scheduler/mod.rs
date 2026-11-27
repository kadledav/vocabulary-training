//! Cards, FSRS memory state, review log, leeches, card creation and unlock
//! (design §3.2 `scheduler`, §6.1–§6.5; R-50, R-51, R-52, R-55, R-58, R-60,
//! R-61).
//!
//! Owned tables: `card`, `review_log`, `fsrs_params`. All SQL touching them
//! is in this module (R-92). Word facts come in as [`WordTraits`] from the
//! catalog; this module never reads catalog tables.
//!
//! Scheduling is deterministic: FSRS-6 (`fsrs` 6.6.2) without fuzz,
//! intervals rounded to whole study days and clamped to 1–36500.

mod cards;
mod model;
mod owner;
mod review;

use std::fmt;

use serde::Serialize;

pub use cards::{
    Card, all_cards, card, cards_of_word, ensure_cards, introduced_words_on, latest_grades_on,
    new_cards, normal_load, reviewed_cards_on, scheduled_cards, sentence_last_shown,
};
pub use model::{Model, Rating, desired_retention, model};
pub use owner::{HistoryRow, delete_cards, history, reset_card};
pub use review::{
    AnswerMatch, RawGrade, ReviewInput, ReviewOutcome, StoredReview, UndoOutcome, UnlockedCard,
    find_review, raw_grade, record_review, review_outcome, undo_review,
};

use crate::catalog::WordTraits;

/// Id of a `card` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CardId(i64);

impl CardId {
    pub fn new(id: i64) -> Self {
        Self(id)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

/// Id of a `review_log` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ReviewId(i64);

impl ReviewId {
    pub fn new(id: i64) -> Self {
        Self(id)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            /// The database (and JSON) spelling.
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant),)+ _ => None }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let text = value.as_str()?;
                Self::parse(text).ok_or_else(|| {
                    rusqlite::types::FromSqlError::Other(
                        format!("unknown {} {text:?}", stringify!($name)).into(),
                    )
                })
            }
        }
    };
}

text_enum!(
    /// The three card kinds of a word (R-52, R-58).
    CardKind { Understand = "understand", Produce = "produce", Spelling = "spelling" }
);
text_enum!(
    /// Exercise format, fixed at a card's first review (D-2).
    CardFormat { Sentence = "sentence", WordOnly = "word_only", Cloze = "cloze", Spelling = "spelling" }
);
text_enum!(
    /// Card state (design §4.2 note): `new` = never reviewed, `relearning` =
    /// last rating today was 4/5, `review` = normal.
    CardState { New = "new", Relearning = "relearning", Review = "review" }
);
text_enum!(
    /// Leech flag (design §6.5).
    LeechState { None = "none", Flagged = "flagged", Acknowledged = "acknowledged" }
);

/// Fewest visible sentences for the `sentence` format of an Understand card
/// (R-52; fewer → `word_only`).
pub const SENTENCE_FORMAT_MIN_SENTENCES: u32 = 3;

impl CardFormat {
    /// The format a card of `kind` gets at its first review, given the
    /// word's current traits (design §6.4).
    pub fn for_first_review(kind: CardKind, word: &WordTraits) -> Self {
        match kind {
            CardKind::Understand if word.visible_sentences >= SENTENCE_FORMAT_MIN_SENTENCES => {
                Self::Sentence
            }
            CardKind::Understand => Self::WordOnly,
            CardKind::Produce => Self::Cloze,
            CardKind::Spelling => Self::Spelling,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("card {0} not found")]
    CardNotFound(i64),
    #[error("review {0} not found")]
    ReviewNotFound(i64),
    #[error("review {0} was already undone")]
    AlreadyUndone(i64),
    #[error("FSRS: {0}")]
    Fsrs(String),
    #[error("stored review data unreadable: {0}")]
    Corrupt(String),
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}
