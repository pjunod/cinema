"""Focused arithmetic admission regression; no GPU or media dependency."""
import os
from pathlib import Path
import subprocess
import shlex
import tempfile
import unittest


class MetadataAdmission(unittest.TestCase):
    def test_title_mastering_constants_survive_missing_blocks(self):
        """Run the production accumulator, including nonadjacent conflicts."""
        source = Path(__file__).with_name("test_title_constants.c")
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "title-constants"
            subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Wall",
                            "-Wextra", "-Werror", str(source), "-o", str(binary)],
                           check=True, timeout=30)
            subprocess.run([str(binary)], check=True, timeout=10)

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
            subprocess.run([os.environ.get("CC", "cc"), "-O2", "-fno-math-errno", "-frounding-math",
                            "-std=c11", "-Wall", "-Wextra", "-Werror",
                            str(source), "-lm", "-o", str(binary)],
                           check=True, timeout=30)
            subprocess.run([str(binary)], check=True, timeout=15)

    def test_public_piecewise_fel_mapping(self):
        """Pinned public headers/libraries; CPU-only mapping and refusal grid."""
        source = Path(__file__).with_name("test_decoded_metadata.c")
        flags = shlex.split(subprocess.check_output(
            ["pkg-config", "--static", "--cflags", "--libs", "libavformat",
             "libavcodec", "libavutil", "libplacebo"], text=True, timeout=15))
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "decoded-metadata"
            subprocess.run([os.environ.get("CC", "cc"), "-O2", "-std=c11",
                            "-Wall", "-Wextra", "-Werror", str(source),
                            "-o", str(binary), *flags, "-lm", "-lpthread", "-ldl"],
                           check=True, timeout=120)
            subprocess.run([str(binary)], check=True, timeout=15)


if __name__ == "__main__":
    unittest.main()
