"""The Apple per-change notes, and the absence of a stored build number.

Issue #509 moved the Apple narrative out of shared documents into one file per
change, because every two concurrent Apple branches conflicted on the prose.
The build number then went the same way for the same reason: it is derived at
build time by scripts/build-number and stored nowhere, so a fragment names its
issue and never a build.

These cases pin both halves: the narrative cannot come back to a shared
insertion point, no fragment carries a build number, and two Apple branches cut
from one base merge in sequence without a conflict on any file.
"""

from __future__ import annotations

from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

from validation.apple_build import (
    APPLE_README,
    NOTES_DIR,
    PARITY,
    check_repository,
    fragment_paths,
    status_blockquote,
    validate_notes,
)


ROOT = Path(__file__).resolve().parents[2]
PROJECT = "clients/apple/project.yml"


class RepositoryNotesCase(unittest.TestCase):
    def test_the_repository_notes_are_well_formed(self) -> None:
        self.assertEqual(check_repository(ROOT), ())

    def test_the_shared_status_blockquotes_carry_no_per_build_narrative(self) -> None:
        """Fails the moment the relocated ledger comes back.

        This is the revert guard. Restoring a `Build <n> …` sentence to either
        anchored status blockquote restores the collision.
        """
        for path in (APPLE_README, PARITY):
            with self.subTest(path):
                blockquote = status_blockquote(
                    (ROOT / path).read_text(encoding="utf-8"), path
                )
                self.assertNotRegex(blockquote, r"\bBuild [1-9]\d*\b")

    def test_reintroduced_narrative_is_rejected(self) -> None:
        def read(path: str) -> str:
            contents = (ROOT / path).read_text(encoding="utf-8")
            if path == APPLE_README:
                return contents.replace(
                    "> regression suite.",
                    "> regression suite. Build 2000 adds a thing.",
                )
            return contents

        errors = validate_notes(read, fragment_paths(ROOT))

        self.assertTrue(errors, "a narrative sentence in the status blockquote")
        self.assertIn(APPLE_README, errors[0])
        self.assertIn("Build 2000", errors[0])

    def test_body_prose_may_still_narrate_a_past_build(self) -> None:
        """The ban is scoped, not total.

        `APPLE-CLIENT-PARITY.md` explains under its own headings how a behaviour
        came to be. Those sentences are documentation and never grow a ledger;
        banning them would push authors to reword true prose.
        """
        parity = (ROOT / PARITY).read_text(encoding="utf-8")
        self.assertRegex(parity, r"\bBuild [1-9]\d*\b")
        self.assertEqual(check_repository(ROOT), ())

    def test_no_fragment_stores_a_build_number(self) -> None:
        """The number is known only from the landing commit, never in source."""
        for path in fragment_paths(ROOT):
            with self.subTest(path):
                self.assertNotRegex(
                    (ROOT / path).read_text(encoding="utf-8"), r"(?m)^Build:"
                )


class FragmentCase(unittest.TestCase):
    def _errors(self, fragment: str) -> tuple[str, ...]:
        path = f"{NOTES_DIR}/1001-example.md"

        def read(name: str) -> str:
            if name == path:
                return fragment
            return (ROOT / name).read_text(encoding="utf-8")

        return tuple(
            error for error in validate_notes(read, (path,)) if "1001-example" in error
        )

    WELL_FORMED = "# A title\n\nIssue: #1001\n\nProse.\n"

    def test_a_well_formed_fragment_passes(self) -> None:
        self.assertEqual(self._errors(self.WELL_FORMED), ())

    def test_a_fragment_must_open_with_a_title(self) -> None:
        errors = self._errors("Issue: #1001\n\nProse.\n")

        self.assertTrue(errors)
        self.assertIn("title", errors[0])

    def test_a_fragment_must_name_its_issue(self) -> None:
        errors = self._errors("# A title\n\nProse.\n")

        self.assertTrue(errors)
        self.assertIn("Issue:", errors[0])

    def test_a_fragment_must_name_exactly_one_issue(self) -> None:
        errors = self._errors("# A title\n\nIssue: #1001\nIssue: #1002\n")

        self.assertTrue(errors)
        self.assertIn("exactly one", errors[0])

    def test_a_fragment_may_not_carry_a_build_line(self) -> None:
        errors = self._errors("# A title\n\nBuild: 91\nIssue: #1001\n\nProse.\n")

        self.assertTrue(errors)
        self.assertIn("`Build:` line", errors[0])
        self.assertIn("scripts/build-number", errors[0])


class SequentialMergeCase(unittest.TestCase):
    """Two Apple branches cut from one base must merge in sequence, cleanly.

    Issue #509's acceptance criterion, executed: each branch adds its own note
    and changes Apple source, exactly as docs/apple-builds/README.md documents,
    against a scratch repository holding the real notes and release files. With
    no stored counter there is nothing left for the second branch to re-claim.
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
        for path in (PROJECT, APPLE_README, PARITY):
            destination = self.root / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / path, destination)
        shutil.copytree(ROOT / NOTES_DIR, self.root / NOTES_DIR)
        self._git_ok("init", "-q", "-b", "main")
        self._git_ok("add", "-A")
        self._git_ok("commit", "-qm", "base")

    def _apple_change(self, branch: str, issue: int, slug: str) -> None:
        self._git_ok("checkout", "-q", "-b", branch, "main")
        (self.root / NOTES_DIR / f"{issue}-{slug}.md").write_text(
            f"# {slug}\n\nIssue: #{issue}\n\nNarrative for {slug}.\n",
            encoding="utf-8",
        )
        (self.root / "clients/apple/Sources").mkdir(parents=True, exist_ok=True)
        (self.root / f"clients/apple/Sources/{slug}.swift").write_text(
            f"// {slug}\n", encoding="utf-8"
        )
        self._git_ok("add", "-A")
        self._git_ok("commit", "-qm", f"apple change {issue}")

    def _merge(self, branch: str) -> tuple[int, str]:
        self._git_ok("checkout", "-q", "main")
        output = self._git("merge", "--no-ff", "--no-edit", branch)
        return self.last.returncode, output

    def test_two_branches_from_one_base_merge_in_sequence_without_conflict(
        self,
    ) -> None:
        self._apple_change("apple-a", 1001, "feature-a")
        self._apple_change("apple-b", 1002, "feature-b")

        self.assertEqual(self._merge("apple-a")[0], 0)
        code, output = self._merge("apple-b")
        self.assertEqual(
            code,
            0,
            "the second Apple branch must not conflict:\n"
            + output
            + "\n"
            + self._git("diff", "--name-only", "--diff-filter=U"),
        )
        # Neither branch touched a release file, and both narratives survive.
        self.assertEqual(self._git("diff", "--name-only", "HEAD~2", "--", PROJECT), "")
        for issue, slug in ((1001, "feature-a"), (1002, "feature-b")):
            self.assertTrue((self.root / NOTES_DIR / f"{issue}-{slug}.md").is_file())
        self.assertEqual(check_repository(self.root), ())

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
