//! The Progress screen (design §7.1 screen 9; R-72, R-55): what is due in
//! the coming days, how far each source is, how well the owner actually
//! remembers compared with what the scheduler aims at, what he did in the
//! last 30 days, his problem words and the pack suggestions waiting for an
//! answer. Read-only, built from the `catalog`, `scheduler`, `session` and
//! `settings` read functions with one query per list (never one per word).
//!
//! No streaks, scores or goals (R-74): only counts the owner can act on.

use std::collections::{BTreeMap, HashMap};

use rusqlite::Connection;
use serde::Serialize;

use super::{ForecastDay, StatsError, count, forecast, words};
use crate::catalog::{self, SourceId, WordId, WordQuery};
use crate::clock::StudyDay;
use crate::scheduler::{self, Card, CardKind, DayActivity, LeechReason};
use crate::session;
use crate::settings;

/// Days of the Progress forecast (the first 7 are Today's).
pub const PROGRESS_FORECAST_DAYS: i32 = 14;
/// Days of the retention measurement and the activity strip (today
/// included).
pub const WINDOW_DAYS: i32 = 30;
/// Fewer recall tests than this in the window: the measured retention is
/// shown, but called too uncertain to compare with the target.
pub const FEW_TESTS: u32 = 20;

/// Words by learning status. The six statuses partition the words:
/// `new + learning + familiar + known + problem + suspended = words`.
/// `can_produce` counts across them (words whose Produce card is
/// remembered for a week or more).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StatusCounts {
    pub words: u32,
    /// Never practised.
    pub new: u32,
    /// Failed last time, or remembered for less than a week.
    pub learning: u32,
    /// Remembered for one to three weeks.
    pub familiar: u32,
    /// Remembered for three weeks or more.
    pub known: u32,
    /// Problem words (R-55).
    pub problem: u32,
    /// Suspended by the owner (not practised).
    pub suspended: u32,
    /// The owner can produce the word: its Produce card is remembered for
    /// a week or more.
    pub can_produce: u32,
}

impl StatusCounts {
    fn add(&mut self, status: WordState, can_produce: bool) {
        self.words += 1;
        match status {
            WordState::Suspended => self.suspended += 1,
            WordState::Status(words::WordStatus::New) => self.new += 1,
            WordState::Status(words::WordStatus::Learning) => self.learning += 1,
            WordState::Status(words::WordStatus::Familiar) => self.familiar += 1,
            WordState::Status(words::WordStatus::Known) => self.known += 1,
            WordState::Status(words::WordStatus::Leech) => self.problem += 1,
        }
        if can_produce {
            self.can_produce += 1;
        }
    }
}

/// A word's place in the counts: a suspended word is counted as suspended
/// whatever its cards say.
#[derive(Debug, Clone, Copy)]
enum WordState {
    Suspended,
    Status(words::WordStatus),
}

/// One source with its words by status and its reading position (R-59).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceProgress {
    pub id: SourceId,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub reading_position: Option<String>,
    /// The export date of the pack that brought the reading position.
    pub reading_position_exported_at: Option<String>,
    #[serde(flatten)]
    pub counts: StatusCounts,
}

/// Measured retention of one card kind over the window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct KindRetention {
    pub kind: CardKind,
    /// Recall tests: reviews of a card on a later day than its previous
    /// review (the card's first review and same-day repeats test nothing).
    pub tested: u32,
    /// Of those, rated 1–3 (remembered).
    pub passed: u32,
    /// `passed / tested` (`None` without tests).
    pub measured: Option<f64>,
    /// The retention the scheduler aimed at for these tests: the mean of
    /// each tested card's desired retention (design §6.2), weighted by its
    /// tests (`None` without tests).
    pub target: Option<f64>,
    /// Fewer than [`FEW_TESTS`] tests: too few to compare.
    pub few: bool,
}

/// Retention per card kind over the window `from..=to`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RetentionView {
    pub from: StudyDay,
    pub to: StudyDay,
    /// Understand, then Produce, then Spelling (a kind without tests and
    /// without cards is left out).
    pub kinds: Vec<KindRetention>,
}

/// The activity strip: every day of the window, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActivityView {
    pub days: Vec<DayActivity>,
    pub reviews: u32,
    pub new_words: u32,
}

/// One problem card of a problem word.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LeechCard {
    pub kind: CardKind,
    pub reason: LeechReason,
    /// Lapses in all (failed on that many study days).
    pub lapses: u32,
    /// Days the card is remembered for now (`None` while new).
    pub stability: Option<f64>,
    pub last_review_day: Option<StudyDay>,
}

/// A problem word (R-55) with why it is one and what can be done.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LeechEntry {
    #[serde(flatten)]
    pub word: catalog::ProblemWord,
    /// `recognise` or `active` (design §6.3).
    pub goal: &'static str,
    /// The leech screen is due (a card is flagged, not yet acknowledged).
    pub flagged: bool,
    pub cards: Vec<LeechCard>,
}

/// A word with pack suggestions the owner has not answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConflictEntry {
    pub word_id: WordId,
    pub headword: String,
    pub open: i64,
}

/// The Progress view.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProgressView {
    pub study_day: StudyDay,
    /// Cards today's blocks still serve (as on Today, all sources).
    pub due_now: u32,
    /// Reviews due on each of the next 14 study days.
    pub forecast: Vec<ForecastDay>,
    /// Sum of the first 7 forecast days.
    pub due_next_7_days: u32,
    /// All words.
    pub totals: StatusCounts,
    /// Each source (a word of two sources counts in both).
    pub sources: Vec<SourceProgress>,
    pub retention: RetentionView,
    pub activity: ActivityView,
    /// Problem words that are not suspended, flagged ones first.
    pub leeches: Vec<LeechEntry>,
    /// Words with open pack suggestions.
    pub conflicts: Vec<ConflictEntry>,
}

/// The Progress view on study day `day`.
pub fn progress(conn: &Connection, day: StudyDay) -> Result<ProgressView, StatsError> {
    let settings = settings::get(conn)?;
    let due_now = session::due_today(conn, day, None, &settings)?.due;
    let forecast = forecast(
        &session::reviews_in_scope(conn, None)?,
        day,
        PROGRESS_FORECAST_DAYS,
    );
    let due_next_7_days = forecast.iter().take(7).map(|f| f.due).sum();

    let rows = catalog::words(conn, &WordQuery::default())?;
    let cards = scheduler::all_cards(conn)?;
    let mut by_word: HashMap<WordId, Vec<&Card>> = HashMap::new();
    for card in &cards {
        by_word.entry(card.word_id).or_default().push(card);
    }
    let cards_of = |id: WordId| by_word.get(&id).map_or(&[][..], Vec::as_slice);
    let mut states: HashMap<WordId, (WordState, bool)> = HashMap::with_capacity(rows.len());
    let mut totals = StatusCounts::default();
    for row in &rows {
        let cards = cards_of(row.id);
        let state = if row.suspended {
            WordState::Suspended
        } else {
            WordState::Status(words::word_status(false, cards))
        };
        let produce = words::can_produce(cards);
        totals.add(state, produce);
        states.insert(row.id, (state, produce));
    }

    let mut per_source: HashMap<SourceId, StatusCounts> = HashMap::new();
    for (word, source) in catalog::word_source_links(conn)? {
        if let Some((state, produce)) = states.get(&word) {
            per_source.entry(source).or_default().add(*state, *produce);
        }
    }
    let sources = catalog::sources(conn)?
        .into_iter()
        .map(|s| SourceProgress {
            counts: per_source.get(&s.id).copied().unwrap_or_default(),
            id: s.id,
            title: s.title,
            kind: s.kind,
            reading_position: s.reading_position,
            reading_position_exported_at: s.reading_position_exported_at,
        })
        .collect();

    let retention = retention(conn, day, &cards)?;
    let activity = activity(conn, day)?;

    // Problem words: not suspended, with a problem card.
    let problem_ids: Vec<WordId> = rows
        .iter()
        .filter(|r| {
            matches!(
                states.get(&r.id),
                Some((WordState::Status(words::WordStatus::Leech), _))
            )
        })
        .map(|r| r.id)
        .collect();
    let traits = catalog::traits(conn, &problem_ids)?;
    let mut leeches: Vec<LeechEntry> = catalog::problem_words(conn, &problem_ids)?
        .into_iter()
        .map(|word| {
            let cards: Vec<LeechCard> = cards_of(word.id)
                .iter()
                .filter(|c| c.is_problem())
                .filter_map(|c| {
                    Some(LeechCard {
                        kind: c.kind,
                        reason: c.leech_reason()?,
                        lapses: c.lapses,
                        stability: c.stability,
                        last_review_day: c.last_review_day,
                    })
                })
                .collect();
            LeechEntry {
                goal: traits
                    .get(&word.id)
                    .map_or("recognise", |t| t.effective_goal().as_str()),
                flagged: cards
                    .iter()
                    .any(|c| !matches!(c.reason, LeechReason::Acknowledged)),
                cards,
                word,
            }
        })
        .collect();
    // Flagged first; otherwise the catalog's order (by headword).
    leeches.sort_by_key(|l| !l.flagged);

    let conflicts = rows
        .iter()
        .filter(|r| r.open_conflicts > 0)
        .map(|r| ConflictEntry {
            word_id: r.id,
            headword: r.headword.clone(),
            open: r.open_conflicts,
        })
        .collect::<Vec<_>>();
    let mut conflicts = conflicts;
    conflicts.sort_by(|a, b| {
        a.headword
            .to_lowercase()
            .cmp(&b.headword.to_lowercase())
            .then(a.word_id.cmp(&b.word_id))
    });

    Ok(ProgressView {
        study_day: day,
        due_now,
        forecast,
        due_next_7_days,
        totals,
        sources,
        retention,
        activity,
        leeches,
        conflicts,
    })
}

/// The first day of the window that ends with `day`.
fn window_start(day: StudyDay) -> StudyDay {
    day.add_days(-(WINDOW_DAYS - 1))
}

fn retention(
    conn: &Connection,
    day: StudyDay,
    cards: &[Card],
) -> Result<RetentionView, StatsError> {
    let from = window_start(day);
    let tallies = scheduler::recall_tallies(conn, from, day)?;
    let traits: HashMap<WordId, catalog::WordTraits> = catalog::all_traits(conn)?
        .into_iter()
        .map(|t| (t.id, t))
        .collect();
    // kind → (tested, passed, Σ target × tested)
    let mut sums: BTreeMap<u8, (CardKind, u32, u32, f64)> = BTreeMap::new();
    let order = |kind: CardKind| match kind {
        CardKind::Understand => 0,
        CardKind::Produce => 1,
        CardKind::Spelling => 2,
    };
    for kind in [CardKind::Understand, CardKind::Produce, CardKind::Spelling] {
        if cards.iter().any(|c| c.kind == kind) {
            sums.insert(order(kind), (kind, 0, 0, 0.0));
        }
    }
    for tally in &tallies {
        let target = traits.get(&tally.word_id).map_or(0.0, |t| {
            f64::from(scheduler::desired_retention(tally.kind, t))
        });
        let entry = sums
            .entry(order(tally.kind))
            .or_insert((tally.kind, 0, 0, 0.0));
        entry.1 += tally.tested;
        entry.2 += tally.passed;
        entry.3 += target * f64::from(tally.tested);
    }
    let kinds = sums
        .into_values()
        .map(|(kind, tested, passed, weighted)| KindRetention {
            kind,
            tested,
            passed,
            measured: (tested > 0).then(|| f64::from(passed) / f64::from(tested)),
            // f32 retention values: round to 3 places for a clean number.
            target: (tested > 0).then(|| (weighted / f64::from(tested) * 1000.0).round() / 1000.0),
            few: tested < FEW_TESTS,
        })
        .collect();
    Ok(RetentionView {
        from,
        to: day,
        kinds,
    })
}

fn activity(conn: &Connection, day: StudyDay) -> Result<ActivityView, StatsError> {
    let from = window_start(day);
    let active: HashMap<StudyDay, DayActivity> = scheduler::activity(conn, from, day)?
        .into_iter()
        .map(|a| (a.day, a))
        .collect();
    let days: Vec<DayActivity> = (0..WINDOW_DAYS)
        .map(|offset| {
            let d = from.add_days(offset);
            active.get(&d).copied().unwrap_or(DayActivity {
                day: d,
                reviews: 0,
                new_words: 0,
            })
        })
        .collect();
    Ok(ActivityView {
        reviews: days.iter().map(|d| d.reviews).sum(),
        new_words: days.iter().map(|d| d.new_words).sum(),
        days,
    })
}

/// How many words are problem words now, for Today's quiet link to
/// Progress (R-55).
pub fn problem_word_count(conn: &Connection) -> Result<u32, StatsError> {
    Ok(count(words::problem_word_ids(conn)?.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use words::WordStatus as S;

    #[test]
    fn r72_statuses_partition_the_words_and_produce_counts_across() {
        let mut c = StatusCounts::default();
        for (state, produce) in [
            (WordState::Status(S::New), false),
            (WordState::Status(S::Learning), false),
            (WordState::Status(S::Familiar), true),
            (WordState::Status(S::Known), true),
            (WordState::Status(S::Leech), false),
            (WordState::Suspended, true),
        ] {
            c.add(state, produce);
        }
        assert_eq!(
            c,
            StatusCounts {
                words: 6,
                new: 1,
                learning: 1,
                familiar: 1,
                known: 1,
                problem: 1,
                suspended: 1,
                can_produce: 3,
            }
        );
        assert_eq!(
            c.new + c.learning + c.familiar + c.known + c.problem + c.suspended,
            c.words
        );
    }

    #[test]
    fn r72_window_is_thirty_days_with_today() {
        let day: StudyDay = "2026-11-20".parse().expect("day");
        assert_eq!(window_start(day).to_string(), "2026-10-22");
        assert_eq!(
            i64::from(window_start(day).days_between(day)),
            i64::from(WINDOW_DAYS - 1)
        );
    }
}
