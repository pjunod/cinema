"""Synthetic controls for exact failed-prepare accounting, never CI outcomes."""
import copy
import hashlib
import json
import unittest
from unittest.mock import patch

from validation import main_prepare_refusal_recovery as recovery
from validation.python_unit_receipts import ReceiptError


class API:
    def __init__(self, run):
        self.proof = copy.deepcopy(recovery.descriptor())
        self.case = next(row for row in self.proof['cases'] if row['run'] == run)
        self.scope = self.proof['scope']
        repo = {'id': 1, 'full_name': 'synthetic/repo'}
        event = {'number': 917, 'repository': repo, 'action': 'synchronized',
                 'pull_request': {'number': 917, 'state': 'open', 'draft': False,
                     'head': {'sha': self.case['commit'], 'ref': self.proof['branch'], 'repo': repo},
                     'base': {'sha': self.case['base'], 'ref': 'main', 'repo': repo}}}
        self.run = {'id': run, 'commit_sha': self.case['commit'], 'status': 'failure',
                    'event': 'pull_request', 'workflow_id': 'main-fast-lane.yml',
                    'repository': repo, 'event_payload': json.dumps(event)}
        self.case['event_sha256'] = hashlib.sha256(self.run['event_payload'].encode()).hexdigest()
        self.jobs = [{'id': row[0], 'name': row[1], 'task_id': row[2], 'attempt': row[3],
                      'status': row[4], 'run_id': run, 'repo_id': 1} for row in self.case['jobs']]
        self.sources = {path: b'synthetic-source:' + path.encode() for path in self.case['sources']}
        self.case['sources'] = {path: hashlib.sha256(raw).hexdigest() for path, raw in self.sources.items()}
        self.log = ('\n'.join([
            self.case['commit'] + ':refs/remotes/pull/917/head',
            'Main Python receipt refused: ' + self.case['reason'],
            *["skipping post step for '" + step + "'; main step was skipped" for step in (
                'Preserve per-ID preflight journal even on failure',
                'Preserve main Python success journal even on unit failure',
                'Publish preflight attempt-start journal',
                'Publish main Python attempt-start marker')],
            "Job 'fast policy and contract preflight' failed"]) + '\n').encode()
        self.refresh_log()
        self.artifacts = []
        self.marker = []

    def refresh_log(self):
        self.case['log_bytes'] = len(self.log)
        self.case['log_sha256'] = hashlib.sha256(self.log).hexdigest()

    def get(self, path):
        if path.endswith('/artifacts'):
            return self.artifacts
        return self.run

    def bytes(self, path, params=None):
        return self.sources[path.removeprefix('/raw/')] if path.startswith('/raw/') else self.log

    def pages(self, path, params):
        return self.marker if '-start-' in params['name'] else self.artifacts


class PrepareRefusalRecoveryCase(unittest.TestCase):
    def recover(self, api):
        with patch.object(recovery, 'descriptor', return_value=api.proof):
            return recovery.recover(api, api.scope, api.run, api.jobs)

    def test_exact_prepares_account_zero_without_importing_outcomes(self):
        for run in (4514, 4516, 4518):
            api = API(run)
            self.assertTrue(self.recover(api))
            self.assertEqual(api.artifacts, [])
            self.assertFalse(recovery.recover(api, api.scope, dict(api.run, id=9999), api.jobs))
            self.assertFalse(recovery.recover(api, dict(api.scope, pr=918), api.run, api.jobs))

    def test_source_event_artifact_and_execution_contradictions_refuse(self):
        for mode in ('source', 'commit', 'event', 'attempt', 'job', 'log', 'unit',
                     'ordering', 'artifact', 'marker', 'truncated'):
            with self.subTest(mode=mode):
                api = API(4514)
                if mode == 'source': api.sources[next(iter(api.sources))] += b'changed'
                if mode == 'commit': api.run['commit_sha'] = '0' * 40
                if mode == 'event': api.run['event_payload'] += ' '
                if mode == 'attempt': api.jobs[0]['attempt'] = 2
                if mode == 'job': api.jobs[0]['task_id'] += 1
                if mode == 'log': api.log += b'changed'
                if mode == 'unit':
                    api.log = api.log.replace(b"Job 'fast", b"MAIN-UNIT-new-success\nJob 'fast")
                    api.refresh_log()
                if mode == 'ordering':
                    api.log = api.log.replace(b'Main Python receipt refused: ', b'late refusal: ')
                    api.refresh_log()
                if mode == 'artifact': api.artifacts = [{'run_id': 4514}]
                if mode == 'marker': api.marker = [{'run_id': 4514}]
                if mode == 'truncated': api.log = api.log.rstrip(b'\n'); api.refresh_log()
                with self.assertRaises(ReceiptError): self.recover(api)
