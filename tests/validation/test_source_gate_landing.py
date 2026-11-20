"""A green commit-status string cannot supply authenticated manual landing."""
import copy
import unittest
from pathlib import Path
from unittest.mock import Mock
from validation import landing
from validation import source_gate_landing as source
from validation import python_unit_receipts as r

class SourceGateLandingTests(unittest.TestCase):
    def test_forged_ci_status_is_not_default_landing_authority(self):
        forge = Mock()
        forge.statuses.return_value = [{'context': 'ci / Main promotion gate', 'status': 'success'}]
        with self.assertRaises(landing.LandRefused):
            landing.require_green(forge, 42, 'a' * 40, None, lambda line: None)
        forge.source_gate.assert_not_called()

    def test_explicit_route_prints_actual_ci_provenance_and_refuses_untested(self):
        forge = Mock()
        forge.source_gate.return_value = {'run': 10, 'job': 20, 'url': 'https://forge/actual/10'}
        output = []
        self.assertFalse(landing.require_green(forge, 42, 'a' * 40, None, output.append,
                         source_gate_run=10, root=Path('.'), main='b' * 40))
        self.assertIn('actual run 10, job 20', output[0])
        forge.statuses.assert_not_called()
        with self.assertRaises(landing.LandRefused):
            landing.require_green(forge, 42, 'a' * 40, 'waiver', output.append,
                                  source_gate_run=10, root=Path('.'), main='b' * 40)

    def test_missing_artifact_or_required_surface_refuses_manual_success(self):
        api = Mock()
        with self.assertRaises(r.ReceiptError):
            source.uploaded(api, [], 'source-gate', {'run_id': 10}, 'check', {'receipt.json'})
        api.bytes.assert_not_called()
        with self.assertRaises(r.ReceiptError):
            source.require_surfaces([], {'jobs': {'rust': 'success'}}, 10, 1, 3, None)

    def test_source_completion_refuses_release_forgery_stale_head_main_and_missing_census(self):
        proof = {'candidate': 'a' * 40, 'base': 'b' * 40, 'run': 9, 'digest': 'c' * 64,
                 'selected': ['plurxd'], 'universe': ['plurxd::bin/plurxd green', 'plurxd::bin/plurxd red'],
                 'serial_ids': [], 'retained_ids': ['plurxd::bin/plurxd green']}
        listing = {'rust-suites': {'plurxd::bin/plurxd': {'testcases': {
                    'green': {'ignored': False}, 'red': {'ignored': False}}}}}
        config = type('Config', (), {'serial_ignored': {}})()
        completion = dict(version=source.nextest.POLICY, kind='selected-pr-rust-source-gate',
            candidate=proof['candidate'], base=proof['base'], source_run=9, enrollment_digest=proof['digest'],
            selected=proof['selected'], retained=proof['retained_ids'], executed=['plurxd::bin/plurxd red'],
            current_listing_sha256='d' * 64, current_universe=proof['universe'], complete=True)
        source.completion_census(proof, completion, listing, config, 'd' * 64)
        for field, value in (('kind', 'effort-qualification'), ('complete', False), ('executed', []),
                             ('candidate', 'f' * 40), ('base', 'f' * 40), ('current_listing_sha256', 'e' * 64)):
            with self.subTest(field=field), self.assertRaises(r.ReceiptError):
                source.completion_census(proof, {**completion, field: value}, listing, config, 'd' * 64)

    def test_wrong_owner_attempt_or_missing_job_is_not_qualified(self):
        job = dict(name='Main promotion gate', id=20, run_id=10, repo_id=1, owner_id=3, attempt=1)
        self.assertEqual(source.one_job([job], job['name'], 10, 1, 3), job)
        for field, value in (('owner_id', 4), ('attempt', 2), ('run_id', 11), ('repo_id', 2)):
            with self.subTest(field=field), self.assertRaises(r.ReceiptError):
                source.one_job([{**job, field: value}], job['name'], 10, 1, 3)
        with self.assertRaises(r.ReceiptError):
            source.one_job([], job['name'], 10, 1, 3)
