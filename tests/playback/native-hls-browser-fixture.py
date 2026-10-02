#!/usr/bin/env python3
"""Isolated Safari cadence experiment; generated media only, no daemon changes."""
import argparse,json,math,time,threading,re
from pathlib import Path
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.parse import urlsplit,parse_qs
parser=argparse.ArgumentParser();parser.add_argument('--media',required=True);parser.add_argument('--receipt',required=True);parser.add_argument('--port',type=int,default=8765);parser.add_argument('--transport',choices=('native','hlsjs'),default='native')
a=parser.parse_args();media=Path(a.media);repo=Path(__file__).resolve().parents[2]
runs={};lock=threading.Lock();receipt=Path(a.receipt)
html='''<!doctype html><meta charset="utf-8"><title>Native startup qualification</title>
<style>body{background:#101822;color:#dae6f2;font:18px system-ui;padding:30px}video{width:480px}pre{white-space:pre-wrap}button{padding:12px}</style>
<h1>Native startup qualification</h1><p>Generated AVC/AAC · fixed target 16 s · 8 s complete segments · burst 3.8× to bootstrap, then 1.05×</p>
<button id="start">Start ordinary-HLS sweep</button><video id="video" muted playsinline controls></video><pre id="result">Ready. Production playback is untouched.</pre>
<script src="/policy.js"></script><script src="/hls.min.js"></script><script src="/player.js"></script><script src="/authority.js"></script><script>
const PlaybackPolicy=window.PlurxPlaybackPolicy;
const video=document.getElementById('video'),result=document.getElementById('result');let active=null,ordinal=0,records=[],sweeping=false;const fixtureTransport="__TRANSPORT__";const TOKEN=null;
function bufferTargets(){return {fwd:30,back:30};}function parseSegTimes(t){let n=0;return t.split('\\n').filter(l=>l.startsWith('#EXTINF:')).map(l=>n+=parseFloat(l.slice(8)));}
function playbackAttemptTerminallyStopped(){return false;}function playbackOwnsAttachedMedia(){return true;}function playbackContext(){return {};}
function clearStreamFailure(){STREAM_FAILURE=null;}
function setPlaybackMediaSource(v,url){v.src=url;}function applyPlaybackAttachmentPosition(v,p,attachment,startAt){if(attachment.current())v.currentTime=startAt;}
function applyPlaybackTransportIntent(v,p){if(p.wantsPlayback)v.play().catch(e=>log('play-rejected',String(e)));else v.pause();}
function pausePlaybackInternally(v){v.pause();}function resetPlaybackTransportEvents(){}
function positionForPlaybackIntent(v){return v.currentTime;}function stallRecoverySnapshot(p,v,facts){return facts;}
function endWait(){}function seekTo(){log('unexpected-authority-reopen','');}function showStallRecoveryFailure(message){log('authority-exhausted',message);}
function raisePlaybackSurface(source,facts){log('surface',{source,...facts});}function toast(){}
function playbackSurfaceSourceIsBlocking(source,context){const row=PlaybackPolicy.SURFACE_SOURCES.find(r=>r.id===source&&(r.context==='any'||r.context===context));return !!PlaybackPolicy.SURFACE_CLASSES[row?.class]?.blocking;}
function clientLog(x){log(x.event,x.message);}function reportTtff(){}function notifyPlaybackControl(){}
function stopPlayerForExhaustion(){video.pause();}function stallDiagnose(){log('exhausted',PLAYER.hlsStartup?.latestFailure||PLAYER.hlsStartup?.exhaustedReason);return Promise.resolve();}
function tok(u){return u;}function nativeHlsSubtitleOrdinal(){return -1;}function qualityCatalogSelectionCurrent(){return true;}function startTranscodeFallback(){log('unexpected-rescue','');}
function log(event,detail){if(!active)return;const row={event,detail,elapsed_ms:Math.round(performance.now()-active.at),position:video.currentTime};active.events.push(row);render();navigator.sendBeacon('/event',JSON.stringify({run:active.id,...row}));}
function render(){result.textContent=JSON.stringify({records,active:active?{threshold:active.threshold,events:active.events,position:video.currentTime,buffered:video.buffered.length?video.buffered.end(video.buffered.length-1):0}:null},null,2);}
async function start(threshold){if(PLAYER?.hlsStartup)cancelHlsStartup(PLAYER,'fixture_next');if(PLAYER?.hls)PLAYER.hls.destroy();video.pause();video.removeAttribute('src');video.load();
 const id=String(++ordinal);await fetch('/begin',{method:'POST',body:JSON.stringify({run:id,threshold})});
 active={id,threshold,at:performance.now(),events:[],first:false};PLAYER={hls:null,wantsPlayback:true,mediaAttachment:{},sessionId:'fixture-'+id,controlIntentGeneration:0,method:'remux',qualityCandidates:[{route:'encode',decoder_compatible:true}],abr:{}};
 const player=PLAYER,attachment={current:()=>PLAYER===player};const url='/qual/'+id+'/master.m3u8';
 if(fixtureTransport==='native')attachNativeHls(video,url,0,player,attachment);
 else{const {startup,tgt}=hlsStartupEpisode(player,attachment,url,0);
   const hls=constructHls(startup,tgt,video,0,()=>PLAYER===player&&player.hls===startup.hls);
   hls.on(Hls.Events.MANIFEST_PARSED,()=>{startup.manifestState='parsed';applyPlaybackTransportIntent(video,player);});
   hls.on(Hls.Events.FRAG_LOADED,()=>{startup.mediaLoaded=true;});
   hls.on(Hls.Events.ERROR,(_,d)=>log('hls-error',{fatal:d.fatal,type:d.type,details:d.details}));}
 render();}
video.addEventListener('playing',()=>log('playing',''));video.addEventListener('waiting',()=>log('waiting',''));video.addEventListener('error',()=>{log('native-error',video.error?.code);classifyNativeHlsError(video,PLAYER,video.error?.code,'fixture').catch(e=>log('classification-error',String(e)));});
function frame(){if(active&&!active.first){active.first=true;log('first-presented-frame','');completeHlsStartup(PLAYER);}video.requestVideoFrameCallback(frame);}video.requestVideoFrameCallback(frame);
setInterval(()=>{if(active){navigator.sendBeacon('/event',JSON.stringify({run:active.id,event:'sample',position:video.currentTime,elapsed_ms:Math.round(performance.now()-active.at)}));render();}},1000);
document.getElementById('start').onclick=async()=>{if(sweeping)return;sweeping=true;const continuity=Number(new URLSearchParams(location.search).get('continuity'));if([12,16,24,32,48].includes(continuity)){await start(continuity);log('continuity-start','30 minute observation');return;}for(const threshold of [12,16,24,32,48]){await start(threshold);await new Promise(r=>setTimeout(r,45000));records.push({threshold,events:active.events});}await start(32);log('continuity-start','30 minute observation');sweeping=false;};
</script>'''.replace('__TRANSPORT__',a.transport)
def produced(r,now):
 elapsed=max(0,now-r['at']);first=2.25
 burst=max(0,math.ceil(r['threshold']/8)*8-8)
 return 0 if elapsed<first else 8+min(burst,(elapsed-first)*3.8)+max(0,elapsed-first-burst/3.8)*1.05
def snapshot(r,now):
 count=min(237,math.floor(produced(r,now)/8))
 if r['published']==0:
  if count*8<r['threshold']:return 0
 elif now-r['publish_at']<16:return r['published']
 # Include completed prefix; this fixture measures discovery, not scratch or actor admission.
 r['published']=count;r['publish_at']=now
 return count
def playlist(count):
 return '#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:16\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-START:TIME-OFFSET=0\n#EXT-X-MAP:URI="init.mp4"\n'+''.join(f'#EXTINF:8.000,\nseg_{i:04d}.m4s\n' for i in range(count))
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def send(self,data,mime='application/json',status=200):
  if isinstance(data,str):data=data.encode()
  self.send_response(status);self.send_header('Content-Type',mime);self.send_header('Content-Length',str(len(data)));self.send_header('Cache-Control','no-store');self.end_headers()
  try:self.wfile.write(data)
  except (BrokenPipeError,ConnectionResetError):pass
 def do_POST(self):
  data=json.loads(self.rfile.read(min(int(self.headers.get('Content-Length',0)),65536)))
  with lock:
   if self.path=='/begin':runs[data['run']]={**data,'at':time.monotonic(),'published':0,'publish_at':0,'events':[],'requests':[]}
   elif self.path=='/event' and data['run'] in runs:
    runs[data['run']]['events'].append(data)
    pending=receipt.with_suffix('.tmp');pending.write_text(json.dumps({k:{'transport':a.transport,**{kk:vv for kk,vv in v.items() if kk not in ('at','publish_at')}} for k,v in runs.items()},indent=2)+'\n');pending.replace(receipt)
  self.send('{}')
 def do_GET(self):
  path=urlsplit(self.path).path
  if path=='/':return self.send(html,'text/html')
  if path=='/authority.js':
   # Exercise the shipped authority owner; fixture session-open effects above
   # remain explicit and are not daemon lifetime/flow qualification.
   text=(repo/'crates/plurxd/src/web/player/measurements.js').read_text()
   start=text.index('function recoverServingFencedAttachment(')
   tail=text[start:];following=re.search(r'\n(?:async )?function ',tail)
   return self.send(tail[:following.start()] if following else tail,'text/javascript')
  if path in ('/player.js','/policy.js','/hls.min.js'):
   rel='player/player.js' if path=='/player.js' else 'hls.min.js' if path=='/hls.min.js' else 'playback-policy.js'
   return self.send((repo/'crates/plurxd/src/web'/rel).read_bytes(),'text/javascript')
  parts=path.split('/')
  if len(parts)!=4 or parts[1]!='qual' or parts[2] not in runs:return self.send('{}',status=404)
  run=parts[2];name=parts[3]
  if name in ('master.m3u8','index.m3u8'):
   with lock:r=runs[run];r['requests'].append({'resource':name,'elapsed_s':round(time.monotonic()-r['at'],3)});count=snapshot(r,time.monotonic())
   # Model the aligned server wait; the application still owns its 12 s/episode bound.
   end=time.monotonic()+10
   while count==0 and time.monotonic()<end:
    time.sleep(.05)
    with lock:count=snapshot(r,time.monotonic())
   if not count:return self.send(json.dumps({'code':'response_publication_timeout','message':'Still preparing generated media.'}),status=503)
   text='#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=200000\nindex.m3u8\n' if name=='master.m3u8' else playlist(count)
   return self.send(text,'application/vnd.apple.mpegurl')
  if name=='init.mp4' or (name.startswith('seg_') and name.endswith('.m4s')):
   p=media/name
   if not p.is_file():return self.send('{}',status=404)
   return self.send(p.read_bytes(),'video/mp4')
  self.send('{}',status=404)
print('Generated-media fixture at http://127.0.0.1:'+str(a.port),flush=True)
ThreadingHTTPServer(('127.0.0.1',a.port),Handler).serve_forever()
