"""Pinned recognizer identity, model metadata and fail-closed packaging."""
import copy
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


def fixture_variant(directory, baseline):
    facts = copy.deepcopy(baseline)
    pin = copy.deepcopy(TOOL.PIN["cpu_variants"]["x86_64-avx2"])
    binary = directory / pin["filename"]
    binary.write_bytes(b"qualified static AVX2 executable fixture")
    binary.chmod(0o755)
    facts["platform"] = "macos-x86_64"
    facts["cpu_variants"] = {"x86_64-avx2": dict(pin,
        binary_sha256=TOOL.digest(binary), binary_bytes=binary.stat().st_size,
        runtime_dependencies=["/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1.0.0)"])}
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
        key = part.replace(r"\.", ".")
        value = value[int(key)] if isinstance(value, list) else value[key]
    if args[2] == "xml1":
        sys.stdout.buffer.write(plistlib.dumps(value, fmt=plistlib.FMT_XML, sort_keys=True))
    elif args[2] == "json":
        print(json.dumps(value))
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

    def test_optional_cpu_variant_preserves_baseline_and_pins_full_static_options(self):
        self.assertNotIn("cpu_variants", self.facts)
        self.assertEqual(TOOL.verify(self.package), self.facts)
        facts = fixture_variant(self.package, self.facts)
        self.assertEqual(TOOL.verify(self.package), facts)
        variant = facts["cpu_variants"]["x86_64-avx2"]
        self.assertEqual(variant["filename"], "whisper-cli-avx2")
        self.assertEqual(variant["required_features"], ["sse4.2", "avx", "avx2", "f16c", "fma", "bmi2"])
        self.assertEqual(set(variant["build_options"]), set(self.facts["build_options"]))
        changed = {key for key, value in variant["build_options"].items() if value != self.facts["build_options"][key]}
        self.assertEqual(changed, {"GGML_SSE42", "GGML_AVX", "GGML_AVX2", "GGML_F16C", "GGML_FMA", "GGML_BMI2"})
        self.assertTrue(all(variant["build_options"][key] == "ON" for key in changed))
        self.assertEqual(variant["build_options"]["BUILD_SHARED_LIBS"], "OFF")
        self.assertEqual(variant["build_options"]["GGML_BACKEND_DL"], "OFF")
        self.assertEqual(facts["build_options"], self.facts["build_options"])

    def test_cpu_variant_metadata_rejects_unknown_features_options_fields_and_architecture(self):
        baseline = fixture_variant(self.package, self.facts)
        cases = [
            ("features", lambda fact: fact["cpu_variants"]["x86_64-avx2"]["required_features"].pop()),
            ("options", lambda fact: fact["cpu_variants"]["x86_64-avx2"]["build_options"].update(GGML_NATIVE="ON")),
            ("extra options", lambda fact: fact["cpu_variants"]["x86_64-avx2"]["build_options"].update(UNKNOWN="ON")),
            ("filename", lambda fact: fact["cpu_variants"]["x86_64-avx2"].update(filename="../external")),
            ("extra field", lambda fact: fact["cpu_variants"]["x86_64-avx2"].update(unknown=True)),
            ("ARM target", lambda fact: fact.update(platform="macos-arm64")),
            ("unknown variant", lambda fact: fact["cpu_variants"].update(other={})),
            ("null variants", lambda fact: fact.update(cpu_variants=None)),
        ]
        for label, change in cases:
            with self.subTest(label=label):
                facts = copy.deepcopy(baseline)
                change(facts)
                (self.package / "subtitle-engine.json").write_text(json.dumps(facts))
                with self.assertRaisesRegex(ValueError, "CPU variant"):
                    TOOL.verify(self.package)

    def test_cpu_variant_member_identity_refuses_changed_missing_undescribed_and_symlinked_bytes(self):
        facts = fixture_variant(self.package, self.facts)
        binary = self.package / "whisper-cli-avx2"
        original = binary.read_bytes()
        binary.write_bytes(original + b"modified")
        with self.assertRaisesRegex(ValueError, "digest/length"):
            TOOL.verify(self.package)
        binary.write_bytes(original)
        binary.rename(self.root / "external-variant")
        binary.symlink_to(self.root / "external-variant")
        with self.assertRaisesRegex(ValueError, "regular"):
            TOOL.verify(self.package)
        binary.unlink()
        with self.assertRaisesRegex(ValueError, "missing"):
            TOOL.verify(self.package)
        (self.root / "external-variant").rename(binary)
        facts.pop("cpu_variants")
        (self.package / "subtitle-engine.json").write_text(json.dumps(facts))
        with self.assertRaisesRegex(ValueError, "unexpected"):
            TOOL.verify(self.package)

    def test_cpu_variant_dependency_observations_refuse_private_and_dynamic_engine_libraries(self):
        baseline = fixture_variant(self.package, self.facts)
        for dependencies in [None, [], ["/opt/private/libworker.dylib"], ["@rpath/libggml.dylib"],
                             ["/usr/lib/libgomp.dylib"], ["/usr/lib/libSystem.B.dylib\n/private/other"], ["/usr/lib/libSystem.B.dylib"] * 17]:
            with self.subTest(dependencies=dependencies):
                facts = copy.deepcopy(baseline)
                facts["cpu_variants"]["x86_64-avx2"]["runtime_dependencies"] = dependencies
                (self.package / "subtitle-engine.json").write_text(json.dumps(facts))
                with self.assertRaisesRegex(ValueError, "runtime|dependency"):
                    TOOL.verify(self.package)
        baseline["platform"] = "linux-x86_64"
        baseline["cpu_variants"]["x86_64-avx2"]["runtime_dependencies"] = ["libstdc++.so.6 => /lib/libstdc++.so.6", "/lib64/ld-linux-x86-64.so.2"]
        (self.package / "subtitle-engine.json").write_text(json.dumps(baseline))
        self.assertEqual(TOOL.verify(self.package), baseline)

    def test_native_verifiers_reject_variant_metadata_or_bytes_before_signature_or_execution(self):
        baseline = fixture_variant(self.package, self.facts)
        mutations = [
            lambda facts: facts["cpu_variants"]["x86_64-avx2"].update(binary_sha256="0" * 64),
            lambda facts: facts["cpu_variants"]["x86_64-avx2"]["required_features"].pop(),
            lambda facts: facts["cpu_variants"]["x86_64-avx2"]["build_options"].update(GGML_NATIVE="ON"),
            lambda facts: facts["cpu_variants"]["x86_64-avx2"].update(unknown=True),
            lambda facts: facts["cpu_variants"]["x86_64-avx2"].update(runtime_dependencies=["/private/libworker.dylib"]),
            lambda facts: facts.update(cpu_variants=None),
        ]
        for mutation in mutations:
            facts = copy.deepcopy(baseline)
            mutation(facts)
            (self.package / "subtitle-engine.json").write_text(json.dumps(facts))
            for verifier in self.installer_verifiers():
                with self.subTest(verifier=verifier.name, facts=facts):
                    result = subprocess.run(["/bin/bash", str(verifier), str(self.package)], capture_output=True, text=True)
                    self.assertNotIn(result.returncode, [0, 99], result.stderr)
                    self.assertNotIn("native signature qualification deliberately unavailable", result.stderr)

    def test_portable_verifier_accepts_variant_metadata_without_faking_native_signature(self):
        fixture_variant(self.package, self.facts)
        result = subprocess.run(["/bin/bash", str(self.portable_verifier), str(self.package)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 99, result.stderr)
        self.assertIn("native signature qualification deliberately unavailable", result.stderr)

    def test_universal_package_keeps_one_intel_variant_without_lipo_or_execution(self):
        intel = self.root / "intel-engine"
        intel_facts = fixture_variant(intel, fixture_bundle(intel))
        dependencies = ["/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1.0.0)"]
        commands = []
        def assemble(argv, **kwargs):
            commands.append(argv)
            if argv[:2] == ["/usr/bin/lipo", "-create"]:
                output = Path(argv[argv.index("-output") + 1])
                output.write_bytes(b"universal baseline fixture")
                output.chmod(0o755)
            return subprocess.CompletedProcess(argv, 0)
        with mock.patch.object(TOOL.platform, "system", return_value="Darwin"), mock.patch.object(TOOL, "run", side_effect=assemble), mock.patch.object(TOOL, "loader_dependencies", return_value=dependencies):
            for index, packages in enumerate([[self.package, intel], [intel, self.package]]):
                output = self.root / ("universal-" + str(index))
                facts = TOOL.combine(packages, output)
                self.assertEqual(facts["platform"], "macos-universal")
                self.assertEqual(facts["cpu_variants"], intel_facts["cpu_variants"])
                self.assertEqual((output / "whisper-cli-avx2").read_bytes(), (intel / "whisper-cli-avx2").read_bytes())
                self.assertEqual(TOOL.verify(output), facts)
        self.assertEqual(sum(command[:2] == ["/usr/bin/lipo", "-create"] for command in commands), 2)
        self.assertFalse(any("whisper-cli-avx2" in str(argument) for command in commands for argument in command))

    def test_builder_emits_pinned_static_variant_only_for_x86_without_executing_it(self):
        dependencies = ["libstdc++.so.6 => /lib/libstdc++.so.6", "/lib64/ld-linux-x86-64.so.2"]
        for machine in ["x86_64", "aarch64"]:
            built = []
            commands = []
            def compile_executable(source, build_root, binary, options, target, system, jobs, environment):
                built.append((source, build_root, binary.name, copy.deepcopy(options), target, environment))
                binary.write_bytes(("static fixture " + binary.name).encode())
                binary.chmod(0o755)
                return "pinned-compiler-fixture"
            def observe(argv, **kwargs):
                commands.append(argv)
                version = "whisper.cpp version: 1.9.2" if Path(argv[0]).name == "whisper-cli" else "compiler or CMake fixture"
                return subprocess.CompletedProcess(argv, 0, stdout=version + "\n")
            output = self.root / ("built-" + machine)
            with mock.patch.object(TOOL.platform, "system", return_value="Linux"), mock.patch.object(TOOL.platform, "machine", return_value=machine), mock.patch.object(TOOL, "unpack", return_value=self.root / "verified-source"), mock.patch.object(TOOL, "build_executable", side_effect=compile_executable), mock.patch.object(TOOL, "loader_dependencies", return_value=dependencies), mock.patch.object(TOOL, "run", side_effect=observe):
                facts = TOOL.build(output, archive=self.root / "pinned-source.tar.gz")
            self.assertEqual(len(built), 2 if machine == "x86_64" else 1)
            self.assertEqual(built[0][3], TOOL.PIN["build_options"])
            self.assertEqual(built[0][0], self.root / "verified-source")
            if machine == "x86_64":
                self.assertEqual(built[1][0], built[0][0])
                self.assertNotEqual(built[1][1], built[0][1])
                self.assertEqual(built[1][3], TOOL.PIN["cpu_variants"]["x86_64-avx2"]["build_options"])
                self.assertIn("x86_64-avx2", facts["cpu_variants"])
            else:
                self.assertNotIn("cpu_variants", facts)
            self.assertFalse(any(Path(command[0]).name == "whisper-cli-avx2" for command in commands))
            self.assertEqual(TOOL.verify(output), facts)

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
