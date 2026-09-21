"""The architecture-review status page stays a view, never a second ledger."""

from __future__ import annotations

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
PAGE = ROOT / "docs/reviews/ARCHITECTURE-REVIEW-2026-09-20-STATUS.html"
BOARD = ROOT / "docs/reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md"

EXPECTED_COLUMNS = (
    "Id",
    "Plan",
    "Executes",
    "Priority",
    "Status",
    "Model",
    "Session",
    "Branch / PR",
    "Last update",
    "Notes",
)


def board_rows() -> list[str]:
    lines = BOARD.read_text(encoding="utf-8").splitlines()
    heading = lines.index("## The board")
    header = next(
        index
        for index, line in enumerate(lines[heading + 1 :], start=heading + 1)
        if line.startswith("| Id | Plan |")
    )
    rows = []
    for line in lines[header + 2 :]:
        if not line.startswith("|"):
            break
        rows.append(line)
    return rows


class ArchitectureReviewStatusPageCase(unittest.TestCase):
    def test_page_fetches_the_canonical_board_without_caching_it(self) -> None:
        page = PAGE.read_text(encoding="utf-8")
        self.assertIn(
            'const WORKBOARD = "ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md";',
            page,
        )
        self.assertRegex(page, r"fetch\(WORKBOARD, \{ cache: \"no-store\" \}\)")
        self.assertIn("window.setInterval(loadBoard, REFRESH_MS)", page)

    def test_page_does_not_copy_plan_rows(self) -> None:
        page = PAGE.read_text(encoding="utf-8")
        rows = board_rows()
        self.assertGreater(len(rows), 30, "the canonical board should contain the effort")
        ids = [re.match(r"\| ([A-Z]-\d{2}) \|", row).group(1) for row in rows]
        copied = [board_id for board_id in ids if board_id in page]
        self.assertEqual([], copied, "plan ids belong only in the canonical workboard")

    def test_page_parser_pins_the_board_schema(self) -> None:
        board = BOARD.read_text(encoding="utf-8")
        header = "| " + " | ".join(EXPECTED_COLUMNS) + " |"
        self.assertIn(header, board)

        page = PAGE.read_text(encoding="utf-8")
        for column in EXPECTED_COLUMNS:
            self.assertIn(f'"{column}"', page)
        self.assertIn("The workboard columns changed", page)


if __name__ == "__main__":
    unittest.main()
