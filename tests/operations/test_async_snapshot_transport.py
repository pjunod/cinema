"""Defined-only mutation fixtures for private snapshot byte primitives."""
from pathlib import Path
import tempfile
import unittest

from validation import async_snapshot_transport as transport

ROOT = Path(__file__).resolve().parents[2]


class SnapshotPrimitiveTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / '.agent-proof-fixtures' / 'snapshot-transport'
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.cas = self.root / 'cas'
        self.cas.mkdir(mode=0o700)
        self.charged = 0
        self.lease_valid = True

    def lease(self):
        if not self.lease_valid:
            raise transport.SnapshotTransportError('expired fixture lease')

    def reserve(self, size):
        self.charged += size

    def release(self, size):
        self.charged -= size

    def owned(self):
        return transport.OwnedDirectory(self.cas, self.root, self.lease)

    def test_fixed_chunks_preserve_exact_tail_and_empty_image(self):
        snapshot = self.cas / 'snapshot'
        snapshot.write_bytes(b'a' * transport.CHUNK_BYTES + b'tail')
        snapshot.chmod(0o600)
        with self.owned() as owned:
            chunks = list(transport.iter_chunks(owned, 'snapshot'))
            self.assertEqual([len(row[1]) for row in chunks], [transport.CHUNK_BYTES, 4])
            self.assertEqual([row[0] for row in chunks], [0, 1])
            snapshot.write_bytes(b'')
            self.assertEqual(list(transport.iter_chunks(owned, 'snapshot')), [])

    def test_reuse_rehashes_bytes_and_never_charges_twice(self):
        with self.owned() as owned:
            digest, size, fresh = transport.store_chunk(owned, b'exact bytes', self.reserve, self.release)
            self.assertTrue(fresh)
            self.assertEqual(self.charged, size)
            self.assertEqual(transport.store_chunk(owned, b'exact bytes', self.reserve, self.release),
                             (digest, size, False))
            self.assertEqual(self.charged, size)
            (self.cas / digest).write_bytes(b'wrong bytes')
            with self.assertRaises(transport.SnapshotTransportError):
                transport.store_chunk(owned, b'exact bytes', self.reserve, self.release)

    def test_symlink_and_external_hardlink_chunks_are_rejected(self):
        outside = self.root / 'outside'
        outside.write_bytes(b'bytes')
        outside.chmod(0o600)
        with self.owned() as owned:
            digest, size, _ = transport.store_chunk(owned, b'bytes', self.reserve, self.release)
            member = self.cas / digest
            member.unlink()
            member.symlink_to(outside)
            with self.assertRaises(OSError):
                transport.verify_chunk(owned, digest, size)
            member.unlink()
            member.hardlink_to(outside)
            with self.assertRaises(transport.SnapshotTransportError):
                transport.verify_chunk(owned, digest, size)

    def test_expired_lease_and_replaced_directory_prevent_writes(self):
        with self.owned() as owned:
            self.lease_valid = False
            with self.assertRaises(transport.SnapshotTransportError):
                transport.store_chunk(owned, b'bytes', self.reserve, self.release)
            self.lease_valid = True
            self.cas.rename(self.root / 'previous')
            self.cas.mkdir(mode=0o700)
            with self.assertRaises(transport.SnapshotTransportError):
                transport.store_chunk(owned, b'bytes', self.reserve, self.release)
        self.assertEqual(self.charged, 0)

    def test_mutation_between_streamed_chunks_is_rejected(self):
        snapshot = self.cas / 'snapshot'
        snapshot.write_bytes(b'a' * (transport.CHUNK_BYTES + 1))
        snapshot.chmod(0o600)
        with self.owned() as owned:
            chunks = transport.iter_chunks(owned, 'snapshot')
            next(chunks)
            snapshot.write_bytes(b'short')
            with self.assertRaises(transport.SnapshotTransportError):
                list(chunks)

    def test_budget_refusal_creates_no_cas_or_temporary_member(self):
        def refuse(_size):
            raise transport.SnapshotTransportError('admitted pool exhausted')

        with self.owned() as owned:
            with self.assertRaises(transport.SnapshotTransportError):
                transport.store_chunk(owned, b'bytes', refuse, self.release)
        self.assertEqual(list(self.cas.iterdir()), [])


# ---------------------------------------------------------------------------
# Result-grant, closure, rehearsal and controller-routing fixtures. These are
# defined-only: they are reviewed before any execution and each asserts a rule
# the corresponding wrong behavior would violate.
# ---------------------------------------------------------------------------
import hashlib
import json
import os
import re
import sqlite3
from unittest import mock

from validation import async_refinement_provenance as provenance
from validation import async_refinement_session as session
from validation import python_unit_receipts as receipts

EXECUTION = {'repository': 1, 'pr': 2, 'event_base': 'a' * 40, 'workflow_id': 'async-proof-session.yml',
             'event_name': 'workflow_dispatch', 'mode': 'unit', 'operation': 'execute',
             'run': 11, 'job': 12, 'task': 13, 'attempt': 1, 'dispatch_ordinal': 0}


def execution_result(unit_ordinal=0):
    unit = {'unit_ordinal': unit_ordinal, 'run': 11, 'job': 12, 'task': 13, 'attempt': 1, 'mode': 'unit',
            'artifact_roots': [], 'stage': 'expand', 'status': 'checkpointed_incomplete'}
    return {'schema_version': 6, 'work_unit': unit, 'checkpoint': {'kind': 'Checkpoint'},
            'initial_image': {'kind': 'InitialImage'}, 'session': {'kind': 'Session'}, 'artifacts': ['a'],
            'stage_complete': False}


def receipt(artifact_id, run=11, job=12, task=13, unit_ordinal=0):
    return {'schema_version': 6, 'kind': 'ArtifactReceipt', 'record': {
        'artifact_id': artifact_id, 'run': run, 'job': job, 'task': task, 'attempt': 1,
        'unit_ordinal': unit_ordinal}}


def reference(value, identity):
    return {'kind': value['kind'], 'id': identity,
            'sha256': hashlib.sha256(transport.canonical(value)).hexdigest()}


class PlannedAcceptedResultTests(unittest.TestCase):
    def test_planned_result_carries_no_artifact_authority_and_renames_the_unit(self):
        planned = transport.planned_result(execution_result(), EXECUTION)
        self.assertEqual(set(planned), transport.PLANNED_KEYS)
        self.assertNotIn('work_unit', planned)
        self.assertEqual(planned['planned_work_unit']['artifact_roots'], [])

    def test_planned_result_refuses_preclaimed_roots_or_a_publisher_origin(self):
        claimed = execution_result()
        claimed['work_unit']['artifact_roots'] = [{'kind': 'ArtifactReceipt', 'id': 1, 'sha256': 'f' * 64}]
        with self.assertRaises(transport.SnapshotTransportError):
            transport.planned_result(claimed, EXECUTION)
        with self.assertRaises(transport.SnapshotTransportError):
            transport.planned_result(execution_result(), {**EXECUTION, 'operation': 'publish'})
        with self.assertRaises(transport.SnapshotTransportError):
            transport.planned_result(execution_result(), {**EXECUTION, 'task': 99})

    def test_accepted_result_roots_are_exactly_the_direct_original_receipts(self):
        planned = transport.planned_result(execution_result(), EXECUTION)
        receipts_ = [receipt(21), receipt(22)]
        accepted = transport.accepted_result(planned, EXECUTION, receipts_, reference)
        self.assertEqual(set(accepted), transport.ACCEPTED_KEYS)
        self.assertEqual(accepted['work_unit']['artifact_roots'],
                         [reference(receipts_[0], 21), reference(receipts_[1], 22)])
        self.assertEqual(planned['planned_work_unit']['artifact_roots'], [])

    def test_a_later_publisher_receipt_can_never_become_a_root(self):
        planned = transport.planned_result(execution_result(), EXECUTION)
        with self.assertRaises(transport.SnapshotTransportError):
            transport.accepted_result(planned, EXECUTION, [receipt(21), receipt(31, run=77, job=78, task=79)], reference)
        with self.assertRaises(transport.SnapshotTransportError):
            transport.accepted_result(planned, EXECUTION, [receipt(21), receipt(22, unit_ordinal=1)], reference)

    def test_empty_and_duplicate_original_artifact_sets_are_refused(self):
        planned = transport.planned_result(execution_result(), EXECUTION)
        with self.assertRaises(transport.SnapshotTransportError):
            transport.accepted_result(planned, EXECUTION, [], reference)
        with self.assertRaises(transport.SnapshotTransportError):
            transport.accepted_result(planned, EXECUTION, [receipt(21), receipt(21)], reference)

    def test_planned_bytes_are_independent_of_any_receipt_so_no_hash_cycle_exists(self):
        planned = transport.planned_result(execution_result(), EXECUTION)
        before = transport.content_ref('planned_execution_result', transport.canonical(planned))
        first = transport.accepted_result(planned, EXECUTION, [receipt(21)], reference)
        second = transport.accepted_result(planned, EXECUTION, [receipt(21), receipt(22)], reference)
        self.assertNotEqual(first['work_unit'], second['work_unit'])
        self.assertEqual(before, transport.content_ref('planned_execution_result', transport.canonical(planned)))


def chunk(sha_char, size=7):
    return {'kind': 'snapshot_chunk', 'sha256': sha_char * 64, 'bytes': size}


def snapshot_node(chunks):
    raw = transport.canonical({'schema_version': 1, 'kind': 'snapshot_manifest_node', 'level': 0,
                               'entries': [{'content': value} for value in chunks]})
    return raw, transport.content_ref('snapshot_manifest_node', raw)


class ConformanceCaseTests(unittest.TestCase):
    def test_case_namespace_binds_registration_session_and_anchor(self):
        base = transport.conformance_case_sha256('1' * 64, 'u' * 32, '2' * 64)
        self.assertEqual(base, transport.conformance_case_sha256('1' * 64, 'u' * 32, '2' * 64))
        self.assertNotEqual(base, transport.conformance_case_sha256('3' * 64, 'u' * 32, '2' * 64))
        self.assertNotEqual(base, transport.conformance_case_sha256('1' * 64, 'v' * 32, '2' * 64))
        self.assertNotEqual(base, transport.conformance_case_sha256('1' * 64, 'u' * 32, '4' * 64))

    def test_offer_forces_one_real_chunk_per_first_two_batches_then_the_remainder(self):
        chunks = [chunk('a'), chunk('b'), chunk('c')]
        self.assertEqual(transport.conformance_offer(0, chunks), {('snapshot_chunk', 'a' * 64)})
        self.assertEqual(transport.conformance_offer(1, chunks), {('snapshot_chunk', 'b' * 64)})
        self.assertIsNone(transport.conformance_offer(2, chunks))
        with self.assertRaises(transport.SnapshotTransportError):
            transport.conformance_offer(0, chunks[:1])  # no substitute chunk is ever generated

    def test_chunk_walk_deduplicates_sorts_and_reads_both_complete_snapshots(self):
        catalog_raw, catalog_ref = snapshot_node([chunk('b'), chunk('a')])
        verifier_raw, verifier_ref = snapshot_node([chunk('c'), chunk('a')])
        stored = {catalog_ref['sha256']: catalog_raw, verifier_ref['sha256']: verifier_raw}
        found = transport.conformance_chunks({'manifest_root': catalog_ref}, {'manifest_root': verifier_ref},
                                             lambda reference: stored[reference['sha256']])
        self.assertEqual([row['sha256'] for row in found], ['a' * 64, 'b' * 64, 'c' * 64])

    def test_a_leaf_that_is_not_a_snapshot_chunk_is_not_a_conformance_chunk(self):
        raw = transport.canonical({'schema_version': 1, 'kind': 'snapshot_manifest_node', 'level': 0,
                                   'entries': [{'content': {**chunk('a'), 'kind': 'padding'}}]})
        reference_ = transport.content_ref('snapshot_manifest_node', raw)
        with self.assertRaises(transport.SnapshotTransportError):
            transport.conformance_chunks({'manifest_root': reference_}, {'manifest_root': reference_},
                                         lambda _reference: raw)

    def test_two_batch_success_and_padded_first_batches_are_false_passes(self):
        chunks = [chunk('a'), chunk('b'), chunk('c')]
        first, second = [('snapshot_chunk', 'a' * 64)], [('snapshot_chunk', 'b' * 64)]
        third = [('snapshot_chunk', 'c' * 64), ('publication_intent', 'd' * 64)]
        transport.check_conformance_payloads(chunks, [first, second, third])
        with self.assertRaises(transport.SnapshotTransportError):
            transport.check_conformance_payloads(chunks, [first, second])
        with self.assertRaises(transport.SnapshotTransportError):
            transport.check_conformance_payloads(chunks, [first + [('snapshot_chunk', 'c' * 64)], second, third[1:]])
        with self.assertRaises(transport.SnapshotTransportError):
            transport.check_conformance_payloads(chunks, [second, first, third])
        with self.assertRaises(transport.SnapshotTransportError):
            transport.check_conformance_payloads(chunks, [first, second, []])
        with self.assertRaises(transport.SnapshotTransportError):
            transport.check_conformance_payloads(chunks, [first, second, first])  # credited twice


def structured_database():
    database = sqlite3.connect(':memory:')
    return database, transport.PublicationLedger(database)


class ReceiverClosureTests(unittest.TestCase):
    def intent(self):
        raw_node, node = snapshot_node([chunk('a')])
        catalog = transport.canonical({'kind': 'catalog_backup', 'manifest_root': node})
        catalog_ref = transport.content_ref('catalog_backup', catalog)
        result = transport.content_ref('planned_execution_result', b'planned')
        evidence_node = transport.canonical({'kind': 'evidence_manifest_node', 'level': 0, 'entries': []})
        evidence = transport.content_ref('evidence_manifest_node', evidence_node)
        intent = transport.canonical({'kind': 'publication_intent', 'catalog_root': catalog_ref, 'verifier_root': None,
                                      'execution_result': result, 'evidence_root': evidence})
        store = {catalog_ref['sha256']: catalog, node['sha256']: raw_node, evidence['sha256']: evidence_node}
        return transport.content_ref('publication_intent', intent), intent, store, [catalog_ref, node, result, evidence]

    def test_children_are_queued_only_from_covered_authenticated_parents(self):
        intent_ref, intent, store, required = self.intent()
        store[intent_ref['sha256']] = intent
        _db, ledger = structured_database()
        ledger.db.execute('INSERT INTO queue VALUES(?,?,?)', (intent_ref['kind'], intent_ref['sha256'], intent_ref['bytes']))
        self.assertEqual(transport.expand_with_resolver(ledger, lambda row: store[row['sha256']]), 0)  # uncovered
        ledger.authenticate([intent_ref])
        transport.expand_with_resolver(ledger, lambda row: store[row['sha256']])
        queued = {row[1] for row in ledger.db.execute('SELECT kind,sha256 FROM queue')}
        self.assertTrue({row['sha256'] for row in required[:1] + required[2:]} <= queued)
        self.assertFalse(transport.closure_complete(ledger))

    def test_closure_requires_every_discovered_child_to_be_covered_and_expanded(self):
        intent_ref, intent, store, required = self.intent()
        store[intent_ref['sha256']] = intent
        _db, ledger = structured_database()
        ledger.db.execute('INSERT INTO queue VALUES(?,?,?)', (intent_ref['kind'], intent_ref['sha256'], intent_ref['bytes']))
        resolver = lambda row: store[row['sha256']]
        ledger.authenticate([intent_ref])
        transport.expand_with_resolver(ledger, resolver)
        for row in required:
            ledger.authenticate([row])
            transport.expand_with_resolver(ledger, resolver)
        chunk_ref = chunk('a')
        self.assertFalse(transport.closure_complete(ledger))
        ledger.authenticate([chunk_ref])
        self.assertTrue(transport.closure_complete(ledger))

    def test_the_frozen_intent_is_always_offered_before_other_missing_content(self):
        _db, ledger = structured_database()
        for kind, char in (('catalog_backup', '1'), ('evidence_manifest_node', '2'), ('publication_intent', '9')):
            ledger.db.execute('INSERT INTO queue VALUES(?,?,?)', (kind, char * 64, 5))
        self.assertEqual(next(transport.missing_content(ledger))['kind'], 'publication_intent')


class AuthenticatedResolverTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / '.agent-proof-fixtures' / 'snapshot-resolver'
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.index = provenance.DependencyIndex(self.root / 'index.sqlite')
        self.addCleanup(self.index.close)
        self.parts = self.root / 'parts'
        self.data = b'native member bytes'
        self.reference = transport.content_ref('snapshot_chunk', self.data)
        self.name = 'content/snapshot_chunk/' + self.reference['sha256']

    def place(self, data, artifact=5):
        path = self.parts / str(artifact) / self.name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def locate(self, artifact=5):
        self.index.add_locator(0, {'batch_receipt_sha256': 'b' * 64, 'publication_origin': EXECUTION,
            'artifact_id': artifact, 'part_ordinal': 0, 'member_path': self.name,
            'bytes': self.reference['bytes'], 'sha256': self.reference['sha256']})

    def resolver(self):
        return transport.AuthenticatedResolver(self.index.db, self.parts)

    def test_bytes_resolve_only_after_an_authenticated_native_locator_exists(self):
        self.place(self.data)
        with self.assertRaises(transport.SnapshotTransportError):
            self.resolver()(self.reference)  # a matching local file is not authority
        self.locate()
        self.assertEqual(self.resolver()(self.reference), self.data)

    def test_mutated_or_aliased_member_bytes_are_refused_even_with_a_locator(self):
        path = self.place(b'x' * len(self.data))
        self.locate()
        with self.assertRaises(transport.SnapshotTransportError):
            self.resolver()(self.reference)
        path.unlink()
        outside = self.root / 'outside'
        outside.write_bytes(self.data)
        path.symlink_to(outside)
        with self.assertRaises(OSError):
            self.resolver()(self.reference)

    def test_a_second_batch_rehearsal_index_gives_no_credit_from_historical_locators(self):
        self.place(self.data)
        self.locate()
        rehearsal = provenance.DependencyIndex(self.root / 'rehearsal.sqlite')
        self.addCleanup(rehearsal.close)
        with self.assertRaises(transport.SnapshotTransportError):
            transport.AuthenticatedResolver(rehearsal.db, self.parts)(self.reference)


class FakeReservation:
    def __init__(self, capacity):
        self.capacity, self.bytes, self.reservation_id = capacity, 0, 'r' * 64

    def grow(self, bytes_):
        if bytes_ > self.capacity:
            raise session.physical.PhysicalIncomplete('prospective physical grant growth exhausted')
        self.bytes = bytes_


class FakeLedger:
    def __init__(self, capacity):
        self.capacity, self.reservations = capacity, []

    def reserve(self, owner, pool, component, bytes_):
        self.reservations.append((owner, pool, component, bytes_))
        return FakeReservation(self.capacity)


class FakeContext:
    def __init__(self, capacity):
        self.ledger = FakeLedger(capacity)


class PoolBudgetTests(unittest.TestCase):
    def test_pool_charges_grow_the_held_reservation_only_by_what_is_written(self):
        context = FakeContext(100)
        budget = session.PoolBudget(context, 'cas_bytes')
        self.assertEqual(context.ledger.reservations, [('provenance', 'cas_bytes', 'bytes', 0)])
        budget.reserve(60)
        self.assertEqual(budget.reservation.bytes, 60)
        budget.release(60)
        budget.reserve(30)
        self.assertEqual(budget.reservation.bytes, 60)  # the held deduction never shrinks before removal

    def test_exhaustion_refuses_without_partial_accounting(self):
        budget = session.PoolBudget(FakeContext(10), 'cas_bytes')
        budget.reserve(8)
        with self.assertRaises(session.physical.PhysicalIncomplete):
            budget.reserve(3)
        self.assertEqual(budget.used, 8)
        with self.assertRaises(session.physical.PhysicalIncomplete):
            budget.reserve(-1)

    def test_preexisting_surviving_files_stay_charged(self):
        with self.assertRaises(session.physical.PhysicalIncomplete):
            session.PoolBudget(FakeContext(10), 'cas_bytes', existing=11)


class FakeGrant:
    def __init__(self, environment):
        self.value = environment

    def environment(self):
        return dict(self.value)


class ChildGrantEnvironmentTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / '.agent-proof-fixtures' / 'child-environment'
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.output = Path(self.temporary.name) / 'run'
        self.output.mkdir()
        self.target = Path(self.temporary.name) / 'compiler-cache'
        self.target.mkdir()

    def test_stale_grant_variables_are_dropped_and_only_the_explicit_grant_is_passed(self):
        stale = {'PLURX_ASYNC_PHYSICAL_GRANT_FD': '99', 'PLURX_ASYNC_PHYSICAL_GRANT_SHA256': '0' * 64,
                 'PLURX_ASYNC_PHYSICAL_LEASE_FD': '98', 'GITHUB_TOKEN': 'secret', 'CARGO_TARGET_DIR': str(self.target)}
        fresh = {'PLURX_ASYNC_PHYSICAL_GRANT_FD': '7', 'PLURX_ASYNC_PHYSICAL_GRANT_SHA256': '1' * 64,
                 'PLURX_ASYNC_PHYSICAL_LEASE_FD': '8'}
        with mock.patch.dict(os.environ, stale):
            environment = session.child_environment({}, self.output / 'request.json', self.output, self.output,
                                                    FakeGrant(fresh))
        for key, value in fresh.items():
            self.assertEqual(environment[key], value)
        self.assertNotIn('GITHUB_TOKEN', environment)

    def test_a_child_without_an_explicit_grant_or_with_a_malformed_one_is_refused(self):
        with mock.patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.target)}):
            with self.assertRaisesRegex(receipts.ReceiptError, 'requires an explicit physical grant'):
                session.child_environment({}, self.output / 'request.json', self.output, self.output, None)
            with self.assertRaisesRegex(receipts.ReceiptError, 'child grant environment differs'):
                session.child_environment({}, self.output / 'request.json', self.output, self.output,
                                          FakeGrant({'PLURX_ASYNC_PHYSICAL_GRANT_FD': '7'}))


def routing_state(**changes):
    state = {'pending_publication': None, 'conformance_pending': None, 'anchor': None,
             'conformance_result': None, 'stage': 'expand', 'checkpoint': None}
    state.update(changes)
    return state


class PublicationRoutingTests(unittest.TestCase):
    def test_an_unfinished_publication_is_continued_before_anything_else(self):
        self.assertEqual(session.expected_dispatch(routing_state()), 'execute')
        self.assertEqual(session.expected_dispatch(routing_state(pending_publication={'x': 1}, anchor={'sha256': 'a' * 64})),
                         'publish')

    def test_rehearsal_precedes_further_semantic_work_and_blocks_completion(self):
        anchor = {'sha256': 'a' * 64}
        self.assertEqual(session.expected_dispatch(routing_state(anchor=anchor)), 'conformance')
        self.assertEqual(session.expected_dispatch(routing_state(anchor=anchor, conformance_pending={'x': 1})),
                         'conformance')
        self.assertEqual(session.expected_dispatch(routing_state(anchor=anchor, conformance_result={'ok': True})),
                         'execute')
        checkpoint = {'kind': 'Checkpoint', 'record': {}}
        final = routing_state(stage='complete', checkpoint=checkpoint, anchor=anchor,
                              final_input_checkpoint_sha256=provenance.digest(provenance.canonical(checkpoint)))
        self.assertEqual(session.expected_dispatch(final), 'conformance')
        final['conformance_result'] = {'ok': True}
        self.assertEqual(session.expected_dispatch(final), 'complete')

    def test_dispatch_kind_separates_ordinary_publish_from_rehearsal(self):
        base = {'mode': 'unit', 'conformance': None}
        self.assertEqual(session.item_kind({**base, 'operation': 'execute'}), 'execute')
        self.assertEqual(session.item_kind({**base, 'operation': 'publish'}), 'publish')
        self.assertEqual(session.item_kind({**base, 'operation': 'publish', 'conformance': {'case_id': 'x'}}),
                         'conformance')
        self.assertEqual(session.item_kind({'mode': 'complete', 'conformance': None, 'operation': 'execute'}),
                         'complete')

    def test_conformance_request_is_bound_to_registration_session_and_first_pair(self):
        registration = {'source': 'x'}
        request, case = session.conformance_request(routing_state(anchor={'sha256': 'a' * 64}), registration, 'u' * 32)
        self.assertEqual(request, {'case_id': 'native_snapshot_three_batch_v1', 'anchor_intent_sha256': 'a' * 64})
        self.assertEqual(case, transport.conformance_case_sha256(
            provenance.digest(provenance.canonical(registration)), 'u' * 32, 'a' * 64))
        self.assertNotEqual(case, session.conformance_request(routing_state(anchor={'sha256': 'b' * 64}),
                                                              registration, 'u' * 32)[1])

    def test_original_control_view_exposes_only_the_direct_original_execution(self):
        unit = {'schema_version': 6, 'kind': 'WorkUnit', 'record': {'session_id': 's', 'unit_ordinal': 3}}
        view = session.original_control({'unit': unit, 'original_receipts': [receipt(21)]})
        self.assertEqual((view['work_unit'], view['artifact_receipts'], view['completion']), (unit, [receipt(21)], None))

    def test_workflow_offers_the_publish_operation(self):
        text = (ROOT / '.github/workflows/async-proof-session.yml').read_text()
        self.assertIn('options: [probe, execute, publish]', text)



# ---------------------------------------------------------------------------
# Publication chain: sender plans real bounded batches over a genuine two-chunk
# snapshot; the controller's own replay functions credit them. `only` offers are
# the per-test native cap, forcing at least three transfer batches.
# ---------------------------------------------------------------------------
import copy
import shutil

SOURCE_ID = {key: 'a' * (40 if key in ('commit', 'tree') else 64) for key in
             ('commit', 'tree', 'source_closure_sha256', 'driver_sha256', 'finite_inventory_sha256',
              'proof_contract_sha256', 'toolchain_sha256', 'workflow_sha256')}
SESSION_ID = '00000000-0000-4000-8000-000000000001'
CHAIN_COUNTS = {key: 0 for key in ('physical_rows', 'episodes', 'stores', 'oracle_edges', 'products', 'candidates',
                'demands', 'product_edges', 'fair_pairs', 'enabledness', 'expansions', 'frontier',
                'monitor_nodes', 'monitor_edges', 'components', 'boundaries')}


def chain_checkpoint():
    return {'schema_version': 6, 'kind': 'Checkpoint', 'record': {
        'session_id': SESSION_ID, 'sequence': 0, 'source': SOURCE_ID, 'initial_image_id': 'b' * 64,
        'resource_plan_sha256': 'c' * 64, 'unit_ordinal': 0, 'previous_checkpoint': None,
        'committed_transaction': 1, 'oldest_pending_ticket': None, 'counts': CHAIN_COUNTS, 'contexts': [],
        'page_root': 'd' * 64, 'catalog_logical_digest': 'e' * 64}}


class Sent:
    def __init__(self, auth, batch, pending_after, plan):
        self.auth, self.batch, self.pending_after, self.plan = auth, batch, pending_after, plan


class PublicationChainCase(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / '.agent-proof-fixtures' / 'publication-chain'
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        for name in ('work', 'cas', 'evidence', 'src', 'parts', 'history', 'stages'):
            (self.root / name).mkdir(mode=0o700)
        self.charged = 0
        cas = transport.OwnedDirectory(self.root / 'cas', self.root, lambda: None)
        self.addCleanup(cas.close)
        self.inventory = transport.ContentInventory(sqlite3.connect(':memory:'), cas, self.charge, self.discharge)
        self.sender = transport.PublicationLedger(sqlite3.connect(':memory:'))
        self.index = provenance.DependencyIndex(self.root / 'index.sqlite')
        self.addCleanup(self.index.close)
        self.original = None
        self.unit_envelope = None
        self.sends = 0
        self.build()

    def charge(self, size):
        self.charged += size

    def discharge(self, size):
        self.charged -= size

    def build(self):
        self.cp = chain_checkpoint()
        record = self.cp['record']
        backup = self.root / 'work' / 'backup.sqlite'
        database = sqlite3.connect(backup)
        database.execute('CREATE TABLE payload(value BLOB)')
        blob, counter = bytearray(), 0
        while len(blob) < 1300 * 1024:
            blob.extend(hashlib.sha256(counter.to_bytes(8, 'big')).digest())
            counter += 1
        database.execute('INSERT INTO payload VALUES(?)', (bytes(blob),))
        database.commit()
        database.close()
        backup.chmod(0o600)
        self.binding = {'source': SOURCE_ID, 'session_id': SESSION_ID, 'unit_ordinal': 0,
                        'resource_plan_sha256': record['resource_plan_sha256'],
                        'initial_image_sha256': record['initial_image_id']}
        descriptor = {'source': SOURCE_ID, 'session_id': SESSION_ID, 'unit_ordinal': 0,
            'checkpoint_sha256': hashlib.sha256(transport.canonical(self.cp)).hexdigest(),
            'previous_checkpoint_sha256': None, 'resource_plan_sha256': record['resource_plan_sha256'],
            'initial_image_sha256': record['initial_image_id'], 'catalog_commit': 1,
            'catalog_logical_digest': record['catalog_logical_digest'], 'semantic_manifest_root': record['page_root'],
            'snapshot_boundary': 'post_checkpoint_pre_upload', 'format': 'sqlite3',
            'backup_method': 'sqlite_backup_api_v1', 'catalog_bytes': backup.stat().st_size,
            'catalog_sha256': hashlib.sha256(backup.read_bytes()).hexdigest()}
        work = transport.OwnedDirectory(self.root / 'work', self.root, lambda: None)
        self.addCleanup(work.close)
        self.catalog, catalog_ref = transport.snapshot_root(self.inventory, self.binding, EXECUTION, self.cp,
                                                            descriptor, work, 'backup.sqlite')
        source = self.root / 'src' / 'unit.json'
        source.write_bytes(b'{"evidence":"original full-D member"}')
        source.chmod(0o600)
        self.evidence = transport.EvidencePlan(
            transport.OwnedDirectory(self.root / 'evidence', self.root, lambda: None),
            sqlite3.connect(':memory:'), self.charge, self.discharge)
        self.addCleanup(self.evidence.close)
        self.evidence.add_file('unit.json', source)
        evidence_ref = transport.evidence_root(self.inventory, self.binding, self.evidence)
        unit = {'unit_ordinal': 0, 'run': 11, 'job': 12, 'task': 13, 'attempt': 1, 'mode': 'unit',
                'artifact_roots': [], 'expected_prior_checkpoint': None, 'stage': 'expand',
                'status': 'checkpointed_incomplete'}
        result = {'schema_version': 6, 'work_unit': unit, 'checkpoint': self.cp,
                  'initial_image': {'kind': 'InitialImage'}, 'session': {'kind': 'Session'},
                  'artifacts': ['unit.json'], 'stage_complete': False}
        self.intent, self.pending = transport.prepare_publication(
            self.inventory, self.sender, self.binding, EXECUTION, result, catalog_ref, None, evidence_ref)
        self.intent_ref = self.pending['intent']
        self.planned = transport.planned_result(result, EXECUTION)
        self.chunks = sorted(row[0] for row in self.inventory.db.execute(
            "SELECT sha256 FROM content WHERE kind='snapshot_chunk'"))
        self.unit_record = unit
        self.assertGreaterEqual(len(self.chunks), 2)

    def send(self, pending, offer, purpose='transfer', operation='publish'):
        ordinal = pending['next_batch_ordinal']
        self.sends += 1
        plan = provenance.PartPlan(self.root / 'stages' / ('send-%d' % self.sends))
        self.addCleanup(plan.close)
        manifest = transport.native_batch(self.inventory, self.sender, self.binding, plan, only=offer)
        members = plan.db.execute("SELECT path,part,bytes,digest FROM members WHERE path LIKE 'content/%' "
                                  "ORDER BY path").fetchall()
        refs = [{'kind': name.split('/')[1], 'sha256': sha, 'bytes': size} for name, _part, size, sha in members]
        after_sha, after_count = transport.coverage_after(self.sender, refs)
        origin = (EXECUTION if operation == 'execute' else dict(EXECUTION, operation='publish', run=21 + ordinal,
                  job=121 + ordinal, task=221 + ordinal, dispatch_ordinal=ordinal))
        batch = {**self.binding, 'schema_version': 1, 'kind': 'transfer_batch_receipt',
                 'intent_sha256': pending['intent']['sha256'], 'execution_origin': EXECUTION,
                 'publication_origin': origin, 'batch_ordinal': ordinal, 'purpose': purpose,
                 'previous_batch': pending['previous_batch'], 'transfer_manifest': manifest,
                 'coverage_before_sha256': pending['coverage_sha256'], 'coverage_after_sha256': after_sha,
                 'coverage_count': after_count, 'artifact_receipts': [], 'retained_artifact_bytes': 0,
                 'acceptance': False, 'proof_checkpoint_advanced': False}
        receipts_, identities = {}, {}
        for part in range(plan.preflight()):
            identity = 1000 * self.sends + part + 1
            identities[part] = identity
            receipts_[part] = {'schema_version': 6, 'kind': 'ArtifactReceipt', 'record': {
                'artifact_id': identity, 'run': origin['run'], 'job': origin['job'], 'task': origin['task'],
                'attempt': 1, 'unit_ordinal': 0, 'zip_bytes': 1 + sum(m['bytes'] for m in plan.manifest(part)),
                'members': plan.manifest(part)}}
            shutil.copytree(plan.directory(part), self.root / 'parts' / str(identity))
        ordered = [receipts_[part] for part in sorted(receipts_)]
        final = {**batch, 'artifact_receipts': ordered,
                 'retained_artifact_bytes': sum(r['record']['zip_bytes'] for r in ordered)}
        if self.original is None:
            self.original = ordered
            self.unit_envelope = {'schema_version': 6, 'kind': 'WorkUnit', 'record': {
                **self.unit_record, 'artifact_roots': [provenance.ref(r, r['record']['artifact_id']) for r in ordered]}}
        pending_after = {**pending, 'previous_batch': transport.content_ref('transfer_batch_receipt',
                                                                            transport.canonical(final)),
                         'coverage_sha256': final['coverage_after_sha256'], 'coverage_count': final['coverage_count'],
                         'next_batch_ordinal': ordinal + 1}
        rows = []
        for name, part, size, sha in members:
            reference = {'kind': name.split('/')[1], 'sha256': sha, 'bytes': size}
            rows.append((reference, self.root / 'parts' / str(identities[part]) / name, {
                'batch_receipt_sha256': provenance.digest(provenance.canonical(final)),
                'publication_origin': origin, 'artifact_id': identities[part], 'part_ordinal': part,
                'member_path': name, 'bytes': size, 'sha256': sha}))
        carrier = {'schema_version': 2, 'role': 'unit_checkpoint' if purpose == 'activate' else 'publication_batch',
                   'work_unit': self.unit_envelope, 'transfer_batch': final, 'origin': origin, 'unit_ordinal': 0,
                   'pending_publication': None if purpose == 'activate' else pending_after,
                   'transport_conformance': None, 'artifact_receipts': ordered, 'previous_control_sha256': None}
        auth = {'carrier': carrier, 'rows': rows, 'control_sha256': provenance.digest(provenance.canonical(carrier)),
                'resolver': transport.AuthenticatedResolver(self.index.db, self.root / 'parts'),
                'unit': self.unit_envelope['record'], 'receipts': receipts_, 'intent': self.intent}
        return Sent(auth, final, pending_after, plan)

    def begin(self, sent):
        return session.begin_pending({'accepted_batches': []}, {}, ROOT, self.root / 'history', sent.auth,
                                     self.index, None)

    @staticmethod
    def fork(sent):
        """Independent mutable copy of a sent batch's carrier and rows (connections stay shared)."""
        return {**sent.auth, 'carrier': copy.deepcopy(sent.auth['carrier']), 'rows': list(sent.auth['rows'])}

    def follow(self, pend, item, activation=False):
        auth = item.auth if isinstance(item, Sent) else item
        after = session.replay_batch({}, auth, self.index, pend, activation=activation)
        return {**pend, 'pending': after}

    def coverage_of(self, pend):
        replay = session.ReplayLedger(pend['ledger_path'])
        try:
            return replay.ledger.coverage()
        finally:
            replay.close()

    def first_offer(self):
        """The intent with every structured parent: a chunk is creditable only once its parent is covered."""
        return {(kind, sha) for kind, sha in self.sender.db.execute(
            "SELECT kind,sha256 FROM queue WHERE kind!='snapshot_chunk'")}

    def started(self):
        first = self.send(self.pending, self.first_offer(), operation='execute')
        pend = self.begin(first)
        self.sync(pend)
        return first, pend

    def sync(self, pend):
        replay = session.ReplayLedger(pend['ledger_path'])
        try:
            rows = replay.db.execute('SELECT kind,sha256,bytes FROM coverage').fetchall()
        finally:
            replay.close()
        self.sender.db.execute('DELETE FROM coverage')
        for row in rows:
            self.sender.db.execute('INSERT INTO coverage VALUES(?,?,?)', row)
        self.sender.db.commit()

    def deliver(self):
        """Intent and parents, then one real chunk, then the remaining closure: three nonempty batches."""
        first = self.send(self.pending, self.first_offer(), operation='execute')
        pend = self.begin(first)
        self.sync(pend)
        second = self.send(pend['pending'], {('snapshot_chunk', self.chunks[0])})
        pend = self.follow(pend, second)
        self.sync(pend)
        third = self.send(pend['pending'], None)
        pend = self.follow(pend, third)
        self.sync(pend)
        return pend, (first, second, third)

    def complete(self, pend):
        replay = session.ReplayLedger(pend['ledger_path'])
        try:
            return transport.closure_complete(replay.ledger)
        finally:
            replay.close()

    def activate(self, pend):
        """The separate activation batch carrying one genuine validation receipt."""
        before = pend['pending']
        # A genuine closed ValidationReceipt record (N2b), exactly the bytes prepare_activation transports.
        from tests.operations.test_async_refinement_validation_receipts import catalog_receipts
        _envelope, _root, private, _semantic = catalog_receipts()
        receipt_ref = self.inventory.put(
            'validation_receipt', transport.canonical(session.validation_receipt('catalog_private', private, ROOT)))
        self.sender.db.execute('INSERT OR IGNORE INTO queue VALUES(?,?,?)',
                               (receipt_ref['kind'], receipt_ref['sha256'], receipt_ref['bytes']))
        self.sender.db.commit()
        sent = self.send(before, {('validation_receipt', receipt_ref['sha256'])}, purpose='activate')
        return before, sent, self.follow(pend, sent, activation=True)

    def activation_record(self, before, **changes):
        intent = self.intent
        record = {**self.binding, 'schema_version': 1, 'kind': 'publication_activation',
                  'intent_sha256': before['intent']['sha256'], 'execution_origin': EXECUTION,
                  'activation_origin': dict(EXECUTION, operation='publish', run=99, job=199, task=299,
                                            dispatch_ordinal=9),
                  'final_batch': before['previous_batch'], 'previous_checkpoint': intent['previous_checkpoint'],
                  'candidate_checkpoint': intent['candidate_checkpoint'], 'catalog_root': intent['catalog_root'],
                  'verifier_root': intent['verifier_root'], 'execution_result': intent['execution_result'],
                  'validations': {}, 'stage': 'expand', 'stage_complete': False, 'activated': True,
                  'acceptance': False}
        record.update(changes)
        return record

    def finish(self, pend, before, validate, **changes):
        replay = session.ReplayLedger(pend['ledger_path'])
        try:
            return transport.finish_publication(replay.ledger, self.intent, before,
                                                self.activation_record(before, **changes), self.planned,
                                                self.original, provenance.ref, validate)
        finally:
            replay.close()


ERRORS = (transport.SnapshotTransportError, receipts.ReceiptError)


class PublicationChainTests(PublicationChainCase):
    def test_three_forced_batches_then_activation_accepts_exactly_once(self):
        pend, sent = self.deliver()
        self.assertTrue(self.complete(pend))
        self.assertEqual(len(sent), 3)
        for item in sent:
            self.assertTrue(item.auth['rows'])  # every transfer batch carries genuine new payload
        self.assertEqual([row[0]['sha256'] for row in sent[1].auth['rows']], [self.chunks[0]])
        before, _activate, pend = self.activate(pend)
        calls = []
        result = self.finish(pend, before, lambda accepted, validations: calls.append(accepted))
        self.assertEqual(len(calls), 1)
        self.assertEqual(result['work_unit']['artifact_roots'],
                         [provenance.ref(r, r['record']['artifact_id']) for r in self.original])
        self.assertEqual(self.planned['planned_work_unit']['artifact_roots'], [])
        with self.assertRaises(transport.SnapshotTransportError):
            self.finish(pend, before, lambda accepted, validations: None)  # a second accept is refused
        self.assertEqual(len(calls), 1)

    def test_replayed_stale_or_skipping_batches_never_credit(self):
        _first, pend = self.started()
        second = self.send(pend['pending'], {('snapshot_chunk', self.chunks[0])})
        advanced = self.follow(pend, second)
        with self.assertRaises(ERRORS):
            self.follow(pend, second)  # replaying a consumed batch against the stale pending
        self.assertEqual(self.coverage_of(advanced), (advanced['pending']['coverage_sha256'],
                                                      advanced['pending']['coverage_count']))
        self.sync(advanced)
        skipped = self.send({**advanced['pending'], 'next_batch_ordinal': 5}, {('snapshot_chunk', self.chunks[1])})
        with self.assertRaises(ERRORS):
            self.follow(advanced, skipped)
        self.assertEqual(self.coverage_of(advanced), (advanced['pending']['coverage_sha256'],
                                                      advanced['pending']['coverage_count']))

    def test_tampered_advertised_coverage_and_zero_progress_are_refused(self):
        _first, pend = self.started()
        second = self.send(pend['pending'], {('snapshot_chunk', self.chunks[0])})
        forged = self.fork(second)
        forged['carrier']['transfer_batch']['coverage_after_sha256'] = '0' * 64
        with self.assertRaises(transport.SnapshotTransportError):
            self.follow(pend, forged)
        idle = self.fork(second)
        batch = idle['carrier']['transfer_batch']
        batch['coverage_after_sha256'] = pend['pending']['coverage_sha256']
        batch['coverage_count'] = pend['pending']['coverage_count']
        idle['rows'] = []
        with self.assertRaises(transport.SnapshotTransportError):
            self.follow(pend, idle)
        self.assertEqual(self.coverage_of(pend), (pend['pending']['coverage_sha256'], pend['pending']['coverage_count']))

    def test_publisher_relabelling_original_unit_or_origin_is_refused(self):
        _first, pend = self.started()
        second = self.send(pend['pending'], {('snapshot_chunk', self.chunks[0])})
        relabelled = self.fork(second)
        relabelled['carrier']['work_unit']['record']['run'] = 21
        with self.assertRaises(receipts.ReceiptError):
            self.follow(pend, relabelled)
        confused = self.fork(second)
        confused['carrier']['transfer_batch']['publication_origin'] = EXECUTION
        with self.assertRaises(receipts.ReceiptError):
            self.follow(pend, confused)
        activation_as_transfer = self.fork(second)
        activation_as_transfer['carrier']['transfer_batch']['purpose'] = 'activate'
        with self.assertRaises(receipts.ReceiptError):
            self.follow(pend, activation_as_transfer)

    def test_first_batch_must_be_the_original_execution_not_a_publisher(self):
        first = self.send(self.pending, {('publication_intent', self.intent_ref['sha256'])}, operation='publish')
        with self.assertRaises(receipts.ReceiptError):
            self.begin(first)

    def test_crash_after_upload_acknowledgment_but_before_credit_grants_nothing_until_replay(self):
        _first, pend = self.started()
        second = self.send(pend['pending'], {('snapshot_chunk', self.chunks[0])})
        reference = second.auth['rows'][0][0]
        # The batch is uploaded and acknowledged; the controller dies before replay credits it.
        self.assertEqual(self.coverage_of(pend), (pend['pending']['coverage_sha256'], pend['pending']['coverage_count']))
        with self.assertRaises(transport.SnapshotTransportError):
            second.auth['resolver'](reference)  # uploaded bytes without an authenticated locator are not content
        # The restarted controller first sees a forged acknowledgment of the same upload and refuses it ...
        forged = self.fork(second)
        forged['carrier']['transfer_batch']['coverage_after_sha256'] = '1' * 64
        with self.assertRaises(transport.SnapshotTransportError):
            self.follow(pend, forged)
        self.assertEqual(self.coverage_of(pend), (pend['pending']['coverage_sha256'], pend['pending']['coverage_count']))
        # ... then re-authenticates the genuine upload and credits it exactly once.
        advanced = self.follow(pend, second)
        self.assertEqual(second.auth['resolver'](reference), self.inventory.bytes(reference))
        self.assertGreater(advanced['pending']['coverage_count'], pend['pending']['coverage_count'])
        with self.assertRaises(ERRORS):
            self.follow(advanced, second)

    def test_crash_before_upload_acknowledgment_leaves_the_receiver_at_the_previous_batch(self):
        _first, pend = self.started()
        # The sender planned and packed batch 1 but nothing reached the receiver.
        lost = self.send(pend['pending'], {('snapshot_chunk', self.chunks[0])})
        self.assertEqual(lost.pending_after['next_batch_ordinal'], 2)
        restarted = session.ReplayLedger(pend['ledger_path'])
        try:
            self.assertEqual(restarted.ledger.coverage(),
                             (pend['pending']['coverage_sha256'], pend['pending']['coverage_count']))
        finally:
            restarted.close()
        # Resuming re-plans the same ordinal from the receiver's pending; the replacement batch is accepted
        # and the earlier unacknowledged pack confers nothing.
        replacement = self.send(pend['pending'], {('snapshot_chunk', self.chunks[1])})
        advanced = self.follow(pend, replacement)
        self.assertEqual(advanced['pending']['next_batch_ordinal'], 2)
        with self.assertRaises(ERRORS):
            self.follow(advanced, lost)  # the abandoned pack names a different coverage and is stale

    def test_lost_spool_fails_the_batch_and_keeps_only_authenticated_coverage(self):
        _first, pend = self.started()
        os.unlink(self.root / 'cas' / self.chunks[0])
        with self.assertRaises((transport.SnapshotTransportError, OSError)):
            self.send(pend['pending'], {('snapshot_chunk', self.chunks[0])})
        self.assertFalse(self.complete(pend))
        self.assertEqual(self.coverage_of(pend), (pend['pending']['coverage_sha256'], pend['pending']['coverage_count']))
        replay = session.ReplayLedger(pend['ledger_path'])
        try:
            with self.assertRaises(transport.SnapshotTransportError):
                transport.finish_publication(replay.ledger, self.intent, pend['pending'],
                                             self.activation_record(pend['pending']), self.planned, self.original,
                                             provenance.ref, lambda accepted, validations: None)
        finally:
            replay.close()

    def test_no_accept_before_closure_is_complete(self):
        _first, pend = self.started()
        self.assertFalse(self.complete(pend))
        with self.assertRaises(transport.SnapshotTransportError):
            self.finish(pend, pend['pending'], lambda accepted, validations: self.fail('validated early'))

    def test_a_failed_restore_validation_leaves_no_activation_and_can_be_retried(self):
        pend, _sent = self.deliver()
        before, _activate, pend = self.activate(pend)

        def refuse(accepted, validations):
            raise transport.SnapshotTransportError('restored validation failed')
        with self.assertRaises(transport.SnapshotTransportError):
            self.finish(pend, before, refuse)
        replay = session.ReplayLedger(pend['ledger_path'])
        try:
            self.assertIsNone(replay.db.execute("SELECT 1 FROM state WHERE key LIKE 'activated:%'").fetchone())
        finally:
            replay.close()
        self.assertEqual(self.finish(pend, before, lambda accepted, validations: None)['schema_version'], 6)

    def test_activation_must_name_the_exact_intent_roots_and_final_batch(self):
        pend, _sent = self.deliver()
        before, _activate, pend = self.activate(pend)
        for change in ({'intent_sha256': '0' * 64},
                       {'catalog_root': {**self.intent['catalog_root'], 'sha256': '1' * 64}},
                       {'final_batch': None}, {'execution_origin': dict(EXECUTION, run=99)},
                       {'acceptance': True}):
            with self.subTest(change=sorted(change)):
                with self.assertRaises(transport.SnapshotTransportError):
                    self.finish(pend, before, lambda accepted, validations: self.fail('validated a bad activation'),
                                **change)

    def test_altered_full_d_evidence_bytes_are_refused_on_reconstruction(self):
        pend, _sent = self.deliver()
        resolver = transport.AuthenticatedResolver(self.index.db, self.root / 'parts')
        destination = self.root / 'history' / 'evidence-good'
        session.reconstruct_evidence(self.intent, resolver, self.binding, destination, None, ROOT,
                                     session.NullBudget())
        self.assertEqual((destination / 'unit.json').read_bytes(), b'{"evidence":"original full-D member"}')
        manifest = self.inventory.db.execute(
            "SELECT content_sha256 FROM members WHERE member_path='unit.json'").fetchone()[0]
        victims = list((self.root / 'parts').rglob(manifest))
        self.assertTrue(victims)
        for victim in victims:
            data = victim.read_bytes()
            victim.write_bytes(bytes([data[0] ^ 1]) + data[1:])
        with self.assertRaises(transport.SnapshotTransportError):
            session.reconstruct_evidence(self.intent, resolver, self.binding, self.root / 'history' / 'evidence-bad',
                                         None, ROOT, session.NullBudget())

    def test_an_omitted_evidence_member_cannot_reconstruct(self):
        pend, _sent = self.deliver()
        chunk_root = self.inventory.db.execute(
            "SELECT content_sha256 FROM members WHERE member_path='unit.json'").fetchone()[0]
        for victim in list((self.root / 'parts').rglob(chunk_root)):
            victim.unlink()
        resolver = transport.AuthenticatedResolver(self.index.db, self.root / 'parts')
        with self.assertRaises((transport.SnapshotTransportError, OSError)):
            session.reconstruct_evidence(self.intent, resolver, self.binding, self.root / 'history' / 'evidence-omitted',
                                         None, ROOT, session.NullBudget())

    def test_evidence_changed_after_reservation_cannot_be_sealed(self):
        replacement = transport.EvidencePlan(
            transport.OwnedDirectory(self.root / 'evidence', self.root, lambda: None),
            sqlite3.connect(':memory:'), self.charge, self.discharge)
        self.addCleanup(replacement.close)
        source = self.root / 'src' / 'other.json'
        source.write_bytes(b'{"evidence":"first"}')
        source.chmod(0o600)
        replacement.add_file('other.json', source)
        (self.root / 'evidence' / 'other.json').write_bytes(b'{"evidence":"alter"}')
        with self.assertRaises(transport.SnapshotTransportError):
            transport.evidence_root(self.inventory, self.binding, replacement)


# ---------------------------------------------------------------------------
# validate_control_identity and authenticate_publication: schema validation is
# stubbed so only the identity and routing rules under test can fail.
# ---------------------------------------------------------------------------
REGISTRATION = {'repository': 1, 'pr': 2, 'event_base': 'a' * 40, 'source': SOURCE_ID,
                'source_archive_sha256': 's' * 64}
JOB = {'id': 12, 'task_id': 13}


def stub_schema(test):
    for name, replacement in (('validate', lambda *args: None),
                              ('envelope', lambda value, kind, root: value['record'])):
        patcher = mock.patch.object(provenance, name, replacement)
        patcher.start()
        test.addCleanup(patcher.stop)


def carrier_for(role='publication_batch', ordinal=0, operation='execute', purpose='transfer'):
    origin = dict(EXECUTION) if operation == 'execute' else dict(EXECUTION, operation='publish', run=21, job=22, task=23,
                                                                dispatch_ordinal=ordinal)
    batch = {'batch_ordinal': ordinal, 'purpose': purpose, 'execution_origin': EXECUTION, 'publication_origin': origin}
    unit = {'session_id': SESSION_ID, 'unit_ordinal': 0, 'source': SOURCE_ID, 'expected_prior_checkpoint': None,
            'run': 11, 'job': 12, 'task': 13, 'attempt': 1, 'status': 'checkpointed_incomplete'}
    return {'role': role, 'origin': origin, 'registration_sha256': provenance.digest(provenance.canonical(REGISTRATION)),
            'source_archive_sha256': 's' * 64, 'source': SOURCE_ID, 'session_id': SESSION_ID, 'unit_ordinal': 0,
            'expected_prior_checkpoint': None, 'work_unit': {'record': unit}, 'artifact_receipts': [{}],
            'completion': None, 'transfer_batch': batch,
            'activation': {'execution_origin': EXECUTION} if role == 'unit_checkpoint' else None,
            'postcheck': {'checked_at': '2026-01-01T00:00:00Z', 'source_commit': 'a' * 40, 'current_base': 'a' * 40,
                          'merge_base': 'a' * 40}}


def check_identity(value, run_id=None, operation=None):
    origin = value['origin']
    metadata = {'session_id': SESSION_ID, 'unit_ordinal': 0, 'parts': [{}],
                'operation': operation or origin['operation']}
    job = {'id': origin['job'], 'task_id': origin['task']}
    session.validate_control_identity(value, REGISTRATION, metadata, run_id or origin['run'], job, ROOT)


class ControlIdentityTests(unittest.TestCase):
    def setUp(self):
        stub_schema(self)

    def test_original_execution_batch_zero_and_later_publisher_batches_are_accepted(self):
        check_identity(carrier_for())
        check_identity(carrier_for(ordinal=1, operation='publish'))
        activation = carrier_for('unit_checkpoint', ordinal=2, operation='publish', purpose='activate')
        check_identity(activation)

    def test_a_publisher_cannot_claim_the_execute_operation_or_the_original_task(self):
        later = carrier_for(ordinal=1, operation='publish')
        later['origin']['operation'] = 'execute'
        later['transfer_batch']['publication_origin'] = later['origin']
        with self.assertRaises(receipts.ReceiptError):
            check_identity(later, operation='execute')
        bad_unit = carrier_for(ordinal=1, operation='publish')
        bad_unit['work_unit']['record']['run'] = 21  # relabelled to the publisher
        with self.assertRaises(receipts.ReceiptError):
            check_identity(bad_unit)

    def test_producer_identity_must_be_the_actual_current_run_and_registered_source(self):
        with self.assertRaises(receipts.ReceiptError):
            check_identity(carrier_for(), run_id=999)
        wrong = carrier_for()
        wrong['registration_sha256'] = '0' * 64
        with self.assertRaises(receipts.ReceiptError):
            check_identity(wrong)
        stale = carrier_for()
        stale['postcheck']['merge_base'] = 'b' * 40  # Main was merged into the head: the merge base moved
        with self.assertRaises(receipts.ReceiptError):
            check_identity(stale)

    def test_a_moved_main_tip_with_an_unchanged_merge_base_is_accepted(self):
        moved = carrier_for()
        moved['postcheck']['current_base'] = 'b' * 40  # audit only: the live Main tip advanced
        check_identity(moved)
        check_identity(dict(carrier_for(ordinal=1, operation='publish'),
                            postcheck=dict(moved['postcheck'])))

    def test_role_specific_shapes_cannot_be_mixed(self):
        activation = carrier_for('unit_checkpoint', ordinal=2, operation='publish', purpose='transfer')
        with self.assertRaises(receipts.ReceiptError):
            check_identity(activation)
        transfer = carrier_for(ordinal=1, operation='publish')
        transfer['activation'] = {'execution_origin': EXECUTION}
        with self.assertRaises(receipts.ReceiptError):
            check_identity(transfer)
        complete_unit = carrier_for(ordinal=1, operation='publish')
        complete_unit['work_unit']['record']['status'] = 'complete'
        with self.assertRaises(receipts.ReceiptError):
            check_identity(complete_unit)

    def test_completion_carrier_is_its_own_complete_unit_and_has_no_transfer(self):
        value = carrier_for('completion')
        value['origin'] = dict(EXECUTION, mode='complete')
        value['transfer_batch'] = None
        value['completion'] = {'record': {'accepted': True}}
        value['work_unit']['record'].update(status='complete')
        check_identity(value)
        value['transfer_batch'] = carrier_for()['transfer_batch']
        with self.assertRaises(receipts.ReceiptError):
            check_identity(value)


class AuthenticatePublicationTests(unittest.TestCase):
    def setUp(self):
        stub_schema(self)
        scratch = ROOT / '.agent-proof-fixtures' / 'authenticate-publication'
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.parts = self.root / 'authenticated-parts' / '5'
        self.parts.mkdir(parents=True, mode=0o700)
        self.index = provenance.DependencyIndex(self.root / 'index.sqlite')
        self.addCleanup(self.index.close)
        self.intent = transport.canonical({'candidate_checkpoint': {'sha256': '7' * 64}})
        self.intent_ref = transport.content_ref('publication_intent', self.intent)
        member = self.parts / 'content' / 'publication_intent' / self.intent_ref['sha256']
        member.parent.mkdir(parents=True)
        member.write_bytes(self.intent)
        self.carrier = carrier_for(ordinal=1, operation='publish')
        self.carrier['pending_publication'] = {'intent': self.intent_ref}
        self.got = {'run': {'id': 21}, 'job': {'id': 22, 'task_id': 23}, 'event': {'inputs': {'operation': 'publish'}},
                    'lines': [], 'metadata': {'operation': 'publish'}, 'parts': {0: self.parts},
                    'receipts': {0: {'artifact': {'id': 5, 'name': 'part', 'size_in_bytes': 9}, 'zip_sha256': 'f' * 64,
                                     'zip_bytes': 9, 'members': [{'path': 'content/x', 'bytes': 1, 'sha256': 'e' * 64}]}}}
        self.state = {'next_dispatch_ordinal': 1, 'pending_publication': {'pending': {'intent': self.intent_ref}}}
        self.api = mock.Mock(scratch=self.root / 'authenticated-parts' / 'scratch')
        self.transfer = mock.Mock(return_value=['rows'])
        self.install()

    def expected_receipts(self):
        return session.native_receipts(REGISTRATION, self.got, self.carrier['work_unit']['record'], '7' * 64)

    def install(self):
        self.parse = mock.Mock()
        patches = (mock.patch.object(session, 'download_parts', return_value=self.got),
                   mock.patch.object(provenance, 'parse_control_frame', self.parse),
                   mock.patch.object(session, 'validate_control_identity'),
                   mock.patch.object(provenance, 'validate_upload_order'),
                   mock.patch.object(provenance, 'upload_slice', return_value=('d' * 64, 'f' * 64, 0, 0)),
                   mock.patch.object(provenance, 'authenticate_transfer_context', self.transfer))
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)

    def run_authentication(self, case=None, carrier=None):
        carrier = self.carrier if carrier is None else carrier
        expected = self.expected_receipts()
        carrier['artifact_receipts'] = [expected[n] for n in sorted(expected)]
        self.parse.return_value = (carrier, 'c' * 64, 0, 1)
        return session.authenticate_publication(self.api, REGISTRATION, 21, ROOT, self.root, self.state, self.index,
                                                case_sha256=case)

    def test_receipts_are_rebuilt_from_the_actual_native_artifacts_with_the_candidate_checkpoint(self):
        result = self.run_authentication()
        record = result['receipts'][0]['record']
        self.assertEqual((record['run'], record['job'], record['task']), (21, 22, 23))  # the publisher, not the original
        self.assertEqual(record['produced_checkpoint'], '7' * 64)
        arguments = self.transfer.call_args
        self.assertEqual(arguments.args[7], 1)  # the reserved dispatch ordinal of this publisher
        self.assertIsNone(arguments.kwargs['case_sha256'])

    def test_carrier_receipts_that_differ_from_the_native_reconstruction_are_refused(self):
        self.run_authentication()
        forged = copy.deepcopy(self.carrier)
        forged['artifact_receipts'][0]['record']['zip_sha256'] = '0' * 64
        self.parse.return_value = (forged, 'c' * 64, 0, 1)
        with self.assertRaises(receipts.ReceiptError):
            session.authenticate_publication(self.api, REGISTRATION, 21, ROOT, self.root, self.state, self.index)
        self.transfer.reset_mock()
        self.parse.return_value = (self.carrier, 'c' * 64, 0, 1)
        session.authenticate_publication(self.api, REGISTRATION, 21, ROOT, self.root, self.state, self.index)
        self.assertEqual(self.transfer.call_count, 1)  # only the exact reconstruction reaches lineage authentication

    def test_rehearsal_case_is_forwarded_and_an_event_operation_mismatch_is_refused(self):
        self.run_authentication(case='9' * 64)
        self.assertEqual(self.transfer.call_args.kwargs['case_sha256'], '9' * 64)
        self.got['event']['inputs']['operation'] = 'execute'
        with self.assertRaises(receipts.ReceiptError):
            session.authenticate_publication(self.api, REGISTRATION, 21, ROOT, self.root, self.state, self.index)

    def test_activation_resolves_the_frozen_intent_from_state_and_refuses_substituted_bytes(self):
        self.carrier['pending_publication'] = None
        result = self.run_authentication()
        self.assertEqual(result['intent']['candidate_checkpoint']['sha256'], '7' * 64)
        member = self.parts / 'content' / 'publication_intent' / self.intent_ref['sha256']
        member.write_bytes(self.intent.replace(b'7', b'8'))  # bytes differ from the frozen identity
        with self.assertRaises(receipts.ReceiptError):
            session.authenticate_publication(self.api, REGISTRATION, 21, ROOT, self.root, self.state, self.index)


class MainUmaskTests(unittest.TestCase):
    def test_every_controller_command_sets_a_private_umask_before_doing_work(self):
        order = []
        with mock.patch.object(session.os, 'umask', side_effect=lambda value: order.append(('umask', value))), \
                mock.patch.object(session, 'require_completion', side_effect=lambda *a: order.append(('work',))):
            session.main(['require-completion', '--output', str(ROOT / '.agent-proof-fixtures')])
        self.assertEqual(order, [('umask', 0o077), ('work',)])



# ---------------------------------------------------------------------------
# PhysicalContext.child_plan: against B's real ReservationLedger and grant issuer.
# ---------------------------------------------------------------------------
import ast
import sys
import uuid
import zipfile
from pathlib import PurePosixPath
from types import SimpleNamespace

from validation import async_physical_policy as physical

PAGE = 4096
MIB = 1024 * 1024
UNIT_BYTES = 96 * MIB  # the admitted unit_byte_allowance of the child-plan fixtures (larger than a control share)
PACKAGE = ROOT / 'verification' / 'async-driver-refinement'
RUNTIME_POOL = 512 * MIB  # source_runtime_scratch_bytes: the unit output share, a control share and scratch fit


def child_policy(source_map, **pools):
    families = {name: {'owner': owner, 'database_bytes': 10 * PAGE, 'main_bytes': 20 * PAGE,
                       'journal_bytes': 20 * PAGE + MIB, 'temp_bytes': 20 * PAGE}
                for name, owner in physical.FAMILY_OWNERS.items()}
    families['verifier_main'].update(database_bytes=20 * PAGE, main_bytes=40 * PAGE,
                                     journal_bytes=40 * PAGE + MIB, temp_bytes=40 * PAGE)
    pool_bytes = {name: 8 * MIB for name in physical.POOL_OWNERS}
    pool_bytes['source_runtime_scratch_bytes'] = RUNTIME_POOL
    pool_bytes.update(pools)
    return {'schema_version': 3, 'kind': 'physical_policy', 'table_map_sha256': physical.digest(source_map),
            'families': families, 'pools': pool_bytes}


class ChildPlanBase(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / '.agent-proof-fixtures' / 'child-plan'
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.out = self.root / 'out'
        self.out.mkdir(mode=0o700)
        self.source_map = json.loads(physical.SOURCE_MAP_PATH.read_bytes())
        lease = self.root / 'lease'
        descriptor = os.open(lease, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.write(descriptor, b'lease')
        os.close(descriptor)
        self.lease_fd = os.open(lease, os.O_RDONLY)
        self.addCleanup(os.close, self.lease_fd)
        self.context = self.make_context(child_policy(self.source_map))

    def make_context(self, policy):
        context = session.PhysicalContext.__new__(session.PhysicalContext)
        info = self.root.stat()
        context.reg = {'source': {'commit': '0' * 40}}
        context.session_id = str(uuid.UUID(int=1))
        context.owned = self.root
        context.plan = {'plan_sha256': '1' * 64, 'unit_byte_allowance': UNIT_BYTES}
        context.plan_sha256 = context.plan['plan_sha256']
        context.policy = policy
        context.policy_sha256 = physical.digest(policy)
        context.lease = SimpleNamespace(fd=self.lease_fd, relative_path='lease')
        context.ledger = physical.ReservationLedger(policy, self.root, source_map=self.source_map)
        context.root_identity = {'path': str(self.root), 'device': info.st_dev, 'inode': info.st_ino}
        context.attachments, context.grants = {}, []
        return context

    def expected(self):
        context = self.context
        return {'source': context.reg['source'], 'session_id': context.session_id,
                'resource_plan_sha256': context.plan['plan_sha256'], 'physical_policy_sha256': context.policy_sha256,
                'physical_policy': context.policy}

    def issue(self, request, output):
        handle = self.context.issue(request, output)
        self.addCleanup(handle.close)
        return handle

    def load(self, handle):
        return physical.load_child_grant(handle.descriptor_fd, handle.sha256, self.lease_fd, self.expected())

    def stage_file(self, generation, name, blobs=1, context=None):
        """One restored file in its own single-file restore_staging reservation."""
        context = self.context if context is None else context
        generation.mkdir(mode=0o700, exist_ok=True)
        staged = generation / name
        budget = session.PoolBudget(context, 'restore_staging_bytes')
        budget.track(staged)
        database = sqlite3.connect(staged)
        database.execute('PRAGMA page_size=%d' % PAGE)
        database.execute('CREATE TABLE t(v BLOB)')
        for _ in range(blobs):
            database.execute('INSERT INTO t VALUES(zeroblob(4000))')
        database.commit()
        database.close()
        staged.chmod(0o600)
        budget.reserve(staged.stat().st_size)
        return staged, budget

    def held_restore(self):
        staged, budget = self.stage_file(self.out / 'state.restore', 'catalog.staged.sqlite', blobs=2)
        validation = self.root / 'restore-validation'
        validation.mkdir(mode=0o700)
        self.context.attach_restore(str(validation), [(budget, staged)])
        return staged, budget, validation



class ChildPlanTests(ChildPlanBase):
    def test_no_pool_capacity_is_delegated_twice_and_the_real_issuer_accepts_the_plan(self):
        context, request = self.context, {'operation': 'unit'}
        allocations, attachments = context.child_plan(request, self.out)
        self.assertEqual(attachments, [])
        totals = {}
        for entry in allocations:
            key = entry['resource'], entry['component']
            totals[key] = totals.get(key, 0) + entry['bytes']
        for key, total in totals.items():
            self.assertLessEqual(total, context.ledger.remaining(*key), key)
        families = context.policy['families']
        spool = {e['relative_path']: e['bytes'] for e in allocations if e['resource'] == 'snapshot_spool_bytes'}
        self.assertEqual(spool, {'out-aux/catalog-backup.sqlite': families['catalog_main']['database_bytes'],
                                 'out-aux/verifier-backup.sqlite': families['verifier_main']['database_bytes']})
        share = physical.unit_output_bytes(UNIT_BYTES)
        runtime = [e for e in allocations if e['resource'] == 'source_runtime_scratch_bytes']
        self.assertEqual([(e['relative_path'], e['recursive'], e['bytes']) for e in runtime],
                         [('out', True, share), ('out-aux/child-scratch', True, RUNTIME_POOL - share)])
        keyed = [(e['resource'], e['component']) for e in allocations if e['resource'] in physical.FAMILY_OWNERS]
        self.assertEqual(len(keyed), len(set(keyed)))
        self.issue(request, self.out)  # B's issuer deducts every share; the old double listing exhausts here
        self.assertEqual(context.ledger.remaining('snapshot_spool_bytes', 'bytes'), 8 * MIB - sum(spool.values()))
        self.assertEqual(context.ledger.remaining('source_runtime_scratch_bytes', 'bytes'), 0)

    def test_a_pool_that_cannot_cover_every_named_share_refuses_before_any_deduction(self):
        families = self.context.policy['families']
        needed = families['catalog_main']['database_bytes'] + families['verifier_main']['database_bytes']
        context = self.make_context(child_policy(self.source_map, snapshot_spool_bytes=needed - 1))
        with self.assertRaises(physical.PhysicalIncomplete):
            context.child_plan({'operation': 'unit'}, self.out)
        self.assertEqual(set(context.ledger.used.values()), {0})

    def test_the_plan_builder_refuses_to_delegate_the_same_capacity_twice(self):
        plan = session.GrantPlan(self.context)
        whole = self.context.ledger.remaining('snapshot_spool_bytes', 'bytes')
        plan.pool('snapshot_spool_bytes', self.out / 'a.bin', whole)
        with self.assertRaises(physical.PhysicalIncomplete):
            plan.pool('snapshot_spool_bytes', self.out / 'b.bin', whole)
        plan.database('catalog_main', self.out / 'catalog.sqlite')
        with self.assertRaises(receipts.ReceiptError):
            plan.database('catalog_main', self.out / 'other.sqlite')

    def test_verifier_continuation_scratch_is_granted_in_full_and_loads_in_the_child(self):
        context, request = self.context, {'operation': 'unit'}
        allocations, _ = context.child_plan(request, self.out)
        continuation = {e['component']: e for e in allocations if e['resource'] == 'verifier_continuation_scratch'}
        self.assertEqual(set(continuation), {'main', 'journal', 'temp'})
        self.assertEqual({e['relative_path'] for e in continuation.values()}, {'out-aux/continuation-validation.sqlite'})
        database = min(context.policy['families']['verifier_continuation_scratch']['database_bytes'],
                       continuation['main']['bytes'])
        self.assertGreaterEqual(continuation['journal']['bytes'], 2 * database + MIB)  # what the child asserts
        self.assertGreaterEqual(continuation['temp']['bytes'], 2 * database)
        child = self.load(self.issue(request, self.out))
        for component in ('main', 'journal', 'temp'):
            self.assertIn(('verifier_continuation_scratch', component), child.limits)

    def test_restore_attaches_the_held_reservation_read_only_and_never_gets_a_second_main(self):
        staged, budget, validation = self.held_restore()
        request = {'operation': 'validate_restore'}
        allocations, attachments = self.context.child_plan(request, validation)
        self.assertEqual(attachments, [{
            'reservation_id': budget.reservation_id, 'owner': 'snapshot_restore', 'resource': 'restore_staging_bytes',
            'component': 'bytes', 'bytes': staged.stat().st_size,
            'relative_path': 'out/state.restore/catalog.staged.sqlite', 'recursive': False,
            'access': 'read_only'}])
        self.assertEqual({e['resource'] for e in allocations},
                         {'catalog_or_verifier_restore_scratch', 'verifier_continuation_scratch',
                          'source_runtime_scratch_bytes'})
        scratch = {e['component'] for e in allocations if e['resource'] == 'catalog_or_verifier_restore_scratch'}
        self.assertEqual(scratch, {'journal', 'temp'})  # ancillary writes only
        self.assertFalse([e for e in allocations if e['component'] == 'main' and e['resource'] != 'verifier_continuation_scratch'])
        handle = self.issue(request, validation)
        self.assertEqual(self.context.ledger.remaining('restore_staging_bytes', 'bytes'), 8 * MIB - staged.stat().st_size)
        child = self.load(handle)
        readonly = physical.validate_readonly_database(child, 'catalog_or_verifier_restore_scratch', staged,
                                                       budget.reservation_id)
        self.assertEqual(readonly.page_count, staged.stat().st_size // PAGE)
        readonly.reserve_ancillary('journal', PAGE)
        with self.assertRaises(physical.PhysicalPolicyError):
            readonly.reserve_ancillary('main', PAGE)

    def test_a_restore_validator_without_the_attachment_or_an_attachment_on_other_work_is_refused(self):
        _staged, _budget, validation = self.held_restore()
        with self.assertRaises(receipts.ReceiptError):
            self.context.child_plan({'operation': 'validate_restore'}, self.root / 'unattached')
        with self.assertRaises(receipts.ReceiptError):
            self.context.child_plan({'operation': 'unit'}, validation)

    def test_an_attachment_needs_the_tracked_held_reservation(self):
        budget = session.PoolBudget(self.context, 'restore_staging_bytes')
        budget.reserve(PAGE)
        with self.assertRaises(receipts.ReceiptError):
            self.context.attach_restore(str(self.root / 'x'), [(budget, self.out / 'state.restore' / 'never-tracked')])

    def writer_request(self):
        """A unit writer's request: the main databases sit beside, never inside, the output tree."""
        return {'operation': 'unit', 'catalog_path': str(self.root / 'catalog.sqlite'),
                'verifier_work_db': str(self.root / 'verifier.sqlite')}

    def test_scratch_is_discarded_after_reap_and_sealed_outputs_stay_attachable(self):
        context, request = self.context, self.writer_request()
        aux = self.root / 'out-aux'
        handle = self.issue(request, self.out)
        catalog, backup = self.root / 'catalog.sqlite', aux / 'verifier-backup.sqlite'
        aux.mkdir(mode=0o700, exist_ok=True)  # the child's environment creates the sibling auxiliary directory
        catalog.write_bytes(b'c' * 1000)
        backup.write_bytes(b'v' * 1000)
        (self.out / 'page.json').write_bytes(b'p' * 1000)
        (aux / 'continuation-validation.sqlite').write_bytes(b'x' * 500)
        (aux / 'continuation-validation.sqlite-journal').write_bytes(b'x' * 500)
        (aux / 'child-scratch' / 'home').mkdir(parents=True)
        (aux / 'child-scratch' / 'home' / 'cache').write_bytes(b'x' * 500)
        context.finish(handle)
        self.assertFalse((aux / 'child-scratch').exists())
        self.assertFalse((aux / 'continuation-validation.sqlite').exists())
        self.assertFalse((aux / 'continuation-validation.sqlite-journal').exists())
        self.assertTrue(catalog.exists() and backup.exists())
        self.assertEqual((self.out / 'page.json').read_bytes(), b'p' * 1000)  # the output share is not scratch
        # The scratch family is free again; the sealed outputs and the output directory's share remain charged.
        self.assertEqual(context.ledger.remaining('source_runtime_scratch_bytes', 'bytes'),
                         RUNTIME_POOL - physical.unit_output_bytes(UNIT_BYTES))
        self.assertEqual(context.ledger.remaining('verifier_continuation_scratch', 'main'),
                         context.policy['families']['verifier_continuation_scratch']['main_bytes'])
        self.assertEqual(context.ledger.remaining('catalog_main', 'main'), 0)
        validation = self.root / 'seal-validation'
        validation.mkdir(mode=0o700)
        context.attach_retained(str(validation), (catalog, backup))
        attached = context.attachments[str(validation)]
        self.assertEqual([(a['resource'], a['relative_path'], a['recursive'], a['access']) for a in attached],
                         [('catalog_main', 'catalog.sqlite', False, 'read_only'),
                          ('snapshot_spool_bytes', 'out-aux/verifier-backup.sqlite', False, 'read_only')])
        self.issue({'operation': 'validate_restore'}, validation)  # the issuer checks them against the held reservations
        with self.assertRaises(receipts.ReceiptError):
            context.attach_retained(str(validation), (self.out / 'unheld.sqlite',))

    def test_an_overage_keeps_the_child_scratch_for_reconciliation(self):
        context = self.context
        handle = self.issue({'operation': 'unit'}, self.out)
        scratch = self.root / 'out-aux' / 'child-scratch'
        scratch.mkdir(parents=True)
        with (scratch / 'huge').open('wb') as stream:
            stream.truncate(RUNTIME_POOL - physical.unit_output_bytes(UNIT_BYTES) + 1)
        with self.assertRaises(physical.PhysicalIncomplete):
            context.finish(handle)
        self.assertTrue((scratch / 'huge').exists())  # evidence is not deleted before the overage is raised

    def test_the_output_directory_is_one_charged_subtree_and_the_named_files_sit_beside_it(self):
        context = self.context
        allocations, _ = context.child_plan(self.writer_request(), self.out)
        share = physical.unit_output_bytes(UNIT_BYTES)
        output = [e for e in allocations if e['relative_path'] == 'out']
        self.assertEqual([(e['resource'], e['component'], e['recursive'], e['bytes']) for e in output],
                         [('source_runtime_scratch_bytes', 'bytes', True, share)])
        self.assertGreater(share, UNIT_BYTES)  # sized from unit_byte_allowance, plus the controller captures
        # Nothing is claimed inside the subtree, and every named file keeps its one claim beside it.
        for entry in allocations:
            if entry is not output[0]:
                self.assertNotIn(PurePosixPath('out'), PurePosixPath(entry['relative_path']).parents, entry)
        claims = [(e['resource'], e['component'], e['relative_path']) for e in allocations]
        self.assertEqual(len(claims), len(set(claims)))
        named = {e['relative_path'] for e in allocations
                 if e['resource'] in ('catalog_export_scratch', 'verifier_continuation_scratch', 'snapshot_spool_bytes')}
        self.assertEqual(named, {'out-aux/manifest-closure.sqlite', 'out-aux/continuation-validation.sqlite',
                                 'out-aux/catalog-backup.sqlite', 'out-aux/verifier-backup.sqlite'})

    def test_a_file_left_in_the_output_directory_beyond_its_share_is_an_overage(self):
        context = self.context
        handle = self.issue(self.writer_request(), self.out)
        (self.out / 'pages').mkdir()
        with (self.out / 'pages' / 'page.json').open('wb') as stream:  # sparse: only the size is charged
            stream.truncate(physical.unit_output_bytes(UNIT_BYTES) + 1)
        with self.assertRaises(physical.PhysicalIncomplete):
            context.finish(handle)
        self.assertTrue((self.out / 'pages' / 'page.json').exists())

    def test_output_files_within_the_share_stay_charged_and_a_second_invocation_reuses_the_share(self):
        context, request = self.context, self.writer_request()
        first = self.issue(request, self.out)
        (self.out / 'page.json').write_bytes(b'p' * 1000)
        context.finish(first)
        share = physical.unit_output_bytes(UNIT_BYTES)
        self.assertEqual(context.ledger.remaining('source_runtime_scratch_bytes', 'bytes'), RUNTIME_POOL - share)
        held = [r for r in context.ledger.reservations if not r.released and r.subtrees]
        self.assertEqual(len(held), 1)
        again = self.issue(request, self.out)  # the held share is delegated by id, never charged twice
        output = [e for e in again.descriptor['allocations'] if e['relative_path'] == 'out']
        self.assertEqual([e['reservation_id'] for e in output], [held[0].reservation_id])
        self.assertEqual(context.ledger.remaining('source_runtime_scratch_bytes', 'bytes'), 0)

    def test_a_control_operation_output_is_a_small_share_and_a_unit_share_is_never_shrunk(self):
        context = self.context
        _staged, _budget, validation = self.held_restore()
        allocations, _ = context.child_plan({'operation': 'validate_restore'}, validation)
        output = [e for e in allocations if e['relative_path'] == 'restore-validation']
        self.assertEqual([(e['recursive'], e['bytes']) for e in output], [(True, physical.CONTROL_OUTPUT_BYTES)])
        # The tree a writer filled later hosts the read-only seal validation: it keeps the larger unit share.
        request, tree = self.writer_request(), self.root / 'writer'
        tree.mkdir(mode=0o700)
        first = self.issue(request, tree)
        catalog, backup = self.root / 'catalog.sqlite', self.root / 'writer-aux' / 'verifier-backup.sqlite'
        backup.parent.mkdir(mode=0o700, exist_ok=True)  # the child's environment creates the sibling auxiliary directory
        catalog.write_bytes(b'c' * 1000)
        backup.write_bytes(b'v' * 1000)
        (tree / 'page.json').write_bytes(b'p' * 1000)
        context.finish(first)
        context.attach_retained(str(tree), (catalog, backup))
        share = physical.unit_output_bytes(UNIT_BYTES)
        self.assertGreater(share, physical.CONTROL_OUTPUT_BYTES)
        seal, _ = context.child_plan({'operation': 'validate_restore'}, tree)
        self.assertEqual([e['bytes'] for e in seal if e['relative_path'] == 'writer'], [share])
        self.issue({'operation': 'validate_restore'}, tree)  # the issuer refuses any shrunken delegation


class ActivationTransferTests(ChildPlanBase):
    """The activation rename moves each staged file's reservation to main exactly once."""
    def setUp(self):
        super().setUp()
        patcher = mock.patch.object(session, 'current_physical', return_value=self.context)
        self.current = patcher.start()
        self.addCleanup(patcher.stop)

    def restored(self):
        generation = self.out / 'state.restore'
        catalog, catalog_budget = self.stage_file(generation, 'catalog.staged.sqlite', blobs=2)
        verifier, verifier_budget = self.stage_file(generation, 'verifier.staged.sqlite', blobs=3)
        staged = [{'budget': catalog_budget, 'path': catalog, 'family': 'catalog_main', 'owner': 'catalog',
                   'name': 'catalog.sqlite'},
                  {'budget': verifier_budget, 'path': verifier, 'family': 'verifier_main', 'owner': 'verifier',
                   'name': 'verifier.sqlite'}]
        return {'generation': generation, 'staged': staged}

    def used(self):
        return dict(self.context.ledger.used)

    def owners(self, path):
        return [r for r in self.context.ledger.reservations if not r.released and path in r.paths]

    def test_each_file_is_charged_once_after_activation_and_staging_is_released(self):
        restored, request = self.restored(), {}
        sizes = {e['family']: e['path'].stat().st_size for e in restored['staged']}
        before = self.used()
        self.assertEqual(before[('restore_staging_bytes', 'bytes')], sum(sizes.values()))
        session.activate_generation(restored, self.out, request)
        used = self.used()
        self.assertEqual(used[('restore_staging_bytes', 'bytes')], 0)  # the double charge is gone
        self.assertEqual(used[('catalog_main', 'main')], sizes['catalog_main'])
        self.assertEqual(used[('verifier_main', 'main')], sizes['verifier_main'])
        self.assertEqual(used[('control_bytes', 'bytes')], 0)
        self.assertEqual(self.context.ledger.transfers, {})
        active = self.out / 'state'
        self.assertEqual(request, {'catalog_path': str(active / 'catalog.sqlite'),
                                   'verifier_work_db': str(active / 'verifier.sqlite')})
        for name in ('catalog.sqlite', 'verifier.sqlite'):
            self.assertEqual(len(self.owners(active / name)), 1)  # one reservation owns the one file
        self.assertFalse(restored['generation'].exists())

    def test_destination_capacity_failure_leaves_the_staged_generation_unactivated(self):
        # Policy where the verifier destination cannot hold the staged verifier file.
        policy = child_policy(self.source_map)
        policy['families']['verifier_main'].update(database_bytes=PAGE, main_bytes=PAGE,
                                                   journal_bytes=2 * PAGE + MIB, temp_bytes=2 * PAGE)
        self.context = self.make_context(policy)
        self.current.return_value = self.context
        restored = self.restored()
        before = self.used()
        with self.assertRaises(physical.PhysicalIncomplete):
            session.activate_generation(restored, self.out, {})
        self.assertEqual(self.used(), before)  # destination claims aborted, sources still charged
        self.assertEqual(self.context.ledger.transfers, {})
        self.assertFalse((self.out / 'state').exists())  # the prior root stays authoritative
        for entry in restored['staged']:
            self.assertTrue(entry['path'].exists())
            self.assertEqual(len(self.owners(entry['path'])), 1)

    def test_a_failed_rename_is_undone_before_any_ownership_commits(self):
        restored = self.restored()
        before = self.used()
        real, calls = os.replace, []

        def flaky(source, destination):
            calls.append(source)
            if len(calls) == 2:
                raise OSError('rename failed')
            return real(source, destination)
        with mock.patch.object(session.os, 'replace', flaky):
            with self.assertRaises(OSError):
                session.activate_generation(restored, self.out, {})
        self.assertEqual(self.used(), before)
        self.assertEqual(self.context.ledger.transfers, {})
        self.assertFalse((self.out / 'state').exists())
        for entry in restored['staged']:
            self.assertTrue(entry['path'].exists())

    def test_an_existing_active_generation_is_never_replaced(self):
        restored = self.restored()
        (self.out / 'state').mkdir()
        before = self.used()
        with self.assertRaises(receipts.ReceiptError):
            session.activate_generation(restored, self.out, {})
        self.assertEqual(self.used(), before)

    def test_the_writer_after_activation_gets_the_file_without_a_second_charge(self):
        restored, request = self.restored(), {'operation': 'unit'}
        sizes = {e['family']: e['path'].stat().st_size for e in restored['staged']}
        session.activate_generation(restored, self.out, request)
        handle = self.issue(request, self.out / 'quantum')  # the held main reservations are delegated by id, never released
        families = self.context.policy['families']
        for family in ('catalog_main', 'verifier_main'):
            self.assertEqual(self.used()[(family, 'main')], families[family]['main_bytes'])  # granted once, in full
        self.assertEqual(self.used()[('restore_staging_bytes', 'bytes')], 0)
        child = self.load(handle)
        self.assertEqual(child.limits[('catalog_main', 'main')], families['catalog_main']['main_bytes'])
        self.assertGreater(families['catalog_main']['main_bytes'], sizes['catalog_main'])

    def test_a_failed_writer_grant_puts_the_held_reservations_back(self):
        restored, request = self.restored(), {'operation': 'unit'}
        session.activate_generation(restored, self.out, request)
        before = self.used()
        families = self.context.policy['families']
        self.assertLess(1, families['catalog_main']['database_bytes'])
        self.context.ledger.limits[('snapshot_spool_bytes', 'bytes')] = 1
        with self.assertRaises(physical.PhysicalIncomplete):
            self.context.issue(request, self.out / 'quantum')
        self.assertEqual(self.used(), before)
        for name in ('catalog.sqlite', 'verifier.sqlite'):
            self.assertEqual(len(self.owners(self.out / 'state' / name)), 1)


    def test_the_writer_grant_delegates_the_held_reservation_without_any_release_or_unowned_interval(self):
        restored, request = self.restored(), {'operation': 'unit'}
        session.activate_generation(restored, self.out, request)
        active = self.out / 'state'
        held = {name: self.owners(active / name)[0] for name in ('catalog.sqlite', 'verifier.sqlite')}
        charged = {name: reservation.bytes for name, reservation in held.items()}

        def forbidden(*_args, **_kwargs):
            raise AssertionError('a held main reservation was released/unclaimed while being handed on')
        with mock.patch.object(physical.Reservation, 'release_after_removal', forbidden), \
                mock.patch.object(physical.ReservationLedger, '_unclaim', forbidden):
            handle = self.issue(request, self.out / 'quantum')
        ids = {a['reservation_id'] for a in handle.descriptor['allocations']}
        for name, reservation in held.items():
            self.assertEqual(self.owners(active / name), [reservation])  # the very same reservation, still the one owner
            self.assertIn(reservation.reservation_id, ids)
            self.assertTrue(reservation.delegated)
            self.assertGreaterEqual(reservation.bytes, charged[name])
        self.assertEqual(self.context.ledger.claims[str((active / 'catalog.sqlite').relative_to(self.root))][0],
                         held['catalog.sqlite'])

    def test_a_reaped_writer_returns_the_delegated_file_to_the_parent_still_charged(self):
        restored, request = self.restored(), {'operation': 'unit'}
        session.activate_generation(restored, self.out, request)
        active = self.out / 'state'
        held = self.owners(active / 'catalog.sqlite')[0]
        handle = self.issue(request, self.out / 'quantum')
        report = handle.reconcile_after_reap(0)
        self.assertIn(held.reservation_id, report['retained'])
        self.assertFalse(held.delegated or held.released)
        self.assertEqual(self.owners(active / 'catalog.sqlite'), [held])
        self.assertGreaterEqual(self.used()[('catalog_main', 'main')], held.bytes)
        # The next writer inherits the same reservation again; nothing was re-reserved in between.
        again = self.issue(request, self.out / 'quantum')
        self.assertIn(held.reservation_id, {a['reservation_id'] for a in again.descriptor['allocations']})


class PreparationCopyChargeTests(ChildPlanBase):
    """The controller-made copy of the restored catalog is a claimed, charged file, never an unowned fallback."""
    def setUp(self):
        super().setUp()
        patcher = mock.patch.object(session, 'current_physical', return_value=self.context)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.backup = self.root / 'restored.sqlite'
        self.backup.write_bytes(b'c' * 5000)
        self.quantum = self.root / 'quantum'
        self.quantum.mkdir(mode=0o700)

    def holders(self, target):
        return [r for r in self.context.ledger.reservations if not r.released and target in r.paths]

    def test_the_copy_holds_one_spool_claim_for_exactly_its_bytes(self):
        target = session.copy_restored_catalog(self.backup, self.quantum)
        self.assertEqual(target, self.root / 'quantum-aux' / 'catalog-backup.sqlite')
        self.assertEqual(target.read_bytes(), b'c' * 5000)
        self.assertEqual(self.context.ledger.used[('snapshot_spool_bytes', 'bytes')], 5000)
        holders = self.holders(target)
        self.assertEqual([(h.owner, h.key, h.bytes) for h in holders],
                         [('snapshot_transport', ('snapshot_spool_bytes', 'bytes'), 5000)])

    def test_a_pool_that_cannot_hold_the_copy_refuses_before_any_byte_is_written(self):
        self.context = self.make_context(child_policy(self.source_map, snapshot_spool_bytes=4999))
        with mock.patch.object(session, 'current_physical', return_value=self.context):
            with self.assertRaises(physical.PhysicalIncomplete):
                session.copy_restored_catalog(self.backup, self.quantum)
        self.assertFalse((self.root / 'quantum-aux' / 'catalog-backup.sqlite').exists())

    def test_a_later_invocation_on_the_tree_delegates_the_held_claim_instead_of_claiming_the_name_twice(self):
        target = session.copy_restored_catalog(self.backup, self.quantum)
        held = self.holders(target)[0]
        handle = self.issue({'operation': 'unit'}, self.quantum)
        entries = [e for e in handle.descriptor['allocations'] if e['relative_path'] == 'quantum-aux/catalog-backup.sqlite']
        self.assertEqual([e['reservation_id'] for e in entries], [held.reservation_id])
        self.assertEqual(self.holders(target), [held])

    def test_the_preparation_unit_has_no_uncharged_copy_left_in_it(self):
        """execute_and_package for an input-preparation unit, with only the quantum child, the verifier reservation
        and the publication builder stubbed: every file it copies lands on a claimed, charged name."""
        output = self.root / 'output'
        output.mkdir()
        image_bytes = b'{}'
        (output / 'initial-image.json').write_bytes(image_bytes)
        sha = hashlib.sha256(self.backup.read_bytes()).hexdigest()
        saved = {key: 'f' * 64 for key in ('checkpoint_sha256', 'resource_plan_sha256', 'initial_image_sha256',
                                           'catalog_logical_digest', 'semantic_manifest_root', 'catalog_sha256')}
        saved.update(source={}, session_id='s', catalog_commit=1, snapshot_boundary='post_checkpoint_pre_upload',
                     format='sqlite3', backup_method='sqlite_backup_api_v1', catalog_bytes=5000)
        saved['catalog_sha256'] = sha
        provenance.write(output / 'restored-catalog-root.json', saved)
        provenance.write(self.quantum / 'phase-result.json', {'work_unit': {'unit_ordinal': 1}, 'stage_complete': False})
        provenance.write(self.quantum / 'verifier-progress' / 'unit.json', {})
        checkpoint = {'kind': 'Checkpoint'}
        image = {'record': {'member_path': 'initial-image.json', 'image_bytes': len(image_bytes),
                            'image_sha256': hashlib.sha256(image_bytes).hexdigest()}}
        state = {'stage': 'semantic_verify', 'checkpoint': checkpoint, 'initial_image': image, 'session': {}}
        request = {'output_dir': str(self.quantum), 'catalog_path': str(self.backup),
                   'work_unit': {'unit_ordinal': 1, 'mode': 'unit', 'expected_prior_checkpoint': 'e' * 64},
                   'execution_origin': {'dispatch_ordinal': 0}, 'driver_graph': str(output / 'none.json'),
                   'dependency_index': str(output / 'index.sqlite'), 'reservation_path': str(output / 'reservation.json')}
        prepare = {'before_cursor': {'operation': 'load_input'}, 'staged_members': [], 'metadata_reservations': []}
        finalized = {'schema_version': 6, 'work_unit': {'unit_ordinal': 1}, 'checkpoint': checkpoint, 'artifacts': [],
                     'stage_complete': False}
        plan = mock.MagicMock()
        plan.db.execute.return_value.fetchone.return_value = None
        built = {'pending': {}, 'binding': {}, 'intent': {'candidate_checkpoint': {'sha256': 'b' * 64}}}
        copies = []
        real = shutil.copyfile

        def recording(source, destination, *args, **kwargs):
            copies.append(Path(destination))
            return real(source, destination, *args, **kwargs)
        with mock.patch.object(session, 'run_quantum', side_effect=[prepare, finalized]), \
                mock.patch.object(session, 'source_commands', return_value=[]), \
                mock.patch.object(session, 'PublicationStore', return_value=mock.MagicMock(evidence=plan)), \
                mock.patch.object(session, 'transport_reservations'), \
                mock.patch.object(session, 'member_stream', return_value=[]), \
                mock.patch.object(session, 'reserve_verifier', return_value={'metadata_reservations': []}), \
                mock.patch.object(provenance, 'verifier_auxiliary',
                                  return_value={'committed_quantum_count': 1, 'result_count': 1}), \
                mock.patch.object(session, 'advance'), \
                mock.patch.object(session, 'open_index'), \
                mock.patch.object(session, 'build_publication', return_value=built), \
                mock.patch.object(session, 'plan_transfer', return_value=({}, mock.MagicMock(), [])), \
                mock.patch.object(session.shutil, 'copyfile', recording):
            session.execute_and_package(state, {}, request, ROOT, ROOT, output, 10 ** 9)
        self.assertEqual(copies, [self.root / 'quantum-aux' / 'catalog-backup.sqlite'])
        for target in copies:
            self.assertEqual([(h.owner, h.key) for h in self.holders(target)],
                             [('snapshot_transport', ('snapshot_spool_bytes', 'bytes'))])


class CatalogRestoreRequestTests(ChildPlanBase):
    """N1: the catalog restore validator names the STAGED verifier and gets BOTH held reservations attached."""
    def staged_pair(self):
        generation = self.out / 'state.restore'
        catalog, catalog_budget = self.stage_file(generation, 'catalog.staged.sqlite', blobs=2)
        verifier, verifier_budget = self.stage_file(generation, 'verifier.staged.sqlite', blobs=3)
        return catalog, catalog_budget, verifier, verifier_budget

    def restore(self, catalog, verifier):
        base = {'operation': 'validate_restore', 'schema_version': 6}
        request = session.catalog_restore_request(base, catalog, verifier, {'kind': 'Checkpoint'},
                                                  self.root / 'dependencies.sqlite', self.out)
        Path(request['output_dir']).mkdir(mode=0o700)
        return request

    def budget(self, handle):
        sys.path.insert(0, str(PACKAGE))
        self.addCleanup(sys.path.remove, str(PACKAGE))
        import paged_codec as codec
        budget = codec.Budget.__new__(codec.Budget)
        budget.physical_policy = self.context.policy
        budget.grant = self.load(handle)
        return budget, codec

    def test_both_staged_files_are_attached_read_only_each_by_its_own_reservation(self):
        catalog, catalog_budget, verifier, verifier_budget = self.staged_pair()
        request = self.restore(catalog, verifier)
        self.assertEqual(request['verifier_work_db'], str(verifier))  # the staged file, not a name nothing created yet
        self.context.attach_restore(request['output_dir'], [(catalog_budget, catalog), (verifier_budget, verifier)])
        _allocations, attachments = self.context.child_plan(request, Path(request['output_dir']))
        self.assertEqual({a['relative_path']: a['reservation_id'] for a in attachments},
                         {'out/state.restore/catalog.staged.sqlite': catalog_budget.reservation_id,
                          'out/state.restore/verifier.staged.sqlite': verifier_budget.reservation_id})
        self.assertEqual({a['access'] for a in attachments}, {'read_only'})
        self.assertEqual({a['bytes'] for a in attachments}, {catalog.stat().st_size, verifier.stat().st_size})

    def test_the_checkers_ceiling_checks_resolve_the_catalog_and_the_verifier_the_request_names(self):
        catalog, catalog_budget, verifier, verifier_budget = self.staged_pair()
        request = self.restore(catalog, verifier)
        self.context.attach_restore(request['output_dir'], [(catalog_budget, catalog), (verifier_budget, verifier)])
        budget, codec = self.budget(self.issue(request, Path(request['output_dir'])))
        # exactly the two checks paged_checker.validate_restore makes before it opens either file
        budget.restored_database('catalog_main', Path(request['catalog_path']))
        budget.restored_database('verifier_main', Path(request['verifier_work_db']), attached=True)

    def test_a_verifier_name_the_restore_did_not_stage_has_no_attached_reservation(self):
        catalog, catalog_budget, verifier, verifier_budget = self.staged_pair()
        unstaged = self.out / 'state.restore' / 'verifier.sqlite'  # what the request used to name
        shutil.copyfile(verifier, unstaged)
        request = self.restore(catalog, unstaged)
        self.context.attach_restore(request['output_dir'], [(catalog_budget, catalog), (verifier_budget, verifier)])
        budget, codec = self.budget(self.issue(request, Path(request['output_dir'])))
        with self.assertRaisesRegex(codec.Incomplete, 'no single held restore reservation'):
            budget.restored_database('verifier_main', Path(request['verifier_work_db']), attached=True)

    def test_a_restore_without_a_staged_verifier_names_the_generation_file_and_attaches_only_the_catalog(self):
        generation = self.out / 'state.restore'
        catalog, catalog_budget = self.stage_file(generation, 'catalog.staged.sqlite', blobs=2)
        request = self.restore(catalog, None)
        self.assertEqual(request['verifier_work_db'], str(generation / 'verifier.sqlite'))
        self.assertFalse(Path(request['verifier_work_db']).exists())
        self.context.attach_restore(request['output_dir'], [(catalog_budget, catalog)])
        _allocations, attachments = self.context.child_plan(request, Path(request['output_dir']))
        self.assertEqual([a['relative_path'] for a in attachments], ['out/state.restore/catalog.staged.sqlite'])


class DownloadChargeTests(ChildPlanBase):
    """N4: a download is charged to restore_staging_bytes BEFORE its first byte is written, each artifact once."""
    POOL = 'restore_staging_bytes'

    def setUp(self):
        super().setUp()
        self.install(self.context)
        self.observed = []

    def install(self, context):
        for patcher in (mock.patch.object(session, '_PHYSICAL', [context]),
                        mock.patch.object(session, '_DOWNLOADS', {}),
                        mock.patch.object(session, '_ARCHIVES', [])):
            patcher.start()
            self.addCleanup(patcher.stop)

    def used(self):
        return self.context.ledger.used[(self.POOL, 'bytes')]

    def artifact(self, identity, size):
        """A real ZIP of one member, its API descriptor and the directory it extracts to."""
        data = bytes((identity + i) % 251 for i in range(size))
        archive = self.root / 'zips' / ('%d.zip' % identity)
        archive.parent.mkdir(exist_ok=True)
        with zipfile.ZipFile(archive, 'w') as z:
            z.writestr('member.bin', data)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        member = {'path': 'member.bin', 'bytes': size, 'sha256': hashlib.sha256(data).hexdigest()}
        artifact = {'id': identity, 'name': 'part-%d' % identity, 'size_in_bytes': archive.stat().st_size}
        directory = self.root / 'authenticated-parts' / str(identity)
        return artifact, {'members': [member]}, directory, archive, digest

    def fetch(self, artifact, descriptor, directory, archive, digest):
        def download(_api, requested):
            self.observed.append(self.used())  # what is already charged when the first byte is about to be written
            target = self.root / 'download' / ('%d.zip' % requested['id'])
            target.parent.mkdir(exist_ok=True)
            shutil.copyfile(archive, target)
            return target
        api = SimpleNamespace(guard_storage=lambda _size: None)
        with mock.patch.object(provenance, 'artifact_bytes', side_effect=download), \
                mock.patch.object(session, 'transport_hash', return_value=digest):
            return session.fetch_part(api, artifact, descriptor, directory, [])

    def test_the_zip_is_charged_before_artifact_bytes_writes_it(self):
        artifact, descriptor, directory, archive, digest = self.artifact(5, 60000)
        self.fetch(artifact, descriptor, directory, archive, digest)
        self.assertEqual(self.observed, [60000 + artifact['size_in_bytes']])  # members plus the ZIP, already held
        self.assertEqual((directory / 'member.bin').stat().st_size, 60000)
        self.assertFalse((self.root / 'download' / '5.zip').exists())  # the transient ZIP is removed

    def test_a_pool_that_cannot_hold_the_zip_refuses_before_a_byte_is_downloaded(self):
        artifact, descriptor, directory, archive, digest = self.artifact(5, 60000)
        pool = 60000 + artifact['size_in_bytes'] - 1
        self.context = self.make_context(child_policy(self.source_map, restore_staging_bytes=pool))
        self.install(self.context)
        with self.assertRaises(physical.PhysicalIncomplete):
            self.fetch(artifact, descriptor, directory, archive, digest)
        self.assertEqual(self.observed, [])  # artifact_bytes was never reached
        self.assertFalse(directory.exists())

    def test_the_members_are_charged_once_per_artifact_and_zips_are_never_cumulative(self):
        artifacts = [self.artifact(identity, size) for identity, size in ((5, 50000), (6, 30000), (7, 40000))]
        for artifact, descriptor, directory, archive, digest in artifacts:
            self.fetch(artifact, descriptor, directory, archive, digest)
        members = sum(row[1]['members'][0]['bytes'] for row in artifacts)
        biggest = max(row[0]['size_in_bytes'] for row in artifacts)
        self.assertEqual(self.used(), members + biggest)  # high water: all members + the largest ZIP, not their sum
        self.assertEqual(self.observed[-1], members + biggest)
        self.assertLess(self.used(), members + sum(row[0]['size_in_bytes'] for row in artifacts))

    def test_a_cached_artifact_directory_is_charged_as_it_is_and_its_zip_is_charged_again(self):
        artifact, descriptor, directory, archive, digest = self.artifact(5, 60000)
        directory.parent.mkdir(exist_ok=True)
        with zipfile.ZipFile(archive) as z:
            z.extractall(directory)  # left by an earlier job: nothing charged it in this process
        cached = session.directory_bytes(directory)
        pool = cached + artifact['size_in_bytes']
        self.context = self.make_context(child_policy(self.source_map, restore_staging_bytes=pool - 1))
        self.install(self.context)
        with self.assertRaises(physical.PhysicalIncomplete):  # the cached copy is re-downloaded: its ZIP needs room
            self.fetch(artifact, descriptor, directory, archive, digest)
        self.assertEqual(self.observed, [])
        self.context = self.make_context(child_policy(self.source_map, restore_staging_bytes=pool))
        self.install(self.context)
        self.fetch(artifact, descriptor, directory, archive, digest)
        self.assertEqual(self.observed, [pool])
        self.assertEqual(self.used(), pool)

    def test_a_cached_artifact_is_not_charged_a_second_time_in_the_same_job(self):
        artifact, descriptor, directory, archive, digest = self.artifact(5, 60000)
        self.fetch(artifact, descriptor, directory, archive, digest)
        first = self.used()
        self.fetch(artifact, descriptor, directory, archive, digest)  # a later stage re-verifies the cache
        self.assertEqual(self.used(), first)
        self.assertEqual(self.observed, [first, first])


class RecordedOwnerReconciliationTests(ChildPlanBase):
    """N7: a crashed parent's transfer record is charged to the pool it was moving FROM, and removed outputs are released."""
    def setUp(self):
        super().setUp()
        self.context.survivors = []

    def crashed(self, move):
        """A parent prepared a transfer of a spool-held file and died; a NEW ledger over the same directory restarts."""
        source = self.out / 'moving.bin'
        source.write_bytes(b'x' * 6000)
        source.chmod(0o600)
        held = self.context.ledger.reserve('snapshot_transport', 'snapshot_spool_bytes', 'bytes', 6000)
        held.track(source)
        (self.out / 'state').mkdir()
        destination = self.out / 'state' / 'catalog.sqlite'
        self.context.ledger.prepare_transfer(held.reservation_id, 'catalog', 'catalog_main', 'main', destination)
        if move:
            os.replace(source, destination)  # the crash came after the rename
        restarted = self.make_context(child_policy(self.source_map))
        restarted.survivors = []
        return restarted, source, destination

    def test_a_file_still_at_its_source_is_charged_to_the_pool_the_record_names(self):
        restarted, source, _destination = self.crashed(move=False)
        restarted.reconcile_crash()
        used = restarted.ledger.used
        self.assertEqual(used[('snapshot_spool_bytes', 'bytes')], 6000)  # not restore_staging_bytes
        self.assertEqual(used[('restore_staging_bytes', 'bytes')], 0)
        self.assertEqual([(r.owner, r.key, r.bytes) for r in restarted.survivors],
                         [('snapshot_transport', ('snapshot_spool_bytes', 'bytes'), 6000)])
        self.assertEqual(restarted.survivors[0].paths, {source})
        self.assertEqual(list((self.root / physical.TRANSFER_DIRECTORY).glob('*.json')), [])

    def test_a_file_already_at_its_destination_is_charged_to_the_destination_family(self):
        restarted, _source, destination = self.crashed(move=True)
        restarted.reconcile_crash()
        used = restarted.ledger.used
        self.assertEqual(used[('catalog_main', 'main')], 6000)
        self.assertEqual(used[('snapshot_spool_bytes', 'bytes')], 0)
        self.assertEqual(restarted.survivors[0].paths, {destination})

    def test_a_transfer_record_that_does_not_name_its_owners_is_refused(self):
        restarted, _source, _destination = self.crashed(move=False)
        (record_path,) = (self.root / physical.TRANSFER_DIRECTORY).glob('*.json')
        record = json.loads(record_path.read_bytes())
        del record['source']['owner']
        record_path.write_bytes(physical.canonical(record))
        with self.assertRaisesRegex(receipts.ReceiptError, 'does not name the owner it was moving from'):
            restarted.reconcile_crash()

    def test_run_zero_is_not_a_job_output_and_does_not_break_the_discard(self):
        for name in ('run-0', 'run-1', 'run-2'):
            (self.root / name).mkdir()
        outputs = self.context.prior_job_outputs()
        self.assertEqual(outputs, [session.job_output_directory(self.root, 1), session.job_output_directory(self.root, 2)])
        self.context.discard_prior_outputs(session.job_output_directory(self.root, 2))
        self.assertTrue((self.root / 'run-0').exists())  # never matched, never removed
        self.assertFalse((self.root / 'run-1').exists())

    def test_a_survivor_inside_a_discarded_job_output_is_released_with_its_files(self):
        earlier = session.job_output_directory(self.root, 1)
        current = session.job_output_directory(self.root, 2)
        earlier.mkdir()
        current.mkdir()
        leftover = earlier / 'moving.bin'
        leftover.write_bytes(b'x' * 6000)
        leftover.chmod(0o600)
        held = self.context.ledger.reserve('snapshot_restore', 'restore_staging_bytes', 'bytes', 6000)
        held.track(leftover)
        self.context.survivors.append(held)
        kept = current / 'kept.bin'
        kept.write_bytes(b'y' * 4000)
        kept.chmod(0o600)
        keep = self.context.ledger.reserve('snapshot_restore', 'restore_staging_bytes', 'bytes', 4000)
        keep.track(kept)
        self.context.survivors.append(keep)
        self.context.discard_prior_outputs(current)
        self.assertTrue(held.released)  # the file is gone, so its charge is gone with it
        self.assertFalse(keep.released)
        self.assertEqual(self.context.ledger.used[('restore_staging_bytes', 'bytes')], 4000)
        self.assertEqual(self.context.survivors, [keep])


class GrantPlanScratchTests(ChildPlanBase):
    """N7: the runtime pool must exceed the output share; the child's scratch is never granted empty."""
    def share(self):
        return physical.unit_output_bytes(UNIT_BYTES)

    def scratch(self, pool):
        context = self.make_context(child_policy(self.source_map, source_runtime_scratch_bytes=pool))
        allocations, _attachments = context.child_plan({'operation': 'unit'}, self.out)
        return {e['relative_path']: e['bytes'] for e in allocations if e['resource'] == 'source_runtime_scratch_bytes'}

    def test_a_pool_exactly_equal_to_the_output_share_leaves_no_scratch_and_is_refused(self):
        context = self.make_context(child_policy(self.source_map, source_runtime_scratch_bytes=self.share()))
        with self.assertRaises(physical.PhysicalIncomplete):
            context.child_plan({'operation': 'unit'}, self.out)

    def test_one_byte_more_than_the_output_share_delegates_exactly_that_byte(self):
        shares = self.scratch(self.share() + 1)
        self.assertEqual(shares, {'out': self.share(), 'out-aux/child-scratch': 1})

    def test_a_pool_below_the_output_share_is_refused(self):
        context = self.make_context(child_policy(self.source_map, source_runtime_scratch_bytes=self.share() - 1))
        with self.assertRaises(physical.PhysicalIncomplete):
            context.child_plan({'operation': 'unit'}, self.out)


class RestoreMainLimitReachabilityTests(unittest.TestCase):
    """A restore validator holds only an attachment; no code it runs may need a verifier_main limit."""
    CHECKER = ROOT / 'verification' / 'async-driver-refinement' / 'paged_checker.py'
    CODEC = ROOT / 'verification' / 'async-driver-refinement' / 'paged_codec.py'
    LOOKUPS = ('enforce_database_cap(', 'check_database(', 'family_limits(', 'prospective_commit(', 'RecordIndex(')

    @staticmethod
    def body(text, name, kind='def'):
        start = re.search(r'^(?: *)%s %s\b' % (kind, name), text, re.M)
        assert start is not None, name
        indent = len(start.group(0)) - len(start.group(0).lstrip())
        # Scan from the next line: the remainder of the header line is never a boundary (it would end the body at once).
        header_end = text.find('\n', start.end()) + 1
        end = re.search(r'^ {0,%d}\S' % indent, text[header_end:], re.M)
        return text[start.start():header_end + (end.start() if end else len(text))]

    def test_restore_validators_and_their_read_only_views_never_look_up_a_main_limit(self):
        checker, codec_text = self.CHECKER.read_text(), self.CODEC.read_text()
        for name in ('validate_restore', 'validate_restored_verifier', 'private_input_view'):
            body = self.body(checker, name)
            for lookup in self.LOOKUPS:
                self.assertNotIn(lookup, body, (name, lookup))
        view = self.body(codec_text, 'readonly_view')
        for lookup in self.LOOKUPS:
            self.assertNotIn(lookup, view, lookup)

    def test_the_only_verifier_family_a_restore_validator_enforces_is_the_granted_continuation_scratch(self):
        codec_text = self.CODEC.read_text()
        resolver = self.body(codec_text, 'ContinuationResolver', 'class')
        self.assertIn("enforce_database_cap(self.db, 'verifier_continuation_scratch')", resolver)
        self.assertNotIn("'verifier_main'", resolver)


if __name__ == '__main__':
    unittest.main()
