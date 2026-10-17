"""Frame-reset evidence must fail when pixels or source-frame association change."""

import copy
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import runpy
import sys
import tempfile
import unittest

TOOL = runpy.run_path(str(Path(__file__).resolve().parents[2] / "scripts/qualify-tonemap-segments"))


def digest(data):
    return hashlib.sha256(data).hexdigest()


def framehash(path, frames, ticks, time_base, width=4, height=2):
    """An independently assembled FFmpeg v2 rawvideo framehash transcript."""
    lines = ["#format: frame checksums", "#version: 2", "#hash: SHA256",
             f"#tb 0: {time_base}", "#media_type 0: video", "#codec_id 0: rawvideo",
             f"#dimensions 0: {width}x{height}", "#sar 0: 1/1", "#stream#, dts, pts, duration, size, hash"]
    for data, pts in zip(frames, ticks):
        lines.append(f"0, {pts}, {pts}, 1, {len(data)}, {digest(data)}")
    path.write_text("\n".join(lines) + "\n")
    return TOOL["parse_framehash"](path)


class ToneMapSegmentTests(unittest.TestCase):
    def fixtures(self, root):
        # Three planes, two chroma samples each; changing a single V sample
        # below proves that aggregate luma parity cannot hide a boundary error.
        output = [bytes([16 + frame] * 8 + [128, 129, 130, 131]) for frame in range(8)]
        source = [bytes([frame, 0] * 12) for frame in range(8)]
        ticks = [0, 1001, 3003, 4004, 6006, 7007, 9009, 10010]
        src = framehash(root / "source.framehash", source, ticks, "1/24000")
        continuous = framehash(root / "continuous.framehash", output, [tick * 3 for tick in ticks], "1/72000")
        raw = root / "continuous.yuv"
        raw.write_bytes(b"".join(output))
        return source, output, ticks, src, continuous, raw

    def reset(self, root, source, output, ticks, start=3, end=7):
        input_frames = framehash(root / "reset-input.framehash", source[start:end], ticks[start:end], "1/24000")
        mapped_frames = framehash(root / "reset-output.framehash", output[start:end], ticks[start:end], "1/24000")
        raw = root / "reset.yuv"
        raw.write_bytes(b"".join(output[start:end]))
        return input_frames, mapped_frames, raw

    def test_identical_resets_preserve_every_rational_vfr_identity_and_boundary(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, output, ticks, src, baseline, raw = self.fixtures(root)
            for start, end in ((0, 3), (3, 7), (7, 8)):
                reset_input, reset_output, reset_raw = self.reset(root, source, output, ticks, start, end)
                result = TOOL["compare_window"](src, baseline, reset_input, reset_output, raw, reset_raw, start, end)
                self.assertEqual(result["result"], "passed")
                self.assertEqual(result["changed_frames"], 0)
                self.assertEqual([row["source_frame"] for row in result["frames"]], list(range(start, end)))
                self.assertEqual([Fraction(row["source_pts"]) for row in result["frames"]],
                                 [Fraction(tick, 24000) for tick in ticks[start:end]])

    def test_changed_chroma_sample_fails_even_with_matching_luma_and_pts(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, output, ticks, src, baseline, raw = self.fixtures(root)
            changed = list(output)
            changed[3] = output[3][:-1] + bytes([200])
            reset_input, reset_output, reset_raw = self.reset(root, source, changed, ticks)
            result = TOOL["compare_window"](src, baseline, reset_input, reset_output, raw, reset_raw, 3, 7)
            self.assertEqual(result["result"], "failed")
            self.assertEqual(result["changed_frames"], 1)
            first = result["boundary_frames"][0]
            self.assertEqual(first["source_frame"], 3)
            self.assertEqual(first["planes"]["y"]["max_absolute_difference"], 0)
            self.assertEqual(first["planes"]["v"]["max_absolute_difference"], 69)

    def test_dropped_extra_shifted_and_wrong_source_frames_cannot_claim_comparison(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source, output, ticks, src, baseline, raw = self.fixtures(root)
            reset_input, reset_output, reset_raw = self.reset(root, source, output, ticks)
            controls = []
            dropped = copy.deepcopy(reset_output)
            dropped["frames"].pop(0)
            controls.append((reset_input, dropped, baseline))
            extra = copy.deepcopy(reset_output)
            extra["frames"].append(extra["frames"][-1])
            controls.append((reset_input, extra, baseline))
            shifted = copy.deepcopy(reset_output)
            shifted["frames"][0]["pts"] = str(Fraction(6006, 24000))
            controls.append((reset_input, shifted, baseline))
            wrong_source = copy.deepcopy(reset_input)
            wrong_source["frames"][0]["sha256"] = digest(source[2])
            controls.append((wrong_source, reset_output, baseline))
            wrong_geometry = copy.deepcopy(reset_input)
            wrong_geometry["dimensions"] = [2, 4]
            controls.append((wrong_geometry, reset_output, baseline))
            missing_continuous = copy.deepcopy(baseline)
            missing_continuous["frames"].pop()
            controls.append((reset_input, reset_output, missing_continuous))
            for input_record, output_record, continuous in controls:
                with self.subTest(control=controls.index((input_record, output_record, continuous))), self.assertRaises(ValueError):
                    TOOL["compare_window"](src, continuous, input_record, output_record, raw, reset_raw, 3, 7)
            reset_raw.write_bytes(reset_raw.read_bytes()[:-1])
            with self.assertRaises(ValueError):
                TOOL["compare_window"](src, baseline, reset_input, reset_output, raw, reset_raw, 3, 7)

    def test_raw_hash_validation_rejects_tampered_bytes_and_framehash_contract(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            _, output, ticks, _, baseline, raw = self.fixtures(root)
            TOOL["validate_raw"](raw, baseline, 12, 8)
            raw.write_bytes(bytes([255]) + raw.read_bytes()[1:])
            with self.assertRaises(ValueError):
                TOOL["validate_raw"](raw, baseline, 12, 8)
            transcript = (root / "continuous.framehash").read_text()
            for malformed in (transcript.replace("#hash: SHA256", "#hash: MD5"),
                              transcript.replace("#hash: SHA256\n", ""),
                              transcript.replace("#tb 0: 1/72000", "#tb 0: 0/72000")):
                bad = root / "bad.framehash"
                bad.write_text(malformed)
                with self.assertRaises(ValueError):
                    TOOL["parse_framehash"](bad)
            # A duplicate timestamp is refused even when every byte hash exists.
            duplicate = root / "duplicate.framehash"
            with self.assertRaises(ValueError):
                framehash(duplicate, output, [ticks[0], ticks[0], *ticks[2:]], "1/24000")

    def test_declared_options_separate_fixed_peak_from_nooverride_controls(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            path = root / "case.json"
            fixed = {"id": "metal-fixed", "mode": "explicit", "peak_nits": 1179,
                     "peak_divisor": 10, "reference_white_nits": 100,
                     "graph": "tonemap_videotoolbox=tonemap=hable:peak=117.9:reference_white=100",
                     "output": {"width": 4, "height": 2}}
            path.write_text(json.dumps(fixed))
            loaded = TOOL["load_case"](path)
            self.assertEqual(loaded["manifest_sha256"], digest(path.read_bytes()))
            self.assertEqual(loaded["graph_sha256"], digest(fixed["graph"].encode()))
            invalid = [dict(fixed, graph=fixed["graph"].replace("117.9", "11.79")),
                       dict(fixed, graph=fixed["graph"].replace("white=100", "white=203")),
                       dict(fixed, mode="nooverride"),
                       dict(fixed, input_adapter="tonemap=peak=11.79"),
                       dict(fixed, input_adapter="zscale=npl=100:nominal_peak_luminance=203")]
            for case in invalid:
                path.write_text(json.dumps(case))
                with self.subTest(case=case), self.assertRaises(ValueError):
                    TOOL["load_case"](path)
            cpu_contradiction = dict(fixed, peak_divisor=100,
                graph="zscale=npl=100:nominal_peak_luminance=203,tonemap=tonemap=hable:peak=11.79")
            path.write_text(json.dumps(cpu_contradiction))
            with self.assertRaises(ValueError):
                TOOL["load_case"](path)
            nooverride = {"id": "metal-default", "mode": "nooverride",
                          "graph": "tonemap_videotoolbox=tonemap=hable",
                          "output": {"width": 4, "height": 2}}
            path.write_text(json.dumps(nooverride))
            self.assertEqual(TOOL["load_case"](path)["mode"], "nooverride")
            for graph in ("zscale=nominal_peak_luminance=203,tonemap=tonemap=hable",
                          "tonemap=hable:0:0:11.79", "tonemap_videotoolbox=hable:117.9",
                          "zscale=npl=203,tonemap=tonemap=hable",
                          "tonemap =hable:0:0:11.79", "zscale =nominal_peak_luminance=203",
                          "'tonemap'=hable:0:0:11.79", "zsca\\le=nominal_peak_luminance=203",
                          "tonemap=tonemap='hable:peak=11.79'",
                          "tonemap=tonemap=hable\\:peak=11.79",
                          'tonemap=tonemap="hable:peak=11.79"'):
                path.write_text(json.dumps(dict(nooverride, graph=graph)))
                with self.subTest(graph=graph), self.assertRaises(ValueError):
                    TOOL["load_case"](path)
            nooverride["output_adapter"] = "zscale=npl=203"
            path.write_text(json.dumps(nooverride))
            with self.assertRaises(ValueError):
                TOOL["load_case"](path)
            self.assertEqual(TOOL["declared_peak_options"]("tonemap =tonemap=hable"), {})
            self.assertEqual(TOOL["declared_peak_options"]("unrelated=nominal_peak_luminance=203"), {"npl": [Fraction(203)]})
            self.assertEqual(TOOL["declared_peak_options"](
                "zscale=w=512:nominal_peak_luminance=100,tonemap=tonemap=hable:peak=11.79"),
                {"npl": [Fraction(100)], "peak": [Fraction(1179, 100)]})
            with self.assertRaises(ValueError):
                TOOL["declared_peak_options"]("zscale=w=min(512\\,iw):npl=100")

    def test_decoded_color_and_complete_partition_are_required(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            path = root / "mapped.stderr"
            lines = []
            for index in range(3):
                lines += [f"[showinfo@mapped @ 0x123] n: {index} pts: {index * 1001} pts_time: 0 fmt:yuv420p sar:1/1 s:4x2 i:P",
                          "[showinfo@mapped @ 0x123] color_range:tv color_space:bt709 color_primaries:bt709 color_trc:bt709"]
            path.write_text("\n".join(lines))
            TOOL["validate_color_log"](path, "mapped", 3, 4, 2, TOOL["OUTPUT_COLOR"], "yuv420p")
            for wrong in ("\n".join(lines[:-1]), "\n".join(lines).replace("color_trc:bt709", "color_trc:smpte2084", 1)):
                path.write_text(wrong)
                with self.assertRaises(ValueError):
                    TOOL["validate_color_log"](path, "mapped", 3, 4, 2, TOOL["OUTPUT_COLOR"], "yuv420p")
            self.assertEqual(TOOL["windows"](8, [3, 7]), [(0, 3), (3, 7), (7, 8)])
            for cuts in ([3, 3], [7, 3], [0, 3], [3, 8], [-1, 3], [3, 9]):
                with self.subTest(cuts=cuts), self.assertRaises(ValueError):
                    TOOL["windows"](8, cuts)

    def test_deadline_and_nonzero_exit_preserve_command_and_logs(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            receipt = {"commands": []}
            runner = TOOL["Runner"](root, receipt, 0.1, 1024)
            with self.assertRaises(RuntimeError):
                runner.run([sys.executable, "-c", "import sys,time; print('started',file=sys.stderr,flush=True); time.sleep(10)"], "deadline")
            self.assertEqual(receipt["commands"][0]["failure"], "deadline exceeded")
            self.assertIn("started", (root / receipt["commands"][0]["stderr"]).read_text())
            with self.assertRaises(RuntimeError):
                runner.run([sys.executable, "-c", "import sys; print('retained failure',file=sys.stderr); sys.exit(7)"], "failed")
            saved = json.loads((root / "receipt.json").read_text())
            self.assertEqual(saved["commands"][1]["returncode"], 7)
            self.assertIn("retained failure", (root / saved["commands"][1]["stderr"]).read_text())


if __name__ == "__main__":
    unittest.main()
