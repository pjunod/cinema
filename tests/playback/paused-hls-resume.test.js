"use strict";
const test=require("node:test"),assert=require("node:assert/strict");
const fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const web=path.join(__dirname,"../../crates/plurxd/src/web");
const policy=require(path.join(web,"playback-policy.js"));
const playerSource=fs.readFileSync(path.join(web,"player/player.js"),"utf8");
const hlsSource=fs.readFileSync(path.join(web,"hls.min.js"),"utf8");
const flush=()=>new Promise(resolve=>setImmediate(resolve));

function harness({state="active"}={}){
  let now=0,nextTimer=0,current=true,diagnoses=0;
  const timers=new Map(),sends=[],loaders=[];
  class XHR{
    constructor(){this.readyState=0;this.status=0;}
    open(){this.readyState=1;}
    setRequestHeader(){}
    addEventListener(){}
    send(){sends.push(this);}
    abort(){this.aborted=true;}
  }
  const player={started:true,wantsPlayback:false,controlIntentGeneration:1,mediaAttachment:{}};
  const ctx=vm.createContext({module:{exports:{}},exports:{},console,player,PlaybackPolicy:policy,
    performance:{now:()=>now},XMLHttpRequest:XHR,
    setTimeout(fn,ms){const id=++nextTimer;timers.set(id,{fn,at:now+ms});return id;},
    clearTimeout(id){timers.delete(id);},bufferTargets:()=>({}),
    clientLog(){},playbackContext:()=>({}),reportTtff(){},
    stallDiagnose(){diagnoses++;return Promise.resolve();}});
  ctx.self=ctx;
  // Each fixture has its own real bundled loader and timers, without changing
  // global.self under the other web-control tests.
  vm.runInContext(hlsSource,ctx);
  const StockLoader=ctx.module.exports.DefaultConfig.loader;
  vm.runInContext(playerSource,ctx);
  vm.runInContext("PLAYER=player",ctx);
  const {startup}=ctx.hlsStartupEpisode(player,{current:()=>current},"/fixture/master.m3u8",0);
  startup.state=state;
  const hls={loadSource:loadManifest,startLoad(){},stopLoad(){}};
  startup.hls=hls;player.hls=hls;
  const Loader=ctx.createHlsStartupLoader(StockLoader,startup);
  function loadManifest(){
    const loader=new Loader({xhrSetup(){}});loaders.push(loader);
    loader.load({type:"manifest",url:startup.playlistUrl,responseType:"text"},
      {loadPolicy:policy.HLS_STARTUP.manifest_load_policy.default,timeout:10000},
      {onSuccess(){},onProgress(){},onAbort(){},onError(){},onTimeout(){}});
  }
  loadManifest();
  return {player,startup,sends,loaders,
    get diagnoses(){return diagnoses;},
    detach(){current=false;},
    resume(){player.wantsPlayback=true;player.controlIntentGeneration++;return ctx.resumeHlsStartup({currentTime:0},player);},
    advance(ms){
      now+=ms;
      for(;;){
        const due=[...timers].find(([,t])=>t.at<=now);if(!due)break;
        timers.delete(due[0]);due[1].fn();
      }
    },
    close(){for(const loader of loaders)loader.destroy();timers.clear();},
  };
}

test("paused replacement sends its first manifest when Play resumes",async()=>{
  const h=harness();
  try{
    await flush();
    assert.equal(h.sends.length,0,"pause prevents network dispatch");
    assert.equal(h.startup.state,"paused","the current attachment must remain resumable");
    const deadline=h.startup.deadlineMs;
    assert.equal(h.resume(),true);
    h.advance(0);await flush();
    assert.equal(h.sends.length,1,"the bundled loader actually sends the previously suppressed manifest");
    assert.equal(h.startup.state,"active");
    assert.equal(h.startup.deadlineMs,deadline,"resume does not extend startup's deadline");
    assert.equal(h.startup.retry.state,"dispatched");
    assert.equal(h.resume(),false,"another Play cannot spend a second corrective reload");
    h.advance(0);await flush();assert.equal(h.sends.length,1);
  }finally{h.close();}
});

test("paused replacement past its deadline diagnoses instead of silently hanging",async()=>{
  const h=harness();
  try{
    await flush();h.advance(policy.HLS_STARTUP.cold_deadline_ms);
    h.resume();h.advance(0);await flush();
    assert.equal(h.startup.state,"exhausted");
    assert.equal(h.diagnoses,1);
    assert.equal(h.sends.length,0);
  }finally{h.close();}
});

test("a detached paused replacement cannot send on Play",async()=>{
  const h=harness();
  try{
    await flush();h.detach();
    assert.equal(h.resume(),false);
    h.advance(0);await flush();assert.equal(h.sends.length,0);
  }finally{h.close();}
});

test("paused manifest gate does not revive cancelled or exhausted startup",async()=>{
  for(const state of ["cancelled","exhausted"]){
    const h=harness({state});
    try{
      await flush();assert.notEqual(h.startup.state,"paused");
      assert.equal(h.resume(),false);
      h.advance(0);await flush();assert.equal(h.sends.length,0);
    }finally{h.close();}
  }
});
