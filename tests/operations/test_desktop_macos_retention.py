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
    lines += ['swift test --package-path clients/server-tray/macos '+chr(92),
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
        # Close first: the central directory and EOCD are part of the baseline.
        remaining = 3930 - len(output.getvalue())
        assert remaining >= 0
        with zipfile.ZipFile(output,'a') as z:
            z.comment = b'x'*remaining
        assert len(output.getvalue()) == 3930
    return output.getvalue()


def upload_lines(name, aid, size, archive_sha):
    return ['With the provided path, there will be 4 files uploaded',
        'Artifact name is valid!','Root directory input is valid!',
        'Beginning upload of artifact content to blob storage','Uploaded bytes '+str(size),
        'Finished uploading artifact content to blob storage!',
        'SHA256 hash of uploaded artifact zip is '+archive_sha,'Finalizing artifact upload',
        f'Artifact {name}.zip successfully finalized. Artifact ID {aid}',
        f'Artifact {name} has been successfully uploaded! Final size is {size} bytes. Artifact ID is {aid}']


def source_tree():
    required = ['Makefile','Cargo.toml','LICENSE','brand/export/icon.png',
        'clients/server-tray/macos/Package.swift','clients/server-tray/macos/build']
    rows = {path:('100644',path.encode()) for path in required}
    rows.update({path:(mode,subprocess.check_output(['git','show',native.PRODUCER+':'+path],cwd=ROOT))
        for path,(mode,sha) in native.RECIPE_PINS.items()})
    rows['Makefile'] = (native.CANONICAL_MAKEFILE['mode'],subprocess.check_output(['git','show',native.PRODUCER+':Makefile'],cwd=ROOT))
    def blob_id(raw):return hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest()
    def read_git(workspace,*args,**kwargs):
        if args[:2] == ('cat-file','-t'): return b'commit'
        if args[0] == 'ls-tree':
            if '-r' in args:
                selected = {p:value for p,value in rows.items() if any(p==r or p.startswith(r+'/') for r in native.ROOTS)}
            else:
                selected = {p.split('/',1)[0]:(('040000',b'tree') if '/' in p else value) for p,value in rows.items()}
            return b''.join((mode+(' tree ' if mode=='040000' else ' blob ')+blob_id(raw)+'\t'+p).encode()+b'\0' for p,(mode,raw) in selected.items())
        raw = next(raw for mode,raw in rows.values() if blob_id(raw) == args[-1])
        return str(len(raw)).encode() if args[1] == '-s' else raw
    rows.update({path:('100644',raw) for path,raw in source_cases().items()})
    return rows,read_git


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
        command = next(line for line in self.native.splitlines() if line.startswith(b'swift test '))
        self.assertEqual(command, b'swift test --package-path clients/server-tray/macos '+bytes([92]))
        self.assertEqual(command.count(bytes([92])),1)
        self.assertEqual(len(self.zip),3930)
        with zipfile.ZipFile(io.BytesIO(self.zip)) as zipped:
            self.assertEqual(zipped.namelist(),[native.MEMBER])
            self.assertEqual(zipped.read(native.MEMBER),self.native)
        self.log = job_log(archive_sha=native.digest(self.zip)); self.api = FakeAPI(self.log)
        make = subprocess.check_output(['git','show',native.PRODUCER+':Makefile'],cwd=ROOT)
        self.entries = native.NativeManifest(
            [dict(native.CANONICAL_MAKEFILE),{'path':'native-source','mode':'100644','blob':'1'*40,'sha256':'2'*64}],make)
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

    def test_native_workflow_scope_accepts_unrelated_jobs_with_distinct_raw_provenance(self):
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        adapted = native.transformed_workflow(legacy)
        actual = (ROOT/native.WORKFLOW).read_bytes()
        self.assertNotEqual(native.digest(actual),native.digest(adapted))
        for raw,kind in ((legacy,'legacy'),(adapted,'adapted'),(actual,'adapted')):
            with mock.patch.object(native,'git',return_value=raw):
                self.assertEqual(native.workflow_scope(ROOT,CURRENT,legacy),{'sha256':native.digest(raw),'kind':kind})
                self.assertEqual(native.workflow_identity(ROOT,CURRENT,legacy),native.digest(raw))
        _,blocks = native.workflow_parts(actual)
        for name in ('rust_compile','async_refinement','web_compile'):
            block=blocks[name];changed=block.replace(b'    steps:\n',b'    steps:\n      - run: echo unrelated-body\n',1)
            self.assertNotEqual(block,changed)
            raw=actual.replace(block,changed,1)
            with self.subTest(name=name),mock.patch.object(native,'git',return_value=raw):
                self.assertEqual(native.workflow_scope(ROOT,CURRENT,legacy),{'sha256':native.digest(raw),'kind':'adapted'})
        added=actual+b'\n  unrelated_job:\n    runs-on: unused\n    steps:\n      - run: echo unrelated\n'
        with mock.patch.object(native,'git',return_value=added):
            self.assertEqual(native.workflow_identity(ROOT,CURRENT,legacy),native.digest(added))

    def test_native_apple_cache_scope_accepts_only_original_optional_step(self):
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        adapted = native.transformed_workflow(legacy)
        cache = native.reviewed_apple_cache(native.workflow_parts(legacy)[1]['apple_compile'])
        self.assertEqual((len(cache),native.digest(cache)),(645,native.APPLE_CACHE_SHA256))
        policy = native.policy_fingerprint()
        with mock.patch.object(native,'APPLE_CACHE_SHA256','f'*64):
            self.assertNotEqual(native.policy_fingerprint(),policy)
        adjacency = cache + native.APPLE_COMPILE_STEP
        self.assertEqual(legacy.count(adjacency),1)
        self.assertEqual(adapted.count(adjacency),1)
        without_legacy = legacy.replace(adjacency,native.APPLE_COMPILE_STEP,1)
        without_adapted = adapted.replace(adjacency,native.APPLE_COMPILE_STEP,1)
        self.assertEqual(len({native.digest(raw) for raw in
            (legacy,without_legacy,adapted,without_adapted)}),4)
        for raw,kind in ((legacy,'legacy'),(without_legacy,'legacy'),
                (adapted,'adapted'),(without_adapted,'adapted')):
            with self.subTest(kind=kind,sha=native.digest(raw)),mock.patch.object(native,'git',return_value=raw):
                self.assertEqual(native.workflow_scope(ROOT,CURRENT,legacy),
                    {'sha256':native.digest(raw),'kind':kind})
                self.assertEqual(native.workflow_identity(ROOT,CURRENT,legacy),native.digest(raw))

        variants = (
            cache.replace(b'actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830',
                b'actions/cache@1057852bfaa89a56745cba8c7296529d2fc39830'),
            cache.replace(b'clients/apple/build/DerivedData',b'clients/apple/build/OtherData'),
            cache.replace(b'clients/apple/build/DerivedData',b'clients/server-tray/macos/build'),
            cache.replace(b'key: main-fast-apple-dd-',b'key: other-apple-dd-'),
            cache.replace(b"if: needs.scope.outputs.apple == 'true'",b'if: always()'),
            cache.replace(b'Cache DerivedData',b'Cache unknown'),
        )
        changes = [adapted.replace(adjacency,variant+native.APPLE_COMPILE_STEP,1)
            for variant in variants]
        apple = native.workflow_parts(adapted)[1]['apple_compile']
        def changed_apple(before,after):
            self.assertEqual(apple.count(before),1)
            return adapted.replace(apple,apple.replace(before,after,1),1)
        changes += [adapted.replace(adjacency,native.APPLE_COMPILE_STEP,1).replace(
            b'      - name: Verify the pinned mobile Apple toolchain\n',
            cache+b'      - name: Verify the pinned mobile Apple toolchain\n',1),
            adapted.replace(adjacency,cache+adjacency,1),
            changed_apple(b'--fresh-on-new-inputs',b'--skip-native-check'),
            changed_apple(b'DEVELOPER_DIR: /Applications/Xcode.app/Contents/Developer',
                b'DEVELOPER_DIR: /tmp/other-xcode'),
            changed_apple(b'    needs: [scope, preflight]\n',b'    needs: [scope]\n')]
        for raw in changes:
            self.assertNotEqual(raw,adapted)
            with self.subTest(sha=native.digest(raw)),mock.patch.object(native,'git',return_value=raw), \
                 self.assertRaisesRegex(native.receipts.ReceiptError,
                    'Unsupported native Apple (cache placement|job scope)'):
                native.workflow_scope(ROOT,CURRENT,legacy)

    def test_native_workflow_scope_refuses_protected_globals_jobs_and_ambiguous_maps(self):
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        actual = (ROOT/native.WORKFLOW).read_bytes();preamble,blocks=native.workflow_parts(actual)
        changes=[actual.replace(b'CARGO_TERM_COLOR: always',b'CARGO_TERM_COLOR: never',1),
            actual.replace(b'branches: [main]',b'branches: [other]',1),
            actual.replace(b'cancel-in-progress: false',b'cancel-in-progress: true',1)]
        for global_value in (b'defaults:\n  run:\n    shell: unsafe\n',b'permissions:\n  contents: write\n'):
            changes.append(actual.replace(b'jobs:\n',global_value+b'jobs:\n',1))
        for name in (*native.PROTECTED_DEPENDENCIES,'apple_compile'):
            block=blocks[name]
            changes.append(actual.replace(block,block.replace(b'    steps:\n',b'    steps:\n      - run: echo changed-protected-command\n',1),1))
            changes.append(actual.replace(block,b'',1))
        for suffix in (b'env:\n  BAD: value\n',b'jobs:\n',b'---\n',b'%YAML 1.2\n',
            b'  apple_compile:\n    runs-on: other\n',b'  "apple_compile":\n',b'  apple_compile: &anchor\n',
            b'  <<: *anchor\n',b'  ? [apple_compile]\n',b'  flow: {runs-on: other}\n',
            b'   unknown: value\n',b' invalid: value\n'):
            changes.append(actual+suffix)
        changes += [actual.replace(b'\n',b'\r\n',1),actual+b'\t# tab\n',actual+b'\xff',
            actual+'\u2028'.encode(),actual+b'#'+b'x'*native.MAX_WORKFLOW_BYTES,
            actual+b''.join(('  extra_'+str(i)+':\n    runs-on: unused\n').encode() for i in range(native.MAX_WORKFLOW_JOBS))]
        for raw in changes:
            self.assertNotEqual(raw,actual)
            with self.subTest(sha=native.digest(raw)),mock.patch.object(native,'git',return_value=raw), \
                 self.assertRaisesRegex(native.receipts.ReceiptError,'[Nn]ative.*(workflow|job|scope)|[Uu]nsupported native'):
                native.workflow_identity(ROOT,CURRENT,legacy)

    def test_current_native_execution_refuses_legacy_scope_without_launch(self):
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        with mock.patch.dict(native.os.environ,{'GITHUB_JOB':'apple_compile'}), \
             mock.patch.object(native,'git',return_value=legacy),mock.patch.object(native.subprocess,'run') as launch:
            with self.assertRaisesRegex(native.receipts.ReceiptError,'Current native job lacks reviewed adapter recipe'):
                native.run_gate(self.api,SCOPE,ROOT,CURRENT,RUN,ROOT/'target/unrun-native-fixture',{},True)
            launch.assert_not_called()

    def test_v1_native_receipt_binds_actual_raw_workflow_when_scope_matches(self):
        api,envelope,payload,receipt,key,legacy,packed = self.v1_fixture()
        actual = (ROOT/native.WORKFLOW).read_bytes();raw_sha=native.digest(actual)
        transformed_sha=native.digest(native.transformed_workflow(legacy))
        self.assertNotEqual(raw_sha,transformed_sha)
        envelope['workflow']={'sha256':raw_sha,'kind':'adapted'}
        receipt['consumer_workflow_sha256']=raw_sha
        payload['receipt.json']=native.canonical(receipt);envelope['archive']=packed(payload)
        with mock.patch.object(native,'v1_envelope',return_value=envelope), \
             mock.patch.object(native,'manifest',return_value=(self.entries,self.contents)), \
             mock.patch.object(native,'git',side_effect=lambda w,*a,**k:b'd'*40 if a[0]=='rev-parse' else b'adapter'):
            self.assertEqual(native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)['key'],key)
            for wrong in (transformed_sha,'0'*64):
                receipt['consumer_workflow_sha256']=wrong
                payload['receipt.json']=native.canonical(receipt);envelope['archive']=packed(payload)
                with self.subTest(raw_sha=wrong),self.assertRaisesRegex(native.receipts.ReceiptError,'Native v1 source/receipt/policy identity differs'):
                    native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)

    def test_history_matching_partial_running_missing_or_manual_scope_blocks_fresh(self):
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        key = native.execution_key(self.entries,self.expected)
        with mock.patch.object(native,'manifest',return_value=(self.entries,self.contents)), \
             mock.patch.object(native,'workflow_scope',return_value={'sha256':native.PRODUCER_WORKFLOW,'kind':'legacy'}), \
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
             mock.patch.object(native,'workflow_scope',return_value={'sha256':native.PRODUCER_WORKFLOW,'kind':'legacy'}):
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
        rows,read_git = source_tree()
        with mock.patch.object(native,'git',side_effect=read_git):
            original,_ = native.manifest(ROOT,CURRENT)
            rows['clients/server-tray/macos/New.swift'] = ('100644',b'new')
            added,_ = native.manifest(ROOT,CURRENT);self.assertNotEqual(added,original)
            rows['clients/server-tray/macos/New.swift'] = ('100755',b'changed')
            changed,_ = native.manifest(ROOT,CURRENT);self.assertNotEqual(changed,added)
            del rows['clients/server-tray/macos/New.swift']
            for path in ('GNUmakefile','makefile','gnumakefile','MAKEFILE','GNUmakeFile/child','clients/server-tray/macos/Package@swift-6.swift',
                         'clients/server-tray/macos/Package.resolved','clients/server-tray/macos/.swiftpm/configuration/mirrors.json',
                         'clients/server-tray/macos/PACKAGE@swift-6.swift','clients/server-tray/macos/PACKAGE.RESOLVED',
                         'clients/server-tray/macos/.SWIFTPM/configuration/mirrors.json'):
                rows[path] = ('100644',b'outside root or alternate dependency selector')
                with self.subTest(path=path),self.assertRaisesRegex(native.receipts.ReceiptError,'closure review required'):native.manifest(ROOT,CURRENT)
                del rows[path]
            for path in (*native.RECIPE_PINS,'Makefile'):
                saved = rows[path]
                changed_bytes = (saved[1].replace(b'    targets: [',
                    b'    dependencies: [.package(path: "../../../outside")],\n    targets: [')
                    if path.endswith('Package.swift') else saved[1]+b'\nquiet: scripts/native-prep external-helper\n')
                self.assertNotEqual(changed_bytes,saved[1])
                for value in (('100755' if saved[0]=='100644' else '100644',saved[1]),(saved[0],changed_bytes)):
                    rows[path] = value
                    with self.subTest(recipe=path),self.assertRaisesRegex(native.receipts.ReceiptError,'closure review required'):native.manifest(ROOT,CURRENT)
                rows[path] = saved
            rows['clients/server-tray/macos/Resources/new.txt'] = ('100644',b'ordinary resource')
            resources,_ = native.manifest(ROOT,CURRENT);self.assertNotEqual(resources,original)
            del rows['clients/server-tray/macos/Resources/new.txt']
            restored,_ = native.manifest(ROOT,CURRENT);self.assertEqual(restored,original)
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
             mock.patch.object(native,'git',side_effect=git_value),mock.patch.object(native,'workflow_scope',return_value={'sha256':native.digest(native.transformed_workflow(legacy)),'kind':'adapted'}), \
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
            recipe_compatibility=native.RECIPE_COMPATIBILITY,
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
            'workflow':{'sha256':receipt['consumer_workflow_sha256'],'kind':'adapted'},
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
        changed = native.NativeManifest(
            [dict(self.entries[0]),{**self.entries[1],'sha256':'3'*64}],self.entries.make_bytes)
        def sources(workspace,commit):return (self.entries if commit==native.PRODUCER else changed),self.contents
        def git_value(workspace,*args,**kwargs):
            return legacy if args[-1]==native.PRODUCER+':'+native.WORKFLOW else b'identity'
        real_manifest = native.manifest
        rows,read_git = source_tree();supported_rows = rows.copy()
        revised_make = subprocess.check_output(['git','show','9bcfa9ca1f792d214122762981c84c85ba5b3e64:Makefile'],cwd=ROOT)
        with tempfile.TemporaryDirectory() as directory,mock.patch.dict(native.os.environ,{'GITHUB_JOB':'apple_compile','GITHUB_TOKEN':'never-child','PATH':'/tools'}), \
             mock.patch.object(native,'git',side_effect=git_value),mock.patch.object(native,'workflow_scope',return_value={'sha256':native.digest(native.transformed_workflow(legacy)),'kind':'adapted'}), \
             mock.patch.object(native,'manifest',side_effect=sources),mock.patch.object(native,'current_job',return_value=self.api.jobs[RUN][0]), \
             mock.patch.object(native,'matching_v1',return_value=None),mock.patch.object(native,'recheck'), \
             mock.patch.object(native,'live_recipe_selectors'), \
             mock.patch.object(native,'history_before_fresh',return_value={'pagination_complete':True,'attempts':[]}), \
             mock.patch.object(native.subprocess,'run') as make,mock.patch('sys.stdout',new_callable=io.StringIO) as console:
            out = Path(directory)
            with self.assertRaises(native.receipts.ReceiptError):native.run_gate(self.api,SCOPE,Path(WORK),CURRENT,RUN,out,{},False)
            make.assert_not_called()
            for drift in ('unsupported','equivalent'):
                rows.clear();rows.update(supported_rows);guard_calls = []
                def guarded_manifest(workspace,commit):
                    if commit == native.PRODUCER:return self.entries,self.contents
                    guard_calls.append(commit)
                    if len(guard_calls) == 2:
                        rows['Makefile'] = ('100644',revised_make if drift=='equivalent' else rows['Makefile'][1]+b'\ninclude outside-helper.mk\n')
                    return real_manifest(ROOT,commit)
                def guarded_git(workspace,*args,**kwargs):
                    return git_value(workspace,*args,**kwargs) if args[0] in ('show','rev-parse') else read_git(workspace,*args,**kwargs)
                with mock.patch.object(native,'manifest',side_effect=guarded_manifest), \
                     mock.patch.object(native,'git',side_effect=guarded_git):
                    refusal = ('Native guarded source closure changed before execution' if drift=='equivalent'
                        else 'Unsupported native Makefile syntax; closure review required')
                    with self.subTest(drift=drift),self.assertRaisesRegex(native.receipts.ReceiptError,refusal):
                        native.run_gate(self.api,SCOPE,Path(WORK),CURRENT,RUN,out,{},True)
                    self.assertEqual(len(guard_calls),2);make.assert_not_called()
                    self.assertFalse((out/'fresh-start.json').exists())
            def fail(argv,**kwargs):
                self.assertTrue((out/'fresh-start.json').exists());self.assertNotIn('GITHUB_TOKEN',kwargs['env'])
                self.assertEqual(kwargs['env']['PATH'],'/tools')
                kwargs['stdout'].write(b"Test Case '-[PlurxTrayCoreTests.CoreCases testCase000]' passed (0.001 seconds).\n")
                return SimpleNamespace(returncode=1)
            make.side_effect = fail
            with self.assertRaises(native.receipts.ReceiptError):native.run_gate(self.api,SCOPE,Path(WORK),CURRENT,RUN,out,{},True)
            make.assert_called_once();self.assertEqual(json.loads((out/'fresh-finish.json').read_text())['exit_code'],1)
            self.assertFalse((out/'receipt.json').exists())
            self.assertNotIn('Test Case',console.getvalue())
            self.assertIn('Test Case',(out/native.MEMBER).read_text())

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
                raw_event=path.read_bytes()
                for tip in ('b'*40,'d'*40):
                    def advanced(p,q=None):
                        response=original(p,q)
                        if p.startswith('/pulls/'):response['base']['sha']=tip
                        return response
                    api.get=advanced
                    self.assertEqual(native.current_context(ROOT)[2],CURRENT)
                    native.recheck(ROOT,CURRENT,RUN)
                    self.assertEqual(path.read_bytes(),raw_event)
                    self.assertEqual(json.loads(api.runs[RUN]['event_payload'])['pull_request']['base']['sha'],'a'*40)
                api.get=original
                def wrong_live_head(p,q=None):
                    response = original(p,q)
                    if p.startswith('/pulls/'):response['head']['sha'] = 'c'*40
                    return response
                api.get = wrong_live_head
                with self.assertRaisesRegex(native.receipts.ReceiptError,'Main receipt must match an open same-repository PR head'):
                    native.current_context(ROOT)
                api.get = original;event['number'] = 5;path.write_text(json.dumps(event))
                with self.assertRaisesRegex(native.receipts.ReceiptError,'Native current repository/PR differs'):
                    native.current_context(ROOT)

    def test_v1_api_upload_and_adapter_identity_authenticate_before_receipt(self):
        api,envelope,payload,receipt,key,legacy,packed = self.v1_fixture()
        artifact = {**envelope['artifact'],'size_in_bytes':len(envelope['archive'])};api.artifacts = [artifact]
        lines = ['Runner 00000000-0000-0000-0000-000000000001 (version:v1) received task 22 of job apple_compile, triggered by event: pull_request',
            *envelope['lines'],*upload_lines('desktop-macos-check-6100-1',5100,
                len(envelope['archive']),native.digest(envelope['archive'])),
            *upload_lines('tv-navigation-6100-1',5101,2048,'e'*64),'🏁  Job succeeded']
        raw = ('\n'.join(lines)+'\n').encode()
        def git_value(w,*a,**k):return b'd'*40 if a[0]=='rev-parse' else b'adapter'
        with mock.patch.object(native,'workflow_scope',return_value={'sha256':native.digest(native.transformed_workflow(legacy)),'kind':'adapted'}), \
             mock.patch.object(native,'git',side_effect=git_value),mock.patch.object(native,'manifest',return_value=(self.entries,self.contents)), \
             mock.patch.object(native,'download_log',return_value=raw) as log, \
             mock.patch.object(native.transport,'archive_bytes',return_value=envelope['archive']) as fetch:
            result = native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)
            self.assertEqual(result['artifact']['id'],5100)
            digest_line = ('SHA256 hash of uploaded artifact zip is '+native.digest(envelope['archive'])+'\n').encode()
            for mutated in (raw.replace(digest_line,b''),raw.replace(digest_line,digest_line*2),
                            raw.replace(digest_line,b'SHA256 hash of uploaded artifact zip is '+b'f'*64+b'\n')):
                log.return_value = mutated
                with self.subTest(native_digest=mutated[-200:]),self.assertRaisesRegex(native.receipts.ReceiptError,'Native v1 (upload|archive/upload)'):
                    native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)
            log.return_value = raw
            fetch.return_value = envelope['archive'][:-1]+b'!'
            with self.assertRaises(native.receipts.ReceiptError):native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)
            fetch.return_value = envelope['archive'];log.return_value = raw.replace(b'c'*40,b'a'*40)
            with self.assertRaises(native.receipts.ReceiptError):native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)
        def changed_adapter(w,*a,**k):return b'changed' if a[-1].startswith('c'*40+':') else b'adapter'
        with mock.patch.object(native,'workflow_scope',return_value={'sha256':native.digest(native.transformed_workflow(legacy)),'kind':'adapted'}), \
             mock.patch.object(native,'git',side_effect=changed_adapter),self.assertRaisesRegex(native.receipts.ReceiptError,'adapter source compatibility'):
            native.v1_envelope(api,SCOPE,ROOT,6100,61001,CURRENT,legacy)


    def test_native_upload_digest_is_bound_to_one_bounded_section(self):
        name,aid,size,sha = 'desktop-macos-check-6100-1',5100,4096,'a'*64
        section = upload_lines(name,aid,size,sha)
        tv = upload_lines('tv-navigation-6100-1',5101,2048,'b'*64)
        for lines in (section+tv,tv+section):
            self.assertEqual(native.upload_digest(lines,name,aid,size),sha)
        changes = [lambda x:x[:6]+x[7:],lambda x:x[:7]+[x[6]]+x[7:],
            lambda x:x[:8]+[x[8].replace('5100','5102')]+x[9:],
            lambda x:x[:-1]+[x[-1].replace('4096','4097')],lambda x:x+x,
            lambda x:x[:6]+[x[7],x[6]]+x[8:],lambda x:x[:3]+['padding']*33+x[3:],
            lambda x:x[:3]+['x'*16385]+x[3:]]
        for change in changes:
            with self.subTest(change=change),self.assertRaisesRegex(native.receipts.ReceiptError,'Native v1 upload'):
                native.upload_digest(change(section)+tv,name,aid,size)
        wrong = section.copy();wrong[6] = wrong[6].replace(sha,'c'*64)
        self.assertNotEqual(native.upload_digest(wrong+tv,name,aid,size),sha)


    def test_unsupported_recipe_refuses_history_selection_and_direct_v1_reuse(self):
        api,envelope,payload,receipt,key,legacy,packed = self.v1_fixture()
        rows,read_git = source_tree();supported = rows.copy()
        with mock.patch.object(native,'git',side_effect=read_git), \
             mock.patch.object(native,'workflow_scope',return_value={'sha256':native.digest(native.transformed_workflow(legacy)),'kind':'adapted'}), \
             mock.patch.object(native,'v1_envelope',return_value=envelope), \
             mock.patch.object(native.subprocess,'run') as launch:
            for shape in ('outside helper','case alias'):
                rows.clear();rows.update(supported)
                if shape == 'outside helper':rows['Makefile'] = ('100644',rows['Makefile'][1]+b'\ndesktop-macos-build:\n\t@scripts/native-prep\n')
                else:rows['GNUmakeFile/child'] = ('100644',b'outside helper')
                for call in (lambda:native.history_before_fresh(api,SCOPE,ROOT,CURRENT,RUN,key,legacy),
                             lambda:native.matching_v1(api,SCOPE,ROOT,CURRENT,RUN,key,legacy),
                             lambda:native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)):
                    with self.subTest(shape=shape,call=call),self.assertRaisesRegex(native.receipts.ReceiptError,'closure review required'):
                        call()
                launch.assert_not_called()

    def test_live_recipe_selectors_use_actual_names_and_refuse_untracked_inputs(self):
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory);package = root/'clients/server-tray/macos';package.mkdir(parents=True)
            (root/'Makefile').write_text('supported spelling')
            native.live_recipe_selectors(root)
            for path in (root/'GNUmakefile',package/'Package@swift-6.swift',package/'Package.resolved',package/'.swiftpm'):
                path.write_text('untracked alternate input')
                with self.subTest(path=path),self.assertRaisesRegex(native.receipts.ReceiptError,'closure review required'):
                    native.live_recipe_selectors(root)
                path.unlink()
            # On a case-insensitive host this spelling aliases Makefile; use a
            # clean directory to verify the actual lower-case name independently.
            (root/'Makefile').unlink();(root/'makefile').write_text('alternate selector')
            with self.assertRaisesRegex(native.receipts.ReceiptError,'closure review required'):native.live_recipe_selectors(root)
            with mock.patch.dict(native.os.environ,{'GITHUB_JOB':'apple_compile'}), \
                 mock.patch.object(native,'git',return_value=legacy), \
                 mock.patch.object(native,'workflow_scope',return_value={'sha256':native.digest(native.transformed_workflow(legacy)),'kind':'adapted'}), \
                 mock.patch.object(native,'manifest') as source,mock.patch.object(native,'legacy_result') as retain, \
                 mock.patch.object(native.subprocess,'run') as launch:
                with self.assertRaisesRegex(native.receipts.ReceiptError,'closure review required'):
                    native.run_gate(self.api,SCOPE,root,CURRENT,RUN,root/'evidence',{},True)
                source.assert_not_called();retain.assert_not_called();launch.assert_not_called()

    def test_recipe_pins_and_rules_are_bound_to_policy_without_changing_execution_key(self):
        key = native.execution_key(self.entries,self.expected);policy = native.policy_fingerprint()
        with mock.patch.object(native,'RECIPE_VERSION',native.RECIPE_VERSION+1):
            self.assertNotEqual(native.policy_fingerprint(),policy)
            self.assertEqual(native.execution_key(self.entries,self.expected),key)
        with mock.patch.object(native,'RECIPE_PINS',{**native.RECIPE_PINS,'Makefile':('100644','f'*64)}):
            self.assertNotEqual(native.policy_fingerprint(),policy)
        for name,value in (('MAKEFILE_FAMILY',(native.MAKEFILE_FAMILY[0],{**native.MAKEFILE_FAMILY[1],'sha256':'f'*64})),
                           ('CANONICAL_MAKEFILE',{**native.CANONICAL_MAKEFILE,'blob':'f'*40}),
                           ('APPLICABILITY_PROJECTION','another-projection')):
            with self.subTest(policy_field=name),mock.patch.object(native,name,value):
                self.assertNotEqual(native.policy_fingerprint(),policy)

    def test_reviewed_make_pair_shares_applicability_without_mutating_raw_inputs(self):
        rows,read_git = source_tree()
        candidate = subprocess.check_output(['git','show','b7368b684da720fd90697fa25a0540e4ec0f875d:Makefile'],cwd=ROOT)
        main = subprocess.check_output(['git','show','94da6eb93d7199e1d92e38ffbbcc53451b075303:Makefile'],cwd=ROOT)
        with mock.patch.object(native,'git',side_effect=read_git):
            original,_ = native.manifest(ROOT,native.PRODUCER)
            manifests = [original]
            for raw in (candidate,main,main+b'\nunrelated-native-fixture: ## Independent literal rule\n\t@echo unrelated\n'):
                rows['Makefile'] = ('100644',raw)
                item,_ = native.manifest(ROOT,CURRENT)
                manifests.append(item)
            before = copy.deepcopy(manifests)
            self.assertEqual(len({native.digest(native.canonical(item)) for item in manifests}),4)
            self.assertTrue(all(native.applicability_inputs(item) == original for item in manifests))
            self.assertEqual(len({native.execution_key(item,self.expected) for item in manifests}),1)
            self.assertEqual(manifests,before)
            rows['brand/export/new-resource.txt'] = ('100644',b'new resource')
            changed,_ = native.manifest(ROOT,CURRENT)
            self.assertNotEqual(native.execution_key(changed,self.expected),
                native.execution_key(original,self.expected))
        for entries in ([],original+original,[{**native.CANONICAL_MAKEFILE,'blob':'0'*40}]):
            with self.assertRaisesRegex(native.receipts.ReceiptError,'Makefile'):
                native.applicability_inputs(entries)

    def test_native_make_scope_refuses_unsafe_constructs_and_raw_row_mismatch(self):
        old = subprocess.check_output(['git','show',native.PRODUCER+':Makefile'],cwd=ROOT)
        main = subprocess.check_output(['git','show','94da6eb93d7199e1d92e38ffbbcc53451b075303:Makefile'],cwd=ROOT)
        self.assertEqual(native.digest(main),'b987ff3b2170f6222c4900a4a1063591bdd0f4b17fbaa095a029ab586e5e6d20')
        native.native_make_scope(old)
        native.native_make_scope(main)
        changed = {
            'native-prerequisite': old.replace(b'desktop-macos-check: desktop-macos-build desktop-macos-test',
                b'desktop-macos-check: desktop-macos-build'),
            'native-body': old.replace(b'swift test --package-path clients/server-tray/macos',
                b'swift test --package-path clients/server-tray/other'),
            'native-duplicate': old+b'\ndesktop-macos-build:\n\t@true\n',
            'native-co-target': old+b'\ndesktop-macos-build unrelated:\n\t@true\n',
            'native-phony': old+b'\n.PHONY: desktop-macos-build\n',
            'check-recipe': old.replace(
                b'desktop-macos-check: desktop-macos-build desktop-macos-test ## Build both slices and test on the native macOS architecture\n',
                b'desktop-macos-check: desktop-macos-build desktop-macos-test ## Build both slices and test on the native macOS architecture\n\t@echo unsafe\n'),
            'selector-phony': old+b'\n.PHONY: makefile\n',
            'version': old.replace(b'VERSION := $(shell sed',b'VERSION := $(shell echo unsafe) # sed'),
            'eager-shell': old.replace(b'BUILD_SHA := $(shell git rev-parse HEAD 2>/dev/null)',
                b'BUILD_SHA := $(shell echo unsafe)'),
            'include': old+b'\ninclude another.mk\n',
            'eval': old+b'\n$(eval VERSION := unsafe)\n',
            'define': old+b'\ndefine UNSAFE\n\t@echo unsafe\nendef\n',
            'oneshell': old+b'\n.ONESHELL:\n',
            'recipeprefix': old+b'\n.RECIPEPREFIX := >\n',
            'second-expansion': old+b'\n.SECONDEXPANSION:\n',
            'default': old+b'\n.DEFAULT_GOAL := unsafe\n',
            'pattern': old+b'\n%.out: %.in\n\t@true\n',
            'make-remake': old+b'\nmakefile:\n\t@true\n',
            'override': old+b'\ndesktop-macos-build: override VERSION := unsafe\n',
            'conditional-native': old.replace(b'.PHONY: desktop-macos-build desktop-macos-test desktop-macos-check',
                b'ifeq (1,1)\n.PHONY: desktop-macos-build desktop-macos-test desktop-macos-check\nendif'),
            'comment-continuation': old+b'\n# comment \\\ninclude unsafe.mk\n',
            'header-continuation': old+b'\nquiet: \\\ninclude unsafe.mk\n',
            'orphan-recipe': old+b'\n.PHONY: quiet\n\t@true\n',
            'recipe-continuation': old+b'\nquiet:\n\t@true \\\ninclude unsafe.mk\n',
            'bad-utf8': old+b'\n# \xff\n',
            'crlf': old.replace(b'\n',b'\r\n',1),
            'nul': old+b'\n# \x00\n',
            'line-bound': old+b'\n'*native.MAX_MAKE_LINES,
            'byte-bound': old+b'\n# '+b'x'*native.MAX_BLOB,
        }
        for label,raw in changed.items():
            with self.subTest(shape=label),self.assertRaisesRegex(native.receipts.ReceiptError,'closure review required'):
                native.native_make_scope(raw)
        row = dict(native.CANONICAL_MAKEFILE)
        for item in ([],[{**row,'blob':'0'*40}],[{**row,'sha256':'0'*64}],
                [{**row,'mode':'100755'}]):
            proof = native.NativeManifest(item,old)
            with self.subTest(row=item),self.assertRaisesRegex(native.receipts.ReceiptError,'Makefile'):
                native.applicability_inputs(proof)
        with self.assertRaisesRegex(native.receipts.ReceiptError,'Makefile'):
            native.applicability_inputs(native.NativeManifest([row],old+b'\n# drift\n'))
        with self.assertRaisesRegex(native.receipts.ReceiptError,'Makefile'):
            native.applicability_inputs([row])

    def test_reviewed_make_pair_retains_known_origin_with_actual_current_provenance(self):
        old_rows,old_read = source_tree();new_rows,new_read = source_tree()
        new_rows['Makefile'] = ('100644',subprocess.check_output(['git','show','94da6eb93d7199e1d92e38ffbbcc53451b075303:Makefile'],cwd=ROOT))
        legacy = subprocess.check_output(['git','show',native.PRODUCER+':'+native.WORKFLOW],cwd=ROOT)
        active = [old_read]
        def read_git(workspace,*args,**kwargs):
            if args[0] == 'show':return legacy if args[-1]==native.PRODUCER+':'+native.WORKFLOW else b'adapter'
            if args[0] == 'rev-parse':return native.PRODUCER_TREE.encode() if args[-1].startswith(native.PRODUCER) else b'd'*40
            if args[0] == 'ls-tree':active[0] = old_read if native.PRODUCER in args else new_read
            return active[0](workspace,*args,**kwargs)
        with tempfile.TemporaryDirectory() as directory, \
             mock.patch.dict(native.os.environ,{'GITHUB_JOB':'apple_compile'}), \
             mock.patch.object(native,'git',side_effect=read_git), \
             mock.patch.object(native,'workflow_scope',return_value={'sha256':native.digest(native.transformed_workflow(legacy)),'kind':'adapted'}), \
             mock.patch.object(native,'recheck'),mock.patch.object(native,'live_recipe_selectors'), \
             mock.patch.object(native,'download_log',return_value=self.log), \
             mock.patch.object(native.transport,'archive_bytes',return_value=self.zip), \
             mock.patch.object(native,'history_before_fresh') as history, \
             mock.patch.object(native.subprocess,'run') as launch:
            out = Path(directory)/'retained';out.mkdir()
            receipt = native.run_gate(self.api,SCOPE,Path(WORK),CURRENT,RUN,out,{},True)
            actual,_ = native.manifest(ROOT,CURRENT);origin,_ = native.manifest(ROOT,native.PRODUCER)
            self.assertEqual(receipt['decision'],'retained_prior_execution')
            self.assertEqual(receipt['current_source_manifest'],actual)
            self.assertNotEqual(actual,origin)
            self.assertEqual(next(row for row in actual if row['path']=='Makefile')['sha256'],
                'b987ff3b2170f6222c4900a4a1063591bdd0f4b17fbaa095a029ab586e5e6d20')
            self.assertEqual(receipt['checkout_attestation']['source_manifest_sha256'],native.digest(native.canonical(actual)))
            self.assertEqual(receipt['execution_origin']['commit'],native.PRODUCER)
            self.assertFalse(receipt['current_native_commands_executed'])
            launch.assert_not_called();history.assert_not_called()
            self.api.artifacts = [];missing = Path(directory)/'missing';missing.mkdir()
            with self.assertRaisesRegex(native.receipts.ReceiptError,'Known native artifact'):
                native.run_gate(self.api,SCOPE,Path(WORK),CURRENT,RUN,missing,{},True)
            launch.assert_not_called();history.assert_not_called()
            self.assertFalse((missing/'fresh-start.json').exists())

    def test_equivalent_make_key_does_not_normalize_v1_producer_raw_identity(self):
        rows,read_git = source_tree()
        revised_make = subprocess.check_output(['git','show','94da6eb93d7199e1d92e38ffbbcc53451b075303:Makefile'],cwd=ROOT)
        with mock.patch.object(native,'git',side_effect=read_git):
            original,contents = native.manifest(ROOT,CURRENT)
            rows['Makefile'] = ('100644',revised_make);revised,_ = native.manifest(ROOT,CURRENT)
        self.entries = original
        api,envelope,payload,receipt,key,legacy,packed = self.v1_fixture()
        rows['Makefile'] = ('100644',contents['Makefile'])
        def actual_git(workspace,*args,**kwargs):
            if args[0]=='show':return b'adapter'
            if args[0]=='rev-parse':return b'd'*40
            return read_git(workspace,*args,**kwargs)
        with mock.patch.object(native,'v1_envelope',return_value=envelope), \
             mock.patch.object(native,'git',side_effect=actual_git):
            self.assertEqual(native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)['manifest'],original)
            receipt['current_source_manifest'] = revised
            payload['receipt.json'] = native.canonical(receipt);envelope['archive'] = packed(payload)
            with self.assertRaisesRegex(native.receipts.ReceiptError,'Native v1 source/receipt/policy identity differs'):
                native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)
            receipt['current_source_manifest'] = original
            old_attest = copy.deepcopy(receipt['checkout_attestation'])
            receipt['checkout_attestation']['source_manifest_sha256'] = native.digest(native.canonical(revised))
            envelope['lines'] = [line.replace(native.canonical(old_attest).decode(),native.canonical(receipt['checkout_attestation']).decode()) for line in envelope['lines']]
            payload['receipt.json'] = native.canonical(receipt);envelope['archive'] = packed(payload)
            with self.assertRaisesRegex(native.receipts.ReceiptError,'Native v1 checkout identity differs'):
                native.v1_result(api,SCOPE,ROOT,6100,61001,CURRENT,legacy,key)
