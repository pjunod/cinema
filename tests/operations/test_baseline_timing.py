import unittest
from validation.baseline_timing import parse, reconcile


def log(*lines):
    return [f'2026-10-20T10:00:{i:02d}.0000000Z {line}\n' for i, line in enumerate(lines)]


class ParseRustJobTests(unittest.TestCase):
    def test_success_failure_ignored_counts_and_repair_mapping(self):
        report = parse(log('cargo test --workspace --no-fail-fast',
            '\x1b[1m Running\x1b[0m unittests src/lib.rs (target/debug/deps/plurx_core-abcd)',
            'running 3 tests', 'test passes ... ok',
            'test cluster::migration::tests::restore_archive_with_retired_purpose_keys_admits_new_node_before_key_census ... FAILED',
            'test expensive ... ignored, requires real hardware',
            'test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 3.25s',
            'Job failed'))
        self.assertEqual(report['totals']['failed'], 1)
        self.assertEqual(report['totals']['reported_test_s'], 3.25)
        self.assertEqual(report['failures'][0]['repair_candidate']['commit'], 'a0c2c6730')
        self.assertEqual(report['job_terminal_status'], 'failed')
        self.assertEqual(report['parser_issues'], [])

    def test_truncated_binary_stays_incomplete_and_has_no_synthetic_results(self):
        report = parse(log('Running unittests src/lib.rs (target/debug/deps/plurxd-abcd)',
                           'running 4000 tests',
                           'test transcode::source_actor::tests::slow has been running for over 60 seconds'))
        self.assertEqual(report['incomplete_suite_count'], 1)
        self.assertEqual(report['completed_suite_count'], 0)
        self.assertEqual(report['totals']['passed'], 0)
        self.assertIsNone(report['suites'][0]['result'])
        self.assertEqual(report['suites'][0]['declared_tests'], 4000)
        self.assertEqual(report['totals']['slow_warnings'], 1)
        self.assertEqual(report['job_terminal_status'], 'not observed')

    def test_stderr_next_banner_does_not_steal_previous_stdout_result(self):
        report = parse(log('Running tests/first.rs (target/debug/deps/first-abcd)',
                           'running 1 test', 'test first ... ok',
                           'Running tests/second.rs (target/debug/deps/second-abcd)',
                           'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s',
                           'running 0 tests',
                           'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s'))
        self.assertEqual([s['binary'] for s in report['suites']], ['first-abcd', 'second-abcd'])
        self.assertEqual(report['suites'][0]['result']['passed'], 1)
        self.assertEqual(report['suites'][1]['result']['filtered_out'], 4)
        self.assertEqual(report['parser_issues'], [])

    def test_banner_only_is_missing_result_and_not_zero_test_suite(self):
        report = parse(log('Running tests/crashed.rs (target/debug/deps/crashed-abcd)'))
        self.assertEqual(report['incomplete_suite_count'], 1)
        self.assertIsNone(report['suites'][0]['declared_tests'])
        self.assertIsNone(report['suites'][0]['result'])

    def test_compiler_and_command_windows_are_separate_from_test_seconds(self):
        report = parse(log('cargo clippy --workspace --all-targets -- -D warnings',
                           'Finished `dev` profile [unoptimized] target(s) in 4m 41s',
                           'cargo test --workspace',
                           'Finished `test` profile [unoptimized] target(s) in 7m 20s'))
        self.assertEqual([c['reported_s'] for c in report['compiler']], [281, 440])
        self.assertEqual(report['commands'][0]['window_until_next_command_s'], 2)
        self.assertEqual(report['totals']['reported_test_s'], 0)

    def test_mismatched_outcome_count_is_visible(self):
        report = parse(log('Running tests/x.rs (target/debug/deps/x-abcd)', 'running 2 tests',
                           'test only_visible ... ok',
                           'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s'))
        self.assertIn('outcome_count_mismatch', [i['kind'] for i in report['parser_issues']])

    def test_bad_metric_is_visible_and_valid_metric_remains_evidence(self):
        report = parse(log('PLURX_TEST_METRIC {"elapsed_s":1,"cpu_pct":90}', 'PLURX_TEST_METRIC invalid'))
        self.assertEqual(report['metric_samples'][0]['value']['cpu_pct'], 90)
        self.assertEqual(report['parser_issues'][0]['kind'], 'invalid_metric_json')

    def test_unknown_failure_is_not_claimed_repaired(self):
        report = parse(log('Running tests/provider.rs (target/debug/deps/provider-abcd)',
                           'running 1 test', 'test provider::tests::unknown ... FAILED',
                           'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'))
        self.assertIsNone(report['failures'][0]['repair_candidate']['commit'])

    def test_required_binary_missing_or_incomplete_is_not_complete_evidence(self):
        report = parse(log('Running tests/x.rs (target/debug/deps/x-abc123abcd123abc)'))
        reconcile(report, {'binaries': ['x', 'never_started']})
        self.assertEqual(report['suites'][0]['binary_stem'], 'x')
        self.assertEqual([gap['status'] for gap in report['inventory_gaps']],
                         ['observed without complete result', 'not observed'])

    def test_previous_failures_distinguish_missing_ignored_and_failed_from_passed(self):
        report = parse(log('Running tests/x.rs (target/debug/deps/x-abcd)', 'running 3 tests',
                           'test fixed ... ok', 'test broken ... FAILED', 'test disabled ... ignored',
                           'test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s'))
        prior = {'failures': [{'name': name} for name in ['fixed', 'broken', 'disabled', 'renamed']]}
        reconcile(report, {'required_test_cases': ['disabled', 'absent']}, prior)
        self.assertEqual([case['status'] for case in report['previous_failure_recheck']],
                         ['passed', 'failed', 'ignored', 'missing'])
        self.assertEqual(len(report['inventory_gaps']), 2)

    def test_doctest_banner_has_distinct_identity_and_keeps_preceding_binary_result(self):
        report = parse(log('Running unittests src/lib.rs (target/debug/deps/plurx_core-abc123abcd123abc)',
                           'running 1 test', 'test unit ... ok',
                           '\x1b[1m\x1b[92m   Doc-tests\x1b[0m plurx_core',
                           'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.00s',
                           'running 1 test', 'test src/lib.rs - example (line 3) ... ok',
                           'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s'))
        unit, docs = report['suites']
        self.assertEqual(unit['binary_stem'], 'plurx_core')
        self.assertEqual(unit['kind'], 'libtest')
        self.assertEqual(unit['result']['reported_s'], 2.00)
        self.assertEqual(docs['binary'], 'doctest:plurx_core')
        self.assertEqual(docs['binary_stem'], 'doctest:plurx_core')
        self.assertEqual(docs['kind'], 'doctest')
        self.assertEqual(docs['result']['reported_s'], 0.12)
        self.assertEqual(report['parser_issues'], [])

    def test_incomplete_doctest_does_not_borrow_result_or_satisfy_unit_inventory(self):
        report = parse(log('Doc-tests plurx_core', 'running 1 test'))
        reconcile(report, {'binaries': ['plurx_core']})
        self.assertEqual(report['suites'][0]['binary'], 'doctest:plurx_core')
        self.assertIsNone(report['suites'][0]['result'])
        self.assertEqual(report['incomplete_suite_count'], 1)
        self.assertEqual(report['inventory_gaps'], [{'binary': 'plurx_core', 'status': 'not observed'}])
        banner_only = parse(log('Doc-tests plurx_pgs'))
        self.assertEqual(banner_only['suites'][0]['binary'], 'doctest:plurx_pgs')
        self.assertEqual(banner_only['incomplete_suite_count'], 1)


if __name__ == '__main__':
    unittest.main()
