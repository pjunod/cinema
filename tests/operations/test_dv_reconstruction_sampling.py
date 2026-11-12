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

    def test_reconstruction_target_is_unorm16_or_the_device_is_refused(self):
        export = (TOOLS / "yuv420p10.h").read_text()
        helper = export[export.index("static pl_tex dv_yuv10_source_texture("):]
        helper = helper[:helper.index("\n}\n")]
        self.assertIn("int width, int height, bool linear)", helper)
        self.assertIn("(linear ? PL_FMT_CAP_LINEAR : 0)", helper)
        for name, target in (("fel_renderer.h", "context.native_texture"),
                             ("base_dv_renderer.h", "base_context.native_texture")):
            source = (TOOLS / name).read_text()
            # One assignment: the linearly sampled UNORM16 target, with no
            # float texture created in its place when the device lacks it.
            self.assertEqual(len(re.findall(re.escape(target) + r" = ", source)), 1, name)
            created = source[source.index(target + " = "):]
            created = created[:created.index(";\n", created.index("require(")) + 1]
            self.assertRegex(created, r"dv_yuv10_source_texture\(gpu, \w+, \w+, true\);")
            self.assertNotIn("pl_tex_create", created)
            self.assertIn('"reconstruction-target-unsupported: linearly sampled renderable UNORM16 RGBA"',
                          created)
        # The 10-bit export's picture is UNORM16 too: no float texture stands
        # in for it, and the float texture exists only for the other exports.
        for name, target in (("fel_renderer.h", "context.output_texture"),
                             ("base_dv_renderer.h", "base_context.output_texture")):
            source = (TOOLS / name).read_text()
            assigned = [m.start() for m in re.finditer(re.escape(target) + r" = ", source)]
            self.assertEqual(len(assigned), 2, name)
            first = source[assigned[0]:assigned[1]]
            self.assertRegex(first, re.escape(target) + r" = EXPORT_YUV10 \? dv_yuv10_source_texture\("
                                    r"gpu, EXPORT_WIDTH, EXPORT_HEIGHT, false\) : NULL;")
            self.assertIn(f"require(!EXPORT_YUV10 || {target} != NULL,", first)
            self.assertIn('"export-target-unsupported: renderable UNORM16 RGBA"', first)
            self.assertTrue(first.rstrip().endswith("if (!EXPORT_YUV10)"), name)
            self.assertNotIn("if (!" + target + ")", source)

    def test_host_copied_base_planes_are_accepted_as_p010(self):
        base = (TOOLS / "base_dv_renderer.h").read_text()
        staging = base[base.index("static void base_restage_software_planes("):]
        staging = staging[:staging.index("\n}\n")]
        p010 = staging[staging.index("if (input->format == AV_PIX_FMT_P010LE) {"):]
        p010 = p010[:p010.index("\n  }\n") + 5]
        # Already code << 6 in two planes: checked, never rewritten.
        self.assertIn("source->num_planes == 2", p010)
        self.assertIn("source->repr.bits.bit_shift == 6", p010)
        self.assertIn("PL_FMT_CAP_LINEAR", p010)
        self.assertIn("return;", p010)
        self.assertNotIn("pl_tex_upload", p010)
        self.assertNotIn("dv_shift_10bit_plane", p010)
        # Planar low-aligned input is still the only layout restaged.
        planar = staging[staging.index(p010) + len(p010):]
        self.assertIn("input->format == AV_PIX_FMT_YUV420P10LE && source->num_planes == 3", planar)
        self.assertIn("source->repr.bits.bit_shift == 0", planar)
        self.assertIn("source->repr.bits.bit_shift = 6;", planar)
        # The host copy reaches staging: both copies precede the one call.
        render = base[base.index("static void base_gpu_render("):]
        self.assertEqual(render.count("input = host = dv_host_frame(frame);"), 2)
        self.assertLess(render.rindex("input = host = dv_host_frame(frame);"),
                        render.index("base_restage_software_planes(gpu, input, &source);"))
        control = (TOOLS / "controls/test_base_host_copy.c").read_text()
        self.assertIn("dv_hw_host_copy = true;", control)
        self.assertIn("stage(AV_PIX_FMT_P010LE, 2, true);", control)
        self.assertIn("base_restage_software_planes(gpu, frame, &source);", control)

    def test_targeted_refusals_name_their_cause(self):
        target = (TOOLS / "display_target.h").read_text()
        render = target[target.index("static inline pl_tex dv_target_render("):]
        render = render[:render.index("\n}\n")]
        reason = '"display-target-l5-invalid: active area outside the reconstructed master"'
        self.assertIn(reason, render)
        # The attributable refusal precedes the shader, whose NULL stays generic.
        self.assertLess(render.index(reason), render.index("dv_dm_render(log, gpu, source, &dv_target.dm)"))
        self.assertIn("dv_dm_active_rect(&dv_target.dm, source->params.w, source->params.h, &active)", render)
        # Reasons are distinct, and the renderers refuse the same metadata first.
        reasons = re.findall(r'"(display-target-[a-z0-9-]+-invalid): ', target)
        self.assertEqual(sorted(reasons), ["display-target-dm-invalid", "display-target-l1-invalid",
                                           "display-target-l5-invalid"])
        self.assertIn('"invalid active area metadata"', (TOOLS / "fel_renderer.h").read_text())
        self.assertIn('"invalid base active area"', (TOOLS / "base_dv_renderer.h").read_text())
        # The daemon reports a refused segment with the helper's own words.
        segment = (ROOT / "crates/plurxd/src/dv_segment.rs").read_text()
        self.assertIn('"refused segment"', segment)
        self.assertIn("String::from_utf8_lossy(&", segment[segment.index('"refused segment"'):][:400])

    @unittest.skipUnless(os.name == "posix", "the native helper is POSIX only")
    def test_shifted_storage_keeps_quarter_code_taps_exact(self):
        metadata_checks.MetadataAdmission().test_finite_rgb48_rounding()


if __name__ == "__main__":
    unittest.main()
