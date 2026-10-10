import json,math,pathlib,metrics
root=pathlib.Path(__file__).resolve().parent
v=json.loads((root/'metric_vectors.json').read_text())
errors=[abs(metrics.delta_e_itp(x['reference'],x['candidate'])-x['expected']) for x in v['delta_e_itp']]
assert len(errors)==9 and max(errors)<1e-7
x=v['rgb_to_ictcp'];actual=metrics.rgb_nits_to_ictcp(x['rgb_nits'])
assert max(abs(a-b) for a,b in zip(actual,x['expected']))<1e-8
p=v['pu21_equation_numeric_checks']
assert max(abs(metrics.pu21_encode(a)-b) for a,b in zip(p['inputs_absolute_cd_m2'],p['outputs']))<1e-10
for value in [0,.0001,.005,.01,.1,1,10,100,1000,4000,10000]:
 assert abs(metrics.pq_eotf(metrics.pq_inverse_eotf(value))-value)<1e-7
assert metrics.bt2020_luminance((0,1000,0))==678
assert metrics.delta_e_itp((0,0,0),(0,1/360,0))==1
assert metrics.delta_e_itp((0,0,0),(1/720,0,0))==1
assert math.isinf(metrics.pu21_psnr_from_mse(0))
assert metrics.compare_rgb48_pixel((0,32768,65535),(0,32768,65535))['delta_e_itp']==0
assert metrics.compare_rgb48_pixel((32768,32768,32768),(32769,32769,32769))['delta_e_itp']>0
for f,arg in [(metrics.pq_eotf,float('nan')),(metrics.pq_eotf,1.1),(metrics.pq_inverse_eotf,-1),(metrics.pu21_encode,float('inf')),(metrics.rgb_nits_to_ictcp,[1,-1,1]),(metrics.pu21_psnr_from_mse,-1)]:
 try: f(arg)
 except ValueError: pass
 else: raise AssertionError('invalid input accepted')
print(json.dumps({'result':'pass','delta_e_vectors':len(errors),'delta_e_max_error':max(errors),'independent_dv_reference':None}))
