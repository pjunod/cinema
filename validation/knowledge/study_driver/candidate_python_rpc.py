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

"""Untrusted candidate-role worker. Never receives graders or outcome keys."""
import importlib
_module=None
_git_stack=[]
_handles={}
_next_handle=0

def dispatch(method,args,kwargs,context):
    """Return one inert result branch; broker independently bounds its framing."""
    global _module
    if context.get('schema_sha256')!=SCHEMA_SHA256 or method not in OPERATIONS or type(args) is not list or type(kwargs) is not dict or kwargs:
        raise ProtocolError('closed contract mismatch')
    if _module is None:_module=importlib.import_module(MODULE)
    def reference(target):
        global _next_handle
        if type(target) is not dict:raise ProtocolError('cache node is not dictionary')
        for handle,value in _handles.items():
            if value is target:return {'t':'cache-ref','id':handle}
        if len(_handles)>=10000:raise ProtocolError('cache handles exceeded')
        _next_handle+=1;_handles[_next_handle]=target
        return {'t':'cache-ref','id':_next_handle}
    def cache(handle):
        if type(handle) is not int or handle not in _handles:raise ProtocolError('unknown cache handle')
        return _handles[handle]
    def unpack(value,depth=0):
        if depth>16:raise ProtocolError('depth exceeded')
        if type(value) is dict and value.get('t')=='cache-ref':
            if set(value)!={'t','id'}:raise ProtocolError('invalid cache reference')
            return cache(value['id'])
        if type(value) is dict and value.get('t') in ('dict','list','tuple'):
            if set(value)!={'t','v'} or type(value['v']) is not list or len(value['v'])>10000:raise ProtocolError('invalid tagged value')
            if value['t']=='dict':
                result={}
                for pair in value['v']:
                    if type(pair) is not list or len(pair)!=2:raise ProtocolError('invalid dictionary pair')
                    key=unpack(pair[0],depth+1)
                    if key in result:raise ProtocolError('duplicate dictionary key')
                    result[key]=unpack(pair[1],depth+1)
                return result
            result=[unpack(item,depth+1) for item in value['v']]
            return tuple(result) if value['t']=='tuple' else result
        return decode(value,depth)
    try:
        if method=='health':
            if args:raise ProtocolError('health takes no arguments')
            if not all(callable(getattr(_module,name,None)) for name in FUNCTIONS) or not all(type(getattr(_module,name,None)) is dict for name in CACHES):raise ProtocolError('public API unavailable')
            value={'ready':True}
        elif method=='call':
            if len(args)!=3 or args[0] not in FUNCTIONS:raise ProtocolError('unknown callable')
            positional=decode(args[1]);named=decode(args[2])
            if type(positional) not in (list,tuple) or type(named) is not dict:raise ProtocolError('invalid call shape')
            value=getattr(_module,args[0])(*positional,**named)
        elif method=='cache_root':
            if len(args)!=1 or args[0] not in CACHES:raise ProtocolError('unknown cache')
            return {'value':reference(getattr(_module,args[0]))}
        elif method=='constant':
            if len(args)!=1 or args[0] not in CONSTANTS:raise ProtocolError('unknown constant')
            value=getattr(_module,args[0])
        elif method=='git_stub':
            if len(args)!=2 or args[0]!='AssertionError' or type(args[1]) is not str:raise ProtocolError('unsupported stub')
            _git_stack.append(_module.git)
            def refuse(*a,**k):raise AssertionError(args[1])
            _module.git=refuse;value=None
        elif method=='git_restore':
            if args or not _git_stack:raise ProtocolError('unbalanced stub')
            _module.git=_git_stack.pop();value=None
        else:
            if len(args)<1:raise ProtocolError('cache arguments required')
            target=cache(args[0]);tail=args[1:]
            if method=='cache_get' and len(tail)==1:
                key=decode(tail[0]);value=target[key]
                if type(value) is dict:return {'value':reference(value)}
            elif method=='cache_set' and len(tail)==2:target[decode(tail[0])]=unpack(tail[1]);value=None
            elif method=='cache_clear' and not tail:target.clear();value=None
            elif method=='cache_update' and len(tail)==1:target.update(unpack(tail[0]));value=None
            elif method=='cache_keys' and not tail:value=list(target)
            elif method=='cache_snapshot' and not tail:value=target
            else:raise ProtocolError('unknown cache operation')
        return {'value':encode(value)}
    except Exception as error:
        name=type(error).__name__
        if name not in EXCEPTIONS:raise ProtocolError('undeclared candidate exception') from None
        return {'exception':{'type':name,'message':str(error)[:2048]}}
