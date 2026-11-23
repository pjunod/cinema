"""Only a proven exact-head manual gate can suppress duplicate automatic work."""
import unittest
from unittest.mock import patch
from pathlib import Path
from validation import merge_train as route
from validation import nextest_shard_continuation as source
from validation import python_unit_receipts as r

class ManualSourceGuardTests(unittest.TestCase):
    def test_missing_claim_preserves_default_scope_exactly(self):
        lines = ['rust=true', 'apple=true']
        with patch.object(route, 'authenticate_manual_source') as authenticate:
            self.assertEqual(route.authenticated_manual_scope(lines, {'pull_request': {'body': 'ordinary'}}, None, True, None),
                             route.scope(lines, None, True, None))
        authenticate.assert_not_called()

    def test_claim_alone_never_suppresses_unproven_or_wrong_head_gate(self):
        event = {'pull_request': {'body': 'Manual-Source-Gate: 12'}}
        with patch.object(route, 'authenticate_manual_source', side_effect=r.ReceiptError('stale head/base')), \
                self.assertRaises(r.ReceiptError):
            route.authenticated_manual_scope(['rust=true'], event, None, True, None)
        for body in ('Manual-Source-Gate: fake', 'Manual-Source-Gate: 12\nManual-Source-Gate: 13'):
            with self.subTest(body=body), self.assertRaises(r.ReceiptError):
                route.authenticated_manual_scope(['rust=true'], {'pull_request': {'body': body}}, None, True, None)

    def test_actual_authenticated_gate_omits_only_duplicate_lane_and_prints_real_provenance(self):
        proof = {'run': 12, 'job': 13, 'url': 'https://forge/actual-ci/12'}
        with patch.object(route, 'authenticate_manual_source', return_value=proof) as authenticate:
            result = dict(line.split('=', 1) for line in route.authenticated_manual_scope(
                ['rust=true', 'apple=true'], {'pull_request': {'body': 'Manual-Source-Gate: 12'}}, None, True, None))
        self.assertEqual(result['run_lane'], 'false')
        self.assertEqual(result['rust_units'], 'false')
        self.assertEqual(result['worker_control'], 'false')
        authenticate.assert_called_once()

    def test_projection_removes_only_scope_token_plumbing_not_test_recipe(self):
        raw = Path('.github/workflows/main-fast-lane.yml').read_bytes()
        projected = source.projected_main_workflow(raw)
        self.assertEqual(source.retention.digest(projected), source.HARNESS['.github/workflows/main-fast-lane.yml'])
        with self.assertRaises(r.ReceiptError):
            source.projected_main_workflow(raw + b'\n# unreviewed workflow change\n')
        with self.assertRaises(r.ReceiptError):
            source.projected_main_workflow(raw.replace(b'GITHUB_TOKEN:', b'OTHER_TOKEN:'))
