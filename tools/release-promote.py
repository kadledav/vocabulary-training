#!/usr/bin/env python3
"""Promote a pushed release image after testing exactly that image (design §12.4).

Usage: python3 tools/release-promote.py <repository> <sha256:digest> <X.Y.Z>

The release workflow builds the image once (with provenance and SBOM) and
pushes it only under its `sha-<short>` tag. This script then

1. pulls that image back by digest — the bytes that were pushed;
2. runs tools/image-smoke.py on it;
3. adds the version tag to the same manifest (`imagetools create` with a
   single source re-tags it; nothing is rebuilt) and checks that the version
   tag now resolves to the tested digest.

If any step fails, the version tag is not created. Standard library only;
needs a `docker login` to the registry.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def run(*args: str) -> str:
    print("+", " ".join(args), flush=True)
    result = subprocess.run(list(args), capture_output=True, text=True, timeout=900)
    if result.returncode != 0:
        raise SystemExit(f"failed ({result.returncode}):\n{result.stderr}{result.stdout}")
    return result.stdout


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(__doc__)
        return 2
    repository, digest, version = argv
    if not re.fullmatch(r"ghcr\.io/[a-z0-9._/-]+", repository):
        raise SystemExit(f"unexpected repository {repository}")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise SystemExit(f"not a digest: {digest}")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise SystemExit(f"not a version: {version}")
    pinned = f"{repository}@{digest}"

    run("docker", "pull", pinned)
    smoke = subprocess.run([sys.executable, str(HERE / "image-smoke.py"), pinned], timeout=1800)
    if smoke.returncode != 0:
        raise SystemExit("smoke tests of the pushed image failed; the version tag was not created")

    tagged = f"{repository}:{version}"
    run("docker", "buildx", "imagetools", "create", "--tag", tagged, pinned)
    resolved = run("docker", "buildx", "imagetools", "inspect", tagged, "--format", "{{json .Manifest.Digest}}")
    if resolved.strip().strip('"') != digest:
        raise SystemExit(f"{tagged} resolves to {resolved.strip()}, not to the tested {digest}")
    print(f"promoted {tagged} = {pinned}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
