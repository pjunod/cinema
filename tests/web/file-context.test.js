"use strict";
const assert=require("node:assert/strict");
const fs=require("node:fs");
const vm=require("node:vm");
const {test}=require("node:test");
const source=fs.readFileSync("crates/plurxd/src/web/core/file-context.js","utf8");
function harness(){
  const context=vm.createContext({AUTH_GENERATION:0});
  vm.runInContext(source+"\nthis.helper={localPlaybackFileContext,sharedPlaybackFileContextFromDetail,playbackFileKey,playbackFileUrl,playbackFileApiPath,withPlaybackFileSession,playbackFileDecisionMediaUrl,playbackFileContext,logout(){AUTH_GENERATION++;}};",context);
  return context.helper;
}
const reference={import_id:"11111111-1111-4111-8111-111111111111",server_id:"22222222-2222-4222-8222-222222222222",catalogue_epoch:"33333333-3333-4333-8333-333333333333",library_id:"7",item_id:"9"};
function detail(ref=reference,locator="opaque_7-A"){return {file_base:`/api/v1/shared/imports/${ref.import_id}/files/${locator}`,id:"7"};}
test("same numeric source file cannot collide with local playback resources or source keys",()=>{
  const h=harness(),local=h.localPlaybackFileContext(7),shared=h.sharedPlaybackFileContextFromDetail(reference,detail());
  assert.equal(h.playbackFileUrl(local,"decision"),"/api/v1/files/7/decision");
  for(const suffix of ["decision","hls/sessions","direct","stream.mp4","subs/0.vtt","subs/0/overlay.json","chapters/0/thumb"]){
    const url=h.playbackFileUrl(shared,suffix);
    assert.equal(url,detail().file_base+"/"+suffix);
    assert.ok(!url.startsWith("/api/v1/files/7/"));
    assert.equal(h.playbackFileApiPath(shared,suffix),url.slice(7));
  }
  assert.notEqual(h.playbackFileKey(local),h.playbackFileKey(shared));
  const other=h.sharedPlaybackFileContextFromDetail({...reference,server_id:"44444444-4444-4444-8444-444444444444"},detail());
  assert.notEqual(h.playbackFileKey(shared),h.playbackFileKey(other));
});
test("shared detail rejects absolute, encoded, traversing, mismatched and local bases",()=>{
  const h=harness(),base=detail().file_base;
  for(const file_base of ["https://source/files/7","//source/files/7","/api/v1/files/7",base+"?session=x",base+"#x",base+"/../direct",base+"%2fthing",base+"%2e",base+"\\thing",base.replace(reference.import_id,reference.server_id),base.replace("opaque_7-A",""),base.replace("opaque_7-A","a".repeat(2049))]){
    assert.throws(()=>h.sharedPlaybackFileContextFromDetail(reference,{file_base}));
  }
  assert.throws(()=>h.sharedPlaybackFileContextFromDetail({...reference,item_id:7},detail()));
  assert.throws(()=>h.sharedPlaybackFileContextFromDetail({...reference,library_id:"9223372036854775808"},detail()));
  assert.throws(()=>h.playbackFileContext({source_ref:reference,file_base:base,id:7}));
});
test("contexts copy source authority and retain exact unsafe-for-Number source IDs",()=>{
  const h=harness(),ref={...reference,item_id:"9007199254740993"},file=detail(ref),c=h.sharedPlaybackFileContextFromDetail(ref,file);
  ref.item_id="1";file.file_base="/api/v1/files/7";
  assert.equal(c.source_ref.item_id,"9007199254740993");
  assert.ok(Object.isFrozen(c));assert.ok(Object.isFrozen(c.source_ref));
  assert.equal(h.playbackFileUrl(h.localPlaybackFileContext("9007199254740993"),"direct"),"/api/v1/files/9007199254740993/direct");
  assert.throws(()=>h.localPlaybackFileContext(9007199254740993));
});
test("file suffixes and query keys stay closed and typed",()=>{
  const h=harness(),c=h.sharedPlaybackFileContextFromDetail(reference,detail());
  for(const suffix of ["../direct","direct?url=x","/direct","subs/-1","subs/01","chapters/1000000/thumb","download","content","subs/0/overlay/../objects/x.png"])
    assert.throws(()=>h.playbackFileUrl(c,suffix));
  for(const query of [{url:"https://upstream"},{token:"secret"},{session:reference.import_id},{start:Infinity},{audio:1.1},{audio_offset_ms:15001},{vcodec:"hevc&token=x"},{force:"unknown"}])
    assert.throws(()=>h.playbackFileUrl(c,"stream.mp4",query));
  assert.equal(h.playbackFileUrl(c,"decision",{force:"auto",audio:0,subtitle:-1}),detail().file_base+"/decision?force=auto&audio=0&subtitle=-1");
  assert.equal(h.playbackFileUrl(c,"subs/0/overlay/"+"a".repeat(64)+"/objects/"+"b".repeat(64)+".png"),detail().file_base+"/subs/0/overlay/"+"a".repeat(64)+"/objects/"+"b".repeat(64)+".png");
});
test("only an exact admitted B UUID may bind headerless file delivery",()=>{
  const h=harness(),c=h.sharedPlaybackFileContextFromDetail(reference,detail()),id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",bound=h.withPlaybackFileSession(c,id);
  assert.equal(c.session_id,null);assert.equal(h.playbackFileKey(c),h.playbackFileKey(bound));
  assert.equal(h.playbackFileUrl(bound,"subs/0"),detail().file_base+"/subs/0?session="+id);
  assert.equal(h.playbackFileUrl(bound,"decision"),detail().file_base+"/decision");
  assert.equal(h.playbackFileUrl(bound,"hls/sessions"),detail().file_base+"/hls/sessions");
  for(const bad of [id.toUpperCase(),"7","../session",id.replace("4aaa","1aaa")])assert.throws(()=>h.withPlaybackFileSession(c,bad));
});

const acorn=require("../vendor/acorn.js");
const {shellSource,WEB}=require("./shell-source.js");
const path=require("node:path");
function shippedFunction(file,name){
  const text=fs.readFileSync(path.join(WEB,file),"utf8");
  const node=acorn.parse(text,{ecmaVersion:"latest"}).body.find(n=>n.type==="FunctionDeclaration"&&n.id.name===name);
  assert.ok(node,`${file}::${name} exists`);return text.slice(node.start,node.end);
}
function callerHarness(){
  const ctx=vm.createContext({requests:[],console,AUTH_GENERATION:0});
  const functions=[
    ["detail/preplay-selection.js",["prePlaySelection","playbackSelection","setPrePlay","prePlaySelectionQuery","decisionUrl","askDecision"]],
    ["player/directed-change.js",["openSession"]],
    ["player/session.js",["newStreamId","remuxUrl"]],
    ["player/audio-sync.js",["subUrl"]],
    ["detail/watch-browser.js",["watchChapterThumbUrl"]],
    ["player/stats.js",["reportProgress"]],
    ["player/autoplay-next.js",["playNextEpisode","playNextAudiobookPart","playbackContinuation"]],
  ].flatMap(([file,names])=>names.map(name=>shippedFunction(file,name))).join("\n");
  vm.runInContext(source+`\nlet PREPLAY={},PLAYER=null,STREAM_SEQ=0;
    const PLAYBACK_ID="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",SERVER=null;
    const CAPS_Q="vcodec=h264,hevc&acodec=aac&container=mp4&hdr=0&dv=0&dvprofile=&hdr10t=0";
    function prePlayPreview(){}function currentCapsDocument(){return {video:[],audio:[]};}
    function capsDocumentIsUsable(){return false;}function qualityForce(){return "auto";}
    function vodClientContract(){return {session:{}};}function newRequestId(){return "fixture-request";}
    function plannedHlsTransport(){return null;}function playbackOwnsAttachedMedia(){return true;}
    function tok(url){return url;}
    async function api(url,options){requests.push({url,options});return {};}
  `+functions+`\nthis.calls={
    shared:sharedPlaybackFileContextFromDetail,local:localPlaybackFileContext,
    bind:withPlaybackFileSession,url:playbackFileUrl,key:playbackFileKey,
    playContext:playbackFileContextForPlay,
    select:setPrePlay,selection:prePlaySelection,carry:playbackSelection,
    decision:askDecision,legacyDecision:decisionUrl,start:openSession,
    attach(c){PLAYER={fileId:"7",fileContext:c,meta:{fileContext:c},aoffset:0};},
    subtitle:subUrl,thumb:watchChapterThumbUrl,remux:remuxUrl,
    progress(){return reportProgress("7");},next:playNextEpisode,bookNext:playNextAudiobookPart,
    requests};`,ctx);
  return ctx.calls;
}
test("shipped preplay choices and every file resource caller retain A/B collision context",async()=>{
  const h=callerHarness(),shared=h.shared(reference,detail()),local=h.local("7");
  h.select(local,"audio","0");h.select(shared,"audio","1");
  assert.equal(h.selection(local).audio,0);assert.equal(h.selection(shared).audio,1);
  assert.equal(h.carry({fileId:"7",fileContext:local,preplay:{audio:2}},shared).audio,1);
  await h.decision(shared,"auto",h.selection(shared));
  assert.ok(h.requests[0].url.startsWith(detail().file_base.slice(7)+"/decision?"));
  assert.ok(h.legacyDecision(shared,"original",null).startsWith(detail().file_base.slice(7)+"/decision?"));
  await h.start(shared,{start:0});
  assert.equal(h.requests[1].url,detail().file_base.slice(7)+"/hls/sessions");
  h.attach(shared);
  assert.equal(h.subtitle(0),detail().file_base+"/subs/0");
  assert.equal(h.thumb({id:"7",fileContext:shared,mtime:123},0),detail().file_base+"/chapters/0/thumb?v=123");
  const progressive=h.remux(shared,0,4);
  assert.ok(progressive.startsWith(detail().file_base+"/stream.mp4?"));
  assert.match(progressive,/start=4\.0/);assert.match(progressive,/stream=/);
  await h.decision(local,"auto",null);
  assert.ok(h.requests[2].url.startsWith("/files/7/decision?"));
  assert.throws(()=>h.thumb({id:"7",file_base:detail().file_base},0));
  assert.throws(()=>h.thumb({id:"7",file_base:detail().file_base,fileContext:7},0));
  assert.throws(()=>h.thumb({id:"7",file_base:detail().file_base,fileContext:local},0));
});
test("shared progress and continuation cannot reach local item routes before admission adapters",async()=>{
  const h=callerHarness(),shared=h.shared(reference,detail());h.attach(shared);
  await h.progress();assert.equal(await h.next(),false);assert.equal(await h.bookNext(),false);
  assert.equal(h.requests.length,0);
  assert.throws(()=>h.playContext(7,{fileContext:shared}));
  assert.throws(()=>h.playContext("8",{fileContext:shared}));
  assert.equal(h.playContext("7",{fileContext:shared}),shared);
  const next=h.playContext("8",{fileContext:h.local("7")});
  assert.equal(next.file_base,"/api/v1/files/8");
});
test("file route census allows only explicit local administration and reader exceptions",()=>{
  const exceptions={
    "pages/reader.js":["`/files/${routeFileId}/publication`"],
    "pages/analysis.js":["`/files/${row.file_id}/analysis`"],
    "detail/track-facts.js":["`/files/${fileId}/analysis`","`/files/${id}/preparation`"],
    "detail/preplay-selection.js":["`/files/${fileId}/subtitles/search?language=${encodeURIComponent(language)}`","`/files/${fileId}/subtitles/download`","`/api/v1/files/${exactWireId(file)}/content`","`/files/${id}/dv-conversion`"],
  };
  const seen=new Set(),unexpected=[];
  for(const file of shellSource().rows.body){
    if(file==="hls.min.js"||file==="core/file-context.js")continue;
    const text=fs.readFileSync(path.join(WEB,file),"utf8");
    function walk(node){
      if(!node||typeof node!=="object")return;
      if(node.type==="TemplateLiteral"||node.type==="Literal"&&typeof node.value==="string"){
        const code=text.slice(node.start,node.end);
        if(code.includes("/files/")){
          if((exceptions[file]||[]).includes(code))seen.add(file+"::"+code);
          else unexpected.push(file+"::"+code);
        }
        // Template expressions may themselves contain unrelated nested URLs.
        if(node.type==="Literal")return;
      }
      for(const value of Object.values(node))if(Array.isArray(value))value.forEach(walk);else if(value&&typeof value==="object")walk(value);
    }
    walk(acorn.parse(text,{ecmaVersion:"latest"}));
  }
  assert.deepEqual(unexpected,[],"playback file paths must use the typed context helper");
  for(const [file,entries]of Object.entries(exceptions))for(const entry of entries)
    assert.ok(seen.has(file+"::"+entry),"stale exception: "+file+"::"+entry);
});

test("logout retires shared contexts and account-scoped selection keys",()=>{
  const h=harness(),first=h.sharedPlaybackFileContextFromDetail(reference,detail()),key=h.playbackFileKey(first);
  const local=h.localPlaybackFileContext("7");h.logout();
  assert.throws(()=>h.playbackFileUrl(first,"subs/0"));
  assert.throws(()=>h.playbackFileKey(first));
  const fresh=h.sharedPlaybackFileContextFromDetail(reference,detail());
  assert.notEqual(h.playbackFileKey(fresh),key);
  assert.equal(h.playbackFileUrl(local,"direct"),"/api/v1/files/7/direct");
});

test("translated decision media URLs cannot acquire upstream origins or unbound sessions",()=>{
  const h=harness(),c=h.sharedPlaybackFileContextFromDetail(reference,detail());
  assert.equal(h.playbackFileDecisionMediaUrl(c,detail().file_base+"/stream.mp4?audio=1"),detail().file_base+"/stream.mp4?audio=1");
  for(const url of ["https://source/files/7/direct","/api/v1/files/7/direct",detail().file_base+"/direct?token=secret",detail().file_base+"/direct?session=aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",detail().file_base+"/stream.mp4?audio=1&audio=2"])
    assert.throws(()=>h.playbackFileDecisionMediaUrl(c,url));
});

test("actual Local play and picker handlers preserve id_text above the JS integer ceiling",()=>{
  const ctx=vm.createContext({AUTH_GENERATION:0,observed:[],PREPLAY:{},PREPLAY_SCOPE_NOTE:"This playback only"});
  const names=["exactWireId","prePlayPickers","playbackMetaFor","playCall","setPrePlay","prePlaySelection"];
  vm.runInContext(source+"\n"+names.map(name=>shippedFunction("detail/preplay-selection.js",name)).join("\n")+`
    function esc(value){return String(value).replaceAll("&","&amp;").replaceAll('"',"&quot;");}
    function audioFactLabel(a){return "Track "+a.index;}function subFactLabel(){return "Subtitle";}
    function prePlayPreview(){}function play(...args){observed.push(args);}
  `,ctx);
  const exact="9007199254740993",file={id:9007199254740993,id_text:exact,available:true,
    duration_ms:123,audio_streams:[{index:0},{index:1}],subtitle_streams:[{index:0}]};
  ctx.file=file;ctx.model={item:{kind:"movie",title:"Film"},meta:{},files:[file]};
  const decode=text=>text.replaceAll("&quot;",'"').replaceAll("&amp;","&");
  const onclick=decode(vm.runInContext("playCall(model,file,0)",ctx));
  vm.runInContext(onclick,ctx);
  assert.equal(ctx.observed[0][0],exact);assert.equal(typeof ctx.observed[0][0],"string");
  const html=vm.runInContext("prePlayPickers(file)",ctx);
  const change=decode(html.match(/onchange="([^"]+)"/)[1]);
  vm.runInContext(`(function(){${change}}).call({value:"1"})`,ctx);
  assert.equal(vm.runInContext(`prePlaySelection("${exact}").audio`,ctx),1);
  assert.throws(()=>{ctx.file={...file,id_text:undefined};vm.runInContext("playCall(model,file,0)",ctx);});
});
