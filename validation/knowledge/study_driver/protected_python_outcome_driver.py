"""Closed Gold-free Python RPC schema; no candidate import or I/O."""
import hashlib,json
MODULE='validation.main_preflight_adoption'
FUNCTIONS=('input_digest','validate_journal')
CACHES=('_TREE_CACHE','_DIGEST_CACHE','_HISTORY_CACHE')
CONSTANTS=('JOB',)
EXCEPTIONS=('ReceiptError','AssertionError','TypeError','ValueError','KeyError')
OPERATIONS=('call','constant','cache_root','cache_get','cache_set','cache_clear','cache_update','cache_keys','cache_snapshot','health','git_stub','git_restore')
SCHEMA={'protocol':2,'module':MODULE,'functions':FUNCTIONS,'caches':CACHES,'constants':CONSTANTS,'exceptions':EXCEPTIONS,'operations':OPERATIONS,'max_frame_bytes':65536,'max_depth':16,'max_items':10000,'primitive_types':['null','bool','int','str'],'container_tags':['list','tuple','dict'],'cache_reference_tag':'cache-ref'}
SCHEMA_SHA256=hashlib.sha256(json.dumps(SCHEMA,sort_keys=True,separators=(',',':')).encode()).hexdigest()
class ProtocolError(ValueError):pass

def encode(value,depth=0):
    if depth>16:raise ProtocolError('depth exceeded')
    if value is None or type(value) in (bool,int,str):return value
    if type(value) in (list,tuple):
        if len(value)>10000:raise ProtocolError('items exceeded')
        return {'t':'tuple' if type(value) is tuple else 'list','v':[encode(v,depth+1) for v in value]}
    if type(value) is dict:
        if len(value)>10000:raise ProtocolError('items exceeded')
        return {'t':'dict','v':[[encode(k,depth+1),encode(v,depth+1)] for k,v in value.items()]}
    raise ProtocolError('unsupported value type')

def decode(value,depth=0):
    if depth>16:raise ProtocolError('depth exceeded')
    if value is None or type(value) in (bool,int,str):return value
    if type(value) is not dict or set(value)!={'t','v'} or type(value['v']) is not list or len(value['v'])>10000:raise ProtocolError('invalid tagged value')
    tag=value['t'];items=value['v']
    if tag in ('tuple','list'):
        result=[decode(v,depth+1) for v in items]
        return tuple(result) if tag=='tuple' else result
    if tag=='dict':
        result={}
        for pair in items:
            if type(pair) is not list or len(pair)!=2:raise ProtocolError('invalid dictionary pair')
            key=decode(pair[0],depth+1)
            if key in result:raise ProtocolError('duplicate dictionary key')
            result[key]=decode(pair[1],depth+1)
        return result
    raise ProtocolError('unknown value tag')

"""Protected module proxy: assertions see inert RPC values, never candidate code."""
import types,unittest.mock,copy

class RemoteDict:
    def __init__(self,client,handle):self.client=client;self.handle=handle
    def _call(self,op,*tail):return self.client('cache_'+op,[self.handle,*tail],{})
    def __getitem__(self,key):return self._call('get',encode(key))
    def __setitem__(self,key,value):self._call('set',encode(key),encode_reference(value))
    def __iter__(self):return iter(self._call('keys'))
    def __len__(self):return len(self._call('keys'))
    def keys(self):return self._call('keys')
    def items(self):return [(key,self[key]) for key in self]
    def values(self):return [self[key] for key in self]
    def clear(self):self._call('clear')
    def update(self,value):self._call('update',encode_reference(dict(value)))
    def __deepcopy__(self,memo):return copy.deepcopy(self._call('snapshot'),memo)

def encode_reference(value,depth=0):
    if depth>16:raise ProtocolError('depth exceeded')
    if isinstance(value,RemoteDict):return {'t':'cache-ref','id':value.handle}
    if type(value) in (list,tuple):
        if len(value)>10000:raise ProtocolError('items exceeded')
        return {'t':'list' if type(value) is list else 'tuple','v':[encode_reference(item,depth+1) for item in value]}
    if type(value) is dict:
        if len(value)>10000:raise ProtocolError('items exceeded')
        return {'t':'dict','v':[[encode_reference(k,depth+1),encode_reference(v,depth+1)] for k,v in value.items()]}
    return encode(value,depth)

class RemoteGit:
    def __call__(self,*args,**kwargs):raise ProtocolError('unmocked git is outside protected RPC contract')

class RemoteModule(types.ModuleType):
    def __init__(self,client):
        super().__init__(MODULE);object.__setattr__(self,'_client',client);object.__setattr__(self,'_remote_git',RemoteGit())
    def __getattr__(self,name):
        client=object.__getattribute__(self,'_client')
        if name in CACHES:return client('cache_root',[name],{})
        if name in CONSTANTS:return client('constant',[name],{})
        if name in FUNCTIONS:
            def call(*args,**kwargs):return client('call',[name,encode(args),encode(kwargs)],{})
            return call
        if name=='git':return object.__getattribute__(self,'_remote_git')
        raise AttributeError(name)
    def __setattr__(self,name,value):
        if name!='git':return super().__setattr__(name,value)
        if isinstance(value,RemoteGit):self._client('git_restore',[],{});return
        if not isinstance(value,unittest.mock.Mock) or not isinstance(value.side_effect,AssertionError):raise ProtocolError('unsupported test override')
        self._client('git_stub',['AssertionError',str(value.side_effect)],{})
    def __delattr__(self,name):
        if name=='git':self._client('git_restore',[],{});return
        super().__delattr__(name)

def make_module(rpc,exception_classes):
    if set(exception_classes)!=set(EXCEPTIONS):raise ProtocolError('exception map mismatch')
    def call(method,args,kwargs):
        result=rpc(method,args,kwargs)
        if type(result) is not dict or len(result)!=1:raise ProtocolError('invalid reply envelope')
        if 'exception' in result:
            error=result['exception']
            if type(error) is not dict or set(error)!={'type','message'} or error['type'] not in exception_classes or type(error['message']) is not str:raise ProtocolError('unknown exception')
            raise exception_classes[error['type']](error['message'])
        if 'value' not in result:raise ProtocolError('missing value')
        value=result['value']
        if type(value) is dict and value.get('t')=='cache-ref':
            if set(value)!={'t','id'} or type(value['id']) is not int or value['id']<1:raise ProtocolError('invalid cache reference')
            return RemoteDict(call,value['id'])
        return decode(value)
    return RemoteModule(call)


import sys,os,hashlib,json,unittest,types,pathlib,subprocess
# Public isolation controller injects this trusted broker capability after load.
rpc=globals().get("rpc")

def _unavailable():return {'protocol':2,'outcome':'unavailable','completed':False}

def run(mode,argv,context):
    """Run unchanged trusted unittest bytes against RPC-only module proxies.

    Protected /work is a separate immutable clean source view. Actual modified
    candidate code is present only in the other role and is never imported here.
    """
    if mode not in ('prerequisite','acceptance') or not callable(rpc) or type(argv) is not list or len(argv)<2:return _unavailable()
    step=context.get('step',{})
    if step.get('argv') not in (argv,tuple(argv)) or context.get('protocol')!=2:return _unavailable()
    # This dependency import resolves only in the clean protected source view.
    from validation.python_unit_receipts import ReceiptError
    errors={'ReceiptError':ReceiptError,'AssertionError':AssertionError,'TypeError':TypeError,'ValueError':ValueError,'KeyError':KeyError}
    proxy=make_module(rpc,errors)
    oldmodule=sys.modules.get(MODULE);parent=sys.modules.get('validation');oldparent=getattr(parent,'main_preflight_adoption',None)
    oldargv=sys.argv[:];oldmain=unittest.main;oldsysmain=sys.modules.get('__main__');oldpath=sys.path[:]
    records=[];discovered=[];protocol_errors=[]
    class Result(unittest.TextTestResult):
        def __init__(self,*a,**k):super().__init__(*a,**k);self.subchecks=0
        def addSubTest(self,test,subtest,error):
            self.subchecks+=1
            if error and isinstance(error[1],ProtocolError):protocol_errors.append(True)
            super().addSubTest(test,subtest,error)
        def addError(self,test,error):
            if isinstance(error[1],ProtocolError):protocol_errors.append(True)
            super().addError(test,error)
    class Runner(unittest.TextTestRunner):
        resultclass=Result
        def run(self,test):
            count=test.countTestCases();discovered.append(count)
            if mode=='prerequisite':return unittest.TestResult()
            result=super().run(test);records.append(result);return result
    def main(*args,**kwargs):
        kwargs['exit']=False;kwargs['testRunner']=Runner
        return oldmain(*args,**kwargs)
    try:
        sys.modules[MODULE]=proxy
        if parent is not None:setattr(parent,'main_preflight_adoption',proxy)
        unittest.main=main
        health=rpc('health',[],{})
        if health!={'value':encode({'ready':True})}:return _unavailable()
        if argv[1]=='-m':
            if len(argv)<4 or argv[2]!='unittest':return _unavailable()
            main(module=None,argv=['unittest',*argv[3:]])
        else:
            path=pathlib.Path(argv[1])
            if not path.is_absolute() or not str(path).startswith('/grader/') or '..' in path.parts or path.is_symlink():return _unavailable()
            descriptor=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
            with os.fdopen(descriptor,'rb') as stream:raw=stream.read(1024*1024+1)
            if len(raw)>1024*1024:return _unavailable()
            module=types.ModuleType('__main__');module.__file__=str(path);module.__package__=None
            sys.modules['__main__']=module;sys.argv=argv[1:]
            exec(compile(raw,str(path),'exec'),module.__dict__)
        if not discovered or any(n<=0 for n in discovered) or protocol_errors:return _unavailable()
        if mode=='prerequisite':
            # Existing focused fixtures use local Git. This fixed check is part
            # of the reviewed prerequisite recipe, not an assertion outcome.
            observed=subprocess.run(['/usr/bin/git','--version'],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=10,close_fds=True)
            if observed.returncode or not observed.stdout.startswith(b'git version '):return _unavailable()
            completed=len(discovered)+1;failed=skipped=0;kind='prerequisite';outcome='prerequisite-passed'
        else:
            if len(records)!=len(discovered) or any(result.errors for result in records):return _unavailable()
            completed=sum(result.testsRun+result.subchecks-len(result.skipped) for result in records)
            failed=sum(len(result.failures) for result in records);skipped=sum(len(result.skipped) for result in records)
            if completed<=0:return _unavailable()
            kind='assertion';outcome='assertion-failed' if failed else 'passed'
        evidence={'schema':SCHEMA_SHA256,'argv':argv,'completed_checks':completed,'failed_checks':failed,'skipped_checks':skipped}
        digest=hashlib.sha256(json.dumps(evidence,sort_keys=True,separators=(',',':')).encode()).hexdigest()
        return {'protocol':2,'outcome':outcome,'completed':True,'evidence':{'kind':kind,'identity':'protected-unittest-rpc-v2','sha256':digest,'completed_checks':completed,'failed_checks':failed,'skipped_checks':skipped}}
    except (Exception,SystemExit):return _unavailable()
    finally:
        unittest.main=oldmain;sys.argv=oldargv;sys.path[:]=oldpath
        if oldmodule is None:sys.modules.pop(MODULE,None)
        else:sys.modules[MODULE]=oldmodule
        if parent is not None:
            if oldparent is None:parent.__dict__.pop('main_preflight_adoption',None)
            else:setattr(parent,'main_preflight_adoption',oldparent)
        if oldsysmain is None:sys.modules.pop('__main__',None)
        else:sys.modules['__main__']=oldsysmain
