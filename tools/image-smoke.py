#!/usr/bin/env python3
"""Smoke tests of the vocab-server image (R-83, R-82; design §13.2, §14.2).

Usage: python3 tools/image-smoke.py <image>

Runs the locally built image the way the Compose stack does (read-only root
filesystem, all capabilities dropped, no-new-privileges, no tmpfs at all,
configuration from VOCAB_* variables): once as the image's user 65532 on a
fresh named volume, once as the deployment's user 3101 on a bind-mounted
directory owned by 3101 (needs passwordless sudo, as on CI runners). Checks:

* the image runs as 65532:65532 and contains no shell, busybox or package
  manager (filesystem listing + `--entrypoint /bin/sh` must fail);
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
FORBIDDEN_FILES = {"sh", "bash", "ash", "dash", "zsh", "busybox", "apt", "apt-get", "dpkg", "apk", "rpm", "microdnf"}


class SmokeFailure(Exception):
    """A failed check: `name` is the check, `reason` one line about why."""

    def __init__(self, name: str, reason: str) -> None:
        super().__init__(f"{name}: {reason}")
        self.name = name
        self.reason = reason


# (check, passed, detail) of this run, for the job summary.
RESULTS: list[tuple[str, bool, str]] = []


def one_line(text: str) -> str:
    return " ".join(text.split())


def docker(*args: str, check: bool = True, stdin: str | None = None) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], input=stdin, capture_output=True, text=True, timeout=120)
    if check and result.returncode != 0:
        raise SmokeFailure(f"docker {' '.join(args[:2])}",
                           f"exit {result.returncode}: {one_line(result.stderr + result.stdout)[:500]}")
    return result


def check(condition: bool, message: str, detail: str = "") -> None:
    RESULTS.append((message, condition, detail))
    if not condition:
        raise SmokeFailure(message, detail or "check failed")
    print(f"ok: {message}")


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
    non-root process, the session gate, first-run setup, login and a write."""
    args = run_args(user, data, {})
    docker(*args[:1], "-d", "--name", name, "-p", "127.0.0.1::8080", *args[1:], image)
    try:
        # `docker port` prints e.g. `127.0.0.1:49153` (one line per binding).
        mapped = docker("port", name, "8080/tcp").stdout.strip().splitlines()
        check(bool(mapped), f"[uid {user}] port 8080 is published on 127.0.0.1", repr(mapped))
        base = f"http://127.0.0.1:{mapped[0].rsplit(':', 1)[1]}"
        status = wait_healthy(base, name)
        check(status == 200, f"[uid {user}] /healthz answers 200", f"last status {status}")
        result = docker("exec", name, "/vocab-server", "healthcheck", check=False)
        check(result.returncode == 0, f"[uid {user}] the healthcheck subcommand passes inside the container",
              one_line(result.stderr + result.stdout))
        # `docker top` needs a PID column to find the container's processes.
        rows = [line.split() for line in docker("top", name, "-eo", "pid,uid").stdout.splitlines()[1:]]
        uids = [row[1] for row in rows if len(row) >= 2]
        check(bool(uids) and all(u == user for u in uids), f"[uid {user}] the process runs as UID {user}", f"got {uids}")

        status = http("GET", f"{base}/api/today")[0]
        check(status == 401, f"[uid {user}] the API needs a session", f"status {status}")
        # Exactly the configured VOCAB_PUBLIC_ORIGIN, and a JSON body type.
        json_headers = {"Origin": ORIGIN, "Content-Type": "application/json"}
        status, _, body = http("GET", f"{base}/api/auth/setup")
        check(status == 200 and json_of(body) == {"required": True}, f"[uid {user}] first start is in setup state",
              f"status {status}, body {body[:200]!r}")
        logs = docker("logs", name)
        codes = SETUP_LINE.findall(logs.stdout + logs.stderr)
        check(len(codes) == 1, f"[uid {user}] the setup code is in the log once", f"found {len(codes)}")
        code = codes[0]
        status, _, body = http("POST", f"{base}/api/auth/setup", {"code": "0000-0000-00", "password": password},
                               json_headers)
        check(status == 401, f"[uid {user}] a wrong setup code is refused", f"status {status}, body {body[:200]!r}")
        status, headers, body = http("POST", f"{base}/api/auth/setup", {"code": code, "password": password},
                                     json_headers)
        cookie = session_cookie(headers)
        check(status == 200 and cookie is not None, f"[uid {user}] setup sets the password and logs in",
              f"status {status}, body {body[:200]!r}")
        status, _, _ = http("POST", f"{base}/api/auth/setup", {"code": code, "password": password}, json_headers)
        check(status == 409, f"[uid {user}] setup is refused once a password is set", f"status {status}")
        status, _, _ = http("POST", f"{base}/api/auth/login", {"password": "test-wrong-password"}, json_headers)
        check(status == 401, f"[uid {user}] a wrong password is refused", f"status {status}")
        status, headers, body = http("POST", f"{base}/api/auth/login", {"password": password}, json_headers)
        cookie = session_cookie(headers)
        check(status == 200 and cookie is not None, f"[uid {user}] login works", f"status {status}, body {body[:200]!r}")
        session = {"Cookie": cookie or ""}
        status, _, _ = http("GET", f"{base}/api/today", headers=session)
        check(status == 200, f"[uid {user}] the session opens the API", f"status {status}")
        status, _, body = http("PATCH", f"{base}/api/settings", {"block_size": 15}, {**json_headers, **session})
        saved = json_of(body)
        check(status == 200 and isinstance(saved, dict) and saved.get("block_size") == 15,
              f"[uid {user}] a setting is saved", f"status {status}, body {body[:200]!r}")
    except BaseException:
        gha.error("container log", f"{name} (last {LOG_TAIL} lines):\n{container_log(name)}")
        raise
    finally:
        print(gha.untrusted_block("container log (tail):", container_log(name)), flush=True)
        docker("rm", "-f", name, check=False)


def smoke(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__)
        return 2
    image = argv[0]
    password = f"test-password-{secrets.token_hex(8)}"
    run_id = secrets.token_hex(4)
    volume = f"vocab-smoke-data-{run_id}"

    # ── static checks of the image ──────────────────────────────────────────
    config = json.loads(docker("image", "inspect", image).stdout)[0]["Config"]
    check(config.get("User") == f"{UID}:{UID}", f"image user is {UID}:{UID} (got {config.get('User')!r})")
    check(config.get("Entrypoint") == ["/vocab-server"], "entrypoint is /vocab-server")
    created = docker("create", "--pull=never", image).stdout.strip()
    try:
        exported = subprocess.run(["docker", "export", created], capture_output=True, timeout=120, check=True).stdout
    finally:
        docker("rm", "-f", created, check=False)
    with tarfile.open(fileobj=io.BytesIO(exported)) as tar:
        names = [m.name for m in tar.getmembers()]
    present = sorted({n for n in names if os.path.basename(n.rstrip("/")) in FORBIDDEN_FILES})
    check(not present, f"no shell or package manager in the filesystem (found: {present})")
    for shell in ["/bin/sh", "/busybox/sh", "sh"]:
        result = docker("run", "--rm", "--pull=never", *HARDENING, "--entrypoint", shell, image, "-c", "true", check=False)
        check(result.returncode != 0, f"--entrypoint {shell} cannot run")

    docker("volume", "create", volume)
    try:
        # ── refuses to start without its origin or with an M5 secret ────────
        for env, origin, expected in [
            ({}, False, "VOCAB_PUBLIC_ORIGIN is not set"),
            ({"VOCAB_PASSWORD_HASH_B64": "x"}, True, "unknown setting VOCAB_PASSWORD_HASH_B64"),
        ]:
            result = docker(*run_args(UID, volume, env, origin=origin), "--rm", image, check=False)
            output = result.stdout + result.stderr
            check(result.returncode != 0 and expected in output, f"refuses to start: {expected}",
                  f"exit {result.returncode}: {one_line(output)[-300:]}")

        # ── the image's own user on a fresh named volume ────────────────────
        serve_and_check(image, f"vocab-smoke-{run_id}", UID, volume, password)
    finally:
        docker("volume", "rm", "-f", volume, check=False)

    # ── the deployment's user on a bind-mounted directory it owns ───────────
    data_dir = tempfile.mkdtemp(prefix="vocab-smoke-")
    try:
        sudo("chown", f"{DEPLOY_UID}:{DEPLOY_UID}", data_dir)
        sudo("chmod", "0700", data_dir)
        serve_and_check(image, f"vocab-smoke-{run_id}-uid", DEPLOY_UID, data_dir, password)
        owned = sudo("find", data_dir, "-maxdepth", "1", "-name", "vocabulary.sqlite", "-user", DEPLOY_UID).stdout.strip()
        check(bool(owned), f"the database was written as UID {DEPLOY_UID}")
        cache = sudo("find", data_dir, "-maxdepth", "1", "-name", "cache", "-type", "d",
                     "-user", DEPLOY_UID, "-perm", "700").stdout.strip()
        check(bool(cache), f"the app created its cache directory (0700, UID {DEPLOY_UID}) in /data")
    finally:
        sudo("rm", "-rf", data_dir, check=False)
    print("image smoke tests passed")
    return 0


def main(argv: list[str]) -> int:
    """Run the smoke tests; report a failure as an annotation and every
    result in the job summary."""
    failure: str | None = None
    try:
        return smoke(argv)
    except SmokeFailure as err:
        failure = str(err)
        gha.error(f"image-smoke {err.name}", one_line(err.reason))
        print(f"FAIL: {one_line(str(err))}")
        return 1
    except Exception as err:  # any crash must become an annotation too
        failure = f"{type(err).__name__}: {err}"
        gha.error("image-smoke crashed", one_line(failure))
        raise
    finally:
        if RESULTS or failure:
            rows = list(RESULTS)
            if failure and not any(not ok for _, ok, _ in rows):
                rows.append(("(aborted)", False, failure))
            gha.append_summary(gha.summary_table("Image smoke tests", rows))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
