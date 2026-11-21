#!/usr/bin/env python3
"""Smoke tests of the vocab-tts image (R-40, R-42, R-83; design §10.2, §13.2).

Usage: python3 tools/tts-smoke.py <image>

Runs the locally built image the way the Compose stack does: read-only root
filesystem, all capabilities dropped, no-new-privileges, as 65532:65532,
with memory (MEMORY_LIMIT, swap off), CPU and PID limits. The engine is
`docker`, which may be Podman (e.g. a docker -> podman shim on a Mac).
Checks:

* the image runs as 65532:65532 with entrypoint /vocab-tts and contains no
  shell, busybox, package manager or other program: the only ELF files are
  /vocab-tts and the shared libraries of the distroless base (filesystem
  listing; `--entrypoint /bin/sh` must fail);
* the model files (and CMUdict) are root-owned and read-only; /licenses
  holds the project's licence, the generated third-party notices and
  SOURCE.md;
* the container refuses to start with an unknown VOCAB_TTS_* variable;
* with `--network none` (no network at all: the model is in the image) it
  starts and its `healthcheck` subcommand passes;
* with only a port published on 127.0.0.1 (the test's way in; in Compose it
  is an internal network): `/healthz` answers 200, `/v1/voices` lists the US
  and UK voice, a sentence and a single word come back as valid MP3 (MPEG-2
  layer III frames, 24 kHz mono, duration from the frames matching
  `X-Duration-Ms` and plausible for the text, loudness reported at about
  −16 LUFS, a bitrate that is speech, not silence), the UK voice works, the
  longest allowed text (the engine's memory peak) works,
  oversize/invalid/markup/non-JSON input is refused with 4xx, the process
  runs as UID 65532, the container's memory stays under the limit (not
  OOM-killed) and the log never contains the spoken text.

Every check runs even after a failure; dependents of a failed check are
reported as skipped. Exit status non-zero if any check failed. Never pulls
anything (`--pull=never`). Texts are synthetic. Standard library only. In
GitHub Actions failures are also `::error` annotations and the results go to
the job summary (tools/gha.py).
"""

from __future__ import annotations

import io
import json
import os
import re
import secrets
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request

import gha

UID = "65532"
PORT = "8090"
SECURITY = ["--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--user", f"{UID}:{UID}"]
# The Compose limits (deploy/docker-compose.example.yml); the spike measured
# a peak of about 0.8 GiB (docs/design/tts-spike.md).
MEMORY_LIMIT = "1280m"
MEMORY_LIMIT_BYTES = 1280 * 1024 * 1024
LIMITS = ["--memory", MEMORY_LIMIT, "--memory-swap", MEMORY_LIMIT, "--cpus", "2", "--pids-limit", "64"]
LOG_TAIL = 30
TARGET_LUFS = -16.0
# Synthetic texts; a distinctive word to look for in the logs.
SENTENCE = "The purple lighthouse hums quietly above the sleeping harbour."
SECRET_WORD = "lighthouse"
WORD = "Wonderful"
LONG_TEXT = ("The quiet river wandered past the old mill, and the miller watched it from his window every evening. " * 6)[:600].strip()
FORBIDDEN_PATHS = frozenset([
    "bin/sh", "bin/bash", "bin/dash", "bin/ash", "bin/busybox", "usr/bin/busybox",
    "bin/dpkg", "usr/bin/dpkg", "usr/bin/apt", "usr/bin/apt-get", "sbin/apk", "usr/bin/apk",
    "usr/bin/yum", "usr/bin/dnf", "usr/bin/rpm", "usr/bin/pip", "usr/bin/python3",
    "usr/bin/perl", "usr/bin/curl", "usr/bin/wget",
])
ALLOWED_EXECUTABLE = "vocab-tts"
# Shared libraries of the distroless "cc" base (glibc, libstdc++, libgcc,
# libssl, …) and its dynamic loader: ELF, but not programs one can run.
SHARED_LIBRARY = re.compile(r"^(?:usr/)?lib(?:64)?/(?:[^/]+/)*(?:[^/]*\.so(?:\.[0-9.]+)?|ld-linux[^/]*)$")
ELF_MAGIC = b"\x7fELF"
MODEL_FILES = ["model.onnx", "voices.bin", "tokens.txt", "lexicon-us-en.txt", "lexicon-gb-en.txt", "cmudict.dict"]
LICENSE_FILES = ["LICENSE.txt", "THIRD-PARTY-NOTICES.txt", "SOURCE.md", "cmudict-LICENSE.txt"]

RESULTS: list[tuple[str, bool | None, str]] = []


class SmokeFailure(Exception):
    def __init__(self, name: str, reason: str) -> None:
        super().__init__(f"{name}: {reason}")
        self.name = name
        self.reason = reason


def one_line(text: str) -> str:
    return " ".join(text.split())


def failures() -> int:
    return sum(1 for _, ok, _ in RESULTS if ok is False)


def check(condition: bool, message: str, detail: str = "") -> bool:
    RESULTS.append((message, condition, detail))
    if condition:
        print(f"ok: {message}")
    else:
        gha.error(f"tts-smoke {message}", one_line(detail or "check failed"))
        print(f"FAIL: {message}: {one_line(detail)}")
    return condition


def skip(messages: list[str], reason: str) -> None:
    for message in messages:
        RESULTS.append((message, None, f"skipped: {reason}"))
        print(f"skipped: {message} ({reason})")


def guarded(section: str, action) -> bool:
    try:
        action()
        return True
    except SmokeFailure as err:
        check(False, f"{section}: {err.name}", err.reason)
        return False


def docker(*args: str, check: bool = True, timeout: int = 180) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)
    if check and result.returncode != 0:
        raise SmokeFailure(f"docker {' '.join(args[:2])}",
                           f"exit {result.returncode}: {one_line(result.stderr + result.stdout)[:500]}")
    return result


_engine: str | None = None


def engine() -> str:
    """'podman' when the `docker` command is Podman, else 'docker'."""
    global _engine
    if _engine is None:
        result = subprocess.run(["docker", "version"], capture_output=True, text=True, timeout=60)
        _engine = "podman" if "podman" in (result.stdout + result.stderr).lower() else "docker"
    return _engine


def top_args(name: str) -> list[str]:
    return ["top", name, "pid", "uid"] if engine() == "podman" else ["top", name, "-eo", "pid,uid"]


def process_uids(top_output: str) -> list[str]:
    rows = [line.split() for line in top_output.splitlines()[1:]]
    return [row[1] for row in rows if len(row) >= 2]


# ── pure helpers (unit-tested in tools/tests/test_tts_smoke.py) ─────────────

def image_programs(entries: list[tuple[str, bool, bytes]]) -> list[str]:
    """Programs that must not be in the image, from (tar name, is regular
    file, first bytes): anything at a FORBIDDEN_PATHS path, and any ELF file
    other than /vocab-tts that is not a shared library of the base image.
    Sorted, without duplicates."""
    found = set()
    for name, is_file, head in entries:
        path = os.path.normpath(name).lstrip("/")
        if path in FORBIDDEN_PATHS:
            found.add(path)
        elif is_file and head.startswith(ELF_MAGIC) and path != ALLOWED_EXECUTABLE and not SHARED_LIBRARY.match(path):
            found.add(path)
    return sorted(found)


BITRATES_V2 = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0]
RATES_V2 = [22050, 24000, 16000]


def mp3_frames(data: bytes) -> tuple[int, int, int]:
    """(frames, sample rate, duration ms) of an MPEG-2 layer III mono stream
    whose frames follow each other from byte 0 to the end (no ID3 tag);
    ValueError on anything else. The first frame (the encoder's Xing/Info
    tag) is counted; a frame is 576 samples."""
    pos = frames = rate = 0
    while pos < len(data):
        header = data[pos:pos + 4]
        if len(header) < 4 or header[0] != 0xFF or header[1] & 0xE0 != 0xE0:
            raise ValueError(f"no frame sync at byte {pos}")
        version, layer = (header[1] >> 3) & 3, (header[1] >> 1) & 3
        if version != 2 or layer != 1:
            raise ValueError(f"not MPEG-2 layer III at byte {pos}")
        bitrate = BITRATES_V2[header[2] >> 4]
        rate_index = (header[2] >> 2) & 3
        if bitrate == 0 or rate_index == 3:
            raise ValueError(f"invalid bitrate or sample rate at byte {pos}")
        frame_rate = RATES_V2[rate_index]
        if rate and frame_rate != rate:
            raise ValueError("sample rate changes")
        rate = frame_rate
        if (header[3] >> 6) != 3:
            raise ValueError(f"not mono at byte {pos}")
        pos += 72 * bitrate * 1000 // rate + ((header[2] >> 1) & 1)
        frames += 1
    if frames == 0:
        raise ValueError("no frames")
    if pos != len(data):
        raise ValueError("truncated last frame")
    return frames, rate, frames * 576 * 1000 // rate


def memory_bytes(text: str) -> int | None:
    """Bytes of the first size in a `stats` MemUsage cell such as
    "812.3MiB / 1.25GiB" or "812.3MB / 1.342GB"."""
    m = re.match(r"\s*([0-9.]+)\s*([kKMGT]?i?B)", text)
    if not m:
        return None
    units = {"B": 1, "KB": 10**3, "MB": 10**6, "GB": 10**9, "TB": 10**12,
             "KiB": 2**10, "MiB": 2**20, "GiB": 2**30, "TiB": 2**40, "kB": 10**3}
    unit = units.get(m.group(2))
    return int(float(m.group(1)) * unit) if unit else None


# ── HTTP ─────────────────────────────────────────────────────────────────────

OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def http(method: str, url: str, body: bytes | None = None,
         headers: dict[str, str] | None = None) -> tuple[int, dict[str, str], bytes]:
    request = urllib.request.Request(url, data=body, method=method, headers=headers or {})
    try:
        with OPENER.open(request, timeout=120) as response:
            return response.status, {k.lower(): v for k, v in response.headers.items()}, response.read()
    except urllib.error.HTTPError as err:
        return err.code, {k.lower(): v for k, v in err.headers.items()}, err.read()


def speak(base: str, payload: object, content_type: str = "application/json") -> tuple[int, dict[str, str], bytes]:
    body = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
    return http("POST", f"{base}/v1/speak", body, {"Content-Type": content_type})


def wait_healthy(base: str, name: str, timeout: float = 180) -> int:
    deadline = time.monotonic() + timeout
    status = 0
    while time.monotonic() < deadline:
        try:
            status = http("GET", f"{base}/healthz")[0]
        except (urllib.error.URLError, ConnectionError, OSError):
            status = 0
        if status == 200:
            break
        if docker("inspect", "--format", "{{.State.Running}}", name, check=False).stdout.strip() == "false":
            break
        time.sleep(0.5)
    return status


def container_log(name: str) -> str:
    logs = docker("logs", name, check=False)
    return logs.stdout + logs.stderr


# ── checks ───────────────────────────────────────────────────────────────────

def static_checks(image: str) -> None:
    config = json.loads(docker("image", "inspect", image).stdout)[0]["Config"]
    check(config.get("User") == f"{UID}:{UID}", f"image user is {UID}:{UID}", repr(config.get("User")))
    check(config.get("Entrypoint") == ["/vocab-tts"], "entrypoint is /vocab-tts", repr(config.get("Entrypoint")))
    labels = config.get("Labels") or {}
    check("LGPL-2.0-or-later" in labels.get("org.opencontainers.image.licenses", ""),
          "the licence label names the combined licences", repr(labels.get("org.opencontainers.image.licenses")))

    def listing() -> None:
        created = docker("create", "--pull=never", image).stdout.strip()
        try:
            exported = subprocess.run(["docker", "export", created], capture_output=True, timeout=300, check=True).stdout
        finally:
            docker("rm", "-f", created, check=False)
        entries = []
        members = {}
        with tarfile.open(fileobj=io.BytesIO(exported)) as tar:
            for m in tar.getmembers():
                member = tar.extractfile(m) if m.isreg() else None
                entries.append((m.name, m.isreg(), member.read(len(ELF_MAGIC)) if member else b""))
                members[os.path.normpath(m.name).lstrip("/")] = m
        present = image_programs(entries)
        check(not present, "no shell, package manager or other program in the filesystem", f"found: {present}")
        model = [members.get(f"model/{f}") for f in MODEL_FILES]
        check(all(m is not None and m.isreg() and m.uid == 0 and m.mode & 0o222 == 0 for m in model),
              "the model files are in the image, root-owned and read-only",
              repr([(f, m and oct(m.mode), m and m.uid) for f, m in zip(MODEL_FILES, model)]))
        missing = [f for f in LICENSE_FILES if f"licenses/{f}" not in members]
        check(not missing, "/licenses holds the licence texts and SOURCE.md", f"missing: {missing}")
        espeak = [p for p in members if "espeak" in p]
        check(not espeak, "no espeak-ng data in the image", repr(espeak[:5]))

    if not guarded("filesystem listing", listing):
        skip(["no shell, package manager or other program in the filesystem"], "the image could not be exported")
    for shell in ["/bin/sh", "/busybox/sh", "sh"]:
        result = docker("run", "--rm", "--pull=never", *SECURITY, "--entrypoint", shell, image, "-c", "true", check=False)
        check(result.returncode != 0, f"--entrypoint {shell} cannot run")


def refusal_checks(image: str) -> None:
    result = docker("run", "--rm", "--pull=never", *SECURITY, "-e", "VOCAB_TTS_TOKEN=x", image, check=False)
    output = result.stdout + result.stderr
    check(result.returncode != 0 and "unknown setting VOCAB_TTS_TOKEN" in output,
          "refuses to start with an unknown VOCAB_TTS_ variable", f"exit {result.returncode}: {one_line(output)[-300:]}")


def offline_checks(image: str, name: str) -> None:
    """No network at all: it starts (nothing is downloaded) and is healthy."""
    message = "with --network none it starts and the healthcheck subcommand passes"
    try:
        docker("run", "-d", "--pull=never", "--name", name, "--network", "none", *SECURITY, *LIMITS, image)
        deadline = time.monotonic() + 180
        result = None
        while time.monotonic() < deadline:
            result = docker("exec", name, "/vocab-tts", "healthcheck", check=False)
            if result.returncode == 0:
                break
            if docker("inspect", "--format", "{{.State.Running}}", name, check=False).stdout.strip() == "false":
                break
            time.sleep(1)
        check(result is not None and result.returncode == 0, message,
              one_line((result.stderr + result.stdout) if result else "") + " | " + one_line(container_log(name))[-300:])
    except SmokeFailure as err:
        check(False, message, err.reason)
    finally:
        docker("rm", "-f", name, check=False)


def api_checks(image: str, name: str) -> None:
    started = guarded("api container", lambda: docker(
        "run", "-d", "--pull=never", "--name", name, "-p", f"127.0.0.1::{PORT}", *SECURITY, *LIMITS, image))
    if not started:
        skip(["API checks"], "the container did not start")
        docker("rm", "-f", name, check=False)
        return

    def exercise() -> None:
        mapped = docker("port", name, f"{PORT}/tcp").stdout.strip().splitlines()
        if not check(bool(mapped), f"port {PORT} is published on 127.0.0.1", repr(mapped)):
            return
        base = f"http://127.0.0.1:{mapped[0].rsplit(':', 1)[1]}"
        status = wait_healthy(base, name)
        if not check(status == 200, "/healthz answers 200", f"last status {status}"):
            return
        status, _, body = http("GET", f"{base}/v1/voices")
        ids = [v.get("id") for v in json.loads(body).get("voices", [])] if status == 200 else []
        check(ids == ["af_heart", "bf_emma"], "/v1/voices lists the US and the UK voice", f"{status} {ids}")

        for label, payload, min_ms, max_ms in [
            ("a sentence (US voice)", {"text": SENTENCE}, 2500, 8000),
            ("a single word (US voice)", {"text": WORD}, 400, 2000),
            ("a sentence (UK voice)", {"text": SENTENCE, "voice": "bf_emma"}, 2500, 8000),
            # The longest text there is: the memory peak of the engine.
            ("the longest text (600 characters)", {"text": LONG_TEXT}, 20000, 60000),
        ]:
            status, headers, body = speak(base, payload)
            if not check(status == 200 and headers.get("content-type") == "audio/mpeg",
                         f"{label}: 200 audio/mpeg", f"{status} {headers.get('content-type')} {body[:200]!r}"):
                continue
            try:
                frames, rate, frames_ms = mp3_frames(body)
                problem = ""
            except ValueError as err:
                frames, rate, frames_ms, problem = 0, 0, 0, str(err)
            check(not problem and rate == 24000, f"{label}: valid MP3 frames, 24 kHz mono", problem or f"{rate} Hz")
            stated = int(headers.get("x-duration-ms", "0"))
            # Frames also hold the encoder delay and the tag frame (≈ 3 frames).
            check(min_ms <= stated <= max_ms and abs(frames_ms - stated) <= 150,
                  f"{label}: plausible duration",
                  f"X-Duration-Ms {stated}, frames {frames_ms} ms, expected {min_ms}-{max_ms}")
            lufs = float(headers.get("x-loudness-lufs", "nan"))
            check(abs(lufs - TARGET_LUFS) <= 1.0, f"{label}: loudness about {TARGET_LUFS:g} LUFS", f"{lufs}")
            kbps = len(body) * 8 / max(stated, 1)
            check(16 <= kbps <= 128, f"{label}: bitrate of speech, not silence", f"{kbps:.1f} kbit/s")

        refused = [
            ("oversize body", speak(base, {"text": "a" * 5000}), 413),
            ("text over 600 characters", speak(base, {"text": "word " * 130}), 400),
            ("control character", speak(base, {"text": "bell\u0007"}), 400),
            ("markup (SSML)", speak(base, {"text": "<speak>hi</speak>"}), 400),
            ("unknown voice", speak(base, {"text": "hi", "voice": "xx_none"}), 400),
            ("unknown field", speak(base, {"text": "hi", "ssml": True}), 400),
            ("not JSON", speak(base, b"text=hi", "application/x-www-form-urlencoded"), 415),
            ("nothing speakable", speak(base, {"text": "?!"}), 422),
        ]
        for label, (status, _, body), expected in refused:
            check(status == expected, f"refused: {label} ({expected})", f"{status} {body[:200]!r}")

        result = docker("exec", name, "/vocab-tts", "healthcheck", check=False)
        check(result.returncode == 0, "the healthcheck subcommand passes", one_line(result.stderr + result.stdout))
        uids = process_uids(docker(*top_args(name)).stdout)
        check(bool(uids) and all(u == UID for u in uids), f"the process runs as UID {UID}", f"got {uids}")
        state = json.loads(docker("inspect", name).stdout)[0]
        check(state["HostConfig"].get("ReadonlyRootfs") is True, "the root filesystem is read-only")
        check(state["State"].get("Running") is True and not state["State"].get("OOMKilled"),
              f"still running, not OOM-killed under {MEMORY_LIMIT}", repr(state["State"]))
        usage = docker("stats", "--no-stream", "--format", "{{.MemUsage}}", name, check=False).stdout.strip()
        used = memory_bytes(usage)
        check(used is not None and used < MEMORY_LIMIT_BYTES, f"memory use under {MEMORY_LIMIT}", usage)
        print(f"memory use: {usage}")
        logs = container_log(name)
        check(SECRET_WORD not in logs.lower() and "spoke" in logs, "the log has the speak lines but never the text",
              one_line(logs)[-300:])

    try:
        guarded("api", exercise)
    finally:
        print(gha.untrusted_block("container log (tail):", "\n".join(container_log(name).splitlines()[-LOG_TAIL:])),
              flush=True)
        docker("rm", "-f", name, check=False)


def smoke(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__)
        return 2
    image = argv[0]
    run_id = secrets.token_hex(4)
    guarded("static checks", lambda: static_checks(image))
    guarded("refusal", lambda: refusal_checks(image))
    offline_checks(image, f"tts-smoke-offline-{run_id}")
    api_checks(image, f"tts-smoke-{run_id}")
    if failures():
        print(f"TTS image smoke tests FAILED: {failures()} failed check(s)")
        return 1
    print("TTS image smoke tests passed")
    return 0


def main(argv: list[str]) -> int:
    crash: str | None = None
    try:
        return smoke(argv)
    except Exception as err:  # any crash must become an annotation too
        crash = f"{type(err).__name__}: {err}"
        gha.error("tts-smoke crashed", one_line(crash))
        raise
    finally:
        if RESULTS or crash:
            rows = list(RESULTS)
            if crash:
                rows.append(("(aborted)", False, crash))
            gha.append_summary(gha.summary_table("TTS image smoke tests", rows))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
