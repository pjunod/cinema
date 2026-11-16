"""GPU check that the base (Profile 5/8) reconstruction composes the master
and nothing else: true black stays code 0 and dark chroma is the composed
value. Runs the compiled test_base_master_black.c, which drives the
production base_gpu_render.

Environment:
  PLURX_BASE_MASTER_BLACK_CONTROL   a compiled test_base_master_black
                                    (build_target_controls.sh builds it); any
                                    Vulkan device, lavapipe included.

A missing control is an error, never a skip. Against the renderer as it was
before dv_master_frames the control fails: the letterbox is code 988
(0.005 nits) and the dark blue patch's red channel is 132 codes high.
"""
from pathlib import Path
import os
import subprocess
import unittest


class BaseMasterBlack(unittest.TestCase):
    def test_base_reconstruction_composes_the_master_without_a_black_remap(self):
        value = os.environ.get("PLURX_BASE_MASTER_BLACK_CONTROL")
        if not value or not Path(value).is_file():
            raise RuntimeError("set PLURX_BASE_MASTER_BLACK_CONTROL to a compiled test_base_master_black")
        result = subprocess.run([str(Path(value).resolve())], capture_output=True, text=True, timeout=120)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        lines = result.stdout.splitlines()
        self.assertIn("test_base_master_black_letterbox_is_code_zero: PASS", lines)
        self.assertTrue(any(line.startswith(
            "test_base_master_black_dark_chroma_is_the_composed_master: PASS") for line in lines), result.stdout)
        self.assertEqual(lines[-1], "test_base_master_black: PASS")


if __name__ == "__main__":
    unittest.main()
