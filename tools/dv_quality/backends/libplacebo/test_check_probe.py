"""Reject corrupt renderer artifacts and writes outside a new scratch run."""
import json
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import unittest

import check_probe
import identity_reference

BUNDLE = Path(__file__).resolve().parent


def make_fixture(root):
    """Checker-only arithmetic data; this does not masquerade as a GPU run."""
    references = root / "references"
    identity_reference.generate(references)
    outputs = root / "outputs"
    outputs.mkdir()
    for name in ("source-definition.json", "reference-method.json"):
        shutil.copyfile(references / name, root / name)
    for name in ("nonzero", "base", "shifted"):
        filename = f"{name}-reference.rgb48le"
        shutil.copyfile(references / filename, outputs / filename)
    frames = []
    for case in check_probe.CASES:
        for stage in ("bl", "el", *check_probe.STAGES):
            rgba = []
            rgb = bytearray()
            for y in range(16):
                for x in range(16):
                    for component in range(3):
                        if stage == "bl":
                            value = check_probe.expected("zero", x, y, component)
                        elif stage == "el":
                            sample_x = (x + 1) % 16 if case == "shifted" else x
                            value = (0.5 if case == "zero" else
                                     0.625 if (sample_x + y + component) % 2 else 0.375)
                        else:
                            value = check_probe.expected(case, x, y, component)
                        rgba.append(value)
                        rgb.extend(struct.pack("<H", round(value * 65535)))
                    rgba.append(1.0)
            (outputs / f"{case}-{stage}.rgba32f").write_bytes(
                struct.pack("<" + "f" * len(rgba), *rgba))
            (outputs / f"{case}-{stage}.rgb48le").write_bytes(rgb)
        frames.append({"kind": "frame", "case": case, "pts": "0/1",
                       "duration": "1/24", "el_bound": case != "omitted",
                       "nlq_active": case != "disabled", "rpu_parsed": False,
                       "direct_dispatch_ok": True, "render_ok": True,
                       "render_errors": 0, "qualified_fel": False})
    (root / "probe.jsonl").write_text("".join(
        json.dumps(row, allow_nan=False) + "\n" for row in frames))
    # Hash inputs are deliberately inert fixtures. Unit tests do not build or
    # run a renderer, and only the live replay may claim renderer execution.
    (root / "fel_export_probe.c").write_text("checker-test source\n")
    (root / "fel_export_probe").write_bytes(b"checker-test binary")
    library = root / "prefix/lib/aarch64-linux-gnu/libplacebo.so.374"
    library.parent.mkdir(parents=True)
    library.write_bytes(b"checker-test library")


class CheckerArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        make_fixture(self.root)

    def overwrite_float(self, case, stage, value, index=0):
        path = self.root / "outputs" / f"{case}-{stage}.rgba32f"
        data = bytearray(path.read_bytes())
        struct.pack_into("<f", data, index * 4, value)
        path.write_bytes(data)

    def test_valid_control_fixture_passes(self):
        receipt = check_probe.check(self.root)
        self.assertEqual(len(receipt["results"]), 10)
        self.assertFalse(receipt["production_qualified"])
        self.assertTrue(all(row["max_rgb48_code_error"] is not None
                            for row in receipt["results"]))

    def test_nan_output_cannot_be_marked_known_answer(self):
        self.overwrite_float("nonzero", "reconstruction", float("nan"))
        with self.assertRaisesRegex(ValueError, "Non-finite"):
            check_probe.check(self.root)
        self.assertFalse((self.root / "render-receipt.json").exists())

    def test_infinite_alpha_output_is_rejected(self):
        self.overwrite_float("nonzero", "rendered", float("inf"), index=3)
        with self.assertRaisesRegex(ValueError, "Non-finite"):
            check_probe.check(self.root)

    def test_nonfinite_input_textures_are_rejected(self):
        for stage, value in (("bl", float("nan")), ("el", -float("inf"))):
            with self.subTest(stage=stage):
                path = self.root / "outputs" / f"zero-{stage}.rgba32f"
                original = path.read_bytes()
                self.overwrite_float("zero", stage, value)
                with self.assertRaisesRegex(ValueError, "Non-finite"):
                    check_probe.check(self.root)
                path.write_bytes(original)

    def test_truncated_float_input_or_output_is_rejected(self):
        for stage in ("el", "reconstruction"):
            with self.subTest(stage=stage):
                path = self.root / "outputs" / f"nonzero-{stage}.rgba32f"
                original = path.read_bytes()
                path.write_bytes(original[:-1])
                with self.assertRaisesRegex(ValueError, "Unexpected RGBA32F length"):
                    check_probe.check(self.root)
                path.write_bytes(original)

    def test_truncated_integer_export_is_rejected(self):
        path = self.root / "outputs/nonzero-rendered.rgb48le"
        path.write_bytes(path.read_bytes()[:-2])
        with self.assertRaisesRegex(ValueError, "Unexpected RGB48LE length"):
            check_probe.check(self.root)

    def test_wrong_shifted_integer_export_is_rejected(self):
        path = self.root / "outputs/shifted-reconstruction.rgb48le"
        path.write_bytes(bytes(len(path.read_bytes())))
        with self.assertRaisesRegex(ValueError, "Integer known-answer failure"):
            check_probe.check(self.root)

    def test_nonfinite_json_literals_or_overflow_are_rejected(self):
        path = self.root / "probe.jsonl"
        original = path.read_text()
        for literal in ("NaN", "Infinity", "-Infinity", "1e9999"):
            with self.subTest(literal=literal):
                path.write_text(original + '{"kind":"diagnostic","number":' + literal + '}\n')
                with self.assertRaisesRegex(ValueError, "Non-finite JSON"):
                    check_probe.check(self.root)
        path.write_text(original)


class ScratchLifecycleTests(unittest.TestCase):
    def test_reference_generator_uses_only_explicit_new_destination(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "reference-output"
            source_before = (BUNDLE / "identity_reference.py").read_bytes()
            generated = subprocess.run(
                ["python3", str(BUNDLE / "identity_reference.py"), str(output)],
                capture_output=True, text=True, check=False)
            self.assertEqual(generated.returncode, 0, generated.stderr)
            self.assertTrue((output / "shifted-reference.rgb48le").exists())
            self.assertEqual((BUNDLE / "identity_reference.py").read_bytes(), source_before)
            repeated = subprocess.run(
                ["python3", str(BUNDLE / "identity_reference.py"), str(output)],
                capture_output=True, text=True, check=False)
            self.assertNotEqual(repeated.returncode, 0)
            self.assertIn("already exists", repeated.stderr)

    def test_replay_refuses_existing_scratch_before_docker_or_network(self):
        with tempfile.TemporaryDirectory() as existing:
            result = subprocess.run(["sh", str(BUNDLE / "replay.sh"), existing],
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 2)
            self.assertIn("must not already exist", result.stderr)
            self.assertEqual(list(Path(existing).iterdir()), [])

    def test_replay_refuses_destination_inside_source_bundle(self):
        output = BUNDLE / "forbidden-checker-test-output"
        self.assertFalse(output.exists())
        result = subprocess.run(["sh", str(BUNDLE / "replay.sh"), str(output)],
                                capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 2)
        self.assertIn("outside the source bundle", result.stderr)
        self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
