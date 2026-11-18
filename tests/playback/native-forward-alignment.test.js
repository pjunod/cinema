"use strict";
const test=require("node:test"),assert=require("node:assert/strict");
const fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const web=path.resolve(__dirname,"../../crates/plurxd/src/web/player");
const ranges=rows=>({get length(){return rows.length;},start:i=>rows[i][0],end:i=>rows[i][1]});
async function flush(){for(let turn=0;turn<8;turn++)await Promise.resolve();}

// Media seeks and frame callbacks are asynchronous browser inputs. The actual
// alignment, seek waiter, hold and paired-frame proof run without replacement.
// Only the final physical exposure and transport-independent ACK sink are seams.
function harness({origin=0,rate=1,latency=97,warmAfter=150,landingOffset=0,
  neverSeek=false,staleSeeked=false,mse=false,growAtWarm=0,buffer=[[1890,1913.876]],
  incumbentTime=1911.813,successorTime=1894.851,readyState=4}={}){
  let now=0,next=0,attached=true,runway=40,sparePosition=successorTime;
  const timers=new Map(),seeks=[],failed=[],exposed=[],logs=[];
  const timer=(fn,ms)=>{const id=++next;timers.set(id,{fn,at:now+ms});return id;};
  const element=time=>Object.assign(new EventTarget(),{currentTime:time,paused:false,seeking:false,
    readyState:4,playbackRate:rate,muted:true,style:{},src:mse?"blob:fixture":"native.m3u8",
    buffered:ranges(buffer),callbacks:new Map(),pauseCalls:0,playCalls:0,
    pause(){this.pauseCalls++;this.paused=true;this.dispatchEvent(new Event("pause"));},
    play(){this.playCalls++;this.paused=false;this.dispatchEvent(new Event("playing"));return Promise.resolve();},
    requestVideoFrameCallback(fn){const id=++next;this.callbacks.set(id,fn);return id;},
    cancelVideoFrameCallback(id){this.callbacks.delete(id);},
    removeAttribute(){},setAttribute(){},load(){},emit(name){this.dispatchEvent(new Event(name));}});
  const incumbent=element(incumbentTime+origin/1000),spare=element(sparePosition);
  incumbent.muted=false;spare.readyState=readyState;
  Object.defineProperty(spare,"currentTime",{get:()=>sparePosition,set(wanted){
    seeks.push({wanted,at:now,paused:spare.paused});
    spare.seeking=true;spare.readyState=2;spare.emit("seeking");
    if(staleSeeked)timer(()=>spare.emit("seeked"),20);
    if(neverSeek)return;
    const delay=Array.isArray(latency)?latency[Math.min(seeks.length-1,latency.length-1)]:latency;
    timer(()=>{sparePosition=wanted+landingOffset;spare.seeking=false;
      spare.readyState=warmAfter===0?4:2;spare.emit("seeked");
      if(warmAfter>0)timer(()=>{
        if(growAtWarm>0)spare.buffered=ranges(buffer.map(([start,end])=>[start,end+growAtWarm]));
        spare.readyState=4;spare.emit("canplay");},warmAfter);
    },delay);
  }});
  const state={actionId:"forward-action",sessionId:"successor",mediaOriginMs:origin,state:"committing",
    bufferReadyAtMs:0,stageAtMs:0,incumbentElement:incumbent,hls:mse?{}:null};
  const player={prepared:state,offset:0,vod:false,hitches:{fps:24},wantsPlayback:true};
  const ctx=vm.createContext({PLAYER:player,document:{getElementById:id=>id==="video"?incumbent:spare},
    performance:{now:()=>now},setTimeout:timer,clearTimeout:id=>timers.delete(id),
    playbackOwnsAttachedMedia:()=>attached,bufferRunway:()=>runway,
    playbackContext:()=>({}),clientLog:row=>logs.push(row),streamHasVideo:()=>true,destroyHlsInstance(){}});
  for(const file of ["video-frames.js","prepared-replacement.js","prepared-switch-measurement.js"])
    vm.runInContext(fs.readFileSync(path.join(web,file),"utf8"),ctx,{filename:file});
  ctx.queuePlaybackControlAcknowledgement=()=>{};
  ctx.failPreparedReplacement=(p,s,reason)=>{failed.push(reason);ctx.freePreparedReplacement(p,s);};
  ctx.exposePreparedReplacement=(p,s,v,target,filmMs)=>exposed.push({filmMs,at:now});
  let settled=false,result;
  const start=()=>{ctx.alignPreparedReplacement(player,state,incumbent,spare).then(value=>{settled=true;result=value;});};
  const move=ms=>{if(!incumbent.paused&&!incumbent.seeking&&!incumbent.starved)incumbent.currentTime+=ms*incumbent.playbackRate/1000;
    if(!spare.paused&&!spare.seeking&&!spare.starved)sparePosition+=ms*spare.playbackRate/1000;now+=ms;};
  async function advance(ms){
    const end=now+ms;let iterations=0;
    while(true){const entry=[...timers].filter(([,t])=>t.at<=end).sort((a,b)=>a[1].at-b[1].at)[0];
      if(!entry)break;assert.ok(++iterations<10000,"timers remain bounded");
      timers.delete(entry[0]);move(Math.max(0,entry[1].at-now));entry[1].fn();await flush();}
    move(end-now);await flush();
  }
  async function until(predicate,max=6000){const deadline=now+max;
    while(!predicate()&&now<deadline)await advance(Math.min(10,deadline-now));
    assert.ok(predicate(),`condition reached by ${max}ms: ${failed.join("; ")}`);}
  function frame(node,count){const entry=node.callbacks.entries().next().value;
    assert.ok(entry,"the real frame proof owns an armed callback");node.callbacks.delete(entry[0]);
    entry[1](now,{mediaTime:node.currentTime,expectedDisplayTime:now,presentedFrames:count});}
  async function prove({drift=0}={}){
    const frameMs=1000/(24*rate);
    for(let count=1;count<=4;count++){await advance(frameMs);frame(spare,count);
      if(count<4&&incumbent.callbacks.size)frame(incumbent,100+count);
      assert.equal(exposed.length,0,"fewer than three advancing steps or an absent paired frame cannot expose");}
    incumbent.currentTime+=drift;frame(incumbent,100);
  }
  return {ctx,player,state,incumbent,spare,timers,seeks,failed,exposed,logs,start,advance,until,prove,
    now:()=>now,settled:()=>settled,result:()=>result,runway:value=>{runway=value;},
    detach(){attached=false;},supersede(){player.prepared={sessionId:"new"};},
    teardown(){ctx.freePreparedReplacement(player,state);},
    position(value){sparePosition=value;}};
}

// The deployed Twilight trace had 2.082s past the incumbent but only 785ms
// past the successor. Model a browser that appends the next range while playing.
function twilightHarness(options={}){
  return harness({incumbentTime:13.895,successorTime:15.192,
    buffer:[[8,15.977]],readyState:3,...options});
}

test("native successor warms its own runway before pausing for the rendezvous",async()=>{
  for(const options of [{},{origin:500000},{rate:0.5},{rate:2}]){
    const h=twilightHarness(options);h.start();await flush();
    assert.equal(h.seeks.length,0);
    await h.advance(300);
    assert.equal(h.spare.paused,false,"thin successor must remain playing while it warms");
    assert.equal(h.spare.muted,true);
    assert.equal(h.exposed.length,0);
    h.spare.buffered=ranges([[8,24]]);
    await h.advance(10);
    assert.equal(h.spare.paused,true);
    assert.ok(h.spare.currentTime>15.192,"hold uses the warmed moving playhead");
    await h.until(()=>h.state.overlapPhase==="video"||h.failed.length);
    assert.equal(h.failed.length,0);
    assert.equal(h.exposed.length,0);
    await h.prove();
    assert.equal(h.exposed.length,1);
    assert.equal(h.seeks.length,0);
    assert.equal(h.incumbent.pauseCalls,0);
    assert.equal(h.timers.size,0);
  }
});

test("native warmup spends the original hold budget",async()=>{
  const h=twilightHarness();h.start();h.incumbent.starved=true;await h.advance(1600);
  h.spare.buffered=ranges([[8,24]]);
  await h.advance(10);
  assert.equal(h.spare.paused,true);
  await h.advance(1200);
  assert.match(h.failed[0],/deadline/);
  assert.equal(h.exposed.length,0);
  assert.equal(h.seeks.length,0);
  assert.equal(h.incumbent.muted,false);
});

test("native warmup refuses if pausing invalidates its observed runway",async()=>{
  const h=twilightHarness();h.start();await h.advance(300);
  const pause=h.spare.pause.bind(h.spare);
  h.spare.pause=()=>{pause();h.spare.buffered=ranges([[8,16]]);};
  h.spare.buffered=ranges([[8,24]]);
  await h.advance(10);
  assert.match(h.failed[0],/lost runway while pausing/);
  assert.equal(h.exposed.length,0);
  assert.equal(h.incumbent.muted,false);
  assert.equal(h.timers.size,0);
});

test("a native successor with sufficient runway holds immediately without a seek",async()=>{
  const h=twilightHarness();h.spare.buffered=ranges([[8,24]]);h.start();await flush();
  assert.equal(h.state.overlapPhase,"native-align-hold");
  assert.equal(h.spare.paused,true);
  assert.equal(h.seeks.length,0);
  await h.until(()=>h.state.overlapPhase==="video");
  await h.prove();
  assert.equal(h.exposed.length,1);
});

for(const [reason,change] of Object.entries({
  pause:h=>{h.incumbent.paused=true;h.incumbent.emit("pause");},
  rate:h=>{h.incumbent.playbackRate=2;h.incumbent.emit("ratechange");},
  seek:h=>{h.spare.seeking=true;},
  unmute:h=>{h.spare.muted=false;},
  stall:h=>{h.spare.starved=true;},
  supersede:h=>h.supersede(),
  teardown:h=>h.teardown(),
})){
  test(`native warmup cancels on ${reason} without exposing the successor`,async()=>{
    const h=twilightHarness();h.start();await flush();
    assert.equal(h.state.overlapPhase,"native-align-warm");
    change(h);
    await h.advance(1400);
    assert.equal(h.failed.length,["supersede","teardown"].includes(reason)?0:1);
    assert.equal(h.exposed.length,0);
    assert.equal(h.seeks.length,0);
    assert.equal(h.incumbent.muted,false);
    assert.equal(h.timers.size,0);
    assert.equal(h.state.overlapListeners,null);
  });
}

test("native warmup without observed buffer growth ends at the original deadline",async()=>{
  const h=twilightHarness();h.start();
  await h.advance(2800);
  assert.match(h.failed[0],/deadline/);
  assert.equal(h.exposed.length,0);
  assert.equal(h.seeks.length,0);
  assert.equal(h.timers.size,0);
});

for(const options of [{},{origin:500000},{rate:0.5},{rate:2}])
  test(`native forward seek survives97ms latency and warms before unchanged paired proof ${JSON.stringify(options)}`,async()=>{
    const h=harness(options);h.start();await flush();
    assert.equal(h.seeks.length,1);assert.equal(h.seeks[0].paused,true,"pause precedes asynchronous seek");
    assert.ok(h.seeks[0].wanted>1911.813&&h.seeks[0].wanted<=1913.876-1);
    assert.equal(h.incumbent.pauseCalls,0);assert.equal(h.incumbent.muted,false);
    await h.advance(97);assert.equal(h.spare.readyState,2);assert.equal(h.spare.paused,true);
    assert.equal(h.exposed.length,0);
    await h.until(()=>h.state.overlapPhase==="video"||h.failed.length);
    assert.equal(h.failed.length,0);assert.equal(h.spare.readyState,4);assert.equal(h.spare.paused,false);
    assert.equal(h.seeks.length,1,"one fixed forward aim replaces repeated moving-current seeks");
    await h.prove();assert.equal(h.exposed.length,1);assert.equal(h.failed.length,0);
    assert.equal(h.incumbent.pauseCalls,0);assert.equal(h.incumbent.muted,false);
    assert.equal(h.timers.size,0);assert.equal(h.spare.callbacks.size,0);assert.equal(h.incumbent.callbacks.size,0);
  });

test("native forward alignment caps its lead at the existing3s allowance",async()=>{
  const h=harness({buffer:[[1890,1950]]});h.start();await flush();
  assert.ok(h.seeks[0].wanted-1911.813<=3+1e-9);
  await h.until(()=>h.state.overlapPhase==="video"||h.failed.length);assert.equal(h.failed.length,0);
  await h.prove();assert.equal(h.exposed.length,1);
});

for(const buffer of [[[1890,1912.86]],[[1890,1911.7],[1911.9,1920]]])
  test(`native forward alignment refuses insufficient contiguous runway ${JSON.stringify(buffer)}`,async()=>{
    const h=harness({buffer});h.start();await flush();assert.equal(h.seeks.length,0);
    assert.equal(h.failed.length,1);assert.match(h.failed[0],/contiguous buffer/);
    assert.equal(h.exposed.length,0);assert.equal(h.incumbent.pauseCalls,0);assert.equal(h.incumbent.muted,false);
  });

test("forward seek timeout settles without moving incumbent ownership",async()=>{
  const h=harness({neverSeek:true});h.start();await h.advance(1600);
  assert.equal(h.failed.length,1);assert.match(h.failed[0],/seek timed out/);
  assert.equal(h.exposed.length,0);assert.equal(h.incumbent.pauseCalls,0);assert.equal(h.incumbent.muted,false);
  assert.equal(h.timers.size,0);
});

test("stale seeked while the forward seek is still pending cannot expose",async()=>{
  const h=harness({staleSeeked:true});h.start();await h.advance(30);
  assert.equal(h.spare.seeking,true);assert.equal(h.exposed.length,0);
  await h.advance(3000);assert.equal(h.exposed.length,0);assert.equal(h.failed.length,1);
  assert.equal(h.incumbent.muted,false);
});

test("native hold uses the actual seek landing instead of its requested aim",async()=>{
  const h=harness({landingOffset:0.02});h.start();await h.advance(110);
  const landed=h.seeks[0].wanted+0.02;
  assert.ok(Math.abs(h.spare.currentTime-landed)<1e-9);assert.equal(h.spare.paused,true);
  await h.until(()=>h.state.overlapPhase==="video"||h.failed.length);
  assert.equal(h.failed.length,0);await h.prove();assert.equal(h.exposed.length,1);
  const resumed=h.logs.find(row=>row.detail==="align_resume");assert.match(resumed.message,new RegExp(`held_at_ms=${Math.round(landed*1000)}`));
});

test("seek latency that spends the forward lead refuses rather than exposing a past frame",async()=>{
  const h=harness({latency:1300,warmAfter:0});h.start();await h.advance(1400);
  assert.equal(h.failed.length,1);assert.match(h.failed[0],/passed held position|deadline/);
  assert.equal(h.exposed.length,0);assert.equal(h.incumbent.muted,false);
});

for(const mutation of ["pause","seek","rate","intent","supersede","detach","teardown"])
  for(const phase of ["seek","hold"])
    test(`${mutation} during native forward ${phase} never exposes`,async()=>{
      const h=harness();h.start();await h.advance(phase==="seek"?20:300);
      if(mutation==="pause"){h.incumbent.paused=true;h.incumbent.emit("pause");}
      if(mutation==="seek"){h.incumbent.seeking=true;h.incumbent.emit("seeking");}
      if(mutation==="rate"){h.incumbent.playbackRate=2;h.incumbent.emit("ratechange");}
      if(mutation==="intent")h.player.wantsPlayback=false;
      if(mutation==="supersede")h.supersede();if(mutation==="detach")h.detach();if(mutation==="teardown")h.teardown();
      await h.advance(4000);
      assert.equal(h.exposed.length,0);assert.equal(h.incumbent.muted,false);
      if(!["supersede","detach","teardown"].includes(mutation))assert.equal(h.failed.length,1);
      else assert.equal(h.failed.length,0);
      assert.equal(h.timers.size,0);assert.equal(h.state.overlapListeners??null,null);
    });

test("successor buffer loss during the warmed native hold refuses the switch",async()=>{
  const h=harness();h.start();await h.advance(300);h.spare.buffered=ranges([]);await h.advance(20);
  assert.equal(h.failed.length,1);assert.match(h.failed[0],/lost held buffer/);assert.equal(h.exposed.length,0);
});

test("incumbent starvation during native hold stays unexposed and reaches the finite bound",async()=>{
  const h=harness();h.start();await h.advance(300);h.runway(0);h.incumbent.starved=true;
  await h.advance(3000);
  assert.equal(h.failed.length,1);assert.match(h.failed[0],/deadline/);assert.equal(h.exposed.length,0);
  assert.equal(h.incumbent.muted,false);assert.equal(h.timers.size,0);
});

test("warming cannot refresh the absolute forward alignment deadline",async()=>{
  const h=harness({warmAfter:5000,landingOffset:0.2});h.start();await h.advance(100);
  h.incumbent.starved=true;await h.advance(2300);
  assert.equal(h.failed.length,1);assert.equal(h.exposed.length,0);
  assert.equal(h.now(),2400);assert.match(h.failed[0],/deadline/);
  await h.advance(3000);assert.equal(h.failed.length,1);assert.equal(h.exposed.length,0,"late warming cannot revive an expired alignment");
});

test("MSE retains its two exact-current corrective seeks and residual-drift refusal",async()=>{
  const h=harness({mse:true,latency:[97,92],warmAfter:0});h.start();await h.advance(300);
  assert.equal(h.seeks.length,2);assert.equal(h.seeks[0].wanted,1911.813);
  assert.ok(Math.abs(h.seeks[1].wanted-(1911.813+0.097))<1e-9);
  assert.equal(h.seeks.every(seek=>seek.paused===false),true);
  assert.equal(h.exposed.length,0);assert.equal(h.failed.length,1);assert.match(h.failed[0],/could not align: attempt=2/);
  assert.equal(h.incumbent.muted,false);
});

test("unchanged decoded proof refuses when proof latency spends the reserved static runway",async()=>{
  const h=harness();h.start();await h.until(()=>h.state.overlapPhase==="video"||h.failed.length);
  assert.equal(h.failed.length,0,"seek and hold alone have succeeded");
  await h.advance(200);
  await h.prove();assert.equal(h.exposed.length,0);assert.equal(h.failed.length,1);
  assert.match(h.failed[0],/lost-alignment/);assert.equal(h.incumbent.muted,false);
});

test("an existing pending native seek does not bypass the new forward seek's real completion",async()=>{
  const h=harness();h.spare.seeking=true;h.start();await h.advance(50);
  assert.equal(h.spare.seeking,true);assert.equal(h.spare.paused,true);assert.equal(h.exposed.length,0);
  await h.until(()=>h.state.overlapPhase==="video"||h.failed.length);
  assert.equal(h.failed.length,0);await h.prove();assert.equal(h.exposed.length,1);
});

test("an optional newly published segment can extend buffer while native alignment warms",async()=>{
  const h=harness({growAtWarm:2});h.start();await h.advance(97);
  assert.equal(h.spare.readyState,2);assert.equal(h.spare.buffered.end(0),1913.876);
  await h.advance(150);assert.equal(h.spare.readyState,4);assert.equal(h.spare.buffered.end(0),1915.876);
  await h.until(()=>h.state.overlapPhase==="video"||h.failed.length);
  assert.equal(h.failed.length,0);await h.prove();assert.equal(h.exposed.length,1);
});
