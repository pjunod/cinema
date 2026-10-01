"""Review43 real scorer consumer refusal; bounded synthetic tools, no media run."""
import copy
import json
from pathlib import Path
import re
import runpy
import tempfile
import unittest
from unittest.mock import patch

if __package__:
    from .test_codec_hdr_score import ROOT, pair
else:
    from test_codec_hdr_score import ROOT, pair


class CodecHdrParentGeometryTests(unittest.TestCase):
    def test_sdr_grade_refuses_jointly_resized_pair_before_model_execution(self):
        namespace = runpy.run_path(str(ROOT / "scripts/codec-hdr-score"))
        parent = pair(namespace)
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory).resolve()
            source, reference, distorted, ffmpeg, ffprobe, receipt = [
                directory / name for name in ("source", "reference", "distorted", "ffmpeg", "ffprobe", "receipt.json")]
            for path in (source, reference, distorted, ffmpeg, ffprobe):
                path.write_text(path.name)
            proof = {"schema": 1, "kind": "independent-PQ-to-BT709-Hable-reference",
                     "parent_source_sha256": namespace["sha256"](source),
                     "reference_sha256": namespace["sha256"](reference),
                     "duration_seconds": 0.5, "reference_white_nits": 100, "source_peak_nits": 1000,
                     "ffmpeg_sha256": namespace["sha256"](ffmpeg),
                     "graph": namespace["sdr_graph"](0.5), "argv": [namespace["sdr_graph"](0.5)],
                     "source_pairing": namespace["validate_pair"](parent, copy.deepcopy(parent))}
            receipt.write_text(json.dumps(proof))
            for width, height in ((32, 64), (64, 32)):
                sdr = copy.deepcopy(parent)
                for record in [sdr[0], *sdr[1]]:
                    record.update(namespace["SDR_GRADE"], width=width, height=height, side_data_list=[])
                model_calls = []
                def tool(command, timeout=90):
                    if "-show_frames" in command:
                        stream, frames = parent if command[-1] == str(source) else sdr
                        return json.dumps({"streams": [stream], "frames": frames})
                    if "-version" in command:
                        return "synthetic tool; not media qualification"
                    model_calls.append(command)
                    graph = command[command.index("-filter_complex") + 1]
                    stats = Path(re.search(r"log_path=([^:]+)", graph).group(1))
                    stats.write_text(json.dumps({"frames": [
                        {"frameNum": 0, "metrics": {"vmaf": 99}},
                        {"frameNum": 1, "metrics": {"vmaf": 99}}]}))
                    return ""
                with self.subTest(width=width, height=height), \
                     patch.dict(namespace["score_sdr"].__globals__, run=tool):
                    with self.assertRaisesRegex(namespace["Refusal"], "source dimensions"):
                        namespace["score_sdr"](
                            source, namespace["sha256"](source), reference, distorted,
                            namespace["sha256"](distorted), receipt, namespace["sha256"](receipt), ffmpeg, ffprobe)
                    self.assertEqual(model_calls, [], "refusal must precede libvmaf")
