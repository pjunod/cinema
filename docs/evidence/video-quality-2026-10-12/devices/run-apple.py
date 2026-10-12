#!/usr/bin/env python3
"""Run a finite asset case on an explicitly supplied task-owned tvOS simulator."""
import argparse, hashlib, json, os, pathlib, subprocess, time
p=argparse.ArgumentParser()
for name in ['device','bundle','case','asset','out']: p.add_argument('--'+name,required=True)
p.add_argument('--port',type=int,required=True)
p.add_argument('--static',action='store_true')
a=p.parse_args()
package='org.plurx.qualification.video.oct12'
record={'schema':1,'case':a.case,'binary_sha256':hashlib.sha256((pathlib.Path(a.bundle)/'VideoProbe').read_bytes()).hexdigest(),'started_utc':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime())}
def run(*args,check=True,env=None):
 r=subprocess.run(['xcrun','simctl',*args],capture_output=True,text=True,timeout=45,env=env)
 if check and r.returncode: raise RuntimeError('simctl '+args[0]+': '+r.stderr[-500:])
 return r.stdout.strip()
try:
 run('install',a.device,a.bundle)
 run('terminate',a.device,package,check=False)
 folder=pathlib.Path(run('get_app_container',a.device,package,'data'))/'Documents'/'qualification.json'
 folder.unlink(missing_ok=True)
 if folder.exists():raise RuntimeError('Could not establish fresh receipt absence')
 env=dict(os.environ,SIMCTL_CHILD_QUAL_URL=f'http://127.0.0.1:{a.port}/'+a.asset.lstrip('/'),SIMCTL_CHILD_QUAL_CASE=a.case,SIMCTL_CHILD_QUAL_STATIC='1' if a.static else '0')
 run('launch',a.device,package,env=env)
 deadline=time.monotonic()+105
 while time.monotonic()<deadline:
  if folder.exists():
   value=json.loads(folder.read_text())
   if value.get('case')!=a.case:raise RuntimeError('Wrong case identity')
   record['probe']=value
   break
  time.sleep(1)
 else:raise RuntimeError('105-second fresh receipt deadline exceeded')
except Exception as error:record['launcher_error']=str(error)
finally:
 try:
  run('terminate',a.device,package,check=False)
  record['owned_app_terminated']=True
 except Exception as error:record['cleanup_error']=str(error)
 record['finished_utc']=time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime())
 pathlib.Path(a.out).write_text(json.dumps(record,indent=2)+'\n')
print(json.dumps({'case':a.case,'result':record.get('probe',{}).get('result'),'error':record.get('launcher_error')}))
raise SystemExit(0 if record.get('probe',{}).get('result')=='pass' and not record.get('launcher_error') and not record.get('cleanup_error') else 1)
