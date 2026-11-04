"""CPU-only raw-RPU parser regression, with explicit pinned native prerequisites.

PLURX_DV_RAW_RPU_CONTROLS points at the existing locked generator's output
(valid, binding-max, invalid-area and nonzero-eotf folders). PLURX_DOVI_PREFIX
contains pinned include/libdovi/rpu_parser.h and lib/libdovi.a. PKG_CONFIG_PATH
must select the intended libavutil and API387 libplacebo. No GPU or implicit skip.
"""
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


class RawRpuMapping(unittest.TestCase):
    def test_authored_raw_rpu_maps_api387_units_and_preserves_admission(self):
        prefix = Path(os.environ["PLURX_DOVI_PREFIX"]).resolve(strict=True)
        controls = Path(os.environ["PLURX_DV_RAW_RPU_CONTROLS"]).resolve(strict=True)
        flags = shlex.split(subprocess.check_output(
            ["pkg-config", "--cflags", "--libs", "libavutil", "libplacebo"],
            text=True, timeout=15))
        source = Path(__file__).with_suffix(".c")
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "raw-rpu"
            # Including the production renderer gives this CPU test its exact
            # static mapper; unused GPU entry points are discarded by -O2.
            subprocess.run([os.environ.get("CC", "cc"), "-O2", "-std=c11",
                            "-Wall", "-Wextra", "-Werror", "-Wno-unused-function",
                            *shlex.split(os.environ.get("CFLAGS", "")),
                            "-I" + str(prefix / "include"), str(source),
                            str(prefix / "lib/libdovi.a"), "-o", str(binary),
                            *flags, "-lm", "-lpthread", "-ldl"], check=True, timeout=120)
            for frame in range(6):
                with self.subTest(frame=frame):
                    result = subprocess.run([str(binary), str(
                        controls / f"valid/rpus/p7-frame{frame}.nal")],
                        capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn("raw-RPU API387 manual projection passed", result.stdout)
            for case, reason in [
                    ("binding-max", "unsupported bounded vdr_in_max residual clipping"),
                    ("invalid-area", "invalid active area metadata"),
                    ("nonzero-eotf", "unsupported EOTF parameters")]:
                with self.subTest(refusal=case):
                    result = subprocess.run([str(binary), str(
                        controls / case / "rpus/p7-frame0.nal")],
                        capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertIn(reason, result.stderr)
                    self.assertNotIn("manual projection passed", result.stdout)


if __name__ == "__main__":
    unittest.main()
