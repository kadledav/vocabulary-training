# vocabulary-training

A small self-hosted app for learning English vocabulary from what you read:
words and example sentences are exported from a Claude conversation as a
JSON "vocabulary pack", imported into the app, and practised on the phone in
short blocks scheduled with FSRS. One user, one SQLite database, no cloud
service besides the optional Claude connector.

- `crates/vocab-server` — the server (Rust, axum, SQLite) that also serves
  the web app; `crates/vocab-pack` — the pack format and its validation.
- `web/` — the frontend: plain ES modules, no build step, no runtime
  dependencies.
- `docker/`, `deploy/` — the container image and an example Compose stack.
- `tools/` — the repository's gates (standard-library Python).
- `docs/` — requirements, design and the pack format.

**Status:** phase 1 is under construction, milestone by milestone
(`docs/execution-plan.md`). No release has been published yet; the first
will be `v0.1.0`.

## Run it locally

With the Rust toolchain of `rust-toolchain.toml` (rustup installs it):

```sh
mkdir -p dev-data
VOCAB_PUBLIC_ORIGIN=http://localhost:8080 \
VOCAB_LISTEN=127.0.0.1:8080 \
VOCAB_DATA_DIR="$PWD/dev-data" \
cargo run --locked -p vocab-server -- serve
```

Open <http://localhost:8080>. On the first start the app is in setup state:
the log shows a one-time setup code (`WARN Setup required: … setup code
XXXX-XXXX-XX`); enter it with a new password. `VOCAB_PUBLIC_ORIGIN` must be
exactly the origin the browser uses (no path, no trailing slash, no default
port). Other settings (`VOCAB_TIMEZONE`, `VOCAB_DAY_ROLLOVER_HOUR`,
`VOCAB_LOG_LEVEL`, `VOCAB_CACHE_DIR`) are listed by `vocab-server --help`.
There are no secrets in the environment: the password and the connector
token are set in the app. `dev-data/` is ignored by git.

## Deploy

The release workflow publishes a multi-platform image (`linux/amd64`,
`linux/arm64`) to `ghcr.io/kadledav/vocabulary-training`; deploy it by tag
and digest from the release notes. Steps, network rules, backup and restore:
[`deploy/README.md`](deploy/README.md).

## Vocabulary packs

How Claude exports words and sentences, and the JSON format the app
imports: [`docs/claude-export-instructions.md`](docs/claude-export-instructions.md)
(schema: `docs/vocab-pack.schema.json`).

## Dependencies: the 14-day rule

No dependency may use a release published less than 14 days ago — crates,
npm packages, GitHub Actions, container images, Debian packages, the Rust
and Node releases, tools — transitive ones included, and everything is
pinned exactly (lock files, digests, commit SHAs). `tools/dep-age.py`
enforces it in CI and fails closed. Renovate (`renovate.json`) proposes
updates only after 14 days and never merges anything by itself; the
Renovate GitHub App still has to be installed on the repository by the
owner.

Gates, as CI runs them:

```sh
python3 -m unittest discover -s tools/tests     # self-tests of the gates
python3 tools/dep-age.py                        # age and pinning of every dependency
python3 tools/repo-guard.py                     # no data, secrets or personal details
python3 tools/licenses.py --check               # THIRD-PARTY-NOTICES.txt is current
cargo deny --locked check
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
(cd web && npm ci --ignore-scripts && npm run typecheck && npm run test)
```

`dep-age.py` caches immutable registry dates in `.cache/dep-age/`; set
`GITHUB_TOKEN` to raise the GitHub API rate limit (unauthenticated: 60
requests per hour). After a change to `Cargo.lock`, regenerate the notices
with `python3 tools/licenses.py`.

## Licence

MIT, see [`LICENSE`](LICENSE). The image and binary also contain third-party
code under its own licences: [`THIRD-PARTY-NOTICES.txt`](THIRD-PARTY-NOTICES.txt)
(in the image under `/licenses/`).
