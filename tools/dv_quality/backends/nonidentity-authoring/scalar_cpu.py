"""Closed-form declared affine luma/FEL arithmetic; no backend calls."""
from pathlib import Path
import hashlib
import json
import struct
from registered_limits import read_limits
ROOT = Path(__file__).resolve().parent
read_limits(ROOT)
MATRIX = ((9574, 0, 13802), (9574, -1540, -5348), (9574, 17610, 0))

def expected(residual, affine):
    output = bytearray()
    for y in range(16):
        for x in range(16):
            bl = (400 + 2*x + y, 512, 512)
            vdr = []
            for c in range(3):
                rr = (16 if (x+y+c)%2 else -16) if residual else 0
                mapped = 256 + 3*bl[c] if affine and c == 0 else 4*bl[c]
                vdr.append(max(0, min(4095, mapped + 4*rr))*16)
            components = tuple(a-b for a,b in zip(vdr, (4096,32768,32768)))
            rgb = [max(0,min(65535,sum(k*v for k,v in zip(row,components))//8192)) for row in MATRIX]
            output.extend(struct.pack('<3H', *rgb))
    return bytes(output)

results = []
for name, residual, affine in [('reconstructed',True,True), ('zero',False,True), ('identity',True,False)]:
    data = expected(residual, affine)
    (ROOT/(name+'16-reference.rgb48le')).write_bytes(data)
    actual = (ROOT/(name+'16.rgb48le')).read_bytes()
    if len(actual) != 1536:
        raise ValueError('CPU artifact length changed')
    a,b = struct.unpack('<768H',actual),struct.unpack('<768H',data)
    error = max(abs(x-y) for x,y in zip(a,b))
    if error != 0:
        raise ValueError('CPU affine known-answer failed')
    results.append({'control':name,'max_code_error':error,'actual_sha256':hashlib.sha256(actual).hexdigest()})
receipt = {'scope':'Restricted supplied standard matrix; affine polynomial and declared linear residual; no approved Dolby oracle',
           'result':'analytic-control-pass','results':results,'p81_conformance':None}
(ROOT/'cpu-check-receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps(receipt,indent=2))
