"use strict";
const test=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const WEB=path.join(__dirname,"../../crates/plurxd/src/web");
const surfaceSource=fs.readFileSync(path.join(WEB,"player/surface.js"),"utf8");
const surfaceHistoryDeclaration=require("../vendor/acorn.js").parse(surfaceSource,{ecmaVersion:"latest"}).body.find(node=>node.type==="FunctionDeclaration"&&node.id.name==="playbackSurfaceHistoryText");
assert.ok(surfaceHistoryDeclaration,"the shipped surface history formatter exists");
const surfaceHistorySource=surfaceSource.slice(surfaceHistoryDeclaration.start,surfaceHistoryDeclaration.end);
const FILE="9007199254740993",JOB="11111111-1111-4111-8111-111111111111",SESSION="44444444-4444-4444-8444-444444444444",CAPTION="a".repeat(64),MODEL="opus-mt-mul-en";
const config=()=>({version:1,revision:"3",enabled:false,model_id:MODEL,target_language:"en"});
const sourceOptions=()=>({version:1,file_id:FILE,source_size:"9007199254740995",source_mtime:"9007199254740997",subtitles:[{index:7,language:"en",title:"English",text_eligible:true,downloaded_subtitle_id:CAPTION}],configuration:config(),model_id:MODEL,input_languages:["ja","de"],workers:[],permissions:{read:true,create:true},server_now_ms:1000});
const translationResult=()=>({file_id:FILE,subtitle_index:3,downloaded_subtitle_id:CAPTION,language:"en"});
const decision=()=>({subtitles:[{index:7,source:"downloaded",text:true,language:"en",title:"Translated English",downloaded_subtitle_id:CAPTION}]});
const response=(data,status=200)=>new Response(JSON.stringify(data),{status,headers:{"content-type":"application/json"}});
function element(){return Object.assign(new EventTarget(),{textContent:"",innerHTML:"",hidden:false,disabled:false,isConnected:true,value:"",checked:false,children:[],dataset:{},classList:{contains:()=>false,add(){},remove(){},toggle(){}},appendChild(child){this.children.push(child);},replaceChildren(...children){this.children=children;},querySelector(selector){return this.fields?.[selector]||null;},querySelectorAll(){return [];},focus(){},scrollIntoView(){},setAttribute(){},hasAttribute(){return false;}});}
function harness(kind="local"){
  let timer=0;const timers=new Map(),calls=[],selected=[],panel=element(),setup=element(),shared=element(),video=element(),fields={};
  const schedule=(run,delay,repeat=false)=>{const id=++timer;timers.set(id,{run,delay,repeat});return id;};
  for(const name of ["status","source","language","confirm","start","stop","retry","use","worker","install","test","refresh","refresh-choices","save","save-status","enabled"])fields[`[data-text-${name}]`]=element();fields.form=element();
  for(const name of ["status","choices","use","refresh"])fields[`[data-shared-caption-${name}]`]=element();
  panel.fields=fields;setup.fields=fields;shared.fields=fields;
  const file={id:FILE,available:true,video_codec:"h264",subtitle_streams:[],audio_streams:[],filename:"fixture.mkv",audio_streams:[]};
  const nodes={"watch-subtitle-text-translation":panel,"text-subtitle-translation-settings":setup,"watch-shared-captions":shared,video,pmenu:element(),statsov:element(),player:element()};
  // Whole shipped rows register browser lifecycle listeners during loading.
  const window=new EventTarget(),document=Object.assign(new EventTarget(),{activeElement:null,getElementById:id=>nodes[id]||null,createElement:()=>element()});
  const ctx=vm.createContext({console,TextEncoder,TextDecoder,Uint8Array,Map,Set,BigInt,Response,AbortController,Date,performance:{now:()=>1000},API:"/api/v1",AUTH_GENERATION:1,TOKEN:"token-fixture",PAGE_RENDER_GENERATION:1,ME:{is_admin:true},location:{hash:"#/watch/1"},window,document,localStorage:{getItem:()=>null,setItem(){}},setTimeout:(run,delay)=>schedule(run,delay),clearTimeout:id=>timers.delete(id),setInterval:(run,delay)=>schedule(run,delay,true),clearInterval:id=>timers.delete(id),esc:value=>String(value).replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;").replaceAll('"',"&quot;"),toast(){},fmtMbps:()=>"",fmtSize:()=>"",fmtDur:()=>"",fmtBytes:()=>"",fmtChannels:()=>"",clockFromSec:()=>"00:00",langName:value=>value,audioFactLabel:()=>"Audio",subFactLabel:()=>"Subtitle",currentCapsDocument:()=>({version:2}),watchCurrentFile:()=>file,watchCanGenerateSubtitles:()=>false,watchGenerationAction:()=>"",watchSelectedAudio:()=>-1,watchSelectedSub:()=>-1,translationSelected:()=>false,playbackOwnsAttachedMedia:()=>false,playerRangeBadge:()=>null,preparedSwitchLedger:()=>({}),decodePredictionLabel:()=>"Unknown",statsRunwayTone:()=>"muted",PlaybackPolicy:{controlLeaseMode:()=>"none",controlLeasePresentation:()=>({ownership:"none",label:"none"}),surfaceEntersFailedRouting:()=>false},PLAYBACK_SURFACE:{surface:{kind:"normal"},history:[]},pbSyncSubIcon(){},watchRenderLedger(){},fetch:async(url,init)=>{calls.push({url,init});return ctx.respond(url,init);},respond:()=>response({})});
  for(const script of ["core/file-context.js","player/subtitle-translation.js","player/subtitle-text-translation.js","player/shared-subtitle-captions.js","player/audio-sync.js","detail/watch-browser.js","player/menus.js","player/stats.js","pages/settings-developer.js"])vm.runInContext(fs.readFileSync(path.join(WEB,script),"utf8"),ctx,{filename:script});
  vm.runInContext(surfaceHistorySource,ctx,{filename:"player/surface.js"});
  // Selection is the adoption boundary under observation, after row declarations.
  ctx.setSub=async index=>{selected.push(index);ctx.PLAYER.curSub=index;};
  let fileContext;
  if(kind==="local")fileContext=ctx.withPlaybackFileSession(ctx.localPlaybackFileContext(FILE),SESSION);
  else{
    const reference={import_id:JOB,server_id:"22222222-2222-4222-8222-222222222222",catalogue_epoch:"33333333-3333-4333-8333-333333333333",library_id:"7",item_id:FILE};
    const revision="b".repeat(64),detail={file_base:`/api/v1/shared/imports/${JOB}/files/${"L".repeat(236)}`,file_id:FILE,revision,reference:{item:reference,file_id:FILE,revision,lifecycle_generation:1n}};
    fileContext=ctx.sharedPlaybackFileContextFromDetail(reference,detail);
  }
  file.fileContext=fileContext;
  ctx.PLAYER={fileId:FILE,fileContext,curSub:-1,curAudio:0,audio:[{index:0}],subs:[],mediaAttachment:{},started:true};ctx.WATCH={page:{files:[file],playable:file},accepted:false};ctx.file=file;
  return {ctx,calls,selected,file,panel,setup,shared,fields};
}
function useState(h){const {ctx,panel,fields}=h;fields["[data-text-source]"].value="0";fields["[data-text-language]"].value="ja";fields["[data-text-confirm]"].checked=true;const state=ctx.textTranslationState("watch",panel);state.player=ctx.PLAYER;state.fileContext=ctx.playbackFileContextForPlayer(ctx.PLAYER);state.fileId=ctx.playbackFileContextForPlayer(ctx.PLAYER).source_ref.file_id;state.options={...sourceOptions(),subtitles:[{index:0,language:"ja",text_eligible:true}]};state.operation={job_id:JOB,source_size:"9007199254740995",source_mtime:"9007199254740997",source_index:0,source_language:"ja",selectionKey:ctx.textTranslationSelectionKey(state)};state.job={version:1,job_id:JOB,state:"succeeded",phase:"succeeded",retryable:false,progress:{completed:1,total:1},result:translationResult()};ctx.seed=state;vm.runInContext("SUBTITLE_TEXT_TRANSLATION=seed",ctx);return state;}
const settle=()=>new Promise(resolve=>setImmediate(resolve));

test("Developer setup keeps the text preference available when worker readiness is unknown",async()=>{
  const h=harness();h.ctx.respond=(url,init)=>init.method==="PUT"?response({...config(),revision:"4",enabled:true}):url.endsWith("/config")?response(config()):response({version:1,catalog:{version:1,model_id:MODEL,adapter_contract:"fixture",bundle_digest:"b".repeat(64),model_sha256:"c".repeat(64)},workers:[],server_now_ms:1000});
  await h.ctx.openTextTranslationSettings();assert.match(h.setup.innerHTML,/Enable subtitle text translation/);assert.match(h.setup.innerHTML,/Readiness does not change this saved preference/);assert.equal(vm.runInContext("TEXT_TRANSLATION_SETUP.saved.enabled",h.ctx),false);
  h.fields["[data-text-enabled]"].checked=true;await h.ctx.saveTextTranslationSettings();const save=h.calls.find(call=>call.init.method==="PUT");assert.ok(save);assert.equal(JSON.parse(save.init.body).enabled,true);
  h.ctx.setCard=(html)=>html;h.ctx.cardHead=(...args)=>args.join(" ");h.ctx.devGraduation=()=>"";
  const card=h.ctx.subtitleTranslationCard();assert.match(card,/openTextTranslationSettings\(\)/);assert.match(card,/text-subtitle-translation-settings/);
});

test("shared captions stay discoverable with no subtitle tracks and expose read-only menu controls",()=>{
  const h=harness("shared"),ledger=h.ctx.watchLedgerHtml(),menu=h.ctx.captionActionsMenu();
  assert.deepEqual(h.file.subtitle_streams,[]);assert.match(ledger,/<dt>Captions<\/dt>/);assert.match(ledger,/Available AI captions/);assert.doesNotMatch(ledger,/Translate existing subtitles/);
  assert.match(menu,/Available AI captions/);assert.doesNotMatch(menu,/openPlayerCaptionControls\('text'\)/);
  h.file.subtitle_streams=[{index:1,language:"en"}];assert.match(h.ctx.watchTranslationAction(h.file),/Available AI captions/);
  for(const suffix of ["subtitle-text-translation/options","subtitles/0/translation"])assert.throws(()=>h.ctx.playbackFileApiPath(h.ctx.PLAYER.fileContext,suffix),/Invalid playback file context/);
});

test("local player menus retain both speech-caption and text-translation entry points",()=>{
  const h=harness("local"),menu=h.ctx.captionActionsMenu();assert.match(menu,/Generate or translate speech/);assert.match(menu,/Translate existing subtitles/);
});

test("watch ledger labels selected generated and translated captions from the current owner",()=>{
  const h=harness("shared");h.ctx.captionFixture={player:h.ctx.PLAYER,task:"transcribe",outputLanguage:"ja"};vm.runInContext("SHARED_CAPTION_READER=captionFixture",h.ctx);
  assert.equal(h.ctx.watchAiCaptionLabel(),"ja · AI captions");h.ctx.captionFixture.task="translate";h.ctx.captionFixture.outputLanguage="en";
  assert.equal(h.ctx.watchAiCaptionLabel(),"en · AI translation");
});

test("a completed text job does not change subtitle selection until explicit Use",async()=>{
  const h=harness(),state=useState(h);h.ctx.respond=()=>response({version:1,job_id:JOB,state:"succeeded",phase:"succeeded",retryable:false,progress:{completed:1,total:1},result:translationResult()});
  await h.ctx.textTranslationPoll(state);assert.deepEqual(h.selected,[]);assert.equal(h.ctx.PLAYER.curSub,-1);
});

test("explicit Use resolves the stable downloaded caption ID to its current track ordinal",async()=>{
  const h=harness(),state=useState(h);h.ctx.respond=url=>url.includes("decision")?response(decision()):response(sourceOptions());
  await h.ctx.textTranslationUse();assert.deepEqual(h.selected,[7]);assert.equal(h.ctx.PLAYER.curSub,7);assert.equal(h.calls.length,3);
  assert.deepEqual(h.calls.map(call=>call.url),[`/api/v1/files/${FILE}/subtitle-text-translation/options`,`/api/v1/files/${FILE}/decision?force=auto`,`/api/v1/files/${FILE}/subtitle-text-translation/options`]);
  Object.assign(h.ctx.PLAYER,{source:{},method:"direct_play",audio:[],aoffset:0,subs:decision().subtitles});h.ctx.STATS_MODE="standard";
  h.ctx.pbTotalSec=()=>60;h.ctx.pbShownSec=()=>20;h.ctx.buildLabel=()=>"fixture";h.ctx.playbackOwnsAttachedMedia=()=>false;h.ctx.playerEnhancementRow=()=>null;
  const telemetry=h.ctx.playbackStatsTelemetry();assert.match(telemetry.subtitles,/Translated English/);assert.equal(telemetry.subtitles_note,"native");
});

test("Use refuses a changed artifact at the same ordinal",async()=>{
  const h=harness(),state=useState(h);h.ctx.respond=url=>url.includes("decision")?response(decision()):response({...sourceOptions(),subtitles:[{index:7,language:"en",text_eligible:true,downloaded_subtitle_id:"d".repeat(64)}]});
  await h.ctx.textTranslationUse();assert.deepEqual(h.selected,[]);assert.equal(h.ctx.PLAYER.curSub,-1);assert.equal(state.job.state,"succeeded");
});

test("auth and subtitle-selection changes fence late Use responses",async()=>{
  {
    const h=harness(),state=useState(h);let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const use=h.ctx.textTranslationUse();await settle();h.ctx.AUTH_GENERATION++;finish(response(decision()));await use;assert.deepEqual(h.selected,[]);assert.equal(h.ctx.PLAYER.curSub,-1);assert.equal(state.using,false);
  }
  {
    const h=harness(),state=useState(h);let finish;h.ctx.respond=url=>url.includes("decision")?response(decision()):new Promise(resolve=>finish=resolve);const use=h.ctx.textTranslationUse();await settle();h.fields["[data-text-language]"].value="de";finish(response(sourceOptions()));await use;assert.deepEqual(h.selected,[]);assert.equal(h.ctx.PLAYER.curSub,-1);assert.equal(state.using,false);
  }
  {
    const h=harness(),state=useState(h);let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const use=h.ctx.textTranslationUse();await settle();h.ctx.PLAYER.fileId="42";finish(response(decision()));await use;assert.deepEqual(h.selected,[]);assert.equal(h.ctx.PLAYER.curSub,-1);assert.equal(state.using,false);
  }
});
