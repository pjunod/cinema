"use strict";
// ---- M3: measuring the prepared switch ------------------------------------
//
// Every reading below is a GETTER on an element the page already owns, taken
// on a tick the player already runs. Nothing here starts a timer, nothing
// reads a node the commit path does not already touch, and nothing is awaited
// on the commit path -- which is the only way an instrument can be trusted not
// to be the thing it measures. The arithmetic itself lives in
// playback-control.js as pure functions, so the windows and thresholds are
// unit-tested rather than inspected.
//
// A commit spans two elements, each with its own cumulative
// `droppedVideoFrames`. `p.switchFrames` therefore keeps samples tagged with
// the element they came from, and the reducer adds the predecessor's last two
// seconds to the successor's first two.
const SWITCH_FRAME_SAMPLES_MAX=32;
function samplePreparedSwitchFrames(p,v){
  if(!p||!v) return;
  let dropped=null;
  try{
    const q=typeof v.getVideoPlaybackQuality==="function"?v.getVideoPlaybackQuality():null;
    if(Number.isFinite(q&&q.droppedVideoFrames)) dropped=Math.max(0,Math.round(q.droppedVideoFrames));
  }catch(e){ return; }
  if(dropped==null) return;
  const samples=p.switchFrames||(p.switchFrames=[]);
  samples.push({at:performance.now(),count:dropped,element:v});
  while(samples.length>SWITCH_FRAME_SAMPLES_MAX) samples.shift();
}
// The moment the successor took the picture, and which element was on either
// side of it. Recorded, not acted on.
function notePreparedSwitchCommit(p,predecessor,successor){
  if(!p) return;
  p.switchCommit={at:performance.now(),predecessor,successor,
    tappedAt:(p.directedChange&&p.directedChange.tappedAt)??null,firstFrameAt:null};
}
function notePreparedSwitchFirstFrame(p){
  if(p&&p.switchCommit&&p.switchCommit.firstFrameAt==null)
    p.switchCommit.firstFrameAt=performance.now();
}
// The three ledger rows, as strings. Called only when the info panel is being
// painted, so a closed panel costs nothing at all.
function preparedSwitchLedger(p){
  const control=window.PlurxPlaybackControl;
  const commit=p&&p.switchCommit;
  if(!control||!commit) return {frames:null,audio:null,visible:null};
  const samples=p.switchFrames||[];
  const delta=control.preparedSwitchCounterDelta({
    before:samples.filter(sample=>sample.element===commit.predecessor),
    after:samples.filter(sample=>sample.element===commit.successor),
    commitAtMs:commit.at});
  return {
    frames:control.preparedSwitchFramesRow(delta),
    // Deliberately not a measurement. See `preparedSwitchAudioEnable`: the
    // only web instrument for an audible seam captures the element's audio
    // permanently, and an instrument that can silence the thing it measures
    // does not ship on the audible path.
    audio:"Not measured · analyser would capture this element's audio for its lifetime",
    visible:control.preparedSwitchVisibleRow(
      control.preparedSwitchVisibleInMs(commit.tappedAt,commit.firstFrameAt)),
  };
}
// Advisory enablement for the one measurement this client does not take.
//
// `MediaElementAudioSourceNode` re-routes an element's audio into an
// `AudioContext` graph for the REST OF THE ELEMENT'S LIFE -- the capture
// cannot be undone -- and the sound then reaches the speakers only while that
// graph is running and connected. A suspended context, a cross-origin-tainted
// response, or a graph torn down with the modal all end in silence on a path
// the viewer is listening to. So the conditions are stated and the probe is
// not built; nothing here blocks anything, because there is nothing to block.
function preparedSwitchAudioEnable(){
  return [
    ["A resumed AudioContext for the whole session",
     "Required. An analyser can only observe audio it is routed, and a context suspended by the autoplay policy routes silence.",false],
    ["Reversible capture of the media element",
     "Not available in any browser. createMediaElementSource permanently moves the element's output into the graph, so a failure anywhere in the graph is a silent player.",false],
    ["Same-origin or CORS-approved media",
     "Required. Tainted media yields zeroed samples, which this detector would read as a seam that never happened.",false],
  ];
}

// First-frame proof makes the switch irreversible. Until this function runs,
// both the old hls.js instance and its media element are deliberately alive.
function retirePreparedPredecessor(p,state){
  const predecessor=state&&state.predecessor;
  if(!predecessor) return;
  state.predecessor=null;
  destroyHlsInstance(p,predecessor.hls,predecessor.element);
  disposeRetiredMediaElement(predecessor.element);
}

// Restore the last proven picture after the visible successor renders no
// frame. This completes before `failed` is queued, so the acknowledgement is
// an honest description of a recovered viewer rather than a black player.
function rollbackPreparedReplacement(p,state,successor){
  const predecessor=state&&state.predecessor;
  if(!p||!predecessor||!predecessor.element||!successor) return false;
  const retired=predecessor.element, successorHls=state.hls;
  state.predecessor=null;
  p.hls=predecessor.hls;
  p.sessionId=predecessor.sessionId;
  p.probeUrl=predecessor.probeUrl;
  p.offset=predecessor.offset;
  p.wantsPlayback=predecessor.wantsPlayback;
  p.health=predecessor.health;
  p.healthObservedAt=predecessor.healthObservedAt;
  p.presentationAdvancedAt=predecessor.presentationAdvancedAt;
  successor.id="";
  retired.id="video";
  successor.id="video-prepared";
  retired.style.display="";
  retired.muted=predecessor.muted;
  try{
    retired.volume=predecessor.volume;
    retired.defaultPlaybackRate=predecessor.defaultPlaybackRate;
    retired.playbackRate=predecessor.playbackRate;
  }catch(e){}
  retired.removeAttribute("aria-hidden");
  successor.style.display="none";
  successor.muted=true;
  successor.setAttribute("aria-hidden","true");
  adoptPlaybackMediaElement(p,retired);
  resetPlaybackTransportEvents(retired);
  if(predecessor.wantsPlayback===false){
    try{ retired.pause(); }catch(e){}
  }else{
    try{ const resumed=retired.play(); if(resumed&&resumed.catch) resumed.catch(()=>{}); }catch(e){}
  }
  destroyHlsInstance(null,successorHls,successor);
  state.hls=null;
  disposeRetiredMediaElement(successor);
  renderPlayerInfo();
  return true;
}
// Move every per-element binding onto the element that now owns the picture.
//
// `wirePlayerMedia` carries the listener set, and `play()` binds five more
// things to the element it captured — the ended handler, the progress tick, the
// hitch detector, the decode probe and the AirPlay button. A switch that
// changed which element is `#video` and left those behind would produce a
// player that looks fine and has no scrubber, no stall detection, no decode
// rescue and no autoplay-next, until the page is reloaded.
function adoptPlaybackMediaElement(p,v){
  if(!p||!v) return;
  wirePlayerMedia(v);
  try{ v.onended=()=>{ handleEnded(p.fileId).catch(()=>{}); }; }catch(e){}
  try{ v.addEventListener("loadedmetadata",probeDecode,{once:true}); }catch(e){}
  armHitchDetector(v);
  setupAirplay(v);
  clearInterval(p.progressTimer);
  p.progressTimer=setInterval(()=>playbackSamplingTick(v,p),500);
  // The subtitle selection is element state — a `<track>`, a script cue list,
  // or an hls.js rendition index — and none of it followed the switch, so it
  // has to be re-applied to the element that now owns the picture.
  //
  // This used to clear `p._subOff` and call `pbTick()`, with a comment saying
  // that was what triggered the re-apply. It is the exact opposite: the tick's
  // guard requires `_subOff != null`, so nulling it is the one value that makes
  // the guard fail, and the `pbTick()` on the next line then short-circuited.
  // After any element swap — a prepared-replacement commit — a text subtitle
  // silently disappeared and never came back, because this tick is the only
  // path that re-applies one.
  //
  // Re-apply directly instead of routing it through a marker the tick reads
  // for a different purpose. `setSub` is idempotent for the current index and
  // sets `_subOff` itself, so the tick's own "the offset moved, re-shift the
  // cues" job keeps working afterwards.
  if(p.curSub>=0) setSub(p.curSub);
  else p._subOff=null;
  pbTick(); pbSyncPlayIcon();
}
// The retired element leaves the page.
//
// It cannot simply be kept as the next successor's host: it carries the whole
// listener set from its time as `#video`, so restaging it would have a hidden
// pipeline dismissing the visible stream's loading overlay, raising its
// "Buffering…" spinner and reporting its own errors as the viewer's. Dropping
// the node drops the listeners with it, and `ensurePreparedVideoElement` builds
// a clean one when a successor is next staged.
function disposeRetiredMediaElement(retired){
  if(!retired) return;
  try{ retired.pause(); }catch(e){}
  try{ retired.removeAttribute("src"); retired.load(); }catch(e){}
  try{ retired.onended=null; }catch(e){}
  try{ if(retired.parentNode) retired.parentNode.removeChild(retired); }catch(e){}
}

function preparedFirstFrame(p,state,element,onFrame,onTimeout){
  let settled=false;
  state.frameCancelled=false;
  state.frameElement=element;
  const finish=(run)=>{
    if(settled||state.frameCancelled) return;
    settled=true;
    if(state.frameTimer!=null){ clearTimeout(state.frameTimer); state.frameTimer=null; }
    if(state.frameListener){
      try{ element.removeEventListener("timeupdate",state.frameListener); }catch(e){}
      state.frameListener=null;
    }
    if(state.framePollTimer!=null){ clearTimeout(state.framePollTimer); state.framePollTimer=null; }
    state.frameCallbackId=null;
    state.frameElement=null;
    run();
  };
  state.frameTimer=setTimeout(()=>finish(onTimeout),PREPARED_FIRST_FRAME_MS);
  const done=()=>finish(()=>onFrame(Date.now()));
  // Prefer the frame callback. Older engines expose a monotonic presented or
  // decoded-frame counter instead; an increment after the local switch is
  // still an observation of video progress, unlike metadata, `playing`, or a
  // moving media clock. The finite outer timer owns engines with neither.
  if(streamHasVideo(p,element)){
    if(typeof element.requestVideoFrameCallback==="function"){
      try{ state.frameCallbackId=element.requestVideoFrameCallback(()=>done()); return; }catch(e){}
    }
    const frameCount=()=>{
      try{
        const quality=typeof element.getVideoPlaybackQuality==="function"
          ?element.getVideoPlaybackQuality():null;
        const count=Number(quality?.totalVideoFrames??element.webkitDecodedFrameCount);
        return Number.isFinite(count)?count:null;
      }catch(e){ return null; }
    };
    const initial=frameCount();
    if(initial==null) return;
    const observe=()=>{
      if(settled||state.frameCancelled) return;
      const current=frameCount();
      if(current!=null&&current>initial){ done(); return; }
      state.framePollTimer=setTimeout(observe,50);
    };
    observe();
    return;
  }
  state.frameListener=()=>done();
  element.addEventListener("timeupdate",state.frameListener);
}

function failPreparedReplacement(p,state,detail){
  if(preparedState(p)!==state) return;
  state.state="failed";
  queuePlaybackControlAcknowledgement(p,state.actionId,"failed");
  clientLog(Object.assign({level:"warn",event:"prepared_replacement",detail:"failed",
    message:`preparation of ${state.sessionId} failed: ${detail}`},playbackContext()));
  // §3.3 row 18, the same reason as the abandonment above: the successor never
  // took the picture, so nothing the viewer can see has changed.
  raisePlaybackSurface("log_only",{attached:playbackSurfaceGeneration(p)});
  freePreparedReplacement(p,state);
  // Until now this only acknowledged, logged and freed -- which was right
  // while a preparation was something the server started on its own. A
  // preparation a VIEWER asked for is owed an answer: the successor is gone
  // and they are still watching the quality they moved away from. A
  // preparation with no directed change keeps exactly the old behaviour.
  fallBackDirectedChange(p,p.directedChange,"failed");
}
// Settle and free. Every exit path runs through here — a new attach, a seek, a
// quality change, the player closing, the reporter going away — because a
// preparation nobody answered holds this session's ONLY preparation slot until
// the 330-second deadline, and that session then gets no second chance for the
// rest of its life. A leaked instance is also a second decoder running behind
// a closed modal.
function abandonPreparedReplacement(p,ackState,reason){
  const state=preparedState(p);
  if(!state) return false;
  if(!PREPARED_TERMINAL_STATES.includes(state.state)){
    queuePlaybackControlAcknowledgement(p,state.actionId,ackState||"aborted");
    clientLog(Object.assign({level:"info",event:"prepared_replacement",detail:ackState||"aborted",
      message:`preparation of ${state.sessionId} abandoned: ${reason||"no reason given"}`},
      playbackContext()));
    // §3.3 row 18. A staging nobody took up leaves the incumbent exactly where
    // it was — the viewer is watching the same picture, and there is nothing to
    // tell them — so the ledger row IS the trace, and the fence keeps it so.
    raisePlaybackSurface("log_only",{attached:playbackSurfaceGeneration(p)});
  }
  freePreparedReplacement(p,state);
  return true;
}
function freePreparedReplacement(p,state){
  if(preparedState(p)!==state) return;
  p.prepared=null;
  markPreparedSettlement(p,state.actionId);
  const spare=preparedVideoElement();
  if(state.frameTimer!=null){ clearTimeout(state.frameTimer); state.frameTimer=null; }
  if(state.framePollTimer!=null){ clearTimeout(state.framePollTimer); state.framePollTimer=null; }
  if(state.frameListener&&spare){
    try{ spare.removeEventListener("timeupdate",state.frameListener); }catch(e){}
  }
  state.frameListener=null;
  if(state.nativeListeners&&spare){
    for(const pair of state.nativeListeners) spare.removeEventListener(pair[0],pair[1]);
  }
  state.nativeListeners=null;
  // The same teardown discipline the incumbent gets, on an instance that was
  // never authoritative: destroy it, then clear the element, or hls.js keeps
  // polling a playlist nothing is watching. The player is deliberately NOT
  // passed — a pipeline that failed should not seed the incumbent's bandwidth
  // estimate with whatever it measured on its way down.
  destroyHlsInstance(null,state.hls,spare);
  state.hls=null;
  if(spare){
    try{ spare.pause(); spare.removeAttribute("src"); spare.load(); }catch(e){}
    spare.muted=true;
    spare.style.display="none";
    spare.setAttribute("aria-hidden","true");
  }
}

async function sendPlaybackControl(url,body,signal){
  const response=await fetch(url,{method:"POST",signal,
    headers:Object.assign({"content-type":"application/json"},TOKEN?{"authorization":"Bearer "+TOKEN}:{}),
    body:JSON.stringify(body)});
  if(!response.ok){
    let failure=null;
    try{ failure=await response.json(); }catch(e){}
    const code=failure&&typeof failure.code==="string"?failure.code:null;
    const invalidField=failure&&typeof failure.invalid_field==="string"
      &&/^[a-z][a-z0-9_.]{0,63}$/.test(failure.invalid_field)?failure.invalid_field:null;
    const error=new Error((code?`playback control ${code}`:"playback control HTTP "+response.status)
      +(invalidField?` (${invalidField})`:""));
    error.status=response.status; error.code=code;
    if(invalidField) error.invalidField=invalidField;
    if(failure&&typeof failure.generation==="string") error.generation=failure.generation;
    if(failure&&Number.isSafeInteger(failure.control_epoch)) error.controlEpoch=failure.control_epoch;
    if(failure&&Number.isSafeInteger(failure.retry_after_ms)) error.retryAfterMs=failure.retry_after_ms;
    throw error;
  }
  return response.json();
}
function stopPlaybackControl(p){
  // Settle first, then flush, then stop — in that order, because each step
  // destroys the means of the one before it. `abandonPreparedReplacement`
  // queues the settlement; `flushPreparedSettlement` is what actually gets it
  // onto an exchange, because the reporter's own capture refuses once the
  // player no longer owns the media; and `stop()` aborts whatever is in flight,
  // so stopping in the same turn is how a settlement is silently lost and a
  // staging left to burn its 330-second deadline.
  abandonPreparedReplacement(p,"aborted","the control reporter stopped");
  cancelPreparedFirstFrame(p);
  const reporter=p&&p.controlReporter;
  if(reporter){
    // The reporter owns this settlement until the server accepts it. Its
    // built-in retry path preserves the exact request; after a committed
    // successor is accepted, `continueStoppingPlaybackControl` explicitly
    // queues the required end exchange before allowing the reporter to stop.
    reporter.plurxSettling=true;
    reporter.plurxSettlingUntil=reporter.now()+PREPARED_FINALIZATION_MS;
    reporter.plurxSettlingTimer=reporter.setTimer(
      ()=>finishStoppingPlaybackControl(reporter),PREPARED_FINALIZATION_MS);
    const flushed=flushPreparedSettlement(p,reporter);
    p.controlReporter=null;
    if(!flushed) finishStoppingPlaybackControl(reporter);
  }
  clearPlaybackControlWaiters(p);
  if(p) p.controlAcknowledgements=[];
}
// Cancel an unproven visible switch before its reporter or media owner goes
// away. The predecessor is still the last known-good picture, so restore it
// and settle the successor as failed; the caller may then tear that restored
// pipeline down through the ordinary path.
function cancelPreparedFirstFrame(p){
  const state=p&&p.preparedCommitting;
  if(!state) return;
  p.preparedCommitting=null;
  state.frameCancelled=true;
  if(state.frameTimer!=null){ clearTimeout(state.frameTimer); state.frameTimer=null; }
  if(state.framePollTimer!=null){ clearTimeout(state.framePollTimer); state.framePollTimer=null; }
  const spare=state.frameElement||document.getElementById("video");
  if(state.frameListener&&spare){
    try{ spare.removeEventListener("timeupdate",state.frameListener); }catch(e){}
  }
  state.frameListener=null;
  if(state.frameCallbackId!=null&&spare&&typeof spare.cancelVideoFrameCallback==="function"){
    try{ spare.cancelVideoFrameCallback(state.frameCallbackId); }catch(e){}
  }
  state.frameCallbackId=null;
  state.frameElement=null;
  state.state="failed";
  rollbackPreparedReplacement(p,state,spare);
  queuePlaybackControlAcknowledgement(p,state.actionId,"failed");
}
// One last exchange for a settlement that has nowhere else to go.
//
// The reporter's `capture` returns null the moment the player stops owning the
// attached media — which is exactly the state every teardown path is in — so a
// queued `aborted` would never be built into a request. Building the capture
// explicitly is the difference between settling the staging and leaving the
// server to reap it 330 seconds later, after which that session gets no second
// preparation for the rest of its life.
function flushPreparedSettlement(p,reporter){
  if(!p||!reporter||reporter.stopped) return false;
  if(!p.controlOwner||!window.PlurxPlaybackControl) return false;
  const queue=p.controlAcknowledgements;
  if(!queue||!queue.length) return false;
  const v=document.getElementById("video");
  let snapshot=playbackControlSnapshot(v,p);
  if(!snapshot||!snapshot.acknowledgement) return false;
  // Failed and aborted settlements may close in the same exchange. A commit
  // may not; `playbackControlSnapshot` has already made that one active, and
  // the accepted callback below follows it with an acknowledgement-free end.
  if(snapshot.acknowledgement.state!=="committed")
    snapshot=endingPlaybackControlSnapshot(snapshot,true);
  const capture=PlurxPlaybackControl.capture(snapshot,p.controlIntentGeneration||0,p.controlOwner);
  return !!(capture&&reporter.notify(capture));
}

function endingPlaybackControlSnapshot(snapshot,carryAcknowledgement){
  if(!snapshot) return null;
  const ending=Object.assign({},snapshot,{demand:"end",
    playback_rate:Math.max(0,Math.min(4,Number(snapshot.playback_rate)||0))});
  if(!carryAcknowledgement) ending.acknowledgement=null;
  return ending;
}

function finishStoppingPlaybackControl(reporter){
  if(!reporter) return;
  if(reporter.plurxSettlingTimer!=null){
    reporter.clearTimer(reporter.plurxSettlingTimer);
    reporter.plurxSettlingTimer=null;
  }
  reporter.plurxSettling=false;
  reporter.plurxSettlingUntil=null;
  reporter.stop();
}

// A stopping reporter is deliberately detached from `p.controlReporter`, so
// its normal ownership callback must not publish observations or actions. It
// still owns one narrow protocol obligation: retry its immutable settlement,
// then (for a commit) send end as the next sequence. `notify` from inside the
// accepted callback becomes pending while the reporter is in flight, so its
// `finally` drains it after the 250 ms protocol floor rather than the ordinary
// cadence, which may be a full minute.
function continueStoppingPlaybackControl(p,reporter,v,request,response,captured){
  if(!reporter||!reporter.plurxSettling) return false;
  if(reporter.now()>=reporter.plurxSettlingUntil){
    finishStoppingPlaybackControl(reporter);
    return true;
  }
  if(request.demand==="end"){
    if(response) finishStoppingPlaybackControl(reporter);
    else reporter.notify(captured);
    return true;
  }
  const committed=request.acknowledgement&&request.acknowledgement.state==="committed";
  if(committed){
    const element=document.getElementById("video")||v;
    const snapshot=endingPlaybackControlSnapshot(playbackControlSnapshot(element,p),false);
    const ending=snapshot&&PlurxPlaybackControl.capture(snapshot,
      captured.intentGeneration,captured.owner);
    if(ending&&reporter.notify(ending)) return true;
  }
  // stopPlaybackControl may have queued the settlement while an earlier
  // ordinary/progress exchange was already in flight. Its callback owns no
  // stopping decision: leave the reporter alive so `finally` drains the
  // pending settlement. Only the request that actually carries commit/end may
  // complete this stopping sequence.
  if(!request.acknowledgement) return true;
  if(!committed) return true;
  // Retryable failures are classified by Reporter after this callback. Leave
  // the settling marker intact so its exact request can retry; if this was a
  // commit, the pending end capture above keeps that retry draining without
  // asking the now-detached page for a fresh capture.
  if(!response) return true;
  finishStoppingPlaybackControl(reporter);
  return true;
}

function notifyPlaybackControl(renderOverride,observationOverride){
  if(!playbackOwnsAttachedMedia(PLAYER))return null;
  const p=PLAYER;
  if(!p) return null;
  if(["stalled","failed"].includes(renderOverride)) p.controlRenderOverride=renderOverride;
  const observation=playbackControlObservationOverride(observationOverride);
  if(observation) p.controlObservationOverride=observation;
  return p.controlReporter?p.controlReporter.notify():null;
}
// How long a recovery owner waits for the server's verdict before deciding for
// itself.
//
// This was one exchange deadline, on the reasoning that waiting longer than an
// exchange can take is pure delay. That was the wrong end to measure from. The
// fallback is the branch the entire fleet takes — no node has ever answered a
// full-vocabulary exchange — so the bound is added to every real stall for
// every viewer, on top of the eight seconds `persistentWait` has already
// waited. A server that cannot answer inside a second and a half is a server
// whose answer is not worth more frozen picture than the recovery it would
// have replaced.
//
// The window is still extended once, and only when an exchange that is NOT
// this ask's completes: `drain` returns immediately while a request is in
// flight, so this ask's own request cannot start until that one finishes, and
// a single window would otherwise expire at the instant an answer first became
// possible. CONTROL_ASK_CAP_MS is the hard stop.
const CONTROL_ASK_MS=1500;
const CONTROL_ASK_CAP_MS=3000;
// The reporter's own exchange floor. Named here because the retry clamp below
// must not pace an ask faster than the transport will carry it.
const CONTROL_MIN_EXCHANGE_MS=250;
// How many times running a paced verdict may push the legacy path back before
// the client takes it anyway. A `hold` is exempt: it is not a pacing hint, it
// is a statement that reopening is pointless, and it earns an explanation
// instead of a bound.
const CONTROL_DEFER_LIMIT=3;
// The seven hold reasons the server can send, in the viewer's words. An
// unknown reason is shown as nothing rather than as its wire name: a viewer
// reading `working_set` learns less than a viewer reading one plain sentence.
// The wire validates `message` as a string and nothing more, while the
// outbound `error_detail` beside it is bounded and stripped. Server text lands
// in the overlay title and in the log ring, so it gets the same treatment on
// the way in. `setLoading` assigns it through textContent, so this is a
// legibility bound rather than an injection guard.
function controlVerdictText(message){
  const text=String(message==null?"":message).replace(/[\r\n\0]+/g," ").trim();
  if(!text) return "Playback stopped.";
  return text.length>160?text.slice(0,159)+"\u2026":text;
}
// A terminal answer belongs to the intent that earned it. Automatic recovery
// may carry it across a replacement of that same intent; any viewer command
// retires it and every ask that was waiting on the previous action epoch.
function supersedePlaybackControlIntent(p,{preserveHlsStartup=false}={}){
  if(!p) return 0;
  const previous=p.controlIntentGeneration||0;
  p.controlIntentGeneration=previous+1;
  // Viewer intent cancels an in-flight automatic episode. Automatic recovery
  // begins its seek with `supersedeIntent=false`, so it retains this exact
  // episode while pause, seek, track change and close fence every old callback.
  p.recoveringStall=null;
  const startup=p.hlsStartup;
  // A local text-track change fences old control replies, but does not
  // replace the video. Carry forward only work owned by the immediately
  // preceding intent: never revive a loader already fenced by pause/seek.
  const keepStartup=preserveHlsStartup&&startup&&hlsStartupCurrent(p,startup)
    &&['active','presenting','paused'].includes(startup.state);
  if(keepStartup){
    for(const loader of startup.loaders){
      if(loader.plurxIntentGeneration===previous)
        loader.plurxIntentGeneration=p.controlIntentGeneration;
    }
    if(startup.retry?.intentGeneration===previous)
      startup.retry.intentGeneration=p.controlIntentGeneration;
  }
  if(!keepStartup&&startup&&startup.retry&&startup.retry.state==='reserved'){
    clearTimeout(startup.retry.timer); startup.retry.timer=null;
  }
  p.controlVerdict=null;
  p.controlVerdictExpiresAt=null;
  clearPlaybackControlWaiters(p);
  return p.controlIntentGeneration;
}
function armedPlaybackControlVerdict(p){
  if(!p||!p.controlVerdict) return null;
  if(Number.isFinite(p.controlVerdictExpiresAt)
     &&performance.now()>=p.controlVerdictExpiresAt){
    p.controlVerdict=null;
    p.controlVerdictExpiresAt=null;
    return null;
  }
  return p.controlVerdict;
}
function holdReasonText(reason){
  return {demand:"Another player is using this stream.",
    time:"The server is pacing this stream.",
    bytes:"The server is pacing this stream.",
    global:"The server is busy.",
    ahead:"The stream is already far enough ahead.",
    working_set:"The server is short of space.",
    no_room:"The server is short of space."}[reason]||"Your place is saved.";
}
// Submit evidence, then wait — briefly — for the action it earns.
//
// `notifyPlaybackControl` tells the server. This tells the server and listens.
// The difference is the whole milestone: an owner that only reports still
// decides for itself, and every one of them decides by guessing toward retry.
//
// Returns the trigger synchronously so the caller can log it at the moment it
// observed the fault, and a promise for the action. The promise ALWAYS settles
// — with `null` when the reporter is gone, the exchange failed, or nothing
// arrived in time. A null verdict means "the server did not answer", and every
// caller must fall through to its existing behaviour on it. That fallback is
// not a hedge: a server that has not yet decided must not strand a stalled
// viewer, and no node has ever answered one of these.
function askPlaybackControl(renderOverride,observationOverride,deadlineMs=Infinity){
  const p=PLAYER;
  if(!p) return {trigger:null,action:Promise.resolve(null)};
  const reporter=p.controlReporter;
  if(!reporter||reporter.stopped){
    return {trigger:notifyPlaybackControl(renderOverride,observationOverride),
      action:Promise.resolve(null)};
  }
  // The request carrying this evidence is the next NEW one the reporter starts.
  // A replayed retry keeps its old sequence, so a floor on the sequence skips
  // the replay without skipping ours.
  //
  // The floor is only meaningful inside one owner: `resetForOwner` zeroes the
  // counter on a 409, so after an owner change a *later, unrelated* request can
  // reach this floor carrying a verdict decided for a different observation,
  // generation and epoch. If that verdict were `terminal` the viewer would read
  // a verdict this ask never asked for. So the identity is pinned too.
  const minSequence=(Number(reporter.sequence)||0)+1;
  const generation=reporter.bootstrap&&reporter.bootstrap.generation;
  const controlEpoch=reporter.bootstrap&&reporter.bootstrap.control_epoch;
  const trigger=notifyPlaybackControl(renderOverride,observationOverride);
  // A notify that enqueued nothing — an invalid snapshot — will never produce a
  // request carrying this evidence, so waiting the full window for it would add
  // the whole bound to a stall for an exchange that is not going to happen.
  if(!trigger) return {trigger:null,action:Promise.resolve(null)};
  const hardExpiry=Math.min(performance.now()+CONTROL_ASK_CAP_MS,deadlineMs);
  if(hardExpiry<=performance.now()) return {trigger,action:Promise.resolve(null)};
  if(!p.controlWaiters) p.controlWaiters=[];
  const waiters=p.controlWaiters;
  let settle=null;
  const action=new Promise((resolve)=>{
    const waiter={minSequence,generation,controlEpoch,
      hardExpiry,
      settle:(value)=>{
        if(waiter.settled) return;
        waiter.settled=true;
        clearTimeout(waiter.timer);
        const at=waiters.indexOf(waiter);
        if(at!==-1) waiters.splice(at,1);
        resolve(value||null);
      },
      // Called when an exchange that is not this ask's completes: ours is now
      // the next one, so it gets a fresh window — up to the hard cap.
      extend:()=>{
        if(waiter.settled) return;
        const left=waiter.hardExpiry-performance.now();
        if(left<=0){ waiter.settle(null); return; }
        clearTimeout(waiter.timer);
        waiter.timer=setTimeout(()=>waiter.settle(null),Math.min(CONTROL_ASK_MS,left));
      }};
    waiter.timer=setTimeout(()=>waiter.settle(null),
      Math.max(0,Math.min(CONTROL_ASK_MS,hardExpiry-performance.now())));
    waiters.push(waiter);
    settle=waiter.settle;
  });
  // A reporter that stopped between the notify and here will never exchange
  // again, so nothing would ever settle this waiter but its timer.
  if(reporter.stopped) settle(null);
  return {trigger,action};
}
