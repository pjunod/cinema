"""The helper and the native player reconstruct one master: one renderer release,
bilinear plane sampling, the same 10-bit plane storage.

A nearest-neighbour helper once reconstructed a different master than the
player it was qualified against (the display-management stage was faithful;
the planes under it were not). These are the CPU-checkable parts of that
contract; pixel parity on real frames needs a GPU and private captures.
"""
from pathlib import Path
import os
import re
import unittest

from tools.dv_processing import test_metadata as metadata_checks

ROOT = Path(__file__).resolve().parents[2]
TOOLS = ROOT / "tools/dv_processing"
PLAYER = ROOT / "clients/linux-player/packaging"


def renderer_pin():
    scope = {}
    source = (TOOLS / "fetch_renderer_sources.py").read_text()
    exec(source[source.index("SOURCES = ("):source.index("def fetch(")], scope)
    return scope["SOURCES"][0]


class DvReconstructionSamplingTests(unittest.TestCase):
    def test_helper_and_player_build_the_same_renderer_release(self):
        row = next(line.split() for line in (PLAYER / "upstream.lock").read_text().splitlines()
                   if line.startswith("libplacebo "))
        pin = renderer_pin()
        self.assertEqual("v" + row[1], pin["revision"])
        self.assertEqual(row[2], pin["sha256"])
        self.assertEqual(row[3], pin["url"])
        api = int(row[1].split(".")[1])
        self.assertIn(f"libplacebo-{pin['revision']}", (TOOLS / "build-dependencies.sh").read_text())
        self.assertIn(f'abi.get("libplacebo") != {api}', (TOOLS / "bundle-manifest.py").read_text())
        self.assertIn(f"libplacebo.so.{api}", (TOOLS / "build-bundle.sh").read_text())
        self.assertIn(f'manifest.abi.get("libplacebo") != Some(&{api})',
                      (ROOT / "crates/plurxd/src/dv_runtime.rs").read_text())

    def test_both_sides_sample_every_plane_bilinearly_without_a_tie_bias(self):
        player = (PLAYER / "patches/mpv/0003-vo_gpu_next-map-Dolby-Vision-with-plurx-s-closed-for.patch").read_text()
        self.assertIn("+    params.plane_upscaler = &pl_filter_bilinear;", player)
        self.assertIn("+    params.plane_downscaler = &pl_filter_bilinear;", player)
        for name in ("fel_renderer.h", "base_dv_renderer.h"):
            source = (TOOLS / name).read_text()
            self.assertNotIn("pl_filter_nearest", source, name)
            self.assertIsNone(re.search(r"shift_[xy]\s*[-+]?=", source), name)
            self.assertRegex(source, r"plane_upscaler = &pl_filter_bilinear;")
            self.assertRegex(source, r"plane_downscaler = &pl_filter_bilinear;")

    def test_software_planes_use_the_players_shifted_unorm16_storage(self):
        upload = (PLAYER / "patches/mpv/0007-vo_gpu_next-preserve-DV-software-sampling-precision.patch").read_text()
        self.assertIn("code <<= 6;", upload)
        fel = (TOOLS / "fel_renderer.h").read_text()
        base = (TOOLS / "base_dv_renderer.h").read_text()
        self.assertEqual(2, fel.count(".bits = {.sample_depth = 16, .color_depth = 10, .bit_shift = 6}"))
        self.assertIn("dv_shift_10bit_plane(decoded[layer] + offset, count, plane)", fel)
        self.assertIn("pl_find_fmt(gpu, PL_FMT_UNORM, 1, 16, 16,", fel)
        self.assertIn("dv_shift_10bit_plane(row, tex->params.w,", base)
        self.assertIn("source->repr.bits.bit_shift = 6;", base)
        # No output bit depth on the reconstruction target: the renderer would
        # rescale a float target as if it held integer samples.
        self.assertNotIn(".color_depth = 32", fel + base)

    @unittest.skipUnless(os.name == "posix", "the native helper is POSIX only")
    def test_shifted_storage_keeps_quarter_code_taps_exact(self):
        metadata_checks.MetadataAdmission().test_finite_rgb48_rounding()


if __name__ == "__main__":
    unittest.main()
