"""Small independent graph fixtures for paged proof plumbing.

Definitions only until the complete implementation's final independent review.
These micrographs exercise monitors/SCC/fairness, not selected production D.
"""
from __future__ import annotations

import atexit
import copy
import functools
import itertools
import json
import os
from pathlib import Path
import shutil
import sqlite3
import sys
import tempfile
import unittest
from unittest import mock

PACKAGE = Path(__file__).resolve().parents[2] / 'verification' / 'async-driver-refinement'
sys.path.insert(0, str(PACKAGE))
import paged_codec as codec
import paged_scc as scc

SOURCE = {k: ('1' * 40 if k in ('commit', 'tree') else '2' * 64)
          for k in ('commit', 'tree', 'source_closure_sha256', 'driver_sha256', 'finite_inventory_sha256',
                    'proof_contract_sha256', 'toolchain_sha256', 'workflow_sha256')}
SESSION = '00000000-0000-4000-8000-000000000001'


MIB = 1024 * 1024
FIXTURE_DATABASE_BYTES = 64 * MIB


def physical():
    return codec.physical_policy_module()


def fixture_policy(source_map):
    """A closed PhysicalPolicy3 whose reservations cover every declared simultaneous database lifetime."""
    ceiling = FIXTURE_DATABASE_BYTES
    families = {name: {'owner': owner, **physical().minimum_family_policy(source_map, name, ceiling)}
                for name, owner in physical().FAMILY_OWNERS.items()}
    pools = {name: 64 * MIB for name in physical().POOL_OWNERS}
    return {'schema_version': 3, 'kind': 'physical_policy', 'table_map_sha256': physical().digest(source_map),
            'families': families, 'pools': pools}


@functools.lru_cache(maxsize=None)
def source_bounds():
    """Canonical OperationalBounds3: typed physical policy + honest calibration, independent of record quotas."""
    module = physical()
    source_map = module.load_source_map()
    policy = fixture_policy(source_map)
    geometry = {}
    for family, databases in source_map['families'].items():
        geometry[family] = []
        for entry in databases:
            connection = sqlite3.connect(':memory:')
            try:
                geometry[family].append(module.install_empty_role(connection, entry['database_role'], source_map))
            finally:
                connection.close()
    calibration = module.calibrate_physical_policy(
        policy, geometry, {'source_workspace_bytes': 0, 'existing_unpooled_bytes': 0},
        {'total_retained_artifact_bytes': TOTAL_RETAINED, 'maximum_units': MAXIMUM_UNITS,
         'total_session_disk_bytes': TOTAL_DISK}, source_map=source_map)
    schema = codec.strict_json((PACKAGE / 'paged-proof.schema.json').read_bytes())
    kinds = [item['properties']['kind']['const'] for item in schema['oneOf']]
    return codec.canonical({'schema_version': 3, 'accounting_rule': 'typed_physical_caps_v1',
        'source_receipt': fixture_source_receipt(),
        'record_allowances': {kind: {'records': 1000000, 'max_encoded_bytes': 8 * MIB} for kind in kinds},
        'physical_policy': policy, 'calibration': calibration,
        'accounting_method_sha256': '4' * 64, 'measurement_sha256': '5' * 64})


def fixture_source_receipt():
    """Structurally complete SourceBoundReceipt for plan schema checks only.

    paged_bounds.derive produces the real receipt and verifies every pin; the
    plan fixtures here need the same shape without depending on those pins.
    """
    specification = codec.strict_json((PACKAGE / 'source-bound-formulas.json').read_bytes())
    return {'schema_version': 1, 'kind': 'source_bound_receipt', 'source': dict(SOURCE),
            'specification_sha256': codec.sha_bytes((PACKAGE / 'source-bound-formulas.json').read_bytes()),
            'finite_inventory_sha256': specification['finite_inventory_sha256'],
            'record_schemas': specification['record_schemas'], 'algorithm_version': 1,
            'report_role': 'source_global_symbolic', 'premise_class': 'reviewed_source_lemmas',
            'machine_sqlite_rust_proof': False, 'init_class': specification['init_class'],
            'primitive_bindings': specification['primitive_bindings'], 'formulas': specification['formulas'],
            'records': specification['records'],
            'complete_kind_inventory_sha256': codec.sha_bytes(codec.encoded([r['record_kind'] for r in specification['records']])),
            'acceptance': False}


TOTAL_DISK = 2**34
TOTAL_RETAINED = 2**30
MAXIMUM_UNITS = 100
_LIVE_PARENTS = []


@atexit.register
def _release_parents():
    for parent in _LIVE_PARENTS:
        parent.close()


class GrantParent:
    """The parent half of LocalPhysicalGrant1: deducts, seals and passes the child its grant.

    The fixture child never receives the full policy ledger. It inherits only
    the sealed read-only descriptor and the held lease through the three
    environment variables, exactly as a spawned verifier does.
    """
    def __init__(self, plan):
        module = physical()
        base = PACKAGE.parents[1] / '.async-proof-sessions' / 'paged-fixtures'
        base.mkdir(parents=True, exist_ok=True)
        self.directory = Path(tempfile.mkdtemp(prefix='physical-parent-', dir=base))
        policy = codec.strict_json(plan['source_bounds_utf8'])['physical_policy']
        self.ledger = module.ReservationLedger(policy, self.directory)
        lease = self.directory / 'lease'
        descriptor = os.open(lease, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(descriptor)
        self.lease_fd = os.open(lease, os.O_RDONLY)
        ceiling = FIXTURE_DATABASE_BYTES

        def database(family, name):
            owner = module.FAMILY_OWNERS[family]
            return [{'owner': owner, 'resource': family, 'component': 'main', 'bytes': ceiling,
                     'relative_path': name, 'recursive': False},
                    {'owner': owner, 'resource': family, 'component': 'journal', 'bytes': 2 * ceiling + MIB,
                     'relative_path': name, 'recursive': False},
                    {'owner': owner, 'resource': family, 'component': 'temp', 'bytes': 2 * ceiling,
                     'relative_path': name + '.temp', 'recursive': True}]
        allocations = (database('verifier_main', 'work.sqlite') + database('verifier_continuation_scratch', 'continuation.sqlite')
                       + [{'owner': module.POOL_OWNERS['snapshot_spool_bytes'], 'resource': 'snapshot_spool_bytes',
                           'component': 'bytes', 'bytes': 64 * MIB, 'relative_path': 'spool', 'recursive': True}])
        binding = {'source': plan['source'], 'session_id': plan['session_id'], 'resource_plan_sha256': plan['plan_sha256'],
                   'physical_policy_sha256': module.digest(policy)}
        self.handle = self.ledger.issue_child_grant(allocations, [], binding, module.HeldLease(self.lease_fd, 'lease'))
        _LIVE_PARENTS.append(self)

    def environment(self):
        return self.handle.environment()

    def close(self):
        self.handle.close()
        try:
            os.close(self.lease_fd)
        except OSError:
            pass
        shutil.rmtree(self.directory, ignore_errors=True)


def granted_budget(plan, directory, *, seconds=None):
    """Budget for an allocation-capable child: built only from an explicit inherited grant.

    The plan's identity is bound into a fresh parent-issued descriptor, so any
    test that edits the plan and recomputes plan_sha256 gets a matching grant.
    """
    parent = GrantParent(plan)
    with mock.patch.dict(os.environ, parent.environment()):
        return codec.Budget(plan, directory, seconds=seconds)


def admitted(directory):
    value = codec.strict_json((PACKAGE / 'paged-resource-plan.example.json').read_bytes())
    plan = value['record'] if 'record' in value else value
    plan = copy.deepcopy(plan)
    plan.update(admission='admitted', session_id=SESSION, source=SOURCE,
                resident_bytes=512 * 1024 * 1024, resident_rss_bytes=2**40,
                total_session_disk_bytes=TOTAL_DISK, total_retained_artifact_bytes=TOTAL_RETAINED,
                maximum_units=MAXIMUM_UNITS, unit_predecessor_allowance=100000,
                unit_record_allowance=1000000, unit_byte_allowance=2**30,
                available_disk_receipt={'kind': 'runtime_receipt', 'id': 'disk.json', 'sha256': '3' * 64})
    plan['source_bounds_utf8'] = source_bounds()
    plan['plan_sha256'] = codec.digest(codec.envelope('ResourcePlan', plan))
    return granted_budget(plan, directory)


def idle():
    return {'stage': 'idle', 'job': -1, 'attempt': '', 'started': False, 'dropped': False}


class Graph:
    def __init__(self, directory, *, nodes, edges, enabled=None, context='base', goals=(), capacities=None, workers=None):
        self.index = codec.RecordIndex(Path(directory) / 'index.sqlite', source=SOURCE, session_id=SESSION, budget=admitted(directory))
        self.db, self.context = self.index.db, context
        self.enabled = enabled or {}
        self.edges, self.nodes = edges, nodes
        self.goals=set(goals)
        for n in nodes:
            p = {'id': n, 'context': context, 'sql':n-1, 'w': copy.deepcopy((workers or {}).get(n, [idle(), idle()]))}
            raw={'jobs':[{'request_done':n in goals} for _ in range(2)]}
            self.db.execute('INSERT INTO v_stores(id,canonical,payload,checked) VALUES(?,?,?,1)',(n-1,codec.canonical([n,raw]),codec.encoded(raw)))
            self.db.execute('INSERT INTO v_products(id,payload,goal,capacity) VALUES(?,?,?,?)',
                            (n, codec.encoded(p), n in goals, (capacities or {}).get(n, True)))
            self.index.add(codec.envelope('Enabledness', {'context': context, 'product_id': n, 'full_graph_labels': sorted(self.enabled.get(n, []))}))
        self.db.execute('INSERT INTO v_roots VALUES(?,?)', (context, nodes[0]))
        pairs = {}
        for identity, (src, dst, labels) in enumerate(edges, 1):
            self.db.execute('INSERT INTO v_edges(id,src,dst) VALUES(?,?,?)', (identity, src, dst))
            self.index.add(codec.envelope('ProductEdge', {'id': identity, 'context': context, 'from_product': src,
                'to_product': dst, 'candidate': identity, 'branch_utf8': codec.canonical({'family': 'fixture', 'worker': None, 'choice': {}}),
                'primary_edge': None, 'resolution_edge': None, 'read_edge': None, 'cleanup_edge': None}))
            pairs.setdefault((src, dst), set()).update(labels)
        for (src, dst), labels in pairs.items():
            self.index.add(codec.envelope('FairPair', {'context': context, 'from_product': src, 'to_product': dst,
                'labels': sorted(labels), 'nonstuttering': src != dst}))
        self.db.commit()
        self.temporal = scc.Temporal(self.index)

    def build(self):
        self.temporal.produce_monitors()
        self.temporal.verify_monitors()
        self.temporal.produce_sccs()
        self.temporal.verify_sccs()

    def components(self, prop='seen_goal_false', obligation=None):
        return [r for r in self.index.find('Scc') if r['property'] == prop and r['obligation'] == obligation]

    def close(self):
        self.index.close()


def monolithic_fair_bad(graph):
    """Tiny independent powerset/reachability oracle; never used at runtime."""
    weak, strong = scc.fairness(graph.context)
    adjacency = {(a, b) for a, b, _ in graph.edges}
    reachable = set() if graph.nodes[0] in graph.goals else {graph.nodes[0]}
    for _ in graph.nodes:
        reachable |= {b for a,b in adjacency if a in reachable and b not in graph.goals}
    for size in range(1, len(graph.nodes) + 1):
        for subset in itertools.combinations(graph.nodes, size):
            subset = set(subset)
            if not subset <= reachable:
                continue
            good = True
            for start in subset:
                seen = {start}
                for _ in subset:
                    seen |= {b for a, b in adjacency if a in seen and b in subset}
                good &= seen == subset
            if not good:
                continue
            occurred = set().union(*(set(labels) for a, b, labels in graph.edges if a != b and a in subset and b in subset))
            en = [set(graph.enabled.get(n, [])) for n in subset]
            if all(a in occurred or any(a not in e for e in en) for a in weak) and all(a in occurred or all(a not in e for e in en) for a in strong):
                return True
    return False


class PagedTemporalFixtures(unittest.TestCase):
    def setUp(self):
        base = PACKAGE.parents[1] / '.async-proof-sessions' / 'paged-fixtures'
        base.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=base)
        self.graphs = []

    def tearDown(self):
        for graph in self.graphs:
            graph.close()
        self.directory.cleanup()

    def graph(self, **fields):
        directory = Path(self.directory.name) / str(len(self.graphs))
        directory.mkdir()
        graph = Graph(directory, **fields)
        self.graphs.append(graph)
        return graph

    def compare(self, graph):
        graph.build()
        composed = any(graph.temporal.fair_walk(c) is not None for c in graph.components())
        self.assertEqual(composed, monolithic_fair_bad(graph))
        return composed

    def test_outgoing_only_enabledness_prevents_false_internal_fair_cycle(self):
        graph = self.graph(nodes=[1, 2], edges=[(1, 2, ['ClaimSend/0'])], enabled={1: ['ClaimSend/0']})
        self.assertTrue(self.compare(graph))  # terminal original stutter is fair
        first = next(c for c in graph.components() if graph.temporal.node(c['monitor_root'])['product'] == 1)
        self.assertIsNone(graph.temporal.fair_walk(first))

    def test_original_deadlock_stutter_is_a_real_suffix(self):
        self.assertTrue(self.compare(self.graph(nodes=[1], edges=[])))

    def test_nonstuttering_occurrence_does_not_count_labelled_selfloop(self):
        graph = self.graph(nodes=[1,2], edges=[(1,1,['ClaimSend/0']),(1,2,['ClaimSend/0'])], enabled={1:['ClaimSend/0']},goals={2})
        self.assertFalse(self.compare(graph))

    def test_wf_and_sf_differ_with_periodically_disabled_action(self):
        edges = [(1,2,['ClaimSend/0']),(2,1,['ClaimSend/0']),(2,3,['Admission/0'])]
        enabled={1:['ClaimSend/0'],2:['ClaimSend/0','Admission/0']}
        weak = self.graph(nodes=[1,2,3],edges=edges,enabled=enabled,context='weak_admission',goals={3})
        strong = self.graph(nodes=[1,2,3],edges=edges,enabled=enabled,context='stable',goals={3})
        self.assertTrue(self.compare(weak))
        self.assertFalse(self.compare(strong))

    def test_bad_fair_subcycle_inside_larger_unfair_scc_is_found(self):
        graph = self.graph(nodes=[1,2,3],edges=[(1,1,[]),(1,2,[]),(2,1,[]),(2,3,['Admission/0'])],enabled={2:['Admission/0']},context='stable',goals={3})
        self.assertTrue(self.compare(graph))
        walk = graph.temporal.fair_walk(graph.components()[0])
        self.assertIsNotNone(walk)

    def test_pair_label_union_keeps_all_original_action_families(self):
        graph = self.graph(nodes=[1, 2], edges=[(1, 2, ['ClaimSend/0']), (1, 2, ['ClaimReply/0']), (2, 1, [])],
                           enabled={1: ['ClaimSend/0', 'ClaimReply/0']})
        self.assertTrue(self.compare(graph))
        self.assertEqual(graph.index.get('FairPair', codec.canonical(['base', 1, 2]))['labels'], ['ClaimReply/0', 'ClaimSend/0'])

    def test_goal_seen_in_prefix_does_not_restart_at_later_component(self):
        graph = self.graph(nodes=[1, 2, 3], edges=[(1, 2, []), (2, 3, []), (3, 3, [])], goals={2})
        graph.build()
        recurrent = [n for n in graph.index.find('MonitorNode') if n['product'] == 3]
        self.assertTrue(all(n['seen_goal'] for n in recurrent))
        self.assertFalse(any(n['id'] in c['members'] for n in recurrent for c in graph.components()))

    def test_pending_crosses_component_boundary_and_fixed_identity_is_retained(self):
        trigger = dict(idle(), stage='driver', job=0, attempt=scc.CLAIMS[0], started=True)
        graph = self.graph(nodes=[1, 2, 3], edges=[(1, 2, []), (2, 3, []), (3, 3, [])], workers={2: [trigger, idle()]})
        graph.build()
        obligation = {'node': 0, 'job': 0, 'claim_id': scc.CLAIMS[0]}
        terminal = next(n for n in graph.index.find('MonitorNode') if n['product'] == 3)
        self.assertIn(obligation, terminal['pending_attempts'])
        self.assertTrue(any(terminal['id'] in c['members'] for c in graph.components('capacity_ok_pending_true', obligation)))

    def test_capacity_false_in_prefix_cannot_be_erased_in_component(self):
        trigger = dict(idle(), stage='driver', job=0, attempt=scc.CLAIMS[0], started=True)
        graph = self.graph(nodes=[1, 2, 3], edges=[(1, 2, []), (2, 3, []), (3, 3, [])], capacities={1: False}, workers={2: [trigger, idle()]})
        graph.build()
        obligation = {'node': 0, 'job': 0, 'claim_id': scc.CLAIMS[0]}
        self.assertFalse(graph.components('capacity_ok_pending_true', obligation))
        self.assertTrue(graph.components('pending_true', obligation))

    def test_recurring_completed_obligations_are_not_permanent_pending(self):
        trigger = dict(idle(), stage='driver', job=0, attempt=scc.CLAIMS[0], started=True)
        response = dict(trigger, dropped=True)
        graph = self.graph(nodes=[1, 2], edges=[(1, 2, []), (2, 1, [])], workers={1: [trigger, idle()], 2: [response, idle()]})
        graph.build()
        obligation = {'node': 0, 'job': 0, 'claim_id': scc.CLAIMS[0]}
        components = graph.components('capacity_ok_pending_true', obligation)
        # A singleton pending suffix with an enabled exit cannot itself recur;
        # original implicit stutter is still independently fairness-filtered.
        self.assertTrue(all(len(c['members']) == 1 for c in components))

    def test_forged_forward_tree_is_rejected_before_any_scc_credit(self):
        graph = self.graph(nodes=[1, 2], edges=[(1, 2, []), (2, 1, [])])
        graph.temporal.produce_monitors()
        graph.temporal.verify_monitors()
        graph.temporal.produce_sccs()
        component = graph.components()[0]
        original = graph.index.get_envelope('Scc', component['id'])
        malformed = copy.deepcopy(original)
        malformed['record']['forward_tree'] = []
        graph.db.execute('UPDATE records SET payload=? WHERE kind=? AND key=?', (codec.encoded(malformed), 'Scc', codec.canonical(component['id'])))
        with self.assertRaisesRegex(codec.Invalid, 'arborescence'):
            graph.temporal.verify_sccs()

    def test_missing_monitor_edge_is_rejected(self):
        graph = self.graph(nodes=[1, 2], edges=[(1, 2, [])])
        graph.temporal.produce_monitors()
        edge = next(graph.index.find('MonitorEdge'))
        graph.db.execute("DELETE FROM records WHERE kind='MonitorEdge' AND key=?", (codec.canonical(edge['id']),))
        with self.assertRaisesRegex(codec.Invalid, 'omitted monitor successor'):
            graph.temporal.verify_monitors()

    def test_forged_condensation_rank_cannot_split_a_reachable_edge(self):
        graph=self.graph(nodes=[1,2],edges=[(1,2,[])])
        graph.temporal.produce_monitors()
        graph.temporal.verify_monitors()
        graph.temporal.produce_sccs()
        components=graph.components()
        self.assertEqual(len(components),2)
        for component in components:
            malformed=copy.deepcopy(graph.index.get_envelope('Scc',component['id']))
            malformed['record']['condensation_rank']=0
            graph.db.execute('UPDATE records SET payload=? WHERE kind=? AND key=?',
                (codec.encoded(malformed),'Scc',codec.canonical(component['id'])))
        with self.assertRaisesRegex(codec.Invalid,'condensation'):
            graph.temporal.verify_sccs()

    def test_unreachable_monitor_history_cannot_acquire_prefix_credit(self):
        graph=self.graph(nodes=[1,2],edges=[(1,2,[])])
        graph.temporal.produce_monitors()
        identity=max(n['id'] for n in graph.index.find('MonitorNode'))+1
        graph.index.add(codec.envelope('MonitorNode',{'id':identity,'context':'base','product':2,
            'seen_goal':True,'capacity_ok':True,'pending_attempts':[]}))
        with self.assertRaisesRegex(codec.Invalid,'orphan monitor history'):
            graph.temporal.verify_monitors()


if __name__ == '__main__':
    unittest.main()
