"""Portable measured RC comparison; admission and failure reasons are data-derived."""
from pathlib import Path
import argparse,hashlib,json
def sha(p):
 with Path(p).open('rb')as f:return hashlib.file_digest(f,'sha256').hexdigest()
p=argparse.ArgumentParser();p.add_argument('--calibration-root',type=Path,required=True);p.add_argument('--shadow-root',type=Path,required=True);p.add_argument('--rc-root',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
roots={'calibration':a.calibration_root.resolve(),'shadow':a.shadow_root.resolve(),'rc':a.rc_root.resolve()};rc=roots['rc'];assert a.output.resolve().is_relative_to(rc)
def ref(path):
 path=Path(path).resolve()
 for name,root in roots.items():
  if path.is_relative_to(root):return {'root':name,'path':path.relative_to(root).as_posix(),'sha256':sha(path)}
 raise ValueError('artifact outside declared evidence roots')
priorp=roots['shadow']/'shadow-stage-comparison-v4.json';prior=json.loads(priorp.read_text());data={};artifacts={};rates={}
arms={'RDOQ0_PSYRDOQ0_B115':'rdoq0-budget115','TWOPASS_B115':'twopass-budget115'}
for name in ['michael5400','michael1800']:
 for arm,suffix in arms.items():
  sp=rc/'quality'/f'{name}-{suffix}/summary.json';s=json.loads(sp.read_text());data[(name,arm)]=s;rates[(name,arm)]=s['coarse_rate_admitted']
  artifacts[name+'/'+arm]={'summary':ref(sp),'provenance':ref(sp.parent/'provenance.json'),'results':ref(sp.parent/'results.json'),'bands':ref(sp.parent/'reference-luminance-bands.json'),'actual_authored_video_packet_Mbps':s['actual_authored_packet_Mbps'],'coarse_130Mbps_window_average_guard':s['coarse_rate_admitted']}
  if arm=='TWOPASS_B115':artifacts[name+'/'+arm]['two_pass_mode_stats_CSV_proof']=ref(rc/'bindings'/f'{name}-{suffix}.two-pass-proof.json')
report={'schema':1,'scope':'Known calibration windows, selected source frames0/24, four correlated frozen models and fixed nearest-positive grid. No held-out, whole-window48 quality, native-device, optical-display, Dolby-conformance or production acceptance claim.',
 'root_aliases':{'calibration':'Previous sealed calibration evidence bundle','shadow':'Previous closed shadow follow-up evidence bundle','rc':'Current offline rate-control evidence bundle'},
 'reference_policy':'Every error is to the SAME model R, and every mask is from that model R. Current R/F images and reference-band counts must match the retained reference exactly.',
 'controls':{'F':'Converted original BL fallback, no recompression.', 'PREENCODE_POINT_SIMULATED_P81':'Uncompressed point-export raw data interpreted as P81; simulated, not authored.', 'SINGLEPASS_B115':'HDRopt0/CbCr-9/AQ2/psy-RD2/psy-RDOQ1/RDOQ2/SAO1, target115/max125/buf136.', 'RDOQ0_PSYRDOQ0_B115':'Same requested nominal budget, coupled RDOQ0 and psy-RDOQ0.', 'TWOPASS_B115':'Same coding policy and nominal budget as single-pass; fresh48-frame stats, slow-firstpass1, pass2 encoder report verified.'},
 'rate_limit':'Actual authored video-packet average over48frames <=130Mbps only. NOT full HRD/CPB/Dolby conformance; actual bits differ despite matched nominal budgets.',
 'strict_bar':'Full active ROI mean<F AND reference<.1nit mean<F AND p99<F, both frames/both windows/all4 models, plus coarse rate guard. No threshold fitting.',
 'sources':{'prior_shadow_comparison':ref(priorp),'quality_protocol':ref(rc/'QUALITY_PROTOCOL.v1.json'),'quantization_protocol':ref(rc/'PROTOCOL-QUANTIZATION-COMMITTED.json'),'two_pass_protocol':ref(rc/'PROTOCOL-TWO-PASS-COMMITTED.json'),'frozen_model':ref(rc/'quality-tools/four_model_audit.py'),'metric':ref(rc/'quality-tools/vector_metrics.py'),'band_scorer':ref(rc/'quality-tools/score_reference_bands.py')},
 'artifacts':artifacts,'rows':[]}
for old in prior['rows']:
 name=old['fixture'];frame=old['frame'];model=old['model'];full={k:old['full_active'][key]for k,key in [('F','F'),('PREENCODE_POINT_SIMULATED_P81','PREENCODE_POINT_SIMULATED_P81'),('SINGLEPASS_B115','BUDGET115')]}
 bands={b:{'count':v['count'],**{k:v[key]for k,key in [('F','F'),('PREENCODE_POINT_SIMULATED_P81','PREENCODE_POINT_SIMULATED_P81'),('SINGLEPASS_B115','BUDGET115')]}}for b,v in old['reference_bands'].items()}
 bars={}
 for arm in arms:
  r=next(x for x in data[(name,arm)]['rows']if x['frame']==frame and x['model']==model);assert full['F']==r['full_active']['R__F']['full_resolution_active_ROI_deltaITP'];full[arm]=r['full_active']['R__C']['full_resolution_active_ROI_deltaITP']
  for b in bands:assert bands[b]['count']==r['reference_bands'][b]['count']and bands[b]['F']==r['reference_bands'][b]['F'];bands[b][arm]=r['reference_bands'][b]['C']
  d=bands['dark_below_0.1'];bars[arm]={'full_mean_advantage_F':full[arm]['mean']<full['F']['mean'],'shadow_mean_advantage_F':d[arm]['mean']<d['F']['mean'],'shadow_p99_advantage_F':d[arm]['p99']<d['F']['p99'],'coarse_rate_pass':rates[(name,arm)]}
 report['rows'].append({'fixture':name,'frame':frame,'model':model,'full_active_deltaITP':full,'reference_luminance_bands':bands,'strict_bar_components':bars})
admission={}
for arm in arms:
 checks={}
 for name in ['michael5400','michael1800']:
  rows=[r for r in report['rows']if r['fixture']==name];failures=[{'frame':r['frame'],'model':r['model'],'failed_components':[k for k,v in r['strict_bar_components'][arm].items()if not v]}for r in rows if not all(r['strict_bar_components'][arm].values())]
  checks[name]={'rows':len(rows),'bar_pass_count':len(rows)-len(failures),'failures':failures}
 passed=all(not c['failures']for c in checks.values());admission[arm]={'all_windows_pass':passed,'checks':checks,'reason':'All measured strict-bar components pass.'if passed else'Measured strict-bar failures remain; see the per-window component lists.'}
report['measured_admission']=admission;report['producer']=ref(Path(__file__))
text=json.dumps(report,indent=2)+'\n';assert all(str(root)not in text for root in roots.values());assert '/Users/'not in text and '/private/tmp/'not in text
with a.output.open('x')as f:f.write(text)
print(json.dumps({'output_sha256':sha(a.output),'rows':len(report['rows']),'portable_paths':True,'measured_admission':admission}))
