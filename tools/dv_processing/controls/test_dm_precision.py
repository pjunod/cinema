"""GPU-versus-double parity for the precise CM4 transfer helpers in dv_dm.h.

Run with PLURX_DM_SHADER_CONTROL naming a compiled dm_shader_control (any
Vulkan device, lavapipe included) and PLURX_DM_PARITY_SRC naming at least five
3840x2160 float32 BT.2020/PQ RGB frames. The CM4 real-crop cases must agree with
the double-precision reference within 1.5e-6 PQ; the legacy float expansion of
the ST 2084 transfer functions misses that by a factor of two or more.
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
            "cm4_l5_col_1000_p1", "cm4_l5_col_100_p2")


class PreciseCm4Parity(unittest.TestCase):
    def test_cm4_real_crops_within_a_tenth_of_a_code(self):
        control = Path(os.environ["PLURX_DM_SHADER_CONTROL"])
        source = Path(os.environ["PLURX_DM_PARITY_SRC"])
        self.assertTrue(control.is_file() and source.is_file())
        with tempfile.TemporaryDirectory(dir=os.environ.get("PLURX_DM_PARITY_SCRATCH")) as work:
            result = subprocess.run([sys.executable, str(HERE / "run_dm_parity.py"), str(control), work,
                                     str(source), *CM4_REAL], capture_output=True, text=True, timeout=900)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(result.stdout.rstrip().endswith("PASS"))
        self.assertEqual(result.stdout.count("max|GPU-CPU|"), len(CM4_REAL))


if __name__ == "__main__":
    unittest.main()
