-- Migration 0001: initial schema (design docs/design/architecture.md §4.2).
-- Every table is STRICT; foreign keys are enforced on every connection (§4.1).
-- Each module owns its tables (R-92); the owner is noted in the section headers.
-- Forward-only: never edit this file once released; add 0002_… instead.

-- ── importer ─────────────────────────────────────────────────────────────
CREATE TABLE pack_import (
  id             INTEGER PRIMARY KEY,
  pack_id        TEXT NOT NULL,
  content_sha256 TEXT NOT NULL,
  format_version INTEGER NOT NULL,
  source_slug    TEXT NOT NULL,
  exported_at    TEXT,
  part_index     INTEGER NOT NULL, part_total INTEGER NOT NULL,
  via            TEXT NOT NULL CHECK (via IN ('paste','connector')),
  imported_at    TEXT NOT NULL,
  report_json    TEXT NOT NULL,                          -- counts + per-word outcome; no sentence text
  UNIQUE (pack_id, content_sha256)
) STRICT;

-- ── catalog ──────────────────────────────────────────────────────────────
CREATE TABLE source (
  id               INTEGER PRIMARY KEY,
  slug             TEXT NOT NULL UNIQUE,                 -- pack source.id
  type             TEXT NOT NULL CHECK (type IN ('book','film','series','article','general')),
  title            TEXT NOT NULL,
  author           TEXT, series TEXT, series_number INTEGER, url TEXT,
  reading_position TEXT,                                 -- latest received (R-59)
  reading_position_exported_at TEXT,                     -- exported_at of the pack that set it
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE word (
  id            INTEGER PRIMARY KEY,
  headword      TEXT NOT NULL,
  headword_key  TEXT NOT NULL UNIQUE,                    -- merge identity (R-03)
  grammar       TEXT, pronunciation TEXT, speak TEXT,
  meaning       TEXT NOT NULL,
  usefulness    INTEGER NOT NULL CHECK (usefulness BETWEEN 1 AND 10),
  goal          TEXT CHECK (goal IN ('recognise','active')),  -- NULL = derive from usefulness (R-15)
  hint          TEXT, spelling INTEGER NOT NULL DEFAULT 0, spelling_note TEXT,
  note          TEXT NOT NULL DEFAULT '', icon TEXT,
  low_priority  INTEGER NOT NULL DEFAULT 0,              -- derived from latest link tag unless overridden
  included      INTEGER NOT NULL DEFAULT 0,              -- owner included a low-priority word (R-51)
  suspended     INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE word_source (                               -- many-to-many (R-02)
  word_id   INTEGER NOT NULL REFERENCES word(id) ON DELETE CASCADE,
  source_id INTEGER NOT NULL REFERENCES source(id),
  seq       INTEGER NOT NULL,                            -- order within source (R-01)
  tag_kind  TEXT CHECK (tag_kind IN ('low-priority','seen-before','not-from-source')),
  tag_text  TEXT,
  first_pack_import_id INTEGER NOT NULL REFERENCES pack_import(id),
  last_pack_import_id  INTEGER NOT NULL REFERENCES pack_import(id),
  PRIMARY KEY (word_id, source_id)
) STRICT;
CREATE INDEX word_source_by_source ON word_source(source_id, seq);

CREATE TABLE word_item (                                 -- list-valued fields
  word_id   INTEGER NOT NULL REFERENCES word(id) ON DELETE CASCADE,
  kind      TEXT NOT NULL CHECK (kind IN ('czech','accepted','confusable','owner_accepted')),
  value     TEXT NOT NULL,
  value_key TEXT NOT NULL,
  position  INTEGER NOT NULL,
  origin    TEXT NOT NULL CHECK (origin IN ('pack','owner')),
  removed   INTEGER NOT NULL DEFAULT 0,                  -- owner removed it; a pack never re-adds it
  PRIMARY KEY (word_id, kind, value_key)
) STRICT;

CREATE TABLE word_form (
  word_id  INTEGER NOT NULL REFERENCES word(id) ON DELETE CASCADE,
  form     TEXT NOT NULL, form_key TEXT NOT NULL,
  common   INTEGER NOT NULL, position INTEGER NOT NULL,
  PRIMARY KEY (word_id, form_key)
) STRICT;

CREATE TABLE sentence (
  id         INTEGER PRIMARY KEY,
  word_id    INTEGER NOT NULL REFERENCES word(id) ON DELETE CASCADE,
  en_marked  TEXT NOT NULL,                              -- canonical text incl. [[...]]
  cs_marked  TEXT NOT NULL,
  en_key     TEXT NOT NULL,                              -- dedupe key (R-22)
  origin     TEXT NOT NULL CHECK (origin IN ('book','source','example')),
  location   TEXT,
  source_id  INTEGER REFERENCES source(id),              -- pack source it arrived with
  hidden     INTEGER NOT NULL DEFAULT 0,                 -- owner "hide this sentence"
  edited     INTEGER NOT NULL DEFAULT 0,                 -- owner edited it (R-25)
  position   INTEGER NOT NULL,
  pack_import_id INTEGER REFERENCES pack_import(id),
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  UNIQUE (word_id, en_key)
) STRICT;

CREATE TABLE word_override (                             -- fields the owner set in the app (R-04, R-52)
  word_id INTEGER NOT NULL REFERENCES word(id) ON DELETE CASCADE,
  field   TEXT NOT NULL,                                 -- 'meaning','usefulness','goal','low_priority',...
  set_at  TEXT NOT NULL,
  PRIMARY KEY (word_id, field)
) STRICT;

CREATE TABLE field_conflict (                            -- R-03: differing pack values kept, not overwritten
  id             INTEGER PRIMARY KEY,
  word_id        INTEGER NOT NULL REFERENCES word(id) ON DELETE CASCADE,
  field          TEXT NOT NULL,
  current_value  TEXT,                                   -- JSON-encoded scalar
  incoming_value TEXT NOT NULL,
  pack_import_id INTEGER NOT NULL REFERENCES pack_import(id),
  status         TEXT NOT NULL CHECK (status IN ('open','applied','dismissed')),
  created_at TEXT NOT NULL, resolved_at TEXT
) STRICT;
CREATE INDEX field_conflict_open ON field_conflict(word_id) WHERE status = 'open';

-- ── scheduler ────────────────────────────────────────────────────────────
CREATE TABLE card (
  id            INTEGER PRIMARY KEY,
  word_id       INTEGER NOT NULL REFERENCES word(id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK (kind IN ('understand','produce','spelling')),
  format        TEXT CHECK (format IN ('sentence','word_only','cloze','spelling')),  -- fixed at first review
  state         TEXT NOT NULL CHECK (state IN ('new','relearning','review')),
  stability     REAL, difficulty REAL,                   -- FSRS memory state (NULL while new)
  due_day       TEXT,                                    -- NULL while new and not yet scheduled
  last_review_day TEXT, last_review_at TEXT,
  introduced_day TEXT,
  reps          INTEGER NOT NULL DEFAULT 0,
  lapses        INTEGER NOT NULL DEFAULT 0,
  consecutive_fives INTEGER NOT NULL DEFAULT 0,
  leech         TEXT NOT NULL DEFAULT 'none' CHECK (leech IN ('none','flagged','acknowledged')),
  leech_lapses_at_ack INTEGER NOT NULL DEFAULT 0,
  suspended     INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  UNIQUE (word_id, kind)
) STRICT;
CREATE INDEX card_due ON card(due_day) WHERE suspended = 0 AND due_day IS NOT NULL;
CREATE INDEX card_new ON card(word_id) WHERE state = 'new' AND suspended = 0;

CREATE TABLE review_log (
  id               INTEGER PRIMARY KEY,
  client_review_id TEXT NOT NULL UNIQUE,                 -- idempotency (client UUID)
  card_id          INTEGER NOT NULL REFERENCES card(id) ON DELETE CASCADE,
  session_id       TEXT,
  reviewed_at      TEXT NOT NULL,                        -- server time
  client_ts        TEXT,                                 -- client time (offline later)
  review_day       TEXT NOT NULL,
  raw_grade        INTEGER NOT NULL CHECK (raw_grade BETWEEN 1 AND 5),   -- R-61
  fsrs_rating      INTEGER NOT NULL CHECK (fsrs_rating BETWEEN 1 AND 4), -- as applied
  suggested_grade  INTEGER CHECK (suggested_grade BETWEEN 1 AND 5),
  format           TEXT NOT NULL,
  sentence_id      INTEGER REFERENCES sentence(id) ON DELETE SET NULL,
  typed_answer     TEXT, answer_match TEXT CHECK (answer_match IN ('exact','none','skipped','dont_know')),
  hint_used        INTEGER NOT NULL DEFAULT 0,
  elapsed_days     INTEGER NOT NULL,                     -- since previous review of this card (0 = same day)
  duration_ms      INTEGER,
  before_json      TEXT NOT NULL,                        -- card snapshot for undo
  created_card_ids TEXT NOT NULL DEFAULT '[]',           -- cards unlocked by this review (undo deletes them)
  undone           INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX review_by_card ON review_log(card_id, reviewed_at);
CREATE INDEX review_by_day  ON review_log(review_day);

CREATE TABLE fsrs_params (
  id INTEGER PRIMARY KEY, fitted_at TEXT NOT NULL, params_json TEXT NOT NULL,
  review_count INTEGER NOT NULL, log_loss_new REAL, log_loss_old REAL,
  active INTEGER NOT NULL
) STRICT;

-- ── session ──────────────────────────────────────────────────────────────
CREATE TABLE practice_session (
  id TEXT PRIMARY KEY,                                   -- random 128-bit hex
  study_day TEXT NOT NULL, scope_source_id INTEGER REFERENCES source(id),
  block_no INTEGER NOT NULL, block_size INTEGER NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('active','finished','expired')),
  started_at TEXT NOT NULL, finished_at TEXT
) STRICT;
CREATE TABLE session_item (
  id INTEGER PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES practice_session(id) ON DELETE CASCADE,
  ord REAL NOT NULL,                                     -- fractional order: repeats are inserted between
  card_id INTEGER NOT NULL REFERENCES card(id) ON DELETE CASCADE,
  origin TEXT NOT NULL CHECK (origin IN ('due','new','repeat')),
  sentence_id INTEGER REFERENCES sentence(id) ON DELETE SET NULL,
  review_id INTEGER REFERENCES review_log(id)            -- NULL = not answered yet
) STRICT;
CREATE INDEX session_item_order ON session_item(session_id, ord);

-- ── audio ────────────────────────────────────────────────────────────────
CREATE TABLE audio_clip (
  hash TEXT PRIMARY KEY,                                 -- sha256(pipeline|model|voice|speed|text), 32 hex
  text TEXT NOT NULL, voice TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('word','sentence')),
  status TEXT NOT NULL CHECK (status IN ('pending','ready','failed')),
  attempts INTEGER NOT NULL DEFAULT 0, next_attempt_at TEXT, last_error TEXT,
  bytes INTEGER, duration_ms INTEGER,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL
) STRICT;
CREATE INDEX audio_pending ON audio_clip(next_attempt_at) WHERE status = 'pending';

-- ── auth / settings ──────────────────────────────────────────────────────
CREATE TABLE auth_session (
  token_sha256 TEXT PRIMARY KEY,
  password_fingerprint TEXT NOT NULL,                    -- sha256 of the configured hash: rotating the password ends all sessions
  created_at TEXT NOT NULL, last_seen_at TEXT NOT NULL, expires_at TEXT NOT NULL,
  user_agent TEXT
) STRICT;
CREATE TABLE setting (key TEXT PRIMARY KEY, value_json TEXT NOT NULL) STRICT;
