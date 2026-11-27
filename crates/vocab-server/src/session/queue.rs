//! Rating, in-session repeats, undo and finishing a block (design §6.7
//! steps 7–9; R-54).

use rusqlite::{Transaction, params};

use super::build::{item_sentence, shown_format};
use super::view::{self, BlockSummary, ItemRow, RateResult, SessionRow, UndoResult};
use super::{ItemOrigin, RateInput, SessionError, SessionId, SessionStatus};
use crate::catalog;
use crate::clock::Moment;
use crate::scheduler::{self, CardId, RawGrade, ReviewInput};

/// A card is repeated at most this often per block (design §6.7 step 7).
const MAX_REPEATS_PER_BLOCK: u32 = 3;

/// Items between a failed rating and its repeat: raw 5 → 3, raw 4 → 7.
fn repeat_gap(grade: RawGrade) -> Option<usize> {
    match grade.get() {
        5 => Some(3),
        4 => Some(7),
        _ => None,
    }
}

/// The `ord` for a repeat that should come after `gap` more items of
/// `upcoming` (unanswered items after the current one, in order): between
/// the gap-th and the next one, or at the end if fewer remain.
fn repeat_ord(current: f64, upcoming: &[f64], gap: usize) -> f64 {
    match (upcoming.get(gap.wrapping_sub(1)), upcoming.get(gap)) {
        (Some(after), Some(before)) if gap > 0 => (after + before) / 2.0,
        _ => upcoming.last().copied().unwrap_or(current) + 1.0,
    }
}

/// The block, checked to be active and of today.
fn active_session(
    tx: &Transaction<'_>,
    id: &SessionId,
    now: &Moment,
) -> Result<SessionRow, SessionError> {
    let row = view::session_row(tx, id)?.ok_or(SessionError::NotFound)?;
    match row.status {
        // Blocks expire at day rollover (stored as such by the next start).
        SessionStatus::Active if row.study_day != now.day => Err(SessionError::Expired),
        SessionStatus::Active => Ok(row),
        SessionStatus::Expired => Err(SessionError::Expired),
        SessionStatus::Finished => Err(SessionError::NotActive("finished")),
    }
}

fn view_of(
    tx: &Transaction<'_>,
    id: &SessionId,
    now: &Moment,
) -> Result<view::SessionView, SessionError> {
    view::session_view(tx, id, now.day)?.ok_or(SessionError::NotFound)
}

/// A repeated post of an already recorded rating (idempotency by
/// `client_review_id`): the stored result, nothing changes.
fn replay(
    tx: &Transaction<'_>,
    id: &SessionId,
    input: &RateInput,
    stored: &scheduler::StoredReview,
    now: &Moment,
) -> Result<RateResult, SessionError> {
    if stored.session_id.as_deref() != Some(id.as_str()) {
        return Err(SessionError::ReviewIdReused);
    }
    if stored.undone {
        return Err(SessionError::ReviewUndone);
    }
    let answered_item = view::items(tx, id)?
        .into_iter()
        .find(|item| item.review_id == Some(stored.review_id));
    if answered_item.is_none_or(|item| item.id != input.item_id) {
        return Err(SessionError::ReviewIdReused);
    }
    let review =
        scheduler::review_outcome(tx, stored.review_id)?.ok_or(SessionError::ReviewIdReused)?;
    Ok(RateResult {
        duplicate: true,
        review,
        repeat_item_id: None,
        session: view_of(tx, id, now)?,
    })
}

/// Save one rating (design §8 `POST /api/sessions/{id}/reviews`): the
/// review (scheduler), the item's answer and, after 4/5, an in-session
/// repeat. Items are rated in queue order. Posting the same
/// `client_review_id` again returns the stored result.
pub fn rate(
    tx: &Transaction<'_>,
    id: &SessionId,
    input: &RateInput,
    now: &Moment,
) -> Result<RateResult, SessionError> {
    let client_review_id = input.validate()?;
    if let Some(stored) = scheduler::find_review(tx, &client_review_id)? {
        return replay(tx, id, input, &stored, now);
    }
    let session = active_session(tx, id, now)?;
    let items = view::items(tx, id)?;
    let position = items
        .iter()
        .position(|item| item.id == input.item_id)
        .ok_or(SessionError::ItemNotFound)?;
    let item = &items[position];
    if item.review_id.is_some() {
        return Err(SessionError::AlreadyAnswered);
    }
    if items.iter().find(|i| i.review_id.is_none()).map(|i| i.id) != Some(item.id) {
        return Err(SessionError::NotNextItem);
    }
    let card = scheduler::card(tx, item.card_id)?.ok_or(SessionError::ItemNotFound)?;
    input.check_for(card.kind)?;
    let word = catalog::word_traits(tx, card.word_id)?.ok_or(SessionError::ItemNotFound)?;
    let typed_answer = input.stored_typed_answer();
    let review = scheduler::record_review(
        tx,
        &ReviewInput {
            client_review_id: &client_review_id,
            card_id: card.id,
            session_id: Some(id.as_str()),
            raw_grade: input.raw_grade,
            suggested_grade: input.suggested_grade,
            typed_answer: typed_answer.as_deref(),
            answer_match: input.answer_match,
            hint_used: input.hint_used,
            duration_ms: input.duration_ms,
            client_ts: input.client_ts.as_deref(),
            sentence_id: item.sentence_id,
            // What was shown: stored with the item when it was queued (G2).
            format: item.format,
        },
        &word,
        now,
    )?;
    view::set_answer(tx, item.id, Some(review.review_id))?;
    let repeat_item_id = queue_repeat(tx, &session, &items, position, input.raw_grade, now)?;
    Ok(RateResult {
        duplicate: false,
        review,
        repeat_item_id,
        session: view_of(tx, id, now)?,
    })
}

/// After a 4/5, insert a repeat of the card (design §6.7 step 7), with a
/// different sentence than this attempt when one exists.
fn queue_repeat(
    tx: &Transaction<'_>,
    session: &SessionRow,
    items: &[ItemRow],
    position: usize,
    grade: RawGrade,
    now: &Moment,
) -> Result<Option<i64>, SessionError> {
    let Some(gap) = repeat_gap(grade) else {
        return Ok(None);
    };
    let item = &items[position];
    let repeats = items
        .iter()
        .filter(|i| i.card_id == item.card_id && i.origin == ItemOrigin::Repeat)
        .count();
    if repeats >= MAX_REPEATS_PER_BLOCK as usize {
        return Ok(None);
    }
    let upcoming: Vec<f64> = items[position + 1..]
        .iter()
        .filter(|i| i.review_id.is_none())
        .map(|i| i.ord)
        .collect();
    let ord = repeat_ord(item.ord, &upcoming, gap);
    let card = scheduler::card(tx, item.card_id)?.ok_or(SessionError::ItemNotFound)?;
    let sentence = item_sentence(tx, &card, now.day, item.sentence_id, session.scope)?;
    // The card's format is fixed now (it was just reviewed).
    let format = shown_format(tx, &card)?;
    let repeat = view::insert_item(
        tx,
        &session.id,
        ord,
        item.card_id,
        ItemOrigin::Repeat,
        sentence,
        format,
    )?;
    Ok(Some(repeat))
}

/// Revert the last non-undone rating of the active block (design §6.7
/// step 9, D-3: repeatable step by step): the scheduler restores the card
/// and removes unlocked cards, the item becomes unanswered again, and the
/// repeat that rating queued is removed.
pub fn undo_last(
    tx: &Transaction<'_>,
    id: &SessionId,
    now: &Moment,
) -> Result<UndoResult, SessionError> {
    active_session(tx, id, now)?;
    let items = view::items(tx, id)?;
    let last = items
        .iter()
        .filter_map(|i| i.review_id.map(|r| (r, i)))
        .max_by_key(|(review, _)| *review)
        .map(|(review, item)| (review, item.clone()))
        .ok_or(SessionError::NothingToUndo)?;
    let (review, item) = last;
    match scheduler::rating_undo(tx, review)? {
        scheduler::RatingUndo::Possible => {}
        scheduler::RatingUndo::CardChanged => return Err(SessionError::CardChangedSince),
        scheduler::RatingUndo::LeechChoiceSince => return Err(SessionError::LeechChoiceSince),
    }
    let card = scheduler::card(tx, item.card_id)?.ok_or(SessionError::ItemNotFound)?;
    let word = catalog::word_traits(tx, card.word_id)?.ok_or(SessionError::ItemNotFound)?;
    let outcome = scheduler::undo_review(tx, review, &word)?;
    view::set_answer(tx, item.id, None)?;
    // The repeat this rating queued: an unanswered repeat of the same card
    // created after the item (ratings after this one were undone first,
    // and their repeats removed with them).
    remove_spawned_repeat(tx, id, item.card_id, item.id)?;
    Ok(UndoResult {
        undone_review_id: review,
        item_id: item.id,
        removed_cards: outcome.removed_cards,
        session: view_of(tx, id, now)?,
    })
}

fn remove_spawned_repeat(
    tx: &Transaction<'_>,
    id: &SessionId,
    card: CardId,
    after_item: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM session_item WHERE id = (SELECT MAX(id) FROM session_item \
         WHERE session_id = ?1 AND card_id = ?2 AND origin = 'repeat' AND review_id IS NULL AND id > ?3)",
        params![id.as_str(), card.get(), after_item],
    )?;
    Ok(())
}

/// End a block (Esc / Done) and return its summary. Unanswered items are
/// simply not reviewed. A block of an earlier study day is marked
/// `expired`, not `finished` (G8). Finishing a finished or expired block
/// only returns the summary again.
pub fn finish(
    tx: &Transaction<'_>,
    id: &SessionId,
    now: &Moment,
) -> Result<BlockSummary, SessionError> {
    view::session_row(tx, id)?.ok_or(SessionError::NotFound)?;
    view::close_block(tx, id, now)?;
    let row = view::session_row(tx, id)?.ok_or(SessionError::NotFound)?;
    Ok(view::summary(tx, &row)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(n: u8) -> RawGrade {
        RawGrade::new(n).expect("grade")
    }

    #[test]
    fn r54_five_repeats_after_three_more_items_four_after_seven() {
        assert_eq!(repeat_gap(grade(5)), Some(3));
        assert_eq!(repeat_gap(grade(4)), Some(7));
        assert_eq!(repeat_gap(grade(3)), None);
        assert_eq!(repeat_gap(grade(1)), None);
    }

    #[test]
    fn r54_repeat_goes_between_the_gap_th_and_the_next_item() {
        let upcoming = [2.0, 3.0, 4.0, 5.0, 6.0];
        // Three items in between → between the 3rd (4.0) and 4th (5.0).
        assert_eq!(repeat_ord(1.0, &upcoming, 3), 4.5);
        // Exactly three left → at the end.
        assert_eq!(repeat_ord(1.0, &upcoming[..3], 3), 5.0);
        // Fewer than the gap → appended at the end.
        assert_eq!(repeat_ord(1.0, &upcoming[..2], 7), 4.0);
        // Nothing left → right after the current item.
        assert_eq!(repeat_ord(1.0, &[], 3), 2.0);
    }
}
