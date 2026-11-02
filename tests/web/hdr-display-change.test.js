"use strict";
// The HDR display claim follows the browser when `(dynamic-range: high)` changes
// after boot (Chrome on Linux/Wayland reports HDR only once its GPU process is
// up; a window moved between SDR and HDR monitors changes it too).
const assert=require("node:assert/strict"),fs=require("node:fs"),vm=require("node:vm"),{test}=require("node:test"),acorn=require("../vendor/acorn.js");
const root="crates/plurxd/src/web/";
function declaration(file,name){
  const source=fs.readFileSync(root+file,"utf8");
  const node=acorn.parse(source,{ecmaVersion:"latest"}).body.find(n=>n.type==="FunctionDeclaration"&&n.id.name===name);
  assert.ok(node,name);return source.slice(node.start,node.end);
}
function constDeclaration(file,name){
  const source=fs.readFileSync(root+file,"utf8");
  const node=acorn.parse(source,{ecmaVersion:"latest"}).body.find(n=>n.type==="VariableDeclaration"&&n.declarations.some(d=>d.id.name===name));
  assert.ok(node,name);return source.slice(node.start,node.end);
}
function harness({hdr,withListener=true,pqProbed=true}){
  const listeners=[];
  const query={matches:hdr,addEventListener:withListener?((type,fn)=>{assert.equal(type,"change");listeners.push(fn);}):undefined};
  const ctx=vm.createContext({console,
    window:{matchMedia:q=>{assert.equal(q,"(dynamic-range: high)");return query;},MediaSource:{isTypeSupported:t=>/hvc1|avc1/.test(t)}},
    document:{createElement:()=>({canPlayType:t=>/hvc1|avc1/.test(t)?"probably":""})}});
  ctx.MediaSource=ctx.window.MediaSource;
  vm.runInContext([constDeclaration("player/decode-tiers.js","HEVC_TIERS"),
    declaration("detail/dynamic-range.js","displayIsHdr"),
    declaration("player/decode-tiers.js","hevcTierSummary"),
    declaration("player/decode-tiers.js","buildPlayCaps"),
    declaration("player/decode-tiers.js","capsQuery"),
    declaration("player/decode-tiers.js","rebuildPlayCapsForDisplay"),
    declaration("player/decode-tiers.js","watchDisplayDynamicRange")].join("\n")+`
    let PLAY_HEVC_TIERS=HEVC_TIERS.map(()=>true),PLAY_HEVC_PQ_TIERS=HEVC_TIERS.map(()=>true),PLAY_HEVC_PQ_PROBED=${pqProbed};
    let PLAY_CAPS=buildPlayCaps(hevcTierSummary(PLAY_HEVC_TIERS,PLAY_HEVC_PQ_PROBED?PLAY_HEVC_PQ_TIERS:null));
    PLAY_CAPS.progressiveHevcSampleEntries=["hvc1"];PLAY_CAPS.hlsHevcSampleEntries=["hvc1"];
    let CAPS_Q=capsQuery(PLAY_CAPS);
    this.h={watch:watchDisplayDynamicRange,caps:()=>PLAY_CAPS,q:()=>CAPS_Q};`,ctx);
  return {...ctx.h,query,fire:()=>listeners.forEach(fn=>fn()),listeners};
}
test("a display that turns HDR after boot is claimed for the next session",()=>{
  const h=harness({hdr:false});
  assert.equal(h.caps().hdr10t,0);assert.match(h.q(),/&hdr=0/);
  assert.equal(h.watch(),true);
  h.query.matches=true;h.fire();
  assert.equal(h.caps().hdrDisplay,true);assert.equal(h.caps().hdr10t,1);assert.equal(h.caps().hdr,1);
  assert.match(h.q(),/&hdr=1/);assert.match(h.q(),/&hdr10t=1/);
  assert.deepEqual([...h.caps().hlsHevcSampleEntries],["hvc1"]);assert.deepEqual([...h.caps().progressiveHevcSampleEntries],["hvc1"]);
});
test("moving to an SDR display withdraws the HDR claim",()=>{
  const h=harness({hdr:true});h.watch();
  assert.equal(h.caps().hdr10t,1);
  h.query.matches=false;h.fire();
  assert.equal(h.caps().hdr10t,0);assert.equal(h.caps().hdr,0);assert.match(h.q(),/&hdr10t=0/);
});
test("a change that happened during the MediaCapabilities probe is applied at once",()=>{
  const h=harness({hdr:false});h.query.matches=true;
  h.watch();
  assert.equal(h.caps().hdr10t,1);
});
test("without a PQ probe the rebuilt claim still cannot assert HDR10 presentation",()=>{
  const h=harness({hdr:false,pqProbed:false});h.watch();
  h.query.matches=true;h.fire();
  assert.equal(h.caps().hdr,1);assert.equal(h.caps().hdr10t,0);
});
test("an unchanged answer leaves the claims object alone",()=>{
  const h=harness({hdr:true});h.watch();const before=h.caps();h.fire();
  assert.equal(h.caps(),before);
});
test("browsers without change events keep the boot answer",()=>{
  const h=harness({hdr:false,withListener:false});
  assert.equal(h.watch(),false);assert.equal(h.caps().hdr10t,0);
});
test("an admitted playback keeps its HDR claim when the browser changes its answer",()=>{
  const ctx=vm.createContext({hdrNow:false,localStorage:{getItem:()=>null}});
  vm.runInContext([declaration("player/decode-tiers.js","hdrDisplayPeakCaps"),declaration("player/decode-tiers.js","currentCapsDocument")].join("\n")+`
    const PLAY_CAPS={acodec:"aac"},SERVER={};
    function browserAudioSinks(){return [];}function browserOutputChannelsCached(){return null;}
    function capsDocument(){return {v:2,display:{hdr:hdrNow,dolby_vision:false},video:[]};}
    function decodeLimits(){return [];}function measuredPresentationTarget(){return null;}
    this.caps=currentCapsDocument;`,ctx);
  const admitted=ctx.caps();assert.equal(admitted.display.hdr,false);
  ctx.hdrNow=true;
  assert.equal(ctx.caps({capsSnapshot:admitted}).display.hdr,false);
  assert.equal(ctx.caps().display.hdr,true);
  const hdrAdmitted=ctx.caps();ctx.hdrNow=false;
  assert.equal(ctx.caps({capsSnapshot:hdrAdmitted}).display.hdr,true);
  assert.equal(ctx.caps({capsSnapshot:{display:{}}}).display.hdr,false);
});
