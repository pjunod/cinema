"""Main migration evidence and unittest semantics fail closed."""
import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from validation import main_unit_receipts as main
from validation.python_unit_receipts import ReceiptError


class MainUnitReceiptsCase(unittest.TestCase):
    def test_authenticated_partial_journal_preserves_positive_passes(self):
        scope = {'repository': 1, 'pr': 7, 'branch': 'topic', 'base': 'main', 'workflow': main.WORKFLOW}
        commit = 'a' * 40
        test = 'validation:test_fixture.Case.test_ok'
        start = {'version': main.receipts.VERSION, 'scope': scope, 'run': 10, 'commit': commit,
                 'complete': False, 'passes': {}, 'fixture_errors': []}
        final = dict(start, passes={test: {'run': 10, 'commit': commit}})
        prior = {'id': 10, 'workflow_id': main.WORKFLOW, 'prettyref': '#7', 'event': 'pull_request',
                 'commit_sha': commit, 'event_payload': {'repository': {'id': 1}, 'number': 7,
                 'pull_request': {'number': 7, 'head': {'ref': 'topic', 'sha': commit, 'repo': {'id': 1}},
                                  'base': {'ref': 'main', 'repo': {'id': 1}}}}}

        class Applicability:
            def __call__(self, identity, attribution):
                return True
            def finish(self, passes):
                pass

        class API:
            status = 'failure'
            def pages(self, path, query=None, field=None):
                if path == '/actions/artifacts':
                    marker = query['name'].endswith('-start-10')
                    return [{'name': query['name'], 'expired': False, 'size_in_bytes': 1,
                             'run_id': 10, 'id': 2 if marker else 1}]
                if path == '/actions/runs':
                    return [prior]
                return [{'id': 3, 'name': main.JOB, 'repo_id': 1, 'run_id': 10,
                         'attempt': 1, 'status': self.status}]
            def bytes(self, path):
                return start if path.endswith('/2/zip') else final

        api = API()
        with patch.object(main.receipts, 'artifact_json', side_effect=lambda value: value):
            for status in ('failure', 'cancelled'):
                api.status = status
                with self.subTest(status=status):
                    self.assertEqual(main.restore(api, scope, 11, Applicability()), final['passes'])
            final['passes'][test] = {'run': 9, 'commit': commit}
            with self.assertRaises(ReceiptError):
                main.restore(api, scope, 11, Applicability())

    def test_exhaustive_dot_failure_retains_only_named_successes(self):
        ids = {'validation:test_fixture.Case.test_ok', 'validation:test_fixture.Case.test_bad'}
        log = b'FAIL: test_bad (test_fixture.Case.test_bad) (value=1)\nRan 2 tests in 0.01s\n\nFAILED (failures=1)\n'
        self.assertEqual(main.dot_passes(log, ids, 'validation'), {'validation:test_fixture.Case.test_ok'})
        for invalid in (
            log.replace(b'Ran 2', b'Ran 3'),
            log.replace(b'test_fixture.Case.test_bad', b'test_fixture.Other.test_bad'),
            log + log,
            log.replace(b'failures=1', b'errors=1'),
            log.replace(b'failures=1', b'failures=1, skipped=1'),
            log.replace(b'FAIL: test_bad', b'ERROR: test_bad'),
        ):
            with self.subTest(log=invalid), self.assertRaises(ReceiptError):
                main.dot_passes(invalid, ids, 'validation')

    def test_historical_custom_discovery_and_runner_are_refused(self):
        sources = (
            b'import unittest\ndef load_tests(*args): return None\nclass Case(unittest.TestCase):\n def test_one(self): pass\n',
            b'import unittest\nclass Case(unittest.TestCase):\n def run(self, result): pass\n def test_one(self): pass\n',
            b'import unittest\nclass Case(External):\n def test_one(self): pass\n',
            b'import unittest\nclass Case(unittest.TestCase):\n def test_one(self): pass\nsetattr(Case, "test_one", lambda self: None)\n',
            b'import unittest\nclass Case(unittest.TestCase):\n def test_one(self): pass\nmutate(Case)\n',
        )
        for raw in sources:
            with self.subTest(source=raw), patch.object(main.subprocess, 'check_output', return_value='tests/validation/test_fixture.py\n'), patch.object(main, 'source', return_value=raw), self.assertRaises(ReceiptError):
                main.inventory('a' * 40, 'validation')

    def test_event_authentication_rejects_other_pr_source_repository_and_workflow(self):
        scope = {'repository': 1, 'pr': 7, 'branch': 'topic', 'base': 'main', 'workflow': main.WORKFLOW}
        run = {'id': 10, 'workflow_id': main.WORKFLOW, 'prettyref': '#7', 'event': 'pull_request', 'commit_sha': 'a' * 40,
               'event_payload': {'repository': {'id': 1}, 'number': 7, 'pull_request': {
                   'number': 7, 'head': {'ref': 'topic', 'sha': 'a' * 40, 'repo': {'id': 1}},
                   'base': {'ref': 'main', 'repo': {'id': 1}}}}}
        main.authenticate_run(scope, run)
        for field, value in (('workflow_id', 'effort-ci.yml'), ('prettyref', '#8'), ('event', 'push'), ('commit_sha', 'b' * 40)):
            invalid = copy.deepcopy(run)
            invalid[field] = value
            with self.subTest(field=field), self.assertRaises(ReceiptError):
                main.authenticate_run(scope, invalid)
        invalid = copy.deepcopy(run)
        invalid['event_payload']['pull_request']['head']['repo']['id'] = 2
        with self.assertRaises(ReceiptError):
            main.authenticate_run(scope, invalid)

    def test_main_skip_is_retained_as_skip_and_never_as_pass(self):
        class Fixture(unittest.TestCase):
            def test_skip(self):
                self.skipTest('optional upstream checkout')
        with tempfile.TemporaryDirectory() as directory:
            journal = {'commit': 'a' * 40, 'run': 1, 'applicability_commit': 'a' * 40, 'passes': {}, 'fixture_errors': [], 'complete': False}
            suites = {name: [Fixture('test_skip')] for name in main.receipts.SUITES}
            self.assertEqual(main.execute(journal, Path(directory) / 'receipt.json', suites), 0)
            self.assertEqual(journal['passes'], {})
            self.assertEqual(len(journal['skips']), 2)
            self.assertTrue(journal['complete'])

    def test_main_failure_blocks_and_cached_success_never_executes(self):
        class Fixture(unittest.TestCase):
            def test_cached(self):
                raise AssertionError('a passed method must never execute again')
            def test_failed(self):
                self.fail('still failed')
        with tempfile.TemporaryDirectory() as directory:
            journal = {'commit': 'a' * 40, 'run': 1, 'applicability_commit': 'a' * 40, 'passes': {}, 'fixture_errors': [], 'complete': False}
            suites = {name: [Fixture('test_cached'), Fixture('test_failed')] for name in main.receipts.SUITES}
            for name in suites:
                journal['passes'][name + ':' + Fixture('test_cached').id()] = {'commit': 'a' * 40, 'run': 1}
            self.assertEqual(main.execute(journal, Path(directory) / 'receipt.json', suites), 1)
            self.assertEqual(len(journal['passes']), 2)

    def test_main_unknown_runner_outcome_refuses_complete_receipt(self):
        class Fixture(unittest.TestCase):
            def test_silent(self):
                pass
            def run(self, result):
                return result
        with tempfile.TemporaryDirectory() as directory:
            journal = {'commit': 'a' * 40, 'run': 1, 'applicability_commit': 'a' * 40, 'passes': {}, 'fixture_errors': [], 'complete': False}
            suites = {name: [Fixture('test_silent')] for name in main.receipts.SUITES}
            with self.assertRaises(ReceiptError):
                main.execute(journal, Path(directory) / 'receipt.json', suites)
            self.assertFalse(journal['complete'])
