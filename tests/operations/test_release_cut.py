"""`scripts/release-cut` and the weekly release tag it feeds.

docs/ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md §3.5 and M7: the script
does the mechanical half of a release and refuses the unsafe half, and the
scheduled release-readiness run tags only a green, untagged, dated release.
"""

from __future__ import annotations

import importlib.machinery
import hashlib
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
ANDROID = (
    '    // runs scripts/build-number\n'
    '    defaultConfig {\n        versionCode = plurxVersionCode\n        versionName = "0.3.0"\n    }\n'
)
STAMP = (
    '        script: |\n'
    '          number="$("${SRCROOT}/../../scripts/build-number" ${mode})"\n'
    '          /usr/libexec/PlistBuddy -c "Set :CFBundleVersion ${number}" "${TARGET_BUILD_DIR}/${INFOPLIST_PATH}"\n'
)
APPLE = textwrap.dedent(
    """\
    settings:
      base:
        MARKETING_VERSION: "0.3.0"
        CURRENT_PROJECT_VERSION: "1"
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
) + STAMP + STAMP

# The status lines that quote the version, in the shape the real documents
# carry them (validation/doc_versions.py pins those shapes on the real tree).
# None quotes a build number: those are derived by scripts/build-number.
DERIVED = "the build number is derived at build time by\n> [`scripts/build-number`](../../scripts/build-number)"
DOCUMENTS = {
    release_cut.APPLE_README: (
        "# plurx for Apple\n\n"
        f"> Status: **v0.3.0**; {DERIVED} — working\n"
        "> regression suite.\n"
    ),
    release_cut.APPLE_PARITY: (
        "# Parity\n\n> Status (2026-09-15): source is v0.3.0; the Apple build number is derived at\n"
        "> build time by [`scripts/build-number`](../../scripts/build-number). Timer-only\n"
    ),
    release_cut.ANDROID_README: (
        f"# plurx for Android\n\n> Status: **v0.3.0**; {DERIVED}. Native viewer parity\n"
    ),
}
STATUS_PAGE = "docs/STATUS.html"
RECEIPTS = "validation/native_receipts.py"
# The shape of validation/native_receipts.py's reviewed Apple project pins.
NATIVE_RECEIPTS = (
    "BUILD_SPECS = {{\n"
    "    # release: v0.3.0 changes only MARKETING_VERSION \"0.2.9\" -> \"0.3.0\".\n"
    "    'clients/apple/project.yml': '{pin}',\n"
    "}}\n"
    'APPLE_PROJECT_SHAPE = "{shape}"\n'
    "APPLE_BUILD_COUNTER = re.compile(rb'(?m)^(    CURRENT_PROJECT_VERSION: \")[1-9][0-9]*(\")$')\n"
)


def load_native_receipts(path: Path):
    """A copy of validation/native_receipts.py, loaded from where the cut rewrote it."""

    loader = importlib.machinery.SourceFileLoader("native_receipts_copy", str(path))
    spec = importlib.util.spec_from_loader("native_receipts_copy", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


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

    def land(self, root: Path, number: int, message: str, path: str | None = None) -> str:
        """A pull request landing the way main receives one: a merge commit carrying its description."""

        main = git(root, "rev-parse", "--abbrev-ref", "HEAD")
        git(root, "checkout", "-q", "-b", f"pr-{number}")
        target = root / (path or f"change-{number}.txt")
        target.write_text(f"{number}\n", encoding="utf-8")
        topic = self.commit(root, f"change {number}")
        git(root, "checkout", "-q", main)
        subject = f"Merge pull request 'change {number}' (#{number}) from pr-{number} into main"
        git(root, "merge", "-q", "--no-ff", "-m", subject, "-m", message, topic)
        return git(root, "rev-parse", "HEAD")

    def test_a_cut_collects_each_landing_entry_exactly_once(self):
        """Entries come from landing commits; the marker keeps the next cut from repeating them."""

        root = self.tree()
        # The field exists from here on, so a landing without one is named.
        (root / "validation").mkdir()
        (root / "validation/changelog_field.py").write_text("# the field\n", encoding="utf-8")
        self.commit(root, "add the field")
        self.land(root, 7, "Changelog: Fixed — **Landed fix.** It works now,\n  across two lines.")
        self.land(root, 9, "No field at all.")
        result = self.run_cut(root, "--date", "2026-09-28")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("#9", result.stderr)
        changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
        first = release_cut._section(changelog, "0.3.1")
        self.assertIn("- **Something a server operator will notice.**", first)
        self.assertIn("- **Landed fix.** It works now,\n  across two lines.", first)
        self.assertEqual(first.count("### Fixed"), 1)

        git(root, "checkout", "-q", "-b", "release/v0.3.1")
        cut = self.commit(root, "release: v0.3.1")
        git(root, "checkout", "-q", "-")
        git(root, "merge", "-q", "--no-ff", "-m",
            "Merge pull request 'release: v0.3.1' (#8) from release/v0.3.1 into main",
            "-m", "Changelog: none — the release itself", cut)
        self.land(root, 10, "Changelog: Added — A later feature.")
        result = self.run_cut(root, "--date", "2026-10-05")
        self.assertEqual(result.returncode, 0, result.stderr)
        changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
        second = release_cut._section(changelog, "0.3.2")
        self.assertIn("### Added\n- A later feature.", second)
        self.assertNotIn("Landed fix", second)
        self.assertEqual(changelog.count("Landed fix"), 1)

    def test_a_cut_with_nothing_to_release_is_refused(self):
        root = self.tree(CHANGELOG.replace(
            "### Fixed\n\n- **Something a server operator will notice.**\n", ""))
        self.land(root, 7, "Changelog: none")
        result = self.run_cut(root, "--date", "2026-09-28")
        self.assertEqual(result.returncode, 1)
        self.assertIn("nothing to release", result.stderr)

    def test_a_patch_cut_rolls_the_changelog_and_every_version(self):
        root = self.tree()
        result = self.run_cut(root, "--date", "2026-09-28")
        self.assertEqual(result.returncode, 0, result.stderr)
        changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
        unreleased, _, rest = changelog.partition("## [0.3.1] — 2026-09-28\n")
        self.assertTrue(rest, changelog)
        # [Unreleased] keeps only the note saying where entries come from.
        self.assertFalse(release_cut._has_content(unreleased.partition("## [Unreleased]\n")[2]))
        self.assertRegex(rest, r"^<!-- changelog-through: [0-9a-f]{40} -->\n\n### Fixed\n")
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
        apple = (root / release_cut.APPLE).read_text(encoding="utf-8")
        self.assertIn('MARKETING_VERSION: "0.3.1"', apple)
        # Build numbers are derived at build time, so the cut stores none.
        self.assertEqual(android, ANDROID.replace('"0.3.0"', '"0.3.1"'))
        self.assertEqual(apple, APPLE.replace('"0.3.0"', '"0.3.1"'))
        # Every status document's quote of the version.
        read = lambda path: (root / path).read_text(encoding="utf-8")  # noqa: E731
        self.assertIn("> Status: **v0.3.1**; the build number", read(release_cut.APPLE_README))
        self.assertIn("source is v0.3.1; the Apple build number", read(release_cut.APPLE_PARITY))
        self.assertIn("> Status: **v0.3.1**; the build number", read(release_cut.ANDROID_README))
        self.assertNotIn(STATUS_PAGE, result.stdout)
        # It never tags; tagging is the scheduled run's, from a green gate.
        tags = subprocess.run(
            ["git", "tag", "-l"], cwd=root, text=True, stdout=subprocess.PIPE
        ).stdout
        self.assertEqual(tags, "")
        self.assertIn("scheduled release-readiness run", result.stdout)

    def test_the_cut_satisfies_the_mobile_version_contract(self):
        from validation.mobile_versions import read_versions, validate_versions

        root = self.tree()
        self.assertEqual(self.run_cut(root, "--part", "minor").returncode, 0)
        after = read_versions(lambda path: (root / path).read_text(encoding="utf-8"))
        self.assertEqual(after.workspace, "0.4.0")
        self.assertEqual(validate_versions(after), ())

    def test_the_cut_real_tree_passes_the_build_claim_sweeps(self):
        """PR #485 review finding 2: the cut left the repository red.

        `make operations-check` pins every status document's version and
        build-number wording (`test_mobile_build_claims`,
        `test_apple_build_claims`). This runs the cut on a copy of this
        repository's own release files and then those same sweeps on the
        result.
        """

        from validation.apple_build import NOTES_DIR, check_repository
        from validation.doc_versions import validate_documented_builds
        from validation.mobile_versions import read_versions, validate_versions

        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for path in (
            "CHANGELOG.md", "Cargo.toml", release_cut.ANDROID, release_cut.APPLE,
            *DOCUMENTS, STATUS_PAGE, RECEIPTS,
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
        self.assertEqual(after.apple_build, before.apple_build, "the placeholder never moves")
        self.assertEqual(after.android_version_code, before.android_version_code)
        self.assertEqual(validate_documented_builds(read), ())
        self.assertEqual(check_repository(root), ())
        self.assertEqual(validate_versions(after), ())
        # v0.9.1 and v0.9.2 each needed a second push: the cut left the
        # reviewed project.yml fingerprints in validation/native_receipts.py
        # describing the old MARKETING_VERSION, failing the fast lane.
        receipts = load_native_receipts(root / RECEIPTS)
        project = (root / release_cut.APPLE).read_bytes()
        self.assertEqual(
            receipts.BUILD_SPECS[release_cut.APPLE], hashlib.sha256(project).hexdigest()
        )
        normalized = receipts.APPLE_BUILD_COUNTER.sub(rb"\g<1>0\g<2>", project)
        self.assertEqual(receipts.APPLE_PROJECT_SHAPE, hashlib.sha256(normalized).hexdigest())
        self.assertIn(
            f'# release: v{after.workspace} changes only MARKETING_VERSION '
            f'"{before.workspace}" -> "{after.workspace}".',
            read(RECEIPTS),
        )

    def receipts_tree(self) -> tuple[Path, str]:
        """A fixture whose native receipts pin its project.yml the way the real file does."""

        root = self.tree()
        digest = lambda data: hashlib.sha256(data.encode("utf-8")).hexdigest()  # noqa: E731
        receipts = NATIVE_RECEIPTS.format(
            pin=digest(APPLE), shape=digest(APPLE.replace('VERSION: "1"', 'VERSION: "0"'))
        )
        (root / RECEIPTS).parent.mkdir(exist_ok=True)
        (root / RECEIPTS).write_text(receipts, encoding="utf-8")
        self.commit(root, "pin the reviewed boundary")
        return root, receipts

    def test_the_cut_moves_the_reviewed_apple_boundary_with_the_marketing_version(self):
        root, receipts = self.receipts_tree()
        result = self.run_cut(root, "--date", "2026-09-28")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"changed {RECEIPTS}", result.stdout)
        bumped = APPLE.replace('"0.3.0"', '"0.3.1"')
        digest = lambda data: hashlib.sha256(data.encode("utf-8")).hexdigest()  # noqa: E731
        self.assertEqual(
            (root / RECEIPTS).read_text(encoding="utf-8"),
            NATIVE_RECEIPTS.format(
                pin=digest(bumped), shape=digest(bumped.replace('VERSION: "1"', 'VERSION: "0"'))
            ).replace('v0.3.0 changes only MARKETING_VERSION "0.2.9" -> "0.3.0"',
                      'v0.3.1 changes only MARKETING_VERSION "0.3.0" -> "0.3.1"'),
        )

    def test_an_unreviewed_project_change_refuses_the_cut_before_anything_is_written(self):
        """The cut never widens the reviewed boundary: only its own version bump re-pins."""

        root, receipts = self.receipts_tree()
        edited = APPLE + "    preBuildScripts: [{script: cat ../../extra/file}]\n"
        (root / release_cut.APPLE).write_text(edited, encoding="utf-8")
        self.commit(root, "an unreviewed project change")
        result = self.run_cut(root, "--date", "2026-09-28")
        self.assertEqual(result.returncode, 1)
        self.assertIn("no longer matches its reviewed fingerprint", result.stderr)
        self.assertEqual((root / release_cut.APPLE).read_text(encoding="utf-8"), edited)
        self.assertEqual((root / RECEIPTS).read_text(encoding="utf-8"), receipts)
        self.assertEqual((root / "Cargo.toml").read_text(encoding="utf-8"), CARGO)

    def test_a_cut_that_moves_more_than_the_marketing_version_is_refused(self):
        module = load_release_cut()
        before = APPLE
        after = APPLE.replace('"0.3.0"', '"0.3.1"').replace("tvOS:", "watchOS:")
        digest = lambda data: hashlib.sha256(data.encode("utf-8")).hexdigest()  # noqa: E731
        receipts = NATIVE_RECEIPTS.format(
            pin=digest(before), shape=digest(before.replace('VERSION: "1"', 'VERSION: "0"'))
        )
        with self.assertRaisesRegex(module.CutError, "only its MARKETING_VERSION line"):
            module.repin_native_receipts(receipts, before, after, "0.3.0", "0.3.1")

    def test_the_cut_needs_both_the_exact_pin_and_the_shape(self):
        """A counter-only variant, or a stale shape, is not the reviewed file."""

        module = load_release_cut()
        digest = lambda data: hashlib.sha256(data.encode("utf-8")).hexdigest()  # noqa: E731
        shape = digest(APPLE.replace('VERSION: "1"', 'VERSION: "0"'))
        bump = lambda text: text.replace('"0.3.0"', '"0.3.1"')  # noqa: E731
        counter = APPLE.replace('VERSION: "1"', 'VERSION: "2"')
        for label, receipts, before in (
            ("counter-only variant", NATIVE_RECEIPTS.format(pin=digest(APPLE), shape=shape), counter),
            ("stale shape", NATIVE_RECEIPTS.format(pin=digest(APPLE), shape="0" * 64), APPLE),
        ):
            with self.subTest(label), self.assertRaisesRegex(
                module.CutError, "no longer matches its reviewed fingerprint"
            ):
                module.repin_native_receipts(receipts, before, bump(before), "0.3.0", "0.3.1")

    def test_a_reviewed_note_above_the_pin_gets_the_release_line_added(self):
        module = load_release_cut()
        digest = lambda data: hashlib.sha256(data.encode("utf-8")).hexdigest()  # noqa: E731
        normalize = lambda text: text.replace('VERSION: "1"', 'VERSION: "0"')  # noqa: E731
        reviewed = NATIVE_RECEIPTS.format(pin=digest(APPLE), shape=digest(normalize(APPLE))).replace(
            '# release: v0.3.0 changes only MARKETING_VERSION "0.2.9" -> "0.3.0".',
            "# PR #2000: a reviewed project change.",
        )
        after = APPLE.replace('"0.3.0"', '"0.3.1"')
        self.assertEqual(
            module.repin_native_receipts(reviewed, APPLE, after, "0.3.0", "0.3.1"),
            NATIVE_RECEIPTS.format(pin=digest(after), shape=digest(normalize(after))).replace(
                '    # release: v0.3.0 changes only MARKETING_VERSION "0.2.9" -> "0.3.0".\n',
                "    # PR #2000: a reviewed project change.\n"
                '    # release: v0.3.1 changes only MARKETING_VERSION "0.3.0" -> "0.3.1".\n',
            ),
        )

    def test_a_reworded_claim_refuses_the_cut_before_anything_is_written(self):
        root = self.tree()
        readme = root / release_cut.ANDROID_README
        readme.write_text(
            DOCUMENTS[release_cut.ANDROID_README].replace("**v0.3.0**;", "version 0.3.0;"),
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


class HistoryAnchorsSurviveACutCase(unittest.TestCase):
    """A release cut rewrites every version claim; history anchors must not quote one.

    history-check fails on any regression-registry anchor that no longer
    occurs in its file. Four anchors quoted the Android README's
    `**v0.3.0**`, so cutting v0.7.0 turned main's history-check red.
    """

    @staticmethod
    def cut_files() -> set[str]:
        files = {"CHANGELOG.md", "Cargo.toml", release_cut.ANDROID, release_cut.APPLE}
        files |= {claim[0] for claim in release_cut.VERSION_CLAIMS}
        return files

    def anchors(self):
        import tomllib

        registries = [ROOT / "tests" / "client-fixes.toml", ROOT / "validation" / "merge-errata.toml"]
        registries += sorted((ROOT / "validation" / "regressions.d").glob("*.toml"))

        def walk(node, where):
            if isinstance(node, dict):
                for key, value in node.items():
                    if key.endswith("_anchor") and isinstance(value, str):
                        yield where, node.get(key[: -len("_anchor")]), key, value
                    else:
                        yield from walk(value, where)
            elif isinstance(node, list):
                for item in node:
                    yield from walk(item, where)

        for registry in registries:
            if registry.exists():
                yield from walk(tomllib.loads(registry.read_text(encoding="utf-8")), registry.name)

    def test_no_history_anchor_in_a_cut_file_quotes_a_version(self):
        cut_files = self.cut_files()
        self.assertIn("clients/android/README.md", cut_files)
        semver = re.compile(r"(?<![0-9.])[0-9]+\.[0-9]+\.[0-9]+(?![0-9])")
        offenders = [
            f"{where}: {path} {key} = {value!r}"
            for where, path, key, value in self.anchors()
            if path in cut_files and semver.search(value)
        ]
        self.assertEqual(offenders, [], "anchors that a release cut rewrites out from under history-check")


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
        # The gate for a pending release is the green promotion gate its
        # landing already carries, read, never rerun.
        self.assertIn(
            "        if: steps.release.outputs.pending == 'true'\n"
            "        env:\n"
            "          GITHUB_TOKEN: ${{ github.token }}\n"
            "          RELEASE_SHA: ${{ steps.release.outputs.sha }}\n"
            '        run: python3 -m validation.release_evidence --sha "$RELEASE_SHA"',
            self.check,
        )

    def test_the_gate_runs_on_the_release_commit_not_the_tip(self):
        """The commit whose evidence is read is the commit tagged: the release commit.

        `--pending`'s `sha` is the release commit (see
        `test_pending_names_the_release_commit_never_main_s_tip`); the evidence
        check is handed that sha, never HEAD, or it would vouch for the tip
        and tag a commit it never looked at.
        """

        self.assertIn('RELEASE_SHA: ${{ steps.release.outputs.sha }}', self.check)
        self.assertIn('--sha "$RELEASE_SHA"', self.check)
        self.assertIn("sha: ${{ steps.release.outputs.sha }}", self.check)
        # A refusal from `--pending` fails the step instead of vanishing into a pipe.
        find = self.check.split("id: release\n", 1)[1].split("      - name:", 1)[0]
        self.assertNotIn("| tee", find)
        self.assertIn('scripts/release-cut --pending > "$found"', find)
        self.assertIn('grep -qx "sha=$TESTED_SHA"', self.tag)

    def test_it_reruns_no_test_suite(self):
        """Release code passed its gate once, when it merged; tagging it reruns nothing.

        Paul, 2026-11-25: run 9184 spent three hours rerunning the Rust suite
        on v0.9.1's already-gated landing until the runner killed it.
        """

        for command in ("make ", "cargo", "release-readiness\"", "scripts/validate", "rust-toolchain",
                        "cargo-cache", "nonvoter"):
            self.assertNotIn(command, self.check, command)
        self.assertIn("    timeout-minutes: 10\n", self.check)

    def test_the_tag_goes_on_the_commit_the_gate_passed_and_is_annotated(self):
        self.assertIn("ref: ${{ needs.release-check.outputs.sha }}", self.tag)
        self.assertIn('test "$(git rev-parse HEAD)" = "$TESTED_SHA"', self.tag)
        self.assertIn('tag -a "$TAG" -m "$TAG" "$TESTED_SHA"', self.tag)
        self.assertNotIn("--force", self.tag)

    def test_the_push_credential_is_scoped_to_its_one_step(self):
        self.assertEqual(self.workflow.count("secrets.RELEASE_TAG_TOKEN"), 1)
        self.assertNotIn("git config", self.tag)
        self.assertIn('http.extraheader="AUTHORIZATION: basic $auth"', self.tag)


class ReleaseTagPublicationCase(unittest.TestCase):
    """A pushed tag publishes from its commit's existing gate; nothing is retested (Paul, 2026-11-25)."""

    def setUp(self):
        self.workflow = (ROOT / ".github/workflows/release-tag.yml").read_text(encoding="utf-8")

    def test_a_tag_publishes_only_after_its_commit_s_existing_gate(self):
        self.assertIn('on:\n  push:\n    tags: ["v*"]\n\n', self.workflow)
        evidence = self.workflow.split("\n  evidence:\n", 1)[1].split("\n  publish:\n", 1)[0]
        publish = self.workflow.split("\n  publish:\n", 1)[1]
        self.assertIn('sha=$(git rev-parse --verify "refs/tags/$TAG^{commit}")', evidence)
        self.assertIn('python3 -m validation.release_evidence --sha "$sha"', evidence)
        self.assertIn("    needs: evidence\n", publish)
        self.assertIn("    uses: ./.github/workflows/publish-release.yml\n", publish)
        self.assertIn("      release_tag: ${{ github.ref_name }}\n", publish)

    def test_a_tag_starts_no_test_sweep(self):
        for command in ("make ", "cargo", "scripts/validate", "rust-toolchain", "nonvoter"):
            self.assertNotIn(command, self.workflow, command)
        for name in ("ci", "lint"):
            triggers = (ROOT / f".github/workflows/{name}.yml").read_text(encoding="utf-8").split("\njobs:", 1)[0]
            self.assertNotIn("tags:", triggers, name)


if __name__ == "__main__":
    unittest.main()
