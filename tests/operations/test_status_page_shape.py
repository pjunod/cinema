"""`STATUS.md` stays a short fixed page, efforts are one file each under
`status.d/`, and the status guard follows the prose wherever it lives.

docs/ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md §3.4 (M6): older sections
move verbatim into their subject folder's status history, and `STATUS.md`
keeps one row per moved section. A move is only safe if the guard in
`test_status_pr_claims.py` reads the documents the prose moved into —
otherwise the check's reach shrinks silently while the diff looks like a
tidy-up.
"""

from __future__ import annotations

from pathlib import Path
import re
import unittest

import sys

from test_status_pr_claims import STATUS_PAGES, status_pages

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from validation import status_entries  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
STATUS = ROOT / "STATUS.md"
# The page is a fixed header plus one index row per section moved out before
# 2026-11-15. It was capped at 180 lines while every effort was appended to
# it; efforts are files under status.d/ now, each with its own limit.
MAX_LINES = 180
# The only sections the page has. A pull request that adds another is putting
# an effort where every other pull request would have to edit around it.
SECTIONS = (
    "## Add or update an effort",
    "## Older efforts — where each one now lives",
)


def linked_documents(markdown: str) -> set[str]:
    """Repository-relative Markdown and HTML documents a page links to."""

    targets = set()
    for target in re.findall(r"\]\(([^)\s#]+)(?:#[^)\s]*)?\)", markdown):
        if re.match(r"^[a-z][a-z0-9+.-]*:", target):
            continue
        if target.endswith((".md", ".html")):
            targets.add(target)
    return targets


class StatusPageShapeCase(unittest.TestCase):
    def test_status_md_stays_short(self):
        lines = STATUS.read_text(encoding="utf-8").count("\n")
        self.assertLess(
            lines,
            MAX_LINES,
            "STATUS.md has outgrown its page: move the oldest sections into "
            "their folder's STATUS-HISTORY.md and leave an index row",
        )

    def test_status_md_holds_no_effort_entries(self):
        lines = STATUS.read_text(encoding="utf-8").splitlines()
        self.assertEqual(
            tuple(line for line in lines if line.startswith("## ")),
            SECTIONS,
            "STATUS.md is a fixed page: an effort is a file under status.d/ "
            "(status.d/README.md), not a section here",
        )
        dated = [line[:60] for line in lines if re.match(r"- \*\*\d{4}-\d{2}-\d{2}", line)]
        self.assertEqual(
            dated, [], "these efforts belong in status.d/<date>-<slug>.md, one file each"
        )

    def test_every_effort_file_is_a_short_titled_entry(self):
        self.assertEqual(list(status_entries.validate(ROOT)), [])

    def test_the_guard_reads_every_effort_file(self):
        entries = {f"status.d/{path.name}" for path in status_entries.entry_paths(ROOT)}
        self.assertTrue(entries <= set(status_pages()))

    def test_a_malformed_effort_file_is_refused(self):
        import tempfile

        cases = {
            "2026-11-15-ok.md": ("# Title\n\nOwner Claude; built.\n", 0),
            "notdated.md": ("# Title\n\nOwner.\n", 1),
            "2026-11-15-no-title.md": ("Owner Claude; built.\n", 1),
            "2026-11-15-empty.md": ("# Title\n", 1),
            "2026-11-15-two.md": ("# One\n\ntext\n\n## Two\n\ntext\n", 1),
            "2026-11-15-long.md": ("# Title\n\n" + "line\n" * 40, 1),
            "2026-11-15-big.md": ("# Title\n\n" + "x" * 3000 + "\n", 1),
        }
        for name, (text, expected) in cases.items():
            with self.subTest(entry=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / "status.d").mkdir()
                (root / "status.d" / "README.md").write_text("# not an entry\n")
                (root / "status.d" / name).write_text(text)
                self.assertEqual(bool(status_entries.validate(root)), bool(expected))

    def test_every_document_the_index_rows_link_to_is_a_guarded_status_page(self):
        index = STATUS.read_text(encoding="utf-8").split(
            "## Older efforts — where each one now lives", 1
        )
        self.assertEqual(len(index), 2, "STATUS.md has no index of moved sections")
        linked = linked_documents(index[1])
        self.assertTrue(linked, "the index of moved sections links to nothing")
        self.assertEqual(sorted(linked - set(STATUS_PAGES)), [])
        for target in linked:
            self.assertTrue((ROOT / target).is_file(), target)


if __name__ == "__main__":
    unittest.main()
