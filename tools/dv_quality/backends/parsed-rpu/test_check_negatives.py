import json
import tempfile
import unittest
from pathlib import Path
import check_negatives

class NegativeReceiptTest(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(); self.root=Path(self.temp.name)
        for name,reason in check_negatives.EXPECTED.items():
            folder=self.root/'negatives'/name; (folder/'outputs').mkdir(parents=True)
            (folder/'status.txt').write_text('1\n');(folder/'stderr.txt').write_text(reason)
            event={'kind':'parsed_rpu','parser_error':False}
            (folder/'stdout.txt').write_text(json.dumps(event)+'\n' if name=='missing-el' else '')
    def tearDown(self): self.temp.cleanup()
    def test_controlled_refusals_pass(self): self.assertEqual(len(check_negatives.check(self.root)),4)
    def test_crash_success_and_wrong_status_refused(self):
        path=self.root/'negatives/missing/status.txt'
        for status in (139,134,0,2,-11):
            with self.subTest(status=status):
                path.write_text(f'{status}\n')
                with self.assertRaises(ValueError): check_negatives.check(self.root)
    def test_wrong_reason_refused(self):
        (self.root/'negatives/missing/stderr.txt').write_text('Probe failed: unrelated refusal\n')
        with self.assertRaises(ValueError): check_negatives.check(self.root)
    def test_render_event_refused(self):
        (self.root/'negatives/missing-el/stdout.txt').write_text('{"kind":"frame"}\n')
        with self.assertRaises(ValueError): check_negatives.check(self.root)
    def test_frame_write_refused(self):
        (self.root/'negatives/bounded/outputs/frame.rgb').write_bytes(b'x')
        with self.assertRaises(ValueError): check_negatives.check(self.root)
    def test_unexpected_artifact_refused(self):
        (self.root/'negatives/truncated/unexpected.rgb').write_bytes(b'x')
        with self.assertRaises(ValueError): check_negatives.check(self.root)
if __name__=='__main__': unittest.main()
