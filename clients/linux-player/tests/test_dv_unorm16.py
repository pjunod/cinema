"""Verify the actual shader carried by the bundled mpv patch."""
import ast
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
PATCH = ROOT / "packaging/patches/mpv/0008-vo_gpu_next-round-the-final-DV-UNORM16-cache-exactly.patch"
SHADER_SHA256 = "ef44de1e8025934deaea30f4cf2fdee88765c0051eb177e5517617ddf7faecd0"


class DvUnorm16(unittest.TestCase):
    def test_exact_terminal_shader_against_rational_reference(self):
        patch = PATCH.read_text()
        start = patch.index("+static const char dv_unorm16_glsl[] =")
        rows = []
        for line in patch[start:].splitlines()[1:]:
            literal = line.removeprefix("+").strip()
            rows.append(ast.literal_eval(literal.removesuffix(";")))
            if literal.endswith(";"):
                break
        shader = "".join(rows)
        self.assertEqual(hashlib.sha256(shader.encode()).hexdigest(), SHADER_SHA256)
        with tempfile.TemporaryDirectory(prefix="plurx-unorm16-") as scratch:
            work = Path(scratch)
            (work / "exact_unorm16.glsl").write_text(shader)
            binary = work / "controls"
            compiler = shlex.split(os.environ.get("CC", "cc"))
            subprocess.run(compiler + ["-std=c11", "-O2", "-fno-fast-math", "-ffp-contract=off",
                                       "-Wall", "-Wextra", "-Werror", "-I", str(work),
                                       str(ROOT / "tests/dv_unorm16_controls.c"), "-lm", "-o", str(binary)],
                           check=True, capture_output=True, timeout=60)
            result = subprocess.run([str(binary)], check=True, capture_output=True, text=True, timeout=60)
            totals = json.loads(result.stdout)
            self.assertEqual(totals["tested"], 142606336)
            self.assertEqual(totals["mismatches"], 0)
            self.assertEqual(totals["float_product_too_high"], 16320)
            self.assertEqual(totals["float_product_too_low"], 16320)


if __name__ == "__main__":
    unittest.main()
