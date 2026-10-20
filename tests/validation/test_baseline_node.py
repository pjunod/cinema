"""Script-level diagnostics retain exact Make commands and continue after failures."""
import io
import json
from pathlib import Path
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

    def collect(self, run):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'Makefile').write_text('web-unit-check:\n\t@node tests/a.js\n\t@node --test tests/b.js\n\nnext:\n\t@node ignored.js\n')
            extension = root / 'clients/desktop-remote/tests/extension.test.mjs'
            extension.parent.mkdir(parents=True)
            extension.write_text('')
            receipt = root / 'receipt.json'
            receipt.write_text(json.dumps({'candidate_sha': 'a' * 40, 'candidate_tree': 'b' * 40,
                                           'archive_sha256': 'c' * 64}))
            with mock.patch.object(baseline.subprocess, 'run', side_effect=run) as execute, \
                    mock.patch('sys.stdout', new=io.StringIO()):
                report = baseline.collect(self.context, root, root / 'logs',
                                          environment=self.environment, source_receipt=receipt)
                return report, execute.call_args_list

    def test_commands_after_failure_still_run_and_parent_scripts_are_preserved(self):
        report, calls = self.collect([subprocess.CompletedProcess([], 1),
                                      subprocess.CompletedProcess([], 0),
                                      subprocess.CompletedProcess([], 0)])
        self.assertEqual(len(calls), 3)
        self.assertEqual(calls[0].args[0], ['node', 'tests/a.js'])
        self.assertEqual(calls[1].args[0], ['node', '--test', 'tests/b.js'])
        self.assertEqual(report['counts']['discovered'], 3)
        self.assertTrue(report['complete'])
        self.assertFalse(report['passed'])
        self.assertEqual(report['granularity'], 'recipe-command')

    def test_timeout_is_recorded_and_does_not_skip_remaining_commands(self):
        report, calls = self.collect([subprocess.TimeoutExpired(['node'], 180),
                                      subprocess.CompletedProcess([], 0),
                                      subprocess.CompletedProcess([], 0)])
        self.assertEqual(len(calls), 3)
        self.assertTrue(report['commands'][0]['timed_out'])
        self.assertEqual(report['commands'][0]['returncode'], 124)
        self.assertFalse(report['passed'])

    def test_source_changes_cannot_be_hidden_by_successful_node_exit_codes(self):
        with mock.patch.object(baseline, 'verify_source', side_effect=[
                {'mode': 'git'}, ManifestError('Checkout must be clean')]):
            report, _ = self.collect([subprocess.CompletedProcess([], 0)] * 3)
        self.assertEqual(report['counts']['passed'], 3)
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])
        self.assertIn('Source changed', report['scope_errors'][0])

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
