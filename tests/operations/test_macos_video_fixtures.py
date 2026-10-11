"""Offline corpus integrity and decoded smoke observation regressions.

No FFmpeg or hardware is launched by these tests. Real source decoding is the
explicit generator/verifier build experiment, separate from this unit suite.
"""

from copy import deepcopy
import json
from pathlib import Path
import runpy
import shutil
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "crates/plurxd/fixtures/macos-processing"
TOOL = runpy.run_path(str(ROOT / "scripts/generate-macos-video-fixtures"))
GLOBALS = TOOL["verify_output"].__globals__


class MacosVideoFixturesCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="macos-fixture-test-")
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name) / "corpus"
        shutil.copytree(CORPUS, self.directory)
        self.manifest = json.loads((self.directory / "manifest.json").read_text())

    def validate(self):
        TOOL["validate_manifest"](self.manifest, self.directory)

    def test_shipped_corpus_is_complete_hash_pinned_and_within_budget(self):
        self.validate()
        size = sum(p.stat().st_size for p in self.directory.iterdir())
        self.assertLess(size, 2 * 1024 * 1024)
        self.assertEqual(["sdr8", "sdr10", "hdr10"], [f["id"] for f in self.manifest["fixtures"]])
        for fixture in self.manifest["fixtures"]:
            self.assertEqual("CC0-1.0", fixture["provenance"]["license"])
            self.assertEqual("$FFMPEG", fixture["generation_argv"][0])
            self.assertNotIn("/private/", " ".join(fixture["generation_argv"]))
        self.assertEqual(TOOL["manifest_schema"](), json.loads((CORPUS / "manifest.schema.json").read_text()))

    def test_shared_content_license_is_pinned_and_tampering_is_rejected(self):
        license_path = self.directory / "LICENSE.txt"
        self.assertEqual(TOOL["LICENSE"], license_path.read_text())
        self.assertIn("original algorithmically generated Profile 5 RPU", TOOL["LICENSE"])
        self.validate()
        license_path.write_text(license_path.read_text().replace("CC0-1.0", "All rights reserved"))
        with self.assertRaisesRegex(ValueError, "missing or altered CC0 content license"):
            self.validate()

    def test_corrupt_same_length_fixture_fails_hash_verification(self):
        path = self.directory / "sdr8.mp4"
        data = bytearray(path.read_bytes())
        data[-1] ^= 1
        path.write_bytes(data)
        with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
            self.validate()

    def test_missing_or_symlinked_fixture_is_unavailable(self):
        path = self.directory / "sdr10.mp4"
        path.unlink()
        with self.assertRaisesRegex(ValueError, "missing or symlinked"):
            self.validate()
        path.symlink_to(CORPUS / "sdr10.mp4")
        with self.assertRaisesRegex(ValueError, "missing or symlinked"):
            self.validate()

    def test_manifest_cannot_redirect_outside_corpus(self):
        self.manifest["fixtures"][0]["path"] = "../sdr8.mp4"
        with self.assertRaisesRegex(ValueError, "inconsistent path"):
            self.validate()

    def test_manifest_cannot_relabel_sdr_pixels_as_hdr(self):
        self.manifest["fixtures"][0]["class"] = "hdr10"
        with self.assertRaisesRegex(ValueError, "inconsistent class"):
            self.validate()
        self.assertAlmostEqual(0.751827096247041, TOOL["transfer"](1000, "hdr10"), places=12)
        self.assertNotAlmostEqual(TOOL["transfer"](10, "sdr"), TOOL["transfer"](10, "hdr10"))

    def test_manifest_cannot_silently_drop_10bit_smoke(self):
        self.manifest["fixtures"].pop(1)
        with self.assertRaisesRegex(ValueError, "exactly sdr8, sdr10, hdr10"):
            self.validate()

    def test_manifest_cannot_relax_color_pixel_or_geometry_contract(self):
        original = deepcopy(self.manifest)
        for location, key, value in (
            ("expected", "width", 160),
            ("expected", "color_transfer", "unknown"),
            ("input_pixels", "absolute_y_tolerance", 1000),
        ):
            with self.subTest(key=key):
                self.manifest = deepcopy(original)
                self.manifest["fixtures"][0][location][key] = value
                with self.assertRaisesRegex(ValueError, "inconsistent"):
                    self.validate()

    def test_future_and_boolean_versions_are_rejected(self):
        for version in (2, True):
            with self.subTest(version=version):
                self.manifest["schema_version"] = version
                with self.assertRaisesRegex(ValueError, "unsupported"):
                    self.validate()

    def test_p5_absence_is_explicit_and_generation_has_no_dolby_claim(self):
        limits = " ".join(self.manifest["limitations"])
        self.assertIn("No Profile 5/RPU-positive source", limits)
        self.assertNotIn("dv_p5", [f["class"] for f in self.manifest["fixtures"]])
        for fixture in self.manifest["fixtures"]:
            self.assertNotIn("-dolbyvision", fixture["generation_argv"])

    def output(self, identity="sdr8"):
        fixture = next(f for f in self.manifest["fixtures"] if f["id"] == identity)
        expectation = fixture["output_expectations"][0]
        expected = expectation["expected"]
        frames = [{**{k: v for k, v in expected.items() if k != "frame_count"},
                   "best_effort_timestamp_time": str(index / 12), "interlaced_frame": 0}
                  for index in range(12)]
        probe = {"streams": [{k: v for k, v in expected.items() if k != "frame_count"}],
                 "frames": frames}
        grays = expectation["pixels"].get("expected_y", [16, 20, 28, 80, 120, 160, 200, 200])
        raw = bytes(grays[x // 20] for y in range(90) for x in range(160)) + bytes([128]) * 7200
        return expectation, probe, raw * 12

    def observe(self, expectation, probe, raw, identity="sdr8"):
        with mock.patch.dict(GLOBALS, {"run": mock.Mock(side_effect=[json.dumps(probe).encode(), raw])}):
            return TOOL["verify_output"](Path("/tool/ffmpeg"), Path("/tool/ffprobe"),
                                         self.directory / "manifest.json", identity,
                                         expectation["graph_id"], Path("/output.mp4"))

    def test_output_uses_decoded_pixels_and_tolerances_not_encoded_byte_hashes(self):
        expectation, probe, raw = self.output()
        shifted = bytearray(raw)
        shifted[30 * 160 + 10] += 8
        report = self.observe(expectation, probe, bytes(shifted))
        self.assertEqual("verified_smoke_output", report["result"])
        self.assertNotIn("sha256", report)
        self.assertIn("No sustained performance", " ".join(report["limitations"]))

    def test_output_repeated_or_nonfinite_pts_is_rejected(self):
        for pts in ("0", "nan"):
            with self.subTest(pts=pts):
                expectation, probe, raw = self.output()
                probe["frames"][1]["best_effort_timestamp_time"] = pts
                with self.assertRaisesRegex(ValueError, "timestamps differ"):
                    self.observe(expectation, probe, raw)

    def test_output_wrong_frame_count_or_truncated_decode_is_rejected(self):
        expectation, probe, raw = self.output()
        probe["frames"].pop()
        with self.assertRaisesRegex(ValueError, "frame count"):
            self.observe(expectation, probe, raw)
        expectation, probe, raw = self.output()
        with self.assertRaisesRegex(ValueError, "incomplete or extra"):
            self.observe(expectation, probe, raw[:-1])

    def test_output_retains_bt709_signaling_on_every_frame(self):
        expectation, probe, raw = self.output()
        probe["frames"][7]["color_transfer"] = "smpte2084"
        with self.assertRaisesRegex(ValueError, "frame color_transfer differs"):
            self.observe(expectation, probe, raw)

    def test_output_stale_hdr_metadata_fails_even_with_sdr_tags(self):
        expectation, probe, raw = self.output()
        probe["frames"][7]["side_data_list"] = [{"side_data_type": "Mastering display metadata"}]
        with self.assertRaisesRegex(ValueError, "stale HDR/Dolby"):
            self.observe(expectation, probe, raw)

    def test_output_hdr_flat_or_reversed_luminance_fails_smoke(self):
        expectation, probe, raw = self.output("hdr10")
        flat = bytes([16]) * 14400 + bytes([128]) * 7200
        with self.assertRaisesRegex(ValueError, "lost contrast"):
            self.observe(expectation, probe, flat * 12, "hdr10")
        reversed_y = bytes([220 if x < 20 else 16 for y in range(90) for x in range(160)])
        reversed_raw = (reversed_y + bytes([128]) * 7200) * 12
        with self.assertRaisesRegex(ValueError, "reverse brightness ordering"):
            self.observe(expectation, probe, reversed_raw, "hdr10")

    def test_generation_refuses_existing_nonempty_output_before_launching_ffmpeg(self):
        with mock.patch.dict(TOOL["generate"].__globals__, {"run": mock.Mock()}) as patched:
            with self.assertRaisesRegex(ValueError, "new or empty"):
                TOOL["generate"](Path("/tool/ffmpeg"), Path("/tool/ffprobe"), self.directory)
            patched["run"].assert_not_called()


if __name__ == "__main__":
    unittest.main()
