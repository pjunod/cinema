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
from validation import owner_counts  # noqa: E402
from validation.history import HistoryError, load_client_fixes  # noqa: E402
from validation.runner import CatalogError, load_catalog  # noqa: E402

BASE = os.environ.get("PLURX_VALIDATION_BASE")
LEDGER = "tests/client-fixes.toml"
OWNERS = "tests/playback/rolling-producer-owners.toml"
CATALOG = "validation/points.toml"

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

    def test_the_owner_ledger_totals_are_not_edited(self):
        """A count changes through a `delta` row, never through the total.

        Two branches that each raise the same `expected_occurrences` either
        conflict or, worse, both write the same next number and merge clean.
        """
        if not BASE:
            self.skipTest("no merge base given (PLURX_VALIDATION_BASE)")
        before = base_file(OWNERS)
        if before is None:
            self.skipTest(f"{OWNERS} is not on the merge base")

        listed = subprocess.run(
            ["git", "ls-tree", "--name-only", BASE, f"{owner_counts.DIRECTORY}/"], cwd=ROOT,
            text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        ).stdout.split()
        base_fragments = {
            Path(path).name: base_file(path) or "" for path in listed if path.endswith(".toml")
        }
        now_names = frozenset(owner_counts.read_fragments(ROOT / owner_counts.DIRECTORY))
        edited = owner_counts.direct_edits(
            before, base_fragments, (ROOT / OWNERS).read_text(encoding="utf-8"), now_names
        )
        self.assertEqual(
            list(edited),
            [],
            f"{OWNERS} totals move only when `scripts/release-cut` folds the recorded deltas in: "
            "record each difference as a delta row in tests/rolling-producer-owners.d/<slug>.toml "
            "(`scripts/migrate-bookkeeping` does it)",
        )

    def test_the_catalog_gains_no_paths_on_an_existing_point(self):
        if not BASE:
            self.skipTest("no merge base given (PLURX_VALIDATION_BASE)")
        before = base_file(CATALOG)
        if before is None:
            self.skipTest(f"{CATALOG} is not on the merge base")

        def paths(text: str) -> dict[str, list[str]]:
            return {point["id"]: list(point["paths"]) for point in tomllib.loads(text)["points"]}

        base, now = paths(before), paths((ROOT / CATALOG).read_text(encoding="utf-8"))
        # A rename or a narrowed glob changes a path in place and is allowed;
        # what is refused is the list growing, which is the append every pull
        # request used to make at the same list tail.
        grown = sorted(
            f"{identifier}: {len(base[identifier])} -> {len(now[identifier])} paths "
            f"(new: {', '.join(sorted(set(now[identifier]) - set(base[identifier])))})"
            for identifier in base.keys() & now.keys()
            if len(now[identifier]) > len(base[identifier])
        )
        self.assertEqual(
            grown,
            [],
            f"{CATALOG} is closed to additional paths on an existing point: put them in "
            "validation/points.d/<slug>.toml (`scripts/migrate-bookkeeping` does it)",
        )

    def test_the_vendored_patch_ledgers_gain_no_numbered_rows(self):
        """A new patch is a file under `patches.d/`, never "the next number"."""
        import re

        if not BASE:
            self.skipTest("no merge base given (PLURX_VALIDATION_BASE)")
        for crate in ("vendor/hiqlite", "vendor/hiqlite-wal"):
            ledger = f"{crate}/PLURX-PATCH.md"
            before = base_file(ledger)
            if before is None:
                continue
            rows = lambda text: len(re.findall(r"(?m)^\| \d+ \|", text))  # noqa: E731
            with self.subTest(ledger=ledger):
                self.assertLessEqual(
                    rows((ROOT / ledger).read_text(encoding="utf-8")),
                    rows(before),
                    f"{ledger}'s numbered table is closed: record the patch as "
                    f"{crate}/patches.d/<name>.toml (`scripts/migrate-bookkeeping` does it)",
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


class OwnerCountFoldCase(unittest.TestCase):
    LEDGER = (
        'version = 1\n\n[[symbols]]\nid = "flag"\npattern = "F"\nexpected_occurrences = 3\n\n'
        '[[module_symbols]]\nid = "sentinel"\npattern = "S"\nexpected_occurrences = 0\n\n'
        '[[module_structures]]\nid = "spawn"\n# Reviewed.\n\n# More review.\npattern = "spawn"\n'
        'expected_occurrences = 10\n'
    )
    ROW = '[[changes]]\nid = "{}"\ndelta = {}\nreason = "{}"\n'

    def test_the_fold_adds_every_delta_and_keeps_each_reason_on_its_row(self):
        fragments = {
            "b.toml": self.ROW.format("spawn", 1, "One joined fixture task."),
            "a.toml": self.ROW.format("spawn", 2, "Two awaited cache reads.") + self.ROW.format("flag", -1, "Flag removed."),
        }
        folded = owner_counts.fold(self.LEDGER, fragments)
        self.assertEqual(owner_counts.totals(folded), {"flag": 2, "sentinel": 0, "spawn": 13})
        self.assertIn(
            "# a (+2): Two awaited cache reads.\n# b (+1): One joined fixture task.\nexpected_occurrences = 13",
            folded,
        )
        self.assertIn("# a (-1): Flag removed.\nexpected_occurrences = 2", folded)
        # Folding is what the reviewed count already was, so the test that
        # counts the source passes before and after it.
        self.assertEqual(owner_counts.fold(folded, {}), folded)

    def test_a_total_moves_only_by_the_deltas_of_the_files_folded_with_it(self):
        fragments = {"a.toml": self.ROW.format("spawn", 2, "x"), "b.toml": self.ROW.format("spawn", 1, "y")}
        names = frozenset(fragments)
        # Untouched; a new fragment beside it; the release cut's fold.
        self.assertEqual(owner_counts.direct_edits(self.LEDGER, fragments, self.LEDGER, names), ())
        self.assertEqual(owner_counts.direct_edits(self.LEDGER, fragments, self.LEDGER, names | {"c.toml"}), ())
        self.assertEqual(
            owner_counts.direct_edits(self.LEDGER, fragments, owner_counts.fold(self.LEDGER, fragments), frozenset()), ()
        )
        # Folding one file of two moves the total by that file only.
        partial = owner_counts.fold(self.LEDGER, {"a.toml": fragments["a.toml"]})
        self.assertEqual(owner_counts.direct_edits(self.LEDGER, fragments, partial, frozenset({"b.toml"})), ())
        # A hand edit, a fold that keeps its files, and files deleted without a fold.
        edited = self.LEDGER.replace("expected_occurrences = 10", "expected_occurrences = 11")
        self.assertEqual(owner_counts.direct_edits(self.LEDGER, fragments, edited, names), ("spawn: 10 -> 11",))
        self.assertEqual(
            owner_counts.direct_edits(self.LEDGER, fragments, owner_counts.fold(self.LEDGER, fragments), names),
            ("spawn: 10 -> 13",),
        )
        self.assertEqual(
            owner_counts.direct_edits(self.LEDGER, fragments, self.LEDGER, frozenset()), ("spawn: 10 -> 10",)
        )


class CatalogPathFragmentCase(unittest.TestCase):
    CATALOG_TEXT = (
        'version = 1\n[settings]\nprofiles = ["commit"]\n'
        '[[points]]\nid = "one"\ntitle = "One"\ncontract = "c"\npaths = ["a/**"]\nchecks = []\n'
        '[[points]]\nid = "two"\ntitle = "Two"\ncontract = "c"\npaths = ["b/**"]\nchecks = []\n'
    )

    def catalog(self) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "points.toml").write_text(self.CATALOG_TEXT)
        (root / "points.d").mkdir()
        return root / "points.toml"

    def test_fragment_paths_join_their_point_in_name_order_without_repeats(self):
        path = self.catalog()
        directory = path.with_suffix(".d")
        (directory / "b.toml").write_text('[[paths]]\npoint = "one"\nadd = ["z/**", "a/**"]\n')
        (directory / "a.toml").write_text(
            '[[paths]]\npoint = "one"\nadd = ["y/**"]\n[[paths]]\npoint = "two"\nadd = ["x"]\n'
        )
        (directory / "README.md").write_text("# not a fragment\n")
        points = load_catalog(path).point_map
        self.assertEqual(points["one"].paths, ("a/**", "y/**", "z/**"))
        self.assertEqual(points["two"].paths, ("b/**", "x"))

    def test_a_catalog_without_fragments_is_unchanged(self):
        path = self.catalog()
        self.assertEqual(load_catalog(path).point_map["one"].paths, ("a/**",))

    def test_a_malformed_fragment_is_refused(self):
        cases = {
            "unknown point": '[[paths]]\npoint = "three"\nadd = ["x"]\n',
            "empty add": '[[paths]]\npoint = "one"\nadd = []\n',
            "extra key": '[[paths]]\npoint = "one"\nadd = ["x"]\nchecks = []\n',
            "other table": 'version = 1\n[[paths]]\npoint = "one"\nadd = ["x"]\n',
            "no rows": "",
        }
        for name, text in cases.items():
            with self.subTest(fragment=name):
                path = self.catalog()
                (path.with_suffix(".d") / "new.toml").write_text(text)
                with self.assertRaises(CatalogError):
                    load_catalog(path)


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
