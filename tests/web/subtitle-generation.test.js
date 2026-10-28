#!/usr/bin/env node
"use strict";
const assert=require("node:assert/strict");
const fs=require("node:fs");
const path=require("node:path");
const vm=require("node:vm");
const test=require("node:test");
const acorn=require("../vendor/acorn.js");
const WEB=path.join(__dirname,"../../crates/plurxd/src/web");
function declarations(file,predicate){const source=fs.readFileSync(path.join(WEB,file),"utf8");return acorn.parse(source,{ecmaVersion:"latest"}).body.filter(node=>node.type==="FunctionDeclaration"&&predicate(node.id.name)).map(node=>source.slice(node.start,node.end)).join("\n");}
function element(value=""){let html="",text="";return {value,checked:false,disabled:false,isConnected:true,hidden:false,addEventListener(){},insertAdjacentHTML(_where,s){html+=s;},get innerHTML(){return html;},set innerHTML(v){html=v;text="";},get textContent(){return text;},set textContent(v){text=String(v);html="";}};}
const model={id:"base",label:"Base <model>",languages:["en","fr"],download_bytes:100,sha256:"a".repeat(64),source:"https://example.test/model",license:"MIT"};
const config={version:1,revision:"1",mode:"managed",enabled:true,interval_mins:0,language:"en",model_id:"base",advanced:{command:"whisper-cli",model_path:""},mode_status:"ok"};
const options={version:1,file_id:"9007199254740993",source_size:"9007199254740993",source_mtime:"9007199254740995",audio_tracks:[{index:0,language:"eng",normalized_language:"en",title:"Original",default:true},{index:2,language:"en",normalized_language:"en",title:"Commentary",default:false},{index:5,language:"und",normalized_language:null,title:"Untagged",default:false}],mode:"managed",model_id:"base",language:"en",enabled:true,captions:[]};
function harness(){
  const nodes={},timers=[],calls=[],workers=[];
  const ctx=vm.createContext({console,Date,Map,Set,Number,String,Promise,Error,TypeError,encodeURIComponent,AUTH_GENERATION:1,TOKEN:"account-one",ME:{id:1,username:"admin-one",created_at:1,is_admin:true},PAGE_RENDER_GENERATION:1,location:{hash:"#/item/7"},PLAYER:null,PREPLAY:{},ACTIVITY_VIEW:{tab:"status"},DURABLE_ACTIVITY:{retries:new Map(),epoch:0},closeActivityInspector(){},history:{state:null,replaceState(state,_title,route){this.state=state;ctx.location.hash=route;}},setTimeout:fn=>timers.push(fn),document:{getElementById:id=>nodes[id]||null,querySelectorAll:selector=>selector.endsWith(":checked")?workers.filter(node=>node.checked):workers},api:async(path,init)=>{calls.push({path,init});return ctx.respond(path,init);},respond:()=>{throw Error("unexpected request");},esc:v=>String(v).replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;").replaceAll('"',"&quot;").replaceAll("'","&#39;"),prePlayPreview(){},samePlaybackFile:(player,context)=>!!player&&player.fileId===context.source_ref.file_id,subFactLabel:row=>row.title||row.language||String(row.index),setSub:index=>calls.push({setSub:index})});
  vm.runInContext(fs.readFileSync(path.join(WEB,"core/file-context.js"),"utf8"),ctx);
  vm.runInContext(declarations("pages/settings-developer.js",name=>name.startsWith("subtitle")||["openSubtitleTranscription","saveSubtitleTranscription","refreshSubtitleModels","pollSubtitleModelOperations","retrySubtitleModelOperations"].includes(name)),ctx);
  vm.runInContext(declarations("detail/preplay-selection.js",name=>name.startsWith("subtitleGeneration")||["prePlaySelection","playbackSelection","setPrePlay","generateSubtitles","submitSubtitleGeneration","pollSubtitleGeneration","completeSubtitleGeneration","useGeneratedSubtitles","retrySubtitleGenerationStatus"].includes(name)),ctx);
  vm.runInContext(declarations("pages/activity.js",name=>["activityAccountKey","activityJobRoute","normalizeActivityJobRoute","viewActivity","resetActivityWork"].includes(name)),ctx);
  ctx.file={id:options.file_id,fileContext:ctx.localPlaybackFileContext(options.file_id),subtitle_streams:[],downloaded_subtitles:[],playback_defaults:{audio:{selected_index:0}}};ctx.WATCH_ITEM_PAGE={id:"7",files:[ctx.file]};
  function node(id,value=""){return nodes[id]=element(value);}
  function settings(){ctx.location.hash="#/settings/maintenance";const panel=node("subtitle-transcription-form");panel._subtitleSetup={capture:ctx.subtitleUiCapture(),catalog:{models:[model],workers:[]},edit:0,revision:"1"};for(const [id,value]of Object.entries({"st-mode":"managed","st-model-id":"base","st-language":"en","st-interval":"0","st-command":"whisper-cli","st-model":""}))node(id,value);node("st-enabled").checked=true;node("st-status");node("st-operation-status");node("st-model-evidence");return panel;}
  function generation(){const panel=node(`subtitle-generate-${options.file_id}`);const state={capture:ctx.subtitleUiCapture(),panel,context:ctx.file.fileContext,page:ctx.WATCH_ITEM_PAGE,file:ctx.file,selection:ctx.prePlaySelection(ctx.file.fileContext),options:structuredClone(options),models:[model]};panel._subtitleGeneration=state;node(`sg-a-${options.file_id}`,"2");node(`sg-l-${options.file_id}`,"en");node(`sg-status-${options.file_id}`);node(`pp-sub-field-${options.file_id}`);return state;}
  return {ctx,nodes,node,timers,calls,workers,settings,generation};
}
const settle=()=>new Promise(resolve=>setImmediate(resolve));
test("managed setup preserves enable despite unavailable readiness and escapes worker evidence",()=>{
  const h=harness();const html=h.ctx.subtitleTranscriptionForm(config,{models:[model],workers:[{node_id:"node",label:"<worker>",eligible:false,observed_at_ms:0,expires_at_ms:0,engine:{status:"available"},models:[{model_id:"base",status:"verified",verified_at_ms:1}]}]});
  assert.match(html,/id="st-enabled" type="checkbox" checked/);assert.match(html,/>Save<\/button>/);assert.match(html,/&lt;worker&gt;/);assert.match(html,/observation is stale; readiness is unknown/);assert.doesNotMatch(html,/Save<\/button>.*disabled/);assert.doesNotMatch(html,/>verified · engine/);
});
test("cancelled cached model stays unverified with Download and test available",()=>{
  const h=harness();const now=Date.now();const worker={node_id:"worker",label:"Named worker",eligible:true,observed_at_ms:now,expires_at_ms:now+60000,engine:{status:"available"},models:[{model_id:"base",status:"unverified_cache",verified_at_ms:null}]};
  const html=h.ctx.subtitleModelEvidence({models:[model],workers:[worker]},"base");
  assert.match(html,/Download cached · download and test to finish setup/);assert.doesNotMatch(html,/ · verified · engine|tested /);
  assert.match(html,/name="st-worker" value="worker" checked/);
  assert.match(html,/<button class="sm" onclick="subtitleModelOperation\('install',this\)">Download and test<\/button>/);
  assert.match(html,/<button class="sm" onclick="subtitleModelOperation\('verify',this\)">Test selected model<\/button>/);
  assert.equal(h.ctx.subtitleModelStatus("PRIVATE /worker/path"),"unknown");
});
test("managed Save uses CAS and preserves newer unsaved edits",async()=>{
  const h=harness(),panel=h.settings();let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const pending=h.ctx.saveSubtitleTranscription(element());h.nodes["st-language"].value="fr";h.ctx.subtitleSetupEdited();finish({...config,revision:"2"});await pending;
  assert.equal(h.calls[0].path,"/subtitle-transcription/managed");assert.equal(h.calls[0].init.body.expected_revision,"1");assert.equal(h.calls[0].init.body.enabled,true);assert.equal(h.calls[0].init.body.language,"en");assert.equal(h.nodes["st-language"].value,"fr");assert.equal(panel.innerHTML,"");assert.equal(panel._subtitleSetup.revision,"2");assert.match(h.nodes["st-status"].textContent,/newer edits are still unsaved/);
});
test("stale account and page responses cannot paint Settings",async()=>{
  for(const change of [ctx=>ctx.AUTH_GENERATION++,ctx=>ctx.PAGE_RENDER_GENERATION++]){const h=harness(),panel=h.settings();let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const pending=h.ctx.saveSubtitleTranscription(element());change(h.ctx);finish(config);await pending;assert.equal(panel.innerHTML,"");assert.equal(h.nodes["st-status"].textContent,"Saving…");}
});
test("late Settings load cannot replace a later visit or account",async()=>{
  const h=harness(),panel=h.settings();let results=[];h.ctx.respond=()=>new Promise(resolve=>results.push(resolve));const pending=h.ctx.openSubtitleTranscription(element());h.ctx.AUTH_GENERATION++;results[0](config);results[1]({models:[model],workers:[]});await pending;assert.equal(panel.innerHTML,"");
});
test("installation refresh changes only observations and retains unsaved controls",async()=>{
  const h=harness(),panel=h.settings();h.nodes["st-enabled"].checked=false;h.nodes["st-language"].value="fr";h.nodes["st-interval"].value="30";const worker=element("node-one");worker.checked=true;h.workers.push(worker);h.ctx.respond=path=>path.endsWith("/install")?{operations:[{node_id:"node-one",job_id:"install-one",outcome:"accepted"}]}:path.includes("/jobs/")?{job_id:"install-one",node_id:"node-one",state:"succeeded",phase:"succeeded"}:{models:[model],workers:[]};
  await h.ctx.subtitleModelOperation("install",element());await settle();assert.equal(h.calls[0].init.body.node_ids[0],"node-one");assert.equal(h.nodes["st-enabled"].checked,false);assert.equal(h.nodes["st-language"].value,"fr");assert.equal(h.nodes["st-interval"].value,"30");assert.equal(panel.innerHTML,"");assert.match(h.nodes["st-operation-status"].innerHTML,/activity\/jobs\/install-one/);
});
test("an installation response from a prior account cannot paint status",async()=>{
  const h=harness();h.settings();const worker=element("node-one");worker.checked=true;h.workers.push(worker);let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const pending=h.ctx.subtitleModelOperation("install",element());h.ctx.AUTH_GENERATION++;finish({operations:[{node_id:"node-one",job_id:"install-one",outcome:"accepted"}]});await pending;assert.equal(h.nodes["st-operation-status"].innerHTML,"");
});
test("chosen same-language nondefault audio and exact source IDs reach the request",async()=>{
  const h=harness(),state=h.generation();h.ctx.respond=path=>path.includes("/jobs/")?{job_id:"job-one",file_id:options.file_id,artifact_key:"artifact",source_size:options.source_size,source_mtime:options.source_mtime,state:"queued",phase:"queued",attempt:0,fence:0,updated_at_ms:1}:{outcome:"accepted",job_id:"job-one",artifact_key:"artifact",file_id:options.file_id,source_size:options.source_size,source_mtime:options.source_mtime};
  await h.ctx.submitSubtitleGeneration(options.file_id,element());await settle();const request=h.calls[0];assert.equal(request.path,`/files/${options.file_id}/subtitles/transcribe`);assert.equal(request.init.body.audio_index,2);assert.equal(request.init.body.language,"en");assert.equal(request.init.body.source_size,options.source_size);assert.equal(request.init.body.source_mtime,options.source_mtime);assert.equal(state.jobId,"job-one");assert.match(h.nodes[`sg-status-${options.file_id}`].innerHTML,/activity\/jobs\/job-one/);
});
test("untagged audio requires explicit supported spoken language and tagged mismatch refuses",async()=>{
  const h=harness(),state=h.generation();h.nodes[`sg-a-${options.file_id}`].value="5";h.nodes[`sg-l-${options.file_id}`].value="";await h.ctx.submitSubtitleGeneration(options.file_id,element());assert.equal(h.calls.length,0);assert.match(h.nodes[`sg-status-${options.file_id}`].textContent,/Choose a spoken language/);
  const html=h.ctx.subtitleGenerationForm({...state,file:{playback_defaults:{audio:{selected_index:5}}}});assert.match(html,/This audio is untagged/);assert.doesNotMatch(html,/<option value="en" selected/);
  h.nodes[`sg-a-${options.file_id}`].value="0";h.nodes[`sg-l-${options.file_id}`].value="fr";await h.ctx.submitSubtitleGeneration(options.file_id,element());assert.equal(h.calls.length,0);assert.match(h.nodes[`sg-status-${options.file_id}`].textContent,/does not match/);
});
test("queue refusal and escaped HTTP failures never look like accepted work",async()=>{
  const h=harness();h.generation();h.ctx.respond=()=>({outcome:"queue_full"});await h.ctx.submitSubtitleGeneration(options.file_id,element());assert.match(h.nodes[`sg-status-${options.file_id}`].textContent,/queue is full/);assert.equal(h.nodes[`sg-status-${options.file_id}`].innerHTML,"");h.ctx.respond=()=>{throw Error("<script>failure</script>");};await h.ctx.submitSubtitleGeneration(options.file_id,element());assert.equal(h.nodes[`sg-status-${options.file_id}`].textContent,"<script>failure</script>");assert.equal(h.nodes[`sg-status-${options.file_id}`].innerHTML,"");
});
function completed(h,selection=null){h.ctx.PREPLAY[options.file_id]=selection;const state=h.generation(),job={job_id:"job-one",state:"succeeded",artifact_key:"artifact",result_ref:"artifact",file_id:options.file_id,source_size:options.source_size,source_mtime:options.source_mtime};state.jobId="job-one";state.artifact="artifact";state.request={};state.jobObservation=job;h.ctx.respond=()=>({...options,subtitle_streams:[{index:3,language:"en",title:"Generated"}],captions:[{index:3,artifact_key:"artifact",source_size:options.source_size,source_mtime:options.source_mtime}]});return {state,job};}
test("published completion preserves Off, existing track, and the active video until explicit Use",async()=>{
  for(const subtitle of [-1,1]){const h=harness(),selection={audio:2,subtitle},before=structuredClone(selection),{state,job}=completed(h,selection),video={id:"same-video"};h.ctx.PLAYER={fileId:options.file_id,started:true,curSub:subtitle,subs:[],video};await h.ctx.completeSubtitleGeneration(state,state.request,job);assert.deepEqual(selection,before);assert.equal(h.ctx.PLAYER.video,video);assert.equal(h.ctx.PLAYER.curSub,subtitle);assert.equal(state.readyIndex,3);assert.match(h.nodes[`sg-status-${options.file_id}`].innerHTML,/Captions ready/);assert.equal(h.calls.some(call=>"setSub"in call),false);h.ctx.useGeneratedSubtitles(options.file_id);assert.equal(h.calls.at(-1).setSub,3);}
});
test("completion rejects replaced source and missing publication without advertising Ready",async()=>{
  for(const replacement of [{...options,source_size:"9007199254740994"},{...options,captions:[]}]){const h=harness(),{state,job}=completed(h);h.ctx.respond=()=>replacement;await assert.rejects(h.ctx.completeSubtitleGeneration(state,state.request,job));assert.equal(state.readyIndex,undefined);assert.doesNotMatch(h.nodes[`sg-status-${options.file_id}`].innerHTML,/Captions ready/);}
});
test("completion responses are fenced by account, page, source page and selection",async()=>{
  for(const change of [h=>h.ctx.AUTH_GENERATION++,h=>h.ctx.PAGE_RENDER_GENERATION++,h=>h.ctx.WATCH_ITEM_PAGE={...h.ctx.WATCH_ITEM_PAGE},h=>h.ctx.PREPLAY[options.file_id]={audio:2,subtitle:-1}]){const h=harness(),{state,job}=completed(h);let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const pending=h.ctx.completeSubtitleGeneration(state,state.request,job);change(h);finish({...options,captions:[{index:3,artifact_key:"artifact",source_size:options.source_size,source_mtime:options.source_mtime}]});await pending;assert.equal(state.readyIndex,undefined);assert.equal(h.calls.length,1);}
});
test("old attempt progress cannot replace the current claim",async()=>{
  const h=harness(),{state,job}=completed(h);state.jobObservation={...job,attempt:2,fence:8,updated_at_ms:100};h.ctx.respond=()=>({...job,state:"running",phase:"transcribing",attempt:1,fence:4,updated_at_ms:200});await h.ctx.pollSubtitleGeneration(state,state.request);assert.equal(state.jobObservation.attempt,2);assert.equal(h.nodes[`sg-status-${options.file_id}`].innerHTML,"");assert.equal(h.timers.length,1);
});
test("Activity job link chooses Jobs and exact inspection after navigation and reload",async()=>{
  const h=harness();let inspected=[];Object.assign(h.ctx,{layoutChrome(){},setPagePhase(){},ACTIVITY_SNAPSHOT:null,DVR_PAGE:{},renderActivityBody:async()=>{},setPageTimer(){},setActivityTab:tab=>h.ctx.ACTIVITY_VIEW.tab=tab,showDurableJob:id=>inspected.push(id)});assert.equal(h.ctx.normalizeActivityJobRoute("#/activity/jobs/job-one"),"#/activity");await h.ctx.viewActivity(1);assert.equal(h.ctx.ACTIVITY_VIEW.tab,"jobs");assert.equal(inspected[0],"job-one");h.ctx.ACTIVITY_VIEW={tab:"status"};await h.ctx.viewActivity(1);assert.equal(inspected[1],"job-one");h.ctx.ME={is_admin:false};h.ctx.ACTIVITY_VIEW={tab:"status"};await h.ctx.viewActivity(1);assert.equal(inspected.length,2);assert.equal(h.ctx.activityJobRoute("#/activity/jobs/../bad"),null);
});
test("server normalized audio language accepts ISO aliases and region tags without a client alias table",async()=>{
  for(const [raw,normalized]of [["por","pt"],["swe","sv"],["en","en"],["pt-BR","pt"],["und",null]]){
    const h=harness(),state=h.generation();state.options.audio_tracks=[{index:2,language:raw,normalized_language:normalized}];state.options.supported_languages=["en","pt","sv"];h.nodes[`sg-l-${options.file_id}`].value=normalized||"pt";h.ctx.respond=()=>({outcome:"no_demand"});await h.ctx.submitSubtitleGeneration(options.file_id,element());assert.equal(h.calls.length,1,raw);assert.equal(h.calls[0].init.body.language,normalized||"pt");
    if(normalized){h.nodes[`sg-l-${options.file_id}`].value=normalized==="en"?"pt":"en";await h.ctx.submitSubtitleGeneration(options.file_id,element());assert.equal(h.calls.length,1,raw+" mismatch");}
  }
});
test("Save and installation complete independently without replacing setup or operation status",async()=>{
  const h=harness(),panel=h.settings(),status=h.nodes["st-operation-status"],worker=element("node-one");worker.checked=true;h.workers.push(worker);let install,save;
  h.ctx.respond=path=>path.endsWith("/install")?new Promise(resolve=>install=resolve):path.endsWith("/managed")?new Promise(resolve=>save=resolve):path.includes("/jobs/")?{job_id:"install-one",node_id:"node-one",state:"succeeded",phase:"succeeded"}:{models:[model],workers:[]};
  const operation=h.ctx.subtitleModelOperation("install",element()),saving=h.ctx.saveSubtitleTranscription(element());save({...config,revision:"2"});await saving;install({operations:[{node_id:"node-one",job_id:"install-one",outcome:"accepted"}]});await operation;await settle();assert.equal(h.nodes["st-operation-status"],status);assert.match(status.innerHTML,/activity\/jobs\/install-one/);assert.equal(panel.innerHTML,"");assert.equal(panel._subtitleSetup.revision,"2");
});
test("active playback selection changes fence a pending caption completion",async()=>{
  for(const change of [player=>player.curSub=-1,player=>player.curAudio=1,player=>player.preplay={audio:2,subtitle:1}]){
    const h=harness(),{state,job}=completed(h),player={fileId:options.file_id,started:true,preplay:{audio:0,subtitle:1},curAudio:0,curSub:1};h.ctx.PLAYER=player;Object.assign(state,{player,playerSelection:player.preplay,playerAudio:0,playerSubtitle:1});let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const pending=h.ctx.completeSubtitleGeneration(state,state.request,job);change(player);finish({...options,subtitle_streams:[{index:3}],captions:[{index:3,artifact_key:"artifact",source_size:options.source_size,source_mtime:options.source_mtime}]});await pending;assert.equal(state.readyIndex,undefined);
  }
});
test("late generation options cannot paint a different account or title visit",async()=>{
  for(const change of [ctx=>ctx.AUTH_GENERATION++,ctx=>ctx.PAGE_RENDER_GENERATION++]){const h=harness(),panel=h.node(`subtitle-generate-${options.file_id}`);let results=[];h.ctx.respond=()=>new Promise(resolve=>results.push(resolve));const pending=h.ctx.generateSubtitles(options.file_id,element());change(h.ctx);results[0](options);results[1]({models:[model],workers:[]});await pending;assert.equal(panel.innerHTML,"");assert.equal(panel.textContent,"Loading audio and model choices…");}
});
test("Activity logout reset retires persisted job intent while preserving unrelated history state",async()=>{
  const h=harness();h.ctx.history.state={scroll:42};h.ctx.normalizeActivityJobRoute("#/activity/jobs/account-one-job");assert.equal(h.ctx.history.state.scroll,42);h.ctx.resetActivityWork();assert.equal(h.ctx.history.state.plurxActivityJob,undefined);assert.equal(h.ctx.history.state.scroll,42);h.ctx.AUTH_GENERATION++;h.ctx.TOKEN="account-two";let inspected=[];Object.assign(h.ctx,{layoutChrome(){},setPagePhase(){},ACTIVITY_SNAPSHOT:null,DVR_PAGE:{},renderActivityBody:async()=>{},setPageTimer(){},setActivityTab:tab=>h.ctx.ACTIVITY_VIEW.tab=tab,showDurableJob:id=>inspected.push(id)});await h.ctx.viewActivity(1);assert.deepEqual(inspected,[]);assert.equal(h.ctx.ACTIVITY_VIEW.tab,"status");
});
test("transient progress failure exposes exact Activity link and recovers through Retry status",async()=>{
  const h=harness(),{state}=completed(h);state.jobObservation=null;let fail=true;h.ctx.respond=()=>{if(fail)throw Error("Temporary network failure <retry>");return {job_id:"job-one",file_id:options.file_id,artifact_key:"artifact",source_size:options.source_size,source_mtime:options.source_mtime,state:"running",phase:"transcribing",attempt:1,fence:1,updated_at_ms:1};};await h.ctx.pollSubtitleGeneration(state,state.request);const status=h.nodes[`sg-status-${options.file_id}`];assert.match(status.innerHTML,/Temporary network failure &lt;retry&gt;/);assert.match(status.innerHTML,/Retry status/);assert.match(status.innerHTML,/activity\/jobs\/job-one/);fail=false;h.ctx.retrySubtitleGenerationStatus(options.file_id);await settle();assert.match(status.innerHTML,/Transcribing speech/);assert.equal(h.timers.length,1);
});
test("partial model operation refusals survive progress and transient status errors",async()=>{
  const h=harness(),panel=h.settings(),setup=panel._subtitleSetup,worker=element("accepted-node");worker.checked=true;h.workers.push(worker);setup.catalog.workers=[{node_id:"accepted-node",label:"Available worker"},{node_id:"refused-node",label:"Refused worker"}];let fail=false;
  h.ctx.respond=path=>{if(path.endsWith("/install"))return {operations:[{node_id:"accepted-node",job_id:"install-one",outcome:"accepted"},{node_id:"refused-node",outcome:"queue_full"}]};if(path.includes("/jobs/")){if(fail)throw Error("Temporary status failure");return {job_id:"install-one",node_id:"accepted-node",state:"running",phase:"verifying"};}return {models:[model],workers:[]};};await h.ctx.subtitleModelOperation("install",element());await settle();const status=h.nodes["st-operation-status"];assert.match(status.innerHTML,/Refused worker: queue full/);assert.match(status.innerHTML,/Verifying model/);fail=true;await h.ctx.pollSubtitleModelOperations(setup,["install-one"]);assert.match(status.innerHTML,/queue full/);assert.match(status.innerHTML,/Retry status/);assert.match(status.innerHTML,/activity\/jobs\/install-one/);fail=false;h.ctx.retrySubtitleModelOperations();await settle();assert.match(status.innerHTML,/queue full/);assert.match(status.innerHTML,/Verifying model/);
});
test("older Activity history entries cannot resurrect another account's inspector after Back",async()=>{
  const h=harness();let inspected=[];Object.assign(h.ctx,{layoutChrome(){},setPagePhase(){},ACTIVITY_SNAPSHOT:null,DVR_PAGE:{},renderActivityBody:async()=>{},setPageTimer(){},setActivityTab:tab=>h.ctx.ACTIVITY_VIEW.tab=tab,showDurableJob:id=>inspected.push(id)});h.ctx.normalizeActivityJobRoute("#/activity/jobs/account-one-job");const older={...h.ctx.history.state};h.ctx.resetActivityWork();h.ctx.AUTH_GENERATION++;h.ctx.TOKEN="account-two";h.ctx.ME={id:2,username:"admin-two",created_at:2,is_admin:true};h.ctx.history.state=older;await h.ctx.viewActivity(1);assert.equal(inspected.length,0);assert.equal(h.ctx.ACTIVITY_VIEW.tab,"status");h.ctx.normalizeActivityJobRoute("#/activity/jobs/account-two-job");await h.ctx.viewActivity(1);assert.deepEqual(inspected,["account-two-job"]);h.ctx.ACTIVITY_VIEW={tab:"status"};await h.ctx.viewActivity(1);assert.deepEqual(inspected,["account-two-job","account-two-job"]);
});
test("unavailable catalog observations never hide saved enable or Save",async()=>{
  const h=harness(),panel=h.settings();h.ctx.respond=path=>path.endsWith("/managed")?config:Promise.reject(Error("No worker observations"));await h.ctx.openSubtitleTranscription(element());assert.match(panel.innerHTML,/id="st-enabled" type="checkbox" checked/);assert.match(panel.innerHTML,/>Save<\/button>/);assert.match(h.nodes["st-operation-status"].textContent,/settings remain editable/);h.ctx.respond=()=>({...config,revision:"2"});await h.ctx.saveSubtitleTranscription(element());assert.equal(h.calls.at(-1).init.body.enabled,true);
});
test("model failure reasons distinguish network storage integrity and engine repair actions",()=>{
  const h=harness();
  for(const [code,pattern]of [["insufficient_disk",/could not safely reserve model storage.*Free space or check its storage support.*retry Install/],["model_download_failed",/network access.*retry Install/],["model_verification_failed",/integrity or transcription test.*Retry Install/],["engine_unavailable",/supported server package.*run Test/],["waiting_for_worker",/worker to take this job/],["worker_observation_unavailable",/Current worker status is unavailable.*Refresh worker status/],["worker_not_current",/no longer a current cluster member.*choose a current worker/],["worker_source_unavailable",/Waiting for a verified worker with access to this media source/],["no_speech_detected",/selected audio is silent.*Choose another audio track/]])assert.match(h.ctx.subtitleReason(code),pattern);
  assert.doesNotMatch(h.ctx.subtitleReason("insufficient_disk"),/disk is full/);
  assert.doesNotMatch(h.ctx.subtitleReason("worker_source_unavailable"),/model|download|playback capacity|source changed/i);
  assert.doesNotMatch(h.ctx.subtitleReason("no_speech_detected"),/model|corrupt|verification/i);
  const html=h.ctx.subtitleModelOperationsHtml({catalog:{workers:[{node_id:"ready",label:"Ready worker"},{node_id:"space",label:"Storage worker"}]},operationRows:[{node_id:"ready",outcome:"accepted",job_id:"install-one",job:{state:"running",phase:"verifying"}},{node_id:"space",outcome:"insufficient_disk"}]});
  assert.match(html,/Ready worker: Verifying model/);assert.match(html,/Storage worker: storage unavailable.*could not safely reserve model storage/);assert.match(html,/activity\/jobs\/install-one/);
  const unknown=h.ctx.subtitleReason("PRIVATE /srv/process/stdout");assert.doesNotMatch(unknown,/PRIVATE|\/srv\/process/);
  const refused=h.ctx.subtitleModelOperationsHtml({catalog:{workers:[]},operationRows:[{node_id:"worker",outcome:"PRIVATE /srv/process/stdout"}]});assert.match(refused,/not accepted/);assert.doesNotMatch(refused,/PRIVATE|\/srv\/process/);
});
test("model operation statuses use friendly phases and broad preparation failures",()=>{
  const h=harness(),setup={catalog:{workers:[{node_id:"worker",label:"Worker"}]},operationRows:[]};
  for(const [phase,label]of [["queued","Queued"],["waiting_for_capacity","Waiting for server capacity"],["downloading","Downloading model"],["verifying","Verifying model"],["decoding","Decoding test audio"],["transcribing","Running speech test"],["validating","Validating"],["publishing","Publishing"]]){
    setup.operationRows=[{node_id:"worker",outcome:"accepted",job:{state:"running",phase}}];assert.match(h.ctx.subtitleModelOperationsHtml(setup),new RegExp(`Worker: ${label}`));
  }
  for(const [state,label]of [["succeeded","Ready"],["failed","Failed"],["cancelled","Cancelled"]]){
    setup.operationRows=[{node_id:"worker",outcome:"accepted",job:{state}}];assert.match(h.ctx.subtitleModelOperationsHtml(setup),new RegExp(`Worker: ${label}`));
  }
  setup.operationRows=[{node_id:"worker",outcome:"accepted",job:{state:"running"}}];assert.match(h.ctx.subtitleModelOperationsHtml(setup),/Worker: Running/);
  setup.operationRows=[{node_id:"worker",outcome:"accepted",job:{state:"cancelling"}}];assert.match(h.ctx.subtitleModelOperationsHtml(setup),/Worker: Cancelling/);
  setup.operationRows=[{node_id:"worker",outcome:"accepted",job:{state:"private_internal_state"}}];const unavailable=h.ctx.subtitleModelOperationsHtml(setup);assert.match(unavailable,/Worker: Status unavailable/);assert.doesNotMatch(unavailable,/private_internal_state/);
  setup.operationRows=[{node_id:"worker",outcome:"accepted",job:{state:"failed",reason:"output_invalid"}}];
  const html=h.ctx.subtitleModelOperationsHtml(setup);assert.match(html,/The model test could not prepare or validate captions.*Check server storage and logs, then retry Test/);assert.doesNotMatch(html,/generated captions failed validation/);
  assert.equal(h.ctx.subtitleReason("no_speech_detected"),"The selected audio is silent. Choose another audio track.");
});
test("watch generation mount survives mirrored ledger refresh and retires on file or account changes",()=>{
  const h=harness();Object.assign(h.ctx.file,{available:true,video_codec:"h264"});h.ctx.watchCurrentFile=()=>h.ctx.file;
  vm.runInContext(declarations("detail/watch-browser.js",name=>["watchCanGenerateSubtitles","watchGenerationAction","watchRenderSubtitleGeneration"].includes(name)),h.ctx);
  const mount=h.node("watch-subtitle-generation");h.ctx.watchRenderSubtitleGeneration();assert.match(mount.innerHTML,/subtitle-generate-9007199254740993/);
  mount.innerHTML="pending form with viewer edits and focus";h.ctx.watchRenderSubtitleGeneration();assert.equal(mount.innerHTML,"pending form with viewer edits and focus");
  h.ctx.file={...h.ctx.file,id:"9007199254740994",fileContext:h.ctx.localPlaybackFileContext("9007199254740994")};h.ctx.watchRenderSubtitleGeneration();assert.match(mount.innerHTML,/subtitle-generate-9007199254740994/);assert.doesNotMatch(mount.innerHTML,/9007199254740993/);
  h.ctx.ME={is_admin:false};h.ctx.watchRenderSubtitleGeneration();assert.equal(mount.hidden,true);assert.equal(mount.innerHTML,"");assert.equal(h.ctx.watchGenerationAction(h.ctx.file),"");
});

test("terminal model work waits for fresh verification with bounded fenced retries",async()=>{
  const h=harness(),panel=h.settings();let clock=1000,fresh=false;
  h.ctx.Date=class extends Date{static now(){return clock;}};
  const setup=panel._subtitleSetup;setup.operation={};setup.operationRows=[{node_id:"worker",job_id:"verify-one",outcome:"accepted",model_id:"base"}];
  h.nodes["st-language"].value="sv";
  h.ctx.respond=path=>path.includes("/jobs/")?{job_id:"verify-one",state:"succeeded",phase:"succeeded",updated_at_ms:900}:{models:[model],workers:[{node_id:"worker",label:"Worker",eligible:true,engine:{status:"available"},observed_at_ms:800,expires_at_ms:60000,models:[{model_id:"base",status:fresh?"verified":"unverified_cache",verified_at_ms:fresh?850:null}]}]};
  await h.ctx.pollSubtitleModelOperations(setup,["verify-one"]);
  assert.match(h.nodes["st-model-evidence"].innerHTML,/Download cached/);assert.match(h.nodes["st-operation-status"].innerHTML,/Verification status is updating/);assert.equal(h.timers.length,1);
  assert.equal(h.nodes["st-language"].value,"sv");assert.equal(h.nodes["st-enabled"].checked,true);
  fresh=true;h.timers.shift()();await settle();
  assert.match(h.nodes["st-model-evidence"].innerHTML,/ · verified · engine/);assert.equal(h.timers.length,0);assert.equal(setup.operationRows[0].awaitingObservation,false);
  const row=setup.operationRows[0];setup.catalog.workers[0].observed_at_ms=clock+30001;assert.equal(h.ctx.subtitleModelObservationPending(setup,row),true);assert.match(h.ctx.subtitleModelEvidence(setup.catalog,"base"),/readiness is unknown/);
  row.job={state:"failed"};assert.equal(h.ctx.subtitleModelObservationPending(setup,row),false);row.job={state:"cancelled"};assert.equal(h.ctx.subtitleModelObservationPending(setup,row),false);
  fresh=false;await h.ctx.pollSubtitleModelOperations(setup,["verify-one"]);assert.equal(h.timers.length,1);
  clock=32000;h.timers.shift()();await settle();assert.equal(h.timers.length,0);assert.match(h.nodes["st-operation-status"].innerHTML,/Refresh verification status/);
  clock=33000;h.ctx.retrySubtitleModelOperations();await settle();assert.equal(h.timers.length,1);
  const calls=h.calls.length;h.ctx.PAGE_RENDER_GENERATION++;h.timers.shift()();await settle();assert.equal(h.calls.length,calls);assert.equal(h.timers.length,0);
  const newer=harness(),newPanel=newer.settings(),newSetup=newPanel._subtitleSetup;newSetup.operation={};newSetup.operationRows=[{node_id:"worker",job_id:"old",outcome:"accepted",model_id:"base"}];
  let rejectOld;newer.ctx.respond=()=>new Promise((_resolve,reject)=>rejectOld=reject);
  const oldPoll=newer.ctx.pollSubtitleModelOperations(newSetup,["old"]);newSetup.operation={};newer.nodes["st-operation-status"].textContent="New operation";
  rejectOld(Error("old response failed"));await oldPoll;assert.equal(newer.nodes["st-operation-status"].textContent,"New operation");
  for(const change of [ctx=>{ctx.location.hash="#/settings/playback";},ctx=>{ctx.TOKEN="other-account";}]){
    const retired=harness(),retiredSetup=retired.settings()._subtitleSetup;retiredSetup.operation={};retiredSetup.operationRows=[{node_id:"worker",job_id:"verify-one",outcome:"accepted",model_id:"base"}];retired.ctx.respond=h.ctx.respond;
    await retired.ctx.pollSubtitleModelOperations(retiredSetup,["verify-one"]);assert.equal(retired.timers.length,1);const before=retired.calls.length;
    change(retired.ctx);retired.timers.shift()();await settle();assert.equal(retired.calls.length,before);assert.equal(retired.timers.length,0);
  }
});
