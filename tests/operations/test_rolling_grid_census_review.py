"""Same-review synthetic regressions; never production measurement evidence."""
from fractions import Fraction
import json
from pathlib import Path
import runpy
import tempfile
import unittest

BASE = runpy.run_path(str(Path(__file__).with_name("test_rolling_grid_census.py")))
CENSUS = BASE["CENSUS"]


def change_probe(root, receipt, update):
    for row in receipt["segments"]:
        path = root / row["probe_name"]
        data = json.loads(path.read_text())
        update(data)
        path.write_text(json.dumps(data))
        row["probe_sha256"] = CENSUS["digest"](path)


class RollingGridReviewTests(unittest.TestCase):
    def test_distinct_complete_fractional_packet_span_cannot_hide_false_no_drift(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            receipt = BASE["fixture"](root)
            def duplicate(probe):
                probe["streams"][0]["time_base"] = "1/24"
                start = probe["packets"][0]["pts_time"]
                for packet in probe["packets"]:
                    packet["pts_time"] = start
            change_probe(root, receipt, duplicate)
            with self.assertRaises(ValueError):
                CENSUS["evaluate"](root, receipt)
            receipt = BASE["fixture"](root)
            def fractional(probe):
                probe["streams"][0].update(avg_frame_rate="24000/1001", time_base="1/90000")
                number = round(float(probe["packets"][0]["pts_time"]) / 2)
                for frame, packet in enumerate(probe["packets"]):
                    packet["pts_time"] = str(float(Fraction(number * 48 + frame) * Fraction(1001, 24000)))
            change_probe(root, receipt, fractional)
            receipt["provenance"]["output_cadence"] = [24000, 1001]
            playlist = root / "index.m3u8"
            playlist.write_text(playlist.read_text().replace("2.000000", "2.002000"))
            receipt["playlist"]["sha256"] = CENSUS["digest"](playlist)
            self.assertEqual(CENSUS["evaluate"](root, receipt)["verdict"], "no-drift")
            playlist.write_text(playlist.read_text().replace("2.002000", "2.200000"))
            receipt["playlist"]["sha256"] = CENSUS["digest"](playlist)
            result = CENSUS["evaluate"](root, receipt)
            self.assertEqual(result["verdict"], "drift")
            self.assertGreater(result["max_grid_error_ms"], 190)

    def test_bounded_input_and_unsupported_init_context_refuse_before_parse(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            receipt = BASE["fixture"](root)
            playlist = (root / "index.m3u8").read_text()
            for unsupported in (playlist.replace("#EXTM3U", '#EXTM3U\n#EXT-X-MAP:URI="init.mp4"'),
                                playlist.replace(".ts", ".m4s")):
                with self.assertRaisesRegex(ValueError, "unsupported.*init|unsupported.*context"):
                    CENSUS["playlist_entries"](unsupported)
            huge = root / "huge.json"
            huge.write_bytes(b" " * (1024 * 1024 + 1))
            with self.assertRaisesRegex(ValueError, "input bound"):
                CENSUS["bounded_text"](huge, 1024 * 1024)
            # Byte cap, not character count, and cap+1 detects a growing stream.
            huge.write_bytes("é".encode() * 1024)
            with self.assertRaisesRegex(ValueError, "input bound"):
                CENSUS["bounded_text"](huge, 1024)
