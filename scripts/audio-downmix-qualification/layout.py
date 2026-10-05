"""Measure the production fold to any target layout, not only stereo.

collector.py, aac.py, margin.py and astats.py are preserved byte-for-byte as
the executed stereo collectors (their hashes are pinned by the focused test),
so the target layout is a parameter of this collector instead. It reuses the
executed helpers (`run`, `stats`, the fixed shipped FFmpeg paths and the
optional `S09_CONTAINER` isolation) exactly as aac.py does, and renders every
output with the production encode argv for the target
(`push_audio_delivery_args`: `-c:a C -ac N [-channel_layout:a 5.1] -b:a Rk
-ar 48000`), so the fold measured is the one the engine performs.

    python3 ROOT/layout.py ROOT --source-layout 7.1 --target-layout 5.1 \
        [--real LABEL=PATH@START+SECONDS ...] [--candidate NAME=FILTER ...] \
        [--scan LABEL=PATH ...]

Synthetic inputs are the stereo receipt's two signals at the source layout:
FC-only pink noise calibrated to -20 dBFS RMS, and the same phase-coherent
-3 dBFS 1 kHz sine on every channel (the worst case: every fold coefficient
adds constructively). `--real` renders a window straight from a source
container, never through a WAV intermediate. `incumbent` is the production
fold; a `--candidate` is an explicit `-af` chain ahead of the same argv, and
is also compared sample by sample with the incumbent's PCM. `--scan` runs a
whole title through the incumbent fold and keeps per-channel peak facts only.
Library paths never enter the receipts: a label and the path's SHA-256 do.
"""
import argparse, hashlib, json, math, pathlib, sys
source = pathlib.Path(sys.argv[1]) / 'collector.py'
exec(source.read_text().split('root.mkdir(exist_ok=True)')[0])

# Channel order of FFmpeg's native layouts; the probe of every generated
# input is retained so the order is evidence, not an assumption.
LAYOUTS = {
    'stereo': ['FL', 'FR'],
    '5.1': ['FL', 'FR', 'FC', 'LFE', 'BL', 'BR'],
    '5.1(side)': ['FL', 'FR', 'FC', 'LFE', 'SL', 'SR'],
    '7.1': ['FL', 'FR', 'FC', 'LFE', 'BL', 'BR', 'SL', 'SR'],
}
# The production deliveries that perform a fold to each target
# (`resolve_audio`, crates/plurx-core/src/playback/audio.rs).
DELIVERIES = {
    'stereo': [('aac', 160)],
    '5.1': [('aac', 320), ('eac3', 640), ('ac3', 640)],
}


def file_key(text):
    return text.replace('(', '-').replace(')', '').replace('.', '_').replace('=', '-')


def encode_args(codec, target):
    """`push_audio_delivery_args` for a non-legacy encode."""
    channels = len(LAYOUTS[target])
    args = ['-c:a', codec, '-ac', str(channels)]
    if channels == 6:
        args += ['-channel_layout:a', '5.1']
    if codec != 'pcm_f32le':
        args += ['-b:a', str(dict(DELIVERIES[target])[codec]) + 'k']
    return args + ['-ar', '48000']


def render_outputs(input_args, label, target, candidates):
    """Pre-encode float PCM plus every production codec, per fold mode."""
    out = {}
    channels = len(LAYOUTS[target])
    for mode, chain in [('incumbent', None), *candidates.items()]:
        out[mode] = {}
        for codec in ['pcm_f32le', *[c for c, _ in DELIVERIES[target]]]:
            suffix = {'pcm_f32le': '.wav', 'aac': '.m4a', 'eac3': '.eac3', 'ac3': '.ac3'}[codec]
            target_path = root / (label + '-' + file_key(target) + '-' + file_key(mode) + '-' + codec + suffix)
            args = [ff, '-v', 'error', '-nostdin', '-threads', '1', '-filter_threads', '1', *input_args]
            if chain:
                args += ['-af', chain]
            args += encode_args(codec, target) + [str(target_path)]
            run(args, target_path.stem + '-render')
            facts = json.loads(run([probe, '-v', 'error', '-show_streams', '-of', 'json', str(target_path)],
                                   target_path.stem + '-probe'))
            layout = facts['streams'][0].get('channel_layout')
            measured = stats(target_path, target_path.stem, channels)
            for row, name in zip(measured, LAYOUTS.get(layout, LAYOUTS[target])):
                row['name'] = name
            out[mode][codec] = {'file': target_path.name, 'channel_layout': layout,
                                'channels': measured}
        if mode != 'incumbent':
            out[mode]['against_incumbent_pcm'] = compare_pcm(
                root / out['incumbent']['pcm_f32le']['file'], root / out[mode]['pcm_f32le']['file'],
                label + '-' + file_key(mode), channels)
    return out


def compare_pcm(incumbent, candidate, label, channels):
    """Per channel: the largest sample difference from the incumbent fold (an
    explicit matrix that reproduces it shows ~0), and the limiter's action —
    the share of samples it lowered by more than 0.1 dB and its largest cut."""
    a = array.array('f'); a.frombytes(run([ff, '-v', 'error', '-i', str(incumbent), '-f', 'f32le', '-'],
                                          label + '-cmp-incumbent'))
    b = array.array('f'); b.frombytes(run([ff, '-v', 'error', '-i', str(candidate), '-f', 'f32le', '-'],
                                          label + '-cmp-candidate'))
    if len(a) != len(b):
        return {'error': 'sample counts differ', 'incumbent': len(a), 'candidate': len(b)}
    rows = []
    for c in range(channels):
        xs, ys = a[c::channels], b[c::channels]
        diff, cut, worst = 0.0, 0, 0.0
        for x, y in zip(xs, ys):
            d = abs(x - y)
            if d > diff:
                diff = d
            if abs(x) > 1e-9 and abs(y) < abs(x) * 0.988553094656939:  # more than 0.1 dB lower
                cut += 1
                db = 20 * math.log10(abs(x) / max(abs(y), 1e-12))
                if db > worst:
                    worst = db
        rows.append({'channel': c, 'max_abs_difference': diff, 'samples_cut_over_0_1_db': cut,
                     'share_cut': cut / len(xs), 'max_cut_db': worst})
    return rows


def measure_source(path, label, layout):
    measured = stats(path, label + '-source', len(LAYOUTS[layout]))
    for row, name in zip(measured, LAYOUTS[layout]):
        row['name'] = name
    return measured


def scan(label, path, target):
    """Every audio frame of a whole title through the production fold: the
    per-frame sample peak of each output channel (`astats` with reset=1),
    reduced to each channel's maximum and the frames at or near full scale."""
    peaks = root / (label + '-scan-peaks.txt')
    pipeline = (' '.join([ff, '-nostdin', '-v', 'error', '-threads', '1', '-i', '"$1"', '-map', '0:a:0',
                          *encode_args('pcm_f32le', target), '-f', 'wav', '-']) + ' | ' +
                ' '.join([ff, '-nostdin', '-v', 'error', '-threads', '1', '-f', 'wav', '-i', '-', '-af',
                          'astats=metadata=1:reset=1:measure_overall=none:measure_perchannel=Peak_level,'
                          'ametadata=mode=print:file=' + str(peaks), '-f', 'null', '-']))
    argv = ['sh', '-c', pipeline, 'sh', path]
    commands.append(argv)
    prefix = ['docker', 'exec', os.environ['S09_CONTAINER']] if os.environ.get('S09_CONTAINER') else []
    p = subprocess.run(prefix + argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=3600)
    (root / (label + '-scan.stderr')).write_bytes(p.stderr)
    if p.returncode:
        raise RuntimeError(label + ' scan failed ' + str(p.returncode))
    names = LAYOUTS[target]
    channels = {name: {'max_dbfs': None, 'at_s': None, 'frames_at_or_above_0_dbfs': 0,
                       'frames_above_minus_1_dbfs': 0} for name in names}
    hottest, frames, t = [], 0, None
    for line in peaks.read_text().splitlines():
        if line.startswith('frame:'):
            t = float(re.search(r'pts_time:([-\d.]+)', line)[1])
            frames += 1
            continue
        match = re.match(r'lavfi\.astats\.(\d+)\.Peak_level=(.*)$', line)
        if not match:
            continue
        name = names[int(match[1]) - 1]
        value = float(match[2]) if match[2] not in ('-inf', 'inf', 'nan') else None
        row = channels[name]
        if value is None:
            continue
        if row['max_dbfs'] is None or value > row['max_dbfs']:
            row['max_dbfs'], row['at_s'] = value, t
        row['frames_at_or_above_0_dbfs'] += value >= 0
        row['frames_above_minus_1_dbfs'] += value > -1
        if value >= 0:
            hottest.append({'at_s': t, 'channel': name, 'peak_dbfs': value})
    peaks.unlink()
    hottest.sort(key=lambda row: -row['peak_dbfs'])
    return {'path_sha256': hashlib.sha256(path.encode()).hexdigest(), 'audio_frames': frames,
            'last_frame_s': t, 'channels': channels, 'hottest_frames_at_or_above_0_dbfs': hottest[:20]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('root')
    parser.add_argument('--source-layout', required=True, choices=sorted(LAYOUTS))
    parser.add_argument('--target-layout', required=True, choices=sorted(DELIVERIES))
    parser.add_argument('--real', action='append', default=[])
    parser.add_argument('--candidate', action='append', default=[])
    parser.add_argument('--scan', action='append', default=[])
    parser.add_argument('--skip-synthetic', action='store_true')
    args = parser.parse_args()
    source_channels = len(LAYOUTS[args.source_layout])
    if source_channels <= len(LAYOUTS[args.target_layout]):
        raise SystemExit('a fold needs more source than target channels')
    candidates = dict(item.split('=', 1) for item in args.candidate)
    root.mkdir(exist_ok=True)
    tag = file_key(args.source_layout) + '-to-' + file_key(args.target_layout)
    if (root / (tag + '-results.json')).exists():
        raise SystemExit('refusing to overwrite existing results')
    run([ff, '-version'], tag + '-ffmpeg-version')
    results = {'source_layout': args.source_layout, 'target_layout': args.target_layout,
               'source_channels': LAYOUTS[args.source_layout],
               'target_channels': LAYOUTS[args.target_layout],
               'deliveries': {codec: encode_args(codec, args.target_layout)
                              for codec in ['pcm_f32le', *dict(DELIVERIES[args.target_layout])]},
               'candidates': candidates, 'synthetic': {}, 'real': {}, 'scans': {}}
    if not args.skip_synthetic:
        noise = root / (tag + '-pink.wav')
        run([ff, '-v', 'error', '-threads', '1', '-filter_threads', '1', '-f', 'lavfi', '-i',
             'anoisesrc=color=pink:seed=20260930:sample_rate=48000:duration=12', '-c:a', 'pcm_f32le',
             str(noise)], tag + '-pink-generate')
        raw = run([ff, '-v', 'error', '-i', str(noise), '-f', 'f32le', '-'], tag + '-pink-decode')
        xs = array.array('f'); xs.frombytes(raw)
        gain = 0.1 / math.sqrt(sum(x * x for x in xs) / len(xs))
        results.update(seed=20260930, duration_s=12, sample_rate_hz=48000, fc_rms_target_dbfs=-20,
                       pink_gain=gain)
        for kind in ('fc', 'all'):
            src = root / (tag + '-' + kind + '.wav')
            if kind == 'fc':
                generate = [ff, '-v', 'error', '-threads', '1', '-filter_threads', '1', '-i', str(noise),
                            '-af', 'volume=' + repr(gain) + ',pan=' + args.source_layout + '|FC=c0',
                            '-c:a', 'pcm_f32le', str(src)]
            else:
                amp = 10 ** (-3 / 20)
                expr = '|'.join([repr(amp) + '*sin(2*PI*1000*t)'] * source_channels)
                generate = [ff, '-v', 'error', '-threads', '1', '-filter_threads', '1', '-f', 'lavfi', '-i',
                            'aevalsrc=' + expr + ':s=48000:d=12:c=' + args.source_layout,
                            '-c:a', 'pcm_f32le', str(src)]
            run(generate, src.stem + '-generate')
            facts = json.loads(run([probe, '-v', 'error', '-show_streams', '-of', 'json', str(src)],
                                   src.stem + '-probe'))
            results['synthetic'][kind] = {
                'probe': facts, 'source': measure_source(src, src.stem, args.source_layout),
                'outputs': render_outputs(['-i', str(src)], src.stem, args.target_layout, candidates)}
    for item in args.real:
        label, rest = item.split('=', 1)
        path, window = rest.rsplit('@', 1)
        start, seconds = window.split('+')
        # One read of the library file: the window's own bitstream, copied.
        # Every render then decodes that bitstream, as production would.
        window_copy = root / (label + '-window.mka')
        run([ff, '-v', 'error', '-nostdin', '-ss', start, '-t', seconds, '-i', path, '-map', '0:a:0',
             '-c:a', 'copy', str(window_copy)], label + '-window-copy')
        input_args = ['-i', str(window_copy)]
        facts = json.loads(run([probe, '-v', 'error', '-select_streams', 'a:0', '-show_entries',
                                'stream=codec_name,channels,channel_layout,sample_rate,sample_fmt,bits_per_raw_sample',
                                '-of', 'json', str(window_copy)], label + '-probe'))
        stream = facts['streams'][0]
        decoded = root / (label + '-source.wav')
        run([ff, '-v', 'error', '-nostdin', '-threads', '1', *input_args, '-c:a', 'pcm_f32le', str(decoded)],
            label + '-source-decode')
        # The decoder's output is the fact; a short copied window can probe as
        # its core substream (DTS-HD MA) before the extension is seen.
        stream['decoded'] = json.loads(run([probe, '-v', 'error', '-show_entries',
                                            'stream=channels,channel_layout', '-of', 'json', str(decoded)],
                                           label + '-decoded-probe'))['streams'][0]
        if stream['decoded'].get('channels') != source_channels:
            raise SystemExit(label + ': decoded channel count differs from --source-layout')
        results['real'][label] = {
            'start_s': float(start), 'duration_s': float(seconds),
            # The path stays in the private log; the receipt names a label.
            'path_sha256': hashlib.sha256(path.encode()).hexdigest(), 'stream': stream,
            'source': measure_source(decoded, label, args.source_layout),
            'outputs': render_outputs(input_args, label, args.target_layout, candidates)}
    for item in args.scan:
        label, path = item.split('=', 1)
        results['scans'][label] = scan(label, path, args.target_layout)
    results['hashes'] = {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                         for p in sorted(root.glob(tag + '-*')) if p.suffix in ('.wav', '.m4a', '.eac3', '.ac3')}
    for label in results['real']:
        for p in sorted(root.glob(label + '-*')):
            if p.suffix in ('.wav', '.m4a', '.eac3', '.ac3', '.mka'):
                results['hashes'][p.name] = hashlib.sha256(p.read_bytes()).hexdigest()
    private = {i.split('=', 1)[1].rsplit('@', 1)[0] for i in args.real} | {i.split('=', 1)[1] for i in args.scan}
    redacted = [[('<real-source>' if arg in private else arg) for arg in argv] for argv in commands]
    (root / (tag + '-commands.json')).write_text(json.dumps(redacted, indent=2))
    (root / (tag + '-results.json')).write_text(json.dumps(results, indent=2))


main()
