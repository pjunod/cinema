"""One new synthetic control; no actual CI units, credentials or HTTP calls."""
import copy
import hashlib
import io
import json
import unittest
from unittest.mock import patch
import zipfile

from validation import python_unit_receipts as receipts


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def archive(journal):
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w') as zipped:
        zipped.writestr('receipt.json', json.dumps(journal))
    return output.getvalue()


class SyntheticZeroAPI:
    def __init__(self):
        self.proof, self.digest = receipts.zero_failure869_case()
        self.proof = copy.deepcopy(self.proof)
        self.scope = self.proof['scope']
        self.case = self.proof['inherited']
        self.zero = self.proof['refusal']
        self.old = {'run':3898,'commit':'1'*40}
        self.baseline = {}
        self.sources = {path: ('synthetic:' + path).encode() for path in self.case['source_hashes']}
        for case in (self.case, self.zero):
            case['source_hashes'] = {path: digest(raw) for path, raw in self.sources.items()}
        self.start = self.journal(4391, self.case['commit'], self.baseline, False)
        self.final = copy.deepcopy(self.start)
        self.raw = {}
        self.live = []
        self.markers = {}
        self.refresh_archives()
        old_final = archive(self.journal(3898, self.old['commit'], self.baseline, True))
        old_start = archive(self.journal(3898, self.old['commit'], {}, False))
        self.live.append(self.metadata(100, 3898, old_final, False))
        self.markers[3898] = [self.metadata(101, 3898, old_start, True)]
        self.raw.update({100: old_final, 101: old_start})
        lines = [self.case['commit'], self.upload('start'),
                 'historical regression coverage is incomplete:',
                 'make: *** [Makefile:140: history-check] Error 1', self.upload('final'),
                 "Job 'Python unit receipts' failed"]
        self.logs = {44632: ('\n'.join(lines) + '\n').encode(),
                     44658: (self.zero['commit'] + '\n'
                         'Python receipt refusal: ReceiptError: Incomplete receipt attempt 4391; preserve artifact and recover individual evidence\n'
                         "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped\n"
                         "skipping post step for 'Publish Python attempt-start marker'; main step was skipped\n").encode()}
        self.refresh_logs()
        self.runs = [{'id': run, 'commit_sha': commit} for run, commit in (
            (4394, self.zero['commit']), (4391, self.case['commit']), (3898, self.old['commit']))]
        self.jobs = {run: [{'id': job, 'run_id': run, 'repo_id': 1, 'attempt': 1,
                           'name': receipts.job_name(self.scope), 'status': status}]
                     for run, job, status in ((4394, 44658, 'failure'), (4391, 44632, 'failure'),
                                              (3898, 40000, 'success'))}
        self.actual = {case['run']: {'id': case['run'], 'repository': {'id': 1},
            'commit_sha': case['commit'], 'prettyref': self.scope['branch'],
            'workflow_id': 'effort-ci.yml', 'status': 'failure'} for case in (self.case, self.zero)}
        self.comments = [{'id': 9, 'user': {'id': 1, 'login': 'synthetic-writer'},
            'body': 'Python-Journal-Recovery: ' + json.dumps({
                'repository': 1, 'pr': 869, 'sha256': self.digest})}]
        self.permission = 'write'
        self.run_artifacts = []

    def journal(self, run, commit, passes, complete):
        return {'version': 1, 'scope': copy.deepcopy(self.scope), 'run': run,
                'commit': commit, 'applicability_commit': commit, 'complete': complete,
                'passes': copy.deepcopy(passes), 'fixture_errors': []}

    def metadata(self, aid, run, raw, start):
        return {'id': aid, 'run_id': run, 'size_in_bytes': len(raw), 'expired': False,
                'name': receipts.key(self.scope) + (f'-start-{run}' if start else '')}

    def refresh_archives(self):
        for label, aid, journal in (('start', 1781, self.start), ('final', 1784, self.final)):
            raw = archive(journal)
            self.case['artifacts'][label] = {'id': aid, 'size': len(raw), 'sha256': digest(raw)}
            self.raw[aid] = raw
            metadata = self.metadata(aid, 4391, raw, label == 'start')
            if label == 'start':
                self.markers[4391] = [metadata]
            else:
                self.live = [item for item in self.live if item['run_id'] != 4391] + [metadata]

    def upload(self, label):
        artifact = self.case['artifacts'][label]
        name = receipts.key(self.scope) + ('-start-4391' if label == 'start' else '')
        return (f"Artifact {name} has been successfully uploaded! Final size is "
                f"{artifact['size']} bytes. Artifact ID is {artifact['id']}")

    def refresh_logs(self):
        self.case['log_sha256'] = digest(self.logs[44632])
        self.zero['log_sha256'] = digest(self.logs[44658])

    def get(self, path):
        if path.startswith('/collaborators/'):
            return {'permission': self.permission, 'user': {'id': 1}}
        if path == '/actions/runs/4394/artifacts':
            return self.run_artifacts
        if path.startswith('/actions/runs/'):
            return self.actual[int(path.rsplit('/', 1)[1])]
        raise AssertionError(path)

    def bytes(self, path, params=None):
        if path.startswith('/raw/'):
            return self.sources[path.removeprefix('/raw/')]
        if path.startswith('/actions/jobs/'):
            return self.logs[int(path.split('/')[3])]
        if path.startswith('/actions/artifacts/'):
            return self.raw[int(path.split('/')[3])]
        raise AssertionError(path)

    def pages(self, path, params=None, field=None):
        if path == '/issues/869/comments':
            return self.comments
        if path == '/actions/runs':
            return self.runs
        if path.startswith('/actions/runs/'):
            return self.jobs[int(path.split('/')[3])]
        if path == '/actions/artifacts':
            name = params['name']
            return [item for item in self.live + [m for v in self.markers.values() for m in v]
                    if item['name'] == name]
        raise AssertionError(path)


class ZeroFailureRecoveryCase(unittest.TestCase):
    def restore(self, api):
        with patch.object(receipts, 'zero_failure869_case', return_value=(api.proof, api.digest)), \
                patch.object(receipts, 'legacy_pr663', return_value={}), \
                patch.object(receipts, 'local_receipts', return_value={}), \
                patch('sys.stdout', new=io.StringIO()):
            return receipts.restore(api, api.scope, 5000)

    def test_exact_zero_unit_history_and_prepare_refusal_import_no_passes(self):
        api = SyntheticZeroAPI()
        # Remove unrelated synthetic baseline run; both historical journals stay incomplete.
        api.runs = api.runs[:2]
        api.live = [m for m in api.live if m['run_id'] == 4391]
        self.assertEqual(self.restore(api), {})
        self.assertFalse(api.start['complete'])
        self.assertEqual(api.start, api.final)
        self.assertFalse(receipts.recover_zero_pr869(api, dict(api.scope, pr=870),
                                                   api.runs[0], api.jobs[4394]))

    def test_corrupt_or_contradictory_evidence_refuses(self):
        for mode in ('attestation', 'reader', 'source', 'log', 'artifact', 'attempt',
                     'fixture', 'passes', 'unit', 'order', 'refusal_artifact', 'refusal_unit'):
            with self.subTest(mode=mode):
                api = SyntheticZeroAPI()
                api.runs = api.runs[:2]
                api.live = [m for m in api.live if m['run_id'] == 4391]
                if mode == 'attestation': api.comments = []
                if mode == 'reader': api.permission = 'read'
                if mode == 'source': api.sources[next(iter(api.sources))] = b'changed'
                if mode == 'log': api.logs[44632] += b'changed'
                if mode == 'artifact': api.raw[1784] += b'changed'
                if mode == 'attempt': api.jobs[4391][0]['attempt'] = 2
                if mode in ('fixture', 'passes'):
                    if mode == 'fixture': api.start['fixture_errors'] = api.final['fixture_errors'] = ['setup']
                    else: api.start['passes'] = api.final['passes'] = {'validation:x.C.test_x': api.old}
                    api.refresh_archives()
                if mode == 'unit': api.logs[44632] += b'... ok\n'; api.refresh_logs()
                if mode == 'order': api.logs[44632] = ('\n'.join(reversed(api.logs[44632].decode().splitlines()))+'\n').encode(); api.refresh_logs()
                if mode == 'refusal_artifact': api.run_artifacts = [{'run_id':4394}]
                if mode == 'refusal_unit': api.logs[44658] += b'pending=1\n'; api.refresh_logs()
                with self.assertRaises(receipts.ReceiptError): self.restore(api)
