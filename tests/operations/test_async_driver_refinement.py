"""Fail-closed importer/receipt contracts. Synthetic fixtures are parser tests,
not source graphs or evidence for the finite protocol theorem. No solver runs.
"""
from __future__ import annotations
import copy
import importlib.util
import importlib.machinery
import json
import io
import hashlib
import re
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('async_refinement_checker',ROOT/'verification/async-driver-refinement/checker.py')
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)

LOADER = importlib.machinery.SourceFileLoader('async_refinement_runner',str(ROOT/'scripts/check-async-driver-refinement'))
RUNNER_SPEC = importlib.util.spec_from_loader(LOADER.name,LOADER)
runner = importlib.util.module_from_spec(RUNNER_SPEC)
LOADER.exec_module(runner)


def fixture():
    states = [dict(id=i,phase='Terminal',held=dict(admission=i == 0,queue=False,domain=False,publisher=False),
      work='Idle',revoked=False,cancelled=False,pending='DropAdmission' if i == 0 else None,
      remaining=[],terminal=i == 1) for i in range(2)]
    return {'schema_version':1,'scope':'synthetic parser fixture only','initial':0,
      'domain':{'reply_classes':list(checker.REPLIES)},'states':states,
      'edges':[{'from':0,'to':1,'reply':'Done','request':None}],
      'rejected':[{'state':i,'reply':r,'unchanged':True} for i in range(2) for r in checker.REPLIES
                  if (i,r) != (0,'Done')]}


class DriverInventoryContracts(unittest.TestCase):
    def test_every_input_including_terminal_must_be_classified_once(self):
        graph = fixture()
        self.assertEqual(checker.validate_driver(graph),[1,0])
        graph['rejected'].pop()
        with self.assertRaisesRegex(checker.Invalid,'missing driver input'):
            checker.validate_driver(graph)

    def test_missing_target_is_not_an_ignored_transition(self):
        graph = fixture(); graph['edges'][0]['to'] = 2
        with self.assertRaisesRegex(checker.Invalid,'missing/outside edge target'):
            checker.validate_driver(graph)

    def test_unknown_reply_or_mutating_rejection_is_refused(self):
        for change in ('unknown','mutation','overlap'):
            graph = fixture()
            if change == 'unknown': graph['rejected'][0]['reply'] = 'Undocumented'
            elif change == 'mutation': graph['rejected'][0]['unchanged'] = False
            else: graph['rejected'].append({'state':0,'reply':'Done','unchanged':True})
            with self.subTest(change=change),self.assertRaises(checker.Invalid):
                checker.validate_driver(graph)

    def test_internal_cycles_do_not_acquire_a_fake_tail_rank(self):
        graph = fixture()
        graph['edges'][0].update(to=0,request='DropAdmission')
        graph['edges'].append({'from':0,'to':1,'reply':'Pressure','request':None})
        graph['rejected'] = [r for r in graph['rejected'] if (r['state'],r['reply']) != (0,'Pressure')]
        with self.assertRaisesRegex(checker.Invalid,'internal cycle'):
            checker.validate_driver(graph)

    def test_work_debug_format_drift_is_rejected_before_join_classification(self):
        graph = fixture(); graph['states'][0]['work'] = 'Completed(ok=true)'
        with self.assertRaisesRegex(checker.Invalid,'unknown driver Work Debug class'):
            checker.validate_driver(graph)

    def test_known_work_debug_classes_map_to_stable_model_tags(self):
        self.assertEqual(checker.driver_work_class('Completed { ok: true }'),'completed_succeeded')
        self.assertEqual(checker.driver_work_class('Completed { ok: false }'),'completed_failed')


class DomainInputContracts(unittest.TestCase):
    def test_empty_episode_includes_both_claim_pairings_and_foreign_domain(self):
        state = {'now':0,'jobs':[{'state':'queued','owner':None,'expiry':None} for _ in range(2)],
          'captured_tokens':[[],[]],'captured_leases':[],'domain':None,
          'budgets':{'claims':[0,0],'renewed_claims':[],'domain_acquired':[False]*3,'removed':[False]*2}}
        actions = checker.expected_actions(state)
        self.assertEqual(len(actions),7)  # Four claims, foreign acquire, two removals.
        self.assertIn(checker.canonical({'op':'claim','job':1,'node':0}),actions)
        self.assertNotIn(checker.canonical({'op':'acquire_domain','node':0}),actions)

    def test_concrete_max_boundary_has_no_wrapped_plus_one(self):
        state = {'now':2**63-2,'jobs':[{'state':'running','owner':'node-a','expiry':2**63-1},
          {'state':'queued','owner':None,'expiry':None}], 'captured_tokens':[[],[]],'captured_leases':[],
          'domain':None,'budgets':{'claims':[2,2],'renewed_claims':[],'domain_acquired':[True]*3,'removed':[True]*2}}
        self.assertEqual(checker.expected_actions(state),{checker.canonical({'op':'clock_advance','now':2**63-1})})
        state['now'] = 2**63-1
        self.assertEqual(checker.expected_actions(state),set())

    def test_retirement_and_resolve_keep_every_historical_original_input(self):
        old = dict(job_id='J0',node_id='node-a',boot_id='B',claim_id='C',fence=1,
                   revision=1,lease_expires_ms=10)
        state = {'now':0,'jobs':[{'state':'running','owner':'node-a','expiry':10},
          {'state':'queued','owner':None,'expiry':None}],
          'captured_tokens':[[old,{**old,'revision':2,'lease_expires_ms':20}],[]],
          'captured_leases':[],'domain':None,
          'budgets':{'claims':[2,2],'renewed_claims':['C'],'domain_acquired':[True]*3,'removed':[True]*2}}
        actions = checker.expected_actions(state)
        for token in (0,1):
            self.assertIn(checker.canonical({'op':'resolve','job':0,'token':token}),actions)
            self.assertIn(checker.canonical({'op':'settle','job':0,'token':token,'disposition':3}),actions)
        self.assertNotIn(checker.canonical({'op':'settle','job':0,'token':0,'disposition':0}),actions)
        self.assertNotIn(checker.canonical({'op':'settle','job':0,'token':1,'disposition':0}),actions)
        self.assertIn(checker.canonical({'op':'settle','job':0,'token':0,'disposition':2}),actions)


def product_domain_fixture():
    """Small D input fixture, never an actual SQL result or qualification bundle."""
    return {'now':0,'jobs':[{'state':'queued','owner':None,'expiry':None,'request_done':False,'not_before_ms':0} for _ in range(2)],
      'captured_tokens':[[],[]],'captured_leases':[],'domain':None,
      'budgets':{'claims':[0,0],'renewed_claims':[],'domain_acquired':[False]*3,'removed':[False]*2}}


def product_predecessor(context='stable'):
    return {'id':0,'context':context,'sql':0,'w':[copy.deepcopy(checker.IDLE),copy.deepcopy(checker.IDLE)],
            'available':[True,True],'completed':[],'mono_now_ms':0,'wall_anchors':[],'mono_anchors':[],'claim_loss_keys':[]}


class ProductCoverageContracts(unittest.TestCase):
    """Independent helper contracts; no generated bundle, SQL, frontier or TLC run."""
    def test_eligible_claim_without_oracle_is_fatal_before_successor_lookup(self):
        p = product_predecessor(); p['w'][0].update(stage='claim',call='sent',job=0)
        store = {'states':[product_domain_fixture()],'edges':[]}
        demands = checker.expected_product_demands(p,fixture(),store)
        self.assertEqual([(d['branch']['family'],d['role'],d['exclusion']) for d in demands],
                         [('ClaimApply','apply',None)])
        with self.assertRaisesRegex(checker.Invalid,'MissingProductOracleEdge'):
            checker.complete_product_demands(p,fixture(),store,{})
        excluded = copy.deepcopy(demands); excluded[0]['exclusion'] = {'kind':'domain','reason':'attempt_budget'}
        with self.assertRaisesRegex(checker.Invalid,'complete independent enumeration'):
            checker.require_exact_inventory(excluded,demands,'demand coverage')

    def test_retirement_capacity_is_required_at_ready_and_sent(self):
        s = product_domain_fixture()
        token = dict(job_id='J0',node_id='node-a',boot_id='B',claim_id='C',fence=1,
                     revision=1,lease_expires_ms=10)
        s['captured_tokens'][0].append(token)
        p = product_predecessor(); p['w'][0].update(stage='driver',call='sent',job=0,token=0,
            ctrl=0,retire='first',original=0,sample=0)
        driver = {'states':[{'pending':'RetireQueue'}]}
        for inner in ('ready','sent'):
            p['w'][0]['inner'] = inner
            demands = checker.expected_product_demands(p,driver,{'states':[s]})
            self.assertEqual([d['role'] for d in demands],['capacity'] if inner == 'ready' else ['capacity','apply'])
            self.assertTrue(all(d['input']['disposition'] == 3 and d['exclusion'] is None for d in demands))
            with self.subTest(inner=inner),self.assertRaisesRegex(checker.Invalid,'MissingProductOracleEdge'):
                checker.complete_product_demands(p,driver,{'states':[s]}, {})
        p['w'][0]['sample'] = -1
        demands,used = checker.complete_product_demands(p,driver,{'states':[s]}, {})
        self.assertFalse(used)
        self.assertTrue(all(d['exclusion'] == {'kind':'sample','reason':'retirement_sample'} and
                            d['oracle_edge'] is None for d in demands))

    def test_capture_and_revision_exclusions_are_typed_not_forged_capacity(self):
        s = product_domain_fixture()
        token = dict(job_id='J0',node_id='node-a',boot_id='B',claim_id='C',fence=1,
                     revision=1,lease_expires_ms=10)
        s['captured_tokens'][0] = [token,{**token,'revision':2}]
        self.assertEqual(checker.classify_domain(s,{'op':'bind','job':0,'token':0,'lease':0}),
                         {'kind':'domain','reason':'ordinary_revision'})
        self.assertIsNone(checker.classify_domain(s,{'op':'settle','job':0,'token':0,'disposition':3}))
        p = product_predecessor(); p['w'][0].update(stage='driver',call='sent',ctrl=0,job=0)
        ds = checker.expected_product_demands(p,{'states':[{'pending':'Bind'}]},{'states':[s]})
        self.assertTrue(all(d['input'] is None and d['exclusion'] ==
                            {'kind':'capture','reason':'missing_token'} for d in ds))

    def test_complete_local_choices_and_targets_cannot_be_omitted(self):
        p = product_predecessor(); store = {'states':[product_domain_fixture()],'edges':[]}
        candidates = checker.expected_product_successors(p,fixture(),store,{})
        branches = [row[0] for row in candidates]
        self.assertEqual({(b['family'],b['worker']) for b in branches},
                         {('Admission',0),('Admission',1),('RemoveAvailability',0),('RemoveAvailability',1)})
        with self.assertRaisesRegex(checker.Invalid,'complete independent enumeration'):
            checker.require_exact_inventory(branches[:-1],branches,'branch coverage')
        with self.assertRaisesRegex(checker.Invalid,'MissingProductSuccessor'):
            checker.product_pair_obligations(p,candidates,{})
        targets = {checker.canonical(row[1]):i+1 for i,row in enumerate(candidates)}
        edges,enabled,_ = checker.product_pair_obligations(p,candidates,targets)
        self.assertEqual(len(edges),4)
        self.assertEqual(enabled['actions'],['Admission/0','Admission/1'])

    def test_pair_membership_unions_overlaps_but_stutter_is_not_enabled(self):
        p = product_predecessor(); same = checker.product_payload(p)
        q = copy.deepcopy(same); q['available'][0] = False
        branch = {'family':'ClaimApply','worker':0,'choice':{}}
        candidates = [(branch,q,0,{'ClaimApply/0','ClaimWin/0'},None,None,None),
          ({**branch,'family':'WorkSuccess'},q,1,{'ExternalReturn/0'},None,None,None),
          ({**branch,'family':'RetireSample'},same,None,{'RetireLocal/0'},None,None,None)]
        by_payload = {checker.canonical(same):0,checker.canonical(q):1}
        edges,enabled,_ = checker.product_pair_obligations(p,candidates,by_payload)
        union = ['ClaimApply/0','ClaimWin/0','ExternalReturn/0']
        self.assertEqual(edges[0]['fair_actions'],union)
        self.assertEqual(edges[1]['fair_actions'],union)
        self.assertEqual(edges[2]['fair_actions'],['RetireLocal/0'])
        self.assertEqual(enabled['actions'],union)
        forged = copy.deepcopy(edges); forged[0]['fair_actions'].remove('ClaimWin/0')
        with self.assertRaisesRegex(checker.Invalid,'complete independent enumeration'):
            checker.require_exact_inventory(forged,edges,'pair membership')
        with self.assertRaisesRegex(checker.Invalid,'duplicates'):
            checker.require_exact_inventory(edges+[edges[0]],edges,'pair membership')

    def test_orphan_is_rejected_without_claiming_full_product_acceptance(self):
        checker.validate_context_reachability(0,[0,1],[[1],[1]])
        with self.assertRaisesRegex(checker.Invalid,'orphan/unreachable'):
            checker.validate_context_reachability(0,[0,1,2],[[1],[1],[2]])

    def test_environment_keeps_unmatched_renewal_foreign_clock_and_removal(self):
        p = product_predecessor('base'); s = product_domain_fixture()
        token = dict(job_id='J0',node_id='node-a',boot_id='B',claim_id='C',fence=1,
                     revision=1,lease_expires_ms=10)
        s['captured_tokens'][0].append(token)
        s['captured_leases'].append(dict(resource='R',owner_node_id='foreign-holder',fence=1,
                                        revision=1,expires_at_unix_ms=10))
        unmatched = list(checker.environment_inputs(p,s))
        self.assertTrue({'renew','release_domain','remove_owner','acquire_domain'} <=
                        {a['op'] for a in unmatched})
        p['wall_anchors'] = [10]
        demands = checker.expected_product_demands(p,fixture(),{'states':[s]})
        self.assertIn('WallAdvance',{d['branch']['family'] for d in demands})
        p['w'][0].update(stage='driver',job=0,token=0,localToken=True,call='sent')
        self.assertNotIn('renew',{a['op'] for a in checker.environment_inputs(p,s)})
        p['w'][0]['call'] = 'ready'
        self.assertIn('renew',{a['op'] for a in checker.environment_inputs(p,s)})

    def test_poll_work_preserves_separate_queue_loss_signal_and_latched_priority(self):
        """Independent transition fixture, not real Driver/SQL qualification evidence."""
        p = product_predecessor('base');s = product_domain_fixture()
        token = dict(job_id='J0',node_id='node-a',boot_id='B',claim_id='C',fence=1,
                     revision=1,lease_expires_ms=30000)
        lease = dict(resource='R',owner_node_id='node-a',fence=1,revision=1,expires_at_unix_ms=30000)
        s['captured_tokens'][0] = [token];s['captured_leases'] = [lease]
        s['jobs'][0].update(state='running',owner='node-a',expiry=30000)
        p['w'][0].update(stage='driver',call='sent',ctrl=0,job=0,token=0,lease=0,
            attempt='C',localToken=True,started=True,readLock=True,local_deadline_mono_ms=30000)
        # A permitted completion is available in both phases, so suppressing
        # WorkSuccess after a signal cannot pass merely because SQL refused.
        done = copy.deepcopy(s);done['jobs'][0].update(state='succeeded',owner=None,expiry=None,request_done=True)
        complete = {'from':0,'to':1,'action':{'op':'complete','job':0,'token':0,'lease':0},'result':'accepted'}
        store = {'states':[s,done],'edges':[complete]}
        driver = {'states':[{'pending':'PollWork','terminal':False},{'pending':None,'terminal':True}],
            'edges':[{'from':0,'to':1,'reply':tag} for tag in
                     ('QueueLoss','DomainLoss','Pressure','WorkFailed','WorkSucceeded')]}

        def successors(state):
            # Keep the local fixture orthogonal to environmental primitives;
            # retain any independently enabled contextual wall demand.
            local = copy.deepcopy(store);target = checker.next_landmark(s['now'],state['wall_anchors'])
            if target is not None:
                advanced = copy.deepcopy(s);advanced['now'] = target
                local['states'].append(advanced)
                local['edges'].append({'from':0,'to':2,'action':{'op':'clock_advance','now':target},'result':'advanced'})
            oracle = {(edge['from'],checker.canonical(edge['action'])):i for i,edge in enumerate(local['edges'])}
            with mock.patch.object(checker,'environment_inputs',return_value=[]):
                return checker.expected_product_successors(state,driver,local,oracle)

        before = successors(p)
        unsignaled = [row for row in before if row[0]['family'] == 'WorkOther' and row[0]['worker'] == 0]
        self.assertEqual({row[0]['choice']['reply'] for row in unsignaled},{'DomainLoss','Pressure','WorkFailed'})
        self.assertTrue(any(row[0]['family'] == 'WorkSuccess' for row in before))
        for row in unsignaled:
            self.assertFalse(row[1]['w'][0]['queue_lost'])
            self.assertEqual(row[1]['w'][0]['call'],'applied')
            self.assertIn('ExternalReturn/0',row[3])

        signals = [row for row in before if row[0]['family'] == 'QueueLossSignal' and row[0]['worker'] == 0]
        self.assertEqual(len(signals),1);signal = signals[0]
        self.assertEqual(signal[1]['sql'],p['sql'])
        self.assertTrue(signal[1]['w'][0]['queue_lost'])
        for field in ('ctrl','call','reply','readLock','localToken'):
            self.assertEqual(signal[1]['w'][0][field],p['w'][0][field])
        self.assertNotIn('ExternalReturn/0',signal[3])

        after = successors({'id':0,**signal[1]})
        signaled = [row for row in after if row[0]['family'] == 'WorkOther' and row[0]['worker'] == 0]
        self.assertEqual([row[0]['choice']['reply'] for row in signaled],['QueueLoss'])
        self.assertTrue(signaled[0][1]['w'][0]['queue_lost'])
        self.assertEqual(signaled[0][1]['w'][0]['call'],'applied')
        self.assertIn('ExternalReturn/0',signaled[0][3])
        self.assertFalse(any(row[0]['family'] in {'WorkSuccess','QueueLossSignal'} and row[0]['worker'] == 0 for row in after))

    def test_coverage_obligations_precede_expected_negative_invariants(self):
        for name,config in checker.CONFIGS.items():
            if config.get('model') == 'BoundaryControls': continue
            with self.subTest(config=name):
                self.assertEqual(config['invariants'][:len(checker.COVERAGE_INVARIANTS)],
                                 list(checker.COVERAGE_INVARIANTS))

    def test_ordinary_transfer_status_cannot_self_authorize_acceptance(self):
        expected = {'ordinary_settlement_payload_transfer':{'status':'pending','domain':'selected_two_worker_normal_clock'}}
        checker.validate_source_boundaries(expected)
        for status in ('unclosed','assumed','waived','closed_bounded'):
            bad = copy.deepcopy(expected);bad['ordinary_settlement_payload_transfer']['status'] = status
            with self.subTest(status=status),self.assertRaises(checker.Invalid): checker.validate_source_boundaries(bad)
        with self.assertRaises(checker.Invalid): checker.validate_source_boundaries({**expected,'extra':{}})
        schema = checker.read_json(checker.PACKAGE/'product-schema.json')
        self.assertEqual(schema['schema_version'],4)
        self.assertEqual(schema['source_boundaries'],expected)

    def test_wire_schema_and_semantic_source_drift_fail_closed(self):
        schema = checker.read_json(checker.PACKAGE/'product-schema.json')
        checker.validate_product_schema(schema)
        for key,value in (('main_contexts',{}),('fair_families',[]),('resource_policy',{}),('store_state_additions',['raw']),('wire_columns',{}),('wire_value_ref_fields',{}),('wire_reference_pools',{}),('wire_nullable_reference_fields',{})):
            bad = copy.deepcopy(schema); bad[key] = value
            with self.subTest(field=key),self.assertRaisesRegex(checker.Invalid,'schema .* drift'):
                checker.validate_product_schema(bad)
        bindings = {path:'a'*64 for path in checker.SEMANTIC_PATHS}
        with mock.patch.object(checker,'sha',return_value='a'*64):
            checker.validate_semantic_bindings(bindings)
            path = next(iter(bindings)); bindings[path] = 'b'*64
            with self.assertRaisesRegex(checker.Invalid,'semantic source hash mismatch'):
                checker.validate_semantic_bindings(bindings)
        bindings['unexpected.py'] = 'a'*64
        with self.assertRaisesRegex(checker.Invalid,'semantic source registry differs'):
            checker.validate_semantic_bindings(bindings,False)

    def test_raw_preimage_and_episode_are_checked_without_a_table_oracle(self):
        s = product_domain_fixture()
        ep = {'now':0,'claims':[0,0],'tokens':[[],[]],'renewed_claims':[],'leases':[],
              'domain_acquired':[False]*3,'removed':[False]*2,'claim_snapshots':[]}
        s['raw'] = {'rows':{name:[] for name in checker.SNAPSHOT_TABLES},'episode':ep}
        s['canonical_json'] = checker.canonical(s['raw'])
        s['canonical_sha256'] = hashlib.sha256(s['canonical_json'].encode()).hexdigest()
        checker.validate_raw_store(s)
        bad = copy.deepcopy(s); bad['raw']['episode']['now'] = 1
        with self.assertRaisesRegex(checker.Invalid,'canonical preimage'):
            checker.validate_raw_store(bad)
        bad = copy.deepcopy(s); bad['now'] = 1
        with self.assertRaisesRegex(checker.Invalid,'episode/projection disagreement'):
            checker.validate_raw_store(bad)


def product_wire_fixture(states=None,edges=None,demands=None,enabledness=None):
    """Hand-built schema4 parser fixture, never a generated/complete relation."""
    states = copy.deepcopy(states if states is not None else [product_predecessor('base')])
    contexts = [{'id':name} for name in dict.fromkeys(p['context'] for p in states)]
    values,workers,value_ids,worker_ids = [],[],{},{}
    def intern(value):
        encoded = checker.canonical(value)
        if encoded not in value_ids:
            value_ids[encoded] = len(values);values.append(copy.deepcopy(value))
        return value_ids[encoded]
    def worker_ref(worker):
        row = [intern(worker[field]) if field in checker.WORKER_VALUE_REFS else worker[field]
            for field in checker.WORKER_COLUMNS]
        encoded = checker.canonical(row)
        if encoded not in worker_ids:
            worker_ids[encoded] = len(workers);workers.append(row)
        return worker_ids[encoded]
    def context_ref(name): return next(i for i,c in enumerate(contexts) if c['id'] == name)
    rows = []
    for p in states:
        encoded = checker.canonical(checker.product_payload(p))
        rows.append([context_ref(p['context']),p['sql'],worker_ref(p['w'][0]),worker_ref(p['w'][1]),
            intern(p['available']),intern(p['completed']),p['mono_now_ms'],intern(p['wall_anchors']),
            intern(p['mono_anchors']),intern(p['claim_loss_keys']),hashlib.sha256(encoded.encode()).hexdigest()])
    edge_rows = [[e['from'],e['to'],intern(e['branch']),e['oracle_edge'],e['resolution_edge'],
        e['read_edge'],e['cleanup_edge'],intern(e['fair_actions'])] for e in edges or []]
    demand_rows = [[context_ref(d['context']),d['from'],d['oracle_from'],intern(d['branch']),d['role'],
        intern(d['input']),d['oracle_edge'],intern(d['exclusion'])] for d in demands or []]
    enabled_rows = [[r['state'],intern(r['actions'])] for r in enabledness or []]
    store = product_domain_fixture()
    for job in store['jobs']: job.update(active=False,reservation_count=0)
    raw = {'rows':{name:[] for name in checker.SNAPSHOT_TABLES},'episode':{
        'now':store['now'],**store['budgets'],'tokens':store['captured_tokens'],'leases':store['captured_leases'],
        'claim_snapshots':[]}}
    raw['rows']['background_jobs'] = [['00000000-0000-4000-8000-000000000010',29999,30000]]
    # Whitespace is intentional: identity hashes original canonical_json bytes.
    text = json.dumps(raw,sort_keys=True,indent=1)
    store.update(id=0,cutoffs=[],canonical_json=text,canonical_sha256=hashlib.sha256(text.encode()).hexdigest())
    wire = {key:None for key in checker.PRODUCT_KEYS}
    wire.update(schema_version=4,scope='selected_two_worker_product',value_pool=values,worker_pool=workers,
        contexts=contexts,product_states=rows,product_edges=edge_rows,demands=demand_rows,enabledness=enabled_rows,
        store={'initial':0,'states':[store],'edges':[]})
    return wire


class CompactProductWireContracts(unittest.TestCase):
    """Lossless transport/helper fixtures; no SQL graph, staging or theorem claim."""
    @staticmethod
    def successor_rows(candidates):
        return [(branch,checker.canonical(payload),edge,sorted(fair),resolution,read,cleanup)
            for branch,payload,edge,fair,resolution,read,cleanup in candidates]

    @staticmethod
    def local_successors(state,driver,store):
        # Synthetic annotations cover unrelated Store branches; these are local
        # Next/codec fixtures, never actual-SQL graph or qualification evidence.
        demands = checker.expected_product_demands(state,driver,store)
        actions = {checker.canonical(d['input']):d['input'] for d in demands
            if d['role'] == 'apply' and d['exclusion'] is None}
        store['edges'] = [{'from':state['sql'],'to':state['sql'],'action':action,
            'result':{'clock_advance':'advanced','acquire_domain':'acquired','remove_owner':'removed'}[action['op']]}
            for action in actions.values()]
        oracle = {(edge['from'],checker.canonical(edge['action'])):i for i,edge in enumerate(store['edges'])}
        return checker.expected_product_successors(state,driver,store,oracle)

    def test_equal_pooled_workers_claim_send_changes_only_selected_occurrence(self):
        driver = fixture();store = {'states':[product_domain_fixture()],'edges':[]}
        state = product_predecessor('base')
        # Reach the reported shape through real independent Admission transitions:
        # both workers select job0 before either ClaimApply.
        for worker in (0,1):
            choices = self.local_successors(state,driver,store)
            selected = next(row for row in choices if row[0] ==
                {'family':'Admission','worker':worker,'choice':{'job':0}})
            state = {'id':0,**selected[1]}
        tree = json.loads(checker.canonical(state))
        self.assertEqual(tree['w'][0],tree['w'][1])
        self.assertIsNot(tree['w'][0],tree['w'][1])
        wire = product_wire_fixture([tree]);before = checker.canonical(wire)
        pooled = checker.decode_product(wire)['product_states'][0]
        self.assertIs(pooled['w'][0],pooled['w'][1])
        self.assertIs(pooled['completed'],pooled['claim_loss_keys'])
        expected = self.local_successors(tree,driver,store)
        actual = self.local_successors(pooled,driver,store)
        self.assertEqual(self.successor_rows(actual),self.successor_rows(expected))
        sent = next(row[1] for row in actual if row[0] == {'family':'ClaimSend','worker':0,'choice':{}})
        self.assertEqual(sent['w'][0]['call'],'sent');self.assertEqual(sent['w'][1]['call'],'ready')
        self.assertEqual(sent['w'][1],tree['w'][1])
        self.assertIsNot(sent['w'][0],sent['w'][1])
        self.assertIsNot(sent['w'][0]['ordinary'],sent['w'][1]['ordinary'])
        self.assertIsNot(sent['completed'],sent['claim_loss_keys'])
        sent['completed'].append(0)
        self.assertEqual(sent['claim_loss_keys'],[])
        self.assertEqual(pooled['completed'],[])
        self.assertEqual(checker.canonical(wire),before)

    def test_distinct_pooled_workers_ordinary_step_detaches_nested_values(self):
        driver = fixture();driver['states'][0]['pending'] = 'SettleYield'
        state = product_predecessor('stable');store_state = product_domain_fixture();store_state['now'] = 1000
        for n,worker in enumerate(state['w']):
            worker.update(stage='driver',call='sent',ctrl=0,job=n,token=0,localToken=True,
                attempt='C'+str(n),started=True,local_deadline_mono_ms=30000,
                driver_settlement={'kind':'yield','not_before_ms':6000})
            worker['ordinary'] = {**copy.deepcopy(checker.ORDINARY),'phase':'check_lost',
                'payload':{'kind':'yield','not_before_ms':6000,'error_code':None,'checkpoint':None}}
            store_state['captured_tokens'][n] = [{'job_id':'raw-job-'+str(n),'node_id':('node-a','node-b')[n],
                'boot_id':'B','claim_id':'C'+str(n),'fence':1,'revision':1,'lease_expires_ms':31000}]
        store = {'states':[store_state],'edges':[]}
        checker.anchor_payload(state,store)
        tree = json.loads(checker.canonical(state))
        wire = product_wire_fixture([tree]);before = checker.canonical(wire)
        pooled = checker.decode_product(wire)['product_states'][0]
        self.assertIsNot(pooled['w'][0],pooled['w'][1])
        self.assertIs(pooled['w'][0]['ordinary'],pooled['w'][1]['ordinary'])
        expected = self.local_successors(tree,driver,store)
        actual = self.local_successors(pooled,driver,store)
        self.assertEqual(self.successor_rows(actual),self.successor_rows(expected))
        advanced = next(row[1] for row in actual if row[0] ==
            {'family':'OrdinaryLocal','worker':0,'choice':{'phase':'check_lost'}})
        self.assertEqual(advanced['w'][0]['ordinary']['phase'],'check_deadline')
        self.assertEqual(advanced['w'][1],tree['w'][1])
        self.assertEqual(pooled['w'][0]['ordinary']['phase'],'check_lost')
        self.assertIsNot(advanced['w'][0]['ordinary']['payload'],advanced['w'][1]['ordinary']['payload'])
        advanced['w'][0]['ordinary']['payload']['not_before_ms'] = 7000
        self.assertEqual(advanced['w'][1]['ordinary']['payload']['not_before_ms'],6000)
        self.assertEqual(pooled['w'][0]['ordinary']['payload']['not_before_ms'],6000)
        self.assertEqual(checker.canonical(wire),before)

    def test_unused_and_certificate_only_floats_fail_global_numeric_preflight(self):
        for value in (1.5,1e100,{'certificate_only':{'nested':[1.5]}},{'certificate_only':[1e100]}):
            wire = product_wire_fixture();wire['source_boundaries'] = copy.deepcopy(checker.SOURCE_BOUNDARIES)
            wire['value_pool'].append(value)
            with self.subTest(value=value),self.assertRaisesRegex(checker.Invalid,'UnsupportedTlcNumericDomain: model input.value_pool'):
                checker.validate_product(wire,fixture(),verify_sources=False)
        ordinary = {'phase':'check_lost','payload':{'kind':'yield','not_before_ms':6000,'error_code':None,'checkpoint':None}}
        self.assertIsNone(checker.certify_tlc_numbers({'ordinary':ordinary,'bounds':[checker.TLC_INT_MIN,checker.TLC_INT_MAX],
            'bools':[False,True],'none':None,'string':'1.5'}))

    def test_finite_floats_in_derived_store_raw_fail_global_numeric_preflight(self):
        for value in (1.5,1e100,{'nested':[1.5]}):
            wire = product_wire_fixture();wire['source_boundaries'] = copy.deepcopy(checker.SOURCE_BOUNDARIES)
            store = wire['store']['states'][0];raw = json.loads(store['canonical_json'])
            raw['rows']['background_jobs'][0][1] = value
            store['canonical_json'] = checker.canonical(raw)
            store['canonical_sha256'] = hashlib.sha256(store['canonical_json'].encode()).hexdigest()
            with self.subTest(value=value),self.assertRaisesRegex(checker.Invalid,'UnsupportedTlcNumericDomain: decoded Store canonical JSON'):
                checker.validate_product(wire,fixture(),verify_sources=False)

    def test_compact_rows_roundtrip_exact_values_hashes_and_shared_workers(self):
        p = product_predecessor('base');q = product_predecessor('stable');q['id'] = 1
        for state in (p,q):
            state['mono_now_ms'] = 29999;state['mono_anchors'] = [29999,30000]
            state['w'][0]['ordinary']['payload'] = {'raw_id':'raw-job-α','truth':True,'integer':1,'none':None,
                'historical_token':{'revision':1,'lease_expires_ms':30000}}
        branch = {'family':'Reply','worker':0,'choice':{'lost':False}}
        edge = dict(zip(('from','to','branch','oracle_edge','resolution_edge','read_edge','cleanup_edge','fair_actions'),
            (0,1,branch,None,None,None,None,['Reply/0'])))
        demand = {'context':'base','from':0,'oracle_from':0,'branch':branch,'role':'apply',
            'input':{'op':'resolve','job':0,'token':0},'oracle_edge':None,'exclusion':None}
        wire = product_wire_fixture([p,q],[edge],[demand],[{'state':0,'actions':['Reply/0']}])
        logical = checker.decode_product(wire)
        self.assertIsInstance(logical['product_states'],checker.LogicalRows)
        for expected,actual in zip((p,q),logical['product_states']):
            self.assertEqual(checker.canonical(checker.product_payload(actual)),checker.canonical(checker.product_payload(expected)))
            self.assertEqual(actual['canonical_sha256'],hashlib.sha256(checker.canonical(checker.product_payload(expected)).encode()).hexdigest())
        self.assertIs(logical['product_states'][0]['w'][0],logical['product_states'][1]['w'][0])
        self.assertEqual(list(logical['product_edges']),[edge]);self.assertEqual(list(logical['demands']),[demand])
        self.assertEqual(list(logical['enabledness']),[{'state':0,'actions':['Reply/0']}])
        with self.assertRaises(IndexError): logical['product_states'][-3]
        self.assertNotIn('raw',wire['store']['states'][0])
        checker.validate_raw_store(logical['store']['states'][0])
        self.assertEqual(logical['store']['states'][0]['canonical_json'],wire['store']['states'][0]['canonical_json'])

    def test_invalid_shapes_references_duplicates_and_legacy_are_refused(self):
        branch = {'family':'Reply','worker':0,'choice':{'lost':False}}
        edge = {'from':0,'to':0,'branch':branch,'oracle_edge':None,'resolution_edge':None,
            'read_edge':None,'cleanup_edge':None,'fair_actions':[]}
        demand = {'context':'base','from':0,'oracle_from':0,'branch':branch,'role':'apply',
            'input':{'op':'resolve','job':0,'token':0},'oracle_edge':None,'exclusion':None}
        baseline = product_wire_fixture(edges=[edge],demands=[demand],enabledness=[{'state':0,'actions':[]}])
        changes = [lambda w:w.update(schema_version=3),lambda w:w['value_pool'].append(copy.deepcopy(w['value_pool'][0])),
            lambda w:w['worker_pool'].append(copy.deepcopy(w['worker_pool'][0])),
            lambda w:w['product_states'][0].pop(),lambda w:w['worker_pool'][0].pop(),
            lambda w:w['product_states'].__setitem__(0,{}),lambda w:w['store']['states'][0].update(raw={})]
        for cell in (False,True,-1,len(baseline['value_pool']),None,'0'):
            changes.append(lambda w,cell=cell:w['product_states'][0].__setitem__(4,cell))
        for cell in (False,-1,len(baseline['worker_pool']),None):
            changes.append(lambda w,cell=cell:w['product_states'][0].__setitem__(2,cell))
        for cell in (False,-1,len(baseline['contexts']),None):
            changes.append(lambda w,cell=cell:w['product_states'][0].__setitem__(0,cell))
        for name,ref_column in (('product_edges',2),('demands',3),('enabledness',1)):
            changes.append(lambda w,name=name:w[name][0].pop())
            changes.append(lambda w,name=name,ref_column=ref_column:w[name][0].__setitem__(ref_column,False))
        changes.append(lambda w:w['worker_pool'][0].__setitem__(checker.WORKER_COLUMNS.index('ordinary'),None))
        changes.append(lambda w:w['store']['states'][0].update(canonical_json='{"rows":{},"rows":{}}'))
        for change in changes:
            wire = copy.deepcopy(baseline);change(wire)
            with self.subTest(change=change),self.assertRaises(checker.Invalid): checker.decode_product(wire)
        # Python bool/int equality must never alias exact dictionary values.
        wire = copy.deepcopy(baseline);wire['value_pool'].extend([True,1,False,0])
        checker.decode_product(wire)
        for text in ('{"a":0,"a":1}','{"n":NaN}','{"n":Infinity}'):
            with self.subTest(text=text),self.assertRaises(checker.Invalid): checker.strict_json(text)

    def test_mutated_shared_values_canonical_bytes_and_hashes_do_not_alias(self):
        wire = product_wire_fixture();logical = checker.decode_product(wire);original = copy.deepcopy(logical['product_states'][0])
        wire['value_pool'][wire['product_states'][0][4]][0] = False
        changed = checker.decode_product(wire)['product_states'][0]
        self.assertNotEqual(changed['canonical_sha256'],hashlib.sha256(checker.canonical(checker.product_payload(changed)).encode()).hexdigest())
        index = checker.ExactPayloadIndex([original])
        index[checker.canonical(checker.product_payload(original))] = 0
        self.assertNotIn(checker.canonical(checker.product_payload(changed)),index)
        store = copy.deepcopy(logical['store']['states'][0]);store['canonical_json'] += ' '
        with self.assertRaisesRegex(checker.Invalid,'preimage hash'): checker.validate_raw_store(store)
        store = copy.deepcopy(logical['store']['states'][0]);store['raw']['episode']['now'] += 1
        with self.assertRaisesRegex(checker.Invalid,'canonical preimage'): checker.validate_raw_store(store)

    def test_derived_store_raw_keeps_global_tlc_numeric_refusal(self):
        wire = product_wire_fixture();wire['source_boundaries'] = copy.deepcopy(checker.SOURCE_BOUNDARIES)
        store = wire['store']['states'][0];raw = json.loads(store['canonical_json'])
        raw['rows']['background_jobs'][0][1] = 2**31
        store['canonical_json'] = checker.canonical(raw)
        store['canonical_sha256'] = hashlib.sha256(store['canonical_json'].encode()).hexdigest()
        with self.assertRaisesRegex(checker.Invalid,'UnsupportedTlcNumericDomain: decoded Store canonical JSON'):
            checker.validate_product(wire,fixture(),verify_sources=False)

    def test_digest_collisions_compare_full_logical_preimages(self):
        p = product_predecessor();q = copy.deepcopy(p);q['available'][0] = False;q['id'] = 1
        encoded = [checker.canonical(checker.product_payload(x)) for x in (p,q)]
        collision = mock.Mock();collision.digest.return_value = b'one bucket'
        with mock.patch.object(checker.hashlib,'sha256',return_value=collision):
            index = checker.ExactPayloadIndex([p,q])
            index[encoded[0]] = 0;index[encoded[1]] = 1
            self.assertEqual(index[encoded[0]],0);self.assertEqual(index[encoded[1]],1)
            missing = copy.deepcopy(p);missing['mono_now_ms'] = 1
            self.assertNotIn(checker.canonical(checker.product_payload(missing)),index)

    def test_compact_full_pair_fairness_stutter_and_missing_candidates_fail_closed(self):
        p = product_predecessor('base');q = copy.deepcopy(p);q['available'][0] = False;q['id'] = 1
        branch = {'family':'ClaimApply','worker':0,'choice':{}}
        candidates = [(branch,checker.product_payload(q),0,{'ClaimApply/0','ClaimWin/0'},None,None,None),
            ({**branch,'family':'WorkSuccess'},checker.product_payload(q),1,{'ExternalReturn/0'},None,None,None),
            ({**branch,'family':'RetireSample'},checker.product_payload(p),None,{'RetireLocal/0'},None,None,None)]
        direct = {checker.canonical(checker.product_payload(x)):x['id'] for x in (p,q)}
        expected = checker.product_pair_obligations(p,candidates,direct)
        wire = product_wire_fixture([p,q],expected[0],[],[expected[1]])
        wire['store']['edges'] = [{},{}]  # Parser-only identities for these two synthetic candidate annotations.
        logical = checker.decode_product(wire);index = checker.ExactPayloadIndex(logical['product_states'])
        for state in logical['product_states']: index[checker.canonical(checker.product_payload(state))] = state['id']
        self.assertEqual(checker.product_pair_obligations(logical['product_states'][0],candidates,index),expected)
        checker.require_exact_inventory(list(logical['product_edges']),expected[0],'pair coverage')
        actions_ref = wire['product_edges'][0][7];wire['value_pool'][actions_ref].remove('ClaimWin/0')
        with self.assertRaisesRegex(checker.Invalid,'complete independent enumeration'):
            checker.require_exact_inventory(list(checker.decode_product(wire)['product_edges']),expected[0],'pair coverage')
        for actual in (expected[0][:-1],expected[0]+[expected[0][0]]):
            with self.assertRaises(checker.Invalid): checker.require_exact_inventory(actual,expected[0],'pair coverage')
        missing = checker.ExactPayloadIndex(logical['product_states']);missing[checker.canonical(checker.product_payload(p))] = 0
        with self.assertRaisesRegex(checker.Invalid,'MissingProductSuccessor'):
            checker.product_pair_obligations(p,candidates,missing)

    def test_streamed_tables_share_exact_key_and_enforce_utf8_byte_boundary(self):
        wire = product_wire_fixture();wire['value_pool'].append({'unused certificate-only key':{}})
        text = checker.tables(fixture(),wire)
        self.assertNotIn('unused certificate-only key',text)
        self.assertEqual(checker.decode_product(wire)['value_pool'][-1],{'unused certificate-only key':{}})
        self.assertIn('GeneratedProductKey(p) == <<p.context,p.sql,p.workers,p.available,p.completed,p.mono_now_ms,p.wall_anchors,p.mono_anchors,p.claim_loss_keys>>',text)
        self.assertIn('GeneratedProductKey(ProductStates[1]) :> 0',text)
        self.assertIn('workers |-> <<GeneratedWorkers[r[3]+1],GeneratedWorkers[r[4]+1]>>',text)
        self.assertNotIn('ProductKey(ProductStates[',text.replace('GeneratedProductKey(ProductStates[',''))
        stream = io.StringIO()
        self.assertEqual(checker.bounded_table_write(stream,iter(['α','x']),3),3)
        self.assertEqual(stream.getvalue(),'αx')
        stream = io.StringIO()
        with self.assertRaisesRegex(checker.Invalid,'SourceTables byte bound'):
            checker.bounded_table_write(stream,iter(['α','x']),2)
        self.assertEqual(stream.getvalue(),'α')
        # Scratch lives inside the workflow-owned test directory established by runner.
        runner.TASK_SCRATCH.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=runner.TASK_SCRATCH) as temp:
            path = Path(temp)/'SourceTables.tla'
            checker.write_tables(path,fixture(),wire,len(text.encode()))
            self.assertEqual(path.read_text(),text)
            with self.assertRaisesRegex(checker.Invalid,'SourceTables byte bound'):
                checker.write_tables(path,fixture(),wire,len(text.encode())-1)
            self.assertFalse(path.with_name(path.name+'.partial').exists())
            self.assertEqual(path.read_text(),text)


class ActualOrdinaryContracts(unittest.TestCase):
    """Pure source-policy/service fixtures only; never actual SQL or qualification evidence."""
    raw_ids = ['00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000002']

    def ordinary(self,phase='check_lost'):
        p = product_predecessor('base');s = product_domain_fixture()
        token = dict(job_id=self.raw_ids[0],node_id='node-a',boot_id='B',claim_id='C',fence=1,
                     revision=1,lease_expires_ms=31000)
        s['captured_tokens'][0] = [token];s['now'] = 1000
        w = p['w'][0];w.update(stage='driver',call='sent',ctrl=0,job=0,token=0,localToken=True,
            attempt='C',started=True,local_deadline_mono_ms=30000,driver_settlement={'kind':'yield','not_before_ms':6000})
        w['ordinary'] = {**copy.deepcopy(checker.ORDINARY),'phase':phase,
            'payload':{'kind':'yield','not_before_ms':6000,'error_code':None,'checkpoint':None}}
        return p,s,{'states':[s],'edges':[]}

    def steps(self,p,s,store,edge=None):
        out = []
        def put(family,worker,choice,change,index=None,resolution=None):
            q = copy.deepcopy(p);change(q);out.append((family,choice,q,index))
        checker.ordinary_successors(p,s,store,0,p['w'][0],put,lambda *_:edge)
        return out

    def test_retry_hash_uses_raw_utf8_and_wrapping_seed(self):
        self.assertEqual(checker.retry_delay_ms('a',0),5097)
        self.assertEqual(checker.retry_delay_ms('a',1),30128)
        self.assertEqual(checker.retry_delay_ms('é',0),5214)
        self.assertEqual(checker.retry_delay_ms('a',-1),600066)

    def test_actual_getter_payload_capture_offsets_and_lifetime(self):
        for pending,tag,delay,deadline in [('AcquireDomain','DomainRefused',0,6000),
          ('ReleaseRefused','ReleaseReturned',0,6000),('ReleaseInterrupted','ReleaseReturned',0,2000),
          ('ReleaseRetry','ReleaseReturned',5097,6097)]:
            with self.subTest(pending=pending):
                self.assertEqual(checker.settlement_after_reply(None,pending,
                    {'tag':tag,'now_ms':1000,'retry_delay_ms':delay}),
                    {'kind':'retry' if pending == 'ReleaseRetry' else 'yield','not_before_ms':deadline})
        previous = {'kind':'yield','not_before_ms':6000}
        self.assertEqual(checker.settlement_after_reply(previous,'SettleYield',{'tag':'Done'}),previous)
        self.assertIsNone(checker.settlement_after_reply(previous,'Bind',{'tag':'BindAccepted'}))
        self.assertEqual(checker.sat_add(checker.I64_MAX-100,5000),checker.I64_MAX)

    def test_guard_reads_are_ordered_and_not_rechecked_after_preemption(self):
        p,s,store = self.ordinary();p['w'][0]['queue_lost'] = False
        q = self.steps(p,s,store)[0][2]
        self.assertEqual(q['w'][0]['ordinary']['phase'],'check_deadline')
        self.assertIs(q['w'][0]['ordinary']['lost_seen'],False)
        q['w'][0]['queue_lost'] = True
        after = self.steps(q,s,store)[0][2]
        self.assertEqual(after['w'][0]['ordinary']['phase'],'take_token')
        self.assertIs(after['w'][0]['ordinary']['lost_seen'],False)
        p,s,store = self.ordinary();p['w'][0]['queue_lost'] = True
        q = self.steps(p,s,store)[0][2]
        self.assertEqual(q['w'][0]['ordinary']['observed_outcome'],'local_lost')
        self.assertEqual(q['sql'],p['sql'])

    def test_expiry_equality_no_token_and_clock_error_are_distinct_no_call_exits(self):
        p,s,store = self.ordinary('check_deadline');p['mono_now_ms'] = 30000
        q = self.steps(p,s,store)[0][2]
        self.assertEqual(q['w'][0]['ordinary']['observed_outcome'],'local_expired')
        self.assertEqual(q['w'][0]['ordinary']['deadline_seen'],30000)
        p,s,store = self.ordinary('take_token');p['w'][0]['localToken'] = False
        q = self.steps(p,s,store)[0][2]
        self.assertEqual(q['w'][0]['ordinary']['observed_outcome'],'local_no_token')
        p,s,store = self.ordinary('sample')
        rows = self.steps(p,s,store)
        failed = next(row[2] for row in rows if row[1]['clock_error'])
        self.assertEqual(failed['w'][0]['ordinary']['observed_outcome'],'clock_error')
        self.assertIsNone(failed['w'][0]['ordinary']['request_now_ms'])
        self.assertTrue(failed['w'][0]['faultUsed'])

    def test_saved_request_survives_later_wall_and_api_validation_error_returns_done(self):
        p,s,store = self.ordinary('sample')
        q = next(row[2] for row in self.steps(p,s,store) if not row[1]['clock_error'])
        self.assertEqual(q['w'][0]['ordinary']['request_now_ms'],1000)
        s['now'] = 9000
        self.assertEqual(checker.ordinary_input(q['w'][0])['request_now_ms'],1000)
        q['w'][0]['ordinary'].update(phase='applied',api_outcome='validation_error_yield_time')
        observed = next(row[2] for row in self.steps(q,s,store) if not row[1]['lost'])
        final = self.steps(observed,s,store)[0][2]
        self.assertEqual(final['w'][0]['reply'],'Done')
        self.assertTrue(final['w'][0]['localToken'])
        self.assertEqual(final['w'][0]['ordinary']['observed_outcome'],'validation_error_yield_time')

    def test_only_observed_accepted_clears_token_and_commit_is_not_rolled_back(self):
        for outcome in ('accepted','refused','validation_error_retry_time','transport_error'):
            p,s,store = self.ordinary('finish');p['w'][0]['ordinary']['observed_outcome'] = outcome
            q = self.steps(p,s,store)[0][2]
            self.assertEqual(q['w'][0]['localToken'],outcome != 'accepted')
            self.assertEqual(q['sql'],p['sql'])
            self.assertEqual(q['w'][0]['ordinary']['phase'],'done')
        p,s,store = self.ordinary('applied');p['w'][0]['ordinary']['api_outcome'] = 'accepted'
        lost = next(row[2] for row in self.steps(p,s,store) if row[1]['lost'])
        self.assertEqual(lost['w'][0]['ordinary']['observed_outcome'],'transport_error')
        self.assertTrue(lost['w'][0]['localToken'])
        self.assertEqual(lost['sql'],p['sql'])

    def test_historical_saved_request_has_contextual_demands_not_latest_filter(self):
        p,s,store = self.ordinary('sent')
        s['captured_tokens'][0].append({**s['captured_tokens'][0][0],'revision':2})
        p['w'][0]['ordinary'].update(captured_token=0,request_now_ms=1000)
        action = checker.ordinary_input(p['w'][0]);driver = {'states':[{'pending':'SettleYield'}]}
        self.assertIsNone(checker.classify_product_input(p,driver,store,action))
        demands = checker.expected_product_demands(p,driver,store)
        ordinary = [d for d in demands if d['input'] and d['input']['op'] == 'ordinary_settle']
        self.assertEqual({d['role'] for d in ordinary},{'apply','capacity'})
        with self.assertRaisesRegex(checker.Invalid,'MissingProductOracleEdge'):
            checker.complete_product_demands(p,driver,store,{})

    def test_exact_clocks_retain_anchors_and_separate_wall_from_mono(self):
        p,s,store = self.ordinary('waiting_lock');p['mono_now_ms'] = 10
        q = checker.anchor_payload(copy.deepcopy(checker.product_payload(p)),store)
        self.assertIn(6000,q['wall_anchors']);self.assertIn(31000,q['wall_anchors'])
        self.assertIn(30000,q['mono_anchors'])
        self.assertEqual(checker.next_landmark(5998,[6000]),5999)
        self.assertEqual(checker.next_landmark(5999,[6000]),6000)
        self.assertEqual(checker.next_landmark(6000,[6000]),6001)
        p['context'] = 'stable';driver = {'states':[{'pending':'SettleYield','held':{'domain':False}}]}
        self.assertTrue(checker.stable_wall_allowed(p,s,6000,driver))
        self.assertFalse(checker.stable_wall_allowed(p,s,6001,driver))
        p['w'][0]['ordinary']['phase'] = 'sent'
        self.assertTrue(checker.stable_wall_allowed(p,s,6001,driver))
        self.assertFalse(checker.stable_mono_allowed(p,30000))

    def test_raw_claim_snapshot_is_bound_before_retry_hash(self):
        p,s,store = self.ordinary()
        snapshot = dict(job=0,raw_job_id=self.raw_ids[0],failed_attempts=1,claim_id='C',
                        token_capture=s['captured_tokens'][0][0])
        checker.validate_claim_snapshot_identity(snapshot,self.raw_ids)
        for key,value in (('raw_job_id','J0'),('claim_id','other'),('failed_attempts',-1)):
            bad = copy.deepcopy(snapshot);bad[key] = value
            with self.subTest(field=key),self.assertRaises(checker.Invalid):
                checker.validate_claim_snapshot_identity(bad,self.raw_ids)
        for field in ('fence','revision','lease_expires_ms'):
            bad = copy.deepcopy(snapshot);bad['token_capture'][field] = 0
            with self.subTest(token_field=field),self.assertRaises(checker.Invalid):
                checker.validate_claim_snapshot_identity(bad,self.raw_ids)

    def test_numeric_incompatibility_is_global_and_full_source_helpers_keep_i64(self):
        checker.certify_tlc_numbers({'values':[0,2**31-1,-(2**31)]})
        for value in (2**31,-(2**31)-1):
            with self.assertRaisesRegex(checker.Invalid,'UnsupportedTlcNumericDomain'):
                checker.certify_tlc_numbers({'nested':[value]})
        self.assertEqual(checker.sat_add(checker.I64_MAX,5000),checker.I64_MAX)
        p,s,store = self.ordinary();p['claim_loss_keys'] = []
        p['w'][0]['adapter_reply'] = {'tag':'DomainRefused','now_ms':2**31-1000,'retry_delay_ms':0}
        # Inputs alone fit, but a disabled/result-filtered +5000 intermediate does not.
        checker.certify_tlc_numbers(p)
        with self.assertRaisesRegex(checker.Invalid,'UnsupportedTlcNumericDomain'):
            checker.certify_tlc_arithmetic(p,store)

    def recovery_fixture(self,result='expired_or_pruned'):
        """Local protocol helper only: no complete graph/SQL/source evidence."""
        p,s,store = self.ordinary();p['context'] = 'base';w = p['w'][0]
        request = dict(job_id=self.raw_ids[0],expected_revision=0,node_id='node-a',boot_id='B',
            claim_id=s['captured_tokens'][0][0]['claim_id'],kind='library_scan',payload_version=1,
            now_ms=1000,dispatched_at_ms=1000)
        w.update(stage='claim',ctrl=-1,call='replied',reply='Unknown',claim_request=request,
            claim_snapshot=None,claim_issued_mono_ms=0,claim_request_now_ms=1000,
            local_deadline_mono_ms=None,localToken=False,faultUsed=True,
            claim_recovery={'phase':'ready','wake_mono_ms':None})
        p['claim_loss_keys'] = [checker.claim_loss_key(w)]
        edge = dict(from_=0,to=0,action=checker.initial_resolution_input(w,s),result=result,
            resolved_token=0 if result in {'running','cancel_requested'} else None)
        edge['from'] = edge.pop('from_');store['edges'] = [edge]
        return p,s,store

    def test_inconclusive_initial_claim_waits_and_timeout_requires_original_deadline(self):
        p,s,store = self.recovery_fixture();oracle={(0,checker.canonical(store['edges'][0]['action'])):0}
        with mock.patch.object(checker,'environment_inputs',return_value=[]):
            rows = checker.expected_product_successors(p,fixture(),store,oracle)
        recovery = [row for row in rows if row[0]['family'] == 'ClaimRecover']
        self.assertEqual([row[0]['choice'] for row in recovery],[{'kind':'resolve'}])
        q = recovery[0][1];self.assertEqual(q['w'][0]['reply'],'Unknown')
        self.assertEqual(q['w'][0]['claim_recovery'],{'phase':'waiting','wake_mono_ms':250})
        self.assertIn(250,q['mono_anchors'])
        self.assertFalse(any(row[0]['family'] == 'ClaimReturn' and row[0]['worker'] == 0 for row in rows))
        p['mono_now_ms'] = 30000
        with mock.patch.object(checker,'environment_inputs',return_value=[]):
            rows = checker.expected_product_successors(p,fixture(),store,{})
        recovery = [row for row in rows if row[0]['family'] == 'ClaimRecover']
        self.assertEqual([row[0]['choice'] for row in recovery],[{'kind':'timeout'}])
        self.assertEqual(recovery[0][1]['w'][0]['reply'],'Refused')
        self.assertEqual(recovery[0][1]['claim_loss_keys'],p['claim_loss_keys'])

    def test_initial_recovery_requires_actual_dependent_read_and_uses_its_snapshot(self):
        p,s,store = self.recovery_fixture('running');primary = checker.expected_product_demands(p,fixture(),store)
        primary = [d for d in primary if d['branch']['family'] == 'ClaimRecover']
        oracle={(0,checker.canonical(store['edges'][0]['action'])):0}
        dependent = checker.dependent_claim_demands(p,store,oracle,primary)
        self.assertEqual(dependent[0]['branch'],{'family':'ClaimRecoverRead','worker':0})
        with mock.patch.object(checker,'environment_inputs',return_value=[]),self.assertRaisesRegex(checker.Invalid,'MissingProductOracleEdge'):
            checker.complete_product_demands(p,fixture(),store,oracle)
        token = s['captured_tokens'][0][0]
        job = dict(id=self.raw_ids[0],token=copy.deepcopy(token),failed_attempts=1,payload_version=1,
            payload={'kind':'library_scan','library_id':1,'generation':'library-work-v1'})
        read = {'from':0,'to':0,'action':{'op':'read_claim_job','job':0},'result':'present','job_read':job}
        store['edges'].append(read);oracle[(0,checker.canonical(read['action']))] = 1
        with mock.patch.object(checker,'environment_inputs',return_value=[]):
            rows = checker.expected_product_successors(p,fixture(),store,oracle)
        recovered = next(row for row in rows if row[0]['family'] == 'ClaimRecover')
        self.assertEqual(recovered[5],1);self.assertIsNone(recovered[4]);self.assertIsNone(recovered[6])
        self.assertEqual(recovered[1]['w'][0]['claim_snapshot']['failed_attempts'],1)
        self.assertEqual(recovered[1]['w'][0]['reply'],'Claimed')
        store['edges'][1]['job_read']['token'] = {**token,'revision':token['revision']+1}
        with mock.patch.object(checker,'environment_inputs',return_value=[]):
            rows = checker.expected_product_successors(p,fixture(),store,oracle)
        self.assertEqual(next(row for row in rows if row[0]['family'] == 'ClaimRecover')[1]['w'][0]['reply'],'Unknown')

    def test_persistent_claim_loss_key_is_not_reset_and_timer_has_separate_membership(self):
        p,s,store = self.recovery_fixture();p['w'][0].update(call='applied',reply='Claimed',faultUsed=False,
            local_deadline_mono_ms=30000,claim_recovery={'phase':'off','wake_mono_ms':None})
        with mock.patch.object(checker,'environment_inputs',return_value=[]):
            rows = checker.expected_product_successors(p,fixture(),store,{})
        replies = [row for row in rows if row[0]['family'] == 'ClaimReply']
        self.assertEqual([row[0]['choice'] for row in replies],[{'lost':False}])
        p['claim_loss_keys'] = []
        with mock.patch.object(checker,'environment_inputs',return_value=[]):
            rows = checker.expected_product_successors(p,fixture(),store,{})
        lost = next(row for row in rows if row[0]['family'] == 'ClaimReply' and row[0]['choice']['lost'])
        self.assertEqual(lost[1]['w'][0]['claim_recovery']['phase'],'ready')
        self.assertEqual(len(lost[1]['claim_loss_keys']),1)
        p = {'id':0,**lost[1]};p['w'][0]['claim_recovery'] = {'phase':'waiting','wake_mono_ms':250}
        p['mono_anchors'] = sorted(set(p['mono_anchors']+[250]))
        # Retained wall anchors also enable an independent wall query: supply
        # that exact helper edge rather than masking it to isolate the timer.
        target = checker.next_landmark(s['now'],p['wall_anchors'])
        after = copy.deepcopy(s);after['now'] = target;store['states'].append(after)
        clock = {'from':0,'to':1,'action':{'op':'clock_advance','now':target},'result':'advanced'}
        store['edges'] = [clock];oracle={(0,checker.canonical(clock['action'])):0}
        with mock.patch.object(checker,'environment_inputs',return_value=[]):
            rows = checker.expected_product_successors(p,fixture(),store,oracle)
        advance = next(row for row in rows if row[0]['family'] == 'MonoAdvance')
        self.assertEqual(advance[0]['choice'],{'now':249})
        self.assertIn('ClaimRecoveryWake/global',advance[3])
        self.assertNotIn('EligibilityAdvance/global',advance[3])

    def test_initial_cancel_recovery_demands_exact_cleanup_before_accepting_none(self):
        p,s,store = self.recovery_fixture('cancel_requested');oracle={(0,checker.canonical(store['edges'][0]['action'])):0}
        with mock.patch.object(checker,'environment_inputs',return_value=[]),self.assertRaisesRegex(checker.Invalid,'MissingProductOracleEdge'):
            checker.complete_product_demands(p,fixture(),store,oracle)
        cleanup = {'from':0,'to':0,'action':{'op':'settle','job':0,'token':0,'disposition':2},'result':'refused'}
        store['edges'].append(cleanup);oracle[(0,checker.canonical(cleanup['action']))] = 1
        with mock.patch.object(checker,'environment_inputs',return_value=[]):
            rows = checker.expected_product_successors(p,fixture(),store,oracle)
        recovered = next(row for row in rows if row[0]['family'] == 'ClaimRecover')
        self.assertEqual(recovered[6],1);self.assertIsNone(recovered[5])
        self.assertEqual(recovered[1]['w'][0]['reply'],'Refused')
        self.assertEqual(recovered[1]['w'][0]['token'],-1)
        self.assertTrue(recovered[1]['w'][0]['faultUsed'])

    def eligibility_fixture(self):
        """Hand-authored witness-helper input; never a complete product/SQL certificate."""
        p,s,_ = self.ordinary('sent')
        p['context'] = 'base';p['w'][0]['ordinary'].update(captured_token=0,request_now_ms=1000)
        p['wall_anchors'] = [1000,6000,31000]
        s['budgets']['claims'] = [1,0]
        states = [copy.deepcopy(p)]; rows = [copy.deepcopy(s)];edges = []
        for index,wall in enumerate((1000,1001,5999,6000),1):
            q = copy.deepcopy(p);q.update(id=index,sql=index)
            q['w'][0]['ordinary'].update(phase='applied',api_outcome='accepted')
            row = copy.deepcopy(s);row['now'] = wall
            row['jobs'][0].update(state='queued',owner=None,expiry=None,not_before_ms=6000)
            states.append(q);rows.append(row)
            edges.append({'from':index-1,'to':index,'oracle_edge':0 if index == 1 else index-1,
                'resolution_edge':None,'read_edge':None,'cleanup_edge':None,'fair_actions':[],
                'branch':{'family':'OrdinaryCallReturn' if index == 1 else 'WallAdvance',
                          'worker':0 if index == 1 else None,
                          'choice':{'kind':'apply'} if index == 1 else {'now':wall}}})
        oracle = [{'from':0,'to':1,'action':checker.ordinary_input(p['w'][0]),'result':'accepted'}]
        oracle += [{'from':i,'to':i+1,'action':{'op':'clock_advance','now':rows[i+1]['now']},'result':'advanced'} for i in range(1,4)]
        bundle = {'product_states':states,'product_edges':edges,'store':{'states':rows,'edges':oracle},
          'contexts':[{'id':'base','initial':0}],'fixture_identity':{'jobs':[{'job':i,'raw_job_id':raw} for i,raw in enumerate(self.raw_ids)]}}
        return bundle,{'states':[{'pending':'SettleYield','held':{'domain':False}}]}

    def test_eligibility_mechanics_requires_real_crossing_safe_windows_and_same_deadline(self):
        bundle,driver = self.eligibility_fixture()
        witness = checker.eligibility_mechanics_witness(bundle,driver)
        self.assertEqual(witness['crossing_edge'],3)
        self.assertEqual(witness['remaining_claim_attempts'],1)
        self.assertEqual(witness['context'],'base')
        for mutation in ('jump','unsafe','rewrite','no_attempt','no_candidate','no_actual_accept'):
            bad = copy.deepcopy(bundle)
            if mutation == 'jump': bad['product_edges'][1]['branch']['choice']['now'] = 6000
            elif mutation == 'unsafe':
                for row in bad['store']['states']: row['captured_tokens'][0][0]['lease_expires_ms'] = 2000
            elif mutation == 'rewrite': bad['store']['states'][-1]['jobs'][0]['not_before_ms'] = 6001
            elif mutation == 'no_attempt':
                for row in bad['store']['states']: row['budgets']['claims'][0] = 2
            elif mutation == 'no_candidate':
                for state in bad['product_states']: state['available'][1] = False
            else: bad['store']['edges'][0]['result'] = 'refused'
            with self.subTest(mutation=mutation),self.assertRaisesRegex(checker.Invalid,'EligibilityMechanicsWitnessMissing'):
                checker.eligibility_mechanics_witness(bad,driver)

    def test_dependent_renew_resolution_requires_its_actual_successor_preimage(self):
        p,s,store = self.ordinary('off');p['w'][0]['ordinary'] = copy.deepcopy(checker.ORDINARY)
        p['w'][0].update(call='ready',faultUsed=False)
        s['budgets'].update(claims=[2,2],domain_acquired=[True]*3,removed=[True]*2)
        newer = copy.deepcopy(s);newer['captured_tokens'][0].append({**s['captured_tokens'][0][0],'revision':2})
        newer['budgets']['renewed_claims'] = ['C']
        renew = {'from':0,'to':1,'action':{'op':'renew','job':0,'token':0},'result':'renewed'}
        resolve = {'from':1,'to':1,'action':{'op':'resolve','job':0,'token':0},'result':'running','resolved_token':1}
        store = {'states':[s,newer],'edges':[renew,resolve]};driver = {'states':[{'pending':'AcquireDomain'}]}
        primary = checker.expected_product_demands(p,driver,store)
        first = {(0,checker.canonical(renew['action'])):0}
        dependent = checker.dependent_resolve_demands(p,driver,store,first,primary)
        self.assertEqual(dependent[0]['oracle_from'],1)
        self.assertEqual(dependent[0]['input']['token'],0)
        with self.assertRaisesRegex(checker.Invalid,'MissingProductOracleEdge'):
            checker.complete_product_demands(p,driver,store,first)
        full = {**first,(1,checker.canonical(resolve['action'])):1}
        demands,used = checker.complete_product_demands(p,driver,store,full)
        self.assertEqual(used,{0,1})
        self.assertTrue(any(d['branch']['family'] == 'EnvironmentResolve' for d in demands))




class RawStoreProjectionContracts(unittest.TestCase):
    def state(self):
        return {'now':10,'jobs':[
          {'state':'running','owner':'node-a','expiry':11,'active':True,'request_done':False,
           'request_state':'pending','reservation_count':3},
          {'state':'queued','owner':None,'expiry':None,'active':False,'request_done':False,
           'request_state':'pending','reservation_count':0}]}

    def test_activity_is_recomputed_at_equality_not_trusted(self):
        state = self.state(); state['jobs'][0]['expiry'] = 10
        with self.assertRaisesRegex(checker.Invalid,'activity projection'):
            checker.validate_job_projection(state)
        state['jobs'][0].update(active=False,reservation_count=1)
        checker.validate_job_projection(state)  # Expired residue is outside full conservation.

    def test_two_unexpired_rows_and_forged_completed_request_are_rejected(self):
        state = self.state(); state['jobs'][1] = {**state['jobs'][0],'owner':'node-b'}
        with self.assertRaisesRegex(checker.Invalid,'NoTwoSourceReaders'):
            checker.validate_job_projection(state)
        for key,value in (('request_state','failed'),('state','queued'),('owner','node-a'),('reservation_count',1)):
            state = self.state()
            state['jobs'][0].update(state='succeeded',owner=None,expiry=None,active=False,
                                   request_done=True,request_state='succeeded',reservation_count=0)
            state['jobs'][0][key] = value
            with self.subTest(field=key),self.assertRaisesRegex(checker.Invalid,'LastRequestAtomicRetirement'):
                checker.validate_job_projection(state)

    def test_resolution_preserves_original_identity_and_durable_projection(self):
        original = dict(job_id='J0',node_id='node-a',boot_id='B',claim_id='C',fence=1,
                        revision=1,lease_expires_ms=11)
        before = {**self.state(),'domain':None,'captured_leases':[],'budgets':{},
                  'captured_tokens':[[original],[]]}
        after = copy.deepcopy(before)
        after['captured_tokens'][0].append({**original,'revision':2,'lease_expires_ms':20})
        edge = {'action':{'op':'resolve','job':0,'token':0},'result':'running','resolved_token':1}
        checker.validate_resolution(edge,before,after)
        for field in ('job_id','node_id','boot_id','claim_id','fence'):
            bad = copy.deepcopy(after); bad['captured_tokens'][0][1][field] = 'other'
            with self.subTest(field=field),self.assertRaisesRegex(checker.Invalid,'immutable attempt identity'):
                checker.validate_resolution(edge,before,bad)
        bad = copy.deepcopy(after); bad['jobs'][0]['reservation_count'] = 0
        with self.assertRaisesRegex(checker.Invalid,'read-only projection'):
            checker.validate_resolution(edge,before,bad)
        with self.assertRaisesRegex(checker.Invalid,'non-running resolution'):
            checker.validate_resolution({**edge,'result':'settled'},before,after)
        with self.assertRaisesRegex(checker.Invalid,'non-resolve'):
            checker.validate_resolution({**edge,'action':{'op':'settle'}},before,after)


class ReceiptContracts(unittest.TestCase):
    def test_positive_requires_complete_nonempty_queue_and_literal_success(self):
        good = 'Model checking completed. No error has been found.\n10 states generated, 4 distinct states found, 0 states left on queue.'
        self.assertTrue(checker.verdict(good,0,None)['complete'])
        for bad in (good.replace('10 states','0 states'),good.replace('0 states left','1 states left'),
                    good.replace('No error has been found.','Unknown result.')):
            with self.subTest(output=bad),self.assertRaises(checker.Invalid): checker.verdict(bad,0,None)

    def test_control_requires_named_semantic_failure_and_trace(self):
        good = ('Invariant ResultAtomicity is violated.\nState 1: <Initial predicate>\n'
                '10 states generated, 4 distinct states found, 2 states left on queue.')
        self.assertEqual(checker.verdict(good,12,'ResultAtomicity')['expected_property'],'ResultAtomicity')
        for bad,code in ((good.replace('ResultAtomicity','WrongProperty'),12),
                         (good.replace('State 1:','No trace:'),12),(good,101),('Parse error\n'+good,12)):
            with self.subTest(output=bad),self.assertRaises(checker.Invalid): checker.verdict(bad,code,'ResultAtomicity')

    def test_temporal_control_has_one_configured_property_and_lasso(self):
        for name,cfg in checker.CONFIGS.items():
            if cfg['expected'] and cfg['properties']:
                self.assertEqual(cfg['properties'],[cfg['expected']],name)
        good = ('Temporal properties were violated.\nState 1: <Initial predicate>\nBack to state 1\n'
                '10 states generated, 4 distinct states found, 0 states left on queue.')
        self.assertTrue(checker.verdict(good,13,'ConditionalAttemptCleanup')['counterexample'])
        with self.assertRaises(checker.Invalid): checker.verdict(good.replace('Back to state','No lasso'),13,'ConditionalAttemptCleanup')

    def test_feature_flag_and_one_pass_do_not_establish_three_voter_sample(self):
        output = 'test result: ok. 1 passed; 0 failed;'
        with self.assertRaisesRegex(runner.c.Invalid,'backend sample'):
            runner.case_evidence('sampled-backends',output,0)

    def test_selector_compile_failure_is_not_a_named_mutation(self):
        with self.assertRaises(runner.c.Invalid):
            runner.case_evidence('control-selector','error: failed to compile SELECTOR_WORK_BEFORE_TICK',101)

    def test_retirement_primitive_markers_are_exact_unique_and_required(self):
        cases = (
            ('store-'+runner.STORE_TESTS['retirement_primitive_test'].rsplit('::',1)[-1],
             runner.STORE_TESTS['retirement_primitive_marker']),
            ('store-'+runner.STORE_TESTS['refusal_guard_test'].rsplit('::',1)[-1],
             runner.STORE_TESTS['refusal_guard_marker']),
        )
        result = 'test result: ok. 1 passed; 0 failed;\n'
        for name,contract in cases:
            line = contract['prefix']+json.dumps(contract['fields'],separators=(',',':'))
            good = result+line+'\n'
            with self.subTest(case=name):
                facts = runner.case_evidence(name,good,0)
                self.assertEqual(facts['named_marker'],contract['fields'])
                malformed = (
                    result,
                    good+line+'\n',
                    result+line.replace(contract['prefix'],'async_store_unknown=')+'\n',
                    result+contract['prefix']+json.dumps({**contract['fields'],'unexpected':True})+'\n',
                )
                for output in malformed:
                    with self.subTest(output=output),self.assertRaises(runner.c.Invalid):
                        runner.case_evidence(name,output,0)

    def test_reader_scope_sentence_is_shared_by_docs_and_receipt_definition(self):
        sentence = runner.CLAIM_BOUNDARIES['NoTwoSourceReaders']
        for name in ('README.md','CONTRACT.md'):
            self.assertIn(sentence,(checker.PACKAGE/name).read_text())
        self.assertIn('structural consistency check',runner.CLAIM_BOUNDARIES['GuardBeforeRevoke'])

    def test_counterexample_kind_and_exit_must_match_registered_expectation(self):
        counts = '10 states generated, 4 distinct states found, 0 states left on queue.'
        temporal = 'Temporal properties were violated.\nState 1: <Initial predicate>\nBack to state 1\n'+counts
        safety = 'Invariant ResultAtomicity is violated.\nState 1: <Initial predicate>\n'+counts
        for output,code,expected in ((temporal,13,'ResultAtomicity'),(safety,12,'ConditionalAttemptCleanup'),
                                     (temporal,12,'ConditionalAttemptCleanup'),(safety,13,'ResultAtomicity')):
            with self.subTest(expected=expected,code=code),self.assertRaises(checker.Invalid):
                checker.verdict(output,code,expected)

    def test_rejection_control_preconditions_fail_by_name(self):
        with self.assertRaisesRegex(checker.Invalid,'requires nonempty source bindings'):
            checker.rejection_controls(fixture(),{'source_bindings':{}})
        with self.assertRaisesRegex(checker.Invalid,'requires a self edge'):
            checker.rejection_controls(fixture(),{'source_bindings':{'source':'0'*64},
              'edges':[{'from':0,'to':1}]})

    def test_retirement_registry_uses_actual_background_jobs_namespace(self):
        names = ('acknowledged_settlement_leaves_retirement_durably_unchanged',
          'retirement_releases_durably_valid_claim_after_local_revocation',
          'retirement_resolves_unobserved_renewal_revision',
          'retirement_resolves_cancellation_revision_after_child_join')
        for name in names:
            command = runner.CASES['daemon-'+name]
            self.assertIn('background_jobs::tests::'+name,command)
            self.assertEqual(command[-3:],['--','--exact','--nocapture'])

    def test_reviewed_configuration_files_match_closed_registry(self):
        for name in checker.CONFIGS:
            self.assertEqual((checker.PACKAGE/f'{name}.cfg').read_text(),checker.config(name))


class SelectorStagingContracts(unittest.TestCase):
    """Staging fixtures inspect source closure and mutation; never invoke Cargo."""
    scratch = ROOT/'target/async-driver-refinement-selector-source-fixture'

    def test_selector_staging_copies_declared_modules_and_only_mutates_one_poll_order(self):
        contract = json.loads((runner.PACKAGE/'selector-control.json').read_text())
        crate = ROOT/'crates/plurx-worker-control'
        source_paths = list((crate/'src').rglob('*.rs'))+[crate/'tests'/f"{contract['test_target']}.rs"]
        before = {path.relative_to(crate).as_posix():path.read_bytes() for path in source_paths}
        library = before['src/lib.rs'].decode()
        required = {'src/lib.rs',f"tests/{contract['test_target']}.rs"}
        required.update('src/'+name+'.rs' for name in re.findall(
            r'^\s*(?:pub\s+)?mod\s+([A-Za-z_]\w*)\s*;',library,re.MULTILINE))
        required.update('src/'+name for name in re.findall(r'include!\(\s*"([^"]+)"\s*\)',library))
        self.assertIn('src/retry_policy.rs',required)
        original = before['src/library_driver.rs'].decode()
        self.assertEqual(hashlib.sha256(before['src/library_driver.rs']).hexdigest(),contract['source_sha256'])
        self.assertEqual(original.count(contract['find']),1)
        self.assertEqual(original.count(contract['replace']),0)
        self.scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='selector-stage-',dir=self.scratch) as directory:
            mutation = runner.stage_selector(Path(directory))
            staged = Path(directory)/'selector-mutant'
            self.assertEqual(set(mutation['original_sha256']),required)
            for relative in required:
                with self.subTest(source=relative):
                    self.assertEqual(mutation['original_sha256'][relative],hashlib.sha256(before[relative]).hexdigest())
                    expected = (original.replace(contract['find'],contract['replace'],1).encode()
                      if relative == 'src/library_driver.rs' else before[relative])
                    self.assertEqual((staged/relative).read_bytes(),expected)
            self.assertEqual(mutation['mutant_sha256'],runner.c.sha(staged/'src/library_driver.rs'))
            self.assertEqual(mutation['manifest_sha256'],runner.c.sha(staged/'Cargo.toml'))
            self.assertEqual(mutation['contract_sha256'],runner.c.sha(runner.PACKAGE/'selector-control.json'))
        self.assertEqual({relative:(crate/relative).read_bytes() for relative in before},before)

    def test_selector_staging_rejects_stale_source_and_nonunique_mutation_anchor(self):
        contract = json.loads((runner.PACKAGE/'selector-control.json').read_text())
        original = (ROOT/contract['source']).read_text()
        self.scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='selector-guard-',dir=self.scratch) as directory:
            root = Path(directory)
            source = root/contract['source']; source.parent.mkdir(parents=True)
            for kind,text in (
                ('stale',original+'\n// source drift\n'),
                ('missing',original.replace(contract['find'],contract['replace'],1)),
                ('duplicate',original+contract['find']),
            ):
                source.write_text(text)
                anchor = dict(contract)
                if kind != 'stale':
                    anchor['source_sha256'] = hashlib.sha256(source.read_bytes()).hexdigest()
                reason = 'reviewed mutation anchor' if kind == 'stale' else 'match exactly once'
                with self.subTest(kind=kind),mock.patch.object(runner,'ROOT',root),mock.patch.object(
                    runner.c,'read_json',return_value=anchor),self.assertRaisesRegex(runner.c.Invalid,reason):
                    runner.stage_selector(root)
                self.assertFalse((root/'selector-mutant').exists())


class CargoPathOverrideIdentityContracts(unittest.TestCase):
    """Synthetic path-override fixtures exercise hashing, never Cargo or TLC."""
    scratch = ROOT/'target/async-driver-refinement-source-fixture'

    def setUp(self):
        shutil.rmtree(self.scratch,ignore_errors=True)
        self.scratch.mkdir(parents=True)
        (self.scratch/'Cargo.toml').write_text(
            '[patch.crates-io]\nfixture = { path = "vendor/fixture" }\n')
        crate = self.scratch/'vendor/fixture'
        (crate/'src').mkdir(parents=True)
        (crate/'Cargo.toml').write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
        (crate/'build.rs').write_text('fn main() {}\n')
        (crate/'src/lib.rs').write_text('pub fn value() -> u8 { 1 }\n')
        (crate/'assets').mkdir()
        (crate/'assets/schema.json').write_text('{"version":1}\n')
        nested = self.scratch/'vendor/transitive'
        (nested/'src').mkdir(parents=True)
        (nested/'Cargo.toml').write_text('[package]\nname="transitive"\nversion="0.1.0"\n')
        (nested/'src/lib.rs').write_text('pub fn nested() -> u8 { 3 }\n')

    def tearDown(self):
        shutil.rmtree(self.scratch,ignore_errors=True)

    def test_override_build_inputs_are_hashed_and_bound_to_every_rust_case(self):
        first = runner.patch_tree_sources(self.scratch)
        self.assertEqual(set(first),{
            'vendor/fixture/Cargo.toml','vendor/fixture/build.rs',
            'vendor/fixture/src/lib.rs','vendor/fixture/assets/schema.json',
            'vendor/transitive/Cargo.toml','vendor/transitive/src/lib.rs'})
        self.assertIn('vendor/fixture/',runner.governed_prefixes(self.scratch))
        self.assertIn('vendor/',runner.governed_prefixes(self.scratch))
        for case in ('graph-store','graph-driver','daemon-ready_work_precedes_burst_pressure_ticks'):
            self.assertEqual(runner.case_sources(case,first,self.scratch),first,case)
        (self.scratch/'vendor/transitive/src/lib.rs').write_text('pub fn nested() -> u8 { 4 }\n')
        second = runner.patch_tree_sources(self.scratch)
        for case in ('graph-store','graph-driver','daemon-ready_work_precedes_burst_pressure_ticks'):
            self.assertNotEqual(first,runner.case_sources(case,second,self.scratch),case)

    def test_missing_or_escaping_patch_directory_is_rejected(self):
        (self.scratch/'Cargo.toml').write_text(
            '[patch.crates-io]\nfixture = { path = "vendor/missing" }\n')
        with self.assertRaises(runner.c.Invalid):
            runner.patch_path_overrides(self.scratch)
        (self.scratch/'Cargo.toml').write_text(
            '[patch.crates-io]\nfixture = { path = "../outside" }\n')
        with self.assertRaises(runner.c.Invalid):
            runner.patch_path_overrides(self.scratch)

if __name__ == '__main__': unittest.main()
