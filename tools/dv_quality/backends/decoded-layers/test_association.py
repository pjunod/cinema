"""Association false-positive regressions using real decoder evidence copies."""
import json, shutil, tempfile, unittest
from pathlib import Path
import sys
import check_association

class AssociationTest(unittest.TestCase):

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        for name in ['source-definition.json', 'bl-b0.mkv', 'bl-b0.ffprobe.json', 'el-b0.ffprobe.json']:
            shutil.copyfile(SOURCE / name, self.root / name)
        shutil.copytree(SOURCE / 'rpu-tags', self.root / 'rpu-tags')
        folder = self.root / 'b0-normal'
        folder.mkdir()
        for name in ['decode-status.txt', 'decode.stderr', 'decode.jsonl', 'compound.mkv']:
            shutil.copyfile(SOURCE / 'b0-normal' / name, folder / name)
        shutil.copytree(SOURCE / 'b0-normal/frames', folder / 'frames')

    def tearDown(self):
        self.temp.cleanup()

    def modify(self, fn):
        path = self.root / 'b0-normal/decode.jsonl'
        rows = check_association.events(path)
        fn(rows)
        path.write_text(''.join((json.dumps(row) + '\n' for row in rows)))

    def refused(self):
        with self.assertRaises(check_association.AssociationError):
            check_association.inspect(self.root, 0, 'normal')

    def test_actual_byte_exact_pairing_passes(self):
        self.assertEqual(len(check_association.inspect(self.root, 0, 'normal')['pairs']), 6)

    def test_duplicate_el_pts_refused(self):

        def change(rows):
            el = [r for r in rows if r['kind'] == 'decoded_frame' and r['layer'] == 'el']
            el[1]['pts'] = el[0]['pts']
        self.modify(change)
        self.refused()

    def test_unknown_pts_refused(self):
        self.modify(lambda rows: next((r for r in rows if r['kind'] == 'decoded_frame')).update(pts=None))
        self.refused()

    def test_metadata_presence_without_raw_rpu_refused(self):
        self.modify(lambda rows: next((r for r in rows if r['kind'] == 'decoded_frame' and r['layer'] == 'bl')).update(decoder_rpu_present=False))
        self.refused()

    def test_wrong_rpu_bytes_refused(self):
        shutil.copyfile(self.root / 'rpu-tags/p7-frame2.nal', self.root / 'b0-normal/frames/bl-frame-001.rpu.nal')
        self.refused()

    def test_wrong_el_pixels_refused(self):
        p = self.root / 'b0-normal/frames/el-frame-001.yuv420p10le'
        raw = p.read_bytes()
        p.write_bytes(b'\x00\x00' + raw[2:])
        self.refused()

    def test_changed_split_duration_refused(self):
        self.modify(lambda rows: next((r for r in rows if r['kind'] == 'split_packet')).update(duration='42/1000'))
        self.refused()

    def test_rext_profile_refused(self):
        p = self.root / 'bl-b0.ffprobe.json'
        data = json.loads(p.read_text())
        data['streams'][0]['profile'] = 'Rext'
        p.write_text(json.dumps(data))
        self.refused()

    def test_crash_not_accepted_as_controlled_refusal(self):
        p = self.root / 'b0-normal/decode-status.txt'
        p.write_text('139\n')
        self.refused()

    def negative(self, mode):
        folder = self.root / f'b0-{mode}'
        shutil.copytree(SOURCE / f'b0-{mode}', folder)
        return folder

    def negative_refused(self, mode):
        with self.assertRaises(check_association.AssociationError):
            check_association.assess(self.root, 0, mode)

    def test_actual_negatives_have_exact_expected_stage(self):
        for mode, (stage, reason) in check_association.EXPECTED_NEGATIVES.items():
            self.negative(mode)
            result = check_association.assess(self.root, 0, mode)
            self.assertFalse(result['accepted'])
            self.assertEqual(result['failure_stage'], stage)
            self.assertEqual(result['reason'], reason)

    def test_negative_crash_refused(self):
        folder = self.negative('missing-el')
        (folder / 'decode-status.txt').write_text('139\n')
        self.negative_refused('missing-el')

    def test_negative_wrong_status_refused(self):
        folder = self.negative('missing-el')
        (folder / 'decode-status.txt').write_text('2\n')
        self.negative_refused('missing-el')

    def test_negative_wrong_diagnostic_refused(self):
        folder = self.negative('missing-el')
        p = folder / 'decode.stderr'
        p.write_text(p.read_text().replace('POC 2', 'POC 99'))
        self.negative_refused('missing-el')

    def test_negative_extra_diagnostic_refused(self):
        folder = self.negative('missing-el')
        p = folder / 'decode.stderr'
        p.write_text('unrelated corruption\n' + p.read_text())
        self.negative_refused('missing-el')

    def test_negative_wrong_decoder_stage_refused(self):
        folder = self.negative('missing-rpu')
        for name in ['decode-status.txt', 'decode.stderr']:
            shutil.copyfile(SOURCE / 'b0-missing-el' / name, folder / name)
        self.negative_refused('missing-rpu')

    def test_negative_wrong_association_reason_refused(self):
        folder = self.negative('swapped-rpu')
        p = folder / 'frames/el-frame-001.yuv420p10le'
        raw = p.read_bytes()
        p.write_bytes(b'\x00\x00' + raw[2:])
        self.negative_refused('swapped-rpu')

    def test_negative_wrong_association_stage_refused(self):
        folder = self.negative('missing-el')
        for name in ['decode-status.txt', 'decode.stderr', 'decode.jsonl']:
            shutil.copyfile(SOURCE / 'b0-missing-rpu' / name, folder / name)
        shutil.rmtree(folder / 'frames')
        shutil.copytree(SOURCE / 'b0-missing-rpu/frames', folder / 'frames')
        self.negative_refused('missing-el')
if __name__ == '__main__':
    SOURCE = Path(sys.argv[1]).resolve()
    unittest.main(argv=[sys.argv[0]])
