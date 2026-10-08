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


class SyntheticInheritedAPI:
    def __init__(self):
        self.proof, self.digest = receipts.inherited_failure_case()
        self.proof = copy.deepcopy(self.proof)
        self.scope = self.proof['scope']
        self.case = self.proof['inherited']
        self.zero = self.proof['refusal']
        self.old = self.case['sources'][0]
        self.baseline = {'validation:test_synthetic.Case.test_' + str(i): dict(self.old)
                         for i in range(3)}
        self.case['count'] = 3
        self.sources = {path: ('synthetic:' + path).encode() for path in self.case['source_hashes']}
        for case in (self.case, self.zero):
            case['source_hashes'] = {path: digest(raw) for path, raw in self.sources.items()}
        self.start = self.journal(3994, self.case['commit'], self.baseline, False)
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
        self.logs = {41507: ('\n'.join(lines) + '\n').encode(),
                     41568: (self.zero['commit'] + '\n'
                         'Python receipt refusal: ReceiptError: Incomplete receipt attempt 3994; preserve artifact and recover individual evidence\n'
                         "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped\n"
                         "skipping post step for 'Publish Python attempt-start marker'; main step was skipped\n").encode()}
        # The unknown-incomplete control must expose a log that cannot prove
        # pre-unit history failure when the restore consumer inspects it.
        self.logs[40000] = b"Unrecognized prior unit attempt; no pre-unit history proof\n"
        self.refresh_logs()
        self.runs = [{'id': run, 'commit_sha': commit} for run, commit in (
            (4002, self.zero['commit']), (3994, self.case['commit']), (3898, self.old['commit']))]
        self.jobs = {run: [{'id': job, 'run_id': run, 'repo_id': 1, 'attempt': 1,
                           'name': receipts.job_name(self.scope), 'status': status}]
                     for run, job, status in ((4002, 41568, 'failure'), (3994, 41507, 'failure'),
                                              (3898, 40000, 'success'))}
        self.actual = {case['run']: {'id': case['run'], 'repository': {'id': 1},
            'commit_sha': case['commit'], 'prettyref': self.scope['branch'],
            'workflow_id': 'effort-ci.yml', 'status': 'failure'} for case in (self.case, self.zero)}
        self.comments = [{'id': 9, 'user': {'id': 1, 'login': 'synthetic-writer'},
            'body': 'Python-Journal-Recovery: ' + json.dumps({
                'repository': 1, 'pr': 742, 'sha256': self.digest})}]
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
        for label, aid, journal in (('start', 1653, self.start), ('final', 1654, self.final)):
            raw = archive(journal)
            self.case['artifacts'][label] = {'id': aid, 'size': len(raw), 'sha256': digest(raw)}
            self.raw[aid] = raw
            metadata = self.metadata(aid, 3994, raw, label == 'start')
            if label == 'start':
                self.markers[3994] = [metadata]
            else:
                self.live = [item for item in self.live if item['run_id'] != 3994] + [metadata]

    def upload(self, label):
        artifact = self.case['artifacts'][label]
        name = receipts.key(self.scope) + ('-start-3994' if label == 'start' else '')
        return (f"Artifact {name} has been successfully uploaded! Final size is "
                f"{artifact['size']} bytes. Artifact ID is {artifact['id']}")

    def refresh_logs(self):
        self.case['log_sha256'] = digest(self.logs[41507])
        self.zero['log_sha256'] = digest(self.logs[41568])

    def get(self, path):
        if path.startswith('/collaborators/'):
            return {'permission': self.permission, 'user': {'id': 1}}
        if path == '/actions/runs/4002/artifacts':
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
        if path == '/issues/742/comments':
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


class InheritedFailureRecoveryCase(unittest.TestCase):
    def restore(self, api, applicability=None):
        with patch.object(receipts, 'inherited_failure_case', return_value=(api.proof, api.digest)), \
                patch.object(receipts, 'legacy_pr663', return_value={}), \
                patch.object(receipts, 'local_receipts', return_value={}), \
                patch('sys.stdout', new=io.StringIO()):
            return receipts.restore(api, api.scope, 5000, applicability)

    def test_exact_inherited_failure_and_empty_refusal_preserve_original_trust(self):
        api = SyntheticInheritedAPI()
        original = copy.deepcopy(api.start)
        self.assertEqual(self.restore(api), api.baseline)
        self.assertEqual(api.start, original)
        self.assertEqual(api.final, original)
        self.assertFalse(api.final['complete'])

        class Applicability:
            def __init__(self): self.seen = []; self.finished = None
            def __call__(self, key, source):
                self.seen.append((key, copy.deepcopy(source)))
                return not key.endswith('test_0')
            def finish(self, passes): self.finished = dict(passes)
        consumer = Applicability()
        expected = {k: v for k, v in api.baseline.items() if not k.endswith('test_0')}
        self.assertEqual(self.restore(api, consumer), expected)
        self.assertEqual(consumer.finished, expected)
        self.assertTrue(all(source == api.old for _, source in consumer.seen))

        # Authenticated hashes alone cannot excuse contradictory phase/map facts.
        for mode in ('attestation', 'reader', 'run', 'attempt', 'source', 'log', 'artifact',
                     'expired', 'start_missing', 'changed_start', 'fixture', 'fresh_source',
                     'untrusted_source', 'unit', 'order', 'duplicate', 'empty_artifact',
                     'empty_unit', 'empty_order', 'unknown_incomplete'):
            with self.subTest(mode=mode):
                api = SyntheticInheritedAPI()
                if mode == 'attestation': api.comments = []
                if mode == 'reader': api.permission = 'read'
                if mode == 'run': api.actual[3994]['status'] = 'success'
                if mode == 'attempt': api.jobs[3994][0]['attempt'] = 2
                if mode == 'source': api.sources[next(iter(api.sources))] = b'changed'
                if mode == 'log': api.logs[41507] += b'changed\n'
                if mode == 'artifact': api.raw[1654] += b'changed'
                if mode == 'expired': api.live[0]['expired'] = True
                if mode == 'start_missing': api.markers[3994] = []
                if mode in ('changed_start', 'fixture', 'fresh_source', 'untrusted_source'):
                    if mode == 'changed_start': api.final['passes'].pop(next(iter(api.baseline)))
                    if mode == 'fixture': api.start['fixture_errors'] = api.final['fixture_errors'] = ['setup']
                    if mode == 'fresh_source':
                        api.start['passes'] = api.final['passes'] = {'validation:test_synthetic.Case.test_0':
                            {'run': 3994, 'commit': api.case['commit']}}
                        api.case['count'] = 1
                    if mode == 'untrusted_source':
                        api.runs = [r for r in api.runs if r['id'] != 3898]
                        api.live = [m for m in api.live if m['run_id'] != 3898]
                    api.refresh_archives()
                if mode == 'unit': api.logs[41507] += b'test_x (test_x.C.test_x) ... ok\n'
                if mode == 'order':
                    api.logs[41507] = (api.upload('final') + '\n' + api.logs[41507].decode()).encode()
                if mode == 'duplicate': api.logs[41507] += b'historical regression coverage is incomplete:\n'
                if mode == 'empty_artifact': api.run_artifacts = [{'run_id': 4002}]
                if mode == 'empty_unit': api.logs[41568] += b'validation: discovered=1\n'
                if mode == 'empty_order':
                    api.logs[41568] = ('\n'.join(reversed(api.logs[41568].decode().splitlines()))+'\n').encode()
                if mode == 'unknown_incomplete':
                    old = receipts.artifact_json(api.raw[100]); old['complete'] = False
                    api.raw[100] = archive(old)
                    next(m for m in api.live if m['id'] == 100)['size_in_bytes'] = len(api.raw[100])
                if mode not in ('log',): api.refresh_logs()
                with self.assertRaises(receipts.ReceiptError):
                    self.restore(api)

        api = SyntheticInheritedAPI()
        foreign = dict(api.scope, pr=743)
        self.assertFalse(receipts.recover_inherited_pr742(api, foreign, api.runs[0], api.jobs[4002]))
        with self.assertRaises(receipts.ReceiptError):
            receipts.validate_journal(api.final, api.scope, 3994, api.case['commit'])
