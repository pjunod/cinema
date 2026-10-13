"use strict";
const test=require("node:test"),assert=require("node:assert/strict");
const fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const web=path.join(__dirname,"../../crates/plurxd/src/web");
const policy=require(path.join(web,"playback-policy.js"));
const playerSource=fs.readFileSync(path.join(web,"player/player.js"),"utf8");
const transportSource=fs.readFileSync(path.join(web,"player/transport.js"),"utf8");
const intentSource=fs.readFileSync(path.join(web,"player/prepared-switch-measurement.js"),"utf8");
const diagnosisSource=fs.readFileSync(path.join(web,"player/stall-diagnosis.js"),"utf8");
const hlsSource=fs.readFileSync(path.join(web,"hls.min.js"),"utf8");
const flush=()=>new Promise(resolve=>setImmediate(resolve));

function harness({state="active"}={}){
  let now=0,nextTimer=0,current=true,diagnoses=0;
  const timers=new Map(),sends=[],loaders=[],loads=[],loadTargets=[],surfaces=[];
  const video={currentTime:0,paused:true,seeking:false,readyState:0,ended:false,playbackRate:1,addEventListener(){},removeEventListener(){}};
  class XHR{
    constructor(){this.readyState=0;this.status=0;}
    open(){this.readyState=1;}
    setRequestHeader(){}
    addEventListener(){}
    send(){sends.push(this);}
    abort(){this.aborted=true;}
  }
  const player={started:true,wantsPlayback:false,controlIntentGeneration:1,mediaAttachment:{},source:{video_codec:"h264"},controlHasFrameCallbacks:true};
  const ctx=vm.createContext({module:{exports:{}},exports:{},console,player,PlaybackPolicy:policy,
    performance:{now:()=>now},XMLHttpRequest:XHR,TOKEN:null,
    setTimeout(fn,ms){const id=++nextTimer;timers.set(id,{fn,at:now+ms});return id;},
    clearTimeout(id){timers.delete(id);},bufferTargets:()=>({}),
    document:{hidden:false,getElementById:()=>video},playbackSurfaceStep(){},notifyPlaybackControl(){},
    playbackOwnsAttachedMedia:()=>current,playbackSeekBufferCovers:()=>false,
    currentStreamFailureOverlay:()=>null,finishStallRecovery(){},clearEffectiveProcessing(){},
    cancelNextEpisodePreparation(){},clearAutoplayNextPreparation(){},clearPlaybackControlWaiters(){},stopPlayerForExhaustion(){loads.push("terminal_stop");},
    raisePlaybackSurface:(source,facts)=>surfaces.push({source,...facts}),toast(){},
    clientLog(){},playbackContext:()=>({}),reportTtff(){},
    stallDiagnose(){diagnoses++;return Promise.resolve();}});
  ctx.self=ctx;
  // Each fixture has its own real bundled loader and timers, without changing
  // global.self under the other web-control tests.
  vm.runInContext(hlsSource,ctx);
  const StockLoader=ctx.module.exports.DefaultConfig.loader;
  vm.runInContext(playerSource,ctx);
  vm.runInContext(transportSource,ctx);
  const intentAt=intentSource.indexOf("function supersedePlaybackControlIntent(");
  vm.runInContext(intentSource.slice(intentAt,intentSource.indexOf("\n}",intentAt)+2),ctx);
  vm.runInContext("PLAYER=player",ctx);
  const {startup}=ctx.hlsStartupEpisode(player,{current:()=>current},"/fixture/master.m3u8",0);
  startup.state=state;
  const hls={loadSource:loadManifest,startLoad(at){loads.push("start");loadTargets.push(at);},stopLoad(){loads.push("stop");for(const loader of [...loaders])loader.destroy();}};
  startup.hls=hls;player.hls=hls;
  const Loader=ctx.createHlsStartupLoader(StockLoader,startup);
  function loadManifest(){
    const loader=new Loader({xhrSetup(){}});loaders.push(loader);
    loader.load({type:"manifest",url:startup.playlistUrl,responseType:"text"},
      {loadPolicy:policy.HLS_STARTUP.manifest_load_policy.default,timeout:10000},
      {onSuccess(){},onProgress(){},onAbort(){},onError(){},onTimeout(){}});
  }
  loadManifest();
  return {ctx,player,startup,sends,loaders,loads,loadTargets,surfaces,video,manifest:loadManifest,
    seek(target=30){player.controlSeek={sequence:1,targetMs:target*1000};return ctx.markPlaybackControlSeekExecuted(player,target,video);},
    present(target=30){video.readyState=4;video.currentTime=target;return ctx.settlePlaybackControlSeek(video,player,target,1,{epoch:player.controlPresentationEpoch,attachment:player.mediaAttachment,intent:player.controlSeek?.sequence});},
    useRealDiagnosis(){const at=diagnosisSource.indexOf("async function stallDiagnose(");vm.runInContext(diagnosisSource.slice(at,diagnosisSource.indexOf("\n}",at)+2),ctx);},
    get diagnoses(){return diagnoses;},
    detach(){current=false;},
    resume(){player.wantsPlayback=true;ctx.supersedePlaybackControlIntent(player);return ctx.resumeHlsStartup(video,player);},
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

test("paused VOD seek permits a manifest only for its current target lease",async()=>{
  const h=harness({state:"presenting"});
  try{
    await flush();assert.equal(h.sends.length,0);
    const pending={executed:true};h.player.controlSeek=pending;
    h.startup.pausedSeek={pending,attachment:h.player.mediaAttachment,
      intentGeneration:h.player.controlIntentGeneration};
    h.manifest();await flush();assert.equal(h.sends.length,1,'current paused seek may fetch its playlist');
    h.player.controlIntentGeneration++;h.manifest();await flush();
    assert.equal(h.sends.length,1,'a stale seek generation cannot dispatch');
    h.startup.pausedSeek.intentGeneration=h.player.controlIntentGeneration;
    h.player.controlSeek={executed:true};h.manifest();await flush();
    assert.equal(h.sends.length,1,'another target cannot reuse the old lease');
    h.player.controlSeek=pending;h.detach();h.manifest();await flush();
    assert.equal(h.sends.length,1,'a detached lease cannot fetch');
  }finally{h.close();}
});


test("paused reopened seek sends its first manifest and settles its target without Play",async()=>{
  const h=harness();
  try{
    h.seek();await flush();
    assert.equal(h.sends.length,1,"the shipped seek execution must authorize the first playlist");
    assert.equal(h.player.wantsPlayback,false);assert.equal(h.video.paused,true);
    assert.equal(h.present(),true);
    assert.equal(h.player.controlSeek,null);assert.equal(h.startup.state,"presenting");
    assert.equal(h.loads.at(-1),"stop","only the paused target is fetched");
    h.advance(policy.HLS_STARTUP.cold_deadline_ms);await flush();
    assert.equal(h.diagnoses,0,"settlement retires the startup deadline");
  }finally{h.close();}
});
test("paused reopened seek revives a suppressed manifest within the original startup budget",async()=>{
  const h=harness();
  try{
    await flush();assert.equal(h.startup.state,"paused");const deadline=h.startup.deadlineMs;
    h.seek();h.advance(0);await flush();assert.equal(h.sends.length,1);
    assert.equal(h.startup.deadlineMs,deadline);assert.equal(h.player.wantsPlayback,false);
  }finally{h.close();}
});
test("paused reopened seek with no target frame reaches the terminal startup surface",async()=>{
  const h=harness();
  try{
    h.useRealDiagnosis();h.seek();await flush();
    h.startup.manifestState="parsed";h.startup.levelState="loaded";h.startup.mediaLoaded=true;
    h.advance(policy.HLS_STARTUP.cold_deadline_ms);await flush();
    assert.equal(h.startup.state,"exhausted");
    assert.equal(h.surfaces.length,1);assert.equal(h.surfaces[0].source,"startup_exhausted");
    assert.deepEqual(Array.from(h.surfaces[0].actions),["close","retry"]);
    assert.match(h.surfaces[0].detail,/no presented frame/);
    assert.ok(h.loads.includes("terminal_stop"));assert.equal(h.player.wantsPlayback,false);
  }finally{h.close();}
});

test("Play then Pause during reopened target loading keeps the loader running",async()=>{
  const h=harness();
  try{
    h.seek();await flush();h.startup.manifestState="parsed";
    h.resume();h.player.wantsPlayback=false;h.player.controlIntentGeneration++;
    h.ctx.pauseHlsStartup(h.player);
    assert.equal(h.loads.at(-1),"start");assert.equal(h.startup.state,"active");
    assert.equal(h.present(),true);assert.equal(h.video.paused,true);
  }finally{h.close();}
});
test("Play before a paused startup corrective reload transfers its retry intent",async()=>{
  const h=harness();
  try{
    await flush();h.seek();h.resume();h.advance(0);await flush();
    assert.equal(h.sends.length,1);assert.equal(h.startup.retry.state,"dispatched");
  }finally{h.close();}
});
test("superseding a paused startup target restarts loading and fences old cleanup",async()=>{
  const h=harness();
  try{
    h.seek();await flush();h.startup.manifestState="parsed";
    const old=h.player.controlSeek.pausedStartupCleanup;
    h.ctx.beginPlaybackControlSeek(h.player,40);
    h.ctx.markPlaybackControlSeekExecuted(h.player,40,h.video);
    assert.equal(h.loads.at(-1),"start");const count=h.loads.length;
    old();assert.equal(h.loads.length,count);assert.equal(h.present(40),true);
  }finally{h.close();}
});

test("superseding a paused startup before manifest parse spends only the existing reload",async()=>{
  const h=harness();
  try{
    h.seek();await flush();const deadline=h.startup.deadlineMs;
    h.ctx.beginPlaybackControlSeek(h.player,40);
    h.ctx.markPlaybackControlSeekExecuted(h.player,40,h.video);
    h.advance(0);await flush();assert.equal(h.sends.length,2);
    assert.equal(h.startup.deadlineMs,deadline);assert.equal(h.startup.retry.state,"dispatched");
    h.ctx.beginPlaybackControlSeek(h.player,50);
    h.ctx.markPlaybackControlSeekExecuted(h.player,50,h.video);
    h.advance(0);await flush();assert.equal(h.sends.length,2,"a third target cannot mint another reload");
    assert.equal(h.startup.state,"exhausted");
  }finally{h.close();}
});
test("detaching or cancelling a paused startup lease fences its deadline and requests",async()=>{
  for(const action of ["detach","cancel"]){
    const h=harness();
    try{
      h.seek();await flush();const old=h.player.controlSeek.pausedStartupCleanup;
      if(action==="detach"){h.detach();for(const loader of h.loaders)loader.destroy();}
      else h.ctx.cancelHlsStartup(h.player,"closed");
      h.manifest();await flush();assert.equal(h.sends.length,1);
      const count=h.loads.length;old();assert.equal(h.loads.length,count);
      h.advance(policy.HLS_STARTUP.cold_deadline_ms);await flush();assert.equal(h.diagnoses,0);
    }finally{h.close();}
  }
});


function positionPausedTarget(h){
  h.player.controlHasFrameCallbacks=false;
  h.ctx.playbackSeekBufferCovers=()=>true;
  h.video.readyState=4;h.video.currentTime=30;
  return h.ctx.settlePlaybackControlSeek(h.video,h.player,null,null,null);
}

test("paused positioned reopened target resumes loading on Play without claiming a frame",async()=>{
  const h=harness();
  try{
    h.seek();await flush();h.startup.manifestState="parsed";
    const deadline=h.startup.deadlineMs,pending=h.player.controlSeek;
    assert.equal(positionPausedTarget(h),true);
    assert.equal(pending.presentationProvenance,"positioned");
    assert.equal(pending.localVodPresented,false);
    assert.equal(h.player.controlSeek,null);assert.equal(h.startup.state,"paused");
    assert.equal(h.loads.at(-1),"stop");
    assert.equal(h.resume(),true);assert.equal(h.loads.at(-1),"start");
    assert.equal(h.startup.state,"active","positioning is not presentation");
    assert.equal(h.startup.deadlineMs,deadline);
    h.ctx.completeHlsStartup(h.player);
    assert.equal(h.startup.state,"presenting");assert.equal(h.startup.pausedSeek,null);
    h.advance(policy.HLS_STARTUP.cold_deadline_ms);await flush();assert.equal(h.diagnoses,0);
  }finally{h.close();}
});

test("paused positioned reopened target retains its original startup deadline",async()=>{
  const h=harness();
  try{
    h.useRealDiagnosis();h.seek();await flush();h.startup.manifestState="parsed";
    const deadline=h.startup.deadlineMs;
    assert.equal(positionPausedTarget(h),true);
    h.advance(100);assert.equal(h.resume(),true);
    assert.equal(h.startup.deadlineMs,deadline);
    h.advance(policy.HLS_STARTUP.cold_deadline_ms-100);await flush();
    assert.equal(h.startup.state,"exhausted");assert.equal(h.surfaces[0].source,"startup_exhausted");
  }finally{h.close();}
});

test("paused positioned reopened target cannot revive a detached or superseded loader",async()=>{
  for(const action of ["detach","supersede","cancel"]){
    const h=harness();
    try{
      h.seek();await flush();h.startup.manifestState="parsed";
      assert.equal(positionPausedTarget(h),true);
      if(action==="detach")h.detach();
      else if(action==="supersede")h.player.controlIntentGeneration++;
      else h.ctx.cancelHlsStartup(h.player,"closed");
      const count=h.loads.length;assert.equal(h.resume(),false);assert.equal(h.loads.length,count);
    }finally{h.close();}
  }
});


test("positioned startup ownership survives a consecutive Play Pause Play without renewing its deadline",async()=>{
  const h=harness();
  try{
    h.seek();await flush();h.startup.manifestState="parsed";
    const deadline=h.startup.deadlineMs;assert.equal(positionPausedTarget(h),true);
    assert.equal(h.resume(),true);
    h.player.wantsPlayback=false;h.player.controlIntentGeneration++;
    assert.equal(h.ctx.pauseHlsStartup(h.player),true);assert.equal(h.loads.at(-1),"stop");
    assert.equal(h.resume(),true);assert.equal(h.loads.at(-1),"start");
    assert.equal(h.startup.state,"active");assert.equal(h.startup.deadlineMs,deadline);
  }finally{h.close();}
});


test("positioned startup lease follows a tightened absolute seek deadline",async()=>{
  const h=harness();
  try{
    h.useRealDiagnosis();h.seek();await flush();h.startup.manifestState="parsed";
    h.ctx.configureHlsStartupDeadline(h.player,20000);
    assert.equal(positionPausedTarget(h),true);assert.equal(h.startup.deadlineMs,20000);
    h.advance(20000);await flush();
    assert.equal(h.startup.state,"exhausted");assert.equal(h.surfaces[0].source,"startup_exhausted");
  }finally{h.close();}
});

test("Play during a coalesced seek replaces positioned loader custody only when the new target executes",async()=>{
  const h=harness();
  try{
    h.seek();await flush();h.startup.manifestState="parsed";
    const deadline=h.startup.deadlineMs;
    assert.equal(positionPausedTarget(h),true);
    h.ctx.beginPlaybackControlSeek(h.player,40);
    const count=h.loads.length;assert.equal(h.resume(),false);
    assert.equal(h.loads.length,count,"Play cannot load the superseded target");
    h.advance(100);
    assert.equal(h.ctx.markPlaybackControlSeekExecuted(h.player,40,h.video),true);
    assert.equal(h.loads.at(-1),"start");assert.equal(h.loadTargets.at(-1),40);
    assert.equal(h.startup.state,"active");assert.equal(h.startup.deadlineMs,deadline);
    assert.equal(h.startup.pausedSeek.pending,h.player.controlSeek);
    assert.notEqual(h.startup.pausedSeek.positioned,true);
  }finally{h.close();}
});
