"""Synthetic authenticated-transport and reuse contracts; no Rust/solver runs."""
from __future__ import annotations
import argparse
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import warnings
import zipfile

from validation import async_refinement_resume as adapter
from validation import worker_control_resume as shared
from validation import python_unit_receipts as receipts

ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader('async_resume_runner',str(ROOT/'scripts/check-async-driver-refinement'))
SPEC = importlib.util.spec_from_loader(LOADER.name,LOADER)
runner = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(runner)


def archive(entries):
    output = io.BytesIO()
    with warnings.catch_warnings():
        warnings.simplefilter('ignore',UserWarning)
        with zipfile.ZipFile(output,'w',compression=zipfile.ZIP_STORED) as stream:
            for name,data,mode in entries:
                member = zipfile.ZipInfo(name); member.external_attr = mode << 16
                stream.writestr(member,data)
    return output.getvalue()


class OwnedScratch(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = ROOT/'target/async-refinement-resume-fixtures'
        cls.scratch.mkdir(parents=True,exist_ok=True)

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(dir=self.scratch)
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)


class InvocationTrust(OwnedScratch):
    def git(self,*args):
        return subprocess.check_output(['git',*args],cwd=self.directory,stderr=subprocess.DEVNULL).decode().strip()

    def commit(self):
        self.git('add','.')
        self.git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture')
        return self.git('rev-parse','HEAD')

    def test_identical_git_blobs_only_even_with_a_reviewed_policy_file(self):
        self.git('init','-q')
        for path in adapter.INVOCATION_PATHS:
            target = self.directory/path; target.parent.mkdir(parents=True,exist_ok=True); target.write_text('fixture\n')
        prior = self.commit()
        result = adapter.identical_invocation(prior,prior,self.directory)
        self.assertEqual(result['decision'],'identical_invocation')
        policy = self.directory/shared.REVIEWED_POLICY
        policy.parent.mkdir(parents=True,exist_ok=True)
        policy.write_text('{"schema_version":1,"entries":[]}')
        (self.directory/adapter.INVOCATION_PATHS[0]).write_text('changed fixture\n')
        current = self.commit()
        with self.assertRaisesRegex(receipts.ReceiptError,'no historical exceptions'):
            adapter.identical_invocation(prior,current,self.directory)

    def test_legacy_defaults_and_redirect_refusal_are_unchanged(self):
        spec,limits = shared.TransportSpec(),shared.ExtractionSpec()
        self.assertEqual((spec.workflow,spec.proof_job,spec.artifact_prefix,spec.max_archive_bytes),
          (shared.WORKFLOW,shared.PROOF_JOB,'worker-control-proof',shared.MAX_ARCHIVE_BYTES))
        self.assertIsNone(spec.trust)
        self.assertEqual(limits.max_member_bytes,shared.MAX_MEMBER_BYTES)
        self.assertIsNone(limits.select)
        with self.assertRaises(receipts.ReceiptError): receipts.NoRedirect().redirect_request(None)


class ArchiveSafety(OwnedScratch):
    def test_only_useful_data_is_hydrated(self):
        data = archive([(name,b'{}',stat.S_IFREG|0o600) for name in (
          'provenance.json','driver-graph.json','product-bundle.json','rust-driver-closure.log',
          'rust-control-C4.log','rust-control-C9.log','tlc/receipt.json','tlc/cleanup.log','tlc/SourceTables.tla','tlc/cleanup.cfg',
          'tlc/inventory.json','selector-target/binary','selector-mutant/src/lib.rs',
          'scratch/private.json','validation/untrusted.py','resume-origin.json')])
        count,_ = adapter.extract_evidence(data,self.directory)
        self.assertEqual(count,8)
        self.assertFalse((self.directory/'validation').exists())
        self.assertFalse((self.directory/'tlc/SourceTables.tla').exists())
        self.assertFalse((self.directory/'resume-origin.json').exists())

    def test_traversal_symlink_special_duplicate_and_oversized_log_refuse(self):
        bad = [
          [('../product-bundle.json',b'{}',stat.S_IFREG|0o600)],
          [('provenance.json',b'{}',stat.S_IFLNK|0o777)],
          [('provenance.json',b'{}',stat.S_IFIFO|0o600)],
          [('provenance.json',b'{}',stat.S_IFREG|0o600)]*2,
          [('rust-driver-closure.log',b'x'*(adapter.MAX_SMALL_BYTES+1),stat.S_IFREG|0o600)],
        ]
        for index,entries in enumerate(bad):
            destination = self.directory/str(index); destination.mkdir()
            with self.subTest(index=index),self.assertRaises(receipts.ReceiptError):
                adapter.extract_evidence(archive(entries),destination)

    def test_graph_limit_is_distinct_from_small_log_limit(self):
        payload = b'x'*(adapter.MAX_SMALL_BYTES+1)
        count,size = adapter.extract_evidence(archive([('product-bundle.json',payload,stat.S_IFREG|0o600)]),self.directory)
        self.assertEqual((count,size),(1,len(payload)))

    def test_generic_extractor_keeps_aggregate_and_compression_bounds(self):
        data = archive([('t1_fixture.log',b'1234',stat.S_IFREG|0o600)])
        with self.assertRaises(receipts.ReceiptError):
            shared.extract_evidence(data,self.directory,spec=shared.ExtractionSpec(max_total_bytes=3))
        output = io.BytesIO()
        with zipfile.ZipFile(output,'w',compression=zipfile.ZIP_DEFLATED) as stream:
            stream.writestr('rust-driver-closure.log',b'x'*100000)
        with self.assertRaises(receipts.ReceiptError): adapter.extract_evidence(output.getvalue(),self.directory)


class ReuseIdentities(OwnedScratch):
    def test_semantic_invocation_changes_invalidate_persistent_case_rows(self):
        sources = {'Cargo.toml':'root','Cargo.lock':'lock','rust-toolchain.toml':'tool',
          'crates/plurx-core/src/store/async_driver_product.rs':'product'}
        sources.update({path:'same invocation bytes' for path in adapter.INVOCATION_PATHS})
        product_input = self.directory/'product-input'; product_input.mkdir()
        raw_driver = product_input/'driver-graph.json'; raw_driver.write_text('{"states":[] }\n')
        baseline = runner.case_sources('graph-product',sources,out=product_input)
        for path in ('verification/async-driver-refinement/checker.py',
                     'verification/async-driver-refinement/product-schema.json',
                     'validation/async_refinement_resume.py',
                     'validation/worker_control_resume.py'):
            changed = dict(sources); changed[path] = 'changed invocation bytes'
            with self.subTest(path=path):
                self.assertNotEqual(baseline,runner.case_sources('graph-product',changed,out=product_input))
        self.assertEqual(baseline,runner.case_sources('graph-product',dict(sources),out=product_input))
        raw_driver.write_text('{"states":[1]}\n')
        self.assertNotEqual(baseline,runner.case_sources('graph-product',sources,out=product_input))

    def test_product_semantic_table_changes_invalidate_embedded_bundle(self):
        sources = {'Cargo.toml':'root','Cargo.lock':'lock','rust-toolchain.toml':'tool',
          'crates/plurx-core/src/store/async_driver_product.rs':'product'}
        sources.update({path:'same invocation bytes' for path in adapter.INVOCATION_PATHS})
        output = self.directory/'semantic-input'; output.mkdir()
        (output/'driver-graph.json').write_text('{}\n')
        semantic = {'verification/async-driver-refinement/AsyncDriverRefinement.tla':'a'*64,
          'verification/async-driver-refinement/cleanup.cfg':'b'*64}
        with patch.object(runner.c,'product_semantic_bindings',return_value=semantic,create=True):
            baseline = runner.case_sources('graph-product',sources,out=output)
            changed = dict(semantic); changed['verification/async-driver-refinement/cleanup.cfg'] = 'c'*64
            with patch.object(runner.c,'product_semantic_bindings',return_value=changed,create=True):
                self.assertNotEqual(baseline,runner.case_sources('graph-product',sources,out=output))

    def test_store_edit_preserves_leaf_but_invalidates_product_and_daemon_scopes(self):
        sources = {'Cargo.toml':'root','Cargo.lock':'lock','rust-toolchain.toml':'tool',
          'crates/plurx-worker-control/src/lib.rs':'leaf','crates/plurx-core/src/store/mod.rs':'store',
          'crates/plurxd/src/library_work.rs':'daemon'}
        sources.update({path:'same invocation bytes' for path in adapter.INVOCATION_PATHS})
        changed = dict(sources); changed['crates/plurx-core/src/store/mod.rs'] = 'changed'
        self.assertEqual(runner.case_sources('graph-driver',sources),runner.case_sources('graph-driver',changed))
        product_input = self.directory/'store-edit-product-input'; product_input.mkdir()
        (product_input/'driver-graph.json').write_text('{}\n')
        self.assertNotEqual(runner.case_sources('graph-product',sources,out=product_input),
                            runner.case_sources('graph-product',changed,out=product_input))
        self.assertNotEqual(runner.case_sources('daemon-ready_work_precedes_burst_pressure_ticks',sources),
                            runner.case_sources('daemon-ready_work_precedes_burst_pressure_ticks',changed))

    def test_product_causal_prefix_requires_linked_named_first_failure(self):
        def fixture(control):
            named,source_path,kind,trigger = {
              'C4':('LastRequestAtomicRetirement','store/background_jobs_library.sql',
                'completion_retirement','background_library_request_completed'),
              'C9':('ReservationConservation','store/background_jobs_schema.sql',
                'reservation_release','background_job_settlement_effects')}[control]
            source_sha = runner.c.sha(ROOT/'crates/plurx-core/src'/source_path)
            bindings = {source_path:source_sha}
            before = runner.c.canonical({'durable':'before'})
            normal_after = runner.c.canonical({'durable':'normal'})
            mutant_after = runner.c.canonical({'durable':'mutated'})
            before_sha = hashlib.sha256(before.encode()).hexdigest()
            intermediate_sha = '1'*64
            mutant_sha = hashlib.sha256(mutant_after.encode()).hexdigest()
            driver = {'initial':0,
              'states':[{'id':0,'pending':'AcquireDomain'},{'id':1,'pending':'Bind'}],
              'edges':[{'from':0,'to':1,'request':'Bind','reply':'DomainAcquired'}]}
            marker = {
              'schema_version':1,'control':control,'named_failure':named,
              'scope':'declared_representative_product','ordinary_settlement_payload_transfer':'unclosed',
              'driver_graph_sha256':'d'*64,'source_bindings':bindings,
              'schema_bindings':{'unmutated':'a'*64,'mutated':'b'*64},
              'mutation':{'kind':kind,'trigger':trigger,'source_sha256':source_sha,
                'mutated_trigger_sha256':'c'*64},
              'unmutated_prefix_replay':True,
              'prefix':[
                {'driver_from':None,'request':'Claim','reply':'Claimed','driver_to':None,
                  'primitive':{'op':'claim','job':0,'node':0},'normal_result':'claimed'},
                {'driver_from':0,'request':'AcquireDomain','reply':'DomainAcquired','driver_to':1,
                  'primitive':{'op':'acquire_domain','node':0},'normal_result':'acquired'}],
              'mutated_prefix':[
                {'primitive':{'op':'claim','job':0,'node':0},'result':'claimed','from_sha256':before_sha,
                  'to_sha256':intermediate_sha,'invariant':None},
                {'primitive':{'op':'acquire_domain','node':0},'result':'acquired','from_sha256':intermediate_sha,
                  'to_sha256':mutant_sha,'invariant':named}],
              'identities':{'normal_predecessor':before,'normal_successor':normal_after,
                'mutated_predecessor':before,'mutated_successor':mutant_after},
              'first_causal_violation':{'step':1,'name':named}}
            return marker,driver,bindings,named

        for control in ('C4','C9'):
            marker,driver,bindings,named = fixture(control)
            output = 'async_store_causal_prefix='+json.dumps(marker,separators=(',',':'),sort_keys=True)
            with self.subTest(control=control),patch.object(runner.c,'product_source_bindings',return_value=bindings,create=True):
                facts = runner.causal_prefix_evidence('control-'+control,output,driver,'d'*64)
                self.assertEqual(facts['causal_prefix_steps'],2)
                negatives = []
                bad = json.loads(json.dumps(marker)); bad['schema_bindings']['mutated'] = bad['schema_bindings']['unmutated']
                negatives.append(('schema-did-not-mutate',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['first_causal_violation']['step'] = 0
                negatives.append(('wrong-final-step',bad,driver))
                bad_driver = json.loads(json.dumps(driver)); bad_driver['edges'] = []
                negatives.append(('absent-driver-edge',marker,bad_driver))
                bad = json.loads(json.dumps(marker)); bad['prefix'][1]['driver_from'] = 2
                negatives.append(('broken-driver-chain',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['prefix'][0]['request'] = None
                negatives.append(('malformed-pre-driver-claim',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['prefix'][1]['request'] = 'Bind'
                negatives.append(('consumed-versus-target-request',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['identities']['mutated_predecessor'] = '{broken'
                negatives.append(('malformed-full-identity',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['mutated_prefix'][0]['from_sha256'] = 'f'*64
                negatives.append(('broken-state-hash-chain',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['mutated_prefix'][0]['invariant'] = named
                negatives.append(('early-causal-violation',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['first_causal_violation']['name'] = 'UnrelatedInvariant'
                negatives.append(('unrelated-causal-violation',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['scope'] = 'source_faithful_adapter'
                negatives.append(('wrong-product-scope',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['ordinary_settlement_payload_transfer'] = 'closed'
                negatives.append(('false-settlement-closure',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['source_bindings'] = {next(iter(bindings)):'0'*64}
                negatives.append(('forged-source-binding',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['mutation']['source_sha256'] = '0'*64
                negatives.append(('forged-mutation-source',bad,driver))
                negatives.append(('unknown-key',{**marker,'unreviewed':True},driver))
                duplicate = output[:-1]+',"schema_version":1}'
                for label,bad_marker,bad_graph in negatives:
                    payload = json.dumps(bad_marker,separators=(',',':'),sort_keys=True)
                    with self.subTest(case=label),self.assertRaises((runner.c.Invalid,ValueError)):
                        runner.causal_prefix_evidence('control-'+control,
                          'async_store_causal_prefix='+payload,bad_graph,'d'*64)
                with self.subTest(case='duplicate-key'),self.assertRaises(runner.c.Invalid):
                    runner.causal_prefix_evidence('control-'+control,
                      'async_store_causal_prefix='+duplicate,driver,'d'*64)

    def test_new_workspace_path_dependency_outside_scope_fails_closed(self):
        crates = ['crates/plurx-core','crates/plurx-worker-control','crates/plurxd','crates/new-input']
        (self.directory/'Cargo.toml').write_text('[workspace]\nmembers = '+json.dumps(crates)+'\n')
        for crate in crates:
            target = self.directory/crate; target.mkdir(parents=True)
            text = '[package]\nname = "fixture"\nversion = "0.1.0"\n'
            if crate.endswith('plurx-worker-control'):
                text += '[dependencies]\nnew-input = { path = "../new-input" }\n'
            (target/'Cargo.toml').write_text(text)
        with patch.object(runner,'ROOT',self.directory),self.assertRaisesRegex(runner.c.Invalid,'omits workspace path dependency'):
            runner.assert_scope_closures()

    def test_absent_is_fresh_but_malformed_journals_are_not_adopted(self):
        path = self.directory/'provenance.json'
        self.assertIsNone(runner.read_journal(path,'rust'))
        for value in ({'schema_version':2,'cases':[]},{'schema_version':1,'cases':{}},
                      {'schema_version':2,'cases':{'unknown':{}}}):
            path.write_text(json.dumps(value))
            with self.subTest(value=value),self.assertRaises(runner.c.Invalid): runner.read_journal(path,'rust')
        path.write_text('{')
        with self.assertRaises(ValueError): runner.read_journal(path,'rust')

    def test_tlc_semantic_identity_excludes_provenance_but_binds_config(self):
        for name in ('SourceTables.tla','AsyncDriverRefinement.tla','BoundaryControls.tla'):
            (self.directory/name).write_text('synthetic identity fixture only')
        for name in runner.c.CONFIGS: (self.directory/f'{name}.cfg').write_text('fixture')
        metadata = {'graph_sha256':{'driver':'a','product':'b'}}
        first = runner.tlc_identities(self.directory,metadata,{'fixture':'tool'})
        (self.directory/'provenance.json').write_text('{"elapsed_seconds":17}')
        self.assertEqual(first,runner.tlc_identities(self.directory,metadata,{'fixture':'tool'}))
        (self.directory/'cleanup.cfg').write_text('changed')
        self.assertNotEqual(first,runner.tlc_identities(self.directory,metadata,{'fixture':'tool'}))

    def test_non_pr_snapshot_is_fresh_and_excludes_builds(self):
        work = self.directory/'work'; work.mkdir()
        (work/'provenance.json').write_text('{}')
        (work/'selector-target').mkdir(); (work/'selector-target/binary').write_bytes(b'not evidence')
        output = self.directory/'snapshot'
        env = {'GITHUB_WORKSPACE':str(ROOT),'GITHUB_RUN_ID':'9','GITHUB_RUN_ATTEMPT':'1','GITHUB_EVENT_NAME':'workflow_dispatch'}
        with patch.dict(os.environ,env):
            adapter.snapshot(argparse.Namespace(working_dir=work,output_dir=output,origin=None))
        self.assertFalse((output/'selector-target').exists())
        self.assertEqual(json.loads((output/'resume-origin.json').read_text())['result_class'],'fresh_non_pr_execution')
        self.assertFalse(json.loads((output/'snapshot-identity.json').read_text())['hydrated_acceptance'])

class ExplicitTransportAuthentication(unittest.TestCase):
    def fixture(self):
        sha = 'a'*40
        scope = {'repository':1,'pr':2,'branch':'codex/fixture','base':'main'}
        run = {'id':11,'workflow_id':'main-fast-lane.yml','prettyref':'#2','event':'pull_request',
          'commit_sha':sha,'repository':{'id':1},'event_payload':json.dumps({'repository':{'id':1},'number':2,
          'pull_request':{'number':2,'head':{'ref':'codex/fixture','sha':sha,'repo':{'id':1}},
            'base':{'ref':'main','repo':{'id':1}}}})}
        job = {'id':12,'name':'fast Rust gate','repo_id':1,'run_id':11,'attempt':1,'task_id':13,'status':'failure'}
        artifact = {'id':14,'name':'async-driver-refinement-11-1','run_id':11,'expired':False,'size_in_bytes':100}
        log = (f'{sha}:refs/remotes/pull/2/head\nSHA256 hash of uploaded artifact zip is '+('b'*64)+'\n'
          'Artifact async-driver-refinement-11-1 has been successfully uploaded! Final size is 100 bytes. Artifact ID is 14\n')
        class API:
            def get(self,path): return run
            def pages(self,path,query=None,field=None):
                if path.endswith('/jobs'): return [job]
                if path == '/actions/artifacts': return [artifact] if query['name'] == artifact['name'] else []
                return [run]
            def bytes(self,path): return log.encode()
        return API(),scope,run,job,artifact

    def test_explicit_job_prefix_still_requires_run_event_checkout_and_upload_receipt(self):
        api,scope,run,job,artifact = self.fixture()
        # Invocation equality has a separate real-Git fixture. This fixture
        # isolates transport authentication without weakening its other checks.
        spec = shared.TransportSpec(proof_job='fast Rust gate',artifact_prefix='async-driver-refinement',
          trust=lambda *args:{'decision':'fixture_invocation_only'},max_archive_bytes=adapter.MAX_TOTAL_BYTES)
        self.assertIsNotNone(shared.artifact_for_run(api,scope,run,20,run['commit_sha'],ROOT,spec=spec))
        job['name'] = 'unrelated job'
        self.assertIsNone(shared.artifact_for_run(api,scope,run,20,run['commit_sha'],ROOT,spec=spec))
        job['name'] = 'fast Rust gate'
        run['event_payload'] = json.dumps({'repository':{'id':1},'number':999,'pull_request':{}})
        with self.assertRaises((receipts.ReceiptError,KeyError)):
            shared.artifact_for_run(api,scope,run,20,run['commit_sha'],ROOT,spec=spec)

    def test_legacy_none_profile_reads_bounds_at_call_time(self):
        with patch.object(shared,'MAX_ARCHIVE_BYTES',17):
            self.assertEqual(shared.legacy_transport_spec().max_archive_bytes,17)
            self.assertEqual(shared.legacy_extraction_spec().max_archive_bytes,17)
