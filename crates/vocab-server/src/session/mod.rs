//! Practice blocks: build, persist the queue, requeue repeats, resume, undo,
//! summary (design §3.2 `session`, §6.6, §6.7; R-54, R-59).
//!
//! Owned tables: `practice_session`, `session_item`. Cards and reviews are
//! changed only through `scheduler` functions; word facts come from
//! `catalog` read functions (R-92).
//!
//! A built block keeps its items: lowering `new_per_day` later does not
//! remove new items already queued (guarantor decision G4).
//!
//! The block builder serves only the card kinds whose practice UI exists
//! ([`SERVED_KINDS`]: Understand and Produce; Spelling from M10). Spelling
//! cards are still created and unlocked by the scheduler; they wait (stay
//! due) until their kind is served. Produce/Spelling cards that wait for
//! their first review come in only up to a daily cap, with a neutral
//! priority ([`TypedAllowance`], design §17 M8).

mod build;
mod queue;
mod view;

use std::fmt;

use serde::{Deserialize, Serialize};

pub use build::{
    BlockPreview, DueToday, NewAllowance, TypedAllowance, choose_sentence, due_today,
    extra_available, new_candidate_count, new_word_allowance, reviews_in_scope, start_block,
    typed_allowance, typed_waiting,
};
pub use queue::{finish, rate, undo_last};
pub use view::{
    AnswerView, BlockSummary, FinishedBlocks, ItemCard, ItemView, LookAgain, RateResult,
    SessionView, StartResult, UndoResult, current, finished_blocks, withdraw_card,
};

use crate::catalog::SourceId;
use crate::scheduler::{AnswerMatch, CardKind, RawGrade, SchedulerError};

/// Card kinds the session builder serves (M8: Understand and Produce).
pub const SERVED_KINDS: &[CardKind] = &[CardKind::Understand, CardKind::Produce];

/// Id of a practice block: 128 random bits as 32 hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Accept only the canonical form (anything else cannot exist).
    pub fn parse(text: &str) -> Option<Self> {
        (text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| Self(text.to_owned()))
    }

    fn random() -> Result<Self, SessionError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| SessionError::Random(e.to_string()))?;
        Ok(Self(hex::encode(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `start` (first block) or `continue` (Continue +20) build a regular block
/// the same way (design §6.7 step 5); `extra` builds an extra-practice block
/// of cards that are not due (R-63, §6.9); `more_new` a block of new words
/// only ("Learn more new words", §6.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartKind {
    Start,
    Continue,
    Extra,
    MoreNew,
}

/// Most new words one "Learn more new words" block may ask for.
pub const MORE_NEW_MAX: u32 = 50;
/// Fewest new words a "Learn more new words" block gets by default (the
/// default is the `new_per_day` setting, at least this).
pub const MORE_NEW_DEFAULT_MIN: u32 = 5;

/// What kind of block a stored block is (`practice_session.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// Due cards with new words mixed in (`start`, `continue`).
    Regular,
    /// Extra practice of cards that are not due (R-63).
    Extra,
    /// New words only, beyond today's limit.
    MoreNew,
}

impl BlockKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Extra => "extra",
            Self::MoreNew => "more_new",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "regular" => Some(Self::Regular),
            "extra" => Some(Self::Extra),
            "more_new" => Some(Self::MoreNew),
            _ => None,
        }
    }
}

/// What `POST /api/sessions` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    /// `None` = all sources (R-59).
    pub scope: Option<SourceId>,
    pub kind: StartKind,
    /// "Add new words anyway (today)": overrides the backlog pause for the
    /// rest of the study day (design §6.6).
    pub add_new_anyway: bool,
    /// `more_new` only: how many new words (1–[`MORE_NEW_MAX`]); `None` =
    /// the `new_per_day` setting, at least [`MORE_NEW_DEFAULT_MIN`].
    pub count: Option<u32>,
}

/// One rating posted by the client (design §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateInput {
    pub client_review_id: String,
    pub item_id: i64,
    pub raw_grade: RawGrade,
    pub suggested_grade: Option<RawGrade>,
    pub typed_answer: Option<String>,
    pub answer_match: Option<AnswerMatch>,
    pub hint_used: bool,
    pub duration_ms: Option<u32>,
    pub client_ts: Option<String>,
}

/// Longest accepted typed answer.
const TYPED_ANSWER_MAX: usize = 300;

/// The suggested grades that agree with how a typed answer compared
/// (design §6.8): the target span of the sentence → 2 (3 after the hint);
/// the right word in another form or an accepted answer that is not the
/// sentence's own (also stored as `exact`) → 3; no match → 4; "I don't
/// know" → 5; "I'll just rate myself" → none.
fn suggestions_for(answer: AnswerMatch, hint_used: bool) -> &'static [u8] {
    match answer {
        AnswerMatch::Exact if hint_used => &[3],
        AnswerMatch::Exact => &[2, 3],
        AnswerMatch::None => &[4],
        AnswerMatch::DontKnow => &[5],
        AnswerMatch::Skipped => &[],
    }
}

/// Is `text` a UUID in canonical form (8-4-4-4-12 hex digits, any case)?
fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, len)| g.len() == len && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

impl RateInput {
    /// Check the input; returns the client review id in its stored form
    /// (lowercase UUID, G10).
    fn validate(&self) -> Result<String, SessionError> {
        if !is_uuid(&self.client_review_id) {
            return Err(SessionError::Invalid(
                "client_review_id must be a UUID (8-4-4-4-12 hex digits)".to_owned(),
            ));
        }
        if let Some(typed) = self.typed_answer.as_deref() {
            if typed.chars().count() > TYPED_ANSWER_MAX {
                return Err(SessionError::Invalid(format!(
                    "typed_answer is longer than {TYPED_ANSWER_MAX} characters"
                )));
            }
            // Blank also when only invisible characters (a zero-width space,
            // a byte-order mark) were typed: the pack's own notion of them.
            if typed
                .chars()
                .all(|c| c.is_whitespace() || vocab_pack::keys::is_invisible(c))
                || typed.chars().any(char::is_control)
            {
                return Err(SessionError::Invalid(
                    "typed_answer must be text without control characters, not blank".to_owned(),
                ));
            }
        }
        // What was typed goes with how it compared (design §6.8): `exact` and
        // `none` judge a typed answer; "I don't know" and "I'll just rate
        // myself" have none.
        let typed = self.typed_answer.is_some();
        match self.answer_match {
            Some(AnswerMatch::Exact | AnswerMatch::None) if !typed => {
                return Err(SessionError::Invalid(
                    "answer_match \"exact\" and \"none\" need the typed_answer".to_owned(),
                ));
            }
            Some(AnswerMatch::Skipped | AnswerMatch::DontKnow) if typed => {
                return Err(SessionError::Invalid(
                    "answer_match \"skipped\" and \"dont_know\" come without a typed_answer"
                        .to_owned(),
                ));
            }
            None if typed => {
                return Err(SessionError::Invalid(
                    "typed_answer needs answer_match".to_owned(),
                ));
            }
            _ => {}
        }
        if let (Some(answer), Some(suggested)) = (self.answer_match, self.suggested_grade)
            && !suggestions_for(answer, self.hint_used).contains(&suggested.get())
        {
            return Err(SessionError::Invalid(
                "suggested_grade does not agree with answer_match and hint_used (design §6.8)"
                    .to_owned(),
            ));
        }
        if let Some(ts) = &self.client_ts
            && ts.parse::<crate::clock::Timestamp>().is_err()
        {
            return Err(SessionError::Invalid(
                "client_ts must be an RFC 3339 timestamp".to_owned(),
            ));
        }
        Ok(self.client_review_id.to_ascii_lowercase())
    }

    /// Check the answer details against the kind of card rated: only typed
    /// cards (Produce, later Spelling) have a typed answer, a match, a hint
    /// or a suggested grade, and a typed card always says how it was
    /// answered (`answer_match`).
    fn check_for(&self, kind: CardKind) -> Result<(), SessionError> {
        let details = self.typed_answer.is_some()
            || self.answer_match.is_some()
            || self.hint_used
            || self.suggested_grade.is_some();
        match kind {
            CardKind::Understand if details => Err(SessionError::Invalid(
                "typed_answer, answer_match, hint_used and suggested_grade are only for typed \
                 cards"
                    .to_owned(),
            )),
            CardKind::Produce | CardKind::Spelling if self.answer_match.is_none() => Err(
                SessionError::Invalid("a typed card needs answer_match".to_owned()),
            ),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Finished,
    Expired,
}

impl SessionStatus {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "active" => Some(Self::Active),
            "finished" => Some(Self::Finished),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// Why an item is in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemOrigin {
    Due,
    New,
    Repeat,
}

impl ItemOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Due => "due",
            Self::New => "new",
            Self::Repeat => "repeat",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "due" => Some(Self::Due),
            "new" => Some(Self::New),
            "repeat" => Some(Self::Repeat),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("practice session not found")]
    NotFound,
    #[error("unknown source")]
    UnknownSource,
    #[error("the block belongs to an earlier study day and has expired")]
    Expired,
    #[error("the block is already {0}")]
    NotActive(&'static str),
    #[error("item not found in this block")]
    ItemNotFound,
    #[error("this item was already rated")]
    AlreadyAnswered,
    #[error("rate the items in order; this is not the next item")]
    NotNextItem,
    #[error("client_review_id was already used for another rating")]
    ReviewIdReused,
    #[error("this rating was undone; send a new client_review_id")]
    ReviewUndone,
    #[error("nothing to undo in this block")]
    NothingToUndo,
    /// The card of the last rating was changed since (reset in word
    /// detail): restoring the state before the rating would undo that too.
    #[error("the last rating cannot be undone: its card was reset since")]
    CardChangedSince,
    /// A problem-word choice was made after the last rating (design §6.5):
    /// its Undo comes first.
    #[error(
        "You chose what to do with this problem word after this rating. Undo that choice first, then the rating."
    )]
    LeechChoiceSince,
    #[error("{0}")]
    Invalid(String),
    #[error("random source failed: {0}")]
    Random(String),
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}
