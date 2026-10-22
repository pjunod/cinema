"""Static contracts for bounded formal-toolchain wiring in existing gates."""

from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = (
    ".github/workflows/ci.yml",
    ".github/workflows/effort-ci.yml",
    ".github/workflows/main-fast-lane.yml",
)


def step_block(source: str, name: str) -> tuple[int, str]:
    """Return the named YAML step and its position, without parsing expressions."""
    lines = source.splitlines()
    for start, line in enumerate(lines):
        match = re.match(r"^(\s*)-\s+name:\s*(.*?)\s*$", line)
        if not match or match.group(2) != name:
            continue
        indent = len(match.group(1))
        end = len(lines)
        for index in range(start + 1, len(lines)):
            if len(lines[index]) - len(lines[index].lstrip()) == indent and re.match(r"^\s*-\s+", lines[index]):
                end = index
                break
        return start, "\n".join(lines[start:end])
    raise AssertionError(f"workflow has no step named {name!r}")


def gate_block(source: str, gate_name: str) -> str:
    """Find the top-level required gate job by its human-readable name."""
    lines = source.splitlines()
    jobs_at = next((i for i, line in enumerate(lines) if line == "jobs:"), None)
    if jobs_at is None:
        raise AssertionError("workflow has no jobs section")
    starts = [
        i for i in range(jobs_at + 1, len(lines))
        if re.match(r"^  [a-zA-Z0-9_-]+:\s*$", lines[i])
    ]
    starts.append(len(lines))
    for left, right in zip(starts, starts[1:]):
        block = "\n".join(lines[left:right])
        if re.search(r"^    name:\s*" + re.escape(gate_name) + r"\s*$", block, re.MULTILINE):
            return block
    raise AssertionError(f"workflow has no top-level {gate_name!r} job")


class FormalWorkflowContracts(unittest.TestCase):
    def read(self, relative: str) -> str:
        return (ROOT / relative).read_text(encoding="utf-8")

    def test_mapping_precedes_pinned_fetch_and_affected_smoke(self) -> None:
        for path in WORKFLOWS:
            with self.subTest(workflow=path):
                source = self.read(path)
                map_at, mapping = step_block(source, "Check formal protocol source mapping")
                fetch_at, fetch = step_block(source, "Provision the pinned formal toolchain")
                smoke_at, smoke = step_block(source, "Complete the bounded formal smoke configuration")
                progress_at, progress_smoke = step_block(source, "Complete operation-progress formal smoke configurations")

                self.assertLess(map_at, fetch_at)
                self.assertLess(fetch_at, smoke_at)
                self.assertLess(fetch_at, progress_at)
                self.assertIn("id: formal-inputs", mapping)
                self.assertIn("check-formal check-mapping --package durable-jobs", mapping)
                self.assertIn("check-formal check-mapping --package operation-progress", mapping)
                self.assertIn("github.event.pull_request.base.sha", mapping)
                self.assertIn("--affected-base", mapping)
                self.assertIn("scripts/bootstrap-formal-toolchain fetch", fetch)
                self.assertIn("--cache-dir target/formal-toolchain", fetch)
                self.assertIn("steps.formal-inputs.outputs.affected", fetch)
                self.assertIn("scripts/check-formal smoke", smoke)
                self.assertIn("steps.formal-inputs.outputs.affected_durable", smoke)
                self.assertIn("scripts/check-formal smoke --package durable-jobs", smoke)
                self.assertIn("steps.formal-inputs.outputs.affected_progress", progress_smoke)
                self.assertIn("scripts/check-formal smoke --package operation-progress", progress_smoke)
                if path.endswith("effort-ci.yml"):
                    self.assertNotIn("compile_only", mapping)
                    self.assertIn("!inputs.compile_only", fetch)
                    self.assertIn("!inputs.compile_only", smoke)
                    self.assertIn("!inputs.compile_only", progress_smoke)

    def test_artifact_upload_runs_after_failure_and_retains_receipts(self) -> None:
        for path in WORKFLOWS:
            with self.subTest(workflow=path):
                _, artifact = step_block(self.read(path), "Preserve formal model receipts and counterexamples")
                condition = re.search(r"^\s*if:\s*(.*)$", artifact, re.MULTILINE)
                self.assertIsNotNone(condition)
                self.assertIn("always()", condition.group(1))
                self.assertIn("steps.formal-inputs.outcome != 'skipped'", condition.group(1))
                self.assertIn("upload-artifact@", artifact)
                self.assertIn("target/verification/durable-jobs/", artifact)
                self.assertIn("target/verification/operation-progress/", artifact)
                self.assertIn("if-no-files-found: error", artifact)
                self.assertRegex(artifact, r"retention-days:\s*90\b")

    def test_final_gate_still_requires_preflight(self) -> None:
        expected = {
            ".github/workflows/ci.yml": "Main promotion gate",
            ".github/workflows/effort-ci.yml": "Effort development gate",
            ".github/workflows/main-fast-lane.yml": "Main promotion gate",
        }
        for path, name in expected.items():
            with self.subTest(workflow=path):
                gate = gate_block(self.read(path), name)
                needs = re.search(r"^    needs:\s*\n((?:^      .*\n|^    [^ ]|^$)*)", gate, re.MULTILINE)
                self.assertIsNotNone(needs)
                self.assertRegex(needs.group(1), r"(?m)^      - preflight\s*$")


if __name__ == "__main__":
    unittest.main()
