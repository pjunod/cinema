"use strict";
// Progressive captions own text, demands and bounded reads, never the media source.
let SUBTITLE_TRANSLATION=null;
let TRANSLATION_SETUP=null;
const TRANSLATION_READ_MAX=65536;
function translationUuid(){
  if(typeof crypto.randomUUID==="function")return crypto.randomUUID();
  // LAN HTTP pages expose getRandomValues, but not randomUUID.
  const bytes=crypto.getRandomValues(new Uint8Array(16));
  bytes[6]=(bytes[6]&15)|64;bytes[8]=(bytes[8]&63)|128;
  const hex=Array.from(bytes,byte=>byte.toString(16).padStart(2,"0")).join("");
  return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
}
function translationId(value){return typeof value==="string"&&/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value);}
// Continuity describes the viewer's source-clock path, not its moving position.
function translationContinuity(state){return state.continuityId||(state.continuityId=translationUuid());}
function translationRotateContinuity(state){state.continuityId=translationUuid();return state.continuityId;}
function translationProducerGeneration(state){return state.producerGeneration||0;}
function translationSeekWindow(state){
  translationRotateContinuity(state);state.windowGeneration++;state.etag=null;state.lease=null;state.manifestWindow=null;state.coverage=[];
  translationClearCues(state);
}
function translationSeekKey(pending){return pending?JSON.stringify([pending.sequence,pending.intentGeneration,pending.targetMs,!!pending.executed]):null;}
function translationPlaybackSeek(player,pending){
  const state=SUBTITLE_TRANSLATION;
  if(!state||player!==state.player&&player!==state.handoffTarget&&player!==PLAYER||!translationSameSourceAudio(state,player)||!pending?.captionViewerSeek)return;
  const key=translationSeekKey(pending);if(state.continuitySeekKey===key)return;
  state.continuitySeekKey=key;translationSeekWindow(state);
  // An accepted seek invalidates a running claim even while the decoder still
  // shows its previous position. Attachment proof controls later reads/renewals.
  if(state.current()&&!state.rebinding&&state.positionReady!==false&&state.demand&&!state.stopped)void translationHeartbeat(state).catch(()=>{});
}
function translationDigest(value){return typeof value==="string"&&/^[0-9a-f]{64}$/.test(value);}
function translationCounter(value){return typeof value==="string"&&/^(0|[1-9][0-9]{0,18})$/.test(value)&&(value.length<19||value<="9223372036854775807");}
function translationCompare(a,b){if(!translationCounter(a)||!translationCounter(b))throw Error("Invalid translation revision");return a.length-b.length||(a<b?-1:a>b?1:0);}
function translationNext(value){if(!translationCounter(value)||value==="9223372036854775807")throw Error("Translation counter exhausted");return String(BigInt(value)+1n);}
function translationMs(value){return Number.isSafeInteger(value)&&value>=0&&value<=14400000;}
function translationClosed(value,keys){return !!value&&typeof value==="object"&&!Array.isArray(value)&&Object.keys(value).length===keys.length&&keys.every(key=>Object.hasOwn(value,key));}
function translationLiteral(text){return String(text).replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;");}
function translationFailure(code){
  return ({disabled:"Enable AI captions in Settings to request work.",model_missing:"Install the selected speech model in Settings.",translation_unverified:"Test this model for the selected caption task in Settings.",source_unverified:"Checking this source is required before new captions can run.",source_changed:"The media source changed. Refresh this title before requesting captions.",source_clock_unsupported:"This source timeline is not supported for progressive captions.",direct_audio_selection_unverified:"Choose a known audio stream supported for progressive captions.",delivery_unsupported:"Progressive captions are not qualified for this playback route.",waiting_for_worker:"Waiting for a qualified worker with access to the source.",worker_source_unavailable:"Waiting for a qualified worker with access to the source.",waiting_for_playback_capacity:"Waiting for capacity while the picture keeps playing.",demand_expired:"Caption work is suspended. Cached captions remain available.",demand_conflict:"Caption work paused while this player was away. Start it again to continue.",worker_interrupted:"The speech worker stopped. Retry the affected range.",inference_failed:"Speech processing failed for this range. Retry is available when reported.",inference_timeout:"Speech processing exceeded its time limit for this range.",output_invalid:"The generated caption text failed validation.",segment_too_large:"This caption segment exceeds its supported size.",quota_exceeded:"Caption storage or request limits have been reached.",operation_stopped:"Caption generation is stopped. Committed captions remain available.",epoch_conflict:"Caption work changed. Refresh its status before a new action.",intent_expired:"This action expired. Refresh status and try explicitly again.",request_conflict:"This request conflicts with an earlier action. Refresh status.",translation_snapshot_expired:"Caption coverage changed. Refreshing this playback window.",segment_unavailable:"A committed caption segment is unavailable. Retry reading its coverage.",permission_denied:"You no longer have access to these captions.",profile_unavailable:"No qualified speech profile is currently available.",source_verifying:"Checking the media identity before reading captions. Playback continues.",network_error:"Caption status is unavailable. Retry status; playback continues."})[code]||"AI captions are unavailable. Refresh status for current information.";
}
function translationCapture(){return {auth:AUTH_GENERATION,token:TOKEN,page:PAGE_RENDER_GENERATION,route:location.hash};}
function translationAuthCurrent(state){return !!state&&!state.retired&&state.capture.auth===AUTH_GENERATION&&state.capture.token===TOKEN&&!!ME;}
function translationPageCurrent(state){return translationAuthCurrent(state)&&state.capture.page===PAGE_RENDER_GENERATION&&state.capture.route===location.hash;}
function translationAudioOrdinal(player){
  const track=Number.isSafeInteger(player?.curAudio)&&player.curAudio>=0?player.audio?.[player.curAudio]:null;
  if(!track||!Number.isSafeInteger(track.index)||track.index<0)return null;
  const ordinal=typeof selectedAudioIndex==="function"?selectedAudioIndex(player):track.index;
  return Number.isSafeInteger(ordinal)&&ordinal>=0&&ordinal===track.index?ordinal:null;
}
function translationAttachedAudioOrdinal(player){return Number.isSafeInteger(player?.mediaAttachment?.audioOrdinal)?player.mediaAttachment.audioOrdinal:translationAudioOrdinal(player);}
function translationOwnsPlayer(state){
  if(PLAYER===state.player)return true;
  // Preparation has not committed a replacement attachment. The predecessor
  // still owns the element and its captions, including on a refused open.
  return PLAYER?.mediaPredecessor===state.player&&!PLAYER.mediaAttachment;
}
function translationCurrent(state){return translationPageCurrent(state)&&translationOwnsPlayer(state)&&!state.handoffTarget&&document.getElementById("video")===state.video&&state.player.mediaAttachment===state.attachment&&String(state.player.fileId)===state.fileId&&translationAttachedAudioOrdinal(state.player)===state.audioIndex;}
function translationSameSourceAudio(state,player){return translationAuthCurrent(state)&&String(player?.fileId)===state.fileId&&player?.fileContext?.source_ref?.kind==="local"&&translationAudioOrdinal(player)===state.audioIndex;}
function translationHidePresentation(state){
  const track=state.video?._translationTrack;if(!track)return;
  track.mode="disabled";try{while(track.cues?.length)track.removeCue(track.cues[0]);}catch(error){void error;}
}
function translationAttachmentWillChange(player){
  const state=SUBTITLE_TRANSLATION;if(!state)return;
  if(!translationSameSourceAudio(state,player)){translationRetire("playback source or audio changed");return;}
  if(state.handoffTarget===player)return;
  if(player!==state.player&&player.captionIntent==null){player.captionIntent=state.player.captionIntent?{...state.player.captionIntent}:{kind:"off"};player.captionIntentSerial=state.player.captionIntentSerial||0;}
  state.handoffTarget=player;state.positionReady=false;state.presentationValidated=false;state.rebinding=true;state.sourceValidated=false;
  state.windowGeneration++;state.requestGeneration=(state.requestGeneration||0)+1;state.etag=null;state.lease=null;state.manifestWindow=null;
  clearTimeout(state.timer);clearTimeout(state.heartbeatTimer);state.heartbeatTimer=null;
  for(const controller of state.controllers)controller.abort();state.controllers.clear();
  translationHidePresentation(state);
}
function translationAttachmentDidBegin(player,token){
  const state=SUBTITLE_TRANSLATION;if(!state)return;
  if(!translationSameSourceAudio(state,player)){translationRetire("playback source or audio changed");return;}
  state.player=player;state.audioChoice=player.curAudio;state.attachment=token;state.handoffTarget=null;state.positionReady=false;state.rebinding=true;state.sourceValidated=false;
  const generation=state.windowGeneration;
  void (async()=>{
    try{
      const {data}=await translationRequest(state,`/files/${encodeURIComponent(state.fileId)}/subtitle-translation/options`);
      if(!state.current()||state.attachment!==token||state.windowGeneration!==generation)return;
      translationValidateOptions(data,state);
      const profile=state.profile&&translationProfile(data,state.profile.profile_id);
      if(data.source_size!==state.options?.source_size||data.source_mtime!==state.options?.source_mtime||state.profile&&(!profile||profile.model_sha256!==state.profile.model_sha256||profile.timeline_contract!==state.profile.timeline_contract||translationTask(profile)!==translationTask(state.profile))){translationRetire("caption source or profile changed");return;}
      state.options=data;state.sourceValidated=true;
      translationAttachmentPositionReady(player,token);
    }catch(error){if(state.current()&&state.attachment===token&&state.windowGeneration===generation){state.error=error.code?translationFailure(error.code):error.message;translationRenderStatus(state);}}
  })();
}
function translationAttachmentPositionReady(player,token){
  const state=SUBTITLE_TRANSLATION;if(!state||state.player!==player||state.attachment!==token||!state.current()||!token?.captionPositionReady||!state.sourceValidated)return;
  state.positionReady=true;state.rebinding=false;
  if(state.createIntent){state.resumeProducerAfterHandoff=true;if(!state.starting){state.resumeProducerAfterHandoff=false;void translationStart(state);}}
  if(state.assetId||state.operation)void translationPoll(state,true);
  else translationRenderStatus(state);
  if(state.demand&&!state.stopped&&!state.stopIntent){void translationHeartbeat(state).catch(()=>{});translationArmHeartbeat(state);}
}
function translationClock(state,now,started=performance.now()){
  if(Number.isSafeInteger(now)&&now>=0)state.clock={server:now,at:performance.now(),transit:Math.max(0,performance.now()-started)};
}
function translationEnvelope(state){
  if(!state.clock)throw Error("Refresh translation status before an action");
  const elapsed=Math.max(0,performance.now()-state.clock.at);
  if(elapsed>240000)throw Error("Refresh translation status before an action");
  return {version:1,request_id:translationUuid(),intent_expires_at_ms:Math.floor(state.clock.server+elapsed+60000)};
}
async function translationReadBytes(response,max){
  if(!Number.isSafeInteger(max)||max<=0||max>TRANSLATION_READ_MAX)throw Error("Invalid translation read budget");
  const reader=response.body?.getReader();if(!reader)throw Error("Translation streaming reads are unavailable");
  const chunks=[];let length=0;
  try{while(true){const {done,value}=await reader.read();if(done)break;length+=value.byteLength;if(length>max){await reader.cancel();throw Error("Translation response exceeds its budget");}chunks.push(value);}}
  finally{reader.releaseLock();}
  const bytes=new Uint8Array(length);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.byteLength;}return bytes;
}
async function translationRequest(state,path,{method="GET",body=null,headers={},max=TRANSLATION_READ_MAX,raw=false}={}){
  if(!state.current())throw Error("Retired translation request");
  const controller=new AbortController();state.controllers.add(controller);
  const started=performance.now(),auth=state.capture.auth,token=state.capture.token,requestGeneration=state.requestGeneration||0;
  const deadline=setTimeout(()=>controller.abort(),state.limits?.retry_max_ms||30000);
  const readAfter=typeof readAfterRequest==="function"?readAfterRequest():null;
  const requestHeaders={authorization:"Bearer "+token,...headers};
  if(readAfter?.index)requestHeaders["x-plurx-read-after"]=readAfter.index;
  if(body)requestHeaders["content-type"]="application/json";
  try{
    const response=await fetch(API+path,{method,body:body?JSON.stringify(body):null,headers:requestHeaders,signal:controller.signal,cache:"no-store"});
    if(!state.current()||auth!==AUTH_GENERATION||token!==TOKEN||requestGeneration!==(state.requestGeneration||0))throw Error("Retired translation response");
    if(readAfter){const commit=response.headers.get("x-plurx-commit-index");if(commit)observeReadAfter(commit,readAfter);else if(method!=="GET")forgetReadAfter(readAfter);}
    const headerNow=Number(response.headers.get("X-Translation-Server-Now-Ms"));
    const headerClock=response.headers.has("X-Translation-Server-Now-Ms")&&Number.isSafeInteger(headerNow)&&headerNow>=0;
    if(headerClock)translationClock(state,headerNow,started);
    if(response.status===304)return {response,data:null,started};
    const bytes=await translationReadBytes(response,max);
    if(!state.current()||requestGeneration!==(state.requestGeneration||0))throw Error("Retired translation response");
    if(!response.ok){
      let code="network_error",legacySetupError=false;try{const error=JSON.parse(new TextDecoder("utf-8",{fatal:true}).decode(bytes));if(typeof error.code==="string")code=error.code;else legacySetupError=path.startsWith("/subtitle-transcription/")&&typeof error.error==="string";if(!headerClock)translationClock(state,error.server_now_ms,started);}catch(error){void error;}
      if(response.status===401&&auth===AUTH_GENERATION&&token===TOKEN&&typeof logout==="function")void logout({revoke:false});
      const error=Object.assign(new Error(legacySetupError&&response.status!==403?"Model setup request failed. Refresh worker status, then retry. Check Activity for an existing job.":translationFailure(response.status===403?"permission_denied":code)),{code:"",status:response.status});error.code=response.status===403?"permission_denied":code;error.status=response.status;throw error;
    }
    if(!/application\/json/i.test(response.headers.get("content-type")||""))throw Error("Unexpected translation response type");
    if(raw)return {response,data:null,bytes,started};
    const data=JSON.parse(new TextDecoder("utf-8",{fatal:true}).decode(bytes));if(!headerClock)translationClock(state,data.server_now_ms,started);
    return {response,data,bytes,started};
  }finally{clearTimeout(deadline);state.controllers.delete(controller);}
}
function translationDirectReason(player,video){
  if(!player||!video||!player.started||!player.mediaAttachment||player.burnedSub!=null||player.pendingMediaChange||player.pendingOpenAttempt||player.retiringOpenAttempt||player.libraryChannel)return "delivery_unsupported";
  if(!["direct_play","remux","transcode"].includes(player.method))return "delivery_unsupported";
  if(!Number.isFinite(player.offset))return "source_clock_unsupported";
  if(translationAudioOrdinal(player)==null)return "direct_audio_selection_unverified";
  if(player.fileContext?.source_ref?.kind!=="local")return "delivery_unsupported";
  return null;
}
function translationSelected(player=PLAYER){return player?.captionIntent?.kind==="translation";}
function translationSelectSource(player,index){
  if(!player)return 0;
  const serial=player.captionIntentSerial=(player.captionIntentSerial||0)+1;
  player.captionIntent=index<0?{kind:"off"}:{kind:"source",index};
  const video=/** @type {HTMLVideoElement|null} */(document.getElementById("video"));if(video?._translationTrack)video._translationTrack.mode="disabled";
  return serial;
}
function translationRetire(reason="retired"){
  const state=SUBTITLE_TRANSLATION;if(!state)return;
  // Release is best effort; server TTL bounds work even after network/auth loss.
  if(translationCurrent(state)&&state.demand&&!state.stopped)void translationRelease(state).catch(()=>{});
  translationRotateContinuity(state);state.producerGeneration=translationProducerGeneration(state)+1;
  state.retired=true;clearTimeout(state.timer);clearTimeout(state.heartbeatTimer);state.heartbeatTimer=null;for(const controller of state.controllers)controller.abort();
  state.controllers.clear();for(const [kind,handler]of state.listeners)state.video.removeEventListener(kind,handler);
  if(state.video._translationTrack)state.video._translationTrack.mode="disabled";
  if(translationSelected(state.player)){state.player.captionIntent={kind:"off"};state.player.captionIntentSerial=(state.player.captionIntentSerial||0)+1;}
  state.reason=reason;SUBTITLE_TRANSLATION=null;
  if(state.panel?.isConnected){state.panel.hidden=true;state.panel.replaceChildren();}
}
function translationTask(profile){return profile?.task==="transcribe"?"transcribe":"translate";}
function translationOutputLanguage(profile,spokenLanguage){return translationTask(profile)==="transcribe"?spokenLanguage:"en";}
function translationTaskLabel(task){return task==="transcribe"?"Generate captions in the spoken language":"Translate speech to English";}
function translationTaskNoun(task){return task==="transcribe"?"captions":"translation";}
function translationProfile(options,id){return (options.models||[]).find(profile=>profile.profile_id===id);}
function translationTaskProfiles(options,task){return (options.models||[]).filter(profile=>translationTask(profile)===task);}
function translationDefaultHint(options){
  const id=options.default_profile_id;if(id==null)return null;
  const profile=translationDigest(id)&&translationProfile(options,id);
  if(!profile)throw Error("Invalid preferred speech profile");return profile;
}
function translationPreferredProfile(options,task,preferredId=null,preferredModel=null){
  const hint=translationDefaultHint(options),rows=translationTaskProfiles(options,task);
  if(preferredId){
    const exact=translationProfile(options,preferredId);
    if(!exact)throw Error(translationFailure("profile_unavailable"));
    return rows.find(profile=>profile.profile_id===preferredId)||rows.find(profile=>profile.model_id===exact.model_id)||null;
  }
  if(preferredModel)return rows.find(profile=>profile.model_id===preferredModel)||null;
  return rows.find(profile=>profile.profile_id===hint?.profile_id)||rows.find(profile=>profile.model_id===hint?.model_id)
    ||rows.find(profile=>profile.default===true)||rows.find(profile=>profile.activation_state==="active")||rows[0]||null;
}
function translationProfileLimits(profile){
  const limits=profile?.limits;if(!limits)return null;
  const keys=["segment_bytes","segment_cues","page_bytes","page_entries","coverage_intervals","concurrent_fetches","cue_memory_bytes","cue_count","cue_window_before_ms","cue_window_after_ms","poll_min_ms","poll_max_ms","retry_max_ms","heartbeat_interval_ms"];
  if(!keys.every(key=>Number.isSafeInteger(limits[key])&&limits[key]>0)||limits.segment_bytes>TRANSLATION_READ_MAX||limits.page_bytes>TRANSLATION_READ_MAX||limits.concurrent_fetches>16)return null;
  return limits;
}
function translationPanel(state){const panel=document.getElementById("watch-subtitle-translation");return translationCurrent(state)&&panel===state.panel&&panel?.isConnected?panel:null;}
function translationStatus(state,message){const node=translationPanel(state)?.querySelector("[data-translation-status]");if(node&&node.textContent!==message)node.textContent=message;}
function translationRangeStatus(coverage,position){
  const rows=coverage.slice().sort((a,b)=>a.start_ms-b.start_ms);let end=null;
  for(const row of rows){if(row.start_ms<=position&&row.end_ms>position)end=Math.max(end??0,row.end_ms);else if(end!=null&&row.start_ms<=end)end=Math.max(end,row.end_ms);}
  if(end!=null)return {covered:true,runway_ms:Math.max(0,end-position),lag_ms:0};
  const previous=rows.filter(row=>row.end_ms<=position).reduce((end,row)=>Math.max(end,row.end_ms),0);
  return {covered:false,runway_ms:0,lag_ms:Math.max(0,position-previous)};
}
function translationPosition(state){const ms=Math.round(((state.player.offset||0)+state.video.currentTime)*1000);return translationMs(ms)?ms:0;}
function translationRenderStatus(state){
  const operation=state.operation,position=translationPosition(state),coverage=translationRangeStatus(state.coverage,position);
  const task=state.task||translationTask(state.profile),noun=translationTaskNoun(task),output=state.outputLanguage||translationOutputLanguage(state.profile,state.spokenLanguage),outputName=output?(typeof subtitleLanguageName==="function"?subtitleLanguageName(output):output):"spoken language";
  let message=operation?.state==="checking_source"?`Checking source before ${noun}…`:operation?.reason?translationFailure(operation.reason):`Preparing AI ${noun}…`;
  if(state.assetId){
    if(state.stopped)message=`${noun[0].toUpperCase()+noun.slice(1)} stopped. Committed captions remain available.`;
    else if(coverage.covered)message=operation?.state==="complete"?`AI ${noun} complete.`:`Partial ${outputName} captions available.`;
    else message=ME?.is_admin&&state.demand?`Processing this part. Playback continues.`:`Not available here yet. Ask an administrator to process this part.`;
  }
  if(state.stopIntent&&!state.stopped)message="Stop is unconfirmed. Retry Stop; cached captions remain available.";
  if(state.error)message=state.error;
  translationStatus(state,message);
  const panel=translationPanel(state);if(!panel)return;
  const clock=panel.querySelector("[data-translation-clock]");if(clock)clock.textContent=coverage.covered?`Coverage ahead: ${Math.floor(coverage.runway_ms/1000)} seconds.`:`Uncovered here · lag ${Math.floor(coverage.lag_ms/1000)} seconds.`;
  const use=/** @type {HTMLButtonElement|null} */(panel.querySelector("[data-translation-use]"));if(use){use.disabled=!state.assetId||!state.coverage.length;use.textContent=translationSelected(state.player)&&state.player.captionIntent.asset_id===state.assetId?"AI captions selected":"Use AI captions";}
  const stop=/** @type {HTMLButtonElement|null} */(panel.querySelector("[data-translation-stop]"));if(stop)stop.hidden=!ME?.is_admin||!state.options.permissions?.stop||!state.operation||state.stopped;
  const resume=/** @type {HTMLButtonElement|null} */(panel.querySelector("[data-translation-resume]"));if(resume)resume.hidden=!ME?.is_admin||!state.options.permissions?.resume||!state.operation||!(state.operation.state==="stopped"||state.operation.state==="failed"&&state.operation.retryable);
  const failures=panel.querySelector("[data-translation-failures]");
  const failureSignature=JSON.stringify([state.operation?.failed_ranges||[],state.operation?.failed_ranges_next_cursor,translationLeaseCurrent(state)]);
  if(!failures||state.failureSignature===failureSignature)return;
  state.failureSignature=failureSignature;
  failures.replaceChildren();
  for(const range of state.operation?.failed_ranges||[]){
    if(!range.retryable||!ME?.is_admin||!state.options.permissions?.demand)continue;
    failures.appendChild(translationActionButton(`Retry range ${range.core_index+1}`,()=>translationRetryRange(state,range)));
  }
  const cursor=state.operation?.failed_ranges_next_cursor;
  if(cursor&&translationLeaseCurrent(state)){
    const more=translationActionButton("More failed ranges",()=>translationFailurePage(state,cursor,more));
    failures.appendChild(more);
  }
}
function translationActionButton(label,onClick){
  const button=document.createElement("button");
  button.className="ghost sm";
  button.textContent=label;
  button.addEventListener("click",onClick);
  return button;
}
function translationAmbiguous(error){return error.status==null||error.status===408||error.status>=500;}
function translationRejectIntent(state,key,error){if(!translationAmbiguous(error))state[key]=null;}
function translationClearCues(state){
  state.cues=new Map();const track=state.video._translationTrack;if(track){track.mode="hidden";try{while(track.cues?.length)track.removeCue(track.cues[0]);}catch(error){void error;}}
}
function translationSwitchAsset(state){
  state.producerGeneration=translationProducerGeneration(state)+1;translationRotateContinuity(state);
  state.windowGeneration++;state.etag=null;state.lease=null;clearTimeout(state.timer);
  for(const controller of state.controllers)controller.abort();
  if(state.demand)void translationRelease(state).catch(()=>{});
  state.demand=null;
  state.coverage=[];state.segments.clear();translationClearCues(state);state.operation=null;state.failurePage=null;state.assetId=null;state.contentRevision=null;state.spokenLanguage=null;state.outputLanguage=null;state.createIntent=null;state.createKey=null;state.stopIntent=null;state.resumeIntent=null;state.rangeIntent=null;state.stopKey=null;state.resumeKey=null;state.rangeKey=null;state.stopped=false;
}
function translationValidateOptions(options,state){
  if(options?.version!==1||options.file_id!==state.fileId||typeof options.source_size!=="string"||typeof options.source_mtime!=="string"||!translationMs(options.duration_ms)||!Array.isArray(options.audio_tracks)||!Array.isArray(options.models))throw Error(translationFailure("direct_audio_selection_unverified"));
  translationDefaultHint(options);
  const matches=options.audio_tracks.filter(track=>track.audio_ordinal===state.audioIndex),selected=matches[0];if(matches.length!==1||!Number.isSafeInteger(selected.index)||selected.index<0)throw Error(translationFailure("direct_audio_selection_unverified"));
  if(selected.capability?.supported!==true)throw Error(translationFailure(selected.capability?.reason||"direct_audio_selection_unverified"));
  const sourceOriginMs=selected.source_origin_ms??options.source_origin_ms;if(sourceOriginMs!=null&&(!Number.isSafeInteger(sourceOriginMs)||Math.abs(sourceOriginMs)>604800000))throw Error(translationFailure("source_clock_unsupported"));
}
function translationClockStale(state){return !state.clock||Math.max(0,performance.now()-state.clock.at)>240000;}
async function translationRefreshOptions(state){
  if(state.optionsRefresh)return state.optionsRefresh;
  const generation=state.windowGeneration,clock=state.clock;
  const refresh=(async()=>{
    try{
      const {data}=await translationRequest(state,`/files/${encodeURIComponent(state.fileId)}/subtitle-translation/options`);
      if(!state.current()||state.windowGeneration!==generation||state.operation||state.assetId)return false;
      translationValidateOptions(data,state);
      if(data.source_size!==state.options.source_size||data.source_mtime!==state.options.source_mtime)throw Error(translationFailure("source_changed"));
      const panel=translationPanel(state);if(!panel)return false;
      const taskSelect=/** @type {HTMLSelectElement} */(panel.querySelector("[data-translation-task]")),profiles=/** @type {HTMLSelectElement} */(panel.querySelector("[data-translation-profile]")),languages=/** @type {HTMLSelectElement} */(panel.querySelector("[data-translation-language]"));
      const task=taskSelect.value,selected=profiles.value,language=languages.value,prior=translationProfile(state.options,selected),profile=translationProfile(data,selected);
      if(prior&&profile&&(prior.model_id!==profile.model_id||prior.model_sha256!==profile.model_sha256||prior.timeline_contract!==profile.timeline_contract||translationTask(prior)!==translationTask(profile)))throw Error("Caption profile identity changed. Reopen caption choices.");
      const tasks=[...new Set([task,...["translate","transcribe"].filter(value=>translationTaskProfiles(data,value).length)])];
      const rows=translationTaskProfiles(data,task),choices=[...new Set([...(profile?.input_languages||[]),...(language?[language]:[])])];
      taskSelect.innerHTML=tasks.map(value=>`<option value="${esc(value)}">${translationTaskLabel(value)}</option>`).join("");taskSelect.value=task;
      profiles.innerHTML=(selected&&!rows.some(row=>row.profile_id===selected)?`<option value="${esc(selected)}">Selected model unavailable</option>`:"")+rows.map(row=>`<option value="${esc(row.profile_id)}">${esc(row.label||row.model_id)}</option>`).join("");profiles.value=selected;
      languages.innerHTML='<option value="">Choose spoken language</option>'+choices.map(code=>`<option value="${esc(code)}">${esc(typeof subtitleLanguageName==="function"?subtitleLanguageName(code):code)}</option>`).join("");languages.value=language;
      state.options=data;if(state.profile?.profile_id===selected&&profile){state.profile=profile;state.limits=translationProfileLimits(profile);}
      /** @type {HTMLButtonElement} */(panel.querySelector("[data-translation-start]")).hidden=!data.permissions?.demand||!ME?.is_admin;
      translationAssetChoices(state,data.assets||[],data.assets_next_cursor);
      return true;
    }catch(error){if(state.current()&&state.windowGeneration===generation)state.clock=clock;throw error;}
  })();
  state.optionsRefresh=refresh;
  try{return await refresh;}finally{if(state.optionsRefresh===refresh)state.optionsRefresh=null;}
}
async function translationOpen(button){
  const player=PLAYER,video=document.getElementById("video"),reason=translationDirectReason(player,video);
  if(reason){toast(translationFailure(reason));return;}
  const previous=SUBTITLE_TRANSLATION;if(previous&&translationCurrent(previous)){previous.panel?.scrollIntoView({block:"nearest"});return;}
  translationRetire();
  const panel=document.getElementById("watch-subtitle-translation");if(!panel)return;
  const state={capture:translationCapture(),player,video,panel,attachment:player.mediaAttachment,fileId:String(player.fileId),audioChoice:player.curAudio,audioIndex:translationAttachedAudioOrdinal(player),positionReady:player.mediaAttachment.captionPositionReady!==false,presentationValidated:true,requestGeneration:0,task:"translate",controllers:new Set(),listeners:[],heartbeatTimer:null,heartbeatAttempt:0,segments:new Map(),cues:new Map(),coverage:[],operation:null,assetId:null,demand:null,playbackId:translationUuid(),continuityId:translationUuid(),producerGeneration:0,queue:Promise.resolve(),timer:null,windowGeneration:0,retired:false,error:null};
  state.current=()=>translationCurrent(state);SUBTITLE_TRANSLATION=state;panel.hidden=false;panel.textContent="Loading AI caption choices…";
  try{
    const {data:options}=await translationRequest(state,`/files/${encodeURIComponent(state.fileId)}/subtitle-translation/options`);translationValidateOptions(options,state);state.options=options;
    if(!translationPanel(state))return;
    const audio=options.audio_tracks.find(track=>track.audio_ordinal===state.audioIndex);
    const availableTasks=["translate","transcribe"].filter(task=>translationTaskProfiles(options,task).length);
    if(!availableTasks.length)throw Error(translationFailure("profile_unavailable"));
    state.task=availableTasks.includes("translate")?"translate":"transcribe";
    panel.innerHTML=`<div class="card" style="padding:16px"><h3 data-translation-heading>${translationTaskLabel(state.task)}</h3><p data-translation-description>AI captions are an estimate and may be wrong. The picture keeps playing; original captions and Off stay available.</p>${ME?.is_admin?'<p><a href="#/settings/playback/enable-subtitle-translation">AI caption settings</a></p>':""}<p>Spoken audio: ${esc(audio.title||audio.language||"Untagged audio")} · stream ${esc(String(state.audioIndex+1))}</p><label>Caption task <select data-translation-task>${availableTasks.map(task=>`<option value="${task}">${translationTaskLabel(task)}</option>`).join("")}</select></label><label>Speech model <select data-translation-profile></select></label><label>Spoken language <select data-translation-language></select></label><p class="hint">Confirm the language spoken in the attached audio. Metadata can be wrong; correct it here.</p><label class="chk"><input type="checkbox" data-translation-confirm> I confirm this is the spoken language.</label><button class="sm" data-translation-start>${translationTaskLabel(state.task)}</button> <button class="ghost sm" data-translation-use disabled>Use generated captions</button> <button class="ghost sm" data-translation-stop hidden>Stop caption work for everyone</button> <button class="ghost sm" data-translation-resume hidden>Resume caption work</button> <button class="ghost sm" data-translation-refresh>Refresh status</button><div data-translation-assets></div><div data-translation-failures></div><p data-translation-status role="status" aria-live="polite"></p><p class="hint" data-translation-clock aria-live="off"></p></div>`;
    const taskSelect=/** @type {HTMLSelectElement} */(panel.querySelector("[data-translation-task]")),profiles=/** @type {HTMLSelectElement} */(panel.querySelector("[data-translation-profile]"));
    const updateLanguage=()=>{const profile=translationProfile(state.options,profiles.value),select=/** @type {HTMLSelectElement} */(panel.querySelector("[data-translation-language]")),previous=select.value,tag=audio.normalized_language,choices=profile?.input_languages||[],chosen=choices.includes(previous)?previous:choices.includes(tag)?tag:"";select.innerHTML=`<option value="">Choose spoken language</option>`+choices.map(code=>`<option value="${esc(code)}"${code===chosen?" selected":""}>${esc(typeof subtitleLanguageName==="function"?subtitleLanguageName(code):code)}</option>`).join("");select.value=chosen;panel.querySelector("[data-translation-heading]").textContent=translationTaskLabel(state.task);panel.querySelector("[data-translation-start]").textContent=translationTaskLabel(state.task);};
    const updateProfiles=(preferred)=>{const rows=translationTaskProfiles(state.options,state.task),active=translationPreferredProfile(state.options,state.task,preferred);profiles.innerHTML=rows.map(profile=>`<option value="${esc(profile.profile_id)}">${esc(profile.label||profile.model_id)}</option>`).join("");profiles.value=active?.profile_id||"";updateLanguage();};
    updateProfiles(null);
    taskSelect.value=state.task;
    taskSelect.addEventListener("change",()=>{const prior=profiles.value;state.task=taskSelect.value;translationSwitchAsset(state);updateProfiles(prior);translationRenderStatus(state);});
    profiles.addEventListener("change",()=>{translationSwitchAsset(state);updateLanguage();translationRenderStatus(state);});
    panel.querySelector("[data-translation-language]").addEventListener("change",()=>{translationSwitchAsset(state);translationRenderStatus(state);});
    /** @type {HTMLButtonElement} */(panel.querySelector("[data-translation-start]")).hidden=!options.permissions?.demand||!ME?.is_admin;
    panel.querySelector("[data-translation-start]").addEventListener("click",()=>translationStart(state));
    panel.querySelector("[data-translation-use]").addEventListener("click",()=>translationUse(state));
    panel.querySelector("[data-translation-stop]").addEventListener("click",()=>translationStop(state));
    panel.querySelector("[data-translation-resume]").addEventListener("click",()=>translationResume(state));
    panel.querySelector("[data-translation-refresh]").addEventListener("click",()=>{state.error=null;void translationPoll(state,true);});
    translationAssetChoices(state,options.assets||[],options.assets_next_cursor);
    for(const kind of ["seeking","seeked","timeupdate","ratechange","pause","play"]){const listener=()=>translationMediaEvent(state,kind);video.addEventListener(kind,listener);state.listeners.push([kind,listener]);}
    state.error=options.models?.some(profile=>translationProfileLimits(profile))?null:translationFailure("profile_unavailable");translationRenderStatus(state);
  }catch(error){if(translationPanel(state)){state.error=error.message;panel.textContent=error.message;}}
  finally{if(button?.isConnected)button.disabled=false;}
}
function translationAssetChoices(state,entries,cursor){
  const root=translationPanel(state)?.querySelector("[data-translation-assets]");if(!root)return;
  if(!Array.isArray(entries)||entries.length>64||cursor!=null&&(typeof cursor!=="string"||cursor.length>1024))throw Error("Invalid existing translation page");
  root.replaceChildren();
  for(const asset of entries){if(!translationDigest(asset.asset_id))throw Error("Invalid existing caption identity");const profile=translationProfile(state.options,asset.profile_id),task=asset.task||translationTask(profile),outputLanguage=asset.output_language||translationOutputLanguage(profile,asset.spoken_language),outputName=typeof subtitleLanguageName==="function"?subtitleLanguageName(outputLanguage):outputLanguage;const button=document.createElement("button");button.className="ghost sm";button.textContent=`Read existing ${outputName} ${task==="transcribe"?"captions":"translation"}`;button.addEventListener("click",()=>translationReadAsset(state,asset));root.appendChild(button);}
  if(cursor){const more=document.createElement("button");more.className="ghost sm";more.textContent="More existing captions";more.addEventListener("click",()=>translationAssetPage(state,cursor,more));root.appendChild(more);}
}
async function translationAssetPage(state,cursor,button){
  if(!state.current()||state.assetPageLoading)return;state.assetPageLoading=true;button.disabled=true;
  try{const {data}=await translationRequest(state,`/files/${encodeURIComponent(state.fileId)}/subtitle-translation/assets?cursor=${encodeURIComponent(cursor)}`);
    if(!state.current())return;
    if(!translationClosed(data,["version","file_id","entries","next_cursor","server_now_ms"])||data.version!==1||data.file_id!==state.fileId)throw Error("Existing translations belong to another source");
    // Keep one bounded page of choices, not an accumulating film catalogue.
    translationAssetChoices(state,data.entries,data.next_cursor);
  }catch(error){if(state.current()){state.error=error.message;translationRenderStatus(state);}}
  finally{state.assetPageLoading=false;if(button.isConnected)button.disabled=false;}
}
async function translationFailurePage(state,cursor,button){
  if(!state.current()||!state.operation||!translationLeaseCurrent(state)||state.failurePageLoading)return;
  const operation=state.operation,lease=state.lease,generation=state.windowGeneration;
  if(typeof cursor!=="string"||cursor.length>1024)return;
  state.failurePageLoading=true;button.disabled=true;
  try{const {data}=await translationRequest(state,`/subtitle-translation/operations/${operation.operation_id}/failed-ranges?cursor=${encodeURIComponent(cursor)}`,{headers:{"X-Translation-Read-Lease":lease.id},max:state.limits.page_bytes});
    if(!state.current()||state.windowGeneration!==generation||state.lease!==lease||!translationLeaseCurrent(state)||state.operation.operation_epoch!==operation.operation_epoch||state.operation.view_revision!==operation.view_revision)return;
    if(!translationClosed(data,["version","operation_id","operation_epoch","view_revision","entries","next_cursor"])||data.version!==1||data.operation_id!==operation.operation_id||data.operation_epoch!==operation.operation_epoch||data.view_revision!==operation.view_revision||!Array.isArray(data.entries)||data.entries.length>16)throw Error("Failed ranges belong to another operation view");
    translationValidateFailures(data.entries,data.next_cursor);state.failurePage={operation_id:operation.operation_id,operation_epoch:operation.operation_epoch,view_revision:operation.view_revision,entries:data.entries,next_cursor:data.next_cursor};state.operation={...state.operation,failed_ranges:data.entries,failed_ranges_next_cursor:data.next_cursor};translationRenderStatus(state);
  }catch(error){if(state.current()&&state.windowGeneration===generation){if(error.status===410){state.lease=null;state.etag=null;}state.error=error.message;translationRenderStatus(state);}}
  finally{state.failurePageLoading=false;if(button.isConnected)button.disabled=false;}
}
function translationValidateFailures(rows,cursor){
  if(!Array.isArray(rows)||rows.length>16||cursor!=null&&(typeof cursor!=="string"||cursor.length>1024))throw Error("Invalid failed range page");
  for(const range of rows)if(!Number.isSafeInteger(range.core_index)||range.core_index<0||!translationCounter(range.range_revision)||typeof range.retryable!=="boolean")throw Error("Invalid failed range identity");
}
function translationAdoptReceipt(state,receipt,sent=null){
  if(sent?.continuity_id!=null&&(!translationId(sent.continuity_id)||receipt?.continuity_id!==sent.continuity_id))throw Error("Caption receipt continuity does not match its request");
  if(receipt?.version!==1||!translationId(receipt.operation_id)||!translationCounter(receipt.operation_epoch)||!translationCounter(receipt.view_revision)||receipt.asset_id!=null&&!translationDigest(receipt.asset_id)||receipt.task!=null&&receipt.task!==translationTask(state.profile)||receipt.output_language!=null&&receipt.output_language!==translationOutputLanguage(state.profile,receipt.spoken_language||state.spokenLanguage))throw Error("Invalid caption operation receipt");
  if(state.operation&&receipt.operation_id!==state.operation.operation_id)throw Error("Translation receipt belongs to another operation");
  if(receipt.demand_id&&(!translationId(receipt.demand_id)||!translationCounter(receipt.demand_generation)))throw Error("Invalid translation demand receipt");
  const demand=receipt.demand_id?{id:receipt.demand_id,generation:receipt.demand_generation,epoch:receipt.operation_epoch}:null;
  if(state.operation&&(translationCompare(receipt.operation_epoch,state.operation.operation_epoch)<0||receipt.operation_epoch===state.operation.operation_epoch&&translationCompare(receipt.view_revision,state.operation.view_revision)<0)){
    // A newer read view must not hide an acknowledged mutation's demand CAS.
    // Its operation presentation stays current, and older epochs stay fenced.
    if(sent?.continuity_id&&demand&&receipt.operation_epoch===state.operation.operation_epoch&&state.operation.state!=="stopped"){
      state.demand=demand;return true;
    }
    return false;
  }
  state.operation={...state.operation,...receipt};state.stopped=receipt.state==="stopped";
  if(receipt.asset_id)state.assetId=receipt.asset_id;
  if(demand)state.demand=demand;
  if(state.stopped)state.demand=null;
  return true;
}
async function translationStart(state){
  if(!translationPanel(state)||!ME?.is_admin)return;
  if(!state.operation&&!state.assetId&&!state.createIntent&&translationClockStale(state)){
    try{if(!await translationRefreshOptions(state))return;}
    catch(error){if(state.current()){state.error=error.message;translationRenderStatus(state);}return;}
  }
  if(!state.current()||!state.options.permissions?.demand)return;
  const reason=translationDirectReason(state.player,state.video);if(reason){state.error=translationFailure(reason);translationRenderStatus(state);return;}
  const profile=translationProfile(state.options,state.panel.querySelector("[data-translation-profile]").value),language=state.panel.querySelector("[data-translation-language]").value;
  const task=state.panel.querySelector("[data-translation-task]").value,outputLanguage=translationOutputLanguage(profile,language);
  if(!profile||translationTask(profile)!==task||profile.activation_state!=="active"||!translationProfileLimits(profile)){state.error=translationFailure("profile_unavailable");translationRenderStatus(state);return;}
  if(!profile.input_languages.includes(language)||!state.panel.querySelector("[data-translation-confirm]").checked){state.error="Choose and confirm the language spoken in this audio stream.";translationRenderStatus(state);return;}
  if(state.starting)return;state.starting=true;state.error=null;let generation=translationProducerGeneration(state);
  try{
    const intentKey=JSON.stringify([state.fileId,state.options.source_size,state.options.source_mtime,state.audioIndex,task,language,outputLanguage,profile.profile_id]);
    if(state.createKey!==intentKey){const changed=state.createKey!=null||state.profile&&state.profile.profile_id!==profile.profile_id||state.spokenLanguage!=null&&state.spokenLanguage!==language;if(changed)translationSwitchAsset(state);state.createIntent=null;state.createKey=intentKey;}
    state.task=task;state.profile=profile;state.spokenLanguage=language;state.outputLanguage=outputLanguage;state.limits=translationProfileLimits(profile);generation=translationProducerGeneration(state);
    const body=state.createIntent||(state.createIntent={...translationEnvelope(state),source_size:state.options.source_size,source_mtime:state.options.source_mtime,audio_index:state.audioIndex,spoken_language:language,output_language:outputLanguage,task,model_id:profile.model_id,profile_id:profile.profile_id,playback_id:state.playbackId,continuity_id:translationContinuity(state),position_ms:translationPosition(state),playback_rate:state.video.playbackRate,paused:state.video.paused});
    const {data}=await translationRequest(state,`/files/${encodeURIComponent(state.fileId)}/subtitle-translation`,{method:"POST",body});
    if(!state.current()||translationProducerGeneration(state)!==generation)return;translationAdoptReceipt(state,data,body);state.error=null;state.createIntent=null;translationRenewContinuity(state,body);await translationPoll(state,true);
  }catch(error){if(state.current()&&translationProducerGeneration(state)===generation){translationRejectIntent(state,"createIntent",error);state.error=error.code?translationFailure(error.code):error.message;translationRenderStatus(state);}}
  finally{state.starting=false;if(state.resumeProducerAfterHandoff&&state.current()&&state.positionReady!==false&&!state.rebinding){state.resumeProducerAfterHandoff=false;if(state.createIntent)void translationStart(state);}}
}
function translationValidateOperation(state,operation){
  if(operation?.version!==1||operation.operation_id!==state.operation.operation_id||operation.file_id!==state.fileId||operation.source_size!==state.options.source_size||operation.source_mtime!==state.options.source_mtime||operation.audio_index!==state.audioIndex||operation.output_language!==translationOutputLanguage(state.profile,operation.spoken_language)||operation.task!==translationTask(state.profile)||operation.profile_id!==state.profile.profile_id||!translationCounter(operation.operation_epoch)||!translationCounter(operation.view_revision))throw Error("Caption operation identity changed");
  if(operation.model_id!==state.profile.model_id||operation.model_sha256!==state.profile.model_sha256||operation.timeline_contract!==state.profile.timeline_contract||!state.profile.input_languages.includes(operation.spoken_language)||state.spokenLanguage&&operation.spoken_language!==state.spokenLanguage)throw Error("Translation profile or spoken language changed");
  if(operation.asset_id!=null&&(!translationDigest(operation.asset_id)||!translationDigest(operation.content_revision)))throw Error("Invalid translation asset");
  const states=["checking_source","waiting_model","waiting_capacity","translating","partial","suspended","stopped","complete","failed"];
  if(!states.includes(operation.state)||!Array.isArray(operation.failed_ranges)||operation.failed_ranges.length>16)throw Error("Invalid translation operation state");
  translationValidateFailures(operation.failed_ranges,operation.failed_ranges_next_cursor);
  if(operation.active_job&&(!translationId(operation.active_job.job_id)||!translationCounter(operation.active_job.fence)))throw Error("Invalid translation claim observation");
}
async function translationReadAsset(state,asset){
  if(!state.current()||!state.options.permissions?.read||!translationDigest(asset.asset_id))return;
  const profile=translationProfile(state.options,asset.profile_id);if(!translationProfileLimits(profile)){state.error=translationFailure("profile_unavailable");translationRenderStatus(state);return;}
  const task=asset.task||translationTask(profile),spokenLanguage=asset.spoken_language||null,outputLanguage=asset.output_language||translationOutputLanguage(profile,spokenLanguage);
  if(asset.task!=null&&asset.task!==task||asset.output_language!=null&&asset.output_language!==outputLanguage||!spokenLanguage||!profile.input_languages.includes(spokenLanguage)){state.error="Caption asset task or language does not match its speech profile.";translationRenderStatus(state);return;}
  translationSwitchAsset(state);state.profile=profile;state.limits=translationProfileLimits(profile);state.assetId=asset.asset_id;state.spokenLanguage=asset.spoken_language||null;state.lease=null;state.etag=null;state.error=null;
  state.task=task;state.outputLanguage=outputLanguage;
  const taskSelect=state.panel.querySelector("[data-translation-task]"),profileSelect=state.panel.querySelector("[data-translation-profile]"),rows=translationTaskProfiles(state.options,task);if(taskSelect)taskSelect.value=task;if(profileSelect){profileSelect.innerHTML=rows.map(row=>`<option value="${esc(row.profile_id)}">${esc(row.label||row.model_id)}</option>`).join("");profileSelect.value=profile.profile_id;}const heading=state.panel.querySelector("[data-translation-heading]"),start=state.panel.querySelector("[data-translation-start]");if(heading)heading.textContent=translationTaskLabel(task);if(start)start.textContent=translationTaskLabel(task);
  const languageSelect=state.panel.querySelector("[data-translation-language]");if(languageSelect){languageSelect.innerHTML=`<option value="">Choose spoken language</option>`+profile.input_languages.map(code=>`<option value="${esc(code)}"${code===spokenLanguage?" selected":""}>${esc(typeof subtitleLanguageName==="function"?subtitleLanguageName(code):code)}</option>`).join("");languageSelect.value=spokenLanguage;}
  if(translationId(asset.operation_id))state.operation={operation_id:asset.operation_id,operation_epoch:asset.operation_epoch||"0",view_revision:"0"};
  await translationPoll(state,true);
}
function translationLeaseCurrent(state){return !!state.lease&&performance.now()<state.lease.deadline;}
function translationAcceptLease(state,data,response,started){
  const snapshot=data?.snapshot;if(!snapshot||!translationId(snapshot.lease_id)||!translationCounter(snapshot.revision)||snapshot.revision!==data.manifest_revision||!Number.isSafeInteger(snapshot.expires_in_ms)||snapshot.expires_in_ms<=0)throw Error("Invalid caption read lease");
  let budget=snapshot.expires_in_ms;
  const now=response.headers.get("X-Translation-Server-Now-Ms"),expires=response.headers.get("X-Translation-Read-Lease-Expires-At-Ms");
  if(now!=null&&expires!=null&&Number.isSafeInteger(Number(now))&&Number.isSafeInteger(Number(expires)))budget=Math.min(budget,Number(expires)-Number(now));
  const remaining=budget-Math.max(0,performance.now()-started);if(remaining<=0)throw Error(translationFailure("translation_snapshot_expired"));
  state.lease={id:snapshot.lease_id,revision:snapshot.revision,deadline:performance.now()+remaining};
}
function translationValidateRanges(rows,max,duration){
  if(!Array.isArray(rows)||rows.length>max)throw Error("Caption coverage exceeds its budget");
  for(const row of rows)if(!translationMs(row.start_ms)||!translationMs(row.end_ms)||row.start_ms>=row.end_ms||row.end_ms>duration)throw Error("Invalid caption source range");
  return rows;
}
async function translationPages(state,kind,first,cursor,lease,windowGeneration){
  if(!Array.isArray(first)||first.length>state.limits.page_entries)throw Error("Caption page exceeds its budget");
  const result=first.slice(),seen=new Set();
  while(cursor){
    if(typeof cursor!=="string"||cursor.length>1024||seen.has(cursor)||!translationLeaseCurrent(state))throw Error(translationFailure("translation_snapshot_expired"));seen.add(cursor);
    // Only the bounded presentation window may be materialized, never a film index.
    const maximum=kind==="segments"?state.limits.cue_count:state.limits.coverage_intervals;
    if(result.length>=maximum)throw Error("Caption window exceeds its budget");
    const {data}=await translationRequest(state,`/subtitle-translation/assets/${state.assetId}/${kind}?cursor=${encodeURIComponent(cursor)}`,{headers:{"X-Translation-Read-Lease":lease.id},max:state.limits.page_bytes});
    if(!state.current()||state.windowGeneration!==windowGeneration||state.lease!==lease||!translationLeaseCurrent(state))return null;
    if(!translationClosed(data,["version","asset_id","snapshot_revision","entries","next_cursor"])||data.version!==1||data.asset_id!==state.assetId||data.snapshot_revision!==lease.revision||!Array.isArray(data.entries)||data.entries.length>state.limits.page_entries)throw Error("Caption page belongs to another snapshot");
    result.push(...data.entries);if(result.length>maximum)throw Error("Caption window exceeds its budget");cursor=data.next_cursor;
  }
  return result;
}
async function translationManifest(state,force){
  const windowGeneration=state.windowGeneration,position=translationPosition(state),headers={};
  if(state.lease&&translationLeaseCurrent(state))headers["X-Translation-Read-Lease"]=state.lease.id;
  if(!force&&state.etag&&translationLeaseCurrent(state)&&state.manifestWindow===windowGeneration)headers["If-None-Match"]=state.etag;
  const renew=state.lease&&!translationLeaseCurrent(state)?"&renew_lease=1":"";
  const {data,response,started}=await translationRequest(state,`/subtitle-translation/assets/${state.assetId}/manifest?position_ms=${position}${renew}`,{headers,max:state.limits.page_bytes});
  if(!state.current()||state.windowGeneration!==windowGeneration)return;
  if(response.status===304){if(!translationLeaseCurrent(state)){state.etag=null;state.lease=null;}return;}
  if(data?.version!==1||data.asset_id!==state.assetId||data.file_id!==state.fileId||data.profile_id!==state.profile.profile_id||data.audio_index!==state.audioIndex||!translationDigest(data.content_revision)||!translationCounter(data.manifest_revision)||!translationCounter(data.view_revision)||!data.operation||!translationCounter(data.operation.epoch))throw Error("Caption manifest identity changed");
  if(state.operation&&data.operation.operation_id!==state.operation.operation_id)throw Error("Caption manifest belongs to another operation");
  const task=translationTask(state.profile),outputLanguage=translationOutputLanguage(state.profile,data.spoken_language);
  if(data.task!==task||data.output_language!==outputLanguage||data.timeline_contract!==state.profile.timeline_contract||state.spokenLanguage&&data.spoken_language!==state.spokenLanguage||!state.profile.input_languages.includes(data.spoken_language)||state.operation?.content_revision&&state.operation.content_revision!==data.content_revision)throw Error("Caption manifest source or language changed");
  if(state.operation&&translationCompare(data.operation.epoch,state.operation.operation_epoch)<0)return;
  translationAcceptLease(state,data,response,started);const lease=state.lease;
  const segments=await translationPages(state,"segments",data.segments,data.segment_next_cursor,lease,windowGeneration);if(!segments)return;
  const coverage=await translationPages(state,"coverage",data.coverage,data.coverage_next_cursor,lease,windowGeneration);if(!coverage)return;
  translationValidateRanges(coverage,state.limits.coverage_intervals,state.options.duration_ms);
  translationValidateRanges(segments,state.limits.cue_count,state.options.duration_ms);
  for(const segment of segments)if(!translationDigest(segment.segment_id)||!translationDigest(segment.digest))throw Error("Invalid caption segment reference");
  if(!state.current()||state.windowGeneration!==windowGeneration||state.lease!==lease||!translationLeaseCurrent(state))return;
  // Stop/Retry/status can settle while snapshot pages are in flight. A valid
  // old lease does not grant authority to roll the operation view backward.
  if(state.operation&&(state.operation.operation_id!==data.operation.operation_id||translationCompare(data.operation.epoch,state.operation.operation_epoch)<0||data.operation.epoch===state.operation.operation_epoch&&translationCompare(data.view_revision,state.operation.view_revision)<0))return;
  state.presentationValidated=true;state.coverage=coverage;state.spokenLanguage=data.spoken_language;state.outputLanguage=outputLanguage;state.task=task;state.contentRevision=data.content_revision;state.etag=response.headers.get("ETag");state.manifestWindow=windowGeneration;
  state.operation={...state.operation,operation_id:data.operation.operation_id,operation_epoch:data.operation.epoch,view_revision:data.view_revision,state:data.operation.state,reason:data.operation.reason};state.stopped=data.operation.state==="stopped";if(state.stopped)state.demand=null;
  const low=Math.max(0,position-state.limits.cue_window_before_ms),high=position+state.limits.cue_window_after_ms;
  translationPrune(state);
  const needed=segments.filter(segment=>segment.end_ms>low&&segment.start_ms<high);
  let cursor=0;
  const workers=Array.from({length:Math.min(state.limits.concurrent_fetches,needed.length)},async()=>{
    while(cursor<needed.length){const segment=needed[cursor++];if(state.segments.get(segment.segment_id)?.digest===segment.digest)continue;await translationSegment(state,segment,lease,windowGeneration);}
  });
  await Promise.all(workers);
  if(!state.current()||state.windowGeneration!==windowGeneration)return;
  translationPrune(state);translationApplyCues(state);translationRenderStatus(state);
}
async function translationSegment(state,reference,lease,windowGeneration){
  const {bytes}=state.segmentRead?await state.segmentRead(reference,lease):await translationRequest(state,`/subtitle-translation/assets/${state.assetId}/segments/${reference.segment_id}`,{headers:{"X-Translation-Read-Lease":lease.id},max:state.limits.segment_bytes,raw:true});
  const digest=Array.from(new Uint8Array(await cinemaDiagnosticSHA256(bytes)),byte=>byte.toString(16).padStart(2,"0")).join("");
  if(!state.current()||state.windowGeneration!==windowGeneration||state.lease!==lease||!translationLeaseCurrent(state))return;
  if(digest!==reference.digest)throw Error("Caption segment digest mismatch");
  // Parse only after the exact fetched bytes have passed the digest boundary.
  const segment=JSON.parse(new TextDecoder("utf-8",{fatal:true}).decode(bytes));
  if(!translationClosed(segment,["asset_id","core_index","cues","end_ms","start_ms","provenance","segment_id","version"])||segment.version!==1||segment.asset_id!==state.assetId||segment.segment_id!==reference.segment_id||segment.start_ms!==reference.start_ms||segment.end_ms!==reference.end_ms||!Number.isSafeInteger(segment.core_index)||segment.core_index<0||!Array.isArray(segment.cues)||segment.cues.length>state.limits.segment_cues)throw Error("Invalid caption segment shape");
  const provenance=segment.provenance;
  if(!translationClosed(provenance,["engine_sha256","model_sha256","operation_epoch","platform","profile_id","source_sha256","worker_fence"])||provenance.profile_id!==state.profile.profile_id||provenance.source_sha256!==state.contentRevision||provenance.model_sha256!==state.profile.model_sha256||!translationDigest(provenance.engine_sha256)||!translationCounter(provenance.operation_epoch)||!translationCounter(provenance.worker_fence))throw Error("Caption provenance changed");
  const ids=new Set();let bytesUsed=0;
  for(const cue of segment.cues){
    if(!translationClosed(cue,["cue_id","start_ms","end_ms","text"])||!translationDigest(cue.cue_id)||ids.has(cue.cue_id)||!translationMs(cue.start_ms)||!translationMs(cue.end_ms)||cue.start_ms<segment.start_ms||cue.end_ms>segment.end_ms||cue.end_ms<=cue.start_ms||typeof cue.text!=="string"||/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/.test(cue.text))throw Error("Invalid translated cue");
    ids.add(cue.cue_id);bytesUsed+=new TextEncoder().encode(cue.text).byteLength;
  }
  if(bytesUsed>state.limits.cue_memory_bytes)throw Error("Caption cue memory exceeds its budget");
  let totalBytes=bytesUsed,totalCues=segment.cues.length;
  for(const [id,existing]of state.segments)if(id!==reference.segment_id)for(const cue of existing.cues){totalCues++;totalBytes+=new TextEncoder().encode(cue.text).byteLength;}
  if(totalBytes>state.limits.cue_memory_bytes||totalCues>state.limits.cue_count)throw Error("Caption presentation exceeds its budget");
  state.segments.set(reference.segment_id,{...reference,cues:segment.cues});
}
function translationPrune(state){
  const position=translationPosition(state),low=Math.max(0,position-state.limits.cue_window_before_ms),high=position+state.limits.cue_window_after_ms;
  for(const [id,segment]of state.segments)if(segment.end_ms<=low||segment.start_ms>=high)state.segments.delete(id);
  const cues=new Map();let bytes=0;
  for(const segment of state.segments.values())for(const cue of segment.cues){if(cue.end_ms<=low||cue.start_ms>=high)continue;const previous=cues.get(cue.cue_id);if(previous&&JSON.stringify(previous)!==JSON.stringify(cue))throw Error("Conflicting translated cue identity");if(previous)continue;cues.set(cue.cue_id,cue);bytes+=new TextEncoder().encode(cue.text).byteLength;if(cues.size>state.limits.cue_count||bytes>state.limits.cue_memory_bytes)throw Error("Caption presentation exceeds its budget");}
  state.cues=cues;
}
function translationApplyCues(state){
  if(!state.current()||state.positionReady===false||state.rebinding||state.presentationValidated===false||!translationSelected(state.player)||state.player.captionIntent.asset_id!==state.assetId)return;
  const video=state.video,outputLanguage=state.outputLanguage||translationOutputLanguage(state.profile,state.spokenLanguage),task=state.task||translationTask(state.profile);if(!video._translationTrack||video._translationTrackTask!==task||video._translationTrackLanguage!==outputLanguage){if(video._translationTrack)video._translationTrack.mode="disabled";video._translationTrack=video.addTextTrack("subtitles",`${task==="transcribe"?"AI captions":"AI translation"} · ${outputLanguage}`,outputLanguage);video._translationTrackTask=task;video._translationTrackLanguage=outputLanguage;}
  const track=video._translationTrack,Cue=window.VTTCue||window.TextTrackCue,desired=new Map();
  for(const cue of state.cues.values()){
    const start=(cue.start_ms/1000)-(state.player.offset||0),end=(cue.end_ms/1000)-(state.player.offset||0);if(end<=0)continue;
    desired.set(cue.cue_id,{start:Math.max(0,start),end,text:translationLiteral(cue.text)});
  }
  // Disabled tracks hide their retained cue list until reactivated.
  if(track.mode==="disabled")track.mode="hidden";
  // Keep unchanged active cues attached across manifest refreshes. An offset
  // change or a pruned window replaces only the affected cue identities.
  for(const rendered of Array.from(track.cues||[])){
    const cue=desired.get(rendered.id);
    if(cue&&rendered.startTime===cue.start&&rendered.endTime===cue.end&&rendered.text===cue.text)desired.delete(rendered.id);
    else track.removeCue(rendered);
  }
  for(const [id,cue]of desired){const rendered=new Cue(cue.start,cue.end,cue.text);rendered.id=id;track.addCue(rendered);}
  track.mode="showing";
}
async function translationUse(state){
  if(!state.current()||PLAYER!==state.player||state.rebinding||state.positionReady===false||!state.assetId||!state.coverage.length)return;
  const reason=translationDirectReason(state.player,state.video);if(reason){state.error=translationFailure(reason);translationRenderStatus(state);return;}
  // The source owner disables old native/script captions first. Its serial
  // also makes A→translation→A reject the first A's late response.
  const expected=(state.player.captionIntentSerial||0)+1;
  await setSub(-1);
  if(!state.current()||state.player.captionIntentSerial!==expected)return;
  state.player.captionIntent={kind:"translation",asset_id:state.assetId};
  translationApplyCues(state);pbSyncSubIcon();if(typeof watchRenderLedger==="function")watchRenderLedger();translationRenderStatus(state);
}
function translationMediaEvent(state,kind){
  if(!state.current()){if(state.handoffTarget||translationOwnsPlayer(state))return;translationRetire("playback changed");return;}
  if(state.rebinding||state.positionReady===false)return;
  if(kind==="seeking"){
    const pending=state.player.controlSeek;
    if(pending?.captionViewerSeek&&pending.executed)translationPlaybackSeek(state.player,pending);
    else translationSeekWindow(state);
  }
  if(kind!=="timeupdate"){
    if(state.demand&&!state.stopped)void translationHeartbeat(state).catch(()=>{});
    if(state.assetId||state.operation)void translationPoll(state,true);
  }
  translationRenderStatus(state);
}
function translationRenewContinuity(state,sent){
  if(state.demand&&sent.continuity_id!==translationContinuity(state)&&!state.heartbeatTask)void translationHeartbeat(state).catch(()=>{});
  translationArmHeartbeat(state);
}
async function translationHeartbeat(state){
  if(!state.current()||state.rebinding||state.positionReady===false||!ME?.is_admin||!state.options.permissions?.demand||!state.demand||state.stopped||state.pendingStop||state.stopIntent)return;
  if(state.heartbeatTask)return state.heartbeatTask;
  let acknowledged=false,sentContinuity=null;
  const task=state.queue.catch(()=>{}).then(async()=>{
    if(!state.current()||state.rebinding||state.positionReady===false||!state.demand||state.stopped||state.pendingStop||state.stopIntent)return;
    const demand=state.demand;
    try{
      const body=demand.intent||(demand.intent={...translationEnvelope(state),expected_operation_epoch:demand.epoch,demand_generation:translationNext(demand.generation),playback_id:state.playbackId,continuity_id:translationContinuity(state),position_ms:translationPosition(state),playback_rate:state.video.playbackRate,paused:state.video.paused});
      sentContinuity=body.continuity_id;
      const {data}=await translationRequest(state,`/subtitle-translation/operations/${state.operation.operation_id}/demands/${demand.id}`,{method:"PUT",body});
      if(state.current()&&state.demand===demand&&data.operation_epoch===demand.epoch){
        if(data.demand_id!==demand.id||data.demand_generation!==body.demand_generation)throw Error("Caption receipt demand does not match its request");
        acknowledged=translationAdoptReceipt(state,data,body);demand.intent=null;state.lastHeartbeat=performance.now();
      }
    }catch(error){if(state.current()&&state.demand===demand){
      if(!translationAmbiguous(error))demand.intent=null;
      if(error.code==="operation_stopped"){state.demand=null;state.stopped=true;}
      else if(error.code==="epoch_conflict"||error.code==="demand_conflict"){state.demand=null;void translationPoll(state,true);}
      if(error.status===401||error.status===403){translationRetire("access ended");return;}
      state.error=error.code?translationFailure(error.code):error.message;translationRenderStatus(state);
    }}
  });
  state.queue=task;state.heartbeatTask=task;
  try{await task;}finally{
    if(state.heartbeatTask===task)state.heartbeatTask=null;
    // A seek during an in-flight/uncertain request keeps the local new token.
    // First acknowledge the old CAS generation, then send one latest renewal.
    if(acknowledged&&state.demand&&sentContinuity!==translationContinuity(state))void translationHeartbeat(state).catch(()=>{});
  }
}
// The demand has its own cadence: read failures back off reads only, so a
// slow manifest page or a 503 cannot let a watched operation's demand lapse.
function translationArmHeartbeat(state){
  if(!state.current()||state.heartbeatTimer||!state.demand||state.stopped||!state.limits)return;
  const now=performance.now(),due=Math.max((state.lastHeartbeat||0)+state.limits.heartbeat_interval_ms,(state.heartbeatAttempt||0)+1000);
  state.heartbeatTimer=setTimeout(async()=>{state.heartbeatTimer=null;state.heartbeatAttempt=performance.now();try{await translationHeartbeat(state);}finally{translationArmHeartbeat(state);}},Math.max(0,due-now));
}
async function translationRelease(state){
  if(!state.current()||!ME?.is_admin||!state.demand)return;
  const demand=state.demand;state.demand=null;
  const body={...translationEnvelope(state),expected_operation_epoch:demand.epoch,demand_generation:translationNext(demand.generation),playback_id:state.playbackId};
  await translationRequest(state,`/subtitle-translation/operations/${state.operation.operation_id}/demands/${demand.id}`,{method:"DELETE",body});
}
async function translationStop(state){
  if(!state.current()||!ME?.is_admin||!state.options.permissions?.stop||!state.operation||state.pendingStop)return;
  state.pendingStop=true;translationRotateContinuity(state);const generation=translationProducerGeneration(state),operationId=state.operation.operation_id;
  const key=JSON.stringify([state.operation.operation_id,state.operation.operation_epoch]);if(state.stopKey!==key){state.stopKey=key;state.stopIntent=null;}
  try{const body=state.stopIntent||(state.stopIntent={...translationEnvelope(state),expected_operation_epoch:state.operation.operation_epoch});const {data}=await translationRequest(state,`/subtitle-translation/operations/${state.operation.operation_id}/stop`,{method:"POST",body});if(state.current()&&translationProducerGeneration(state)===generation&&state.operation?.operation_id===operationId){if(!translationAdoptReceipt(state,data))return;state.stopped=true;state.demand=null;state.stopIntent=null;state.error=null;translationRenderStatus(state);}}
  catch(error){if(state.current()&&translationProducerGeneration(state)===generation&&state.operation?.operation_id===operationId){translationRejectIntent(state,"stopIntent",error);state.error=error.code?translationFailure(error.code):error.message;translationRenderStatus(state);}}
  finally{state.pendingStop=false;}
}
async function translationResume(state){
  if(!state.current()||!ME?.is_admin||!state.options.permissions?.resume||!state.operation||!["stopped","failed"].includes(state.operation.state))return;
  const operation=state.operation;
  const key=JSON.stringify([operation.operation_id,operation.operation_epoch,operation.view_revision,operation.state]);if(state.resumeKey!==key){state.resumeKey=key;state.resumeIntent=null;}
  try{const body=state.resumeIntent||(state.resumeIntent={...translationEnvelope(state),scope:"operation",expected_operation_epoch:operation.operation_epoch,expected_view_revision:operation.view_revision,expected_state:operation.state,playback_id:state.playbackId,continuity_id:translationContinuity(state),position_ms:translationPosition(state),playback_rate:state.video.playbackRate,paused:state.video.paused});
    await translationRetry(state,body,"resumeIntent");
  }catch(error){if(state.current()){state.error=error.message;translationRenderStatus(state);}}
}
async function translationRetryRange(state,range){
  if(!state.current()||!ME?.is_admin||!state.options.permissions?.demand||state.operation?.state!=="partial"||!range.retryable)return;
  const key=JSON.stringify([state.operation.operation_id,state.operation.operation_epoch,range.core_index,range.range_revision]);if(state.rangeKey!==key){state.rangeKey=key;state.rangeIntent=null;}
  try{const body=state.rangeIntent||(state.rangeIntent={...translationEnvelope(state),scope:"range",expected_operation_epoch:state.operation.operation_epoch,expected_operation_state:"partial",core_index:range.core_index,expected_range_revision:range.range_revision,expected_range_state:"failed",playback_id:state.playbackId,continuity_id:translationContinuity(state),position_ms:translationPosition(state),playback_rate:state.video.playbackRate,paused:state.video.paused});
    await translationRetry(state,body,"rangeIntent");
  }catch(error){if(state.current()){state.error=error.message;translationRenderStatus(state);}}
}
async function translationRetry(state,body,key){
  const generation=translationProducerGeneration(state),operationId=state.operation.operation_id;
  try{const {data}=await translationRequest(state,`/subtitle-translation/operations/${state.operation.operation_id}/retry`,{method:"POST",body});if(!state.current()||translationProducerGeneration(state)!==generation||state.operation?.operation_id!==operationId)return;translationAdoptReceipt(state,data,body);state[key]=null;state.error=null;translationRenewContinuity(state,body);await translationPoll(state,true);}
  catch(error){if(state.current()&&translationProducerGeneration(state)===generation&&state.operation?.operation_id===operationId){translationRejectIntent(state,key,error);state.error=error.code?translationFailure(error.code):error.message;translationRenderStatus(state);}}
}
async function translationPoll(state,force=false){
  if(!state.current()||state.rebinding||state.positionReady===false)return;
  if(!state.operation&&!state.assetId){
    if(!force)return;
    try{if(await translationRefreshOptions(state)){state.error=null;translationRenderStatus(state);}}
    catch(error){if(state.current()){state.error=error.code?translationFailure(error.code):error.message;translationRenderStatus(state);}}
    return;
  }
  if(!state.limits)return;const generation=state.windowGeneration;if(state.polling){state.pollAgain=state.pollAgain||force;return;}
  clearTimeout(state.timer);state.polling=true;
  try{
    if(state.operation){const {data}=await translationRequest(state,`/subtitle-translation/operations/${state.operation.operation_id}`);if(!state.current()||state.windowGeneration!==generation)return;translationValidateOperation(state,data);if(translationCompare(data.operation_epoch,state.operation.operation_epoch)>0||translationCompare(data.operation_epoch,state.operation.operation_epoch)===0&&translationCompare(data.view_revision,state.operation.view_revision)>=0){const page=state.failurePage;if(page&&page.operation_id===data.operation_id&&page.operation_epoch===data.operation_epoch&&page.view_revision===data.view_revision){data.failed_ranges=page.entries;data.failed_ranges_next_cursor=page.next_cursor;}else state.failurePage=null;state.operation=data;state.spokenLanguage=data.spoken_language;state.assetId=data.asset_id;state.stopped=data.state==="stopped";if(state.stopped)state.demand=null;}}
    if(state.assetId)await translationManifest(state,force);
    if(!state.current()||state.windowGeneration!==generation)return;
    state.error=null;
    state.failures=0;translationRenderStatus(state);
  }catch(error){
    if(state.current()&&state.windowGeneration===generation){
      if(error.status===410){state.lease=null;state.etag=null;}
      if(error.code==="source_changed")translationClearCues(state);
      if(error.status===401||error.status===403){translationRetire("access ended");return;}
      state.failures=(state.failures||0)+1;state.error=error.code?translationFailure(error.code):error.message;translationRenderStatus(state);
    }
  }finally{
    state.polling=false;
    if(state.current()){const immediate=state.pollAgain;state.pollAgain=false;const delay=immediate?state.limits.poll_min_ms:Math.min(state.limits.retry_max_ms,state.limits.poll_max_ms*(2**Math.min(state.failures||0,4)));state.timer=setTimeout(()=>translationPoll(state,immediate),delay);translationArmHeartbeat(state);}
  }
}
// Settings uses the same bounded transport but a page/account owner, not PLAYER.
function translationSetupCurrent(setup){return TRANSLATION_SETUP===setup&&translationPageCurrent(setup)&&!!ME?.is_admin&&document.getElementById("subtitle-translation-form")===setup.panel&&setup.panel.isConnected;}
function translationInput(root,selector){return /** @type {HTMLInputElement} */(root.querySelector(selector));}
function translationProfileChoices(profiles,selected){
  const rows=profiles.slice();if(selected&&!rows.some(row=>row.profile_id===selected))rows.unshift({profile_id:selected,model_id:"Saved model",activation_state:"unavailable"});
  return '<option value="">Not configured</option>'+rows.map(row=>`<option value="${esc(row.profile_id)}"${row.profile_id===selected?" selected":""}>${esc(row.label||row.model_id)} · ${translationTaskLabel(translationTask(row))} · ${esc(row.activation_state||"unavailable")}</option>`).join("");
}
function translationSetupChoices(setup){
  const profiles=setup.catalog?.profiles||[],profileInput=translationInput(setup.panel,"[data-translation-setting-profile]"),languageInput=translationInput(setup.panel,"[data-translation-setting-language]");
  let selected=profileInput.value;
  const options={models:profiles,default_profile_id:setup.catalog?.default_profile_id};translationDefaultHint(options);
  if(!setup.choicesLoaded&&!setup.edit&&!setup.saved.profile_id&&!selected){
    const hinted=setup.saved.model_id||options.default_profile_id!=null||profiles.some(profile=>profile.default===true);
    if(hinted)selected=translationPreferredProfile(options,"translate",null,setup.saved.model_id)?.profile_id||"";
    else{const active=profiles.filter(row=>row.activation_state==="active");if(active.length===1)selected=active[0].profile_id;}
  }
  const chosenProfile=profiles.find(row=>row.profile_id===selected),language=languageInput.value,languages=[...new Set((chosenProfile?.input_languages?.length?chosenProfile.input_languages:profiles.flatMap(row=>row.input_languages||[])))],keptLanguage=languages.includes(language)?language:!chosenProfile?language:"";if(keptLanguage&&!languages.includes(keptLanguage))languages.push(keptLanguage);
  profileInput.innerHTML=translationProfileChoices(profiles,selected);profileInput.value=selected;
  languageInput.innerHTML='<option value="">Choose for each movie</option>'+languages.map(code=>`<option value="${esc(code)}"${code===keptLanguage?" selected":""}>${esc(typeof subtitleLanguageName==="function"?subtitleLanguageName(code):code)}</option>`).join("");languageInput.value=keptLanguage;
  const task=translationTask(chosenProfile),test=setup.panel.querySelector("[data-translation-test]");if(test)test.textContent=task==="transcribe"?"Test speech captions":"Test English translation";
  const enabled=setup.panel.querySelector("[data-translation-setting-enabled]");if(enabled){const label=enabled.parentElement;if(label)label.childNodes[1].textContent=" Enable progressive AI captions";}
  if(profiles.length)setup.choicesLoaded=true;
}
function translationSetupReason(code){
  const reasons={output_invalid:"The model test could not prepare or validate captions. Check server storage and logs, then retry Test.",inference_failed:"The model speech test could not run. Retry Test; if it fails again, update or repair this server package.",inference_timeout:"The model speech test exceeded its time limit. Retry when this server has capacity.",decode_failed:"The model speech test could not decode its sample audio. Update or repair this server package, then retry Test.",model_download_failed:"The model download failed. Check this server’s network access, then retry Download and test.",model_verification_failed:"The model failed its integrity or speech test. Retry Download and test.",model_self_test_failed:"The model's caption task test failed. Retry Download and test.",translation_unverified:"Test this model for its selected caption task.",engine_unavailable:"This server’s packaged speech engine is unavailable. Update or repair its server package, then retry.",packaged_engine_unqualified:"This server’s packaged speech engine is unavailable. Update or repair its server package, then retry.",missing_model:"Download and test this model on a compatible server.",model_missing:"Download and test this model on a compatible server.",model_unavailable:"Download and test this model on a compatible server.",insufficient_disk:"Free space on this server, then retry Download and test.",waiting_for_playback_capacity:"Waiting for playback capacity. The test runs when this server has capacity.",worker_interrupted:"The worker stopped. Retry Download and test.",waiting_for_worker:"Waiting for a compatible server.",worker_source_unavailable:"Waiting for a compatible server with source access.",worker_not_current:"This server is no longer current. Refresh worker status and choose a current server.",unavailable_worker:"The server is unavailable. Refresh worker status and retry.",unsupported_worker:"This server cannot run the packaged speech engine. Choose a compatible server.",queue_full:"The background queue is full. Retry after current jobs finish.",quota_exceeded:"The background work limit was reached. Retry after current jobs finish.",conflict:"Another request changed this work. Refresh worker status before retrying.",configuration_conflict:"The configuration changed. Reload saved settings before retrying.",disabled:"Model setup is paused. Check the server and retry.",unknown:"Worker status is unknown. Refresh worker status.",network_error:"Model status is unavailable. Retry status or check Activity."};
  if(!Object.hasOwn(reasons,code))return "Model setup could not finish. Refresh worker status and check Activity before retrying.";
  return reasons[code];
}
function translationSetupNeedsClock(setup,intent){
  return !intent&&(!setup.clock||performance.now()-setup.clock.at>240000);
}
function translationSetupBusy(setup,busy){
  setup.modelBusy=busy;
  for(const [selector,label]of [["[data-translation-install]","Download and test"],["[data-translation-test]","Test English translation"]]){const button=setup.panel.querySelector(selector);if(button){button.disabled=busy;button.textContent=busy&&setup.modelAction===(selector.includes("install")?"install":"verify")?"Working…":label;}}
}
function translationWorkerFresh(setup,worker){
  if(!setup.clock)return false;const now=setup.clock.server+Math.max(0,performance.now()-setup.clock.at);
  return worker.current===true&&Number.isFinite(worker.observed_at_ms)&&worker.observed_at_ms<=now+30000&&Number.isFinite(worker.expires_at_ms)&&worker.expires_at_ms>now;
}
function translationWorkerStatus(setup,worker,modelId,profileId){
  if(!translationWorkerFresh(setup,worker))return "unknown";
  const capability=worker.capabilities?.find(row=>row.model_id===modelId&&row.profile_id===profileId);
  const allowed=["unknown","missing","installed","unverified_cache","installing","verifying","verified","verification_failed","engine_unavailable"];
  return allowed.includes(capability?.status)?capability.status:"unknown";
}
function translationSetupEvidence(setup){
  if(!translationSetupCurrent(setup))return;
  const selected=translationInput(setup.panel,"[data-translation-setting-profile]").value,profile=setup.catalog?.profiles?.find(row=>row.profile_id===selected),model=setup.catalog?.models?.find(row=>row.id===(profile?.model_id||setup.saved.model_id));
  const taskName=translationTask(profile)==="transcribe"?"speech captions":"English translation";
  if(setup.workerProfile!==selected){setup.workerProfile=selected;setup.workerSelectionInitialized=false;}
  const old=new Set([...setup.panel.querySelectorAll('[name="translation-worker"]:checked')].map(node=>node.value));
  const evidence=setup.panel.querySelector("[data-translation-workers]");if(!evidence)return;
  const label={unknown:"Readiness unknown",missing:"Model missing",unverified_cache:"Download cached · Install or Test to finish",installing:"Installing",verifying:`Testing ${taskName}`,installed:"Downloaded · Test to finish",verified:`Verified for ${taskName}`,verification_failed:`${taskName} test failed`,engine_unavailable:"Packaged engine unavailable"};
  evidence.innerHTML=(model?`<p>${esc(model.label)} · ${esc(typeof fmtSize==="function"?fmtSize(model.download_bytes):String(model.download_bytes)+" bytes")} · ${esc(model.license)}</p>`:"")+(profile&&profile.activation_state!=="active"?'<p class="hint">This speech model profile is not yet available for playback. Saved enablement remains editable.</p>':"")+(setup.catalog?.workers||[]).map(worker=>{
    const fresh=translationWorkerFresh(setup,worker),compatible=!!profile&&worker.eligible&&fresh&&worker.engine?.adapter_contract===profile?.adapter_contract&&translationDigest(worker.engine?.binary_sha256),status=translationWorkerStatus(setup,worker,profile?.model_id,selected);
    const checked=setup.workerSelectionInitialized?old.has(worker.node_id):compatible;
    const capability=worker.capabilities?.find(row=>row.model_id===profile?.model_id&&row.profile_id===selected),reason=!fresh?"unknown":!worker.eligible?"unsupported_worker":!worker.engine?"engine_unavailable":profile&&!compatible?"engine_unavailable":capability?.reason;
    return `<p><label class="chk"><input name="translation-worker" type="checkbox" value="${esc(worker.node_id)}"${compatible?checked?" checked":"":" disabled"}> ${esc(worker.label||worker.node_id)}</label> · ${esc(label[status])}${reason?`<br><span class="hint">${esc(translationSetupReason(reason))}</span>`:""}</p>`;
  }).join("")||"No current worker observations are available.";
  setup.workerSelectionInitialized=true;
}
function translationSetupChanged(){const setup=TRANSLATION_SETUP;if(!translationSetupCurrent(setup))return;setup.edit++;translationSetupEvidence(setup);}
async function openTranslationSettings(){
  const panel=document.getElementById("subtitle-translation-form");if(!panel||!ME?.is_admin)return;
  if(TRANSLATION_SETUP?.panel===panel&&translationSetupCurrent(TRANSLATION_SETUP))return;
  if(TRANSLATION_SETUP){TRANSLATION_SETUP.retired=true;clearTimeout(TRANSLATION_SETUP.timer);for(const controller of TRANSLATION_SETUP.controllers)controller.abort();}
  const setup={capture:translationCapture(),panel,controllers:new Set(),edit:0,retired:false,rows:[],catalog:null,saved:null,operation:null,timer:null};setup.current=()=>translationSetupCurrent(setup);TRANSLATION_SETUP=setup;panel.textContent="Loading translation settings…";
  try{
    const {data:saved}=await translationRequest(setup,"/subtitle-translation");if(!setup.current())return;setup.saved=saved;
    try{setup.catalog=(await translationRequest(setup,"/subtitle-translation/models")).data;}catch(error){if(!setup.current())return;setup.catalogError=error.message;}
    if(!setup.current())return;
    const profiles=setup.catalog?.profiles||[],languages=[...new Set(profiles.flatMap(profile=>profile.input_languages||[]))];if(saved.preferred_spoken_language&&!languages.includes(saved.preferred_spoken_language))languages.push(saved.preferred_spoken_language);
    panel.innerHTML=`<form data-translation-settings><label class="chk"><input type="checkbox" data-translation-setting-enabled${saved.enabled?" checked":""}> Enable progressive AI captions</label><p class="hint">1. Download and test the selected speech model. 2. Enable captions and Save. Then choose a caption task in the player.</p><label>Speech model <select data-translation-setting-profile>${translationProfileChoices(profiles,saved.profile_id)}</select></label><label>Preferred spoken language <select data-translation-setting-language><option value="">Choose for each movie</option>${languages.map(code=>`<option value="${esc(code)}"${code===saved.preferred_spoken_language?" selected":""}>${esc(typeof subtitleLanguageName==="function"?subtitleLanguageName(code):code)}</option>`).join("")}</select></label><div data-translation-workers></div><button type="button" class="sm" data-translation-install>Download and test</button> <button type="button" class="ghost sm" data-translation-test>Test speech model</button> <button type="button" class="ghost sm" data-translation-worker-refresh>Refresh worker status</button><p data-translation-model-status role="status" aria-live="polite"></p><button type="submit" class="sm">Save</button><p data-translation-save-status role="status" aria-live="polite"></p></form>`;
    for(const input of panel.querySelectorAll("[data-translation-settings] input,[data-translation-settings] select"))input.addEventListener("change",()=>{if(input===panel.querySelector("[data-translation-setting-profile]"))translationSetupChoices(setup);translationSetupChanged();});
    panel.querySelector("form").addEventListener("submit",event=>{event.preventDefault();void saveTranslationSettings();});
    panel.querySelector("[data-translation-install]").addEventListener("click",()=>translationSetupModel("install"));
    panel.querySelector("[data-translation-test]").addEventListener("click",()=>translationSetupModel("verify"));
    panel.querySelector("[data-translation-worker-refresh]").addEventListener("click",()=>translationRefreshSetup());
    translationSetupChoices(setup);translationSetupEvidence(setup);if(setup.catalogError)panel.querySelector("[data-translation-model-status]").textContent="Worker observations unavailable. Saved choices remain editable.";
  }catch(error){if(setup.current())panel.textContent=error.message;}
}
async function saveTranslationSettings(){
  const setup=TRANSLATION_SETUP;if(!translationSetupCurrent(setup))return;
  const profileId=translationInput(setup.panel,"[data-translation-setting-profile]").value||null,profile=setup.catalog?.profiles?.find(row=>row.profile_id===profileId),language=translationInput(setup.panel,"[data-translation-setting-language]").value||null;
  const desired={enabled:translationInput(setup.panel,"[data-translation-setting-enabled]").checked,model_id:profile?.model_id||(profileId===setup.saved.profile_id?setup.saved.model_id:null),profile_id:profileId,preferred_spoken_language:language};
  const key=JSON.stringify(desired);if(setup.saveKey!==key){setup.saveIntent=null;setup.saveKey=key;}
  const request=setup.saveRequest={};const status=setup.panel.querySelector("[data-translation-save-status]");status.textContent="Saving…";
  try{
    if(translationSetupNeedsClock(setup,setup.saveIntent)&&!await translationRefreshSetup())throw Error("Worker status is unavailable. Refresh worker status before saving.");
    if(!setup.current()||setup.saveRequest!==request)return;
    const body=setup.saveIntent||(setup.saveIntent={...translationEnvelope(setup),expected_revision:setup.saved.revision,...desired});
    const {data}=await translationRequest(setup,"/subtitle-translation",{method:"PUT",body});
    if(!setup.current()||setup.saveRequest!==request)return;
    setup.saved=data;setup.saveIntent=null;status.textContent="Saved. Current unsaved controls were kept.";
  }catch(error){if(setup.current()&&setup.saveRequest===request){translationRejectIntent(setup,"saveIntent",error);status.textContent=error.status===409?"Settings changed elsewhere. Reload saved settings before saving; your controls were kept.":error.message;if(error.status===409){const reload=document.createElement("button");reload.type="button";reload.className="ghost sm";reload.textContent="Reload saved settings";reload.addEventListener("click",()=>{TRANSLATION_SETUP=null;void openTranslationSettings();});status.appendChild(reload);}}}
}
async function translationRefreshSetup(){
  const setup=TRANSLATION_SETUP;if(!translationSetupCurrent(setup))return false;
  const request=setup.refresh={},operation=setup.operation;
  try{const {data}=await translationRequest(setup,"/subtitle-translation/models");if(!setup.current()||setup.refresh!==request||setup.operation!==operation)return false;setup.catalog=data;translationSetupChoices(setup);translationSetupEvidence(setup);return true;}
  catch(error){if(setup.current()&&setup.refresh===request&&setup.operation===operation)setup.panel.querySelector("[data-translation-model-status]").textContent="Worker status unavailable. Refresh to retry; saved controls remain editable.";return false;}
}
function translationSetupModelRows(setup){
  const status=setup.panel.querySelector("[data-translation-model-status]");status.replaceChildren();const profileId=setup.rows?.[0]?.profile_id||translationInput(setup.panel,"[data-translation-setting-profile]").value,profile=setup.catalog?.profiles?.find(item=>item.profile_id===profileId),task=translationTask(profile),testLabel=task==="transcribe"?"speech test":"caption translation test";
  const phases={queued:"Queued",running:"Running model test",waiting_for_capacity:"Waiting for server capacity",downloading:"Downloading model",verifying:`Testing ${testLabel}`,decoding:"Decoding test audio",transcribing:"Running speech test",validating:"Validating",publishing:"Publishing",succeeded:"Download and test complete",failed:"Failed",cancelled:"Cancelled"};
  for(const row of setup.rows){const paragraph=document.createElement("p"),worker=setup.catalog?.workers?.find(worker=>worker.node_id===row.node_id),phase=row.job?.phase||row.job?.state,reason=row.job?.reason||row.reason||(!["accepted","existing"].includes(row.outcome)?row.outcome:null);paragraph.textContent=`${worker?.label||row.node_id}: ${phases[phase]||phases[row.job?.state]||(["accepted","existing"].includes(row.outcome)?"Queued":"Not accepted")}. ${reason?translationSetupReason(reason)+" ":""}`;if(translationId(row.job_id)){const link=document.createElement("a");link.href="#/activity/jobs/"+row.job_id;link.textContent="Follow this job in Activity";paragraph.appendChild(link);}status.appendChild(paragraph);}
}
async function translationSetupModel(action){
  const setup=TRANSLATION_SETUP;if(!translationSetupCurrent(setup)||setup.modelBusy||!["install","verify"].includes(action))return;
  const profileId=translationInput(setup.panel,"[data-translation-setting-profile]").value,profile=setup.catalog?.profiles?.find(row=>row.profile_id===profileId),nodeIds=[...setup.panel.querySelectorAll('[name="translation-worker"]:checked')].filter(node=>!node.disabled).map(node=>node.value);
  const status=setup.panel.querySelector("[data-translation-model-status]");
  if(!profile||!translationProfileLimits(profile)||!nodeIds.length||nodeIds.length>16){status.textContent="Choose a model and current named worker.";return;}
  const key=JSON.stringify([action,profile.model_id,profileId,nodeIds]);if(setup.modelKey!==key){setup.modelIntent=null;setup.modelKey=key;}
  const operation=setup.operation={};clearTimeout(setup.timer);setup.modelAction=action;translationSetupBusy(setup,true);status.textContent=action==="install"?"Requesting model download and test…":`Requesting ${translationTask(profile)==="transcribe"?"speech caption":"caption translation"} test…`;
  try{
    if(translationSetupNeedsClock(setup,setup.modelIntent)&&!await translationRefreshSetup())throw Error("Worker status is unavailable. Refresh worker status before downloading or testing.");
    if(!setup.current()||setup.operation!==operation)return;
    const body=setup.modelIntent||(setup.modelIntent={...translationEnvelope(setup),version:2,purpose:"translation",profile_id:profileId,node_ids:nodeIds});
    const {data}=await translationRequest(setup,`/subtitle-transcription/models/${encodeURIComponent(profile.model_id)}/${action}`,{method:"POST",body});if(!setup.current()||setup.operation!==operation)return;
    if(data.version!==2||data.purpose!=="translation"||data.profile_id!==profileId||data.model_id!==profile.model_id||!Array.isArray(data.operations)||data.operations.length>16)throw Error("Invalid model operation response");
    setup.rows=data.operations.map(row=>({...row,model_id:profile.model_id,profile_id:profileId}));setup.modelIntent=null;translationSetupModelRows(setup);void translationSetupPoll(setup,operation,profile);
  }catch(error){if(setup.current()&&setup.operation===operation){translationRejectIntent(setup,"modelIntent",error);translationSetupBusy(setup,false);status.textContent=error.message;}}
}
async function translationSetupPoll(setup,operation,profile){
  if(!setup.current()||setup.operation!==operation)return;
  let again=false,updating=false;
  try{
    for(const row of setup.rows){if(!["accepted","existing"].includes(row.outcome)||!translationId(row.job_id))continue;
      const {data}=await translationRequest(setup,`/subtitle-transcription/jobs/${row.job_id}`);if(!setup.current()||setup.operation!==operation)return;row.job=data;if(!["succeeded","failed","cancelled"].includes(data.state))again=true;
    }
    translationSetupModelRows(setup);await translationRefreshSetup();if(!setup.current()||setup.operation!==operation)return;
    for(const row of setup.rows){if(row.job?.state!=="succeeded")continue;const worker=setup.catalog?.workers?.find(worker=>worker.node_id===row.node_id);if(worker&&translationWorkerStatus(setup,worker,row.model_id,row.profile_id)==="verified")continue;
      row.deadline??=performance.now()+profile.limits.retry_max_ms;if(performance.now()<row.deadline)again=true;updating=true;
    }
    translationSetupBusy(setup,again);
    if(updating){const status=setup.panel.querySelector("[data-translation-model-status]"),paragraph=document.createElement("p"),retry=document.createElement("button");paragraph.textContent="Translation verification status is updating. ";retry.type="button";retry.textContent="Refresh verification status";retry.addEventListener("click",()=>{for(const row of setup.rows)row.deadline=null;void translationSetupPoll(setup,operation,profile);});paragraph.appendChild(retry);status.appendChild(paragraph);}
  }catch(error){if(setup.current()&&setup.operation===operation){translationSetupBusy(setup,setup.rows.some(row=>["accepted","existing"].includes(row.outcome)&&!["succeeded","failed","cancelled"].includes(row.job?.state)));translationSetupModelRows(setup);const status=setup.panel.querySelector("[data-translation-model-status]"),message=document.createElement("p");message.textContent="Model status is unavailable. Retry status or check Activity to resolve the current job.";status.appendChild(message);const retry=document.createElement("button");retry.type="button";retry.textContent="Retry model status";retry.addEventListener("click",()=>translationSetupPoll(setup,operation,profile));setup.panel.querySelector("[data-translation-model-status]").appendChild(retry);}}
  if(again&&setup.current()&&setup.operation===operation)setup.timer=setTimeout(()=>translationSetupPoll(setup,operation,profile),profile.limits.poll_max_ms);
}
function translationReset(){
  translationRetire("account ended");const setup=TRANSLATION_SETUP;if(setup){setup.retired=true;clearTimeout(setup.timer);for(const controller of setup.controllers)controller.abort();TRANSLATION_SETUP=null;}
}
function translationMenuChoice(){
  const state=SUBTITLE_TRANSLATION;if(!translationCurrent(state)||!state.assetId||!state.coverage.length)return "";
  const output=state.outputLanguage||translationOutputLanguage(state.profile,state.spokenLanguage),task=state.task||translationTask(state.profile),name=typeof subtitleLanguageName==="function"?subtitleLanguageName(output):output;
  return `<button class="${translationSelected(state.player)&&state.player.captionIntent.asset_id===state.assetId?"sel":""}" onclick="translationUse(SUBTITLE_TRANSLATION)">${esc(name)} · AI ${task==="transcribe"?"captions":"translation"} · ${state.operation?.state==="complete"?"complete":"partial"}</button>`;
}
function translationWatchChanged(){
  const state=SUBTITLE_TRANSLATION;if(!state||translationCurrent(state)||state.handoffTarget&&translationSameSourceAudio(state,state.handoffTarget))return;
  translationRetire("watch source changed");const panel=document.getElementById("watch-subtitle-translation");if(panel===state.panel){panel.hidden=true;panel.replaceChildren();}
}
