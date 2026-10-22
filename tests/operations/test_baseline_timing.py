import unittest
import json
from collections import Counter
from pathlib import Path
from validation.baseline_timing import failure_mapping, parse, reconcile


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

    def test_historical_failure_mapping_matches_all_48_source_audited_dispositions(self):
        audit = json.loads((Path(__file__).resolve().parents[2] /
                            'docs/ci/FULL-SUITE-AUDIT-2026-10-20.json').read_text())
        names = next(sample['failure_tests'] for sample in audit['log_samples']
                     if sample.get('task') == 21274)
        self.assertEqual(len(set(names)), 48)
        mappings = {name: failure_mapping(name) for name in names}
        self.assertEqual(Counter(row['disposition'] for row in mappings.values()), {
            'candidate_specific_repair_pending_execution': 7,
            'candidate_fixture_mitigation_pending_execution': 39,
            'unresolved_pending_reproduction': 2,
        })
        for row in mappings.values():
            self.assertEqual(row['fresh_outcome'], 'pending')
        broker = mappings['provider::tests::durable_revoke_in_oauth_other_connection_fences_provider_effect']
        self.assertEqual(broker['commit'], 'd282a429')
        for name in ('store_result::tests::production_store_results_never_use_bare_let_underscore',
                     'telemetry::tests::a_store_outliving_its_writer_runtime_cannot_reuse_a_stale_sink'):
            self.assertIsNone(mappings[name]['commit'])
            self.assertEqual(mappings[name]['disposition'], 'unresolved_pending_reproduction')

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
        prior = {'failures': [{'name': name, 'binary_stem': 'x-abcd'} for name in ['fixed', 'broken', 'disabled', 'renamed']]}
        reconcile(report, {'required_test_cases': [{'binary_stem': 'x-abcd', 'name': name} for name in ['disabled', 'absent']]}, prior)
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

    def test_same_case_in_other_binary_cannot_satisfy_previous_failure_or_requirement(self):
        report = parse(log('Running unittests src/main.rs (target/debug/deps/plurxd-aaaaaaaaaaaaaaaa)',
                           'running 1 test', 'test tests::same ... ok',
                           'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'))
        prior = {'failures': [{'name': 'tests::same', 'binary': 'plurx_core-bbbbbbbbbbbbbbbb'}]}
        reconcile(report, {'required_test_cases': [{'binary_stem': 'plurx_core', 'name': 'tests::same'}]}, prior)
        self.assertEqual(report['previous_failure_recheck'][0]['status'], 'missing')
        self.assertEqual(report['previous_failure_recheck'][0]['binary_stem'], 'plurx_core')
        self.assertEqual(report['inventory_gaps'][0]['status'], 'not passed')

    def test_same_binary_across_hashes_retains_independent_case_outcomes(self):
        report = parse(log('Running unittests src/lib.rs (target/debug/deps/plurx_core-aaaaaaaaaaaaaaaa)',
                           'running 1 test', 'test tests::same ... ok',
                           'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s',
                           'Running unittests src/main.rs (target/debug/deps/plurxd-bbbbbbbbbbbbbbbb)',
                           'running 1 test', 'test tests::same ... FAILED',
                           'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'))
        prior = {'failures': [{'name': 'tests::same', 'binary': 'plurx_core-cccccccccccccccc'},
                              {'name': 'tests::same', 'binary': 'plurxd-dddddddddddddddd'}]}
        reconcile(report, previous=prior)
        self.assertEqual([case['status'] for case in report['previous_failure_recheck']], ['passed', 'failed'])
        self.assertEqual(report['totals']['failed'], 1)

    def test_explicit_cell_qualification_cannot_borrow_same_binary_case(self):
        report = parse(log('Running tests/x.rs (target/debug/deps/x-aaaaaaaaaaaaaaaa)',
                           'running 1 test', 'test tests::same ... ok',
                           'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'))
        report['suites'][0]['cell_id'] = 'linux'
        prior = {'failures': [{'name': 'tests::same', 'binary_stem': 'x', 'cell_id': cell} for cell in ['macos', 'linux']]}
        reconcile(report, {'required_test_cases': [{'name': 'tests::same', 'binary_stem': 'x', 'cell_id': 'macos'}]}, prior)
        self.assertEqual([case['status'] for case in report['previous_failure_recheck']], ['missing', 'passed'])
        self.assertEqual(report['inventory_gaps'][0]['cell_id'], 'macos')

    def test_name_only_failure_and_requirement_remain_unqualified(self):
        report = parse(log('Running tests/x.rs (target/debug/deps/x-aaaaaaaaaaaaaaaa)',
                           'running 1 test', 'test tests::same ... ok',
                           'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s'))
        reconcile(report, {'required_test_cases': ['tests::same']}, {'failures': [{'name': 'tests::same'}]})
        self.assertEqual(report['previous_failure_recheck'][0]['status'], 'unqualified')
        self.assertEqual(report['inventory_gaps'][0]['status'], 'unqualified identity')

    def test_explicit_native_binary_and_case_id_are_not_rewritten(self):
        name = 'native suite::case/α (variant)'
        binary = 'native-suite-deadbeef'
        report = {'suites': [{'binary_stem': binary, 'result': {}, 'outcomes': [{'name': name, 'status': 'ok'}]}]}
        prior = {'failures': [{'name': name, 'binary_stem': binary}]}
        reconcile(report, previous=prior)
        case = report['previous_failure_recheck'][0]
        self.assertEqual(case['status'], 'passed')
        self.assertEqual(case['name'], name)
        self.assertEqual(case['binary_stem'], binary)


if __name__ == '__main__':
    unittest.main()
