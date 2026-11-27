"""GPU checks of the precise CM4 transfer helpers in dv_dm.h.

Environment:
  PLURX_DM_SHADER_CONTROL   a compiled dm_shader_control (any Vulkan device, lavapipe included)
  PLURX_DM_PARITY_SRC       at least five 3840x2160 float32 BT.2020/PQ RGB frames
  PLURX_DM_PARITY_SCRATCH   optional directory for the rasters; default: beside the control.
                            Never the system temporary directory: a run writes about 100 MB.
  PLURX_DM_REQUIRE_FUSED_FMA=1  optional: refuse a device whose fma is not fused.

test_cm4_real_crops_within_a_tenth_of_a_code keeps its historical regression ID.
Each real channel must agree within 1.5e-6 PQ OR four binary32 epsilons of that
pixel's peak in linear light, with max dE_ITP below 0.05. Matrix cancellation
before PQ can leave tiny light errors at a zero channel that exceed the PQ
bound: the retained 600-nit crop differs by 2.42e-5 PQ but only 3.99e-8 nits.
An instrumented pre-PQ readback reproduces that light residue with every RGB
bit unchanged. This content-scaled acceptance does not weaken helper exactness.

test_helpers_match_their_cpu_evaluation is the check that can: every helper result must be the
header's own CPU evaluation (run_pq_helper_exact.py: FUSED), which is within 1 ULP of correctly
rounded on every float32 input (pq_helper_exhaustive.py), and the two deliberately broken headers
must not be. On a device that does not fuse fma the results must instead be bit-identical to the
header evaluated with fma as a*b+c (UNFUSED), also within 1 ULP everywhere, and the check still
fails without `precise`.
"""
from pathlib import Path
import os
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
CM4_REAL = ("cm4_real_300_p1", "cm4_real_100_p1", "cm4_real_1000_p1", "cm4_real_600_p0",
            "cm4_synreal_400_p1", "cm4_synreal_1000_p1", "cm4_synreal_2000_p0", "cm4_synreal_100_p2",
            "cm4_l5_real_1000_p1", "cm4_l5_real_100_p2", "cm4_l5_row_1000_p1", "cm4_l5_row_100_p2",
            "cm4_l5_col_1000_p1", "cm4_l5_col_100_p2", "cm4_black_1000_p1")
# Every case is its own process and compiles its shaders again: about 4 s a case on a two-core
# lavapipe, tens of seconds on a driver's first use. Two minutes a case leaves room for both.
CASE_TIMEOUT = 120
FUSED, UNFUSED = 0, 3   # run_pq_helper_exact.py exit statuses


def required_file(name, what):
    value = os.environ.get(name)
    if not value:
        raise RuntimeError("set %s to %s" % (name, what))
    path = Path(value)
    if not path.is_file():
        raise RuntimeError("%s=%s is not a file; set it to %s" % (name, value, what))
    return path.resolve()


class PreciseCm4(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.control = required_file("PLURX_DM_SHADER_CONTROL", "a compiled dm_shader_control")
        root = os.environ.get("PLURX_DM_PARITY_SCRATCH") or str(cls.control.parent)
        if not os.path.isdir(root):
            raise RuntimeError("scratch directory %s does not exist (PLURX_DM_PARITY_SCRATCH)" % root)
        scratch = tempfile.TemporaryDirectory(dir=root, prefix="dm-precision-")
        cls.addClassCleanup(scratch.cleanup)
        cls.work = scratch.name

    def test_cm4_real_crops_within_a_tenth_of_a_code(self):
        source = required_file("PLURX_DM_PARITY_SRC", "five 3840x2160 float32 BT.2020/PQ RGB frames")
        result = subprocess.run([sys.executable, str(HERE / "run_dm_parity.py"), str(self.control), self.work,
                                 str(source), *CM4_REAL], capture_output=True, text=True,
                                timeout=CASE_TIMEOUT * len(CM4_REAL))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(result.stdout.rstrip().endswith("PASS"))
        self.assertEqual(result.stdout.count("max|GPU-CPU|"), len(CM4_REAL))

    def test_cm4_l8_spatial_weight_matches_licensed_controls(self):
        source = required_file("PLURX_DM_PARITY_SRC", "five 3840x2160 float32 BT.2020/PQ RGB frames")
        cases = ("cm4_ms_exact", "cm4_ms_fade", "cm4_ms_mix", "cm4_ms_off", "cm4_ms_source1000", "cm4_ms_below_first")
        result = subprocess.run([sys.executable, str(HERE / "run_dm_parity.py"), str(self.control), self.work,
                                 str(source), *cases], capture_output=True, text=True,
                                timeout=CASE_TIMEOUT * len(cases))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(result.stdout.rstrip().endswith("PASS"))
        self.assertEqual(result.stdout.count("max|GPU-CPU|"), len(cases))

    def helper_check(self, *mutation):
        result = subprocess.run([sys.executable, str(HERE / "run_pq_helper_exact.py"), str(self.control),
                                 self.work, *mutation], capture_output=True, text=True, timeout=2 * CASE_TIMEOUT)
        return result.returncode, result.stdout + result.stderr

    def test_helpers_match_their_cpu_evaluation(self):
        status, report = self.helper_check()
        self.assertIn(status, (FUSED, UNFUSED), report)
        without_precise, broken_report = self.helper_check("no-precise")
        self.assertNotIn(without_precise, (FUSED, UNFUSED), "the check passes without `precise`:\n" + broken_report)
        unfused, unfused_report = self.helper_check("unfused-fma")
        # Whether or not the device fuses, the mutated header is exactly the unfused evaluation.
        self.assertEqual(unfused, UNFUSED, unfused_report)
        if status == FUSED:
            return
        if os.environ.get("PLURX_DM_REQUIRE_FUSED_FMA") == "1":
            self.fail("this device does not fuse fma:\n" + report)
        print("\nthis device does not fuse fma: helpers bit-identical to the header with fma as a*b+c, "
              "within 1 ULP of correctly rounded\n" + report, file=sys.stderr)

if __name__ == "__main__":
    unittest.main()
