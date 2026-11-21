"""Self-tests for tools/licenses.py: licence choice, licence files, the
notices file and the image label. Offline: cargo metadata is faked.

Run: python3 -m unittest discover -s tools/tests -v
"""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("licenses", TOOLS / "licenses.py")
assert _spec and _spec.loader
licenses = importlib.util.module_from_spec(_spec)
sys.modules["licenses"] = licenses  # dataclasses look the module up by name
_spec.loader.exec_module(licenses)

MIT = "Permission is hereby granted, free of charge, to any person ...\n"
APACHE = "Apache License\nVersion 2.0, January 2004\n"


class Choice(unittest.TestCase):
    def test_or_takes_the_preferred_option_and_and_takes_all(self):
        cases = {
            "MIT OR Apache-2.0": ["MIT"],
            "Apache-2.0 OR MIT": ["MIT"],
            "MIT/Apache-2.0": ["MIT"],
            "Unlicense OR MIT": ["MIT"],
            "Apache-2.0 OR BSL-1.0": ["Apache-2.0"],
            "(MIT OR Apache-2.0) AND Unicode-3.0": ["MIT", "Unicode-3.0"],
            "MIT AND BSD-3-Clause": ["MIT", "BSD-3-Clause"],
            "LGPL-3.0-or-later OR MPL-2.0": ["MPL-2.0"],
            "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT": ["MIT"],
            "Apache-2.0 WITH LLVM-exception": ["Apache-2.0 WITH LLVM-exception"],
        }
        for expression, chosen in cases.items():
            with self.subTest(expression=expression):
                self.assertEqual(licenses.choose(expression), chosen)

    def test_unacceptable_or_broken_expressions_fail(self):
        for expression in ("GPL-3.0-only", "MIT AND GPL-2.0", "(MIT OR", "MIT OR OR Apache-2.0", ""):
            with self.subTest(expression=expression), self.assertRaises(ValueError):
                licenses.choose(expression)

    def test_combined_expression_starts_with_the_own_licence(self):
        parts = [licenses.Component("a", "1", "x", ["MPL-2.0"]), licenses.Component("b", "1", "x", ["Apache-2.0", "MIT"])]
        self.assertEqual(licenses.combined(parts), "MIT AND Apache-2.0 AND MPL-2.0")


class Files(unittest.TestCase):
    def test_files_are_classified_by_name_then_content(self):
        self.assertEqual(licenses.classify("LICENSE-MIT", ""), {"MIT"})
        self.assertEqual(licenses.classify("LICENSE-APACHE", ""), {"Apache-2.0"})
        self.assertEqual(licenses.classify("MPL-2.0.txt", ""), {"MPL-2.0"})
        self.assertEqual(licenses.classify("LICENSE", MIT), {"MIT"})
        self.assertEqual(licenses.classify("COPYING", APACHE), {"Apache-2.0"})
        self.assertEqual(licenses.classify("NOTICE", "x"), {"NOTICE"})
        self.assertTrue(licenses.LICENSE_FILE.match("LICENSE.httprouter"))
        self.assertFalse(licenses.LICENSE_FILE.match("README.md"))

    def test_only_the_chosen_licence_texts_and_apache_notices(self):
        files = {"LICENSE-MIT": MIT, "LICENSE-APACHE": APACHE, "NOTICE": "notice\n"}
        self.assertEqual(licenses.texts_for(["MIT"], files), ([MIT], []))
        self.assertEqual(licenses.texts_for(["Apache-2.0"], files), ([APACHE, "notice\n"], []))
        self.assertEqual(licenses.texts_for(["Zlib"], files), ([], ["Zlib"]))

    def test_missing_mit_text_gets_the_standard_text_with_the_authors(self):
        text = licenses.standard_text("MIT", ["Jane Doe <jane@example.com>"], "x")
        self.assertIn("Copyright (c) Jane Doe\n", text)
        with self.assertRaises(ValueError):
            licenses.standard_text("MPL-2.0", [], "x")


def metadata(root: Path) -> dict:
    """A workspace `app` -> a (normal) -> b (normal); app -> c (dev only), a -> d (build only)."""
    def pkg(name: str, licence: str, files: dict[str, str]) -> dict:
        directory = root / f"{name}-1.0.0"
        directory.mkdir()
        (directory / "Cargo.toml").write_text("")
        for file, text in files.items():
            (directory / file).write_text(text)
        return {"id": f"registry+x#{name}@1.0.0", "name": name, "version": "1.0.0", "license": licence,
                "source": "registry+https://github.com/rust-lang/crates.io-index", "authors": [],
                "manifest_path": str(directory / "Cargo.toml"), "repository": None}

    app = {"id": "path+file:///app#0.1.0", "name": "vocab-server", "version": "0.1.0", "license": "MIT", "source": None}
    packages = [app, pkg("a", "MIT OR Apache-2.0", {"LICENSE-MIT": MIT}), pkg("b", "Apache-2.0", {"LICENSE": APACHE}),
                pkg("c", "GPL-3.0-only", {}), pkg("d", "GPL-3.0-only", {})]
    normal, dev, build = [{"kind": None, "target": None}], [{"kind": "dev", "target": None}], [{"kind": "build", "target": None}]
    nodes = [
        {"id": app["id"], "deps": [{"pkg": "registry+x#a@1.0.0", "dep_kinds": normal},
                                   {"pkg": "registry+x#c@1.0.0", "dep_kinds": dev}]},
        {"id": "registry+x#a@1.0.0", "deps": [{"pkg": "registry+x#b@1.0.0", "dep_kinds": normal},
                                              {"pkg": "registry+x#d@1.0.0", "dep_kinds": build}]},
        {"id": "registry+x#b@1.0.0", "deps": []},
        {"id": "registry+x#c@1.0.0", "deps": []},
        {"id": "registry+x#d@1.0.0", "deps": []},
    ]
    return {"packages": packages, "workspace_members": [app["id"]], "resolve": {"nodes": nodes}}


class Notices(unittest.TestCase):
    def test_only_shipped_crates_and_each_text_once(self):
        with tempfile.TemporaryDirectory() as tmp:
            meta = metadata(Path(tmp))
            shipped = licenses.shipped_packages(meta)
            self.assertEqual([p["name"] for p in shipped], ["a", "b"])  # no dev or build dependency
            components = [licenses.crate_component(p) for p in shipped] + licenses.extras(shipped)
        text = licenses.render(components)
        self.assertIn("  a 1.0.0: MIT OR Apache-2.0 -> MIT\n", text)
        self.assertIn("  b 1.0.0: Apache-2.0 -> Apache-2.0\n", text)
        self.assertIn("musl libc", text)
        self.assertNotIn(tmp, text)  # no local paths
        self.assertEqual(text.count("Apache License"), 1)
        self.assertIn("Combined licence expression: MIT AND Apache-2.0 AND Apache-2.0 WITH LLVM-exception\n", text)

    def test_label_must_match(self):
        dockerfile = 'LABEL org.opencontainers.image.licenses="MIT AND Apache-2.0"\n'
        self.assertIsNone(licenses.label_problem(dockerfile, "MIT AND Apache-2.0"))
        self.assertIn("expected", licenses.label_problem(dockerfile, "MIT"))
        self.assertIn("no org.opencontainers", licenses.label_problem("FROM x\n", "MIT"))

    def test_repository_label_and_notices_agree(self):
        root = TOOLS.parent
        notices = (root / licenses.OUTPUT).read_text()
        expression = notices.split("Combined licence expression: ", 1)[1].split("\n", 1)[0]
        self.assertIsNone(licenses.label_problem((root / licenses.DOCKERFILE).read_text(), expression))

    def test_r83_tts_image_label_and_notices_agree(self):
        root = TOOLS.parent
        image = licenses.IMAGES["tts"]
        notices = (root / image.output).read_text()
        expression = notices.split("Combined licence expression: ", 1)[1].split("\n", 1)[0]
        self.assertIsNone(licenses.label_problem((root / image.dockerfile).read_text(), expression, image.dockerfile))
        # What the Dockerfile adds besides crates is listed with its licence.
        for component in ("ONNX Runtime", "LAME 3.100", "CMUdict", "Kokoro-82M", "kissfft", "Eigen", "abseil"):
            self.assertIn(component, notices)
        for spdx in ("LGPL-2.0-or-later", "MPL-2.0", "BSL-1.0", "BSD-2-Clause"):
            self.assertIn(spdx, expression)
        # ONNX Runtime's own notices, which the TTS notices refer to, are shipped.
        self.assertIn(licenses.ORT_NOTICES, (root / image.dockerfile).read_text())

    def test_deprecated_ids_and_lgpl_texts(self):
        self.assertEqual(licenses.choose("LGPL-3.0"), ["LGPL-3.0-only"])
        self.assertEqual(licenses.classify("LICENSE", "GNU LESSER GENERAL PUBLIC LICENSE\n  Version 3, 29 June 2007"),
                         {"LGPL-3.0-only", "GPL"})
        self.assertIn("LGPL-2.0-or-later",
                      licenses.classify("COPYING", "GNU LIBRARY GENERAL PUBLIC LICENSE\n Version 2, June 1991"))


if __name__ == "__main__":
    unittest.main()
