#!/usr/bin/env python3
"""Bounded scalar RPU arithmetic control; not a Dolby conformance oracle."""
import json, math, struct, sys
from pathlib import Path
root=Path(sys.argv[1]).resolve()
def reject_constant(value):
    raise ValueError(f'nonfinite JSON constant: {value}')
dm=json.loads((root/'fixtures/p7_fel_linear.json').read_text(), parse_constant=reject_constant)['vdr_dm_data']
M=[[dm[f'ycc_to_rgb_coef{3*i+j}']/8192 for j in range(3)] for i in range(3)]
L=[[dm[f'rgb_to_lms_coef{3*i+j}']/16384 for j in range(3)] for i in range(3)]
H=[[3.06441879,-2.16597676,.10155818],[-.65612108,1.78554118,-.12943749],[.01736321,-.04725154,1.03004253]]
A=[[sum(H[i][k]*L[k][j] for k in range(3)) for j in range(3)] for i in range(3)]
# Native 10-bit normalized samples: RPU offsets use 2^28 coding, followed
# by the public color representation's 1024/1023 normalization.
O=[dm[f'ycc_to_rgb_offset{i}']/2**28*1024/1023 for i in range(3)]
def eotf(x):
    p=max(x,0)**(32/2523)
    return (max(p-3424/4096,0)/(2413/128-2392/128*p))**(16384/2610)
def oetf(x):
    p=max(x,0)**(2610/16384)
    return ((3424/4096+2413/128*p)/(1+2392/128*p))**(2523/32)
results=[]
for case in ('zero','nonzero','omitted','shifted','disabled'):
    for component in ('bl','el'):
        raw_input=(root/f'outputs/{case}-{component}.rgba32f').read_bytes()
        if len(raw_input)!=4096: raise ValueError('truncated input')
        if not all(math.isfinite(v) for v in struct.unpack('<1024f',raw_input)):
            raise ValueError('nonfinite input')
    expected=[]
    for y in range(16):
        for x in range(16):
            v=[(400+2*x+y)/1023,512/1023,512/1023]
            if case in ('nonzero','shifted'):
                v[0]+=(16 if (x+y+(case=='shifted'))%2 else -16)/1024
            rgb=[sum(M[i][j]*(v[j]-O[j]) for j in range(3)) for i in range(3)]
            lin=list(map(eotf,rgb))
            expected.extend(oetf(sum(A[i][j]*lin[j] for j in range(3))) for i in range(3))
    if not all(math.isfinite(value) for value in expected):
        raise ValueError('nonfinite scalar expected output')
    for stage in ('reconstruction','rendered'):
        raw=(root/f'outputs/{case}-{stage}.rgba32f').read_bytes()
        if len(raw)!=16*16*4*4: raise ValueError('truncated float output')
        floats=struct.unpack('<1024f',raw)
        if not all(math.isfinite(v) for v in floats): raise ValueError('nonfinite float output')
        actual=[floats[4*i+c] for i in range(256) for c in range(3)]
        err=max(abs(a-b) for a,b in zip(actual,expected))
        rgb=(root/f'outputs/{case}-{stage}.rgb48le').read_bytes()
        if len(rgb)!=16*16*3*2: raise ValueError('truncated integer output')
        codes=struct.unpack('<768H',rgb)
        codeerr=max(abs(a-round(min(1,max(0,b))*65535)) for a,b in zip(codes,expected))
        results.append({'case':case,'stage':stage,'max_pq_error':err,'max_code_error':codeerr})
        if err>2e-5 or codeerr>2: raise ValueError(f'known answer failed {results[-1]}')
print(json.dumps({'scope':'parsed-synthetic-arithmetic-control','results':results,'qualified_fel':False},allow_nan=False,indent=2))
