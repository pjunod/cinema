"""Main-lane Python receipts: history never refuses, adoption stays authenticated."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from validation import main_unit_receipts as main
from validation.python_unit_receipts import ReceiptError

SCOPE = {'repository': 1, 'pr': 7, 'branch': 'topic', 'base': 'main', 'workflow': main.WORKFLOW}
X, Y, Z = 'a' * 40, 'b' * 40, 'c' * 40
ONE = 'validation:test_fixture.Case.test_one'
TWO = 'operations:test_fixture.Case.test_two'
THREE = 'validation:test_fixture.Case.test_three'


def zipped(value):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, 'w') as archive:
        archive.writestr('receipt.json', json.dumps(value))
    return buffer.getvalue()


def framed(commit, *journals, checkout=(None, None, 'triggered by event: pull_request')):
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        print(*(line or commit for line in checkout), sep='\n')
        for phase, journal in zip(('start', 'final'), journals):
            main.emit_snapshot(journal, phase)
    return output.getvalue().encode()


class History:
    """One PR's main-lane runs as the Forgejo API returns them."""

    def __init__(self):
        self.runs, self.jobs, self.artifacts, self.blobs, self.logs = [], {}, [], {}, {}

    def run(self, rid, commit, own=(), inherited=None, final=True, marker=True, **job):
        self.runs.append({'id': rid, 'workflow_id': main.WORKFLOW, 'prettyref': '#7', 'event': 'pull_request',
                          'commit_sha': commit, 'event_payload': {'repository': {'id': 1}, 'number': 7,
                          'pull_request': {'number': 7, 'head': {'ref': 'topic', 'sha': commit, 'repo': {'id': 1}},
                                           'base': {'ref': 'main', 'repo': {'id': 1}}}}})
        self.jobs[rid] = [{'id': rid * 10, 'name': main.JOB, 'repo_id': 1, 'run_id': rid,
                           'attempt': 1, 'status': 'failure', 'task_id': rid, **job}]
        start = {'version': main.receipts.VERSION, 'scope': SCOPE, 'run': rid, 'commit': commit,
                 'complete': False, 'fixture_errors': [], 'passes': dict(inherited or {})}
        journal = dict(start, complete=True, passes={**start['passes'],
                                                     **{test: {'run': rid, 'commit': commit} for test in own}})
        if marker:
            self.publish(main.key(SCOPE) + f'-start-{rid}', rid, start)
        if final:
            self.publish(main.key(SCOPE), rid, journal)
        return start, journal

    def publish(self, name, rid, value):
        identity = len(self.blobs) + 1
        self.blobs[identity] = value
        self.artifacts.append({'id': identity, 'name': name, 'run_id': rid, 'expired': False, 'size_in_bytes': 1})
        return self.artifacts[-1]

    def pages(self, path, query=None, field=None):
        if path == '/actions/runs':
            return copy.deepcopy(self.runs)
        if path == '/actions/artifacts':
            return [dict(item) for item in self.artifacts if item['name'] == query['name']]
        return copy.deepcopy(self.jobs[int(path.split('/')[3])])

    def bytes(self, path, query=None):
        identity = int(path.split('/')[3])
        if path.endswith('/zip'):
            return zipped(self.blobs[identity])
        return self.logs.get(identity // 10, b'checkout only\n')


class Applicable:
    def __call__(self, test, value):
        return True

    def finish(self, passes):
        self.finished = dict(passes)


def restore(history, current=99, available=(X, Y, Z)):
    output = io.StringIO()
    with patch.object(main.adoption, 'commit_available', side_effect=lambda commit: commit in available), \
         contextlib.redirect_stdout(output):
        passes = main.restore(history, SCOPE, current, Applicable())
    return passes, output.getvalue()


class MainUnitReceiptsCase(unittest.TestCase):
    def test_prepare_crash_without_marker_or_journal_does_not_block(self):
        history = History()
        history.run(10, X, own=[ONE])
        history.run(11, Y, final=False, marker=False)
        history.logs[11] = (Y + '\n' + Y + '\ntriggered by event: pull_request\n'
                            "subprocess.CalledProcessError: Command '['git', 'show']' returned non-zero exit status 128.\n").encode()
        passes, output = restore(history)
        self.assertEqual(passes, {ONE: {'run': 10, 'commit': X}})
        self.assertIn('run 11: journal not adopted — no complete start/final journal in the log', output)

    def test_force_pushed_journal_commit_is_not_adopted_and_does_not_block(self):
        # PR #997: run A published journals at X, a force push removed X from
        # the checkout, run B crashed in prepare, and run C must still start.
        gone = 'e' * 40
        self.assertFalse(main.adoption.commit_available(gone))  # real git cat-file
        history = History()
        history.run(10, gone, own=[ONE, TWO])
        history.run(11, Y, final=False, marker=False)
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            passes = main.restore(history, SCOPE, 12, Applicable())
        self.assertEqual(passes, {})
        self.assertIn(f'run 10: journal not adopted — commit {gone} is not available in this checkout', output.getvalue())

    def test_inherited_success_is_kept_only_when_its_originating_journal_is_adopted(self):
        history = History()
        _, older = history.run(9, Z, own=[THREE])
        _, gone = history.run(10, X, own=[ONE])
        history.run(11, Y, own=[TWO], inherited={ONE: gone['passes'][ONE], THREE: older['passes'][THREE]})
        passes, output = restore(history, available=(Y, Z))
        self.assertEqual(passes, {TWO: {'run': 11, 'commit': Y}, THREE: {'run': 9, 'commit': Z}})
        self.assertIn('run 11: 1 inherited successes not adopted', output)

    def test_each_authentication_check_alone_prevents_adoption_and_never_blocks(self):
        def mutate_final(history, **changes):
            final = [item for item in history.artifacts if item['name'] == main.key(SCOPE) and item['run_id'] == 11][0]
            history.blobs[final['id']] = dict(history.blobs[final['id']], **changes)
        def mutate_marker(history, **changes):
            marker = [item for item in history.artifacts if item['name'].endswith('-start-11')][0]
            history.blobs[marker['id']] = dict(history.blobs[marker['id']], **changes)
        def mutate_run(history, **changes):
            history.runs[1].update(changes)
        mutations = {
            'start marker missing': lambda h: h.artifacts.remove(
                [item for item in h.artifacts if item['name'].endswith('-start-11')][0]),
            'start marker expired': lambda h: [item.update(expired=True) for item in h.artifacts
                                               if item['name'].endswith('-start-11')],
            'start marker already complete': lambda h: mutate_marker(h, complete=True),
            'start marker for another run': lambda h: mutate_marker(h, run=12),
            'final artifact expired': lambda h: [item.update(expired=True) for item in h.artifacts
                                                 if item['name'] == main.key(SCOPE) and item['run_id'] == 11],
            'two final journals': lambda h: h.publish(main.key(SCOPE), 11, h.blobs[2]),
            'final for another PR': lambda h: mutate_final(h, scope=dict(SCOPE, pr=8)),
            'final for another commit': lambda h: mutate_final(h, commit=X),
            'final records fixture errors': lambda h: mutate_final(h, fixture_errors=['setUpClass']),
            'final completion marker missing': lambda h: mutate_final(h, complete=None),
            'final dropped inherited start record': lambda h: (
                mutate_marker(h, passes={THREE: {'run': 9, 'commit': Z}})),
            're-run job': lambda h: h.jobs[11][0].update(attempt=2),
            'job still running': lambda h: h.jobs[11][0].update(status='running'),
            'job skipped': lambda h: h.jobs[11][0].update(status='skipped'),
            'job never assigned': lambda h: h.jobs[11][0].update(task_id=0),
            'job from another repository': lambda h: h.jobs[11][0].update(repo_id=2),
            'second preflight job': lambda h: h.jobs[11].append(dict(h.jobs[11][0], id=111)),
            'run for another PR': lambda h: mutate_run(h, prettyref='#8'),
            'run from a push': lambda h: mutate_run(h, event='push'),
            'run not listed for this PR': lambda h: h.runs.pop(1),
            'commit not available': lambda h: mutate_run(h, commit_sha='f' * 40, event_payload=dict(
                h.runs[1]['event_payload'], pull_request=dict(h.runs[1]['event_payload']['pull_request'],
                head={'ref': 'topic', 'sha': 'f' * 40, 'repo': {'id': 1}}))),
        }
        baseline = History()
        baseline.run(10, X, own=[ONE])
        baseline.run(11, Y, own=[TWO])
        self.assertEqual(set(restore(baseline)[0]), {ONE, TWO})
        for name, mutation in mutations.items():
            history = copy.deepcopy(baseline)
            mutation(history)
            with self.subTest(check=name):
                passes, output = restore(history, available=(X, Y))
                self.assertEqual(passes, {ONE: {'run': 10, 'commit': X}})
                self.assertIn('run 11: journal not adopted — ', output)

    def test_completed_log_journal_replaces_a_missing_final_upload(self):
        history = History()
        start, journal = history.run(10, X, own=[ONE], final=False)
        history.logs[10] = framed(X, start, journal) + b'context deadline exceeded\n'
        self.assertEqual(restore(history)[0], {ONE: {'run': 10, 'commit': X}})
        history.logs[10] = framed(X, start) + b'context deadline exceeded\n'
        passes, output = restore(history)
        self.assertEqual(passes, {})
        self.assertIn('run 10: journal not adopted', output)
        # A complete pair still needs the job to have checked out exactly this
        # commit for this pull request.
        for checkout in ((None, 'checkout', 'triggered by event: pull_request'),
                         (None, None, 'triggered by event: push')):
            history.logs[10] = framed(X, start, journal, checkout=checkout)
            with self.subTest(checkout=checkout):
                passes, output = restore(history)
                self.assertEqual(passes, {})
                self.assertIn('run 10: journal not adopted — log lacks exact-source checkout evidence', output)

    def test_unlisted_history_adopts_nothing_and_names_unexpected_error_types(self):
        import http.client
        for path in ('/actions/artifacts', '/actions/runs'):
            history = History()
            history.run(10, X, own=[ONE])
            listing = history.pages
            def broken(page, query=None, field=None, path=path):
                if page == path:
                    raise ReceiptError('Evidence pagination limit reached')
                return listing(page, query, field)
            history.pages = broken
            with self.subTest(listing=path):
                passes, output = restore(history)
                self.assertEqual(passes, {})
                self.assertIn('unavailable — Evidence pagination limit reached; nothing adopted, every test runs', output)
        history = History()
        history.run(10, X, own=[ONE])
        history.runs[0]['event_payload'] = 5  # metadata of an unexpected shape
        passes, output = restore(history)
        self.assertEqual(passes, {})
        self.assertRegex(output, r'run 10: journal not adopted — (TypeError|AttributeError): ')
        history = History()
        history.run(10, X, own=[ONE])
        def truncated(path, query=None):
            raise http.client.IncompleteRead(b'partial', 10)
        history.bytes = truncated
        passes, output = restore(history)
        self.assertEqual(passes, {})
        self.assertIn('run 10: journal not adopted — IncompleteRead', output)

    def test_integrity_errors_are_never_downgraded_to_not_adopted(self):
        history = History()
        history.run(10, X, own=[ONE])
        for target in ('preflight_job', 'listed_run'):
            with self.subTest(target=target), \
                 patch.object(main, target, side_effect=main.receipts.IntegrityError('current run unsafe')), \
                 self.assertRaisesRegex(main.receipts.IntegrityError, 'current run unsafe'):
                restore(history)

    def test_run_step_executes_passes_whose_recheck_cannot_complete(self):
        class Fixture(unittest.TestCase):
            ran = []
            def test_kept(self): raise AssertionError('a rechecked pass must not execute')
            def test_unreadable(self): self.ran.append('unreadable')
            def test_changed(self): self.ran.append('changed')
        suites = {name: [Fixture('test_kept'), Fixture('test_unreadable'), Fixture('test_changed')]
                  for name in main.receipts.SUITES}
        commit = 'd' * 40
        cached = {name + ':' + test.id(): {'run': 3, 'commit': X} for name in suites for test in suites[name]}
        class Recheck:
            error = main.subprocess.TimeoutExpired(['git', 'cat-file'], 5)
            def __init__(self, commit): pass
            def __call__(self, test, value):
                if test.endswith('test_unreadable'): raise self.error
                return test.endswith('test_kept')
            def finish(self, passes): pass
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(main.receipts, 'SourceApplicability', Recheck), \
             patch.object(main.receipts, 'discover', side_effect=lambda name: suites[name]):
            path = Path(directory) / 'receipt.json'
            journal = {'version': main.receipts.VERSION, 'scope': SCOPE, 'commit': commit, 'run': 4,
                       'applicability_commit': commit, 'passes': dict(cached), 'fixture_errors': [], 'complete': False}
            self.assertEqual(main.execute(journal, path), 0)
            self.assertEqual(sorted(Fixture.ran), ['changed', 'changed', 'unreadable', 'unreadable'])
            self.assertEqual({test: value['run'] for test, value in journal['passes'].items()},
                             {test: 3 if test.endswith('test_kept') else 4 for test in cached})
            Recheck.error = main.receipts.IntegrityError('Current unit worktree differs from dispatched Git source')
            journal.update(passes=dict(cached), complete=False)
            with self.assertRaises(main.receipts.IntegrityError):
                main.execute(journal, path)
            # Without a current-source prepare nothing may be skipped at all.
            journal.update(passes=dict(cached), applicability_commit=X)
            with self.assertRaisesRegex(ReceiptError, 'Missing current-source prepare'):
                main.execute(journal, path)

    def test_current_run_integrity_still_refuses(self):
        history = History()
        history.run(10, X, own=[ONE])
        history.run(11, Y, own=[TWO])
        with self.assertRaisesRegex(ReceiptError, 'Attempt already executed'):
            restore(history, current=11)
        environment = {'GITHUB_API_URL': 'https://forge.example/api/v1', 'GITHUB_SERVER_URL': 'https://forge.example',
                       'GITHUB_REPOSITORY': 'owner/repository', 'GITHUB_TOKEN': 'token', 'GITHUB_SHA': X,
                       'GITHUB_RUN_ID': '12', 'PR_NUMBER': '7', 'GITHUB_RUN_ATTEMPT': '2'}
        with patch.dict(os.environ, environment), \
             patch.object(main.subprocess, 'check_output', return_value=X + '\n'), \
             patch.object(main.adoption, 'prepare', side_effect=AssertionError('history must not be read')), \
             patch.object(main, 'identity', side_effect=AssertionError('no API call before the attempt check')):
            with self.assertRaisesRegex(ReceiptError, 'never rerun jobs'):
                main.main(['prepare'])
            os.environ['GITHUB_SHA'] = Y
            with self.assertRaisesRegex(ReceiptError, 'Checkout is not event source'):
                main.main(['prepare'])

    def test_log_snapshots_require_complete_ordered_digest_bound_frames(self):
        import contextlib
        import io
        journal = {'passes': {'validation:test_fixture.Case.test_ok': {'run': 3, 'commit': 'a' * 40}}}
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            main.emit_snapshot(journal, 'start')
            main.emit_snapshot(dict(journal, complete=True), 'final')
        lines = output.getvalue().splitlines()
        self.assertEqual(main.read_snapshots(lines)['start'], journal)
        for invalid in (lines[:-1], lines + lines, [line.replace('CHUNK start 0 ', 'CHUNK start 1 ') for line in lines],
                        [line + 'A' if line.startswith('MAIN-UNIT-CHUNK') else line for line in lines]):
            with self.subTest(lines=invalid), self.assertRaises(ReceiptError):
                main.read_snapshots(invalid)

    def test_completed_framed_log_preserves_passes_after_job_timeout(self):
        import contextlib
        import io
        scope = {'repository': 1, 'pr': 7, 'branch': 'topic', 'base': 'main', 'workflow': main.WORKFLOW}
        commit, rid = 'a' * 40, 10
        cached = {'validation:test_fixture.Case.test_cached': {'run': 9, 'commit': 'b' * 40}}
        fresh = {'operations:test_fixture.Case.test_ok': {'run': rid, 'commit': commit}}
        start = {'version': main.receipts.VERSION, 'scope': scope, 'run': rid, 'commit': commit,
                 'complete': False, 'fixture_errors': [], 'passes': cached}
        final = dict(start, complete=True, passes={**cached, **fresh})
        def log(last=final):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                print(commit, commit, 'triggered by event: pull_request', sep='\n')
                main.emit_snapshot(start, 'start')
                main.emit_snapshot(last, 'final')
                print('context deadline exceeded')
            return output.getvalue().encode()
        api = unittest.mock.Mock()
        api.bytes.return_value = log()
        prior = {'id': rid, 'commit_sha': commit}
        for status in ('success', 'failure', 'cancelled'):
            with self.subTest(status=status):
                recovered = main.recover_log(api, scope, prior, {'id': 20, 'status': status})
                self.assertEqual(recovered['passes'], {**cached, **fresh})
        for last in (dict(final, complete=False), dict(final, passes=fresh),
                     dict(final, fixture_errors=['discovery failed'])):
            api.bytes.return_value = log(last)
            with self.subTest(last=last), self.assertRaises(ReceiptError):
                main.recover_log(api, scope, prior, {'id': 20, 'status': 'failure'})
        api.bytes.return_value = log()
        for status in ('running', 'waiting', 'skipped'):
            with self.subTest(status=status), self.assertRaises(ReceiptError):
                main.recover_log(api, scope, prior, {'id': 20, 'status': status})


    def test_event_authentication_rejects_other_pr_source_repository_and_workflow(self):
        scope = {'repository': 1, 'pr': 7, 'branch': 'topic', 'base': 'main', 'workflow': main.WORKFLOW}
        run = {'id': 10, 'workflow_id': main.WORKFLOW, 'prettyref': '#7', 'event': 'pull_request', 'commit_sha': 'a' * 40,
               'event_payload': {'repository': {'id': 1}, 'number': 7, 'pull_request': {
                   'number': 7, 'head': {'ref': 'topic', 'sha': 'a' * 40, 'repo': {'id': 1}},
                   'base': {'ref': 'main', 'repo': {'id': 1}}}}}
        main.authenticate_run(scope, run)
        for field, value in (('workflow_id', 'effort-ci.yml'), ('prettyref', '#8'), ('event', 'push'), ('commit_sha', 'b' * 40)):
            invalid = copy.deepcopy(run)
            invalid[field] = value
            with self.subTest(field=field), self.assertRaises(ReceiptError):
                main.authenticate_run(scope, invalid)
        invalid = copy.deepcopy(run)
        invalid['event_payload']['pull_request']['head']['repo']['id'] = 2
        with self.assertRaises(ReceiptError):
            main.authenticate_run(scope, invalid)

    def test_main_skip_is_retained_as_skip_and_never_as_pass(self):
        class Fixture(unittest.TestCase):
            def test_skip(self):
                self.skipTest('optional upstream checkout')
        with tempfile.TemporaryDirectory() as directory:
            journal = {'commit': 'a' * 40, 'run': 1, 'applicability_commit': 'a' * 40, 'passes': {}, 'fixture_errors': [], 'complete': False}
            suites = {name: [Fixture('test_skip')] for name in main.receipts.SUITES}
            self.assertEqual(main.execute(journal, Path(directory) / 'receipt.json', suites), 0)
            self.assertEqual(journal['passes'], {})
            self.assertEqual(len(journal['skips']), 2)
            self.assertTrue(journal['complete'])

    def test_main_failure_blocks_and_cached_success_never_executes(self):
        class Fixture(unittest.TestCase):
            def test_cached(self):
                raise AssertionError('a passed method must never execute again')
            def test_failed(self):
                self.fail('still failed')
        with tempfile.TemporaryDirectory() as directory:
            journal = {'commit': 'a' * 40, 'run': 1, 'applicability_commit': 'a' * 40, 'passes': {}, 'fixture_errors': [], 'complete': False}
            suites = {name: [Fixture('test_cached'), Fixture('test_failed')] for name in main.receipts.SUITES}
            for name in suites:
                journal['passes'][name + ':' + Fixture('test_cached').id()] = {'commit': 'a' * 40, 'run': 1}
            self.assertEqual(main.execute(journal, Path(directory) / 'receipt.json', suites), 1)
            self.assertEqual(len(journal['passes']), 2)

    def test_main_unknown_runner_outcome_refuses_complete_receipt(self):
        class Fixture(unittest.TestCase):
            def test_silent(self):
                pass
            def run(self, result):
                return result
        with tempfile.TemporaryDirectory() as directory:
            journal = {'commit': 'a' * 40, 'run': 1, 'applicability_commit': 'a' * 40, 'passes': {}, 'fixture_errors': [], 'complete': False}
            suites = {name: [Fixture('test_silent')] for name in main.receipts.SUITES}
            with self.assertRaises(ReceiptError):
                main.execute(journal, Path(directory) / 'receipt.json', suites)
            self.assertFalse(journal['complete'])
