-- Migration 0005: fix round R1 (learning logic and session hardening).
--
-- 1. `source.title_defaulted` (R-02): 1 while the title is the stand-in the
--    app chose because the first pack had none; the first real title from a
--    later pack then replaces it. Existing sources named after their slug,
--    and `general` sources named "General Vocabulary", were named that way
--    by the app (v0.1.0's stand-ins).
ALTER TABLE source ADD COLUMN title_defaulted INTEGER NOT NULL DEFAULT 0
  CHECK (title_defaulted IN (0, 1));
UPDATE source SET title_defaulted = 1
  WHERE title = slug OR (type = 'general' AND title = 'General Vocabulary');

-- 2. `review_log.memory_updated` (R-50/R-54): 0 for a failing rating that
--    was logged and requeued but did not change the card's FSRS memory
--    state, because the card had already failed once on that study day.
--    Everything that replays the history (parameter fitting, memory-state
--    recompute) skips those rows.
ALTER TABLE review_log ADD COLUMN memory_updated INTEGER NOT NULL DEFAULT 1
  CHECK (memory_updated IN (0, 1));

-- 3. Sessions (R-82): wrong passwords typed inside a logged-in session are
--    counted per session (five end that session), not against the global
--    login lock.
ALTER TABLE auth_session ADD COLUMN password_failures INTEGER NOT NULL DEFAULT 0;

-- 4. The login throttle survives a restart (R-82): one row, the state as
--    JSON (times of recent failed logins, lock end, lock level). '{}' = no
--    failures. `reset-password` clears it.
CREATE TABLE auth_throttle (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  state_json TEXT NOT NULL
) STRICT;
INSERT INTO auth_throttle (id, state_json) VALUES (1, '{}');

-- 5. Sentence keys (R-22): the key now also folds dash, ellipsis and quote
--    variants and closing punctuation. The keys are computed in Rust
--    (`vocab_pack::keys::sentence_key`), so the re-keying and the merging of
--    rows that now share a key is the data step that runs after this file
--    in the same transaction (`db::rekey_sentences`). The references to a
--    sentence are indexed first: the merge moves them set-based, and
--    deleting a sentence (here and later) must not scan every review.
CREATE INDEX review_log_sentence ON review_log(sentence_id);
CREATE INDEX session_item_sentence ON session_item(sentence_id);

-- 6. Block kinds (R-63, owner feedback after v0.1.0): `regular` (due cards
--    with new words mixed in), `extra` (extra practice of cards that are not
--    due) or `more_new` ("learn more new words"). Blocks built before this
--    migration were regular ones.
ALTER TABLE practice_session ADD COLUMN kind TEXT NOT NULL DEFAULT 'regular'
  CHECK (kind IN ('regular', 'extra', 'more_new'));
