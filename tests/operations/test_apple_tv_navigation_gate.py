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
                self.assertNotIn("continue-on-error", apple_job)
                # This job also serves the native macOS tray. Mobile TV
                # journeys execute unless verified compatible native evidence
                # is retained. Builds share that proof; promotion still depends
                # on this current job and its pinned toolchain checks.
                expected_guard = "needs.scope.outputs.apple == 'true'"
                if path == ".github/workflows/ci.yml":
                    expected_guard += " && needs.scope.outputs.retain_apple != 'true'"
                else:
                    expected_guard += " && steps.native_receipts.outputs.tv_navigation != 'true'"
                    self.assertIn("run: python3 -m validation.native_receipts", apple_job)
                    self.assertIn("run: python3 -m validation.native_receipts --verify-tv-plan", apple_job)
                    self.assertIn("PLURX_TV_NAVIGATION_PLAN: .main-preflight-receipts/native/receipt.json", step)
                    build = apple_job.split("- name: Compile iOS and tvOS without running simulator tests", 1)[1].split("- name:", 1)[0]
                    self.assertEqual(re.findall(r"^        if: (.+)$", build, re.MULTILINE), [expected_guard])
                    self.assertIn("run: make apple-build", build)
                    tools = apple_job.split("- name: Verify the pinned Apple toolchain", 1)[1].split("- name:", 1)[0]
                    self.assertNotIn("if:", tools)
                    mobile_tools = apple_job.split("- name: Verify the pinned mobile Apple toolchain", 1)[1].split("- name:", 1)[0]
                    self.assertEqual(re.findall(r"^        if: (.+)$", mobile_tools, re.MULTILINE), ["needs.scope.outputs.apple == 'true'"])
                    self.assertIn('test "$(xcodegen --version)" = "Version: 2.46.0"', mobile_tools)
                    self.assertIn("path: .main-preflight-receipts/native/receipt.json", apple_job)
                    self.assertIn("name: native-adoption-${{ github.run_id }}-${{ github.run_attempt }}", apple_job)
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

    def test_apple_jobs_keep_derived_data_local_and_preserve_build_contracts(self):
        for path, apple, command, budget, serialized in (
            (".github/workflows/main-fast-lane.yml", "apple_compile", "apple-build", 120, True),
            (".github/workflows/effort-ci.yml", "apple_compile", "apple-build", 45, False),
            (".github/workflows/ci.yml", "apple", "apple-test", 120, True),
        ):
            with self.subTest(path=path):
                apple_job = job(path, apple)
                for step in re.split(r"\n      - ", apple_job):
                    if re.search(r"uses:.*?/cache(?:/(?:restore|save))?@", step):
                        self.assertNotIn("DerivedData", step, "whole build state must stay in its job root")
                self.assertNotIn("path: clients/apple/build/DerivedData", apple_job)
                self.assertIn("run: make " + command, apple_job)
                self.assertIn('grep -Fxq "Xcode 27.1"', apple_job)
                self.assertIn('grep -Fxq "Build version 27A9275"', apple_job)
                self.assertRegex(apple_job, rf"(?m)^    timeout-minutes: {budget}$")
                self.assertEqual("concurrency:" in apple_job, serialized)
                if serialized:
                    self.assertIn("group: plurx-apple-silicon-heavy", apple_job)
                    self.assertIn("cancel-in-progress: false", apple_job)
        build = (ROOT / "Makefile").read_text().split("apple-build:", 1)[1].split(".PHONY:", 1)[0]
        for platform in ("iOS", "tvOS"):
            self.assertIn('-destination "generic/platform=' + platform + ' Simulator"', build)
        for override in ("ARCHS=", "EXCLUDED_ARCHS", "ONLY_ACTIVE_ARCH"):
            self.assertNotIn(override, build)

    def test_full_apple_budget_covers_units_and_required_native_journeys(self):
        full = job(".github/workflows/ci.yml", "apple")
        fast = job(".github/workflows/main-fast-lane.yml", "apple_compile")
        def budget(section):
            return int(re.search(r"(?m)^    timeout-minutes: ([0-9]+)$", section).group(1))
        self.assertGreaterEqual(budget(full), budget(fast))
        self.assertIn("run: make apple-test", full)
        for section in (full, fast):
            self.assertIn("run: make apple-tv-navigation-test", section)
        runner = (ROOT / "scripts/apple-tv-navigation-test").read_text()
        for flag, seconds in (("-default-test-execution-time-allowance", 180),
                              ("-maximum-test-execution-time-allowance", 240)):
            self.assertRegex(runner, re.escape('"' + flag + '"') + r'\s*,\s*"' + str(seconds) + '"')

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
