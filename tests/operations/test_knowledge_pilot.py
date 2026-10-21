"""Real pilot CLI boundaries; no lab, provider, receipt or product execution."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class KnowledgePilotSmokeTests(unittest.TestCase):
    def command(self, *args, expected=0):
        result = subprocess.run([str(ROOT / 'scripts/knowledge'), *args], cwd=ROOT,
                                env={**os.environ, 'PYTHONDONTWRITEBYTECODE': '1'},
                                capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, expected, result.stderr + result.stdout)
        return result

    def test_code_context_is_source_bound_and_utf8_bounded(self):
        result = self.command('context', '--path', 'crates/plurx-core/src/playback/caps.rs',
                              '--max-bytes', '8192')
        self.assertLessEqual(len(result.stdout.encode('utf-8')), 8192)
        answer = json.loads(result.stdout)
        self.assertIn(answer['status'], {'ok', 'partial'})
        self.assertTrue(answer['snapshot']['key'])
        self.assertEqual(answer['coverage']['level'], 'curated')
        self.assertTrue(any(r['kind'] in {'contract', 'obligation'} for r in answer['records']))
        self.assertTrue(any(r['provenance']['source_digest'] for r in answer['records']))

    def test_both_cohorts_return_real_multi_edge_paths(self):
        evidence_path = 'validation/main_unit_receipts.py'
        artifact = 'plurx:artifact:' + hashlib.sha256(evidence_path.encode()).hexdigest()[:20]
        for identity in ('plurx:route:server.learned-decode-limit-expiry', artifact):
            with self.subTest(identity=identity):
                answer = json.loads(self.command('why', identity, '--max-depth', '6').stdout)
                self.assertIn(answer['status'], {'ok', 'partial'})
                self.assertTrue(any(len(p['steps']) >= 2 for p in answer['relationship_paths']))
                for path in answer['relationship_paths']:
                    self.assertTrue(all(step['provenance']['source_digest'] for step in path['steps']))

    def test_named_check_is_not_authenticated_execution(self):
        # Classify the named check directly; the full relationship walk may
        # correctly refuse the default 8 KiB budget before returning records.
        response = self.command('evidence', 'plurx:check:routing.server.learned-decode-limit-expiry',
                                '--max-depth', '0', '--max-bytes', '32768')
        self.assertLessEqual(len(response.stdout.encode('utf-8')), 32768)
        answer = json.loads(response.stdout)
        self.assertIn(answer['status'], {'ok', 'partial'})
        evidence = answer['evidence_assessment']
        self.assertEqual(evidence['execution'], 'not-run')
        self.assertEqual(evidence['origin'], 'unavailable')
        self.assertFalse(evidence['authenticated_credit'])

    def test_invalid_supplied_capture_fails_without_mutating_body(self):
        directory = ROOT / 'target' / 'knowledge-tests'
        directory.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='body-', dir=directory) as owned:
            body = Path(owned) / 'description.md'
            text = 'Knowledge-Obligation: plurx:obligation:unknown-synthetic-id\n'
            body.write_text(text)
            result = self.command('lint', '--scope', 'pilot', '--body-file', str(body), '--base', 'HEAD', expected=1)
            self.assertTrue(json.loads(result.stdout)['errors'])
            self.assertEqual(body.read_text(), text)


if __name__ == '__main__':
    unittest.main()
