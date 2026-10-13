"""Negative controls for typed source cohorts; no real unit body is invoked."""
import base64
import copy
import json
import unittest
from unittest import mock

from validation import nonrust_unit_cohort as cohort
from validation import promotion_unit_retention as retention
from validation import python_unit_receipts as receipts
from validation import qualification_unit_continuation as continuation


class NonRustSourceCohortCase(unittest.TestCase):
    def blob(self, proof, raw):
        digest = retention.digest(raw)
        proof['logs'][digest] = {'sha256': digest, 'base64': base64.b64encode(raw).decode()}
        return {'sha256_ref': digest}

    def fixture(self):
        ids = ['test_old.Case.test_' + str(i) for i in range(359)]
        ids.sort()
        proof = {'logs': {}}
        raw = b'Ran 359 tests in 1.000s\n\nOK\n'
        baseline = {'sequence': 1, 'source_commit': cohort.BASE_SOURCE,
                    'origin': {'kind': 'authenticated-ci', 'identity': 'run:4954/job:49307',
                               'command': continuation.COMMANDS['validation'],
                               'execution': {'mode': 'source-cohort-v1', 'kind': 'validation', 'job_status': 'success'}},
                    'changes': [ids[0]], 'log': self.blob(proof, raw),
                    'coverage': {'mode': 'python-discovery', 'ids': ids}, 'exit_code': None}
        named_raw = ('test_0 (' + ids[0] + ') ... ok\n\nRan 1 test in 0.001s\n\nOK\n').encode()
        named_ref = self.blob(proof, named_raw)
        binding = {'source_commit': 'b' * 40, 'branch': 'effort/test',
                   'source_archive_sha256': 'c' * 64,
                   'argv': ['/python3.14', '-m', 'unittest', '-v', 'tests.validation.' + ids[0]],
                   'python_executable': '/python3.14', 'python_version': '3.14.7', 'python_binary_sha256': cohort.MODERN_PYTHON_BINARY,
                   'platform': 'darwin', 'node_version': 'v26.8.1', 'node_binary_sha256': cohort.MODERN_NODE_BINARY,
                   'exit_code': 0, 'source_stable': True,
                   'log_sha256': named_ref['sha256_ref']}
        named = {'sequence': 2, 'source_commit': 'b' * 40,
                 'origin': {'kind': 'attested-local', 'identity': 'actual-tool-record',
                            'command': binding['argv'],
                            'execution': {'mode': 'source-cohort-v1', 'kind': 'validation',
                                          'binding': self.blob(proof, json.dumps(binding).encode())}},
                 'changes': [], 'log': named_ref, 'coverage': {'mode': 'python-named'}, 'exit_code': 0}
        item = {'name': 'validation', 'invocation': continuation.COMMANDS['validation'],
                'protocol': 'source-cohort-v1', 'contract': cohort.CONTRACTS['validation'],
                'inventory': {'ids': ids}, 'records': [baseline, named], 'obligations': []}
        return item, proof, ids, raw, binding

    def validate(self, item, proof, ids, raw):
        def discovery(source, kind):
            return ids if kind == 'validation' else ['test_foreign.Other.test_other']
        with mock.patch.object(cohort, 'BASE_PYTHON_LOG', retention.digest(raw)):
            return cohort.validate(item, proof, 'b' * 40, discovery,
                                   lambda record, candidate, logs: set(record['changes']),
                                   continuation.clean_log, lambda source: 'c' * 64)

    def test_opaque_membership_and_actual_overlay_are_disjoint_without_runtime_rows(self):
        item, proof, ids, raw, _ = self.fixture()
        result = self.validate(item, proof, ids, raw)
        self.assertEqual(result['ids_count'], 359)
        self.assertEqual(result['actual_named_pass_count'], 1)
        self.assertEqual(result['opaque_source_cohorts'][0]['membership_count'], 358)
        self.assertIsNone(result['opaque_source_cohorts'][0]['runtime_named_outcomes'])
        self.assertNotIn('ids', result)  # Bounded CI environment, no huge inventory output.

    def test_missing_invalidated_overlay_and_wrong_contract_refuse(self):
        item, proof, ids, raw, _ = self.fixture()
        item['records'].pop()
        with self.assertRaises(receipts.ReceiptError):
            self.validate(item, proof, ids, raw)
        item['contract'] = 'unknown-runtime-waiver'
        with self.assertRaises(receipts.ReceiptError):
            self.validate(item, proof, ids, raw)

    def test_actual_named_failure_requires_later_named_pass_even_after_opaque_success(self):
        item, proof, ids, raw, binding = self.fixture()
        failed = copy.deepcopy(item['records'][1])
        failed['sequence'] = 1
        failed['exit_code'] = 1
        fail_raw = ('test_0 (' + ids[0] + ') ... FAIL\n\nRan 1 test in 0.001s\n\nFAILED (failures=1)\n').encode()
        failed['log'] = self.blob(proof, fail_raw)
        binding.update(exit_code=1, log_sha256=failed['log']['sha256_ref'])
        failed['origin']['execution']['binding'] = self.blob(proof, json.dumps(binding).encode())
        baseline = item['records'][0]
        baseline['sequence'] = 2
        baseline['changes'] = []
        item['records'] = [failed, baseline]
        with self.assertRaises(receipts.ReceiptError):
            self.validate(item, proof, ids, raw)

    def test_modern_binding_refuses_source_archive_runtime_log_and_unknown_exit(self):
        item, proof, ids, raw, binding = self.fixture()
        for key, value in [('source_archive_sha256', 'e' * 64), ('python_version', '3.13.9'),
                           ('log_sha256', 'e' * 64), ('exit_code', None), ('source_stable', False)]:
            candidate = copy.deepcopy(item)
            bad = {**binding, key: value}
            candidate['records'][1]['origin']['execution']['binding'] = self.blob(proof, json.dumps(bad).encode())
            with self.subTest(key=key), self.assertRaises(receipts.ReceiptError):
                self.validate(candidate, proof, ids, raw)

    def test_named_python_docstring_wrap_and_cross_suite_preserve_exact_namespace(self):
        raw = ('test_a (tests.validation.test_a.Case.test_a)\nAn actual docstring label ... ok\n'
               'test_b (tests.operations.test_b.Case.test_b) ... ok\n\nRan 2 tests in 0.001s\n\nOK')
        self.assertEqual(cohort.python_rows(raw, 'validation', {'test_a.Case.test_a'}, {'test_b.Case.test_b'}),
                         {'test_a.Case.test_a': 'pass'})
        with self.assertRaises(receipts.ReceiptError):
            cohort.python_rows(raw.replace('Ran 2', 'Ran 1'), 'validation', {'test_a.Case.test_a'}, {'test_b.Case.test_b'})
        with self.assertRaises(receipts.ReceiptError):
            cohort.python_rows(raw.replace(' ... ok', ' ... skipped'), 'validation', {'test_a.Case.test_a'}, {'test_b.Case.test_b'})

    def test_junit_preserves_full_class_identity_and_native_failure_totals(self):
        raw = b'<testsuites><testsuite tests="2" failures="1" errors="0"><testcase classname="a.Case" name="same"/><testcase classname="b.Case" name="same"><failure/></testcase></testsuite></testsuites>'
        self.assertEqual(cohort.junit_rows(raw, {'a.Case/same', 'b.Case/same'}),
                         {'a.Case/same': 'pass', 'b.Case/same': 'fail'})
        for bad in [raw.replace(b'failures="1"', b'failures="0"'),
                    raw.replace(b'b.Case', b'a.Case'), raw.replace(b'<failure/>', b'<skipped/>'),
                    b'<!DOCTYPE x>' + raw]:
            with self.subTest(bad=bad), self.assertRaises(receipts.ReceiptError):
                cohort.junit_rows(bad, {'a.Case/same', 'b.Case/same'})

    def test_compiled_metadata_binds_source_and_never_asserts_runtime_passes(self):
        proof = {'logs': {}}
        source = b'actual test source'
        metadata = {'kind': 'compiled-junit4-discovery-not-runtime-outcomes', 'unit_runtime_outcomes': None,
                    'classes': [{'class': 'a.Case', 'ids': ['a.Case.same'],
                                 'source_path': 'clients/android/app/src/test/java/a/Case.kt', 'source_file': 'Case.kt',
                                 'source_sha256': retention.digest(source)}]}
        ref = self.blob(proof, json.dumps(metadata).encode())
        with mock.patch.object(retention, 'git_bytes', return_value=source):
            self.assertEqual(cohort.jvm_metadata(proof, ref, 'b' * 40), ['a.Case/same'])
        with mock.patch.object(retention, 'git_bytes', return_value=b'changed'):
            with self.assertRaises(receipts.ReceiptError):
                cohort.jvm_metadata(proof, ref, 'b' * 40)
        metadata['unit_runtime_outcomes'] = {'a.Case/same': 'pass'}
        ref = self.blob(proof, json.dumps(metadata).encode())
        with self.assertRaises(receipts.ReceiptError):
            cohort.jvm_metadata(proof, ref, 'b' * 40)

    def test_finite_legacy_capture_cannot_authorize_different_log_or_source(self):
        item, proof, ids, raw, _ = self.fixture()
        record = item['records'][1]
        record['origin']['execution'] = {'mode': 'source-cohort-v1', 'kind': 'validation',
                                         'legacy_capture': next(iter(cohort.LEGACY_PYTHON))}
        with self.assertRaises(receipts.ReceiptError):
            self.validate(item, proof, ids, raw)


if __name__ == '__main__':
    unittest.main()
