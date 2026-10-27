#!/usr/bin/env python3
"""Stdlib-only, hash-bound RGB48 NUT collector. No decode/filter/swscale step.

Usage: python3 collect_rgb48.py REQUEST.json
Required request keys: schema_version=1,input_nut,helper_trace,output_dir,
width,height,frames,ffmpeg,ffprobe (all paths absolute).
Optional expected hashes: input_nut_sha256,helper_trace_sha256,
ffmpeg_sha256,ffprobe_sha256. full_frames defaults [0,24] within the window.
All four stride16 phases are fixed: (x,y)=(0,0),(1,0),(0,1),(1,1).
Run under the external resource/GPU/session guard. SIGINT/TERM clean up only
this collector's own FFmpeg child. An optional guard_command_file is a JSON
argv array invoked without shell before launch and each frame; stdout must
be an object with allowed=true. Bind it with guard_command_sha256.
"""
from contextlib import ExitStack
from fractions import Fraction
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys

PHASES=((0,0),(1,0),(0,1),(1,1))
STRIDE=16


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream,'sha256').hexdigest()


def absolute(value):
    p=Path(value)
    if not p.is_absolute():
        raise ValueError('absolute path required: '+str(p))
    return p


def bind(path,expected=None):
    path=absolute(path)
    actual=digest(path)
    if expected is not None and actual!=expected.lower():
        raise ValueError('SHA256 mismatch: '+str(path))
    return {'path':str(path),'bytes':path.stat().st_size,'sha256':actual,
            'expected_sha256':expected}


def events(path):
    text=Path(path).read_text()
    try:
        parsed=json.loads(text)
        if isinstance(parsed,list):
            return parsed
        if isinstance(parsed,dict) and isinstance(parsed.get('events'),list):
            return parsed['events']
        if isinstance(parsed,dict) and 'kind' in parsed:
            return [parsed]
    except json.JSONDecodeError:
        pass
    out=[]
    for line in text.splitlines():
        at=line.find('{')
        if at<0:
            continue
        try:
            item=json.loads(line[at:])
        except json.JSONDecodeError:
            continue
        if isinstance(item,dict):
            out.append(item)
    return out


def read_exact_into(stream,buffer):
    """Return False only for clean EOF before any byte; reject partial frames."""
    view=memoryview(buffer)
    position=0
    while position<len(view):
        count=stream.readinto(view[position:])
        if count is None:
            raise RuntimeError('unexpected nonblocking pipe read')
        if count==0:
            if position==0:
                return False
            raise RuntimeError(f'truncated RGB frame: {position}/{len(view)} bytes')
        position+=count
    return True


def write_full_planar(words,path):
    with path.open('xb') as output:
        for channel in range(3):
            output.write(words[channel::3].tobytes())
    return {'file':str(path),'bytes':path.stat().st_size,'sha256':digest(path),
            'layout':'channel,y,x; RGB; uint16 little-endian'}


def write_phase(words,width,height,offset,output):
    dx,dy=offset
    h=hashlib.sha256()
    count=0
    for channel in range(3):
        for y in range(dy,height,STRIDE):
            data=words[(y*width+dx)*3+channel:(y+1)*width*3:STRIDE*3].tobytes()
            output.write(data)
            h.update(data)
            count+=len(data)
    return count,h.hexdigest()


def stop_child(child):
    if child is None or child.poll() is not None:
        return
    child.terminate()
    try:
        child.wait(timeout=8)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait(timeout=8)


def interrupted(signum,_frame):
    raise InterruptedError('collector interrupted by signal '+str(signum))


def collect(request_path):
    if sys.byteorder!='little':
        raise RuntimeError('this collector requires a little-endian host')
    req_bytes=request_path.read_bytes()
    req=json.loads(req_bytes)
    if req.get('schema_version')!=1:
        raise ValueError('schema_version must be1')
    w,h,n=(req[k] for k in ('width','height','frames'))
    if not all(isinstance(x,int) for x in (w,h,n)) or not (1<=n<=64 and 16<=w<=4096 and 16<=h<=2160):
        raise ValueError('invalid bounded raster/window')
    frame_bytes=w*h*6
    root=absolute(req['output_dir'])
    root.mkdir(parents=True,exist_ok=False)
    report={'schema_version':1,'status':'in_progress','request_sha256':hashlib.sha256(req_bytes).hexdigest(),
            'scope':'RGB48 packet streamcopy only; no swscale, decoder, filter or pixel conversion',
            'collector':bind(Path(__file__).resolve()),'python':sys.version,
            'raster':[w,h],'frames_expected':n,'stride':STRIDE,
            'phase_offsets_xy':[list(p) for p in PHASES],
            'planar_layout':'frame,channel,y,x; RGB; uint16 little-endian',
            'frame_hash_layout':'interleavedRGB48LE, exactly helper raw RGB bytes',
            'resource_guard':'external_wrapper' if not req.get('guard_command_file') else 'bound_guard_command',
            'frames':[],'lattices':[]}
    result_path=root/'collection.json'
    def save():
        temp=root/'collection.json.tmp'
        temp.write_text(json.dumps(report,indent=2)+'\n')
        temp.replace(result_path)
    child=None
    try:
        report['input_nut']=bind(req['input_nut'],req.get('input_nut_sha256'))
        report['helper_trace']=bind(req['helper_trace'],req.get('helper_trace_sha256'))
        report['ffmpeg']=bind(req['ffmpeg'],req.get('ffmpeg_sha256'))
        report['ffprobe']=bind(req['ffprobe'],req.get('ffprobe_sha256'))
        source=absolute(req['input_nut'])
        before=source.stat()
        if before.st_size>frame_bytes*n+1024*1024:
            raise ValueError('raw NUT exceeds expected RGB payload plus1MiB container overhead')
        items=events(req['helper_trace'])
        rendered=[e for e in items if e.get('kind')=='rendered_frame']
        if len(rendered)!=n or [e.get('frame') for e in rendered]!=list(range(n)):
            raise ValueError('helper rendered-frame count/order differs from requested window')
        accepted={e['frame']:e for e in items if e.get('kind') in ('accepted_source_pair','accepted_source_base')}
        for i,e in enumerate(rendered):
            if [e.get('width'),e.get('height')]!=[w,h] or Fraction(e['duration'])<=0:
                raise ValueError('helper raster/duration differs')
            if i and Fraction(rendered[i-1]['pts'])>=Fraction(e['pts']):
                raise ValueError('duplicate or reordered helperPTS')
            if i in accepted and (Fraction(accepted[i]['pts'])!=Fraction(e['pts']) or Fraction(accepted[i]['duration'])!=Fraction(e['duration'])):
                raise ValueError('accepted-source timing differs from rendered frame')
        guard_argv=None
        if req.get('guard_command_file'):
            report['guard_command']=bind(req['guard_command_file'],req['guard_command_sha256'])
            guard_argv=json.loads(Path(req['guard_command_file']).read_text())
            if not isinstance(guard_argv,list) or not guard_argv or not all(isinstance(x,str) for x in guard_argv):
                raise ValueError('guard command must be a JSONargvarray')
        def guard():
            if guard_argv:
                checked=subprocess.run(guard_argv,capture_output=True,text=True,timeout=10)
                state=json.loads(checked.stdout)
                report['last_guard']=state
                if checked.returncode or state.get('allowed') is not True:
                    raise RuntimeError('resource guard refused collector')
        guard()
        probe_cmd=[req['ffprobe'],'-v','error','-show_streams','-show_format',
                   '-show_entries','stream=codec_name,codec_type,pix_fmt,width,height:format=format_name',
                   '-of','json',str(source)]
        probe=subprocess.run(probe_cmd,capture_output=True,text=True,check=True,timeout=30)
        info=json.loads(probe.stdout)
        streams=info.get('streams',[])
        if len(streams)!=1 or streams[0].get('codec_name')!='rawvideo' or streams[0].get('pix_fmt')!='rgb48le' or [streams[0].get('width'),streams[0].get('height')]!=[w,h] or info.get('format',{}).get('format_name')!='nut':
            raise ValueError('expected exactly one rawvideo RGB48LE NUT stream at requested raster')
        report['ffprobe_command']=probe_cmd
        report['ffprobe_result']=info
        report['ffmpeg_version']=subprocess.run([req['ffmpeg'],'-version'],capture_output=True,text=True,check=True,timeout=10).stdout.splitlines()[0]
        full_indices=req.get('full_frames',[i for i in (0,24) if i<n])
        if not isinstance(full_indices,list) or len(set(full_indices))!=len(full_indices) or any(not isinstance(i,int) or not 0<=i<n for i in full_indices):
            raise ValueError('invalid full-frame selection')
        (root/'full').mkdir()
        (root/'lattices').mkdir()
        # Request N+1, rather than truncating at N: an unexpected extra frame
        # must be detected by the final one-byte EOF check.
        command=[req['ffmpeg'],'-hide_banner','-loglevel','error','-nostdin','-threads','1',
                 '-i',str(source),'-map','0:v:0','-frames:v',str(n+1),'-c:v','copy','-f','rawvideo','pipe:1']
        report['ffmpeg_command']=command
        buffer=bytearray(frame_bytes)
        with ExitStack() as stack:
            outputs=[]
            for dx,dy in PHASES:
                path=root/'lattices'/f'phase-x{dx}-y{dy}.rgbp16le'
                outputs.append(stack.enter_context(path.open('xb')))
                report['lattices'].append({'offset_xy':[dx,dy],'shape':[n,3,len(range(dy,h,STRIDE)),len(range(dx,w,STRIDE))],
                                           'file':str(path.relative_to(root)),'frame_bytes':3*len(range(dy,h,STRIDE))*len(range(dx,w,STRIDE))*2})
            stderr=stack.enter_context((root/'ffmpeg.stderr').open('xb'))
            child=subprocess.Popen(command,stdout=subprocess.PIPE,stderr=stderr)
            report['ffmpeg_pid']=child.pid
            (root/'ffmpeg.pid').write_text(str(child.pid)+'\n')
            save()
            for i in range(n):
                guard()
                if not read_exact_into(child.stdout,buffer):
                    raise RuntimeError(f'clean EOF before frame{i}/{n}')
                raw_hash=hashlib.sha256(buffer).hexdigest()
                expected=rendered[i].get('rgb_sha256')
                if expected and expected!=raw_hash:
                    raise ValueError(f'frame{i} RGB hash differs from helper')
                if req.get('require_helper_rgb_sha256',False) and not expected:
                    raise ValueError('helper RGB hash required but missing')
                words=memoryview(buffer).cast('H')
                row={'frame':i,'pts':str(rendered[i]['pts']),'duration':str(rendered[i]['duration']),
                     'interleaved_rgb48le_sha256':raw_hash,'helper_rgb_sha256':expected,
                     'helper_hash_matches':None if not expected else True,
                     'accepted_source':accepted.get(i),'phase_chunks':[]}
                if i in full_indices:
                    row['full_planar']=write_full_planar(words,root/'full'/f'frame-{i:03}.rgbp16le')
                    row['full_planar']['file']=str(Path(row['full_planar']['file']).relative_to(root))
                for lattice,output in zip(report['lattices'],outputs):
                    size,chunk_sha=write_phase(words,w,h,tuple(lattice['offset_xy']),output)
                    if size!=lattice['frame_bytes']:
                        raise RuntimeError('phase byte-count mismatch')
                    row['phase_chunks'].append({'offset_xy':lattice['offset_xy'],'byte_offset':i*size,'bytes':size,'sha256':chunk_sha})
                words.release()
                report['frames'].append(row)
                save()
            if child.stdout.read(1):
                raise RuntimeError('unexpected extra RGB frame/output after requested window')
            if child.wait(timeout=30)!=0:
                raise RuntimeError('FFmpeg streamcopy failed; see ffmpeg.stderr')
        after=source.stat()
        if (before.st_dev,before.st_ino,before.st_size,before.st_mtime_ns)!=(after.st_dev,after.st_ino,after.st_size,after.st_mtime_ns):
            raise RuntimeError('input NUT changed during collection')
        for lattice in report['lattices']:
            path=root/lattice['file']
            if path.stat().st_size!=n*lattice['frame_bytes']:
                raise RuntimeError('lattice final size mismatch')
            lattice.update(bytes=path.stat().st_size,sha256=digest(path))
        report['input_stat_unchanged_during_collection']=True
        report['status']='complete'
        report['frames_collected']=n
        save()
        return report,result_path
    except BaseException as error:
        report['status']='failed'
        report['reason']=str(error)
        save()
        raise
    finally:
        stop_child(child)
        if child is not None and child.stdout is not None:
            child.stdout.close()


def main():
    signal.signal(signal.SIGINT,interrupted)
    signal.signal(signal.SIGTERM,interrupted)
    try:
        if len(sys.argv)!=2:
            raise ValueError('usage: collect_rgb48.py REQUEST.json')
        report,path=collect(absolute(sys.argv[1]))
        print(json.dumps({'status':report['status'],'receipt':str(path),'receipt_sha256':digest(path),
                          'frames':report['frames_collected'],'raster':report['raster']}))
    except BaseException as error:
        print(json.dumps({'status':'failed','reason':str(error)}))
        raise SystemExit(3)


if __name__=='__main__':
    main()
