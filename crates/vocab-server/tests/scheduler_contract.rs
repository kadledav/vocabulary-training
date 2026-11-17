//! Contract tests of the scheduler written from the requirements and the
//! design (R-50, R-51, R-52, R-55 flagging, R-58, R-60, R-61; design §6.1 to
//! §6.5). Reviews go through `scheduler::record_review` with a fake clock.
//! All words are synthetic.

mod common;

use std::sync::atomic::{AtomicU32, Ordering};

use common::{Env, env, word};
use serde_json::json;
use vocab_server::catalog::WordId;
use vocab_server::db::DbError;
use vocab_server::scheduler::{
    self, Card, CardKind, CardState, LeechState, RawGrade, ReviewInput, ReviewOutcome,
};

static REVIEW_NO: AtomicU32 = AtomicU32::new(0);

async fn card_of(env: &Env, word: WordId, kind: CardKind) -> Option<Card> {
    env.db
        .read(move |conn| Ok::<_, DbError>(scheduler::cards_of_word(conn, word)?))
        .await
        .expect("cards")
        .into_iter()
        .find(|c| c.kind == kind)
}

async fn card(env: &Env, word: WordId, kind: CardKind) -> Card {
    card_of(env, word, kind).await.expect("card")
}

async fn review(env: &Env, word: WordId, kind: CardKind, raw: u8) -> ReviewOutcome {
    let card = card(env, word, kind).await.id;
    let traits = env.traits(word).await;
    let now = env.moment();
    let client_id = format!(
        "test-contract-{}",
        REVIEW_NO.fetch_add(1, Ordering::Relaxed)
    );
    env.db
        .write(move |tx| {
            let input = ReviewInput {
                client_review_id: &client_id,
                card_id: card,
                session_id: None,
                raw_grade: RawGrade::new(raw).expect("grade"),
                suggested_grade: None,
                typed_answer: None,
                answer_match: None,
                hint_used: false,
                duration_ms: None,
                client_ts: None,
                sentence_id: None,
                format: None,
            };
            Ok::<_, DbError>(
                scheduler::record_review(tx, &input, &traits, &now).expect("record review"),
            )
        })
        .await
        .expect("review")
}

async fn rate(env: &Env, word: WordId, raw: u8) -> ReviewOutcome {
    review(env, word, CardKind::Understand, raw).await
}

fn expected_interval(memory: Option<fsrs::MemoryState>, retention: f32, elapsed: u32) -> i32 {
    let next = fsrs::FSRS::default()
        .next_states(memory, retention, elapsed)
        .expect("fsrs")
        .good;
    next.interval.round().clamp(1.0, 36500.0) as i32
}

/// Give every Understand card the memory state of a card reviewed 15 days
/// ago and due today (stability 15, difficulty 5).
fn seed_mature(env: &Env) {
    let today = env.today();
    env.exec(&format!(
        "UPDATE card SET state = 'review', stability = 15.0, difficulty = 5.0, \
         due_day = '{today}', last_review_day = '{}', introduced_day = '{}', reps = 3, \
         format = 'sentence' WHERE kind = 'understand'",
        today.add_days(-15),
        today.add_days(-40),
    ));
}

async fn interval_after(env: &Env, headword: &str, raw: u8) -> i64 {
    let id = env.word_id(headword).await;
    let today = env.today();
    let outcome = rate(env, id, raw).await;
    i64::from(today.days_between(outcome.card.due_day.expect("due")))
}

// ── R-50: multi-day simulation ──────────────────────────────────────────────

#[tokio::test]
async fn r50_intervals_grow_for_good_ratings_over_sixty_days() {
    let env = env();
    env.import("test-book", vec![word("alpha", 10, 3)]).await;
    let id = env.word_id("alpha").await;
    let start = env.today();
    let mut intervals = Vec::new();
    for offset in 0..60 {
        env.set_day(&start.add_days(offset).to_string());
        let c = card(&env, id, CardKind::Understand).await;
        if c.due_day.is_none_or(|d| d <= env.today()) {
            let out = rate(&env, id, 2).await;
            intervals.push(env.today().days_between(out.card.due_day.expect("due")));
        }
    }
    assert!(intervals.len() >= 3, "{intervals:?}");
    assert!(intervals.iter().all(|i| *i >= 1));
    assert!(
        intervals.windows(2).all(|w| w[1] > w[0]),
        "good ratings must widen intervals: {intervals:?}"
    );
}

#[tokio::test]
async fn r50_a_failure_in_the_simulation_shrinks_the_next_interval() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3)]).await;
    let id = env.word_id("alpha").await;
    let start = env.today();
    let mut last_good = 0;
    let mut before_stability = 0.0;
    for offset in 0..60 {
        env.set_day(&start.add_days(offset).to_string());
        let c = card(&env, id, CardKind::Understand).await;
        if c.due_day.is_none_or(|d| d <= env.today()) {
            if last_good >= 8 {
                // Established card: fail it, then pass the repeat.
                let failed = rate(&env, id, 4).await;
                assert_eq!(failed.card.state, CardState::Relearning);
                assert_eq!(failed.card.due_day, Some(env.today()));
                assert!(failed.card.stability.expect("s") < before_stability);
                let again = rate(&env, id, 2).await;
                let after = env.today().days_between(again.card.due_day.expect("due"));
                assert!(after < last_good, "{after} vs {last_good}");
                return;
            }
            let out = rate(&env, id, 2).await;
            before_stability = out.card.stability.expect("s");
            last_good = env.today().days_between(out.card.due_day.expect("due"));
        }
    }
    panic!("never reached an interval of 8 days");
}

#[tokio::test]
async fn r50_each_card_has_its_own_memory_state() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3), word("beta", 5, 3)])
        .await;
    let a = env.word_id("alpha").await;
    let b = env.word_id("beta").await;
    let beta_before = env.rows("card", &format!("word_id = {}", b.get()));
    rate(&env, a, 3).await;
    assert_eq!(
        env.rows("card", &format!("word_id = {}", b.get())),
        beta_before
    );
    assert_eq!(
        env.int(&format!(
            "SELECT COUNT(*) FROM review_log WHERE card_id IN (SELECT id FROM card WHERE word_id = {})",
            b.get()
        )),
        0
    );
}

// ── R-60: 1 to 5 mapping ────────────────────────────────────────────────────

#[tokio::test]
async fn r60_hard_gives_a_shorter_interval_than_good_and_one_equals_good() {
    let env = env();
    let words = ["g1", "g2", "hard", "one"].map(|h| word(h, 5, 3));
    env.import("test-book", words.to_vec()).await;
    seed_mature(&env);
    let good = interval_after(&env, "g1", 2).await;
    let good2 = interval_after(&env, "g2", 2).await;
    let hard = interval_after(&env, "hard", 3).await;
    let one = interval_after(&env, "one", 1).await;
    assert_eq!(good, good2);
    assert!(hard < good, "hard {hard} vs good {good}");
    assert_eq!(one, good, "raw 1 is Good until parameters are fitted");
    let g = card(&env, env.word_id("g1").await, CardKind::Understand).await;
    let o = card(&env, env.word_id("one").await, CardKind::Understand).await;
    assert_eq!(
        (g.stability, g.difficulty, g.due_day),
        (o.stability, o.difficulty, o.due_day)
    );
}

#[tokio::test]
async fn r60_four_and_five_reset_to_relearning_due_today() {
    let env = env();
    env.import("test-book", vec![word("v", 5, 3), word("n", 5, 3)])
        .await;
    seed_mature(&env);
    for (headword, raw) in [("v", 4), ("n", 5)] {
        let id = env.word_id(headword).await;
        let out = rate(&env, id, raw).await;
        assert_eq!(out.card.state, CardState::Relearning, "raw {raw}");
        assert_eq!(out.card.due_day, Some(env.today()), "raw {raw}");
        assert_eq!(out.card.lapses, 1);
        assert!(out.card.stability.expect("s") < 15.0, "raw {raw}");
        assert_eq!(out.fsrs_rating, 1);
    }
}

// ── R-61: raw grade and applied rating for every review ─────────────────────

#[tokio::test]
async fn r61_every_review_of_a_simulation_keeps_raw_grade_and_applied_rating() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3)]).await;
    let id = env.word_id("alpha").await;
    let start = env.today();
    let grades: [u8; 12] = [1, 2, 3, 5, 2, 4, 2, 1, 3, 2, 5, 2];
    let mut given = Vec::new();
    let mut next = 0;
    for offset in 0..120 {
        env.set_day(&start.add_days(offset).to_string());
        while next < grades.len() {
            let c = card(&env, id, CardKind::Understand).await;
            if c.due_day.is_some_and(|d| d > env.today()) {
                break;
            }
            rate(&env, id, grades[next]).await;
            given.push(grades[next]);
            next += 1;
            // After a 4/5 the card is due again today: one more attempt is
            // allowed per day by this loop, as in a repeat.
        }
    }
    assert_eq!(given.len(), grades.len(), "all grades were given");
    let conn = env.sql();
    let mut stmt = conn
        .prepare("SELECT raw_grade, fsrs_rating, undone FROM review_log ORDER BY id")
        .expect("select");
    let rows: Vec<(u8, u8, u8)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(rows.len(), grades.len());
    for ((raw, applied, undone), expected) in rows.iter().zip(grades) {
        assert_eq!(*raw, expected);
        let rating = match expected {
            1 | 2 => 3,
            3 => 2,
            _ => 1,
        };
        assert_eq!(*applied, rating, "raw {expected}");
        assert_eq!(*undone, 0);
    }
}

// ── R-51: retention by usefulness ───────────────────────────────────────────

#[tokio::test]
async fn r51_desired_retention_follows_the_usefulness_bands() {
    let env = env();
    let words: Vec<_> = (1..=10).map(|u| word(&format!("u{u}"), u, 3)).collect();
    env.import("test-book", words).await;
    seed_mature(&env);
    let memory = Some(fsrs::MemoryState {
        stability: 15.0,
        difficulty: 5.0,
    });
    let mut intervals = Vec::new();
    for u in 1..=10u8 {
        intervals.push(interval_after(&env, &format!("u{u}"), 2).await);
    }
    // Design §6.2: 1-3 -> 0.82, 4-5 -> 0.87, 6-7 -> 0.90, 8-10 -> 0.93.
    let bands = [(0..3, 0.82_f32), (3..5, 0.87), (5..7, 0.90), (7..10, 0.93)];
    for (range, retention) in bands {
        let expected = i64::from(expected_interval(memory, retention, 15));
        for i in range {
            assert_eq!(
                intervals[i],
                expected,
                "usefulness {} -> {retention}",
                i + 1
            );
        }
    }
    assert!(
        intervals[8] < intervals[1],
        "ux 9 must come back sooner than ux 2"
    );
}

#[tokio::test]
async fn r51_included_low_priority_words_use_the_lowest_retention_and_stay_recognise() {
    let env = env();
    let mut minor = word("minor", 9, 3);
    minor["tag"] = json!({ "kind": "low-priority" });
    minor["goal"] = json!("active");
    env.import("test-book", vec![minor, word("lowux", 2, 3)])
        .await;
    env.exec("UPDATE word SET included = 1 WHERE headword = 'minor'");
    seed_mature(&env);
    let minor_interval = interval_after(&env, "minor", 2).await;
    let low_interval = interval_after(&env, "lowux", 2).await;
    assert_eq!(minor_interval, low_interval, "retention 0.82 for both");
    // Never a Produce card, even when the pack says active (R-51, design §6.3).
    let id = env.word_id("minor").await;
    for _ in 0..8 {
        let due = card(&env, id, CardKind::Understand)
            .await
            .due_day
            .expect("due");
        env.set_day(&due.to_string());
        rate(&env, id, 2).await;
    }
    assert!(
        card(&env, id, CardKind::Understand)
            .await
            .stability
            .expect("s")
            >= 7.0
    );
    assert!(card_of(&env, id, CardKind::Produce).await.is_none());
}

// ── R-52: Produce goal rules ────────────────────────────────────────────────

#[tokio::test]
async fn r52_produce_follows_the_effective_goal() {
    let env = env();
    let mut recognise = word("rec", 9, 3);
    recognise["goal"] = json!("recognise");
    let mut active_low = word("actlow", 2, 3);
    active_low["goal"] = json!("active");
    env.import(
        "test-book",
        vec![
            recognise,
            active_low,
            word("dflt6", 6, 3),
            word("dflt5", 5, 3),
        ],
    )
    .await;
    let mut ids = Vec::new();
    for h in ["rec", "actlow", "dflt6", "dflt5"] {
        ids.push((h, env.word_id(h).await));
    }
    for _ in 0..10 {
        for (_, id) in &ids {
            let c = card(&env, *id, CardKind::Understand).await;
            if c.due_day.is_some_and(|due| due > env.today()) {
                continue;
            }
            rate(&env, *id, 2).await;
        }
        let next = ids
            .iter()
            .filter_map(|(_, id)| futures_next_due(&env, *id))
            .min();
        match next {
            Some(day) => env.set_day(&day),
            None => break,
        }
    }
    for (h, id) in &ids {
        assert!(
            card(&env, *id, CardKind::Understand)
                .await
                .stability
                .expect("s")
                >= 7.0,
            "{h} reached stability 7"
        );
        let expected = matches!(*h, "actlow" | "dflt6");
        assert_eq!(
            card_of(&env, *id, CardKind::Produce).await.is_some(),
            expected,
            "{h}"
        );
    }
}

/// Next due day of the Understand card (direct SQL; sync helper).
fn futures_next_due(env: &Env, id: WordId) -> Option<String> {
    env.sql()
        .query_row(
            "SELECT due_day FROM card WHERE word_id = ?1 AND kind = 'understand'",
            [id.get()],
            |r| r.get(0),
        )
        .ok()
}

#[tokio::test]
async fn r52_produce_and_spelling_cards_are_scheduled_independently() {
    let env = env();
    let mut keen = word("keen", 9, 3);
    keen["spelling"] = json!(true);
    env.import("test-book", vec![keen]).await;
    let id = env.word_id("keen").await;
    for _ in 0..12 {
        if card_of(&env, id, CardKind::Produce).await.is_some()
            && card_of(&env, id, CardKind::Spelling).await.is_some()
        {
            break;
        }
        if let Some(due) = card(&env, id, CardKind::Understand).await.due_day {
            env.set_day(&due.to_string());
        }
        rate(&env, id, 2).await;
    }
    let understand_before = env.rows("card", "kind = 'understand'");
    let spelling = card(&env, id, CardKind::Spelling).await;
    let produce = card(&env, id, CardKind::Produce).await;
    env.set_day(&spelling.due_day.expect("due").to_string());
    // Spelling retention 0.80 regardless of usefulness (design §6.2).
    let out = review(&env, id, CardKind::Spelling, 2).await;
    let days = expected_interval(None, 0.80, 0);
    assert_eq!(
        out.card.due_day,
        Some(env.today().add_days(days)),
        "spelling retention 0.80"
    );
    assert_eq!(env.rows("card", "kind = 'understand'"), understand_before);
    let produce_after = card(&env, id, CardKind::Produce).await;
    assert_eq!(produce_after.due_day, produce.due_day);
    assert_eq!(produce_after.state, CardState::New);
}

// ── R-55: leech flagging edge cases ─────────────────────────────────────────

async fn seed_one(env: &Env, headword: &str, stability: f64) -> WordId {
    let id = env.word_id(headword).await;
    // Reviewed on an earlier day (M3 fix G5: the two-5s rule looks at the
    // first rating of the previous review day).
    env.set_day("2026-11-01");
    rate(env, id, 2).await;
    env.set_day("2026-11-20");
    env.sql()
        .execute(
            "UPDATE card SET stability = ?2, last_review_day = '2026-10-01', due_day = '2026-11-20' \
             WHERE word_id = ?1",
            rusqlite::params![id.get(), stability],
        )
        .expect("seed");
    id
}

#[tokio::test]
async fn r55_maturity_boundary_is_stability_twenty_one() {
    let env = env();
    env.import("test-book", vec![word("edge", 5, 3), word("below", 5, 3)])
        .await;
    for (h, stability, flagged) in [("edge", 21.0, true), ("below", 20.9, false)] {
        let id = seed_one(&env, h, stability).await;
        rate(&env, id, 5).await;
        // G5: the second 5 must be on a later study day.
        env.set_day("2026-11-21");
        let out = rate(&env, id, 5).await;
        assert_eq!(out.card.leech == LeechState::Flagged, flagged, "{h}");
    }
}

#[tokio::test]
async fn r55_a_pass_between_two_fives_or_a_four_does_not_count_as_two_fives() {
    let env = env();
    env.import("test-book", vec![word("gap", 5, 3), word("mixed", 5, 3)])
        .await;
    let gap = seed_one(&env, "gap", 40.0).await;
    rate(&env, gap, 5).await;
    rate(&env, gap, 2).await;
    let out = rate(&env, gap, 5).await;
    assert_eq!(out.card.leech, LeechState::None, "5, 2, 5");
    let mixed = seed_one(&env, "mixed", 40.0).await;
    rate(&env, mixed, 4).await;
    let out = rate(&env, mixed, 5).await;
    assert_eq!(out.card.leech, LeechState::None, "4, 5");
}

#[tokio::test]
async fn r55_lapses_from_fours_and_fives_both_count_and_a_new_card_has_none() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3), word("fresh", 5, 3)])
        .await;
    // A first rating of 5 on a new card is not a lapse.
    let fresh = env.word_id("fresh").await;
    let out = rate(&env, fresh, 5).await;
    assert_eq!(out.card.lapses, 0);
    let id = env.word_id("alpha").await;
    rate(&env, id, 2).await;
    for (n, raw) in [4u8, 5, 4, 5].into_iter().enumerate() {
        let due = card(&env, id, CardKind::Understand)
            .await
            .due_day
            .expect("due");
        env.set_day(&due.to_string());
        let out = rate(&env, id, raw).await;
        assert_eq!(out.card.lapses as usize, n + 1);
        rate(&env, id, 2).await;
        let flagged = card(&env, id, CardKind::Understand).await.leech == LeechState::Flagged;
        assert_eq!(flagged, n == 3, "after {} lapses", n + 1);
    }
}

// ── R-58: spelling unlock ───────────────────────────────────────────────────

#[tokio::test]
async fn r58_spelling_needs_a_pass_of_three_or_better_on_a_later_day() {
    let env = env();
    let mut tricky = word("tricky", 4, 3);
    tricky["spelling"] = json!(true);
    env.import("test-book", vec![tricky]).await;
    let id = env.word_id("tricky").await;
    // A pass on the introduction day does not unlock.
    rate(&env, id, 3).await;
    assert!(card_of(&env, id, CardKind::Spelling).await.is_none());
    // A 4 on a later day does not unlock either.
    let due = card(&env, id, CardKind::Understand)
        .await
        .due_day
        .expect("due");
    env.set_day(&due.to_string());
    let out = rate(&env, id, 4).await;
    assert!(out.unlocked.is_empty());
    assert!(card_of(&env, id, CardKind::Spelling).await.is_none());
}
