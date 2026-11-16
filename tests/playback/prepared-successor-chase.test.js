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
