#!/usr/bin/env python3
"""Dependency age gate (R-90): fail if anything pinned in the repo is too young.

The owner's rule: no release published less than 14 days before the run may be
used anywhere, transitive dependencies included. This script checks every pin
it can find and exits non-zero on any violation, on any lookup it cannot
complete, and on anything it cannot verify (fail closed).

Threat model: it catches *accidental* young or unpinned dependencies (added by
an agent, by Renovate or by copy-paste), not a committer who sets out to hide
one. The command checks below are a deny-list of known installer/downloader
tokens (cargo install, npm, pip, curl, docker, …) whose every use must match
one of the verified forms; a tool it does not know is not detected, and
deliberate shell obfuscation is out of scope (review covers that).

* every Cargo.lock      crates.io packages (incl. transitive)        -> crates.io API
                        entries without a source must be workspace members
* every package-lock.json  lockfileVersion 3 (`packages` map, no legacy
                        `dependencies` tree), root entry = package.json's
                        exact dependencies; npm packages (incl. transitive);
                        the `resolved` tarball URL must be registry.npmjs.org
                        and name the same package and version; `integrity`
                        is required                                   -> npm `time` map
* rust-toolchain.toml   the pinned Rust release                      -> static.rust-lang.org
* Dockerfiles (`Dockerfile*`, `*.dockerfile`, `Containerfile`, and any file a
  workflow or compose file builds with `file:` / `dockerfile:`):
    - every `FROM` / `COPY --from` image must be `name:tag@sha256:<digest>`
      (or an earlier stage, or `scratch`)                            -> registry
    - `apt-get install` only with exact `package=version` pins       -> snapshot.debian.org
      (dated by its first appearance for every built architecture,
      DEBIAN_ARCHES) and, for a real install, a preceding dry-run proof
      `test "$(apt-get install -s <same pins> | grep -c '^Inst ')" = N`;
      apt sources only `https://snapshot.debian.org/archive/…/<timestamp>`
      (required when apt installs anything; the timestamp is dated)
    - `# syntax=` directives and `RUN --mount=…,from=` images like `FROM`;
      heredocs, `BUILDKIT_*` arguments and `ONBUILD` are errors
    - known install/download commands only in these forms: `apt-get update`,
      `rustup target add <target or "$VAR">`; other URLs and downloads
      (curl, wget, pip, …) are errors
    - `ADD` from a URL only as `ADD --checksum=sha256:<hex> <GitHub release
      asset URL> <dest>`: the checksum must equal GitHub's digest of the
      asset, dated by the asset's upload                           -> GitHub API
* everywhere: `cargo update|generate-lockfile|add` are errors, cargo commands
  that resolve dependencies must use `--locked`, `cargo --config` is an error;
  settings that redirect downloads (registry/source variables, CARGO_HOME,
  RUSTUP_TOOLCHAIN, PLAYWRIGHT_*, npm_config_*) are errors; any
  `.cargo/config*` file is an error; `.npmrc` may only set NPMRC_KEYS
* compose files (`compose*.yml`, `docker-compose*.yml`, also `*.yml.example`,
  any YAML or JSON file with top-level `services`; `include:`/`extends:` are
  errors): every `image:` (quoted keys too) must be
  `name:tag@sha256:<digest>`                                         -> registry
  (the project's own images, built from this repo, are exempt: OWN_IMAGES)
* every file under .github/ (and local actions it references), line by line
  (comments stripped per physical line, folded blocks split as YAML does):
    - every `uses` must be exactly `uses: owner/repo[/path]@<40-hex SHA> # vX.Y.Z`
      (tag must point to the SHA; age = later of release date and commit date;
      only HTTP 404 means "no release", any other error fails)
      or `uses: ./local/path` (whose files are scanned the same way);
    - every `*version*:` key must be exactly `node-version: X.Y.Z`   -> nodejs.org
    - `runs-on:` only versioned labels (ALLOWED_RUNNERS, never `*-latest`), or
      `${{ matrix.runner }}` / `${{ inputs.runner }}` with every `runner:`
      value in the file on that list;
    - step inputs: `docker/setup-buildx-action` needs `driver-opts:
      image=<pinned>`; `sbom:` only `false` or `generator=<pinned>`;
      `context:` only a local path; `cache-from:` registry refs and
      `build-contexts:` images only by digest; no `BUILDKIT_*` build-args;
      `file:`/`dockerfile:` must name a file in the repo (checked as above);
    - known install/download commands are allowed only in these forms:
        cargo install --locked <crate> --version X.Y.Z               -> crates.io API
        rustup toolchain install          (installs rust-toolchain.toml's version)
        npm ci --ignore-scripts / npm run <script>   (lock file checked above)
        npx --no-install playwright install <chromium|webkit|firefox>…
                                          (browsers of the Playwright version in the
                                          lock file; --no-install never fetches another
                                          version; no --with-deps, i.e. no OS packages)
      other uses of those tokens (pip, npx, yarn, curl, docker, `| sh`, …) are errors;
    - `name:tag@sha256:<digest>` image references (e.g. the BuildKit image in
      `driver-opts: image=…`, the SBOM generator in `sbom: generator=…`) are
      dated; `image=` / `generator=` without a digest are errors -> registry
    - container/services keys, YAML escape sequences and explicit `? key` entries are errors.
* dependency manifests the gate cannot check (requirements.txt, yarn.lock,
  go.mod, …) are errors until support is added.

Directories skipped: SKIP_DIRS only at the repository root, and anywhere a
`node_modules` that sits next to a (checked) package-lock.json.

Image dates: gcr.io's upload time of the digest; elsewhere the newest
`created` time of the image configs behind the digest (images without a real
creation time, e.g. reproducible builds dated 1970, cannot be dated: error).

Immutable lookups (crate/npm/Rust/Node versions, image digests, Debian
packages) are cached in .cache/dep-age/ (ignored by git; `--no-cache` or
DEP_AGE_CACHE=<dir> to change); action tags are looked up on every run.

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
# Skipped only directly under the repository root (build output, VCS data,
# this tool's cache); the same names deeper down are scanned. Anywhere, a
# `node_modules` next to a package-lock.json is skipped: that lock file is
# what installs it, and it is checked.
# `.claude`: local agent worktrees (other checkouts of this repository).
SKIP_DIRS = {".git", "node_modules", "target", ".idea", "dev-data", ".cache", ".claude"}
# runs-on labels: versioned GitHub-hosted runner images only (never *-latest,
# never self-hosted). `${{ matrix.runner }}` / `${{ inputs.runner }}` are
# allowed when every `runner:` value in the file is on this list too.
ALLOWED_RUNNERS = {"ubuntu-24.04", "ubuntu-24.04-arm", "macos-15"}
RUNNER_REFERENCES = {"${{ matrix.runner }}", "${{ inputs.runner }}"}
# The only .npmrc settings allowed: none of them changes where packages come
# from or which versions are installed.
NPMRC_KEYS = {"save-exact", "ignore-scripts", "engine-strict", "fund", "audit", "update-notifier"}
SEMVER = r"\d+\.\d+\.\d+"
# Images built from this repository (not third-party dependencies): compose
# files may reference them with any tag/digest, e.g. a placeholder.
OWN_IMAGES = {"ghcr.io/kadledav/vocabulary-training", "ghcr.io/kadledav/vocabulary-training-tts"}


class LookupFailed(Exception):
    """A registry lookup failed; the gate fails closed. `status` is the HTTP
    status when the server answered with an error, else None."""

    def __init__(self, message: str, status: int | None = None) -> None:
        super().__init__(message)
        self.status = status


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
            raise LookupFailed(f"GET {url}: HTTP {err.code}", err.code) from err
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


# ── cache of immutable facts ─────────────────────────────────────────────────
#
# Publish dates of a crate/npm/Rust/Node version, of an image digest and of a
# Debian package version never change once known, so successful lookups are
# kept in CACHE_DIR (one JSON file per kind, keyed by name and version or
# digest; ignored by git) and repeated runs do not spend registry rate limits.
# Failures are never cached, and neither is anything that can change: which
# commit an action's tag points to and its release are looked up every run.

CACHE_DIR = Path(os.environ.get("DEP_AGE_CACHE") or Path(__file__).resolve().parent.parent / ".cache" / "dep-age")
cache_enabled = True
_cache: dict[str, dict[str, str]] = {}


def _cache_file(kind: str) -> Path:
    return CACHE_DIR / f"{kind}.json"


def cached(kind: str, key: str, lookup) -> dt.datetime:
    """`lookup()`, or its earlier result for (kind, key) from the cache."""
    if not cache_enabled:
        return lookup()
    if kind not in _cache:
        try:
            loaded = json.loads(_cache_file(kind).read_text())
            _cache[kind] = {k: v for k, v in loaded.items() if isinstance(v, str)} if isinstance(loaded, dict) else {}
        except (OSError, ValueError):
            _cache[kind] = {}
    entries = _cache[kind]
    if key in entries:
        try:
            return parse_ts(entries[key])
        except ValueError:
            del entries[key]
    value = lookup()
    entries[key] = value.isoformat()
    try:
        CACHE_DIR.mkdir(parents=True, exist_ok=True)
        tmp = _cache_file(kind).with_suffix(".tmp")
        tmp.write_text(json.dumps(entries, indent=0, sort_keys=True))
        tmp.replace(_cache_file(kind))
    except OSError as err:
        print(f"dep-age: cannot write the cache {CACHE_DIR}: {err}", file=sys.stderr)
    return value


# ── registry lookups (replaced by fakes in the self-tests) ───────────────────


def crate_published(name: str, version: str) -> dt.datetime:
    def lookup() -> dt.datetime:
        # crates.io asks API clients for at most one request per second.
        data = fetch_json(f"https://crates.io/api/v1/crates/{name}/{version}", min_interval=1.0)
        return parse_ts(data["version"]["created_at"])  # type: ignore[index]

    return cached("crate", f"{name}@{version}", lookup)


_npm_cache: dict[str, dict] = {}


def npm_published(name: str, version: str) -> dt.datetime:
    def lookup() -> dt.datetime:
        if name not in _npm_cache:
            quoted = urllib.parse.quote(name, safe="@").replace("/", "%2f")
            _npm_cache[name] = fetch_json(NPM_REGISTRY + quoted)  # type: ignore[assignment]
        times = _npm_cache[name].get("time", {})
        if version not in times:
            raise LookupFailed("no publish time in the npm registry")
        return parse_ts(times[version])

    return cached("npm", f"{name}@{version}", lookup)


def rust_published(version: str) -> dt.datetime:
    def lookup() -> dt.datetime:
        manifest = tomllib.loads(fetch(f"https://static.rust-lang.org/dist/channel-rust-{version}.toml").decode())
        return end_of_day(manifest["date"])

    return cached("rust", version, lookup)


_node_index: list[dict] | None = None


def node_published(version: str) -> dt.datetime:
    def lookup() -> dt.datetime:
        global _node_index
        if _node_index is None:
            _node_index = fetch_json("https://nodejs.org/dist/index.json")  # type: ignore[assignment]
        for rel in _node_index or []:
            if rel["version"] == f"v{version}":
                return end_of_day(rel["date"])
        raise LookupFailed("not in the nodejs.org release index")

    return cached("node", version, lookup)


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
        except LookupFailed as err:
            # Only "not found" means there is no GitHub release for this tag
            # (then the commit and tag dates count); a rate limit, an outage
            # or any other error fails the lookup (fail closed).
            if err.status != 404:
                raise
        _action_cache[key] = max(dates)
    return _action_cache[key]


# a file attached to a GitHub release, pinned by its SHA-256 (ADD --checksum).
GITHUB_RELEASE_ASSET = re.compile(
    r"^https://github\.com/(?P<repo>[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)/releases/download/"
    r"(?P<tag>[A-Za-z0-9_.+-]+)/(?P<asset>[A-Za-z0-9_.+-]+)$"
)
ADD_CHECKSUM = re.compile(r"^--checksum=sha256:(?P<sha256>[0-9a-f]{64})$")


def release_asset_published(repo: str, tag: str, asset: str, sha256: str) -> dt.datetime:
    """When the release asset was uploaded (the later of its creation and
    last update: a re-upload under the same name counts as new), after
    checking that GitHub's digest of the asset is the pinned checksum. An
    asset without a digest, or a digest that differs, fails the lookup."""
    def lookup() -> dt.datetime:
        release = github_api(f"/repos/{repo}/releases/tags/{urllib.parse.quote(tag)}")
        assets = release.get("assets", []) if isinstance(release, dict) else []
        for item in assets:
            if item.get("name") == asset:
                if item.get("digest") != f"sha256:{sha256}":
                    raise LookupFailed(f"GitHub's digest of {asset} is {item.get('digest')!r}, not the pinned sha256")
                return max(parse_ts(item["created_at"]), parse_ts(item["updated_at"]))
        raise LookupFailed(f"no asset {asset} in release {tag} of {repo}")

    return cached("release-asset", f"{repo}@{tag}/{asset}#{sha256}", lookup)


def check_add_url(args: str, where: str, result: "Result") -> None:
    """`ADD --checksum=sha256:<hex> <GitHub release asset URL> <dest>`: the
    only form of ADD from a URL (the build verifies the checksum, the gate
    compares it with GitHub's digest and dates the asset)."""
    words = args.split()
    flags = [w for w in words if w.startswith("--")]
    rest = [w for w in words if not w.startswith("--")]
    checksum = ADD_CHECKSUM.match(flags[0]) if len(flags) == 1 else None
    url = GITHUB_RELEASE_ASSET.match(rest[0]) if len(rest) == 2 else None
    if not checksum or not url:
        result.errors.append(
            f"{where}: ADD from a URL must be `ADD --checksum=sha256:<hex> "
            f"https://github.com/<owner>/<repo>/releases/download/<tag>/<asset> <dest>`"
        )
        return
    repo, tag, asset, sha256 = url.group("repo"), url.group("tag"), url.group("asset"), checksum.group("sha256")
    result.add("asset", f"{repo}/{asset}", tag,
               lambda: release_asset_published(repo, tag, asset, sha256), where)


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
    for problem in package_lock_problems(lock, path.parent / "package.json"):
        result.errors.append(f"{path}: {problem}")
    packages = lock.get("packages") if isinstance(lock, dict) else None
    if not isinstance(packages, dict):
        return
    for key, pkg in packages.items():
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


DEPENDENCY_FIELDS = ("dependencies", "devDependencies", "optionalDependencies", "peerDependencies")


def package_lock_problems(lock: object, manifest: Path) -> list[str]:
    """A package-lock.json the gate can trust to be complete: lockfileVersion
    3 (a `packages` map only, no legacy `dependencies` tree that npm could
    fall back to), whose root entry declares exactly the dependencies of
    package.json, each exact and present in the map at that version."""
    if not isinstance(lock, dict):
        return ["not a JSON object"]
    problems = []
    if lock.get("lockfileVersion") != 3:
        problems.append(f"lockfileVersion must be 3 (found {lock.get('lockfileVersion')!r}); run npm install with npm >= 9")
    if "dependencies" in lock:
        problems.append("legacy `dependencies` tree (lockfileVersion 1/2) is not allowed; only `packages`")
    packages = lock.get("packages")
    if not isinstance(packages, dict) or not isinstance(packages.get(""), dict):
        return problems + ["no `packages` map with a root entry \"\""]
    try:
        declared = json.loads(manifest.read_text())
    except (OSError, ValueError) as err:
        return problems + [f"cannot read {manifest.name} next to it: {err}"]
    root = packages[""]
    for kind in DEPENDENCY_FIELDS:
        wanted = declared.get(kind) or {} if isinstance(declared, dict) else {}
        locked = root.get(kind) or {}
        if wanted != locked:
            problems.append(f"{kind} of the root entry differ from {manifest.name}: {locked} != {wanted}")
        for name, spec in wanted.items() if isinstance(wanted, dict) else []:
            entry = packages.get(f"node_modules/{name}")
            if not isinstance(spec, str) or not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", spec):
                problems.append(f"{manifest.name}: {name} {spec!r} must be an exact version")
            elif not isinstance(entry, dict) or entry.get("version") != spec:
                problems.append(f"{name}@{spec} of {manifest.name} is not in `packages` at that version")
    if len(packages) == 1 and any(declared.get(k) for k in DEPENDENCY_FIELDS if isinstance(declared, dict)):
        problems.append("`packages` has no entries besides the root")
    return problems


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
    r"\bcargo\s+(?:install|binstall|update|generate-lockfile|add|\+)\b|\bcargo\s+\+|\bcargo-binstall\b|\brustup\b"
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
    r"|CARGO_SOURCE_[A-Z0-9_]*|CARGO_HOME|RUSTUP_DIST_SERVER|RUSTUP_UPDATE_ROOT|RUSTUP_TOOLCHAIN)\b"
)
TRAILING_BACKSLASH = re.compile(r"\\\s*$")
BLOCK_SCALAR = re.compile(r"^(?P<indent>\s*)(?:-\s+)*(?:[\w\"'.-]+\s*:\s*)?(?P<style>[|>])[-+0-9]*\s*(?:#.*)?$")
KEY_START = re.compile(r"^(?P<indent>\s*)(?:-\s+)*(?P<key>[\w\"'.-]+\s*:)(?:\s|$)")
OTHER_FORBIDDEN = [
    (re.compile(r"\b(image|container|services)[\"']?\s*:"), "container images are not checked by the age gate yet"),
    (re.compile(r"\\[xuUN0-9]"), "YAML/shell escape sequences cannot be verified"),
    (re.compile(r"^\s*(?:-\s+)*\?(\s|$)"), "explicit '? key' YAML entries cannot be verified"),
]


def command_tokens(text: str):
    """Install/download tokens in `text`; a token directly followed by `/` is
    a path segment (`docker/Dockerfile`, `/var/lib/apt/lists`), not a command."""
    for m in COMMAND_TOKENS.finditer(text):
        if not text[m.end():].startswith("/"):
            yield m


def indent_of(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def strip_comment(line: str) -> str:
    """`line` without a trailing `# comment`. Only lines without any quote
    or backtick are stripped: there a `#` after whitespace starts a comment
    for YAML and for the shell, or lies inside a string opened on an earlier
    line, which is not executed either. Lines with quotes stay whole (more
    text is checked, never less)."""
    if any(q in line for q in "\"'`"):
        return line
    m = re.search(r"(^|\s)#", line)
    return line[: m.start()] if m else line


def logical_commands(lines: list[str]) -> list[tuple[int, str]]:
    """Join the physical lines of one YAML value into the logical command the
    shell sees, so a continuation line cannot hide flags from the allow-list:

    * comments are stripped per physical line (see `strip_comment`);
    * folded block scalars (`key: >`) are joined into one command per
      paragraph; a more-indented line keeps its line breaks (YAML folding
      rules), so it is a command of its own and ends the paragraph;
    * literal block scalars (`key: |`) give one command per line;
    * a plain scalar continued on more-indented lines is joined into one.

    Returns (first line number, text) pairs covering every line once."""
    out: list[tuple[int, str]] = []
    i = 0
    while i < len(lines):
        line = lines[i]
        block = BLOCK_SCALAR.match(line)
        if block:
            out.append((i + 1, strip_comment(line)))
            owner = indent_of(line)
            body: list[tuple[int, str]] = []
            j = i + 1
            while j < len(lines) and (lines[j].strip() == "" or indent_of(lines[j]) > owner):
                body.append((j + 1, lines[j]))
                j += 1
            if block.group("style") == "|":
                out.extend((n, strip_comment(text)) for n, text in body if strip_comment(text).strip())
            else:
                base = min((indent_of(t) for _, t in body if t.strip()), default=0)
                paragraph: list[tuple[int, str]] = []

                def flush() -> None:
                    if paragraph:
                        out.append((paragraph[0][0], " ".join(t for _, t in paragraph)))
                        paragraph.clear()

                for n, text in body:
                    if not text.strip():
                        flush()
                    elif indent_of(text) > base:
                        flush()
                        out.append((n, strip_comment(text).strip()))
                    else:
                        paragraph.append((n, strip_comment(text).strip()))
                flush()
            i = j
            continue
        key = KEY_START.match(line)
        joined = strip_comment(line)
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
                joined += " " + strip_comment(lines[j]).strip()
                j += 1
        out.append((i + 1, joined))
        i = j
    return out


CARGO_RESOLVING = re.compile(r"\bcargo\s+(?:\+\S+\s+)?(build|test|run|check|clippy|fetch|doc|bench|tree|deny|metadata)\b")


def cargo_commands(text: str) -> list[str]:
    """The cargo commands in `text`: segments that start with cargo (after
    `run:`, a list dash or `VAR=value` assignments), not names or values."""
    found = []
    for segment in re.split(r"&&|\|\||;|\||\$\(|`", text):
        words = segment.strip()
        words = re.sub(r"^(?:-\s+)+", "", words)
        key = re.match(r"^[\"']?([\w-]+)[\"']?\s*:(\s|$)", words)
        if key:
            if key.group(1) != "run":
                continue
            words = words[key.end():].strip()
        words = re.sub(r"^(?:(?:[A-Za-z_][A-Za-z0-9_]*=\S*|env|time|nice|command|exec|then|do|else|!)\s+)+", "", words)
        if re.match(r"cargo\b", words):
            found.append(words)
    return found


def unlocked_cargo(text: str) -> list[str]:
    """cargo subcommands that resolve dependencies but lack `--locked`
    (without it cargo may rewrite Cargo.lock with newer releases)."""
    found = []
    for words in cargo_commands(text):
        m = CARGO_RESOLVING.match(words)
        if m and not re.search(r"(^|\s)(--locked|--version|-V)(\s|$)", words):
            found.append(m.group(1))
    return found


def cargo_problems(text: str) -> list[str]:
    """Everything wrong with the cargo commands in `text`: a missing
    `--locked`, and `--config` (which can replace the registry or patch
    crates, like a .cargo/config.toml)."""
    problems = [f"cargo {sub} must use --locked" for sub in unlocked_cargo(text)]
    problems += [
        "cargo --config can redirect where crates come from; not allowed"
        for words in cargo_commands(text)
        if re.search(r"(^|\s)--config(=|\s|$)", words)
    ]
    return problems


def check_commands(lineno: int, text: str, path: Path, result: Result) -> None:
    where = f"{path}:{lineno}"
    for problem in cargo_problems(text):
        result.errors.append(f"{where}: {problem}")
    allowed_spans: list[tuple[int, int]] = []
    for pattern in ALLOWED_COMMANDS:
        for m in pattern.finditer(text):
            allowed_spans.append(m.span())
            if "crate" in pattern.groupindex:
                crate, version = m.group("crate"), m.group("version")
                result.add("tool", crate, version, lambda c=crate, v=version: crate_published(c, v), where)
    for m in command_tokens(text):
        if not any(start <= m.start() < end for start, end in allowed_spans):
            result.errors.append(f"{where}: install/download command {m.group(0)!r} is not an allowed verified form")


def scan_github_file(path: Path, root: Path, result: Result, queue: list[Path],
                     dockerfiles: list[Path] | None = None) -> None:
    """Check one file under .github/ (or a local action); local actions it
    uses go to `queue`, Dockerfiles its steps build go to `dockerfiles`."""
    raw = path.read_bytes()
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        result.errors.append(f"{path}: not UTF-8; cannot be verified")
        return
    lines = text.splitlines()
    for lineno, logical in logical_commands(lines):
        if USES_LINE.match(logical):
            continue  # verified below; action names like docker/… are not commands
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

        check_runners(strip_comment(line), where, result)

        images = list(IMAGE_REF.finditer(line))
        for m in images:
            add_image(result, m.group("name"), m.group("tag"), m.group("digest"), where)
        if re.search(r"\b(image|generator)\s*=", line) and len(images) != len(re.findall(r"\b(?:image|generator)\s*=", line)):
            result.errors.append(f"{where}: image=/generator= must be name:tag@sha256:<digest>")

    for lineno, action, inputs in step_inputs(lines):
        check_step_inputs(f"{path}:{lineno}", action, inputs, root, result, dockerfiles)


RUNS_ON = re.compile(r"^\s*(?:-\s+)?[\"']?runs-on[\"']?\s*:(?P<value>.*)$")
RUNNER_KEY = re.compile(r"(?:^|[\s{,-])[\"']?runner[\"']?\s*:(?P<value>[^,}]*)")


def unquote(value: str) -> str:
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1].strip()
    return value


def check_runners(line: str, where: str, result: Result) -> None:
    """`runs-on:` names a versioned runner image of ALLOWED_RUNNERS (or the
    matrix/input `runner`, whose every value is checked the same way)."""
    m = RUNS_ON.match(line)
    if m:
        value = unquote(m.group("value"))
        if value not in ALLOWED_RUNNERS and value not in RUNNER_REFERENCES:
            result.errors.append(
                f"{where}: runs-on {value or '(on the next lines)'!r} is not a versioned runner label "
                f"({', '.join(sorted(ALLOWED_RUNNERS))}, or {' / '.join(sorted(RUNNER_REFERENCES))})"
            )
    for m in RUNNER_KEY.finditer(line):
        value = unquote(m.group("value"))
        if not value:
            continue  # a mapping (e.g. the declaration of a `runner` input), not a label
        if value not in ALLOWED_RUNNERS:
            result.errors.append(f"{where}: runner {value or '(empty)'!r} is not one of {', '.join(sorted(ALLOWED_RUNNERS))}")


def step_inputs(lines: list[str]) -> list[tuple[int, str, dict[str, tuple[int, str]]]]:
    """(line, action, {input: (line, value)}) of every `uses:` (inputs from
    its `with:` block, if any): the step's keys are the lines at the column of its `uses:`; block
    scalar and continuation lines of an input are joined with newlines."""
    out = []
    for i, line in enumerate(lines):
        m = USES_LINE.match(strip_comment(line))
        if not m:
            continue
        column = line.index("uses")
        start = i
        if not re.match(r"^\s*-\s", line):
            # Back to the step's `- ` line (keys before `uses:`), not into
            # the previous step.
            while start > 0:
                prev = lines[start - 1]
                dash = re.match(r"^\s*-\s+", prev)
                if not prev.strip() or (indent_of(prev) >= column and not dash):
                    start -= 1
                    continue
                if dash and len(dash.group(0)) == column:
                    start -= 1
                break
        end = i + 1
        while end < len(lines) and (not lines[end].strip() or indent_of(lines[end]) >= column):
            end += 1
        with_line = None
        for j in range(start, end):
            text = lines[j]
            if re.match(r"^\s*(?:-\s+)?with\s*:\s*(#.*)?$", text) and text.index("with") == column:
                with_line = j
        inputs: dict[str, tuple[int, str]] = {}
        j = end if with_line is None else with_line + 1
        key_indent = None
        current = None
        while j < end:
            text = lines[j]
            if text.strip() and indent_of(text) <= column:
                break
            if text.strip() and not text.lstrip().startswith("#"):
                if key_indent is None:
                    key_indent = indent_of(text)
                km = re.match(r"^\s*[\"']?([\w-]+)[\"']?\s*:(?:\s+(.*))?$", text)
                if indent_of(text) == key_indent and km:
                    current = km.group(1)
                    value = unquote(strip_comment(km.group(2) or ""))
                    if re.fullmatch(r"[|>][-+0-9]*", value):
                        value = ""
                    inputs[current] = (j + 1, value)
                elif current is not None:
                    line_no, value = inputs[current]
                    inputs[current] = (line_no, (value + "\n" + strip_comment(text).strip()).strip())
            j += 1
        out.append((i + 1, m.group("target").split("@", 1)[0], inputs))
    return out


def check_step_inputs(where: str, action: str, inputs: dict[str, tuple[int, str]], root: Path,
                      result: Result, dockerfiles: list[Path] | None) -> None:
    """Inputs of an action step that make it fetch something floating."""
    base = where.rsplit(":", 1)[0]

    def error(key: str, message: str) -> None:
        line = inputs[key][0] if key in inputs else where.rsplit(":", 1)[1]
        result.errors.append(f"{base}:{line}: {message}")

    if action == "docker/setup-buildx-action":
        opts = inputs.get("driver-opts", (0, ""))[1]
        if not re.search(r"(^|[\s,])image=" + IMAGE_REF.pattern, opts):
            error("driver-opts", "setup-buildx-action needs `driver-opts: image=<buildkit>:tag@sha256:<digest>` "
                                 "(otherwise it pulls the floating moby/buildkit image)")
    for key, (_, value) in inputs.items():
        if key == "sbom" and value != "false" and not value.startswith("generator="):
            error(key, "sbom must be `false` or `generator=<image>:tag@sha256:<digest>` (true uses a floating generator image)")
        elif key == "context" and not re.fullmatch(r"[\w./-]+", value):
            error(key, f"context {value!r} must be a local path (a remote or computed context cannot be verified)")
        elif key == "cache-from":
            for entry in filter(None, re.split(r"[\n]", value)):
                registry = "type=registry" in entry or "type=" not in entry
                if registry and "@sha256:" not in entry:
                    error(key, f"cache-from {entry!r} pulls a registry cache without a digest")
        elif key == "build-contexts":
            for entry in filter(None, value.split("\n")):
                if "://" in entry and not re.search(r"docker-image://\S+@sha256:[0-9a-f]{64}", entry):
                    error(key, f"build context {entry!r} must be local or a docker-image:// pinned by digest")
        elif key == "build-args":
            for entry in filter(None, value.split("\n")):
                if entry.strip().startswith("BUILDKIT_"):
                    error(key, f"build-arg {entry.strip()!r} controls BuildKit (frontend images, …); not allowed")
        elif key in ("file", "dockerfile"):
            target = (root / value).resolve() if value and "${{" not in value else None
            if target is None or root.resolve() not in target.parents or not target.is_file():
                error(key, f"{key} {value!r} must name a Dockerfile in the repository (it is checked like one)")
            elif dockerfiles is not None:
                dockerfiles.append(target)


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


# ── container images and Debian packages ────────────────────────────────────

IMAGE_REF = re.compile(
    r"(?P<name>(?:[a-z0-9.-]+(?::[0-9]+)?/)?[a-z0-9._-]+(?:/[a-z0-9._-]+)*)"
    r":(?P<tag>[A-Za-z0-9_][A-Za-z0-9_.-]{0,127})@sha256:(?P<digest>[0-9a-f]{64})"
)
MANIFEST_TYPES = ", ".join(
    [
        "application/vnd.oci.image.index.v1+json",
        "application/vnd.docker.distribution.manifest.list.v2+json",
        "application/vnd.oci.image.manifest.v1+json",
        "application/vnd.docker.distribution.manifest.v2+json",
    ]
)
# Before this, a `created` time is a reproducible-build placeholder, not a date.
EARLIEST_REAL_DATE = dt.datetime(2015, 1, 1, tzinfo=dt.timezone.utc)


def split_image(name: str) -> tuple[str, str]:
    """(registry host, repository path) of an image name, Docker Hub rules."""
    first, _, rest = name.partition("/")
    if rest and ("." in first or ":" in first or first == "localhost"):
        registry, repo = first, rest
    else:
        registry, repo = "docker.io", name
    if registry == "docker.io":
        registry = "registry-1.docker.io"
        if "/" not in repo:
            repo = f"library/{repo}"
    return registry, repo


_registry_tokens: dict[tuple[str, str], str] = {}


def registry_get(registry: str, repo: str, path: str, accept: str) -> bytes:
    """GET https://<registry>/v2/<repo>/<path>, with an anonymous pull token
    when the registry asks for one (WWW-Authenticate: Bearer …)."""
    url = f"https://{registry}/v2/{repo}/{path}"
    for _ in range(2):
        headers = {"User-Agent": USER_AGENT, "Accept": accept}
        token = _registry_tokens.get((registry, repo))
        if token:
            headers["Authorization"] = f"Bearer {token}"
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as resp:
                return resp.read()
        except urllib.error.HTTPError as err:
            challenge = err.headers.get("WWW-Authenticate", "") if err.headers else ""
            if err.code != 401 or not challenge.lower().startswith("bearer ") or token:
                raise LookupFailed(f"GET {url}: HTTP {err.code}") from err
            params = dict(re.findall(r'(\w+)="([^"]*)"', challenge))
            realm = params.pop("realm", "")
            if not realm.startswith("https://"):
                raise LookupFailed(f"GET {url}: unusable auth challenge") from err
            params.setdefault("scope", f"repository:{repo}:pull")
            answer = fetch_json(realm + "?" + urllib.parse.urlencode(params))
            token = answer.get("token") or answer.get("access_token")  # type: ignore[union-attr]
            if not token:
                raise LookupFailed(f"GET {url}: no registry token") from err
            _registry_tokens[(registry, repo)] = token
        except (urllib.error.URLError, TimeoutError) as err:
            raise LookupFailed(f"GET {url}: {err}") from err
    raise LookupFailed(f"GET {url}: not authorised")


def _blob_json(registry: str, repo: str, kind: str, digest: str, accept: str) -> dict:
    body = registry_get(registry, repo, f"{kind}/{digest}", accept)
    import hashlib

    if "sha256:" + hashlib.sha256(body).hexdigest() != digest:
        raise LookupFailed(f"{repo}@{digest}: registry content does not match the digest")
    return json.loads(body)


_image_cache: dict[tuple[str, str], dt.datetime] = {}


def image_published(name: str, digest: str) -> dt.datetime:
    """When the image `name@digest` was published (see the module docstring)."""
    return cached("image", f"{name}@{digest}", lambda: _image_published(name, digest))


def _image_published(name: str, digest: str) -> dt.datetime:
    key = (name, digest)
    if key in _image_cache:
        return _image_cache[key]
    registry, repo = split_image(name)
    if registry == "gcr.io":
        listing = json.loads(registry_get(registry, repo, "tags/list", "application/json"))
        info = listing.get("manifest", {}).get(digest)
        if not info:
            raise LookupFailed(f"{digest} is not in {name}")
        stamps = [int(info.get(k, 0)) for k in ("timeUploadedMs", "timeCreatedMs")]
        published = dt.datetime.fromtimestamp(max(stamps) / 1000, dt.timezone.utc)
    else:
        top = _blob_json(registry, repo, "manifests", digest, MANIFEST_TYPES)
        manifests = [top]
        if "manifests" in top:  # index: every real platform image (not attestations)
            manifests = [
                _blob_json(registry, repo, "manifests", m["digest"], MANIFEST_TYPES)
                for m in top["manifests"]
                if m.get("platform", {}).get("os") not in (None, "unknown")
            ]
        dates = []
        for manifest in manifests:
            config = _blob_json(registry, repo, "blobs", manifest["config"]["digest"], "*/*")
            dates.append(parse_ts(config["created"]))
        if not dates:
            raise LookupFailed("no platform images behind the digest")
        published = max(dates)
    if published < EARLIEST_REAL_DATE:
        raise LookupFailed(f"no real publish date ({published:%Y-%m-%d}); cannot be dated")
    _image_cache[key] = published
    return published


# The platforms the images are built for (docker/Dockerfile.server is built
# natively for each), i.e. the architectures whose .debs get installed.
DEBIAN_ARCHES = ("amd64", "arm64")


def debian_published(package: str, version: str) -> dt.datetime:
    """When the .deb was in the Debian archive for every built architecture:
    per architecture (or `all`) its first appearance, then the latest of those."""
    def lookup() -> dt.datetime:
        url = (
            "https://snapshot.debian.org/mr/binary/"
            f"{urllib.parse.quote(package, safe='')}/{urllib.parse.quote(version, safe='')}/binfiles?fileinfo=1"
        )
        data = fetch_json(url, min_interval=1.0)
        dates = []
        for arch in DEBIAN_ARCHES:
            seen = [
                info["first_seen"]
                for item in data.get("result", [])  # type: ignore[union-attr]
                if item.get("architecture") in (arch, "all")
                for info in data.get("fileinfo", {}).get(item["hash"], [])  # type: ignore[union-attr]
                if info.get("archive_name") in ("debian", "debian-security")
            ]
            if not seen:
                raise LookupFailed(f"not in snapshot.debian.org for {arch}")
            dates.append(min(dt.datetime.strptime(s, "%Y%m%dT%H%M%SZ").replace(tzinfo=dt.timezone.utc) for s in seen))
        return max(dates)

    return cached("debian", f"{package}={version}/{'+'.join(DEBIAN_ARCHES)}", lookup)


def add_image(result: "Result", name: str, tag: str, digest: str, where: str) -> None:
    result.add("image", f"{name}:{tag}", digest[:12], lambda: image_published(name, f"sha256:{digest}"), where)


DEBIAN_PIN = re.compile(r"^(?P<pkg>[a-z0-9][a-z0-9+.-]+)=(?P<version>[0-9A-Za-z.+~:-]+)$")
APT_FLAGS = {"-y", "--yes", "-q", "-qq", "--no-install-recommends", "-s", "--simulate"}
SIMULATE_FLAGS = {"-s", "--simulate"}
# `test "$(apt-get install -s … | grep -c '^Inst ')" = N`: proves that apt
# installs exactly N packages, i.e. no unpinned dependency.
APT_PROOF = re.compile(
    r"""\btest\s+"\$\(apt-get install(?P<args>[^|"]*)\|\s*grep -c '\^Inst '\)"\s*=\s*(?P<count>[0-9]+)"""
)
APT_INSTALL = re.compile(r"\bapt-get install\b(?P<args>[^&;|)\"]*)")
SNAPSHOT_URL = re.compile(
    r"https://snapshot\.debian\.org/archive/(?P<archive>debian|debian-security)/(?P<stamp>[0-9]{8}T[0-9]{6}Z)/?(?=[\s'\"]|$)"
)
URL = re.compile(r"\b(?:https?|ftp|git|ssh)://\S+")
DOCKER_ALLOWED = [
    re.compile(rf"\bapt-get update{END}"),
    # A target triple, or a shell variable holding one (the musl target of
    # the platform being built): rustup only adds the standard library of
    # the image's own, dated Rust release.
    re.compile(rf"\brustup target add (?:[a-z0-9_-]+|\"\$[A-Za-z_][A-Za-z0-9_]*\"){END}"),
]


def snapshot_published(stamp: str) -> dt.datetime:
    """A snapshot.debian.org timestamp is the state of the archive at that time."""
    try:
        return dt.datetime.strptime(stamp, "%Y%m%dT%H%M%SZ").replace(tzinfo=dt.timezone.utc)
    except ValueError as err:
        raise LookupFailed(f"invalid snapshot timestamp {stamp}") from err


def dockerfile_instructions(text: str) -> list[tuple[int, str, str]]:
    """(line, INSTRUCTION, arguments) with backslash continuations joined and
    comments dropped (Dockerfile semantics)."""
    out: list[tuple[int, str, str]] = []
    buffer: list[str] = []
    start = 0
    for lineno, line in enumerate(text.splitlines(), 1):
        if not buffer and (not line.strip() or line.lstrip().startswith("#")):
            continue
        if buffer and line.lstrip().startswith("#"):
            continue
        if not buffer:
            start = lineno
        stripped = line.rstrip()
        if stripped.endswith("\\"):
            buffer.append(stripped[:-1])
            continue
        buffer.append(stripped)
        joined = " ".join(part.strip() for part in buffer)
        buffer = []
        word, _, rest = joined.partition(" ")
        out.append((start, word.upper(), rest.strip()))
    if buffer:
        out.append((start, "UNTERMINATED", " ".join(buffer)))
    return out


def apt_args(args: str) -> tuple[list[str], list[str]]:
    words = args.split()
    return [w for w in words if not w.startswith("-")], [w for w in words if w.startswith("-")]


def check_apt_installs(command: str, where: str, result: "Result") -> list[tuple[int, int]]:
    """Every `apt-get install` must use exact pins and known flags, and a real
    install must be preceded (in the same RUN) by a dry-run proof that apt
    installs exactly those packages. Dates every pin; returns accepted spans."""
    spans: list[tuple[int, int]] = []
    proven: set[frozenset[str]] = set()
    for m in APT_PROOF.finditer(command):
        pins, flags = apt_args(m.group("args"))
        if set(flags) & SIMULATE_FLAGS and len(set(pins)) == int(m.group("count")):
            proven.add(frozenset(pins))
    for m in APT_INSTALL.finditer(command):
        pins, flags = apt_args(m.group("args"))
        bad = [f for f in flags if f not in APT_FLAGS]
        unpinned = [p for p in pins if not DEBIAN_PIN.match(p)]
        if bad or unpinned or not pins:
            result.errors.append(
                f"{where}: apt-get install needs exact package=version pins and known flags "
                f"(unpinned: {unpinned or '-'}, flags: {bad or '-'})"
            )
            continue
        simulate = bool(set(flags) & SIMULATE_FLAGS)
        if not simulate and frozenset(pins) not in proven:
            result.errors.append(
                f"{where}: apt-get install of {' '.join(pins)} is not proven exact: precede it with "
                f"test \"$(apt-get install -s … | grep -c '^Inst ')\" = {len(set(pins))}"
            )
            continue
        spans.append(m.span())
        for pin in pins:
            pm = DEBIAN_PIN.match(pin)
            assert pm
            pkg, version = pm.group("pkg"), pm.group("version")
            result.add("debian", pkg, version, lambda p=pkg, v=version: debian_published(p, v), where)
    return spans


def check_from_source(source: str, stages: set[str], where: str, label: str, result: "Result") -> None:
    if source.lower() in stages or source.isdigit():
        return
    m = IMAGE_REF.fullmatch(source)
    if not m:
        result.errors.append(f"{where}: {label}{source} must be a stage or name:tag@sha256:<digest>")
        return
    add_image(result, m.group("name"), m.group("tag"), m.group("digest"), where)


SYNTAX_DIRECTIVE = re.compile(r"^\s*#\s*syntax\s*=\s*(?P<ref>\S*)", re.I)


def check_dockerfile(path: Path, result: Result) -> None:
    try:
        text = path.read_text()
    except (OSError, UnicodeDecodeError) as err:
        result.errors.append(f"{path}: cannot read: {err}")
        return
    # A syntax directive makes BuildKit download that frontend image.
    for lineno, line in enumerate(text.splitlines(), 1):
        m = SYNTAX_DIRECTIVE.match(line)
        if m:
            im = IMAGE_REF.fullmatch(m.group("ref"))
            if im:
                add_image(result, im.group("name"), im.group("tag"), im.group("digest"), f"{path}:{lineno}")
            else:
                result.errors.append(f"{path}:{lineno}: syntax directive must be name:tag@sha256:<digest>")
    stages: set[str] = set()
    froms = 0
    apt_used = False
    snapshots = 0
    for lineno, instruction, args in dockerfile_instructions(text):
        where = f"{path}:{lineno}"
        for m in REDIRECT_ENV.finditer(args):
            result.errors.append(f"{where}: {m.group(0)} can redirect what installers download; not allowed")
        if "BUILDKIT_SYNTAX" in args:
            result.errors.append(f"{where}: BUILDKIT_SYNTAX selects a frontend image; not allowed")
        if "<<" in args:
            result.errors.append(f"{where}: heredocs cannot be verified by the age gate; not allowed")
            continue
        if instruction == "FROM":
            froms += 1
            words = [w for w in args.split() if not w.startswith("--platform=")]
            if any(w.startswith("--") for w in words) or not words or "$" in args:
                result.errors.append(f"{where}: FROM must name a pinned image without variables or flags")
                continue
            image = words[0]
            if len(words) not in (1, 3) or (len(words) == 3 and words[1].lower() != "as"):
                result.errors.append(f"{where}: cannot parse FROM {args}")
                continue
            internal = image.lower() in stages or image == "scratch"
            if len(words) == 3:
                stages.add(words[2].lower())
            if internal:
                continue
            m = IMAGE_REF.fullmatch(image)
            if not m:
                result.errors.append(f"{where}: {image} must be name:tag@sha256:<digest>")
                continue
            add_image(result, m.group("name"), m.group("tag"), m.group("digest"), where)
        elif instruction in ("COPY", "ADD"):
            for source in re.findall(r"--from=(\S+)", args):
                check_from_source(source, stages, where, "--from=", result)
            if instruction == "ADD" and re.search(r"\b(https?|git|ssh)://|git@", args):
                check_add_url(args, where, result)  # only checksum-pinned GitHub release assets
        elif instruction == "RUN":
            for mount in re.findall(r"--mount=(\S+)", args):
                for option in mount.split(","):
                    key, _, value = option.partition("=")
                    if key == "from":
                        check_from_source(value, stages, where, "--mount from=", result)
            for problem in cargo_problems(args):
                result.errors.append(f"{where}: {problem}")
            # Package sources: only snapshot.debian.org at a fixed (dated) time.
            for url in URL.findall(args):
                snap = SNAPSHOT_URL.match(url) or SNAPSHOT_URL.match(url.rstrip("'\""))
                if not snap:
                    result.errors.append(f"{where}: URL {url} cannot be verified (only snapshot.debian.org/archive/…/<timestamp>)")
                    continue
                snapshots += 1
                stamp = snap.group("stamp")
                result.add("snapshot", f"snapshot.debian.org/{snap.group('archive')}", stamp,
                           lambda st=stamp: snapshot_published(st), where)
            if APT_INSTALL.search(args):
                apt_used = True
            allowed = check_apt_installs(args, where, result)
            allowed += [m.span() for pattern in DOCKER_ALLOWED for m in pattern.finditer(args)]
            for m in command_tokens(args):
                if not any(start <= m.start() < end for start, end in allowed):
                    result.errors.append(f"{where}: install/download command {m.group(0)!r} is not an allowed verified form")
            if re.search(r"\b(apt|aptitude)\s+install\b", args):
                result.errors.append(f"{where}: use apt-get install with pins")
        elif instruction == "ARG" and re.search(r"\bBUILDKIT_", args):
            result.errors.append(f"{where}: BuildKit control arguments are not allowed")
        elif instruction == "UNTERMINATED":
            result.errors.append(f"{where}: instruction ends with a line continuation")
        elif instruction == "ONBUILD":
            result.errors.append(f"{where}: ONBUILD cannot be verified")
    if froms == 0:
        result.errors.append(f"{path}: no FROM instruction; cannot be verified")
    if apt_used and snapshots == 0:
        result.errors.append(
            f"{path}: apt-get install needs package sources at a fixed snapshot.debian.org timestamp"
        )


COMPOSE_IMAGE = re.compile(r"^\s*(?:-\s+)?[\"']?image[\"']?\s*:\s*[\"']?(?P<ref>[^\s\"'#]*)[\"']?\s*(?:#.*)?$")
TOP_LEVEL_SERVICES = re.compile(r"^[\"']?services[\"']?\s*:", re.M)


def check_compose(path: Path, result: Result, dockerfiles: list[Path] | None = None) -> None:
    try:
        text = path.read_text()
    except (OSError, UnicodeDecodeError) as err:
        result.errors.append(f"{path}: cannot read: {err}")
        return
    if path.suffix.lower() == ".json":
        check_compose_json(path, text, result)
        return
    if not TOP_LEVEL_SERVICES.search(text):
        result.errors.append(f"{path}: no top-level services; cannot be verified")
    for lineno, line in enumerate(text.splitlines(), 1):
        where = f"{path}:{lineno}"
        if line.lstrip().startswith("#"):
            continue  # comments configure nothing
        if re.search(r"^\s*(?:-\s+)?[\"']?build[\"']?\s*:", line):
            result.errors.append(f"{where}: build: in compose files is not checked by the age gate")
        if re.search(r"^\s*(?:-\s+)?[\"']?(include|extends)[\"']?\s*:", line) or re.search(r"\b(include|extends)\b\s*:", line):
            result.errors.append(f"{where}: include/extends pull in other compose files; not allowed")
        ref = re.match(r"^\s*(?:-\s+)?[\"']?dockerfile[\"']?\s*:\s*[\"']?([^\s\"'#]*)", line)
        if ref:
            target = (path.parent / ref.group(1)).resolve()
            if target.is_file() and dockerfiles is not None:
                dockerfiles.append(target)
            else:
                result.errors.append(f"{where}: dockerfile {ref.group(1)!r} not found next to the compose file; cannot be verified")
        if not re.search(r"\bimage\b", line):
            continue
        m = COMPOSE_IMAGE.match(line)
        if not m:
            result.errors.append(f"{where}: unverifiable image line (only `image: name:tag@sha256:<digest>`)")
            continue
        ref = m.group("ref")
        own = ref.split("@", 1)[0].rsplit(":", 1)[0]
        if own in OWN_IMAGES:
            continue  # built from this repository
        im = IMAGE_REF.fullmatch(ref)
        if not im:
            result.errors.append(f"{where}: {ref or '(empty)'} must be name:tag@sha256:<digest>")
            continue
        add_image(result, im.group("name"), im.group("tag"), im.group("digest"), where)


def check_compose_json(path: Path, text: str, result: Result) -> None:
    """A compose file in JSON (valid YAML too): the same rules, structurally."""
    try:
        doc = json.loads(text)
    except ValueError as err:
        result.errors.append(f"{path}: cannot parse: {err}")
        return
    if not isinstance(doc, dict) or not isinstance(doc.get("services"), dict):
        result.errors.append(f"{path}: no top-level services; cannot be verified")
        return

    def walk(node: object, where: str) -> None:
        if isinstance(node, dict):
            for key, value in node.items():
                here = f"{path}: {where}.{key}"
                if key in ("include", "extends"):
                    result.errors.append(f"{here}: include/extends pull in other compose files; not allowed")
                elif key == "build":
                    result.errors.append(f"{here}: build: in compose files is not checked by the age gate")
                elif key == "image":
                    ref = value if isinstance(value, str) else ""
                    if ref.split("@", 1)[0].rsplit(":", 1)[0] in OWN_IMAGES:
                        continue
                    im = IMAGE_REF.fullmatch(ref)
                    if im:
                        add_image(result, im.group("name"), im.group("tag"), im.group("digest"), here)
                    else:
                        result.errors.append(f"{here}: {ref or '(empty)'} must be name:tag@sha256:<digest>")
                else:
                    walk(value, f"{where}.{key}")
        elif isinstance(node, list):
            for i, item in enumerate(node):
                walk(item, f"{where}[{i}]")

    walk(doc, "$")


YAML_EXAMPLE = re.compile(r"\.ya?ml(\.example|\.sample|\.dist)?$", re.I)


def is_compose(path: Path) -> bool:
    """Named like a compose file (also `….yml.example`), or any other YAML
    file (`.yml`, `.yml.example`, …) or JSON file with top-level services."""
    name = path.name.lower()
    if COMPOSE.match(name) or COMPOSE.match(re.sub(r"\.(example|sample|dist)$", "", name)):
        return True
    try:
        if YAML_EXAMPLE.search(name):
            return bool(TOP_LEVEL_SERVICES.search(path.read_text(errors="replace")))
        if name.endswith(".json"):
            doc = json.loads(path.read_text(errors="replace"))
            return isinstance(doc, dict) and "services" in doc
    except OSError:
        return True  # unreadable: let check_compose report it (fail closed)
    except ValueError:
        return False  # not JSON at all: not a compose file
    return False


# ── unsupported manifests (fail closed) ──────────────────────────────────────

UNSUPPORTED = re.compile(
    r"^(requirements.*\.txt|constraints.*\.txt|Pipfile(\.lock)?|pyproject\.toml|poetry\.lock|uv\.lock|setup\.py"
    r"|yarn\.lock|pnpm-lock\.yaml|bun\.lockb?|npm-shrinkwrap\.json|\.yarnrc(\.yml)?|\.pnpmfile\.cjs"
    r"|go\.(mod|sum)|Gemfile(\.lock)?|composer\.(json|lock)|deno\.(json|jsonc|lock)|pom\.xml|build\.gradle(\.kts)?"
    r"|Package\.resolved|Podfile\.lock|mix\.lock|flake\.lock|rust-toolchain|\.tool-versions|\.nvmrc|\.node-version"
    r"|\.python-version|Dockerfile.*|.*\.dockerfile|Containerfile|(docker-)?compose.*\.ya?ml)$"
)


DOCKERFILE = re.compile(r"^(dockerfile.*|.*\.dockerfile|containerfile.*)$", re.I)
COMPOSE = re.compile(r"^(docker-)?compose.*\.ya?ml$", re.I)


# ── main ─────────────────────────────────────────────────────────────────────


def walk(root: Path):
    for dirpath, dirnames, filenames in os.walk(root):
        here = Path(dirpath)
        dirnames[:] = sorted(
            d for d in dirnames
            if not (here == root and d in SKIP_DIRS)
            and not (d == "node_modules" and (here / "package-lock.json").is_file())
        )
        for f in sorted(filenames):
            yield here / f


def check_npmrc(path: Path, result: Result) -> None:
    """Only settings that change nothing about what npm installs (NPMRC_KEYS)."""
    try:
        lines = path.read_text().splitlines()
    except (OSError, UnicodeDecodeError) as err:
        result.errors.append(f"{path}: cannot read: {err}")
        return
    for lineno, line in enumerate(lines, 1):
        text = line.strip()
        if not text or text[0] in "#;":
            continue
        key = text.split("=", 1)[0].strip()
        if key not in NPMRC_KEYS:
            result.errors.append(f"{path}:{lineno}: .npmrc setting {key!r} is not on the allow-list "
                                 f"({', '.join(sorted(NPMRC_KEYS))}); it may change where packages come from")


def is_workspace_root(path: Path) -> bool:
    try:
        return "workspace" in tomllib.loads(path.read_text())
    except (tomllib.TOMLDecodeError, OSError):
        return True  # unreadable: treat as needing a lock (fail closed)


def evaluate(root: Path) -> Result:
    result = Result()
    root = root.resolve()
    files = list(walk(root))
    github = root / ".github"
    dockerfiles: list[Path] = []  # referenced by workflows and compose files
    checked: set[Path] = set()
    for path in files:
        name = path.name
        if path.parent.name == ".cargo" and name.lower().startswith("config"):
            result.errors.append(f"{path}: cargo configuration can replace registries and patch crates; not allowed")
        elif name == ".npmrc":
            check_npmrc(path, result)
        elif name == "Cargo.lock":
            check_cargo_lock(path, result)
        elif name == "Cargo.toml" and not (path.parent / "Cargo.lock").exists() and is_workspace_root(path):
            result.errors.append(f"{path}: workspace without Cargo.lock: commit the lock file")
        elif name == "package-lock.json":
            check_package_lock(path, result)
        elif name == "package.json" and not (path.parent / "package-lock.json").exists():
            result.errors.append(f"{path}: package.json without package-lock.json")
        elif name == "rust-toolchain.toml":
            check_rust_toolchain(path, result)
        elif DOCKERFILE.match(name):
            check_dockerfile(path, result)
            checked.add(path.resolve())
        elif github not in path.parents and is_compose(path):
            check_compose(path, result, dockerfiles)
        elif UNSUPPORTED.match(name):
            result.errors.append(f"{path}: dependency manifest the age gate cannot check yet; extend tools/dep-age.py")

    queue: list[Path] = [p for p in files if github in p.parents]
    seen: set[Path] = set()
    while queue:
        path = queue.pop(0).resolve()
        if path in seen:
            continue
        seen.add(path)
        scan_github_file(path, root, result, queue, dockerfiles)
    # Dockerfiles that are built, whatever their name.
    for path in dockerfiles:
        if path.resolve() not in checked:
            checked.add(path.resolve())
            check_dockerfile(path, result)
    return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--min-age-days", type=float, default=14.0)
    parser.add_argument("--now", help="evaluate as of this UTC time (ISO 8601), default: now")
    parser.add_argument("--verbose", "-v", action="store_true", help="list every checked item")
    parser.add_argument("--no-cache", action="store_true", help="look everything up again (no .cache/dep-age)")
    args = parser.parse_args(argv)
    global cache_enabled
    if args.no_cache:
        cache_enabled = False

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
