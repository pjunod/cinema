"""Native package integrity and atomic publication; no binaries are executed."""
import hashlib
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import subprocess
import struct
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("macos_daemon_package_test", str(ROOT / "scripts/package-macos"))
spec = importlib.util.spec_from_loader(loader.name, loader)
TOOL = importlib.util.module_from_spec(spec)
loader.exec_module(TOOL)


class MacosDaemonPackageCase(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.package = self.root / "native"
        self.package.mkdir()
        provenance = self.package / "provenance/source-parser"
        provenance.mkdir(parents=True)
        for name in ["ffmpeg", "ffprobe"]:
            # Distinct ARM64 Mach-O-shaped regular images and the real native
            # manifest schema: copying/reblessing changed bytes must fail before signing.
            header = struct.pack("<8I", 0xFEEDFACF, 0x0100000C, 0, 2, 0, 0, 0, 0)
            (self.package / name).write_bytes(header + name.encode())
        module = self.package / "plurx-source-parser.wasm"
        module.write_bytes(b"\0asm\1\0\0\0")
        recipe = TOOL.recipe()
        self.manifest = {"source_sha256": recipe.SOURCE_SHA256, "source_commit": recipe.COMMIT,
                         "parser_abi": "plurx-source-wasi-p1-v1-wasmtime-49.0.2", "module_bytes": 8,
                         "module_sha256": hashlib.sha256(module.read_bytes()).hexdigest()}
        (provenance / "manifest.json").write_text(json.dumps(self.manifest))
        self.native_manifest = {"source_sha256": recipe.SOURCE_SHA256, "source_commit": recipe.COMMIT,
            "binaries": {name: {"sha256": hashlib.sha256((self.package / name).read_bytes()).hexdigest()}
                         for name in ["ffmpeg", "ffprobe"]},
            "source_parser": {"sha256": self.manifest["module_sha256"], "parser_abi": self.manifest["parser_abi"]}}
        (self.package / "provenance/manifest.json").write_text(json.dumps(self.native_manifest))
        self.daemon = self.root / "plurxd"
        self.daemon.write_bytes(b"original daemon")
        self.output = self.root / "candidate"

    def test_companion_is_unique_and_bound_to_the_native_package_receipt(self):
        self.assertEqual(TOOL.validate_native_package(self.package), self.manifest)
        duplicate = self.package / "other/plurx-source-parser.wasm"
        duplicate.parent.mkdir()
        duplicate.write_bytes(b"\0asm\1\0\0\0")
        with self.assertRaisesRegex(ValueError, "conflicting"):
            TOOL.validate_native_package(self.package)
        duplicate.unlink()
        self.native_manifest["source_parser"]["sha256"] = "wrong"
        (self.package / "provenance/manifest.json").write_text(json.dumps(self.native_manifest))
        with self.assertRaisesRegex(ValueError, "attest"):
            TOOL.validate_native_package(self.package)

    def test_corruption_or_symlink_cannot_choose_a_different_parser(self):
        module = self.package / "plurx-source-parser.wasm"
        module.write_bytes(b"\0asm\1\0\0\1")
        with self.assertRaisesRegex(ValueError, "provenance"):
            TOOL.validate_native_package(self.package)
        module.unlink()
        module.symlink_to(self.daemon)
        with self.assertRaisesRegex(ValueError, "unsafe"):
            TOOL.validate_native_package(self.package)

    def test_altered_native_binaries_cannot_be_reblessed_by_packaging(self):
        for name in ["ffmpeg", "ffprobe"]:
            with self.subTest(binary=name):
                image = self.package / name
                original = image.read_bytes()
                image.write_bytes(original + b"swapped regular executable")
                with self.assertRaisesRegex(ValueError, f"native {name}.*provenance"):
                    TOOL.assemble(self.daemon, self.package, self.output, "-")
                self.assertFalse(self.output.exists())
                image.write_bytes(original)

    def test_missing_or_mismatched_native_source_cannot_publish(self):
        for key in ["source_commit", "source_sha256"]:
            for value in [None, "wrong"]:
                with self.subTest(field=key, value=value):
                    manifest = dict(self.native_manifest)
                    if value is None:
                        manifest.pop(key)
                    else:
                        manifest[key] = value
                    (self.package / "provenance/manifest.json").write_text(json.dumps(manifest))
                    with self.assertRaisesRegex(ValueError, "native package provenance source"):
                        TOOL.assemble(self.daemon, self.package, self.output, "-")
                    self.assertFalse(self.output.exists())
        (self.package / "provenance/manifest.json").write_text(json.dumps(self.native_manifest))

    def test_symbol_uuid_mismatch_cannot_publish_a_candidate(self):
        symbols = self.root / "plurxd.dSYM"
        dwarf = symbols / "Contents/Resources/DWARF/plurxd"
        dwarf.parent.mkdir(parents=True)
        dwarf.write_bytes(b"debug fixture")
        with mock.patch.object(TOOL, "image_uuids", side_effect=[[("arm64", "first")], [("arm64", "other")]]):
            with self.assertRaisesRegex(ValueError, "UUID mismatch"):
                TOOL.assemble(self.daemon, self.package, self.output, "-", symbols)
        self.assertFalse(self.output.exists())
        self.assertEqual(dwarf.read_bytes(), b"debug fixture")

    def test_symbol_symlink_cannot_copy_an_external_file(self):
        symbols = self.root / "plurxd.dSYM"
        dwarf = symbols / "Contents/Resources/DWARF/plurxd"
        dwarf.parent.mkdir(parents=True)
        dwarf.symlink_to(self.daemon)
        with self.assertRaisesRegex(ValueError, "unsafe debug-symbol"):
            TOOL.assemble(self.daemon, self.package, self.output, "-", symbols)
        self.assertFalse(self.output.exists())
        self.assertTrue(dwarf.is_symlink())

    def test_failed_signing_does_not_publish_or_mutate_source_artifacts(self):
        original_daemon = self.daemon.read_bytes()
        original_native = {p.relative_to(self.package): p.read_bytes()
                           for p in self.package.rglob("*") if p.is_file()}
        with mock.patch.object(TOOL.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "codesign")):
            with self.assertRaises(subprocess.CalledProcessError):
                TOOL.assemble(self.daemon, self.package, self.output, "-")
        self.assertFalse(self.output.exists())
        self.assertEqual(self.daemon.read_bytes(), original_daemon)
        self.assertEqual({p.relative_to(self.package): p.read_bytes()
                          for p in self.package.rglob("*") if p.is_file()}, original_native)


if __name__ == "__main__":
    unittest.main()
