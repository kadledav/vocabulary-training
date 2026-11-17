-- Migration 0003: `session_item.format` (M3 review, guarantor decision G2).
--
-- The exercise format a practice item is shown in is decided when the block
-- is built and must not change before the item is rated: an import that
-- adds sentences in between would otherwise switch a new Understand card
-- from `word_only` to `sentence` on the next view, and the first review
-- would fix a format the owner never saw (R-52, D-2). The item therefore
-- stores the format next to the sentence it already stores. NULL = an item
-- of a block built before this migration (the view falls back to the card).
ALTER TABLE session_item ADD COLUMN format TEXT
  CHECK (format IN ('sentence','word_only','cloze','spelling'));
