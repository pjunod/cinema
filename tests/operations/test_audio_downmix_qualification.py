"""Pin retained real synthetic receipts; this is not another media run."""
import hashlib
import json
import re
from pathlib import Path
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


if __name__ == '__main__':
    unittest.main()
