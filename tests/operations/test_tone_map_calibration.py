"""The comparison must refuse unmatched frames rather than invent evidence."""

import copy
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest
from unittest.mock import patch

TOOL = runpy.run_path(str(Path(__file__).resolve().parents[2] / "scripts/tone-map-calibration"))


class ToneMapCalibrationTests(unittest.TestCase):
    def test_metadata_graph_drift_and_failed_execution_cannot_claim_evidence(self):
        validate = TOOL["validate_source_metadata"]
        with self.assertRaises(ValueError):
            validate({"streams": [{}], "frames": [{}]}, "pq-4000")
        tagged = {"frames": [{"side_data_list": [{"side_data_type": "Content light level metadata", "max_content": 4000}]}]}
        validate(tagged, "pq-4000")
        for case in ("pq-absent", "pq-1000", "hlg"):
            with self.subTest(case=case), self.assertRaises(ValueError):
                validate(tagged, case)
        source = Path(__file__).resolve().parents[2] / "crates/plurx-core/src/transcode/mod.rs"
        TOOL["production_binding"](source)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            changed = root / "mod.rs"
            changed.write_text(source.read_text().replace("tonemap=tonemap=hable:desat=0:peak={peak}", "tonemap=tonemap=clip:desat=0:peak={peak}"))
            with self.assertRaises(ValueError):
                TOOL["production_binding"](changed)
            runner = TOOL["Runner"]("ffmpeg", "ffprobe", root)
            with patch("subprocess.run", side_effect=subprocess.TimeoutExpired(["ffmpeg"], 45)):
                with self.assertRaises(subprocess.TimeoutExpired):
                    runner.run(["ffmpeg", "-version"])
            self.assertEqual(runner.commands[0]["argv"], ["ffmpeg", "-version"])
            self.assertIn("failure", runner.commands[0])
            self.assertIsNone(runner.commands[0]["exit"])

    def test_decoded_comparison_refuses_wrong_grade_geometry_or_grid(self):
        grade = TOOL["SDR"]
        fixture = {"streams": [dict(grade)], "frames": [dict(grade, best_effort_timestamp_time=str(n / 4)) for n in range(8)]}
        self.assertEqual(TOOL["validate_decoded"](fixture, grade), [n / 4 for n in range(8)])
        absent_container = copy.deepcopy(fixture)
        del absent_container["streams"][0]["color_transfer"]
        self.assertEqual(TOOL["validate_decoded"](absent_container, grade), [n / 4 for n in range(8)])
        del absent_container["frames"][0]["color_transfer"]
        with self.assertRaises(ValueError):
            TOOL["validate_decoded"](absent_container, grade)
        for key, value in (("color_transfer", "smpte2084"), ("width", 256), ("best_effort_timestamp_time", "1.01")):
            wrong = copy.deepcopy(fixture)
            wrong["frames"][4][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                TOOL["validate_decoded"](wrong, grade)
        with self.assertRaises(ValueError):
            TOOL["validate_decoded"](dict(fixture, frames=fixture["frames"][:-1]), grade)

    def test_authored_luminance_and_clipping_census_have_independent_controls(self):
        self.assertEqual(TOOL["pq_code"](0), 64)
        self.assertEqual(TOOL["pq_code"](10000), 940)
        self.assertEqual(TOOL["pq_code"](1000), 723)
        self.assertEqual(TOOL["hlg_code"](0), 64)
        self.assertEqual(TOOL["hlg_code"](1), 940)
        size = TOOL["WIDTH"] * TOOL["HEIGHT"]
        frame = bytes([16]) * (size // 2) + bytes([235]) * (size // 2) + bytes([128]) * (size // 2)
        readings = TOOL["luma_readings"](frame * 8)
        self.assertEqual(readings[0]["at_or_below_16_fraction"], 0.5)
        self.assertEqual(readings[0]["at_or_above_235_fraction"], 0.5)
        self.assertEqual(readings[0]["histogram"][235], size // 2)
        self.assertEqual(TOOL["difference"](frame, frame)["psnr_db"], "identical")
        with self.assertRaises(ValueError):
            TOOL["luma_readings"](frame * 7)


if __name__ == "__main__":
    unittest.main()
