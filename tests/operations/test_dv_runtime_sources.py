"""Offline runtime source-pin and bounded extraction contracts; no native build."""
import hashlib
import io
import json
from pathlib import Path
import runpy
import tarfile
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
RUNTIME_PATH = ROOT / "tools/dv_processing/fetch_sources.py"
GENERIC_PATH = ROOT / "tools/dv_quality/backends/libplacebo/fetch_sources.py"
RUNTIME = runpy.run_path(str(RUNTIME_PATH))
GENERIC = runpy.run_path(str(GENERIC_PATH))


class RuntimeSourcesTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)

    def archive(self, component, revision, member=None, symlink=False):
        filename = component + ".tar.gz"
        path = self.root / filename
        with tarfile.open(path, "w:gz") as archive:
            entry = tarfile.TarInfo(member or f"{component}-{revision}/LICENSE")
            if symlink:
                entry.type = tarfile.SYMTYPE
                entry.linkname = "../../outside"
                archive.addfile(entry)
            else:
                payload = b"public-source-fixture"
                entry.size = len(payload)
                archive.addfile(entry, io.BytesIO(payload))
        return dict(component=component, revision=revision, filename=filename,
                    url="https://invalid.example/never-requested",
                    sha256=hashlib.sha256(path.read_bytes()).hexdigest())

    def test_current_runtime_pin_does_not_redefine_historical_renderer(self):
        current = RUNTIME["LIBPLACEBO"]
        self.assertEqual(current["revision"], "v7.387.0")
        self.assertEqual(current["sha256"],
                         "3bf5d72786682c194387c33d563872f6bfe202a9816f9dde6535a31a9b2e0d74")
        self.assertEqual(current["url"],
                         "https://code.videolan.org/videolan/libplacebo/-/archive/v7.387.0/libplacebo-v7.387.0.tar.gz")
        self.assertEqual(GENERIC["SOURCES"][0]["revision"],
                         "0d043c7f6f79cd3687c023454bdacbe615e4d96f")
        self.assertEqual(GENERIC["SOURCES"][0]["sha256"],
                         "fd72d09e83a7776f4ec5f2c2f13712ee085a97c75473de2041d09801c49db621")

    def test_runtime_wrapper_extracts_both_pins_and_records_relink_inputs(self):
        library = self.archive("libplacebo", "v7.387.0")
        headers = self.archive("Vulkan-Headers", "control")
        utility = dict(GENERIC, SOURCES=(headers,))
        function = RUNTIME["fetch_runtime"]
        with mock.patch.dict(function.__globals__, LIBPLACEBO=library), \
             mock.patch("runpy.run_path", return_value=utility), \
             mock.patch("urllib.request.urlopen", side_effect=AssertionError("network forbidden")):
            result = function(self.root, self.root)
        self.assertEqual(result.name, "libplacebo-v7.387.0")
        self.assertEqual((result / "3rdparty/Vulkan-Headers/LICENSE").read_bytes(),
                         b"public-source-fixture")
        self.assertEqual(json.loads((self.root / "source-lock.json").read_text()),
                         [library, headers])

    def test_hash_mismatch_refuses_before_source_extraction(self):
        source = self.archive("libplacebo", "v7.387.0")
        source["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "Source hash mismatch"):
            GENERIC["fetch"](self.root, source)
        self.assertFalse((self.root / "libplacebo-v7.387.0").exists())

    def test_unsafe_members_and_existing_symlink_destinations_refuse(self):
        for member, symlink in [("../escape", False),
                                ("wrong-root/LICENSE", False),
                                ("libplacebo-v7.387.0/link", True)]:
            with self.subTest(member=member):
                source = self.archive("libplacebo", "v7.387.0", member, symlink)
                with self.assertRaisesRegex(ValueError, "Unsafe source archive member"):
                    GENERIC["fetch"](self.root, source)
        source = self.archive("libplacebo", "v7.387.0")
        outside = self.root / "outside"
        outside.mkdir()
        (self.root / "libplacebo-v7.387.0").symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "destination has a symlink"):
            GENERIC["fetch"](self.root, source)
        self.assertEqual(list(outside.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
