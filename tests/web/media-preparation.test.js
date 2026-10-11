"use strict";
const assert=require("node:assert/strict");
const fs=require("node:fs");
const vm=require("node:vm");
const test=require("node:test");
const trackSource=fs.readFileSync("crates/plurxd/src/web/detail/track-facts.js","utf8");
const preplaySource=fs.readFileSync("crates/plurxd/src/web/detail/preplay-selection.js","utf8");
function harness(){
  const mounts=new Map();
  const ctx=vm.createContext({console,Date,Map,Set,Promise,JSON,
    esc:s=>String(s??"").replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll('"','&quot;'),
    langName:s=>s,fmtChannels:()=>"",exactWireId:f=>String(f.id),
    PAGE_RENDER_GENERATION:1,PAGE_TIMER:null,location:{hash:"#/item/1"},
    clearInterval:()=>{ctx.clears++},clears:0,arms:0,
    document:{getElementById:id=>mounts.get(id)||null,visibilityState:"visible",activeElement:null},
    setPageTimer:()=>{ctx.arms++},api:async()=>{},ME:{is_admin:false},
  });
  vm.runInContext(trackSource,ctx);vm.runInContext(preplaySource,ctx);
  function mount(id){
    const summary={focus(){ctx.document.activeElement=this}};
    const details={dataset:{prepDisclosure:"tracks"},open:true,querySelector:()=>summary};
    const m={dataset:{},writes:0,details,querySelector:()=>m.details,querySelectorAll:()=>[m.details],
      set innerHTML(value){m.html=value;m.writes++;m.details={dataset:{prepDisclosure:"tracks"},parentElement:{closest:()=>null},open:false,querySelector:()=>({focus(){ctx.document.activeElement="restored"}})}},get innerHTML(){return m.html}};
    mounts.set(`prep-file-${id}`,m);return m;
  }
  return {ctx,mount,mounts};
}
const file={id:"9007199254740993",video_codec:"hevc",subtitle_streams:[{index:0,language:"eng",codec:"ass"}]};
function complete(){return {checked_at_ms:1,active:false,playback:{state:"ready"},subtitles:{state:"done",preferred_index:0,ready:1,total:1,tracks:[{index:0,state:"ready"}],work:{state:"idle"}},markers:{state:"done"},probe:{state:"done"},metadata:{state:"done"}}}
test("preparation groups count every part once and start collapsed",()=>{
  const {ctx}=harness();
  const data=complete();
  Object.assign(data,{playback:{state:"queued"},subtitles:{state:"unknown"},versions:{state:"on_demand"},thumbnails:{state:"on_demand"},conversion:{state:"off"},search:{state:"ready"},downloads:{state:"off"}});
  const html=ctx.mediaPreparationHtml(file,data);
  const groups=[...html.matchAll(/<details class="prep-group" data-prep-disclosure="([^"]+)"><summary><span>(.*?)<\/span><span class="prep-count" aria-label="(\d+) parts">/g)];
  assert.deepEqual(groups.map(g=>[g[1],Number(g[3])]),[["queued",1],["on-demand",2],["done-ready",4],["unknown",1],["off",2]]);
  assert.equal(groups.reduce((n,g)=>n+Number(g[3]),0),10);
  assert.doesNotMatch(html,/<details[^>]*\bopen\b/);
  assert.equal((html.match(/class="prep-row"/g)||[]).length,10);
  data.playback.state="failed";data.search.state="future_state";
  const changed=ctx.mediaPreparationHtml(file,data);
  assert.match(changed,/data-prep-disclosure="attention"/);
  assert.match(changed,/data-prep-disclosure="unknown"><summary><span>Unknown<\/span><span class="prep-count" aria-label="2 parts">/);
});
test("cancelled and unknown preparation never claim core completion",()=>{
  const {ctx}=harness();
  assert.match(ctx.mediaPreparationHtml(file,complete()),/Core preparation complete/);
  for(const state of ["cancelled","future_state"]){const d=complete();d.markers.state=state;assert.doesNotMatch(ctx.mediaPreparationHtml(file,d),/Core preparation complete/)}
  const d=complete();d.subtitles.preferred_index=null;d.subtitles.state="pending";
  assert.match(ctx.mediaPreparationHtml(file,d),/waiting for all tracks/);
});
test("refresh preserves subtitle disclosure focus and unchanged snapshots",async()=>{
  const {ctx,mount}=harness();const m=mount(file.id);
  ctx.document.activeElement=m.details.querySelector("summary");
  ctx.api=async()=>complete();
  assert.equal(await ctx.hydrateMediaPreparation([file],1),false);
  assert.equal(m.details.open,true);assert.equal(ctx.document.activeElement,"restored");
  await ctx.hydrateMediaPreparation([file],1);assert.equal(m.writes,1);
  ctx.api=async()=>{throw new Error("unavailable")};
  await ctx.hydrateMediaPreparation([file],1);assert.match(m.html,/status unavailable/);assert.doesNotMatch(m.html,/Core preparation complete/);
});
test("manual and automatic refresh join one flight and terminal work stops polling",async()=>{
  const {ctx,mount}=harness();mount(file.id);
  let resolve;let calls=0;ctx.api=()=>{calls++;return new Promise(r=>resolve=r)};
  const first=ctx.pollDvFileActions([file],1,true);
  const second=ctx.pollDvFileActions([file],1,true);
  assert.equal(calls,1);resolve(complete());await Promise.all([first,second]);
  assert.equal(ctx.arms,0);assert.equal(ctx.clears,1);
  ctx.api=async()=>({...complete(),active:true});await ctx.pollDvFileActions([file],1,true);assert.equal(ctx.arms,1);
});
test("refresh keeps keyboard focus in the list when its group disappears",async()=>{
  const {ctx,mount}=harness();const m=mount(file.id);
  m.details.dataset.prepDisclosure="queued";
  ctx.document.activeElement=m.details.querySelector("summary");
  m.querySelector=()=>({focus(){ctx.document.activeElement="fallback summary"}});
  ctx.api=async()=>({...complete(),playback:{state:"running"}});
  await ctx.hydrateMediaPreparation([file],1);
  assert.equal(ctx.document.activeElement,"fallback summary");
});
test("a navigation discards the old preparation response",async()=>{
  const {ctx,mount}=harness();const m=mount(file.id);let resolve;
  ctx.api=()=>new Promise(r=>resolve=r);
  const pending=ctx.hydrateMediaPreparation([file],1);ctx.PAGE_RENDER_GENERATION=2;
  resolve(complete());await pending;assert.equal(m.writes,0);
});

// Exercise the shipped preparation owner and page model together: metadata
// warming must remain independent of the open player and its track choices.
function nextEpisodeHarness(){
  const {ctx}=harness();
  vm.runInContext(fs.readFileSync("crates/plurxd/src/web/core/file-context.js","utf8"),ctx);
  const reads=[],settings=new Map();
  const season={id:"10",kind:"season"},show={id:"20",kind:"show"};
  const episode=id=>({item:{id,kind:"episode",title:`Episode ${id}`,library_id:"1"},
    files:[{id:String(100+Number(id)),available:true,duration_ms:100000}],ancestors:[show,season]});
  const data={"/items/1":episode("1"),"/items/2":episode("2"),
    "/items/10":{children:[{id:"1",kind:"episode"},{id:"2",kind:"episode"}]}};
  Object.assign(ctx,{AbortController,setTimeout,clearTimeout,now:0,position:75,
    performance:{now:()=>ctx.now},localStorage:{getItem:k=>settings.get(k),setItem:(k,v)=>settings.set(k,v)},
    PLAYER:{fileId:"101",wantsPlayback:true,meta:{kind:"episode"}},ITEM_FOR_FILE:{"101":"1"},WATCH:null,
    playerMeta:it=>({kind:it.kind}),pbTotalSec:()=>100,pbPosSec:()=>ctx.position,
    playbackOwnsAttachedMedia:()=>true,libsCached:async()=>[],
    syncPlayerNextTrack:()=>{},toast:()=>{},api:async(path)=>{reads.push(path);assert.ok(data[path],path);return data[path];},
  });
  const measure=fs.readFileSync("crates/plurxd/src/web/player/measurements.js","utf8");
  vm.runInContext(measure.slice(measure.indexOf("function beginPlaybackPreparation("),measure.indexOf("// Set by a caller")),ctx);
  vm.runInContext(fs.readFileSync("crates/plurxd/src/web/player/autoplay-next.js","utf8"),ctx);
  const video={paused:false,seeking:false};
  return {ctx,reads,data,video,warm:()=>ctx.prepareNextEpisodeIfNearEnd(ctx.PLAYER,video)};
}
test("next episode prewarm is single flight and publishes no playback state",async()=>{
  const {ctx,reads,warm}=nextEpisodeHarness();
  vm.runInContext('PREPLAY={101:{audio:3}}',ctx);
  warm();const state=ctx.PLAYER.nextEpisodePreparation;
  for(let i=0;i<10;i++)warm();
  await state.promise;
  assert.deepEqual(reads,["/items/1","/items/10","/items/2"]);
  assert.equal(vm.runInContext("PREPLAY['101'].audio",ctx),3);
  assert.equal(ctx.ITEM_FOR_FILE["102"],undefined);
  assert.equal(ctx.PLAYER.fileId,"101");
  assert.equal(await ctx.playNextEpisode(),true);
  assert.equal(ctx.location.hash,"#/item/2");
  assert.equal(reads.length,3,"transition repeated successor metadata reads");
  const prepared=ctx.takeAutoplayNextPreparation("2");
  assert.equal(prepared.page.id,"2");
  await ctx.loadItem("2",()=>true,prepared.page);
  assert.equal(reads.length,3,"loadItem repeated a prepared item read");
  assert.equal(ctx.ITEM_FOR_FILE["102"],"2");
  assert.equal(vm.runInContext("Object.keys(PREPLAY).length",ctx),0);
  assert.equal(ctx.takeAutoplayNextPreparation("2"),null);
});
test("next episode prewarm aborts on seek and autoplay off and drops late reads",async()=>{
  for(const action of [ctx=>{ctx.PLAYER._seekToken=1;},ctx=>ctx.setAutoNext(false)]){
    const {ctx,warm}=nextEpisodeHarness();let finish,signal;
    ctx.api=(_path,options)=>{signal=options.signal;return new Promise(r=>finish=r);};
    warm();const state=ctx.PLAYER.nextEpisodePreparation;
    action(ctx);ctx.position=10;warm();
    assert.equal(signal.aborted,true);
    finish({item:{kind:"episode"}});
    await state.promise;
    assert.equal(state.page,null);
    assert.equal(ctx.PLAYER.nextEpisodePreparation,null);
  }
});
test("next episode metadata expires without background polling or stale acceptance",async()=>{
  const {ctx,reads,warm}=nextEpisodeHarness();
  warm();await ctx.PLAYER.nextEpisodePreparation.promise;
  ctx.now=61000;
  for(let i=0;i<20;i++)warm();
  assert.equal(reads.length,3,"an expired warm result became a polling loop");
  assert.equal(await ctx.playNextEpisode(),true);
  assert.equal(reads.length,6,"expired metadata was accepted");
});
test("next episode preparation crosses seasons through the same bounded resolver",async()=>{
  const {ctx,reads,data,warm}=nextEpisodeHarness();
  data["/items/10"]={children:[{id:"1",kind:"episode"}]};
  data["/items/20"]={children:[{id:"10",kind:"season"},{id:"11",kind:"season"}]};
  data["/items/11"]={children:[{id:"2",kind:"episode"}]};
  warm();await ctx.PLAYER.nextEpisodePreparation.promise;
  assert.deepEqual(reads,["/items/1","/items/10","/items/20","/items/11","/items/2"]);
  assert.equal(ctx.PLAYER.nextEpisodePreparation.page.id,"2");
});
test("next episode preparation skips distant paused seeking and non-episode playback",()=>{
  for(const change of [h=>{h.ctx.position=10;},h=>{h.video.paused=true;},
    h=>{h.video.seeking=true;},h=>{h.ctx.PLAYER.meta.kind="movie";},
    h=>{h.ctx.PLAYER.libraryChannel={};},h=>h.ctx.setAutoNext(false)]){
    const h=nextEpisodeHarness();change(h);h.warm();assert.equal(h.reads.length,0);
  }
});


test("close seek cancellation tick cannot recreate next episode preparation",async()=>{
  const {ctx,reads,warm}=nextEpisodeHarness();
  warm();const original=ctx.PLAYER.nextEpisodePreparation;
  await original.promise;
  // Execute the real close -> cancelPendingSeek -> pbTick ordering. Stop at
  // timer teardown, after the synchronous admission edge but before UI cleanup.
  const noop=()=>{},stopped=new Error("after close seek cancellation");
  Object.assign(ctx,{WATCH_CLOSE_PROMISE:null,WATCH_GENERATION:1,
    watchDetach:()=>{ctx.WATCH=null;},PLAY_OPEN_GATE:{invalidate:noop},play:{},
    reportProgress:()=>Promise.resolve(),retirePlaybackPredecessor:noop,
    exitPresentationModes:noop,clearPlayerMediaSession:noop,
    supersedePlaybackControlIntent:p=>{ctx.cancelNextEpisodePreparation(p);p.controlIntentGeneration=1;},
    finishPlaybackSeekTelemetry:noop,releaseSession:noop,
    clearPendingSeekTimer:noop,pbTick:warm,stopPlayerTimers:()=>{throw stopped;}});
  const video={paused:false,seeking:false,classList:{remove:noop},style:{}};
  ctx.document.getElementById=()=>video;
  function shipped(file,name){
    const source=fs.readFileSync(`crates/plurxd/src/web/player/${file}`,"utf8");
    const at=source.indexOf(`function ${name}(`);assert.ok(at>=0);
    return source.slice(at,source.indexOf("\n}",at)+2);
  }
  vm.runInContext(shipped("transport.js","cancelPendingSeek"),ctx);
  vm.runInContext(shipped("stats.js","closePlayer"),ctx);
  assert.throws(()=>ctx.closePlayer(),error=>error===stopped);
  assert.equal(ctx.PLAYER.wantsPlayback,false);
  assert.equal(ctx.PLAYER.nextEpisodePreparation,null);
  assert.equal(reads.length,3,"close admitted a new metadata owner");
});

test("stopped playback intent rejects late successor metadata",async()=>{
  const {ctx,warm}=nextEpisodeHarness();let finish,signal;
  ctx.api=(_path,options)=>{signal=options.signal;return new Promise(r=>finish=r);};
  warm();const state=ctx.PLAYER.nextEpisodePreparation;
  ctx.PLAYER.wantsPlayback=false;
  assert.equal(ctx.nextEpisodePreparationCurrent(ctx.PLAYER,state),false);
  warm();
  assert.equal(signal.aborted,true);
  finish({item:{kind:"episode"}});
  await state.promise;
  assert.equal(state.page,null);
  assert.equal(ctx.PLAYER.nextEpisodePreparation,null);
});
