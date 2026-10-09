"""Synthetic controls; no production CI outcomes or network requests."""
import copy
import hashlib
import json
import unittest
from unittest.mock import patch
from validation import main_source_skew4513 as recovery
from validation import main_preflight_adoption as adoption
from validation import main_unit_receipts as main
from validation.python_unit_receipts import ReceiptError


class Evidence:
    def __init__(self, run):
        self.proof = copy.deepcopy(recovery.descriptor())
        self.case = self.proof if run == 4513 else self.proof['chain']
        self.scope = {'repository': 1, 'pr': 917}
        repo = {'id': 1, 'full_name': 'synthetic/repo'}
        head = self.proof['event_head'] if run == 4513 else self.case['commit']
        event = {'number': 917, 'repository': repo, 'action': 'synchronized',
                 'pull_request': {'number': 917, 'state': 'open', 'draft': False,
                     'head': {'sha': head, 'ref': self.proof['branch'], 'repo': repo},
                     'base': {'sha': self.case['base'], 'ref': 'main', 'repo': repo}}}
        self.actual = dict(id=run, repository=repo, commit_sha=self.case['commit'],
                           prettyref='#917', workflow_id='main-fast-lane.yml',
                           event='pull_request', status='failure', event_payload=json.dumps(event))
        self.case['event_sha256'] = hashlib.sha256(self.actual['event_payload'].encode()).hexdigest()
        self.jobs = [dict(id=row[0], name=row[1], task_id=row[2], attempt=row[3], status=row[4],
                          run_id=run, repo_id=1) for row in self.case['jobs']]
        self.sources = {path: ('synthetic:' + path).encode() for path in self.case['sources']}
        self.case['sources'] = {path: hashlib.sha256(raw).hexdigest() for path, raw in self.sources.items()}
        reason = ('Main receipt must match an open same-repository PR head' if run == 4513
                  else 'Prior event/source/PR/base/readiness binding mismatch')
        self.log = ('\n'.join([self.case['commit'] + ':refs/remotes/pull/917/head',
            'Main Python receipt refused: ' + reason,
            *["skipping post step for '" + step + "'; main step was skipped" for step in (
                'Preserve per-ID preflight journal even on failure',
                'Preserve main Python success journal even on unit failure',
                'Publish preflight attempt-start journal', 'Publish main Python attempt-start marker')],
            "Job 'fast policy and contract preflight' failed"]) + '\n').encode()
        self.refresh_log()
        self.artifacts = []
        self.markers = []

    def refresh_log(self):
        self.case['log_bytes'] = len(self.log)
        self.case['log_sha256'] = hashlib.sha256(self.log).hexdigest()

    def get(self, path):
        return self.artifacts if path.endswith('/artifacts') else self.actual

    def bytes(self, path, query=None):
        return self.sources[path.removeprefix('/raw/')] if path.startswith('/raw/') else self.log

    def pages(self, path, query=None, field=None):
        if path == '/actions/runs': return [self.actual]
        if path.endswith('/jobs'): return self.jobs
        return self.markers if '-start-' in query['name'] else self.artifacts

    def recover(self):
        with patch.object(recovery, 'descriptor', return_value=self.proof):
            return recovery.recover(self, self.scope, self.actual, self.jobs)


class SourceSkew4513Case(unittest.TestCase):
    def test_exact_skew_and_refusal_import_zero_and_preserve_original_event(self):
        for run in (4513, 4540):
            evidence = Evidence(run)
            original = copy.deepcopy(evidence.actual)
            self.assertTrue(evidence.recover())
            self.assertEqual(evidence.actual, original)
            self.assertEqual(evidence.artifacts, [])
            self.assertFalse(recovery.recover(evidence, dict(evidence.scope, pr=918), evidence.actual, evidence.jobs))
            self.assertFalse(recovery.recover(evidence, evidence.scope, dict(evidence.actual, id=4541), evidence.jobs))
        skew = Evidence(4513)
        with self.assertRaises(ReceiptError): adoption.bind_event(skew.actual, skew.scope)

    def test_source_event_job_artifact_and_execution_contradictions_refuse(self):
        for run in (4513, 4540):
            for mode in ('source', 'commit', 'event', 'attempt', 'job', 'log', 'unit',
                         'ordering', 'artifact', 'marker', 'truncated', 'repo', 'branch'):
                with self.subTest(run=run, mode=mode):
                    evidence = Evidence(run)
                    if mode == 'source': evidence.sources[next(iter(evidence.sources))] += b'changed'
                    if mode == 'commit': evidence.actual['commit_sha'] = '0' * 40
                    if mode == 'event': evidence.actual['event_payload'] += ' '
                    if mode == 'attempt': evidence.jobs[0]['attempt'] = 2
                    if mode == 'job': evidence.jobs[0]['task_id'] += 1
                    if mode == 'log': evidence.log += b'changed'
                    if mode == 'unit':
                        evidence.log = evidence.log.replace(b"Job 'fast", b"MAIN-UNIT-success\nJob 'fast")
                        evidence.refresh_log()
                    if mode == 'ordering':
                        evidence.log = evidence.log.replace(b'Main Python receipt refused: ', b'late refusal: ')
                        evidence.refresh_log()
                    if mode == 'artifact': evidence.artifacts = [{'run_id': run}]
                    if mode == 'marker': evidence.markers = [{'run_id': run}]
                    if mode == 'truncated': evidence.log = evidence.log.rstrip(b'\n'); evidence.refresh_log()
                    if mode == 'repo': evidence.actual['repository'] = {'id': 2}
                    if mode == 'branch': evidence.scope['branch'] = 'foreign'
                    with self.assertRaises(ReceiptError): evidence.recover()

    def test_main_restore_accounts_exact_empty_attempts_before_binding_without_passes(self):
        class Applicability:
            def __call__(self, *_): raise AssertionError('No successes may be imported')
            def finish(self, passes): self.passes = passes
        for run in (4513, 4540):
            evidence = Evidence(run)
            applicability = Applicability()
            with patch.object(recovery, 'descriptor', return_value=evidence.proof):
                self.assertEqual(main.restore(evidence, evidence.scope, 9999, applicability), {})
            self.assertEqual(applicability.passes, {})
            # A real final artifact cannot take this zero-outcome shortcut.
            evidence.artifacts = [dict(run_id=run, name=main.key(evidence.scope), expired=False,
                                       size_in_bytes=1)]
            with patch.object(recovery, 'descriptor', return_value=evidence.proof):
                with self.assertRaises(ReceiptError): main.restore(evidence, evidence.scope, 9999, Applicability())


class PrepareChainEvidence(Evidence):
    def __init__(self):
        super().__init__(4540)
        self.run = 6000
        self.actual['id'] = self.run
        for job in self.jobs:
            job['run_id'] = self.run
        self.prior = copy.deepcopy(self.actual)
        self.log = self.log.replace(b'Prior event/source/PR/base/readiness binding mismatch',
            b'Missing final journal for prior attempt 5999; preserve possibly passed IDs')
        self.sources = {path: ('synthetic:' + path).encode() for path in recovery.PREPARE_PRODUCERS}
        self.accepted = {path: [hashlib.sha256(raw).hexdigest()] for path, raw in self.sources.items()}
        self.ancestor = True
        self.current_run = '9000'

    def context(self):
        import contextlib
        stack = contextlib.ExitStack()
        stack.enter_context(patch.object(recovery, 'PREPARE_PRODUCERS', self.accepted))
        stack.enter_context(patch.object(recovery, 'prepare_ancestor', return_value=self.ancestor))
        stack.enter_context(patch.dict('os.environ', GITHUB_RUN_ID=self.current_run, GITHUB_SHA='d' * 40))
        return stack

    def recover(self):
        with self.context():
            return recovery.recover(self, self.scope, self.prior, self.jobs)


class PrepareRefusalChainCase(unittest.TestCase):
    def test_reviewed_prepare_dependency_chain_imports_zero_and_keeps_event(self):
        evidence = PrepareChainEvidence()
        original = copy.deepcopy(evidence.prior)
        self.assertTrue(evidence.recover())
        self.assertEqual(evidence.prior, original)
        self.assertEqual(evidence.artifacts, [])
        evidence.log = evidence.log.replace(
            b'Missing final journal for prior attempt 5999; preserve possibly passed IDs',
            b'Main receipt must match an open same-repository PR head')
        self.assertTrue(evidence.recover())
        evidence.log = evidence.log.replace(b'Main receipt must match an open same-repository PR head',
                                            b'Unknown unrelated refusal')
        self.assertFalse(evidence.recover())

    def test_prepare_chain_source_identity_execution_and_artifact_contradictions_refuse(self):
        modes = ('source', 'event', 'event_source', 'commit', 'repo', 'branch',
                 'attempt', 'missing_job', 'job_status', 'job_run', 'duplicate_job',
                 'truncated', 'unit', 'uploaded', 'ordering', 'artifact', 'marker',
                 'ancestor', 'current_run', 'self_dependency')
        for mode in modes:
            with self.subTest(mode=mode):
                evidence = PrepareChainEvidence()
                if mode == 'source': evidence.sources[next(iter(evidence.sources))] += b'changed'
                if mode == 'event': evidence.actual['event_payload'] += ' '
                if mode == 'event_source':
                    event = json.loads(evidence.actual['event_payload'])
                    event['pull_request']['head']['sha'] = '0' * 40
                    evidence.actual['event_payload'] = json.dumps(event)
                    evidence.prior = copy.deepcopy(evidence.actual)
                if mode == 'commit': evidence.actual['commit_sha'] = '0' * 40
                if mode == 'repo': evidence.actual['repository'] = {'id': 2}
                if mode == 'branch': evidence.scope['branch'] = 'foreign'
                if mode == 'attempt': evidence.jobs[0]['attempt'] = 2
                if mode == 'missing_job': evidence.jobs.pop()
                if mode == 'job_status': evidence.jobs[3]['status'] = 'success'
                if mode == 'job_run': evidence.jobs[0]['run_id'] += 1
                if mode == 'duplicate_job': evidence.jobs.append(copy.deepcopy(evidence.jobs[0]))
                if mode == 'truncated': evidence.log = evidence.log.rstrip(b'\n')
                if mode == 'unit': evidence.log = evidence.log.replace(b"Job 'fast", b"MAIN-UNIT-success\nJob 'fast")
                if mode == 'uploaded': evidence.log = evidence.log.replace(b"Job 'fast", b"has been successfully uploaded!\nJob 'fast")
                if mode == 'ordering':
                    evidence.log = evidence.log.replace(b'Main Python receipt refused:', b'late refusal:')
                if mode == 'artifact': evidence.artifacts = [{'run_id': evidence.run}]
                if mode == 'marker': evidence.markers = [{'run_id': evidence.run}]
                if mode == 'ancestor': evidence.ancestor = False
                if mode == 'current_run': evidence.current_run = str(evidence.run)
                if mode == 'self_dependency': evidence.log = evidence.log.replace(b'attempt 5999;', b'attempt 6000;')
                if mode == 'ordering':
                    self.assertFalse(evidence.recover())
                else:
                    with self.assertRaises(ReceiptError): evidence.recover()

    def test_prepare_chain_restore_imports_no_successes_and_future_helper_stays_bound(self):
        from pathlib import Path
        class Applicability:
            def __call__(self, *_): raise AssertionError('No successes may be imported')
            def finish(self, passes): self.passes = passes
        evidence = PrepareChainEvidence()
        applicability = Applicability()
        with evidence.context():
            self.assertEqual(main.restore(evidence, evidence.scope, 9000, applicability), {})
        self.assertEqual(applicability.passes, {})
        path = 'validation/main_preflight_adoption.py'
        evidence.sources[path] = b'from validation.main_source_skew4513 import recover'
        evidence.accepted[path] = [hashlib.sha256(evidence.sources[path]).hexdigest()]
        evidence.sources['validation/main_source_skew4513.py'] = Path(recovery.__file__).read_bytes()
        self.assertTrue(evidence.recover())
        evidence.sources['validation/main_source_skew4513.py'] += b'unknown producer'
        with self.assertRaises(ReceiptError): evidence.recover()
