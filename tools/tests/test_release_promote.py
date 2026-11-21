"""Pure parts of tools/release-promote.py: tag and version checks, registry
answers, the never-move rule for version tags, the merged index and the
release notes. Registry, docker and gh themselves only run in the release.

Run: python3 -m unittest discover -s tools/tests -v
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

TOOLS = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(TOOLS))

import gha  # noqa: E402 - needs the path above

_spec = importlib.util.spec_from_file_location("release_promote", TOOLS / "release-promote.py")
assert _spec and _spec.loader
promote = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(promote)

CARGO = '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.1.0"\n'
D1, D2, D3, D4, D5 = (f"sha256:{c * 64}" for c in "12345")


def index(*entries: tuple[str, str]) -> str:
    """An image index of (digest, os/arch) entries; os 'unknown' = attestation."""
    return json.dumps({"manifests": [
        {"digest": d, "platform": {"os": p.split("/")[0], "architecture": p.split("/")[1]}} for d, p in entries
    ]})


class Tags(unittest.TestCase):
    def test_only_plain_semver_tags(self):
        for tag in ("v0.1.0", "v1.20.3", "v10.0.0"):
            self.assertEqual(promote.version_of_tag(tag), tag[1:])
        for tag in ("0.1.0", "v0.1", "v0.1.0-rc1", "v0.1.0+meta", "v01.1.0", "v0.1.0 ", "vv0.1.0", "v0.1.0.1"):
            with self.subTest(tag=tag), self.assertRaises(promote.PromoteFailure):
                promote.version_of_tag(tag)

    def test_tag_must_equal_the_workspace_version(self):
        self.assertEqual(promote.check_tag_matches("v0.1.0", CARGO), "0.1.0")
        with self.assertRaisesRegex(promote.PromoteFailure, "does not match the workspace version 0.1.0"):
            promote.check_tag_matches("v0.2.0", CARGO)
        with self.assertRaisesRegex(promote.PromoteFailure, "no \\[workspace.package\\] version"):
            promote.check_tag_matches("v0.1.0", "[package]\nname = 'x'\n")

    def test_repository_cargo_toml_has_a_workspace_version(self):
        version = promote.workspace_version((TOOLS.parent / "Cargo.toml").read_text())
        self.assertRegex(version, r"^\d+\.\d+\.\d+$")


class Registry(unittest.TestCase):
    def test_manifest_answers(self):
        self.assertEqual(promote.manifest_state(200, b""), "present")
        self.assertEqual(promote.manifest_state(404, b""), "absent")
        self.assertEqual(promote.manifest_state(404, b'{"errors":[{"code":"MANIFEST_UNKNOWN"}]}'), "absent")
        self.assertEqual(promote.manifest_state(404, b'{"errors":[{"code":"NAME_UNKNOWN"}]}'), "absent")
        for status, body in ((401, b""), (403, b'{"errors":[{"code":"DENIED"}]}'), (500, b""), (429, b""),
                             (404, b'{"errors":[{"code":"TOOMANYREQUESTS"}]}')):
            with self.subTest(status=status), self.assertRaises(promote.PromoteFailure):
                promote.manifest_state(status, body)

    def test_an_existing_version_tag_is_never_moved(self):
        self.assertTrue(promote.tag_decision(None, D1, "r:0.1.0"))
        self.assertFalse(promote.tag_decision(D1, D1, "r:0.1.0"))  # re-run: already done
        with self.assertRaisesRegex(promote.PromoteFailure, "refusing to move it"):
            promote.tag_decision(D2, D1, "r:0.1.0")


class MergedIndex(unittest.TestCase):
    def test_merged_index_holds_exactly_the_tested_manifests(self):
        amd = index((D1, "linux/amd64"), (D2, "unknown/unknown"))
        arm = index((D3, "linux/arm64"), (D4, "unknown/unknown"))
        merged = index((D1, "linux/amd64"), (D2, "unknown/unknown"), (D3, "linux/arm64"), (D4, "unknown/unknown"))
        self.assertEqual(promote.merged_problems(merged, [amd, arm]), [])
        self.assertEqual(promote.platforms_of(merged), ["linux/amd64", "linux/arm64"])
        extra = index((D1, "linux/amd64"), (D3, "linux/arm64"), (D5, "linux/arm64"), (D2, "unknown/unknown"))
        problems = promote.merged_problems(extra, [amd, arm])
        self.assertTrue(any("untested manifest " + D5 in p for p in problems), problems)
        self.assertTrue(any("missing from the merged index" in p and D4 in p for p in problems), problems)

    def test_a_single_manifest_is_not_an_index(self):
        with self.assertRaises(promote.PromoteFailure):
            promote.manifest_digests(json.dumps({"config": {}, "layers": []}))


class Notes(unittest.TestCase):
    REF = f"ghcr.io/o/r:0.1.0@{D1}"

    def test_new_release_gets_the_reference(self):
        body = promote.notes_body(None, self.REF)
        self.assertIn(f"    {self.REF}\n", body)

    def test_existing_notes_keep_their_text_and_get_it_once(self):
        body = promote.notes_body("Changes: …", self.REF)
        self.assertTrue(body.startswith("Changes: …\n\n"))
        self.assertIn(self.REF, body)
        self.assertIsNone(promote.notes_body(body, self.REF))  # a re-run adds nothing


class Arguments(unittest.TestCase):
    def test_arguments_are_validated(self):
        ok = promote.parse(["publish", "--repository", "ghcr.io/o/r", "--version", "0.1.0", "--short", "abc1234",
                            "--digest", D1, "--digest", D2])
        self.assertEqual(ok.digest, [D1, D2])
        for argv in (["smoke", "--repository", "docker.io/o/r", "--digest", D1, "--platform", "linux/amd64"],
                     ["smoke", "--repository", "ghcr.io/o/r", "--digest", "sha256:x", "--platform", "linux/amd64"],
                     ["smoke", "--repository", "ghcr.io/o/r", "--digest", D1, "--platform", "linux/riscv64"],
                     ["publish", "--repository", "ghcr.io/o/r", "--version", "0.1", "--short", "abc1234", "--digest", D1]):
            with self.subTest(argv=argv), self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
                promote.parse(argv)

    def test_a_failure_is_annotated_and_summarised(self):
        with tempfile.TemporaryDirectory() as tmp:
            summary = Path(tmp) / "summary.md"
            out = io.StringIO()
            with mock.patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": str(summary)}), contextlib.redirect_stdout(out):
                code = promote.main(["prepare", "--repository", "ghcr.io/o/r", "--tag", "v0.1.0-rc1",
                                     "--commit", "0" * 40])
            self.assertEqual(code, 1)
            self.assertIn("::error title=release-promote prepare tag::", out.getvalue())
            self.assertIn("❌ tag:", summary.read_text())

    def test_outputs_go_to_the_github_output_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "out"
            with mock.patch.dict(os.environ, {"GITHUB_OUTPUT": str(path)}):
                gha.set_output("digest", D1)
            self.assertEqual(path.read_text(), f"digest={D1}\n")
            with self.assertRaises(ValueError):
                gha.set_output("x", "a\nb")


if __name__ == "__main__":
    unittest.main()
