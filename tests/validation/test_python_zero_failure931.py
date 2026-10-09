"""Synthetic evidence controls; never production CI outcome receipts."""
import copy
import hashlib
import io
import json
import unittest
from unittest.mock import patch
import zipfile

from validation import python_unit_receipts as receipts
from validation import python_unit_zero_failure931 as recovery


def archive(value):
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w') as zipped:
        zipped.writestr('receipt.json', json.dumps(value))
    return output.getvalue()


class API:
    def __init__(self, run):
        self.proof = copy.deepcopy(recovery.descriptor())
        self.scope = self.proof['scope']
        self.case = next(row for row in self.proof['cases'] if row['run'] == run)
        self.actual = {'id': run, 'repository': {'id': 1}, 'commit_sha': self.case['commit'],
                       'prettyref': self.scope['branch'], 'workflow_id': 'effort-ci.yml',
                       'status': 'failure', 'event_payload': '{"synthetic":true}'}
        self.case['event_sha256'] = hashlib.sha256(self.actual['event_payload'].encode()).hexdigest()
        self.jobs = [{'id': row[0], 'name': row[1], 'task_id': row[2], 'attempt': row[3],
                      'status': row[4], 'run_id': run, 'repo_id': 1} for row in self.case['jobs']]
        self.sources = {path: b'synthetic:' + path.encode() for path in self.case['sources']}
        self.case['sources'] = {p: hashlib.sha256(raw).hexdigest() for p, raw in self.sources.items()}
        self.journal = {'version': 1, 'scope': self.scope, 'run': run, 'commit': self.case['commit'],
                        'applicability_commit': self.case['commit'], 'complete': False,
                        'passes': {}, 'fixture_errors': []}
        self.artifacts = []
        self.refresh()

    def refresh(self):
        self.start_raw = self.final_raw = archive(self.journal)
        lines = [self.case['commit']]
        self.items = {}
        if self.case['run'] == 4522:
            for label, aid, name in (('start', 1957, receipts.key(self.scope) + '-start-4522'),
                                     ('final', 1958, receipts.key(self.scope))):
                self.case['artifacts'][label] = {'id': aid, 'size': len(self.start_raw),
                    'sha256': hashlib.sha256(self.start_raw).hexdigest()}
                self.items[label] = {'id': aid, 'name': name, 'run_id': 4522,
                                    'size_in_bytes': len(self.start_raw), 'expired': False}
                lines.append(f'Artifact {name} has been successfully uploaded! Final size is '
                             f'{len(self.start_raw)} bytes. Artifact ID is {aid}')
                if label == 'start':
                    lines += ['historical regression coverage is incomplete:',
                              'make: *** [Makefile:144: history-check] Error 1']
        else:
            lines += ['Python receipt refusal: ReceiptError: Incomplete receipt attempt 4522; '
                      'preserve artifact and recover individual evidence',
                      "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped",
                      "skipping post step for 'Publish Python attempt-start marker'; main step was skipped"]
        lines.append("Job 'Python unit receipts' failed")
        self.log = ('\n'.join(lines) + '\n').encode()
        self.refresh_log()

    def refresh_log(self):
        self.case['log_bytes'] = len(self.log)
        self.case['log_sha256'] = hashlib.sha256(self.log).hexdigest()

    def get(self, path):
        return self.artifacts if path.endswith('/artifacts') else self.actual

    def bytes(self, path, params=None):
        return self.sources[path.removeprefix('/raw/')] if path.startswith('/raw/') else self.log

    def pages(self, path, params):
        return self.artifacts


class ZeroFailure931Case(unittest.TestCase):
    def recover(self, api):
        inputs = () if api.case['run'] == 4523 else (
            api.items['start'], api.start_raw, api.items['final'], api.final_raw, api.journal)
        with patch.object(recovery, 'descriptor', return_value=api.proof):
            return recovery.recover(api, api.scope, api.actual, api.jobs, *inputs)

    def test_exact_history_and_prepare_refusal_preserve_zero_outcomes(self):
        for run in (4522, 4523):
            api = API(run)
            self.assertTrue(self.recover(api))
            self.assertFalse(api.journal['complete'])
            self.assertEqual(api.journal['passes'], {})
            self.assertFalse(recovery.recover(api, dict(api.scope, pr=932), api.actual, api.jobs))
            self.assertFalse(recovery.recover(api, api.scope, dict(api.actual, id=4524), api.jobs))

    def test_restore_accounts_both_attempts_without_completing_or_importing_journals(self):
        history, refusal = API(4522), API(4523)
        proof = history.proof
        proof['cases'][1] = refusal.case
        class Combined:
            def get(self, path):
                return [] if path.endswith('/artifacts') else (history.actual if '/4522' in path else refusal.actual)
            def bytes(self, path, params=None):
                if path.startswith('/raw/'):
                    api = history if params['ref'] == history.case['commit'] else refusal
                    return api.bytes(path, params)
                if path.startswith('/actions/artifacts/'):
                    return history.start_raw if '/1957/' in path else history.final_raw
                return history.log if '/45783/' in path else refusal.log
            def pages(self, path, params=None, field=None):
                if path == '/actions/runs': return [refusal.actual, history.actual]
                if path.endswith('/jobs'): return history.jobs if '/4522/' in path else refusal.jobs
                name = params['name']
                if name.endswith('-start-4522'): return [history.items['start']]
                if '-start-' in name: return []
                return [history.items['final']]
        with patch.object(recovery, 'descriptor', return_value=proof), \
             patch.object(receipts, 'legacy_pr663', return_value={}), \
             patch.object(receipts, 'local_receipts', return_value={}):
            self.assertEqual(receipts.restore(Combined(), history.scope, 9999), {})
        self.assertFalse(history.journal['complete'])
        self.assertEqual(history.journal['passes'], {})

    def test_changed_identity_evidence_execution_or_nonempty_maps_refuse(self):
        for mode in ('source', 'event', 'attempt', 'job', 'log', 'artifact', 'passes',
                     'fixture', 'unit', 'order', 'refusal_artifact', 'refusal_unit'):
            with self.subTest(mode=mode):
                api = API(4523 if mode.startswith('refusal_') else 4522)
                if mode == 'source': api.sources[next(iter(api.sources))] += b'changed'
                if mode == 'event': api.actual['event_payload'] += ' '
                if mode == 'attempt': api.jobs[0]['attempt'] = 2
                if mode == 'job': api.jobs[0]['task_id'] += 1
                if mode == 'log': api.log += b'changed'
                if mode == 'artifact': api.final_raw += b'changed'
                if mode == 'passes': api.journal['passes'] = {'validation:x.C.test_x': {'run': 4522, 'commit': api.case['commit']}}; api.refresh()
                if mode == 'fixture': api.journal['fixture_errors'] = ['setup']; api.refresh()
                if mode in ('unit', 'refusal_unit'):
                    api.log += b'discovered=1\n'; api.refresh_log()
                if mode == 'order':
                    api.log = ('\n'.join(reversed(api.log.decode().splitlines())) + '\n').encode(); api.refresh_log()
                if mode == 'refusal_artifact': api.artifacts = [{'run_id': 4523}]
                with self.assertRaises(receipts.ReceiptError): self.recover(api)
