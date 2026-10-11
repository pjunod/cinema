"""Refusal and actual identity propagation regressions without Docker side effects."""
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import image_identity

class ImageIdentityTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.path = self.root / 'image-id.txt'
        self.image_id = 'sha256:' + 'a' * 64
    def tearDown(self):
        self.temp.cleanup()
    def test_alternate_valid_identity_propagates(self):
        self.path.write_text(self.image_id + '\n')
        result = subprocess.CompletedProcess([], 0, self.image_id+'|linux|arm64\n', '')
        with patch('image_identity.subprocess.run', return_value=result) as docker:
            actual = image_identity.inspect_identity(self.path)
        self.assertEqual(docker.call_args.args[0][3], self.image_id)
        for stage in ('parsed','cross','encode','chroma'):
            image_identity.record_identity(self.root, stage, actual)
            receipt = json.loads((self.root/f'{stage}-build-identity.json').read_text())
            self.assertEqual(receipt['image_id'], self.image_id)
    def test_malformed_missing_and_tag_refused(self):
        with self.assertRaises(ValueError): image_identity.read_identity(self.path)
        for value in ('codex/plurx-dv-m0:mechanics', self.image_id+'\nextra',
                      'sha256:'+'A'*64, ' '+self.image_id, 'sha256:123'):
            with self.subTest(value=value):
                self.path.write_text(value)
                with self.assertRaises(ValueError): image_identity.read_identity(self.path)
    def test_symlink_refused(self):
        target = self.root/'target'; target.write_text(self.image_id)
        self.path.symlink_to(target)
        with self.assertRaises(ValueError): image_identity.read_identity(self.path)
    def test_inspection_identity_platform_and_unavailable_refused(self):
        self.path.write_text(self.image_id)
        for output in ('sha256:'+'b'*64+'|linux|arm64', self.image_id+'|linux|amd64'):
            with patch('image_identity.subprocess.run', return_value=subprocess.CompletedProcess([],0,output,'')):
                with self.assertRaises(ValueError): image_identity.inspect_identity(self.path)
        with patch('image_identity.subprocess.run', side_effect=subprocess.CalledProcessError(1,[])):
            with self.assertRaises(subprocess.CalledProcessError): image_identity.inspect_identity(self.path)
    def test_replay_scripts_refuse_bad_identity_before_creating_scratch(self):
        self.path.write_text('mutable:tag')
        bundle = Path(__file__).resolve().parent
        for script, count in (('replay_parsed.sh',2),('replay_cross.sh',3)):
            with self.subTest(script=script):
                output = self.root / script
                arguments = ['sh',str(bundle/script),str(output),str(self.root)]
                if count == 3: arguments.append(str(self.root))
                result = subprocess.run(arguments,capture_output=True)
                self.assertNotEqual(result.returncode,0)
                self.assertFalse(output.exists())
        result = subprocess.run(['sh',str(bundle/'encode_hdr10.sh'),str(self.root)],capture_output=True)
        self.assertNotEqual(result.returncode,0)
        self.assertFalse((self.root/'parsed-render-hdr10.hevc').exists())
    def test_receipt_identity_mismatch_refused(self):
        self.path.write_text(self.image_id)
        with self.assertRaises(ValueError): image_identity.record_identity(self.root,'parsed','sha256:'+'b'*64)
        self.assertFalse((self.root/'parsed-build-identity.json').exists())
if __name__=='__main__': unittest.main()
