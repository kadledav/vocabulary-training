//! What the Words screens read and change in the scheduler (design §7.1
//! screens 6 and 7): every card at once for the word list, a word's review
//! history, "Reset card", the cards of a deleted word, and the cards'
//! state before and after an owner change (its Undo, design §8).

use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};

use super::cards::{self, Card, Snapshot};
use super::{CardFormat, CardId, CardKind, ReviewId};
use crate::catalog::WordId;
use crate::clock::{Moment, StudyDay};

/// Most reviews [`history`] returns (the newest).
const HISTORY_LIMIT: i64 = 500;

/// One review of a word, as its history shows it (R-61: the raw grade).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryRow {
    pub review_id: ReviewId,
    pub card_id: CardId,
    pub kind: CardKind,
    pub format: Option<CardFormat>,
    pub reviewed_at: String,
    pub review_day: StudyDay,
    pub raw_grade: u8,
    pub typed_answer: Option<String>,
    /// `exact`, `none`, `skipped` or `dont_know` (typed answers).
    pub answer_match: Option<String>,
    pub hint_used: bool,
    pub sentence_id: Option<i64>,
    /// Study days since the card's previous review (0 = same day).
    pub elapsed_days: i64,
}

/// The word's reviews (not undone), newest first, at most 500.
pub fn history(conn: &Connection, word: WordId) -> rusqlite::Result<Vec<HistoryRow>> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.card_id, c.kind, r.format, r.reviewed_at, r.review_day, r.raw_grade, \
         r.typed_answer, r.answer_match, r.hint_used, r.sentence_id, r.elapsed_days \
         FROM review_log r JOIN card c ON c.id = r.card_id \
         WHERE c.word_id = ?1 AND r.undone = 0 ORDER BY r.id DESC LIMIT ?2",
    )?;
    stmt.query_map(params![word.get(), HISTORY_LIMIT], |row| {
        Ok(HistoryRow {
            review_id: ReviewId::new(row.get(0)?),
            card_id: CardId::new(row.get(1)?),
            kind: row.get(2)?,
            format: row.get(3)?,
            reviewed_at: row.get(4)?,
            review_day: row.get(5)?,
            raw_grade: row.get(6)?,
            typed_answer: row.get(7)?,
            answer_match: row.get(8)?,
            hint_used: row.get(9)?,
            sentence_id: row.get(10)?,
            elapsed_days: row.get(11)?,
        })
    })?
    .collect()
}

/// "Reset card" (design §6.4, §7.1): the card of `kind` forgets its memory
/// state, due day, format, counts and leech flag, as if it had never been
/// reviewed. An Understand card becomes a new word again (introduced
/// through the new-word quota, its format chosen again at its first
/// review); a Produce or Spelling card is due tomorrow, like a freshly
/// unlocked one. Its reviews stay in the history. Returns the card, `None`
/// when the word has no card of that kind.
pub fn reset_card(
    tx: &Transaction<'_>,
    word: WordId,
    kind: CardKind,
    now: &Moment,
) -> rusqlite::Result<Option<Card>> {
    let due = match kind {
        CardKind::Understand => None,
        CardKind::Produce | CardKind::Spelling => Some(now.day.add_days(1)),
    };
    let changed = tx.execute(
        "UPDATE card SET format = NULL, state = 'new', stability = NULL, difficulty = NULL, \
         due_day = ?3, last_review_day = NULL, last_review_at = NULL, introduced_day = NULL, \
         reps = 0, lapses = 0, consecutive_fives = 0, leech = 'none', leech_lapses_at_ack = 0 \
         WHERE word_id = ?1 AND kind = ?2",
        params![word.get(), kind.as_str(), due],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    let id: i64 = tx.query_row(
        "SELECT id FROM card WHERE word_id = ?1 AND kind = ?2",
        params![word.get(), kind.as_str()],
        |row| row.get(0),
    )?;
    super::card(tx, CardId::new(id))
}

/// Delete every card of a word with its review history (the word itself is
/// being deleted). Practice items of those cards go with them (foreign key
/// cascade of `session_item`). Returns how many cards were deleted.
pub fn delete_cards(tx: &Transaction<'_>, word: WordId) -> rusqlite::Result<usize> {
    tx.execute("DELETE FROM card WHERE word_id = ?1", [word.get()])
}

/// The scheduling state of every card of a word (opaque to other modules):
/// taken before and after an owner change, written back by its Undo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardsSnapshot(Vec<(i64, Snapshot)>);

/// The state of the word's cards now.
pub fn cards_snapshot(conn: &Connection, word: WordId) -> rusqlite::Result<CardsSnapshot> {
    Ok(CardsSnapshot(
        cards::cards_of_word(conn, word)?
            .iter()
            .map(|card| (card.id.get(), Snapshot::of(card)))
            .collect(),
    ))
}

/// Put the word's cards back to `before`: each card's state as it was, and
/// a card the change created (unlocked) deleted again while nobody has
/// reviewed it. The caller has checked that the cards are exactly as the
/// change left them.
pub fn restore_cards(
    tx: &Transaction<'_>,
    word: WordId,
    before: &CardsSnapshot,
) -> rusqlite::Result<()> {
    let created: Vec<CardId> = cards::cards_of_word(tx, word)?
        .iter()
        .map(|card| card.id)
        .filter(|id| !before.0.iter().any(|(known, _)| *known == id.get()))
        .collect();
    cards::delete_unreviewed(tx, &created)?;
    for (id, snapshot) in &before.0 {
        snapshot.restore(tx, CardId::new(*id))?;
    }
    Ok(())
}
