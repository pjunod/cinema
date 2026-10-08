"""Synthetic negative controls for authenticated zero-outcome history recovery."""
import copy
import hashlib
import io
import json
import unittest
from unittest.mock import patch
import zipfile
from validation import python_unit_receipts as receipts
from validation import python_unit_preunit_history as recovery


def zipped(value, indent=None):
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, 'w') as archive:
        archive.writestr('receipt.json', json.dumps(value, indent=indent))
    return stream.getvalue()


class Evidence:
    def __init__(self, pr=938, run=4533):
        self.scope = dict(repository=1, pr=pr, branch='codex/synthetic-task', base='effort/synthetic')
        self.commit = 'a' * 40
        self.actual = dict(id=run, repository={'id': 1}, commit_sha=self.commit,
                           prettyref=self.scope['branch'], workflow_id='effort-ci.yml',
                           status='failure', trigger_event='workflow_dispatch',
                           event_payload=json.dumps(dict(repository={'id': 1},
                               ref='refs/heads/' + self.scope['branch'], workflow='.github/workflows/effort-ci.yml')))
        self.pull = dict(number=pr, head={'repo': {'id': 1}, 'ref': self.scope['branch']},
                         base={'repo': {'id': 1}, 'ref': self.scope['base']})
        self.jobs = [dict(id=i + 100, name=name, repo_id=1, run_id=run, attempt=1,
                         status='failure' if name == receipts.JOB_PREFIX else 'skipped')
                     for i, name in enumerate(sorted(recovery.JOB_NAMES))]
        self.sources = {path: ('synthetic producer ' + path).encode() for path in recovery.PRODUCERS}
        self.registry = {path: {hashlib.sha256(raw).hexdigest()} for path, raw in self.sources.items()}
        self.journal = dict(version=1, scope=self.scope, run=run, commit=self.commit,
                            applicability_commit=self.commit, complete=False, passes={}, fixture_errors=[])
        self.start_raw = zipped(self.journal)
        self.final_raw = zipped(self.journal, indent=2)
        self.start = dict(id=1001, run_id=run, name=receipts.key(self.scope) + f'-start-{run}',
                          expired=False, size_in_bytes=len(self.start_raw))
        self.final = dict(id=1002, run_id=run, name=receipts.key(self.scope),
                          expired=False, size_in_bytes=len(self.final_raw))
        lines = [self.commit]
        for item, raw in [(self.start, self.start_raw), (self.final, self.final_raw)]:
            lines.extend(['SHA256 hash of uploaded artifact zip is ' + hashlib.sha256(raw).hexdigest(),
                          f"Artifact {item['name']} has been successfully uploaded! Final size is "
                          f"{len(raw)} bytes. Artifact ID is {item['id']}"])
            if item is self.start:
                lines.extend(['history audit error: authenticated promotion history binding refused',
                              'make: *** [Makefile:144: history-check] Error 2'])
        lines.append("Job 'Python unit receipts' failed")
        self.log = ('\n'.join(lines) + '\n').encode()

    def get(self, path):
        return self.pull if path.startswith('/pulls/') else self.actual

    def bytes(self, path, query=None):
        return self.sources[path.removeprefix('/raw/')] if path.startswith('/raw/') else self.log

    def recover(self):
        with patch.object(recovery, 'PRODUCERS', self.registry):
            return recovery.recover(self, self.scope, self.actual, self.jobs, self.start,
                                    self.start_raw, self.final, self.final_raw, self.journal)


class PreunitHistoryCase(unittest.TestCase):
    def test_proven_empty_history_failure_imports_no_outcomes(self):
        for pr, run in [(938, 4533), (939, 4534), (1940, 8000)]:
            evidence = Evidence(pr, run)
            before = copy.deepcopy(evidence.journal)
            self.assertTrue(evidence.recover())
            self.assertEqual(before, evidence.journal)
            self.assertEqual(evidence.journal['passes'], {})
            self.assertFalse(evidence.journal['complete'])

    def test_adversarial_evidence_is_refused(self):
        changes = [
            lambda e: e.actual.update(commit_sha='b' * 40),
            lambda e: e.actual.update(repository={'id': 2}),
            lambda e: e.actual.update(prettyref='codex/other'),
            lambda e: e.actual.update(trigger_event='push'),
            lambda e: e.pull['base'].update(ref='main'),
            lambda e: e.pull['head'].update(repo={'id': 2}),
            lambda e: e.jobs[0].update(attempt=2),
            lambda e: e.jobs.pop(),
            lambda e: e.sources.update({'Makefile': b'unreviewed'}),
            lambda e: e.final.update(run_id=9999),
            lambda e: e.start.update(expired=True),
            lambda e: e.final.update(size_in_bytes=1),
            lambda e: e.journal.update(passes={'validation:x': {'run': 1, 'commit': 'b' * 40}}),
            lambda e: e.journal.update(fixture_errors=['real fixture failure']),
            lambda e: setattr(e, 'log', e.log.replace(b'make: *** [Makefile:144: history-check] Error 2', b'Ran 1 test\n... ok')),
            lambda e: setattr(e, 'log', e.log.replace(b"Job 'Python unit receipts' failed\n", b'')),
            lambda e: setattr(e, 'final_raw', e.start_raw),
        ]
        for index, change in enumerate(changes):
            with self.subTest(index=index):
                evidence = Evidence()
                # Keep prior list metadata separate from the authoritative run.
                prior = copy.deepcopy(evidence.actual)
                change(evidence)
                with patch.object(recovery, 'PRODUCERS', evidence.registry):
                    with self.assertRaises(receipts.ReceiptError):
                        recovery.recover(evidence, evidence.scope, prior, evidence.jobs,
                                         evidence.start, evidence.start_raw, evidence.final,
                                         evidence.final_raw, evidence.journal)

    def test_unrelated_incomplete_failures_keep_existing_recovery(self):
        evidence = Evidence()
        evidence.log = b"other refusal\nJob 'Python unit receipts' failed\n"
        self.assertFalse(evidence.recover())
