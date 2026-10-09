"""Adversarial decoded order/source controls against the independent scalar check."""
from pathlib import Path
import shutil
import tempfile
import unittest
import check_authoring
ROOT = Path(__file__).resolve().parent

class PictureControls(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in ('reconstructed16.rgb48le', 'reconstructed64.rgb48le',
                     'reconstructed64.yuv420p10le', 'decoded.yuv420p10le',
                     'decoded-hdr10.rgb48le', 'baseline-decoded.yuv420p10le', 'probe.json'):
            shutil.copyfile(ROOT/name, self.root/name)
        shutil.copytree(ROOT/'cpu-reference', self.root/'cpu-reference')
        self.old_root = check_authoring.ROOT
        check_authoring.ROOT = self.root
        self.addCleanup(setattr, check_authoring, 'ROOT', self.old_root)

    def swap(self, name, size):
        path = self.root/name
        data = path.read_bytes()
        frames = [data[f*size:(f+1)*size] for f in range(4)]
        frames[1], frames[2] = frames[2], frames[1]
        path.write_bytes(b''.join(frames))

    def test_distinct_frames_pass(self):
        self.assertEqual(check_authoring.verify()['result'], 'pass')

    def test_swapped_decoded_frames_rejected(self):
        for name in ('decoded.yuv420p10le', 'baseline-decoded.yuv420p10le'):
            self.swap(name, 64*64*3)
        self.swap('decoded-hdr10.rgb48le', 64*64*6)
        with self.assertRaisesRegex(ValueError, 'analytic bound failed'):
            check_authoring.verify()

    def test_swapped_source_frames_rejected(self):
        self.swap('reconstructed64.rgb48le', 64*64*6)
        with self.assertRaisesRegex(ValueError, 'source expansion/timing'):
            check_authoring.verify()

    def test_truncated_decode_rejected(self):
        p = self.root/'decoded.yuv420p10le'
        p.write_bytes(p.read_bytes()[:-1])
        with self.assertRaisesRegex(ValueError, 'artifact length'):
            check_authoring.verify()

if __name__ == '__main__':
    unittest.main()
