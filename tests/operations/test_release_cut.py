"""`scripts/release-cut` and the weekly release tag it feeds.

docs/ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md §3.5 and M7: the script
does the mechanical half of a release and refuses the unsafe half, and the
scheduled release-readiness run tags only a green, untagged, dated release.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "release-cut"


def load_release_cut():
    loader = importlib.machinery.SourceFileLoader("release_cut", str(SCRIPT))
    spec = importlib.util.spec_from_loader("release_cut", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


release_cut = load_release_cut()

CHANGELOG = textwrap.dedent(
    """\
    # Changelog

    ## [Unreleased]

    ### Fixed

    - **Something a server operator will notice.**

    ## [0.3.0] — 2026-08-31

    - The previous release.

    [Unreleased]: https://github.com/pjunod/plurx/compare/v0.3.0...HEAD
    [0.3.0]: https://github.com/pjunod/plurx/compare/v0.2.8...v0.3.0
    """
)
CARGO = textwrap.dedent(
    """\
    [workspace]
    members = ["crates/a"]

    [workspace.package]
    version = "0.3.0"
    edition = "2024"

    [workspace.dependencies]
    serde = { version = "1.0.0" }
    """
)
ANDROID = '    defaultConfig {\n        versionCode = 120\n        versionName = "0.3.0"\n    }\n'
APPLE = textwrap.dedent(
    """\
    settings:
      base:
        MARKETING_VERSION: "0.3.0"
        CURRENT_PROJECT_VERSION: "180"
    targets:
      iOS:
        info:
          properties:
            CFBundleShortVersionString: "$(MARKETING_VERSION)"
            CFBundleVersion: "$(CURRENT_PROJECT_VERSION)"
      tvOS:
        info:
          properties:
            CFBundleShortVersionString: "$(MARKETING_VERSION)"
            CFBundleVersion: "$(CURRENT_PROJECT_VERSION)"
    """
)


class ReleaseCutCase(unittest.TestCase):
    def tree(self, changelog: str = CHANGELOG) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "clients/android/app").mkdir(parents=True)
        (root / "clients/apple").mkdir(parents=True)
        (root / "CHANGELOG.md").write_text(changelog, encoding="utf-8")
        (root / "Cargo.toml").write_text(CARGO, encoding="utf-8")
        (root / release_cut.ANDROID).write_text(ANDROID, encoding="utf-8")
        (root / release_cut.APPLE).write_text(APPLE, encoding="utf-8")
        for args in (
            ["init", "-q"],
            ["config", "user.name", "Release Test"],
            ["config", "user.email", "release@example.invalid"],
            ["add", "-A"],
            ["commit", "-qm", "seed"],
        ):
            subprocess.run(["git", *args], cwd=root, check=True)
        return root

    def run_cut(self, root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(root), "--no-lockfiles", *args],
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

    def test_a_patch_cut_rolls_the_changelog_and_every_version(self):
        root = self.tree()
        result = self.run_cut(root, "--date", "2026-09-28")
        self.assertEqual(result.returncode, 0, result.stderr)
        changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
        self.assertIn(
            "## [Unreleased]\n\n## [0.3.1] — 2026-09-28\n\n### Fixed\n", changelog
        )
        self.assertIn(
            "[Unreleased]: https://github.com/pjunod/plurx/compare/v0.3.1...HEAD\n"
            "[0.3.1]: https://github.com/pjunod/plurx/compare/v0.3.0...v0.3.1\n"
            "[0.3.0]: ",
            changelog,
        )
        cargo = (root / "Cargo.toml").read_text(encoding="utf-8")
        self.assertIn('[workspace.package]\nversion = "0.3.1"\n', cargo)
        # Only the workspace version moves, never a dependency's.
        self.assertIn('serde = { version = "1.0.0" }', cargo)
        android = (root / release_cut.ANDROID).read_text(encoding="utf-8")
        self.assertIn('versionName = "0.3.1"', android)
        self.assertIn("versionCode = 121", android)
        apple = (root / release_cut.APPLE).read_text(encoding="utf-8")
        self.assertIn('MARKETING_VERSION: "0.3.1"', apple)
        self.assertIn('CURRENT_PROJECT_VERSION: "181"', apple)
        # It never tags; tagging is the scheduled run's, from a green gate.
        tags = subprocess.run(
            ["git", "tag", "-l"], cwd=root, text=True, stdout=subprocess.PIPE
        ).stdout
        self.assertEqual(tags, "")
        self.assertIn("scheduled release-readiness run", result.stdout)

    def test_the_cut_satisfies_the_mobile_version_contract(self):
        from validation.mobile_versions import read_versions, validate_versions

        root = self.tree()
        before = read_versions(lambda path: (root / path).read_text(encoding="utf-8"))
        self.assertEqual(self.run_cut(root, "--part", "minor").returncode, 0)
        after = read_versions(lambda path: (root / path).read_text(encoding="utf-8"))
        self.assertEqual(after.workspace, "0.4.0")
        self.assertEqual(validate_versions(after, baseline=before), ())

    def test_an_empty_unreleased_section_is_refused_and_nothing_is_written(self):
        empty = CHANGELOG.replace(
            "### Fixed\n\n- **Something a server operator will notice.**\n\n", ""
        )
        root = self.tree(empty)
        result = self.run_cut(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("empty", result.stderr)
        self.assertEqual((root / "CHANGELOG.md").read_text(encoding="utf-8"), empty)
        self.assertEqual((root / "Cargo.toml").read_text(encoding="utf-8"), CARGO)

    def test_an_existing_tag_or_section_is_refused(self):
        root = self.tree()
        subprocess.run(["git", "tag", "v0.3.1"], cwd=root, check=True)
        result = self.run_cut(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("v0.3.1 already exists", result.stderr)
        result = self.run_cut(root, "--version", "0.3.0")
        self.assertEqual(result.returncode, 1)
        self.assertIn("not newer", result.stderr)

    def test_pending_names_an_untagged_dated_release_only(self):
        root = self.tree()

        def pending() -> dict[str, str]:
            out = self.run_cut(root, "--pending").stdout
            return dict(line.split("=", 1) for line in out.splitlines())

        # v0.3.0 has a dated section and no tag in this fixture.
        self.assertEqual(pending()["pending"], "true")
        subprocess.run(["git", "tag", "-a", "v0.3.0", "-m", "v0.3.0"], cwd=root, check=True)
        self.assertEqual(pending()["pending"], "false")
        self.assertEqual(pending()["tagged"], "true")

        self.assertEqual(self.run_cut(root, "--date", "2026-09-28").returncode, 0)
        subprocess.run(["git", "commit", "-qam", "release: v0.3.1"], cwd=root, check=True)
        state = pending()
        self.assertEqual(state["pending"], "true")
        self.assertEqual(state["tag"], "v0.3.1")
        self.assertEqual(state["dated"], "true")


class WeeklyTagWorkflowCase(unittest.TestCase):
    """The cadence Paul chose on 2026-09-23: weekly, from a green scheduled run."""

    def setUp(self):
        self.workflow = (ROOT / ".github/workflows/release-readiness.yml").read_text(
            encoding="utf-8"
        )
        self.tag = self.workflow.split("\n  tag:\n", 1)[1]
        self.check = self.workflow.split("\n  release-check:\n", 1)[1].split("\n  tag:\n", 1)[0]

    def test_it_runs_every_monday_and_on_demand(self):
        self.assertIn('  schedule:\n    - cron: "0 6 * * 1"\n', self.workflow)
        self.assertIn("  workflow_dispatch:\n", self.workflow)

    def test_the_tag_needs_a_green_scheduled_gate_on_a_pending_release(self):
        self.assertIn("    needs: release-check\n", self.tag)
        condition = re.search(r"(?m)^    if: (.+)$", self.tag).group(1)
        self.assertIn("github.event_name == 'schedule'", condition)
        self.assertIn("needs.release-check.result == 'success'", condition)
        self.assertIn("needs.release-check.outputs.pending == 'true'", condition)
        # The gate for a pending release is the whole of `make release-check`.
        self.assertIn(
            "        if: steps.release.outputs.pending == 'true'\n        run: make release-check",
            self.check,
        )

    def test_the_tag_goes_on_the_commit_the_gate_passed_and_is_annotated(self):
        self.assertIn("ref: ${{ needs.release-check.outputs.sha }}", self.tag)
        self.assertIn('test "$(git rev-parse HEAD)" = "$TESTED_SHA"', self.tag)
        self.assertIn('tag -a "$TAG" -m "$TAG" "$TESTED_SHA"', self.tag)
        self.assertNotIn("--force", self.tag)

    def test_the_push_credential_is_scoped_to_its_one_step(self):
        self.assertEqual(self.workflow.count("secrets.RELEASE_TAG_TOKEN"), 1)
        self.assertNotIn("git config", self.tag)
        self.assertIn('http.extraheader="AUTHORIZATION: basic $auth"', self.tag)


if __name__ == "__main__":
    unittest.main()
