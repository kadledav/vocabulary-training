"""Self-tests for tools/dep-age.py (R-90). Offline: registry lookups are faked.

Run: python3 -m unittest discover -s tools/tests -v
"""

from __future__ import annotations

import contextlib
import datetime as dt
import importlib.util
import io
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("dep_age", TOOLS / "dep-age.py")
assert _spec and _spec.loader
dep_age = importlib.util.module_from_spec(_spec)
sys.modules["dep_age"] = dep_age  # dataclasses look the module up by name
_spec.loader.exec_module(dep_age)
dep_age.cache_enabled = False  # never read or write the real cache (see the Cache tests)

OLD = dt.datetime(2020, 1, 1, tzinfo=dt.timezone.utc)
YOUNG = dt.datetime(2026, 11, 16, tzinfo=dt.timezone.utc)
NOW = "2026-11-17T00:00:00Z"
SHA = "3d3c42e5aac5ba805825da76410c181273ba90b1"

WORKSPACE_TOML = """\
[workspace]
resolver = "3"
members = ["crates/a"]

[workspace.package]
version = "0.1.0"
"""
MEMBER_TOML = """\
[package]
name = "a"
version.workspace = true
"""
CLEAN_LOCK = """\
version = 4

[[package]]
name = "a"
version = "0.1.0"
dependencies = ["serde"]

[[package]]
name = "serde"
version = "1.0.229"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""
CLEAN_WORKFLOW = f"""\
name: CI
on: [push]
jobs:
  a:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@{SHA} # v7.0.1
      - uses: actions/setup-node@{SHA} # v7.1.0
        with:
          node-version: 24.22.2
      - run: rustup toolchain install && cargo test --locked
      - run: cargo install --locked cargo-deny --version 0.20.2
      - run: npm ci --ignore-scripts && npm run typecheck
"""


def npm_entry(name: str, version: str, **extra) -> dict:
    base = name.rsplit("/", 1)[-1]
    entry = {
        "version": version,
        "resolved": f"https://registry.npmjs.org/{name}/-/{base}-{version}.tgz",
        "integrity": "sha512-AAAA",
        "dev": True,
    }
    entry.update(extra)
    return entry


def package_lock(packages: dict, root_deps: dict | None = None) -> str:
    """A lockfileVersion 3 lock; the root entry declares `root_deps` as
    devDependencies (default: every package at its locked version)."""
    import json

    if root_deps is None:
        root_deps = {key.rsplit("node_modules/", 1)[-1]: entry["version"] for key, entry in packages.items()
                     if key.count("node_modules/") == 1 and "version" in entry}
    root = {"name": "w", **({"devDependencies": root_deps} if root_deps else {})}
    return json.dumps({"name": "w", "lockfileVersion": 3, "packages": {"": root, **packages}})


class FakeRegistries:
    """Replaces every network lookup; names in `young` are reported as just published."""

    def __init__(self, young: set[str] | None = None):
        self.young = young or set()
        self.calls: list[tuple] = []

    def _date(self, *key) -> dt.datetime:
        self.calls.append(key)
        return YOUNG if key[0] in self.young else OLD

    def __enter__(self):
        self.saved = {
            name: getattr(dep_age, name)
            for name in (
                "crate_published", "npm_published", "rust_published", "node_published", "action_published",
                "image_published", "debian_published", "release_asset_published",
            )
        }
        dep_age.crate_published = lambda n, v: self._date(n, v)
        dep_age.npm_published = lambda n, v: self._date(n, v)
        dep_age.rust_published = lambda v: self._date("rust", v)
        dep_age.node_published = lambda v: self._date("node", v)
        dep_age.action_published = lambda repo, sha, tag: self._date(repo, sha, tag)
        dep_age.image_published = lambda name, digest: self._date(name, digest)
        dep_age.debian_published = lambda pkg, version: self._date(pkg, version)
        dep_age.release_asset_published = lambda repo, tag, asset, sha256: self._date(asset, tag, sha256)
        return self

    def __exit__(self, *exc):
        for name, fn in self.saved.items():
            setattr(dep_age, name, fn)


class RepoCase(unittest.TestCase):
    def make_repo(self, files: dict[str, str]) -> Path:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)
        for rel, content in files.items():
            path = root / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(textwrap.dedent(content) if isinstance(content, str) else content)
        return root

    def clean_files(self) -> dict[str, str]:
        return {
            "Cargo.toml": WORKSPACE_TOML,
            "crates/a/Cargo.toml": MEMBER_TOML,
            "Cargo.lock": CLEAN_LOCK,
            "rust-toolchain.toml": '[toolchain]\nchannel = "1.99.0"\n',
            ".github/workflows/ci.yml": CLEAN_WORKFLOW,
            "web/package.json": '{"devDependencies": {"typescript": "7.0.2"}}',
            "web/package-lock.json": package_lock({"node_modules/typescript": npm_entry("typescript", "7.0.2")}),
        }

    def evaluate(self, files: dict[str, str], young: set[str] | None = None):
        root = self.make_repo(files)
        with FakeRegistries(young):
            return dep_age.evaluate(root)

    def assertError(self, result, *needles: str) -> None:
        for needle in needles:
            self.assertTrue(
                any(needle in e for e in result.errors),
                f"expected an error containing {needle!r}, got:\n" + "\n".join(result.errors),
            )


class CleanRepo(RepoCase):
    def test_r90_clean_repo_passes_and_every_pin_is_looked_up(self):
        result = self.evaluate(self.clean_files())
        self.assertEqual(result.errors, [])
        kinds = sorted(f.kind for f in result.findings)
        self.assertEqual(kinds, ["action", "action", "crate", "node", "npm", "rust", "tool"])

    def test_r90_young_release_fails_the_gate(self):
        root = self.make_repo(self.clean_files())
        out = io.StringIO()
        with FakeRegistries({"serde"}), contextlib.redirect_stdout(out):
            code = dep_age.main(["--root", str(root), "--now", NOW])
        self.assertEqual(code, 1)
        self.assertIn("TOO YOUNG: crate serde 1.0.229", out.getvalue())

        with FakeRegistries(), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(dep_age.main(["--root", str(root), "--now", NOW]), 0)


class Workflows(RepoCase):
    """Reviewer reproduction r1 and further bypass attempts: all must fail closed."""

    def workflow(self, body: str, extra: dict[str, str] | None = None):
        files = self.clean_files()
        files[".github/workflows/x.yml"] = "jobs:\n  b:\n    steps:\n" + textwrap.indent(textwrap.dedent(body), "      ")
        files.update(extra or {})
        return self.evaluate(files)

    def test_r90_flow_style_and_quoted_uses_are_rejected(self):
        result = self.workflow(
            """\
            - { uses: evil/action@main }
            - "uses": evil/action2@v1
            - 'uses': evil/action4@v1
            - uses: "evil/action5@main"
            """
        )
        for line in (4, 5, 6, 7):
            self.assertError(result, f"x.yml:{line}:")

    def test_r90_unpinned_untagged_and_docker_actions_are_rejected(self):
        result = self.workflow(
            f"""\
            - uses: evil/action@main
            - uses: evil/action@{SHA}
            - uses: docker://alpine:3
            - uses: evil/action@{SHA} # latest
            """
        )
        self.assertError(result, "must be owner/repo@<full 40-hex commit SHA>", "needs a trailing '# vX.Y.Z'")
        for line in (4, 5, 6, 7):
            self.assertError(result, f"x.yml:{line}:")

    def test_r90_local_composite_actions_are_scanned(self):
        result = self.workflow(
            "- uses: ./.github/actions/x\n- uses: ./tools/ci-action\n- uses: ./missing\n",
            {
                ".github/actions/x/action.yml": "runs:\n  using: composite\n  steps:\n    - uses: evil/action3@main\n",
                "tools/ci-action/action.yaml": "runs:\n  using: composite\n  steps:\n    - run: curl -fsSL x | sh\n",
            },
        )
        self.assertError(result, "actions/x/action.yml:4", "ci-action/action.yaml:4", "local action ./missing not found")

    def test_r90_version_inputs_other_than_exact_node_version_are_rejected(self):
        result = self.workflow(
            """\
            - with:
                node-version-file: .nvmrc
                python-version: "3.12"
                node-version: 24
                "node-version": 24.22.2
            """
        )
        self.assertEqual(
            sum("only `node-version: X.Y.Z`" in e for e in result.errors), 4, result.errors
        )

    def test_r90_install_commands_must_match_the_allow_list(self):
        result = self.workflow(
            """\
            - run: cargo install cargo-deny
            - run: cargo install --locked --version 0.20.2 cargo-deny
            - run: cargo install --locked cargo-deny@0.21.0
            - run: cargo install --locked cargo-deny --version 0.20.2-rc1
            - run: pip install requests && npm install -g pnpm && docker run alpine:latest true
            - run: npx something && yarn && pnpm i && python3 -m pip install x
            - run: curl -fsSL https://example.invalid/i.sh | sh
            - run: wget -qO- https://example.invalid/x | bash
            - run: npm ci && npm install typescript
            - run: rustup target add x86_64-unknown-linux-musl
            - run: cargo +nightly build
            - run: go install example.invalid/tool@latest
            - run: |
                echo start
                sudo apt-get install -y musl-tools
            """
        )
        for needle in ["'cargo install'", "'pip'", "'npm'", "'pnpm'", "'docker'", "'npx'", "'yarn'",
                       "'curl'", "'wget'", "'| bash'", "'rustup'", "'cargo +'", "'go install'", "'sudo'",
                       "'apt-get'"]:
            self.assertError(result, needle)
        tools = [f for f in result.findings if f.kind == "tool"]
        self.assertEqual(len(tools), 1, "only the clean workflow's cargo-deny line is accepted")

    def test_r90_playwright_browsers_only_from_the_locked_version(self):
        ok = self.workflow(
            """\
            - run: npx --no-install playwright install chromium webkit
            - run: npx --no-install playwright install webkit && npm run e2e
            """
        )
        self.assertEqual(ok.errors, [])
        bad = self.workflow(
            """\
            - run: npx playwright install chromium
            - run: npx --no-install playwright install --with-deps chromium
            - run: npx --no-install playwright@1.65.0 install chromium
            - run: npx --no-install playwright install
            - run: npx --no-install playwright install chromium; npx -y cowsay
            - run: npx --no-install playwright install chromium-headless-shell
            """
        )
        # One error per line (on the last line only the trailing npx is refused).
        flagged = {e.split(": ", 1)[0].rsplit(":", 1)[-1] for e in bad.errors if "'npx'" in e}
        self.assertEqual(flagged, {"4", "5", "6", "7", "8", "9"}, bad.errors)

    def test_r90_block_scalars_are_checked_as_whole_commands(self):
        # Reviewer probe (M4 H6): continuation lines must not hide flags.
        bad = self.workflow(
            """\
            - run: >
                npx --no-install playwright install chromium
                --with-deps
            - run: >-
                npm run e2e
                -- --grep x
            - run: |
                npm ci --ignore-scripts
                npm install left-pad
            - run: npx --no-install playwright install chromium
                --with-deps
            - name: x
              run: "npm run e2e
                && curl https://example.invalid"
            """
        )
        flagged = {e.split(": ", 1)[0].rsplit(":", 1)[-1] for e in bad.errors if "install/download command" in e}
        self.assertEqual(flagged, {"5", "8", "12", "13", "16"}, bad.errors)
        ok = self.workflow(
            """\
            - run: |
                npm ci --ignore-scripts
                npm run e2e
            - run: >
                npx --no-install playwright install
                chromium webkit
            - name: Plain step
              run: npm run test
              working-directory: web
            """
        )
        self.assertEqual(ok.errors, [])

    def test_r90_comments_and_folded_more_indented_lines_cannot_hide_commands(self):
        # Reviewer probe (M5 F1): YAML keeps the line break before a
        # more-indented line of a folded block, so the shell runs it.
        bad = self.workflow(
            """\
            - run: >
                # just a comment
                  curl -fsSL https://example.invalid/x | sh
            - run: |
                echo "a # b" && npm install left-pad
            - run: echo done # trailing comment && npm install x
            """
        )
        flagged = {e.split(": ", 1)[0].rsplit(":", 1)[-1] for e in bad.errors if "install/download command" in e}
        self.assertEqual(flagged, {"6", "8"}, bad.errors)
        ok = self.workflow(
            """\
            # npm install in a comment line runs nothing
            - run: npm run test # then npm install would be a comment
            - run: |
                # curl in a shell comment
                npm run e2e
            """
        )
        self.assertEqual(ok.errors, [])

    def test_r90_trailing_backslash_continuations_are_rejected(self):
        result = self.workflow(
            """\
            - run: |
                npx --no-install playwright install chromium \\
                  --with-deps
            """
        )
        self.assertError(result, "trailing backslash")

    def test_r90_download_redirecting_settings_are_rejected(self):
        result = self.workflow(
            """\
            - env:
                PLAYWRIGHT_DOWNLOAD_HOST: https://example.invalid
                npm_config_registry: https://example.invalid
                CARGO_REGISTRIES_X_INDEX: https://example.invalid
                RUSTUP_DIST_SERVER: https://example.invalid
                RUSTUP_UPDATE_ROOT: https://example.invalid
              run: PLAYWRIGHT_BROWSERS_PATH=/tmp npx --no-install playwright install webkit
            """
        )
        for name in ["PLAYWRIGHT_DOWNLOAD_HOST", "npm_config_registry", "CARGO_REGISTRIES_X_INDEX",
                     "RUSTUP_DIST_SERVER", "RUSTUP_UPDATE_ROOT", "PLAYWRIGHT_BROWSERS_PATH"]:
            self.assertError(result, f"{name} can redirect")

    def test_r90_containers_escapes_and_explicit_keys_are_rejected(self):
        result = self.workflow(
            """\
            - run: echo hi
            container: node:24
            services:
              db:
                image: postgres
            - "\\x75ses": evil/action@main
            - ? uses
              : evil/action@main
            """
        )
        self.assertError(result, "container images", "escape sequences", "explicit '? key'")

    def test_r90_young_tool_and_node_are_reported(self):
        result = self.evaluate(self.clean_files(), young={"cargo-deny", "node"})
        young = {f.name for f in result.findings if f.published == YOUNG}
        self.assertEqual(young, {"cargo-deny", "node"})


class Lockfiles(RepoCase):
    def test_r90_sourceless_lock_entries_must_be_workspace_members(self):
        files = self.clean_files()
        files["Cargo.lock"] = CLEAN_LOCK + textwrap.dedent(
            """
            [[package]]
            name = "serde_json"
            version = "1.0.999"

            [[package]]
            name = "a"
            version = "0.2.0"
            """
        )
        result = self.evaluate(files)
        self.assertError(result, "serde_json 1.0.999 has no source", "a 0.2.0 has no source")
        self.assertEqual(len(result.errors), 2, result.errors)

    def test_r90_reviewer_r1_lock_without_members(self):
        result = self.evaluate(
            {
                "Cargo.toml": "[workspace]\n",
                "Cargo.lock": 'version = 4\n\n[[package]]\nname = "serde"\nversion = "1.0.999"\n\n'
                '[[package]]\nname = "app"\nversion = "0.1.0"\n',
            }
        )
        self.assertError(result, "serde 1.0.999 has no source", "app 0.1.0 has no source")

    def test_r90_git_sources_and_missing_lock_are_rejected(self):
        files = self.clean_files()
        files["Cargo.lock"] = CLEAN_LOCK + (
            '\n[[package]]\nname = "x"\nversion = "1.0.0"\nsource = "git+https://example.invalid/x#abc"\n'
        )
        self.assertError(self.evaluate(files), "only crates.io is allowed")
        files = self.clean_files()
        del files["Cargo.lock"]
        self.assertError(self.evaluate(files), "workspace without Cargo.lock")

    def test_r90_npm_resolved_must_match_name_version_and_registry(self):
        cases = {
            "other version": npm_entry("typescript", "7.0.2", resolved="https://registry.npmjs.org/typescript/-/typescript-6.0.0.tgz"),
            "other package": npm_entry("typescript", "7.0.2", resolved="https://registry.npmjs.org/evil/-/evil-7.0.2.tgz"),
            "other host": npm_entry("typescript", "7.0.2", resolved="https://registry.example.invalid/typescript/-/typescript-7.0.2.tgz"),
            "git": npm_entry("typescript", "7.0.2", resolved="git+ssh://git@example.invalid/x.git#abc"),
            "missing": {"version": "7.0.2", "integrity": "sha512-AAAA"},
            "integrity": npm_entry("typescript", "7.0.2", integrity="sha1-AAAA"),
        }
        for label, entry in cases.items():
            with self.subTest(label):
                files = self.clean_files()
                files["web/package-lock.json"] = package_lock({"node_modules/typescript": entry})
                result = self.evaluate(files)
                self.assertTrue(result.errors, label)
                self.assertEqual([f for f in result.findings if f.kind == "npm"], [])

    def test_r90_scoped_npm_packages_are_accepted(self):
        files = self.clean_files()
        files["web/package.json"] = '{"devDependencies": {"@typescript/typescript-linux-x64": "7.0.2"}}'
        files["web/package-lock.json"] = package_lock(
            {"node_modules/@typescript/typescript-linux-x64": npm_entry("@typescript/typescript-linux-x64", "7.0.2")}
        )
        result = self.evaluate(files)
        self.assertEqual(result.errors, [])

    def test_r90_unsupported_manifests_fail_closed(self):
        files = self.clean_files()
        for name in ["requirements.txt", "web2/yarn.lock", "x/go.mod", "pnpm-lock.yaml", ".nvmrc"]:
            files[name] = "x\n"
        files["other/package.json"] = "{}"
        result = self.evaluate(files)
        for name in ["requirements.txt", "yarn.lock", "go.mod", "pnpm-lock.yaml", ".nvmrc",
                     "package.json without package-lock.json"]:
            self.assertError(result, name)

    def test_r90_unparseable_dockerfiles_and_compose_files_fail_closed(self):
        files = self.clean_files()
        files["docker/Dockerfile.server"] = "x\n"
        files["deploy/docker-compose.example.yml"] = "x\n"
        result = self.evaluate(files)
        self.assertError(result, "Dockerfile.server: no FROM instruction", "docker-compose.example.yml: no top-level services")


class ActionDates(unittest.TestCase):
    """action_published: tag must match the SHA; age = later of release and commit."""

    def run_with(self, responses: dict[str, object]):
        saved = dep_age.github_api
        dep_age._action_cache.clear()

        def fake(path: str):
            for prefix, value in responses.items():
                if path.startswith(prefix):
                    if isinstance(value, Exception):
                        raise value
                    return value
            raise dep_age.LookupFailed(f"unexpected {path}")

        dep_age.github_api = fake
        try:
            return dep_age.action_published("o/r", SHA, "v1.2.3")
        finally:
            dep_age.github_api = saved

    def responses(self, **override):
        base = {
            "/repos/o/r/git/ref/tags/": {"object": {"type": "commit", "sha": SHA}},
            "/repos/o/r/commits/": {"commit": {"committer": {"date": "2026-01-01T00:00:00Z"}}},
            "/repos/o/r/releases/tags/": {"published_at": "2026-02-01T00:00:00Z"},
        }
        base.update(override)
        return base

    def test_r90_release_date_wins_when_later(self):
        self.assertEqual(self.run_with(self.responses()).isoformat(), "2026-02-01T00:00:00+00:00")

    def test_r90_commit_date_wins_when_later(self):
        r = self.responses(**{"/repos/o/r/commits/": {"commit": {"committer": {"date": "2026-11-16T00:00:00Z"}}}})
        self.assertEqual(self.run_with(r).isoformat(), "2026-11-16T00:00:00+00:00")

    def test_r90_without_release_the_commit_date_is_used(self):
        r = self.responses(**{"/repos/o/r/releases/tags/": dep_age.LookupFailed("HTTP 404", 404)})
        self.assertEqual(self.run_with(r).isoformat(), "2026-01-01T00:00:00+00:00")

    def test_r90_release_lookup_errors_other_than_404_fail_closed(self):
        for status in (403, 429, 500, None):
            r = self.responses(**{"/repos/o/r/releases/tags/": dep_age.LookupFailed(f"HTTP {status}", status)})
            with self.subTest(status=status), self.assertRaises(dep_age.LookupFailed):
                self.run_with(r)

    def test_r90_tag_pointing_elsewhere_fails(self):
        r = self.responses(**{"/repos/o/r/git/ref/tags/": {"object": {"type": "commit", "sha": "0" * 40}}})
        with self.assertRaises(dep_age.LookupFailed):
            self.run_with(r)

    def test_r90_ambiguous_tag_fails(self):
        r = self.responses(**{"/repos/o/r/git/ref/tags/": [{"ref": "refs/tags/v1.2.30"}]})
        with self.assertRaises(dep_age.LookupFailed):
            self.run_with(r)


class ReleaseAssetDates(unittest.TestCase):
    """release_asset_published: GitHub's digest must be the pinned checksum;
    the age is the asset's upload (a re-upload counts)."""

    def run_with(self, release):
        saved, saved_cache = dep_age.github_api, dep_age.cache_enabled
        dep_age.cache_enabled = False
        dep_age.github_api = lambda path: release if path == "/repos/o/r/releases/tags/v1" else {}
        try:
            return dep_age.release_asset_published("o/r", "v1", "a.tar.bz2", DIGEST)
        finally:
            dep_age.github_api, dep_age.cache_enabled = saved, saved_cache

    def asset(self, **override):
        base = {"name": "a.tar.bz2", "digest": f"sha256:{DIGEST}",
                "created_at": "2026-10-26T09:08:26Z", "updated_at": "2026-10-26T09:08:34Z"}
        base.update(override)
        return {"published_at": "2023-11-21T07:43:51Z", "assets": [{"name": "other"}, base]}

    def test_r90_upload_time_of_the_asset_is_its_date(self):
        self.assertEqual(self.run_with(self.asset()).isoformat(), "2026-10-26T09:08:34+00:00")
        reuploaded = self.asset(updated_at="2026-11-20T00:00:00Z")
        self.assertEqual(self.run_with(reuploaded).isoformat(), "2026-11-20T00:00:00+00:00")

    def test_r90_digest_must_match_and_be_known(self):
        for release in [self.asset(digest=f"sha256:{DIGEST2}"), self.asset(digest=None),
                        {"assets": [{"name": "other"}]}, {}]:
            with self.subTest(release), self.assertRaises(dep_age.LookupFailed):
                self.run_with(release)


DIGEST = "a" * 64
DIGEST2 = "b" * 64

CLEAN_DOCKERFILE = f"""\
# comment
FROM rust:1.99.0-slim-trixie@sha256:{DIGEST} AS build
RUN echo 'deb https://snapshot.debian.org/archive/debian/20261101T000000Z trixie main' > /etc/apt/sources.list \\
 && rm -f /etc/apt/sources.list.d/debian.sources \\
 && apt-get update \\
 && test "$(apt-get install -s --no-install-recommends musl=1.2.5-3.1~deb13u1 musl-dev=1.2.5-3.1~deb13u1 | grep -c '^Inst ')" = 2 \\
 && apt-get install -y --no-install-recommends musl=1.2.5-3.1~deb13u1 musl-dev=1.2.5-3.1~deb13u1 \\
 && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl
COPY Cargo.toml Cargo.lock ./
RUN cargo build --release --locked --target x86_64-unknown-linux-musl -p app
FROM gcr.io/distroless/static-debian13:nonroot@sha256:{DIGEST2}
COPY --from=build /out/app /app
USER 65532:65532
"""


class Dockerfiles(RepoCase):
    def dockerfile(self, text: str, young: set[str] | None = None):
        files = self.clean_files()
        files["docker/Dockerfile.server"] = text
        return self.evaluate(files, young)

    def test_r90_pinned_images_and_debian_packages_are_dated(self):
        result = self.dockerfile(CLEAN_DOCKERFILE)
        self.assertEqual(result.errors, [])
        images = sorted(f.name for f in result.findings if f.kind == "image")
        self.assertEqual(images, ["gcr.io/distroless/static-debian13:nonroot", "rust:1.99.0-slim-trixie"])
        debian = sorted({(f.name, f.version) for f in result.findings if f.kind == "debian"})
        self.assertEqual(debian, [("musl", "1.2.5-3.1~deb13u1"), ("musl-dev", "1.2.5-3.1~deb13u1")])
        snapshots = [(f.name, f.version, f.published) for f in result.findings if f.kind == "snapshot"]
        self.assertEqual(
            snapshots,
            [("snapshot.debian.org/debian", "20261101T000000Z", dt.datetime(2026, 11, 1, tzinfo=dt.timezone.utc))],
        )

    def test_r90_young_debian_snapshot_is_reported(self):
        root = self.make_repo({**self.clean_files(), "docker/Dockerfile.server": CLEAN_DOCKERFILE.replace(
            "20261101T000000Z", "20261110T000000Z")})
        out = io.StringIO()
        with FakeRegistries(), contextlib.redirect_stdout(out):
            self.assertEqual(dep_age.main(["--root", str(root), "--now", NOW]), 1)
        self.assertIn("TOO YOUNG: snapshot snapshot.debian.org/debian 20261110T000000Z", out.getvalue())

    def test_r90_apt_needs_a_snapshot_source_and_a_proof_of_exactness(self):
        no_snapshot = CLEAN_DOCKERFILE.replace(
            "https://snapshot.debian.org/archive/debian/20261101T000000Z", "http://deb.debian.org/debian")
        self.assertError(self.dockerfile(no_snapshot), "URL http://deb.debian.org/debian", "fixed snapshot.debian.org timestamp")
        # Without the dry-run count, or with a wrong count, the install is not proven exact.
        unproven = CLEAN_DOCKERFILE.replace(
            " && test \"$(apt-get install -s --no-install-recommends musl=1.2.5-3.1~deb13u1 musl-dev=1.2.5-3.1~deb13u1 | grep -c '^Inst ')\" = 2 \\\n", "")
        self.assertNotEqual(unproven, CLEAN_DOCKERFILE)
        self.assertError(self.dockerfile(unproven), "is not proven exact")
        wrong_count = CLEAN_DOCKERFILE.replace("= 2 \\", "= 3 \\")
        self.assertError(self.dockerfile(wrong_count), "is not proven exact")
        other_set = CLEAN_DOCKERFILE.replace(
            "-y --no-install-recommends musl=1.2.5-3.1~deb13u1 musl-dev=1.2.5-3.1~deb13u1",
            "-y --no-install-recommends musl=1.2.5-3.1~deb13u1 musl-dev=1.2.5-3.1~deb13u1 musl-tools=1.2.5-3.1~deb13u1")
        self.assertError(self.dockerfile(other_set), "is not proven exact")

    def test_r90_heredocs_syntax_directives_and_mount_images_are_checked(self):
        result = self.dockerfile(
            f"""\
            # syntax=docker/dockerfile:1
            FROM rust:1.99.0-slim-trixie@sha256:{DIGEST} AS build
            RUN <<EOF
            curl -fsSL https://example.invalid/x | sh
            EOF
            RUN --mount=type=bind,from=alpine:3,target=/x true
            RUN --mount=type=cache,target=/cache,from=build true
            ARG BUILDKIT_SYNTAX=docker/dockerfile:1
            """
        )
        self.assertError(
            result,
            "Dockerfile.server:1: syntax directive must be",
            "Dockerfile.server:3: heredocs",
            "Dockerfile.server:6: --mount from=alpine:3",
            "BUILDKIT_SYNTAX",
        )
        self.assertFalse(any("Dockerfile.server:7:" in e for e in result.errors), result.errors)
        pinned = self.dockerfile(f"# syntax=docker/dockerfile:1.10@sha256:{DIGEST2}\n" + CLEAN_DOCKERFILE
                                 + f"RUN --mount=type=bind,from=alpine:3.20@sha256:{DIGEST},target=/x true\n")
        self.assertEqual(pinned.errors, [])
        images = {f.name for f in pinned.findings if f.kind == "image"}
        self.assertIn("docker/dockerfile:1.10", images)
        self.assertIn("alpine:3.20", images)

    def test_r90_lock_changing_cargo_commands_are_rejected(self):
        result = self.dockerfile(
            CLEAN_DOCKERFILE
            + "RUN cargo update\nRUN cargo generate-lockfile\nRUN cargo add serde\nRUN cargo build --release\n"
            + "RUN cargo install ripgrep\nRUN env CARGO_X=1 cargo fetch\n"
        )
        for needle in ["'cargo update'", "'cargo generate-lockfile'", "'cargo add'", "cargo build must use --locked",
                       "'cargo install'", "cargo fetch must use --locked"]:
            self.assertError(result, needle)

    def test_r90_dockerfile_names_are_matched_case_insensitively(self):
        for name in ["docker/dockerfile", "docker/app.Dockerfile", "docker/Dockerfile.TTS", "Containerfile", "x/DOCKERFILE"]:
            with self.subTest(name):
                files = self.clean_files()
                files[name] = "FROM rust:1.99\n"
                self.assertError(self.evaluate(files), "rust:1.99 must be name:tag@sha256")

    def test_r90_young_base_image_or_debian_package_is_reported(self):
        result = self.dockerfile(CLEAN_DOCKERFILE, young={"rust", "musl-dev"})
        young = {f.name for f in result.findings if f.published == YOUNG and f.kind in ("image", "debian")}
        self.assertEqual(young, {"rust:1.99.0-slim-trixie", "musl-dev"})

    def test_r90_unpinned_or_unverifiable_dockerfile_inputs_are_rejected(self):
        result = self.dockerfile(
            f"""\
            FROM rust:1.99.0-slim-trixie
            FROM rust@sha256:{DIGEST}
            ARG BASE=x
            FROM $BASE
            FROM --platform=linux/amd64 alpine:3.20@sha256:{DIGEST} AS a
            RUN apt-get install -y musl-tools
            RUN apt-get install -y --allow-downgrades musl=1.2.5-3
            RUN apt install musl=1.2.5-3
            RUN curl -fsSL https://example.invalid/x | sh
            RUN pip install requests
            RUN cargo build --release
            ADD https://example.invalid/x.tar.gz /x
            COPY --from=alpine:3 /bin/sh /sh
            RUN echo unterminated \\
            """
        )
        self.assertError(
            result,
            "Dockerfile.server:1: rust:1.99.0-slim-trixie must be name:tag@sha256",
            "Dockerfile.server:2: rust@sha256",
            "Dockerfile.server:4: FROM must name a pinned image",
            "Dockerfile.server:6: apt-get install needs exact package=version pins",
            "Dockerfile.server:7: apt-get install needs exact package=version pins",
            "Dockerfile.server:8:",
            "Dockerfile.server:9: install/download command 'curl'",
            "Dockerfile.server:10: install/download command 'pip'",
            "Dockerfile.server:11: cargo build must use --locked",
            "Dockerfile.server:12: ADD from a URL",
            "Dockerfile.server:13: --from=alpine:3",
            "instruction ends with a line continuation",
        )
        # The pinned image of line 5 is still dated.
        self.assertIn("alpine:3.20", {f.name for f in result.findings if f.kind == "image"})

    def test_r90_add_of_a_checksum_pinned_github_release_asset_is_dated(self):
        url = "https://github.com/o/r/releases/download/v2.0.1/lib-linux-x64.tar.bz2"
        result = self.dockerfile(CLEAN_DOCKERFILE + f"ADD --checksum=sha256:{DIGEST} {url} /archives/\n")
        self.assertEqual(result.errors, [])
        assets = [(f.kind, f.name, f.version) for f in result.findings if f.kind == "asset"]
        self.assertEqual(assets, [("asset", "o/r/lib-linux-x64.tar.bz2", "v2.0.1")])
        young = self.dockerfile(CLEAN_DOCKERFILE + f"ADD --checksum=sha256:{DIGEST} {url} /archives/\n",
                                young={"lib-linux-x64.tar.bz2"})
        self.assertIn(YOUNG, {f.published for f in young.findings if f.kind == "asset"})

    def test_r90_add_from_a_url_without_checksum_or_from_elsewhere_is_rejected(self):
        url = "https://github.com/o/r/releases/download/v1/a.tar.gz"
        for line in [
            f"ADD {url} /x",
            f"ADD --checksum=sha256:{DIGEST[:10]} {url} /x",
            f"ADD --checksum=sha256:{DIGEST} https://example.invalid/a.tar.gz /x",
            f"ADD --checksum=sha256:{DIGEST} https://github.com/o/r/archive/refs/heads/main.tar.gz /x",
            f"ADD --checksum=sha256:{DIGEST} --chmod=755 {url} /x",
            f"ADD --checksum=sha256:{DIGEST} {url} {url} /x",
            f"ADD --checksum=sha256:{DIGEST} git@github.com:o/r.git /x",
        ]:
            with self.subTest(line):
                self.assertError(self.dockerfile(CLEAN_DOCKERFILE + line + "\n"), "ADD from a URL must be")

    def test_r90_redirecting_settings_in_dockerfiles_are_rejected(self):
        result = self.dockerfile(CLEAN_DOCKERFILE + "ENV RUSTUP_DIST_SERVER=https://example.invalid\n")
        self.assertError(result, "RUSTUP_DIST_SERVER can redirect")


class ComposeFiles(RepoCase):
    def compose(self, text: str):
        files = self.clean_files()
        files["deploy/docker-compose.example.yml"] = text
        return self.evaluate(files)

    def test_r90_compose_images_are_dated_and_own_images_exempt(self):
        result = self.compose(
            f"""\
            # Set `image:` to the released tag and digest.
            services:
              app:
                image: ghcr.io/kadledav/vocabulary-training:X.Y.Z@sha256:DIGEST-FROM-THE-RELEASE-NOTES
              db:
                image: "postgres:17.2@sha256:{DIGEST}"
            """
        )
        self.assertEqual(result.errors, [])
        self.assertEqual([f.name for f in result.findings if f.kind == "image"], ["postgres:17.2"])

    def test_r90_unpinned_compose_images_and_builds_are_rejected(self):
        result = self.compose(
            """\
            services:
              a:
                image: postgres:17
              b:
                image: ${IMAGE}
              c:
                build: .
              d: { image: redis }
            """
        )
        self.assertError(
            result,
            "compose.example.yml:3: postgres:17 must be",
            "compose.example.yml:5:",
            "compose.example.yml:7: build:",
            "compose.example.yml:8: unverifiable image line",
        )


class ComposeDiscovery(RepoCase):
    def test_r90_any_yaml_with_top_level_services_is_a_compose_file(self):
        files = self.clean_files()
        files["stack/app.yaml"] = "services:\n  a:\n    image: postgres:17\n"
        files["stack/other.yml"] = "x:\n  services:\n    image: postgres:17\n"
        result = self.evaluate(files)
        self.assertError(result, "app.yaml:3: postgres:17 must be")
        self.assertFalse(any("other.yml" in e for e in result.errors), result.errors)

    def test_r90_compose_include_and_extends_are_rejected(self):
        files = self.clean_files()
        files["compose.yml"] = (
            f"include:\n  - other.yml\nservices:\n  a:\n    extends:\n      file: base.yml\n      service: b\n"
            f"    image: postgres:17.2@sha256:{DIGEST}\n"
        )
        result = self.evaluate(files)
        self.assertError(result, "compose.yml:1: include/extends", "compose.yml:5: include/extends")


class WorkflowImages(RepoCase):
    def workflow(self, body: str):
        files = self.clean_files()
        files["docker/Dockerfile.server"] = CLEAN_DOCKERFILE
        files[".github/workflows/x.yml"] = "jobs:\n  b:\n    steps:\n" + textwrap.indent(textwrap.dedent(body), "      ")
        return self.evaluate(files)

    def test_r90_buildkit_and_sbom_generator_images_are_dated(self):
        result = self.workflow(
            f"""\
            # A comment that mentions Docker and curl runs nothing.
            - uses: docker/setup-buildx-action@{SHA} # v4.4.1
              with:
                driver-opts: image=moby/buildkit:v0.34.1@sha256:{DIGEST}
            - uses: docker/build-push-action@{SHA} # v7.4.0
              with:
                file: docker/Dockerfile.server
                sbom: generator=docker/buildkit-syft-scanner:1.12.1@sha256:{DIGEST2}
            - run: python3 tools/image-smoke.py vocab-server:ci
            """
        )
        self.assertEqual(result.errors, [])
        images = sorted({f.name for f in result.findings if f.kind == "image"} - {
            "rust:1.99.0-slim-trixie", "gcr.io/distroless/static-debian13:nonroot"})
        self.assertEqual(images, ["docker/buildkit-syft-scanner:1.12.1", "moby/buildkit:v0.34.1"])

    def test_r90_unpinned_buildkit_or_generator_images_are_rejected(self):
        result = self.workflow(
            """\
            - with:
                driver-opts: image=moby/buildkit:latest
                sbom: generator=docker/buildkit-syft-scanner:stable-1
            - run: docker run --rm alpine true
            """
        )
        self.assertError(result, "x.yml:5: image=/generator=", "x.yml:6: image=/generator=", "x.yml:7: install/download command 'docker'")


class StepInputs(RepoCase):
    """`with:` inputs that would make an action fetch something floating."""

    def workflow(self, body: str, extra: dict[str, str] | None = None):
        files = self.clean_files()
        files["docker/Dockerfile.server"] = CLEAN_DOCKERFILE
        files.update(extra or {})
        files[".github/workflows/x.yml"] = (
            "jobs:\n  b:\n    runs-on: ubuntu-24.04\n    steps:\n" + textwrap.indent(textwrap.dedent(body), "      ")
        )
        return self.evaluate(files)

    def test_r90_setup_buildx_needs_a_pinned_buildkit_image(self):
        result = self.workflow(
            f"""\
            - uses: docker/setup-buildx-action@{SHA} # v4.4.1
            - name: other
              uses: docker/setup-buildx-action@{SHA} # v4.4.1
              with:
                driver-opts: network=host
            """
        )
        self.assertEqual(sum("setup-buildx-action needs" in e for e in result.errors), 2, result.errors)

    def test_r90_build_push_inputs_that_pull_floating_things_are_rejected(self):
        result = self.workflow(
            f"""\
            - uses: docker/build-push-action@{SHA} # v7.4.0
              with:
                context: https://github.com/o/r.git#main
                file: docker/Dockerfile.server
                sbom: true
                cache-from: |
                  type=gha
                  type=registry,ref=ghcr.io/o/cache:main
                  ghcr.io/o/cache:latest
                build-contexts: base=docker-image://alpine:3
                build-args: |
                  VERSION=1
                  BUILDKIT_SYNTAX=docker/dockerfile:1
            """
        )
        self.assertError(
            result, "x.yml:7: context", "x.yml:9: sbom must be", "pulls a registry cache without a digest",
            "x.yml:14: build context", "x.yml:15: build-arg 'BUILDKIT_SYNTAX",
        )
        self.assertEqual(sum("pulls a registry cache" in e for e in result.errors), 2, result.errors)
        self.assertFalse(any("type=gha" in e for e in result.errors))

    def test_r90_pinned_inputs_pass(self):
        result = self.workflow(
            f"""\
            - uses: docker/setup-buildx-action@{SHA} # v4.4.1
              with:
                driver-opts: image=moby/buildkit:v0.34.1@sha256:{DIGEST}
            - uses: docker/build-push-action@{SHA} # v7.4.0
              with:
                context: .
                file: docker/Dockerfile.server
                sbom: false
                cache-from: type=registry,ref=ghcr.io/o/cache:main@sha256:{DIGEST}
                build-args: VERSION=1
            """
        )
        self.assertEqual(result.errors, [])

    def test_r90_runs_on_must_be_a_versioned_label(self):
        for label, ok in [("ubuntu-24.04", True), ("ubuntu-24.04-arm", True), ("'macos-15'", True),
                          ("ubuntu-latest", False), ("macos-latest", False), ("[self-hosted, linux]", False),
                          ("${{ matrix.os }}", False), ("", False)]:
            files = self.clean_files()
            files[".github/workflows/x.yml"] = f"jobs:\n  b:\n    runs-on: {label}\n    steps: []\n"
            result = self.evaluate(files)
            with self.subTest(label=label):
                self.assertEqual(not any("runs-on" in e for e in result.errors), ok, result.errors)

    def test_r90_runner_matrix_values_are_checked_too(self):
        files = self.clean_files()
        files[".github/workflows/x.yml"] = textwrap.dedent(
            """\
            jobs:
              b:
                strategy:
                  matrix:
                    include:
                      - { arch: amd64, runner: ubuntu-24.04 }
                      - arch: arm64
                        runner: ubuntu-latest
                runs-on: ${{ matrix.runner }}
                steps: []
            """
        )
        result = self.evaluate(files)
        self.assertError(result, "x.yml:8: runner 'ubuntu-latest'")
        self.assertEqual(sum("runner" in e for e in result.errors), 1, result.errors)


class ReferencedDockerfiles(RepoCase):
    def test_r90_dockerfiles_named_by_workflows_are_scanned_whatever_their_name(self):
        files = self.clean_files()
        files["build/server.image"] = "FROM alpine:3\n"
        files[".github/workflows/x.yml"] = textwrap.dedent(
            f"""\
            jobs:
              b:
                runs-on: ubuntu-24.04
                steps:
                  - uses: docker/build-push-action@{SHA} # v7.4.0
                    with:
                      file: build/server.image
                      sbom: false
                  - uses: docker/build-push-action@{SHA} # v7.4.0
                    with:
                      file: build/missing.image
            """
        )
        result = self.evaluate(files)
        self.assertError(result, "server.image:1: alpine:3 must be name:tag@sha256", "x.yml:11: file 'build/missing.image'")

    def test_r90_dockerfiles_named_by_compose_files_are_scanned(self):
        files = self.clean_files()
        files["deploy/compose.yml"] = "services:\n  a:\n    build:\n      dockerfile: app.build\n"
        files["deploy/app.build"] = "FROM alpine:3\n"
        result = self.evaluate(files)
        self.assertError(result, "app.build:1: alpine:3 must be", "compose.yml:3: build:")

    def test_r90_skip_directories_apply_only_at_the_root(self):
        files = self.clean_files()
        files["target/Dockerfile"] = "FROM alpine:3\n"  # build output at the root: skipped
        files["sub/target/Dockerfile"] = "FROM alpine:3\n"
        files["sub/.idea/Dockerfile"] = "FROM alpine:3\n"
        files["web/node_modules/x/Dockerfile"] = "FROM alpine:3\n"  # installed by the checked lock
        files["other/node_modules/x/Dockerfile"] = "FROM alpine:3\n"  # no lock file next to it
        files[".claude/worktrees/a/Dockerfile"] = "FROM alpine:3\n"  # agent worktree at the root: skipped
        files["sub/.claude/Dockerfile"] = "FROM alpine:3\n"
        result = self.evaluate(files)
        self.assertError(result, "sub/target/Dockerfile", "sub/.idea/Dockerfile", "other/node_modules/x/Dockerfile",
                         "sub/.claude/Dockerfile")
        self.assertFalse(any("/.claude/worktrees/" in e for e in result.errors), result.errors)
        self.assertFalse(any("/web/node_modules/" in e or e.split(": ")[0].endswith("/target/Dockerfile:1")
                             and "/sub/" not in e for e in result.errors), result.errors)


class ComposeVariants(RepoCase):
    def test_r90_json_and_example_compose_files_and_quoted_keys(self):
        files = self.clean_files()
        files["deploy/stack.json"] = '{"services": {"a": {"image": "nginx:latest"}, "b": {"build": "."}}}'
        files["deploy/own.json"] = '{"services": {"a": {"image": "ghcr.io/kadledav/vocabulary-training:X"}}}'
        files["deploy/compose.yml.example"] = 'services:\n  a:\n    "image": "redis:7"\n'
        files["deploy/other.yaml.example"] = "services:\n  a:\n    'image': redis:7\n"
        files["data/not-compose.json"] = '{"name": "x"}'
        result = self.evaluate(files)
        self.assertError(result, "stack.json: $.services.a.image: nginx:latest must be", "stack.json: $.services.b.build",
                         "compose.yml.example:3: redis:7 must be", "other.yaml.example:3: redis:7 must be")
        self.assertFalse(any("own.json" in e or "not-compose" in e for e in result.errors), result.errors)


class RedirectingConfig(RepoCase):
    def test_r90_cargo_config_files_are_rejected_anywhere(self):
        files = self.clean_files()
        files[".cargo/config.toml"] = "[source.crates-io]\nreplace-with = 'x'\n"
        files["crates/a/.cargo/config"] = "[net]\n"
        result = self.evaluate(files)
        self.assertError(result, ".cargo/config.toml: cargo configuration", "crates/a/.cargo/config: cargo configuration")

    def test_r90_npmrc_only_allow_listed_keys(self):
        files = self.clean_files()
        files["web/.npmrc"] = "# comment\nsave-exact=true\nignore-scripts=true\nregistry=https://example.invalid/\n@x:registry=y\n"
        result = self.evaluate(files)
        self.assertError(result, ".npmrc:4: .npmrc setting 'registry'", ".npmrc:5: .npmrc setting '@x:registry'")
        self.assertEqual(sum(".npmrc setting" in e for e in result.errors), 2)

    def test_r90_toolchain_and_source_overrides_and_cargo_config_flags(self):
        files = self.clean_files()
        files[".github/workflows/x.yml"] = textwrap.dedent(
            """\
            jobs:
              b:
                runs-on: ubuntu-24.04
                env:
                  RUSTUP_TOOLCHAIN: nightly
                  CARGO_SOURCE_CRATES_IO_REPLACE_WITH: mirror
                steps:
                  - run: cargo build --locked --config 'patch.crates-io.x.path="x"'
                  - run: cargo --config=net.offline=true test --locked
            """
        )
        result = self.evaluate(files)
        self.assertError(result, "x.yml:5: RUSTUP_TOOLCHAIN", "x.yml:6: CARGO_SOURCE_CRATES_IO_REPLACE_WITH",
                         "x.yml:8: cargo --config", "x.yml:9: cargo --config")

    def test_r90_dockerfile_rustup_target_from_a_variable(self):
        files = self.clean_files()
        files["docker/Dockerfile.server"] = CLEAN_DOCKERFILE.replace(
            "RUN rustup target add x86_64-unknown-linux-musl",
            'RUN target="$(cat /rust-target)" && rustup target add "$target"',
        )
        self.assertEqual(self.evaluate(files).errors, [])
        files["docker/Dockerfile.server"] = CLEAN_DOCKERFILE.replace(
            "RUN rustup target add x86_64-unknown-linux-musl", "RUN rustup target add $(curl -s x)"
        )
        self.assertError(self.evaluate(files), "install/download command 'rustup'")


class PackageLocks(RepoCase):
    def lock_errors(self, lock: dict, manifest: str = '{"devDependencies": {"typescript": "7.0.2"}}') -> list[str]:
        import json

        files = self.clean_files()
        files["web/package.json"] = manifest
        files["web/package-lock.json"] = json.dumps(lock)
        return self.evaluate(files).errors

    def entry(self):
        return npm_entry("typescript", "7.0.2")

    def test_r90_lockfile_version_3_without_legacy_tree(self):
        root = {"name": "w", "devDependencies": {"typescript": "7.0.2"}}
        errors = self.lock_errors({"lockfileVersion": 2, "packages": {"": root, "node_modules/typescript": self.entry()},
                                   "dependencies": {"typescript": {"version": "7.0.2"}}})
        self.assertTrue(any("lockfileVersion must be 3" in e for e in errors), errors)
        self.assertTrue(any("legacy `dependencies` tree" in e for e in errors), errors)
        errors = self.lock_errors({"lockfileVersion": 3})
        self.assertTrue(any("no `packages` map" in e for e in errors), errors)

    def test_r90_lock_must_match_package_json(self):
        root = {"name": "w", "devDependencies": {"typescript": "7.0.2"}}
        ok = {"lockfileVersion": 3, "packages": {"": root, "node_modules/typescript": self.entry()}}
        self.assertEqual(self.lock_errors(ok), [])
        errors = self.lock_errors(ok, '{"devDependencies": {"typescript": "7.0.3"}}')
        self.assertTrue(any("differ from package.json" in e for e in errors), errors)
        errors = self.lock_errors(ok, '{"devDependencies": {"typescript": "^7.0.2"}}')
        self.assertTrue(any("must be an exact version" in e for e in errors), errors)
        missing = {"lockfileVersion": 3, "packages": {"": root}}
        errors = self.lock_errors(missing)
        self.assertTrue(any("typescript@7.0.2 of package.json is not in `packages`" in e for e in errors), errors)


class Cache(unittest.TestCase):
    """Immutable lookups are cached on disk; failures are not."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        saved = (dep_age.CACHE_DIR, dep_age.cache_enabled, dep_age.fetch_json, dict(dep_age._cache))
        dep_age.CACHE_DIR, dep_age.cache_enabled = Path(tmp.name), True
        dep_age._cache.clear()

        def restore():
            dep_age.CACHE_DIR, dep_age.cache_enabled, dep_age.fetch_json = saved[:3]
            dep_age._cache.clear()
            dep_age._cache.update(saved[3])

        self.addCleanup(restore)
        self.urls: list[str] = []

    def fake(self, answer):
        def fetch_json(url, **kwargs):
            self.urls.append(url)
            if isinstance(answer, Exception):
                raise answer
            return answer

        dep_age.fetch_json = fetch_json

    def test_r90_crate_dates_are_cached_by_name_and_version(self):
        self.fake({"version": {"created_at": "2026-01-02T03:04:05Z"}})
        first = dep_age.crate_published("serde", "1.0.0")
        dep_age._cache.clear()  # a new run reads the file
        self.fake(dep_age.LookupFailed("must not be asked again"))
        self.assertEqual(dep_age.crate_published("serde", "1.0.0"), first)
        self.assertEqual(len(self.urls), 1)
        self.assertTrue((dep_age.CACHE_DIR / "crate.json").is_file())
        with self.assertRaises(dep_age.LookupFailed):
            dep_age.crate_published("serde", "1.0.1")  # another version is looked up

    def test_r90_failures_are_not_cached(self):
        self.fake(dep_age.LookupFailed("HTTP 503", 503))
        with self.assertRaises(dep_age.LookupFailed):
            dep_age.crate_published("x", "1.0.0")
        self.fake({"version": {"created_at": "2026-01-02T03:04:05Z"}})
        self.assertEqual(dep_age.crate_published("x", "1.0.0").year, 2026)
        self.assertEqual(len(self.urls), 2)

    def test_r90_action_lookups_are_never_cached(self):
        import inspect

        self.assertNotIn("cached(", inspect.getsource(dep_age.action_published))


class ImageDates(unittest.TestCase):
    """image_published / debian_published against faked registries."""

    def setUp(self):
        dep_age._image_cache.clear()
        self.saved = (dep_age.registry_get, dep_age.fetch_json)
        self.addCleanup(self.restore)

    def restore(self):
        dep_age.registry_get, dep_age.fetch_json = self.saved

    def serve(self, blobs: dict[str, object]):
        import hashlib
        import json

        store = {}
        for key, value in blobs.items():
            body = value if isinstance(value, bytes) else json.dumps(value).encode()
            store[key] = body

        def fake(registry, repo, path, accept):
            if path in store:
                return store[path]
            raise dep_age.LookupFailed(f"unexpected {path}")

        dep_age.registry_get = fake
        return lambda value: "sha256:" + hashlib.sha256(json.dumps(value).encode()).hexdigest()

    def test_r90_image_split_follows_docker_hub_rules(self):
        self.assertEqual(dep_age.split_image("rust"), ("registry-1.docker.io", "library/rust"))
        self.assertEqual(dep_age.split_image("moby/buildkit"), ("registry-1.docker.io", "moby/buildkit"))
        self.assertEqual(dep_age.split_image("gcr.io/distroless/static"), ("gcr.io", "distroless/static"))
        self.assertEqual(dep_age.split_image("localhost:5000/x"), ("localhost:5000", "x"))

    def test_r90_gcr_uses_the_upload_time(self):
        digest = "sha256:" + DIGEST
        listing = {"manifest": {digest: {"timeUploadedMs": "1761950220000", "timeCreatedMs": "0"}}}
        self.serve({"tags/list": listing})
        published = dep_age.image_published("gcr.io/distroless/static-debian13", digest)
        self.assertEqual(published, dt.datetime.fromtimestamp(1761950220, dt.timezone.utc))
        with self.assertRaises(dep_age.LookupFailed):
            dep_age.image_published("gcr.io/distroless/static-debian13", "sha256:" + DIGEST2)

    def test_r90_index_uses_the_newest_platform_config_and_skips_attestations(self):
        configs = [{"created": "2026-10-01T00:00:00Z"}, {"created": "2026-10-06T04:00:00Z"}]
        import hashlib
        import json

        def digest_of(value):
            return "sha256:" + hashlib.sha256(json.dumps(value).encode()).hexdigest()

        manifests = [{"config": {"digest": digest_of(c)}} for c in configs]
        attestation = {"config": {"digest": "sha256:" + "c" * 64}}
        index = {
            "manifests": [
                {"digest": digest_of(manifests[0]), "platform": {"os": "linux", "architecture": "amd64"}},
                {"digest": digest_of(manifests[1]), "platform": {"os": "linux", "architecture": "arm64"}},
                {"digest": digest_of(attestation), "platform": {"os": "unknown", "architecture": "unknown"}},
            ]
        }
        blobs = {f"manifests/{digest_of(index)}": index}
        for m, c in zip(manifests, configs):
            blobs[f"manifests/{digest_of(m)}"] = m
            blobs[f"blobs/{digest_of(c)}"] = c
        self.serve(blobs)
        published = dep_age.image_published("rust", digest_of(index))
        self.assertEqual(published.isoformat(), "2026-10-06T04:00:00+00:00")

    def test_r90_images_without_a_real_date_or_with_wrong_content_fail(self):
        import hashlib
        import json

        def digest_of(value):
            return "sha256:" + hashlib.sha256(json.dumps(value).encode()).hexdigest()

        config = {"created": "1970-01-01T00:00:00Z"}
        manifest = {"config": {"digest": digest_of(config)}}
        self.serve({f"manifests/{digest_of(manifest)}": manifest, f"blobs/{digest_of(config)}": config})
        with self.assertRaises(dep_age.LookupFailed):
            dep_age.image_published("x/reproducible", digest_of(manifest))
        # The registry answers with content that does not hash to the digest.
        self.serve({f"manifests/sha256:{DIGEST}": manifest})
        with self.assertRaises(dep_age.LookupFailed):
            dep_age.image_published("x/tampered", f"sha256:{DIGEST}")

    def test_r90_debian_date_is_the_first_amd64_appearance(self):
        answer = {
            "result": [
                {"architecture": "arm64", "hash": "h1"},
                {"architecture": "amd64", "hash": "h2"},
            ],
            "fileinfo": {
                "h1": [{"archive_name": "debian", "first_seen": "20250101T000000Z"}],
                "h2": [
                    {"archive_name": "debian", "first_seen": "20260510T204219Z"},
                    {"archive_name": "debian-ports", "first_seen": "20240101T000000Z"},
                ],
            },
        }
        dep_age.fetch_json = lambda url, **kw: answer
        self.assertEqual(
            dep_age.debian_published("musl-tools", "1.2.5-3.1~deb13u1").isoformat(), "2026-05-10T20:42:19+00:00"
        )
        dep_age.fetch_json = lambda url, **kw: {"result": [], "fileinfo": {}}
        with self.assertRaises(dep_age.LookupFailed):
            dep_age.debian_published("musl-tools", "9.9")

    def test_r90_debian_date_is_the_latest_over_the_built_architectures(self):
        def answer(arm64_seen):
            result = [{"architecture": "amd64", "hash": "h1"}]
            fileinfo = {"h1": [{"archive_name": "debian", "first_seen": "20260101T000000Z"}]}
            if arm64_seen:
                result.append({"architecture": "arm64", "hash": "h2"})
                fileinfo["h2"] = [{"archive_name": "debian", "first_seen": arm64_seen}]
            return {"result": result, "fileinfo": fileinfo}

        dep_age.fetch_json = lambda url, **kw: answer("20260301T000000Z")
        self.assertEqual(dep_age.debian_published("musl", "1").isoformat(), "2026-03-01T00:00:00+00:00")
        dep_age.fetch_json = lambda url, **kw: answer(None)
        with self.assertRaisesRegex(dep_age.LookupFailed, "arm64"):
            dep_age.debian_published("musl", "1")


if __name__ == "__main__":
    unittest.main()
