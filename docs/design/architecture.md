# Vocabulary Training — Architecture & Technical Design

Status: **design v1.0 for phase 1** · Date: 2026-11-17 · Author: design agent · Reviewer: guarantor
Inputs: `docs/requirements.md` (v1.0 incl. the R-85 storage change and R-59), `docs/vocabulary-export-instructions.txt` (pack format v1, single source of the instruction text), `docs/research/training-ui.md` (as amended by R-74), the owner's home infrastructure (`home/INFRASTRUCTURE.md`, Traefik and `another-service-notify` stacks).
Hostname: `vocab.example.home` is a placeholder for the owner's real hostname (kept out of this public repo; set via `VOCAB_PUBLIC_ORIGIN` in the Compose environment and the Traefik rule in the home repo).
Companion file: `docs/vocab-pack.schema.json` (JSON Schema draft 2020-12 for pack format v1).

The owner does not review this design; he wants a working app quickly and will give feedback by using it. So this document **decides** wherever a decision is reversible and lists only genuinely irreversible or out-of-scope items for him (§19.3).

---

## 0. Decisions at a glance

| Topic | Decision |
|---|---|
| Language / runtime | **Rust** (stable 1.99.0), one Cargo workspace: `vocab-pack` (format library), `vocab-server` (the app), `vocab-tts` (TTS worker). |
| Web stack | `axum` 0.8 on `tokio`; JSON API + static single-page frontend embedded in the binary. |
| Frontend | **Vanilla ES modules, no framework, no build step, zero runtime npm dependencies.** DOM is built only through a small `h()` helper (never `innerHTML`). Type-checked with `tsc --checkJs` in CI. Ready for a later service worker (static assets, idempotent review posts, content-hashed audio URLs). |
| Storage | SQLite via `rusqlite` (bundled SQLite), WAL journal, `synchronous=FULL`, one file set in `/data`. Migrations embedded, forward-only, `PRAGMA user_version`. |
| Scheduling | **`fsrs` crate 6.6.2** (the reference FSRS-6 implementation used by Anki), incl. in-app parameter fitting. |
| TTS | **Kokoro-82M v1.0** (Apache-2.0 weights) run by **sherpa-onnx 2.0.1** inside a separate hardened `vocab-tts` container with no network except an internal-only Docker network. MP3 (LAME), loudness-normalised to −16 LUFS. Default voice `af_heart` (US), `bf_emma` (UK) for R-42. |
| Images | App: static musl binary on `gcr.io/distroless/static-debian13:nonroot` (pinned digest). TTS: `gcr.io/distroless/cc-debian13:nonroot` (pinned digest). Both default to UID/GID **65532:65532** (deployed as 3101:3101), read-only rootfs, `cap_drop: ALL`, `no-new-privileges`, limits. |
| Network | Traefik `home` entrypoint only, host `vocab.example.home`; no `websecure` router, no published ports, no mesh VPN. TTS container only on an `internal: true` network. |
| Auth | Single password (argon2id hash in the DB, set on first start with a logged one-time setup code) → 180-day `__Host-` session cookie (SameSite=Strict, HttpOnly, Secure). Separate bearer **connector token** (made in Settings, stored as SHA-256) for the connector, scoped to the connector endpoints. Origin check on every state-changing request. Global login throttling. |
| Backup | None in the app (R-85): ZFS dataset `tank/services/vocabulary`, replicated daily by the existing job. App keeps all DB files in `/data` and runs `quick_check` at start. |
| MCP connector | **Node.js Claude Desktop extension (`.mcpb`), zero runtime dependencies**, hand-written minimal MCP stdio server; token stored by Claude Desktop as a `sensitive` user-config value. Tools: `add_words`, `list_sources`. |
| Dependency hygiene | Every version pinned exactly (Cargo `=x.y.z` + `Cargo.lock`, `package-lock.json`, image digests, Action SHAs). Renovate with `minimumReleaseAge: "14 days"`. A CI gate (`tools/dep-age.py`) fails the build if **any** locked crate or npm package (incl. transitive) was published < 14 days ago. |
| First usable version | After milestone **M5** (import + Understand card + 1–5 rating + FSRS + auth + deploy, browser-speech audio); neural audio arrives in **M6**. Produce, leeches/progress, Spelling and the MCP connector follow in M7–M11. |

---

## 1. Tech stack decision

### 1.1 Options compared

| Criterion | **A. Rust** (axum + rusqlite + `fsrs`) | **B. Go** (net/http + modernc SQLite + `go-fsrs`) | **C. TypeScript/Node** (Hono/Express + better-sqlite3 + `ts-fsrs`) |
|---|---|---|---|
| Static binary for scratch/distroless | Yes: musl static binary, ~10 MB, runs on `distroless/static` (no libc). | Yes: pure-Go SQLite (CGO-free), easiest static build. | No: needs a Node runtime image (`distroless/nodejs`) and a native addon (better-sqlite3) or experimental `node:sqlite`. Larger surface. |
| Mature FSRS | **Best**: `fsrs` crate is the reference implementation (open-spaced-repetition/fsrs-rs, used inside Anki), FSRS-6, **includes the parameter optimiser** (needed for R-60 "until personal parameters are fitted"). Since 6.x it no longer pulls the `burn` ML framework — only `ndarray`/`rayon`. | `go-fsrs` implements scheduling only; no optimiser → fitting would need a second tool. | `ts-fsrs` is mature for scheduling; the optimiser is a native binding to fsrs-rs (`@open-spaced-repetition/binding`) — Rust anyway, through napi. |
| SQLite | `rusqlite` with bundled SQLite: mature, statically linked, no system lib. | modernc (C transpiled to Go) — fine, slower, large. | Native addon (prebuilt binaries per Node ABI) or experimental built-in. |
| Patching burden for an owner who patches rarely | Medium dependency count, but compile-time checked; Renovate PRs + CI make updates mechanical. No runtime/VM to patch; base image has no libc. | Lowest: stdlib covers HTTP/templating/crypto. | Highest: npm transitive trees churn fastest; runtime image needs regular patching. |
| TTS integration | Same language for the TTS worker via the official `sherpa-onnx` Rust crate → one toolchain, one CI. | sherpa-onnx has official Go bindings too. | sherpa-onnx has Node bindings. |
| Owner fit | Owner's global rules are written for Rust (fmt/clippy rules) → he expects Rust. | — | — |

### 1.2 Choice

**Rust (option A).** It is the only option where the FSRS reference implementation *and* its optimiser run in-process, it produces a static binary for a libc-free distroless image, and the TTS worker can share the same toolchain, CI and Renovate setup. Go would be marginally lower-maintenance but would leave parameter fitting unsolved; Node loses on image hardening and supply-chain churn.

### 1.3 Frontend approach

- **No framework, no bundler, no runtime npm packages.** About nine screens; the only heavy-interaction screen is Practice. Plain ES modules served from the binary (`include_dir`), `// @ts-check` + JSDoc types, checked by `tsc --noEmit` in CI (dev-only dependency).
- **XSS safety by construction (R-84):** all DOM is created by `web/js/dom.js` (`h(tag, attrs, ...children)` → `createElement`/`textContent`). CI forbids `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `document.write`, `eval`, `new Function` in `web/` (grep gate). CSP `script-src 'self'` with no inline scripts.
- **Offline/PWA later is not blocked:** the client renders from JSON payloads that already contain everything a card needs; reviews are posted one by one with a client-generated UUID and client timestamp (idempotent); audio URLs are content-hashed and immutable; app shell assets are versioned (`/assets/<build-id>/…`). Phase 2 adds a service worker and an outbox queue without changing the API. The site is served over HTTPS (wildcard `*.example.home` cert in Traefik), i.e. a secure context, which service workers require.
- Answer grading (normalisation + exact match + suggested grade, spelling diff) lives **only in the client** (`web/js/grading.js`): it is mechanical, the server only stores the outcome, and offline practice will need it client-side anyway. One implementation, unit-tested with `node --test`.

---

## 2. System overview

```
 Phone / desktop browser ──HTTPS──► Traefik (entrypoint "home" :443, *.example.home cert)
                                          │  router vocabulary-home: Host(`vocab.example.home`)
                                          ▼  network proxy-net
                              ┌────────────────────────────┐
 Claude Desktop (Mac)         │ vocabulary  (vocab-server) │   /data  (rw)  SQLite + WAL
  └ vocab connector .mcpb ───►│  :8080  HTTP API + SPA     │   (config: environment)
    (bearer import token,     │  background: audio queue,  │   /data/cache  audio/*.mp3
     over LAN/VPN,      │  daily maintenance         │
     HTTPS via Traefik)       └──────────────┬─────────────┘
                                             │ network vocab-internal (internal: true, no egress)
                                             ▼ POST /v1/synthesize (shared token)
                              ┌────────────────────────────┐
                              │ vocabulary-tts (vocab-tts) │  Kokoro-82M v1.0 + sherpa-onnx,
                              │  :8090, stateless          │  model baked into the image, no volumes
                              └────────────────────────────┘
```

- The browser and the connector talk only to `vocab-server` through Traefik.
- `vocab-server` is the only component with state. `vocab-tts` is a pure function text → MP3.
- Day boundary ("study day"): timezone `Europe/Prague`, rollover at 04:00 local (configurable). All "same day", "due today", "new per day" rules use the study day.

---

## 3. Module map

Rule (R-92): every module owns its tables/files; SQL touching a table lives only in the owning module; other modules use the listed functions. Cross-module operations that must be atomic run in `workflows`, which opens one transaction and calls the modules' functions with it. Visibility: modules expose only the listed items (`pub` at crate level for these, everything else private); no `pub(crate)` shortcuts.

### 3.1 Crate `vocab-pack` (library, no I/O, no DB)

Pure format logic, shared by the server and by tests (and reusable later by a CLI).

| Item | Responsibility |
|---|---|
| `paste::split_paste(text) -> Vec<RawPack>` | Accepts a paste with one or several packs: strips Markdown code fences (```` ```json ````), reads a stream of JSON values (`serde_json::StreamDeserializer`); each value becomes one `RawPack { value, content_sha256 }`. |
| `validate::validate(&serde_json::Value) -> PackValidation` | Typed validation per §5.2. Returns `PackValidation { pack: Result<PackHeader, PackError>, words: Vec<WordResult>, warnings }`, where `WordResult = Ok(PackWord{ sentences: Vec<SentenceResult>, .. }) | Err(WordError)`. Errors carry a JSON pointer (`/words/3/sentences/1/en`) and a human message. |
| `marked::split(&str) -> Result<MarkedText, MarkError>` | The `[[…]]` scanner: `MarkedText { pre, target, post }`. Used at import and when the owner edits a sentence. |
| `keys::headword_key`, `keys::sentence_key`, `keys::item_key` | Mechanical identity keys (§5.3). |
| `hash::content_sha256(&Value)` | SHA-256 over the canonical serialisation (`serde_json::Value` with sorted object keys). |
| types | `PackHeader`, `PackSource`, `PackWord`, `PackSentence`, `PackTag`, `Goal`, `Origin`, … (serde, `deny_unknown_fields` is **not** used: unknown fields are collected as warnings). |

### 3.2 Crate `vocab-server` (binary)

Subcommands: `serve` (default), `healthcheck` (GET `http://127.0.0.1:8080/healthz`, exit code; used by the Compose healthcheck because the image has no shell/curl), `reset-password` (with the stack stopped: clear the password, every session, the connector token and the login-throttle state, §9.1), `logout-all` (with the stack stopped: end every session, §9.1), `check` (open DB, run migrations + `quick_check`, exit).

| Module | Responsibility | Owned state | Public interface (function level) |
|---|---|---|---|
| `config` | Load non-secret config from `VOCAB_*` environment variables (unknown `VOCAB_*` or invalid values = startup error); secrets are read by `auth`. | — | `Config::load() -> Result<Config>`; fields: `public_origin`, `day_rule`, `listen`, `log_level`, `session_days`, `limits`, paths. Secrets as opaque types with `Debug` that prints `***`. |
| `clock` | Time source and study-day arithmetic (injectable for tests). | — | `trait Clock { fn now(&self) -> Timestamp }`; `StudyDay::of(ts, &DayRule) -> StudyDay`; `StudyDay::{add_days, days_between}`. Uses `jiff` with bundled tzdb (the image has no `/usr/share/zoneinfo`). |
| `db` | Connection, PRAGMAs, migrations, integrity check; async wrappers. | the SQLite file set; `schema_migrations` via `PRAGMA user_version` | `Db::open(path) -> Result<Db>`; `Db::read(|&Connection| …)`, `Db::write(|&Transaction| …)` (run on `spawn_blocking`, one connection behind a mutex — single user, ample); `Db::read_detached` (a read-only connection of its own, for the export); `Db::scratch_file()` (an anonymous temporary file in the data directory and the free bytes there). |
| `catalog` | Vocabulary: sources, words, sentences, lists, forms, owner overrides, pack conflicts. Merge rules (§5.4). | tables `source`, `word`, `word_source`, `word_item`, `word_form`, `sentence`, `word_override`, `field_conflict`, `word_pack_spelling`, `id_floor` | `upsert_source(tx, &PackSource, ctx) -> SourceId`; `merge_word(tx, SourceId, &PackWord, ctx) -> WordMerge`; `preview_word(conn, &PackWord) -> WordMerge` (dry run); `word_view(conn, WordId)`; `search(conn, &WordQuery) -> Vec<WordRow>`; `update_word(tx, WordId, WordPatch)`; `delete_word(tx, WordId)`; `update_sentence(tx, SentenceId, en, cs)`; `set_sentence_hidden(tx, SentenceId, bool)`; `resolve_conflict(tx, ConflictId, Resolution)`; `add_owner_accepted(tx, WordId, &str)`; `traits(conn, &[WordId]) -> Vec<WordTraits>`; `presentation(conn, WordId) -> WordPresentation`; `speakables(conn) -> Vec<SpeakText>`; `sources(conn) -> Vec<SourceSummary>`; `feedback_items(conn) -> CatalogFeedback`; `export_all(conn) -> CatalogExport`. |
| `importer` | Preview/commit of packs, idempotency log. Orchestrates `vocab-pack`, `catalog`, `scheduler`, `audio` inside one transaction per pack. | table `pack_import` | `preview(db, text, Via) -> Vec<PackReport>`; `commit(db, text, Via) -> Vec<PackReport>`; `recent(conn) -> Vec<ImportLogRow>`. |
| `scheduler` | Cards, FSRS memory state, review log, leeches, card creation/unlock, FSRS parameters and fitting. | tables `card`, `review_log`, `fsrs_params` | `ensure_cards(tx, &WordTraits, day)`; `on_word_changed(tx, &WordTraits, day)`; `due_cards(conn, day, &Scope) -> Vec<DueCard>`; `new_cards(conn, &Scope) -> Vec<NewCard>`; `introduced_on(conn, day) -> u32`; `reviewed_words_on(conn, day) -> HashSet<WordId>`; `record_review(tx, ReviewInput) -> ReviewOutcome`; `undo_review(tx, ReviewId) -> UndoOutcome`; `sentence_last_shown(conn, WordId) -> HashMap<SentenceId, StudyDay>`; `leech_action(tx, WordId, LeechAction)` (its own part: acknowledge/suspend card); `reset_card(tx, CardId)`; `card_states(conn) -> Vec<CardState>`; `history(conn, WordId) -> Vec<ReviewRow>`; `review_stats(conn, range) -> ReviewStats`; `forecast(conn, day, days) -> Vec<u32>`; `fit_parameters_if_due(db, day) -> Option<FitResult>`; `export_all(conn)`. |
| `session` | Practice blocks: build, persist queue, requeue repeats, resume, undo, summary. | tables `practice_session`, `session_item` | `start_block(tx, day, Scope, &Settings, StartKind) -> SessionView`; `current(conn, day) -> Option<SessionView>`; `rate(tx, SessionId, RateInput) -> RateResult`; `undo_last(tx, SessionId) -> SessionView`; `finish(tx, SessionId) -> BlockSummary`. Calls `scheduler` and `catalog` read functions; never writes their tables. |
| `audio` | Clip index, generation queue + worker, TTS client, file store, garbage collection, file serving. | table `audio_clip`; directory `<cache dir>/audio` (default `/data/cache/audio`) | `clip_ref(conn, &SpeakText, Voice) -> ClipRef { url, ready }`; `request(tx, &[SpeakText], Voice)`; `spawn_worker(db, TtsClient, CacheDir, Notify)`; `open_clip(hash) -> Option<File>`; `status(conn) -> AudioStatus`; `retry_failed(tx)`; `gc(db, keep: HashSet<ClipHash>)`; `reconcile_files(db)`. |
| `auth` | First-run setup, password check and change, sessions (incl. "log out everywhere"), connector token, login throttling, Origin and fetch-metadata (CSRF) guard. | tables `auth_owner`, `auth_session`, `auth_throttle`; in-memory session cache and setup code | `login(db, password, ua) -> Result<SessionCookie>`; `logout(db, cookie)`; axum extractors `RequireSession`, `RequireImportToken`; middleware `origin_guard`. |
| `settings` | Owner preferences (new/day, block size, voice, autoplay, speed, theme, legend). | table `setting` | `get(conn) -> Settings`; `update(tx, SettingsPatch) -> Settings`. |
| `stats` | Read-only aggregations for Today/Progress/Stats. | — | `today(conn, day) -> TodayView`; `progress(conn, day) -> ProgressView`; `word_status(&[&Card]) -> WordStatus`; `problem_word_count(conn)`. Uses `catalog` + `scheduler` + `session` read functions (M9: `scheduler::recall_tallies`, `activity`, `leech_cards`, `Card::is_problem`/`leech_reason`; `catalog::problem_words`, `word_source_links`). |
| `exports` | R-57 feedback export, R-86 full export (§8.1). | — | `feedback(conn, day) -> FeedbackExport` (in memory: small); `write_full(conn, impl Write, now)` (row by row, never collected); `spool_full(conn, file, free_bytes, now)` (the same into a scratch file from `Db::scratch_file`, refused with `NoSpace` below the 256 MiB reserve). Uses `catalog::feedback_items`/`problem_words`/`export`, `importer::export`, `scheduler::export`, `settings::get`; the owning modules write their own tables through `db::write_table` into a `db::TableSink`. |
| `undo` | The Undo of the owner's latest change to a word: stores the word's state before/after (opaque, from `workflows`) under a token, valid once and for 10 minutes. | table `word_undo` | `record(tx, word, before, after, now) -> UndoToken`; `take(tx, word, token, now)`. |
| `workflows` | Atomic cross-module operations: commit import, edit word (+scheduler hook), leech actions (catalog + scheduler), delete word, maintenance; every owner word change through `with_undo` (catalog + scheduler snapshots) and `undo_change`. | — | `edit_word`, `leech_action`, `delete_word`, `include_low_priority`, `daily_maintenance`. |
| `web_assets` | Embedded frontend files, build id, index.html. | — | `router() -> Router`. |
| `http` | Router, handlers (thin: parse → call module → map errors), body limits, timeouts, security headers, error mapping, request logging (method, path template, status, duration — never bodies). | — | `app(state) -> Router`. |

### 3.3 Crate `vocab-tts` (binary)

| Module | Responsibility | Interface |
|---|---|---|
| `engine` | Owns the sherpa-onnx `OfflineTts` (Kokoro), loaded once at start; synthesis is serialised behind a mutex and run on `spawn_blocking`. | `Engine::load(model_dir, threads)`, `Engine::synthesize(text, voice_id, speed) -> Pcm` |
| `postprocess` | Trim silence, loudness normalisation (EBU R128 via `ebur128`), peak limit, MP3 encode (LAME). | `to_mp3(Pcm) -> Vec<u8>` |
| `http` | `POST /v1/synthesize`, `GET /v1/voices`, `GET /healthz`; token check; 2 KB body limit. | — |
| main | `serve`, `healthcheck`. | — |

### 3.4 Frontend (`web/`) and connector (`connector/`)

See §7 and §11.

---

## 4. Data model

SQLite, `STRICT` tables, foreign keys on. Text timestamps are RFC 3339 UTC (`…Z`); study days are `YYYY-MM-DD` text. All `*_key` columns are computed by `vocab-pack::keys` (§5.3).

### 4.1 PRAGMAs (every connection)

```sql
PRAGMA journal_mode = WAL;        -- crash-safe; -wal/-shm live next to the DB in /data
PRAGMA synchronous = FULL;        -- R-85 asks >= NORMAL; FULL costs nothing at this scale
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
PRAGMA temp_store = MEMORY;       -- read-only rootfs: never create temp files elsewhere
```

Startup: `PRAGMA quick_check` (refuse to start on failure, log the result), then migrations. Daily maintenance: `PRAGMA wal_checkpoint(TRUNCATE)` and `PRAGMA optimize`. Database file: `/data/vocabulary.sqlite` (+ `-wal`, `-shm`). Nothing else is written to `/data`.

### 4.2 Schema (migration `0001_initial.sql`)

```sql
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
```

Later migrations (the listing above is `0001`): `0002` makes `word.spelling` nullable (NULL = no pack said anything); `0003` adds `session_item.format`; `0004` adds the one-row table `auth_owner` (§9.1); `0005` (fix round R1, v0.1.1) adds `practice_session.kind` (`regular`/`extra`/`more_new`, §6.6, §6.9), `source.title_defaulted` (§5.4), `review_log.memory_updated` (§6.1), `auth_session.password_failures` and the one-row table `auth_throttle (id, state_json)` (§9.4), indexes `review_log(sentence_id)` and `session_item(sentence_id)`, and re-keys `sentence.en_key` with the wider folding of §5.3, merging sentences of a word that now share a key into the oldest row (reviews and practice items follow it, set-based): the merged row keeps the oldest id and takes the newest copy's Czech text and location (its English only when it marks another span), as an import merge would; if a copy was edited by the owner, the newest edited copy gives the text; `hidden`/`edited` are OR-ed. A sentence whose new key is empty (punctuation only) is never merged and keeps its key. Untitled sources v0.1.0 named after their slug or (type `general`) "General Vocabulary" are flagged `title_defaulted`. `0006` (M6, v0.2.0) adds `audio_clip.approximate` (words the TTS service pronounced by a guess), an index on `audio_clip(status)` and the one-row table `audio_engine (frontend, engine, checked_at)`: the TTS service's versions, part of every clip's identity (§10.3). `0007` (M7, v0.3.0) adds `word_pack_spelling (key PRIMARY KEY, word_id, headword)` — every headword key a pack used for a word (seeded with every stored headword, all of which came from packs); the merge matches a pack word by its current key or a pack spelling, so a re-import still finds a word whose headword the owner changed, while the owner's own (intermediate) spellings are never kept (§5.4) — `word_form.removed` (a form the owner removed is never re-added by a pack, like `word_item.removed`), an index on `word(created_at)` (list sort "newest"), `id_floor (name, last_id)` with `AFTER DELETE` triggers on `word`, `sentence` and `field_conflict` (they also fire for cascades): new rows of these tables get an id above the highest stored and the highest deleted one, so an id in a URL (`/api/words/{id}`, `/api/sentences/{id}`, `/api/conflicts/{id}`) never names a newer row after a delete (chosen over `AUTOINCREMENT`, which would need rebuilding `word` and `sentence`, parents of the learning history, under foreign keys), and `word_undo (word_id PRIMARY KEY, token, expires_at, before_json, after_json)` — the Undo of the owner's latest change to a word (§8).

Card states: `new` (never reviewed; Understand cards have `due_day` NULL and are introduced through the new-word quota, Produce/Spelling cards get a `due_day` when unlocked and are then treated as due items), `relearning` (last rating today was 4/5; due again today, repeated in-session), `review` (normal). FSRS itself is stateless about these labels; they only drive the session builder.

### 4.3 Migrations

- `vocab-server/migrations/NNNN_name.sql`, embedded with `include_str!`, applied in order inside one transaction each; `PRAGMA user_version` = last applied number. A migration may have a data step in Rust that runs after its SQL in the same transaction, for what SQL cannot compute (0005: sentence keys).
- Forward-only. If the DB's `user_version` is higher than the binary knows → refuse to start (protects against running an older image on a newer DB).
- Each migration has a test that applies all migrations to an empty DB and to a DB at the previous version filled with a synthetic fixture, then runs `PRAGMA foreign_key_check` + `integrity_check`.
- Upgrade test from the deployed release (`tests/upgrade.rs`, R-85): a database exactly as v0.1.0 leaves it (0001–0004, data the app wrote through its API: imports, reviews, a block in progress, the owner's password and two logged-in devices, plus sentence copies the 0005 key merges) is opened by the current app: every v0.1.0 row is still there (copies merged into the oldest row, references moved), foreign keys and `quick_check` hold, the old sessions and the block in progress keep working, login works, and sessions then follow the current rules (365-day absolute lifetime, password change).
- Before upgrades that contain a migration, the release notes tell the owner to take a manual ZFS snapshot (`zfs snapshot tank/services/vocabulary@pre-vX.Y.Z`).

---

## 5. Import

### 5.1 Flow

Two entry points, same code path (`importer`):

1. **Paste page (R-11):** owner pastes one or more packs (with or without ```` ```json ```` fences) → `POST /api/import/preview` → report per pack: new words, merged words (with added sentences/translations), unchanged words, field conflicts (kept existing), word/sentence errors, warnings, "already imported" → owner presses **Import** → `POST /api/import/commit` with the same text → server re-validates and commits each pack in its own transaction → final report. Preview stores nothing; commit is deterministic given the DB state, so no preview token is needed.
2. **Connector (R-12):** `POST /api/connector/v1/packs` with `{ "pack": {…}, "dry_run": false }` and the bearer token. `dry_run: true` = preview. No interactive confirmation: Claude gets the report and fixes errors itself.

Per pack, `commit`:
1. `split_paste` → `validate`. Pack-level error → pack rejected, reported.
2. Look up `pack_import (pack_id, content_sha256)`. Present → **no-op**, report "already imported on …" (R-11 idempotency).
3. Same `pack_id` with a *different* hash → import normally, warning "pack_id reused with different content" (two chats can both produce `…--2026-11-20--1`; the merge is idempotent anyway).
4. Insert `pack_import`, `catalog::upsert_source`, then for every valid word `catalog::merge_word` → `scheduler::ensure_cards` → `audio::request` for the word's speak text and new/changed sentences.
5. Store the report (counts, headwords, outcome per word; **no sentence text** — R-24 keeps book text out of logs; the DB itself is the owner's private store).

A pack whose import fails unexpectedly (a database error), or that would pass a hard cap of the catalog (20 000 words, 200 000 sentences; constants in `catalog`), is rolled back as a whole and reported with status `failed` (error code `import_failed` or `capacity`) in the normal report; the other packs of the paste are imported as usual, and the preview reports the same (R-23: a paste that was partly stored is never answered with a bare error).

One import (preview or commit) is processed at a time, its request body included; a second one waits briefly and is then refused with `503 busy` and `Retry-After` (§9.4).

### 5.2 Validation rules (R-23, R-84)

Errors are scoped: **pack** errors reject the pack; **word** errors drop that word; **sentence** errors drop that sentence (the word is still imported). Everything is listed in the preview; nothing is dropped silently.

| Scope | Rule | Severity |
|---|---|---|
| Paste | Total paste ≤ 2 MiB; one JSON value ≤ 1 MiB, ≤ 60 000 JSON values and ≤ 8 levels deep; ≤ 20 packs and ≤ 200 000 JSON values per paste. A JSON value is first only measured (bytes, values, depth) and is built only when it is within these limits, so hostile JSON costs a scan, not memory | pack error |
| Pack | Valid JSON object; `format == "vocab-pack"`; `format_version` is an integer the app knows (only `1`) — unknown → "This pack uses format N; update the app / re-paste the instructions" | pack error |
| Pack | `pack_id` matches `^[a-z0-9]+(-[a-z0-9]+)*--YYYY-MM-DD--n$`; `source` object valid (`id` pattern `^[a-z0-9]+(-[a-z0-9]+)*$`, `type` enum, `title` 1–300 chars); `words` array 1–100 items; `exported_at` a real calendar date if present (`2026-02-30` is none); `source.url` starts with `http://` or `https://`; `part.index ≤ part.total` | pack error |
| Pack | `pack_id` prefix ≠ `source.id`; `reading_position` present for non-book source; `part` missing (defaults to 1/1); `exported_at` later than the day after the import's study day (the reading position is then dated the import's study day, §5.4) | warning |
| Word | `headword` 1–100 chars with at least one visible character (not only whitespace or invisible characters: its key must not be empty); `meaning` 1–1500, not blank; `czech` ≥ 1 item, items 1–200 chars; `usefulness` integer 1–10; `goal` in {recognise, active} if present; `forms` items `{form, common}`; `tag` `{kind ∈ enum, text?}`; lists ≤ 50 items; `note` ≤ 6000; `hint` ≤ 300; `seq` integer ≥ 1; text fields are strings | word error |
| All | `null` for an optional property means "not given": `forms`, `accepted`, `confusables`, `goal`, `spelling`, `tag`, `seq`, `sentences` (= no sentences), every optional text, `source.title`, `exported_at`, `part` | — |
| Word | Same `headword_key` twice in one pack → second occurrence | word error |
| Word | Unknown property → ignored | warning |
| Word | `spelling_note` without `spelling: true`; fewer than 3 valid sentences ("will use the word-only card"); `hint` > 120 chars | warning |
| Sentence | `en` and `cs` each contain **exactly one** `[[…]]` span: scanner finds the first `[[`, the first `]]` after it, and no other `[[` or `]]` anywhere; span non-empty, no leading/trailing whitespace inside the brackets, not starting with `]` (`[[]x]]`); the text outside the markers may be empty | sentence error |
| Sentence | `en`, `cs` ≤ 600 chars; `cs` non-empty translation (after removing markers it has ≥ 1 letter); `origin` ∈ {book, source, example}; `location` ≤ 100, text or a number (a page; stored as text) | sentence error |
| Sentence | Duplicate of another sentence of the same word in the same pack (same `en_key`) | warning, dropped |
| All text | `\r\n` and a lone `\r` are read as `\n`; other control characters than `\n`/`\t` are rejected; text is otherwise stored as given (NFC-normalised) and is **never** interpreted as HTML or Markdown anywhere | sentence/word error |

The JSON Schema (`docs/vocab-pack.schema.json`) encodes the same contract. Conformance test: every fixture in `testdata/packs/{valid,invalid}/` must have the same verdict from the schema (validated with the `jsonschema` crate as a dev-dependency) and from the importer, except the documented leniency (unknown properties = warning in the importer, invalid in the schema) and the rules a schema cannot express (`invalid/importer-only--*`: a repeated headword, control characters, a Czech translation without letters, `part.index > part.total`, an `exported_at` that is no calendar date). The same test checks a table of single-value changes to a valid pack (every `null`, line break, number, blank and URL case above), so the validator, the schema and the instruction text cannot drift apart unnoticed.

### 5.3 Identity keys (mechanical normalisation only)

- `headword_key`: Unicode NFC → trim → collapse inner whitespace to one space → lowercase → typographic apostrophes `’ ‘ ʼ` → `'`; hyphen variants U+2010/U+2011 → `-`. (`Pore over` = `pore  over`; `carpet-bomb` ≠ `carpet bomb` — that would need judgement, so it isn't done.)
- `sentence_key`: markers removed, then the same steps as `headword_key`, plus the variants a model changes freely between two exports of one sentence: typographic quotes (`“ ” „ « »`, `‚ ‛`) → straight ones; `...` = `…`; en/em dashes → `-`; the spaces around a dash or an ellipsis are dropped (`a — b` = `a—b` = `a - b`); and quotes, ellipses, whitespace and closing sentence punctuation (`.`, `!`, `?`) at the ends are ignored (Claude may cut or punctuate a quote differently between exports). Words and inner punctuation still count. Migration 0005 re-keyed the stored sentences.
- `item_key` (Czech equivalents, accepted answers, confusables, forms): NFC → trim → collapse whitespace → lowercase.

### 5.4 Merge rules (R-03, R-22) — precise

A pack word is matched to an existing word by `headword_key`. Learning state (cards, reviews) is **never** touched by a merge, except that new cards may be created/unlocked by `scheduler::ensure_cards`.

| Field class | Fields | Rule |
|---|---|---|
| Identity | `headword` | Matched by key: the word's current `headword_key`, or a spelling an earlier pack used for it (`word_pack_spelling`; every merge records the pack's spelling, the first one of a key is kept). Displayed spelling: existing kept. Two entries of one pack that land on the same word (a renamed word's pack spelling and its new headword) are both merged into it, and the report warns, naming both. |
| Scalars | `grammar`, `pronunciation`, `speak`, `meaning`, `usefulness`, `goal`, `hint`, `spelling`, `spelling_note`, `note`, `icon` | (1) incoming absent/null → nothing. (2) existing empty/NULL → **take incoming**. (3) equal after trim+NFC → nothing. (4) field is owner-overridden (`word_override`) → keep owner value, record a `field_conflict` (status open) so the owner sees what the pack suggests. (5) otherwise → **keep existing, record a `field_conflict`**. Open conflicts show as a badge in word detail with *Use new* / *Keep*; an existing open conflict for the same field and same incoming value is not duplicated. |
| Lists | `czech`, `accepted`, `confusables` | **Union** by `item_key`, existing order first, new items appended. Items the owner removed (`removed = 1`) are not re-added. The owner's accepted answers (`owner_accepted`) belong to the accepted list: a pack never adds one the owner added or removed, and the detail lists one entry per key. Nothing is ever deleted by a pack. |
| Forms | `forms` | Union by `form_key`; for an existing form a different `common` flag keeps the existing one (no conflict record — low value). |
| Sentences | `sentences` | Dedupe by `(word, sentence_key)` — the stored key, and also the key of the stored text (an edited sentence keeps the pack's key; a pack bringing exactly the owner's corrected text recognises it); new sentences appended with `source_id` = pack source and `pack_import_id`. A known sentence whose `cs` (translation or its span) or marked English span differs in the pack is **updated in place** to the pack's version (same row and id, so its review history stays; corrections must reach the app) and counted as "N sentences updated" — unless the owner edited or hid it (`edited`/`hidden`), then it stays exactly as it is. English that differs only in what the key folds (how the quote was cut or punctuated) is not a correction: the stored text stays. |
| Source link | `source`, order, `tag` | `word_source (word, source)` created if missing with `seq` = next order value (§5.5); if present, `last_pack_import_id` is updated and `tag_kind/tag_text` are replaced when the pack gives a tag object (it describes this source relationship). A word without `tag`, or with `"tag": null`, says nothing: the stored tag stays (a re-export must not un-skip a low-priority word). |
| Low priority | derived | `word.low_priority` = owner override if set, else "the most recently imported link has tag `low-priority`". Seeing the word again in a new source without the tag therefore promotes it. |
| Source record | title, author, series, series_number, url | Fill empty fields; never overwrite non-empty ones. One exception: a title the app made up because the first pack had none (`title_defaulted = 1`: "General Vocabulary" or the source id) is replaced by the first real title a later pack brings. |
| Reading position (R-59) | pack `reading_position` | Stored on the source when the pack's date ≥ the stored `reading_position_exported_at` (ties: later import wins). The pack's date is its `exported_at`; a pack without one is dated the **study day** of the import (local time, not UTC), and so is a pack whose `exported_at` is later than the day after that study day (with a warning: a mistyped year must not block every later position). When a position is not applied, the report says why (`reading_position.reason`). Shown on Today, Progress and in the session scope chooser. |

**"Update the sentences" re-exports** (instruction text: Claude re-exports the existing words of a book with sentences from newly read chapters): handled by the same rules — new sentences are appended, repeated ones skipped by key (or corrected in place), learning state untouched. Claude may rephrase scalars when re-exporting; rule (5) keeps the existing values and only records conflicts, so the owner's data never drifts silently. The import report for such packs leads with "N new sentences for M words" and folds conflicts into a collapsed line.

### 5.5 Order within a source (`seq`, R-01/R-13)

The v1 format has no `seq` field; order is the array order. `seq` for a new link = `max(seq in source) + 1` in the order: packs as imported, words in array order. Multi-part exports carry `part.index`; the paste page sorts packs of one paste by `(source, exported_at, pack n, part.index)` before committing, so pasting parts in any order still yields the right `seq`. (Proposal P-1 in §19 adds an optional `seq` for converted old pages.)

### 5.6 Format versioning

- The importer has one validator per known `format_version`; v1 stays importable forever.
- Any incompatible change → `format_version: 2` + a new validator that maps into the same internal `PackWord` types; the instruction text and schema are updated together. Additive optional fields that old packs simply lack do **not** need a version bump only if the importer already tolerates them (unknown fields are warnings in v1, so v1 importers stay safe against v1+ fields).

---

## 6. Scheduling and sessions

### 6.1 FSRS library and mapping

- `fsrs` crate **6.6.2** (FSRS-6, 21 parameters). Scheduling: `FSRS::new(&params)?.next_states(memory, desired_retention, days_elapsed)` → pick `again/hard/good/easy` → new `MemoryState` + interval (days). Interval rounded to the nearest whole day, clamped to `[1, 36500]`. No fuzz (deterministic, testable; the deck is small).
- `days_elapsed` = study days between the card's previous review and now (0 for same-day repeats; FSRS-6 models same-day reviews).
- **One bad day counts once (R-50, R-54):** per card and study day, only the **first** failing rating (4/5 → Again) lowers the memory state. Further failing ratings of the same card on that study day are stored in the review log with their raw grade (`review_log.memory_updated = 0`) and requeue the card in the block as usual (§6.7 step 7), but leave stability and difficulty as they are; the card stays `relearning`, due today. Passing ratings always apply. Without this, failing the in-session repeats two or three times pushed a mature card's stability to the floor and cost weeks of extra reviews for one bad day. Whatever replays the history (parameter fitting, memory-state recompute) skips rows with `memory_updated = 0`.
- **1–5 → FSRS (R-60):**

| Raw | Label | FSRS rating applied |
|---|---|---|
| 1 | Instantly | **Good** until personal parameters are active, **Easy** afterwards |
| 2 | Knew it | Good |
| 3 | Shaky | Hard |
| 4 | Vague | Again (in-session repeat after ~7 cards); memory lowered only by the first 4/5 of the study day |
| 5 | No idea | Again (in-session repeat after ~3 cards; full learn view); likewise |

  The raw grade and the applied FSRS rating are both stored (R-61).
- **Parameter fitting:** `scheduler::fit_parameters_if_due` runs in the daily maintenance task. Trigger: ≥ 400 non-undone reviews with `elapsed_days ≥ 1` and ≥ 30 days since the first review, then every 30 days. Training items are built from the full review history with raw 1 → Easy (the owner's meaning of "1" never changed; only the scheduler's interpretation did). New parameters are activated only if their log-loss on the history beats the current ones (`FSRS::evaluate`). On activation: memory states of all cards are recomputed from their history (`memory_state`), due dates are kept (no workload spike), and from then on raw 1 → Easy. Settings shows "Personal parameters since …". One parameter set for all card kinds in phase 1 (too little data to split).

### 6.2 Desired retention (R-51, R-58)

| Card | Desired retention |
|---|---|
| Understand / Produce, usefulness 8–10 | 0.93 |
| usefulness 6–7 | 0.90 |
| usefulness 4–5 | 0.87 |
| usefulness 1–3, and low-priority words the owner included | 0.82 |
| Spelling (any usefulness) | 0.80 |

Effective usefulness = owner's value if overridden, else pack value. The retention is read at review time, so changing usefulness affects the next interval (no rescheduling of existing due dates). Error rate is not added on top (FSRS difficulty already reflects it).

### 6.3 Effective goal

`effective_goal = owner override (word_override 'goal') ?? pack goal ?? (usefulness ≥ 6 ? active : recognise)`; low-priority words that the owner included are always `recognise` (R-51).

### 6.4 Card creation and unlock (R-52, R-58)

| Card | Created | Format (fixed for the card's lifetime, with one one-way upgrade) |
|---|---|---|
| **Understand** | At import, for every word (state `new`). | Chosen at its first review: `sentence` if the word has ≥ 3 visible sentences then, else `word_only`. One automatic switch, one-way: a card fixed to `word_only` becomes `sentence` as soon as the word has ≥ 3 visible sentences (checked by `scheduler::ensure_cards` at every import, at start and on word changes; the memory state is kept, because the card was only word-only for lack of sentences). Never back: a sentence card stays one when sentences are hidden later. A practice item already queued keeps the format it was built with. "Reset card" in word detail clears the card's state and re-chooses (explicit owner action). |
| **Produce** | When effective goal = `active` **and** the Understand card has stability ≥ 7 days after a review **and** the word has ≥ 1 visible sentence. Checked after every Understand review and on goal/word changes. Created in state `new` with `due_day = today + 1` and no memory state; from that day it waits for its first review, which initialises the memory state. First-time Produce/Spelling cards come in only up to the daily cap `new_typed_per_day` (§6.6, M8), not against the new-word limit. | `cloze` |
| **Spelling** | When `spelling = true` **and** the Understand card is past the new stage = state `review` with at least one passing review (raw ≤ 3) on a day after its introduction day. `due_day = today + 1`. | `spelling` |

- Goal changed to `recognise` (owner or leech action) → Produce card suspended (state kept; re-activated if the goal goes back to `active`). `spelling` set to false → Spelling card suspended.
- Word suspended → all its cards are excluded from sessions, also from the block already open: its unanswered items (and those of a card suspended by a goal or spelling switch, and new items of a low-priority word that was excluded) are left out of the queue, the counts and the rating order while that holds; the stored items stay, so unsuspend or an Undo brings them back in place. The word list shows no next due day for a suspended word.
- "Reset card" takes the card's unanswered items out of the open blocks. A card introduced today keeps today as its introduction day, so the reset does not free a new-word slot (§6.6); a card reset on an earlier day counts as introduced again on the day of its next first rating.
- Cards unlocked by a review are listed in that review's `created_card_ids`, so undo removes them.

### 6.5 Leeches (R-55)

- **Lapse** = the first failing review (raw 4/5) of a study day of a card that is not new, i.e. in state `review` **or** `relearning` (still failed from an earlier day); at most one lapse per card and study day. Failing the in-session repeats is not another lapse, and passing them does not take the lapse back. (A word failed day after day, repeats included, therefore does become a leech.)
- **Flag** when `lapses − leech_lapses_at_ack ≥ 4`, or when a card gets two raw 5s in a row and its stability before the first of them was ≥ 21 days ("previously mature"). Flag = `leech = 'flagged'`. No automatic suspension.
- The next time a flagged card comes up, the answer side is replaced by the **leech screen** (§7.4): the pack's `hint` (or "Hint requested for the next pack"), the pack's `confusables`, and actions: *Make recognise-only* (goal override → recognise), *Lower usefulness* (picker), *Suspend word*, *Keep practising*. For usefulness ≤ 4 the *Suspend* button is the primary one. Any action sets `leech = 'acknowledged'` and `leech_lapses_at_ack = lapses` (so 4 further lapses flag again).
- Flagged/acknowledged words are listed on the Progress screen with their actions; a word leaves the list after its card reaches stability ≥ 21 days or the word is suspended.
- **M9 as built:** a *problem card* is a flagged card, or an acknowledged one with stability < 21 days (`Card::is_problem`, the one rule behind the Words status "Problem word", the Progress list, Today's count and the feedback export); a *problem word* has one and is not suspended. `Card::leech_reason` says why: `lapses` (lapses since the last acknowledgement ≥ 4), `forgot_when_known` (flagged with fewer: the two-5s rule) or `acknowledged`. The choices are `POST /api/words/{id}/actions` `leech_keep` (*Keep practising*), `leech_recognise`, `leech_usefulness {usefulness}` (1 … current − 1, else `422`), `leech_suspend`; each runs the catalog change, then acknowledges every flagged card of the word, then `ensure_cards` (recognise-only suspends the Produce card) — in one transaction, with the word's Undo (the flag comes back). In practice the leech panel opens **above** the answer side of a flagged card (not instead of it): its hint (or "no memory hint yet") and confusables, the choices, a link to the word (write a hint, fix a sentence, reset); the card is then rated as usual. A choice that takes the item out of the block (suspend; recognise-only on a Produce card) moves on to the next card. Usefulness ≤ 4 makes *Suspend word* the primary choice. "Lower usefulness" opens a row of the lower values (buttons; a native `<select>` makes WebKit break the CSP). Today shows a quiet "N problem words: see Progress" line, the Words status filter "Problem word" links to Progress; no badges or alerts (R-74).

### 6.6 Daily new-word limit and backlog pause (R-54)

- `new_per_day` setting (0–100, default 5) counts Understand cards introduced on the study day.
- `normal_load` = mean number of reviews per study day (excluding new introductions and in-session repeats) over the last 14 days, floor 20.
- **Pause** new words when `due_now > 2 × normal_load`, where `due_now` = cards with `due_day ≤ today` (all scopes). Today screen shows "New words paused — backlog of N reviews" with a one-tap *Add new words anyway (today)*.
- **Learn more new words** (R-54, `POST /api/sessions {kind: "more_new", count?}`): a block of `count` unseen words only (1–50; default `max(new_per_day, 5)`), chosen like step 4 of §6.7 (candidates, order, interference rule, scope), regardless of the day's quota and of the backlog pause (the pause itself stays). They are introduced like any new word, so they count toward `introduced_today` and use up what is left of the day's quota. Stored with `practice_session.kind = 'more_new'`.

- **First-time Produce/Spelling cards (M8):** `new_typed_per_day` setting (0–100, default 10) caps the Produce/Spelling cards that get their first review on a study day (counted by `introduced_day`). The backlog pause and *Add new words anyway* apply to them as to new words. They are not a review backlog: `due_now` of the pause and the forecast count reviewed cards only. Today: `new_typed {per_day, introduced_today, left_today, quota, paused}`, `typed_waiting` (first-time cards in scope whose due day has come) and `typed_today` (of those, the ones today's blocks serve). **One source of truth (M8 fix round):** `session::due_today` derives Today's `due_now`, `relearning`, `typed_today` and `next_block {due, new}` (the plan a Start would build now) from the same function the block builder uses (`due_cards`: due cards one per word, the sibling rule, the cap, no first-time card for a word with a due review), and extra practice's "nothing due" check uses it too. So Today never offers a Start that yields nothing; a day whose remaining first-time cards are all blocked by the sibling rule is done and extra practice is offered; Today's line says those cards come on a later day. With `new_typed_per_day = 0` Today says first-time Produce cards are switched off and names the setting.

### 6.7 Session building (R-54, R-59, research §5)

`session::start_block(day, scope, settings, kind)`; `scope` = all sources (default) or one source (R-59: only words linked to that source). Block size from settings (default 20).

1. **Exclusions — same-day siblings:** at most **one card per word per study day** (generalises R-52's Understand/Produce rule to Spelling; Spelling after seeing the word written in Understand/Produce would be trivial). Words that already had a review today on another card, or have another card in today's unfinished queue, are skipped. In-session repeats of the *same* card are allowed.
2. **Due cards:** `due_day` not NULL (all reviewed cards, plus unlocked Produce/Spelling cards), not suspended, word not suspended, `due_day ≤ today`, in scope, not excluded. Sort by priority = `(1 − R_now / R_target) × w(u)`, descending, where `R_now` is the FSRS retrievability today, `R_target` the card's desired retention, `w(u) = 0.5 + u/10` (u = usefulness). Relearning cards (rated 4/5 earlier today, session abandoned) come first. One card per word (highest priority). A first-time Produce/Spelling card (no memory state) **never goes ahead of a due review** (on time or overdue, whatever its priority, M8 fix round): first-time cards sort after every due review and fill what is left of the block; they join only up to the day's cap (§6.6), those waiting longest first (then usefulness, card id), and never in place of a due review of the same word. Today counts exactly these cards (§6.6).
3. **Spelling cap:** at most 3 Spelling cards per block; the rest wait (they stay due).
4. **New words:** quota = `new_per_day − introduced_today` (0 if paused). Candidates: Understand cards in state `new`, word not suspended, not low-priority unless included, in scope. Order: usefulness desc → source `seq` asc → word id asc. **Interference rule:** skip a candidate if one of its `confusables` (by `item_key`) is the headword of a word introduced today or already chosen, and vice versa (similar words are not introduced on the same day — §15).
5. **Interleave:** fill the block with due cards; place new cards at every 4th position (3, 7, 11, …); if due cards run out, fill with further new cards up to the quota; if both run out, the block is shorter. "Continue +20" builds the next block the same way.
6. **Sentence choice per item:** among the word's visible sentences, the one least recently shown (from `scheduler::sentence_last_shown`, across all card kinds), excluding sentences shown in the last 7 study days when an alternative exists; ties → lowest position. A repeat item always takes a different sentence than the previous attempt when one exists.
7. **Requeue (in-session repeats):** raw 5 → repeat item inserted after 3 more items; raw 4 → after 7; if fewer remain, appended at the end. A card is repeated at most 3 times per block; after that it stays `relearning` and due today/tomorrow. A repeat answered 1–3 → FSRS update with `days_elapsed = 0` → interval ≥ 1 day. A repeat answered 4/5 again is logged and requeued the same way but does not lower the memory state a second time that day (§6.1).
8. **Persistence and resume:** the block (items with order) is stored; every rating is saved immediately (`review_log` + card update in one transaction); reopening the app shows "Resume — 12 of 20 left". Blocks expire at day rollover (unanswered items are simply not reviewed; nothing is lost).
9. **Undo (R-54):** "Undo" reverts the **last non-undone rating of the active session** (repeatable step by step): card's scheduling state restored from `before_json` (its suspension stays as it is now: a rating never changes suspension, so neither does its Undo — e.g. after a goal switch to "recognise only"), unlocked cards deleted if still unreviewed, inserted repeat item removed, review marked `undone` (kept for audit, ignored everywhere else). Refused (`409 card_changed`) when the card was reset in word detail after that rating (its `last_review_at` is no longer the rating's): restoring `before_json` would undo the reset too. Toast with Undo for 4 s after each rating; ↶ in the top bar and `U` on the keyboard work any time during the block.
10. **New words** are tested cold (Understand card, "New" badge); after reveal the answer side opens the full learn view (note expanded, all forms, the word's other sentences behind "More"), then the owner rates as usual (R-52).

### 6.8 Answer checking (R-56, R-58) — client side, mechanical

- **Produce normalisation:** lowercase → NFKD → drop combining marks (diacritics) → fold the ligatures and stroke letters æ œ ø ł đ (ß stays: no language rules) → drop apostrophe-like letters (ʼ ʻ ꞌ …) → hyphens, dashes, the minus sign and runs of white space become **one space** → drop everything else that is not a letter, digit or space (punctuation, quotes, invisible characters) → trim. Inner space is significant: *pot-luck* = *pot luck*, but neither is *potluck*, and *a part* ≠ *apart*, *any way* ≠ *anyway* — a spaced and a closed form match only when the pack or the owner lists both (M8 fix round). Compared with the item's **accept set** = pack `accepted` ∪ headword ∪ the target span of the shown sentence ∪ owner-accepted answers (all supplied by the server per item), plus answers the owner accepted with "Also accept" earlier in the same block (a repeat of the word accepts them at once), and the word's listed `forms`.
- **Verdict and suggested grade (guarantor decision of 2026-11-27):** with a sentence (a blank) shown, only the **target span of that sentence** is "✓ Correct" → **2**, after the hint (first letter + letter count) **3**. The headword or another listed form of the word is a separate, friendly verdict — "Right word — here it is “dwindled”" → **3** — never an error and never fully correct. The pack's `accepted` and the owner's answers do not say whether an entry is a form or a full alternative for this blank, so an accepted answer that is not the target span is treated the same way ("Accepted — this sentence has “…”" → **3**). Without a sentence (gloss-only card) every accepted answer, the headword included, is "Correct" (2 / 3 after the hint). No match → **4** (typed answer shown above the correct one in monospace, "Just a typo? Choose 3."); "I don't know" → **5**; "I'll just rate myself" → no suggestion, reveal → rate. Never suggests 1. The review log keeps `answer_match = 'exact'` for all three match verdicts (its `CHECK` constraint cannot take a new value without rebuilding `review_log`); the distinction is in `suggested_grade` (3 without the hint = right word / accepted alternative) and, fully, in `typed_answer` with `sentence_id`. The server rejects (`422`) details that disagree: `exact` → 2 or 3 (only 3 with `hint_used`), `none` → 4, `dont_know` → 5, `skipped` → no suggestion; a typed answer of only white space and invisible characters (U+200B, U+FEFF, …) is blank. Overriding a 4/5 suggestion to 1–3 offers "Also accept '…' next time?" → `owner_accepted` item (exported in the feedback file); `409 item_exists` counts as added; not offered for an answer longer than the list's 200 characters.
- **Kept across a reload:** what is typed, the hint and — once checked — the check are kept per item in the tab (sessionStorage) until the item is rated: a reload after Check returns to the checked answer side; undoing the previous card never touches the current card's draft.
- **All sentences hidden:** a Produce card whose word has no visible sentence left is served with the Czech gloss only (the headword is then "Correct"); left as is.
- **Spelling:** strict — compare after trimming and lowercasing only. Exact → suggest 2, else 4. Letter-by-letter diff: Levenshtein alignment (pure function in `grading.js`) rendered as per-letter marks (correct / wrong / missing / extra) with colour **and** symbol.

---

### 6.9 Extra practice (R-63)

`POST /api/sessions {kind: "extra", scope_source_id?}` is offered **only while no card is due in the scope** (due cards of the served kinds, words and cards not suspended); otherwise it answers the empty start and Today's `extra_available` is 0. It builds a block (size = `block_size` setting) of cards that are **not due**: cards of the served kinds with at least one review (`reps ≥ 1`), card and word not suspended, in scope, `due_day > today`, one card per word. A card whose latest non-undone rating today was 1–2 is left out (just shown and known). Order: cards rated 3–5 today first, then lowest current FSRS retrievability, then card id. Items are stored with origin `due` and the block with `practice_session.kind = 'extra'`; requeue, undo, resume and summary work as in §6.7. A rating is an ordinary review: FSRS gets the real elapsed days (an early review raises stability less), the one-failure-a-day rule of §6.1 and the lapse rule of §6.5 apply. When nothing qualifies the answer is the empty start `{session: null, resumed: false}`. An unfinished block of today is resumed instead, whatever kind is asked for.

## 7. UI

### 7.1 Screens (from the research wireframes, R-74)

| # | Screen | Route | Notes |
|---|---|---|---|
| 0 | Login | `#/login` | Password field, "Stay signed in" is implicit (180-day cookie). |
| 1 | **Today** | `#/` | Due now / later today, 7-day forecast, new words left today (or "paused — backlog"), **scope chooser** (All sources · each source with its reading position, R-59; last choice remembered per device), Start / Resume, per-source progress strip. |
| 2 | Practice — Understand question / revealed (sentence) | `#/practice` | Wireframes 2 and 3. Criterion line above the button: "Rate the highlighted word: would you have understood it here, without help?" |
| 2b | Practice — Understand word-only | `#/practice` | Wireframe 2b; reveal shows Czech gloss, meaning, one example with its translation. |
| 3 | Practice — Produce question / answer | `#/practice` | Wireframes 4/4b. No audio on the question side. Input: `autocomplete=off autocorrect=off autocapitalize=none spellcheck=false enterkeyhint=go`, kept above the keyboard with `visualViewport`. When the visible height is under 300 px with the keyboard open (a phone in landscape), the bottom bar keeps only the hint and the answer row and the blanked sentence is scrolled into the room above it; portrait keeps the full bar. |
| 3b | Practice — Spelling question / answer | `#/practice` | ▶ word (auto-plays where allowed), Czech gloss as disambiguating cue, optional blanked sentence text (no sentence audio — it says the word). Answer: letter diff, `spelling_note`. |
| 4 | Leech screen | above the answer side | §6.5 (M9 as built). |
| 5 | Block summary | `#/summary` | Wireframe 6: rating distribution, "look at these again", Done / Continue +20. |
| 6 | Words (browse/search, R-73) | `#/words` | Search (headword, Czech, meaning — plain substring on keys), sort (alphabetical, usefulness, added, next due, status), filters (source, usefulness range, status, has conflicts, suspended, low priority). |
| 7 | Word detail + edit (R-04, R-25) | `#/words/:id` | Wireframe 7 plus: edit any field, sentence edit/hide, conflicts (*Use new* / *Keep*), accepted answers (pack + mine, removable), the pack spellings a renamed word still answers to (removable), goal switch, usefulness ±, include low-priority, suspend, reset card, delete word (confirm; Esc cancels), per-card history (date, card, sentence, raw grade, typed answer, hint, next due, recall chance). Every change is saved at once; its toast's Undo restores the exact state before it on the server (§8). A save, an Undo or a switch never loses text typed in another open editor or "add" field, and keeps the scroll position and focus. Single-line fields (headword, grammar, pronunciation, speak, spelling note, icon, list items, forms) take no line break; a spelling note needs spelling practice on (as the import stores it). |
| 8 | Import | `#/import` | Paste area → Preview → Import; recent imports list. Link to the instruction file in the repo. |
| 9 | Progress (R-72) | `#/progress` | Due today / this week, forecast, per-source progress (new / learning / familiar / known / can produce / leech, reading position), real retention 30 days per card kind vs target, 30-day activity strip, leech list, words with open conflicts. **M9 as built** (in the navigation between Words and Import): *Coming up* — cards due today (Today's `due_now`, all sources) and a 14-day bar chart of reviews falling due (`forecast`, Today's first 7 days, `due_next_7_days` their sum); *Your words* — all words by status (known ≥ 21 d, familiar 7–21 d, learning, problem, new, suspended: they partition the words) plus "can produce" (Produce card in review with stability ≥ 7 d), as a bar and a legend with the numbers; *By source* — the same per source (a word of two sources counts in both) with the reading position and its pack date; *How well you remember* — per card kind over the last 30 study days: recall tests = reviews with `elapsed_days ≥ 1` of a card that was not new (same-day repeats and first reviews test nothing), remembered = raw 1–3, aim = mean desired retention (§6.2, current traits) of the tested cards weighted by their tests, "too few to compare" under 20 tests; *Last 30 days* — ratings (undone ones left out, repeats counted) and new words per day as bars (empty days are just empty: no streaks); *Problem words* with why and the choices; *Pack suggestions to answer*. Every number has a sentence saying what it is; charts are SVG (aria-hidden) with the same numbers as text. One query per list (5,000 words / 100,000 reviews: ~230 ms in a debug build). |
| 10 | Settings | `#/settings` | New/day, block size, voice (US/UK, R-42), auto-play word on reveal, speed 1×/0.75×, sound on/off (per device), theme (dark default / light / system, R-71), legend (auto/always/off), audio status (pending/failed, *Retry failed*), **Export for Claude** (R-57), **Export everything** (R-86), sign out. M9: the Export section shows the feedback in a text field with *Copy* (to paste into the Claude project on the phone) and links that download both files. |

### 7.2 Rating bar (R-60–R-62)

Five buttons in one row, left → right **1 Instantly · 2 Knew it · 3 Shaky ┆ 4 Vague · 5 No idea**; number + label always (never a bare digit); green → teal → amber → orange → red ramp with distinct lightness; 1–3 filled, 4–5 outlined; extra 12 px gap with a "┆ pass | fail" divider; 250 ms input guard after reveal; toast "Rated 2 · Knew it — Undo" (4 s) in the rating's colour; legend always visible during the first 3 completed blocks, then on `?`/long-press; slip guard (a 1 within 600 ms of reveal on a card last rated 4–5 → toast stays 8 s and pulses). Labels in English (decision 8). `aria-label="Rate 2, Knew it"`.

### 7.3 Keyboard (R-70)

Space = reveal · 1–5 = rate (disabled while a text input has focus) · Enter = check (P/S input) / accept suggested grade / next · R = replay word · S = replay sentence · M = toggle More · H = hint (P) · U = undo · E = word detail · Esc = end block → summary · ? = shortcut overlay. Space never rates.

### 7.4 Audio in the UI (R-40–R-42)

▶ word and ▶ sentence controls in fixed positions; word auto-plays once on reveal (Understand) if sound is on; nothing plays on Produce/Spelling question sides except the Spelling prompt; next two cards' clips preloaded; speed via `playbackRate` with `preservesPitch`. If a clip is not ready (`ready: false`) or fails to load → **browser `speechSynthesis`** fallback, triggered only by a tap (R-41), English voice preferring `en-US` (or `en-GB` when the UK voice is selected).

### 7.5 Visual and accessibility

Dark theme default with the existing palette (fixing the two low-contrast styles noted in the research), light theme via setting/`prefers-color-scheme` (R-71); sizes in `rem`, sentence ≥ 19 px / line-height 1.5; no flip animation, ≤ 150 ms fades, none with `prefers-reduced-motion`; `lang="en"`/`lang="cs"` on text; `aria-live` region for results; target highlight = bold + underline (not colour alone); emoji icons shown on the answer side; Lucide icon names are stored but not rendered in phase 1 (no icon set shipped). Screen Wake Lock during a block where available.

---

## 8. HTTP API

All JSON. Session-authenticated unless noted. State-changing methods (POST/PUT/PATCH/DELETE) require the `Origin` header to equal `public_origin` (§9.3). Errors: `{ "error": { "code": "…", "message": "…" } }`. Under load any route may answer `503` with `Retry-After` (`overloaded`: no request slot; `busy`: another import is running) or `408 timeout` (a request body that did not arrive in time); clients retry after the stated seconds.

| Method & path | Purpose | Module |
|---|---|---|
| `GET /healthz` | Liveness (no auth; served outside the request slots and without touching the database, §9.4) | http |
| `GET /api/auth/setup` / `POST /api/auth/setup` `{code, password}` | Setup state / first password → Set-Cookie (public) | auth |
| `POST /api/auth/login` `{password}` | Login → Set-Cookie (throttled; `429 locked` + `Retry-After`) | auth |
| `POST /api/auth/logout` | Delete session | auth |
| `POST /api/auth/logout-all` | "Log out everywhere": delete every session, this one included → cookie removed | auth |
| `POST /api/auth/password` `{current_password, new_password}` | Change password → **new** Set-Cookie; every other session ends. Wrong current password: `401 invalid_password`; the fifth in one session: `401 session_ended` + cookie removed | auth |
| `GET /api/auth/connector-token` / `POST /api/auth/connector-token` `{current_password}` | Token exists? / new token (same password errors as above) | auth |
| `GET /api/auth/me` | 200 / 401 | auth |
| `GET /api/today?scope=` | Today view; besides due counts, forecast, `new_words` and the active block (with its `kind`): `new_waiting` (unseen words in scope), `new_left_today` (0 while paused), `new_paused_reason` (`null`\|`"backlog"`), `extra_available` (cards an extra block would serve now, ≤ block size; 0 while any card is due in the scope), `due_tomorrow` (= `forecast[0].due`), `new_typed`, `typed_waiting`, `typed_today`, `next_block {due, new}` (what a new regular block would hold now; `due_now`, `typed_today` and `next_block` come from the block builder's own rules, §6.6) | stats |
| `POST /api/sessions` `{scope_source_id?, kind: "start"\|"continue"\|"extra"\|"more_new", add_new_anyway?, count?}` | Build a block → `{session, resumed}`; `session: null` = nothing to practise. `extra`: §6.9; `more_new`: §6.6, `count` 1–50 (only with `more_new`, else `422 invalid`). The session carries `kind` (`regular`\|`extra`\|`more_new`) | session |
| `GET /api/sessions/current` | Active block or 204 | session |
| `POST /api/sessions/{id}/reviews` `{client_review_id, item_id, raw_grade, suggested_grade?, typed_answer?, answer_match?, hint_used, duration_ms, client_ts}` | Save one rating (idempotent by `client_review_id`) → updated queue tail, leech flag, unlocked cards. Typed cards (Produce) must send `answer_match`; `exact`/`none` need `typed_answer` (non-blank, ≤ 300 characters, no control characters), `dont_know`/`skipped` take none; Understand ratings take no answer details (`422 invalid`) | session → scheduler |
| `POST /api/sessions/{id}/undo` | Undo last rating (`409 card_changed` when its card was reset since, §6.7 step 9) | session → scheduler |
| `POST /api/sessions/{id}/finish` | Summary | session |
| `GET /api/words?q=&sort=&source=&min_u=&max_u=&status=&flags=` | Browse/search. `sort` ∈ `alpha` (default), `usefulness`, `added`, `due`, `status`, `source` (default with `source`); `status` ∈ `new`, `learning` (failed last or stability < 7 d), `familiar` (< 21 d), `known`, `leech`; `flags` comma list of `conflicts`, `suspended`, `low_priority`. Rows carry `status`, `next_due`, `included`, `suspended`, `created_at`. One query per module, joined in memory | catalog + stats |
| `GET /api/words/{id}` | Word detail incl. sentences (`edited`), conflicts (with the value `now`), `items` (lists with keys, `mine`), `overridden`, `effective_goal`, `status`, cards (with `recall`, `desired_retention`), history (newest 500) | stats (catalog + scheduler) |
| `PATCH /api/words/{id}` `{field: value, …}` | Edit fields with the pack's rules (marks overrides; `409 headword_taken` naming what collides: another word's headword, or a pack's spelling of another word with that word as shown) → detail + `undo` | workflows |
| `DELETE /api/words/{id}` | Delete word, its cards and history (confirm in UI; its ids are never reused) | workflows |
| `POST /api/words/{id}/actions` `{action: suspend\|unsuspend\|include\|exclude\|reset_card\|leech_keep\|leech_recognise\|leech_usefulness\|leech_suspend, kind?, usefulness?}` | Word actions → detail + `undo` (`reset_card` asks first and has none; the `leech_*` choices acknowledge the flag, §6.5; `kind` only with `reset_card`, `usefulness` only with `leech_usefulness`) | workflows |
| `POST /api/words/{id}/items` `{kind: czech\|accepted\|confusable\|form, value, common?}` / `DELETE /api/words/{id}/items` `{kind, key}` | Add (owner `accepted` → `owner_accepted`; a removed item is revived; one already listed, same key: `409 item_exists`; a listed form with the other `common` flag gets the new flag) / remove a list item named in the body, never in the path (a key may be `.` or `..`, which URL normalisation removes; kept as removed; the last Czech item stays, `422 last_czech`) → detail + `undo` | workflows (catalog) |
| `DELETE /api/words/{id}/spellings` `{key}` | Forget a spelling a pack used for the word (detail `pack_spellings`, never the current headword): a later pack with it adds a new word → detail + `undo` | workflows (catalog) |
| `POST /api/words/{id}/undo` `{token}` | Undo the owner's latest change of the word: `token` is the `undo` of its answer (`null` when nothing changed). Restores the exact state before it — the catalog's rows of the word (override rows, `edited`/`hidden` flags, removed items, suggestions, pack spellings) and its cards (a card the change unlocked is deleted again) — once, within 10 minutes, and only while the word is exactly as that change left it; else `409 undo_unavailable` (another change since, also from another tab or by an import or a rating; already undone; too late; word deleted), or `409 headword_taken` when another word has since taken a spelling it would get back → detail | workflows (catalog, scheduler, `undo`) |
| `POST /api/conflicts/{id}` `{use: "incoming"\|"keep"}` | Resolve a pack conflict: *Use new* stores the pack value and drops the override; *Keep* dismisses (`409 conflict_closed` when already answered) → detail + `undo` | workflows |
| `PATCH /api/sentences/{id}` `{en, cs}` / `POST /api/sentences/{id}/hidden` `{hidden}` | Edit (re-validated with the `[[…]]` rule; `en_key` keeps the pack's key so re-imports recognise it; `409 duplicate_sentence`) / hide a sentence (R-25) → detail + `undo` (of the sentence's word); audio synced (clips are named by text) | workflows (+audio) |
| `GET /api/sources` / `PATCH /api/sources/{id}` | Sources with counts and reading position; edit title etc. | catalog |
| `POST /api/import/preview` `{text}` / `POST /api/import/commit` `{text}` / `GET /api/import/recent` | Paste import (R-11). Report per pack: `status` ∈ `ready`, `imported`, `already_imported`, `rejected`, `unreadable`, `failed`; `summary.updated_sentences` and per word `merge.sentences_updated`; `reading_position: {value, updated, reason}` | importer |
| `GET /api/progress` | Progress view (R-72, §7.1 screen 9); Today also carries `problem_words` | stats |
| `GET /api/settings` / `PATCH /api/settings` | Settings | settings |
| `GET /api/export/feedback` | R-57 feedback JSON (§8.1): `Content-Disposition: attachment; filename="vocab-feedback-<day>.json"`, `Cache-Control: no-store` | exports |
| `GET /api/export/full` | R-86 full JSON (§8.1), spooled then streamed with `Content-Length`, same headers (`vocab-export-<day>.json`); one at a time (`503 busy` + `Retry-After`); `507 no_space` when the spool would leave less than 256 MiB free; broken off after 30 s without progress | exports |
| `GET /api/audio/status` / `POST /api/audio/retry` | Audio state for Settings (connected, voice, ready/pending/failed/approximate counts, `storage` ∈ `ok`, `cache_full`, `disk_low`, cache bytes and limit) / failed clips queued again | audio |
| `GET /api/audio/words/{id}` | The clips of a word and its visible sentences: `{url, ready, other_voice, approximate}` each | audio |
| `GET /api/audio/clips/{hash}.mp3` | Clip (immutable, `Cache-Control: private, max-age=31536000, immutable`, byte ranges) | audio |
| `POST /api/connector/v1/packs` `{pack, dry_run?}` | **Import token only** | importer |
| `GET /api/connector/v1/sources` | **Import token only**: id, type, title, reading position, word count | catalog |
| `GET /`, `GET /assets/{build}/…` | SPA shell and assets | web_assets |

Practice item payload (everything needed to render offline later): `{item_id, card: {id, kind, format, is_new, is_repeat, leech_flagged}, word: {headword, pronunciation, grammar, czech[], meaning, forms[], note, hint, icon, spelling_note, confusables[], tag}, sentence: {id, en: {pre,target,post}, cs: {pre,target,post}, caption}, other_sentences: [...], accept: [...], audio: {word: {url, ready}, sentence: {url, ready}}}`.

### 8.1 Export formats (M9; R-57, R-86)

Both are GET downloads behind the session like every `/api/*` route: a cross-site page cannot get them (the cookie is `SameSite=Strict`, fetch metadata `cross-site`/`same-site` → `403`, no CORS), and they are never cached (`no-store`). Neither contains anything of the account (password hash, sessions, connector token), the audio cache or open practice blocks.

**Feedback** (`vocab-feedback`, version 1, D-12) — vocabulary only, no ids, no dates of reviews, no settings. Members: `format`, `format_version`, `generated_on` (study day), `instructions_for_claude` (what Claude should do with it, so it works before the project instructions mention it), and lists, each entry naming the word and its sources' slugs:
`needs_hint` (problem words without a hint: `word`, `sources`, `czech`, `confusables`), `hard_words` (problem words that have one: plus `hint`), `hidden_sentences` and `edited_sentences` (`en`, `cs` marked with `[[…]]`; a hidden sentence is reported as hidden only), `owner_accepted` (`answers`), `overrides` (`field`, the owner's `value`), `open_conflicts` (`field`, `current`, `incoming`). Built in memory (bounded by the catalog caps).

Sample (synthetic):

```json
{
  "format": "vocab-feedback", "format_version": 1, "generated_on": "2026-11-20",
  "instructions_for_claude": "This is feedback from my vocabulary training app …",
  "needs_hint": [{ "word": "brontle", "sources": ["test-novel"], "czech": ["bručet si"] }],
  "hard_words": [{ "word": "glimmerous", "sources": ["test-novel"], "czech": ["třpytivý"], "hint": "Think of a candle in a draught." }],
  "hidden_sentences": [{ "word": "quabble", "sources": ["test-novel"], "en": "They would [[quabble]] over crumbs.", "cs": "[[Hádali se]] o drobky." }],
  "edited_sentences": [],
  "owner_accepted": [{ "word": "brontle", "sources": ["test-novel"], "answers": ["brontled"] }],
  "overrides": [{ "word": "snorkish", "sources": ["test-novel"], "field": "usefulness", "value": 3 }],
  "open_conflicts": []
}
```

**Full export** (`vocab-full-export`, version 1, D-16 as built) — everything needed to restore or move the data: a header `format`, `format_version`, `exported_at` (RFC 3339), `study_day`, `app_version`, `schema_version` (`PRAGMA user_version` the tables follow, §4), `settings` (the effective preferences, as `GET /api/settings`), then one member per table, each `{"columns": [names], "rows": [[values], …]}` (one row per line, values in column order; integers, reals, text, null as stored): `sources`, `words`, `word_sources`, `word_items`, `word_forms`, `sentences`, `word_overrides`, `field_conflicts`, `word_pack_spellings` (catalog, every column), `pack_imports` (without `report_json`), `cards`, `reviews` (`review_log` without the undo snapshot `before_json`; undone ratings kept, marked) and `fsrs_params`. To restore into a fresh database of the same schema version, insert the rows into the tables of the same names (`words` → `word`, …) with foreign keys off, `before_json`/`report_json` = `'{}'`; the round trip is tested (`tests/exports.rs`). Written row by row from **a read-only connection of its own** (one consistent WAL snapshot; the shared connection stays free) into an **anonymous temporary file in the data directory** (unlinked at once: nothing is left behind, not even after a crash; the read-only root filesystem is never written), so the snapshot is given back as soon as the export is written (5,000 words / 100,000 reviews ≈ 17.5 MB in well under a second) and never for as long as a slow client takes: WAL checkpoints go on during the download. The spool may not leave less than 256 MiB free on that filesystem (the database needs the room more): otherwise `507 no_space` before anything is sent. The file is then streamed with a `Content-Length` through a bounded body channel (64 KiB chunks, 4 queued), so memory stays flat whatever the size. One export at a time (`503 busy`); a download that takes no chunk for **30 s** (`export_idle_ms`, a stalled client) is broken off and gives the turn back at once, while a download that keeps moving is never cut off, however slow. A failure breaks the download (short of `Content-Length`, no closing `}`: never a complete-looking file). Trade-off: up to the export's size in temporary disk space for the length of a download, in exchange for a snapshot held only for the time it takes to write it.

---

## 9. Authentication and request protection (R-82, R-83)

### 9.1 Password and session

- No secrets in the environment (M5.1). The password is an argon2id PHC string (m = 19 MiB, t = 2, p = 1, OWASP minimum) in the one-row table `auth_owner` (migration 0004); NULL = **setup state**.
- Setup state: every start makes a one-time **setup code** (10 Crockford-base32 characters, shown `XXXX-XXXX-XX`), keeps only its SHA-256 in memory and logs it at WARN (`Setup required: open the app and enter setup code …`) at the start and again every hour while the app waits to be set up (so it is still to be found after the log has rotated). Public `GET /api/auth/setup` → `{required}`; `POST /api/auth/setup {code, password}` (≥ 12 characters) stores the hash (conditional `UPDATE … WHERE password_phc IS NULL`, so of two concurrent setups exactly one wins), ends all sessions and logs the owner in. Wrong codes are **not** part of any lockout (whoever can reach the app during setup must not be able to keep the owner from setting it up): the code has 50 random bits, checks run one at a time, and each wrong code holds that single check slot for a fixed 250 ms, so at most four codes a second can be tried against 2^50 (§9.4). Once a password exists, setup answers `409` for good; login answers `409 setup_required` while there is none.
- `POST /api/auth/password {current_password, new_password}` (session): checks the current password (counted per session, §9.4), stores the new hash, deletes **every** session row and gives the device that made the change a new token (`Set-Cookie`), so a stolen copy of its old cookie dies with the others.
- `POST /api/auth/logout-all` (session, Origin-protected) ends every session ("Log out everywhere" in Settings); `vocab-server logout-all` does the same with the stack stopped (same lock rule as `reset-password`).
- Recovery: `vocab-server reset-password` with the stack stopped clears the password, every session, the connector token and the stored login-throttle state; the next start is in setup state. `serve` holds an exclusive `flock` on `vocabulary.lock` in the data dir for its lifetime; `reset-password`, `logout-all` and `check` take it without waiting and refuse ("stop the stack first") while it is held, and refuse when there is no database (only `serve` creates one). The process umask is 077: everything the app creates is owner-only; existing directories are not changed.
- Login success → 32 random bytes (`getrandom`) → cookie `__Host-vocab_session=<hex>; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=15552000` (180 days, sliding: renewed when older than 7 days; and never longer than **365 days from its creation**, however often it is renewed — the renewed cookie's `Max-Age` is cut accordingly). DB stores only `sha256(token)` plus the fingerprint (`sha256` of the PHC string) of the password at login, so a new password invalidates the sessions of the old one.
- Hashing and verification run on `spawn_blocking`; they, the setup-code check and every change of the stored credentials run behind one semaphore of 1.

### 9.2 Connector token

- Created in the app: Settings → *Create token* (`POST /api/auth/connector-token {current_password}`, session + CSRF guard; the current password is asked again, counted per session (§9.4), so a stolen cookie alone cannot mint a token) makes 32 random bytes (64 hex), shows them once and replaces any old token; `auth_owner` keeps only the SHA-256 (`GET` tells whether one exists and when it was made). No token → the connector routes answer `401`. Sent as `Authorization: Bearer …`, compared in constant time (`subtle`) on SHA-256 digests.
- Accepted **only** by `/api/connector/v1/*`; session cookies are not accepted there and the token is not accepted anywhere else. The connector therefore cannot read progress, edit or delete (R-12).

### 9.3 CSRF

- `SameSite=Strict` cookie (no cross-site cookie at all) **plus** an `Origin` check on every state-changing request: `Origin` must equal `public_origin` (`https://vocab.example.home`). This matters because other `*.example.home` services are *same-site*, so SameSite alone would not stop a request from a compromised sibling subdomain. JSON bodies with `Content-Type: application/json` are required (blocks HTML-form posts). Connector endpoints use the bearer token and no cookies, so they are exempt.
- Fetch metadata: every `/api/*` request (GET included) whose `Sec-Fetch-Site` header is present and says `cross-site` or `same-site` is refused with `403 forbidden_origin` before anything else is looked at. An absent header is allowed (the connector and other non-browser clients, old browsers); `same-origin` and `none` pass.

### 9.4 Rate limiting and limits

Three kinds of secret checks are kept apart on purpose, so that nobody without a session can lock the owner out of what he is logged in to:

- **Login:** global throttle (single user, so not per IP — X-Forwarded-For could be spoofed by any container on `proxy-net`). Two rules: after 5 failed logins in 15 min, further attempts are refused for 15 min, doubling up to 24 h (the doubling starts over after 24 h without a failed or refused login); and more than 20 failed logins within 24 h lock logins for 24 h whatever their spacing. A successful login resets both. The state (times of recent failures, lock end, lock level, latest failed or refused attempt) is stored in `auth_throttle`, so a restart neither lifts a lock nor hands out fresh guesses nor restarts the decay (a refused attempt is stored at most once a minute); `reset-password` clears it. Clock errors cannot lock the owner out for long: on load and on every check a lock end later than now + 24 h is cut to now + 24 h, and failures dated in the future are dropped. Existing sessions keep working during a lockout.
- **Password typed inside a session** (change password, create connector token): counted per session (`auth_session.password_failures`); five wrong ones end that session (`401 session_ended`, cookie removed), a right one resets the count. These checks never count towards the login lock and are never blocked by it.
- **Setup code:** no lockout at all; serialised, 250 ms per wrong code (§9.1).
- Every failed login, every start of a lock, every wrong password inside a session and every wrong setup code is logged at WARN (never what was typed).
- **Accepted residual risk:** a hostile device on the LAN can still keep *new* logins locked (5 wrong passwords every lock period). It cannot affect existing sessions (180 days, sliding), nor anything a logged-in owner does. The way out, if ever needed, is `reset-password` on the server.
- **Connector:** 30 requests / 10 min per token; **paste import:** 30 / 10 min, and one import (preview or commit) at a time, its body included: a second one waits up to 2 s for its turn, then `503 busy` + `Retry-After: 5`.
- Body limits (over the limit → `413`): 64 KiB default; `/api/import/*` 2.5 MiB on the transport (a 2 MiB paste plus the escaping inside a JSON string), the decoded paste itself ≤ 2 MiB with the structure limits of §5.2 (else `422 invalid_paste`); 1 MiB connector; 2 KiB login. Hard caps on totals: 20 000 words, 200 000 sentences (§5.1).
- **Request slots (nobody without a session can stop the app):** 64 concurrent requests, 8 of them reserved for requests that carry the cookie of a session this process already knows to be live (checked in memory, without the database). A request waits at most 2 s for a slot, then `503 overloaded` + `Retry-After: 5`. The whole request, body read included, is under the request timeout (30 s; import commit work 120 s, its body 30 s → `408`); the body of a request to a public route (login, setup; ≤ 2 KiB) must arrive within 5 s (`408`); every other route refuses a request without a session before reading its body. `/healthz` is served outside the slot pool and never touches the database (whose single connection a long import may hold), so the container health check answers under any load and during any import.
- **Connections:** 256 open connections in all, at most 128 per peer address; a connection over the per-peer cap is answered `503` and closed at once. Behind the reverse proxy every browser shares the proxy's address, which is why the per-peer cap is as generous as half the pool: it does not tell users apart, it only keeps one address (a device that reaches the container directly) from holding every connection, and leaves room for the health check, which comes from loopback. A request head must be complete within 10 s of its first byte (a new connection must send its first head within 10 s); a connection that has answered a request may idle up to 120 s before the next one (`keep_alive_idle_secs`: longer than the ~90 s a reverse proxy keeps idle upstream connections, so the proxy never reuses a connection the app is closing → no sporadic 502). Idle connections of one address stay bounded by the per-peer cap, those of all addresses by the connection cap.
- **Logging:** request lines hold method, route template, status and duration only. `/healthz` is logged at debug. At most 20 request lines a second are written; what is over is counted and reported in one line (`suppressed N request log lines`) when the next second starts, so a flood of requests cannot fill the disk through the log.
- Security headers on every response: `Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; media-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'; require-trusted-types-for 'script'` (the page builds its DOM from nodes and text only; with no Trusted Types policy defined, every string-to-code sink is simply forbidden where the browser supports the directive), `X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin` (not `no-referrer`: Firefox then sends `Origin: null` on same-origin POSTs), `Cross-Origin-Opener-Policy: same-origin`, `Cross-Origin-Resource-Policy: same-origin`, `Permissions-Policy: camera=(), microphone=(), geolocation=()`, `Strict-Transport-Security: max-age=31536000` (this host only).

---

## 10. TTS (R-40, R-41, R-42)

### 10.1 Candidate models

| Model | Quality (English) | Licence | CPU performance | Footprint | Hardening fit | Verdict |
|---|---|---|---|---|---|---|
| **Kokoro-82M v1.0** (hexgrad) via sherpa-onnx | Best of the small open models; natural prosody; 20 US + 8 UK voices incl. `af_heart`, `bf_emma` | Weights + voices **Apache-2.0**; front-end uses espeak-ng data (**GPL-3.0**) for out-of-lexicon words | Faster than real time on x86 with 2 threads (Raspberry Pi 4 measures RTF ≈ 2.8–3.2 at 4 threads per sherpa-onnx docs; a NAS-class Intel core is several times faster — measured in the M6 spike) | `kokoro-multi-lang-v1_0` 349 MB archive (model.onnx 310 MB fp32); ~0.8–1.2 GB RAM | Static C++/onnxruntime libs, no Python, runs on `distroless/cc`, no network | **Chosen** |
| Piper (VITS) `en_US-lessac-*` via sherpa-onnx | Clear but noticeably more synthetic | Engine GPL-3.0 (piper1-gpl); voice licences vary per dataset (check each MODEL_CARD) | Very fast (RTF ≪ 0.1) | 20–115 MB | Same runtime | Fallback if Kokoro is too slow |
| Kitten TTS v0.8 (nano/mini) via sherpa-onnx | Below Kokoro; "developer preview" | Apache-2.0 | Very fast | 26–156 MB | Same runtime | Not chosen (quality/maturity) |
| Supertonic (Supertone) | Good, very fast | Code MIT, **weights OpenRAIL-M** (use restrictions → extra licence review) | Very fast | 84–128 MB | Same runtime | Not chosen (licence) |
| XTTS-v2 / F5-TTS | High, voice cloning | Coqui Public Model Licence / CC-BY-NC (non-commercial) | Needs a GPU for reasonable speed | GBs + PyTorch | Heavy Python runtime | Excluded |
| Chatterbox (Resemble AI) | High | MIT | 0.5B params; slow on CPU | GBs + PyTorch | Heavy | Excluded |

**Choice: Kokoro-82M v1.0**, file `kokoro-multi-lang-v1_0.tar.bz2` from the sherpa-onnx `tts-models` release (uploaded 2026-10-26, sha256 `60703707bc05ccd48d60e1eb723f38b07d6e474795a0b6a588d7220400e0f7a4`), run with **sherpa-onnx 2.0.1** (released 2026-10-26; the model asset and the runtime are from the same release cycle, avoiding format drift). Voices: `af_heart` (US, default, decision 7) and `bf_emma` (UK, R-42). sherpa-onnx also runs Piper/Kitten/Supertonic, so a model swap is a config + image change only.

**Fallback plan inside M6:** if the spike shows the fp32 model is too slow (> 2 s for a 20-word sentence with 2 threads) use `kokoro-int8-multi-lang-v1_0` (132 MB, same release, sha256 `72c347230107ef8a31098ec4d468a605244beef5f1ed00db97efa13518800653`); if sherpa-onnx 2.0.1 has a blocking problem, use 1.13.8 (2026-09-11) with `kokoro-en-v0_19` (2025-08-10, sha256 `912804855a04745fa77a30be545b3f9a5d15c4d66db00b88cbcd4921df605ac7`; 11 voices incl. US and UK).

**Licence note (no action needed, FYI for the owner):** the `vocab-tts` image links sherpa-onnx's espeak-ng-based front-end (GPL-3.0) and LAME (LGPL-3.0). The repository source stays MIT; the **TTS image as distributed is a GPL-3.0 combined work**. That is compatible with personal use and with a public GHCR image as long as the corresponding sources are available (all are public); the image ships `/licenses/` with the licence texts and source URLs. The app image contains no GPL code.

### 10.2 Container and API

*As built in M6 (v0.2.0); measurements and the front-end in [`tts-spike.md`](tts-spike.md).*

- `vocab-tts` binary (Rust): runs Kokoro-82M through sherpa-onnx 2.0.1 (`voices.bin`, `tokens.txt`; provider `cpu`). Since v0.2.1 (owner's decision, little RAM on the server): the int8 model (`model.int8.onnx`), one thread (`VOCAB_TTS_THREADS`, default 1), in an engine process started on demand and ended after `VOCAB_TTS_IDLE_SECONDS` (default 180) without work, so the idle container uses a few MiB (docs/design/tts-spike.md, "Small memory"). sherpa-onnx 2.x has no espeak-ng, so the service has its own grapheme-to-phoneme front-end: the US/UK misaki lexicons, CMUdict as the fallback, numbers, compounds, letter spelling (`X-Approximate` counts guessed, spelled and dropped words).
- `POST /v1/speak` `{ "text": "...", "voice": "af_heart", "speed": 1.0 }` → `200 audio/mpeg` + `X-Duration-Ms`, `X-Approximate`, `X-Frontend`, `X-Engine`. Text: invisible formatting characters are removed (the set the pack keys ignore; a soft hyphen joins its word) and every whitespace character (tab, CR, line breaks) becomes a space; then 400 `invalid_text` for empty, > 600 characters, control characters or `<`/`>` (no SSML); 422 `token_too_long` (a run of > 64 non-space characters), `number_too_long` (> 15 digits), `too_long_to_speak` (more than 50 s of audio at the requested speed: estimated from the phonemes before the engine starts, and checked while it speaks), `nothing_to_say`; 400 for an unknown voice or a speed outside 0.5–1.5. One synthesis at a time, four may wait; beyond that `503 busy` at once. `503 engine_unavailable` (with `Retry-After`) when the engine process cannot be started (model missing, killed while loading for lack of memory): nothing to do with the text. Request timeout 600 s (`503 timeout`; the longest text on a slow CPU limited to a fraction of a core); an engine process that gives no sign of life for 180 s (it reports after every chunk) is stopped. A request that is abandoned (timeout, client gone) is cancelled — skipped if it has not reached the engine, stopped at once (the engine process is killed) if it has. On `SIGTERM` (also during the start self-test) the engine process is killed, the request in progress is answered `503 shutting_down`, and the service exits within seconds. No chunk the engine sees exceeds 100 symbols (a pause counted as three; fewer for slower speech, so a chunk's audio and the engine's memory stay the same at every speed; inside Kokoro's 510). `GET /v1/voices` (with the front-end and engine versions), `GET /healthz`.
- **No credentials.** The service is only on the `vocab-internal` network (`internal: true`): no published port, no route to the internet or the LAN; its only peer is `vocab-server`. A shared token would have to live in the environment of both containers (against the rule of §12.1: no secrets in the environment) and would protect against nothing the network does not already exclude — anything that can reach the service can already read the app's environment. What it could do with access is bounded: speak ≤ 600 validated characters at a time, one at a time. The app accepts only an internal `VOCAB_TTS_URL` (a host name without dots, `localhost`, or a loopback/private/link-local IP; anything else stops the app at start), so a configuration mistake cannot send book text to the internet. No volumes; model files baked into the image (read-only rootfs).

### 10.3 Generation pipeline (`audio` module in vocab-server)

1. **What is spoken:** per word `speak ?? headword`; per sentence the English text with markers removed and a leading/trailing `…`/`...` trimmed; invisible formatting characters removed and whitespace collapsed (the same normalisation the service does). Czech is never spoken (Kokoro has no Czech voice; the app is EN→CS).
2. **Clip identity:** `hash = hex(sha256("v1|" + frontend + "|" + engine + "|" + voice + "|1.0|" + text))[0..32]` with the versions from `GET /v1/voices` (`engine` names the model, the runtime and the post-processing, e.g. `kokoro-int8-multi-lang-v1_0+sherpa-onnx-2.0.1+audio-2`). Identical text → one clip shared across words. Changing the text (owner edit, pack conflict applied) yields a new hash; the old clip becomes garbage. A new front-end or engine version makes every clip again; until a text's new clip is ready the old one is played, and it is deleted as soon as its replacement is ready (one by one).
3. **Queue:** `audio::request` inserts `pending` rows (ignore if the hash exists). Triggers: import commit, word/sentence edits, voice change in settings (enqueues the whole set for the new voice in the background; until a clip exists the client falls back to the other voice's clip, then to browser speech).
4. **Worker:** one tokio task; wakes on a `Notify` (new work) or every 30 s; picks the next due `pending` clip (`kind = 'word'` first, then clips never tried, then by when they became due); calls the TTS service (timeout 630 s, longer than the service's own 600 s, so a slow synthesis ends with the service's `503 timeout` and its cancellation); writes `<cache dir>/audio/<h0h1>/<hash>.tmp`, `fsync`, renames to `.mp3`; marks `ready` with size and duration.
5. **Retries:** unreachable service (connection refused, `/v1/voices` failing) → the worker waits (30 s doubling to 10 min) and no clip loses an attempt. A service that is up but cannot work (`503 engine_unavailable`, `503 shutting_down`) → the worker waits 60 s and tries again, no attempt spent. Both are logged once when they begin and once when they end. With the service up: 5xx, a timeout, an answer that is not `audio/mpeg`/not an MP3 stream, or larger than 1 MiB → `attempts + 1`, `next_attempt_at` = now + 30 s, 2 min, 10 min, 1 h, 6 h, 24 h; after 6 attempts `failed` with the reason. 4xx → `failed` immediately with the error code; a clip longer than 60 s → `failed` ("clip longer than 60 s"). The service's `500 engine_crashed` (its engine process died on the text, e.g. killed for lack of memory) counts as an attempt **and a crash**; so does a connection that dies after the request was sent while the service is still there afterwards. A connection that dies with the service gone afterwards (a stop or redeploy that did not answer `shutting_down`) costs nothing: the worker waits as for an unreachable service. After 3 crashes the clip is `failed` ("crashed the audio service"). A clip that failed waits behind the other due clips, so one bad text never blocks the queue. Settings shows pending/failed counts and *Retry failed*.
6. **Post-processing (in vocab-tts):** join the chunks (where the text was cut between words without a pause, the engine's silence at the chunk ends is cut to a 40 ms word gap with 5 ms fades) → trim leading/trailing silence below −50 dBFS → EBU R128 integrated loudness (`ebur128`; for clips < 400 ms where integrated loudness is undefined, use maximum momentary loudness) → gain to **−16 LUFS** (clamped to ±20 dB) with a look-ahead limiter → true peak held at −3.0 dBTP before encoding (the limiter aims lower until it holds), so the MP3 stays under **−1.5 dBTP** (encoding raises the true peak by up to ~0.9 dB) → pad 60 ms silence before, 150 ms after (avoids clipped onsets on mobile browsers) → **MP3**, mono, 24 kHz, LAME VBR quality 5 (≈ 48–64 kbit/s; ~10 KB per word, ~30–40 KB per sentence). MP3 plays everywhere incl. iOS Safari; Opus is still only partially supported by desktop Safari.
7. **Reconciliation & GC:** at start, `ready` rows whose file is missing go back to `pending` (the cache volume is regenerable, R-85) and files without a row are deleted. Every sync (start, import, voice change, every 10 min) deletes clips not in the keep-set (current speak texts × both voices, plus older-version clips still standing in for one not ready yet).
8. **Serving:** `GET /api/audio/clips/<32-hex>.mp3` (session required), `Content-Type: audio/mpeg`, immutable caching, `ETag` = hash, range requests supported (iOS Safari requires them for `<audio>`). `GET /api/audio/words/{id}` tells the client which clips of a word are ready.
9. **Fallback (R-41):** `GET /api/audio/words/{id}` carries `{url, ready}` per clip; if `ready` is false or loading fails, the ▶ button uses `speechSynthesis` (tap-triggered only). Never two sounds at once: a clip stops before the browser speaks and speech stops before a clip plays.
10. **Storage:** at most 2 GiB of clips, and the disk keeps 256 MiB + one clip free (the database shares it, so SQLite never meets a full disk): beyond either the worker makes no new clips, logs one warning, and Settings says why (`storage`). Single clips are capped at 60 s / 1 MiB.

Expected load: a 15-word pack ≈ 15 word clips + ~60 sentence clips; migrating ~500 old words (~3 000 clips) is a background job of the order of an hour; practice works meanwhile with the fallback.

---

## 11. MCP connector for Claude Desktop (R-12)

- **Packaging:** a Claude Desktop extension (`.mcpb`, manifest_version 0.3), `server.type: "node"` — Claude Desktop runs it with its built-in Node.js, so nothing is installed on the Mac.
- **Code:** `connector/server/index.js`, one ES module, **no runtime dependencies** (Node built-ins: stdin line reader, `fetch`). A minimal MCP stdio server (newline-delimited JSON-RPC 2.0): `initialize` (supports protocol versions `2025-11-25`, `2025-06-18`, `2025-03-26`; answers with the client's version if supported, else the newest), `notifications/initialized`, `ping`, `tools/list`, `tools/call`; anything else → JSON-RPC error −32601. Why not the official SDK: it brings ~15 transitive packages (HTTP server, validation libraries) for features not used here; two tools over stdio are ~250 lines. A protocol replay test guards against drift.
- **Tools:**
  - `add_words` — input `{ "pack": <vocab-pack v1 object>, "dry_run"?: boolean }`; the `pack` property's schema is `docs/vocab-pack.schema.json`, copied into the bundle at pack time (single source). Description tells Claude to send one pack per call, to use the format from the project instructions, and, on errors, to fix and resend only the failed words in a new pack. Returns a text summary plus `structuredContent` with the import report (new/merged/unchanged words, added sentences, conflicts, errors with JSON pointers). Annotations: `destructiveHint: false`, `idempotentHint: true`, `openWorldHint: false`.
  - `list_sources` — no input; returns `[{id, type, title, reading_position, word_count}]` so Claude can reuse the exact `source.id` and knows the last reading position. `readOnlyHint: true`.
- **Configuration (manifest `user_config`):** `api_url` (string, default `https://vocab.example.home`) and `import_token` (string, **`sensitive: true`**, required). Claude Desktop stores sensitive values in its secure store (macOS Keychain) and passes them as environment variables (`VOCAB_API_URL`, `VOCAB_IMPORT_TOKEN`) to the process; the token is never written into `claude_desktop_config.json` or the repo.
- **Install:** `cd connector && npm ci && npm run pack` → `vocab-connector-X.Y.Z.mcpb` → open it with Claude Desktop (Settings → Extensions → Install) → enter URL and token. Use it inside the "Learning english" project chat ("add these words to the training app").
- **What it cannot do:** delete, edit, or read learning progress — it has no code for other endpoints and the server accepts the token only on `/api/connector/v1/packs` and `/api/connector/v1/sources`.
- **Safety:** refuses non-HTTPS URLs except `http://localhost`/`127.0.0.1` (tests); `redirect: "error"` so the token is never forwarded to another host; 30 s timeout; response cap 1 MiB; logs (stderr) contain only pack_id and counts.
- **Network:** the Mac must reach `vocab.example.home` via the LAN or the VPN tunnel (same DNS name as the browser).

---

## 12. Deployment (R-80, R-81, R-85)

### 12.1 Storage: ZFS dataset, UID/GID

- Container UID/GID: the images default to **65532:65532** (distroless `nonroot`); the deployment runs them as the owner's TrueNAS user **3101:3101** (`user:` in Compose).
- Owner, once (TrueNAS UI): Datasets → `tank/services` → Add dataset `vocabulary`, preset *Generic*; Permissions: owner 3101:3101, `0700`. The dataset root is the only mount (`/data`); the app creates missing directories itself (data dir, cache dir `/data/cache`, mode 0700) and stops with `storage is not usable` if it cannot create or write them. `VOCAB_DATA_DIR` (default `/data`) and `VOCAB_CACHE_DIR` (default `<data dir>/cache`) can override the paths.

- Backup: the existing recursive daily replication `tank/services` → `backup-tank/services` picks up the new child dataset automatically; check after the first run that `backup-tank/services/vocabulary` exists. ZFS snapshots are atomic across `vocabulary.sqlite`, `-wal` and `-shm`, and WAL mode is crash-safe, so every snapshot is a consistent DB. The app has no backup logic (R-85).

Configuration is environment only (no config file): `VOCAB_PUBLIC_ORIGIN` (required, e.g. `https://vocab.example.home`), `VOCAB_TIMEZONE` (default `Europe/Prague`), `VOCAB_DAY_ROLLOVER_HOUR` (default 4), `VOCAB_LOG_LEVEL` (default `info`); unknown `VOCAB_*` variables or invalid values stop the app with a message naming the variable.

There is no `.env`: the password is set on first start with the logged setup code, the connector token in Settings (§9.1, §9.2). The TTS service (M6) has no credentials at all: it is reachable only on the internal network (§10.2, §13).

### 12.2 Compose draft (`home/docker/vocabulary/docker-compose.yml` — to be added to the home repo by the owner/guarantor, not by this design)

```yaml
name: vocabulary

services:
  vocabulary:
    container_name: vocabulary
    image: ghcr.io/kadledav/vocabulary-training:0.1.0@sha256:<digest-from-CI>
    restart: unless-stopped
    user: "3101:3101"
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    tmpfs:
      - /tmp:size=16m,mode=1777,noexec,nosuid,nodev
    environment:
      - VOCAB_PUBLIC_ORIGIN=https://vocab.example.home
    volumes:
      - /mnt/tank/services/vocabulary:/data
    mem_limit: 256m
    memswap_limit: 256m
    cpus: 1.0
    pids_limit: 128
    healthcheck:
      test: ["CMD", "/vocab-server", "healthcheck"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 20s
    logging:
      driver: json-file
      options: { max-size: "10m", max-file: "3" }
    networks:
      - proxy-net
      - vocab-internal
    labels:
      - "traefik.enable=true"
      # The container is on two networks: tell Traefik which one to use.
      - "traefik.docker.network=proxy-net"
      - "traefik.http.services.vocabulary-svc.loadbalancer.server.port=8080"
      # 'home' entrypoint ONLY (LAN + VPN). Never add a 'websecure' router:
      # a router without an explicit entrypoints label would attach to ALL entrypoints.
      - "traefik.http.routers.vocabulary-home.rule=Host(`vocab.example.home`)"
      - "traefik.http.routers.vocabulary-home.entrypoints=home"
      - "traefik.http.routers.vocabulary-home.service=vocabulary-svc"
      - "traefik.http.routers.vocabulary-home.tls=true"

  vocabulary-tts:
    container_name: vocabulary-tts
    image: ghcr.io/kadledav/vocabulary-training-tts:0.1.0@sha256:<digest-from-CI>
    restart: unless-stopped
    user: "3101:3101"
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    tmpfs:
      - /tmp:size=16m,mode=1777,noexec,nosuid,nodev
    environment:
      - VOCAB_TTS_TOKEN=${VOCAB_TTS_TOKEN}
      - TTS_THREADS=2
    mem_limit: 1536m
    memswap_limit: 1536m
    cpus: 2.0
    pids_limit: 64
    healthcheck:
      test: ["CMD", "/vocab-tts", "healthcheck"]
      interval: 60s
      timeout: 5s
      retries: 3
      start_period: 60s
    logging:
      driver: json-file
      options: { max-size: "10m", max-file: "3" }
    networks:
      - vocab-internal          # internal only: no route to LAN or internet
    labels:
      - "traefik.enable=false"

networks:
  proxy-net:
    external: true
  vocab-internal:
    name: vocabulary-internal
    internal: true
```

Notes:
- No `ports:`, no Docker socket, no host networking, no devices (the iGPU is not used: Kokoro on CPU is fast enough and `/dev/dri` would widen the attack surface).
- **DNS (owner action):** `vocab.example.home` must resolve to the TrueNAS LAN IP on the LAN/VPN resolver (local DNS host override, like the other `home` services). If the public wildcard DNS also resolves it, a request from the internet lands on router → `websecure` (:PUBLIC_PORT), where no router exists for this host → 404. R-81 holds by construction; an e2e check in the stack README verifies `curl -k https://<public-ip>:PUBLIC_PORT -H 'Host: vocab.example.home'` returns 404.
- **No mesh VPN** in phase 1 (R-81, decision 2): the mesh-VPN ACL is not changed; Traefik :443 stays "nobody" on the mesh VPN.
- **Updates:** CI publishes images on a `vX.Y.Z` tag and prints both digests in the release notes; the owner updates `image:` (tag + digest) in the home repo and redeploys via Portainer. If a release contains a DB migration, the release notes say so and recommend `zfs snapshot tank/services/vocabulary@pre-vX.Y.Z` first.

### 12.3 Restore procedure (goes into `home/docker/vocabulary/README.md`; tested once before phase-1 acceptance, R-85)

1. Stop the stack (Portainer → Stop, or `docker compose down`).
2. Pick a snapshot: `zfs list -t snapshot -o name,creation -s creation tank/services/vocabulary` (local) or `backup-tank/services/vocabulary` (replica).
3. Move the current data aside: `mv /mnt/tank/services/vocabulary/data /mnt/tank/services/vocabulary/data.broken-$(date +%F)` and `mkdir` a new `data` (`chown 3101:3101`, `chmod 0700`).
4. Copy the **whole** `data/` directory from the snapshot — always the DB together with its `-wal`/`-shm` files, never the `.sqlite` alone:
   `cp -a /mnt/tank/services/vocabulary/.zfs/snapshot/<snap>/data/. /mnt/tank/services/vocabulary/data/`
   (from the replica: `/mnt/backup-tank/services/vocabulary/.zfs/snapshot/<snap>/data/.`).
   Alternative: `zfs rollback -r tank/services/vocabulary@<snap>` rolls back config and cache too and destroys newer snapshots — only if that is intended.
5. Start the stack. The app runs `PRAGMA quick_check` and migrations at start and refuses to start (log line `database check failed`) if the file is damaged.
6. Verify in the UI: word count on Progress, last review date in a word's history. Audio may regenerate (cache is reconciled automatically).

### 12.4 Image build and publish

- `docker/Dockerfile.server`: build stage `rust:1.99.0-slim-trixie@sha256:2752b332db73fdbb7dc576f06c82ed1f312005784ef913d7e04a28f5f55dc581` + `musl-tools` (exact Debian version pinned in the Dockerfile) + `rustup target add x86_64-unknown-linux-musl`; `cargo build --release --locked --target x86_64-unknown-linux-musl -p vocab-server`. Runtime stage `gcr.io/distroless/static-debian13:nonroot@sha256:25ac8f0c3b2334a9eb05452266bd4697864a8c8ef0ef71e5e6480d7f92868329`; copy `/vocab-server`; `USER 65532:65532`; `ENTRYPOINT ["/vocab-server"]`; `CMD ["serve"]`; OCI labels (source, MIT licence).
- `docker/Dockerfile.tts`: build stage same Rust image; `ADD --checksum=sha256:<pinned> https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/sherpa-onnx-v2.0.1-linux-x64-static-lib.tar.bz2 /archives/` and `SHERPA_ONNX_ARCHIVE_DIR=/archives` (the `sherpa-onnx-sys` build script otherwise downloads the archive without checksum verification); model stage `ADD --checksum=sha256:60703707… …/tts-models/kokoro-multi-lang-v1_0.tar.bz2`, unpack (`bzip2` pinned), keep only the needed files. Runtime `gcr.io/distroless/cc-debian13:nonroot@sha256:08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1`; `/vocab-tts`, `/model/…`, `/licenses/…`.
- Registry: `ghcr.io/kadledav/vocabulary-training` and `ghcr.io/kadledav/vocabulary-training-tts`, public (like the owner's other images). Tags `X.Y.Z` and `sha-<short>`; no `latest` is used for deployment. `docker/build-push-action` with `provenance: mode=max` and an SBOM builds the image **once** and pushes it as `sha-<short>`; `tools/release-promote.py` pulls that digest back, runs the smoke tests on it and only then adds the `X.Y.Z` tag to the same manifest (no rebuild).

---

## 13. Security

### 13.1 Threat model (short)

| Asset | Threat | Main controls |
|---|---|---|
| Book sentences (copyright, R-24) | Leak into the public repo, logs, CI artefacts | Stored only in `/data`; logs never contain bodies/sentences; fixtures synthetic + CI guards (§14.4); no telemetry; TTS container has no egress |
| Vocabulary and progress (personal) | Household devices/guests on the LAN, compromised IoT/TV, other containers on `proxy-net` reaching `vocabulary:8080` directly (bypassing Traefik) | Auth enforced **in the app**, not in Traefik; long random session tokens; global login throttling (not IP-based, X-Forwarded-For is spoofable from inside the Docker network); no public route |
| Import token | Theft from the Mac | Stored in Claude Desktop's secure store; scope = add words + list sources only; rotate by changing `.env` |
| Browser session | XSS via pack content (Claude output is untrusted input, R-84), CSRF from sibling `*.example.home` sites | DOM built with `textContent` only + CI grep gate; strict CSP; SameSite=Strict + Origin check + JSON-only bodies |
| Host / NAS | Container breakout, resource exhaustion | Distroless, non-root, read-only rootfs, `cap_drop: ALL`, `no-new-privileges`, no socket, limits (R-83) |
| Supply chain | Malicious/compromised new releases | 14-day age gate on every locked dependency (CI-enforced), exact pins, image digests, Action SHAs, checksum-verified downloads, few dependencies, `cargo-deny` advisories/licences |
| Availability | Huge pack, request floods | Body/size limits, per-endpoint rate limits, request timeouts, bounded concurrency |
| TTS service (no credentials) | Another container or device using it, a text that crashes it, a cache that fills the disk | Only on the `internal: true` network, app refuses a non-internal `VOCAB_TTS_URL`; text validated and bounded (words, numbers, phoneme symbols, chunks), abandoned work cancelled, bounded queue; crash quarantine after 3 dropped connections; cache cap and free-space reserve (§10.2, §10.3) |

### 13.2 R-83 / R-84 control mapping

| Requirement item | Control |
|---|---|
| R-83 minimal image, no shell, no package manager | App: `distroless/static-debian13:nonroot` + static binary. TTS: `distroless/cc-debian13:nonroot`. CI smoke test asserts `--entrypoint /bin/sh` fails. Healthchecks use the binaries themselves. |
| R-83 fixed non-root UID/GID | `USER 65532:65532` in both images; Compose runs them as `user: "3101:3101"`; dataset ownership per §12.1. |
| R-83 read-only rootfs; only data volume + small tmpfs writable | `read_only: true`; writable: `/data` (database + `/data/cache`), `/tmp` tmpfs 16 MB `noexec`; SQLite `temp_store=MEMORY`. No config mount. |
| R-83 cap_drop ALL, no-new-privileges, no socket, no host network, no published ports | Compose §12.2; app listens on 8080 (> 1024, no capability needed). |
| R-83 memory, CPU, PID limits | App 256 MB / 1 CPU / 128 PIDs; TTS 1.5 GB / 2 CPU / 64 PIDs; swap disabled via `memswap_limit`. |
| R-83 body size limits and rate limiting on login and import | §9.4. |
| R-83 base images pinned by digest | Dockerfiles use `image:tag@sha256:…`; Compose uses `tag@sha256:…`; Renovate `docker:pinDigests`; dep-age gate checks digest age. |
| R-83 TTS in separate container with same hardening and no outbound network; model baked in or mounted read-only | `vocabulary-tts` on `internal: true` network only; same flags; model baked into the read-only image; no volumes. No token between app and service (it would be a secret in two environments and add nothing the network does not give, §10.2); the app only talks to an internal `VOCAB_TTS_URL`. |
| R-84 schema validation | `vocab-pack::validate` (§5.2) + schema conformance tests. |
| R-84 size limits | Paste ≤ 2 MiB, pack ≤ 1 MiB, ≤ 100 words/pack, per-field maxima; control characters rejected. |
| R-84 everything rendered escaped | DOM via `h()`/`textContent` only; marked spans arrive pre-split (`pre/target/post`) so the client never parses or injects markup; CSP without `unsafe-inline`; CI forbids HTML-injection APIs in `web/`. |
| R-82 no secrets in the environment | Password hash and connector-token digest in the DB (`auth_owner`); setup code only in memory and one WARN log line; `Debug` of secrets prints `***`. |

---

## 14. Engineering

### 14.1 Repository layout

```
Cargo.toml / Cargo.lock / rust-toolchain.toml (channel = "1.99.0") / deny.toml / renovate.json
crates/
  vocab-pack/        src/{lib,paste,validate,marked,keys,hash,types}.rs   tests/
  vocab-server/      src/{main,config,clock,db,catalog,importer,scheduler,session,audio,auth,
                          settings,stats,exports,workflows,web_assets,http}/…   migrations/   tests/
  vocab-tts/         src/{main,engine,postprocess,http}.rs
web/                 index.html  css/  js/{main,api,dom,router,grading,audio,keys}.js
                     js/screens/*.js  js/practice/*.js  test/*.test.js  e2e/*.spec.js
                     package.json + package-lock.json (devDependencies only) tsconfig.json
connector/           manifest.json  server/index.js  test/*.test.js  package.json (devDependencies only)
docker/              Dockerfile.server  Dockerfile.tts
deploy/              docker-compose.example.yml  config.example.json  .env.example  README.md (restore)
testdata/packs/      valid/*.json  invalid/*.json   (synthetic only, §14.4)
tools/               dep-age.py  repo-guard.py
docs/                requirements.md  design/  research/  vocab-pack.schema.json  claude-*.{md,txt}
.github/workflows/   ci.yml  release.yml
```

`Cargo.toml` uses exact requirements (`axum = "=0.8.9"`); workspace `default-members` excludes `vocab-tts` so that a plain `cargo build` on the Mac does not need the sherpa-onnx archive; CI and the guarantor run `--workspace`. There are no Cargo features in the project crates, so "all feature combinations" (R-93) = the default set; `sherpa-onnx` is used with its default `static` feature only.

### 14.2 CI (GitHub Actions, all actions pinned by commit SHA)

`ci.yml` on push/PR:

| Job | Steps |
|---|---|
| `guards` | `python3 tools/dep-age.py` — reads `Cargo.lock`, both `package-lock.json`, Dockerfile/compose digests, workflow Action SHAs, the sherpa-onnx/Kokoro URLs; queries crates.io / npm / registry / GitHub APIs; **fails if anything is < 14 days old** (R-90, transitive included). `python3 tools/repo-guard.py` — fails on `*.sqlite*`, `.env`, files > 1 MB outside allowed paths, fixture packs whose `source.id` does not start with `test-`, and `innerHTML|outerHTML|insertAdjacentHTML|document.write|eval(|new Function` under `web/js`. `cargo-deny check` (advisories, licences allow-list incl. GPL-3.0/LGPL-3.0 **only** for `vocab-tts`, bans, sources = crates.io only). |
| `rust` | toolchain from `rust-toolchain.toml` (preinstalled `rustup`, no third-party action); download sherpa-onnx static archive with `sha256sum -c`, export `SHERPA_ONNX_ARCHIVE_DIR`; `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo test --workspace --locked`. |
| `web` | `actions/setup-node` (Node 24.22.2); `npm ci` in `web/`; `npm run typecheck` (`tsc --noEmit`, checkJs); `node --test web/test`; e2e: start the `vocab-server` binary built by `rust` (artifact) with a temp data dir, test config, no TTS (exercises the R-41 fallback), run Playwright (Chromium + WebKit, 375×812 and desktop viewports). |
| `connector` | `node --test connector/test` (MCP handshake replay, tool calls against a fake API, token never logged). |
| `images` | buildx build of both images; smoke test: run the app image with `--read-only --cap-drop ALL --security-opt no-new-privileges --user 65532:65532 --tmpfs /tmp`, wait healthy, `/healthz` 200, login with a test hash, `--entrypoint /bin/sh` fails; TTS image: synthesize "hello" and check an MP3 frame header. |

`release.yml` on tag `v*`: same checks, then push both images to GHCR (`permissions: packages: write` in that job only), provenance + SBOM, digests written to the release notes.

### 14.3 Renovate (`renovate.json`)

```json
{
  "$schema": "https://docs.renovatebot.com/renovate-schema.json",
  "extends": ["config:recommended", "helpers:pinGitHubActionDigests", "docker:pinDigests"],
  "minimumReleaseAge": "14 days",
  "internalChecksFilter": "strict",
  "prCreation": "not-pending",
  "rangeStrategy": "pin",
  "vulnerabilityAlerts": { "enabled": true, "minimumReleaseAge": "14 days" },
  "lockFileMaintenance": { "enabled": true, "schedule": ["before 6am on the first day of the month"] },
  "packageRules": [
    { "matchManagers": ["cargo"], "rangeStrategy": "bump" },
    { "matchUpdateTypes": ["major"], "dependencyDashboardApproval": true },
    { "matchPackageNames": ["fsrs"], "dependencyDashboardApproval": true }
  ],
  "customManagers": [
    {
      "customType": "regex",
      "managerFilePatterns": ["/^rust-toolchain\\.toml$/"],
      "matchStrings": ["channel = \"(?<currentValue>[0-9.]+)\""],
      "datasourceTemplate": "github-releases",
      "depNameTemplate": "rust-lang/rust"
    },
    {
      "customType": "regex",
      "managerFilePatterns": ["/^docker/Dockerfile\\.tts$/"],
      "matchStrings": ["ARG SHERPA_ONNX_VERSION=(?<currentValue>[0-9.]+)"],
      "datasourceTemplate": "github-releases",
      "depNameTemplate": "k2-fsa/sherpa-onnx",
      "extractVersionTemplate": "^v(?<version>.+)$"
    }
  ]
}
```

- `rangeStrategy: bump` for Cargo keeps the `=x.y.z` exact pins. Security fixes are also held for 14 days (owner's hard rule, R-90); the CI age gate is the backstop for anything Renovate does not cover (transitive updates in lock-file maintenance, which Renovate does not age-filter: such a PR simply stays red until all its packages are 14 days old).
- The sherpa-onnx archive checksum and the Kokoro model URL/checksum are updated by hand in the same PR (CI fails on a checksum mismatch). `fsrs` majors need a look at memory-state compatibility.
- Debian packages of the build stages come from a fixed `snapshot.debian.org` timestamp (dated by dep-age); Renovate cannot bump it — timestamp and pins are updated by hand together (steps in the Dockerfile comment).

### 14.4 Tests keyed to R-IDs (R-94)

Every test name starts with its R-ID (`r23_sentence_with_two_spans_is_rejected`, `test('R-62 …')`), so `grep R-xx` lists coverage; `tools/repo-guard.py` prints a coverage table of MUST R-IDs without any test.

| Level | Tooling | Covers |
|---|---|---|
| Unit (Rust) | `cargo test` | `vocab-pack`: R-10, R-15, R-20, R-23 (marker scanner property tests on generated strings), keys/hash. `scheduler`: R-50, R-51, R-52 (unlock rules, sibling rule), R-55, R-58, R-60, R-61 using a fake clock. `session`: R-54, R-59 (scope), interference rule. `auth`: R-82 (throttle, cookie flags, token scope, Origin). |
| Integration (Rust) | `axum` router via `tower::ServiceExt::oneshot`, temp SQLite | Import end-to-end incl. merge rules R-03, R-22, "update the sentences" re-export, idempotency, conflicts, R-11 preview/commit, R-84 limits; review → FSRS → due; undo; audio queue with a fake TTS HTTP server (retries, file naming, reconciliation) R-40; migrations; exports R-57, R-86; connector endpoints R-12. |
| Unit (JS) | `node --test` | `grading.js` (R-56, R-58 normalisation, suggested grades, diff), `dom.js` escaping, key map (R-70). |
| E2E | Playwright (Chromium + WebKit, phone + desktop viewports) | Login (R-82), paste import with preview (R-11, R-23), Understand session with keyboard and touch (R-53, R-54, R-60–R-62, R-70), dark/light (R-71), fallback speech when no clip (R-41), word edit (R-04, R-25), browse (R-73), progress (R-72), Produce/Spelling flows (R-56, R-58), leech screen (R-55). |
| Container | CI `images` job | R-83 (no shell, read-only, non-root), healthchecks. |
| Manual, once | Owner/guarantor checklist in `deploy/README.md` | R-80/R-81 (Traefik home only, 404 on websecure), R-85 restore drill, R-12 in Claude Desktop, R-14 real exports. |

### 14.5 Keeping fixtures synthetic (R-24, R-95)

- `testdata/packs/` contains only sentences written for the tests (or public-domain text with a `location` such as "Project Gutenberg #11"), sources with ids `test-…`, and invented headwords where convenient. Fixtures are plain v1 packs; the provenance rule is enforced by `repo-guard.py` (source id prefix `test-`) and by review.
- `.gitignore`: `*.sqlite*`, `/dev-data/`, `.env`, `*.mcpb`, `packs/`, `exports/`. Local development uses `VOCAB_DATA_DIR=./dev-data` (ignored).
- The owner's real packs and old HTML pages are imported at runtime only; e2e tests never read anything outside `testdata/`.
- Logs: request logs contain method, path template (no query values), status, duration; importer logs pack_id and counts only.

---

## 15. Learning best-practice check

`docs/research/training-ui.md` already grounds most choices (retrieval practice, short-answer vs multiple choice, errorful learning, receptive vs productive hierarchy, interleaving, leeches, self-rating risks, streak-free motivation). This section cross-checks the *final* design against the main findings on vocabulary learning, reuses that report where it applies, and records what was added or deliberately left out.

| Principle (evidence) | What the design does | Gap closed / left for later |
|---|---|---|
| **Four strands** — balanced course = meaning-focused input, meaning-focused output, language-focused learning, fluency; deliberate study is one strand, roughly a quarter of the time ([Nation 2007](https://openaccess.wgtn.ac.nz/articles/journal_contribution/The_four_strands/12552167), doi:10.2167/illt039.0) | The app is the **language-focused** strand. The owner's reading of real books is the meaning-focused input strand, and the app feeds on it (real book sentences, reading position). Defaults (5 new words/day, 20-card blocks ≈ 5 min, ≈ 10–15 min/day) keep deliberate study a small share next to reading. | **Left for later:** fluency (listening variant with hidden text, fast re-reading of known sentences) and meaning-focused output (free writing) are phase 2 / outside the app. |
| **Retrieval practice / testing effect** ([Roediger & Karpicke 2006](https://doi.org/10.1111/j.1467-9280.2006.01693.x); research §1) | Every card is a retrieval attempt before any answer is shown; there is no passive "study" mode; new words are tested cold, then shown in full (errorful generation followed by feedback, research §2). | — |
| **Spacing**; expanding vs equal schedules differ little once retrieval succeeds ([Cepeda et al. 2006](https://doi.org/10.1037/0033-2909.132.3.354); [Nakata 2015](https://www.cambridge.org/core/journals/studies-in-second-language-acquisition/article/effects-of-expanding-and-equal-spacing-on-second-language-vocabulary-learning/D1D796306985C52F9BE7A1200AC50DB9)) | FSRS-6 sets intervals per card from its own history; short in-session repeats after a failure, then next day. FSRS is the best-performing open scheduler in the public benchmark ([srs-benchmark](https://github.com/open-spaced-repetition/srs-benchmark)). | — |
| **Retrieval effort / desirable difficulty** (Pyc & Rawson 2009; Bjork — research §1) | Desired retention 0.82–0.93 (not 0.97+): retrievals are effortful but mostly successful; backlog pause prevents overload. | — |
| **Enough encounters** — knowledge grows over ~10 meetings, across several aspects ([Webb 2007](https://doi.org/10.1093/applin/aml048)) | 3–5 sentences per word rotate (least-recently-shown, not within 7 days), so reviews are also new contextual encounters; audio adds spoken encounters. | — |
| **Receptive before productive; productive builds fuller knowledge** (Webb 2009; Laufer & Goldstein 2004 — research §1) | Separate Understand and Produce cards; Produce unlocks only for `active` words after Understand stability ≥ 7 days; never both on one day. | — |
| **Context: inferring alone is unreliable, inferring + verifying works** ([Mondria 2003](https://doi.org/10.1017/S0272263103000202)); **deliberate decontextualised learning also builds lexical knowledge** ([Elgort 2011](https://doi.org/10.1111/j.1467-9922.2010.00613.x)) | Understand = read in context → **verify** against the full Czech translation with the Czech equivalent marked; the criterion line makes "only guessed from context" a 3 (Shaky), not a 1–2. Word-only cards remain for words without sentences; Produce removes context support for the form. | **Left for later:** occasional word-only "hard check" for mature Understand cards (research §9, phase 2) to measure context-free recall. |
| **Involvement load** — need, search, evaluation predict retention ([Laufer & Hulstijn 2001](https://doi.org/10.1093/applin/22.1.1)) | Understand: need + meaning search (moderate). Produce: form search + evaluation (the word must fit the sentence and its form) — higher load for the `active` words where it matters. Spelling adds form search for tricky words. | **Deliberately left out:** the highest-load task (writing one's own sentences) needs judgement, which the thin app may not make. Phase-2 idea: export "write a sentence with X" prompts and let Claude check them in the project. |
| **Interference between similar words** — learning semantically or formally similar words together hinders learning; contrast them only once one is established ([Tinkham 1997](https://ideals.illinois.edu/items/19069); Nation 2000, *TESOL Journal* 9(2), [summary](https://skyteach.ru/2020/01/03/learning-vocabulary-in-lexical-sets-by-paul-nation/)) | **Closed:** the session builder never introduces two new words on the same day when one is listed in the other's `confusables` (§6.7 step 4; mechanical, uses pack data). New words are ordered by usefulness, not grouped into lexical sets. Confusables are contrasted only on the leech screen, i.e. after interference has shown up. | — |
| **Aspects of word knowledge** — form (spoken, written), meaning, use ([Nation 2013](https://doi.org/10.1017/CBO9781139858656)) | Spoken form: neural audio on every word and sentence, auto-played on reveal (the owner met these words in print only). Written form: Produce and Spelling. Meaning: Understand with the Czech equivalent. Use: real sentences, `grammar`, `forms`, `note`. | **Left for later:** explicit collocation practice (available only as text in `note`). |
| **Prioritise by usefulness/frequency** (Nation 2013) | Usefulness orders new words, sets retention, and orders the backlog; low-priority words are not introduced automatically. | — |
| **Feedback and attention to errors** (Metcalfe 2017; Butler & Roediger 2008 — research §3) | After 4/5 the answer stays until Next, the note opens, typed vs correct answer shown aligned; 5 opens the full learn view. | — |
| **Learners misjudge their own learning** ([Kornell & Bjork 2007](https://doi.org/10.3758/BF03194055)) | Criterion printed on the card; legend; Produce/Spelling suggestions are objective; Progress shows real retention vs target per card kind, exposing lenient self-rating. | — |
| **Interleaving** (Kornell 2009 — research §5) | New words interleaved 1 in 4; blocks mix sources by default (R-59 lets the owner narrow to one source when he wants). | — |

---

## 16. Dependency table

All versions checked on 2026-11-17 against the registries; cut-off for the 14-day rule = published on or before **2026-11-03**. "Newest" is given where a newer release exists that is still too young. Transitive dependencies are pinned through the lock files and gated by `tools/dep-age.py` in CI; the implementation agent records the resolved lock file in its milestone report.

### 16.1 Rust — `vocab-pack` and `vocab-server` (runtime)

| Crate | Version | Published | Licence | Why |
|---|---|---|---|---|
| axum | 0.8.9 | 2026-04-14 | MIT | HTTP routing/extractors |
| tokio | 1.54.1 | 2026-10-21 | MIT | async runtime, timers, `spawn_blocking` |
| hyper | 1.12.0 | 2026-10-06 | MIT | HTTP (via axum; also the TTS client) |
| hyper-util | 0.1.21 | 2026-09-24 | MIT | server glue + `client-legacy` for calling vocab-tts |
| http-body-util | 0.1.5 | 2026-08-12 | MIT | collecting response bodies |
| rusqlite (feature `bundled`) | 0.41.1 | 2026-10-28 | MIT | SQLite access (newest 0.43.0 is 2026-11-15, too young) |
| libsqlite3-sys | 0.39.1 | 2026-10-25 | MIT | bundled SQLite C source (newest 0.40.0 too young) |
| serde | 1.0.229 | 2026-07-18 | MIT/Apache-2.0 | (de)serialisation |
| serde_json | 1.0.154 | 2026-10-11 | MIT/Apache-2.0 | JSON, stream parsing of pastes |
| fsrs | 6.6.2 | 2026-08-29 | BSD-3-Clause | FSRS-6 scheduler + optimiser (7.0.0 of 2026-11-10 too young; 6.7.0 yanked). Pulls ndarray 0.17.2 (2026-01-10), rayon 1.12.0 (2026-04-14), rand 0.10.x, snafu, strum, itertools, priority-queue =2.7.0 |
| argon2 | 0.6.0 | 2026-08-27 | MIT/Apache-2.0 | password hash verification |
| getrandom | 0.4.3 | 2026-06-17 | MIT/Apache-2.0 | session tokens, ids |
| sha2 | 0.11.1 | 2026-10-12 | MIT/Apache-2.0 | token digests, content hashes |
| hex | 0.4.3 | 2021-03-03 | MIT/Apache-2.0 | hex encoding |
| subtle | 2.6.1 | 2024-06-24 | BSD-3-Clause | constant-time token comparison |
| unicode-normalization | 0.1.26 | 2026-10-16 | MIT/Apache-2.0 + Unicode-3.0 | NFC/NFKD for identity keys |
| jiff (+ bundled tzdb) | 0.2.39 | 2026-11-02 | Unlicense/MIT | time zones/study day without system tzdata (1.0.0 of 2026-11-15 too young) |
| jiff-tzdb | 0.1.9 | 2026-10-06 | Unlicense/MIT | embedded IANA tz database |
| include_dir | 0.7.4 | 2024-06-17 | MIT | embed `web/` into the binary |
| tracing | 0.1.44 | 2025-12-18 | MIT | logging |
| tracing-subscriber | 0.3.23 | 2026-03-13 | MIT | log formatting |
| thiserror | 2.0.21 | 2026-09-23 | MIT/Apache-2.0 | error types |

### 16.2 Rust — `vocab-tts` (runtime, additional)

| Crate / artefact | Version | Published | Licence | Why |
|---|---|---|---|---|
| sherpa-onnx (default `static`) | 2.0.1 | 2026-10-27 | Apache-2.0 | Rust API for the TTS engine |
| sherpa-onnx-sys | 2.0.1 | 2026-10-27 | Apache-2.0 | FFI + static link |
| sherpa-onnx native static libs `sherpa-onnx-v2.0.1-linux-x64-static-lib.tar.bz2` | v2.0.1 | 2026-10-26 (GitHub release) | Apache-2.0 (+ onnxruntime MIT; espeak-ng GPL-3.0 front-end) | engine + onnxruntime; downloaded in Docker build with pinned sha256 |
| mp3lame-encoder | 0.2.5 | 2026-08-20 | LGPL-3.0 | MP3 encoding (0.3.0 of 2026-11-03 is exactly at the cut-off; 0.2.5 chosen as the more mature line) |
| mp3lame-sys | 0.1.11 | 2025-12-04 | LGPL-3.0 | bundled LAME |
| ebur128 | 0.1.11 | 2026-10-26 | MIT | EBU R128 loudness + true peak |
| axum, tokio, serde, serde_json, tracing, tracing-subscriber, thiserror, subtle, sha2 | as in 16.1 | | | |

### 16.3 Rust — dev-dependencies and tools

| Item | Version | Published | Licence | Why |
|---|---|---|---|---|
| jsonschema (dev) | 0.60.2 | 2026-10-26 | MIT | schema ↔ importer conformance tests (0.61.x too young) |
| tempfile (dev) | 3.27.0 | 2026-03-11 | MIT/Apache-2.0 | temp DBs/dirs in tests |
| tower (dev, `util`) | 0.5.3 | 2026-01-12 | MIT | `ServiceExt::oneshot` for router tests |
| Rust toolchain | 1.99.0 | 2026-10-01 | MIT/Apache-2.0 | compiler (1.99.1/1.100.0 images too young) |
| cargo-deny | 0.20.2 | 2026-07-09 | MIT/Apache-2.0 | advisories/licences/sources gate in CI (0.21.0 too young) |

### 16.4 Model, images, OS packages

| Item | Version / digest | Published | Licence | Why |
|---|---|---|---|---|
| Kokoro-82M v1.0, `kokoro-multi-lang-v1_0.tar.bz2` (sherpa-onnx `tts-models`) | sha256 `60703707bc05ccd48d60e1eb723f38b07d6e474795a0b6a588d7220400e0f7a4` | 2026-10-26 (asset upload) | Apache-2.0 (weights/voices), GPL-3.0 (bundled espeak-ng-data) | TTS model |
| `rust:1.99.0-slim-trixie` | `sha256:2752b332db73fdbb7dc576f06c82ed1f312005784ef913d7e04a28f5f55dc581` | 2026-10-09 | various (Debian) | build stage only |
| `gcr.io/distroless/static-debian13:nonroot` | `sha256:25ac8f0c3b2334a9eb05452266bd4697864a8c8ef0ef71e5e6480d7f92868329` | 2026-10-31 | Apache-2.0 + Debian | app runtime |
| `gcr.io/distroless/cc-debian13:nonroot` | `sha256:08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1` | 2026-10-31 | Apache-2.0 + Debian | TTS runtime (glibc + libstdc++) |
| Debian `musl-tools` (build stage) | 1.2.5-3.1~deb13u1 (trixie) | trixie stable update; date checked by dep-age via snapshot.debian.org | MIT | musl static build of the app |
| Debian `bzip2` (build stage) | 1.0.8-6 (trixie) | trixie release | bzip2 licence | unpack model/libs archives |

### 16.5 JavaScript (dev-only; no runtime npm dependencies anywhere)

| Package | Version | Published | Licence | Why |
|---|---|---|---|---|
| Node.js (CI) | 24.22.2 LTS | 2026-10-27 | MIT | runs tests/tools in CI |
| typescript | 7.0.2 | 2026-07-08 | Apache-2.0 | `tsc --noEmit --checkJs` for `web/` and `connector/` |
| @playwright/test | 1.64.1 | 2026-10-14 | Apache-2.0 | e2e tests (1.65.x too young) |
| @anthropic-ai/mcpb | 2.1.2 | 2025-12-04 | MIT | packs the `.mcpb` bundle (dev-only CLI) |

### 16.6 GitHub Actions (pinned by SHA)

| Action | Tag | SHA | Published |
|---|---|---|---|
| actions/checkout | v7.0.1 | `3d3c42e5aac5ba805825da76410c181273ba90b1` | 2026-07-20 |
| actions/setup-node | v7.1.0 | `949feb2413d6458794dcd2491c4babbbce0c15c1` | 2026-10-08 |
| docker/setup-buildx-action | v4.4.1 | `f87e5991a6d7451dcb8d9637bfbc97413f497069` | 2026-09-16 |
| docker/login-action | v4.7.0 | `81c6f93d4bc991e510b342a00b419b3bb4efc5c6` | 2026-10-23 (v4.8.0 too young) |
| docker/metadata-action | v6.2.0 | `dc802804100637a589fabce1cb79ff13a1411302` | 2026-07-02 |
| docker/build-push-action | v7.4.0 | `c3c9e263c25d99ce0380d002d59b67737d91b0dc` | 2026-09-15 |

Renovate itself runs as the hosted GitHub App (not a pinned dependency of the repo).

---

## 17. Implementation plan

Each milestone is sized for one implementation-agent run, leaves `main` green (fmt, clippy `-D warnings`, tests, dep-age), and ends with the guarantor's checks. The data model (all tables of §4) and the pack format are complete from M1/M2 so later milestones add behaviour, not migrations.

| # | Milestone | Done when | R-IDs |
|---|---|---|---|
| **M1** | **Foundations** — workspace (`vocab-pack`, `vocab-server`), `rust-toolchain.toml`, exact-pinned deps, `config`, `clock`, `db` (PRAGMAs, migration `0001` = full §4 schema, `quick_check`), `http` skeleton (security headers, limits, `/healthz`, embedded web shell), subcommands `serve/healthcheck/check`, CI (`guards` incl. dep-age + repo-guard + cargo-deny, `rust`, `web` typecheck), Renovate config, `.gitignore`. | CI green on a PR; `dep-age.py` passes on the lock file; migration test (empty DB → v1, FK + integrity checks) passes; WAL files appear only in the data dir; `GET /` returns the shell with the CSP header. | R-85 (DB mode), R-90, R-91, R-92, R-93, R-94, R-95 |
| **M2** | **Pack format & import** — `vocab-pack` (paste, validate, marked, keys, hash), synthetic fixtures valid/invalid, schema conformance test, `catalog` merge rules, `importer` preview/commit/idempotency, source reading position, Import page (paste → preview → import), read-only word list + minimal word view to inspect results. | Every rule of §5.2 and §5.4 has a test; the example pack from `claude-export-instructions.md` and a synthetic "update the sentences" re-export import correctly; second import of the same pack is a no-op; preview never writes; e2e: paste two packs (one with a bad sentence) → preview shows the error → import. | R-01, R-02, R-03, R-10, R-11, R-13, R-15, R-20, R-22, R-23, R-24, R-59 (reading position), R-84 |
| **M3** | **Scheduler & session engine (API)** — `scheduler` (cards, FSRS 6.6.2, 1–5 mapping with 1→Good before fitting, retention table, review log with raw grade, undo snapshots, leech flagging, Produce/Spelling unlock rules), `session` (scope, new-word limit, backlog pause, interleave, sibling rule, interference rule, sentence rotation, requeue, resume, undo, summary), `settings`, `stats::today`. Session builder serves only card kinds whose UI exists (`understand` until M8/M10). | Unit tests with a fake clock for every rule in §6; integration test: import fixture → start block → rate → due dates/states as expected → undo restores exactly; idempotent review posts. | R-50, R-51, R-52, R-54, R-55 (flagging), R-58 (unlock), R-59, R-60, R-61 |
| **M4** | **Practice UI: Understand** — Today (scope chooser, counts, forecast, Start/Resume), Practice Understand (sentence + word-only), new-word full learn view, rating bar with all safeguards, toast + undo, keyboard map, block summary, Settings (new/day, block size, theme, legend), dark + light theme, browser-speech audio buttons (fallback path). | Playwright (WebKit 375×812 + Chromium desktop): full block by touch and by keyboard; inverted-scale safeguards present (labels, colours, divider, filled/outlined, guard, toast); resume after reload; undo. | R-41, R-52, R-53, R-54, R-59, R-60, R-62, R-70, R-71, R-74 |
| **M5** | **Auth, hardened image, deployment → first usable version** — `auth` (password, cookie sessions, Origin guard, throttling, import-token extractor), login screen, `hash-password`, `Dockerfile.server`, image smoke tests, release workflow to GHCR, `deploy/` (compose example = §12.2, config/env examples, README with dataset/UID setup, DNS note and restore procedure). | All R-82/R-83 tests pass; image runs read-only/non-root/no-shell in CI; owner can deploy following `deploy/README.md`; a real practice session works on the phone over the `home` entrypoint (browser speech). | R-80, R-81, R-82, R-83, R-84, R-85 |
| **M6** | **Neural TTS** — spike first (load Kokoro with sherpa-onnx 2.0.1, measure RTF on the CI runner, decide fp32/int8), `vocab-tts` (engine, post-processing, API, healthcheck), `Dockerfile.tts`, `audio` module (queue, worker, retries, files, reconciliation, GC, serving with range requests), audio controls with preload/auto-play/speed/mute, audio status in Settings, voice setting (US/UK). | Integration tests with a fake TTS; TTS image smoke test produces a valid MP3 at ≈ −16 LUFS; e2e: clip ready → played from server, not ready → fallback; compose updated with `vocabulary-tts` on the internal network. | R-40, R-41, R-42, R-83 (TTS container) |
| **M7** | **Words: browse, detail, edit** — Words screen (search, sort, filters), word detail with history, edit any field (overrides), sentence edit/hide, conflicts resolution, accepted answers, goal switch, usefulness, include low-priority, suspend, reset card, delete word. | e2e for each action; overrides survive a re-import (merge rule 4); edited sentence re-validated and its audio re-queued. | R-03 (conflict UI), R-04, R-25, R-51 (include), R-52 (goal switch), R-73 |
| **M8** | **Produce card** — enable `produce` in the session builder; question/answer screens; `grading.js` (normalisation, accept set, suggested grade, hint, "I don't know", "I'll just rate myself"); owner-accepted answers. **Before enabling: cap first-time Produce/Spelling cards per day and give them a neutral priority.** Numbers from the review's simulation (probe L: 38 study days, 105 words, 1 174 reviews): on the day `produce`/`spelling` are first served, 29 Produce and 9 Spelling cards that were unlocked meanwhile are due at once, next to 25 due Understand cards; a never-reviewed card counts as `R_now = 0`, so all 38 sort ahead of every real review, and 10 of the 25 due Understand cards lose their turn to a sibling card (one card per word per day). | JS unit tests for normalisation/suggestions; e2e on WebKit phone viewport incl. keyboard overlap; no audio on the question side. | R-52, R-56 |
| **M9** | **Leeches, progress, exports** — leech screen and actions, Progress view (forecast, per-source incl. reading position, retention vs target, activity strip, leech list, conflicts), feedback export (R-57), full export (R-86). | Tests for leech actions and acknowledgement; progress numbers checked against a fixture history; exports validate against their documented shape and contain no data beyond the spec. | R-55, R-57, R-72, R-86 |
| **M10** | **Spelling card** — enable `spelling`, cap 3 per block, audio prompt, letter diff, `spelling_note`. | Unit tests (diff, strict compare, cap, retention 0.80, unlock); e2e flow. | R-58 |
| **M11** | **MCP connector** — `/api/connector/v1/*` (if not already added in M5), `connector/` extension, protocol replay tests, packaging, install doc. | Tests green; manual check in Claude Desktop: `list_sources`, `add_words` (dry run + real) from the project chat; token not visible in any config file. | R-12 |
| **M12** | **FSRS personal parameters & polish** — `fit_parameters_if_due` (+ log-loss comparison, memory-state recompute, 1→Easy switch, Settings line), daily maintenance (checkpoint, GC), accessibility pass (screen reader labels, contrast), WebKit e2e on all screens. | Fitting tested on a synthetic history; Settings shows parameter status; axe-style manual checklist done. | R-50, R-60, R-70, R-71 |

The **first usable version** is the end of **M5** (import → Understand practice → ratings → FSRS, deployed, browser speech); **M6** brings neural audio. Before M6 the owner can already import his migrated pages and practise.

---

## 18. Traceability matrix

| R-ID | Level | Design section(s) | Milestone | Remark |
|---|---|---|---|---|
| R-01 | MUST | §4.2 (`word`, `word_item`, `word_form`, `word_source.seq/tag`), §5.4, schema | M2 | `forms` free-text variant not in format v1 (Claude converts); see P-6 |
| R-02 | MUST | §4.2 `source`, `word_source`; §5.4 | M2 | |
| R-03 | MUST | §5.3, §5.4 (conflict rule), §4.2 `field_conflict` | M2, M7 | |
| R-04 | MUST | §7.1 word detail, §8 `PATCH /api/words`, §4.2 `word_override` | M7 | |
| R-10 | MUST | §5, `docs/vocab-pack.schema.json` | M2 | |
| R-11 | MUST | §5.1, §7.1 Import, §8 | M2 | |
| R-12 | MUST | §11, §9.2, §8 connector endpoints | M11 | |
| R-13 | MUST | §5.4, §5.5 (order across parts) | M2 | Conversion done by Claude; P-1 for old `seq` |
| R-14 | MUST | `docs/vocabulary-export-instructions.txt` (guarantor), §5, §19.2 | — | Validated by real exports (manual) |
| R-15 | MUST | §5.2, §6.3, §6.4, schema | M2, M3 | No extra pack fields needed in v1 |
| R-20 | MUST | §4.2 `sentence`, §5.2 | M2 | |
| R-21 | MUST | instruction text (guarantor) | — | App only stores; no sentence selection in the app |
| R-22 | MUST | §5.3 `sentence_key`, §5.4 | M2 | Also covers "update the sentences" re-exports |
| R-23 | MUST | §5.2 | M2 | |
| R-24 | MUST | §5.1 step 5, §13.1, §14.5 | M1–M2 | |
| R-25 | SHOULD | §7.1 word detail, §8 `PATCH /api/sentences` | M7 | |
| R-40 | MUST | §10 | M6 | |
| R-41 | MUST | §7.4, §10.3 step 9 | M4 (fallback), M6 | |
| R-42 | SHOULD | §10.1 voices, §10.3, Settings | M6 | |
| R-50 | MUST | §6.1 | M3, M12 | FSRS justified in §1.1/§15 |
| R-51 | MUST | §6.2, §6.6, §6.7 | M3, M7 | |
| R-52 | MUST | §6.4, §6.7, §7.1 | M3, M4, M8 | Sibling rule generalised to all card kinds (D-1) |
| R-53 | MUST | §7.1 screen 2, §6.7 step 6 | M4 | |
| R-54 | MUST | §6.6, §6.7 | M3, M4 | Undo is multi-step (D-3) |
| R-55 | MUST | §6.5, §7.1 | M3 (flag), M9 | |
| R-56 | MUST | §6.8, §7.1 screen 3 | M8 | |
| R-57 | SHOULD | §3.2 `exports`, §8, §19.1 D-12 | M9 | |
| R-58 | SHOULD | §6.2, §6.4, §6.7, §6.8 | M3, M10 | |
| R-63 | MUST | §6.6 (more new words), §6.9, §8 | R1 fix round | Owner feedback after v0.1.0 |
| R-59 | MUST | §5.4 (reading position), §6.7 (scope), §7.1 Today | M2, M3, M4 | |
| R-60 | MUST | §6.1, §7.2 | M3, M4, M12 | |
| R-61 | MUST | §4.2 `review_log.raw_grade` | M3 | |
| R-62 | MUST | §7.2 | M4 | |
| R-70 | MUST | §7.3, §7.5 | M4 | |
| R-71 | MUST | §7.5 | M4 | |
| R-72 | MUST | §7.1 Progress, §3.2 `stats` | M9 | Today counts already in M4 |
| R-73 | MUST | §7.1 Words | M7 | Minimal list in M2 |
| R-74 | MUST | §6–§7 | M4, M8–M10 | Exclusions honoured (no "confused with" chips, no listening variant, no gamification, no reminders) |
| R-80 | MUST | §12 | M5 | |
| R-81 | MUST | §12.2 labels/notes | M5 | |
| R-82 | MUST | §9 | M5 | |
| R-83 | MUST | §12.2, §13.2 | M5, M6 | |
| R-84 | MUST | §5.2, §9.4, §13.2 | M2, M5 | |
| R-85 | MUST | §4.1, §12.1, §12.3 | M1, M5 | No in-app backup (changed requirement) |
| R-86 | SHOULD | §3.2 `exports`, §8 | M9 | |
| R-90 | MUST | §14.2 dep-age gate, §16 | M1 | |
| R-91 | MUST | §14.3 | M1 | |
| R-92 | MUST | §3 | all | |
| R-93 | MUST | §14.1, §14.2 | all | |
| R-94 | MUST | §14.2, §14.4 | all | |
| R-95 | MUST | §14.5 | all | |

Nothing is marked infeasible.

---

## 19. Decisions, proposed changes, owner items

### 19.1 Decisions taken by this design (reversible; reasons in short)

| # | Decision | Reason |
|---|---|---|
| D-1 | At most one card **per word** per study day (Understand, Produce, Spelling), not only U/P. | Seeing the word written answers the spelling test; one rule is simpler. |
| D-2 | Understand format fixed at first review; no automatic switch when sentences arrive; "Reset card" re-chooses. | R-52 forbids format changes inside a card. |
| D-3 | Undo works step by step for all ratings of the active block (R-54 asks for "the last"). | Same mechanism, more forgiving. |
| D-4 | Pack conflicts: scalars keep existing + recorded conflict; lists union; sentences dedupe; owner removals are sticky. | R-03 "kept rather than silently overwritten", no nagging. |
| D-5 | Produce accept set = `accepted` ∪ headword ∪ marked span of the shown sentence ∪ owner-accepted. With a sentence shown only the marked span is "Correct"; another form or accepted answer is the friendly "Right word" verdict, suggestion 3 (guarantor decision of 2026-11-27, §6.8). | The marked span is by definition correct for that sentence; purely mechanical. |
| D-6 | Hint ladder reduced to one step (first letter + letter count); research's `near` answers not implemented (no such pack field in v1). | Thin app; feature hides itself without data. |
| D-7 | No "confused with…?" chips (excluded by R-74). | Requirement. |
| D-8 | Login throttling is global, not per IP. | Single user; X-Forwarded-For is spoofable from containers on the shared network. |
| D-9 | iGPU not used for TTS. | CPU is fast enough for batch generation; `/dev/dri` widens the attack surface. |
| D-10 | Reading position is displayed but does not filter sentences. | Claude already restricts sentences to the read part (instructions); filtering needs comparable positions, i.e. judgement. |
| D-11 | Session scope chooser on Today, remembered per device; daily new-word limit is global. | R-59 + simplicity. |
| D-12 | Feedback export (R-57) is a self-describing JSON `{"format":"vocab-feedback","format_version":1,"instructions_for_claude":"…", "needs_hint":[…], "overrides":[…], "hidden_sentences":[…], "owner_accepted":[…], "open_conflicts":[…]}` so it works even before the project instructions mention it. | Round trip without app-side language logic. |
| D-13 | FSRS fitting: automatic, ≥ 400 reviews and ≥ 30 days, monthly, only if log-loss improves; due dates kept. | R-60 needs a defined "fitted" moment; avoids workload spikes. |
| D-14 | UI language English (labels, decision 8); Czech only in content. | Decision 8. |
| D-15 | Lucide icon names stored, not rendered; emoji rendered. | No icon set in the image; display is nice-to-have (R-01). |
| D-16 | Full export (R-86) is a JSON dump; the regular backup and restore stay ZFS (§12.3). M9: the dump is table-shaped (§8.1), so it can also restore or move the data into a fresh app of the same schema version (round trip tested); the app has no import of it. | R-85 makes ZFS the backup. |
| D-17 | GHCR images public (like the owner's other images); no secrets or data in images. | Simplest pull on TrueNAS. |
| D-18 | Words can be deleted in the UI (with confirmation). | Recovery from a bad or spammed import (e.g. a leaked import token). |

### 19.2 Proposed changes to the format / instruction text

**Guarantor review (2026-11-17):** P-1, P-3, P-4, P-5 applied to `vocabulary-export-instructions.txt`; P-1 `seq` must be added to the schema and importer in M2 (optional integer ≥ 1; when present it orders the word within its source, otherwise §5.5 applies). P-6 noted in requirements R-01. P-2 rejected (content-hash idempotency already covers it; keep the pattern stable).

| # | Proposal | Why | Compatibility |
|---|---|---|---|
| P-1 | Optional word field `seq` (integer) for converted old pages: "keep the old page's `seq`". | R-01/R-13 want the original order preserved even if packs are pasted out of order or words are re-exported. Today the importer derives order from `part.index` and array position (§5.5). | Additive; v1 importer would warn "unknown field" and ignore it until implemented — implement together with the instruction change. |
| P-2 | Make `pack_id` unique across chats, e.g. `<source.id>--<YYYY-MM-DD>--<HHMM>` or "continue `n` from the last pack you produced in any chat". | Two chats on the same day both produce `…--1`. The app copes (idempotency uses the content hash), but the import log is clearer with unique ids. | Pattern would change → keep the current pattern accepted (v1), only the instruction text changes if `n` semantics change; `HHMM` would need a schema pattern relaxation. |
| P-3 | Add one line: "Never write `[[` or `]]` in a sentence except around the target; for omissions use `…`, not `[…]`." | Prevents sentence errors from book quotes with bracketed elisions. | Instruction only. |
| P-4 | Add a paragraph for the feedback round trip (R-57): "When I paste a `vocab-feedback` JSON, re-export the listed words: write a `hint` for `needs_hint`, replace `hidden_sentences`, reconsider `goal`/`usefulness` where I overrode them, add my `owner_accepted` answers to `accepted` if they are correct." | Closes the loop; the export also carries this text itself (D-12). | Instruction only. |
| P-5 | For "update the sentences": "copy the other fields **verbatim** from the earlier export if it is in this chat". | Rephrased scalars create (harmless) conflict records (§5.4). | Instruction only. |
| P-6 | R-01 allows `forms` as free text; format v1 only has the list. Keep it that way (Claude converts free text when migrating) and note this in the requirements. | Avoids a second shape for one field. | None. |

### 19.3 Items for the owner (only what is irreversible or outside the requirements)

1. **Licence (FYI, no action unless he objects):** the TTS container image is a GPL-3.0 combined work (espeak-ng front-end of the TTS engine) and contains LGPL LAME; the app image and the repository stay MIT. Publishing the TTS image publicly on GHCR is fine because all sources are public. Alternative if he prefers: keep the `-tts` package private on GHCR (TrueNAS then needs a read-only GHCR pull token).
2. **Hard rule consequence (FYI):** security fixes in dependencies are also held for 14 days (R-90). The design applies this everywhere (Renovate + CI gate) and does not add an exception mechanism.
3. **Owner actions at M5 (not decisions):** create the dataset and ownership (§12.1), write `.env` (secrets) and the Compose environment, add the `vocab.example.home` DNS override on the LAN/VPN resolver, deploy the stack; at M11 install the Claude Desktop extension and enter the token.
