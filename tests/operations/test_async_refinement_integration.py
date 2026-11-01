"""Integrated-path fixtures for the checkpoint-capacity amendment (review PR #1263 F1-F15).

Each fixture drives the real session functions against the real physical ledger and the real
transport; only the schema validators (which read frozen JSON schemas) and the quantum child
process are replaced, exactly where the neighbouring transport fixtures replace them. Each one
asserts a decision the previous code got wrong.
"""
import ast
import hashlib
import json
import os
import sqlite3
import unittest
import uuid
from pathlib import Path
from unittest import mock

from tests.operations.test_async_snapshot_transport import (
    EXECUTION, ERRORS, MIB, PAGE, REGISTRATION, ChildPlanBase, PublicationChainCase, carrier_for, check_identity,
    stub_schema)
from validation import async_physical_policy as physical
from validation import async_refinement_provenance as provenance
from validation import async_refinement_session as session
from validation import async_snapshot_transport as transport
from validation import python_unit_receipts as receipts

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = ROOT / 'verification' / 'async-driver-refinement'


def function_node(path, name):
    tree = ast.parse(Path(path).read_text())
    return next(node for node in ast.walk(tree) if isinstance(node, ast.FunctionDef) and node.name == name)


class IntegrationBase(ChildPlanBase):
    def make_context(self, policy):
        context = super().make_context(policy)
        context.survivors = []
        return context


# ---------------------------------------------------------------------------
# F1/F2: the probe measurement is the typed physical calibration, and admission
# is exact equality with it. There is no record-times-table overhead anywhere.
# ---------------------------------------------------------------------------
class CalibrationAdmissionTests(IntegrationBase):
    def calibrated(self, runtime_pool=512 * MIB):
        policy = {'schema_version': 3, 'kind': 'physical_policy', 'table_map_sha256': physical.digest(self.source_map),
                  'families': {name: {'owner': owner, **physical.minimum_family_policy(self.source_map, name, 4 * MIB)}
                               for name, owner in physical.FAMILY_OWNERS.items()},
                  'pools': {name: 8 * MIB for name in physical.POOL_OWNERS}}
        policy['pools']['source_runtime_scratch_bytes'] = runtime_pool
        context = self.make_context(policy)
        connection = sqlite3.connect(':memory:')
        self.addCleanup(connection.close)
        rust = {'geometry': physical.install_empty_role(connection, 'rust_catalog', self.source_map)}
        geometry = session.calibration_geometry(context, self.source_map, self.out, rust)
        plan = {'unit_byte_allowance': 64 * MIB, 'unit_record_allowance': 1000, 'maximum_units': 4,
                'total_session_disk_bytes': 4096 * MIB, 'total_retained_artifact_bytes': MIB,
                'resident_bytes': MIB, 'resident_rss_bytes': 2 * MIB, 'unit_predecessor_allowance': 8}
        allowances = {'kind_a': 7}
        plan['source_bounds_utf8'] = json.dumps({'physical_policy': policy, 'record_allowances': allowances,
                                                 'source_receipt': {'receipt': 1}})
        baseline = {'source_workspace_bytes': MIB, 'existing_unpooled_bytes': 0}
        calibration = physical.calibrate_physical_policy(policy, geometry, baseline, plan, source_map=self.source_map)
        return policy, plan, allowances, geometry, calibration

    def admission(self, measurement_changes=None, calibration_changes=None):
        policy, plan, allowances, _geometry, calibration = self.calibrated()
        reg = {'accounting_method_sha256': 'a' * 64, 'source_files': {provenance.SOURCE_BOUND_SPEC: 'b' * 64}}
        inventory = {'kind_a': {'tables': 1}}
        bounds = {'physical_policy': policy, 'record_allowances': allowances,
                  'calibration': {**calibration, **(calibration_changes or {})}}
        measurement = {'free_disk_bytes': 1 << 40, 'resource_calibration': calibration,
                       'resource_method': session.physical_method(reg, plan, policy, inventory, allowances)}
        measurement.update(measurement_changes or {})
        return session.physical_admission(plan, reg, measurement, bounds, inventory), calibration

    def test_every_source_map_role_is_measured_and_the_restore_family_declares_no_database(self):
        _policy, _plan, _allowances, geometry, _calibration = self.calibrated()
        self.assertEqual(set(geometry), set(physical.FAMILY_OWNERS))
        self.assertEqual(geometry['catalog_or_verifier_restore_scratch'], [])
        for family, declared in self.source_map['families'].items():
            self.assertEqual([row['database_role'] for row in geometry[family]],
                             [row['database_role'] for row in declared])
            for observed in geometry[family]:
                self.assertGreater(observed['page_count'], 0)

    def test_admission_is_exact_equality_with_the_typed_physical_calibration(self):
        expected, calibration = self.admission()
        self.assertEqual(expected, calibration)
        self.assertEqual(expected['kind'], 'physical_calibration')

    def test_a_runtime_pool_that_cannot_hold_the_output_directories_is_not_calibrated(self):
        # The pool must exceed the measured source baseline (1 MiB here), the unit output share and
        # every control output directory, with at least a byte left for runtime scratch.
        demand = physical.runtime_pool_demand(64 * MIB)
        with self.assertRaisesRegex(physical.PhysicalPolicyError, 'every output directory'):
            self.calibrated(runtime_pool=demand + MIB)
        self.calibrated(runtime_pool=demand + MIB + 1)

    def test_the_retired_record_times_table_overheads_cannot_authorize_a_plan(self):
        with self.assertRaises(receipts.ReceiptError):
            self.admission({'resource_overheads': {'index_bytes': 1, 'wal_bytes': 1}})

    def test_a_missing_changed_or_foreign_calibration_or_method_is_refused(self):
        with self.assertRaises(receipts.ReceiptError):
            self.admission({'resource_calibration': None})
        with self.assertRaises(receipts.ReceiptError):
            self.admission(calibration_changes={'peak_bytes': 1})
        with self.assertRaises(receipts.ReceiptError):
            self.admission({'resource_method': {'id': 'record_times_table'}})

    def test_the_calibration_module_has_no_backup_or_wal_overhead_arithmetic(self):
        names = {node.id for node in ast.walk(function_node(session.__file__, 'calibrate_overheads'))
                 if isinstance(node, ast.Name)}
        self.assertFalse(names & {'index_bytes', 'wal_bytes', 'maximum_backups', 'duplicate_backup',
                                  'minimum_fixed_prefix_parts'})


# ---------------------------------------------------------------------------
# F3: the packaging preflight cannot demand transport roots before they exist.
# ---------------------------------------------------------------------------
class PackagingPreflightTests(PublicationChainCase):
    def test_reserved_transport_roots_are_pending_until_fulfilled(self):
        (self.root / 'evidence2').mkdir(mode=0o700)
        plan = transport.EvidencePlan(transport.OwnedDirectory(self.root / 'evidence2', self.root, lambda: None),
                                      sqlite3.connect(':memory:'), self.charge, self.discharge)
        self.addCleanup(plan.close)
        plan.reserve('catalog-backup/root.json', 4096, 'source_receipt')
        plan.preflight(pending=True)
        with self.assertRaises(transport.SnapshotTransportError):
            plan.preflight()

    def test_execute_and_package_only_runs_the_pending_form_before_roots_are_fulfilled(self):
        calls = [node for node in ast.walk(function_node(session.__file__, 'execute_and_package'))
                 if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == 'preflight']
        self.assertTrue(calls)
        for call in calls:
            self.assertEqual([(keyword.arg, getattr(keyword.value, 'value', None)) for keyword in call.keywords],
                             [('pending', True)])


# ---------------------------------------------------------------------------
# F4: the activation/restore request is the closed private request, not a subset.
# ---------------------------------------------------------------------------
class RestoreRequestShapeTests(unittest.TestCase):
    def private_keys(self):
        tree = ast.parse((PACKAGE / 'paged_checker.py').read_text())
        for node in ast.walk(tree):
            if isinstance(node, ast.Assign) and any(getattr(t, 'id', None) == 'PRIVATE_REQUEST_KEYS' for t in node.targets):
                return {element.value for element in node.value.elts}
        self.fail('PRIVATE_REQUEST_KEYS not found')

    def request(self):
        reg = {'source_files': {}, 'runtime': {'java': {'path': 'j', 'sha256': 'x'},
                                               'tlc': {'path': 't', 'sha256': 'y'}}}
        state = {'resource_plan': {'kind': 'ResourcePlan'}, 'checkpoint': {'kind': 'Checkpoint'}}
        unit = {'kind': 'unit-record', 'artifact_roots': []}
        return session.restore_request_base(state, reg, unit, EXECUTION, {'kind': 'InitialImage'},
                                            {'kind': 'Checkpoint', 'id': 'c'}, '/index.sqlite', '/graph.json', '/out')

    def test_the_restore_request_plus_the_staged_paths_is_exactly_the_checkers_closed_key_set(self):
        keys = set(self.request()) | {'catalog_path', 'output_dir', 'verifier_work_db'}
        self.assertEqual(keys, self.private_keys())

    def test_the_restore_request_names_the_original_execution_and_an_empty_artifact_root_set(self):
        request = self.request()
        self.assertEqual(request['execution_origin'], EXECUTION)
        self.assertEqual(request['work_unit']['artifact_roots'], [])
        self.assertEqual(request['operation'], 'validate_restore')
        self.assertTrue(Path(request['prepare_path']).is_absolute() and Path(request['reservation_path']).is_absolute())

    def test_prepare_activation_builds_its_request_from_the_planned_unit(self):
        source = ast.dump(function_node(session.__file__, 'prepare_activation'))
        self.assertIn('planned_work_unit', source)
        self.assertIn('restore_request_base', source)


# ---------------------------------------------------------------------------
# F5: the final rehearsal carrier is a restore batch of purpose `activate`.
# ---------------------------------------------------------------------------
class ConformanceCarrierTests(unittest.TestCase):
    def setUp(self):
        stub_schema(self)

    def carrier(self, purpose, result):
        value = carrier_for('transport_conformance', ordinal=3, operation='publish', purpose=purpose)
        value['transport_conformance'] = {'case_id': 'native_snapshot_three_batch_v1', 'result': result}
        return value

    def test_the_restore_batch_that_carries_the_case_result_is_an_activate_batch(self):
        check_identity(self.carrier('activate', {'kind': 'native_transport_conformance'}))

    def test_rehearsal_transfer_batches_carry_no_result_and_remain_transfers(self):
        check_identity(self.carrier('transfer', None))

    def test_a_result_on_a_transfer_batch_or_a_bare_activate_batch_is_refused(self):
        with self.assertRaises(receipts.ReceiptError):
            check_identity(self.carrier('transfer', {'kind': 'native_transport_conformance'}))
        with self.assertRaises(receipts.ReceiptError):
            check_identity(self.carrier('activate', None))

    def test_a_rehearsal_cannot_carry_an_activation_record(self):
        value = self.carrier('activate', {'kind': 'native_transport_conformance'})
        value['activation'] = {'execution_origin': EXECUTION}
        with self.assertRaises(receipts.ReceiptError):
            check_identity(value)


# ---------------------------------------------------------------------------
# F6: a failed first rehearsal never blocks the case.
# ---------------------------------------------------------------------------
class RehearsalSpoolTests(IntegrationBase):
    def setUp(self):
        super().setUp()
        for patcher in (mock.patch.object(session, 'current_physical', return_value=self.context),
                        mock.patch.object(session, 'owned_directory',
                                          side_effect=lambda path, reg=None, root=None:
                                          transport.OwnedDirectory(Path(path), Path(path).parent, lambda: None)),
                        mock.patch.object(session, 'open_database',
                                          side_effect=lambda family, path: sqlite3.connect(path))):
            patcher.start()
            self.addCleanup(patcher.stop)

    def test_a_retry_reopens_the_spool_a_failed_first_rehearsal_left_behind(self):
        first = session.PublicationStore({}, ROOT, 'conformance-case', create=True, evidence=False)
        first.close()
        with self.assertRaises(FileExistsError):
            session.PublicationStore({}, ROOT, 'conformance-case', create=True, evidence=False)
        retry = session.PublicationStore({}, ROOT, 'conformance-case', create=True, evidence=False, resume=True)
        retry.close()
        self.assertTrue((self.root / 'conformance-case' / 'ledger.sqlite').exists())

    def test_run_publish_resumes_only_the_first_rehearsal_spool(self):
        calls = [node for node in ast.walk(function_node(session.__file__, 'run_publish'))
                 if isinstance(node, ast.Call) and getattr(node.func, 'id', None) == 'PublicationStore']
        self.assertEqual(len(calls), 1)
        self.assertEqual({keyword.arg for keyword in calls[0].keywords}, {'create', 'evidence', 'resume'})


# ---------------------------------------------------------------------------
# F10/F14: progress is closure-relative; coverage is seeded from accepted lineage only.
# ---------------------------------------------------------------------------
class ClosureRelativeProgressTests(PublicationChainCase):
    def orphan(self):
        reference = self.inventory.put('snapshot_chunk', b'unrelated authenticated bytes')
        self.sender.db.execute('INSERT OR IGNORE INTO queue VALUES(?,?,?)',
                               (reference['kind'], reference['sha256'], reference['bytes']))
        self.sender.db.commit()
        return reference

    def test_authenticated_bytes_outside_the_closure_are_not_credited_and_leave_no_locator(self):
        _first, pend = self.started()
        orphan = self.orphan()
        sent = self.send(pend['pending'], {('snapshot_chunk', orphan['sha256'])})
        with self.assertRaises(transport.SnapshotTransportError):
            self.follow(pend, sent)
        self.assertEqual(self.coverage_of(pend), (pend['pending']['coverage_sha256'], pend['pending']['coverage_count']))
        self.assertEqual(self.index.db.execute('SELECT count(*) FROM native_locators WHERE sha256=?',
                                               (orphan['sha256'],)).fetchone()[0], 0)

    def test_ordinary_packing_does_not_offer_content_whose_owner_is_not_covered(self):
        orphan = self.orphan()
        sent = self.send(self.pending, None, operation='execute')
        offered = {row[0]['sha256'] for row in sent.auth['rows']}
        self.assertIn(self.intent_ref['sha256'], offered)
        self.assertNotIn(orphan['sha256'], offered)

    def test_a_parent_and_its_children_may_travel_in_one_batch(self):
        sent = self.send(self.pending, None, operation='execute')
        pend = self.begin(sent)
        self.assertTrue(self.complete(pend))

    def test_an_activation_batch_may_carry_only_validation_receipts(self):
        pend, _sent = self.deliver()
        extra = self.inventory.put('snapshot_chunk', b'not a validation receipt')
        self.sender.db.execute('INSERT OR IGNORE INTO queue VALUES(?,?,?)',
                               (extra['kind'], extra['sha256'], extra['bytes']))
        self.sender.db.commit()
        sent = self.send(pend['pending'], {('snapshot_chunk', extra['sha256'])}, purpose='activate')
        with self.assertRaises(transport.SnapshotTransportError):
            self.follow(pend, sent, activation=True)

    def test_pending_and_foreign_locators_never_seed_coverage(self):
        pend, sent = self.deliver()
        self.assertEqual(list(session.lineage_references(self.index, [])), [])
        accepted = {session.batch_digest(sent[0].batch)}
        seeded = {(row['kind'], row['sha256']) for row in session.lineage_references(self.index, accepted)}
        self.assertEqual(seeded, {(row[0]['kind'], row[0]['sha256']) for row in sent[0].auth['rows']})

    def test_a_new_publication_starts_from_empty_coverage_when_nothing_was_accepted(self):
        _pend, sent = self.deliver()  # locators of three unaccepted batches now exist in the index
        (self.root / 'history2').mkdir(mode=0o700)
        pend = session.begin_pending({'accepted_batches': []}, {}, ROOT, self.root / 'history2', sent[0].auth,
                                     self.index, None)
        self.assertEqual(pend['pending']['coverage_count'], sent[0].batch['coverage_count'])
        self.assertEqual(pend['batch_receipts'], [session.batch_digest(sent[0].batch)])


# ---------------------------------------------------------------------------
# F11/F7: crash reconciliation and the survivors a new controller process must charge.
# ---------------------------------------------------------------------------
class CrashReconciliationTests(IntegrationBase):
    def prepared_transfer(self):
        generation = self.out / 'state.restore'
        staged, budget = self.stage_file(generation, 'catalog.staged.sqlite', blobs=2)
        active = self.out / 'state'
        active.mkdir(mode=0o700)
        handle = self.context.ledger.prepare_transfer(budget.reservation_id, 'catalog', 'catalog_main', 'main',
                                                      active / 'catalog.sqlite')
        return staged, active / 'catalog.sqlite', handle

    def restarted(self):
        return self.make_context(child_policy_for(self))

    def test_a_crash_before_the_rename_keeps_the_staged_file_charged_to_restore_staging(self):
        staged, _destination, _handle = self.prepared_transfer()
        size = staged.stat().st_size
        fresh = self.restarted()
        fresh.reconcile_crash()
        self.assertEqual(fresh.ledger.used[('restore_staging_bytes', 'bytes')], size)
        self.assertEqual(fresh.ledger.used[('catalog_main', 'main')], 0)
        self.assertEqual(fresh.ledger.used[('control_bytes', 'bytes')], 0)
        self.assertEqual(list((self.root / physical.TRANSFER_DIRECTORY).glob('*.json')), [])

    def test_a_crash_after_the_rename_charges_the_destination_main_file_alone(self):
        staged, destination, _handle = self.prepared_transfer()
        size = staged.stat().st_size
        os.replace(staged, destination)
        fresh = self.restarted()
        fresh.reconcile_crash()
        self.assertEqual(fresh.ledger.used[('catalog_main', 'main')], size)
        self.assertEqual(fresh.ledger.used[('restore_staging_bytes', 'bytes')], 0)

    def test_a_lost_or_ambiguous_object_refuses_the_job_and_keeps_the_record(self):
        staged, destination, _handle = self.prepared_transfer()
        staged.unlink()
        fresh = self.restarted()
        with self.assertRaises(physical.PhysicalPolicyError):
            fresh.reconcile_crash()
        self.assertEqual(len(list((self.root / physical.TRANSFER_DIRECTORY).glob('*.json'))), 1)

    def grant_descriptor(self, name, pid, ticks):
        directory = self.root / physical.GRANT_DIRECTORY
        directory.mkdir(mode=0o700, exist_ok=True)
        path = directory / (name + '.json')
        path.write_bytes(physical.canonical({'parent': {'pid': pid, 'start_ticks': ticks}}))
        return path

    def test_a_descriptor_of_a_dead_parent_is_charged_then_removed(self):
        path = self.grant_descriptor('a' * 64, 2 ** 22 + 12345, 1)
        fresh = self.restarted()
        fresh.reconcile_crash()
        self.assertFalse(path.exists())
        self.assertEqual(fresh.ledger.used[('control_bytes', 'bytes')], 0)

    def test_a_descriptor_of_a_live_parent_refuses_the_job(self):
        path = self.grant_descriptor('b' * 64, os.getpid(), physical.process_start_ticks(os.getpid()))
        fresh = self.restarted()
        with self.assertRaises(receipts.ReceiptError):
            fresh.reconcile_crash()
        self.assertTrue(path.exists())

    def test_dead_spools_and_prior_outputs_are_removed_and_live_ones_kept(self):
        for name in ('publication-0', 'publication-1', 'conformance-abc', 'controller-api'):
            (self.root / name).mkdir()
        earlier = [session.job_output_directory(self.root, n) for n in (3, 5)]
        current = session.job_output_directory(self.root, 7)
        for directory in [*earlier, current]:
            directory.mkdir()
        (earlier[0] / 'quantum').mkdir()
        (earlier[0] / 'quantum' / 'page.json').write_bytes(b'x' * 100)
        self.context.discard_prior_outputs(current)
        self.assertFalse(any(directory.exists() for directory in earlier))
        self.assertTrue(current.exists() and self.out.exists() and (self.root / 'controller-api').exists())
        self.context.discard_dead_spools({'publication-1'})
        self.assertFalse((self.root / 'publication-0').exists() or (self.root / 'conformance-abc').exists())
        self.assertTrue((self.root / 'publication-1').exists())

    def test_the_prior_output_set_comes_from_the_constructor_the_job_output_uses(self):
        made = {session.job_output_directory(self.root, n) for n in (1, 2, 12)}
        for directory in made:
            directory.mkdir()
        # Names the constructor never produces are not job outputs, whatever they look like.
        for name in ('run-', 'run-x', 'run-007', 'run--1', 'runs-1', 'run-1.bak', 'earlier-job'):
            (self.root / name).mkdir()
        self.assertEqual(set(self.context.prior_job_outputs()), made)
        self.context.discard_prior_outputs(session.job_output_directory(self.root, 2))
        self.assertEqual(set(self.context.prior_job_outputs()), {session.job_output_directory(self.root, 2)})
        self.assertTrue((self.root / 'run-007').exists() and (self.root / 'earlier-job').exists())

    def test_discarding_prior_outputs_refuses_an_output_the_job_constructor_would_not_place(self):
        earlier = session.job_output_directory(self.root, 1)
        earlier.mkdir()
        for stranger in (self.out, self.root / 'run-007', self.root / 'elsewhere' / 'run-9'):
            stranger.mkdir(parents=True, exist_ok=True)
            with self.assertRaises(receipts.ReceiptError):  # never a silent no-op that reclaims nothing
                self.context.discard_prior_outputs(stranger)
        self.assertTrue(earlier.exists())

    def test_the_job_output_path_and_the_discard_path_cannot_diverge(self):
        source = Path(session.__file__)
        tree = ast.parse(source.read_text())
        # One literal spelling of the job output name exists in the whole module: the constructor's prefix.
        spellings = [node for node in ast.walk(tree) if isinstance(node, ast.Constant) and node.value == 'run-']
        self.assertEqual(len(spellings), 1)
        event_output = function_node(source, 'event_output')
        called = {node.func.id for node in ast.walk(event_output) if isinstance(node, ast.Call)
                  and isinstance(node.func, ast.Name)}
        self.assertIn('job_output_directory', called)  # the job places its output with the constructor ...
        discard = function_node(source, 'prior_job_outputs')
        called = {node.func.id for node in ast.walk(discard) if isinstance(node, ast.Call)
                  and isinstance(node.func, ast.Name)}
        self.assertIn('job_output_directory', called)  # ... and the discard finds earlier outputs with it
        body = function_node(source, '_runner_body')
        self.assertIn('discard_prior_outputs', {node.func.attr for node in ast.walk(body)
                                                if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)})

    def test_only_spools_an_authenticated_chain_can_still_use_are_live(self):
        reg = {'source': {'commit': '0' * 40}}
        control = {'session_id': str(uuid.UUID(int=1))}
        state = {'pending_publication': {'execution_origin': {'dispatch_ordinal': 3}},
                 'anchor': {'sha256': 'a' * 64}, 'conformance_result': None}
        case = transport.conformance_case_sha256(provenance.digest(provenance.canonical(reg)), control['session_id'],
                                                 'a' * 64)
        self.assertEqual(session.live_spools(state, reg, control), {'publication-3', 'conformance-' + case[:32]})
        state['conformance_result'] = {'kind': 'native_transport_conformance'}
        state['pending_publication'] = None
        self.assertEqual(session.live_spools(state, reg, control), set())


def child_policy_for(test):
    from tests.operations.test_async_snapshot_transport import child_policy
    return child_policy(test.source_map)


# ---------------------------------------------------------------------------
# F15: observed identities, root origins, completion and hygiene.
# ---------------------------------------------------------------------------
class ObservedIdentityTests(IntegrationBase):
    def setUp(self):
        super().setUp()
        patcher = mock.patch.object(session, 'current_physical', return_value=self.context)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_activation_commits_the_identity_the_filesystem_reports_for_the_moved_file(self):
        staged, budget = self.stage_file(self.out / 'state.restore', 'catalog.staged.sqlite', blobs=2)
        restored = {'generation': self.out / 'state.restore',
                    'staged': [{'budget': budget, 'path': staged, 'family': 'catalog_main', 'owner': 'catalog',
                                'name': 'catalog.sqlite'}]}
        real = os.lstat
        active = self.out / 'state' / 'catalog.sqlite'

        def lying(path, *args, **kwargs):
            info = real(path, *args, **kwargs)
            if Path(path) == active:
                return os.stat_result((info.st_mode, info.st_ino + 1, info.st_dev, info.st_nlink, info.st_uid,
                                       info.st_gid, info.st_size, info.st_atime, info.st_mtime, info.st_ctime))
            return info
        with mock.patch.object(session.os, 'lstat', side_effect=lying):
            with self.assertRaises(physical.PhysicalPolicyError):
                session.activate_generation(restored, self.out, {})


class RootOriginTests(IntegrationBase):
    def test_a_snapshot_root_sealed_by_another_execution_is_refused(self):
        session.check_root_origin({'execution_origin': EXECUTION}, EXECUTION)
        with self.assertRaises(receipts.ReceiptError):
            session.check_root_origin({'execution_origin': dict(EXECUTION, run=99)}, EXECUTION)

    def test_restore_checks_the_root_origin_before_staging_any_byte(self):
        foreign = transport.canonical({'execution_origin': dict(EXECUTION, run=99)})
        with mock.patch.object(session, 'current_physical', return_value=self.context), \
                mock.patch.object(session, 'owned_directory',
                                  side_effect=lambda path, reg=None, root=None:
                                  transport.OwnedDirectory(Path(path), Path(path).parent, lambda: None)), \
                mock.patch.object(provenance, 'auxiliary', side_effect=lambda value, root: value):
            with self.assertRaises(receipts.ReceiptError):
                session.restore_snapshots({}, ROOT, {}, self.out, {}, ROOT, self.root / 'index.sqlite',
                                          lambda reference: foreign, {'kind': 'catalog_backup'}, None, {}, EXECUTION)
        self.assertEqual(list((self.out / 'state.restore').iterdir()), [])


class CompletionAndHygieneTests(unittest.TestCase):
    def test_completion_requires_the_exact_current_source_conformance_result(self):
        state = {'stage': 'complete', 'phase': 'qualified', 'conformance_result': None,
                 'registration': {'source': {'commit': '0' * 40}}}
        with self.assertRaisesRegex(receipts.ReceiptError, 'conformance result'):
            session.completion(state, {}, [], {}, {}, ROOT)
        state['conformance_result'] = {'kind': 'native_transport_conformance', 'case_id': 'other', 'complete': True,
                                       'acceptance': False, 'source': {'commit': '0' * 40}}
        with self.assertRaisesRegex(receipts.ReceiptError, 'conformance result'):
            session.completion(state, {}, [], {}, {}, ROOT)

    def test_no_dead_resource_plan_id_hedge_remains_in_expected_validations(self):
        self.assertNotIn('resource_plan_id', ast.dump(function_node(session.__file__, 'expected_validations')))

    def test_test_scratch_directories_inside_the_checkout_are_ignored(self):
        lines = (ROOT / '.gitignore').read_text().splitlines()
        self.assertIn('.agent-proof-fixtures/', lines)
        self.assertIn('.async-proof-sessions/', lines)

    def test_the_parent_identity_is_the_platform_neutral_physical_helper(self):
        self.assertIs(session.process_start_ticks.__globals__['physical'], physical)
        self.assertEqual(session.process_start_ticks(os.getpid()), physical.process_start_ticks(os.getpid()))


if __name__ == '__main__':
    unittest.main()
