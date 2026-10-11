import shutil
import tempfile
import unittest
from pathlib import Path
import sys
import chroma_control

class ChromaControlTest(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
        for name in ['native444.yuv','rgb-edge.rgb48le','native-center.yuv','native-left.yuv','rgb-center.yuv','rgb-left.yuv']:
            shutil.copyfile(SOURCE/name,self.root/name)
    def tearDown(self):self.temp.cleanup()
    def test_actual_phases_pass(self): self.assertEqual(len(chroma_control.check(self.root)['results']),2)
    def test_left_pixels_mislabelled_center_refused(self):
        for prefix in ('native','rgb'):
            with self.subTest(prefix=prefix):
                path=self.root/f'{prefix}-center.yuv';original=path.read_bytes()
                path.write_bytes((self.root/f'{prefix}-left.yuv').read_bytes())
                with self.assertRaises(ValueError):chroma_control.check(self.root)
                path.write_bytes(original)
    def test_truncated_pixels_refused(self):
        path=self.root/'rgb-center.yuv';path.write_bytes(path.read_bytes()[:-1])
        with self.assertRaises(ValueError):chroma_control.check(self.root)
if __name__=='__main__':
    SOURCE=Path(sys.argv[1]).resolve()
    unittest.main(argv=[sys.argv[0]])
