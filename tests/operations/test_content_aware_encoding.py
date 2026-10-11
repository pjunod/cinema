"""Regressions for bounded offline content-aware recommendations."""

import contextlib
import copy
import io
import json
import math
import os
from pathlib import Path
import runpy
import signal
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
TOOL = runpy.run_path(str(ROOT / "scripts/content-aware-encoding"))
G = TOOL["main"].__globals__
Inconclusive = TOOL["Inconclusive"]


def measurement(*, mean=95, p10=93, size=1000, seconds=1):
    return {"vmaf": {"mean": mean, "p10": p10, "frames": 10},
            "bytes": size, "encode_seconds": seconds}


def window():
    return {"frames": 10, "modes": {"vbr": measurement(),
                                     "q23": measurement(size=800)}}


def exporter(quality=23):
    common = ["-c:v", "libx264", "-preset", "veryfast", "-maxrate", "6000k",
              "-bufsize", "8000k", "-profile:v", "high", "-threads", "1"]
    return {"schema_version": 1, "scope": "encoder_arguments_only", "family": "software",
            "bitrate_kbps": 4000, "quality": quality, "software_threads": 1,
            "output_grade": "sdr", "modes": {
                "vbr": {"recipe_value": "vbr:maxrate1.5x:bufsize2x",
                        "encoder_args": common + ["-b:v", "4000k"]},
                "qvbr": {"recipe_value": f"qvbr:q{quality}:maxrate1.5x:bufsize2x",
                         "encoder_args": common + ["-crf", str(quality)]}}}


def source():
    return {"width": 1920, "height": 1080, "avg_frame_rate": "30000/1001",
            "sample_aspect_ratio": "1:1", "color_transfer": "bt709",
            "color_primaries": "bt709", "color_space": "bt709", "color_range": "tv"}


class ContentAwareEncodingTests(unittest.TestCase):
    def test_hard_window_vetoes_better_aggregate_quality(self):
        rows = [window(), window()]
        rows[0]["modes"]["q23"]["vmaf"] = {"mean": 99, "p10": 98, "frames": 10}
        rows[1]["modes"]["q23"]["vmaf"] = {"mean": 94, "p10": 93, "frames": 10}
        result = TOOL["recommend"](rows, ["q23"], 90, 1.10)
        self.assertEqual(result["decision"], "retain_baseline")
        self.assertIn("window_1:mean_regression", result["rejected"]["q23"])

    def test_lower_tail_floor_speed_and_savings_have_independent_vetoes(self):
        cases = [("p10", 92, "window_0:p10_regression"),
                 ("encode_seconds", 1.11, "aggregate_encode_time_regression"),
                 ("bytes", 950, "aggregate_savings_below_10_percent")]
        for field, value, reason in cases:
            with self.subTest(field=field):
                row = window()
                current = row["modes"]["q23"]
                (current["vmaf"] if field == "p10" else current)[field] = value
                result = TOOL["recommend"]([row], ["q23"], 90, 1.10)
                self.assertIn(reason, result["rejected"]["q23"])
        result = TOOL["recommend"]([window()], ["q23"], 96, 1.10)
        self.assertIn("window_0:below_mean_floor", result["rejected"]["q23"])

    def test_encode_time_gate_uses_total_elapsed_and_window_timings_are_diagnostic(self):
        rows = [window(), window()]
        rows[0]["modes"]["q23"]["encode_seconds"] = 1.5
        rows[1]["modes"]["q23"]["encode_seconds"] = 0.5
        result = TOOL["recommend"](rows, ["q23"], 90, 1.10)
        self.assertEqual(result["decision"], "recommend_candidate")
        rows[1]["modes"]["q23"]["encode_seconds"] = 0.8
        result = TOOL["recommend"](rows, ["q23"], 90, 1.10)
        self.assertIn("aggregate_encode_time_regression", result["rejected"]["q23"])

    def test_smallest_eligible_candidate_wins_without_mutating_measurements(self):
        row = window()
        row["modes"]["q26"] = measurement(size=700)
        before = copy.deepcopy(row)
        result = TOOL["recommend"]([row], ["q23", "q26"], 90, 1.10)
        self.assertEqual(result["mode"], "q26")
        self.assertAlmostEqual(result["sample_byte_savings"], 0.30)
        self.assertEqual(row, before)

    def test_invalid_metrics_and_frame_coverage_fail_closed(self):
        for invalid in (float("nan"), float("inf"), None, True, "95"):
            with self.subTest(value=invalid):
                row = window()
                row["modes"]["q23"]["vmaf"]["mean"] = invalid
                with self.assertRaises(Inconclusive):
                    TOOL["recommend"]([row], ["q23"], 90, 1.10)
        row = window()
        row["modes"]["q23"]["vmaf"]["frames"] = 9
        with self.assertRaisesRegex(Inconclusive, "frame coverage"):
            TOOL["recommend"]([row], ["q23"], 90, 1.10)

    def test_vmaf_nearest_rank_p10_and_full_frame_correspondence(self):
        data = {"frames": [{"frameNum": index, "metrics": {"vmaf": value}}
                            for index, value in enumerate(range(80, 100))]}
        metrics = TOOL["vmaf_metrics"](data, 20)
        self.assertEqual(metrics, {"mean": 89.5, "p10": 81, "frames": 20})
        with self.assertRaises(Inconclusive):
            TOOL["vmaf_metrics"](data, 21)
        data["frames"][1]["frameNum"] = 0
        with self.assertRaises(Inconclusive):
            TOOL["vmaf_metrics"](data, 20)

    def test_extra_encoded_frames_and_mismatched_output_color_are_refused(self):
        stream = dict(source(), width=1280, height=720, nb_read_frames="24", pix_fmt="yuv420p")
        spec = {"width": 1280, "height": 720}
        TOOL["verify_sample"](stream, spec, 24)
        for changed in ({"nb_read_frames": "25"}, {"height": 718},
                        {"color_transfer": "unknown"}, {"color_range": "pc"},
                        {"pix_fmt": "yuv420p10le"}):
            with self.subTest(changed=changed), self.assertRaises(Inconclusive):
                TOOL["verify_sample"](dict(stream, **changed), spec, 24)

    def test_windows_are_deterministic_bounded_and_nonoverlapping(self):
        for duration in (0.3, 1, 12, 123.456, 7200):
            for count in (1, 2, 3):
                with self.subTest(duration=duration, count=count):
                    rows = TOOL["windows"](duration, count, 10, 30000 / 1001)
                    self.assertEqual(rows, TOOL["windows"](duration, count, 10, 30000 / 1001))
                    end = 0
                    for row in rows:
                        self.assertGreaterEqual(row["start_seconds"], end - 1e-8)
                        self.assertLessEqual(row["duration_seconds"], 10)
                        self.assertLessEqual(row["frames"], 600)
                        end = row["start_seconds"] + row["duration_seconds"]
                    self.assertLessEqual(end, duration + 1e-8)
        for args in ((1, 4, 1, 30), (1, 1, 11, 30), (1, 1, 1, 61), (0.01, 3, 1, 30)):
            with self.assertRaises(Inconclusive):
                TOOL["windows"](*args)

    def test_geometry_never_upscales_and_ambiguous_color_hdr_and_excess_work_refused(self):
        spec = TOOL["source_spec"](source(), {"duration": "12"}, 720)
        self.assertEqual((spec["width"], spec["height"]), (1280, 720))
        small = dict(source(), width=320, height=180)
        spec = TOOL["source_spec"](small, {"duration": "12"}, 1080)
        self.assertEqual((spec["width"], spec["height"]), (320, 180))
        for changed in ({"color_transfer": "smpte2084"}, {"color_space": "unknown"},
                        {"color_range": None}, {"avg_frame_rate": "61/1"},
                        {"width": 4098}, {"height": 2162},
                        {"side_data_list": [{"side_data_type": "DOVI configuration record"}]}):
            with self.subTest(changed=changed), self.assertRaises(Inconclusive):
                TOOL["source_spec"](dict(source(), **changed), {"duration": "12"}, 720)

    def test_exported_production_arguments_are_used_and_cannot_escape_bounds(self):
        runner = mock.Mock()
        runner.run.side_effect = [(json.dumps(exporter(23)), 0), (json.dumps(exporter(26)), 0)]
        recipes, exports = TOOL["export_recipes"](runner, "exporter", 4000, [23, 26], 1)
        self.assertEqual(recipes["q23"]["encoder_args"], exports[0]["modes"]["qvbr"]["encoder_args"])
        for mutation in ("encoder", "threads", "input", "bitrate", "baseline"):
            row = exporter()
            if mutation == "encoder":
                row["family"] = "qsv"
            else:
                args = row["modes"]["vbr"]["encoder_args"]
                if mutation == "input":
                    args.extend(["-i", "other-media"])
                elif mutation == "threads":
                    args[args.index("-threads") + 1] = "30"
                elif mutation == "bitrate":
                    args[args.index("-b:v") + 1] = "8000k"
                else:
                    row["modes"]["vbr"]["recipe_value"] = "invented"
            runner.run.side_effect = None
            runner.run.return_value = (json.dumps(row), 0)
            with self.subTest(mutation=mutation), self.assertRaises(Inconclusive):
                TOOL["export_recipes"](runner, "exporter", 4000, [23], 1)

    def test_source_change_and_recipe_change_invalidate_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "source"
            path.write_bytes(b"original")
            runner = TOOL["Runner"](directory, 5, 1024)
            sha, identity = TOOL["file_hash"](path, runner)
            # Size differs deterministically, even on coarse-timestamp filesystems.
            path.write_bytes(b"replaced-content")
            with self.assertRaisesRegex(Inconclusive, "source changed"):
                TOOL["unchanged"](path, identity)
            changed, _ = TOOL["file_hash"](path, runner)
            self.assertNotEqual(sha, changed)
            before = {"source": sha, "recipe": exporter(23)}
            self.assertNotEqual(TOOL["digest"](before),
                                TOOL["digest"]({"source": changed, "recipe": exporter(23)}))
            self.assertNotEqual(TOOL["digest"](before),
                                TOOL["digest"]({"source": sha, "recipe": exporter(26)}))
            with self.assertRaisesRegex(Inconclusive, "byte ceiling"):
                TOOL["file_hash"](path, runner, maximum=1)

    def test_same_metadata_content_change_emits_no_recommendation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            media, report_path = root / "source", root / "report.json"
            media.write_bytes(b"original")
            original_stat = TOOL["stat_identity"]
            identity = original_stat(media)

            def frozen_source_stat(path):
                return identity if Path(path) == media else original_stat(path)

            def fake_probe(_runner, _ffprobe, path, **_kwargs):
                if Path(path) == media:
                    return dict(source(), avg_frame_rate="24/1"), {"duration": "1"}
                return dict(source(), width=1280, height=720, nb_read_frames="24",
                            pix_fmt="yuv420p"), {}

            def fake_run(runner, argv):
                runner.commands.append([str(value) for value in argv])
                if "-lavfi" in argv:
                    (runner.root / "vmaf.json").write_text(json.dumps({"frames": [
                        {"frameNum": index, "metrics": {"vmaf": 96}} for index in range(24)
                    ]}))
                    # All stat fields stay identical, just as with coarse
                    # timestamps; only the final digest can detect this write.
                    media.write_bytes(b"replaced")
                elif str(argv[-1]).endswith(".mkv"):
                    Path(argv[-1]).write_bytes(b"x" * (500 if "-crf" in argv else 1000))
                return "test tool version", 0.1

            modes = exporter()["modes"]
            patches = {"stat_identity": frozen_source_stat, "probe": fake_probe,
                       "executable_identity": lambda path, _runner: (path, "test-tool-sha"),
                       "export_recipes": lambda *_: ({"vbr": modes["vbr"], "q23": modes["qvbr"]}, [])}
            arguments = ["--input", str(media), "--json", str(report_path), "--encoder-args", "exporter",
                         "--height", "720", "--bitrate-kbps", "4000", "--qualities", "23",
                         "--min-vmaf", "90", "--windows", "1", "--window-seconds", "1",
                         "--scratch-parent", directory]
            with mock.patch.dict(G, patches), mock.patch.object(TOOL["Runner"], "run", fake_run), \
                    mock.patch.dict(G, {"recommend": mock.Mock(wraps=TOOL["recommend"])}), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(TOOL["main"](arguments), 2)
                G["recommend"].assert_not_called()
            report = json.loads(report_path.read_text())
            self.assertEqual(report["outcome"], "inconclusive")
            self.assertIn("source content changed", report["error"])
            self.assertIsNone(report["recommendation"])
            self.assertEqual(set(root.iterdir()), {media, report_path})

    def test_deadline_and_disk_watchdog_kill_process_group(self):
        for budget, scratch_bytes, code, message in (
            (0.1, 1024**2, "import time; time.sleep(30)", "budget exhausted"),
            (5, 1024, "import pathlib,time; pathlib.Path('large').write_bytes(b'x'*2048); time.sleep(30)",
             "scratch byte ceiling"),
        ):
            with self.subTest(message=message), tempfile.TemporaryDirectory() as directory:
                runner = TOOL["Runner"](directory, budget, scratch_bytes)
                with mock.patch.object(G["os"], "killpg", wraps=os.killpg) as kill:
                    with self.assertRaisesRegex(Inconclusive, message):
                        runner.run([sys.executable, "-c", code])
                    self.assertEqual(kill.call_count, 1)
                    self.assertEqual(kill.call_args.args[1], signal.SIGKILL)

    def test_cancellation_and_failure_clean_scratch_and_emit_inconclusive_report(self):
        for failure in (KeyboardInterrupt("cancelled"), Inconclusive("source changed")):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                report = root / "report.json"

                def interrupted(_args, runner):
                    (runner.root / "temporary-media").write_bytes(b"sample")
                    raise failure

                arguments = ["--input", str(root / "source"), "--json", str(report),
                             "--encoder-args", "exporter", "--height", "720",
                             "--bitrate-kbps", "4000", "--qualities", "23",
                             "--min-vmaf", "90", "--scratch-parent", directory]
                with mock.patch.dict(G, {"analyze": interrupted}), contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(TOOL["main"](arguments), 2)
                self.assertEqual(list(root.iterdir()), [report])
                result = json.loads(report.read_text())
                self.assertEqual(result["outcome"], "inconclusive")
                self.assertIsNone(result["recommendation"])
                self.assertFalse(result["production_qualified"])


if __name__ == "__main__":
    unittest.main()
