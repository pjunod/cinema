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
