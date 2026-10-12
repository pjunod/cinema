import subprocess,json,pathlib,time,hashlib,re,threading,tempfile,sys
root=pathlib.Path(tempfile.mkdtemp(prefix='plurx-hdr-real-oct12-',dir='/var/tmp')); cid=None;stop=threading.Event()
source=json.load(open(pathlib.Path(__file__).parent/'private-sources.json'))[0]; offset=int(sys.argv[1]); report={'source_selection':{'opaque_id':'hdr-oct12-title01','offset_seconds':offset,'stat':source['source_stat']},'commands':[],'scope':'48-frame private plainPQ native16:9 title window; source bytes stay on host','root':str(root)}
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
 cid=run(['docker','run','-d','--rm','--network','none','--cpus','2','--memory','2g','--pids-limit','128','--user',str(__import__('os').getuid())+':'+str(__import__('os').getgid()),'--group-add',str(pathlib.Path('/dev/dri/renderD128').stat().st_gid),'--device','/dev/dri/renderD128','--mount',f'type=bind,src={root},dst={root}','--mount','type=bind,src='+source['path']+',dst=/input.mkv,readonly','--entrypoint','/bin/sleep',image,'200']).strip()
 report['container']=cid;t=threading.Thread(target=monitor);t.start()
 template=json.load(open(pathlib.Path(__file__).parent/'real-template.json'))
 oldroot=template['root'];oldcid=template['container']
 def adapt(a,name='baseline'):
  return [x.replace(oldroot,str(root)).replace(oldcid,cid).replace('baseline',name) for x in a]
 cmds=[c['argv'] for c in template['commands']]
 gen=adapt(next(a for a in cmds if 'ffv1' in a)); gen[gen.index('-ss')+1]=str(offset); run(gen)
 ref=root/'reference.mkv';report['reference_sha256']=hashlib.sha256(ref.read_bytes()).hexdigest()
 for name,args in [('baseline',[]),('vbaq',['-compression_level','19']),('full',['-compression_level','1'])]:
  a=adapt(next(a for a in cmds if 'hevc_vaapi' in a),name);a[-1:-1]=args; a[a.index('-ss')+1]=str(offset); run(a)
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
 report['source_unchanged']=source['source_stat']=={'size':pathlib.Path(source['path']).stat().st_size,'mtime_ns':pathlib.Path(source['path']).stat().st_mtime_ns}; data=json.dumps(report,indent=2).replace(source['path'],'private-source-readonly'); (root/'result.json').write_text(data+'\n');print(root,flush=True)
