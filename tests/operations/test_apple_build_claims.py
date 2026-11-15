"""Apple changes: per-change notes, no narrative and no number in shared files.

Issue #509. Two concurrent Apple branches used to conflict on prose prepended
to three shared documents, and both claimed the same next build number. The
narrative is one file per change; the number is assigned at release
(validation/mobile_build.py) and appears in no document.

These cases pin both halves: the narrative cannot come back to a shared
insertion point, and two branches cut from one base merge in either order
without a conflict and without either one editing a build number.
"""

from __future__ import annotations

from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

from validation.apple_build import (
    ANDROID_README,
    APPLE_README,
    NOTES_DIR,
    PARITY,
    check_repository,
    fragment_paths,
    status_blockquote,
    validate_notes,
)
from validation.doc_versions import validate_documented_builds
from validation.mobile_build import ANDROID, APPLE
from validation.mobile_versions import check_repository as check_versions


ROOT = Path(__file__).resolve().parents[2]
SURFACE_PATHS = (APPLE, ANDROID, APPLE_README, ANDROID_README, PARITY, "docs/STATUS.html")


class RepositoryClaimCase(unittest.TestCase):
    def test_the_repository_notes_are_whole(self) -> None:
        self.assertEqual(check_repository(ROOT), ())

    def test_the_shared_status_blockquotes_carry_no_per_build_narrative(self) -> None:
        """Fails the moment the relocated ledger comes back.

        This is the revert guard. Restoring a `Build <n> …` sentence to an
        anchored status blockquote restores the collision.
        """
        for path in (APPLE_README, ANDROID_README, PARITY):
            with self.subTest(path):
                blockquote = status_blockquote(
                    (ROOT / path).read_text(encoding="utf-8"), path
                )
                self.assertNotRegex(blockquote, r"\bBuild `?[1-9]\d*\b")

    def test_reintroduced_narrative_is_rejected(self) -> None:
        cases = (
            (APPLE_README, "> regression suite.", "> regression suite. Build 302 adds a thing."),
            (ANDROID_README, "> Android TV.", "> Android TV.\n>\n> Build `208` — adds a thing."),
        )
        for target, anchor, replacement in cases:
            with self.subTest(target):
                def read(path: str) -> str:
                    contents = (ROOT / path).read_text(encoding="utf-8")
                    if path == target:
                        self.assertIn(anchor, contents)
                        return contents.replace(anchor, replacement)
                    return contents

                errors = validate_notes(read, fragment_paths(ROOT))
                self.assertTrue(errors, "a narrative sentence in the status blockquote")
                self.assertIn(target, errors[0])

    def test_body_prose_may_still_narrate_a_past_build(self) -> None:
        """The ban is scoped, not total.

        `APPLE-CLIENT-PARITY.md` explains under its own headings how a behaviour
        came to be. Those sentences are documentation and never grow a ledger;
        banning them would push authors to reword true prose.
        """
        parity = (ROOT / PARITY).read_text(encoding="utf-8")
        self.assertRegex(parity, r"\bBuild [1-9]\d*\b")
        self.assertEqual(check_repository(ROOT), ())

    def test_a_status_line_that_quotes_a_build_number_is_rejected(self) -> None:
        cases = (
            (APPLE_README, "> Status: **v", "> Status: build `302`, **v"),
            (ANDROID_README, "> Status: **v", "> Status: build `208`, **v"),
            (PARITY, "> Status (", "> Status at Apple build 302 ("),
        )
        self.assertEqual(
            validate_documented_builds(lambda path: (ROOT / path).read_text(encoding="utf-8")), ()
        )
        for target, anchor, replacement in cases:
            with self.subTest(target):
                def read(path: str) -> str:
                    contents = (ROOT / path).read_text(encoding="utf-8")
                    if path == target:
                        self.assertEqual(contents.count(anchor), 1)
                        return contents.replace(anchor, replacement)
                    return contents

                errors = validate_documented_builds(read)
                self.assertEqual(len(errors), 1, errors)
                self.assertIn(target, errors[0])
                self.assertIn("assigned at release", errors[0])


class FragmentCase(unittest.TestCase):
    def _errors(self, fragment: str) -> tuple[str, ...]:
        def read(path: str) -> str:
            if path == f"{NOTES_DIR}/1001-example.md":
                return fragment
            return (ROOT / path).read_text(encoding="utf-8")

        return tuple(
            error
            for error in validate_notes(read, (f"{NOTES_DIR}/1001-example.md",))
            if "1001-example" in error
        )

    def test_a_fragment_needs_no_build_line(self) -> None:
        self.assertEqual(self._errors("# A title\n\nIssue: #1001\n\nProse.\n"), ())

    def test_a_fragment_from_before_release_numbering_keeps_its_build_line(self) -> None:
        self.assertEqual(self._errors("# A title\n\nBuild: 90\nIssue: #1001\n\nProse.\n"), ())

    def test_a_fragment_must_open_with_a_title(self) -> None:
        errors = self._errors("Issue: #1001\n\nProse.\n")

        self.assertTrue(errors)
        self.assertIn("title", errors[0])

    def test_a_fragment_must_name_its_issue(self) -> None:
        errors = self._errors("# A title\n\nProse.\n")

        self.assertTrue(errors)
        self.assertIn("Issue:", errors[0])

    def test_a_fragment_may_not_carry_two_build_lines(self) -> None:
        errors = self._errors("# A title\n\nBuild: 90\nBuild: 91\nIssue: #1001\n")

        self.assertTrue(errors)
        self.assertIn("Build:", errors[0])


class SequentialMergeCase(unittest.TestCase):
    """Two Apple branches cut from one base must merge in sequence, cleanly.

    This is issue #509's acceptance criterion executed rather than described,
    against a scratch repository holding the real client documents and both
    manifests, so a layout change that reintroduces a shared insertion point
    or a claimed build number turns it red.
    """

    def _git(self, *arguments: str) -> str:
        result = subprocess.run(
            ["git", *arguments],
            cwd=self.root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            env=self.env,
        )
        self.last = result
        return result.stdout

    def _git_ok(self, *arguments: str) -> str:
        output = self._git(*arguments)
        self.assertEqual(self.last.returncode, 0, output)
        return output

    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="apple-build-claim-"))
        self.addCleanup(shutil.rmtree, self.root, True)
        self.env = {
            "PATH": "/usr/bin:/bin:/usr/local/bin",
            "HOME": str(self.root),
            "TMPDIR": str(self.root),
            "GIT_AUTHOR_NAME": "t",
            "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t",
            "GIT_COMMITTER_EMAIL": "t@example.invalid",
            "GIT_CONFIG_GLOBAL": str(self.root / "gitconfig"),
            "GIT_CONFIG_SYSTEM": "/dev/null",
        }
        for path in SURFACE_PATHS:
            destination = self.root / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / path, destination)
        shutil.copytree(ROOT / NOTES_DIR, self.root / NOTES_DIR)
        shutil.copy(ROOT / "Cargo.toml", self.root / "Cargo.toml")
        self._git_ok("init", "-q", "-b", "main")
        self._git_ok("add", "-A")
        self._git_ok("commit", "-qm", "base")

    def _apple_change(self, branch: str, issue: int, slug: str) -> None:
        """One Apple change, exactly as docs/apple-builds/README.md documents."""
        self._git_ok("checkout", "-q", "-b", branch, "main")
        (self.root / NOTES_DIR / f"{issue}-{slug}.md").write_text(
            f"# {slug}\n\nIssue: #{issue}\n\nNarrative for {slug}.\n",
            encoding="utf-8",
        )
        source = self.root / "clients/apple/Sources" / f"{slug}.swift"
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_text(f"let change = {issue}\n", encoding="utf-8")
        self._git_ok("add", "-A")
        self._git_ok("commit", "-qm", f"apple change {issue}")

    def _merge(self, branch: str) -> tuple[int, str]:
        self._git_ok("checkout", "-q", "main")
        output = self._git("merge", "--no-ff", "--no-edit", branch)
        return self.last.returncode, output

    def test_two_branches_from_one_base_merge_in_either_order_without_conflict(
        self,
    ) -> None:
        self._apple_change("apple-a", 1001, "feature-a")
        self._apple_change("apple-b", 1002, "feature-b")
        base = self._git_ok("rev-parse", "main").strip()
        for order in (("apple-a", "apple-b"), ("apple-b", "apple-a")):
            with self.subTest(order=order):
                self._git_ok("checkout", "-q", "main")
                self._git_ok("reset", "-q", "--hard", base)
                for branch in order:
                    code, output = self._merge(branch)
                    self.assertEqual(
                        code, 0,
                        f"{branch} must not conflict:\n{output}\n"
                        + self._git("diff", "--name-only", "--diff-filter=U"),
                    )
                # Both narratives survive; neither overwrote the other.
                for issue, slug in ((1001, "feature-a"), (1002, "feature-b")):
                    self.assertTrue((self.root / NOTES_DIR / f"{issue}-{slug}.md").is_file())
                # Neither branch wrote a build number, so the merged tree
                # carries the development build and nothing to re-claim.
                self.assertEqual(check_versions(self.root), ())
                self.assertEqual(
                    self._git_ok("diff", "--name-only", base, "HEAD", "--", APPLE, ANDROID), ""
                )

    def test_narrative_at_a_shared_insertion_point_still_conflicts(self) -> None:
        """The contrast case: relocating the narrative is what removes the conflict.

        Both branches append a sentence to the same anchored status blockquote,
        which is exactly what every Apple change did before #509. Git conflicts,
        confirming the sequential-merge result above is earned by the layout and
        not by the scratch repository being easy to merge.
        """
        for branch, sentence in (("prose-a", "Build A."), ("prose-b", "Build B.")):
            self._git_ok("checkout", "-q", "-b", branch, "main")
            readme = self.root / APPLE_README
            contents = readme.read_text(encoding="utf-8")
            readme.write_text(
                contents.replace(
                    "> regression suite.", f"> regression suite. {sentence}"
                ),
                encoding="utf-8",
            )
            self._git_ok("commit", "-qam", branch)

        self.assertEqual(self._merge("prose-a")[0], 0)
        code, output = self._merge("prose-b")

        self.assertNotEqual(code, 0, "shared-insertion-point prose must conflict")
        self.assertIn("CONFLICT", output)
        self._git_ok("merge", "--abort")


if __name__ == "__main__":
    unittest.main()
