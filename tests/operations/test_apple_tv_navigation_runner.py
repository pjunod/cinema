"""A successful xcodebuild exit alone cannot qualify an empty/skipped UI run."""
import importlib.machinery
import importlib.util
import json
import os
import subprocess
import tempfile
from unittest.mock import patch
from pathlib import Path
import unittest
import re


PATH = Path(__file__).resolve().parents[2] / "scripts/apple-tv-navigation-test"
SPEC = importlib.util.spec_from_loader("tv_navigation_runner", importlib.machinery.SourceFileLoader(
    "tv_navigation_runner", str(PATH)))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class TVNavigationRunnerTests(unittest.TestCase):
    def test_exported_screenshots_use_portable_distinct_names(self):
        # This real capture name rejected the entire CI evidence upload.
        name = RUNNER.screenshot_name("Down reaches library:filter.png")
        self.assertNotIn(":", name)
        self.assertTrue(name.endswith(".png"))
        self.assertNotEqual(name, RUNNER.screenshot_name("Down reaches library?filter.png"))
        self.assertEqual(RUNNER.screenshot_name("Guide first row.png"), "Guide first row.png")
        for invalid in '<>:"/\\|?*\r\n\x00':
            self.assertNotIn(invalid, RUNNER.screenshot_name("Capture" + invalid + ".png"))

    def test_requires_every_native_journey_to_execute_and_pass(self):
        RUNNER.verify_summary({"passedTests": 2, "failedTests": 0, "skippedTests": 0}, 2)
        for summary in [
            {},
            {"passedTests": 0, "failedTests": 0, "skippedTests": 0},
            {"passedTests": 1, "failedTests": 0, "skippedTests": 1},
            {"passedTests": 2, "failedTests": 1, "skippedTests": 0},
            {"passedTests": 2, "failedTests": 0, "skippedTests": 1},
        ]:
            with self.subTest(summary=summary), self.assertRaises(RuntimeError):
                RUNNER.verify_summary(summary, 2)

    def test_every_fixture_journey_is_required_and_in_the_navigation_scheme(self):
        root = PATH.parents[1]
        found = set()
        classes = set()
        for path in (root / "clients/apple/UITests").glob("TV*NavigationTests.swift"):
            source = path.read_text()
            name = re.search(r"final class (\w+): TVNavigationTestCase", source).group(1)
            classes.add(name)
            found.update(name + "/" + test for test in re.findall(r"func (test\w+)\(", source))
        self.assertEqual(set(RUNNER.REQUIRED), found)
        self.assertEqual(len(RUNNER.REQUIRED), len(found), "No duplicate journey selections")
        project = (root / "clients/apple/project.yml").read_text()
        navigation = project.split("  plurx-tvOS-navigation:")[1].split("  plurx-tvOS-physical:")[0]
        physical = project.split("  plurx-tvOS-physical:")[1].split("  plurx-iOS-library-ui:")[0]
        for name in classes:
            self.assertIn("            - " + name, navigation)
            self.assertIn("            - " + name, physical)


class TVNavigationResumeTests(unittest.TestCase):
    def test_pending_selection_is_exact_complement_of_retained_originals(self):
        required = ['Suite/testOne', 'Suite/testTwo']
        plan = {'current_commit': 'head', 'current_run': 4,
                'tv_cases': {'required_tests': required, 'passes': {'Suite/testOne': {'test': 'Suite/testOne'}},
                             'pending': ['Suite/testTwo']}}
        with tempfile.TemporaryDirectory() as directory, patch.object(RUNNER, 'REQUIRED', required), \
                patch.dict(os.environ, {'GITHUB_RUN_ID': '4'}):
            path = Path(directory) / 'plan.json'
            path.write_text(json.dumps(plan))
            self.assertEqual(RUNNER.resume_selection(path, 'head')[0], ['Suite/testTwo'])
            plan['tv_cases']['pending'] = required
            path.write_text(json.dumps(plan))
            with self.assertRaises(RuntimeError):
                RUNNER.resume_selection(path, 'head')

    def test_failed_execution_exports_own_cases_and_checks_final_source(self):
        selected = ['Suite/testOne', 'Suite/testTwo']
        log = ("Test Case '-[plurx_tvOSUITests.Suite testOne]' started.\n"
               "Test Case '-[plurx_tvOSUITests.Suite testOne]' passed (1.234 seconds).\n"
               "Test Case '-[plurx_tvOSUITests.Suite testTwo]' started.\n"
               "Test Case '-[plurx_tvOSUITests.Suite testTwo]' failed (2.345 seconds).\n")
        summary = {'passedTests': 1, 'failedTests': 1, 'skippedTests': 0}
        snapshot = ('head', False, 'digest')
        for final in (snapshot, ('head', True, 'changed')):
            with self.subTest(final=final), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / 'result'
                def command(*args, **kwargs):
                    if args[:2] == ('xcodebuild', 'test'):
                        kwargs['stdout'].write(log)
                        bundle = output / 'navigation.xcresult'
                        bundle.mkdir()
                        (bundle / 'Info.plist').write_text('placeholder')
                        raise subprocess.CalledProcessError(65, args)
                    return subprocess.CompletedProcess(args, 0, stdout='device' if args[:3] == ('xcrun', 'simctl', 'create') else '')
                with patch.object(RUNNER, 'REQUIRED', selected), \
                        patch.object(RUNNER, 'source_snapshot', side_effect=[snapshot, final]), \
                        patch.object(RUNNER, 'run', side_effect=command), \
                        patch.object(RUNNER.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, stdout=json.dumps(summary))), \
                        patch.dict(os.environ, {}, clear=True), \
                        patch.object(RUNNER.sys, 'argv', ['runner', '--output', str(output)]):
                    self.assertEqual(RUNNER.main(), 1)
                receipt = json.loads((output / 'receipt.json').read_text())
                self.assertEqual(receipt['source_unchanged'], final == snapshot)
                self.assertEqual(receipt['case_results'], {'Suite/testOne': 'passed', 'Suite/testTwo': 'failed'}
                                 if final == snapshot else {})


    def test_complete_retained_union_executes_no_native_case(self):
        required = ['Suite/testOne']
        retained = {'Suite/testOne': {'test': 'Suite/testOne'}}
        plan = {'current_commit': 'head', 'current_run': 4,
                'tv_cases': {'required_tests': required, 'passes': retained, 'pending': []}}
        with tempfile.TemporaryDirectory() as directory:
            path, output = Path(directory) / 'plan.json', Path(directory) / 'result'
            path.write_text(json.dumps(plan))
            with patch.object(RUNNER, 'REQUIRED', required), \
                    patch.object(RUNNER, 'source_snapshot', return_value=('head', False, 'digest')), \
                    patch.object(RUNNER, 'run', return_value=subprocess.CompletedProcess([], 0, stdout='Xcode')) as command, \
                    patch.dict(os.environ, {'GITHUB_RUN_ID': '4', 'PLURX_TV_NAVIGATION_PLAN': str(path)}), \
                    patch.object(RUNNER.sys, 'argv', ['runner', '--output', str(output)]):
                self.assertEqual(RUNNER.main(), 0)
                command.assert_called_once_with('xcodebuild', '-version', capture_output=True)
            receipt = json.loads((output / 'receipt.json').read_text())
            self.assertTrue(receipt['native_suite_complete'])
            self.assertEqual(receipt['suite_passes'], retained)
            self.assertEqual(receipt['case_results'], {})
            self.assertNotIn('command', receipt)
