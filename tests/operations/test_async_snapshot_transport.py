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
        return session.begin_pending({}, {}, ROOT, self.root / 'history', sent.auth, self.index, None)

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

    def started(self):
        first = self.send(self.pending, {('publication_intent', self.intent_ref['sha256'])}, operation='execute')
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
        """Intent, then one real chunk, then the remaining closure: three nonempty transfer batches."""
        first = self.send(self.pending, {('publication_intent', self.intent_ref['sha256'])}, operation='execute')
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
        receipt_ref = self.inventory.put('validation_receipt', transport.canonical({'ok': True}))
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
            'postcheck': {'checked_at': '2026-01-01T00:00:00Z', 'source_commit': 'a' * 40, 'current_base': 'a' * 40}}


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
        stale['postcheck']['current_base'] = 'b' * 40
        with self.assertRaises(receipts.ReceiptError):
            check_identity(stale)

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


if __name__ == '__main__':
    unittest.main()
