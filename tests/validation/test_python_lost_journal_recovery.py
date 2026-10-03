"""New synthetic recovery controls; no real CI units or network are executed."""
import base64
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


class SyntheticLostAPI:
    """Actual case identities, but entirely fabricated source/log/archive bytes."""
    def __init__(self, zero=False):
        self.zero = zero
        self.scope = {'repository': 1, 'pr': 742, 'branch': 'opus/client-evidence',
                      'base': 'effort/architecture-review-2026-09-20'}
        self.old = '12990cf0a80fa7b4b89153ec290213b9075985b0'
        self.commit = '5fa478987bf464ef6ab9b50bf49dea842acef018'
        self.refusal_commit = '2a299d44b38e67c029b66087f69edc4d5315222e'
        self.case_digest = 'd' * 64
        self.attestor = {'id': 1, 'login': 'synthetic-writer'}
        self.comments = [{'id': 9, 'user': self.attestor,
                          'body': 'Python-Journal-Recovery: ' + json.dumps({
                              'repository': 1, 'pr': 742, 'sha256': self.case_digest})}]
        self.permission = 'write'
        self.sources = {'.github/workflows/effort-ci.yml': b'synthetic workflow',
                        'validation/python_unit_receipts.py': b'synthetic source'}
        self.baseline = {'validation:test_old.Case.test_' + str(i):
                         {'run': 3898, 'commit': self.old} for i in range(3)}
        self.fresh = {suite + ':test_fresh.Case.test_' + suite + str(i):
                      {'run': 3915, 'commit': self.commit}
                      for suite, count in [('validation', 2), ('operations', 7)] for i in range(count)}
        self.start = self.journal(3915, self.commit, self.baseline, False)
        self.final = self.journal(3915, self.commit, {**self.baseline, **self.fresh}, True)
        self.archive_records = {}
        for label, aid, journal in [('start', 1630, self.start), ('final', 1631, self.final)]:
            self.archive_records[label] = self.artifact_witness(aid, journal)
        lines = [self.commit, self.upload('start')]
        for key in self.fresh:
            name = key.split(':', 1)[1]
            lines += [name.rsplit('.', 1)[1] + ' (' + name + ')', 'synthetic docstring ... ok']
        lines += ['Ran 2 tests in 0.001s', 'OK', 'Ran 7 tests in 0.001s', 'OK',
                  'validation: discovered=283, historical-passes=281, pending=2',
                  'operations: discovered=652, historical-passes=645, pending=7', self.upload('final')]
        self.log = ('\n'.join(lines) + '\n').encode()
        self.refusal_log = (self.refusal_commit + '\n'
            'Python receipt refusal: ReceiptError: Missing final receipt for run 3915; do not rerun possibly passed tests\n'
            "skipping post step for 'Preserve Python success journal even on unit failure'; main step was skipped\n"
            "skipping post step for 'Publish Python attempt-start marker'; main step was skipped\n").encode()
        self.case = {'version': 1, 'scope': copy.deepcopy(self.scope),
                     'journal': self.proof(3915, 40909, self.commit, self.log),
                     'refusal': self.proof(3930, 41018, self.refusal_commit, self.refusal_log)}
        self.case['journal'].update({'baseline_count': 3, 'fresh_counts': {'validation': 2, 'operations': 7},
                                    'baseline_run': 3898, 'baseline_commit': self.old,
                                    'artifacts': self.archive_records})
        self.runs = [{'id': 3930, 'commit_sha': self.refusal_commit}] if zero else [
            {'id': 3915, 'commit_sha': self.commit}, {'id': 3898, 'commit_sha': self.old}]
        self.jobs = {run: [{'id': job, 'run_id': run, 'repo_id': 1, 'attempt': 1,
                           'name': receipts.job_name(self.scope), 'status': status}]
                     for run, job, status in [(3915, 40909, 'success'), (3930, 41018, 'failure'),
                                              (3898, 40000, 'success')]}
        # A non-unit retry is irrelevant to the exact original Python attempt.
        self.jobs[3915].append({'id': 40911, 'run_id': 3915, 'repo_id': 1,
                               'attempt': 3, 'name': 'Windows compilation', 'status': 'failure'})
        self.actual = {run: {'id': run, 'repository': {'id': 1}, 'commit_sha': commit,
                            'prettyref': self.scope['branch'], 'workflow_id': 'effort-ci.yml', 'status': 'failure'}
                       for run, commit in [(3915, self.commit), (3930, self.refusal_commit)]}
        old_final = archive(self.journal(3898, self.old, self.baseline, True))
        old_start = archive(self.journal(3898, self.old, {}, False))
        self.live = [] if zero else [self.metadata(30, 3898, old_final)]
        self.markers = {3898: [self.metadata(31, 3898, old_start)], 3915: [], 3930: []}
        self.live_raw = {30: old_final, 31: old_start}
        self.run_artifacts = {3915: [], 3930: []}

    def journal(self, run, commit, passes, complete):
        return {'version': 1, 'scope': copy.deepcopy(self.scope), 'run': run, 'commit': commit,
                'complete': complete, 'fixture_errors': [], 'passes': copy.deepcopy(passes)}

    def artifact_witness(self, aid, journal):
        raw = archive(journal)
        return {'id': aid, 'size': len(raw), 'sha256': digest(raw),
                'zip_base64': base64.b64encode(raw).decode()}

    def metadata(self, aid, run, raw):
        return {'id': aid, 'run_id': run, 'name': receipts.key(self.scope),
                'expired': False, 'size_in_bytes': len(raw)}

    def proof(self, run, job, commit, log):
        return {'run': run, 'job': job, 'commit': commit, 'log_sha256': digest(log),
                'source_hashes': {path: digest(raw) for path, raw in self.sources.items()}}

    def upload(self, label):
        witness = self.archive_records[label]
        name = receipts.key(self.scope) + ('-start-3915' if label == 'start' else '')
        return (f"Artifact {name} has been successfully uploaded! Final size is {witness['size']} bytes. "
                f"Artifact ID is {witness['id']}")

    def pages(self, path, query=None, field=None):
        if path == '/actions/runs': return self.runs
        if path.startswith('/actions/runs/') and path.endswith('/jobs'):
            return self.jobs[int(path.split('/')[3])]
        if path == '/actions/artifacts':
            name = query['name']
            return self.markers[int(name.rsplit('-', 1)[1])] if '-start-' in name else self.live
        if path == '/issues/742/comments': return self.comments
        raise AssertionError(path)

    def get(self, path):
        if path == '/collaborators/synthetic-writer/permission':
            return {'user': self.attestor, 'permission': self.permission}
        if path.endswith('/artifacts'): return self.run_artifacts[int(path.split('/')[3])]
        if path.startswith('/actions/runs/'): return self.actual[int(path.split('/')[3])]
        raise AssertionError(path)

    def bytes(self, path, query=None):
        if path.startswith('/raw/'):
            assert query['ref'] in (self.commit, self.refusal_commit)
            return self.sources[path.removeprefix('/raw/')]
        if path.startswith('/actions/artifacts/'): return self.live_raw[int(path.split('/')[3])]
        if path == '/actions/jobs/40909/logs': return self.log
        if path == '/actions/jobs/41018/logs': return self.refusal_log
        raise AssertionError(path)


class LostJournalRecoveryCase(unittest.TestCase):
    def restore(self, api):
        calls = []
        class Applicability:
            def __call__(self, key, source):
                calls.append((key, copy.deepcopy(source)))
                return True
            def finish(self, passes): calls.append(('finish', len(passes)))
        with patch.object(receipts, 'lost_journal_case', return_value=(api.case, api.case_digest)), \
             patch.object(receipts, 'local_receipts', return_value={}), \
             patch.object(receipts, 'legacy_pr663', return_value={}), \
             patch.object(receipts, 'discover') as discovery, \
             patch('sys.stdout', new=io.StringIO()) as output:
            result = receipts.restore(api, api.scope, 4000, Applicability())
            discovery.assert_not_called()
            return result, calls, output.getvalue()

    def test_lost_journal_restores_original_provenance_only_with_bound_authenticated_witness(self):
        api = SyntheticLostAPI()
        restored, calls, output = self.restore(api)
        self.assertEqual(restored, {**api.baseline, **api.fresh})
        self.assertEqual(calls[-1], ('finish', 12))
        self.assertTrue(set(api.fresh).issubset({key for key, _ in calls}))
        self.assertIn('no units replayed', output)
        modes = ('authority', 'wrong_hash', 'permission', 'source', 'log', 'job_attempt',
                 'job_status', 'job_duplicate', 'run_source', 'run_status', 'scope', 'unknown_run',
                 'encoding', 'archive_hash', 'upload', 'ordering', 'baseline', 'fresh_source',
                 'missing_ok', 'duplicate_event', 'summary', 'live_start', 'run_artifact', 'orphan_source')
        for mode in modes:
            with self.subTest(mode=mode):
                api = SyntheticLostAPI()
                if mode == 'authority': api.comments = []
                elif mode == 'wrong_hash': api.comments[0]['body'] = api.comments[0]['body'].replace('d' * 64, 'e' * 64)
                elif mode == 'permission': api.permission = 'read'
                elif mode == 'source': api.sources['validation/python_unit_receipts.py'] += b'changed'
                elif mode == 'log': api.log += b'changed'
                elif mode == 'job_attempt': api.jobs[3915][0]['attempt'] = 2
                elif mode == 'job_status': api.jobs[3915][0]['status'] = 'failure'
                elif mode == 'job_duplicate': api.jobs[3915].append(copy.deepcopy(api.jobs[3915][0]))
                elif mode == 'run_source': api.actual[3915]['commit_sha'] = 'f' * 40
                elif mode == 'run_status': api.actual[3915]['status'] = 'success'
                elif mode == 'scope': api.scope['pr'] = 743
                elif mode == 'unknown_run': api.runs[0]['id'] = 3916; api.jobs[3916] = api.jobs[3915]
                elif mode == 'encoding': api.archive_records['final']['zip_base64'] = '@'
                elif mode == 'archive_hash': api.archive_records['final']['sha256'] = 'e' * 64
                elif mode in ('upload', 'ordering', 'missing_ok', 'duplicate_event', 'summary'):
                    if mode == 'upload': api.log = api.log.replace(b'Artifact ID is 1631', b'Artifact ID is 1632')
                    elif mode == 'ordering':
                        lines = api.log.splitlines(); lines.remove(api.upload('start').encode())
                        lines.append(api.upload('start').encode()); api.log = b'\n'.join(lines) + b'\n'
                    elif mode == 'missing_ok': api.log = api.log.replace(b'synthetic docstring ... ok', b'synthetic docstring ... FAIL', 1)
                    elif mode == 'duplicate_event': api.log += b'test_validation0 (test_fresh.Case.test_validation0) ... ok\n'
                    elif mode == 'summary': api.log = api.log.replace(b'pending=2', b'pending=3')
                    api.case['journal']['log_sha256'] = digest(api.log)
                elif mode in ('baseline', 'fresh_source', 'orphan_source'):
                    if mode == 'baseline': api.final['passes'][next(iter(api.baseline))]['run'] = 123
                    elif mode == 'fresh_source': api.final['passes'][next(iter(api.fresh))]['commit'] = 'e' * 40
                    else: api.jobs.pop(3898); api.runs.pop()
                    if mode != 'orphan_source':
                        before = api.upload('final').encode()
                        api.archive_records['final'] = api.artifact_witness(1631, api.final)
                        api.case['journal']['artifacts'] = api.archive_records
                        api.log = api.log.replace(before, api.upload('final').encode())
                        api.case['journal']['log_sha256'] = digest(api.log)
                elif mode == 'live_start': api.markers[3915] = [{'id': 99}]
                elif mode == 'run_artifact': api.run_artifacts[3915] = [{'id': 99}]
                with self.assertRaises(receipts.ReceiptError): self.restore(api)

    def test_bound_missing_journal_prepare_refusal_imports_zero_and_refuses_started_units(self):
        api = SyntheticLostAPI(zero=True)
        restored, calls, output = self.restore(api)
        self.assertEqual(restored, {})
        self.assertEqual(calls, [('finish', 0)])
        self.assertIn('zero units, no successes imported', output)
        modes = ('authority', 'job_attempt', 'unit_phase', 'missing_skip', 'skip_order',
                 'start', 'final', 'run_artifact', 'unknown_refusal', 'source', 'log')
        for mode in modes:
            with self.subTest(mode=mode):
                api = SyntheticLostAPI(zero=True)
                if mode == 'authority': api.comments = []
                elif mode == 'job_attempt': api.jobs[3930][0]['attempt'] = 2
                elif mode in ('unit_phase', 'missing_skip', 'skip_order', 'unknown_refusal'):
                    if mode == 'unit_phase': api.refusal_log += b'Ran 1 test\n'
                    elif mode == 'missing_skip': api.refusal_log = api.refusal_log.replace(b'Publish Python attempt-start marker', b'other')
                    elif mode == 'unknown_refusal': api.refusal_log = api.refusal_log.replace(b'run 3915', b'run 3916')
                    else:
                        lines = api.refusal_log.splitlines(keepends=True)
                        api.refusal_log = b''.join([lines[0], lines[2], lines[3], lines[1]])
                    api.case['refusal']['log_sha256'] = digest(api.refusal_log)
                elif mode == 'start': api.markers[3930] = [{'id': 99}]
                elif mode == 'final': api.live = [api.metadata(99, 3930, b'fake')]
                elif mode == 'run_artifact': api.run_artifacts[3930] = [{'id': 99}]
                elif mode == 'source': api.sources['validation/python_unit_receipts.py'] += b'changed'
                elif mode == 'log': api.refusal_log += b'changed'
                with self.assertRaises(receipts.ReceiptError): self.restore(api)
