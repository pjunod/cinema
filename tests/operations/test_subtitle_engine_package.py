"""Pinned recognizer identity, model metadata and fail-closed packaging."""
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import shutil
import shlex
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


def portable_installer_verifier(directory):
    """Exercise shell validation portably, never simulate native qualification.

    The production installer keeps its absolute macOS system tools. This test
    copy adapts only their file/JSON/hash formats; codesign explicitly refuses.
    Native macOS assertions also run the original unmodified verifier.
    """
    adapter = directory / "system-tool-fixture.py"
    adapter.write_text(r'''import hashlib
import json
from pathlib import Path
import plistlib
import re
import sys

mode, *args = sys.argv[1:]
if mode == "plutil":
    assert args[0] == "-extract" and args[3:5] == ["-o", "-"]
    value = json.loads(Path(args[5]).read_text())
    for part in re.split(r"(?<!\\)\.", args[1]):
        value = value[part.replace(r"\.", ".")]
    if args[2] == "xml1":
        sys.stdout.buffer.write(plistlib.dumps(value, fmt=plistlib.FMT_XML, sort_keys=True))
    else:
        assert args[2] == "raw"
        print(value)
elif mode == "stat":
    assert args[:2] == ["-f", "%z"]
    print(Path(args[2]).stat().st_size)
elif mode == "shasum":
    assert args[:2] == ["-a", "256"]
    data = Path(args[2]).read_bytes() if len(args) == 3 else sys.stdin.buffer.read()
    print(hashlib.sha256(data).hexdigest())
elif mode == "codesign":
    print("native signature qualification deliberately unavailable in portable fixture", file=sys.stderr)
    sys.exit(99)
else:
    raise AssertionError(mode)
''')
    source = (ROOT / "deploy/verify-subtitle-engine").read_text()
    for tool in ["plutil", "stat", "shasum", "codesign"]:
        source = source.replace("/usr/bin/" + tool,
            " ".join(shlex.quote(part) for part in [sys.executable, str(adapter), tool]))
    verifier = directory / "portable-verifier"
    verifier.write_text(source)
    return verifier


class SubtitleEnginePackageCase(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.package = self.root / "engine"
        self.facts = fixture_bundle(self.package)

    @property
    def portable_verifier(self):
        verifier = self.root / "portable-verifier"
        return verifier if verifier.exists() else portable_installer_verifier(self.root)

    def installer_verifiers(self):
        yield self.portable_verifier
        if sys.platform == "darwin":
            yield ROOT / "deploy/verify-subtitle-engine"

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
        expected["small"] = (487601967, "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b")
        self.assertEqual({item["id"] for item in catalog["models"]}, set(expected))
        for item in catalog["models"]:
            self.assertEqual((item["download_bytes"], item["sha256"]), expected[item["id"]])
            self.assertEqual(item["license"], "MIT")
            self.assertIn("/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/", item["url"])
            # This base model has n_vocab=51865: 99 language tokens. Engine
            # language ID 99 (yue) belongs to the newer 51866-vocabulary models.
            self.assertEqual(len(item["languages"]), 99 if item["id"] in ("base", "small") else 1)
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

    def test_macos_installer_real_verifier_rejects_changed_binary_before_execution(self):
        (self.package / "whisper-cli").write_bytes(b"changed executable")
        for verifier in self.installer_verifiers():
            with self.subTest(verifier=verifier.name):
                result = subprocess.run(["/bin/bash", str(verifier), str(self.package)], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("executable length mismatch", result.stderr)

    def test_macos_installer_rejects_unqualified_and_extra_build_options(self):
        for options in [dict(self.facts["build_options"], GGML_NATIVE="ON"), dict(self.facts["build_options"], UNKNOWN="ON")]:
            facts = dict(self.facts, build_options=options)
            (self.package / "subtitle-engine.json").write_text(json.dumps(facts))
            for verifier in self.installer_verifiers():
                with self.subTest(verifier=verifier.name, options=options):
                    result = subprocess.run(["/bin/bash", str(verifier), str(self.package)], capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("unqualified build options", result.stderr)

    def test_portable_verifier_reaches_but_never_fakes_native_signature_qualification(self):
        result = subprocess.run(["/bin/bash", str(self.portable_verifier), str(self.package)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 99, result.stderr)
        self.assertIn("native signature qualification deliberately unavailable", result.stderr)


if __name__ == "__main__":
    unittest.main()
