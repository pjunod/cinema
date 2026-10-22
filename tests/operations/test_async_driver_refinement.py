"""Fail-closed importer/receipt contracts. Synthetic fixtures are parser tests,
not source graphs or evidence for the finite protocol theorem. No solver runs.
"""
from __future__ import annotations
import importlib.util
import importlib.machinery
from pathlib import Path
import unittest

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

    def test_reviewed_configuration_files_match_closed_registry(self):
        for name in checker.CONFIGS:
            self.assertEqual((checker.PACKAGE/f'{name}.cfg').read_text(),checker.config(name))

if __name__ == '__main__': unittest.main()
