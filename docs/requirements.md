# Vocabulary Training — Requirements

Status: **v1.0 — APPROVED by the product owner (2026-11-17)**. Changes after this point go through the guarantor and are logged in §13.
Owner: David (product owner) · Guarantor: Claude (main session)
Last updated: 2026-11-17

Requirement IDs (`R-xx`) are stable: design, implementation, tests and bug reports must reference them.
**MUST** = required for phase 1. **SHOULD** = phase 1 if cheap, otherwise phase 2. **LATER** = explicitly not phase 1.

---

## 1. Purpose

A private, self-hosted web app for practising English vocabulary that the owner collects while reading books (e.g. the Bobiverse series), watching films and reading articles. Today the vocabulary lives in Claude-generated HTML pages that can only be read, not practised. The app turns that vocabulary into a spaced-repetition practice routine that adapts to how well each word is known and how useful it is.

- Single user: the owner (native Czech, advanced English learner).
- Languages: English → Czech only (phase 1).
- Devices: phone browser (primary) and desktop browser.

### Design principle: thin app, smart export

**All language judgement is done by Claude (LLM) at export time and arrives in the pack.** The app contains **no** language logic: no lemmatisation, no sentence selection, no translation, no usefulness estimation, no answer-similarity heuristics beyond simple normalisation (case, whitespace, punctuation, diacritics).

The app's job is mechanical: store vocabulary and history, schedule reviews (FSRS), run practice sessions, show metrics and generate audio (TTS).

**Scale:** a few hundred words, growing slowly; design for up to ~5 000 words without effort. **SQLite** is the storage (single file on the data volume). No separate database server.

If a feature seems to need language judgement inside the app, the answer is to add a field to the pack (R-15) — not to add logic to the app. Design proposals that violate this must be flagged to the guarantor.

## 2. Scope

| Phase 1 | Phase 2 / LATER |
|---|---|
| Import vocabulary (paste JSON), migrate existing HTML pages | Offline practice on the phone, with sync |
| Local Claude Desktop connector that pushes words | Tap an unknown word in a sentence → candidate new card |
| Real book sentences + Czech translations, supplied by Claude in the pack | Other language pairs |
| Ready-to-paste export instructions for the claude.ai project | Public-internet connector (rejected for now — see §11) |
| Server-generated neural audio | |
| Spaced-repetition practice with 1–5 self-rating | |
| Basic progress view | |

## 3. Vocabulary data

**R-01 MUST** — Each word entry supports every field present in the owner's current pages:

| Field | Current key | Required | Notes |
|---|---|---|---|
| Headword | `w` | yes | e.g. `pore over`, `carpet-bomb` — may contain spaces/hyphens |
| Czech translation(s) | `cz` | yes | free text, may list several meanings separated by `,` / `;` |
| English meaning | `m` | no | |
| Forms | `forms` | no | a list of `{form, common}` in the pack (old pages' free-text forms are converted by Claude) |
| Pronunciation respelling | `s` | no | e.g. `STRAY-tum` |
| Text to speak | `say` | no | defaults to the headword |
| Example sentences | `u` | no | 0–n strings |
| Usage note | `n` | no | long text: synonyms, pitfalls, false friends |
| Usefulness | `ux` | yes | integer 1–10 |
| Icon | `ic` / `em` | no | icon name or emoji — display is nice-to-have |
| Tag | `tag` + `tagtext` | no | `skip` (low priority), `rep` (also in earlier source), `late` (not from this source) |
| Order within source | `seq` | no | preserves the order words were added (optional pack field; old pages keep their `seq`) |

**R-02 MUST** — Every word belongs to one or more **sources**. A source has a type (`book`, `film`, `article`, `general`), a title, and optionally author, series and number in series (Bobiverse book 2).

**R-03 MUST** — The same headword arriving again (another book, another pack) is **merged**, not duplicated: learning progress is kept, the new source is linked, new example sentences are added. Conflicting field values (e.g. a different Czech translation) are kept rather than silently overwritten; the design must define the rule.

**R-04 MUST** — The owner can edit any field of a word and change its usefulness in the app.

## 4. Getting words into the app

**R-10 MUST — Exchange format.** A versioned, documented JSON "vocabulary pack" format carrying the fields of R-01/R-02 **and the practice sentences of §5** (English sentence, Czech translation, target-word position in both, location in the source). The design agent defines it as a JSON Schema.

**R-14 MUST — Export instructions for Claude.** A ready-to-paste instruction text (`docs/claude-export-instructions.md`) that the owner copies into the claude.ai "Learning english" project. After that, the owner only says e.g. *"export the words for the training app"* and Claude produces a valid pack. The instructions must cover: the schema, how to pick sentences from the book in the project (§5), how to translate them, how to rate usefulness 1–10, how to mark the target word, and splitting large exports into several packs (e.g. ≤ 20 words each) so the output is not cut off. The design agent writes it; it is validated by running it on real exports before phase 1 is accepted.

**R-15 MUST — The pack carries everything the app needs.** Besides R-01 and §5, each word in the pack includes, as decided by Claude:
- `goal`: `recognise` (understand when reading/hearing) or `active` (also produce it). Drives which exercises are used (R-52). Default if missing: `active` for usefulness ≥ 6, else `recognise`.
- `accepted`: alternative answers accepted in the Produce card (synonyms in this meaning, US/UK spellings, valid forms).
- `hint`: a short memory hook shown when the word becomes a problem word (R-55).
- `spelling`: whether the word is worth spelling practice (R-58), with optional `spelling_note` (the trap, e.g. "one s, two c").
The design agent may propose further fields if an exercise needs them (e.g. distractors for multiple choice), following the design principle in §1.

**R-11 MUST — Paste import.** An Import page where the owner pastes a pack. The app validates it, shows a preview (new words / merged words / errors), and imports on confirmation.

**R-12 MUST — Local connector (Claude Desktop).** A small local MCP server that runs on the owner's Mac, used from the Claude Desktop app. It exposes a minimal tool set (`add_words` with the pack format; optionally `list_sources`). It forwards to the app's import API over the private network (LAN / VPN / mesh VPN). It cannot delete, edit or read learning progress.

**R-13 MUST — Migration.** All words from the owner's existing HTML vocabulary pages are imported with all fields preserved (incl. source, order, tags). Migration is done by Claude, not by app code: the owner attaches an old page in the claude.ai project and asks Claude to convert it (instruction in `docs/claude-export-instructions.md`); the result is imported as ordinary packs (R-11). The app needs no HTML/JS parser.

## 5. Practice sentences

The app does **not** process books or translate anything. Claude, in the owner's claude.ai project (which holds the book PDF), picks sentences and translates them; the app only stores and uses what arrives in the pack.

**R-20 MUST** — Each word carries a set of practice sentences (target 3–5 per word for book words). Each sentence has: English text, Czech translation, the target word's span marked in both, origin (`source` = quoted from the book/article/film, or `example` = written by Claude), and location where known (chapter/page).

**R-21 MUST** — For book words, the export instructions (R-14) tell Claude to quote real sentences from the book where the word is used **in the intended meaning**, in any of its forms, preferring sentences of reasonable length with few other difficult words. Words from films, articles or general vocabulary use Claude-written examples.

**R-22 MUST** — Re-importing a word merges new sentences into the existing set (no duplicates), so the owner can ask Claude for more sentences later. The same sentence punctuated or cut slightly differently (dashes, `…` vs `...`, quote style, closing punctuation) is still the same sentence. When a later pack corrects the Czech translation or the marked span of a sentence the app already has, the correction replaces the stored version and the import report counts it — unless the owner edited or hid that sentence.

**R-23 MUST** — The importer validates sentences (target span present and inside the text; translation non-empty) and reports bad entries in the preview instead of importing them silently.

**R-24 MUST — Copyright / privacy.** Book sentences are stored only on the owner's server. They are **never committed to the git repository** (the repo is on GitHub, MIT-licensed), never sent to third parties, and never included in logs or test fixtures. Tests use synthetic or public-domain text.

**R-25 SHOULD** — The owner can correct a sentence or its translation in the app.

## 6. (removed)

Sentence translation by a local engine was dropped on 2026-11-17 — translation is done by Claude as part of the export (§5).

## 7. Audio

**R-40 MUST** — Clear, natural audio for every headword and every practice sentence, generated **on the server** with a self-hosted neural TTS model, at import time, cached as audio files. No external TTS service.

**R-41 MUST** — If a recording is not ready yet, fall back to browser speech synthesis.

**R-42 SHOULD** — Choice between US and UK voice.

Speech recognition (STT) is **out of scope** (decided 2026-11-17). The app only speaks; it never listens.

## 8. Learning and scheduling

**R-50 MUST — Spaced repetition.** Scheduling uses an established algorithm (FSRS recommended; design must justify). Each word has its own memory state and full review history.

**R-51 MUST — Usefulness drives priority.** Usefulness (1–10) affects:
- order of introducing new words (most useful first);
- desired retention per word (research proposal: ux 8–10 → 0.93 down to ux 1–3 → 0.82; design finalises). Error rate is left to FSRS — not counted twice;
- `low-priority`-tagged words are not introduced automatically; the owner can include them per word (then recognise-only, lowest retention target).

**R-52 MUST — Two cards per word.** Each word has up to two independently scheduled cards, each with its own FSRS memory state and a fixed exercise format (changing the format inside a card would corrupt its scheduling):
- **Understand** — every word. Default format: sentence comprehension (R-53). Words with fewer than 3 sentences use a word-only flip (word + audio → reveal Czech, meaning, one example); once such a word has 3 sentences its card becomes a sentence card (one-way, the only automatic format change).
- **Produce** — only words with `goal = active` (R-15), created once the Understand card has stability ≥ 7 days. Format: a sentence with the word blanked out + the Czech sentence as cue → owner produces the word (R-56) → rates.
- Understand and Produce of the same word are never scheduled on the same day.
- New words are tested cold on the Understand card (owner already met them in the book), followed by the full card.
The owner can switch a word's goal in the app (overrides the pack locally).

**R-53 MUST — Sentence comprehension exercise** (core Understand format):
English sentence from a source with the target word highlighted + play button → owner reads it → reveals the Czech translation of the whole sentence (target word highlighted in Czech too) plus the word's Czech gloss and short meaning → self-rates. Each review of the same word shows a **different** sentence when available.

**R-56 MUST — Produce answer mode.** Typed answer with a suggested grade (exact match after normalisation against `accepted` → suggest 2; otherwise suggest 4 and show the typed vs correct answer; a typo is a no-match the owner can rate 3 himself — no fuzzy matching). When a sentence with a blank is shown, only the word as it stands in that sentence is "Correct" (suggest 2, or 3 after the hint); the bare headword or another form of the word gets a separate, friendly verdict — "Right word — here it is …" — with suggestion 3, never shown as an error and never as fully correct (guarantor decision of 2026-11-27). Without a sentence the headword is correct. A spaced and a closed form ("a part" / "apart") match only when both are listed. An "I'll just rate myself" button skips typing: reveal → self-rate.

**R-54 MUST — Daily session.** Blocks of 20 cards with "Continue +20". New words (configurable 0–100 per day, default 5) are mixed among reviews and paused automatically when the review backlog grows. When today's new words are done (or paused), the owner can still ask for more: "Learn more new words" gives a block of unseen words (default: the daily number, at least 5; at most 50 per block) regardless of the daily limit and the pause; they count as introduced today. Every rating is saved immediately; a session can be interrupted and resumed; the last rating can be undone.

**R-55 MUST — Problem words (leeches).** A card is flagged after 4 lapses (or two 5s in a row on a previously mature card). A lapse is a study day on which a card that is not new was failed (4/5): at most one per card and day, however often its in-session repeats are failed or passed; in the same way only the first failure of a day lowers the card's memory state (R-50). Flagged words are not suspended automatically; their next review shows the pack's `hint` and `confusables` with options: demote to recognise-only, lower usefulness, suspend. They are listed in the progress view.

**R-58 SHOULD (phase 1) — Spelling card.** A third, independently scheduled card for words where the pack sets `spelling: true` (Claude's call: words whose spelling doesn't follow from the sound, e.g. *desiccated*, *nuisance*, *verbatim*). Flow: the app plays the word (and optionally the sentence with the word blanked) → the owner types it → letter-by-letter comparison with the correct spelling → suggested grade (exact match → 2, otherwise 4), owner confirms. Spelling is strict: a typo is a miss. To keep it occasional ("sometimes, not every time"), Spelling cards use a low desired retention (≈ 0.80) so they come up rarely, and at most ~3 Spelling cards per 20-card block. Created only after the Understand card is past the new stage.

**R-63 MUST — Extra practice.** When nothing is due (in the chosen scope), the owner can still practise — it is offered only then, due cards always come first: an extra block (block size from settings, scope respected) of words already seen that are not due yet, those rated 3–5 today first, then those most likely to be forgotten (lowest current recall probability); a card whose latest rating today was 1–2 is left out. Its ratings are ordinary reviews (the scheduler takes the real time since the last review into account, and a failure lowers the memory at most once a day, R-50/R-55). Today shows how many cards an extra block would have, how many new words are waiting, how many are left of today's limit (or why new words are paused) and how many cards are due tomorrow. If nothing qualifies, the app says there is nothing to practise.

**R-59 MUST — Practice scope by source.** A session practises all sources together by default; the owner can restrict a session to one source (e.g. only Bobiverse 2, only General Vocabulary). The latest `reading_position` received for a book is shown on that source.

**R-57 SHOULD — Feedback export for Claude.** A one-click export of what Claude should improve in the next pack (problem words without a hint, owner's overrides of goal/usefulness, hidden sentences), to paste into the claude.ai project.

## 9. Self-rating 1–5

**R-60 MUST** — Every exercise ends with the owner's self-rating **1–5, where 1 = best** (Czech school-grade direction). The rating is about the **highlighted target word only**, not the whole sentence.

| Grade | Label | Definition (legend) | FSRS grade |
|---|---|---|---|
| 1 | Instantly | sure, no effort | Easy (treated as Good until personal FSRS parameters are fitted) |
| 2 | Knew it | after a moment | Good |
| 3 | Shaky | effort, doubt, or only worked out from context | Hard |
| 4 | Vague | half-knew it, or wrong sense | Again — repeat this session |
| 5 | No idea | didn't know it at all | Again — repeat sooner, show full card |

The pass/fail line between 3 and 4 is visible in the button row. Labels are in English.

**R-61 MUST** — The raw 1–5 grade is stored for every review (not only the derived algorithm grade), so statistics and leech detection can use it.

**R-62 MUST** — The UI must make the inverted scale unmistakable (labels/colours), since other apps put "best" on the right/high end.

## 10. Practice UI and progress

**R-70 MUST** — Mobile-first, works one-handed on a phone; full keyboard control on desktop (e.g. Space = reveal, 1–5 = rate).
**R-71 MUST** — Dark theme (matching the current pages); light theme SHOULD.
**R-72 MUST** — Progress view: words due today/this week, per-source progress (e.g. per book), per-word history, problem words.
**R-73 MUST** — Browse/search all words (like the current pages: search, sort, usefulness filter) and open a word's detail.

**R-74 MUST** — The practice UI follows the reviewed research report `docs/research/training-ui.md` (wireframes, layout, accessibility, keyboard map, inverted-scale safeguards), as amended here. Where the report and this document disagree, this document wins. Not adopted for phase 1: "confused with…?" chips after a 4, listening variant, confusables drill, forms practice, gamification, reminders. Ignore the report's mentions of PDF import (superseded by §5).

## 11. Hosting, network and security

**R-80 MUST — Deployment.** Runs as a Docker Compose stack in the owner's home infrastructure repo (`home/docker/vocabulary/`) on TrueNAS Scale, on the existing `proxy-net`, behind the existing Traefik. The image is built from this repo and published to GHCR (same pattern as `another-image`).

**R-81 MUST — No public internet exposure.** Reachable only via the Traefik `home` entrypoint (LAN / VPN) and optionally mesh VPN. **Not** routed on the public `websecure` entrypoint. Any mesh VPN access requires an explicit change to the mesh-VPN ACL, limited to the owner's devices (not `*`).

**R-82 MUST — Authentication.** The UI requires login even on the private network (other household devices share the network). The import API used by the connector requires a separate secret token. No secrets in the environment: on first start the owner sets the password (≥ 12 characters) with a one-time setup code from the container log; the password can be changed in Settings (logs out every other device and replaces this device's session) and reset with a `reset-password` command; "Log out everywhere" (Settings, or the `logout-all` command) ends every session, and no session lives longer than a year; the connector token is created in Settings and shown once. Only hashes are stored.

**R-83 MUST — Hardened container.**
- Minimal image (`scratch` / distroless): no shell, no package manager.
- Runs as a fixed non-root UID/GID.
- `read_only: true` root filesystem; the only writable paths are the data volume and, if needed, a small `tmpfs`.
- `cap_drop: [ALL]`, `security_opt: [no-new-privileges:true]`, no Docker socket, no host networking, no published host ports.
- Memory, CPU and PID limits.
- Request body size limits and rate limiting on login and import.
- Base images pinned by digest.
- If the TTS model needs a heavier runtime, it runs in a **separate** container with the same hardening and **no outbound network** at runtime (models baked in or mounted read-only).

**R-84 MUST — Untrusted input.** Pack import treats input as untrusted: schema validation, size limits, all text rendered escaped (sentences and notes are never interpreted as HTML).

**R-85 MUST — Data, config & backup.** The app gets its own ZFS child dataset **`tank/services/vocabulary`** on the SSD pool, following the home convention (see `home/INFRASTRUCTURE.md` → "Storage Layout"):

| Host path | Container mount | Content |
|---|---|---|
| `/mnt/tank/services/vocabulary` | `/data`, read-write | SQLite database; `cache/` with generated audio files (regenerable from the DB), created by the app |

- **Backup** is done by the existing daily ZFS replication `tank/services` → `backup-tank/services`. Snapshots are atomic, so the live SQLite file is captured consistently; the app needs **no backup logic of its own**. The SQLite database must therefore use a crash-safe journal mode (WAL or rollback journal, `synchronous` ≥ NORMAL) and keep all its files (incl. `-wal`/`-shm`) together in the data directory.
- Configuration (non-secret `VOCAB_*` settings) comes from the Compose environment; there is no config volume and no secret in the environment (R-82).
- The container's fixed non-root UID/GID is documented (deployment: 3101:3101); the dataset owner (set in TrueNAS) is that user; the app creates its directories itself.
- A restore procedure (stop container → restore the database files from a snapshot or the replica → start) is documented in the stack's README and tested once before phase 1 is accepted.
- **R-86 SHOULD** — Export of all vocabulary + progress to a single JSON file from the UI.

## 12. Engineering rules (apply to every agent)

**R-90 MUST** — Never use a dependency release published less than 14 days ago — crates, npm, Python, tools, GitHub Actions, container images, ML models — including transitive dependencies (check the lock file). Pin exact versions.
**R-91 MUST** — Automated dependency updates (e.g. Renovate with `minimumReleaseAge: "14 days"`) so security patches arrive as reviewable PRs.
**R-92 MUST** — Encapsulated modules with small, intentional interfaces; no reaching into another module's internals; no widening visibility for shortcuts.
**R-93 MUST** — If Rust: `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` pass for all feature combinations; no `#[allow]` without a justifying comment.
**R-94 MUST** — Automated tests per requirement ID; CI builds the image and runs tests.
**R-95 MUST** — No personal data or copyrighted text in the repository (see R-24); the owner's existing vocabulary is imported at runtime, not committed.

## 13. Decisions (closed questions)

All decided by the product owner on 2026-11-17.

1. **Backup** — own child dataset `tank/services/vocabulary`, covered by the existing daily ZFS replication to `backup-tank/services`; no backup logic in the app (R-85).
2. **mesh VPN** — not in phase 1; LAN + VPN only (R-81).
3. **Login** — single password + long-lived session cookie (R-82).
4. **Practice UI** — research report adopted as amended (R-74).
5. **Existing pages** — migrated by Claude ("convert this old page" instruction), with real book sentences added (R-13).
6. **Produce card** (R-56) — typed answer with suggested grade, plus an "I'll just rate myself" button that skips typing.
7. **TTS accent** — US English; UK voice remains a SHOULD (R-42).
8. **Rating labels** — English: Instantly / Knew it / Shaky / Vague / No idea (R-60).
9. **New words per day** — default 5, configurable 0–100 (R-54); "Learn more new words" and extra practice on demand (R-54, R-63, owner feedback after v0.1.0).
10. **STT** — out of scope. **Translation / PDF processing in the app** — out of scope (done by Claude at export).
