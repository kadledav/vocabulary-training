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
  longest allowed text works, half of it at the slowest speed works, the
  worst texts the API accepts (long hyphenated compounds, long numbers,
  long unknown words spelled out, texts dense with pauses, each up to its
  limit, at normal and at the slowest speed) are spoken as clips the app
  keeps (≤ 60 s) or refused with 422 and never kill the service, overlong
  words and numbers are refused with 422, oversize/invalid/markup/non-JSON
  input is refused with 4xx, the process runs as UID 65532 and the log
  never contains the spoken text;
* memory (the model is loaded only while there is work, in an engine
  process that ends when idle): after the start self-test and again after
  the idle time (VOCAB_TTS_IDLE_SECONDS, short here) `/healthz` says the
  engine is stopped and the container's anonymous memory (cgroup
  `memory.stat`) is under IDLE_ANON_LIMIT. While the requests run, the
  anonymous memory is sampled: its peak must leave at least ANON_MARGIN to
  MEMORY_LIMIT; the kernel's `memory.events` must show no OOM kill (the
  engine process is a child: Docker's `OOMKilled` stays false when only it
  dies); the model's file cache must stay in memory (`memory.stat`
  `workingset_refault_file`: at most REFAULT_LIMIT pages read again). The
  cgroup numbers (current, peak, anon, file, refaults) are printed at each
  step; where the cgroup cannot be read (not Linux, not Docker or Podman)
  the memory checks are skipped, not failed;
* stopping (`docker stop`) during the start self-test and during a
  synthesis ends the container within STOP_LIMIT seconds with exit code 0,
  and the request in progress is answered `503 shutting_down`.

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
import threading
import time
import urllib.error
import urllib.request

import gha

UID = "65532"
PORT = "8090"
SECURITY = ["--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--user", f"{UID}:{UID}"]
# The Compose limits (deploy/docker-compose.example.yml; measured peak and
# margin in docs/design/tts-spike.md, "Small memory").
MEMORY_LIMIT = "640m"
MEMORY_LIMIT_BYTES = 640 * 1024 * 1024
# The sampled anonymous memory must stay this far under the limit (the
# kernel kills the engine process when anonymous memory and the file cache
# it cannot drop reach the limit).
ANON_MARGIN = 1.15
# Pages of files read again after the kernel dropped them from the cache:
# the model is 114 MB (~28 000 pages) and is read for every chunk, so this
# is far below one full re-read during the whole run.
REFAULT_LIMIT = 4000
# `docker stop` must end the container this fast (the Compose grace period
# is longer).
STOP_LIMIT = 10
LIMITS = ["--memory", MEMORY_LIMIT, "--memory-swap", MEMORY_LIMIT, "--cpus", "1", "--pids-limit", "64"]
# The engine process ends after this long without work (the default is
# minutes; short here so the test sees it).
IDLE_SECONDS = 5
# Anonymous memory of the container with no engine process: the HTTP front
# alone (a few MiB).
IDLE_ANON_LIMIT = 48 * 1024 * 1024
LOG_TAIL = 30
TARGET_LUFS = -16.0
# Synthetic texts; a distinctive word to look for in the logs.
SENTENCE = "The purple lighthouse hums quietly above the sleeping harbour."
SECRET_WORD = "lighthouse"
WORD = "Wonderful"
LONG_TEXT = ("The quiet river wandered past the old mill, and the miller watched it from his window every evening. " * 6)[:600].strip()
# The slowest speed the API accepts.
SLOWEST = 0.5
# The longest clip the app keeps (vocab-server `MAX_CLIP_MS`).
MAX_CLIP_MS = 60_000


def up_to_600(token: str) -> str:
    """`token` repeated, separated by spaces, as often as fits 600 characters."""
    return " ".join([token] * ((600 + 1) // (len(token) + 1)))


# The worst texts the API accepts: every token at its limit (64 characters,
# 15 digits), as many as fit. They must come back as audio or as 422
# `too_long_to_speak`, never as a dead service.
COMPOUND = "-".join(["internationalization"] * 3)  # 62 characters
SPELLED = "zxqvbnmkjhgfdcwplrtsxqvbnmkjhgfdcwplrtszxqvbnmkjhgfdcwplrtszxqvb"  # 64, spelled out
WORST_TEXTS = [
    ("hyphenated compounds up to the token limit", up_to_600(COMPOUND)),
    ("one 15-digit number", "999999999999999"),
    ("15-digit numbers up to 600 characters", up_to_600("999999999999999")),
    ("one 64-letter unknown word", SPELLED),
    ("64-letter unknown words up to 600 characters", up_to_600(SPELLED)),
    ("a comma after every word, 600 characters", up_to_600("a,")),
    ("one-word sentences, 600 characters", up_to_600("Oh.")),
]
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
MODEL_FILES = ["model.int8.onnx", "voices.bin", "tokens.txt", "lexicon-us-en.txt", "lexicon-gb-en.txt", "cmudict.dict"]
LICENSE_FILES = ["LICENSE.txt", "THIRD-PARTY-NOTICES.txt", "SOURCE.md", "cmudict-LICENSE.txt",
                 "onnxruntime-ThirdPartyNotices.txt"]

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


def cgroup_numbers(current: str, peak: str | None, stat: str, events: str | None = None) -> dict[str, int]:
    """Numbers of a cgroup v2 memory controller: bytes of `memory.current`,
    `memory.peak` (when the kernel has it), `anon` / `file` of
    `memory.stat`; counts of `workingset_refault_file` (pages read again
    after they were dropped from the cache) and `oom_kill` of
    `memory.events` (when given)."""
    numbers = {"current": int(current.strip())}
    if peak is not None and peak.strip():
        numbers["peak"] = int(peak.strip())
    for line in stat.splitlines():
        key, _, value = line.partition(" ")
        if key in ("anon", "file"):
            numbers[key] = int(value)
        elif key == "workingset_refault_file":
            numbers["refaults"] = int(value)
    for line in (events or "").splitlines():
        key, _, value = line.partition(" ")
        if key == "oom_kill":
            numbers["oom_kill"] = int(value)
    return numbers


def mib(numbers: dict[str, int]) -> str:
    return ", ".join(f"{key} {value}" if key in ("refaults", "oom_kill") else f"{key} {value / 2**20:.0f} MiB"
                     for key, value in numbers.items())


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


# Where a container's cgroup is: Docker (systemd and cgroupfs drivers), Podman.
CGROUP_DIRS = ["/sys/fs/cgroup/system.slice/docker-{id}.scope", "/sys/fs/cgroup/docker/{id}",
               "/sys/fs/cgroup/machine.slice/libpod-{id}.scope"]


def read_cgroup(container: str) -> dict[str, int] | None:
    """The container's cgroup memory numbers, read on this machine (Linux)
    or, for Podman on a Mac, inside the Podman machine; None if unreadable."""
    cid = docker("inspect", "--format", "{{.Id}}", container, check=False).stdout.strip()
    if not re.fullmatch(r"[0-9a-f]{64}", cid):
        return None
    for template in CGROUP_DIRS:
        path = template.format(id=cid)
        files = [f"{path}/memory.current", f"{path}/memory.stat", f"{path}/memory.peak", f"{path}/memory.events"]
        if sys.platform == "linux":
            if not os.path.isfile(files[0]):
                continue
            try:
                texts = [open(f).read() if os.path.isfile(f) else None for f in files]
            except OSError:
                continue
        elif engine() == "podman":
            # memory.current must exist (this is the container's cgroup); the
            # others may be missing on older kernels.
            script = f"cat {files[0]} && echo === && " + " && echo === && ".join(
                f"(cat {f} 2>/dev/null || true)" for f in files[1:])
            result = docker("machine", "ssh", script, check=False, timeout=60)
            if result.returncode != 0 or "===" not in result.stdout:
                continue
            texts = result.stdout.split("===\n")
        else:
            return None
        try:
            texts += [None] * (4 - len(texts))
            return cgroup_numbers(texts[0], texts[2], texts[1], texts[3])
        except (ValueError, TypeError, IndexError, AttributeError):
            return None
    return None


def engine_state(base: str) -> str:
    """`engine` of /healthz: "running" or "stopped" (or what went wrong)."""
    try:
        status, _, body = http("GET", f"{base}/healthz")
        return json.loads(body).get("engine", "?") if status == 200 else f"status {status}"
    except (urllib.error.URLError, ConnectionError, OSError, ValueError) as err:
        return f"unreachable: {err}"


def memory_check(name: str, when: str, idle: bool) -> None:
    """Print the cgroup numbers; when `idle`, the anonymous memory must be
    the front's alone."""
    numbers = read_cgroup(name)
    if numbers is None:
        print(f"memory {when}: cgroup not readable here; docker stats: "
              f"{docker('stats', '--no-stream', '--format', '{{.MemUsage}}', name, check=False).stdout.strip()}")
        if idle:
            skip([f"memory {when}: anonymous memory under {IDLE_ANON_LIMIT >> 20} MiB"], "cgroup not readable here")
        return
    print(f"memory {when}: {mib(numbers)}")
    if idle:
        check(numbers.get("anon", IDLE_ANON_LIMIT) < IDLE_ANON_LIMIT,
              f"memory {when}: anonymous memory under {IDLE_ANON_LIMIT >> 20} MiB", mib(numbers))


class MemorySampler:
    """Samples the container's cgroup in the background (as often as it can
    be read): the highest anonymous memory and file cache seen."""

    def __init__(self, name: str) -> None:
        self.name = name
        self.max_anon = 0
        self.max_file = 0
        self.samples = 0
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def _run(self) -> None:
        while not self._stop.is_set():
            numbers = read_cgroup(self.name)
            if numbers is None:
                return
            self.samples += 1
            self.max_anon = max(self.max_anon, numbers.get("anon", 0))
            self.max_file = max(self.max_file, numbers.get("file", 0))
            self._stop.wait(0.1)

    def start(self) -> "MemorySampler":
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stop.set()
        self._thread.join(timeout=120)


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
        "run", "-d", "--pull=never", "--name", name, "-p", f"127.0.0.1::{PORT}", *SECURITY, *LIMITS,
        "-e", f"VOCAB_TTS_IDLE_SECONDS={IDLE_SECONDS}", image))
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
        state = engine_state(base)
        check(state == "stopped", "after the start self-test the engine process is stopped", state)
        memory_check(name, "after the start (idle)", idle=True)
        at_start = read_cgroup(name) or {}
        sampler = MemorySampler(name).start()

        for label, payload, min_ms, max_ms in [
            ("a sentence (US voice)", {"text": SENTENCE}, 2500, 8000),
            ("a single word (US voice)", {"text": WORD}, 400, 2000),
            ("a sentence (UK voice)", {"text": SENTENCE, "voice": "bf_emma"}, 2500, 8000),
            # The longest text there is: the memory peak of the engine.
            ("the longest text (600 characters)", {"text": LONG_TEXT}, 20000, 60000),
            # Slower speech makes more audio per chunk: the chunks are shorter.
            ("half the longest text at the slowest speed", {"text": LONG_TEXT[:300].strip(), "speed": SLOWEST},
             20000, 55000),
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

        for speed in (1.0, SLOWEST):
            for label, text in WORST_TEXTS:
                label = f"{label} at speed {speed:g}"
                started = time.monotonic()
                status, headers, body = speak(base, {"text": text, "speed": speed})
                took = time.monotonic() - started
                code = ""
                if status == 422:
                    code = json.loads(body).get("error", {}).get("code", "")
                audio_ms = int(headers.get("x-duration-ms", "0") or 0)
                check(status == 200 and headers.get("content-type") == "audio/mpeg" and 0 < audio_ms <= MAX_CLIP_MS
                      or code == "too_long_to_speak",
                      f"worst case: {label} -> a clip of at most {MAX_CLIP_MS // 1000} s or 422 too_long_to_speak",
                      f"{status} {code} {audio_ms} ms {body[:200] if status != 200 else ''!r}")
                print(f"worst case: {label}: {status} {code} {headers.get('x-duration-ms', '-')} ms of audio "
                      f"in {took:.1f} s")
                status, _, _ = http("GET", f"{base}/healthz")
                check(status == 200, f"worst case: {label}: the service is still up", f"/healthz {status}")

        refused = [
            ("oversize body", speak(base, {"text": "a" * 5000}), 413),
            ("a 590-digit number (token)", speak(base, {"text": "9" * 590}), 422),
            ("20 hyphenated words (token)", speak(base, {"text": "-".join(["internationalization"] * 20)}), 422),
            ("a 16-digit number", speak(base, {"text": "call 1234567890123456"}), 422),
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

        sampler.stop()
        memory_check(name, "after all requests", idle=False)
        peak_checks(name, sampler, at_start)
        deadline = time.monotonic() + IDLE_SECONDS + 30
        while (state := engine_state(base)) != "stopped" and time.monotonic() < deadline:
            time.sleep(1)
        check(state == "stopped", f"the engine process ends after {IDLE_SECONDS} s without work", state)
        memory_check(name, "after the idle time", idle=True)
        result = docker("exec", name, "/vocab-tts", "healthcheck", check=False)
        check(result.returncode == 0, "the healthcheck subcommand passes", one_line(result.stderr + result.stdout))
        uids = process_uids(docker(*top_args(name)).stdout)
        check(bool(uids) and all(u == UID for u in uids), f"the process runs as UID {UID}", f"got {uids}")
        state = json.loads(docker("inspect", name).stdout)[0]
        check(state["HostConfig"].get("ReadonlyRootfs") is True, "the root filesystem is read-only")
        check(state["State"].get("Running") is True and not state["State"].get("OOMKilled"),
              f"still running, not OOM-killed under {MEMORY_LIMIT}", repr(state["State"]))
        logs = container_log(name)
        check(SECRET_WORD not in logs.lower() and "spoke" in logs, "the log has the speak lines but never the text",
              one_line(logs)[-300:])

    try:
        guarded("api", exercise)
    finally:
        print(gha.untrusted_block("container log (tail):", "\n".join(container_log(name).splitlines()[-LOG_TAIL:])),
              flush=True)
        docker("rm", "-f", name, check=False)


def peak_checks(name: str, sampler: MemorySampler, at_start: dict[str, int]) -> None:
    """The memory of the requests, from the cgroup: sampled anonymous peak,
    OOM kills, file pages read again."""
    names = [f"memory: anonymous peak at most 1/{ANON_MARGIN:g} of {MEMORY_LIMIT}",
             "memory: no OOM kill (memory.events oom_kill)",
             f"memory: the model stays in the file cache (at most {REFAULT_LIMIT} pages read again)"]
    numbers = read_cgroup(name)
    if numbers is None or sampler.samples == 0:
        skip(names, "cgroup not readable here")
        return
    print(f"memory while speaking: {sampler.samples} samples, anon peak {sampler.max_anon / 2**20:.0f} MiB, "
          f"file cache peak {sampler.max_file / 2**20:.0f} MiB, memory.peak "
          f"{numbers.get('peak', 0) / 2**20:.0f} MiB, limit {MEMORY_LIMIT}")
    check(sampler.max_anon * ANON_MARGIN <= MEMORY_LIMIT_BYTES, names[0],
          f"anon peak {sampler.max_anon / 2**20:.0f} MiB")
    if "oom_kill" in numbers:
        check(numbers["oom_kill"] == 0, names[1], f"oom_kill {numbers['oom_kill']}")
    else:
        skip(names[1:2], "memory.events not readable here")
    if "refaults" in numbers and "refaults" in at_start:
        refaults = numbers["refaults"] - at_start["refaults"]
        check(refaults <= REFAULT_LIMIT, names[2], f"{refaults} pages read again")
    else:
        skip(names[2:], "workingset_refault_file not readable here")


def stop_checks(image: str, name: str) -> None:
    """`docker stop` during the start self-test, and during a synthesis."""
    def stop_timed() -> tuple[float, str]:
        started = time.monotonic()
        docker("stop", "-t", "30", name, timeout=60)
        took = time.monotonic() - started
        code = docker("inspect", "--format", "{{.State.ExitCode}}", name, check=False).stdout.strip()
        return took, code

    def during_start() -> None:
        docker("run", "-d", "--pull=never", "--name", name, *SECURITY, *LIMITS, image)
        time.sleep(0.5)
        took, code = stop_timed()
        check(took < STOP_LIMIT and code == "0", "docker stop during the start self-test: exits promptly with 0",
              f"{took:.1f} s, exit {code} | {one_line(container_log(name))[-300:]}")

    def during_synthesis() -> None:
        docker("run", "-d", "--pull=never", "--name", name, "-p", f"127.0.0.1::{PORT}", *SECURITY, *LIMITS, image)
        mapped = docker("port", name, f"{PORT}/tcp").stdout.strip().splitlines()
        base = f"http://127.0.0.1:{mapped[0].rsplit(':', 1)[1]}"
        if wait_healthy(base, name) != 200:
            raise SmokeFailure("stop during a synthesis", "the container did not get healthy")
        answer: list[object] = []
        request = threading.Thread(target=lambda: answer.append(_speak_or_error(base, {"text": LONG_TEXT})))
        request.start()
        time.sleep(3)
        took, code = stop_timed()
        request.join(timeout=60)
        status, error_code = answer[0] if answer else (0, "no answer")
        check(took < STOP_LIMIT and code == "0", "docker stop during a synthesis: exits promptly with 0",
              f"{took:.1f} s, exit {code}")
        check((status, error_code) == (503, "shutting_down"),
              "docker stop during a synthesis: the request is answered 503 shutting_down", f"{status} {error_code}")

    for section in (during_start, during_synthesis):
        try:
            guarded("stop", section)
        finally:
            docker("rm", "-f", name, check=False)


def _speak_or_error(base: str, payload: object) -> tuple[int, str]:
    """Status and error code of a speak request (0 and the error when the
    connection failed)."""
    try:
        status, _, body = speak(base, payload)
    except (urllib.error.URLError, ConnectionError, OSError) as err:
        return 0, str(err)
    if status == 200:
        return status, ""
    try:
        return status, json.loads(body).get("error", {}).get("code", "")
    except ValueError:
        return status, "not JSON"


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
    stop_checks(image, f"tts-smoke-stop-{run_id}")
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
