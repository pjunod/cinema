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

    def custody_fixture(self):
        proof = {'logs': {}}
        binding = {'source_clean_after': False, 'source_commit': 'b' * 40,
                   'log_sha256': 'd' * 64, 'tree': 'a' * 40,
                   'source_archive_sha256': 'c' * 64, 'started_utc': 'start',
                   'finished_utc': 'finish', 'exit_code': 0}
        raw = json.dumps(binding).encode()
        supplement = {'original_binding_sha256': retention.digest(raw),
                      'original_raw_log_sha256': binding['log_sha256'],
                      'source_commit': binding['source_commit'], 'tree': binding['tree'],
                      'original_source_archive_sha256': binding['source_archive_sha256'],
                      'original_started_utc': 'start', 'original_finished_utc': 'finish',
                      'original_native_exit': 0, 'original_wrapper_exit': 1,
                      'original_source_clean_after': False, 'original_dirty_paths': None,
                      'tracked_entries': 5874, 'tracked_byte_type_git_executable_mismatches': [],
                      'tracked_entries_with_mtime_or_ctime_after_original_start': [],
                      'current_head': binding['source_commit'], 'current_porcelain': '',
                      'unit_bodies_executed': 0}
        reference = self.blob(proof, json.dumps(supplement).encode())
        registry = {retention.digest(raw): {'source': binding['source_commit'],
                    'log': binding['log_sha256'], 'supplement': reference['sha256_ref']}}
        return proof, raw, binding, supplement, reference, registry

    def test_finite_custody_preserves_dirty_observation_and_wrapper_failure(self):
        proof, raw, binding, _, reference, registry = self.custody_fixture()
        with mock.patch.object(cohort, 'RECONCILED_CUSTODY', registry):
            cohort.require_capture_custody(proof, {'custody_supplement': reference}, raw, binding)
        self.assertIs(binding['source_clean_after'], False)

    def test_finite_custody_refuses_missing_mutated_cross_source_and_other_capture(self):
        proof, raw, binding, supplement, reference, registry = self.custody_fixture()
        with mock.patch.object(cohort, 'RECONCILED_CUSTODY', registry):
            with self.assertRaises(receipts.ReceiptError):
                cohort.require_capture_custody(proof, {}, raw, binding)
            for key, value in [('source_commit', 'e' * 40), ('original_wrapper_exit', 0),
                               ('tracked_entries_with_mtime_or_ctime_after_original_start', ['input.py'])]:
                changed = {**supplement, key: value}
                changed_ref = self.blob(proof, json.dumps(changed).encode())
                with self.subTest(key=key), self.assertRaises(receipts.ReceiptError):
                    cohort.require_capture_custody(proof, {'custody_supplement': changed_ref}, raw, binding)
            other = {**binding, 'source_commit': 'e' * 40}
            with self.assertRaises(receipts.ReceiptError):
                cohort.require_capture_custody(proof, {'custody_supplement': reference}, json.dumps(other).encode(), other)

    def test_custody_keeps_absent_history_unknown_and_refuses_invalid_new_flag(self):
        cohort.require_capture_custody({'logs': {}}, {}, b'{}', {})
        cohort.require_capture_custody({'logs': {}}, {}, b'{}', {'source_clean_after': True})
        for value in (None, 0, 'true', False):
            with self.subTest(value=value), self.assertRaises(receipts.ReceiptError):
                cohort.require_capture_custody({'logs': {}}, {}, b'{}', {'source_clean_after': value})
        with self.assertRaises(receipts.ReceiptError):
            cohort.require_capture_custody({'logs': {}}, {'custody_supplement': {}}, b'{}', {'source_clean_after': True})

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

    def interleaved_fixture(self, identity):
        prefix = identity.rsplit('.', 1)[1] + ' (' + identity + ') ... '
        raw = (prefix + 'COMPILE_ONLY actual mocked compiler\n'
               'ADOPTED_COMPLETE_UNIT_COMMAND wal:237\n' +
               'COMPILE_ONLY actual mocked compiler\n' * 4 +
               'ok\n\nRan 1 test in 0.001s\n\nOK\n').encode()
        registry = {retention.digest(raw): {'source': 'b' * 40, 'id': identity,
                                          'binding': retention.digest(b'original binding')}}
        return raw, registry

    def test_finite_interleaved_capture_preserves_raw_and_requires_exact_source_and_terminal(self):
        identity = 'tests.validation.test_a.Case.test_a'
        raw, registry = self.interleaved_fixture(identity)
        original = bytes(raw)
        with mock.patch.object(cohort, 'INTERLEAVED_PYTHON_CAPTURES', registry):
            projected = cohort.named_python_capture_text(raw, 'b' * 40, 'validation', b'original binding')
            self.assertEqual(cohort.python_rows(projected, 'validation', {'test_a.Case.test_a'}),
                             {'test_a.Case.test_a': 'pass'})
            with self.assertRaises(receipts.ReceiptError):
                cohort.named_python_capture_text(raw, 'e' * 40, 'validation', b'original binding')
            for mutated in (raw + b'changed\n', raw.replace(b'\nok\n', b'\nFAIL\n'),
                            raw.replace(b'wal:237', b'wal:238')):
                self.assertEqual(cohort.named_python_capture_text(mutated, 'b' * 40, 'validation', b'original binding'), mutated.decode())
                with self.assertRaises(receipts.ReceiptError):
                    cohort.python_rows(mutated.decode(), 'validation', {'test_a.Case.test_a'})
            for bad in ({**registry[retention.digest(raw)], 'id': identity + '_other'},):
                with mock.patch.object(cohort, 'INTERLEAVED_PYTHON_CAPTURES', {retention.digest(raw): bad}):
                    with self.assertRaises(receipts.ReceiptError):
                        cohort.named_python_capture_text(raw, 'b' * 40, 'validation', b'original binding')
            for kind, binding_raw in [('operations', b'original binding'), ('validation', b'changed binding')]:
                with self.subTest(kind=kind, binding=binding_raw), self.assertRaises(receipts.ReceiptError):
                    cohort.named_python_capture_text(raw, 'b' * 40, kind, binding_raw)
        self.assertEqual(raw, original)

    def test_named_interleaved_enrollment_keeps_binding_and_refuses_mutated_custody(self):
        item, proof, ids, baseline, binding = self.fixture()
        raw, registry = self.interleaved_fixture('tests.validation.' + ids[0])
        record = item['records'][1]
        record['log'] = self.blob(proof, raw)
        binding['log_sha256'] = record['log']['sha256_ref']
        record['origin']['execution']['binding'] = self.blob(proof, json.dumps(binding).encode())
        registry[retention.digest(raw)]['binding'] = retention.digest(json.dumps(binding).encode())
        with mock.patch.object(cohort, 'INTERLEAVED_PYTHON_CAPTURES', registry):
            result = self.validate(item, proof, ids, baseline)
            self.assertEqual(result['actual_named_pass_count'], 1)
            self.assertEqual(cohort.blob(proof, record['log']), raw)
            binding['log_sha256'] = 'e' * 64
            record['origin']['execution']['binding'] = self.blob(proof, json.dumps(binding).encode())
            with self.assertRaises(receipts.ReceiptError):
                self.validate(item, proof, ids, baseline)

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

    def test_actual_jvm_command_shape_refuses_executable_init_project_and_runtime_overrides(self):
        command = ['./gradlew', '--no-daemon', '--max-workers=1', ':app:testDebugUnitTest',
                   '-Dorg.gradle.jvmargs=-Xmx4g', '--tests', 'tv.plurx.app.data.ExampleTest.example']
        self.assertEqual(cohort.modern_jvm_argv(command), ['tv.plurx.app.data.ExampleTest.example'])
        for bad in [['evil-gradle', *command[1:]], command + ['--init-script', 'evil.gradle'],
                    command + ['--project-dir', '/other'], command + ['-PskipTests=true'],
                    command + ['-Dorg.gradle.java.home=/other'], command + command[5:]]:
            with self.subTest(bad=bad), self.assertRaises(receipts.ReceiptError):
                cohort.modern_jvm_argv(bad)

    def test_legacy_native_xml_preserves_unknown_execution_and_exact_source_equivalence(self):
        proof = {'logs': {}}
        xml = b'<testsuite tests="1" failures="0" errors="0"><testcase classname="a.Case" name="same"/></testsuite>'
        reference = self.blob(proof, xml)
        raw = b'> Task :app:testDebugUnitTest\nBUILD SUCCESSFUL in 1s\n'
        source = b'original reviewed equivalent input'
        capture = {'tool': 'actual-original-tool', 'reviewed_source_equivalence': 'b' * 40,
                   'exit_code': 0, 'mode': 'junit-xml', 'membership_ids': ['a.Case/same'],
                   'reviewed_input_sha256': {'native.kt': retention.digest(source)},
                   'xml_sha256': [reference['sha256_ref']]}
        log = self.blob(proof, raw)
        record = {'source_commit': 'b' * 40, 'exit_code': 0, 'log': log,
                  'origin': {'kind': 'attested-local', 'identity': 'actual-original-tool', 'command': None,
                             'execution': {'mode': 'source-cohort-v1', 'kind': 'android_jvm',
                                           'legacy_capture': log['sha256_ref'], 'actual_execution_source_commit': None,
                                           'runtime_binary_sha256': None, 'image_digest': None}},
                  'coverage': {'mode': 'junit-xml', 'xml': [reference]}}
        with mock.patch.object(retention, 'git_bytes', return_value=source):
            self.assertEqual(cohort.legacy_jvm_outcomes(proof, record, capture, raw, {'a.Case/same'}),
                             {'a.Case/same': 'pass'})
            record['origin']['execution']['actual_execution_source_commit'] = 'b' * 40
            with self.assertRaises(receipts.ReceiptError):
                cohort.legacy_jvm_outcomes(proof, record, capture, raw, {'a.Case/same'})
            record['origin']['execution']['actual_execution_source_commit'] = None
            record['coverage']['xml'] = [self.blob(proof, xml + b'\n')]
            with self.assertRaises(receipts.ReceiptError):
                cohort.legacy_jvm_outcomes(proof, record, capture, raw, {'a.Case/same'})

    def test_original_jvm_failure_repair_chronology_cannot_be_omitted_or_reordered(self):
        order = ['9b07da0a9bcb25fdf1183a3c4f41056ab96b179aab9a247b0dda7e4dad9fdad6',
                 '12ffb580c4ae2bf03f9a76b42b7bfff87678744eb4294ee5905b8cef6f08fdeb',
                 'd998e79e3563d80208b23b20b16e0e547a7972aca1ed614d6187a23af7cdc5be']
        cohort.require_legacy_chronology(dict(zip(order, [5, 6, 7])))
        for sequences in [dict(zip(order[:2], [5, 6])), dict(zip(order, [6, 5, 7]))]:
            with self.assertRaises(receipts.ReceiptError):
                cohort.require_legacy_chronology(sequences)

    def test_partial_selected_python_keeps_explicit_error_and_opaque_success_membership(self):
        capture = {'mode': 'python-selected-partial', 'count': 2, 'failed_ids': ['test_b.Case.test_bad']}
        selected = {'test_a.Case.test_ok', 'test_b.Case.test_bad'}
        raw = ('ERROR: test_bad (tests.operations.test_b.Case.test_bad)\n'
               'Ran 2 tests in 0.001s\n\nFAILED (errors=1)')
        self.assertEqual(cohort.selected_python_outcomes(raw, capture, selected),
                         {'test_a.Case.test_ok': 'opaque-pass', 'test_b.Case.test_bad': 'fail'})
        for bad in [raw.replace('Ran 2', 'Ran 1'), raw.replace('errors=1', 'errors=1, skipped=1'),
                    raw.replace('test_b.Case.test_bad', 'test_b.Case.unknown'),
                    raw + '\nFAIL: test_ok (tests.operations.test_a.Case.test_ok)']:
            with self.subTest(bad=bad), self.assertRaises(receipts.ReceiptError):
                cohort.selected_python_outcomes(bad, capture, selected)

    def test_exact_single_selected_repair_is_typed_without_invented_named_output(self):
        identity = 'test_b.Case.test_bad'
        capture = {'mode': 'python-selected', 'count': 1, 'selectors': [identity], 'exact_selected_repair': [identity]}
        raw = 'Ran 1 test in 0.001s\n\nOK'
        self.assertEqual(cohort.selected_python_outcomes(raw, capture, {identity}), {identity: 'opaque-pass'})
        capture['count'] = 2
        with self.assertRaises(receipts.ReceiptError):
            cohort.selected_python_outcomes('Ran 2 tests in 0.001s\n\nOK', capture, {identity, 'test_a.Case.test_ok'})

    def test_original_error_requires_later_exact_repair_of_the_same_source_variant(self):
        edge = {'source': 'a' * 40, 'id': 'test_port.Case.test_port', 'repair': 'c' * 64}
        key = (edge['source'], edge['id'], 'd' * 64)
        repair = (edge['source'], edge['id'], edge['repair'])
        with mock.patch.object(cohort, 'ORIGINAL_PYTHON_REPAIR_EDGES', {key[2]: edge}):
            cohort.require_source_bound_repairs({key: 5}, {repair: 6})
            for repairs in [{}, {repair: 4}, {('b' * 40, key[1], repair[2]): 6},
                            {(key[0], 'test_other.Case.test_other', repair[2]): 6},
                            {(key[0], key[1], 'e' * 64): 6}]:
                with self.subTest(repairs=repairs), self.assertRaises(receipts.ReceiptError):
                    cohort.require_source_bound_repairs({key: 5}, repairs)

    def test_closed_old_variant_never_supplies_missing_current_applicable_coverage(self):
        item, proof, ids, raw, _ = self.fixture()
        old_source = 'a' * 40
        failed_raw = ('ERROR: test_0 (' + ids[0] + ')\n'
                      'Ran 2 tests in 0.001s\n\nFAILED (errors=1)\n').encode()
        repaired_raw = b'Ran 1 test in 0.001s\n\nOK\n'
        failed_ref, repaired_ref = self.blob(proof, failed_raw), self.blob(proof, repaired_raw)
        registry, records = {}, []
        for sequence, ref, mode, count, selectors, exit_code in [
                (3, failed_ref, 'python-selected-partial', 2, ids[:2], 1),
                (4, repaired_ref, 'python-selected', 1, ids[:1], 0)]:
            capture = {'source': old_source, 'tool': 'actual-original-' + str(sequence),
                       'mode': mode, 'count': count, 'selectors': selectors, 'exit_code': exit_code}
            capture['failed_ids' if exit_code else 'exact_selected_repair'] = ids[:1]
            registry[ref['sha256_ref']] = capture
            records.append({'sequence': sequence, 'source_commit': old_source, 'exit_code': exit_code,
                            'origin': {'kind': 'attested-local', 'identity': capture['tool'], 'command': None,
                                       'execution': {'mode': 'source-cohort-v1', 'kind': 'validation',
                                                     'legacy_capture': ref['sha256_ref']}},
                            'changes': ids[:2], 'log': ref, 'coverage': {'mode': mode}})
        edge = {'source': old_source, 'id': ids[0], 'repair': repaired_ref['sha256_ref']}
        item['records'].extend(records)
        with mock.patch.object(cohort, 'LEGACY_PYTHON', registry), \
                mock.patch.object(cohort, 'ORIGINAL_PYTHON_REPAIR_EDGES', {failed_ref['sha256_ref']: edge}):
            self.assertEqual(self.validate(item, proof, ids, raw)['actual_named_pass_count'], 1)
            later = copy.deepcopy(item['records'][1])
            later['sequence'], later['exit_code'] = 5, 1
            later_raw = ('test_0 (' + ids[0] + ') ... FAIL\n\nRan 1 test in 0.001s\n\nFAILED (failures=1)\n').encode()
            later['log'] = self.blob(proof, later_raw)
            binding = json.loads(cohort.blob(proof, later['origin']['execution']['binding']))
            binding.update(exit_code=1, log_sha256= later['log']['sha256_ref'])
            later['origin']['execution']['binding'] = self.blob(proof, json.dumps(binding).encode())
            item['records'].append(later)
            with self.assertRaises(receipts.ReceiptError):
                self.validate(item, proof, ids, raw)
            item['records'].pop()
            item['records'].remove(item['records'][1])
            with self.assertRaises(receipts.ReceiptError):
                self.validate(item, proof, ids, raw)

    def test_original_jvm_failed_command_witness_requires_exact_ordered_native_repair(self):
        proof = {'logs': {}}
        raw = (b'> Task :app:testDebugUnitTest FAILED\nCase > bad FAILED\n'
               b'3 tests completed, 1 failed\nBUILD FAILED in 1s\n')
        ref = self.blob(proof, raw)
        commands = {'items': [{'id': 'failed-tool', 'exitCode': 1, 'source_turn': 'same-turn'},
                              {'id': 'passed-tool', 'exitCode': 0, 'source_turn': 'same-turn'}]}
        commands_ref = self.blob(proof, json.dumps(commands).encode())
        capture = {'tool': 'passed-tool', 'prior_failure': {
            'log_sha256': ref['sha256_ref'], 'tool': 'failed-tool', 'id': 'a.Case/bad',
            'native_count': 3, 'native_failures': 1, 'original_commands_sha256': commands_ref['sha256_ref']}}
        self.assertEqual(cohort.original_jvm_failure_witness(proof, capture), 'a.Case/bad')
        for bad in [raw.replace(b'3 tests', b'4 tests'), raw.replace(b'bad FAILED', b'other FAILED'),
                    raw.replace(b'BUILD FAILED', b'BUILD SUCCESSFUL')]:
            capture['prior_failure']['log_sha256'] = self.blob(proof, bad)['sha256_ref']
            with self.assertRaises(receipts.ReceiptError):
                cohort.original_jvm_failure_witness(proof, capture)
        capture['prior_failure']['log_sha256'] = ref['sha256_ref']
        commands['items'].reverse()
        capture['prior_failure']['original_commands_sha256'] = self.blob(proof, json.dumps(commands).encode())['sha256_ref']
        with self.assertRaises(receipts.ReceiptError):
            cohort.original_jvm_failure_witness(proof, capture)

    def linked_ci_fixture(self):
        # Small source-native fixtures exercise the closed-pair decoder without
        # copying production logs or executing any imported test body.
        failures = tuple(kind + ':test_pair.Case.test_' + str(i)
                         for kind in ('validation', 'operations') for i in range(2))
        docs = tuple('operations:test_docs.Case.test_' + str(i) for i in range(2))
        scope = copy.deepcopy(cohort.LINKED_PYTHON_CI_SCOPE)
        policy = copy.deepcopy(cohort.LINKED_PYTHON_CI_RUNS)
        proof, raw_logs, snapshots = {'logs': {}}, {}, {}
        initial = {key: {'commit': policy[5249]['source'], 'run': 5249} for key in docs}
        repaired = {**initial, **{key: {'commit': policy[5254]['source'], 'run': 5254}
                                  for key in failures}}

        def wire(journal, phase):
            raw = json.dumps(journal, sort_keys=True, separators=(',', ':')).encode()
            encoded = base64.b64encode(raw).decode()
            chunks = [encoded[i:i + 2048] for i in range(0, len(encoded), 2048)]
            digest = retention.digest(raw)
            return ('MAIN-UNIT-JOURNAL ' + phase + ' ' + str(len(chunks)) + ' ' + digest + '\n'
                    + ''.join('MAIN-UNIT-CHUNK ' + phase + ' ' + str(i) + ' ' + part + '\n'
                              for i, part in enumerate(chunks)) + 'MAIN-UNIT-END ' + phase + '\n'), digest

        def native(keys, status):
            return ''.join(key.rsplit('.', 1)[1] + ' (tests.' + key.replace(':', '.', 1)
                           + ') ... ' + status + '\n' for key in keys)

        for run, row in policy.items():
            phases = {}
            for phase in ('start', 'final'):
                passes = ({} if run == 5249 else initial) if phase == 'start' else (initial if run == 5249 else repaired)
                phases[phase] = {'version': 1, 'scope': scope, 'commit': row['source'],
                    'applicability_commit': row['source'], 'run': run,
                    'complete': phase == 'final', 'fixture_errors': [], 'passes': passes}
            start, start_sha = wire(phases['start'], 'start')
            finish, finish_sha = wire(phases['final'], 'final')
            if run == 5249:
                body = ''
                for kind in ('validation', 'operations'):
                    own = [key for key in failures if key.startswith(kind + ':')]
                    successful = [key for key in docs if key.startswith(kind + ':')]
                    for index, key in enumerate(own):
                        method, identity = key.rsplit('.', 1)[1], 'tests.' + key.replace(':', '.', 1)
                        # Cover both native subtest shapes: same-line and
                        # docstring-wrapped, with no invented parent terminal.
                        body += method + ' (' + identity + ') ... \n'
                        body += '  ' + method + ' (' + identity + ") (scope='owned')"
                        body += (' ... FAIL\n' if index == 0 else '\nNative docstring ... FAIL\n')
                    body += native(successful, 'ok')
                    body += ''.join('FAIL: ' + key.rsplit('.', 1)[1] + ' (tests.'
                                    + key.replace(':', '.', 1) + ") (scope='owned')\n"
                                    for key in own)
                    body += '\nRan ' + str(len(own + successful)) + ' tests in 0.001s\n\nFAILED (failures=2)\n'
            else:
                body = ''.join(native([key for key in failures if key.startswith(kind + ':')], 'ok')
                               + '\nRan 2 tests in 0.001s\n\nOK\n'
                               for kind in ('validation', 'operations'))
            raw = (start + body + finish).encode()
            raw_logs[run], snapshots[run] = raw, phases
            row.update(raw_sha256=self.blob(proof, raw)['sha256_ref'],
                journal_artifact_sha256=self.blob(proof, json.dumps(phases['final'], indent=2).encode())['sha256_ref'],
                wire_journals={'start': start_sha, 'final': finish_sha},
                journal_pass_count=len(phases['final']['passes']),
                producer_inputs={'producer.py': retention.digest(b'producer')})
        ids = sorted(key.split(':', 1)[1] for key in failures if key.startswith('validation:'))
        records = []
        for sequence, (run, row) in enumerate(policy.items(), 1):
            records.append({'sequence': sequence, 'source_commit': row['source'],
                'origin': {'kind': 'authenticated-ci', 'identity': f"run:{run}/job:{row['job']}",
                    'command': None, 'execution': {'mode': 'source-cohort-v1', 'kind': 'validation',
                        'legacy_capture': row['raw_sha256'], 'job_status': row['job_status']}},
                'changes': [], 'log': {'sha256_ref': row['raw_sha256']},
                'coverage': {'mode': 'python-named'}, 'exit_code': None})
        item = {'name': 'validation', 'invocation': continuation.COMMANDS['validation'],
            'protocol': 'source-cohort-v1', 'contract': cohort.CONTRACTS['validation'],
            'inventory': {'ids': ids}, 'records': records, 'obligations': []}
        discovery = lambda source, kind: sorted(key.split(':', 1)[1]
            for key in failures + docs if key.startswith(kind + ':'))
        return item, proof, policy, failures, docs, discovery, raw_logs

    def check_linked_ci_capture_refusals(self):
        item, proof, policy, failures, docs, discovery, raws = self.linked_ci_fixture()
        def validate(value):
            return cohort.validate(value, proof, 'b' * 40, discovery,
                lambda record, candidate, logs: set(record['changes']),
                continuation.clean_log, lambda source: 'c' * 64)
        with mock.patch.object(cohort, 'LINKED_PYTHON_CI_RUNS', policy), \
                mock.patch.object(cohort, 'LINKED_PYTHON_CI_FAILURES', failures), \
                mock.patch.object(cohort, 'LINKED_PYTHON_CI_DOCS', docs), \
                mock.patch.object(cohort, 'LINKED_PYTHON_CI_FAILURE_TOTALS', {'validation': 2, 'operations': 2}):
            pair = cohort.linked_python_ci_pair(proof, discovery)
            self.assertEqual(set(pair[5254]), set(failures))
            self.assertEqual(set(pair[5249]), set(failures + docs))
            self.assertTrue(all(pair[5249][key] == 'fail' for key in failures))
            self.assertFalse(set(docs) & set(pair[5254]))
            first_identity = 'tests.' + failures[0].replace(':', '.', 1)
            parent = failures[0].rsplit('.', 1)[1] + ' (' + first_identity + ') ... '
            detail = 'FAIL: ' + failures[0].rsplit('.', 1)[1] + ' (' + first_identity + ')'
            for changed_raw in (
                raws[5249].replace(detail.encode(), b'FAIL: other (wrong.Owner.test_other)', 1),
                raws[5249].replace((parent + '\n').encode(), b'', 1),
                raws[5249].replace((parent + '\n').encode(), (parent + 'ok\n').encode(), 1),
                raws[5249].replace(b'FAILED (failures=2)', b'FAILED (failures=3)', 1),
            ):
                changed_policy = copy.deepcopy(policy)
                changed_policy[5249]['raw_sha256'] = self.blob(proof, changed_raw)['sha256_ref']
                with mock.patch.object(cohort, 'LINKED_PYTHON_CI_RUNS', changed_policy), \
                        self.assertRaises(receipts.ReceiptError):
                    cohort.linked_python_ci_pair(proof, discovery)
            self.assertEqual(validate(item)['actual_named_pass_count'], 2)
            for mutate in (
                lambda x: x['records'].pop(0),
                lambda x: x['records'].pop(),
                lambda x: x['records'].append(copy.deepcopy(x['records'][0])),
                lambda x: x['records'][0].update(sequence=3),
                lambda x: x['records'][1].update(source_commit='e' * 40),
                lambda x: x['records'][1].update(exit_code=0),
                lambda x: x['records'][1]['origin'].update(identity='run:5254/job:1'),
                lambda x: x['records'][1].update(changes=item['inventory']['ids']),
            ):
                changed = copy.deepcopy(item); mutate(changed)
                with self.subTest(mutation=mutate), self.assertRaises(receipts.ReceiptError):
                    validate(changed)
            for run in policy:
                digest = policy[run]['raw_sha256']
                saved = proof['logs'].pop(digest)
                with self.assertRaises(receipts.ReceiptError):
                    cohort.linked_python_ci_pair(proof, discovery)
                proof['logs'][digest] = {**saved, 'base64': base64.b64encode(raws[run] + b'changed').decode()}
                with self.assertRaises(receipts.ReceiptError):
                    cohort.linked_python_ci_pair(proof, discovery)
                proof['logs'][digest] = saved
            api = mock.Mock()
            runs, jobs = {}, {}
            scope = cohort.LINKED_PYTHON_CI_SCOPE
            for run, row in policy.items():
                pull = {'number': scope['pr'], 'base': {'ref': 'main', 'repo': {'id': 1}},
                        'head': {'ref': scope['branch'], 'sha': row['source'], 'repo': {'id': 1}}}
                runs[run] = {'workflow_id': scope['workflow'], 'prettyref': '#1048',
                    'event': 'pull_request', 'commit_sha': row['source'], 'repository': {'id': 1},
                    'event_payload': {'pull_request': pull, 'number': 1048, 'repository': {'id': 1}}}
                jobs[run] = [{'id': row['job'], 'name': 'fast policy and contract preflight',
                    'attempt': 1, 'run_id': run, 'repo_id': 1, 'task_id': 1, 'status': row['job_status']}]
            live = mock.Mock(); live.get.side_effect = lambda api, run, job: raws[run]
            document = {'pr': 1045, 'branch': 'effort/dv-hdr-repair'}
            def authenticate():
                cohort.authenticate_linked_python_ci(api, proof, item['records'][0], document, 1,
                                                     live, runs, jobs)
            api.get.side_effect = lambda path: runs[int(path.rsplit('/', 1)[1])]
            api.pages.side_effect = lambda path: jobs[int(path.split('/')[-2])]
            with mock.patch.object(retention, 'git_bytes', return_value=b'producer'):
                authenticate()
                authenticated = {'version': 1, 'commands': [item], 'logs': proof['logs']}
                continuation.authenticate(api, authenticated, document, 1, live_logs=live)
                foreign = copy.deepcopy(authenticated)
                foreign['commands'][0]['name'] = 'transport:1'
                with self.assertRaises(receipts.ReceiptError):
                    continuation.authenticate(api, foreign, document, 1, live_logs=live)
                for obj, key, value in [(document, 'pr', 1046), (document, 'branch', 'main'),
                        (runs[5254], 'commit_sha', 'e' * 40), (jobs[5254][0], 'status', 'failure'),
                        (jobs[5254][0], 'attempt', 2),
                        (runs[5254]['event_payload'], 'number', 999)]:
                    original = obj[key]; obj[key] = value
                    with self.subTest(field=key), self.assertRaises(receipts.ReceiptError):
                        authenticate()
                    obj[key] = original
                live.get.side_effect = lambda api, run, job: raws[run] + b'changed'
                with self.assertRaises(receipts.ReceiptError):
                    authenticate()

    def test_finite_legacy_capture_cannot_authorize_different_log_or_source(self):
        item, proof, ids, raw, _ = self.fixture()
        record = item['records'][1]
        record['origin']['execution'] = {'mode': 'source-cohort-v1', 'kind': 'validation',
                                         'legacy_capture': next(iter(cohort.LEGACY_PYTHON))}
        with self.assertRaises(receipts.ReceiptError):
            self.validate(item, proof, ids, raw)
        self.check_linked_ci_capture_refusals()


if __name__ == '__main__':
    unittest.main()
