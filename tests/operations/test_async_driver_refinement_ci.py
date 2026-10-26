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
        workflow = (ROOT / ".github/workflows/main-fast-lane.yml").read_text()
        self.compile = job_block(workflow, "rust_compile")
        self.main = job_block(workflow, "async_refinement")
        self.effort = job_block((ROOT / ".github/workflows/effort-ci.yml").read_text(), "rust_compile")

    def assert_qualification_sequence(self, block: str, *, effort: bool) -> None:
        selection = step_block(block if effort else self.compile, "Select affected async driver refinement inputs")
        provision = step_block(block, "Provision the pinned TLC toolchain for async driver refinement")
        build = step_block(block, "Build async driver refinement graph and control inputs")
        qualify = step_block(block, "Qualify async driver refinement with pinned TLC")
        snapshot = step_block(block, "Snapshot compact async driver refinement evidence")
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
        for required in ("--driver-graph", "--product-bundle", "--provenance", "--output-dir", "--cache-dir target/formal-toolchain"):
            with self.subTest(required=required):
                self.assertIn(required, qualify)
        self.assertIn("always()", artifact)
        self.assertIn("target/verification/async-driver-refinement-${{ github.run_id }}-${{ github.run_attempt }}/", artifact)
        self.assertIn("retention-days: 90", artifact)
        if effort:
            self.assertLess(block.index("Compile every Rust target without executing tests"), block.index(build))
        else:
            self.assertIn("needs: [scope, preflight, rust_compile]",block)
            self.assertNotIn("Compile every Rust target without executing tests",block)
        self.assertLess(block.index(provision), block.index(build))
        self.assertLess(block.index(build), block.index(qualify))
        self.assertIn("validation.async_refinement_resume snapshot", snapshot)
        self.assertIn("--working-dir", snapshot)
        self.assertIn("--output-dir", snapshot)
        self.assertLess(block.index(qualify), block.index(snapshot))
        self.assertLess(block.index(snapshot), block.index(artifact))
        if effort:
            for step in (provision, build, qualify, snapshot, artifact):
                self.assertIn("!inputs.compile_only", step)
            self.assertNotIn("--origin", snapshot)
        else:
            prepare = step_block(block, "Prepare authenticated same-PR async driver proof resumption")
            self.assertIn("validation.async_refinement_resume prepare", prepare)
            self.assertIn("GITHUB_TOKEN: ${{ github.token }}", prepare)
            self.assertIn("steps.async-driver-resume.outputs.working_dir", build)
            self.assertIn("steps.async-driver-resume.outputs.working_dir", qualify)
            self.assertIn("steps.async-driver-resume.outputs.origin_path", snapshot)
            self.assertIn("--origin", snapshot)
            self.assertIn("async_driver_affected: ${{ steps.async-driver-inputs.outputs.affected }}",self.compile)
            self.assertIn("needs.rust_compile.outputs.async_driver_affected == 'true'",block)
            self.assertLess(block.index(prepare), block.index(provision))
            gate = job_block((ROOT / ".github/workflows/main-fast-lane.yml").read_text(), "promotion_gate")
            self.assertIn("\n      - rust_compile\n", gate)
            self.assertIn("\n      - async_refinement\n",gate)

    def test_main_lane_qualifies_after_rust_compilation_and_uploads_evidence(self) -> None:
        self.assert_qualification_sequence(self.main, effort=False)
        self.assertIn("fetch-depth: 0", self.main)
        # build-inputs owns the exact Rust graph/control commands; CI must not
        # run a second copy of either source-generating regression.
        self.assertEqual(self.main.count("cargo test"), 0)

    def test_async_proof_bootstraps_selected_locked_cargo_home_before_offline_cases(self):
        import os
        import subprocess
        import tempfile
        cache=step_block(self.main,"Restore the main fast Rust cache")
        bootstrap=step_block(self.main,"Provision locked dependencies for offline async qualification")
        self.assertIn("id: cargo-cache",cache)
        self.assertIn("uses: ./.github/actions/cargo-cache",cache)
        self.assertIn("lane: main-fast-rust",cache)
        self.assertLess(self.main.index(cache),self.main.index(bootstrap))
        for name in ("Prepare authenticated same-PR async driver proof resumption",
            "Provision the pinned TLC toolchain for async driver refinement",
            "Build deterministic async driver prefix checkpoint",
            "Build async driver refinement graph and control inputs"):
            following=step_block(self.main,name)
            self.assertLess(self.main.index(bootstrap),self.main.index(following))
            self.assertNotIn("always()",following)
        self.assertLess(self.main.index("Install the pinned Rust toolchain"),self.main.index(cache))
        self.assertNotIn("env:",bootstrap)
        self.assertNotIn("GITHUB_TOKEN",bootstrap)
        self.assertNotIn("CARGO_HOME",bootstrap)  # inherit the selected cache
        commands=bootstrap.split("        run: |\n",1)[1].splitlines()
        self.assertTrue(all(not line or line.startswith("          ") for line in commands))
        script="\n".join(line[10:] for line in commands if line)
        self.assertEqual(script,"\n".join(("set -euo pipefail",
            "cargo +1.97.1 fetch --locked --manifest-path Cargo.toml",
            "cargo +1.97.1 fetch --locked --offline --manifest-path Cargo.toml",
            "git diff --exit-code -- Cargo.lock")))
        # Exercise only shell failure propagation with fake functions: no real
        # Cargo, dependency fetch, compilation or proof command is invoked.
        scratch=ROOT/"target/async-refinement-ci-fixtures";scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            trace=Path(directory)/"commands.log"
            stubs=("cargo() { printf 'cargo %s\\n' \"$*\" >> \"$TRACE\"; "
                "case \" $* \" in *' --offline '*) return \"$OFFLINE_STATUS\";; "
                "*) return \"$FETCH_STATUS\";; esac; };\n"
                "git() { printf 'git %s\\n' \"$*\" >> \"$TRACE\"; return \"$LOCK_STATUS\"; };\n")
            for fetch,offline,lock,wanted in ((0,0,0,4),(17,0,0,1),(0,18,0,2),(0,0,19,3)):
                trace.write_text("")
                result=subprocess.run(["/bin/bash","-c",stubs+script+'\nprintf "proof-start\\n" >> "$TRACE"'],
                    env={**os.environ,"TRACE":str(trace),"FETCH_STATUS":str(fetch),
                        "OFFLINE_STATUS":str(offline),"LOCK_STATUS":str(lock)},capture_output=True)
                calls=trace.read_text().splitlines()
                self.assertEqual(len(calls),wanted)
                self.assertEqual(result.returncode,fetch or offline or lock)
                self.assertEqual("proof-start" in calls,not (fetch or offline or lock))

    def test_effort_lane_skips_formal_acceptance_for_compile_only_runs(self) -> None:
        self.assert_qualification_sequence(self.effort, effort=True)
        self.assertEqual(self.effort.count("cargo test"), 0)

    def test_prefix_upload_precedes_product_and_both_phase_budgets_gate_promotion(self):
        import os
        import subprocess
        prefix = step_block(self.main,"Build deterministic async driver prefix checkpoint")
        snapshot = step_block(self.main,"Snapshot immutable async driver prefix checkpoint")
        upload = step_block(self.main,"Preserve immutable async driver prefix checkpoint")
        product = step_block(self.main,"Build async driver refinement graph and control inputs")
        self.assertIn("build-prefix --output-dir",prefix)
        self.assertIn("--phase prefix",snapshot)
        self.assertIn("async-driver-refinement-prefix-${{ github.run_id }}-${{ github.run_attempt }}",upload)
        self.assertNotIn("always()",upload)
        self.assertLess(self.main.index(prefix),self.main.index(snapshot))
        self.assertLess(self.main.index(snapshot),self.main.index(upload))
        self.assertLess(self.main.index(upload),self.main.index(product))
        for block in (self.compile,self.main):
            self.assertIn("timeout-minutes: 90",block)
            self.assertIn('CARGO_BUILD_JOBS: "1"',block)
            self.assertIn("lane: main-fast-rust",block)
        workflow = (ROOT/".github/workflows/main-fast-lane.yml").read_text()
        gate = step_block(job_block(workflow,"promotion_gate"),"Require policy and every affected compile surface")
        self.assertIn("ASYNC_DRIVER_RESULT: ${{ needs.async_refinement.result }}",gate)
        lines = gate.split("        run: |\n",1)[1].splitlines();commands=[]
        for line in lines:
            if line and not line.startswith("          "):break
            commands.append(line[10:] if line else '')
        env = {key:'success' for key in ('PAPERWORK_RESULT','SCOPE_RESULT','PREFLIGHT_RESULT','WORKER_CONTROL_RESULT',
            'RUST_RESULT','ASYNC_DRIVER_RESULT','WEB_RESULT','APPLE_RESULT','ANDROID_RESULT')}
        def result(**values):
            return subprocess.run(['/bin/bash','-eu','-c','\n'.join(commands)],
                env={**os.environ,**env,'ASYNC_DRIVER_AFFECTED':'true',**values},capture_output=True).returncode
        self.assertEqual(result(),0)
        for status in ('skipped','failure','cancelled'):
            self.assertNotEqual(result(ASYNC_DRIVER_RESULT=status),0)
        self.assertNotEqual(result(RUST_RESULT='failure',ASYNC_DRIVER_RESULT='skipped'),0)
        self.assertEqual(result(ASYNC_DRIVER_AFFECTED='false',ASYNC_DRIVER_RESULT='skipped'),0)



if __name__ == "__main__":
    unittest.main()
