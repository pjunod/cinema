"""Exact PR920 inherited-failure controls with synthetic authenticated APIs."""

import copy
import hashlib
import io
import json
import unittest
from unittest.mock import patch
import zipfile

from validation import python_unit_inherited_recovery920 as recovery
from validation import python_unit_receipts as receipts


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def archive(journal):
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w') as zipped:
        zipped.writestr('receipt.json', json.dumps(journal))
    return output.getvalue()


class InheritedAPI:
    def __init__(self):
        self.proof = copy.deepcopy(recovery.inherited_failure_case())
        self.scope = self.proof['scope']
        self.proof['count'] = 3
        self.baseline = {'validation:test_synthetic.Case.test_' + str(i): dict(self.proof['source'])
                         for i in range(3)}
        self.proof['passes_sha256'] = digest(json.dumps(self.baseline, sort_keys=True, separators=(',', ':')).encode())
        self.start = self.journal(4506, self.proof['commit'], False, self.baseline)
        self.final = copy.deepcopy(self.start)
        self.live = {}
        self.raw = {}
        self.refresh_archives()
        old = self.proof['source']
        self.raw[1944] = archive(self.journal(4503, old['commit'], False, {}))
        self.raw[1945] = archive(self.journal(4503, old['commit'], True, self.baseline))
        self.live[4503] = [self.metadata(1944, 4503, True), self.metadata(1945, 4503, False)]
        self.sources = {path: ('synthetic:' + path).encode() for path in recovery.SOURCE_PATHS}
        self.proof['source_hashes'] = {path: digest(raw) for path, raw in self.sources.items()}
        lines = ['Runner synthetic received task 17045 of job preflight, triggered by event: workflow_dispatch',
                 self.proof['commit'], self.proof['commit'], self.upload('start'),
                 'historical regression coverage is incomplete:',
                 '  - corrective client commit 8148ffea needs a tests/client-fixes.toml anchor row: '
                 'fix(validation): reconcile effort baseline evidence',
                 'make: *** [Makefile:140: history-check] Error 1', self.upload('final'),
                 "Job 'Python unit receipts' failed"]
        self.log = ('\n'.join(lines) + '\n').encode()
        self.resign_log()
        self.jobs = {4506: copy.deepcopy(self.proof['jobs']), 4503: [
            {'id': 45586, 'run_id': 4503, 'repo_id': 1, 'name': receipts.job_name(self.scope),
             'attempt': 1, 'status': 'failure'}]}
        self.runs = [{'id': 4506, 'commit_sha': self.proof['commit']},
                     {'id': 4503, 'commit_sha': old['commit']}]
        event = {'repository': {'id': 1}, 'ref': 'refs/heads/' + self.scope['branch'],
                 'workflow': '.github/workflows/effort-ci.yml'}
        self.actual = {'id': 4506, 'repository': {'id': 1}, 'commit_sha': self.proof['commit'],
                       'prettyref': self.scope['branch'], 'workflow_id': 'effort-ci.yml',
                       'status': 'failure', 'trigger_event': 'workflow_dispatch', 'event_payload': json.dumps(event)}

    def journal(self, run, commit, complete, passes):
        return {'version': 1, 'scope': copy.deepcopy(self.scope), 'run': run, 'commit': commit,
                'applicability_commit': commit, 'complete': complete,
                'passes': copy.deepcopy(passes), 'fixture_errors': []}

    def metadata(self, aid, run, start):
        return {'id': aid, 'run_id': run, 'size_in_bytes': len(self.raw[aid]), 'expired': False,
                'name': receipts.key(self.scope) + (f'-start-{run}' if start else '')}

    def refresh_archives(self):
        self.live[4506] = []
        for label, aid, journal in (('start', 1946, self.start), ('final', 1947, self.final)):
            raw = archive(journal); self.raw[aid] = raw
            self.proof['artifacts'][label].update(size=len(raw), sha256=digest(raw))
            self.live[4506].append(self.metadata(aid, 4506, label == 'start'))

    def upload(self, label):
        expected = self.proof['artifacts'][label]
        name = receipts.key(self.scope) + ('-start-4506' if label == 'start' else '')
        return f"Artifact {name} has been successfully uploaded! Final size is {expected['size']} bytes. Artifact ID is {expected['id']}"

    def resign_log(self):
        self.proof['log_bytes'], self.proof['log_sha256'] = len(self.log), digest(self.log)

    def get(self, path):
        if path == '/actions/runs/4506':
            return self.actual
        if path == '/actions/runs/4506/artifacts':
            return self.live[4506]
        raise AssertionError(path)

    def bytes(self, path, params=None):
        if path.startswith('/raw/'):
            return self.sources[path.removeprefix('/raw/')]
        if path == '/actions/jobs/45612/logs':
            return self.log
        return self.raw[int(path.split('/')[3])]

    def pages(self, path, query=None, field=None):
        if path == '/actions/runs':
            return self.runs
        if path.endswith('/jobs'):
            return self.jobs[int(path.split('/')[3])]
        if path == '/actions/artifacts':
            return [item for artifacts in self.live.values() for item in artifacts if item['name'] == query['name']]
        raise AssertionError(path)


class InheritedFailure920Tests(unittest.TestCase):
    def restore(self, api):
        with patch.object(recovery, 'inherited_failure_case', return_value=api.proof), \
                patch.object(receipts, 'legacy_pr663', return_value={}), \
                patch.object(receipts, 'local_receipts', return_value={}):
            return receipts.restore(api, api.scope, 999999)

    def test_exact_history_failure_preserves_original_trust_and_rejects_contradictions(self):
        with patch.object(recovery, 'PROOF_SHA256', '0' * 64):
            with self.assertRaises(receipts.ReceiptError):
                recovery.inherited_failure_case()
        api = InheritedAPI(); before = copy.deepcopy(api.__dict__)
        self.assertEqual(self.restore(api), api.baseline)
        self.assertEqual(api.__dict__, before)
        self.assertFalse(recovery.recover_inherited_pr920(api, {**api.scope, 'pr': 921}, api.runs[0], [], None, None, None, None, None))
        self.assertFalse(recovery.recover_inherited_pr920(api, api.scope, {'id': 4507}, [], None, None, None, None, None))
        mutations = ('source', 'log_hash', 'units', 'task', 'checkout', 'finding', 'phase_order',
                     'terminal', 'retry', 'missing_job', 'extra_job', 'running_job', 'actual_source',
                     'dispatch', 'missing_start', 'expired', 'artifact_hash', 'start_changed',
                     'reattributed', 'same_count_new_id', 'fixture_error', 'complete', 'extra_artifact',
                     'original_source_absent', 'failed_origin_trusted')
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                candidate = copy.deepcopy(api)
                if mutation == 'source':
                    candidate.sources['validation/python_unit_receipts.py'] += b'changed'
                elif mutation in ('log_hash', 'units', 'task', 'checkout', 'finding', 'phase_order', 'terminal'):
                    if mutation == 'log_hash': candidate.log += b'changed'
                    elif mutation == 'units': candidate.log += b'validation: discovered=1, historical-passes=0, pending=1\n'
                    elif mutation == 'task': candidate.log = candidate.log.replace(b'task 17045', b'task 17046')
                    elif mutation == 'checkout': candidate.log = candidate.log.replace(candidate.proof['commit'].encode(), b'0' * 40)
                    elif mutation == 'finding': candidate.log = candidate.log.replace(b'corrective client commit 8148ffea', b'other failure')
                    elif mutation == 'terminal': candidate.log = candidate.log.replace(b"Job 'Python unit receipts' failed", b'')
                    else:
                        lines = candidate.log.splitlines(); lines[3], lines[4] = lines[4], lines[3]
                        candidate.log = b'\n'.join(lines) + b'\n'
                    if mutation != 'log_hash': candidate.resign_log()
                elif mutation in ('retry', 'missing_job', 'extra_job', 'running_job'):
                    jobs = candidate.jobs[4506]
                    if mutation == 'retry': jobs[1]['attempt'] = 2
                    elif mutation == 'missing_job': jobs.pop()
                    elif mutation == 'extra_job': jobs.append({**jobs[-1], 'id': 7777})
                    else: jobs[1]['status'] = 'running'
                elif mutation == 'actual_source': candidate.actual['commit_sha'] = '0' * 40
                elif mutation == 'dispatch': candidate.actual['event_payload'] = json.dumps({'repository': {'id': 2}})
                elif mutation == 'missing_start': candidate.live[4506].pop(0)
                elif mutation == 'expired': candidate.live[4506][0]['expired'] = True
                elif mutation == 'artifact_hash': candidate.raw[1947] += b'changed'
                elif mutation == 'extra_artifact': candidate.live[4506].append({**candidate.live[4506][0], 'id': 7777})
                elif mutation == 'original_source_absent':
                    candidate.runs = [candidate.runs[0]]; candidate.live.pop(4503)
                elif mutation == 'failed_origin_trusted':
                    source = {'run': 4506, 'commit': candidate.proof['commit']}
                    bad = {'validation:test_synthetic.Case.test_new': source}
                    candidate.raw[2000] = archive(candidate.journal(4510, 'b' * 40, False, {}))
                    candidate.raw[2001] = archive(candidate.journal(4510, 'b' * 40, True, bad))
                    candidate.live[4510] = [candidate.metadata(2000, 4510, True), candidate.metadata(2001, 4510, False)]
                    candidate.runs.append({'id': 4510, 'commit_sha': 'b' * 40})
                    candidate.jobs[4510] = [{'id': 50000, 'run_id': 4510, 'repo_id': 1, 'attempt': 1,
                                             'name': receipts.job_name(candidate.scope), 'status': 'success'}]
                else:
                    old_uploads = [candidate.upload(label) for label in ('start', 'final')]
                    if mutation == 'start_changed': candidate.start['applicability_commit'] = '0' * 40
                    elif mutation == 'reattributed':
                        for journal in (candidate.start, candidate.final):
                            for source in journal['passes'].values():
                                source.update(run=4506, commit=candidate.proof['commit'])
                    elif mutation == 'same_count_new_id':
                        for journal in (candidate.start, candidate.final):
                            journal['passes']['validation:test_synthetic.Case.test_invented'] = journal['passes'].pop(next(iter(journal['passes'])))
                    elif mutation == 'fixture_error': candidate.final['fixture_errors'] = ['unresolved']
                    else: candidate.final['complete'] = True
                    candidate.refresh_archives()
                    for label, old in zip(('start', 'final'), old_uploads):
                        candidate.log = candidate.log.replace(old.encode(), candidate.upload(label).encode())
                    candidate.resign_log()
                with self.assertRaises(receipts.ReceiptError):
                    self.restore(candidate)
