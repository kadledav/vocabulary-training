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
* `hash-password` works inside the image;
* the container refuses to start without a password hash, with a short
  import token and without VOCAB_PUBLIC_ORIGIN;
* `/healthz` answers 200 and the built-in `healthcheck` subcommand passes;
* the API needs a session, login with the configured password works, a
  setting can be saved (a database write), and the process runs as the
  requested UID.

Never pulls anything (`--pull=never`): the image must have been built
locally. Secrets are generated per run (`test-` prefix). Standard library only.
"""

from __future__ import annotations

import io
import json
import os
import secrets
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request

UID = "65532"
ORIGIN = "https://vocab.example.home"
# The deployment's own user (deploy/README.md), owner of its data directory.
DEPLOY_UID = "3101"
SECURITY = ["--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges"]
HARDENING = [*SECURITY, "--user", f"{UID}:{UID}"]
# Names of the secret variables (values are generated per run).
HASH_ENV = "VOCAB_PASSWORD_HASH_B64"
TOKEN_ENV = "VOCAB_IMPORT_TOKEN"
FORBIDDEN_FILES = {"sh", "bash", "ash", "dash", "zsh", "busybox", "apt", "apt-get", "dpkg", "apk", "rpm", "microdnf"}


def docker(*args: str, check: bool = True, stdin: str | None = None) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], input=stdin, capture_output=True, text=True, timeout=120)
    if check and result.returncode != 0:
        raise SystemExit(f"docker {' '.join(args[:2])} failed ({result.returncode}):\n{result.stderr}{result.stdout}")
    return result


def check(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"FAIL: {message}")
    print(f"ok: {message}")


def run_args(image_user: str, data: str, env: dict[str, str], origin: bool = True) -> list[str]:
    """`docker run` arguments as Compose runs the app (no tmpfs at all)."""
    args = ["run", "--pull=never", *SECURITY, "--user", f"{image_user}:{image_user}", "-v", f"{data}:/data"]
    if origin:
        args += ["-e", f"VOCAB_PUBLIC_ORIGIN={ORIGIN}"]
    for key, value in env.items():
        args += ["-e", f"{key}={value}"]
    return args


def http(method: str, url: str, body: dict | None = None, headers: dict[str, str] | None = None):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            return response.status, dict(response.headers), response.read()
    except urllib.error.HTTPError as err:
        return err.code, dict(err.headers), err.read()


def sudo(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    """A host command as root (the CI runner has passwordless sudo)."""
    result = subprocess.run(["sudo", "-n", *args], capture_output=True, text=True, timeout=60)
    if check and result.returncode != 0:
        raise SystemExit(f"sudo {args[0]} failed:\n{result.stderr}")
    return result


def serve_and_check(image: str, name: str, user: str, data: str, env: dict[str, str], password: str) -> None:
    """Start the app hardened as `user` on `data`, then check health, the
    non-root process, the session gate, login and a write."""
    args = run_args(user, data, env)
    docker(*args[:1], "-d", "--name", name, "-p", "127.0.0.1::8080", *args[1:], image)
    try:
        port = docker("port", name, "8080/tcp").stdout.strip().splitlines()[0].rsplit(":", 1)[1]
        base = f"http://127.0.0.1:{port}"
        deadline = time.monotonic() + 60
        status = 0
        while time.monotonic() < deadline:
            try:
                status = http("GET", f"{base}/healthz")[0]
            except (urllib.error.URLError, ConnectionError, OSError):
                status = 0
            if status == 200:
                break
            time.sleep(0.5)
        check(status == 200, f"[uid {user}] /healthz answers 200")
        check(docker("exec", name, "/vocab-server", "healthcheck", check=False).returncode == 0,
              f"[uid {user}] the healthcheck subcommand passes inside the container")
        uids = docker("top", name, "-eo", "uid").stdout.split()[1:]
        check(bool(uids) and all(u == user for u in uids), f"[uid {user}] the process runs as UID {user} (got {uids})")

        check(http("GET", f"{base}/api/today")[0] == 401, f"[uid {user}] the API needs a session")
        json_headers = {"Origin": ORIGIN, "Content-Type": "application/json"}
        status, _, _ = http("POST", f"{base}/api/auth/login", {"password": "test-wrong-password"}, json_headers)
        check(status == 401, f"[uid {user}] a wrong password is refused")
        status, headers, _ = http("POST", f"{base}/api/auth/login", {"password": password}, json_headers)
        cookie = next((v for k, v in headers.items() if k.lower() == "set-cookie"), "")
        check(status == 200 and cookie.startswith("__Host-vocab_session="), f"[uid {user}] login works")
        session = {"Cookie": cookie.split(";")[0]}
        status, _, _ = http("GET", f"{base}/api/today", headers=session)
        check(status == 200, f"[uid {user}] the session opens the API")
        status, _, body = http("PATCH", f"{base}/api/settings", {"block_size": 15}, {**json_headers, **session})
        check(status == 200 and json.loads(body).get("block_size") == 15, f"[uid {user}] a setting is saved")
    finally:
        logs = docker("logs", name, check=False)
        if logs.returncode == 0:
            print("container log (tail):\n" + "\n".join((logs.stdout + logs.stderr).splitlines()[-20:]))
        docker("rm", "-f", name, check=False)


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__)
        return 2
    image = argv[0]
    password = f"test-password-{secrets.token_hex(8)}"
    token = secrets.token_hex(32)
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

    # ── hash-password ───────────────────────────────────────────────────────
    result = docker("run", "--rm", "-i", "--pull=never", *HARDENING, image, "hash-password", stdin=f"{password}\n")
    password_hash = result.stdout.strip()
    check(len(password_hash) > 60 and "$" not in password_hash, "hash-password prints a base64 PHC string")

    docker("volume", "create", volume)
    try:
        # ── refuses to start without proper secrets ─────────────────────────
        short = token[:31]
        for env, expected in [
            ({TOKEN_ENV: token}, f"{HASH_ENV} is not set"),
            ({HASH_ENV: password_hash, TOKEN_ENV: short}, f"{TOKEN_ENV} must be"),
        ]:
            result = docker(*run_args(UID, volume, env), "--rm", image, check=False)
            output = result.stdout + result.stderr
            check(result.returncode != 0 and expected in output, f"refuses to start: {expected}")
        # ...and without its origin (configuration comes from the environment).
        args = run_args(UID, volume, {HASH_ENV: password_hash, TOKEN_ENV: token}, origin=False)
        result = docker(*args, "--rm", image, check=False)
        check(result.returncode != 0 and "VOCAB_PUBLIC_ORIGIN is not set" in result.stdout + result.stderr,
              "refuses to start without VOCAB_PUBLIC_ORIGIN")

        env = {HASH_ENV: password_hash, TOKEN_ENV: token}
        # ── the image's own user on a fresh named volume ────────────────────
        serve_and_check(image, f"vocab-smoke-{run_id}", UID, volume, env, password)
    finally:
        docker("volume", "rm", "-f", volume, check=False)

    # ── the deployment's user on a bind-mounted directory it owns ───────────
    data_dir = tempfile.mkdtemp(prefix="vocab-smoke-")
    try:
        sudo("chown", f"{DEPLOY_UID}:{DEPLOY_UID}", data_dir)
        sudo("chmod", "0700", data_dir)
        serve_and_check(image, f"vocab-smoke-{run_id}-uid", DEPLOY_UID, data_dir, env, password)
        owned = sudo("find", data_dir, "-name", "vocabulary.sqlite", "-user", DEPLOY_UID).stdout.strip()
        check(bool(owned), f"the database was written as UID {DEPLOY_UID}")
    finally:
        sudo("rm", "-rf", data_dir, check=False)
    print("image smoke tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
