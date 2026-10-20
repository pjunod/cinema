from __future__ import annotations

import json
import errno
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

from validation import baseline_commands as commands


class BaselineCommandsCase(unittest.TestCase):
    identity = dict(candidate_sha="a" * 40, candidate_tree="b" * 40)

    def execute(self, root, cells, **overrides):
        manifest = root / "manifest.json"
        manifest.write_text(json.dumps({**self.identity, "cells": cells, **overrides}))
        with mock.patch.object(commands, "source_identity", return_value=self.identity):
            return commands.run_manifest(manifest, root / "results", root, grace=0.2)

    def cell(self, name, code, timeout=5):
        return dict(id=name, argv=[sys.executable, "-c", code], timeout_seconds=timeout)

    def test_failure_does_not_hide_independent_cell_or_original_returncode(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.execute(root, [self.cell("bad", "print('original failure');raise SystemExit(7)"),
                                         self.cell("after", "print('independent ran')")])
            self.assertFalse(result["success"])
            self.assertEqual([cell["returncode"] for cell in result["cells"]], [7, 0])
            self.assertIn("original failure", (root / "results/bad.log").read_text())
            self.assertIn("independent ran", (root / "results/after.log").read_text())
            self.assertEqual(result["source"], self.identity)
            self.assertTrue(result["manifest_sha256"])
            self.assertEqual((root / "manifest.json").read_bytes(), (root / "results/input-manifest.json").read_bytes())

    def test_invalid_missing_or_duplicate_identity_prevents_every_command(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            good = self.cell("one", "raise RuntimeError('must not run')")
            for cells in ([good, good], [{k:v for k,v in good.items() if k != "id"}], [],
                          [{**good, "cwd": "../"}], [{**good, "timeout_seconds": 0}]):
                with self.subTest(cells=cells):
                    result = self.execute(root, cells)
                    self.assertFalse(result["success"])
                    self.assertEqual(result["cells"], [])
                    self.assertTrue(result["errors"])

    def test_wrong_source_and_symlink_escape_refuse_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cell = self.cell("one", "raise RuntimeError('must not run')")
            result = self.execute(root, [cell], candidate_sha="c" * 40)
            self.assertFalse(result["success"])
            self.assertEqual(result["cells"], [])
            (root / "outside").symlink_to(root.parent, target_is_directory=True)
            result = self.execute(root, [{**cell, "cwd": "outside"}])
            self.assertFalse(result["success"])
            self.assertEqual(result["cells"], [])

    def test_timeout_kills_descendant_even_when_leader_exits_on_term(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child = "import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(30)"
            code = ("import subprocess,sys,time,pathlib;"
                    f"p=subprocess.Popen([sys.executable,'-c',{child!r}]);"
                    "pathlib.Path('child.pid').write_text(str(p.pid));time.sleep(30)")
            result = self.execute(root, [self.cell("timeout", code, timeout=0.4), self.cell("after", "print('after timeout')")])
            self.assertEqual(len(result["cells"]), 2, result.get("errors"))
            outcome = result["cells"][0]
            self.assertTrue(outcome["timed_out"])
            self.assertIn("SIGKILL", outcome["cleanup_signals"])
            self.assertFalse(result["success"])
            self.assertTrue(result["cells"][1]["success"])
            pid = int((root / "child.pid").read_text())
            # A killed orphan may briefly be a zombie; it cannot execute work.
            proc_stat = Path(f"/proc/{pid}/stat")
            if proc_stat.exists():
                self.assertEqual(proc_stat.read_text().split()[2], "Z")
            else:
                with self.assertRaises(ProcessLookupError):
                    os.kill(pid, 0)

    def test_normal_leader_exit_reaps_background_group_before_next_cell(self):
        for returncode in (0, 7):
            with self.subTest(returncode=returncode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                child = ("import pathlib,signal,time;"
                         "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
                         "pathlib.Path('child.ready').touch();time.sleep(30)")
                code = ("import subprocess,sys,time,pathlib;"
                        f"p=subprocess.Popen([sys.executable,'-c',{child!r}]);"
                        "pathlib.Path('child.pid').write_text(str(p.pid));"
                        "deadline=time.monotonic()+3;"
                        "\nwhile not pathlib.Path('child.ready').exists() and time.monotonic()<deadline: time.sleep(0.01)"
                        f"\nraise SystemExit({returncode})")
                try:
                    result = self.execute(root, [self.cell("leaked", code),
                                                 self.cell("after", "print('independent ran')")])
                    self.assertTrue(result["cells"], result.get("errors"))
                    outcome = result["cells"][0]
                    if (sys.platform == "darwin"
                            and outcome.get("cleanup_error_errno") in (errno.EPERM, errno.EACCES)):
                        self.assertEqual(outcome["returncode"], returncode)
                        self.assertFalse(outcome["success"])
                        self.assertFalse(result["success"])
                        self.assertEqual(result["blocked_cells"], ["after"])
                        self.assertEqual(len(result["cells"]), 1)
                        self.skipTest("macOS denied signalling the reparented fixture process group; "
                                      "real orphan-group cleanup capability is unavailable in this sandbox")
                    self.assertEqual(len(result["cells"]), 2, result.get("errors"))
                    self.assertEqual(outcome["returncode"], returncode)
                    self.assertFalse(outcome["timed_out"])
                    self.assertTrue(outcome["leaked_process_group"])
                    self.assertIn("cleanup_signals", outcome, outcome)
                    self.assertIn("SIGKILL", outcome["cleanup_signals"])
                    self.assertFalse(outcome["success"])
                    self.assertFalse(result["success"])
                    self.assertTrue(result["cells"][1]["success"])
                    pid = int((root / "child.pid").read_text())
                    state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                                           text=True, capture_output=True, check=False)
                    self.assertTrue(state.returncode != 0 or "Z" in state.stdout,
                                    f"child still executing after cleanup: {state.stdout}")
                finally:
                    pid_path = root / "child.pid"
                    if pid_path.exists():
                        try:
                            os.kill(int(pid_path.read_text()), signal.SIGKILL)
                        except ProcessLookupError:
                            pass

    def test_cleanup_refusal_preserves_original_cell_and_blocks_contaminated_followups(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with mock.patch.object(commands.os, "killpg", side_effect=PermissionError("group signal refused")):
                result = self.execute(root, [self.cell("unclean", "print('original exit zero')"),
                                             self.cell("after", "print('must not run')")])
            self.assertEqual(len(result["cells"]), 1, result.get("errors"))
            self.assertEqual(result["cells"][0]["returncode"], 0)
            self.assertFalse(result["cells"][0]["success"])
            self.assertIn("group signal refused", result["cells"][0]["cleanup_error"])
            self.assertEqual(result["blocked_cells"], ["after"])
            self.assertFalse((root / "results/after.log").exists())
            self.assertTrue(result["errors"])
            self.assertFalse(result["success"])

    def test_spawn_failure_is_preserved_and_remaining_commands_run(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            missing = dict(id="missing", argv=[str(root / "missing")], timeout_seconds=1)
            result = self.execute(root, [missing, self.cell("after", "print('ran')")])
            self.assertFalse(result["success"])
            self.assertIsNone(result["cells"][0]["returncode"])
            self.assertTrue(result["cells"][1]["success"])


if __name__ == "__main__":
    unittest.main()
