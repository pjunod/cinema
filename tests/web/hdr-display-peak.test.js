"use strict";
const assert=require("node:assert/strict"),fs=require("node:fs"),vm=require("node:vm"),{test}=require("node:test"),acorn=require("../vendor/acorn.js");
const root="crates/plurxd/src/web/";
function declaration(file,name){
  const source=fs.readFileSync(root+file,"utf8");
  const node=acorn.parse(source,{ecmaVersion:"latest"}).body.find(n=>n.type==="FunctionDeclaration"&&n.id.name===name);
  assert.ok(node,name);return source.slice(node.start,node.end);
}
function harness(initial){
  const values=new Map(initial==null?[]:[["plurx.hdrDisplayPeak",String(initial)]]),requests=[];
  const ctx=vm.createContext({values,requests,console,localStorage:{getItem:k=>values.get(k)??null,setItem:(k,v)=>values.set(k,v),removeItem:k=>values.delete(k)},document:{getElementById:()=>({textContent:""})},toast:()=>{}});
  vm.runInContext(`const PLAY_CAPS={},SERVER={};
    function browserAudioSinks(){return [];}function browserOutputChannelsCached(){return null;}
    function capsDocument(){return {v:2,display:{hdr:true,dolby_vision:false},video:[]};}
    function decodeLimits(){return [];}function measuredPresentationTarget(){return null;}
    function qualityForce(){return "auto";}function prePlaySelectionQuery(){return "";}function playbackFileContext(){return {source_ref:{kind:"local"}};}
    function playbackFileQueryFromLegacy(){return {};}function playbackFileApiPath(){return "/decision";}
    async function api(path,options){requests.push(options.body.caps);return {};}
    `+[declaration("player/decode-tiers.js","hdrDisplayPeakCaps"),declaration("player/decode-tiers.js","currentCapsDocument"),declaration("player/decode-tiers.js","decideForPlay"),declaration("pages/settings-playback.js","saveHdrDisplayPeak"),declaration("detail/preplay-selection.js","askDecision")].join("\n")+`
    this.h={caps:currentCapsDocument,save:saveHdrDisplayPeak,decision:askDecision,fullDecision:decideForPlay};`,ctx);
  return {...ctx.h,values,requests,ctx};
}
test("browser Auto and malformed storage preserve Unknown rather than infer nits",()=>{
  for(const value of [null,"auto","garbage","0","5000","Infinity","NaN","1600.5"]){
    const h=harness(value),display=h.caps().display;
    assert.equal(display.max_nits,undefined);assert.equal(display.peak_source,undefined);
  }
});
test("viewer presets persist locally and the next decision reports their provenance",async()=>{
  const h=harness();
  for(const value of [400,600,1000,1600,2000,4000]){
    h.save({value:String(value)});const d=await h.decision(1,"auto",null);
    assert.equal(d._capsSnapshot.display.max_nits,value);
    assert.equal(h.requests.at(-1).display.peak_source,"viewer");
  }
  h.save({value:"auto"});assert.equal(h.caps().display.max_nits,undefined);
});
test("incumbent re-decisions retain admitted peak including Unknown while new playback changes",async()=>{
  for(const value of [null,600]){
    const h=harness(value),first=await h.decision(1,"auto",null),incumbent={capsSnapshot:first._capsSnapshot};
    h.save({value:"1600"});
    const successor=await h.decision(1,"auto",null,null,incumbent);
    assert.equal(successor._capsSnapshot.display.max_nits,value??undefined);
    assert.equal(successor._capsSnapshot.display.peak_source,value==null?undefined:"viewer");
    assert.equal((await h.decision(1,"auto",null))._capsSnapshot.display.max_nits,1600);
  }
});
test("blocked browser storage stays Unknown and reports a failed save",()=>{
  const h=harness();vm.runInContext('localStorage.getItem=()=>{throw new Error("blocked")};localStorage.setItem=()=>{throw new Error("blocked")}',h.ctx);
  assert.equal(h.caps().display.max_nits,undefined);const control={value:"1000"};h.save(control);assert.equal(control.value,"auto");
});
test("comparison clip is an embedded hvc1 MP4 with BT2020 PQ limited-range nclx tags",()=>{
  const source=fs.readFileSync(root+"pages/settings-playback.js","utf8"),match=source.match(/const HDR_PEAK_PATTERN_URI="data:video\/mp4;base64,([A-Za-z0-9+/=]+)";/);
  assert.ok(match);const bytes=Buffer.from(match[1],"base64");assert.ok(bytes.length<20000);
  assert.equal(bytes.toString("ascii",4,8),"ftyp");assert.ok(bytes.includes(Buffer.from("hvc1")));
  const at=bytes.indexOf(Buffer.from("nclx"));assert.ok(at>0);
  assert.equal(bytes.readUInt16BE(at+4),9);assert.equal(bytes.readUInt16BE(at+6),16);assert.equal(bytes.readUInt16BE(at+8),9);assert.equal(bytes[at+10]&128,0);
});

test("full-play quality reload passes its captured peak to the decision",async()=>{
  const h=harness(600),captured={capsSnapshot:h.caps()};h.save({value:"1600"});
  const result=await h.fullDecision({fileId:1,peakIncumbent:captured,selection:{},sessionAudioOffset:0,
    preparation:{run:fn=>fn(null)},openIsCurrent:()=>true,failPreparation:error=>{throw error;}});
  assert.equal(result.decision._capsSnapshot.display.max_nits,600);
});


test("same-title display peak freezes while open and refreshes after Close",async()=>{
  for(const value of [null,600]){
    const h=harness(value),first=await h.decision(1,"auto",null);
    h.ctx.incumbent={fileId:1,capsSnapshot:first._capsSnapshot};
    vm.runInContext(`let PLAYER=incumbent,PENDING_DEFAULT_SUB_OFF=false;
      const modal={open:true,classList:{contains:()=>modal.open,remove:()=>{modal.open=false;}}};
      document.getElementById=id=>id==="modal"?modal:{};
      const performance={now:()=>0},PlaybackPolicy={bandwidthSeedBps:()=>null};
      function takePlaybackAttemptReason(){return null;}
      function playbackFileContextForPlayer(player){return player.fileId;}
      function playbackFileKey(file){return file;}
      function playbackSelection(){return {};}
      function wirePlayer(){}function watchPrepare(){return null;}
      `+declaration("core/file-context.js","samePlaybackFile")+"\n"+
      declaration("player/decode-tiers.js","capturePlayInputs")+`
      this.capture=()=>capturePlayInputs(1,{},null);
      this.closePresentation=()=>modal.classList.remove("open");
      this.captureRetry=peakIncumbent=>capturePlayInputs(1,{}, {peakIncumbent});`,h.ctx);
    h.save({value:"1600"});
    const decide=inputs=>h.fullDecision({...inputs,fileId:1,
      preparation:{run:fn=>fn(null)},openIsCurrent:()=>true,failPreparation:error=>{throw error;}});
    const activeInputs=h.ctx.capture();
    const active=await decide(activeInputs);
    assert.equal(active.decision._capsSnapshot.display.max_nits,value??undefined);
    assert.equal(active.decision._capsSnapshot.display.peak_source,value==null?undefined:"viewer");
    // Close removes the modal's open state while deliberately retaining PLAYER
    // and its file identity/caps. Reopening this title is a new presentation.
    h.ctx.closePresentation();
    const reopened=await decide(h.ctx.capture());
    assert.equal(reopened.decision._capsSnapshot.display.max_nits,1600);
    assert.equal(reopened.decision._capsSnapshot.display.peak_source,"viewer");
    const retry=await decide(h.ctx.captureRetry(activeInputs.peakIncumbent));
    assert.equal(retry.decision._capsSnapshot.display.max_nits,value??undefined);
  }
});
