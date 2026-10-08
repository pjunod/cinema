"""Analytical fixture and strict E1 source-contract regressions (no hardware)."""
from copy import deepcopy
from fractions import Fraction
from pathlib import Path
import runpy
import unittest

TOOL = runpy.run_path(str(Path(__file__).resolve().parents[2] / "scripts/generate-macos-extension-fixtures"))


class MacosExtensionFixturesCase(unittest.TestCase):
    def observation(self, family):
        stream = {"width": 320, "height": 180, "pix_fmt": "yuv420p10le",
                  "color_transfer": "arib-std-b67" if family == "hlg" else "smpte2084",
                  "color_primaries": "bt2020", "color_space": "bt2020nc" if family == "hlg" else "ipt-c2",
                  "color_range": "tv" if family == "hlg" else "pc", "time_base": "1/12288"}
        frames = [{"pts": i * 1024} for i in range(24)]
        if family == "p5":
            stream["side_data_list"] = [{"side_data_type": "DOVI configuration record", "dv_profile": 5, "el_present_flag": 0}]
            for frame in frames:
                frame["side_data_list"] = [{"side_data_type": "Dolby Vision Metadata", "disable_residual_flag": 1, "vdr_rpu_profile": 0,
                                            "signal_eotf": 65535, "signal_bit_depth": 12, "signal_color_space": 2,
                                            "signal_chroma_format": 0, "signal_full_range_flag": 1}]
        return {"streams": [stream], "frames": frames}

    def test_hlg_reference_white_uses_inverse_ootf_before_oetf(self):
        signal = TOOL["hlg_signal"]((203, 203, 203))
        for channel in signal:
            self.assertAlmostEqual(channel, 0.75, delta=0.001)
        self.assertAlmostEqual(TOOL["hlg_signal"]((1000, 1000, 1000))[0], 1, places=6)
        self.assertEqual(TOOL["codes"]((0, 0, 0), "hlg"), (64, 512, 512))

    def test_p5_missing_effective_metadata_is_rejected_initially_and_midstream(self):
        observed = self.observation("p5")
        TOOL["validate_observed"](observed, "p5")
        for index in (0, 12, 23):
            bad = deepcopy(observed)
            bad["frames"][index]["side_data_list"] = [{"side_data_type": "Dolby Vision RPU Data"}]
            with self.subTest(frame=index), self.assertRaisesRegex(ValueError, "effective Dolby metadata"):
                TOOL["validate_observed"](bad, "p5")

    def test_reordered_presentation_timestamps_cannot_qualify_fixture(self):
        observed = self.observation("p5")
        observed["frames"][7]["pts"], observed["frames"][8]["pts"] = observed["frames"][8]["pts"], observed["frames"][7]["pts"]
        with self.assertRaisesRegex(ValueError, "presentation timestamps"):
            TOOL["validate_observed"](observed, "p5")

    def test_other_dolby_profiles_and_enhancement_layers_are_not_p5_evidence(self):
        for profile, el in ((7, 1), (8, 0), (5, 1)):
            observed = self.observation("p5")
            config = observed["streams"][0]["side_data_list"][0]
            config.update(dv_profile=profile, el_present_flag=el)
            with self.subTest(profile=profile, el=el), self.assertRaisesRegex(ValueError, "base-layer-only P5"):
                TOOL["validate_observed"](observed, "p5")

    def test_hlg_cannot_inherit_pq_signaling(self):
        observed = self.observation("hlg")
        TOOL["validate_observed"](observed, "hlg")
        observed["streams"][0]["color_transfer"] = "smpte2084"
        with self.assertRaisesRegex(ValueError, "signal declarations"):
            TOOL["validate_observed"](observed, "hlg")

    def test_changing_metadata_is_bound_to_coded_au_across_reorder(self):
        order = [0, 3, 1, 2] + list(range(4, 24))
        packets = [{"pts": index * 1024} for index in order]
        ordinal = {int(packet["pts"]): i for i, packet in enumerate(packets)}
        frames = [{"pts": i * 1024, "side_data_list": [{"side_data_type": "Dolby Vision Metadata",
                   "dm_metadata_id": ordinal[i * 1024] % 16, "source_max_pq": 3079 + ordinal[i * 1024]}]}
                  for i in range(24)]
        TOOL["validate_frame_bound_metadata"]({"frames": frames}, packets)
        frames[1]["side_data_list"], frames[3]["side_data_list"] = frames[3]["side_data_list"], frames[1]["side_data_list"]
        with self.assertRaisesRegex(ValueError, "bound to its coded AU"):
            TOOL["validate_frame_bound_metadata"]({"frames": frames}, packets)

    def test_p5_generic_color_default_is_rejected_without_peak_blacklist(self):
        observed = self.observation("p5")
        for frame in observed["frames"]:
            frame["side_data_list"][0]["source_max_pq"] = 3696
        TOOL["validate_observed"](observed, "p5")
        observed["frames"][12]["side_data_list"][0].update(
            signal_eotf=39322, signal_bit_depth=14, signal_color_space=0)
        with self.assertRaisesRegex(ValueError, "supported effective Dolby metadata"):
            TOOL["validate_observed"](observed, "p5")
