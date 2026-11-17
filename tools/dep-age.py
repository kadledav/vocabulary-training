#!/usr/bin/env python3
"""Dependency age gate (R-90): fail if anything pinned in the repo is too young.

The owner's rule: no release published less than 14 days before the run may be
used anywhere, transitive dependencies included. This script checks every pin
it can find and exits non-zero on any violation, on any lookup it cannot
complete, and on anything it cannot verify (fail closed):

* every Cargo.lock      crates.io packages (incl. transitive)        -> crates.io API
                        entries without a source must be workspace members
* every package-lock.json  npm packages (incl. transitive); the `resolved`
                        tarball URL must be registry.npmjs.org and name the
                        same package and version; `integrity` is required -> npm `time` map
* rust-toolchain.toml   the pinned Rust release                      -> static.rust-lang.org
* every file under .github/ (and local actions it references), line by line:
    - every `uses` must be exactly `uses: owner/repo[/path]@<40-hex SHA> # vX.Y.Z`
      (tag must point to the SHA; age = later of release date and commit date)
      or `uses: ./local/path` (whose files are scanned the same way);
    - every `*version*:` key must be exactly `node-version: X.Y.Z`   -> nodejs.org
    - install/download commands are allowed only in these forms:
        cargo install --locked <crate> --version X.Y.Z               -> crates.io API
        rustup toolchain install          (installs rust-toolchain.toml's version)
        npm ci --ignore-scripts / npm run <script>   (lock file checked above)
        npx --no-install playwright install <chromium|webkit|firefox>…
                                          (browsers of the Playwright version in the
                                          lock file; --no-install never fetches another
                                          version; no --with-deps, i.e. no OS packages)
      anything else (pip, npx, yarn, curl, docker, `| sh`, …) is an error;
    - container images, YAML escape sequences and explicit `? key` entries are errors.
* dependency manifests the gate cannot check (requirements.txt, yarn.lock,
  go.mod, Dockerfile, compose files, …) are errors until support is added.

Standard library only. Set GITHUB_TOKEN to raise the GitHub API rate limit
(CI does this automatically). Self-tests: tools/tests/test_dep_age.py.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import sys
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

USER_AGENT = "dep-age-check/1.0 (dependency age gate; python urllib)"
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
NPM_REGISTRY = "https://registry.npmjs.org/"
SKIP_DIRS = {".git", "node_modules", "target", ".idea", "dev-data"}
SEMVER = r"\d+\.\d+\.\d+"


class LookupFailed(Exception):
    """A registry lookup failed; the gate fails closed."""


@dataclass
class Finding:
    kind: str
    name: str
    version: str
    published: dt.datetime

    def age_days(self, now: dt.datetime) -> float:
        return (now - self.published).total_seconds() / 86400


@dataclass
class Result:
    findings: list[Finding] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)

    def add(self, kind: str, name: str, version: str, lookup, where: str = "") -> None:
        """Record a finding; a failed lookup becomes an error."""
        try:
            self.findings.append(Finding(kind, name, version, lookup()))
        except (LookupFailed, KeyError, TypeError, ValueError) as err:
            self.errors.append(f"{where}{': ' if where else ''}{kind} {name} {version}: {err}")


# ── HTTP ─────────────────────────────────────────────────────────────────────

_last_request: dict[str, float] = {}


def fetch(url: str, *, min_interval: float = 0.0, headers: dict[str, str] | None = None) -> bytes:
    host = urllib.parse.urlsplit(url).netloc
    wait = _last_request.get(host, 0.0) + min_interval - time.monotonic()
    if wait > 0:
        time.sleep(wait)
    req_headers = {"User-Agent": USER_AGENT, "Accept": "application/json"}
    req_headers.update(headers or {})
    delay = 2.0
    for attempt in range(5):
        _last_request[host] = time.monotonic()
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=req_headers), timeout=60) as resp:
                return resp.read()
        except urllib.error.HTTPError as err:
            if err.code in (429, 500, 502, 503, 504) and attempt < 4:
                time.sleep(delay)
                delay *= 2
                continue
            raise LookupFailed(f"GET {url}: HTTP {err.code}") from err
        except (urllib.error.URLError, TimeoutError) as err:
            if attempt < 4:
                time.sleep(delay)
                delay *= 2
                continue
            raise LookupFailed(f"GET {url}: {err}") from err
    raise LookupFailed(f"GET {url}: retries exhausted")


def fetch_json(url: str, **kwargs) -> object:
    return json.loads(fetch(url, **kwargs))


def parse_ts(value: str) -> dt.datetime:
    return dt.datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(dt.timezone.utc)


def end_of_day(day: str) -> dt.datetime:
    """Date-only sources count as published at the very end of that day (conservative)."""
    d = dt.date.fromisoformat(day)
    return dt.datetime(d.year, d.month, d.day, 23, 59, 59, tzinfo=dt.timezone.utc)


# ── registry lookups (replaced by fakes in the self-tests) ───────────────────


def crate_published(name: str, version: str) -> dt.datetime:
    # crates.io asks API clients for at most one request per second.
    data = fetch_json(f"https://crates.io/api/v1/crates/{name}/{version}", min_interval=1.0)
    return parse_ts(data["version"]["created_at"])  # type: ignore[index]


_npm_cache: dict[str, dict] = {}


def npm_published(name: str, version: str) -> dt.datetime:
    if name not in _npm_cache:
        quoted = urllib.parse.quote(name, safe="@").replace("/", "%2f")
        _npm_cache[name] = fetch_json(NPM_REGISTRY + quoted)  # type: ignore[assignment]
    times = _npm_cache[name].get("time", {})
    if version not in times:
        raise LookupFailed("no publish time in the npm registry")
    return parse_ts(times[version])


def rust_published(version: str) -> dt.datetime:
    manifest = tomllib.loads(fetch(f"https://static.rust-lang.org/dist/channel-rust-{version}.toml").decode())
    return end_of_day(manifest["date"])


_node_index: list[dict] | None = None


def node_published(version: str) -> dt.datetime:
    global _node_index
    if _node_index is None:
        _node_index = fetch_json("https://nodejs.org/dist/index.json")  # type: ignore[assignment]
    for rel in _node_index or []:
        if rel["version"] == f"v{version}":
            return end_of_day(rel["date"])
    raise LookupFailed("not in the nodejs.org release index")


def github_api(path: str) -> object:
    headers = {"Accept": "application/vnd.github+json"}
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    return fetch_json(f"https://api.github.com{path}", headers=headers)


_action_cache: dict[tuple[str, str, str], dt.datetime] = {}


def action_published(repo: str, sha: str, tag: str) -> dt.datetime:
    """Later of the tag's GitHub release date and the pinned commit's date.

    The tag must resolve exactly to `sha`. Using the later date means neither
    an old commit re-tagged recently nor a recent commit under an old release
    passes."""
    key = (repo, sha, tag)
    if key not in _action_cache:
        ref = github_api(f"/repos/{repo}/git/ref/tags/{urllib.parse.quote(tag)}")
        if not isinstance(ref, dict):
            raise LookupFailed(f"tag {tag} not found exactly in {repo}")
        obj = ref["object"]
        dates = []
        if obj["type"] == "tag":  # annotated tag -> dereference, keep the tagging date
            tag_obj = github_api(f"/repos/{repo}/git/tags/{obj['sha']}")
            dates.append(parse_ts(tag_obj["tagger"]["date"]))  # type: ignore[index]
            obj = tag_obj["object"]  # type: ignore[index]
        if obj["sha"] != sha:
            raise LookupFailed(f"tag {tag} points to {obj['sha']}, not to the pinned {sha}")
        commit = github_api(f"/repos/{repo}/commits/{sha}")
        dates.append(parse_ts(commit["commit"]["committer"]["date"]))  # type: ignore[index]
        try:
            release = github_api(f"/repos/{repo}/releases/tags/{urllib.parse.quote(tag)}")
            dates.append(parse_ts(release["published_at"]))  # type: ignore[index]
        except LookupFailed:
            pass  # no GitHub release for this tag: commit (and tag) date only
        _action_cache[key] = max(dates)
    return _action_cache[key]


# ── Cargo ────────────────────────────────────────────────────────────────────


def workspace_members(manifest_dir: Path) -> set[tuple[str, str]]:
    """(name, version) of the packages of the workspace rooted at manifest_dir."""
    root_toml = tomllib.loads((manifest_dir / "Cargo.toml").read_text())
    ws = root_toml.get("workspace", {})
    ws_version = ws.get("package", {}).get("version", "0.0.0")

    def package_of(toml: dict) -> tuple[str, str] | None:
        pkg = toml.get("package")
        if not pkg:
            return None
        version = pkg.get("version", "0.0.0")
        if isinstance(version, dict) and version.get("workspace"):
            version = ws_version
        return pkg["name"], str(version)

    members: set[tuple[str, str]] = set()
    if (root_pkg := package_of(root_toml)) is not None:
        members.add(root_pkg)
    excluded = {(manifest_dir / e).resolve() for e in ws.get("exclude", [])}
    for pattern in ws.get("members", []):
        for member in sorted(manifest_dir.glob(pattern)):
            if member.resolve() in excluded or not (member / "Cargo.toml").is_file():
                continue
            if (pkg := package_of(tomllib.loads((member / "Cargo.toml").read_text()))) is not None:
                members.add(pkg)
    return members


def check_cargo_lock(path: Path, result: Result) -> None:
    if not (path.parent / "Cargo.toml").is_file():
        result.errors.append(f"{path}: no Cargo.toml next to the lock file")
        return
    try:
        lock = tomllib.loads(path.read_text())
        members = workspace_members(path.parent)
    except (tomllib.TOMLDecodeError, KeyError, OSError) as err:
        result.errors.append(f"{path}: cannot read: {err}")
        return
    for pkg in lock.get("package", []):
        name, version, source = pkg.get("name"), pkg.get("version"), pkg.get("source")
        if source is None:
            if (name, version) not in members:
                result.errors.append(
                    f"{path}: {name} {version} has no source and is not a workspace member "
                    "(path dependency or [patch]; not allowed)"
                )
            continue
        if source != CRATES_IO:
            result.errors.append(f"{path}: {name} {version} comes from {source} (only crates.io is allowed)")
            continue
        result.add("crate", name, version, lambda n=name, v=version: crate_published(n, v), str(path))


# ── npm ──────────────────────────────────────────────────────────────────────


def check_package_lock(path: Path, result: Result) -> None:
    try:
        lock = json.loads(path.read_text())
    except (json.JSONDecodeError, OSError) as err:
        result.errors.append(f"{path}: cannot read: {err}")
        return
    if lock.get("lockfileVersion", 0) < 2:
        result.errors.append(f"{path}: lockfileVersion < 2 is not supported")
        return
    for key, pkg in lock.get("packages", {}).items():
        if key == "":
            continue
        where = f"{path}: {key}"
        if pkg.get("link"):
            target = (path.parent / pkg.get("resolved", "")).resolve()
            if not str(target).startswith(str(path.parent.resolve())):
                result.errors.append(f"{where}: link points outside the package directory")
            continue
        name = pkg.get("name") or key.rsplit("node_modules/", 1)[-1]
        version = pkg.get("version")
        resolved = pkg.get("resolved")
        integrity = pkg.get("integrity", "")
        if not version or not resolved:
            result.errors.append(f"{where}: missing version or resolved URL")
            continue
        expected = f"{NPM_REGISTRY}{name}/-/{name.rsplit('/', 1)[-1]}-{version}.tgz"
        if resolved != expected:
            result.errors.append(f"{where}: resolved URL {resolved} is not the registry tarball of {name}@{version}")
            continue
        if not integrity.startswith("sha512-"):
            result.errors.append(f"{where}: missing sha512 integrity")
            continue
        result.add("npm", name, version, lambda n=name, v=version: npm_published(n, v), str(path))


# ── Rust toolchain ───────────────────────────────────────────────────────────


def check_rust_toolchain(path: Path, result: Result) -> None:
    channel = tomllib.loads(path.read_text()).get("toolchain", {}).get("channel", "")
    if not re.fullmatch(SEMVER, channel):
        result.errors.append(f"{path}: channel must be an exact version, found {channel!r}")
        return
    result.add("rust", "rust", channel, lambda: rust_published(channel), str(path))


# ── .github/ and local actions ───────────────────────────────────────────────

USES_WORD = re.compile(r"uses", re.I)
USES_LINE = re.compile(rf"^\s*(?:-\s+)?uses:\s+(?P<target>[^\s#]+)(?:\s+#\s+(?P<tag>v{SEMVER}))?\s*$")
REMOTE_ACTION = re.compile(r"^(?P<repo>[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)(?:/[A-Za-z0-9_.-]+)*@(?P<sha>[0-9a-f]{40})$")
LOCAL_ACTION = re.compile(r"^\./[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*/?$")
VERSION_KEY = re.compile(r"[\w-]*version[\w-]*[\"']?\s*:", re.I)
NODE_LINE = re.compile(rf"^\s*(?:-\s+)?node-version:\s+(?P<version>{SEMVER})\s*$")
END = r"(?=\s*$|\s*&&|\s*;|\s*\|\|)"
ALLOWED_COMMANDS = [
    re.compile(rf"\bcargo install --locked (?P<crate>[a-z0-9_-]+) --version (?P<version>{SEMVER}){END}"),
    re.compile(rf"\brustup toolchain install{END}"),
    re.compile(rf"\bnpm ci --ignore-scripts{END}"),
    re.compile(rf"\bnpm run [A-Za-z0-9:_-]+{END}"),
    re.compile(rf"\bnpx --no-install playwright install(?: (?:chromium|webkit|firefox))+{END}"),
]
COMMAND_TOKENS = re.compile(
    r"\bcargo\s+(?:install|binstall|\+)|\bcargo-binstall\b|\brustup\b"
    r"|\bnpm\b|\bnpx\b|\bpnpm\b|\byarn\b|\bbunx?\b|\bdeno\b|\bcorepack\b|\bnvm\b|\bvolta\b"
    r"|\bpip[0-9.]*\b|\bpipx\b|\buv\b|\buvx\b|\bpoetry\b|\bconda\b|\beasy_install\b|\bensurepip\b"
    r"|\bgem\b|\bbundler?\b|\bgo\s+(?:install|get|run)\b|\bapt(?:-get)?\b|\bdpkg\b|\brpm\b|\byum\b|\bdnf\b|\bapk\b"
    r"|\bbrew\b|\bsnap\b|\bchoco\b|\bwinget\b|\bscoop\b|\bnix\b"
    r"|\bdocker\b|\bpodman\b|\bbuildah\b|\bcurl\b|\bwget\b|\baria2c\b|\bInvoke-WebRequest\b|\bInvoke-RestMethod\b"
    r"|\biwr\b|\birm\b|\bgit\s+(?:clone|submodule|fetch|pull)\b|\bgh\s+release\s+download\b"
    r"|\|\s*(?:ba|z|da|k)?sh\b|\b(?:ba|z|da|k)?sh\s+-c\b|\bsudo\b|\beval\b|\bsource\s+<\(",
    re.I,
)
# Settings that redirect where installers download from (or what they install).
REDIRECT_ENV = re.compile(
    r"\b(?:PLAYWRIGHT_[A-Z0-9_]*|npm_config_[A-Za-z0-9_]*|NPM_CONFIG_[A-Z0-9_]*|CARGO_[A-Z0-9_]*REGISTR[A-Z0-9_]*"
    r"|RUSTUP_DIST_SERVER|RUSTUP_UPDATE_ROOT)\b"
)
TRAILING_BACKSLASH = re.compile(r"\\\s*$")
BLOCK_SCALAR = re.compile(r"^(?P<indent>\s*)(?:-\s+)*(?:[\w\"'.-]+\s*:\s*)?(?P<style>[|>])[-+0-9]*\s*(?:#.*)?$")
KEY_START = re.compile(r"^(?P<indent>\s*)(?:-\s+)*(?P<key>[\w\"'.-]+\s*:)(?:\s|$)")
OTHER_FORBIDDEN = [
    (re.compile(r"\b(image|container|services)[\"']?\s*:"), "container images are not checked by the age gate yet"),
    (re.compile(r"\\[xuUN0-9]"), "YAML/shell escape sequences cannot be verified"),
    (re.compile(r"^\s*(?:-\s+)*\?(\s|$)"), "explicit '? key' YAML entries cannot be verified"),
]


def indent_of(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def logical_commands(lines: list[str]) -> list[tuple[int, str]]:
    """Join the physical lines of one YAML value into the logical command the
    shell sees, so a continuation line cannot hide flags from the allow-list:

    * folded block scalars (`key: >`) are joined into one command per paragraph;
    * literal block scalars (`key: |`) give one command per line;
    * a plain scalar continued on more-indented lines is joined into one.

    Returns (first line number, text) pairs covering every line once."""
    out: list[tuple[int, str]] = []
    i = 0
    while i < len(lines):
        line = lines[i]
        block = BLOCK_SCALAR.match(line)
        if block:
            out.append((i + 1, line))
            owner = indent_of(line)
            body: list[tuple[int, str]] = []
            j = i + 1
            while j < len(lines) and (lines[j].strip() == "" or indent_of(lines[j]) > owner):
                body.append((j + 1, lines[j]))
                j += 1
            if block.group("style") == "|":
                out.extend((n, text) for n, text in body if text.strip())
            else:
                paragraph: list[tuple[int, str]] = []
                for n, text in body + [(0, "")]:
                    if text.strip():
                        paragraph.append((n, text.strip()))
                    elif paragraph:
                        out.append((paragraph[0][0], " ".join(t for _, t in paragraph)))
                        paragraph = []
            i = j
            continue
        key = KEY_START.match(line)
        joined = line
        j = i + 1
        if key and line[key.end():].strip() and not line.strip().endswith(":"):
            column = line.index(key.group("key"))
            while (
                j < len(lines)
                and lines[j].strip()
                and not lines[j].lstrip().startswith("#")
                and indent_of(lines[j]) > column
                and not KEY_START.match(lines[j])
                and not lines[j].lstrip().startswith("- ")
            ):
                joined += " " + lines[j].strip()
                j += 1
        out.append((i + 1, joined))
        i = j
    return out


def check_commands(lineno: int, text: str, path: Path, result: Result) -> None:
    where = f"{path}:{lineno}"
    allowed_spans: list[tuple[int, int]] = []
    for pattern in ALLOWED_COMMANDS:
        for m in pattern.finditer(text):
            allowed_spans.append(m.span())
            if "crate" in pattern.groupindex:
                crate, version = m.group("crate"), m.group("version")
                result.add("tool", crate, version, lambda c=crate, v=version: crate_published(c, v), where)
    for m in COMMAND_TOKENS.finditer(text):
        if not any(start <= m.start() < end for start, end in allowed_spans):
            result.errors.append(f"{where}: install/download command {m.group(0)!r} is not an allowed verified form")


def scan_github_file(path: Path, root: Path, result: Result, queue: list[Path]) -> None:
    raw = path.read_bytes()
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        result.errors.append(f"{path}: not UTF-8; cannot be verified")
        return
    lines = text.splitlines()
    for lineno, logical in logical_commands(lines):
        check_commands(lineno, logical, path, result)
    for lineno, line in enumerate(lines, 1):
        where = f"{path}:{lineno}"
        if TRAILING_BACKSLASH.search(line):
            result.errors.append(f"{where}: a trailing backslash continues the command on the next line; cannot be verified")
        for m in REDIRECT_ENV.finditer(line):
            result.errors.append(f"{where}: {m.group(0)} can redirect what installers download; not allowed")

        if USES_WORD.search(line):
            m = USES_LINE.match(line)
            if not m or len(USES_WORD.findall(line)) != 1:
                result.errors.append(f"{where}: unverifiable 'uses' (only `uses: owner/repo@<sha> # vX.Y.Z` lines)")
            else:
                check_uses(m.group("target"), m.group("tag"), where, root, result, queue)

        if VERSION_KEY.search(line):
            node = NODE_LINE.match(line)
            if node and len(VERSION_KEY.findall(line)) == 1:
                version = node.group("version")
                result.add("node", "node", version, lambda v=version: node_published(v), where)
            else:
                result.errors.append(f"{where}: only `node-version: X.Y.Z` is supported as a version input")

        for pattern, why in OTHER_FORBIDDEN:
            if pattern.search(line):
                result.errors.append(f"{where}: {why}")


def check_uses(target: str, tag: str | None, where: str, root: Path, result: Result, queue: list[Path]) -> None:
    if target.startswith("./"):
        if not LOCAL_ACTION.match(target) or ".." in target:
            result.errors.append(f"{where}: invalid local action path {target}")
            return
        local = (root / target).resolve()
        if local.is_file():
            queue.append(local)
        elif any((local / name).is_file() for name in ("action.yml", "action.yaml")):
            queue.extend(local / name for name in ("action.yml", "action.yaml") if (local / name).is_file())
        else:
            result.errors.append(f"{where}: local action {target} not found")
        return
    m = REMOTE_ACTION.match(target)
    if not m:
        result.errors.append(f"{where}: {target} must be owner/repo@<full 40-hex commit SHA>")
        return
    if not tag:
        result.errors.append(f"{where}: {target} needs a trailing '# vX.Y.Z' comment naming its release")
        return
    repo, sha = m.group("repo"), m.group("sha")
    result.add("action", target.split("@")[0], tag, lambda: action_published(repo, sha, tag), where)


# ── unsupported manifests (fail closed) ──────────────────────────────────────

UNSUPPORTED = re.compile(
    r"^(requirements.*\.txt|constraints.*\.txt|Pipfile(\.lock)?|pyproject\.toml|poetry\.lock|uv\.lock|setup\.py"
    r"|yarn\.lock|pnpm-lock\.yaml|bun\.lockb?|npm-shrinkwrap\.json|\.yarnrc(\.yml)?|\.pnpmfile\.cjs"
    r"|go\.(mod|sum)|Gemfile(\.lock)?|composer\.(json|lock)|deno\.(json|jsonc|lock)|pom\.xml|build\.gradle(\.kts)?"
    r"|Package\.resolved|Podfile\.lock|mix\.lock|flake\.lock|rust-toolchain|\.tool-versions|\.nvmrc|\.node-version"
    r"|\.python-version|Dockerfile.*|.*\.dockerfile|Containerfile|(docker-)?compose.*\.ya?ml)$"
)


# ── main ─────────────────────────────────────────────────────────────────────


def walk(root: Path):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)
        for f in sorted(filenames):
            yield Path(dirpath) / f


def is_workspace_root(path: Path) -> bool:
    try:
        return "workspace" in tomllib.loads(path.read_text())
    except (tomllib.TOMLDecodeError, OSError):
        return True  # unreadable: treat as needing a lock (fail closed)


def evaluate(root: Path) -> Result:
    result = Result()
    root = root.resolve()
    files = list(walk(root))
    for path in files:
        name = path.name
        if name == "Cargo.lock":
            check_cargo_lock(path, result)
        elif name == "Cargo.toml" and not (path.parent / "Cargo.lock").exists() and is_workspace_root(path):
            result.errors.append(f"{path}: workspace without Cargo.lock: commit the lock file")
        elif name == "package-lock.json":
            check_package_lock(path, result)
        elif name == "package.json" and not (path.parent / "package-lock.json").exists():
            result.errors.append(f"{path}: package.json without package-lock.json")
        elif name == "rust-toolchain.toml":
            check_rust_toolchain(path, result)
        elif UNSUPPORTED.match(name):
            result.errors.append(f"{path}: dependency manifest the age gate cannot check yet; extend tools/dep-age.py")

    github = root / ".github"
    queue: list[Path] = [p for p in files if github in p.parents]
    seen: set[Path] = set()
    while queue:
        path = queue.pop(0).resolve()
        if path in seen:
            continue
        seen.add(path)
        scan_github_file(path, root, result, queue)
    return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--min-age-days", type=float, default=14.0)
    parser.add_argument("--now", help="evaluate as of this UTC time (ISO 8601), default: now")
    parser.add_argument("--verbose", "-v", action="store_true", help="list every checked item")
    args = parser.parse_args(argv)

    now = parse_ts(args.now) if args.now else dt.datetime.now(dt.timezone.utc)
    result = evaluate(args.root)
    too_young = [f for f in result.findings if f.age_days(now) < args.min_age_days]
    if args.verbose:
        for f in sorted(result.findings, key=lambda f: (f.kind, f.name, f.version)):
            print(f"  {f.kind:6} {f.name} {f.version}  {f.published:%Y-%m-%d}  {f.age_days(now):6.1f} d")
    for f in sorted(too_young, key=lambda f: f.published, reverse=True):
        print(
            f"TOO YOUNG: {f.kind} {f.name} {f.version} published {f.published:%Y-%m-%d %H:%M} UTC "
            f"({f.age_days(now):.1f} days < {args.min_age_days:g})"
        )
    for e in result.errors:
        print(f"ERROR: {e}")
    counts: dict[str, int] = {}
    for f in result.findings:
        counts[f.kind] = counts.get(f.kind, 0) + 1
    summary = ", ".join(f"{n} {k}" for k, n in sorted(counts.items()))
    print(
        f"dep-age: checked {len(result.findings)} items ({summary or 'none'}) as of {now:%Y-%m-%d %H:%M} UTC, "
        f"minimum age {args.min_age_days:g} days: {len(too_young)} too young, {len(result.errors)} errors"
    )
    return 1 if too_young or result.errors else 0


if __name__ == "__main__":
    sys.exit(main())
