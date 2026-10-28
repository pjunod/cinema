"use strict";
// Grant-scoped readers feed the existing player's caption intent and text renderer.
let SHARED_CAPTION_READER=null;
const SHARED_CAPTION_LIMITS={segment_bytes:16384,segment_cues:128,page_bytes:65536,page_entries:16,coverage_intervals:32,concurrent_fetches:2,cue_memory_bytes:262144,cue_count:1024,cue_window_before_ms:30000,cue_window_after_ms:90000,poll_min_ms:3000,poll_max_ms:30000,retry_max_ms:30000,heartbeat_interval_ms:5000};
function sharedCaptionContext(player){try{const value=playbackFileContextForPlayer(player);return value?.source_ref&&value.source_ref.kind!=="local"?value:null;}catch(_){return null;}}
function sharedCaptionSourceKey(player){const context=sharedCaptionContext(player);return context?playbackFileKey(context):null;}
function sharedCaptionOwnerCurrent(state){return translationPageCurrent(state)&&sharedCaptionSourceKey(state.player)===state.sourceKey&&translationAttachedAudioOrdinal(state.player)===state.audioIndex&&(PLAYER===state.player||PLAYER?.mediaPredecessor===state.player&&!PLAYER.mediaAttachment);}
function sharedCaptionCurrent(state){return sharedCaptionOwnerCurrent(state)&&!state.handoffTarget&&state.player.mediaAttachment===state.attachment&&document.getElementById("video")===state.video;}
function sharedCaptionPath(state,suffix,query={}){if(!sharedCaptionCurrent(state))throw Error("Retired shared caption request");return playbackFileApiPath(playbackFileContextForPlayer(state.player),suffix,query);}
function sharedCaptionStatus(state,message){if(state.current()&&state.panel?.isConnected){const node=state.panel.querySelector("[data-shared-caption-status]");if(node)node.textContent=message;}}
function sharedCaptionCancelRead(state){state.requestGeneration++;state.readOwner=null;state.catalogOwner=null;state.busy=false;for(const controller of state.controllers)controller.abort();state.controllers.clear();}
function sharedCaptionClearPresentation(state,hide=sharedCaptionOwnerCurrent(state)){
 clearTimeout(state.leaseTimer);state.leaseTimer=null;state.lease=null;state.presentationValidated=false;state.coverage=[];state.segments.clear();state.cues.clear();if(hide)translationHidePresentation(state);
 const use=state.panel?.isConnected&&state.panel.querySelector("[data-shared-caption-use]");if(use)use.disabled=true;
}
function sharedCaptionArmLease(state,lease){
 clearTimeout(state.leaseTimer);
 const expire=()=>{
  if(SHARED_CAPTION_READER!==state||state.retired||state.lease!==lease)return;
  if(performance.now()<lease.deadline){state.leaseTimer=setTimeout(expire,Math.max(1,Math.ceil(lease.deadline-performance.now())));return;}
  state.windowGeneration++;sharedCaptionCancelRead(state);sharedCaptionClearPresentation(state);
  sharedCaptionStatus(state,"The shared caption read lease expired. Refreshing this window; playback continues.");sharedCaptionSchedule(state);
 };
 state.leaseTimer=setTimeout(expire,Math.max(1,Math.ceil(lease.deadline-performance.now())));
}
function sharedCaptionRetire(reason="retired"){
 const state=SHARED_CAPTION_READER;if(!state)return;const owned=sharedCaptionOwnerCurrent(state);sharedCaptionClearPresentation(state,owned);state.retired=true;clearTimeout(state.timer);sharedCaptionCancelRead(state);for(const[k,f]of state.listeners)state.video.removeEventListener(k,f);if(owned&&state.player?.captionIntent?.kind==="translation"&&state.player.captionIntent.asset_id===state.assetId){state.player.captionIntent={kind:"off"};state.player.captionIntentSerial=(state.player.captionIntentSerial||0)+1;}state.reason=reason;SHARED_CAPTION_READER=null;if(state.panel?.isConnected){state.panel.hidden=true;state.panel.replaceChildren();}
}
function sharedCaptionAsset(value,audio){
 if(!value||!translationDigest(value.asset_id)||!translationDigest(value.content_revision)||!translationDigest(value.profile_id)||typeof value.model_id!=="string"||value.model_id.length<1||value.model_id.length>64||!translationDigest(value.model_sha256)||!Number.isSafeInteger(value.audio_index)||value.audio_index<0||value.audio_index>4095||!translationMs(value.duration_ms)||value.duration_ms===0||!["translate","transcribe"].includes(value.task)||! /^[a-z]{2,3}$/.test(value.spoken_language)||value.output_language!==(value.task==="transcribe"?value.spoken_language:"en"))throw Error("Invalid shared caption identity");
 return value.audio_index===audio;
}
function sharedCaptionSchedule(state,delay=3000){clearTimeout(state.timer);if(state.current()&&!state.rebinding)state.timer=setTimeout(()=>void sharedCaptionRefresh(state),Math.min(30000,Math.max(3000,delay)));}
function sharedCaptionFailure(state,error){
 if(!state.current())return;
 state.windowGeneration++;sharedCaptionCancelRead(state);sharedCaptionClearPresentation(state);
 if([401,403,404,410].includes(error.status)&&error.code!=="source_verifying"){state.error="These shared captions are no longer available for this file or account.";}
 else state.error=error.code==="source_verifying"?"Checking the Source's media identity. Captions will be retried; playback continues.":"Shared caption status is unavailable. Playback continues; retry status here.";
 sharedCaptionStatus(state,state.error);sharedCaptionSchedule(state,error.code==="source_verifying"?3000:6000);
}
async function openSharedCaptionReader(button){
 const player=PLAYER,video=document.getElementById("video"),context=sharedCaptionContext(player);
 if(!context||!player?.started||!player.mediaAttachment||!video||translationAttachedAudioOrdinal(player)==null){toast("Open a shared title with a confirmed audio track first.");return;}
 if(SHARED_CAPTION_READER?.current()){SHARED_CAPTION_READER.panel.scrollIntoView({block:"nearest"});return;}
 sharedCaptionRetire();const panel=document.getElementById("watch-shared-captions");if(!panel)return;
 const state={capture:translationCapture(),player,video,panel,sourceKey:playbackFileKey(context),audioIndex:translationAttachedAudioOrdinal(player),attachment:player.mediaAttachment,controllers:new Set(),listeners:[],limits:SHARED_CAPTION_LIMITS,assetId:null,profile:null,contentRevision:null,segments:new Map(),cues:new Map(),coverage:[],windowGeneration:0,requestGeneration:0,positionReady:player.mediaAttachment.captionPositionReady!==false,presentationValidated:false,retired:false,busy:false,catalogCursor:null,error:null};
 state.current=()=>sharedCaptionCurrent(state);state.segmentRead=(reference,lease)=>translationRequest(state,sharedCaptionPath(state,`captions/${state.assetId}/segments/${reference.segment_id}`,{lease:lease.id}),{max:16384,raw:true});
 SHARED_CAPTION_READER=state;panel.hidden=false;panel.innerHTML='<div class="card"><h3>Available AI captions from the Source</h3><p>These are committed captions. Reading them never starts or prioritizes inference; ask the Source administrator for missing coverage.</p><div data-shared-caption-choices></div><button class="sm" data-shared-caption-use disabled>Use these captions</button> <button class="ghost sm" data-shared-caption-refresh>Refresh status</button><p data-shared-caption-status role="status" aria-live="polite"></p></div>';
 panel.querySelector("[data-shared-caption-use]").addEventListener("click",()=>void sharedCaptionUse(state));panel.querySelector("[data-shared-caption-refresh]").addEventListener("click",()=>void sharedCaptionRefresh(state));
 for(const kind of ["seeking","seeked","timeupdate","ratechange","pause","play"]){const handler=()=>sharedCaptionMediaEvent(state,kind);video.addEventListener(kind,handler);state.listeners.push([kind,handler]);}
 if(button)button.disabled=true;try{await sharedCaptionCatalog(state,null);}catch(error){sharedCaptionFailure(state,error);}finally{if(button?.isConnected)button.disabled=false;sharedCaptionSchedule(state);}
}
async function sharedCaptionCatalog(state,after){
 if(!state.current()||state.catalogOwner)return;if(after!=null&&!translationDigest(after))throw Error("Invalid shared caption cursor");
 const owner={},generation=state.requestGeneration;state.catalogOwner=owner;
 try{
  const{data}=await translationRequest(state,sharedCaptionPath(state,"captions",after?{after}:{}));if(!state.current()||state.catalogOwner!==owner||generation!==state.requestGeneration)return;
  if(data?.version!==1||!Array.isArray(data.assets)||data.assets.length>16||data.next_asset!=null&&!translationDigest(data.next_asset))throw Error("Invalid shared caption catalog");
  let previous=after||"";for(const asset of data.assets){sharedCaptionAsset(asset,state.audioIndex);if(asset.asset_id<=previous)throw Error("Invalid shared caption catalog order");previous=asset.asset_id;}
  const choices=state.panel.querySelector("[data-shared-caption-choices]");choices.replaceChildren();
  for(const asset of data.assets){if(!sharedCaptionAsset(asset,state.audioIndex))continue;const button=document.createElement("button");button.className="ghost sm";button.textContent="Use "+(asset.task==="transcribe"?"Generated ":"Translated ")+asset.output_language+" captions";button.addEventListener("click",()=>void sharedCaptionChoose(state,asset));choices.appendChild(button);}
  if(data.next_asset){const next=document.createElement("button");next.className="ghost sm";next.textContent="More available caption tracks";next.addEventListener("click",()=>void sharedCaptionCatalog(state,data.next_asset).catch(e=>sharedCaptionFailure(state,e)));choices.appendChild(next);}
  sharedCaptionStatus(state,choices.children.length?"Choose a caption track. Original captions and Off remain available.":"No committed AI captions are available for this audio track yet. Ask the Source administrator.");
 }catch(error){if(state.current()&&state.catalogOwner===owner&&generation===state.requestGeneration)throw error;}
 finally{if(state.catalogOwner===owner)state.catalogOwner=null;}
}
async function sharedCaptionChoose(state,asset){
 if(!state.current()||!sharedCaptionAsset(asset,state.audioIndex))return;
 state.windowGeneration++;sharedCaptionCancelRead(state);sharedCaptionClearPresentation(state);state.profile={profile_id:asset.profile_id,model_id:asset.model_id,model_sha256:asset.model_sha256,task:asset.task};state.spokenLanguage=asset.spoken_language;state.assetId=asset.asset_id;state.contentRevision=asset.content_revision;state.asset=asset;state.segments.clear();state.cues.clear();state.coverage=[];state.lease=null;state.revision=null;state.presentationValidated=false;state.error=null;state.useIntent={assetId:asset.asset_id,serial:state.player.captionIntentSerial||0};
 await sharedCaptionRefresh(state);
}
function sharedCaptionManifest(state,data,started){
 const a=state.asset;
 if(data?.version!==1||data.asset_id!==a.asset_id||data.content_revision!==a.content_revision||data.profile_id!==a.profile_id||data.audio_index!==a.audio_index||data.spoken_language!==a.spoken_language||data.output_language!==a.output_language||data.task!==a.task||data.duration_ms!==a.duration_ms||!translationCounter(data.manifest_revision)||state.revision&&translationCompare(data.manifest_revision,state.revision)<0)throw Error("Shared caption manifest identity changed");
 const snap=data.snapshot,budget=snap?.expires_in_ms-(performance.now()-started);
 if(!translationId(snap?.lease_id)||snap.revision!==data.manifest_revision||!Number.isSafeInteger(snap.expires_in_ms)||snap.expires_in_ms>30000||budget<=0)throw Error("Shared caption snapshot expired");
 if(!Array.isArray(data.coverage)||data.coverage.length>32||!Array.isArray(data.segments)||data.segments.length>8)throw Error("Shared caption window exceeds its budget");
 let previous=-1;for(const r of data.coverage){if(!translationMs(r.start_ms)||!translationMs(r.end_ms)||r.start_ms<previous||r.end_ms<=r.start_ms||r.end_ms>a.duration_ms)throw Error("Invalid shared caption coverage");previous=r.end_ms;}
 const low=Math.max(0,translationPosition(state)-30000),high=translationPosition(state)+90000,ids=new Set();
 for(const r of data.segments){if(!translationDigest(r.segment_id)||!translationDigest(r.digest)||ids.has(r.segment_id)||!translationMs(r.start_ms)||!translationMs(r.end_ms)||r.end_ms<=r.start_ms||r.end_ms>a.duration_ms||r.end_ms<=low||r.start_ms>=high)throw Error("Invalid shared caption segment reference");ids.add(r.segment_id);}
 state.revision=data.manifest_revision;state.lease={id:snap.lease_id,revision:snap.revision,deadline:performance.now()+budget};state.coverage=data.coverage;state.manifest=data;sharedCaptionArmLease(state,state.lease);return data.segments;
}
async function sharedCaptionRefresh(state){
 if(!state.current()||state.rebinding||state.busy||!state.assetId&&state.catalogOwner)return;state.busy=true;const generation=state.windowGeneration,owner={serial:++state.requestGeneration};state.readOwner=owner;
 try{
  if(!state.assetId){await sharedCaptionCatalog(state,null);return;}
  const{data,started}=await translationRequest(state,sharedCaptionPath(state,`captions/${state.assetId}/manifest`,{position_ms:translationPosition(state)}));
  if(!state.current()||state.readOwner!==owner||generation!==state.windowGeneration)return;const refs=sharedCaptionManifest(state,data,started),lease=state.lease;const allowed=new Map(refs.map(ref=>[ref.segment_id,ref.digest]));for(const[id,segment]of state.segments)if(allowed.get(id)!==segment.digest)state.segments.delete(id);translationPrune(state);
  // Two bounded readers share the existing canonical segment validator.
  let cursor=0;await Promise.all(Array.from({length:Math.min(2,refs.length)},async()=>{while(cursor<refs.length){const r=refs[cursor++];if(state.segments.get(r.segment_id)?.digest!==r.digest)await translationSegment(state,r,lease,generation);}}));
  if(!state.current()||state.readOwner!==owner||generation!==state.windowGeneration||state.lease!==lease||!translationLeaseCurrent(state))return;if(refs.some(ref=>state.segments.get(ref.segment_id)?.digest!==ref.digest))throw Error("Shared caption snapshot is incomplete");translationPrune(state);state.presentationValidated=true;state.error=null;translationApplyCues(state);void sharedCaptionCommitUse(state);
  const button=state.panel.querySelector("[data-shared-caption-use]");if(button)button.disabled=false;
  const covered=translationRangeStatus(state.coverage,translationPosition(state));sharedCaptionStatus(state,covered.covered?"Committed AI captions cover this position. Missing ranges remain unavailable.":"No committed captions cover this position yet. Ask the Source administrator; playback continues.");
 }catch(error){if(state.readOwner===owner&&generation===state.windowGeneration)sharedCaptionFailure(state,error);}finally{if(state.readOwner===owner){state.readOwner=null;state.busy=false;sharedCaptionSchedule(state,state.error?6000:3000);}}
}
function sharedCaptionUse(state){
 if(!state.current()||!state.assetId||!state.presentationValidated||!translationLeaseCurrent(state))return;state.useIntent={assetId:state.assetId,serial:state.player.captionIntentSerial||0};void sharedCaptionCommitUse(state);
}
async function sharedCaptionCommitUse(state){
 const intent=state.useIntent;if(!intent||state.selecting||!state.current()||!state.presentationValidated||!translationLeaseCurrent(state))return;
 if(intent.assetId!==state.assetId||intent.serial!==(state.player.captionIntentSerial||0)){state.useIntent=null;return;}
 state.selecting=true;const expected=intent.serial+1;
 try{await setSub(-1);if(state.useIntent!==intent||!sharedCaptionOwnerCurrent(state)||state.assetId!==intent.assetId||state.player.captionIntentSerial!==expected)return;state.player.captionIntent={kind:"translation",asset_id:intent.assetId,source_key:state.sourceKey};translationApplyCues(state);pbSyncSubIcon();if(typeof watchRenderLedger==="function")watchRenderLedger();}
 finally{state.selecting=false;if(state.useIntent===intent)state.useIntent=null;}
}
function sharedCaptionMediaEvent(state,kind){
 if(!state.current()){if(sharedCaptionOwnerCurrent(state)||PLAYER?.mediaPredecessor===state.player)return;sharedCaptionRetire("playback changed");return;}
 if(kind==="seeking"){state.windowGeneration++;sharedCaptionCancelRead(state);sharedCaptionClearPresentation(state);}
 if(kind!=="timeupdate")void sharedCaptionRefresh(state);
}
function sharedCaptionAttachmentWillChange(player){
 const s=SHARED_CAPTION_READER;if(!s)return;
 if(!translationPageCurrent(s)||sharedCaptionSourceKey(player)!==s.sourceKey||translationAudioOrdinal(player)!==s.audioIndex){sharedCaptionRetire("shared source or audio changed");return;}
 if(player!==s.player&&player.captionIntent==null){player.captionIntent=s.player.captionIntent?{...s.player.captionIntent}:{kind:"off"};player.captionIntentSerial=s.player.captionIntentSerial||0;}
 s.handoffTarget=player;s.positionReady=false;s.presentationValidated=false;s.rebinding=true;s.windowGeneration++;sharedCaptionCancelRead(s);clearTimeout(s.timer);sharedCaptionClearPresentation(s);
}
function sharedCaptionAttachmentDidBegin(player,token){const s=SHARED_CAPTION_READER;if(!s)return;if(sharedCaptionSourceKey(player)!==s.sourceKey||translationAudioOrdinal(player)!==s.audioIndex){sharedCaptionRetire("shared source changed");return;}s.player=player;s.attachment=token;s.handoffTarget=null;s.positionReady=false;s.rebinding=true;}
function sharedCaptionAttachmentPositionReady(player,token){const s=SHARED_CAPTION_READER;if(!s||s.player!==player||s.attachment!==token||!token?.captionPositionReady||!s.current())return;s.positionReady=true;s.rebinding=false;void sharedCaptionRefresh(s);}
function sharedCaptionWatchChanged(){const s=SHARED_CAPTION_READER;if(s&&!s.current()&&!sharedCaptionOwnerCurrent(s))sharedCaptionRetire("watch source changed");}
