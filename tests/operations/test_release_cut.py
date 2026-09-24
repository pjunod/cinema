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
import shutil
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

# The status lines that quote the version and both build counters, in the
# shape the real documents carry them (validation/apple_build.py and
# validation/doc_versions.py pin those shapes on the real tree).
DOCUMENTS = {
    release_cut.APPLE_README: (
        "# plurx for Apple\n\n"
        "> Status: **v0.3.0**, build `180` in [`project.yml`](project.yml) — working\n"
        "> regression suite.\n"
    ),
    release_cut.APPLE_PARITY: (
        "# Parity\n\n> Status (2026-09-15): source is v0.3.0, Apple build 180. Timer-only\n"
    ),
    release_cut.ANDROID_README: (
        "# plurx for Android\n\n> Status: **v0.3.0**, build `120` — native viewer parity\n"
    ),
    release_cut.STATUS_PAGE: (
        "<div>web · Android versionCode 119 source · Apple build 180 source, not yet uploaded</div>\n"
        "<li>upload Apple build 180 (release notes: x) to TestFlight</li>\n"
        "<li>Install Apple build 180 and Android versionCode 119 on the physical devices.</li>\n"
    ),
}


def git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=root, check=True, text=True, stdout=subprocess.PIPE
    ).stdout.strip()


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
        for path, contents in DOCUMENTS.items():
            (root / path).parent.mkdir(parents=True, exist_ok=True)
            (root / path).write_text(contents, encoding="utf-8")
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
        # Every status document's quote of the version and both counters.
        read = lambda path: (root / path).read_text(encoding="utf-8")  # noqa: E731
        self.assertIn("> Status: **v0.3.1**, build `181` in", read(release_cut.APPLE_README))
        self.assertIn("source is v0.3.1, Apple build 181. Timer-only", read(release_cut.APPLE_PARITY))
        self.assertIn("> Status: **v0.3.1**, build `121` — native", read(release_cut.ANDROID_README))
        status = read(release_cut.STATUS_PAGE)
        self.assertIn("Android versionCode 121 source · Apple build 181 source", status)
        self.assertIn("upload Apple build 181 (release notes", status)
        self.assertIn("Install Apple build 181 and Android versionCode 121 on", status)
        self.assertNotRegex(status, r"\b(180|119)\b")
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

    def test_the_cut_real_tree_passes_the_build_claim_sweeps(self):
        """PR #485 review finding 2: the cut left the repository red.

        `make operations-check` pins every status document's build claim to
        the manifests (`test_mobile_build_claims`, `test_apple_build_claims`),
        and a cut that advanced the counters without them failed both. This
        runs the cut on a copy of this repository's own release files and
        then those same sweeps on the result.
        """

        from validation.apple_build import NOTES_DIR, check_repository
        from validation.doc_versions import validate_documented_builds
        from validation.mobile_versions import read_versions, validate_versions

        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for path in (
            "CHANGELOG.md", "Cargo.toml", release_cut.ANDROID, release_cut.APPLE,
            *DOCUMENTS,
        ):
            (root / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / path, root / path)
        shutil.copytree(ROOT / NOTES_DIR, root / NOTES_DIR)
        changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
        if release_cut._unreleased_is_empty(changelog):
            (root / "CHANGELOG.md").write_text(
                changelog.replace("## [Unreleased]\n", "## [Unreleased]\n\n- An entry.\n", 1),
                encoding="utf-8",
            )
        git(root, "init", "-q")
        read = lambda path: (root / path).read_text(encoding="utf-8")  # noqa: E731
        self.assertEqual(validate_documented_builds(read), (), "the copy starts green")
        self.assertEqual(check_repository(root), (), "the copy starts green")
        before = read_versions(read)

        result = self.run_cut(root, "--date", "2026-09-28")
        self.assertEqual(result.returncode, 0, result.stderr)

        after = read_versions(read)
        self.assertEqual(after.apple_build, before.apple_build + 1)
        self.assertEqual(validate_documented_builds(read), ())
        self.assertEqual(check_repository(root), ())
        self.assertEqual(validate_versions(after, baseline=before), ())

    def test_a_reworded_claim_refuses_the_cut_before_anything_is_written(self):
        root = self.tree()
        readme = root / release_cut.ANDROID_README
        readme.write_text(
            DOCUMENTS[release_cut.ANDROID_README].replace("build `120`", "versionCode 120"),
            encoding="utf-8",
        )
        result = self.run_cut(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{release_cut.ANDROID_README}: expected exactly one", result.stderr)
        self.assertEqual((root / "Cargo.toml").read_text(encoding="utf-8"), CARGO)
        self.assertEqual((root / "CHANGELOG.md").read_text(encoding="utf-8"), CHANGELOG)

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

        # v0.3.0 is dated and untagged in this fixture, but the one commit
        # that dated it also carries `[Unreleased]` entries, so it is not a
        # release commit and nothing is named for tagging.
        refused = self.run_cut(root, "--pending")
        self.assertEqual(refused.returncode, 1)
        self.assertIn("[Unreleased] section is not empty", refused.stderr)
        subprocess.run(["git", "tag", "-a", "v0.3.0", "-m", "v0.3.0"], cwd=root, check=True)
        self.assertEqual(pending()["pending"], "false")
        self.assertEqual(pending()["tagged"], "true")

        self.assertEqual(self.run_cut(root, "--date", "2026-09-28").returncode, 0)
        subprocess.run(["git", "commit", "-qam", "release: v0.3.1"], cwd=root, check=True)
        state = pending()
        self.assertEqual(state["pending"], "true")
        self.assertEqual(state["tag"], "v0.3.1")
        self.assertEqual(state["dated"], "true")
        self.assertEqual(state["sha"], git(root, "rev-parse", "HEAD"))

    def pending_at(self, root: Path) -> tuple[subprocess.CompletedProcess[str], dict[str, str]]:
        result = self.run_cut(root, "--pending")
        return result, dict(line.split("=", 1) for line in result.stdout.splitlines())

    def released(self) -> tuple[Path, str, str]:
        """v0.3.0 tagged, then a release pull request for v0.3.1 landed on main."""

        root = self.tree()
        git(root, "tag", "-a", "v0.3.0", "-m", "v0.3.0")
        main = git(root, "rev-parse", "--abbrev-ref", "HEAD")
        git(root, "checkout", "-q", "-b", "release/v0.3.1")
        self.assertEqual(self.run_cut(root, "--date", "2026-09-28").returncode, 0)
        cut = self.commit(root, "release: v0.3.1")
        git(root, "checkout", "-q", main)
        git(root, "merge", "-q", "--no-ff", "-m", "Merge pull request 'release: v0.3.1'", cut)
        return root, cut, git(root, "rev-parse", "HEAD")

    def commit(self, root: Path, subject: str) -> str:
        git(root, "add", "-A")
        git(root, "commit", "-qm", subject)
        return git(root, "rev-parse", "HEAD")

    def test_pending_names_the_release_commit_never_main_s_tip(self):
        """PR #485 review finding 1, the reviewer's own reproduction.

        After the release lands, `main` keeps the version and the dated
        section, so the Monday run used to tag whatever tip it found --
        including a fix the changelog files under `[Unreleased]`. The tag has
        to go on the release's landing commit, whatever is on top of it.
        """

        root, cut, landing = self.released()
        self.assertEqual(self.pending_at(root)[1]["sha"], landing)

        changelog = root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                "## [Unreleased]\n", "## [Unreleased]\n\n### Fixed\n\n- A later fix.\n", 1
            ),
            encoding="utf-8",
        )
        later_fix = self.commit(root, "fix(app): a later fix")
        (root / "notes.txt").write_text("undocumented\n", encoding="utf-8")
        tip = self.commit(root, "chore: a change with no changelog entry")

        result, state = self.pending_at(root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(state["pending"], "true")
        self.assertEqual(state["head"], tip)
        self.assertEqual(state["sha"], landing, "the tag target is not the release commit")
        self.assertNotIn(state["sha"], (tip, later_fix))

        # The tag job re-asks from the release commit it checked out, and must
        # get that same commit back.
        git(root, "checkout", "-q", "--detach", landing)
        self.assertEqual(self.pending_at(root)[1]["sha"], landing)

        # Before it lands, on the release branch, the release is the cut itself.
        git(root, "checkout", "-q", cut)
        self.assertEqual(self.pending_at(root)[1]["sha"], cut)

    def test_a_dated_section_no_release_commit_explains_is_refused(self):
        """A heading that arrived without its release is not tagged anywhere."""

        root = self.tree()
        git(root, "tag", "-a", "v0.3.0", "-m", "v0.3.0")
        rolled = release_cut.roll_changelog(CHANGELOG, "0.3.1", "2026-09-28")
        (root / "CHANGELOG.md").write_text(rolled, encoding="utf-8")
        self.commit(root, "docs: date the changelog by hand")
        (root / "Cargo.toml").write_text(
            release_cut.set_workspace_version(CARGO, "0.3.1"), encoding="utf-8"
        )
        self.commit(root, "chore: move the version later")

        result, state = self.pending_at(root)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(state, {})
        self.assertIn("does not declare 0.3.1", result.stderr)

    def test_a_section_edited_after_the_release_commit_is_refused(self):
        root, _, landing = self.released()
        changelog = root / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                "Something a server operator will notice.", "Something else entirely."
            ),
            encoding="utf-8",
        )
        self.commit(root, "docs: reword the release")
        result, _ = self.pending_at(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(landing[:12], result.stderr)
        self.assertIn("section differs", result.stderr)


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

    def test_the_gate_runs_on_the_release_commit_not_the_tip(self):
        """The commit gated is the commit tagged: the release commit.

        `--pending`'s `sha` is the release commit (see
        `test_pending_names_the_release_commit_never_main_s_tip`); the gate has
        to check it out before `make release-check`, or it tests the tip and
        tags a commit it never ran.
        """

        checkout = self.check.index(
            "      - name: Check out the release commit\n"
            "        if: steps.release.outputs.pending == 'true'\n"
            "        env:\n"
            "          RELEASE_SHA: ${{ steps.release.outputs.sha }}\n"
            '        run: git checkout -q --detach "$RELEASE_SHA"\n'
        )
        self.assertLess(checkout, self.check.index("run: make release-check"))
        self.assertIn("sha: ${{ steps.release.outputs.sha }}", self.check)
        # A refusal from `--pending` fails the step instead of vanishing into a pipe.
        find = self.check.split("id: release\n", 1)[1].split("      - name:", 1)[0]
        self.assertNotIn("| tee", find)
        self.assertIn('scripts/release-cut --pending > "$found"', find)
        self.assertIn('grep -qx "sha=$TESTED_SHA"', self.tag)

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
