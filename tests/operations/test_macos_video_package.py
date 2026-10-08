"""Package provenance integrity fences; no executable or hardware is launched."""
import hashlib
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("mac_video_package", str(ROOT / "scripts/build-macos-video-ffmpeg"))
spec = importlib.util.spec_from_loader(loader.name, loader)
TOOL = importlib.util.module_from_spec(spec)
loader.exec_module(TOOL)


class MacosVideoPackageCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.package = self.root / "parser"
        (self.package / "bin").mkdir(parents=True)
        (self.package / "provenance").mkdir()
        self.stage = self.root / "stage"
        self.stage.mkdir()
        self.evidence = self.stage / "provenance"
        self.evidence.mkdir()
        self.artifact = self.package / "bin/plurx-source-parser.wasm"
        self.artifact.write_bytes(b"\0asm\1\0\0\0")
        self.manifest = {"source_sha256": TOOL.SOURCE_SHA256, "source_commit": TOOL.COMMIT,
                         "parser_abi": "plurx-source-wasi-p1-v1-wasmtime-49.0.2",
                         "module_sha256": hashlib.sha256(self.artifact.read_bytes()).hexdigest(), "module_bytes": 8}
        self.write_manifest()

    def write_manifest(self):
        (self.package / "provenance/manifest.json").write_text(json.dumps(self.manifest))

    def test_parser_companion_is_hash_bound_to_native_source_and_retained_provenance(self):
        facts = TOOL.attach_source_parser(self.package, self.stage, self.evidence)
        self.assertEqual((self.stage / "plurx-source-parser.wasm").read_bytes(), self.artifact.read_bytes())
        self.assertEqual(facts["sha256"], self.manifest["module_sha256"])
        self.assertIn("manifest.json", facts["provenance_sha256"])
        self.assertEqual(facts["qualification"], "package-integrity-only")

    def test_parser_from_other_source_or_abi_is_rejected(self):
        for key in ("source_sha256", "source_commit", "parser_abi"):
            original = self.manifest[key]
            self.manifest[key] = "wrong"
            self.write_manifest()
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "source/ABI"):
                TOOL.attach_source_parser(self.package, self.stage, self.evidence)
            self.manifest[key] = original

    def test_corrupted_parser_companion_is_rejected_before_publication(self):
        self.artifact.write_bytes(b"\0asm\1\0\0\1")
        with self.assertRaisesRegex(ValueError, "does not match"):
            TOOL.attach_source_parser(self.package, self.stage, self.evidence)
        self.assertFalse((self.stage / "plurx-source-parser.wasm").exists())

    def test_parser_provenance_cannot_include_symlinks_or_git_credentials(self):
        unsafe = self.package / "provenance/outside"
        unsafe.symlink_to(self.artifact)
        with self.assertRaisesRegex(ValueError, "unsafe member"):
            TOOL.attach_source_parser(self.package, self.stage, self.evidence)
        unsafe.unlink()
        git = self.package / "provenance/.git"
        git.mkdir()
        (git / "config").write_text("not a source artifact")
        with self.assertRaisesRegex(ValueError, "Git checkout"):
            TOOL.attach_source_parser(self.package, self.stage, self.evidence)
