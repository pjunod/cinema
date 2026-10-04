"use strict";
const test=require('node:test'),assert=require('node:assert/strict'),vm=require('node:vm'),fs=require('node:fs'),path=require('node:path');
const source=fs.readFileSync(path.join(__dirname,'../../crates/plurxd/src/web/player/player.js'),'utf8');
function shippedFunction(file,name){
 const text=fs.readFileSync(path.join(__dirname,'../../crates/plurxd/src/web/player/',file),'utf8');
 const start=text.indexOf(`function ${name}(`);assert.ok(start>=0,name);
 const tail=text.slice(start),next=/\n(?:async )?function /.exec(tail);
 return next?tail.slice(0,next.index):tail;
}
const policy=require('../../crates/plurxd/src/web/playback-policy.js');
const master='#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=200000\nindex.m3u8\n';
const media='#EXTM3U\n#EXT-X-TARGETDURATION:16\n#EXT-X-MAP:URI="init.mp4"\n#EXTINF:8,\nseg_0000.m4s\n#EXTINF:8,\nseg_0001.m4s\n#EXTINF:8,\nseg_0002.m4s\n';
async function flush(){for(let i=0;i<16;i++)await new Promise(resolve=>setImmediate(resolve));}
function harness({pendingOpen=false,pendingChange=false}={}){
 let now=0,ordinal=0;const timers=new Map(),answers=[],requests=[],sources=[],rescues=[],diagnoses=[],positions=[],listeners=new Map(),reopens=[],surfaces=[],stops=[];
 const video={readyState:0,videoWidth:0,currentTime:0,paused:true,currentSrc:'/session/master.m3u8',getAttribute:()=>'/session/master.m3u8',addEventListener(event,fn){listeners.set(event,fn);},removeEventListener(event,fn){if(listeners.get(event)===fn)listeners.delete(event);},removeAttribute(){},load(){},pause(){},play(){return Promise.resolve();}};
 const player={hls:null,wantsPlayback:true,method:'remux',sessionId:'retired',stallRecoveries:0,mediaAttachment:{},controlIntentGeneration:0,qualityCandidates:[],abr:{}};
 if(pendingOpen) player.pendingOpenAttempt={};
 if(pendingChange) player.pendingMediaChange={};
 const ctx=vm.createContext({PLAY_OPEN_GATE:{current:()=>true},PlaybackPolicy:policy,URL,TextDecoder,AbortController,Uint8Array,console,
  document:{getElementById:id=>id==='video'?video:{classList:{remove(){}}}},performance:{now:()=>now},location:{href:'http://fixture.invalid/'},player,video,
  setTimeout:(fn,ms)=>{const id=++ordinal;timers.set(id,{fn,at:now+ms});return id;},clearTimeout:id=>timers.delete(id),
  fetch:async(url,options)=>{requests.push({url,options});const answer=answers.shift();if(!answer)throw Error('No scripted answer');return typeof answer==='function'?answer(options):answer;},
  bufferTargets:()=>({}),parseSegTimes:text=>text.split('\n').filter(line=>line.startsWith('#EXTINF:')).map((_,i)=>(i+1)*8),
  setPlaybackMediaSource:(_,url)=>sources.push(url),applyPlaybackTransportIntent(){},applyPlaybackAttachmentPosition(){positions.push(true);},pausePlaybackInternally(){},resetPlaybackTransportEvents(){},
  qualityCatalogSelectionCurrent:()=>true,notifyPlaybackControl(){},clearStall(){},playbackContext:()=>({}),clientLog(){},tok:x=>x,
  pbTick(){},pbSyncPlayIcon(){},endWait(){},positionForPlaybackIntent:()=>42,stallRecoverySnapshot:(_p,_v,facts)=>facts,seekTo:(...args)=>reopens.push(args),showStallRecoveryFailure(){throw Error('unexpected recovery exhaustion');},playbackIsReal:()=>false,playbackSurfaceSourceIsBlocking:()=>true,stopPlayerForExhaustion:()=>stops.push(true),raisePlaybackSurface:(source,facts)=>surfaces.push(policy.presentSurface(policy.initialSurfaceState(),{raise:source,...facts,now_ms:now})),toast(){},
  startTranscodeFallback:(...args)=>rescues.push(args),stallDiagnose:()=>{diagnoses.push(true);return Promise.resolve();}});
 vm.runInContext(source,ctx);
 vm.runInContext("PLAYER=player;function clearStreamFailure(){STREAM_FAILURE=null;}",ctx);
 vm.runInContext(shippedFunction("stall-diagnosis.js","hasPendingPlaybackOpen"),ctx);
 vm.runInContext(shippedFunction("stall-diagnosis.js","playbackOwnsAttachedMedia"),ctx);
 vm.runInContext(shippedFunction("measurements.js","recoverServingFencedAttachment"),ctx);
 vm.runInContext(shippedFunction("transport.js","wirePlayerMedia"),ctx);
 ctx.wirePlayerMedia(video);
 const response=(text,status=200,type='application/vnd.apple.mpegurl')=>new Response(text,{status,headers:{'Content-Type':type}});
 ctx.attachNativeHls(video,'http://fixture.invalid/session/master.m3u8?token=fixture',0,player,{current:()=>ctx.current!==false});
 if(!pendingOpen&&!pendingChange){for(const [id,t]of [...timers])if(t.at<=now){timers.delete(id);t.fn();}}
 return {ctx,player,video,answers,requests,sources,rescues,diagnoses,positions,listeners,response,reopens,surfaces,stops,
  advance(ms){now+=ms;for(const [id,t]of [...timers])if(t.at<=now){timers.delete(id);t.fn();}},
  retry(){ctx.runNativeHlsReadiness(video,player,player.hlsStartup);},
  stop(){ctx.cancelHlsStartup(player,'test_done');},
 };
}
test('native readiness waits for master and selected child before assigning src',async()=>{
 const h=harness();await flush();
 h.answers.push(h.response(JSON.stringify({code:'response_publication_timeout',message:'not yet'}),503,'application/json'));
 h.advance(1000);await flush();assert.equal(h.sources.length,0);assert.equal(h.rescues.length,0);
 h.answers.push(h.response(master),h.response(media));h.advance(2000);await flush();
 assert.equal(h.sources.length,1);assert.equal(h.player.segTimes.length,3);
 assert.match(h.requests.at(-1).url,/index\.m3u8\?token=fixture$/);
 assert.equal(h.player.hlsStartup.dispatches,4);h.stop();
});
test('native readiness ignores superseded delayed body and does not leak credentials across session',async()=>{
 const h=harness();await flush();
 let release;h.answers.push(()=>new Promise(resolve=>release=resolve));h.advance(1000);await flush();
 h.ctx.current=false;release(h.response(master));await flush();assert.equal(h.sources.length,0);h.stop();
 assert.throws(()=>h.ctx.nativeHlsResourceUrl('http://other.invalid/index.m3u8','http://fixture.invalid/session/master.m3u8?token=fixture'),/outside/);
 assert.throws(()=>h.ctx.nativeHlsResourceUrl('../another/index.m3u8','http://fixture.invalid/session/master.m3u8?token=fixture'),/outside/);
});
test('native pause and expired resume retain the original readiness deadline',async()=>{
 const h=harness();await flush();const episode=h.player.hlsStartup,deadline=episode.deadlineMs;
 h.ctx.pauseHlsStartup(h.player);h.player.wantsPlayback=false;h.advance(50000);await flush();assert.equal(h.sources.length,0);
 h.player.wantsPlayback=true;h.player.controlIntentGeneration++;h.ctx.resumeHlsStartup(h.video,h.player);await flush();
 assert.equal(episode.deadlineMs,deadline);assert.equal(episode.state,'exhausted');assert.equal(h.sources.length,0);h.stop();
});
test('native persistent rejection reloads once and refuses an absent encode route without rescue',async()=>{
 const h=harness();await flush();h.answers.push(h.response(master),h.response(media));h.advance(1000);await flush();
 const init=Buffer.from([0,0,0,16,102,116,121,112,105,115,111,109,0,0,0,0]);
 h.answers.push(h.response(master),h.response(media),h.response(init,200,'video/mp4'),h.response(master),h.response(media));
 await h.ctx.classifyNativeHlsError(h.video,h.player,4,'unsupported');await flush();
 assert.equal(h.player.hlsStartup.native.reloadUsed,true);assert.equal(h.sources.length,2);assert.equal(h.rescues.length,0);
 h.answers.push(h.response(master),h.response(media),h.response(init,200,'video/mp4'));
 await h.ctx.classifyNativeHlsError(h.video,h.player,4,'unsupported');await flush();
 assert.equal(h.sources.length,2);assert.equal(h.rescues.length,0);assert.equal(h.player.triedFallback,undefined);
 assert.match(h.player.hlsStartup.latestFailure.message,/No compatible encode route/);h.stop();
});
test('native readiness bounds an abort-ignoring fetch and malformed successful response',async()=>{
 const h=harness();await flush();h.answers.push(()=>new Promise(()=>{}));h.advance(1000);await flush();
 h.advance(12000);await flush();assert.equal(h.sources.length,0);assert.equal(h.player.hlsStartup.native.busy,false);
 h.answers.push(h.response('<html>not media</html>'));h.advance(2000);await flush();
 assert.equal(h.player.hlsStartup.state,'exhausted');assert.equal(h.sources.length,0);h.stop();
});

test('native readiness metadata ownership survives pause and is retired on cancellation',async()=>{
 const h=harness();await flush();h.answers.push(h.response(master),h.response(media));h.advance(1000);await flush();
 const metadata=h.listeners.get('loadedmetadata');assert.equal(typeof metadata,'function');
 h.ctx.pauseHlsStartup(h.player);assert.equal(h.listeners.get('loadedmetadata'),metadata);
 metadata();assert.equal(h.positions.length,1);assert.equal(h.listeners.has('loadedmetadata'),false);h.stop();
 const stale=harness();await flush();stale.answers.push(stale.response(master),stale.response(media));stale.advance(1000);await flush();
 const old=stale.listeners.get('loadedmetadata');stale.stop();assert.equal(stale.listeners.has('loadedmetadata'),false);
 old();assert.equal(stale.positions.length,0);
});
test('native persistent refusal admits one verified encode fallback with an unverified cause',async()=>{
 const h=harness();await flush();h.player.qualityCandidates=[{route:'encode',decoder_compatible:true,target_height:1080}];
 h.answers.push(h.response(master),h.response(media));h.advance(1000);await flush();
 const init=Buffer.from([0,0,0,16,102,116,121,112,105,115,111,109,0,0,0,0]);
 h.answers.push(h.response(master),h.response(media),h.response(init,206,'video/mp4'),h.response(master),h.response(media));
 await h.ctx.classifyNativeHlsError(h.video,h.player,4,'unsupported');await flush();
 h.answers.push(h.response(master),h.response(media),h.response(init,200,'video/mp4'));
 await h.ctx.classifyNativeHlsError(h.video,h.player,4,'unsupported');await flush();
 assert.equal(h.rescues.length,1);assert.match(h.rescues[0][1],/unverified, refused before decode/);
 assert.equal(h.player.triedFallback,true);assert.equal(h.sources.length,2);h.stop();
});

test('native attached startup expires at its fixed deadline without a new readiness fetch',async()=>{
 const h=harness();await flush();h.answers.push(h.response(master),h.response(media));h.advance(1000);await flush();
 assert.equal(h.sources.length,1);const requests=h.requests.length;
 h.advance(39001);await flush();assert.equal(h.player.hlsStartup.state,'exhausted');
 assert.equal(h.requests.length,requests);assert.equal(h.rescues.length,0);
 h.ctx.exhaustHlsStartup(h.player,'duplicate');h.advance(1);await flush();assert.equal(h.diagnoses.length,1);h.stop();
});

async function readyNativeHarness(){
 const h=harness();await flush();h.answers.push(h.response(master),h.response(media));
 h.advance(1000);await flush();assert.equal(h.sources.length,1);return h;
}
function retireNative(h){
 h.ctx.noteStreamFailure(503,JSON.stringify({code:'serving_fenced',message:'proof expired'}),{attachment:h.player.mediaAttachment});
}
test('shipped native media error gives known authority retirement precedence over codec readiness',async()=>{
 for(const code of [3,4]){
  const h=await readyNativeHarness(),requests=h.requests.length;retireNative(h);
  h.video.error={code};await h.listeners.get('error')();
  assert.equal(h.reopens.length,1);assert.equal(h.reopens[0][0],42);
  assert.equal(h.requests.length,requests);assert.equal(h.sources.length,1);
  assert.equal(h.rescues.length,0);assert.equal(h.player.hlsStartup.native.reloadUsed,false);h.stop();
 }
});
test('shipped native error rejoins authority retirement during playlist or init reads',async()=>{
 for(const stage of ['playlist','init','typed_refusal']){
  const h=await readyNativeHarness();let release;
  const held=()=>new Promise(resolve=>release=resolve);
  if(stage==='init')h.answers.push(h.response(master),h.response(media),held);
  else h.answers.push(held);
  h.video.error={code:4};const operation=h.listeners.get('error')();await flush();
  if(stage!=='typed_refusal')retireNative(h);
  release(stage==='init'?h.response(Buffer.from([0,0,0,16,102,116,121,112]),200,'video/mp4'):
   stage==='typed_refusal'?h.response(JSON.stringify({code:'serving_fenced',message:'proof expired'}),503,'application/json'):h.response(media));
  await operation;await flush();
  assert.equal(h.reopens.length,1,stage);assert.equal(h.sources.length,1,stage);
  assert.equal(h.rescues.length,0,stage);assert.equal(h.diagnoses.length,0,stage);
  assert.equal(h.player.hlsStartup.native.reloadUsed,false,stage);h.stop();
 }
});
test('native master and child authentication refusals keep the shipped Sign in surface',async()=>{
 for(const status of [401,403])for(const stage of ['master','child','classification']){
  const h=stage==='classification'?await readyNativeHarness():harness();await flush();
  const refused=h.response(JSON.stringify({message:'Please sign in again.'}),status,'application/json');
  if(stage==='master')h.answers.push(refused);
  else h.answers.push(h.response(master),refused);
  if(stage==='classification'){h.video.error={code:4};await h.listeners.get('error')();}
  else{h.advance(1000);await flush();}
  assert.equal(h.player.hlsStartup.state,'cancelled',stage);
  assert.equal(h.player.hlsStartup.cancelledReason,'authentication_refused');
  assert.equal(h.stops.length,1);assert.equal(h.surfaces.length,1);
  const fault=h.surfaces[0].state.faults[0];
  assert.equal(fault.source,'auth_401_403');assert.deepEqual([...fault.actions],['sign_in','close']);
  h.advance(50000);await flush();assert.equal(h.diagnoses.length,0);assert.equal(h.rescues.length,0);h.stop();
 }
});

test('late native authority or auth refusal cannot act on a superseded attachment',async()=>{
 for(const status of [401,503]){
  const h=await readyNativeHarness();let release;
  h.answers.push(()=>new Promise(resolve=>release=resolve));
  h.video.error={code:4};const operation=h.listeners.get('error')();await flush();
  h.ctx.current=false;
  release(h.response(JSON.stringify({code:'serving_fenced',message:'old refusal'}),status,'application/json'));
  await operation;await flush();
  assert.equal(h.reopens.length,0);assert.equal(h.surfaces.length,0);
  assert.equal(h.stops.length,0);assert.equal(h.rescues.length,0);
  assert.equal(h.player.sessionTerminal,undefined);assert.equal(h.sources.length,1);h.stop();
 }
});

test('native classification paused during readiness resumes under the original clock',async()=>{
 const h=await readyNativeHarness(),episode=h.player.hlsStartup,deadline=episode.deadlineMs;
 let release;h.answers.push(()=>new Promise(resolve=>release=resolve));
 h.video.error={code:4};const operation=h.listeners.get('error')();await flush();
 h.player.wantsPlayback=false;h.player.controlIntentGeneration++;h.ctx.pauseHlsStartup(h.player);
 await operation;assert.equal(episode.native.pendingError.code,4);
 assert.equal(h.sources.length,1);assert.equal(h.rescues.length,0);
 release(h.response(media));await flush();
 h.answers.push(h.response(master),h.response(media),h.response(Buffer.from([0,0,0,16,102,116,121,112]),200,'video/mp4'),h.response(master),h.response(media));
 h.player.wantsPlayback=true;h.player.controlIntentGeneration++;h.ctx.resumeHlsStartup(h.video,h.player);await flush();
 assert.equal(episode.deadlineMs,deadline);assert.equal(episode.native.reloadUsed,true);
 assert.equal(h.sources.length,2);assert.equal(h.rescues.length,0);h.stop();
});


test('native cold open and stream change start readiness after attachment ownership settles',async()=>{
 for(const pending of ['pendingOpen','pendingChange']){
  const h=harness({[pending]:true});await flush();
  const episode=h.player.hlsStartup,deadline=episode.deadlineMs;
  assert.equal(h.requests.length,0,'the preparing player must not fetch yet');
  h.answers.push(h.response(master),h.response(media));
  h.player.pendingOpenAttempt=null;h.player.pendingMediaChange=null;
  h.advance(0);await flush();
  assert.equal(h.requests.length,2,'the committed attachment must fetch its playlists');
  assert.equal(h.sources.length,1);assert.equal(episode.deadlineMs,deadline);h.stop();
 }
});

test('native readiness keeps a bounded wakeup while attachment ownership is pending',async()=>{
 const h=harness({pendingOpen:true});await flush();
 const deadline=h.player.hlsStartup.deadlineMs;
 h.advance(0);await flush();assert.equal(h.requests.length,0);
 h.player.pendingOpenAttempt=null;
 h.answers.push(h.response(master),h.response(media));
 h.advance(1000);await flush();
 assert.equal(h.sources.length,1);assert.equal(h.player.hlsStartup.deadlineMs,deadline);h.stop();
});

test('native deferred attachment cannot fetch after it is cancelled or superseded',async()=>{
 for(const stale of ['cancelled','superseded']){
  const h=harness({pendingChange:true});await flush();
  h.player.pendingMediaChange=null;
  if(stale==='cancelled')h.stop();else h.ctx.current=false;
  h.advance(1000);await flush();
  assert.equal(h.requests.length,0);assert.equal(h.sources.length,0);h.stop();
 }
});
