"""Exercise exact prior Node selection, current outcomes, and the root custody proof."""
import json
from pathlib import Path
import unittest
from unittest.mock import Mock, patch

from validation import python_unit_receipts as r
from validation import selected_node_preflight as node
from validation import selected_root_custody as root

SOURCE = 'a' * 40
CURRENT = 'b' * 40
DESCRIPTOR = {'version': node.POLICY, 'pr': 17, 'run': 9, 'job': 12, 'source_commit': SOURCE, 'artifact': 14}


class SelectedNodePreflightTests(unittest.TestCase):
    def plan(self, records):
        first, second = node.WEB_SCRIPTS

        def digest(revision, script):
            if revision == CURRENT and script == f'node:{second}':
                return 'd' * 64
            return 'e' * 64

        with patch.object(node.retention, 'git_bytes', return_value=b'{"families":[]}'), \
                patch.object(node.journal, 'witness', side_effect=lambda identity, _: identity), \
                patch.object(node.journal, 'input_digest', side_effect=digest):
            return node.plan_for_records(DESCRIPTOR, records, CURRENT)

    def records(self):
        return {'node:' + script: {
            'run': 9, 'job': 12, 'attempt': 1, 'commit': SOURCE, 'environment': dict(node.ENVIRONMENT),
            'outcome': 'success', 'inputs': 'e' * 64,
        } for script in node.WEB_SCRIPTS}

    def test_changed_digest_runs_only_its_program_and_keeps_actual_origin(self):
        first, second = node.WEB_SCRIPTS
        records = self.records()
        selected = self.plan(records)
        self.assertEqual(selected['retained'], {first: records['node:' + first]})
        self.assertEqual(selected['uncovered'], {second: 'd' * 64})
        plan = {'source_commit': CURRENT, 'evidence_sha256': 'c' * 64, 'node_preflight': selected}
        with patch.object(node.journal, 'environment', return_value=node.ENVIRONMENT), \
                patch.object(node, 'emit'), patch.object(node.subprocess, 'run') as execute:
            node.execute(plan)
        execute.assert_called_once_with(['node', second], check=True)

    def test_prior_selection_refuses_missing_foreign_inherited_and_bad_digest_records(self):
        identity = 'node:' + node.WEB_SCRIPTS[0]
        for change in (None, {'job': 13}, {'commit': CURRENT}, {'inputs': 'f' * 64},
                       {'outcome': 'skipped'}, {'environment': {'node': 'v25.1.0'}}):
            records = self.records()
            if change is None:
                del records[identity]
            else:
                records[identity].update(change)
            with self.subTest(change=change), self.assertRaises(r.ReceiptError):
                self.plan(records)
        with self.assertRaises(r.ReceiptError):
            node.prepare(Mock(), DESCRIPTOR, [], CURRENT, 1)

    def test_successful_job_requires_exact_direct_current_node_results(self):
        selection = self.plan(self.records())
        plan = {'source_commit': CURRENT, 'evidence_sha256': 'c' * 64, 'node_preflight': selection}
        identity = json.dumps(node.marker(plan))
        _, second = node.WEB_SCRIPTS
        current = json.dumps({'script': second, 'input_sha256': 'd' * 64})
        raw = (node.BEGIN + identity + '\n' + node.PASS + current + '\n' + node.END + identity + '\n').encode()
        self.assertEqual(node.coverage(raw, plan)['current_passes'], {second: 'd' * 64})
        for broken in (b'', raw.replace((node.PASS + current + '\n').encode(), b''),
                       raw.replace(second.encode(), b'unrelated.js'), raw + raw):
            with self.subTest(broken=broken), self.assertRaises(r.ReceiptError):
                node.coverage(broken, plan)

    def test_root_custody_rejects_any_missing_positive_or_changed_actual_origin_input(self):
        name = root.ROOT_CASE.rsplit('.', 1)[-1]
        test_source = ('@unittest.skipUnless(os.geteuid() == 0, "requires root")\n'
                       + f'def {name}(self): pass\n').encode()
        root_case = 'operations:' + root.ROOT_CASE
        peer = root.MODULE + 'RecoveryHelperTests.test_peer'
        records = {key: {'run': 9, 'commit': SOURCE} for key in (root_case, 'operations:' + peer)}
        suite = {'inventory': [peer, root.ROOT_CASE], 'retained_source_records': records}

        def files(revision, path):
            if path == root.INPUTS[0]:
                return test_source
            return b'actual helper' if revision == CURRENT else b'old helper'

        with patch.object(root.retention, 'git_bytes', side_effect=files), self.assertRaises(r.ReceiptError):
            root.prepare(suite, CURRENT)
        with patch.object(root.retention, 'git_bytes', return_value=test_source):
            self.assertEqual(set(root.prepare(suite, CURRENT)['records']), {root.ROOT_CASE, peer})
            records['operations:' + peer] = {'run': 10, 'commit': SOURCE}
            with self.assertRaisesRegex(r.ReceiptError, 'one actual root execution domain'):
                root.prepare(suite, CURRENT)
            records['operations:' + peer] = {'run': 10, 'commit': SOURCE}
            with self.assertRaisesRegex(r.ReceiptError, 'one actual root execution domain'):
                root.prepare(suite, CURRENT)
            del records[root_case]
            with self.assertRaises(r.ReceiptError):
                root.prepare(suite, CURRENT)



    def test_workflow_projection_keeps_default_and_rejects_unreviewed_changes(self):
        from validation import workspace_rust_continuation as workspace

        workflow = Path(__file__).resolve().parents[2] / '.github/workflows/ci.yml'
        raw = workflow.read_bytes()
        projected = workspace.selected_preflight_projection(raw)
        self.assertNotIn(b'root_ownership_retained', projected)
        self.assertNotIn(b'NODE_SELECTION', projected)
        self.assertEqual(workspace.selected_preflight_projection(projected), projected)
        node_env = b'          NODE_SELECTION: ${{ steps.python-continuation.outputs.node_selection }}\n'
        missing = raw.replace(node_env, b'')
        moved = missing.replace(b'      - name: Audit corrective-history evidence\n',
                                b'      - name: Audit corrective-history evidence\n' + node_env)
        for bad in (moved, raw.replace(b'validation.native_python_execute node',
                                       b'validation.native_python_execute unsupported'),
                    raw.replace(b"&& needs.preflight.outputs.root_ownership_retained != 'true'",
                                b'&& false')):
            with self.subTest(bad=bad[-100:]), self.assertRaises(r.ReceiptError):
                workspace.selected_preflight_projection(bad)
