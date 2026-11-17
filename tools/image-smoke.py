#!/usr/bin/env python3
"""Smoke tests of the vocab-server image (R-83, R-82; design §13.2, §14.2).

Usage: python3 tools/image-smoke.py <image>

Runs the locally built image the way the Compose stack does (read-only root
filesystem, all capabilities dropped, no-new-privileges, no tmpfs at all,
configuration from VOCAB_* variables): once as the image's user 65532 on a
fresh named volume, once as the deployment's user 3101 on a bind-mounted
directory owned by 3101 (needs passwordless sudo, as on CI runners). Checks:

* the image runs as 65532:65532 and contains no shell, busybox, package
  manager or other program but /vocab-server (filesystem listing, where
  package metadata such as var/lib/dpkg/status.d is fine, and
  `--entrypoint /bin/sh` must fail);
* the container refuses to start without VOCAB_PUBLIC_ORIGIN and with a
  leftover secret variable of M5 (`VOCAB_PASSWORD_HASH_B64`);
* `/healthz` answers 200 and the built-in `healthcheck` subcommand passes;
* first run: the API needs a session, the setup code is read from
  `docker logs` (one WARN line), a wrong code is refused, setup with the
  right code sets the password and logs in, setup is refused afterwards,
  login with that password works, a setting can be saved (a database
  write), and the process runs as the requested UID;
* with only `/data` mounted (owned by 3101), the app creates its cache
  directory there itself (mode 0700, owner 3101).

Every check runs even after a failure; a check whose prerequisite failed
(container not started, no session) is reported as skipped. The exit status
is non-zero if any check failed.

Never pulls anything (`--pull=never`): the image must have been built
locally. The password is generated per run (`test-` prefix). Standard library
only.

In GitHub Actions every failure is also an `::error` annotation (readable
through the public API without a login) together with the tail of the
container's log, and the results go to the job summary (tools/gha.py).
The server speaks plain HTTP here; its session cookie is `Secure`, so the
cookie is taken from `Set-Cookie` and sent back by hand (a cookie jar would
drop it on http://).
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
import tempfile
import time
import urllib.error
import urllib.request

import gha

UID = "65532"
ORIGIN = "https://vocab.example.home"
# The deployment's own user (deploy/README.md), owner of its data directory.
DEPLOY_UID = "3101"
SECURITY = ["--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges"]
HARDENING = [*SECURITY, "--user", f"{UID}:{UID}"]
SESSION_COOKIE = "__Host-vocab_session"
# Lines of the container log shown with a failure.
LOG_TAIL = 30
SETUP_LINE = re.compile(r"WARN Setup required: open the app and enter setup code ([0-9A-Z]{4}-[0-9A-Z]{4}-[0-9A-Z]{2})$", re.M)
# Programs that must not exist in the image (paths in the `docker export` tar,
# without a leading slash). Metadata such as var/lib/dpkg/status.d/* or
# etc/dpkg is not a program and is fine.
FORBIDDEN_PATHS = frozenset([
    "bin/sh", "bin/bash", "bin/dash", "bin/ash", "bin/busybox", "usr/bin/busybox",
    "bin/dpkg", "usr/bin/dpkg", "usr/bin/apt", "usr/bin/apt-get", "sbin/apk", "usr/bin/apk",
    "usr/bin/yum", "usr/bin/dnf", "usr/bin/rpm", "usr/bin/pip", "usr/bin/python3",
    "usr/bin/perl", "usr/bin/curl", "usr/bin/wget",
])
# The only program the image may hold.
ALLOWED_EXECUTABLE = "vocab-server"


class SmokeFailure(Exception):
    """A failed check: `name` is the check, `reason` one line about why."""

    def __init__(self, name: str, reason: str) -> None:
        super().__init__(f"{name}: {reason}")
        self.name = name
        self.reason = reason


# (check, passed, detail) of this run, for the job summary; passed is None
# for a skipped check.
RESULTS: list[tuple[str, bool | None, str]] = []


def failures() -> int:
    return sum(1 for _, ok, _ in RESULTS if ok is False)


def image_programs(entries: list[tuple[str, int, bool]]) -> list[str]:
    """Programs that must not be in the image, from its filesystem listing
    as (tar name, mode, is regular file): anything at a FORBIDDEN_PATHS path
    (a symlink such as bin/sh -> busybox counts), and any regular file with an
    executable bit other than /vocab-server. Sorted, without duplicates."""
    found = set()
    for name, mode, is_file in entries:
        path = os.path.normpath(name).lstrip("/")
        if path in FORBIDDEN_PATHS or (is_file and mode & 0o111 and path != ALLOWED_EXECUTABLE):
            found.add(path)
    return sorted(found)


def one_line(text: str) -> str:
    return " ".join(text.split())


def docker(*args: str, check: bool = True, stdin: str | None = None) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], input=stdin, capture_output=True, text=True, timeout=120)
    if check and result.returncode != 0:
        raise SmokeFailure(f"docker {' '.join(args[:2])}",
                           f"exit {result.returncode}: {one_line(result.stderr + result.stdout)[:500]}")
    return result


def check(condition: bool, message: str, detail: str = "") -> bool:
    """Record one check and report a failure as an annotation, without
    stopping; returns whether it passed so that dependents can be skipped."""
    RESULTS.append((message, condition, detail))
    if condition:
        print(f"ok: {message}")
    else:
        gha.error(f"image-smoke {message}", one_line(detail or "check failed"))
        print(f"FAIL: {message}: {one_line(detail)}")
    return condition


def skip(messages: list[str], reason: str) -> None:
    """Record checks that cannot run because a prerequisite failed."""
    for message in messages:
        RESULTS.append((message, None, f"skipped: {reason}"))
        print(f"skipped: {message} ({reason})")


def guarded(section: str, action) -> bool:
    """Run `action`; a SmokeFailure (a docker/sudo command failed) is
    recorded as a failed check named `section`. Returns whether it ran."""
    try:
        action()
        return True
    except SmokeFailure as err:
        check(False, f"{section}: {err.name}", err.reason)
        return False


def session_cookie(headers: list[tuple[str, str]]) -> str | None:
    """The `name=value` pair of the session cookie among the response's
    `Set-Cookie` headers (attributes such as `Secure` are ignored: the test
    talks plain HTTP to 127.0.0.1 and sends the cookie back by hand)."""
    for name, value in headers:
        if name.lower() != "set-cookie":
            continue
        pair = value.split(";", 1)[0].strip()
        key, sep, token = pair.partition("=")
        if sep and key == SESSION_COOKIE and token:
            return pair
    return None


def container_log(name: str) -> str:
    """The last lines of a container's log (stdout and stderr)."""
    logs = docker("logs", "--tail", str(LOG_TAIL), name, check=False)
    return "\n".join((logs.stdout + logs.stderr).splitlines()[-LOG_TAIL:])


def run_args(image_user: str, data: str, env: dict[str, str], origin: bool = True) -> list[str]:
    """`docker run` arguments as Compose runs the app (no tmpfs at all)."""
    args = ["run", "--pull=never", *SECURITY, "--user", f"{image_user}:{image_user}", "-v", f"{data}:/data"]
    if origin:
        args += ["-e", f"VOCAB_PUBLIC_ORIGIN={ORIGIN}"]
    for key, value in env.items():
        args += ["-e", f"{key}={value}"]
    return args


# No proxies (a runner's *_proxy variables must not reroute 127.0.0.1) and no
# redirects or cookie handling: every request is exactly what the test sends.
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def http(method: str, url: str, body: dict | None = None,
         headers: dict[str, str] | None = None) -> tuple[int, list[tuple[str, str]], bytes]:
    """One request; returns status, all response headers (repeated ones
    kept) and the body. HTTP error statuses are returned, not raised."""
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method, headers=headers or {})
    try:
        with OPENER.open(request, timeout=10) as response:
            return response.status, list(response.headers.items()), response.read()
    except urllib.error.HTTPError as err:
        return err.code, list(err.headers.items()), err.read()


def json_of(body: bytes) -> object:
    try:
        return json.loads(body)
    except ValueError:
        return None


def sudo(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    """A host command as root (the CI runner has passwordless sudo)."""
    result = subprocess.run(["sudo", "-n", *args], capture_output=True, text=True, timeout=60)
    if check and result.returncode != 0:
        raise SmokeFailure(f"sudo {args[0]}", f"exit {result.returncode}: {one_line(result.stderr)[:500]}")
    return result


def wait_healthy(base: str, name: str, timeout: float = 90) -> int:
    """Poll /healthz until 200, the container stops, or `timeout` passes."""
    deadline = time.monotonic() + timeout
    status = 0
    while time.monotonic() < deadline:
        try:
            status = http("GET", f"{base}/healthz")[0]
        except (urllib.error.URLError, ConnectionError, OSError):
            status = 0
        if status == 200:
            break
        running = docker("inspect", "--format", "{{.State.Running}}", name, check=False).stdout.strip()
        if running == "false":
            break
        time.sleep(0.5)
    return status


def serve_and_check(image: str, name: str, user: str, data: str, password: str) -> None:
    """Start the app hardened as `user` on `data`, then check health, the
    non-root process, the session gate, first-run setup, login and a write.
    Every check runs unless one it depends on failed (then it is skipped)."""
    tag = f"[uid {user}]"
    gates = {  # checks that need a prerequisite, by prerequisite
        "port": [f"{tag} /healthz answers 200"],
        "http": [f"{tag} the API needs a session", f"{tag} first start is in setup state",
                 f"{tag} the setup code is in the log once"],
        "code": [f"{tag} a wrong setup code is refused", f"{tag} setup sets the password and logs in",
                 f"{tag} setup is refused once a password is set"],
        "setup": [f"{tag} a wrong password is refused", f"{tag} login works"],
        "login": [f"{tag} the session opens the API", f"{tag} a setting is saved"],
    }
    failures_before = failures()
    args = run_args(user, data, {})
    started = guarded(tag, lambda: docker(*args[:1], "-d", "--name", name, "-p", "127.0.0.1::8080", *args[1:], image))
    if not started:
        skip([f"{tag} port 8080 is published on 127.0.0.1", f"{tag} the healthcheck subcommand passes inside the container",
              f"{tag} the process runs as UID {user}", *[n for names in gates.values() for n in names]],
             "the container did not start")
        docker("rm", "-f", name, check=False)
        return

    def exercise() -> None:
        # `docker port` prints e.g. `127.0.0.1:49153` (one line per binding).
        mapped = docker("port", name, "8080/tcp").stdout.strip().splitlines()
        published = check(bool(mapped), f"{tag} port 8080 is published on 127.0.0.1", repr(mapped))
        healthy = False
        base = ""
        if published:
            base = f"http://127.0.0.1:{mapped[0].rsplit(':', 1)[1]}"
            status = wait_healthy(base, name)
            healthy = check(status == 200, f"{tag} /healthz answers 200", f"last status {status}")
        else:
            skip(gates["port"], "port is not published")
        result = docker("exec", name, "/vocab-server", "healthcheck", check=False)
        check(result.returncode == 0, f"{tag} the healthcheck subcommand passes inside the container",
              one_line(result.stderr + result.stdout))
        # `docker top` needs a PID column to find the container's processes.
        rows = [line.split() for line in docker("top", name, "-eo", "pid,uid").stdout.splitlines()[1:]]
        uids = [row[1] for row in rows if len(row) >= 2]
        check(bool(uids) and all(u == user for u in uids), f"{tag} the process runs as UID {user}", f"got {uids}")
        if not healthy:
            skip(gates["http"] + gates["code"] + gates["setup"] + gates["login"], "the server is not healthy")
            return

        status = http("GET", f"{base}/api/today")[0]
        check(status == 401, f"{tag} the API needs a session", f"status {status}")
        # Exactly the configured VOCAB_PUBLIC_ORIGIN, and a JSON body type.
        json_headers = {"Origin": ORIGIN, "Content-Type": "application/json"}
        status, _, body = http("GET", f"{base}/api/auth/setup")
        check(status == 200 and json_of(body) == {"required": True}, f"{tag} first start is in setup state",
              f"status {status}, body {body[:200]!r}")
        logs = docker("logs", name)
        codes = SETUP_LINE.findall(logs.stdout + logs.stderr)
        if not check(len(codes) == 1, f"{tag} the setup code is in the log once", f"found {len(codes)}"):
            skip(gates["code"] + gates["setup"] + gates["login"], "no setup code")
            return
        code = codes[0]
        status, _, body = http("POST", f"{base}/api/auth/setup", {"code": "0000-0000-00", "password": password},
                               json_headers)
        check(status == 401, f"{tag} a wrong setup code is refused", f"status {status}, body {body[:200]!r}")
        status, headers, body = http("POST", f"{base}/api/auth/setup", {"code": code, "password": password},
                                     json_headers)
        if not check(status == 200 and session_cookie(headers) is not None, f"{tag} setup sets the password and logs in",
                     f"status {status}, body {body[:200]!r}"):
            skip(gates["code"][2:] + gates["setup"] + gates["login"], "setup failed")
            return
        status, _, _ = http("POST", f"{base}/api/auth/setup", {"code": code, "password": password}, json_headers)
        check(status == 409, f"{tag} setup is refused once a password is set", f"status {status}")
        status, _, _ = http("POST", f"{base}/api/auth/login", {"password": "test-wrong-password"}, json_headers)
        check(status == 401, f"{tag} a wrong password is refused", f"status {status}")
        status, headers, body = http("POST", f"{base}/api/auth/login", {"password": password}, json_headers)
        cookie = session_cookie(headers)
        if not check(status == 200 and cookie is not None, f"{tag} login works", f"status {status}, body {body[:200]!r}"):
            skip(gates["login"], "login failed")
            return
        session = {"Cookie": cookie or ""}
        status, _, _ = http("GET", f"{base}/api/today", headers=session)
        check(status == 200, f"{tag} the session opens the API", f"status {status}")
        status, _, body = http("PATCH", f"{base}/api/settings", {"block_size": 15}, {**json_headers, **session})
        saved = json_of(body)
        check(status == 200 and isinstance(saved, dict) and saved.get("block_size") == 15,
              f"{tag} a setting is saved", f"status {status}, body {body[:200]!r}")

    try:
        guarded(tag, exercise)
        if failures() > failures_before:
            gha.error("container log", f"{name} (last {LOG_TAIL} lines):\n{container_log(name)}")
    finally:
        print(gha.untrusted_block("container log (tail):", container_log(name)), flush=True)
        docker("rm", "-f", name, check=False)


def static_checks(image: str) -> None:
    """Checks of the image's configuration and filesystem."""
    config = json.loads(docker("image", "inspect", image).stdout)[0]["Config"]
    check(config.get("User") == f"{UID}:{UID}", f"image user is {UID}:{UID} (got {config.get('User')!r})")
    check(config.get("Entrypoint") == ["/vocab-server"], "entrypoint is /vocab-server")

    def listing() -> None:
        created = docker("create", "--pull=never", image).stdout.strip()
        try:
            exported = subprocess.run(["docker", "export", created], capture_output=True, timeout=120, check=True).stdout
        finally:
            docker("rm", "-f", created, check=False)
        with tarfile.open(fileobj=io.BytesIO(exported)) as tar:
            entries = [(m.name, m.mode, m.isreg()) for m in tar.getmembers()]
        present = image_programs(entries)
        check(not present, f"no shell, package manager or other program in the filesystem (found: {present})")

    if not guarded("filesystem listing", listing):
        skip(["no shell, package manager or other program in the filesystem"], "the image could not be exported")
    for shell in ["/bin/sh", "/busybox/sh", "sh"]:
        result = docker("run", "--rm", "--pull=never", *HARDENING, "--entrypoint", shell, image, "-c", "true", check=False)
        check(result.returncode != 0, f"--entrypoint {shell} cannot run")


def refusal_checks(volume: str, image: str) -> None:
    """The container refuses to start without its origin or with an M5 secret."""
    for env, origin, expected in [
        ({}, False, "VOCAB_PUBLIC_ORIGIN is not set"),
        ({"VOCAB_PASSWORD_HASH_B64": "x"}, True, "unknown setting VOCAB_PASSWORD_HASH_B64"),
    ]:
        result = docker(*run_args(UID, volume, env, origin=origin), "--rm", image, check=False)
        output = result.stdout + result.stderr
        check(result.returncode != 0 and expected in output, f"refuses to start: {expected}",
              f"exit {result.returncode}: {one_line(output)[-300:]}")


def bind_mount_checks(image: str, run_id: str, password: str) -> None:
    """The deployment's user on a bind-mounted directory it owns."""
    data_dir = tempfile.mkdtemp(prefix="vocab-smoke-")
    owned_check = f"the database was written as UID {DEPLOY_UID}"
    cache_check = f"the app created its cache directory (0700, UID {DEPLOY_UID}) in /data"
    try:
        def prepare() -> None:
            sudo("chown", f"{DEPLOY_UID}:{DEPLOY_UID}", data_dir)
            sudo("chmod", "0700", data_dir)

        if not guarded("bind-mounted data directory", prepare):
            skip([owned_check, cache_check], "the data directory could not be prepared")
            return
        serve_and_check(image, f"vocab-smoke-{run_id}-uid", DEPLOY_UID, data_dir, password)

        def files() -> None:
            owned = sudo("find", data_dir, "-maxdepth", "1", "-name", "vocabulary.sqlite", "-user", DEPLOY_UID).stdout.strip()
            check(bool(owned), owned_check)
            cache = sudo("find", data_dir, "-maxdepth", "1", "-name", "cache", "-type", "d",
                         "-user", DEPLOY_UID, "-perm", "700").stdout.strip()
            check(bool(cache), cache_check)

        guarded("bind-mounted data directory", files)
    finally:
        sudo("rm", "-rf", data_dir, check=False)


def smoke(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__)
        return 2
    image = argv[0]
    password = f"test-password-{secrets.token_hex(8)}"
    run_id = secrets.token_hex(4)
    volume = f"vocab-smoke-data-{run_id}"

    guarded("static checks", lambda: static_checks(image))

    def on_volume() -> None:
        docker("volume", "create", volume)
        try:
            refusal_checks(volume, image)
            # The image's own user on a fresh named volume.
            serve_and_check(image, f"vocab-smoke-{run_id}", UID, volume, password)
        finally:
            docker("volume", "rm", "-f", volume, check=False)

    guarded("named volume", on_volume)
    bind_mount_checks(image, run_id, password)

    if failures():
        print(f"image smoke tests FAILED: {failures()} failed check(s)")
        return 1
    print("image smoke tests passed")
    return 0


def main(argv: list[str]) -> int:
    """Run the smoke tests; every failed check is already an annotation, a
    crash becomes one here, and every result goes to the job summary."""
    crash: str | None = None
    try:
        return smoke(argv)
    except Exception as err:  # any crash must become an annotation too
        crash = f"{type(err).__name__}: {err}"
        gha.error("image-smoke crashed", one_line(crash))
        raise
    finally:
        if RESULTS or crash:
            rows = list(RESULTS)
            if crash:
                rows.append(("(aborted)", False, crash))
            gha.append_summary(gha.summary_table("Image smoke tests", rows))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
