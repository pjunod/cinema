#!/usr/bin/env python3
"""Finite actual Safari native-HLS capture; no WebDriver/preferences required."""
import argparse, hashlib, http.server, json, pathlib, subprocess, threading, time
from urllib.parse import urlsplit
PAGE = r'''<!doctype html><meta charset="utf-8"><title>Plurx owned Safari qualification</title><video id="v" muted playsinline width="640" height="360"></video><pre id="s">Running finite native HLS observations.</pre><script>
const v=document.querySelector('video');v.muted=true;const canvas=document.createElement('canvas');canvas.width=32;canvas.height=18;const ctx=canvas.getContext('2d',{willReadFrequently:true});
const report={userAgent:navigator.userAgent,nativeHls:v.canPlayType('application/vnd.apple.mpegurl'),rvfc:typeof v.requestVideoFrameCallback==='function',visibility:document.visibilityState,hdrDisplayMediaQuery:matchMedia('(dynamic-range: high)').matches,cases:[],events:[],callbackCount:0};let phase=null,callbackId=null;
window.addEventListener('error',e=>report.events.push({event:'script-error',message:e.message}));function frame(w,m){report.callbackCount++;if(phase&&!v.seeking&&!v.paused){ctx.drawImage(v,0,0,32,18);const p=ctx.getImageData(0,0,32,18).data;let hash=0,min=255,max=0;for(let i=0;i<p.length;i++){hash=(hash*31+p[i])>>>0;if(i%4!==3){min=Math.min(min,p[i]);max=Math.max(max,p[i])}}phase.frames.push({wall:w,time:m.mediaTime,clock:v.currentTime,presented:m.presentedFrames,pixels:hash,pixelRange:max-min,width:v.videoWidth,height:v.videoHeight});}callbackId=v.requestVideoFrameCallback(frame);}
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
function until(f,ms=15000){const end=performance.now()+ms;return new Promise((resolve,reject)=>{function tick(){try{if(v.error)throw Error(v.error.message);if(f())return resolve();if(performance.now()>end)throw Error('deadline');setTimeout(tick,25)}catch(e){reject(e)}}tick()})}
for(const e of ['error','waiting','stalled','seeking','seeked','playing'])v.addEventListener(e,()=>report.events.push({event:e,time:v.currentTime,wall:performance.now(),stage:phase?.name,error:v.error?.message}));
async function observe(row,name,target,reload){phase=null;if(callbackId!==null)v.cancelVideoFrameCallback(callbackId);callbackId=null;v.pause();if(reload){v.src='/'+row.asset+'/index.m3u8';v.load();await until(()=>v.readyState>=1)}v.currentTime=target;await until(()=>!v.seeking&&Math.abs(v.currentTime-target)<0.1);const p={name,target,frames:[],started:performance.now()};row.phases.push(p);phase=p;callbackId=v.requestVideoFrameCallback(frame);await v.play();await until(()=>p.frames.length>=30);phase=null;v.pause();p.firstFrameMs=p.frames[0].wall-p.started;p.firstError=Math.abs(p.frames[0].time-target);p.maxClockError=Math.max(...p.frames.map(x=>Math.abs(x.time-x.clock)));p.reversals=p.frames.slice(1).filter((x,i)=>x.time<p.frames[i].time-0.001).length;p.uniquePixels=new Set(p.frames.map(x=>x.pixels)).size;p.pass=p.firstError<=0.25&&p.maxClockError<=0.25&&p.reversals===0&&(STATIC?p.frames.every(x=>x.pixelRange>16):p.uniquePixels>1)&&p.frames.every(x=>x.width>0&&x.height>0);}
(async()=>{try{if(!report.rvfc||!report.nativeHls)throw Error('required Safari API unavailable');for(const asset of ASSETS){const row={asset,phases:[]};report.cases.push(row);for(const [name,target,reload] of PHASES)await observe(row,name,target,reload)}report.pass=report.cases.every(c=>c.phases.length===PHASES.length&&c.phases.every(p=>p.pass))}catch(e){report.error=String(e);report.pass=false}finally{phase=null;v.pause();v.removeAttribute('src');v.load();await fetch('/receipt',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(report)});document.querySelector('#s').textContent=JSON.stringify(report,null,2)}})();
</script>'''
def main():
 p=argparse.ArgumentParser();p.add_argument('--served',type=pathlib.Path,required=True);p.add_argument('--output',type=pathlib.Path,required=True);p.add_argument('--assets',nargs='+',default=['sdr-bf0','sdr-bf2']);p.add_argument('--static',action='store_true');p.add_argument('--phases',nargs='+',default=['cold-mid','forward','backward','restart']);p.add_argument('--deadline',type=int,default=180);a=p.parse_args();done=threading.Event();state={}; phase_specs={'cold-mid':['cold-mid',6,True],'forward':['forward',18,False],'backward':['backward',3,False],'restart':['restart',0,True]};page=PAGE.replace('ASSETS',json.dumps(a.assets)).replace('PHASES',json.dumps([phase_specs[n] for n in a.phases])).replace('STATIC',json.dumps(a.static)).encode()
 class Handler(http.server.SimpleHTTPRequestHandler):
  def __init__(self,*args,**kwargs):super().__init__(*args,directory=str(a.served),**kwargs)
  def log_message(self,*args):pass
  def do_GET(self):
   if urlsplit(self.path).path=='/':self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(page)
   else:super().do_GET()
  def do_POST(self):
   if self.path!='/receipt':self.send_error(404);return
   size=int(self.headers.get('Content-Length','0'))
   if not 0<size<2000000:self.send_error(400);return
   state['receipt']=json.loads(self.rfile.read(size));self.send_response(200);self.end_headers();done.set()
 server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start();url='http://127.0.0.1:'+str(server.server_port)+'/'
 doc=False
 try:
  cmd='tell application "Safari"\nset d to make new document with properties {URL:"'+url+'"}\nactivate\nreturn true\nend tell'
  doc=True;subprocess.check_output(['/usr/bin/osascript','-e',cmd],text=True,timeout=15).strip();state['owned_document_created']=True
  if not done.wait(a.deadline):state['error']='bounded Safari receipt deadline exceeded'
 finally:
  if doc:
   cmd='tell application "Safari"\nrepeat with i from (count documents) to 1 by -1\nset d to document i\nif URL of d starts with "'+url+'" then close d\nend repeat\nend tell'
   try:subprocess.run(['/usr/bin/osascript','-e',cmd],check=True,timeout=15,capture_output=True);state['owned_document_close_requested']=True
   except Exception as e:state['cleanup_error']=str(e)
  server.shutdown();server.server_close();state['server_closed']=True
  state['harness_sha256']=hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest();a.output.write_text(json.dumps(state,indent=2));print(json.dumps({k:v for k,v in state.items() if k!='receipt'}));print('pass:',state.get('receipt',{}).get('pass'))
 if state.get('receipt',{}).get('pass') is not True:raise SystemExit(1)
if __name__=='__main__':main()
