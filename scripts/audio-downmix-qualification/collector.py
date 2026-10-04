import array, hashlib, json, math, os, pathlib, re, subprocess, sys

root = pathlib.Path(sys.argv[1])
ff = '/usr/lib/jellyfin-ffmpeg/ffmpeg'
probe = '/usr/lib/jellyfin-ffmpeg/ffprobe'
commands = []
def run(args, name):
    commands.append(args)
    prefix = ['docker', 'exec', os.environ['S09_CONTAINER']] if os.environ.get('S09_CONTAINER') else []
    p = subprocess.run(prefix + args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
    (root / (name + '.stderr')).write_bytes(p.stderr)
    if p.returncode:
        raise RuntimeError(name + ' failed ' + str(p.returncode))
    return p.stdout

def stats(path, label, channels):
    raw = run([ff, '-v', 'error', '-threads', '1', '-i', str(path), '-f', 'f32le', '-c:a', 'pcm_f32le', '-'], label+'-decode')
    values = array.array('f'); values.frombytes(raw)
    result = []
    for c in range(channels):
        xs = values[c::channels]
        peak = max(map(abs, xs))
        rms = math.sqrt(sum(x*x for x in xs)/len(xs))
        log = root/(label+'-lufs-'+str(c)+'.stderr')
        run([ff, '-hide_banner', '-threads', '1', '-i', str(path), '-af', 'pan=mono|c0=c'+str(c)+',ebur128=peak=sample', '-f', 'null', '-'], label+'-lufs-'+str(c))
        matches = re.findall(r'I:\s+(-?[\d.]+) LUFS', log.read_text())
        result.append({'channel': c, 'rms_dbfs':20*math.log10(rms) if rms else None, 'sample_peak_dbfs':20*math.log10(peak) if peak else None, 'samples_at_or_above_full_scale':sum(abs(x)>=1 for x in xs), 'integrated_lufs':float(matches[-1]) if matches else None, 'samples':len(xs)})
    return result

root.mkdir(exist_ok=True)
run([ff, '-version'], 'ffmpeg-version')
noise = root/'pink.wav'
run([ff, '-v', 'error', '-threads', '1', '-filter_threads', '1', '-f', 'lavfi', '-i', 'anoisesrc=color=pink:seed=20260930:sample_rate=48000:duration=12', '-c:a', 'pcm_f32le', str(noise)], 'pink-generate')
raw = run([ff, '-v', 'error', '-i', str(noise), '-f', 'f32le', '-'], 'pink-decode')
xs=array.array('f'); xs.frombytes(raw)
gain=0.1/math.sqrt(sum(x*x for x in xs)/len(xs))
matrices={'5.1':'FL=FL+0.707*FC+0.707*BL|FR=FR+0.707*FC+0.707*BR', '5.1(side)':'FL=FL+0.707*FC+0.707*SL|FR=FR+0.707*FC+0.707*SR', '7.1':'FL=FL+0.707*FC+0.5*SL+0.5*BL|FR=FR+0.707*FC+0.5*SR+0.5*BR'}
results={'seed':20260930,'duration_s':12,'sample_rate_hz':48000,'fc_rms_target_dbfs':-20,'pink_gain':gain,'layouts':{}}
for layout, matrix in matrices.items():
    n=8 if layout=='7.1' else 6
    out={}
    for kind in ('fc','all'):
        src=root/(layout.replace('(','-').replace(')','')+'-'+kind+'.wav')
        if kind=='fc':
            filt='volume='+repr(gain)+',pan='+layout+'|FC=c0'
            args=[ff,'-v','error','-threads','1','-filter_threads','1','-i',str(noise),'-af',filt,'-c:a','pcm_f32le',str(src)]
        else:
            amp=10**(-3/20)
            expr='|'.join([repr(amp)+'*sin(2*PI*1000*t)']*n)
            args=[ff,'-v','error','-threads','1','-filter_threads','1','-f','lavfi','-i','aevalsrc='+expr+':s=48000:d=12:c='+layout,'-c:a','pcm_f32le',str(src)]
        run(args,src.stem+'-generate')
        facts=json.loads(run([probe,'-v','error','-show_streams','-of','json',str(src)],src.stem+'-probe'))
        out[kind]={'probe':facts,'source':stats(src,src.stem,n),'outputs':{}}
        for mode in ('default','pan','limited'):
            target=root/(src.stem+'-'+mode+'.wav')
            args=[ff,'-v','error','-threads','1','-filter_threads','1','-i',str(src)]
            if mode!='default':
                filt='pan=stereo|'+matrix
                if mode=='limited': filt+=',alimiter=limit='+repr(10**(-1/20))+':level=0:latency=1'
                args+=['-af',filt]
            args+=['-ac','2','-ar','48000','-c:a','pcm_f32le',str(target)]
            run(args,target.stem+'-render')
            out[kind]['outputs'][mode]=stats(target,target.stem,2)
    results['layouts'][layout]={'matrix':'pan=stereo|'+matrix,'fixtures':out}
results['hashes']={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in root.glob('*.wav')}
(root/'commands.json').write_text(json.dumps(commands,indent=2))
(root/'results.json').write_text(json.dumps(results,indent=2))
print(json.dumps(results))
