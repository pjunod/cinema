import json,pathlib,subprocess,hashlib
controls=['/var/tmp/plurx-hdr-pressure-oct12-p1h9668g','/var/tmp/plurx-hdr-real-oct12-lvtx29zt','/var/tmp/plurx-hdr-real-oct12-_ag0t8ls','/var/tmp/plurx-hdr-real-oct12-4lnu02yu'];out={}
for directory in controls:
 p=pathlib.Path(directory);result=json.load(open(p/'result.json'));raw=subprocess.check_output(['docker','run','--rm','--network','none','--cpus','1','--memory','512m','--pids-limit','64','--user','1000:1000','--mount',f'type=bind,src={p},dst=/capture,readonly','--entrypoint','/usr/lib/jellyfin-ffmpeg/ffprobe',result['image'],'-v','error','-select_streams','v:0','-show_frames','-show_streams','-of','json','/capture/reference.mkv'],timeout=40)
 facts=json.loads(raw);frames=facts['frames'];grid=[f['pts_time'] for f in frames];grades=[{k:f.get(k) for k in ['width','height','pix_fmt','color_range','color_space','color_primaries','color_transfer']} for f in frames]
 x={'reference_probe':facts,'reference_sha256':hashlib.sha256((p/'reference.mkv').read_bytes()).hexdigest(),'metrics':{f.name:f.read_text() for f in p.glob('*-*.txt')},'frame_matches':{}}
 for name in ['baseline','vbaq','full']:
  other=result[name]['frames'];og=[f['pts_time'] for f in other];othergrades=[{k:f.get(k) for k in grades[0]} for f in other]
  x['frame_matches'][name]={'reference_count':len(frames),'output_count':len(other),'same_pts_grid':grid==og,'same_grade':grades==othergrades}
 x['source_pair_note']='second window first attempt deliberately invalid; reference3600 encoder1800 retained as rejected' if directory.endswith('_ag0t8ls') else 'reference and encoder use same source and seek in recorded commands'
 out[directory]=x
pathlib.Path('/var/tmp/plurx-hdr-edge-captures-20261012/control-details.json').write_text(json.dumps(out,indent=2)+'\n');print({k:v['frame_matches'] for k,v in out.items()})
