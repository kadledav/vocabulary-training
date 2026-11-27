-- Migration 0008: which clips spell a word out letter by letter (M10,
-- design §10.3; R-58).
--
-- `spelled`: how many words of the clip the TTS service read letter by
-- letter (its `X-Words-Spelled` header; a word in no dictionary that is not
-- a compound). Such a clip says the answer of a Spelling question aloud, so
-- it is never played as a Spelling prompt. NULL: unknown (a clip made
-- before this version); an unknown clip is not used as a Spelling prompt
-- either, until it is made again (the clip identity changed with this
-- version, so every clip is).
ALTER TABLE audio_clip ADD COLUMN spelled INTEGER
  CHECK (spelled IS NULL OR spelled >= 0);
