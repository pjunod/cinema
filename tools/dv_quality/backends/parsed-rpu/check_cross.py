#!/usr/bin/env python3
"""Difference-only same-GPU P8.1/HDR10 comparison; no acceptance threshold."""
import hashlib,json,struct,sys
from pathlib import Path
root=Path(sys.argv[1]).resolve();rows=[]
with (root/'parsed-p81-four.rgb48le').open('wb') as stream:
    for frame in range(4):
        for stage in ('reconstruction','rendered'):
            decoded=(root/'decoded.yuv420p10le').read_bytes()[frame*12288:(frame+1)*12288]
            rpu=(root/f'adapted-rpu-frame{frame}.nal').read_bytes()
            events=[json.loads(line) for line in (root/f'p81_export_probe/frame-{frame}/probe.jsonl').read_text().splitlines()]
            l1=next(e for e in events if e['kind']=='parsed_l1')
            if l1['max_pq']!=4095-frame or l1['avg_pq']!=2048-frame:
                raise ValueError('parsed synthetic RPU L1 frame tag mismatch')
            candidate=root/f'p81_export_probe/frame-{frame}/outputs/zero-{stage}.rgb48le'
            baseline=root/f'hdr10_baseline_probe/frame-{frame}/outputs/zero-{stage}.rgb48le'
            a=candidate.read_bytes();b=baseline.read_bytes()
            if len(a)!=24576 or len(b)!=24576: raise ValueError('truncated 64x64 RGB48 frame')
            aa=struct.unpack('<12288H',a);bb=struct.unpack('<12288H',b)
            rows.append({'frame_index':frame,'pts':f'{frame}/24','stage':stage,
                'decoded_frame_sha256':hashlib.sha256(decoded).hexdigest(),
                'rpu_sha256':hashlib.sha256(rpu).hexdigest(),'parsed_l1':l1,
                'max_code_delta':max(abs(x-y) for x,y in zip(aa,bb)),
                'candidate_sha256':hashlib.sha256(a).hexdigest(),
                'baseline_sha256':hashlib.sha256(b).hexdigest()})
        stream.write((root/f'p81_export_probe/frame-{frame}/outputs/zero-rendered.rgb48le').read_bytes())
print(json.dumps({'scope':'difference-only same-GPU; not independent reference',
    'pairing':'explicit external frame index; no P7 reorder proof',
    'acceptance_bound':None,'qualified':False,'results':rows},indent=2,allow_nan=False))
