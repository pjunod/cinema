"""Refusal controls for complete command-scoped unit evidence."""
import base64
import copy
import hashlib
import unittest
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
    raw = log('running 2 tests\ntest a ... ok\ntest b ... ok\n\ntest result: ok. 2 passed; 0 failed; 0 ignored;\n')
    record = {'sequence': 1, 'source_commit': CANDIDATE, 'environment': copy.deepcopy(environment),
              'origin': {'kind': 'attested-local', 'identity': 'actual owned effort executor', 'command': 'binary --test-threads=1'},
              'changes': {}, 'log': {'sha256_ref': raw['sha256']}, 'exit_code': 0,
              'coverage': {'mode': 'rust', 'first_line': 1, 'last_line': 5, 'format': 'rust-serial', 'outcomes': {'a': 'pass', 'b': 'pass'}}}
    item = {'name': 'store_contract:1', 'invocation': COMMAND, 'environment': environment,
            'inventory': {'source_commit': CANDIDATE, 'ids': ['a', 'b'], 'list_log': log('a: test\nb: test\n'),
                          'binary_sha256': 'c' * 64, 'features': features}, 'records': [record], 'obligations': []}
    return {'version': 1, 'source_commit': CANDIDATE, 'commands': [item], 'logs': {raw['sha256']: raw}}


class CompleteCommandUnitCase(unittest.TestCase):
    def validate(self, proof, commands=None, invalid=()):
        with patch.object(q, 'changed_inputs', return_value=set(invalid)):
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

    def test_original_failed_command_needs_explicit_completion_obligation(self):
        proof=fixture();proof['commands'][0]['records'][0]['exit_code']=101
        with self.assertRaises(receipts.ReceiptError):self.validate(proof)
        proof['commands'][0]['obligations']=[{'sequence':1,'source_commit':CANDIDATE,'invocation':COMMAND,
            'native_pass_count':2,'reason':'preserved original wrapper count failure; complete current native count verified'}]
        self.assertEqual(self.validate(proof)['store_contract']['mode'],'retained-complete-command-union')
        proof['commands'][0]['obligations'][0]['native_pass_count']=1
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
