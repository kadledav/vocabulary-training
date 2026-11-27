//! The Words screens (design §7.1 screens 6 and 7; R-73, R-72 per-word
//! history): the word list with each word's learning status and next due
//! day, and one word with its cards and review history. Read-only; built
//! from the `catalog` and `scheduler` read functions, joined in memory (one
//! query per module for the whole list, never one per word).

use std::cmp::Ordering;
use std::collections::HashMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::StatsError;
use crate::catalog::{self, WordId, WordQuery, WordRow, WordView};
use crate::clock::StudyDay;
use crate::scheduler::{self, Card, CardKind, CardState, HistoryRow};

/// Understand stability (days) from which a word counts as familiar (the
/// Produce card unlocks there too, design §6.4).
const FAMILIAR_DAYS: f64 = 7.0;
/// Stability (days) from which a word counts as known ("mature", the leech
/// rule's threshold, design §6.5).
const KNOWN_DAYS: f64 = 21.0;

/// How far a word is (design §7.1: the status filter and sort of Words).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordStatus {
    /// Never practised.
    New,
    /// Failed last time, or remembered for less than a week.
    Learning,
    /// Remembered for a week or more.
    Familiar,
    /// Remembered for three weeks or more.
    Known,
    /// A problem word (R-55): flagged, or acknowledged and not yet known.
    Leech,
}

impl WordStatus {
    /// Order of the status sort: problem words first, unseen words last.
    fn rank(self) -> u8 {
        match self {
            Self::Leech => 0,
            Self::Learning => 1,
            Self::Familiar => 2,
            Self::Known => 3,
            Self::New => 4,
        }
    }
}

/// Is the word a problem word (R-55, design §6.5)? The one rule every
/// screen and the feedback export use: the word is practised (not
/// suspended) and one of its cards is a problem card (the scheduler owns
/// the card rule; a suspended card never is one).
pub fn is_problem_word(word_suspended: bool, cards: &[&Card]) -> bool {
    !word_suspended && cards.iter().any(|c| c.is_problem())
}

/// The problem words now, by id (Today's count, the Progress list, the
/// feedback export). Only the words with a leech flag are looked at.
pub fn problem_word_ids(conn: &Connection) -> Result<Vec<WordId>, StatsError> {
    let leech_cards = scheduler::leech_cards(conn)?;
    let mut by_word: HashMap<WordId, Vec<&Card>> = HashMap::new();
    for card in &leech_cards {
        by_word.entry(card.word_id).or_default().push(card);
    }
    let mut ids: Vec<WordId> = by_word.keys().copied().collect();
    ids.sort();
    let traits = catalog::traits(conn, &ids)?;
    ids.retain(|id| {
        traits
            .get(id)
            .is_some_and(|t| is_problem_word(t.suspended, &by_word[id]))
    });
    Ok(ids)
}

/// The status of a word from its cards (the Understand card's memory; a
/// problem card, unless the word is suspended).
pub fn word_status(word_suspended: bool, cards: &[&Card]) -> WordStatus {
    if is_problem_word(word_suspended, cards) {
        return WordStatus::Leech;
    }
    let Some(understand) = cards.iter().find(|c| c.kind == CardKind::Understand) else {
        return WordStatus::New;
    };
    let stability = understand.stability.unwrap_or(0.0);
    match understand.state {
        CardState::New => WordStatus::New,
        CardState::Relearning => WordStatus::Learning,
        CardState::Review if stability < FAMILIAR_DAYS => WordStatus::Learning,
        CardState::Review if stability < KNOWN_DAYS => WordStatus::Familiar,
        CardState::Review => WordStatus::Known,
    }
}

/// Can the owner produce the word: its Produce card has been passed and
/// is remembered for a week or more (stability ≥ 7 days)?
pub fn can_produce(cards: &[&Card]) -> bool {
    cards.iter().any(|c| {
        c.kind == CardKind::Produce
            && c.state == CardState::Review
            && c.stability.unwrap_or(0.0) >= FAMILIAR_DAYS
    })
}

/// The earliest due day of the word's cards that are practised (none
/// while the word is suspended: no card of it is practised, design §6.4).
fn next_due(word_suspended: bool, cards: &[&Card]) -> Option<StudyDay> {
    if word_suspended {
        return None;
    }
    cards
        .iter()
        .filter(|c| !c.suspended)
        .filter_map(|c| c.due_day)
        .min()
}

/// Order of the word list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordSort {
    /// By headword (the default without a source filter).
    #[default]
    Alpha,
    /// Most useful first.
    Usefulness,
    /// Most recently added first.
    Added,
    /// Soonest due first; words with nothing due last.
    Due,
    /// Problem words, learning, familiar, known, then new words.
    Status,
    /// The order within the filtered source (the default with a source
    /// filter; without one it is alphabetical).
    Source,
}

/// What the word list shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WordListQuery {
    pub filter: WordQuery,
    pub status: Option<WordStatus>,
    /// `None`: source order with a source filter, else alphabetical.
    pub sort: Option<WordSort>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WordListRow {
    #[serde(flatten)]
    pub word: WordRow,
    pub status: WordStatus,
    pub next_due: Option<StudyDay>,
}

fn alphabetical(a: &WordRow, b: &WordRow) -> Ordering {
    a.headword
        .to_lowercase()
        .cmp(&b.headword.to_lowercase())
        .then(a.id.cmp(&b.id))
}

/// The word list (design §7.1 screen 6).
pub fn word_list(conn: &Connection, query: &WordListQuery) -> Result<Vec<WordListRow>, StatsError> {
    let rows = catalog::words(conn, &query.filter)?;
    let cards = scheduler::all_cards(conn)?;
    let mut by_word: HashMap<WordId, Vec<&Card>> = HashMap::new();
    for card in &cards {
        by_word.entry(card.word_id).or_default().push(card);
    }
    let mut list: Vec<WordListRow> = rows
        .into_iter()
        .map(|word| {
            let cards = by_word.get(&word.id).map_or(&[][..], Vec::as_slice);
            WordListRow {
                status: word_status(word.suspended, cards),
                next_due: next_due(word.suspended, cards),
                word,
            }
        })
        .filter(|row| query.status.is_none_or(|status| row.status == status))
        .collect();
    let sort = query.sort.unwrap_or(if query.filter.source.is_some() {
        WordSort::Source
    } else {
        WordSort::Alpha
    });
    match sort {
        // The catalog's order: by source position, else by headword key.
        WordSort::Source => {}
        WordSort::Alpha => list.sort_by(|a, b| alphabetical(&a.word, &b.word)),
        WordSort::Usefulness => list.sort_by(|a, b| {
            b.word
                .usefulness
                .cmp(&a.word.usefulness)
                .then_with(|| alphabetical(&a.word, &b.word))
        }),
        WordSort::Added => list.sort_by(|a, b| {
            b.word
                .created_at
                .cmp(&a.word.created_at)
                .then(b.word.id.cmp(&a.word.id))
        }),
        WordSort::Due => list.sort_by(|a, b| {
            match (a.next_due, b.next_due) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
            .then_with(|| alphabetical(&a.word, &b.word))
        }),
        WordSort::Status => list.sort_by(|a, b| {
            a.status
                .rank()
                .cmp(&b.status.rank())
                .then_with(|| alphabetical(&a.word, &b.word))
        }),
    }
    Ok(list)
}

/// A card as the word detail shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CardView {
    #[serde(flatten)]
    pub card: Card,
    /// FSRS probability of recalling it today (`None` while new).
    pub recall: Option<f64>,
    /// The retention the scheduler aims at for this card (design §6.2).
    pub desired_retention: f32,
}

/// One word with everything the detail screen shows (design §7.1 screen 7).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WordDetail {
    #[serde(flatten)]
    pub word: WordView,
    pub status: WordStatus,
    /// `recognise` or `active`: the goal in force (design §6.3).
    pub effective_goal: &'static str,
    pub cards: Vec<CardView>,
    /// Reviews, newest first.
    pub history: Vec<HistoryRow>,
}

/// The word detail (`None`: no such word).
pub fn word_detail(
    conn: &Connection,
    day: StudyDay,
    id: WordId,
) -> Result<Option<WordDetail>, StatsError> {
    let (Some(word), Some(traits)) = (
        catalog::word_view(conn, id)?,
        catalog::word_traits(conn, id)?,
    ) else {
        return Ok(None);
    };
    let model = scheduler::model(conn)?;
    let cards = scheduler::cards_of_word(conn, id)?;
    let refs: Vec<&Card> = cards.iter().collect();
    let status = word_status(traits.suspended, &refs);
    let cards = cards
        .into_iter()
        .map(|card| CardView {
            recall: model.retrievability(&card, day),
            desired_retention: scheduler::desired_retention(card.kind, &traits),
            card,
        })
        .collect();
    Ok(Some(WordDetail {
        word,
        status,
        effective_goal: traits.effective_goal().as_str(),
        cards,
        history: scheduler::history(conn, id)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::{CardId, LeechState};

    fn card(kind: CardKind, state: CardState, stability: Option<f64>) -> Card {
        Card {
            id: CardId::new(1),
            word_id: WordId::new(1),
            kind,
            format: None,
            state,
            stability,
            difficulty: stability.map(|_| 5.0),
            due_day: None,
            last_review_day: None,
            last_review_at: None,
            introduced_day: None,
            reps: 0,
            lapses: 0,
            consecutive_fives: 0,
            leech: LeechState::None,
            leech_lapses_at_ack: 0,
            suspended: false,
        }
    }

    #[test]
    fn r73_status_follows_the_understand_card() {
        let status = |c: Card| word_status(false, &[&c]);
        assert_eq!(word_status(false, &[]), WordStatus::New);
        assert_eq!(
            status(card(CardKind::Understand, CardState::New, None)),
            WordStatus::New
        );
        assert_eq!(
            status(card(
                CardKind::Understand,
                CardState::Relearning,
                Some(30.0)
            )),
            WordStatus::Learning
        );
        assert_eq!(
            status(card(CardKind::Understand, CardState::Review, Some(6.9))),
            WordStatus::Learning
        );
        assert_eq!(
            status(card(CardKind::Understand, CardState::Review, Some(7.0))),
            WordStatus::Familiar
        );
        assert_eq!(
            status(card(CardKind::Understand, CardState::Review, Some(21.0))),
            WordStatus::Known
        );
    }

    #[test]
    fn r55_a_flag_on_any_card_makes_a_problem_word() {
        let understand = card(CardKind::Understand, CardState::Review, Some(40.0));
        let mut produce = card(CardKind::Produce, CardState::Review, Some(3.0));
        produce.leech = LeechState::Flagged;
        assert_eq!(
            word_status(false, &[&understand, &produce]),
            WordStatus::Leech
        );
        produce.leech = LeechState::Acknowledged;
        assert_eq!(
            word_status(false, &[&understand, &produce]),
            WordStatus::Leech
        );
        produce.stability = Some(21.0);
        assert_eq!(
            word_status(false, &[&understand, &produce]),
            WordStatus::Known
        );
    }

    #[test]
    fn r55_suspension_ends_the_problem_card_or_word_alike() {
        let understand = card(CardKind::Understand, CardState::Review, Some(40.0));
        let mut produce = card(CardKind::Produce, CardState::Review, Some(3.0));
        produce.leech = LeechState::Acknowledged;
        assert!(is_problem_word(false, &[&understand, &produce]));
        // "Recognise only", a goal switch or the spelling switch suspend
        // the card: the word shows by its remaining cards.
        produce.suspended = true;
        assert!(!is_problem_word(false, &[&understand, &produce]));
        assert_eq!(
            word_status(false, &[&understand, &produce]),
            WordStatus::Known
        );
        // Practised again: a problem again until it is remembered.
        produce.suspended = false;
        assert!(is_problem_word(false, &[&understand, &produce]));
        // A suspended word is no problem word, whatever its cards.
        produce.leech = LeechState::Flagged;
        assert!(!is_problem_word(true, &[&understand, &produce]));
        assert_eq!(
            word_status(true, &[&understand, &produce]),
            WordStatus::Known
        );
    }

    #[test]
    fn r73_next_due_skips_suspended_cards() {
        let mut a = card(CardKind::Understand, CardState::Review, Some(9.0));
        a.due_day = Some("2026-12-05".parse().expect("day"));
        let mut b = card(CardKind::Produce, CardState::Review, Some(9.0));
        b.due_day = Some("2026-12-01".parse().expect("day"));
        b.suspended = true;
        assert_eq!(next_due(false, &[&a, &b]), a.due_day);
        assert_eq!(next_due(true, &[&a, &b]), None, "a suspended word");
    }
}
