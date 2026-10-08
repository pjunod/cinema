"""Offline experiment contracts; fake children never require Mac hardware."""

from pathlib import Path
import runpy
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
BENCH = runpy.run_path(str(ROOT / "scripts/bench-macos-video"))
G = BENCH["main"].__globals__


def graph(name="cpu"):
    return {"id": name, "classes": ["sdr"], "decoder_argv": [],
            "filter_chain": "scale=160:90", "encoder_argv": ["-c:v", "h264_videotoolbox"],
            "output_args": ["-f", "matroska"], "output_extension": "mkv",
            "output_expected": {"width": 160, "height": 90}}


def run_row(name, pair, throughput, cpu, result="successful", cache="warm_process"):
    return {"pair_id": pair, "input": {"id": "sdr8"}, "treatment": {"id": name},
            "result": result, "measurement": {"cache_condition": cache,
            "throughput_x": throughput, "cpu_seconds_per_media_second": cpu, "sustained_sample": True}}


class MacosVideoBenchTests(unittest.TestCase):
    def test_exact_capability_names_and_successful_unknown_help_are_absent(self):
        names = BENCH["table_names"]("Filters:\n .. scale_vt V->V Scale video\n"
                                    " ... tonemapx V->V CPU Dolby mapper\n"
                                    " prose mentions tonemap_videotoolbox\n", "filters")
        self.assertEqual(names, {"scale_vt", "tonemapx"})
        command = {"result": "successful", "stdout": "Unknown filter 'tonemap_videotoolbox'.\n",
                   "stderr": "", "returncode": 0}
        row = BENCH["filter_help"]("tonemap_videotoolbox", command, False)
        self.assertEqual(row["result"], "unavailable")
        self.assertEqual(row["options"], [])
        command["stdout"] = "Filter tonemapx\n  apply_dovi <boolean> ..FV....... use Dolby\n"
        row = BENCH["filter_help"]("tonemapx", command, True)
        self.assertEqual(row["result"], "available")
        self.assertEqual(row["options"], ["apply_dovi"])

    def test_child_argv_is_literal_and_timeout_reaps(self):
        with tempfile.TemporaryDirectory() as temporary:
            literal = "$(printf secret); `printf secret` space"
            row = BENCH["run_child"]([sys.executable, "-c", "import sys; print(sys.argv[1])", literal], temporary, 5)
            self.assertEqual(row["result"], "successful")
            self.assertEqual(row["stdout"].strip(), literal)
            row = BENCH["run_child"]([sys.executable, "-c", "import time; time.sleep(30)"], temporary, 0.1)
            self.assertEqual(row["result"], "failed")
            self.assertEqual(row["reason"], "timeout")
            self.assertIsNotNone(row["returncode"])

    def test_fast_excess_logs_fail_and_retained_bytes_are_bounded(self):
        with tempfile.TemporaryDirectory() as temporary:
            row = BENCH["run_child"]([sys.executable, "-c", "print('x' * 100000)"], temporary, 5, 1024)
            self.assertEqual(row["reason"], "log_limit")
            self.assertLessEqual(Path(row["raw_stdout"]).stat().st_size, 1024)

    def test_cancel_reaps_child_and_prevents_followup_spawns(self):
        child = mock.Mock(pid=987654, returncode=-15)
        child.poll.side_effect = [None, -15]
        with tempfile.TemporaryDirectory() as temporary, mock.patch.dict(G, {
            "subprocess": mock.Mock(Popen=mock.Mock(return_value=child), DEVNULL=subprocess_devnull(),
                                    TimeoutExpired=TimeoutError),
            "terminate_group": mock.Mock(),
            "time": mock.Mock(monotonic=mock.Mock(side_effect=[0, 0, 1]),
                              sleep=mock.Mock(side_effect=KeyboardInterrupt))}):
            with self.assertRaises(BENCH["Cancelled"]) as caught:
                BENCH["run_child"](["/fake/ffmpeg"], temporary, 5)
            self.assertEqual(caught.exception.command["reason"], "cancelled")
            G["terminate_group"].assert_called_once_with(child)

    def test_terminate_group_kills_remaining_descendants_after_parent_exits(self):
        child = mock.Mock(pid=987654)
        with mock.patch.dict(G, {"os": mock.Mock(killpg=mock.Mock())}):
            BENCH["terminate_group"](child)
            calls = G["os"].killpg.call_args_list
            self.assertEqual(calls[0].args, (987654, G["signal"].SIGTERM))
            self.assertEqual(calls[1].args, (987654, G["signal"].SIGKILL))
            self.assertEqual(child.wait.call_count, 2)

    def test_pair_schedule_alternates_and_never_calls_first_use_cold_file(self):
        pairs = list(BENCH["schedule_pairs"]("a", "b", 5))
        self.assertEqual(len(pairs), 6)
        self.assertEqual(pairs[0], (0, "first_use_process", ["a", "b"]))
        self.assertEqual(pairs[2], (2, "warm_process", ["b", "a"]))
        self.assertEqual(pairs[3], (3, "warm_process", ["a", "b"]))

    def test_summary_retains_failures_excludes_first_use_and_never_approves(self):
        rows = [run_row("a", "warm:1", 1, 2), run_row("b", "warm:1", 1.5, 1),
                run_row("a", "warm:2", 1, 2), run_row("b", "warm:2", None, None, "failed"),
                run_row("a", "first", 100, 0.01, cache="first_use_process"),
                run_row("b", "first", 1, 10, cache="first_use_process")]
        summary = BENCH["summarize"](rows, "a", "b")
        self.assertEqual(summary["complete_warm_pairs"], 1)
        self.assertEqual(summary["failed_or_missing_pairs"], ["warm:2"])
        self.assertEqual(summary["median_throughput_change_percent"], 50)
        self.assertEqual(summary["median_cpu_reduction_percent"], 50)
        self.assertEqual(summary["visual_approval"], "unmeasured")
        self.assertEqual(summary["promotion"], "unmeasured")

    def test_output_contract_rejects_wrong_tags_empty_frames_and_stale_side_data(self):
        observation = {"result": "successful", "streams": [{"width": 160, "height": 90,
                       "color_transfer": "smpte2084"}], "frame_count": 12,
                       "timestamps_monotonic": True, "side_data_types": ["DOVI metadata"]}
        result = BENCH["check_output"](observation, {"width": 160, "height": 90,
                  "color_transfer": "bt709", "frame_count": 12, "forbidden_side_data": ["DOVI metadata"]})
        self.assertEqual(result["result"], "failed")
        self.assertEqual(len(result["reasons"]), 2)
        observation["frame_count"] = 0
        self.assertEqual(BENCH["check_output"](observation, {})["result"], "failed")

    def test_fixture_schema_resolves_relative_path_and_p5_missing_is_honest(self):
        fixture = BENCH["normalize_fixture"]({"id": "hdr10", "path": "hdr10.hevc",
                  "sha256": "abc", "class": "hdr10", "duration_seconds": 1}, "/tmp/corpus/manifest.json")
        self.assertEqual(fixture["path"], "/tmp/corpus/hdr10.hevc")
        matrix = BENCH["p5_matrix"]([graph()], [fixture], [])
        self.assertEqual(len(matrix), 4)
        self.assertTrue(all(row["result"] == "unavailable" for row in matrix))
        self.assertTrue(all(row["metadata_proof"] == "unmeasured" for row in matrix))

    def test_timestamp_reference_rejects_seek_offset_and_missing_timestamps(self):
        reference = {"start_seconds": 0, "step_seconds": 1 / 12, "absolute_tolerance_seconds": 0.00002}
        observation = {"decoded_timestamps_seconds": [0, 0.083333, 0.166667]}
        self.assertEqual(BENCH["check_timestamps"](observation, reference)["result"], "successful")
        observation["decoded_timestamps_seconds"] = [1, 1.083333, 1.166667]
        self.assertEqual(BENCH["check_timestamps"](observation, reference)["result"], "failed")
        self.assertEqual(BENCH["check_timestamps"]({}, reference)["result"], "failed")

    def test_graph_requires_explicit_argv_and_cannot_override_run_ownership(self):
        valid = graph()
        self.assertEqual(BENCH["validate_graphs"]({"schema_version": 1, "graphs": [valid]}), [valid])
        valid["decoder_argv"] = "-hwaccel videotoolbox"
        with self.assertRaises(ValueError):
            BENCH["validate_graphs"]({"schema_version": 1, "graphs": [valid]})
        valid["decoder_argv"] = ["-i", "http://example.invalid/input"]
        with self.assertRaises(ValueError):
            BENCH["validate_graphs"]({"schema_version": 1, "graphs": [valid]})

    def test_cli_rejects_path_lookup_and_negative_bounds(self):
        with self.assertRaises(ValueError):
            BENCH["executable_path"]("ffmpeg")
        args = ["run", "--ffmpeg", "/explicit/ffmpeg", "--ffprobe", "/explicit/ffprobe",
                "--corpus", "corpus.json", "--graphs", "graphs.json", "--baseline", "a",
                "--candidate", "b", "--output-dir", "out", "--timeout", "-1"]
        with mock.patch("sys.stderr"):
            self.assertEqual(BENCH["main"](args), 2)


def subprocess_devnull():
    import subprocess
    return subprocess.DEVNULL


if __name__ == "__main__":
    unittest.main()
