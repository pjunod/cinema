"""Focused checks that prevent stale or broken output from passing the checker."""
import json
from pathlib import Path
import shutil
import tempfile
import unittest

import check_cpu_outputs

ROOT = Path(__file__).resolve().parent


class CpuCheckerControls(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.actual = self.root / 'actual'
        self.reference = self.root / 'reference'
        shutil.copytree(ROOT / 'cpu-outputs', self.actual)
        shutil.copytree(ROOT / 'cpu-reference', self.reference)
        self.negative = self.root / 'negative.json'
        shutil.copyfile(ROOT / 'negative-controls.json', self.negative)

    def verify(self):
        return check_cpu_outputs.verify(self.actual, self.reference, self.negative)

    def test_real_controls_pass(self):
        self.assertEqual(self.verify()['result'], 'pass')

    def test_modified_pixel_rejected(self):
        path = self.actual / 'nonzero.rgb48le'
        data = bytearray(path.read_bytes())
        data[0] ^= 1
        path.write_bytes(data)
        with self.assertRaises(ValueError):
            self.verify()

    def test_truncated_frame_rejected(self):
        path = self.actual / 'zero.rgb48le'
        path.write_bytes(path.read_bytes()[:-1])
        with self.assertRaises(ValueError):
            self.verify()

    def test_missing_frame_rejected(self):
        (self.actual / 'bounded.rgb48le').unlink()
        with self.assertRaises(FileNotFoundError):
            self.verify()

    def test_missing_reference_rejected(self):
        (self.reference / 'disabled.rgb48le').unlink()
        with self.assertRaises(FileNotFoundError):
            self.verify()

    def test_wrong_negative_exit_rejected(self):
        self.negative.write_text(json.dumps({'missing_el': 0, 'truncated_rpu': 67}))
        with self.assertRaises(ValueError):
            self.verify()

    def test_changed_matrix_control_rejected(self):
        path = self.actual / 'nonstandard.rgb48le'
        data = bytearray(path.read_bytes())
        data[0] ^= 1
        path.write_bytes(data)
        with self.assertRaises(ValueError):
            self.verify()

    def test_rejected_case_output_rejected(self):
        (self.actual / 'missing-el.rgb48le').write_bytes(b'bad')
        with self.assertRaises(ValueError):
            self.verify()


if __name__ == '__main__':
    unittest.main()
