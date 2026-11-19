"""Focused served-readback alignment checks without media or rendering."""
import hashlib
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from served_refit_eval import VIDEO_CONFIG, bind_readback, validate_curves, verify_extraction


def readback(pts):
    return {
        "source_pts": pts, "rpu_sha256": "1" * 64, "bytes": 435,
        "profile": 8, "bl_bit_depth": 10, "vdr_bit_depth": 12,
        "coefficient_log2_denom": 23, "disable_residual_flag": True,
        "luma_pivot_deltas": [0, 128, 128, 128, 128, 128, 128, 128, 127],
        "luma_coef": [[0, 1, 0] for _ in range(8)],
        "chroma": [{"constant": 0, "coef": [[0] * 7 for _ in range(3)]} for _ in range(2)],
    }


class ServedReadbackTest(unittest.TestCase):
    def setUp(self):
        self.refs = {name: {"sha256": str(index) * 64}
                     for index, name in enumerate(("source", "init", "readback"))}
        self.receipt = {f"{name}_sha256": ref["sha256"] for name, ref in self.refs.items()}
        self.receipt.update(source_time_base="1/1000", video_config=dict(VIDEO_CONFIG))

    def bind(self, frames, *, allow_extra=False):
        return bind_readback({"frames": frames}, self.receipt, self.refs, [125, 167], allow_extra)

    def test_aligns_presentation_clocks_and_refuses_duplicate_or_missing_frames(self):
        first, second = readback(125), readback(167)
        self.assertEqual(self.bind([second, first]), [first, second])
        with self.assertRaisesRegex(ValueError, "duplicate PTS"):
            self.bind([first, second, first])
        with self.assertRaisesRegex(ValueError, "missing"):
            self.bind([first])

    def test_only_explicit_outside_window_rows_can_be_filtered(self):
        frames = [readback(125), readback(167)]
        with self.assertRaisesRegex(ValueError, "unexpected readback PTS 2127"):
            self.bind([*frames, readback(2127)])
        self.assertEqual(self.bind([*frames, readback(2127)], allow_extra=True), frames)
        with self.assertRaisesRegex(ValueError, "unexpected readback PTS 209"):
            self.bind([*frames, readback(209)], allow_extra=True)

    def test_init_profile_and_production_chroma_grid_are_required(self):
        self.receipt["video_config"]["dv_bl_signal_compatibility_id"] = 4
        with self.assertRaisesRegex(ValueError, "served init profile"):
            self.bind([readback(125), readback(167)])
        curve = readback(125)
        curve["chroma"][0]["constant"] = 2 ** -23
        with self.assertRaisesRegex(ValueError, "Q12"):
            validate_curves(curve)


    def test_extraction_retains_raw_wire_bytes_for_each_scored_clock(self):
        with TemporaryDirectory() as scratch:
            root = Path(scratch)
            row = readback(125)
            name = "source-pts-125.nal"
            nal = root / name
            nal.write_bytes(b"wire")
            row.update(out=name, bytes=4, rpu_sha256=hashlib.sha256(b"wire").hexdigest())
            capture = {"kind": "accepted_source_pair", "frame": 0, "rpu_sha256": "b" * 64,
                       "packet_sha256": "c" * 64}
            served = root / "served.mp4"
            served.write_bytes(b"media")
            served_sha = hashlib.sha256(b"media").hexdigest()
            refs = {name: {"sha256": "d" * 64} for name in ("source", "events", "init")}
            receipt = {"schema": 1, "kind": "served_refit_extraction",
                       "artifacts": {**refs, "served": {"path": str(served), "bytes": 5, "sha256": served_sha}},
                       "served_sha256": served_sha, "served_time_base": "1/1000",
                       "source_pts_offset_seconds": "125/1000",
                       "frames": [{"frame": 0, "source_pts": 125, "served_pts": 0,
                                   "source_rpu_sha256": capture["rpu_sha256"],
                                   "served_rpu_sha256": row["rpu_sha256"],
                                   "source_packet": {"data_hash": "SHA256:" + capture["packet_sha256"]},
                                   "served_packet": {"pts": 0}, "bl_vcl_sha256": "e" * 64, "bl_vcl_nals": 1}]}
            paths = {"readback": root / "readback.json"}
            verify_extraction(receipt, paths, refs, [capture], [row])
            nal.write_bytes(b"edit")
            with self.assertRaisesRegex(ValueError, "served RPU bytes differ"):
                verify_extraction(receipt, paths, refs, [capture], [row])


if __name__ == "__main__":
    unittest.main()
