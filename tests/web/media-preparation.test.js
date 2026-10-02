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
