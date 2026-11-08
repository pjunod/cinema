"""Native command receipts retain success at the granularity actually logged."""
import copy
import base64
import hashlib
import unittest
from unittest.mock import patch
from validation import promotion_unit_retention as retention
from validation import retained_lane_commands as native
from validation.python_unit_receipts import ReceiptError

PRIOR = 'a' * 40
CANDIDATE = 'b' * 40
DEVICE = '[command]/usr/bin/sh -c ' + native.DEVICE_COMMAND + '\nTime: 1.5\nOK (85 tests)\n🏁  Job succeeded\n'
JVM = native.JVM_ARGV + '\n> Task :app:testDebugUnitTest\n> Task :app:lintDebug\nBUILD SUCCESSFUL in 1s\n🏁  Job succeeded\n'
WEB = '''node: v22.23.4
ok - custom contract
PASS source assertion
TAP version 13
# Subtest: native contract
ok 1 - native contract
  ---
  duration_ms: 1.2
  type: 'test'
  ...
1..1
# tests 1
# suites 0
# pass 1
# fail 0
# cancelled 0
# skipped 0
# todo 0
# duration_ms 1.2
⭐ Run Post cleanup
🏁  Job succeeded
'''
WORKFLOW = ('''name: ci
on:
  workflow_dispatch:
    inputs:
      promotion_pr:
        type: string
env:
  CARGO_TERM_COLOR: always
concurrency:
  group: ci-${{ github.ref }}
jobs:
  preflight:
    runs-on: general
    steps:
      - uses: setup-node@pinned
        with:
          node-version: "22.23.4"
      - run: |
''' + ''.join('            node ' + path + '\n' for path in native.WEB_SCRIPTS)
    + '''  android_jvm:
    runs-on: general
    steps:
      - run: |
''' + '            ' + native.JVM_COMMAND + '\n'
    + '''  android_device:
    runs-on: emulator
    steps:
      - run: |
''' + ('            ' + native.DEVICE_COMMAND + '\n') * 2
    + '''  rust:
    runs-on: general
    steps:
      - run: make ci-rust-gate
''')


def source(commit, path):
    if path == native.WEB_SCRIPTS[0]:
        return b'test("custom contract", () => {});\n'
    if path == native.WEB_SCRIPTS[1]:
        return b'process.stdout.write("PASS source assertion\\n");\ntest("native contract", () => {});\n'
    return WORKFLOW.encode() if path == '.github/workflows/ci.yml' else b'unchanged input'


def fixture(fmt):
    raw, lane, name, command = {
        native.DEVICE_FORMAT: (DEVICE, 'android_device', 'device', native.DEVICE_COMMAND),
        native.JVM_FORMAT: (JVM, 'android_jvm', 'jvm', native.JVM_COMMAND),
        native.WEB_FORMAT: (WEB, 'web_preflight', 'node-contracts', native.WEB_COMMAND),
    }[fmt]
    outcomes = retention.log_outcomes(raw, fmt, source_commit=PRIOR, source=source)
    env = native.environment(raw, fmt)
    record = {'format': fmt, 'sequence': 1, 'source_commit': PRIOR,
              'origin': {'kind': 'authenticated-ci', 'identity': 'run:12/job:34', 'command': command},
              'environment': env, 'outcomes': outcomes}
    suite = {'name': name, 'ids': list(outcomes), 'records': [record], 'environment': env}
    return raw, lane, suite, record


class NativeCommandCase(unittest.TestCase):
    def test_commands_preserve_whole_command_accounting_and_actual_node_cases(self):
        self.assertEqual(native.device_summary(DEVICE), 85)
        self.assertEqual(native.jvm_outcomes(JVM), {'make android-test': 'pass'})
        self.assertEqual(native.web_outcomes(WEB, PRIOR, source), dict.fromkeys(native.WEB_SCRIPTS, 'pass'))
        for fmt in native.FORMATS:
            raw, lane, suite, record = fixture(fmt)
            with self.subTest(format=fmt), patch.object(retention, 'android_device_inputs', return_value={'input': 'same'}), patch.object(native, 'tree_id', return_value='same'):
                native.record_contract(lane, suite, record, CANDIDATE, raw, source)

    def test_device_rejects_missing_duplicate_failed_or_truncated_execution(self):
        for raw in (DEVICE.replace('OK (85 tests)', ''), DEVICE.replace('OK (85 tests)', 'OK (0 tests)'),
                    DEVICE + 'OK (85 tests)\n', DEVICE.replace('🏁  Job succeeded', '🏁 Job failed'),
                    DEVICE.replace('Time:', 'FAILURES!!!\nTime:'), DEVICE + 'untrusted result\n',
                    DEVICE.replace(native.DEVICE_COMMAND, 'echo ' + native.DEVICE_COMMAND)):
            with self.subTest(raw=raw), self.assertRaises(ReceiptError):
                native.device_summary(raw)

    def test_jvm_rejects_cache_hits_missing_tasks_failure_or_other_argv(self):
        for raw in (JVM.replace('> Task :app:testDebugUnitTest', '> Task :app:testDebugUnitTest UP-TO-DATE'),
                    JVM.replace('> Task :app:lintDebug\n', ''), JVM.replace('BUILD SUCCESSFUL', 'BUILD FAILED'),
                    JVM.replace(native.JVM_ARGV, native.JVM_ARGV + ' --tests Selected'),
                    JVM + 'BUILD SUCCESSFUL in 2s\n'):
            with self.subTest(raw=raw), self.assertRaises(ReceiptError):
                native.jvm_outcomes(raw)

    def test_node_rejects_missing_cases_skips_contradictions_and_false_summary(self):
        for raw in (WEB.replace('ok - custom contract\n', ''), WEB.replace('PASS source assertion\n', ''),
                    WEB.replace('ok 1 - native contract', 'not ok 1 - native contract'),
                    WEB.replace('# Subtest: native contract', '# Subtest: another contract'),
                    WEB.replace('# fail 0', '# fail 1'), WEB.replace('# skipped 0', '# skipped 1'),
                    WEB.replace('# pass 1', '# pass 1\n# pass 0'), WEB.replace('1..1', '1..2')):
            with self.subTest(raw=raw), self.assertRaises(ReceiptError):
                native.web_outcomes(raw, PRIOR, source)

    def test_source_contract_rejects_lane_scope_origin_environment_and_input_changes(self):
        for fmt in native.FORMATS:
            raw, lane, suite, record = fixture(fmt)
            for field, value in (('outcomes', {'invented': 'pass'}), ('sequence', 2),
                                 ('origin', {**record['origin'], 'command': 'echo success'})):
                altered = copy.deepcopy(record)
                altered[field] = value
                with self.subTest(format=fmt, field=field), self.assertRaises(ReceiptError):
                    native.record_contract(lane, suite, altered, CANDIDATE, raw, source)
            changed = lambda commit, path: b'changed' if commit == CANDIDATE and path == '.github/workflows/ci.yml' else source(commit, path)
            with self.subTest(format=fmt), patch.object(retention, 'android_device_inputs', return_value={'input': 'same'}), patch.object(native, 'tree_id', return_value='same'), self.assertRaises(ReceiptError):
                native.record_contract(lane, suite, record, CANDIDATE, raw, changed)
        raw, lane, suite, record = fixture(native.WEB_FORMAT)
        with patch.object(native, 'tree_id', side_effect=['old', 'new']), self.assertRaises(ReceiptError):
            native.record_contract(lane, suite, record, CANDIDATE, raw, source)
        raw, lane, suite, record = fixture(native.DEVICE_FORMAT)
        with patch.object(retention, 'android_device_inputs', side_effect=[{'input': 'old'}, {'input': 'new'}]), self.assertRaises(ReceiptError):
            native.record_contract(lane, suite, record, CANDIDATE, raw, source)

    def test_authentication_requires_canonical_workflow_job_success_and_first_attempt(self):
        for fmt, name in ((native.DEVICE_FORMAT, 'Android instrumented UI tests'),
                          (native.JVM_FORMAT, 'Android JVM tests and lint'),
                          (native.WEB_FORMAT, 'fast policy and contract preflight')):
            job = {'name': name, 'status': 'success', 'attempt': 1}
            document = {'pr': 2, 'branch': 'effort/test'}
            record = {'format': fmt, 'source_commit': PRIOR}
            run = {'workflow_id': 'ci.yml', 'trigger_event': 'workflow_dispatch', 'commit_sha': PRIOR,
                   'event_payload': '{"ref":"refs/heads/effort/test","inputs":{"promotion_pr":"2","promotion_head_sha":"' + PRIOR + '"}}'}
            native.authenticate(run, job, record, document)
            for replacement in ({'status': 'failure'}, {'name': 'unrelated job'}, {'attempt': 2}, {'attempt': True}):
                with self.subTest(format=fmt, replacement=replacement), self.assertRaises(ReceiptError):
                    native.authenticate(run, {**job, **replacement}, record, document)
            with self.assertRaises(ReceiptError):
                native.authenticate({**run, 'workflow_id': 'unrelated.yml'}, job, record, document)
            for key, value in (('trigger_event', 'push'), ('commit_sha', CANDIDATE),
                               ('event_payload', run['event_payload'].replace('effort/test', 'effort/unrelated')),
                               ('event_payload', run['event_payload'].replace('\"2\"', '\"3\"'))):
                with self.subTest(format=fmt, key=key), self.assertRaises(ReceiptError):
                    native.authenticate({**run, key: value}, job, record, document)


    def test_full_retention_model_exposes_command_counts_and_unknown_named_outcomes(self):
        def blob(text):
            data = text.encode()
            return {'base64': base64.b64encode(data).decode(), 'sha256': hashlib.sha256(data).hexdigest()}
        for fmt in native.FORMATS:
            raw, lane, suite, record = fixture(fmt)
            record.update(changes={}, log=blob(raw))
            suite['inventory'] = blob('\n'.join(suite['ids']) + '\n')
            proof = {'version': 1, 'repository': 1, 'pr': 2, 'branch': 'effort/test',
                     'base_sha': PRIOR, 'source_commit': CANDIDATE, 'windows_waiver': None,
                     'lanes': {lane: [suite]}}
            with self.subTest(format=fmt), patch.object(retention, 'android_device_inputs', return_value={'input': 'same'}), patch.object(native, 'tree_id', return_value='same'):
                adopted = retention.validate(proof, CANDIDATE, source=source, differences=lambda a, b: set())
                self.assertEqual(adopted[lane]['accounting'], 'whole-command-success')
                self.assertIsNone(adopted[lane]['named_body_outcomes'])
                self.assertIsNone(adopted[lane]['passed_body_count'])
                self.assertEqual(adopted[lane]['commands_passed'], len(suite['ids']))
                if fmt == native.DEVICE_FORMAT:
                    self.assertEqual(adopted[lane]['reported_tests'], 85)


    def test_jvm_rejects_shared_fixture_changes_even_when_android_source_is_identical(self):
        raw, lane, suite, record = fixture(native.JVM_FORMAT)
        for changed in ('tests/contracts', 'tests/playback', 'tests/sharing',
                        'crates/plurx-core/tests/fixtures'):
            tree = lambda commit, path: 'new' if commit == CANDIDATE and path == changed else 'same'
            with self.subTest(path=changed), patch.object(retention, 'android_device_inputs', return_value={'input': 'same'}), patch.object(native, 'tree_id', side_effect=tree), self.assertRaises(ReceiptError):
                native.record_contract(lane, suite, record, CANDIDATE, raw, source)


class NativeJvmResourceCommentCase(unittest.TestCase):
    def test_only_reviewed_playback_trees_with_identical_catalog_data_are_equivalent(self):
        old, new = native.JVM_PLAYBACK_COMMENT_TREES
        for current_tree, path, data, accepted in (
            (new, 'tests/playback', b'# attribution\ncount=2080', True),
            (new, 'tests/playback', b'count=2081', False),
            ('another-tree', 'tests/playback', b'count=2080', False),
            (new, 'tests/contracts', b'count=2080', False),
        ):
            with self.subTest(tree=current_tree, path=path, data=data):
                with patch.object(native, 'tree_id', side_effect=[old, current_tree]):
                    self.assertEqual(native.same_jvm_fixture_tree(
                        PRIOR, CANDIDATE, path, lambda ref, _: b'count=2080' if ref == PRIOR else data),
                        accepted)


class NativeWorkflowScopeCase(unittest.TestCase):
    def test_unrelated_rust_job_and_exact_descriptor_preserve_native_scopes(self):
        changed = WORKFLOW.replace('      promotion_pr:', native.RUST_DESCRIPTOR_INPUT + '      promotion_pr:')
        changed = changed.replace('run: make ci-rust-gate', 'run: python3 -m validation.workspace_rust_execute')
        for job in ('preflight', 'android_jvm', 'android_device'):
            with self.subTest(job=job):
                self.assertEqual(native.scoped_workflow(WORKFLOW.encode(), job),
                                 native.scoped_workflow(changed.encode(), job))
        self.assertEqual(native.android_source(lambda commit, path: changed.encode())('x', '.github/workflows/ci.yml'),
                         changed.replace(native.RUST_DESCRIPTOR_INPUT, '').encode())

    def test_consumed_executor_globals_or_unrecognized_descriptor_remain_bound(self):
        for job, old, new in (('preflight', 'setup-node@pinned', 'setup-node@untrusted'),
                              ('android_jvm', native.JVM_COMMAND, 'echo skipped'),
                              ('android_device', 'runs-on: emulator', 'runs-on: other'),
                              ('preflight', 'CARGO_TERM_COLOR: always', 'CARGO_TERM_COLOR: never'),
                              ('preflight', 'promotion_pr:', 'other_input:')):
            with self.subTest(job=job, change=old):
                self.assertNotEqual(native.scoped_workflow(WORKFLOW.encode(), job),
                                    native.scoped_workflow(WORKFLOW.replace(old, new).encode(), job))
        unknown = native.RUST_DESCRIPTOR_INPUT.replace('required: false', 'required: true')
        changed = WORKFLOW.replace('      promotion_pr:', unknown + '      promotion_pr:')
        self.assertNotEqual(native.scoped_workflow(WORKFLOW.encode(), 'preflight'),
                            native.scoped_workflow(changed.encode(), 'preflight'))

    def test_real_native_contract_keeps_pass_after_unrelated_rust_changes(self):
        changed = WORKFLOW.replace('      promotion_pr:', native.RUST_DESCRIPTOR_INPUT + '      promotion_pr:')
        changed = changed.replace('run: make ci-rust-gate', 'run: python3 -m validation.workspace_rust_execute')
        def revised(commit, path):
            if commit == CANDIDATE and path == '.github/workflows/ci.yml':
                return changed.encode()
            return source(commit, path)
        for fmt in native.FORMATS:
            raw, lane, suite, record = fixture(fmt)
            with self.subTest(format=fmt), patch.object(retention, 'android_device_tree', return_value='same'), patch.object(native, 'tree_id', return_value='same'):
                # A valid Make recipe is unnecessary here: pin the returned
                # projection while exercising the actual normalized reader.
                def inputs(reader, commit):
                    return native.scoped_workflow(reader(commit, '.github/workflows/ci.yml'), 'android_device')
                with patch.object(retention, 'android_device_inputs', side_effect=inputs):
                    native.record_contract(lane, suite, record, CANDIDATE, raw, revised)


class NativePythonTransitionCase(unittest.TestCase):
    def test_only_reviewed_whole_preflight_transition_preserves_node_scope(self):
        before = native.scoped_workflow(WORKFLOW.encode(), 'preflight')[-1]
        changed = WORKFLOW.replace('      promotion_pr:', native.PYTHON_DESCRIPTOR_INPUT + '      promotion_pr:')
        changed = changed.replace('      - uses: setup-node@pinned',
            '      - name: Prepare scoped Python selection\n        run: prepare-exact\n      - uses: setup-node@pinned')
        after = native.scoped_workflow(changed.encode(), 'preflight')[-1]
        digests = (native.retention_digest(before), native.retention_digest(after))
        with patch.object(native, 'NODE_PREFLIGHT_TRANSITION', digests):
            self.assertEqual(native.scoped_workflow(WORKFLOW.encode(), 'preflight'),
                             native.scoped_workflow(changed.encode(), 'preflight'))
            for old, new in (('prepare-exact', 'arbitrary-command'),
                             ('setup-node@pinned', 'setup-node@untrusted'),
                             ('runs-on: general', 'runs-on: other')):
                with self.subTest(change=old):
                    self.assertNotEqual(native.scoped_workflow(WORKFLOW.encode(), 'preflight'),
                        native.scoped_workflow(changed.replace(old, new).encode(), 'preflight'))
            ambiguous = changed.replace('      promotion_pr:', native.PYTHON_DESCRIPTOR_INPUT + '      promotion_pr:')
            with self.assertRaises(ReceiptError):
                native.scoped_workflow(ambiguous.encode(), 'preflight')
