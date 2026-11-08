"""The bounded publisher authenticates each required job independently."""
import copy
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from validation import publish_cinema_main as publish
from validation.python_unit_receipts import ReceiptError


class CinemaPublisherCase(unittest.TestCase):
    def test_build_tree_binds_content_and_modes_outside_the_two_controls(self):
        with tempfile.TemporaryDirectory(prefix='cinema-publisher-') as directory:
            root = Path(directory)
            def git(*args):
                subprocess.run(['git', *args], cwd=root, check=True,
                               capture_output=True)
            git('init', '-q')
            git('config', 'core.hooksPath', '/dev/null')
            git('config', 'user.name', 'Publisher test')
            git('config', 'user.email', 'publisher@example.invalid')
            app = root / 'app'
            app.write_text('reviewed')
            control = root / 'validation/publish_cinema_main.py'
            control.parent.mkdir()
            control.write_text('initial controller')
            git('add', '.')
            git('commit', '-qm', 'initial')
            expected = publish.build_tree(root)
            control.write_text('final authenticated bindings')
            git('add', '.')
            git('commit', '-qm', 'control')
            self.assertEqual(publish.build_tree(root), expected)
            app.write_text('changed application')
            git('add', '.')
            git('commit', '-qm', 'application')
            self.assertNotEqual(publish.build_tree(root), expected)
            app.write_text('reviewed')
            app.chmod(0o755)
            git('add', '.')
            git('commit', '-qm', 'mode')
            self.assertNotEqual(publish.build_tree(root), expected)

    def test_only_main_first_parent_commits_containing_the_landing_publish(self):
        with tempfile.TemporaryDirectory(prefix='cinema-main-lineage-') as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(['git', *args], cwd=root, text=True).strip()
            git('init', '-q', '-b', 'main')
            git('config', 'core.hooksPath', '/dev/null')
            git('config', 'user.name', 'Publisher test')
            git('config', 'user.email', 'publisher@example.invalid')
            (root / 'initial').write_text('base')
            git('add', '.')
            git('commit', '-qm', 'initial')
            initial = git('rev-parse', 'HEAD')
            git('checkout', '-qb', 'task')
            (root / 'task').write_text('task change')
            git('add', '.')
            git('commit', '-qm', 'task')
            task = git('rev-parse', 'HEAD')
            git('checkout', '-q', 'main')
            (root / 'landing').write_text('Cinema landing')
            git('add', '.')
            git('commit', '-qm', 'landing')
            landing = git('rev-parse', 'HEAD')
            git('merge', '--no-ff', '-qm', 'merge task', 'task')
            current = git('rev-parse', 'HEAD')
            with patch.object(publish, 'LANDING', landing):
                publish.require_main_commit(root, landing, current)
                publish.require_main_commit(root, current, current)
                with self.assertRaises(ReceiptError):
                    publish.require_main_commit(root, task, current)
                with self.assertRaises(ReceiptError):
                    publish.require_main_commit(root, initial, current)

    def test_required_job_must_succeed_on_its_reviewed_controller(self):
        expected = (12, 34, 'required lane', 'reviewed.yml', 'a' * 40)
        run = {'id': 12, 'repository': {'id': 9}, 'workflow_id': 'reviewed.yml',
               'commit_sha': 'a' * 40, 'trigger_event': 'workflow_dispatch',
               'status': 'failure'}
        job = {'id': 34, 'name': 'required lane', 'run_id': 12, 'repo_id': 9,
               'attempt': 1, 'status': 'success', 'task_id': 56}

        class API:
            def get(self, path):
                self_outer.assertEqual(path, '/actions/runs/12')
                return run

            def pages(self, path):
                self_outer.assertEqual(path, '/actions/runs/12/jobs')
                return [job]

        self_outer = self
        api = API()
        self.assertEqual(publish.successful_job(api, 9, expected)['job_id'], 34)
        original = copy.deepcopy(job)
        for field, value in [('status', 'failure'), ('attempt', 2), ('task_id', 0),
                             ('repo_id', 10), ('run_id', 13), ('name', 'different lane')]:
            with self.subTest(field=field):
                job.update(original)
                job[field] = value
                with self.assertRaises(ReceiptError):
                    publish.successful_job(api, 9, expected)
        job.update(original)
        run['commit_sha'] = 'b' * 40
        with self.assertRaises(ReceiptError):
            publish.successful_job(api, 9, expected)

    def test_native_completion_requires_explicit_single_expected_success(self):
        name = 'fixture::one'
        for output in (
            'test fixture::one ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n',
            'test fixture::one ... diagnostic\nok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n',
        ):
            publish.require_single_native_pass(output, name)
        for output in (
            'test fixture::other ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n',
            'test fixture::one ... pending\n\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n',
            *('test fixture::one ... ' + disposition + '\nok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n'
              for disposition in ('FAILED', 'ignored', 'diagnostic\nFAILED', 'diagnostic\nignored')),
        ):
            with self.subTest(output=output), self.assertRaises(ReceiptError):
                publish.require_single_native_pass(output, name)

    def test_native_partition_refuses_missing_overlapping_and_unknown_names(self):
        inventory = {str(i) for i in range(4927)}
        ignored = {str(i) for i in range(49)}
        passing = inventory - ignored
        publish.validate_daemon_partition(inventory, ignored, passing)
        for invalid in (passing - {'49'}, passing | {'0'}, passing | {'unknown'}):
            with self.subTest(invalid=len(invalid)), self.assertRaises(ReceiptError):
                publish.validate_daemon_partition(inventory, ignored, invalid)

    def test_filtered_native_outcomes_keep_targets_and_refuse_replayed_names(self):
        rust = {'retained': {'fixture@src/lib.rs': ['retained']}}
        raw = ('    Running unittests src/lib.rs (/x/fixture-aaaaaaaaaaaaaaaa)\n'
               'fixture@src/lib.rs: 2 discovered, 1 authenticated passes, 1 selected\n'
               'running 1 test\ntest fresh ... ok\n'
               'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.01s\n'
               '    Running unittests src/main.rs (/x/plurxd-bbbbbbbbbbbbbbbb)\n'
               'plurxd@src/main.rs: 1 discovered, 0 authenticated passes, 1 selected\n'
               'running 1 test\nfatal runtime error: stack overflow, aborting\n')
        result = publish.partial_workspace_outcomes(raw.encode(), rust)
        self.assertEqual(result['fixture@src/lib.rs']['outcomes'], {'fresh': 'pass'})
        self.assertTrue(result['plurxd@src/main.rs']['interrupted'])
        for invalid in (raw.replace('test fresh ...', 'test retained ...'),
                        raw.replace('test fresh ... ok', 'test fresh ... ok\ntest fresh ... ok'),
                        raw.replace('1 filtered out', '0 filtered out')):
            with self.subTest(invalid=invalid), self.assertRaises(ReceiptError):
                publish.partial_workspace_outcomes(invalid.encode(), rust)

    def test_evidence_cache_preserves_values_and_refreshes_main(self):
        class API:
            def __init__(self):
                self.calls = 0

            def get(self, path, query=None):
                self.calls += 1
                return {'values': [self.calls]}

        origin = API()
        cached = publish.CachedReadAPI(origin)
        first = cached.get('/actions/runs/1')
        first['values'].append('mutated')
        self.assertEqual(cached.get('/actions/runs/1'), {'values': [1]})
        self.assertEqual(origin.calls, 1)
        self.assertEqual(cached.get('/branches/main'), {'values': [2]})
        self.assertEqual(cached.get('/branches/main'), {'values': [3]})


if __name__ == '__main__':
    unittest.main()
