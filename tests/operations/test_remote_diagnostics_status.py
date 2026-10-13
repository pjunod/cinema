"""The generated diagnostics summary follows the canonical current state."""
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


class RemoteDiagnosticsStatusTest(unittest.TestCase):
    def test_current_summary_follows_ledger_state_and_next_action(self):
        repo = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "docs/features").mkdir(parents=True)
            shutil.copy2(repo / "scripts/remote-diagnostics-status", root / "scripts/remote-diagnostics-status")
            ledger = root / "docs/features/REMOTE-DIAGNOSTICS-STATUS.md"
            for state, next_action in [("building", "Complete implementation"),
                                       ("merged; qualification active", '<img src=x onerror="bad">')]:
                ledger.write_text("# Diagnostics status\n\n**Status:** " + state + "\n\n"
                                  "| Field | Current value |\n|---|---|\n"
                                  "| Active pull request | #1066 |\n"
                                  "| Implementation owner | qualification owner |\n"
                                  "| Implementation workspace/base | landed source |\n"
                                  "| Next action | " + next_action + " |\n")
                subprocess.run(["python3", str(root / "scripts/remote-diagnostics-status")], check=True, capture_output=True)
                page = (root / "diagnostics-status.html").read_text()
                banner = page.split('<p class="banner">', 1)[1].split("</p>", 1)[0]
                self.assertIn("Active qualification PR #1066", page)
                self.assertIn("/pulls/1066", page)
                self.assertNotIn("Active batched PR #1037", page)
                self.assertIn(state, banner)
                self.assertIn("qualification owner", banner)
                self.assertIn("landed source", banner)
                self.assertNotIn("Implementation continues", banner)
                self.assertNotIn("<img", banner)
                if state.startswith("merged"):
                    self.assertIn("&lt;img", banner)
                else:
                    self.assertIn(next_action, banner)
