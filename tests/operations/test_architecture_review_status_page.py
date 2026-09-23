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
OVERLAY_TEST = ROOT / "tests/operations/architecture_review_status_overlay.test.js"
C01_PLAN = ROOT / "docs/server/HTTP-LISTENER-TIMEOUTS-AND-ASSET-DELIVERY.md"

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
        ):
            self.assertNotIn(superseded, board)

    def test_c01_records_the_merge_without_claiming_post_merge_evidence(self) -> None:
        row = next(line for line in board_rows() if line.startswith("| C-01 |"))
        self.assertIn("| merged: M1-M3 |", row)
        self.assertIn("[PR #395]", row)
        self.assertIn("`79113254`", row)
        self.assertIn("§6.2–§6.4 lab soak, HAR/waterfall", row)
        self.assertIn("device-reader evidence remains pending", row)
        self.assertIn("not done", row)

        plan = C01_PLAN.read_text(encoding="utf-8")
        self.assertIn(
            "**Status:** implementation merged; post-merge evidence pending",
            plan,
        )

    def test_page_fetches_the_canonical_board_without_caching_it(self) -> None:
        page = PAGE.read_text(encoding="utf-8")
        self.assertIn(
            'const WORKBOARD = "ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md";',
            page,
        )
        self.assertRegex(
            page,
            r"fetch\(WORKBOARD, \{ cache: \"no-store\", signal \}\)",
        )
        self.assertIn("if (!refreshFence.hasActive()) loadBoard();", page)
        self.assertIn("const refresh = refreshFence.begin();", page)
        self.assertIn(PARSER.name, page)
        self.assertIn("globalThis.ArchitectureReviewStatusParser", page)

    def test_forgejo_overlay_is_bounded_and_fails_open_to_board_rows(self) -> None:
        page = PAGE.read_text(encoding="utf-8")
        parser = PARSER.read_text(encoding="utf-8")

        self.assertIn('const PULLS_API = "/api/v1/repos/noirr/plurx/pulls";', page)
        self.assertIn('credentials: "same-origin"', page)
        self.assertIn("runWithDeadline(async (signal)", page)
        self.assertIn("slice(0, MAX_STATUS_FETCHES)", page)
        self.assertIn("STATUS_CONCURRENCY", page)
        self.assertIn("state.overlay = overlayFallback(previous, error.message)", page)
        self.assertIn('mode: snapshot.complete ? "available" : "truncated"', page)
        self.assertIn("complete: snapshot.complete", page)
        self.assertIn("overlayAbsenceText(state.overlay)", page)
        self.assertIn("effectiveStatus(row.Status, pulls, state.overlay).group", page)
        self.assertIn("metric(labels.livePlans, livePlans)", page)
        self.assertGreaterEqual(page.count("renderSummary();"), 3)
        self.assertIn("the canonical board remains complete", page)
        load_board = page[page.index("async function loadBoard()") :]
        self.assertLess(
            load_board.index("renderTable();"),
            load_board.index("await loadOverlay(refresh, previousOverlay);"),
        )
        self.assertIn("const MAX_PULL_PAGES = 2;", parser)
        self.assertIn("const MAX_STATUS_FETCHES = 12;", parser)
        self.assertIn("const REQUEST_TIMEOUT_MS = 10_000;", parser)

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
        # 47 board rows: the 46 the review opened with, plus A-05, the build
        # plan A-04 produced as its D4. A count that moves without a row
        # being added to the board is the parser having stopped reading it.
        self.assertEqual(47, observed["rowCount"])
        self.assertEqual([10], observed["cellCounts"])
        # A distinctive phrase from the S-04 row's live Notes cell: this
        # pins that the parser reads a real row's last column, and it moves
        # when that row does. Escaped-pipe parsing has its own coverage in
        # `test_overlay_javascript_contract_executes`.
        self.assertIn(
            "folds the encoder executable into the attestation batch",
            observed["s04Notes"],
        )
        self.assertTrue(observed["schemaMismatchFailedClosed"])

    def test_overlay_javascript_contract_executes(self) -> None:
        result = subprocess.run(
            ["node", str(OVERLAY_TEST), str(PARSER), str(BOARD)],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        )
        observed = json.loads(result.stdout)
        self.assertEqual(["P-01", "C-02", "S-01"], observed["currentExamples"])
        self.assertEqual("C-01", observed["customBranchFallback"])
        self.assertTrue(observed["escaped"])
        self.assertEqual(["unavailable", "stale"], observed["fallbackModes"])
        self.assertEqual([False, True], observed["snapshotCompleteness"])
        self.assertTrue(observed["deadlineAborted"])
        self.assertTrue(observed["newestGenerationWon"])

    def test_embedded_page_script_compiles(self) -> None:
        page = PAGE.read_text(encoding="utf-8")
        scripts = [
            script
            for script in re.findall(
                r"<script(?:\s+[^>]*)?>(.*?)</script>", page, re.DOTALL
            )
            if script.strip()
        ]
        self.assertEqual(1, len(scripts), "only the page controller is inline")
        subprocess.run(
            [
                "node",
                "-e",
                'new Function(require("fs").readFileSync(0, "utf8"));',
            ],
            cwd=ROOT,
            input=scripts[0],
            text=True,
            check=True,
        )


if __name__ == "__main__":
    unittest.main()
