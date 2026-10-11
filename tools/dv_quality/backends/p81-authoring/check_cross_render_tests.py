"""Reject substituted GPU pixels and ambiguous frame/stage provenance."""
from pathlib import Path
import json
import shutil
import tempfile
import unittest
import check_cross_render

ROOT = Path(__file__).resolve().parent


class CrossRenderControls(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        shutil.copytree(ROOT / 'cross-render', self.root / 'cross-render')
        for name in ('source-definition.json', 'analytic-hdr10-reference.rgb48le', 'decoded.yuv420p10le'):
            shutil.copyfile(ROOT / name, self.root / name)
        self.upstream_path = self.root / 'cross-render/upstream-results.json'
        self.upstream = json.loads(self.upstream_path.read_text())

    def verify(self):
        return check_cross_render.verify(self.root)

    def write_rows(self):
        self.upstream_path.write_text(json.dumps(self.upstream))

    def test_recorded_outputs_pass(self):
        self.assertEqual(self.verify()['result'], 'diagnostic-control-pass')

    def test_modified_close_output_rejected(self):
        for stage in check_cross_render.STAGES:
            path = self.root / 'cross-render/frame-0' / f'dv-on-{stage}.rgb48le'
            data = bytearray(path.read_bytes())
            data[0] ^= 1
            path.write_bytes(data)
        with self.assertRaisesRegex(ValueError, 'output hash'):
            self.verify()

    def test_scalar_substitute_rejected(self):
        reference = (self.root / 'analytic-hdr10-reference.rgb48le').read_bytes()
        for frame in range(4):
            data = reference[frame * 24576:(frame + 1) * 24576]
            for label in ('hdr10', 'dv-on'):
                for stage in check_cross_render.STAGES:
                    (self.root / 'cross-render' / f'frame-{frame}' / f'{label}-{stage}.rgb48le').write_bytes(data)
        with self.assertRaisesRegex(ValueError, 'output hash'):
            self.verify()

    def test_swapped_picture_outputs_rejected(self):
        for label in ('hdr10', 'dv-on'):
            for stage in check_cross_render.STAGES:
                paths = [self.root / 'cross-render' / f'frame-{f}' / f'{label}-{stage}.rgb48le' for f in (1, 2)]
                a, b = [path.read_bytes() for path in paths]
                paths[0].write_bytes(b)
                paths[1].write_bytes(a)
        with self.assertRaisesRegex(ValueError, 'output hash'):
            self.verify()

    def test_duplicate_stage_rejected(self):
        self.upstream['results'][1] = self.upstream['results'][0].copy()
        self.write_rows()
        with self.assertRaisesRegex(ValueError, 'duplicate'):
            self.verify()

    def test_missing_stage_rejected(self):
        self.upstream['results'].pop()
        self.write_rows()
        with self.assertRaisesRegex(ValueError, 'missing'):
            self.verify()

    def test_extra_row_rejected(self):
        self.upstream['results'].append(self.upstream['results'][0].copy())
        self.write_rows()
        with self.assertRaisesRegex(ValueError, 'duplicate'):
            self.verify()

    def test_wrong_recorded_delta_rejected(self):
        self.upstream['results'][0]['max_code_delta'] = 0
        self.write_rows()
        with self.assertRaisesRegex(ValueError, 'numeric delta'):
            self.verify()

    def test_wrong_recorded_hash_rejected(self):
        self.upstream['results'][0]['candidate_sha256'] = '0' * 64
        self.write_rows()
        with self.assertRaisesRegex(ValueError, 'output hash'):
            self.verify()


if __name__ == '__main__':
    unittest.main()
