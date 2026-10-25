"""Synthetic authentication/closure/native-log refusals; never runs Swift/make."""
import copy
import hashlib
import io
import json
from pathlib import Path
import stat
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock
import urllib.error
import zipfile

from validation import desktop_macos_retention as native

ROOT = Path(__file__).resolve().parents[2]
CURRENT = 'b' * 40
RUN = 6000
SCOPE = {'repository':1,'pr':1263,'branch':'native-topic','base':'main','workflow':'main-fast-lane.yml'}
WORK = '/work/native'


def source_cases():
    return {'clients/server-tray/macos/Tests/'+module+'/'+cls+'.swift':
        ('final class '+cls+': XCTestCase {\n'+''.join('    func testCase%03d() {\n    }\n' % i for i in range(count))+'}\n').encode()
        for module,cls,count in [('PlurxTrayCoreTests','CoreCases',61),('PlurxTrayUITests','UICases',14)]}


def native_log(expected, workspace=WORK):
    lines = []
    for arch in ('arm64','x86_64'):
        lines += ['build: swift build -c release --arch '+arch,'Building for production...',
            'Build complete! (1.00 sec)',workspace+'/target/desktop-macos/'+arch+'/Plurx Server.app']
    lines += ['swift test --package-path clients/server-tray/macos '+chr(92)*2,
        '\t  --scratch-path "'+workspace+'/target/desktop-macos/tests"','Building for debugging...',
        'Build complete! (1.00 sec)']
    for module in ('PlurxTrayUITests','PlurxTrayCoreTests'):
        suite = module+'.xctest'; names = sorted(k for k,v in expected.items() if v['module'] == module)
        def suite_line(name, outcome): return "Test Suite '"+name+"' "+outcome+' at 2026-10-25 12:00:00.000.'
        def summary(count): return '\t Executed '+str(count)+' tests, with 0 failures (0 unexpected) in 0.001 (0.002) seconds'
        lines += [suite_line('All tests','started'),suite_line(suite,'started')]
        for cls in sorted({expected[k]['class'] for k in names}):
            cases = [k for k in names if expected[k]['class'] == cls]
            lines.append(suite_line(cls,'started'))
            for key in cases:
                record = "Test Case '-["+module+'.'+cls+' '+expected[key]['method']+"]' "
                lines += [record+'started.',record+'passed (0.001 seconds).']
            lines += [suite_line(cls,'passed'),summary(len(cases))]
        lines += [suite_line(suite,'passed'),summary(len(names)),suite_line('All tests','passed'),summary(len(names))]
    lines += ['◇ Test run started.','↳ Testing Library Version: 2084','↳ Target Platform: arm64e-apple-macos14.0',
        '✔ Test run with 0 tests in 0 suites passed after 0.001 seconds.']*2
    return ('\n'.join(lines)+'\n').encode()


def archive(payload, name=native.MEMBER, mode=stat.S_IFREG | 0o644, padded=False):
    output = io.BytesIO()
    with zipfile.ZipFile(output,'w',compression=zipfile.ZIP_DEFLATED) as z:
        info = zipfile.ZipInfo(name); info.external_attr = mode << 16
        z.writestr(info,payload,compress_type=zipfile.ZIP_DEFLATED)
        if padded:
            remaining = 3930 - (output.tell()+22)
            assert remaining >= 0
            z.comment = b'x'*remaining
    return output.getvalue()


def run_record(identity, commit, scope=SCOPE):
    pull = {'number':scope['pr'],'state':'open','draft':False,
        'head':{'ref':scope['branch'],'sha':commit,'repo':{'id':scope['repository']}},
        'base':{'ref':'main','sha':'a'*40,'repo':{'id':scope['repository']}}}
    event = {'number':scope['pr'],'action':'synchronize','repository':{'id':scope['repository'],'full_name':'example/project'},'pull_request':pull}
    return {'id':identity,'workflow_id':'main-fast-lane.yml','repository':event['repository'],
        'prettyref':'#'+str(scope['pr']),'event':'pull_request','commit_sha':commit,
        'status':'failure','event_payload':json.dumps(event)}


def job_log(commit=native.PRODUCER, job_id=native.ORIGIN_JOB, task=12, ending='🏁  Job succeeded', archive_sha='0'*64):
    lines = ['Runner 00000000-0000-0000-0000-000000000001 (version:v1) received task '+str(task)+' of job apple_compile, triggered by event: pull_request',
        "Deleting the contents of '"+WORK+"'",'[command]/usr/bin/git init '+WORK,
        '[command]/usr/bin/git -c protocol.version=2 fetch --no-tags --prune --no-recurse-submodules --depth=1 origin +'+commit+':refs/remotes/pull/1263/head',
        '[command]/usr/bin/git checkout --progress --force refs/remotes/pull/1263/head',
        '[command]/usr/bin/git log -1 --format=%H',commit,*native.TOOLCHAIN,native.SWIFT,
        'Native macOS server tray build and unit checks passed.',
        'SHA256 hash of uploaded artifact zip is '+archive_sha,
        'Artifact desktop-macos-check-5943-1 has been successfully uploaded! Final size is 3930 bytes. Artifact ID is 4225']
    stamp = '2026-10-25T12:00:00.0000000Z '
    body = ''.join(stamp+line+'\n' for line in lines)
    tail = stamp+ending+'\n'
    padding = 14852-len((body+stamp+'\n'+tail).encode())
    assert padding >= 0
    return (body+stamp+'x'*padding+'\n'+tail).encode()


class FakeAPI:
    def __init__(self, raw=None):
        self.requests = 0; self.root = 'https://ci.example/api/v1/repos/example/project'; self.token = 'secret-not-for-children'
        self.runs = {5943:run_record(5943,native.PRODUCER),RUN:run_record(RUN,CURRENT)}
        self.jobs = {5943:[{'id':57583,'run_id':5943,'attempt':1,'repo_id':1,'name':native.JOB,
            'runs_on':native.LABELS,'task_id':12,'status':'success'}],RUN:[{'id':60001,'run_id':RUN,
            'attempt':1,'repo_id':1,'name':native.JOB,'runs_on':native.LABELS,'task_id':13,'status':'running'}]}
        self.artifacts = [{'id':4225,'run_id':5943,'name':'desktop-macos-check-5943-1','size_in_bytes':3930,'expired':False}]
        self.raw = raw
    def get(self,path,query=None):
        if path == '': return {'id':1}
        if path == '/actions/runs':
            rows = list(self.runs.values());page = (query or {}).get('page',1);limit = (query or {}).get('limit',25)
            return {'workflow_runs':copy.deepcopy(rows[(page-1)*limit:page*limit])}
        if path.startswith('/pulls/'): return json.loads(self.runs[RUN]['event_payload'])['pull_request']
        return copy.deepcopy(self.runs[int(path.rsplit('/',1)[1])])
    def pages(self,path,query=None,field=None):
        if path == '/actions/runs': return copy.deepcopy(list(self.runs.values()))
        if path == '/actions/artifacts': return copy.deepcopy(self.artifacts)
        return copy.deepcopy(self.jobs[int(path.split('/')[3])])


class DesktopMacosRetentionContracts(unittest.TestCase):
    def setUp(self):
        self.contents = source_cases(); self.expected = native.case_inventory(self.contents)
        self.native = native_log(self.expected); self.zip = archive(self.native,padded=True)
        self.log = job_log(archive_sha=native.digest(self.zip)); self.api = FakeAPI(self.log)
        self.entries = [{'path':'native-source','mode':'100644','blob':'1'*40,'sha256':'2'*64}]
        self.policy = mock.patch.multiple(native,ORIGIN_ZIP_SHA=native.digest(self.zip),ORIGIN_JOB_SHA=native.digest(self.log))
        self.policy.start(); self.addCleanup(self.policy.stop)
    def retained(self):
        with mock.patch.object(native,'download_log',return_value=self.api.raw), \
             mock.patch.object(native.transport,'archive_bytes',return_value=self.zip), \
             mock.patch.object(native,'manifest',return_value=(self.entries,self.contents)), \
             mock.patch.object(native,'git',return_value=native.PRODUCER_TREE.encode()):
            return native.legacy_result(self.api,SCOPE,ROOT,CURRENT,RUN)

    def test_authenticated_failed_overall_run_retains_exact_75_names_and_both_builds(self):
        result = self.retained()
        self.assertEqual(result['run']['status'],'failure')
        self.assertEqual(result['observed']['module_counts'],{'PlurxTrayCoreTests':61,'PlurxTrayUITests':14})
        self.assertEqual(set(result['observed']['started_once']),set(self.expected))
        self.assertEqual(result['observed']['started_once'],result['observed']['passed_once'])
        self.assertEqual(result['observed']['release_architectures'],['arm64','x86_64'])

    def test_wrong_origin_head_pr_attempt_failed_job_or_metadata_refuses(self):
        changes = [lambda a:a.runs[5943].update(commit_sha='c'*40),
            lambda a:a.runs[5943].update(prettyref='#5'),
            lambda a:a.jobs[5943][0].update(attempt=2),lambda a:a.jobs[5943][0].update(status='failure'),
            lambda a:a.jobs[5943][0].update(repo_id=2),lambda a:a.artifacts[0].update(expired=True),
            lambda a:a.artifacts[0].update(id=4226),lambda a:a.artifacts[0].update(size_in_bytes=3931),
            lambda a:a.jobs[5943].append(copy.deepcopy(a.jobs[5943][0]))]
        for change in changes:
            self.api = FakeAPI(self.log); change(self.api)
            with self.subTest(change=change),self.assertRaises(native.receipts.ReceiptError): self.retained()

    def test_archive_or_job_log_tamper_is_not_an_adoption_or_fresh_fallback(self):
        original = self.zip
        for value in (b'not-a-zip',original[:-1]+b'!'):
            self.zip = value
            with self.subTest(archive=value[:8]),self.assertRaises(native.receipts.ReceiptError): self.retained()
        self.zip = original;self.api.raw = self.log.replace(b'Xcode 27.1',b'Xcode 27.0')
        with self.assertRaisesRegex(native.receipts.ReceiptError,'job log hash'): self.retained()

    def test_archive_traversal_symlink_duplicate_and_size_bounds_refuse(self):
        values = [archive(b'x','../'+native.MEMBER),archive(b'x',native.MEMBER,stat.S_IFLNK|0o777),
            archive(b'x'*(native.MAX_LOG+1)),b'x'*(native.MAX_ZIP+1)]
        stream = io.BytesIO()
        with zipfile.ZipFile(stream,'w') as z:
            z.writestr(native.MEMBER,b'x');z.writestr('extra',b'x')
        values.append(stream.getvalue())
        for value in values:
            with self.subTest(size=len(value)),self.assertRaises(native.receipts.ReceiptError): native.unpack(value)

    def test_equal_case_counts_with_wrong_identity_or_incomplete_or_failed_log_refuse(self):
        changes = [lambda s:s.replace(b'testCase000',b'testUnknown'),
            lambda s:s.replace(b"passed (0.001 seconds).",b"skipped (0.001 seconds).",1),
            lambda s:s.replace(b"passed (0.001 seconds).",b"failed (0.001 seconds).",1),
            lambda s:s.replace(b"passed (0.001 seconds).",b"",1),lambda s:s[:-90],
            lambda s:s.replace(b'--arch x86_64',b'--arch arm64'),
            lambda s:s.replace(b'Build complete! (1.00 sec)',b'build incomplete',1)]
        for change in changes:
            with self.subTest(change=change),self.assertRaises(native.receipts.ReceiptError):
                native.parse_native(change(self.native),self.expected,WORK)

    def test_unsupported_source_case_shape_or_duplicate_identity_refuses(self):
        for contents in ({'clients/server-tray/macos/Tests/PlurxTrayCoreTests/Bad.swift':b'final class Bad: XCTestCase {\n    func testParameterized(value: Int) {\n}\n'},
            {**self.contents,'clients/server-tray/macos/Tests/PlurxTrayCoreTests/Duplicate.swift':next(iter(self.contents.values()))}):
            with self.assertRaises(native.receipts.ReceiptError): native.case_inventory(contents)

    def test_exact_workflow_delta_and_toolchain_are_required(self):
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        current = native.transformed_workflow(legacy)
        with mock.patch.object(native,'git',return_value=current):
            self.assertEqual(native.workflow_identity(ROOT,CURRENT,legacy),native.digest(current))
        for wrong in (current.replace(b'Xcode 27.1',b'Xcode 27.0'),current.replace(b'--fresh-on-new-inputs',b'--silently-skip'),
            current.replace(b"if: needs.scope.outputs.apple == 'true'",b"if: false")):
            with mock.patch.object(native,'git',return_value=wrong),self.assertRaises(native.receipts.ReceiptError):
                native.workflow_identity(ROOT,CURRENT,legacy)
        env = {'DEVELOPER_DIR':'/Applications/Xcode.app/Contents/Developer','GITHUB_TOKEN':'never-child','PATH':'/tools'}
        with mock.patch.dict(native.os.environ,env,clear=True),mock.patch.object(native.sys,'platform','darwin'):
            with mock.patch.object(native.subprocess,'check_output',side_effect=['Xcode 27.1\nBuild version 27A9275\n',native.SWIFT+'\n']) as tool:
                native.verify_toolchain()
                self.assertTrue(all('GITHUB_TOKEN' not in call.kwargs['env'] for call in tool.call_args_list))
            with mock.patch.object(native.subprocess,'check_output',side_effect=['Xcode 27.0\nBuild version old\n',native.SWIFT]):
                with self.assertRaises(native.receipts.ReceiptError): native.verify_toolchain()

    def test_history_matching_partial_running_missing_or_manual_scope_blocks_fresh(self):
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        key = native.execution_key(self.entries,self.expected)
        with mock.patch.object(native,'manifest',return_value=(self.entries,self.contents)), \
             mock.patch.object(native,'workflow_identity',return_value=native.PRODUCER_WORKFLOW), \
             mock.patch.object(native,'download_log',return_value=self.log):
            for change in (lambda a:None,lambda a:a.jobs[5943][0].update(status='running'),
                lambda a:a.jobs.update({5943:[]}),lambda a:a.runs[5943].update(workflow_id='ci.yml'),
                lambda a:a.runs.pop(5943)):
                api = FakeAPI();change(api)
                with self.subTest(change=change),self.assertRaises(native.receipts.ReceiptError):
                    native.history_before_fresh(api,SCOPE,ROOT,CURRENT,RUN,key,legacy)
            api = FakeAPI();api.jobs[5943][0].update(status='skipped',task_id=0)
            result = native.history_before_fresh(api,SCOPE,ROOT,CURRENT,RUN,key,legacy)
            self.assertEqual(result['attempts'][0]['classification'],'positively_not_started')

    def test_complete_first_run_history_allows_only_the_explicit_fresh_option(self):
        scope = {**SCOPE,'pr':2};api = FakeAPI();api.runs = {RUN:run_record(RUN,CURRENT,scope)}
        with mock.patch.object(native,'manifest',return_value=(self.entries,self.contents)), \
             mock.patch.object(native,'workflow_identity',return_value=native.PRODUCER_WORKFLOW):
            result = native.history_before_fresh(api,scope,ROOT,CURRENT,RUN,'new',b'legacy')
            self.assertTrue(result['pagination_complete']);self.assertEqual(result['attempts'],[])
        api.pages = mock.Mock(side_effect=native.receipts.ReceiptError('pagination truncated'))
        with self.assertRaises(native.receipts.ReceiptError):
            native.history_before_fresh(api,scope,ROOT,CURRENT,RUN,'new',b'legacy')

    def test_duplicate_output_and_symlink_destination_never_enable_a_rerun(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory);out = native.output_directory(root)
            self.assertTrue(out.is_dir())
            with self.assertRaises(native.receipts.ReceiptError): native.output_directory(root)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory);(root/'target').symlink_to(root)
            with self.assertRaises(native.receipts.ReceiptError): native.output_directory(root)

    def test_source_closure_membership_bytes_modes_and_symlinks_are_not_ignored(self):
        required = ['Makefile','Cargo.toml','LICENSE','brand/export/icon.png',
            'clients/server-tray/macos/Package.swift','clients/server-tray/macos/build']
        rows = {path:('100644',path.encode()) for path in required}
        def read_git(workspace,*args,**kwargs):
            if args[:2] == ('cat-file','-t'): return b'commit'
            if args[0] == 'ls-tree':
                return b''.join((mode+' blob '+hashlib.sha1(raw).hexdigest()+'\t'+p).encode()+b'\0' for p,(mode,raw) in rows.items())
            raw = next(raw for mode,raw in rows.values() if hashlib.sha1(raw).hexdigest() == args[-1])
            return str(len(raw)).encode() if args[1] == '-s' else raw
        with mock.patch.object(native,'git',side_effect=read_git):
            original,_ = native.manifest(ROOT,CURRENT)
            rows['clients/server-tray/macos/New.swift'] = ('100644',b'new')
            added,_ = native.manifest(ROOT,CURRENT);self.assertNotEqual(added,original)
            del rows['clients/server-tray/macos/New.swift'];rows['Makefile'] = ('100755',b'Makefile')
            mode_only,_ = native.manifest(ROOT,CURRENT);self.assertNotEqual(mode_only,original)
            rows['Makefile'] = ('100644',b'changed')
            changed,_ = native.manifest(ROOT,CURRENT);self.assertNotEqual(changed,original)
            rows['Makefile'] = ('120000',b'target')
            with self.assertRaises(native.receipts.ReceiptError): native.manifest(ROOT,CURRENT)
            rows['Makefile'] = ('160000',b'submodule')
            with self.assertRaises(native.receipts.ReceiptError): native.manifest(ROOT,CURRENT)
            del rows['Makefile']
            with self.assertRaises(native.receipts.ReceiptError): native.manifest(ROOT,CURRENT)

    def test_head_drift_or_missing_known_evidence_cannot_call_make(self):
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        def git_value(workspace,*args,**kwargs):
            return legacy if args[-1] == native.PRODUCER+':'+native.WORKFLOW else b'identity'
        with tempfile.TemporaryDirectory() as directory,mock.patch.dict(native.os.environ,{'GITHUB_JOB':'apple_compile'}), \
             mock.patch.object(native,'git',side_effect=git_value),mock.patch.object(native,'workflow_identity',return_value=native.digest(native.transformed_workflow(legacy))), \
             mock.patch.object(native,'manifest',return_value=(self.entries,self.contents)),mock.patch.object(native,'current_job',return_value=self.api.jobs[RUN][0]), \
             mock.patch.object(native,'legacy_result',side_effect=native.receipts.ReceiptError('known evidence missing')), \
             mock.patch.object(native,'recheck'),mock.patch.object(native.subprocess,'run') as make:
            with self.assertRaises(native.receipts.ReceiptError):
                native.run_gate(self.api,SCOPE,ROOT,CURRENT,RUN,Path(directory),{},True)
            make.assert_not_called()
        with mock.patch.object(native,'current_context',return_value=(self.api,SCOPE,'c'*40,RUN)):
            with self.assertRaises(native.receipts.ReceiptError): native.recheck(ROOT,CURRENT,RUN)

    def test_child_environment_and_download_errors_do_not_expose_api_tokens(self):
        with mock.patch.dict(native.os.environ,{'GITHUB_TOKEN':'top-secret','GH_TOKEN':'top-secret','DEVELOPER_DIR':'/toolchain','PATH':'/bin'},clear=True):
            clean = native.child_environment()
            self.assertEqual(clean,{'DEVELOPER_DIR':'/toolchain','PATH':'/bin'})
        api = FakeAPI();api.opener = SimpleNamespace(open=mock.Mock(side_effect=urllib.error.HTTPError('secret-url',403,'secret',{},None)))
        with self.assertRaises(native.receipts.ReceiptError) as refused: native.download_log(api,57583)
        self.assertNotIn('secret',str(refused.exception))
        response = mock.MagicMock();response.__enter__.return_value.read.return_value = b'x'*(native.MAX_LOG+1)
        api.opener.open = mock.Mock(return_value=response)
        with self.assertRaises(native.receipts.ReceiptError): native.download_log(api,57583)

    def v1_fixture(self):
        rid,jid,commit,tree = 6100,61001,'c'*40,'d'*40
        key = native.execution_key(self.entries,self.expected)
        start = {'state':'started','commit':commit,'run':rid,'job':jid,'attempt':1,'native_execution_key':key}
        finish = {**start,'state':'finished','exit_code':0}
        attest = {'commit':commit,'tree':tree,'run':rid,'job':jid,'attempt':1,'native_execution_key':key,
            'source_manifest_sha256':native.digest(native.canonical(self.entries)),'tracked_clean':True,'workspace':WORK}
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        receipt = {key:None for key in native.RECEIPT_COMMON}
        receipt.update(schema_version=1,case='desktop-macos-check',current_commit=commit,current_tree=tree,
            current_run=rid,current_job=jid,current_attempt=1,scope=SCOPE,native_execution_key=key,
            current_source_manifest=self.entries,expected_case_ids=sorted(self.expected),
            source_manifest_kind='reconstructed_from_authenticated_checkout',
            toolchain_contract={'xcode_output':native.TOOLCHAIN,'swift_path':native.SWIFT,'host_os_release':None,
                'compiler_binary_sha256':None,'sdk_inventory':None,'unrecorded_reason':'not recorded'},
            adapter_environment={'python':[3,12,15],'platform':'darwin'},producer_workflow_sha256=native.PRODUCER_WORKFLOW,
            consumer_workflow_sha256=native.digest(native.transformed_workflow(legacy)),
            recipe_compatibility='exact legacy workflow plus bounded Apple-only adapter delta v1',
            adapter_sha256=native.digest(b'adapter'),policy_sha256=native.policy_fingerprint(),checkout_attestation=attest,
            decision='fresh_execution',current_native_commands_executed=True,history={'pagination_complete':True},
            observed=native.parse_native(self.native,self.expected,WORK),native_log_sha256=native.digest(self.native),
            execution_origin={'commit':commit,'tree':tree,'run':rid,'job':jid,'attempt':1,
                'native_execution_key':key,'native_log_sha256':native.digest(self.native)})
        payload = {'receipt.json':native.canonical(receipt),'fresh-start.json':native.canonical(start),
            'fresh-finish.json':native.canonical(finish),native.MEMBER:self.native}
        def packed(values):
            out = io.BytesIO()
            with zipfile.ZipFile(out,'w',compression=zipfile.ZIP_DEFLATED) as archive:
                for name,raw in values.items():archive.writestr(name,raw)
            return out.getvalue()
        api = FakeAPI();api.runs[rid] = run_record(rid,commit);api.jobs[rid] = [{**api.jobs[5943][0],
            'id':jid,'run_id':rid,'task_id':22}]
        envelope = {'run':api.runs[rid],'job':api.jobs[rid][0],
            'artifact':{'id':5100,'run_id':rid,'name':'desktop-macos-check-6100-1','size_in_bytes':1,'expired':False},
            'archive':packed(payload),'job_log':b'authenticated-v1-job',
            'lines':['[command]/usr/bin/git log -1 --format=%H',commit,*native.TOOLCHAIN,native.SWIFT,
                'DESKTOP-NATIVE-CHECKOUT '+native.canonical(attest).decode(),
                'DESKTOP-NATIVE-START '+native.canonical(start).decode(),
                'DESKTOP-NATIVE-FINISH '+native.canonical(finish).decode()]}
        return api,envelope,payload,receipt,key,legacy,packed

    def test_v1_fresh_receipt_binds_real_start_finish_and_exact_native_outcomes(self):
        api,envelope,payload,receipt,key,legacy,packed = self.v1_fixture()
        with mock.patch.object(native,'v1_envelope',return_value=envelope), \
             mock.patch.object(native,'manifest',return_value=(self.entries,self.contents)), \
             mock.patch.object(native,'git',side_effect=lambda w,*a,**k:b'd'*40 if a[0]=='rev-parse' else b'adapter'):
            result = native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)
            self.assertEqual(result['key'],key);self.assertEqual(result['observed']['passed_once'],sorted(self.expected))
            changes = [lambda p:p.pop('fresh-finish.json'),
                lambda p:p.__setitem__('fresh-finish.json',native.canonical({**json.loads(p['fresh-finish.json']),'exit_code':1})),
                lambda p:p.__setitem__('receipt.json',native.canonical({**receipt,'policy_sha256':'0'*64})),
                lambda p:p.__setitem__('receipt.json',native.canonical({**receipt,'native_execution_key':'0'*64})),
                lambda p:p.__setitem__('receipt.json',native.canonical({**receipt,'execution_origin':{}}))]
            for change in changes:
                bad = copy.deepcopy(payload);change(bad);envelope['archive'] = packed(bad)
                with self.subTest(change=change),self.assertRaises(native.receipts.ReceiptError):
                    native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)

    def test_v1_retained_resolves_one_real_fresh_origin_and_refuses_chains(self):
        api,envelope,payload,fresh,key,legacy,packed = self.v1_fixture()
        original = {**envelope,'tree':'d'*40,'key':key,'native_log':self.native,
            'manifest':self.entries,'expected':self.expected,'observed':fresh['observed'],'checkout_path':WORK,
            'receipt_member_sha256':native.digest(payload['receipt.json'])}
        retained = copy.deepcopy(fresh)
        for name in ('history','observed','native_log_sha256'):retained.pop(name)
        retained.update(decision='retained_prior_execution',current_native_commands_executed=False,
            current_run=6200,current_job=62001,execution_origin=native.origin_identity(original))
        retained['checkout_attestation'].update(run=6200,job=62001,workspace='/another/checkout')
        metadata = {'run':original['run'],'job':original['job'],'artifact':original['artifact']}
        meta_raw = native.canonical(metadata);retained['attached_metadata_sha256'] = native.digest(meta_raw)
        values = {'receipt.json':native.canonical(retained),'origin-artifact.zip':original['archive'],
            'origin-job.log':original['job_log'],'origin-metadata.json':meta_raw,native.MEMBER:self.native}
        wrapper = {**envelope,'run':{**envelope['run'],'id':6200},
            'job':{**envelope['job'],'id':62001,'run_id':6200},'archive':packed(values),
            'lines':['[command]/usr/bin/git log -1 --format=%H','c'*40,*native.TOOLCHAIN,native.SWIFT,
                'DESKTOP-NATIVE-CHECKOUT '+native.canonical(retained['checkout_attestation']).decode()]}
        real_v1 = native.v1_result
        with mock.patch.object(native,'v1_envelope',return_value=wrapper), \
             mock.patch.object(native,'manifest',return_value=(self.entries,self.contents)), \
             mock.patch.object(native,'git',side_effect=lambda w,*a,**k:b'd'*40 if a[0]=='rev-parse' else b'adapter'), \
             mock.patch.object(native,'v1_result',return_value=original) as resolve:
            result = real_v1(api,SCOPE,ROOT,6200,62001,CURRENT,legacy,key)
            self.assertIs(result,original);self.assertTrue(resolve.call_args.kwargs['require_fresh'])
            with self.assertRaises(native.receipts.ReceiptError):
                real_v1(api,SCOPE,ROOT,6200,62001,CURRENT,legacy,key,require_fresh=True)
            retained['execution_origin']['run'] = 6200
            values['receipt.json'] = native.canonical(retained);wrapper['archive'] = packed(values)
            with self.assertRaises(native.receipts.ReceiptError):real_v1(api,SCOPE,ROOT,6200,62001,CURRENT,legacy,key)

    def test_fresh_execution_starts_once_with_scrubbed_environment_and_preserves_failure(self):
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        changed = [{**self.entries[0],'sha256':'3'*64}]
        def sources(workspace,commit):return (self.entries if commit==native.PRODUCER else changed),self.contents
        def git_value(workspace,*args,**kwargs):
            return legacy if args[-1]==native.PRODUCER+':'+native.WORKFLOW else b'identity'
        with tempfile.TemporaryDirectory() as directory,mock.patch.dict(native.os.environ,{'GITHUB_JOB':'apple_compile','GITHUB_TOKEN':'never-child','PATH':'/tools'}), \
             mock.patch.object(native,'git',side_effect=git_value),mock.patch.object(native,'workflow_identity',return_value=native.digest(native.transformed_workflow(legacy))), \
             mock.patch.object(native,'manifest',side_effect=sources),mock.patch.object(native,'current_job',return_value=self.api.jobs[RUN][0]), \
             mock.patch.object(native,'matching_v1',return_value=None),mock.patch.object(native,'recheck'), \
             mock.patch.object(native,'history_before_fresh',return_value={'pagination_complete':True,'attempts':[]}), \
             mock.patch.object(native.subprocess,'run') as make:
            out = Path(directory)
            with self.assertRaises(native.receipts.ReceiptError):native.run_gate(self.api,SCOPE,Path(WORK),CURRENT,RUN,out,{},False)
            make.assert_not_called()
            def fail(argv,**kwargs):
                self.assertTrue((out/'fresh-start.json').exists());self.assertNotIn('GITHUB_TOKEN',kwargs['env'])
                self.assertEqual(kwargs['env']['PATH'],'/tools');return SimpleNamespace(returncode=1)
            make.side_effect = fail
            with self.assertRaises(native.receipts.ReceiptError):native.run_gate(self.api,SCOPE,Path(WORK),CURRENT,RUN,out,{},True)
            make.assert_called_once();self.assertEqual(json.loads((out/'fresh-finish.json').read_text())['exit_code'],1)
            self.assertFalse((out/'receipt.json').exists())

    def test_current_authentication_rejects_wrong_live_head_or_pr_event(self):
        api = FakeAPI();event = json.loads(api.runs[RUN]['event_payload'])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'event.json';path.write_text(json.dumps(event))
            env = {'GITHUB_EVENT_NAME':'pull_request','GITHUB_EVENT_PATH':str(path),'GITHUB_REPOSITORY':'example/project',
                'GITHUB_SHA':CURRENT,'GITHUB_RUN_ID':str(RUN),'GITHUB_RUN_ATTEMPT':'1','GITHUB_TOKEN':'test-secret',
                'GITHUB_API_URL':'https://ci.example/api/v1','GITHUB_SERVER_URL':'https://ci.example'}
            with mock.patch.dict(native.os.environ,env,clear=True),mock.patch.object(native.receipts,'API',return_value=api), \
                 mock.patch.object(native,'git',side_effect=lambda w,*a,**k:b'' if a[0]=='diff' else CURRENT.encode()):
                self.assertEqual(native.current_context(ROOT)[2],CURRENT)
                original = api.get
                api.get = lambda p,q=None:{**original(p,q),'head':{'sha':'c'*40}} if p.startswith('/pulls/') else original(p,q)
                with self.assertRaises(native.receipts.ReceiptError):native.current_context(ROOT)
                api.get = original;event['number'] = 5;path.write_text(json.dumps(event))
                with self.assertRaises(native.receipts.ReceiptError):native.current_context(ROOT)

    def test_v1_api_upload_and_adapter_identity_authenticate_before_receipt(self):
        api,envelope,payload,receipt,key,legacy,packed = self.v1_fixture()
        artifact = {**envelope['artifact'],'size_in_bytes':len(envelope['archive'])};api.artifacts = [artifact]
        lines = ['Runner 00000000-0000-0000-0000-000000000001 (version:v1) received task 22 of job apple_compile, triggered by event: pull_request',
            *envelope['lines'],'SHA256 hash of uploaded artifact zip is '+native.digest(envelope['archive']),
            'Artifact desktop-macos-check-6100-1 has been successfully uploaded! Final size is '+str(len(envelope['archive']))+' bytes. Artifact ID is 5100',
            '🏁  Job succeeded']
        raw = ('\n'.join(lines)+'\n').encode()
        def git_value(w,*a,**k):return b'd'*40 if a[0]=='rev-parse' else b'adapter'
        with mock.patch.object(native,'workflow_identity',return_value=native.digest(native.transformed_workflow(legacy))), \
             mock.patch.object(native,'git',side_effect=git_value),mock.patch.object(native,'manifest',return_value=(self.entries,self.contents)), \
             mock.patch.object(native,'download_log',return_value=raw) as log, \
             mock.patch.object(native.transport,'archive_bytes',return_value=envelope['archive']) as fetch:
            result = native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)
            self.assertEqual(result['artifact']['id'],5100)
            fetch.return_value = envelope['archive'][:-1]+b'!'
            with self.assertRaises(native.receipts.ReceiptError):native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)
            fetch.return_value = envelope['archive'];log.return_value = raw.replace(b'c'*40,b'a'*40)
            with self.assertRaises(native.receipts.ReceiptError):native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)
        def changed_adapter(w,*a,**k):return b'changed' if a[-1].startswith('c'*40+':') else b'adapter'
        with mock.patch.object(native,'workflow_identity',return_value=native.digest(native.transformed_workflow(legacy))), \
             mock.patch.object(native,'git',side_effect=changed_adapter),self.assertRaisesRegex(native.receipts.ReceiptError,'adapter source compatibility'):
            native.v1_envelope(api,SCOPE,ROOT,6100,61001,CURRENT,legacy)
