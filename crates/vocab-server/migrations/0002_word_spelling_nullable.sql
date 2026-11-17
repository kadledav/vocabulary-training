-- Migration 0002: `word.spelling` becomes nullable (M2 review finding F2).
--
-- NULL means "no pack has said whether this word needs spelling practice",
-- so a later pack can still fill it (merge rule "existing empty -> take
-- incoming", design §5.4). With 0001's `NOT NULL DEFAULT 0` a word imported
-- without `spelling` looked like an explicit `false`, and a later `true`
-- could only ever become a conflict. Existing values are kept unchanged.
--
-- SQLite cannot drop a NOT NULL constraint in place; a new column plus
-- DROP/RENAME COLUMN avoids rebuilding `word`, which many tables reference
-- with ON DELETE CASCADE.
ALTER TABLE word ADD COLUMN spelling_nullable INTEGER CHECK (spelling_nullable IN (0, 1));
UPDATE word SET spelling_nullable = spelling;
ALTER TABLE word DROP COLUMN spelling;
ALTER TABLE word RENAME COLUMN spelling_nullable TO spelling;
