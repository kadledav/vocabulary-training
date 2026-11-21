#!/usr/bin/env python3
"""Release steps of .github/workflows/release.yml (design §12.4).

  release-promote.py prepare --repository R --tag vX.Y.Z --commit SHA
      Before anything is built: the tag is exactly vMAJOR.MINOR.PATCH, equals
      the workspace version in Cargo.toml, and its commit is an ancestor of
      origin/main. Looks the version tag up in the registry: if R:X.Y.Z
      exists, the release was published before (a re-run) and its digest is
      returned as `existing` (nothing is built again). Outputs (to
      $GITHUB_OUTPUT): version, short, existing.

  release-promote.py smoke --repository R --digest sha256:… --platform linux/arm64
      Pulls the per-platform image that was just pushed (by digest, untagged)
      on a runner of that platform, checks its architecture and runs
      tools/image-smoke.py on it. A failure says in the job summary that the
      pushed digest stays in the registry untagged and nothing was tagged.

  release-promote.py publish --repository R --version X.Y.Z --short abc1234 --digest D1 --digest D2
      Merges the tested per-platform images into one multi-platform index
      under `sha-<short>` (`imagetools create`, no rebuild), checks that the
      index holds exactly the tested manifests, then adds the version tag to
      that index. An existing version tag is never moved: the same digest is
      fine (a re-run), any other digest is an error. Output: digest.

  release-promote.py notes --repository R --tag vX.Y.Z --version X.Y.Z --digest sha256:…
      Writes the reference `R:X.Y.Z@digest` to the job summary and to the
      GitHub release notes (creates the release, or adds the reference to an
      existing one once; a re-run does not repeat it). Uses `gh`.

Registry lookups use the registry API with GHCR_USER / GITHUB_TOKEN (pull
scope); docker commands need a `docker login`. Every failure is an `::error`
annotation plus a line in the job summary (tools/gha.py). Standard library
only. Self-tests of the pure parts: tools/tests/test_release_promote.py.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

import gha

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
TAG = re.compile(r"v(?P<version>(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
REPOSITORY = re.compile(r"ghcr\.io/[a-z0-9._-]+/[a-z0-9._/-]+")
PLATFORM = re.compile(r"linux/(?P<arch>amd64|arm64)")
INDEX_TYPES = ", ".join([
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
])


class PromoteFailure(Exception):
    """A failed step: `step` names it, the message says why (one line)."""

    def __init__(self, step: str, reason: str) -> None:
        super().__init__(reason)
        self.step = step


# ── pure checks (unit-tested) ────────────────────────────────────────────────


def version_of_tag(tag: str) -> str:
    """X.Y.Z of a tag that is exactly vX.Y.Z (no suffix, no leading zeros)."""
    m = TAG.fullmatch(tag)
    if not m:
        raise PromoteFailure("tag", f"tag {tag!r} is not vMAJOR.MINOR.PATCH (e.g. v0.1.0, no suffix)")
    return m.group("version")


def workspace_version(cargo_toml: str) -> str:
    try:
        version = tomllib.loads(cargo_toml)["workspace"]["package"]["version"]
    except (tomllib.TOMLDecodeError, KeyError, TypeError) as err:
        raise PromoteFailure("version", f"no [workspace.package] version in Cargo.toml: {err}") from err
    return str(version)


def check_tag_matches(tag: str, cargo_toml: str) -> str:
    version = version_of_tag(tag)
    workspace = workspace_version(cargo_toml)
    if workspace != version:
        raise PromoteFailure("version", f"tag {tag} does not match the workspace version {workspace} in Cargo.toml")
    return version


def manifest_state(status: int, body: bytes) -> str:
    """'present' (200), 'absent' (404 with MANIFEST_UNKNOWN/NAME_UNKNOWN or
    no error body); anything else is an error (fail closed)."""
    if status == 200:
        return "present"
    if status == 404:
        try:
            codes = {e.get("code") for e in json.loads(body or b"{}").get("errors", [])}
        except (ValueError, AttributeError):
            codes = set()
        if not codes or codes & {"MANIFEST_UNKNOWN", "NAME_UNKNOWN", "NOT_FOUND"}:
            return "absent"
    raise PromoteFailure("registry", f"unexpected registry answer HTTP {status}: {body[:300]!r}")


def tag_decision(existing: str | None, digest: str, tagged: str) -> bool:
    """Whether the tag still has to be created: no (it already names the
    digest, a re-run), yes (it does not exist); an existing tag naming
    another digest is never moved."""
    if existing is None:
        return True
    if existing == digest:
        return False
    raise PromoteFailure(
        "existing tag",
        f"{tagged} already exists as {existing}; refusing to move it to {digest}. A published version is "
        "immutable: release a new version instead",
    )


def manifest_digests(raw: str) -> list[str]:
    """Digests of the manifests in an image index (`imagetools inspect --raw`)."""
    doc = json.loads(raw)
    if "manifests" not in doc:
        raise PromoteFailure("verify index", "not an image index")
    return [m["digest"] for m in doc["manifests"]]


def merged_problems(merged_raw: str, sources_raw: list[str]) -> list[str]:
    """The merged index must hold exactly the manifests (images and their
    attestations) of the tested per-platform indexes."""
    merged = set(manifest_digests(merged_raw))
    tested = {d for raw in sources_raw for d in manifest_digests(raw)}
    problems = [f"untested manifest {d} in the merged index" for d in sorted(merged - tested)]
    problems += [f"tested manifest {d} missing from the merged index" for d in sorted(tested - merged)]
    return problems


def platforms_of(raw: str) -> list[str]:
    """`os/arch` of the platform images of an index (attestations excluded)."""
    doc = json.loads(raw)
    return sorted(
        f"{m['platform']['os']}/{m['platform']['architecture']}"
        for m in doc.get("manifests", [])
        if m.get("platform", {}).get("os") not in (None, "unknown")
    )


def notes_body(existing: str | None, reference: str) -> str | None:
    """The release notes with the image reference, or None if `existing`
    already has it (a re-run must not repeat it)."""
    block = (
        "Server image (linux/amd64, linux/arm64), deploy by tag and digest:\n\n"
        f"    {reference}\n\n"
        "If this release adds a database migration, take a ZFS snapshot first (deploy/README.md).\n"
    )
    if existing is None:
        return block
    if reference in existing:
        return None
    return f"{existing.rstrip()}\n\n{block}" if existing.strip() else block


# ── commands and registry ────────────────────────────────────────────────────


def run(step: str, *args: str, timeout: int = 900) -> str:
    print("+", " ".join(args), flush=True)
    result = subprocess.run(list(args), capture_output=True, text=True, timeout=timeout)
    if result.returncode != 0:
        output = " ".join((result.stderr + result.stdout).split())
        raise PromoteFailure(step, f"{' '.join(args[:3])} exited {result.returncode}: {output[-500:]}")
    return result.stdout


def http(url: str, headers: dict[str, str], method: str = "GET") -> tuple[int, dict[str, str], bytes]:
    request = urllib.request.Request(url, headers={"User-Agent": "release-promote", **headers}, method=method)
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            return response.status, dict(response.headers.items()), response.read()
    except urllib.error.HTTPError as err:
        return err.code, dict(err.headers.items()) if err.headers else {}, err.read()
    except (urllib.error.URLError, TimeoutError) as err:
        raise PromoteFailure("registry", f"{url}: {err}") from err


def registry_digest(repository: str, tag: str) -> str | None:
    """Digest the tag names in the registry, or None if it does not exist."""
    host, _, name = repository.partition("/")
    user = os.environ.get("GHCR_USER", "")
    token = os.environ.get("GITHUB_TOKEN", "")
    headers = {}
    if token:
        headers["Authorization"] = "Basic " + base64.b64encode(f"{user or 'x'}:{token}".encode()).decode()
    scope = urllib.parse.urlencode({"scope": f"repository:{name}:pull", "service": host})
    status, _, body = http(f"https://{host}/token?{scope}", headers)
    if status != 200:
        if status in (401, 403) and not package_exists(name):
            return None  # the package does not exist yet (first release)
        raise PromoteFailure("registry", f"no pull token for {repository}: HTTP {status}: {body[:200]!r}")
    bearer = json.loads(body).get("token", "")
    status, response_headers, body = http(
        f"https://{host}/v2/{name}/manifests/{urllib.parse.quote(tag)}",
        {"Authorization": f"Bearer {bearer}", "Accept": INDEX_TYPES},
        method="HEAD",
    )
    if manifest_state(status, body) == "absent":
        return None
    digest = {k.lower(): v for k, v in response_headers.items()}.get("docker-content-digest", "")
    if not DIGEST.fullmatch(digest):
        raise PromoteFailure("registry", f"{repository}:{tag}: no digest in the registry answer")
    return digest


def package_exists(name: str) -> bool:
    """Whether the GHCR package exists at all (GitHub packages API); only a
    404 means it does not."""
    owner, _, package = name.partition("/")
    token = os.environ.get("GITHUB_TOKEN", "")
    headers = {"Accept": "application/vnd.github+json", **({"Authorization": f"Bearer {token}"} if token else {})}
    for kind in ("users", "orgs"):
        status, _, body = http(
            f"https://api.github.com/{kind}/{owner}/packages/container/{urllib.parse.quote(package, safe='')}", headers
        )
        if status == 200:
            return True
        if status != 404:
            raise PromoteFailure("registry", f"cannot tell whether package {name} exists: HTTP {status}: {body[:200]!r}")
    return False


def ancestor_of_main(commit: str) -> None:
    if subprocess.run(["git", "rev-parse", "--verify", "--quiet", "origin/main"], cwd=ROOT,
                      capture_output=True).returncode != 0:
        raise PromoteFailure("ancestry", "origin/main is not available (checkout with fetch-depth: 0)")
    result = subprocess.run(["git", "merge-base", "--is-ancestor", commit, "origin/main"], cwd=ROOT,
                            capture_output=True, text=True)
    if result.returncode == 1:
        raise PromoteFailure("ancestry", f"the tagged commit {commit} is not on main (not an ancestor of origin/main)")
    if result.returncode != 0:
        raise PromoteFailure("ancestry", f"git merge-base failed: {result.stderr.strip()}")


# ── subcommands ──────────────────────────────────────────────────────────────


def prepare(args: argparse.Namespace) -> None:
    version = check_tag_matches(args.tag, (ROOT / "Cargo.toml").read_text())
    if not re.fullmatch(r"[0-9a-f]{40}", args.commit):
        raise PromoteFailure("arguments", f"not a commit SHA: {args.commit}")
    ancestor_of_main(args.commit)
    existing = registry_digest(args.repository, version) or ""
    if existing:
        print(f"{args.repository}:{version} exists already ({existing}): nothing is built again")
        gha.append_summary(f"### Release\n\nℹ️ `{args.repository}:{version}` exists already as `{existing}`; "
                           "this run only completes the release notes.\n")
    for key, value in (("version", version), ("short", args.commit[:7]), ("existing", existing)):
        gha.set_output(key, value)
    print(f"release {args.tag}: version {version}, commit {args.commit[:7]}, existing {existing or '-'}")


def smoke(args: argparse.Namespace) -> None:
    pinned = f"{args.repository}@{args.digest}"
    arch = PLATFORM.fullmatch(args.platform).group("arch")  # type: ignore[union-attr]
    try:
        run("pull", "docker", "pull", "--platform", args.platform, pinned)
        got = run("architecture", "docker", "image", "inspect", "--format", "{{.Architecture}}", pinned).strip()
        if got != arch:
            raise PromoteFailure("architecture", f"{pinned} is {got}, expected {arch}")
        # image-smoke.py prints its own annotations and summary to the same log.
        result = subprocess.run([sys.executable, str(HERE / "image-smoke.py"), pinned], timeout=1800)
        if result.returncode != 0:
            raise PromoteFailure("smoke tests", f"smoke tests of the pushed {args.platform} image failed")
    except PromoteFailure as err:
        raise PromoteFailure(
            err.step,
            f"{err} — the pushed digest {args.digest} stays in the registry untagged (not deleted); nothing was "
            "tagged and no version was published",
        ) from err
    gha.append_summary(f"### {args.platform}\n\n✅ smoke tests passed for `{pinned}`\n")


def publish(args: argparse.Namespace) -> None:
    tagged = f"{args.repository}:{args.version}"
    sources = [f"{args.repository}@{d}" for d in args.digest]
    sources_raw = [run("inspect source", "docker", "buildx", "imagetools", "inspect", "--raw", s) for s in sources]
    platforms = sorted(p for raw in sources_raw for p in platforms_of(raw))
    if platforms != ["linux/amd64", "linux/arm64"]:
        raise PromoteFailure("platforms", f"the tested images are {platforms}, expected linux/amd64 and linux/arm64")
    commit_tag = f"{args.repository}:sha-{args.short}"
    run("merge", "docker", "buildx", "imagetools", "create", "--tag", commit_tag, *sources)
    merged_raw = run("verify index", "docker", "buildx", "imagetools", "inspect", "--raw", commit_tag)
    problems = merged_problems(merged_raw, sources_raw)
    if problems:
        raise PromoteFailure("verify index", "; ".join(problems))
    digest = registry_digest(args.repository, f"sha-{args.short}")
    if digest is None:
        raise PromoteFailure("verify index", f"{commit_tag} is not in the registry after the merge")
    if tag_decision(registry_digest(args.repository, args.version), digest, tagged):
        # One source index: the tag is added to that same manifest (no rebuild).
        run("tag", "docker", "buildx", "imagetools", "create", "--tag", tagged, f"{args.repository}@{digest}")
    resolved = registry_digest(args.repository, args.version)
    if resolved != digest:
        raise PromoteFailure("verify tag", f"{tagged} resolves to {resolved}, not to the tested index {digest}")
    gha.set_output("digest", digest)
    print(f"published {tagged} = {args.repository}@{digest}")
    gha.append_summary(f"### Release promotion\n\n✅ `{tagged}` = `{args.repository}@{digest}` (linux/amd64, linux/arm64)\n")


def notes(args: argparse.Namespace) -> None:
    reference = f"{args.repository}:{args.version}@{args.digest}"
    view = subprocess.run(["gh", "release", "view", args.tag, "--json", "body", "--jq", ".body"],
                          capture_output=True, text=True)
    existing = view.stdout if view.returncode == 0 else None
    if existing is None and "not found" not in (view.stderr or "").lower():
        raise PromoteFailure("release notes", f"gh release view failed: {view.stderr.strip()[:300]}")
    body = notes_body(existing, reference)
    gha.append_summary(f"### Release notes\n\n`{reference}`\n")
    if body is None:
        print(f"release {args.tag} already names {reference}")
    elif existing is None:
        run("release notes", "gh", "release", "create", args.tag, "--title", args.tag, "--notes", body, "--verify-tag")
    else:
        run("release notes", "gh", "release", "edit", args.tag, "--notes", body)


def parse(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    def repository(value: str) -> str:
        if not REPOSITORY.fullmatch(value):
            raise argparse.ArgumentTypeError(f"unexpected repository {value}")
        return value

    def digest(value: str) -> str:
        if not DIGEST.fullmatch(value):
            raise argparse.ArgumentTypeError(f"not a digest: {value}")
        return value

    def version(value: str) -> str:
        if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", value):
            raise argparse.ArgumentTypeError(f"not a version: {value}")
        return value

    def short_sha(value: str) -> str:
        if not re.fullmatch(r"[0-9a-f]{7}", value):
            raise argparse.ArgumentTypeError(f"not a short commit SHA: {value}")
        return value

    def platform(value: str) -> str:
        if not PLATFORM.fullmatch(value):
            raise argparse.ArgumentTypeError(f"unsupported platform: {value}")
        return value

    p = sub.add_parser("prepare")
    p.add_argument("--repository", type=repository, required=True)
    p.add_argument("--tag", required=True)
    p.add_argument("--commit", required=True)
    p = sub.add_parser("smoke")
    p.add_argument("--repository", type=repository, required=True)
    p.add_argument("--digest", type=digest, required=True)
    p.add_argument("--platform", type=platform, required=True)
    p = sub.add_parser("publish")
    p.add_argument("--repository", type=repository, required=True)
    p.add_argument("--version", type=version, required=True)
    p.add_argument("--short", type=short_sha, required=True)
    p.add_argument("--digest", type=digest, action="append", required=True)
    p = sub.add_parser("notes")
    p.add_argument("--repository", type=repository, required=True)
    p.add_argument("--tag", required=True)
    p.add_argument("--version", type=version, required=True)
    p.add_argument("--digest", type=digest, required=True)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse(argv)
    try:
        {"prepare": prepare, "smoke": smoke, "publish": publish, "notes": notes}[args.command](args)
        return 0
    except PromoteFailure as err:
        gha.error(f"release-promote {args.command} {err.step}", str(err))
        gha.append_summary(f"### Release ({args.command})\n\n❌ {err.step}: {err}\n")
        print(f"FAIL: {err.step}: {err}")
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
