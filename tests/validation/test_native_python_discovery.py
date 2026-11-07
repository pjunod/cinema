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
            for key in ('retained_units_sha256', 'retained_units_attachment_id', 'retained_units_attachment_uuid'):
                with self.subTest(key=key), self.assertRaises(ReceiptError):
                    native.native_event('source', 'workflow', {key: 'previous'})
        with patch.object(native, 'environment'), patch.object(native, 'job_digest', return_value='changed'):
            with self.assertRaises(ReceiptError):
                native.native_event('source', 'workflow', {})

    def test_native_continuation_runs_only_selected_exact_cases(self):
        from validation.native_python_execute import run_selection

        ran = []

        class Fixture(unittest.TestCase):
            def test_prior(self):
                self.fail('Prior native coverage must be retained')

            def test_prior_extra(self):
                ran.append(self.id())

        tests = [Fixture('test_prior'), Fixture('test_prior_extra')]
        plan = {'inventory': sorted(test.id() for test in tests),
                'uncovered': [tests[1].id()], 'prior_discovery': []}
        run_selection(plan, tests)
        self.assertEqual(ran, [tests[1].id()])
        with self.assertRaises(ReceiptError):
            run_selection(plan, tests[1:])
