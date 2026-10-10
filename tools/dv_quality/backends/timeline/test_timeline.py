"""Focused false-acceptance mutations against actual timeline artifacts."""
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
import check_timeline as checker
import inspect_matroska
import dependency_identity

CASES = ['normal-b0', 'normal-b2', 'seek-reset', 'epochs-reset', *checker.EXPECTED_NEGATIVES]


class TimelineTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        for pattern in ('*.mkv', '*.ffprobe.json', 'source-definition.json'):
            for path in SOURCE.glob(pattern):
                shutil.copyfile(path, self.root / path.name)
        shutil.copytree(SOURCE / 'rpu-tags', self.root / 'rpu-tags')
        for case in CASES:
            shutil.copytree(SOURCE / case, self.root / case)

    def tearDown(self):
        self.temp.cleanup()

    def mutate(self, case, function):
        path = self.root / case / 'decode.jsonl'
        rows = checker.events(path)
        function(rows)
        path.write_text(''.join(json.dumps(row) + '\n' for row in rows))

    def refuse(self, case):
        with self.assertRaises(checker.TimelineError):
            checker.assess(self.root, case)

    def test_actual_slice_and_negatives(self):
        results = [checker.assess(self.root, case) for case in CASES]
        self.assertEqual(sum(len(result.get('pairs', [])) for result in results), 21)
        self.assertEqual(sum(not result['accepted'] for result in results), 5)
        seek = results[2]['pairs']
        self.assertEqual([pair['presentation_role'] for pair in seek],
                         ['preroll', 'selected-display', 'selected-display'])

    def test_seek_filtered_list_without_api_evidence_refused(self):
        self.mutate('seek-reset', lambda rows: next(row for row in rows if row['kind'] == 'epoch_boundary').update(seek_result=-1))
        self.refuse('seek-reset')

    def test_false_reset_refused(self):
        self.mutate('seek-reset', lambda rows: next(row for row in rows if row['kind'] == 'epoch_boundary').update(reset_applied=False))
        self.refuse('seek-reset')

    def test_wrong_epoch_boundary_refused(self):
        self.mutate('epochs-reset', lambda rows: next(row for row in rows if row['kind'] == 'epoch_boundary').update(to=9))
        self.refuse('epochs-reset')

    def test_non_keyframe_seek_refused(self):
        self.mutate('seek-reset', lambda rows: next(row for row in rows if row['kind'] == 'demux_packet' and row['epoch'] == 1).update(keyframe=False))
        self.refuse('seek-reset')

    def test_post_seek_nominal_duration_guess_refused(self):
        def change(rows):
            for row in rows:
                if row.get('epoch') == 1 and row['kind'] in ('demux_packet', 'split_packet', 'decoded_frame'):
                    row['duration'] = '33/1000'
        self.mutate('seek-reset', change)
        self.refuse('seek-reset')

    def test_split_timing_change_refused(self):
        self.mutate('normal-b2', lambda rows: next(row for row in rows if row['kind'] == 'split_packet').update(pts='999/1000'))
        self.refuse('normal-b2')

    def test_cross_epoch_pts_flattening_refused(self):
        self.mutate('epochs-reset', lambda rows: [row.update(epoch=0) for row in rows if row['kind'] == 'decoded_frame' and row['epoch'] == 1])
        self.refuse('epochs-reset')

    def test_actual_cross_epoch_el_bytes_refused(self):
        folder = self.root / 'epochs-reset/frames'
        shutil.copyfile(folder / 'epoch-0-el-frame-000.yuv420p10le', folder / 'epoch-1-el-frame-000.yuv420p10le')
        self.refuse('epochs-reset')

    def test_actual_rpu_substitution_refused(self):
        shutil.copyfile(self.root / 'rpu-tags/p7-frame0.nal', self.root / 'seek-reset/frames/epoch-1-bl-frame-000.rpu.nal')
        self.refuse('seek-reset')

    def test_stored_block_duration_change_refused(self):
        path = self.root / 'compound-vfr-b2.mkv'
        report = inspect_matroska.inspect(path)
        block = next(block for block in report['blocks'] if block['pts_ticks'] == 140)
        self.assertEqual(block['block_duration_ticks'], 90)
        offset = block['duration_payload_offset']
        size = block['duration_payload_size']
        data = path.read_bytes()
        path.write_bytes(data[:offset] + (89).to_bytes(size, 'big') + data[offset + size:])
        self.refuse('seek-reset')

    def test_negative_crash_never_passes(self):
        (self.root / 'seek-no-reset/decode-status.txt').write_text('139\n')
        self.refuse('seek-no-reset')

    def test_negative_unrelated_diagnostic_never_passes(self):
        (self.root / 'implicit-seek/decode.stderr').write_text('unrelated decoder corruption\n')
        self.refuse('implicit-seek')

    def test_negative_unknown_native_bytes_never_passes(self):
        path = self.root / 'implicit-seek/frames/epoch-1-bl-frame-000.yuv420p10le'
        path.write_bytes(b'\0\0' + path.read_bytes()[2:])
        self.refuse('implicit-seek')

    def test_nonfinite_trace_never_passes(self):
        path = self.root / 'normal-b0/decode.jsonl'
        path.write_text(path.read_text().replace('"l1_min":0', '"l1_min":NaN', 1))
        self.refuse('normal-b0')

    def test_seek_boundary_after_frames_refused(self):
        def change(rows):
            boundary = next(row for row in rows if row['kind'] == 'epoch_boundary')
            rows.remove(boundary)
            rows.append(boundary)
        self.mutate('seek-reset', change)
        self.refuse('seek-reset')

    def test_discontinuity_boundary_after_frames_refused(self):
        def change(rows):
            boundary = next(row for row in rows if row['kind'] == 'epoch_boundary')
            rows.remove(boundary)
            rows.append(boundary)
        self.mutate('epochs-reset', change)
        self.refuse('epochs-reset')

    def test_missing_required_drains_refused(self):
        self.mutate('epochs-reset', lambda rows: rows.__setitem__(slice(None), [row for row in rows if row['kind'] != 'drain']))
        self.refuse('epochs-reset')

    def test_missing_final_seek_drain_refused(self):
        self.mutate('seek-reset', lambda rows: rows.pop())
        self.refuse('seek-reset')

    def test_duplicate_lifecycle_event_refused(self):
        self.mutate('seek-reset', lambda rows: rows.append(dict(next(row for row in rows if row['kind'] == 'epoch_boundary'))))
        self.refuse('seek-reset')

    def test_overflow_number_refused(self):
        path = self.root / 'normal-b0/decode.jsonl'
        path.write_text(path.read_text().replace('"l1_min":0', '"l1_min":1e400', 1))
        self.refuse('normal-b0')

    def test_boolean_integer_fields_refused(self):
        for case, kind, field in [('normal-b0', 'decoded_frame', 'l1_min'),
                                  ('seek-reset', 'epoch_boundary', 'seek_result')]:
            path = self.root / case / 'decode.jsonl'
            original = path.read_text()
            self.mutate(case, lambda rows: next(row for row in rows if row['kind'] == kind).update({field: False}))
            self.refuse(case)
            path.write_text(original)

    def test_float_integer_field_refused(self):
        self.mutate('normal-b0', lambda rows: next(row for row in rows if row['kind'] == 'decoded_frame').update(l1_min=0.0))
        self.refuse('normal-b0')


class DependencyTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        for name in ('ffmpeg-prefix', 'rpu-tags'):
            shutil.copytree(SOURCE / name, self.root / name)

    def tearDown(self):
        self.temp.cleanup()

    def test_approved_copy_passes(self):
        self.assertEqual(dependency_identity.validate(self.root)['status'], 'passed')

    def test_substituted_library_refused(self):
        path = self.root / 'ffmpeg-prefix/lib/libavcodec.a'
        path.write_bytes(path.read_bytes() + b'changed')
        with self.assertRaises(ValueError):
            dependency_identity.validate(self.root)

    def test_substituted_fixture_refused(self):
        path = self.root / 'rpu-tags/p7-frame0.nal'
        path.write_bytes(path.read_bytes() + b'changed')
        with self.assertRaises(ValueError):
            dependency_identity.validate(self.root)


if __name__ == '__main__':
    SOURCE = Path(sys.argv[1]).resolve()
    unittest.main(argv=[sys.argv[0]])
