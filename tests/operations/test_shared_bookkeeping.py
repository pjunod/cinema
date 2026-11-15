"""Bookkeeping a pull request adds is a file of its own, never a shared line.

docs/ci/SHARED-BOOKKEEPING.md: changelog entries, client fix anchors, status
entries and reviewed count changes each live one file per change, so two pull
requests cannot conflict on them. These rules keep a branch from going back
to the shared file, and prove the per-change files lose nothing.
"""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from validation import changelog_fragments as changelog  # noqa: E402
from validation.history import HistoryError, load_client_fixes  # noqa: E402

BASE = os.environ.get("PLURX_VALIDATION_BASE")
LEDGER = "tests/client-fixes.toml"

ROW = (
    '[[fixes]]\nid = "{id}"\ncommits = ["{sha}"]\nsource = "a.kt"\n'
    'source_anchor = "fun a()"\ntest = "a_test.kt"\ntest_anchor = "fun testA()"\n'
)


def base_file(path: str) -> str | None:
    shown = subprocess.run(
        ["git", "show", f"{BASE}:{path}"], cwd=ROOT, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    )
    return shown.stdout if shown.returncode == 0 else None


class RepositoryCase(unittest.TestCase):
    def test_changelog_fragments_are_well_formed_and_assemble(self):
        self.assertEqual(list(changelog.validate(ROOT)), [])
        text = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
        changelog.assemble(changelog.read_fragments(ROOT), changelog.unreleased_body(text))

    def test_the_branch_adds_nothing_under_unreleased(self):
        if not BASE:
            self.skipTest("no merge base given (PLURX_VALIDATION_BASE)")
        self.assertEqual(list(changelog.check_branch(ROOT, BASE)), [])

    def test_the_client_fix_ledger_gains_no_rows(self):
        if not BASE:
            self.skipTest("no merge base given (PLURX_VALIDATION_BASE)")
        before = base_file(LEDGER)
        if before is None:
            self.skipTest(f"{LEDGER} is not on the merge base")
        known = {row["id"] for row in tomllib.loads(before).get("fixes", [])}
        now = tomllib.loads((ROOT / LEDGER).read_text(encoding="utf-8")).get("fixes", [])
        added = sorted({row["id"] for row in now} - known)
        self.assertEqual(
            added,
            [],
            f"{LEDGER} is closed to new rows: put each in "
            "tests/client-fixes.d/<slug>.toml (tests/client-fixes.d/README.md)",
        )

    def test_client_fix_fragments_load_with_the_ledger(self):
        ledger = load_client_fixes(ROOT / LEDGER)
        fragment_rows = [fix for fix in ledger.fixes if fix.origin is not None]
        files = sorted((ROOT / "tests/client-fixes.d").glob("*.toml"))
        self.assertEqual(
            {fix.origin for fix in fragment_rows}, {f"client-fixes.d/{path.name}" for path in files}
        )


class ChangelogFragmentCase(unittest.TestCase):
    def test_fragments_fold_by_category_in_name_order_ahead_of_the_section(self):
        body = changelog.assemble(
            {
                "b.md": "### Fixed\n\n- B fixed.\n\n### Added\n\n- B added.\n",
                "a.md": "### Fixed\n\n- A fixed.\n",
            },
            "\n### Fixed\n\n- Already listed.\n",
        )
        self.assertEqual(
            body,
            "### Added\n\n- B added.\n\n"
            "### Fixed\n\n- A fixed.\n- B fixed.\n- Already listed.\n\n",
        )

    def test_no_entry_is_lost_whatever_order_the_files_arrive_in(self):
        fragments = {f"{n}.md": f"### Fixed\n\n- Entry {n}.\n" for n in "dcba"}
        one = changelog.assemble(fragments)
        two = changelog.assemble(dict(reversed(list(fragments.items()))))
        self.assertEqual(one, two)
        for n in "abcd":
            self.assertEqual(one.count(f"- Entry {n}."), 1)

    def test_a_malformed_fragment_is_refused(self):
        cases = {
            "no heading": "- An entry.\n",
            "unknown category": "### Improved\n\n- An entry.\n",
            "empty category": "### Fixed\n\n### Added\n\n- An entry.\n",
            "release heading": "### Fixed\n\n- An entry.\n\n## [1.0.0]\n",
        }
        for name, text in cases.items():
            with self.subTest(fragment=name), self.assertRaises(changelog.FragmentError):
                changelog.parse(text, "x.md")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "changelog.d").mkdir()
            (root / "changelog.d" / "README.md").write_text("# not a fragment\n")
            (root / "changelog.d" / "Bad Name.md").write_text("### Fixed\n\n- x\n")
            (root / "changelog.d" / "empty.md").write_text("\n")
            (root / "changelog.d" / "good.md").write_text("### Fixed\n\n- x\n")
            self.assertEqual(len(changelog.validate(root)), 2)

    def test_only_additions_under_unreleased_are_refused(self):
        base = "# C\n\n## [Unreleased]\n\n### Fixed\n\n- One.\n- Two.\n\n## [1.0.0] — 2026-01-01\n\n- Old.\n"
        self.assertEqual(changelog.added_to_unreleased(base, base), ())
        removed = base.replace("- One.\n", "")
        self.assertEqual(changelog.added_to_unreleased(base, removed), ())
        cut = "# C\n\n## [Unreleased]\n\n## [1.1.0] — 2026-02-01\n\n### Fixed\n\n- One.\n- Two.\n"
        self.assertEqual(changelog.added_to_unreleased(base, cut), ())
        added = base.replace("- One.\n", "- Zero.\n- One.\n")
        self.assertEqual(changelog.added_to_unreleased(base, added), ("- Zero.",))
        elsewhere = base.replace("- Old.\n", "- Old, reworded.\n")
        self.assertEqual(changelog.added_to_unreleased(base, elsewhere), ())


class ClientFixFragmentCase(unittest.TestCase):
    def ledger(self) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "client-fixes.d").mkdir()
        (root / "client-fixes.toml").write_text(
            'version = 1\nenforce_after = "abcdef1"\n' + ROW.format(id="old", sha="1111111")
        )
        return root / "client-fixes.toml"

    def test_ledger_rows_come_first_then_fragments_in_name_order(self):
        path = self.ledger()
        directory = path.with_suffix(".d")
        (directory / "b.toml").write_text(ROW.format(id="b", sha="3333333"))
        (directory / "a.toml").write_text(
            ROW.format(id="a", sha="2222222")
            + '[[fixes]]\nid = "a.extra"\ncommits = []\nsupplements = "old"\nsource = "a.kt"\n'
            'source_anchor = "x"\ntest = "a_test.kt"\ntest_anchor = "y"\n'
        )
        (directory / "README.md").write_text("# not a fragment\n")
        ledger = load_client_fixes(path)
        self.assertEqual(ledger.enforce_after, "abcdef1")
        self.assertEqual([fix.id for fix in ledger.fixes], ["old", "a", "a.extra", "b"])
        self.assertEqual(
            [fix.origin for fix in ledger.fixes],
            [None, "client-fixes.d/a.toml", "client-fixes.d/a.toml", "client-fixes.d/b.toml"],
        )

    def test_a_malformed_or_colliding_fragment_is_refused(self):
        cases = {
            "reused id": ROW.format(id="old", sha="2222222"),
            "ledger keys": "version = 1\n" + ROW.format(id="n", sha="2222222"),
            "no rows": "",
            "missing key": '[[fixes]]\nid = "n"\ncommits = ["2222222"]\n',
            "orphan supplement": ROW.format(id="n", sha="2222222").replace(
                'commits = ["2222222"]', 'commits = []\nsupplements = "absent"'
            ),
        }
        for name, text in cases.items():
            with self.subTest(fragment=name):
                path = self.ledger()
                (path.with_suffix(".d") / "new.toml").write_text(text)
                with self.assertRaises(HistoryError):
                    load_client_fixes(path)

    def test_fragments_without_a_ledger_are_refused(self):
        path = self.ledger()
        (path.with_suffix(".d") / "new.toml").write_text(ROW.format(id="n", sha="2222222"))
        path.unlink()
        with self.assertRaises(HistoryError):
            load_client_fixes(path)


if __name__ == "__main__":
    unittest.main()
