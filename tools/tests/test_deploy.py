"""Checks of the deployment files (R-80, R-81, R-83, R-85): deploy/ and
docker/Dockerfile.server. Text checks, standard library only; the running
image itself is tested by tools/image-smoke.py in CI.

Run: python3 -m unittest discover -s tools/tests -v
"""

from __future__ import annotations

import json
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
COMPOSE = (REPO / "deploy" / "docker-compose.example.yml").read_text()
DOCKERFILE = (REPO / "docker" / "Dockerfile.server").read_text()


def labels() -> list[str]:
    return re.findall(r'^\s*-\s*"(traefik\.[^"]+)"', COMPOSE, re.M)


class Compose(unittest.TestCase):
    def test_r80_the_stack_runs_the_ghcr_image_behind_traefik_on_home_docker_net(self):
        self.assertRegex(COMPOSE, r"(?m)^\s*image: ghcr\.io/kadledav/vocabulary-training:\S+@sha256:\S+$")
        self.assertRegex(COMPOSE, r"(?m)^  proxy-net:\n    external: true$")
        self.assertIn("traefik.enable=true", labels())

    def test_r81_routers_only_on_the_home_entrypoint_and_no_published_ports(self):
        routers = {m.group(1) for label in labels() if (m := re.match(r"traefik\.http\.routers\.([^.]+)\.", label))}
        self.assertTrue(routers)
        for router in routers:
            self.assertIn(f"traefik.http.routers.{router}.entrypoints=home", labels(), router)
        self.assertNotIn("websecure", "\n".join(labels()))
        for forbidden in (r"(?m)^\s*ports\s*:", r"network_mode", r"docker\.sock", r"(?m)^\s*devices\s*:", "privileged"):
            self.assertNotRegex(COMPOSE, forbidden)

    def test_r83_container_is_hardened_and_limited(self):
        for required in (
            r'(?m)^\s*user: "3101:3101"$',
            r"(?m)^\s*read_only: true$",
            r"(?m)^\s*cap_drop: \[ALL\]$",
            r'(?m)^\s*security_opt: \["no-new-privileges:true"\]$',
            r"(?m)^\s*mem_limit: \d+m$",
            r"(?m)^\s*memswap_limit: \d+m$",
            r"(?m)^\s*cpus: [0-9.]+$",
            r"(?m)^\s*pids_limit: \d+$",
            r'test: \["CMD", "/vocab-server", "healthcheck"\]',
        ):
            self.assertRegex(COMPOSE, required)

    def test_r85_data_and_cache_volumes_config_and_secrets_from_the_environment(self):
        base = "/mnt/tank/services/vocabulary"
        self.assertIn(f"{base}/data:/data\n", COMPOSE)
        self.assertIn(f"{base}/cache:/cache\n", COMPOSE)
        self.assertNotIn("/config", COMPOSE)
        self.assertFalse((REPO / "deploy" / "config.example.json").exists())
        self.assertIn("- VOCAB_PUBLIC_ORIGIN=https://vocab.example.home\n", COMPOSE)
        for key in ("VOCAB_PASSWORD_HASH_B64", "VOCAB_IMPORT_TOKEN"):
            self.assertIn(f"{key}=${{{key}:?", COMPOSE)
        env_example = (REPO / "deploy" / ".env.example").read_text()
        keys = []
        for line in env_example.splitlines():
            if line and not line.startswith("#"):
                key, _, value = line.partition("=")
                keys.append(key)
                self.assertEqual(value.strip(), "", f"{key} must be empty in .env.example")
        self.assertEqual(keys, ["VOCAB_PASSWORD_HASH_B64", "VOCAB_IMPORT_TOKEN"])

    def test_r85_readme_sets_up_data_and_cache_for_uid_3101(self):
        readme = (REPO / "deploy" / "README.md").read_text()
        self.assertIn("mkdir -p /mnt/tank/services/vocabulary/{data,cache}", readme)
        self.assertIn("chown 3101:3101 /mnt/tank/services/vocabulary/data /mnt/tank/services/vocabulary/cache", readme)
        self.assertIn("chmod 600 .env", readme)


class Dockerfile(unittest.TestCase):
    def runtime_stage(self) -> str:
        return DOCKERFILE.rsplit("\nFROM ", 1)[1]

    def test_r83_runtime_is_distroless_static_nonroot_pinned_by_digest(self):
        stage = self.runtime_stage()
        self.assertRegex(stage, r"^gcr\.io/distroless/static-debian13:nonroot@sha256:[0-9a-f]{64}\n")
        self.assertRegex(stage, r"(?m)^USER 65532:65532$")
        self.assertNotIn("/config", stage)
        self.assertRegex(stage, r'(?m)^ENTRYPOINT \["/vocab-server"\]$')
        # Nothing runs in the runtime stage: there is no shell to run it.
        self.assertNotRegex(stage, r"(?m)^(RUN|SHELL)\b")

    def test_r83_every_base_image_is_pinned_by_digest(self):
        froms = re.findall(r"(?m)^FROM\s+(\S+)", DOCKERFILE)
        self.assertEqual(len(froms), 2)
        for image in froms:
            self.assertRegex(image, r":[^@]+@sha256:[0-9a-f]{64}$")


if __name__ == "__main__":
    unittest.main()
