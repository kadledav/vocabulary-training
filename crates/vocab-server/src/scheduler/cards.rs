//! Card rows: creation and unlock (design §6.4), listings for the session
//! builder and Today (design §6.6, §6.7).

use std::collections::{HashMap, HashSet};

use fsrs::MemoryState;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};
use vocab_pack::types::Goal;

use super::{CardFormat, CardId, CardKind, CardState, LeechState, RawGrade};
use crate::catalog::{WordId, WordTraits};
use crate::clock::{Moment, StudyDay};

/// Understand stability (days) from which a Produce card unlocks (R-52).
pub const PRODUCE_UNLOCK_STABILITY: f64 = 7.0;
/// Days over which `normal_load` is averaged, and its floor (design §6.6).
const NORMAL_LOAD_DAYS: i32 = 14;
const NORMAL_LOAD_FLOOR: f64 = 20.0;

/// A card with its scheduling state.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Card {
    pub id: CardId,
    pub word_id: WordId,
    pub kind: CardKind,
    pub format: Option<CardFormat>,
    pub state: CardState,
    pub stability: Option<f64>,
    pub difficulty: Option<f64>,
    pub due_day: Option<StudyDay>,
    pub last_review_day: Option<StudyDay>,
    pub last_review_at: Option<String>,
    pub introduced_day: Option<StudyDay>,
    pub reps: u32,
    pub lapses: u32,
    pub consecutive_fives: u32,
    pub leech: LeechState,
    pub leech_lapses_at_ack: u32,
    pub suspended: bool,
    /// The card's latest review when a leech choice last acknowledged it
    /// (ratings up to it cannot be undone while the choice stands).
    #[serde(skip)]
    pub leech_acked_after: Option<i64>,
}

impl Card {
    /// The FSRS memory state (`None` while the card has none yet).
    pub fn memory(&self) -> Option<MemoryState> {
        match (self.stability, self.difficulty) {
            // Stored from f32 values; converting back is exact.
            (Some(s), Some(d)) => Some(MemoryState {
                stability: s as f32,
                difficulty: d as f32,
            }),
            _ => None,
        }
    }

    /// Due on `day` (due day today or earlier).
    pub fn is_due_on(&self, day: StudyDay) -> bool {
        self.due_day.is_some_and(|d| d <= day)
    }

    /// An unlocked Produce/Spelling card that was never reviewed: it has a
    /// due day (the earliest day it may come) but no memory state yet, so
    /// its first review is new learning, not a review (design §6.4, §17 M8).
    pub fn awaits_first_review(&self) -> bool {
        self.kind != CardKind::Understand && self.state == CardState::New
    }
}

const CARD_COLUMNS: &str = "id, word_id, kind, format, state, stability, difficulty, due_day, \
     last_review_day, last_review_at, introduced_day, reps, lapses, consecutive_fives, leech, \
     leech_lapses_at_ack, suspended, leech_acked_after";

fn card_row(row: &Row<'_>) -> rusqlite::Result<Card> {
    Ok(Card {
        id: CardId(row.get(0)?),
        word_id: WordId::new(row.get(1)?),
        kind: row.get(2)?,
        format: row.get(3)?,
        state: row.get(4)?,
        stability: row.get(5)?,
        difficulty: row.get(6)?,
        due_day: row.get(7)?,
        last_review_day: row.get(8)?,
        last_review_at: row.get(9)?,
        introduced_day: row.get(10)?,
        reps: row.get(11)?,
        lapses: row.get(12)?,
        consecutive_fives: row.get(13)?,
        leech: row.get(14)?,
        leech_lapses_at_ack: row.get(15)?,
        suspended: row.get(16)?,
        leech_acked_after: row.get(17)?,
    })
}

fn cards_where(
    conn: &Connection,
    condition: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Vec<Card>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CARD_COLUMNS} FROM card WHERE {condition} ORDER BY id"
    ))?;
    stmt.query_map(params, card_row)?.collect()
}

/// One card.
pub fn card(conn: &Connection, id: CardId) -> rusqlite::Result<Option<Card>> {
    conn.query_row(
        &format!("SELECT {CARD_COLUMNS} FROM card WHERE id = ?1"),
        [id.0],
        card_row,
    )
    .optional()
}

/// All cards of a word.
pub fn cards_of_word(conn: &Connection, word: WordId) -> rusqlite::Result<Vec<Card>> {
    cards_where(conn, "word_id = ?1", [word.get()])
}

/// Every card (the word list joins them to its words in memory: one query
/// for the whole list, design §7.1 screen 6).
pub fn all_cards(conn: &Connection) -> rusqlite::Result<Vec<Card>> {
    cards_where(conn, "1", [])
}

/// Cards with a due day (reviewed cards and unlocked Produce/Spelling
/// cards), not suspended. Callers filter by word, scope and kind.
pub fn scheduled_cards(conn: &Connection) -> rusqlite::Result<Vec<Card>> {
    cards_where(conn, "due_day IS NOT NULL AND suspended = 0", [])
}

/// Cards with a leech flag, flagged or acknowledged (design §6.5).
pub fn leech_cards(conn: &Connection) -> rusqlite::Result<Vec<Card>> {
    cards_where(conn, "leech != 'none'", [])
}

/// Understand cards that were never reviewed (new-word candidates).
pub fn new_cards(conn: &Connection) -> rusqlite::Result<Vec<Card>> {
    cards_where(
        conn,
        "kind = 'understand' AND state = 'new' AND suspended = 0",
        [],
    )
}

/// Words whose Understand card was introduced (first reviewed) on `day`;
/// its length is the day's count against the new-word limit (R-54).
pub fn introduced_words_on(conn: &Connection, day: StudyDay) -> rusqlite::Result<Vec<WordId>> {
    Ok(
        cards_where(conn, "kind = 'understand' AND introduced_day = ?1", [day])?
            .into_iter()
            .map(|c| c.word_id)
            .collect(),
    )
}

/// How many Produce/Spelling cards had their first review on `day` (the
/// daily cap on first-time typed cards, design §17 M8).
pub fn first_typed_reviews_on(conn: &Connection, day: StudyDay) -> rusqlite::Result<u32> {
    conn.query_row(
        "SELECT COUNT(*) FROM card WHERE kind != 'understand' AND introduced_day = ?1",
        [day],
        |row| row.get(0),
    )
}

/// The cards each word had a (non-undone) review on, on `day` (sibling
/// rule, design §6.7 step 1).
pub fn reviewed_cards_on(
    conn: &Connection,
    day: StudyDay,
) -> rusqlite::Result<HashMap<WordId, HashSet<CardId>>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.word_id, c.id FROM review_log r JOIN card c ON c.id = r.card_id \
         WHERE r.review_day = ?1 AND r.undone = 0",
    )?;
    let mut out: HashMap<WordId, HashSet<CardId>> = HashMap::new();
    for row in stmt.query_map([day], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (word, card): (i64, i64) = row?;
        out.entry(WordId::new(word))
            .or_default()
            .insert(CardId(card));
    }
    Ok(out)
}

/// The raw grade of each card's latest (non-undone) review on `day`
/// (extra practice, R-63).
pub fn latest_grades_on(
    conn: &Connection,
    day: StudyDay,
) -> rusqlite::Result<HashMap<CardId, RawGrade>> {
    let mut stmt = conn.prepare(
        "SELECT r.card_id, r.raw_grade FROM review_log r WHERE r.id IN \
         (SELECT MAX(id) FROM review_log WHERE review_day = ?1 AND undone = 0 GROUP BY card_id)",
    )?;
    let mut out = HashMap::new();
    for row in stmt.query_map([day], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (card, grade): (i64, u8) = row?;
        if let Some(grade) = RawGrade::new(grade) {
            out.insert(CardId(card), grade);
        }
    }
    Ok(out)
}

/// The study day each sentence of `word` was last shown in a (non-undone)
/// review, across all card kinds (design §6.7 step 6).
pub fn sentence_last_shown(
    conn: &Connection,
    word: WordId,
) -> rusqlite::Result<HashMap<i64, StudyDay>> {
    let mut stmt = conn.prepare(
        "SELECT r.sentence_id, MAX(r.review_day) FROM review_log r JOIN card c ON c.id = r.card_id \
         WHERE c.word_id = ?1 AND r.undone = 0 AND r.sentence_id IS NOT NULL GROUP BY r.sentence_id",
    )?;
    stmt.query_map([word.get()], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

/// Mean reviews per study day over the 14 days before `day`, without new
/// introductions and same-day repeats, at least 20 (design §6.6).
pub fn normal_load(conn: &Connection, day: StudyDay) -> rusqlite::Result<f64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM review_log WHERE undone = 0 AND review_day >= ?1 AND review_day < ?2 \
         AND elapsed_days >= 1 AND json_extract(before_json, '$.state') != 'new'",
        params![day.add_days(-NORMAL_LOAD_DAYS), day],
        |row| row.get(0),
    )?;
    // Review counts are far below 2^53.
    let mean = count as f64 / f64::from(NORMAL_LOAD_DAYS);
    Ok(mean.max(NORMAL_LOAD_FLOOR))
}

// ── creation and unlock (design §6.4) ───────────────────────────────────────

fn find(tx: &Connection, word: WordId, kind: CardKind) -> rusqlite::Result<Option<Card>> {
    conn_card(
        tx,
        "word_id = ?1 AND kind = ?2",
        params![word.get(), kind.as_str()],
    )
}

fn conn_card(
    conn: &Connection,
    condition: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Option<Card>> {
    conn.query_row(
        &format!("SELECT {CARD_COLUMNS} FROM card WHERE {condition}"),
        params,
        card_row,
    )
    .optional()
}

fn insert(
    tx: &Transaction<'_>,
    word: WordId,
    kind: CardKind,
    due_day: Option<StudyDay>,
    now: &Moment,
) -> rusqlite::Result<CardId> {
    tx.execute(
        "INSERT INTO card (word_id, kind, state, due_day, created_at) VALUES (?1, ?2, 'new', ?3, ?4)",
        params![word.get(), kind.as_str(), due_day, now.stamp()],
    )?;
    Ok(CardId(tx.last_insert_rowid()))
}

fn set_suspended(tx: &Transaction<'_>, card: &Card, suspended: bool) -> rusqlite::Result<()> {
    if card.suspended != suspended {
        tx.execute(
            "UPDATE card SET suspended = ?2 WHERE id = ?1",
            params![card.id.0, suspended],
        )?;
    }
    Ok(())
}

/// Has the Understand card had a passing review (raw ≤ 3) on a day after
/// its introduction day (Spelling unlock, design §6.4)?
fn passed_after_introduction(tx: &Connection, understand: &Card) -> rusqlite::Result<bool> {
    let Some(introduced) = understand.introduced_day else {
        return Ok(false);
    };
    tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM review_log WHERE card_id = ?1 AND undone = 0 \
         AND raw_grade <= 3 AND review_day > ?2)",
        params![understand.id.0, introduced],
        |row| row.get(0),
    )
}

/// Which of the Produce/Spelling unlock conditions (design §6.4) hold now.
fn unlock_conditions(
    tx: &Connection,
    word: &WordTraits,
    understand: &Card,
) -> rusqlite::Result<(bool, bool)> {
    let produce = word.effective_goal() == Goal::Active
        && understand.state != CardState::New
        && understand
            .stability
            .is_some_and(|s| s >= PRODUCE_UNLOCK_STABILITY)
        && word.visible_sentences >= 1;
    let spelling = word.spelling
        && understand.state == CardState::Review
        && passed_after_introduction(tx, understand)?;
    Ok((produce, spelling))
}

/// Create the Produce/Spelling cards whose unlock condition holds now
/// (design §6.4). Only creates; never changes existing cards, so a review
/// that calls it can be undone exactly by deleting what it returns.
pub fn unlock_cards(
    tx: &Transaction<'_>,
    word: &WordTraits,
    now: &Moment,
) -> rusqlite::Result<Vec<CardId>> {
    let mut created = Vec::new();
    let Some(understand) = find(tx, word.id, CardKind::Understand)? else {
        return Ok(created);
    };
    let tomorrow = Some(now.day.add_days(1));
    let (produce_ready, spelling_ready) = unlock_conditions(tx, word, &understand)?;
    if produce_ready && find(tx, word.id, CardKind::Produce)?.is_none() {
        created.push(insert(tx, word.id, CardKind::Produce, tomorrow, now)?);
    }
    if spelling_ready && find(tx, word.id, CardKind::Spelling)?.is_none() {
        created.push(insert(tx, word.id, CardKind::Spelling, tomorrow, now)?);
    }
    Ok(created)
}

/// The reverse of [`unlock_cards`] after an undo (guarantor decision G1):
/// delete the word's Produce/Spelling cards that were never reviewed and
/// whose unlock condition no longer holds, e.g. a card an import unlocked
/// on the strength of the rating that was just undone. Reviewed cards keep
/// their state.
pub fn relock_cards(tx: &Transaction<'_>, word: &WordTraits) -> rusqlite::Result<Vec<CardId>> {
    let Some(understand) = find(tx, word.id, CardKind::Understand)? else {
        return Ok(Vec::new());
    };
    let (produce_ready, spelling_ready) = unlock_conditions(tx, word, &understand)?;
    let mut stale = Vec::new();
    for (kind, ready) in [
        (CardKind::Produce, produce_ready),
        (CardKind::Spelling, spelling_ready),
    ] {
        if !ready && let Some(card) = find(tx, word.id, kind)? {
            stale.push(card.id);
        }
    }
    delete_unreviewed(tx, &stale)
}

/// One-way format upgrade (R-52, design §6.4): an Understand card that was
/// fixed to `word_only` because the word had too few sentences becomes a
/// `sentence` card once the word has enough visible ones. Never back: a
/// sentence card keeps its format when sentences are hidden later.
pub fn upgrade_format(tx: &Transaction<'_>, word: &WordTraits) -> rusqlite::Result<bool> {
    if CardFormat::for_first_review(CardKind::Understand, word) != CardFormat::Sentence {
        return Ok(false);
    }
    let changed = tx.execute(
        "UPDATE card SET format = ?2 WHERE word_id = ?1 AND kind = 'understand' AND format = ?3",
        params![
            word.id.get(),
            CardFormat::Sentence.as_str(),
            CardFormat::WordOnly.as_str()
        ],
    )?;
    Ok(changed > 0)
}

/// Make the word's cards match its traits (design §6.4): the Understand
/// card exists (and is upgraded from `word_only` to `sentence` when the
/// word has enough sentences now); Produce is suspended unless the goal is
/// `active` and Spelling unless `spelling` is set (state kept, re-activated
/// when the trait comes back); Produce/Spelling cards whose unlock
/// condition holds are created. Idempotent; called at import, at startup
/// (backfill) and on word changes. Returns the cards it created.
pub fn ensure_cards(
    tx: &Transaction<'_>,
    word: &WordTraits,
    now: &Moment,
) -> rusqlite::Result<Vec<CardId>> {
    let mut created = Vec::new();
    if find(tx, word.id, CardKind::Understand)?.is_none() {
        created.push(insert(tx, word.id, CardKind::Understand, None, now)?);
    }
    upgrade_format(tx, word)?;
    if let Some(produce) = find(tx, word.id, CardKind::Produce)? {
        set_suspended(tx, &produce, word.effective_goal() != Goal::Active)?;
    }
    if let Some(spelling) = find(tx, word.id, CardKind::Spelling)? {
        set_suspended(tx, &spelling, !word.spelling)?;
    }
    created.extend(unlock_cards(tx, word, now)?);
    Ok(created)
}

// ── snapshots for undo ──────────────────────────────────────────────────────

/// Every mutable column of a card, as stored in `review_log.before_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    format: Option<String>,
    state: String,
    stability: Option<f64>,
    difficulty: Option<f64>,
    due_day: Option<String>,
    last_review_day: Option<String>,
    last_review_at: Option<String>,
    introduced_day: Option<String>,
    reps: u32,
    lapses: u32,
    consecutive_fives: u32,
    leech: String,
    leech_lapses_at_ack: u32,
    suspended: bool,
    #[serde(default)]
    leech_acked_after: Option<i64>,
}

impl Snapshot {
    pub fn of(card: &Card) -> Self {
        Self {
            format: card.format.map(|f| f.as_str().to_owned()),
            state: card.state.as_str().to_owned(),
            stability: card.stability,
            difficulty: card.difficulty,
            due_day: card.due_day.map(|d| d.to_string()),
            last_review_day: card.last_review_day.map(|d| d.to_string()),
            last_review_at: card.last_review_at.clone(),
            introduced_day: card.introduced_day.map(|d| d.to_string()),
            reps: card.reps,
            lapses: card.lapses,
            consecutive_fives: card.consecutive_fives,
            leech: card.leech.as_str().to_owned(),
            leech_lapses_at_ack: card.leech_lapses_at_ack,
            suspended: card.suspended,
            leech_acked_after: card.leech_acked_after,
        }
    }

    pub fn stability(&self) -> Option<f64> {
        self.stability
    }

    /// Write the snapshot back to card `id`, suspension included (the Undo
    /// of an owner change, which may have suspended the card).
    pub fn restore(&self, tx: &Transaction<'_>, id: CardId) -> rusqlite::Result<()> {
        self.restore_scheduling(tx, id)?;
        tx.execute(
            "UPDATE card SET suspended = ?2 WHERE id = ?1",
            params![id.0, self.suspended],
        )?;
        Ok(())
    }

    /// Write the snapshot's scheduling state back to card `id`, leaving
    /// its suspension as it is now: a rating never changes suspension, so
    /// neither does its Undo (a goal switch made since stays).
    pub fn restore_scheduling(&self, tx: &Transaction<'_>, id: CardId) -> rusqlite::Result<()> {
        tx.execute(
            "UPDATE card SET format = ?2, state = ?3, stability = ?4, difficulty = ?5, due_day = ?6, \
             last_review_day = ?7, last_review_at = ?8, introduced_day = ?9, reps = ?10, lapses = ?11, \
             consecutive_fives = ?12, leech = ?13, leech_lapses_at_ack = ?14, \
             leech_acked_after = ?15 WHERE id = ?1",
            params![
                id.0,
                self.format,
                self.state,
                self.stability,
                self.difficulty,
                self.due_day,
                self.last_review_day,
                self.last_review_at,
                self.introduced_day,
                self.reps,
                self.lapses,
                self.consecutive_fives,
                self.leech,
                self.leech_lapses_at_ack,
                self.leech_acked_after,
            ],
        )?;
        Ok(())
    }
}

/// Store `card`'s scheduling columns (after a review; suspension is not
/// the scheduler's to change here).
pub fn save(tx: &Transaction<'_>, card: &Card) -> rusqlite::Result<()> {
    Snapshot::of(card).restore_scheduling(tx, card.id)
}

/// Delete cards created by an undone review if nobody reviewed them since.
pub fn delete_unreviewed(tx: &Transaction<'_>, ids: &[CardId]) -> rusqlite::Result<Vec<CardId>> {
    let mut removed = Vec::new();
    for id in ids {
        let n = tx.execute(
            "DELETE FROM card WHERE id = ?1 AND reps = 0 \
             AND NOT EXISTS (SELECT 1 FROM review_log WHERE card_id = ?1)",
            [id.0],
        )?;
        if n > 0 {
            removed.push(*id);
        }
    }
    Ok(removed)
}
