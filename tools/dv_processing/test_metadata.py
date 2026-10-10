"""Focused arithmetic admission regression; no GPU or media dependency."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class MetadataAdmission(unittest.TestCase):
    def test_nonbinding_residual_limit(self):
        source = Path(__file__).with_name("test_nlq_clipping.c")
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "nlq-limit"
            subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Wall",
                            "-Wextra", "-Werror", str(source), "-lm", "-o",
                            str(binary)], check=True, timeout=30)
            subprocess.run([str(binary)], check=True, timeout=10)

    def test_finite_rgb48_rounding(self):
        source = Path(__file__).with_name("test_rgb48.c")
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "rgb48-rounding"
            subprocess.run([os.environ.get("CC", "cc"), "-O2", "-fno-math-errno",
                            "-std=c11", "-Wall", "-Wextra", "-Werror",
                            str(source), "-lm", "-o", str(binary)],
                           check=True, timeout=30)
            subprocess.run([str(binary)], check=True, timeout=15)


if __name__ == "__main__":
    unittest.main()
