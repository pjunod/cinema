"""The architecture-review status page stays a view, never a second ledger."""

from __future__ import annotations

import json
from pathlib import Path
import re
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
PAGE = ROOT / "docs/reviews/ARCHITECTURE-REVIEW-2026-09-20-STATUS.html"
PARSER = ROOT / "docs/reviews/ARCHITECTURE-REVIEW-2026-09-20-STATUS.js"
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
    def test_workboard_requires_one_implementation_pr_per_plan(self) -> None:
        board = BOARD.read_text(encoding="utf-8")
        self.assertIn("One implementation PR owns the whole plan", board)
        self.assertIn("one evidence-only docs PR", board)
        self.assertIn("Milestones are logical commits", board)
        for superseded in (
            "One PR per milestone",
            "Each milestone PR",
            "milestone PRs open",
            "merged: <milestones>",
        ):
            self.assertNotIn(superseded, board)

    def test_page_fetches_the_canonical_board_without_caching_it(self) -> None:
        page = PAGE.read_text(encoding="utf-8")
        self.assertIn(
            'const WORKBOARD = "ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md";',
            page,
        )
        self.assertRegex(page, r"fetch\(WORKBOARD, \{ cache: \"no-store\" \}\)")
        self.assertIn("window.setInterval(loadBoard, REFRESH_MS)", page)
        self.assertIn(PARSER.name, page)
        self.assertIn("globalThis.ArchitectureReviewStatusParser", page)

    def test_page_does_not_copy_plan_rows(self) -> None:
        page = PAGE.read_text(encoding="utf-8")
        rows = board_rows()
        self.assertGreater(len(rows), 30, "the canonical board should contain the effort")
        ids = [re.match(r"\| ([A-Z]-\d{2}) \|", row).group(1) for row in rows]
        copied = [board_id for board_id in ids if board_id in page]
        self.assertEqual([], copied, "plan ids belong only in the canonical workboard")

    def test_exact_page_parser_executes_against_the_canonical_board(self) -> None:
        board = BOARD.read_text(encoding="utf-8")
        header = "| " + " | ".join(EXPECTED_COLUMNS) + " |"
        self.assertIn(header, board)

        script = r"""
const fs = require("fs");
const parser = require(process.argv[1]);
const board = fs.readFileSync(process.argv[2], "utf8");
const rows = parser.parseBoard(board);
const s04 = rows.find((row) => row.Id === "S-04");
const changedHeader = board.replace(
  "| Id | Plan | Executes | Priority | Status | Model | Session | Branch / PR | Last update | Notes |",
  "| Id | Plan | Executes | Priority | Status | Model | Session | Branch / PR | Last update | Remark |",
);
let schemaMismatchFailedClosed = false;
try {
  parser.parseBoard(changedHeader);
} catch (error) {
  schemaMismatchFailedClosed = error.message.includes("columns changed");
}
console.log(JSON.stringify({
  rowCount: rows.length,
  cellCounts: [...new Set(rows.map((row) => Object.keys(row).length))],
  s04Notes: s04 && s04.Notes,
  schemaMismatchFailedClosed,
}));
"""
        result = subprocess.run(
            ["node", "-e", script, str(PARSER), str(BOARD)],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        )
        observed = json.loads(result.stdout)
        self.assertEqual(46, observed["rowCount"])
        self.assertEqual([10], observed["cellCounts"])
        self.assertIn("ldd | grep fontconfig", observed["s04Notes"])
        self.assertTrue(observed["schemaMismatchFailedClosed"])


if __name__ == "__main__":
    unittest.main()
