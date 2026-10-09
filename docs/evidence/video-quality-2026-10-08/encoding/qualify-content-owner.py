#!/usr/bin/env python3
"""Finite isolated durable C2 owner qualification; private pixels remain source-local."""
import hashlib,json,os,pathlib,shutil,sqlite3,subprocess,tempfile,time,urllib.request,uuid
import sys
manifest=json.load(open(sys.argv[1])); output=pathlib.Path(sys.argv[2])
root=pathlib.Path(tempfile.mkdtemp(prefix='plurx-content-owner-',dir='/var/tmp'))
report={'schema_version':1,'scope':'isolated_current_image_durable_offline_owner','host':manifest.get('host','lab6'),'source_revision':manifest['source_revision'],'events':[],'cleanup':{},'resource_limits':{'cpu':2,'memory_gib':2,'wall_seconds':600},'script_sha256':hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest()}
container=None;network=None;deadline=time.monotonic()+600

def run(args,timeout=30):
 p=subprocess.run(args,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=timeout)
 if p.returncode: raise RuntimeError('command failed: '+p.stderr.decode(errors='replace')[-300:])
 return p.stdout.decode()
def idle():
 rows=run(['curl','-fsS','--max-time','5','http://127.0.0.1:32400/metrics']).splitlines()
 counts=[float(x.split()[-1]) for x in rows if x.startswith('plurx_transcode_sessions_active')]
 if not counts or any(counts): raise RuntimeError('active or unknown viewers')
def wait(fn,seconds=120):
 until=min(deadline,time.monotonic()+seconds)
 while time.monotonic()<until:
  idle()
  try:
   value=fn()
   if value:return value
  except (urllib.error.URLError,ConnectionError):pass
  time.sleep(1)
 raise TimeoutError('bounded wait')
try:
 idle(); image=run(['docker','inspect','plurxd','--format','{{.Image}}']).strip();report['image']=image
 report['tool_sha256']={}
 for name,path in [('daemon','/usr/local/bin/plurxd'),('encoder','/usr/lib/jellyfin-ffmpeg/ffmpeg'),('scorer','/usr/local/lib/plurx/vmaf-ffmpeg')]:
  report['tool_sha256'][name]=run(['docker','exec','plurxd','sha256sum',path]).split()[0]
 (root/'library').mkdir();(root/'data').mkdir()
 network='plurx-content-owner-'+uuid.uuid4().hex[:10]
 run(['docker','network','create','--internal',network])
 container=run(['docker','run','--pull','never','--detach','--user',f'{os.getuid()}:{os.getgid()}','--network',network,'--cpus','2','--memory','2g','--pids-limit','256','--mount',f'type=bind,src={root},dst=/work','--mount',f'type=bind,src={manifest["source_path"]},dst=/source,readonly','--entrypoint','/bin/sleep',image,'660']).strip()
 ff='/usr/lib/jellyfin-ffmpeg/ffmpeg'
 run(['docker','exec',container,ff,'-nostdin','-hide_banner','-loglevel','error','-y','-threads','1','-ss','120','-i','/source','-t','60','-map','0:v:0','-map','0:a:0?','-c','copy','-map_metadata','-1','-map_chapters','-1','/work/library/source-1.mkv'],90)
 report['excerpt_sha256']=hashlib.sha256((root/'library/source-1.mkv').read_bytes()).hexdigest()
 report['excerpt_bytes']=(root/'library/source-1.mkv').stat().st_size
 config='[server]\nname="C2 owner qualification"\nbind="0.0.0.0:32400"\n[cluster]\nraft_bind="127.0.0.1:32401"\napi_bind="127.0.0.1:32402"\n[storage]\ndata_dir="/work/data"\n'
 (root/'plurx.toml').write_text(config)
 ip=run(['docker','inspect',container,'--format','{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}']).strip()
 subprocess.run(['docker','exec','-d','-e','PLURX_DATA_DIR=/work/data','-e','PLURX_HWACCEL=software','-e','PLURX_LOG=info',container,'sh','-c','exec /usr/local/bin/plurxd --config /work/plurx.toml run > /work/daemon.log 2>&1'],check=True)
 token=None
 def api(path,body=None,method=None):
  headers={'Content-Type':'application/json'}
  if token:headers['Authorization']='Bearer '+token
  request=urllib.request.Request('http://'+ip+':32400'+path,data=None if body is None else json.dumps(body).encode(),headers=headers,method=method)
  try:
   with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request,timeout=10) as response:
    raw=response.read();return (raw.decode() if path=='/healthz' else json.loads(raw)) if raw else {}
  except urllib.error.HTTPError as e:
   if path=='/healthz' and e.code in [404,503]: return None
   raise RuntimeError('API '+path.split('?')[0]+' '+str(e.code))
 wait(lambda:api('/healthz'))
 token=api('/api/v1/setup',{'username':'qualification','password':uuid.uuid4().hex})['token']
 desired={'offline_enabled':True,'content_aware_encoding':True,'transcode_software_pool_threads':2,'vod_index_mins':0}
 api('/api/v1/settings',desired,'PUT');settings=api('/api/v1/settings')
 report['settings']={k:settings.get(k) for k in list(desired)+['content_encoding_scorer_ready']}
 library=api('/api/v1/libraries',{'name':'Qualification','kind':'home','paths':['/work/library'],'anime':False})
 def scanned():
  status=api('/api/v1/scan/status').get(str(library['id']),{})
  return status.get('last_scan') and not status.get('running')
 wait(scanned)
 items=api('/api/v1/libraries/'+str(library['id'])+'/items?limit=10')['items']
 files=[f for item in items for f in api('/api/v1/items/'+str(item['id'])).get('files',[])]
 assert len(files)==1,'fixture count'
 report['source_probe']={k:files[0].get(k) for k in ['video_codec','height','width','hdr','field_order','duration_ms']}
 package=api('/api/v1/files/'+str(files[0]['id'])+'/offline-packages',{'request_id':str(uuid.uuid4()),'height':480,'audio_index':None,'subtitle_index':None})
 package_id=package['id']; start=time.monotonic()
 def status():
  value=api('/api/v1/offline/packages/'+package_id)
  key=(value['state'],value['phase'])
  if not report['events'] or key!=(report['events'][-1]['state'],report['events'][-1]['phase']):
   report['events'].append({'state':key[0],'phase':key[1],'elapsed_seconds':time.monotonic()-start})
  if value['state'] in ['ready','complete','completed','failed','cancelled']:
   return value
 result=wait(status,360)
 report['package']={k:v for k,v in result.items() if k not in ['id','status_url']}
 # Stop the owned daemon before inspecting its owned SQLite file.
 run(['docker','exec',container,'sh','-c','kill -TERM $(pidof plurxd)']);time.sleep(3)
 dbs=[]
 for path in (root/'data').rglob('*'):
  if path.is_file():
   with open(path,'rb') as f: header=f.read(16)
   if header==b'SQLite format 3\x00':dbs.append(path)
 for path in dbs:
  conn=sqlite3.connect('file:'+str(path)+'?mode=ro',uri=True)
  tables=[r[0] for r in conn.execute("select name from sqlite_master where type='table'")]
  if 'files' in tables:
   rows=conn.execute('select probe_json from files').fetchall()
   report.setdefault('durable_reports',[]).extend(json.loads(r[0]).get('plurx_content_encoding') for r in rows if r[0])
   for value in report['durable_reports']:
    if value:value['context'].pop('source',None)
  if 'offline_packages' in tables:
   conn.row_factory=sqlite3.Row
   row=conn.execute('select * from offline_packages limit 1').fetchone()
   if row is not None:report['offline_snapshot']={k:row[k] for k in row.keys() if 'recipe' in k or 'rate' in k or k in ['state','phase','target_height']}
  conn.close()
 report['status']='measured' if report.get('durable_reports') and any(x and x.get('outcome')=='measured' for x in report['durable_reports']) and result['state'] in ['ready','complete','completed'] else 'incomplete'
except Exception as e:
 report['status']='incomplete';report['error']=type(e).__name__+': '+str(e)
finally:
 if container:report['cleanup']['container_removed']=subprocess.run(['docker','rm','-fv',container],capture_output=True).returncode==0
 if network:report['cleanup']['network_removed']=subprocess.run(['docker','network','rm',network],capture_output=True).returncode==0
 # Only this freshly-created private directory belongs to the experiment.
 if report.get('status')!='measured' and (root/'daemon.log').exists():
  text=(root/'daemon.log').read_text(errors='replace')
  report['diagnostic_codes']=[x for x in ['source-parser','permission denied','panic','scorer','offline'] if x.lower() in text.lower()]
 shutil.rmtree(root);report['cleanup']['private_data_removed']=not root.exists()
 output.write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps({'status':report['status'],'cleanup':report['cleanup']}))
raise SystemExit(0 if report["status"] == "measured" else 1)
