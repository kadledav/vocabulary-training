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
//! ([`SERVED_KINDS`]: Understand until M8/M10). Produce and Spelling cards
//! are still created and unlocked by the scheduler; they wait (stay due)
//! until their kind is served.

mod build;
mod queue;
mod view;

use std::fmt;

use serde::{Deserialize, Serialize};

pub use build::{
    NewAllowance, choose_sentence, new_candidate_count, new_word_allowance, scheduled_in_scope,
    start_block,
};
pub use queue::{finish, rate, undo_last};
pub use view::{
    AnswerView, BlockSummary, ItemCard, ItemView, LookAgain, RateResult, SessionView, StartResult,
    UndoResult, current,
};

use crate::catalog::SourceId;
use crate::scheduler::{AnswerMatch, CardKind, RawGrade, SchedulerError};

/// Card kinds the session builder serves (M3: Understand only).
pub const SERVED_KINDS: &[CardKind] = &[CardKind::Understand];

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

/// `start` (first block) or `continue` (Continue +20); both build a block
/// the same way (design §6.7 step 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartKind {
    Start,
    Continue,
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
        if self
            .typed_answer
            .as_deref()
            .is_some_and(|t| t.chars().count() > TYPED_ANSWER_MAX)
        {
            return Err(SessionError::Invalid(format!(
                "typed_answer is longer than {TYPED_ANSWER_MAX} characters"
            )));
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
    #[error("{0}")]
    Invalid(String),
    #[error("random source failed: {0}")]
    Random(String),
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}
