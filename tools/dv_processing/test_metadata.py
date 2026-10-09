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


if __name__ == "__main__":
    unittest.main()
