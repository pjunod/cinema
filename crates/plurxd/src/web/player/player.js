"use strict";
// ---- player ---------------------------------------------------------------
// Three playback methods (server /decision picks one):
//  direct_play → native <video>, client-side seek/resume
//  remux       → progressive fMP4 from ffmpeg, server fast-seek for resume
//  transcode   → HLS via hls.js (ffmpeg tone-maps/re-encodes), offset resume
let PLAYER={fileId:null,timer:null,offset:0,hls:null,knownDur:0,durMs:0,markers:[],source:null,started:false,idleTimer:null,autoskip:false,deliveredRange:null,deliveredDvProfile:null,waitTimer:null,bookOffset:0,bookDuration:0,bookParts:null,_seekPreview:null,_seekPending:null,_lastFocusedControl:"pbplay",_opener:null,_openerClick:null};
let PENDING_LIBRARY_CHANNEL_PLAYBACK=null;
let LIBRARY_CHANNEL_RETURN=null;
let PLAYER_WIRED=false;
// Native HLS (`video.src = playlist.m3u8`) is for real Safari ONLY — it's what
// makes AirPlay work (an MSE stream can't be flung to an Apple TV). The trap:
// modern Chrome answers "maybe" to canPlayType('application/vnd.apple.mpegurl')
// because it grew a half-working built-in HLS player — which downloads the
// playlist and first segment and then silently never plays (the gray-screen
// bug). So the gate is the WebKit AirPlay API, which only Safari has, never
// canPlayType alone. Everyone else gets hls.js (MSE), which is the tested path.
function useNativeHls(video){
  return PlaybackPolicy.nativeHlsAvailable({
    canPlayNativeHls:!!video.canPlayType('application/vnd.apple.mpegurl'),
    hasWebKitPlaybackTarget:window.WebKitPlaybackTargetAvailabilityEvent !== undefined,
    hlsJsSupported:!!(window.Hls && Hls.isSupported())
  });
}
// Which HLS transport to actually use. hls.js (MSE) plays inline so OUR controls
// + scrubber sit on top (the Chrome experience); Safari's native HLS instead
// hands the stream to its own player — "Live Broadcast", no scrubber, and our
// overlay is gone. So on Safari prefer hls.js and reserve native HLS for the one
// case that needs it: HEVC, which Safari won't decode through MSE. Trade-off:
// AirPlay of a *stream* needs native HLS (MSE can't be flung to an Apple TV), so
// AirPlaying a transcode/remux from Safari is sacrificed — direct-play files
// (played from a plain video.src) still AirPlay.
function streamIsHevc(){
  if(!PLAYER || PLAYER.method==='transcode') return false;   // transcode output is always H.264
  if(PLAYER.copyHls){
    const c=((PLAYER.source&&PLAYER.source.video_codec)||"").toLowerCase();
    return c==='hevc'||c==='h265'||c==='hevc10';
  }
  return false;
}
function hlsJsSupported(){
  return !!(window.Hls && Hls.isSupported());
}
function preferNativeHls(video){
  return PlaybackPolicy.hlsTransport({
    nativeHls:useNativeHls(video), hevcCopy:streamIsHevc(),
    hlsJsSupported:hlsJsSupported()
  })==='native';
}
// The transport this session will actually be attached with, named at create
// so the server knows how this client tears the stream down.
//
// It is the same `PlaybackPolicy.hlsTransport` decision `preferNativeHls`
// makes at attach, given the same inputs — a guess from the user agent would
// be worth nothing, and a disagreement between the two would leave a session
// eligible for a short retired promise that its real teardown does not earn.
// `hevcCopy` has to be passed in because `streamIsHevc` reads `PLAYER.copyHls`,
// which a first copy open has not set yet.
//
// Returns null when the answer cannot be decided here. An absent field is the
// conservative class on the server, which is the right answer for "unknown".
function plannedHlsTransport(hevcCopy){
  try{
    const video=document.getElementById("video");
    if(!video) return null;
    return PlaybackPolicy.hlsTransport({
      nativeHls:useNativeHls(video), hevcCopy:!!hevcCopy,
      hlsJsSupported:hlsJsSupported()
    })==='native'?"native":"hlsjs";
  }catch(e){ return null; }
}
// The last refusal the server explained.
//
// hls.js consumes the HTTP response and hands its ERROR event only the status
// line (verified in the vendored build: `response:{url,data:void 0,code:
// o.status,text:o.statusText}`), so the `{code, message}` body naming the
// actual cause never reaches the error handler. Catching it at the transport
// is the only place it exists.
//
// Two guards against this file's documented stale-reason trap (PLAYBACK.md,
// "The error fallback"): it is cleared whenever a stream is attached, and it
// is stamped, so a segment refused at minute two cannot be produced as the
// confident explanation for a fatal error at minute forty.
let STREAM_FAILURE=null;
function clearStreamFailure(){ STREAM_FAILURE=null; }
function boundStreamFailureBody(body){
  if(typeof body!=="string") return null;
  return body.length<=PlaybackPolicy.STREAM_FAILURE_BODY_MAX_CHARS?body:null;
}
function decodeStreamFailureBytes(value){
  try{
    let bytes=null;
    if(value instanceof ArrayBuffer) bytes=new Uint8Array(value);
    else if(ArrayBuffer.isView(value)) bytes=new Uint8Array(value.buffer,value.byteOffset,value.byteLength);
    if(!bytes||bytes.byteLength>PlaybackPolicy.STREAM_FAILURE_BODY_MAX_CHARS) return null;
    return boundStreamFailureBody(new TextDecoder("utf-8",{fatal:false}).decode(bytes));
  }catch(e){ return null; }
}
// Media requests use ArrayBuffer responses, while manifest requests normally
// use text. Read only the property allowed by the declared response type:
// Safari throws InvalidStateError when responseText is touched on binary XHR.
// Blob is the one asynchronous case and is bounded before decoding.
function streamFailureResponseBodyNow(xhr){
  if(!xhr) return null;
  const type=String(xhr.responseType||"").toLowerCase();
  try{
    if(type===""||type==="text") return boundStreamFailureBody(xhr.responseText);
    if(type==="json"){
      if(typeof xhr.response==="string") return boundStreamFailureBody(xhr.response);
      const source=xhr.response;
      if(!source||typeof source!=="object") return null;
      const selected={};
      for(const key of ["code","message","error","film_position_ms"]){
        if(Object.prototype.hasOwnProperty.call(source,key)) selected[key]=source[key];
      }
      return boundStreamFailureBody(JSON.stringify(selected));
    }
    if(type==="arraybuffer") return decodeStreamFailureBytes(xhr.response);
  }catch(e){}
  return null;
}
async function streamFailureResponseBody(xhr){
  const immediate=streamFailureResponseBodyNow(xhr);
  if(immediate!=null) return immediate;
  if(!xhr||String(xhr.responseType||"").toLowerCase()!=="blob") return null;
  try{
    const blob=xhr.response;
    if(!blob||blob.size>PlaybackPolicy.STREAM_FAILURE_BODY_MAX_CHARS
      ||typeof blob.arrayBuffer!=="function") return null;
    return decodeStreamFailureBytes(await blob.arrayBuffer());
  }catch(e){ return null; }
}
async function observeStreamFailureResponse(xhr,evidence,current){
  const body=await streamFailureResponseBody(xhr);
  if(typeof current==="function"&&!current()) return null;
  return noteStreamFailure(Number(xhr&&xhr.status)||0,body,evidence);
}
function noteStreamFailure(status, body, evidence){
  const parsed=PlaybackPolicy.parseStreamFailure({status:status, body:body});
  if(!parsed) return null;
  const owned=evidence||{};
  const previous=STREAM_FAILURE;
  // A completion from an older manifest request cannot replace a newer
  // diagnosis. The attachment identity is checked by the caller; the ordinal
  // orders requests within that attachment.
  if(previous&&owned.attachment&&previous.attachment===owned.attachment
    &&previous.resource===owned.resource
    &&previous.request_ordinal!=null&&owned.request_ordinal!=null
    &&Number(previous.request_ordinal||0)>Number(owned.request_ordinal||0)) return previous;
  Object.assign(parsed,{at:Date.now()},owned);
  STREAM_FAILURE=parsed;
  return parsed;
}
// A successful reload belongs to one hls.js instance. Only that instance may
// clear its retryable playlist refusal: a late success event from a destroyed
// predecessor must not erase the current stream's explanation, and a level
// reload is not evidence that some terminal segment refusal recovered.
function clearStreamFailureFor(hls){
  const retryable=STREAM_FAILURE &&
    PlaybackPolicy.hlsStartupResponseAction(STREAM_FAILURE)==="retry";
  if(retryable && PLAYER && PLAYER.hls===hls
    &&(!STREAM_FAILURE.attachment||STREAM_FAILURE.attachment===PLAYER.mediaAttachment))
    clearStreamFailure();
}
function currentStreamFailureOverlay(){
  if(STREAM_FAILURE&&STREAM_FAILURE.attachment&&PLAYER
    &&STREAM_FAILURE.attachment!==PLAYER.mediaAttachment) return null;
  return PlaybackPolicy.streamFailureOverlay(STREAM_FAILURE,Date.now());
}
// Session creation fails before `attachHls`, so the preserved typed refusal is
// classified here. Returning false lets callers keep their generic handling for
// an unstructured network/server error; true means a fault carrying the
// server's own sentence has been raised and the caller has nothing to add.
//
// `context` is the caller's, because only the caller knows whether a
// predecessor still has the picture — which is the difference between a failed
// start and a refused change (contract §3.3 rows 4-10).
function showSessionOpenFailure(error,context){
  const failure=error&&error.streamFailure;
  if(!failure) return false;
  failure.at=Date.now();
  STREAM_FAILURE=failure;
  const explained=currentStreamFailureOverlay();
  if(!explained) return false;
  const where=context||playbackSurfaceContext(PLAYER);
  // A refusal no row claims is still a refusal the server explained, and the
  // sentence is the whole reason this function exists. It falls to the row its
  // context makes true — the owner having nothing left, or a change that was
  // refused — rather than back to the caller's generic "could not prepare".
  const source=PlaybackPolicy.classifyStreamFailure({
    status:failure.status,code:failure.code,context:where})
    ||(where==="change"?"change_failed":"owner_stopped");
  if(PLAYER) clientLog(Object.assign({level:"error",event:"stream_refused",
    detail:failure.code||String(failure.status), message:explained.detail},
    playbackContext()));
  // The owner's decision, taken here rather than by the table: a refusal the
  // table answers with a blocking class is the server saying this attempt is
  // over, so there is nothing left to try and the player stops before the
  // surface goes over it (§3.2, §3.4). Ruled 2026-09-13: row 3 (401/403) wins
  // in every context, a pending change included — a stream nobody is
  // authorised for is not a stream the predecessor's playback can outlive.
  const spent=playbackSurfaceSourceIsBlocking(source,where);
  if(spent) stopPlayerForExhaustion();
  raisePlaybackSurface(source,{context:where,player_stopped:spent,
    title:explained.title,detail:explained.detail,
    intent:where==="change"&&!spent?playbackSurfaceIntent(PLAYER):null,
    position_ms:failure.position_ms==null?null:failure.position_ms});
  toast(explained.retryable?"Still starting…":"Playback failed");
  return true;
}
// One place that attaches an HLS playlist to the video element (used by start,
// seek-restart, and audio-switch). Worker off: a wallet extension's injected
// CSP can silently kill a blob worker; main-thread demux of one 720p stream
// is nothing.
// `startAt` is where to begin inside the immutable film timeline.
function vodClientContract(){
  // P3 measured this exact vendored hls.js policy. Keep the fragment request's
  // first-byte timer and the server declaration in one place: the server must
  // answer two seconds before hls.js aborts, leaving room for the typed 503 to
  // reach the retry controller. Seven error retries extend the measured
  // 1/2/4/8/8… ladder past the producer's 30-second materialization watchdog.
  const firstByteMs=10000;
  const serverMarginMs=2000;
  return {
    session:{
      presentation:"vod",
      block_budget_secs:(firstByteMs-serverMarginMs)/1000
    },
    fragLoadPolicy:{default:{
      maxTimeToFirstByteMs:firstByteMs,
      maxLoadTimeMs:120000,
      timeoutRetry:{maxNumRetry:4,retryDelayMs:0,maxRetryDelayMs:0},
      errorRetry:{maxNumRetry:7,retryDelayMs:1000,maxRetryDelayMs:8000}
    }}
  };
}
function playbackAttemptTerminallyStopped(player,attachment){
  const stopped=player&&player.terminalStop;
  return !!(stopped&&stopped.attachment===(attachment||player.mediaAttachment));
}
function hlsStartupCurrent(player,episode){
  return !!(player&&episode&&PLAYER===player&&player.hlsStartup===episode
    &&player.hls===episode.hls&&episode.attachment.current()
    &&!playbackAttemptTerminallyStopped(player,
      episode.mediaAttachment||player.mediaAttachment));
}
function hlsStartupIncomplete(player){
  const episode=player&&player.hlsStartup;
  return !!(episode&&!['presenting','cancelled'].includes(episode.state));
}
function hlsStartupManifestRequest(context){
  return !!(context&&(context.type==='manifest'||/\.m3u8(?:\?|$)/i.test(String(context.url||''))));
}
function abortHlsStartupLoaders(episode){
  if(!episode) return;
  for(const loader of [...episode.loaders]){
    try{loader.abort()}catch(e){}
    try{loader.destroy()}catch(e){}
  }
  episode.loaders.clear();
}
function cancelHlsStartup(player,reason){
  const episode=player&&player.hlsStartup;
  if(!episode||episode.state==='cancelled') return;
  if(episode.state==='presenting'){
    episode.establishedSuspension=null;
    return;
  }
  episode.state='cancelled'; episode.cancelledReason=reason||'cancelled';
  clearTimeout(episode.retry.timer); episode.retry.timer=null;
  abortHlsStartupLoaders(episode);
}
function completeHlsStartup(player){
  const episode=player&&player.hlsStartup;
  if(!episode||!hlsStartupCurrent(player,episode)||episode.state!=='active'
    ||player.controlSeek?.executed) return false;
  episode.state='presenting';
  clearTimeout(episode.retry.timer); episode.retry.timer=null;
  clearTimeout(player.stallTimer); player.stallTimer=null;
  reportTtff();
  clientLog(Object.assign({level:'info',event:'hls_startup_presenting',
    detail:`manifest=${episode.manifestState};media=${episode.mediaLoaded?'yes':'no'}`,
    message:`HLS startup presented after ${Math.round(performance.now()-episode.startedAt)}ms`},
    playbackContext()));
  return true;
}
function configureHlsStartupDeadline(player,graceMs){
  const episode=player&&player.hlsStartup;
  if(!episode||!hlsStartupCurrent(player,episode)||episode.state==='presenting') return;
  const grace=Number(graceMs)||PlaybackPolicy.HLS_STARTUP.cold_deadline_ms;
  episode.deadlineMs=Math.min(episode.deadlineMs,performance.now()+grace);
}
function diagnoseHlsStartup(player){
  const episode=player&&player.hlsStartup;
  const failure=episode&&episode.latestFailure;
  const explained=failure&&PlaybackPolicy.streamFailureOverlay(failure,Date.now());
  if(explained&&failure.code==='startup_timeout') return {
    title:'The stream did not become ready in time.',detail:explained.detail};
  if(episode&&episode.manifestState!=='parsed'){
    const status=failure&&failure.status;
    return {title:'The stream playlist could not be loaded.',
      detail:failure&&failure.message?failure.message:
        `The playlist did not load before the startup deadline${status?` (HTTP ${status})`:''}.`};
  }
  if(episode&&!episode.mediaLoaded) return {
    title:'The video data did not arrive in time.',
    detail:'The playlist loaded, but no media arrived before the startup deadline.'};
  return {title:'Playback did not start.',
    detail:'Media arrived, but no presented frame or advancing audio clock was observed. Try again; the decoder cause is not known.'};
}
function exhaustHlsStartup(player,reason){
  const episode=player&&player.hlsStartup;
  if(!episode||!hlsStartupCurrent(player,episode)||episode.state==='presenting'
    ||episode.state==='cancelled') return false;
  if(episode.state!=='exhausted'){
    episode.state='exhausted'; episode.exhaustedReason=reason||'deadline';
    clearTimeout(episode.retry.timer); episode.retry.timer=null;
    abortHlsStartupLoaders(episode);
    try{episode.hls.stopLoad()}catch(e){}
  }
  setTimeout(()=>stallDiagnose().catch(()=>{}),0);
  return true;
}
function armHlsStartupRetry(video,player,episode){
  clearTimeout(episode.retry.timer);
  const delay=Math.max(0,episode.retry.dueMs-performance.now());
  episode.retry.timer=setTimeout(()=>{
    episode.retry.timer=null;
    if(!hlsStartupCurrent(player,episode)) return cancelHlsStartup(player,'ownership_changed');
    if(episode.state==='paused') return;
    if(episode.retry.intentGeneration!==(player.controlIntentGeneration||0)){
      episode.retry.state='cancelled'; return;
    }
    if(performance.now()>=episode.deadlineMs) return exhaustHlsStartup(player,'deadline');
    if(player.wantsPlayback===false) return pauseHlsStartup(player);
    episode.retry.state='dispatched'; player.hlsRetryUsed=1;
    const at=Math.max(0,Number(video&&video.currentTime)||0);
    const missing=episode.state!=='presenting'&&episode.manifestState!=='parsed';
    const action=missing?'reload_manifest':'resume_loading';
    clientLog(Object.assign({level:'info',event:'hls_retry',detail:episode.retry.detail,
      action,ordinal:episode.dispatches+1,
      elapsed_ms:Math.round(performance.now()-episode.startedAt),
      remaining_ms:Math.max(0,Math.round(episode.deadlineMs-performance.now())),
      message:missing?'reloading the current manifest after its initial load failed'
        :`resuming the established stream at ${at.toFixed(1)}s after a fatal network error`},
      playbackContext()));
    try{
      if(missing) episode.hls.loadSource(episode.playlistUrl);
      else episode.hls.startLoad(at);
    }catch(e){ exhaustHlsStartup(player,'retry_dispatch_failed'); }
  },delay);
}
function pauseHlsStartup(player){
  const episode=player&&player.hlsStartup;
  if(!episode||!hlsStartupCurrent(player,episode)
    ||!['active','presenting'].includes(episode.state)) return false;
  if(episode.state==='presenting'){
    if(episode.retry.state==='reserved') episode.retry.state='cancelled';
    clearTimeout(episode.retry.timer); episode.retry.timer=null;
    if(!episode.establishedSuspension){
      episode.establishedSuspension={attachment:player.mediaAttachment,
        intentGeneration:player.controlIntentGeneration||0};
    }
    try{episode.hls.stopLoad()}catch(e){}
    return true;
  }
  episode.state='paused';
  clearTimeout(episode.retry.timer); episode.retry.timer=null;
  abortHlsStartupLoaders(episode);
  try{episode.hls.stopLoad()}catch(e){}
  return true;
}
function resumeHlsStartup(video,player){
  const episode=player&&player.hlsStartup;
  if(!episode||!hlsStartupCurrent(player,episode)) return false;
  if(episode.state==='presenting'){
    const suspended=episode.establishedSuspension;
    if(!suspended||suspended.attachment!==player.mediaAttachment
      ||player.wantsPlayback===false) return false;
    // Pause and Play are consecutive viewer intents. A seek, replacement, or
    // other command between them advances the generation again and retires
    // this suspension instead of reviving an older loader decision.
    if((player.controlIntentGeneration||0)!==suspended.intentGeneration+1){
      episode.establishedSuspension=null;
      return false;
    }
    const at=Math.max(0,Number(video&&video.currentTime)||0);
    try{
      episode.hls.startLoad(at);
      episode.establishedSuspension=null;
      clientLog(Object.assign({level:'info',event:'hls_loader_resumed',position:at,
        message:`resuming the established stream loader at ${at.toFixed(1)}s`},
        playbackContext()));
      return true;
    }catch(e){ return false; }
  }
  if(episode.state!=='paused') return false;
  episode.intentGeneration=player.controlIntentGeneration||0;
  if(performance.now()>=episode.deadlineMs){
    episode.state='active'; return exhaustHlsStartup(player,'paused_past_deadline');
  }
  if(episode.retry.state==='dispatched'){
    episode.state='active'; return exhaustHlsStartup(player,'corrective_reload_interrupted');
  }
  episode.state='active';
  if(episode.retry.state==='unused'){
    episode.retry.state='reserved'; episode.retry.dueMs=performance.now();
    episode.retry.detail='resume_after_startup_pause';
  }
  if(episode.retry.state!=='reserved') return false;
  episode.retry.intentGeneration=player.controlIntentGeneration||0;
  armHlsStartupRetry(video,player,episode);
  return true;
}
function createHlsStartupLoader(StockLoader,episode){
  return class PlurxStartupLoader extends StockLoader{
    constructor(config){ super(config); episode.loaders.add(this); this.plurxRequestOrdinal=0; }
    load(context,config,callbacks){
      this.plurxContext=context;
      this.plurxIntentGeneration=episode.player.controlIntentGeneration||0;
      return super.load(context,config,callbacks);
    }
    openAndSendXhr(xhr,context,config){
      if(hlsStartupManifestRequest(context)){
        const current=hlsStartupCurrent(episode.player,episode)
          &&this.plurxIntentGeneration===(episode.player.controlIntentGeneration||0)
          &&episode.player.wantsPlayback!==false;
        // Presentation retires startup accounting, not the loader. Ordinary
        // level refreshes still cross the same ownership/intent gate, but no
        // longer consume the startup request ceiling.
        if(episode.state==='presenting'){
          if(current) return super.openAndSendXhr(xhr,context,config);
          try{this.abort()}catch(e){}
          try{this.destroy()}catch(e){}
          return;
        }
        const action=PlaybackPolicy.hlsStartupSendAction({state:episode.state,
          nowMs:performance.now(),deadlineMs:episode.deadlineMs,
          dispatches:episode.dispatches,current});
        if(action!=='send'){
          if(action==='exhaust') exhaustHlsStartup(episode.player,'manifest_dispatch_limit');
          else if(action==='cancel') episode.state='cancelled';
          try{this.abort()}catch(e){}
          try{this.destroy()}catch(e){}
          return;
        }
        this.plurxRequestOrdinal=++episode.dispatches;
        episode.manifestState='loading';
        episode.requestStartedAt=performance.now();
        clientLog(Object.assign({level:'info',event:'hls_manifest_dispatch',
          ordinal:this.plurxRequestOrdinal,
          elapsed_ms:Math.round(performance.now()-episode.startedAt),
          remaining_ms:Math.max(0,Math.round(episode.deadlineMs-performance.now())),
          message:`sending manifest request ${this.plurxRequestOrdinal}`},playbackContext()));
      }
      return super.openAndSendXhr(xhr,context,config);
    }
    readystatechange(){
      const xhr=this.loader,context=this.context;
      if(xhr&&xhr.readyState===4&&hlsStartupManifestRequest(context)
        &&hlsStartupCurrent(episode.player,episode)){
        xhr._plurxStartupObserved=true;
        if(xhr.status>=400){
          const evidence={
            attachment:episode.player.mediaAttachment,resource:'manifest',
            request_ordinal:this.plurxRequestOrdinal};
          const failure=noteStreamFailure(xhr.status,streamFailureResponseBodyNow(xhr),evidence);
          if(!failure) observeStreamFailureResponse(xhr,evidence,()=>
            hlsStartupCurrent(episode.player,episode)
            &&this.plurxIntentGeneration===(episode.player.controlIntentGeneration||0)).catch(()=>{});
          episode.latestFailure=failure||{status:xhr.status,code:null,
            message:(xhr.status===401||xhr.status===403)
              ?'Sign in again before retrying playback.'
              :`The manifest request failed with HTTP ${xhr.status}.`,
            at:Date.now(),attachment:episode.player.mediaAttachment,
            request_ordinal:this.plurxRequestOrdinal};
          if(PlaybackPolicy.hlsStartupResponseAction({status:xhr.status,
            code:failure&&failure.code})==='terminal'){
            STREAM_FAILURE=episode.latestFailure;
            clearTimeout(this.requestTimeout); xhr.onreadystatechange=null; xhr.onprogress=null;
            const callbacks=this.callbacks;
            if(callbacks) callbacks.onError({code:xhr.status,text:xhr.statusText},context,xhr,this.stats);
            return;
          }
        }
      }
      return super.readystatechange();
    }
    destroy(){ episode.loaders.delete(this); return super.destroy(); }
  };
}
// One application retry per attachment, shared by initial-manifest and
// established-stream network recovery. An unloaded manifest must be reloaded;
// startLoad has no levels to start in that state.
function scheduleHlsNetworkRetry(video,player,detail){
  if(!player||player.hls==null||playbackAttemptTerminallyStopped(player,player.mediaAttachment))
    return false;
  const episode=player.hlsStartup;
  if(episode&&hlsStartupCurrent(player,episode)
    &&['active','presenting'].includes(episode.state)){
    if(episode.retry.state!=='unused') return false;
    episode.retry.state='reserved';
    player.hlsRetryUsed=1;
    episode.retry.detail=String(detail&&detail.details||detail||'network');
    episode.retry.dueMs=performance.now()+PlaybackPolicy.HLS_RETRY.delay_ms;
    episode.retry.intentGeneration=player.controlIntentGeneration||0;
    armHlsStartupRetry(video,player,episode);
    return true;
  }
  if(!PlaybackPolicy.hlsRetryAllowed({used:player.hlsRetryUsed||0})) return false;
  player.hlsRetryUsed=(player.hlsRetryUsed||0)+1;
  const hls=player.hls,attempt=player.attemptId||null,attachment=player.mediaAttachment;
  setTimeout(()=>{
    if(!PLAYER||PLAYER!==player||PLAYER.hls!==hls||PLAYER.attemptId!==attempt
      ||playbackAttemptTerminallyStopped(player,attachment)) return;
    const at=Math.max(0,Number(video&&video.currentTime)||0);
    try{hls.startLoad(at)}catch(e){}
  },PlaybackPolicy.HLS_RETRY.delay_ms);
  return true;
}
function attachHls(video, playlistUrl, startAt){
  const attachedPlayer=PLAYER;
  rememberPlaybackTransportIntent(video,attachedPlayer);
  attachedPlayer.internalMediaReset=true;
  pausePlaybackInternally(video);
  // Always tear the incumbent down first. Overwriting PLAYER.hls used to strand
  // the old instance: nothing else held a reference, but hls.js keeps polling a
  // media playlist that never gets #EXT-X-ENDLIST, so it lived on invisibly,
  // re-fetching every few seconds forever. Each seek and audio switch added
  // another. Also cancels the previous session's segment fetches, which is the
  // client half of the server's supersede.
  teardownHls();
  const attachment=beginPlaybackMediaAttachment(attachedPlayer);
  // A new stream owns its own explanations. Whatever the last one was refused
  // for is now stale, and a stale reason is worse than no reason.
  clearStreamFailure();
  // …and its own retry budget. ONE `startLoad` per attach, shared between the
  // network-class fatal and the `segment_503_not_yet` row (M5 §4.6.2): the
  // budget is per ATTACH, not per source, so two refusals of different kinds
  // cannot produce two retries. Reset here because this is the attach.
  attachedPlayer.hlsRetryUsed=0;
  if(!preferNativeHls(video) && window.Hls && Hls.isSupported()){
    // Buffer targets, in SECONDS, and deliberately not in bytes.
    //
    // An earlier version of this raised maxBufferSize to 400MB on the theory
    // that hls.js's 60MB default was a hard cap binding before the 30s target
    // — which would have made 4K unable to hold more than ~10s. That was
    // backwards. hls.js computes its target as
    //   min(max(8*maxBufferSize/bitrate, maxBufferLength), maxMaxBufferLength)
    // so maxBufferLength is a FLOOR the byte value can only extend, never
    // undercut: the stock config was already targeting 30s of 4K. What
    // actually bounds a big forward buffer is the browser's own MSE quota,
    // which varies by platform, device memory class, and version, and which
    // no hls.js setting can raise — it arrives as BUFFER_FULL_ERROR and
    // hls.js shrinks its own target in response.
    //
    // So: tune seconds, and move the byte target WITH them rather than leaving
    // it at the stock 60 MB — hls.js takes the larger of the two, so a stock
    // byte target is a floor under the seconds, not a cap on them (see
    // applyBufferTargets). backBufferLength is bounded (hls.js keeps
    // everything by default) because the server already prunes played-past
    // segments and a two-hour 4K session would otherwise grow the tab's memory
    // for the whole film.
    const tgt=bufferTargets(PLAYER&&PLAYER.bufSegSecs);
    PLAYER.bufTarget=tgt;
    const startup={player:attachedPlayer,attachment,playlistUrl,
      startAt:Math.max(0,Number(startAt)||0),hls:null,state:'active',
      manifestState:'unknown',mediaLoaded:false,decoderFailed:false,
      startedAt:performance.now(),
      deadlineMs:performance.now()+PlaybackPolicy.HLS_STARTUP.cold_deadline_ms,
      dispatches:0,loaders:new Set(),latestFailure:null,
      intentGeneration:attachedPlayer.controlIntentGeneration||0,
      evidenceOrdinal:0,establishedSuspension:null,
      mediaAttachment:attachedPlayer.mediaAttachment,
      retry:{state:'unused',dueMs:null,detail:null,timer:null,intentGeneration:null}};
    attachedPlayer.hlsStartup=startup;
    const observesCurrent=()=>attachment.current()&&attachedPlayer.hls===hls
      &&playbackOwnsAttachedMedia(attachedPlayer)
      &&!playbackAttemptTerminallyStopped(attachedPlayer,startup.mediaAttachment);
    const StockLoader=Hls.DefaultConfig&&Hls.DefaultConfig.loader;
    const hls=new Hls({
      maxBufferLength:tgt.fwd,
      backBufferLength:tgt.back,
      ...(tgt.budgeted?{maxBufferSize:tgt.fwdBytes}:{}),
      ...(StockLoader?{loader:createHlsStartupLoader(StockLoader,startup)}:{}),
      manifestLoadPolicy:PlaybackPolicy.HLS_STARTUP.manifest_load_policy,
      // HLS may be immutable VOD or the bounded sliding recovery presentation.
      // Both use the same finite fragment retry budget: rolling publication
      // advertises only completed objects and keeps removed URLs readable
      // through Grace, so an unbounded client retry would hide a real terminal
      // retirement rather than make a late object safer.
      fragLoadPolicy:vodClientContract().fragLoadPolicy,
      // Told before the first fragment loads, not seeked afterwards. Seeking
      // after attach downloads the opening of the film and throws it away —
      // on a 4K cache hit that is tens of megabytes and several seconds of
      // the viewer watching a spinner to arrive where they already were.
      startPosition:(startAt>0?startAt:-1),
      // `load` rather than a property assignment: hls.js installs its own
      // onreadystatechange *after* this hook runs, so anything set there is
      // overwritten — an added listener coexists with it and sees the body
      // hls.js is about to throw away.
      xhrSetup:x=>{
        if(TOKEN) x.setRequestHeader("authorization","Bearer "+TOKEN);
        x.addEventListener("load",()=>{
          if(!observesCurrent()||x.status<400||x._plurxStartupObserved) return;
          const intentGeneration=attachedPlayer.controlIntentGeneration||0;
          const evidence={attachment:attachedPlayer.mediaAttachment,
            intent_generation:intentGeneration,
            resource:/\.m3u8(?:\?|$)/i.test(String(x.responseURL||''))?'manifest':'media',
            request_ordinal:++startup.evidenceOrdinal};
          observeStreamFailureResponse(x,evidence,()=>observesCurrent()
            &&attachedPlayer.controlIntentGeneration===intentGeneration).catch(()=>{});
        });
      }});
    const estimateSeed=PlaybackPolicy.bandwidthSeedBps({
      outgoingEstimateBps:PLAYER&&PLAYER.bandwidthSeedBps,
      priorKbps:PLAYER&&PLAYER.priorKbps
    });
    if(estimateSeed){
      // hls.js exposes this setter so a restart does not throw away the EWMA
      // it just paid to learn. A server prior seeds only the first instance;
      // an outgoing live estimate always wins after that.
      try{ hls.bandwidthEstimate=estimateSeed; }catch(e){}
      PLAYER.bandwidthSeedBps=estimateSeed;
    }
    startup.hls=hls;
    PLAYER.hls=hls;
    hls.loadSource(playlistUrl);
    hls.attachMedia(video);
    if(Hls.Events.MANIFEST_LOADING) hls.on(Hls.Events.MANIFEST_LOADING,()=>{
      if(observesCurrent()) startup.manifestState='loading';
    });
    if(Hls.Events.MANIFEST_LOADED) hls.on(Hls.Events.MANIFEST_LOADED,()=>{
      if(observesCurrent()) startup.manifestState='loaded';
    });
    resetPlaybackTransportEvents(video);
    // A subtitle chosen before the rendition list arrived is dropped on the
    // floor: `hls.subtitleTrack = n` with no tracks yet sets nothing, and
    // nothing re-applies it. That is one of the ways a viewer selects a
    // subtitle, sees no error, and gets no cues — and it is most likely on
    // the pre-play selection, which is applied at the moment the session
    // opens. Re-apply on the edge where the list becomes real.
    if(Hls.Events.SUBTITLE_TRACKS_UPDATED) hls.on(Hls.Events.SUBTITLE_TRACKS_UPDATED,()=>{
      if(!observesCurrent()) return;
      const p=attachedPlayer;
      if(!p||p.burnedSub!=null||!(p.curSub>=0)) return;
      const ordinal=nativeHlsSubtitleOrdinal(p,p.curSub);
      if(ordinal<0) return;
      try{ if(hls.subtitleTrack!==ordinal) hls.subtitleTrack=ordinal; }catch(err){}
    });
    hls.on(Hls.Events.MANIFEST_PARSED,()=>{
      if(observesCurrent()) startup.manifestState='parsed';
      // A newer native seek still belongs to this attachment. It must not
      // suppress the only play request when the manifest finally arrives.
      if(attachment.current()) applyPlaybackTransportIntent(video,attachedPlayer);
    });
    // Timestamps for the hitch detector to blame things on. A count of hitches
    // says the picture stuttered; only an attribution says which subsystem to
    // fix, and these three are the only things happening on a cadence that
    // could plausibly produce one every few seconds:
    //   flush   hls.js asking the browser to DROP buffered video. It does this
    //           to honour backBufferLength and again whenever the quota bites,
    //           and a removal that lands near the playhead is visible.
    //   frag    a segment boundary — 1.75s apart on this source.
    //   append  new data handed to the SourceBuffer.
    if(Hls.Events.BUFFER_FLUSHING) hls.on(Hls.Events.BUFFER_FLUSHING,(_,d)=>{
      if(!observesCurrent()) return;
      markEvent("flush");
      const p=PLAYER; if(!p||!p.marks) return;
      // How far BEHIND the picture the eviction ended. A few seconds is the
      // interesting case: that is the browser being asked to free memory in
      // the region the decoder is still working in.
      const end=d&&isFinite(d.endOffset)?d.endOffset:null;
      p.marks.flushEdge=end==null?null:+((video.currentTime||0)-end).toFixed(1);
    });
    if(Hls.Events.FRAG_CHANGED) hls.on(Hls.Events.FRAG_CHANGED,()=>{
      if(observesCurrent()) markEvent("frag");
    });
    // The playlist is the only place the real append granularity is written
    // down. TARGETDURATION tracks the longest segment published so far, so a
    // session that later cuts a longer one re-tunes again.
    if(Hls.Events.LEVEL_LOADED) hls.on(Hls.Events.LEVEL_LOADED,(_,d)=>{
      if(!observesCurrent()) return;
      // A retry that loaded the playlist succeeded. Its earlier 503 is stale
      // now, not after an arbitrary wall-clock TTL; otherwise an unrelated
      // later fatal is confidently mislabeled "Still preparing".
      clearStreamFailureFor(hls);
      const t=d&&d.details&&d.details.targetduration;
      if(t>0) retuneBuffer(t);
      // Feed the reload keeper below: when the playlist last arrived, and
      // whether it is still growing.
      PLAYER._levelAt=performance.now();
      PLAYER._levelLive=!!(d&&d.details&&d.details.live);
    });
    // The largest segment this session has actually appended, in BYTES.
    //
    // Every other figure available before this one is a derivation — seconds
    // times an average bitrate, or a server-side ceiling — and the derivation
    // was wrong by 2x on a real film, because a long segment is long exactly
    // when the picture is cheap. This is the number the quota is charged in,
    // read from the transfer that just happened.
    if(Hls.Events.FRAG_LOADED) hls.on(Hls.Events.FRAG_LOADED,(_,d)=>{
      if(!observesCurrent()) return;
      startup.mediaLoaded=true;
      const stats=d&&((d.frag&&d.frag.stats)||d.stats);
      const b=stats&&(stats.total||stats.loaded);
      const p=PLAYER; if(!p||p.hls!==hls||!(b>0)) return;
      const loading=stats.loading||{};
      const sampleKbps=d.frag&&d.frag.duration>0
        ? PlaybackPolicy.transferSampleKbps({
            loadedBytes:stats.loaded||b,
            loadingStartMs:loading.start,
            loadingEndMs:loading.end
          })
        : null;
      if(sampleKbps&&p.abr){
        p.abr.recentEstimateKbps=sampleKbps;
        p.abr.recentEstimateAtMs=performance.now();
      }
      if(b<=(p.segBytes|0)) return;
      p.segBytes=b;
      retuneBuffer(p.bufSegSecs||((d.frag.duration>0)?d.frag.duration:1), true);
    });
    if(Hls.Events.BUFFER_APPENDED) hls.on(Hls.Events.BUFFER_APPENDED,()=>{
      if(observesCurrent()) markEvent("append");
    });
    // Buffer pressure, classified. "It stopped buffering" has four unrelated
    // causes and they need different fixes: the browser refusing more data
    // (quota — the ceiling we cannot raise), an append that failed for another
    // reason, a decode error, or simply nothing arriving. Reporting them as
    // one event would leave the M0 buffer question unanswerable.
    hls.on(Hls.Events.ERROR,(_,d)=>{
      if(!observesCurrent()) return;
      // Counted here, but a FATAL one still falls through to the rescue below:
      // hls.js gives up on a segment after `appendErrorMaxRetry`, and a buffer
      // failure that has become terminal is a dead player, not a statistic.
      if(d.details===Hls.ErrorDetails.BUFFER_FULL_ERROR){ reportBufferLimit("quota", d); if(!d.fatal) return; }
      if(d.details===Hls.ErrorDetails.BUFFER_APPEND_ERROR){ reportBufferLimit("append", d); if(!d.fatal) return; }
      if(!d.fatal){
        // The stall family names its mechanism — bufferStalledError,
        // bufferNudgeOnStall, bufferSeekOverHole — and this handler used to
        // drop every non-fatal on the floor. A deterministic 3-second stop
        // at 12.5 s emitted its own explanation into this function on every
        // play, and nothing kept it. First three per kind per session: the
        // first occurrence is the diagnosis, the count is the pattern, and
        // the thousandth would bury both.
        const det=String(d.details||"");
        if(/stall|nudge|hole|gap/i.test(det)){
          const p=PLAYER;
          if(p){
            if(/bufferstallederror/i.test(det)){
              const stallRunway=bufferRunway(video);
              noteAutoStall(p,
                stallRunway<SUPPLY_RUNWAY_SECS?'supply':'decode',p.waitAt);
            }
            p._hlsEvt=p._hlsEvt||{};
            const n=(p._hlsEvt[det]=(p._hlsEvt[det]||0)+1);
            if(n<=3) clientLog(Object.assign({level:"warn",event:"player_event",
              detail:det,
              message:`hls.js ${det}${d.reason?` — ${d.reason}`:""} at ${(video.currentTime||0).toFixed(1)}s (#${n})`},
              playbackContext(), {runway:bufferRunway(video)}));
          }
        }
        return;
      }
      console.warn("[cinema] hls.js fatal",d.type,d.details);
      const hlsFailure=playbackControlHlsFatal(d,!!(PLAYER&&PLAYER.started));
      const isMedia=hlsFailure.media_failure;
      if(isMedia) startup.decoderFailed=true;
      const controlTrigger=notifyPlaybackControl("failed",hlsFailure.observation);
      clientLog(Object.assign({level:"error",event:"hls_fatal",detail:d.type,message:d.details,
        control_trigger:controlTrigger}, playbackContext()));
      if(finishStallRecovery("failed",String(d.details||d.type||"hls.js fatal error"))){
        showStallRecoveryFailure("hls.js could not resume the stream ("+d.details+").");
        return;
      }
      // The rescue the progressive path has had all along.
      //
      // On this transport the <video> element's own `error` never fires for a
      // media failure: hls.js owns the MediaSource, consumes the failure, and
      // reports it here — so a copy stream this browser's decoder will not
      // take used to end at a toast with the player dead. A fatal media/codec
      // error IS terminal (hls.js retries nothing further unless the app calls
      // recoverMediaError, and a decoder that refused these samples refuses
      // them again), so restart the playback as a transcode at position.
      //
      // Same guards as the <video> rescue, and for the same reasons: only a
      // copy path is rescuable — a transcode already IS the fallback, and
      // re-opening one on its own error is the infinite loop — and only once
      // per item, via the `triedFallback` flag those two share. It lives on
      // PLAYER, which a seek mutates in place and a new item replaces, so
      // "once" means once per item and not once per stream. Network fatals are
      // deliberately left alone: a different encode does not fix an
      // unreachable server, and re-encoding a title because the wifi dropped
      // is the "downgrade on a guess" this whole change refuses to do.
      const fallback=PLAYER && PlaybackPolicy.fallbackAction({
        method:PLAYER.method,
        alreadyTried:PLAYER.triedFallback,
        playbackIsReal:false,
        mediaFailure:isMedia
      });
      if(fallback==='transcode'){
        PLAYER.triedFallback=true;
        const rejection=streamRejectionFacts();
        // `isMedia` is hls.js's own classification, and it is exactly the
        // question the profile sentence needs: a network fatal is not the
        // profile whatever the session was handed.
        const note=streamRejectionNote(rejection,PLAYER.method,d.details,isMedia);
        clientLog(streamRejectionReport(rejection,{detail:d.type,
          message:streamRejectionMessage(rejection,PLAYER.method,d.details,isMedia),
          control_trigger:controlTrigger}));
        raisePlaybackSurface("owner_recovery_step",{
          title:"Stream rejected — switching to transcode…",detail:note});
        startTranscodeFallback("stream-rejected",note);
        return;
      }
      // Say what the server said. This used to be "the server couldn't build
      // this stream — see Settings → Logs" for every cause alike: a producer
      // that exited non-zero, a session superseded by another device, an
      // ffmpeg build with no `subtitles` filter, and — the case that made this
      // a defect rather than a wording problem — a session the server was
      // still successfully starting. The refusal body names which; the
      // overlay is only useful if it repeats that instead of sending someone
      // to a log file on a headless box.
      const startupFailure=hlsStartupIncomplete(PLAYER)&&PLAYER.hlsStartup.latestFailure;
      if(startupFailure&&PlaybackPolicy.hlsStartupResponseAction(startupFailure)==='terminal')
        STREAM_FAILURE=startupFailure;
      const explained=currentStreamFailureOverlay();
      // The class decides whether the picture stops. hls.js has called
      // stopLoad() and the element keeps playing whatever it buffered — up to
      // half a minute of it — so a refusal that is still "not yet" gets an
      // indicator over a moving picture rather than the full screen that used
      // to sit there calling it a failed start (§2.1, closed by §3.3 rows 9-12).
      const where=hlsStartupIncomplete(PLAYER)?"start":PLAYER&&PLAYER.started?"attached":"start";
      const refusal=explained?PlaybackPolicy.classifyStreamFailure({
        status:STREAM_FAILURE.status,code:STREAM_FAILURE.code,context:where}):null;
      if(explained){
        clientLog(Object.assign({level:"error",event:"stream_refused",
          detail:STREAM_FAILURE.code||String(STREAM_FAILURE.status),
          message:explained.detail,control_trigger:controlTrigger}, playbackContext()));
      }
      if(refusal){
        // The site's own decision, not the table's (§3.4).
        const spent=playbackSurfaceSourceIsBlocking(refusal,where);
        if(spent) stopPlayerForExhaustion();
        raisePlaybackSurface(refusal,{context:where,player_stopped:spent,
          title:explained.title,detail:explained.detail,
          position_ms:STREAM_FAILURE.position_ms==null?null:STREAM_FAILURE.position_ms});
        // A "not yet" the server explained is the other half of M5's shared
        // hls.js budget: the picture is still moving, so one reload at the
        // current position is worth a try before the reopen path runs.
        if(explained.retryable
          &&PlaybackPolicy.hlsStartupResponseAction(STREAM_FAILURE)==='retry'
          &&(refusal==="segment_503_not_yet"||hlsStartupIncomplete(PLAYER)))
          scheduleHlsNetworkRetry(video,PLAYER,d);
        toast(explained.retryable?"Still starting…":"Playback failed");
        return;
      }
      // A NETWORK fatal is not the owner out of rungs. hls.js has called
      // stopLoad() and the element still holds everything it buffered — up to
      // half a minute of it — and `persistentWait`'s reopen, with its budget
      // untouched, is the thing that fixes exactly this. Stopping here would
      // throw the buffer away AND tear down the detector that would have used
      // it, so §3.0 does not authorise it: the owner has plenty left to try.
      // Row 13, and the picture keeps playing under an indicator.
      if(!isMedia){
        raisePlaybackSurface("owner_recovery_step",{context:where,
          title:"Reconnecting…",
          detail:"the stream connection failed ("+d.details+") — recovering"});
        // M5: one bounded reload at the current position before the reopen
        // path. The classification above is untouched — this is a retry INSIDE
        // `recovering`, not a new class.
        scheduleHlsNetworkRetry(video,PLAYER,String(d.details||d.type||"network"));
        toast("Reconnecting…");
        return;
      }
      // A media/codec fatal with the rescue ladder spent: a decoder that
      // refused these samples refuses them again, and there is no rung left.
      // Contract §3.4: the owner stops the player, then raises.
      stopPlayerForExhaustion();
      raisePlaybackSurface("owner_stopped",{context:where,player_stopped:true,
        title:explained?explained.title:"Playback failed to start ("+d.details+").",
        detail:explained?explained.detail:"the server couldn't build this stream — see Settings → Logs"});
      toast(explained?"Playback failed":"Playback failed ("+d.details+")");
    });
  } else {
    // Native HLS: the session id in the URL is the credential, so the
    // Apple TV can fetch segments itself when AirPlaying.
    PLAYER.segSrc=tok(playlistUrl); PLAYER.segTimes=null; PLAYER._segIdx=null;
    refreshSegTimes();
    setPlaybackMediaSource(video,tok(playlistUrl));
    const go=()=>{ video.removeEventListener("loadedmetadata",go);
      applyPlaybackAttachmentPosition(video,attachedPlayer,attachment,startAt); };
    video.addEventListener("loadedmetadata",go);
    applyPlaybackTransportIntent(video,attachedPlayer);
  }
}
