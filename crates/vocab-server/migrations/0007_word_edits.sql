-- Migration 0007: the owner edits words in the app (M7, design §5.4, §7.1;
-- R-03, R-04).
--
-- 1. `word_alias`: earlier headword keys of a word whose headword the owner
--    changed. A pack word is matched by its key against the words' keys
--    and these aliases, so a re-import of the same pack still finds the
--    edited word (and its learning state) instead of adding the old
--    spelling as a new word.
CREATE TABLE word_alias (
  key     TEXT PRIMARY KEY,                               -- a former headword_key
  word_id INTEGER NOT NULL REFERENCES word(id) ON DELETE CASCADE
) STRICT;
CREATE INDEX word_alias_by_word ON word_alias(word_id);

-- 2. `word_form.removed`: a form the owner removed; a pack never adds it
--    again (as `word_item.removed` does for the list items).
ALTER TABLE word_form ADD COLUMN removed INTEGER NOT NULL DEFAULT 0
  CHECK (removed IN (0, 1));

-- 3. The word list sorts by date added.
CREATE INDEX word_by_created ON word(created_at);
