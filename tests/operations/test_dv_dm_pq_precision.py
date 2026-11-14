"""Exercise the production GLSL PQ functions on the CPU against exact references.

GLSL has no log1p/expm1. Extract the legacy scalar functions (pq_*) from
dv_dm.h and map only GLSL builtins/literals to float C; this catches
cancellation returning when the shader source changes.

The precise CM4 helpers (dfi_eq_finish_eotf, eqpq_oetf3 and the r20q_*/eq*
split-float functions under them) are three channels wide, so their GLSL is
compiled unchanged as C++ against a small vec3 shim
(controls/pq_helper_port.py). With a fused fma and no reassociation they must
return the correctly rounded float32 of the exact ST 2084 functions on every
sample; with fma as a*b+c, or compiled as if `precise` were absent, they must
not, which is what makes the same check meaningful on a GPU.

Actual shader compilation and driver behaviour remain the job of
controls/run_dm_parity.py and controls/run_pq_helper_exact.py.
"""
import ast
import ctypes
import json
import math
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "tools/dv_processing/dv_dm.h"
sys.path.insert(0, str(ROOT / "tools/dv_processing/controls"))
import pq_helper_port  # noqa: E402


def scalar_pq_source(header):
    # Read the C string literals, then find complete GLSL functions, including
    # nested blocks that span several literals.
    literals = re.findall(r'^\s*("(?:[^"\\\n]|\\.)*")', header, re.MULTILINE)
    shader = "".join(json.loads(literal) for literal in literals)
    functions = []
    for name in ("pq_log1p", "pq_expm1", "pq_eotf_ext", "pq_eotf", "pq_oetf"):
        start, = re.finditer(rf"\bfloat\s+{name}\s*\(float\s+\w+\)\s*\{{", shader)
        depth = 1
        for brace in re.finditer(r"[{}]", shader[start.end():]):
            depth += 1 if brace.group() == "{" else -1
            if depth == 0:
                functions.append(shader[start.start():start.end() + brace.end()])
                break
        else:
            raise ValueError(f"unterminated GLSL function {name}")
    # Unsuffixed GLSL floating literals use binary32; C defaults to binary64.
    decimal = r"(?<![\w.])((?:\d+\.\d*|\.\d+)(?:[eE][+-]?\d+)?|\d+[eE][+-]?\d+)(?![\w.])"
    return re.sub(decimal, r"\1f", "\n".join(functions))


class ShaderPQPrecision(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        compiler = shutil.which("cc")
        if not compiler:
            raise RuntimeError("C compiler required for shader PQ precision regression")
        glsl = scalar_pq_source(SOURCE.read_text())
        harness = (
            '#include "dm_closed_form.h"\n'
            'double reference_oetf(double y) { return dm_pq_oetf(y); }\n'
            'double reference_eotf(double e) { return dm_pq_eotf(e); }\n'
            'double reference_ext(double e) { return dm_pq_eotf_ext(e); }\n'
            '#define max fmaxf\n#define clamp(x,a,b) fminf(fmaxf(x,a),b)\n'
            '#define pow powf\n#define log logf\n#define exp expf\n' + glsl
        )
        cls.scratch = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.scratch.cleanup)
        tmp = Path(cls.scratch.name)
        src = tmp / "pq.c"
        src.write_text(harness)
        cls.implementations = {}
        for mode, flags in (("default", []), ("fast-math", ["-ffast-math"])):
            obj = tmp / (mode + ".o")
            lib = tmp / (mode + ".so")
            subprocess.run([compiler, "-std=c11", "-O2", "-fPIC", "-Wall", "-Wextra", "-Werror",
                            "-Wno-unused-function", *flags, "-I", str(SOURCE.parent),
                            "-c", str(src), "-o", str(obj)], check=True)
            # Keep fast-math initialization out of the Python process's FP environment.
            subprocess.run([compiler, "-shared", str(obj), "-o", str(lib), "-lm"], check=True)
            compiled = ctypes.CDLL(str(lib))
            cls.implementations[mode] = compiled
            for name in ("pq_log1p", "pq_expm1", "pq_oetf", "pq_eotf", "pq_eotf_ext"):
                fn = getattr(compiled, name)
                fn.argtypes = [ctypes.c_float]
                fn.restype = ctypes.c_float
        cls.lib = cls.implementations["default"]
        for name in ("reference_oetf", "reference_eotf", "reference_ext"):
            fn = getattr(cls.lib, name)
            fn.argtypes = [ctypes.c_double]
            fn.restype = ctypes.c_double

    def test_elementary_functions_resist_float32_cancellation(self):
        # On |x| <= 1/4 the Taylor terms have magnitude <= |x|^j/j.
        # At most 2j+1 binary32 roundings affect term j. Summing their
        # weighted errors gives <= 4u|x|/(1-25u), u=2^-24. The omitted
        # log tail at 21/128 and exp tail at 1/4 each use < 0.001u|x|.
        unit = 2**-24
        for name, endpoint, reference, observed in (
            ("pq_log1p", -21/128, math.log1p, -0.014437053352594376),
            ("pq_expm1", 0.25, math.expm1, 0.16448824107646942),
        ):
            arguments = [ctypes.c_float(endpoint * i / 65535).value for i in range(65536)]
            arguments += [math.ldexp(endpoint, -i) for i in range(1, 65)]
            arguments.append(observed)
            if name == "pq_expm1":
                arguments.append(0.25 - 2**-26)  # adjacent binary32 inside the series
            for mode, compiled in self.implementations.items():
                fn = getattr(compiled, name)
                with self.subTest(function=name, mode=mode):
                    for x in arguments:
                        error = abs(fn(x) - reference(x))
                        self.assertLessEqual(error, 5 * unit * abs(x), x)
                    if name == "pq_expm1":
                        # Just outside the series the exp(x)-1 fallback carries
                        # absolute rounding error on exp(x), whose scale is near one.
                        x = 0.25 + 2**-25
                        self.assertLessEqual(abs(fn(x) - reference(x)), 4 * unit * math.exp(x))

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


class PreciseHelperExactness(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.scratch.cleanup)
        cls.ports = {mode: pq_helper_port.build(cls.scratch.name, mode) for mode in pq_helper_port.MODES}
        cls.samples = {}
        for helper, (inputs, exact) in pq_helper_port.HELPERS.items():
            xs = inputs()
            cls.samples[helper] = (xs, [exact(x) for x in xs])

    def wrong(self, mode, helper):
        xs, want = self.samples[helper]
        return pq_helper_port.compare(xs, self.ports[mode].run(helper, xs), want)

    def test_precise_helpers_are_correctly_rounded(self):
        for helper in pq_helper_port.HELPERS:
            with self.subTest(helper=helper):
                self.assertGreater(len(self.samples[helper][0]), 9000)
                self.assertEqual(self.wrong("fused", helper), (0, 0, None))

    def test_precise_helpers_need_a_fused_fma(self):
        # The product's error term is fma(a,b,-a*b): unfused it is zero and the low words are lost.
        for helper in pq_helper_port.HELPERS:
            with self.subTest(helper=helper):
                count, worst, _ = self.wrong("unfused-fma", helper)
                self.assertGreater(count, len(self.samples[helper][0]) // 2)
                self.assertGreater(worst, 8)

    def test_precise_helpers_need_precise(self):
        # Free to reassociate, a compiler cancels the error-free sums: (a+b)-a becomes b.
        for helper in pq_helper_port.HELPERS:
            with self.subTest(helper=helper):
                self.assertGreater(self.wrong("no-precise", helper)[0], len(self.samples[helper][0]) // 2)

    def test_precise_helper_edges(self):
        port, bits = self.ports["fused"], pq_helper_port.bits
        inf = float("inf")
        # Black and below, the unit code, the pole of the extended EOTF and beyond.
        self.assertEqual(port.run("eotf", [0.0, -1.0, 7e-7, 1.0, 1.9921, 3.0]),
                         [0, 0, 0, bits(10000.0), bits(inf), bits(inf)])
        # No light and below, the peak and beyond.
        black = pq_helper_port.exact_oetf(0.0)
        self.assertEqual(port.run("oetf", [0.0, -5.0, 10000.0, 20000.0, inf, 10000.0]),
                         [black, black, bits(1.0), bits(1.0), bits(1.0), bits(1.0)])


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


class PreciseRealChannelTolerance(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        script = ROOT / "tools/dv_processing/controls/run_dm_parity.py"
        tree = ast.parse(script.read_text())
        node = next(n for n in tree.body if isinstance(n, ast.FunctionDef)
                    and n.name == "precise_cm4_channel_ok")
        namespace = {}
        exec(compile(ast.Module(body=[node], type_ignores=[]), str(script), "exec"), namespace)
        cls.match = staticmethod(namespace["precise_cm4_channel_ok"])

    def test_real_channel_tolerance_accepts_measured_matrix_cancellation(self):
        # Retained lavapipe failures: PQ exaggerates the matrix's tiny blue residue.
        self.assertTrue(self.match(1.005452759e-5, 8.769299029e-7,
                                   3.696186862e-9, 6.285448058e-17, .1775264471))
        self.assertTrue(self.match(2.521367423e-5, 9.673170780e-7,
                                   3.994856040e-8, 9.811133310e-16, .5388778437))
        self.assertTrue(self.match(.4, .400001, 32.447, 32.448, 32.448))

    def test_real_channel_tolerance_rejects_larger_light_errors(self):
        self.assertFalse(self.match(.01, 0, .0001, 0, .5))
        self.assertFalse(self.match(.300002, .3, 1.00003, 1, 1.00003))
        # It scales with the pixel, not the display target: a shadow gets a
        # much tighter light budget than a peak-white pixel.
        self.assertFalse(self.match(2.521367423e-5, 9.673170780e-7,
                                    3.994856040e-8, 9.811133310e-16, .001))

    def test_real_channel_tolerance_rejects_invalid_values(self):
        for bad in (-1, 1.2, math.inf, -math.inf, math.nan):
            with self.subTest(bad=bad):
                self.assertFalse(self.match(bad, 0, 0, 0, 1))
                self.assertFalse(self.match(0, bad, 0, 0, 1))
        for bad in (-1, math.inf, math.nan):
            with self.subTest(pixel_peak=bad):
                self.assertFalse(self.match(0, 0, 0, 0, bad))


if __name__ == "__main__":
    unittest.main()
