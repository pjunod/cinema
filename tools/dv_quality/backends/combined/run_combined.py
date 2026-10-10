"""Six-picture real synthetic P7/FEL-to-HDR10 experiment; never serves media."""
from fractions import Fraction
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import check_association

ROOT=Path('/work')
sha=lambda p:hashlib.sha256(Path(p).read_bytes()).hexdigest()
def write(path,value):
    path.write_text(json.dumps(value,indent=2,allow_nan=False)+'\n')
def run(stage,args,cwd=None,env=None):
    stdout=ROOT/f'{stage}.stdout';stderr=ROOT/f'{stage}.stderr'
    inputs={str(Path(a).relative_to(ROOT)):sha(a) for a in args[1:] if a.startswith('/work/') and Path(a).is_file()}
    before={'argv':args,'program_sha256':sha(args[0]),'input_sha256':inputs,'cwd':str(cwd if cwd else Path.cwd()),'environment':{k:(env if env is not None else os.environ).get(k) for k in ['LD_LIBRARY_PATH','VK_ICD_FILENAMES','XDG_RUNTIME_DIR','LC_ALL']}}
    with stdout.open('xb') as out,stderr.open('xb') as err:
        result=subprocess.run(args,cwd=cwd,env=env,stdin=subprocess.DEVNULL,stdout=out,stderr=err)
    outputs={'decode':list((ROOT/'b2-normal/frames').glob('*')),'mux-rgb':[ROOT/'reconstructed.nut'],'encode':[ROOT/'hdr10.mkv'],'probe':[],'decode-output':[ROOT/'decoded.yuv420p10le'],'decode-output-rgb':[ROOT/'decoded.rgb48le']}.get(stage,list((cwd/'outputs').glob('*')) if cwd else [])
    before.update(exit=result.returncode,stdout_sha256=sha(stdout),stderr_sha256=sha(stderr),output_sha256={str(p.relative_to(ROOT)):sha(p) for p in outputs if p.is_file()})
    write(ROOT/f'{stage}.execution.json',before)
    if any(sha(ROOT/p)!=v for p,v in inputs.items()):raise ValueError(stage+': consumed input changed')
    if result.returncode!=0:
        raise ValueError(f'{stage}: controlled stage exit {result.returncode}')
    if stdout.stat().st_size>65536 or stderr.stat().st_size>65536:
        raise ValueError(f'{stage}: diagnostic cap')
    return stdout

def main():
    control=sys.argv[1] if len(sys.argv)==2 else 'normal'
    if control=='lifecycle-hold':
        (ROOT/'held-helper.pid').write_text(str(os.getpid()))
        subprocess.run(['/bin/sh','-c','sleep 60 & echo $! > /work/held-descendant.pid; wait'],check=True)
        return
    if control not in ('normal','swapped-rpu'):raise ValueError('unknown bounded control')
    if (ROOT/'hdr10.mkv').exists():raise ValueError('output destination already exists')
    inventory=ROOT/'encoder-dependency-identity.json'
    if sha(inventory)!='8facd5a51e85dcc6a61a3e7419749dbeda9824eb0c80d658474a3830f9db8f78':raise ValueError('encoder inventory changed')
    for name,entry in json.loads(inventory.read_text())['files'].items():
        path=Path(name)
        if str(path.resolve())!=entry['resolved_path'] or path.stat().st_size!=entry['bytes'] or sha(path)!=entry['sha256']:
            raise ValueError('encoder dependency identity changed: '+name)
    before={n:sha(ROOT/n) for n in ['source.mkv','source-definition.json','bl-b2.mkv','el-b2.mkv']}
    if before!=json.loads((ROOT/'expected-inputs.json').read_text()):raise ValueError('source identity changed')
    out=run('decode',['/work/decode_layers','/work/source.mkv','/work/b2-normal/frames'])
    (ROOT/'b2-normal/decode.jsonl').write_bytes(out.read_bytes())
    (ROOT/'b2-normal/decode.stderr').write_bytes((ROOT/'decode.stderr').read_bytes())
    (ROOT/'b2-normal/decode-status.txt').write_text('0\n')
    if control=='swapped-rpu':
        a=ROOT/'b2-normal/frames/bl-frame-000.rpu.nal';b=ROOT/'b2-normal/frames/bl-frame-001.rpu.nal'
        aa=a.read_bytes();a.write_bytes(b.read_bytes());b.write_bytes(aa)
    receipt=check_association.inspect(ROOT,2,'normal')
    write(ROOT/'association.json',receipt)
    with (ROOT/'rgb48le.bin').open('xb') as rgb,(ROOT/'timing.tsv').open('x') as timing:
        for pair in receipt['pairs']:
            i=pair['source_frame'];folder=ROOT/f'render-{i}';folder.mkdir();(folder/'outputs').mkdir()
            env=os.environ.copy();env.update(LD_LIBRARY_PATH='/work/prefix/lib/aarch64-linux-gnu',XDG_RUNTIME_DIR='/work/runtime',VK_ICD_FILENAMES='/usr/share/vulkan/icd.d/lvp_icd.aarch64.json')
            (ROOT/'runtime').mkdir(exist_ok=True);(ROOT/'runtime').chmod(0o700)
            paths=[str(ROOT/pair[k]) for k in ['rpu_path','bl_path','el_path']]
            run(f'render-{i}',['/work/fel_export',*paths,pair['pts'],pair['duration']],folder,env)
            pixels=(folder/'outputs/nonzero-rendered.rgb48le').read_bytes()
            if len(pixels)!=24576:raise ValueError('rendered frame size')
            rgb.write(pixels)
            pts=Fraction(pair['pts'])*1000;duration=Fraction(pair['duration'])*1000
            if pts.denominator!=1 or duration.denominator!=1:raise ValueError('fixture timing cannot be exactly expressed in milliseconds')
            timing.write(f'{pts.numerator}\t{duration.numerator}\n')
            pair['rendered_sha256']=hashlib.sha256(pixels).hexdigest()
    write(ROOT/'render-association.json',receipt)
    run('mux-rgb',['/work/mux_rgb','/work/rgb48le.bin','/work/timing.tsv','/work/reconstructed.nut'])
    encode=['/usr/bin/ffmpeg','-nostdin','-hide_banner','-n','-copyts','-i','/work/reconstructed.nut','-an','-vf','zscale=matrixin=gbr:primariesin=2020:transferin=smpte2084:rangein=full:matrix=2020_ncl:primaries=2020:transfer=smpte2084:range=limited:chromal=center:filter=point,format=yuv420p10le','-fps_mode','passthrough','-enc_time_base','1/1000','-r','24','-c:v','libx265','-profile:v','main10','-preset','ultrafast','-x265-params','qp=0:bframes=0:keyint=24:scenecut=0:repeat-headers=1:chromaloc=1:colorprim=9:transfer=16:colormatrix=9:master-display=G(8500,39850)B(6550,2300)R(35400,14600)WP(15635,16450)L(100000000,50):pools=1:frame-threads=1','-color_primaries','bt2020','-color_trc','smpte2084','-colorspace','bt2020nc','-color_range','tv','/work/hdr10.mkv']
    run('encode',encode)
    probe=run('probe',['/usr/bin/ffprobe','-v','error','-show_streams','-show_packets','-show_frames','-of','json','/work/hdr10.mkv'])
    (ROOT/'hdr10.ffprobe.json').write_bytes(probe.read_bytes())
    run('decode-output',['/usr/bin/ffmpeg','-nostdin','-hide_banner','-n','-i','/work/hdr10.mkv','-map','0:v:0','-fps_mode','passthrough','-pix_fmt','yuv420p10le','-f','rawvideo','/work/decoded.yuv420p10le'])
    run('decode-output-rgb',['/usr/bin/ffmpeg','-nostdin','-hide_banner','-n','-i','/work/hdr10.mkv','-map','0:v:0','-fps_mode','passthrough','-vf','zscale=matrixin=2020_ncl:primariesin=2020:transferin=smpte2084:rangein=limited:chromalin=center:matrix=gbr:primaries=2020:transfer=smpte2084:range=full:filter=point:chromal=center:dither=none,format=gbrp16le','-pix_fmt','rgb48le','-f','rawvideo','/work/decoded.rgb48le'])
    if {n:sha(ROOT/n) for n in before}!=before:raise ValueError('source mutated during pipeline')
    stages=['decode','mux-rgb','encode','probe','decode-output','decode-output-rgb']+[f'render-{i}' for i in range(6)]
    write(ROOT/'execution-binding.json',{n:json.loads((ROOT/f'{n}.execution.json').read_text()) for n in stages})
    print(json.dumps({'result':'execution-complete-unqualified','pictures':6,'output_sha256':sha(ROOT/'hdr10.mkv'),'production_admission':False}))

try:main()
except (ValueError,OSError,KeyError,check_association.AssociationError) as error:
    print(json.dumps({'result':'refused','reason':str(error),'production_admission':False}));sys.exit(1)
