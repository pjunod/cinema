"""The fast lane's merge-train rules (docs/ci/MERGE-TRAIN.md, validation/merge_train.py)."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from validation.merge_train import scope, selection  # noqa: E402

LANE = ROOT / ".github/workflows/main-fast-lane.yml"
SCOPE_LINES = ["rust=false", "apple=false", "desktop_macos=false", "android_jvm=false", "web_layout=true",
               "mobile_version=false", "docs_only=false"]


def event(head="train/3-b2", author="train-bot", body="Train-Jobs: preflight,rust_compile\n"):
    return {"pull_request": {"head": {"ref": head}, "user": {"login": author}, "body": body}}


def job_block(name: str) -> str:
    text = LANE.read_text()
    match = re.search(r"(?ms)^  " + re.escape(name) + r":\n.*?(?=^  [A-Za-z0-9_]+:\n|\Z)", text)
    assert match, name
    return match.group()


class SelectionCase(unittest.TestCase):
    def test_a_probe_runs_exactly_its_jobs_forced_on(self):
        out = dict(line.split("=") for line in scope(SCOPE_LINES, selection(event(), "train-bot")))
        self.assertEqual(out["rust"], "true")       # forced on although the diff did not select it
        self.assertEqual(out["preflight"], "true")
        self.assertEqual(out["web_layout"], "false")  # selected by the diff, not by the probe
        self.assertEqual(out["worker_control"], "false")
        self.assertEqual(out["train_selected"], "true")
        self.assertEqual(out["mobile_version"], "false")

    def test_only_the_bots_train_heads_can_select(self):
        for case in (event(head="feature/x"), event(author="someone"), event(body="no line"),
                     {"pull_request": None}, {}):
            self.assertIsNone(selection(case, "train-bot"), case)
        self.assertEqual(selection(event(author="robot"), "robot"), ["preflight", "rust_compile"])

    def test_outside_a_probe_the_scope_passes_through(self):
        out = scope(SCOPE_LINES, None)
        self.assertEqual(out[: len(SCOPE_LINES)], SCOPE_LINES)
        self.assertEqual(out[len(SCOPE_LINES):], ["preflight=true", "worker_control=true", "train_selected=false"])

    def test_an_unknown_job_fails_the_scope(self):
        with self.assertRaises(SystemExit):
            scope(SCOPE_LINES, ["promotion_gate"])

    def test_command_reads_the_event_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "event.json"
            path.write_text(json.dumps(event(body="Train-Jobs: android_compile\n")))
            result = subprocess.run([sys.executable, "-m", "validation.merge_train", "scope"], cwd=ROOT,
                                    input="\n".join(SCOPE_LINES), capture_output=True, text=True,
                                    env=dict(os.environ, GITHUB_EVENT_PATH=str(path)))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("android_jvm=true", result.stdout.split())
            self.assertIn("preflight=false", result.stdout.split())


class LaneCase(unittest.TestCase):
    TRAIN = ("vars.MERGE_TRAIN != 'on' || (startsWith(github.event.pull_request.head.ref, 'train/') && "
             "github.event.pull_request.user.login == (vars.MERGE_TRAIN_BOT || 'train-bot'))")

    def condition(self, job: str) -> str:
        block = job_block(job)
        match = re.search(r"(?m)^    if: >-\n((?:      .*\n)+)", block) or re.search(r"(?m)^    if: (.+)\n", block)
        return " ".join(match.group(1).split())

    def test_with_the_train_on_only_train_prs_run_past_paperwork(self):
        self.assertIn(self.TRAIN, self.condition("scope"))
        self.assertIn(self.TRAIN, self.condition("promotion_gate"))
        self.assertNotIn("if:", job_block("paperwork").split("steps:")[0])  # paperwork runs on every PR

    def test_probes_select_jobs_through_the_scope_outputs(self):
        self.assertIn("python3 -m validation.merge_train scope", job_block("scope"))
        self.assertIn("set -euo pipefail", job_block("scope"))
        self.assertIn("needs.scope.outputs.preflight == 'true'", self.condition("preflight"))
        self.assertIn("needs.scope.outputs.worker_control == 'true'", self.condition("worker_control_proof"))
        self.assertIn("needs.scope.outputs.train_selected != 'true'", self.condition("promotion_gate"))
        for job, output in (("rust_compile", "rust"), ("web_compile", "web_layout"),
                            ("android_compile", "android_jvm")):
            self.assertIn(f"needs.scope.outputs.{output} == 'true'", self.condition(job))

    def test_a_train_head_is_never_fixed_in_place(self):
        paperwork = job_block("paperwork")
        guard = paperwork.index('case "$HEAD_REF" in train/*) exit 1 ;; esac')
        self.assertLess(guard, paperwork.index("scripts/paperwork-fix"))


if __name__ == "__main__":
    unittest.main()
