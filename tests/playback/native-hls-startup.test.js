"use strict";
const test=require('node:test'),assert=require('node:assert/strict'),vm=require('node:vm'),fs=require('node:fs'),path=require('node:path');
const source=fs.readFileSync(path.join(__dirname,'../../crates/plurxd/src/web/player/player.js'),'utf8');
const policy=require('../../crates/plurxd/src/web/playback-policy.js');
const master='#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=200000\nindex.m3u8\n';
const media='#EXTM3U\n#EXT-X-TARGETDURATION:16\n#EXT-X-MAP:URI="init.mp4"\n#EXTINF:8,\nseg_0000.m4s\n#EXTINF:8,\nseg_0001.m4s\n#EXTINF:8,\nseg_0002.m4s\n';
async function flush(){for(let i=0;i<16;i++)await new Promise(resolve=>setImmediate(resolve));}
function harness(){
 let now=0,ordinal=0;const timers=new Map(),answers=[],requests=[],sources=[],rescues=[],diagnoses=[],positions=[],listeners=new Map();
 const video={readyState:0,videoWidth:0,currentTime:0,paused:true,addEventListener(event,fn){listeners.set(event,fn);},removeEventListener(event,fn){if(listeners.get(event)===fn)listeners.delete(event);},removeAttribute(){},load(){},pause(){},play(){return Promise.resolve();}};
 const player={hls:null,wantsPlayback:true,method:'remux',mediaAttachment:{},controlIntentGeneration:0,qualityCandidates:[],abr:{}};
 const ctx=vm.createContext({PlaybackPolicy:policy,URL,TextDecoder,AbortController,Uint8Array,console,
  performance:{now:()=>now},location:{href:'http://fixture.invalid/'},player,video,
  setTimeout:(fn,ms)=>{const id=++ordinal;timers.set(id,{fn,at:now+ms});return id;},clearTimeout:id=>timers.delete(id),
  fetch:async(url,options)=>{requests.push({url,options});const answer=answers.shift();if(!answer)throw Error('No scripted answer');return typeof answer==='function'?answer(options):answer;},
  bufferTargets:()=>({}),parseSegTimes:text=>text.split('\n').filter(line=>line.startsWith('#EXTINF:')).map((_,i)=>(i+1)*8),
  setPlaybackMediaSource:(_,url)=>sources.push(url),applyPlaybackTransportIntent(){},applyPlaybackAttachmentPosition(){positions.push(true);},pausePlaybackInternally(){},resetPlaybackTransportEvents(){},
  playbackOwnsAttachedMedia:()=>true,qualityCatalogSelectionCurrent:()=>true,notifyPlaybackControl(){},clearStall(){},playbackContext:()=>({}),clientLog(){},tok:x=>x,
  startTranscodeFallback:(...args)=>rescues.push(args),stallDiagnose:()=>{diagnoses.push(true);return Promise.resolve();}});
 vm.runInContext(source,ctx);
 vm.runInContext("PLAYER=player;function clearStreamFailure(){STREAM_FAILURE=null;}function noteStreamFailure(status,body){let data={};try{data=JSON.parse(body)}catch(e){}return {status,...data};}",ctx);
 const response=(text,status=200,type='application/vnd.apple.mpegurl')=>new Response(text,{status,headers:{'Content-Type':type}});
 ctx.attachNativeHls(video,'http://fixture.invalid/session/master.m3u8?token=fixture',0,player,{current:()=>ctx.current!==false});
 return {ctx,player,video,answers,requests,sources,rescues,diagnoses,positions,listeners,response,
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
