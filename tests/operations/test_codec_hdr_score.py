"""Bounded synthetic tool results exercise the actual offline score consumer."""

import copy
import json
from pathlib import Path
import re
import runpy
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]


def pair(namespace):
    frame = dict(namespace["GRADE"], width=64, height=64,
                 best_effort_timestamp_time="0.000000",
                 side_data_list=[{"side_data_type": "Content light level metadata",
                                  "max_content": 1000, "max_average": 400}])
    return dict(frame), [frame, dict(frame, best_effort_timestamp_time="0.250000")]


class CodecHdrScoreTests(unittest.TestCase):
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
