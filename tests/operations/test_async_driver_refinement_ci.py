"""Static contracts for async driver refinement in the Rust CI lanes."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]


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


class AsyncDriverRefinementCiTests(unittest.TestCase):
    def setUp(self) -> None:
        self.main = job_block((ROOT / ".github/workflows/main-fast-lane.yml").read_text(), "rust_compile")
        self.effort = job_block((ROOT / ".github/workflows/effort-ci.yml").read_text(), "rust_compile")

    def assert_qualification_sequence(self, block: str, *, effort: bool) -> None:
        selection = step_block(block, "Select affected async driver refinement inputs")
        provision = step_block(block, "Provision the pinned TLC toolchain for async driver refinement")
        build = step_block(block, "Build async driver refinement graph and control inputs")
        qualify = step_block(block, "Qualify async driver refinement with pinned TLC")
        artifact = step_block(block, "Preserve async driver refinement evidence")

        self.assertIn("scripts/check-async-driver-refinement affected", selection)
        self.assertIn("value.get(\"affected\")", selection)
        if effort:
            self.assertNotIn("--base", selection)
        else:
            self.assertIn("--base \"$PR_BASE_SHA\"", selection)
        self.assertIn("scripts/bootstrap-formal-toolchain fetch --cache-dir target/formal-toolchain", provision)
        self.assertIn("scripts/check-async-driver-refinement build-inputs --output-dir", build)
        self.assertIn("scripts/check-async-driver-refinement qualify", qualify)
        for required in ("--driver-graph", "--store-graph", "--provenance", "--output-dir", "--cache-dir target/formal-toolchain"):
            with self.subTest(required=required):
                self.assertIn(required, qualify)
        self.assertIn("always()", artifact)
        self.assertIn("target/verification/async-driver-refinement-${{ github.run_id }}-${{ github.run_attempt }}/", artifact)
        self.assertIn("retention-days: 90", artifact)
        self.assertLess(block.index("Compile every Rust target without executing tests"), block.index(build))
        self.assertLess(block.index(provision), block.index(build))
        self.assertLess(block.index(build), block.index(qualify))
        if effort:
            for step in (provision, build, qualify, artifact):
                self.assertIn("!inputs.compile_only", step)
        else:
            gate = job_block((ROOT / ".github/workflows/main-fast-lane.yml").read_text(), "promotion_gate")
            self.assertIn("\n      - rust_compile\n", gate)

    def test_main_lane_qualifies_after_rust_compilation_and_uploads_evidence(self) -> None:
        self.assert_qualification_sequence(self.main, effort=False)
        self.assertIn("fetch-depth: 0", self.main)
        # build-inputs owns the exact Rust graph/control commands; CI must not
        # run a second copy of either source-generating regression.
        self.assertEqual(self.main.count("cargo test"), 0)

    def test_effort_lane_skips_formal_acceptance_for_compile_only_runs(self) -> None:
        self.assert_qualification_sequence(self.effort, effort=True)
        self.assertEqual(self.effort.count("cargo test"), 0)


if __name__ == "__main__":
    unittest.main()
