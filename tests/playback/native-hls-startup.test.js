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
function harness({pendingOpen=false,pendingChange=false,realTransport=false}={}){
 let now=0,ordinal=0;const timers=new Map(),answers=[],requests=[],sources=[],rescues=[],diagnoses=[],positions=[],listeners=new Map(),reopens=[],surfaces=[],stops=[],logs=[];
 const listenerSets=new Map();
 const video={readyState:0,videoWidth:0,currentTime:0,paused:true,currentSrc:'/session/master.m3u8',getAttribute:()=>'/session/master.m3u8',
  addEventListener(event,fn){let callbacks=listenerSets.get(event);if(!callbacks){callbacks=new Set();listenerSets.set(event,callbacks);}callbacks.add(fn);listeners.set(event,(...args)=>Promise.all([...callbacks].map(callback=>callback(...args))));},
  removeEventListener(event,fn){const callbacks=listenerSets.get(event);callbacks?.delete(fn);if(!callbacks?.size)listeners.delete(event);},
  removeAttribute(){},load(){},pause(){},play(){return Promise.resolve();}};
 const player={hls:null,wantsPlayback:true,method:'remux',sessionId:'retired',stallRecoveries:0,mediaAttachment:{},controlIntentGeneration:0,qualityCandidates:[],abr:{}};
 if(pendingOpen) player.pendingOpenAttempt={};
 if(pendingChange) player.pendingMediaChange={};
 const ctx=vm.createContext({PLAY_OPEN_GATE:{current:()=>true},PlaybackPolicy:policy,URL,TextDecoder,AbortController,Uint8Array,console,
  document:{getElementById:id=>id==='video'?video:{classList:{remove(){}}}},performance:{now:()=>now},location:{href:'http://fixture.invalid/'},player,video,
  setTimeout:(fn,ms)=>{const id=++ordinal;timers.set(id,{fn,at:now+ms});return id;},clearTimeout:id=>timers.delete(id),
  fetch:async(url,options)=>{requests.push({url,options});const answer=answers.shift();if(!answer)throw Error('No scripted answer');return typeof answer==='function'?answer(options):answer;},
  bufferTargets:()=>({}),parseSegTimes:text=>text.split('\n').filter(line=>line.startsWith('#EXTINF:')).map((_,i)=>(i+1)*8),
  setPlaybackMediaSource:(_,url)=>sources.push(url),applyPlaybackTransportIntent(){},applyPlaybackAttachmentPosition(){positions.push(true);},pausePlaybackInternally(){},resetPlaybackTransportEvents(){},
  qualityCatalogSelectionCurrent:()=>true,notifyPlaybackControl(){},clearStall(){},playbackContext:()=>({}),clientLog:row=>logs.push(row),tok:x=>x,
  pbTick(){},pbSyncPlayIcon(){},endWait(){},positionForPlaybackIntent:()=>42,stallRecoverySnapshot:(_p,_v,facts)=>facts,seekTo:(...args)=>reopens.push(args),showStallRecoveryFailure(){throw Error('unexpected recovery exhaustion');},playbackIsReal:()=>false,playbackSurfaceSourceIsBlocking:()=>true,stopPlayerForExhaustion:()=>stops.push(true),raisePlaybackSurface:(source,facts)=>surfaces.push(policy.presentSurface(policy.initialSurfaceState(),{raise:source,...facts,now_ms:now})),toast(){},
  startTranscodeFallback:(...args)=>rescues.push(args),stallDiagnose:()=>{diagnoses.push(true);return Promise.resolve();}});
 vm.runInContext(source,ctx);
 vm.runInContext("PLAYER=player;function clearStreamFailure(){STREAM_FAILURE=null;}",ctx);
 vm.runInContext(shippedFunction("stall-diagnosis.js","hasPendingPlaybackOpen"),ctx);
 vm.runInContext(shippedFunction("stall-diagnosis.js","playbackOwnsAttachedMedia"),ctx);
 vm.runInContext(shippedFunction("measurements.js","recoverServingFencedAttachment"),ctx);
 vm.runInContext(shippedFunction("transport.js","wirePlayerMedia"),ctx);
 if(realTransport){
  Object.assign(ctx,{playerActivity(){},reportTtff(){},settlePlaybackControlSeek(){},samplePlaybackPresentationClock(){},samplePreparedSwitchFrames(){},
   playbackSurfaceStep(){},playbackSurfaceGeneration:()=>player.mediaAttachment,finishStallRecovery(){},streamHasVideo:()=>true,
   supersedePlaybackControlIntent:p=>++p.controlIntentGeneration});
  for(const name of ['resetPlaybackTransportEvents','playbackTransportEvents','playbackTransportRecord','logPlaybackTransportRecord',
   'queuePlaybackTransportCommand','playbackTransportMarker','prunePlaybackPlayInvocations','cancelPlaybackPlayInvocation','pausePlaybackInternally','rememberPlaybackTransportIntent',
   'applyPlaybackTransportIntent','handlePlaybackTransportEvent']) vm.runInContext(shippedFunction('decode-margin.js',name),ctx);
  for(const name of ['playerWantsPlayback','togglePlay','playbackProgressTick','playbackWaitNeedsProgress','handlePlaybackPlaying'])
   vm.runInContext(shippedFunction('transport.js',name),ctx);
  video.pause=()=>{video.paused=true;};
  video.play=()=>{video.paused=false;ctx.handlePlaybackTransportEvent(video,player,'play');return Promise.resolve();};
 }
 ctx.wirePlayerMedia(video);
 const response=(text,status=200,type='application/vnd.apple.mpegurl')=>new Response(text,{status,headers:{'Content-Type':type}});
 ctx.attachNativeHls(video,'http://fixture.invalid/session/master.m3u8?token=fixture',0,player,{current:()=>ctx.current!==false});
 if(!pendingOpen&&!pendingChange){for(const [id,t]of [...timers])if(t.at<=now){timers.delete(id);t.fn();}}
 return {ctx,player,video,answers,requests,sources,rescues,diagnoses,positions,listeners,response,reopens,surfaces,stops,logs,
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
function processedMoviePlaylist(entries=5915){
 return '#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI="init.mp4"\n'+
  Array.from({length:entries},(_,i)=>`#EXTINF:2.002000,\nseg${String(i).padStart(5,'0')}.m4s\n`).join('')+'#EXT-X-ENDLIST\n';
}
test('native readiness admits the complete processed movie timeline as child or scalar media',async()=>{
 const playlist=processedMoviePlaylist();assert.ok(Buffer.byteLength(playlist)>65536);
 for(const scalar of [false,true]){
  const h=harness();await flush();
  vm.runInContext(shippedFunction('decode-tiers.js','parseSegTimes'),h.ctx);
  h.answers.push(...(scalar?[h.response(playlist)]:[h.response(master),h.response(playlist)]));
  h.advance(1000);await flush();
  assert.equal(h.sources.length,1,`scalar=${scalar}`);
  assert.equal(h.player.segTimes.length,5915,'retain every boundary, not a truncated prefix');
  assert.equal(h.player.segTimes.at(-1),11841.83);
  assert.equal(h.player.hlsStartup.manifestState,'parsed');h.stop();
 }
});
test('native readiness keeps finite and distinct media, master, error and init body budgets',async()=>{
 const probe=async({kind,size,status=200})=>{
  const h=harness();await flush();let reads=0,cancels=0;
  const prefix=kind==='init'?Buffer.from([0,0,0,8,102,116,121,112]):Buffer.from(kind==='master'?master:media);
  const bytes=new Uint8Array(size);bytes.fill(32);bytes.set(prefix);
  h.answers.push({ok:status>=200&&status<300,status,url:'',headers:{get:()=>kind==='init'?'video/mp4':'application/vnd.apple.mpegurl'},
   body:{getReader:()=>({read:async()=>++reads===1?{done:false,value:bytes}:{done:true},cancel:async()=>{cancels++;}})}});
  try{return {value:await h.ctx.nativeHlsFetch(h.player,h.player.hlsStartup,'http://fixture.invalid/session/index.m3u8',{init:kind==='init'}),reads,cancels};}
  catch(error){return {error,reads,cancels};}finally{h.stop();}
 };
 const limitHarness=harness();await flush();
 const limits=vm.runInContext('NATIVE_HLS_BODY_LIMITS',limitHarness.ctx);limitHarness.stop();
 for(const [kind,limit]of [['media',limits.playlist],['master',limits.master],['init',limits.init]]){
  const accepted=await probe({kind,size:limit});assert.ok(accepted.value,kind);
  const refused=await probe({kind,size:limit+1});assert.equal(refused.error?.terminal,true,kind);
  assert.match(refused.error.message,/byte limit/);assert.ok(refused.cancels>=1,'the bounded reader is retired');
 }
 const refused=await probe({kind:'media',size:limits.error+1,status:503});
 assert.equal(refused.error?.terminal,true);assert.match(refused.error.message,/byte limit/);
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
test('unowned startup pause is an observation and the original native deadline still diagnoses',async()=>{
 const h=harness({realTransport:true});await flush();
 h.answers.push(h.response(master),h.response(media));h.advance(1000);await flush();
 const episode=h.player.hlsStartup,deadline=episode.deadlineMs,generation=h.player.controlIntentGeneration;
 assert.equal(h.video.paused,false,'the owned startup Play was dispatched');
 h.video.paused=true;h.listeners.get('pause')();h.listeners.get('pause')();
 assert.equal(h.player.wantsPlayback,true);assert.equal(h.player.controlIntentGeneration,generation);
 assert.equal(episode.state,'active');assert.equal(episode.deadlineMs,deadline);
 const observed=h.logs.filter(row=>row.transport?.reason==='native_observation_before_presentation');
 assert.equal(observed.length,2);assert.ok(observed.every(row=>row.transport.desired_after===true));
 h.advance(deadline+1);await flush();h.advance(0);await flush();
 assert.equal(episode.state,'exhausted');assert.equal(h.diagnoses.length,1);h.stop();
});
test('explicit Pause owns preparation and an unknown play cannot undo it',async()=>{
 const h=harness({realTransport:true});await flush();h.ctx.togglePlay();
 assert.equal(h.player.wantsPlayback,false);assert.equal(h.player.hlsStartup.state,'paused');
 const generation=h.player.controlIntentGeneration;
 h.video.paused=false;h.ctx.handlePlaybackTransportEvent(h.video,h.player,'play');
 assert.equal(h.player.wantsPlayback,false);assert.equal(h.player.controlIntentGeneration,generation);
 assert.equal(h.player.hlsStartup.state,'paused');h.stop();
});
test('native controls require presentation of this attachment and this element',async()=>{
 const h=harness({realTransport:true});await flush();
 h.answers.push(h.response(master),h.response(media));h.advance(1000);await flush();
 let frames=0;h.video.getVideoPlaybackQuality=()=>({totalVideoFrames:frames});
 h.ctx.handlePlaybackPlaying(h.video,h.player);h.ctx.playbackProgressTick(h.video,h.player,true);
 frames++;h.video.currentTime=0.1;h.ctx.playbackProgressTick(h.video,h.player,true);
 assert.equal(h.player.hlsStartup.state,'presenting');
 h.video.paused=true;h.ctx.handlePlaybackTransportEvent(h.video,h.player,'pause');
 assert.equal(h.player.wantsPlayback,false,'native/PiP Pause remains supported after actual presentation');
 h.video.paused=false;h.ctx.handlePlaybackTransportEvent(h.video,h.player,'play');
 assert.equal(h.player.wantsPlayback,true);
 const generation=h.player.controlIntentGeneration;
 h.ctx.handlePlaybackTransportEvent(h.video,h.player,'pause');
 assert.equal(h.player.controlIntentGeneration,generation,'a queued Pause superseded by Play is only an observation');
 assert.equal(h.logs.at(-1).transport.desired_after,true);
 assert.equal(h.logs.at(-1).transport.reason,'native_observation_superseded');
 h.ctx.handlePlaybackTransportEvent({...h.video,paused:true},h.player,'pause');
 assert.equal(h.player.wantsPlayback,true,'a retired element cannot borrow current proof');
 const lookup=h.ctx.document.getElementById,successor={...h.video};
 h.ctx.document.getElementById=id=>id==='video'?successor:lookup(id);
 h.video.paused=true;h.ctx.handlePlaybackTransportEvent(h.video,h.player,'pause');
 assert.equal(h.player.wantsPlayback,true,'the actually proven predecessor cannot change successor intent after a DOM swap');
 h.ctx.document.getElementById=lookup;
 h.player.mediaAttachment={};h.video.paused=true;h.ctx.handlePlaybackTransportEvent(h.video,h.player,'pause');
 assert.equal(h.player.wantsPlayback,true,'presentation from the predecessor cannot authorize a replacement Pause');h.stop();
});
test('play rejection retains original invocation identity and does not invent viewer Pause',async()=>{
 const h=harness({realTransport:true});await flush();
 h.player.attemptId='original';h.player.sessionId='original-session';
 let reject;h.video.play=()=>new Promise((_resolve,no)=>{reject=no;});
 h.ctx.applyPlaybackTransportIntent(h.video,h.player);
 reject(Object.assign(new Error('secret source URL'),{name:'NotSupportedError',code:0}));await flush();
 const failed=h.logs.find(row=>row.event==='transport_play_rejected');
 assert.equal(failed.level,'warn');assert.equal(failed.session_id,'original-session');
 assert.equal(h.player.wantsPlayback,true);
 assert.match(failed.detail,/^rejected:NotSupportedError:code=0:status=none$/,'the accepted persisted detail retains the cause');
 assert.equal(h.player.hlsStartup.state,'active');assert.equal(h.rescues.length,0);
 assert.ok(!JSON.stringify(failed).includes('secret source URL'));
 h.ctx.applyPlaybackTransportIntent(h.video,h.player);
 h.player.mediaAttachment={};h.player.sessionId='replacement';h.player.attemptId='incoming';
 reject(Object.assign(new Error('old abort'),{name:'AbortError'}));await flush();
 const late=h.logs.filter(row=>row.event==='transport_play_rejected').at(-1);
 assert.equal(late.level,'warn');assert.equal(late.session_id,'original-session');
 assert.equal(late.attempt,'original');assert.match(late.detail,/^late_rejection:AbortError:/);h.stop();
});
test('owned pause and source commands cancel only their exact pending play invocations',async()=>{
 for(const command of ['pause','reset','replace']){
  const h=harness({realTransport:true});await flush();h.player.sessionId='original-session';
  for(const name of ['resetMediaSource','setPlaybackMediaSource'])vm.runInContext(shippedFunction('decode-margin.js',name),h.ctx);
  const pending=[];h.video.play=()=>new Promise((resolve,reject)=>pending.push({resolve,reject}));
  h.ctx.applyPlaybackTransportIntent(h.video,h.player);h.ctx.applyPlaybackTransportIntent(h.video,h.player);
  if(command==='pause')h.ctx.pausePlaybackInternally(h.video,'viewer_pause');
  else if(command==='reset')h.ctx.resetMediaSource(h.video);
  else h.ctx.setPlaybackMediaSource(h.video,'/replacement.m3u8');
  assert.equal(h.player.transportPlayInvocation,null,'owned cancellation releases its pending invocation chain');
  for(const request of pending)request.reject(Object.assign(new Error('cancelled'),{name:'AbortError'}));
  await flush();
  const cancelled=h.logs.filter(row=>row.event==='transport_play_cancelled');
  assert.equal(cancelled.length,2,`${command} proves cancellation of both outstanding Play calls`);
  assert.ok(cancelled.every(row=>row.level==='info'&&row.session_id==='original-session'&&/^cancelled:.*:AbortError:/.test(row.detail)));
  assert.equal(h.logs.some(row=>row.event==='transport_play_rejected'),false);
  assert.equal(h.player.hlsStartup.playRejection,null);h.stop();
 }
 const h=harness({realTransport:true});await flush();let reject;
 h.video.play=()=>new Promise((_resolve,no)=>{reject=no;});h.ctx.applyPlaybackTransportIntent(h.video,h.player);
 h.ctx.pausePlaybackInternally(h.video,'viewer_pause');
 reject(Object.assign(new Error('actual failure'),{name:'NotSupportedError'}));await flush();
 assert.equal(h.logs.at(-1).level,'warn','owned cancellation never hides a non-cancellation browser failure');h.stop();
});
test('direct outgoing teardown cancels Play before source replacement loses its owner',async()=>{
 const h=harness({realTransport:true});await flush();h.player.method='direct';h.player.hls=null;
 h.player.sessionId='outgoing-direct';h.player.attemptId='outgoing-attempt';
 Object.assign(h.ctx,{abandonPreparedReplacement(){},supersedeDirectedChange(){},cancelPreparedFirstFrame(){}});
 for(const name of ['teardownHls','setPlaybackMediaSource'])vm.runInContext(shippedFunction('decode-margin.js',name),h.ctx);
 let reject;h.video.play=()=>new Promise((_resolve,no)=>{reject=no;});h.ctx.applyPlaybackTransportIntent(h.video,h.player);
 h.ctx.teardownHls();assert.equal(h.video.paused,true,'retirement issues a real owned pause for direct media too');
 vm.runInContext("PLAYER={sessionId:'incoming',mediaAttachment:{},wantsPlayback:true}",h.ctx);
 h.ctx.setPlaybackMediaSource(h.video,'/incoming.mp4');
 reject(Object.assign(new Error('old command aborted'),{name:'AbortError'}));await flush();
 const cancelled=h.logs.find(row=>row.event==='transport_play_cancelled');
 assert.equal(cancelled.session_id,'outgoing-direct');assert.equal(cancelled.attempt,'outgoing-attempt');
 assert.match(cancelled.detail,/^cancelled:source_teardown:AbortError:/);
 assert.equal(h.logs.some(row=>row.event==='transport_play_rejected'),false);h.stop();
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
 metadata();assert.equal(h.positions.length,1);assert.equal(h.player.hlsStartup.native.metadataListener,null);h.stop();
 const stale=harness();await flush();stale.answers.push(stale.response(master),stale.response(media));stale.advance(1000);await flush();
 const old=stale.listeners.get('loadedmetadata');stale.stop();assert.equal(stale.player.hlsStartup.native.metadataListener,null);
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

// ---------------------------------------------------------------------------
// Startup lab (A-M0 of docs/streaming/STARTUP-AND-TONE-MAP-BUILD-HANDOFF.md):
// scripts/playback-startup-lab and the fixture's arm mode. The advertised
// target and the bootstrap are separate parameters; the summariser's decision
// may only name B = 32 000 when the 8 / 32 arm itself passed.
const {execFileSync,spawn}=require('node:child_process'),os=require('node:os');
const LAB=path.join(__dirname,'../../scripts/playback-startup-lab');
const FIXTURE=path.join(__dirname,'native-hls-browser-fixture.py');
function lab(args){
 try{return {status:0,stdout:execFileSync('python3',[LAB,...args],{encoding:'utf8',stdio:['ignore','pipe','pipe']})};}
 catch(error){return {status:error.status,stdout:String(error.stdout||''),stderr:String(error.stderr||'')};}
}
function plan(...args){const run=lab(['--plan',...args]);assert.equal(run.status,0,run.stderr);return JSON.parse(run.stdout);}
function scratch(t){const dir=fs.mkdtempSync(path.join(os.tmpdir(),'startup-lab-'));t.after(()=>fs.rmSync(dir,{recursive:true,force:true}));return dir;}
function fakeMedia(dir,segments=120){
 fs.mkdirSync(dir,{recursive:true});fs.writeFileSync(path.join(dir,'init.mp4'),'init');
 for(let i=0;i<segments;i++)fs.writeFileSync(path.join(dir,`seg${String(i).padStart(5,'0')}.m4s`),'seg');
 fs.writeFileSync(path.join(dir,'lab-media.json'),JSON.stringify({segment_seconds:2,segments,durations:Array(segments).fill(2)}));
 return dir;
}
function serve(t,command,args){
 return new Promise((resolve,reject)=>{
  const child=spawn(command,args,{stdio:['ignore','pipe','pipe']});t.after(()=>child.kill());
  let text='';child.stdout.on('data',chunk=>{text+=chunk;const port=/127\.0\.0\.1:(\d+)/.exec(text);if(port)resolve(`http://127.0.0.1:${port[1]}`);});
  child.stderr.on('data',chunk=>{text+=chunk;});child.on('exit',code=>reject(Error(`exited ${code}: ${text}`)));
 });
}
const tagOf=text=>Number(/#EXT-X-TARGETDURATION:(\d+)/.exec(text)[1]);
const extinfs=text=>text.split('\n').filter(line=>line.startsWith('#EXTINF:')).map(line=>parseFloat(line.slice(8)));

test('startup lab separates the advertised target from the bootstrap for every arm',()=>{
 for(const [target,bootstrap] of [[16,48],[8,24],[8,32],[6,18],[4,12],[10,30]]){
  const result=plan('--target',String(target),'--bootstrap',String(bootstrap),'--rate','1','--ticks','4');
  assert.equal(tagOf(result.first_playlist),target);
  assert.deepEqual(extinfs(result.first_playlist),Array(bootstrap/2).fill(2));
  assert.match(result.first_playlist,/#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-START:TIME-OFFSET=0\n/);
  assert.doesNotMatch(result.first_playlist,/ENDLIST|PLAYLIST-TYPE/);
  assert.equal(result.snapshots[0].end_s,bootstrap);
  // Publication is due once per target, and 1x production adds one target of media.
  assert.equal(result.snapshots[1].at_s-result.snapshots[0].at_s,target);
  assert.equal(result.snapshots[1].end_s,bootstrap+target);
 }
 // Same target, different bootstrap: same tag, different first snapshot.
 const a=plan('--arm','a','--rate','1.05'),b=plan('--arm','b','--rate','1.05');
 assert.equal(tagOf(a.first_playlist),tagOf(b.first_playlist));
 assert.deepEqual([a.snapshots[0].end_s,b.snapshots[0].end_s],[24,32]);
 assert.deepEqual([a.arm,b.arm,plan('--target','10','--bootstrap','30').arm],['a','b','t10-b30']);
});

test('startup lab validates any pair by rule rather than a whitelist',()=>{
 const refusals=[
  [['--target','1','--bootstrap','12'],/shorter than the 2(\.0)? s segment/],
  [['--target','21','--bootstrap','48'],/above the 20 s bound/],
  [['--target','8','--bootstrap','14'],/under 2 x target \(16 s\)/],
  [['--target','8','--bootstrap','126'],/reserve maximum/],
  [['--target','8','--bootstrap','25'],/whole number of 2(\.0)? s segments/],
  [['--target','8.5','--bootstrap','24'],/whole number of seconds/],
  [['--target','8','--bootstrap','24','--rate','9'],/outside/],
  [['--target','8','--bootstrap','24','--rate','0.1'],/outside/],
 ];
 for(const [args,message] of refusals){const run=lab(['--plan',...args]);assert.equal(run.status,2,args.join(' '));assert.match(run.stderr,message);}
 for(const [target,bootstrap] of [[2,4],[8,16],[12,124],[20,40]])assert.equal(plan('--target',String(target),'--bootstrap',String(bootstrap)).target,target);
});

test('startup lab publishes at least one segment per cycle, caps the published lead and slides behind the viewer',()=>{
 for(const rate of ['1','1.05','2']){
  const result=plan('--arm','a','--rate',rate,'--ticks','120','--position-lag','3');
  for(let i=1;i<result.snapshots.length;i++){
   const [before,after]=[result.snapshots[i-1],result.snapshots[i]];
   assert.ok(after.count>before.count,`tick ${i} at ${rate}x published nothing`);
   assert.ok(after.media_sequence>=before.media_sequence);
   assert.ok(after.lead_s<=48+2,`lead ${after.lead_s} at ${rate}x exceeds the steady reserve`);
  }
  const last=result.snapshots.at(-1);
  assert.ok(last.media_sequence>0&&last.position_s-2*last.media_sequence<=30+2,'removed segments trail the viewer by about 30 s');
 }
});

test('startup lab server serves the arm, refuses unpublished media and records a receipt',async t=>{
 const dir=scratch(t),evidence=path.join(dir,'evidence');
 const base=await serve(t,'python3',[LAB,'--media-dir',fakeMedia(path.join(dir,'media')),'--evidence-dir',evidence,'--port','0','--setup','0','--burst','1000']);
 const refused=await fetch(`${base}/start?target=8&bootstrap=12&transport=hlsjs`);
 assert.equal(refused.status,400);assert.match((await refused.json()).error,/under 2 x target/);
 const {id,playlist}=await (await fetch(`${base}/start?arm=a&rate=2&transport=hlsjs`,{headers:{'user-agent':'lab-test'}})).json();
 const text=await (await fetch(base+playlist)).text();
 assert.equal(tagOf(text),8);assert.deepEqual(extinfs(text),Array(12).fill(2));
 assert.match(text,new RegExp(`#EXT-X-MAP:URI="/media/${id}/init.mp4"`));
 assert.equal((await fetch(`${base}/media/${id}/seg00011.m4s`)).status,200);
 assert.equal((await fetch(`${base}/media/${id}/seg00012.m4s`)).status,404);
 assert.equal((await fetch(`${base}/report/${id}`,{method:'POST',body:JSON.stringify({event:'first_frame',client_ms:900,position:0})})).status,200);
 const rows=await (await fetch(`${base}/runs`)).json();
 assert.equal(rows.length,1);assert.equal(rows[0].player,'hlsjs');assert.equal(rows[0].metrics.first_frame,true);
 let receipt;
 for(let i=0;i<40&&!receipt;i++){await new Promise(r=>setTimeout(r,100));try{receipt=JSON.parse(fs.readFileSync(path.join(evidence,`${id}.json`),'utf8'));if(!receipt.client.length)receipt=null;}catch{}}
 assert.ok(receipt,'receipt written');
 assert.deepEqual([receipt.target,receipt.bootstrap,receipt.rate,receipt.transport,receipt.user_agent],[8,24,2,'hlsjs','lab-test']);
 assert.equal(receipt.reloads.length,1);
 assert.deepEqual(Object.keys(receipt.reloads[0]).sort(),['changed','end_s','ended','interval_ms','media_sequence','segments','server_ms','since_first_playlist_ms','tick']);
 assert.deepEqual([receipt.reloads[0].media_sequence,receipt.reloads[0].segments,receipt.reloads[0].end_s],[0,12,24]);
 assert.equal(receipt.model.publication_cadence_s,8);assert.equal(receipt.rule.deficit_margin_s,2);
 assert.match(receipt.rule.clean_rule,/zero buffer deficits/);
 assert.equal(receipt.segment_requests.refused,1);
});

const SAFARI='Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15';
const CHROME='Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36';
let receiptOrdinal=0;
function receipt(dir,{target,bootstrap,rate,transport,ua,defect=null,seconds=60}){
 const client=[{event:'first_frame',client_ms:1000,position:0}];
 let position=0;
 for(let ms=2000;ms<=(seconds+2)*1000;ms+=1000){
  position+=1;
  if(defect==='jump'&&ms===30000)position+=5;
  client.push({event:'sample',client_ms:ms,position,paused:false,ended:false,seeking:false,rate:1,ahead:defect==='deficit'&&ms===40000?1.0:20});
 }
 if(defect==='stall')client.push({event:'waiting',client_ms:30500,position:29.5});
 if(defect==='pause')client.push({event:'pause',client_ms:30500,position:29.5});
 if(defect==='fatal')client.push({event:'hls_error',client_ms:30500,fatal:true,type:'mediaError',details:'bufferAppendError'});
 client.push({event:'run_complete',client_ms:(seconds+2)*1000,position});
 for(const event of client)event.server_ms=event.client_ms+50;
 const id=`run${++receiptOrdinal}`;
 fs.writeFileSync(path.join(dir,`${id}.json`),JSON.stringify({schema:'plurx.startup-lab.run.v2',id,target,bootstrap,rate,transport,
  user_agent:ua||(transport==='native'?SAFARI:CHROME),device:'',run_seconds:seconds,first_playlist_served_ms:500,endlist_served_ms:null,reloads:[],client}));
}
function matrix(dir,defects={},seconds=60){
 for(const [arm,target,bootstrap] of [['a',8,24],['b',8,32]])
  for(const transport of ['native','hlsjs'])for(const rate of [1.05,1,2]){
   const key=`${arm}/${transport}/${rate}`;
   if(defects[key]==='missing')continue;
   receipt(dir,{target,bootstrap,rate,transport,defect:defects[key]||null,seconds});
  }
}
function decide(dir){
 const run=lab(['--summarize',dir,'--min-seconds','60']);
 return {...run,result:JSON.parse(fs.readFileSync(path.join(dir,'decision.json'),'utf8'))};
}

test('startup lab summariser decides B = 24 000 only when 8 / 24 is clean everywhere',t=>{
 const dir=scratch(t);matrix(dir);receipt(dir,{target:16,bootstrap:48,rate:1,transport:'native',defect:'stall'});
 const {status,result}=decide(dir);
 assert.equal(status,2);assert.equal(result.provisional_decision,'8 / 24 clean at every rate → B = 24 000');assert.equal(result.provisional_bootstrap_ms,24000);
 assert.equal(result.missing_cells.length,0);assert.equal(result.required_cells.length,12);
 assert.equal(result.acceptance_grade,false);assert.match(result.clean_rule,/under 2(\.0)? s while the served playlist has no ENDLIST/);
 const control=result.cells.find(c=>c.arm==='control');assert.equal(control.status,'not clean','other arms are reported, not decided on');
 const clean=result.cells.find(c=>c.arm==='a'&&c.player==='safari-native'&&c.rate===1.05);
 assert.equal(clean.status,'clean');assert.equal(clean.runs[0].ttff_after_first_playlist_ms,550);
});

test('startup lab summariser names B = 32 000 only when 8 / 32 itself is clean everywhere',t=>{
 for(const defect of ['stall','jump','deficit','fatal']){
  const dir=scratch(t);matrix(dir,{'a/native/1.05':defect});
  const {result}=decide(dir);
  assert.equal(result.provisional_decision,'8 / 24 not clean, 8 / 32 clean at every rate → B = 32 000',defect);
  assert.equal(result.provisional_bootstrap_ms,32000);
  assert.equal(result.cells.find(c=>c.arm==='a'&&c.player==='safari-native'&&c.rate===1.05).status,'not clean');
 }
 const dir=scratch(t);matrix(dir,{'a/hlsjs/2':'stall','b/native/1':'jump'});
 const {status,result}=decide(dir);
 assert.equal(status,2);assert.equal(result.provisional_decision,'neither clean → Part A is not built');assert.equal(result.provisional_bootstrap_ms,null);
});

test('startup lab summariser names B only from acceptance-grade runs',t=>{
 // Sixty-second runs judged at sixty seconds: what they would decide is
 // provisional, and neither the decision nor B is written.
 const short=scratch(t);matrix(short);
 let {status,result}=decide(short);
 assert.equal(status,2);assert.equal(result.acceptance_grade,false);
 assert.equal(result.decision,null);assert.equal(result.bootstrap_ms,null);
 assert.equal(result.provisional_bootstrap_ms,24000);assert.match(result.refusal,/not acceptance grade/);
 // Full-length runs judged at the default length decide.
 const full=scratch(t);matrix(full,{},1800);
 const run=lab(['--summarize',full]);
 result=JSON.parse(fs.readFileSync(path.join(full,'decision.json'),'utf8'));
 assert.equal(run.status,0);assert.equal(result.acceptance_grade,true);
 assert.equal(result.decision,'8 / 24 clean at every rate → B = 24 000');assert.equal(result.bootstrap_ms,24000);
});

test('startup lab summariser refuses to decide while a target-8 cell is missing',t=>{
 // "Arm a failed, so use 32" is not a result: 8 / 32 has to have run everywhere.
 const failedA=scratch(t);matrix(failedA,{'a/native/1':'stall','b/hlsjs/2':'missing'});
 let {status,result}=decide(failedA);
 assert.equal(status,2);assert.equal(result.provisional_decision,null);assert.equal(result.provisional_bootstrap_ms,null);
 assert.deepEqual(result.missing_cells,['8 / 32 hlsjs 2x']);assert.match(result.refusal,/Missing: 8 \/ 32 hlsjs 2x$/);
 // A clean 8 / 24 does not excuse a missing 8 / 32 either.
 const onlyA=scratch(t);matrix(onlyA,Object.fromEntries(['native','hlsjs'].flatMap(p=>[1.05,1,2].map(r=>[`b/${p}/${r}`,'missing']))));
 ({status,result}=decide(onlyA));
 assert.equal(status,2);assert.equal(result.provisional_decision,null);assert.equal(result.missing_cells.length,6);
 // A paused run is not counted, and native playback outside Safari is not Safari native.
 const paused=scratch(t);matrix(paused,{'a/native/2':'pause'});
 ({result}=decide(paused));
 assert.deepEqual(result.missing_cells,['8 / 24 safari-native 2x']);
 const chrome=scratch(t);matrix(chrome,{'b/native/1.05':'missing'});receipt(chrome,{target:8,bootstrap:32,rate:1.05,transport:'native',ua:CHROME});
 ({result}=decide(chrome));
 assert.deepEqual(result.missing_cells,['8 / 32 safari-native 1.05x']);
 assert.ok(result.cells.some(c=>c.player==='native-other'));
 // Runs shorter than the required length do not fill a cell.
 const short=scratch(t);matrix(short);
 assert.equal(lab(['--summarize',short]).status,2);
 assert.equal(JSON.parse(fs.readFileSync(path.join(short,'decision.json'),'utf8')).missing_cells.length,12);
});

test('native fixture keeps its 16 s sweep by default and serves a lab arm when given one',async t=>{
 const dir=scratch(t),media=fakeMedia(path.join(dir,'media'));
 for(let i=0;i<4;i++)fs.writeFileSync(path.join(media,`seg_${String(i).padStart(4,'0')}.m4s`),'seg');
 const begin=(base,run,threshold)=>fetch(`${base}/begin`,{method:'POST',body:JSON.stringify({run,threshold})});
 const sweep=await serve(t,'python3',[FIXTURE,'--media',media,'--receipt',path.join(dir,'sweep.json'),'--port','0']);
 await begin(sweep,'1',8);
 const old=await (await fetch(`${sweep}/qual/1/index.m3u8`)).text();
 assert.equal(tagOf(old),16);assert.ok(extinfs(old).length>=1&&extinfs(old).every(d=>d===8));
 const armed=await serve(t,'python3',[FIXTURE,'--media',media,'--receipt',path.join(dir,'arm.json'),'--port','0',
  '--target','8','--bootstrap','32','--rate','1.05','--setup','0','--burst','1000']);
 assert.match(await (await fetch(armed)).text(),/const fixtureArm=\{"target": 8, "bootstrap": 32, "rate": 1.05, "name": "b"\}/);
 await begin(armed,'1',32);
 const text=await (await fetch(`${armed}/qual/1/index.m3u8`)).text();
 assert.equal(tagOf(text),8);assert.deepEqual(extinfs(text),Array(16).fill(2));
 assert.equal((await fetch(`${armed}/qual/1/seg00015.m4s`)).status,200);
 assert.equal((await fetch(`${armed}/qual/1/seg00016.m4s`)).status,404);
 const refused=spawn('python3',[FIXTURE,'--media',media,'--receipt',path.join(dir,'x.json'),'--port','0','--target','8','--bootstrap','12','--rate','1']);
 const timer=setTimeout(()=>refused.kill(),5000);
 const [code]=await new Promise(resolve=>refused.on('exit',(...args)=>resolve(args)));clearTimeout(timer);
 assert.equal(code,2,'an invalid arm must be refused, not served');
});

// These cases retain the real terminal owner, not just the logging seam above.
function installPermissionOwner(h){
 Object.assign(h.ctx,{clearEffectiveProcessing(){},finishPlaybackSeekTelemetry(){},
  stopPlaybackControl(){h.stops.push('control');},clearPendingSeekTimer(){},clearInterval(){}});
 for(const [file,names] of [
  ['surface.js',['retireHlsTerminalAttempt','stopPlayerForExhaustion']],
  ['decode-margin.js',['stopPlayerTimers']],
 ])for(const name of names)vm.runInContext(shippedFunction(file,name),h.ctx);
}
test('native permission refusal stops recovery immediately and waits for a viewer gesture',async()=>{
 const h=harness({realTransport:true});installPermissionOwner(h);await flush();
 h.video.play=()=>Promise.reject(Object.assign(new Error('private URL'),{name:'NotAllowedError'}));
 h.answers.push(h.response(master),h.response(media));h.advance(1000);await flush();
 assert.equal(h.sources.length,1,'the actual readiness owner attached before Play was refused');
 assert.equal(h.player.wantsPlayback,true,'browser policy never becomes a viewer Pause');
 assert.equal(h.player.playPermissionDenied.element,h.video);
 assert.equal(h.player.terminalStop.attachment,h.player.mediaAttachment);
 assert.equal(h.player.hlsStartup.state,'cancelled');
 assert.equal(h.player.hlsStartup.native.metadataListener,null);
 assert.equal(h.player.samplingStopped,true);
 assert.equal(h.stops.length,1,'the control owner stops exactly once');
 const surface=h.surfaces.at(-1).surface;
 assert.equal(surface.kind,'blocking');assert.equal(surface.fault.source,'owner_stopped');
 assert.deepEqual(Array.from(surface.actions),['retry','close']);
 assert.match(surface.title,/permission/);
 assert.equal(h.ctx.playbackOwnsAttachedMedia(h.player),false);
 h.ctx.applyPlaybackTransportIntent(h.video,h.player);await flush();
 h.advance(90000);await flush();h.advance(0);await flush();
 assert.equal(h.logs.filter(row=>row.event==='transport_play_rejected').length,1);
 assert.equal(h.requests.length,3,'no permission retry is spent on playlist requests');
 assert.equal(h.reopens.length,0);assert.equal(h.rescues.length,0);assert.equal(h.diagnoses.length,0);
});
test('permission refusal is attachment fenced and also stops progressive playback',async()=>{
 for(const change of ['attachment','element','intent','pause','source','cancelled','none']){
  const h=harness({realTransport:true});installPermissionOwner(h);await flush();
  h.ctx.cancelHlsStartup(h.player,'progressive');h.player.hlsStartup=null;
  let reject;h.video.play=()=>new Promise((_yes,no)=>{reject=no;});
  h.ctx.applyPlaybackTransportIntent(h.video,h.player);
  if(change==='attachment')h.player.mediaAttachment={};
  if(change==='element')h.ctx.document.getElementById=()=>({});
  if(change==='intent')h.player.controlIntentGeneration++;
  if(change==='pause')h.player.wantsPlayback=false;
  if(change==='source')h.video.getAttribute=()=>'/different.mp4';
  if(change==='cancelled')h.ctx.cancelPlaybackPlayInvocation(h.video,'source_teardown');
  reject(Object.assign(new Error('refused'),{name:'NotAllowedError'}));await flush();
  assert.equal(!!h.player.playPermissionDenied,change==='none',change);
  assert.equal(h.stops.length,change==='none'?1:0,change);
  assert.equal(h.logs.filter(row=>row.event==='transport_play_rejected').length,1,'every refusal remains observable');
 }
});

test('explicit transport after permission refusal retries Play without turning Pause into a retry',async()=>{
 const h=harness({realTransport:true});installPermissionOwner(h);await flush();
 h.video.play=()=>Promise.reject(Object.assign(new Error('denied'),{name:'NotAllowedError'}));
 h.ctx.applyPlaybackTransportIntent(h.video,h.player);await flush();
 const actions=[];h.ctx.playbackSurfaceAction=action=>actions.push(action);
 vm.runInContext(shippedFunction('transport.js','setPlayerPlaying'),h.ctx);
 h.ctx.setPlayerPlaying(false);assert.equal(h.player.wantsPlayback,false);assert.equal(actions.length,0);
 h.ctx.setPlayerPlaying(true);assert.equal(h.player.wantsPlayback,true);assert.deepEqual(actions,['retry']);
 h.stop();
});

test('presentation reevaluation permission rejection fences the already pending recovery ask',async()=>{
 const h=harness({realTransport:true});installPermissionOwner(h);await flush();
 h.player.started=true;h.player.waitAt=1;h.player.waitStartedRunway=5.5;h.video.paused=false;
 h.ctx.performance.now=()=>8001;
 let answer;
 Object.assign(h.ctx,{SUPPLY_RUNWAY_SECS:1.5,CONTROL_STALL_DEFER_DEADLINE_MS:20000,
  bufferRunway:()=>5.5,pbPosSec:()=>42,recordWaitStall(){},armedPlaybackControlVerdict:()=>null,
  askPlaybackControl:()=>({trigger:'ask',action:new Promise(resolve=>{answer=resolve;})}),
  endWait:()=>{h.player.waitAt=null;}});
 vm.runInContext(shippedFunction('measurements.js','persistentWaitEvidence'),h.ctx);
 vm.runInContext('async '+shippedFunction('measurements.js','persistentWait'),h.ctx);
 h.video.play=()=>Promise.reject(Object.assign(new Error('denied'),{name:'NotAllowedError'}));
 const pending=h.ctx.persistentWait(h.video,h.player,1,0,0);await flush();
 assert.ok(h.player.playPermissionDenied,'reevaluation uses the central rejection owner');
 answer(null);await pending;
 assert.equal(h.reopens.length,0);assert.equal(h.rescues.length,0);
 assert.equal(h.player.hlsStartup.state,'cancelled');assert.equal(h.stops.length,1);
});

test('refilled supply wait permission rejection stops before the same-turn recovery verdict',async()=>{
 for(const rejection of ['throw','promise']){
  const h=harness({realTransport:true});installPermissionOwner(h);await flush();
  h.player.started=true;h.player.waitAt=1;h.player.waitStartedRunway=0;h.video.paused=false;
  let answer,runway=0,now=8001,plays=0;
  h.ctx.performance.now=()=>now;
  Object.assign(h.ctx,{SUPPLY_RUNWAY_SECS:1.5,CONTROL_STALL_DEFER_DEADLINE_MS:20000,
   bufferRunway:()=>runway,pbPosSec:()=>42,recordWaitStall(){},armedPlaybackControlVerdict:()=>null,
   askPlaybackControl:()=>({trigger:'ask',action:new Promise(resolve=>{answer=resolve;})}),
   controlVerdictText:text=>text,playbackStallActions:()=>['retry','close'],
   endWait:()=>{h.player.waitAt=null;}});
  vm.runInContext(shippedFunction('measurements.js','persistentWaitEvidence'),h.ctx);
  vm.runInContext('async '+shippedFunction('measurements.js','persistentWait'),h.ctx);
  h.video.play=()=>{
   plays++;const error=Object.assign(new Error('denied'),{name:'NotAllowedError'});
   if(rejection==='throw')throw error;
   // Browser play() returns a promise from the media owner's realm.
   return vm.runInContext("Promise.reject(Object.assign(new Error('denied'),{name:'NotAllowedError'}))",h.ctx);
  };
  const pending=h.ctx.persistentWait(h.video,h.player,1,0,0);await flush();
  assert.equal(plays,0,'the initial empty buffer is a supply wait');
  runway=5.5;now=25001;
  answer({type:'terminal',code:'recipe_failed',message:'The producer recipe failed.'});await pending;
  assert.equal(plays,1,'refill receives exactly one central transport reevaluation');
  assert.ok(h.player.playPermissionDenied,rejection);
  assert.equal(h.stops.length,1,'the permission owner alone stops the attachment');
  assert.equal(h.surfaces.length,1,'the pending verdict cannot replace the permission prompt');
  assert.match(h.surfaces[0].surface.title,/permission/);
  assert.equal(h.reopens.length,0);assert.equal(h.rescues.length,0);
  assert.equal(h.player.hlsStartup.state,'cancelled');
 }
});
