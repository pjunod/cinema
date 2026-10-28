"use strict";
const test=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const {webcrypto,createHash}=require('node:crypto');
const WEB=path.join(__dirname,'../../crates/plurxd/src/web');
const A='a'.repeat(64),B='b'.repeat(64),P='c'.repeat(64),M='d'.repeat(64),E='e'.repeat(64),S='f'.repeat(64);
const OP='11111111-1111-4111-8111-111111111111',DEMAND='22222222-2222-4222-8222-222222222222',LEASE='33333333-3333-4333-8333-333333333333';
const limits={segment_bytes:16384,segment_cues:128,page_bytes:65536,page_entries:64,coverage_intervals:64,concurrent_fetches:2,cue_memory_bytes:524288,cue_count:1024,cue_window_before_ms:30000,cue_window_after_ms:90000,poll_min_ms:1000,poll_max_ms:5000,retry_max_ms:30000,heartbeat_interval_ms:5000};
const profile={profile_id:P,model_id:'measured-model-fixture',model_sha256:M,task:'translate',output_language:'en',input_languages:['de','ja'],activation_state:'active',limits,adapter_contract:'fixture-adapter',timeline_contract:'fixture-clock'};
function canonical(value){return JSON.stringify(value,(_key,row)=>row&&typeof row==='object'&&!Array.isArray(row)?Object.fromEntries(Object.entries(row).sort(([a],[b])=>a.localeCompare(b))):row);}
function sha(bytes){return createHash('sha256').update(bytes).digest('hex');}
function response(data,status=200,headers={}){const reply=new Response(typeof data==='string'||data instanceof Uint8Array?data:JSON.stringify(data),{status,headers:{'content-type':'application/json',...headers}});if(data&&typeof data==='object'&&!(data instanceof Uint8Array))reply.fixtureData=data;return reply;}
function track(){return {mode:'disabled',cues:[],addCue(cue){this.cues.push(cue);},removeCue(cue){this.cues.splice(this.cues.indexOf(cue),1);}};}
function node(){let text='';return {children:[],isConnected:true,hidden:false,disabled:false,value:'',checked:false,dataset:{},className:'',listeners:{},addEventListener(type,fn){this.listeners[type]=fn;},appendChild(child){this.children.push(child);return child;},replaceChildren(){this.children=[];text='';},querySelector(selector){return this.fields?.[selector]||null;},querySelectorAll(){return [];},get textContent(){return text+this.children.map(child=>child.textContent||'').join('');},set textContent(v){text=String(v);this.children=[];},innerHTML:''};}
function harness(){
 let now=1000,nextTimer=0;const timers=new Map(),calls=[],nodes={},video=node(),panel=node();
 video.currentTime=5;video.playbackRate=1;video.paused=false;video.textTracks=[];video.domTracks=[];
 video.querySelectorAll=selector=>selector==='track'?video.domTracks.slice():[];
 video.addTextTrack=(_kind,label,language)=>{const t=track();t.label=label;t.language=language;video.textTracks.push(t);return t;};
 video.removeEventListener=()=>{};
 video.appendChild=tr=>{video.domTracks.push(tr);video.textTracks.push(tr.track);tr.remove=()=>video.domTracks.splice(video.domTracks.indexOf(tr),1);};
 nodes.video=video;nodes['watch-subtitle-translation']=panel;
 const controls={};for(const key of ['profile','language','confirm','status','clock','use','stop','resume','failures','task','heading','description','start','assets'])controls[`[data-translation-${key}]`]=node();
 controls['[data-translation-profile]'].value=P;controls['[data-translation-language]'].value='de';controls['[data-translation-confirm]'].checked=true;controls['[data-translation-task]'].value='translate';panel.fields=controls;
 const ctx=vm.createContext({console,Uint8Array,TextEncoder,TextDecoder,Response,AbortController,URLSearchParams,Map,Set,BigInt,crypto:webcrypto,performance:{now:()=>now},Date,API:'/api/v1',AUTH_GENERATION:1,TOKEN:'private-fixture',PAGE_RENDER_GENERATION:1,ME:{is_admin:true},location:{hash:'#/item/1'},window:{VTTCue:class{constructor(start,end,text){this.startTime=start;this.endTime=end;this.text=text;}}},document:{getElementById:id=>nodes[id]||null,createElement:tag=>{const el=node();if(tag==='track')el.track=track();return el;}},setTimeout:(fn,ms)=>{const id=++nextTimer;timers.set(id,{fn,ms});return id;},clearTimeout:id=>timers.delete(id),esc:x=>String(x).replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;').replaceAll('"','&quot;'),toast(){},closeMenu(){},endWait(){},PlaybackPolicy:{subtitleBurnAction:()=>"native"},subNeedsBurn:()=>false,positionForPlaybackIntent:()=>0,supersedePlaybackControlIntent:p=>p.controlIntentGeneration=(p.controlIntentGeneration||0)+1,notifyPlaybackControl(){},restartPendingPlaybackOpen:()=>false,nativeHlsSubtitleOrdinal:()=>-1,pbSyncSubIcon(){},tok:x=>x,playbackFileContextForPlayer:p=>p.fileContext,playbackFileUrl:(_c,s)=>'/source/'+s,subLabelMenu:()=>"Original",fetch:async(url,init)=>{calls.push({url,init});const reply=await ctx.respond(url,init),sent=init.body?JSON.parse(init.body):null,data=reply.fixtureData;
  // Legacy receipt fixtures echo newly supplied continuity unless a test gives
  // an explicit echo (including a deliberately invalid one).
  if(reply.ok&&sent?.continuity_id&&data&&data.version===1&&!Object.hasOwn(data,'continuity_id'))return response({...data,continuity_id:sent.continuity_id},reply.status,Object.fromEntries(reply.headers));return reply;},respond:()=>{throw Error('Unexpected request');}});
 vm.runInContext(fs.readFileSync(path.join(WEB,'core/diagnostics-hash.js'),'utf8'),ctx);
 vm.runInContext(fs.readFileSync(path.join(WEB,'player/subtitle-translation.js'),'utf8'),ctx);
 vm.runInContext(fs.readFileSync(path.join(WEB,'player/audio-sync.js'),'utf8'),ctx);
 const player={fileId:'9007199254740993',started:true,mediaAttachment:{id:1},audio:[{index:0,language:'deu'}],curAudio:0,curSub:-1,offset:0,subs:[{index:0,text:true,language:'de'}],method:'direct_play',fileContext:{source_ref:{kind:'local',file_id:'9007199254740993'}}};ctx.PLAYER=player;
 const state={capture:ctx.translationCapture(),player,video,panel,attachment:player.mediaAttachment,fileId:player.fileId,audioChoice:0,audioIndex:0,task:'translate',outputLanguage:'en',controllers:new Set(),listeners:[],segments:new Map(),cues:new Map(),coverage:[{start_ms:0,end_ms:20000}],operation:{operation_id:OP,operation_epoch:'1',view_revision:'1',state:'partial',failed_ranges:[]},assetId:A,playbackId:'44444444-4444-4444-8444-444444444444',queue:Promise.resolve(),windowGeneration:0,profile:structuredClone(profile),limits:{...limits},options:{version:1,file_id:player.fileId,source_size:'9007199254740995',source_mtime:'9007199254740997',duration_ms:120000,source_origin_ms:0,audio_tracks:[{index:0,audio_ordinal:0,normalized_language:'de',capability:{supported:true}}],models:[structuredClone(profile)],permissions:{read:true,demand:true,stop:true,resume:true}},lease:{id:LEASE,revision:'7',deadline:31000},contentRevision:S,retired:false,clock:{server:100000,at:now,transit:0}};
 state.current=()=>ctx.translationCurrent(state);ctx.seed=state;vm.runInContext('SUBTITLE_TRANSLATION=seed',ctx);
 return {ctx,state,player,video,panel,controls,calls,timers,nodes,clock:value=>now=value,fire(ms){for(const [id,t]of [...timers])if(t.ms===ms){timers.delete(id);t.fn();}}};
}
function segment(h,text='A & B <b>word</b> <00:01>\nnext line'){
 const body={version:1,asset_id:A,segment_id:B,core_index:0,start_ms:0,end_ms:20000,cues:[{cue_id:'1'.repeat(64),start_ms:0,end_ms:11000,text}],provenance:{engine_sha256:E,model_sha256:M,operation_epoch:'1',platform:'fixture',profile_id:P,source_sha256:S,worker_fence:'1'}};
 const bytes=Buffer.from(canonical(body));return {body,bytes,reference:{segment_id:B,digest:sha(bytes),start_ms:0,end_ms:20000}};
}
function operation(h,fields={}){const p=h.state.profile;return {version:1,...h.state.operation,file_id:h.state.fileId,source_size:h.state.options.source_size,source_mtime:h.state.options.source_mtime,audio_index:0,spoken_language:'de',model_id:p.model_id,model_sha256:p.model_sha256,timeline_contract:p.timeline_contract,content_revision:S,output_language:p.task==='transcribe'?'de':'en',task:p.task||'translate',profile_id:p.profile_id,asset_id:A,failed_ranges:[],...fields};}
function manifest(h,fields={}){const p=h.state.profile;return {version:1,asset_id:A,file_id:h.state.fileId,profile_id:P,audio_index:0,spoken_language:'de',output_language:p.task==='transcribe'?'de':'en',task:p.task||'translate',timeline_contract:p.timeline_contract,content_revision:S,manifest_revision:'7',view_revision:'2',operation:{operation_id:OP,epoch:'1',state:'partial'},snapshot:{lease_id:LEASE,revision:'7',expires_in_ms:30000},coverage:[{start_ms:0,end_ms:20000}],segments:[],coverage_next_cursor:null,segment_next_cursor:null,...fields};}
const settle=()=>new Promise(resolve=>setImmediate(resolve));
test('exact counters and supplied profile limits never invent an activated model',()=>{
 const h=harness();assert.equal(h.ctx.translationNext('9007199254740993'),'9007199254740994');assert.throws(()=>h.ctx.translationNext('9223372036854775807'));
 assert.equal(h.ctx.translationProfileLimits({}),null);const incomplete=structuredClone(profile);delete incomplete.limits.poll_max_ms;assert.equal(h.ctx.translationProfileLimits(incomplete),null);
 h.state.profile.activation_state='pending';h.state.options.models=[h.state.profile];return h.ctx.translationStart(h.state).then(()=>{assert.equal(h.calls.length,0);assert.match(h.state.error,/No qualified/);});
});
test('local direct, remux and encoded attachments allow a known selected audio ordinal',()=>{
 const h=harness();for(const method of ['direct_play','remux','transcode']){h.player.method=method;if(method==='remux')h.player.sessionId='owned-session';assert.equal(h.ctx.translationDirectReason(h.player,h.video),null);}
 h.player.curAudio=-1;assert.equal(h.ctx.translationDirectReason(h.player,h.video),'direct_audio_selection_unverified');h.player.curAudio=0;h.player.burnedSub=1;assert.equal(h.ctx.translationDirectReason(h.player,h.video),'delivery_unsupported');h.player.burnedSub=null;h.player.pendingMediaChange={};assert.equal(h.ctx.translationDirectReason(h.player,h.video),'delivery_unsupported');
});
test('transcription request, operation and manifest retain spoken-language output identity',async()=>{
 const h=harness(),p={...structuredClone(profile),task:'transcribe',input_languages:['de','en']};h.state.options.models=[p];h.state.profile=p;h.state.task='transcribe';h.controls['[data-translation-task]'].value='transcribe';h.controls['[data-translation-language]'].value='de';h.ctx.translationPoll=async()=>{};h.ctx.respond=()=>response({version:1,operation_id:OP,operation_epoch:'1',view_revision:'1',asset_id:A,state:'checking_source',demand_id:DEMAND,demand_generation:'1',server_now_ms:100000});
 await h.ctx.translationStart(h.state);const body=JSON.parse(h.calls[0].init.body);assert.equal(body.task,'transcribe');assert.equal(body.spoken_language,'de');assert.equal(body.output_language,'de');assert.doesNotThrow(()=>h.ctx.translationValidateOperation(h.state,operation(h)));h.ctx.respond=()=>response(manifest(h));await h.ctx.translationManifest(h.state,true);assert.equal(h.state.outputLanguage,'de');
});
test('options validate selected audio ordinal independently of other unsupported tracks',()=>{
 const h=harness();h.state.options.audio_tracks.push({index:4,audio_ordinal:1,normalized_language:'ja',capability:{supported:false,reason:'source_clock_unsupported'}});h.ctx.translationValidateOptions(h.state.options,h.state);h.state.player.curAudio=1;h.state.audioIndex=1;assert.throws(()=>h.ctx.translationValidateOptions(h.state.options,h.state),/timeline is not supported/);
});
test('task selector switches among task-qualified profiles and retains valid language choices',async()=>{
 const h=harness(),transcribe={...structuredClone(profile),profile_id:A,task:'transcribe',input_languages:['de','en']};h.ctx.respond=()=>response({version:1,file_id:h.player.fileId,source_size:'9007199254740995',source_mtime:'9007199254740997',duration_ms:120000,source_origin_ms:0,audio_tracks:[{index:0,audio_ordinal:0,normalized_language:'de',title:'German',capability:{supported:true}}],models:[structuredClone(profile),transcribe],permissions:{read:true,demand:true,stop:true,resume:true},assets:[],assets_next_cursor:null});
 h.ctx.translationRetire('open task selector fixture');assert.equal(h.state.retired,true);
 await h.ctx.translationOpen(node());const selector=h.controls['[data-translation-task]'];assert.equal(selector.value,'translate');selector.value='transcribe';selector.listeners.change();assert.equal(vm.runInContext('SUBTITLE_TRANSLATION.task',h.ctx),'transcribe');assert.equal(h.controls['[data-translation-profile]'].value,A);assert.equal(h.controls['[data-translation-language]'].value,'de');assert.match(h.panel.innerHTML,/Generate captions in the spoken language/);
});
test('existing caption assets remain readable and bind their task to the matching profile',async()=>{
 const h=harness(),p={...structuredClone(profile),profile_id:A,task:'transcribe',input_languages:['de','en']};h.state.options.models.push(p);h.ctx.translationPoll=async()=>{};await h.ctx.translationReadAsset(h.state,{asset_id:A,profile_id:A,task:'transcribe',spoken_language:'de',output_language:'de'});assert.equal(h.state.task,'transcribe');assert.equal(h.state.outputLanguage,'de');
 await h.ctx.translationReadAsset(h.state,{asset_id:B,profile_id:P,task:'translate',spoken_language:'de',output_language:'en'});assert.equal(h.state.task,'translate');assert.equal(h.state.outputLanguage,'en');
});
test('transcription cue presentation uses its output language and source-offset seconds',()=>{
 const h=harness();h.state.profile={...structuredClone(profile),task:'transcribe',input_languages:['de','en']};h.state.task='transcribe';h.state.outputLanguage='de';h.state.spokenLanguage='de';h.player.offset=2;h.player.captionIntent={kind:'translation',asset_id:A};h.state.cues.set('1'.repeat(64),{cue_id:'1'.repeat(64),start_ms:3000,end_ms:7000,text:'spoken'});h.ctx.translationApplyCues(h.state);
 assert.equal(h.video._translationTrackLanguage,'de');assert.match(h.video._translationTrack.label,/AI captions · de/);assert.equal(h.video._translationTrack.cues[0].startTime,1);assert.equal(h.video._translationTrack.cues[0].endTime,5);
});
test('digest is checked before decoding or parsing and never client reserialization',async()=>{
 const h=harness();h.ctx.respond=()=>response(new Uint8Array([255]));await assert.rejects(h.ctx.translationSegment(h.state,{segment_id:B,digest:'0'.repeat(64),start_ms:0,end_ms:20000},h.state.lease,0),/digest mismatch/);
 const s=segment(h);h.ctx.respond=()=>response(JSON.stringify(s.body,null,2));await assert.rejects(h.ctx.translationSegment(h.state,s.reference,h.state.lease,0),/digest mismatch/);assert.equal(h.state.segments.size,0);
});
test('literal cue text survives verified raw bytes without WebVTT markup and duplicates',async()=>{
 const h=harness(),s=segment(h);h.ctx.respond=()=>response(s.bytes);await h.ctx.translationSegment(h.state,s.reference,h.state.lease,0);h.ctx.translationPrune(h.state);h.player.captionIntent={kind:'translation',asset_id:A};h.ctx.translationApplyCues(h.state);h.ctx.translationApplyCues(h.state);
 assert.equal(h.video._translationTrack.cues.length,1);assert.equal(h.video._translationTrack.cues[0].text,'A &amp; B &lt;b&gt;word&lt;/b&gt; &lt;00:01&gt;\nnext line');assert.equal(h.video._translationTrack.mode,'showing');
});
test('manifest refresh preserves unchanged cue objects and applies only the window delta',()=>{
 const h=harness();h.player.captionIntent={kind:'translation',asset_id:A};
 const first={cue_id:'1'.repeat(64),start_ms:3000,end_ms:7000,text:'first'},second={cue_id:'2'.repeat(64),start_ms:8000,end_ms:11000,text:'second'};
 h.state.cues.set(first.cue_id,first);h.ctx.translationApplyCues(h.state);const t=h.video._translationTrack,original=t.cues[0];let added=0,removed=0;
 const add=t.addCue.bind(t),remove=t.removeCue.bind(t);t.addCue=cue=>{added++;add(cue);};t.removeCue=cue=>{removed++;remove(cue);};
 h.ctx.translationApplyCues(h.state);assert.equal(t.cues[0],original);assert.equal(added,0);assert.equal(removed,0);
 h.state.cues.set(second.cue_id,second);h.ctx.translationApplyCues(h.state);const retained=t.cues[1];assert.equal(t.cues[0],original);assert.equal(added,1);assert.equal(removed,0);
 h.state.cues.delete(first.cue_id);h.ctx.translationApplyCues(h.state);assert.equal(t.cues.length,1);assert.equal(t.cues[0],retained);assert.equal(removed,1);
 h.player.offset=2;h.ctx.translationApplyCues(h.state);assert.notEqual(t.cues[0],retained);assert.equal(t.cues[0].startTime,6);assert.equal(t.cues[0].endTime,9);assert.equal(added,2);assert.equal(removed,2);
 h.state.cues.set(second.cue_id,{...second,text:'changed'});h.ctx.translationApplyCues(h.state);assert.equal(t.cues[0].text,'changed');assert.equal(added,3);assert.equal(removed,3);assert.equal(t.mode,'showing');
});
test('disabled retained tracks expose cues before reconciliation after Off and reselection',async()=>{
 const h=harness();h.player.captionIntent={kind:'translation',asset_id:A};
 const first={cue_id:'1'.repeat(64),start_ms:3000,end_ms:7000,text:'first'},second={cue_id:'2'.repeat(64),start_ms:8000,end_ms:11000,text:'second'};
 h.state.cues.set(first.cue_id,first);h.state.cues.set(second.cue_id,second);h.ctx.translationApplyCues(h.state);
 const t=h.video._translationTrack,stored=t.cues,retained=stored[1];Object.defineProperty(t,'cues',{get(){return this.mode==='disabled'?null:stored;}});t.addCue=cue=>stored.push(cue);t.removeCue=cue=>stored.splice(stored.indexOf(cue),1);
 await h.ctx.setSub(-1);assert.equal(t.mode,'disabled');assert.equal(t.cues,null);h.state.cues.delete(first.cue_id);await h.ctx.translationUse(h.state);
 assert.equal(t.mode,'showing');assert.equal(t.cues.length,1);assert.equal(t.cues[0],retained);
 await h.ctx.setSub(0);assert.equal(t.cues,null);await h.ctx.translationUse(h.state);assert.equal(t.cues.length,1);assert.equal(t.cues[0],retained);
});
test('silent coverage and disjoint future work do not fabricate a gap or runway',()=>{
 const h=harness();assert.deepEqual({...h.ctx.translationRangeStatus([{start_ms:0,end_ms:20000}],10000)},{covered:true,runway_ms:10000,lag_ms:0});assert.deepEqual({...h.ctx.translationRangeStatus([{start_ms:0,end_ms:20000},{start_ms:40000,end_ms:60000}],30000)},{covered:false,runway_ms:0,lag_ms:10000});
});
test('Off or original chosen during a segment read cannot be stolen by its completion',async()=>{
 for(const selected of [-1,0]){const h=harness(),s=segment(h);h.player.captionIntent={kind:'translation',asset_id:A};let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const read=h.ctx.translationSegment(h.state,s.reference,h.state.lease,0);await h.ctx.setSub(selected);finish(response(s.bytes));await read;h.ctx.translationPrune(h.state);h.ctx.translationApplyCues(h.state);assert.equal(h.player.captionIntent.kind,selected<0?'off':'source');assert.equal(h.video._translationTrack,undefined);}
});
test('existing complete-track delayed mode cannot revive after source A to translation to A',async()=>{
 const h=harness();await h.ctx.setSub(0);const first=h.video.domTracks[0];await h.ctx.translationUse(h.state);await h.ctx.setSub(0);const second=h.video.domTracks[0];h.fire(30);assert.equal(first.track.mode,'disabled');assert.equal(second.track.mode,'showing');assert.equal(h.player.captionIntent.kind,'source');
});
test('existing offset VTT fetch cannot replace a newer source selection with same index',async()=>{
 const h=harness();h.player.offset=2;h.player.method='remux';const answers=[];h.ctx.fetch=()=>new Promise(resolve=>answers.push(resolve));const old=h.ctx.setSub(0);await h.ctx.setSub(-1);const current=h.ctx.setSub(0);answers[1]({ok:true,text:async()=>"WEBVTT\n\n00:03.000 --> 00:06.000\nnew\n"});await current;answers[0]({ok:true,text:async()=>"WEBVTT\n\n00:03.000 --> 00:06.000\nold\n"});await old;assert.equal(h.video._vsubs.cues.length,1);assert.equal(h.video._vsubs.cues[0].text,'new');
});
test('pending asset A reads cannot populate asset B or retain A demand',async()=>{
 const h=harness(),s=segment(h);h.state.demand={id:DEMAND,generation:'1',epoch:'1'};let finish;h.ctx.respond=(url,init)=>init?.method==='DELETE'?response({version:1,server_now_ms:100000}):new Promise(resolve=>finish=resolve);const read=h.ctx.translationSegment(h.state,s.reference,h.state.lease,0);h.ctx.translationPoll=async()=>{};await h.ctx.translationReadAsset(h.state,{asset_id:'9'.repeat(64),profile_id:P,task:'translate',spoken_language:'de',output_language:'en'});finish(response(s.bytes));await read;assert.equal(h.state.assetId,'9'.repeat(64));assert.equal(h.state.segments.size,0);assert.equal(h.state.demand,null);assert.equal(h.calls.filter(call=>call.init.method==='DELETE').length,1);assert.equal(h.calls.some(call=>call.url.endsWith('/stop')),false);
});
test('expired lease after fetched bytes and mixed snapshot pages refuse adoption',async()=>{
 const h=harness(),s=segment(h);h.ctx.respond=()=>{h.clock(40000);return response(s.bytes);};await h.ctx.translationSegment(h.state,s.reference,h.state.lease,0);assert.equal(h.state.segments.size,0);
 h.clock(1000);h.ctx.respond=()=>response({version:1,asset_id:A,snapshot_revision:'8',entries:[],next_cursor:null});await assert.rejects(h.ctx.translationPages(h.state,'segments',[],'cursor',h.state.lease,0),/another snapshot/);
});
test('conditional 304 never renews a lease and explicit resync always requests new snapshot',async()=>{
 const h=harness();h.state.etag='"view1"';h.state.manifestWindow=0;h.ctx.respond=()=>new Response(null,{status:304,headers:{'X-Translation-Server-Now-Ms':'100000','X-Translation-Read-Lease-Expires-At-Ms':'130000'}});const deadline=h.state.lease.deadline;await h.ctx.translationManifest(h.state,false);assert.equal(h.state.lease.deadline,deadline);assert.equal(h.calls[0].init.headers['If-None-Match'],'"view1"');h.clock(32000);h.ctx.respond=()=>response(manifest(h));await h.ctx.translationManifest(h.state,false);assert.match(h.calls[1].url,/renew_lease=1/);assert.equal(h.calls[1].init.headers['If-None-Match'],undefined);
});
test('explicit confirmed spoken language can correct wrong metadata',async()=>{
 const h=harness();h.state.assetId=null;h.state.operation=null;h.controls['[data-translation-language]'].value='ja';h.ctx.translationPoll=async()=>{};h.ctx.respond=()=>response({version:1,operation_id:OP,operation_epoch:'1',view_revision:'1',asset_id:null,state:'checking_source',demand_id:DEMAND,demand_generation:'1',server_now_ms:100000});await h.ctx.translationStart(h.state);assert.equal(JSON.parse(h.calls[0].init.body).spoken_language,'ja');assert.equal(JSON.parse(h.calls[0].init.body).audio_index,0);assert.equal(JSON.parse(h.calls[0].init.body).source_size,'9007199254740995');
});
test('nonadmin reads do not create or refresh demand and Stop latches old heartbeats',async()=>{
 const h=harness();h.ctx.ME={is_admin:false};h.state.demand={id:DEMAND,generation:'1',epoch:'1'};await h.ctx.translationStart(h.state);await h.ctx.translationHeartbeat(h.state);await h.ctx.translationStop(h.state);assert.equal(h.calls.length,0);
 h.ctx.ME={is_admin:true};h.ctx.respond=()=>response({version:1,operation_id:OP,operation_epoch:'2',view_revision:'9',state:'stopped',asset_id:A,server_now_ms:100000});await h.ctx.translationStop(h.state);await h.ctx.translationHeartbeat(h.state);assert.equal(h.calls.length,1);assert.equal(h.state.demand,null);assert.equal(h.state.stopped,true);assert.equal(h.state.assetId,A);
});
test('ambiguous retries keep exact intent but definitive expiry and changed profile retire it',async()=>{
 const h=harness();h.state.operation=null;h.state.assetId=null;let mode='network';h.ctx.respond=()=>{if(mode==='network')throw Error('network');return response({version:1,code:'intent_expired',server_now_ms:100000},410);};await h.ctx.translationStart(h.state);await h.ctx.translationStart(h.state);assert.equal(h.calls[0].init.body,h.calls[1].init.body);mode='expired';await h.ctx.translationStart(h.state);assert.equal(h.state.createIntent,null);mode='network';await h.ctx.translationStart(h.state);assert.notEqual(h.calls[2].init.body,h.calls[3].init.body);
 const other={...structuredClone(profile),profile_id:'8'.repeat(64)};h.state.options.models.push(other);h.controls['[data-translation-profile]'].value=other.profile_id;await h.ctx.translationStart(h.state);assert.equal(JSON.parse(h.calls.at(-1).init.body).profile_id,other.profile_id);assert.notEqual(JSON.parse(h.calls.at(-1).init.body).request_id,JSON.parse(h.calls.at(-2).init.body).request_id);
});
test('account route and attachment retirement abort reads without painting late errors',async()=>{
 for(const change of [h=>h.ctx.AUTH_GENERATION++,h=>h.ctx.PAGE_RENDER_GENERATION++,h=>h.player.mediaAttachment={}]){const h=harness();let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const read=h.ctx.translationRequest(h.state,'/subtitle-translation/operations/'+OP);change(h);finish(response({version:1,server_now_ms:100000}));await assert.rejects(read,/Retired/);assert.equal(h.state.controllers.size,0);assert.equal(h.controls['[data-translation-status]'].textContent,'');}
});
test('polling interval is finite and expires snapshots before retry, never zero-delay',async()=>{
 const h=harness();h.ctx.respond=()=>response(operation(h,{asset_id:null}));await h.ctx.translationPoll(h.state);assert.equal([...h.timers.values()].some(timer=>timer.ms===5000),true);assert.equal([...h.timers.values()].some(timer=>!Number.isFinite(timer.ms)||timer.ms<=0),false);
});
function settings(h){
 h.ctx.location.hash='#/settings/developer/enable-subtitle-translation';const panel=node(),fields={};
 for(const name of ['profile','language','enabled'])fields[`[data-translation-setting-${name}]`]=node();fields['[data-translation-setting-profile]'].value=P;fields['[data-translation-setting-language]'].value='de';fields['[data-translation-setting-enabled]'].checked=true;
 for(const name of ['workers','model-status','save-status','install','test'])fields[`[data-translation-${name}]`]=node();panel.fields=fields;
 const worker=node();worker.value='55555555-5555-4555-8555-555555555555';worker.checked=true;panel.querySelectorAll=selector=>selector.includes('translation-worker')?[worker]:[];
 h.nodes['subtitle-translation-form']=panel;
 const setup={capture:h.ctx.translationCapture(),panel,controllers:new Set(),retired:false,rows:[],edit:0,catalog:{profiles:[structuredClone(profile)],models:[],workers:[]},saved:{version:1,revision:'0.'+S,enabled:true,model_id:profile.model_id,profile_id:P,preferred_spoken_language:'de'},clock:{server:100000,at:1000,transit:0}};setup.current=()=>h.ctx.translationSetupCurrent(setup);h.ctx.setupSeed=setup;vm.runInContext('TRANSLATION_SETUP=setupSeed',h.ctx);return {setup,panel,fields,worker};
}
test('LAN HTTP translation actions generate UUID intents without randomUUID',async()=>{
 const h=harness(),s=settings(h);h.ctx.crypto={getRandomValues:bytes=>webcrypto.getRandomValues(bytes)};
 const ids=new Set();
 for(let i=0;i<32;i++){const envelope=h.ctx.translationEnvelope(s.setup);assert.match(envelope.request_id,/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);ids.add(envelope.request_id);assert.equal(envelope.intent_expires_at_ms,160000);}
 assert.equal(ids.size,32);
 h.ctx.respond=()=>response({...s.setup.saved,server_now_ms:100000});await h.ctx.saveTranslationSettings();assert.equal(h.calls.length,1);assert.equal(h.ctx.translationId(JSON.parse(h.calls[0].init.body).request_id),true);
 for(const action of ['install','verify']){h.ctx.respond=url=>response(url.endsWith('/'+action)?{version:2,purpose:'translation',model_id:profile.model_id,profile_id:P,operations:[],server_now_ms:100000}:{...s.setup.catalog,version:1,server_now_ms:100000});await h.ctx.translationSetupModel(action);await settle();const call=h.calls.find(call=>call.url.endsWith('/'+action));assert.ok(call);assert.equal(h.ctx.translationId(JSON.parse(call.init.body).request_id),true);}
});
test('LAN HTTP translation opens with a UUID playback identity and starts work',async()=>{
 const h=harness();h.ctx.crypto={getRandomValues:bytes=>webcrypto.getRandomValues(bytes)};
 vm.runInContext('SUBTITLE_TRANSLATION=null',h.ctx);h.ctx.respond=()=>{throw Error('fixture status unavailable');};await h.ctx.translationOpen(node());
 assert.equal(h.calls.length,1);assert.match(h.calls[0].url,/subtitle-translation\/options$/);assert.equal(h.ctx.translationId(vm.runInContext('SUBTITLE_TRANSLATION.playbackId',h.ctx)),true);
 // Existing player actions use the same HTTP-compatible envelope.
 h.state.operation=null;h.state.assetId=null;h.ctx.translationPoll=async()=>{};h.ctx.respond=()=>response({version:1,operation_id:OP,operation_epoch:'1',view_revision:'1',asset_id:null,state:'checking_source',demand_id:DEMAND,demand_generation:'1',server_now_ms:100000});
 await h.ctx.translationStart(h.state);assert.equal(h.calls.length,2);assert.equal(h.ctx.translationId(JSON.parse(h.calls[1].init.body).request_id),true);
});
test('LAN HTTP caption reads verify exact bytes without SubtleCrypto',async()=>{
 const h=harness(),s=segment(h);h.ctx.crypto={getRandomValues:bytes=>webcrypto.getRandomValues(bytes)};h.ctx.respond=()=>response(s.bytes);
 await h.ctx.translationSegment(h.state,s.reference,h.state.lease,0);assert.equal(h.state.segments.size,1);
 const corrupt=Buffer.from(s.bytes);corrupt[corrupt.length-1]^=1;h.ctx.respond=()=>response(corrupt);
 await assert.rejects(h.ctx.translationSegment(h.state,s.reference,h.state.lease,0),/Caption segment digest mismatch/);assert.equal(h.state.segments.size,1);
});
test('Settings CAS save keeps enabled unconfigured and preserves edits made during save',async()=>{
 const h=harness(),s=settings(h);s.fields['[data-translation-setting-profile]'].value='';s.setup.saved.model_id=null;s.setup.saved.profile_id=null;s.setup.catalog=null;let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);
 const save=h.ctx.saveTranslationSettings();const body=JSON.parse(h.calls[0].init.body);assert.equal(body.enabled,true);assert.equal(body.model_id,null);assert.equal(body.profile_id,null);assert.equal(body.expected_revision,'0.'+S);
 s.fields['[data-translation-setting-enabled]'].checked=false;s.fields['[data-translation-setting-language]'].value='ja';finish(response({...s.setup.saved,revision:'new.'+S,server_now_ms:100000}));await save;
 assert.equal(s.setup.saved.revision,'new.'+S);assert.equal(s.fields['[data-translation-setting-enabled]'].checked,false);assert.equal(s.fields['[data-translation-setting-language]'].value,'ja');assert.match(s.fields['[data-translation-save-status]'].textContent,/Saved/);
});
test('Settings conflict keeps unsaved choices and does not retry rejected CAS forever',async()=>{
 const h=harness(),s=settings(h);h.ctx.respond=()=>response({version:1,code:'epoch_conflict',server_now_ms:100000},409);await h.ctx.saveTranslationSettings();assert.equal(s.setup.saveIntent,null);assert.equal(s.fields['[data-translation-setting-enabled]'].checked,true);assert.match(s.fields['[data-translation-save-status]'].textContent,/Reload saved settings/);
});
test('English transcription receipt never becomes foreign translation capability',()=>{
 const h=harness(),s=settings(h),worker={current:true,eligible:true,observed_at_ms:99900,expires_at_ms:160000,models:[{model_id:profile.model_id,status:'verified'}]};assert.equal(h.ctx.translationWorkerStatus(s.setup,worker,profile.model_id,P),'unknown');worker.capabilities=[{model_id:profile.model_id,profile_id:P,status:'verified'}];assert.equal(h.ctx.translationWorkerStatus(s.setup,worker,profile.model_id,P),'verified');worker.expires_at_ms=99999;assert.equal(h.ctx.translationWorkerStatus(s.setup,worker,profile.model_id,P),'unknown');
});
test('translation Install uses version2 and preserves partial refusals through stale terminal observations',async()=>{
 const h=harness(),s=settings(h);const refused='66666666-6666-4666-8666-666666666666',job='77777777-7777-4777-8777-777777777777';let verified=false;
 const catalog=()=>({version:1,server_now_ms:100000,profiles:[profile],models:[],workers:[{node_id:s.worker.value,label:'Available worker',current:true,eligible:true,observed_at_ms:99900,expires_at_ms:160000,engine:{adapter_contract:profile.adapter_contract,binary_sha256:E},capabilities:[{model_id:profile.model_id,profile_id:P,status:verified?'verified':'unverified_cache'}]},{node_id:refused,label:'Refused worker'}]});
 s.setup.catalog=catalog();h.ctx.respond=url=>url.endsWith('/install')?response({version:2,purpose:'translation',model_id:profile.model_id,profile_id:P,operations:[{node_id:s.worker.value,job_id:job,outcome:'accepted'},{node_id:refused,outcome:'quota_exceeded'}],server_now_ms:100000}):url.includes('/jobs/')?response({job_id:job,state:'succeeded',phase:'succeeded'}):response(catalog());
 await h.ctx.translationSetupModel('install');await settle();await settle();const body=JSON.parse(h.calls[0].init.body);assert.equal(body.version,2);assert.equal(body.purpose,'translation');assert.equal(body.profile_id,P);assert.equal(body.node_ids[0],s.worker.value);assert.equal(s.setup.rows.length,2);assert.match(s.fields['[data-translation-model-status]'].textContent,/Refused worker: Not accepted/);assert.match(s.fields['[data-translation-model-status]'].textContent,/status is updating/);
 verified=true;h.fire(5000);await settle();await settle();assert.equal(s.setup.rows.length,2);assert.doesNotMatch(s.fields['[data-translation-model-status]'].textContent,/status is updating/);assert.equal(s.fields['[data-translation-setting-enabled]'].checked,true);
});
test('model setup shows download failure reasons and explicit package recovery without exposing unknown details',()=>{
 const h=harness(),s=settings(h);s.setup.rows=[{node_id:s.worker.value,job_id:OP,outcome:'accepted',job:{state:'failed',phase:'failed',reason:'model_download_failed'}},{node_id:'refused',outcome:'engine_unavailable'},{node_id:'unknown',outcome:'PRIVATE /server/path'}];
 h.ctx.translationSetupModelRows(s.setup);const text=s.fields['[data-translation-model-status]'].textContent;assert.match(text,/Failed.*network access.*retry Download and test/);assert.match(text,/packaged speech engine.*Update or repair/);assert.match(text,/check Activity before retrying/);assert.doesNotMatch(text,/PRIVATE|server\/path/);
 h.ctx.subtitleReason=()=>"The model download failed. Check this worker’s network access, then retry Install.";s.setup.rows=[{node_id:s.worker.value,outcome:'model_download_failed'}];h.ctx.translationSetupModelRows(s.setup);assert.match(s.fields['[data-translation-model-status]'].textContent,/retry Download and test/);assert.doesNotMatch(s.fields['[data-translation-model-status]'].textContent,/retry Install/);
});
test('running model test phases show their work instead of queued status',()=>{
 const h=harness(),s=settings(h);
 for(const [phase,label]of [['waiting_for_capacity','Waiting for server capacity'],['decoding','Decoding test audio'],['transcribing','Running speech test'],['validating','Validating'],['publishing','Publishing'],[null,'Running model test'],['unknown-stage','Running model test']]){s.setup.rows=[{node_id:s.worker.value,outcome:'accepted',job:{state:'running',phase}}];h.ctx.translationSetupModelRows(s.setup);assert.match(s.fields['[data-translation-model-status]'].textContent,new RegExp(label));assert.doesNotMatch(s.fields['[data-translation-model-status]'].textContent,/Queued/);}
});
test('model speech test failures retain their distinct trusted recovery actions',()=>{
 const h=harness(),s=settings(h);
 for(const [reason,expected]of [['output_invalid',/model test could not prepare or validate captions.*server storage and logs/],['inference_failed',/model speech test could not run.*Retry Test.*update or repair/],['inference_timeout',/speech test exceeded its time limit/],['decode_failed',/speech test could not decode its sample audio/]]){s.setup.rows=[{node_id:s.worker.value,outcome:'accepted',job:{state:'failed',phase:'failed',reason}}];h.ctx.translationSetupModelRows(s.setup);assert.match(s.fields['[data-translation-model-status]'].textContent,expected);}
});
test('download and test prevents duplicate submission and reopens actions after an observed failure',async()=>{
 const h=harness(),s=settings(h);let finish;
 h.ctx.respond=url=>url.endsWith('/install')?new Promise(resolve=>finish=resolve):url.includes('/jobs/')?response({job_id:OP,state:'failed',phase:'failed',reason:'model_download_failed'}):response({profiles:[profile],models:[],workers:[],server_now_ms:100000});
 const pending=h.ctx.translationSetupModel('install');await settle();assert.match(s.fields['[data-translation-model-status]'].textContent,/Requesting model download and test/);assert.equal(s.fields['[data-translation-install]'].disabled,true);assert.equal(s.fields['[data-translation-test]'].disabled,true);
 await h.ctx.translationSetupModel('install');await h.ctx.translationSetupModel('verify');assert.equal(h.calls.length,1);
 finish(response({version:2,purpose:'translation',model_id:profile.model_id,profile_id:P,operations:[{node_id:s.worker.value,job_id:OP,outcome:'accepted'}],server_now_ms:100000}));await pending;await settle();await settle();
 assert.equal(s.fields['[data-translation-install]'].disabled,false);assert.equal(s.fields['[data-translation-test]'].disabled,false);assert.equal(s.fields['[data-translation-install]'].textContent,'Download and test');assert.match(s.fields['[data-translation-model-status]'].textContent,/model download failed/);
});
test('accepted model work stays busy through a status failure until terminal status is known',async()=>{
 const h=harness(),s=settings(h);let fail=true;
 h.ctx.respond=url=>url.endsWith('/install')?response({version:2,purpose:'translation',model_id:profile.model_id,profile_id:P,operations:[{node_id:s.worker.value,job_id:OP,outcome:'accepted'}],server_now_ms:100000}):url.includes('/jobs/')?(fail?Promise.reject(Error('Temporary status failure')):response({job_id:OP,state:'failed',phase:'failed',reason:'model_download_failed'})):response({profiles:[profile],models:[],workers:[],server_now_ms:100000});
 await h.ctx.translationSetupModel('install');await settle();assert.equal(s.fields['[data-translation-install]'].disabled,true);assert.equal(s.fields['[data-translation-test]'].disabled,true);assert.match(s.fields['[data-translation-model-status]'].textContent,/Retry model status/);
 await h.ctx.translationSetupModel('install');await h.ctx.translationSetupModel('verify');assert.equal(h.calls.filter(call=>call.init.method==='POST').length,1);
 fail=false;const retry=s.fields['[data-translation-model-status]'].children.at(-1);retry.listeners.click();await settle();await settle();assert.equal(s.fields['[data-translation-install]'].disabled,false);assert.match(s.fields['[data-translation-model-status]'].textContent,/model download failed/);
});
test('stale setup clocks refresh once before a new save or model intent',async()=>{
 for(const action of ['save','install']){const h=harness(),s=settings(h);h.clock(242000);let finish;
 h.ctx.respond=url=>url.endsWith('/models')?new Promise(resolve=>finish=resolve):url.endsWith('/install')?response({version:2,purpose:'translation',model_id:profile.model_id,profile_id:P,operations:[],server_now_ms:500000}):response({...s.setup.saved,server_now_ms:500000});
 const pending=action==='save'?h.ctx.saveTranslationSettings():h.ctx.translationSetupModel('install');await settle();assert.equal(h.calls.length,1);assert.match(h.calls[0].url,/subtitle-translation\/models$/);assert.equal(h.calls[0].init.method,'GET');
 s.fields['[data-translation-setting-language]'].value='ja';s.fields['[data-translation-setting-enabled]'].checked=false;s.setup.edit++;
 finish(response({profiles:[profile],models:[],workers:[],server_now_ms:500000}));await pending;await settle();
 const mutation=h.calls.find(call=>call.init.method==='POST'||call.init.method==='PUT');assert.ok(mutation);assert.equal(JSON.parse(mutation.init.body).intent_expires_at_ms,560000);assert.equal(s.fields['[data-translation-setting-language]'].value,'ja');assert.equal(s.fields['[data-translation-setting-enabled]'].checked,false);
 }
});
test('ambiguous setup retries retain their exact old envelope instead of refreshing the clock',async()=>{
 for(const action of ['save','install']){const h=harness(),s=settings(h);h.ctx.respond=()=>Promise.reject(Error('Temporary network failure'));if(action==='save')await h.ctx.saveTranslationSettings();else await h.ctx.translationSetupModel('install');const first=h.calls[0].init.body;h.clock(242000);if(action==='save')await h.ctx.saveTranslationSettings();else await h.ctx.translationSetupModel('install');assert.equal(h.calls.length,2);assert.equal(h.calls[1].init.body,first);assert.equal(h.calls.some(call=>call.init.method==='GET'),false);}
});
test('legacy setup API errors retain retry guidance without revealing arbitrary server text',async()=>{
 const h=harness(),s=settings(h);h.ctx.respond=()=>response({error:'PRIVATE /server/secret failure'},503);await h.ctx.translationSetupModel('install');
 assert.match(s.fields['[data-translation-model-status]'].textContent,/Model setup request failed.*Refresh worker status.*Check Activity/);assert.doesNotMatch(s.fields['[data-translation-model-status]'].textContent,/PRIVATE|secret/);assert.equal(s.fields['[data-translation-install]'].disabled,false);
});
test('catalog refresh recovers initial choices and keeps newer unsaved model language and enablement',async()=>{
 const h=harness(),s=settings(h);s.setup.catalog={profiles:[],models:[],workers:[]};s.setup.saved.profile_id=null;s.setup.saved.model_id=null;s.setup.saved.enabled=false;s.fields['[data-translation-setting-profile]'].value='';s.fields['[data-translation-setting-language]'].value='';s.fields['[data-translation-setting-enabled]'].checked=false;
 h.ctx.respond=()=>response({profiles:[profile],models:[],workers:[],server_now_ms:100000});await h.ctx.translationRefreshSetup();
 assert.equal(s.fields['[data-translation-setting-profile]'].value,P);assert.match(s.fields['[data-translation-setting-profile]'].innerHTML,/measured-model-fixture/);assert.match(s.fields['[data-translation-setting-language]'].innerHTML,/value="ja"/);assert.equal(s.setup.saved.profile_id,null);assert.equal(s.fields['[data-translation-setting-enabled]'].checked,false);assert.equal(h.calls.some(call=>call.init.method==='PUT'),false);
 const other={...profile,profile_id:A,model_id:'Other model',input_languages:['ja']};s.fields['[data-translation-setting-profile]'].value=A;s.fields['[data-translation-setting-language]'].value='fr';s.fields['[data-translation-setting-enabled]'].checked=true;s.setup.edit++;
 h.ctx.respond=()=>response({profiles:[other],models:[],workers:[],server_now_ms:100000});await h.ctx.translationRefreshSetup();assert.equal(s.fields['[data-translation-setting-profile]'].value,A);assert.equal(s.fields['[data-translation-setting-language]'].value,'');assert.doesNotMatch(s.fields['[data-translation-setting-language]'].innerHTML,/value="fr" selected/);assert.equal(s.fields['[data-translation-setting-enabled]'].checked,true);
 s.fields['[data-translation-setting-profile]'].value='';await h.ctx.translationRefreshSetup();assert.equal(s.fields['[data-translation-setting-profile]'].value,'');
});
test('first-use model suggestion requires exactly one active profile and never replaces a saved profile',()=>{
 const h=harness(),s=settings(h);s.setup.saved.profile_id=null;s.setup.saved.model_id=null;s.fields['[data-translation-setting-profile]'].value='';s.setup.catalog.profiles=[profile,{...profile,profile_id:A}];h.ctx.translationSetupChoices(s.setup);assert.equal(s.fields['[data-translation-setting-profile]'].value,'');
 s.setup.choicesLoaded=false;s.setup.catalog.profiles=[{...profile,activation_state:'pending'}];h.ctx.translationSetupChoices(s.setup);assert.equal(s.fields['[data-translation-setting-profile]'].value,'');
 s.setup.choicesLoaded=false;s.setup.saved.profile_id=B;s.fields['[data-translation-setting-profile]'].value=B;s.setup.catalog.profiles=[profile];h.ctx.translationSetupChoices(s.setup);assert.equal(s.fields['[data-translation-setting-profile]'].value,B);assert.match(s.fields['[data-translation-setting-profile]'].innerHTML,/Saved model/);
});
test('disabled translation worker explains missing packaged engine and cached model test state',()=>{
 const h=harness(),s=settings(h);s.setup.catalog={profiles:[profile],models:[],workers:[{node_id:s.worker.value,label:'Server',current:true,eligible:true,observed_at_ms:99900,expires_at_ms:160000,engine:null,capabilities:[{model_id:profile.model_id,profile_id:P,status:'installed'}]}]};h.ctx.translationSetupEvidence(s.setup);
 assert.match(s.fields['[data-translation-workers]'].innerHTML,/Downloaded · Test to finish/);assert.match(s.fields['[data-translation-workers]'].innerHTML,/packaged speech engine.*Update or repair/);assert.match(s.fields['[data-translation-workers]'].innerHTML,/ disabled/);
});
test('late Settings observations cannot overwrite a new account or newer model operation',async()=>{
 for(const change of [(h,s)=>h.ctx.AUTH_GENERATION++,(h,s)=>s.setup.operation={}]){const h=harness(),s=settings(h);s.fields['[data-translation-workers]'].innerHTML='New evidence';let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const refresh=h.ctx.translationRefreshSetup();change(h,s);finish(response({version:1,profiles:[],workers:[],server_now_ms:100000}));await refresh;assert.equal(s.fields['[data-translation-workers]'].innerHTML,'New evidence');}
});

test('choosing a profile after enabled unconfigured save selects eligible named workers',()=>{
 const h=harness(),s=settings(h);s.setup.catalog={models:[],profiles:[profile],workers:[{node_id:s.worker.value,label:'Named worker',current:true,eligible:true,observed_at_ms:99900,expires_at_ms:160000,engine:{adapter_contract:profile.adapter_contract,binary_sha256:E},capabilities:[]}]};
 s.fields['[data-translation-setting-profile]'].value='';h.ctx.translationSetupEvidence(s.setup);assert.doesNotMatch(s.fields['[data-translation-workers]'].innerHTML,/ checked/);
 s.fields['[data-translation-setting-profile]'].value=P;h.ctx.translationSetupEvidence(s.setup);assert.match(s.fields['[data-translation-workers]'].innerHTML,/ checked/);
 s.panel.querySelectorAll=()=>[];h.ctx.translationSetupEvidence(s.setup);assert.doesNotMatch(s.fields['[data-translation-workers]'].innerHTML,/ checked/,'manual deselection stays respected within the same profile');
});

test('heartbeat storms coalesce and ambiguous transport retries preserve the exact envelope',async()=>{
 const h=harness();h.state.demand={id:DEMAND,generation:'1',epoch:'1'};let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);
 const pending=h.ctx.translationHeartbeat(h.state);const storms=Array.from({length:30},()=>h.ctx.translationHeartbeat(h.state));await settle();assert.equal(h.calls.length,1);h.video.currentTime=45;finish(response({code:'network_error',server_now_ms:100000},503));await Promise.all([pending,...storms]);
 const first=h.calls[0].init.body;h.ctx.respond=()=>response({version:1,operation_id:OP,operation_epoch:'1',view_revision:'2',demand_id:DEMAND,demand_generation:'2',server_now_ms:100000});await h.ctx.translationHeartbeat(h.state);assert.equal(h.calls[1].init.body,first);
 await h.ctx.translationHeartbeat(h.state);assert.equal(JSON.parse(h.calls[2].init.body).position_ms,45000);assert.equal(JSON.parse(h.calls[2].init.body).demand_generation,'3');
});
test('existing asset pages consume exact bounded read DTO without creating demand',async()=>{
 const h=harness();h.controls['[data-translation-assets]']=node();h.panel.fields=h.controls;h.ctx.respond=()=>response({version:1,file_id:h.state.fileId,entries:[{asset_id:B,profile_id:P}],next_cursor:null,server_now_ms:100000});
 await h.ctx.translationAssetPage(h.state,'opaque',node());assert.equal(h.controls['[data-translation-assets]'].children.length,1);assert.match(h.calls[0].url,/subtitle-translation\/assets\?cursor=opaque/);assert.equal(h.calls[0].init.method,'GET');
});
test('failed range pages are lease and operation-view fenced without a synthetic range state field',async()=>{
 const h=harness();const ranges=[{core_index:2,range_revision:'4',reason:'inference_failed',retryable:true,attempts:1}];h.ctx.respond=()=>response({version:1,operation_id:OP,operation_epoch:'1',view_revision:'1',entries:ranges,next_cursor:null});
 await h.ctx.translationFailurePage(h.state,'opaque',node());assert.equal(h.calls[0].init.headers['X-Translation-Read-Lease'],LEASE);assert.equal(h.state.operation.failed_ranges[0].core_index,2);
 h.ctx.respond=()=>response({version:1,operation_id:OP,operation_epoch:'2',view_revision:'1',entries:[],next_cursor:null});await h.ctx.translationFailurePage(h.state,'other',node());assert.equal(h.state.operation.failed_ranges[0].core_index,2);assert.match(h.state.error,/another operation view/);
});

test('operation and manifest keep the exact selected spoken language, model and source identity',async()=>{
 const h=harness();h.state.spokenLanguage='de';assert.throws(()=>h.ctx.translationValidateOperation(h.state,operation(h,{spoken_language:'ja'})),/spoken language changed/);assert.throws(()=>h.ctx.translationValidateOperation(h.state,operation(h,{model_sha256:B})),/profile/);
 h.state.operation={...h.state.operation,content_revision:S};h.ctx.respond=()=>response(manifest(h,{content_revision:B}));await assert.rejects(h.ctx.translationManifest(h.state,true),/source or language changed/);assert.equal(h.state.segments.size,0);
});

test('delayed manifest pages cannot roll back Stop or a newer same-epoch operation view',async()=>{
 for(const epoch of ['1','2']){const h=harness();let finish;h.ctx.respond=url=>url.includes('/coverage?')?new Promise(resolve=>finish=resolve):response(manifest(h,{coverage:[],coverage_next_cursor:'later'}));
 const read=h.ctx.translationManifest(h.state,true);await settle();h.ctx.translationAdoptReceipt(h.state,{version:1,operation_id:OP,operation_epoch:epoch,view_revision:'9',state:'stopped',asset_id:A});
 finish(response({version:1,asset_id:A,snapshot_revision:'7',entries:[{start_ms:0,end_ms:20000}],next_cursor:null}));await read;assert.equal(h.state.stopped,true);assert.equal(h.state.operation.view_revision,'9');assert.equal(h.state.operation.operation_epoch,epoch);assert.equal(h.ctx.translationAdoptReceipt(h.state,{version:1,operation_id:OP,operation_epoch:epoch,view_revision:'8',state:'partial',asset_id:A}),false);assert.equal(h.state.stopped,true);}
});
test('changing profile retires old asset while the new preparation receipt is pending',async()=>{
 const h=harness(),other={...structuredClone(profile),profile_id:'8'.repeat(64)};h.state.options.models.push(other);h.controls['[data-translation-profile]'].value=other.profile_id;h.state.spokenLanguage='de';let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);h.ctx.translationPoll=async()=>{};
 const creating=h.ctx.translationStart(h.state);await settle();assert.equal(h.state.assetId,null);assert.equal(h.state.contentRevision,null);assert.equal(h.state.operation,null);assert.equal(h.state.segments.size,0);
 finish(response({version:1,operation_id:'77777777-7777-4777-8777-777777777777',operation_epoch:'1',view_revision:'1',asset_id:null,state:'checking_source',server_now_ms:100000}));await creating;assert.equal(h.state.assetId,null);assert.equal(h.state.profile.profile_id,other.profile_id);
});

test('pending create and Stop cannot overwrite a newly selected asset operation',async()=>{
 for(const action of ['create','stop']){const h=harness();let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);h.ctx.translationPoll=async()=>{};
 const pending=action==='create'?h.ctx.translationStart(h.state):h.ctx.translationStop(h.state);await settle();h.ctx.translationSwitchAsset(h.state);h.state.assetId=B;h.state.operation={operation_id:'77777777-7777-4777-8777-777777777777',operation_epoch:'1',view_revision:'2',state:'partial'};
 finish(response({version:1,operation_id:OP,operation_epoch:'2',view_revision:'9',state:'stopped',asset_id:A,server_now_ms:100000}));await pending;assert.equal(h.state.assetId,B);assert.equal(h.state.operation.state,'partial');assert.equal(h.state.stopped,false);assert.equal(h.state.error==null,true);}
});

test('definitive Stop and scoped Retry refusals retire rejected envelopes for fresh explicit action',async()=>{
 for(const action of ['stop','resume','range']){const h=harness();h.ctx.respond=()=>response({code:'epoch_conflict',server_now_ms:100000},409);h.ctx.translationPoll=async()=>{};const run=()=>action==='stop'?h.ctx.translationStop(h.state):action==='resume'?h.ctx.translationResume(h.state):h.ctx.translationRetryRange(h.state,{core_index:2,range_revision:'4',retryable:true});
 if(action==='resume')h.state.operation.state='stopped';await run();await run();assert.equal(h.calls.length,2);assert.notEqual(JSON.parse(h.calls[0].init.body).request_id,JSON.parse(h.calls[1].init.body).request_id);assert.equal(h.state[action==='stop'?'stopIntent':action==='resume'?'resumeIntent':'rangeIntent'],null);}
});
test('expired local clock on Retry reports Refresh without unhandled work or mutation',async()=>{
 const h=harness();h.state.operation.state='stopped';h.clock(300000);await h.ctx.translationResume(h.state);assert.equal(h.calls.length,0);assert.match(h.state.error,/Refresh translation status/);
});

test('delayed exact save replay cannot replace fresh header clock for the next explicit action',async()=>{
 const h=harness(),s=settings(h);const fresh=500000;h.ctx.respond=()=>response({...s.setup.saved,outcome:'saved',server_now_ms:100000},200,{'X-Translation-Server-Now-Ms':String(fresh)});
 await h.ctx.saveTranslationSettings();assert.equal(s.setup.clock.server,fresh);s.fields['[data-translation-setting-enabled]'].checked=false;
 await h.ctx.saveTranslationSettings();const next=JSON.parse(h.calls[1].init.body);assert.equal(next.intent_expires_at_ms,fresh+60000);assert.notEqual(next.request_id,JSON.parse(h.calls[0].init.body).request_id);
});
test('unresolved Stop suppresses queued and polled heartbeat without hiding cached captions',async()=>{
 const h=harness();h.state.demand={id:DEMAND,generation:'1',epoch:'1'};
 let releaseQueue,stopAttempts=0;
 h.state.queue=new Promise(resolve=>releaseQueue=resolve);
 h.ctx.respond=(url)=>{
  if(url.endsWith('/stop')){if(++stopAttempts===1)throw Error('lost Stop response');return response({version:1,outcome:'stopped',operation_id:OP,operation_epoch:'2',view_revision:'9',state:'stopped',asset_id:A,server_now_ms:100000});}
  if(url.endsWith('/operations/'+OP))return response(operation(h));
  if(url.includes('/manifest?'))return response(manifest(h));
  throw Error('Unexpected renewed interest');
 };
 const queued=h.ctx.translationHeartbeat(h.state);await h.ctx.translationStop(h.state);
 const stopBody=h.calls.find(call=>call.url.endsWith('/stop')).init.body;
 assert.ok(h.state.stopIntent);assert.equal(h.state.pendingStop,false);
 releaseQueue();await queued;await h.ctx.translationPoll(h.state);await h.ctx.translationHeartbeat(h.state);
 assert.equal(h.calls.filter(call=>call.init.method==='PUT').length,0,'neither queued nor fresh heartbeat can renew unresolved Stop');
 assert.equal(h.state.assetId,A);assert.equal(h.state.coverage.length,1);assert.equal(h.controls['[data-translation-use]'].disabled,false);
 assert.match(h.controls['[data-translation-status]'].textContent,/Stop is unconfirmed/);
 await h.ctx.translationStop(h.state);
 assert.equal(h.calls.filter(call=>call.url.endsWith('/stop'))[1].init.body,stopBody,'only explicit retry reuses exact Stop intent');
 assert.equal(h.state.stopIntent,null);assert.equal(h.state.stopped,true);assert.equal(h.state.demand,null);
 await h.ctx.translationHeartbeat(h.state);assert.equal(h.calls.filter(call=>call.init.method==='PUT').length,0);
});
test("a refused heartbeat keeps the acknowledged generation and a demand conflict never latches Stop",async()=>{
 const h=harness();h.state.demand={id:DEMAND,generation:"1",epoch:"1"};h.ctx.translationPoll=async()=>{};
 h.ctx.respond=()=>response({version:1,code:"quota_exceeded",server_now_ms:100000},429);
 await h.ctx.translationHeartbeat(h.state);
 assert.equal(h.state.demand.generation,"1","a refused heartbeat does not advance the demand");
 h.ctx.respond=()=>response({version:1,code:"demand_conflict",server_now_ms:100000},409);
 await h.ctx.translationHeartbeat(h.state);
 const puts=h.calls.filter(call=>call.init.method==="PUT").map(call=>JSON.parse(call.init.body).demand_generation);
 assert.deepEqual(puts,["2","2"],"the next heartbeat is still the acknowledged generation plus one");
 assert.equal(h.state.demand,null);assert.notEqual(h.state.stopped,true,"a lapsed demand is not a Stop");
});

test('same-audio quality handoff preserves durable intent and reprojects only after fresh source and owned position',async()=>{
 const h=harness(),s=segment(h,'same committed caption');s.body.cues[0].start_ms=18000;s.body.cues[0].end_ms=19000;s.bytes=Buffer.from(canonical(s.body));s.reference.digest=sha(s.bytes);
 h.state.segments.set(B,{...s.reference,cues:s.body.cues});h.ctx.translationPrune(h.state);h.player.captionIntent={kind:'translation',asset_id:A};h.ctx.translationApplyCues(h.state);
 assert.equal(h.video._translationTrack.cues[0].startTime,18);
 const demand={id:DEMAND,generation:'4',epoch:'1'},stopIntent={version:1,request_id:DEMAND};h.state.demand=demand;h.state.stopIntent=stopIntent;
 const refreshed=structuredClone(h.state.options);h.ctx.respond=url=>url.endsWith('/options')?response(refreshed):url.includes('/operations/')?response(operation(h)):url.includes('/manifest?')?response(manifest(h,{segments:[s.reference]})):Promise.reject(Error('Unexpected handoff read'));
 h.ctx.translationAttachmentWillChange(h.player);assert.equal(h.video._translationTrack.cues.length,0);assert.equal(h.video._translationTrack.mode,'disabled');
 h.player.offset=10;const token={id:2,audioOrdinal:0,captionPositionReady:false};h.player.mediaAttachment=token;h.ctx.translationAttachmentDidBegin(h.player,token);await settle();
 assert.equal(h.state.demand,demand);assert.equal(h.state.stopIntent,stopIntent);assert.equal(h.state.assetId,A);assert.equal(h.state.positionReady,false);assert.equal(h.video._translationTrack.cues.length,0);
 token.captionPositionReady=true;h.ctx.translationAttachmentPositionReady(h.player,token);await settle();await settle();
 assert.equal(h.state.positionReady,true);assert.equal(h.player.captionIntent.asset_id,A);assert.equal(h.video._translationTrack.cues[0].startTime,8);assert.equal(h.video._translationTrack.cues[0].endTime,9);
 assert.equal(h.calls.some(call=>['POST','PUT','DELETE'].includes(call.init.method)),false,'a quality handoff must not create, release or stop work');
});
test('uncommitted replacement and rollback leave predecessor captions and demand owned by the attached decoder',()=>{
 const h=harness(),demand={id:DEMAND,generation:'1',epoch:'1'};h.state.demand=demand;h.player.captionIntent={kind:'translation',asset_id:A};
 const candidate={...h.player,started:false,mediaAttachment:null,mediaPredecessor:h.player,captionIntent:undefined};h.ctx.PLAYER=candidate;
 assert.equal(h.ctx.translationCurrent(h.state),true);h.ctx.translationMediaEvent(h.state,'timeupdate');h.ctx.translationWatchChanged();
 assert.equal(vm.runInContext('SUBTITLE_TRANSLATION',h.ctx),h.state);assert.equal(h.state.demand,demand);assert.equal(h.player.captionIntent.asset_id,A);
 h.ctx.PLAYER=h.player;assert.equal(h.ctx.translationCurrent(h.state),true);assert.equal(h.state.retired,false);assert.equal(h.player.captionIntent.kind,'translation');
});
test('new player quality commit transfers explicit caption intent but source and audio commits retire it',async()=>{
 const h=harness();h.state.stopped=true;h.player.captionIntent={kind:'translation',asset_id:A};const original=h.player;
 const candidate={...original,started:false,mediaAttachment:null,mediaPredecessor:original,captionIntent:undefined};h.ctx.PLAYER=candidate;
 h.ctx.respond=()=>response(structuredClone(h.state.options));h.ctx.translationAttachmentWillChange(candidate);
 assert.equal(candidate.captionIntent.asset_id,A);const token={id:2,audioOrdinal:0,captionPositionReady:false};candidate.mediaAttachment=token;candidate.mediaPredecessor=null;h.ctx.translationAttachmentDidBegin(candidate,token);await settle();
 assert.equal(h.state.player,candidate);assert.equal(h.state.stopped,true);assert.equal(h.state.assetId,A);
 candidate.audio=[{index:0},{index:3}];candidate.curAudio=1;h.ctx.translationAttachmentWillChange(candidate);assert.equal(h.state.retired,true);assert.equal(candidate.captionIntent.kind,'off');
 const changed=harness();changed.state.stopped=true;changed.ctx.translationAttachmentWillChange({...changed.player,fileId:'123',fileContext:{source_ref:{kind:'local',file_id:'123'}}});assert.equal(changed.state.retired,true);
});
test('late prior-attachment bytes cannot become current again after a same-player rebind',async()=>{
 const h=harness();let finish;h.ctx.respond=url=>url.endsWith('/prior')?new Promise(resolve=>finish=resolve):response(structuredClone(h.state.options));
 const prior=h.ctx.translationRequest(h.state,'/prior');h.ctx.translationAttachmentWillChange(h.player);const token={id:2,audioOrdinal:0,captionPositionReady:false};h.player.mediaAttachment=token;h.ctx.translationAttachmentDidBegin(h.player,token);
 finish(response({version:1,server_now_ms:100000}));await assert.rejects(prior,/Retired translation response/);await settle();assert.equal(h.state.attachment,token);
});
test('Off survives delayed rebind validation and changed source metadata never reuses cached captions',async()=>{
 const h=harness();h.player.captionIntent={kind:'translation',asset_id:A};let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);
 h.ctx.translationAttachmentWillChange(h.player);const token={id:2,audioOrdinal:0,captionPositionReady:true};h.player.mediaAttachment=token;h.ctx.translationAttachmentDidBegin(h.player,token);h.ctx.translationSelectSource(h.player,-1);
 h.ctx.translationPoll=async()=>{};finish(response(structuredClone(h.state.options)));await settle();assert.equal(h.player.captionIntent.kind,'off');h.ctx.translationApplyCues(h.state);assert.notEqual(h.video._translationTrack?.mode,'showing');
 const changed=harness();changed.state.stopped=true;changed.player.captionIntent={kind:'translation',asset_id:A};changed.ctx.respond=()=>response({...structuredClone(changed.state.options),source_size:'999'});changed.ctx.translationAttachmentWillChange(changed.player);const next={id:2,audioOrdinal:0,captionPositionReady:true};changed.player.mediaAttachment=next;changed.ctx.translationAttachmentDidBegin(changed.player,next);await settle();assert.equal(changed.state.retired,true);assert.equal(changed.player.captionIntent.kind,'off');
});
test('audio selection uses the source ordinal rather than its array position and permits signed container origins',async()=>{
 const h=harness();vm.runInContext('SUBTITLE_TRANSLATION=null',h.ctx);h.player.offset=-1.5;h.video.currentTime=.25;h.player.audio=[{index:2,language:'de'},{index:7,language:'ja'}];h.player.curAudio=1;h.player.mediaAttachment.audioOrdinal=7;
 h.ctx.selectedAudioIndex=p=>p.audio[p.curAudio].index;h.ctx.respond=()=>response({...structuredClone(h.state.options),source_origin_ms:-1500,audio_tracks:[{index:7,audio_ordinal:7,source_index:9,normalized_language:'ja',capability:{supported:true}}],assets:[],assets_next_cursor:null});
 await h.ctx.translationOpen(node());assert.equal(vm.runInContext('SUBTITLE_TRANSLATION.audioIndex',h.ctx),7);assert.equal(h.ctx.translationPosition(h.state),0);h.video.currentTime=1.75;assert.equal(h.ctx.translationPosition(h.state),250);
});
test('accepted attachment position waits for current data and rejects obsolete readiness events',()=>{
 const h=harness();h.ctx.PlaybackPolicy.DECODE_LIMIT_DEFAULTS={ttlMs:1000,retestMs:1000};h.ctx.clearEffectiveProcessing=()=>{};h.ctx.playbackSurfaceStep=()=>{};h.ctx.playbackSurfaceGeneration=()=>({});h.ctx.translationAttachmentWillChange=()=>{};h.ctx.translationAttachmentDidBegin=()=>{};
 const ready=[];h.ctx.translationAttachmentPositionReady=(_p,token)=>ready.push(token.id);h.ctx.sharedCaptionAttachmentPositionReady=()=>{};vm.runInContext(fs.readFileSync(path.join(WEB,'player/decode-margin.js'),'utf8'),h.ctx);
 // Establish the predecessor through the real owner so the attachment ordinal
 // and its token agree; the generic harness only seeds a stand-in token.
 const predecessor=h.ctx.beginPlaybackMediaAttachment(h.player);assert.equal(predecessor.token.id,1);
 h.player.offset=10;h.player.controlSeek={targetMs:12000};h.video.readyState=1;h.video.seeking=false;const attachment=h.ctx.beginPlaybackMediaAttachment(h.player);h.ctx.applyPlaybackAttachmentPosition(h.video,h.player,attachment,0);assert.equal(h.video.currentTime,2);assert.deepEqual(ready,[]);
 h.video.readyState=2;h.video.seeking=true;h.video.listeners.seeked();assert.deepEqual(ready,[]);h.video.seeking=false;h.video.listeners.seeked();assert.deepEqual(ready,[2]);
 // Retire a positioned attachment before it has current data. Its captured
 // handler must fail ownership, rather than the already-ready short circuit.
 h.video.readyState=1;const retired=h.ctx.beginPlaybackMediaAttachment(h.player);h.ctx.applyPlaybackAttachmentPosition(h.video,h.player,retired,0);
 assert.equal(retired.token.id,3);assert.equal(retired.token.captionPositionApplied,true);assert.equal(retired.token.captionPositionReady,false);
 const old=h.video.listeners.seeked,next=h.ctx.beginPlaybackMediaAttachment(h.player);assert.equal(next.token.id,4);h.video.readyState=2;old();
 assert.deepEqual(ready,[2]);assert.equal(retired.token.captionPositionReady,false);assert.equal(next.token.captionPositionReady,false);assert.equal(attachment.token.captionPositionReady,true);
});

test('opaque transport delegation reuses the exact byte and provenance validation without local file addressing',async()=>{
 const h=harness(),s=segment(h);h.state.fileId=undefined;let reads=0;h.state.current=()=>true;
 h.state.segmentRead=async(reference,lease)=>{reads++;assert.equal(reference.segment_id,B);assert.equal(lease,h.state.lease);return {bytes:s.bytes};};
 await h.ctx.translationSegment(h.state,s.reference,h.state.lease,0);assert.equal(reads,1);assert.equal(h.calls.length,0);assert.equal(h.state.segments.size,1);
 h.state.segmentRead=async()=>({bytes:Buffer.from('untrusted bytes')});await assert.rejects(h.ctx.translationSegment(h.state,s.reference,h.state.lease,0),/digest mismatch/);
});

function continuityReceipt(h,sent,fields={}){
 return response({version:1,operation_id:OP,operation_epoch:h.state.operation?.operation_epoch||'1',view_revision:'2',state:'partial',asset_id:A,demand_id:DEMAND,demand_generation:sent.demand_generation||'1',continuity_id:sent.continuity_id,server_now_ms:100000,...fields});
}
function loadTransport(h){
 h.ctx.PlaybackPolicy.createHeldKeyCommitter=()=>({cancel(){}});h.ctx.PlaybackPolicy.contractTiming=()=>100;
 vm.runInContext(fs.readFileSync(path.join(WEB,'player/transport.js'),'utf8'),h.ctx);
 Object.assign(h.ctx,{clearEffectiveProcessing(){},supersedePlaybackControlIntent:p=>p.controlIntentGeneration=(p.controlIntentGeneration||0)+1,notifyPlaybackControl(){},playbackSurfaceStep(){},finishPlaybackSeekTelemetry(){},watchPlaybackSeekTelemetry(){},samplePlaybackPresentationClock(){}});
}
test('ordinary clock progress across cores, pause and rate changes retain one continuity UUID',async()=>{
 const h=harness();h.state.demand={id:DEMAND,generation:'1',epoch:'1'};h.ctx.translationPoll=async()=>{};
 h.ctx.respond=(_url,init)=>continuityReceipt(h,JSON.parse(init.body));
 for(const seconds of [5,31,61,92]){h.video.currentTime=seconds;h.ctx.translationMediaEvent(h.state,'timeupdate');await h.ctx.translationHeartbeat(h.state);}
 const continuity=JSON.parse(h.calls[0].init.body).continuity_id;assert.equal(h.ctx.translationId(continuity),true);
 h.video.paused=true;h.ctx.translationMediaEvent(h.state,'pause');await settle();h.video.playbackRate=1.5;h.ctx.translationMediaEvent(h.state,'ratechange');await settle();
 assert.equal(new Set(h.calls.map(call=>JSON.parse(call.init.body).continuity_id)).size,1);
 assert.equal(h.state.continuityId,continuity);assert.equal(h.state.playbackId,'44444444-4444-4444-8444-444444444444');
});
test('viewer seek invalidates continuity before held HLS attachment and at actual execution, while quality retains it',()=>{
 const h=harness();loadTransport(h);const original=h.ctx.translationContinuity(h.state),attachment=h.player.mediaAttachment;
 h.ctx.beginPlaybackControlSeek(h.player,5,false);assert.equal(h.state.continuityId,original,'quality-only destination keeps continuity');
 const pending=h.ctx.beginPlaybackControlSeek(h.player,80,true),announced=h.state.continuityId;
 assert.notEqual(announced,original);assert.equal(h.player.mediaAttachment,attachment,'held decoder still owns its old attachment');
 const successor={...h.player,controlSeek:{...pending}};h.ctx.PLAYER=successor;h.state.handoffTarget=successor;
 assert.equal(h.ctx.markPlaybackControlSeekExecuted(successor,80,h.video),true);
 const executed=h.state.continuityId;assert.notEqual(executed,announced);
 h.ctx.translationPlaybackSeek(successor,{...successor.controlSeek});assert.equal(h.state.continuityId,executed,'copied pending identity rotates once per stage');
 h.ctx.beginPlaybackControlSeek(successor,80,false);assert.equal(h.state.continuityId,executed);
 h.ctx.PLAYER=h.player;h.state.handoffTarget=null;h.player.controlSeek=null;h.ctx.translationPoll=async()=>{};
 h.ctx.translationMediaEvent(h.state,'seeking');assert.notEqual(h.state.continuityId,executed,'native media seek is an explicit discontinuity');
});
test('held old heartbeat acknowledges CAS before one current-continuity renewal and keeps newer presentation',async()=>{
 const h=harness();h.state.demand={id:DEMAND,generation:'1',epoch:'1'};h.ctx.translationPoll=async()=>{};let finish;
 h.ctx.respond=(_url,init)=>h.calls.length===1?new Promise(resolve=>finish=resolve):continuityReceipt(h,JSON.parse(init.body),{view_revision:'8'});
 const pending=h.ctx.translationHeartbeat(h.state);await settle();const sent=JSON.parse(h.calls[0].init.body);
 h.ctx.translationPlaybackSeek(h.player,{sequence:1,intentGeneration:1,targetMs:85000,executed:false,captionViewerSeek:true});h.video.currentTime=85;
 const current=h.state.continuityId;h.state.operation.view_revision='8';finish(continuityReceipt(h,sent));await pending;await settle();if(h.state.heartbeatTask)await h.state.heartbeatTask;
 const puts=h.calls.filter(call=>call.init.method==='PUT').map(call=>JSON.parse(call.init.body));
 assert.equal(puts.length,2);assert.equal(puts[0].demand_generation,'2');assert.equal(puts[1].demand_generation,'3');assert.equal(puts[1].continuity_id,current);assert.notEqual(current,sent.continuity_id);assert.equal(puts[1].position_ms,85000);
 assert.equal(h.state.demand.generation,'3');assert.equal(h.state.operation.view_revision,'8');assert.equal(h.state.continuityId,current);
});
test('uncertain old heartbeat replays exact bytes after seek, then renews once without a failure loop',async()=>{
 const h=harness();h.state.demand={id:DEMAND,generation:'1',epoch:'1'};h.ctx.translationPoll=async()=>{};
 h.ctx.respond=()=>response({code:'network_error',server_now_ms:100000},503);await h.ctx.translationHeartbeat(h.state);await settle();assert.equal(h.calls.length,1);
 const original=h.calls[0].init.body;h.ctx.translationRotateContinuity(h.state);const current=h.state.continuityId;h.video.currentTime=65;
 h.ctx.respond=(_url,init)=>continuityReceipt(h,JSON.parse(init.body));await h.ctx.translationHeartbeat(h.state);await settle();if(h.state.heartbeatTask)await h.state.heartbeatTask;
 assert.equal(h.calls[1].init.body,original);assert.equal(h.calls.length,3);const latest=JSON.parse(h.calls[2].init.body);
 assert.equal(latest.demand_generation,'3');assert.equal(latest.continuity_id,current);assert.equal(latest.position_ms,65000);
});
test('wrong continuity echo cannot advance acknowledged generation or replace the exact pending intent',async()=>{
 for(const echo of [null,'99999999-9999-4999-8999-999999999999']){
  const h=harness();h.state.demand={id:DEMAND,generation:'1',epoch:'1'};
  h.ctx.respond=(_url,init)=>continuityReceipt(h,JSON.parse(init.body),{continuity_id:echo});await h.ctx.translationHeartbeat(h.state);await settle();
  assert.equal(h.state.demand.generation,'1');assert.equal(h.calls.length,1);assert.match(h.state.error,/continuity/);
  const original=h.calls[0].init.body;h.ctx.respond=(_url,init)=>continuityReceipt(h,JSON.parse(init.body));await h.ctx.translationHeartbeat(h.state);
  assert.equal(h.calls[1].init.body,original);assert.equal(h.state.demand.generation,'2');
 }
});
test('late Create and both Retry scopes survive seek and immediately renew their adopted demand',async()=>{
 for(const action of ['create','resume','range']){
  const h=harness();h.ctx.translationPoll=async()=>{};let finish;
  if(action==='create'){h.state.operation=null;h.state.assetId=null;}else if(action==='resume'){h.state.operation.state='stopped';h.state.stopped=true;}
  h.ctx.respond=(_url,init)=>init.method==='POST'?new Promise(resolve=>finish=resolve):continuityReceipt(h,JSON.parse(init.body));
  const pending=action==='create'?h.ctx.translationStart(h.state):action==='resume'?h.ctx.translationResume(h.state):h.ctx.translationRetryRange(h.state,{core_index:2,range_revision:'4',retryable:true});
  await settle();const sent=JSON.parse(h.calls[0].init.body);h.video.currentTime=75;h.ctx.translationMediaEvent(h.state,'seeking');const current=h.state.continuityId;
  finish(continuityReceipt(h,sent));await pending;await settle();if(h.state.heartbeatTask)await h.state.heartbeatTask;
  assert.equal(h.state.operation.operation_id,OP);assert.equal(h.state.demand.generation,'2');assert.equal(h.state.continuityId,current);assert.notEqual(current,sent.continuity_id);
  const renewed=JSON.parse(h.calls.find(call=>call.init.method==='PUT').init.body);assert.equal(renewed.continuity_id,current);assert.equal(renewed.position_ms,75000);
 }
});
test('late Stop survives seek but Create and Retry cannot overwrite a replacement producer',async()=>{
 const stopped=harness();stopped.ctx.translationPoll=async()=>{};let finishStop;stopped.ctx.respond=()=>new Promise(resolve=>finishStop=resolve);
 const stopping=stopped.ctx.translationStop(stopped.state);await settle();stopped.ctx.translationMediaEvent(stopped.state,'seeking');finishStop(response({version:1,operation_id:OP,operation_epoch:'2',view_revision:'9',state:'stopped',asset_id:A,server_now_ms:100000}));await stopping;assert.equal(stopped.state.stopped,true);
 for(const action of ['create','resume','range'])for(const replacement of ['asset','profile']){
  const h=harness();h.ctx.translationPoll=async()=>{};let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);
  if(action==='resume')h.state.operation.state='stopped';const pending=action==='create'?h.ctx.translationStart(h.state):action==='resume'?h.ctx.translationResume(h.state):h.ctx.translationRetryRange(h.state,{core_index:2,range_revision:'4',retryable:true});
  await settle();const sent=JSON.parse(h.calls[0].init.body);h.ctx.translationSwitchAsset(h.state);h.state.assetId=B;
  h.state.operation={operation_id:'77777777-7777-4777-8777-777777777777',operation_epoch:'1',view_revision:'2',state:'partial'};if(replacement==='profile')h.state.profile={...h.state.profile,profile_id:'8'.repeat(64)};
  finish(continuityReceipt(h,sent));await pending;assert.equal(h.state.assetId,B);assert.equal(h.state.operation.operation_id,'77777777-7777-4777-8777-777777777777');assert.equal(h.state.demand,null);assert.equal(h.state.error==null,true);
 }
});
test('continuous quality handoff resolves an uncertain Create with the original UUID and body once position is proven',async()=>{
 const h=harness();h.state.operation=null;h.state.assetId=null;h.ctx.translationPoll=async()=>{};
 h.ctx.respond=(url,init)=>{
  if(url.endsWith('/options'))return response(structuredClone(h.state.options));
  if(h.calls.filter(call=>call.init.method==='POST').length===1)return new Promise((_resolve,reject)=>init.signal.addEventListener('abort',()=>reject(Error('attachment replaced')),{once:true}));
  return continuityReceipt(h,JSON.parse(init.body));
 };
 const creating=h.ctx.translationStart(h.state);await settle();const original=h.calls[0].init.body,continuity=h.state.continuityId;
 h.ctx.translationAttachmentWillChange(h.player);const token={id:2,audioOrdinal:0,captionPositionReady:false};h.player.mediaAttachment=token;h.ctx.translationAttachmentDidBegin(h.player,token);await creating;await settle();
 assert.equal(h.calls.filter(call=>call.init.method==='POST').length,1);assert.ok(h.state.createIntent);token.captionPositionReady=true;h.ctx.translationAttachmentPositionReady(h.player,token);await settle();await settle();
 const posts=h.calls.filter(call=>call.init.method==='POST');assert.equal(posts.length,2);assert.equal(posts[1].init.body,original);assert.equal(h.state.continuityId,continuity);assert.equal(h.state.demand.id,DEMAND);assert.equal(h.state.createIntent,null);
});
test('preferred speech profiles honor saved Small task pairs, new Base defaults and strict server hints',()=>{
 const h=harness(),base={...profile,profile_id:'0'.repeat(64),model_id:'base',default:true,label:'Base multilingual'},baseCaptions={...base,profile_id:'1'.repeat(64),task:'transcribe'},small={...profile,model_id:'small',default:false},smallCaptions={...small,profile_id:A,task:'transcribe'};
 const options={models:[base,baseCaptions,small,smallCaptions],default_profile_id:base.profile_id};
 assert.equal(h.ctx.translationPreferredProfile(options,'translate').profile_id,base.profile_id);options.default_profile_id=small.profile_id;
 assert.equal(h.ctx.translationPreferredProfile(options,'transcribe').profile_id,smallCaptions.profile_id);
 assert.equal(h.ctx.translationPreferredProfile(options,'translate',baseCaptions.profile_id).profile_id,base.profile_id);
 options.default_profile_id='broken';assert.throws(()=>h.ctx.translationPreferredProfile(options,'translate'),/Invalid preferred/);
 options.default_profile_id='7'.repeat(64);assert.throws(()=>h.ctx.translationPreferredProfile(options,'translate',small.profile_id),/Invalid preferred/);
});
test('first unedited setup follows saved Small model or Base hint without changing enablement or later edits',()=>{
 const h=harness(),s=settings(h),base={...profile,profile_id:'0'.repeat(64),model_id:'base',default:true},small={...profile,model_id:'small',default:false};
 s.setup.catalog.profiles=[base,small];s.setup.catalog.default_profile_id=base.profile_id;s.setup.saved.model_id='small';s.setup.saved.profile_id=null;s.fields['[data-translation-setting-profile]'].value='';s.fields['[data-translation-setting-enabled]'].checked=false;
 h.ctx.translationSetupChoices(s.setup);assert.equal(s.fields['[data-translation-setting-profile]'].value,P);assert.equal(s.fields['[data-translation-setting-enabled]'].checked,false);
 s.setup.edit++;s.fields['[data-translation-setting-profile]'].value=base.profile_id;h.ctx.translationSetupChoices(s.setup);assert.equal(s.fields['[data-translation-setting-profile]'].value,base.profile_id);
 s.setup.edit=0;s.setup.choicesLoaded=false;s.setup.saved.model_id=null;s.fields['[data-translation-setting-profile]'].value='';h.ctx.translationSetupChoices(s.setup);assert.equal(s.fields['[data-translation-setting-profile]'].value,base.profile_id);
 s.setup.catalog.default_profile_id='8'.repeat(64);assert.throws(()=>h.ctx.translationSetupChoices(s.setup),/Invalid preferred/);assert.equal(s.fields['[data-translation-setting-profile]'].value,base.profile_id);
});
