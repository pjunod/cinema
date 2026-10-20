"""A TV focus regression must fail promotion and preserve its native evidence."""
from pathlib import Path
import re
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]


def job(path, name):
    text = (ROOT / path).read_text()
    return re.split(r"\n  [a-z_]+:\n", text.split(f"\n  {name}:\n", 1)[1])[0]


class AppleTVNavigationGateTests(unittest.TestCase):
    def test_native_navigation_failure_blocks_both_promotion_paths(self):
        for path, apple, gate in (
            (".github/workflows/main-fast-lane.yml", "apple_compile", "promotion_gate"),
            (".github/workflows/ci.yml", "apple", "pr_gate"),
        ):
            with self.subTest(path=path):
                workflow = (ROOT / path).read_text()
                apple_job = job(path, apple)
                step = apple_job.split("- name: Require native TV navigation journeys", 1)[1].split("- name:", 1)[0]
                self.assertIn("run: make apple-tv-navigation-test", step)
                allowed = "continue-on-error: ${{ inputs.diagnostic_baseline == 'true' }}"
                if path == ".github/workflows/ci.yml":
                    self.assertNotIn("continue-on-error", apple_job.replace(allowed, ""))
                    self.assertIn("Require original diagnostic Apple outcomes", apple_job)
                    self.assertIn('test "$result" = success', apple_job)
                else:
                    self.assertNotIn("continue-on-error", apple_job)
                # This job also serves the native macOS tray. Mobile TV
                # journeys remain mandatory whenever the mobile Apple lane
                # executes; retained mobile evidence is handled separately.
                expected_guard = "needs.scope.outputs.apple == 'true'"
                if path == ".github/workflows/ci.yml":
                    expected_guard += " && needs.scope.outputs.retain_apple != 'true'"
                self.assertEqual(
                    re.findall(r"^        if: (.+)$", step, re.MULTILINE),
                    [expected_guard],
                )
                self.assertIn("PLURX_TV_NAVIGATION_OUTPUT:", step)
                self.assertIn("- " + apple, job(path, gate))
                self.assertIn("needs." + apple + ".result", workflow)

    def test_failed_navigation_retains_results_and_screenshots(self):
        for path, apple in (
            (".github/workflows/main-fast-lane.yml", "apple_compile"),
            (".github/workflows/ci.yml", "apple"),
        ):
            with self.subTest(path=path):
                step = job(path, apple).split("- name: Preserve TV navigation evidence including failures", 1)[1].split("- name:", 1)[0]
                self.assertIn("if: always()", step)
                self.assertIn("if-no-files-found: error", step)
                self.assertIn("path: ${{ runner.temp }}/tv-navigation-${{ github.run_id }}-${{ github.run_attempt }}", step)

    def test_affected_apple_catalog_requires_native_journeys(self):
        catalog = tomllib.loads((ROOT / "validation/points.toml").read_text())
        apple = next(point for point in catalog["points"] if point["id"] == "apple.client")
        self.assertIn("apple-tv-navigation", apple["checks"])
        check = next(check for check in catalog["checks"] if check["id"] == "apple-tv-navigation")
        self.assertEqual(check["command"], "make apple-tv-navigation-test")
        self.assertEqual(check["missing"], "fail")
        self.assertTrue({"ci", "full"}.issubset(check["profiles"]))
        makefile = (ROOT / "Makefile").read_text()
        target = makefile.split("apple-tv-navigation-test:", 1)[1].split(".PHONY:", 1)[0]
        self.assertIn("python3 scripts/apple-tv-navigation-test", target)
        self.assertNotIn("--only-testing", target)


if __name__ == "__main__":
    unittest.main()
