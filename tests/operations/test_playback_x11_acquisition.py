"""Dual-position optical acquisition must never hide conflicting valid pixels."""
from importlib.machinery import SourceFileLoader
from importlib.util import module_from_spec, spec_from_loader
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import ctypes
import hashlib
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
LOADER = SourceFileLoader("cq_x11_acquisition", str(ROOT / "scripts/playback-x11-acquisition"))
SPEC = spec_from_loader(LOADER.name, LOADER)
ACQUISITION = module_from_spec(SPEC)
LOADER.exec_module(ACQUISITION)


def encoded_pixels(frame, corrupt=False):
    values = [0] + ACQUISITION.bits(frame) + [0]
    if corrupt:
        values[33] ^= 1  # Flip the encoded checksum, retaining a valid guard.
    return bytes(channel for _ in range(4) for value in values for channel in [255 * value] * 3)


class FakeX:
    """One native image encloses both strips; no host display is opened."""
    def __init__(self, first, second=None):
        self.display, self.root = 1, 2
        self.calls = []
        self.destroyed = 0
        self.lib = self
        self.first, self.second = first, second

    def inside(self, *_):
        pass

    def XGetImage(self, display, root, left, top, width, height, *_):
        self.calls.append((left, top, width, height))
        raw = bytearray(width * height * 4)
        for origin, rgb in [(0, self.first), (8, self.second)]:
            if rgb is None:
                continue
            for y in range(4):
                for cell in range(42):
                    offset = ((origin + y) * width + cell * 16 + 8) * 4
                    source = (y * 42 + cell) * 3
                    raw[offset:offset + 3] = rgb[source:source + 3]
        self.buffer = ctypes.create_string_buffer(bytes(raw))
        self.image = ACQUISITION.XImage(width=width, height=height,
            data=ctypes.cast(self.buffer, ctypes.c_void_p), byte_order=0,
            bits_per_pixel=32, bytes_per_line=width * 4,
            red_mask=255, green_mask=65280, blue_mask=16711680)
        return ctypes.pointer(self.image)

    def XDestroyImage(self, _):
        self.destroyed += 1


def acquire(first, second):
    x = FakeX(first, second)
    args = SimpleNamespace(video=(0, 0), video_alternate=(0, 8) if second is not None else None,
                           control=None, seconds=.01, rate=120, stop_file=None)
    rows = []
    with TemporaryDirectory() as directory:
        with patch.object(ACQUISITION.time, "monotonic_ns", side_effect=[0, 0, 100, 200, 300, 400, 1_000_000_000]), \
             patch.object(ACQUISITION, "paced", return_value=None):
            failure = None
            try:
                ACQUISITION.capture(x, args, rows.append, Path(directory))
            except RuntimeError as error:
                failure = str(error)
        streams = {p.name: p.read_bytes() for p in Path(directory).glob("*.rgb")}
    return x, rows, streams, failure


class PlaybackX11DualRoiIntegrityTests(unittest.TestCase):
    def test_conflicting_valid_counters_fail_integrity(self):
        first, second = encoded_pixels(42), encoded_pixels(43)
        x, rows, streams, failure = acquire(first, second)
        self.assertEqual(x.calls, [(0, 0, 672, 12)])
        self.assertEqual(x.destroyed, 1)
        sample = next(row for row in rows if row["type"] == "sample")
        self.assertEqual((sample["request_ns"], sample["reply_ns"]), (100, 200))
        self.assertEqual(sample["video_counter"]["status"], "conflicting")
        self.assertIsNone(sample["video_counter"]["frame"])
        self.assertIn("Conflicting valid video counters", failure)
        self.assertEqual(streams, {"video.rgb": first, "video_alternate.rgb": second})
        footer = rows[-1]
        self.assertEqual(footer["video_counter_conflicts"], 1)
        self.assertEqual(footer["sha256"]["video"], hashlib.sha256(first).hexdigest())
        self.assertEqual(footer["sha256"]["video_alternate"], hashlib.sha256(second).hexdigest())

    def test_corrupt_primary_cannot_hide_valid_alternate(self):
        corrupt, valid = encoded_pixels(42, corrupt=True), encoded_pixels(43)
        self.assertIsNone(ACQUISITION.decode(corrupt))
        _, rows, _, failure = acquire(corrupt, valid)
        self.assertIsNone(failure)
        counter = next(row for row in rows if row["type"] == "sample")["video_counter"]
        self.assertEqual(counter, {"status": "unambiguous", "frame": 43, "valid_rois": ["video_alternate"]})

    def test_agreeing_rois_and_single_roi_preserve_counter(self):
        rgb = encoded_pixels(42)
        for alternate, status in [(rgb, "agreeing"), (None, "unambiguous")]:
            with self.subTest(status=status):
                _, rows, _, failure = acquire(rgb, alternate)
                self.assertIsNone(failure)
                counter = next(row for row in rows if row["type"] == "sample")["video_counter"]
                self.assertEqual((counter["status"], counter["frame"]), (status, 42))

    def test_neither_valid_remains_unknown(self):
        _, rows, _, failure = acquire(encoded_pixels(42, True), encoded_pixels(43, True))
        self.assertIsNone(failure)  # Existing capture preserves unknown; acceptance rejects it.
        self.assertEqual(next(row for row in rows if row["type"] == "sample")["video_counter"],
                         {"status": "unknown", "frame": None, "valid_rois": []})
