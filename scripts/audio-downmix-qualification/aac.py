import pathlib, sys
source=pathlib.Path(sys.argv[1])/'collector.py'
exec(source.read_text().split('root.mkdir(exist_ok=True)')[0])
results={}
for layout in ('5.1','5.1-side','7.1'):
    results[layout]={}
    matrix={'5.1':'FL=FL+0.707*FC+0.707*BL|FR=FR+0.707*FC+0.707*BR','5.1-side':'FL=FL+0.707*FC+0.707*SL|FR=FR+0.707*FC+0.707*SR','7.1':'FL=FL+0.707*FC+0.5*SL+0.5*BL|FR=FR+0.707*FC+0.5*SR+0.5*BR'}[layout]
    for kind in ('fc','all'):
        results[layout][kind]={}
        for mode in ('default','pan','limited'):
            src=root/(layout+'-'+kind+'.wav')
            target=root/(layout+'-'+kind+'-'+mode+'.m4a')
            args=[ff,'-v','error','-threads','1','-filter_threads','1','-i',str(src)]
            if mode!='default':
                filt='pan=stereo|'+matrix
                if mode=='limited': filt+=',alimiter=limit=0.8912509381337456:level=0:latency=1'
                args+=['-af',filt]
            args+=['-c:a','aac','-ac','2','-b:a','160k','-ar','48000',str(target)]
            run(args,target.stem+'-aac')
            results[layout][kind][mode]=stats(target,target.stem+'-aac',2)
(root/'aac-commands.json').write_text(json.dumps(commands,indent=2))
(root/'aac-results.json').write_text(json.dumps(results,indent=2))
print(json.dumps(results))
