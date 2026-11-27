"""Exercise the production GLSL PQ functions on the CPU against exact references.

GLSL has no log1p/expm1. Extract the legacy scalar functions (pq_*) from
dv_dm.h and map only GLSL builtins/literals to float C; this catches
cancellation returning when the shader source changes.

The CM4 transfer helpers (dfi_eq_finish_eotf, eqpq_oetf3 and the table-driven
pqt_* functions under them) read dm_pq_tables.h, so their GLSL is compiled
unchanged as C++ against a small shim (controls/pq_helper_port.py). Fused or
with fma as a*b+c they must stay within 1 ULP of the correctly rounded float32
of the exact ST 2084 functions on every sample (every float32 input:
controls/pq_helper_exhaustive.py); the two fma modes must differ, and compiled
as if `precise` were absent they must move further, which is what makes the
same comparison meaningful on a GPU.

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


class TableHelperBounds(unittest.TestCase):
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

    def test_table_helpers_are_within_one_ulp(self):
        for mode in ("fused", "unfused-fma"):
            for helper in pq_helper_port.HELPERS:
                with self.subTest(mode=mode, helper=helper):
                    self.assertGreater(len(self.samples[helper][0]), 9000)
                    self.assertLessEqual(self.wrong(mode, helper)[1], 1)

    def test_loading_the_ports_leaves_subnormals_in_this_process(self):
        # setUpClass has loaded every mode, "no-precise" (-ffast-math) included. A port linked
        # with that flag brings crtfastmath.o on GCC before 13 and turns flush-to-zero on for
        # this thread, after which "fused" is wrong on 16 samples at the black threshold.
        self.assertEqual(self.ports.keys(), set(pq_helper_port.MODES))
        smallest_normal = ctypes.c_double(2.0 ** -1022)
        self.assertEqual(smallest_normal.value / 2, 2.0 ** -1023)
        self.assertGreater(ctypes.c_double(smallest_normal.value / 2).value * 1.0, 0.0)

    def differ(self, mode, other, helper):
        xs = self.samples[helper][0]
        return pq_helper_port.compare(xs, self.ports[mode].run(helper, xs), self.ports[other].run(helper, xs))[0]

    def test_unfused_fma_is_visible(self):
        # c1_hi tau is split exactly by fma(c1_hi, tau, -c1_hi*tau); unfused its low word is lost,
        # which stays within 1 ULP but changes results, so a GPU's fusion can be told from them.
        for helper in pq_helper_port.HELPERS:
            with self.subTest(helper=helper):
                self.assertGreater(self.differ("fused", "unfused-fma", helper), 20)

    def test_table_helpers_need_precise(self):
        # Free to reassociate, a compiler cancels the error-free sums: (a+b)-a becomes b.
        for helper in pq_helper_port.HELPERS:
            with self.subTest(helper=helper):
                self.assertGreater(self.differ("fused", "no-precise", helper), len(self.samples[helper][0]) // 8)
                self.assertGreater(self.wrong("no-precise", helper)[1], 1)

    def test_table_generator_reproduces_the_table_layout(self):
        # The fitted words may differ in their low bits on another libm (a regenerated header
        # must pass pq_helper_exhaustive.py again); the layout the shader indexes may not.
        compiler = shutil.which("cc")
        if not compiler:
            raise RuntimeError("C compiler required for the PQ table generator")
        controls = ROOT / "tools/dv_processing/controls"
        work = Path(self.scratch.name)
        subprocess.run([compiler, "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-I", str(controls),
                        str(controls / "pq_table_gen.c"), "-o", str(work / "pq_table_gen"), "-lm"], check=True)
        subprocess.run([str(work / "pq_table_gen"), str(work / "tables.h")], check=True, capture_output=True)

        def layout(text):
            return (pq_helper_port.glsl_literals(text, "DM_PQT_GLSL"),
                    re.search(r"(\d+) binades", text).group(1), re.search(r"DM_PQT_WIDTH (\d+)", text).group(1))

        self.assertEqual(layout((work / "tables.h").read_text()), layout(pq_helper_port.TABLES.read_text()))

    def test_double_reference_is_inside_the_bound_the_exhaustive_check_assumes(self):
        # pq_helper_exhaustive.py decides each input from pq_reference.h unless it lies within
        # PQR_BOUND (2^-36) of a rounding midpoint; that is sound only while the reference is
        # that accurate. Keep a 16x margin: every sample, and the black, pole and white edges.
        compiler = shutil.which("cc")
        if not compiler:
            raise RuntimeError("C compiler required for the PQ reference")
        controls = ROOT / "tools/dv_processing/controls"
        work = Path(self.scratch.name)
        (work / "reference.c").write_text(
            '#include "pq_reference.h"\n'
            'double reference_eotf(double e) { return pqr_eotf(e); }\n'
            'double reference_oetf(double y) { return pqr_oetf(y); }\n'
            'double reference_bound(void) { return PQR_BOUND; }\n')
        subprocess.run([compiler, "-std=c11", "-O2", "-fPIC", "-Wall", "-Wextra", "-Werror", "-I", str(controls),
                        "-c", str(work / "reference.c"), "-o", str(work / "reference.o")], check=True)
        subprocess.run([compiler, "-shared", str(work / "reference.o"), "-o", str(work / "reference.so"), "-lm"], check=True)
        library = ctypes.CDLL(str(work / "reference.so"))
        for name in ("reference_eotf", "reference_oetf", "reference_bound"):
            getattr(library, name).restype = ctypes.c_double
        library.reference_eotf.argtypes = library.reference_oetf.argtypes = [ctypes.c_double]
        bound = library.reference_bound() / 16
        value = pq_helper_port.value
        from decimal import Decimal, getcontext
        M1, M2, C1, C2, C3 = (pq_helper_port.M1, pq_helper_port.M2, pq_helper_port.C1,
                              pq_helper_port.C2, pq_helper_port.C3)

        def eotf(x):
            getcontext().prec = 60
            if not x > 0:
                return None
            p = Decimal(x) ** (1 / M2)
            if p <= C1 or C2 - C3 * p <= 0:
                return None
            return 10000 * ((p - C1) / (C2 - C3 * p)) ** (1 / M1)

        def oetf(y):
            getcontext().prec = 60
            p = (Decimal(y) / 10000) ** M1
            return ((C1 + C2 * p) / (1 + C3 * p)) ** M2

        edges = [value(w) for w in list(range(0x35443723, 0x35443723 + 4000, 13)) + list(range(0x3ffefbae - 4000, 0x3ffefbae, 11))]
        for name, xs, exact, reference in (
                ("eotf", self.samples["eotf"][0] + edges, eotf, library.reference_eotf),
                ("oetf", [y for y in self.samples["oetf"][0] if 0 < y < 10000], oetf, library.reference_oetf)):
            worst = 0
            for x in xs:
                want = exact(x)
                if want is None or want == 0:
                    continue
                worst = max(worst, abs(Decimal(reference(x)) - want) / want)
            with self.subTest(helper=name):
                self.assertLess(worst, bound)

    def test_table_helper_edges(self):
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
