#!/usr/bin/env python3
"""Third-party notices of the two images (IMAGES): the vocab-server image
(THIRD-PARTY-NOTICES.txt) and the TTS image
(docker/tts-licenses/THIRD-PARTY-NOTICES.txt).

Usage:
  python3 tools/licenses.py            write both notices files
  python3 tools/licenses.py --check    fail if a notices file (or an image's
                                       licence label) is out of date
  python3 tools/licenses.py --label [--image tts]
                                       print an image's combined expression

The crates are those linked into `vocab-server` on the release targets
(x86_64/aarch64 musl): the normal (not dev, not build) dependencies of the
package, from `cargo metadata --locked --format-version 1`. For each crate
one licence is chosen from its SPDX expression (`A OR B` → the first of
PREFERENCE; `A AND B` → both), and the matching licence files are taken from
the crate's sources in the local cargo registry (the directory of its
manifest). A crate without a matching file gets the standard text of the
SPDX id, with the crate's authors as copyright holders (only MIT is known;
anything else is an error to resolve here). Identical texts are printed once.
Besides the crates, the server binary contains the Rust standard library,
musl libc, LLVM's libunwind (both linked statically by the musl targets) and
SQLite (bundled by libsqlite3-sys); they are listed as extras. The TTS
image's extras are what docker/Dockerfile.tts adds besides crates: the
sherpa-onnx static libraries and what they contain (ONNX Runtime and the
libraries ONNX Runtime links in, whose notices the image ships verbatim as
ORT_NOTICES; kaldi, OpenFst, sentencepiece, kissfft, utf8proc), LAME (bundled by mp3lame-sys),
the Kokoro model files and CMUdict; their licence texts come from the
crates' sources and from docker/tts-licenses/. For the TTS image, cargo
metadata runs with `RUSTFLAGS=--cfg vocab_tts_engine` (the engine's
dependencies exist only with that cfg, crates/vocab-tts/src/engine.rs).

The combined expression (all chosen licences joined with AND) is also the
image's `org.opencontainers.image.licenses` label; --check verifies that
each Dockerfile carries its image's expression.

Standard library only (and cargo). Self-tests: tools/tests/test_licenses.py.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = "THIRD-PARTY-NOTICES.txt"
DOCKERFILE = "docker/Dockerfile.server"
PACKAGE = "vocab-server"
TARGETS = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]
OWN_LICENSE = "MIT"
# ONNX Runtime's ThirdPartyNotices.txt, taken from its wheel by
# docker/Dockerfile.tts (--check verifies the Dockerfile ships it).
ORT_NOTICES = "/licenses/onnxruntime-ThirdPartyNotices.txt"


@dataclass(frozen=True)
class Image:
    """What one image ships: a package built for some targets, its
    Dockerfile (licence label) and its notices file."""

    name: str
    package: str
    targets: tuple[str, ...]
    dockerfile: str
    output: str
    title: str
    rustflags: str = ""


IMAGES = {
    "server": Image("server", PACKAGE, tuple(TARGETS), DOCKERFILE, OUTPUT,
                    "the vocab-server binary and container image"),
    "tts": Image("tts", "vocab-tts", ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"),
                 "docker/Dockerfile.tts", "docker/tts-licenses/THIRD-PARTY-NOTICES.txt",
                 "the vocab-tts binary and the TTS container image (vocabulary-training-tts)",
                 "--cfg vocab_tts_engine"),
}
# Deprecated SPDX ids some crates still use, and their current form.
DEPRECATED = {"LGPL-3.0": "LGPL-3.0-only", "LGPL-2.1": "LGPL-2.1-only", "GPL-3.0": "GPL-3.0-only"}

# Order of preference when a crate offers a choice (`OR`); a licence that is
# not listed is never chosen and makes the crate an error.
PREFERENCE = [
    "MIT",
    "Apache-2.0",
    "BSD-3-Clause",
    "Zlib",
    "Unicode-3.0",
    "Unlicense",
    "BSL-1.0",
    "MPL-2.0",
    "Apache-2.0 WITH LLVM-exception",
    # The TTS image only: CMUdict, LAME and its Rust bindings.
    "BSD-2-Clause",
    "LGPL-2.0-or-later",
    "LGPL-3.0-only",
]

MIT_TEXT = """\
MIT License

Copyright (c) {holders}

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the
following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial
portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO
EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
USE OR OTHER DEALINGS IN THE SOFTWARE.
"""

LLVM_EXCEPTION = """\
---- LLVM Exceptions to the Apache 2.0 License ----

As an exception, if, as a result of your compiling your source code, portions
of this Software are embedded into an Object form of such source code, you
may redistribute such embedded portions in such Object form without complying
with the conditions of Sections 4(a), 4(b) and 4(d) of the License.

In addition, if you combine or link compiled forms of this Software with
software that is licensed under the GPLv2 ("Combined Software") and if a
court of competent jurisdiction determines that the patent provision (Section
3), the indemnity provision (Section 9) or other Section of the License
conflicts with the conditions of the GPLv2, you may retroactively and
prospectively choose to deem waived or otherwise exclude such Section(s) of
the License, but only in their entirety and only with respect to the Combined
Software.
"""

SQLITE_TEXT = """\
SQLite is in the public domain (https://sqlite.org/copyright.html). Its
source code carries this notice in place of a licence:

  The author disclaims copyright to this source code.  In place of
  a legal notice, here is a blessing:

     May you do good and not evil.
     May you find forgiveness for yourself and forgive others.
     May you share freely, never taking more than you give.
"""


@dataclass
class Component:
    name: str
    version: str
    expression: str
    chosen: list[str]
    texts: list[str] = field(default_factory=list)
    note: str = ""


# ── SPDX expressions ─────────────────────────────────────────────────────────


def tokenize(expression: str) -> list[str]:
    """Tokens of an SPDX expression; the legacy `A/B` form means `A OR B`."""
    expression = expression.replace("/", " OR ")
    return [DEPRECATED.get(t, t) for t in re.findall(r"\(|\)|[A-Za-z0-9.+-]+", expression)]


def rank(ids: frozenset[str]) -> tuple[int, int]:
    worst = max((PREFERENCE.index(i) if i in PREFERENCE else len(PREFERENCE) for i in ids), default=0)
    return worst, len(ids)


def choose(expression: str) -> list[str]:
    """The licences we comply with: one option of every OR (the best by
    PREFERENCE), every operand of AND; `X WITH Y` stays one licence.
    Raises ValueError for an unparsable expression or when no acceptable
    option exists."""
    tokens = tokenize(expression)
    pos = 0

    def peek() -> str | None:
        return tokens[pos] if pos < len(tokens) else None

    def take() -> str:
        nonlocal pos
        if pos >= len(tokens):
            raise ValueError(f"unexpected end of licence expression {expression!r}")
        pos += 1
        return tokens[pos - 1]

    def primary() -> frozenset[str]:
        token = take()
        if token == "(":
            inner = disjunction()
            if take() != ")":
                raise ValueError(f"unbalanced parentheses in {expression!r}")
            return inner
        if token in (")", "AND", "OR", "WITH"):
            raise ValueError(f"unexpected {token!r} in {expression!r}")
        if peek() == "WITH":
            take()
            token = f"{token} WITH {take()}"
        return frozenset([token])

    def conjunction() -> frozenset[str]:
        ids = primary()
        while peek() == "AND":
            take()
            ids = ids | primary()
        return ids

    def disjunction() -> frozenset[str]:
        options = [conjunction()]
        while peek() == "OR":
            take()
            options.append(conjunction())
        return min(options, key=rank)

    chosen = disjunction()
    if pos != len(tokens):
        raise ValueError(f"cannot parse licence expression {expression!r}")
    unacceptable = sorted(i for i in chosen if i not in PREFERENCE)
    if unacceptable:
        raise ValueError(f"{expression!r}: no acceptable option (would need {', '.join(unacceptable)})")
    return sorted(chosen, key=PREFERENCE.index)


def combined(components: list[Component]) -> str:
    """The project's own licence first, then every chosen licence once."""
    ids = {i for c in components for i in c.chosen} - {OWN_LICENSE}
    return " AND ".join([OWN_LICENSE, *sorted(ids, key=PREFERENCE.index)])


# ── licence files ────────────────────────────────────────────────────────────

LICENSE_FILE = re.compile(
    r"^(licen[cs]e|copying|copyright|notice|unlicense|mit|apache|bsd|zlib|unicode|mpl|lgpl|gpl|bsl)"
    r"([-._].*)?$|^(MIT|Apache-2\.0|BSD-3-Clause|Zlib|Unicode-3\.0|MPL-2\.0|BSL-1\.0)(\.txt|\.md)?$",
    re.I,
)
NAME_HINTS = [
    (re.compile(r"mit", re.I), "MIT"),
    (re.compile(r"apache", re.I), "Apache-2.0"),
    (re.compile(r"bsd|httprouter", re.I), "BSD-3-Clause"),
    (re.compile(r"zlib", re.I), "Zlib"),
    (re.compile(r"unicode", re.I), "Unicode-3.0"),
    (re.compile(r"^unlicense", re.I), "Unlicense"),
    (re.compile(r"bsl|boost", re.I), "BSL-1.0"),
    (re.compile(r"mpl", re.I), "MPL-2.0"),
    (re.compile(r"gpl", re.I), "GPL"),
]
CONTENT_HINTS = [
    (re.compile(r"Permission is hereby granted, free of charge", re.I), "MIT"),
    (re.compile(r"Apache License,?\s+Version 2\.0", re.I), "Apache-2.0"),
    (re.compile(r"Redistribution and use in source and binary forms", re.I), "BSD-3-Clause"),
    (re.compile(r"provided ['‘]as-is['’]", re.I), "Zlib"),
    (re.compile(r"UNICODE LICENSE V3", re.I), "Unicode-3.0"),
    (re.compile(r"free and unencumbered software released into the public domain", re.I), "Unlicense"),
    (re.compile(r"Boost Software License", re.I), "BSL-1.0"),
    (re.compile(r"Mozilla Public License,?\s+(Version|v\.?)\s*2\.0", re.I), "MPL-2.0"),
    (re.compile(r"GNU LESSER GENERAL PUBLIC LICENSE\s+Version 3", re.I), "LGPL-3.0-only"),
    (re.compile(r"GNU LIBRARY GENERAL PUBLIC LICENSE\s+Version 2", re.I), "LGPL-2.0-or-later"),
    (re.compile(r"GNU (LESSER )?GENERAL PUBLIC LICENSE", re.I), "GPL"),
]


def classify(name: str, text: str) -> set[str]:
    """SPDX ids a licence file is the text of: from its name when it names
    one (LICENSE-MIT, MPL-2.0.txt), otherwise from its content."""
    if re.match(r"^notice", name, re.I):
        return {"NOTICE"}
    stem = re.sub(r"^(licen[cs]e|copying)[-._]?", "", name, flags=re.I)
    by_name = {spdx for pattern, spdx in NAME_HINTS if pattern.search(stem)}
    if by_name:
        return by_name
    return {spdx for pattern, spdx in CONTENT_HINTS if pattern.search(text)}


def texts_for(chosen: list[str], files: dict[str, str]) -> tuple[list[str], list[str]]:
    """(texts, licences without a file) for the chosen licences, from the
    crate's licence files {name: text}; NOTICE files go with Apache-2.0."""
    texts: list[str] = []
    missing: list[str] = []
    classes = {name: classify(name, text) for name, text in files.items()}
    for spdx in chosen:
        base = spdx.split(" WITH ", 1)[0]
        matching = [name for name in sorted(files) if base in classes[name]]
        if not matching:
            missing.append(spdx)
        for name in matching:
            if files[name] not in texts:
                texts.append(files[name])
        if base == "Apache-2.0":
            texts += [files[n] for n in sorted(files) if "NOTICE" in classes[n] and files[n] not in texts]
    return texts, missing


def standard_text(spdx: str, authors: list[str], name: str) -> str:
    if spdx != "MIT":
        raise ValueError(f"{name}: no {spdx} licence file in the crate and no standard text known; add one to tools/licenses.py")
    holders = ", ".join(re.sub(r"\s*<[^>]*>", "", a).strip() for a in authors) or f"the {name} authors"
    return MIT_TEXT.format(holders=holders)


# ── cargo ────────────────────────────────────────────────────────────────────


def cargo_metadata(root: Path, image: Image = IMAGES["server"]) -> dict:
    args = ["cargo", "metadata", "--locked", "--format-version", "1"]
    for target in image.targets:
        args += ["--filter-platform", target]
    env = {**os.environ, "RUSTFLAGS": image.rustflags}
    out = subprocess.run(args, cwd=root, check=True, capture_output=True, text=True, env=env).stdout
    return json.loads(out)


def shipped_packages(metadata: dict, package: str = PACKAGE) -> list[dict]:
    """Registry packages reachable from `package` over normal dependencies
    (proc-macros included: their output is in the binary), sorted."""
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    roots = [i for i in metadata["workspace_members"] if packages[i]["name"] == package]
    if len(roots) != 1:
        raise ValueError(f"package {package} not found in the workspace")
    seen: set[str] = set()
    stack = roots
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for dep in nodes[current]["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    shipped = []
    for pid in seen:
        pkg = packages[pid]
        if pkg["source"] is None:
            continue  # workspace member: the project's own licence
        if not str(pkg["source"]).startswith("registry+"):
            raise ValueError(f"{pkg['name']} {pkg['version']}: not from a registry ({pkg['source']})")
        shipped.append(pkg)
    return sorted(shipped, key=lambda p: (p["name"], p["version"]))


def licence_files(directory: Path) -> dict[str, str]:
    return {
        f.name: f.read_text(encoding="utf-8", errors="replace")
        for f in sorted(directory.iterdir())
        if f.is_file() and LICENSE_FILE.match(f.name)
    }


def crate_component(pkg: dict) -> Component:
    name, version = pkg["name"], pkg["version"]
    expression = pkg.get("license") or ""
    if not expression:
        raise ValueError(f"{name} {version}: no SPDX licence expression")
    chosen = choose(expression)
    texts, missing = texts_for(chosen, licence_files(Path(pkg["manifest_path"]).parent))
    texts += [standard_text(spdx, pkg.get("authors") or [], name) for spdx in missing]
    note = ""
    if "MPL-2.0" in chosen:
        note = (f"MPL-2.0: the Source Code Form of {name} {version} (unmodified) is available at "
                f"https://crates.io/crates/{name}/{version} and in its repository {pkg.get('repository') or '-'}.")
    return Component(name, version, expression, chosen, texts, note)


def extras(packages: list[dict]) -> list[Component]:
    sqlite = next((p for p in packages if p["name"] == "libsqlite3-sys"), None)
    found = [
        Component("Rust standard library", "(of the pinned Rust release)", "MIT OR Apache-2.0", ["MIT"],
                  [MIT_TEXT.format(holders="The Rust Project Contributors")]),
        Component("musl libc", "(linked statically by the Rust musl targets)", "MIT", ["MIT"],
                  [MIT_TEXT.format(holders="2005-2020 Rich Felker, et al.")]),
        Component("LLVM libunwind", "(linked statically by the Rust musl targets)", "Apache-2.0 WITH LLVM-exception",
                  ["Apache-2.0 WITH LLVM-exception"], [LLVM_EXCEPTION],
                  "LLVM libunwind: Apache-2.0 WITH LLVM-exception (https://llvm.org/LICENSE.txt). The exception (text "
                  "below) waives the attribution conditions for compiled forms; the Apache-2.0 text is printed below "
                  "for other components."),
    ]
    if sqlite:
        found.append(Component("SQLite", f"(bundled by libsqlite3-sys {sqlite['version']})", "public domain", [],
                               [SQLITE_TEXT]))
    return found


def tts_extras(packages: list[dict], root: Path) -> list[Component]:
    """What docker/Dockerfile.tts adds to the TTS image besides the crates."""
    by_name = {p["name"]: Path(p["manifest_path"]).parent for p in packages}
    for needed in ("sherpa-onnx", "mp3lame-sys"):
        if needed not in by_name:
            raise ValueError(f"{needed} is not among the TTS image's crates (RUSTFLAGS cfg missing?)")
    here = root / "docker" / "tts-licenses"

    def text(path: Path) -> str:
        return path.read_text(encoding="utf-8")

    apache = text(by_name["sherpa-onnx"] / "LICENSE")
    return [
        Component("Rust standard library", "(of the pinned Rust release)", "MIT OR Apache-2.0", ["MIT"],
                  [MIT_TEXT.format(holders="The Rust Project Contributors")]),
        Component("sherpa-onnx static libraries", "2.0.1", "Apache-2.0", ["Apache-2.0"], [apache]),
        Component("kaldi-native-fbank, kaldi-decoder, kaldifst, OpenFst, simple-sentencepiece",
                  "(in the sherpa-onnx static libraries)", "Apache-2.0", ["Apache-2.0"], [apache]),
        Component("ONNX Runtime", "1.28.2 (in the sherpa-onnx static libraries)", "MIT", ["MIT"],
                  [text(here / "onnxruntime-LICENSE.txt")]),
        # What ONNX Runtime's static library links in (checked against the
        # symbols of libonnxruntime.a 1.28.2); their licence texts and
        # copyright notices are ORT_NOTICES, shipped next to this file.
        Component("abseil, protobuf (with utf8_range), Eigen, re2, flatbuffers, ONNX, nlohmann/json, SafeInt, "
                  "date (H. Hinnant), Microsoft GSL, cpuinfo, Boost.Mp11, KleidiAI (arm64)",
                  "(as linked into ONNX Runtime 1.28.2)",
                  "Apache-2.0 AND BSD-3-Clause AND MPL-2.0 AND MIT AND BSD-2-Clause AND BSL-1.0",
                  ["MIT", "Apache-2.0", "BSD-3-Clause", "BSL-1.0", "MPL-2.0", "BSD-2-Clause"], [],
                  "ONNX Runtime's third-party libraries: their licence texts and copyright notices are in "
                  f"{ORT_NOTICES} (ONNX Runtime's ThirdPartyNotices.txt, verbatim). Eigen is MPL-2.0: its "
                  "unmodified source is the one ONNX Runtime 1.28.2 builds with (docker/tts-licenses/SOURCE.md)."),
        Component("kissfft", "(in the sherpa-onnx static libraries)", "BSD-3-Clause", ["BSD-3-Clause"],
                  [text(here / "kissfft-LICENSE.txt")]),
        Component("utf8proc", "2.12.0 (in the sherpa-onnx static libraries)", "MIT AND Unicode-3.0",
                  ["MIT", "Unicode-3.0"], [text(here / "utf8proc-LICENSE.md")]),
        Component("LAME", "3.100 (bundled by mp3lame-sys, linked statically)", "LGPL-2.0-or-later",
                  ["LGPL-2.0-or-later"], [text(by_name["mp3lame-sys"] / "lame-3.100" / "COPYING")],
                  "LAME: LGPL-2.0-or-later; its source is in the crate mp3lame-sys 0.1.11 "
                  "(https://crates.io/crates/mp3lame-sys/0.1.11); the binary can be relinked with a modified "
                  "LAME from this repository's sources (docker/tts-licenses/SOURCE.md)."),
        Component("Kokoro-82M v1.0 model, voices and US/UK lexicons", "(kokoro-multi-lang-v1_0)", "Apache-2.0",
                  ["Apache-2.0"], [apache]),
        Component("CMUdict", "(cmusphinx/cmudict at 74790861)", "BSD-2-Clause", ["BSD-2-Clause"],
                  [text(here / "cmudict-LICENSE.txt")]),
    ]


# ── output ───────────────────────────────────────────────────────────────────


def render(components: list[Component], title: str = IMAGES["server"].title) -> str:
    """The notices file: a table of components, then each distinct text
    once with the components it belongs to."""
    lines = [
        f"THIRD-PARTY NOTICES of {title}",
        "",
        "Generated by tools/licenses.py from Cargo.lock; do not edit by hand.",
        f"Combined licence expression: {combined(components)}",
        "(the project's own code: MIT, see LICENSE)",
        "",
        "Components (name, version, licence expression -> licence(s) used):",
        "",
    ]
    for c in components:
        used = " AND ".join(c.chosen) or c.expression
        lines.append(f"  {c.name} {c.version}: {c.expression} -> {used}")
    notes = [c.note for c in components if c.note]
    if notes:
        lines += ["", "Notes:", ""] + [f"  {n}" for n in notes]
    groups: dict[str, tuple[str, list[str]]] = {}
    for c in components:
        for text in c.texts:
            key = hashlib.sha256(" ".join(text.split()).encode()).hexdigest()
            groups.setdefault(key, (text, []))[1].append(f"{c.name} {c.version}")
    for text, users in groups.values():
        lines += ["", "=" * 78, "Used by: " + ", ".join(users), "=" * 78, "", text.rstrip("\n").replace("\r", "")]
    return "\n".join(lines) + "\n"


LABEL = re.compile(r'org\.opencontainers\.image\.licenses="(?P<value>[^"]*)"')


def label_problem(dockerfile_text: str, expression: str, dockerfile: str = DOCKERFILE) -> str | None:
    m = LABEL.search(dockerfile_text)
    if not m:
        return f"{dockerfile}: no org.opencontainers.image.licenses label"
    if m.group("value") != expression:
        return f'{dockerfile}: org.opencontainers.image.licenses="{m.group("value")}", expected "{expression}"'
    return None


def generate(root: Path, image: Image = IMAGES["server"]) -> tuple[str, str]:
    packages = shipped_packages(cargo_metadata(root, image), image.package)
    native = tts_extras(packages, root) if image.name == "tts" else extras(packages)
    components = [crate_component(p) for p in packages] + native
    return render(components, image.title), combined(components)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="fail if a notices file or a label is out of date")
    mode.add_argument("--label", action="store_true", help="print an image's combined licence expression")
    parser.add_argument("--image", choices=sorted(IMAGES), default="server", help="the image of --label")
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    images = [IMAGES[args.image]] if args.label else list(IMAGES.values())
    problems: list[str] = []
    for image in images:
        try:
            text, expression = generate(args.root, image)
        except (ValueError, OSError, subprocess.CalledProcessError) as err:
            detail = getattr(err, "stderr", "") or ""
            print(f"licenses ({image.name}): {err} {detail}".strip())
            return 1
        if args.label:
            print(expression)
            return 0
        target = args.root / image.output
        if args.check:
            if not target.is_file() or target.read_text(encoding="utf-8") != text:
                problems.append(f"{image.output} is out of date with Cargo.lock: run python3 tools/licenses.py "
                                "and commit it")
            dockerfile = args.root / image.dockerfile
            if dockerfile.is_file() and (problem := label_problem(dockerfile.read_text(), expression,
                                                                  image.dockerfile)):
                problems.append(problem)
            if image.name == "tts" and dockerfile.is_file() and ORT_NOTICES not in dockerfile.read_text():
                problems.append(f"{image.dockerfile}: does not ship {ORT_NOTICES}, which the notices refer to")
            print(f"licenses ({image.name}): combined licence {expression}")
            continue
        target.write_text(text, encoding="utf-8")
        print(f"licenses: wrote {image.output} ({len(text)} bytes); combined licence {expression}")
    if args.check:
        for problem in problems:
            print(f"FAIL: {problem}")
        print(f"licenses: {len(problems)} problem(s)")
        return 1 if problems else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
