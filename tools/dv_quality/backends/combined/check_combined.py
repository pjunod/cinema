"""Focused finite synthetic chain check; no production admission or DV oracle."""
from pathlib import Path
from fractions import Fraction
import hashlib,json,math,struct
import check_association
import stage_contract

def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def read(p):
    def refuse(v):raise ValueError('nonfinite JSON')
    return json.loads(p.read_text(),parse_constant=refuse)
def strict(a,b):return json.dumps(a,sort_keys=True,allow_nan=False)==json.dumps(b,sort_keys=True,allow_nan=False)
def words(p,n):
    raw=p.read_bytes()
    if len(raw)!=2*n:raise ValueError('truncated words: '+p.name)
    return struct.unpack('<'+str(n)+'H',raw)
def eotf(x):
    p=max(x,0)**(32/2523)
    return (max(p-3424/4096,0)/(2413/128-2392/128*p))**(16384/2610)
def oetf(x):
    p=max(x,0)**(2610/16384)
    return ((3424/4096+2413/128*p)/(1+2392/128*p))**(2523/32)
def verify(root):
    root=Path(root)
    expected=read(root/'expected-inputs.json')
    if {n:sha(root/n) for n in expected}!=expected:raise ValueError('input source changed')
    pairs=check_association.inspect(root,2,'normal')['pairs']
    association=read(root/'render-association.json')
    if len(association['pairs'])!=6:raise ValueError('six rendered pairs required')
    limits=read(root/'limits.json')
    wanted={'max_gpu_pq_error':2e-5,'max_gpu_rgb_code_error':2,'max_rgb_to_yuv_model_code_error':1,'max_qp0_yuv_code_error':2,'max_inverse_ncl_rgb_code_error':1}
    for k,v in wanted.items():
        if type(limits[k]) is not type(v) or limits[k]!=v:raise ValueError('fixed arithmetic limits changed')
    if sha(root/'limits.json')!=read(root/'limits-selection.json')['limits_sha256']:raise ValueError('limits registration changed')
    # Verify bytes actually retained from each stage against its execution-time record.
    stages=['decode','mux-rgb','encode','probe','decode-output','decode-output-rgb']+[f'render-{i}' for i in range(6)]
    execution=read(root/'execution-binding.json')
    if set(execution)!=set(stages):raise ValueError('exact execution stage inventory required')
    tools=read(root/'build-identity.json')['tools']
    for stage in stages:
        wanted_stage=stage_contract.contract(stage,pairs)
        event=read(root/f'{stage}.execution.json')
        if not strict(event,execution[stage]):raise ValueError('execution stage changed')
        if type(event['exit']) is not int or event['exit']!=0:raise ValueError('stage did not exit cleanly')
        for stream in ['stdout','stderr']:
            if sha(root/f'{stage}.{stream}')!=event[stream+'_sha256']:raise ValueError('stale '+stage+' '+stream)
        if not strict(event['argv'],wanted_stage['argv']) or not strict(event['cwd'],wanted_stage['cwd']) or not strict(event['environment'],wanted_stage['environment']):raise ValueError('actual stage invocation differs from fixed contract')
        if event['program_sha256']!=tools[wanted_stage['argv'][0]]:raise ValueError('recorded tool identity differs from retained build identity')
        if set(event['input_sha256'])!=set(wanted_stage['inputs']) or set(event['output_sha256'])!=set(wanted_stage['outputs']):raise ValueError('exact consumed/emitted stage inventory required')
        if any(sha(root/p)!=v for p,v in event['input_sha256'].items()):raise ValueError('consumed stage inputs changed')
        if any(sha(root/p)!=v for p,v in event['output_sha256'].items()):raise ValueError('stage output substituted')
    dm=read(root/'rpu-tags/p7-frame0.json')['vdr_dm_data']
    M=[[dm[f'ycc_to_rgb_coef{3*i+j}']/8192 for j in range(3)] for i in range(3)]
    L=[[dm[f'rgb_to_lms_coef{3*i+j}']/16384 for j in range(3)] for i in range(3)]
    H=[[3.06441879,-2.16597676,.10155818],[-.65612108,1.78554118,-.12943749],[.01736321,-.04725154,1.03004253]]
    A=[[sum(H[i][k]*L[k][j] for k in range(3)) for j in range(3)] for i in range(3)]
    O=[dm[f'ycc_to_rgb_offset{i}']/2**28*1024/1023 for i in range(3)]
    max_pq=0.;max_gpu_code=0;ordered=[]
    for i,pair in enumerate(pairs):
        retained=association['pairs'][i]
        if not strict({k:retained[k] for k in pair},pair):raise ValueError('render input association changed')
        events=check_association.events(root/f'render-{i}.stdout')
        expected_events=[{'kind':'parsed_rpu','bytes':(root/pair['rpu_path']).stat().st_size,'profile':7,'el_type':'FEL','parser_error':False,'bl_depth':10,'el_depth':10,'vdr_depth':12,'mapping_segments':1,'nlq_method':'LINEAR_DZ','creative_l2_count':0,'creative_l8_count':0,'creative_trims_applied':False},{'kind':'environment','api':374,'output_format':'rgba32f','format_pixel_size':16,'input_domain':'actual paired decoded native Main10 components','rpu_input':'libdovi parsed UNSPEC62 NAL; guarded synthetic subset'},{'kind':'frame','case':'nonzero','pts':pair['pts'],'duration':pair['duration'],'el_bound':True,'nlq_active':True,'rpu_parsed':True,'direct_dispatch_ok':True,'render_ok':True,'render_errors':0,'qualified_fel':False}]
        if not strict(events,expected_events):raise ValueError('parsed renderer event differs')
        expected_rgb=[]
        for y in range(64):
            for x in range(64):
                base=(400+4*i+2*(x//4)+y//4)/1023
                residual=(8+i)*(1 if (x//4+y//4+i)%2 else -1)/1024
                v=[base+residual,512/1023,512/1023]
                rgb=[sum(M[c][j]*(v[j]-O[j]) for j in range(3)) for c in range(3)]
                lin=list(map(eotf,rgb));expected_rgb.extend(oetf(sum(A[c][j]*lin[j] for j in range(3))) for c in range(3))
        for stage in ['reconstruction','rendered']:
            folder=root/f'render-{i}/outputs';raw=(folder/f'nonzero-{stage}.rgba32f').read_bytes()
            if len(raw)!=65536:raise ValueError('float frame size')
            floats=struct.unpack('<16384f',raw)
            if not all(math.isfinite(v) for v in floats):raise ValueError('nonfinite RGBA')
            actual=[floats[4*p+c] for p in range(4096) for c in range(3)]
            codes=words(folder/f'nonzero-{stage}.rgb48le',12288)
            max_pq=max(max_pq,max(abs(a-b) for a,b in zip(actual,expected_rgb)))
            max_gpu_code=max(max_gpu_code,max(abs(a-round(max(0,min(1,b))*65535)) for a,b in zip(codes,expected_rgb)))
        pixels=(root/f'render-{i}/outputs/nonzero-rendered.rgb48le').read_bytes()
        if sha(root/f'render-{i}/outputs/nonzero-rendered.rgb48le')!=retained['rendered_sha256']:raise ValueError('rendered pair hash changed')
        ordered.append(pixels)
    if b''.join(ordered)!=(root/'rgb48le.bin').read_bytes():raise ValueError('encoder RGB frame order changed')
    if max_pq>2e-5 or max_gpu_code>2:raise ValueError('GPU scalar arithmetic limit')
    if (root/'hdr10.ffprobe.json').read_bytes()!=(root/'probe.stdout').read_bytes():raise ValueError('encoded probe is stale')
    probe=read(root/'hdr10.ffprobe.json');stream=probe['streams'][0]
    target={'codec_name':'hevc','profile':'Main 10','width':64,'height':64,'pix_fmt':'yuv420p10le','color_range':'tv','color_space':'bt2020nc','color_transfer':'smpte2084','color_primaries':'bt2020','chroma_location':'center','has_b_frames':0}
    if len(probe['streams'])!=1 or not strict({k:stream.get(k) for k in target},target):raise ValueError('actual HDR10 output signal differs')
    frames=[e for e in probe['packets_and_frames'] if e['type']=='frame'];packets=[e for e in probe['packets_and_frames'] if e['type']=='packet'];tb=Fraction(stream['time_base'])
    if len(frames)!=6 or len(packets)!=6:raise ValueError('encoded frame count')
    for table in [frames,packets]:
        for e,pair in zip(table,pairs):
            dur=e.get('pkt_duration') if e['type']=='frame' else e.get('duration')
            if Fraction(e['pts'])*tb!=Fraction(pair['pts']) or Fraction(dur)*tb!=Fraction(pair['duration']):raise ValueError('encoded PTS/duration not preserved')
    src=words(root/'rgb48le.bin',6*12288);decoded=words(root/'decoded.yuv420p10le',6*6144);out=words(root/'decoded.rgb48le',6*12288)
    expected_yuv=[];expected_inverse=[];kr,kg,kb=.2627,.6780,.0593
    for f in range(6):
        planes=[[0]*4096,[0]*1024,[0]*1024]
        for y in range(64):
            for x in range(64):
                i=f*4096+y*64+x;r,g,b=[v/65535 for v in src[i*3:i*3+3]];yp=kr*r+kg*g+kb*b
                ci=(y//2)*32+x//2;planes[0][y*64+x]=round(64+876*yp)
                planes[1][ci]=round(512+896*(b-yp)/(2*(1-kb)));planes[2][ci]=round(512+896*(r-yp)/(2*(1-kr)))
                base=f*6144;yf=(decoded[base+y*64+x]-64)/876;uf=(decoded[base+4096+ci]-512)/896;vf=(decoded[base+5120+ci]-512)/896
                q=[yf+2*(1-kr)*vf,yf-2*kb*(1-kb)/kg*uf-2*kr*(1-kr)/kg*vf,yf+2*(1-kb)*uf]
                expected_inverse.extend(round(max(0,min(1,v))*65535) for v in q)
        expected_yuv.extend(sum(planes,[]))
    yuv_error=max(abs(a-b) for a,b in zip(decoded,expected_yuv));inverse_error=max(abs(a-b) for a,b in zip(out,expected_inverse));rgb_error=max(abs(a-b) for a,b in zip(src,out))
    initial_bound=math.ceil(65535*3*(1/876+1.8814/896))+2
    prior=root/'prior-preregistered-controls.json'
    if sha(prior)!='d7d71c1aad9f1278e567a38589dc96bf1cc8b0f800b40c98a95d801c16821e9f':raise ValueError('prior registered limits changed')
    previous=read(prior);legacy_yuv_cap=1+previous['encoder_max_yuv_code_error']
    rgb_bound=math.ceil(65535*legacy_yuv_cap*(1/876+1.8814/896))+2
    if yuv_error>legacy_yuv_cap or inverse_error>1 or rgb_error>rgb_bound:raise ValueError(f'HDR10 arithmetic diagnostic failed: {yuv_error=} {inverse_error=} {rgb_error=} {rgb_bound=}')
    return {'result':'bounded-synthetic-combined-pass','frames':6,'pts_preserved':True,'stored_durations_preserved':True,'gpu_max_pq_error':max_pq,'gpu_max_rgb_code_error':max_gpu_code,'decoded_yuv_vs_independent_ncl_max_code_error':yuv_error,'decoded_rgb_vs_inverse_ncl_max_code_error':inverse_error,'decoded_rgb_vs_reconstructed_max_code_error':rgb_error,'inherited_arithmetic_rgb_bound':rgb_bound,'initial_stricter_yuv_envelope_met':yuv_error<=3,'initial_stricter_rgb_bound':initial_bound,'legacy_bound_application':'preexisting registered cap4 applied after observing current combined result; diagnostic only; no newly preregistered acceptance claim','source_sha256':sha(root/'source.mkv'),'output_sha256':sha(root/'hdr10.mkv'),'production_admission':False,'full_dv_conformance':None,'independent_dv_picture_reference':None}
if __name__=='__main__':
    import sys
    print(json.dumps(verify(sys.argv[1]),indent=2,allow_nan=False))
