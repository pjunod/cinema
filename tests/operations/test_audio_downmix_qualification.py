"""Pin retained real synthetic receipts; this is not another media run."""
import hashlib
import json
import re
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / 'tests/fixtures/audio-downmix'


class AudioDownmixQualification(unittest.TestCase):
    def test_shipped_astats_crosschecks_each_side_of_all_twelve_outputs(self):
        rows = {}
        channels = {}
        for line in (FIXTURES / 'astats-crosscheck.txt').read_text().splitlines():
            filename, message = line.split(' ', 1)
            match = re.search(r'Channel: ([12])$', message)
            if match:
                channels[filename] = int(match[1]) - 1
                continue
            match = re.search(r'(Peak level dB|RMS level dB|Number of NaNs|Number of Infs): (-?[\d.]+)$', message)
            if match:
                side = channels[filename]
                rows.setdefault(filename, {}).setdefault(side, {}).setdefault(match[1], float(match[2]))
        self.assertEqual(len(rows), 12)
        pcm = json.loads((FIXTURES / 'pcm-results.json').read_text())
        aac = json.loads((FIXTURES / 'aac-results.json').read_text())
        margin = json.loads((FIXTURES / 'margin-results.json').read_text())
        for layout in pcm['layouts']:
            key = layout.replace('(side)', '-side')
            expected = {
                'pan-wav': pcm['layouts'][layout]['fixtures']['all']['outputs']['pan'],
                'limited-wav': pcm['layouts'][layout]['fixtures']['all']['outputs']['limited'],
                'limited-m4a': aac[key]['all']['limited'],
                'margin2-m4a': margin[key],
            }
            for suffix, values in expected.items():
                filename = f'/var/tmp/plurx-s09-m4.mKtfS7/{key}-all-{suffix}-astats.stderr'
                self.assertEqual(set(rows[filename]), {0, 1})
                for side, observed in rows[filename].items():
                    self.assertAlmostEqual(observed['Peak level dB'], values[side]['sample_peak_dbfs'], places=5)
                    self.assertAlmostEqual(observed['RMS level dB'], values[side]['rms_dbfs'], places=5)
                    self.assertEqual(observed['Number of NaNs'], 0)
                    self.assertEqual(observed['Number of Infs'], 0)

    def test_fc_source_and_each_output_side_are_measured(self):
        data = json.loads((FIXTURES / 'pcm-results.json').read_text())
        for layout, row in data['layouts'].items():
            fixture = row['fixtures']['fc']
            self.assertEqual(fixture['probe']['streams'][0]['channel_layout'], layout)
            source = fixture['source']
            self.assertAlmostEqual(source[2]['rms_dbfs'], -20, places=5)
            self.assertTrue(all(x['rms_dbfs'] is None for i, x in enumerate(source) if i != 2))
            for side in fixture['outputs']['pan']:
                self.assertLess(abs(side['integrated_lufs'] - (-23)), 1)
                self.assertAlmostEqual(side['rms_dbfs'] - source[2]['rms_dbfs'], -3.011611724, places=5)

    def test_pcm_and_aac_limiter_failure_and_margin_are_retained(self):
        pcm = json.loads((FIXTURES / 'pcm-results.json').read_text())
        aac = json.loads((FIXTURES / 'aac-results.json').read_text())
        margin = json.loads((FIXTURES / 'margin-results.json').read_text())
        for layout, row in pcm['layouts'].items():
            key = layout.replace('(side)', '-side')
            for side in row['fixtures']['all']['outputs']['pan']:
                self.assertGreater(side['sample_peak_dbfs'], 0)
                self.assertGreater(side['samples_at_or_above_full_scale'], 0)
            for side in aac[key]['all']['limited']:
                self.assertGreater(side['sample_peak_dbfs'], -1)
            for side in margin[key]:
                self.assertLessEqual(side['sample_peak_dbfs'], -1)
                self.assertEqual(side['samples_at_or_above_full_scale'], 0)

    def test_executed_collector_bytes_are_preserved(self):
        expected = {
            'collector.py': '2693e738a058f8e42181742640d9aebcac6baac1676a9859aed1451eccc12585',
            'aac.py': 'f3927f94e4727166cddf72a665e2b0953afd049ebecdb221ad3c9d82764f3e8c',
            'margin.py': '1287eae7e7fc561b9bb877ff96c952da97a3d52fa8ec27e6df36ca56268c684c',
            'astats.py': '86e804a79b0fb0b8d9eb0c1705450f36f5a4fbf2e84d897639828f87ad91f9fd',
        }
        for filename, digest in expected.items():
            path = ROOT / 'scripts/audio-downmix-qualification' / filename
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), digest)

    def test_raw_receipts_match_remote_capture_except_final_newline(self):
        expected = {
            'pcm-results.json': '18b18434509b5a182bd701353dd8972ebb59d159f54d8a6f46b453c08a0baa70',
            'aac-results.json': 'c46419f80d20a777eeafdf7bace85fa4e57db28363af38d18952bdfcb5b1f587',
            'margin-results.json': 'e27e7a58a07bd70f5eafda5100cf358d1135f932279c0e0fa4141362b361c035',
            'astats-commands.json': 'fd9868c04fbd2373d6ef127829352b061b66059871c537206ae365ad73d4a9f4',
        }
        for filename, digest in expected.items():
            self.assertEqual(hashlib.sha256((FIXTURES / filename).read_bytes().rstrip(b'\n')).hexdigest(), digest)


SCRIPTS = ROOT / 'scripts/audio-downmix-qualification'
EVIDENCE = ROOT / 'docs/evidence/audio-downmix-7-1-to-5-1-2026-10-05'
# What `DownmixMatrix::LimitedDefaultTo { target_channels: 6, .. }.filter()`
# returns (crates/plurx-core/src/playback/audio.rs).
FOLD_TO_5_1 = 'aformat=sample_fmts=fltp:channel_layouts=5.1,alimiter=limit=0.6309573444801932:level=0:latency=1'


def layout_harness():
    """layout.py's definitions without running it: the same prelude split the
    executed collectors use, with the final `main()` call left out."""
    namespace = {}
    argv = sys.argv
    sys.argv = ['layout.py', str(SCRIPTS)]
    try:
        exec((SCRIPTS / 'layout.py').read_text().rsplit('\nmain()\n', 1)[0], namespace)
    finally:
        sys.argv = argv
    return namespace


class TargetLayoutQualification(unittest.TestCase):
    """F-M2: the harness takes a target layout, and the 7.1 -> 5.1 receipt it
    produced is what justified the limited fold to 5.1."""

    @classmethod
    def setUpClass(cls):
        cls.results = json.loads((EVIDENCE / 'results.json').read_text())

    def test_renders_use_the_production_argv_for_each_target(self):
        harness = layout_harness()
        self.assertEqual(harness['encode_args']('aac', '5.1'),
                         ['-c:a', 'aac', '-ac', '6', '-channel_layout:a', '5.1', '-b:a', '320k', '-ar', '48000'])
        self.assertEqual(harness['encode_args']('eac3', '5.1')[-4:], ['-b:a', '640k', '-ar', '48000'])
        self.assertEqual(harness['encode_args']('ac3', '5.1')[-4:], ['-b:a', '640k', '-ar', '48000'])
        self.assertEqual(harness['encode_args']('aac', 'stereo'),
                         ['-c:a', 'aac', '-ac', '2', '-b:a', '160k', '-ar', '48000'])
        self.assertEqual(self.results['deliveries'],
                         {codec: harness['encode_args'](codec, '5.1') for codec in ('pcm_f32le', 'aac', 'eac3', 'ac3')})

    def test_a_target_that_does_not_fold_is_refused_before_any_media_work(self):
        harness = layout_harness()
        argv = sys.argv
        with tempfile.TemporaryDirectory() as tmp:
            sys.argv = ['layout.py', tmp, '--source-layout', '5.1', '--target-layout', '5.1']
            try:
                with self.assertRaisesRegex(SystemExit, 'more source than target'):
                    harness['main']()
            finally:
                sys.argv = argv
            self.assertEqual(list(Path(tmp).iterdir()), [])

    def test_receipt_came_from_the_committed_harness_bytes(self):
        environment = (EVIDENCE / 'environment.txt').read_text()
        for name in ('layout.py', 'collector.py'):
            digest = hashlib.sha256((SCRIPTS / name).read_bytes()).hexdigest()
            self.assertIn(f'{digest}  ', environment, name)
        self.assertIn('90004301255382e1beb441a294fa8b75ac7fbb54678837f224d3458032e38787', environment)

    def test_the_measured_candidate_is_the_shipped_fold(self):
        self.assertEqual(self.results['candidates'], {'limited_default_to': FOLD_TO_5_1})
        source = (ROOT / 'crates/plurx-core/src/playback/audio.rs').read_text()
        self.assertIn('"aformat=sample_fmts=fltp:channel_layouts={},{DOWNMIX_LIMIT}"', source)
        self.assertIn('6 => "5.1",', source)
        self.assertIn('"alimiter=limit=0.6309573444801932:level=0:latency=1"', source)
        self.assertNotIn('RequiresLayoutMeasurement {', source.split('fn stored_downmix')[0])

    def test_the_incumbent_fold_clips_on_a_real_title_and_the_worst_case(self):
        clipped = [label for label, scan in self.results['scans'].items()
                   if any(scan['channels'][name]['frames_at_or_above_0_dbfs'] for name in ('BL', 'BR'))]
        self.assertTrue(clipped, 'no whole-title scan shows the fold reaching full scale')
        for label in clipped:
            channels = self.results['scans'][label]['channels']
            # Only the folded pair goes over: the pass-through channels are
            # the source's own samples, which an integer decoder cannot clip.
            for name in ('FL', 'FR', 'FC', 'LFE'):
                self.assertEqual(channels[name]['frames_at_or_above_0_dbfs'], 0, (label, name))
        worst = self.results['synthetic']['all']['outputs']['incumbent']['pcm_f32le']['channels']
        self.assertTrue(all(row['samples_at_or_above_full_scale'] > 0 for row in worst[4:]))

    def test_the_shipped_fold_keeps_the_folded_pair_below_full_scale_in_every_codec(self):
        cases = {'synthetic ' + kind: row for kind, row in self.results['synthetic'].items()}
        cases.update(self.results['real'])
        self.assertGreaterEqual(len(self.results['real']), 25)
        for label, case in cases.items():
            limited, incumbent = case['outputs']['limited_default_to'], case['outputs']['incumbent']
            for row in limited['pcm_f32le']['channels']:
                if row['sample_peak_dbfs'] is not None:
                    self.assertLessEqual(row['sample_peak_dbfs'], -4 + 1e-3, (label, row))
            for codec in ('aac', 'eac3', 'ac3'):
                # The surround pair the fold sums never decodes at full scale.
                for row in limited[codec]['channels'][4:]:
                    self.assertEqual(row['samples_at_or_above_full_scale'], 0, (label, codec, row))
                # Pass-through channels keep AAC's own overshoot (a 5.1
                # source has it too); the limiter only ever reduces it.
                for mine, theirs in zip(limited[codec]['channels'], incumbent[codec]['channels']):
                    self.assertLessEqual(mine['samples_at_or_above_full_scale'],
                                         theirs['samples_at_or_above_full_scale'], (label, codec))

    def test_the_float_fold_is_the_incumbent_fold_wherever_the_limiter_is_idle(self):
        idle = 0
        for label, case in self.results['real'].items():
            for row in case['outputs']['limited_default_to']['against_incumbent_pcm']:
                if row['samples_cut_over_0_1_db'] == 0:
                    idle += 1
                    # Same swresample fold, in float: the gains are unchanged.
                    self.assertLess(row['max_abs_difference'], 1e-6, (label, row))
        self.assertGreater(idle, 0)

    def test_every_real_window_decoded_as_eight_channel_7_1_and_names_no_library_path(self):
        for label, case in self.results['real'].items():
            self.assertEqual(case['stream']['decoded'], {'channels': 8, 'channel_layout': '7.1'}, label)
        for path in EVIDENCE.iterdir():
            text = path.read_text()
            for private in ('/20t/', '/8tb/', '/8t-2/', '/media/', '.mkv'):
                self.assertNotIn(private, text, path.name)


if __name__ == '__main__':
    unittest.main()
