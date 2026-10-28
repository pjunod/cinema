"""Closed admission/progress/Main authority fixtures. No remote producer runs."""
from __future__ import annotations
import copy
import os
from pathlib import Path
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
                env=s.child_environment({},Path(directory)/'request.json',ROOT,directory)
            self.assertFalse({'GITHUB_TOKEN','ACTIONS_RUNTIME_TOKEN','GITHUB_EVENT_PATH'} & set(env))
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
              'probe_run':{},'unit_runs':[],'attempt_runs':[],'resume_decisions':[],
              'main_run':None,'session_id':'00000000-0000-4000-8000-000000000001'}
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

if __name__ == '__main__':unittest.main()
