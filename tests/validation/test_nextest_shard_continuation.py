"""Original nextest rows and current discovery fence every retained body."""
import copy
import io
import json
import unittest
from unittest.mock import patch
import zipfile

from validation import nextest_shard_continuation as proof
from validation import python_unit_receipts as r
from validation import qualification


class NextestShardContinuationTests(unittest.TestCase):
    def test_duplicate_and_contradictory_original_statuses_are_refused(self):
        green = b'2026-11-20T17:00:00Z         PASS [ 0.01s] (1/2) plurxd::bin/plurxd group::a\n'
        self.assertEqual(proof.strict_outcomes(green), {'plurxd::bin/plurxd group::a': 'PASS'})
        for raw in (green + green, green + green.replace(b'PASS', b'FAIL'), b'no actual status'):
            with self.subTest(raw=raw), self.assertRaises(r.ReceiptError):
                proof.strict_outcomes(raw)

    def listing(self, names):
        return {'rust-suites': {'plurxd::bin/plurxd': {'testcases': {
            name: {'ignored': ignored} for name, ignored in names.items()}}}}

    def test_current_discovery_never_loses_failed_or_invalidated_bodies(self):
        record = {'selected': ['plurxd'], 'universe': ['plurxd::bin/plurxd green', 'plurxd::bin/plurxd red'],
                  'serial_ids': [], 'retained_ids': ['plurxd::bin/plurxd green']}
        config = type('Config', (), {'serial_ignored': {}})()
        current, due, serial = proof.current_census(record, self.listing({'green': False, 'red': False, 'new': False}), config)
        self.assertEqual(due, {'plurxd::bin/plurxd red', 'plurxd::bin/plurxd new'})
        self.assertFalse(serial)
        for listing in (self.listing({'green': False}), self.listing({'green': False, 'red': True}),
                        {'rust-suites': {'foreign': {'testcases': {}}}}):
            with self.subTest(listing=listing), self.assertRaises(r.ReceiptError):
                proof.current_census(record, listing, config)

    def test_every_changed_path_and_byte_hash_requires_review(self):
        path = 'crates/plurxd/src/http/stream/tests.rs'
        record = {'prior': 'a' * 40, 'changes': {path: {'before': proof.retention.digest(b'old'),
                  'after': proof.retention.digest(b'new'), 'affected': ['bin body'], 'reason': 'changed regression'}}}
        with patch.object(proof.retention, 'changed', return_value={path}), patch.object(
                proof.retention, 'git_bytes', side_effect=lambda rev, _: b'old' if rev == 'a' * 40 else b'new'):
            self.assertEqual(proof.reviewed_changes(record, 'b' * 40, {'bin body'}), {'bin body'})
            for field, value in (('before', '0' * 64), ('after', '0' * 64), ('affected', []),
                                 ('affected', ['bin unknown']), ('reason', '')):
                bad = copy.deepcopy(record)
                bad['changes'][path][field] = value
                with self.subTest(field=field, value=value), self.assertRaises(r.ReceiptError):
                    proof.reviewed_changes(bad, 'b' * 40, {'bin body'})
        for path in ('Cargo.toml', 'Cargo.lock', '.github/workflows/rust-units.yml', '.github/workflows/ci.yml',
                     'validation/rust_units.py', 'crates/plurxd/src/transcode.rs'):
            with self.subTest(path=path), patch.object(proof.retention, 'changed', return_value={path}), \
                    self.assertRaises(r.ReceiptError):
                proof.reviewed_changes({'prior': 'a' * 40, 'changes': {path: {}}}, 'b' * 40, {'bin body'})

    def test_unsafe_or_multiple_artifact_members_cannot_supply_plan(self):
        for members in ({'other.json': '{}'}, {'plan.json': '{}', 'extra.json': '{}'}):
            blob = io.BytesIO()
            with zipfile.ZipFile(blob, 'w') as archive:
                for name, value in members.items():
                    archive.writestr(name, value)
            api = type('API', (), {'bytes': lambda self, path: blob.getvalue()})()
            with self.subTest(members=members), self.assertRaises(r.ReceiptError):
                proof.artifact(api, {'expired': False, 'id': 1, 'size_in_bytes': len(blob.getvalue())}, 'plan.json')

    def test_manual_ordinary_pr_binding_preserves_auto_and_release_routes(self):
        environment = {'GITHUB_EVENT_NAME': 'workflow_dispatch', 'GITHUB_RUN_ATTEMPT': '1',
                       'GITHUB_REPOSITORY': 'owner/repo', 'PLURX_PROMOTION_PR': '42',
                       'PLURX_PROMOTION_HEAD_SHA': 'a' * 40, 'PLURX_PROMOTION_BASE_SHA': 'b' * 40,
                       'GITHUB_REF': 'refs/heads/codex/task', 'GITHUB_SHA': 'a' * 40}
        pull = {'number': 42, 'state': 'open', 'merged': False,
                'head': {'ref': 'codex/task', 'sha': 'a' * 40, 'repo': {'full_name': 'owner/repo'}},
                'base': {'ref': 'main', 'sha': 'b' * 40, 'repo': {'full_name': 'owner/repo'}}}
        result = qualification.manual_binding(environment, pull, 'a' * 40, 'b' * 40, 'a' * 40, True)
        self.assertEqual(result['schema'], 2)
        self.assertFalse(qualification.promotion_ref('codex/task'))
        self.assertTrue(qualification.promotion_ref('effort/project'))
        for mutate in ('fork', 'moved', 'base', 'closed'):
            bad = copy.deepcopy(pull)
            if mutate == 'fork': bad['head']['repo']['full_name'] = 'foreign/repo'
            if mutate == 'moved': bad['head']['sha'] = 'c' * 40
            if mutate == 'base': bad['base']['sha'] = 'c' * 40
            if mutate == 'closed': bad['state'] = 'closed'
            with self.subTest(mutate=mutate), self.assertRaises(qualification.QualificationError):
                qualification.manual_binding(environment, bad, 'a' * 40, 'b' * 40, 'a' * 40, True)
        for key, value in (('GITHUB_EVENT_NAME', 'pull_request'), ('GITHUB_RUN_ATTEMPT', '2'),
                           ('PLURX_PROMOTION_HEAD_SHA', ''), ('PLURX_PROMOTION_BASE_SHA', '')):
            with self.subTest(key=key), self.assertRaises(qualification.QualificationError):
                qualification.manual_binding({**environment, key: value}, pull, 'a' * 40, 'b' * 40, 'a' * 40, True)
        with self.assertRaises(qualification.QualificationError):
            qualification.manual_binding(environment, pull, 'a' * 40, 'b' * 40, 'a' * 40, False)

    def test_authenticator_rejects_unknown_contract_or_foreign_original_run(self):
        record = dict(version=proof.POLICY, repository=9, pr=42, branch='codex/task',
                      base_sha='c' * 40, source_commit='b' * 40, prior='a' * 40, run=10,
                      plan_artifact=1, crate_artifacts={}, shard_artifacts={}, changes={}, compatibility=proof.COMPATIBILITY)
        binding = {'schema': 2, 'pull_request': 42, 'base_sha': 'c' * 40, 'head_ref': 'codex/task'}
        good = {'repository': {'id': 9}, 'commit_sha': 'a' * 40, 'workflow_id': 'main-fast-lane.yml',
                'trigger_event': 'pull_request', 'prettyref': '#42'}
        for field, value in (('commit_sha', 'f' * 40), ('repository', {'id': 99}),
                             ('workflow_id', 'foreign.yml'), ('trigger_event', 'workflow_dispatch'), ('prettyref', '#99')):
            api = type('API', (), {'get': lambda self, path, row={**good, field: value}: row})()
            with self.subTest(field=field), patch.object(proof, 'HARNESS', {}), \
                    patch('validation.workspace_rust_continuation.check_workflow_environment'), \
                    patch.object(proof.retention, 'git_bytes', return_value=b'workflow'), self.assertRaises(r.ReceiptError):
                proof.authenticate(api, record, binding, 'b' * 40, 9)
        for field, value in (('compatibility', 'arbitrary'), ('version', 'unknown'), ('source_commit', 'f' * 40)):
            with self.subTest(field=field), self.assertRaises(r.ReceiptError):
                proof.authenticate(None, {**record, field: value}, binding, 'b' * 40, 9)
        with self.assertRaises(r.ReceiptError):
            proof.authenticate(None, record, {**binding, 'schema': 1}, 'b' * 40, 9)

    def test_source_recipe_and_compiler_mutations_cannot_reuse_rows(self):
        record = dict(version=proof.POLICY, repository=9, pr=42, branch='codex/task',
                      base_sha='c' * 40, source_commit='b' * 40, prior='a' * 40, run=10,
                      plan_artifact=1, crate_artifacts={}, shard_artifacts={}, changes={}, compatibility=proof.COMPATIBILITY)
        for value in (None, b'changed image', b'changed toolchain'):
            with self.subTest(value=value), patch('validation.workspace_rust_continuation.check_workflow_environment'), \
                    patch.object(proof.retention, 'git_bytes', side_effect=lambda rev, path: b'workflow'
                                 if path == '.github/workflows/ci.yml' else value), self.assertRaises(r.ReceiptError):
                proof.authenticate(None, record, {'schema': 2, 'pull_request': 42, 'base_sha': 'c' * 40, 'head_ref': 'codex/task'}, 'b' * 40, 9)

    def test_complete_census_preserves_failure_and_refuses_missing_duplicate_or_forged_status(self):
        plan = {'run': 10, 'head': 'a' * 40, 'shards': 2, 'crates': {'plurxd': {
            'remaining': ['bin green', 'bin red'], 'serial': []}}}
        shards = [{'schema': 1, 'run': 10, 'head': 'a' * 40, 'index': index, 'shards': 2,
                   'results': {'plurxd': {'passed': [name] if index == 1 else [],
                                         'failed': [name] if index == 2 else [], 'skipped': []}}}
                  for index, name in ((1, 'bin green'), (2, 'bin red'))]
        logged = {1: {'bin green': 'PASS'}, 2: {'bin red': 'FAIL'}}
        self.assertEqual(proof.complete_shard_census(plan, shards, logged), {'bin green': 'PASS', 'bin red': 'FAIL'})
        with self.assertRaises(r.ReceiptError):
            proof.complete_shard_census(plan, shards[:1], {1: logged[1]})
        with self.assertRaises((r.ReceiptError, proof.units.UnitsError)):
            proof.complete_shard_census(plan, shards + [shards[0]], logged)
        with self.assertRaises(r.ReceiptError):
            proof.complete_shard_census(plan, shards, {1: logged[1], 2: {'bin red': 'PASS'}})
        bad = copy.deepcopy(shards)
        bad[0]['results']['plurxd']['passed'].append('bin green')
        with self.assertRaises((r.ReceiptError, proof.units.UnitsError)):
            proof.complete_shard_census(plan, bad, logged)
        bad = copy.deepcopy(shards)
        bad[1]['results']['plurxd']['failed'] = []
        with self.assertRaises(r.ReceiptError):
            proof.complete_shard_census(plan, bad, logged)

    def test_manual_source_gate_receipt_never_claims_effort_qualification(self):
        binding = {'schema': 2, 'repository': 'owner/repo', 'pull_request': 42,
                   'head_ref': 'codex/task', 'base_ref': 'main', 'head_sha': 'a' * 40, 'base_sha': 'b' * 40}
        environment = {'GITHUB_EVENT_NAME': 'workflow_dispatch', 'GITHUB_RUN_ATTEMPT': '1',
                       'GITHUB_REPOSITORY': 'owner/repo', 'GITHUB_RUN_ID': '10',
                       'GITHUB_WORKFLOW_REF': 'owner/repo/.github/workflows/ci.yml@refs/heads/codex/task',
                       'PLURX_PROMOTION_PR': '42', 'PLURX_PROMOTION_HEAD_SHA': 'a' * 40,
                       'PLURX_PROMOTION_BASE_SHA': 'b' * 40, 'GITHUB_SHA': 'a' * 40,
                       'PLURX_PROMOTION_BINDING': json.dumps(binding)}
        result = qualification.build_receipt(environment, {name: 'success' for name in qualification.REQUIRED_JOBS},
                                             'a' * 40, 'c' * 40)
        self.assertEqual(result['kind'], 'selected-pr-source-gate')
        self.assertFalse(qualification.promotion_ref(result['head_ref']))

    def test_exact_filter_discovery_keeps_binary_identity_and_refuses_unknown_status(self):
        listing = {'rust-suites': {'one': {'testcases': {'same': {'filter-match': {'status': 'matches'}}}},
                                  'two': {'testcases': {'same': {'filter-match': {'status': 'mismatch'}}}}}}
        self.assertEqual(proof.selected_listing_ids(listing), {'one same'})
        listing['rust-suites']['two']['testcases']['same']['filter-match']['status'] = 'matches'
        self.assertEqual(proof.selected_listing_ids(listing), {'one same', 'two same'})
        del listing['rust-suites']['two']['testcases']['same']['filter-match']
        with self.assertRaises(r.ReceiptError):
            proof.selected_listing_ids(listing)
