"""Source scaffold checks; samples below are fixtures, never device evidence."""
import importlib.machinery
import importlib.util
from pathlib import Path
import unittest
import re
import json
import hashlib
import base64

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("android_profile_report", str(ROOT / "scripts/android-profile-report"))
spec = importlib.util.spec_from_loader(loader.name, loader)
reporter = importlib.util.module_from_spec(spec)
loader.exec_module(reporter)


class AndroidReleaseProfileReportCase(unittest.TestCase):
    def fixture(self):
        common = dict(schema="plurx_release_profile_ttff_v2", source_sha="a" * 40, apk_sha256="b" * 64,
                      package="tv.plurx.app", version_code=135, device_alias="fixture-device",
                      device_model="fixture-model", sdk=34, device_fingerprint="fixture-fingerprint",
                      device_build_device="fixture-hardware", run_id="1" * 32, startup_mode="COLD", iterations=5,
                      media3_ttff_ms=[100, 120, 110, 130, 90], first_frame_boundary="fixture-boundary")
        without = dict(common, compilation_mode="none")
        required = dict(common, compilation_mode="required")
        fields = [str(common[k]) for k in ("source_sha", "apk_sha256", "package", "version_code", "device_alias",
                                           "device_model", "sdk", "device_fingerprint", "device_build_device")]
        binding = hashlib.sha256("".join(f"{len(v.encode('utf-8'))}:{v}" for v in fields).encode()).hexdigest()
        benchmark_binding = base64.urlsafe_b64encode(bytes.fromhex(binding)).decode("ascii").rstrip("=")
        macro = {"context": {"build": {"model": common["device_model"], "fingerprint": common["device_fingerprint"],
                                       "device": common["device_build_device"], "version": {"sdk": 34}}},
                 "benchmarks": [{"className": "tv.plurx.profile.ReleaseProfileMeasurement",
                                 "name": f"{n}[run={common['run_id']},binding={benchmark_binding}]",
                                 "params": {"run": common["run_id"], "binding": benchmark_binding},
                                 "repeatIterations": 5, "warmupIterations": 0,
                                 "metrics": {"timeToInitialDisplayMs": {"runs": [5, 3, 4, 6, 2]}}}
                                for n in ("withoutProfileFiveColdIterations", "requiredProfileFiveColdIterations")]}
        macro_hash = hashlib.sha256(json.dumps(macro).encode()).hexdigest()
        for ttff in (without, required):
            ttff.update(invocation_binding_sha256=binding, macrobenchmark_sha256=macro_hash)
        signed = dict(package=common["package"], version_code=135, artifact_sha256="b" * 64,
                      source_candidate="a" * 40, signature_verified=True, debuggable=False)
        return without, required, macro, signed, macro_hash

    def test_complete_pair_keeps_raw_samples_and_medians_without_acceptance(self):
        report = reporter.build_report(*self.fixture())
        self.assertEqual(report["modes"]["none"]["startup_median_ms"], 4)
        self.assertEqual(report["modes"]["required"]["media3_ttff_median_ms"], 110)
        self.assertEqual(len(report["modes"]["none"]["media3_ttff_ms"]), 5)
        self.assertFalse(report["measured_gain_claimed"])
        self.assertIn("Lenovo", report["acceptance"])

    def test_mixed_apk_or_debug_receipt_is_rejected(self):
        for field, value in (("artifact_sha256", "c" * 64), ("debuggable", True), ("signature_verified", False)):
            with self.subTest(field=field):
                args = list(self.fixture()); args[3][field] = value
                with self.assertRaises(ValueError): reporter.build_report(*args)
        args = list(self.fixture()); args[1]["device_alias"] = "different-device"
        with self.assertRaises(ValueError): reporter.build_report(*args)

    def test_missing_iteration_or_nonfinite_sample_is_rejected(self):
        for values in ([1, 2, 3, 4], [1, 2, 3, 4, float("nan")], [1, 2, 3, 4, -1], [1, 2, 3, 4, True]):
            with self.subTest(values=values):
                args = list(self.fixture()); args[1]["media3_ttff_ms"] = values
                with self.assertRaises(ValueError): reporter.build_report(*args)

    def test_missing_startup_result_cannot_be_replaced_with_ttff(self):
        args = list(self.fixture()); args[2]["benchmarks"].pop()
        with self.assertRaises(ValueError): reporter.build_report(*args)

    def test_capture_tasks_require_release_signing_and_non_debuggable_target(self):
        gradle = (ROOT / "clients/android/app/build.gradle.kts").read_text()
        expression = re.search(r"gradle.startParameter.taskNames.any \{([^}]+)\}", gradle).group(1)
        tokens = re.findall(r'it.contains\("([^"\n]+)"\)', expression)
        for task in (":app:assembleRelease", ":app:assembleProfileCapture",
                     ":baselineprofile:connectedProfileCaptureAndroidTest",
                     ":baselineprofile:connectedReleaseAndroidTest"):
            self.assertTrue(any(token in task for token in tokens), task)
        self.assertFalse(any(token in ":app:assembleDebug" for token in tokens))
        capture = gradle.split('create("profileCapture") {', 1)[1].split("\n        }", 1)[0]
        self.assertIn('initWith(getByName("release"))', capture)
        self.assertIn('signingConfig = signingConfigs.getByName("release")', capture)
        self.assertIn("isDebuggable = false", capture)
        self.assertNotIn('signingConfigs.getByName("debug")', capture)
        self.assertIn("if (releaseTaskRequested)", gradle)
        for field in ("PLURX_ANDROID_KEYSTORE", "PLURX_ANDROID_KEYSTORE_PASSWORD",
                      "PLURX_ANDROID_KEY_ALIAS", "PLURX_ANDROID_KEY_PASSWORD"):
            self.assertIn(f'requiredSigningValue("{field}")', gradle)
        helper = gradle.split("fun requiredSigningValue", 1)[1].split("\n}", 1)[0]
        self.assertIn("?: error(", helper)
        self.assertIn("it.isNotBlank()", helper)
        module = (ROOT / "clients/android/baselineprofile/build.gradle.kts").read_text()
        self.assertIn('experimentalProperties["android.experimental.self-instrumenting"] = true', module)
        self.assertIn('targetProjectPath = ":app"', module)
        self.assertEqual(module.count('signingConfig = signingConfigs.getByName("debug")'), 2)

    def test_missing_or_malformed_device_identity_and_boundary_is_rejected(self):
        for field in ("device_alias", "device_model", "sdk", "version_code", "first_frame_boundary"):
            with self.subTest(missing=field):
                args = list(self.fixture())
                del args[0][field]; del args[1][field]
                with self.assertRaises(ValueError): reporter.build_report(*args)
        for field, value in (("device_alias", None), ("device_alias", ""),
                             ("device_alias", "bad alias"), ("device_model", None),
                             ("device_model", []), ("device_model", "  "),
                             ("sdk", None), ("sdk", True), ("sdk", 27),
                             ("version_code", False), ("version_code", 0),
                             ("first_frame_boundary", None), ("first_frame_boundary", "")):
            with self.subTest(field=field, value=value):
                args = list(self.fixture())
                args[0][field] = value; args[1][field] = value
                with self.assertRaises(ValueError): reporter.build_report(*args)
        args = list(self.fixture()); args[1]["first_frame_boundary"] = "different-boundary"
        with self.assertRaises(ValueError): reporter.build_report(*args)

    def test_stale_or_mixed_startup_run_and_apk_are_rejected(self):
        args = list(self.fixture()); args[4] = "c" * 64
        with self.assertRaisesRegex(ValueError, "original Macrobenchmark bytes"): reporter.build_report(*args)
        args = list(self.fixture()); args[1]["run_id"] = "2" * 32
        with self.assertRaises(ValueError): reporter.build_report(*args)
        args = list(self.fixture())
        for ttff in args[:2]: ttff["run_id"] = "2" * 32
        with self.assertRaisesRegex(ValueError, "matching Macrobenchmark"): reporter.build_report(*args)
        args = list(self.fixture())
        for ttff in args[:2]: ttff["apk_sha256"] = "c" * 64
        args[3]["artifact_sha256"] = "c" * 64
        with self.assertRaisesRegex(ValueError, "Invocation binding"): reporter.build_report(*args)
        for key in ("run_id", "invocation_binding_sha256", "macrobenchmark_sha256"):
            with self.subTest(missing=key):
                args = list(self.fixture())
                for ttff in args[:2]: del ttff[key]
                with self.assertRaises(ValueError): reporter.build_report(*args)

    def test_real_macrobenchmark_device_context_and_binding_are_required(self):
        for field, value in (("model", "other-model"), ("fingerprint", "other-fingerprint"),
                             ("device", "other-hardware"), ("version", {"sdk": 35})):
            with self.subTest(field=field):
                args = list(self.fixture()); args[2]["context"]["build"][field] = value
                # Even relabeling the freely supplied TTFF hashes cannot turn another device into this run.
                args[4] = hashlib.sha256(json.dumps(args[2]).encode()).hexdigest()
                for ttff in args[:2]: ttff["macrobenchmark_sha256"] = args[4]
                with self.assertRaisesRegex(ValueError, "device context differs"): reporter.build_report(*args)
        for mutation in ("context", "params", "binding", "iterations"):
            with self.subTest(mutation=mutation):
                args = list(self.fixture())
                if mutation == "context": del args[2]["context"]
                elif mutation == "params": del args[2]["benchmarks"][0]["params"]
                elif mutation == "binding": args[2]["benchmarks"][0]["params"]["binding"] = "c" * 64
                else: args[2]["benchmarks"][0]["repeatIterations"] = 4
                args[4] = hashlib.sha256(json.dumps(args[2]).encode()).hexdigest()
                for ttff in args[:2]: ttff["macrobenchmark_sha256"] = args[4]
                with self.assertRaises(ValueError): reporter.build_report(*args)
