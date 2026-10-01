import pathlib, sys
exec((pathlib.Path(sys.argv[1])/'collector.py').read_text().split('root.mkdir(exist_ok=True)')[0])
for layout in ('5.1','5.1-side','7.1'):
    for suffix in ('all-pan.wav','all-limited.wav','all-limited.m4a','all-margin2.m4a'):
        source=root/(layout+'-'+suffix)
        run([ff,'-hide_banner','-threads','1','-filter_threads','1','-i',str(source),'-af','astats=metadata=0:reset=0','-f','null','-'],source.stem+'-'+source.suffix[1:]+'-astats')
(root/'astats-commands.json').write_text(json.dumps(commands,indent=2))
print('12 actual astats cross-checks completed')
