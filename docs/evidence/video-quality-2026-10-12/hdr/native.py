import subprocess,json,pathlib,time,hashlib,re,threading,tempfile,os
root=pathlib.Path(tempfile.mkdtemp(prefix='plurx-hdr-native-oct12-',dir='/var/tmp'));cid=None;stop=threading.Event();report={'commands':[],'scope':'24-second generated sharp-panel PQ native-client comparison','root':str(root)}
def run(a,timeout=65):
 t=time.monotonic();p=subprocess.run(a,capture_output=True,text=True,timeout=timeout);report['commands'].append({'argv':a,'exit':p.returncode,'seconds':time.monotonic()-t,'stderr':p.stderr})
 if p.returncode:raise RuntimeError(p.stderr[-2000:])
 return p.stdout
def idle():
 p=subprocess.run(['curl','-fsS','--max-time','3','http://127.0.0.1:32400/metrics'],capture_output=True,text=True);return p.returncode==0 and re.search(r'^plurx_transcode_sessions_active 0$',p.stdout,re.M)
def watch():
 while not stop.wait(2):
  if not idle():report['abort']='viewer';subprocess.run(['docker','rm','-fv',cid],capture_output=True,timeout=8);return
try:
 assert idle();image=run(['docker','inspect','plurxd','--format','{{.Image}}']).strip();report['image']=image
 cid=run(['docker','run','-d','--rm','--network','none','--cpus','2','--memory','2g','--pids-limit','128','--user',f'{os.getuid()}:{os.getgid()}','--group-add',str(pathlib.Path('/dev/dri/renderD128').stat().st_gid),'--device','/dev/dri/renderD128','--mount',f'type=bind,src={root},dst={root}','--entrypoint','/bin/sleep',image,'240']).strip();report['container']=cid;t=threading.Thread(target=watch);t.start()
 import importlib.util
 spec=importlib.util.spec_from_file_location('signal',pathlib.Path(__file__).parent/'qualify-vaapi.py');sig=importlib.util.module_from_spec(spec);spec.loader.exec_module(sig)
 raw,w,h,count=sig.reference_frame('original-sharp-panels');(root/'source.yuv').write_bytes(raw);report['source_sha256']=hashlib.sha256(raw).hexdigest()
 ff='/usr/lib/jellyfin-ffmpeg/ffmpeg';fp='/usr/lib/jellyfin-ffmpeg/ffprobe';prefix=['docker','exec',cid,'timeout','--kill-after=3','60']
 base=['-nostdin','-hide_banner','-loglevel','verbose','-threads','1','-filter_threads','1','-y','-vaapi_device','/dev/dri/renderD128','-stream_loop','-1','-f','rawvideo','-pixel_format','yuv420p10le','-video_size','1920x1080','-framerate','24','-i',str(root/'source.yuv'),'-f','lavfi','-i','anullsrc=r=48000:cl=stereo','-t','24','-map','0:v:0','-map','1:a:0','-vf','scale=1920:1080,format=yuv420p10le,format=p010le,hwupload']
 recipe=json.load(open(pathlib.Path(__file__).parent/'baseline.json'))['modes']['vbr']['encoder_args']
 for name,extra in [('baseline',[]),('vbaq',['-compression_level','19'])]:
  out=root/(name+'.mp4');run(prefix+[ff]+base+recipe+extra+['-bf','0','-g','48','-keyint_min','48','-sc_threshold','0','-force_key_frames','expr:gte(t,n_forced*2)','-c:a','aac','-b:a','128k','-tag:v','hvc1','-movflags','+faststart',str(out)])
  report[name]={'sha256':hashlib.sha256(out.read_bytes()).hexdigest(),'bytes':out.stat().st_size,'probe':json.loads(run(prefix+[fp,'-v','error','-show_streams','-show_format','-of','json',str(out)]))}
  (root/name).mkdir();run(prefix+[ff,'-nostdin','-v','error','-i',str(out),'-map','0','-c','copy','-hls_time','2','-hls_list_size','0','-hls_segment_type','fmp4','-hls_flags','independent_segments','-hls_segment_filename',str(root/name/'segment-%03d.m4s'),str(root/name/'index.m3u8')])
 report['driver']=run(['docker','exec',cid,'/usr/lib/jellyfin-ffmpeg/vainfo','--display','drm','--device','/dev/dri/renderD128'])
 report['driver_hash']=run(['docker','exec',cid,'sha256sum','/usr/lib/jellyfin-ffmpeg/lib/dri/radeonsi_drv_video.so'])
 report['outcome']='completed'
except Exception as e:report['failure']=repr(e)
finally:
 stop.set()
 if cid:
  try:report['cleanup']=run(['docker','rm','-fv',cid],10)
  except Exception as e:report['cleanup_failure']=repr(e)
 if 't' in globals():t.join(10)
 (root/'result.json').write_text(json.dumps(report,indent=2)+'\n');print(root)
