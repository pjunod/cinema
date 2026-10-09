"""Synthetic adversarial evidence controls; no HTTP or actual units replayed."""

import copy
import hashlib
import io
import json
import unittest
from unittest.mock import patch
import zipfile

from validation import python_unit_empty_recovery as recovery
from validation import python_unit_receipts as receipts


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def archive(journal):
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, 'w') as zipped:
        zipped.writestr('receipt.json', json.dumps(journal))
    return stream.getvalue()


class EmptyAPI:
    def __init__(self):
        self.proof = copy.deepcopy(recovery.empty_failure_case())
        self.scope = self.proof['scope']
        self.journals = {}
        self.raw = {}
        self.artifacts = {}
        self.logs = {}
        self.actual = {}
        self.jobs = {}
        self.runs = []
        self.sources = {path: ('synthetic:' + path).encode() for path in recovery.SOURCE_PATHS}
        for case in self.proof['cases']:
            run = case['run']
            self.runs.append({'id': run, 'commit_sha': case['commit']})
            self.jobs[run] = copy.deepcopy(case['jobs'])
            case['source_hashes'] = {path: digest(raw) for path, raw in self.sources.items()}
            event = {'repository': {'id': 1}, 'ref': 'refs/heads/' + self.scope['branch'],
                     'workflow': '.github/workflows/effort-ci.yml'}
            self.actual[run] = {'id': run, 'repository': {'id': 1}, 'commit_sha': case['commit'],
                                'prettyref': self.scope['branch'], 'workflow_id': 'effort-ci.yml',
                                'status': 'cancelled', 'trigger_event': 'workflow_dispatch',
                                'event_payload': json.dumps(event)}
            self.artifacts[run] = []
            lines = [f"Runner synthetic received task {case['task']} of job preflight, triggered by event: workflow_dispatch",
                     case['commit'], case['commit']]
            if run == 4501:
                self.journals[run] = {'version': 1, 'scope': self.scope, 'run': run, 'commit': case['commit'],
                                      'applicability_commit': case['commit'], 'complete': False,
                                      'passes': {}, 'fixture_errors': []}
                for label in ('start', 'final'):
                    expected = case['artifacts'][label]
                    raw = archive(self.journals[run])
                    expected['size'], expected['sha256'] = len(raw), digest(raw)
                    self.raw[expected['id']] = raw
                    name = receipts.key(self.scope) + (f'-start-{run}' if label == 'start' else '')
                    self.artifacts[run].append({'id': expected['id'], 'run_id': run, 'name': name,
                                               'size_in_bytes': len(raw), 'expired': False})
                    if label == 'final':
                        lines.append('Python receipt refusal: ReceiptError: Dispatch must match exactly one open '
                                     'same-repository PR into effort (branch/head/base)')
                    lines.append(f'Artifact {name} has been successfully uploaded! Final size is '
                                 f"{len(raw)} bytes. Artifact ID is {expected['id']}")
            else:
                lines.extend(['Python receipt refusal: ReceiptError: Incomplete receipt attempt 4501; '
                              'preserve artifact and recover individual evidence',
                              "skipping post step for 'Publish Python attempt-start marker'; main step was skipped",
                              "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped"])
            lines.append("Job 'Python unit receipts' failed")
            self.logs[case['job']] = ('\n'.join(lines) + '\n').encode()
            self.resign_log(case)

    def resign_log(self, case):
        raw = self.logs[case['job']]
        case['log_bytes'], case['log_sha256'] = len(raw), digest(raw)

    def get(self, path):
        segments = path.split('/')
        if segments[-1] == 'artifacts':
            return self.artifacts[int(segments[-2])]
        return self.actual[int(segments[-1])]

    def bytes(self, path, params=None):
        if path.startswith('/raw/'):
            return self.sources[path.removeprefix('/raw/')]
        if path.startswith('/actions/jobs/'):
            return self.logs[int(path.split('/')[3])]
        return self.raw[int(path.split('/')[3])]

    def pages(self, path, query=None, field=None):
        if path == '/actions/runs':
            return self.runs
        if path.endswith('/jobs'):
            return self.jobs[int(path.split('/')[3])]
        if path == '/actions/artifacts':
            return [item for artifacts in self.artifacts.values() for item in artifacts
                    if item['name'] == query['name']]
        raise AssertionError(path)


class EmptyFailureRecoveryTests(unittest.TestCase):
    def test_exact_empty_attempts_preserve_no_passes_and_reject_contradictions(self):
        with patch.object(recovery, 'PROOF_SHA256', '0' * 64):
            with self.assertRaises(receipts.ReceiptError):
                recovery.empty_failure_case()
        api = EmptyAPI()
        before = copy.deepcopy(api.__dict__)
        with patch.object(recovery, 'empty_failure_case', return_value=api.proof), \
                patch.object(receipts, 'legacy_pr663', return_value={}), \
                patch.object(receipts, 'local_receipts', return_value={}):
            self.assertEqual(receipts.restore(api, api.scope, 9999), {})
        self.assertEqual(api.__dict__, before)
        self.assertFalse(recovery.recover_empty_pr920(api, {**api.scope, 'pr': 921}, api.runs[0], []))
        self.assertFalse(recovery.recover_empty_pr920(api, api.scope, {'id': 4503}, []))

        mutations = ('source', 'log_hash', 'partial_log', 'units', 'wrong_task', 'wrong_checkout',
                     'missing_refusal', 'reordered_upload', 'missing_terminal', 'retry',
                     'missing_job', 'additional_job', 'running_job', 'run_source', 'dispatch',
                     'missing_start', 'expired', 'zip_hash', 'changed_journal', 'positive_map',
                     'fixture_error', 'complete', 'new_final', 'prepare_artifact', 'prepare_upload',
                     'prepare_units', 'prepare_publication', 'prepare_log_hash')
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                candidate = copy.deepcopy(api)
                first, second = candidate.proof['cases']
                if mutation == 'source':
                    candidate.sources['validation/python_unit_receipts.py'] += b'changed'
                elif mutation in ('log_hash', 'partial_log', 'units', 'wrong_task', 'wrong_checkout',
                                  'missing_refusal', 'reordered_upload', 'missing_terminal'):
                    raw = candidate.logs[first['job']]
                    if mutation == 'log_hash':
                        raw += b'changed'
                    elif mutation == 'partial_log':
                        raw = raw[:10]
                    elif mutation == 'units':
                        raw += b'validation: discovered=1, historical-passes=0, pending=1\n'
                    elif mutation == 'wrong_task':
                        raw = raw.replace(b'task 17017', b'task 17018')
                    elif mutation == 'wrong_checkout':
                        raw = raw.replace(first['commit'].encode(), b'0' * 40)
                    elif mutation == 'missing_refusal':
                        raw = raw.replace(b'Dispatch must match', b'Unrelated refusal')
                    elif mutation == 'missing_terminal':
                        raw = raw.replace(b"Job 'Python unit receipts' failed", b'')
                    else:
                        lines = raw.splitlines(); lines[3], lines[4] = lines[4], lines[3]
                        raw = b'\n'.join(lines) + b'\n'
                    candidate.logs[first['job']] = raw
                    if mutation != 'log_hash':
                        candidate.resign_log(first)
                elif mutation in ('retry', 'missing_job', 'additional_job', 'running_job'):
                    jobs = candidate.jobs[4501]
                    if mutation == 'retry':
                        jobs[1]['attempt'] = 2
                    elif mutation == 'missing_job':
                        jobs.pop()
                    elif mutation == 'additional_job':
                        jobs.append({**jobs[-1], 'id': 7777})
                    else:
                        jobs[1]['status'] = 'running'
                elif mutation == 'run_source':
                    candidate.actual[4501]['commit_sha'] = '0' * 40
                elif mutation == 'dispatch':
                    candidate.actual[4501]['event_payload'] = json.dumps({'repository': {'id': 2},
                        'ref': 'refs/heads/' + candidate.scope['branch'], 'workflow': '.github/workflows/effort-ci.yml'})
                elif mutation == 'missing_start':
                    candidate.artifacts[4501].pop(0)
                elif mutation == 'expired':
                    candidate.artifacts[4501][0]['expired'] = True
                elif mutation == 'zip_hash':
                    candidate.raw[1942] += b'changed'
                elif mutation in ('changed_journal', 'positive_map', 'fixture_error', 'complete'):
                    journal = copy.deepcopy(candidate.journals[4501])
                    if mutation == 'changed_journal':
                        journal['applicability_commit'] = '0' * 40
                    elif mutation == 'positive_map':
                        journal['passes'] = {'validation:test_synthetic.Case.test_pass':
                                             {'run': 4501, 'commit': first['commit']}}
                    elif mutation == 'fixture_error':
                        journal['fixture_errors'] = ['error']
                    else:
                        journal['complete'] = True
                    old_size = first['artifacts']['final']['size']
                    raw = archive(journal); candidate.raw[1943] = raw
                    first['artifacts']['final'].update(size=len(raw), sha256=digest(raw))
                    candidate.artifacts[4501][1]['size_in_bytes'] = len(raw)
                    candidate.logs[first['job']] = candidate.logs[first['job']].replace(
                        f'{old_size} bytes. Artifact ID is 1943'.encode(),
                        f'{len(raw)} bytes. Artifact ID is 1943'.encode())
                    candidate.resign_log(first)
                elif mutation == 'new_final':
                    candidate.artifacts[4501].append({**candidate.artifacts[4501][-1], 'id': 8888})
                elif mutation == 'prepare_artifact':
                    candidate.artifacts[4502].append({'id': 8888, 'run_id': 4502,
                        'name': receipts.key(candidate.scope), 'size_in_bytes': 10, 'expired': False})
                else:
                    raw = candidate.logs[second['job']]
                    if mutation == 'prepare_publication':
                        raw = raw.replace(b'Publish Python attempt-start marker', b'Unknown step')
                    elif mutation == 'prepare_upload':
                        raw += b'Artifact unexpected has been successfully uploaded!\n'
                    elif mutation == 'prepare_units':
                        raw += b'Ran 1 tests in 0.001s\n'
                    else:
                        raw += b'changed'
                    candidate.logs[second['job']] = raw
                    if mutation != 'prepare_log_hash':
                        candidate.resign_log(second)
                with patch.object(recovery, 'empty_failure_case', return_value=candidate.proof), \
                        patch.object(receipts, 'legacy_pr663', return_value={}), \
                        patch.object(receipts, 'local_receipts', return_value={}):
                    with self.assertRaises(receipts.ReceiptError):
                        receipts.restore(candidate, candidate.scope, 9999)
