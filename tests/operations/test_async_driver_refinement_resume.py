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
import sys
import time
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


class RustCommandDiagnostics(OwnedScratch):
    def command(self, code):
        return [sys.executable,'-u','-c',code]

    def capture(self, code, **kwargs):
        with patch.object(runner,'TASK_SCRATCH',self.directory/'scratch'):
            return runner.run_rust_case(self.command(code),env=os.environ.copy(),
              log=self.directory/'rust-driver-closure.log',**kwargs)

    def test_runtime_diagnostic_unit_is_bound_once_before_product_closure(self):
        name = 'store-product_runtime_diagnostic_records_are_bounded'
        case = runner.PRODUCT_RUNTIME_TEST
        self.assertEqual(runner.STORE_TESTS['core_tests'].count(case),1)
        self.assertEqual(sum(case in argv for argv in runner.CASES.values()),1)
        self.assertLess(list(runner.CASES).index(name),list(runner.CASES).index('graph-product'))
        self.assertEqual(runner.CASES[name],runner.CORE+[case,'--','--exact','--nocapture'])
        source = 'crates/plurx-core/src/store/async_driver_product.rs'
        sources = {source:'a'*64,**{path:'b'*64 for path in adapter.INVOCATION_PATHS}}
        self.assertEqual(runner.case_sources(name,sources,out=self.directory),sources)
        output = f'test {case} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n'
        self.assertEqual(runner.case_evidence(name,output,0),
          {'test_count':1,'result_class':'product_runtime_diagnostic_records_are_bounded'})

    def test_forged_accepted_capture_cannot_reuse_or_validate_provenance(self):
        name = 'driver-closure'
        argv = runner.rendered_command(name,self.directory)
        text = 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n'
        raw = text.encode()+b'\n'
        log = self.directory/f'rust-{name}.log'; log.write_bytes(raw)
        sources = {path:'a'*64 for path in adapter.INVOCATION_PATHS}
        tools = {'fixture':'pinned'}
        capture = {'timeout_seconds':1800,'timed_out':False,'output_limit_exceeded':False,
          'complete':True,'failure':None,'stdout_bytes':len(text.encode()),'stderr_bytes':0,
          'stdout_sha256':hashlib.sha256(text.encode()).hexdigest(),'stderr_sha256':hashlib.sha256(b'').hexdigest()}
        row = {'argv':argv,'exit_code':0,'log_sha256':runner.c.sha(log),'elapsed_seconds':0.1,
          'test_count':1,'accepted':True,'result_class':'focused_rust_regression','scope':'leaf',
          'input_sha256':runner.case_sources(name,sources,out=self.directory),'output_capture':capture}
        driver = self.directory/'driver-graph.json'; driver.write_text('{}')
        product = self.directory/'product-bundle.json'; product.write_text('{}')
        journal = {'schema_version':2,'complete':True,'source_sha256':sources,'rust_toolchain':tools,
          'cases':{name:row},'graphs':{'driver':{'sha256':runner.c.sha(driver)},'product':{'sha256':runner.c.sha(product)}}}
        path = self.directory/'provenance.json'
        with patch.object(runner,'CASES',{name:runner.CASES[name]}), \
             patch.object(runner,'source_identity',return_value=sources), \
             patch.object(runner,'rust_identity',return_value=tools):
            self.assertTrue(runner.reusable_case(name,row,self.directory,sources))
            path.write_text(json.dumps(journal))
            runner.validate_provenance(path,driver,product)
            mutations = [None,{'complete':False},{'timed_out':True},{'timed_out':0},
              {'output_limit_exceeded':True},{'failure':'deadline exceeded'},
              {'timeout_seconds':3600},{'stdout_bytes':True},{'stderr_bytes':1},
              {'stdout_sha256':'0'*64},{'stderr_sha256':'0'*64}]
            for mutation in mutations:
                changed = json.loads(json.dumps(row))
                if mutation is None: changed.pop('output_capture')
                else: changed['output_capture'].update(mutation)
                with self.subTest(mutation=mutation),patch.object(runner,'case_evidence') as parser:
                    self.assertFalse(runner.reusable_case(name,changed,self.directory,sources))
                    journal['cases'][name] = changed; path.write_text(json.dumps(journal))
                    with self.assertRaises(runner.c.Invalid): runner.validate_provenance(path,driver,product)
                    parser.assert_not_called()
            # Even an updated combined-log SHA cannot hide a truncated stream.
            log.write_bytes(raw[:-1]); changed = json.loads(json.dumps(row))
            changed['log_sha256'] = runner.c.sha(log)
            self.assertFalse(runner.reusable_case(name,changed,self.directory,sources))

    def test_rust_capture_preserves_exact_json_stdout_and_separate_stderr(self):
        stdout = '{"exact":[true,null,"é"]}\n'
        stderr = 'diagnostic only\n'
        proc,_,capture = self.capture(f'import sys; sys.stdout.write({stdout!r}); sys.stderr.write({stderr!r})')
        self.assertEqual(proc.returncode,0)
        self.assertEqual((proc.stdout,proc.stderr),(stdout,stderr))
        self.assertIsNone(capture['failure'])
        self.assertTrue(capture['complete'])
        self.assertEqual(capture['stdout_bytes'],len(stdout.encode()))
        self.assertEqual(capture['stderr_sha256'],hashlib.sha256(stderr.encode()).hexdigest())
        self.assertEqual((self.directory/'rust-driver-closure.log').read_bytes(),
          stdout.encode()+b'\n'+stderr.encode())
        self.assertEqual(json.loads(proc.stdout),{'exact':[True,None,'é']})

    def test_rust_timeout_retains_output_and_explicit_failed_identity_before_raise(self):
        argv = self.command('import sys,time; print("partial stdout",flush=True); print("partial stderr",file=sys.stderr,flush=True); time.sleep(30)')
        sources = {'scripts/check-async-driver-refinement':'a'*64}
        tools = {'rustc':{'version':'fixture-pinned'},'cargo':{'version':'fixture-pinned'}}
        real_capture = runner.run_rust_case
        def short_capture(command, **kwargs):
            return real_capture(command,timeout=0.3,**kwargs)
        with patch.object(runner,'TASK_SCRATCH',self.directory/'scratch'), \
             patch.object(runner,'source_identity',return_value=sources), \
             patch.object(runner,'rust_identity',return_value=tools), \
             patch.object(runner,'rendered_command',return_value=argv), \
             patch.object(runner,'reusable_case',return_value=False), \
             patch.object(runner,'case_sources',return_value=sources), \
             patch.object(runner,'refresh_graphs'), \
             patch.object(runner,'case_evidence') as parser, \
             patch.object(runner,'run_rust_case',side_effect=short_capture):
            with self.assertRaisesRegex(runner.c.Invalid,'no semantic result'):
                runner.build_inputs(argparse.Namespace(output_dir=self.directory,case='driver-closure'))
        parser.assert_not_called()
        journal = json.loads((self.directory/'provenance.json').read_text())
        row = journal['cases']['driver-closure']
        self.assertFalse(journal['complete'])
        self.assertEqual(journal['rust_toolchain'],tools)
        self.assertEqual(journal['source_sha256'],sources)
        self.assertEqual((row['argv'],row['input_sha256']),(argv,sources))
        self.assertEqual((row['accepted'],row['test_count'],row['result_class']),
          (False,0,'failed_command'))
        self.assertTrue(row['output_capture']['timed_out'])
        self.assertFalse(row['output_capture']['complete'])
        self.assertEqual(row['output_capture']['timeout_seconds'],0.3)
        self.assertGreaterEqual(row['elapsed_seconds'],0.3)
        self.assertLess(row['exit_code'],0)
        log = self.directory/'rust-driver-closure.log'
        self.assertEqual(row['log_sha256'],runner.c.sha(log))
        self.assertIn(b'partial stdout',log.read_bytes())
        self.assertIn(b'partial stderr',log.read_bytes())
        self.assertEqual(journal['graphs'],{})

    def test_rust_capture_output_bound_and_spawn_failure_cannot_pass(self):
        with patch.object(runner,'rust_capture_limit',return_value=31):
            proc,_,capture = self.capture('import sys; sys.stdout.write("x"*5000); sys.stdout.flush()')
        self.assertTrue(capture['output_limit_exceeded'])
        self.assertIsNotNone(capture['failure'])
        self.assertLessEqual((self.directory/'rust-driver-closure.log').stat().st_size,32)
        with patch.object(runner,'TASK_SCRATCH',self.directory/'scratch'):
            proc,_,capture = runner.run_rust_case([str(self.directory/'absent-command')],
              env=os.environ.copy(),log=self.directory/'rust-driver-closure.log')
        self.assertEqual(proc.returncode,127)
        self.assertIsNotNone(capture['failure'])

    @unittest.skipUnless(os.name == 'posix','owned process sessions require POSIX')
    def test_rust_timeout_terminates_owned_child_even_after_parent_exit(self):
        code = ('import subprocess,sys; child=subprocess.Popen([sys.executable,"-c",'
          '"import time; time.sleep(30)"]); print(child.pid,flush=True)')
        proc,_,capture = self.capture(code,timeout=0.3)
        self.assertTrue(capture['timed_out'])
        child = int(proc.stdout.strip())
        # A reparented dead child can briefly remain a zombie. It must never
        # remain runnable after the entire owned session has been killed.
        state = ''
        for _ in range(20):
            state = subprocess.run(['ps','-o','stat=','-p',str(child)],capture_output=True,text=True,check=False).stdout.strip()
            if not state or state.startswith('Z'): break
            time.sleep(0.025)
        self.assertTrue(not state or state.startswith('Z'),f'owned child remains live: {state}')


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
    def test_local_guard_case_binds_exact_driver_graph_input(self):
        name = 'store-actual_ordinary_local_guard_and_response_conformance'
        sources = {'Cargo.toml':'root','Cargo.lock':'lock','rust-toolchain.toml':'tool'}
        sources.update({path:'actual-settlement source fixture' for path in runner.ACTUAL_SETTLEMENT_SOURCE_PATHS})
        sources.update({path:'same invocation bytes' for path in adapter.INVOCATION_PATHS})
        output = self.directory/'local-guard-input'; output.mkdir()
        graph = output/'driver-graph.json'; graph.write_text('{"states":[] }\n')
        baseline = runner.case_sources(name,sources,out=output)
        graph.write_text('{"states":[1]}\n')
        self.assertNotEqual(baseline,runner.case_sources(name,sources,out=output))
        graph.unlink()
        with self.assertRaises(runner.c.Invalid):
            runner.case_sources(name,sources,out=output)

    def test_external_input_formal_cases_use_one_exact_ignored_selection(self):
        selected = {
          'graph-product':runner.STORE_TESTS['product_test'],
          'control-C4':runner.STORE_TESTS['product_controls']['C4'],
          'control-C9':runner.STORE_TESTS['product_controls']['C9'],
        }
        selected.update({'store-'+case.rsplit('::',1)[-1]:case
          for case in runner.STORE_TESTS['core_tests']
          if case.rsplit('::',1)[-1] in {
            'actual_ordinary_settlement_payload_and_time_conformance',
            'actual_ordinary_local_guard_and_response_conformance',
            'actual_queued_eligibility_progress_witness'}})
        self.assertEqual(set(selected),runner.IGNORED_INPUT_CASES)
        for name,test_name in selected.items():
            with self.subTest(name=name):
                argv = runner.CASES[name]
                separator = argv.index('--')
                self.assertEqual(argv[separator+1:separator+4],['--ignored','--exact','--nocapture'])
                self.assertNotIn('--include-ignored',argv)
                self.assertEqual(sum(test_name in command for command in runner.CASES.values()),1)
                marker = ''
                if name in {'control-C4','control-C9'}:
                    control = name.removeprefix('control-')
                    failure = 'LastRequestAtomicRetirement' if control == 'C4' else 'ReservationConservation'
                    marker = f'async_store_control={{"control":"{control}","named_failure":"{failure}"}}\n'
                summary = 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1476 filtered out; finished in 0.00s\n'
                output = f'test {test_name} ... ok\n'+marker+summary
                runner.case_evidence(name,output,0)
        core_name = 'store-actual_ordinary_settlement_payload_and_time_conformance'
        core_test = 'store::async_driver_refinement::actual_initial_claim_recovery_queries_preserve_dispatch_and_read_rows'
        core_command = runner.CASES['store-actual_initial_claim_recovery_queries_preserve_dispatch_and_read_rows']
        self.assertNotIn('--ignored',core_command)
        self.assertIn('--exact',core_command)
        self.assertEqual(sum(core_test in command for command in runner.CASES.values()),1)
        runner.case_evidence('store-actual_initial_claim_recovery_queries_preserve_dispatch_and_read_rows',
          f'test {core_test} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1476 filtered out; finished in 0.00s\n',0)
        with self.assertRaises(runner.c.Invalid):
            runner.case_evidence(core_name,
              'test store::async_driver_refinement::actual_ordinary_settlement_payload_and_time_conformance ... ok\n'
              'test result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 1476 filtered out; finished in 0.00s\n',0)

    def test_retry_policy_leaf_units_are_scheduled_once_with_exact_attribution(self):
        expected = {
          'driver-retry-policy-bases':('retry_policy::tests::retry_bases_preserve_attempt_thresholds',
            'retry_policy::tests::retry_bases_preserve_attempt_thresholds'),
          'driver-retry-policy-jitter':('retry_policy::tests::raw_id_bytes_and_wrapping_signed_seed_preserve_jitter',
            'retry_policy::tests::raw_id_bytes_and_wrapping_signed_seed_preserve_jitter'),
          'daemon-ordinary_settlement_local_guards_and_validation_preserve_owner':
            ('background_jobs::tests::ordinary_settlement_local_guards_and_validation_preserve_owner',
             'ordinary_settlement_local_guards_and_validation_preserve_owner'),
        }
        for name,(test_name,result_class) in expected.items():
            with self.subTest(name=name):
                argv = runner.CASES[name]
                self.assertEqual(sum(test_name in command for command in runner.CASES.values()),1)
                self.assertEqual(argv[-4:], [test_name,'--','--exact','--nocapture'])
                output = f'test {test_name} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n'
                evidence = runner.case_evidence(name,output,0)
                self.assertEqual(evidence,{'test_count':1,'result_class':result_class})
    def test_product_wire_units_precede_closure_once_with_exact_current_sources(self):
        expected = {
          'product_wire_pools_preserve_exact_types_order_and_complex_worker_values',
          'product_wire_compact_keys_are_bijective_with_full_logical_payloads',
          'product_wire_byte_accounting_and_stream_writer_fail_closed',
          'product_wire_store_export_omits_only_redundant_raw',
        }
        self.assertEqual(runner.PRODUCT_WIRE_TESTS,expected)
        order = list(runner.CASES)
        source = 'crates/plurx-core/src/store/async_driver_product.rs'
        sources = {source:'a'*64,**{path:'b'*64 for path in adapter.INVOCATION_PATHS}}
        for short in expected:
            name = 'store-'+short;test_name = 'store::async_driver_product::'+short
            with self.subTest(name=name):
                argv = runner.CASES[name]
                self.assertLess(order.index(name),order.index('graph-product'))
                self.assertEqual(sum(test_name in command for command in runner.CASES.values()),1)
                self.assertEqual(argv,runner.CORE+[test_name,'--','--exact','--nocapture'])
                self.assertNotIn('--ignored',argv)
                self.assertNotIn(name,runner.DRIVER_INPUT_CASES | runner.IGNORED_INPUT_CASES)
                self.assertEqual(runner.case_scope(name),'store')
                bound = runner.case_sources(name,sources,out=self.directory)
                self.assertEqual(bound,sources)
                changed = dict(sources);changed[source] = 'c'*64
                self.assertNotEqual(bound,runner.case_sources(name,changed,out=self.directory))
                output = f'test {test_name} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n'
                self.assertEqual(runner.case_evidence(name,output,0),{'test_count':1,'result_class':short})
                for invalid in (output.replace('1 passed','0 passed'),output.replace(test_name,'another::test'),output+output):
                    with self.assertRaises(runner.c.Invalid): runner.case_evidence(name,invalid,0)

    def test_ordinary_settlement_boundary_closes_only_in_complete_receipt(self):
        partial = runner.settlement_receipt_boundary(False)
        complete = runner.settlement_receipt_boundary(True)
        self.assertEqual(partial[1],runner.PRODUCT_SOURCE_BOUNDARIES)
        self.assertEqual(partial[2],'pending')
        self.assertIn('Pending',partial[0]['ordinary_settlement_payload_transfer'])
        self.assertIn('pending',partial[3])
        self.assertEqual(complete[1],{'ordinary_settlement_payload_transfer':{
          'status':'closed_bounded','domain':'selected_two_worker_normal_clock'}})
        self.assertEqual(complete[2],'closed_bounded')
        self.assertIn('closed_bounded',complete[0]['ordinary_settlement_payload_transfer'])
        self.assertIn('not a full Rust/Tokio/cluster trace theorem',complete[3])

    def test_semantic_invocation_changes_invalidate_persistent_case_rows(self):
        sources = {'Cargo.toml':'root','Cargo.lock':'lock','rust-toolchain.toml':'tool',
          'crates/plurx-core/src/store/async_driver_product.rs':'product'}
        sources.update({path:'settlement source fixture' for path in runner.ACTUAL_SETTLEMENT_SOURCE_PATHS})
        sources.update({path:'same invocation bytes' for path in adapter.INVOCATION_PATHS})
        product_input = self.directory/'product-input'; product_input.mkdir()
        raw_driver = product_input/'driver-graph.json'; raw_driver.write_text('{"states":[] }\n')
        baseline = runner.case_sources('graph-product',sources,out=product_input)
        for path in ('verification/async-driver-refinement/checker.py',
                     'verification/async-driver-refinement/product-schema.json',
                     'validation/async_refinement_resume.py',
                     'validation/worker_control_resume.py',
                     *runner.ACTUAL_SETTLEMENT_SOURCE_PATHS):
            changed = dict(sources); changed[path] = 'changed invocation bytes'
            with self.subTest(path=path):
                self.assertNotEqual(baseline,runner.case_sources('graph-product',changed,out=product_input))
        self.assertEqual(baseline,runner.case_sources('graph-product',dict(sources),out=product_input))
        raw_driver.write_text('{"states":[1]}\n')
        self.assertNotEqual(baseline,runner.case_sources('graph-product',sources,out=product_input))

    def test_product_semantic_table_changes_invalidate_embedded_bundle(self):
        sources = {'Cargo.toml':'root','Cargo.lock':'lock','rust-toolchain.toml':'tool',
          'crates/plurx-core/src/store/async_driver_product.rs':'product'}
        sources.update({path:'actual-settlement source fixture' for path in runner.ACTUAL_SETTLEMENT_SOURCE_PATHS
                        if path not in sources})
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
        sources.update({path:'actual-settlement source fixture' for path in runner.ACTUAL_SETTLEMENT_SOURCE_PATHS
                        if path not in sources})
        sources.update({path:'same invocation bytes' for path in adapter.INVOCATION_PATHS})
        changed = dict(sources); changed['crates/plurx-core/src/store/mod.rs'] = 'changed'
        self.assertEqual(runner.case_sources('graph-driver',sources),runner.case_sources('graph-driver',changed))
        policy_changed = dict(sources)
        policy_changed['crates/plurx-worker-control/src/retry_policy.rs'] = 'changed'
        self.assertNotEqual(runner.case_sources('store-actual_initial_claim_recovery_queries_preserve_dispatch_and_read_rows',sources),
                            runner.case_sources('store-actual_initial_claim_recovery_queries_preserve_dispatch_and_read_rows',policy_changed))
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
            raw_ids = ['00000000-0000-4000-8000-000000000001',
                       '00000000-0000-4000-8000-000000000002']
            snapshot = {'job':0,'raw_job_id':raw_ids[0],'failed_attempts':0,
              'claim_id':'00000000-0000-4000-8000-000000000010',
              'token_capture':{'job_id':raw_ids[0],'node_id':'node-a',
                'boot_id':'00000000-0000-4000-8000-000000000011',
                'claim_id':'00000000-0000-4000-8000-000000000010','fence':1,
                'revision':1,'lease_expires_ms':31000}}
            before = runner.c.canonical({'durable':'before'})
            normal_after = runner.c.canonical({'durable':'normal'})
            mutant_after = runner.c.canonical({'durable':'mutated'})
            before_sha = hashlib.sha256(before.encode()).hexdigest()
            intermediate_sha = '1'*64
            mutant_sha = hashlib.sha256(mutant_after.encode()).hexdigest()
            if control == 'C4':
                steps = [
                  ('AcquireDomain','DomainAcquired',{'op':'acquire_domain','node':0},'acquired',
                    {'tag':'DomainAcquired','now_ms':0,'retry_delay_ms':0},None),
                  ('Bind','BindAccepted',{'op':'bind','job':0,'token':0,'lease':0},'bound',
                    {'tag':'BindAccepted','now_ms':None,'retry_delay_ms':None},None),
                  ('PollWork','WorkSucceeded',{'op':'complete','job':0,'token':0,'lease':0},'completed',
                    {'tag':'WorkSucceeded','now_ms':None,'retry_delay_ms':None},None),
                ]
            else:
                steps = [
                  ('AcquireDomain','DomainAcquired',{'op':'acquire_domain','node':0},'acquired',
                    {'tag':'DomainAcquired','now_ms':0,'retry_delay_ms':0},None),
                  ('Bind','BindAccepted',{'op':'bind','job':0,'token':0,'lease':0},'bound',
                    {'tag':'BindAccepted','now_ms':None,'retry_delay_ms':None},None),
                  ('PollWork','Pressure',None,'pressure',
                    {'tag':'Pressure','now_ms':None,'retry_delay_ms':None},None),
                  ('RevokeDomain','Done',None,'revoked',
                    {'tag':'Done','now_ms':None,'retry_delay_ms':None},None),
                  ('CancelWork','Done',None,'cancelled',
                    {'tag':'Done','now_ms':None,'retry_delay_ms':None},None),
                  ('JoinWork','JoinedFailed',None,'joined',
                    {'tag':'JoinedFailed','now_ms':None,'retry_delay_ms':None},None),
                  ('DropPublisher','Done',None,'dropped',
                    {'tag':'Done','now_ms':None,'retry_delay_ms':None},None),
                  ('ReleaseInterrupted','ReleaseReturned',{'op':'release','lease':0},'released',
                    {'tag':'ReleaseReturned','now_ms':1000,'retry_delay_ms':0},
                    {'kind':'yield','not_before_ms':2000}),
                  ('SettleYield','Done',{'op':'ordinary_settle','job':0,'token':0,'kind':'yield',
                    'not_before_ms':2000,'request_now_ms':1000,'error_code':None,'checkpoint':None},'settled',
                    {'tag':'Done','now_ms':None,'retry_delay_ms':None},
                    {'kind':'yield','not_before_ms':2000}),
                ]
            states = [{'id':0,'pending':steps[0][0]}]
            states.extend({'id':index+1,'pending':steps[index+1][0] if index+1 < len(steps) else 'RetireQueue'}
                          for index in range(len(steps)))
            driver = {'initial':0,'states':states,
              'edges':[{'from':index,'to':index+1,'request':states[index+1]['pending'],'reply':step[1]}
                       for index,step in enumerate(steps)]}
            prefix = [{'driver_from':None,'request':'Claim','reply':'Claimed','driver_to':None,
              'primitive':{'op':'claim','job':0,'node':0},'normal_result':'claimed',
              'adapter_reply':None,'driver_settlement':None,'claim_snapshot':snapshot}]
            prefix.extend({'driver_from':index,'request':step[0],'reply':step[1],'driver_to':index+1,
              'primitive':step[2],'normal_result':step[3],'adapter_reply':step[4],
              'driver_settlement':step[5],'claim_snapshot':None}
              for index,step in enumerate(steps))
            marker = {
              'schema_version':1,'control':control,'named_failure':named,
              'scope':'bounded_actual_settlement_product','ordinary_settlement_payload_transfer':'pending',
              'driver_graph_sha256':'d'*64,'source_bindings':bindings,
              'schema_bindings':{'unmutated':'a'*64,'mutated':'b'*64},
              'mutation':{'kind':kind,'trigger':trigger,'source_sha256':source_sha,
                'mutated_trigger_sha256':'c'*64},
              'unmutated_prefix_replay':True,
              'fixture_identity':{'jobs':[{'job':0,'raw_job_id':raw_ids[0]},
                {'job':1,'raw_job_id':raw_ids[1]}]},
              'prefix':prefix,
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
            def settlement_after(previous,pending,reply):
                if pending == 'ReleaseInterrupted': return {'kind':'yield','not_before_ms':reply['now_ms']+1000}
                return previous
            with self.subTest(control=control),patch.object(runner.c,'product_source_bindings',return_value=bindings,create=True), \
                    patch.object(runner.c,'settlement_after_reply',side_effect=settlement_after,create=True):
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
                bad = json.loads(json.dumps(marker)); bad['fixture_identity']['jobs'][0]['raw_job_id'] = marker['fixture_identity']['jobs'][1]['raw_job_id']
                negatives.append(('claim-fixture-raw-id-mismatch',bad,driver))
                bad = json.loads(json.dumps(marker)); bad['prefix'][0]['claim_snapshot']['token_capture']['job_id'] = marker['fixture_identity']['jobs'][1]['raw_job_id']
                negatives.append(('forged-claim-token-identity',bad,driver))
                if control == 'C9':
                    release_rows = [index for index,row in enumerate(marker['prefix'])
                      if row['request'] == 'ReleaseInterrupted' and row['reply'] == 'ReleaseReturned']
                    settle_rows = [index for index,row in enumerate(marker['prefix'])
                      if isinstance(row['primitive'],dict) and row['primitive'].get('op') == 'ordinary_settle']
                    self.assertEqual(len(release_rows),1)
                    self.assertEqual(len(settle_rows),1)
                    release_index,settle_index = release_rows[0],settle_rows[0]
                    self.assertEqual(marker['prefix'][release_index]['adapter_reply']['tag'],'ReleaseReturned')
                    self.assertEqual(marker['prefix'][settle_index]['request'],'SettleYield')
                    bad = json.loads(json.dumps(marker)); bad['prefix'][release_index]['adapter_reply']['retry_delay_ms'] = 1
                    negatives.append(('forged-release-payload',bad,driver))
                    bad = json.loads(json.dumps(marker)); bad['prefix'][settle_index]['primitive']['request_now_ms'] = -1
                    negatives.append(('negative-settlement-request-time',bad,driver))
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
