"""Focused strict identity and order controls for synthetic NAL insertion."""
from pathlib import Path
import json
import unittest
import inject_rpu
ROOT = Path(__file__).resolve().parent

class InjectionControls(unittest.TestCase):
    def setUp(self):
        self.source = (ROOT / 'hdr10-base.hevc').read_bytes()
        frames = json.loads((ROOT / 'source-definition.json').read_text())['frames']
        self.rpus = [(ROOT / f['rpu_path']).read_bytes() for f in frames]
        self.expected = [f['rpu_sha256'] for f in frames]

    def test_exact_four(self):
        out = inject_rpu.inject(self.source, self.rpus, self.expected)
        self.assertEqual([n for n in inject_rpu.nals(out) if (n[0] >> 1) & 63 == 62], self.rpus)
        self.assertEqual(len(set(self.expected)), 4)

    def test_missing_metadata_rejected(self):
        with self.assertRaises(ValueError):
            inject_rpu.inject(self.source, self.rpus[:-1], self.expected)

    def test_extra_metadata_rejected(self):
        with self.assertRaises(ValueError):
            inject_rpu.inject(self.source, self.rpus + [self.rpus[-1]], self.expected)

    def test_swapped_metadata_rejected(self):
        wrong = self.rpus.copy()
        wrong[1], wrong[2] = wrong[2], wrong[1]
        with self.assertRaisesRegex(ValueError, 'identity/order'):
            inject_rpu.inject(self.source, wrong, self.expected)

    def test_existing_rpu_rejected(self):
        with self.assertRaises(ValueError):
            inject_rpu.inject((ROOT / 'candidate.hevc').read_bytes(), self.rpus, self.expected)

    def test_missing_aud_rejected(self):
        units = inject_rpu.nals(self.source)
        data = b''.join(b'\x00\x00\x00\x01' + n for n in units if (n[0] >> 1) & 63 != 35)
        with self.assertRaises(ValueError):
            inject_rpu.inject(data, self.rpus, self.expected)

if __name__ == '__main__':
    unittest.main()
