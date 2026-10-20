"""Runtime identity accounting must fail closed on omitted or mismatched evidence."""
import unittest

from validation import test_cell_manifest as cells


class RuntimeCellManifestTests(unittest.TestCase):
    def setUp(self):
        self.context = {
            'candidate_sha': 'a' * 40, 'candidate_tree': 'b' * 40,
            'cell_id': 'rust-core-lib', 'framework': 'libtest',
            'target': 'x86_64-unknown-linux-gnu', 'profile': 'test',
            'features': ['hiqlite-store'], 'toolchain': 'rustc 1.97.1',
        }

    def assess(self, discovered=None, outcomes=None, **kwargs):
        return cells.assess_cell(self.context, ['a', 'b'] if discovered is None else discovered,
                                 [{'id': 'a', 'status': 'passed'}, {'id': 'b', 'status': 'passed'}]
                                 if outcomes is None else outcomes, **kwargs)

    def test_missing_test_result_never_proves_a_full_suite(self):
        report = self.assess(outcomes=[{'id': 'a', 'status': 'passed'}])
        self.assertEqual(report['missing'], ['b'])
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])
        with self.assertRaisesRegex(cells.ManifestError, 'incomplete'):
            cells.require_complete(report)

    def test_undiscovered_result_cannot_replace_a_missing_test(self):
        report = self.assess(outcomes=[{'id': 'a', 'status': 'passed'}, {'id': 'c', 'status': 'passed'}])
        self.assertEqual(report['missing'], ['b'])
        self.assertEqual(report['unexpected'], ['c'])
        self.assertFalse(report['complete'])

    def test_discovery_error_cannot_be_hidden_by_matching_success_rows(self):
        report = self.assess(discovery_returncode=2)
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])

    def test_duplicate_result_cannot_mask_partial_execution(self):
        report = self.assess(outcomes=[{'id': 'a', 'status': 'passed'}, {'id': 'a', 'status': 'passed'}])
        self.assertEqual(report['duplicate_outcomes'], ['a'])
        self.assertEqual(report['missing'], ['b'])
        self.assertFalse(report['complete'])

    def test_duplicate_discovery_is_ambiguous(self):
        report = self.assess(discovered=['a', 'a'], outcomes=[{'id': 'a', 'status': 'passed'}])
        self.assertEqual(report['duplicate_discovery'], ['a'])
        self.assertFalse(report['passed'])

    def test_test_failure_is_complete_evidence_but_does_not_pass(self):
        report = self.assess(outcomes=[{'id': 'a', 'status': 'failed'}, {'id': 'b', 'status': 'passed'}],
                             execution_returncode=101)
        self.assertTrue(report['complete'])
        self.assertFalse(report['passed'])
        self.assertEqual(report['counts']['failed'], 1)

    def test_runner_failure_prevents_pass_even_with_all_success_rows(self):
        report = self.assess(execution_returncode=137)
        self.assertTrue(report['complete'])
        self.assertFalse(report['passed'])

    def test_ignored_case_needs_explicit_owner_and_reason(self):
        rows = [{'id': 'a', 'status': 'passed'}, {'id': 'b', 'status': 'ignored'}]
        report = self.assess(outcomes=rows)
        self.assertEqual(report['unqualified_skips'], ['b'])
        self.assertTrue(report['complete'])
        self.assertFalse(report['passed'])
        report = self.assess(outcomes=rows, exceptions=[
            {'id': 'b', 'owner': 'media', 'reason': 'Requires real GPU qualification'}])
        self.assertTrue(report['passed'])
        with self.assertRaises(cells.ManifestError):
            self.assess(outcomes=rows, exceptions=[{'id': 'b', 'owner': '', 'reason': 'GPU'}])

    def test_exception_cannot_excuse_a_missing_result(self):
        with self.assertRaisesRegex(cells.ManifestError, 'actually skipped'):
            self.assess(outcomes=[{'id': 'a', 'status': 'passed'}], exceptions=[
                {'id': 'b', 'owner': 'media', 'reason': 'Requires real GPU qualification'}])

    def test_cell_aggregate_requires_every_declared_cell(self):
        second = dict(self.context, cell_id='rust-daemon-lib')
        report = cells.aggregate_cells([self.assess()], [self.context, second])
        self.assertEqual(report['missing_cells'], ['rust-daemon-lib'])
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])

    def test_results_for_another_candidate_are_rejected(self):
        report = self.assess()
        report['context']['candidate_sha'] = 'c' * 40
        with self.assertRaisesRegex(cells.ManifestError, 'differently bound'):
            cells.aggregate_cells([report], [self.context])

    def test_aggregate_recomputes_and_rejects_fabricated_pass_flags(self):
        report = self.assess(outcomes=[{'id': 'a', 'status': 'passed'}])
        report['complete'] = report['passed'] = True
        aggregate = cells.aggregate_cells([report], [self.context])
        self.assertFalse(aggregate['complete'])
        self.assertFalse(aggregate['passed'])

    def test_suite_cannot_combine_expected_cells_from_different_candidates(self):
        second = dict(self.context, cell_id='rust-daemon-lib', candidate_sha='c' * 40)
        with self.assertRaisesRegex(cells.ManifestError, 'one candidate'):
            cells.aggregate_cells([], [self.context, second])

    def test_unregistered_source_scope_cannot_be_hidden_by_success_rows(self):
        report = self.assess(scope_errors=['Unregistered manual renderer control'])
        self.assertFalse(report['complete'])
        self.assertFalse(report['passed'])
        report['complete'] = report['passed'] = True
        aggregate = cells.aggregate_cells([report], [self.context])
        self.assertFalse(aggregate['complete'])
        self.assertFalse(aggregate['passed'])

    def test_cell_aggregate_rejects_duplicate_cells(self):
        with self.assertRaisesRegex(cells.ManifestError, 'Duplicate cell result'):
            cells.aggregate_cells([self.assess(), self.assess()], [self.context])

    def test_feature_order_is_normalized_but_feature_set_changes_binding(self):
        original = dict(self.context, features=['b', 'a'])
        reversed_features = dict(self.context, features=['a', 'b'])
        self.assertEqual(cells.canonical_context(original), cells.canonical_context(reversed_features))
        changed = dict(self.context, features=[])
        self.assertNotEqual(cells.canonical_context(changed)[1], cells.canonical_context(self.context)[1])

    def test_libtest_listing_preserves_names_and_ignored_results_are_visible(self):
        inventory = cells.libtest_inventory('a: test\nb: test\n\n2 tests, 0 benchmarks\n')
        results = cells.libtest_outcomes('running 2 tests\ntest a ... ok\ntest b ... ignored, needs GPU\n')
        report = self.assess(discovered=inventory, outcomes=results)
        self.assertEqual(report['unqualified_skips'], ['b'])
        self.assertFalse(report['passed'])

    def test_zero_test_cell_is_visible_and_discovery_exit_code_still_matters(self):
        report = self.assess(discovered=[], outcomes=[])
        self.assertEqual(report['counts']['discovered'], 0)
        self.assertTrue(report['complete'])
        report = self.assess(discovered=[], outcomes=[], discovery_returncode=1)
        self.assertFalse(report['complete'])

    def test_malformed_outcome_and_boolean_return_code_are_rejected(self):
        with self.assertRaises(cells.ManifestError):
            self.assess(outcomes=[{'id': 'a', 'status': 'success'}])
        with self.assertRaises(cells.ManifestError):
            self.assess(discovery_returncode=True)


if __name__ == '__main__':
    unittest.main()
