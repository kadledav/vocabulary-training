"""Pure parts of tools/tts-smoke.py: the filesystem rule (no programs but
/vocab-tts and the base image's shared libraries), the MP3 frame parser and
the memory figure of `docker stats`. The image itself is exercised in CI.

Run: python3 -m unittest discover -s tools/tests -v
"""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(TOOLS))

_spec = importlib.util.spec_from_file_location("tts_smoke", TOOLS / "tts-smoke.py")
assert _spec and _spec.loader
smoke = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(smoke)

ELF = b"\x7fELF"


def frame(bitrate_index: int = 8, rate_index: int = 1, padding: int = 0, mono: bool = True) -> bytes:
    """One MPEG-2 layer III frame (header + zero payload)."""
    bitrate = smoke.BITRATES_V2[bitrate_index]
    rate = smoke.RATES_V2[rate_index]
    length = 72 * bitrate * 1000 // rate + padding
    header = bytes([0xFF, 0xF3, (bitrate_index << 4) | (rate_index << 2) | (padding << 1), 0xC0 if mono else 0x00])
    return header + bytes(length - 4)


class ImagePrograms(unittest.TestCase):
    def test_r83_only_the_service_and_shared_libraries(self):
        entries = [
            ("vocab-tts", True, ELF),
            ("lib/aarch64-linux-gnu/libc.so.6", True, ELF),
            ("usr/lib/x86_64-linux-gnu/libstdc++.so.6.0.33", True, ELF),
            ("lib/ld-linux-aarch64.so.1", True, ELF),
            ("lib64/ld-linux-x86-64.so.2", True, ELF),
            ("model/model.int8.onnx", True, b"\x08\x07\x12\x07"),
            ("licenses/SOURCE.md", True, b"# Co"),
            ("etc/passwd", True, b"root"),
        ]
        self.assertEqual(smoke.image_programs(entries), [])

    def test_r83_shells_and_other_programs_are_found(self):
        entries = [
            ("bin/sh", False, b""),  # a symlink counts
            ("usr/bin/openssl", True, ELF),
            ("usr/local/bin/tool", True, ELF),
            ("lib/helper", True, ELF),  # in a lib dir but not a shared library
            ("./bin/busybox", True, ELF),
        ]
        self.assertEqual(
            smoke.image_programs(entries),
            ["bin/busybox", "bin/sh", "lib/helper", "usr/bin/openssl", "usr/local/bin/tool"],
        )


class Mp3Frames(unittest.TestCase):
    def test_r40_counts_frames_and_duration(self):
        data = frame() * 50 + frame(bitrate_index=5, padding=1) * 10
        frames, rate, duration_ms = smoke.mp3_frames(data)
        self.assertEqual((frames, rate), (60, 24000))
        self.assertEqual(duration_ms, 60 * 576 * 1000 // 24000)

    def test_r40_rejects_what_is_not_a_clean_mono_mpeg2_stream(self):
        cases = {
            "empty": b"",
            "garbage": b"ID3\x04" + bytes(100),
            "truncated": frame() * 3 + frame()[:50],
            "stereo": frame(mono=False),
            "bad bitrate": bytes([0xFF, 0xF3, 0xF4, 0xC0]) + bytes(100),
            "rate change": frame(rate_index=1) + frame(rate_index=0),
            "mpeg-1": bytes([0xFF, 0xFB, 0x90, 0xC0]) + bytes(400),
        }
        for label, data in cases.items():
            with self.subTest(label), self.assertRaises(ValueError):
                smoke.mp3_frames(data)


class Memory(unittest.TestCase):
    def test_r83_docker_and_podman_memory_figures(self):
        self.assertEqual(smoke.memory_bytes("910.3MB / 1.342GB"), 910_300_000)
        self.assertEqual(smoke.memory_bytes("812MiB / 1.25GiB"), 812 * 2**20)
        self.assertEqual(smoke.memory_bytes("1.1GiB / 1.25GiB"), int(1.1 * 2**30))
        self.assertEqual(smoke.memory_bytes("512kB / 1GB"), 512_000)
        self.assertIsNone(smoke.memory_bytes("--"))

    def test_r83_cgroup_numbers(self):
        stat = "anon 4194304\nfile 121634816\nkernel 917504\nanon_thp 0\nfile_mapped 0\n"
        self.assertEqual(
            smoke.cgroup_numbers("130023424\n", "400556032\n", stat),
            {"current": 130023424, "peak": 400556032, "anon": 4194304, "file": 121634816},
        )
        # Older kernels have no memory.peak.
        self.assertEqual(smoke.cgroup_numbers("1048576", None, "anon 1\n"), {"current": 1048576, "anon": 1})
        self.assertEqual(smoke.mib({"current": 3 << 20, "anon": 1 << 20}), "current 3 MiB, anon 1 MiB")

    def test_r83_limit_covers_the_measured_peak(self):
        # docs/design/tts-spike.md, "Small memory": a peak of 413 MiB for
        # the worst accepted texts in one engine process (arm64; amd64 is
        # measured by CI), plus a margin.
        self.assertGreater(smoke.MEMORY_LIMIT_BYTES, 413 * 2**20 * 1.2)
        self.assertLess(smoke.MEMORY_LIMIT_BYTES, 1280 * 2**20)

    def test_r83_the_smoke_test_runs_under_the_compose_limit(self):
        compose = (TOOLS.parent / "deploy" / "docker-compose.example.yml").read_text()
        tts = compose.split("  vocabulary-tts:\n", 1)[1]
        self.assertIn(f"    mem_limit: {smoke.MEMORY_LIMIT}\n", tts)
        self.assertEqual(smoke.MEMORY_LIMIT_BYTES, int(smoke.MEMORY_LIMIT.rstrip("m")) * 2**20)


if __name__ == "__main__":
    unittest.main()
