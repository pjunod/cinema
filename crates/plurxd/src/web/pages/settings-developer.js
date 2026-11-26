"use strict";
// A readiness status is evidence, not authority. The daemon can report facts
// it can observe on this node; the controls remain available regardless of
// the answer, including when the reading itself is unavailable.
const DEV_READINESS_LABEL={met:"met",unmet:"not met",unobservable:"not observable",unknown:"unknown",unavailable:"unavailable"};
function devReadinessRow(readiness,itemId,reqId){
  if(!readiness||!Array.isArray(readiness.items)) return null;
  const item=readiness.items.find(row=>row&&row.id===itemId);
  if(!item||!Array.isArray(item.requirements)) return null;
  return item.requirements.find(row=>row&&row.id===reqId)||null;
}
function devReadinessPill(found,readiness){
  if(readiness&&readiness.unavailable) return `<span class="pill warn">unavailable</span>`;
  if(!found) return readiness?`<span class="pill warn">not reported</span>`:`<span class="pill">checking…</span>`;
  const label=DEV_READINESS_LABEL[found.status]||"unknown";
  if(found.status==="met") return `<span class="pill" style="color:var(--good);border-color:var(--good)">${esc(label)}</span>`;
  if(found.status==="unmet") return `<span class="pill warn">${esc(label)}</span>`;
  return `<span class="pill">${esc(label)}</span>`;
}
function devReadinessEvidence(found,readiness){
  if(readiness&&readiness.unavailable) return `The prerequisite reading failed: ${readiness.unavailable}`;
  if(found) return found.evidence+(readiness?.observed_at_ms?` Observed ${new Date(readiness.observed_at_ms).toLocaleString()}.`:"");
  return readiness?"This server did not report on this prerequisite.":"";
}
function devReq(readiness,itemId,reqId,title,detail){
  const found=devReadinessRow(readiness,itemId,reqId);
  const key=`${itemId}:${reqId}`;
  return `<div class="devcheck">
    <div class="devcheck-head"><strong>${title}</strong><span class="devcheck-status" data-devstat="${esc(key)}">${devReadinessPill(found,readiness)}</span></div>
    <p class="devcheck-description">${detail}</p>
    <p class="devcheck-evidence" data-devev="${esc(key)}">${esc(devReadinessEvidence(found,readiness))}</p>
  </div>`;
}
// The last readiness document this page received, so a card that re-renders
// itself after its own save can redraw its advisory rows instead of dropping
// them back to "checking…". Set on both the success and the failure path, so
// "unavailable" survives a save too.
let DEVELOPER_READINESS=null;
function applyDeveloperReadiness(readiness){
  DEVELOPER_READINESS=readiness;
  document.querySelectorAll("[data-devstat]").forEach(node=>{
    const [itemId,reqId]=String(node.getAttribute("data-devstat")).split(":");
    node.innerHTML=devReadinessPill(devReadinessRow(readiness,itemId,reqId),readiness);
  });
  document.querySelectorAll("[data-devev]").forEach(node=>{
    const [itemId,reqId]=String(node.getAttribute("data-devev")).split(":");
    node.textContent=devReadinessEvidence(devReadinessRow(readiness,itemId,reqId),readiness);
  });
}

function devStaticReq(title,status,detail,tone){
  return `<div class="devcheck"><div class="devcheck-head"><strong>${title}</strong><span class="devcheck-status"><span class="pill${tone?` ${tone}`:""}">${status}</span></span></div><p class="devcheck-description">${detail}</p></div>`;
}
function macosVideoProcessingCard(settings,readiness){
  const enabled=!!settings.macos_video_processing_enabled;
  return setCard(`${cardHead("Mac video processing","VideoToolbox scaling, HDR/HLG conversion, subtitle burns, deinterlacing and strict Dolby Vision on Mac workers.",`<span class="pill">${enabled?"Enabled":"Disabled"}</span>`)}
    ${togRow("pmacosvideo","Use Mac video processing","New sessions only. Unsupported sources keep their existing processing.",enabled)}
    <details class="setdetails"><summary>Details</summary><div class="setdetails-body">
    ${devReq(readiness,"macos_video_processing","effective_encoder","Effective VideoToolbox encoder","Uses the existing Hardware acceleration selection. Auto can choose Software from its encode benchmark; these switches preserve that choice. Select VideoToolbox there when wanted.")}
    ${devReq(readiness,"macos_video_processing","sdr_scale","Progressive SDR scaling","This worker checks a small embedded 8-bit and 10-bit source corpus.")}
    ${devReq(readiness,"macos_video_processing","hdr10_metal","HDR10 to SDR processing","HDR10 is resized at 10-bit precision before mapping to SDR; each source grade has its own compatibility observation.")}
    ${devReq(readiness,"macos_video_processing","hlg_metal","HLG to SDR processing","HLG has a separate analytical reference-white and rendering check.")}
    ${devReq(readiness,"macos_video_processing","subtitle_burns","SDR, HDR10 and HLG subtitle burns","Text and bitmap cues use their independently checked composition graphs.")}
    ${devReq(readiness,"macos_video_processing","deinterlace","SDR frame and field deinterlacing","TFF/BFF moving-field controls verify native SDR deinterlacing and frame or field cadence. HDR interlaced sources keep the existing processing path.")}
    ${devReq(readiness,"macos_video_processing","dolby_vision","Strict Dolby Vision profile 5","Software/VideoToolbox decode and CPU/Metal renderers have separate metadata and fail-closed observations.")}
    ${devReq(readiness,"macos_video_processing","live_upload","Live TV software decode and native processing","Known SDR sources use explicit upload, optional native deinterlacing and scaling; Live TV keeps H264 output.")}
    ${devReq(readiness,"macos_video_processing","delivery_qualification","Visual and streaming qualification","Runtime smoke checks do not approve visual quality, startup, concurrency, a sustained soak or physical clients. Normalized VOD still requires supported source facts and decoder/encoder ceilings for each source.")}
    <div class="hint">The saved choice is always accepted. Readiness is advisory. Dependency checks require a working /usr/bin/otool (Apple Command Line Tools may be needed on a clean Mac), Apple system-only dependencies for both FFmpeg tools, and no DYLD override.</div>
    <div class="row"><button class="ghost sm" onclick="reprobeMacosVideoProcessing(this)">Check compatibility again</button></div>
    </div></details>
    <div class="err" id="macos-video-error" role="alert"></div>${setCardFoot("saveMacosVideoProcessing")}`,{id:"macos-video-card"});
}
async function saveMacosVideoProcessing(btn){
  const err=document.getElementById("macos-video-error");if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{macos_video_processing_enabled:!!/** @type {HTMLInputElement} */ (document.getElementById("pmacosvideo")).checked}}));
    const card=btn?.closest(".setcard");if(card)card.outerHTML=macosVideoProcessingCard(saved,DEVELOPER_READINESS);
    toast("Mac video processing saved; applies to new sessions");
  }catch(error){if(err)err.textContent=error.message||String(error);if(btn)btn.disabled=false;}
}

function macosHevcOutputCard(settings,readiness){
  const enabled=!!settings.macos_hevc_output_enabled;
  return setCard(`${cardHead("Mac HEVC output","Let Mac workers negotiate SDR HEVC or HDR10 Main10 output.",`<span class="pill">${enabled?"Enabled":"Disabled"}</span>`)}
    ${togRow("pmacoshevc","Use Mac HEVC output","New sessions only, when the client supports the format.",enabled)}
    <details class="setdetails"><summary>Details</summary><div class="setdetails-body">
    ${devReq(readiness,"macos_hevc_output","hevc_effective_encoder","Effective VideoToolbox encoder","Uses the existing Hardware acceleration selection. Auto can choose Software from its encode benchmark; these switches preserve that choice. Select VideoToolbox there when wanted.")}
    ${devReq(readiness,"macos_hevc_output","hevc_sdr","Negotiated SDR HEVC","This worker checks the complete SDR HEVC encoding graph independently.")}
    ${devReq(readiness,"macos_hevc_output","hevc_hdr10","Negotiated HDR10 Main10 HEVC","HDR10 output preserves 10-bit pixels, BT.2020/PQ signaling and effective static HDR metadata. This does not authorize Dolby passthrough.")}
    ${devReq(readiness,"macos_hevc_output","hevc_delivery_qualification","Client and HDR presentation qualification","Produced segments, negotiation, seek/resume, fallback and HDR presentation require independent client and display evidence.")}
    <div class="hint">The saved choice is always accepted. The processing switch and the HEVC output switch are separate preferences.</div>
    </div></details>
    <div class="err" id="macos-hevc-error" role="alert"></div>${setCardFoot("saveMacosHevcOutput")}`,{id:"macos-hevc-card"});
}
async function saveMacosHevcOutput(btn){
  const err=document.getElementById("macos-hevc-error");if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{macos_hevc_output_enabled:!!document.getElementById("pmacoshevc").checked}}));
    const card=btn?.closest(".setcard");if(card)card.outerHTML=macosHevcOutputCard(saved,DEVELOPER_READINESS);
    toast("Mac HEVC output saved; applies to new sessions");
  }catch(error){if(err)err.textContent=error.message||String(error);if(btn)btn.disabled=false;}
}

async function reprobeMacosVideoProcessing(btn){
  const err=document.getElementById("macos-video-error");if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    await api("/developer/macos-video-processing/reprobe",{method:"POST",body:{}});
    applyDeveloperReadiness(await api("/developer/readiness"));
    toast("Compatibility check requested; refresh Settings for its result");
  }catch(error){if(err)err.textContent=error.message||String(error);}
  finally{if(btn)btn.disabled=false;}
}
function systemAttentionHtml(sys){
  const storage=sys.storage||{}, mounts=(storage.mounts||[]).filter(m=>m.read_bps==null||m.cache_suspect);
  const messages=mounts.map(m=>`<p><b>${esc((m.roots||[]).join(', ')||'Storage location')}</b><br>${esc(m.note||'No current read measurement is available.')} ${storage.measured_at?`<span class="muted">Observed ${fmtAgo(storage.measured_at)}.</span>`:''}</p>`);
  if(!sys.ffmpeg_version)messages.push('<p><b>Media tools unavailable.</b> Scanning and transcoding need the configured FFmpeg executable.</p>');
  if(sys.tone_map&&!sys.tone_map.ran&&sys.tone_map.verdicts?.some(v=>v.rejected))messages.push(`<p><b>HDR processing checks have not run.</b> ${esc(sys.tone_map.verdicts.find(v=>v.rejected)?.rejected||'See the capability details below.')}</p>`);
  if(sys.login_proxy_advisory)messages.push('<p><b>Login throttling sees most clients at one proxy address.</b> Configure this node’s <code>server.trusted_proxies</code> only if that proxy overwrites or appends <code>X-Forwarded-For</code> correctly. This is advisory; sign-in remains available.</p>');
  return messages.length?`<section class="system-attention"><h2>Needs attention · ${messages.length} item${messages.length===1?'':'s'}</h2><details><summary>Storage, media or sign-in warnings</summary>${messages.join('')}<div class="row"><a class="ghost sm" href="#/settings/libraries">Review library locations</a><a class="ghost sm" href="#/settings/cluster">Cluster health</a></div></details></section>`:'';
}

function toggleMobileSearch(){
  const top=document.querySelector('header.top');if(!top)return;
  const open=!top.classList.contains('search-open');top.classList.toggle('search-open',open);
  const button=top.querySelector('.mobile-search');if(button)button.setAttribute('aria-expanded',String(open));
  if(open)document.getElementById('q')?.focus();
}

// Developer holds only features still being built (Paul, 2026-11-18): one
// row per feature with its switch, and anything else collapsed in Details.
// A feature that works leaves for its proper section, or loses its switch.
function developerPanel(settings,readiness){
  return `${setHead("Developer","Features still in development.")}
      ${macosVideoProcessingCard(settings,readiness)}${macosHevcOutputCard(settings,readiness)}${hevcCopyCard(settings)}`;
}
function subtitleTranscriptionCard(){
  return setCard(`${cardHead("Generate subtitles offline","Create machine-generated captions locally with whisper.cpp.")}
      <p>Runs as background work. You can follow progress and stop queued or running transcription in Activity.</p>
      <button class="sm" onclick="openSubtitleTranscription(this)">Configure transcription</button>
      <div id="subtitle-transcription-form"></div>`,{id:"enable-subtitle-transcription"});
}
// Async setup and generation results belong to the account and page that
// requested them. A connected node alone is insufficient after sign-out/sign-in.
/** @returns {HTMLElement & {_subtitleSetup?: any, _subtitleLoad?: object}|null} */
function subtitleSetupPanel(){return document.getElementById("subtitle-transcription-form");}
/** @returns {HTMLInputElement} */
function subtitleSetupInput(id){return /** @type {HTMLInputElement} */ (document.getElementById(id));}
/** @returns {HTMLInputElement[]} */
function subtitleSetupWorkers(checked=false){return [.../** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="st-worker"]'+(checked?":checked":"")))];}
function subtitleUiCapture(){return {auth:AUTH_GENERATION,token:TOKEN,page:PAGE_RENDER_GENERATION,route:location.hash};}
function subtitleUiCurrent(c){return !!ME?.is_admin&&c.auth===AUTH_GENERATION&&c.token===TOKEN&&c.page===PAGE_RENDER_GENERATION&&c.route===location.hash;}
function subtitleReason(reason){
  return ({disabled:"Paused: subtitle generation is disabled.",missing_model:"Install the selected model on a compatible worker.",insufficient_disk:"This worker could not safely reserve model storage. Free space or check its storage support, then retry Install.",model_download_failed:"The model download failed. Check this worker’s network access, then retry Install.",engine_unavailable:"This worker’s packaged engine is unavailable or unqualified. Update or repair its supported server package, then run Test.",queue_full:"The background queue is full. Retry after current work finishes.",model_verification_failed:"The model failed its integrity or transcription test. Retry Install.",waiting_for_worker:"Waiting for a compatible worker to take this job.",worker_source_unavailable:"Waiting for a verified worker with access to this media source.",worker_observation_unavailable:"Current worker status is unavailable. Refresh worker status or check the job in Activity.",worker_not_current:"This worker is no longer a current cluster member. Refresh worker status and choose a current worker.",no_compatible_worker:"Waiting for a compatible verified worker.",source_unavailable:"No compatible worker can read this source.",waiting_for_playback_capacity:"Waiting for playback capacity. An interrupted attempt starts again.",worker_interrupted:"The worker stopped. Waiting for another attempt.",decode_failed:"The audio could not be decoded.",no_speech_detected:"The selected audio is silent. Choose another audio track.",inference_failed:"Speech recognition failed.",inference_timeout:"Speech recognition exceeded its time limit.",output_invalid:"The generated captions failed validation.",output_too_large:"The generated captions exceed the size limit.",source_changed:"The media file changed. Refresh this title before trying again.",legacy_profile_incompatible:"This older request cannot run with the current setup. Cancel it and create a new request.",configuration_conflict:"The transcription configuration changed. Reload Settings before saving."})[reason]||"The worker has not reported an actionable reason. Check the job in Activity.";
}
function subtitleJobLink(id,label="Follow this job in Activity"){
  return typeof id==="string"&&/^[a-zA-Z0-9_-]{1,128}$/.test(id)?`<a href="#/activity/jobs/${encodeURIComponent(id)}">${esc(label)}</a>`:"";
}
function subtitleLanguageName(code){try{return new Intl.DisplayNames(["en"],{type:"language"}).of(code)||code;}catch(error){return code;}}
function subtitleLanguageOptions(languages,selected=""){
  const codes=[...new Set(languages||[])];
  return `<option value="">Choose the spoken language</option>${selected&&!codes.includes(selected)?`<option value="${esc(selected)}" selected>${esc(subtitleLanguageName(selected))} · unavailable for this model</option>`:""}${codes.map(code=>`<option value="${esc(code)}"${code===selected?" selected":""}>${esc(subtitleLanguageName(code))}</option>`).join("")}`;
}
function subtitleModelSize(bytes){const value=Number(bytes);return Number.isFinite(value)&&value>=0?(value>=1048576?`${(value/1048576).toFixed(1)} MiB`:`${Math.round(value/1024)} KiB`):"Size unknown";}
function subtitleSetupModelChanged(){
  const panel=subtitleSetupPanel(),setup=panel?._subtitleSetup,language=subtitleSetupInput("st-language");if(!setup||!language)return;
  const model=setup.catalog.models.find(row=>row.id===subtitleSetupInput("st-model-id").value);
  language.innerHTML=subtitleLanguageOptions(model?.languages||[],language.value);refreshSubtitleModels();
}
function subtitleModelOptions(models,selected){
  const rows=(models||[]).map(model=>`<option value="${esc(model.id)}"${model.id===selected?" selected":""}>${esc(model.label||model.id)} · ${esc(subtitleModelSize(model.download_bytes))}</option>`).join("");
  return (models||[]).some(model=>model.id===selected)?rows:`<option value="${esc(selected||"")}" selected>${esc(selected||"Choose a model")}${selected?" · unavailable in this catalog":""}</option>${rows}`;
}
function subtitleTranscriptionForm(settings,catalog={models:[],workers:[]}){
  const advanced=settings.advanced||{},managed=settings.mode!=="advanced";
  return `<div oninput="subtitleSetupEdited()" onchange="subtitleSetupEdited()">
      <label class="tog"><span>Enable offline subtitle generation</span><input id="st-enabled" type="checkbox"${settings.enabled?" checked":""}></label>
      <p class="hint">Disabled pauses queued work and refuses new requests. A running attempt may finish; Cancel it in Activity. Readiness never changes this saved choice.</p>
      <label for="st-mode">Setup</label><select id="st-mode" onchange="subtitleSetupMode()"><option value="managed"${managed?" selected":""}>Managed installation</option><option value="advanced"${managed?"":" selected"}>Advanced · existing local installation</option></select>
      <div id="st-managed"${managed?"":" hidden"}><label for="st-model-id">Model for new requests</label><select id="st-model-id" onchange="subtitleSetupModelChanged()">${subtitleModelOptions(catalog.models,settings.model_id)}</select><p class="hint">Models download only when you choose Download and test. Audio and captions stay on your servers.</p></div>
      <div id="st-advanced"${managed?" hidden":""}><label for="st-command">Existing whisper.cpp executable on the worker</label><input id="st-command" autocomplete="off" value="${esc(advanced.command||"whisper-cli")}"><label for="st-model">Existing local model file on the worker</label><input id="st-model" autocomplete="off" value="${esc(advanced.model_path||"")}"></div>
      <label for="st-language">Default spoken language</label><select id="st-language">${subtitleLanguageOptions((catalog.models||[]).find(model=>model.id===settings.model_id)?.languages||(catalog.models||[]).flatMap(model=>model.languages||[]),settings.language||"en")}</select>
      <label for="st-interval">Check for missing subtitles every (minutes; 0 for manual requests only)</label><input id="st-interval" type="number" min="0" max="10080" step="1" value="${esc(String(settings.interval_mins||0))}">
      <p class="hint">Generation transcribes speech; it does not translate. Machine-generated captions may contain mistakes.</p>
      ${settings.mode_status&&!["ok","advanced"].includes(settings.mode_status)?`<p class="hint">${esc(subtitleReason("configuration_conflict"))}</p>`:""}
      <button class="sm" onclick="saveSubtitleTranscription(this)">Save</button></div><div id="st-status" role="status" aria-live="polite"></div>
      <div id="st-model-evidence">${subtitleModelEvidence(catalog,settings.model_id)}</div><div id="st-operation-status" role="status" aria-live="polite"></div>`;
}
function subtitleModelStatus(status){
  return ({missing:"missing",installed:"installed",verified:"verified",verification_failed:"verification failed",unverified_cache:"Download cached · download and test to finish setup",unknown:"unknown"})[status]||"unknown";
}
function subtitleModelEvidence(catalog,modelId){
  const model=(catalog.models||[]).find(row=>row.id===modelId);
  return `<h3>Download and test a model</h3>${model?`<p>${esc(model.label||model.id)} · ${esc(subtitleModelSize(model.download_bytes))} · ${esc(model.license||"")}</p><details class="setdetails"><summary>Advanced model identity</summary><p class="hint">SHA-256: ${esc(model.sha256||"")} · ${esc(model.source||"")}</p></details>`:`<p class="hint">Select a catalog model to download and test.</p>`}
    <p class="hint">Choose named workers that can run this engine. Installation and testing use background jobs. Installed alone does not mean verified.</p>
    ${(catalog.workers||[]).map(worker=>{const fresh=Number.isFinite(worker.expires_at_ms)&&worker.expires_at_ms>Date.now()&&worker.observed_at_ms<=Date.now()+30000;const entry=(worker.models||[]).find(row=>row.model_id===modelId);const status=fresh?(entry?.status||"missing"):"unknown";return `<div><label class="chk"><input type="checkbox" name="st-worker" value="${esc(worker.node_id)}"${worker.eligible&&fresh&&worker.engine?.status==="available"?" checked":" disabled"}> ${esc(worker.label||worker.node_id)}</label> · ${esc(subtitleModelStatus(status))} · engine ${esc(fresh?worker.engine?.status||"unknown":"unknown")}${entry?.verified_at_ms?` · tested ${esc(new Date(entry.verified_at_ms).toLocaleString())}`:""}${entry?.reason?`<p class="hint">${esc(subtitleReason(entry.reason))}</p>`:""}${!fresh?'<p class="hint">Worker observation is stale; readiness is unknown.</p>':""}</div>`;}).join("")||'<p class="hint">No eligible worker observations are available.</p>'}
    <button class="sm" onclick="subtitleModelOperation('install',this)">Download and test</button> <button class="sm" onclick="subtitleModelOperation('verify',this)">Test selected model</button> <button class="ghost sm" onclick="refreshSubtitleModels()">Refresh worker status</button>`;
}
function subtitleSetupEdited(){const panel=subtitleSetupPanel();if(panel?._subtitleSetup)panel._subtitleSetup.edit++;}
function subtitleSetupMode(){const advanced=subtitleSetupInput("st-mode").value==="advanced";document.getElementById("st-managed").hidden=advanced;document.getElementById("st-advanced").hidden=!advanced;}
async function openSubtitleTranscription(button){
  const panel=subtitleSetupPanel();
  if(!panel||!ME?.is_admin)return;
  const capture=subtitleUiCapture(),request={};panel._subtitleLoad=request;
  if(button)button.disabled=true;
  try{
    const [settingsRead,catalogRead]=await Promise.allSettled([api("/subtitle-transcription/managed"),api("/subtitle-transcription/models")]);
    if(!subtitleUiCurrent(capture)||!panel.isConnected||panel._subtitleLoad!==request)return;
    if(settingsRead.status!=="fulfilled")throw settingsRead.reason;
    const settings=settingsRead.value,catalog=catalogRead.status==="fulfilled"?catalogRead.value:{models:[],workers:[]};
    panel._subtitleSetup={capture,catalog,edit:0,revision:settings.revision};panel.innerHTML=subtitleTranscriptionForm(settings,catalog);
    if(catalogRead.status!=="fulfilled")document.getElementById("st-operation-status").textContent="Model and worker observations are unavailable. Your settings remain editable. Use Refresh worker status to try again.";
  }catch(error){if(subtitleUiCurrent(capture)&&panel.isConnected&&panel._subtitleLoad===request)panel.textContent=error.message||"Could not load transcription settings.";}
  finally{if(subtitleUiCurrent(capture)&&button?.isConnected)button.disabled=false;}
}
async function saveSubtitleTranscription(button){
  const panel=subtitleSetupPanel(),status=document.getElementById("st-status"),setup=panel?._subtitleSetup;
  if(!setup||!status||!subtitleUiCurrent(setup.capture))return;
  const input=id=>/** @type {HTMLInputElement} */ (document.getElementById(id));
  const interval=Number(input("st-interval").value);
  if(!Number.isInteger(interval)||interval<0||interval>10080){status.textContent="Choose a whole number of minutes from 0 to 10080.";return;}
  const body={version:1,expected_revision:setup.revision,mode:input("st-mode").value,enabled:input("st-enabled").checked,model_id:input("st-model-id").value,language:input("st-language").value.trim(),interval_mins:interval};
  if(body.mode==="advanced")body.advanced={command:input("st-command").value.trim(),model_path:input("st-model").value.trim()};
  const revision=setup.edit,request={};setup.save=request;
  if(button)button.disabled=true;status.textContent="Saving…";
  try{
    const saved=await api("/subtitle-transcription/managed",{method:"PUT",body});
    if(!subtitleUiCurrent(setup.capture)||!panel.isConnected||panel._subtitleSetup!==setup||setup.save!==request)return;
    setup.revision=saved.revision;
    if(setup.edit===revision){
      input("st-enabled").checked=saved.enabled;
      for(const [id,value]of [["st-mode",saved.mode],["st-model-id",saved.model_id],["st-language",saved.language],["st-interval",String(saved.interval_mins)],["st-command",saved.advanced?.command||"whisper-cli"],["st-model",saved.advanced?.model_path||""]])input(id).value=value;
    }
    document.getElementById("st-status").textContent=setup.edit===revision?"Saved. Worker readiness does not change this choice.":"Saved the submitted choices. Your newer edits are still unsaved.";
  }catch(error){if(subtitleUiCurrent(setup.capture)&&status.isConnected&&panel._subtitleSetup===setup)status.textContent=error.message||"Could not save transcription settings.";}
  finally{if(subtitleUiCurrent(setup.capture)&&button?.isConnected)button.disabled=false;}
}
async function refreshSubtitleModels(){
  const panel=subtitleSetupPanel(),setup=panel?._subtitleSetup,mount=document.getElementById("st-model-evidence");
  if(!setup||!mount||!subtitleUiCurrent(setup.capture))return;
  const modelId=subtitleSetupInput("st-model-id").value,request={},operation=setup.operation;setup.refresh=request;
  const selected=subtitleSetupWorkers(true).map(node=>node.value);
  try{const catalog=await api("/subtitle-transcription/models");if(!subtitleUiCurrent(setup.capture)||panel._subtitleSetup!==setup||setup.refresh!==request||setup.operation!==operation||!mount.isConnected||subtitleSetupInput("st-model-id").value!==modelId)return;setup.catalog=catalog;subtitleSetupInput("st-model-id").innerHTML=subtitleModelOptions(catalog.models,modelId);mount.innerHTML=subtitleModelEvidence(catalog,modelId);for(const node of subtitleSetupWorkers())if(!node.disabled)node.checked=selected.includes(node.value);}
  catch(error){if(subtitleUiCurrent(setup.capture)&&mount.isConnected&&setup.refresh===request&&setup.operation===operation)document.getElementById("st-operation-status").innerHTML=`${setup.operationRows?.length?subtitleModelOperationsHtml(setup):""}<p>${esc(error.message||"Could not refresh worker status.")} <button class="ghost sm" onclick="refreshSubtitleModels()">Retry worker status</button></p>`;}
}
async function subtitleModelOperation(action,button){
  const panel=subtitleSetupPanel(),setup=panel?._subtitleSetup,status=document.getElementById("st-operation-status");
  if(!setup||!status||!subtitleUiCurrent(setup.capture)||!["install","verify"].includes(action))return;
  const modelId=subtitleSetupInput("st-model-id").value,nodeIds=subtitleSetupWorkers(true).filter(node=>!node.disabled).map(node=>node.value);
  if(!(setup.catalog.models||[]).some(model=>model.id===modelId)){status.textContent="Choose an available catalog model.";return;}
  if(!nodeIds.length){status.textContent="Choose at least one eligible worker.";return;}
  const request={};setup.operation=request;button.disabled=true;status.textContent=action==="install"?"Requesting model installation…":"Requesting model verification…";
  try{const result=await api(`/subtitle-transcription/models/${encodeURIComponent(modelId)}/${action}`,{method:"POST",body:{version:1,node_ids:nodeIds}});
    if(!subtitleUiCurrent(setup.capture)||panel._subtitleSetup!==setup||setup.operation!==request||!status.isConnected)return;
    const operations=result.operations||[];
    setup.operationRows=operations.map(operation=>({...operation,model_id:modelId}));
    status.innerHTML=subtitleModelOperationsHtml(setup);
    pollSubtitleModelOperations(setup,operations.filter(op=>["accepted","existing"].includes(op.outcome)).map(op=>op.job_id));
  }catch(error){if(subtitleUiCurrent(setup.capture)&&status.isConnected&&setup.operation===request)status.textContent=error.message||"Could not request model work.";}
  finally{if(subtitleUiCurrent(setup.capture)&&button.isConnected)button.disabled=false;}
}
function subtitleModelOutcome(outcome){
  return ({accepted:"queued",existing:"already queued",queue_full:"queue full",conflict:"configuration changed",source_changed:"source changed",no_demand:"no work needed",unsupported_worker:"unsupported worker",unavailable_worker:"worker unavailable",worker_not_current:"worker is no longer current",insufficient_disk:"storage unavailable",model_download_failed:"download failed",model_verification_failed:"verification failed",engine_unavailable:"engine unavailable",missing_model:"model missing",disabled:"disabled",configuration_conflict:"configuration changed"})[outcome]||"not accepted";
}
function subtitleModelOperationPhase(job){
  const phases={queued:"Queued",waiting_for_capacity:"Waiting for server capacity",downloading:"Downloading model",verifying:"Verifying model",decoding:"Decoding test audio",transcribing:"Running speech test",validating:"Validating",publishing:"Publishing",succeeded:"Ready",failed:"Failed",cancelled:"Cancelled",running:"Running",cancelling:"Cancelling"};
  if(["succeeded","failed","cancelled"].includes(job?.state))return phases[job.state];
  if(typeof job?.phase==="string"&&Object.hasOwn(phases,job.phase))return phases[job.phase];
  if(typeof job?.state==="string"&&Object.hasOwn(phases,job.state))return phases[job.state];
  return "Status unavailable";
}
function subtitleModelOperationReason(reason){
  if(reason==="output_invalid")return "The model test could not prepare or validate captions. Check server storage and logs, then retry Test.";
  if(reason==="insufficient_disk")return "This worker could not safely reserve model storage. Free space or check its storage support, then retry Download and test.";
  if(reason==="model_download_failed")return "The model download failed. Check this worker’s network access, then retry Download and test.";
  if(reason==="model_verification_failed")return "The model failed its integrity or transcription test. Retry Download and test.";
  return subtitleReason(reason);
}
function subtitleModelOperationsHtml(setup){
  return (setup.operationRows||[]).map(row=>{const job=row.job,label=(setup.catalog.workers||[]).find(worker=>worker.node_id===row.node_id)?.label||row.node_id,reason=job?.reason||row.reason||(!["accepted","existing"].includes(row.outcome)?row.outcome:null);return `<p>${esc(label)}: ${esc(job?subtitleModelOperationPhase(job):(["accepted","existing"].includes(row.outcome)?(row.outcome==="accepted"?"Queued":"Already queued"):subtitleModelOutcome(row.outcome)))}${reason?" · "+esc(subtitleModelOperationReason(reason)):""}. ${subtitleJobLink(row.job_id)}</p>`;}).join("")||"No operation was accepted.";
}
function retrySubtitleModelOperations(){
  const setup=subtitleSetupPanel()?._subtitleSetup;if(!setup||!subtitleUiCurrent(setup.capture))return;
  const ids=(setup.operationRows||[]).filter(row=>["accepted","existing"].includes(row.outcome)&&(!["succeeded","failed","cancelled"].includes(row.job?.state)||row.awaitingObservation)).map(row=>{row.observationDeadline=null;return row.job_id;});
  pollSubtitleModelOperations(setup,ids);
}
function subtitleModelObservationPending(setup,row){
  const job=row.job;
  if(job?.state!=="succeeded")return false;
  const worker=(setup.catalog.workers||[]).find(worker=>worker.node_id===row.node_id);
  const model=worker?.models?.find(model=>model.model_id===row.model_id);
  return !worker||!Number.isFinite(worker.observed_at_ms)||worker.observed_at_ms>Date.now()+30000||!Number.isFinite(worker.expires_at_ms)||worker.expires_at_ms<=Date.now()||
    model?.status!=="verified";
}
async function pollSubtitleModelOperations(setup,ids){
  if(!ids.length||!subtitleUiCurrent(setup.capture))return;
  const panel=subtitleSetupPanel();if(panel?._subtitleSetup!==setup)return;
  const request=setup.operation;
  try{const jobs=await Promise.all(ids.map(id=>api(`/subtitle-transcription/jobs/${encodeURIComponent(id)}`)));
    if(!subtitleUiCurrent(setup.capture)||panel._subtitleSetup!==setup||setup.operation!==request)return;
    for(const job of jobs){const row=(setup.operationRows||[]).find(operation=>operation.job_id===job.job_id);if(row)row.job=job;}
    document.getElementById("st-operation-status").innerHTML=subtitleModelOperationsHtml(setup);
    await refreshSubtitleModels();
    if(!subtitleUiCurrent(setup.capture)||panel._subtitleSetup!==setup||setup.operation!==request)return;
    const pending=jobs.filter(job=>!["succeeded","failed","cancelled"].includes(job.state)).map(job=>job.job_id);
    let updating=false;
    for(const job of jobs){
      const row=(setup.operationRows||[]).find(row=>row.job_id===job.job_id);if(!row)continue;
      row.awaitingObservation=subtitleModelObservationPending(setup,row);
      if(row.awaitingObservation){
        if(row.observationDeadline==null)row.observationDeadline=Date.now()+30000;
        if(Date.now()<row.observationDeadline)pending.push(job.job_id);
        updating=true;
      }
    }
    if(updating)document.getElementById("st-operation-status").innerHTML+=`<p>Verification status is updating. <button class="ghost sm" onclick="retrySubtitleModelOperations()">Refresh verification status</button></p>`;
    if(pending.length)setTimeout(()=>{if(setup.operation===request)pollSubtitleModelOperations(setup,pending);},3000);
  }catch(error){if(subtitleUiCurrent(setup.capture)&&panel._subtitleSetup===setup&&setup.operation===request)document.getElementById("st-operation-status").innerHTML=`${subtitleModelOperationsHtml(setup)}<p>${esc(error.message||"Could not refresh model work.")} <button class="ghost sm" onclick="retrySubtitleModelOperations()">Retry status</button></p>`;}
}

function hevcCopyCard(settings){
  const enabled=!!settings.hevc_unverified_copy;
  const trace=settings.hevc_header_trace_available;
  const indexing=!!settings.vod_index_cluster_cache||Number(settings.vod_index_mins)>0;
  return setCard(`${cardHead("Unverified HEVC copy","Let a VOD copy drop in-band parameter sets before the title is proved.",`<span class="pill${enabled?" warn":""}">${enabled?"Enabled":"Verified VOD only"}</span>`)}
    ${togRow("hevc-unverified","Enable unverified HEVC copy","Can bring back pink/green corruption on files whose parameter sets change. New playback starts only.",enabled)}
    <div class="err" id="hevc-copy-error" role="alert"></div>${setCardFoot("saveHevcCopy")}`,{id:"hevc-copy-card"});
}
async function saveHevcCopy(btn){
  const err=document.getElementById("hevc-copy-error"); if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    const saved=await api("/settings",{method:"PUT",body:{hevc_unverified_copy:!!(/** @type {HTMLInputElement} */ (document.getElementById("hevc-unverified"))).checked}});
    cacheSettings(saved);
    const card=document.getElementById("hevc-copy-card"); if(card)card.outerHTML=hevcCopyCard(saved);
    toast("HEVC copy preference saved");
  }catch(error){if(err)err.textContent=error.message;if(btn)btn.disabled=false;}
}

function subtitleTranslationCard(){
  return setCard(`${cardHead("AI captions and subtitle translation","Generate captions from speech, translate speech to English, or translate an existing text subtitle.",'<span class="pill">Accuracy needs checking</span>')}
    <p>Download and test the speech model on a worker, then choose Generate or translate speech in the player. A separate text model translates existing text subtitles. Each setup shows download progress, worker readiness and an explicit retry.</p>
    <p class="hint">Web, iPhone and Apple TV use the selected audio track and the current playback timeline. Shared viewers can read committed AI captions; a Source administrator requests missing coverage. Partial captions may lag or be wrong. Original captions and Off remain available.</p>
    <button type="button" class="sm" onclick="openTranslationSettings()">Set up speech captions</button><div id="subtitle-translation-form"></div>
    <button type="button" class="sm" onclick="openTextTranslationSettings()">Set up subtitle text translation</button><div id="text-subtitle-translation-settings"></div>`,{id:"enable-subtitle-translation"});
}
