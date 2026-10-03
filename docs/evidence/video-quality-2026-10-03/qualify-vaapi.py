#!/usr/bin/env python3
"""Bounded, neutral synthetic graph qualification; called inside an idle-host wrapper."""
import array, hashlib, json, pathlib, subprocess, sys, time
root = pathlib.Path(sys.argv[1]).resolve()
container = sys.argv[2]
recipe = json.loads((root / 'vaapi-recipe.json').read_text())
report = {'scope': 'neutral_1080p_vaapi_hdr10_graph', 'recipe': recipe,
          'qualification_limits': ['neutral synthetic signal only', 'no physical display or client matrix',
                                   'no Dolby processing, HDR subtitle burn or 4K qualification'], 'commands': []}
started = time.monotonic()
def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()
def run(binary, args):
    command = ['docker', 'exec', container, '/usr/bin/timeout', '--kill-after=3', '50', binary] + args
    before = time.monotonic()
    result = subprocess.run(command, capture_output=True, timeout=55)
    report['commands'].append({'argv': command, 'seconds': time.monotonic()-before,
                               'exit': result.returncode, 'stderr': result.stderr.decode(errors='replace')[-4000:]})
    if result.returncode:
        raise RuntimeError('qualification command failed')
    if time.monotonic()-started > 180:
        raise RuntimeError('qualification total time budget exhausted')
    return result.stdout
ffmpeg = '/usr/lib/jellyfin-ffmpeg/ffmpeg'
ffprobe = '/usr/lib/jellyfin-ffmpeg/ffprobe'
def ff(args):
    return run(ffmpeg, ['-nostdin', '-hide_banner', '-loglevel', 'error', '-threads', '1', '-filter_threads', '1', '-y']+args)
try:
    width,height,count = 1920,1080,96
    def pq(nits):
        p=(nits/10000)**(2610/16384)
        return round(64+876*((3424/4096+2413/128*p)/(1+2392/128*p))**(2523/32))
    # Uniform PQ-code spacing avoids a discontinuous step at black or
    # between panels, which measures lossy ringing rather than HDR transfer.
    rows = [array.array('H', [round(pq(0)+(pq(1000)-pq(0))*x/(width-1)) for x in range(width)])]*3
    luma = array.array('H')
    for row in rows:
        luma.extend(row*360)
    frame = luma + array.array('H', [512])*(width*height//2)
    if sys.byteorder != 'little': frame.byteswap()
    source_raw=root/'source.yuv'; source_raw.write_bytes(frame.tobytes())
    color=['-color_primaries','bt2020','-color_trc','smpte2084','-colorspace','bt2020nc','-color_range','tv']
    source=root/'source.mkv'; output=root/'encoded.mp4'
    ff(['-stream_loop','-1','-f','rawvideo','-pixel_format','yuv420p10le','-video_size','1920x1080','-framerate','24','-i',str(source_raw),'-frames:v',str(count),'-vf','setparams=range=limited:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc','-c:v','ffv1','-threads','1']+color+[str(source)])
    source_probe=json.loads(run(ffprobe,['-v','error','-select_streams','v:0','-show_streams','-of','json',str(source)]))['streams'][0]
    report['source_probe']=source_probe
    assert source_probe.get('color_transfer')=='smpte2084' and source_probe.get('color_primaries')=='bt2020', 'authored HDR source must carry actual PQ/BT2020 tags'
    report['source']={'sha256':digest(source),'raw_sha256':digest(source_raw),'frames':count,'duration_seconds':4,
        'signal':'Neutral continuous PQ code ramp from analytical ST2084 black to 1000 nits; uniform code spacing'}
    graph=recipe['hdr10_1080_filter']+','+recipe['upload_filter']
    # Encoder/filter args are exported from the reviewed source. The explicit
    # no-reorder VOD timestamp/GOP/muxer contract is the current 24 fps grid.
    ff(recipe['input_args']+['-i',str(source),'-map','0:v:0','-an','-vf',graph]+recipe['modes']['vbr']['encoder_args']+
       ['-frames:v',str(count),'-force_key_frames','expr:eq(mod(n,48),0)','-bf','0','-flags','+cgop','-g','48','-keyint_min','48','-sc_threshold','0','-fps_mode:v','passthrough','-enc_time_base:v','1:24','-avoid_negative_ts','disabled','-use_editlist','0','-movflags','+empty_moov+delay_moov+default_base_moof+frag_keyframe','-video_track_timescale','24','-tag:v',recipe['vod_hevc_sample_entry'],'-f','mp4',str(output)])
    report['encode_seconds']=report['commands'][-1]['seconds']
    report['realtime_multiple']=4/report['encode_seconds']
    probe=json.loads(run(ffprobe,['-v','error','-select_streams','v:0','-show_streams','-show_frames','-show_data','-of','json',str(output)]))
    stream=probe['streams'][0]; frames=probe['frames']
    report['first_frame']=frames[0]
    report['stream']=stream
    hvcc=bytes.fromhex(''.join(line.split(':',1)[1].strip().split('  ',1)[0].replace(' ','')
        for line in stream['extradata'].splitlines() if ':' in line))
    assert len(hvcc)>=13 and hvcc[0]==1
    compatibility=int(f"{int.from_bytes(hvcc[2:6], 'big'):032b}"[::-1],2)
    constraints=list(hvcc[6:12])
    while constraints and constraints[-1]==0: constraints.pop()
    profile_space=hvcc[1]>>6
    suffix=f"{'' if profile_space==0 else chr(64+profile_space)}{hvcc[1]&31}.{compatibility:X}.{'H' if hvcc[1]&32 else 'L'}{hvcc[12]}"
    if constraints: suffix+='.'+'.'.join(f'{value:02X}' for value in constraints)
    report['codec_declaration']={'actual_sample_entry':stream['codec_tag_string'],
        'actual_rfc6381':stream['codec_tag_string']+'.'+suffix,
        'expected_mpegts_hls_codec':recipe['expected_hls_codec'],
        'hvcc_hex':hvcc.hex()}
    assert stream['codec_tag_string']+'.'+suffix==recipe['expected_hls_codec'], 'HEVC declaration differs from actual hvcC'
    assert stream['codec_name']=='hevc' and stream['profile']=='Main 10'
    assert stream['width']==width and stream['height']==height and stream['level']==120
    assert len(frames)==count
    for n,frame_info in enumerate(frames):
        assert frame_info['pix_fmt']=='yuv420p10le'
        assert frame_info['color_transfer']=='smpte2084' and frame_info['color_primaries']=='bt2020'
        assert frame_info['color_space']=='bt2020nc' and frame_info['color_range']=='tv'
        assert abs(float(frame_info['pts_time'])-n/24)<0.000002
    ff(['-i',str(output),'-map','0:v:0','-an','-f','null','-'])
    decoded=root/'decoded.yuv'
    ff(['-i',str(output),'-frames:v','1','-pix_fmt','yuv420p10le','-f','rawvideo',str(decoded)])
    decoded_values=array.array('H');decoded_values.frombytes(decoded.read_bytes())
    if sys.byteorder!='little':decoded_values.byteswap()
    diffs=[abs(int(a)-int(b)) for a,b in zip(luma,decoded_values)]
    report['luma']={'mae_10bit_codes':sum(diffs)/len(diffs),'max_error_10bit_codes':max(diffs),'decoded_min':min(decoded_values[:width*height]),'decoded_max':max(decoded_values[:width*height]),'max_error_pixel':diffs.index(max(diffs)), 'p999_error_10bit_codes':sorted(diffs)[int(len(diffs)*0.999)]}
    assert report['luma']['mae_10bit_codes']<4 and report['luma']['max_error_10bit_codes']<32
    report['output']={'sha256':digest(output),'bytes':output.stat().st_size,'decoded_frames':len(frames),'first_pts':frames[0]['pts_time'],'last_pts':frames[-1]['pts_time']}
    report['outcome']='pass'
except Exception as error:
    report['outcome']='failed';report['reason']=str(error)
    raise
finally:
    report['total_seconds']=time.monotonic()-started
    (root/'qualification.json').write_text(json.dumps(report,indent=2)+'\n')
