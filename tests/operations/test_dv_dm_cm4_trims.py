"""CM4 source-gamut hue knots, checked against independent reference probes.

The blue-only probe uses uniform ICtCp hue tiles, P3 mastering at 4000 nits,
1000-nit P3 output, and saturation-vector B=-0.25. Weights below come from the
ratio of normal licensed neutral/blue outputs, not the implementation formula.
Full rasters and every worsening coordinate remain in private qualification.
"""
import ctypes
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HEADER = ROOT / "tools/dv_processing/dm_closed_form.h"
HARNESS = r'''
#define DM_STATIC static inline
#include "dm_closed_form.h"
int cm4_hue(int prim, double shot_max, double target_nits, double hue, double *out) {
  double l1[3] = {0.001, 0.25, shot_max};
  dm_cm4_trim trim = {0}; dm_cm4_state s;
  trim.sat[4] = -0.25;
  if (!dm_cm4_prepare(&s, l1, dm_pq_oetf(0.005), dm_pq_oetf(target_nits), &trim,
                      DM_P3D65, (enum dm_prim)prim)) return 0;
  dm_six_weights(s.anc, hue, out);
  for (int k = 0; k < 6; k++) out[6 + k] = s.anc[k];
  return 1;
}
double cm4_ms(int count, double target_nits, double master_nits) {
  dm_cm4_trim trims[2] = {{0}, {0}};
  trims[0].ms = -0.5; trims[1].ms = 0.25;
  double peaks[2] = {dm_pq_oetf(100), dm_pq_oetf(1000)};
  enum dm_prim primaries[2] = {DM_BT709, DM_P3D65};
  dm_cm4_trim out = dm_cm4_interp_gamut(trims, peaks, primaries, count,
                    dm_pq_oetf(target_nits), dm_pq_oetf(master_nits), DM_P3D65, DM_P3D65);
  return out.ms;
}
'''


class CM4CreativeTrims(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        compiler = shutil.which("cc")
        if not compiler:
            raise RuntimeError("C compiler required for the CM4 hue regression")
        (ROOT / "target").mkdir(parents=True, exist_ok=True)
        scratch = tempfile.TemporaryDirectory(dir=ROOT / "target", prefix="cm4-hue-")
        cls.addClassCleanup(scratch.cleanup)
        root = Path(scratch.name)
        source, library = root / "harness.c", root / "libcm4.so"
        source.write_text(HARNESS)
        subprocess.run([compiler, "-std=c11", "-O1", "-shared", "-fPIC", "-Wall", "-Wextra", "-Werror",
                        "-I", str(HEADER.parent), str(source), "-o", str(library), "-lm"], check=True)
        cls.lib = ctypes.CDLL(str(library))
        cls.lib.cm4_hue.argtypes = [ctypes.c_int, ctypes.c_double, ctypes.c_double,
                                   ctypes.c_double, ctypes.POINTER(ctypes.c_double)]
        cls.lib.cm4_ms.argtypes = [ctypes.c_int, ctypes.c_double, ctypes.c_double]
        cls.lib.cm4_ms.restype = ctypes.c_double

    def hue(self, prim, shot_max, target_nits, hue):
        out = (ctypes.c_double * 12)()
        self.assertEqual(self.lib.cm4_hue(prim, shot_max, target_nits, hue, out), 1)
        return list(out)

    def test_source_gamut_weights_match_measured_reference(self):
        # Uniform I=0.35/S=0.025 P3 and I=0.35/S=0.01 BT.2020/BT.709.
        # Unchanged intensity/hue exclude target clipping. Each tuple is a
        # licensed neutral/control chroma-response ratio, not the C formula.
        measured = ((1, 4, ((289.5, 0.5670508384), (294.0, 0.6790442239),
                            (300.0, 0.8283781054), (301.5, 0.8657062196))),
                    (0, 4, ((270.0, 0.1723382961), (280.5, 0.5059421480),
                            (289.5, 0.7918001159), (294.0, 0.9347689786))),
                    (2, 0, ((84.0, 0.6588220858), (99.0, 0.9187734436),
                            (120.0, 0.7282285937), (145.5, 0.3033949171))))
        for prim, vector, samples in measured:
            for hue, weight in samples:
                with self.subTest(prim=prim, vector=vector, hue=hue):
                    self.assertAlmostEqual(self.hue(prim, 0.9, 1000, hue)[vector], weight, delta=0.0005)

    def test_source_palette_is_independent_of_shot_and_target_peaks(self):
        self.assertEqual(self.hue(1, 0.6, 600, 294), self.hue(1, 0.9, 1000, 294))

    def test_spatial_weight_matches_reference_controls(self):
        # Independently qualified sinusoidal/step controls cover interpolation
        # through authored nodes and fading toward the source mastering peak.
        for count, target, master, expected in ((1, 1000, 4000, -0.1910615977),
                                                 (2, 600, 4000, 0.0791282692),
                                                 (1, 600, 1000, -0.1139144872),
                                                 (2, 1000, 4000, 0.25),
                                                 (1, 100, 4000, -0.5),
                                                 (2, 50, 4000, -0.5)):
            with self.subTest(count=count, target=target, master=master):
                self.assertAlmostEqual(self.lib.cm4_ms(count, target, master), expected, delta=1e-9)

    def test_spatial_weight_is_neutral_without_trim_or_at_master(self):
        self.assertEqual(self.lib.cm4_ms(0, 1000, 4000), 0)
        self.assertEqual(self.lib.cm4_ms(2, 4000, 4000), 0)


if __name__ == "__main__":
    unittest.main()
