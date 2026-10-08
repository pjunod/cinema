"""Main migration evidence and unittest semantics fail closed."""
import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from validation import main_unit_receipts as main
from validation.python_unit_receipts import ReceiptError


class MainUnitReceiptsCase(unittest.TestCase):
    def preunit4431_fixture(self):
        import json
        proof = copy.deepcopy(main.PREUNIT4431)
        scope = {'repository': 1, 'pr': 888, 'branch': proof['branch'], 'base': 'main', 'workflow': main.WORKFLOW}
        repo = {'id': 1, 'full_name': 'noirr/plurx'}
        event = {'repository': repo, 'number': 888, 'action': 'synchronized', 'pull_request': {
            'number': 888, 'draft': False, 'state': 'open',
            'head': {'repo': repo, 'sha': proof['commit'], 'ref': proof['branch']},
            'base': {'repo': repo, 'ref': 'main', 'sha': proof['base']}}}
        prior = {'id': proof['run'], 'repository': repo, 'commit_sha': proof['commit'],
                 'workflow_id': main.WORKFLOW, 'event': 'pull_request',
                 'event_payload': json.dumps(event), 'status': 'cancelled'}
        jobs = [{'id': identity, 'name': name, 'task_id': task, 'status': status,
                 'run_id': proof['run'], 'repo_id': 1, 'attempt': 2}
                for identity, name, task, status in proof['jobs']]
        logs = {}
        attempts = []
        for attempt, task, _, _, reason in proof['attempts']:
            lines = [f'Runner fixture received task {task} of job preflight, triggered by event: pull_request',
                     proof['commit'] + ':refs/remotes/pull/888/head', 'node: v22.23.2',
                     'Main Python receipt refused: ' + reason]
            lines += ["skipping post step for '" + step + "'; main step was skipped" for step in (
                'Preserve per-ID preflight journal even on failure',
                'Preserve main Python success journal even on unit failure',
                'Publish preflight attempt-start journal',
                'Publish main Python attempt-start marker')]
            lines += [f"Job '{main.JOB}' failed"]
            raw = ('\n'.join(lines) + '\n').encode()
            logs[attempt] = raw
            attempts.append((attempt, task, len(raw), main.hashlib.sha256(raw).hexdigest(), reason))
        proof['attempts'] = tuple(attempts)
        proof['sources'] = {path: main.hashlib.sha256(b'reviewed source').hexdigest() for path in proof['sources']}
        class API:
            def __init__(self):
                self.run, self.logs = copy.deepcopy(prior), dict(logs)
                self.producer, self.run_artifacts, self.artifacts = b'reviewed source', [], []
                self.requested_attempts = []
            def get(self, path):
                if path == '/actions/runs/4431': return self.run
                if path == '/actions/runs/4431/artifacts': return self.run_artifacts
                raise AssertionError(path)
            def pages(self, path, query):
                assert path == '/actions/artifacts'
                return self.artifacts
            def bytes(self, path, query=None):
                if path == '/actions/jobs/44940/logs':
                    assert set(query) == {'attempt'}
                    self.requested_attempts.append(query['attempt'])
                    return self.logs[query['attempt']]
                assert path.startswith('/raw/') and query == {'ref': proof['commit']}
                return self.producer
        return proof, scope, prior, jobs, API()

    def test_exact_prepare_retry_authenticates_both_attempts_and_imports_no_outcomes(self):
        proof, scope, prior, jobs, api = self.preunit4431_fixture()
        with patch.object(main, 'PREUNIT4431', proof), patch.object(main, 'source', return_value=b'reviewed source'), \
             patch.object(main.receipts, 'atomic_json') as writer:
            self.assertIs(main.recover_preunit4431(api, scope, prior, jobs), True)
            self.assertEqual(api.requested_attempts, [1, 2])
            self.assertIs(main.recover_preunit4431(api, dict(scope, pr=889), prior, jobs), False)
            self.assertIs(main.recover_preunit4431(api, scope, dict(prior, id=4432), jobs), False)
            writer.assert_not_called()

    def test_prepare_retry_refuses_changed_metadata_sources_and_any_artifacts(self):
        proof, scope, prior, jobs, api = self.preunit4431_fixture()
        with patch.object(main, 'PREUNIT4431', proof), patch.object(main, 'source', return_value=b'reviewed source'):
            for changed in (jobs[:-1], jobs + jobs[:1],
                            [dict(job, attempt=3) for job in jobs],
                            [dict(job, repo_id=2) for job in jobs],
                            [dict(job, task_id=99999) if job['id'] == proof['job'] else job for job in jobs],
                            [dict(job, status='running') if job['id'] == proof['job'] else job for job in jobs]):
                with self.subTest(jobs=changed), self.assertRaises(ReceiptError):
                    main.recover_preunit4431(api, scope, prior, changed)
            for changed in (dict(prior, status='running'), dict(prior, commit_sha='b' * 40),
                            dict(prior, state='failure'), dict(prior, workflow_id='other.yml')):
                api.run = changed
                with self.subTest(run=changed), self.assertRaises(ReceiptError):
                    main.recover_preunit4431(api, scope, prior, jobs)
            api.run = prior
            api.producer = b'different remote source'
            with self.assertRaises(ReceiptError): main.recover_preunit4431(api, scope, prior, jobs)
            api.producer, api.run_artifacts = b'reviewed source', [{'id': 3}]
            with self.assertRaises(ReceiptError): main.recover_preunit4431(api, scope, prior, jobs)
            api.run_artifacts, api.artifacts = [], [{'run_id': proof['run']}]
            with self.assertRaises(ReceiptError): main.recover_preunit4431(api, scope, prior, jobs)

    def test_prepare_retry_first_attempt_execution_or_incomplete_log_still_refuses(self):
        proof, scope, prior, jobs, api = self.preunit4431_fixture()
        originals = dict(api.logs)
        with patch.object(main, 'source', return_value=b'reviewed source'):
            for attempt in (1, 2):
                # The digest and the phase proof are independent controls.
                for raw in (b'', originals[attempt][:-1],
                            originals[attempt].replace(b'node: v22.23.2', b'MAIN-UNIT-JOURNAL start'),
                            originals[attempt].replace(b'node: v22.23.2', b'Main preflight outcome success'),
                            originals[attempt].replace(b'node: v22.23.2', b'Ran 1 test in 0.01s'),
                            originals[attempt].replace(b'node: v22.23.2', b'Artifact receipt has been successfully uploaded!'),
                            originals[attempt].replace(b'Publish main Python attempt-start marker', b'other step'),
                            originals[3 - attempt]):
                    api.logs = {**originals, attempt: raw}
                    with self.subTest(attempt=attempt, raw=raw), patch.object(main, 'PREUNIT4431', proof), \
                         self.assertRaises(ReceiptError):
                        main.recover_preunit4431(api, scope, prior, jobs)
                    altered = copy.deepcopy(proof)
                    altered['attempts'] = tuple((n, task, len(raw), main.hashlib.sha256(raw).hexdigest(), reason)
                                                if n == attempt else row
                                                for row in proof['attempts'] for n, task, _, _, reason in (row,))
                    with self.subTest(phase_attempt=attempt, raw=raw), patch.object(main, 'PREUNIT4431', altered), \
                         self.assertRaises(ReceiptError):
                        main.recover_preunit4431(api, scope, prior, jobs)

    def test_proven_empty_retry_preserves_older_actual_successes(self):
        proof = main.PREUNIT4431
        scope = {'repository': 1, 'pr': 888, 'branch': proof['branch'], 'base': 'main', 'workflow': main.WORKFLOW}
        older = {'id': 4429, 'commit_sha': 'b' * 40}
        retried = {'id': 4431, 'commit_sha': proof['commit']}
        test = 'validation:test_fixture.Case.test_ok'
        value = {'run': 4429, 'commit': older['commit_sha']}
        journal = {'version': main.receipts.VERSION, 'scope': scope, 'run': 4429,
                   'commit': older['commit_sha'], 'complete': True, 'fixture_errors': [], 'passes': {test: value}}
        class API:
            def pages(self, path, query=None, field=None):
                if path == '/actions/artifacts': return []
                if path == '/actions/runs': return [retried, older]
                run = int(path.split('/')[-2])
                return [{'id': run, 'name': main.JOB, 'repo_id': 1, 'run_id': run,
                         'attempt': 2 if run == 4431 else 1, 'status': 'failure', 'task_id': run}]
            def bytes(self, path, query=None): return b'validation.main_unit_receipts'
        class Applicability:
            def __call__(self, identity, attribution): return True
            def finish(self, passes): pass
        with patch.object(main, 'authenticate_run'), patch.object(main, 'unexecuted_preflight', return_value=False), \
             patch.object(main, 'recover_preunit4431', side_effect=lambda api, scope, run, jobs: run['id'] == 4431), \
             patch.object(main, 'recover_log', return_value=journal) as recover:
            self.assertEqual(main.restore(API(), scope, 4432, Applicability()), {test: value})
            self.assertEqual(recover.call_args.args[2], older)

    def test_exact_upload_failure_retains_only_original_incomplete_start(self):
        import contextlib
        import io
        import json
        proof = copy.deepcopy(main.PREUNIT_UPLOAD4324)
        scope = {'repository': 1, 'pr': 845, 'branch': proof['branch'], 'base': 'main', 'workflow': main.WORKFLOW}
        repo = {'id': 1, 'full_name': 'owner/repository'}
        event = {'repository': repo, 'number': 845, 'action': 'synchronized', 'pull_request': {
            'number': 845, 'draft': False, 'state': 'open',
            'head': {'repo': repo, 'sha': proof['commit'], 'ref': proof['branch']},
            'base': {'repo': repo, 'ref': 'main', 'sha': proof['base']}}}
        prior = {'id': proof['run'], 'repository': repo, 'commit_sha': proof['commit'],
                 'workflow_id': main.WORKFLOW, 'event': 'pull_request',
                 'event_payload': json.dumps(event), 'status': 'failure'}
        job = {'id': proof['job'], 'run_id': proof['run'], 'repo_id': 1, 'attempt': 1,
               'task_id': proof['task'], 'name': main.JOB, 'status': 'failure'}
        test = 'validation:test_fixture.Case.test_ok'
        start = {'version': main.receipts.VERSION, 'scope': scope, 'run': proof['run'],
                 'commit': proof['commit'], 'complete': False, 'fixture_errors': [],
                 'passes': {test: {'run': 4315, 'commit': 'b' * 40}}}
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            main.emit_snapshot(start, 'start')
        lines = output.getvalue().splitlines()
        for _ in range(3):
            lines += ['Beginning upload of artifact content to blob storage',
                      '::error::Error runner api getting task: task is not running%0A']
        lines += [f"Job '{main.JOB}' failed"]
        raw = ('\n'.join(lines) + '\n').encode()
        proof.update(log_bytes=len(raw), log_sha256=main.hashlib.sha256(raw).hexdigest(),
                     sources={'validation/main_unit_receipts.py': main.hashlib.sha256(b'reviewed source').hexdigest()})
        class API:
            log = raw
            run = prior
            producer = b'reviewed source'
            markers = []
            run_artifacts = []
            def get(self, path):
                return self.run_artifacts if path.endswith('/artifacts') else self.run
            def pages(self, path, query=None):
                return [job] if path.endswith('/jobs') else self.markers
            def bytes(self, path, query=None):
                return self.log if path.endswith('/logs') else self.producer
        api = API()
        with patch.object(main, 'PREUNIT_UPLOAD4324', proof), patch.object(main, 'source', return_value=b'reviewed source'), \
             patch.object(main, 'validate_bridge_environment'):
            recovered = main.recover_log(api, scope, prior, job, [])
            self.assertEqual(recovered, start)
            self.assertIs(recovered['complete'], False)
            self.assertEqual(recovered['passes'][test]['run'], 4315)
            self.assertIsNone(main.recover_preunit_upload4324(api, scope, dict(prior, id=4323), job))
            for changed in (dict(job, status='running'), dict(job, attempt=2), dict(job, task_id=0),
                            dict(job, id=44111), dict(job, repo_id=2)):
                with self.subTest(job=changed), self.assertRaises(ReceiptError):
                    main.recover_preunit_upload4324(api, scope, prior, changed)
            api.log = raw + b'Ran 1 test in 0.01s\n'
            with self.assertRaises(ReceiptError):
                main.recover_preunit_upload4324(api, scope, prior, job)
            api.log, api.producer = raw, b'changed remote source'
            with self.assertRaises(ReceiptError):
                main.recover_preunit_upload4324(api, scope, prior, job)
            api.producer, api.markers = b'reviewed source', [{'run_id': proof['run']}]
            with self.assertRaises(ReceiptError):
                main.recover_preunit_upload4324(api, scope, prior, job)
            api.markers, api.run_artifacts = [], [{'id': 1}]
            with self.assertRaises(ReceiptError):
                main.recover_preunit_upload4324(api, scope, prior, job)
            api.run_artifacts, api.run = [], dict(prior, status='success')
            with self.assertRaises(ReceiptError):
                main.recover_preunit_upload4324(api, scope, prior, job)

    def test_failed_upload_start_requires_authenticated_origin_then_current_applicability(self):
        commit = 'a' * 40
        scope = {'repository': 1, 'pr': 845, 'branch': 'topic', 'base': 'main', 'workflow': main.WORKFLOW}
        prior = {'id': 4324, 'commit_sha': commit}
        test = 'validation:test_fixture.Case.test_ok'
        value = {'run': 4315, 'commit': 'b' * 40}
        start = {'version': main.receipts.VERSION, 'scope': scope, 'run': 4324, 'commit': commit,
                 'complete': False, 'passes': {test: value}, 'fixture_errors': []}
        class API:
            def pages(self, path, query=None, field=None):
                if path == '/actions/artifacts': return []
                if path == '/actions/runs': return [prior]
                return [{'id': 44110, 'name': main.JOB, 'repo_id': 1, 'run_id': 4324,
                         'attempt': 1, 'status': 'failure', 'task_id': 16574}]
            def bytes(self, path, query=None): return b'validation.main_unit_receipts'
        class Applicability:
            applicable = True
            def __call__(self, identity, origin): return self.applicable
            def finish(self, passes): pass
        bridge = ({test: dict(value, job=44000, attempt=1, outcome='success')}, {4315, 4324})
        checker = Applicability()
        with patch.object(main, 'authenticate_run'), patch.object(main, 'validate_bridge_environment'), \
             patch.object(main, 'unexecuted_preflight', return_value=False), \
             patch.object(main, 'recover_preunit_upload4324', return_value=start):
            self.assertEqual(main.restore(API(), scope, 4325, checker, bridge), {test: value})
            checker.applicable = False
            self.assertEqual(main.restore(API(), scope, 4325, checker, bridge), {})
            with self.assertRaisesRegex(ReceiptError, 'authenticated provenance'):
                main.restore(API(), scope, 4325, checker, ({}, {4324}))
        self.assertIs(start['complete'], False)

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
            def bytes(self, path, query=None):
                if path.endswith(main.WORKFLOW): return b'validation.main_unit_receipts'
                return start
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

    def test_failed_pre_receipt_run_still_uses_legacy_bootstrap(self):
        scope = {'repository': 1, 'pr': 7, 'branch': 'topic', 'base': 'main', 'workflow': main.WORKFLOW}
        commit = 'a' * 40
        prior = {'id': 10, 'workflow_id': main.WORKFLOW, 'prettyref': '#7', 'event': 'pull_request',
                 'commit_sha': commit, 'event_payload': {'repository': {'id': 1}, 'number': 7,
                 'pull_request': {'number': 7, 'head': {'ref': 'topic', 'sha': commit, 'repo': {'id': 1}},
                                  'base': {'ref': 'main', 'repo': {'id': 1}}}}}
        expected = {'validation:test_fixture.Case.test_ok': {'run': 10, 'commit': commit}}
        class API:
            def pages(self, path, query=None, field=None):
                if path == '/actions/artifacts': return []
                if path == '/actions/runs': return [prior]
                return [{'id': 3, 'name': main.JOB, 'repo_id': 1, 'run_id': 10,
                         'attempt': 1, 'status': 'failure', 'task_id': 7}]
            def bytes(self, path, query=None):
                return b'Ran 2 tests in 0.01s' if path.endswith('/logs') else b'legacy workflow'
        class Applicability:
            def __call__(self, test, value): return True
            def finish(self, passes): pass
        with patch.object(main, 'bootstrap', return_value=expected) as bootstrap, \
             patch.object(main, 'unexecuted_preflight', side_effect=AssertionError('legacy has no receipt source')):
            self.assertEqual(main.restore(API(), scope, 11, Applicability()), expected)
            bootstrap.assert_called_once()

    def test_cancelled_unassigned_preflight_does_not_require_test_evidence(self):
        scope = {'repository': 1, 'pr': 7, 'branch': 'topic', 'base': 'main', 'workflow': main.WORKFLOW}
        class API:
            markers = []
            def pages(self, path, query=None):
                return self.markers
        api = API()
        prior = {'id': 3, 'commit_sha': 'a' * 40}
        self.assertTrue(main.unexecuted_preflight(api, scope, prior, {'status': 'cancelled', 'task_id': 0}))
        for job in ({'status': 'cancelled'}, {'status': 'cancelled', 'task_id': 1},
                    {'status': 'cancelled', 'task_id': False}, {'status': 'running', 'task_id': 0}):
            with self.subTest(job=job):
                self.assertFalse(main.unexecuted_preflight(api, scope, prior, job))
        api.markers = [{'id': 9}]
        with self.assertRaises(ReceiptError):
            main.unexecuted_preflight(api, scope, prior, {'status': 'cancelled', 'task_id': 0})

    def test_prepare_refusal_requires_admitted_source_and_complete_phase_evidence(self):
        scope = {'repository': 1, 'pr': 7, 'branch': 'topic', 'base': 'main', 'workflow': main.WORKFLOW}
        commit = 'a' * 40
        runner = b'reviewed prepare-before-tests source'
        lines = [commit, commit, 'triggered by event: pull_request',
                 'Main Python receipt refused: missing prior evidence',
                 "skipping post step for 'Publish main Python attempt-start marker'; main step was skipped",
                 f"Job '{main.JOB}' failed"]
        class API:
            log = '\n'.join(lines).encode()
            raw = runner
            def bytes(self, path, query=None):
                return self.log if path.endswith('/logs') else self.raw
            def pages(self, path, query=None):
                return []
        api = API()
        prior, job = {'id': 3, 'commit_sha': commit}, {'id': 4, 'status': 'failure', 'task_id': 5}
        with patch.object(main, 'PREPARE_REFUSAL_HASHES', {'validation/main_unit_receipts.py': main.hashlib.sha256(runner).hexdigest()}), \
             patch.object(main, 'source', return_value=runner):
            self.assertTrue(main.unexecuted_preflight(api, scope, prior, job))
            for invalid in (lines[:-1], lines[1:], lines[:4] + lines[5:], lines + [lines[3]],
                            lines[:3] + lines[4:5] + lines[3:4] + lines[5:],
                            lines + ['MAIN-UNIT-JOURNAL start 1 ' + '0' * 64]):
                api.log = '\n'.join(invalid).encode()
                with self.subTest(lines=invalid):
                    self.assertFalse(main.unexecuted_preflight(api, scope, prior, job))
            api.log = '\n'.join(lines).encode()
            api.raw = b'different remote source'
            self.assertFalse(main.unexecuted_preflight(api, scope, prior, job))

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
            task_id = 7
            def pages(self, path, query=None, field=None):
                if path == '/actions/artifacts':
                    marker = query['name'].endswith('-start-10')
                    return [{'name': query['name'], 'expired': False, 'size_in_bytes': 1,
                             'run_id': 10, 'id': 2 if marker else 1}]
                if path == '/actions/runs':
                    return [prior]
                return [{'id': 3, 'name': main.JOB, 'repo_id': 1, 'run_id': 10,
                         'attempt': 1, 'status': self.status, 'task_id': self.task_id}]
            def bytes(self, path, query=None):
                if path.endswith(main.WORKFLOW): return b'validation.main_unit_receipts'
                return start if path.endswith('/2/zip') else final

        api = API()
        with patch.object(main.receipts, 'artifact_json', side_effect=lambda value: value):
            for status in ('failure', 'cancelled'):
                api.status = status
                with self.subTest(status=status):
                    self.assertEqual(main.restore(api, scope, 11, Applicability()), final['passes'])
            api.task_id = 0
            with self.assertRaisesRegex(ReceiptError, 'Unassigned preflight has final journal'):
                main.restore(api, scope, 11, Applicability())
            api.task_id = 7
            final['passes'][test] = {'run': 9, 'commit': commit}
            with self.assertRaises(ReceiptError):
                main.restore(api, scope, 11, Applicability())

    def test_legacy_retry_imports_latest_bound_log_but_receipt_retries_are_refused(self):
        scope = {'repository': 1, 'pr': 7, 'branch': 'topic', 'base': 'main', 'workflow': main.WORKFLOW}
        commit = 'a' * 40
        prior = {'id': 10, 'workflow_id': main.WORKFLOW, 'prettyref': '#7', 'event': 'pull_request',
                 'commit_sha': commit, 'event_payload': {'repository': {'id': 1}, 'number': 7,
                 'pull_request': {'number': 7, 'head': {'ref': 'topic', 'sha': commit, 'repo': {'id': 1}},
                                  'base': {'ref': 'main', 'repo': {'id': 1}}}}}
        passes = {'validation:test_fixture.Case.test_ok': {'run': 10, 'commit': commit}}
        class API:
            workflow = b'legacy unittest discovery'
            attempt = 2
            status = 'failure'
            def pages(self, path, query=None, field=None):
                if path == '/actions/artifacts': return []
                if path == '/actions/runs': return [prior]
                return [{'id': 3, 'name': main.JOB, 'repo_id': 1, 'run_id': 10,
                         'attempt': self.attempt, 'status': self.status}]
            def bytes(self, path, query=None):
                return b'Ran 2 tests in 0.01s' if path.endswith('/logs') else self.workflow
        class Applicability:
            def __call__(self, identity, attribution): return True
            def finish(self, passes): pass
        api = API()
        with patch.object(main, 'bootstrap', return_value=passes) as bootstrap:
            self.assertEqual(main.restore(api, scope, 11, Applicability()), passes)
            self.assertEqual(bootstrap.call_args.args[3]['attempt'], 2)
            api.workflow = b'validation.main_unit_receipts'
            for status in ('failure', 'skipped'):
                api.status = status
                with self.subTest(status=status), self.assertRaisesRegex(ReceiptError, 'ambiguous artifact identity'):
                    main.restore(api, scope, 11, Applicability())
            api.workflow = b'legacy unittest discovery'
            for value in (0, -1, True, '2'):
                api.attempt = value
                with self.subTest(attempt=value), self.assertRaises(ReceiptError):
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
