"""Focused offline quality/association regressions, using authored tiny controls."""
from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from tools.dv_quality import metrics, run as quality


class MetricKnownAnswers(unittest.TestCase):
    def test_pinned_independent_metric_vectors(self):
        vectors = json.loads((ROOT / 'tools/dv_quality/metric_vectors.json').read_text())
        self.assertEqual(9, len(vectors['delta_e_itp']))
        for vector in vectors['delta_e_itp']:
            self.assertAlmostEqual(vector['expected'], metrics.delta_e_itp(vector['reference'], vector['candidate']), delta=1e-7)
        vector = vectors['rgb_to_ictcp']
        for actual, expected in zip(metrics.rgb_nits_to_ictcp(vector['rgb_nits']), vector['expected']):
            self.assertAlmostEqual(expected, actual, delta=1e-8)
        # PU21 values use independently evaluated 50-digit decimal arithmetic;
        # this proves numerical agreement, not independent metric semantics.
        pu = vectors['pu21_equation_numeric_checks']
        for nits, expected in zip(pu['inputs_absolute_cd_m2'], pu['outputs']):
            self.assertAlmostEqual(expected, metrics.pu21_encode(nits), delta=1e-10)

    def test_absolute_pq_bt2020_weights_and_ct_half_once(self):
        self.assertEqual(0, metrics.pq_eotf(0))
        self.assertAlmostEqual(10000, metrics.pq_eotf(1), delta=1e-7)
        self.assertAlmostEqual(100, metrics.pq_eotf(.508078421517399), delta=1e-8)
        self.assertEqual(678, metrics.bt2020_luminance((0, 1000, 0)))
        self.assertEqual(1, metrics.delta_e_itp((0, 0, 0), (0, 1 / 360, 0)))
        self.assertEqual(1, metrics.delta_e_itp((0, 0, 0), (1 / 720, 0, 0)))
        self.assertAlmostEqual(20 * math.log10(256), metrics.pu21_psnr_from_mse(1))
        for function, value in [(metrics.pq_eotf, float('nan')), (metrics.pq_eotf, 1.1),
                                (metrics.pq_inverse_eotf, -1), (metrics.pu21_encode, float('inf'))]:
            with self.assertRaises(ValueError):
                function(value)


class OfflineComparatorTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.output = self.root / 'receipt'
        self.candidate = self.make_manifest('candidate')
        self.baseline = self.make_manifest('baseline')

    def tearDown(self):
        self.temporary.cleanup()

    def make_manifest(self, role, samples=None, timestamps=None):
        folder = self.root / role
        folder.mkdir(exist_ok=True)
        if samples is None:
            samples = [[(32768, 32768, 32768)] * 4] * 2
        if timestamps is None:
            timestamps = [('0/1', '1/24'), ('1/24', '1/24')]
        entries = []
        for index, (pixels, (pts, duration)) in enumerate(zip(samples, timestamps)):
            data = b''.join(struct.pack('<HHH', *pixel) for pixel in pixels)
            name = f'frame-{index}.rgb'
            (folder / name).write_bytes(data)
            entries.append({'path': name, 'sha256': quality.digest(data), 'pts': pts, 'duration': duration})
        manifest = {
            'schema': 1, 'role': role,
            'source': {'id': 'authored-neutral-control', 'sha256': hashlib.sha256(b'authored-neutral-control-v1').hexdigest()},
            'renderer': {'id': 'authored-test', 'revision': 'v1', 'artifact_sha256': '2' * 64},
            'picture': {'width': 4, 'height': 1, 'pixel_format': 'rgb48le', 'range': 'full',
                        'primaries': 'bt2020', 'transfer': 'smpte2084', 'domain': 'mapped'},
            'target': {'id': 'fixed-1000', 'peak_nits': 1000, 'black_nits': .005, 'gamut': 'bt2020',
                       'mapping': 'none', 'parameters': {}},
            'frames': entries}
        if role == 'independent-reference':
            evidence = b'Authored neutral PQ samples. Analytic control only, no Dolby/FEL oracle.'
            (folder / 'method.txt').write_bytes(evidence)
            manifest['provenance'] = {'kind': 'independent', 'scope': 'analytic-control', 'producer': 'test-author',
                                      'method': 'independently authored neutral PQ code values',
                                      'evidence': {'path': 'method.txt', 'sha256': quality.digest(evidence)}}
        path = folder / 'manifest.json'
        path.write_text(json.dumps(manifest, indent=2) + '\n')
        return path

    def edit(self, path, change):
        manifest = json.loads(path.read_text())
        change(manifest)
        path.write_text(json.dumps(manifest) + '\n')

    def compare(self, reference=None):
        return quality.run(self.candidate, self.baseline, self.output, reference)

    def assert_refusal(self, message, reference=None):
        with self.assertRaisesRegex(quality.Refusal, message):
            self.compare(reference)
        self.assertFalse(self.output.exists())

    def test_difference_only_null_reference_and_reproducible_snapshots(self):
        original = self.candidate.read_bytes()
        frame = (self.candidate.parent / 'frame-0.rgb').read_bytes()
        receipt = self.compare()
        self.assertEqual('difference-only', receipt['mode'])
        self.assertIsNone(receipt['reference_error'])
        self.assertIsNone(receipt['capability_qualification'])
        self.assertEqual('declared-unverified', receipt['identities']['candidate']['source']['verification'])
        score = receipt['difference']
        self.assertEqual(8, score['pixels'])
        self.assertEqual(0, score['delta_e_itp']['mean'])
        self.assertTrue(score['pu21_luma']['identical'])
        self.assertIsNone(score['pu21_luma']['psnr_db'])
        self.assertNotIn('Infinity', (self.output / 'receipt.json').read_text())
        self.candidate.write_text('{}')
        (self.candidate.parent / 'frame-0.rgb').write_bytes(b'mutated')
        self.assertEqual(original, (self.output / 'inputs/candidate/manifest.json').read_bytes())
        self.assertEqual(frame, (self.output / 'inputs/candidate/artifacts/frame-0.rgb').read_bytes())
        result = subprocess.run([sys.executable, *receipt['reproduce'][1:]], cwd=self.output,
                                capture_output=True, text=True, check=False)
        self.assertEqual(0, result.returncode, result.stderr)
        replay = json.loads((self.root / 'new-comparison/receipt.json').read_text())
        self.assertEqual(receipt['difference'], replay['difference'])

    def test_pixel_weighted_neutral_pq_known_answer(self):
        self.candidate = self.make_manifest('candidate', samples=[[(32768 + n,) * 3 for n in range(4)]] * 2)
        score = self.compare()['difference']
        # For neutral PQ RGB, I equals input PQ and Ct/Cp vanish. The expected
        # step is defined by the 16-bit code grid, independent of this module.
        self.assertAlmostEqual(720 * 1.5 / 65535, score['delta_e_itp']['mean'], delta=1e-8)
        self.assertAlmostEqual(720 * 3 / 65535, score['delta_e_itp']['p95'], delta=1e-8)
        self.assertGreater(score['pu21_luma']['mse'], 0)
        self.assertTrue(math.isfinite(score['pu21_luma']['psnr_db']))

    def test_reference_errors_require_provenance_and_remain_unqualified(self):
        reference = self.make_manifest('independent-reference')
        receipt = self.compare(reference)
        self.assertEqual({'candidate', 'baseline'}, set(receipt['reference_error']))
        self.assertEqual('analytic-control', receipt['reference_provenance']['declared']['scope'])
        self.assertIn('independence and semantic validity declared', receipt['reference_provenance']['verification'])
        self.assertIsNone(receipt['capability_qualification'])

    def test_missing_independent_reference_provenance_refused(self):
        reference = self.make_manifest('independent-reference')
        self.edit(reference, lambda m: m.pop('provenance'))
        self.assert_refusal('independent provenance', reference)

    def test_reference_provenance_evidence_hash_mismatch_refused(self):
        reference = self.make_manifest('independent-reference')
        (reference.parent / 'method.txt').write_text('changed')
        self.assert_refusal('SHA256 mismatch', reference)

    def test_hash_mismatch_refuses_without_output(self):
        (self.candidate.parent / 'frame-0.rgb').write_bytes(b'x' * 24)
        self.assert_refusal('SHA256 mismatch')

    def test_rational_equivalence_is_exactly_paired(self):
        self.edit(self.candidate, lambda m: m['frames'][0].update(pts='0/100', duration='2/48'))
        self.edit(self.candidate, lambda m: m['frames'][1].update(pts='2/48', duration='3/72'))
        receipt = self.compare()
        self.assertEqual('1/24', receipt['difference']['frames'][1]['pts'])

    def test_shared_gap_does_not_claim_source_completeness(self):
        for path in (self.candidate, self.baseline):
            self.edit(path, lambda m: m['frames'][1].update(pts='10/24'))
        receipt = self.compare()
        self.assertEqual('unverified', receipt['association']['source_timeline_completeness'])
        self.assertEqual([{'start': '1/24', 'end': '5/12'}], receipt['association']['common_gaps'])
        self.assertEqual(0, receipt['difference']['delta_e_itp']['mean'])
        self.assertIsNone(receipt['capability_qualification'])

    def test_oversized_luminance_integer_refused_cleanly(self):
        self.edit(self.candidate, lambda m: m['target'].update(peak_nits=10**400))
        self.assert_refusal('PQ bounds')

    def test_misaligned_pts_refused(self):
        self.edit(self.candidate, lambda m: m['frames'][1].update(pts='1/23'))
        self.assert_refusal('association mismatch')

    def test_duration_mismatch_refused(self):
        self.edit(self.candidate, lambda m: m['frames'][1].update(duration='1/25'))
        self.assert_refusal('association mismatch')

    def test_missing_frame_refused(self):
        self.edit(self.candidate, lambda m: m['frames'].pop())
        self.assert_refusal('frame counts differ')

    def test_duplicate_or_overlapping_pts_refused(self):
        self.edit(self.candidate, lambda m: m['frames'][1].update(pts='0/1'))
        self.assert_refusal('duplicate, reordered or overlapping')

    def test_negative_duration_and_zero_denominator_refused(self):
        for change, expected in [({'duration': '-1/24'}, 'positive'), ({'pts': '1/0'}, 'denominator')]:
            self.candidate = self.make_manifest('candidate')
            self.edit(self.candidate, lambda m: m['frames'][0].update(change))
            self.assert_refusal(expected)

    def test_color_domain_and_target_mismatch_refused(self):
        for section, values, expected in [
            ('picture', {'primaries': 'bt709'}, 'only full-range'),
            ('picture', {'domain': 'reconstruction'}, 'domain/raster/color'),
            ('target', {'peak_nits': 2000}, 'target policy'),
            ('source', {'sha256': 'a' * 64}, 'source binding')]:
            self.candidate = self.make_manifest('candidate')
            self.edit(self.candidate, lambda m: m[section].update(values))
            self.assert_refusal(expected)

    def test_parent_and_absolute_path_escapes_refused(self):
        for name in ('../outside.rgb', '/outside.rgb', 'frame/../outside.rgb', 'frame\\outside.rgb'):
            self.candidate = self.make_manifest('candidate')
            self.edit(self.candidate, lambda m: m['frames'][0].update(path=name))
            self.assert_refusal('path')

    def test_symlink_inputs_and_output_parent_refused(self):
        frame = self.candidate.parent / 'frame-0.rgb'
        saved = frame.with_suffix('.saved')
        frame.rename(saved)
        frame.symlink_to(saved.name)
        self.assert_refusal('symlink')
        frame.unlink()
        saved.rename(frame)
        linked = self.root / 'linked'
        linked.symlink_to(self.candidate.parent, target_is_directory=True)
        with self.assertRaisesRegex(quality.Refusal, 'symlink'):
            quality.run(self.candidate, self.baseline, linked / 'out')

    def test_output_overwrite_refused_and_preserves_existing_data(self):
        self.output.mkdir()
        sentinel = self.output / 'keep'
        sentinel.write_text('do not erase')
        with self.assertRaisesRegex(quality.Refusal, 'overwrite refused'):
            self.compare()
        self.assertEqual('do not erase', sentinel.read_text())

    def test_raster_and_rawframe_and_manifest_bounds(self):
        self.edit(self.candidate, lambda m: m['picture'].update(width=257))
        self.assert_refusal('raster')
        self.candidate = self.make_manifest('candidate')
        short = b'\0' * 12
        (self.candidate.parent / 'frame-0.rgb').write_bytes(short)
        self.edit(self.candidate, lambda m: m['frames'][0].update(sha256=quality.digest(short)))
        self.assert_refusal('raw frame size')
        self.candidate.write_bytes(b' ' * (quality.MAX_MANIFEST_BYTES + 1))
        self.assert_refusal('byte allowance')

    def test_nonfinite_duplicate_key_and_wrong_json_types_refused(self):
        for raw, expected in [(' {"schema":1,"schema":1}', 'duplicate JSON'),
                              ('{"schema":NaN}', 'nonfinite'),
                              ('{"schema":1e999}', 'nonfinite'),
                              ('[]', 'schema')]:
            self.candidate.write_text(raw)
            self.assert_refusal(expected)

    def test_supplied_source_bytes_are_hash_verified_and_snapshotted(self):
        data = b'authored-neutral-control-v1'
        for path in (self.candidate, self.baseline):
            (path.parent / 'source.txt').write_bytes(data)
            self.edit(path, lambda m: m['source'].update(path='source.txt'))
        receipt = self.compare()
        self.assertIn('hash-verified-bytes', receipt['identities']['candidate']['source']['verification'])
        self.assertEqual(data, (self.output / 'inputs/candidate/artifacts/source.txt').read_bytes())


if __name__ == '__main__':
    unittest.main()
