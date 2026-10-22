"""Contracts for production worker controller proof selection and promotion."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ".github/workflows/main-fast-lane.yml"


def job_block(source: str, name: str) -> str:
    start = source.index(f"\n  {name}:\n") + 1
    following = re.search(r"\n  [a-zA-Z0-9_-]+:\n", source[start:])
    end = start + following.start() if following else len(source)
    return source[start:end]


def step_block(source: str, name: str) -> str:
    start = source.index(f"      - name: {name}\n")
    following = re.search(r"\n      - name:", source[start + 1 :])
    end = start + 1 + following.start() if following else len(source)
    return source[start:end]


class WorkerControlCiTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = (ROOT / WORKFLOW).read_text(encoding="utf-8")
        self.proof = job_block(self.workflow, "worker_control_proof")
        self.gate = job_block(self.workflow, "promotion_gate")

    def test_proof_job_selects_only_relevant_source_changes(self) -> None:
        self.assertIn("needs: scope", self.proof)
        self.assertIn("if: github.event.pull_request.draft == false", self.proof)
        selection = step_block(self.proof, "Select affected worker controller proof inputs")
        self.assertIn('git diff --name-only "$PR_BASE_SHA...$PR_HEAD_SHA"', selection)
        for path in (
            "crates/plurx-worker-control/*",
            "crates/plurxd/src/library_work.rs",
            "crates/plurxd/Cargo.toml",
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "scripts/check-worker-control",
            "verification/worker-control/*",
            ".github/workflows/main-fast-lane.yml",
        ):
            with self.subTest(path=path):
                self.assertIn(path, selection)

    def test_selected_inputs_run_pinned_full_proof_and_preserve_attempt(self) -> None:
        python = step_block(self.proof, "Install the pinned Python 3.12 runtime")
        self.assertIn("steps.worker-control-inputs.outputs.affected == 'true'", python)
        self.assertIn("target/worker-control-python-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT", python)
        self.assertIn("731af898886c5f821890dc901eca3c651cca8e51fa7308c159d12a1194aeac91", python)
        self.assertIn("sys.version_info[:3] == (3, 12, 15)", python)

        run = step_block(self.proof, "Run the pinned production worker controller proof")
        self.assertIn("steps.worker-control-inputs.outputs.affected == 'true'", run)
        self.assertIn("scripts/check-worker-control full", run)
        self.assertIn("--cache-dir target/worker-control-tools", run)
        self.assertIn("target/verification/worker-control-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT", run)
        self.assertIn("--provision", run)
        self.assertNotIn("continue-on-error", run)

        artifact = step_block(self.proof, "Preserve worker controller proof receipts and logs")
        self.assertIn("always()", artifact)
        self.assertIn("steps.worker-control-inputs.outputs.affected == 'true'", artifact)
        self.assertIn("target/verification/worker-control-${{ github.run_id }}-${{ github.run_attempt }}/", artifact)
        self.assertIn("if-no-files-found: error", artifact)
        self.assertRegex(artifact, r"retention-days:\s*90\b")

    def test_promotion_gate_requires_worker_proof_success(self) -> None:
        self.assertIn("- worker_control_proof", self.gate)
        self.assertIn("WORKER_CONTROL_RESULT: ${{ needs.worker_control_proof.result }}", self.gate)
        self.assertIn('if [ "$WORKER_CONTROL_RESULT" != success ]; then', self.gate)
        self.assertIn('echo "The required production worker controller proof ended with:', self.gate)


if __name__ == "__main__":
    unittest.main()
