"use strict";
const test=require("node:test"),assert=require("node:assert/strict");
const fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const source=fs.readFileSync(path.join(__dirname,"../../crates/plurxd/src/web/player/prepared-replacement.js"),"utf8");
function harness({origin=600000,rate=1,voluntary=true}={}){
  let now=0,next=0,attached=true;
  const timers=new Map(),exposed=[],failed=[],pairs=[],logs=[];
  const element=()=>({currentTime:17,playbackRate:rate,paused:false,seeking:false,readyState:4,
    buffered:{length:1,start:()=>0,end:()=>60},callbacks:new Map(),listeners:new Map(),muted:true,
    requestVideoFrameCallback(fn){const id=++next;this.callbacks.set(id,fn);return id;},
    cancelVideoFrameCallback(id){this.callbacks.delete(id);},
    addEventListener(name,fn){this.listeners.set(name,fn);},removeEventListener(name){this.listeners.delete(name);}});
  const incumbent=element(),target=element();incumbent.muted=false;
  const state={sessionId:"target",mediaOriginMs:origin,bufferReadyAtMs:0};
  const player={prepared:state,offset:origin/1000,vod:false,wantsPlayback:true,
    directedChange:voluntary?{retainIncumbent:true}:null};
  const ctx=vm.createContext({PLAYER:player,document:{getElementById:id=>id==="video"?incumbent:target},
    performance:{now:()=>now},setTimeout(fn,ms){const id=++next;timers.set(id,{fn,at:now+ms});return id;},
    clearTimeout(id){timers.delete(id);},playbackOwnsAttachedMedia:()=>attached,
    streamHasVideo:()=>true,bufferRunway:()=>20,playbackContext:()=>({}),clientLog:row=>logs.push(row)});
  vm.runInContext(source,ctx);
  ctx.failPreparedReplacement=(p,s,reason)=>{failed.push(reason);p.prepared=null;};
  ctx.exposePreparedReplacement=(p,s,v,spare,filmMs)=>{exposed.push({p,s,v,spare,filmMs});};
  const meet=ctx.preparedFramesMeet;
  ctx.preparedFramesMeet=(...args)=>{const ok=meet(...args);pairs.push({args,ok});return ok;};
  function frame(element,mediaTime,displayMs,presentedFrames){
    const entry=element.callbacks.entries().next().value;assert.ok(entry,"owned frame callback is armed");
    element.callbacks.delete(entry[0]);element.currentTime=mediaTime;entry[1](now,{mediaTime,expectedDisplayTime:displayMs,presentedFrames});
  }
  return {ctx,player,state,incumbent,target,timers,exposed,failed,pairs,logs,
    start(){ctx.exposePreparedReplacementAtFrame(player,state,incumbent,target);},
    warm(){for(let i=0;i<4;i++){now=i*1000/24;frame(target,17.76775+i/24,86400+i*1000/24,213+i);}},
    targetFrame(time,display,count){frame(target,time,display,count);},
    incumbentFrame(time,display,count){frame(incumbent,time,display,count);},
    advance(ms){now+=ms;},
    deadline(){now=800;for(const [id,t] of [...timers])if(t.at<=now){timers.delete(id);t.fn();}},
    supersede(){player.prepared={sessionId:"new"};},detach(){attached=false;}};
}
test("prepared HDR handoff waits for a later aligned frame pair within its original budget",()=>{
  const h=harness();h.start();h.warm();
  h.incumbentFrame(17.976,86558.4,100);
  assert.equal(h.pairs.at(-1).ok,false,"the retained Air-shaped 49.7ms pair exceeds a 24fps frame");
  assert.equal(h.failed.length,0,"one unaligned callback does not terminate the 800ms rendezvous");
  assert.equal(h.exposed.length,0,"mismatch never exposes picture or changes audible ownership");
  assert.equal(h.incumbent.muted,false);assert.equal(h.target.muted,true);
  h.advance(34);h.targetFrame(17.93458,86558.4,217);
  assert.equal(h.pairs.at(-1).ok,true,"the later successor callback supplies the aligned display pair");assert.equal(h.exposed.length,1);
  assert.equal(h.failed.length,0);assert.equal(h.timers.size,0);
  assert.equal(h.target.callbacks.size,0);assert.equal(h.incumbent.callbacks.size,0);
});
test("prepared handoff refuses sustained decoded drift at the unchanged deadline",()=>{
  const h=harness();h.start();h.warm();
  h.incumbentFrame(18.2,86525,100);
  assert.equal(h.failed.length,0);assert.equal(h.exposed.length,0);
  h.deadline();assert.equal(h.failed.length,1);assert.match(h.failed[0],/deadline/);
  assert.equal(h.exposed.length,0);assert.equal(h.incumbent.muted,false);
  assert.equal(h.target.callbacks.size,0);assert.equal(h.incumbent.callbacks.size,0);
});
test("prepared handoff never settles a superseded frame rendezvous",()=>{
  const h=harness();h.start();h.warm();h.incumbentFrame(18.2,86525,100);
  const late=[...h.incumbent.callbacks.values()][0];h.supersede();late(0,{mediaTime:17.9,expectedDisplayTime:86525});
  h.deadline();assert.equal(h.exposed.length,0);assert.equal(h.failed.length,0);
});
test("prepared handoff retires on pause rather than waiting for alignment",()=>{
  const h=harness();h.start();h.warm();h.incumbentFrame(18.2,86525,100);
  h.incumbent.paused=true;h.incumbent.listeners.get("pause")();h.deadline();
  assert.equal(h.exposed.length,0);assert.equal(h.failed.length,1);assert.match(h.failed[0],/pressure before voluntary commit/);
});

test("prepared handoff rejects rate and buffer loss during a pending rendezvous",()=>{
  for(const lost of ["rate","buffer"]){
    const h=harness();h.start();h.warm();h.incumbentFrame(18.2,86525,100);
    if(lost==="rate")h.target.playbackRate=2;
    else h.target.buffered={length:0};
    h.incumbentFrame(17.9,86525,101);h.deadline();
    assert.equal(h.failed.length,1);assert.match(h.failed[0],/lost-alignment/);
    assert.equal(h.exposed.length,0);
  }
});
test("prepared handoff cannot use stale aligned frames to expose the target",()=>{
  const h=harness();h.start();h.warm();h.advance(51);
  h.incumbentFrame(17.9,86525,100);assert.equal(h.exposed.length,0);
  h.deadline();assert.equal(h.failed.length,1);assert.equal(h.exposed.length,0);
});
test("prepared frame rendezvous remains aligned across mapped origins and playback rates",()=>{
  const h=harness({origin:900000,rate:2});h.start();h.warm();
  h.incumbentFrame(18.2,86525,100);assert.equal(h.exposed.length,0);
  h.targetFrame(17.93458,86558.4,217);
  h.incumbentFrame(18.0,86591.8,101);
  assert.equal(h.exposed.length,1);assert.equal(h.exposed[0].filmMs,918000);
});

test("prepared successor callback cannot expose against an expired incumbent observation",()=>{
  const h=harness();h.start();h.warm();h.incumbentFrame(17.976,86558.4,100);
  h.advance(51);h.targetFrame(17.93458,86558.4,217);
  assert.equal(h.exposed.length,0);h.deadline();assert.equal(h.failed.length,1);
});
test("prepared successor callback rechecks viewer intent before exposing an aligned pair",()=>{
  const h=harness();h.start();h.warm();h.incumbentFrame(17.976,86558.4,100);
  h.player.wantsPlayback=false;h.targetFrame(17.93458,86558.4,217);
  assert.equal(h.exposed.length,0);assert.equal(h.failed.length,1);h.deadline();
  assert.equal(h.failed.length,1);
});
