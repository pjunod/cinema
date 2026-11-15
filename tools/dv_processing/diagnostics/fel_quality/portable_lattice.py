"""Reduce a private lattice_eval.py report to the committed portable JSON.

Keeps scope, bindings, gates (equality checks summarised), per-frame win counts,
pooled statistics and per-frame all/dark statistics; neutralizes private path
prefixes. Usage: portable_lattice.py REPORT.json OUT.json PRIVATE_ROOT
"""
import json,hashlib,sys
from pathlib import Path
RR=sys.argv[3];HOME=str(Path.home());H=HOME+'/.local/state/plurx-fel-quality/'
P={RR:'$FEL_ROI_ROOT',H+'20261111-qg8-relocated':'$FEL_QG8_ROOT',H+'20261026-shadow-followup':'$FEL_SHADOW_ROOT',H+'20261026-measurement-calibration':'$FEL_CAL_ROOT',HOME:'$HOME'}
def neut(v):
  if isinstance(v,str):
    for k,r in P.items():v=v.replace(k,r)
    return v
  if isinstance(v,list):return [neut(x) for x in v]
  if isinstance(v,dict):return {k:neut(x) for k,x in v.items()}
  return v
p=Path(sys.argv[1]);d=json.loads(p.read_text())
frames=[{'frame':f['frame'],'models':{m:{arm:{'all':e['all'],'dark_below_0.1':e['bands']['[0,0.1)']} for arm,e in mm.items()} for m,mm in f['models'].items()}} for f in d['frames']]
out={k:d[k] for k in ['scope','protocol','request','script','lattice','inputs','candidates','per_frame_wins_vs_F_sampled','pooled_all48']}
out['gates']={k:(v if k!='lattice_equals_retained_full_RGB' else {'checks':len(v),'all_equal':all(x['equal'] for x in v)}) for k,v in d['gates'].items()}
out['frames_all_and_dark']=frames;out['private_full_report_sha256']=hashlib.sha256(p.read_bytes()).hexdigest()
Path(sys.argv[2]).write_text(json.dumps(neut(out),indent=1)+'\n')
