"""Shared successor web source/runtime guards; never executes Node/npm/Make."""
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
from validation import web_unit_receipts as controller
from validation import python_unit_receipts as receipts

ROOT = Path(__file__).resolve().parents[2]
COMMIT, BASE = 'b'*40, 'c'*40
SCOPE = {'repository':1,'pr':1263,'branch':'topic','base':'main','workflow':'main-fast-lane.yml'}


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
                self.assertNotEqual(web.digest(web.canonical(previous)),web.digest(web.canonical(after)))
                self.assertEqual(before,frozen)
            different_base = web.manifest(ROOT,COMMIT,'d'*40)
            self.assertNotEqual(web.digest(web.canonical(after)),web.digest(web.canonical(different_base)))
            self.assertEqual(after['baseline']['sha256'],different_base['baseline']['sha256'])

    def test_current_node_provisioning_matches_exact_successor_runtime(self):
        block = controller.job_block((ROOT / controller.WORKFLOW).read_text(), 'web_compile')
        self.assertEqual(block.count('node-version: "22.23.4"'), 1)
        self.assertEqual(web.digest(block.encode()), controller.CURRENT_JOB_SHA256)
        observed = {'node': 'v22.23.4', 'npm': '10.9.9', 'git': 'git version 2.55.0', 'make': 'GNU Make 4.4'}
        with mock.patch.object(web.sys, 'platform', 'linux'), \
                mock.patch.object(web.platform, 'machine', return_value='x86_64'), \
                mock.patch.object(web.subprocess, 'check_output', side_effect=lambda command, **kw: observed[command[0]] + '\n'):
            self.assertEqual(web.runtime()['job_recorded'], {k: v for k, v in web.RUNTIME.items() if k != 'typescript_lock'})
            observed['node'] = 'v22.23.3'
            with self.assertRaisesRegex(receipts.ReceiptError, 'Unsupported web concrete runtime family'):
                web.runtime()


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
                original_event=event_path.read_bytes()
                for live_base in ('d'*40,'e'*40):
                    pull['base']['sha']=live_base
                    self.assertEqual(web.current_context(ROOT)[5],BASE)
                    self.assertEqual(event_path.read_bytes(),original_event)
                    self.assertEqual(json.loads(run['event_payload'])['pull_request']['base']['sha'],BASE)
                for change in ({'attempt':2},{'attempt':True},{'status':'success'},{'repo_id':2},{'task_id':0},{'task_id':-1},{'task_id':True},{'task_id':1.0},{'task_id':'1'}):
                    saved=job.copy();job.update(change)
                    with self.subTest(change=change),self.assertRaises(receipts.ReceiptError):web.current_context(ROOT)
                    job.clear();job.update(saved)
                # Draft nonproducer history grants no current-job authority.
                ready_payload=run['event_payload'];draft_event=json.loads(original_event)
                draft_event['pull_request']['draft']=True
                event_path.write_text(json.dumps(draft_event));run['event_payload']=json.dumps(draft_event)
                with self.assertRaisesRegex(receipts.ReceiptError,'not a ready PR event'):
                    web.current_context(ROOT)
                event_path.write_bytes(original_event);run['event_payload']=ready_payload
                pull['head']={**pull['head'],'sha':'d'*40}
                with self.assertRaisesRegex(receipts.ReceiptError,'Main receipt must match'):web.current_context(ROOT)

    def test_adapter_changes_select_the_real_web_job_and_live_aliases_refuse(self):
        from validation import ci_scope
        for path in ('validation/web_cohort_retention.py', 'tests/operations/test_web_cohort_retention.py',
                     'validation/web_unit_receipts.py', 'tests/validation/test_web_unit_receipts.py'):
            self.assertTrue(ci_scope.scope_for_paths(ci_scope.load_catalog(),(path,))['web_layout'])
        with mock.patch.object(web,'selectors'),mock.patch.object(web,'git',return_value=b'unknown.js\0'):
            with self.assertRaisesRegex(receipts.ReceiptError,'Untracked/ignored web closure'):
                web.live_inputs(ROOT)
        with mock.patch.object(web,'selectors'),mock.patch.object(web,'git',return_value=b''), \
             mock.patch.dict(os.environ,{'NODE_OPTIONS':'--require /outside/helper.js'}):
            with self.assertRaisesRegex(receipts.ReceiptError,'interpreter configuration'):
                web.live_inputs(ROOT)

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
            self.assertNotEqual(web.digest(web.canonical(old)),web.digest(web.canonical(current)))
            self.assertEqual(original,frozen)
            self.assertNotEqual(old['files'],current['files'])
            for helper in ('tests/vendor/acorn.js','scripts/playback-startup-lab'):
                original_helper=contents[helper];contents[helper] += b'require("outside-closure")\n'
                with self.subTest(helper=helper),self.assertRaisesRegex(receipts.ReceiptError,'executable/recipe/dependency shape'):
                    web.manifest(ROOT,COMMIT,BASE)
                contents[helper]=original_helper

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
             mock.patch.object(web,'live_inputs'),mock.patch.object(web,'live_script_resolvers'), \
             mock.patch.object(web.subprocess,'run') as launch:
            for path,raw in cases.items():
                original=contents[path];contents[path]=raw
                with self.subTest(path=path),self.assertRaisesRegex(receipts.ReceiptError,'executable/recipe/dependency shape'):
                    web.manifest(ROOT, COMMIT, BASE)
                contents[path]=original
            for suffix in ('.js','.mjs','.cjs','.ts','.d.ts','.html','.json'):
                path='crates/plurxd/src/web/new-loader'+suffix;contents[path]=b'new unreviewed member'
                with self.subTest(suffix=suffix),self.assertRaisesRegex(receipts.ReceiptError,'executable/recipe/dependency shape'):
                    web.manifest(ROOT, COMMIT, BASE)
                del contents[path]
            launch.assert_not_called()

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
             mock.patch.object(web,'live_inputs'),mock.patch.object(web,'live_script_resolvers'), \
             mock.patch.object(web.subprocess,'run') as launch:
            for path in paths:
                contents[path]=b'unreviewed committed resolver input'
                with self.subTest(path=path),self.assertRaisesRegex(receipts.ReceiptError,'executable/recipe/dependency shape'):
                    web.manifest(ROOT, COMMIT, BASE)
                del contents[path]
            launch.assert_not_called()

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
