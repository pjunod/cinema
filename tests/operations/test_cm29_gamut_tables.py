"""Reference-derived gamut vertices, qualified target domain and generated-data integrity."""
import ctypes
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "tools/dv_processing"
HARNESS = r'''
#include "dm_closed_form.h"
int vertex(double peak, double black, int prim, int i, int p, int t, double out[6]) {
  double l1[3] = {0.000488400488, 0.5001221, 0.902808303};
  dm_cm29_target target = {peak, black, (enum dm_prim)prim, 0};
  dm_cm29_trim trim = {0}; dm_cm29_state s;
  if (!dm_cm29_prepare(&s, l1, 0.005, 4000, &target, &trim)) return -1;
  double q[3] = {i*s.TM/64, p*s.TM/32, t*s.TM/32};
  int flags = (s.gamut ? 1 : 0) | (dm_cm29_measured_vertex(&s,q,out) ? 2 : 0);
  dm_cm29_grid_vertex(&s,q,out);
  s.gamut = NULL; dm_cm29_grid_vertex(&s,q,out+3);
  for (int k=0;k<6;k++) out[k] *= 65535;
  return flags;
}
int table_selected(double peak, double black, int prim) {
  return dm_cm29_gamut_table(peak, black, prim) != NULL;
}
int flat_red(double peak, double black, double out[3]) {
  double l1[3] = {0.000488400488, 0.5001221, 0.902808303};
  dm_cm29_target target = {peak, black, DM_P3D65, 0};
  dm_cm29_trim trim = {0}; dm_cm29_state s; dm_cm29_pix p;
  if (!dm_cm29_prepare(&s, l1, 0.005, 4000, &target, &trim)) return 0;
  const double rgb[3] = {0.03125, 0, 0};
  dm_cm29_pixel_pre(&s, rgb, &p);
  dm_cm29_finish(&s, &p, p.Ib, p.Io, out);
  return 1;
}
'''


class MeasuredGamut(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        compiler = shutil.which("cc")
        if not compiler:
            raise RuntimeError("C compiler required for measured-gamut regression")
        target = ROOT / "target"
        target.mkdir(exist_ok=True)
        cls.temp = tempfile.TemporaryDirectory(dir=target, prefix="cm29-gamut-")
        cls.addClassCleanup(cls.temp.cleanup)
        path = Path(cls.temp.name)
        (path / "control.c").write_text(HARNESS)
        library = path / "control.so"
        subprocess.run([compiler, "-std=c11", "-O2", "-shared", "-fPIC", "-Wall", "-Wextra", "-Werror",
                        "-Wno-unused-function", "-I", str(SOURCE), str(path / "control.c"),
                        "-o", str(library), "-lm"], check=True)
        cls.lib = ctypes.CDLL(str(library))
        cls.lib.vertex.argtypes = [ctypes.c_double, ctypes.c_double, ctypes.c_int,
                                   ctypes.c_int, ctypes.c_int, ctypes.c_int,
                                   ctypes.POINTER(ctypes.c_double)]
        cls.lib.vertex.restype = ctypes.c_int
        cls.lib.table_selected.argtypes = [ctypes.c_double, ctypes.c_double, ctypes.c_int]
        cls.lib.table_selected.restype = ctypes.c_int
        cls.lib.flat_red.argtypes = [ctypes.c_double, ctypes.c_double,
                                    ctypes.POINTER(ctypes.c_double)]
        cls.lib.flat_red.restype = ctypes.c_int

    def vertex(self, peak, black, prim, coordinate):
        values = (ctypes.c_double * 6)()
        flags = self.lib.vertex(peak, black, prim, *coordinate, values)
        self.assertGreaterEqual(flags, 0)
        return flags, tuple(values[:3]), tuple(values[3:])

    def test_reference_dark_chroma_and_highlight_desaturation(self):
        # Independently solved native P3/PQ observations; acquisition hashes and
        # both random probe seeds are retained in cm29_gamut/provenance.json.
        for peak, dark, highlight in (
            (600, (0, 757, 1168), (45632, 45336, 45062)),
            (1000, (0, 816, 1257), (49271, 48937, 48622)),
            (2000, (0, 897, 1377), (54225, 53836, 53464)),
        ):
            for coordinate, expected in (((1, -1, -1), dark), ((64, 1, 1), highlight)):
                with self.subTest(peak=peak, coordinate=coordinate):
                    flags, measured, analytic = self.vertex(peak, 0.005, 1, coordinate)
                    self.assertEqual(flags, 3)
                    for actual, wanted in zip(measured, expected):
                        self.assertAlmostEqual(actual, wanted, places=7)
                    self.assertGreater(max(abs(a-b) for a, b in zip(measured, analytic)), 1)

    def test_saturated_blue_corner_is_observed_without_analytic_fallback(self):
        # Synthetic saturation-expanded probes reach this corner; neutral/dark
        # probes could not. Its missing green caused the two Smile outliers.
        flags, measured, analytic = self.vertex(600, .005, 1, (16, -5, -10))
        self.assertEqual(flags, 3)
        for actual, expected in zip(measured, (0, 4784, 20525)):
            self.assertAlmostEqual(actual, expected, places=7)
        self.assertLess(analytic[1], 1)
        self.assertGreater(measured[1] - analytic[1], 4000)

    def test_unqualified_peak_black_or_gamut_retains_analytic_mapping(self):
        for peak, black, prim in ((400, .005, 1), (1600, .005, 1), (4000, .005, 1),
                                  (600.01, .005, 1), (1000, -.0001, 1),
                                  (1000, .0051, 1), (1000, .005, 0), (1000, .005, 2)):
            with self.subTest(peak=peak, black=black, prim=prim):
                flags, measured, analytic = self.vertex(peak, black, prim, (1, -1, -1))
                self.assertEqual(flags, 0)
                self.assertEqual(measured, analytic)

    def test_nonfinite_black_does_not_select_measured_data(self):
        for black in (float("nan"), float("inf"), -float("inf")):
            with self.subTest(black=black):
                self.assertEqual(self.lib.table_selected(600, black, 1), 0)

    def test_qualified_black_retains_licensed_near_black_chroma_and_curve(self):
        # Licensed cm_offline 5.6.4 flat 11x5 tile centers: BT.2020/PQ
        # input (0.03125, 0, 0), L1 (2/4095, 0.5001221, 0.902808303),
        # source mastering 0.005/4000 nits, neutral L2. This exercises the
        # target-black curve AND native lattice, rather than only its selector.
        # Receipts and raster hashes: cm29_gamut/black-range-qualification.json.
        references = (
            (600, 0, (0.019852010533213615, 0.004362965002655983, 0.003189210081472993)),
            (600, .0001, (0.020007992163300514, 0.00440142722800374, 0.0031737233512103558)),
            (600, .001, (0.0228175800293684, 0.005104268901050091, 0.0028415576089173555)),
            (600, .005, (0.036073584109544754, 0.008884485810995102, 0.0036929622292518616)),
            (1000, 0, (0.022162573412060738, 0.004937880206853151, 0.003363579511642456)),
            (1000, .0001, (0.02230958826839924, 0.004974932409822941, 0.0033474776428192854)),
            (1000, .001, (0.02469496801495552, 0.00558320851996541, 0.003050920320674777)),
            (1000, .005, (0.036657437682151794, 0.009198719635605812, 0.003675541840493679)),
            (2000, 0, (0.025401482358574867, 0.0057698022574186325, 0.003582173027098179)),
            (2000, .0001, (0.025539647787809372, 0.005805643275380135, 0.0035653579980134964)),
            (2000, .001, (0.027484163641929626, 0.006314571481198072, 0.0033073807135224342)),
            (2000, .005, (0.03776499629020691, 0.009614568203687668, 0.0035763252526521683)),
        )
        for peak, black, reference in references:
            with self.subTest(peak=peak, black=black):
                result = (ctypes.c_double * 3)()
                self.assertEqual(self.lib.flat_red(peak, black, result), 1)
                for actual, expected in zip(result, reference):
                    self.assertLess(abs(actual - expected) * 65535, .25)
        for peak in (600, 1000, 2000):
            flags, _, _ = self.vertex(peak, .0049, 1, (1, -1, -1))
            self.assertEqual(flags, 3)

    def test_unmeasured_virtual_vertex_retains_analytic_mapping(self):
        for peak in (600, 1000, 2000):
            for black in (0, .0001, .001, .005):
                with self.subTest(peak=peak, black=black):
                    flags, measured, analytic = self.vertex(peak, black, 1, (1, 16, -16))
                    self.assertEqual(flags, 1)
                    self.assertEqual(measured, analytic)

    def test_generated_header_matches_pinned_observations(self):
        subprocess.run([sys.executable, str(SOURCE / "cm29_gamut/generate_tables.py"), "--check"],
                       check=True, capture_output=True, text=True)


if __name__ == "__main__":
    unittest.main()
