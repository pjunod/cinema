"""Fail-closed importer/receipt contracts. Synthetic fixtures are parser tests,
not source graphs or evidence for the finite protocol theorem. No solver runs.
"""
from __future__ import annotations
import copy
import importlib.util
import importlib.machinery
import json
import hashlib
from pathlib import Path
import shutil
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
        self.assertIn(checker.canonical({'op':'settle','job':0,'token':1,'disposition':0}),actions)


def product_domain_fixture():
    """Small D input fixture, never an actual SQL result or qualification bundle."""
    return {'now':0,'jobs':[{'state':'queued','owner':None,'expiry':None,'request_done':False} for _ in range(2)],
      'captured_tokens':[[],[]],'captured_leases':[],'domain':None,
      'budgets':{'claims':[0,0],'renewed_claims':[],'domain_acquired':[False]*3,'removed':[False]*2}}


def product_predecessor(context='stable'):
    return {'id':0,'context':context,'sql':0,'w':[dict(checker.IDLE),dict(checker.IDLE)],
            'available':[True,True],'completed':[]}


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
        self.assertEqual(checker.classify_domain(s,{'op':'settle','job':0,'token':0,'disposition':0}),
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
        candidates = [(branch,q,0,{'ClaimApply/0','ClaimWin/0'}),
          ({**branch,'family':'WorkSuccess'},q,1,{'ExternalReturn/0'}),
          ({**branch,'family':'RetireSample'},same,None,{'RetireLocal/0'})]
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
        self.assertTrue({'renew','release_domain','clock_advance','remove_owner','acquire_domain'} <=
                        {a['op'] for a in unmatched})
        p['w'][0].update(stage='driver',job=0,token=0,localToken=True,call='sent')
        self.assertNotIn('renew',{a['op'] for a in checker.environment_inputs(p,s)})
        p['w'][0]['call'] = 'ready'
        self.assertIn('renew',{a['op'] for a in checker.environment_inputs(p,s)})

    def test_coverage_obligations_precede_expected_negative_invariants(self):
        for name,config in checker.CONFIGS.items():
            if config.get('model') == 'BoundaryControls': continue
            with self.subTest(config=name):
                self.assertEqual(config['invariants'][:len(checker.COVERAGE_INVARIANTS)],
                                 list(checker.COVERAGE_INVARIANTS))

    def test_ordinary_settlement_payload_transfer_cannot_be_silently_closed(self):
        expected = {'OrdinarySettlementPayloadTransfer':'unclosed'}
        checker.validate_source_boundaries(expected)
        self.assertIn('source_boundaries',checker.PRODUCT_KEYS)
        for bad in ({}, {'OrdinarySettlementPayloadTransfer':'closed'},
                    {'OrdinarySettlementPayloadTransfer':'assumed'},
                    {**expected,'UnknownBoundary':'unclosed'}, None):
            with self.subTest(boundaries=bad),self.assertRaisesRegex(checker.Invalid,
                    'OrdinarySettlementPayloadTransfer boundary must be exactly unclosed'):
                checker.validate_source_boundaries(bad)
        schema = checker.read_json(checker.PACKAGE/'product-schema.json')
        self.assertEqual(schema['source_boundaries'],expected)
        bad = copy.deepcopy(schema); bad['source_boundaries']['OrdinarySettlementPayloadTransfer'] = 'proved'
        with self.assertRaisesRegex(checker.Invalid,'boundary must be exactly unclosed'):
            checker.validate_product_schema(bad)
        # Boundary rejection precedes even driver/schema/oracle validation.
        for bad in ({}, {'OrdinarySettlementPayloadTransfer':'closed'}):
            bundle = {key:None for key in checker.PRODUCT_KEYS}
            bundle.update(schema_version=2,scope='selected_two_worker_product',source_boundaries=bad)
            with self.assertRaisesRegex(checker.Invalid,'boundary must be exactly unclosed'):
                checker.validate_product(bundle,{},False)
        text = checker.STORE_BOUNDARIES['OrdinarySettlementPayloadTransfer']
        for fragment in ('capture-time deadlines','retry delay','failed-attempt snapshot','error text',
                         'local dispatch/loss/deadline guards','future claim eligibility','no payload noninterference lemma'):
            self.assertIn(fragment,text)

    def test_wire_schema_and_semantic_source_drift_fail_closed(self):
        schema = checker.read_json(checker.PACKAGE/'product-schema.json')
        checker.validate_product_schema(schema)
        for key,value in (('main_contexts',{}),('fair_families',[]),('resource_policy',{}),('store_state_additions',['raw'])):
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
              'domain_acquired':[False]*3,'removed':[False]*2}
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
