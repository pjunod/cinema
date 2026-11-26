"""Bounded synthetic tool results exercise the actual offline score consumer."""

import copy
from fractions import Fraction
import json
from pathlib import Path
import re
import runpy
import tempfile
import unittest
from unittest.mock import Mock, patch


ROOT = Path(__file__).resolve().parents[2]


def pair(namespace):
    frame = dict(namespace["GRADE"], width=64, height=64,
                 best_effort_timestamp_time="0.000000",
                 side_data_list=[{"side_data_type": "Content light level metadata",
                                  "max_content": 1000, "max_average": 400}])
    return dict(frame), [frame, dict(frame, best_effort_timestamp_time="0.250000")]


def ntsc_pair(namespace):
    """Authored ffprobe rows: preserve presentation ordinals at 60000/1001."""
    stream, _ = pair(namespace)
    frames = [dict(copy.deepcopy(stream), best_effort_timestamp_time=
                   f"{float(index * Fraction(1001, 60000)):.6f}")
              for index in range(61)]
    return stream, frames


class CodecHdrScoreTests(unittest.TestCase):
    def score_probe_pair(self, namespace, reference, distorted, refuses=False):
        """Use the real probe/pairing consumer; never execute a native scorer."""
        metric = Mock(return_value={"synthetic": "not media evidence"})
        census = Mock(return_value={"synthetic": "not media evidence"})
        with tempfile.TemporaryDirectory() as directory:
            paths = [Path(directory).resolve() / name
                     for name in ("reference", "distorted", "ffmpeg", "ffprobe")]
            for path in paths:
                path.write_bytes(path.name.encode())
            ref, out, ffmpeg, ffprobe = paths

            def tool(command, timeout=90):
                if "-show_frames" in command:
                    entries = command[command.index("-show_entries") + 1]
                    fields = dict(section.split("=", 1) for section in entries.split(":"))
                    interpretation = {"width", "height", "pix_fmt", "color_range",
                                      "color_space", "color_transfer", "color_primaries",
                                      "side_data_list"}
                    self.assertLessEqual(interpretation, set(fields["stream"].split(",")))
                    self.assertLessEqual(interpretation | {"best_effort_timestamp_time"},
                                         set(fields["frame"].split(",")))
                    stream, frames = reference if command[-1] == str(ref) else distorted
                    return json.dumps({"streams": [stream], "frames": frames})
                if "-version" in command:
                    return "authored probe control; not actual decoded media"
                raise AssertionError("Unexpected native tool invocation")

            with patch.dict(namespace["score"].__globals__, run=tool,
                            metric=metric, census=census):
                args = (ref, out, namespace["sha256"](ref), namespace["sha256"](out),
                        ffmpeg, ffprobe, 2.0)
                if refuses:
                    with self.assertRaises(namespace["Refusal"]):
                        namespace["score"](*args)
                    metric.assert_not_called()
                    census.assert_not_called()
                    return None
                result = namespace["score"](*args)
                self.assertEqual(metric.call_count, 2)
                self.assertEqual(census.call_count, 2)
                self.assertTrue(all(call.args[4] == len(reference[1])
                                    for call in metric.call_args_list))
                return result

    def test_ntsc_actual_grid_refuses_nominal_rate_and_same_count_duplicates_before_metrics(self):
        namespace = runpy.run_path(str(ROOT / "scripts/codec-hdr-score"))
        reference = ntsc_pair(namespace)
        result = self.score_probe_pair(namespace, reference, copy.deepcopy(reference))
        self.assertEqual(result["pairing"]["frames"], 61)
        self.assertEqual(result["pairing"]["timestamps"][30], 0.5005)
        self.assertEqual(result["pairing"]["timestamps"][60], 1.001)

        nominal = copy.deepcopy(reference)
        for index, frame in enumerate(nominal[1]):
            frame["best_effort_timestamp_time"] = f"{index / 60:.6f}"
        early = copy.deepcopy(reference)
        early[1][30]["best_effort_timestamp_time"] = "0.500000"
        duplicate = copy.deepcopy(reference)
        duplicate[1][30] = copy.deepcopy(duplicate[1][29])
        dropped = copy.deepcopy(reference)
        dropped[1].pop(30)
        for label, distorted in [("nominal-rate substitution", nominal),
                                 ("one late pairing error", early),
                                 ("same-count drop/duplicate", duplicate),
                                 ("dropped frame", dropped)]:
            with self.subTest(case=label):
                self.score_probe_pair(namespace, reference, distorted, refuses=True)
        # Identical bad grids must not validate each other.
        self.score_probe_pair(namespace, duplicate, copy.deepcopy(duplicate), refuses=True)

    def test_late_decoded_grade_and_container_conflicts_refuse_before_metrics(self):
        namespace = runpy.run_path(str(ROOT / "scripts/codec-hdr-score"))
        reference = ntsc_pair(namespace)
        changes = [("color_transfer", "arib-std-b67"),
                   ("color_transfer", "bt709"), ("color_primaries", "bt709"),
                   ("color_space", "bt709"), ("color_range", "pc"),
                   ("pix_fmt", "yuv420p"), ("pix_fmt", "yuv444p10le")]
        for target in ("stream", 30, 60):
            for field, value in changes:
                distorted = copy.deepcopy(reference)
                row = distorted[0] if target == "stream" else distorted[1][target]
                row[field] = value
                with self.subTest(target=target, field=field, value=value):
                    self.score_probe_pair(namespace, reference, distorted, refuses=True)
        for field in namespace["GRADE"]:
            distorted = copy.deepcopy(reference)
            del distorted[1][30][field]
            with self.subTest(missing_decoded_field=field):
                self.score_probe_pair(namespace, reference, distorted, refuses=True)
        distorted = copy.deepcopy(reference)
        distorted[1][30]["side_data_list"].append({"side_data_type": "DOVI RPU Data"})
        self.score_probe_pair(namespace, reference, distorted, refuses=True)

        # Missing container fields are allowed only because every decoded frame
        # still supplies the interpretation. Do not manufacture container tags.
        omitted = copy.deepcopy(reference)
        for field in namespace["GRADE"]:
            del omitted[0][field]
        result = self.score_probe_pair(namespace, reference, omitted)
        self.assertEqual(result["pairing"]["frames"], 61)

    def test_sdr_vmaf_requires_pinned_parent_grade_and_decoded_bt709(self):
        namespace = runpy.run_path(str(ROOT / "scripts/codec-hdr-score"))
        pq_stream, pq_frames = pair(namespace)
        sdr_stream, sdr_frames = copy.deepcopy((pq_stream, pq_frames))
        for record in [sdr_stream, *sdr_frames]:
            record.update(namespace["SDR_GRADE"], side_data_list=[])
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory).resolve()
            source, reference, distorted, ffmpeg, ffprobe, receipt = [
                directory / name for name in ("source", "reference", "distorted", "ffmpeg", "ffprobe", "receipt.json")]
            for path in (source, distorted, ffmpeg, ffprobe):
                path.write_text(path.name)

            def tool(command, timeout=90):
                if "-show_frames" in command:
                    stream, frames = ((pq_stream, pq_frames) if command[-1] == str(source)
                                      else (sdr_stream, sdr_frames))
                    return json.dumps({"streams": [stream], "frames": frames})
                if "-version" in command:
                    return "synthetic grader/scorer; not actual corpus evidence"
                if "-vf" in command:
                    Path(command[-1]).write_text("synthetic separately graded BT709 fixture")
                else:
                    graph = command[command.index("-filter_complex") + 1]
                    stats = Path(re.search(r"log_path=([^:]+)", graph).group(1))
                    stats.write_text(json.dumps({"frames": [
                        {"frameNum": 0, "metrics": {"vmaf": 90}},
                        {"frameNum": 1, "metrics": {"vmaf": 91}}]}))
                return ""

            with patch.dict(namespace["score_sdr"].__globals__, run=tool):
                grade = namespace["grade_sdr"](source, namespace["sha256"](source),
                                               ffmpeg, ffprobe, reference, 0.5)
                receipt.write_text(json.dumps(grade))
                args = (source, namespace["sha256"](source), reference, distorted,
                        namespace["sha256"](distorted), receipt, namespace["sha256"](receipt), ffmpeg, ffprobe)
                result = namespace["score_sdr"](*args)
                self.assertEqual(result["per_frame"], [90, 91])
                self.assertEqual(result["domain"], "SDR BT709 VMAF, not HDR perceptual fidelity")
                for key, value in (("parent_source_sha256", "0" * 64),
                                   ("graph", "format=yuv420p"),
                                   ("ffmpeg_sha256", "unknown")):
                    wrong = dict(grade, **{key: value})
                    receipt.write_text(json.dumps(wrong))
                    with self.subTest(key=key), self.assertRaises(namespace["Refusal"]):
                        namespace["score_sdr"](*args[:6], namespace["sha256"](receipt), *args[7:])
                receipt.write_text(json.dumps(grade))
                sdr_frames[0]["color_transfer"] = "smpte2084"
                with self.assertRaises(namespace["Refusal"]):
                    namespace["score_sdr"](*args)

    def test_pq_pairing_refuses_grade_grid_metadata_and_residual_dv(self):
        namespace = runpy.run_path(str(ROOT / "scripts/codec-hdr-score"))
        reference = pair(namespace)
        self.assertEqual(namespace["validate_pair"](reference, copy.deepcopy(reference))["frames"], 2)
        for field, value in (("color_transfer", "arib-std-b67"),
                             ("color_transfer", "bt709"), ("pix_fmt", "yuv420p"),
                             ("color_range", "pc"), ("width", 32),
                             ("best_effort_timestamp_time", "0.125000"),
                             ("side_data_list", []),
                             ("side_data_list", [{"side_data_type": "DOVI RPU Data"}])):
            distorted = copy.deepcopy(reference)
            distorted[1][0][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(namespace["Refusal"]):
                namespace["validate_pair"](reference, distorted)
        distorted = copy.deepcopy(reference)
        distorted[1].pop()
        with self.assertRaises(namespace["Refusal"]):
            namespace["validate_pair"](reference, distorted)

    def test_score_attributes_all_frames_and_refuses_missing_metric_output(self):
        namespace = runpy.run_path(str(ROOT / "scripts/codec-hdr-score"))
        stream, frames = pair(namespace)
        payload = json.dumps({"streams": [stream], "frames": frames})
        incomplete = False

        def tool(command, timeout=90):
            if "-show_frames" in command:
                return payload
            if "-version" in command:
                return "synthetic test tool; not actual media evidence"
            graph = command[command.index("-filter_complex") + 1] if "-filter_complex" in command else command[command.index("-vf") + 1]
            match = re.search(r"(?:stats_file=|file=)([^:;,]+)", graph)
            output = Path(match.group(1))
            if "psnr=" in graph:
                content = "n:1 psnr_avg:40.0\nn:2 psnr_avg:41.0\n"
                if incomplete:
                    content = "n:1 psnr_avg:40.0\n"
            elif "ssim=" in graph:
                content = "n:1 All:0.99\nn:2 All:0.98\n"
            else:
                content = ("lavfi.signalstats.YMIN=64\nlavfi.signalstats.YMAX=940\n"
                           "lavfi.signalstats.YAVG=511.5\n") * 2
            output.write_text(content)
            return ""

        with tempfile.TemporaryDirectory() as directory:
            paths = [Path(directory).resolve() / name for name in ("ref", "out", "ffmpeg", "ffprobe")]
            for path in paths:
                path.write_bytes(path.name.encode())
            ref, out, ffmpeg, ffprobe = paths
            args = (ref, out, namespace["sha256"](ref), namespace["sha256"](out), ffmpeg, ffprobe, 0.5)
            with patch.dict(namespace["score"].__globals__, run=tool):
                result = namespace["score"](*args)
                self.assertEqual(result["metrics"]["psnr"]["per_frame"], [40.0, 41.0])
                self.assertEqual(result["census"]["reference"]["highlight"]["fraction"], [0.5, 0.5])
                self.assertEqual(result["domain"], "PQ encoded code values, not nits")
                self.assertIsNone(result["hdr_perceptual_model"])
                incomplete = True
                with self.assertRaisesRegex(namespace["Refusal"], "every validated"):
                    namespace["score"](*args)
                with self.assertRaises(namespace["Refusal"]):
                    namespace["score"](ref, out, "0" * 64, args[3], ffmpeg, ffprobe, 0.5)


if __name__ == "__main__":
    unittest.main()
