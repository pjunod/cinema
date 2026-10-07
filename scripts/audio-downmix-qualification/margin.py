import pathlib, sys
exec((pathlib.Path(sys.argv[1])/'collector.py').read_text().split('root.mkdir(exist_ok=True)')[0])
results={}
for layout in ('5.1','5.1-side','7.1'):
    src=root/(layout+'-all-pan.wav')
    target=root/(layout+'-all-margin2.m4a')
    run([ff,'-v','error','-threads','1','-filter_threads','1','-i',str(src),'-af','alimiter=limit=0.7943282347242815:level=0:latency=1','-c:a','aac','-ac','2','-b:a','160k','-ar','48000',str(target)],layout+'-margin2-render')
    results[layout]=stats(target,layout+'-margin2',2)
(root/'margin-commands.json').write_text(json.dumps(commands,indent=2))
(root/'margin-results.json').write_text(json.dumps(results,indent=2))
print(json.dumps(results))
