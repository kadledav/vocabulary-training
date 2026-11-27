//! Building a block (design §6.6, §6.7 steps 1–6, §15 interference rule).

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, Transaction, params};
use serde::Serialize;

use super::view::{self, StartResult};
use super::{
    BlockKind, ItemOrigin, MORE_NEW_DEFAULT_MIN, MORE_NEW_MAX, SERVED_KINDS, SessionError,
    SessionId, StartKind, StartRequest,
};
use crate::catalog::{self, PracticeWord, PresentedSentence, SourceId, WordId, WordTraits};
use crate::clock::{Moment, StudyDay};
use crate::scheduler::{self, Card, CardFormat, CardId, CardKind, CardState};
use crate::settings::{self, Settings};

/// Most Spelling cards per block (R-58).
const SPELLING_CAP: usize = 3;
/// New cards go to every 4th position: 3, 7, 11, … (0-based).
const NEW_EVERY: usize = 4;
/// A sentence shown within this many study days is avoided when another
/// one exists (design §6.7 step 6).
const SENTENCE_REST_DAYS: i32 = 7;
/// Backlog factor of the new-word pause (design §6.6).
const BACKLOG_FACTOR: f64 = 2.0;

/// Today's new-word budget (R-54, design §6.6).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewAllowance {
    pub per_day: u32,
    /// Understand cards introduced (first reviewed) today.
    pub introduced_today: u32,
    /// `per_day − introduced_today`.
    pub left_today: u32,
    /// What a block may introduce now (0 while paused).
    pub quota: u32,
    /// New words are paused (backlog, and not overridden today).
    pub paused: bool,
    /// The backlog condition holds (`due_now > 2 × normal_load`).
    pub backlog: bool,
    /// The owner chose "Add new words anyway" today.
    pub anyway_today: bool,
    /// Reviews due (cards of served kinds with a memory state, all
    /// sources, words not suspended); Produce/Spelling cards waiting for
    /// their first review are not a backlog and do not count.
    pub due_now: u32,
    /// Mean daily reviews over the last 14 days, at least 20.
    pub normal_load: f64,
}

/// Today's allowance of first-time Produce/Spelling cards (design §17
/// M8): unlocked cards wait until a day has room for them, so words that
/// reach the unlock threshold together are spread over several days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TypedAllowance {
    /// The `new_typed_per_day` setting.
    pub per_day: u32,
    /// Produce/Spelling cards first reviewed today.
    pub introduced_today: u32,
    /// `per_day − introduced_today`.
    pub left_today: u32,
    /// What blocks may still introduce now: 0 while new learning is paused
    /// for the review backlog (the same pause and the same "add anyway" as
    /// new words, design §6.6).
    pub quota: u32,
    pub paused: bool,
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Scheduled (reviewed or unlocked) cards of `kinds` whose word is in
/// `words`. The one place that decides which scheduled cards a scope has.
fn scheduled_of(
    conn: &Connection,
    words: &HashMap<WordId, PracticeWord>,
    kinds: &[CardKind],
) -> rusqlite::Result<Vec<Card>> {
    Ok(scheduler::scheduled_cards(conn)?
        .into_iter()
        .filter(|c| kinds.contains(&c.kind) && words.contains_key(&c.word_id))
        .collect())
}

/// Reviewed cards (with a memory state) of the served kinds in `scope`
/// (words not suspended, R-59): the reviews that are or will be due.
/// Produce/Spelling cards waiting for their first review are left out;
/// they come through [`TypedAllowance`]. Callers pick the due ones with
/// `Card::is_due_on`.
pub fn reviews_in_scope(conn: &Connection, scope: Option<SourceId>) -> rusqlite::Result<Vec<Card>> {
    Ok(
        scheduled_of(conn, &catalog::practice_words(conn, scope)?, SERVED_KINDS)?
            .into_iter()
            .filter(|c| !c.awaits_first_review())
            .collect(),
    )
}

/// Unlocked Produce/Spelling cards of the served kinds in `scope` that wait
/// for their first review and may come on `day` (due day reached).
pub fn typed_waiting(
    conn: &Connection,
    day: StudyDay,
    scope: Option<SourceId>,
) -> rusqlite::Result<u32> {
    let words = catalog::practice_words(conn, scope)?;
    Ok(count(
        scheduled_of(conn, &words, SERVED_KINDS)?
            .iter()
            .filter(|c| c.awaits_first_review() && c.is_due_on(day))
            .count(),
    ))
}

/// The allowance of first-time Produce/Spelling cards on `day`; `new` is
/// the day's new-word allowance (its pause applies to both).
pub fn typed_allowance(
    conn: &Connection,
    day: StudyDay,
    settings: &Settings,
    new: &NewAllowance,
) -> rusqlite::Result<TypedAllowance> {
    let introduced_today = scheduler::first_typed_reviews_on(conn, day)?;
    let left_today = settings.new_typed_per_day.saturating_sub(introduced_today);
    Ok(TypedAllowance {
        per_day: settings.new_typed_per_day,
        introduced_today,
        left_today,
        quota: if new.paused { 0 } else { left_today },
        paused: new.paused,
    })
}

/// New-word candidates among `words`: never reviewed Understand cards
/// (when served) of words that may be introduced (R-51). The one place
/// that decides what counts as a candidate.
fn candidates_of<'w>(
    conn: &Connection,
    words: &'w HashMap<WordId, PracticeWord>,
) -> rusqlite::Result<Vec<(&'w PracticeWord, Card)>> {
    if !SERVED_KINDS.contains(&CardKind::Understand) {
        return Ok(Vec::new());
    }
    Ok(scheduler::new_cards(conn)?
        .into_iter()
        .filter_map(|c| {
            let word = words.get(&c.word_id)?;
            word.traits.introducible().then_some((word, c))
        })
        .collect())
}

/// How many new words could be introduced in `scope` (before the quota,
/// the sibling and the interference rules).
pub fn new_candidate_count(conn: &Connection, scope: Option<SourceId>) -> rusqlite::Result<u32> {
    let words = catalog::practice_words(conn, scope)?;
    Ok(count(candidates_of(conn, &words)?.len()))
}

/// The new-word allowance on `day` (design §6.6).
pub fn new_word_allowance(
    conn: &Connection,
    day: StudyDay,
    settings: &Settings,
) -> rusqlite::Result<NewAllowance> {
    let introduced_today = count(scheduler::introduced_words_on(conn, day)?.len());
    let due_now = count(
        reviews_in_scope(conn, None)?
            .iter()
            .filter(|c| c.is_due_on(day))
            .count(),
    );
    let normal_load = scheduler::normal_load(conn, day)?;
    let backlog = f64::from(due_now) > BACKLOG_FACTOR * normal_load;
    let anyway_today = settings::new_words_anyway_on(conn, day)?;
    let paused = backlog && !anyway_today;
    let left_today = settings.new_per_day.saturating_sub(introduced_today);
    Ok(NewAllowance {
        per_day: settings.new_per_day,
        introduced_today,
        left_today,
        quota: if paused { 0 } else { left_today },
        paused,
        backlog,
        anyway_today,
        due_now,
        normal_load,
    })
}

/// Pick the sentence to show (design §6.7 step 6): the least recently shown
/// visible sentence, avoiding those shown in the last 7 study days when an
/// alternative exists; ties → lowest position. A repeat passes the previous
/// attempt's sentence, which is skipped when another one exists.
///
/// `sentences` are `(id, position)` of the visible sentences.
pub fn choose_sentence(
    sentences: &[(i64, i64)],
    last_shown: &HashMap<i64, StudyDay>,
    day: StudyDay,
    previous: Option<i64>,
) -> Option<i64> {
    let mut pool: Vec<(i64, i64)> = sentences
        .iter()
        .copied()
        .filter(|(id, _)| Some(*id) != previous)
        .collect();
    if pool.is_empty() {
        pool = sentences.to_vec();
    }
    let rested: Vec<(i64, i64)> = pool
        .iter()
        .copied()
        .filter(|(id, _)| {
            last_shown
                .get(id)
                .is_none_or(|shown| shown.days_between(day) >= SENTENCE_REST_DAYS)
        })
        .collect();
    if !rested.is_empty() {
        pool = rested;
    }
    pool.into_iter()
        // `None` (never shown) sorts first.
        .min_by_key(|(id, position)| (last_shown.get(id).copied(), *position, *id))
        .map(|(id, _)| id)
}

/// The sentences a block in `scope` uses (G3): those that came with the
/// scope's source; all of them when there are none (or no scope).
pub fn in_scope(
    sentences: &[PresentedSentence],
    scope: Option<SourceId>,
) -> Vec<&PresentedSentence> {
    let scoped: Vec<&PresentedSentence> = sentences
        .iter()
        .filter(|s| scope.is_some_and(|scope| s.source_id == Some(scope)))
        .collect();
    if scoped.is_empty() {
        sentences.iter().collect()
    } else {
        scoped
    }
}

/// The sentence for an item of `word` in a block of `scope` (`None` when
/// the word has no visible sentence).
pub fn sentence_for(
    conn: &Connection,
    word: WordId,
    day: StudyDay,
    previous: Option<i64>,
    scope: Option<SourceId>,
) -> rusqlite::Result<Option<i64>> {
    let Some(presentation) = catalog::presentation(conn, word)? else {
        return Ok(None);
    };
    let sentences: Vec<(i64, i64)> = in_scope(&presentation.sentences, scope)
        .into_iter()
        .map(|s| (s.id, s.position))
        .collect();
    let last_shown = scheduler::sentence_last_shown(conn, word)?;
    Ok(choose_sentence(&sentences, &last_shown, day, previous))
}

/// One planned item.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Planned {
    card: CardId,
    word: WordId,
    origin: ItemOrigin,
}

/// A due card ranked for the block: relearning first, then priority
/// (highest first), then `rank` (the waiting order of first-time cards),
/// then card id.
struct Ranked {
    relearning: bool,
    priority: f64,
    rank: usize,
    card: Card,
}

/// Steps 1–3: due cards of `kinds` in priority order, one per word, sibling
/// rule and Spelling cap applied.
///
/// Produce/Spelling cards waiting for their first review (design §17 M8)
/// join only up to `typed_quota`, those waiting longest first (then
/// usefulness, then card id), with the neutral priority 0, and never in
/// place of a due review of the same word: a review put off costs memory,
/// a first-time card loses nothing by waiting a day.
fn plan_due(
    conn: &Connection,
    day: StudyDay,
    kinds: &[CardKind],
    words: &HashMap<WordId, PracticeWord>,
    reviewed_today: &HashMap<WordId, HashSet<CardId>>,
    typed_quota: u32,
) -> rusqlite::Result<Vec<Planned>> {
    let model = scheduler::model(conn)?;
    let (mut first_time, reviews): (Vec<Card>, Vec<Card>) = scheduled_of(conn, words, kinds)?
        .into_iter()
        .filter(|c| c.is_due_on(day))
        .filter(|c| {
            // Sibling rule: no other card of the word reviewed today.
            reviewed_today
                .get(&c.word_id)
                .is_none_or(|cards| cards.iter().all(|id| *id == c.id))
        })
        .filter(|c| words.contains_key(&c.word_id))
        .partition(Card::awaits_first_review);
    let usefulness = |c: &Card| words.get(&c.word_id).map_or(0, |w| w.traits.usefulness);
    let mut due: Vec<Ranked> = Vec::with_capacity(reviews.len());
    for card in reviews {
        if let Some(word) = words.get(&card.word_id) {
            due.push(Ranked {
                relearning: card.state == CardState::Relearning,
                priority: model.priority(&card, &word.traits, day),
                rank: 0,
                card,
            });
        }
    }
    let review_words: HashSet<WordId> = due.iter().map(|r| r.card.word_id).collect();
    // Waiting longest first, then the more useful word, then card id.
    first_time.sort_by(|a, b| {
        a.due_day
            .cmp(&b.due_day)
            .then(usefulness(b).cmp(&usefulness(a)))
            .then(a.id.cmp(&b.id))
    });
    let mut typed_words = HashSet::new();
    let admitted = first_time
        .into_iter()
        .filter(|c| !review_words.contains(&c.word_id) && typed_words.insert(c.word_id))
        .take(typed_quota as usize);
    for (rank, card) in admitted.enumerate() {
        if let Some(word) = words.get(&card.word_id) {
            due.push(Ranked {
                relearning: false,
                priority: model.priority(&card, &word.traits, day),
                rank,
                card,
            });
        }
    }
    due.sort_by(|a, b| {
        b.relearning
            .cmp(&a.relearning)
            .then(b.priority.total_cmp(&a.priority))
            .then(a.rank.cmp(&b.rank))
            .then(a.card.id.cmp(&b.card.id))
    });
    let mut seen_words = HashSet::new();
    let mut spelling = 0;
    let mut planned = Vec::new();
    for Ranked { card, .. } in due {
        if !seen_words.insert(card.word_id) {
            continue;
        }
        if card.kind == CardKind::Spelling {
            if spelling == SPELLING_CAP {
                continue;
            }
            spelling += 1;
        }
        planned.push(Planned {
            card: card.id,
            word: card.word_id,
            origin: ItemOrigin::Due,
        });
    }
    Ok(planned)
}

/// Step 4: new Understand cards within the quota, by usefulness, source
/// order and word id, skipping words that interfere with a word introduced
/// today or already chosen.
fn plan_new(
    conn: &Connection,
    day: StudyDay,
    quota: u32,
    words: &HashMap<WordId, PracticeWord>,
    reviewed_today: &HashMap<WordId, HashSet<CardId>>,
    planned_words: &HashSet<WordId>,
) -> rusqlite::Result<Vec<Planned>> {
    if quota == 0 {
        return Ok(Vec::new());
    }
    let mut candidates: Vec<(&PracticeWord, Card)> = candidates_of(conn, words)?
        .into_iter()
        .filter(|(_, c)| {
            !reviewed_today.contains_key(&c.word_id) && !planned_words.contains(&c.word_id)
        })
        .collect();
    candidates.sort_by(|(a, _), (b, _)| {
        b.traits
            .usefulness
            .cmp(&a.traits.usefulness)
            .then(a.seq.cmp(&b.seq))
            .then(a.traits.id.cmp(&b.traits.id))
    });
    let introduced = scheduler::introduced_words_on(conn, day)?;
    let mut blockers: Vec<WordTraits> = catalog::traits(conn, &introduced)?.into_values().collect();
    let mut chosen = Vec::new();
    for (word, card) in candidates {
        if chosen.len() >= quota as usize {
            break;
        }
        if blockers.iter().any(|b| b.confusable_with(&word.traits)) {
            continue;
        }
        blockers.push(word.traits.clone());
        chosen.push(Planned {
            card: card.id,
            word: card.word_id,
            origin: ItemOrigin::New,
        });
    }
    Ok(chosen)
}

/// Step 5: due cards with a new card at every 4th position; when due cards
/// run out the rest of the new cards follow.
fn interleave(due: Vec<Planned>, new: Vec<Planned>, block_size: usize) -> Vec<Planned> {
    let mut due = due.into_iter();
    let mut new = new.into_iter();
    let mut out = Vec::with_capacity(block_size);
    while out.len() < block_size {
        let slot_for_new = out.len() % NEW_EVERY == NEW_EVERY - 1;
        let next = if slot_for_new {
            new.next().or_else(|| due.next())
        } else {
            due.next().or_else(|| new.next())
        };
        match next {
            Some(item) => out.push(item),
            None => break,
        }
    }
    out
}

/// The items of a new block (steps 1–5), without sentences.
fn plan_block(
    conn: &Connection,
    day: StudyDay,
    scope: Option<SourceId>,
    settings: &Settings,
) -> rusqlite::Result<Vec<Planned>> {
    let words = catalog::practice_words(conn, scope)?;
    let reviewed_today = scheduler::reviewed_cards_on(conn, day)?;
    let allowance = new_word_allowance(conn, day, settings)?;
    let typed = typed_allowance(conn, day, settings, &allowance)?;
    let due = plan_due(
        conn,
        day,
        SERVED_KINDS,
        &words,
        &reviewed_today,
        typed.quota,
    )?;
    let due_words: HashSet<WordId> = due.iter().map(|p| p.word).collect();
    let new = plan_new(
        conn,
        day,
        allowance.quota,
        &words,
        &reviewed_today,
        &due_words,
    )?;
    Ok(interleave(due, new, settings.block_size as usize))
}

/// Is anything left for a regular block in `scope` on `day`: a due review,
/// or a first-time Produce/Spelling card that today's cap still lets in?
fn anything_due(
    conn: &Connection,
    day: StudyDay,
    scope: Option<SourceId>,
    settings: &Settings,
) -> rusqlite::Result<bool> {
    if reviews_in_scope(conn, scope)?
        .iter()
        .any(|c| c.is_due_on(day))
    {
        return Ok(true);
    }
    let new = new_word_allowance(conn, day, settings)?;
    Ok(typed_allowance(conn, day, settings, &new)?.quota > 0
        && typed_waiting(conn, day, scope)? > 0)
}

/// Extra practice (R-63, design §6.9), offered only while nothing is due
/// in `scope` (due cards come first; an empty plan otherwise — first-time
/// Produce/Spelling cards count as due while today's cap lets them in):
/// cards already seen (at least one review) of the served kinds in
/// `scope`, one per word.
/// Cards whose latest rating today was 1–2 are left out (just shown, known);
/// cards rated 3–5 today come first, then the lowest current FSRS
/// retrievability. Not limited to a block yet.
fn plan_extra(
    conn: &Connection,
    day: StudyDay,
    scope: Option<SourceId>,
    settings: &Settings,
) -> rusqlite::Result<Vec<Planned>> {
    if anything_due(conn, day, scope, settings)? {
        return Ok(Vec::new());
    }
    let words = catalog::practice_words(conn, scope)?;
    let model = scheduler::model(conn)?;
    let scheduled = scheduled_of(conn, &words, SERVED_KINDS)?;
    let latest = scheduler::latest_grades_on(conn, day)?;
    let mut cards: Vec<(bool, f64, Card)> = scheduled
        .into_iter()
        .filter(|c| c.reps >= 1 && !c.is_due_on(day))
        .filter_map(|c| {
            let rated_today = match latest.get(&c.id) {
                Some(grade) if grade.get() <= 2 => return None,
                Some(_) => true,
                None => false,
            };
            let recall = model.retrievability(&c, day).unwrap_or(0.0);
            Some((rated_today, recall, c))
        })
        .collect();
    // Rated 3–5 today first, then lowest retrievability, then card id.
    cards.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.total_cmp(&b.1))
            .then(a.2.id.cmp(&b.2.id))
    });
    let mut seen_words = HashSet::new();
    Ok(cards
        .into_iter()
        .filter(|(_, _, card)| seen_words.insert(card.word_id))
        .map(|(_, _, card)| Planned {
            card: card.id,
            word: card.word_id,
            origin: ItemOrigin::Due,
        })
        .collect())
}

/// How many cards an extra-practice block (R-63) in `scope` would serve now
/// (at most a block).
pub fn extra_available(
    conn: &Connection,
    day: StudyDay,
    scope: Option<SourceId>,
    settings: &Settings,
) -> rusqlite::Result<u32> {
    let eligible = plan_extra(conn, day, scope, settings)?.len();
    Ok(count(eligible).min(settings.block_size))
}

/// "Learn more new words" (design §6.6): `count` new words in `scope` by
/// the usual candidate order and interference rule, regardless of today's
/// limit and of the backlog pause.
fn plan_more_new(
    conn: &Connection,
    day: StudyDay,
    scope: Option<SourceId>,
    count: u32,
) -> rusqlite::Result<Vec<Planned>> {
    let words = catalog::practice_words(conn, scope)?;
    let reviewed_today = scheduler::reviewed_cards_on(conn, day)?;
    plan_new(conn, day, count, &words, &reviewed_today, &HashSet::new())
}

/// The number of new words a `more_new` request asks for (validated).
fn more_new_count(request: &StartRequest, settings: &Settings) -> Result<u32, SessionError> {
    match (request.kind, request.count) {
        (StartKind::MoreNew, None) => Ok(settings.new_per_day.max(MORE_NEW_DEFAULT_MIN)),
        (StartKind::MoreNew, Some(n)) if (1..=MORE_NEW_MAX).contains(&n) => Ok(n),
        (StartKind::MoreNew, Some(_)) => Err(SessionError::Invalid(format!(
            "count must be 1-{MORE_NEW_MAX}"
        ))),
        (_, Some(_)) => Err(SessionError::Invalid(
            "count is only accepted with kind \"more_new\"".to_owned(),
        )),
        (_, None) => Ok(0),
    }
}

/// The kind, target size and items of the block `request` asks for.
fn plan_request(
    conn: &Connection,
    day: StudyDay,
    request: &StartRequest,
    settings: &Settings,
    more_new: u32,
) -> rusqlite::Result<(BlockKind, u32, Vec<Planned>)> {
    Ok(match request.kind {
        StartKind::Start | StartKind::Continue => (
            BlockKind::Regular,
            settings.block_size,
            plan_block(conn, day, request.scope, settings)?,
        ),
        StartKind::Extra => {
            let mut planned = plan_extra(conn, day, request.scope, settings)?;
            planned.truncate(settings.block_size as usize);
            (BlockKind::Extra, settings.block_size, planned)
        }
        StartKind::MoreNew => (
            BlockKind::MoreNew,
            more_new,
            plan_more_new(conn, day, request.scope, more_new)?,
        ),
    })
}

/// Mark active blocks of earlier study days as expired (design §6.7 step 8:
/// blocks expire at day rollover; their unanswered items are simply not
/// reviewed).
pub fn expire_stale(tx: &Transaction<'_>, day: StudyDay) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE practice_session SET status = 'expired' WHERE status = 'active' AND study_day != ?1",
        [day],
    )?;
    Ok(())
}

/// The format an item of `card` is shown in: the card's fixed format, or
/// the one its first review will fix given the word now (G2: stored with
/// the item so it cannot change before the rating).
pub fn shown_format(conn: &Connection, card: &Card) -> rusqlite::Result<Option<CardFormat>> {
    if let Some(format) = card.format {
        return Ok(Some(format));
    }
    Ok(catalog::word_traits(conn, card.word_id)?
        .map(|word| CardFormat::for_first_review(card.kind, &word)))
}

/// Start a block (design §6.7). An active block of today with unanswered
/// items is resumed instead (whatever scope was asked for); a fully
/// answered one is finished first. Returns no session when there is
/// nothing to practise.
///
/// The request is validated (unknown source → error) and "add new words
/// anyway" is recorded before a block is resumed, so neither is lost (G6,
/// G8).
///
/// A built block keeps its items (G4): its new items stay in it even if
/// `new_per_day` is lowered afterwards, so the day can end with more
/// introductions than the new limit. The limit only decides how many new
/// items a block gets when it is built.
///
/// `extra` and `more_new` blocks (R-63, §6.6) are resumed and stored the
/// same way; their ratings are ordinary reviews.
pub fn start_block(
    tx: &Transaction<'_>,
    now: &Moment,
    request: &StartRequest,
) -> Result<StartResult, SessionError> {
    let day = now.day;
    if let Some(source) = request.scope
        && !catalog::source_exists(tx, source)?
    {
        return Err(SessionError::UnknownSource);
    }
    let settings = settings::get(tx)?;
    let more_new = more_new_count(request, &settings)?;
    if request.add_new_anyway {
        settings::allow_new_words_anyway(tx, day)?;
    }
    expire_stale(tx, day)?;
    if let Some(active) = view::active_id(tx, day)? {
        if view::remaining(tx, &active)? > 0 {
            return Ok(StartResult {
                session: view::session_view(tx, &active, day)?,
                resumed: true,
            });
        }
        view::close_block(tx, &active, now)?;
    }
    let (kind, block_size, planned) = plan_request(tx, day, request, &settings, more_new)?;
    if planned.is_empty() {
        return Ok(StartResult {
            session: None,
            resumed: false,
        });
    }
    let id = SessionId::random()?;
    let block_no: i64 = tx.query_row(
        "SELECT COUNT(*) + 1 FROM practice_session WHERE study_day = ?1",
        [day],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO practice_session \
         (id, study_day, scope_source_id, block_no, block_size, status, started_at, kind) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?7)",
        params![
            id.as_str(),
            day,
            request.scope.map(SourceId::get),
            block_no,
            block_size,
            now.stamp(),
            kind.as_str()
        ],
    )?;
    for (ord, item) in (1u32..).zip(&planned) {
        let Some(card) = scheduler::card(tx, item.card)? else {
            continue;
        };
        let sentence = sentence_for(tx, item.word, day, None, request.scope)?;
        let format = shown_format(tx, &card)?;
        view::insert_item(
            tx,
            &id,
            f64::from(ord),
            item.card,
            item.origin,
            sentence,
            format,
        )?;
    }
    Ok(StartResult {
        session: view::session_view(tx, &id, day)?,
        resumed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Moment;
    use crate::db::{Db, DbError};
    use crate::importer::Via;
    use crate::scheduler::{RawGrade, ReviewInput};
    use crate::workflows;

    fn day(s: &str) -> StudyDay {
        s.parse().expect("day")
    }

    fn now() -> Moment {
        Moment::of(
            "2026-11-20T09:00:00Z".parse().expect("ts"),
            &crate::clock::DayRule::new("Europe/Prague", 4).expect("rule"),
        )
    }

    /// A database with the synthetic words `headwords` (usefulness 8, three
    /// sentences each) imported.
    async fn db_with(headwords: &[&str]) -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Db::open(dir.path()).expect("db");
        let words: Vec<serde_json::Value> = headwords
            .iter()
            .map(|h| {
                serde_json::json!({
                    "headword": h, "meaning": "Invented.", "czech": ["x"], "usefulness": 8,
                    "sentences": (1..=3).map(|i| serde_json::json!({
                        "en": format!("Line {i} [[{h}]]."), "cs": format!("Řádek {i} [[{h}]]."),
                        "origin": "example"
                    })).collect::<Vec<_>>()
                })
            })
            .collect();
        let pack = serde_json::json!({
            "format": "vocab-pack", "format_version": 1, "pack_id": "test-unit--2026-11-17--1",
            "source": { "id": "test-unit", "type": "book", "title": "Test" }, "words": words
        });
        workflows::commit_import(&db, &pack.to_string(), Via::Paste, now())
            .await
            .expect("import");
        (dir, db)
    }

    /// Give every word a due card of `kind` (state normally built up by
    /// reviews) and return the plan for `kinds`.
    async fn plan_with_due(
        db: &Db,
        kind: CardKind,
        kinds: &'static [CardKind],
        review_first_understand: bool,
    ) -> Vec<Planned> {
        let today = now();
        db.write(move |tx| {
            if review_first_understand {
                let card = scheduler::new_cards(tx)?.into_iter().next().expect("card");
                let word = catalog::word_traits(tx, card.word_id)?.expect("word");
                let input = ReviewInput {
                    client_review_id: "test-sibling",
                    card_id: card.id,
                    session_id: None,
                    raw_grade: RawGrade::new(2).expect("grade"),
                    suggested_grade: None,
                    typed_answer: None,
                    answer_match: None,
                    hint_used: false,
                    duration_ms: None,
                    client_ts: None,
                    sentence_id: None,
                    format: None,
                };
                scheduler::record_review(tx, &input, &word, &today).expect("review");
            }
            tx.execute(
                "INSERT OR IGNORE INTO card (word_id, kind, state, stability, difficulty, due_day, \
                 last_review_day, created_at) SELECT id, ?1, 'review', 5.0, 5.0, '2026-11-20', \
                 '2026-11-12', '2026-11-01T00:00:00Z' FROM word",
                [kind.as_str()],
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("setup");
        db.read(move |conn| {
            let words = catalog::practice_words(conn, None)?;
            let reviewed = scheduler::reviewed_cards_on(conn, today.day)?;
            Ok::<_, DbError>(plan_due(conn, today.day, kinds, &words, &reviewed, 100)?)
        })
        .await
        .expect("plan")
    }

    #[tokio::test]
    async fn r52_sibling_rule_one_card_per_word_per_study_day() {
        let (_dir, db) = db_with(&["alpha", "beta"]).await;
        // `alpha`'s Understand card was reviewed today; both words have a
        // Produce card due today.
        let plan = plan_with_due(
            &db,
            CardKind::Produce,
            &[CardKind::Understand, CardKind::Produce],
            true,
        )
        .await;
        let reviewed = db
            .read(|c| Ok::<_, DbError>(scheduler::reviewed_cards_on(c, day("2026-11-20"))?))
            .await
            .expect("reviewed");
        assert_eq!(reviewed.len(), 1);
        let alpha = *reviewed.keys().next().expect("alpha");
        assert_eq!(plan.len(), 1, "{plan:?}");
        assert_ne!(plan[0].word, alpha, "alpha already had a card today");
    }

    #[tokio::test]
    async fn r52_at_most_one_due_card_per_word_in_a_block() {
        let (_dir, db) = db_with(&["alpha", "beta"]).await;
        // Understand and Produce both due for each word.
        db.write(|tx| {
            tx.execute_batch(
                "UPDATE card SET state = 'review', stability = 5.0, difficulty = 5.0, \
                 due_day = '2026-11-20', last_review_day = '2026-11-12'",
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("setup");
        let plan = plan_with_due(
            &db,
            CardKind::Produce,
            &[CardKind::Understand, CardKind::Produce],
            false,
        )
        .await;
        assert_eq!(plan.len(), 2);
        assert_ne!(plan[0].word, plan[1].word);
    }

    #[tokio::test]
    async fn r58_at_most_three_spelling_cards_per_block() {
        let (_dir, db) = db_with(&["s1", "s2", "s3", "s4", "s5"]).await;
        let plan = plan_with_due(&db, CardKind::Spelling, &[CardKind::Spelling], false).await;
        assert_eq!(plan.len(), 3);
        // Understand-only plans (M3 serves Understand) never contain them.
        let served = plan_with_due(&db, CardKind::Spelling, SERVED_KINDS, false).await;
        assert!(served.is_empty());
    }

    fn item(n: i64, origin: ItemOrigin) -> Planned {
        Planned {
            card: CardId::new(n),
            word: WordId::new(n),
            origin,
        }
    }

    fn origins(items: &[Planned]) -> String {
        items
            .iter()
            .map(|p| match p.origin {
                ItemOrigin::Due => 'D',
                ItemOrigin::New => 'N',
                ItemOrigin::Repeat => 'R',
            })
            .collect()
    }

    fn dues(n: i64) -> Vec<Planned> {
        (1..=n).map(|i| item(i, ItemOrigin::Due)).collect()
    }

    fn news(n: i64) -> Vec<Planned> {
        (101..=100 + n).map(|i| item(i, ItemOrigin::New)).collect()
    }

    #[test]
    fn r54_new_cards_take_every_fourth_position() {
        let block = interleave(dues(30), news(5), 20);
        assert_eq!(origins(&block), "DDDNDDDNDDDNDDDNDDDN");
    }

    #[test]
    fn r54_when_due_cards_run_out_new_cards_fill_up_to_the_quota() {
        let block = interleave(dues(4), news(5), 20);
        assert_eq!(origins(&block), "DDDNDNNNN");
    }

    #[test]
    fn r54_when_new_cards_run_out_due_cards_fill_their_slots() {
        let block = interleave(dues(10), news(1), 20);
        assert_eq!(origins(&block), "DDDNDDDDDDD");
    }

    #[test]
    fn r54_block_is_shorter_when_both_run_out() {
        assert!(interleave(Vec::new(), Vec::new(), 20).is_empty());
        assert_eq!(interleave(dues(2), Vec::new(), 20).len(), 2);
    }

    #[test]
    fn r53_sentence_rotation_prefers_never_or_least_recently_shown() {
        let today = day("2026-11-20");
        let sentences = [(10, 0), (11, 1), (12, 2)];
        let mut shown = HashMap::new();
        // Nothing shown yet → lowest position.
        assert_eq!(choose_sentence(&sentences, &shown, today, None), Some(10));
        shown.insert(10, day("2026-11-19"));
        assert_eq!(choose_sentence(&sentences, &shown, today, None), Some(11));
        shown.insert(11, day("2026-11-01"));
        shown.insert(12, day("2026-11-05"));
        // All shown; 11 is the least recent.
        assert_eq!(choose_sentence(&sentences, &shown, today, None), Some(11));
    }

    #[test]
    fn r53_sentences_shown_in_the_last_seven_days_rest_when_possible() {
        let today = day("2026-11-20");
        let sentences = [(10, 0), (11, 1)];
        let mut shown = HashMap::new();
        shown.insert(10, day("2026-11-19"));
        shown.insert(11, day("2026-11-14"));
        // 11 was shown 6 days ago (inside the rest window) but is the least
        // recent; both are recent, so the least recent wins.
        assert_eq!(choose_sentence(&sentences, &shown, today, None), Some(11));
        // Exactly 7 days ago counts as rested.
        shown.insert(11, day("2026-11-13"));
        assert_eq!(choose_sentence(&sentences, &shown, today, None), Some(11));
    }

    #[test]
    fn r53_repeat_takes_a_different_sentence_when_one_exists() {
        let today = day("2026-11-20");
        let mut shown = HashMap::new();
        shown.insert(11, day("2026-11-01"));
        let sentences = [(10, 0), (11, 1)];
        assert_eq!(
            choose_sentence(&sentences, &shown, today, Some(10)),
            Some(11)
        );
        // Only one sentence: the repeat shows it again.
        assert_eq!(
            choose_sentence(&[(10, 0)], &shown, today, Some(10)),
            Some(10)
        );
        assert_eq!(choose_sentence(&[], &shown, today, None), None);
    }
}
