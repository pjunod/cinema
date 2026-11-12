"""Only actual, applicable target-qualified passes can skip a Rust body."""
import unittest
from unittest.mock import patch

from validation import python_unit_receipts as receipts
from validation import workspace_rust_continuation as proof
from validation import workspace_rust_execute as execute
from validation import workspace_rust_outcomes as native


class WorkspaceRustContinuationTests(unittest.TestCase):
    def original(self, text):
        return (native.COMMAND + '\n' + text).encode()

    def test_stderr_headers_and_interrupted_targets_do_not_exchange_successes(self):
        raw = self.original('''   Running unittests src/lib.rs (target/debug/deps/core-0123456789abcdef)
running 1 test
   Running unittests src/main.rs (target/debug/deps/daemon-0123456789abcdef)
test same ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
running 3 tests
test same ... FAILED
thread 'decode::cancelling_idet' (28055) has overflowed its stack
fatal runtime error: stack overflow, aborting
''')
        rows = native.workspace_targets(raw)
        self.assertEqual(rows['core@src/lib.rs']['outcomes'], {'same':'pass'})
        self.assertEqual(rows['daemon@src/main.rs']['outcomes'], {'same':'fail'})
        self.assertFalse(rows['daemon@src/main.rs']['complete'])
        self.assertEqual(rows['daemon@src/main.rs']['interrupted_test'],'decode::cancelling_idet')
        with self.assertRaises(receipts.ReceiptError):
            native.workspace_targets(raw.replace(b'1 passed;', b'2 passed;'))

    def test_duplicate_or_unframed_outcomes_cannot_inflate_a_prior_pass(self):
        raw = self.original('''   Running unittests src/lib.rs (target/debug/deps/core-0123456789abcdef)
running 2 tests
test same ... ok
test same ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s''')
        with self.assertRaises(receipts.ReceiptError):
            native.workspace_targets(raw)
        with self.assertRaises(receipts.ReceiptError):
            native.workspace_targets(self.original('running 1 test\ntest x ... ok'))

    def test_reviewed_changes_require_every_path_and_exact_prior_byte_identity(self):
        row = {'prior':'a'*40,'changes':{'widget.rs':{'before':proof.retention.digest(b'old'),
            'after':proof.retention.digest(b'new'),'affected':['bin@src/lib.rs::a'],'reason':'logic repaired'}}}
        targets = {'bin@src/lib.rs':{'outcomes':{'a':'pass','b':'fail'}}}
        with patch.object(proof.retention,'changed',return_value={'widget.rs'}), \
             patch.object(proof.retention,'git_bytes',side_effect=lambda rev,path: b'old' if rev=='a'*40 else b'new'):
            self.assertEqual(proof.check_changes(row,'b'*40,targets),{'bin@src/lib.rs::a'})
            row['changes']['widget.rs']['before'] = '0'*64
            with self.assertRaises(receipts.ReceiptError):
                proof.check_changes(row,'b'*40,targets)
            row['changes'] = {}
            with self.assertRaises(receipts.ReceiptError):
                proof.check_changes(row,'b'*40,targets)

    def test_exact_selection_preserves_prefix_siblings_and_fails_when_retained_id_disappears(self):
        remain, selector = execute.exact_selection({'group::one','group::one_long','fresh'}, {'group::one'})
        self.assertEqual(remain,{'group::one_long','fresh'})
        self.assertEqual(selector,['--exact','--skip','group::one'])
        with self.assertRaises(RuntimeError):
            execute.exact_selection({'group::one_long'},{'group::one'})

    def test_root_ownership_needs_complete_native_summary_and_real_names(self):
        raw = b'test_case (test_source_custody_recovery.RecoveryHelperTests.test_case) ... ok\nRan 1 tests in 1.2s\nOK\ncargo fmt --all --check\n'
        source = b'class RecoveryHelperTests:\n def test_case(self): pass\n'
        self.assertTrue(proof.root_prerequisite(raw,source))
        self.assertFalse(proof.root_prerequisite(raw,source + b' def test_other(self): pass\n'))
        self.assertFalse(proof.root_prerequisite(raw.replace(b'Ran 1 tests',b'Ran 2 tests'),source))
        self.assertFalse(proof.root_prerequisite(raw.replace(b'.test_case)',b'.test_else)'),source))

    def test_disappeared_failed_and_ignored_invalidated_bodies_cannot_close_the_target(self):
        proof = {'observed':{'daemon':['green','red','affected']},'must_pass':{'daemon':['red','affected']}}
        with self.assertRaises(RuntimeError):
            execute.verify_observed(proof,'daemon',{'green','affected'},set())
        with self.assertRaises(RuntimeError):
            execute.verify_observed(proof,'daemon',{'green','red','affected'},{'affected'})
        execute.verify_observed(proof,'daemon',{'green','red','affected'},set())

    def test_changed_native_environment_or_global_compiler_flags_cannot_reuse_passes(self):
        from pathlib import Path
        current = Path('.github/workflows/ci.yml').read_text()
        proof.check_workflow_environment(current,proof.CURRENT_JOB)
        for changed in (current.replace('CARGO_TERM_COLOR: always','CARGO_TERM_COLOR: always\n  RUSTFLAGS: -Copt-level=3'),
                        current.replace('RUST_TEST_THREADS: "2"','RUST_TEST_THREADS: "4"'),
                        current.replace('--memory 20g', '--memory 21g'),
                        current.replace('--cpus 4', '--cpus 5')):
            with self.assertRaises(receipts.ReceiptError):
                proof.check_workflow_environment(changed,proof.CURRENT_JOB)

    def test_reviewed_current_native_job_is_a_historical_source_with_no_adopted_inputs(self):
        original = '7069cc0684b6c669023a901cfd0f767bcb3cf11e'
        source = '5ca3abcb0259b401b7da3716c41f6026e2b95415'
        original_workflow = proof.retention.git_bytes(original, '.github/workflows/ci.yml').decode()
        workflow = proof.retention.git_bytes(source, '.github/workflows/ci.yml').decode()
        proof.check_workflow_environment(original_workflow, proof.HISTORICAL_JOBS)
        proof.check_workflow_environment(workflow, proof.HISTORICAL_JOBS)
        with self.assertRaises(receipts.ReceiptError):
            proof.check_workflow_environment(workflow, proof.ORIGINAL_JOB)
        proof.require_native_inputs({})
        for key in ('retained_units_sha256', 'retained_units_attachment_id',
                    'retained_units_attachment_uuid', 'workspace_rust_evidence'):
            with self.subTest(key=key), self.assertRaises(receipts.ReceiptError):
                proof.require_native_inputs({key: 'previous'})
        for changed in (workflow.replace('RUST_TEST_THREADS: "2"', 'RUST_TEST_THREADS: "4"'),
                        workflow.replace('run: make ci-rust-gate', 'run: make rust-check'),
                        workflow + '\ndefaults:\n  run:\n    shell: bash\n',
                        workflow + '\nenv:\n  CARGO_TERM_COLOR: never\n',
                        workflow + '\n"env":\n  CARGO_TERM_COLOR: never\n',
                        workflow + '\n  check:\n    runs-on: [self-hosted, Linux, X64]\n',
                        workflow + '\n  "check":\n    runs-on: [self-hosted, Linux, X64]\n',
                        workflow + '\n  <<: *other_jobs\n'):
            with self.subTest(change=changed[-80:]), self.assertRaises(receipts.ReceiptError):
                proof.check_workflow_environment(changed, proof.HISTORICAL_JOBS)

    def test_reviewed_historical_jobs_do_not_relax_current_admission(self):
        from pathlib import Path
        current = Path('.github/workflows/ci.yml').read_text()
        self.assertEqual(proof.HISTORICAL_JOBS, (proof.ORIGINAL_JOB, proof.PRE_NONVOTER_JOB, proof.NONVOTER_JOB, proof.CGROUP_PARSE_JOB, proof.CURRENT_JOB))
        self.assertEqual(len(set(proof.HISTORICAL_JOBS)), 5)
        for identity in proof.HISTORICAL_JOBS:
            with self.subTest(identity=identity), patch.object(proof.retention, 'digest', return_value=identity):
                proof.check_workflow_environment(current, proof.HISTORICAL_JOBS)
                if identity != proof.CURRENT_JOB:
                    with self.assertRaises(receipts.ReceiptError):
                        proof.check_workflow_environment(current, proof.CURRENT_JOB)
        with patch.object(proof.retention, 'digest', return_value='f' * 64):
            with self.assertRaises(receipts.ReceiptError):
                proof.check_workflow_environment(current, proof.HISTORICAL_JOBS)
