"use strict";
// Full-caption text translation has jobs and explicit adoption, never playback ownership.
let TEXT_TRANSLATION_SETUP=null;
let SUBTITLE_TEXT_TRANSLATION=null;
const TEXT_TRANSLATION_MODEL="opus-mt-mul-en";
function textTranslationExplain(code){
  return ({worker_changed:"The worker restarted. Refresh workers before an explicit model test.",subtitle_changed:"The source subtitle changed. Refresh choices before a new request.",subtitle_unavailable:"The source subtitle is unavailable. Refresh choices.",subtitle_invalid:"The source subtitle failed text validation.",subtitle_too_large:"The source subtitle exceeds the supported size.",model_not_installed:"Download the text translation model on this worker.",model_self_test_failed:"The model test failed. Refresh worker status before testing again.",text_self_test_failed:"The English text translation self-test failed. Refresh and test the model again.",text_inference_timeout:"Text translation exceeded its time limit.",inference_timeout:"Text translation exceeded its time limit.",text_input_too_long:"A source cue exceeds this model's supported input length.",slice_publication_refused:"Work ownership changed. Refresh job status before retrying.",model_changed:"The model changed. Test the current model before retrying.",engine_changed:"The worker engine changed. Test the current model before retrying.",engine_unavailable:"The text translation engine is unavailable on this worker.",text_translation_failed:"Text translation failed. Refresh job status and retry when offered.",disabled:"Enable subtitle text translation in Settings, then retry.",translation_unverified:"Download and test the text translation model in Settings.",model_unverified:"Test the text translation model on this worker.",model_missing:"Download the text translation model on this worker.",source_changed:"The media or source subtitle changed. Refresh choices before a new request.",source_language_unsupported:"Choose a language supported by this text translation model.",configuration_conflict:"Settings changed elsewhere. Reload saved settings before saving.",request_conflict:"This action conflicts with an earlier request. Refresh status.",waiting_for_playback_capacity:"Waiting for capacity while playback continues.",waiting_for_worker:"Waiting for a qualified text translation worker.",worker_interrupted:"The worker stopped. Retry when the job offers it.",inference_failed:"Text translation failed. Retry when the job offers it.",output_invalid:"The translated subtitle failed validation.",permission_denied:"An administrator is required for this action.",invalid_result:"This result no longer identifies the same English subtitle. Refresh choices.",network_error:"Text translation status is unavailable. Refresh or retry the same action."})[code]||"Text translation could not complete this action. Refresh its status.";
}
function textTranslationStatus(state,message,slot="status"){
  if(!state.current())return;const node=state.panel.querySelector(`[data-text-${slot}]`);if(node&&node.textContent!==message)node.textContent=message;
}
function textTranslationRetire(state){if(!state)return;state.retired=true;clearTimeout(state.timer);for(const controller of state.controllers)controller.abort();}
function textTranslationSourceCurrent(state){
  try{return playbackFileKey(playbackFileContextForPlayer(state.player))===playbackFileKey(state.fileContext);}
  catch(error){return false;}
}
function textTranslationState(kind,panel){
  const state={kind,panel,capture:translationCapture(),controllers:new Set(),retired:false,timer:null,attempt:0,pollEpoch:0,busy:false,saving:false,using:false,operation:null,job:null};
  state.current=()=>translationPageCurrent(state)&&document.getElementById(kind==="setup"?"text-subtitle-translation-settings":"watch-subtitle-text-translation")===panel&&(kind==="setup"?TEXT_TRANSLATION_SETUP===state&&!!ME?.is_admin:SUBTITLE_TEXT_TRANSLATION===state&&PLAYER===state.player&&String(PLAYER?.fileId)===state.fileId&&textTranslationSourceCurrent(state));
  return state;
}
function textTranslationConfig(value){if(value?.version!==1||!translationCounter(value.revision)||typeof value.enabled!=="boolean"||value.model_id!==TEXT_TRANSLATION_MODEL||value.target_language!=="en")throw Error("Invalid text translation settings");return value;}
function textTranslationWorkers(rows){
  if(!Array.isArray(rows)||rows.length>32||rows.some(row=>typeof row.node_id!=="string"||!row.node_id||row.node_id.length>128||typeof row.label!=="string"||row.label.length>256||!translationId(row.boot_id)||row.model_id!==TEXT_TRANSLATION_MODEL||!["missing","unverified_cache","verified","unknown"].includes(row.status)))throw Error("Invalid text translation workers");return rows;
}
function textTranslationCatalog(value){
  if(value?.version!==1||value.catalog?.version!==1||value.catalog.model_id!==TEXT_TRANSLATION_MODEL||!translationDigest(value.catalog.bundle_digest)||!translationDigest(value.catalog.model_sha256)||typeof value.catalog.adapter_contract!=="string")throw Error("Invalid text translation catalog");textTranslationWorkers(value.workers);return value;
}
function textTranslationOptions(value,state){
  if(value?.version!==1||value.file_id!==state.fileId||!translationCounter(value.source_size)||value.source_size==="0"||typeof value.source_mtime!=="string"||!/^(-?[1-9][0-9]{0,18}|0)$/.test(value.source_mtime)||value.model_id!==TEXT_TRANSLATION_MODEL||!Array.isArray(value.subtitles)||value.subtitles.length>4096||!Array.isArray(value.input_languages)||value.input_languages.length>512||value.input_languages.some(code=>typeof code!=="string"||! /^[a-z]{2,3}$/.test(code)))throw Error("Invalid text translation choices");
  if(value.subtitles.some(row=>!Number.isSafeInteger(row.index)||row.index<0||row.index>1000000||typeof row.text_eligible!=="boolean"||row.language!=null&&typeof row.language!=="string"||row.title!=null&&typeof row.title!=="string"))throw Error("Invalid source subtitle");
  textTranslationConfig(value.configuration);textTranslationWorkers(value.workers);return value;
}
function textTranslationModelKey(state){const c=state.catalog?.catalog;return JSON.stringify([c?.model_id,c?.adapter_contract,c?.bundle_digest,c?.model_sha256]);}
function textTranslationReceipt(data){if(data?.version!==1||!translationId(data.job_id)||!["queued","running","succeeded","failed","cancelled"].includes(data.state))throw Error("Invalid text translation job receipt");return data;}
function textTranslationObservedReceipt(data){return {...textTranslationReceipt(data),retryable:false,progress:{completed:0,total:0},result:null};}
function textTranslationJob(data,id){
  textTranslationReceipt(data);if(data.job_id!==id||typeof data.retryable!=="boolean"||!Number.isSafeInteger(data.progress?.completed)||!Number.isSafeInteger(data.progress?.total)||data.progress.completed<0||data.progress.total<data.progress.completed)throw Error("Invalid text translation progress");return data;
}
function textTranslationJobLabel(job){
  const phase=({queued:"Queued",checking_source:"Checking source",downloading:"Downloading model",verifying:"Testing model",waiting_for_capacity:"Waiting for capacity",waiting_for_playback_capacity:"Waiting for playback capacity",translating:"Translating subtitle text",validating:"Validating output",publishing:"Publishing subtitle",running:"Working",succeeded:"Completed",failed:"Failed",cancelled:"Stopped"})[job.phase]||({queued:"Queued",running:"Working",succeeded:"Completed",failed:"Failed",cancelled:"Stopped"})[job.state];
  return `${phase}${job.progress.total?` · ${job.progress.completed} / ${job.progress.total} cues`:""}${job.reason?` · ${textTranslationExplain(job.reason)}`:""}`;
}
function textTranslationObserveSoon(state){if(state.current()&&state.operation&&!state.polling&&["queued","running"].includes(state.job?.state)){clearTimeout(state.timer);state.timer=setTimeout(()=>void textTranslationPoll(state),5000);}}
function textTranslationAmbiguous(error){return error.status==null||error.status===408||error.status>=500;}
function textTranslationPending(state,slot,key,body){if(state[slot]?.key!==key)state[slot]={key,body:body()};return state[slot].body;}
function textTranslationSelectionKey(state){return JSON.stringify([state.panel.querySelector("[data-text-source]")?.value,state.panel.querySelector("[data-text-language]")?.value]);}
function textTranslationResult(state,result,latest){
  const op=state.operation,matches=latest?.subtitles?.filter(track=>track.downloaded_subtitle_id===result?.downloaded_subtitle_id)||[],row=matches.length===1?matches[0]:null;
  if(state.job?.state!=="succeeded"||state.job.job_id!==op?.job_id||op.selectionKey!==textTranslationSelectionKey(state)||latest.file_id!==state.fileId||latest.source_size!==op.source_size||latest.source_mtime!==op.source_mtime||result?.file_id!==state.fileId||result.language!=="en"||!translationDigest(result.downloaded_subtitle_id)||!row?.text_eligible||row.downloaded_subtitle_id!==result.downloaded_subtitle_id)throw Object.assign(Error("Invalid text translation result"),{code:"invalid_result"});return {...result,subtitle_index:row.index};
}
function textTranslationRenderJob(state){
  if(!state.current())return;const job=state.job,live=state.busy||job&&["queued","running"].includes(job.state);
  for(const selector of state.kind==="setup"?["install","test"]:["start","source","language","confirm"]){const node=state.panel.querySelector(`[data-text-${selector}]`);if(node)node.disabled=!!live||state.kind==="watch"&&selector==="start"&&state.options?.permissions?.create!==true;}
  const stop=state.panel.querySelector("[data-text-stop]"),retry=state.panel.querySelector("[data-text-retry]"),use=state.panel.querySelector("[data-text-use]");
  if(stop){stop.hidden=!live;stop.disabled=state.stopping===true;}if(retry){retry.hidden=!job?.retryable;retry.disabled=state.busy;}if(use){use.hidden=job?.state!=="succeeded"||!job?.result;use.disabled=state.using||state.operation?.selectionKey!==textTranslationSelectionKey(state);}
  if(job)textTranslationStatus(state,`${state.stopRequested&&["queued","running"].includes(job.state)?"Stop requested · ":""}${textTranslationJobLabel(job)}`);
}
async function textTranslationPoll(state){
  if(!state.current()||!state.operation)return;if(state.polling){clearTimeout(state.timer);state.timer=setTimeout(()=>void textTranslationPoll(state),5000);return;}const operation=state.operation,epoch=state.pollEpoch;state.polling=true;
  try{
    const {data}=await translationRequest(state,`/subtitle-text-translation/jobs/${encodeURIComponent(operation.job_id)}`);
    if(!state.current()||state.operation!==operation||state.pollEpoch!==epoch)return;
    if(state.kind==="setup"&&(operation.identity!==textTranslationModelKey(state)||!state.catalog.workers.some(row=>row.node_id===operation.node_id&&row.boot_id===operation.boot_id))){state.operation=null;state.job=null;textTranslationRenderJob(state);textTranslationStatus(state,"The model or worker boot changed. Refresh workers before an explicit test.");return;}
    state.job=textTranslationJob(data,operation.job_id);state.attempt=0;textTranslationRenderJob(state);
    if(["succeeded","failed","cancelled"].includes(data.state)){
      if(state.kind==="setup")await textTranslationRefreshSetup(state);
      else if(data.state==="succeeded")textTranslationStatus(state,"Translation finished. Use is an explicit track choice; playback has not changed.");
      return;
    }
  }catch(error){if(!state.current()||state.operation!==operation||state.pollEpoch!==epoch)return;state.attempt=Math.min(state.attempt+1,3);textTranslationStatus(state,textTranslationExplain(error.code||"network_error"));}
  finally{state.polling=false;}
  if(state.current()&&state.operation===operation&&state.pollEpoch===epoch){clearTimeout(state.timer);state.timer=setTimeout(()=>void textTranslationPoll(state),Math.min(30000,5000*2**state.attempt));}
}
async function textTranslationRefreshSetup(state=TEXT_TRANSLATION_SETUP){
  if(!state?.current()||state.refreshing)return;state.refreshing=true;
  try{const {data}=await translationRequest(state,"/subtitle-text-translation/models");if(!state.current())return;state.catalog=textTranslationCatalog(data);textTranslationRenderWorkers(state);}
  catch(error){textTranslationStatus(state,textTranslationExplain(error.code||"network_error"));}finally{state.refreshing=false;}
}
function textTranslationRenderWorkers(state){
  if(!state.current())return;const select=state.panel.querySelector("[data-text-worker]"),old=select.value;
  select.innerHTML=state.catalog.workers.map(row=>`<option value="${esc(row.node_id)}">${esc(row.label)} · ${esc(({missing:"Model missing",unverified_cache:"Needs testing",verified:"Verified for this worker boot",unknown:"Readiness unknown"})[row.status])}</option>`).join("");if(state.catalog.workers.some(row=>row.node_id===old))select.value=old;
  textTranslationRenderJob(state);
}
async function openTextTranslationSettings(){
  const panel=document.getElementById("text-subtitle-translation-settings");if(!panel||!ME?.is_admin)return;if(TEXT_TRANSLATION_SETUP?.current())return;
  textTranslationRetire(TEXT_TRANSLATION_SETUP);const state=textTranslationState("setup",panel);TEXT_TRANSLATION_SETUP=state;panel.textContent="Loading subtitle text translation settings…";
  try{
    const {data}=await translationRequest(state,"/subtitle-text-translation/config");if(!state.current())return;state.saved=textTranslationConfig(data);
    panel.innerHTML=`<form data-text-settings><label class="chk"><input type="checkbox" data-text-enabled${data.enabled?" checked":""}> Enable subtitle text translation</label><p class="hint">Use the local Marian model for existing text subtitles. Speech generation uses its separate model. Readiness does not change this saved preference.</p><label>Text model worker <select data-text-worker aria-label="Text translation model worker"></select></label><button type="button" class="sm" data-text-install>Download and test</button> <button type="button" class="ghost sm" data-text-test>Test text model</button> <button type="button" class="ghost sm" data-text-refresh>Refresh workers</button><p data-text-status role="status" aria-live="polite"></p><button type="submit" class="sm" data-text-save>Save</button><p data-text-save-status role="status" aria-live="polite"></p></form>`;
    panel.querySelector("form").addEventListener("submit",event=>{event.preventDefault();void saveTextTranslationSettings();});
    panel.querySelector("[data-text-install]").addEventListener("click",()=>void textTranslationSetupModel("install"));panel.querySelector("[data-text-test]").addEventListener("click",()=>void textTranslationSetupModel("verify"));panel.querySelector("[data-text-refresh]").addEventListener("click",()=>void textTranslationRefreshSetup());
    await textTranslationRefreshSetup(state);
  }catch(error){if(state.current())panel.textContent=textTranslationExplain(error.code||"network_error");}
}
async function saveTextTranslationSettings(){
  const state=TEXT_TRANSLATION_SETUP;if(!state?.current()||state.saving)return;state.saving=true;const button=state.panel.querySelector("[data-text-save]");button.disabled=true;
  const enabled=!!state.panel.querySelector("[data-text-enabled]").checked,key=JSON.stringify([state.saved.revision,enabled,state.saved.model_id]);
  const body=textTranslationPending(state,"saveIntent",key,()=>({version:1,expected_revision:state.saved.revision,enabled,model_id:state.saved.model_id}));
  textTranslationStatus(state,"Saving…","save-status");
  try{const {data}=await translationRequest(state,"/subtitle-text-translation/config",{method:"PUT",body});if(!state.current())return;state.saved=textTranslationConfig(data);state.saveIntent=null;textTranslationStatus(state,"Saved. Worker readiness remains advisory.","save-status");}
  catch(error){if(state.current()){if(!textTranslationAmbiguous(error))state.saveIntent=null;textTranslationStatus(state,textTranslationExplain(error.status===409?"configuration_conflict":error.code||"network_error"),"save-status");if(error.status===409){const reload=translationActionButton("Reload saved settings",()=>{textTranslationRetire(state);TEXT_TRANSLATION_SETUP=null;void openTextTranslationSettings();});state.panel.querySelector("[data-text-save-status]").appendChild(reload);}}}
  finally{state.saving=false;if(state.current())button.disabled=false;}
}
async function textTranslationSetupModel(action){
  const state=TEXT_TRANSLATION_SETUP;if(!state?.current()||state.busy||state.job&&["queued","running"].includes(state.job.state)||!["install","verify"].includes(action))return;
  const nodeId=state.panel.querySelector("[data-text-worker]").value,worker=state.catalog?.workers.find(row=>row.node_id===nodeId);if(!worker){textTranslationStatus(state,"Refresh workers and choose a text model worker.");return;}
  const identity=textTranslationModelKey(state),key=JSON.stringify([action,worker.node_id,worker.boot_id,worker.model_id,identity]);
  const body=textTranslationPending(state,"modelIntent",key,()=>({version:1,request_id:translationUuid(),node_id:worker.node_id,boot_id:worker.boot_id,model_id:worker.model_id}));state.busy=true;textTranslationRenderJob(state);
  try{
    const {data}=await translationRequest(state,`/subtitle-text-translation/models/${action}`,{method:"POST",body});if(!state.current()||identity!==textTranslationModelKey(state))return;
    const receipt=textTranslationReceipt(data);state.modelIntent=null;state.operation={job_id:receipt.job_id,identity,node_id:worker.node_id,boot_id:worker.boot_id};state.job=textTranslationObservedReceipt(receipt);state.pollEpoch++;void textTranslationPoll(state);
  }catch(error){if(state.current()){if(!textTranslationAmbiguous(error))state.modelIntent=null;textTranslationStatus(state,textTranslationExplain(error.code||"network_error"));}}
  finally{state.busy=false;textTranslationRenderJob(state);}
}
async function openTextSubtitleTranslation(button){
  const player=PLAYER,panel=document.getElementById("watch-subtitle-text-translation");if(!player||!panel)return;
  let context;try{context=typeof playbackFileContextForPlayer==="function"?playbackFileContextForPlayer(player):null;}catch(error){context=null;}
  if(context?.source_ref?.kind!=="local"){if(typeof toast==="function")toast("Text translation is available for local library subtitles.");return;}
  if(SUBTITLE_TEXT_TRANSLATION?.current()){panel.hidden=false;panel.scrollIntoView?.({block:"nearest"});return;}
  textTranslationRetire(SUBTITLE_TEXT_TRANSLATION);const state=textTranslationState("watch",panel);state.player=player;state.fileContext=context;state.fileId=context.source_ref.file_id;SUBTITLE_TEXT_TRANSLATION=state;panel.hidden=false;panel.textContent="Loading existing text subtitle choices…";if(button)button.disabled=true;
  try{
    const {data}=await translationRequest(state,playbackFileApiPath(state.fileContext,"subtitle-text-translation/options"));if(!state.current())return;state.options=textTranslationOptions(data,state);
    panel.innerHTML=`<h3>Translate existing subtitles to English</h3><p class="hint">The local text model translates an existing text subtitle. Bitmap subtitles need captions generated from audio.</p><label>Source subtitle <select data-text-source aria-label="Source text subtitle">${data.subtitles.map(row=>`<option value="${row.index}"${row.text_eligible?"":" disabled"}>${esc(row.title||row.language||`Subtitle ${row.index}`)}${row.text_eligible?"":" · bitmap"}</option>`).join("")}</select></label><label>Language written in the source subtitle <select data-text-language aria-label="Source subtitle language"><option value="">Choose the source language</option>${data.input_languages.map(code=>`<option value="${esc(code)}">${esc(typeof subtitleLanguageName==="function"?subtitleLanguageName(code):code)}</option>`).join("")}</select></label><label class="chk"><input type="checkbox" data-text-confirm> I confirm the source subtitle language</label><button type="button" class="ghost sm" data-text-refresh-choices>Refresh choices</button> <button type="button" class="sm" data-text-start>Translate text to English</button> <button type="button" class="ghost sm" data-text-stop hidden>Stop</button> <button type="button" class="ghost sm" data-text-retry hidden>Retry</button> <button type="button" class="sm" data-text-use hidden>Use English translated subtitle</button><p data-text-status role="status" aria-live="polite"></p>`;
    for(const selector of ["source","language"]){state.panel.querySelector(`[data-text-${selector}]`).addEventListener("change",()=>{state.panel.querySelector("[data-text-confirm]").checked=false;textTranslationRenderJob(state);});}
    panel.querySelector("[data-text-refresh-choices]").addEventListener("click",()=>void textTranslationRefreshChoices());
    /** @type {HTMLButtonElement} */ (panel.querySelector("[data-text-start]")).disabled=!data.permissions?.create;panel.querySelector("[data-text-start]").addEventListener("click",()=>void textTranslationStart());panel.querySelector("[data-text-stop]").addEventListener("click",()=>void textTranslationStop());panel.querySelector("[data-text-retry]").addEventListener("click",()=>void textTranslationRetry());panel.querySelector("[data-text-use]").addEventListener("click",()=>void textTranslationUse());
    textTranslationStatus(state,data.permissions?.create?"Choose and confirm the subtitle language before translating.":"An administrator can request text translation. Existing captions remain available in the subtitle menu.");
  }catch(error){if(state.current())panel.textContent=textTranslationExplain(error.code||"network_error");}finally{if(button?.isConnected)button.disabled=false;}
}
async function textTranslationRefreshChoices(){
  const state=SUBTITLE_TEXT_TRANSLATION;if(!state?.current()||state.refreshing||state.busy)return;state.refreshing=true;
  const source=state.panel.querySelector("[data-text-source]"),language=state.panel.querySelector("[data-text-language]"),oldSource=source.value,oldLanguage=language.value;
  try{const {data}=await translationRequest(state,playbackFileApiPath(state.fileContext,"subtitle-text-translation/options"));if(!state.current())return;state.options=textTranslationOptions(data,state);
    source.innerHTML=data.subtitles.map(row=>`<option value="${row.index}"${row.text_eligible?"":" disabled"}>${esc(row.title||row.language||`Subtitle ${row.index}`)}</option>`).join("");if(data.subtitles.some(row=>String(row.index)===oldSource&&row.text_eligible))source.value=oldSource;
    language.innerHTML=`<option value="">Choose the source language</option>`+data.input_languages.map(code=>`<option value="${esc(code)}">${esc(typeof subtitleLanguageName==="function"?subtitleLanguageName(code):code)}</option>`).join("");if(data.input_languages.includes(oldLanguage))language.value=oldLanguage;
    state.panel.querySelector("[data-text-confirm]").checked=false;textTranslationRenderJob(state);textTranslationStatus(state,"Choices refreshed. Confirm the source subtitle language.");if(state.operation)void textTranslationPoll(state);
  }catch(error){textTranslationStatus(state,textTranslationExplain(error.code||"network_error"));}finally{state.refreshing=false;}
}
async function textTranslationStart(){
  const state=SUBTITLE_TEXT_TRANSLATION;if(!state?.current()||state.busy||state.job&&["queued","running"].includes(state.job.state)||state.options.permissions?.create!==true||!ME?.is_admin)return;
  const source=state.options.subtitles.find(row=>String(row.index)===state.panel.querySelector("[data-text-source]").value),language=state.panel.querySelector("[data-text-language]").value,selectionKey=textTranslationSelectionKey(state);
  if(!source?.text_eligible||!state.options.input_languages.includes(language)||!state.panel.querySelector("[data-text-confirm]").checked){textTranslationStatus(state,"Choose a text subtitle and confirm one of this model's supported source languages.");return;}
  const key=JSON.stringify([state.fileId,state.options.source_size,state.options.source_mtime,source.index,language,state.options.model_id]);
  if(state.createIntent?.key!==key)state.stopRequested=false;
  const body=textTranslationPending(state,"createIntent",key,()=>({version:1,request_id:translationUuid(),source_size:state.options.source_size,source_mtime:state.options.source_mtime,source_language:language,target_language:"en",model_id:state.options.model_id}));state.busy=true;textTranslationRenderJob(state);
  try{
    const {data}=await translationRequest(state,playbackFileApiPath(state.fileContext,`subtitles/${source.index}/translation`),{method:"POST",body});if(!state.current()||selectionKey!==textTranslationSelectionKey(state))return;
    const receipt=textTranslationReceipt(data);state.createIntent=null;state.operation={job_id:receipt.job_id,source_size:body.source_size,source_mtime:body.source_mtime,source_index:source.index,source_language:language,selectionKey};state.job=textTranslationObservedReceipt(receipt);state.pollEpoch++;
    if(state.stopRequested)await textTranslationStopJob(state);void textTranslationPoll(state);
  }catch(error){if(state.current()){if(!textTranslationAmbiguous(error))state.createIntent=null;textTranslationStatus(state,textTranslationExplain(error.code||"network_error"));}}
  finally{state.busy=false;textTranslationRenderJob(state);textTranslationObserveSoon(state);}
}
async function textTranslationStopJob(state){const operation=state.operation;if(!operation)return;state.pollEpoch++;await translationRequest(state,`/cluster/jobs/${encodeURIComponent(operation.job_id)}/cancel`,{method:"POST"});if(state.current()&&state.operation===operation){clearTimeout(state.timer);void textTranslationPoll(state);}}
async function textTranslationStop(){
  const state=SUBTITLE_TEXT_TRANSLATION;if(!state?.current()||!ME?.is_admin)return;state.stopRequested=true;if(state.busy){textTranslationStatus(state,"Stop requested; waiting for the job receipt.");return;}state.busy=true;state.stopping=true;textTranslationRenderJob(state);
  try{await textTranslationStopJob(state);}catch(error){if(state.current())textTranslationStatus(state,textTranslationExplain(error.code||"network_error"));}finally{state.busy=false;state.stopping=false;textTranslationRenderJob(state);textTranslationObserveSoon(state);}
}
async function textTranslationRetry(){
  const state=SUBTITLE_TEXT_TRANSLATION;if(!state?.current()||state.busy||!state.operation||!state.job?.retryable||!ME?.is_admin)return;const operation=state.operation;
  if(state.retryIntent?.key!==operation.job_id)state.stopRequested=false;
  const body=textTranslationPending(state,"retryIntent",operation.job_id,()=>({version:1,request_id:translationUuid()}));state.busy=true;textTranslationRenderJob(state);
  try{const {data}=await translationRequest(state,`/subtitle-text-translation/jobs/${encodeURIComponent(operation.job_id)}/retry`,{method:"POST",body});if(!state.current()||state.operation!==operation)return;const receipt=textTranslationReceipt(data);state.retryIntent=null;state.operation={...operation,job_id:receipt.job_id};state.job=textTranslationObservedReceipt(receipt);state.pollEpoch++;clearTimeout(state.timer);if(state.stopRequested)await textTranslationStopJob(state);void textTranslationPoll(state);}
  catch(error){if(state.current()){if(!textTranslationAmbiguous(error))state.retryIntent=null;textTranslationStatus(state,textTranslationExplain(error.code||"network_error"));}}finally{state.busy=false;textTranslationRenderJob(state);}
}
async function textTranslationUse(){
  const state=SUBTITLE_TEXT_TRANSLATION;if(!state?.current()||state.using||!state.job?.result)return;const operation=state.operation,result=state.job.result;state.using=true;textTranslationRenderJob(state);
  try{const {data}=await translationRequest(state,playbackFileApiPath(state.fileContext,"subtitle-text-translation/options"));if(!state.current()||state.operation!==operation)return;const latest=textTranslationOptions(data,state);const verifiedResult=textTranslationResult(state,result,latest);state.options=latest;await textTranslationUseResult(state,verifiedResult);}
  catch(error){if(state.current())textTranslationStatus(state,textTranslationExplain(error.code||"invalid_result"));}finally{state.using=false;textTranslationRenderJob(state);}
}
function textTranslationWatchChanged(){const state=SUBTITLE_TEXT_TRANSLATION;if(state&&!state.current()){textTranslationRetire(state);if(document.getElementById("watch-subtitle-text-translation")===state.panel){state.panel.hidden=true;state.panel.replaceChildren();}}}
function textTranslationReset(){textTranslationRetire(TEXT_TRANSLATION_SETUP);textTranslationRetire(SUBTITLE_TEXT_TRANSLATION);TEXT_TRANSLATION_SETUP=null;SUBTITLE_TEXT_TRANSLATION=null;}

async function textTranslationUseResult(state,result){
  if(!state.current())return;
  const player=state.player,operation=state.operation,serial=player.captionIntentSerial||0,selection=textTranslationSelectionKey(state);
  const current=()=>state.current()&&state.operation===operation&&(player.captionIntentSerial||0)===serial&&textTranslationSelectionKey(state)===selection;
  // Decision is a read-only capability query. Refresh track descriptors without
  // opening media or borrowing a new playback owner.
  const {data:decision}=await translationRequest(state,playbackFileApiPath(state.fileContext,"decision",{force:"auto"}),{method:"POST",body:{caps:currentCapsDocument(player)}});
  if(!current())return;
  const matches=(decision.subtitles||[]).filter(track=>track.downloaded_subtitle_id===result.downloaded_subtitle_id);
  if(matches.length!==1||matches[0].source!=="downloaded"||matches[0].text!==true||!Number.isSafeInteger(matches[0].index))throw Object.assign(Error("Translated subtitle identity changed"),{code:"invalid_result"});
  const {data}=await translationRequest(state,playbackFileApiPath(state.fileContext,"subtitle-text-translation/options"));
  if(!current())return;
  const latest=textTranslationOptions(data,state),verified=textTranslationResult(state,result,latest);
  if(verified.subtitle_index!==matches[0].index)throw Object.assign(Error("Translated subtitle moved during refresh"),{code:"invalid_result"});
  player.subs=decision.subtitles;
  const file=typeof watchCurrentFile==="function"?watchCurrentFile():null;
  if(file&&String(file.id)===state.fileId)file.subtitle_streams=decision.subtitles;
  state.options=latest;
  await setSub(verified.subtitle_index);
  if(state.current()&&player.curSub===verified.subtitle_index)textTranslationStatus(state,"English translated subtitle selected.");
}
