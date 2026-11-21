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
            ("model/model.onnx", True, b"\x08\x07\x12\x07"),
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

    def test_r83_limit_covers_the_measured_peak(self):
        # docs/design/tts-spike.md: about 0.91 GB peak for the longest text.
        self.assertGreater(smoke.MEMORY_LIMIT_BYTES, 910_000_000 * 1.3)


if __name__ == "__main__":
    unittest.main()
