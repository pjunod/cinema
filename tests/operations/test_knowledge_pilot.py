"""Real pilot CLI boundaries; no lab, provider, receipt or product execution."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


class KnowledgePilotSmokeTests(unittest.TestCase):
    def command(self, *args, expected=0):
        result = subprocess.run([str(ROOT / 'scripts/knowledge'), *args], cwd=ROOT,
                                env={**os.environ, 'PYTHONDONTWRITEBYTECODE': '1'},
                                capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, expected, result.stderr + result.stdout)
        return result

    def context_answer(self, code_path):
        budget, ceiling = 8192, 64 * 1024
        for _ in range(4):
            result = self.command('context', '--path', code_path, '--max-depth', '6',
                                  '--max-bytes', str(budget))
            self.assertLessEqual(len(result.stdout.encode('utf-8')), budget)
            answer = json.loads(result.stdout)
            self.assertIsInstance(answer, dict)
            if answer.get('status') != 'budget-too-small':
                self.assertIn(answer.get('status'), {'ok', 'partial'})
                return answer
            self.assertEqual(set(answer), {'status', 'required_bytes_at_least'})
            required = answer['required_bytes_at_least']
            self.assertIs(type(required), int)  # A JSON boolean is not a byte bound.
            self.assertGreater(required, budget)
            self.assertLessEqual(required, ceiling, 'context exceeds the smoke budget ceiling')
            # The advertised size is a lower bound, not a promise that the next
            # complete source/provenance record will fit at exactly that size.
            budget = min(ceiling, max(required, 2 * budget))
        self.fail('context did not return a source-bound answer within four bounded requests')

    def test_code_context_is_source_bound_and_utf8_bounded(self):
        answer = self.context_answer('crates/plurx-core/src/playback/caps.rs')
        self.assertIn(answer['status'], {'ok', 'partial'})
        self.assertTrue(answer['snapshot']['key'])
        self.assertEqual(answer['coverage']['level'], 'curated')
        self.assertTrue(any(r['kind'] in {'contract', 'obligation'} for r in answer['records']))
        self.assertTrue(any(r['provenance']['source_digest'] for r in answer['records']))

    def test_both_cohorts_return_real_multi_edge_paths(self):
        import hashlib

        # Context prioritizes direct obligations; multi-hop paths can be on
        # later bounded pages. Section 5 requires multi-hop discovery in why.
        for code_path in ('crates/plurx-core/src/playback/caps.rs', 'validation/main_unit_receipts.py'):
            with self.subTest(path=code_path):
                answer = self.context_answer(code_path)
                self.assertIn(answer['status'], {'ok', 'partial'})
                self.assertEqual(answer['coverage']['level'], 'curated')
                self.assertTrue(answer['records'])
                self.assertTrue(all(record['provenance']['source_digest'] for record in answer['records']))

        artifact = 'plurx:artifact:' + hashlib.sha256(b'validation/main_unit_receipts.py').hexdigest()[:20]
        for identity in ('plurx:route:server.learned-decode-limit-expiry', artifact):
            with self.subTest(identity=identity):
                response = self.command('why', identity, '--max-depth', '6', '--max-bytes', '32768')
                self.assertLessEqual(len(response.stdout.encode('utf-8')), 32768)
                answer = json.loads(response.stdout)
                self.assertIn(answer['status'], {'ok', 'partial'})
                self.assertTrue(any(len(path['steps']) >= 2 for path in answer['relationship_paths']))
                for path in answer['relationship_paths']:
                    self.assertTrue(all(step['provenance']['source_digest'] for step in path['steps']))

    def test_context_negotiation_respects_lower_bounds_and_utf8_bytes(self):
        source_answer = {'status': 'partial', 'snapshot': {'key': 'synthetic'},
                         'coverage': {'level': 'curated'},
                         'records': [{'kind': 'contract',
                                      'provenance': {'source_digest': 'synthetic-source'}}]}
        replies = [dict(status='budget-too-small', required_bytes_at_least=20000),
                   dict(status='budget-too-small', required_bytes_at_least=20001), source_answer]
        with mock.patch.object(self, 'command', side_effect=[
                subprocess.CompletedProcess([], 0, stdout=json.dumps(reply)) for reply in replies]) as command:
            self.assertEqual(self.context_answer('synthetic/source.rs'), source_answer)
        budgets = [int(call.args[call.args.index('--max-bytes') + 1]) for call in command.call_args_list]
        self.assertEqual(budgets, [8192, 20000, 40000])
        oversized = json.dumps({'status': 'ok', 'text': '雪' * 3000}, ensure_ascii=False)
        self.assertLess(len(oversized), 8192)  # Character count would miss this overflow.
        with mock.patch.object(self, 'command', return_value=subprocess.CompletedProcess([], 0, stdout=oversized)) as command:
            with self.assertRaises(AssertionError):
                self.context_answer('synthetic/source.rs')
            self.assertEqual(command.call_count, 1)

    def test_context_negotiation_refuses_invalid_or_exhausted_budgets(self):
        refusals = [dict(status='budget-too-small'),
                    dict(status='budget-too-small', required_bytes_at_least=True),
                    dict(status='budget-too-small', required_bytes_at_least='8193'),
                    dict(status='budget-too-small', required_bytes_at_least=8192),
                    dict(status='budget-too-small', required_bytes_at_least=8193, records=[]),
                    dict(status='budget-too-small', required_bytes_at_least=65537)]
        for refusal in refusals:
            with self.subTest(refusal=refusal), mock.patch.object(self, 'command',
                    return_value=subprocess.CompletedProcess([], 0, stdout=json.dumps(refusal))) as command:
                with self.assertRaises(AssertionError):
                    self.context_answer('synthetic/source.rs')
                self.assertEqual(command.call_count, 1)
        budgets = []
        def still_too_small(*args):
            budget = int(args[args.index('--max-bytes') + 1])
            budgets.append(budget)
            return subprocess.CompletedProcess([], 0, stdout=json.dumps(
                dict(status='budget-too-small', required_bytes_at_least=budget + 1)))
        with mock.patch.object(self, 'command', side_effect=still_too_small):
            with self.assertRaises(AssertionError):
                self.context_answer('synthetic/source.rs')
        self.assertEqual(budgets, [8192, 16384, 32768, 65536])

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
