"""A successful xcodebuild exit alone cannot qualify an empty/skipped UI run."""
import importlib.machinery
import importlib.util
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
