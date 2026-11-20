"""Main-lane journals retain their own typed source identity."""
import unittest
import copy
from unittest.mock import Mock, patch
from validation import main_python_continuation as source
from validation import python_unit_receipts as r
class MainPythonContinuationTests(unittest.TestCase):
    def test_unknown_provider_or_wrong_candidate_never_reaches_original_journal(self):
        with self.assertRaises(r.ReceiptError):
            source.prepare(Mock(), {'version': 'native-python-continuation-v1'}, {'schema': 2}, 'a' * 40, 1)
        record = dict(version=source.POLICY, repository=1, pr=42, branch='codex/task',
                      base_sha='b' * 40, source_commit='c' * 40, prior='d' * 40,
                      run=10, journal_artifact=20, changes={}, source_impacts={})
        with self.assertRaises(r.ReceiptError):
            source.prepare(Mock(), record, {'schema': 2}, 'a' * 40, 1)

    def test_missing_or_forged_inherited_origin_never_becomes_a_direct_pass(self):
        scope = {'repository': 1, 'pr': 42, 'branch': 'codex/task', 'base': 'main', 'workflow': 'main-fast-lane.yml'}
        key = 'validation:test_fixture.Case.test_body'
        source_record = {'run': 10, 'commit': 'a' * 40}
        enclosing = {'run': 11, 'commit': 'b' * 40, 'passes': {key: source_record}}
        api = Mock()
        api.pages.return_value = [{'name': source.main.key(scope), 'run_id': 10}]
        with self.assertRaises(r.ReceiptError):
            source.authenticate_origins(api, scope, enclosing, [], 'c' * 40)
        origin = {'run': 10, 'commit': 'a' * 40, 'passes': {key: source_record}}
        with patch.object(source.main, 'journal_from_artifacts', return_value=origin), \
                patch.object(source, 'require_producer'):
            result = source.authenticate_origins(api, scope, enclosing, [{'id': 10, 'commit_sha': 'a' * 40}], 'c' * 40)
            self.assertEqual(result, {10: 'a' * 40, 11: 'b' * 40})
        forged = {'run': 10, 'commit': 'a' * 40, 'passes': {key: {'run': 9, 'commit': 'd' * 40}}}
        with patch.object(source.main, 'journal_from_artifacts', return_value=forged), \
                patch.object(source, 'require_producer'), self.assertRaises(r.ReceiptError):
            source.authenticate_origins(api, scope, enclosing, [{'id': 10, 'commit_sha': 'a' * 40}], 'c' * 40)

    def test_imported_change_before_enclosing_prior_requires_actual_origin_impact(self):
        key = 'validation:test_fixture.Case.test_unchanged_method'
        path = 'validation/imported_implementation.py'
        witness = {path: {'before': source.retention.digest(b'old'), 'after': source.retention.digest(b'new'),
                         'affected': [key], 'reason': 'Imported behavior changed before the enclosing journal'}}
        with patch.object(source.retention, 'changed', return_value={path}), \
                patch.object(source.retention, 'git_bytes', side_effect=lambda rev, _: b'old' if rev == 'a' * 40 else b'new'):
            self.assertEqual(source.origin_impacts('a' * 40, 'c' * 40, witness, {key}), {key})
            with self.assertRaises(r.ReceiptError):
                source.origin_impacts('a' * 40, 'c' * 40, {}, {key})
            wrong = {path: {**witness[path], 'before': 'f' * 64}}
            with self.assertRaises(r.ReceiptError):
                source.origin_impacts('a' * 40, 'c' * 40, wrong, {key})

    def main_supplement(self):
        key='validation:test_fixture.Case.test_body';commit='a'*40
        item={'pr':7,'branch':'codex/main-ancestor','source_commit':commit,'run':11,'journal_artifact':21,'ids':[key]}
        pull={'number':7,'state':'closed','merged':True,'merge_commit_sha':'c'*40,
              'head':{'sha':commit,'ref':item['branch'],'repo':{'id':1}},
              'base':{'ref':'main','repo':{'id':1}}}
        listed={'id':11,'commit_sha':commit,'status':'success','workflow_id':source.main.WORKFLOW,
                'prettyref':'#7','event':'pull_request','event_payload':{'repository':{'id':1},'number':7,'pull_request':copy.deepcopy(pull)}}
        journal={'run':11,'commit':commit,'complete':True,'fixture_errors':[],
                 'passes':{key:{'run':11,'commit':commit}}}
        api=Mock();api.get.return_value=pull
        api.pages.side_effect=[[listed],[{'id':21,'run_id':11}]]
        return api,item,journal,key

    def read_main_supplement(self,api,item,journal,ancestor=True,job_status='success'):
        with patch.object(source,'main_ancestor',return_value=ancestor), \
             patch.object(source,'require_producer'), \
             patch.object(source.main,'preflight_job',return_value={'status':job_status}), \
             patch.object(source.main,'journal_from_artifacts',return_value=journal):
            return source.authenticate_main_journals(api,[item],{'base_sha':'b'*40},'d'*40,1)

    def test_merged_main_scoped_journal_preserves_actual_direct_source_without_relabeling(self):
        api,item,journal,key=self.main_supplement()
        records,origins=self.read_main_supplement(api,item,journal)
        self.assertEqual(records,[{key:{'run':11,'commit':'a'*40}}])
        self.assertEqual(origins,{11:'a'*40})
        self.assertEqual(journal['passes'][key],records[0][key])

    def test_main_supplement_refuses_unmerged_foreign_or_nonancestor_sources(self):
        for kind in ('unmerged','foreign','nonancestor','failed-job'):
            api,item,journal,_=self.main_supplement()
            if kind=='unmerged':api.get.return_value['merged']=False
            if kind=='foreign':api.get.return_value['head']['repo']['id']=2
            with self.subTest(kind=kind),self.assertRaises(r.ReceiptError):
                self.read_main_supplement(api,item,journal,ancestor=kind!='nonancestor',
                                          job_status='failure' if kind=='failed-job' else 'success')

    def test_main_supplement_refuses_partial_inherited_unknown_or_duplicate_selected_rows(self):
        for kind in ('partial','inherited','unknown','duplicate','forged-commit'):
            api,item,journal,key=self.main_supplement()
            if kind=='partial':journal['complete']=False
            elif kind=='inherited':journal['passes'][key]={'run':10,'commit':'c'*40}
            elif kind=='unknown':item['ids']=['validation:unknown.Case.test_absent']
            elif kind=='duplicate':item['ids']=[key,key]
            else:journal['commit']='f'*40
            with self.subTest(kind=kind),self.assertRaises(r.ReceiptError):
                self.read_main_supplement(api,item,journal)

    def test_main_supplement_requires_the_real_listed_run_and_original_artifact(self):
        for kind in ('status','commit','artifact','event'):
            api,item,journal,_=self.main_supplement()
            rows,artifacts=api.pages.side_effect
            if kind=='status':rows[0]['status']='failure'
            elif kind=='commit':rows[0]['commit_sha']='f'*40
            elif kind=='artifact':artifacts[0]['run_id']=99
            else:rows[0]['event']='workflow_dispatch'
            api.pages.side_effect=[rows,artifacts]
            with self.subTest(kind=kind),self.assertRaises(r.ReceiptError):
                self.read_main_supplement(api,item,journal)

    def test_closed_pr_surrogate_preserves_original_event_branch_and_refuses_other_refs(self):
        for ref,valid in [('refs/pull/7/head',True),('refs/pull/8/head',False),('codex/foreign',False)]:
            api,item,journal,key=self.main_supplement()
            api.get.return_value['head']['ref']=ref
            if valid:
                records,_=self.read_main_supplement(api,item,journal)
                self.assertEqual(records[0][key],{'run':11,'commit':'a'*40})
                self.assertEqual(item['branch'],'codex/main-ancestor')
            else:
                with self.subTest(ref=ref),self.assertRaises(r.ReceiptError):
                    self.read_main_supplement(api,item,journal)
