"""Negative controls that must fail the independent once-mapping arithmetic."""
from pathlib import Path
import shutil
import struct
import tempfile
import unittest
import scalar_gpu
import json
from registered_limits import read_limits

ROOT = Path(__file__).resolve().parent


class NonidentityCheckerControls(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in ('decoded.yuv420p10le','preregistered-controls.json','source-definition.json'):
            shutil.copyfile(ROOT/name,self.root/name)
        for prefix in ('adapted','wrong'):
            for f in range(4):
                for extension in ('nal','json'):
                    name = f'{prefix}-rpu-frame{f}.{extension}'
                    shutil.copyfile(ROOT/name,self.root/name)
        for control in ('correct','wrong'):
            shutil.copytree(ROOT/'gpu'/control,self.root/'gpu'/control)

    def verify(self):
        return scalar_gpu.verify(self.root)

    def test_actual_correct_and_deliberate_negative_pass(self):
        receipt = self.verify()
        self.assertEqual(receipt['result'],'analytic-control-pass')
        self.assertTrue(all(row['once_scalar_control_rejects_wrong_output'] for row in receipt['repeated_mapping_negative']))

    def test_wrong_output_substituted_for_correct_rejected(self):
        for stage in ('reconstruction','rendered'):
            for extension in ('rgb48le','rgba32f'):
                name = f'zero-{stage}.{extension}'
                shutil.copyfile(self.root/'gpu/wrong/frame-0/outputs'/name,
                                self.root/'gpu/correct/frame-0/outputs'/name)
        with self.assertRaisesRegex(ValueError,'registered arithmetic limit'):
            self.verify()

    def test_swapped_frame_outputs_rejected(self):
        for stage in ('reconstruction','rendered'):
            for extension in ('rgb48le','rgba32f'):
                name = f'zero-{stage}.{extension}'
                a,b = [self.root/'gpu/correct'/f'frame-{f}/outputs'/name for f in (1,2)]
                data_a,data_b = a.read_bytes(),b.read_bytes()
                a.write_bytes(data_b);b.write_bytes(data_a)
        with self.assertRaisesRegex(ValueError,'registered arithmetic limit'):
            self.verify()

    def test_truncated_decoded_sequence_rejected(self):
        path = self.root/'decoded.yuv420p10le'
        path.write_bytes(path.read_bytes()[:-1])
        with self.assertRaisesRegex(ValueError,'sequence length'):
            self.verify()

    def test_nonfinite_gpu_output_rejected(self):
        path = self.root/'gpu/correct/frame-0/outputs/zero-reconstruction.rgba32f'
        data = bytearray(path.read_bytes());data[:4] = struct.pack('<f',float('nan'))
        path.write_bytes(data)
        with self.assertRaisesRegex(ValueError,'nonfinite'):
            self.verify()

    def test_nonfinite_alpha_rejected(self):
        path = self.root/'gpu/correct/frame-0/outputs/zero-reconstruction.rgba32f'
        data = bytearray(path.read_bytes());data[12:16] = struct.pack('<f',float('nan'))
        path.write_bytes(data)
        with self.assertRaisesRegex(ValueError,'nonfinite'):
            self.verify()

    def test_nonfinite_limits_rejected(self):
        path = self.root/'preregistered-controls.json'
        data = json.loads(path.read_text());data['gpu_vs_scalar_full_matrix_max_pq_error'] = float('nan')
        path.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError,'immutable registered'):
            self.verify()

    def test_widened_finite_limits_rejected(self):
        path = self.root/'preregistered-controls.json'
        data = json.loads(path.read_text());data['gpu_vs_scalar_full_matrix_max_rgb_code_error'] = 999999
        path.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError,'immutable registered'):
            self.verify()

    def test_changed_consumed_decoded_input_rejected(self):
        path = self.root/'gpu/correct/frame-0/decoded.yuv420p10le'
        data = bytearray(path.read_bytes());data[0] ^= 1;path.write_bytes(data)
        with self.assertRaisesRegex(ValueError,'consumed decoded'):
            self.verify()

    def test_swapped_consumed_rpu_rejected(self):
        a = self.root/'gpu/correct/frame-0/consumed-rpu.nal'
        b = self.root/'gpu/correct/frame-1/consumed-rpu.nal'
        a.write_bytes(b.read_bytes())
        with self.assertRaisesRegex(ValueError,'consumed RPU'):
            self.verify()

    def test_changed_curve_event_rejected(self):
        path = self.root/'gpu/correct/frame-0/probe.jsonl'
        rows = [json.loads(line) for line in path.read_text().splitlines()]
        for row in rows:
            if row.get('kind') == 'parsed_luma_curve':
                row['constant_fraction'] = 524288
        path.write_text(''.join(json.dumps(row)+'\n' for row in rows))
        with self.assertRaisesRegex(ValueError,'parsed luma curve'):
            self.verify()

    def test_changed_frame_index_event_rejected(self):
        path = self.root/'gpu/correct/frame-0/probe.jsonl'
        rows = [json.loads(line) for line in path.read_text().splitlines()]
        for row in rows:
            if row.get('kind') == 'frame':
                row['pts'] = '1/24'
        path.write_text(''.join(json.dumps(row)+'\n' for row in rows))
        with self.assertRaisesRegex(ValueError,'frame event'):
            self.verify()

    def test_numeric_boolean_event_substitution_rejected(self):
        path = self.root/'gpu/correct/frame-0/probe.jsonl'
        original = path.read_text()
        for kind,field,value in [('frame','rpu_parsed',1),('frame','render_errors',False),
                                  ('parsed_rpu','parser_error',0),('parsed_luma_curve','constant_integer',False)]:
            rows = [json.loads(line) for line in original.splitlines()]
            for row in rows:
                if row.get('kind') == kind:
                    row[field] = value
            path.write_text(''.join(json.dumps(row)+'\n' for row in rows))
            with self.assertRaisesRegex(ValueError,'actual GPU|actual parsed'):
                self.verify()

    def test_missing_output_rejected(self):
        (self.root/'gpu/correct/frame-2/outputs/zero-rendered.rgb48le').unlink()
        with self.assertRaises(FileNotFoundError):
            self.verify()

    def test_changed_integer_code_rejected(self):
        path = self.root/'gpu/correct/frame-0/outputs/zero-reconstruction.rgb48le'
        data = bytearray(path.read_bytes());value = struct.unpack('<H',data[:2])[0]
        data[:2] = struct.pack('<H',value+256);path.write_bytes(data)
        with self.assertRaisesRegex(ValueError,'registered arithmetic limit'):
            self.verify()

    def test_correct_output_substituted_for_wrong_rejected(self):
        for stage in ('reconstruction','rendered'):
            for extension in ('rgb48le','rgba32f'):
                name = f'zero-{stage}.{extension}'
                shutil.copyfile(self.root/'gpu/correct/frame-0/outputs'/name,
                                self.root/'gpu/wrong/frame-0/outputs'/name)
        with self.assertRaisesRegex(ValueError,'registered arithmetic limit'):
            self.verify()


if __name__ == '__main__':
    unittest.main()
