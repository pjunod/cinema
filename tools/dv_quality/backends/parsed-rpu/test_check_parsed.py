"""Meaningful corruption controls for the bounded parsed-RPU arithmetic checker."""
import json,shutil,struct,subprocess,sys,tempfile,unittest
from pathlib import Path
BUNDLE=Path(__file__).resolve().parent
class CheckerTest(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.root=Path(self.temp.name)
        source=Path(sys.argv[-1]) if len(sys.argv)>1 and Path(sys.argv[-1]).is_dir() else Path('/private/tmp/plurx-dv-m0-parsed-replay')
        shutil.copytree(source/'outputs',self.root/'outputs')
        shutil.copytree(BUNDLE/'fixtures',self.root/'fixtures')
    def tearDown(self):
        self.temp.cleanup()
    def run_checker(self):
        return subprocess.run([sys.executable,str(BUNDLE/'check_parsed.py'),str(self.root)],capture_output=True).returncode
    def test_valid(self):
        self.assertEqual(self.run_checker(),0)
    def test_nan_and_inf_input_output(self):
        for name in ('nonzero-bl','nonzero-el','nonzero-reconstruction','nonzero-rendered'):
            for value in (float('nan'),float('inf')):
                with self.subTest(name=name,value=str(value)):
                    path=self.root/f'outputs/{name}.rgba32f';original=path.read_bytes()
                    path.write_bytes(struct.pack('<f',value)+original[4:])
                    self.assertNotEqual(self.run_checker(),0)
                    path.write_bytes(original)
    def test_truncated_float_and_integer(self):
        for suffix in ('rgba32f','rgb48le'):
            path=self.root/f'outputs/shifted-reconstruction.{suffix}';original=path.read_bytes()
            path.write_bytes(original[:-1]);self.assertNotEqual(self.run_checker(),0);path.write_bytes(original)
    def test_shifted_integer_corruption(self):
        path=self.root/'outputs/shifted-reconstruction.rgb48le';original=path.read_bytes()
        path.write_bytes(b'\0\0'+original[2:]);self.assertNotEqual(self.run_checker(),0)
    def test_nonfinite_metadata(self):
        path=self.root/'fixtures/p7_fel_linear.json';data=json.loads(path.read_text())
        data['vdr_dm_data']['ycc_to_rgb_coef0']=float('nan')
        path.write_text(json.dumps(data));self.assertNotEqual(self.run_checker(),0)
if __name__=='__main__':
    unittest.main(argv=[sys.argv[0]])
