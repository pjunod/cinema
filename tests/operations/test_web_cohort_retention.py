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


def packed(receipt,log,extra=(),*,log_name='web-cohort.log',log_mode=stat.S_IFREG|0o644):
    output = io.BytesIO()
    with warnings.catch_warnings():
        warnings.simplefilter('ignore',UserWarning)
        with zipfile.ZipFile(output,'w',compression=zipfile.ZIP_STORED) as archive:
            for name,raw,mode in [('receipt.json',json.dumps(receipt).encode(),stat.S_IFREG|0o644),
                (log_name,log,log_mode),*extra]:
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
            'crates/plurxd/src/web/player.js':b'// shipped executable\n',
            'crates/plurxd/src/web/playback-policy.js':b'module.exports = {};\n',
            'crates/plurxd/src/web/index.html':b'<script src="/assets/player.js"></script>\n',
            'crates/plurxd/src/web/jsconfig.json':b'{"files":["player.js"]}\n',
            'crates/plurxd/src/web/globals.d.ts':b'declare const fixture: string;\n',
            'crates/plurxd/src/web/theme.css':b':root { color: white; }\n',
            'crates/plurxd/src/web/icon.png':b'fixed image fixture',
            'crates/plurxd/src/web/site.webmanifest':b'{"name":"fixture"}\n'})
        def rows():
            return sorted([{'path':path,'mode':'100644','blob':hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest(),
                'sha256':web.digest(raw)} for path,raw in contents.items()],key=lambda row:row['path'])
        original_shape = web.digest(web.canonical([row for row in rows() if web.shape_row(row)]))
        roots = [b'100644 blob '+b'a'*40+b'\tMakefile']
        resolver_rows=[{'name':'unrelated-tool','mode':'100755','kind':'blob'}]
        def git(workspace,*args,**kwargs):
            if args[0]=='ls-tree':
                if args[-1].endswith(':scripts'):
                    return b'\0'.join((row['mode']+' '+row['kind']+' '+('a'*40)+'\t'+row['name']).encode() for row in resolver_rows)+b'\0'
                if '-r' not in args and '--' in args:
                    return b'\0'.join((row['mode']+' blob '+row['blob']+'\t'+row['path']).encode() for row in rows() if row['path'] in web.NODE_PACKAGE_SCOPES)+b'\0'
                if '-r' not in args:return b'\0'.join(roots)+b'\0'
                return b'\0'.join((row['mode']+' blob '+row['blob']+'\t'+row['path']).encode() for row in rows())+b'\0'
            if args[0]=='cat-file':
                data = next(raw for path,raw in contents.items() if next(row for row in rows() if row['path']==path)['blob']==args[2])
                return str(len(data)).encode() if args[1]=='-s' else data
            if args[0]=='merge-base':return (BASE+'\n').encode()
            if args[0]=='show':return b'# historical baseline\n'
            raise AssertionError(args)
        git.resolver_rows=resolver_rows
        git.resolver_layout_sha256=web.digest(web.canonical(resolver_rows))
        return contents,roots,git,original_shape

    def test_complete_shape_guard_refuses_added_executable_helper_and_alternate_make(self):
        contents,roots,git,shape = self.source()
        with mock.patch.object(web,'git',side_effect=git),mock.patch.object(web,'SCRIPTS_RESOLVER_FAMILIES',{'fixture':git.resolver_layout_sha256}),mock.patch.object(web,'SHAPE_FAMILIES',{'fixture':shape}):
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
        with mock.patch.object(web,'git',side_effect=git),mock.patch.object(web,'SCRIPTS_RESOLVER_FAMILIES',{'fixture':git.resolver_layout_sha256}),mock.patch.object(web,'SHAPE_FAMILIES',{'fixture':shape}):
            before = web.manifest(ROOT,COMMIT,BASE); frozen = copy.deepcopy(before)
            after = before
            for resource in ('theme.css','icon.png','site.webmanifest'):
                previous = after
                contents['crates/plurxd/src/web/'+resource] += b' changed resource'
                after = web.manifest(ROOT,COMMIT,BASE)
                self.assertNotEqual(web.execution_key(previous),web.execution_key(after))
                self.assertEqual(before,frozen)
            different_base = web.manifest(ROOT,COMMIT,'d'*40)
            self.assertNotEqual(web.execution_key(after),web.execution_key(different_base))
            self.assertEqual(after['baseline']['sha256'],different_base['baseline']['sha256'])

    def test_web_job_identity_preserves_commands_and_ignores_other_job_bytes(self):
        for block in (web.LEGACY_JOB,web.adapted_job()):
            text = 'jobs:\n'+block+'  rust_compile:\n    run: arbitrary unrelated Rust change\n'
            self.assertEqual(web.web_job(text.encode()),block)
        for change in ('node-version: "24"','timeout-minutes: 11','runs-on: [self-hosted, Linux, ARM64]'):
            changed = web.adapted_job().replace('node-version: "22.23.4"',change) if change.startswith('node-version') else (
                web.adapted_job().replace('timeout-minutes: 10',change) if change.startswith('timeout') else
                web.adapted_job().replace('runs-on: [self-hosted, Linux, X64, lab, general]',change))
            with mock.patch.object(web,'git',return_value=('jobs:\n'+changed).encode()):
                with self.assertRaisesRegex(receipts.ReceiptError,'job/command/environment shape'):
                    web.workflow_identity(ROOT,COMMIT)
        self.assertIn('make web-unit-check',web.COMMANDS)
        self.assertIn('GITHUB_TOKEN: ${{ github.token }}',web.WRAPPER)
        self.assertNotIn('make web-unit-check',web.WRAPPER)

    def test_current_node_provisioning_matches_exact_runtime_and_legacy_stays_historical(self):
        self.assertEqual(web.LEGACY_JOB.count('node-version: "22"'),1)
        self.assertEqual(web.adapted_job().count('node-version: "22.23.4"'),1)
        self.assertNotIn('node-version: "22"',web.adapted_job())
        self.assertEqual(web.web_job((ROOT/web.WORKFLOW).read_bytes()),web.adapted_job())
        for version in ('22','22.23.3'):
            changed=web.adapted_job().replace('node-version: "22.23.4"','node-version: "'+version+'"')
            with self.subTest(version=version),mock.patch.object(web,'git',return_value=('jobs:\n'+changed).encode()):
                with self.assertRaisesRegex(receipts.ReceiptError,'job/command/environment shape'):
                    web.workflow_identity(ROOT,COMMIT)
        observed={'node':'v22.23.4','npm':'10.9.9','git':'git version 2.55.0','make':'GNU Make 4.4'}
        with mock.patch.object(web.sys,'platform','linux'),mock.patch.object(web.platform,'machine',return_value='x86_64'), \
             mock.patch.object(web.subprocess,'check_output',side_effect=lambda command,**kw:observed[command[0]]+'\n'):
            self.assertEqual(web.runtime()['job_recorded'],{k:v for k,v in web.RUNTIME.items() if k!='typescript_lock'})
            observed['node']='v22.23.3'
            with self.assertRaisesRegex(receipts.ReceiptError,'Unsupported web concrete runtime family'):
                web.runtime()

    def test_cli_refusal_diagnostics_expose_only_static_stage_and_allowlisted_reason(self):
        for stage,error,reason in (
            ('runtime',receipts.ReceiptError('Unsupported web concrete runtime family'),'runtime_family_mismatch'),
            ('context',receipts.ReceiptError('private-token-value raw API object'),'receipt_guard_failed'),
            ('context',KeyError('private-token-value'),'metadata_shape_invalid'),
            ('context',receipts.ReceiptError({'token':'private-token-value'}),'receipt_guard_failed'),
            ('runtime',web.subprocess.CalledProcessError(1,['private-token-value']),'required_command_failed')):
            output=io.StringIO()
            with self.subTest(stage=stage,reason=reason),mock.patch.object(web,'current_context',
                    side_effect=error if stage=='context' else None,return_value=object()), \
                 mock.patch.object(web,'runtime',side_effect=error if stage=='runtime' else None), \
                 mock.patch.object(web,'run_gate') as gate,mock.patch.object(web.sys,'stderr',output):
                self.assertEqual(web.main([]),1)
                self.assertEqual(output.getvalue(),'Whole web cohort refused: stage='+stage+' reason='+reason+'; no fallback execution\n')
                self.assertNotIn('private-token-value',output.getvalue());gate.assert_not_called()

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
        for name,mode in (('../escape',stat.S_IFREG|0o644),
                          ('web-cohort.log',stat.S_IFLNK|0o777),
                          ('receipt.json',stat.S_IFREG|0o644)):
            # Exactly two members: each refusal must reach the path/mode/
            # duplicate guard rather than an unrelated member-count check.
            data=packed({},log,log_name=name,log_mode=mode)
            with zipfile.ZipFile(io.BytesIO(data)) as archive:self.assertEqual(len(archive.infolist()),2)
            with self.subTest(name=name,mode=mode),self.assertRaisesRegex(receipts.ReceiptError,'Unsafe/duplicate'):
                web.unpack(data)
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
             mock.patch.object(web,'live_inputs'),mock.patch.object(web,'live_script_resolvers'),mock.patch.object(web,'manifest',return_value=inputs), \
             mock.patch.object(web,'matching_result',return_value=None),mock.patch.object(web,'current_context',return_value=context), \
             mock.patch.object(web,'git',side_effect=lambda workspace,*args,**kw:b'' if args[0]=='diff' else COMMIT.encode() if args[0]=='rev-parse' and args[1]=='HEAD' else b'd'*40), \
             mock.patch.object(web.subprocess,'run',return_value=mock.Mock(returncode=0)) as launch, \
             mock.patch.object(web.subprocess,'check_output',side_effect=lambda argv,**kw:('cpython\n3.12.3\n'+kw['env']['PYTHONPYCACHEPREFIX']+'\n1\n') if argv[0]=='python3' else 'Version 6.0.3\n'), \
             mock.patch.dict(os.environ,{'GITHUB_TOKEN':'secret','GH_TOKEN':'secret','ACTIONS_RUNTIME_TOKEN':'secret','PATH':'/tools','TMPDIR':'/scratch','PYTHONPYCACHEPREFIX':'/untrusted/cache','PYTHONDONTWRITEBYTECODE':'0'}):
            # Different policy/input key selects explicit fresh, never a legacy re-run.
            out=Path(directory)/'fresh';out.mkdir()
            with mock.patch.object(web,'manifest',side_effect=[inputs,{**inputs,'different':True},inputs,inputs]):
                result=web.run_gate(ROOT,context,out,{'typescript':{'declared_locked':'6.0.3','current_binary_observed':None,'current_qualification_execution':False}},True)
            self.assertTrue(result['current_test_events']);self.assertIsNone(result['origin'])
            self.assertEqual(result['python_cache_execution']['cache_policy'],web.PYTHON_CACHE_POLICY)
            self.assertNotIn(str(out),json.dumps(result['python_cache_execution']))
            self.assertFalse((out/'python-cache').exists())
            self.assertEqual([call.args[0][-1] for call in launch.call_args_list],list(web.COMMANDS))
            for call in launch.call_args_list:
                env=call.kwargs['env'];self.assertEqual(env['PATH'],'/tools')
                self.assertFalse({'GITHUB_TOKEN','GH_TOKEN','ACTIONS_RUNTIME_TOKEN'} & set(env))
                self.assertEqual(env['PYTHONPYCACHEPREFIX'],str(out.absolute()/'python-cache'))
                self.assertEqual(env['PYTHONDONTWRITEBYTECODE'],'1')
            self.assertNotIn('PR_BASE_SHA',launch.call_args_list[0].kwargs['env'])
            self.assertEqual(launch.call_args_list[1].kwargs['env']['PR_BASE_SHA'],BASE)
            self.assertNotIn('PR_BASE_SHA',launch.call_args_list[2].kwargs['env'])
            self.assertEqual(launch.call_args_list[-1].kwargs['env']['TMPDIR'],'/tmp')
            launch.reset_mock();launch.return_value=mock.Mock(returncode=1)
            out=Path(directory)/'failed';out.mkdir()
            with mock.patch.object(web,'manifest',side_effect=[inputs,{**inputs,'different':True},inputs,inputs]):
                with self.assertRaisesRegex(receipts.ReceiptError,'failed; no retention/fallback'):
                    web.run_gate(ROOT,context,out,{'typescript':{'declared_locked':'6.0.3','current_binary_observed':None,'current_qualification_execution':False}},True)
            self.assertEqual(launch.call_count,1)
            self.assertNotIn(b'WEB-COHORT-FINISH',(out/'web-cohort.log').read_bytes())
            launch.reset_mock();out=Path(directory)/'unsupported-cache';out.mkdir()
            with mock.patch.object(web,'manifest',side_effect=[inputs,{**inputs,'different':True},inputs]), \
                 mock.patch.object(web.subprocess,'check_output',return_value='cpython\n3.12.3\n/wrong-prefix\n0\n'):
                with self.assertRaisesRegex(receipts.ReceiptError,'cache isolation is unsupported'):
                    web.run_gate(ROOT,context,out,{'typescript':{}},True)
            launch.assert_not_called()

    def test_retention_emits_original_log_and_zero_current_web_events(self):
        api,pull,event,run,job=self.context();context=(api,SCOPE,COMMIT,run,job,BASE)
        inputs={'files':[]};key=web.execution_key(inputs);raw=b'original raw whole-job execution\n'
        original={'kind':'legacy','input_key':key,'log_sha256':web.digest(raw)}
        scratch=ROOT/'target/web-retention-fixtures';scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory, \
             mock.patch.object(web,'workflow_identity',return_value={'kind':'v1','sha256':'a'*64}), \
             mock.patch.object(web,'live_inputs'),mock.patch.object(web,'live_script_resolvers'),mock.patch.object(web,'manifest',return_value=inputs), \
             mock.patch.object(web,'legacy_result',return_value=(original,raw)), \
             mock.patch.object(web,'current_context',return_value=context), \
             mock.patch.object(web,'git',side_effect=lambda workspace,*args,**kw:b'' if args[0]=='diff' else COMMIT.encode() if args[0]=='rev-parse' and args[1]=='HEAD' else b'd'*40), \
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
                'policy_compatible':{'python':web.PYTHON_POLICY},
                'current_diagnostics':{'python':{'implementation':'CPython','version':'3.12.3'}},
                'typescript':{'declared_locked':'6.0.3','current_binary_observed':'Version 6.0.3','current_qualification_execution':True}},
            'python_cache_execution':{'implementation':'cpython','version':'3.12.3','cache_policy':web.PYTHON_CACHE_POLICY},
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
        receipt.update(mode='retained_prior_execution',origin=origin,current_test_events=False,python_cache_execution=None,log_sha256=web.digest(raw))
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

    def test_checkout_runtime_sections_accept_real_two_git_observations_and_reject_conflicts(self):
        # Sanitized section/order fixture reconstructed from complete job 57771:
        # initialization Git, final checkout HEAD, setup-node environment, then
        # checkout Post Git and completion. The private 595,925-byte log stays out.
        action='https://data.forgejo.org/actions/checkout@11d5960a326750d5838078e36cf38b85af677262'
        lines=[
            'Runner 00000000-0000-0000-0000-000000000001 (version:v13.1.0) received task 23006 of job web_compile, triggered by event: pull_request',
            'workflow prepared','⭐ Run Main '+action,'Syncing repository: example/project',
            '##[group]Getting Git version info',"Working directory is '/work/project'",
            '[command]/usr/bin/git version','git version 2.55.0','##[endgroup]',
            '[command]/usr/bin/git init /work/project',
            '[command]/usr/bin/git log -1 --format=%H',COMMIT,
            '##[group]Removing auth','##[endgroup]','::remove-matcher owner=checkout-git::',
            'Found in cache @ /tools/node/22.23.4/x64','##[group]Environment details',
            'node: v22.23.4','npm: 10.9.9','yarn: ','##[endgroup]',
            'js-check: 96 served scripts OK','PASS jsconfig.json',
            'web-types: installing typescript 6.0.3','web-types: OK, unchanged',
            'PASS the Player typedef','TAP version 13','ok 1 - complete web cohort',
            '# fail 0','# cancelled 0','# skipped 0',
            '⭐ Run Post https://data.forgejo.org/actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020',
            '  ✅  Success - Post https://data.forgejo.org/actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020',
            '⭐ Run Post '+action,'[command]/usr/bin/git version','git version 2.55.0',
            '[command]/usr/bin/git config --local --name-only --get-regexp core.sshCommand',
            '  ✅  Success - Post '+action,'🏁  Job succeeded']
        def raw(values):return ('\n'.join('2026-10-25T23:27:07.0000000Z '+line for line in values)+'\n').encode()
        job={'task_id':23006}
        self.assertEqual(web.checkout(raw(lines),COMMIT,job),lines)
        versions=[i for i,line in enumerate(lines) if line == 'git version 2.55.0']
        self.assertEqual(len(versions),2)
        for index in versions:
            conflicting=lines.copy();conflicting[index]='git version 2.54.0'
            with self.subTest(index=index),self.assertRaisesRegex(receipts.ReceiptError,'Git runtime sections differ'):
                web.checkout(raw(conflicting),COMMIT,job)
        for changed in (lines[:versions[1]]+lines[versions[1]+1:],
                        lines[:-1]+['git version 2.55.0',lines[-1]],
                        [line for line in lines if line != '⭐ Run Post '+action]):
            with self.assertRaisesRegex(receipts.ReceiptError,'runtime section|Git runtime sections'):
                web.checkout(raw(changed),COMMIT,job)

    def test_reviewed_raw_shape_families_keep_distinct_keys_and_pin_imported_helpers(self):
        contents,roots,git,shape=self.source()
        with mock.patch.object(web,'git',side_effect=git),mock.patch.object(web,'SCRIPTS_RESOLVER_FAMILIES',{'fixture':git.resolver_layout_sha256}),mock.patch.object(web,'SHAPE_FAMILIES',{'old':shape}):
            original=web.manifest(ROOT,COMMIT,BASE)
        frozen=copy.deepcopy(original);raw=contents['tests/web/check.test.js']+b'// reviewed new body\n'
        shaped=[row.copy() for row in original['files'] if web.shape_row(row)]
        for row in shaped:
            if row['path']=='tests/web/check.test.js':
                row.update(blob=hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest(),sha256=web.digest(raw))
        newer=web.digest(web.canonical(shaped))
        with mock.patch.object(web,'git',side_effect=git),mock.patch.object(web,'SCRIPTS_RESOLVER_FAMILIES',{'fixture':git.resolver_layout_sha256}),mock.patch.object(web,'SHAPE_FAMILIES',{'old':shape,'new':newer}):
            old=web.manifest(ROOT,COMMIT,BASE)
            contents['tests/web/check.test.js']=raw
            current=web.manifest(ROOT,COMMIT,BASE)
            self.assertNotEqual(web.execution_key(old),web.execution_key(current))
            self.assertEqual(original,frozen)
            self.assertNotEqual(old['files'],current['files'])
            for helper in ('tests/vendor/acorn.js','scripts/playback-startup-lab'):
                original_helper=contents[helper];contents[helper] += b'require("outside-closure")\n'
                with self.subTest(helper=helper),self.assertRaisesRegex(receipts.ReceiptError,'executable/recipe/dependency shape'):
                    web.manifest(ROOT,COMMIT,BASE)
                contents[helper]=original_helper

    def test_terminal_failed_nonproducer_history_is_classified_before_source_compatibility(self):
        api,pull,event,run,job=self.context()
        state={'artifacts':[],'jobs':[job]}
        def pages(path,query=None,field=None):
            return state['jobs'] if path.endswith('/jobs') else state['artifacts'] if path.endswith('/artifacts') else [run]
        api.pages=pages
        with mock.patch.object(web,'git',return_value=b'validation.web_cohort_retention unsupported old wrapper/adapter') as source, \
             mock.patch.object(web,'manifest') as manifest,mock.patch.object(web,'v1_result') as result, \
             mock.patch.object(web.subprocess,'run') as launch,mock.patch('builtins.print') as report:
            for status in ('failure','cancelled'):
                job.update(status=status,attempt=1)
                self.assertIsNone(web.matching_result(api,SCOPE,ROOT,COMMIT,7000,'new key'))
                report.assert_called_with('Web history: no accepted whole-cohort result; execution unknown',flush=True)
            source.assert_not_called();manifest.assert_not_called();result.assert_not_called();launch.assert_not_called()
            for change in ({'attempt':0},{'attempt':2},{'attempt':True},{'task_id':0},{'repo_id':2},{'run_id':1},{'id':0}):
                saved=job.copy();job.update(change)
                with self.subTest(change=change),self.assertRaisesRegex(receipts.ReceiptError,'repository/run/task/attempt'):
                    web.matching_result(api,SCOPE,ROOT,COMMIT,7000,'new key')
                job.clear();job.update(saved)
            for status in ('success','running','queued'):
                job['status']=status
                with self.subTest(status=status),self.assertRaisesRegex(receipts.ReceiptError,'Web job shape missing/ambiguous'):
                    web.matching_result(api,SCOPE,ROOT,COMMIT,7000,'new key')
            job['status']='failure'
            for artifacts in ([{'name':'web-cohort-6000-1'}],[{'name':'web-cohort-malformed'}]):
                state['artifacts']=artifacts
                with self.subTest(artifacts=artifacts),self.assertRaisesRegex(receipts.ReceiptError,'Web job shape missing/ambiguous|artifact name malformed'):
                    web.matching_result(api,SCOPE,ROOT,COMMIT,7000,'new key')
            state['artifacts']=[{'name':None}]
            with self.assertRaisesRegex(receipts.ReceiptError,'artifact listing has malformed names'):
                web.matching_result(api,SCOPE,ROOT,COMMIT,7000,'new key')
            state['artifacts']=[];state['jobs']=[job,job.copy()]
            with self.assertRaisesRegex(receipts.ReceiptError,'Web job shape missing/ambiguous'):
                web.matching_result(api,SCOPE,ROOT,COMMIT,7000,'new key')
            launch.assert_not_called();result.assert_not_called()

    def test_history_excludes_pre_v1_but_missing_matching_v1_never_becomes_fresh(self):
        api,pull,event,run,job=self.context();job['status']='success'
        inputs={'files':[]};key=web.execution_key(inputs)
        state={'workflow':('jobs:\n'+web.LEGACY_JOB).encode(),'artifacts':[]}
        def pages(path,query=None,field=None):
            if path.endswith('/artifacts'):return state['artifacts']
            if path.endswith('/jobs'):return [job]
            return [run]
        api.pages=pages
        def git(workspace,*args,**kwargs):
            return state['workflow'] if args[-1].endswith(':'+web.WORKFLOW) else b'reviewed adapter'
        with mock.patch.object(web,'git',side_effect=git),mock.patch.object(web,'manifest',return_value=inputs) as manifest, \
             mock.patch.object(web,'v1_result',return_value=({'kind':'v1_fresh'},b'original log')) as result:
            self.assertIsNone(web.matching_result(api,SCOPE,ROOT,COMMIT,7000,key))
            manifest.assert_not_called();result.assert_not_called()
            state['workflow']=('jobs:\n'+web.adapted_job()).encode()
            with self.assertRaisesRegex(receipts.ReceiptError,'artifact missing/invalid/ambiguous'):
                web.matching_result(api,SCOPE,ROOT,COMMIT,7000,key)
            artifact={'id':7001,'name':'web-cohort-6000-1','run_id':6000,'expired':False}
            for artifacts in ([{**artifact,'expired':True}],[artifact,artifact]):
                state['artifacts']=artifacts
                with self.subTest(artifacts=artifacts),self.assertRaisesRegex(receipts.ReceiptError,'artifact missing/invalid/ambiguous'):
                    web.matching_result(api,SCOPE,ROOT,COMMIT,7000,key)
            state['artifacts']=[artifact]
            self.assertEqual(web.matching_result(api,SCOPE,ROOT,COMMIT,7000,key),({'kind':'v1_fresh'},b'original log'))
            state['workflow']=state['workflow'].replace(b'node-version: "22.23.4"',b'node-version: "24"')
            with self.assertRaisesRegex(receipts.ReceiptError,'job/command/environment shape'):
                web.matching_result(api,SCOPE,ROOT,COMMIT,7000,key)

    def test_script_resolver_namespace_refuses_socket_packages_and_node_precedence_but_allows_cache(self):
        contents,roots,git,shape=self.source()
        with mock.patch.object(web,'git',side_effect=git), \
             mock.patch.object(web,'SCRIPTS_RESOLVER_FAMILIES',{'fixture':git.resolver_layout_sha256}):
            self.assertEqual(web.script_resolver_layout(ROOT,COMMIT),git.resolver_rows)
            for name,mode,kind in (('socket.py','100644','blob'),('socket','040000','tree'),
                                   ('playback-frame-clock','100644','blob')):
                git.resolver_rows.append({'name':name,'mode':mode,'kind':kind})
                with self.subTest(tracked=name),self.assertRaisesRegex(receipts.ReceiptError,'script resolver namespace'):
                    web.script_resolver_layout(ROOT,COMMIT)
                git.resolver_rows.pop()
            scratch=ROOT/'target/web-retention-fixtures';scratch.mkdir(parents=True,exist_ok=True)
            with tempfile.TemporaryDirectory(dir=scratch) as directory:
                workspace=Path(directory);scripts=workspace/'scripts';scripts.mkdir()
                unrelated=scripts/'unrelated-tool';unrelated.write_text('#!/bin/sh\n');unrelated.chmod(0o755)
                web.live_script_resolvers(workspace,COMMIT)
                unrelated.write_text('#!/bin/sh\n# changed unrelated body\n')
                web.live_script_resolvers(workspace,COMMIT)
                cache=scripts/'__pycache__';cache.mkdir();(cache/'generated.pyc').write_bytes(b'generated cache fixture')
                web.live_script_resolvers(workspace,COMMIT)
                for name in ('socket.py','playback-frame-clock'):
                    selected=scripts/name;selected.write_text('untracked selector\n')
                    with self.subTest(live=name),self.assertRaisesRegex(receipts.ReceiptError,'live web script resolver namespace'):
                        web.live_script_resolvers(workspace,COMMIT)
                    selected.unlink()
                package=scripts/'socket';package.mkdir();(package/'__init__.py').write_text('# shadow\n')
                with self.assertRaisesRegex(receipts.ReceiptError,'live web script resolver namespace'):
                    web.live_script_resolvers(workspace,COMMIT)
        for name in ('tests/playback/socket.py','tests/playback/socket/__init__.py'):
            contents[name]=b'# unsupported local resolver\n'
            with mock.patch.object(web,'git',side_effect=git), \
                 mock.patch.object(web,'SCRIPTS_RESOLVER_FAMILIES',{'fixture':git.resolver_layout_sha256}), \
                 mock.patch.object(web,'SHAPE_FAMILIES',{'fixture':shape}):
                with self.subTest(playback=name),self.assertRaisesRegex(receipts.ReceiptError,'executable/recipe/dependency shape'):
                    web.manifest(ROOT,COMMIT,BASE)
            del contents[name]
            with mock.patch.object(web,'selectors'),mock.patch.object(web,'git',return_value=(name+'\0').encode()):
                with self.assertRaisesRegex(receipts.ReceiptError,'Untracked/ignored web closure'):
                    web.live_inputs(ROOT)

    def test_production_executable_and_loader_shapes_refuse_outside_dependencies_before_fallback(self):
        contents,roots,git,shape=self.source()
        api,pull,event,run,job=self.context();context=(api,SCOPE,COMMIT,run,job,BASE)
        cases={
            'crates/plurxd/src/web/playback-policy.js':b'module.exports=require("../../../../outside.js");\n',
            'crates/plurxd/src/web/index.html':b'<script src="../../outside.js"></script>\n',
            'crates/plurxd/src/web/jsconfig.json':b'{"files":["../../outside.ts"]}\n',
            'crates/plurxd/src/web/globals.d.ts':b'/// <reference path="../../outside.d.ts" />\n',
        }
        with mock.patch.object(web,'git',side_effect=git), \
             mock.patch.object(web,'SCRIPTS_RESOLVER_FAMILIES',{'fixture':git.resolver_layout_sha256}), \
             mock.patch.object(web,'SHAPE_FAMILIES',{'fixture':shape}), \
             mock.patch.object(web,'workflow_identity',return_value={'kind':'v1'}), \
             mock.patch.object(web,'live_inputs'),mock.patch.object(web,'live_script_resolvers'), \
             mock.patch.object(web,'legacy_result') as legacy,mock.patch.object(web,'matching_result') as history, \
             mock.patch.object(web.subprocess,'run') as launch:
            for path,raw in cases.items():
                original=contents[path];contents[path]=raw
                with self.subTest(path=path),self.assertRaisesRegex(receipts.ReceiptError,'executable/recipe/dependency shape'):
                    web.run_gate(ROOT,context,ROOT/'target/web-retention-never-created',{},True)
                contents[path]=original
            for suffix in ('.js','.mjs','.cjs','.ts','.d.ts','.html','.json'):
                path='crates/plurxd/src/web/new-loader'+suffix;contents[path]=b'new unreviewed member'
                with self.subTest(suffix=suffix),self.assertRaisesRegex(receipts.ReceiptError,'executable/recipe/dependency shape'):
                    web.run_gate(ROOT,context,ROOT/'target/web-retention-never-created',{},True)
                del contents[path]
            legacy.assert_not_called();history.assert_not_called();launch.assert_not_called()

    def test_all_test_members_refuse_bytecode_native_extensions_packages_and_node_selectors(self):
        contents,roots,git,shape=self.source()
        api,pull,event,run,job=self.context();context=(api,SCOPE,COMMIT,run,job,BASE)
        paths=('tests/playback/socket.pyc','tests/playback/socket.cpython-312-x86_64-linux-gnu.so',
            'tests/playback/socket/__init__.pyc','tests/playback/socket/__init__.so',
            'tests/playback/__pycache__/native-hls-browser-fixture.cpython-312.pyc',
            'tests/web/socket.node','tests/web/shell-source','tests/playback/new-package/package.json')
        with mock.patch.object(web,'git',side_effect=git), \
             mock.patch.object(web,'SCRIPTS_RESOLVER_FAMILIES',{'fixture':git.resolver_layout_sha256}), \
             mock.patch.object(web,'SHAPE_FAMILIES',{'fixture':shape}), \
             mock.patch.object(web,'workflow_identity',return_value={'kind':'v1'}), \
             mock.patch.object(web,'live_inputs'),mock.patch.object(web,'live_script_resolvers'), \
             mock.patch.object(web,'legacy_result') as legacy,mock.patch.object(web,'matching_result') as history, \
             mock.patch.object(web.subprocess,'run') as launch:
            for path in paths:
                contents[path]=b'unreviewed committed resolver input'
                with self.subTest(path=path),self.assertRaisesRegex(receipts.ReceiptError,'executable/recipe/dependency shape'):
                    web.run_gate(ROOT,context,ROOT/'target/web-retention-never-created',{},True)
                del contents[path]
            legacy.assert_not_called();history.assert_not_called();launch.assert_not_called()

    def test_node_package_scopes_pin_absence_and_reviewed_bytes_in_git_and_live_checkout(self):
        contents,roots,git,shape=self.source()
        candidates=('package.json','tests/package.json','tests/vendor/package.json','scripts/package.json',
            'scripts/cq-native-shipping/package.json','crates/package.json','crates/plurxd/package.json',
            'crates/plurxd/src/package.json','tests/web/package.json','tests/playback/package.json')
        with mock.patch.object(web,'git',side_effect=git):
            self.assertTrue(all(row['identity'] is None for row in web.node_package_scope(ROOT,COMMIT)))
            for path in candidates:
                self.assertIn(path,web.NODE_PACKAGE_SCOPES);contents[path]=b'{"main":"../../outside.js"}'
                with self.subTest(committed=path),self.assertRaisesRegex(receipts.ReceiptError,'Node package scope'):
                    web.node_package_scope(ROOT,COMMIT)
                del contents[path]
            path='tests/vendor/package.json';raw=b'{"type":"commonjs"}\n';contents[path]=raw
            identity={'mode':'100644','blob':hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest(),
                'sha256':web.digest(raw)}
            with mock.patch.object(web,'NODE_PACKAGE_PINS',{path:identity}):
                self.assertEqual(next(row['identity'] for row in web.node_package_scope(ROOT,COMMIT) if row['path']==path),identity)
                contents[path]+=b' changed'
                with self.assertRaisesRegex(receipts.ReceiptError,'Node package scope'):web.node_package_scope(ROOT,COMMIT)
            del contents[path]
        scratch=ROOT/'target/web-retention-fixtures';scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            workspace=Path(directory);web.live_node_packages(workspace)
            for relative in candidates:
                path=workspace/relative;path.parent.mkdir(parents=True,exist_ok=True);path.write_text('{"main":"outside.js"}')
                with self.subTest(live=relative),self.assertRaisesRegex(receipts.ReceiptError,'live web Node package scope'):
                    web.live_node_packages(workspace)
                path.unlink()
            external=workspace/'external.json';external.write_text('{}')
            (workspace/'package.json').symlink_to(external)
            with self.assertRaisesRegex(receipts.ReceiptError,'package scope type'):web.live_node_packages(workspace)

    def test_live_cache_allowance_is_bounded_regular_and_never_exempts_parent_packages(self):
        scratch=ROOT/'target/web-retention-fixtures';scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            workspace=Path(directory);cache=workspace/'tests/playback/__pycache__';cache.mkdir(parents=True)
            cached=cache/'generated.cpython-312.pyc';cached.write_bytes(b'opaque cache bytes, never certified')
            relative=cached.relative_to(workspace).as_posix();before=cached.read_bytes()
            with mock.patch.object(web,'selectors'),mock.patch.object(web,'git',return_value=(relative+'\0').encode()):
                web.live_inputs(workspace)
                self.assertEqual(cached.read_bytes(),before)
            for bad in ('socket.pyc','socket.platform.so','socket/__pycache__/module.pyc'):
                path='tests/playback/'+bad
                with mock.patch.object(web,'selectors'),mock.patch.object(web,'git',return_value=(path+'\0').encode()):
                    with self.subTest(path=path),self.assertRaisesRegex(receipts.ReceiptError,'Untracked/ignored web closure'):
                        web.live_inputs(workspace)
            unwanted=cache/'package.json';unwanted.write_text('{}')
            with self.assertRaisesRegex(receipts.ReceiptError,'cache member/bound'):web.live_cache_paths(workspace)
            unwanted.unlink();target=workspace/'outside.pyc';target.write_bytes(b'outside')
            alias=cache/'alias.pyc';alias.symlink_to(target)
            with self.assertRaisesRegex(receipts.ReceiptError,'cache member/bound'):web.live_cache_paths(workspace)
            alias.unlink()
            with mock.patch.object(web,'MAX_CACHE_FILES',0):
                with self.assertRaisesRegex(receipts.ReceiptError,'cache member/bound'):web.live_cache_paths(workspace)

    def test_fresh_python_isolation_ignores_unchecked_cache_and_preserves_its_bytes(self):
        import py_compile
        import subprocess
        import sys
        scratch=ROOT/'target/web-retention-fixtures';scratch.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            workspace=Path(directory);source=workspace/'selected_fixture.py';source.write_text('VALUE="unchecked-cache"\n')
            cache=workspace/'__pycache__'/('selected_fixture.'+sys.implementation.cache_tag+'.pyc')
            py_compile.compile(str(source),cfile=str(cache),doraise=True,
                invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)
            before=cache.read_bytes();self.assertEqual(int.from_bytes(before[4:8],'little') & 3,1)
            source.write_text('VALUE="source"\n');output=workspace/'owned-output';output.mkdir(mode=0o700)
            prefix=output/'python-cache';facts=web.verify_fresh_cache(workspace,prefix)
            self.assertEqual(facts['cache_policy'],web.PYTHON_CACHE_POLICY)
            env=web.fresh_environment(prefix)
            code='import sys;sys.path.insert(0,sys.argv[1]);import selected_fixture;print(selected_fixture.VALUE)'
            result=subprocess.check_output([sys.executable,'-c',code,str(workspace)],cwd=workspace,text=True,env=env).strip()
            self.assertEqual(result,'source');self.assertEqual(cache.read_bytes(),before)
            self.assertFalse(prefix.exists());self.assertNotIn(str(prefix),json.dumps(facts))
