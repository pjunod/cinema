"use strict";
// ---- prepared replacement --------------------------------------------------
//
// The client half of a two-player handoff: the server stages a successor
// session, tells this client where its playlist is and what source position
// that playlist's zero maps to, and then waits. It never orders the switch —
// there is no `commit_replacement` action — so everything below is this
// client's decision, reported back as acknowledgement states.
//
// Two gates decide whether any of it ever runs, and they are independent. Gate
// A is `capabilities.dual_player_preparation`: a hardware claim, and the
// server stages nothing at all for a client that says false. Gate B is naming
// `prepare_replacement` in `supported_actions`: a vocabulary claim, which this
// build now makes unconditionally, because a client that declares it and is
// never offered a preparation behaves exactly as it did before.
//
// Settings → Developer owns Gate A for this browser. It is advisory there —
// the card lists what is measured and what is not, and whether each is
// currently met — and it does not gate the switch: an operator who wants the
// path on gets it on, and the readiness list is what tells them the cost.
const PREPARED_HANDOFF_KEY="plurx.prepared_handoff";
// How far past the incumbent's current film position the successor must be
// buffered before this client will switch. A handoff that commits at the
// playhead hands the viewer a decoder with nothing in front of it.
const PREPARED_BUFFER_LEAD_MS=4000;
// Drift the successor may carry into the switch before it is seeked onto the
// incumbent's second. Below this a corrective seek costs more than it fixes:
// it flushes the buffer that was the whole point of preparing.
const PREPARED_ALIGN_SLACK_MS=250;
// How long a corrective alignment seek may take before this client stops
// believing it will land. The successor is muted and playing, so a seek that
// completes leaves only its own latency behind; one that does not complete is
// evidence about the successor, not about the incumbent.
const PREPARED_ALIGN_SEEK_MS=1500;
// Corrective seeks per commit. The incumbent keeps moving while each one runs,
// so a second measurement is worth taking and a third is a loop.
const PREPARED_ALIGN_ATTEMPTS=2;
// A switch that never renders is a failed preparation, and the server is owed
// that answer rather than a 330-second silence.
const PREPARED_FIRST_FRAME_MS=8000;
const PREPARED_TERMINAL_STATES=["committed","failed","aborted"];
// Unsent settlements, bounded. Two stagings can be unsettled at once — a
// supersede queues the old one's `aborted` while the new one is already
// reporting progress — and more than a few means the exchanges are not
// happening at all, at which point the server's deadline has settled them.
const PREPARED_ACK_QUEUE_MAX=4;
// Stagings this client has settled, remembered so a replay of one is
// recognised rather than built again. The server replays a `Prepare`
// byte-identically until it processes the acknowledgement.
const PREPARED_SETTLED_MEMORY=8;
// A detached reporter gets one bounded chance to settle commit → end. The
// server's durable deadline owns cleanup after this; the browser must not keep
// a player closure and retry timers alive forever after the viewer leaves.
const PREPARED_FINALIZATION_MS=3000;
// Whether this browser will offer preparation for the current explicit ask.
// A failed action settles once; it never becomes a hidden session-wide gate.
function preparedHandoffOffered(p){
  return !!p&&preparedHandoffEnabled();
}
function preparedHandoffEnabled(){
  // Default on. Only a stored opt-out turns it off; a scope that refuses
  // storage cannot manufacture an opt-out the viewer never made.
  try{ return localStorage.getItem(PREPARED_HANDOFF_KEY)!=="0"; }catch(e){ return true; }
}
function setPreparedHandoffEnabled(on){
  try{ localStorage.setItem(PREPARED_HANDOFF_KEY,on?"1":"0"); }catch(e){}
}
// Source position represented by a session's own zero.
//
// Named after Android's `MediaOrigin.kt` and Apple's `sessionMediaOriginMs`
// rather than inventing a third vocabulary for the same two timelines. A VOD
// playlist is the whole immutable title, so its zero IS the source's zero; a
// live or recovery session's zero is wherever the server started cutting, and
// `media_origin_ms` is the server saying so. `start_seconds` is the older
// field that says the same thing, and is the fallback.
function sessionMediaOriginMs(session){
  if(!session||typeof session!=="object") return 0;
  if(session.vod) return 0;
  const explicit=Number(session.media_origin_ms);
  if(Number.isFinite(explicit)) return Math.max(0,Math.round(explicit));
  const seconds=Number(session.start_seconds);
  if(Number.isFinite(seconds)) return Math.max(0,Math.round(seconds*1000));
  return 0;
}
// Film time from a player's local time. The commit boundary is expressed in
// film time, so a handoff that reasons in local time lands the viewer at the
// wrong second and reads as a seek bug rather than an alignment bug.
function realMediaPositionMs(localMs,baseMs,isVod){
  const local=Number(localMs);
  if(!Number.isFinite(local)) return 0;
  const base=isVod?0:Math.max(0,Number(baseMs)||0);
  return Math.max(0,Math.round(base+local));
}
// The inverse: where a film position sits on a session whose zero is `baseMs`.
// This is the only thing that puts the second pipeline on the first's second.
function preparedLocalPositionMs(filmMs,baseMs){
  return Math.max(0,Math.round((Number(filmMs)||0)-Math.max(0,Number(baseMs)||0)));
}
// Where the incumbent is, in film time, right now.
function playbackFilmPositionMs(v,p){
  if(!v||!p) return 0;
  return realMediaPositionMs(Math.max(0,(v.currentTime||0)*1000),(p.offset||0)*1000,!!p.vod);
}
// The queued settlement, if this exchange may carry it.
//
// A `committed` may not share an exchange with `demand:"end"` — the server
// answers 400 to that pair — so a commit that collides with a close waits for
// its own exchange instead of being dropped or renamed. Switching and then
// closing is two exchanges, in that order.
function pendingPlaybackControlAcknowledgement(p,demand){
  const queue=p&&p.controlAcknowledgements;
  if(!queue||!queue.length) return null;
  // Never hide the head. `playbackControlSnapshot` turns a same-turn
  // commit+end into the required active commit exchange; hiding it here loses
  // the only durable proof that the viewer reached the successor.
  return queue[0];
}

function queuePlaybackControlAcknowledgement(p,actionId,state,extra){
  if(!p||!actionId||!state) return null;
  const queue=p.controlAcknowledgements||(p.controlAcknowledgements=[]);
  // A QUEUE and not a slot, because two stagings can be unsettled at once: a
  // second `prepare` aborts the first, and the first's `aborted` is still
  // waiting for an exchange when the second's `metadata_ready` is queued. One
  // slot loses the abort, and the staging it belonged to then holds the
  // session's only preparation slot until the 330-second deadline.
  const at=queue.findIndex(entry=>entry.action_id===actionId);
  if(at!==-1){
    const pending=queue[at];
    // A terminal settlement outranks a progress one: a `failed` must not be
    // overwritten by a `metadata_ready` that was already in flight when the
    // pipeline died.
    if(PREPARED_TERMINAL_STATES.includes(pending.state)
      &&!PREPARED_TERMINAL_STATES.includes(state)) return pending;
    queue.splice(at,1);
  }
  const ack=Object.assign({action_id:actionId,state},extra||{});
  queue.push(ack);
  // Bounded, because an unsent queue is a leak with a wire cost. The oldest
  // goes first: a settlement nobody has managed to send in four preparations
  // is one the server's deadline has already reaped.
  while(queue.length>PREPARED_ACK_QUEUE_MAX) queue.shift();
  notifyPlaybackControl();
  return ack;
}

function settlePlaybackControlAcknowledgement(p,request){
  const sent=request&&request.acknowledgement;
  const queue=p&&p.controlAcknowledgements;
  if(!sent||!queue||!queue.length) return;
  // Matched on both fields rather than on identity: the reporter replays a
  // retried request, and the object it replays is the one that was queued.
  const at=queue.findIndex(entry=>entry.action_id===sent.action_id&&entry.state===sent.state);
  if(at!==-1) queue.splice(at,1);
}

function preparedVideoElement(){
  return document.getElementById("video-prepared");
}
// Created on the first preparation rather than shipped in the modal. The
// structural golden pins what `#player` contains, and a second `<video>` that
// exists on every page — including the ones that never prepare anything — is a
// decoder host the page has no use for. Once a switch has happened the retired
// element IS this element, so nothing is created twice.
function ensurePreparedVideoElement(){
  const existing=preparedVideoElement();
  if(existing) return existing;
  const v=document.getElementById("video");
  if(!v||!v.parentNode) return null;
  const spare=document.createElement("video");
  spare.id="video-prepared";
  spare.muted=true;
  // The attributes the authoritative element carries, because after a switch
  // this element IS the authoritative one. A successor without
  // `x-webkit-airplay` would silently cost the viewer AirPlay for the rest of
  // the session, and nobody would connect that to a quality change.
  spare.setAttribute("playsinline","");
  spare.setAttribute("x-webkit-airplay","allow");
  spare.setAttribute("aria-hidden","true");
  spare.setAttribute("tabindex","-1");
  spare.style.display="none";
  v.parentNode.appendChild(spare);
  return spare;
}

function preparedState(p){
  return (p&&p.prepared)||null;
}
// One successor at a time.
//
// A repeated `action_id` is the SAME staging replayed — the server replays it
// byte-identically on every exchange until it settles — so it must not build a
// second pipeline. A different `action_id` while one is live aborts the old
// one first: the server allows one preparation per session, and a client that
// silently dropped the first would hold its slot until the 330 s deadline.
function handlePreparedReplacementAction(p,action){
  if(!p||!action) return null;
  if(!window.PlurxPlaybackControl||!PlurxPlaybackControl.validPreparation(action)) return null;
  // A staging this client has already settled is replayed until the server
  // processes the acknowledgement, which takes at least one more exchange —
  // and `p.prepared` is cleared the moment it settles. Without this, a commit
  // whose first frame is still pending, or an abort whose exchange has not
  // landed, is answered by BUILDING THE SAME PREPARATION AGAIN, on the element
  // the viewer is now watching.
  if(preparedSettlementDone(p,action.action_id)) return null;
  const existing=preparedState(p);
  if(existing&&existing.actionId===action.action_id) return existing;
  if(existing) abandonPreparedReplacement(p,"aborted","superseded by a newer preparation");
  return beginPreparedReplacement(p,action);
}
// Settled stagings, so a replay is recognised rather than rebuilt. Bounded and
// keyed by `action_id`; a session sees a handful of these at most.
function preparedSettlementDone(p,actionId){
  return !!(p&&p.preparedSettled&&p.preparedSettled.includes(actionId));
}
function markPreparedSettlement(p,actionId){
  if(!p||!actionId) return;
  const settled=p.preparedSettled||(p.preparedSettled=[]);
  if(settled.includes(actionId)) return;
  settled.push(actionId);
  while(settled.length>PREPARED_SETTLED_MEMORY) settled.shift();
}

function beginPreparedReplacement(p,action){
  const v=document.getElementById("video"), spare=ensurePreparedVideoElement();
  if(!v||!spare||PLAYER!==p) return null;
  // Two origins, and they are not the same number.
  //
  // `offeredOriginMs` is what the server said, echoed verbatim in the commit —
  // it drops a commit that names a different one, silently, because the origin
  // is how it tells "the client built what I offered" from "the client built
  // something else and is asking me to publish it".
  //
  // `mediaOriginMs` is what this client aligns against, and a VOD successor's
  // is zero: a VOD playlist is the whole immutable title, so its own zero IS
  // the source's zero and adding the resume position would seek the successor
  // twice. A prepared replacement never changes presentation — the successor is
  // the predecessor's request with the selection fields overwritten — so the
  // predecessor's `vod` is the successor's.
  const offeredOriginMs=Math.max(0,Math.round(Number(action.media_origin_ms)||0));
  const originMs=sessionMediaOriginMs({vod:!!p.vod,media_origin_ms:offeredOriginMs});
  const filmMs=playbackFilmPositionMs(v,p);
  // A replacement that begins encoding at the incumbent's current second
  // spends its entire preparation chasing a moving playhead. Begin up to six
  // seconds ahead when the incumbent already has enough runway to play
  // until that second. The buffer gate below still requires overlap with the
  // actual incumbent position before exposure, so this cannot skip content.
  const startLeadMs=Math.min(PREPARED_BUFFER_LEAD_MS+2000,
    Math.max(0,Math.round((bufferRunway(v)-3)*1000)));
  const state={actionId:action.action_id,sessionId:action.session_id,
    playlistUrl:action.playlist_url,mediaOriginMs:originMs,offeredOriginMs,
    selection:action.effective_selection,
    startAtSec:preparedLocalPositionMs(filmMs+startLeadMs,originMs)/1000,
    state:"building",hls:null,metadata:false,buffered:false,
    incumbentHls:null,incumbentLoadPaused:false,incumbentResumeTimer:null,
    frameTimer:null,framePollTimer:null,frameListener:null,startedAt:Date.now()};
  p.prepared=state;
  clientLog(Object.assign({level:"info",event:"prepared_replacement",detail:"staged",
    message:`preparing ${action.session_id} at film ${Math.round(filmMs/1000)}s `+
      `(origin ${Math.round(offeredOriginMs/1000)}s, ${preparedSelectionText(action.effective_selection)})`},
    playbackContext()));
  try{
    spare.muted=true;
    spare.style.display="none";
    if(preferNativeHls(spare)||!window.Hls||!Hls.isSupported()) preparedNativeAttach(p,state,spare);
    else preparedHlsAttach(p,state,spare);
  }catch(error){
    failPreparedReplacement(p,state,String(error&&error.message||error));
    return null;
  }
  return state;
}

function preparedSelectionText(selection){
  if(!selection) return "unknown delivery";
  const delivery=selection.codec==="source"?"direct":"transcode";
  const grade=selection.dynamic_range?` ${selection.dynamic_range}`:"";
  return `${selection.height||0}p${grade} ${delivery}${selection.quality_auto?" auto":""}`;
}
function preparedHlsAttach(p,state,spare){
  const tgt=bufferTargets(p&&p.bufSegSecs);
  const hls=new Hls({
    maxBufferLength:tgt.fwd,
    backBufferLength:tgt.back,
    ...(tgt.budgeted?{maxBufferSize:tgt.fwdBytes}:{}),
    fragLoadPolicy:vodClientContract().fragLoadPolicy,
    // Told before the first fragment, exactly as the incumbent was: seeking
    // after attach downloads the opening of the successor and throws it away,
    // which on a prepared handoff is the entire cost the preparation exists to
    // avoid.
    startPosition:state.startAtSec>0?state.startAtSec:-1,
    xhrSetup:x=>{ if(TOKEN) x.setRequestHeader("authorization","Bearer "+TOKEN); }});
  // The EWMA the incumbent paid to learn, so the successor's first rung choice
  // is not a cold guess on a link this client has been measuring all along.
  // Deliberately one-way: what the successor measures is not written back, so
  // a pipeline that fails on its way up cannot drag the incumbent's estimate
  // down with it.
  const seed=PlaybackPolicy.bandwidthSeedBps({
    outgoingEstimateBps:p&&p.bandwidthSeedBps, priorKbps:p&&p.priorKbps});
  if(seed) try{ hls.bandwidthEstimate=seed; }catch(e){}
  state.hls=hls;
  // The successor and incumbent share the viewer's link. Give the lower
  // bitrate successor a bounded first-fragment window while the incumbent
  // continues playing its buffered media. Restore incumbent loading on every
  // failed/aborted preparation and after at most six seconds if no handoff occurred.
  const incumbent=p&&p.hls;
  const pauseMs=Math.min(6000,Math.max(0,(bufferRunway(document.getElementById("video"))-3)*1000));
  if(pauseMs>0&&incumbent&&typeof incumbent.stopLoad==="function"
     &&typeof incumbent.startLoad==="function"){
    try{
      incumbent.stopLoad();
      state.incumbentHls=incumbent;
      state.incumbentLoadPaused=true;
      state.incumbentResumeTimer=setTimeout(()=>resumePreparedIncumbentLoad(p,state),pauseMs);
    }catch(e){}
  }
  hls.loadSource(state.playlistUrl);
  hls.attachMedia(spare);
  const current=()=>preparedState(p)===state&&PLAYER===p;
  hls.on(Hls.Events.MANIFEST_PARSED,()=>{
    if(!current()) return;
    notePreparedMetadata(p,state);
    // Muted and hidden, and playing: a paused successor's clock does not track
    // the incumbent's, and every second of drift is a corrective seek at the
    // switch.
    try{ const started=spare.play(); if(started&&started.catch) started.catch(()=>{}); }catch(e){}
  });
  if(Hls.Events.BUFFER_APPENDED) hls.on(Hls.Events.BUFFER_APPENDED,()=>{
    if(current()) notePreparedBuffer(p,state);
  });
  hls.on(Hls.Events.ERROR,(_,d)=>{
    if(!current()||!d||!d.fatal) return;
    failPreparedReplacement(p,state,String(d.details||d.type||"hls.js fatal error"));
  });
}
function resumePreparedIncumbentLoad(p,state){
  if(!state) return;
  if(state.incumbentResumeTimer!=null){
    clearTimeout(state.incumbentResumeTimer);
    state.incumbentResumeTimer=null;
  }
  if(!state.incumbentLoadPaused) return;
  state.incumbentLoadPaused=false;
  if(p&&p.hls===state.incumbentHls){
    try{ state.incumbentHls.startLoad(-1); }catch(e){}
  }
}
// Native HLS: the session id in the URL is the credential, same as the
// incumbent's path.
function preparedNativeAttach(p,state,spare){
  const current=()=>preparedState(p)===state&&PLAYER===p;
  const onMetadata=()=>{
    spare.removeEventListener("loadedmetadata",onMetadata);
    if(!current()) return;
    if(state.startAtSec>0) try{ spare.currentTime=state.startAtSec; }catch(e){}
    notePreparedMetadata(p,state);
    try{ const started=spare.play(); if(started&&started.catch) started.catch(()=>{}); }catch(e){}
  };
  const onProgress=()=>{ if(current()) notePreparedBuffer(p,state); };
  const onError=()=>{
    if(current()) failPreparedReplacement(p,state,"the successor element reported an error");
  };
  state.nativeListeners=[["loadedmetadata",onMetadata],["progress",onProgress],
    ["timeupdate",onProgress],["error",onError]];
  for(const pair of state.nativeListeners) spare.addEventListener(pair[0],pair[1]);
  spare.src=tok(state.playlistUrl);
}
// The manifest parsed and the tracks are known. Progress states are optional —
// the server behaves correctly without them — but they are how an operator
// sees how far a preparation got before it died.
function notePreparedMetadata(p,state){
  if(state.metadata) return;
  state.metadata=true;
  state.state="metadata_ready";
  queuePlaybackControlAcknowledgement(p,state.actionId,"metadata_ready");
}
// How far the successor is buffered, in FILM time, through the range that
// contains its own playhead. A range that does not contain the playhead is not
// runway; it is a gap the decoder will stop at.
function preparedBufferedThroughMs(state,spare){
  try{
    const local=spare.currentTime||0;
    for(let i=0;i<spare.buffered.length;i++){
      if(spare.buffered.start(i)<=local+0.25&&spare.buffered.end(i)>=local){
        return Math.max(0,Math.round(state.mediaOriginMs+spare.buffered.end(i)*1000));
      }
    }
  }catch(e){}
  return null;
}
function notePreparedBuffer(p,state){
  const v=document.getElementById("video"), spare=preparedVideoElement();
  if(!v||!spare) return;
  const through=preparedBufferedThroughMs(state,spare);
  if(through==null) return;
  const filmMs=playbackFilmPositionMs(v,p);
  // The successor may start ahead to catch up quickly. Wait until its first
  // buffered range also covers the exact incumbent second to avoid a skip.
  let coversIncumbent=false;
  for(let i=0;i<spare.buffered.length;i++){
    const start=state.mediaOriginMs+spare.buffered.start(i)*1000;
    const end=state.mediaOriginMs+spare.buffered.end(i)*1000;
    if(start<=filmMs+250&&end>=filmMs){ coversIncumbent=true; break; }
  }
  if(!coversIncumbent) return;
  const target=filmMs+PREPARED_BUFFER_LEAD_MS;
  if(through<target) return;
  if(!state.buffered){
    state.buffered=true;
    state.state="buffer_ready";
    queuePlaybackControlAcknowledgement(p,state.actionId,"buffer_ready",
      {buffered_through_ms:through});
  }
  commitPreparedReplacement(p,state);
}
// The switch.
//
// Order matters and it is not negotiable: the successor becomes visible and
// audible, the predecessor remains recoverable, and only once a frame has
// actually rendered is the predecessor retired and `committed` sent. The
// commit is a claim that the switch happened, and the store CAS that follows
// moves the playback pointer; destroying the predecessor before that proof
// turns a failed successor into an unrecoverable black player.
function commitPreparedReplacement(p,state){
  const v=document.getElementById("video"), spare=preparedVideoElement();
  if(!v||!spare||PLAYER!==p||preparedState(p)!==state) return false;
  if(state.state==="committing"||state.state==="committed") return false;
  if(!playbackOwnsAttachedMedia(p)) return false;
  state.state="committing";
  const filmMs=playbackFilmPositionMs(v,p);
  const wanted=preparedLocalPositionMs(filmMs,state.mediaOriginMs)/1000;
  const drift=Math.abs((spare.currentTime||0)-wanted)*1000;
  // Already on the incumbent's second: nothing to wait for, and the exposure
  // is the same synchronous block it has always been.
  if(drift<=PREPARED_ALIGN_SLACK_MS) return exposePreparedReplacement(p,state,v,spare,filmMs);
  // Otherwise finish aligning BEFORE the successor is seen or heard. The
  // corrective seek used to be the line above, with nothing between it and the
  // element swap; a seek is asynchronous, so the successor had nothing decoded
  // at its new position at the moment it took the picture, and the runway
  // measured before the seek was never evidence about the position after it.
  alignPreparedReplacement(p,state,v,spare);
  return true;
}
// Phase one of the switch: put the successor on the incumbent's second while
// the incumbent is still the one being seen and heard.
//
// Two attempts, and the drift is re-measured against the incumbent every time
// rather than assumed to be the number that started this: the incumbent kept
// playing for the whole duration of each seek. The web runs its successor
// muted and PLAYING, so what a completed seek leaves behind is only the seek's
// own latency and a rendezvous hold is not needed -- but the two-attempt bound
// is the same one it would have had.
async function alignPreparedReplacement(p,state,v,spare){
  const live=()=>PLAYER===p&&preparedState(p)===state
    &&document.getElementById("video")===v&&preparedVideoElement()===spare
    &&playbackOwnsAttachedMedia(p);
  for(let attempt=0;attempt<PREPARED_ALIGN_ATTEMPTS;attempt++){
    if(!live()) return false;
    const wanted=Math.max(0,
      preparedLocalPositionMs(playbackFilmPositionMs(v,p),state.mediaOriginMs)/1000);
    const landed=await preparedAlignSeek(spare,wanted);
    if(!live()) return false;
    // `readyState` answers for the element, not for this position, and a seek
    // that landed in a hole reports HAVE_FUTURE_DATA about a range the
    // playhead is not in. Both questions have to be asked here.
    if(landed&&preparedAlignedBuffered(spare)){
      const filmMs=playbackFilmPositionMs(v,p);
      const target=preparedLocalPositionMs(filmMs,state.mediaOriginMs)/1000;
      if(Math.abs((spare.currentTime||0)-target)*1000<=PREPARED_ALIGN_SLACK_MS)
        return exposePreparedReplacement(p,state,v,spare,filmMs);
    }
  }
  if(!live()) return false;
  // The incumbent is untouched and still playing. `failPreparedReplacement`
  // settles the staging with the server and hands the directed change its one
  // reopen.
  failPreparedReplacement(p,state,"could not align");
  return false;
}
// Resolve true when the element says the seek completed, false on the bound.
function preparedAlignSeek(spare,wanted){
  return new Promise((resolve)=>{
    let done=false, timer=null;
    const finish=(ok)=>{
      if(done) return;
      done=true;
      clearTimeout(timer);
      try{ spare.removeEventListener("seeked",onSeeked); }catch(e){}
      resolve(ok);
    };
    const onSeeked=()=>finish(true);
    try{ spare.addEventListener("seeked",onSeeked); }catch(e){ resolve(false); return; }
    timer=setTimeout(()=>finish(false),PREPARED_ALIGN_SEEK_MS);
    try{ spare.currentTime=wanted; }catch(e){ finish(false); }
  });
}
// Decoded, through the range that contains the playhead it just moved to, with
// half the ordinary lead in front of it. Half, not the whole lead: this is a
// re-check after a seek the preparation already earned its runway for, and
// demanding the full lead again would fail a successor that is fine.
function preparedAlignedBuffered(spare){
  try{
    if(!(Number(spare.readyState)>=3)) return false;
    const at=spare.currentTime||0, need=at+PREPARED_BUFFER_LEAD_MS/2000;
    for(let i=0;i<spare.buffered.length;i++){
      if(spare.buffered.start(i)<=at+0.25&&spare.buffered.end(i)>=need) return true;
    }
  }catch(e){}
  return false;
}
// Phase two: the exposure, unchanged. Intent is sampled at the last reversible
// boundary, mute and rate are applied before display, the elements swap, and
// the predecessor stays alive and hidden until a real successor frame proves
// rollback is no longer needed.
function exposePreparedReplacement(p,state,v,spare,filmMs){
  const predecessor=p.hls, retired=v;
  // The timer is only for a still-authoritative incumbent. Keep the paused
  // state for rollback, which explicitly restarts this pipeline if exposure
  // fails its first-frame proof.
  if(state.incumbentResumeTimer!=null){
    clearTimeout(state.incumbentResumeTimer);
    state.incumbentResumeTimer=null;
  }
  // Intent is sampled at the last reversible boundary. Preparation can take
  // seconds, during which the viewer may pause, mute, or change rate; copying
  // the earlier snapshot would overwrite that newer choice.
  const intent={
    wantsPlayback:p.wantsPlayback!==false,
    muted:!!retired.muted,
    volume:retired.volume,
    playbackRate:retired.playbackRate,
    defaultPlaybackRate:retired.defaultPlaybackRate
  };
  state.predecessor={hls:predecessor,element:retired,sessionId:p.sessionId,
    probeUrl:p.probeUrl,offset:p.offset,wantsPlayback:p.wantsPlayback,
    health:p.health,healthObservedAt:p.healthObservedAt,
    presentationAdvancedAt:p.presentationAdvancedAt,
    muted:retired.muted,volume:retired.volume,playbackRate:retired.playbackRate,
    defaultPlaybackRate:retired.defaultPlaybackRate};
  // Authoritative before visible, so anything that reads PLAYER.hls during the
  // swap reads the instance that owns the picture.
  p.hls=state.hls;
  p.sessionId=state.sessionId;
  p.probeUrl=state.playlistUrl;
  p.offset=state.mediaOriginMs/1000;
  p.health=null;
  p.healthObservedAt=null;
  p.presentationAdvancedAt=null;
  p.prepared=null;
  // Still this player's business until its first frame settles: the watchdog
  // below has to be cancellable by a teardown, or it fires eight seconds after
  // the player is gone and files a `failed` against a staging nobody is
  // watching any more.
  p.preparedCommitting=state;
  markPreparedSettlement(p,state.actionId);
  // Apply audio and rate before exposure. In particular, a muted incumbent
  // must never leak one audible successor frame during the DOM swap.
  spare.muted=intent.muted;
  try{
    spare.volume=intent.volume;
    spare.defaultPlaybackRate=intent.defaultPlaybackRate;
    spare.playbackRate=intent.playbackRate;
  }catch(e){}
  spare.style.display="";
  spare.removeAttribute("aria-hidden");
  retired.style.display="none";
  retired.muted=true;
  retired.setAttribute("aria-hidden","true");
  // The element the rest of the page addresses is `#video`. Swapping the ids
  // rather than moving nodes keeps every `getElementById("video")` correct
  // without touching the modal's structure — but only the lookups. Everything
  // bound to the old NODE has to move too, which is what `adoptPlaybackMediaElement`
  // is for. The retired element remains alive and hidden until a real successor
  // frame proves that rollback is no longer needed.
  retired.id="video-prepared";
  spare.id="video";
  adoptPlaybackMediaElement(p,spare);
  // Deliberately NOT a new media attachment. `startPlaybackControl` captured
  // the current one and refuses to capture a snapshot against any other, so
  // beginning one here would wedge the reporter — and the `committed` this
  // switch is about to earn would never reach an exchange.
  resetPlaybackTransportEvents(spare);
  p.wantsPlayback=intent.wantsPlayback;
  if(intent.wantsPlayback){
    // Un-muting without a user gesture pauses the element on WebKit, and the
    // last gesture was minutes ago. Ask again: a switch whose picture never
    // advances is reported as a failure eight seconds later and rolled back.
    try{ const resumed=spare.play(); if(resumed&&resumed.catch) resumed.catch(()=>{}); }catch(e){}
  }else{
    try{ spare.pause(); }catch(e){}
  }
  // The delivery badges and the info panel are painted from the session, and
  // the session just changed. Without this they describe the predecessor for
  // the rest of the film — on a handoff whose entire purpose was to change what
  // is being delivered.
  // M3. The swap has happened; record when, and which element is on each side
  // of it. Two assignments, no await, nothing read back by anything that
  // decides anything.
  notePreparedSwitchCommit(p,retired,spare);
  renderPlayerInfo();
  preparedFirstFrame(p,state,spare,unixMs=>{
    notePreparedSwitchFirstFrame(p);
    state.state="committed";
    if(p.preparedCommitting===state) p.preparedCommitting=null;
    // The switch the viewer asked for happened. Nothing owes them a reopen,
    // and the rung they asked for is the one being delivered.
    settleDirectedChange(p,p.directedChange,"committed",
      Math.round(performance.now()-((p.directedChange&&p.directedChange.tappedAt)||performance.now())));
    retirePreparedPredecessor(p,state);
    queuePlaybackControlAcknowledgement(p,state.actionId,"committed",
      {first_frame_unix_ms:unixMs,committed_media_origin_ms:state.offeredOriginMs});
    clientLog(Object.assign({level:"info",event:"prepared_replacement",detail:"committed",
      message:`switched to ${state.sessionId} at film ${Math.round(filmMs/1000)}s`},
      playbackContext()));
  },()=>{
    // Switched, and nothing rendered. The honest answer is `failed`: the
    // server is owed a settlement either way, and reporting a commit that did
    // not happen would write it to a durable row and move the playback pointer
    // to a session showing the viewer nothing.
    state.state="failed";
    if(p.preparedCommitting===state) p.preparedCommitting=null;
    rollbackPreparedReplacement(p,state,spare);
    queuePlaybackControlAcknowledgement(p,state.actionId,"failed");
    // Rolled back to the incumbent, which is the quality the viewer was
    // already watching -- so the change they asked for still has not happened
    // and still owes them its one reopen.
    fallBackDirectedChange(p,p.directedChange,"failed");
    clientLog(Object.assign({level:"error",event:"prepared_replacement",detail:"no_first_frame",
      message:`successor ${state.sessionId} rendered no frame within ${PREPARED_FIRST_FRAME_MS}ms`},
      playbackContext()));
  });
  return true;
}
