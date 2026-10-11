"""Closed-form integer control for identity polynomial + declared linear residual.
Restricted to supplied synthetic RPU; not an independently approved Dolby decoder.
"""
import hashlib,json,pathlib,struct
root=pathlib.Path(__file__).resolve().parent
out=root/'cpu-reference';out.mkdir(exist_ok=True)
M=((9574,0,13802),(9574,-1540,-5348),(9574,17610,0))
def reference(name,contribution,bound=None):
 b=bytearray()
 for y in range(16):
  for x in range(16):
   base=(400+2*x+y,512,512)
   codes=[]
   for c in range(3):
    rr=(16 if (x+y+c)%2 else -16) if contribution else 0
    if bound is not None:rr=max(-bound,min(bound,rr))
    # Chosen S=1/1024,T=S/2 makes declared residual rr/1024;
    # these samples are exactly representable in the 12-bit intermediate.
    codes.append(max(0,min(4095,(base[c]+rr)*4))*16)
   components=tuple(a-b for a,b in zip(codes,(4096,32768,32768)))
   rgb=[max(0,min(65535,sum(k*v for k,v in zip(row,components))//8192)) for row in M]
   b.extend(struct.pack('<3H',*rgb))
 p=out/(name+'.rgb48le');p.write_bytes(b)
 return hashlib.sha256(b).hexdigest()
hashes={n:reference(n,active,bound) for n,active,bound in [('nonzero',True,None),('zero',False,None),('disabled',False,None),('bounded',True,2)]}
evidence={'scope':'analytic-control','kind':'independent-arithmetic','method':'Closed-form declared identity polynomial + S=1/1024,T=S/2 residual rr/1024, followed by supplied standard P81 fixed coefficients. Independent Python integer evaluation; no independent Dolby decoding/reference fidelity approval.','width':16,'height':16,'frames':1,'output_format':'rgb48le','color_domain':'BT2020 PQ full-range under standard-P7-matrix assumption','bl_y_code':'400+2*x+y','bl_uv_codes':[512,512],'el_code':'512 + (16 if (x+y+c)%2 else -16)','coefficient_log2_denom':23,'nlq_slope':8192,'nlq_threshold':4096,'unbounded_vdr_in_max':1,'bounded_vdr_in_max':'2/1024','expected_hashes':hashes,'independent_dv_reference':None}
(out/'reference-method.json').write_text(json.dumps(evidence,indent=2)+'\n')
print(json.dumps(evidence,indent=2))
