"""Actual pipe transport controls; small source correctness, not throughput."""
import argparse, hashlib, json, os, signal, subprocess
from pathlib import Path
def test_stream_controls():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--helper', type=Path, required=True)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--upper-source', type=Path)
    options=parser.parse_args()
    R=options.helper.resolve().parent; RUN=options.output.resolve(); RUN.mkdir()
    SOURCE=options.source.resolve()
    env=os.environ.copy()
    results=[]
    def stop(p):
        if p and p.poll() is None:
            os.killpg(p.pid,signal.SIGKILL); p.wait()
    def run(name,source,w,h,ew,eh,cap,encode=False,broken=False):
        out=RUN/name; out.mkdir()
        readfd,writefd=os.pipe(); consumer=None; renderer=None
        try:
            if broken:
                os.close(readfd); readfd=-1
            else:
                args=['ffmpeg','-nostdin','-v','error','-copyts','-threads','1','-f','nut','-i','pipe:0','-map','0:v:0','-an','-filter_threads','1','-vsync','0','-enc_time_base','-1']
                if encode:
                    args += ['-vf','format=gbrp16le,zscale=matrixin=gbr:matrix=2020_ncl:rangein=full:range=limited:primariesin=2020:primaries=2020:transferin=smpte2084:transfer=smpte2084:dither=none:filter=point:chromalin=center:chromal=center,format=yuv420p10le','-c:v','libx265','-threads','1','-preset','ultrafast','-x265-params','qp=0:pools=none:frame-threads=1:bframes=2:keyint=24:scenecut=0:repeat-headers=1:chromaloc=1:colorprim=9:transfer=16:colormatrix=9:range=limited','-color_primaries','bt2020','-color_trc','smpte2084','-colorspace','bt2020nc','-color_range','tv','-chroma_sample_location','center',str(out/'encoded.mp4')]
                else:
                    args += ['-c:v','rawvideo','-threads','1','-pix_fmt','rgb48le','-f','rawvideo',str(out/'decoded.rgb')]
                consumer=subprocess.Popen(args,stdin=readfd,stdout=subprocess.DEVNULL,stderr=(out/'encoder.stderr').open('wb'),start_new_session=True)
                os.close(readfd); readfd=-1
            with (out/'renderer.stdout').open('wb') as log,(out/'renderer.stderr').open('wb') as err:
                renderer=subprocess.Popen([str(options.helper.resolve()),str(source),str(out),'0',str(cap),str(w),str(h),str(ew),str(eh),'0','bt2020-pq-master-clip',f'/proc/self/fd/{writefd}'],env=env,pass_fds=(writefd,),stdout=log,stderr=err,start_new_session=True)
                os.close(writefd); writefd=-1
                code=renderer.wait(timeout=40)
            if broken:
                assert code == -signal.SIGPIPE, code
                assert 'segment_complete' not in (out/'renderer.stdout').read_text()
            else:
                assert code==0,(name,code,(out/'renderer.stderr').read_text())
                assert consumer.wait(timeout=40)==0,(name,(out/'encoder.stderr').read_text())
                events=[json.loads(line) for line in (out/'renderer.stdout').read_text().splitlines()]
                assert events[-1]['kind']=='segment_complete'
                assert not (out/'reconstructed.rgb48le').exists()
                rows=(out/'timing.tsv').read_text().splitlines()
                assert len(rows)==events[-1]['frames']==len(list((out/'rpus').glob('*.nal')))
                accepted=[e for e in events if e['kind']=='accepted_source_pair']
                for i,event in enumerate(accepted):
                    assert hashlib.sha256((out/'rpus'/f'frame-{i:03d}.nal').read_bytes()).hexdigest()==event['rpu_sha256']
                if not encode:
                    reference=RUN/'raw/reconstructed.rgb48le'
                    assert (out/'decoded.rgb').read_bytes()==reference.read_bytes()
                else:
                    probe=subprocess.check_output(['ffprobe','-v','error','-show_streams','-show_packets','-of','json',str(out/'encoded.mp4')])
                    (out/'probe.json').write_bytes(probe)
                    data=json.loads(probe); from fractions import Fraction
                    tb=Fraction(data['streams'][0]['time_base'])
                    actual=sorted(Fraction(p['pts'])*tb for p in data['packets'])
                    assert actual==[Fraction(row.split('\t')[0]) for row in rows],(actual,rows)
                    assert all('dts' in p for p in data['packets'])
                    assert data['streams'][0]['color_transfer']=='smpte2084'
            results.append({'case':name,'exit':code,'encoded':encode,'broken_peer':broken})
        finally:
            if readfd>=0: os.close(readfd)
            if writefd>=0: os.close(writefd)
            stop(renderer); stop(consumer)
    raw=RUN/'raw'; raw.mkdir()
    with (raw/'stdout').open('wb') as out, (raw/'stderr').open('wb') as err:
        subprocess.run([str(options.helper.resolve()),str(SOURCE),str(raw),'0','6','64','64','64','64','0','bt2020-pq-master-clip'],env=env,stdout=out,stderr=err,check=True,timeout=40)
    run('pipe-exact',SOURCE,64,64,64,64,6)
    run('pipe-encode',SOURCE,64,64,64,64,6,True)
    run('pipe-broken',SOURCE,64,64,64,64,6,broken=True)
    if options.upper_source:
        run('pipe-4k',options.upper_source.resolve(),3840,2160,1920,1080,64,True)
    (RUN/'stream-results.json').write_text(json.dumps(results,indent=2)+'\n')
    print(json.dumps(results))


if __name__ == '__main__':
    test_stream_controls()
