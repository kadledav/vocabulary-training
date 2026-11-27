//! Recording and undoing reviews (design §6.1, §6.4, §6.5, §6.7.9; R-55,
//! R-60, R-61).
//!
//! Every review stores the raw 1–5 grade and the FSRS rating applied
//! (R-61), and a snapshot of the card before the review plus the ids of the
//! cards it unlocked, so undo restores the card exactly and removes those
//! cards. Undone reviews stay in the log (`undone = 1`) and are ignored
//! everywhere else.
//!
//! One bad day counts once (R-50, R-54, R-55): per card and study day, only
//! the first failing rating (4/5) lowers the FSRS memory state and only it
//! can be a lapse. Further failing ratings of that day are logged with
//! their raw grade (`memory_updated = 0`) and leave the card in
//! `relearning`, due today, with its memory state untouched.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use super::cards::{self, Card, Snapshot};
use super::model::{Rating, desired_retention, model};
use super::{CardFormat, CardId, CardKind, CardState, LeechState, ReviewId, SchedulerError};
use crate::catalog::WordTraits;
use crate::clock::{Moment, StudyDay};

/// Lapses since the last acknowledgement that flag a leech (R-55).
const LEECH_LAPSES: u32 = 4;
/// "Previously mature": stability before the first of two 5s (design §6.5).
const LEECH_MATURE_STABILITY: f64 = 21.0;

/// Why a card is a problem card (design §6.5), as the Progress screen and
/// the leech screen explain it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeechReason {
    /// Failed on `lapses` different study days since it was last
    /// acknowledged (4 or more flag it).
    Lapses { lapses: u32 },
    /// Rated 5 (No idea) on two study days in a row although it had been
    /// remembered for three weeks or more before.
    ForgotWhenKnown,
    /// Flagged before; the owner chose what to do on its leech screen.
    /// It stays a problem card until it is remembered for three weeks
    /// (stability ≥ 21 days), and four further lapses flag it again.
    Acknowledged,
}

impl Card {
    /// Why this card is a problem card (`None`: it is not one).
    pub fn leech_reason(&self) -> Option<LeechReason> {
        match self.leech {
            LeechState::None => None,
            LeechState::Acknowledged => Some(LeechReason::Acknowledged),
            LeechState::Flagged => {
                let lapses = self.lapses.saturating_sub(self.leech_lapses_at_ack);
                Some(if lapses >= LEECH_LAPSES {
                    LeechReason::Lapses { lapses }
                } else {
                    LeechReason::ForgotWhenKnown
                })
            }
        }
    }

    /// Is the card still a problem card (design §6.5: a flagged card, or an
    /// acknowledged one not yet remembered for three weeks)? A suspended
    /// card is never one: it is not practised (the owner's choice, a goal
    /// or the spelling switch), so it can neither fail nor recover; it
    /// becomes one again only if it is practised again.
    pub fn is_problem(&self) -> bool {
        if self.suspended {
            return false;
        }
        match self.leech {
            LeechState::None => false,
            LeechState::Flagged => true,
            LeechState::Acknowledged => self.stability.unwrap_or(0.0) < LEECH_MATURE_STABILITY,
        }
    }
}

/// The owner's grade, 1 (Instantly) … 5 (No idea) (R-60).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct RawGrade(u8);

impl RawGrade {
    pub fn new(grade: u8) -> Option<Self> {
        (1..=5).contains(&grade).then_some(Self(grade))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// 4 (Vague) or 5 (No idea): below the pass/fail line.
    pub fn failed(self) -> bool {
        self.0 >= 4
    }
}

/// How a typed answer compared (Produce/Spelling; graded by the client).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerMatch {
    Exact,
    None,
    Skipped,
    DontKnow,
}

impl AnswerMatch {
    fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::None => "none",
            Self::Skipped => "skipped",
            Self::DontKnow => "dont_know",
        }
    }
}

/// One rating to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewInput<'a> {
    /// Client-generated UUID; the review log's idempotency key.
    pub client_review_id: &'a str,
    pub card_id: CardId,
    pub session_id: Option<&'a str>,
    pub raw_grade: RawGrade,
    pub suggested_grade: Option<RawGrade>,
    pub typed_answer: Option<&'a str>,
    pub answer_match: Option<AnswerMatch>,
    pub hint_used: bool,
    pub duration_ms: Option<u32>,
    /// Client clock (RFC 3339), kept for offline use later.
    pub client_ts: Option<&'a str>,
    /// The sentence shown with the card.
    pub sentence_id: Option<i64>,
    /// The format the card was shown in (stored with the practice item when
    /// the block was built, G2). It becomes the card's format at its first
    /// review; `None` = decide from the word's traits now.
    pub format: Option<CardFormat>,
}

/// A card created by a review's unlock check.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnlockedCard {
    pub card_id: CardId,
    pub kind: CardKind,
    pub due_day: Option<StudyDay>,
}

/// What a review did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewOutcome {
    pub review_id: ReviewId,
    pub raw_grade: RawGrade,
    /// FSRS rating applied, 1 (Again) … 4 (Easy).
    pub fsrs_rating: u8,
    /// The card now.
    pub card: Card,
    /// The card is flagged as a leech (R-55).
    pub leech_flagged: bool,
    /// Cards this review unlocked (still existing).
    pub unlocked: Vec<UnlockedCard>,
    pub undone: bool,
}

/// What undo did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UndoOutcome {
    pub review_id: ReviewId,
    /// The restored card.
    pub card: Card,
    /// Unlocked cards that were removed.
    pub removed_cards: Vec<CardId>,
}

/// A logged review found by its client id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReview {
    pub review_id: ReviewId,
    pub card_id: CardId,
    pub session_id: Option<String>,
    pub undone: bool,
}

/// The review recorded under `client_review_id`, if any (idempotency).
pub fn find_review(
    conn: &Connection,
    client_review_id: &str,
) -> rusqlite::Result<Option<StoredReview>> {
    conn.query_row(
        "SELECT id, card_id, session_id, undone FROM review_log WHERE client_review_id = ?1",
        [client_review_id],
        |row| {
            Ok(StoredReview {
                review_id: ReviewId(row.get(0)?),
                card_id: CardId::new(row.get(1)?),
                session_id: row.get(2)?,
                undone: row.get(3)?,
            })
        },
    )
    .optional()
}

/// The raw grade of a non-undone review (`None` if missing or undone).
pub fn raw_grade(conn: &Connection, id: ReviewId) -> rusqlite::Result<Option<RawGrade>> {
    let grade: Option<u8> = conn
        .query_row(
            "SELECT raw_grade FROM review_log WHERE id = ?1 AND undone = 0",
            [id.0],
            |row| row.get(0),
        )
        .optional()?;
    Ok(grade.and_then(RawGrade::new))
}

fn created_ids(json: &str) -> Vec<CardId> {
    serde_json::from_str::<Vec<i64>>(json)
        .unwrap_or_default()
        .into_iter()
        .map(CardId::new)
        .collect()
}

/// The outcome of a logged review as it stands now (replayed for a
/// repeated post of the same client id).
pub fn review_outcome(conn: &Connection, id: ReviewId) -> rusqlite::Result<Option<ReviewOutcome>> {
    let row = conn
        .query_row(
            "SELECT card_id, raw_grade, fsrs_rating, created_card_ids, undone FROM review_log WHERE id = ?1",
            [id.0],
            |row| {
                Ok((
                    CardId::new(row.get(0)?),
                    row.get::<_, u8>(1)?,
                    row.get::<_, u8>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((card_id, raw, rating, created, undone)) = row else {
        return Ok(None);
    };
    let Some(card) = cards::card(conn, card_id)? else {
        return Ok(None);
    };
    let mut unlocked = Vec::new();
    for id in created_ids(&created) {
        if let Some(c) = cards::card(conn, id)? {
            unlocked.push(UnlockedCard {
                card_id: c.id,
                kind: c.kind,
                due_day: c.due_day,
            });
        }
    }
    Ok(Some(ReviewOutcome {
        review_id: id,
        raw_grade: RawGrade(raw),
        fsrs_rating: rating,
        leech_flagged: card.leech == LeechState::Flagged,
        card,
        unlocked,
        undone,
    }))
}

/// Did the card already get a failing rating (4/5, not undone) on `day`?
fn failed_on(tx: &Connection, card: CardId, day: StudyDay) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM review_log WHERE card_id = ?1 AND undone = 0 \
         AND review_day = ?2 AND raw_grade >= 4)",
        params![card.get(), day],
        |row| row.get(0),
    )
}

/// Two-5s leech rule (design §6.5, guarantor decision G5): the 5s must be on
/// different study days, like lapses; same-day repeats do not count. This
/// is the stability before the first review of the card's previous review
/// day, if that review was a 5; `None` otherwise.
fn previous_day_five_stability(
    tx: &Connection,
    card: CardId,
    previous_day: StudyDay,
) -> rusqlite::Result<Option<f64>> {
    let first: Option<(u8, String)> = tx
        .query_row(
            "SELECT raw_grade, before_json FROM review_log WHERE card_id = ?1 AND undone = 0 \
             AND review_day = ?2 ORDER BY id LIMIT 1",
            params![card.get(), previous_day],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(match first {
        Some((5, before)) => serde_json::from_str::<Snapshot>(&before)
            .ok()
            .and_then(|s| s.stability()),
        _ => None,
    })
}

/// Record one rating: FSRS update (design §6.1), state and due day, lapse
/// and leech bookkeeping (§6.5), the review log row (R-61) and, after an
/// Understand review, the Produce/Spelling unlock check (§6.4).
///
/// Rated 4/5 (Again) → `relearning`, due today (repeated in the session);
/// rated 1–3 → `review`, due after the FSRS interval. A second failing
/// rating on the same study day changes neither the memory state nor the
/// lapse count (module docs).
pub fn record_review(
    tx: &Transaction<'_>,
    input: &ReviewInput<'_>,
    word: &WordTraits,
    now: &Moment,
) -> Result<ReviewOutcome, SchedulerError> {
    let before =
        cards::card(tx, input.card_id)?.ok_or(SchedulerError::CardNotFound(input.card_id.get()))?;
    let model = model(tx)?;
    let raw = input.raw_grade;
    let rating = model.rating(raw);
    let day = now.day;
    // Same-day reviews have 0 elapsed days (FSRS-6 models them).
    let elapsed = before
        .last_review_day
        .map_or(0, |last| last.days_between(day).max(0).unsigned_abs());
    let retention = desired_retention(before.kind, word);
    // The first failure of a study day is the one that counts.
    let first_failure_today = raw.failed() && !failed_on(tx, before.id, day)?;
    let memory_updated = !raw.failed() || first_failure_today || before.memory().is_none();
    let (stability, difficulty, interval) = if memory_updated {
        let (memory, interval) = model.next(before.memory(), retention, elapsed, rating)?;
        (
            Some(f64::from(memory.stability)),
            Some(f64::from(memory.difficulty)),
            interval,
        )
    } else {
        // Failed again today: nothing more to learn from it (the interval
        // is unused, a failed card is due today).
        (before.stability, before.difficulty, 0)
    };

    let mut after = before.clone();
    let shown = input
        .format
        .or(before.format)
        .unwrap_or_else(|| CardFormat::for_first_review(before.kind, word));
    // Fixed at the first review (D-2): the format that was shown.
    after.format = before.format.or(Some(shown));
    after.stability = stability;
    after.difficulty = difficulty;
    if rating == Rating::Again {
        after.state = CardState::Relearning;
        after.due_day = Some(day);
    } else {
        after.state = CardState::Review;
        after.due_day = Some(day.add_days(interval));
    }
    // A lapse is the first failing review of a study day of a card that is
    // not new (design §6.5): at most one per card and day, and also when
    // the card is still `relearning` from an earlier day.
    if first_failure_today && before.state != CardState::New {
        after.lapses += 1;
    }
    after.consecutive_fives = if raw.get() == 5 {
        before.consecutive_fives + 1
    } else {
        0
    };
    after.last_review_day = Some(day);
    after.last_review_at = Some(now.stamp());
    // The first review introduces the card (also after "Reset card": a
    // card reset on an earlier day counts as introduced again today).
    after.introduced_day = if before.reps == 0 {
        Some(day)
    } else {
        before.introduced_day.or(Some(day))
    };
    after.reps += 1;
    if after.leech != LeechState::Flagged {
        let lapses_since_ack = after.lapses.saturating_sub(after.leech_lapses_at_ack);
        let first_review_today = before.last_review_day.is_some_and(|d| d < day);
        let two_fives_on_mature = raw.get() == 5
            && first_review_today
            && match before.last_review_day {
                Some(previous) => previous_day_five_stability(tx, before.id, previous)?
                    .is_some_and(|s| s >= LEECH_MATURE_STABILITY),
                None => false,
            };
        if lapses_since_ack >= LEECH_LAPSES || two_fives_on_mature {
            after.leech = LeechState::Flagged;
        }
    }
    cards::save(tx, &after)?;

    let before_json = serde_json::to_string(&Snapshot::of(&before))
        .map_err(|e| SchedulerError::Corrupt(format!("snapshot: {e}")))?;
    let format = shown;
    tx.execute(
        "INSERT INTO review_log (client_review_id, card_id, session_id, reviewed_at, client_ts, \
         review_day, raw_grade, fsrs_rating, suggested_grade, format, sentence_id, typed_answer, \
         answer_match, hint_used, elapsed_days, duration_ms, before_json, memory_updated) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            input.client_review_id,
            before.id.get(),
            input.session_id,
            now.stamp(),
            input.client_ts,
            day,
            raw.get(),
            rating.number(),
            input.suggested_grade.map(RawGrade::get),
            format.as_str(),
            input.sentence_id,
            input.typed_answer,
            input.answer_match.map(AnswerMatch::as_str),
            input.hint_used,
            elapsed,
            input.duration_ms,
            before_json,
            memory_updated,
        ],
    )?;
    let review_id = ReviewId(tx.last_insert_rowid());

    let created = if before.kind == CardKind::Understand {
        cards::unlock_cards(tx, word, now)?
    } else {
        Vec::new()
    };
    if !created.is_empty() {
        let ids: Vec<i64> = created.iter().map(|c| c.get()).collect();
        tx.execute(
            "UPDATE review_log SET created_card_ids = ?2 WHERE id = ?1",
            params![review_id.0, serde_json::Value::from(ids).to_string()],
        )?;
    }
    let mut unlocked = Vec::new();
    for id in created {
        if let Some(c) = cards::card(tx, id)? {
            unlocked.push(UnlockedCard {
                card_id: c.id,
                kind: c.kind,
                due_day: c.due_day,
            });
        }
    }
    Ok(ReviewOutcome {
        review_id,
        raw_grade: raw,
        fsrs_rating: rating.number(),
        leech_flagged: after.leech == LeechState::Flagged,
        card: after,
        unlocked,
        undone: false,
    })
}

/// Can review `id` be undone, as far as its card goes?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatingUndo {
    /// The card is as that rating left it.
    Possible,
    /// The card was changed since ("Reset card", or reviewed again):
    /// undoing would bring back the state the owner reset.
    CardChanged,
    /// A problem-word choice acknowledged the card after this rating
    /// (design §6.5): undoing the rating would take back the
    /// acknowledgement but leave the choice. The choice's own Undo comes
    /// first.
    LeechChoiceSince,
}

/// Whether review `id` can be undone (see [`RatingUndo`]).
pub fn rating_undo(conn: &Connection, id: ReviewId) -> rusqlite::Result<RatingUndo> {
    let found: Option<(bool, bool)> = conn
        .query_row(
            "SELECT c.last_review_at IS r.reviewed_at, \
               COALESCE(c.leech_acked_after >= r.id, 0) \
             FROM review_log r JOIN card c ON c.id = r.card_id WHERE r.id = ?1",
            [id.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(match found {
        Some((true, false)) => RatingUndo::Possible,
        Some((true, true)) => RatingUndo::LeechChoiceSince,
        _ => RatingUndo::CardChanged,
    })
}

/// Undo one review (design §6.7.9): the card's scheduling state is
/// restored from the snapshot (its suspension stays as it is now), the review is marked `undone` (kept for audit), the cards it
/// unlocked are deleted if still unreviewed, and so is any other unreviewed
/// Produce/Spelling card of the word whose unlock condition no longer holds
/// (G1). `word` are the traits of the card's word.
pub fn undo_review(
    tx: &Transaction<'_>,
    id: ReviewId,
    word: &WordTraits,
) -> Result<UndoOutcome, SchedulerError> {
    let (card_id, before, created, undone): (i64, String, String, bool) = tx
        .query_row(
            "SELECT card_id, before_json, created_card_ids, undone FROM review_log WHERE id = ?1",
            [id.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or(SchedulerError::ReviewNotFound(id.0))?;
    if undone {
        return Err(SchedulerError::AlreadyUndone(id.0));
    }
    let card_id = CardId::new(card_id);
    let snapshot: Snapshot = serde_json::from_str(&before)
        .map_err(|e| SchedulerError::Corrupt(format!("snapshot of review {}: {e}", id.0)))?;
    snapshot.restore_scheduling(tx, card_id)?;
    // The snapshot may predate sentences that upgraded the card's format.
    cards::upgrade_format(tx, word)?;
    tx.execute("UPDATE review_log SET undone = 1 WHERE id = ?1", [id.0])?;
    let mut removed_cards = cards::delete_unreviewed(tx, &created_ids(&created))?;
    // Cards unlocked meanwhile by something else (an import) on the
    // strength of this review go too if their condition no longer holds.
    removed_cards.extend(cards::relock_cards(tx, word)?);
    let card = cards::card(tx, card_id)?.ok_or(SchedulerError::CardNotFound(card_id.get()))?;
    Ok(UndoOutcome {
        review_id: id,
        card,
        removed_cards,
    })
}
