"""Compile the double-precision display-management reference and check CM v2.9 black behavior."""
import ctypes
import math
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
/* out[0] = input IPT chroma, out[1] = output IPT chroma, out[2] = Ib */
int cm29_chroma(double peak, double black, int prim, const double *rgb, double *out) {
  dm_cm29_state s; if (!prep(&s, peak, black, prim)) return 0;
  dm_cm29_pix p, q; double o[3];
  dm_cm29_pixel_pre(&s, rgb, &p); dm_cm29_finish(&s, &p, p.Ib, p.Io, o); dm_cm29_pixel_pre(&s, o, &q);
  out[0] = p.C; out[1] = q.C; out[2] = p.Ib; out[3] = 0; return 1;
}
/* the output colour (BT.2020/PQ) of rgb at the target */
int cm29_out(double peak, double black, int prim, const double *rgb, double *out3) {
  dm_cm29_state s; if (!prep(&s, peak, black, prim)) return 0;
  dm_cm29_pix p; dm_cm29_pixel_pre(&s, rgb, &p); dm_cm29_finish(&s, &p, p.Ib, p.Io, out3); return 1;
}
int cm29_blackplane(double peak, double black, int prim, double io, const double *rgb, double *out3) {
  dm_cm29_state s; if (!prep(&s, peak, black, prim)) return 0;
  dm_cm29_pix p; dm_cm29_pixel_pre(&s, rgb, &p); dm_cm29_finish(&s, &p, p.Ib, io, out3); return 1;
}
'''

PRIM = {BT2020: 0, P3: 1, BT709: 2}


class CM29BlackBehavior(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        compiler = shutil.which("cc")
        if not compiler:
            raise RuntimeError("C compiler required for the closed-form display-management regression")
        (ROOT / "target").mkdir(parents=True, exist_ok=True)
        cls.tmp = tempfile.TemporaryDirectory(dir=ROOT / "target", prefix="dv-dm-")
        cls.addClassCleanup(cls.tmp.cleanup)
        cls.build_dir = Path(cls.tmp.name)
        src = cls.build_dir / "harness.c"; lib = cls.build_dir / "libdm.so"
        src.write_text(HARNESS)
        subprocess.run([compiler, "-std=c11", "-O1", "-shared", "-fPIC", "-Wall", "-Wextra", "-Werror", "-Wno-unused-function",
                        "-I", str(HEADER.parent), str(src), "-o", str(lib), "-lm"], check=True)
        cls.lib = ctypes.CDLL(str(lib))
        dp = ctypes.POINTER(ctypes.c_double)
        cls.lib.cm29_chroma.argtypes = [ctypes.c_double, ctypes.c_double, ctypes.c_int, dp, dp]
        cls.lib.cm29_out.argtypes = [ctypes.c_double, ctypes.c_double, ctypes.c_int, dp, dp]
        cls.lib.cm29_blackplane.argtypes = [ctypes.c_double, ctypes.c_double, ctypes.c_int,
                                            ctypes.c_double, dp, dp]

    def chroma(self, peak, black, prim, rgb):
        a = (ctypes.c_double * 3)(*rgb); o = (ctypes.c_double * 4)()
        self.assertTrue(self.lib.cm29_chroma(peak, black, PRIM[prim], a, o))
        return o[0], o[1], o[2], o[3]

    def blackplane(self, peak, black, prim, io, rgb):
        a = (ctypes.c_double * 3)(*rgb); o = (ctypes.c_double * 3)()
        self.assertTrue(self.lib.cm29_blackplane(peak, black, PRIM[prim], io, a, o))
        return list(o)

    @staticmethod
    def pq_oetf(n):
        m1, m2, c1, c2, c3 = 0.1593017578125, 78.84375, 0.8359375, 18.8515625, 18.6875
        y = (n / 1e4) ** m1
        return ((c1 + c2 * y) / (1 + c3 * y)) ** m2

    def test_black_plane_is_neutral_independent_of_source_chroma(self):
        probes = ((0.018, 0, 0), (0, 0.018, 0), (0, 0, 0.018), (0.015, 0.008, 0))
        targets = ((600, 0.005, P3, 0.0), (2000, 0.005, P3, 0.0),
                   (1000, 0.005, BT2020, 0.0), (100, 0.005, BT709, 0.005),
                   (100, 0.0005, BT709, 0.005), (100, 0.0001, BT709, 0.005))
        for peak, black, prim, grid_nits in targets:
            io = self.pq_oetf(grid_nits) if grid_nits > 0 else 0.0
            expected = self.pq_oetf(black) if prim == BT709 else io
            outputs = [self.blackplane(peak, black, prim, io, rgb) for rgb in probes]
            for rgb, out in zip(probes, outputs):
                self.assertTrue(all(abs(v - out[1]) < 2e-6 for v in out), (peak, prim, rgb, out))
                self.assertTrue(all(abs(v - expected) < 2e-6 for v in out), (peak, black, prim, rgb, out))
            for out in outputs[1:]:
                self.assertLess(max(abs(a - b) for a, b in zip(outputs[0], out)), 2e-6,
                                (peak, prim, outputs[0], out))
        for peak, black, prim in ((600, 0.005, P3), (2000, 0.005, P3),
                                  (1000, 0.005, BT2020), (100, 0.005, BT709)):
            source = (0.50, 0.30, 0.25)
            out = self.out(peak, black, prim, source)
            _, cout, _, _ = self.chroma(peak, black, prim, source)
            self.assertTrue(all(math.isfinite(v) and 0 <= v <= self.pq_oetf(peak) for v in out), (peak, prim, out))
            self.assertGreater(cout, 0, (peak, prim, out))

    def out(self, peak, black, prim, rgb):
        a = (ctypes.c_double * 3)(*rgb); o = (ctypes.c_double * 3)()
        self.assertTrue(self.lib.cm29_out(peak, black, PRIM[prim], a, o))
        return list(o)

    def test_cm29_pq_targets_clip_at_zero_not_at_the_black(self):
        # the cube clip's floor is 0 for PQ targets (their black shapes the tone curve only) and the
        # display black for the BT.709 SDR target.
        def lin(pq):
            m1, m2, c1, c2, c3 = 0.1593017578125, 78.84375, 0.8359375, 18.8515625, 18.6875
            q = min(max(pq, 0.0), 1.0) ** (1 / m2)
            return 1e4 * (max(q - c1, 0) / (c2 - c3 * q)) ** (1 / m1)
        to709 = ((1.6605, -0.5876, -0.0728), (-0.1246, 1.1329, -0.0083), (-0.0182, -0.1006, 1.1187))
        dark = (0.030, 0.0, 0.0)
        o = [lin(v) for v in self.out(600, 0.005, P3, dark)]
        self.assertLess(min(o), 0.004, o)              # below the 0.005-nit black: not clipped up to it
        o = [lin(v) for v in self.out(100, 0.005, BT709, dark)]
        t = [sum(m * v for m, v in zip(row, o)) for row in to709]
        self.assertGreaterEqual(min(t), 0.005 * 0.99, t)


if __name__ == "__main__":
    unittest.main()
