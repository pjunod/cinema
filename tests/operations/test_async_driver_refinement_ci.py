"""Source contracts for ordinary async consumption and registered continuation."""
from pathlib import Path
import re
import unittest
from validation import async_refinement_session as session
from validation import async_refinement_provenance as provenance
from validation import python_unit_receipts as receipts

ROOT = Path(__file__).resolve().parents[2]


def job_block(source, name):
    start = source.index('\n  '+name+':\n')+1
    following = re.search(r'\n  [a-zA-Z0-9_-]+:\n',source[start:])
    return source[start:start+following.start()] if following else source[start:]


def step_block(source,name):
    start=source.index('      - name: '+name+'\n')
    following=re.search(r'\n      - name:',source[start+1:])
    return source[start:start+1+following.start()] if following else source[start:]


class AsyncDriverRefinementCiTests(unittest.TestCase):
    def setUp(self):
        self.workflow=(ROOT/'.github/workflows/main-fast-lane.yml').read_text()
        self.main=job_block(self.workflow,'async_refinement')
        self.compile=job_block(self.workflow,'rust_compile')
        self.effort=job_block((ROOT/'.github/workflows/effort-ci.yml').read_text(),'rust_compile')
        self.session=(ROOT/'.github/workflows/async-proof-session.yml').read_text()

    def test_main_lane_qualifies_after_rust_compilation_and_uploads_evidence(self):
        # The new complete root replaces monolithic construction in ordinary PR
        # jobs. Keeping this historical test identity pins the stronger boundary.
        self.assertIn('needs: [scope, preflight, rust_compile]',self.main)
        self.assertIn('needs.rust_compile.outputs.async_driver_affected',self.main)
        self.assertIn('validation.async_refinement_session require-completion',self.main)
        self.assertIn('completion.json',self.main)
        self.assertIn('if-no-files-found: error',self.main)
        self.assertNotIn('build-inputs',self.main)
        self.assertNotIn('cargo test',self.main)
        self.assertIn('async_driver_affected:',self.compile)
        gate=job_block(self.workflow,'promotion_gate')
        self.assertIn('\n      - async_refinement\n',gate)
        self.assertIn('validation.async_refinement_session main-aggregate',gate)

    def test_async_proof_bootstraps_selected_locked_cargo_home_before_offline_cases(self):
        for name in ('unit','complete'):
            block=job_block(self.session,name)
            cache=step_block(block,'Restore the owned compiler slot')
            inputs=step_block(block,'Provision locked offline inputs')
            execute=step_block(block,'Authenticate and execute the registered protocol')
            self.assertLess(block.index(cache),block.index(inputs))
            self.assertLess(block.index(inputs),block.index(execute))
            self.assertIn('cargo +1.97.1 fetch --locked --offline',inputs)
            self.assertIn('git diff --exit-code -- Cargo.lock',inputs)
            self.assertNotIn('GITHUB_TOKEN',inputs)
            self.assertIn('GITHUB_TOKEN: ${{ github.token }}',execute)
            self.assertIn('persist-credentials: false',block)
            self.assertIn('validation.async_refinement_session runner',execute)

    def test_effort_lane_skips_formal_acceptance_for_compile_only_runs(self):
        # Effort remains its original compiler/monolithic protocol; it is not an
        # implicit fallback session producer or bootstrap merge exception.
        for name in ('Provision the pinned TLC toolchain for async driver refinement',
                     'Build async driver refinement graph and control inputs',
                     'Qualify async driver refinement with pinned TLC',
                     'Preserve async driver refinement evidence'):
            self.assertIn('!inputs.compile_only',step_block(self.effort,name))
        self.assertNotIn('async_refinement_session',self.effort)

    def test_prefix_upload_precedes_product_and_both_phase_budgets_gate_promotion(self):
        # Prefix-only success is now rejected by the common Main predicate;
        # units publish their complete multipart inventory before native slots.
        for job in ('unit','complete'):
            block=job_block(self.session,job)
            execute=block.index('Authenticate and execute the registered protocol')
            for n in range(provenance.UPLOAD_SLOTS):
                slot=step_block(block,'Native immutable part '+str(n))
                self.assertLess(execute,block.index(slot))
                self.assertIn('steps.session.outputs.part_'+str(n)+'_name',slot)
                self.assertIn('if-no-files-found: error',slot)
                self.assertIn('upload-artifact@16871d9e8cfcf27ff31822cac382bbb5450f1e1e',slot)
            self.assertNotIn('strategy:',block)
        values={key:'success' for key in session.MAIN_ENV}
        for bad in ('skipped','failure','cancelled','checkpointed_incomplete'):
            with self.subTest(bad=bad),self.assertRaises(receipts.ReceiptError):
                session.main_conditions({**values,'async_refinement':bad},True)
        self.assertTrue(session.main_conditions({**values,'async_refinement':'skipped'},False))

    def test_native_part_slots_share_exact_producer_task_and_closed_modes(self):
        self.assertEqual(self.session.count('Native immutable part '),2*provenance.UPLOAD_SLOTS)
        self.assertNotIn('node ',self.session)
        self.assertNotIn('matrix:',self.session)
        self.assertIn('options: [unit, complete]',self.session)
        self.assertIn('options: [probe, execute]',self.session)
        self.assertIn('cancel-in-progress: false',self.session)


if __name__=='__main__':unittest.main()
