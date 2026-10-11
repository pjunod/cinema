"""The qualification gradient must encode its grade, not just request tags."""

import hashlib
import json
from pathlib import Path
import runpy
from types import SimpleNamespace
import tempfile
import unittest
from unittest import mock


BENCH = runpy.run_path(str(Path(__file__).resolve().parents[2] / "scripts/bench"))
G = BENCH["build_fixture"].__globals__


class GradientVuiTests(unittest.TestCase):
    def test_gradient_binds_bt709_x265_vui_and_keeps_metadata_refusal(self):
        spec = BENCH["FIXTURES"]["dark-gradient-10bit"]
        argv = BENCH["fixture_command"]("/owned/gradient.mkv", spec)
        params = dict(part.split("=", 1) for part in argv[argv.index("-x265-params") + 1].split(":"))
        self.assertEqual({key: params.get(key) for key in ("colorprim", "transfer", "colormatrix", "range")},
                         {"colorprim": "bt709", "transfer": "bt709", "colormatrix": "bt709", "range": "limited"})
        self.assertEqual(spec["size"], "1920x1080")
        self.assertEqual(BENCH["DURATION"], 45)
        self.assertIn("duration=45", argv[argv.index("-i") + 1])
        other = {name: BENCH["fixture_command"]("/owned/" + name + ".mkv", entry)
                 for name, entry in BENCH["FIXTURES"].items()
                 if name != "dark-gradient-10bit" and not entry.get("external")}
        self.assertEqual(hashlib.sha256(json.dumps(other, sort_keys=True).encode()).hexdigest(),
                         "34f2cda89a74d4cf97194a875fae125d26cc3de3ab579ef90d33053d5bc14a0d")
        video = {"codec_type": "video", "pix_fmt": "yuv420p10le", "color_range": "tv",
                 "color_space": "bt709", "color_transfer": "bt709", "color_primaries": "bt709"}
        calls = []
        def tool(command):
            calls.append(command)
            return SimpleNamespace(returncode=0, stderr="", stdout=json.dumps({"streams": [video]}))
        with tempfile.TemporaryDirectory() as directory, mock.patch.dict(G, sh=tool):
            BENCH["build_fixture"](directory, "dark-gradient-10bit", spec)
            encoded = next(command for command in calls if command[0] == "ffmpeg")
            self.assertIn("colorprim=bt709:transfer=bt709:colormatrix=bt709:range=limited",
                          encoded[encoded.index("-x265-params") + 1])
            for missing in ("color_transfer", "color_primaries"):
                original = video.pop(missing)
                with self.assertRaisesRegex(BENCH["BenchError"], "ten-bit limited BT.709"):
                    BENCH["probe_fixture"]("/owned/refused.mkv", "dark-gradient-10bit")
                video[missing] = original


if __name__ == "__main__":
    unittest.main()
