#!/usr/bin/env python3
"""Mac NumPy scoring of hash-bound, explicit-grid RGB48 collections.

Usage: python3 score_collections.py REQUEST.json OUTPUT.json
Request: schema_version1, reference/fallback:{receipt:absolute,sha256:hex},
arms:{label:{receipt,sha256}}, roi:[x0,y0,x1,y1].
Without alignment, all actual rational PTS/durations must match reference.
With alignment:{cadence:'1/24', origins:{label:fraction},
timestamp_tolerance:{label:fraction},duration_tolerance:{label:fraction},
provenance:{path:absolute,sha256:hex}}, each actual clock is checked against
its declared ordinal grid. Tolerances must be strictly below half a frame.
No lag search, implicit offset correction, bootstrap or independence claim.
"""
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import sys
import numpy as np
import vector_metrics as vm

PHASES=((0,0),(1,0),(0,1),(1,1))
BANDS=((0,.005),(.005,.1),(.1,1),(1,10),(10,100),(100,10000))
WEIGHTS=np.array((.2627,.678,.0593),dtype=np.float64)


def sha(path):
    with Path(path).open('rb') as f:
        return hashlib.file_digest(f,'sha256').hexdigest()


def bound(binding,path_key='path'):
    p=Path(binding[path_key])
    if not p.is_absolute() or sha(p)!=binding['sha256'].lower():
        raise ValueError('absolute binding/hash mismatch: '+str(p))
    return p


def artifact(root,record):
    rel=Path(record['file'])
    if rel.is_absolute() or '..' in rel.parts:
        raise ValueError('collection artifacts must use safe relative paths')
    p=root/rel
    if p.stat().st_size!=record['bytes'] or sha(p)!=record['sha256']:
        raise ValueError('collection artifact size/hash mismatch: '+str(p))
    return p


def load_collection(binding):
    p=bound(binding,'receipt')
    d=json.loads(p.read_text())
    if d.get('status')!='complete' or d.get('schema_version')!=1 or d.get('stride')!=16 or d.get('phase_offsets_xy')!=[list(x) for x in PHASES]:
        raise ValueError('incomplete or incompatible collection')
    n=d['frames_collected'];w,h=d['raster']
    if len(d['frames'])!=n or [x['frame'] for x in d['frames']]!=list(range(n)):
        raise ValueError('collection frame count/order differs')
    lattices={}
    for rec in d['lattices']:
        offset=tuple(rec['offset_xy'])
        expected=[n,3,len(range(offset[1],h,16)),len(range(offset[0],w,16))]
        if rec['shape']!=expected or rec['bytes']!=int(np.prod(expected))*2:
            raise ValueError('lattice shape differs')
        path=artifact(p.parent,rec)
        lattices[offset]=np.memmap(path,dtype='<u2',mode='r',shape=tuple(expected))
    if set(lattices)!=set(PHASES):
        raise ValueError('missing or duplicate phase')
    full={}
    for frame in d['frames']:
        if 'full_planar' in frame:
            rec=frame['full_planar'];path=artifact(p.parent,rec)
            if rec['bytes']!=w*h*6:
                raise ValueError('full frame size differs')
            full[frame['frame']]=np.memmap(path,dtype='<u2',mode='r',shape=(3,h,w))
    return {'receipt':d,'binding':binding,'root':p.parent,'lattices':lattices,'full':full}


def validate_grid(collections,alignment):
    ref=collections['reference']['receipt'];n=ref['frames_collected'];raster=ref['raster']
    timing={}
    if alignment is not None:
        bound(alignment['provenance'])
        cadence=Fraction(alignment['cadence'])
        if cadence<=0:
            raise ValueError('nonpositive cadence')
    for label,c in collections.items():
        d=c['receipt']
        if d['frames_collected']!=n or d['raster']!=raster:
            raise ValueError('collections have different window/raster')
        frame_errors=[]
        for i,frame in enumerate(d['frames']):
            pts=Fraction(frame['pts']);duration=Fraction(frame['duration'])
            if duration<=0 or i and pts<=Fraction(d['frames'][i-1]['pts']):
                raise ValueError('nonpositive/reordered/duplicate frame clock')
            if alignment is None:
                expected=ref['frames'][i]
                if pts!=Fraction(expected['pts']) or duration!=Fraction(expected['duration']):
                    raise ValueError('actual grids differ; explicit bounded alignment required')
            else:
                origin=Fraction(alignment['origins'][label])
                ptol=Fraction(alignment['timestamp_tolerance'][label])
                dtol=Fraction(alignment['duration_tolerance'][label])
                if ptol<0 or dtol<0 or 2*max(ptol,dtol)>=cadence:
                    raise ValueError('grid tolerance must be nonnegative and below half a frame')
                ep=pts-(origin+i*cadence);ed=duration-cadence
                if abs(ep)>ptol or abs(ed)>dtol:
                    raise ValueError(f'{label} frame{i} outside declared exact-rational clock tolerance')
                frame_errors.append({'frame':i,'actual_pts':str(pts),'actual_duration':str(duration),
                                     'pts_error':str(ep),'duration_error':str(ed)})
        timing[label]={'frame_count':n,'actual_first_pts':d['frames'][0]['pts'],
                       'actual_last_pts':d['frames'][-1]['pts'],'explicit_grid_errors':frame_errors}
    return timing


def linear_nits(rgb):
    x=np.asarray(rgb,dtype=np.float64)/65535
    p=x**(1/vm.M2)
    return 10000*(np.maximum(p-vm.C1,0)/(vm.C2-vm.C3*p))**(1/vm.M1)


def summaries(de,lum_ref,lum_test,active):
    low=(lum_ref<.005)&(lum_test<.005)
    sensitivity=np.where(low,0,de)
    return {'all_pixels':vm.stats(de),'active_roi':vm.stats(de[active]),
            'active_blackfloor_sensitivity':vm.stats(sensitivity[active]),
            'active_both_below_0_005_nit_count':int(np.count_nonzero(low&active)),
            'active_reference_luminance_bands':{
                f'{lo:g}-{hi:g}nit':vm.stats(de[active&(lum_ref>=lo)&((lum_ref<hi) if hi<10000 else (lum_ref<=hi))])
                for lo,hi in BANDS}}


def rgb_metrics(a,b,active):
    d=np.abs(a.astype(np.int32)-b.astype(np.int32))
    values=d[active]
    return {'active_RGB16_abs_mean':float(values.mean()),'active_RGB16_abs_max':int(values.max()),
            'active_signal_PSNR_dB':vm.psnr(a,b,active)}


def pooled_summary(bags):
    def aggregate(key):
        return vm.stats(np.concatenate(bags[key]))
    return {'all_pixels':aggregate('all'),'active_roi':aggregate('active'),
            'active_blackfloor_sensitivity':aggregate('floor'),
            'active_both_below_0_005_nit_count':bags['below'],
            'active_reference_luminance_bands':{key:vm.stats(np.concatenate(parts)) for key,parts in bags['bands'].items()}}


def new_bags():
    return {'all':[],'active':[],'floor':[],'below':0,
            'bands':{f'{lo:g}-{hi:g}nit':[] for lo,hi in BANDS}}


def add_bags(bags,de,lr,lt,mask):
    low=(lr<.005)&(lt<.005)
    bags['all'].append(de.ravel())
    bags['active'].append(de[mask])
    bags['floor'].append(np.where(low,0,de)[mask])
    bags['below']+=int(np.count_nonzero(low&mask))
    for lo,hi in BANDS:
        bags['bands'][f'{lo:g}-{hi:g}nit'].append(de[mask&(lr>=lo)&((lr<hi) if hi<10000 else (lr<=hi))])


def score_lattices(ref,test,roi):
    x0,y0,x1,y1=roi;w,h=ref['receipt']['raster'];n=ref['receipt']['frames_collected']
    phases=[];all_bags=new_bags()
    for offset in PHASES:
        dx,dy=offset;ys=np.arange(dy,h,16);xs=np.arange(dx,w,16)
        active=(ys[:,None]>=y0)&(ys[:,None]<y1)&(xs[None,:]>=x0)&(xs[None,:]<x1)
        if not np.any(active):
            raise ValueError('ROI contains no samples for phase '+str(offset))
        bags=new_bags();frames=[]
        for i in range(n):
            a=ref['lattices'][offset][i].transpose(1,2,0)
            b=test['lattices'][offset][i].transpose(1,2,0)
            de=vm.delta(a,b);lr=linear_nits(a)@WEIGHTS;lt=linear_nits(b)@WEIGHTS
            row={'frame':i,**summaries(de,lr,lt,active),**rgb_metrics(a,b,active)}
            frames.append(row);add_bags(bags,de,lr,lt,active)
        pooled=pooled_summary(bags)
        phases.append({'offset_xy':list(offset),'global_parity_yx':[dy%2,dx%2],
                       'sample_shape_yx':[len(ys),len(xs)],'per_frame':frames,'pooled':pooled})
        for key in ('all','active','floor'):
            all_bags[key].extend(bags[key])
        all_bags['below']+=bags['below']
        for key in all_bags['bands']:
            all_bags['bands'][key].extend(bags['bands'][key])
    return {'phases':phases,'pooled_four_deterministic_phases':pooled_summary(all_bags),
            'interpretation':'Descriptive deterministic sample pooling only; four phases are not independent observations and do not cover every raster pixel.'}


def score_full(ref,test,index,roi):
    a=ref['full'][index];b=test['full'][index];w,h=ref['receipt']['raster'];x0,y0,x1,y1=roi
    bags=new_bags();parity={f'{yp}{xp}':[] for yp in range(2) for xp in range(2)}
    rgb_sum=0;rgb_max=0;rgb_squared=0
    for y in range(0,h,64):
        end=min(y+64,h);aa=a[:,y:end,:].transpose(1,2,0);bb=b[:,y:end,:].transpose(1,2,0)
        ys=np.arange(y,end);xs=np.arange(w)
        active=(ys[:,None]>=y0)&(ys[:,None]<y1)&(xs[None,:]>=x0)&(xs[None,:]<x1)
        de=vm.delta(aa,bb);lr=linear_nits(aa)@WEIGHTS;lt=linear_nits(bb)@WEIGHTS
        add_bags(bags,de,lr,lt,active)
        signed=aa.astype(np.int32)-bb.astype(np.int32);d=np.abs(signed[active]);rgb_sum+=int(d.sum())
        if d.size:
            rgb_max=max(rgb_max,int(d.max()));rgb_squared+=float(np.sum((signed[active].astype(np.float64)/65535)**2))
        for yp in range(2):
            for xp in range(2):
                parity[f'{yp}{xp}'].append(de[active&((ys[:,None]%2)==yp)&((xs[None,:]%2)==xp)])
    count=(x1-x0)*(y1-y0)*3;mse=rgb_squared/count
    return {'frame':index,**pooled_summary(bags),'active_parities_yx':{k:vm.stats(np.concatenate(v)) for k,v in parity.items()},
            'active_RGB16_abs_mean':rgb_sum/count,'active_RGB16_abs_max':rgb_max,
            'active_signal_PSNR_dB':None if mse==0 else float(-10*np.log10(mse))}


def main():
    if len(sys.argv)!=3:
        raise SystemExit('usage: score_collections.py REQUEST.json NEW_OUTPUT.json')
    req_path=Path(sys.argv[1]);output=Path(sys.argv[2])
    if not req_path.is_absolute() or not output.is_absolute() or output.exists():
        raise ValueError('absolute request and new absolute output paths required')
    req=json.loads(req_path.read_text())
    if req.get('schema_version')!=1:
        raise ValueError('schema_version must be1')
    bindings={'reference':req['reference'],'fallback':req['fallback'],**req.get('arms',{})}
    if len(bindings)!=2+len(req.get('arms',{})):
        raise ValueError('arm names cannot replace reference/fallback')
    collections={label:load_collection(binding) for label,binding in bindings.items()}
    timing=validate_grid(collections,req.get('alignment'))
    w,h=collections['reference']['receipt']['raster'];roi=req['roi']
    if len(roi)!=4 or not all(isinstance(x,int) for x in roi) or not (0<=roi[0]<roi[2]<=w and 0<=roi[1]<roi[3]<=h):
        raise ValueError('ROI must be a nonempty bounded integer rectangle')
    selected=req.get('full_frames',[i for i in (0,24) if i<collections['reference']['receipt']['frames_collected']])
    if any(i not in c['full'] for c in collections.values() for i in selected):
        raise ValueError('selected full frame missing from a collection')
    pairs=[('reference',label) for label in bindings if label!='reference']
    pairs.extend(tuple(x) for x in req.get('extra_pairs',[]))
    report={'schema_version':1,'status':'in_progress','request_sha256':sha(req_path),
            'scope':'BT.2124 signal-domain fidelity of collected RGB16 PQ samples; not native DV presentation or physical-display accuracy',
            'scorer_sha256':sha(Path(__file__).resolve()),'vector_metrics_sha256':sha(Path(vm.__file__)),
            'numpy_version':np.__version__,'python':sys.version,'collection_bindings':bindings,
            'roi':roi,'raster':[w,h],'timing_validation':timing,'alignment':req.get('alignment'),
            'percentile_rule':'Audited vector_metrics.stats: order statistic floor(q*(n-1)), no interpolation',
            'blackfloor_sensitivity_rule':'Keep denominator; set deltaITP to0 only when BOTH weighted linear BT2020 luminances are below0.005nit. Sensitivity assumption, not a measured display threshold.',
            'luminance_bands_rule':'Reference weighted linear BT2020 Y in nits; active ROI only; lower-inclusive, upper-exclusive except10000inclusive',
            'inference_limit':'No bootstrap, confidence interval or independence claim; deterministic lattice summaries and selected full rasters are descriptive.',
            'pairs':{}}
    def save():
        temp=output.with_suffix(output.suffix+'.tmp');temp.write_text(json.dumps(report,indent=2)+'\n');temp.replace(output)
    save()
    try:
        for first,second in pairs:
            if first not in collections or second not in collections or first==second:
                raise ValueError('invalid extra comparison pair')
            ref,test=collections[first],collections[second]
            report['pairs'][first+'__'+second]={'lattices':score_lattices(ref,test,roi),
                                               'full_frames':[score_full(ref,test,i,roi) for i in selected]}
            save()
            print(json.dumps({'pair':[first,second],'status':'scored'}),flush=True)
        report['status']='complete';save()
    except BaseException as error:
        report['status']='failed';report['reason']=str(error);save();raise


if __name__=='__main__':
    main()
