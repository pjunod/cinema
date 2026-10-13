"""Refusal controls for complete command-scoped unit evidence."""
import base64
import copy
import hashlib
import unittest
import shlex
import sys
from unittest.mock import patch
from validation import qualification_unit_continuation as q
from validation import python_unit_receipts as receipts

CANDIDATE = 'b' * 40
COMMAND = 'RUST_MIN_STACK=8388608 $(CARGO) test --locked -p plurx-core --features cluster-read-cost-validation,hiqlite-contract-tests --test store_contract -- --test-threads=1'


def log(text):
    raw = text.encode()
    return {'base64': base64.b64encode(raw).decode(), 'sha256': hashlib.sha256(raw).hexdigest()}


def fixture():
    features = ['cluster-read-cost-validation', 'hiqlite-contract-tests', 'hiqlite-store']
    environment = {'platform': 'linux-x64', 'toolchain': 'rust1.97.1', 'features': features,
                   'runtime': {'test_threads': 1, 'rust_min_stack': 8388608}}
    records=[]; logs={}
    for sequence,name in enumerate(['a','b'],1):
        argv=['/owned/store_contract-deadbeef',name,'--exact','--test-threads=1']
        text='COMMAND '+__import__('json').dumps(argv)+'\nrunning 1 test\ntest '+name+' ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored;\nNATIVE_EXIT 0\n'
        raw=log(text);logs[raw['sha256']]=raw
        records.append({'sequence':sequence,'source_commit':CANDIDATE,'environment':copy.deepcopy(environment),
            'origin':{'kind':'attested-local','identity':'actual owned effort executor','command':shlex.join(argv),
                'execution':{'mode':'local-binary','first_line':1,'build_invocation':COMMAND.replace('$(CARGO)','cargo')+' --no-run',
                    'archive_sha256':'d'*64,'binary_sha256_before':'c'*64,'binary_sha256_after':'c'*64,'features':features,
                    'fingerprint':{'features':__import__('json').dumps(features),'profile':123}}},
            'changes':{},'log':{'sha256_ref':raw['sha256']},'exit_code':0,
            'coverage':{'mode':'rust','first_line':2,'last_line':5,'format':'rust-serial','outcomes':{name:'pass'}}})
    item = {'name': 'store_contract:1', 'invocation': COMMAND, 'environment': environment,
            'inventory': {'source_commit': CANDIDATE, 'ids': ['a', 'b'], 'list_log': log('a: test\nb: test\n'),
                          'binary_sha256': 'c' * 64, 'features': features}, 'records': records, 'obligations': []}
    return {'version': 1, 'source_commit': CANDIDATE, 'commands': [item], 'logs': logs}


class CompleteCommandUnitCase(unittest.TestCase):
    def validate(self, proof, commands=None, invalid=()):
        with patch.object(q, 'changed_inputs', return_value=set(invalid)),patch.object(q,'source_archive_digest',return_value='d'*64):
            return q.validate(proof, CANDIDATE, commands=lambda _: commands or {'store_contract:1': COMMAND})

    def test_complete_actual_union_is_pure_and_candidate_bound(self):
        proof = fixture();before = copy.deepcopy(proof)
        result = self.validate(proof)
        self.assertEqual(result['store_contract']['mode'], 'retained-complete-command-union')
        self.assertEqual(proof, before)
        proof['source_commit'] = 'd' * 40
        with self.assertRaises(receipts.ReceiptError): self.validate(proof)

    def test_missing_command_and_wrong_invocation_refuse(self):
        for mutate in [lambda p: p['commands'][0].update(invocation=COMMAND+' --ignored'),
                       lambda p: p['commands'][0].update(name='wal:99')]:
            proof=fixture();mutate(proof)
            with self.assertRaises(receipts.ReceiptError):self.validate(proof)
        with self.assertRaises(receipts.ReceiptError):
            self.validate(fixture(), {'store_contract:1': COMMAND, 'store_contract:2': COMMAND+' --lib'})

    def test_native_inventory_filter_and_binary_identity_are_required(self):
        for mutate in [lambda i:i.update(ids=['a']),lambda i:i.update(binary_sha256='unknown'),
                       lambda i:i.update(source_commit='d'*40),lambda i:i.update(list_log=log('a: test\na: test\n'))]:
            proof=fixture();mutate(proof['commands'][0]['inventory'])
            with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_runtime_stack_thread_and_feature_mismatch_refuse(self):
        for key,value in [('rust_min_stack',None),('test_threads',8)]:
            proof=fixture();proof['commands'][0]['environment']['runtime'][key]=value
            with self.assertRaises(receipts.ReceiptError):self.validate(proof)
        proof=fixture();proof['commands'][0]['records'][0]['environment']['features']=[]
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_tampered_shared_log_and_unknown_outcome_refuse(self):
        proof=fixture();next(iter(proof['logs'].values()))['sha256']='0'*64
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)
        proof=fixture();proof['commands'][0]['records'][0]['coverage']['outcomes']['a']='fail'
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_changed_input_cannot_reuse_an_invalidated_pass(self):
        with self.assertRaises(receipts.ReceiptError):self.validate(fixture(), invalid={'a'})

    def test_ambiguous_chronology_and_duplicate_commands_refuse(self):
        proof=fixture();proof['commands'][0]['records'].append(copy.deepcopy(proof['commands'][0]['records'][0]))
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)
        proof=fixture();proof['commands'].append(copy.deepcopy(proof['commands'][0]))
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_original_unexplained_command_failure_cannot_be_cleared_by_reason(self):
        proof=fixture();proof['commands'][0]['records'][0]['exit_code']=101
        proof['commands'][0]['obligations']=[{'sequence':1,'source_commit':CANDIDATE,'invocation':COMMAND,
            'native_pass_count':2,'reason':'freeform repaired wrapper'}]
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_full_python_summary_uses_actual_frozen_discovery(self):
        record={'source_commit':CANDIDATE,'coverage':{'mode':'python-discovery','first_line':1,'last_line':3,'ids':['A.a','A.b']}}
        self.assertEqual(q.coverage(record,'Ran 2 tests in 0.2s\n\nOK','validation',lambda c,k:['A.a','A.b']),{'A.a':'pass','A.b':'pass'})
        with self.assertRaises(receipts.ReceiptError):q.coverage(record,'Ran 2 tests in 0.2s\n\nOK','validation',lambda c,k:['A.a'])
        with self.assertRaises(receipts.ReceiptError):q.coverage(record,'Ran 2 tests in 0.2s\n\nOK\nRan 2 tests in 0.3s\n\nOK','validation',lambda c,k:['A.a','A.b'])

    def test_jvm_whole_success_stays_opaque_and_requires_real_tasks(self):
        record={'coverage':{'mode':'jvm-whole-command','first_line':1,'last_line':3,'tasks':['testDebugUnitTest','lintDebug']}}
        raw='> Task :app:testDebugUnitTest\n> Task :app:lintDebug\nBUILD SUCCESSFUL in 1s'
        self.assertEqual(q.coverage(record,raw,'android_jvm'),{'whole-command':'pass'})
        with self.assertRaises(receipts.ReceiptError):q.coverage(record,raw.replace('testDebugUnitTest','testDebugUnitTest UP-TO-DATE'),'android_jvm')

    def test_wal_identity_retains_environment_target_features_and_ignored_policy(self):
        identity=q.rust_identity('RUST_MIN_STACK=8388608 '+COMMAND.split(' ',1)[1].replace('--test-threads=1','--exact --ignored --test-threads=1'))
        self.assertEqual(identity['cargo']['--test'],'store_contract')
        self.assertIn('--ignored',identity['harness'])
        with self.assertRaises(receipts.ReceiptError):q.rust_identity('$(CARGO) test --locked --lib --arbitrary')

    def test_every_changed_path_includes_the_adapter_and_workflow(self):
        record={'source_commit':'a'*40,'changes':{'.github/workflows/ci.yml':{'before':q.retention.digest(b'old'),'after':q.retention.digest(b'new'),'affected_ids':['a'],'reason':'changes current executor'}}}
        with patch.object(q.retention,'changed',return_value={'.github/workflows/ci.yml'}),patch.object(q.retention,'git_bytes',side_effect=lambda c,p:b'old' if c=='a'*40 else b'new'):
            self.assertEqual(q.changed_inputs(record,CANDIDATE),{'a'})
            record['changes']={}
            with self.assertRaises(receipts.ReceiptError):q.changed_inputs(record,CANDIDATE)

    def test_later_invalidated_failure_requires_a_later_applicable_pass(self):
        proof=fixture();item=proof['commands'][0]
        failed=copy.deepcopy(item['records'][0]);failed['sequence']=3;failed['source_commit']='a'*40
        item['records'].append(failed)
        with patch.object(q,'source_archive_digest',return_value='d'*64),patch.object(q,'bound_execution',return_value=''),patch.object(q,'changed_inputs',side_effect=[set(),set(),{'a'}]),patch.object(q,'coverage',side_effect=[{'a':'pass'},{'b':'pass'},{'a':'fail'}]):
            with self.assertRaises(receipts.ReceiptError):q.validate(proof,CANDIDATE,commands=lambda _: {'store_contract:1':COMMAND})

    def test_retained_store_refuses_accelerated_and_shadow_before_jobs(self):
        for mode in ['accelerated', 'shadow', 'unknown']:
            with self.assertRaises(receipts.ReceiptError): q.require_store_mode({'store_contract':{}},mode)
        q.require_store_mode({'store_contract':{}},'legacy')
        q.require_store_mode({},'shadow')

    def test_owned_command_timeout_and_output_limit_refuse(self):
        for command,timeout,limit in [([sys.executable,'-c','import time;time.sleep(5)'],0.05,1024),
                                       ([sys.executable,'-c','print("x"*65536)'],5,1024)]:
            with self.assertRaises(receipts.ReceiptError):q.owned_output(command,timeout=timeout,limit=limit)

    def test_local_hash_malformed_boundary_and_target_mismatch_refuse(self):
        for change in [lambda w:w.update(binary_sha256_after='e'*64),
                       lambda w:w.update(first_line=2),
                       lambda w:w.update(build_invocation=w['build_invocation'].replace('--test store_contract','--test different'))]:
            proof=fixture();change(proof['commands'][0]['records'][0]['origin']['execution'])
            with self.assertRaises(receipts.ReceiptError):self.validate(proof)

    def test_complete_block_cannot_crop_a_failed_outcome(self):
        proof=fixture();row=proof['commands'][0]['records'][0]
        original=q.command_log(proof,row);raw=log(original.replace('NATIVE_EXIT 0','test hidden ... FAILED\nNATIVE_EXIT 0'))
        proof['logs'][raw['sha256']]=raw;row['log']={'sha256_ref':raw['sha256']}
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)
