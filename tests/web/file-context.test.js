"use strict";
const assert=require("node:assert/strict");
const fs=require("node:fs");
const vm=require("node:vm");
const {test}=require("node:test");
const source=fs.readFileSync("crates/plurxd/src/web/core/file-context.js","utf8");
function harness(){
  const context=vm.createContext({AUTH_GENERATION:0});
  vm.runInContext(source+"\nthis.helper={sharedPlaybackDirectStartContext,sharedPlaybackStatusMetrics,localPlaybackFileContext,sharedPlaybackFileContextFromDetail,playbackFileKey,playbackFileUrl,playbackFileApiPath,withPlaybackFileSession,playbackFileDecisionMediaUrl,playbackFileContext,logout(){AUTH_GENERATION++;}};",context);
  return context.helper;
}
const reference={import_id:"11111111-1111-4111-8111-111111111111",server_id:"22222222-2222-4222-8222-222222222222",catalogue_epoch:"33333333-3333-4333-8333-333333333333",library_id:"7",item_id:"9"};
function detail(ref=reference,locator="L".repeat(236)){return {file_base:`/api/v1/shared/imports/${ref.import_id}/files/${locator}`,file_id:"7",revision:"a".repeat(64),reference:{item:{...ref},file_id:"7",revision:"a".repeat(64),lifecycle_generation:1n}};}
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
  const other=h.sharedPlaybackFileContextFromDetail({...reference,server_id:"44444444-4444-4444-8444-444444444444"},detail({...reference,server_id:"44444444-4444-4444-8444-444444444444"}));
  assert.notEqual(h.playbackFileKey(shared),h.playbackFileKey(other));
});
test("shared detail rejects absolute, encoded, traversing, mismatched and local bases",()=>{
  const h=harness(),base=detail().file_base;
  for(const file_base of ["https://source/files/7","//source/files/7","/api/v1/files/7",base+"?session=x",base+"#x",base+"/../direct",base+"%2fthing",base+"%2e",base+"\\thing",base.replace(reference.import_id,reference.server_id),base.replace("L".repeat(236),""),base.replace("L".repeat(236),"a".repeat(237))]){
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
function callerHarness(capQuery="vcodec=h264,hevc&acodec=aac&container=mp4&hdr=0&dv=0&dvprofile=&hdr10t=0"){
  const ctx=vm.createContext({requests:[],console,AUTH_GENERATION:0});
  const functions=[
    ["detail/preplay-selection.js",["prePlaySelection","playbackSelection","setPrePlay","prePlaySelectionQuery","decisionUrl","askDecision"]],
    ["player/directed-change.js",["openSession"]],
    ["player/session.js",["newStreamId","remuxUrl"]],
    ["player/audio-sync.js",["subUrl"]],
    ["detail/watch-browser.js",["watchChapterThumbUrl"]],
    ["player/stats.js",["reportProgress"]],
    ["player/autoplay-next.js",["playNextEpisode","playNextSharedEpisode","playNextAudiobookPart","playbackContinuation"]],
  ].flatMap(([file,names])=>names.map(name=>shippedFunction(file,name))).join("\n");
  vm.runInContext(source+`\nlet PREPLAY={},PLAYER=null,STREAM_SEQ=0;
    const PLAYBACK_ID="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",SERVER=null;
    const CAPS_Q=${JSON.stringify(capQuery)};
    function prePlayPreview(){}function currentCapsDocument(){return {video:[],audio:[]};}
    function capsDocumentIsUsable(){return false;}function qualityForce(){return "auto";}
    function vodClientContract(){return {session:{}};}function newRequestId(){return "fixture-request";}
    function plannedHlsTransport(){return null;}function playbackOwnsAttachedMedia(){return true;}
    function tok(url){return url;}
    async function api(url,options){requests.push({url,options});return {};}
    // URL-routing seam only; complete synthetic protocol responses are tested
    // through the actual Shared decision module in shared-decision.test.js.
    const SHARED_DECISION={
      decision(c,q,signal){return api(playbackFileApiPath(c,"decision",q),{signal});},
      start(c,body,signal){return api(playbackFileApiPath(c,"hls/sessions"),{body,signal});}
    };
  `+functions+`\nthis.calls={
    shared:sharedPlaybackFileContextFromDetail,local:localPlaybackFileContext,
    bind:withPlaybackFileSession,url:playbackFileUrl,key:playbackFileKey,
    playContext:playbackFileContextForPlay,
    select:setPrePlay,selection:prePlaySelection,carry:playbackSelection,
    decision:askDecision,legacyDecision:decisionUrl,start:openSession,
    attach(c){PLAYER={fileId:"7",fileContext:c,meta:{fileContext:c},aoffset:0};},
    subtitle:subUrl,thumb:watchChapterThumbUrl,remux:remuxUrl,
    progress(){return reportProgress("7");},next:playNextEpisode,bookNext:playNextAudiobookPart,
    contract(session){vodClientContract=()=>({session});},
    stub(name,fn){globalThis[name]=fn;},
    attachShared(c,sharedReference){PLAYER={fileId:"7",fileContext:c,meta:{fileContext:c,kind:"episode",sharedReference},aoffset:0};},
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

test("shipped remux and session-start callers preserve legitimate full engine capability fields",async()=>{
  const query="client=web&device=Cinema%20browser&profile=directplay-any&vcodec=hevc,h264&vmaxheight=h264:1080,hevc:2160&acodec=aac,eac3&container=mkv,mp4&hdr=1&dv=1&dvprofile=5,8&dvhls=1&hdr10t=1&maxheight=2160";
  const h=callerHarness(query),local=h.local("7");h.attach(local);
  const remux=new URL(h.remux(local,1,2),"http://cinema.invalid");
  const original=new URLSearchParams(query);
  for(const [key,value]of original)assert.equal(remux.searchParams.get(key),value,key);
  assert.equal(remux.searchParams.get("audio"),"1");assert.equal(remux.searchParams.get("start"),"2.0");
  const caps={video:[{codec:"hevc",profiles:["main10"],max_height:2160}],audio:[{codec:"eac3"}],containers:["mkv","mp4"]};
  await h.start(local,{start:2,copy:true,caps,profile:"directplay-any",client:"web"});
  const body=h.requests[0].options.body;
  assert.deepEqual(JSON.parse(JSON.stringify(body.caps)),caps);assert.equal(body.profile,"directplay-any");assert.equal(body.copy,true);
  const shared=h.shared(reference,detail());h.attach(shared);
  const remote=new URL(h.remux(shared,1,2),"http://cinema.invalid");
  assert.ok(remote.pathname.startsWith(detail().file_base+"/stream.mp4"));
  for(const [key,value]of original)assert.equal(remote.searchParams.get(key),value,key);
  const extra={achannels:6,capver:"2",hdrtypes:"1,2",dvdecoders:"decoder-1",dvraw:"5,8",dvstatus:"proven"};
  assert.ok(h.url(shared,"decision",extra).includes("achannels=6"));
  assert.throws(()=>h.url(shared,"stream.mp4",extra));
  assert.throws(()=>h.url(shared,"decision",{achannels:17}));
  assert.throws(()=>h.url(shared,"stream.mp4",{vmaxheight:"hevc:999999"}));
});

test("Shared initial route is direct only for an actual direct decision, else session-first HLS refusing burn HDR or absent capability",()=>{
 const ctx=vm.createContext({AUTH_GENERATION:0,window:{Hls:{isSupported:()=>true}},PLAYER:{},failed:[],ROUTE:"direct"});
 vm.runInContext(source+`
 const Hls=window.Hls;
 const useNativeHls=()=>false,segmentedRemuxOk=()=>true,copyHlsMseOk=()=>true,noSegments=()=>false;
 const playbackInitialRoute=()=>ROUTE;
 `+shippedFunction("player/decode-tiers.js","choosePlayRoute")+`
 this.run=(c,method,grade,burn)=>choosePlayRoute({fileContext:c,video:{},sessionAudioOffset:0,libraryChannel:false,failPreparation:e=>failed.push(e.code)},{decision:{method,delivered_dynamic_range:grade}},{preBurn:burn},null);
 this.shared=sharedPlaybackFileContextFromDetail;this.local=localPlaybackFileContext;`,ctx);
 const c=ctx.shared(reference,detail());
 // The browser plays the original container: the Shared route is direct,
 // whatever the grade, because raw bytes need no Source HDR capability.
 assert.equal(ctx.run(c,"direct_play","sdr",null),"shared_direct");assert.equal(ctx.run(c,"direct_play","hdr10",null),"shared_direct");
 assert.equal(ctx.run(ctx.local("7"),"direct_play","sdr",null),"direct");
 // A direct decision the browser cannot take as a raw file (a non-default
 // audio track) is Copy HLS, never a progressive remux, for a Shared file.
 ctx.ROUTE="progressive_remux";assert.equal(ctx.run(c,"direct_play","sdr",null),"copy_hls");ctx.ROUTE="direct";
 assert.equal(ctx.run(c,"direct_play","sdr",2),null);
 assert.equal(ctx.run(c,"transcode","sdr",null),"transcode_hls");
 assert.equal(ctx.run(c,"transcode","hdr10",null),null);assert.equal(ctx.run(c,"remux","sdr",0),null);
 ctx.window.Hls.isSupported=()=>false;assert.equal(ctx.run(c,"transcode","sdr",null),null);
 assert.deepEqual(Array.from(ctx.failed),Array(4).fill("sharing_start_unsupported"));
});
test("shared status metrics are read only from the bound Shared grammar",()=>{
  const h=harness(),id="55555555-5555-4555-8555-555555555555";
  const started=h.withPlaybackFileSession(h.sharedPlaybackFileContextFromDetail(reference,detail()),id);
  const status={target_height:720,http_wait_count:2,active_encode_milli_realtime:1500};
  const reply={subject:"shared",reference:{item:{...reference},file_id:"7",revision:"a".repeat(64),lifecycle_generation:1},
    session_id:id,incarnation_id:"66666666-6666-4666-8666-666666666666",control_epoch:1,status};
  assert.equal(h.sharedPlaybackStatusMetrics(started,reply),status);
  for(const wrong of [
    {...reply,subject:"local"},
    {...reply,session_id:"77777777-7777-4777-8777-777777777777"},
    {...reply,reference:{...reply.reference,file_id:"8"}},
    {...reply,reference:{...reply.reference,revision:"b".repeat(64)}},
    {...reply,reference:{...reply.reference,item:{...reference,item_id:"10"}}},
    {...reply,status:null},
    status,
  ]) assert.equal(h.sharedPlaybackStatusMetrics(started,wrong),null);
  assert.equal(h.sharedPlaybackStatusMetrics(h.sharedPlaybackFileContextFromDetail(reference,detail()),reply),null);
  assert.equal(h.sharedPlaybackStatusMetrics(h.localPlaybackFileContext(7),reply),null);
});

function directReply(id){return {presentation:"direct",session_id:id,url:`${detail().file_base}/direct?session=${id}`,length:4096,mime:"video/mp4"};}
test("a Shared direct Start reply binds only its own file alias and session",()=>{
  const h=harness(),c=h.sharedPlaybackFileContextFromDetail(reference,detail()),id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
  const bound=h.sharedPlaybackDirectStartContext(c,directReply(id));
  assert.equal(bound.session_id,id);assert.equal(c.session_id,null);assert.equal(h.playbackFileKey(bound),h.playbackFileKey(c));
  assert.equal(h.playbackFileUrl(bound,"direct"),directReply(id).url);
  for(const mutate of [r=>r.presentation="vod",r=>r.url=r.url.replace(id,"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),r=>r.url="/api/v1/files/7/direct?session="+id,
    r=>r.url+="&token=x",r=>r.url=r.url.replace("/direct?","/stream.mp4?"),r=>r.url="https://source"+r.url,r=>r.session_id=id.toUpperCase(),
    r=>r.length=-1,r=>r.length=Number.MAX_SAFE_INTEGER+1,r=>r.length="4096",r=>r.mime="text/html",r=>r.mime=undefined,
    r=>r.extra=1,r=>delete r.length,r=>r.playlist_url=`/api/v1/hls/${id}/master.m3u8`])
  {const r=directReply(id);mutate(r);assert.throws(()=>h.sharedPlaybackDirectStartContext(c,r));}
  assert.throws(()=>h.sharedPlaybackDirectStartContext(bound,directReply(id)));
  assert.throws(()=>h.sharedPlaybackDirectStartContext(h.localPlaybackFileContext(7),directReply(id)));
});
test("shipped Start caller asks Shared direct play without HLS-only or subtitle fields",async()=>{
  const h=callerHarness(),shared=h.shared(reference,detail());h.attach(shared);
  h.contract({presentation:"vod",block_budget_secs:8});
  await h.start(shared,{presentation:"direct",start:3,audio:0,native_subtitles:true,subtitle:1,copy:true,height:720});
  const r=h.requests[0];assert.equal(r.url,detail().file_base.slice(7)+"/hls/sessions");
  assert.equal(r.options.body.presentation,"direct");assert.equal(r.options.body.start,3);assert.equal(r.options.body.audio,0);
  for(const field of ["block_budget_secs","transport","height","copy","native_subtitles","subtitle"])assert.ok(!Object.hasOwn(r.options.body,field),field);
  await h.start(shared,{start:3,copy:true});assert.equal(h.requests[1].options.body.presentation,"vod");
});
test("Shared direct attachment polls no status, starts no control, DELETEs its B session and restarts once",async()=>{
  const ctx=vm.createContext({AUTH_GENERATION:0,console});
  const functions=[["player/directed-change.js",["releaseSession","releaseSharedDirect","attachSharedDirect","restartSharedDirectAfterError"]],
    ["player/decode-margin.js",["beginPlaybackMediaAttachment"]],["player/stats.js",["pollSessionHealth"]]]
    .flatMap(([file,names])=>names.map(name=>shippedFunction(file,name))).join("\n");
  vm.runInContext(source+"\n"+functions+`
    const API="/api/v1",TOKEN="t",log=[],deletes=[],apis=[],changes=[];let PLAYER=null;
    const document={getElementById(){return null;}};
    function fetch(url,o){deletes.push({url,method:o.method,keepalive:o.keepalive});return Promise.resolve();}
    async function api(url){apis.push(url);return {};}
    function playbackOwnsAttachedMedia(p){return !!p;}function playbackWaitSurfaceLive(){return false;}function updateStats(){}
    function stopPlaybackControl(){log.push("stop-control");}function startPlaybackControl(){log.push("start-control");}
    function renderPlayerInfo(){}function resetMediaSource(){log.push("reset");}function setPlaybackMediaSource(v,url){v.src=url;}
    function markPlaybackControlSeekExecuted(){}function applyPlaybackTransportIntent(){}
    function applyPlaybackAttachmentPosition(v,t,a,at){if(a.current())log.push("position:"+at);}
    function playbackSurfaceStep(){}function playbackSurfaceGeneration(){return 1;}
    function positionForPlaybackIntent(v){return v.currentTime;}function beginPlaybackControlSeek(){}
    function clientLog(){}function playbackContext(){return {};}
    function requestPlaybackMediaChange(p,change){changes.push(change);return Promise.resolve(true);}
    this.t={shared:sharedPlaybackFileContextFromDetail,local:localPlaybackFileContext,bind:sharedPlaybackDirectStartContext,
      attach:attachSharedDirect,restart:restartSharedDirectAfterError,release:releaseSharedDirect,poll:pollSessionHealth,
      set(p){PLAYER=p;},log,deletes,apis,changes};`,ctx);
  const t=ctx.t,c=t.shared(reference,detail()),sid="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",sid2="bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
  const info=id=>{const r=directReply(id);Object.defineProperty(r,"_sharedContext",{value:t.bind(c,r)});return r;};
  const p={fileId:"7",fileContext:c,meta:{fileContext:c},method:"direct_play",sessionId:null,streamId:null};t.set(p);
  const v={src:"",currentTime:42,onloadedmetadata:null};
  assert.equal(t.attach(v,p,info(sid),42),42);
  assert.equal(v.src,directReply(sid).url);assert.equal(p.sessionId,null);assert.equal(p.vod,false);assert.equal(p.copyHls,false);
  assert.equal(p.fileContext.session_id,sid);assert.equal(p.meta.fileContext,p.fileContext);assert.equal(p.sharedDirect.session_id,sid);
  assert.ok(t.log.includes("stop-control"));assert.ok(!t.log.includes("start-control"));
  await t.poll(true);assert.equal(t.apis.length,0);
  // No restart before the attachment reached a timeline, nor on a decode or
  // format error, which keep the compatibility rescue.
  assert.equal(t.restart(v,p,2),false);
  v.onloadedmetadata();assert.ok(t.log.includes("position:42"));
  assert.equal(t.restart(v,p,3),false);assert.equal(t.restart(v,p,4),false);
  v.currentTime=1234.5;assert.equal(t.restart(v,p,2),true);assert.equal(t.restart(v,p,2),false);
  assert.equal(t.changes.length,1);assert.equal(t.changes[0].sharedDirect,true);assert.equal(t.changes[0].method,"direct_play");
  assert.equal(t.deletes.length,0);
  // The fresh session's attachment retires the unreadable one exactly once,
  // and fails before metadata without another restart.
  t.attach(v,p,info(sid2),1234.5);
  assert.deepEqual(Array.from(t.deletes,d=>[d.url,d.method,d.keepalive]),[[`/api/v1/hls/${sid}`,"DELETE",true]]);
  assert.equal(p.sharedDirect.session_id,sid2);assert.equal(t.restart(v,p,2),false);assert.equal(t.changes.length,1);
  await t.poll(true);assert.equal(t.apis.length,0);
  // Stop releases the live session once.
  t.release(p);t.release(p);
  assert.deepEqual(Array.from(t.deletes,d=>d.url),[`/api/v1/hls/${sid}`,`/api/v1/hls/${sid2}`]);assert.equal(p.sharedDirect,null);
  // An unbound, retargeted or HLS reply, and a Local player, are refused.
  const q={fileId:"7",fileContext:c,meta:{}};t.set(q);
  assert.throws(()=>t.attach(v,q,{...directReply(sid)},0));
  const moved=info(sid);moved.url=detail().file_base+"/direct";assert.throws(()=>t.attach(v,q,moved,0));
  const hls=info(sid);hls.presentation="vod";assert.throws(()=>t.attach(v,q,hls,0));
  const local={fileId:"7",fileContext:t.local("7"),meta:{}};t.set(local);assert.throws(()=>t.attach(v,local,info(sid),0));
  assert.equal(t.deletes.length,2);
  // Every owner that ends a player or its media releases the direct session.
  assert.match(shippedFunction("player/stats.js","closePlayer"),/releaseSharedDirect\(PLAYER\)/);
  assert.match(shippedFunction("player/decode-margin.js","retirePlaybackPredecessor"),/releaseSharedDirect\(predecessor\)/);
  assert.match(shippedFunction("player/decode-tiers.js","preparePlayOutgoing"),/releaseSharedDirect\(outgoing\)/);
});
test("Shared next episode dispatches to the Source-order resolver and a fresh authorized start, never a Local route",async()=>{
  const h=callerHarness(),shared=h.shared(reference,detail()),calls=[];
  const next={...reference,item_id:"9007199254740994"};
  h.stub("toast",()=>{});
  h.stub("beginPlaybackPreparation",()=>({run:fn=>fn(null),finish(){calls.push("finish");}}));
  h.stub("sharedCatalogueNextEpisode",async(ref,read)=>{calls.push(["resolve",ref.item_id]);await read("/shared/imports/"+ref.import_id+"/items/"+ref.item_id);return next;});
  h.stub("sharedCatalogueLaunch",async(ref,file,current)=>{calls.push(["launch",ref.item_id,file,current()]);});
  h.attachShared(shared,reference);
  assert.equal(await h.next(),true);
  assert.deepEqual(calls,[["resolve","9"],"finish",["launch","9007199254740994",null,true]]);
  assert.deepEqual(h.requests.map(r=>r.url),["/shared/imports/"+reference.import_id+"/items/9"]);
  // The end of the series, or a resolver refusal, ends without any start.
  calls.length=0;h.stub("sharedCatalogueNextEpisode",async()=>null);
  assert.equal(await h.next(),false);assert.deepEqual(calls,["finish"]);
  calls.length=0;h.stub("sharedCatalogueNextEpisode",async()=>{throw new Error("Shared source changed");});
  assert.equal(await h.next(),false);assert.deepEqual(calls,["finish"]);
});
