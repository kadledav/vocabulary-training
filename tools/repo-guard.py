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
* R-82       no secret-looking strings: private keys (PEM/PGP blocks), API
             tokens, JWTs and bearer tokens, argon2 hashes (also
             base64-encoded, as stored in .env), filled-in VOCAB_* secrets
             in `KEY=value` or `KEY: value` form, password literals outside
             tests (test values start with `test-`);
* R-95       public repository: no absolute home directories
             (/Users/<name>/, /home/<name>/), private IPv4 addresses
             (10/8, 172.16/12, 192.168/16, 100.64/10, 169.254/16; loopback,
             0.0.0.0 and the RFC 5737 documentation ranges are fine), e-mail
             addresses other than noreply@… / …@example.*, and in
             configuration files no host name under a real TLD that is not on
             ALLOWED_HOSTS (add a host there, reviewed, to allow it);
* R-84       every file under web/ (except node_modules): no HTML-injection
             APIs (innerHTML/outerHTML in any spelling incl. string literals
             naming them, insertAdjacentHTML, document.write, eval, Function,
             execCommand, string timers, srcdoc, javascript: URLs, …), no
             inline <script>; markup files also no inline styles, event
             handler attributes or embedded frames;
* R-92       no `pub(…)` restricted-visibility shortcuts in the Rust crates
             (code only: comments are ignored);
* R-90       every Dockerfile's `FROM rust:<X>…` uses the release pinned in
             rust-toolchain.toml (the image build does not read that file);
             every sherpa-onnx static library a Dockerfile adds
             (`…/sherpa-onnx/releases/download/v<X>/sherpa-onnx-v<X>-…`) is
             the version of the `sherpa-onnx` crate in Cargo.lock (the
             crate's FFI must match the libraries it links);
* R-91       renovate.json: top-level minimumReleaseAge "14 days", every
             minimumReleaseAge anywhere equal to "14 days", only known presets,
             internalChecksFilter "strict" everywhere, no automerge, no
             ignoreTests, no minimumReleaseAgeBehaviour other than
             "timestamp-required".

Afterwards it prints (without failing) the MUST requirements that no test
names yet (tests start with their R-ID: `r23_…` in Rust, `test('R-23 …')` in JS,
`test_r23_…` in tools/tests).

Standard library only. Self-tests: tools/tests/test_repo_guard.py.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath

# Top-level directories that are not part of the repository's content even
# when untracked and not ignored: local agent worktrees (other checkouts).
ROOT_SKIP_DIRS = {".claude"}
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
    (re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----"), "private key"),
    (re.compile(r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"), "JSON Web Token"),
    (re.compile(r"\bBearer\s+[A-Za-z0-9._~+/-]{20,}=*"), "bearer token"),
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

# A password literal (`password = "…"`, `"password": "…"`) outside tests;
# test values start with `test-`, placeholders are fine.
PASSWORD_LITERAL = re.compile(
    r"\bpassword[\"']?\s*[=:]\s*[\"'](?!test-|<|\$\{|\{\{|\$[A-Z_])[^\"'\s]{4,}[\"']", re.I
)
TEST_PATH = re.compile(r"(^|/)(tests?|e2e|testdata|fixtures)/|(^|/)test_[^/]*$|[._-](test|spec)\.[^/]+$")

# ── personal data and home infrastructure (public repository) ────────────────
#
# Absolute home directories, private IPv4 addresses and e-mail addresses
# anywhere; real host names in configuration files. To allow a new host, add
# it to ALLOWED_HOSTS below (a reviewed change) -- a subdomain of an allowed
# host is allowed too.
HOME_PATH = re.compile(r"/(?:Users|home)/(?!<|\$|\{|runner/|user/|me/|you/|example/|USER/|USERNAME/)[A-Za-z0-9._-]+/")
PRIVATE_IPV4 = re.compile(
    r"(?<![\d.])(?:10\.\d{1,3}|172\.(?:1[6-9]|2\d|3[01])|192\.168|169\.254"
    r"|100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7]))\.\d{1,3}\.\d{1,3}(?![\d.]*\d)"
)
EMAIL = re.compile(r"(?<![\w.%+-])[A-Za-z0-9._%+-]+@(?P<domain>[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,})\b")
ALLOWED_EMAIL = re.compile(r"^(noreply|no-reply|git)@|@(?:[\w-]+\.)*(example\.[a-z]+|users\.noreply\.github\.com)$", re.I)
# Real top-level domains likely in a home setup or a config file (file
# extensions such as .rs, .sh, .py, .md, .pl, .so, .in are left out on purpose).
REAL_TLDS = (
    "com|net|org|io|dev|app|ai|co|me|info|biz|xyz|cloud|tech|site|online|eu|uk|de|cz|sk|at|ch|fr|nl|be|dk|se|no"
    "|fi|it|es|pt|ie|hu|ro|hr|si|lt|lv|ee|us|ca|au|nz|jp|ru|ua|tv|cc|ws|link|page|host|lol|duckdns|ddns"
)
HOST = re.compile(rf"(?<![\w.@-])((?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+(?:{REAL_TLDS}))(?![\w-]|\.[\w-])", re.I)
ALLOWED_HOSTS = {
    "github.com", "githubusercontent.com", "ghcr.io", "gcr.io", "crates.io", "npmjs.org", "debian.org",
    "docker.io", "docker.com", "anthropic.com", "rust-lang.org", "renovatebot.com", "json-schema.org",
    "nodejs.org", "opencontainers.org", "sqlite.org", "llvm.org", "playwright.dev",
    "pythonhosted.org", "pypi.org",
}
CONFIG_SUFFIXES = {".yml", ".yaml", ".toml", ".json", ".ini", ".cfg", ".conf", ".env", ".example", ".npmrc",
                   ".dockerignore", ".gitignore"}
# Generated or third-party text: licence texts name their authors.
PERSONAL_DATA_EXEMPT = {"THIRD-PARTY-NOTICES.txt", "LICENSE", "docker/tts-licenses/THIRD-PARTY-NOTICES.txt"}

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
    return sorted({
        f for f in out.decode().split("\0")
        if f and PurePosixPath(f).parts[0] not in ROOT_SKIP_DIRS and (root / f).is_file()
    })


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
            if not TEST_PATH.search(rel):
                problems += scan(text, rel, [(PASSWORD_LITERAL, "password literal outside tests")],
                                 "looks like a secret: ")
            if rel not in PERSONAL_DATA_EXEMPT:
                problems += personal_data(rel, text)

        if parts[0] == "web" and "node_modules" not in parts:
            problems += scan(text, rel, WEB_FORBIDDEN, "forbidden in web/ (R-84): ")
            if path.suffix.lower() in MARKUP_SUFFIXES:
                markup = MARKUP_FORBIDDEN if rel != SHELL else MARKUP_FORBIDDEN[1:]
                problems += scan(text, rel, markup, "forbidden in markup (R-84): ")

        if parts[0] == "crates" and "src" in parts and path.suffix == ".rs":
            code = without_rust_comments(text)
            problems += scan(code, rel, [(VISIBILITY_SHORTCUT, "restricted visibility pub(…) (R-92)")], "")

        if FIXTURE_DIRS.intersection(parts[:-1]):
            problems += check_fixture(rel, text)

    problems += check_renovate(root)
    problems += check_rust_images(root, files)
    problems += check_sherpa_libraries(root, files)
    return problems


def is_config(rel: str) -> bool:
    """Configuration-like files (not docs, fixtures or code): where a real
    host name means a real deployment."""
    parts = PurePosixPath(rel).parts
    if parts[0] == "docs" or FIXTURE_DIRS.intersection(parts) or parts[-1] in ("Cargo.lock", "package-lock.json"):
        return False
    name = parts[-1].lower()
    return bool(DOCKERFILE_NAME.search(rel)) or name.startswith(".env") or any(
        name.endswith(suffix) for suffix in CONFIG_SUFFIXES
    )


def host_allowed(host: str) -> bool:
    host = host.lower()
    if re.search(r"(^|\.)example\.[a-z]+$", host):
        return True
    return any(host == allowed or host.endswith("." + allowed) for allowed in ALLOWED_HOSTS)


def personal_data(rel: str, text: str) -> list[str]:
    problems = []
    for m in HOME_PATH.finditer(text):
        problems.append(f"{rel}:{line_of(text, m.start())}: absolute home directory {m.group(0)!r} (personal data)")
    for m in PRIVATE_IPV4.finditer(text):
        problems.append(f"{rel}:{line_of(text, m.start())}: private IPv4 address {m.group(0)} (home network detail)")
    for m in EMAIL.finditer(text):
        if not ALLOWED_EMAIL.search(m.group(0)):
            problems.append(f"{rel}:{line_of(text, m.start())}: e-mail address {m.group(0)!r} (personal data; "
                            "use noreply@… or …@example.com)")
    if is_config(rel):
        for m in HOST.finditer(text):
            if not host_allowed(m.group(1)):
                problems.append(f"{rel}:{line_of(text, m.start())}: host name {m.group(1)!r} is not on "
                                "ALLOWED_HOSTS (use a placeholder such as vocab.example.home, or allow it in "
                                "tools/repo-guard.py)")
    return problems


def without_rust_comments(text: str) -> str:
    """Rust source with `//` and (nested) `/* */` comments blanked out, line
    breaks kept; string literals (also raw strings) are left alone."""
    out = []
    i, n = 0, len(text)
    while i < n:
        if text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end < 0 else end
            out.append(" " * (end - i))
            i = end
        elif text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        elif (raw := re.match(r'b?r(#*)"', text[i:])) and not (i and (text[i - 1].isalnum() or text[i - 1] == "_")):
            end = text.find('"' + raw.group(1), i + raw.end())
            end = n if end < 0 else end + 1 + len(raw.group(1))
            out.append(text[i:end])
            i = end
        elif text[i] == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            out.append(text[i:j + 1])
            i = j + 1
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


RUST_IMAGE = re.compile(r"^\s*FROM\s+(?:--\S+\s+)*(?:docker\.io/)?(?:library/)?rust:(?P<version>[0-9.]+)\b", re.I | re.M)
DOCKERFILE_NAME = re.compile(r"(^|/)(Dockerfile[^/]*|[^/]*\.dockerfile|Containerfile[^/]*)$", re.I)


def check_rust_images(root: Path, files: list[str]) -> list[str]:
    """Rust images in Dockerfiles must match rust-toolchain.toml's channel."""
    toolchain = root / "rust-toolchain.toml"
    channel = None
    if toolchain.is_file():
        m = re.search(r'^\s*channel\s*=\s*"([^"]+)"', toolchain.read_text(), re.M)
        channel = m.group(1) if m else None
    problems = []
    for rel in files:
        if not DOCKERFILE_NAME.search(rel) or not (root / rel).is_file():
            continue
        for m in RUST_IMAGE.finditer((root / rel).read_text(errors="replace")):
            if m.group("version") != channel:
                problems.append(
                    f"{rel}:{line_of(m.string, m.start())}: rust:{m.group('version')} does not match "
                    f"rust-toolchain.toml channel {channel!r} (R-90)"
                )
    return problems


SHERPA_LIBRARY = re.compile(
    r"sherpa-onnx/releases/download/v(?P<tag>[0-9][0-9A-Za-z.+-]*)/sherpa-onnx-v(?P<file>[0-9][0-9A-Za-z.+-]*?)-[a-z]"
)
SHERPA_CRATE = re.compile(r'^\[\[package\]\]\nname = "sherpa-onnx"\nversion = "(?P<version>[^"]+)"', re.M)


def check_sherpa_libraries(root: Path, files: list[str]) -> list[str]:
    """The sherpa-onnx static libraries in Dockerfiles must be the release of
    the sherpa-onnx crate in Cargo.lock."""
    lock = root / "Cargo.lock"
    m = SHERPA_CRATE.search(lock.read_text()) if lock.is_file() else None
    crate = m.group("version") if m else None
    problems = []
    for rel in files:
        if not DOCKERFILE_NAME.search(rel) or not (root / rel).is_file():
            continue
        text = (root / rel).read_text(errors="replace")
        for lib in SHERPA_LIBRARY.finditer(text):
            versions = {lib.group("tag"), lib.group("file")}
            if versions != {crate}:
                problems.append(
                    f"{rel}:{line_of(text, lib.start())}: sherpa-onnx libraries v{'/'.join(sorted(versions))} "
                    f"do not match the sherpa-onnx crate {crate!r} in Cargo.lock (R-90)"
                )
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

    if not isinstance(cfg, dict) or cfg.get("internalChecksFilter") != "strict":
        problems.append('renovate.json: top-level internalChecksFilter must be "strict" (only versions past the age)')

    def walk(node, where: str) -> None:
        if isinstance(node, dict):
            for key, value in node.items():
                here = f"{where}.{key}"
                if key == "automerge" and value is not False:
                    problems.append(f"renovate.json: {here} = {value!r}; no automerge (every update is reviewed)")
                if key == "ignoreTests" and value is not False:
                    problems.append(f"renovate.json: {here} = {value!r}; updates must pass CI")
                if key == "internalChecksFilter" and value != "strict":
                    problems.append(f'renovate.json: {here} = {value!r}; must be "strict"')
                if key == "minimumReleaseAgeBehaviour" and value != "timestamp-required":
                    problems.append(f"renovate.json: {here} = {value!r}; a release without a timestamp must "
                                    "not count as old enough")
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
        elif rel.endswith(".py") and rel.startswith("tools/tests/"):
            text = (root / rel).read_text(errors="replace")
            covered.update(f"R-{int(n):02d}" for n in re.findall(r"\bdef test_r(\d+)_", text))
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
