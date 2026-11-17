"""Pure parts of the image tools: GitHub annotations and job summary
(tools/gha.py), and the session-cookie and log handling of
tools/image-smoke.py. Docker itself is only exercised in CI.

Run: python3 -m unittest discover -s tools/tests -v
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

TOOLS = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(TOOLS))

import gha  # noqa: E402 - needs the path above

_spec = importlib.util.spec_from_file_location("image_smoke", TOOLS / "image-smoke.py")
assert _spec and _spec.loader
smoke = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(smoke)


class Annotations(unittest.TestCase):
    def test_data_escapes_percent_and_line_breaks(self):
        self.assertEqual(gha.escape_data("50% done\r\nnext"), "50%25 done%0D%0Anext")
        # `%` first, so an escape is never escaped twice.
        self.assertEqual(gha.escape_data("%0A"), "%250A")

    def test_property_also_escapes_colon_and_comma(self):
        self.assertEqual(gha.escape_property("a:b,c%\n"), "a%3Ab%2Cc%25%0A")

    def test_error_line_is_one_workflow_command(self):
        line = gha.error_line("image-smoke [uid 3101] login works", "status 401\nbody: x")
        self.assertEqual(line, "::error title=image-smoke [uid 3101] login works::status 401%0Abody: x")
        self.assertEqual(gha.error_line("a: b, c", "m"), "::error title=a%3A b%2C c::m")
        self.assertNotIn("\n", line)
        long = gha.error_line("t", "x" * (gha.MAX_MESSAGE + 10))
        self.assertLess(len(long), gha.MAX_MESSAGE + 40)

    def test_summary_table_and_append(self):
        table = gha.summary_table("Image smoke tests", [("a | b", True, ""), ("c", False, "line\nbreak")])
        self.assertIn("| ✅ | a \\| b |  |", table)
        self.assertIn("| ❌ | c | line break |", table)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "summary.md"
            with mock.patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": str(path)}):
                gha.append_summary(table)
                gha.append_summary("more")
            self.assertEqual(path.read_text(), table + "\nmore\n")
        with mock.patch.dict(os.environ, {}, clear=True):
            gha.append_summary("nowhere")  # no summary file outside Actions: no error


class UntrustedOutput(unittest.TestCase):
    def test_summary_cells_escape_markdown_and_html(self):
        table = gha.summary_table("t", [("<b>x</b> & y", False, "a|b")])
        self.assertIn("| ❌ | &lt;b&gt;x&lt;/b&gt; &amp; y | a\\|b |", table)

    def test_container_log_cannot_issue_workflow_commands(self):
        log = "::error title=fake::injected\n::add-mask::x"
        block = gha.untrusted_block("container log:", log, token="t0ken")
        lines = block.splitlines()
        self.assertEqual(lines[0], "container log:")
        self.assertEqual(lines[1], "::stop-commands::t0ken")
        self.assertEqual(lines[-1], "::t0ken::")
        self.assertEqual(lines[2:-1], log.splitlines())
        # A fresh random token every time, so the log cannot guess it.
        first, second = gha.untrusted_block("h", "x"), gha.untrusted_block("h", "x")
        self.assertNotEqual(first, second)


class SessionCookie(unittest.TestCase):
    def test_secure_cookie_is_taken_from_set_cookie_for_plain_http(self):
        token = "ab" * 32
        headers = [
            ("content-type", "application/json"),
            ("Set-Cookie", "theme=dark; Path=/"),
            ("set-cookie", f"__Host-vocab_session={token}; Path=/; Max-Age=15552000; Secure; HttpOnly; SameSite=Strict"),
        ]
        self.assertEqual(smoke.session_cookie(headers), f"__Host-vocab_session={token}")

    def test_no_or_cleared_session_cookie(self):
        self.assertIsNone(smoke.session_cookie([]))
        self.assertIsNone(smoke.session_cookie([("set-cookie", "__Host-vocab_session=; Max-Age=0; Secure")]))
        self.assertIsNone(smoke.session_cookie([("set-cookie", "x__Host-vocab_session=abc")]))
        self.assertIsNone(smoke.session_cookie([("cookie", "__Host-vocab_session=abc")]))


ELF = b"\x7fELF"
DISTROLESS = [
    ("./", False, b""),
    (".dockerenv", True, b""),
    ("dev/console", True, b""),
    ("etc/hosts", True, b"127.0"),
    ("etc/dpkg/", False, b""),
    ("etc/ssl/certs/ca-certificates.crt", True, b"----"),
    ("etc/update-motd.d/10-uname", True, b"#!/b"),
    ("var/lib/dpkg/status.d/base-files", True, b"Pack"),
    ("usr/lib/os-release", True, b"PRET"),
    ("vocab-server", True, ELF),
]


class ImagePrograms(unittest.TestCase):
    def test_distroless_listing_with_docker_files_and_scripts_passes(self):
        self.assertEqual(smoke.image_programs(DISTROLESS), [])

    def test_shell_is_found_even_as_symlink(self):
        self.assertEqual(smoke.image_programs([*DISTROLESS, ("bin/sh", False, b"")]), ["bin/sh"])
        self.assertEqual(smoke.image_programs([*DISTROLESS, ("./usr/bin/dpkg", True, ELF)]), ["usr/bin/dpkg"])

    def test_any_other_elf_file_is_found(self):
        self.assertEqual(smoke.image_programs([*DISTROLESS, ("usr/bin/foo", True, ELF)]), ["usr/bin/foo"])

    def test_elf_magic_on_a_non_file_is_ignored(self):
        self.assertEqual(smoke.image_programs([("usr/bin/foo", False, ELF)]), [])


class SmokeOutput(unittest.TestCase):
    def test_setup_line_of_the_server_log(self):
        log = (
            "2026-11-17T09:00:00.000001Z  INFO applied database migration\n"
            "2026-11-17T09:00:00.000002Z  WARN Setup required: open the app and enter setup code AB12-CD34-EF\n"
            "2026-11-17T09:00:00.000003Z  INFO serving\n"
        )
        self.assertEqual(smoke.SETUP_LINE.findall(log), ["AB12-CD34-EF"])
        self.assertEqual(smoke.SETUP_LINE.findall("INFO Setup required: … setup code AB12-CD34-EF"), [])

    def test_a_failed_check_becomes_an_annotation_and_a_summary_row(self):
        def failing(_argv):
            smoke.check(True, "first")
            smoke.check(False, "[uid 3101] login works", "status 401, body b'{}'")
            smoke.check(False, "second failure", "still reported")
            smoke.skip(["dependent"], "login failed")
            return 1

        out = io.StringIO()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "summary.md"
            with (
                mock.patch.object(smoke, "smoke", failing),
                mock.patch.object(smoke, "RESULTS", []),
                mock.patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": str(path)}),
                contextlib.redirect_stdout(out),
            ):
                self.assertEqual(smoke.main(["image"]), 1)
            summary = path.read_text()
        self.assertIn("::error title=image-smoke [uid 3101] login works::status 401, body b'{}'\n", out.getvalue())
        self.assertIn("| ✅ | first |", summary)
        self.assertIn("| ❌ | [uid 3101] login works | status 401, body b'{}' |", summary)
        # Later checks still ran and are reported; a skipped one is marked so.
        self.assertIn("::error title=image-smoke second failure::still reported\n", out.getvalue())
        self.assertIn("| ⏭️ | dependent | skipped: login failed |", summary)

    def test_a_crash_is_annotated_too(self):
        def crashing(_argv):
            raise KeyError("boom")

        out = io.StringIO()
        with (
            mock.patch.object(smoke, "smoke", crashing),
            mock.patch.object(smoke, "RESULTS", []),
            mock.patch.dict(os.environ, {}, clear=True),
            contextlib.redirect_stdout(out),
            self.assertRaises(KeyError),
        ):
            smoke.main(["image"])
        self.assertIn("::error title=image-smoke crashed::KeyError", out.getvalue())


if __name__ == "__main__":
    unittest.main()
