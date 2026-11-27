//! What the client gets: blocks with their items (design §8 practice item
//! payload), rating/undo results and the block summary.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::{BlockKind, ItemOrigin, SessionId, SessionStatus, build};
use crate::catalog::{self, PresentedSentence, SourceId, WordId, WordPresentation};
use crate::clock::{Moment, StudyDay};
use crate::scheduler::{
    self, CardFormat, CardId, CardKind, CardState, LeechState, RawGrade, ReviewId, ReviewOutcome,
};

/// The card part of an item.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemCard {
    pub id: CardId,
    pub kind: CardKind,
    /// The card's fixed format, or the one its first review will fix.
    pub format: CardFormat,
    /// Introduced by this item (shows the "New" badge; tested cold, R-52).
    pub is_new: bool,
    /// An in-session repeat after a 4/5.
    pub is_repeat: bool,
    /// Leech flagged (R-55): the client shows the leech screen (M9).
    pub leech_flagged: bool,
    /// The card's last rating was a fail (4/5): the client's slip guard
    /// watches for a reflex "1" (design §7.2).
    pub last_failed: bool,
}

/// The rating an item got.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnswerView {
    pub review_id: ReviewId,
    pub raw_grade: RawGrade,
}

/// One practice item with everything needed to render it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemView {
    pub item_id: i64,
    pub origin: ItemOrigin,
    /// `None` = not answered yet.
    pub answered: Option<AnswerView>,
    pub card: ItemCard,
    pub word: WordPresentation,
    /// The sentence chosen for this item (rotation, design §6.7 step 6).
    pub sentence: Option<PresentedSentence>,
    /// The word's other visible sentences ("More").
    pub other_sentences: Vec<PresentedSentence>,
    /// Accepted answers for typed cards (design §6.8, D-5): pack
    /// `accepted` ∪ headword ∪ target span of the shown sentence ∪
    /// owner-accepted. The client normalises and compares.
    pub accept: Vec<String>,
}

/// A practice block.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionView {
    pub id: SessionId,
    /// `regular`, `extra` (R-63) or `more_new`.
    pub kind: BlockKind,
    pub study_day: StudyDay,
    pub scope_source_id: Option<SourceId>,
    pub block_no: i64,
    pub block_size: i64,
    pub status: SessionStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub total: u32,
    pub answered: u32,
    /// "Resume — 12 of 20 left".
    pub remaining: u32,
    /// There is a rating in this block that undo would revert.
    pub can_undo: bool,
    /// All items in queue order; the next one is the first unanswered.
    pub items: Vec<ItemView>,
}

/// Result of `start_block`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StartResult {
    /// `None`: nothing to practise in this scope right now.
    pub session: Option<SessionView>,
    /// An unfinished block of today was returned instead of a new one.
    pub resumed: bool,
}

/// Result of a rating.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RateResult {
    /// The same `client_review_id` was posted before; nothing changed.
    pub duplicate: bool,
    pub review: ReviewOutcome,
    /// The in-session repeat this rating queued (4/5).
    pub repeat_item_id: Option<i64>,
    pub session: SessionView,
}

/// Result of an undo.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UndoResult {
    pub undone_review_id: ReviewId,
    /// The item that is unanswered again.
    pub item_id: i64,
    /// Unlocked cards that were removed.
    pub removed_cards: Vec<CardId>,
    pub session: SessionView,
}

/// A word to look at again (rated 4/5 in the block).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LookAgain {
    pub word_id: WordId,
    pub headword: String,
}

/// Block summary (design §7.1 screen 5).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlockSummary {
    pub session_id: SessionId,
    pub status: SessionStatus,
    pub answered: u32,
    pub unanswered: u32,
    /// Count of raw grades 1…5 (index 0 = grade 1).
    pub ratings: [u32; 5],
    /// New words introduced in this block.
    pub new_words: u32,
    /// In-session repeats answered.
    pub repeats: u32,
    pub look_again: Vec<LookAgain>,
}

/// A stored item.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemRow {
    pub id: i64,
    pub ord: f64,
    pub card_id: CardId,
    pub origin: ItemOrigin,
    pub sentence_id: Option<i64>,
    pub review_id: Option<ReviewId>,
    /// The format fixed when the item was created (G2); NULL for items of
    /// blocks built before migration 0003.
    pub format: Option<CardFormat>,
}

fn item_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ItemRow> {
    let origin: String = row.get(3)?;
    Ok(ItemRow {
        id: row.get(0)?,
        ord: row.get(1)?,
        card_id: CardId::new(row.get(2)?),
        origin: ItemOrigin::parse(&origin).unwrap_or(ItemOrigin::Due),
        sentence_id: row.get(4)?,
        review_id: row.get::<_, Option<i64>>(5)?.map(ReviewId::new),
        format: row.get(6)?,
    })
}

/// Items of a block in queue order: every answered item, and the
/// unanswered ones that may still be practised ([`still_practised`]). A
/// change of the word after the block was built (suspended, goal or
/// spelling switched off, a low-priority word excluded) leaves the stored
/// item alone, so an Undo or "unsuspend" brings it back (design §6.4: a
/// suspended word's cards are excluded from sessions).
pub fn items(conn: &Connection, id: &SessionId) -> rusqlite::Result<Vec<ItemRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, ord, card_id, origin, sentence_id, review_id, format FROM session_item \
         WHERE session_id = ?1 ORDER BY ord, id",
    )?;
    let rows: Vec<ItemRow> = stmt
        .query_map([id.as_str()], item_row)?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for item in rows {
        if item.review_id.is_some() || still_practised(conn, &item)? {
            out.push(item);
        }
    }
    Ok(out)
}

/// May the unanswered `item` still be practised? Its card and word exist
/// and are not suspended, and a new item's word may still be introduced.
fn still_practised(conn: &Connection, item: &ItemRow) -> rusqlite::Result<bool> {
    let Some(card) = scheduler::card(conn, item.card_id)? else {
        return Ok(false);
    };
    let Some(word) = catalog::word_traits(conn, card.word_id)? else {
        return Ok(false);
    };
    Ok(!card.suspended
        && !word.suspended
        && (item.origin != ItemOrigin::New || word.introducible()))
}

/// The block row.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: SessionId,
    pub kind: BlockKind,
    pub study_day: StudyDay,
    pub scope: Option<SourceId>,
    pub block_no: i64,
    pub block_size: i64,
    pub status: SessionStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
}

pub fn session_row(conn: &Connection, id: &SessionId) -> rusqlite::Result<Option<SessionRow>> {
    conn.query_row(
        "SELECT study_day, scope_source_id, block_no, block_size, status, started_at, finished_at, \
         kind FROM practice_session WHERE id = ?1",
        [id.as_str()],
        |row| {
            let status: String = row.get(4)?;
            let kind: String = row.get(7)?;
            Ok(SessionRow {
                id: id.clone(),
                kind: BlockKind::parse(&kind).unwrap_or(BlockKind::Regular),
                study_day: row.get(0)?,
                scope: row.get::<_, Option<i64>>(1)?.map(SourceId::new),
                block_no: row.get(2)?,
                block_size: row.get(3)?,
                status: SessionStatus::parse(&status).unwrap_or(SessionStatus::Expired),
                started_at: row.get(5)?,
                finished_at: row.get(6)?,
            })
        },
    )
    .optional()
}

/// The active block of `day`, if any.
pub fn active_id(conn: &Connection, day: StudyDay) -> rusqlite::Result<Option<SessionId>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM practice_session WHERE status = 'active' AND study_day = ?1 \
             ORDER BY started_at DESC, rowid DESC LIMIT 1",
            [day],
            |row| row.get(0),
        )
        .optional()?;
    Ok(id.map(SessionId))
}

/// How many blocks were finished with at least one rating (undone ratings
/// do not count), in all and on one study day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FinishedBlocks {
    /// Ever (the rating legend shows during the first blocks, design §7.2).
    pub total: u32,
    /// On the study day asked about ("Continue +20" on Today).
    pub today: u32,
}

/// Finished blocks in all and on study day `day`.
pub fn finished_blocks(conn: &Connection, day: StudyDay) -> rusqlite::Result<FinishedBlocks> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(p.study_day = ?1), 0) FROM practice_session p \
         WHERE p.status = 'finished' AND EXISTS (SELECT 1 FROM session_item i \
         WHERE i.session_id = p.id AND i.review_id IS NOT NULL)",
        [day],
        |row| {
            Ok(FinishedBlocks {
                total: row.get(0)?,
                today: row.get(1)?,
            })
        },
    )
}

/// Unanswered items of a block that may still be practised.
pub fn remaining(conn: &Connection, id: &SessionId) -> rusqlite::Result<u32> {
    Ok(count(
        items(conn, id)?
            .iter()
            .filter(|item| item.review_id.is_none())
            .count(),
    ))
}

/// Take the unanswered items of `card` out of the active blocks: its
/// learning state was reset, so what they were built for no longer holds.
pub fn withdraw_card(tx: &Connection, card: CardId) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM session_item WHERE card_id = ?1 AND review_id IS NULL \
         AND session_id IN (SELECT id FROM practice_session WHERE status = 'active')",
        [card.get()],
    )?;
    Ok(())
}

fn accept_set(word: &WordPresentation, sentence: Option<&PresentedSentence>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let candidates = word
        .accepted
        .iter()
        .chain(std::iter::once(&word.headword))
        .chain(sentence.map(|s| &s.en.target))
        .chain(word.owner_accepted.iter());
    for answer in candidates {
        if !answer.is_empty() && !out.contains(answer) {
            out.push(answer.clone());
        }
    }
    out
}

fn item_view(
    conn: &Connection,
    item: &ItemRow,
    scope: Option<SourceId>,
) -> rusqlite::Result<Option<ItemView>> {
    let Some(card) = scheduler::card(conn, item.card_id)? else {
        return Ok(None);
    };
    let Some(mut word) = catalog::presentation(conn, card.word_id)? else {
        return Ok(None);
    };
    // The stored format (G2); older items fall back to the card's.
    let format = match item.format {
        Some(format) => Some(format),
        None => build::shown_format(conn, &card)?,
    };
    let Some(format) = format else {
        return Ok(None);
    };
    let sentences = std::mem::take(&mut word.sentences);
    let sentence = sentences
        .iter()
        .find(|s| Some(s.id) == item.sentence_id)
        .cloned();
    // "More": the block's sentences (in-scope first rule, G3) but the shown one.
    let other_sentences: Vec<PresentedSentence> = build::in_scope(&sentences, scope)
        .into_iter()
        .filter(|s| Some(s.id) != item.sentence_id)
        .cloned()
        .collect();
    let accept = accept_set(&word, sentence.as_ref());
    let answered = match item.review_id {
        Some(review_id) => scheduler::raw_grade(conn, review_id)?.map(|raw_grade| AnswerView {
            review_id,
            raw_grade,
        }),
        None => None,
    };
    Ok(Some(ItemView {
        item_id: item.id,
        origin: item.origin,
        answered,
        card: ItemCard {
            id: card.id,
            kind: card.kind,
            format,
            is_new: item.origin == ItemOrigin::New,
            is_repeat: item.origin == ItemOrigin::Repeat,
            leech_flagged: card.leech == LeechState::Flagged,
            last_failed: card.state == CardState::Relearning,
        },
        word,
        sentence,
        other_sentences,
        accept,
    }))
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A block with its items as seen on study day `today` (`None` if it
/// does not exist). An active block of an earlier day shows as expired
/// (it is stored as such by the next start) and cannot be undone (G8).
pub fn session_view(
    conn: &Connection,
    id: &SessionId,
    today: StudyDay,
) -> rusqlite::Result<Option<SessionView>> {
    let Some(row) = session_row(conn, id)? else {
        return Ok(None);
    };
    let status = effective_status(&row, today);
    let rows = items(conn, id)?;
    let mut views = Vec::with_capacity(rows.len());
    for item in &rows {
        if let Some(view) = item_view(conn, item, row.scope)? {
            views.push(view);
        }
    }
    let answered = count(views.iter().filter(|v| v.answered.is_some()).count());
    let total = count(views.len());
    Ok(Some(SessionView {
        id: row.id,
        kind: row.kind,
        study_day: row.study_day,
        scope_source_id: row.scope,
        block_no: row.block_no,
        block_size: row.block_size,
        status,
        started_at: row.started_at,
        finished_at: row.finished_at,
        total,
        answered,
        remaining: total - answered,
        can_undo: status == SessionStatus::Active && answered > 0,
        items: views,
    }))
}

/// The active block of study day `day` (resume after reload or restart,
/// design §6.7 step 8). Blocks of earlier days are not returned.
pub fn current(conn: &Connection, day: StudyDay) -> rusqlite::Result<Option<SessionView>> {
    match active_id(conn, day)? {
        Some(id) => session_view(conn, &id, day),
        None => Ok(None),
    }
}

/// Summary of a block's answered items.
pub fn summary(conn: &Connection, row: &SessionRow) -> rusqlite::Result<BlockSummary> {
    let items = items(conn, &row.id)?;
    let mut ratings = [0u32; 5];
    let mut answered = 0;
    let mut new_words = 0;
    let mut repeats = 0;
    let mut look_again: Vec<LookAgain> = Vec::new();
    for item in &items {
        let Some(review) = item.review_id else {
            continue;
        };
        let Some(grade) = scheduler::raw_grade(conn, review)? else {
            continue;
        };
        answered += 1;
        if let Some(slot) = ratings.get_mut(usize::from(grade.get()) - 1) {
            *slot += 1;
        }
        match item.origin {
            ItemOrigin::New => new_words += 1,
            ItemOrigin::Repeat => repeats += 1,
            ItemOrigin::Due => {}
        }
        if grade.failed()
            && let Some(card) = scheduler::card(conn, item.card_id)?
            && !look_again.iter().any(|l| l.word_id == card.word_id)
            && let Some(word) = catalog::presentation(conn, card.word_id)?
        {
            look_again.push(LookAgain {
                word_id: card.word_id,
                headword: word.headword,
            });
        }
    }
    Ok(BlockSummary {
        session_id: row.id.clone(),
        status: row.status,
        answered,
        unanswered: count(items.len()) - answered,
        ratings,
        new_words,
        repeats,
        look_again,
    })
}

/// The status of a block on study day `today`: an active block of another
/// day has expired (blocks expire at day rollover, design §6.7 step 8).
pub fn effective_status(row: &SessionRow, today: StudyDay) -> SessionStatus {
    match row.status {
        SessionStatus::Active if row.study_day != today => SessionStatus::Expired,
        status => status,
    }
}

/// End an active block: `finished`, or `expired` when it belongs to an
/// earlier study day (G8). Other blocks are left as they are.
pub fn close_block(tx: &Connection, id: &SessionId, now: &Moment) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE practice_session SET status = CASE WHEN study_day = ?3 THEN 'finished' ELSE 'expired' END, \
         finished_at = CASE WHEN study_day = ?3 THEN ?2 ELSE finished_at END \
         WHERE id = ?1 AND status = 'active'",
        params![id.as_str(), now.stamp(), now.day],
    )?;
    Ok(())
}

/// Add an item to a block.
pub fn insert_item(
    tx: &Connection,
    session: &SessionId,
    ord: f64,
    card: CardId,
    origin: ItemOrigin,
    sentence: Option<i64>,
    format: Option<CardFormat>,
) -> rusqlite::Result<i64> {
    tx.execute(
        "INSERT INTO session_item (session_id, ord, card_id, origin, sentence_id, format) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            session.as_str(),
            ord,
            card.get(),
            origin.as_str(),
            sentence,
            format.map(CardFormat::as_str)
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Record that item `item` was answered by `review` (or not, with `None`).
pub fn set_answer(tx: &Connection, item: i64, review: Option<ReviewId>) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE session_item SET review_id = ?2 WHERE id = ?1",
        params![item, review.map(ReviewId::get)],
    )?;
    Ok(())
}
