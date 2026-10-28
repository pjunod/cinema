"""Exercise the production GLSL PQ functions as float32 C against the double reference.

GLSL has no log1p/expm1. Extract its scalar functions from dv_dm.h and map
only GLSL builtins/literals to float C; this catches cancellation returning
when the shader source changes. Actual shader compilation and driver parity
remain the job of controls/run_dm_parity.py.
"""
import ast
import ctypes
import json
import math
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "tools/dv_processing/dv_dm.h"


class ShaderPQPrecision(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        compiler = shutil.which("cc")
        if not compiler:
            raise RuntimeError("C compiler required for shader PQ precision regression")
        functions = []
        for line in SOURCE.read_text().splitlines():
            if re.match(r'\s*"float pq_\w+\(float ', line):
                functions.append(json.loads(line.strip()))
        glsl = "".join(functions)
        # GLSL decimal constants are float32. C's unsuffixed decimals are double.
        glsl = re.sub(r"(?<![\w.])(\d+\.\d*(?:[eE][+-]?\d+)?)(?![\w.])", r"\1f", glsl)
        harness = (
            '#include "dm_closed_form.h"\n'
            'double reference_oetf(double y) { return dm_pq_oetf(y); }\n'
            'double reference_eotf(double e) { return dm_pq_eotf(e); }\n'
            'double reference_ext(double e) { return dm_pq_eotf_ext(e); }\n'
            '#define max fmaxf\n#define clamp(x,a,b) fminf(fmaxf(x,a),b)\n'
            '#define pow powf\n#define log logf\n#define exp expf\n' + glsl
        )
        cls.scratch = tempfile.TemporaryDirectory()
        src = Path(cls.scratch.name) / "pq.c"
        lib = Path(cls.scratch.name) / "pq.so"
        src.write_text(harness)
        try:
            subprocess.run([compiler, "-std=c11", "-O2", "-shared", "-fPIC", "-Wall", "-Wextra", "-Werror",
                            "-Wno-unused-function", "-I", str(SOURCE.parent), str(src), "-o", str(lib), "-lm"], check=True)
            cls.lib = ctypes.CDLL(str(lib))
            for name in ("pq_oetf", "pq_eotf", "pq_eotf_ext"):
                fn = getattr(cls.lib, name)
                fn.argtypes = [ctypes.c_float]; fn.restype = ctypes.c_float
            for name in ("reference_oetf", "reference_eotf", "reference_ext"):
                fn = getattr(cls.lib, name)
                fn.argtypes = [ctypes.c_double]; fn.restype = ctypes.c_double
        except BaseException:
            cls.scratch.cleanup()
            raise

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def test_shader_pq_oetf_preserves_float_precision(self):
        # Log-spaced luminance includes the near-zero channels of saturated
        # synthetic colours; linear samples cover the base-near-one problem.
        values = [0.0] + [10 ** (-12 + 16 * i / 4096) for i in range(4097)]
        values += [10000 * i / 4096 for i in range(4097)]
        worst = max(abs(self.lib.pq_oetf(y) - self.lib.reference_oetf(ctypes.c_float(y).value)) for y in values)
        self.assertLess(worst, 1e-6)

    def test_shader_pq_eotf_preserves_float_precision(self):
        # Compare at the same rounded input: PQ input quantisation is not
        # arithmetic error. Below 1e-4 the zero threshold dominates relative error.
        for code in [10 ** (-4 + 4 * i / 4096) for i in range(4097)]:
            code = ctypes.c_float(code).value
            expected = self.lib.reference_eotf(code)
            actual = self.lib.pq_eotf(code)
            self.assertLessEqual(abs(actual - expected), 5e-5 * expected + 1e-12, code)

    def test_shader_pq_clamps_and_extended_domain(self):
        zero = self.lib.reference_oetf(0)
        for y in (-10000, -1, 0):
            self.assertAlmostEqual(self.lib.pq_oetf(y), zero, delta=1e-11)
        for y in (10000, 20000):
            self.assertEqual(self.lib.pq_oetf(y), 1)
        for code in (-1, 0, zero * 0.5):
            self.assertEqual(self.lib.pq_eotf(code), 0)
            self.assertEqual(self.lib.pq_eotf_ext(code), 0)
        self.assertEqual(self.lib.pq_eotf(-1), self.lib.pq_eotf(0))
        self.assertEqual(self.lib.pq_eotf(2), self.lib.pq_eotf(1))
        for code in (1.01, 1.05, 1.1, 1.2):
            code = ctypes.c_float(code).value
            expected = self.lib.reference_ext(code)
            actual = self.lib.pq_eotf_ext(code)
            self.assertTrue(math.isfinite(actual))
            self.assertAlmostEqual(actual / expected, 1, delta=1e-4)


class SyntheticChannelTolerance(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Extract the production scalar/array predicate without executing the
        # controller CLI or needing NumPy, a GPU or private media.
        script = ROOT / "tools/dv_processing/controls/run_dm_parity.py"
        tree = ast.parse(script.read_text())
        node = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "synthetic_channel_ok")
        namespace = {}
        exec(compile(ast.Module(body=[node], type_ignores=[]), str(script), "exec"), namespace)
        cls.match = staticmethod(namespace["synthetic_channel_ok"])

    def test_synthetic_channel_tolerance_bounds_near_zero_light_error(self):
        self.assertTrue(self.match(0.06922, 0, 0.128, 0, 2000))
        self.assertFalse(self.match(0.06922, 0, 0.128, 0, 100))
        self.assertFalse(self.match(0.06922, 0, 0.2, 0, 2000))
        # Retain original well-conditioned PQ acceptance; a blanket linear
        # bound would add an unrelated new restriction on bright channels.
        self.assertTrue(self.match(0.42515, 0.42218, 40, 38.667, 2000))

    def test_synthetic_channel_tolerance_rejects_invalid_outputs(self):
        for bad in (-1, 1.2, math.inf, -math.inf, math.nan):
            with self.subTest(bad=bad):
                # Clipped EOTF/dE can hide invalid values. The predicate must
                # reject them even when the supplied light difference is zero.
                self.assertFalse(self.match(bad, 0, 0, 0, 2000))
                self.assertFalse(self.match(0, bad, 0, 0, 2000))


if __name__ == "__main__":
    unittest.main()
