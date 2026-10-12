import os,pathlib,json,subprocess
root=pathlib.Path('/var/tmp/plurx-hdr-edge-captures-20261012');found=[];tried=0
for directory,dirs,files in os.walk('/media/movies'):
 dirs[:]=sorted(d for d in dirs if not d.startswith('.'))
 for f in sorted(files):
  if not f.lower().endswith(('.mkv','.mp4')) or '2160' not in f:continue
  tried+=1;path=str(pathlib.Path(directory)/f)
  try:
   mapped='/media'+path.removeprefix('/media')
   p=subprocess.run(['docker','exec','plurxd','/usr/lib/jellyfin-ffmpeg/ffprobe','-v','error','-select_streams','v:0','-show_streams','-of','json',mapped],capture_output=True,text=True,timeout=10)
   j=json.loads(p.stdout);s=j['streams'][0]
   if s.get('height')==2160 and s.get('width')==3840 and s.get('color_transfer')=='smpte2084' and not any('DOVI' in x.get('side_data_type','') for x in s.get('side_data_list',[])):
    found.append({'path':path,'stream':s,'source_stat':{'size':os.stat(path).st_size,'mtime_ns':os.stat(path).st_mtime_ns}})
  except Exception:pass
  if len(found)>=1 or tried>=35:break
 if len(found)>=1 or tried>=35:break
(root/'private-sources.json').write_text(json.dumps(found,indent=2));print({'tried':tried,'qualified_sources':len(found)})
