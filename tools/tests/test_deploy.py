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

    def test_r85_one_dataset_mount_config_from_the_environment_and_no_secrets(self):
        base = "/mnt/tank/services/vocabulary"
        volumes = re.findall(r"(?m)^\s*- (/mnt/\S+:\S+)$", COMPOSE)
        self.assertEqual(volumes, [f"{base}:/data"])
        self.assertNotRegex(COMPOSE, r":/cache\b")
        self.assertNotIn("/config", COMPOSE)
        self.assertFalse((REPO / "deploy" / "config.example.json").exists())
        self.assertIn("- VOCAB_PUBLIC_ORIGIN=https://vocab.example.home\n", COMPOSE)
        # No secrets in the environment, no .env at all (R-82: set in the app).
        for secret in ("VOCAB_PASSWORD_HASH_B64", "VOCAB_IMPORT_TOKEN", "${"):
            self.assertNotIn(secret, COMPOSE)
        self.assertFalse((REPO / "deploy" / ".env.example").exists())

    def test_r85_readme_uses_the_dataset_root_setup_code_and_reset(self):
        readme = (REPO / "deploy" / "README.md").read_text()
        self.assertNotIn("mkdir -p /mnt/tank/services/vocabulary", readme)
        self.assertIn("owner user\n**e.g. 3101**", readme)
        self.assertIn("docker logs vocabulary 2>&1 | grep 'Setup required'", readme)
        self.assertIn("docker compose run --rm vocabulary reset-password", readme)
        self.assertIn("*Create token*", readme)
        self.assertNotIn("hash-password", readme)

    def test_r85_readme_limits_the_dataset_and_logs_out_after_a_restore(self):
        readme = (REPO / "deploy" / "README.md").read_text()
        self.assertRegex(readme, r"zfs set refquota=\S+ tank/services/vocabulary")
        self.assertRegex(readme, r"zfs set quota=\S+ tank/services/vocabulary")
        restore = readme.split("## 6. Restore", 1)[1]
        self.assertIn("docker compose run --rm vocabulary logout-all", restore)
        self.assertLess(restore.index("Verify in the UI"), restore.index("logout-all"))

    def test_r80_readme_makes_the_package_public_and_checks_anonymous_pulls(self):
        readme = (REPO / "deploy" / "README.md").read_text()
        self.assertIn("*Change visibility*", readme)
        self.assertIn('DOCKER_CONFIG="$(mktemp -d)" docker buildx imagetools inspect', readme)


def service(name: str) -> str:
    """The block of one service in the compose file (up to the next one)."""
    m = re.search(rf"(?ms)^  {re.escape(name)}:\n(.*?)(?=^  \S|^\S)", COMPOSE)
    assert m, name
    return m.group(1)


class TtsService(unittest.TestCase):
    def test_r83_tts_runs_hardened_on_the_internal_network_only(self):
        tts = service("vocabulary-tts")
        self.assertRegex(tts, r"(?m)^    image: ghcr\.io/kadledav/vocabulary-training-tts:\S+@sha256:\S+$")
        for required in (
            r'(?m)^    user: "3101:3101"$',
            r"(?m)^    read_only: true$",
            r"(?m)^    cap_drop: \[ALL\]$",
            r'(?m)^    security_opt: \["no-new-privileges:true"\]$',
            r"(?m)^    mem_limit: 1280m$",
            r"(?m)^    memswap_limit: 1280m$",
            r"(?m)^    cpus: 2\.0$",
            r"(?m)^    pids_limit: 64$",
            r'test: \["CMD", "/vocab-tts", "healthcheck"\]',
        ):
            self.assertRegex(tts, required)
        self.assertEqual(re.findall(r"(?m)^      - (\S+)$", tts.split("networks:", 1)[1]), ["vocab-internal"])
        for forbidden in (r"(?m)^\s*ports\s*:", r"(?m)^\s*volumes\s*:", r"traefik", r"(?m)^\s*tmpfs\s*:"):
            self.assertNotRegex(tts, forbidden)

    def test_r83_internal_network_shared_only_with_the_app(self):
        self.assertRegex(COMPOSE, r"(?m)^  vocab-internal:\n    internal: true$")
        app = service("vocabulary")
        self.assertIn("      - vocab-internal\n", app)
        self.assertIn("traefik.docker.network=proxy-net", app)
        # The app reaches the service by its name on that network.
        self.assertIn("      - VOCAB_TTS_URL=http://vocabulary-tts:8090\n", app)
        self.assertEqual(COMPOSE.count("      - vocab-internal\n"), 2)


class TtsDockerfile(unittest.TestCase):
    TEXT = (REPO / "docker" / "Dockerfile.tts").read_text()

    def test_r83_tts_runtime_is_distroless_cc_nonroot_with_a_read_only_model(self):
        stage = self.TEXT.rsplit("\nFROM ", 1)[1]
        self.assertRegex(stage, r"^gcr\.io/distroless/cc-debian13:nonroot@sha256:[0-9a-f]{64}\n")
        self.assertRegex(stage, r"(?m)^USER 65532:65532$")
        self.assertRegex(stage, r'(?m)^ENTRYPOINT \["/vocab-tts"\]$')
        self.assertRegex(stage, r"(?m)^COPY --from=model /out/model /model$")
        self.assertNotRegex(stage, r"(?m)^(RUN|SHELL|VOLUME)\b")
        self.assertIn("install -m 0444 model.onnx", self.TEXT)

    def test_r90_tts_inputs_are_pinned_and_checksummed(self):
        froms = re.findall(r"(?m)^FROM\s+(\S+)", self.TEXT)
        external = [image for image in froms if image not in ("tools", "build")]
        self.assertEqual(len(external), 2)
        for image in external:
            self.assertRegex(image, r":[^@]+@sha256:[0-9a-f]{64}$")
        adds = re.findall(r"(?m)^ADD (.+)$", self.TEXT)
        self.assertEqual(len(adds), 4)
        for add in adds:
            self.assertRegex(add, r"^--checksum=sha256:[0-9a-f]{64} https://(github\.com/k2-fsa/sherpa-onnx/releases/"
                                  r"download/|raw\.githubusercontent\.com/cmusphinx/cmudict/[0-9a-f]{40}/)\S+ /archives/$")

    def test_r83_tts_image_ships_its_licences_and_no_espeak(self):
        for name in ("SOURCE.md", "THIRD-PARTY-NOTICES.txt", "onnxruntime-LICENSE.txt", "cmudict-LICENSE.txt"):
            self.assertTrue((REPO / "docker" / "tts-licenses" / name).is_file(), name)
        self.assertIn("COPY docker/tts-licenses/ /licenses/", self.TEXT)
        self.assertNotRegex(self.TEXT, r"(?m)^[^#]*install[^#\n]*espeak")


class Dockerfile(unittest.TestCase):
    def runtime_stage(self) -> str:
        return DOCKERFILE.rsplit("\nFROM ", 1)[1]

    def test_r83_runtime_is_distroless_static_nonroot_pinned_by_digest(self):
        stage = self.runtime_stage()
        self.assertRegex(stage, r"^gcr\.io/distroless/static-debian13:nonroot@sha256:[0-9a-f]{64}\n")
        self.assertRegex(stage, r"(?m)^USER 65532:65532$")
        self.assertNotIn("/config", stage)
        self.assertNotRegex(stage, r"(?m)^COPY .* /cache$")
        self.assertRegex(stage, r'(?m)^ENTRYPOINT \["/vocab-server"\]$')
        # Nothing runs in the runtime stage: there is no shell to run it.
        self.assertNotRegex(stage, r"(?m)^(RUN|SHELL)\b")

    def test_r83_build_is_native_for_amd64_and_arm64(self):
        self.assertIn("ARG TARGETARCH", DOCKERFILE)
        self.assertIn("amd64) echo x86_64-unknown-linux-musl", DOCKERFILE)
        self.assertIn("arm64) echo aarch64-unknown-linux-musl", DOCKERFILE)
        self.assertNotRegex(DOCKERFILE, r"--target x86_64")

    def test_licences_and_lock_file_are_in_the_image(self):
        stage = self.runtime_stage()
        self.assertRegex(stage, r"(?m)^COPY LICENSE THIRD-PARTY-NOTICES.txt /licenses/$")
        self.assertRegex(stage, r"(?m)^COPY Cargo.lock /usr/share/doc/vocab-server/Cargo.lock$")
        ignore = (REPO / ".dockerignore").read_text()
        for name in ("LICENSE", "THIRD-PARTY-NOTICES.txt", "Cargo.lock"):
            self.assertIn(f"!{name}\n", ignore)

    def test_r83_every_base_image_is_pinned_by_digest(self):
        froms = re.findall(r"(?m)^FROM\s+(\S+)", DOCKERFILE)
        self.assertEqual(len(froms), 2)
        for image in froms:
            self.assertRegex(image, r":[^@]+@sha256:[0-9a-f]{64}$")


if __name__ == "__main__":
    unittest.main()
