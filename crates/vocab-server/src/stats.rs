//! Read-only aggregations for the Today screen (design §3.2 `stats`, §7.1
//! screen 1; R-54, R-59). Uses only the read functions of `catalog`,
//! `scheduler`, `session` and `settings`.

use rusqlite::Connection;
use serde::Serialize;

use crate::catalog::{self, SourceId, SourceSummary};
use crate::clock::StudyDay;
use crate::scheduler::{self, CardState};
use crate::session::{self, FinishedBlocks, NewAllowance, SessionId};
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
    pub active_session: Option<ActiveBlock>,
    /// Blocks finished in all and today (rating legend rule, "Continue").
    pub finished_blocks: FinishedBlocks,
    /// All sources with their reading position (scope chooser).
    pub sources: Vec<SourceSummary>,
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
    let forecast = (1..=FORECAST_DAYS)
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
        scope_source_id: s.scope_source_id,
        block_no: s.block_no,
        total: s.total,
        remaining: s.remaining,
    });
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
        new_words: session::new_word_allowance(conn, day, &settings)?,
        active_session,
        finished_blocks: session::finished_blocks(conn, day)?,
        sources: catalog::sources(conn)?,
    })
}
