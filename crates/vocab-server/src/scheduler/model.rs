//! The FSRS model: parameters, the 1–5 → FSRS mapping, desired retention
//! and retrievability (design §6.1, §6.2, §6.7 step 2).

use fsrs::{DEFAULT_PARAMETERS, FSRS, FSRS6_DEFAULT_DECAY, MemoryState, NextStates};
use rusqlite::{Connection, OptionalExtension};

use super::review::RawGrade;
use super::{Card, CardKind, SchedulerError};
use crate::catalog::WordTraits;
use crate::clock::StudyDay;

/// Longest interval (days).
const MAX_INTERVAL_DAYS: f32 = 36500.0;

/// FSRS rating (1 = Again … 4 = Easy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rating {
    Again = 1,
    Hard = 2,
    Good = 3,
    Easy = 4,
}

impl Rating {
    pub fn number(self) -> u8 {
        self as u8
    }

    fn pick(self, states: &NextStates) -> &fsrs::ItemState {
        match self {
            Self::Again => &states.again,
            Self::Hard => &states.hard,
            Self::Good => &states.good,
            Self::Easy => &states.easy,
        }
    }
}

/// The scheduler's FSRS parameters: personal ones once fitted (M12),
/// otherwise the library defaults.
#[derive(Debug, Clone)]
pub struct Model {
    fsrs: FSRS,
    decay: f32,
    personal: bool,
}

/// Load the active parameter set (`fsrs_params.active = 1`). An unusable
/// stored set falls back to the defaults (logged), so practice never stops.
pub fn model(conn: &Connection) -> rusqlite::Result<Model> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT params_json FROM fsrs_params WHERE active = 1 ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(json) = stored {
        match serde_json::from_str::<Vec<f32>>(&json)
            .map_err(|e| e.to_string())
            .and_then(|p| Model::with_parameters(&p, true).map_err(|e| e.to_string()))
        {
            Ok(model) => return Ok(model),
            Err(err) => {
                tracing::warn!(error = %err, "active FSRS parameters unusable; using defaults");
            }
        }
    }
    Ok(Model::default_parameters())
}

impl Model {
    pub fn default_parameters() -> Self {
        Self {
            fsrs: FSRS::default(),
            decay: DEFAULT_PARAMETERS
                .get(20)
                .copied()
                .unwrap_or(FSRS6_DEFAULT_DECAY),
            personal: false,
        }
    }

    fn with_parameters(params: &[f32], personal: bool) -> Result<Self, SchedulerError> {
        let fsrs = FSRS::new(params).map_err(|e| SchedulerError::Fsrs(e.to_string()))?;
        Ok(Self {
            fsrs,
            decay: params.get(20).copied().unwrap_or(FSRS6_DEFAULT_DECAY),
            personal,
        })
    }

    /// Personal parameters are active (R-60: then raw 1 → Easy).
    pub fn personal(&self) -> bool {
        self.personal
    }

    /// R-60 / design §6.1 mapping of the owner's 1–5 grade.
    pub fn rating(&self, raw: RawGrade) -> Rating {
        match raw.get() {
            1 if self.personal => Rating::Easy,
            1 | 2 => Rating::Good,
            3 => Rating::Hard,
            _ => Rating::Again,
        }
    }

    /// The memory state after rating `rating`, and the interval in whole
    /// days (rounded, clamped to 1–36500; design §6.1).
    pub fn next(
        &self,
        memory: Option<MemoryState>,
        desired_retention: f32,
        days_elapsed: u32,
        rating: Rating,
    ) -> Result<(MemoryState, i32), SchedulerError> {
        let states = self
            .fsrs
            .next_states(memory, desired_retention, days_elapsed)
            .map_err(|e| SchedulerError::Fsrs(e.to_string()))?;
        let next = rating.pick(&states);
        let days = next.interval.round().clamp(1.0, MAX_INTERVAL_DAYS);
        // Exact: a whole number in 1..=36500.
        Ok((next.memory, days as i32))
    }

    /// FSRS retrievability of a reviewed card on `day` (`None` while new).
    pub fn retrievability(&self, card: &Card, day: StudyDay) -> Option<f64> {
        let memory = card.memory()?;
        let last = card.last_review_day?;
        let elapsed = last.days_between(day).max(0);
        // Study-day counts are small; f32 holds them exactly.
        let r = fsrs::current_retrievability(memory, elapsed as f32, self.decay);
        Some(f64::from(r))
    }

    /// Due-card priority (design §6.7 step 2): `(1 − R_now / R_target) × w(u)`
    /// with `w(u) = 0.5 + u/10`. A card without a memory state yet (an
    /// unlocked Produce/Spelling card waiting for its first review) gets the
    /// neutral priority 0, as if `R_now = R_target` (design §17 M8): it is
    /// not forgotten, it was never learnt in this direction, so it must not
    /// push ahead of real reviews.
    pub fn priority(&self, card: &Card, word: &WordTraits, day: StudyDay) -> f64 {
        let target = f64::from(desired_retention(card.kind, word));
        let Some(now) = self.retrievability(card, day) else {
            return 0.0;
        };
        // Usefulness is 1..=10; f64 holds it exactly.
        let weight = 0.5 + word.usefulness as f64 / 10.0;
        (1.0 - now / target) * weight
    }
}

/// Desired retention per card (design §6.2, R-51, R-58). Read at review
/// time, so a usefulness change affects the next interval only.
pub fn desired_retention(kind: CardKind, word: &WordTraits) -> f32 {
    if kind == CardKind::Spelling {
        return 0.80;
    }
    if word.included_low_priority() {
        return 0.82;
    }
    match word.usefulness {
        8.. => 0.93,
        6..=7 => 0.90,
        4..=5 => 0.87,
        _ => 0.82,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::WordId;

    fn word(usefulness: i64) -> WordTraits {
        WordTraits {
            id: WordId::new(1),
            headword_item_key: "w".to_owned(),
            usefulness,
            goal: None,
            low_priority: false,
            included: false,
            spelling: false,
            suspended: false,
            visible_sentences: 3,
            confusable_keys: Vec::new(),
        }
    }

    fn raw(n: u8) -> RawGrade {
        RawGrade::new(n).expect("1-5")
    }

    #[test]
    fn r60_one_is_good_until_personal_parameters_then_easy() {
        let default = Model::default_parameters();
        assert!(!default.personal());
        assert_eq!(default.rating(raw(1)), Rating::Good);
        assert_eq!(default.rating(raw(2)), Rating::Good);
        assert_eq!(default.rating(raw(3)), Rating::Hard);
        assert_eq!(default.rating(raw(4)), Rating::Again);
        assert_eq!(default.rating(raw(5)), Rating::Again);
        let personal = Model::with_parameters(&DEFAULT_PARAMETERS, true).expect("params");
        assert_eq!(personal.rating(raw(1)), Rating::Easy);
        assert_eq!(personal.rating(raw(2)), Rating::Good);
    }

    #[test]
    fn r51_retention_table() {
        for (u, r) in [
            (10, 0.93),
            (8, 0.93),
            (7, 0.90),
            (6, 0.90),
            (5, 0.87),
            (4, 0.87),
            (3, 0.82),
            (1, 0.82),
        ] {
            assert_eq!(
                desired_retention(CardKind::Understand, &word(u)),
                r,
                "u={u}"
            );
            assert_eq!(desired_retention(CardKind::Produce, &word(u)), r, "u={u}");
        }
    }

    #[test]
    fn r58_spelling_retention_is_low_for_any_usefulness() {
        assert_eq!(desired_retention(CardKind::Spelling, &word(10)), 0.80);
        assert_eq!(desired_retention(CardKind::Spelling, &word(1)), 0.80);
    }

    #[test]
    fn r51_included_low_priority_word_gets_the_lowest_retention() {
        let mut w = word(10);
        w.low_priority = true;
        w.included = true;
        assert_eq!(desired_retention(CardKind::Understand, &w), 0.82);
    }

    #[test]
    fn r50_intervals_are_whole_days_clamped_and_deterministic() {
        let m = Model::default_parameters();
        let (mem, days) = m.next(None, 0.9, 0, Rating::Again).expect("next");
        // Again on a new card: stability 0.212 → interval clamped up to 1.
        assert_eq!(days, 1);
        assert!(mem.stability < 1.0);
        let (_, good) = m.next(None, 0.9, 0, Rating::Good).expect("next");
        let (_, good_again) = m.next(None, 0.9, 0, Rating::Good).expect("next");
        assert_eq!(good, good_again, "no fuzz");
        // Good on a new card at R=0.9: interval = initial stability 2.3065 → 2.
        assert_eq!(good, 2);
        let huge = MemoryState {
            stability: 1.0e6,
            difficulty: 1.0,
        };
        let (_, max) = m.next(Some(huge), 0.8, 1000, Rating::Easy).expect("next");
        assert_eq!(max, 36500);
    }

    #[test]
    fn r52_a_card_without_memory_has_the_neutral_priority() {
        use crate::scheduler::{CardFormat, CardId, CardState, LeechState};
        let day: StudyDay = "2026-11-20".parse().expect("day");
        let mut card = Card {
            id: CardId::new(1),
            word_id: WordId::new(1),
            kind: CardKind::Produce,
            format: None,
            state: CardState::New,
            stability: None,
            difficulty: None,
            due_day: Some(day),
            last_review_day: None,
            last_review_at: None,
            introduced_day: None,
            reps: 0,
            lapses: 0,
            consecutive_fives: 0,
            leech: LeechState::None,
            leech_lapses_at_ack: 0,
            suspended: false,
        };
        let m = Model::default_parameters();
        assert_eq!(m.priority(&card, &word(10), day), 0.0);
        // An overdue reviewed card ranks above it.
        card.format = Some(CardFormat::Cloze);
        card.state = CardState::Review;
        card.stability = Some(5.0);
        card.difficulty = Some(5.0);
        card.last_review_day = Some("2026-10-20".parse().expect("day"));
        assert!(m.priority(&card, &word(10), day) > 0.0);
    }

    #[test]
    fn r50_higher_retention_gives_shorter_intervals() {
        let m = Model::default_parameters();
        let mem = MemoryState {
            stability: 20.0,
            difficulty: 5.0,
        };
        let (_, low) = m.next(Some(mem), 0.82, 20, Rating::Good).expect("next");
        let (_, high) = m.next(Some(mem), 0.93, 20, Rating::Good).expect("next");
        assert!(high < low, "0.93 → {high} days, 0.82 → {low} days");
    }
}
