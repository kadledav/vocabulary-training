//! Read-only aggregations for the Today screen (design §3.2 `stats`, §7.1
//! screen 1; R-54, R-59) and the Words screens ([`words`]). Uses only the
//! read functions of `catalog`, `scheduler`, `session` and `settings`.

mod progress;
mod words;

pub use progress::{
    ActivityView, ConflictEntry, FEW_TESTS, KindRetention, LeechCard, LeechEntry,
    PROGRESS_FORECAST_DAYS, ProgressView, RetentionView, SourceProgress, StatusCounts, WINDOW_DAYS,
    problem_word_count, progress,
};
pub use words::{
    CardView, WordDetail, WordListQuery, WordListRow, WordSort, WordStatus, can_produce,
    problem_word_ids, word_detail, word_list, word_status,
};

use std::collections::HashMap;

use rusqlite::Connection;
use serde::Serialize;

use crate::catalog::{self, SourceId, SourceSummary};
use crate::clock::StudyDay;
use crate::session::{
    self, BlockKind, BlockPreview, FinishedBlocks, NewAllowance, SessionId, TypedAllowance,
};
use crate::settings;

/// Days of the Today forecast.
const FORECAST_DAYS: i32 = 7;

/// Due cards on one future study day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForecastDay {
    pub day: StudyDay,
    pub due: u32,
}

/// The unfinished block of today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActiveBlock {
    pub id: SessionId,
    /// `regular`, `extra` or `more_new`.
    pub kind: BlockKind,
    pub scope_source_id: Option<SourceId>,
    pub block_no: i64,
    pub total: u32,
    pub remaining: u32,
}

/// The Today view. Counts cover the card kinds the session builder serves
/// and words that are not suspended; `due_now` and `forecast` are limited
/// to `scope` (R-59), the new-word allowance is global (D-11).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TodayView {
    pub study_day: StudyDay,
    pub scope_source_id: Option<SourceId>,
    /// Cards today's blocks still serve as due in the scope: due reviews
    /// (one per word, none of a word that already had a card today) plus
    /// the first-time Produce/Spelling cards today's cap lets in — exactly
    /// what the block builder serves.
    pub due_now: u32,
    /// Of those, cards rated 4/5 earlier (repeated first).
    pub relearning: u32,
    /// Due cards on each of the next 7 study days (in the scope).
    pub forecast: Vec<ForecastDay>,
    /// New words that could be introduced in the scope (not low-priority
    /// unless included).
    pub new_available: u32,
    pub new_words: NewAllowance,
    /// Unseen words waiting in the scope (same count as `new_available`:
    /// not low-priority unless included, word not suspended).
    pub new_waiting: u32,
    /// What is left of today's new-word limit (0 while paused).
    pub new_left_today: u32,
    /// Why new words are paused: `null` or `"backlog"` (design §6.6).
    pub new_paused_reason: Option<PauseReason>,
    /// Today's cap on first-time Produce/Spelling cards (design §17 M8).
    pub new_typed: TypedAllowance,
    /// Produce/Spelling cards in the scope that wait for their first
    /// review and could come today (beyond the cap they wait for a later
    /// day).
    pub typed_waiting: u32,
    /// Of `due_now`, the first-time Produce/Spelling cards that come today
    /// (within the cap, not blocked by a review of the same word).
    pub typed_today: u32,
    /// What a new regular block would hold now (an active block is
    /// resumed instead).
    pub next_block: BlockPreview,
    /// Cards an extra-practice block (R-63) would serve now in the scope
    /// (at most a block).
    pub extra_available: u32,
    /// Cards due on the next study day in the scope (= `forecast[0].due`).
    pub due_tomorrow: u32,
    pub active_session: Option<ActiveBlock>,
    /// Blocks finished in all and today (rating legend rule, "Continue").
    pub finished_blocks: FinishedBlocks,
    /// All sources with their reading position (scope chooser).
    pub sources: Vec<SourceSummary>,
    /// Problem words (R-55, all sources; suspended words left out): Today
    /// links to their list on Progress, without nagging.
    pub problem_words: u32,
}

/// Why new words are paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// More cards due than twice the normal daily load.
    Backlog,
}

/// Reviews due on each of the `days` study days after `day` among
/// `scheduled` (one count per day: the cards whose due day it is).
fn forecast(scheduled: &[crate::scheduler::Card], day: StudyDay, days: i32) -> Vec<ForecastDay> {
    let mut due: HashMap<StudyDay, u32> = HashMap::new();
    for card in scheduled {
        if let Some(d) = card.due_day {
            *due.entry(d).or_default() += 1;
        }
    }
    (1..=days)
        .map(|offset| {
            let target = day.add_days(offset);
            ForecastDay {
                day: target,
                due: due.get(&target).copied().unwrap_or(0),
            }
        })
        .collect()
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[derive(Debug, thiserror::Error)]
pub enum StatsError {
    #[error("unknown source")]
    UnknownSource,
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Today in `scope` (`None` = all sources).
pub fn today(
    conn: &Connection,
    day: StudyDay,
    scope: Option<SourceId>,
) -> Result<TodayView, StatsError> {
    if let Some(source) = scope
        && !catalog::source_exists(conn, source)?
    {
        return Err(StatsError::UnknownSource);
    }
    // One implementation each: the session module decides which cards and
    // candidates a scope has (G9).
    let forecast = forecast(&session::reviews_in_scope(conn, scope)?, day, FORECAST_DAYS);
    let new_available = session::new_candidate_count(conn, scope)?;
    let settings = settings::get(conn)?;
    let active_session = session::current(conn, day)?.map(|s| ActiveBlock {
        id: s.id,
        kind: s.kind,
        scope_source_id: s.scope_source_id,
        block_no: s.block_no,
        total: s.total,
        remaining: s.remaining,
    });
    let new_words = session::new_word_allowance(conn, day, &settings)?;
    let new_typed = session::typed_allowance(conn, day, &settings, &new_words)?;
    let typed_waiting = session::typed_waiting(conn, day, scope)?;
    let due = session::due_today(conn, day, scope, &settings)?;
    let extra_available = session::extra_available(conn, day, scope, &settings)?;
    let due_tomorrow = forecast.first().map_or(0, |f| f.due);
    Ok(TodayView {
        study_day: day,
        scope_source_id: scope,
        due_now: due.due,
        relearning: due.relearning,
        forecast,
        new_available,
        new_waiting: new_available,
        new_left_today: new_words.quota,
        new_paused_reason: new_words.paused.then_some(PauseReason::Backlog),
        new_typed,
        typed_waiting,
        typed_today: due.first_time,
        next_block: due.next_block,
        extra_available,
        due_tomorrow,
        new_words,
        active_session,
        finished_blocks: session::finished_blocks(conn, day)?,
        sources: catalog::sources(conn)?,
        problem_words: problem_word_count(conn)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::WordId;
    use crate::scheduler::{Card, CardId, CardKind, CardState, LeechState};

    fn due(id: i64, day: Option<&str>) -> Card {
        Card {
            id: CardId::new(id),
            word_id: WordId::new(id),
            kind: CardKind::Understand,
            format: None,
            state: CardState::Review,
            stability: Some(3.0),
            difficulty: Some(5.0),
            due_day: day.map(|d| d.parse().expect("day")),
            last_review_day: None,
            last_review_at: None,
            introduced_day: None,
            reps: 1,
            lapses: 0,
            consecutive_fives: 0,
            leech: LeechState::None,
            leech_lapses_at_ack: 0,
            suspended: false,
            leech_acked_after: None,
        }
    }

    #[test]
    fn r72_forecast_counts_each_following_day_only() {
        let cards = [
            due(1, Some("2026-11-20")), // today: not in the forecast
            due(2, Some("2026-11-21")),
            due(3, Some("2026-11-21")),
            due(4, Some("2026-11-27")),
            due(5, Some("2026-11-28")), // day 8: beyond 7 days
            due(6, None),
        ];
        let day: StudyDay = "2026-11-20".parse().expect("day");
        let week = forecast(&cards, day, 7);
        assert_eq!(week.len(), 7);
        assert_eq!(week[0].day.to_string(), "2026-11-21");
        assert_eq!(
            week.iter().map(|f| f.due).collect::<Vec<_>>(),
            [2, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(forecast(&cards, day, 14)[7].due, 1);
    }
}
