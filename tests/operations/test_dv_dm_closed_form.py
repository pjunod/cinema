"""Compile the helper's double-precision display-management reference (dm_closed_form.h) and check
the CM v2.9 near-black chroma rule measured against the reference renderer.

Near black the CM v2.9 chroma scale (Ib/I)^0.9385 exp(-0.3246(Ib-I)) grew without bound as I -> 0:
letterbox noise came out 2-13x more saturated than the source while the reference keeps it neutral.
The rule caps the intensity ratio at 2 and multiplies the scale by a measured gain D(Ib - black).
No GPU, media or vendor code is involved.
"""
import ctypes
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HEADER = ROOT / "tools/dv_processing/dm_closed_form.h"
P3, BT2020, BT709 = "DM_P3D65", "DM_BT2020", "DM_BT709"

HARNESS = r'''
#include "dm_closed_form.h"
static int prim_of(int p) { return p == 0 ? DM_BT2020 : (p == 1 ? DM_P3D65 : DM_BT709); }
static int prep(dm_cm29_state *s, double peak, double black, int prim) {
  const double l1[3] = {80 / 4095.0, 1528 / 4095.0, 3728 / 4095.0};
  dm_cm29_target tg = {peak, black, (enum dm_prim)prim_of(prim), 0.0};
  dm_cm29_trim tr = {0, 0, 0, 0, 0, 0};
  return dm_cm29_prepare(s, l1, 0.005, 4000, &tg, &tr);
}
/* out[0] = input IPT chroma, out[1] = output IPT chroma, out[2] = Ib, out[3] = target black offset */
int cm29_chroma(double peak, double black, int prim, const double *rgb, double *out) {
  dm_cm29_state s; if (!prep(&s, peak, black, prim)) return 0;
  dm_cm29_pix p, q; double o[3];
  dm_cm29_pixel_pre(&s, rgb, &p); dm_cm29_finish(&s, &p, p.Ib, p.Io, o); dm_cm29_pixel_pre(&s, o, &q);
  out[0] = p.C; out[1] = q.C; out[2] = p.Ib; out[3] = s.dark_off; return 1;
}
int cm29_dark(double peak, double black, int prim, double *out23) {
  dm_cm29_state s; if (!prep(&s, peak, black, prim)) return 0;
  for (int i = 0; i < DM_CM29_DARK_N; i++) out23[i] = s.dark[i];
  return DM_CM29_DARK_N;
}
'''

PRIM = {BT2020: 0, P3: 1, BT709: 2}


class CM29NearBlackChroma(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        compiler = shutil.which("cc")
        if not compiler:
            raise RuntimeError("C compiler required for the closed-form display-management regression")
        cls.tmp = tempfile.TemporaryDirectory()
        src = Path(cls.tmp.name) / "harness.c"; lib = Path(cls.tmp.name) / "libdm.so"
        src.write_text(HARNESS)
        subprocess.run([compiler, "-std=c11", "-O1", "-shared", "-fPIC", "-Wall", "-Wextra", "-Werror", "-Wno-unused-function",
                        "-I", str(HEADER.parent), str(src), "-o", str(lib), "-lm"], check=True)
        cls.lib = ctypes.CDLL(str(lib))
        dp = ctypes.POINTER(ctypes.c_double)
        cls.lib.cm29_chroma.argtypes = [ctypes.c_double, ctypes.c_double, ctypes.c_int, dp, dp]
        cls.lib.cm29_dark.argtypes = [ctypes.c_double, ctypes.c_double, ctypes.c_int, dp]

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def chroma(self, peak, black, prim, rgb):
        a = (ctypes.c_double * 3)(*rgb); o = (ctypes.c_double * 4)()
        self.assertTrue(self.lib.cm29_chroma(peak, black, PRIM[prim], a, o))
        return o[0], o[1], o[2], o[3]

    def dark(self, peak, black, prim):
        o = (ctypes.c_double * 23)()
        self.assertEqual(self.lib.cm29_dark(peak, black, PRIM[prim], o), 23)
        return list(o)

    def test_cm29_near_black_chroma_is_neutral(self):
        # saturated colours below the shot's crush (L1 min 80/4095): at most 2 x max D of chroma
        # gain, and none where they land on the target black; mid-tones keep their chroma
        noise = [(0.018, 0, 0), (0, 0.018, 0), (0, 0, 0.018), (0.015, 0.008, 0)]
        for peak, black, prim in ((600, 0.005, P3), (2000, 0.005, P3), (1000, 0.005, BT2020), (100, 0.005, BT709), (1000, 0.0, P3)):
            dmax = max(self.dark(peak, black, prim))
            for rgb in noise:
                cin, cout, ib, off = self.chroma(peak, black, prim, rgb)
                ratio = cout / cin
                self.assertLessEqual(ratio, 2 * dmax + 0.02, (peak, prim, rgb))
                if ib <= off + 1e-9:
                    self.assertLess(ratio, 0.02, (peak, prim, rgb))
            cin, cout, _, _ = self.chroma(peak, black, prim, (0.50, 0.30, 0.25))
            self.assertTrue(0.3 < cout / cin < 3, (peak, prim))

    def test_cm29_near_black_gain_is_linear_in_target_pq_and_held_outside(self):
        def pq(n):
            m1, m2, c1, c2, c3 = 0.1593017578125, 78.84375, 0.8359375, 18.8515625, 18.6875
            y = (n / 1e4) ** m1
            return ((c1 + c2 * y) / (1 + c3 * y)) ** m2
        r600, r1000, r2000 = self.dark(600, 0, P3), self.dark(1000, 0, P3), self.dark(2000, 0, P3)
        self.assertEqual(self.dark(400, 0, P3), r600)          # held below the lowest stock target
        self.assertEqual(self.dark(4000, 0, P3), r2000)        # and above the highest
        t = (pq(1600) - pq(1000)) / (pq(2000) - pq(1000))
        for got, a, b in zip(self.dark(1600, 0, P3), r1000, r2000):
            self.assertAlmostEqual(got, (1 - t) * a + t * b, places=12)
        for row in (r600, r1000, r2000, self.dark(100, 0.005, BT709), self.dark(1000, 0, BT2020)):
            self.assertEqual(row[0], 0.0)
            self.assertEqual(row[-1], 1.0)


if __name__ == "__main__":
    unittest.main()
