"""Synthetic protected-RPC regression drafts; no actual source/cases/Gold reads."""
import copy,sys,types,unittest
from unittest.mock import patch
from validation.knowledge.study_driver import python_rpc_contract as codec
from validation.knowledge.study_driver import candidate_python_rpc as worker
from validation.knowledge.study_driver import protected_python_outcome_driver as proxy
from validation.knowledge.study_driver import protected_python_outcome_driver as driver

class SyntheticReceiptError(Exception):pass

class ProtectedRpcContracts(unittest.TestCase):
    def setUp(self):
        self.previous=worker._module;self.handles=worker._handles;self.next=worker._next_handle;self.stack=worker._git_stack
        self.module=types.SimpleNamespace(_TREE_CACHE={},_DIGEST_CACHE={},_HISTORY_CACHE={},JOB='synthetic-job',git=lambda:'original',input_digest=lambda *a:'synthetic-result',validate_journal=lambda *a:None)
        worker._module=self.module;worker._handles={};worker._next_handle=0;worker._git_stack=[]
        self.calls=[]
        def rpc(method,args,kwargs):
            self.calls.append(method)
            return worker.dispatch(method,args,kwargs,{'schema_sha256':codec.SCHEMA_SHA256})
        self.rpc=rpc
        errors={'ReceiptError':SyntheticReceiptError,'AssertionError':AssertionError,'TypeError':TypeError,'ValueError':ValueError,'KeyError':KeyError}
        self.remote=proxy.make_module(rpc,errors)
    def tearDown(self):
        worker._module=self.previous;worker._handles=self.handles;worker._next_handle=self.next;worker._git_stack=self.stack
    def test_opaque_cache_identity_survives_shallow_snapshot_and_replacement(self):
        self.module._TREE_CACHE['old']={'leaf':('mode','blob','digest')}
        saved=dict(self.remote._TREE_CACHE)
        self.remote._TREE_CACHE['old']={'replacement':1}
        self.assertEqual(saved['old']['leaf'],('mode','blob','digest'))
        self.remote._TREE_CACHE.clear();self.remote._TREE_CACHE.update(saved)
        self.remote._TREE_CACHE['old']['new']=2
        self.assertEqual(self.module._TREE_CACHE['old']['new'],2)
        deep=copy.deepcopy(self.remote._TREE_CACHE);deep['old']['new']=3
        self.assertEqual(self.module._TREE_CACHE['old']['new'],2)
    def test_mock_override_is_remote_and_restored_without_callable_serialization(self):
        with patch.object(self.remote,'git',side_effect=AssertionError('synthetic refusal')):
            with self.assertRaises(AssertionError):self.module.git()
        self.assertEqual(self.module.git(),'original');self.assertIn('git_stub',self.calls);self.assertIn('git_restore',self.calls)
    def test_unknown_operations_and_executable_wire_values_refuse(self):
        with self.assertRaises(worker.ProtocolError):self.rpc('arbitrary_import',['os'],{})
        with self.assertRaises(codec.ProtocolError):codec.encode(lambda:None)
        with self.assertRaises(codec.ProtocolError):codec.decode({'t':'object','v':[]})
        with self.assertRaises(codec.ProtocolError):codec.decode({'t':'dict','v':[['key',1],['key',2]]})
    def test_wrong_candidate_value_cannot_forge_protected_assertion_success(self):
        package=types.ModuleType('validation');package.__path__=[]
        receipts=types.ModuleType('validation.python_unit_receipts');receipts.ReceiptError=SyntheticReceiptError
        tests=types.ModuleType('synthetic_rpc_tests')
        class Case(unittest.TestCase):
            def test_value(self):
                from validation import main_preflight_adoption as target
                self.assertEqual(target.input_digest('synthetic',{}),'required-fixture-value')
        tests.Case=Case
        argv=['python','-m','unittest','synthetic_rpc_tests.Case.test_value']
        self.module.input_digest=lambda *a:'wrong-fixture-value'
        with patch.dict(sys.modules,{'validation':package,'validation.python_unit_receipts':receipts,'synthetic_rpc_tests':tests}),patch.object(driver,'rpc',self.rpc):
            outcome=driver.run('acceptance',argv,{'protocol':2,'step':{'argv':argv}})
        self.assertEqual(outcome['outcome'],'assertion-failed');self.assertTrue(outcome['completed'])
        self.assertGreater(outcome['evidence']['completed_checks'],0);self.assertEqual(outcome['evidence']['failed_checks'],1)
    def test_empty_trusted_suite_is_unavailable_even_when_worker_reports_healthy(self):
        package=types.ModuleType('validation');package.__path__=[]
        receipts=types.ModuleType('validation.python_unit_receipts');receipts.ReceiptError=SyntheticReceiptError
        tests=types.ModuleType('synthetic_rpc_empty')
        argv=['python','-m','unittest','synthetic_rpc_empty']
        with patch.dict(sys.modules,{'validation':package,'validation.python_unit_receipts':receipts,'synthetic_rpc_empty':tests}),patch.object(driver,'rpc',self.rpc):
            outcome=driver.run('acceptance',argv,{'protocol':2,'step':{'argv':argv}})
        self.assertEqual(outcome['outcome'],'unavailable');self.assertFalse(outcome['completed'])
