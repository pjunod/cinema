"""Offline screens cannot masquerade as production qualification."""

import copy
import json
from pathlib import Path
import runpy
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SCREEN = runpy.run_path(str(ROOT / "scripts/encoder-calibration-screen"))
# `hls_keyframe_args()` and `SEGMENT_SECONDS` in
# crates/plurx-core/src/transcode/mod.rs: what the export binary emits.
PRODUCTION_KEYFRAMES = ["-force_key_frames", "expr:gte(t,n_forced*2)"]


def mode(bytes_, vmaf, idr=True):
    return {"bytes": bytes_, "vmaf": vmaf, "first_segment_seconds": 0.5,
            "forced_idr": {"passed": idr}}


def complete_reports():
    fixtures = json.loads(SCREEN["CORPUS"].read_text())["fixtures"]
    return [{
        "schema_version": 2, "status": "scored", "scope": SCREEN["SCOPE"], "fixture": fixture,
        "encoder_export": {"family": "qsv", "quality": 22},
        "candidate": {"name": "preset-slower", "base": "vbr", "extra_args": ["-preset", "slower"]},
        "encoder": {"build": "qualified-binary"}, "source_revision": "a" * 40,
        "bench_sha256": "b" * 64, "duration_seconds": 12,
        "corpus_sha256": SCREEN["BENCH"]["sha256_file"](SCREEN["CORPUS"]),
        "capture_environment": {"encoder_sha256": "d" * 64, "image_id": "sha256:image"},
        "scorer": {"sha256": "c" * 64, "model": "vmaf_v0.6.1"},
        "modes": {"vbr": mode(1000, 95), "candidate": mode(800, 95)},
        "speed_relative_to_vbr": 0.9,
        "forced_idr_control": {"forced_idr": {"passed": False}},
    } for fixture in fixtures]


def export(**candidate):
    base = ["-c:v", "h264_qsv", "-b:v", "8000k", "-maxrate", "12000k", "-bufsize", "16000k",
            "-forced_idr", "1"]
    extra = candidate.get("extra_args", ["-preset", "slower"])
    return {"schema_version": 2, "scope": "encoder_arguments_only", "output_grade": "sdr",
            "family": "qsv", "input_args": [], "quality": 22, "bitrate_kbps": 8000,
            "modes": {"vbr": {"encoder_args": base},
                      "qvbr": {"encoder_args": base[:2] + ["-global_quality", "22"] + base[2:]}},
            "keyframe_args": PRODUCTION_KEYFRAMES, "segment_seconds": 2,
            "candidate": {"base": "vbr", "extra_args": extra,
                          "encoder_args": candidate.get("encoder_args", base + extra)}}


def libx264():
    """No skip: without a real encoder the control proves nothing."""
    ffmpeg = shutil.which("ffmpeg")
    if ffmpeg is None or shutil.which("ffprobe") is None:
        raise AssertionError("ffmpeg and ffprobe are required: the IDR control runs real encodes")
    encoders = subprocess.run([ffmpeg, "-hide_banner", "-encoders"], capture_output=True, text=True).stdout
    if " libx264 " not in encoders:
        raise AssertionError("ffmpeg with libx264 is required for the software IDR control")
    return ffmpeg


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
        reports[0]["modes"]["candidate"]["vmaf"] = 94
        result = SCREEN["summarize"](reports)
        self.assertFalse(result["candidate_warrants_further_qualification"])
        self.assertEqual(result["quality_regressions"], [reports[0]["fixture"]["identity"]])

    def test_missing_or_duplicate_fixture_cannot_pass(self):
        for reports in (complete_reports()[:-1], complete_reports() + complete_reports()[:1]):
            with self.subTest(count=len(reports)), self.assertRaises(ValueError):
                SCREEN["summarize"](reports)

    def test_mixed_encoder_source_scorer_or_candidate_cannot_pass(self):
        for section, key, value in (("encoder", "build", "other"),
                                    ("candidate", "name", "look-ahead-20"),
                                    ("candidate", "extra_args", ["-preset", "veryslow"]),
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
            reports[0]["modes"]["candidate"]["vmaf"] = invalid
            result = SCREEN["summarize"](reports)
            self.assertFalse(result["candidate_warrants_further_qualification"])
            self.assertEqual(result["benefit_failures"][0]["code"], "benefit_evidence_invalid")

    def test_no_measured_benefit_retains_bitrate(self):
        reports = complete_reports()
        for report in reports:
            report["modes"]["candidate"] = copy.deepcopy(report["modes"]["vbr"])
        result = SCREEN["summarize"](reports)
        self.assertFalse(result["benefit"]["passed"])
        self.assertFalse(result["candidate_warrants_further_qualification"])

    def test_duration_bound_is_checked_before_any_capture(self):
        from types import SimpleNamespace
        for duration in (0, 31, float("nan"), float("inf")):
            with self.assertRaisesRegex(ValueError, "duration"):
                SCREEN["capture"](SimpleNamespace(duration=duration))

    def test_export_rejects_non_sdr_or_missing_production_args(self):
        value = export()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "args.json"
            path.write_text(json.dumps(value))
            self.assertEqual(SCREEN["load_export"](path), value)
            for key, bad in (("output_grade", "hdr10"), ("schema_version", 1),
                             ("keyframe_args", ["-g", "48"]), ("segment_seconds", 0)):
                with self.subTest(key=key):
                    path.write_text(json.dumps({**value, key: bad}))
                    with self.assertRaises(ValueError):
                        SCREEN["load_export"](path)

    def test_candidate_starts_from_production_argv_and_only_adds(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "args.json"
            for value in (
                    # Arguments that are not production's plus the extras.
                    export(encoder_args=["-c:v", "h264_qsv", "-preset", "slower"]),
                    # B-frames or GOP length would replace what the screen holds fixed.
                    export(extra_args=["-bf", "2"]), export(extra_args=["-g", "48"]),
                    # Restating a production option overrides production.
                    export(extra_args=["-maxrate", "20000k"])):
                path.write_text(json.dumps(value))
                with self.subTest(extra=value["candidate"]["extra_args"]), self.assertRaises(ValueError):
                    SCREEN["load_export"](path)

    def test_committed_candidate_list_is_the_handoff_set(self):
        listed = SCREEN["load_candidates"]()
        rows = {row["name"]: row for row in listed["candidates"]}
        self.assertEqual(listed["encoder"], "h264_qsv")
        presets = [r for r in rows.values() if "-preset" in r["extra_args"]]
        self.assertEqual([r["extra_args"][r["extra_args"].index("-preset") + 1] for r in presets], ["slower"])
        depths = sorted(int(r["extra_args"][r["extra_args"].index("-look_ahead_depth") + 1])
                        for r in rows.values() if "-look_ahead" in r["extra_args"])
        self.assertEqual(len(set(depths)), 2)
        qualities = sorted(r["quality"] for r in rows.values() if r["base"] == "qvbr")
        self.assertEqual(len(set(qualities)), 2)
        self.assertNotIn(22, qualities)
        for row in rows.values():
            self.assertFalse(SCREEN["option_names"](row["extra_args"]) & {"-bf", "-g", "-b_strategy"})

    def test_an_export_for_another_candidate_is_refused(self):
        row = SCREEN["candidate_row"]("look-ahead-20")
        with self.assertRaisesRegex(ValueError, "not made for this candidate"):
            SCREEN["export_matches_candidate"](export(), row)
        SCREEN["export_matches_candidate"](export(extra_args=row["extra_args"]), row)

    def test_a_control_that_passes_voids_the_idr_column(self):
        reports = complete_reports()
        reports[2]["forced_idr_control"]["forced_idr"]["passed"] = True
        result = SCREEN["summarize"](reports)
        self.assertFalse(result["forced_idr_column_valid"])
        self.assertIsNone(result["forced_idr_kept"])
        self.assertEqual(result["forced_idr_control_passed_on"], [reports[2]["fixture"]["identity"]])
        self.assertTrue(result["benefit"]["passed"])
        self.assertFalse(result["candidate_warrants_further_qualification"])

    def test_a_candidate_that_loses_forced_idr_cannot_warrant_qualification(self):
        reports = complete_reports()
        reports[4]["modes"]["candidate"]["forced_idr"]["passed"] = False
        result = SCREEN["summarize"](reports)
        self.assertTrue(result["forced_idr_column_valid"])
        self.assertFalse(result["forced_idr_kept"])
        self.assertFalse(result["candidate_warrants_further_qualification"])

    def test_summary_carries_speed_and_first_segment_per_fixture(self):
        result = SCREEN["summarize"](complete_reports())
        self.assertEqual(set(result["speed_relative_to_vbr"].values()), {0.9})
        self.assertEqual(len(result["first_segment_seconds"]), 6)
        self.assertTrue(result["forced_idr_kept"])

    def test_hdr_probe_is_refused_before_scoring(self):
        from types import SimpleNamespace
        result = SimpleNamespace(stdout=json.dumps({"streams": [{"nb_read_frames": "24",
                                 "color_transfer": "smpte2084"}]}))
        with patch.dict(SCREEN["inspect_video"].__globals__, bounded_run=lambda *a: (result, 1)):
            with self.assertRaisesRegex(ValueError, "HDR"):
                SCREEN["inspect_video"]("ffprobe", "fixture.mp4")


class SoftwareForcedIdrControl(unittest.TestCase):
    """Real libx264 encodes through the production segmenter: the IDR check
    passes with production forcing and fails on every control that lacks it."""

    def segmented(self, tmp, name, video, rate="24"):
        ffmpeg = libx264()
        before = ["-hide_banner", "-nostdin", "-loglevel", "error", "-f", "lavfi"]
        source = f"testsrc=size=320x240:rate={rate}:duration=6"
        after = ["-c:v", "libx264", "-preset", "ultrafast", "-threads", "1", *video]
        directory = Path(tmp) / name
        capture = SCREEN["segment_capture"](ffmpeg, before, source, after, directory, 2)
        check = SCREEN["check_forced_idr"](ffmpeg, shutil.which("ffprobe"), directory, 6, 2)
        return capture, check

    def test_production_forcing_lands_an_idr_on_every_two_second_boundary(self):
        for rate in ("24", "60000/1001"):
            with tempfile.TemporaryDirectory() as tmp, self.subTest(rate=rate):
                capture, check = self.segmented(tmp, "forced", ["-bf", "0", *PRODUCTION_KEYFRAMES], rate)
                self.assertTrue(check["passed"], check["failures"])
                self.assertEqual(len(capture["segments"]), 3)
                self.assertGreater(capture["first_segment_seconds"], 0)
                self.assertLessEqual(capture["first_segment_seconds"], capture["elapsed_seconds"])
                self.assertTrue(all(5 in row["first_access_unit_nal_types"] for row in check["segments"]))

    def test_control_without_forcing_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, check = self.segmented(tmp, "control", ["-bf", "0"])
            self.assertFalse(check["passed"])
            self.assertFalse(check["boundaries"][1]["within_one_frame"])

    def test_a_short_fixed_gop_hides_missing_forcing_which_is_why_the_control_exists(self):
        # `-g 48` at 24 fps puts a keyframe on every 2 s boundary with no
        # forcing at all: the old capture's GOP would have passed this control.
        with tempfile.TemporaryDirectory() as tmp:
            _, check = self.segmented(tmp, "fixed-gop", ["-bf", "0", "-g", "48"])
            self.assertTrue(check["passed"], check["failures"])

    def test_an_i_frame_that_is_not_an_idr_does_not_count(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, check = self.segmented(tmp, "open-gop", ["-bf", "2", "-x264-params", "open-gop=1:scenecut=0",
                                                        *PRODUCTION_KEYFRAMES])
            self.assertFalse(check["passed"])
            later = check["segments"][1:]
            self.assertTrue(later and not any(row["random_access_start"] for row in later))


if __name__ == "__main__":
    unittest.main()
