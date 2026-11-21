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
    def test_native_empty_domain_requires_exact_reviewed_source_and_complete_current_additions(self):
        path = next(iter(proof.REVIEWED_NATIVE_FEL_EMPTY))
        before, after = b'old native source', b'new native source'
        witness = {'affected': [], 'compile_only': {'kind': 'reviewed-native-fel-domain',
                   'new_ids': proof.NATIVE_FEL_CURRENT_ADDITIONS}}
        pair = (proof.retention.digest(before), proof.retention.digest(after))
        with patch.dict(proof.REVIEWED_NATIVE_FEL_EMPTY, {path: pair}):
            proof.compile_only_native_fel(path, witness, before, after, set())
            for selected_path, row, changed in (
                    ('unreviewed.rs', witness, after),
                    (path, witness, after + b'changed'),
                    (path, {**witness, 'affected': ['unrelated old case']}, after),
                    (path, {**witness, 'compile_only': {'kind': 'reviewed-native-fel-domain', 'new_ids': []}}, after)):
                with self.subTest(path=selected_path, row=row), self.assertRaises(r.ReceiptError):
                    proof.compile_only_native_fel(selected_path, row, before, changed, set())

    def test_native_empty_domain_cannot_relabel_old_cases_as_new_current_obligations(self):
        path = next(iter(proof.REVIEWED_NATIVE_FEL_EMPTY))
        before, after = b'old native source', b'new native source'
        witness = {'affected': [], 'compile_only': {'kind': 'reviewed-native-fel-domain',
                   'new_ids': proof.NATIVE_FEL_CURRENT_ADDITIONS}}
        pair = (proof.retention.digest(before), proof.retention.digest(after))
        with patch.dict(proof.REVIEWED_NATIVE_FEL_EMPTY, {path: pair}):
            with self.assertRaises(r.ReceiptError):
                proof.compile_only_native_fel(path, witness, before, after,
                                               {proof.NATIVE_FEL_CURRENT_ADDITIONS[0]})

    def test_new_only_source_requires_closed_bytes_names_and_empty_old_impact(self):
        path = next(iter(proof.REVIEWED_TEST_ADDITIONS))
        ids = proof.REVIEWED_TEST_ADDITIONS[path][2]
        before, after = b'old source', b'old source plus new tests'
        pair = (proof.retention.digest(before), proof.retention.digest(after), ids)
        witness = {'affected': [], 'compile_only': {'kind': 'reviewed-test-additions', 'new_ids': ids}}
        with patch.dict(proof.REVIEWED_TEST_ADDITIONS, {path: pair}):
            proof.compile_only_test_additions(path, witness, before, after, set())
            for source_path, row, old, current, universe in (
                    ('unreviewed.rs', witness, before, after, set()),
                    (path, witness, before, after + b'changed', set()),
                    (path, {**witness, 'affected': ['unrelated old']}, before, after, set()),
                    (path, witness, before, after, set(ids)),
                    (path, {**witness, 'compile_only': {'kind': 'reviewed-test-additions', 'new_ids': []}}, before, after, set())):
                with self.subTest(path=source_path, row=row, universe=universe), self.assertRaises(r.ReceiptError):
                    proof.compile_only_test_additions(source_path, row, old, current, universe)

    def test_new_only_cases_must_appear_in_current_ordinary_discovery_without_retained_credit(self):
        name = 'new_body'
        identity = 'plurxd::bin/plurxd ' + name
        record = {'selected': ['plurxd'], 'universe': [], 'serial_ids': [], 'retained_ids': [],
                  'mandatory_new_ids': [identity]}
        config = type('Config', (), {'serial_ignored': {}})()
        _, due, _ = proof.current_census(record, self.listing({name: False}), config)
        self.assertEqual(due, {identity})
        for listing, retained in ((self.listing({}), []), (self.listing({name: True}), []),
                                  (self.listing({name: False}), [identity])):
            with self.subTest(listing=listing, retained=retained), self.assertRaises(r.ReceiptError):
                proof.current_census({**record, 'retained_ids': retained}, listing, config)

    def retirement_record(self):
        return {'changes': {proof.RETIRED_FEL_PATH: {
            'before': proof.RETIRED_FEL_SOURCE[0], 'after': proof.RETIRED_FEL_SOURCE[1],
            'affected': [proof.RETIRED_FEL_ID],
            'retirement': {'retired': proof.RETIRED_FEL_ID, 'successor': proof.FEL_SUCCESSOR_ID}}}}

    def test_reviewed_retirement_preserves_only_an_authenticated_pass_with_exact_source_pair(self):
        record = self.retirement_record()
        self.assertEqual(proof.reviewed_retirements(record, {proof.RETIRED_FEL_ID: 'PASS'}),
                         {proof.RETIRED_FEL_ID: proof.FEL_SUCCESSOR_ID})
        for field in ('before', 'after', 'affected', 'retirement'):
            changed = copy.deepcopy(record)
            changed['changes'][proof.RETIRED_FEL_PATH][field] = [] if field == 'affected' else 'forged'
            with self.subTest(field=field), self.assertRaises(r.ReceiptError):
                proof.reviewed_retirements(changed, {proof.RETIRED_FEL_ID: 'PASS'})

    def test_failed_missing_or_existing_successor_outcomes_cannot_be_retired(self):
        for outcomes in ({}, {proof.RETIRED_FEL_ID: 'FAIL'},
                         {proof.RETIRED_FEL_ID: 'PASS', proof.FEL_SUCCESSOR_ID: 'PASS'}):
            with self.subTest(outcomes=outcomes), self.assertRaises(r.ReceiptError):
                proof.reviewed_retirements(self.retirement_record(), outcomes)

    def test_retired_pass_never_counts_as_current_coverage_and_successor_is_due(self):
        record = {'selected': ['plurxd'], 'universe': [proof.RETIRED_FEL_ID],
                  'serial_ids': [], 'retained_ids': [],
                  'retired_ids': {proof.RETIRED_FEL_ID: proof.FEL_SUCCESSOR_ID}}
        name = proof.FEL_SUCCESSOR_ID.split(' ', 1)[1]
        config = type('Config', (), {'serial_ignored': {}})()
        current, due, _ = proof.current_census(record, self.listing({name: False}), config)
        self.assertEqual(current, {proof.FEL_SUCCESSOR_ID})
        self.assertEqual(due, current)
        for names in ({name: True}, {}, {name: False, proof.RETIRED_FEL_ID.split(' ', 1)[1]: False}):
            with self.subTest(names=names), self.assertRaises(r.ReceiptError):
                proof.current_census(record, self.listing(names), config)
        record['retained_ids'] = [proof.FEL_SUCCESSOR_ID]
        with self.assertRaises(r.ReceiptError):
            proof.current_census(record, self.listing({name: False}), config)

    def test_duplicate_and_contradictory_original_statuses_are_refused(self):
        green = b'2026-11-20T17:00:00Z         PASS [ 0.01s] (1/2) plurxd::bin/plurxd group::a\n'
        self.assertEqual(proof.strict_outcomes(green), {'plurxd::bin/plurxd group::a': 'PASS'})
        for raw in (green + green, green + green.replace(b'PASS', b'FAIL'), b'no actual status'):
            with self.subTest(raw=raw), self.assertRaises(r.ReceiptError):
                proof.strict_outcomes(raw)

    def test_only_exact_failed_recaps_match_authenticated_invocation_summaries(self):
        red = b'        FAIL [ 0.01s] (1/2) plurxd::bin/plurxd group::red\n'
        green = b'        PASS [ 0.02s] (2/2) plurxd::bin/plurxd group::green\n'
        serial = b'        PASS [ 0.03s] (1/1) plurxd::bin/plurxd group::serial\n'
        summary = b'     Summary [ 0.030s] 2 tests run: 1 passed, 1 failed, 803 skipped\n'
        end = b'     Summary [ 0.030s] 1 test run: 1 passed, 804 skipped\n'
        live = red + green + summary
        self.assertEqual(proof.strict_outcomes(live + red + serial + end), {
            'plurxd::bin/plurxd group::red': 'FAIL',
            'plurxd::bin/plurxd group::green': 'PASS',
            'plurxd::bin/plurxd group::serial': 'PASS',
        })
        for broken in (live, live + green, live + red + red,
                       live + red.replace(b'0.01s', b'0.09s'),
                       red + green + summary.replace(b'2 tests run', b'3 tests run') + red):
            with self.subTest(broken=broken), self.assertRaises(r.ReceiptError):
                proof.strict_outcomes(broken)

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
        environment['PLURX_SOURCE_GATE_PAPERWORK'] = json.dumps({'version': 1, 'head': 'a' * 40,
            'base': 'b' * 40, 'pr': 42, 'passed': True, 'body_sha256': 'd' * 64, 'title_sha256': 'e' * 64})
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

    def test_actual_compiler_image_nextest_and_ffmpeg_witnesses_are_required(self):
        common = ('image=registry/ci@sha256:' + proof.IMAGE_DIGEST + '\ncargo-nextest 0.9.151 (build)\n')
        compiler = ('release: 1.97.1\ncommit-hash: 8bab26f4f68e0e26f0bb7960be334d5b520ea452\n'
                    'host: x86_64-unknown-linux-gnu\n')
        probe = 'expected major=8 resolved=ffmpeg version ' + proof.FFMPEG_VERSION + ' Copyright\n'
        proof.environment_witness((common + compiler).encode(), archive=True)
        proof.environment_witness((common + probe).encode())
        for raw, archive in ((compiler, True), (common, True), (common, False),
                             (common.replace('0.9.151', '0.9.152') + probe, False),
                             (common + compiler.replace('1.97.1', '1.98.0'), True),
                             (common + probe.replace(proof.FFMPEG_VERSION, '9.0'), False)):
            with self.subTest(raw=raw), self.assertRaises(r.ReceiptError):
                proof.environment_witness(raw.encode(), archive=archive)

    def _pidfd_constants(self):
        # Exercise the Linux contract on every development host without skips
        # or any actual kernel/process operation in these source regressions.
        return patch.multiple(proof.os, create=True, P_PIDFD=3, WNOWAIT=0x1000000,
                              WNOHANG=1, WSTOPPED=2, WCONTINUED=8, WEXITED=4,
                              CLD_STOPPED=5, CLD_CONTINUED=6, CLD_KILLED=2, waitid=None)

    def test_pidfd_capability_refuses_enosys_before_any_body(self):
        import errno
        with self._pidfd_constants(), \
                patch.object(proof.signal, 'pidfd_send_signal', create=True), \
                patch.object(proof.os, 'pidfd_open', create=True, side_effect=OSError(errno.ENOSYS, 'unavailable')), \
                patch.object(proof.subprocess, 'Popen') as child, self.assertRaises(r.ReceiptError):
            proof.require_pidfd_environment()
        child.assert_not_called()

    def test_owned_child_pidfd_probe_observes_full_lifecycle_and_reaps(self):
        from types import SimpleNamespace
        from unittest.mock import Mock, call
        child = Mock(pid=777)
        child.poll.return_value = 0
        events = [SimpleNamespace(si_pid=777, si_code=code, si_status=status) for code, status in (
            (5, proof.signal.SIGSTOP), (6, proof.signal.SIGCONT), (2, proof.signal.SIGTERM))]
        with self._pidfd_constants(), \
                patch.object(proof.os, 'pidfd_open', create=True, side_effect=[100, 101]) as opened, \
                patch.object(proof.os, 'close') as closed, \
                patch.object(proof.os, 'waitid', side_effect=events), \
                patch.object(proof.signal, 'pidfd_send_signal', create=True) as sent, \
                patch.object(proof.subprocess, 'Popen', return_value=child):
            proof.require_pidfd_environment()
        self.assertEqual(opened.call_args_list, [call(proof.os.getpid()), call(777)])
        self.assertEqual(closed.call_args_list, [call(100), call(101)])
        self.assertEqual(sent.call_args_list, [call(101, proof.signal.SIGSTOP),
                         call(101, proof.signal.SIGCONT), call(101, proof.signal.SIGTERM)])
        child.kill.assert_not_called()
        self.assertEqual(child.wait.call_count, 2)

    def test_failed_child_pidfd_signal_still_kills_reaps_and_closes_owned_probe(self):
        import errno
        from unittest.mock import Mock, call
        child = Mock(pid=777)
        child.poll.return_value = None
        with self._pidfd_constants(), \
                patch.object(proof.os, 'pidfd_open', create=True, side_effect=[100, 101]), \
                patch.object(proof.os, 'close') as closed, \
                patch.object(proof.signal, 'pidfd_send_signal', create=True,
                             side_effect=OSError(errno.ENOSYS, 'unavailable')), \
                patch.object(proof.subprocess, 'Popen', return_value=child), self.assertRaises(r.ReceiptError):
            proof.require_pidfd_environment()
        child.kill.assert_called_once_with()
        child.wait.assert_called_once_with(timeout=5.0)
        self.assertEqual(closed.call_args_list, [call(100), call(101)])

    def test_workflow_projection_accepts_only_exact_metadata_blocks(self):
        from validation import workspace_rust_continuation as workspace
        before = b'known original workflow'
        after = before + workspace.SOURCE_GATE_UPLOAD.encode() + workspace.SOURCE_GATE_PAPERWORK.encode()
        self.assertTrue(workspace.metadata_only_workflow(before, after))
        for bad in (after + b'changed runtime', after.replace(b'retention-days: 14', b'retention-days: 1'),
                    after + workspace.SOURCE_GATE_UPLOAD.encode()):
            self.assertFalse(workspace.metadata_only_workflow(before, bad))

    def test_pidfd_wait_deadline_and_foreign_lifecycle_never_succeed(self):
        from types import SimpleNamespace
        with self._pidfd_constants(), patch.object(proof.os, 'waitid') as waited, self.assertRaises(r.ReceiptError):
            proof._pidfd_observe(101, 777, proof.os.WSTOPPED, proof.os.CLD_STOPPED, proof.signal.SIGSTOP, -1.0)
        waited.assert_not_called()
        event = SimpleNamespace(si_pid=778, si_code=5, si_status=proof.signal.SIGSTOP)
        with self._pidfd_constants(), patch.object(proof.os, 'waitid', return_value=event), \
                self.assertRaises(r.ReceiptError):
            proof._pidfd_observe(101, 777, proof.os.WSTOPPED, proof.os.CLD_STOPPED,
                                 proof.signal.SIGSTOP, proof.time.monotonic() + 1.0)

    def test_compile_only_none_witness_refuses_nonliteral_production_changes_or_false_runtime_ids(self):
        path='crates/plurxd/src/prodsched.rs'
        before=b'fn row() {\n    Row { x: true, }\n}\n'
        after=b'fn row() {\n    first_cto: None,\n    Row { x: true, }\n}\n'
        witness={'affected':[],'compile_only':{'kind':'none-row-field-initializer'}}
        proof.compile_only_none(path,witness,before,after)
        cases=[('crates/plurxd/src/vodgen.rs',witness,after),
               (path,{**witness,'affected':['bin body']},after),
               (path,witness,after.replace(b'None',b'Some(7)')),
               (path,witness,after.replace(b'true',b'false')),
               (path,witness,after.replace(b'    first_cto: None,\n',b'    first_cto: None,\n    first_cto: None,\n'))]
        for candidate,row,changed in cases:
            with self.subTest(candidate=candidate,changed=changed),self.assertRaises(r.ReceiptError):
                proof.compile_only_none(candidate,row,before,changed)

    def ignored_source(self):
        return (b'async fn measured_landing() -> u64 {\n    1\n}\n'
                b'#[tokio::test]\n#[ignore = "physical inputs required"]\n'
                b'async fn real_fitted_index_and_seek_publish_stored_curves() {\n'
                b'    measured_landing().await;\n}\n'
                b'fn ordinary() {\n    assert!(true);\n}\n')

    def test_ignored_compile_only_projection_preserves_every_selected_source_byte_and_ignore(self):
        before=self.ignored_source();after=before.replace(b'    1\n',b'    2\n')
        self.assertEqual(proof.ignored_source_projection(before),proof.ignored_source_projection(after))
        for changed in (after.replace(b'#[ignore = "physical inputs required"]\n',b''),
                        after.replace(b'assert!(true)',b'assert!(false)'),
                        after+b'fn outside() { measured_landing(); }\n'):
            with self.subTest(changed=changed):
                try:projection=proof.ignored_source_projection(changed)
                except r.ReceiptError:continue
                self.assertNotEqual(proof.ignored_source_projection(before),projection)

    def test_ignored_compile_only_witness_requires_sealed_original_metadata_and_unselected_status(self):
        import base64
        binary,name=proof.IGNORED_COMPILE_ID.split(' ',1)
        for ignored,included,forged in [(True,False,False),(False,False,False),(True,True,False),(True,False,True)]:
            listing=self.listing({name:ignored,'green':False});raw=json.dumps(listing).encode()
            digest=proof.retention.digest(raw)
            envelope={'sha256':digest,'base64':base64.b64encode(raw+(b' ' if forged else b'')).decode()}
            witness={'affected':[],'compile_only':{'kind':'ignored-test-only','test':proof.IGNORED_COMPILE_ID,'prior_listing':envelope}}
            universe={binary+' green'}|({proof.IGNORED_COMPILE_ID} if included else set())
            with patch.object(proof,'PRIOR_LISTING_SHA256',digest):
                if ignored and not included and not forged:
                    proof.compile_only_ignored(proof.IGNORED_COMPILE_PATH,witness,self.ignored_source(),self.ignored_source(),universe,prior=proof.PRIOR_LISTING_SOURCE,run=proof.PRIOR_LISTING_RUN)
                    for prior,run in [('0'*40,proof.PRIOR_LISTING_RUN),(proof.PRIOR_LISTING_SOURCE,1)]:
                        with self.subTest(prior=prior,run=run),self.assertRaises(r.ReceiptError):
                            proof.compile_only_ignored(proof.IGNORED_COMPILE_PATH,witness,self.ignored_source(),self.ignored_source(),universe,prior=prior,run=run)
                else:
                    with self.subTest(ignored=ignored,included=included,forged=forged),self.assertRaises(r.ReceiptError):
                        proof.compile_only_ignored(proof.IGNORED_COMPILE_PATH,witness,self.ignored_source(),self.ignored_source(),universe,prior=proof.PRIOR_LISTING_SOURCE,run=proof.PRIOR_LISTING_RUN)

    def test_current_listing_cannot_promote_the_compile_only_ignored_case_into_runtime_coverage(self):
        binary,name=proof.IGNORED_COMPILE_ID.split(' ',1)
        record={'selected':['plurxd'],'universe':[binary+' green'],'serial_ids':[],
                'retained_ids':[binary+' green'],'compile_only_ignored_ids':[proof.IGNORED_COMPILE_ID]}
        config=type('Config',(),{'serial_ignored':{}})()
        current,due,_=proof.current_census(record,self.listing({'green':False,name:True}),config)
        self.assertEqual(current,{binary+' green'});self.assertEqual(due,set())
        with self.assertRaises(r.ReceiptError):
            proof.current_census(record,self.listing({'green':False,name:False}),config)

    def test_reviewed_native_device_route_projection_never_relaxes_rust_recipe_bytes(self):
        from validation import workspace_rust_continuation as native
        old=b'name: ci\njobs:\n  check:\n    run: cargo test\n  android_device:\n    old native device\n  package_smoke:\n    run: package\n'
        new=old.replace(b'  android_device:\n    old native device\n',b'  android_apks:\n    compile APKs\n  android_device:\n    native ARM64 device\n')
        selected=lambda raw: raw.split(b'  android_',1)[1].split(b'  package_smoke:',1)[0]
        oldblock=b'  android_'+selected(old);newblock=b'  android_'+selected(new)
        after=new+native.SOURCE_GATE_UPLOAD.encode()+native.SOURCE_GATE_PAPERWORK.encode()
        with patch.object(native,'ANDROID_DEVICE_ROUTE_SHA256',{proof.retention.digest(oldblock),proof.retention.digest(newblock)}):
            self.assertTrue(native.metadata_only_workflow(old,after))
            self.assertFalse(native.metadata_only_workflow(old,after.replace(b'cargo test',b'cargo test --release')))
            self.assertFalse(native.metadata_only_workflow(old,after.replace(b'native ARM64 device',b'unreviewed device')))
