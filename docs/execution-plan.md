# Execution Plan — phase 1

Status: **PAUSED after M5 (2026-11-17)** — first usable version; waiting for the owner to push (CI verifies the image), tag a release and deploy. Next: owner feedback, then M6 (neural TTS). Single branch `main`.
Guarantor: Claude (main session). The guarantor does not write application code; it briefs agents, runs verification commands, reviews, and decides.

Inputs: `docs/requirements.md` (v1.0) · `docs/design/architecture.md` (after guarantor review) · `docs/research/training-ui.md` · `docs/claude-export-instructions.md` · `docs/vocab-pack.schema.json`

---

## 0. Before the first milestone (guarantor)

- [ ] Design agent finished; guarantor reviewed `architecture.md` against every R-ID and fixed gaps with the design agent. Owner is **not** asked to approve (decision 2026-11-17) — only irreversible items are escalated (public exposure, paid services, licence problems, breaking the 14-day dependency rule).
- [ ] Milestone list in §2 filled in from the reviewed design.
- [ ] Toolchain check on the Mac (whatever the design chose: e.g. Rust toolchain, Docker) — report what is missing instead of installing silently.
- [x] Commits go straight to `main` (single branch, owner's decision 2026-11-17); the owner pushes.

## 1. Loop per milestone

| Step | Who | Model | What | Output |
|---|---|---|---|---|
| 1 | **Implementation agent** | Opus | Builds the milestone from the design section + R-IDs + "done when". Follows the owner's rules (below). | Code on `main` (uncommitted until acceptance), short report |
| 2 | **Guarantor check** | — | Runs build, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` (all feature combos), existing tests, lock-file dependency-age check. Anything red → back to step 1. | Pass / list of failures |
| 3 | **Testing agent** | Sonnet | Writes tests **from the requirements, not from the code**, keyed by R-ID, synthetic fixtures only; runs them. | Tests + failing list |
| 4 | **Bug-finding agent** | Opus, fresh context | Independent review of the milestone diff (doesn't see the implementer's reasoning): correctness, security (R-83/R-84), encapsulation, thin-app principle violations. Each finding verified before it counts. | Confirmed findings |
| 5 | **Fix loop** | Opus | Implementation agent fixes failing tests + confirmed findings. Max 2 rounds; then the guarantor decides (fix later / accept / escalate). | |
| 6 | **Guarantor acceptance** | — | R-ID checklist for the milestone, all checks green, commit on `main`, **usage check** (§3). | Milestone done |

Rules passed to every agent:
- Never use any dependency release < 14 days old (incl. transitive, ML models, base images, Actions); pin exactly; check the lock file.
- Encapsulated modules, small interfaces, no visibility widening for shortcuts.
- Rust: `fmt` + `clippy -D warnings` green; no `#[allow]` without a comment.
- Thin app: no language logic in the app.
- No book text, personal vocabulary or secrets in the repo; synthetic fixtures only.
- No commits/pushes by agents; the guarantor commits after acceptance.

## 2. Milestones

From the reviewed design (`docs/design/architecture.md` §17 — scope, "done when" and R-IDs are defined there).

| # | Milestone | Owner can try? |
|---|---|---|
| M1 | Foundations: workspace, config/clock/db, full schema + migration, HTTP skeleton, CI guards (dep-age, repo-guard, cargo-deny), Renovate | — |
| M2 | Pack format & import (incl. optional `seq`, P-1), Import page, minimal word list | — |
| M3 | Scheduler & session engine (API) | — |
| M4 | Practice UI: Understand card, rating bar, Today, Settings | — |
| M5 | Auth, hardened image, deployment | **Yes — first usable version** (browser speech) |
| M6 | Neural TTS (Kokoro + sherpa-onnx) | yes |
| M7 | Words: browse, detail, edit | yes |
| M8 | Produce card | yes |
| M9 | Leeches, progress, exports | yes |
| M10 | Spelling card | yes |
| M11 | MCP connector (Claude Desktop extension) | yes |
| M12 | FSRS personal parameters & polish | yes |

After **M5** the plan pauses: the owner deploys and practises with real words, gives feedback; the guarantor turns feedback into changes before M6+.

Environment notes (checked 2026-11-17): local Rust 1.95 — the repo pins 1.99.0 via `rust-toolchain.toml`, rustup fetches it. **No Docker on the Mac** — container builds and image smoke tests run only in GitHub Actions, i.e. after the owner pushes; the guarantor verifies Rust/JS checks locally and asks the owner to push when a milestone needs CI evidence.

## 3. Budget guard

Estimate: design 0.2–0.4 M tokens · implementation 3–6 M · testing 0.5–1.5 M · bug-finding 0.3–1 M · guarantor reviews 0.3–0.6 M → **≈ 5–10 M tokens for phase 1**. M1–M4 expected to fit in one weekly window.

- Check plan usage after **every step** (each agent run and each guarantor check).
- **Cap for the night run of 2026-11-17: 18 % weekly usage.** Before starting a step, estimate its cost (implementation step ≈ 2–4 %, testing/bug-finding ≈ 1–2 %, guarantor check < 1 %); don't start it if it would likely cross the cap. Running steps can't be cut exactly, so stop early rather than late.
- Default cap for later runs (unless the owner sets another): **weekly usage ≥ 80 %** → finish the current step, don't start the next one, tell the owner.
- The 5-hour window is not a stop criterion (owner's decision); if it runs out, work simply resumes when it resets.
- Extra usage is capped (monthly limit reached) — work stops at the plan limit, no surprise cost.

## 4. Owner actions (cannot be done by agents)

- At M4: create ZFS dataset `tank/services/vocabulary` with `data/`, `config/`, `cache/` and the ownership/ACL for the container UID/GID (from the design); create `.env` with secrets; deploy the stack (Portainer/TrueNAS); add the DNS name for the `home` entrypoint.
- **Pushes to GitHub are done by the owner** (no GitHub credentials in the agent shell). The guarantor commits accepted milestones locally on `main` and tells the owner when there is something to push.
- Paste the export instructions into the claude.ai project and produce the first real packs (incl. converting the old Bobiverse pages).

## 5. Progress log

| When | Step | Weekly usage after | Note |
|---|---|---|---|
| 2026-11-17 | Plan started, cap 18 % | 6 % | Design agent running |
| 2026-11-17 | Design done (316k tokens), guarantor review: accepted; domain scrubbed; P-1/3/4/5/6 applied, P-2 rejected | 7 % | committed 3078bd1 |
| 2026-11-17 | M1 implementation agent started | 7 % | |
| 2026-11-17 | M1 implemented (243k tokens); guarantor gates green (fmt, clippy ×2, 32 tests, dep-age 129 items, repo-guard, typecheck); testing agent skipped for M1 (foundations/tooling, starts at M2) | 8 % | |
| 2026-11-17 | M1 bug-finding review started | 8 % | |
| 2026-11-17 | M1 bug review: 11 findings (dep-age/repo-guard bypasses, CI order, slowloris); 10 fixed, #10 moved to M2, 2 low accepted. M1 committed 5ad8705 | 9 % | owner push pending |
| 2026-11-17 | M2 implementation agent started | 9 % | |
| 2026-11-17 | M2 implemented (376k tokens); gates green (122 tests, dep-age 192); guarantor smoke import of the owner's real export: 70 Bobiverse words/223 sentences OK, re-import no-op; general pack rejected (missing source.title → format gap, fix: optional title + instruction line) | 9 % | |
| 2026-11-17 | M2 testing agent (Sonnet) + bug-finding agent started in parallel | 9 % | |
| 2026-11-17 | M2 review: 10 findings (paste memory DoS, spelling fill, seq, preview≠commit, timeout drops packs, tag wipe, invisible chars, body limit, UI race, R-92) + testing agent 83 tests / 2 bugs (same as review); fix round F1–F12 sent incl. optional source.title | ~10 % | |
| 2026-11-17 | M2 fix round done (222 tests); real-export smoke: 74 words/223 sentences, both sources, preview = commit; M2 committed db93df9 | 10 % | owner push pending |
| 2026-11-17 | M3 implementation agent started | 10 % | |
| 2026-11-17 | M3 implemented (345k tokens); gates green (286 tests, dep-age 210, cargo-deny ok); accepted deviations incl. backlog counts only served kinds (risk: Produce cards unlocked before M8 all due at once — handle in M8); M3 testing + bug-finding agents started | 11 % | |
| 2026-11-17 | M3 testing agent: 35 contract tests, 1 low defect (UUID); bug review: 9 findings (undo vs import-unlocked cards, first-review format, scoped sentences, leech same-day, resume drops add-anyway, empty scope, expired block state, duplicated filters); fix round G1–G10 sent | ~12 % | |
| 2026-11-17 | M3 fix round done (336 tests, migration 0003); real-word smoke: block of 5 new words by usefulness, ratings/requeue/undo/finish OK; M3 committed f7c2aeb | 12 % | owner push pending |
| 2026-11-17 | M4 implementation agent started (est. 3–4 % incl. reviews) | 12 % | |
| 2026-11-17 | M4 implemented (343k tokens); gates green (339 Rust, 34 node, 18 Playwright webkit-phone+chromium-desktop, dep-age 216); guarantor visual check of screenshots OK (note: "More" content hidden under fixed rating bar); M4 testing + bug-finding agents started | 13 % | |
| 2026-11-17 | M4 testing agent: 37 e2e contract tests, 4 defects; bug review: 10 findings (double-tap reveals next answer, lost-response rating mismatch, stalled request lock, dep-age YAML block bypass, Czech keyboard digits, …); fix round H1–H11, T1–T4, G sent | ~14 % | |
| 2026-11-17 | M4 fix round done (339 Rust, 36 node, 111 Playwright ×2 repeat green); M4 committed 4c3fbe7 | | owner push pending |
| 2026-11-17 | **PAUSED** before M5 — end of the night run (M5 incl. reviews would likely cross that night's 18 % cap) | | next: M5 auth + image + deploy |
| 2026-11-17 | Resumed (owner: "continue"); `phase-1` fast-forwarded into `main`, single branch from now on; owner set the cap for this run to 20 % | 14 % | M5 implementation agent started |
| 2026-11-17 | M5 implemented (463k tokens): auth module, rate limits, login screen, hash-password, Dockerfile.server, image-smoke CI job, release workflow, deploy/; gates green (370 Rust, 118 Playwright, dep-age 240) | 15 % | first CI run of M1–M4 on GitHub: all jobs green |
| 2026-11-17 | M5 bug review: no auth/CSRF/session holes; 8 confirmed (dep-age bypasses ×5, default-port origin, rate_limit visibility, release pushed untested build) + 5 plausible accepted (Referrer-Policy for Firefox, Debian snapshot pin, .env note, lockout decay, 413). Testing agent: 34 contract tests, 0 defects | 16 % | |
| 2026-11-17 | M5 fix round incl. owner changes: config via env variables only (no config file), deployment UID:GID 3101:3101; gates green (398 Rust, 67 tool tests, 132 Playwright, cargo-deny ok, repo-guard ok; dep-age 0 too young, GitHub API rate-limited locally → verified in CI); real-export smoke with env config: login, 74 words/223 sentences import, re-import no-op, logout → 401, no book text in logs. M5 committed | 16 % | image build, smoke tests and release workflow only verifiable in CI after push |
| 2026-11-17 | First CI on main (8f3dca7): guards, rust, web, e2e green; image **builds** (musl + Debian snapshot), smoke test failed (logs need a GitHub login; likely `docker top -eo uid`) | 17 % | |
| 2026-11-17 | M5.1 (owner requests): first-run setup with a logged one-time code, no secrets in the env, change password, `reset-password` (refuses while serving), in-app connector token (password re-entry), single `/data` mount with auto-created dirs, umask 077; smoke test fixed and reports failures as public annotations. Focused security review: no claim/throttle bypass; 8 fixes applied. Gates green (407 Rust, 78 tool tests, 138 Playwright; dep-age rate-limited locally). Home repo: `docker/vocabulary/` stack committed locally | 17 % | waiting for owner push → CI image-smoke → tag v0.1.0 |

### Resume notes for M5
- Docker is not installed on the Mac: image build and smoke tests run only in GitHub Actions → the owner pushes, guarantor reads CI results.
- Carry-overs: Origin check + auth on all state-changing routes (M3/M4 endpoints are unauthenticated until M5); per-endpoint rate limits (login, import); dep-age must learn Dockerfile/compose image digests and Debian package pins before the first Dockerfile lands (it fails closed on them today).
- Risk noted in M3: Produce cards unlocked before M8 all become due when Produce is enabled — spread them in M8.
