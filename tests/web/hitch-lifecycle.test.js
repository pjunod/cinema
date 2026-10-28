'use strict';
const {test}=require('node:test'),assert=require('node:assert/strict'),vm=require('node:vm'),fs=require('node:fs');
const source=fs.readFileSync('crates/plurxd/src/web/player/decode-tiers.js','utf8');
const transportSource=fs.readFileSync('crates/plurxd/src/web/player/transport.js','utf8');
const PlaybackPolicy=require('../../crates/plurxd/src/web/playback-policy.js');
const start=source.indexOf('function armHitchDetector('),end=source.indexOf('\n// The spacing between hitches',start);
const boundary=source.indexOf('const HITCH_LIFECYCLE_BOUNDARIES=');
const setup=boundary>=0&&boundary<start?source.slice(boundary,start):'';
assert.ok(start>=0&&end>start,'shipped detector is present');
function detector(){
 const listeners=new Map();let pending,clock=0;
 const video={paused:false,seeking:false,currentTime:0,playbackRate:1,dataset:{},
  requestVideoFrameCallback(){},addEventListener(name,fn){if(!listeners.has(name))listeners.set(name,new Set());listeners.get(name).add(fn);},
  removeEventListener(name,fn){listeners.get(name)?.delete(fn);}};
 const player={};
 const context=vm.createContext({PLAYER:player,document:{getElementById:()=>video},performance:{now:()=>clock},
  PLAYBACK_LIFETIME_HITCHES:0,HITCH_WARMUP:12,HITCH_WINDOW:120,HITCH_NEAR_MS:100,
  HITCH_GAP_FRAMES:2,HITCH_LATE_FLOOR_MS:100,HITCH_LATE_FRACTION:2,HITCH_SLOW_FACTOR:3,
  PlaybackPolicy,playbackOwnsAttachedMedia:()=>true,
  settlePlaybackControlSeek(){},sampleStartupPresentation(){},reportRateChase(){},
  queuePlaybackFrame(_v,_p,fn){pending=fn;}});
 vm.runInContext(setup+source.slice(start,end),context);context.armHitchDetector(video);
 const frame=(media,wall,presented,mediaTime=media)=>{clock=wall;video.currentTime=media;
  pending(wall,{mediaTime,presentedFrames:presented,expectedDisplayTime:wall},0,{});};
 const event=name=>{for(const fn of [...(listeners.get(name)||[])])fn();};
 return {video,player,context,frame,event,listeners,setNow(value){clock=value;}};
}
test('pause without frame callbacks resets resume timing and preserves genuine faults',()=>{
 const d=detector();
 for(let i=0;i<=15;i++)d.frame(i/24,i*1000/24,i+1);
 d.frame(15/24,16*1000/24,17); // an ordinary duplicate remains a held fault
 assert.equal(d.player.hitches.held,1);
 const lifetime=d.context.PLAYBACK_LIFETIME_HITCHES;
 d.player.hitches.rate=.8;d.player.hitches.renderedFps=19;
 d.video.paused=true;d.event('pause');
 assert.equal(d.player.hitches.rate,null);
 // No frame callback occurs anywhere in the eight-second intentional pause.
 d.video.paused=false;d.event('play');
 d.frame(16/24,8000+17*1000/24,18);
 assert.equal(d.player.hitches.held,1);
 assert.equal(d.player.hitches.late,0);
 assert.equal(d.context.PLAYBACK_LIFETIME_HITCHES,lifetime);
 assert.equal(d.player.hitches.rate,null,'paused wall time does not enter resumed rate');
 d.frame(16/24,8000+18*1000/24,19);
 assert.equal(d.player.hitches.held,2,'an unrelated resumed duplicate is still a fault');
 assert.equal(d.context.PLAYBACK_LIFETIME_HITCHES,lifetime+1);
 for(let i=17;i<=120;i++)d.frame(i/24,8000+(i+2)*1000/24,i+3);
 assert.ok(d.player.hitches.rate>.98&&d.player.hitches.rate<=1.01,
  'resumed rate uses only moving playback');
 d.context.armHitchDetector(d.video);
 assert.equal(d.listeners.get('pause').size,1,'rearming retires old lifecycle listeners');
 assert.equal(d.listeners.get('play').size,1);
 d.player.hitches.rate=1;
 d.context.document.getElementById=()=>({});
 d.event('pause');
 assert.equal(d.player.hitches.rate,1,'a retired element cannot reset the current detector');
 assert.equal(d.listeners.get('pause').size,0);
 assert.equal(d.listeners.get('play').size,0);
});
// The first presentation sample after an attach is taken on the frames, so the
// startup scrim and the HLS time-to-first-frame stamp do not wait 500 to 1000 ms
// for two timer ticks after the picture is already moving.
function startupSampler(){
 const d=detector();const ticks=[];
 // The shipped tick needs two samples: the first is its baseline, the second
 // sees the clock and the frame count advance and records the proof.
 d.context.playbackProgressTick=(_v,p)=>{ticks.push(d.context.performance.now());
  if(ticks.length%2===0)p.presentationAdvancedAt=d.context.performance.now();};
 d.player.mediaAttachment={id:1};
 return Object.assign(d,{ticks});
}
test('the first presentation proof is sampled on the frames, then left to the timer',()=>{
 const d=startupSampler();
 d.frame(0,10,1);
 assert.equal(d.ticks.length,0,'a frame before playback has started proves nothing');
 d.player.started=true;
 d.frame(1/24,52,2);d.frame(2/24,94,3);
 assert.deepEqual(d.ticks,[52,94],'baseline and proof land on consecutive frames');
 assert.equal(d.player.startupEvidence.done,true);
 for(let i=3;i<40;i++)d.frame(i/24,10+i*42,i+1);
 assert.equal(d.ticks.length,2,'once presenting, the half-second tick is the only sampler');
});
test('a new attachment owes its own first presentation proof',()=>{
 const d=startupSampler();d.player.started=true;
 d.frame(1/24,52,2);d.frame(2/24,94,3);
 d.player.mediaAttachment={id:2};
 d.frame(0,5000,1);d.frame(1/24,5042,2);d.frame(2/24,5084,3);
 assert.deepEqual(d.ticks,[52,94,5000,5042],'the predecessor\'s proof does not cover the successor');
});
test('future PTS callbacks still count as presentation receipts without becoming seek or hitch evidence',()=>{
 const d=detector();let startupSamples=0;const settlements=[];
 d.context.sampleStartupPresentation=()=>{startupSamples++;};
 d.context.settlePlaybackControlSeek=(_v,_p,mediaTime)=>{settlements.push(mediaTime);};
 d.player.started=true;d.player.controlHasFrameCallbacks=true;
 // The element clock and actual callbacks advance for twenty seconds, while
 // every callback reports an implausibly future mediaTime. The shipped policy
 // must reject those timestamps without erasing proof that frames arrived.
 for(let i=0;i<=480;i++){
  const media=i/24,wall=i*1000/24;
  // The detector owns the callback closure; pass the real metadata shape.
  // `expectedDisplayTime` equals callback time, so the one-second PTS lead is
  // outside the policy's allowed future window.
  d.frame(media,wall,i+1,media+1);
 }
 assert.equal(d.player.controlPresentedFrames,481,'each ownership-valid callback is a receipt');
 assert.equal(d.player.hitches.metadataAnomalies,481,'the future PTS remains diagnosed');
 assert.equal(d.player.hitches.frames,0,'untrusted PTS does not enter trusted hitch history');
 assert.deepEqual(settlements,[],'untrusted PTS cannot settle a seek');
 assert.equal(startupSamples,481,'receipt callbacks still reach startup presentation sampling');
 d.frame(20+1/24,20_041.667,482,20+1/24);
 assert.equal(d.player.hitches.frames,1,'the first valid callback returns to trusted hitch history');
 assert.deepEqual(settlements,[20+1/24],'only valid PTS reaches seek settlement');
 d.frame(19.9,20_083.333,483,19.9);
 assert.equal(d.player.hitches.back,1,'a subsequent real backward timestamp remains classified');
});
function installRealProgressSampler(d){
 const samplerStart=transportSource.indexOf('const PLAYBACK_SAMPLING_PERIOD_MS=');
 const samplerEnd=transportSource.indexOf('\nfunction settlePlaybackControlSeek(',samplerStart);
 assert.ok(samplerStart>=0&&samplerEnd>samplerStart,'shipped transport sampler is present');
 const startupStart=source.indexOf('function sampleStartupPresentation(');
 const startupEnd=source.indexOf('\nfunction armHitchDetector(',startupStart);
 assert.ok(startupStart>=0&&startupEnd>startupStart,'shipped startup sampler is present');
 const surfaces=[],waits={resumed:0,attempts:0,cleared:0,finished:0};
 d.player.started=true;d.player.wantsPlayback=true;d.player.ended=false;
 d.player.source={video_codec:'h264'};d.player.controlHasFrameCallbacks=true;
 d.player.controlPresentedFrames=0;d.player.mediaAttachment={id:1};d.player.attemptId='a1';
 Object.assign(d.context,{
  PERSISTENT_STALL_MS:8000,STARTUP_EVIDENCE_WINDOW_MS:1000,
  playbackSurfaceStep:event=>surfaces.push(event),playbackSurfaceGeneration:p=>p.attemptId,
  samplePlaybackPresentationClock(){},samplePreparedSwitchFrames(){},streamHasVideo:()=>true,
  endWait(resumed){if(resumed){waits.resumed++;d.player.waitAt=null;}},
  clearStall(){waits.cleared++;},finishStallRecovery(){waits.finished++;},completeHlsStartup(){},
  bufferRunway:()=>10,persistentWait(){waits.attempts++;return Promise.resolve();},
  playbackContext:()=>({}),clientLog(){},
 });
 vm.runInContext(transportSource.slice(samplerStart,samplerEnd),d.context);
 // Startup proof must pass through the same shipped frame-sample entry point,
 // which delegates to the real progress sampler without running stall timers.
 vm.runInContext(source.slice(startupStart,startupEnd),d.context);
 return {surfaces,waits,tick(at){d.setNow(at);d.context.playbackProgressTick(d.video,d.player);}};
}
test('future PTS receipts keep the real sampler presenting through a long playback interval',()=>{
 const d=detector(),h=installRealProgressSampler(d);
 d.player.waitAt=0;
 // A real callback arrives at 24 fps for 20 seconds. The video clock advances
 // at the same rate, but every PTS is one second ahead and rejected by policy.
 // Run the production half-second timer alongside those callbacks.
 for(let i=0;i<=480;i++){
  const wall=i*1000/24,media=i/24;
  d.frame(media,wall,i+1,media+1);
  if(i%12===0)h.tick(wall);
 }
 assert.ok(h.surfaces.some(event=>event.presenting===true),
  'clock plus callback receipts prove presentation despite rejected PTS');
 assert.equal(d.player.waitAt,null,'the media wait retires on actual observed presentation');
 assert.equal(h.waits.resumed,1);
 assert.equal(h.waits.attempts,0,'healthy callbacks do not enter persistent stall recovery');
 assert.ok(d.player.hitches.metadataAnomalies>0);
 assert.equal(d.player.hitches.frames,0,'bad timestamps remain outside trusted hitch history');
});
test('the real sampler still detects absent receipts or a frozen element clock',()=>{
 {
  const d=detector(),h=installRealProgressSampler(d);
  d.frame(0,0,1,0);h.tick(0);
  // Clock advances but callbacks stop: neither source alone is presentation.
  for(let at=500;at<=8500;at+=500){d.setNow(at);d.video.currentTime=at/1000;
   d.context.playbackProgressTick(d.video,d.player);}
  assert.equal(h.waits.attempts,1,'advancing clock without callback receipts remains stalled');
 }
 {
  const d=detector(),h=installRealProgressSampler(d);
  d.frame(0,0,1,0);h.tick(0);
  // Callbacks continue but repeatedly present at the same media position.
  for(let at=500;at<=8500;at+=500){d.frame(0,at,at/500+1,0);h.tick(at);}
  assert.equal(h.waits.attempts,1,'callbacks without media-clock movement remain stalled');
 }
});
test('frame sampling stands down for a paused, seeking or seek-pending player and after one second',()=>{
 const d=startupSampler();d.player.started=true;
 d.video.paused=true;d.frame(0,10,1);
 d.video.paused=false;d.video.seeking=true;d.frame(0,20,2);
 d.video.seeking=false;d.player.controlSeek={sequence:1};d.frame(0,30,3);
 assert.equal(d.ticks.length,0);
 d.player.controlSeek=null;
 // A stream whose clock never advances: the tick never records proof.
 d.context.playbackProgressTick=()=>{d.ticks.push(d.context.performance.now());};
 for(let i=0;i<60;i++)d.frame(0,100+i*42,4+i);
 const last=d.ticks[d.ticks.length-1];
 assert.ok(d.ticks.length>1&&last<=100+1000,'sampling is bounded to the first second');
 assert.equal(d.player.startupEvidence.done,true,'after that the stall detector owns the diagnosis');
});
