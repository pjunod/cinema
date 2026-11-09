import unittest
from unittest.mock import patch

from validation import native_python_discovery as native
from validation.python_unit_receipts import ReceiptError


def terminal(n, result):
    return '-' * 70 + f'\nRan {n} tests in 1.00s\n\n{result}\n'


def log(validation, operations):
    return ('catalog ok: 3 points\n' + validation + '##[group]Environment details\n'
            'Artifact formal-model-19-1 has been successfully uploaded! Final size is 99 bytes.\n'
            + operations + 'ok - every surface routes every state and every input to a defined outcome\n'
            '🏁  Job succeeded\n')


class NativeDiscoveryCase(unittest.TestCase):
    def test_native_skips_remain_unnamed_and_nested_fixtures_do_not_supply_outcomes(self):
        text = log(terminal(2, 'FAILED (errors=1)') + terminal(8, 'OK'), terminal(9, 'OK (skipped=3)'))
        self.assertEqual(native.outer_counts(text, 'validation', 19, 8)['native_skipped'], 0)
        self.assertEqual(native.outer_counts(text, 'operations', 19, 9),
                         {'native_discovered': 9, 'native_skipped': 3, 'native_skipped_ids': None})

    def test_refuses_misplaced_adopted_failed_or_ambiguous_discovery(self):
        good = terminal(8, 'OK')
        for text in (log(good, terminal(9, 'OK')), log(good, good + good),
                     log(good, good + terminal(3, 'FAILED')), log(good, terminal(8, 'OK (skipped=9)')),
                     log(good, good + 'Adopted authenticated complete operations unit discovery\n'),
                     log(good, good).replace('formal-model-19-1', 'formal-model-91-1'),
                     log(good, good).replace('Job succeeded', 'Job failed')):
            with self.subTest(text=text[-160:]), self.assertRaises(ReceiptError):
                native.outer_counts(text, 'operations', 19, 8)

    def test_historical_retained_input_or_changed_scope_cannot_claim_execution(self):
        with patch.object(native, 'environment'), patch.object(native, 'job_digest', return_value=native.HISTORICAL_SCOPE):
            native.native_event('source', 'workflow', {})
            for key in ('retained_units_sha256', 'retained_units_attachment_id',
                        'retained_units_attachment_uuid', 'python_unit_evidence'):
                with self.subTest(key=key), self.assertRaises(ReceiptError):
                    native.native_event('source', 'workflow', {key: 'previous'})
        with patch.object(native, 'environment'), patch.object(native, 'job_digest', return_value='changed'):
            with self.assertRaises(ReceiptError):
                native.native_event('source', 'workflow', {})

    def test_trusted_current_preflight_execution_survives_only_top_level_metadata(self):
        original = '7069cc0684b6c669023a901cfd0f767bcb3cf11e'
        old = '49c4ec103c9caab84149ee5538ab208a789f7248'
        current = '5ca3abcb0259b401b7da3716c41f6026e2b95415'
        original_workflow = native.retention.git_bytes(original, '.github/workflows/ci.yml').decode()
        old_workflow = native.retention.git_bytes(old, '.github/workflows/ci.yml').decode()
        current_workflow = native.retention.git_bytes(current, '.github/workflows/ci.yml').decode()
        self.assertEqual(native.job_digest(original_workflow, 'preflight'), native.PREFLIGHT)
        self.assertEqual(native.job_digest(old_workflow, 'preflight'), native.CURRENT_PREFLIGHT)
        self.assertEqual(native.preflight_execution_digest(old_workflow), native.CURRENT_EXECUTION)
        self.assertEqual(native.preflight_execution_digest(current_workflow), native.CURRENT_EXECUTION)
        native.environment(original, original_workflow, historical=True)
        native.environment(old, old_workflow, historical=True)
        native.environment(current, current_workflow, historical=True)
        native.environment(current, current_workflow, historical=False)
        native.native_event(current, current_workflow, {})
        with self.assertRaises(ReceiptError):
            native.environment(original, original_workflow, historical=False)
        metadata = current_workflow.replace('    timeout-minutes: 30\n', '    timeout-minutes: 25\n', 1)
        metadata = metadata.replace('    # Match the fast lane', '    # Reviewed metadata: Match the fast lane', 1)
        native.environment(current, metadata, historical=True)
        self.assertNotEqual(native.job_digest(metadata, 'preflight'), native.job_digest(current_workflow, 'preflight'))

    def test_current_preflight_rejects_execution_or_makefile_drift(self):
        current = '5ca3abcb0259b401b7da3716c41f6026e2b95415'
        workflow = native.retention.git_bytes(current, '.github/workflows/ci.yml').decode()
        job = native.job_body(workflow, 'preflight')
        changes = (
            job.replace('runs-on: [self-hosted, Linux, X64, lab, general]',
                        'runs-on: [self-hosted, Linux, X64, lab, different]', 1),
            job.replace('          make validation-lint\n', '          make rust-check\n', 1),
            job.replace('    steps:\n', '    defaults:\n      run:\n        shell: bash\n    steps:\n', 1),
            job.replace('    timeout-minutes: 30\n', '', 1),
            job.replace('    timeout-minutes: 30\n', '    timeout-minutes: 30\n    timeout-minutes: 30\n', 1),
            job.replace('          make validation-lint\n',
                        '          # nested run-body comment\n          make validation-lint\n', 1),
        )
        for changed_job in changes:
            changed = workflow.replace(job, changed_job, 1)
            with self.subTest(change=changed[-80:]), self.assertRaises(ReceiptError):
                native.environment(current, changed, historical=True)
        global_env = workflow.replace('CARGO_TERM_COLOR: always', 'CARGO_TERM_COLOR: never', 1)
        with self.assertRaises(ReceiptError):
            native.environment(current, global_env, historical=True)
        for suffix in ('\ndefaults:\n  run:\n    shell: bash\n',
                       '\nenv:\n  CARGO_TERM_COLOR: never\n',
                       '\n"env":\n  CARGO_TERM_COLOR: never\n',
                       '\n<<: *other_globals\n'):
            with self.subTest(root_override=suffix), self.assertRaises(ReceiptError):
                native.environment(current, workflow + suffix, historical=True)
        for suffix in ('\n  preflight:\n    runs-on: [self-hosted, Linux, X64]\n',
                       '\n  "preflight":\n    runs-on: [self-hosted, Linux, X64]\n',
                       '\n  <<: *other_jobs\n'):
            with self.subTest(job_override=suffix), self.assertRaises(ReceiptError):
                native.environment(current, workflow + suffix, historical=True)
        with patch.object(native.retention, 'git_bytes', return_value=b'unreviewed Makefile'):
            with self.assertRaises(ReceiptError):
                native.environment(current, workflow, historical=True)
        original_makefile = native.retention.git_bytes(
            '5ab77b99b739aefd251fa339693fd5529be7d5c6', 'Makefile')
        with patch.object(native.retention, 'git_bytes', return_value=original_makefile):
            with self.assertRaises(ReceiptError):
                native.environment(current, workflow, historical=True)

    def test_native_continuation_runs_only_selected_exact_cases(self):
        from validation.native_python_execute import run_selection

        import contextlib
        import io

        ran = []

        class Fixture(unittest.TestCase):
            def test_prior(self):
                self.fail('Prior native coverage must be retained')

            def test_prior_extra(self):
                ran.append(self.id())

        tests = [Fixture('test_prior'), Fixture('test_prior_extra')]
        plan = {'inventory': sorted(test.id() for test in tests),
                'uncovered': [tests[1].id()], 'prior_discovery': []}
        with contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(io.StringIO()):
            run_selection(plan, tests)
        self.assertEqual(ran, [tests[1].id()])
        with self.assertRaises(ReceiptError):
            run_selection(plan, tests[1:])
