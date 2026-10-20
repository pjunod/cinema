"""Negative accounting controls; no Rust builds or suite execution."""
import hashlib
from pathlib import Path
import tempfile
import unittest

from validation.rust_repair_evidence import parse_bound


def footer(passed, failed=0, ignored=0, filtered=0):
    return (f"test result: {'FAILED' if failed else 'ok'}. {passed} passed; {failed} failed; "
            f"{ignored} ignored; 0 measured; {filtered} filtered out; finished in 0.01s")


class RepairEvidenceCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.owner = self.root / 'crates/plurx-core/src/cluster/owner.rs'
        self.owner.parent.mkdir(parents=True)
        self.owner.write_text('reviewed child replay protocol')
        self.policy = {'nested_source_sha256': {
            str(self.owner.relative_to(self.root)): hashlib.sha256(self.owner.read_bytes()).hexdigest()},
            'restore_children': ['restore_control'], 'tray_children': ['delegated_child']}
        self.binary = {'executable': '/target/plurx_core-aabbccdd',
                       'target': {'name': 'plurx_core'},
                       'cases': [{'name': 'restore_control', 'ignored': False},
                                 {'name': 'other_case', 'ignored': False}]}
        self.inventory = {'discovery_complete': True, 'binaries': [self.binary]}
        self.log = '\n'.join([
            'Running unittests src/lib.rs (/target/plurx_core-aabbccdd)',
            'running 2 tests',
            '--- restore child stdout.log tail ---',
            'running 1 test', 'test restore_control ... FAILED', footer(0, 1, filtered=1),
            '--- restore child stderr.log tail ---',
            'test restore_control ... ok', 'test other_case ... ok', footer(2)])

    def assess(self, log=None, **kwargs):
        return parse_bound(self.log if log is None else log, self.inventory, root=self.root,
                           policy=self.policy, **kwargs)

    def test_reviewed_nested_failure_does_not_replace_outer_outcomes(self):
        report = self.assess()
        self.assertTrue(report['qualification_complete'])
        self.assertTrue(report['native_passed'])
        self.assertEqual(report['observed_suites'][0]['result']['passed'], 2)
        self.assertEqual(len(report['nested_protocol_observations']), 1)

    def test_native_failure_is_complete_and_remains_failed(self):
        report = self.assess(self.log.replace('test other_case ... ok', 'test other_case ... FAILED')
                             .replace(footer(2), footer(1, 1)))
        self.assertTrue(report['qualification_complete'])
        self.assertFalse(report['native_passed'])

    def test_missing_duplicate_and_unexpected_outcomes_are_rejected(self):
        for replacement in ['', 'test other_case ... ok\ntest other_case ... ok',
                            'test another_case ... ok']:
            with self.subTest(replacement=replacement):
                self.assertFalse(self.assess(self.log.replace('test other_case ... ok', replacement))
                                 ['qualification_complete'])

    def test_corrupt_footer_and_duplicate_header_are_rejected(self):
        for log in [self.log.replace(footer(2), footer(1)), self.log + '\nrunning 2 tests',
                    self.log + '\n' + footer(2)]:
            self.assertFalse(self.assess(log)['qualification_complete'])

    def test_native_outcomes_must_be_between_header_and_footer(self):
        banner = 'Running unittests src/lib.rs (/target/plurx_core-aabbccdd)'
        for lines in [
            [footer(2), 'running 2 tests', 'test restore_control ... ok', 'test other_case ... ok'],
            ['test restore_control ... ok', 'running 2 tests', 'test other_case ... ok', footer(2)],
            ['running 2 tests', 'test restore_control ... ok', footer(2), 'test other_case ... ok']]:
            self.assertFalse(self.assess('\n'.join([banner, *lines]))['qualification_complete'])

    def test_empty_selected_universe_is_never_qualified(self):
        with self.assertRaises(ValueError):
            self.assess('', selected={})

    def test_wrong_missing_or_duplicate_binary_is_rejected(self):
        for log in [self.log.replace('plurx_core-aabbccdd', 'another-binary-deadbeef'),
                    self.log.split('\n', 1)[1], self.log + '\n' + self.log]:
            self.assertFalse(self.assess(log)['qualification_complete'])

    def test_changed_protocol_source_refuses_to_suppress_child_text(self):
        self.owner.write_text('changed source has not been reviewed')
        self.assertFalse(self.assess()['qualification_complete'])

    def test_child_block_cannot_hide_unrelated_native_result(self):
        log = self.log.replace('test restore_control ... FAILED', 'test other_case ... FAILED')
        self.assertFalse(self.assess(log)['qualification_complete'])

    def test_unclosed_child_block_is_rejected(self):
        self.assertFalse(self.assess(self.log.replace('--- restore child stderr.log tail ---', ''))
                         ['qualification_complete'])

    def test_changed_ignore_disposition_is_rejected(self):
        self.binary['cases'][1]['ignored'] = True
        self.assertFalse(self.assess()['qualification_complete'])

    def test_expected_doctest_cannot_be_omitted_or_satisfied_by_libtest(self):
        self.assertFalse(self.assess(doctest_targets=['plurx_core'])['qualification_complete'])
        report = self.assess(self.log + '\nDoc-tests plurx_core\nrunning 1 test\n'
                             'test src/lib.rs - x (line 2) ... ok\n' + footer(1),
                             doctest_targets=['plurx_core'])
        self.assertTrue(report['qualification_complete'])

    def test_direct_exact_selection_requires_its_own_inventory_case(self):
        native = 'running 1 test\ntest other_case ... ok\n' + footer(1, filtered=1)
        report = self.assess(native, selected={'plurx_core-aabbccdd': ['other_case']},
                             direct_binary='plurx_core-aabbccdd')
        self.assertTrue(report['qualification_complete'])
        self.assertFalse(self.assess(native.replace('other_case', 'restore_control'),
                                    selected={'plurx_core-aabbccdd': ['other_case']},
                                    direct_binary='plurx_core-aabbccdd')['qualification_complete'])

    def test_tray_child_needs_reviewed_owner_and_exact_filtered_footer(self):
        owner = self.root / 'crates/plurx-tray/src/tests.rs'
        owner.parent.mkdir(parents=True)
        owner.write_text('reviewed inherited child protocol')
        self.policy['nested_source_sha256'][str(owner.relative_to(self.root))] = hashlib.sha256(owner.read_bytes()).hexdigest()
        self.binary.update(executable='/target/plurx_tray-aabbccdd', target={'name': 'plurx_tray'},
                           cases=[{'name': 'delegated_child', 'ignored': True},
                                  {'name': 'parent', 'ignored': False}])
        log = '\n'.join(['Running unittests src/lib.rs (/target/plurx_tray-aabbccdd)',
                         'running 2 tests', 'test delegated_child ... ignored',
                         'running 1 test', 'test delegated_child ... ok', footer(1, filtered=1),
                         'test parent ... ok', footer(1, ignored=1)])
        self.assertTrue(self.assess(log)['qualification_complete'])
        self.assertFalse(self.assess(log.replace('1 filtered out', '0 filtered out'))['qualification_complete'])

    def test_empty_or_incomplete_inventory_is_never_qualified(self):
        for inventory in [{'binaries': [], 'discovery_complete': True},
                          dict(self.inventory, discovery_complete=False)]:
            with self.assertRaises(ValueError):
                parse_bound('', inventory, root=self.root, policy=self.policy)


if __name__ == '__main__':
    unittest.main()
