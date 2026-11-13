"""Reference-derived gamut vertices, exact target identity and generated-data integrity."""
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

    def test_only_exact_measured_target_uses_the_table(self):
        for peak, black, prim in ((400, .005, 1), (1600, .005, 1), (4000, .005, 1),
                                  (600.01, .005, 1), (1000, 0, 1), (1000, .0049, 1),
                                  (1000, .0051, 1), (1000, .005, 0), (1000, .005, 2)):
            with self.subTest(peak=peak, black=black, prim=prim):
                flags, measured, analytic = self.vertex(peak, black, prim, (1, -1, -1))
                self.assertEqual(flags, 0)
                self.assertEqual(measured, analytic)

    def test_unmeasured_virtual_vertex_retains_analytic_mapping(self):
        for peak in (600, 1000, 2000):
            flags, measured, analytic = self.vertex(peak, .005, 1, (1, 16, -16))
            self.assertEqual(flags, 1)
            self.assertEqual(measured, analytic)

    def test_generated_header_matches_pinned_observations(self):
        subprocess.run([sys.executable, str(SOURCE / "cm29_gamut/generate_tables.py"), "--check"],
                       check=True, capture_output=True, text=True)


if __name__ == "__main__":
    unittest.main()
