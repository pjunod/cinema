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
import sqlite3
from unittest import mock

from validation import async_refinement_provenance as provenance
from validation import async_refinement_session as session

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

    def test_dispatch_ordinals_are_reserved_once_and_exhaust_without_reuse(self):
        _db, ledger = structured_database()
        self.assertEqual([ledger.reserve_dispatch('publish', 'a' * 64, 3) for _ in range(3)], [0, 1, 2])
        with self.assertRaises(transport.SnapshotTransportError):
            ledger.reserve_dispatch('publish', 'a' * 64, 3)
        self.assertEqual(ledger.db.execute('SELECT count(*) FROM dispatches').fetchone()[0], 3)


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
            with self.assertRaises(Exception):
                session.child_environment({}, self.output / 'request.json', self.output, self.output, None)
            with self.assertRaises(Exception):
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


if __name__ == '__main__':
    unittest.main()
