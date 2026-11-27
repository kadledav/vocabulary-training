//! What the Progress screen (design §7.1 screen 9; R-72) and the exports
//! (design §8.1; R-57, R-86) read from the scheduler, and the leech
//! acknowledgement (design §6.5; R-55).
//!
//! Every query here is one statement over the whole range (grouped in
//! SQL), never one per card or per day.

use rusqlite::{Connection, Transaction, params};
use serde::Serialize;

use super::{CardId, CardKind};
use crate::catalog::WordId;
use crate::clock::StudyDay;
use crate::db::{TableError, TableSink, write_table};

/// The recall tests of one card in a range of study days: its reviews that
/// came on a later study day than the card's previous review (`elapsed_days
/// ≥ 1`) of a card that was not new (`before_json` state ≠ `new`). Same-day
/// repeats and a card's very first review test nothing about memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallTally {
    pub card_id: CardId,
    pub word_id: WordId,
    pub kind: CardKind,
    /// Recall tests in the range.
    pub tested: u32,
    /// Of those, passed (raw grade 1–3).
    pub passed: u32,
}

/// Recall tests per card on the study days `from..=to` (undone ratings
/// left out).
pub fn recall_tallies(
    conn: &Connection,
    from: StudyDay,
    to: StudyDay,
) -> rusqlite::Result<Vec<RecallTally>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.word_id, c.kind, COUNT(*), SUM(r.raw_grade <= 3) \
         FROM review_log r JOIN card c ON c.id = r.card_id \
         WHERE r.undone = 0 AND r.review_day >= ?1 AND r.review_day <= ?2 \
           AND r.elapsed_days >= 1 AND json_extract(r.before_json, '$.state') != 'new' \
         GROUP BY c.id ORDER BY c.id",
    )?;
    stmt.query_map(params![from, to], |row| {
        Ok(RecallTally {
            card_id: CardId::new(row.get(0)?),
            word_id: WordId::new(row.get(1)?),
            kind: row.get(2)?,
            tested: row.get(3)?,
            passed: row.get(4)?,
        })
    })?
    .collect()
}

/// What happened on one study day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DayActivity {
    pub day: StudyDay,
    /// Ratings given (undone ones left out), in-session repeats included.
    pub reviews: u32,
    /// Words seen for the first time (their Understand card introduced).
    pub new_words: u32,
}

/// Activity on the study days `from..=to`, only days with any, oldest
/// first.
pub fn activity(
    conn: &Connection,
    from: StudyDay,
    to: StudyDay,
) -> rusqlite::Result<Vec<DayActivity>> {
    let mut stmt = conn.prepare(
        "SELECT day, SUM(reviews), SUM(new_words) FROM ( \
           SELECT review_day AS day, COUNT(*) AS reviews, 0 AS new_words FROM review_log \
           WHERE undone = 0 AND review_day >= ?1 AND review_day <= ?2 GROUP BY review_day \
           UNION ALL \
           SELECT introduced_day, 0, COUNT(*) FROM card \
           WHERE kind = 'understand' AND introduced_day >= ?1 AND introduced_day <= ?2 \
             AND reps > 0 \
           GROUP BY introduced_day) \
         GROUP BY day ORDER BY day",
    )?;
    stmt.query_map(params![from, to], |row| {
        Ok(DayActivity {
            day: row.get(0)?,
            reviews: row.get(1)?,
            new_words: row.get(2)?,
        })
    })?
    .collect()
}

/// The owner saw the leech screen of word `word` and chose what to do
/// (design §6.5): every flagged card of the word becomes `acknowledged`
/// and counts its lapses from now on, so four further lapses flag it
/// again. Returns how many cards were flagged.
pub fn acknowledge_leech(tx: &Transaction<'_>, word: WordId) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE card SET leech = 'acknowledged', leech_lapses_at_ack = lapses \
         WHERE word_id = ?1 AND leech = 'flagged'",
        [word.get()],
    )
}

/// Does the word have a flagged card (its leech screen is due)?
pub fn leech_flagged(conn: &Connection, word: WordId) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM card WHERE word_id = ?1 AND leech = 'flagged')",
        [word.get()],
        |row| row.get(0),
    )
}

/// The scheduler's tables for the full export (design §8.1): cards, the
/// review log (without the undo snapshot `before_json`) and the FSRS
/// parameter sets.
pub fn export(conn: &Connection, sink: &mut dyn TableSink) -> Result<(), TableError> {
    write_table(
        conn,
        sink,
        "cards",
        "SELECT id, word_id, kind, format, state, stability, difficulty, due_day, \
         last_review_day, last_review_at, introduced_day, reps, lapses, consecutive_fives, leech, \
         leech_lapses_at_ack, suspended, created_at FROM card ORDER BY id",
    )?;
    write_table(
        conn,
        sink,
        "reviews",
        "SELECT id, client_review_id, card_id, session_id, reviewed_at, client_ts, review_day, \
         raw_grade, fsrs_rating, suggested_grade, format, sentence_id, typed_answer, answer_match, \
         hint_used, elapsed_days, duration_ms, created_card_ids, undone, memory_updated \
         FROM review_log ORDER BY id",
    )?;
    write_table(
        conn,
        sink,
        "fsrs_params",
        "SELECT id, fitted_at, params_json, review_count, log_loss_new, log_loss_old, active \
         FROM fsrs_params ORDER BY id",
    )
}

#[cfg(test)]
mod tests {
    use super::super::{CardState, LeechReason, LeechState};
    use super::*;
    use crate::scheduler::Card;

    fn card(leech: LeechState, lapses: u32, at_ack: u32, stability: f64) -> Card {
        Card {
            id: CardId::new(1),
            word_id: WordId::new(1),
            kind: CardKind::Understand,
            format: None,
            state: CardState::Review,
            stability: Some(stability),
            difficulty: Some(5.0),
            due_day: None,
            last_review_day: None,
            last_review_at: None,
            introduced_day: None,
            reps: 9,
            lapses,
            consecutive_fives: 0,
            leech,
            leech_lapses_at_ack: at_ack,
            suspended: false,
        }
    }

    #[test]
    fn r55_reason_names_the_rule_that_flagged_the_card() {
        assert_eq!(card(LeechState::None, 9, 0, 1.0).leech_reason(), None);
        assert_eq!(
            card(LeechState::Flagged, 4, 0, 1.0).leech_reason(),
            Some(LeechReason::Lapses { lapses: 4 })
        );
        // Lapses count from the last acknowledgement.
        assert_eq!(
            card(LeechState::Flagged, 9, 4, 1.0).leech_reason(),
            Some(LeechReason::Lapses { lapses: 5 })
        );
        // Flagged with fewer: only the two-5s rule flags such a card.
        assert_eq!(
            card(LeechState::Flagged, 5, 4, 1.0).leech_reason(),
            Some(LeechReason::ForgotWhenKnown)
        );
        assert_eq!(
            card(LeechState::Acknowledged, 5, 5, 1.0).leech_reason(),
            Some(LeechReason::Acknowledged)
        );
    }

    #[test]
    fn r55_a_problem_card_until_remembered_for_three_weeks() {
        assert!(!card(LeechState::None, 9, 0, 1.0).is_problem());
        assert!(
            card(LeechState::Flagged, 4, 0, 90.0).is_problem(),
            "flagged"
        );
        assert!(card(LeechState::Acknowledged, 4, 4, 20.9).is_problem());
        assert!(!card(LeechState::Acknowledged, 4, 4, 21.0).is_problem());
    }
}
