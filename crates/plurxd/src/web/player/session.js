"use strict";
// ---- session lifecycle -----------------------------------------------------
// One identity per player instance, for the life of this page. Supersession is
// keyed by it server-side, which is what stops two devices signed in to one
// account from killing each other's streams — and lets this player's own
// seeks, quality changes and audio switches replace its previous stream, which
// is exactly what they should do.
const PLAYBACK_ID=(function(){
  try{ if(crypto&&crypto.randomUUID) return crypto.randomUUID(); }catch(e){}
  return "pb-"+Math.random().toString(36).slice(2)+Date.now().toString(36);
})();
// The control endpoint deliberately requires a UUID so an untrusted client
// cannot create an unbounded identity vocabulary. Keep this separate from the
// older playback_id fallback, whose server contract predates that rule.
const CONTROL_CLIENT_ID=(function(){
  try{ if(crypto&&crypto.randomUUID) return crypto.randomUUID(); }catch(e){}
  let seed=Date.now();
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g,c=>{
    const r=(seed+Math.random()*16)%16|0; seed=Math.floor(seed/16);
    return (c==='x'?r:(r&3)|8).toString(16);
  });
})();
function newRequestId(){
  try{ if(crypto&&crypto.randomUUID) return crypto.randomUUID(); }catch(e){}
  return "rq-"+Math.random().toString(36).slice(2)+Date.now().toString(36);
}
// A progressive remux's telemetry handle. Fresh per stream rather than reusing
// PLAYBACK_ID, because a seek starts a *new* ffmpeg while the old response is
// still draining: one id for both would have the two of them writing to the
// same counters, and the overlay would read a delivery rate that is the sum of
// a stream and its own corpse.
let STREAM_SEQ=0;
function newStreamId(){ return PLAYBACK_ID+"-s"+(++STREAM_SEQ); }
// Build a `/stream.mp4` URL and claim a telemetry handle for it. Every
// progressive remux is built here — first play, seek, audio switch — because
// each of those is a *new* ffmpeg, and a handle that outlived its process
// would have the overlay reporting a dead stream's numbers as live.
function remuxUrl(context, audioIndex, startSec){
  PLAYER.streamId=newStreamId(); PLAYER.sessionId=null;
  const query=playbackFileQueryFromLegacy(CAPS_Q);
  if(audioIndex!=null) query.audio=audioIndex;
  if(PLAYER.aoffset) query.audio_offset_ms=PLAYER.aoffset;
  if(startSec>0) query.start=startSec.toFixed(1);
  query.stream=PLAYER.streamId;
  return tok(playbackFileUrl(context,"stream.mp4",query));
}

// Open a stream. POST, not GET: this spawns a process and supersedes the
// previous one, and a GET that does that can be replayed by anything that
// believes GETs are safe.
// The options every transcode session open shares.
//
// A helper because there are four of them — first play, the error fallback, a
// seek, an audio switch — and any one that forgot a field would drop it in
// silence. A seek that omits the burned subtitle simply loses the subtitles,
// which reads to a viewer as a bug in the subtitles rather than in the seek,
// and that is exactly the shape of thing that survives a review.
function transcodeOpts(startSec, audioIndex, autoHeightOverride){
  let height=transcodeHeight();
  if(height==null && qualityForce()==='auto'){
    height=autoHeightOverride>0?autoHeightOverride:(PLAYER&&PLAYER.autoHeight);
  }
  const o={height, start:startSec};
  // No explicit rung: the session may still owe the viewer the source's
  // resolution (see sessionHeight). Every transcode open shares this helper
  // precisely so the burn, the fallback, a seek and an audio switch cannot
  // disagree about it mid-session.
  if(o.height==null) o.height=sessionHeight();
  if(audioIndex!=null) o.audio=audioIndex;
  // `/decision` already proved the browser can decode Main10 PQ and selected
  // the HDR10 grade. Preserve that immutable request across every session
  // open; `deliveredRange` is display truth and mutates when a session lands,
  // so using it here would turn one SDR fallback into permanent SDR on seek.
  if(PLAYER && PLAYER.requestHdr10) o.hdr10=true;
  if(PLAYER && PLAYER.burnedSub!=null) o.subtitle_burn=PLAYER.burnedSub;
  if(PLAYER && PLAYER.aoffset) o.audio_offset_ms=PLAYER.aoffset;
  return o;
}
// The track whose checkmark `/decision` supplied. Every cold-start transport
// carries it explicitly; otherwise progressive remuxes fall back to stream 0
// while the menu continues to claim the preferred language is playing.
function selectedAudioIndex(player){
  const p=player||PLAYER;
  const track=p&&p.audio&&p.audio[p.curAudio];
  return track?track.index:0;
}
function playbackControlCapabilities(){
  const codecs=["h264"];
  const advertised=String(PLAY_CAPS&&PLAY_CAPS.vcodec||"").split(",");
  if(advertised.includes("hevc")) codecs.push("hevc");
  if(advertised.includes("av1")) codecs.push("av1");
  const ranges=["sdr"];
  if(PLAY_CAPS&&PLAY_CAPS.hdr10t) ranges.push("hdr10");
  if(PLAY_CAPS&&PLAY_CAPS.dv) ranges.push("dolby_vision");
  const displayPx=Math.max((typeof screen!=="undefined"&&screen.height)||0, window.innerHeight||0)
    * Math.max(1, window.devicePixelRatio||1);
  const decoderPx=Number(PLAY_CAPS&&PLAY_CAPS.maxheight)||displayPx;
  const px=Math.min(displayPx||decoderPx,decoderPx||displayPx);
  const capabilities={platform:"web",max_height:Math.max(144,Math.min(2160,Math.round(px)||1080)),
    codecs,dynamic_ranges:ranges,dual_player_preparation:preparedHandoffOffered(PLAYER)};
  if(SERVER&&SERVER.playback_display_aware_auto&&PLAYER?.qualityProtocol==='route-v1'){
    const caps=currentCapsDocument(), target=measuredPresentationTarget();
    const video=caps.video.map(entry=>({codec:entry.codec,profiles:entry.profiles||[],available:true,
      dynamic_ranges:[...(entry.present||[]).map(range=>range==='pq'?'hdr10':range)
        .filter(range=>range==='sdr'||range==='hdr10'||range==='hlg'),
        ...((entry.dv_profiles||[]).length?['dolby_vision']:[])],
      dv_profiles:entry.dv_profiles||[],...(entry.max_height?{max_height:entry.max_height}:{}),
      ...(entry.max_width?{max_width:entry.max_width}:{}),
      ...(entry.max_frame_rate?{max_frame_rate:entry.max_frame_rate}:{}),
      ...(entry.max_bitrate_bps?{max_bitrate_bps:entry.max_bitrate_bps}:{})}));
    const key=JSON.stringify(video);
    const p=PLAYER;
    if(p&&p.abr){
      if(p.abr.decoderSnapshotKey!==key){
        p.abr.decoderSnapshotKey=key;
        p.abr.decoderRevision=Math.min(Number.MAX_SAFE_INTEGER,(p.abr.decoderRevision||0)+1);
      }
      Object.assign(capabilities,{decoder_caps:{revision:p.abr.decoderRevision,video}});
    }
    if(target) Object.assign(capabilities,{presentation_target:target});
  }
  return capabilities;
}
function qualityCatalogSelectionKey(p){
  return JSON.stringify([selectedAudioIndex(p),p.curSub,p.burnedSub,p.aoffset]);
}
function qualityCatalogSelectionCurrent(p){
  return !!p?.abr&&p.abr.catalogSelectionKey===qualityCatalogSelectionKey(p);
}
function playbackControlSelection(p,ownerBound=false){
  const retained=p&&p.qualityRetainedSelection;
  if(retained&&(p.controlIntentGeneration||0)===retained.intentGeneration
    &&p.sessionId===retained.sessionId) return retained.selection;
  const negotiating=p&&p.qualityNegotiatingSelection;
  if(negotiating&&p.directedChange===negotiating.change
    &&(p.controlIntentGeneration||0)===negotiating.change.intentGeneration) return negotiating.selection;
  const requested=playQuality();
  const manualHeight=/^\d+$/.test(String(requested||""))?Number(requested):null;
  const quality=["original","nomse"].includes(requested)
    ? {mode:"original"}
    : manualHeight!=null
      ? {mode:"manual",height:Math.max(144,Math.min(2160,manualHeight))}
      // D3-a: Auto carries the rung the client's own controller wants.
      //
      // `p.autoHeight` is what is being DELIVERED; this is what was asked for,
      // and they are deliberately different fields. Without the ask on the
      // wire an Auto controller moving 720 -> 1080 sends the same selection it
      // sent at 720, the server's digest is unchanged, and the exchange is not
      // a selection change at all - so no successor is ever staged for it.
      // Absent means plain Auto, which digests exactly as it always has.
      : (p&&p.abr&&p.abr.requestedCandidateId&&qualityCatalogSelectionCurrent(p)&&SERVER&&SERVER.playback_display_aware_auto
        &&SERVER.display_aware_auto_protocol==='route-v1'&&(!ownerBound||p.qualityProtocol==='route-v1'))
        ? {mode:"auto",candidate_id:p.abr.requestedCandidateId,
          ...((p.qualityCandidates||[]).find(candidate=>candidate.id===p.abr.requestedCandidateId)?.route==='encode'
            ?{height:(p.qualityCandidates||[]).find(candidate=>candidate.id===p.abr.requestedCandidateId).target_height}:{})}
      : (p&&p.autoRequestedHeight>0)
        ? {mode:"auto",height:Math.max(144,Math.min(2160,Math.round(p.autoRequestedHeight)))}
        : {mode:"auto"};
  const selected=(p&&p.curSub>=0)?p.curSub:null;
  const subtitle=selected==null ? {mode:"off",track:null}
    : {mode:p.burnedSub!=null?"burn":"native",track:selected};
  const audioTrack=Math.max(0,Math.min(1024,Math.round(selectedAudioIndex(p))));
  const audioOffset=Math.max(-15000,Math.min(15000,Math.round(p&&p.aoffset||0)));
  return {quality,audio_track:audioTrack,subtitle,
    audio_offset_ms:audioOffset,codec:"auto",dynamic_range:"auto"};
}
function qualityMediaIntent(p,wireSelection,independentlyNegotiated=false){
  const independent=independentlyNegotiated||!!p?.continuousQualityBootstrap
    ||typeof qualityControlSupported==="function"&&qualityControlSupported(p);
  if(!p||(!independent&&(!p.abr||!SERVER||SERVER.display_aware_auto_protocol!=='route-v1'
    ||!SERVER.playback_display_aware_auto))) return null;
  const wire=wireSelection||playbackControlSelection(p);
  const subtitles=wire.subtitle.mode==='off'?{mode:'off'}:{mode:wire.subtitle.mode,track:wire.subtitle.track};
  const selection={quality:wire.quality,codec:wire.codec,dynamic_range:wire.dynamic_range,
    audio_track:wire.audio_track,audio_offset_ms:wire.audio_offset_ms,subtitles};
  const holder=p.abr||p;
  const key=JSON.stringify(selection), state=holder.mediaIntent||{
    lifetimeId:newRequestId(),recipeRevision:0,transportRevision:0};
  if(state.recipeKey!==key){state.recipeKey=key;state.recipeRevision++;}
  const video=/** @type {HTMLVideoElement|null} */ (document.getElementById('video'));
  const transport=JSON.stringify([p.wantsPlayback!==false,video&&video.playbackRate||1]);
  if(state.transportKey!==transport){state.transportKey=transport;state.transportRevision++;}
  holder.mediaIntent=state;
  return {lifetime_id:state.lifetimeId,recipe_revision:state.recipeRevision,
    destination_revision:(p.controlSeekSequence||0)+1,transport_revision:state.transportRevision,selection};
}
function playbackControlBufferedRange(v,p,positionMs){
  try{
    const local=(v.currentTime||0), origin=p.offset||0;
    for(let i=0;i<v.buffered.length;i++){
      if(v.buffered.start(i)<=local+0.25 && v.buffered.end(i)>=local){
        const from=Math.max(0,Math.round((origin+v.buffered.start(i))*1000));
        const through=Math.max(positionMs,Math.round((origin+v.buffered.end(i))*1000));
        return {from,through};
      }
    }
  }catch(e){}
  return {from:null,through:positionMs};
}
// Recovery handlers know facts the media element cannot expose: whether a
// persistent wait ran out of bytes or stalled with bytes already buffered,
// and whether hls.js stopped on a manifest, network, media, or decoder error.
// Admit only protocol enums and a tightly-bounded single-line detail so a
// library error object can never leak arbitrary fields into the wire request.
function playbackControlObservationOverride(value){
  if(!value||typeof value!=="object") return null;
  const observation={};
  if(["unknown","ready","starved","failed"].includes(value.decoder_state))
    observation.decoder_state=value.decoder_state;
  if(["network","manifest","media","decoder","drm","unknown"].includes(value.error_code))
    observation.error_code=value.error_code;
  if(observation.error_code && value.error_detail!=null){
    const detail=String(value.error_detail).replace(/[\r\n\0]/g," ").slice(0,120);
    if(detail) observation.error_detail=detail;
  }
  return Object.keys(observation).length?observation:null;
}
function playbackControlHlsFatal(data,started){
  const detail=String(data&&data.details||"");
  const type=String(data&&data.type||"");
  const errorTypes=typeof Hls!=="undefined"&&Hls.ErrorTypes;
  const mediaFailure=!!((errorTypes && data&&data.type===errorTypes.MEDIA_ERROR)
    || /codec|decode|parsing/i.test(detail));
  const manifestFailure=/manifest/i.test(detail);
  const networkFailure=!mediaFailure && !!((errorTypes&&data&&data.type===errorTypes.NETWORK_ERROR)
    || /network|loaderror|loadtimeout/i.test(type+" "+detail));
  const observation=mediaFailure
    ? {decoder_state:"failed",error_code:/codec|decode/i.test(detail)?"decoder":"media",
        error_detail:detail||type||"hls_media_fatal"}
    : {decoder_state:started?"ready":"unknown",
        error_code:manifestFailure?"manifest":networkFailure?"network":"unknown",
        error_detail:detail||type||"hls_fatal"};
  return {media_failure:mediaFailure,observation};
}
function playbackControlSnapshot(v,p){
  if(!v||!p) return null;
  // R1. A committed destination is only this presentation's seek once an
  // attachment has executed it — `markPlaybackControlSeekExecuted` runs when
  // a local seek is applied to this element or when a successor attaches. An
  // unexecuted destination belongs to a replacement that is still being
  // created, or was refused, and it is not something the attached stream is
  // doing. Reporting it as `seeking` with a foreign `seek_target_ms` is what
  // reset the server's startup baseline on every exchange and reaped a
  // healthy incumbent at first_served_at + 30 s while it was visibly playing.
  const pendingSeek=p.controlSeek||null;
  const attachedSeek=pendingSeek&&pendingSeek.executed?pendingSeek:null;
  const seeking=!!attachedSeek || !!v.seeking || p._seekPreview!=null;
  const hinted=!p.started&&p.controlPositionHintSec!=null?p.controlPositionHintSec:null;
  const preview=!attachedSeek&&seeking&&p._seekPreview!=null
    ? Math.max(0,p._seekPreview-((p.bookOffset||0)/1000)) : null;
  let positionMs=Math.max(0,Math.round((hinted!=null?hinted:
    preview!=null?preview:(p.offset||0)+(v.currentTime||0))*1000));
  const duration=Math.max(0,Number(p.durMs||p.knownDur||0));
  if(duration>0) positionMs=Math.min(positionMs,duration);
  const range=hinted!=null?{from:null,through:positionMs}
    :playbackControlBufferedRange(v,p,positionMs);
  // `ended` means the end of the bytes the browser received, not necessarily
  // the title. The legacy early-end handler reopens a truncated stream, so it
  // is active failed demand until it is actually within the completion slack.
  const terminalEnded=!!v.ended && (!(duration>0)
    || positionMs>=Math.max(0,duration-ENDED_SLACK_SEC*1000));
  const wantsPlayback=p.wantsPlayback??!v.paused;
  const demand=(terminalEnded?"end":v.ended?"active":wantsPlayback?"active":"hold");
  let render="rendering";
  if(v.error) render="failed";
  else if(v.ended) render=terminalEnded?"ended":"failed";
  else if(p.controlRenderOverride) render=p.controlRenderOverride;
  else if(seeking) render="seeking";
  else if(!p.started) render="starting";
  else if(p.waitAt && performance.now()-p.waitAt>=PERSISTENT_STALL_MS) render="stalled";
  else if(p.waitAt || (!v.paused&&v.readyState<3)) render="waiting";
  const rate=Number(v.playbackRate||0);
  const playbackRate=demand==="active"
    ? Math.max(0.25,Math.min(4,rate||1)) : Math.max(0,Math.min(4,rate));
  let dropped=null;
  try{
    const q=v.getVideoPlaybackQuality&&v.getVideoPlaybackQuality();
    if(q&&Number.isFinite(q.droppedVideoFrames)) dropped=Math.max(0,Math.round(q.droppedVideoFrames));
  }catch(e){}
  const observation={decoder_state:v.error?"failed":render==="stalled"?"starved":p.started?"ready":"unknown"};
  if(dropped!=null) observation.dropped_frames=dropped;
  if(v.error){ observation.error_code="media"; observation.error_detail=`media_code_${v.error.code||0}`; }
  // The element-level error is a generic fallback. A recovery callback may
  // know the actual HLS class, so its bounded evidence takes precedence.
  const triggerObservation=playbackControlObservationOverride(p.controlObservationOverride);
  if(triggerObservation) Object.assign(observation,triggerObservation);
  const bps=p.hls&&Number(p.hls.bandwidthEstimate);
  const snapshot={demand,position_ms:positionMs,buffered_from_ms:range.from,
    buffered_through_ms:range.through,playback_rate:playbackRate,render_state:render,
    seek_target_ms:render==="seeking"
      ? (attachedSeek?attachedSeek.targetMs:positionMs) : null,
    observed_download_bps:Number.isFinite(bps)&&bps>0?Math.round(bps):null,
    selection:playbackControlSelection(p,true),capabilities:playbackControlCapabilities(),
    observation,acknowledgement:pendingPlaybackControlAcknowledgement(p,demand)};
  if(p.continuousQuality&&continuousQualitySelectionCompatible(p,snapshot.selection))
    snapshot.supported_actions=PlurxPlaybackControl.SUPPORTED_ACTIONS.filter(action=>action!==PlurxPlaybackControl.PREPARE_REPLACEMENT_ACTION);
  const intent=(p.qualityProtocol==='route-v1'||qualityControlSupported(p)||p.continuousQualityBootstrap)?qualityMediaIntent(p,snapshot.selection):null;
  if(intent) Object.assign(snapshot,{intent});
  // A commit and terminal demand are both true, but the protocol deliberately
  // refuses them in one exchange: publishing the successor has to win before
  // ending the newly-published session. Carry the commit on one synthetic
  // active snapshot; once the accepted acknowledgement leaves the queue, the
  // next capture reports the element's real `end` demand.
  if(snapshot.demand==="end"&&snapshot.acknowledgement?.state==="committed"){
    snapshot.demand="active";
    snapshot.playback_rate=Math.max(0.25,snapshot.playback_rate||0);
  }
  return snapshot;
}

// Cancellation is negotiated separately from rendition switching and the
// legacy strict control envelope. Cache support only for the current owner.
function qualityControlOwnerKey(p){
  const bootstrap=p&&p.controlReporter&&p.controlReporter.bootstrap;
  return bootstrap?JSON.stringify([p.sessionId,bootstrap.generation,bootstrap.control_epoch]):null;
}
function qualityControlSupported(p){
  const key=qualityControlOwnerKey(p);
  return !!key&&!!p.qualityControlSupport&&p.qualityControlSupport.key===key
    &&p.qualityControlSupport.supported===true;
}
function validQualityControlIdentity(identity,request){
  if(!identity||typeof identity!=="object"||Array.isArray(identity)) return false;
  const keys=["generation","control_epoch","client_instance_id","lifetime_id","recipe_revision","accepted_sequence"];
  if(Object.keys(identity).some(key=>!keys.includes(key))) return false;
  return identity.generation===request.generation&&identity.control_epoch===request.control_epoch
    &&typeof identity.client_instance_id==="string"&&identity.client_instance_id.length===36
    &&typeof identity.lifetime_id==="string"&&identity.lifetime_id.length>0&&identity.lifetime_id.length<=128
    &&!/[\r\n\0]/.test(identity.lifetime_id)
    &&Number.isSafeInteger(identity.recipe_revision)&&identity.recipe_revision>0
    &&Number.isSafeInteger(identity.accepted_sequence)&&identity.accepted_sequence>0;
}
async function exchangeQualityControl(p,request){
  const reporter=p&&p.controlReporter, bootstrap=reporter&&reporter.bootstrap;
  const key=qualityControlOwnerKey(p);
  if(!bootstrap||reporter.stopped||!key||!String(bootstrap.url||"").endsWith("/control")) return null;
  const controller=new AbortController(), timer=setTimeout(()=>controller.abort(),4000);
  try{
    const response=await fetch(bootstrap.url.replace(/\/control$/,"/quality-control"),{
      method:"POST",signal:controller.signal,headers:{"content-type":"application/json"},
      body:JSON.stringify(request)});
    if(qualityControlOwnerKey(p)!==key||p.controlReporter!==reporter||reporter.stopped) return null;
    if(response.status===404||response.status===405) return {version:1,
      generation:request.generation,control_epoch:request.control_epoch,features:[],
      outcome:"unsupported",pending_identity:null};
    if(!response.ok) return null;
    const text=await response.text();
    if(text.length>4096||qualityControlOwnerKey(p)!==key
      ||p.controlReporter!==reporter||reporter.stopped) return null;
    const value=JSON.parse(text), keys=["version","generation","control_epoch","features","outcome","pending_identity"];
    if(!value||typeof value!=="object"||Array.isArray(value)
      ||Object.keys(value).some(key=>!keys.includes(key))
      ||value.version!==1||value.generation!==request.generation||value.control_epoch!==request.control_epoch
      ||!Array.isArray(value.features)||value.features.length>1
      ||value.features.some(feature=>feature!=="quality_cancel_v1")
      ||(value.pending_identity!=null&&!validQualityControlIdentity(value.pending_identity,request))) return null;
    const outcomes=request.operation==="discover"?["supported","unsupported"]
      :["cancel_requested","cancelled","observation_unknown","unsupported"];
    if(!outcomes.includes(value.outcome)
      ||(value.outcome==="unsupported"?value.features.length!==0:value.features.length!==1)) return null;
    return value;
  }catch(e){ return null; }
  finally{ clearTimeout(timer); }
}
async function discoverQualityControl(p,fresh=false){
  const key=qualityControlOwnerKey(p), reporter=p&&p.controlReporter;
  if(!key||!reporter||reporter.stopped) return null;
  if(p.qualityControlDiscovery&&p.qualityControlDiscovery.key===key) return p.qualityControlDiscovery.promise;
  if(!fresh&&p.qualityControlSupport&&p.qualityControlSupport.key===key) return p.qualityControlSupport.response;
  const bootstrap=reporter.bootstrap, request={version:1,generation:bootstrap.generation,
    control_epoch:bootstrap.control_epoch,operation:"discover",identity:null};
  const promise=exchangeQualityControl(p,request).then(response=>{
    if(qualityControlOwnerKey(p)===key&&p.controlReporter===reporter){
      p.qualityControlSupport={key,supported:!!response&&response.features.includes("quality_cancel_v1"),response};
    }
    return response;
  }).finally(()=>{
    if(p.qualityControlDiscovery&&p.qualityControlDiscovery.promise===promise) p.qualityControlDiscovery=null;
  });
  p.qualityControlDiscovery={key,promise};
  return promise;
}
async function cancelUnappendedQualityIntent(p,change){
  const intent=change&&change.qualityIntent;
  if(!p||!intent) return "unsupported";
  const ownerKey=qualityControlOwnerKey(p);
  let identity=change.cancellationOwnerKey===ownerKey?change.cancellationIdentity:null;
  if(!identity){
    const response=await discoverQualityControl(p,true);
    if(qualityControlOwnerKey(p)!==ownerKey) return "observation_unknown";
    if(!response||!response.features.includes("quality_cancel_v1")) return "unsupported";
    identity=response.pending_identity;
    // A discovery for a newer choice cannot cancel it for this older one.
    if(!identity||identity.client_instance_id!==CONTROL_CLIENT_ID
      ||identity.lifetime_id!==intent.lifetime_id||identity.recipe_revision!==intent.recipe_revision)
      return "observation_unknown";
    change.cancellationIdentity=identity;
    change.cancellationOwnerKey=ownerKey;
  }
  const request={version:1,generation:identity.generation,control_epoch:identity.control_epoch,
    operation:"cancel_unappended",identity};
  const result=await exchangeQualityControl(p,request);
  if(qualityControlOwnerKey(p)!==ownerKey) return "observation_unknown";
  // Only the exact receipt settles cleanup. A missing or different identity
  // cannot promote an initiation response to completed cancellation.
  if(result?.outcome==="unsupported") return "unsupported";
  if(!result||!result.pending_identity||Object.keys(identity).some(key=>result.pending_identity[key]!==identity[key]))
    return "observation_unknown";
  change.cancellationOutcome=result.outcome;
  return result.outcome;
}
async function settleQualityCancellation(p,change){
  if(change.cancellationPromise) return change.cancellationPromise;
  const ownerKey=qualityControlOwnerKey(p);
  const promise=(async()=>{
    let outcome=await cancelUnappendedQualityIntent(p,change);
    // Three exact, idempotent attempts bound receipt recovery. Keep the
    // identity after request loss; discovery may no longer name a cleaned job.
    for(const delay of [500,1000]){
      if(!change.cancellationIdentity||["cancelled","unsupported"].includes(outcome)) break;
      await new Promise(resolve=>setTimeout(resolve,delay));
      if(qualityControlOwnerKey(p)!==ownerKey||p.controlReporter?.stopped) return "observation_unknown";
      outcome=await cancelUnappendedQualityIntent(p,change);
    }
    return outcome;
  })().finally(()=>{if(change.cancellationPromise===promise) change.cancellationPromise=null;});
  change.cancellationPromise=promise;
  return promise;
}
