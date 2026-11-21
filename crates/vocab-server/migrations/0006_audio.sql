-- Migration 0006: neural audio (M6, design §10.3; R-40, R-42).
--
-- `audio_clip` exists since 0001 (one row per clip, named by its content
-- hash); nothing wrote to it before v0.2.0.
--
-- 1. `approximate`: how many words of the clip the TTS service pronounced
--    by a guess (fallback dictionary or letter by letter); the app shows
--    such clips as approximate.
ALTER TABLE audio_clip ADD COLUMN approximate INTEGER NOT NULL DEFAULT 0
  CHECK (approximate >= 0);

-- 2. `crashes`: how often the TTS service dropped the connection while
--    making this clip (it may crash on the text); three crashes and the
--    clip fails for good, so one text cannot keep the service restarting.
ALTER TABLE audio_clip ADD COLUMN crashes INTEGER NOT NULL DEFAULT 0
  CHECK (crashes >= 0);

-- 3. Counts per status for Settings; lookups by text (a clip of an older
--    TTS version is played until its replacement is ready).
CREATE INDEX audio_clip_status ON audio_clip(status);
CREATE INDEX audio_clip_text ON audio_clip(voice, text);

-- 4. The TTS service's front-end and engine versions, part of every clip's
--    identity: one row, written when the service first answers and
--    whenever its versions change (then all clips are made again; an old
--    clip stays playable until its replacement is ready, then it is
--    garbage-collected).
CREATE TABLE audio_engine (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  frontend TEXT NOT NULL,
  engine TEXT NOT NULL,
  checked_at TEXT NOT NULL
) STRICT;
