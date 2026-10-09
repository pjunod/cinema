"""Packaging hash controls only; synthetic files never establish video authority."""
import contextlib
import io
import json
from pathlib import Path
import runpy
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PACKAGING = runpy.run_path(str(ROOT / "tools/dv_processing/bundle-manifest.py"))


class DvBundleManifestTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.bundle = Path(self.scratch.name) / "bundle"
        for folder in ("bin", "lib", "sources"):
            (self.bundle / folder).mkdir(parents=True)
        for name in ("segment_decode_render", "mux_rgb", "author_p81"):
            (self.bundle / "bin" / name).write_bytes(b"packaging-only synthetic artifact")
        (self.bundle / "lib/libplacebo.so.374").write_bytes(b"packaging-only library")
        (self.bundle / "sources/libdovi.a").write_bytes(b"packaging-only parser")
        (self.bundle / "sources/abi.json").write_text(json.dumps(
            {"libplacebo": 374, "avcodec": 63, "avformat": 63, "avutil": 61}))
        self.source = ROOT / "tools/dv_processing"
        with contextlib.redirect_stdout(io.StringIO()):
            PACKAGING["build"](self.source, self.bundle)

    def verify(self):
        with contextlib.redirect_stdout(io.StringIO()):
            PACKAGING["verify"](self.source, self.bundle)

    def test_installed_renderer_and_library_hash_tampering_is_refused(self):
        self.verify()
        for relative in ("bin/segment_decode_render", "lib/libplacebo.so.374"):
            artifact = self.bundle / relative
            original = artifact.read_bytes()
            artifact.write_bytes(original + b"tampered")
            with self.assertRaises(AssertionError):
                self.verify()
            artifact.write_bytes(original)
        self.verify()

    def test_installed_artifact_escape_and_machine_icd_override_are_refused(self):
        path = self.bundle / "build-identity.json"
        original = json.loads(path.read_text())
        outside = Path(self.scratch.name) / "outside"
        outside.write_bytes(b"outside root")
        changed = json.loads(json.dumps(original))
        changed["libraries"][0] = {"path": "../outside", "sha256": PACKAGING["sha"](outside)}
        path.write_text(json.dumps(changed))
        with self.assertRaises(AssertionError):
            self.verify()
        changed = json.loads(json.dumps(original))
        changed["environment"]["VK_ICD_FILENAMES"] = "specific-machine.json"
        path.write_text(json.dumps(changed))
        with self.assertRaises(AssertionError):
            self.verify()


if __name__ == "__main__":
    unittest.main()
