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

-- 4. Ids in URLs are never reused (`/api/words/{id}`, `/api/sentences/{id}`,
--    `/api/conflicts/{id}`): a stale page or an Undo for a deleted row must
--    not reach a newer row with the same id. `id_floor` keeps the highest
--    id ever deleted per table (kept by triggers, which also see rows
--    deleted by a cascade); new rows of these tables get an id above both
--    the highest stored and the highest deleted one (catalog). Additive:
--    no table is rebuilt.
CREATE TABLE id_floor (
  name    TEXT PRIMARY KEY CHECK (name IN ('word', 'sentence', 'field_conflict')),
  last_id INTEGER NOT NULL
) STRICT;
INSERT INTO id_floor (name, last_id) VALUES
  ('word', COALESCE((SELECT MAX(id) FROM word), 0)),
  ('sentence', COALESCE((SELECT MAX(id) FROM sentence), 0)),
  ('field_conflict', COALESCE((SELECT MAX(id) FROM field_conflict), 0));
CREATE TRIGGER word_id_floor AFTER DELETE ON word BEGIN
  UPDATE id_floor SET last_id = MAX(last_id, OLD.id) WHERE name = 'word';
END;
CREATE TRIGGER sentence_id_floor AFTER DELETE ON sentence BEGIN
  UPDATE id_floor SET last_id = MAX(last_id, OLD.id) WHERE name = 'sentence';
END;
CREATE TRIGGER field_conflict_id_floor AFTER DELETE ON field_conflict BEGIN
  UPDATE id_floor SET last_id = MAX(last_id, OLD.id) WHERE name = 'field_conflict';
END;
