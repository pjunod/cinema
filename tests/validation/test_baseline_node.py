"""Script-level diagnostics retain exact Make commands and continue after failures."""
from contextlib import nullcontext
import io
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

from validation import baseline_node as baseline
from validation.test_cell_manifest import ManifestError


class NodeBaselineTests(unittest.TestCase):
    def setUp(self):
        self.context = {'candidate_sha': 'a' * 40, 'candidate_tree': 'b' * 40,
                        'cell_id': 'node-units', 'framework': 'node-script', 'target': 'linux-x86_64',
                        'profile': 'node', 'features': [], 'toolchain': 'Node 22.23.2',
                        'include_desktop_remote': True}
        self.environment = {'PLURX_TEST_CANDIDATE_SHA': 'a' * 40,
                            'PLURX_TEST_CANDIDATE_TREE': 'b' * 40}

    @staticmethod
    def process_result(returncode=0, *, timed_out=False, cleanup_complete=True, **extra):
        return dict(returncode=returncode, timed_out=timed_out, cleanup_complete=cleanup_complete,
                    success=returncode == 0 and not timed_out and cleanup_complete,
                    started_utc='2026-10-20T00:00:00Z', finished_utc='2026-10-20T00:00:01Z',
                    seconds=1.0, **extra)

    def collect(self, run=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'Makefile').write_text('web-unit-check:\n\t@node tests/a.js\n\t@node --test tests/b.js\n\nnext:\n\t@node ignored.js\n')
            extension = root / 'clients/desktop-remote/tests/extension.test.mjs'
            extension.parent.mkdir(parents=True)
            extension.write_text("console.log('extension actual stdout');console.error('extension actual stderr');\n")
            (root / 'tests').mkdir()
            (root / 'tests/a.js').write_text("console.log('first actual stdout');console.error('first actual stderr');\n")
            (root / 'tests/b.js').write_text("console.log('second actual stdout');console.error('second actual stderr');\n")
            receipt = root / 'receipt.json'
            receipt.write_text(json.dumps({'candidate_sha': 'a' * 40, 'candidate_tree': 'b' * 40,
                                           'archive_sha256': 'c' * 64}))
            executor = mock.patch.object(baseline, 'run_process', side_effect=run) if run is not None else nullcontext(None)
            with executor as execute, mock.patch('sys.stdout', new=io.StringIO()):
                report = baseline.collect(self.context, root, root / 'logs',
                                          environment=self.environment, source_receipt=receipt)
                for row in report['commands']:
                    if row.get('log'):
                        row['captured_log_for_test'] = Path(row['log']).read_text()
                return report, execute.call_args_list if execute is not None else []

    def test_commands_after_failure_still_run_and_parent_scripts_are_preserved(self):
        report, calls = self.collect([self.process_result(1), self.process_result(), self.process_result()])
        self.assertEqual(len(calls), 3)
        self.assertEqual(calls[0].args[0], ['node', 'tests/a.js'])
        self.assertEqual(calls[1].args[0], ['node', '--test', 'tests/b.js'])
        self.assertEqual(report['counts']['discovered'], 3)
        self.assertTrue(report['complete'])
        self.assertFalse(report['passed'])
        self.assertEqual(report['granularity'], 'recipe-command')

    def test_timeout_is_recorded_and_does_not_skip_remaining_commands(self):
        report, calls = self.collect([self.process_result(-9, timed_out=True, cleanup_signals=['SIGTERM', 'SIGKILL']),
                                      self.process_result(), self.process_result()])
        self.assertEqual(len(calls), 3)
        self.assertTrue(report['commands'][0]['timed_out'])
        self.assertEqual(report['commands'][0]['returncode'], -9)
        self.assertEqual(report['commands'][0]['cleanup_signals'], ['SIGTERM', 'SIGKILL'])
        self.assertTrue(report['commands'][0]['cleanup_complete'])
        self.assertFalse(report['passed'])

    def test_source_changes_cannot_be_hidden_by_successful_node_exit_codes(self):
        with mock.patch.object(baseline, 'verify_source', side_effect=[
                {'mode': 'git'}, ManifestError('Checkout must be clean')]):
            report, _ = self.collect([self.process_result()] * 3)
        self.assertEqual(report['counts']['passed'], 3)
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])
        self.assertIn('Source changed', report['scope_errors'][0])

    def test_unqualified_cleanup_blocks_and_accounts_for_all_remaining_commands(self):
        execution = self.process_result(0, cleanup_complete=False,
                                        cleanup_error='live or unknown group survived',
                                        cleanup_signals=['SIGTERM', 'SIGKILL'])
        report, calls = self.collect([execution])
        self.assertEqual(len(calls), 1)
        self.assertEqual(report['blocked_identities'], ['web-unit-check:002', 'desktop-remote-extension'])
        self.assertEqual(report['commands'][0]['returncode'], 0)
        self.assertFalse(report['commands'][0]['success'])
        self.assertEqual([row['execution_status'] for row in report['commands']], ['executed', 'blocked', 'blocked'])
        self.assertEqual([row['status'] for row in report['outcomes']], ['error', 'failed', 'error'])
        self.assertEqual(report['counts']['discovered'], 3)
        self.assertEqual(report['counts']['outcomes'], 3)
        self.assertTrue(report['scope_errors'])
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])
        for row in report['commands'][1:]:
            self.assertIsNone(row['returncode'])
            self.assertIsNone(row['duration_seconds'])
            self.assertIsNone(row['log'])

    def test_zero_exit_with_live_leak_is_failed_but_cleaned_group_allows_followups(self):
        first = self.process_result(leaked_process_group=True)
        first['success'] = False
        report, calls = self.collect([first, self.process_result(), self.process_result()])
        self.assertEqual(len(calls), 3)
        self.assertEqual(report['counts']['failed'], 1)
        self.assertEqual(report['counts']['passed'], 2)
        self.assertTrue(report['complete'])
        self.assertFalse(report['passed'])

    def test_timed_out_unclean_group_blocks_followups_with_native_exit_preserved(self):
        report, calls = self.collect([self.process_result(
            -9, timed_out=True, cleanup_complete=False,
            cleanup_error='unknown group survived timeout cleanup')])
        self.assertEqual(len(calls), 1)
        self.assertTrue(report['commands'][0]['timed_out'])
        self.assertEqual(report['commands'][0]['returncode'], -9)
        self.assertEqual(report['blocked_identities'], ['web-unit-check:002', 'desktop-remote-extension'])
        self.assertEqual(report['counts']['outcomes'], report['counts']['discovered'])
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])

    def test_executor_receives_merged_streams_and_native_evidence_is_preserved(self):
        def run(command, stdout, stderr, timeout, **options):
            self.assertIs(stderr, subprocess.STDOUT)
            self.assertEqual(timeout, 180)
            self.assertIsInstance(options['cwd'], Path)
            stdout.write(b'combined stdout and stderr\n')
            result = self.process_result(group_observations=[{'status': 'nonexecuting'}])
            result['seconds'] = 2.75
            return result
        report, calls = self.collect(run)
        self.assertEqual(len(calls), 3)
        self.assertTrue(report['passed'])
        for row in report['commands']:
            self.assertEqual(row['duration_seconds'], 2.75)
            self.assertEqual(row['group_observations'], [{'status': 'nonexecuting'}])
            self.assertIn('combined stdout and stderr', row['captured_log_for_test'])

    @unittest.skipUnless(shutil.which('node'), 'Node runtime unavailable for actual temporary script')
    def test_actual_temporary_node_scripts_run_through_owned_executor(self):
        report, _ = self.collect()
        self.assertTrue(report['passed'], report.get('scope_errors'))
        self.assertEqual(report['counts']['passed'], 3)
        self.assertEqual(report['blocked_identities'], [])
        self.assertIn('first actual stdout', report['commands'][0]['captured_log_for_test'])
        self.assertIn('first actual stderr', report['commands'][0]['captured_log_for_test'])
        self.assertTrue(all(row['cleanup_complete'] for row in report['commands']))

    def test_unsupported_recipe_cannot_silently_omit_a_command(self):
        for command in ['echo passed', 'node tests/*.js', 'node a.js && node b.js', '$(MAKE) other']:
            with self.subTest(command=command), self.assertRaises(ManifestError):
                baseline.make_commands('web-unit-check:\n\t@' + command + '\n')

    def test_comments_and_other_targets_do_not_become_tests(self):
        commands = baseline.make_commands('web-unit-check:\n\t# node absent.js\n\t@node --test a.js b.js\n\nother:\n\t@node absent.js\n')
        self.assertEqual(commands, [['node', '--test', 'a.js', 'b.js']])

    def test_missing_target_is_a_discovery_error(self):
        with self.assertRaisesRegex(ManifestError, 'Missing Make target'):
            baseline.make_commands('other:\n\t@node absent.js\n')


if __name__ == '__main__':
    unittest.main()
