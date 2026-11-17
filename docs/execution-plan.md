# Execution Plan — phase 1

Status: **RUNNING (started 2026-11-17) with a hard usage cap: stop at 18 % weekly usage (all models)** — owner's decision. When the cap is reached the guarantor records the stop point in §5 and waits for the owner to say "continue".
Guarantor: Claude (main session). The guarantor does not write application code; it briefs agents, runs verification commands, reviews, and decides.

Inputs: `docs/requirements.md` (v1.0) · `docs/design/architecture.md` (after guarantor review) · `docs/research/training-ui.md` · `docs/claude-export-instructions.md` · `docs/vocab-pack.schema.json`

---

## 0. Before the first milestone (guarantor)

- [ ] Design agent finished; guarantor reviewed `architecture.md` against every R-ID and fixed gaps with the design agent. Owner is **not** asked to approve (decision 2026-11-17) — only irreversible items are escalated (public exposure, paid services, licence problems, breaking the 14-day dependency rule).
- [ ] Milestone list in §2 filled in from the reviewed design.
- [ ] Toolchain check on the Mac (whatever the design chose: e.g. Rust toolchain, Docker) — report what is missing instead of installing silently.
- [ ] Ask the owner once: OK to commit the docs and create branch `phase-1`? (Commits/pushes only with the owner's OK.)

## 1. Loop per milestone

| Step | Who | Model | What | Output |
|---|---|---|---|---|
| 1 | **Implementation agent** | Opus | Builds the milestone from the design section + R-IDs + "done when". Follows the owner's rules (below). | Code on branch `phase-1`, short report |
| 2 | **Guarantor check** | — | Runs build, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` (all feature combos), existing tests, lock-file dependency-age check. Anything red → back to step 1. | Pass / list of failures |
| 3 | **Testing agent** | Sonnet | Writes tests **from the requirements, not from the code**, keyed by R-ID, synthetic fixtures only; runs them. | Tests + failing list |
| 4 | **Bug-finding agent** | Opus, fresh context | Independent review of the milestone diff (doesn't see the implementer's reasoning): correctness, security (R-83/R-84), encapsulation, thin-app principle violations. Each finding verified before it counts. | Confirmed findings |
| 5 | **Fix loop** | Opus | Implementation agent fixes failing tests + confirmed findings. Max 2 rounds; then the guarantor decides (fix later / accept / escalate). | |
| 6 | **Guarantor acceptance** | — | R-ID checklist for the milestone, all checks green, commit on `phase-1`, **usage check** (§3). | Milestone done |

Rules passed to every agent:
- Never use any dependency release < 14 days old (incl. transitive, ML models, base images, Actions); pin exactly; check the lock file.
- Encapsulated modules, small interfaces, no visibility widening for shortcuts.
- Rust: `fmt` + `clippy -D warnings` green; no `#[allow]` without a comment.
- Thin app: no language logic in the app.
- No book text, personal vocabulary or secrets in the repo; synthetic fixtures only.
- No commits/pushes by agents; the guarantor commits after acceptance.

## 2. Milestones

> Tentative — replaced by the design agent's milestone plan after review.

| # | Milestone | Covers (indicative) | Owner can try? |
|---|---|---|---|
| M1 | Skeleton, SQLite schema & migrations, pack import (validate, preview, merge, idempotency) | R-01..R-04, R-10, R-11, R-15, R-20..R-25, R-84 | — |
| M2 | Understand card, 1–5 rating, FSRS scheduling, daily session, undo | R-50..R-54, R-60..R-62, R-70, R-71 | — |
| M3 | Server-side TTS pipeline + browser fallback | R-40, R-41 | — |
| M4 | Auth, hardened container, Compose stack draft for `home/docker/vocabulary/`, CI + Renovate | R-80..R-85, R-90..R-95 | **Yes — first usable version** |
| M5 | Produce card | R-52, R-56 | yes |
| M6 | Spelling card | R-58 | yes |
| M7 | Progress view, browse/search, word editing, leeches, feedback export | R-55, R-57, R-72, R-73, R-04, R-86 | yes |
| M8 | Local MCP connector for Claude Desktop | R-12 | yes |

After **M4** the plan pauses: the owner deploys and practises with real words, gives feedback; the guarantor turns feedback into changes before M5+.

## 3. Budget guard

Estimate: design 0.2–0.4 M tokens · implementation 3–6 M · testing 0.5–1.5 M · bug-finding 0.3–1 M · guarantor reviews 0.3–0.6 M → **≈ 5–10 M tokens for phase 1**. M1–M4 expected to fit in one weekly window.

- Check plan usage after **every step** (each agent run and each guarantor check).
- **Current run cap: 18 % weekly usage.** Before starting a step, estimate its cost (implementation step ≈ 2–4 %, testing/bug-finding ≈ 1–2 %, guarantor check < 1 %); don't start it if it would likely cross the cap. Running steps can't be cut exactly, so stop early rather than late.
- Default cap for later runs (unless the owner sets another): **weekly usage ≥ 80 %** → finish the current step, don't start the next one, tell the owner.
- **5-hour window ≥ 90 %** → pause until it resets.
- Extra usage is capped (monthly limit reached) — work stops at the plan limit, no surprise cost.

## 4. Owner actions (cannot be done by agents)

- At M4: create ZFS dataset `tank/services/vocabulary` with `data/`, `config/`, `cache/` and the ownership/ACL for the container UID/GID (from the design); create `.env` with secrets; deploy the stack (Portainer/TrueNAS); add the DNS name for the `home` entrypoint.
- Approve any push to GitHub / GHCR image publishing when asked.
- Paste the export instructions into the claude.ai project and produce the first real packs (incl. converting the old Bobiverse pages).

## 5. Progress log

| When | Step | Weekly usage after | Note |
|---|---|---|---|
| 2026-11-17 | Plan started, cap 18 % | 6 % | Design agent running |
