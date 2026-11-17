#!/usr/bin/env python3
"""Repository guard: keep data, secrets and unsafe frontend code out of the repo.

Fails (exit 1) when any file that is tracked or would be added (untracked and
not ignored) breaks one of these rules. Every file is scanned as text
(non-UTF-8 bytes are replaced, never skipped):

* R-24/R-95  no databases (`*.sqlite*`, `*.db`), `.env` files, connector
             bundles or key files; no file over 1 MiB (allow-list below);
             fixtures (any `testdata/`, `fixtures/`, … directory) use only
             `test-…` source/pack ids — JSON/YAML/TOML keys and the `slug`,
             `pack_id`, `source_slug` columns of SQL INSERTs (synthetic
             content, design §14.5);
* R-82       no secret-looking strings: private keys, API tokens, argon2
             hashes (also base64-encoded, as stored in .env), filled-in
             VOCAB_* secrets in `KEY=value` or `KEY: value` form;
* R-84       every file under web/ (except node_modules): no HTML-injection
             APIs (innerHTML/outerHTML in any spelling incl. string literals
             naming them, insertAdjacentHTML, document.write, eval, Function,
             execCommand, string timers, srcdoc, javascript: URLs, …), no
             inline <script>; markup files also no inline styles, event
             handler attributes or embedded frames;
* R-92       no `pub(…)` restricted-visibility shortcuts in the Rust crates;
* R-91       renovate.json: top-level minimumReleaseAge "14 days", every
             minimumReleaseAge anywhere equal to "14 days", only known presets.

Afterwards it prints (without failing) the MUST requirements that no test
names yet (tests start with their R-ID: `r23_…` in Rust, `test('R-23 …')` in JS).

Standard library only. Self-tests: tools/tests/test_repo_guard.py.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath

MAX_BYTES = 1024 * 1024
LARGE_FILE_ALLOWLIST: set[str] = set()
SELF = "tools/repo-guard.py"

FORBIDDEN_NAMES = [
    (re.compile(r"\.(sqlite[0-9]*|db|db3|s3db|sl3)([-.][^/]*)?$", re.I), "database files never belong in the repo"),
    (re.compile(r"(^|/)\.env(\.(?!example$)[^/]*)?$"), ".env files hold secrets (only .env.example may be committed)"),
    (re.compile(r"\.mcpb$"), "built connector bundles are not committed"),
    (re.compile(r"\.(pem|key|p12|pfx|jks|keystore)$|(^|/)id_(rsa|ed25519|ecdsa|dsa)"), "key material"),
]

SECRET_PATTERNS = [
    (re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"), "private key"),
    (re.compile(r"\bgh[pousr]_[A-Za-z0-9]{36,}"), "GitHub token"),
    (re.compile(r"\bgithub_pat_[A-Za-z0-9_]{40,}"), "GitHub token"),
    (re.compile(r"\bsk-ant-[A-Za-z0-9_-]{20,}"), "Anthropic API key"),
    (re.compile(r"\bAKIA[0-9A-Z]{16}\b"), "AWS access key"),
    (re.compile(r"\bxox[abposr]-[A-Za-z0-9-]{10,}"), "Slack token"),
    (re.compile(r"\$argon2(id|i|d)\$"), "argon2 password hash"),
    # base64 of "$argon2id$", "$argon2i$", "$argon2d$" (how VOCAB_PASSWORD_HASH_B64 is stored)
    (re.compile(r"JGFyZ29uMmlk|JGFyZ29uMmkk|JGFyZ29uMmQk"), "base64-encoded argon2 password hash"),
    (
        re.compile(r"\bVOCAB_(PASSWORD_HASH_B64|IMPORT_TOKEN|TTS_TOKEN)[\"']?[ \t]*[=:][ \t]*[\"']?(?![$#\s\"'<])[^\s#\"']{8,}"),
        "filled-in app secret",
    ),
]

WEB_FORBIDDEN = [
    (re.compile(r"\b(innerHTML|outerHTML|insertAdjacentHTML)\b"), "HTML-injection property/method"),
    (re.compile(r"(['\"`])[^'\"`\n]*HTML[^'\"`\n]*\1"), "string literal naming an HTML API (computed access)"),
    (re.compile(r"(inner|outer)\s*['\"`]\s*\+", re.I), "split HTML-injection property name"),
    (re.compile(r"\bdocument\s*(\.|\[)\s*['\"`]?write"), "document.write"),
    (re.compile(r"\.\s*write(?:ln)?\s*\("), ".write( call"),
    (re.compile(r"\beval\b"), "eval"),
    (re.compile(r"\bFunction\b"), "Function constructor"),
    (re.compile(r"\bexecCommand\b"), "execCommand"),
    (re.compile(r"\bset(Timeout|Interval)\s*\(\s*(?!\(|function\b|async\b|[A-Za-z_$][\w$]*\s*=>)"),
     "timer without an inline function (string argument)"),
    (re.compile(r"\b(setImmediate|execScript)\b"), "setImmediate/execScript"),
    (re.compile(r"\bcreateContextualFragment\b"), "createContextualFragment"),
    (re.compile(r"\b(setHTMLUnsafe|parseHTMLUnsafe|setHTML|DOMParser)\b"), "HTML parser API"),
    (re.compile(r"\bsrcdoc\b", re.I), "srcdoc"),
    (re.compile(r"javascript\s*:", re.I), "javascript: URL"),
    (re.compile(r"<script\b(?![^>]*\ssrc\s*=)[^>]*>", re.I), "inline <script> (CSP script-src 'self')"),
]
MARKUP_SUFFIXES = {".html", ".htm", ".xhtml", ".svg", ".xml", ".xsl", ".mathml"}
MARKUP_FORBIDDEN = [
    (re.compile(r"<script\b", re.I), "<script> in markup other than the shell's module script"),
    (re.compile(r"<style\b", re.I), "inline <style> (CSP style-src 'self')"),
    (re.compile(r"\sstyle\s*=", re.I), "style attribute (CSP style-src 'self')"),
    (re.compile(r"\son[a-z]+\s*=", re.I), "inline event handler attribute"),
    (re.compile(r"<(iframe|frame|object|embed|foreignObject|base)\b", re.I), "embedded/frame element"),
]
SHELL = "web/index.html"

VISIBILITY_SHORTCUT = re.compile(r"\bpub\s*\(")

FIXTURE_DIRS = {"testdata", "test-data", "test_data", "fixtures", "__fixtures__"}
FIXTURE_KEYS = r"id|pack_id|slug|source_slug|source_id"
FIXTURE_KV = re.compile(rf"[\"']?\b({FIXTURE_KEYS})\b[\"']?\s*[:=]\s*[\"']?([^\"'\s,}}\]#]+)")
SQL_INSERT = re.compile(r"\bINSERT\s+(?:OR\s+\w+\s+)?INTO\s+[\"`]?(\w+)[\"`]?\s*(\()?", re.I)
SQL_FIXTURE_COLUMNS = {"slug", "pack_id", "source_slug"}

RENOVATE_PRESETS = {"config:recommended", "helpers:pinGitHubActionDigests", "docker:pinDigests"}

# Requirements enforced by CI gates rather than by named tests.
GATE_COVERAGE = {
    "R-90": "tools/dep-age.py (CI job guards)",
    "R-91": "renovate.json + repo-guard renovate check",
    "R-92": "repo-guard visibility check + review",
    "R-93": "CI: cargo fmt --check, cargo clippy -D warnings",
    "R-94": "CI runs every test",
    "R-95": "repo-guard file, secret and fixture checks",
}


def repo_files(root: Path) -> list[str]:
    try:
        out = subprocess.run(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
            cwd=root,
            check=True,
            capture_output=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as err:
        sys.exit(f"repo-guard: cannot list files with git in {root}: {err}")
    return sorted({f for f in out.decode().split("\0") if f and (root / f).is_file()})


def line_of(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


def scan(text: str, rel: str, patterns, label: str) -> list[str]:
    return [
        f"{rel}:{line_of(text, m.start())}: {label}{what}"
        for pattern, what in patterns
        for m in pattern.finditer(text)
    ]


def check(root: Path, files: list[str]) -> list[str]:
    problems: list[str] = []
    for rel in files:
        path = root / rel
        parts = PurePosixPath(rel).parts
        for pattern, why in FORBIDDEN_NAMES:
            if pattern.search(rel):
                problems.append(f"{rel}: {why}")
        size = path.stat().st_size
        if size > MAX_BYTES and rel not in LARGE_FILE_ALLOWLIST:
            problems.append(f"{rel}: {size} bytes > {MAX_BYTES} (large files need an explicit allow-list entry)")

        text = path.read_bytes().decode("utf-8", errors="replace")

        if rel != SELF:
            problems += scan(text, rel, SECRET_PATTERNS, "looks like a secret: ")

        if parts[0] == "web" and "node_modules" not in parts:
            problems += scan(text, rel, WEB_FORBIDDEN, "forbidden in web/ (R-84): ")
            if path.suffix.lower() in MARKUP_SUFFIXES:
                markup = MARKUP_FORBIDDEN if rel != SHELL else MARKUP_FORBIDDEN[1:]
                problems += scan(text, rel, markup, "forbidden in markup (R-84): ")

        if parts[0] == "crates" and "src" in parts and path.suffix == ".rs":
            problems += scan(text, rel, [(VISIBILITY_SHORTCUT, "restricted visibility pub(…) (R-92)")], "")

        if FIXTURE_DIRS.intersection(parts[:-1]):
            problems += check_fixture(rel, text)

    problems += check_renovate(root)
    return problems


# ── fixtures ─────────────────────────────────────────────────────────────────


def check_fixture(rel: str, text: str) -> list[str]:
    def bad(lineno: int, key: str, value: str) -> str:
        return f"{rel}:{lineno}: fixture {key} {value!r} must start with 'test-' (synthetic fixtures only, R-95)"

    problems = []
    if rel.lower().endswith(".sql"):
        try:
            for lineno, column, value in sql_fixture_values(text):
                if column in SQL_FIXTURE_COLUMNS and value is not None and not value.startswith("test-"):
                    problems.append(bad(lineno, column, value))
        except ValueError as err:
            problems.append(f"{rel}: cannot verify SQL fixture: {err}")
        return problems
    for m in FIXTURE_KV.finditer(text):
        key, value = m.group(1), m.group(2)
        if value.isdigit() or value in ("null", "~", "None"):
            continue
        if not value.startswith("test-"):
            problems.append(bad(line_of(text, m.start()), key, value))
    return problems


def sql_fixture_values(text: str):
    """Yield (line, column, value) for every value of every INSERT statement."""
    for m in SQL_INSERT.finditer(text):
        if not m.group(2):
            raise ValueError(f"line {line_of(text, m.start())}: INSERT without a column list")
        pos = m.end()
        close = text.index(")", pos)
        columns = [c.strip().strip('"`').lower() for c in text[pos:close].split(",")]
        pos = skip_ws(text, close + 1)
        if not text[pos:pos + 6].upper() == "VALUES":
            raise ValueError(f"line {line_of(text, pos)}: only INSERT … VALUES is supported")
        pos += 6
        while True:
            pos = skip_ws(text, pos)
            if pos >= len(text) or text[pos] != "(":
                raise ValueError(f"line {line_of(text, pos)}: expected a value tuple")
            values, pos = parse_tuple(text, pos)
            if len(values) != len(columns):
                raise ValueError(f"line {line_of(text, pos)}: {len(values)} values for {len(columns)} columns")
            for column, (start, value) in zip(columns, values):
                yield line_of(text, start), column, value
            pos = skip_ws(text, pos)
            if pos < len(text) and text[pos] == ",":
                pos += 1
                continue
            break


def skip_ws(text: str, pos: int) -> int:
    while pos < len(text):
        if text[pos].isspace():
            pos += 1
        elif text.startswith("--", pos):
            nl = text.find("\n", pos)
            pos = len(text) if nl < 0 else nl + 1
        else:
            break
    return pos


def parse_tuple(text: str, pos: int) -> tuple[list[tuple[int, str | None]], int]:
    """Parse `( v, v, … )` at pos; strings are unquoted, other values kept raw (NULL -> None)."""
    assert text[pos] == "("
    pos += 1
    values: list[tuple[int, str | None]] = []
    while True:
        pos = skip_ws(text, pos)
        start = pos
        if pos >= len(text):
            raise ValueError("unterminated value tuple")
        if text[pos] == "'":
            out = []
            pos += 1
            while True:
                end = text.find("'", pos)
                if end < 0:
                    raise ValueError("unterminated string literal")
                out.append(text[pos:end])
                if text.startswith("''", end):
                    out.append("'")
                    pos = end + 2
                    continue
                pos = end + 1
                break
            values.append((start, "".join(out)))
        else:
            depth = 0
            while pos < len(text) and (depth > 0 or text[pos] not in ",)"):
                if text[pos] == "(":
                    depth += 1
                elif text[pos] == ")":
                    depth -= 1
                elif text[pos] == "'":
                    raise ValueError(f"line {line_of(text, pos)}: string inside an expression is not supported")
                pos += 1
            raw = text[start:pos].strip()
            values.append((start, None if raw.upper() == "NULL" else raw))
        pos = skip_ws(text, pos)
        if pos < len(text) and text[pos] == ",":
            pos += 1
            continue
        if pos < len(text) and text[pos] == ")":
            return values, pos + 1
        raise ValueError(f"line {line_of(text, pos)}: malformed value tuple")


# ── renovate ─────────────────────────────────────────────────────────────────


def check_renovate(root: Path) -> list[str]:
    path = root / "renovate.json"
    if not path.exists():
        return ["renovate.json is missing (R-91)"]
    try:
        cfg = json.loads(path.read_text())
    except json.JSONDecodeError as err:
        return [f"renovate.json: invalid JSON: {err}"]
    problems = []
    if not isinstance(cfg, dict) or cfg.get("minimumReleaseAge") != "14 days":
        problems.append('renovate.json: top-level minimumReleaseAge must be "14 days" (R-90/R-91)')
    if not isinstance(cfg, dict) or cfg.get("vulnerabilityAlerts", {}).get("minimumReleaseAge") != "14 days":
        problems.append('renovate.json: vulnerabilityAlerts.minimumReleaseAge must be "14 days" (R-90)')

    def walk(node, where: str) -> None:
        if isinstance(node, dict):
            for key, value in node.items():
                here = f"{where}.{key}"
                if key in ("minimumReleaseAge", "stabilityDays", "minimumMinorAge") and value != "14 days":
                    problems.append(f'renovate.json: {here} = {value!r}; every release age must be "14 days"')
                if key in ("extends", "ignorePresets") and isinstance(value, list):
                    for preset in value:
                        if preset not in RENOVATE_PRESETS:
                            problems.append(f"renovate.json: {here} preset {preset!r} is not on the reviewed list")
                walk(value, here)
        elif isinstance(node, list):
            for i, item in enumerate(node):
                walk(item, f"{where}[{i}]")

    walk(cfg, "$")
    return problems


# ── coverage table ───────────────────────────────────────────────────────────


def coverage(root: Path, files: list[str]) -> list[str]:
    """MUST requirements that no test names yet."""
    req_path = root / "docs" / "requirements.md"
    req = req_path.read_text() if req_path.exists() else ""
    must = sorted(set(re.findall(r"\*\*(R-\d+) MUST", req)), key=lambda r: int(r[2:]))
    covered: set[str] = set(GATE_COVERAGE)
    for rel in files:
        if rel.endswith(".rs") and rel.startswith("crates/"):
            text = (root / rel).read_text(errors="replace")
            covered.update(f"R-{int(n):02d}" for n in re.findall(r"\bfn r(\d+)_", text))
        elif rel.endswith(".js") and re.search(r"(^|/)(test|e2e)/", rel):
            text = (root / rel).read_text(errors="replace")
            covered.update(f"R-{int(n):02d}" for n in re.findall(r"\btest\(\s*['\"`]R-(\d+)\b", text))
    return [r for r in must if r not in covered]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args(argv)
    root = args.root.resolve()
    files = repo_files(root)
    problems = check(root, files)
    for p in problems:
        print(f"FAIL: {p}")
    missing = coverage(root, files)
    if missing:
        print(f"coverage: {len(missing)} MUST requirement(s) without a test yet: {', '.join(missing)}")
    print(f"repo-guard: checked {len(files)} files: {len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
