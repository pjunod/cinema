"""The requested lane (docs/ci/REQUESTED-LANE.md, validation/lane_request.py)."""

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

from validation.lane_request import LABEL_ACTIONS, requested, scope  # noqa: E402

WORKFLOW = (ROOT / ".github/workflows/main-fast-lane.yml").read_text()
SCOPE_LINES = ["rust=true", "apple=false", "web_layout=true", "docs_only=false"]
HEAVY = ("preflight", "worker_control_proof", "rust_units", "apple_compile")
CHEAP = ("rust_compile", "web_compile", "android_compile")


def event(action="label_updated", labels=("lane",), draft=False, body=""):
    return {"action": action, "number": 7,
            "pull_request": {"number": 7, "draft": draft, "body": body,
                             "labels": [{"id": index, "name": name} for index, name in enumerate(labels)]}}


def job_block(name: str) -> str:
    match = re.search(rf"(?ms)^  {re.escape(name)}:\n.*?(?=^  [a-z_]+:\n|\Z)", WORKFLOW)
    assert match, name
    return match.group(0)


def condition(job: str) -> str:
    block = job_block(job)
    match = re.search(r"(?m)^    if: >-\n((?:      .*\n)+)", block) or re.search(r"(?m)^    if: (.+)\n", block)
    return " ".join(match.group(1).split())


class RequestedCase(unittest.TestCase):
    def test_a_lane_label_event_on_a_ready_pr_is_the_request(self):
        # Forgejo's payload action is label_updated; labeled is the GitHub spelling.
        for action in ("label_updated", "labeled"):
            self.assertTrue(requested(event(action)), action)
            self.assertTrue(requested(event(action, labels=("hold", "lane"))), action)

    def test_pushes_drafts_and_other_labels_request_nothing(self):
        for case in (event("synchronized"), event("synchronize"), event("opened"),
                     event("ready_for_review"), event("reopened"),
                     event(labels=()), event(labels=("hold",)), event(labels=("Lane",)),
                     event(draft=True), event("label_cleared", labels=()), event("unlabeled"),
                     {"action": "label_updated", "pull_request": None}, {}, None):
            self.assertFalse(requested(case), case)

    def test_a_payload_without_draft_is_not_ready(self):
        case = event()
        del case["pull_request"]["draft"]
        self.assertFalse(requested(case))

    def test_scope_passes_surfaces_through_and_adds_the_request(self):
        out = dict(line.split("=", 1) for line in scope(SCOPE_LINES, event()))
        self.assertEqual(out["lane_requested"], "true")
        self.assertEqual((out["rust"], out["apple"], out["web_layout"]), ("true", "false", "true"))
        for output in ("preflight", "worker_control", "rust_units"):
            self.assertEqual(out[output], "true")
        cheap = dict(line.split("=", 1) for line in scope(SCOPE_LINES, event("synchronized")))
        self.assertEqual(cheap["lane_requested"], "false")
        self.assertEqual({k: v for k, v in cheap.items() if k != "lane_requested"},
                         {k: v for k, v in out.items() if k != "lane_requested"})

    def test_a_requested_run_always_selects_preflight_and_the_worker_proof(self):
        # The gate accepts skipped jobs; a scope that switched preflight off would
        # otherwise skip every job behind it and still go green.
        lines = SCOPE_LINES + ["preflight=false", "worker_control=false", "rust_units=false"]
        out = dict(line.split("=", 1) for line in scope(lines, event()))
        self.assertEqual((out["preflight"], out["worker_control"], out["rust_units"]), ("true", "true", "true"))

    def test_the_gate_requires_the_requested_preflight_to_pass(self):
        self.assertIn('if [ "$PREFLIGHT_RESULT" != success ]', job_block("promotion_gate"))

    def test_the_command_writes_github_output_lines(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "event.json"
            path.write_text(json.dumps(event()))
            result = subprocess.run([sys.executable, "-m", "validation.lane_request", "scope"], cwd=ROOT,
                                    input="\n".join(SCOPE_LINES), text=True, capture_output=True, check=True,
                                    env={**os.environ, "GITHUB_EVENT_PATH": str(path)})
        self.assertIn("lane_requested=true", result.stdout.splitlines())
        self.assertIn("rust=true", result.stdout.splitlines())


class WorkflowCase(unittest.TestCase):
    def test_label_events_start_the_workflow(self):
        types = re.search(r"(?m)^    types: \[(.*)\]$", WORKFLOW).group(1).split(", ")
        self.assertIn("labeled", types)
        for kept in ("opened", "synchronize", "reopened", "ready_for_review"):
            self.assertIn(kept, types)
        self.assertTrue(LABEL_ACTIONS >= {"labeled", "label_updated"})

    def test_the_scope_step_decides_the_request(self):
        block = job_block("scope")
        self.assertIn("python3 -m validation.lane_request scope", block)
        self.assertIn("set -euo pipefail", block)
        self.assertIn("lane_requested: ${{ steps.scope.outputs.lane_requested }}", block)

    def test_a_label_event_without_lane_costs_nothing(self):
        guard = condition("paperwork")
        self.assertIn("github.event.action != 'labeled' && github.event.action != 'label_updated'", guard)
        self.assertIn("contains(github.event.pull_request.labels.*.name, 'lane')", guard)
        # Every other job sits behind paperwork and scope.
        self.assertIn("needs: paperwork", job_block("scope"))

    def test_heavy_jobs_run_only_on_the_request(self):
        for job in HEAVY:
            with self.subTest(job=job):
                self.assertIn("needs.scope.outputs.lane_requested == 'true'", condition(job))
        self.assertIn("needs.scope.outputs.preflight == 'true'", condition("preflight"))
        self.assertIn("needs.scope.outputs.worker_control == 'true'", condition("worker_control_proof"))
        self.assertIn("needs.scope.outputs.apple == 'true'", condition("apple_compile"))
        for job in ("rust_units", "apple_compile"):
            self.assertIn("needs.preflight.result == 'success'", condition(job))
        suite = re.search(r"(?ms)- name: Run every Node web and playback test\n(.*?)\n      - ", job_block("web_compile"))
        self.assertIn("if: needs.scope.outputs.lane_requested == 'true'", suite.group(1))

    def test_cheap_jobs_run_on_every_ready_push_and_wait_for_a_requested_preflight(self):
        for job, output in zip(CHEAP, ("rust", "web_layout", "android_jvm")):
            with self.subTest(job=job):
                text = condition(job)
                self.assertIn("!cancelled()", text)
                self.assertIn("needs.scope.result == 'success'", text)
                self.assertIn(f"needs.scope.outputs.{output} == 'true'", text)
                self.assertIn("needs.preflight.result == 'success' || "
                              "(needs.scope.outputs.lane_requested != 'true' && needs.preflight.result == 'skipped')", text)

    def test_the_promotion_gate_is_the_requested_run_alone(self):
        self.assertIn("needs.scope.outputs.lane_requested == 'true'", condition("promotion_gate"))
        self.assertIn("always()", condition("promotion_gate"))

    def test_the_merge_train_is_gone(self):
        for text in ("MERGE_TRAIN", "merge_train", "train_selected", "run_lane", "train_tests"):
            self.assertNotIn(text, WORKFLOW)
        self.assertNotIn("inputs.tests", (ROOT / ".github/workflows/rust-units.yml").read_text())

    def test_one_run_per_pr_and_a_push_cancels_it(self):
        self.assertIn("group: main-fast-lane-${{ github.event.pull_request.number }}", WORKFLOW)
        self.assertIn("cancel-in-progress: true", WORKFLOW)

    def test_a_pr_head_is_never_fixed_in_place(self):
        paperwork = job_block("paperwork")
        # PR heads stay immutable in Actions; publication is external.
        self.assertNotIn("scripts/paperwork-fix", paperwork)
        self.assertNotRegex(paperwork, r"(?m)^\s*git[^\n]*\b(?:push|commit)\b")
        self.assertNotIn("secrets.PAPERWORK_PUSH_TOKEN", paperwork)
        self.assertIn("publication is external", paperwork)


if __name__ == "__main__":
    unittest.main()
