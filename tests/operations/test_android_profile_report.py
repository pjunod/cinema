"""Source scaffold checks; samples below are fixtures, never device evidence."""
import importlib.machinery
import importlib.util
from pathlib import Path
import unittest
import re

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("android_profile_report", str(ROOT / "scripts/android-profile-report"))
spec = importlib.util.spec_from_loader(loader.name, loader)
reporter = importlib.util.module_from_spec(spec)
loader.exec_module(reporter)


class AndroidReleaseProfileReportCase(unittest.TestCase):
    def fixture(self):
        common = dict(schema="plurx_release_profile_ttff_v1", source_sha="a" * 40, apk_sha256="b" * 64,
                      package="tv.plurx.app", version_code=135, device_alias="fixture-device",
                      device_model="fixture-model", sdk=34, startup_mode="COLD", iterations=5,
                      media3_ttff_ms=[100, 120, 110, 130, 90], first_frame_boundary="fixture-boundary")
        without = dict(common, compilation_mode="none")
        required = dict(common, compilation_mode="required")
        macro = {"benchmarks": [{"className": "tv.plurx.profile.ReleaseProfileMeasurement", "name": n,
                                 "metrics": {"timeToInitialDisplayMs": {"runs": [5, 3, 4, 6, 2]}}}
                                for n in ("withoutProfileFiveColdIterations", "requiredProfileFiveColdIterations")]}
        signed = dict(package=common["package"], version_code=135, artifact_sha256="b" * 64,
                      source_candidate="a" * 40, signature_verified=True, debuggable=False)
        return without, required, macro, signed

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

    def test_missing_or_malformed_device_identity_and_boundary_is_rejected(self):
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
