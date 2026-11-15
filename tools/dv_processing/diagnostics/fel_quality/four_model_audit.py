#!/usr/bin/env python3
"""CPU-only fixture audit: four models applied consistently to all R/F/C arms.

This is not a normative Dolby renderer or a general fix. Never compares an
alternative reference with a GPU-rendered candidate: every model reconstructs
R (rawBL+EL), F (same rawBL, noEL), C (decoded top-left point candidate).
The nearest-positive grid is fixed for every model and arm.

Usage: python3 four_model_audit.py HASH_BOUND_REQUEST.json
Request fields: schema_version1,raw_binding_provenance:{path,sha256},
authorities:{composition_vectors:{path,sha256},colour_vectors:{path,sha256},...},
output_dir(newabsolute),cases:[{label,raster,el_raster,roi,frames:[0,24],
bl/el/candidate:{path,sha256,frame_sha256:[canonical0,canonical24]},
metadata:{'0':{path,sha256},'24':{path,sha256}},
source_rpus:{'0':{path,sha256},'24':{path,sha256}},
bl_siting:'topleft',el_siting:'topleft',el_layer_siting:'left',
candidate_siting:'topleft'}]. Optional retain_rgb:'auto'/true/false,
blackfloor_sensitivity:true/false. Raw files contain ONLY frames0/24 in order.
Do not run image reconstruction until the root qualifies every raw binding.
"""
import os
os.environ.setdefault('OPENBLAS_NUM_THREADS','1')
os.environ.setdefault('VECLIB_MAXIMUM_THREADS','1')
from contextlib import ExitStack
from pathlib import Path
import hashlib,json,re,shutil,sys
import numpy as np
import vector_metrics as vm

MODEL_IDS=('A','B','C','D')
ARMS=('R','F','C')
PAIRS=(('R','F'),('R','C'),('F','C'))
HPE_INVERSE=np.array(((3.06441879,-2.16597676,.10155818),
                      (-.65612108,1.78554118,-.12943749),
                      (.01736321,-.04725154,1.03004253)),dtype=np.float64)
Y_WEIGHTS=np.array((.2627,.678,.0593),dtype=np.float64)
VECTOR_MATRIX=np.array((9574,0,13802,9574,-1540,-5348,9574,17610,0),dtype=np.int64).reshape(3,3)
VECTOR_OFFSETS=np.array((16777216,134217728,134217728),dtype=np.int64)


def digest(path):
    with Path(path).open('rb') as f:
        return hashlib.file_digest(f,'sha256').hexdigest()


def bound(binding):
    path=Path(binding['path'])
    if not path.is_absolute() or digest(path)!=binding['sha256'].lower():
        raise ValueError('absolute binding/SHA mismatch: '+str(path))
    return path


def eotf(x):
    p=np.maximum(x,0)**(1/vm.M2)
    return (np.maximum(p-vm.C1,0)/(vm.C2-vm.C3*p))**(1/vm.M1)


def oetf(x):
    p=np.maximum(x,0)**vm.M1
    return ((vm.C1+vm.C2*p)/(1+vm.C3*p))**vm.M2


def fixture_residual(centered):
    if centered is None:
        return 0.0
    d=np.asarray(centered,dtype=np.float64)
    return np.sign(d)*(np.abs(d)-.5)/4096


def composition12(base,centered):
    """Exact integer equivalent of floor((B/1024 + fixture_r)*4096+.5)."""
    b=np.asarray(base,dtype=np.int64)
    if centered is None:
        correction=0
    else:
        d=np.asarray(centered,dtype=np.int64)
        correction=np.where(d>0,d,np.where(d<0,d+1,0))
    return np.clip(4*b+correction,0,4095).astype(np.int64)


def kernel_b_rgb(composed12,matrix,raw_offsets):
    """DoViBaker source-matched direct kernel, arithmetic Q13 shift/truncation."""
    signal=(composed12.astype(np.int64)<<4)-(raw_offsets>>12)
    raw=signal@matrix.T
    return np.clip(raw>>13,0,65535).astype('<u2')


def quantize_rgb(rgb):
    if not np.isfinite(rgb).all():
        raise ValueError('nonfinite helper/PQ model output; no silent repair')
    return np.floor(np.clip(rgb,0,1)*65535+.5).astype('<u2')


def model_rgb(model,base,centered,matrix,offsets,lms_transform):
    if model=='A':
        signal=base.astype(np.float64)/1023+fixture_residual(centered)
        affine=(signal-offsets.astype(np.float64)/2**28*(1024/1023))@(matrix.astype(np.float64)/8192).T
        return quantize_rgb(oetf(eotf(affine)@lms_transform.T))
    composed=composition12(base,centered)
    if model=='B':
        return kernel_b_rgb(composed,matrix,offsets)
    affine=(composed.astype(np.float64)/4095-offsets.astype(np.float64)/2**28*(4096/4095))@(matrix.astype(np.float64)/8192).T
    if model=='C':
        return quantize_rgb(affine)
    if model=='D':
        return quantize_rgb(oetf(eotf(affine)@lms_transform.T))
    raise ValueError('unknown model')


def fixture_metadata(path):
    d=json.loads(path.read_text());header=d['header'];mapping=d['rpu_data_mapping'];dm=d['vdr_dm_data']
    assert d['dovi_profile']==7 and d['el_type']=='FEL'
    assert header['bl_bit_depth_minus8']==header['el_bit_depth_minus8']==2
    assert header['vdr_bit_depth_minus8']==4 and header['coefficient_log2_denom']==23
    assert header['coefficient_data_type']==0 and header['disable_residual_flag'] is False
    assert dm['signal_bit_depth']==12 and dm['signal_color_space']==dm['signal_chroma_format']==0
    assert dm['signal_full_range_flag']==1
    assert mapping['mapping_color_space']==mapping['mapping_chroma_format_idc']==0
    assert mapping['num_x_partitions_minus1']==mapping['num_y_partitions_minus1']==0
    assert mapping['nlq_method_idc']=='LinearDeadzone' and mapping['nlq_num_pivots_minus2']==0
    assert len(mapping['curves'])==3
    for curve in mapping['curves']:
        assert curve['mapping_idc']=='Polynomial' and curve['pivots']==[0,1023]
        assert curve['poly_order_minus1']==[0] and curve['linear_interp_flag']==[False]
        assert curve['poly_coef_int']==[[0,1]] and curve['poly_coef']==[[0,0]]
    nlq=mapping['nlq'];assert nlq['nlq_offset']==[512]*3
    assert nlq['linear_deadzone_slope']==[2048]*3 and nlq['linear_deadzone_threshold']==[0]*3
    assert nlq['linear_deadzone_slope_int']==nlq['linear_deadzone_threshold_int']==nlq['vdr_in_max_int']==[0]*3
    assert nlq['vdr_in_max']==[1048576]*3 and 511.5/4096<1048576/2**23
    matrix=np.array([dm[f'ycc_to_rgb_coef{i}'] for i in range(9)],dtype=np.int64).reshape(3,3)
    offsets=np.array([dm[f'ycc_to_rgb_offset{i}'] for i in range(3)],dtype=np.int64)
    linear=np.array([dm[f'rgb_to_lms_coef{i}'] for i in range(9)],dtype=np.float64).reshape(3,3)/16384
    low=-(offsets>>12);high=65520-(offsets>>12)
    maximum=np.sum(np.maximum(matrix*low,matrix*high),axis=1)
    minimum=np.sum(np.minimum(matrix*low,matrix*high),axis=1)
    assert np.max(maximum)<2**31 and np.min(minimum)>=-2**31
    return matrix,offsets,HPE_INVERSE@linear,{'header':header,'DM_signal_bit_depth':dm['signal_bit_depth'],
        'ycc_to_rgb_matrix_rawQ13':matrix.tolist(),'offsets_rawQ28':offsets.tolist(),'rgb_to_lms_rawQ14':[dm[f'rgb_to_lms_coef{i}'] for i in range(9)]}


def validate_vectors(authorities):
    paths={k:bound(v) for k,v in authorities.items()}
    composition=np.loadtxt(paths['composition_vectors'],delimiter=',',dtype=np.int64)
    colour=np.loadtxt(paths['colour_vectors'],delimiter=',',dtype=np.int64)
    assert composition.shape==(81,5) and colour.shape==(225,12)
    expected=composition[:,4]
    got=composition12(composition[:,0],composition[:,1]-512)
    assert np.array_equal(got,expected)
    b,e=colour[:,:3],colour[:,3:6]
    c=composition12(b,e-512)
    patent=np.floor((b/1024+fixture_residual(e-512))*4096+.5).clip(0,4095).astype(np.int64)
    assert np.array_equal(c,colour[:,6:9]) and np.array_equal(c,patent)
    rgb=kernel_b_rgb(c,VECTOR_MATRIX,VECTOR_OFFSETS)
    assert np.array_equal(rgb,colour[:,9:12])
    return {'composition_vectors':81,'colour_vectors':225,'composition_exact':True,
            'integer_composition_equals_requested_float_patent_equation':True,
            'DoViBaker_Q13_RGB16_exact':True,'authorities':authorities,
            'interpretation':'Unchanged source-kernel fixture checks, not normative Dolby conformance.'}


def raw_input(binding,w,h,count):
    path=bound(binding);frame_bytes=w*h*3
    if path.stat().st_size!=count*frame_bytes or len(binding['frame_sha256'])!=count:
        raise ValueError('selected raw frame byte-count/hash-list mismatch')
    actual=[]
    with path.open('rb') as stream:
        for expected in binding['frame_sha256']:
            raw=stream.read(frame_bytes)
            if len(raw)!=frame_bytes or hashlib.sha256(raw).hexdigest()!=expected:
                raise ValueError('raw selected frame differs from bound canonical source: '+str(path))
            actual.append(expected)
    data=np.memmap(path,dtype='<u2',mode='r',shape=(count,w*h*3//2))
    if int(data.max())>1023:
        raise ValueError('raw input must be low10-bit planar YUV420, not padded P010')
    return data,{'path':str(path),'sha256':binding['sha256'],'frame_sha256':actual,'raster':[w,h]}


def planes(raw,w,h,n):
    a=raw[n]
    return (a[:w*h].reshape(h,w),a[w*h:w*h*5//4].reshape(h//2,w//2),a[w*h*5//4:].reshape(h//2,w//2))


def sample_codes(bl,el,candidate,w,h,lo,hi):
    xx=np.arange(w);yy=np.arange(lo,hi)
    cx=np.minimum((xx+1)//2,w//2-1);cy=np.minimum((yy+1)//2,h//2-1)
    def full(ps):
        return np.stack((ps[0][lo:hi],ps[1][cy[:,None],cx],ps[2][cy[:,None],cx]),-1).astype(np.int64)
    ew,eh=w//2,h//2
    ex=np.minimum((xx+1)//2,ew-1);ey=np.minimum(yy//2,eh-1)
    ecx=np.minimum((xx+2)//4,ew//2-1);ecy=np.minimum((yy+1)//4,eh//2-1)
    enhancement=np.stack((el[0][ey[:,None],ex],el[1][ecy[:,None],ecx],el[2][ecy[:,None],ecx]),-1).astype(np.int64)
    base=full(bl)
    return {'R':(base,enhancement-512),'F':(base,None),'C':(full(candidate),None)}


def bags():
    return {'delta':[],'parity':{f'{yp}{xp}':[] for yp in range(2) for xp in range(2)},
            'floor':[],'below':0,'RGB_abs_sum':0,'RGB_abs_max':0,'RGB_count':0}


def add_metric(bag,first,second,lo,hi,roi,blackfloor):
    x0,y0,x1,y1=roi;a=max(lo,y0);b=min(hi,y1)
    if a>=b:
        return None
    aa=first[a-lo:b-lo,x0:x1];bb=second[a-lo:b-lo,x0:x1]
    de=vm.delta(aa,bb);bag['delta'].append(de.ravel())
    ys=np.arange(a,b);xs=np.arange(x0,x1)
    for yp in range(2):
        for xp in range(2):
            bag['parity'][f'{yp}{xp}'].append(de[(ys%2)==yp][:,(xs%2)==xp].ravel())
    d=np.abs(aa.astype(np.int32)-bb.astype(np.int32));bag['RGB_abs_sum']+=int(d.sum())
    bag['RGB_abs_max']=max(bag['RGB_abs_max'],int(d.max()));bag['RGB_count']+=d.size
    if blackfloor:
        lum_a=(10000*eotf(aa.astype(np.float64)/65535))@Y_WEIGHTS
        lum_b=(10000*eotf(bb.astype(np.float64)/65535))@Y_WEIGHTS
        low=(lum_a<.005)&(lum_b<.005);bag['below']+=int(np.count_nonzero(low))
        bag['floor'].append(np.where(low,0,de).ravel())
    return de


def metric_result(bag,blackfloor):
    result={'full_resolution_active_ROI_deltaITP':vm.stats(np.concatenate(bag['delta'])),
            'active_parities_yx':{k:vm.stats(np.concatenate(v)) for k,v in bag['parity'].items()},
            'active_RGB16_abs_mean':bag['RGB_abs_sum']/bag['RGB_count'],'active_RGB16_abs_max':bag['RGB_abs_max']}
    if blackfloor:
        result.update(active_blackfloor_sensitivity=vm.stats(np.concatenate(bag['floor'])),
                      active_both_weighted_linear_Y_below_0_005nit_count=bag['below'])
    return result


def reconstruct_model(model,case,ordinal,frame,inputs,metadata,out,retain,blackfloor):
    w,h=case['raster'];ew,eh=case['el_raster'];roi=case['roi'];matrix,offsets,lms,metadata_receipt=metadata
    bl=planes(inputs['bl'],w,h,ordinal);el=planes(inputs['el'],ew,eh,ordinal);candidate=planes(inputs['candidate'],w,h,ordinal)
    phase_bags={a+'__'+b:bags() for a,b in PAIRS};paired=[]
    hashes={k:hashlib.sha256() for k in ARMS};paths={};writers={}
    with ExitStack() as stack:
        for arm in ARMS:
            p=out/f'{model}-frame-{frame:03}-{arm}.rgb48le'
            paths[arm]=str(p) if retain else None
            writers[arm]=stack.enter_context(p.open('xb')) if retain else None
        for lo in range(0,h,64):
            hi=min(lo+64,h);codes=sample_codes(bl,el,candidate,w,h,lo,hi)
            rgbs={arm:model_rgb(model,*codes[arm],matrix,offsets,lms) for arm in ARMS}
            for arm,rgb in rgbs.items():
                raw=rgb.tobytes();hashes[arm].update(raw)
                if writers[arm]:writers[arm].write(raw)
            errors={}
            for a,b in PAIRS:
                de=add_metric(phase_bags[a+'__'+b],rgbs[a],rgbs[b],lo,hi,roi,blackfloor)
                if de is not None:errors[a+'__'+b]=de
            if 'R__F' in errors:
                paired.append((errors['R__F']-errors['R__C']).ravel())
    advantage=np.concatenate(paired)
    return {'model':model,'frame':frame,'metadata':metadata_receipt,
            'RGB16_outputs':{arm:{'path':paths[arm],'sha256':hashes[arm].hexdigest(),'bytes':w*h*6,
                                  'format':'interleaved RGB16 little-endian'} for arm in ARMS},
            'within_model_comparisons':{k:metric_result(b,blackfloor) for k,b in phase_bags.items()},
            'paired_reference_error_advantage_F_minus_C':{
                **vm.stats(advantage),'min':float(advantage.min()),
                'candidate_closer_count':int(np.count_nonzero(advantage>0)),
                'candidate_worse_count':int(np.count_nonzero(advantage<0)),
                'tie_count':int(np.count_nonzero(advantage==0)),
                'meaning':'Positive means C is closer than F to this SAME model R; descriptive pixels, no independence claim.'}}


def main():
    if len(sys.argv)!=2:
        raise SystemExit('usage: four_model_audit.py HASH_BOUND_REQUEST.json')
    request_path=Path(sys.argv[1]);req=json.loads(request_path.read_text())
    if req.get('schema_version')!=1:
        raise ValueError('schema_version must be1')
    bound(req['raw_binding_provenance'])
    vector_check=validate_vectors(req['authorities'])
    prepared=[];required_bytes=0
    for case in req['cases']:
        if not re.fullmatch(r'[A-Za-z0-9_-]+',case['label']) or case['frames']!=[0,24]:
            raise ValueError('expected safe case label and explicitly storedframes0/24')
        w,h=case['raster'];ew,eh=case['el_raster'];x0,y0,x1,y1=case['roi']
        if not (64<=w<=4096 and 64<=h<=2160 and w%4==h%4==0 and (ew,eh)==(w//2,h//2) and 0<=x0<x1<=w and 0<=y0<y1<=h):
            raise ValueError('unsupported bounded raster/ROI/halfELfixture')
        assert case['bl_siting']==case['el_siting']==case['candidate_siting']=='topleft'
        assert case['el_layer_siting']=='left'
        inputs={};binding_rows={}
        for kind,dims in [('bl',(w,h)),('el',(ew,eh)),('candidate',(w,h))]:
            inputs[kind],binding_rows[kind]=raw_input(case[kind],*dims,2)
        for f in (0,24):
            bound(case['source_rpus'][str(f)])
        metadata={f:fixture_metadata(bound(case['metadata'][str(f)])) for f in (0,24)}
        prepared.append((case,inputs,binding_rows,metadata));required_bytes+=2*4*3*w*h*6
    root=Path(req['output_dir'])
    if not root.is_absolute():raise ValueError('absolute new output_dir required')
    available=shutil.disk_usage(root.parent).free
    selection=req.get('retain_rgb','auto')
    retain=available>=required_bytes+2*1024**3 if selection=='auto' else bool(selection)
    if retain and available<required_bytes+1024**3:
        raise ValueError('insufficient space for requested retained RGB outputs')
    root.mkdir(exist_ok=False)
    report={'schema_version':1,'status':'in_progress','scope':'Source/implementation and sensitivity audit only, NOT normative Dolby conformance or a general fix.',
            'request_sha256':digest(request_path),'raw_binding_provenance':req['raw_binding_provenance'],
            'script_sha256':digest(Path(__file__).resolve()),'audited_vector_metrics_sha256':digest(Path(vm.__file__)),
            'numpy_version':np.__version__,'python':sys.version,'vector_checks':vector_check,
            'thread_environment':{k:os.environ.get(k) for k in ('OPENBLAS_NUM_THREADS','VECLIB_MAXIMUM_THREADS')},
            'fixed_grid':'ALL models and arms: BL/C TLnearestpositive cx=(x+1)//2,cy=(y+1)//2; EL layerLeft ex=(x+1)//2,ey=y//2; ELTLchroma ecx=(x+2)//4,ecy=(y+1)//4; clampedge. No lagsearch or filter alternatives.',
            'models':{'A':'Consistent10: B/1023 + fixtureNLQ residual, rawQ13affine offsetraw/2^28*1024/1023, helperPQ/LMS sequence, nearestRGB16; unquantizedcomposedcomponents.',
                      'B':'Patent-shaped S/1024+fixtureNLQ, 12bitfloor(c*4096+.5)clip0..4095, then source-matchedDoViBaker direct((comp12<<4)-(rawOffset>>12))*rawQ13 >>13 arithmetic andclipRGB16.',
                      'C':'SAMEcomposed12asB; /4095, offsetraw/2^28*4096/4095, directrawQ13matrix; nearesthalfupRGB16.',
                      'D':'SAMECaffine then helperPQ EOTF→(HPEinverse@rawQ14linear)→PQ OETF, nearesthalfupRGB16.'},
            'rounding_boundary':'B uses source arithmeticshifttruncation. C/D use nearesthalfup; B-vsC differs in normalization AND explicitoutputrounding, not denominatoralone.',
            'reference_policy':'Only within-model R/F/C comparisons. Never alternative model R versus GPU-rendered C.',
            'retained_RGB':retain,'estimated_retained_RGB_bytes':required_bytes,'available_disk_bytes_before':available,
            'blackfloor_sensitivity':bool(req.get('blackfloor_sensitivity',True)),
            'blackfloor_rule':'Keep denominator, set error0 onlywhen BOTH weightedlinearBT2020Y<.005nit. Sensitivity only, not measured display threshold.',
            'percentile_rule':'Audited floor(q*(n-1)) order statistic, no interpolation or bootstrap.',
            'cases':{}}
    path=root/'results.json'
    def save():
        temp=root/'results.json.tmp';temp.write_text(json.dumps(report,indent=2)+'\n');temp.replace(path)
    save()
    try:
        for case,inputs,bindings,metadata in prepared:
            target=root/case['label'];target.mkdir()
            row={'raster':case['raster'],'EL_raster':case['el_raster'],'roi':case['roi'],'raw_bindings':bindings,'metadata_bindings':case['metadata'],'source_rpu_bindings':case['source_rpus'],'frames':{}}
            report['cases'][case['label']]=row
            for ordinal,frame in enumerate(case['frames']):
                row['frames'][str(frame)]={}
                for model in MODEL_IDS:
                    row['frames'][str(frame)][model]=reconstruct_model(model,case,ordinal,frame,inputs,metadata[frame],target,retain,report['blackfloor_sensitivity'])
                    save();print(json.dumps({'case':case['label'],'frame':frame,'model':model,'status':'scored_consistently_within_model'}),flush=True)
        report['status']='complete';save()
    except BaseException as error:
        report['status']='failed';report['reason']=str(error);save();raise


if __name__=='__main__':
    main()
