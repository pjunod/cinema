"""Synthetic whole-web-cohort provenance; never executes Node/npm/Make."""
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest import mock
import warnings
import zipfile

from validation import web_cohort_retention as web
from validation import python_unit_receipts as receipts

ROOT = Path(__file__).resolve().parents[2]
COMMIT, BASE = 'b'*40, 'c'*40
SCOPE = {'repository':1,'pr':1263,'branch':'topic','base':'main','workflow':'main-fast-lane.yml'}


def packed(receipt,log,extra=()):
    output = io.BytesIO()
    with warnings.catch_warnings():
        warnings.simplefilter('ignore',UserWarning)
        with zipfile.ZipFile(output,'w',compression=zipfile.ZIP_STORED) as archive:
            for name,raw,mode in [('receipt.json',json.dumps(receipt).encode(),stat.S_IFREG|0o644),
                ('web-cohort.log',log,stat.S_IFREG|0o644),*extra]:
                info = zipfile.ZipInfo(name); info.external_attr = mode << 16
                archive.writestr(info,raw)
    return output.getvalue()


def upload(name,aid,data):
    size = len(data)
    return ['With the provided path, there will be 2 files uploaded','Artifact name is valid!',
        'Root directory input is valid!','Beginning upload of artifact content to blob storage',
        f'Uploaded bytes {size}','Finished uploading artifact content to blob storage!',
        'SHA256 hash of uploaded artifact zip is '+web.digest(data),'Finalizing artifact upload',
        f'Artifact {name}.zip successfully finalized. Artifact ID {aid}',
        f'Artifact {name} has been successfully uploaded! Final size is {size} bytes. Artifact ID is {aid}']


class WebCohortRetentionTests(unittest.TestCase):
    def source(self):
        contents = {path:b'# fixed reviewed helper\n' for path in web.HELPERS}
        contents.update({'Makefile':b'web-unit-check:\n\t@node tests/web/check.test.js\n',
            'Cargo.toml':b'[workspace.package]\nversion = "1.0.0"\n',
            'tools/web-types/package.json':b'{}','tools/web-types/package-lock.json':json.dumps(
                {'packages':{'node_modules/typescript':{'version':'6.0.3'}}}).encode(),
            web.BASELINE:b'# fixed baseline\n','tests/web/check.test.js':b'// fixed test\n',
            'crates/plurxd/src/web/player.js':b'// shipped asset\n'})
        def rows():
            return sorted([{'path':path,'mode':'100644','blob':hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest(),
                'sha256':web.digest(raw)} for path,raw in contents.items()],key=lambda row:row['path'])
        original_shape = web.digest(web.canonical([row for row in rows() if web.shape_row(row)]))
        roots = [b'100644 blob '+b'a'*40+b'\tMakefile']
        def git(workspace,*args,**kwargs):
            if args[0]=='ls-tree':
                if '-r' not in args:return b'\0'.join(roots)+b'\0'
                return b'\0'.join((row['mode']+' blob '+row['blob']+'\t'+row['path']).encode() for row in rows())+b'\0'
            if args[0]=='cat-file':
                data = next(raw for path,raw in contents.items() if next(row for row in rows() if row['path']==path)['blob']==args[2])
                return str(len(data)).encode() if args[1]=='-s' else data
            if args[0]=='merge-base':return (BASE+'\n').encode()
            if args[0]=='show':return b'# historical baseline\n'
            raise AssertionError(args)
        return contents,roots,git,original_shape

    def test_complete_shape_guard_refuses_added_executable_helper_and_alternate_make(self):
        contents,roots,git,shape = self.source()
        with mock.patch.object(web,'git',side_effect=git),mock.patch.object(web,'SHAPE_SHA256',shape):
            web.manifest(ROOT,COMMIT,BASE)
            for path in ('tests/web/new.test.js','scripts/web-types'):
                original = contents.get(path);contents[path]=b'require("outside-root")\n'
                with self.subTest(path=path),self.assertRaisesRegex(receipts.ReceiptError,'closure review required'):
                    web.manifest(ROOT,COMMIT,BASE)
                if original is None:del contents[path]
                else:contents[path]=original
            for alias in ('makefile','GNUmakefile','MAKEFILE','GNUMakefile'):
                roots.append(b'120000 blob '+b'f'*40+b'\t'+alias.encode())
                with self.subTest(alias=alias),self.assertRaisesRegex(receipts.ReceiptError,'Make selector'):
                    web.manifest(ROOT,COMMIT,BASE)
                roots.pop()

    def test_supported_asset_and_baseline_inputs_change_key_without_forging_old_manifest(self):
        contents,roots,git,shape = self.source()
        with mock.patch.object(web,'git',side_effect=git),mock.patch.object(web,'SHAPE_SHA256',shape):
            before = web.manifest(ROOT,COMMIT,BASE); frozen = copy.deepcopy(before)
            contents['crates/plurxd/src/web/player.js'] += b'// changed\n'
            after = web.manifest(ROOT,COMMIT,BASE)
            self.assertNotEqual(web.execution_key(before),web.execution_key(after))
            self.assertEqual(before,frozen)
            different_base = web.manifest(ROOT,COMMIT,'d'*40)
            self.assertNotEqual(web.execution_key(after),web.execution_key(different_base))
            self.assertEqual(after['baseline']['sha256'],different_base['baseline']['sha256'])

    def test_web_job_identity_preserves_commands_and_ignores_other_job_bytes(self):
        for block in (web.LEGACY_JOB,web.adapted_job()):
            text = 'jobs:\n'+block+'  rust_compile:\n    run: arbitrary unrelated Rust change\n'
            self.assertEqual(web.web_job(text.encode()),block)
        for change in ('node-version: "24"','timeout-minutes: 11','runs-on: [self-hosted, Linux, ARM64]'):
            changed = web.adapted_job().replace('node-version: "22"',change) if change.startswith('node-version') else (
                web.adapted_job().replace('timeout-minutes: 10',change) if change.startswith('timeout') else
                web.adapted_job().replace('runs-on: [self-hosted, Linux, X64, lab, general]',change))
            with mock.patch.object(web,'git',return_value=('jobs:\n'+changed).encode()):
                with self.assertRaisesRegex(receipts.ReceiptError,'job/command/environment shape'):
                    web.workflow_identity(ROOT,COMMIT)
        self.assertIn('make web-unit-check',web.COMMANDS)
        self.assertIn('GITHUB_TOKEN: ${{ github.token }}',web.WRAPPER)
        self.assertNotIn('make web-unit-check',web.WRAPPER)

    def context(self):
        repo={'id':1,'full_name':'example/project'}
        pull={'number':1263,'draft':False,'state':'open','head':{'sha':COMMIT,'ref':'topic','repo':{'id':1}},
            'base':{'sha':BASE,'ref':'main','repo':{'id':1}}}
        event={'repository':repo,'number':1263,'action':'synchronize','pull_request':pull}
        run={'id':6000,'commit_sha':COMMIT,'workflow_id':'main-fast-lane.yml','event':'pull_request',
            'event_payload':json.dumps(event),'repository':repo,'prettyref':'#1263'}
        job={'id':6001,'name':web.JOB,'repo_id':1,'run_id':6000,'attempt':1,'task_id':6002,'status':'running'}
        class API:
            def get(self,path):return repo if path=='' else pull if path.startswith('/pulls/') else run
            def pages(self,path,query=None,field=None):return [job]
        return API(),pull,event,run,job

    def test_current_authentication_requires_wellformed_live_head_and_own_initial_task(self):
        api,pull,event,run,job = self.context()
        scratch=ROOT/'target/web-retention-fixtures';scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            event_path=Path(directory)/'event.json';event_path.write_text(json.dumps(event))
            env={'GITHUB_EVENT_NAME':'pull_request','GITHUB_EVENT_PATH':str(event_path),'GITHUB_JOB':web.JOB_ID,
                'GITHUB_REPOSITORY':'example/project','GITHUB_SHA':COMMIT,'GITHUB_RUN_ATTEMPT':'1','GITHUB_RUN_ID':'6000',
                'GITHUB_API_URL':'https://ci.example/api/v1','GITHUB_SERVER_URL':'https://ci.example'}
            with mock.patch.dict(os.environ,env),mock.patch.object(web.receipts,'API',return_value=api), \
                 mock.patch.object(web,'git',side_effect=lambda workspace,*args,**kw:(COMMIT+'\n').encode() if args[0]=='rev-parse' else b''):
                self.assertEqual(web.current_context(ROOT)[4],job)
                for change in ({'attempt':2},{'attempt':True},{'status':'success'},{'repo_id':2},{'task_id':0}):
                    saved=job.copy();job.update(change)
                    with self.subTest(change=change),self.assertRaises(receipts.ReceiptError):web.current_context(ROOT)
                    job.clear();job.update(saved)
                pull['head']={**pull['head'],'sha':'d'*40}
                with self.assertRaisesRegex(receipts.ReceiptError,'Main receipt must match'):web.current_context(ROOT)

    def test_known_origin_tamper_or_failed_job_refuses_without_launch(self):
        api,pull,event,run,job = self.context()
        run.update(id=web.ORIGIN_RUN,commit_sha=web.PRODUCER)
        event['pull_request']['head']['sha']=web.PRODUCER;run['event_payload']=json.dumps(event)
        job.update(id=web.ORIGIN_JOB,run_id=web.ORIGIN_RUN,task_id=web.ORIGIN_TASK,status='success')
        raw=b'authenticated but altered log'
        with mock.patch.object(api,'bytes',return_value=raw,create=True),mock.patch.object(web,'checkout',return_value=[]), \
             mock.patch.object(web,'manifest',return_value={}),mock.patch.object(web.subprocess,'run') as launch:
            with self.assertRaisesRegex(receipts.ReceiptError,'Known web origin source/log/task'):web.legacy_result(api,SCOPE,ROOT)
            job['status']='failure'
            with self.assertRaisesRegex(receipts.ReceiptError,'did not succeed'):web.legacy_result(api,SCOPE,ROOT)
            launch.assert_not_called()

    def test_archive_and_command_log_refuse_traversal_symlink_duplicate_and_incomplete(self):
        log=b'WEB-COHORT-START key\n'
        for extra in ([('../escape',b'x',stat.S_IFREG|0o644)],
                      [('alias',b'x',stat.S_IFLNK|0o777)],
                      [('receipt.json',b'{}',stat.S_IFREG|0o644)]):
            with self.subTest(path=extra[0][0]),self.assertRaisesRegex(receipts.ReceiptError,'member set'):
                web.unpack(packed({},log,extra))
        with mock.patch.object(web,'MAX_LOG',2):
            with self.assertRaisesRegex(receipts.ReceiptError,'size/ratio'):web.unpack(packed({},log))
        complete=['WEB-COHORT-START key']
        for i in range(len(web.COMMANDS)):complete += [f'WEB-COHORT-COMMAND {i} START',f'WEB-COHORT-COMMAND {i} PASS']
        complete += ['WEB-COHORT-TYPESCRIPT Version 6.0.3','WEB-COHORT-FINISH key 0']
        web.fresh_log(('\n'.join(complete)+'\n').encode(),'key')
        for changed in (complete[:-1],complete+[complete[-1]],[*complete[:2],*complete[3:]],['WEB-COHORT-START wrong',*complete[1:]]):
            with self.subTest(changed=changed),self.assertRaisesRegex(receipts.ReceiptError,'incomplete/ambiguous'):
                web.fresh_log(('\n'.join(changed)+'\n').encode(),'key')

    def test_fresh_branch_preserves_commands_scrubs_credentials_and_has_no_failure_fallback(self):
        api,pull,event,run,job=self.context();context=(api,SCOPE,COMMIT,run,job,BASE)
        inputs={'files':[],'baseline':{'event_base':BASE},'runtime_contract':web.RUNTIME}
        scratch=ROOT/'target/web-retention-fixtures';scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory, \
             mock.patch.object(web,'workflow_identity',return_value={'kind':'v1','sha256':'a'*64}), \
             mock.patch.object(web,'live_inputs'),mock.patch.object(web,'manifest',return_value=inputs), \
             mock.patch.object(web,'matching_result',return_value=None),mock.patch.object(web,'current_context',return_value=context), \
             mock.patch.object(web,'git',side_effect=lambda workspace,*args,**kw:b'' if args[0]=='diff' else b'd'*40), \
             mock.patch.object(web.subprocess,'run',return_value=mock.Mock(returncode=0)) as launch, \
             mock.patch.object(web.subprocess,'check_output',return_value='Version 6.0.3\n'), \
             mock.patch.dict(os.environ,{'GITHUB_TOKEN':'secret','GH_TOKEN':'secret','ACTIONS_RUNTIME_TOKEN':'secret','PATH':'/tools','TMPDIR':'/scratch'}):
            # Different policy/input key selects explicit fresh, never a legacy re-run.
            out=Path(directory)/'fresh';out.mkdir()
            with mock.patch.object(web,'manifest',side_effect=[inputs,{**inputs,'different':True},inputs]):
                result=web.run_gate(ROOT,context,out,{'typescript':{'declared_locked':'6.0.3','current_binary_observed':None,'current_qualification_execution':False}},True)
            self.assertTrue(result['current_test_events']);self.assertIsNone(result['origin'])
            self.assertEqual([call.args[0][-1] for call in launch.call_args_list],list(web.COMMANDS))
            for call in launch.call_args_list:
                env=call.kwargs['env'];self.assertEqual(env['PATH'],'/tools')
                self.assertFalse({'GITHUB_TOKEN','GH_TOKEN','ACTIONS_RUNTIME_TOKEN'} & set(env))
            self.assertEqual(launch.call_args_list[-1].kwargs['env']['TMPDIR'],'/tmp')
            launch.reset_mock();launch.return_value=mock.Mock(returncode=1)
            out=Path(directory)/'failed';out.mkdir()
            with mock.patch.object(web,'manifest',side_effect=[inputs,{**inputs,'different':True},inputs]):
                with self.assertRaisesRegex(receipts.ReceiptError,'failed; no retention/fallback'):
                    web.run_gate(ROOT,context,out,{'typescript':{'declared_locked':'6.0.3','current_binary_observed':None,'current_qualification_execution':False}},True)
            self.assertEqual(launch.call_count,1)
            self.assertNotIn(b'WEB-COHORT-FINISH',(out/'web-cohort.log').read_bytes())

    def test_retention_emits_original_log_and_zero_current_web_events(self):
        api,pull,event,run,job=self.context();context=(api,SCOPE,COMMIT,run,job,BASE)
        inputs={'files':[]};key=web.execution_key(inputs);raw=b'original raw whole-job execution\n'
        original={'kind':'legacy','input_key':key,'log_sha256':web.digest(raw)}
        scratch=ROOT/'target/web-retention-fixtures';scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory, \
             mock.patch.object(web,'workflow_identity',return_value={'kind':'v1','sha256':'a'*64}), \
             mock.patch.object(web,'live_inputs'),mock.patch.object(web,'manifest',return_value=inputs), \
             mock.patch.object(web,'legacy_result',return_value=(original,raw)), \
             mock.patch.object(web,'current_context',return_value=context), \
             mock.patch.object(web,'git',side_effect=lambda workspace,*args,**kw:b'' if args[0]=='diff' else b'd'*40), \
             mock.patch.object(web.subprocess,'run') as launch:
            result=web.run_gate(ROOT,context,Path(directory),{'typescript':{'declared_locked':'6.0.3','current_binary_observed':None,'current_qualification_execution':False}},True)
            self.assertEqual(result['mode'],'retained_prior_execution');self.assertFalse(result['current_test_events'])
            self.assertEqual(result['origin'],original);self.assertEqual((Path(directory)/'web-cohort.log').read_bytes(),raw)
            launch.assert_not_called()

    def v1_fixture(self):
        api,pull,event,run,job=self.context();job['status']='success'
        inputs={'files':[],'baseline':{'event_base':BASE},'runtime_contract':web.RUNTIME};key=web.execution_key(inputs)
        lines=['WEB-COHORT-START '+key]
        for i in range(len(web.COMMANDS)):lines += [f'WEB-COHORT-COMMAND {i} START',f'WEB-COHORT-COMMAND {i} PASS']
        log=('\n'.join([*lines,'WEB-COHORT-TYPESCRIPT Version 6.0.3','WEB-COHORT-FINISH '+key+' 0'])+'\n').encode()
        adapter=b'reviewed adapter bytes';tree='d'*40
        receipt={'schema_version':1,'mode':'fresh_execution','commit':COMMIT,'tree':tree,'run':6000,'job':6001,
            'attempt':1,'scope':SCOPE,'inputs':inputs,'input_key':key,'policy_sha256':web.policy_fingerprint(),
            'adapter_sha256':web.digest(adapter),'web_job':{'kind':'v1','sha256':'a'*64},
            'runtime':{'job_recorded':{k:v for k,v in web.RUNTIME.items() if k!='typescript_lock'},
                'typescript':{'declared_locked':'6.0.3','current_binary_observed':'Version 6.0.3','current_qualification_execution':True}},
            'origin':None,'log_sha256':web.digest(log),'current_test_events':True}
        envelope={'id':7000,'name':'web-cohort-6000-1','run_id':6000,'expired':False}
        state={'receipt':receipt,'log':log,'data':b'','lines':[]}
        def seal():
            state['data']=packed(state['receipt'],state['log']);envelope['size_in_bytes']=len(state['data'])
            state['lines']=['WEB-COHORT-RECEIPT '+web.digest(web.canonical(state['receipt'])),
                *upload(envelope['name'],7000,state['data'])]
        seal()
        api.pages=lambda path,query=None,field=None:[envelope]
        api.bytes=lambda path:state['data']
        return api,run,job,inputs,receipt,state,seal,adapter,tree

    def test_v1_fresh_upload_binds_raw_source_runtime_and_unique_upload_digest(self):
        api,run,job,inputs,receipt,state,seal,adapter,tree=self.v1_fixture()
        with mock.patch.object(web,'authenticate_job',side_effect=lambda *a:(run,job,b'authenticated job log',state['lines'],inputs)), \
             mock.patch.object(web,'workflow_identity',return_value=receipt['web_job']), \
             mock.patch.object(web,'git',side_effect=lambda workspace,*args,**kw:tree.encode() if args[0]=='rev-parse' else adapter):
            original,log=web.v1_result(api,SCOPE,ROOT,6000,6001,COMMIT)
            self.assertEqual(original['kind'],'v1_fresh');self.assertEqual(original['artifact_id'],7000);self.assertEqual(log,state['log'])
            for change in ({'commit':'e'*40},{'inputs':{'files':['tampered']}},{'attempt':True},
                           {'current_test_events':False},{'adapter_sha256':'f'*64}):
                before=copy.deepcopy(receipt);receipt.update(change);seal()
                with self.subTest(change=change),self.assertRaisesRegex(receipts.ReceiptError,'binding differs|origin/event claim'):
                    web.v1_result(api,SCOPE,ROOT,6000,6001,COMMIT)
                receipt.clear();receipt.update(before);seal()
            state['lines'].insert(5,'SHA256 hash of uploaded artifact zip is '+'e'*64)
            with self.assertRaisesRegex(receipts.ReceiptError,'digest/order'):web.v1_result(api,SCOPE,ROOT,6000,6001,COMMIT)
            seal();state['data']=state['data'][:-1]+b'!'
            with self.assertRaisesRegex(receipts.ReceiptError,'uploaded archive digest/size'):
                web.v1_result(api,SCOPE,ROOT,6000,6001,COMMIT)

    def test_v1_retained_resolves_real_origin_once_and_refuses_chain_or_fabricated_events(self):
        api,run,job,inputs,receipt,state,seal,adapter,tree=self.v1_fixture()
        raw=b'original legacy raw log';key=web.execution_key(inputs)
        origin={'kind':'legacy','run':web.ORIGIN_RUN,'job':web.ORIGIN_JOB,'input_key':key,'log_sha256':web.digest(raw)}
        receipt.update(mode='retained_prior_execution',origin=origin,current_test_events=False,log_sha256=web.digest(raw))
        receipt['runtime']['typescript']={'declared_locked':'6.0.3','current_binary_observed':None,'current_qualification_execution':False}
        state['log']=raw;seal()
        with mock.patch.object(web,'authenticate_job',side_effect=lambda *a:(run,job,b'authenticated job log',state['lines'],inputs)), \
             mock.patch.object(web,'workflow_identity',return_value=receipt['web_job']), \
             mock.patch.object(web,'git',side_effect=lambda workspace,*args,**kw:tree.encode() if args[0]=='rev-parse' else adapter), \
             mock.patch.object(web,'legacy_result',return_value=(origin,raw)) as original:
            self.assertEqual(web.v1_result(api,SCOPE,ROOT,6000,6001,COMMIT),(origin,raw));original.assert_called_once()
            with self.assertRaisesRegex(receipts.ReceiptError,'directly to real execution'):
                web.v1_result(api,SCOPE,ROOT,6000,6001,COMMIT,root_only=True)
            receipt['current_test_events']=True;seal()
            with self.assertRaisesRegex(receipts.ReceiptError,'directly to real execution'):
                web.v1_result(api,SCOPE,ROOT,6000,6001,COMMIT)
            receipt['current_test_events']=False;receipt['origin']={'kind':'v1_fresh','run':6000,'job':6001};seal()
            with self.assertRaisesRegex(receipts.ReceiptError,'cycle/chain'):web.v1_result(api,SCOPE,ROOT,6000,6001,COMMIT)

    def test_adapter_changes_select_the_real_web_job_and_live_aliases_refuse(self):
        from validation import ci_scope
        for path in ('validation/web_cohort_retention.py','tests/operations/test_web_cohort_retention.py'):
            self.assertTrue(ci_scope.scope_for_paths(ci_scope.load_catalog(),(path,))['web_layout'])
        with mock.patch.object(web,'selectors'),mock.patch.object(web,'git',return_value=b'unknown.js\0'):
            with self.assertRaisesRegex(receipts.ReceiptError,'Untracked/ignored web closure'):
                web.live_inputs(ROOT)
        with mock.patch.object(web,'selectors'),mock.patch.object(web,'git',return_value=b''), \
             mock.patch.dict(os.environ,{'NODE_OPTIONS':'--require /outside/helper.js'}):
            with self.assertRaisesRegex(receipts.ReceiptError,'interpreter configuration'):
                web.live_inputs(ROOT)
