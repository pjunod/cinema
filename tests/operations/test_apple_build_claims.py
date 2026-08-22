"""The Apple build claim: one owner, generated copies, per-change notes.

Issue #509. The build number is a single monotonic counter that used to be
duplicated as prose in four documents. Two concurrent Apple branches conflicted
on the prose and silently auto-merged the number to a value that was already
taken, so every base move cost three prose merges plus six hand-edited mentions
that had merged clean and wrong.

These cases pin both halves: the narrative cannot come back to a shared
insertion point, and two branches cut from one base merge in sequence without a
conflict on any build-claim file.
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
    PROJECT,
    STATUS,
    AppleBuildError,
    bump,
    check_repository,
    current_build,
    fragment_paths,
    render,
    status_blockquote,
    validate_notes,
)


ROOT = Path(__file__).resolve().parents[2]
SURFACE_PATHS = (PROJECT, APPLE_README, PARITY, STATUS)


class RepositoryClaimCase(unittest.TestCase):
    def test_the_repository_claim_is_whole(self) -> None:
        self.assertEqual(check_repository(ROOT), ())

    def test_the_shared_status_blockquotes_carry_no_per_build_narrative(self) -> None:
        """Fails the moment the relocated ledger comes back.

        This is the revert guard. Restoring a `Build <n> …` sentence to either
        anchored status blockquote restores the collision, and nothing else in
        the gate would notice, because the number itself still agrees.
        """
        for path in (APPLE_README, PARITY):
            with self.subTest(path):
                blockquote = status_blockquote(
                    (ROOT / path).read_text(encoding="utf-8"), path
                )
                self.assertNotRegex(blockquote, r"\bBuild [1-9]\d*\b")

    def test_reintroduced_narrative_is_rejected(self) -> None:
        build = current_build(ROOT)

        def read(path: str) -> str:
            contents = (ROOT / path).read_text(encoding="utf-8")
            if path == APPLE_README:
                return contents.replace(
                    "> regression suite.",
                    f"> regression suite. Build {build} adds a thing.",
                )
            return contents

        errors = validate_notes(read, build, fragment_paths(ROOT))

        self.assertTrue(errors, "a narrative sentence in the status blockquote")
        self.assertIn(APPLE_README, errors[0])
        self.assertIn(f"Build {build}", errors[0])

    def test_body_prose_may_still_narrate_a_past_build(self) -> None:
        """The ban is scoped, not total.

        `APPLE-CLIENT-PARITY.md` explains under its own headings how a behaviour
        came to be. Those sentences are documentation and never grow a ledger;
        banning them would push authors to reword true prose.
        """
        parity = (ROOT / PARITY).read_text(encoding="utf-8")
        self.assertRegex(parity, r"\bBuild [1-9]\d*\b")
        self.assertEqual(check_repository(ROOT), ())

    def test_a_hand_edited_generated_surface_is_rejected(self) -> None:
        build = current_build(ROOT)

        def read(path: str) -> str:
            contents = (ROOT / path).read_text(encoding="utf-8")
            if path == STATUS:
                return contents.replace(
                    f"· Apple build {build} source, not yet uploaded",
                    f"· Apple build {build - 1} source, not yet uploaded",
                )
            return contents

        stale = render(read, build)

        self.assertIn(STATUS, stale)

    def test_a_missing_generated_surface_fails_closed(self) -> None:
        """A reworded anchor must be a hard error, not a silent no-op rewrite."""

        def read(path: str) -> str:
            contents = (ROOT / path).read_text(encoding="utf-8")
            if path == PROJECT:
                return contents.replace("CURRENT_PROJECT_VERSION:", "BUILD_NUMBER:")
            return contents

        with self.assertRaises(AppleBuildError) as raised:
            render(read, current_build(ROOT))

        self.assertIn("exactly one", str(raised.exception))


class FragmentCase(unittest.TestCase):
    BUILD = 90

    def _read(self, fragment: str):
        def read(path: str) -> str:
            if path == f"{NOTES_DIR}/1001-example.md":
                return fragment
            return (ROOT / path).read_text(encoding="utf-8")

        return read

    def _errors(self, fragment: str) -> tuple[str, ...]:
        return tuple(
            error
            for error in validate_notes(
                self._read(fragment), self.BUILD, (f"{NOTES_DIR}/1001-example.md",)
            )
            if "1001-example" in error
        )

    WELL_FORMED = "# A title\n\nBuild: 90\nIssue: #1001\n\nProse.\n"

    def test_a_well_formed_fragment_passes(self) -> None:
        self.assertEqual(self._errors(self.WELL_FORMED), ())

    def test_a_fragment_must_open_with_a_title(self) -> None:
        errors = self._errors("Build: 90\nIssue: #1001\n\nProse.\n")

        self.assertTrue(errors)
        self.assertIn("title", errors[0])

    def test_a_fragment_must_name_its_issue(self) -> None:
        errors = self._errors("# A title\n\nBuild: 90\n\nProse.\n")

        self.assertTrue(errors)
        self.assertIn("Issue:", errors[0])

    def test_a_fragment_must_carry_exactly_one_build_line(self) -> None:
        errors = self._errors("# A title\n\nBuild: 90\nBuild: 91\nIssue: #1001\n")

        self.assertTrue(errors)
        self.assertIn("Build:", errors[0])

    def test_a_fragment_may_not_claim_a_build_project_yml_has_not_reached(
        self,
    ) -> None:
        errors = self._errors("# A title\n\nBuild: 91\nIssue: #1001\n\nProse.\n")

        self.assertTrue(errors)
        self.assertIn("claims Apple build 91", errors[0])
        self.assertIn("apple-build-bump", errors[0])


class SequentialMergeCase(unittest.TestCase):
    """Two Apple branches cut from one base must merge in sequence, cleanly.

    This is issue #509's acceptance criterion executed rather than described. It
    runs the real bump tool against a scratch repository holding the real
    build-claim surfaces, so a layout change that reintroduces a shared
    insertion point turns it red.
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
        self._git_ok("init", "-q", "-b", "main")
        self._git_ok("add", "-A")
        self._git_ok("commit", "-qm", "base")

    def _apple_change(self, branch: str, issue: int, slug: str) -> int:
        """One Apple change, exactly as docs/apple-builds/README.md documents."""
        self._git_ok("checkout", "-q", "-b", branch, "main")
        (self.root / NOTES_DIR / f"{issue}-{slug}.md").write_text(
            f"# {slug}\n\nBuild: {current_build(self.root)}\nIssue: #{issue}\n\n"
            f"Narrative for {slug}.\n",
            encoding="utf-8",
        )
        claimed, _ = bump(self.root, merge_target="main")
        self._git_ok("add", "-A")
        self._git_ok("commit", "-qm", f"apple change {issue}")
        return claimed

    def _merge(self, branch: str) -> tuple[int, str]:
        self._git_ok("checkout", "-q", "main")
        output = self._git("merge", "--no-ff", "--no-edit", branch)
        return self.last.returncode, output

    def test_two_branches_from_one_base_merge_in_sequence_without_conflict(
        self,
    ) -> None:
        base = current_build(self.root)
        first = self._apple_change("apple-a", 1001, "feature-a")
        second = self._apple_change("apple-b", 1002, "feature-b")
        self.assertEqual((first, second), (base + 1, base + 1))

        code, output = self._merge("apple-a")
        self.assertEqual(code, 0, output)

        code, output = self._merge("apple-b")
        self.assertEqual(
            code,
            0,
            "the second Apple branch must not conflict:\n"
            + output
            + "\n"
            + self._git("diff", "--name-only", "--diff-filter=U"),
        )
        self.assertEqual(self._git("diff", "--name-only", "--diff-filter=U"), "")

        # Both narratives survive; neither overwrote the other.
        for issue, slug in ((1001, "feature-a"), (1002, "feature-b")):
            self.assertTrue((self.root / NOTES_DIR / f"{issue}-{slug}.md").is_file())

    def test_the_second_branch_reclaims_the_counter_with_one_generated_diff(
        self,
    ) -> None:
        """The re-bump is what remains, and it must stay mechanical.

        A monotonic counter checked against the merge target cannot avoid a
        re-claim once a competing Apple change lands. What #509 removes is its
        cost: one base sync Git resolves by itself, one tool run, and one
        generated diff — no prose merge, and no hand-edited mention left
        agreeing with a stale number.

        The order is the one the project already mandates: bring `main` into the
        branch first, then claim. Claiming before the sync sets the branch to
        `base + 2` while `main` holds `base + 1` on the same line, which is an
        ordinary same-line conflict and the one thing the tool cannot merge for
        you.
        """
        base = current_build(self.root)
        self._apple_change("apple-a", 1001, "feature-a")
        self._apple_change("apple-b", 1002, "feature-b")
        self.assertEqual(self._merge("apple-a")[0], 0)

        self._git_ok("checkout", "-q", "apple-b")
        sync = self._git("merge", "--no-ff", "--no-edit", "main")
        self.assertEqual(self.last.returncode, 0, "the base sync is conflict-free:\n" + sync)
        self.assertEqual(current_build(self.root), base + 1)

        claimed, changed = bump(self.root, merge_target="main")

        self.assertEqual(claimed, base + 2)
        self.assertEqual(
            set(changed),
            {*SURFACE_PATHS, f"{NOTES_DIR}/1002-feature-b.md"},
            "the bump rewrites every generated surface and only this branch's note",
        )
        self.assertNotIn(f"{NOTES_DIR}/1001-feature-a.md", changed)

        self._git_ok("commit", "-qam", "reclaim")
        code, output = self._merge("apple-b")
        self.assertEqual(code, 0, output)
        self.assertEqual(current_build(self.root), base + 2)
        self.assertEqual(check_repository(self.root), ())

    def test_claiming_twice_confirms_the_claim_rather_than_inflating_it(self) -> None:
        """Re-running the tool is the documented response to a base move.

        It has to be safe when the base did not move, or the advice becomes
        "run it, but only once, and only if you are sure" — which is how a
        counter drifts.
        """
        base = current_build(self.root)
        self._git_ok("checkout", "-q", "-b", "apple-a", "main")
        (self.root / NOTES_DIR / "1001-feature-a.md").write_text(
            f"# feature-a\n\nBuild: {base}\nIssue: #1001\n\nProse.\n",
            encoding="utf-8",
        )

        first, changed = bump(self.root, merge_target="main")
        self.assertEqual(first, base + 1)
        self.assertTrue(changed)

        second, changed_again = bump(self.root, merge_target="main")

        self.assertEqual(second, base + 1)
        self.assertEqual(changed_again, ())

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
