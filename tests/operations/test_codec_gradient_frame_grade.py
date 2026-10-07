"""Bind the declared synthetic grade to GEQ frames before encoding."""

import json
from pathlib import Path
import runpy
from types import SimpleNamespace
import unittest
from unittest import mock


BENCH = runpy.run_path(str(Path(__file__).resolve().parents[2] / "scripts/bench"))
G = BENCH["probe_fixture"].__globals__


class GradientFrameGradeTests(unittest.TestCase):
    def test_gradient_final_frame_grade_is_metadata_only_and_refusal_stays_strict(self):
        spec = BENCH["FIXTURES"]["dark-gradient-10bit"]
        argv = BENCH["fixture_command"]("/owned/gradient.mkv", spec)
        graph = argv[argv.index("-i") + 1]
        pixel_graph = ("nullsrc=size=1920x1080:rate=24:duration=45,format=yuv420p10le,"
                       "geq=lum='64+64*X/W*(0.75+0.25*sin(T))':cb=512:cr=512")
        frame_grade = (",setparams=range=limited:color_primaries=bt709:"
                       "color_trc=bt709:colorspace=bt709")
        self.assertEqual(graph, pixel_graph + frame_grade)
        self.assertEqual(BENCH["DURATION"], 45)
        self.assertEqual(spec["size"], "1920x1080")
        video = {"codec_type": "video", "pix_fmt": "yuv420p10le", "color_range": "tv",
                 "color_space": "bt709", "color_transfer": "bt709", "color_primaries": "bt709"}
        def probe(command):
            self.assertEqual(command[0], "ffprobe")
            return SimpleNamespace(returncode=0, stderr="", stdout=json.dumps({"streams": [video]}))
        with mock.patch.dict(G, sh=probe):
            self.assertEqual(BENCH["probe_fixture"]("/owned/gradient.mkv", "dark-gradient-10bit"), [video])
            for field, invalid in (("color_transfer", None), ("color_primaries", None),
                                   ("color_space", "bt2020nc"), ("color_range", "pc"),
                                   ("pix_fmt", "yuv420p")):
                original = video[field]
                video[field] = invalid
                with self.assertRaisesRegex(BENCH["BenchError"], "ten-bit limited BT.709"):
                    BENCH["probe_fixture"]("/owned/refused.mkv", "dark-gradient-10bit")
                video[field] = original


if __name__ == "__main__":
    unittest.main()
