//! Read-only aggregations for the Today screen (design §3.2 `stats`, §7.1
//! screen 1; R-54, R-59). Uses only the read functions of `catalog`,
//! `scheduler`, `session` and `settings`.

use rusqlite::Connection;
use serde::Serialize;

use crate::catalog::{self, SourceId, SourceSummary};
use crate::clock::StudyDay;
use crate::scheduler::{self, CardState};
use crate::session::{self, BlockKind, FinishedBlocks, NewAllowance, SessionId};
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
    /// Cards due today or earlier in the scope.
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
}

/// Why new words are paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    /// More cards due than twice the normal daily load.
    Backlog,
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
    let scheduled = session::scheduled_in_scope(conn, scope)?;
    let due: Vec<&scheduler::Card> = scheduled.iter().filter(|c| c.is_due_on(day)).collect();
    let forecast: Vec<ForecastDay> = (1..=FORECAST_DAYS)
        .map(|offset| {
            let target = day.add_days(offset);
            ForecastDay {
                day: target,
                due: count(
                    scheduled
                        .iter()
                        .filter(|c| c.due_day == Some(target))
                        .count(),
                ),
            }
        })
        .collect();
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
    let extra_available = session::extra_available(conn, day, scope, &settings)?;
    let due_tomorrow = forecast.first().map_or(0, |f| f.due);
    Ok(TodayView {
        study_day: day,
        scope_source_id: scope,
        due_now: count(due.len()),
        relearning: count(
            due.iter()
                .filter(|c| c.state == CardState::Relearning)
                .count(),
        ),
        forecast,
        new_available,
        new_waiting: new_available,
        new_left_today: new_words.quota,
        new_paused_reason: new_words.paused.then_some(PauseReason::Backlog),
        extra_available,
        due_tomorrow,
        new_words,
        active_session,
        finished_blocks: session::finished_blocks(conn, day)?,
        sources: catalog::sources(conn)?,
    })
}
