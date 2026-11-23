from __future__ import annotations

from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

from validation.changelog_field import (
    ChangelogError,
    Entry,
    collect,
    landing_notices,
    parse,
    render,
    require,
)
from validation.runner import REPO_ROOT


class ChangelogFieldCase(unittest.TestCase):
    """The pull request description's `Changelog:` field (validation/changelog_field.py)."""

    def test_entries_keep_section_text_and_continuation_lines(self):
        field = parse(textwrap.dedent("""\
            Summary of the change.

            Changelog: Fixed — **Shared titles start again.** A shared copy
              of a Matroska title now starts from the beginning.
            Changelog: added: A new Settings card.
            Changelog: Removed -- The old pilot.
            """))
        self.assertEqual(field.entries, (
            Entry("Fixed", "**Shared titles start again.** A shared copy\n"
                           "of a Matroska title now starts from the beginning."),
            Entry("Added", "A new Settings card."),
            Entry("Removed", "The old pilot."),
        ))
        self.assertFalse(field.none)
        self.assertEqual(
            render(field.entries)["Fixed"],
            ["- **Shared titles start again.** A shared copy\n"
             "  of a Matroska title now starts from the beginning."],
        )
        self.assertEqual(list(render(field.entries)), ["Added", "Removed", "Fixed"])

    def test_none_is_an_answer_and_may_say_why(self):
        self.assertTrue(parse("Changelog: none").none)
        self.assertEqual(parse("Changelog: None — CI only").reason, "CI only")

    def test_a_missing_field_is_refused_by_require(self):
        self.assertIsNone(parse("Just a description."))
        with self.assertRaisesRegex(ChangelogError, "no `Changelog:` field"):
            require("Just a description.")

    def test_examples_in_comments_and_code_fences_do_not_count(self):
        body = textwrap.dedent("""\
            <!-- Changelog: Fixed — the template's example
            Changelog: Added — also inside the comment -->
            ```
            Changelog: Fixed — quoted in a code block
            ```
            """)
        self.assertIsNone(parse(body))

    def test_a_fence_closes_only_on_its_own_marker_and_hides_its_comment_sample(self):
        body = textwrap.dedent("""\
            ```html
            <!-- an unclosed sample
            ~~~
            ```
            Changelog: Fixed — after the fence.
            """)
        self.assertEqual(parse(body).entries, (Entry("Fixed", "after the fence."),))

    def test_malformed_fields_are_refused_with_the_reason(self):
        for body, reason in (
            ("Changelog: Improved — not a section", "does not start with a section"),
            ("Changelog: Fixed", "no entry text"),
            ("Changelog: none\nChangelog: Fixed — something", "cannot be combined"),
        ):
            with self.subTest(body=body), self.assertRaisesRegex(ChangelogError, reason):
                parse(body)

    def test_the_pull_request_template_carries_no_entry_of_its_own(self):
        template = (REPO_ROOT / ".github/pull_request_template.md").read_text(encoding="utf-8")
        self.assertIn("Changelog:", template)
        # Left blank, the template's line is refused with a reason, never read as an entry.
        with self.assertRaisesRegex(ChangelogError, "is empty"):
            parse(template)


class CollectCase(unittest.TestCase):
    """Release-time collection reads landing commits on the first-parent line only."""

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.root, check=True,
                              capture_output=True, text=True).stdout.strip()

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Changelog Test")
        self.git("config", "user.email", "changelog@example.invalid")
        (self.root / "seed").write_text("seed\n")
        self.git("add", "-A")
        self.git("commit", "-qm", "seed")

    def land(self, number: int, branch_message: str, landing_message: str) -> str:
        self.git("checkout", "-q", "-b", f"pr-{number}")
        (self.root / f"f{number}").write_text("x\n")
        self.git("add", "-A")
        self.git("commit", "-qm", branch_message)
        self.git("checkout", "-q", "main")
        self.git("merge", "-q", "--no-ff", "-m",
                 f"Merge pull request 'change' (#{number}) from pr-{number} into main",
                 "-m", landing_message, f"pr-{number}")
        return self.git("rev-parse", "HEAD")

    def test_branch_commits_never_count_and_bad_landings_are_named(self):
        first = self.land(1, "fix: x\n\nChangelog: Fixed — from a branch commit",
                          "Changelog: Fixed — from the landing")
        self.land(2, "chore: y", "Changelog: Weird — malformed")
        self.land(3, "chore: z", "no field")
        entries, lacking = collect(self.root, None)
        self.assertEqual(entries, [Entry("Fixed", "from the landing")])
        self.assertEqual([landing.pull for landing in lacking], [2, 3])
        self.assertIn("does not start with a section", lacking[0].problem)
        entries, lacking = collect(self.root, first)
        self.assertEqual(entries, [])


    def test_only_landings_after_the_cutover_are_flagged(self):
        before = self.land(1, "chore: old", "landed before the field existed")
        self.git("checkout", "-q", "-b", "cutover")
        (self.root / "validation").mkdir()
        (self.root / "validation/changelog_field.py").write_text("# the field\n")
        self.git("add", "-A")
        self.git("commit", "-qm", "add the field")
        self.git("checkout", "-q", "main")
        self.git("merge", "-q", "--no-ff", "-m",
                 "Merge pull request 'cutover' (#2) from cutover into main", "-m", "Changelog: none", "cutover")
        self.land(3, "chore: new", "forgot the field")
        notices = landing_notices(self.root, None, "HEAD")
        self.assertEqual(len(notices), 1, notices)
        self.assertIn("#3", notices[0])
        self.assertEqual(landing_notices(self.root, before, before), [])

class PaperworkChangelogCase(unittest.TestCase):
    """`scripts/paperwork`'s check: the field for pull requests into main, CHANGELOG.md release-only."""

    def check(self, body, merge_target="origin/main", paths=(), base="HEAD"):
        sys.path.insert(0, str(REPO_ROOT / "scripts"))
        import importlib.machinery
        import importlib.util

        loader = importlib.machinery.SourceFileLoader("paperwork_script", str(REPO_ROOT / "scripts/paperwork"))
        spec = importlib.util.spec_from_loader("paperwork_script", loader)
        module = importlib.util.module_from_spec(spec)
        loader.exec_module(module)
        return module.check_changelog(base, body, merge_target, list(paths))

    def test_a_pull_request_into_main_needs_the_field(self):
        self.assertFalse(self.check("No field.").ok)
        self.assertTrue(self.check("Changelog: none").ok)
        self.assertTrue(self.check("Changelog: Fixed — A fix.").ok)

    def test_a_task_pull_request_into_an_effort_branch_does_not(self):
        self.assertTrue(self.check("No field.", merge_target="origin/effort/x").ok)

    def test_a_local_run_without_the_description_only_warns(self):
        result = self.check(None)
        self.assertTrue(result.ok)
        self.assertTrue(result.warn)

    def test_editing_changelog_md_outside_a_release_cut_is_refused(self):
        # HEAD already carries validation/changelog_field.py once this lands, so
        # a CHANGELOG.md edit against it is not the cutover and not a release.
        base = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, check=True,
                              capture_output=True, text=True).stdout.strip()
        has_module = subprocess.run(["git", "cat-file", "-e", f"{base}:validation/changelog_field.py"],
                                    cwd=REPO_ROOT).returncode == 0
        if not has_module:
            self.skipTest("HEAD predates the field; the cutover pull request itself may edit CHANGELOG.md")
        result = self.check("Changelog: none", paths=["CHANGELOG.md"], base=base)
        self.assertFalse(result.ok)
        self.assertIn("not a release cut", result.detail)


if __name__ == "__main__":
    unittest.main()
