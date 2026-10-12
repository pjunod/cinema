import subprocess,json,pathlib,time,hashlib,re,threading,tempfile,sys
root=pathlib.Path(tempfile.mkdtemp(prefix='plurx-hdr-pressure-oct12-',dir='/var/tmp')); cid=None;stop=threading.Event()
report={'commands':[],'scope':'48-frame analytical PQ motion/noise; bounded cause isolation','root':str(root)}
def run(a,timeout=50):
 t=time.monotonic();p=subprocess.run(a,capture_output=True,text=True,timeout=timeout);report['commands'].append({'argv':a,'exit':p.returncode,'seconds':time.monotonic()-t,'stderr':p.stderr[-6000:]})
 if p.returncode:raise RuntimeError(p.stderr[-2000:])
 return p.stdout
def idle():
 p=subprocess.run(['curl','-fsS','--max-time','3','http://127.0.0.1:32400/metrics'],capture_output=True,text=True)
 return p.returncode==0 and re.search(r'^plurx_transcode_sessions_active 0$',p.stdout,re.M)
def monitor():
 while not stop.wait(2):
  if not idle():
   report['abort']='viewer or metrics unavailable';subprocess.run(['docker','rm','-fv',cid],capture_output=True,timeout=8);return
try:
 assert idle()
 image=run(['docker','inspect','plurxd','--format','{{.Image}}']).strip();report['image']=image
 cid=run(['docker','run','-d','--rm','--network','none','--cpus','2','--memory','2g','--pids-limit','128','--user',str(__import__('os').getuid())+':'+str(__import__('os').getgid()),'--group-add',str(pathlib.Path('/dev/dri/renderD128').stat().st_gid),'--device','/dev/dri/renderD128','--mount',f'type=bind,src={root},dst={root}','--entrypoint','/bin/sleep',image,'200']).strip()
 report['container']=cid;t=threading.Thread(target=monitor);t.start()
 template=json.load(open(pathlib.Path(__file__).parent/'pressure-template.json'))
 oldroot=template['root'];oldcid=template['container']
 def adapt(a,name='baseline'):
  return [x.replace(oldroot,str(root)).replace(oldcid,cid).replace('baseline',name) for x in a]
 cmds=[c['argv'] for c in template['commands']]
 gen=next(a for a in cmds if 'lavfi' in a);run(adapt(gen))
 ref=root/'reference.mkv';report['reference_sha256']=hashlib.sha256(ref.read_bytes()).hexdigest()
 for name,args in [('baseline',[]),('vbaq',['-compression_level','19']),('full',['-compression_level','1'])]:
  a=adapt(next(a for a in cmds if 'hevc_vaapi' in a),name);a[-1:-1]=args;run(a)
  probe=adapt(next(a for a in cmds if '-show_packets' in a and a[-1].endswith('baseline.mp4')),name);facts=json.loads(run(probe));packets=facts.get('packets',facts.get('packets_and_frames',[]));frames=facts.get('frames',facts.get('packets_and_frames',[]))
  packets=[p for p in packets if 'size' in p];frames=[f for f in frames if 'pix_fmt' in f]
  metric={}
  for typ,field in [('psnr','psnr_avg'),('ssim','All')]:
   a=adapt(next(a for a in cmds if any(typ+'=stats_file=' in x and 'baseline' in x for x in a)),name);run(a)
   values=[float(x) for x in re.findall(r'\b'+field+r':([\d.]+)',(root/(name+'-'+typ+'.txt')).read_text())]
   metric[typ]={'mean':sum(values)/len(values),'min':min(values),'frames':len(values)}
  times=[float(p['pts_time']) for p in packets];sizes=[int(p['size'])*8 for p in packets]
  peak=max(sum(sizes[k] for k,v in enumerate(times) if start<=v<start+1) for start in times)
  report[name]={'sha256':hashlib.sha256((root/(name+'.mp4')).read_bytes()).hexdigest(),'bytes':(root/(name+'.mp4')).stat().st_size,'metrics':metric,'packet_bits':sum(sizes),'peak_1s_bits':peak,'frames':frames,'packets':packets,'streams':facts['streams']}
 report['outcome']='completed'
except Exception as e:report['failure']=repr(e)
finally:
 stop.set()
 if cid:
  try:report['cleanup']=run(['docker','rm','-fv',cid],10)
  except Exception as e:report['cleanup_failure']=repr(e)
 if 't' in globals():t.join(10)
 (root/'result.json').write_text(json.dumps(report,indent=2)+'\n');print(root,flush=True)
