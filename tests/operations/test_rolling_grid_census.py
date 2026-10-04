"""Synthetic copied evidence exercises the consumer; not production evidence."""
import copy
from fractions import Fraction
import json
from pathlib import Path
import runpy
import tempfile
import unittest

CENSUS = runpy.run_path(str(Path(__file__).resolve().parents[2] / "scripts/rolling-grid-census"))


def fixture(root, extra_keys=False, drift=False):
    facts = {"route": "rolling-transcode", "source_commit": "a" * 40,
             "source_sha256": "b" * 64, "graph_argv_sha256": "c" * 64,
             "daemon_sha256": "d" * 64, "tool_sha256": "e" * 64,
             "session_id": "synthetic-only", "family": "software", "pipeline": "cpu",
             "fixture": "synthetic-test", "tool_version": "synthetic",
             "rung": 360, "output_cadence": [24, 1], "output_geometry": [640, 360]}
    rows = []
    lines = ["#EXTM3U"]
    for number in range(30):
        name = f"seg{number:05}.ts"
        duration = Fraction(13, 6) if drift and number == 5 else Fraction(2)
        lines += [f"#EXTINF:{float(duration):.6f},", name]
        (root / name).write_bytes(b"synthetic-not-media" + bytes([number]))
        packets = [{"pts_time": str(number * 2 + frame / 24),
                    "flags": "K_" if frame == 0 or (extra_keys and frame == 20) else "__"}
                   for frame in range(48)]
        raw = root / (name + ".probe.json")
        raw.write_text(json.dumps({"streams": [{"avg_frame_rate": "24/1", "time_base": "1/90000",
                                               "width": 640, "height": 360}], "packets": packets}))
        rows.append({"name": name, "sha256": CENSUS["digest"](root / name),
                     "probe_name": raw.name, "probe_sha256": CENSUS["digest"](raw)})
    playlist = root / "index.m3u8"
    playlist.write_text("\n".join(lines) + "\n")
    return {"version": 1, "provenance": facts,
            "playlist": {"name": playlist.name, "sha256": CENSUS["digest"](playlist)}, "segments": rows}


class RollingGridReceiptTests(unittest.TestCase):
    def test_rolling_grid_receipt_requires_all_segments_and_exact_cadence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            receipt = fixture(root)
            self.assertEqual(CENSUS["evaluate"](root, receipt)["segments"], 30)
            for mutate in (lambda r: r["segments"].pop(),
                           lambda r: r["provenance"].update(output_cadence=[25, 1]),
                           lambda r: r["provenance"].update(output_geometry=[1280, 720]),
                           lambda r: r["provenance"].update(graph_argv_sha256=""),
                           lambda r: r["segments"][0].update(probe_sha256="0" * 64),
                           lambda r: r["segments"][0].update(name="../foreign.ts")):
                bad = copy.deepcopy(receipt)
                mutate(bad)
                with self.assertRaises(ValueError):
                    CENSUS["evaluate"](root, bad)
            media = root / receipt["segments"][0]["name"]
            media.unlink()
            media.symlink_to(root / receipt["segments"][1]["name"])
            with self.assertRaises(ValueError):
                CENSUS["evaluate"](root, receipt)

    def test_rolling_grid_receipt_distinguishes_extra_idr_from_start_drift(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            receipt = fixture(root, extra_keys=True)
            result = CENSUS["evaluate"](root, receipt)
            self.assertEqual(result["verdict"], "no-drift")
            self.assertEqual(result["internal_extra_keys"], 30)
            self.assertIn("not an independent IDR", result["limits"])
            receipt = fixture(root, extra_keys=True, drift=True)
            result = CENSUS["evaluate"](root, receipt)
            self.assertEqual(result["verdict"], "drift")
            self.assertGreater(result["max_grid_error_frames"], 1)
