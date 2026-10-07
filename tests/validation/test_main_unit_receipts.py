"""Main migration evidence and unittest semantics fail closed."""
import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from validation import main_unit_receipts as main
from validation.python_unit_receipts import ReceiptError


class MainUnitReceiptsCase(unittest.TestCase):
    def test_bridge_production_inputs_invalidate_only_the_changed_method(self):
        from validation import main_preflight_adoption as adoption
        current, original = 'a' * 40, 'b' * 40
        checker = object.__new__(main.DeclaredApplicability)
        checker.adoption, checker.commit = adoption, current
        checker.paths = {'tests/validation/test_fixture.py'}
        checker.families = [{'id_prefix': 'validation:test_fixture.', 'inputs': ['input'], 'reason': 'fixture'}]
        checker.source = unittest.mock.Mock(return_value=True)
        value = {'run': 3, 'commit': original}
        with patch.object(adoption, 'input_digest', side_effect=lambda commit, row: commit):
            self.assertFalse(checker('validation:test_fixture.Case.test_changed', value))
        with patch.object(adoption, 'input_digest', return_value='same'):
            self.assertTrue(checker('validation:test_fixture.Case.test_retained', value))
            checker.source.return_value = False
            self.assertFalse(checker('validation:test_fixture.Case.test_changed_fixture', value))
        checker.families = []
        with self.assertRaises(ReceiptError):
            checker('validation:test_fixture.Case.test_unknown', value)
        self.assertFalse(checker('validation:test_removed.Case.test_removed', value))

    def test_bridge_environment_requires_original_producer_and_matching_runtime(self):
        from validation import main_preflight_adoption as adoption
        env = {'platform': 'linux', 'machine': 'x86_64', 'python': [3, 12], 'node': 'v22.23.2'}
        journal = {'environment': env, 'producer_blob': 'a' * 40}
        commands = ['python3 -m validation.main_unit_receipts prepare',
                    'python3 -m validation.main_unit_receipts run --suite-dir tests/validation --suite-dir tests/operations',
                    'python3 -m validation.main_preflight_adoption node']
        with patch.object(adoption, 'environment', return_value=env), \
             patch.object(adoption, 'git', return_value='a' * 40), \
             patch.object(adoption, 'run_commands', return_value=commands):
            main.validate_bridge_environment(journal, journal, 'b' * 40)
            for changed in (dict(journal, producer_blob='c' * 40),
                            dict(journal, environment=dict(env, python=[3, 14]))):
                with self.assertRaises(ReceiptError):
                    main.validate_bridge_environment(journal, changed, 'b' * 40)
            with patch.object(adoption, 'run_commands', return_value=commands[:1]), self.assertRaises(ReceiptError):
                main.validate_bridge_environment(journal, journal, 'b' * 40)

    def test_bridge_retains_authenticated_origin_before_current_input_filtering(self):
        commit = 'a' * 40
        test = 'validation:test_fixture.Case.test_ok'
        scope = {'repository': 1, 'pr': 845, 'branch': 'topic', 'base': 'main', 'workflow': main.WORKFLOW}
        attribution = {'run': 5, 'commit': 'b' * 40}
        start = {'version': main.receipts.VERSION, 'scope': scope, 'run': 10, 'commit': commit,
                 'complete': False, 'passes': {test: attribution}, 'fixture_errors': []}
        prior = {'id': 10, 'commit_sha': commit}
        class Applicability:
            def __call__(self, identity, value): return False
            def finish(self, passes): pass
        class API:
            def pages(self, path, query=None, field=None):
                if path == '/actions/runs': return [prior]
                if path == '/actions/artifacts':
                    marker = query['name'].endswith('-start-10')
                    return [{'name': query['name'], 'expired': False, 'size_in_bytes': 1,
                             'run_id': 10, 'id': 2 if marker else 1}]
                return [{'id': 3, 'name': main.JOB, 'repo_id': 1, 'run_id': 10, 'attempt': 1, 'status': 'failure'}]
            def bytes(self, path): return start
        bridge = ({test: dict(attribution, job=4, attempt=1, outcome='success')}, {5})
        with patch.object(main, 'authenticate_run'), patch.object(main, 'validate_bridge_environment'), \
             patch.object(main.receipts, 'artifact_json', side_effect=lambda value: value):
            self.assertEqual(main.restore(API(), scope, 11, Applicability(), bridge), {})
            with self.assertRaises(ReceiptError):
                main.restore(API(), scope, 11, Applicability(), ({}, {5}))

    def test_bridge_does_not_replace_ordinary_main_python_applicability(self):
        with patch.object(main.receipts, 'SourceApplicability', return_value='ordinary') as checker, \
             patch.object(main, 'DeclaredApplicability', return_value='declared') as bridge:
            self.assertEqual(main.applicability_for({'repository': 1, 'pr': 7}, 'a' * 40), 'ordinary')
            self.assertEqual(main.applicability_for({'repository': 1, 'pr': 845}, 'a' * 40), 'declared')
            self.assertEqual(checker.call_count, 1)
            self.assertEqual(bridge.call_count, 1)

    def test_log_snapshots_require_complete_ordered_digest_bound_frames(self):
        import contextlib
        import io
        journal = {'passes': {'validation:test_fixture.Case.test_ok': {'run': 3, 'commit': 'a' * 40}}}
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            main.emit_snapshot(journal, 'start')
            main.emit_snapshot(dict(journal, complete=True), 'final')
        lines = output.getvalue().splitlines()
        self.assertEqual(main.read_snapshots(lines)['start'], journal)
        for invalid in (lines[:-1], lines + lines, [line.replace('CHUNK start 0 ', 'CHUNK start 1 ') for line in lines],
                        [line + 'A' if line.startswith('MAIN-UNIT-CHUNK') else line for line in lines]):
            with self.subTest(lines=invalid), self.assertRaises(ReceiptError):
                main.read_snapshots(invalid)

    def test_verbose_recovery_requires_exact_pending_ids_counts_and_real_ok(self):
        inherited = {'validation:test_validation.Case.test_cached': {'run': 2, 'commit': 'a' * 40}}
        inventories = {'validation': set(inherited), 'operations': {
            'operations:test_operations.Case.test_ok', 'operations:test_operations.Case.test_skip'}}
        lines = ['test_ok (test_operations.Case.test_ok) ... ok',
                 'test_skip (test_operations.Case.test_skip)', 'Documentation. ... skipped \'optional\'',
                 'Ran 2 tests in 0.01s', '', 'OK (skipped=1)',
                 'validation: discovered=1, historical-passes=1, pending=0',
                 'operations: discovered=2, historical-passes=0, pending=2']
        self.assertEqual(main.verbose_passes(lines, inventories, inherited), {'operations:test_operations.Case.test_ok'})
        for invalid in (lines + lines, lines[1:],
                        [line.replace('Ran 2', 'Ran 3') for line in lines],
                        [line.replace('test_operations.Case.test_ok', 'test_other.Case.test_ok') for line in lines],
                        [line.replace('OK (skipped=1)', 'OK') for line in lines],
                        [line.replace('historical-passes=1', 'historical-passes=0') for line in lines]):
            with self.subTest(lines=invalid), self.assertRaises(ReceiptError):
                main.verbose_passes(invalid, inventories, inherited)

    def test_verbose_recovery_refuses_unadmitted_runner_before_parsing_outcomes(self):
        api = unittest.mock.Mock()
        commit = 'a' * 40
        api.bytes.return_value = (commit + '\n' + commit + '\ntriggered by event: pull_request\n').encode()
        with patch.object(main, 'source', return_value=b'unsupported runner'), self.assertRaises(ReceiptError):
            main.recover_log(api, {}, {'id': 3, 'commit_sha': commit}, {'id': 4, 'status': 'success'}, [])

    def test_verbose_recovery_proves_explicit_methods_without_global_helper_inventory(self):
        commit = 'a' * 40
        inherited = [('validation:test_validation.Case.test_cached', {'run': 2, 'commit': commit})]
        log = ('\n'.join([commit, commit, 'triggered by event: pull_request',
            'test_ok (test_operations.Case.test_ok) ... ok', 'Ran 1 test in 0.01s', 'OK',
            'validation: discovered=1, historical-passes=1, pending=0',
            'operations: discovered=1, historical-passes=0, pending=1'])).encode()
        runner = b'admitted runner'
        workflow = b'run: python3 -m validation.main_unit_receipts run --suite-dir tests/validation --suite-dir tests/operations'
        class API:
            def bytes(self, path, query=None):
                return log if path.endswith('/logs') else workflow if path.endswith(main.WORKFLOW) else runner
        class Applicability:
            def __init__(self, *args, **kwargs): pass
            def __call__(self, test, value): return True
            def finish(self, passes): pass
            def fingerprint(self, commit, test):
                return 'fingerprint' if test == 'operations:test_operations.Case.test_ok' else None
        with patch.object(main, 'LEGACY_RUNNER_HASHES', {'validation/main_unit_receipts.py': main.hashlib.sha256(runner).hexdigest()}), \
             patch.object(main, 'source', side_effect=lambda commit, path: workflow if path.endswith(main.WORKFLOW) else runner), \
             patch.object(main.receipts, 'SourceApplicability', Applicability), \
             patch.object(main, 'inventory', side_effect=AssertionError('global helper discovery must not run')):
            journal = main.recover_log(API(), {}, {'id': 3, 'commit_sha': commit}, {'id': 4, 'status': 'success'}, inherited)
        self.assertEqual(journal['passes']['operations:test_operations.Case.test_ok'], {'run': 3, 'commit': commit})
        self.assertEqual(journal['passes'][inherited[0][0]], inherited[0][1])

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
