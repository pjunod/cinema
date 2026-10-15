"""A successful xcodebuild exit alone cannot qualify an empty/skipped UI run."""
import importlib.machinery
import importlib.util
from pathlib import Path
import unittest


PATH = Path(__file__).resolve().parents[2] / "scripts/apple-tv-navigation-test"
SPEC = importlib.util.spec_from_loader("tv_navigation_runner", importlib.machinery.SourceFileLoader(
    "tv_navigation_runner", str(PATH)))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class TVNavigationRunnerTests(unittest.TestCase):
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
