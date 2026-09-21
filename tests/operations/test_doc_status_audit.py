"""The status audit rejects mechanically impossible lifecycle claims."""

from __future__ import annotations

from pathlib import Path
import tempfile
import unittest

from validation.doc_status_audit import audit


ROOT = Path(__file__).resolve().parents[2]


class DocStatusAuditCase(unittest.TestCase):
    def test_current_index_has_no_mechanical_contradictions(self) -> None:
        report = audit(ROOT)
        self.assertEqual([], report["contradictions"])
        self.assertEqual([], report["missing_headers"])

    def test_wrapped_terminal_status_cannot_be_ready_for_the_same_work(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            docs = root / "docs"
            docs.mkdir()
            (docs / "README.md").write_text(
                "| File | Answers | Status |\n"
                "|---|---|---|\n"
                "| [BUILT.md](BUILT.md) | Built case. | built |\n"
                "| [DONE.md](DONE.md) | Done case. | done |\n",
                encoding="utf-8",
            )
            (docs / "BUILT.md").write_text(
                "# Built\n\n**Status:** built — ready to\n"
                "build · **Written:** 2026-09-20\n",
                encoding="utf-8",
            )
            (docs / "DONE.md").write_text(
                "# Done\n\n**Status:** done — ready to run\n",
                encoding="utf-8",
            )

            report = audit(root)

        self.assertEqual(
            ["BUILT.md", "DONE.md"],
            [finding["document"] for finding in report["contradictions"]],
        )

    def test_landed_code_can_truthfully_leave_deployment_open(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            docs = root / "docs"
            docs.mkdir()
            (docs / "README.md").write_text(
                "| File | Answers | Status |\n"
                "|---|---|---|\n"
                "| [OPEN.md](OPEN.md) | Rollout case. | open |\n",
                encoding="utf-8",
            )
            (docs / "OPEN.md").write_text(
                "# Open\n\n**Status:** open — implementation complete; "
                "deployment pending\n",
                encoding="utf-8",
            )

            report = audit(root)

        self.assertEqual([], report["contradictions"])


if __name__ == "__main__":
    unittest.main()
