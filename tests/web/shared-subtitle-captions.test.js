"use strict";
const test=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const {webcrypto,createHash}=require("node:crypto"),WEB=path.join(__dirname,"../../crates/plurxd/src/web");
const A="a".repeat(64),B="b".repeat(64),P="c".repeat(64),M="d".repeat(64),CONTENT="e".repeat(64),ENGINE="f".repeat(64),LEASE="44444444-4444-4444-8444-444444444444",SESSION="55555555-5555-4555-8555-555555555555";
const reference={import_id:"11111111-1111-4111-8111-111111111111",server_id:"22222222-2222-4222-8222-222222222222",catalogue_epoch:"33333333-3333-4333-8333-333333333333",library_id:"9007199254740993",item_id:"9"};
const asset=(fields={})=>({asset_id:A,profile_id:P,model_id:"caption-model",model_sha256:M,content_revision:CONTENT,audio_index:3,spoken_language:"ja",output_language:"ja",task:"transcribe",duration_ms:120000,...fields});
const response=(value,status=200)=>new Response(value instanceof Uint8Array?value:JSON.stringify(value),{status,headers:{"content-type":"application/json"}});
function node(){return {children:[],listeners:{},isConnected:true,disabled:false,hidden:false,textContent:"",innerHTML:"",addEventListener(k,f){this.listeners[k]=f;},removeEventListener(k,f){if(this.listeners[k]===f)delete this.listeners[k];},appendChild(c){this.children.push(c);return c;},replaceChildren(){this.children=[];this.textContent="";},querySelector(k){return this.fields?.[k]||null;},scrollIntoView(){}};}
function harness({seed=true}={}){
 let now=1000,timerId=0;const timers=new Map(),calls=[],events=[],panel=node(),video=node(),fields={};
 for(const key of ["status","choices","use","refresh"])fields[`[data-shared-caption-${key}]`]=node();panel.fields=fields;fields["[data-shared-caption-use]"].disabled=true;
 video.currentTime=5;video.playbackRate=1;video.paused=true;video.addTextTrack=()=>({mode:"disabled",cues:[],addCue(cue){this.cues.push(cue);},removeCue(cue){this.cues.splice(this.cues.indexOf(cue),1);}});
 const nodes={video,"watch-shared-captions":panel};
 const ctx=vm.createContext({console,TextEncoder,TextDecoder,Uint8Array,Response,AbortController,URLSearchParams,WeakSet,Map,Set,BigInt,Date,crypto:webcrypto,performance:{now:()=>now},API:"/api/v1",AUTH_GENERATION:1,TOKEN:"viewer-bearer",PAGE_RENDER_GENERATION:1,ME:{is_admin:false},location:{hash:"#/watch/shared"},window:{VTTCue:class{constructor(start,end,text){this.startTime=start;this.endTime=end;this.text=text;}}},document:{getElementById:id=>nodes[id]||null,createElement:()=>node()},setTimeout:(fn,ms)=>{const id=++timerId;timers.set(id,{fn,ms});return id;},clearTimeout:id=>timers.delete(id),toast:message=>events.push(message),pbSyncSubIcon(){},watchRenderLedger(){},fetch:async(url,init)=>{calls.push({url,init});return ctx.respond(url,init);},respond:()=>{throw Error("Unexpected request");},setSub:async()=>{const player=ctx.PLAYER;player.captionIntent={kind:"off"};player.captionIntentSerial=(player.captionIntentSerial||0)+1;}});
 for(const relative of ["core/file-context.js","core/diagnostics-hash.js","player/subtitle-translation.js","player/shared-subtitle-captions.js"])vm.runInContext(fs.readFileSync(path.join(WEB,relative),"utf8"),ctx);
 const detail={file_base:`/api/v1/shared/imports/${reference.import_id}/files/${"L".repeat(236)}`,file_id:"7",revision:"9".repeat(64),reference:{item:{...reference},file_id:"7",revision:"9".repeat(64),lifecycle_generation:1n}};
 const fileContext=ctx.withPlaybackFileSession(ctx.sharedPlaybackFileContextFromDetail(reference,detail),SESSION);
 const player={fileContext,fileId:"7",started:true,offset:0,audio:[{index:3,language:"jpn"}],curAudio:0,curSub:-1,mediaAttachment:{audioOrdinal:3,captionPositionReady:true},captionIntent:{kind:"off"},captionIntentSerial:0};ctx.PLAYER=player;
 const state={capture:ctx.translationCapture(),player,video,panel,sourceKey:ctx.playbackFileKey(fileContext),audioIndex:3,attachment:player.mediaAttachment,controllers:new Set(),listeners:[],assetId:A,asset:asset(),profile:{profile_id:P,model_id:"caption-model",model_sha256:M,task:"transcribe"},contentRevision:CONTENT,spokenLanguage:"ja",outputLanguage:"ja",task:"transcribe",segments:new Map(),cues:new Map(),coverage:[],windowGeneration:0,requestGeneration:0,positionReady:true,presentationValidated:false,retired:false,busy:false,error:null};
 state.current=()=>ctx.sharedCaptionCurrent(state);state.segmentRead=(ref,lease)=>ctx.translationRequest(state,ctx.sharedCaptionPath(state,`captions/${state.assetId}/segments/${ref.segment_id}`,{lease:lease.id}),{max:16384,raw:true});
 ctx.seed=state;vm.runInContext("seed.limits=SHARED_CAPTION_LIMITS;SHARED_CAPTION_READER="+(seed?"seed":"null"),ctx);
 return {ctx,state,player,video,panel,fields,timers,calls,events,clock:value=>now=value,fire(id){const t=timers.get(id);timers.delete(id);t?.fn();},fileContext,detail};
}
function manifest(fields={}){return {version:1,asset_id:A,content_revision:CONTENT,profile_id:P,audio_index:3,spoken_language:"ja",output_language:"ja",task:"transcribe",duration_ms:120000,manifest_revision:"9007199254740993",state:"stopped",reason:null,snapshot:{lease_id:LEASE,revision:"9007199254740993",expires_in_ms:30000},coverage:[{start_ms:0,end_ms:30000}],segments:[],...fields};}
function segment(text="A & B <b>literal</b>"){
 const body={version:1,asset_id:A,segment_id:B,core_index:0,start_ms:0,end_ms:30000,cues:[{cue_id:"1".repeat(64),start_ms:1000,end_ms:11000,text}],provenance:{engine_sha256:ENGINE,model_sha256:M,operation_epoch:"1",platform:"fixture",profile_id:P,source_sha256:CONTENT,worker_fence:"1"}};
 const canonical=JSON.stringify(body,(_k,v)=>v&&typeof v==="object"&&!Array.isArray(v)?Object.fromEntries(Object.entries(v).sort(([a],[b])=>a.localeCompare(b))):v),bytes=Buffer.from(canonical);
 return {body,bytes,ref:{segment_id:B,digest:createHash("sha256").update(bytes).digest("hex"),start_ms:0,end_ms:30000}};
}
function paint(h){h.ctx.sharedCaptionManifest(h.state,manifest(),1000);h.state.presentationValidated=true;h.fields["[data-shared-caption-use]"].disabled=false;h.player.captionIntent={kind:"translation",asset_id:A};h.state.cues.set("fixture",{cue_id:"1".repeat(64),start_ms:1000,end_ms:11000,text:"cached"});h.ctx.translationApplyCues(h.state);assert.equal(h.video._translationTrack.mode,"showing");}
const settle=()=>new Promise(resolve=>setImmediate(resolve));

test("actual registered opaque contexts work without an invented shared kind or local ID fallback",async()=>{
 const h=harness({seed:false});assert.equal(h.fileContext.source_ref.kind,undefined);assert.equal(h.ctx.sharedCaptionContext(h.player),h.fileContext);h.ctx.respond=()=>response({version:1,assets:[asset()],next_asset:null});await h.ctx.openSharedCaptionReader();assert.equal(h.calls.length,1);assert.match(h.calls[0].url,/\/shared\/imports\/.*\/files\/[A-Za-z0-9_-]{236}\/captions\?session=/);assert.equal(h.calls[0].url.includes("/files/7/"),false);assert.equal(h.calls[0].init.method,"GET");assert.equal(h.calls[0].init.headers.authorization,"Bearer viewer-bearer");
});
test("catalog and window use only bounded grant-scoped GETs and never producer routes",async()=>{
 const h=harness(),s=segment();h.ctx.respond=url=>url.includes("/manifest?")?response(manifest({segments:[s.ref]})):response(s.bytes);await h.ctx.sharedCaptionRefresh(h.state);assert.equal(h.state.presentationValidated,true);assert.equal(h.calls.length,2);assert.ok(h.calls.every(call=>call.init.method==="GET"));assert.match(h.calls[1].url,/lease=44444444.*session=55555555/);assert.equal(h.calls.some(call=>/demands|operations|retry|install|\/stop/.test(call.url)),false);
});
test("hash verification rejects raw bytes before UTF8 decoding and blocks ungranted provenance",async()=>{
 const h=harness(),s=segment();h.ctx.respond=url=>url.includes("/manifest?")?response(manifest({segments:[s.ref]})):response(new Uint8Array([255]));await h.ctx.sharedCaptionRefresh(h.state);assert.equal(h.state.presentationValidated,false);assert.equal(h.state.segments.size,0);assert.equal(h.video._translationTrack,undefined);
 const other=harness(),wrong=segment();wrong.body.provenance.source_sha256="0".repeat(64);const bytes=Buffer.from(JSON.stringify(wrong.body));const ref={...wrong.ref,digest:createHash("sha256").update(bytes).digest("hex")};other.ctx.respond=url=>url.includes("/manifest?")?response(manifest({segments:[ref]})):response(bytes);await other.ctx.sharedCaptionRefresh(other.state);assert.equal(other.state.presentationValidated,false);
});
test("verified shared cue text stays literal on the existing text renderer",async()=>{
 const h=harness(),s=segment();h.player.captionIntent={kind:"translation",asset_id:A};h.ctx.respond=url=>url.includes("/manifest?")?response(manifest({segments:[s.ref]})):response(s.bytes);await h.ctx.sharedCaptionRefresh(h.state);assert.equal(h.video._translationTrack.cues[0].text,"A &amp; B &lt;b&gt;literal&lt;/b&gt;");assert.equal(h.video._translationTrack.mode,"showing");
});
test("lease expiry clears paused presentation while a status read remains stalled",async()=>{
 const h=harness();paint(h);const expiry=h.state.leaseTimer;let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const read=h.ctx.sharedCaptionRefresh(h.state);h.clock(31001);h.fire(expiry);assert.equal(h.video._translationTrack.mode,"disabled");assert.equal(h.video._translationTrack.cues.length,0);assert.equal(h.state.presentationValidated,false);assert.equal(h.state.lease,null);assert.equal(h.state.busy,false);finish(response(manifest()));await read;assert.equal(h.state.presentationValidated,false);assert.equal(h.state.cues.size,0);assert.equal(h.calls[0].init.signal.aborted,true);
});
test("an earlier lease timer cannot clear a renewed snapshot",()=>{
 const h=harness();paint(h);const oldTimer=h.timers.get(h.state.leaseTimer).fn;h.clock(5000);h.ctx.sharedCaptionManifest(h.state,manifest({snapshot:{lease_id:"66666666-6666-4666-8666-666666666666",revision:"9007199254740993",expires_in_ms:30000}}),5000);const current=h.state.lease;h.clock(31001);oldTimer();assert.equal(h.state.lease,current);assert.equal(h.state.presentationValidated,true);assert.equal(h.video._translationTrack.mode,"showing");
});
test("expired lease cannot validate a completed segment read or enable Use",async()=>{
 const h=harness(),s=segment();h.ctx.respond=url=>url.includes("/manifest?")?response(manifest({segments:[s.ref],snapshot:{lease_id:LEASE,revision:"9007199254740993",expires_in_ms:1000}})):(h.clock(2001),response(s.bytes));await h.ctx.sharedCaptionRefresh(h.state);assert.equal(h.state.presentationValidated,false);assert.equal(h.fields["[data-shared-caption-use]"].disabled,true);assert.equal(h.state.segments.size,0);
});
test("source verifying clears old text and retries only reads at the bounded cadence",async()=>{
 const h=harness();paint(h);h.ctx.respond=()=>response({code:"source_verifying",message:"private Source detail"},425);await h.ctx.sharedCaptionRefresh(h.state);assert.equal(h.video._translationTrack.mode,"disabled");assert.equal(h.state.presentationValidated,false);assert.match(h.fields["[data-shared-caption-status]"].textContent,/Checking the Source/);assert.equal(h.fields["[data-shared-caption-status]"].textContent.includes("private"),false);assert.ok([...h.timers.values()].some(timer=>timer.ms===3000));assert.ok(h.calls.every(call=>call.init.method==="GET"));
});
test("seek cancels an older window and its finalizer cannot release the newer read owner",async()=>{
 const h=harness();const pending=[];h.ctx.respond=()=>new Promise(resolve=>pending.push(resolve));const old=h.ctx.sharedCaptionRefresh(h.state);h.video.currentTime=80;h.ctx.sharedCaptionMediaEvent(h.state,"seeking");assert.equal(pending.length,2);const current=h.state.readOwner;pending[0](response(manifest()));await old;assert.equal(h.state.readOwner,current);assert.equal(h.state.busy,true);pending[1](response(manifest({coverage:[{start_ms:70000,end_ms:90000}]})));await settle();assert.equal(h.state.busy,false);assert.equal(h.state.presentationValidated,true);
});
test("auth source lifecycle and attached audible audio fence pending reads",async()=>{
 for(const change of [h=>h.ctx.AUTH_GENERATION++,h=>h.player.mediaAttachment.audioOrdinal=4,h=>{const next={...h.detail,reference:{...h.detail.reference,lifecycle_generation:2n}};h.player.fileContext=h.ctx.withPlaybackFileSession(h.ctx.sharedPlaybackFileContextFromDetail(reference,next),SESSION);},h=>h.player.fileContext=h.ctx.localPlaybackFileContext("7")]){const h=harness();let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const read=h.ctx.sharedCaptionRefresh(h.state);change(h);finish(response(manifest()));await read;assert.equal(h.state.presentationValidated,false);assert.equal(h.state.segments.size,0);}
});
test("late catalog responses cannot overwrite a selected asset window or its status",async()=>{
 const h=harness();let finish;h.ctx.respond=url=>url.includes("/captions?session")?new Promise(resolve=>finish=resolve):response(manifest());const catalog=h.ctx.sharedCaptionCatalog(h.state,null);const choose=h.ctx.sharedCaptionChoose(h.state,asset());await choose;const status=h.fields["[data-shared-caption-status]"].textContent;finish(response({version:1,assets:[asset({asset_id:B})],next_asset:null}));await catalog;assert.equal(h.state.assetId,A);assert.equal(h.fields["[data-shared-caption-status]"].textContent,status);assert.equal(h.fields["[data-shared-caption-choices]"].children.length,0);
});
test("asset change hides old text before asynchronous replacement reads and drops stale cache entries",async()=>{
 const h=harness();paint(h);h.state.segments.set("old",{digest:"9".repeat(64),start_ms:0,end_ms:30000,cues:[{cue_id:"2".repeat(64),start_ms:1000,end_ms:11000,text:"old"}]});let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const choose=h.ctx.sharedCaptionChoose(h.state,asset({asset_id:B}));assert.equal(h.video._translationTrack.mode,"disabled");assert.equal(h.video._translationTrack.cues.length,0);finish(response(manifest({asset_id:B})));await choose;assert.equal(h.state.segments.size,0);
});
test("Off or source selection during asynchronous Use cannot be stolen by its completion",async()=>{
 for(const kind of ["off","source"]){const h=harness();paint(h);let finish;h.ctx.setSub=()=>new Promise(resolve=>finish=resolve);h.ctx.sharedCaptionUse(h.state);h.player.captionIntent={kind};h.player.captionIntentSerial+=2;finish();await settle();assert.equal(h.player.captionIntent.kind,kind);assert.equal(h.state.selecting,false);}
});
test("attachment handoff clears its lease and waits for the new position receipt before reading",()=>{
 const h=harness();paint(h);const oldTimer=h.state.leaseTimer;h.ctx.sharedCaptionAttachmentWillChange(h.player);assert.equal(h.state.lease,null);assert.equal(h.timers.has(oldTimer),false);assert.equal(h.video._translationTrack.mode,"disabled");const token={audioOrdinal:3,captionPositionReady:false};h.player.mediaAttachment=token;h.ctx.sharedCaptionAttachmentDidBegin(h.player,token);h.ctx.sharedCaptionAttachmentPositionReady(h.player,token);assert.equal(h.calls.length,0);assert.equal(h.state.rebinding,true);
});
test("retirement cannot hide a newer local caption presentation on the reused video element",()=>{
 const h=harness();paint(h);h.player.fileContext=h.ctx.localPlaybackFileContext("7");const newer=h.video.addTextTrack();newer.mode="showing";newer.addCue({text:"new local"});h.video._translationTrack=newer;h.ctx.sharedCaptionRetire("source changed");assert.equal(newer.mode,"showing");assert.equal(newer.cues[0].text,"new local");assert.equal(h.state.retired,true);assert.equal(h.player.captionIntent.kind,"translation");
});

test("a stale source-verifying catalog response cannot clear a newer validated window",async()=>{
 const h=harness();let finish;h.ctx.respond=url=>url.includes("/captions?session")?new Promise(resolve=>finish=resolve):response(manifest());const old=h.ctx.sharedCaptionCatalog(h.state,null);await h.ctx.sharedCaptionChoose(h.state,asset());assert.equal(h.state.presentationValidated,true);const lease=h.state.lease;finish(response({code:"source_verifying",message:"old private detail"},425));await old;assert.equal(h.state.presentationValidated,true);assert.equal(h.state.lease,lease);assert.equal(h.state.error,null);
});
