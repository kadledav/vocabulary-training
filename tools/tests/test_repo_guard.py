"""Self-tests for tools/repo-guard.py (R-24, R-84, R-91, R-92, R-95).

Each case builds a small synthetic git work tree. Secret-looking test inputs
are assembled from fragments so this file itself never matches the patterns.

Run: python3 -m unittest discover -s tools/tests -v
"""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parent.parent
REPO = TOOLS.parent
_spec = importlib.util.spec_from_file_location("repo_guard", TOOLS / "repo-guard.py")
assert _spec and _spec.loader
guard = importlib.util.module_from_spec(_spec)
sys.modules["repo_guard"] = guard
_spec.loader.exec_module(guard)

RENOVATE_OK = json.loads((REPO / "renovate.json").read_text())
SHELL_OK = (REPO / "web" / "index.html").read_text()
DOM_OK = (REPO / "web" / "js" / "dom.js").read_text()

TOKEN_KEY = "VOCAB_IMPORT" + "_TOKEN"
HASH_KEY = "VOCAB_PASSWORD" + "_HASH_B64"
ARGON_B64 = "JGFyZ29u" + "MmlkJHY9MTkkbT0xOTQ1Nix0PTIscD0xJGFiY2RlZmdoaWprbG1ub3A"


class GuardCase(unittest.TestCase):
    def problems(self, files: dict[str, str | bytes], renovate: object = RENOVATE_OK) -> list[str]:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)
        subprocess.run(["git", "init", "-q"], cwd=root, check=True)
        all_files: dict[str, str | bytes] = {"renovate.json": json.dumps(renovate)}
        all_files.update(files)
        for rel, content in all_files.items():
            path = root / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            if isinstance(content, bytes):
                path.write_bytes(content)
            else:
                path.write_text(content)
        return guard.check(root, guard.repo_files(root))

    def assertFlags(self, problems: list[str], *needles: str) -> None:
        for needle in needles:
            self.assertTrue(
                any(needle in p for p in problems),
                f"expected a problem containing {needle!r}, got:\n" + "\n".join(problems),
            )


class Clean(GuardCase):
    def test_r84_clean_shell_and_dom_helper_pass(self):
        problems = self.problems(
            {
                "web/index.html": SHELL_OK,
                "web/js/dom.js": DOM_OK,
                "web/js/ok.js": "setTimeout(() => go(), 10);\nsetInterval(function () {}, 5);\nel.textContent = t;\n",
                "crates/a/src/lib.rs": "pub fn x() {}\npub struct Y;\n",
                "crates/a/tests/fixtures/f.sql": "INSERT INTO source (id, slug, title) VALUES (1, 'test-x', 'T');\n",
                "testdata/packs/valid/p.json": '{"pack_id": "test-x--2026-11-17--1", "source": {"id": "test-x"}}',
            }
        )
        self.assertEqual(problems, [])


class Web(GuardCase):
    """Reviewer reproduction rg plus further spellings."""

    def test_r84_html_injection_bypasses_are_caught(self):
        js = "\n".join(
            [
                "el['inner' + 'HTML'] = packText;",
                "window.eval(packText);",
                "globalThis.eval(packText);",
                "Function(packText)();",
                "setTimeout(packText, 0);",
                "document.execCommand('insertHTML', false, packText);",
                "const f = new (Function)(packText);",
                'el["outerHTML"] = x;',
                "el[`inner${'HTML'}`] = x;",
                "const d = document; d.write(x);",
                "setInterval('tick()', 5);",
            ]
        )
        problems = self.problems({"web/js/a.js": js})
        for line in range(1, 12):
            self.assertFlags(problems, f"web/js/a.js:{line}:")

    def test_r84_every_file_type_under_web_is_scanned(self):
        problems = self.problems(
            {
                "web/js/b.jsx": "el.innerHTML = x;\n",
                "web/css/x.svg": '<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>',
                "web/js/c.ts": "el.insertAdjacentHTML('beforeend', x);\n",
                "web/notes.txt": "eval(x)\n",
                "web/node_modules/pkg/index.js": "el.innerHTML = x;\n",
            }
        )
        self.assertFlags(problems, "web/js/b.jsx:1", "web/css/x.svg:1", "web/js/c.ts:1", "web/notes.txt:1")
        self.assertFalse(any("node_modules" in p for p in problems), problems)

    def test_r84_inline_scripts_and_handlers_in_markup(self):
        problems = self.problems(
            {
                "web/a.html": '<script data-src="/x.js">alert(1)</script>\n',
                "web/b.html": '<div onclick="x()"></div>\n<p style="color:red"></p>\n<style>p{}</style>\n',
                "web/c.html": '<iframe src="/x"></iframe>\n<a href="javascript:x()">x</a>\n',
            }
        )
        self.assertFlags(
            problems, "web/a.html:1", "inline event handler", "style attribute", "inline <style>",
            "embedded/frame element", "javascript: URL",
        )


class Secrets(GuardCase):
    def test_r82_secrets_in_any_syntax_or_encoding_are_caught(self):
        problems = self.problems(
            {
                "deploy/docker-compose.example.yml": (
                    f"services:\n  v:\n    environment:\n      {TOKEN_KEY}: 3f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c\n"
                    f"      {HASH_KEY}: {ARGON_B64}\n"
                ),
                "deploy/.env.example": f'{TOKEN_KEY}="0123456789abcdef0123"\n',
                "docs/binary.bin": b"\xff\xfe\x00" + f"{TOKEN_KEY}=0123456789abcdef".encode() + b"\x80\x81",
                "notes.md": "$argon" + "2id$v=19$m=19456,t=2,p=1$abc$def\n",
            }
        )
        self.assertFlags(
            problems, "docker-compose.example.yml:4", "docker-compose.example.yml:5", "base64-encoded argon2",
            "deploy/.env.example:1", "docs/binary.bin", "notes.md:1",
        )

    def test_r82_placeholders_are_not_secrets(self):
        problems = self.problems(
            {
                "deploy/.env.example": f"{TOKEN_KEY}=\n{HASH_KEY}=   # generate with hash-password\n",
                "deploy/compose.md": f"- {TOKEN_KEY}=${{{TOKEN_KEY}}}\n",
            }
        )
        self.assertEqual(problems, [])

    def test_r95_database_and_env_files_are_rejected(self):
        problems = self.problems(
            {"vocabulary.db": "x", "a/b.sqlite3": "x", "c.db-wal": "x", ".env": "x", "x/.env.local": "x"}
        )
        self.assertFlags(problems, "vocabulary.db:", "a/b.sqlite3:", "c.db-wal:", ".env:", "x/.env.local:")


class Fixtures(GuardCase):
    def test_r95_fixture_ids_must_be_synthetic_in_every_fixture_dir_and_format(self):
        problems = self.problems(
            {
                "testdata/packs/valid/b.yaml": "source:\n  id: real-book\n",
                "testdata/packs/valid/c.json": '{"pack_id": "real-book--2026-11-17--1"}',
                "crates/x/tests/fixtures/d.toml": 'slug = "real-book"\n',
                "crates/x/tests/fixtures/e.sql": (
                    "INSERT INTO source (id, slug, title) VALUES (1, 'test-ok', 'A'), (2, 'real-book', 'B');\n"
                    "INSERT INTO pack_import (pack_id, source_slug) VALUES ('real--2026-11-17--1', 'test-ok');\n"
                ),
                "crates/x/tests/fixtures/f.sql": "INSERT INTO source VALUES (1, 'real-book');\n",
            }
        )
        self.assertFlags(
            problems, "b.yaml:2", "c.json:1", "d.toml:1", "e.sql:1: fixture slug 'real-book'",
            "e.sql:2: fixture pack_id", "f.sql: cannot verify SQL fixture",
        )
        self.assertFalse(any("test-ok" in p for p in problems), problems)

    def test_r95_repository_sql_fixture_is_accepted(self):
        text = (REPO / "crates/vocab-server/tests/fixtures/schema_v1_synthetic.sql").read_text()
        values = list(guard.sql_fixture_values(text))
        slugs = {v for _, c, v in values if c in guard.SQL_FIXTURE_COLUMNS}
        self.assertTrue(slugs and all(v.startswith("test-") for v in slugs), slugs)


class Rust(GuardCase):
    def test_r92_restricted_visibility_is_rejected(self):
        problems = self.problems(
            {"crates/a/src/lib.rs": "pub(self) fn a() {}\npub(crate) fn b() {}\npub (super) fn c() {}\npub(in crate::x) fn d() {}\n"}
        )
        for line in range(1, 5):
            self.assertFlags(problems, f"crates/a/src/lib.rs:{line}:")


class Renovate(GuardCase):
    def test_r91_every_release_age_must_be_14_days(self):
        cfg = json.loads(json.dumps(RENOVATE_OK))
        index = len(cfg["packageRules"])
        cfg["packageRules"].append({"matchPackageNames": ["*"], "minimumReleaseAge": "0 days"})
        self.assertFlags(self.problems({}, cfg), f"packageRules[{index}].minimumReleaseAge")

    def test_r91_top_level_age_is_required_and_presets_are_reviewed(self):
        cfg = json.loads(json.dumps(RENOVATE_OK))
        del cfg["minimumReleaseAge"]
        cfg["extends"].append("security:minimumReleaseAgeNpm")
        self.assertFlags(self.problems({}, cfg), "top-level minimumReleaseAge", "security:minimumReleaseAgeNpm")

    def test_r91_no_automerge_ignore_tests_or_weaker_age_rules(self):
        cfg = json.loads(json.dumps(RENOVATE_OK))
        cfg["packageRules"].append({"matchManagers": ["npm"], "automerge": True, "ignoreTests": True,
                                    "internalChecksFilter": "flexible",
                                    "minimumReleaseAgeBehaviour": "timestamp-optional"})
        self.assertFlags(self.problems({}, cfg), ".automerge = True", ".ignoreTests = True",
                         ".internalChecksFilter = 'flexible'", "timestamp-optional")
        del cfg["internalChecksFilter"]
        self.assertFlags(self.problems({}, cfg), "top-level internalChecksFilter")

    def test_r91_repository_config_passes(self):
        self.assertEqual(guard.check_renovate(REPO), [])



class SkippedDirectories(GuardCase):
    def test_r95_agent_worktrees_at_the_root_are_skipped_only_there(self):
        problems = self.problems({".claude/worktrees/a/vocabulary.db": "x", "sub/.claude/vocabulary.db": "x"})
        self.assertFlags(problems, "sub/.claude/vocabulary.db")
        self.assertFalse(any(p.startswith(".claude/") for p in problems), problems)


class MoreSecrets(GuardCase):
    def test_r82_key_blocks_tokens_and_password_literals(self):
        problems = self.problems(
            {
                "a.txt": "-----BEGIN PGP " + "PRIVATE KEY BLOCK-----\n",
                "b.txt": "-----BEGIN OPENSSH " + "PRIVATE KEY-----\n",
                "c.yml": "token: " + "eyJhbGciOiJIUzI1NiJ9" + ".eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijklmnop\n",
                "d.sh": "curl -H 'Authorization: " + "Bearer " + "abcdefghijklmnopqrstuvwxyz012345'\n",
                "e.toml": "pass" + 'word = "hunter2hunter2"\n',
                "src/f.rs": 'let c = Config { pass' + 'word: "correct-horse" };\n',
            }
        )
        self.assertFlags(problems, "a.txt:1: looks like a secret: private key", "b.txt:1", "c.yml:1: looks like a secret: JSON Web Token",
                         "d.sh:1: looks like a secret: bearer token", "e.toml:1: looks like a secret: password literal",
                         "src/f.rs:1: looks like a secret: password literal")

    def test_r82_test_passwords_placeholders_and_code_are_fine(self):
        problems = self.problems(
            {
                "tools/tests/test_x.py": "pass" + 'word = "anything-goes-in-tests"\n',
                "web/e2e/login.spec.js": "const pass" + "word = 'test-pw'\n",
                "tools/smoke.py": '{"pass' + 'word": "test-wrong-password"}\nheaders["Authorization"] = f"Bearer {token}"\n',
                "deploy/x.yml": "pass" + 'word: "${PASSWORD}"\n',
            }
        )
        self.assertEqual(problems, [])


class PersonalData(GuardCase):
    def test_r95_home_paths_private_addresses_and_emails(self):
        problems = self.problems(
            {
                "a.md": "/Us" + "ers/alice/dev/x and /ho" + "me/bob/.ssh\n",
                "b.yml": "host: 192.16" + "8.1.20\nother: 10.0" + ".0.7\nts: 100.10" + "0.1.2\nlink: 169.25" + "4.1.1\n",
                "c.toml": 'authors = ["Alice <alice@exam' + 'ple.org>", "bob@mail' + 'box.cz"]\n',
            }
        )
        self.assertFlags(problems, "a.md:1: absolute home directory '/Us" + "ers/alice/'", "absolute home directory '/ho" + "me/bob/'",
                         "b.yml:1: private IPv4", "b.yml:2: private IPv4", "b.yml:3: private IPv4", "b.yml:4: private IPv4",
                         "c.toml:1: e-mail address 'bob@mail" + "box.cz'")
        self.assertFalse(any("alice@" in p for p in problems), problems)  # example.org is fine

    def test_r95_allowed_forms_are_not_flagged(self):
        problems = self.problems(
            {
                "a.md": ("127.0.0.1, 0.0.0.0, 192.0.2.10, 198.51.100.1, 203.0.113.5, version 1.10.0.1, 2.192.168.1.1.3\n"
                         "/home/runner/work, /Users/<you>/x, /home/$USER/x\n"
                         "Co-Authored-By: X <noreply@anthrop" + "ic.com>, git@github.com:o/r.git, 1234+x@users.noreply.github.com\n"
                         "@playwright/test, actions/checkout@v7, typescript@7.0.2\n"),
            }
        )
        self.assertEqual(problems, [])

    def test_r95_real_hosts_in_config_files_only_when_allowed(self):
        problems = self.problems(
            {
                "deploy/compose.yml": "rule: Host(`vocab.my-home.n" + "et`)\nimage: ghcr.io/o/r:1\norigin: https://vocab.example.home\n",
                "docker/Dockerfile.x": "RUN echo https://mirror.some" + "where.de/x\n",
                ".github/workflows/x.yml": "url: https://docs.github.com/x and https://registry.npmjs.org/\n",
                "docs/notes.md": "see https://www.some" + "where.de (docs are not config)\n",
                "testdata/packs/valid/a.json": '{"url": "https://www.bbc.c' + 'o.uk/x"}\n',
                "deploy/labels.yml": "labels: org.opencontainers.image.title, com.docker.compose.project\n",
            }
        )
        self.assertFlags(problems, "deploy/compose.yml:1: host name 'vocab.my-home.n" + "et'",
                         "docker/Dockerfile.x:1: host name 'mirror.some" + "where.de'")
        self.assertEqual(len([p for p in problems if "host name" in p]), 2, problems)

    def test_r95_generated_notices_are_exempt(self):
        self.assertEqual(self.problems({"THIRD-PARTY-NOTICES.txt": "Copyright (c) A <a@mail" + "box.cz>\n"}), [])


class RustComments(GuardCase):
    def test_r92_visibility_in_comments_is_ignored_but_not_in_code(self):
        source = (
            "// a pub(crate) helper would be wrong here\n"
            "/* pub(super) fn x() {} /* nested pub(crate) */ still comment pub(self) */\n"
            'const S: &str = "// not a comment"; pub(crate) fn a() {}\n'
            'const R: &str = r#"/* not a comment */"#; pub(super) fn b() {}\n'
            "/// docs mention pub(crate)\n"
        )
        problems = self.problems({"crates/a/src/lib.rs": source})
        self.assertFlags(problems, "crates/a/src/lib.rs:3:", "crates/a/src/lib.rs:4:")
        self.assertEqual(len(problems), 2, problems)


class RustImages(GuardCase):
    TOOLCHAIN = '[toolchain]\nchannel = "1.99.0"\n'

    def test_r90_dockerfile_rust_image_matches_the_toolchain_file(self):
        ok = self.problems(
            {
                "rust-toolchain.toml": self.TOOLCHAIN,
                "docker/Dockerfile.server": "FROM rust:1.99.0-slim-trixie@sha256:" + "a" * 64 + " AS build\n",
            }
        )
        self.assertEqual(ok, [])
        bad = self.problems(
            {
                "rust-toolchain.toml": self.TOOLCHAIN,
                "docker/Dockerfile.tts": "# x\nfrom --platform=linux/amd64 rust:1.98.0-slim AS b\n",
            }
        )
        self.assertFlags(bad, "docker/Dockerfile.tts:2: rust:1.98.0 does not match")


class SherpaLibraries(GuardCase):
    LOCK = '[[package]]\nname = "sherpa-onnx"\nversion = "2.0.1"\nsource = "registry"\n'

    @staticmethod
    def add(tag: str, file: str) -> str:
        return (f"ADD --checksum=sha256:{'a' * 64} https://github.com/k2-fsa/sherpa-onnx/releases/download/"
                f"v{tag}/sherpa-onnx-v{file}-linux-x64-static-lib.tar.bz2 /archives/\n")

    def test_r90_static_libraries_match_the_crate_in_the_lock_file(self):
        ok = self.problems({"Cargo.lock": self.LOCK, "docker/Dockerfile.tts": "# x\n" + self.add("2.0.1", "2.0.1")})
        self.assertEqual(ok, [])
        for tag, file in [("2.0.2", "2.0.2"), ("2.0.1", "2.0.2"), ("2.1.0", "2.0.1")]:
            with self.subTest(tag=tag, file=file):
                bad = self.problems({"Cargo.lock": self.LOCK, "docker/Dockerfile.tts": "# x\n" + self.add(tag, file)})
                self.assertFlags(bad, "docker/Dockerfile.tts:2: sherpa-onnx libraries")
        no_crate = self.problems({"docker/Dockerfile.tts": self.add("2.0.1", "2.0.1")})
        self.assertFlags(no_crate, "docker/Dockerfile.tts:1: sherpa-onnx libraries v2.0.1 do not match")


if __name__ == "__main__":
    unittest.main()
