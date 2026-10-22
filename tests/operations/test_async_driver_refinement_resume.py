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
          'provenance.json','driver-graph.json','store-graph.json','rust-driver-closure.log',
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
          [('../store-graph.json',b'{}',stat.S_IFREG|0o600)],
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
        count,size = adapter.extract_evidence(archive([('store-graph.json',payload,stat.S_IFREG|0o600)]),self.directory)
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
    def test_store_edit_preserves_leaf_but_invalidates_store_and_daemon_scopes(self):
        sources = {'Cargo.toml':'root','Cargo.lock':'lock','rust-toolchain.toml':'tool',
          'crates/plurx-worker-control/src/lib.rs':'leaf','crates/plurx-core/src/store/mod.rs':'store',
          'crates/plurxd/src/library_work.rs':'daemon'}
        changed = dict(sources); changed['crates/plurx-core/src/store/mod.rs'] = 'changed'
        self.assertEqual(runner.case_sources('graph-driver',sources),runner.case_sources('graph-driver',changed))
        for case in ('graph-store','daemon-ready_work_precedes_burst_pressure_ticks'):
            self.assertNotEqual(runner.case_sources(case,sources),runner.case_sources(case,changed))

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
        metadata = {'graph_sha256':{'driver':'a','store':'b'}}
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
