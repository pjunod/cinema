"""Closed source-gate repair custody and exact execution framing."""

import copy
import io
import json
from pathlib import Path
import unittest
from unittest import mock
import zipfile

from validation import paperwork_continuation as paperwork
from validation import source_gate_repair as repair
from validation.python_unit_receipts import ReceiptError

SOURCE = 'a' * 40
BASE = 'b' * 40
CURRENT = 'c' * 40


class GateRepairCase(unittest.TestCase):
    def test_default_manual_workflow_still_admits_exact_old_and_current_recipes(self):
        from validation import source_gate_landing as landing

        original = repair.retention.git_bytes(repair.PREDECESSOR['head'], '.github/workflows/ci.yml')
        current = (Path(__file__).resolve().parents[2] / '.github/workflows/ci.yml').read_bytes()
        landing.require_manual_workflow(original)
        landing.require_manual_workflow(current)
        altered = current.replace(b'python3 -m validation.native_python_execute prepare', b'false', 1)
        self.assertNotEqual(altered, current)
        with self.assertRaises(ReceiptError):
            landing.require_manual_workflow(altered)

    def test_android_instrumentation_refuses_a_changed_emulator_action(self):
        import subprocess

        previous = repair.PREDECESSOR['head']
        current = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
        self.assertIn('.github/actions/android-emulator-ready',
                      repair.android_device_source_projection(previous, current))
        identify = repair.retention.android_device_tree

        def changed(revision, path):
            if revision == current and path == '.github/actions/android-emulator-ready':
                return 'foreign emulator action'
            return identify(revision, path)

        with mock.patch.object(repair.retention, 'android_device_tree', side_effect=changed):
            with self.assertRaises(ReceiptError):
                repair.android_device_source_projection(previous, current)

    def test_android_compile_lint_retains_only_completed_tasks_before_the_metadata_failure(self):
        lines = [
            SOURCE,
            'Digest: sha256:' + 'd' * 64,
            repair.ANDROID_COMPILE_ARGV,
            *repair.ANDROID_COMPILE_TASKS,
            'BUILD SUCCESSFUL in 1m 15s',
            'File "/usr/lib/python3.10/runpy.py", line 196, in _run_module_as_main',
            "ids = verify_adopted(Path.cwd(), proof['command_units']['android_jvm'])",
            'import tomllib',
            repair.ANDROID_VERIFY_ERROR,
            '🏁  Job failed',
        ]
        raw = '\n'.join(lines).encode()
        self.assertEqual(repair.android_compile_lint(raw, SOURCE)['test_bodies'], 0)
        for match, replacement in (
            (SOURCE, BASE),
            (repair.ANDROID_COMPILE_TASKS[-1], repair.ANDROID_COMPILE_TASKS[-1] + ' SKIPPED'),
            ('BUILD SUCCESSFUL in 1m 15s', 'BUILD FAILED in 1m 15s'),
            (repair.ANDROID_VERIFY_ERROR, 'ModuleNotFoundError: No module named something_else'),
            ('import tomllib', '> Task :app:testDebugUnitTest\nimport tomllib'),
        ):
            with self.subTest(match=match), self.assertRaises(ReceiptError):
                repair.android_compile_lint(raw.replace(match.encode(), replacement.encode()), SOURCE)

    def test_compiled_snapshot_refuses_traversal_and_changed_original_bytes(self):
        def snapshot(path, raw):
            data = io.BytesIO()
            with zipfile.ZipFile(data, 'w') as archive:
                archive.writestr('classes/' + path, raw)
                archive.writestr(repair.ROSTER_JAR, b'actual runtime fixture')
            return data.getvalue()

        name = "fixture/Test$test's method$1.class"
        actual = b'actual class bytes'
        hashed = {name: repair.retention.digest(actual)}
        self.assertEqual(repair.compiled_members(snapshot(name, actual), hashed)[0], {name: actual})
        with self.assertRaises(ReceiptError):
            repair.compiled_members(snapshot(name, b'different class'), hashed)
        foreign = '../Outside.class'
        with self.assertRaises(ReceiptError):
            repair.compiled_members(snapshot(foreign, actual), {foreign: repair.retention.digest(actual)})

    def test_predecessor_is_an_earlier_actual_manual_gate_for_this_pr_head_base_and_branch(self):
        event = {'ref': 'refs/heads/codex/repair', 'inputs': {
            'promotion_pr': '17', 'promotion_head_sha': SOURCE, 'promotion_base_sha': BASE}}
        original = {'id': 4, 'workflow_id': 'ci.yml', 'status': 'failure', 'repository': {'id': 6},
                    'trigger_event': 'workflow_dispatch', 'commit_sha': SOURCE, 'prettyref': 'codex/repair',
                    'event_payload': json.dumps(event)}
        binding = {'head_ref': 'codex/repair', 'head_sha': CURRENT, 'base_sha': BASE, 'pull_request': 17}
        value = {'predecessor_run': 4}
        api = mock.Mock()
        api.get.return_value = original
        reviewed = {'run': 4, 'head': SOURCE, 'base': BASE}
        with mock.patch.object(repair, 'PREDECESSOR', reviewed):
            new_base = 'e' * 40
            binding['base_sha'] = new_base
            _, old, _ = repair.prior_run(api, value, binding, 6, current_run=5)
            self.assertEqual((old['head_sha'], old['base_sha']), (SOURCE, BASE))
            with self.assertRaises(ReceiptError):
                repair.prior_run(api, value, binding, 6, current_run=4)
            for key in event['inputs']:
                changed = copy.deepcopy(event)
                changed['inputs'][key] = 'foreign'
                api.get.return_value = {**original, 'event_payload': json.dumps(changed)}
                with self.subTest(input=key), self.assertRaises(ReceiptError):
                    repair.prior_run(api, value, binding, 6, current_run=5)

    def test_python_projection_keeps_only_applicable_positive_old_methods(self):
        from validation import main_python_continuation as main

        old = {'head_sha': SOURCE}
        binding = {'head_sha': CURRENT}
        same, changed, imported, new = ('case.Case.test_' + x for x in ('same','changed','imported','new'))
        older = {'commit': 'f' * 40, 'run': 2}
        successes = {'direct_passes': ['validation:' + changed, 'validation:' + imported],
                     'retained_passes': {'validation:' + same: older}}
        old_inventory = {'validation': [same, changed, imported], 'operations': []}
        inventory = {'validation': [same, changed, imported, new], 'operations': []}
        with mock.patch.object(main, 'origin_impacts', return_value={'validation:' + imported}), \
             mock.patch.object(repair.receipts, 'SourceApplicability') as applicability:
            applicability.return_value.matches.side_effect = lambda key, _: key == 'validation:' + same
            retained, due = repair.python_selection(old, binding, successes, old_inventory, inventory, {})
            self.assertEqual(retained, {'validation:' + same: older})
            self.assertEqual(due, ['validation:' + changed, 'validation:' + imported, 'validation:' + new])
            with self.assertRaises(ReceiptError):
                repair.python_selection(old, binding, {**successes, 'direct_passes': []}, old_inventory, inventory, {})

    def test_current_repair_framing_requires_every_selected_positive_once(self):
        selected = 'test_source_gate_repair.GateRepairCase.test_case'
        first = repair.canonical({'kind': 'validation', 'ids': [selected]}).decode()
        second = repair.canonical({'kind': 'operations', 'ids': []}).decode()
        raw = '\n'.join((
            'SOURCE-GATE-REPAIR-TESTS-BEGIN ' + first, 'test_case (' + selected + ') ... ok',
            'Ran 1 test in 0.01s', 'OK', 'SOURCE-GATE-REPAIR-TESTS-END ' + first,
            'SOURCE-GATE-REPAIR-TESTS-BEGIN ' + second, 'SOURCE-GATE-REPAIR-TESTS-END ' + second,
        )).encode()
        due = ['validation:' + selected]
        self.assertEqual(repair.current_repair_tests(raw, due), due)
        with self.assertRaises(ReceiptError):
            repair.current_repair_tests(raw.replace(b'... ok', b'... skipped'), due)
        with self.assertRaises(ReceiptError):
            repair.current_repair_tests(raw[:-5], due)

    def test_repair_enrollment_is_closed_and_cannot_use_the_ordinary_python_descriptor(self):
        snapshot = {'sha256': '1' * 64, 'attachment_id': 3, 'attachment_uuid': 'proof-id'}
        value = {'version': 2, 'pr': 17, 'base': BASE, 'head': CURRENT,
                 'predecessor_run': 4, 'android_snapshot': snapshot, 'python_impact': snapshot,
                 'workspace_rust_evidence': snapshot}
        self.assertEqual(paperwork.enrollment(paperwork.PREFIX + json.dumps(value)), value)
        for field in ('predecessor_run', 'android_snapshot', 'python_impact', 'workspace_rust_evidence'):
            with self.subTest(field=field), self.assertRaises(ReceiptError):
                paperwork.enrollment(paperwork.PREFIX + json.dumps({k: v for k, v in value.items() if k != field}))
        with self.assertRaises(ReceiptError):
            paperwork.enrollment(paperwork.PREFIX + json.dumps({**value, 'python_unit_evidence': snapshot}))


    def test_dispatched_continuation_refuses_an_extra_provider_or_different_rust_descriptor(self):
        descriptor = {'sha256': 'a' * 64, 'attachment_id': 2, 'attachment_uuid': 'artifact'}
        binding = {'pull_request': 17, 'head_sha': CURRENT, 'base_sha': BASE}
        value = {'predecessor_run': 4, 'workspace_rust_evidence': descriptor}
        inputs = {'promotion_pr': '17', 'promotion_head_sha': CURRENT, 'promotion_base_sha': BASE,
                  'predecessor_run': '4', 'workspace_rust_evidence': json.dumps(descriptor),
                  'python_unit_evidence': ''}
        repair.dispatch_inputs(inputs, binding, value)
        for field, replacement in (('predecessor_run', ''), ('python_unit_evidence', 'another provider'),
                                   ('workspace_rust_evidence', json.dumps({**descriptor, 'attachment_id': 7}))):
            with self.subTest(field=field), self.assertRaises(ReceiptError):
                repair.dispatch_inputs({**inputs, field: replacement}, binding, value)


    def test_only_the_named_bounded_fixture_diagnostic_recovers_a_split_predecessor_result(self):
        identity = ('test_nextest_shard_continuation.NextestShardContinuationTests.'
                    'test_pidfd_capability_refuses_enosys_before_any_body')
        prefix = f"{identity.rsplit('.', 1)[1]} ({identity}) ... "
        raw = (prefix + 'Nextest kernel: 7.3.0-test\nok\n').encode()
        self.assertEqual(repair.predecessor_python_lines(raw), (prefix + 'ok\n').encode())
        ambiguous = raw.replace(b'\nok', b'\ntest_foreign (case.Case.test_foreign) ... ok\nok')
        self.assertEqual(repair.predecessor_python_lines(ambiguous), ambiguous)
        with self.assertRaises(ReceiptError):
            repair.predecessor_python_lines(raw + raw)

    def test_wal_exact_and_grouped_commands_bind_each_actual_count_and_name(self):
        recipes = [
            '$(CARGO) test --locked -p fixture --lib sample::tests::exact -- --exact',
            'PLURX_EXPECT_TEST_COUNT=2 $(CARGO) test --locked -p fixture --lib watch_tests',
        ]
        raw = ('running 1 test\n'
               'test sample::tests::exact ... ok\n'
               'test result: ok. 1 passed; 0 failed; 0 ignored; 2 filtered out; finished in 0.01s\n'
               'running 2 tests\n'
               'test cache::watch_tests::first ... ok\n'
               'test cache::watch_tests::second ... ok\n'
               'test result: ok. 2 passed; 0 failed; 0 ignored; 1 filtered out; finished in 0.02s\n').encode()
        self.assertEqual(repair.wal_coverage(raw, recipes), [
            {'selector': 'sample::tests::exact', 'passed': ['sample::tests::exact']},
            {'selector': 'watch_tests', 'passed': ['cache::watch_tests::first', 'cache::watch_tests::second']},
        ])
        for modified in (raw.replace(b'watch_tests::second', b'other::second'),
                         raw.replace(b'2 passed; 0 failed', b'1 passed; 0 failed'),
                         raw + b'test result: ok. 1 passed; 0 failed; 0 ignored;\n'):
            with self.subTest(raw=modified), self.assertRaises(ReceiptError):
                repair.wal_coverage(modified, recipes)

    def test_formal_source_rebind_accepts_only_the_exact_reviewed_projection(self):
        path, (before, after) = next(iter(repair.FORMAL_SOURCE_REFINEMENTS['durable-jobs'].items()))
        document = {'input_sha256': {path: before}}
        with mock.patch.object(repair.retention, 'git_bytes', return_value=b'actual renamed panic message'), \
             mock.patch.object(repair.retention, 'digest', return_value=after) as digest:
            self.assertEqual(repair.formal_input_projection('durable-jobs', document, CURRENT),
                             ({path: after}, {path: (before, after)}))
            digest.return_value = 'f' * 64
            with self.assertRaises(ReceiptError):
                repair.formal_input_projection('durable-jobs', document, CURRENT)
