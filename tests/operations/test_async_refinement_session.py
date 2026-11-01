"""Closed admission/progress/Main authority fixtures. No remote producer runs."""
from __future__ import annotations
import copy
import os
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from validation import async_refinement_session as s
from validation import async_refinement_provenance as p
from validation import python_unit_receipts as receipts

ROOT = Path(__file__).resolve().parents[2]


class MainCompletionTests(unittest.TestCase):
    def results(self):return {key:'success' for key in s.MAIN_ENV}

    def test_incomplete_async_never_satisfies_affected_main(self):
        for state in ('skipped','failure','cancelled','pending','checkpointed_incomplete'):
            rows = self.results(); rows['async_refinement'] = state
            with self.subTest(state=state),self.assertRaises(receipts.ReceiptError):s.main_conditions(rows,True)
        self.assertTrue(s.main_conditions(self.results(),True))

    def test_unaffected_skip_never_waives_policy_or_worker(self):
        rows = self.results(); rows['async_refinement']='skipped'
        self.assertTrue(s.main_conditions(rows,False))
        for key in ('paperwork','worker_control_proof'):
            bad = dict(rows); bad[key]='skipped'
            with self.assertRaises(receipts.ReceiptError):s.main_conditions(bad,False)

    def test_main_applicability_comes_from_exact_scope(self):
        rows = s.main_required({'rust':False,'apple':False,'desktop_macos':True,
                               'android_jvm':False,'web_layout':True})
        self.assertTrue(rows['apple_compile']);self.assertTrue(rows['web_compile'])
        self.assertFalse(rows['rust_compile']);self.assertFalse(rows['android_compile'])
        self.assertTrue(rows['preflight']);self.assertTrue(rows['worker_control_proof'])

    def test_completion_cannot_relabel_an_ordinary_pull_request_job(self):
        with patch.dict(os.environ,{'GITHUB_EVENT_NAME':'workflow_dispatch'},clear=True):
            with self.assertRaises(receipts.ReceiptError):s.ordinary_main()
        rows = self.results();rows['new_waiver']='success'
        with self.assertRaises(receipts.ReceiptError):s.main_conditions(rows,True)

    def test_source_archive_child_never_inherits_api_or_event_credentials(self):
        import tempfile
        scratch=ROOT/'.agent-proof-fixtures/session';scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            with patch.dict(os.environ,{'GITHUB_TOKEN':'private','ACTIONS_RUNTIME_TOKEN':'private',
                                        'GITHUB_EVENT_PATH':'private','CARGO_TARGET_DIR':str(ROOT/'target/async-fixture-compiler')},clear=False):
                grant=SimpleNamespace(environment=lambda:dict(zip(s.GRANT_ENV,('7','1'*64,'8'))))
                env=s.child_environment({},Path(directory)/'request.json',ROOT,directory,grant)
            self.assertFalse({'GITHUB_TOKEN','ACTIONS_RUNTIME_TOKEN','GITHUB_EVENT_PATH'} & set(env))
            self.assertEqual({key:env[key] for key in s.GRANT_ENV},dict(zip(s.GRANT_ENV,('7','1'*64,'8'))))
            self.assertEqual(env['CARGO_NET_OFFLINE'],'true')
            self.assertEqual(env['PLURX_ASYNC_PAGED_REQUEST'],str(Path(directory)/'request.json'))

    def test_complete_root_requires_qualified_session_and_all_source_outcomes(self):
        with self.assertRaises(receipts.ReceiptError):s.completion({'stage':'expand','phase':'running'}, {}, [], {}, {}, ROOT)

    def test_artifact_identity_rejects_path_alias_and_slot_overrun(self):
        session='00000000-0000-4000-8000-000000000001'
        self.assertEqual(s.artifact_name(session,2,3),'async-proof-'+session+'-2-part-3')
        for part in (-1,p.UPLOAD_SLOTS):
            with self.assertRaises(receipts.ReceiptError):s.artifact_name(session,2,part)


if __name__=='__main__':unittest.main()

class CapabilityAndStorageTests(unittest.TestCase):
    def test_capability_probe_never_substitutes_for_executed_control(self):
        value={'registration_sha256':'a'*64,'source_archive_sha256':'b'*64,'run':1,'job':2,'task':3,
               'attempt':1,'case':'success','nonce':'c'*64,'uploads':[]}
        import base64
        raw=p.canonical(value);sha=p.digest(raw);encoded=base64.b64encode(raw).decode('ascii')
        chunks=[encoded[i:i+2048] for i in range(0,len(encoded),2048)]
        lines=[f'ASYNC-PROOF-CAPABILITY-BEGIN 1 success '+('c'*64)+f' {len(raw)} {len(chunks)} {sha}']
        lines += [f'ASYNC-PROOF-CAPABILITY-CHUNK {i} {chunk}' for i,chunk in enumerate(chunks)]
        lines += ['ASYNC-PROOF-CAPABILITY-END '+sha,'ASYNC-PROOF-CAPABILITY-DONE '+sha]
        self.assertEqual(s.capability_frame(lines)[0],value)
        with self.assertRaises(receipts.ReceiptError):p.parse_control_frame(lines)
        with self.assertRaises(receipts.ReceiptError):s.capability_frame(lines[:-1])
        with self.assertRaises(receipts.ReceiptError):s.capability_frame(lines+lines)

    def test_negative_capability_requires_exact_post_done_failure_marker(self):
        import base64
        value={'case':'failure','nonce':'d'*64}
        raw=p.canonical(value);sha=p.digest(raw);encoded=base64.b64encode(raw).decode('ascii')
        lines=['ASYNC-PROOF-CAPABILITY-BEGIN 1 failure '+('d'*64)+f' {len(raw)} 1 {sha}',
               'ASYNC-PROOF-CAPABILITY-CHUNK 0 '+encoded,'ASYNC-PROOF-CAPABILITY-END '+sha,
               'ASYNC-PROOF-CAPABILITY-DONE '+sha]
        with self.assertRaises(receipts.ReceiptError):s.capability_frame(lines)
        marker='ASYNC-PROOF-CAPABILITY-EXPECTED-FAILURE 1 '+('d'*64)+' '+sha
        self.assertEqual(s.capability_frame(lines+[marker])[0],value)
        with self.assertRaises(receipts.ReceiptError):s.capability_frame(lines+[marker,marker])

    def test_isolated_post_failure_requires_bound_marker_after_successful_done(self):
        import base64
        value={'case':'post_failure','nonce':'e'*64}
        raw=p.canonical(value);sha=p.digest(raw);encoded=base64.b64encode(raw).decode('ascii')
        lines=['ASYNC-PROOF-CAPABILITY-BEGIN 1 post_failure '+('e'*64)+f' {len(raw)} 1 {sha}',
               'ASYNC-PROOF-CAPABILITY-CHUNK 0 '+encoded,'ASYNC-PROOF-CAPABILITY-END '+sha,
               'ASYNC-PROOF-CAPABILITY-DONE '+sha]
        marker='ASYNC-PROOF-CAPABILITY-EXPECTED-POST-FAILURE 1 '+('e'*64)+' '+sha
        for candidate in (lines,lines+[marker.replace(sha,'f'*64)],lines+[marker,marker]):
            with self.assertRaises(receipts.ReceiptError):s.capability_frame(candidate)
        self.assertEqual(s.capability_frame(lines+['ordinary runner post cleanup',marker])[0],value)

    def attempted_controls(self):
        import json
        reg={'source':{'commit':'a'*40}}
        base={'schema_version':1,'registration':reg,'resource_plan':{'immutable':'plan'},
              'probe_run':{},'unit_runs':[],'attempt_runs':[],'resume_decisions':[],'uncertain_dispatches':[],
              'main_run':None,'session_id':'00000000-0000-4000-8000-000000000001','transport_conformance':None}
        self.assertEqual(set(base),set(s.CONTROL_KEYS))  # a closed control: every key the controller replays, no more
        rows=[]
        for ordinal,run_id in enumerate((8,9,10)):
            prior=copy.deepcopy(base);prior['attempt_runs']=[8,9,10][:ordinal]
            prior['unit_runs']=[8] if ordinal else []
            rows.append({'id':run_id,'workflow_id':p.WORKFLOW,'event':'workflow_dispatch',
                'event_payload':json.dumps({'inputs':{'operation':'execute','mode':'unit',
                                                     'control':json.dumps(prior)}})})
        base['attempt_runs']=[8,9,10];base['unit_runs']=[8,10]
        class API:
            def pages(self,*args,**kwargs):return rows
        def producer(api,run_id,*args):
            return {'id':run_id},{'id':run_id+100,'task_id':run_id+200,'status':'failure' if run_id==9 else 'success'},{}
        return API(),reg,base,rows,producer

    def test_failed_attempt_is_retained_between_successful_ordinals(self):
        api,reg,control,rows,producer=self.attempted_controls()
        with patch.object(p,'producer',producer):
            actual=s.attempt_inventory(api,reg,control)
        self.assertEqual([r['run']['id'] for r in actual],[8,9,10])
        self.assertEqual(actual[1]['job']['status'],'failure')

    def test_unexplained_gap_or_hidden_failed_attempt_refuses(self):
        api,reg,control,rows,producer=self.attempted_controls()
        with patch.object(p,'producer',producer):
            omitted=copy.deepcopy(control);omitted['attempt_runs']=[8,10]
            with self.assertRaises(receipts.ReceiptError):s.attempt_inventory(api,reg,omitted)
            rows.pop(1)
            with self.assertRaises(receipts.ReceiptError):s.attempt_inventory(api,reg,control)

    def test_failed_before_upload_ordinal_is_not_silently_reseeded(self):
        # Even a concrete infrastructure diagnosis cannot invent a missing Init.
        with self.assertRaises(receipts.ReceiptError):
            s.validate_resume_decision(None,{}, {}, {'initial_image':None}, {})

    def test_completion_stage_allows_readonly_restore_but_never_catalog_publication(self):
        import sys
        request=ROOT/'owned-request.json'
        runner=str(ROOT/'scripts/check-async-driver-refinement')
        checker=str(ROOT/'verification/async-driver-refinement/paged_checker.py')
        s.validate_completion_command('validate_restore',
            [sys.executable,runner,'paged-unit','--request',str(request)],ROOT,request,'unit')
        s.validate_completion_command('unit',
            [sys.executable,checker,'prepare-unit','--request',str(request)],ROOT,request,'unit')
        for operation in ('expand','temporal_import','completion_verify'):
            with self.assertRaises(receipts.ReceiptError):
                s.validate_completion_command(operation,
                    [sys.executable,runner,'paged-unit','--request',str(request)],ROOT,request,'unit')

    def test_final_snapshot_preparation_routes_unit_until_exact_authenticated_certificate(self):
        checkpoint={'record':{'page_root':'a'*64,'committed_transaction':9,'counts':{'products':2},'contexts':['all seven']}}
        state={'stage':'complete','checkpoint':checkpoint,'semantic_ready':True,
               'restored-input-readiness.json':{'semantic_ready':True}}
        self.assertEqual(s.dispatch_mode(state),'unit')
        state['final_input_checkpoint_sha256']=p.digest(p.canonical(checkpoint))
        self.assertEqual(s.dispatch_mode(state),'complete')
        # A new final publication invalidates a previously authenticated scope.
        state['checkpoint']=copy.deepcopy(checkpoint)
        state['checkpoint']['record']['committed_transaction']+=1
        self.assertEqual(s.dispatch_mode(state),'unit')

    def test_final_readiness_receipt_rejects_partial_stale_and_forged_ready_forms(self):
        cp={'source':{'commit':'a'*40},'session_id':'owned-session','committed_transaction':9,
            'catalog_logical_digest':'b'*64,'page_root':'c'*64,'counts':{'products':2},'contexts':['seven']}
        checkpoint={'record':cp};catalog='d'*64
        ready={'schema_version':6,'source':cp['source'],'session_id':cp['session_id'],
            'checkpoint_sha256':p.digest(p.canonical(checkpoint)),'catalog_commit':9,
            'catalog_logical_digest':cp['catalog_logical_digest'],'manifest_root':cp['page_root'],
            'catalog_sha256':catalog,'counts':cp['counts'],'contexts':cp['contexts'],
            'complete':True,'acceptance':False,'scope':s.FINAL_INPUT_SCOPE}
        self.assertTrue(s.final_input_receipt(ready,checkpoint,catalog))
        self.assertFalse(s.final_input_receipt({**ready,'complete':False,'scope':'input_preparation'},checkpoint,catalog))
        for changes in ({'checkpoint_sha256':'f'*64},{'catalog_sha256':'f'*64},
                        {'counts':{'products':3}},{'complete':1},{'scope':'input_preparation'},
                        {'caller_ready':True}):
            with self.subTest(changes=changes),self.assertRaises(receipts.ReceiptError):
                s.final_input_receipt({**ready,**changes},checkpoint,catalog)

    def test_actual_complete_command_has_no_preparation_fallback(self):
        import sys
        request=ROOT/'owned-request.json'
        checker=str(ROOT/'verification/async-driver-refinement/paged_checker.py')
        s.validate_completion_command('completion_verify',
            [sys.executable,checker,'complete','--request',str(request)],ROOT,request,'complete')
        with self.assertRaises(receipts.ReceiptError):
            s.validate_completion_command('unit',
                [sys.executable,checker,'prepare-unit','--request',str(request)],ROOT,request,'complete')
        with self.assertRaises(receipts.ReceiptError):
            s.validate_completion_command('completion_verify',
                [sys.executable,checker,'complete','--request',str(request)],ROOT,request,'unit')

    def test_missing_actual_persistent_mount_or_lease_refuses(self):
        with self.assertRaises(receipts.ReceiptError):s.validate_storage({},ROOT)

    def test_registered_workflow_requires_all_slots_and_last_finalizer(self):
        # Static source predicate only; this does not establish service capability.
        s.validate_workflow_protocol(ROOT)
        text=(ROOT/'.github/workflows/async-proof-session.yml').read_text()
        self.assertEqual(text.count('      - name: Finalize authenticated post-upload control'),2)

    def native_consumer_fixture(self):
        from types import SimpleNamespace
        reg={'repository':1,'pr':7,'event_base':'b'*40,'source':{'commit':'a'*40}}
        api=SimpleNamespace(scratch=ROOT/'.agent-proof-fixtures/native-main')
        artifacts=[{'name':'native-adoption-10-1','id':20,'run_id':10,'expired':False}]
        report={'version':1,'scope':{'repository':1,'pr':7},'current_commit':'a'*40,
                'current_run':10,'adoptions':{},'compatibility':'pinned native source contract',
                'tv_cases': {'required_tests': [], 'passes': {}, 'pending': []}}
        return api,reg,artifacts,report,{'id':30,'task_id':40,'attempt':1,'run_id':10,'repo_id':1,'status':'success'}

    def test_desktop_main_consumption_uses_only_shared_strict_current_validator(self):
        from validation import native_receipts as native
        from validation import desktop_macos_retention as desktop
        api,reg,artifacts,report,job=self.native_consumer_fixture()
        selected={'apple':False,'desktop_macos':True}
        evidence={'current':{'run':10,'receipt':{'current_native_commands_executed':False}},
                  'execution_origin':{'run':1,'commit':'original'}}
        with patch.object(s,'authenticated_main_artifact',return_value=[{'path':'receipt.json'}]), \
             patch.object(p,'read',return_value=report), \
             patch.object(desktop,'authenticate_current_result',return_value=evidence) as strict, \
             patch.object(native,'verify_desktop') as generic:
            value=s.authenticate_native(api,reg,10,artifacts,[],selected,ROOT,job)
            self.assertEqual(value['desktop_macos'],evidence)
            strict.assert_called_once_with(api,ROOT,'a'*40,10,30,1,7,event_base='b'*40)
            generic.assert_not_called()
        with patch.object(s,'authenticated_main_artifact',return_value=[{'path':'receipt.json'}]), \
             patch.object(p,'read',return_value=report), \
             patch.object(desktop,'authenticate_current_result',side_effect=receipts.ReceiptError('strict artifact absent')), \
             patch.object(native,'verify_desktop') as generic:
            with self.assertRaises(receipts.ReceiptError):
                s.authenticate_native(api,reg,10,artifacts,[],selected,ROOT,job)
            generic.assert_not_called()

    def test_generic_desktop_report_flag_never_suppresses_strict_adapter(self):
        from validation import desktop_macos_retention as desktop
        api,reg,artifacts,report,job=self.native_consumer_fixture()
        report['adoptions']={'desktop_macos':{'disposition':'adopted'}}
        with patch.object(s,'authenticated_main_artifact',return_value=[{'path':'receipt.json'}]), \
             patch.object(p,'read',return_value=report),patch.object(desktop,'authenticate_current_result') as strict:
            with self.assertRaises(receipts.ReceiptError):
                s.authenticate_native(api,reg,10,artifacts,[],{'apple':False,'desktop_macos':True},ROOT,job)
            strict.assert_not_called()

    def test_current_tv_requires_full_upload_transaction_and_exact_archive(self):
        import tempfile
        from validation import native_receipts as native
        api,reg,artifacts,report,job=self.native_consumer_fixture()
        artifact={'name':'tv-navigation-10-1','id':21,'run_id':10,'expired':False,'size_in_bytes':7}
        artifacts.append(artifact)
        scratch=ROOT/'.agent-proof-fixtures/native-main';scratch.mkdir(parents=True,exist_ok=True)
        lines=['Artifact tv-navigation-10-1 has been successfully uploaded! Final size is 7 bytes. Artifact ID is 21']
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            archive=Path(directory)/'actual.zip';archive.write_bytes(b'archive');sha=p.file_sha(archive)
            with patch.object(s,'authenticated_main_artifact',return_value=[{'path':'receipt.json'}]), \
                 patch.object(p,'read',return_value=report),patch.object(p,'artifact_bytes',return_value=archive), \
                 patch.object(p,'upload_slice',return_value=('transaction',sha,0,1)) as transaction, \
                 patch.object(native,'current_tv_result',return_value={'disposition':'composed_cases','original_cases':{'Suite/test':{}},'receipt_sha256':'b'*64}) as verify:
                value=s.authenticate_native(api,reg,10,artifacts,lines,{'apple':True,'desktop_macos':False},ROOT,job)
                transaction.assert_called_once_with(lines,artifact['name'],21,7)
                self.assertEqual(value['tv_navigation']['archive_sha256'],sha)
                self.assertEqual(verify.call_args.args[:5], (api, {'repository':1,'pr':7}, 'a'*40, 10, report))
                self.assertEqual(value['tv_navigation']['disposition'], 'composed_cases')
                self.assertFalse(archive.exists())
        with patch.object(s,'authenticated_main_artifact',return_value=[{'path':'receipt.json'}]), \
             patch.object(p,'read',return_value=report), \
             patch.object(p,'upload_slice',side_effect=receipts.ReceiptError('transaction absent')), \
             patch.object(native,'current_tv_result') as verify:
            with self.assertRaises(receipts.ReceiptError):
                s.authenticate_native(api,reg,10,artifacts,lines,{'apple':True,'desktop_macos':False},ROOT,job)
            verify.assert_not_called()

    def test_current_native_report_requires_exact_member_and_successful_job(self):
        from validation import native_receipts as native
        api, reg, artifacts, report, job = self.native_consumer_fixture()
        with patch.object(s, 'authenticated_main_artifact', return_value=[{'path': 'other.json'}]), \
                patch.object(p, 'read', return_value=report), patch.object(native, 'current_tv_result') as verify:
            with self.assertRaises(receipts.ReceiptError):
                s.authenticate_native(api, reg, 10, artifacts, [], {'apple': True, 'desktop_macos': False}, ROOT, job)
            verify.assert_not_called()
        with patch.object(s, 'authenticated_main_artifact') as artifact:
            with self.assertRaises(receipts.ReceiptError):
                s.authenticate_native(api, reg, 10, artifacts, [], {'apple': True, 'desktop_macos': False}, ROOT, {**job, 'status': 'failure'})
            artifact.assert_not_called()

if __name__ == '__main__':unittest.main()


class DedicatedRouteTests(unittest.TestCase):
    def registration(self):
        route=s.route_contract(ROOT)
        return {'route':{'logical_slot':route['logical_slot'],'contract_sha256':p.file_sha(ROOT/s.ROUTE_FILE),
                    'runner_id':19,'runner_uuid':'00000000-0000-4000-8000-000000000019'},
                'runner':{'name':'actual-private-fixture-slot','os':'Linux','arch':'X64'},
                'storage':{'mount_root':route['mount_root']},
                'source_files':{s.ROUTE_FILE:p.file_sha(ROOT/s.ROUTE_FILE)}}

    def runner(self,**changes):
        reg=self.registration()
        return {'id':reg['route']['runner_id'],'uuid':reg['route']['runner_uuid'],
            'name':reg['runner']['name'],'labels':s.ROUTE_LABELS.copy(),
            'status':'idle','ephemeral':False,**changes}

    def inventory_api(self,pages,totals=None):
        from types import SimpleNamespace
        api=SimpleNamespace(calls=[],response_headers={})
        def get(path,query):
            self.assertEqual(path,'/actions/runners');self.assertEqual(query['visible'],'true')
            self.assertEqual(query['limit'],50);api.calls.append(query['page'])
            api.response_headers={'x-total-count':str((totals or [sum(map(len,pages))]*len(pages))[query['page']-1])}
            return pages[query['page']-1]
        api.get=get;return api

    def test_both_producer_modes_pin_the_same_source_route_and_container_bind(self):
        route=s.route_contract(ROOT)
        self.assertIn(route['state'],('provisioning_required','active'))
        self.assertNotEqual(self.registration()['runner']['name'],route['logical_slot'])
        s.validate_workflow_protocol(ROOT)
        text=(ROOT/'.github/workflows/async-proof-session.yml').read_text()
        self.assertEqual(text.count('    runs-on: ['+', '.join(s.ROUTE_LABELS)+']'),2)
        self.assertEqual(text.count('        - '+route['container_bind']),2)
        self.assertNotIn('${{ github.event.inputs.runner',text)
        for changed in (text.replace('async-proof-owned','general'),
                        text.replace('/srv/plurx-async-proof:/srv/plurx-async-proof','/wrong:/srv/plurx-async-proof'),
                        text.replace('    timeout-minutes: 30','        - /unrelated:/extra\n    timeout-minutes: 30')):
            with patch.object(s.Path,'read_text',return_value=changed), \
                 patch.object(s,'route_contract',return_value=route),self.assertRaises(receipts.ReceiptError):
                s.validate_workflow_protocol(ROOT)

    def test_registration_rejects_substituted_route_mount_and_identity(self):
        reg=self.registration();s.registration_route(reg,ROOT)
        for section,key,value in (('route','logical_slot','another-slot'),('route','runner_id',0),
                ('route','contract_sha256','f'*64),('route','runner_uuid','unobserved'),
                ('runner','arch','ARM64'),('storage','mount_root','/unprovisioned')):
            bad=copy.deepcopy(reg);bad[section][key]=value
            with self.subTest(key=key),self.assertRaises(receipts.ReceiptError):s.registration_route(bad,ROOT)

    def authenticate_active(self,api,reg):
        route={**s.route_contract(ROOT),'state':'active'}
        with patch.object(s,'route_contract',return_value=route):
            return s.authenticate_runner_route(api,reg,ROOT)

    def test_planned_route_refuses_dispatch_authority_without_source_activation(self):
        from unittest.mock import Mock
        route={**s.route_contract(ROOT),'state':'provisioning_required'}
        api=Mock()
        with patch.object(s,'route_contract',return_value=route),self.assertRaises(receipts.ReceiptError):
            s.authenticate_runner_route(api,self.registration(),ROOT)
        api.get.assert_not_called()

    def test_visible_route_is_unique_across_pages_including_offline_carriers(self):
        reg=self.registration();owner=self.runner()
        other=self.runner(id=20,uuid='00000000-0000-4000-8000-000000000020',
            labels=['self-hosted','Linux','X64','lab','general','high-cpu'],status='offline')
        api=self.inventory_api([[other],[owner]])
        self.assertEqual(self.authenticate_active(api,reg),owner)
        self.assertEqual(api.calls,[1,2])
        for rows in ([owner,{**other,'labels':s.ROUTE_LABELS}], [owner,owner],
                     [{**owner,'status':'offline'}],[{**owner,'ephemeral':True}],
                     [{**owner,'name':'substituted'}],[{**owner,'id':22}],
                     [{**owner,'uuid':other['uuid']}],[],[{**owner,'labels':['async-proof-owned']} ]):
            with self.subTest(rows=rows),self.assertRaises(receipts.ReceiptError):
                self.authenticate_active(self.inventory_api([rows]),reg)

    def test_visible_inventory_rejects_missing_count_truncation_and_changed_total(self):
        for pages,totals in (([[self.runner()],[]],[2,2]),([[self.runner()],[self.runner()]],[2,3])):
            with self.subTest(totals=totals),self.assertRaises(receipts.ReceiptError):
                s.visible_runner_inventory(self.inventory_api(pages,totals))
        from types import SimpleNamespace
        with self.assertRaises(receipts.ReceiptError):
            s.visible_runner_inventory(SimpleNamespace(get=lambda *args:[],response_headers={}))

    def test_every_dispatch_refuses_unavailable_live_route_before_post(self):
        from types import SimpleNamespace
        from unittest.mock import Mock
        import tempfile
        scratch=ROOT/'.agent-proof-fixtures/route';scratch.mkdir(parents=True,exist_ok=True)
        reg={**self.registration(),'ref':'candidate'}
        api=SimpleNamespace(root='https://forge.invalid/api/v1/repos/example/repo',token='fixture',opener=Mock())
        with tempfile.TemporaryDirectory(dir=scratch) as directory, \
             patch.object(s,'registration',return_value=reg),patch.object(s,'live_pr'), \
             patch.object(s,'reject_prior_ordinal_dispatch'), \
             patch.object(s,'expected_dispatch',side_effect=lambda value:'complete' if value['stage']=='complete' else 'execute'), \
             patch.object(s,'authenticate_runner_route',side_effect=receipts.ReceiptError('unavailable')) as check:
            for stage in ('expand','complete'):
                state={'registration':reg,'stage':stage,'checkpoint':{},'phase':'running','retry_authorized':True,
                    'next_ordinal':0,'next_dispatch_ordinal':0,'attempt_control_sha256':[],'resource_plan':{'record':{'maximum_units':3}}}
                if stage=='complete':state['final_input_checkpoint_sha256']=p.digest(p.canonical({}))
                reservations=s.DispatchReservations(Path(directory)/('reservations-'+stage))
                with self.assertRaises(receipts.ReceiptError):s.dispatch(api,state,{'session_id':'00000000-0000-4000-8000-000000000001'},ROOT,reservations)
            control={key:None for key in s.CONTROL_KEYS}
            control.update(registration=reg,unit_runs=[],attempt_runs=[],resume_decisions=[],uncertain_dispatches=[],
                probe_case='success',probe_nonce='a'*64)
            with patch.object(s,'api_environment',return_value=api),self.assertRaises(receipts.ReceiptError):
                s.dispatch_probe(control,ROOT,directory)
            self.assertEqual(check.call_count,3);api.opener.open.assert_not_called()


class DispatchReservationTests(unittest.TestCase):
    """N6: the dispatch ordinal is reserved durably BEFORE the API call and reconciled on restart."""
    SESSION = '00000000-0000-4000-8000-000000000001'

    def setUp(self):
        import tempfile
        scratch = ROOT / '.agent-proof-fixtures/dispatch-reservations'
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name) / 'dispatch-reservations'
        self.reservations = s.DispatchReservations(self.directory)

    def control(self, marker='a'):
        return {'session_id': self.SESSION, 'marker': marker}

    def expected(self, **changes):
        return {'registration_sha256': 'b' * 64, 'mode': 'unit', 'operation': 'execute', **changes}

    def record(self, ordinal, marker='a'):
        return s.dispatch_record(ordinal, self.SESSION, self.expected(), self.control(marker))

    def restarted(self):
        """A new controller process: nothing but the directory survives."""
        return s.DispatchReservations(self.directory)

    def test_a_reservation_is_durable_exclusive_and_never_a_partial_record(self):
        record = self.record(0)
        self.reservations.reserve(record)
        self.assertEqual(self.restarted().read(0), record)
        self.assertEqual(self.restarted().ordinals(), [0])
        self.assertEqual(list(self.directory.glob('*.partial')), [])
        with self.assertRaises(FileExistsError):
            self.reservations.reserve(self.record(0, 'other'))
        self.assertEqual(self.restarted().read(0), record)

    def test_the_ordinal_is_reserved_before_anything_is_sent(self):
        action, held = s.claim_dispatch(self.reservations, self.record(0), lambda _r: [], [])
        self.assertEqual(action, 'post')
        self.assertEqual(self.restarted().state(0), 'reserved')  # on disk before the caller can POST

    def test_a_crash_before_the_post_resends_the_same_ordinal_and_payload_exactly_once(self):
        first = self.record(0)
        s.claim_dispatch(self.reservations, first, lambda _r: [], [])
        # restart: the controller rebuilds an equal dispatch (later wall clock) and recovers the reservation
        later = {**self.record(0), 'reserved_at': '2999-01-01T00:00:00+00:00'}
        action, held = s.claim_dispatch(self.restarted(), later, lambda _r: [], [])
        self.assertEqual((action, held), ('post', first))  # same ordinal, same payload, the ORIGINAL timestamp
        self.assertEqual(self.restarted().ordinals(), [0])  # nothing new was reserved or skipped
        # a different dispatch can never take that ordinal
        with self.assertRaises(receipts.ReceiptError):
            s.claim_dispatch(self.restarted(), self.record(0, 'different control'), lambda _r: [], [])
        with self.assertRaises(receipts.ReceiptError):
            s.claim_dispatch(self.restarted(), s.dispatch_record(0, self.SESSION, self.expected(operation='publish'),
                                                                 self.control()), lambda _r: [], [])

    def test_a_crash_after_the_post_before_the_run_is_visible_neither_resends_nor_skips(self):
        s.claim_dispatch(self.reservations, self.record(0), lambda _r: [], [])
        self.reservations.mark_sent(0)
        restarted = self.restarted()
        with self.assertRaisesRegex(receipts.ReceiptError, 'not visible yet'):
            s.claim_dispatch(restarted, self.record(0), lambda _r: [], [])
        # the ordinal stays consumed: the next ordinal cannot be claimed over it
        with self.assertRaises(receipts.ReceiptError):
            s.claim_dispatch(restarted, self.record(1), lambda _r: [], [])
        # once the run appears it is adopted, and nothing is sent
        run = {'id': 7}
        self.assertEqual(s.claim_dispatch(restarted, self.record(0), lambda _r: [run], []), ('adopt', run))
        with self.assertRaises(receipts.ReceiptError):
            s.claim_dispatch(restarted, self.record(0), lambda _r: [run, {'id': 8}], [])

    def test_a_visible_run_is_adopted_even_when_the_sent_marker_was_never_written(self):
        s.claim_dispatch(self.reservations, self.record(0), lambda _r: [], [])
        action, run = s.claim_dispatch(self.restarted(), self.record(0), lambda _r: [{'id': 3}], [])
        self.assertEqual((action, run), ('adopt', {'id': 3}))
        self.assertEqual(self.restarted().state(0), 'sent')

    def test_reservations_are_reconciled_with_the_authenticated_attempt_inventory(self):
        digest = lambda marker: __import__('hashlib').sha256(p.canonical(self.control(marker))).hexdigest()
        for ordinal, marker in enumerate('abc'):
            self.reservations.reserve(self.record(ordinal, marker))
        ran = [digest('a'), digest('b')]
        self.assertEqual(s.settle_dispatch(self.reservations, ran), 2)  # the reservation at the next ordinal
        with self.assertRaisesRegex(receipts.ReceiptError, 'never skipped'):
            s.settle_dispatch(self.reservations, ran[:1])  # a reservation beyond the next ordinal
        with self.assertRaisesRegex(receipts.ReceiptError, 'other than the one'):
            s.settle_dispatch(self.reservations, [digest('a'), digest('x')])
        self.assertIsNone(s.settle_dispatch(self.reservations, ran + [digest('c')]))

    def test_a_dispatch_not_at_the_next_authenticated_ordinal_is_refused(self):
        with self.assertRaises(receipts.ReceiptError):
            s.claim_dispatch(self.reservations, self.record(2), lambda _r: [], [])
        self.assertEqual(self.reservations.ordinals(), [])

    def api(self, observed, status=204, error=None):
        from types import SimpleNamespace
        from unittest.mock import MagicMock

        def open_(request, timeout):
            observed.append((self.restarted().state(0), request.data))
            if error is not None:
                raise error
            response = MagicMock()
            response.__enter__.return_value = SimpleNamespace(status=status)
            return response
        return SimpleNamespace(root='https://forge.invalid/api/v1/repos/example/repo', token='fixture',
                               opener=SimpleNamespace(open=open_))

    def plan(self):
        return {**self.expected(), 'kind': 'execute', 'control': self.control(), 'ordinal': 0, 'payload': b'{"ref":"x"}',
                'reg': {}, 'expected_prior_checkpoint': None}

    def test_the_sent_marker_is_durable_before_the_post_begins(self):
        observed = []
        s.claim_dispatch(self.reservations, self.record(0), lambda _r: [], [])
        with patch.object(s, 'authenticate_runner_route'):
            s.post_dispatch(self.api(observed), self.plan(), ROOT, self.reservations)
        self.assertEqual(observed, [('sent', b'{"ref":"x"}')])

    def test_a_definitive_refusal_makes_the_reserved_ordinal_sendable_again_but_a_server_error_does_not(self):
        import urllib.error
        s.claim_dispatch(self.reservations, self.record(0), lambda _r: [], [])
        with patch.object(s, 'authenticate_runner_route'):
            with self.assertRaises(urllib.error.HTTPError):
                s.post_dispatch(self.api([], error=urllib.error.HTTPError('u', 422, 'bad', {}, None)),
                                self.plan(), ROOT, self.reservations)
            self.assertEqual(self.restarted().state(0), 'reserved')
            with self.assertRaises(urllib.error.HTTPError):
                s.post_dispatch(self.api([], error=urllib.error.HTTPError('u', 502, 'gateway', {}, None)),
                                self.plan(), ROOT, self.reservations)
        self.assertEqual(self.restarted().state(0), 'sent')  # it may have reached the server: uncertain

    def test_no_post_is_made_when_the_route_check_fails_and_the_reservation_waits_unsent(self):
        observed = []
        s.claim_dispatch(self.reservations, self.record(0), lambda _r: [], [])
        with patch.object(s, 'authenticate_runner_route', side_effect=receipts.ReceiptError('unavailable')):
            with self.assertRaises(receipts.ReceiptError):
                s.post_dispatch(self.api(observed), self.plan(), ROOT, self.reservations)
        self.assertEqual(observed, [])
        self.assertEqual(self.restarted().state(0), 'reserved')

    def test_dispatch_adopts_a_run_without_posting_and_otherwise_posts_once(self):
        state = {'attempt_control_sha256': []}
        plan = self.plan()
        posts = []
        with patch.object(s, 'plan_dispatch', return_value=plan), \
                patch.object(s, 'post_dispatch', side_effect=lambda api, planned, root, res: posts.append(planned['ordinal'])), \
                patch.object(s, 'matching_runs', return_value=[]):
            expected, adopted = s.dispatch(None, state, self.control(), ROOT, self.reservations)
            self.assertIsNone(adopted)
            self.assertEqual(posts, [0])
            self.assertEqual(expected['ordinal'], 0)
            self.assertEqual(expected['dispatched_at'], self.reservations.read(0)['reserved_at'])
        # the controller crashed after the POST: the restarted dispatch finds the run and sends nothing
        with patch.object(s, 'plan_dispatch', return_value=plan), \
                patch.object(s, 'post_dispatch', side_effect=lambda *a: posts.append('again')), \
                patch.object(s, 'matching_runs', return_value=[{'id': 9}]):
            self.reservations.mark_sent(0)
            expected, adopted = s.dispatch(None, state, self.control(), ROOT, self.restarted())
        self.assertEqual(adopted, {'id': 9})
        self.assertEqual(posts, [0])


    # R3: an uncertain dispatch consumes its unit and ordinal; the retry takes a new ordinal, reconciled with the inventory.
    def digest(self, marker='a'):
        return p.digest(p.canonical(self.control(marker)))

    def sent(self, ordinal=0, marker='a'):
        s.claim_dispatch(self.reservations, self.record(ordinal, marker), lambda _r: [], [self.digest()] * ordinal)
        self.reservations.mark_sent(ordinal)
        return self.reservations.read(ordinal)

    def later(self, held, seconds):
        return s.utc(held['reserved_at']) + seconds

    def test_a_sent_dispatch_is_waited_for_and_then_settled_as_uncertain_consuming_its_ordinal(self):
        held = self.sent()
        before = self.later(held, s.UNCERTAIN_DISPATCH_SECONDS - 1)
        with self.assertRaisesRegex(receipts.ReceiptError, 'not visible yet'):
            s.claim_dispatch(self.restarted(), self.record(0), lambda _r: [], [], now=before)
        self.assertEqual(self.restarted().state(0), 'sent')  # the wait changed nothing
        after = self.later(held, s.UNCERTAIN_DISPATCH_SECONDS)
        action, settled = s.claim_dispatch(self.restarted(), self.record(0), lambda _r: [], [], now=after)
        self.assertEqual((action, settled), ('uncertain', held))
        self.assertEqual(self.restarted().state(0), 'uncertain')
        self.assertEqual(self.restarted().uncertain(), [{'ordinal': 0, 'control_sha256': self.digest()}])
        # settling is idempotent and never turns into a second POST of the same ordinal
        again = s.claim_dispatch(self.restarted(), self.record(0), lambda _r: [], [], now=after + 10 ** 6)
        self.assertEqual(again[0], 'uncertain')

    def test_the_retry_of_an_uncertain_dispatch_takes_a_new_ordinal_and_the_old_one_is_never_sent_again(self):
        held = self.sent()
        s.claim_dispatch(self.restarted(), self.record(0), lambda _r: [], [], now=self.later(held, 10 ** 6))
        folded = s.fold_uncertain({'uncertain_dispatches': []}, self.restarted())
        self.assertEqual(folded['uncertain_dispatches'], [{'ordinal': 0, 'control_sha256': self.digest()}])
        self.assertEqual(s.control_ordinal({'attempt_runs': [], **folded}), 1)  # the uncertain dispatch consumed ordinal 0
        dispatched = [row['control_sha256'] for row in folded['uncertain_dispatches']]
        action, retry = s.claim_dispatch(self.restarted(), self.record(1, 'b'), lambda _r: [], dispatched)
        self.assertEqual((action, retry['ordinal']), ('post', 1))
        self.assertEqual(self.restarted().ordinals(), [0, 1])  # nothing reused, nothing skipped
        self.assertEqual(self.restarted().state(0), 'uncertain')
        with self.assertRaises(receipts.ReceiptError):  # the old ordinal can never be claimed again by another dispatch
            s.claim_dispatch(self.restarted(), self.record(0, 'b'), lambda _r: [], dispatched)

    def test_an_uncertain_dispatch_whose_run_appears_later_is_refused_not_spliced(self):
        held = self.sent()
        s.claim_dispatch(self.restarted(), self.record(0), lambda _r: [], [], now=self.later(held, 10 ** 6))
        with self.assertRaisesRegex(receipts.ReceiptError, 'settled as uncertain but its run appeared'):
            s.claim_dispatch(self.restarted(), self.record(0), lambda _r: [{'id': 7}], [])

    def test_coordinate_folds_the_uncertain_dispatch_and_dispatches_again_with_the_next_ordinal(self):
        import json
        self.sent()  # reserved and sent by an earlier controller process; its run never became visible
        reg = {'source': {'commit': 'a' * 40}}
        control = {key: None for key in s.CONTROL_KEYS}
        control.update(registration=reg, session_id=self.SESSION, attempt_runs=[], unit_runs=[], resume_decisions=[],
                       uncertain_dispatches=[])
        replayed, sent = [], []

        class Stop(Exception):
            pass

        def replay(_api, current, _root, _work):
            replayed.append(current)
            return {'next_dispatch_ordinal': s.control_ordinal(current),
                    'resource_plan': {'record': {'maximum_units': 5}}}, None

        def dispatch(_api, state, current, _root, reservations):
            sent.append(state['next_dispatch_ordinal'])
            if state['next_dispatch_ordinal'] == 0:
                reservations.mark_uncertain(0)  # what claim_dispatch does once the wait has elapsed
                raise s.UncertainDispatch(0)
            return {'control': current, 'ordinal': state['next_dispatch_ordinal']}, None

        def write(path, _value):
            if Path(path).name == 'dispatch-0.json':
                raise Stop

        with patch.object(s, 'api_environment', return_value=SimpleNamespace()), \
                patch.object(s, 'reconcile_control', side_effect=lambda _api, current, _root: (current, None)), \
                patch.object(s, 'replay_control', side_effect=replay), patch.object(s, 'dispatch', side_effect=dispatch), \
                patch.object(s.p, 'write', side_effect=write):
            with self.assertRaises(Stop):
                s.coordinate(control, ROOT, self.directory.parent)
        self.assertEqual(sent, [0, 1])  # the uncertain ordinal is not retried: the retry takes ordinal 1
        self.assertEqual([current['uncertain_dispatches'] for current in replayed],
                         [[], [{'ordinal': 0, 'control_sha256': self.digest()}]])

    def test_the_inventory_explains_an_uncertain_ordinal_and_refuses_a_late_run_for_it(self):
        import json
        reg = {'source': {'commit': 'a' * 40}}
        base = {'schema_version': 1, 'registration': reg, 'resource_plan': {'immutable': 'plan'}, 'probe_run': {},
                'unit_runs': [], 'attempt_runs': [], 'resume_decisions': [], 'uncertain_dispatches': [],
                'main_run': None, 'session_id': self.SESSION, 'transport_conformance': None}
        row = {'ordinal': 0, 'control_sha256': self.digest()}

        def event(run_id, control):
            return {'id': run_id, 'workflow_id': p.WORKFLOW, 'event': 'workflow_dispatch', 'event_payload':
                    json.dumps({'inputs': {'operation': 'execute', 'mode': 'unit', 'control': json.dumps(control)}})}

        def producer(_api, run_id, *_args):
            return {'id': run_id}, {'id': run_id + 100, 'task_id': run_id + 200, 'status': 'success'}, {}

        def inventory(rows, control):
            api = SimpleNamespace(pages=lambda *args, **kwargs: rows)
            with patch.object(p, 'producer', producer):
                return s.attempt_inventory(api, reg, control)
        retry = event(8, {**base, 'uncertain_dispatches': [row]})  # ordinal 1: the retry of the uncertain ordinal 0
        current = {**base, 'attempt_runs': [8], 'uncertain_dispatches': [row]}
        items = inventory([retry], current)
        self.assertEqual([item['ordinal'] for item in items], [1])
        self.assertEqual(list(items.uncertain), [row])
        self.assertEqual(s.control_ordinal(current), 2)
        # a run at ordinal 1 with no run and no declaration at ordinal 0 is an unexplained gap
        skipped = event(8, {**base, 'attempt_runs': [7]})
        with self.assertRaisesRegex(receipts.ReceiptError, 'unexplained attempted-ordinal gap'):
            inventory([skipped], {**base, 'attempt_runs': [8]})
        # declared by the current control but not by the control of the run that followed it
        with self.assertRaisesRegex(receipts.ReceiptError, 'omitted or changed an uncertain dispatch'):
            inventory([skipped], {**base, 'attempt_runs': [8], 'uncertain_dispatches': [row]})
        # the uncertain dispatch's own run appears later: it takes ordinal 0, which was declared uncertain
        late = event(7, base)
        with self.assertRaisesRegex(receipts.ReceiptError, 'settled as uncertain but its run appeared'):
            inventory([late, retry], {**current, 'attempt_runs': [7, 8]})

    def test_a_reservation_directory_belongs_to_one_session_and_registration(self):
        identity = {'session_id': self.SESSION, 'registration_sha256': 'b' * 64}
        directory = self.directory.parent / 'bound'
        s.DispatchReservations(directory, identity)
        s.DispatchReservations(directory, identity)  # the same session may reopen it
        for changed in ({**identity, 'session_id': '00000000-0000-4000-8000-000000000002'},
                        {**identity, 'registration_sha256': 'c' * 64}):
            with self.assertRaisesRegex(receipts.ReceiptError, 'different session or registration'):
                s.DispatchReservations(directory, changed)

    def test_settle_reads_only_the_reservations_that_exist_and_names_the_session_of_each(self):
        bound = s.DispatchReservations(self.directory.parent / 'settled',
                                       {'session_id': self.SESSION, 'registration_sha256': 'b' * 64})
        self.assertIsNone(s.settle_dispatch(bound, [self.digest()]))  # an ordinal nobody here reserved passes
        foreign = {**self.record(0), 'session_id': '00000000-0000-4000-8000-000000000002'}
        bound.reserve(foreign)
        with self.assertRaisesRegex(receipts.ReceiptError, 'names a different session'):
            s.settle_dispatch(bound, [self.digest()])
