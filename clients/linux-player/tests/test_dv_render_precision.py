"""Executable CPU controls over the code shipped in the mpv patch series.

The complete added headers/functions are extracted, not rewritten as a second
implementation. The C harness mocks only libplacebo/TA/frame-owner boundaries.
Native compilation and GPU qualification remain separate required evidence.
"""
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
PATCHES = ROOT / "packaging/patches/mpv"
METADATA = "0006-vo_gpu_next-normalize-DV-reconstruction-metadata.patch"
UPLOAD = "0007-vo_gpu_next-preserve-DV-software-sampling-precision.patch"


def added_file(patch, path):
    sections = patch.split("diff --git ")
    selected = [part for part in sections if part.startswith(f"a/{path} b/{path}\n")]
    if len(selected) != 1 or "--- /dev/null\n" not in selected[0]:
        raise AssertionError(f"expected one complete new production file: {path}")
    lines = selected[0].splitlines(keepends=True)
    return "".join(line[1:] for line in lines if line.startswith("+") and not line.startswith("+++"))


def added_function(patch, name):
    added = "".join(line[1:] for line in patch.splitlines(keepends=True)
                    if line.startswith("+") and not line.startswith("+++"))
    return function_body(added, name)


def function_body(text, name):
    match = re.search(r"static (?:void|bool) " + re.escape(name) + r"\([^;]+?\n\{", text)
    if not match:
        raise AssertionError(f"complete added production function missing: {name}")
    # These small functions have no braces in literals/comments; reject any
    # future such change rather than silently extracting a truncated body.
    start = match.start()
    depth = 1
    i = match.end()
    while depth:
        if i == len(text):
            raise AssertionError(f"unterminated production function: {name}")
        depth += (text[i] == "{") - (text[i] == "}")
        i += 1
    result = text[start:i] + "\n"
    if "PATCH GAP" in result:
        raise AssertionError(f"patch context omits production function body: {name}")
    return result


class DvRenderPrecision(unittest.TestCase):
    def test_production_upload_lifetime_and_required_el_cleanup(self):
        upload = (PATCHES / UPLOAD).read_text()
        metadata = (PATCHES / METADATA).read_text()
        compiler = shlex.split(os.environ.get("CC", "cc"))
        self.assertTrue(compiler and shutil.which(compiler[0]), "C compiler required; no silent skip")
        with tempfile.TemporaryDirectory(prefix="plurx-dv-render-") as directory:
            temp = Path(directory)
            for name in ("libplacebo/utils/upload.h", "libplacebo/colorspace.h", "ta/ta.h"):
                stub = temp / name
                stub.parent.mkdir(parents=True, exist_ok=True)
                stub.write_text("/* Types and boundary mocks supplied by the control harness. */\n")
            (temp / "dv_upload.h").write_text(added_file(upload, "video/out/placebo/dv_upload.h"))
            (temp / "dv_reconstruction.h").write_text(added_file(metadata, "video/out/placebo/dv_reconstruction.h"))
            (temp / "frame_cleanup.h").write_text("\n".join(
                added_function(upload, name) for name in
                ("free_frame_resources", "frame_requires_el", "allow_missing_el")))
            # The wider context in the production patch carries this whole
            # uploader. Fail closed if future patch context truncates it.
            right = "".join(line[1:] if line.startswith((" ", "+")) and
                            not line.startswith("+++") else "/* PATCH GAP */\n"
                            if line.startswith("@@") else ""
                            for line in upload.splitlines(keepends=True))
            (temp / "software_upload.h").write_text(
                added_function(upload, "frame_needs_precise_sw10") +
                function_body(right, "upload_planes_sw"))
            binary = temp / "controls"
            command = compiler + ["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror",
                                  "-I", str(temp), str(ROOT / "tests/dv_render_controls.c"),
                                  "-lm", "-o", str(binary)]
            built = subprocess.run(command, capture_output=True, text=True, timeout=60)
            self.assertEqual(built.returncode, 0, built.stdout + built.stderr)
            ran = subprocess.run([str(binary)], capture_output=True, text=True, timeout=30)
            self.assertEqual(ran.returncode, 0, ran.stdout + ran.stderr)
            self.assertIn("PASS actual production", ran.stdout)


if __name__ == "__main__":
    unittest.main()
