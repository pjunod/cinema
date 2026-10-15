"""Pinned recognizer identity, model metadata and fail-closed packaging."""
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


def load(name, filename):
    loader = importlib.machinery.SourceFileLoader(name, str(ROOT / filename))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


TOOL = load("subtitle_engine_package_test", "scripts/build-subtitle-engine")
SMOKE = load("subtitle_engine_smoke_test", "scripts/subtitle-engine-self-test")


def fixture_bundle(directory):
    directory.mkdir()
    (directory / "whisper-cli").write_bytes(b"qualified executable fixture")
    (directory / "whisper-cli").chmod(0o755)
    for source, name in [("licenses/whisper.cpp.LICENSE.txt", "whisper.cpp.LICENSE.txt"),
                         ("licenses/whisper-models.LICENSE.txt", "whisper-models.LICENSE.txt"),
                         ("crates/plurxd/src/subtitle_models/self-test.wav", "self-test.wav")]:
        shutil.copyfile(ROOT / source, directory / name)
    facts = {key: TOOL.PIN[key] for key in ["version", "adapter_contract", "source_commit", "source_archive_sha256", "build_options"]}
    facts.update(platform="macos-arm64", binary_sha256=TOOL.digest(directory / "whisper-cli"),
                 binary_bytes=(directory / "whisper-cli").stat().st_size,
                 companions={name: {"sha256": TOOL.digest(directory / name), "bytes": (directory / name).stat().st_size}
                             for name in ["self-test.wav", "whisper.cpp.LICENSE.txt", "whisper-models.LICENSE.txt"]})
    (directory / "subtitle-engine.json").write_text(json.dumps(facts))
    return facts


class SubtitleEnginePackageCase(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.package = self.root / "engine"
        self.facts = fixture_bundle(self.package)

    def test_engine_identity_rejects_changed_binary_source_options_and_companions(self):
        self.assertEqual(TOOL.verify(self.package), self.facts)
        binary = self.package / "whisper-cli"
        binary.write_bytes(binary.read_bytes() + b"modified")
        with self.assertRaisesRegex(ValueError, "digest/length"):
            TOOL.verify(self.package)
        binary.write_bytes(b"qualified executable fixture")
        for field, bad in [("adapter_contract", "other"), ("source_commit", "other"), ("build_options", {})]:
            changed = dict(self.facts, **{field: bad})
            (self.package / "subtitle-engine.json").write_text(json.dumps(changed))
            with self.assertRaisesRegex(ValueError, "unqualified"):
                TOOL.verify(self.package)
        (self.package / "subtitle-engine.json").write_text(json.dumps(self.facts))
        (self.package / "self-test.wav").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "companion"):
            TOOL.verify(self.package)

    def test_engine_identity_refuses_symlinks_and_missing_notices(self):
        binary = self.package / "whisper-cli"
        binary.rename(self.root / "external")
        binary.symlink_to(self.root / "external")
        with self.assertRaisesRegex(ValueError, "regular|missing"):
            TOOL.verify(self.package)
        binary.unlink()
        (self.root / "external").rename(binary)
        (self.package / "whisper-models.LICENSE.txt").unlink()
        with self.assertRaisesRegex(ValueError, "missing"):
            TOOL.verify(self.package)

    def test_source_archive_is_verified_before_extraction(self):
        archive = self.root / "source.tar.gz"
        archive.write_bytes(b"not the source archive")
        with self.assertRaisesRegex(ValueError, "source archive digest/length"):
            TOOL.unpack(archive, self.root)
        self.assertEqual(sorted(item.name for item in self.root.iterdir()), ["engine", "source.tar.gz"])

    def test_linux_architecture_cannot_be_mislabeled_without_cross_compilation(self):
        with mock.patch.object(TOOL.platform, "system", return_value="Linux"), mock.patch.object(TOOL.platform, "machine", return_value="aarch64"):
            with self.assertRaisesRegex(ValueError, "native build host"):
                TOOL.build(self.root / "output", architecture="x86_64")
        self.assertFalse((self.root / "output").exists())

    def test_catalog_pins_immutable_models_and_explicit_supported_languages(self):
        catalog = json.loads((ROOT / "crates/plurxd/src/subtitle_models/catalog.json").read_text())
        self.assertEqual(catalog["adapter_contract"], TOOL.PIN["adapter_contract"])
        expected = {"base.en": (147964211, "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"),
                    "base": (147951465, "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe"),
                    "tiny.en": (77704715, "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f")}
        self.assertEqual({item["id"] for item in catalog["models"]}, set(expected))
        for item in catalog["models"]:
            self.assertEqual((item["download_bytes"], item["sha256"]), expected[item["id"]])
            self.assertEqual(item["license"], "MIT")
            self.assertIn("/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/", item["url"])
            # This base model has n_vocab=51865: 99 language tokens. Engine
            # language ID 99 (yue) belongs to the newer 51866-vocabulary models.
            self.assertEqual(len(item["languages"]), 99 if item["id"] == "base" else 1)
            self.assertNotIn("yue", item["languages"])
            self.assertEqual(len(set(item["languages"])), len(item["languages"]))
            self.assertNotIn("*", item["languages"])

    def test_speech_fixture_requires_recognizable_words_and_valid_timing(self):
        provenance = json.loads((ROOT / "crates/plurxd/src/subtitle_models/self-test.provenance.json").read_text())
        self.assertEqual(TOOL.digest(self.package / "self-test.wav"), provenance["sha256"])
        self.assertEqual(SMOKE.check_output("WEBVTT\n\n00:00:00.000 --> 00:00:10.000\nAsk what you can do for your country.\n"), 1)
        for text in ["", "WEBVTT\n", "WEBVTT\n\n00:00:00.000 --> 00:00:10.000\nSilence.\n",
                     "WEBVTT\n\n00:00:10.000 --> 00:00:01.000\nAsk your country about you.\n"]:
            with self.assertRaises(ValueError):
                SMOKE.check_output(text)

    @unittest.skipUnless(sys.platform == "darwin", "macOS system verifier")
    def test_macos_installer_real_verifier_rejects_changed_binary_before_execution(self):
        (self.package / "whisper-cli").write_bytes(b"changed executable")
        result = subprocess.run(["/bin/bash", str(ROOT / "deploy/verify-subtitle-engine"), str(self.package)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("executable length mismatch", result.stderr)

    @unittest.skipUnless(sys.platform == "darwin", "macOS system verifier")
    def test_macos_installer_rejects_unqualified_and_extra_build_options(self):
        for options in [dict(self.facts["build_options"], GGML_NATIVE="ON"), dict(self.facts["build_options"], UNKNOWN="ON")]:
            facts = dict(self.facts, build_options=options)
            (self.package / "subtitle-engine.json").write_text(json.dumps(facts))
            result = subprocess.run(["/bin/bash", str(ROOT / "deploy/verify-subtitle-engine"), str(self.package)], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("unqualified build options", result.stderr)


if __name__ == "__main__":
    unittest.main()
