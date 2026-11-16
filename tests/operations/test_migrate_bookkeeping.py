"""`scripts/migrate-bookkeeping` moves an open branch onto the per-change layout.

A branch cut before docs/ci/SHARED-BOOKKEEPING.md landed carries its changelog
entry, client fix row, count change, governed path, status entry and claimed
build number as edits to shared files. After `git merge <target>` one run of
the tool has to leave those files exactly as the target has them, with the
branch's bookkeeping in fragments and nothing lost.
"""

from __future__ import annotations

from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/migrate-bookkeeping"
SHARED = (
    "CHANGELOG.md", "tests/client-fixes.toml", "tests/playback/rolling-producer-owners.toml",
    "validation/points.toml", "STATUS.md", "clients/android/app/build.gradle.kts",
    "clients/apple/project.yml", "clients/android/README.md",
)

CHANGELOG = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- An old fix.\n\n## [0.8.0] — 2026-10-29\n\n- Released.\n"
FIX = (
    '[[fixes]]\nid = "{id}"\ncommits = ["1111111"]\nsource = "a.kt"\nsource_anchor = "fun a()"\n'
    'test = "a_test.kt"\ntest_anchor = "fun testA()"\n'
)
LEDGER = 'version = 1\nenforce_after = "abcdef1"\n\n# An old row.\n' + FIX.format(id="old")
OWNERS = (
    'version = 1\n\n[[symbols]]\nid = "legacy-flag"\npattern = "FLAG"\nexpected_occurrences = 3\n\n'
    '[[module_symbols]]\nid = "sentinel"\npattern = "S"\nexpected_occurrences = 0\n\n'
    '[[module_structures]]\nid = "task-spawn"\n# Reviewed: ten owned spawns.\n\n# A second paragraph.\n'
    'pattern = "spawn"\nexpected_occurrences = 10\n'
)
CATALOG = (
    'version = 1\n\n[[points]]\nid = "one"\ntitle = "One"\ncontract = "c"\npaths = [\n  "a/**",\n  "b",\n]\n'
    'checks = []\n\n[[points]]\nid = "two"\ntitle = "Two"\ncontract = "c"\npaths = ["c/**"]\nchecks = []\n'
)
STATUS_OLD = (
    "# Status\n\n**Updated:** 2026-11-13\n<!-- status: insert new efforts below this line -->\n"
    "- **2026-11-13 · Trim fidelity:** Owner Codex. Built; [ledger](notes/x.md).\n\n"
    "## Older efforts — where each one now lives\n\n| a | b |\n"
)
STATUS_NEW = "# Status\n\nEach effort is a file under status.d/.\n\n## Older efforts — where each one now lives\n\n| a | b |\n"
GRADLE = "android {\n        versionCode = {n}\n        versionName = \"0.8.0\"\n}\n"
PROJECT = 'settings:\n    MARKETING_VERSION: "0.8.0"\n    CURRENT_PROJECT_VERSION: "{n}"\n'
README_OLD = "# Android\n\nIntro.\n\n> Status: **v0.8.0**, build `207` — an old change.\n>\n> Build `206` — older.\n\nBody.\n"
PATCHES = (
    "# Vendored Hiqlite\n\nPlurx carries two patches.\n\n**Owner:** someone.\n\n"
    "Background on why the fork exists,\nover a few lines,\nso edits have room.\n\n"
    "| # | Patch | Kind | Upstream | Drop condition |\n|---:|---|---|---|---|\n"
    "| 1 | First | generic bug | pending M6 | Upstream fixes it. |\n"
    "| 2 | Second | plurx policy | — | Never; Plurx owns it. |\n\n"
    "- The first patch.\n  Files: `src/a.rs`.\n- The second patch.\n  Files: `src/a.rs`.\n\n"
    "Remove this vendor when (rows 1, 9) retire and (rows 2, 8) are unneeded.\n"
)
LEDGER_FILE = "vendor/hiqlite/PLURX-PATCH.md"
README_NEW = "# Android\n\nIntro.\n\n> Status: **v0.8.0** — native viewer.\n\nBody.\n"


class MigrateBookkeepingCase(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "t")
        self.git("config", "user.email", "t@example.invalid")
        for path, text in {
            "CHANGELOG.md": CHANGELOG, "tests/client-fixes.toml": LEDGER,
            "tests/playback/rolling-producer-owners.toml": OWNERS, "validation/points.toml": CATALOG,
            "STATUS.md": STATUS_OLD, "clients/android/README.md": README_OLD,
            "clients/android/app/build.gradle.kts": GRADLE.replace("{n}", "207"),
            "clients/apple/project.yml": PROJECT.replace("{n}", "301"),
            LEDGER_FILE: PATCHES,
        }.items():
            self.write(path, text)
        self.commit("old layout")
        self.git("branch", "old")
        # The target adopts the per-change layout.
        for path, text in {
            "changelog.d/README.md": "# Changelog fragments\n", "STATUS.md": STATUS_NEW,
            "status.d/2026-11-13-trim-fidelity.md": "# Trim fidelity\n\nOwner Codex. Built; [ledger](../notes/x.md).\n",
            "clients/android/README.md": README_NEW,
            "clients/android/app/build.gradle.kts": GRADLE.replace("{n}", "1"),
            "clients/apple/project.yml": PROJECT.replace("{n}", "1"),
            LEDGER_FILE: PATCHES.replace("carries two patches", "carries two numbered patches"),
        }.items():
            self.write(path, text)
        self.commit("per-change layout")

    def git(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *args], cwd=self.root, check=check, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )

    def write(self, path: str, text: str) -> None:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def read(self, path: str) -> str:
        return (self.root / path).read_text(encoding="utf-8")

    def commit(self, message: str) -> None:
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)

    def migrate(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--target", "main", *args], cwd=self.root, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )

    def old_style_branch(self) -> None:
        self.git("checkout", "-q", "-b", "codex/In_Flight", "old")
        self.write("CHANGELOG.md", CHANGELOG.replace(
            "### Fixed\n\n", "### Fixed\n\n- A new fix that runs\n  onto a second line.\n", 1
        ).replace("## [0.8.0]", "### Added\n\n- A new thing.\n\n## [0.8.0]", 1))
        self.write("tests/client-fixes.toml", LEDGER.replace(
            "# An old row.", "# The branch's row.\n" + FIX.format(id="new") + "\n# An old row.", 1))
        self.write("tests/playback/rolling-producer-owners.toml", OWNERS.replace(
            'pattern = "spawn"\nexpected_occurrences = 10',
            '# Branch: +2 joined fixture tasks,\n# both awaited.\npattern = "spawn"\nexpected_occurrences = 12',
        ).replace("expected_occurrences = 3", "expected_occurrences = 2"))
        self.write("validation/points.toml", CATALOG.replace('  "b",\n', '  "b",\n  "scripts/new-tool",\n'))
        self.write("STATUS.md", STATUS_OLD.replace(
            "this line -->\n",
            "this line -->\n- **2026-11-15 · New effort:** Owner Claude. Draft; [plan](notes/p.md).\n", 1,
        ).replace("Owner Codex. Built;", "Owner Codex. Qualified;"))
        self.write("clients/android/app/build.gradle.kts", GRADLE.replace("{n}", "208"))
        self.write("clients/apple/project.yml", PROJECT.replace("{n}", "302"))
        self.write("clients/android/README.md", README_OLD.replace(
            "> Status: **v0.8.0**, build `207` — an old change.",
            "> Status: **v0.8.0**, build `208` — the branch's change.\n>\n> Build `207` — an old change.",
        ))
        self.write(LEDGER_FILE, PATCHES.replace("carries two patches", "carries three patches").replace(
            "| 2 | Second | plurx policy | — | Never; Plurx owns it. |\n",
            "| 2 | Second | plurx policy | — | Never; Plurx owns it. |\n"
            "| 3 | Keep the writer alive | generic bug | pending M6 | Upstream delivers every reply. |\n",
        ).replace(
            "- The second patch.\n  Files: `src/a.rs`.\n",
            "- The second patch.\n  Files: `src/a.rs`.\n"
            "- The writer no longer panics on a dropped receiver.\n  Files: `src/a.rs`.\n",
        ).replace("(rows 1, 9)", "(rows 1, 9 and 3)"))
        self.write("src/code.rs", "fn main() {}\n")
        self.commit("old-layout edits")

    def test_one_run_after_the_merge_moves_everything_and_leaves_no_conflict(self) -> None:
        self.old_style_branch()
        merged = self.git("merge", "main", check=False)
        self.assertNotEqual(merged.returncode, 0, "the old layout conflicts with the new")
        self.assertIn("CONFLICT", merged.stdout)

        result = self.migrate()

        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.git("diff", "--name-only", "--diff-filter=U").stdout, "")
        # Every shared file is exactly the target's again.
        for path in SHARED:
            with self.subTest(path=path):
                self.assertEqual(self.read(path), self.git("show", f"main:{path}").stdout)
        slug = "codex-in-flight"
        self.assertEqual(
            self.read(f"changelog.d/{slug}.md"),
            "### Fixed\n\n- A new fix that runs\n  onto a second line.\n\n### Added\n\n- A new thing.\n",
        )
        fixes = tomllib.loads(self.read(f"tests/client-fixes.d/{slug}.toml"))["fixes"]
        self.assertEqual([row["id"] for row in fixes], ["new"])
        self.assertIn("# The branch's row.", self.read(f"tests/client-fixes.d/{slug}.toml"))
        changes = tomllib.loads(self.read(f"tests/rolling-producer-owners.d/{slug}.toml"))["changes"]
        self.assertEqual(
            [(row["id"], row["delta"]) for row in changes], [("legacy-flag", -1), ("task-spawn", 2)]
        )
        self.assertEqual(changes[1]["reason"], "Branch: +2 joined fixture tasks, both awaited.")
        self.assertIn("Count moved from 3 to 2", changes[0]["reason"])
        self.assertEqual(
            tomllib.loads(self.read(f"validation/points.d/{slug}.toml"))["paths"],
            [{"point": "one", "add": ["scripts/new-tool"]}],
        )
        self.assertEqual(
            self.read("status.d/2026-11-15-new-effort.md"),
            "# New effort\n\nOwner Claude. Draft; [plan](../notes/p.md).\n",
        )
        # The branch's update to an effort the target already moved lands on its file.
        self.assertEqual(
            self.read("status.d/2026-11-13-trim-fidelity.md"),
            "# Trim fidelity\n\nOwner Codex. Qualified; [ledger](../notes/x.md).\n",
        )
        # The patch the branch numbered 3 is a file keyed by name; the ledger,
        # its count and its row lists are the target's again.
        self.assertEqual(self.read(LEDGER_FILE), self.git("show", f"main:{LEDGER_FILE}").stdout)
        patch = tomllib.loads(self.read("vendor/hiqlite/patches.d/keep-the-writer-alive.toml"))
        self.assertEqual(
            patch,
            {
                "patch": "Keep the writer alive", "kind": "generic bug", "upstream": "pending M6",
                "drop": "Upstream delivers every reply.",
                "description": "The writer no longer panics on a dropped receiver.\nFiles: `src/a.rs`.\n",
            },
        )
        self.assertIn("the branch's change.", result.stdout, "the dropped narrative is shown")
        # Work that is not bookkeeping is untouched, and the merge can be committed.
        self.assertEqual(self.read("src/code.rs"), "fn main() {}\n")
        self.git("commit", "-q", "-m", "merge main")
        # A second run finds nothing left to move.
        again = self.migrate()
        self.assertEqual(again.returncode, 0, again.stdout)
        self.assertIn("no shared-file bookkeeping to move", again.stdout)

    def test_a_branch_edit_that_really_conflicts_is_left_and_named(self) -> None:
        self.git("checkout", "-q", "-b", "feature", "old")
        self.write("STATUS.md", STATUS_OLD.replace(
            "this line -->\n", "this line -->\n- **2026-11-15 · New effort:** Owner Claude.\n", 1
        ).replace("| a | b |", "| a | changed by the branch |"))
        self.commit("status with an index edit")
        # The target rewrites the same index row, so the page cannot be settled.
        self.git("checkout", "-q", "main")
        self.write("STATUS.md", STATUS_NEW.replace("| a | b |", "| a | changed by the target |"))
        self.commit("the target edits the index too")
        self.git("checkout", "-q", "feature")
        self.git("merge", "main", check=False)
        result = self.migrate()
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("MANUAL  STATUS.md: not migrated and nothing written", result.stdout)
        self.assertIn("status.d/<YYYY-MM-DD>-<slug>.md", result.stdout, "the instruction is exact")
        self.assertIn("NOT complete", result.stdout)
        # Nothing is in two places: the effort was not written, the page not touched.
        self.assertFalse((self.root / "status.d/2026-11-15-new-effort.md").exists())
        self.assertIn("STATUS.md", self.git("diff", "--name-only", "--diff-filter=U").stdout)
        self.assertIn("<<<<<<<", self.read("STATUS.md"))

    def test_an_effort_the_branch_retired_and_an_index_row_it_added_both_carry_over(self) -> None:
        self.git("checkout", "-q", "-b", "feature", "old")
        self.write("STATUS.md", STATUS_OLD.replace(
            "- **2026-11-13 · Trim fidelity:** Owner Codex. Built; [ledger](notes/x.md).\n",
            "- **2026-11-15 · New effort:** Owner Claude.\n",
        ).replace("**Updated:** 2026-11-13", "**Updated:** 2026-11-15") + "| trim | history |\n")
        self.commit("retire the oldest effort, add one, index the retired one")
        self.git("merge", "main", check=False)
        result = self.migrate()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.read("STATUS.md"), STATUS_NEW + "| trim | history |\n")
        self.assertTrue((self.root / "status.d/2026-11-15-new-effort.md").is_file())
        self.assertFalse((self.root / "status.d/2026-11-13-trim-fidelity.md").exists())

    def test_committed_conflict_markers_are_never_carried_into_a_fragment(self) -> None:
        self.git("checkout", "-q", "-b", "feature", "old")
        self.write("CHANGELOG.md", CHANGELOG.replace(
            "### Fixed\n\n",
            "### Fixed\n\n<<<<<<< HEAD\n- One side.\n=======\n- The other side.\n>>>>>>> other\n", 1))
        self.commit("a changelog with committed markers")
        self.git("merge", "main", check=False)
        result = self.migrate()
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("conflict markers", result.stdout)
        self.assertFalse((self.root / "changelog.d/feature.md").exists())
        self.assertIn("<<<<<<< HEAD", self.read("CHANGELOG.md"), "the file is left as it was")

    def patch_branch(self, extra) -> None:
        self.git("checkout", "-q", "-b", "feature", "old")
        text = PATCHES.replace("carries two patches", "carries three patches").replace(
            "| 2 | Second | plurx policy | — | Never; Plurx owns it. |\n",
            "| 2 | Second | plurx policy | — | Never; Plurx owns it. |\n"
            "| 3 | Third | generic bug | pending M6 | Upstream fixes it. |\n",
        ).replace(
            "- The second patch.\n  Files: `src/a.rs`.\n",
            "- The second patch.\n  Files: `src/a.rs`.\n- The third patch.\n  Files: `src/a.rs`.\n",
        ).replace("(rows 1, 9)", "(rows 1, 9 and 3)")
        self.write(LEDGER_FILE, extra(text))
        self.commit("a patch the old way")
        self.git("merge", "main", check=False)

    def test_a_branch_edit_outside_the_patch_table_survives_the_move(self) -> None:
        self.patch_branch(lambda text: text.replace(
            "so edits have room.\n", "so edits have room.\nA sentence the branch added to the intro.\n"
        ) + "\nA paragraph the branch added after the removal condition.\n")
        result = self.migrate()
        self.assertEqual(result.returncode, 0, result.stdout)
        ledger = self.read(LEDGER_FILE)
        self.assertIn("A sentence the branch added to the intro.", ledger)
        self.assertIn("A paragraph the branch added after the removal condition.", ledger)
        # The row, its bullet and the restated count are gone; the target's wording is in.
        self.assertNotIn("| 3 |", ledger)
        self.assertNotIn("The third patch.", ledger)
        self.assertIn("carries two numbered patches", ledger)
        self.assertIn("(rows 1, 9) retire", ledger)
        self.assertTrue((self.root / "vendor/hiqlite/patches.d/third.toml").is_file())

    def test_a_patch_row_beside_an_edit_to_an_existing_row_is_refused_whole(self) -> None:
        # The branch restated the count on a second line, so the derived lines
        # cannot be told apart from its own prose and the target's rewording
        # of the count genuinely conflicts with it.
        self.patch_branch(lambda text: text.replace(
            "Plurx carries three patches.\n", "Plurx carries three patches.\nPlurx carries them with care.\n"))
        result = self.migrate()
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn(f"MANUAL  {LEDGER_FILE}: not migrated and nothing written", result.stdout)
        self.assertIn("vendor/hiqlite/patches.d/<name>.toml", result.stdout)
        # Not in two places: no patch file, and the ledger still holds the row.
        self.assertFalse((self.root / "vendor/hiqlite/patches.d").exists())
        self.assertIn("| 3 | Third |", self.read(LEDGER_FILE))

    def test_a_target_without_the_layout_is_refused(self) -> None:
        self.git("checkout", "-q", "-b", "feature", "old")
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--target", "old"], cwd=self.root, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("does not have the per-change layout", result.stdout)


if __name__ == "__main__":
    unittest.main()
