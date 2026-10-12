#!/usr/bin/env python3
"""Bounded, neutral synthetic graph qualification; called inside an idle-host wrapper."""
import array, hashlib, json, pathlib, subprocess, sys, time

DEFAULT_SIGNAL = 'continuous-pq-ramp'
SIGNALS = {
    DEFAULT_SIGNAL: ('Neutral continuous PQ code ramp from analytical ST2084 black to 1000 nits; uniform code spacing',
                     '3f78739c3a34985743d3070753a42c4327c54f16999b465ff9b9e83a587417a4'),
    'original-sharp-panels': ('Original discontinuous three-panel ST2084 signal: 0-1000, 0-10 and 20-21 nits; 360 rows each',
                              '777e283050d76a5d51c723ee0a23d04f2abee492aa02046803441715fb6b17f9'),
}

def validate_signal(signal):
    if signal not in SIGNALS:
        raise ValueError('unsupported diagnostic signal: select continuous-pq-ramp or original-sharp-panels')
    return signal

def reference_frame(signal=DEFAULT_SIGNAL):
    """One fixed-size raw frame; count is the later encoded repetition count."""
    validate_signal(signal)
    width,height,count = 1920,1080,96
    def pq(nits):
        p=(nits/10000)**(2610/16384)
        return round(64+876*((3424/4096+2413/128*p)/(1+2392/128*p))**(2523/32))
    if signal == 'original-sharp-panels':
        # Exact recovered historical generator math, including Python round.
        rows = [array.array('H', [pq((1000*t if panel==0 else 10*t if panel==1 else 20+t))
                for t in [x/(width-1) for x in range(width)]]) for panel in range(3)]
    else:
        # Preserve the default's original uniform PQ-code spacing byte for byte.
        rows = [array.array('H', [round(pq(0)+(pq(1000)-pq(0))*x/(width-1)) for x in range(width)])]*3
    luma = array.array('H')
    for row in rows:
        luma.extend(row*360)
    frame = luma + array.array('H', [512])*(width*height//2)
    if sys.byteorder != 'little': frame.byteswap()
    raw = frame.tobytes()
    if len(raw) != 6220800 or hashlib.sha256(raw).hexdigest() != SIGNALS[signal][1]:
        raise ValueError('diagnostic reference bytes differ from the selected fixed signal')
    return raw, width, height, count

def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            result.update(chunk)
    return result.hexdigest()

def artifact(path):
    return {'name': path.name, 'bytes': path.stat().st_size, 'sha256': digest(path)}

def measure_frame(reference, decoded, width, height):
    """Compare one tightly packed planar LE ten-bit frame, not a video stream."""
    if not (0 < width <= 1920 and 0 < height <= 1080 and width % 2 == height % 2 == 0):
        raise ValueError('unexpected or unbounded yuv420p10le frame geometry')
    pixels = width * height
    expected_bytes = pixels * 3  # Y: 2*w*h; U and V: 2*(w/2)*(h/2) each.
    if reference.stat().st_size != expected_bytes or decoded.stat().st_size != expected_bytes:
        raise ValueError('reference and decoded must each contain exactly one yuv420p10le frame')
    values = []
    for path in (reference, decoded):
        plane = array.array('H')
        plane.frombytes(path.read_bytes())
        if sys.byteorder != 'little':
            plane.byteswap()
        if len(plane) != pixels * 3 // 2 or max(plane) > 1023:
            raise ValueError('unexpected yuv420p10le sample layout or ten-bit range')
        values.append(plane)
    before, after = values
    diffs = [abs(int(before[n]) - int(after[n])) for n in range(pixels)]
    maximum = max(diffs)
    locations = [n for n, error in enumerate(diffs) if error == maximum]
    regions = {name: {'pixels': 0, 'error_sum': 0, 'max_error_10bit_codes': 0}
               for name in ('reference_constant_interior', 'reference_transition_or_border')}
    for n, error in enumerate(diffs):
        x, y = n % width, n // width
        interior = (0 < x < width-1 and 0 < y < height-1 and
                    all(before[n] == before[k] for k in (n-1, n+1, n-width, n+width)))
        region = regions['reference_constant_interior' if interior else 'reference_transition_or_border']
        region['pixels'] += 1
        region['error_sum'] += error
        region['max_error_10bit_codes'] = max(region['max_error_10bit_codes'], error)
    for region in regions.values():
        region['mae_10bit_codes'] = (region.pop('error_sum') / region['pixels']
                                    if region['pixels'] else None)
    return {
        'geometry': {'width': width, 'height': height, 'pixel_format': 'yuv420p10le',
                     'raw_frames_each': 1, 'bytes_each': expected_bytes,
                     'plane_samples': {'Y': pixels, 'U': pixels//4, 'V': pixels//4},
                     'alignment': 'source.yuv is one source frame; decoded frame 0'},
        'mae_10bit_codes': sum(diffs)/pixels, 'max_error_10bit_codes': maximum,
        'decoded_min': min(after[:pixels]), 'decoded_max': max(after[:pixels]),
        'max_error_pixel': locations[0], 'max_error_pixel_count': len(locations),
        'max_error_coordinates': [dict(x=n % width, y=n // width,
                                      reference=int(before[n]), decoded=int(after[n]))
                                  for n in locations[:32]],
        'max_error_coordinates_limit': 32,
        'p999_error_10bit_codes': sorted(diffs)[int(pixels*0.999)],
        'regions': regions,
        'region_definition': 'constant interior: not a frame border and all four immediate '
                             'reference luma neighbours equal; otherwise transition or border. '
                             'Diagnostic partition only, not a quality bar or causal verdict.',
    }

def main():
    if len(sys.argv) not in (3, 4):
        raise SystemExit('usage: qualify-vaapi.py ROOT CONTAINER [SIGNAL]')
    signal = validate_signal(sys.argv[3] if len(sys.argv) == 4 else DEFAULT_SIGNAL)
    root = pathlib.Path(sys.argv[1]).resolve()
    container = sys.argv[2]
    recipe = json.loads((root / 'vaapi-recipe.json').read_text())
    report = {'capture_version': 2, 'scope': 'neutral_1080p_vaapi_hdr10_graph', 'recipe': recipe,
              'diagnostic_signal': {'requested': signal, 'actual': None},
              'qualification_limits': ['neutral synthetic signal only', 'no physical display or client matrix',
                                       'no Dolby processing, HDR subtitle burn or 4K qualification'], 'commands': []}
    started = time.monotonic()
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
        raw,width,height,count = reference_frame(signal)
        source_raw=root/'source.yuv'; source_raw.write_bytes(raw)
        report['diagnostic_signal']['actual'] = signal
        color=['-color_primaries','bt2020','-color_trc','smpte2084','-colorspace','bt2020nc','-color_range','tv']
        source=root/'source.mkv'; output=root/'encoded.mp4'
        ff(['-stream_loop','-1','-f','rawvideo','-pixel_format','yuv420p10le','-video_size','1920x1080','-framerate','24','-i',str(source_raw),'-frames:v',str(count),'-vf','setparams=range=limited:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc','-c:v','ffv1','-threads','1']+color+[str(source)])
        source_probe=json.loads(run(ffprobe,['-v','error','-select_streams','v:0','-show_streams','-of','json',str(source)]))['streams'][0]
        report['source_probe']=source_probe
        assert source_probe.get('color_transfer')=='smpte2084' and source_probe.get('color_primaries')=='bt2020', 'authored HDR source must carry actual PQ/BT2020 tags'
        report['source']={'sha256':digest(source),'raw_sha256':digest(source_raw),'frames':count,'duration_seconds':4,
            'signal': SIGNALS[signal][0], 'signal_selector': signal}
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
        report['artifacts']={path.name: artifact(path) for path in (source_raw, source, output, decoded)}
        report['output']={'sha256':digest(output),'bytes':output.stat().st_size,'decoded_frames':len(frames),'first_pts':frames[0]['pts_time'],'last_pts':frames[-1]['pts_time']}
        assert report['source']['raw_sha256']==report['artifacts']['source.yuv']['sha256'], 'reference raw frame changed after source generation'
        report['luma']=measure_frame(source_raw, decoded, width, height)
        assert report['luma']['mae_10bit_codes']<4 and report['luma']['max_error_10bit_codes']<32
        report['outcome']='pass'
    except Exception as error:
        report['outcome']='failed';report['reason']=str(error)
        raise
    finally:
        report['artifacts']={name: artifact(root/name) for name in ('source.yuv', 'source.mkv', 'encoded.mp4', 'decoded.yuv') if (root/name).is_file()}
        report['total_seconds']=time.monotonic()-started
        (root/'qualification.json').write_text(json.dumps(report,indent=2)+'\n')

if __name__ == '__main__':
    main()
