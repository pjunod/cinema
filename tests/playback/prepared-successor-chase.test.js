"use strict";
// A prepared successor plays muted at normal speed from where it was aimed, and
// a browser buffers only around the successor's own playhead. One that opened
// slower than its staging lead (Twilight in Safari: 4K Dolby Vision, 40 s
// behind) never caught the incumbent and was reaped at the server's deadline.
const test=require("node:test"),assert=require("node:assert/strict");
const fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const source=fs.readFileSync(path.join(__dirname,"../../crates/plurxd/src/web/player/prepared-replacement.js"),"utf8");
function ranges(list){return {length:list.length,start:i=>list[i][0],end:i=>list[i][1]};}
function harness({filmSec=557,spareSec=292,spareBuffered=[[254.7,292.3]]}={}){
  const incumbent=Object.assign(new EventTarget(),{currentTime:filmSec,paused:false,seeking:false});
  const spare=Object.assign(new EventTarget(),{currentTime:spareSec,seeking:false,readyState:4,play(){},
    buffered:ranges(spareBuffered),seeks:[]});
  Object.defineProperty(spare,"position",{get(){return this.currentTime;}});
  const state={sessionId:"successor",mediaOriginMs:0,metadata:true,buffered:false,
    state:"metadata_ready",stageAtMs:0};
  const player={prepared:state,offset:0,vod:false,directedChange:null};
  const logs=[],failed=[],commits=[],acks=[];
  const ctx=vm.createContext({PLAYER:player,
    document:{getElementById:id=>id==="video"?incumbent:spare},
    performance:{now:()=>0},setTimeout:(fn)=>0,clearTimeout:()=>{},
    playbackOwnsAttachedMedia:()=>true,bufferRunway:()=>40,playbackContext:()=>({}),
    clientLog:row=>logs.push(row),tok:url=>url});
  vm.runInContext(source,ctx);
  ctx.preparedVideoElement=()=>spare;
  ctx.failPreparedReplacement=(p,s,reason)=>{failed.push(reason);s.state="failed";p.prepared=null;};
  ctx.commitPreparedReplacement=(p,s)=>{commits.push(s);};
  ctx.queuePlaybackControlAcknowledgement=(p,id,st)=>acks.push(st);
  // A seek lands when `seeked` fires; the harness decides when that is.
  ctx.preparedAlignSeek=(element,wanted)=>{element.seeks.push(wanted);
    return new Promise(resolve=>{element.land=()=>{element.currentTime=wanted;resolve(true);};});};
  return {ctx,player,state,incumbent,spare,logs,failed,commits,acks,
    note(){ctx.notePreparedBuffer(player,state);}};
}
const tick=()=>new Promise(resolve=>setImmediate(resolve));

test("a successor playing behind the incumbent is re-aimed at the incumbent's current second",async()=>{
  const h=harness();
  h.note();
  assert.equal(h.spare.seeks.length,1,"one re-aim, not a wait for a successor that cannot catch up");
  assert.equal(h.spare.seeks[0],560,"aimed at the incumbent's second plus the chase lead");
  assert.equal(h.commits.length,0);assert.equal(h.failed.length,0);
  assert.match(h.logs.at(-1).message,/265000 ms behind the incumbent; re-aim 1\/4/);
  h.note();
  assert.equal(h.spare.seeks.length,1,"no second re-aim while the first is still seeking");
  h.spare.buffered=ranges([[560,575]]);h.incumbent.currentTime=559;
  h.spare.land();await tick();
  assert.equal(h.commits.length,0,"landed ahead: it waits for the incumbent to reach it");
  assert.equal(h.spare.seeks.length,1,"and is not re-aimed backwards");
  h.incumbent.currentTime=560.1;h.note();
  assert.equal(h.commits.length,1,"once its buffer covers the incumbent's second the switch proceeds");
  assert.deepEqual(h.acks,["buffer_ready"]);
  assert.equal(h.failed.length,0);
});

test("a successor whose buffer already covers the incumbent's second is not re-aimed",()=>{
  const h=harness({filmSec:100,spareSec:99,spareBuffered:[[90,130]]});
  h.note();
  assert.equal(h.spare.seeks.length,0);
  assert.equal(h.commits.length,1);
});

test("a successor ahead of the incumbent waits for it rather than seeking back",()=>{
  const h=harness({filmSec:100,spareSec:103,spareBuffered:[[103,110]]});
  h.note();
  assert.equal(h.spare.seeks.length,0);assert.equal(h.commits.length,0);assert.equal(h.failed.length,0);
});

test("each re-aim doubles its lead and the fifth miss fails the preparation",async()=>{
  const h=harness({filmSec:100,spareSec:50,spareBuffered:[[40,50]]});
  const leads=[];
  for(let i=0;i<4;i++){
    h.note();
    leads.push(Math.round((h.spare.seeks.at(-1)-h.incumbent.currentTime)*1000));
    h.spare.land();await tick();
    h.spare.currentTime=50;h.spare.buffered=ranges([[40,50]]);h.incumbent.currentTime+=20;
  }
  assert.deepEqual(leads,[3000,6000,12000,24000]);
  h.note();
  assert.equal(h.failed.length,1);
  assert.match(h.failed[0],/stayed \d+ ms behind the incumbent after 4 re-aims/);
  assert.equal(h.commits.length,0);
});

test("a successor already past buffer_ready is never re-aimed mid-switch",()=>{
  const h=harness();h.state.buffered=true;h.state.state="committing";
  h.note();
  assert.equal(h.spare.seeks.length,0);
});

test("a successor that has not started is never judged behind",()=>{
  const h=harness({spareSec:0,spareBuffered:[]});h.spare.readyState=1;
  h.note();
  assert.equal(h.spare.seeks.length,0,"hls.js reports time zero before its first append");
  assert.equal(h.failed.length,0);
});

test("a voluntary hls.js trial keeps no back buffer and is not re-aimed",()=>{
  const h=harness();h.state.hls={};h.player.directedChange={retainIncumbent:true};
  h.ctx.bufferRunway=()=>40;
  h.note();
  assert.equal(h.spare.seeks.length,0);
});

test("native metadata aims the successor at the incumbent's current second, not the staged one",()=>{
  const h=harness();h.state.metadata=false;h.state.startAtSec=7.934;h.state.state="building";
  h.ctx.notePreparedMetadata=(p,s)=>{s.metadata=true;};
  h.spare.currentTime=0;
  h.ctx.preparedNativeAttach(h.player,h.state,h.spare);
  h.spare.dispatchEvent(new Event("loadedmetadata"));
  assert.equal(h.spare.currentTime,560,"557 s incumbent + 3 s lead, not the 7.9 s staged when the open began");
});

// The deployed chase reached buffer_ready at film 1035.005 after aiming at
// 1038.739. Failure followed 3007ms later, consistent with two seek bounds;
// the old log did not record the exact failing predicate. Exercise that overlap
// with the real commit, hold, and decoded frame-rendezvous guard.
function nativeHoldHarness({origin=0,rate=1,runway=40}={}){
  let now=0,next=0,attached=true;
  const timers=new Map(),logs=[],failed=[],exposed=[],seeks=[];
  const element=(time)=>Object.assign(new EventTarget(),{currentTime:time,paused:false,seeking:false,
    playbackRate:rate,readyState:4,muted:true,style:{},buffered:ranges([[1032,1050]]),callbacks:new Map(),
    pause(){this.paused=true;},play(){this.paused=false;return Promise.resolve();},
    requestVideoFrameCallback(fn){const id=++next;this.callbacks.set(id,fn);return id;},
    cancelVideoFrameCallback(id){this.callbacks.delete(id);},removeAttribute(){},setAttribute(){},load(){},
    emit(name){this.dispatchEvent(new Event(name));}});
  const incumbent=element(origin/1000+1035.005),spare=element(1038.739);incumbent.muted=false;
  const state={actionId:"action",sessionId:"successor",mediaOriginMs:origin,metadata:true,
    state:"metadata_ready",buffered:false,stageAtMs:0,incumbentElement:incumbent};
  const player={prepared:state,offset:0,vod:false,hitches:{fps:24},wantsPlayback:true};
  const ctx=vm.createContext({PLAYER:player,document:{getElementById:id=>id==="video"?incumbent:spare},
    performance:{now:()=>now},setTimeout(fn,ms){const id=++next;timers.set(id,{fn,at:now+ms});return id;},
    clearTimeout:id=>timers.delete(id),playbackOwnsAttachedMedia:()=>attached,bufferRunway:()=>runway,
    playbackContext:()=>({}),clientLog:row=>logs.push(row),streamHasVideo:()=>true,destroyHlsInstance(){}});
  const web=path.join(__dirname,"../../crates/plurxd/src/web/player");
  vm.runInContext(fs.readFileSync(path.join(web,"video-frames.js"),"utf8"),ctx);
  vm.runInContext(source,ctx);
  vm.runInContext(fs.readFileSync(path.join(web,"prepared-switch-measurement.js"),"utf8"),ctx);
  ctx.queuePlaybackControlAcknowledgement=()=>{};
  ctx.failPreparedReplacement=(p,s,reason)=>{failed.push(reason);ctx.freePreparedReplacement(p,s);};
  ctx.exposePreparedReplacement=()=>exposed.push(true);
  ctx.preparedAlignSeek=async(element,wanted)=>{seeks.push(wanted);return false;};
  function frame(element,time,display,count){
    const entry=element.callbacks.entries().next().value;assert.ok(entry,"frame observer remains armed");
    element.callbacks.delete(entry[0]);element.currentTime=time;
    entry[1](now,{mediaTime:time,expectedDisplayTime:display,presentedFrames:count});
  }
  return {ctx,player,state,incumbent,spare,timers,logs,failed,exposed,seeks,
    start(){ctx.notePreparedBuffer(player,state);},
    async arrive(overshoot=0,held=1038.739){incumbent.currentTime=origin/1000+held+overshoot;
      incumbent.emit("timeupdate");await tick();},
    async advance(ms){now+=ms;for(const [id,t] of [...timers])if(t.at<=now){timers.delete(id);t.fn();}await tick();},
    detach(){attached=false;},
    prove({drift=0,held=1038.739}={}){
      for(let i=0;i<4;i++) frame(spare,held+i/24,1000+i*1000/24,1+i);
      frame(incumbent,origin/1000+held+3/24+drift,1125,100);
    }};
}

test("native chase holds its buffered lead and resumes through the unchanged frame guard",async()=>{
  for(const options of [{},{origin:600000,rate:2}]){
    const h=nativeHoldHarness(options);h.start();
    assert.equal(h.state.state,"committing");assert.equal(h.spare.paused,true);
    assert.equal(h.incumbent.paused,false);assert.equal(h.incumbent.muted,false);
    assert.equal(h.spare.muted,true);assert.equal(h.seeks.length,0,"no native backward seek");
    assert.equal(h.exposed.length,0);assert.equal(h.logs.at(-1).detail,"align_hold");
    await h.arrive();
    assert.equal(h.spare.paused,false);assert.equal(h.spare.playbackRate,h.incumbent.playbackRate);
    assert.equal(h.exposed.length,0,"resuming is not proof of an aligned decoded picture");
    assert.equal(h.state.overlapListeners.length,4,"hold listeners were replaced, not leaked");
    h.prove();assert.equal(h.exposed.length,1);assert.equal(h.failed.length,0);
    assert.equal(h.seeks.length,0);assert.equal(h.timers.size,0);
    assert.equal(h.spare.callbacks.size,0);assert.equal(h.incumbent.callbacks.size,0);
  }
});

test("native hold still refuses decoded frame drift after resuming",async()=>{
  const h=nativeHoldHarness();h.start();await h.arrive();h.prove({drift:0.1});
  assert.equal(h.exposed.length,0);await h.advance(800);
  assert.match(h.failed[0],/deadline/);assert.equal(h.incumbent.muted,false);
});

test("native alignment hold cancels on viewer intent, buffer loss, and exhausted runway",async()=>{
  for(const reason of ["pause","seeking","ratechange","intent","buffer","late","deadline","runway"]){
    const h=nativeHoldHarness({runway:reason==="runway"?2:40});h.start();
    if(reason==="pause")h.incumbent.paused=true;
    if(reason==="seeking")h.incumbent.seeking=true;
    if(reason==="ratechange")h.incumbent.playbackRate=2;
    if(reason==="intent")h.player.wantsPlayback=false;
    if(reason==="buffer")h.spare.buffered=ranges([]);
    if(reason==="late")await h.arrive(0.1);
    else if(reason==="deadline")await h.advance(6000);
    else if(reason!=="runway")await h.advance(10);
    assert.equal(h.failed.length,1,reason);assert.equal(h.exposed.length,0,reason);
    assert.equal(h.incumbent.muted,false);assert.equal(h.timers.size,0);
    assert.equal(h.state.overlapListeners,null);assert.equal(h.seeks.length,0);
  }
});

test("native alignment hold cannot resume after its deadline even at the held position",async()=>{
  const h=nativeHoldHarness();h.start();h.incumbent.currentTime=1038.739;
  await h.advance(6000);
  assert.match(h.failed[0],/deadline/);assert.equal(h.exposed.length,0);
  assert.equal(h.spare.paused,true);assert.equal(h.timers.size,0);
});

test("native alignment hold teardown and supersession release their owned work",async()=>{
  const h=nativeHoldHarness();h.start();
  h.ctx.freePreparedReplacement(h.player,h.state);
  assert.equal(h.timers.size,0);assert.equal(h.state.overlapListeners,null);
  await h.arrive();assert.equal(h.exposed.length,0);assert.equal(h.failed.length,0);
  const stale=nativeHoldHarness();stale.start();stale.player.prepared={sessionId:"new"};
  await stale.advance(10);assert.equal(stale.timers.size,0);assert.equal(stale.failed.length,0);
  assert.equal(stale.state.overlapListeners,null);assert.equal(stale.exposed.length,0);
});

test("alignment telemetry distinguishes seek timeout, buffer loss, and residual drift",async()=>{
  for(const outcome of ["timeout","buffer","drift"]){
    const h=nativeHoldHarness();h.state.hls={};h.spare.currentTime=1034;
    h.ctx.preparedAlignSeek=async(element,wanted)=>{
      if(outcome==="timeout")return false;
      element.currentTime=wanted;
      if(outcome==="buffer")element.readyState=2;
      else h.incumbent.currentTime+=0.1;
      return true;
    };
    await h.ctx.alignPreparedReplacement(h.player,h.state,h.incumbent,h.spare);
    const seeks=h.logs.filter(row=>row.detail==="align_seek");assert.equal(seeks.length,2);
    assert.match(seeks[0].message,/from_ms=1034000 wanted_ms=1035005/);
    assert.match(seeks[0].message,new RegExp(`seeked=${outcome!=="timeout"}`));
    if(outcome==="buffer")assert.match(seeks[0].message,/ready_state=2 buffered=false/);
    if(outcome==="drift")assert.match(seeks[0].message,/drift_ms=-100/);
    assert.match(h.failed[0],/could not align: attempt=2/);
  }
});

// The 21:21 UTC Safari replay had 4338ms of buffer past the incumbent but
// only 496ms past the successor. #1711 treated that as a reason to seek back.
test("native hold warms the replay's 496ms own runway without a backward seek",async()=>{
  const h=nativeHoldHarness();h.incumbent.currentTime=2191.821;h.spare.currentTime=2195.663;
  h.spare.buffered=ranges([[2188,2196.159]]);h.start();
  assert.equal(h.spare.paused,false,"thin native runway warms while playing muted");
  assert.equal(h.seeks.length,0);assert.equal(h.failed.length,0);assert.equal(h.exposed.length,0);
  await h.advance(1000);
  assert.equal(h.failed.length,0,"initial thin buffer is allowed to warm under the incumbent");
  h.spare.buffered=ranges([[2188,2199]]);await h.advance(10);
  assert.equal(h.spare.paused,true);
  await h.arrive(0,2195.663);
  assert.equal(h.spare.paused,false);assert.equal(h.seeks.length,0);
  assert.equal(h.exposed.length,0,"warm buffer and resume still need decoded frame proof");
  h.prove({held:2195.663});assert.equal(h.exposed.length,1);assert.equal(h.failed.length,0);
  assert.equal(h.timers.size,0);
});

test("native hold freezes the completed seek position rather than its requested target",async()=>{
  const h=nativeHoldHarness();h.spare.seeking=true;h.spare.readyState=2;h.start();
  assert.equal(h.spare.paused,true);assert.equal(h.seeks.length,0);
  // A stale seeked event cannot prove that the element has finished seeking.
  h.spare.emit("seeked");await h.advance(10);
  assert.equal(h.failed.length,0);assert.equal(h.exposed.length,0);
  h.spare.currentTime=1039;h.spare.seeking=false;h.spare.readyState=4;await h.advance(10);
  await h.arrive();assert.equal(h.spare.paused,true,"the requested 1038.739 is not the actual rendezvous");
  await h.arrive(0,1039);assert.equal(h.spare.paused,false);
  h.prove({held:1039});assert.equal(h.exposed.length,1);assert.equal(h.seeks.length,0);
});

test("native hold waits for future-data readiness without seeking back",async()=>{
  const h=nativeHoldHarness();h.spare.readyState=2;h.spare.paused=true;h.start();await h.advance(10);
  assert.equal(h.spare.paused,true);assert.equal(h.failed.length,0);assert.equal(h.seeks.length,0);
  h.spare.readyState=3;await h.advance(10);await h.arrive();h.prove();
  assert.equal(h.exposed.length,1);assert.equal(h.failed.length,0);
});

test("native hold refuses thin buffer at crossing and bounds an unfinished seek",async()=>{
  const thin=nativeHoldHarness();thin.spare.buffered=ranges([[1032,1039.235]]);thin.start();
  await thin.arrive();assert.match(thin.failed[0],/lost its lead while warming/);
  assert.equal(thin.exposed.length,0);assert.equal(thin.incumbent.muted,false);
  assert.equal(thin.seeks.length,0);assert.equal(thin.timers.size,0);
  const seeking=nativeHoldHarness();seeking.spare.seeking=true;seeking.start();
  await seeking.advance(6000);assert.match(seeking.failed[0],/deadline/);
  assert.equal(seeking.exposed.length,0);assert.equal(seeking.seeks.length,0);
  assert.equal(seeking.timers.size,0);
});

test("native hold checks the settled seek position for overshoot and incumbent runway",async()=>{
  for(const at of [1034,1075]){
    const h=nativeHoldHarness();h.spare.seeking=true;h.start();
    h.spare.currentTime=at;h.spare.buffered=ranges([[at,at+10]]);h.spare.seeking=false;
    await h.advance(10);
    assert.match(h.failed[0],at===1034?/passed held position/:/insufficient incumbent runway/);
    assert.equal(h.exposed.length,0);assert.equal(h.seeks.length,0);assert.equal(h.timers.size,0);
  }
});

test("native hold does not reset its deadline after seek completion or warming",async()=>{
  const h=nativeHoldHarness();h.spare.seeking=true;h.start();await h.advance(5000);
  h.spare.seeking=false;await h.advance(10);await h.advance(300);
  assert.match(h.failed[0],/deadline/);assert.equal(h.exposed.length,0);assert.equal(h.timers.size,0);
});

test("native hold tears down while initially thin or still seeking",async()=>{
  for(const pending of ["thin","seek"]){
    const h=nativeHoldHarness();
    if(pending==="thin")h.spare.buffered=ranges([[1032,1039.235]]);
    else h.spare.seeking=true;
    h.start();h.ctx.freePreparedReplacement(h.player,h.state);
    assert.equal(h.timers.size,0);assert.equal(h.state.overlapListeners,null);
    await h.arrive();assert.equal(h.exposed.length,0);assert.equal(h.failed.length,0);
  }
});
