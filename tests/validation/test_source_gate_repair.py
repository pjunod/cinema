"""Closed source-gate repair custody and exact execution framing."""

import copy
import io
import json
from pathlib import Path
import unittest
from unittest import mock
import zipfile

from validation import paperwork_continuation as paperwork
from validation import source_gate_landing as landing
from validation import source_gate_repair as repair
from validation.python_unit_receipts import ReceiptError

SOURCE = 'a' * 40
BASE = 'b' * 40
CURRENT = 'c' * 40


class GateRepairCase(unittest.TestCase):
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
        self.assertEqual(repair.prior_run(api, value, binding, 6, current_run=5)[1]['head_sha'], SOURCE)
        with self.assertRaises(ReceiptError):
            repair.prior_run(api, value, binding, 6, current_run=4)
        for key in event['inputs']:
            changed = copy.deepcopy(event)
            changed['inputs'][key] = 'foreign'
            api.get.return_value = {**original, 'event_payload': json.dumps(changed)}
            with self.subTest(input=key), self.assertRaises(ReceiptError):
                repair.prior_run(api, value, binding, 6, current_run=5)

    def test_source_delta_refuses_any_runtime_change_even_with_the_same_workflow(self):
        workflow = b'original pinned commands'
        changes = repair.REPAIR_PATHS
        corrected = 'validation/jvm_unit_discovery.py'
        expected = repair.retention.digest(workflow)
        with mock.patch.object(paperwork, 'ancestor', return_value=True), \
             mock.patch.object(landing, 'WORKFLOW_SHA256', expected), \
             mock.patch.object(repair, 'REPAIR_WORKFLOW_SHA256', expected), \
             mock.patch.object(repair, 'REVIEWED_CORRECTIONS', {corrected: (expected, expected)}), \
             mock.patch.object(repair.retention, 'git_bytes') as source, \
             mock.patch.object(repair.retention, 'changed', return_value=changes) as delta:
            old_paths = {corrected, '.github/workflows/ci.yml'}
            source.side_effect = lambda revision, path: workflow if revision == CURRENT or path in old_paths else None
            self.assertEqual(set(repair.source_delta(Path('.'), SOURCE, CURRENT)), changes)
            delta.return_value = changes | {'crates/plurxd/src/lib.rs'}
            with self.assertRaises(ReceiptError):
                repair.source_delta(Path('.'), SOURCE, CURRENT)
            delta.return_value = changes
            source.side_effect = lambda revision, path: b'changed correction' if revision == CURRENT and path == corrected else (
                workflow if revision == CURRENT or path in old_paths else None)
            with self.assertRaises(ReceiptError):
                repair.source_delta(Path('.'), SOURCE, CURRENT)

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
                 'predecessor_run': 4, 'android_snapshot': snapshot}
        self.assertEqual(paperwork.enrollment(paperwork.PREFIX + json.dumps(value)), value)
        for field in ('predecessor_run', 'android_snapshot'):
            with self.subTest(field=field), self.assertRaises(ReceiptError):
                paperwork.enrollment(paperwork.PREFIX + json.dumps({k: v for k, v in value.items() if k != field}))
        with self.assertRaises(ReceiptError):
            paperwork.enrollment(paperwork.PREFIX + json.dumps({**value, 'python_unit_evidence': snapshot}))


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
