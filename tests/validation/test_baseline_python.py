"""Unittest baseline evidence must retain native discovery and failure semantics."""
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from validation import baseline_python as baseline
from validation.test_cell_manifest import ManifestError


class PythonBaselineTests(unittest.TestCase):
    def setUp(self):
        self.context = {
            'candidate_sha': 'a' * 40, 'candidate_tree': 'b' * 40,
            'cell_id': 'python-validation', 'framework': 'unittest',
            'target': 'linux-x86_64', 'profile': 'unittest', 'features': [],
            'toolchain': 'Python 3.14', 'suite_dir': 'tests/validation',
        }
        self.environment = {'PLURX_TEST_CANDIDATE_SHA': 'a' * 40,
                            'PLURX_TEST_CANDIDATE_TREE': 'b' * 40}

    def fixture(self, root):
        directory = root / 'tests/validation'
        directory.mkdir(parents=True)
        receipt = root / 'source-receipt.json'
        receipt.write_text(json.dumps({'candidate_sha': 'a' * 40, 'candidate_tree': 'b' * 40,
                                       'archive_sha256': 'c' * 64}))
        return receipt

    def collect_suite(self, suite, errors=()):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            loader = mock.Mock(errors=list(errors))
            loader.discover.return_value = suite
            result = baseline.collect(self.context, 'tests/validation', root,
                                      environment=self.environment, source_receipt=receipt,
                                      loader=loader, stream=io.StringIO())
            return result

    def test_native_methods_are_discovered_before_execution_and_timed(self):
        class Scenario(unittest.TestCase):
            def case_pass(self):
                self.assertTrue(True)
        test = Scenario('case_pass')
        report = self.collect_suite(unittest.TestSuite([test]))
        self.assertEqual(report['discovered'], [test.id()])
        self.assertEqual(report['outcomes'], [{'id': test.id(), 'status': 'passed'}])
        self.assertTrue(report['complete'])
        self.assertTrue(report['passed'])
        self.assertGreaterEqual(report['durations_seconds'][test.id()], 0)
        self.assertEqual(report['source_provenance']['mode'], 'source-transfer-receipt')

    def test_failing_subtest_cannot_be_erased_by_later_success(self):
        class Scenario(unittest.TestCase):
            def case_subtests(self):
                with self.subTest(value=1):
                    self.fail('first subtest fails')
                with self.subTest(value=2):
                    self.assertTrue(True)
        test = Scenario('case_subtests')
        report = self.collect_suite(unittest.TestSuite([test]))
        self.assertEqual(report['outcomes'], [{'id': test.id(), 'status': 'failed'}])
        self.assertTrue(report['complete'])
        self.assertFalse(report['passed'])

    def test_skipped_subtest_is_visible_on_its_discovered_parent(self):
        class Scenario(unittest.TestCase):
            def case_subtests(self):
                with self.subTest(value=1):
                    self.skipTest('missing capability')
        test = Scenario('case_subtests')
        report = self.collect_suite(unittest.TestSuite([test]))
        self.assertEqual(report['outcomes'], [{'id': test.id(), 'status': 'skipped'}])
        self.assertEqual(report['unqualified_skips'], [test.id()])
        self.assertFalse(report['passed'])

    def test_expected_failure_is_recorded_as_unqualified_skip(self):
        class Scenario(unittest.TestCase):
            @unittest.expectedFailure
            def case_expected_failure(self):
                self.fail('known issue')
        test = Scenario('case_expected_failure')
        report = self.collect_suite(unittest.TestSuite([test]))
        self.assertEqual(report['unqualified_skips'], [test.id()])
        self.assertEqual(report['observations'][0]['detail'], 'unittest expected failure')
        self.assertFalse(report['passed'])

    def test_class_setup_failure_does_not_fabricate_method_success(self):
        class Scenario(unittest.TestCase):
            @classmethod
            def setUpClass(cls):
                raise RuntimeError('fixture startup failed')
            def case_never_reached(self):
                self.fail('must not run')
        test = Scenario('case_never_reached')
        report = self.collect_suite(unittest.TestSuite([test]))
        self.assertEqual(report['missing'], [test.id()])
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])
        self.assertIn('fixture startup failed', report['observations'][0]['detail'])

    def test_discovery_import_error_is_not_a_zero_test_success(self):
        report = self.collect_suite(unittest.TestSuite(), errors=['ImportError in source'])
        self.assertEqual(report['discovery_errors'], ['ImportError in source'])
        self.assertEqual(report['discovery_returncode'], 1)
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])

    def test_syntax_error_is_preserved_as_native_discovery_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            (root / 'tests/validation/test_baseline_broken_fixture.py').write_text('def broken(\n')
            report = baseline.collect(self.context, 'tests/validation', root,
                                      environment=self.environment, source_receipt=receipt,
                                      stream=io.StringIO())
            self.assertFalse(report['complete'])
            self.assertFalse(report['passed'])
            self.assertEqual(report['discovery_returncode'], 1)
            self.assertTrue(report['discovery_errors'])
            self.assertTrue(report['source_inspection_errors'])

    def test_plain_function_control_is_visible_without_inventing_a_native_case(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            (root / 'tests/validation/test_baseline_manual_fixture.py').write_text(
                'def test_manual_renderer(helper):\n    raise AssertionError("must not execute")\n')
            report = baseline.collect(self.context, 'tests/validation', root,
                                      environment=self.environment, source_receipt=receipt,
                                      stream=io.StringIO())
            self.assertEqual(report['counts']['discovered'], 0)
            self.assertFalse(report['complete'])
            self.assertFalse(report['passed'])
            self.assertIn('test_manual_renderer', report['scope_errors'][0])
            self.assertEqual(report['unregistered_function_controls'][0]['functions'],
                             ['test_manual_renderer'])

    def test_exact_file_allowlist_binds_fixture_argv_and_excludes_driver_import(self):
        import sys
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            suite_directory = root / 'tests/validation'
            name = 'test_baseline_selected_fixture.py'
            (suite_directory / name).write_text(
                'import sys, unittest\n'
                'FIXTURE = sys.argv.pop(1)\n'
                'class PortableFixture(unittest.TestCase):\n'
                '    def test_bound_fixture(self):\n'
                '        self.assertEqual(FIXTURE, "prepared-evidence")\n')
            (suite_directory / 'test_baseline_excluded_driver.py').write_text(
                'raise RuntimeError("unsafe campaign must not be imported")\n')
            context = dict(self.context, test_files=[name], discovery_argv=['prepared-evidence'])
            original_argv = sys.argv
            report = baseline.collect(context, 'tests/validation', root,
                                      environment=self.environment, source_receipt=receipt,
                                      stream=io.StringIO())
            self.assertIs(sys.argv, original_argv)
            self.assertTrue(report['passed'])
            self.assertEqual(report['counts']['discovered'], 1)
            self.assertEqual(report['context']['test_files'], [name])
            self.assertEqual(report['context']['discovery_argv'], ['prepared-evidence'])
            changed = dict(context, discovery_argv=['different-evidence'])
            self.assertNotEqual(baseline.canonical_context(context)[1],
                                baseline.canonical_context(changed)[1])

    def test_fixture_argv_is_reset_per_file_and_restored_after_import_error(self):
        import sys
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            names = ['test_baseline_argv_first.py', 'test_baseline_argv_second.py']
            for name in names:
                (root / 'tests/validation' / name).write_text(
                    'import sys, unittest\n'
                    'FIXTURE = sys.argv.pop(1)\n'
                    'class Fixture(unittest.TestCase):\n'
                    '    def test_argument(self):\n'
                    '        self.assertEqual(FIXTURE, "fixture")\n')
            context = dict(self.context, test_files=names, discovery_argv=['fixture'])
            original_argv = sys.argv
            report = baseline.collect(context, 'tests/validation', root,
                                      environment=self.environment, source_receipt=receipt,
                                      stream=io.StringIO())
            self.assertIs(sys.argv, original_argv)
            self.assertTrue(report['passed'])
            self.assertEqual(report['counts']['discovered'], 2)
            broken = 'test_baseline_argv_error.py'
            (root / 'tests/validation' / broken).write_text(
                'import sys\nsys.argv.pop(1)\nraise ImportError("fixture import failure")\n')
            context = dict(context, test_files=[broken])
            report = baseline.collect(context, 'tests/validation', root,
                                      environment=self.environment, source_receipt=receipt,
                                      stream=io.StringIO())
            self.assertIs(sys.argv, original_argv)
            self.assertFalse(report['complete'])
            self.assertTrue(report['discovery_errors'])

    def test_file_allowlist_cannot_escape_directory_or_select_globs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            for name in ['../test_driver.py', 'test_*.py', '/tmp/test_driver.py']:
                context = dict(self.context, test_files=[name], discovery_argv=['fixture'])
                with self.subTest(name=name), self.assertRaises(ManifestError):
                    baseline.collect(context, 'tests/validation', root,
                                     environment=self.environment, source_receipt=receipt)

    def entrypoint(self, source):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            name = 'test_entrypoint_fixture.py'
            (root / 'tests/validation' / name).write_text(source)
            context = dict(self.context, mode='entrypoint', entrypoint=name, test_files=[name],
                           cwd='tests/validation', discovery_argv=['prepared-fixture'])
            return baseline.collect(context, 'tests/validation', root,
                                    environment=self.environment, source_receipt=receipt,
                                    stream=io.StringIO())

    def test_entrypoint_runs_main_initialization_with_native_suite_and_exit(self):
        report = self.entrypoint(
            'import sys, unittest\n'
            'class Fixture(unittest.TestCase):\n'
            '    def test_main_setup(self):\n'
            '        self.assertEqual(SOURCE, "prepared-fixture")\n'
            'if __name__ == "__main__":\n'
            '    SOURCE = sys.argv[1]\n'
            '    unittest.main(argv=[sys.argv[0]])\n')
        self.assertTrue(report['passed'])
        self.assertEqual(report['framework_calls'], 1)
        self.assertEqual(report['counts']['discovered'], 1)
        self.assertEqual(report['discovered'], ['__main__.Fixture.test_main_setup'])
        self.assertEqual(report['execution_returncode'], 0)

    def test_entrypoint_preserves_failure_exit_and_restores_process_state(self):
        import sys
        original_argv, original_path, original_cwd = sys.argv, list(sys.path), Path.cwd()
        original_main = unittest.main
        report = self.entrypoint(
            'import unittest\n'
            'class Fixture(unittest.TestCase):\n'
            '    def test_failure(self):\n'
            '        self.fail("actual failure")\n'
            'if __name__ == "__main__":\n'
            '    unittest.main(argv=["fixture"])\n')
        self.assertTrue(report['complete'])
        self.assertFalse(report['passed'])
        self.assertEqual(report['execution_returncode'], 1)
        self.assertEqual(report['counts']['failed'], 1)
        self.assertIs(sys.argv, original_argv)
        self.assertEqual(sys.path, original_path)
        self.assertEqual(Path.cwd(), original_cwd)
        self.assertIs(unittest.main, original_main)

    def test_entrypoint_that_changes_source_cannot_claim_committed_candidate(self):
        with mock.patch.object(baseline, 'verify_source', side_effect=[
                {'mode': 'git'}, ManifestError('Checkout must be clean')]):
            report = self.entrypoint(
                'import unittest\n'
                'class Fixture(unittest.TestCase):\n'
                '    def test_pass(self):\n'
                '        self.assertTrue(True)\n'
                'unittest.main(argv=["fixture"])\n')
        self.assertEqual(report['counts']['passed'], 1)
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])
        self.assertIn('Source changed', report['scope_errors'][0])

    def test_entrypoint_without_framework_call_is_incomplete(self):
        report = self.entrypoint('import unittest\nif False:\n    unittest.main()\n')
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])
        self.assertEqual(report['framework_calls'], 0)

    def test_entrypoint_cannot_run_a_nonunit_campaign(self):
        with self.assertRaisesRegex(ManifestError, 'non-unit campaign'):
            self.entrypoint('raise AssertionError("campaign must not execute")\n')

    def test_repeated_entrypoint_framework_runs_are_not_one_complete_cell(self):
        report = self.entrypoint('import unittest\n'
                                 'unittest.main(argv=["fixture"], exit=False)\n'
                                 'unittest.main(argv=["fixture"], exit=False)\n')
        self.assertEqual(report['framework_calls'], 2)
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])

    def test_candidate_mismatch_refuses_before_loader_imports_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            loader = mock.Mock()
            with self.assertRaisesRegex(ManifestError, 'PLURX_TEST_CANDIDATE_SHA'):
                baseline.collect(self.context, 'tests/validation', root, environment={},
                                 source_receipt=receipt, loader=loader)
            loader.discover.assert_not_called()

    def test_suite_binding_cannot_discover_another_directory(self):
        with self.assertRaisesRegex(ManifestError, 'suite_dir'):
            baseline.collect(self.context, 'tests/operations', Path.cwd(), environment={})

    def test_source_only_checkout_requires_bound_transfer_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = self.fixture(root)
            with self.assertRaisesRegex(ManifestError, 'transfer receipt'):
                baseline.verify_source(self.context, root, self.environment)
            data = json.loads(receipt.read_text())
            data['candidate_tree'] = 'd' * 40
            receipt.write_text(json.dumps(data))
            with self.assertRaisesRegex(ManifestError, 'candidate_tree'):
                baseline.verify_source(self.context, root, self.environment, receipt)

    def test_dirty_git_source_cannot_claim_committed_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / '.git').mkdir()
            with mock.patch.object(baseline.subprocess, 'check_output', side_effect=[
                    'a' * 40 + '\n', 'b' * 40 + '\n', ' M tests/validation/test_foo.py\n']):
                with self.assertRaisesRegex(ManifestError, 'clean'):
                    baseline.verify_source(self.context, root, self.environment)


if __name__ == '__main__':
    unittest.main()
