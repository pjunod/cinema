"""`STATUS.md` stays a short page, and the status guard follows its prose.

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

from test_status_pr_claims import STATUS_PAGES

ROOT = Path(__file__).resolve().parents[2]
STATUS = ROOT / "STATUS.md"
MAX_LINES = 400


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
    def test_status_md_stays_under_four_hundred_lines(self):
        lines = STATUS.read_text(encoding="utf-8").count("\n")
        self.assertLess(
            lines,
            MAX_LINES,
            "STATUS.md has outgrown its page: move the oldest sections into "
            "their folder's STATUS-HISTORY.md and leave an index row",
        )

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
