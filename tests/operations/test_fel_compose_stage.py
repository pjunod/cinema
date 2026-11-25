"""The pq-ccm001-* composition stage's shader matches the CCM 001 reference.

tools/dv_processing/fel_compose.h composes the FEL on the GPU in exact
integers; tools/dv_processing/fel_reference_composer.h is the independent
reference it must equal code for code (FEL reference composition plan,
milestone 5.3). Without a GPU, the shader's GLSL is compiled unchanged as
C++ (controls/fel_compose_cpu_port.py) and run on seeded synthetic vectors
through controls/run_fel_compose_parity.py against the reference's CLI. The
parity check must also fail when the shader drops clause 5.4.3.3's rounding
or the floor of negative residuals: a 1-code allowance would pass both.

Driver behaviour, GPU precision and descriptor binding remain the job of the
GPU control (controls/fel_compose_control.c) run by the same script.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TOOLS = ROOT / "tools/dv_processing"
sys.path.insert(0, str(TOOLS / "controls"))
import fel_compose_cpu_port  # noqa: E402

CASES = 48


@unittest.skipUnless(os.name == "posix", "the controls build with the host compilers")
class FelComposeStageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cc, cxx = os.environ.get("CC", "cc"), os.environ.get("CXX", "c++")
        if not shutil.which(cc) or not shutil.which(cxx):
            raise RuntimeError("C and C++ compilers required for the composition stage regression")
        cls.cxx = cxx
        cls.scratch = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.scratch.cleanup)
        cls.tmp = Path(cls.scratch.name)
        cls.oracle = cls.tmp / "fel_reference_compose"
        subprocess.run([cc, "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror",
                        str(TOOLS / "diagnostics/fel_quality/fel_reference_compose.c"),
                        "-o", str(cls.oracle)], check=True, timeout=120)
        cls.port = fel_compose_cpu_port.build(cls.tmp / "fel_compose_cpu_port", cxx)

    def parity(self, control, name):
        out = self.tmp / f"{name}.json"
        run = subprocess.run([sys.executable, str(TOOLS / "controls/run_fel_compose_parity.py"),
                              "--control", str(control), "--oracle", str(self.oracle),
                              "--work", str(self.tmp / f"{name}-work"), "--synthetic", str(CASES), "--out", str(out)],
                             capture_output=True, text=True, timeout=900)
        self.assertTrue(out.exists(), run.stderr[-2000:])
        return json.loads(out.read_text())

    def test_shader_matches_reference_code_for_code(self):
        result = self.parity(self.port, "stage")
        self.assertEqual(result["verdict"], "pass", result["failed"])
        cases = result["cases"]
        self.assertEqual(len(cases), CASES)
        matched = [c for c in cases if c["status"] == "match"]
        # The seeded set spans what the stage admits: both EL sizes, both
        # chroma layouts, parameters past clause 5.3.3's bounds (refused by
        # both) and pictures that are not clipped flat.
        self.assertTrue(any(c["el_width"] == c["width"] for c in matched))
        self.assertTrue(any(2 * c["el_width"] == c["width"] for c in matched))
        self.assertTrue(any(c["interleaved_chroma"] for c in matched))
        self.assertTrue(any(c["status"] == "both refused" for c in cases))
        self.assertTrue(any(p["clipped_fraction"] < 0.25 and p["distinct_codes"] > 100
                            for c in matched for p in c["planes"].values()))

    def test_parity_fails_without_output_rounding(self):
        port = fel_compose_cpu_port.build(self.tmp / "no-rounding", self.cxx,
                                          ("+ (1 << (15 - hdr));", "+ 0;"))
        self.assertEqual(self.parity(port, "no-rounding")["verdict"], "fail")

    def test_parity_fails_without_negative_residual_floor(self):
        port = fel_compose_cpu_port.build(self.tmp / "no-floor", self.cxx,
                                          ("if (rr < 0) { uint add", "if (false) { uint add"))
        self.assertEqual(self.parity(port, "no-floor")["verdict"], "fail")


if __name__ == "__main__":
    unittest.main()
