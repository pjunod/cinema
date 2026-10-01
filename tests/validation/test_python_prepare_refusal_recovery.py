"""Bounded synthetic API evidence; never actual CI blobs or unit imports."""
import copy
import hashlib
import io
import unittest
from unittest.mock import patch

from validation import python_unit_receipts as receipts


def digest(data):
    return hashlib.sha256(data).hexdigest()


class SyntheticPrepareAPI:
    def __init__(self):
        self.scope = {'repository': 7, 'pr': 42, 'branch': 'codex/synthetic', 'base': 'effort/example'}
        self.commit = 'a' * 40
        self.sources = {'.github/workflows/effort-ci.yml': b'synthetic reviewed workflow ordering',
                        'validation/python_unit_receipts.py': b'synthetic reviewed prepare ordering'}
        self.log = (self.commit + '\nPython receipt refusal: ReceiptError: Local pass receipts await authenticated PR hash attestation\n'
                    "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped\n"
                    "skipping post step for 'Publish Python attempt-start marker'; main step was skipped\n").encode()
        self.case = {'version': 1, 'scope': copy.deepcopy(self.scope), 'run': 10, 'job': 20,
                     'commit': self.commit, 'log_sha256': digest(self.log),
                     'source_hashes': {path: digest(data) for path, data in self.sources.items()},
                     'provenance': 'synthetic zero-unit proof, not actual CI'}
        self.prior = {'id': 10, 'commit_sha': self.commit}
        self.actual = {'id': 10, 'repository': {'id': 7}, 'commit_sha': self.commit,
                       'prettyref': self.scope['branch'], 'workflow_id': 'effort-ci.yml', 'status': 'failure'}
        self.jobs = [{'id': 20, 'run_id': 10, 'repo_id': 7, 'attempt': 1,
                      'name': receipts.job_name(self.scope), 'status': 'failure'}]
        self.final, self.start, self.run_artifacts = [], [], []

    def pages(self, path, query=None, field=None):
        if path == '/actions/artifacts':
            return self.start if '-start-' in query['name'] else self.final
        if path == '/actions/runs':
            return [self.prior]
        if path == '/actions/runs/10/jobs':
            return self.jobs
        raise AssertionError(path)

    def get(self, path):
        if path == '/actions/runs/10':
            return self.actual
        if path == '/actions/runs/10/artifacts':
            return self.run_artifacts
        raise AssertionError(path)

    def bytes(self, path, query=None):
        if path.startswith('/raw/'):
            assert query == {'ref': self.commit}
            return self.sources[path.removeprefix('/raw/')]
        if path == '/actions/jobs/20/logs':
            return self.log
        raise AssertionError(path)


class PrepareRefusalRecoveryTests(unittest.TestCase):
    def restore(self, api):
        with patch.object(receipts, 'prepare_refusal_case', return_value=api.case), \
             patch.object(receipts, 'local_receipts', return_value={}), \
             patch.object(receipts, 'legacy_pr663', return_value={}), \
             patch.object(receipts, 'discover') as discovery, \
             patch('sys.stdout', new=io.StringIO()) as output:
            result = receipts.restore(api, api.scope, 11)
            discovery.assert_not_called()
            return result, output.getvalue()

    def test_actual_restore_recognizes_only_bound_zero_unit_prepare_and_imports_no_successes(self):
        api = SyntheticPrepareAPI()
        restored, output = self.restore(api)
        self.assertEqual(restored, {})
        self.assertIn('zero units, no successes imported', output)
        self.assertEqual(api.final, [])
        self.assertEqual(api.start, [])
        self.assertNotIn('complete', api.case)

    def test_actual_restore_refuses_unknown_metadata_source_log_artifact_and_unit_phase(self):
        self.assertEqual(self.restore(SyntheticPrepareAPI())[0], {})
        modes = ('scope', 'unknown', 'prior_source', 'run_status', 'workflow', 'repository',
                 'job_attempt', 'duplicate_job', 'source', 'log_hash', 'unit_phase',
                 'discovery_phase', 'missing_skip', 'skip_order', 'start', 'final', 'run_artifact')
        for mode in modes:
            with self.subTest(mode=mode):
                api = SyntheticPrepareAPI()
                if mode == 'scope': api.case['scope']['pr'] = 43
                elif mode == 'unknown': api.case['run'] = 9
                elif mode == 'prior_source': api.prior['commit_sha'] = 'b' * 40
                elif mode == 'run_status': api.actual['status'] = 'success'
                elif mode == 'workflow': api.actual['workflow_id'] = 'other.yml'
                elif mode == 'repository': api.actual['repository']['id'] = 8
                elif mode == 'job_attempt': api.jobs[0]['attempt'] = 2
                elif mode == 'duplicate_job': api.jobs.append(copy.deepcopy(api.jobs[0]))
                elif mode == 'source': api.sources['validation/python_unit_receipts.py'] += b'tamper'
                elif mode == 'log_hash': api.log += b'tamper'
                elif mode == 'unit_phase': api.log += b'Ran 1 test\n'; api.case['log_sha256'] = digest(api.log)
                elif mode == 'discovery_phase': api.log += b'discovered=1\n'; api.case['log_sha256'] = digest(api.log)
                elif mode == 'missing_skip':
                    api.log = api.log.replace(b'Publish Python attempt-start marker', b'other step')
                    api.case['log_sha256'] = digest(api.log)
                elif mode == 'skip_order':
                    lines = api.log.splitlines(keepends=True)
                    api.log = b''.join([lines[0], lines[2], lines[3], lines[1]])
                    api.case['log_sha256'] = digest(api.log)
                elif mode == 'start': api.start = [{'id': 30}]
                elif mode == 'final':
                    api.final = [{'id': 31, 'run_id': 10, 'name': receipts.key(api.scope),
                                  'expired': False, 'size_in_bytes': 100}]
                elif mode == 'run_artifact': api.run_artifacts = [{'id': 32}]
                with self.assertRaises(receipts.ReceiptError):
                    self.restore(api)
