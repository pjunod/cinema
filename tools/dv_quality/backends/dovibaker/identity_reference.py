"""Independent arithmetic reference for a declared identity shader control.
No Dolby bitstream/creative-mapping standard conformance is claimed.
"""
from decimal import Decimal, ROUND_HALF_EVEN
import hashlib,json,pathlib,struct
root=pathlib.Path(__file__).resolve().parent/'identity-control';root.mkdir(exist_ok=True)
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
D=Decimal
definition={'schema':'identity-composition-control-v1','width':16,'height':16,'frames':1,'domain':'full-range BT2020 PQ RGB','base_pq':'0.25 + 0.02*c + 0.005*x + 0.003*y','el_pq':'0.625 if (x+y+c)%2 else 0.375','offset':'0.5','slope':'0.2','threshold':'0','shape':'identity','nonlinear_matrix':'identity','linear_matrix':'inverse of declared HPE-BT2020 LMS-to-RGB matrix','target_mapping':'none','expected_operation':'base + (el-offset)*slope','scope':'analytic-control; declared identity arithmetic only; no proof of generic FEL or P8.1'}
(root/'source-definition.json').write_text(json.dumps(definition,indent=2)+'\n')
for name,residual in [('nonzero',True),('base',False)]:
 data=bytearray()
 for y in range(16):
  for x in range(16):
   for c in range(3):
    value=D('.25')+D('.02')*c+D('.005')*x+D('.003')*y
    if residual:value += D('.025') if (x+y+c)%2 else D('-.025')
    code=int((value*65535).quantize(D(1),rounding=ROUND_HALF_EVEN))
    data.extend(struct.pack('<H',code))
 (root/(name+'-reference.rgb48le')).write_bytes(data)
evidence={'kind':'independent','scope':'analytic-control','producer':'identity_reference.py using Python Decimal exact decimal arithmetic','method':'Closed-form declared identity BL+signed residual control, independent from executing renderer functions. Output quantization is nearest-even full-range PQ*65535. Does not validate original DV equation, metadata parsing, timing, decoding, chroma resampling, P8.1 compatibility, or creative trims.','input_definition_sha256':sha(root/'source-definition.json'),'script_sha256':sha(pathlib.Path(__file__)),'output_format':'rgb48le','artifacts':{n:sha(root/n) for n in ['nonzero-reference.rgb48le','base-reference.rgb48le']}}
(root/'reference-method.json').write_text(json.dumps(evidence,indent=2)+'\n')
print(json.dumps(evidence,indent=2))
