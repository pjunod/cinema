"""Offline screens cannot masquerade as production qualification."""

import copy
import json
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SCREEN = runpy.run_path(str(ROOT / "scripts/encoder-calibration-screen"))


def complete_reports():
    fixtures = json.loads(SCREEN["CORPUS"].read_text())["fixtures"]
    return [{
        "status": "scored", "scope": SCREEN["SCOPE"], "fixture": fixture,
        "encoder_export": {"family": "qsv", "quality": 22},
        "encoder": {"build": "qualified-binary"}, "source_revision": "a" * 40,
        "bench_sha256": "b" * 64, "duration_seconds": 12,
        "corpus_sha256": SCREEN["BENCH"]["sha256_file"](SCREEN["CORPUS"]),
        "capture_environment": {"encoder_sha256": "d" * 64, "image_id": "sha256:image"},
        "scorer": {"sha256": "c" * 64, "model": "vmaf_v0.6.1"},
        "modes": {"vbr": {"bytes": 1000, "vmaf": 95},
                  "qvbr": {"bytes": 800, "vmaf": 95}},
    } for fixture in fixtures]


class EncoderCalibrationScreenTests(unittest.TestCase):
    def test_equal_frame_counts_do_not_hide_a_truncated_capture(self):
        video = {"width": 1920, "height": 1080, "nb_read_frames": "248", "avg_frame_rate": "24/1"}
        with self.assertRaisesRegex(ValueError, "truncated"):
            SCREEN["validate_frame_match"](video, video, 12)
        video["nb_read_frames"] = "288"
        SCREEN["validate_frame_match"](video, video, 12)

    def test_geometry_or_frame_mismatch_cannot_be_scored(self):
        reference = {"width": 1920, "height": 1080, "nb_read_frames": "288", "avg_frame_rate": "24/1"}
        for field, value in (("width", 1280), ("height", 720), ("nb_read_frames", "287")):
            with self.subTest(field=field), self.assertRaises(ValueError):
                SCREEN["validate_frame_match"](reference, {**reference, field: value}, 12)

    def test_beneficial_screen_never_qualifies_a_default(self):
        result = SCREEN["summarize"](complete_reports())
        self.assertTrue(result["benefit"]["passed"])
        self.assertTrue(result["candidate_warrants_further_qualification"])
        self.assertFalse(result["acceptance_eligible"])
        self.assertEqual(result["decision"], "retain_bitrate_pending_production_qualification")
        self.assertEqual(result["matched_identity"]["encoder_sha256"], "d" * 64)
        self.assertEqual(len(result["input_reports"]), 6)

    def test_forged_fixture_metadata_or_binary_cannot_pass(self):
        for target, key, value in (("fixture", "class", "other"),
                                   ("capture_environment", "encoder_sha256", "e" * 64),
                                   ("capture_environment", "encoder_sha256", "unknown")):
            reports = complete_reports()
            reports[-1][target][key] = value
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                SCREEN["summarize"](reports)

    def test_one_damaged_fixture_vetoes_further_qualification(self):
        reports = complete_reports()
        reports[0]["modes"]["qvbr"]["vmaf"] = 94
        result = SCREEN["summarize"](reports)
        self.assertFalse(result["candidate_warrants_further_qualification"])
        self.assertEqual(result["quality_regressions"], [reports[0]["fixture"]["identity"]])

    def test_missing_or_duplicate_fixture_cannot_pass(self):
        for reports in (complete_reports()[:-1], complete_reports() + complete_reports()[:1]):
            with self.subTest(count=len(reports)), self.assertRaises(ValueError):
                SCREEN["summarize"](reports)

    def test_mixed_encoder_source_scorer_or_candidate_cannot_pass(self):
        for section, key, value in (("encoder", "build", "other"),
                                    ("encoder_export", "quality", 23),
                                    ("scorer", "sha256", "other")):
            reports = complete_reports()
            reports[-1][section][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                SCREEN["summarize"](reports)
        reports = complete_reports()
        reports[-1]["source_revision"] = "other"
        with self.assertRaises(ValueError):
            SCREEN["summarize"](reports)

    def test_incomplete_capture_cannot_pass(self):
        reports = complete_reports()
        reports[0]["status"] = "incomplete"
        with self.assertRaises(ValueError):
            SCREEN["summarize"](reports)

    def test_nonfinite_metrics_fail_closed(self):
        for invalid in (float("nan"), float("inf"), -1):
            reports = complete_reports()
            reports[0]["modes"]["qvbr"]["vmaf"] = invalid
            result = SCREEN["summarize"](reports)
            self.assertFalse(result["candidate_warrants_further_qualification"])
            self.assertEqual(result["benefit_failures"][0]["code"], "benefit_evidence_invalid")

    def test_no_measured_benefit_retains_bitrate(self):
        reports = complete_reports()
        for report in reports:
            report["modes"]["qvbr"] = copy.deepcopy(report["modes"]["vbr"])
        result = SCREEN["summarize"](reports)
        self.assertFalse(result["benefit"]["passed"])
        self.assertFalse(result["candidate_warrants_further_qualification"])

    def test_duration_bound_is_checked_before_any_capture(self):
        from types import SimpleNamespace
        for duration in (0, 31, float("nan"), float("inf")):
            with self.assertRaisesRegex(ValueError, "duration"):
                SCREEN["capture"](SimpleNamespace(duration=duration))

    def test_export_rejects_non_sdr_or_missing_production_args(self):
        export = {"schema_version": 1, "scope": "encoder_arguments_only",
                  "output_grade": "sdr", "family": "qsv", "input_args": [],
                  "modes": {"vbr": {"encoder_args": ["-c:v", "h264_qsv"]},
                            "qvbr": {"encoder_args": ["-c:v", "h264_qsv"]}}}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "args.json"
            path.write_text(json.dumps(export))
            self.assertEqual(SCREEN["load_export"](path), export)
            export["output_grade"] = "hdr10"
            path.write_text(json.dumps(export))
            with self.assertRaises(ValueError):
                SCREEN["load_export"](path)

    def test_hdr_probe_is_refused_before_scoring(self):
        from types import SimpleNamespace
        result = SimpleNamespace(stdout=json.dumps({"streams": [{"nb_read_frames": "24",
                                 "color_transfer": "smpte2084"}]}))
        with patch.dict(SCREEN["inspect_video"].__globals__, bounded_run=lambda *a: (result, 1)):
            with self.assertRaisesRegex(ValueError, "HDR"):
                SCREEN["inspect_video"]("ffprobe", "fixture.mp4")


if __name__ == "__main__":
    unittest.main()
