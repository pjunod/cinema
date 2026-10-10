"""Dual-position optical acquisition must never hide conflicting valid pixels."""
from importlib.machinery import SourceFileLoader
from importlib.util import module_from_spec, spec_from_loader
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import ctypes
import hashlib
import json
import subprocess
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

class RenderedMappedX(FakeX):
    def __init__(self, strips):
        super().__init__(None)
        self.strips = strips

    def XGetImage(self, display, root, left, top, width, height, *_):
        self.calls.append((left, top, width, height))
        raw = bytearray(width * height * 4)
        # Rasterize the encoded reference cells independently of roi_sampling:
        # a device pixel center is projected back into the source rectangle.
        for mapping, frame in self.strips:
            origin_x, origin_y, scale = mapping
            cells = [0] + ACQUISITION.bits(frame) + [0]
            for y in range(height):
                source_y = (top + y + .5 - origin_y) / scale
                if not 0 <= source_y < 64:
                    continue
                for x in range(width):
                    source_x = (left + x + .5 - origin_x) / scale
                    if not 0 <= source_x < 672:
                        continue
                    value = 255 * cells[int(source_x // 16)]
                    offset = (y * width + x) * 4
                    raw[offset:offset + 3] = bytes([value] * 3)
        self.buffer = ctypes.create_string_buffer(bytes(raw))
        self.image = ACQUISITION.XImage(width=width, height=height,
            data=ctypes.cast(self.buffer, ctypes.c_void_p), byte_order=0,
            bits_per_pixel=32, bytes_per_line=width * 4,
            red_mask=255, green_mask=65280, blue_mask=16711680)
        return ctypes.pointer(self.image)


def acquire_mapped(strips, vertical_search=False, x_override=None):
    x = x_override or RenderedMappedX(strips)
    args = SimpleNamespace(video=None, video_alternate=None, video_map=strips[0][0],
        video_alternate_map=strips[1][0] if len(strips) > 1 else None, video_vertical_search=vertical_search,
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


class PlaybackX11MappedRoiTests(unittest.TestCase):
    def test_fractional_compact_geometry_preserves_encoded_counter(self):
        mapping = (10.375, 2.625, .3125)
        x, rows, streams, failure = acquire_mapped([(mapping, 0x123456)])
        self.assertIsNone(failure)
        self.assertEqual(x.calls, [(10, 2, 211, 21)])
        self.assertEqual(x.destroyed, 1)
        calibration = rows[0]["calibrated_sampling"]["video"]
        self.assertEqual(calibration["sampled_x"][:3], [12, 17, 22])
        self.assertEqual(calibration["sampled_y"], [5, 10, 15, 20])
        self.assertEqual(streams["video.rgb"], encoded_pixels(0x123456))
        sample = next(row for row in rows if row["type"] == "sample")
        self.assertEqual(sample["video_counter"]["frame"], 0x123456)
        self.assertEqual((sample["request_ns"], sample["reply_ns"]), (100, 200))

    def test_different_scale_conflict_cannot_be_hidden(self):
        _, rows, streams, failure = acquire_mapped([((.25, .5, .25), 42), ((200.5, 30.25, 1), 43)])
        sample = next(row for row in rows if row["type"] == "sample")
        self.assertEqual(sample["video_counter"]["status"], "conflicting")
        self.assertIn("Conflicting valid video counters", failure)
        self.assertEqual(streams["video.rgb"], encoded_pixels(42))
        self.assertEqual(streams["video_alternate.rgb"], encoded_pixels(43))
        self.assertEqual(rows[-1]["video_counter_conflicts"], 1)

    def test_minimum_cell_pitch_and_invalid_maps_are_explicit(self):
        _, rows, _, failure = acquire_mapped([((.25, .5, .125), 123)])
        self.assertIsNone(failure)
        self.assertEqual(next(row for row in rows if row["type"] == "sample")["video_counter"]["frame"], 123)
        for mapping in [(0, 0, .1249), (0, 0, float("nan")), (-.1, 0, 1), (0, 0, 4.01)]:
            with self.subTest(mapping=mapping), self.assertRaises(RuntimeError):
                ACQUISITION.roi_sampling(mapping)

class BandMappedX(RenderedMappedX):
    def XGetImage(self, *args):
        pointer = super().XGetImage(*args)
        # Keep only one16-reference-pixel horizontal band. Four adjacent
        # pixels could decode; the original four16px-spaced replicas cannot.
        image = pointer.contents
        raw = bytearray(ctypes.string_at(image.data, image.bytes_per_line * image.height))
        top = args[3]
        origin = self.strips[0][0][1]
        for y in range(image.height):
            if not origin + 24 <= top + y < origin + 40:
                raw[y * image.bytes_per_line:(y + 1) * image.bytes_per_line] = bytes(image.bytes_per_line)
        self.buffer = ctypes.create_string_buffer(bytes(raw))
        self.image.data = ctypes.cast(self.buffer, ctypes.c_void_p)
        return ctypes.pointer(self.image)


class PlaybackX11SpacedSearchTests(unittest.TestCase):
    def test_intermediate_stripe_preserves_four_spaced_row_integrity(self):
        # Actual stripe20..84 is between declared stripes0..64 and40..104.
        # Both fixed samples are partial, yet translated8/24/40/56 fits it.
        x = RenderedMappedX([((0, 20, 1), 308)])
        _, rows, streams, failure = acquire_mapped([((0, 0, 1), 0), ((0, 40, 1), 0)], True, x)
        self.assertIsNone(failure)
        self.assertEqual(len(x.calls), 1)
        sample = next(row for row in rows if row["type"] == "sample")
        self.assertIsNone(sample["strips"]["video"]["frame"])
        self.assertIsNone(sample["strips"]["video_alternate"]["frame"])
        self.assertEqual(sample["video_counter"]["frame"], 308)
        search = sample["vertical_search"]
        self.assertEqual(search["candidate_count"], 41)
        self.assertEqual(search["valid_frames"], [308])
        self.assertEqual([b - a for a, b in zip(search["chosen_spaced_rows"], search["chosen_spaced_rows"][1:])], [16, 16, 16])
        self.assertEqual(streams["video_search.rgb"], encoded_pixels(308))
        self.assertEqual(len(search["acquired_region_sha256"]), 64)

    def test_one_valid_sixteen_pixel_band_remains_unknown(self):
        x = BandMappedX([((0, 20, 1), 308)])
        _, rows, _, failure = acquire_mapped([((0, 0, 1), 0), ((0, 40, 1), 0)], True, x)
        self.assertIsNone(failure)
        sample = next(row for row in rows if row["type"] == "sample")
        self.assertEqual(sample["vertical_search"]["valid_count"], 0)
        self.assertEqual(sample["video_counter"]["status"], "unknown")

    def test_conflicting_intermediate_windows_fail_even_when_fixed_rois_unknown(self):
        x = RenderedMappedX([((0, 20, 1), 42), ((0, 80, 1), 43)])
        _, rows, _, failure = acquire_mapped([((0, 0, 1), 0), ((0, 120, 1), 0)], True, x)
        sample = next(row for row in rows if row["type"] == "sample")
        self.assertEqual(sample["vertical_search"]["valid_frames"], [42, 43])
        self.assertIsNone(sample["strips"]["video"]["frame"])
        self.assertIsNone(sample["strips"]["video_alternate"]["frame"])
        self.assertEqual(sample["video_counter"]["status"], "conflicting")
        self.assertIn("Conflicting valid video counters", failure)

    def test_fractional_maps_search_only_the_actual_spaced_pattern(self):
        x = RenderedMappedX([((10.375, 10.4, .3125), 123)])
        _, rows, _, failure = acquire_mapped([((10.375, .1, .3125), 0), ((10.375, 20.1, .3125), 0)], True, x)
        self.assertIsNone(failure)
        start = rows[0]["vertical_search"]
        self.assertEqual(start["relative_spaced_y"], [0, 5, 10, 15])
        self.assertEqual((start["first_row_min"], start["first_row_max"], start["candidate_count"]), (2, 22, 21))
        self.assertEqual(next(r for r in rows if r["type"] == "sample")["video_counter"]["frame"], 123)

    def test_search_rejects_different_columns_patterns_or_excessive_bounds(self):
        primary = ACQUISITION.roi_sampling((0, 0, 1))
        bad = [(1, 20, 1), (0, 20, .5), (0, 256, 1)]
        for mapping in bad:
            with self.subTest(mapping=mapping), self.assertRaises(RuntimeError):
                ACQUISITION.vertical_search_plan({"video": primary, "video_alternate": ACQUISITION.roi_sampling(mapping)})
        first = ACQUISITION.roi_sampling((0, 0, .2))
        second = ACQUISITION.roi_sampling((0, 20.5, .2))
        self.assertNotEqual([v - first['sampled_y'][0] for v in first['sampled_y']], [v - second['sampled_y'][0] for v in second['sampled_y']])
        with self.assertRaises(RuntimeError):
            ACQUISITION.vertical_search_plan({"video": first, "video_alternate": second})


class PlaybackX11PixelLayoutTests(unittest.TestCase):
    def test_byte_exact_channels_preserve_masks_endianness_and_padded_stride(self):
        for bpp in (3, 4):
            for byte_order, order in [(0, 'little'), (1, 'big')]:
                with self.subTest(bpp=bpp, order=order):
                    stride = 672 * bpp + 16
                    raw = bytearray(stride * 4)
                    expected = bytearray()
                    for y in range(4):
                        for cell in range(42):
                            rgb = ((cell * 7 + y) % 256, (cell * 13 + y * 9) % 256, (cell * 17 + y * 31) % 256)
                            word = rgb[0] << 16 | rgb[1] << 8 | rgb[2]
                            offset = y * stride + (cell * 16 + 8) * bpp
                            raw[offset:offset + bpp] = word.to_bytes(bpp, order)
                            expected.extend(rgb)
                    image = ACQUISITION.XImage(width=672, height=4, bits_per_pixel=bpp * 8, byte_order=byte_order, bytes_per_line=stride, red_mask=0xff0000, green_mask=0xff00, blue_mask=0xff)
                    self.assertEqual(ACQUISITION.strip(image, bytes(raw), (0, 0)), bytes(expected))

    def test_generic_sixteen_bit_rounding_and_invalid_layouts(self):
        word = 17 << 11 | 35 << 5 | 9
        image = ACQUISITION.XImage(width=672, height=4, bits_per_pixel=16, byte_order=0, bytes_per_line=1344, red_mask=0xf800, green_mask=0x7e0, blue_mask=0x1f)
        rgb = bytes([round(17 * 255 / 31), round(35 * 255 / 63), round(9 * 255 / 31)])
        self.assertEqual(ACQUISITION.strip(image, word.to_bytes(2, 'little') * 672 * 4, (0, 0)), rgb * 42 * 4)
        for mask in [0, 0x15, 0x10000, image.green_mask]:
            with self.subTest(mask=mask):
                image.red_mask = mask
                with self.assertRaises(RuntimeError):
                    ACQUISITION.strip(image, bytes(1344 * 4), (0, 0))
        image.red_mask = 0xf800
        with self.assertRaises(RuntimeError):
            ACQUISITION.strip(image, bytes(1344 * 4), (-9, 0))
        image.bytes_per_line = 1
        with self.assertRaises(RuntimeError):
            ACQUISITION.strip(image, bytes(4), (0, 0))


class PlaybackX11CpuObservationSafetyTests(unittest.TestCase):
    def test_cpu_metadata_cannot_override_wall_sampling_failure(self):
        # Capture two valid images across a real wall gap in the mocked host
        # clock. Even a zero-CPU wait must remain a failed sampling interval.
        x = FakeX(encoded_pixels(42))
        args = SimpleNamespace(video=(0, 0), video_alternate=None, control=None,
                               seconds=.1, rate=120, stop_file=None)
        for cpu_values in ([10] * 6, [None] * 6):
            rows = []
            with self.subTest(cpu_values=cpu_values), TemporaryDirectory() as directory:
                with patch.object(ACQUISITION.time, "monotonic_ns", side_effect=[
                        0, 0, 1_000_000, 2_000_000, 3_000_000, 4_000_000,
                        20_000_000, 21_000_000, 22_000_000, 23_000_000,
                        24_000_000, 1_000_000_000]), \
                     patch.object(ACQUISITION, "own_process_cpu_ns", side_effect=cpu_values), \
                     patch.object(ACQUISITION, "paced", return_value=None):
                    ACQUISITION.capture(x, args, rows.append, Path(directory))
            samples = [row for row in rows if row["type"] == "sample"]
            self.assertEqual(len(samples), 2)
            self.assertEqual(samples[1]["reply_ns"] - samples[0]["request_ns"], 21_000_000)
            self.assertEqual(samples[0]["own_process_cpu"]["status"],
                             "unknown" if cpu_values[0] is None else "measured")
            self.assertTrue(samples[0]["own_process_cpu"]["advisory_only"])
            script = """
                const {analyzeFrameClock}=require('./scripts/playback-frame-clock.js');
                const input=JSON.parse(require('node:fs').readFileSync(0,'utf8'));
                const plain=input.map(({at_ms,frame})=>({at_ms,frame}));
                const options={startMs:1,endMs:22,maximumSamplingGapMs:12.5};
                const observed=analyzeFrameClock(input,options),baseline=analyzeFrameClock(plain,options);
                process.stdout.write(JSON.stringify({observed,baseline}));
            """
            payload = [{"at_ms": row["reply_ns"] / 1e6,
                        "frame": row["video_counter"]["frame"],
                        "own_process_cpu": row["own_process_cpu"]} for row in samples]
            result = subprocess.run(["node", "-e", script], cwd=ROOT, input=json.dumps(payload),
                                    text=True, capture_output=True, check=True, timeout=10)
            verdicts = json.loads(result.stdout)
            self.assertEqual(verdicts["observed"], verdicts["baseline"])
            self.assertFalse(verdicts["observed"]["complete"])
            self.assertEqual(verdicts["observed"]["capture_gaps"], 1)
        with patch.object(ACQUISITION.time, "process_time_ns", side_effect=OSError("unavailable")):
            self.assertIsNone(ACQUISITION.own_process_cpu_ns())
        self.assertEqual(ACQUISITION.own_process_cpu_observation(3, 2, 1)["status"], "unknown")
