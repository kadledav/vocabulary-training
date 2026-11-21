//! Scheduler rules with a fake clock (design §6.1–§6.5; R-50, R-51, R-52,
//! R-55 flagging, R-58 unlock, R-60, R-61). Words come in through the real
//! import; reviews go through `scheduler::record_review`.

mod common;

use std::sync::atomic::{AtomicU32, Ordering};

use common::{Env, day, env, word};
use serde_json::json;
use vocab_server::catalog::WordId;
use vocab_server::db::DbError;
use vocab_server::scheduler::{
    self, Card, CardFormat, CardKind, CardState, LeechState, RawGrade, ReviewInput, ReviewOutcome,
};
use vocab_server::workflows;

static REVIEW_NO: AtomicU32 = AtomicU32::new(0);

async fn cards(env: &Env, word: WordId) -> Vec<Card> {
    env.db
        .read(move |conn| Ok::<_, DbError>(scheduler::cards_of_word(conn, word)?))
        .await
        .expect("cards")
}

async fn card_of(env: &Env, word: WordId, kind: CardKind) -> Option<Card> {
    cards(env, word).await.into_iter().find(|c| c.kind == kind)
}

async fn understand(env: &Env, word: WordId) -> Card {
    card_of(env, word, CardKind::Understand)
        .await
        .expect("understand card")
}

/// Rate `card` of `word` with `raw` now.
async fn review(env: &Env, word: WordId, kind: CardKind, raw: u8) -> ReviewOutcome {
    let card = card_of(env, word, kind).await.expect("card").id;
    let traits = env.traits(word).await;
    let now = env.moment();
    let client_id = format!("test-review-{}", REVIEW_NO.fetch_add(1, Ordering::Relaxed));
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
                duration_ms: Some(1200),
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

async fn rate_understand(env: &Env, word: WordId, raw: u8) -> ReviewOutcome {
    review(env, word, CardKind::Understand, raw).await
}

async fn ensure(env: &Env, word: WordId) -> usize {
    let traits = env.traits(word).await;
    let now = env.moment();
    env.db
        .write(move |tx| Ok::<_, DbError>(scheduler::ensure_cards(tx, &traits, &now)?.len()))
        .await
        .expect("ensure")
}

/// Review the Understand card with `raw` on each due day until `until`
/// holds (at most 12 reviews); returns the outcomes.
async fn review_until(
    env: &Env,
    word: WordId,
    raw: u8,
    until: impl Fn(&ReviewOutcome) -> bool,
) -> Vec<ReviewOutcome> {
    let mut outcomes = Vec::new();
    for _ in 0..12 {
        let card = understand(env, word).await;
        if let Some(due) = card.due_day {
            env.set_day(&due.to_string());
        }
        let outcome = rate_understand(env, word, raw).await;
        let done = until(&outcome);
        outcomes.push(outcome);
        if done {
            return outcomes;
        }
    }
    panic!("condition not reached after 12 reviews");
}

/// Review the Understand card with 2 on each due day until both Produce
/// and Spelling exist.
async fn review_until_unlocked(env: &Env, id: WordId) {
    for _ in 0..12 {
        if card_of(env, id, CardKind::Produce).await.is_some()
            && card_of(env, id, CardKind::Spelling).await.is_some()
        {
            return;
        }
        if let Some(due) = understand(env, id).await.due_day {
            env.set_day(&due.to_string());
        }
        rate_understand(env, id, 2).await;
    }
    panic!("Produce and Spelling were not unlocked");
}

// ── card creation (design §6.4) ─────────────────────────────────────────────

#[tokio::test]
async fn r52_import_creates_one_new_understand_card_per_word() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3), word("beta", 8, 0)])
        .await;
    for headword in ["alpha", "beta"] {
        let id = env.word_id(headword).await;
        let cards = cards(&env, id).await;
        assert_eq!(cards.len(), 1, "{headword}");
        let card = &cards[0];
        assert_eq!(card.kind, CardKind::Understand);
        assert_eq!(card.state, CardState::New);
        assert_eq!(card.due_day, None);
        assert_eq!(card.format, None);
        assert_eq!((card.reps, card.lapses), (0, 0));
    }
    // Re-importing (merge) never adds a second Understand card.
    env.import("test-other", vec![word("alpha", 5, 4)]).await;
    assert_eq!(cards(&env, env.word_id("alpha").await).await.len(), 1);
}

#[tokio::test]
async fn r52_backfill_creates_missing_cards_and_is_idempotent() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3), word("beta", 6, 3)])
        .await;
    // A database from before cards existed.
    env.exec("DELETE FROM card");
    assert_eq!(env.int("SELECT COUNT(*) FROM card"), 0);
    let created = workflows::ensure_all_cards(&env.db, env.moment())
        .await
        .expect("backfill");
    assert_eq!(created, 2);
    let again = workflows::ensure_all_cards(&env.db, env.moment())
        .await
        .expect("backfill");
    assert_eq!(again, 0);
    assert_eq!(
        env.int("SELECT COUNT(*) FROM card WHERE kind = 'understand' AND state = 'new'"),
        2
    );
}

// ── 1–5 mapping and review log (R-60, R-61) ─────────────────────────────────

#[tokio::test]
async fn r61_review_log_stores_raw_grade_and_applied_fsrs_rating() {
    let env = env();
    let words: Vec<_> = (1..=5).map(|i| word(&format!("w{i}"), 5, 3)).collect();
    env.import("test-book", words).await;
    for raw in 1..=5u8 {
        let id = env.word_id(&format!("w{raw}")).await;
        let outcome = rate_understand(&env, id, raw).await;
        assert_eq!(outcome.raw_grade.get(), raw);
    }
    let conn = env.sql();
    let mut stmt = conn
        .prepare("SELECT raw_grade, fsrs_rating FROM review_log ORDER BY id")
        .expect("select");
    let pairs: Vec<(u8, u8)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    // 1 → Good before personal parameters (R-60), 2 Good, 3 Hard, 4/5 Again.
    assert_eq!(pairs, vec![(1, 3), (2, 3), (3, 2), (4, 1), (5, 1)]);
}

#[tokio::test]
async fn r60_raw_one_is_easy_once_personal_parameters_are_active() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3)]).await;
    // Fitting is M12; an active parameter set is seeded directly.
    let params = serde_json::to_string(&fsrs::DEFAULT_PARAMETERS.to_vec()).expect("json");
    env.sql()
        .execute(
            "INSERT INTO fsrs_params (fitted_at, params_json, review_count, active) \
             VALUES ('2026-11-01T00:00:00Z', ?1, 400, 1)",
            [params],
        )
        .expect("params");
    let outcome = rate_understand(&env, env.word_id("alpha").await, 1).await;
    assert_eq!(outcome.fsrs_rating, 4);
}

// ── FSRS scheduling (design §6.1, §6.2) ─────────────────────────────────────

fn expected_interval(memory: Option<fsrs::MemoryState>, retention: f32, elapsed: u32) -> i32 {
    let next = fsrs::FSRS::default()
        .next_states(memory, retention, elapsed)
        .expect("fsrs")
        .good;
    next.interval.round().clamp(1.0, 36500.0) as i32
}

#[tokio::test]
async fn r50_due_dates_follow_fsrs_deterministically() {
    let env = env();
    // Usefulness 9 → retention 0.93; usefulness 2 → 0.82 (design §6.2).
    env.import("test-book", vec![word("high", 9, 3), word("low", 2, 3)])
        .await;
    let today = env.today();
    for (headword, retention) in [("high", 0.93), ("low", 0.82)] {
        let id = env.word_id(headword).await;
        let outcome = rate_understand(&env, id, 2).await;
        let card = &outcome.card;
        assert_eq!(card.state, CardState::Review);
        assert_eq!(card.stability, Some(f64::from(2.3065f32)));
        let days = expected_interval(None, retention, 0);
        assert_eq!(card.due_day, Some(today.add_days(days)), "{headword}");
        assert_eq!(card.introduced_day, Some(today));
        assert_eq!(card.last_review_day, Some(today));
        assert_eq!(card.reps, 1);
    }

    // Second review on the due day: FSRS with the elapsed study days.
    let id = env.word_id("low").await;
    let before = understand(&env, id).await;
    let due = before.due_day.expect("due");
    env.set_day(&due.to_string());
    let outcome = rate_understand(&env, id, 2).await;
    let elapsed = today.days_between(due) as u32;
    let days = expected_interval(before_memory(&before), 0.82, elapsed);
    assert_eq!(outcome.card.due_day, Some(due.add_days(days)));
    assert_eq!(
        env.int("SELECT elapsed_days FROM review_log ORDER BY id DESC LIMIT 1"),
        i64::from(elapsed)
    );
}

fn before_memory(card: &Card) -> Option<fsrs::MemoryState> {
    card.memory()
}

/// A5 (review probe D): the format is fixed at the first review, with one
/// one-way exception: `word_only` becomes `sentence` once the word has
/// three visible sentences.
#[tokio::test]
async fn r52_understand_format_is_fixed_at_the_first_review_and_upgrades_one_way() {
    let env = env();
    env.import(
        "test-book",
        vec![word("rich", 5, 3), word("poor", 5, 2), word("none", 5, 0)],
    )
    .await;
    let rich = rate_understand(&env, env.word_id("rich").await, 2).await;
    let poor = rate_understand(&env, env.word_id("poor").await, 2).await;
    let none = rate_understand(&env, env.word_id("none").await, 2).await;
    assert_eq!(rich.card.format, Some(CardFormat::Sentence));
    assert_eq!(poor.card.format, Some(CardFormat::WordOnly));
    assert_eq!(none.card.format, Some(CardFormat::WordOnly));
    // One more sentence is not enough yet (still two visible ones).
    env.import("test-book", vec![word("poor", 5, 2)]).await;
    let id = env.word_id("poor").await;
    assert_eq!(
        understand(&env, id).await.format,
        Some(CardFormat::WordOnly)
    );
    // More sentences arrive later: the word-only card becomes a sentence
    // card at the import, with its memory state untouched.
    let before = understand(&env, id).await;
    env.import("test-book", vec![word("poor", 5, 5)]).await;
    assert_eq!(env.traits(id).await.visible_sentences, 5);
    let upgraded = understand(&env, id).await;
    assert_eq!(upgraded.format, Some(CardFormat::Sentence));
    assert_eq!(
        (upgraded.stability, upgraded.due_day, upgraded.reps),
        (before.stability, before.due_day, before.reps)
    );
    let due = upgraded.due_day.expect("due");
    env.set_day(&due.to_string());
    let again = rate_understand(&env, id, 2).await;
    assert_eq!(again.card.format, Some(CardFormat::Sentence));
    assert_eq!(
        env.int("SELECT COUNT(*) FROM review_log WHERE format = 'sentence' AND card_id = (SELECT id FROM card WHERE word_id = (SELECT id FROM word WHERE headword = 'poor'))"),
        1
    );
    // Never back: hiding sentences later leaves a sentence card as it is,
    // and a card that started as a sentence card stays one.
    env.exec("UPDATE sentence SET hidden = 1 WHERE word_id IN (SELECT id FROM word WHERE headword IN ('poor', 'rich'))");
    for headword in ["poor", "rich"] {
        let id = env.word_id(headword).await;
        assert_eq!(env.traits(id).await.visible_sentences, 0);
        ensure(&env, id).await;
        assert_eq!(
            understand(&env, id).await.format,
            Some(CardFormat::Sentence)
        );
    }
    // Undoing a review made before the upgrade does not bring `word_only`
    // back while the word still has its sentences.
    let none = env.word_id("none").await;
    let due = understand(&env, none).await.due_day.expect("due");
    env.set_day(&due.to_string());
    let second = rate_understand(&env, none, 2).await;
    assert_eq!(second.card.format, Some(CardFormat::WordOnly));
    env.import("test-book", vec![word("none", 5, 3)]).await;
    assert_eq!(
        understand(&env, none).await.format,
        Some(CardFormat::Sentence)
    );
    let traits = env.traits(none).await;
    let review_id = second.review_id;
    env.db
        .write(move |tx| {
            Ok::<_, DbError>(scheduler::undo_review(tx, review_id, &traits).expect("undo"))
        })
        .await
        .expect("undo");
    assert_eq!(
        understand(&env, none).await.format,
        Some(CardFormat::Sentence)
    );
}

#[tokio::test]
async fn r54_failed_rating_puts_the_card_into_relearning_due_today() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3)]).await;
    let id = env.word_id("alpha").await;
    let today = env.today();
    let failed = rate_understand(&env, id, 4).await;
    assert_eq!(failed.card.state, CardState::Relearning);
    assert_eq!(failed.card.due_day, Some(today));
    // A same-day repeat rated 1–3: FSRS with 0 elapsed days, at least 1 day.
    env.set_time("2026-11-20T09:05:00Z");
    let repeat = rate_understand(&env, id, 2).await;
    assert_eq!(repeat.card.state, CardState::Review);
    assert!(repeat.card.due_day.expect("due") >= today.add_days(1));
    assert_eq!(
        env.int("SELECT elapsed_days FROM review_log ORDER BY id DESC LIMIT 1"),
        0
    );
    // Failing a new card is not a lapse (design §6.5).
    assert_eq!(repeat.card.lapses, 0);
}

// ── leeches (R-55 flagging, design §6.5) ────────────────────────────────────

/// Fail a card in `review` state once on its due day, then pass the repeat.
async fn lapse(env: &Env, id: WordId) -> ReviewOutcome {
    let due = understand(env, id).await.due_day.expect("due");
    env.set_day(&due.to_string());
    let failed = rate_understand(env, id, 4).await;
    // Failing the same-day repeat again is not another lapse.
    rate_understand(env, id, 4).await;
    rate_understand(env, id, 2).await;
    failed
}

#[tokio::test]
async fn r55_card_is_flagged_after_four_lapses_not_suspended() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3)]).await;
    let id = env.word_id("alpha").await;
    rate_understand(&env, id, 2).await;
    for n in 1..=3 {
        let outcome = lapse(&env, id).await;
        assert_eq!(outcome.card.lapses, n);
        assert_eq!(outcome.card.leech, LeechState::None, "after {n} lapses");
        assert!(!outcome.leech_flagged);
    }
    let fourth = lapse(&env, id).await;
    assert_eq!(fourth.card.lapses, 4);
    assert_eq!(fourth.card.leech, LeechState::Flagged);
    assert!(fourth.leech_flagged);
    let card = understand(&env, id).await;
    assert_eq!(card.lapses, 4);
    assert!(!card.suspended, "leeches are not suspended automatically");
    assert_eq!(card.leech, LeechState::Flagged);
}

#[tokio::test]
async fn r55_acknowledged_leech_is_flagged_again_after_four_more_lapses() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3)]).await;
    let id = env.word_id("alpha").await;
    rate_understand(&env, id, 2).await;
    for _ in 0..4 {
        lapse(&env, id).await;
    }
    // The leech actions are M9; acknowledgement is seeded as they will store it.
    env.exec("UPDATE card SET leech = 'acknowledged', leech_lapses_at_ack = lapses");
    for _ in 0..3 {
        let outcome = lapse(&env, id).await;
        assert_eq!(outcome.card.leech, LeechState::Acknowledged);
    }
    let outcome = lapse(&env, id).await;
    assert_eq!(outcome.card.lapses, 8);
    assert_eq!(outcome.card.leech, LeechState::Flagged);
}

#[tokio::test]
async fn r55_two_fives_in_a_row_flag_a_previously_mature_card() {
    let env = env();
    env.import("test-book", vec![word("mature", 5, 3), word("young", 5, 3)])
        .await;
    for (headword, stability) in [("mature", 30.0), ("young", 10.0)] {
        let id = env.word_id(headword).await;
        env.set_day("2026-11-01");
        rate_understand(&env, id, 2).await;
        // Seed the memory state of a card reviewed long ago.
        env.sql()
            .execute(
                "UPDATE card SET stability = ?2, last_review_day = '2026-10-01', due_day = '2026-11-20' \
                 WHERE word_id = ?1",
                rusqlite::params![id.get(), stability],
            )
            .expect("seed");
        env.set_day("2026-11-20");
        let first = rate_understand(&env, id, 5).await;
        assert_eq!(first.card.leech, LeechState::None);
        // The next day (G5: the two 5s are on different study days).
        env.set_day("2026-11-21");
        let second = rate_understand(&env, id, 5).await;
        let expected = if headword == "mature" {
            LeechState::Flagged
        } else {
            LeechState::None
        };
        assert_eq!(second.card.leech, expected, "{headword}");
        assert_eq!(second.card.consecutive_fives, 2);
    }
}

#[tokio::test]
async fn r55_two_fives_on_the_same_study_day_do_not_flag() {
    // G5: a same-day repeat is not a second 5 "in a row" (like lapses).
    let env = env();
    env.import("test-book", vec![word("mature", 5, 3)]).await;
    let id = env.word_id("mature").await;
    env.set_day("2026-11-01");
    rate_understand(&env, id, 2).await;
    env.exec(
        "UPDATE card SET stability = 30.0, last_review_day = '2026-10-01', due_day = '2026-11-20'",
    );
    env.set_day("2026-11-20");
    rate_understand(&env, id, 5).await;
    for _ in 0..3 {
        let repeat = rate_understand(&env, id, 5).await;
        assert_eq!(repeat.card.leech, LeechState::None);
    }
    // The next day's 5 follows a day whose first rating was a 5 on the
    // mature card: flagged now.
    env.set_day("2026-11-21");
    let next_day = rate_understand(&env, id, 5).await;
    assert_eq!(next_day.card.leech, LeechState::Flagged);
}

// ── Produce / Spelling unlock (R-52, R-58, design §6.4) ─────────────────────

#[tokio::test]
async fn r52_produce_unlocks_for_active_words_after_understand_stability_seven() {
    let env = env();
    let mut no_sentences = word("bare", 8, 0);
    no_sentences["goal"] = json!("active");
    env.import(
        "test-book",
        vec![word("keen", 8, 3), word("mild", 3, 3), no_sentences],
    )
    .await;
    let keen = env.word_id("keen").await;
    let outcomes = review_until(&env, keen, 2, |o| !o.unlocked.is_empty()).await;
    let unlocking = outcomes.last().expect("outcome");
    assert!(unlocking.card.stability.expect("stability") >= 7.0);
    for earlier in &outcomes[..outcomes.len() - 1] {
        assert!(earlier.card.stability.expect("stability") < 7.0);
    }
    let unlock_day = env.today();
    let produce = card_of(&env, keen, CardKind::Produce)
        .await
        .expect("produce card");
    assert_eq!(unlocking.unlocked.len(), 1);
    assert_eq!(unlocking.unlocked[0].card_id, produce.id);
    assert_eq!(produce.state, CardState::New);
    assert_eq!(produce.due_day, Some(unlock_day.add_days(1)));
    assert_eq!(produce.stability, None);
    assert!(card_of(&env, keen, CardKind::Spelling).await.is_none());
    let created: String = env
        .sql()
        .query_row(
            "SELECT created_card_ids FROM review_log WHERE id = ?1",
            [unlocking.review_id.get()],
            |r| r.get(0),
        )
        .expect("row");
    assert_eq!(created, format!("[{}]", produce.id.get()));

    // Recognise word (usefulness 3) and an active word without sentences
    // never get one.
    for headword in ["mild", "bare"] {
        let id = env.word_id(headword).await;
        review_until(&env, id, 2, |o| o.card.stability.unwrap_or(0.0) >= 7.0).await;
        assert!(
            card_of(&env, id, CardKind::Produce).await.is_none(),
            "{headword}"
        );
    }
}

#[tokio::test]
async fn r58_spelling_unlocks_after_a_pass_on_a_later_day() {
    let env = env();
    let mut tricky = word("tricky", 4, 3);
    tricky["spelling"] = json!(true);
    env.import("test-book", vec![tricky, word("plain", 4, 3)])
        .await;
    let id = env.word_id("tricky").await;
    // Introduction day: a fail and a passing repeat on the same day.
    rate_understand(&env, id, 5).await;
    let pass_same_day = rate_understand(&env, id, 2).await;
    assert!(pass_same_day.unlocked.is_empty());
    assert!(card_of(&env, id, CardKind::Spelling).await.is_none());
    // A later day: failing does not unlock either.
    let due = understand(&env, id).await.due_day.expect("due");
    env.set_day(&due.to_string());
    let fail = rate_understand(&env, id, 4).await;
    assert!(fail.unlocked.is_empty());
    let pass = rate_understand(&env, id, 3).await;
    assert_eq!(pass.unlocked.len(), 1);
    assert_eq!(pass.unlocked[0].kind, CardKind::Spelling);
    assert_eq!(pass.unlocked[0].due_day, Some(due.add_days(1)));

    // Without `spelling: true` there is no Spelling card.
    let plain = env.word_id("plain").await;
    rate_understand(&env, plain, 2).await;
    review_until(&env, plain, 2, |_| true).await;
    assert!(card_of(&env, plain, CardKind::Spelling).await.is_none());
}

#[tokio::test]
async fn r52_goal_and_spelling_changes_suspend_and_restore_cards() {
    let env = env();
    let mut keen = word("keen", 8, 3);
    keen["spelling"] = json!(true);
    env.import("test-book", vec![keen]).await;
    let id = env.word_id("keen").await;
    review_until_unlocked(&env, id).await;
    let produce = card_of(&env, id, CardKind::Produce).await.expect("produce");
    let spelling = card_of(&env, id, CardKind::Spelling)
        .await
        .expect("spelling");
    assert!(!produce.suspended && !spelling.suspended);

    // Owner edits are M7; the stored values are changed directly here.
    env.exec("UPDATE word SET goal = 'recognise', spelling = 0");
    assert_eq!(ensure(&env, id).await, 0);
    let p = card_of(&env, id, CardKind::Produce).await.expect("produce");
    let s = card_of(&env, id, CardKind::Spelling)
        .await
        .expect("spelling");
    assert!(p.suspended && s.suspended);
    assert_eq!(p.due_day, produce.due_day, "state is kept");

    env.exec("UPDATE word SET goal = 'active', spelling = 1");
    ensure(&env, id).await;
    let p = card_of(&env, id, CardKind::Produce).await.expect("produce");
    let s = card_of(&env, id, CardKind::Spelling)
        .await
        .expect("spelling");
    assert!(!p.suspended && !s.suspended);
    assert_eq!(cards(&env, id).await.len(), 3);
}

// ── undo (design §6.7.9) ────────────────────────────────────────────────────

#[tokio::test]
async fn r54_undo_restores_the_card_and_removes_unlocked_cards() {
    let env = env();
    env.import("test-book", vec![word("keen", 8, 3)]).await;
    let id = env.word_id("keen").await;
    // Reach the review that unlocks Produce, but undo it.
    loop {
        let card = understand(&env, id).await;
        if let Some(due) = card.due_day {
            env.set_day(&due.to_string());
        }
        let before = env.rows("card", "1");
        let log_before = env.rows("review_log", "undone = 0");
        let outcome = rate_understand(&env, id, 2).await;
        if outcome.unlocked.is_empty() {
            continue;
        }
        let traits = env.traits(id).await;
        let again_traits = traits.clone();
        let undo = env
            .db
            .write(move |tx| {
                Ok::<_, DbError>(
                    scheduler::undo_review(tx, outcome.review_id, &traits).expect("undo"),
                )
            })
            .await
            .expect("undo");
        assert_eq!(undo.removed_cards.len(), 1);
        assert_eq!(env.rows("card", "1"), before, "card table restored exactly");
        assert_eq!(env.rows("review_log", "undone = 0"), log_before);
        assert_eq!(
            env.int("SELECT undone FROM review_log ORDER BY id DESC LIMIT 1"),
            1,
            "kept for audit"
        );
        // Undoing twice is refused.
        let again = env
            .db
            .write(move |tx| {
                Ok::<_, DbError>(scheduler::undo_review(tx, undo.review_id, &again_traits))
            })
            .await
            .expect("tx");
        assert!(again.is_err());
        return;
    }
}

// ── normal load (design §6.6) ───────────────────────────────────────────────

#[tokio::test]
async fn r54_normal_load_is_the_fourteen_day_mean_with_a_floor_of_twenty() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3)]).await;
    let today = day("2026-11-20");
    let load = |env: &Env| {
        let db = env.db.clone();
        async move {
            db.read(move |c| Ok::<_, DbError>(scheduler::normal_load(c, today)?))
                .await
                .expect("load")
        }
    };
    assert_eq!(load(&env).await, 20.0);
    let card = env.int("SELECT id FROM card LIMIT 1");
    let conn = env.sql();
    let insert = |n: u32, review_day: &str, elapsed: i64, state: &str, undone: i64| {
        for _ in 0..n {
            let no = REVIEW_NO.fetch_add(1, Ordering::Relaxed);
            conn.execute(
                "INSERT INTO review_log (client_review_id, card_id, reviewed_at, review_day, raw_grade, \
                 fsrs_rating, format, elapsed_days, before_json, undone) \
                 VALUES (?1, ?2, '2026-11-10T09:00:00Z', ?3, 2, 3, 'sentence', ?4, ?5, ?6)",
                rusqlite::params![
                    format!("test-load-{no}"),
                    card,
                    review_day,
                    elapsed,
                    format!("{{\"state\":\"{state}\"}}"),
                    undone
                ],
            )
            .expect("insert");
        }
    };
    // 14 × 30 = 420 counted reviews in the window → mean 30.
    insert(420, "2026-11-12", 3, "review", 0);
    // Not counted: introductions, same-day repeats, undone, today, too old.
    insert(100, "2026-11-12", 0, "new", 0);
    insert(100, "2026-11-12", 0, "relearning", 0);
    insert(100, "2026-11-12", 3, "review", 1);
    insert(100, "2026-11-20", 3, "review", 0);
    insert(100, "2026-11-05", 3, "review", 0);
    insert(14, "2026-11-06", 3, "review", 0);
    assert_eq!(load(&env).await, 31.0);
}

// ── fix round R1: one bad day counts once (A2, A3) ──────────────────────────

fn memory_of(card: &Card) -> (Option<f64>, Option<f64>) {
    (card.stability, card.difficulty)
}

/// A2 (review probes A/A2): a lapse is the first failing review of a study
/// day of any card that is not new, also while it is still `relearning`;
/// at most one per card and day. A word failed day after day, with every
/// in-session repeat failed too, becomes a leech.
#[tokio::test]
async fn r55_a_word_failed_every_day_becomes_a_leech_even_when_all_repeats_fail() {
    for (label, learn_first, grade) in [("never learned", false, 5u8), ("learned first", true, 4)] {
        let env = env();
        env.import("test-book", vec![word("stubborn", 8, 3)]).await;
        let id = env.word_id("stubborn").await;
        let mut day_no = 20;
        if learn_first {
            rate_understand(&env, id, 2).await;
            day_no += 1;
            env.set_day(&format!("2026-11-{day_no}"));
        }
        let mut lapses_by_day = Vec::new();
        let mut flagged_on = None;
        for n in 0..6 {
            // The first rating of the day and two failed in-session repeats.
            for _ in 0..3 {
                let outcome = rate_understand(&env, id, grade).await;
                assert_eq!(outcome.card.state, CardState::Relearning, "{label}");
                if outcome.leech_flagged && flagged_on.is_none() {
                    flagged_on = Some(n);
                }
            }
            lapses_by_day.push(understand(&env, id).await.lapses);
            day_no += 1;
            env.set_day(&format!("2026-11-{day_no}"));
        }
        if learn_first {
            assert_eq!(
                lapses_by_day,
                [1, 2, 3, 4, 5, 6],
                "{label}: one lapse per day"
            );
            assert_eq!(
                flagged_on,
                Some(3),
                "{label}: flagged with the fourth lapse"
            );
        } else {
            // The day the card was new is not a lapse.
            assert_eq!(lapses_by_day, [0, 1, 2, 3, 4, 5], "{label}");
            assert_eq!(flagged_on, Some(4), "{label}");
        }
        let card = understand(&env, id).await;
        assert_eq!(card.leech, LeechState::Flagged, "{label}");
        assert!(!card.suspended);
    }
}

/// A2: passing the in-session repeat does not hide the day's lapse, and a
/// second failure after a pass on the same day is not a second lapse.
#[tokio::test]
async fn r55_at_most_one_lapse_per_card_and_study_day() {
    let env = env();
    env.import("test-book", vec![word("alpha", 5, 3)]).await;
    let id = env.word_id("alpha").await;
    rate_understand(&env, id, 2).await;
    env.set_day("2026-11-25");
    assert_eq!(rate_understand(&env, id, 4).await.card.lapses, 1);
    assert_eq!(rate_understand(&env, id, 2).await.card.lapses, 1);
    assert_eq!(rate_understand(&env, id, 5).await.card.lapses, 1);
    assert_eq!(rate_understand(&env, id, 5).await.card.lapses, 1);
    env.set_day("2026-11-26");
    assert_eq!(rate_understand(&env, id, 2).await.card.lapses, 1);
    assert_eq!(rate_understand(&env, id, 4).await.card.lapses, 2);
}

/// A3 (R-50/R-54, review probe G): only the first failing rating of a study
/// day lowers the memory state. Later failing ratings of that day are
/// logged with their raw grade, keep the card `relearning` and due today,
/// and change neither stability nor difficulty; a pass applies as always.
#[tokio::test]
async fn r50_only_the_first_failure_of_a_study_day_lowers_the_memory_state() {
    let env = env();
    env.import(
        "test-book",
        vec![word("mature", 8, 3), word("control", 8, 3)],
    )
    .await;
    let id = env.word_id("mature").await;
    let control = env.word_id("control").await;
    for word in [id, control] {
        env.set_day("2026-11-20");
        review_until(&env, word, 2, |o| {
            o.card.stability.is_some_and(|s| s > 25.0)
        })
        .await;
    }
    // Both cards are in the same state; give them the same bad day.
    let bad_day = understand(&env, id)
        .await
        .due_day
        .max(understand(&env, control).await.due_day)
        .expect("due");
    env.set_day(&bad_day.to_string());
    let before = understand(&env, id).await;

    let first = rate_understand(&env, id, 4).await;
    assert!(
        first.card.stability < before.stability,
        "the first failure counts"
    );
    let after_first = memory_of(&first.card);
    for grade in [4, 5, 5] {
        let again = rate_understand(&env, id, grade).await;
        assert_eq!(
            memory_of(&again.card),
            after_first,
            "raw {grade}: memory untouched"
        );
        assert_eq!(again.card.state, CardState::Relearning);
        assert_eq!(again.card.due_day, Some(bad_day));
        assert_eq!(again.raw_grade.get(), grade);
        assert_eq!(again.fsrs_rating, 1);
    }
    assert_eq!(understand(&env, id).await.reps, before.reps + 4);
    let logged: Vec<(i64, i64)> = {
        let conn = env.sql();
        let mut stmt = conn
            .prepare(
                "SELECT raw_grade, memory_updated FROM review_log WHERE review_day = ?1 \
                 AND card_id = ?2 ORDER BY id",
            )
            .expect("prepare");
        stmt.query_map(
            rusqlite::params![bad_day.to_string(), before.id.get()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("query")
        .map(|r| r.expect("row"))
        .collect()
    };
    assert_eq!(
        logged,
        [(4, 1), (4, 0), (5, 0), (5, 0)],
        "raw grades kept (R-61)"
    );

    // Four failures then a pass leave the card exactly where one failure
    // and a pass leave the control card.
    let passed = rate_understand(&env, id, 2).await;
    rate_understand(&env, control, 4).await;
    let control_passed = rate_understand(&env, control, 2).await;
    assert_eq!(memory_of(&passed.card), memory_of(&control_passed.card));
    assert_eq!(passed.card.due_day, control_passed.card.due_day);
    assert_eq!(passed.card.state, CardState::Review);

    // Undo goes back step by step through logged-only ratings too.
    let traits = env.traits(id).await;
    let review_id = passed.review_id;
    let undone = env
        .db
        .write(move |tx| {
            Ok::<_, DbError>(scheduler::undo_review(tx, review_id, &traits).expect("undo"))
        })
        .await
        .expect("undo");
    assert_eq!(memory_of(&undone.card), after_first);
    assert_eq!(undone.card.state, CardState::Relearning);
    // The next day a failure counts again.
    env.set_day(&bad_day.add_days(1).to_string());
    let next = rate_understand(&env, id, 4).await;
    assert_ne!(memory_of(&next.card), after_first);
    assert_eq!(next.card.lapses, before.lapses + 2);
}
