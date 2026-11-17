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


def package_lock(packages: dict) -> str:
    import json

    return json.dumps({"name": "w", "lockfileVersion": 3, "packages": {"": {"name": "w"}, **packages}})


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
            for name in ("crate_published", "npm_published", "rust_published", "node_published", "action_published")
        }
        dep_age.crate_published = lambda n, v: self._date(n, v)
        dep_age.npm_published = lambda n, v: self._date(n, v)
        dep_age.rust_published = lambda v: self._date("rust", v)
        dep_age.node_published = lambda v: self._date("node", v)
        dep_age.action_published = lambda repo, sha, tag: self._date(repo, sha, tag)
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
        files["web/package-lock.json"] = package_lock(
            {"node_modules/@typescript/typescript-linux-x64": npm_entry("@typescript/typescript-linux-x64", "7.0.2")}
        )
        result = self.evaluate(files)
        self.assertEqual(result.errors, [])

    def test_r90_unsupported_manifests_fail_closed(self):
        files = self.clean_files()
        for name in ["requirements.txt", "web2/yarn.lock", "x/go.mod", "docker/Dockerfile.server",
                     "deploy/docker-compose.example.yml", "pnpm-lock.yaml", ".nvmrc"]:
            files[name] = "x\n"
        files["other/package.json"] = "{}"
        result = self.evaluate(files)
        for name in ["requirements.txt", "yarn.lock", "go.mod", "Dockerfile.server", "docker-compose.example.yml",
                     "pnpm-lock.yaml", ".nvmrc", "package.json without package-lock.json"]:
            self.assertError(result, name)


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
        r = self.responses(**{"/repos/o/r/releases/tags/": dep_age.LookupFailed("404")})
        self.assertEqual(self.run_with(r).isoformat(), "2026-01-01T00:00:00+00:00")

    def test_r90_tag_pointing_elsewhere_fails(self):
        r = self.responses(**{"/repos/o/r/git/ref/tags/": {"object": {"type": "commit", "sha": "0" * 40}}})
        with self.assertRaises(dep_age.LookupFailed):
            self.run_with(r)

    def test_r90_ambiguous_tag_fails(self):
        r = self.responses(**{"/repos/o/r/git/ref/tags/": [{"ref": "refs/tags/v1.2.30"}]})
        with self.assertRaises(dep_age.LookupFailed):
            self.run_with(r)


if __name__ == "__main__":
    unittest.main()
