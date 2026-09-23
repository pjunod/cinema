"use strict";
// ---- playback measurements -------------------------------------------------
// Two numbers decide whether a playback change actually worked, and neither is
// visible from the server alone: how long the viewer waited for the first
// frame, and how much runway they had at the moment it ran out. Both land in
// Settings → Logs next to the server's own timings for the same session, which
// is what turns "4K buffers sometimes" into an attributable event.

// Seconds of already-downloaded video ahead of the playhead. This is the
// quantity every stall argument is really about: a stall with 0.2s of runway
// is a supply problem (server or link), and a stall with 20s buffered is the
// decoder or the tab, which is a completely different bug.
function bufferRunway(v){
  try{
    const t=v.currentTime;
    for(let i=0;i<v.buffered.length;i++){
      if(v.buffered.start(i)<=t+0.25 && v.buffered.end(i)>=t) return +(v.buffered.end(i)-t).toFixed(1);
    }
  }catch(e){}
  return 0;
}
// Every stream start gets an identity and a stated reason. Without them a
// seek's numbers and a cold start's land in the same bucket, and "time to
// first frame" stops meaning anything — the restart paths (seek, audio
// switch, quality change, rescue transcode) are far cheaper than a cold start
// and would flatter the average.
let ATTEMPT_SEQ=0;
// Auto quality evidence needs the switch itself, not a later sample that may
// have skipped over several moves. This sequence is page-lifetime like the
// attempt sequence, while PLAYER.abr.switches remains the bounded debug log.
let AUTO_SWITCH_SEQ=0;
// These page-lifetime totals are incremented at the fault itself. Sampling an
// outgoing PLAYER after replacement is impossible, so rolling per-player
// counters only when a harness polls can lose the last fault before a switch.
let PLAYBACK_LIFETIME_STALLS=0;
let PLAYBACK_LIFETIME_HITCHES=0;
// Every full play() open supersedes the previous one, including an older call
// still awaiting a decision or session. The token is independent of PLAYER so
// it survives the point where a successful open replaces that object.
function createPlaybackOpenGate(){
  let sequence=0, replayAttempt=null;
  const current=attempt=>!!attempt&&attempt.token===sequence;
  return {
    begin(kind){
      if(kind==="replay"&&replayAttempt&&current(replayAttempt)) return null;
      const next=sequence+1;
      if(!Number.isSafeInteger(next)) throw new RangeError("playback open sequence exhausted");
      const attempt=Object.freeze({token:next,kind:kind==="replay"?"replay":"open"});
      sequence=next;
      if(attempt.kind==="replay") replayAttempt=attempt;
      return attempt;
    },
    current,
    finish(attempt){ if(replayAttempt===attempt) replayAttempt=null; },
    invalidate(){
      const next=sequence+1;
      if(!Number.isSafeInteger(next)) throw new RangeError("playback open sequence exhausted");
      sequence=next;
    },
    acceptResource(attempt,resource,release){
      if(current(attempt)) return true;
      if(resource!=null&&typeof release==="function") release(resource);
      return false;
    },
  };
}
const PLAY_OPEN_GATE=createPlaybackOpenGate();
// Network preparation has one absolute lifetime, separate from presentation.
// A moving predecessor cannot extend it. Cancellation/timeout settles even a
// transport that ignores abort, and its eventual session result is released.
function beginPlaybackPreparation(isCurrent){
  beginPlaybackPreparation.active?.cancel();
  const controller=new AbortController(), began=performance.now();
  let abandoned=false, rejectPending=null;
  const cancelled=()=>Object.assign(new Error("Playback preparation superseded."),{name:"AbortError"});
  // The 20 s bound stays where it is (§6: it is a pre-existing threshold), and
  // ruling 2 makes what it PRODUCES honest. An operation that has its own
  // truthful answer for this deadline leaves it in `expiry`; M5's create-retry
  // sequence is the one that does, because when the server has been answering
  // "not yet" the end of that sequence is the sequence's own exhaustion and not
  // a generic preparation failure. The hook raises before it returns, so the
  // error carries `surfaceRaised` and `failPreparation` leaves the prompt
  // standing instead of painting "Playback could not prepare." over it.
  const timedOut=()=>{
    const own=owner.expiry&&owner.expiry();
    return own||Object.assign(new Error("Playback preparation timed out. Your place is saved."),{name:"TimeoutError"});
  };
  const owner={
    // Set by the running operation, cleared when it leaves. A supersede does
    // not consult it: cancelling an open raises nothing, because the newer
    // intent owns the surface.
    expiry:null,
    cancel(){abandoned=true;controller.abort();rejectPending?.(cancelled());},
    finish(){if(beginPlaybackPreparation.active===owner)beginPlaybackPreparation.active=null;},
    async run(operation,release){
      if(abandoned||!isCurrent()) throw cancelled();
      const remaining=20000-(performance.now()-began);
      if(remaining<=0){abandoned=true;controller.abort();throw timedOut();}
      let timer;
      const deadline=new Promise((_,reject)=>{
        rejectPending=reject;
        timer=setTimeout(()=>{
          abandoned=true;
          reject(timedOut());
          controller.abort();
        },remaining);
      });
      try{
        const result=Promise.resolve(operation(controller.signal)).then(value=>{
          if(!abandoned&&performance.now()-began>=20000){
            abandoned=true;controller.abort();if(release)release(value);throw timedOut();
          }
          if(abandoned||!isCurrent()){
            if(release) release(value);
            throw cancelled();
          }
          return value;
        });
        return await Promise.race([result,deadline]);
      }finally{clearTimeout(timer);rejectPending=null;}
    }
  };
  beginPlaybackPreparation.active=owner;
  return owner;
}
// Set by a caller that is about to re-enter play() for a reason play()
// cannot infer — a quality change looks exactly like a resume from the
// inside, and mislabelling it would put restart timings in the cold-start
// bucket where they flatter the numbers.
let PENDING_ATTEMPT_REASON=null;
function takePlaybackAttemptReason(){
  const reason=PENDING_ATTEMPT_REASON;
  PENDING_ATTEMPT_REASON=null;
  return reason;
}
// Set when a restart's whole point is "without the subtitles". play() re-applies
// the server-chosen default track on arrival, and when that default is the
// bitmap track that forced a burn — a forced-flag PGS on a foreign-dialogue
// film — "subtitles off" would burn them straight back in. One-shot, consumed
// at the top of play().
let PENDING_DEFAULT_SUB_OFF=false;
function newAttempt(reason){
  if(!PLAYER) return;
  PLAYER.attemptId=`a${++ATTEMPT_SEQ}`;
  PLAYER.attemptReason=reason||"cold-start";
  PLAYER.attemptAt=performance.now();
}
function playbackContext(){
  const v=document.getElementById("video"), p=PLAYER||{};
  return {
    // What is on screen, not what was asked for — under Auto the request
    // carries no height at all, and a beacon that reported null would lose the
    // one field that separates a 4K session's numbers from a 720p one's.
    height:(v&&v.videoHeight)||(p.method==='transcode'?transcodeHeight():null)
           ||((p.source&&p.source.height)||null),
    encoder:p.encoder||null,
    // Handoff §4.8 names the wire field `session`; the server accepts it as an
    // alias of its typed `session_id`, so old and native clients stay additive.
    session:p.sessionId||null,
    runway:v?bufferRunway(v):null,
    attempt:p.attemptId||null,
    reason:p.attemptReason||null,
    // hls.js's EWMA over real segment downloads. On a just-in-time server this
    // measures min(link speed, encode speed) — which is exactly why a stall
    // needs the server's own `speed` reading beside it to say which one ran out.
    bandwidth:(p.hls&&p.hls.bandwidthEstimate)?Math.round(p.hls.bandwidthEstimate/1000):null,
    // Whether this machine is decoding in hardware. `powerEfficient` is the
    // browser's own word for it, and it is the difference between two failures
    // that look identical from every other number here: a full buffer with
    // late frames because the link is fine and the GPU is doing the work, and
    // a full buffer with late frames because a CPU is software-decoding 4K.
    // Nothing else in the beacon can tell those apart, and asking the operator
    // which machine the browser is on has not worked.
    decode_hw:p.decodeInfo?p.decodeInfo.hw:null,
    decode_smooth:p.decodeInfo?p.decodeInfo.smooth:null,
  };
}
// Ask the browser what it will do with this stream, before it does it.
//
// mediaCapabilities is the only API that answers "will this be hardware
// decoded" — `smooth` and `powerEfficient` are computed from the real codec,
// resolution, framerate and bitrate rather than from a codec string alone, so
// it distinguishes 1080p HEVC (fine nearly everywhere) from 4K HEVC at 69 Mb/s
// (fine almost nowhere without a GPU path). Best-effort: a browser without it,
// or one that throws on an odd configuration, leaves the field null rather
// than blocking playback on a diagnostic.
async function probeDecode(){
  const p=PLAYER; if(!playbackOwnsAttachedMedia(p)) return;
  const attachment=p.mediaAttachment;
  const src=p.source||{};
  // Same ladder the transport choice uses, for the same reason: `codec[0]` is
  // what gets asked about, and on a 10-bit source the honest question is the
  // Main10 one. Asking a Main (8-bit) string about a Main10 file would have
  // this diagnostic report a decoder that is not the one doing the work.
  const codec=MSE_VIDEO[mseCodecKey(src.video_codec, src.bit_depth)];
  if(!codec || !navigator.mediaCapabilities || !navigator.mediaCapabilities.decodingInfo) return;
  const v=document.getElementById("video");
  const cfg={
    type:p.hls?"media-source":"file",
    video:{
      contentType:`video/mp4; codecs="${codec[0]}"`,
      width:(v&&v.videoWidth)||src.width||1920,
      height:(v&&v.videoHeight)||src.height||1080,
      bitrate:Math.max(1, src.bitrate||5000000),
      framerate:24,
    },
  };
  try{
    const r=await navigator.mediaCapabilities.decodingInfo(cfg);
    if(playbackOwnsAttachedMedia(p)&&p.mediaAttachment===attachment)
      p.decodeInfo={supported:!!r.supported, smooth:!!r.smooth, hw:!!r.powerEfficient};
  }catch(e){}
}
function reportTtff(){
  const p=PLAYER; if(!playbackOwnsAttachedMedia(p)||!p.playStartedAt) return;
  const ms=Math.round(performance.now()-p.playStartedAt);
  p.playStartedAt=null;   // first frame only; a seek is not a start
  p.ttffMs=ms;
  clientLog(Object.assign({level:"info",event:"ttff",ms,
    message:`first frame after ${(ms/1000).toFixed(1)}s`}, playbackContext()));
}
// A `waiting` event is not yet a stall, and a stall is not one kind of thing.
//
// Safari fires `waiting` at fMP4 segment boundaries and whenever its decoder
// hitches on a high-bitrate stream — an 83 Mb/s 4K HEVC remux produces them
// during entirely healthy playback. Counting each one made a session holding
// forty-four seconds of buffer report itself as stalling, which is both wrong
// and the opposite of useful: it buries the real ones.
//
// Two things fix it. A wait only counts once it has LASTED — the video going
// hungry for 80ms between segments is not an interruption anyone saw. And a
// wait is classified by how much was buffered when it began, because that
// single number separates two unrelated faults: nothing left to play (supply —
// the server or the link ran out) versus plenty left and still not presenting.
// The latter is an unattributed presentation wait unless the media element
// exposes a typed error; loaded bytes alone say nothing about the decoder.
// Only supply pressure should send anyone looking at pacing.
const STALL_MIN_MS=350;      // shorter than this and playback did not visibly pause
const SUPPLY_RUNWAY_SECS=1.5; // less buffered than this when it began: we ran dry
const PERSISTENT_STALL_MS=8000; // browser nudges failed; recover instead of spinning forever
// One control hold may explain why production paused, but it cannot own a
// frozen picture. This is absolute from beginWait(), not another interval
// from each verdict.
const CONTROL_STALL_DEFER_DEADLINE_MS=20000;
const CHASE_RATE_MAX=0.90;   // realized rate under this fraction of the asked rate is a chase
const CHASE_RUNWAY_SECS=4;   // and only with this much buffered — starved is a stall, not a chase
const CHASE_REPORT_MS=60000; // one report per minute while it persists
function beginWait(v){
  const p=PLAYER;
  if(!p||!p.started||v.seeking||v.paused) return;
  if(!playbackOwnsAttachedMedia(p)) return;
  if(p.waitAt) return;                       // already hungry; not a second one
  p.waitAt=performance.now();
  p.waitStartedRunway=bufferRunway(v);
  p.waitNudgedAt=null;
  p.waitReported=false;
  const began=p.waitAt;
  const generation=p._seekToken||0;
  const actionGeneration=p.controlIntentGeneration||0;
  p.waitTimer=setTimeout(()=>persistentWait(v,p,began,generation,actionGeneration).catch(()=>{}),PERSISTENT_STALL_MS);
}
function noteAutoStall(p,kind,episodeAtMs){
  const abr=p&&p.abr; if(!abr||!(kind==='supply'||kind==='decode')) return;
  const now=performance.now();
  // `waiting` and hls.js's bufferStalledError describe opposite edges of one
  // pause. Key both reports to the wait's start so a long pause cannot satisfy
  // the three-stall rescue by being counted once at each edge.
  const episodeAt=episodeAtMs!=null&&Number.isFinite(Number(episodeAtMs))
    ? Number(episodeAtMs)
    : (p.waitAt!=null?p.waitAt:now);
  abr.stallEvents[kind]=PlaybackPolicy.recordStallEpisode({
    events:abr.stallEvents[kind],episodeAtMs:episodeAt,nowMs:now
  });
  abr.lastStallAtMs=now;
}
function recordWaitStall(p, kind, ms, startedRunway, currentRunway, detail, episodeAtMs,
  controlTrigger){
  p.stalls=(p.stalls||0)+1;
  PLAYBACK_LIFETIME_STALLS++;
  p.stallsByKind=p.stallsByKind||{supply:0,decode:0,presentation:0};
  p.stallsByKind[kind]=(p.stallsByKind[kind]||0)+1;
  noteAutoStall(p,kind,episodeAtMs);
  const ctx=playbackContext();
  clientLog(Object.assign({level:"warn",event:"stall",detail:detail||kind,ms,
    message:`${kind} stall, ${(ms/1000).toFixed(1)}s, ${currentRunway==null?"?":currentRunway}s buffered`+
      ` (started ${startedRunway==null?"?":startedRunway}s)`,
    control_trigger:controlTrigger||null},
    ctx, {runway:currentRunway,wait_started_runway:startedRunway,
      current_runway:currentRunway}));
}
// Called when playback resumes (or is abandoned). Decides, in retrospect,
// whether that gap was worth anyone's attention.
function endWait(resumed){
  const p=PLAYER; if(!p||!p.waitAt) return;
  clearTimeout(p.waitTimer); p.waitTimer=null;
  const began=p.waitAt;
  const ms=Math.round(performance.now()-began);
  const runway=p.waitStartedRunway;
  const reported=!!p.waitReported;
  p.waitAt=null; p.waitStartedRunway=null; p.waitNudgedAt=null; p.waitReported=false;
  if(reported) return;                       // persistentWait already emitted it
  if(!resumed || ms<STALL_MIN_MS) return;    // a hitch, not a stall
  let video=null; try{video=document.getElementById&&document.getElementById("video");}catch(e){}
  const kind=persistentWaitEvidence(video,runway).kind;
  recordWaitStall(p,kind,ms,runway,runway,kind,began);
}
// Classify only what the browser actually exposed. A stationary film clock
// with loaded media is a presentation wait, not a decoder verdict. Typed media
// errors remain usable evidence, while an empty buffer identifies supply
// pressure without guessing whether disk, producer or network caused it.
function persistentWaitEvidence(v,runway){
  const error=v&&v.error;
  const code=Number(error&&error.code)||0;
  const detail=String(error&&error.message||"").replace(/[\r\n\0]+/g," ").slice(0,120);
  if(code===3||code===4){
    return {kind:"decode",cause:"decoder",observation:{decoder_state:"failed",
      error_code:"decoder",error_detail:detail||`media_error_${code}`}};
  }
  if(code===2){
    return {kind:"supply",cause:"network",observation:{decoder_state:"unknown",
      error_code:"network",error_detail:detail||"media_error_2"}};
  }
  if(runway!=null&&runway<SUPPLY_RUNWAY_SECS){
    return {kind:"supply",cause:"unknown",observation:{decoder_state:"starved"}};
  }
  return {kind:"presentation",cause:"unknown",
    observation:{decoder_state:"unknown"}};
}
// Freeze the evidence and selected delivery contract before the attachment
// owner mutates anything. Later callbacks can describe this episode without
// re-reading a successor's method, quality, tracks or media identity.
function stallRecoverySnapshot(p,v,{began,kind,cause,action,position,targetHeight}){
  const source=p&&p.source||{};
  const selected=p&&p.audio&&p.audio[p.curAudio];
  const recipe=Object.freeze({
    method:p&&p.method||null,copy_hls:!!(p&&p.copyHls),quality:playQuality(),
    height:Number((p&&p.health&&p.health.target_height)||(p&&p.autoHeight)||
      (v&&v.videoHeight)||source.height)||null,
    video_codec:source.video_codec||null,video_profile:source.video_profile||null,
    width:Number(source.width)||null,
    dynamic_range:(p&&p.deliveredRange)||source.hdr_format||source.hdr||null,
    audio_track:selected?selected.index:0,subtitle_track:p&&p.curSub>=0?p.curSub:null,
    subtitle_mode:p&&p.burnedSub!=null?"burn":p&&p.curSub>=0?"native":"off",
    audio_offset_ms:Number(p&&p.aoffset)||0,
  });
  return Object.freeze({player:p,attachment:p&&p.mediaAttachment||null,
    generation:p&&p._seekToken||0,intentGeneration:p&&p.controlIntentGeneration||0,
    presentationEpoch:p&&p.controlPresentationEpoch||0,
    sessionId:p&&p.sessionId||null,began,at:performance.now(),kind,cause,action,
    position,targetHeight,recipe});
}
// A wait that never resumes used to be invisible forever: endWait() was the
// only reporter and therefore never ran. Record it at a fixed deadline, then
// make one evidence-aware attempt. Unattributed waits keep the selected recipe;
// independently attributed decoder failures may use the compatible path;
// generic transfer failures reconnect without inventing a capacity verdict,
// and explicit quality choices remain exact.
async function persistentWait(v,p,began,generation,actionGeneration){
  if(!playbackOwnsAttachedMedia(p)) return;
  if(PLAYER!==p || p.waitAt!==began || !(p.started||p.controlSeek?.executed) ||
     (p._seekToken||0)!==generation ||
     (p.controlIntentGeneration||0)!==actionGeneration ||
     p.wantsPlayback===false || (p.wantsPlayback==null&&v.paused) ||
     (v.seeking&&!p.controlSeek?.executed&&!p.progressWatch?.fired)) return;
  p.waitTimer=null;
  const ms=Math.round(performance.now()-began);
  const startedRunway=p.waitStartedRunway;
  let currentRunway=bufferRunway(v);
  let evidence=persistentWaitEvidence(v,currentRunway), kind=evidence.kind;
  const firstReport=!p.waitReported;
  p.waitReported=true;
  // This exact transition owns the legacy reopen. Cadence alone is too late:
  // the replacement normally stops this reporter before its next scheduled
  // exchange, leaving the server with only the earlier `waiting` fact.
  const controlObservation=evidence.observation;
  // Presentation recovery belongs to this client, not to the producer
  // verdict. Spend the one harmless browser reevaluation before the bounded
  // ask, then observe actual clock/frame progress through the same absolute
  // deadline rather than treating play() itself as recovery.
  let nativeReevaluated=false;
  if(kind==="presentation"&&p.waitNudgedAt!==began){
    p.waitNudgedAt=began;
    nativeReevaluated=true;
    try{
      const nudge=v.play();
      if(nudge&&typeof nudge.catch==="function") nudge.catch(()=>{});
    }catch(e){}
  }
  // Both the initial ask and any extension share the absolute stall deadline.
  // A callback near the boundary gets only the remaining control budget.
  const ask=ms>=CONTROL_STALL_DEFER_DEADLINE_MS
    ? {trigger:null,action:Promise.resolve(null)}
    : askPlaybackControl("stalled",controlObservation,began+CONTROL_STALL_DEFER_DEADLINE_MS);
  const controlTrigger=ask.trigger;
  if(nativeReevaluated){
    clientLog({level:"info",event:"stall_recovery",detail:"native_reevaluation:wait",
      message:"loaded media remained waiting — native playback reevaluated before recovery",
      control_trigger:controlTrigger,wait_started_runway:startedRunway,
      current_runway:currentRunway});
  }
  if(firstReport) recordWaitStall(p,kind,ms,startedRunway,currentRunway,
    `${kind}-persistent`,began,controlTrigger);
  p.stallFrom=p.controlSeek?.targetMs!=null
    ? (p.controlSeek.targetMs+(p.bookOffset||0))/1000 : pbPosSec();
  // An armed verdict is used only when this ask produced none of its own: a
  // fresh answer is always better evidence than a remembered one.
  const verdict=(await ask.action)||armedPlaybackControlVerdict(p);
  // The viewer had up to CONTROL_ASK_MS to seek, pause or close. Re-check
  // every condition the entry guard checked, because none of them survived
  // the await on their own — acting on a stale generation here would reopen
  // a stream the viewer already left.
  if(!playbackOwnsAttachedMedia(p)||p.waitAt!==began || !(p.started||p.controlSeek?.executed) ||
     (p._seekToken||0)!==generation ||
     (p.controlIntentGeneration||0)!==actionGeneration ||
     p.wantsPlayback===false || (p.wantsPlayback==null&&v.paused) ||
     (v.seeking&&!p.controlSeek?.executed&&!p.progressWatch?.fired)) return;
  // The ask itself consumes frozen-picture time. Re-read the absolute age
  // after it settles so a slow control response cannot extend the deadline.
  const controlElapsedMs=Math.round(performance.now()-began);
  // The control exchange may consume most of the remaining observation
  // window. Its pre-await runway is telemetry, not authority over a buffer
  // that can refill, drain, or acquire a typed media error while we wait.
  const decisionRunway=bufferRunway(v);
  const decisionEvidence=persistentWaitEvidence(v,decisionRunway);
  if(decisionRunway!==currentRunway||decisionEvidence.kind!==kind){
    clientLog({level:"info",event:"stall_recovery",detail:"evidence_resampled",
      message:`stall evidence changed during control: ${kind}/${currentRunway}s -> `+
        `${decisionEvidence.kind}/${decisionRunway}s`,control_trigger:controlTrigger,
      wait_started_runway:startedRunway,current_runway:decisionRunway});
  }
  currentRunway=decisionRunway; evidence=decisionEvidence; kind=evidence.kind;
  // A supply wait can refill while the ask is outstanding. It has not yet
  // received the presentation reevaluation that an initially loaded wait got,
  // so spend that harmless step now before observing the remaining deadline.
  if(kind==="presentation"&&p.waitNudgedAt!==began){
    p.waitNudgedAt=began;
    try{
      const nudge=v.play();
      if(nudge&&typeof nudge.catch==="function") nudge.catch(()=>{});
    }catch(e){}
    clientLog({level:"info",event:"stall_recovery",detail:"native_reevaluation:refill",
      message:"media refilled during control — native playback reevaluated before recovery",
      control_trigger:controlTrigger,wait_started_runway:startedRunway,
      current_runway:currentRunway});
  }
  if(verdict&&verdict.type==="terminal"){
    // Ruling D1: the verdict is armed, not executed. Everything this player
    // had buffered is already spent — that is what a persistent wait means —
    // so the only thing the verdict changes is whose words the viewer reads.
    // The viewer's own options are untouched: a terminal recipe is not a
    // terminal file, and Force transcode is a different recipe.
    const verdictText=controlVerdictText(verdict.message);
    clientLog({level:"warn",event:"stall_recovery",detail:"terminal:"+verdict.code,
      message:"server verdict ended this recipe — "+verdictText,
      control_trigger:controlTrigger});
    // The verdict is still armed, not executed: what ends this recipe is that
    // the owner returns from here with nothing left to try. A blocking surface
    // is only ever drawn over a player its owner has stopped (§3.2), so stop
    // first and let the presenter say the server's sentence.
    stopPlayerForExhaustion();
    raisePlaybackSurface("owner_stopped",{player_stopped:true,
      title:verdictText,detail:"Your place is saved.",
      actions:playbackStallActions(p,"stall-terminal")});
    return;
  }
  if(kind==="presentation"&&controlElapsedMs<CONTROL_STALL_DEFER_DEADLINE_MS){
    const remaining=CONTROL_STALL_DEFER_DEADLINE_MS-controlElapsedMs;
    const again=Math.min(PERSISTENT_STALL_MS,remaining);
    clientLog({level:"info",event:"stall_recovery",detail:"deferred:presentation_observation",
      message:`loaded media remains stationary — observing progress for ${(again/1000).toFixed(1)}s`,
      control_trigger:controlTrigger,wait_started_runway:startedRunway,
      current_runway:currentRunway,deadline_remaining_ms:remaining});
    p.waitTimer=setTimeout(()=>persistentWait(v,p,began,generation,actionGeneration).catch(()=>{}),again);
    return;
  }
  if(p.stallDeferralsAt!==began){ p.stallDeferralsAt=began; p.stallDeferrals=0; }
  if(verdict&&verdict.type==="hold"){
    if(kind==="supply"||kind==="presentation"){
      // A hold says why the producer paused. It never says whether published
      // bytes can be fetched or whether already-loaded bytes can be presented,
      // and production state is not authority over either serving or the local
      // presentation owner. Fall through to the same bounded local recovery an
      // unanswered ask gets.
      clientLog({level:"warn",event:"stall_recovery",detail:"fallthrough:hold",
        message:`server holds this ${kind} stall (${verdict.reason}) — recovering locally`,
        control_trigger:controlTrigger});
    }else if(controlElapsedMs<CONTROL_STALL_DEFER_DEADLINE_MS){
      // Production is deliberately not advancing, so the client's reopen would
      // churn against a server that already knows better. This player holds
      // media it cannot render rather than media it cannot get, so waiting
      // costs it nothing it could have fetched. Defer the reopen — but not the
      // viewer's information. A client that only waited would leave a viewer
      // eight seconds into a frozen picture with no UI and no bound, forever.
      // The reopen is what a hold suppresses; the explanation is what it earns.
      p.stallDeferrals=(p.stallDeferrals||0)+1;
      clientLog({level:"info",event:"stall_recovery",detail:"deferred:hold",
        message:`server holds this stall (${verdict.reason}) — reopen deferred`,
        control_trigger:controlTrigger});
      // A notice, not a screen: the server is deliberately not advancing and
      // the viewer is told why, once per hold rather than once per deferral.
      raisePlaybackSurface("control_hold",{context:"attached",
        title:"Waiting for the server.",detail:holdReasonText(verdict.reason),
        actions:["retry","close"]},{once:true});
      p.waitTimer=setTimeout(()=>persistentWait(v,p,began,generation,actionGeneration).catch(()=>{}),
        Math.min(PERSISTENT_STALL_MS,CONTROL_STALL_DEFER_DEADLINE_MS-controlElapsedMs));
      return;
    }else{
      clientLog({level:"warn",event:"stall_recovery",detail:"fallthrough:hold_deadline",
        message:"server hold reached the client recovery deadline — recovering anyway",
        control_trigger:controlTrigger});
    }
  }
  if(verdict&&verdict.type==="retry_resource"&&(p.stallDeferrals||0)<CONTROL_DEFER_LIMIT
     &&controlElapsedMs<CONTROL_STALL_DEFER_DEADLINE_MS){
    // Production stopped for something that may not recur, and named when to
    // look again. Pace to it rather than reopening — but bounded, because a
    // server that keeps saying "soon" is not distinguishable, from here, from
    // one that is never going to be ready, and today's path is better than an
    // unbounded wait.
    p.stallDeferrals=(p.stallDeferrals||0)+1;
    const again=Math.min(Math.max(verdict.after_ms,CONTROL_MIN_EXCHANGE_MS),PERSISTENT_STALL_MS);
    clientLog({level:"info",event:"stall_recovery",detail:"deferred:retry_resource",
      message:`server paces this stall (${verdict.reason}) — waiting ${(again/1000).toFixed(1)}s`+
        ` (${p.stallDeferrals}/${CONTROL_DEFER_LIMIT})`,
      control_trigger:controlTrigger});
    p.waitTimer=setTimeout(()=>persistentWait(v,p,began,generation,actionGeneration).catch(()=>{}),
      Math.min(again,CONTROL_STALL_DEFER_DEADLINE_MS-controlElapsedMs));
    return;
  }
  const action=PlaybackPolicy.stallRecoveryAction({
    method:p.method, quality:playQuality(),cause:evidence.cause,
    alreadyRecovered:(p.stallRecoveries||0)>0
  });
  if(action==='prompt'){
    // One automatic recovery was already spent, and this branch offers no
    // second one. Contract §3.4: the owner stops the player, then raises.
    stopPlayerForExhaustion();
    raisePlaybackSurface("owner_exhausted",{player_stopped:true,
      title:"Playback is still stalled.",
      detail:"One automatic recovery was already tried. Your place is saved.",
      actions:playbackExhaustedActions(p)});
    return;
  }
  const estimateBps=(p.hls&&p.hls.bandwidthEstimate)||p.bandwidthSeedBps;
  const recoveryHeight=PlaybackPolicy.stallRecoveryTargetHeight({
    method:p.method,quality:playQuality(),kind,ladder:p.ladder,
    currentHeight:Number((p.health&&p.health.target_height)||p.autoHeight||v.videoHeight),
    estimateKbps:estimateBps>0?estimateBps/1000:(p.priorKbps||null),
    recentEstimateKbps:p.abr&&p.abr.recentEstimateKbps,
    recentEstimateAtMs:p.abr&&p.abr.recentEstimateAtMs,
    nowMs:performance.now()
  });
  p.stallRecoveries=(p.stallRecoveries||0)+1;
  p.recoveringStall=stallRecoverySnapshot(p,v,{began,kind,cause:evidence.cause,
    action,position:p.stallFrom,targetHeight:recoveryHeight});
  clientLog(Object.assign({level:"warn",event:"stall_recovery",detail:`attempt:${action}`,
    message:`persistent ${kind} stall — ${action==='transcode'?"switching to transcode":"reconnecting"}`,
    control_trigger:controlTrigger},
    playbackContext()));
  const position=p.recoveringStall.position;
  endWait(false);
  raisePlaybackSurface("owner_recovery_step",{
    title:action==='transcode'
      ?(kind==='supply'?"Buffer ran dry — optimizing…":"Playback stalled — changing decoder path…")
      :"Playback stalled — reconnecting…",
    detail:`resuming from ${clockFromSec(position)}`});
  if(action==='transcode') startTranscodeFallback("stall-recovery",
    `switched to a transcode after a persistent ${kind} stall`);
  else seekTo(position,true,recoveryHeight,false,p.recoveringStall);
}
function finishStallRecovery(outcome,note){
  const p=PLAYER, r=p&&p.recoveringStall; if(!r) return false;
  const ms=Math.round(performance.now()-r.at);
  clientLog(Object.assign({level:outcome==='recovered'?"info":"error",
    event:"stall_recovery",detail:outcome,ms,
    message:`stall recovery ${outcome} after ${(ms/1000).toFixed(1)}s`+(note?` — ${note}`:"")},
    playbackContext()));
  p.recoveringStall=null;
  // The recovery owner reporting its own outcome, which is what retires a
  // `recovering` fault the reducer would otherwise hold until evidence arrives.
  if(outcome==='recovered') playbackSurfaceStep({owner_success:playbackSurfaceGeneration(p)});
  return true;
}
function showStallRecoveryFailure(note){
  const p=PLAYER; if(!p) return;
  // The ladder is spent. Contract §3.4: stop the player, then raise — the
  // sentence, the buttons and every caller are what they were.
  stopPlayerForExhaustion();
  raisePlaybackSurface("owner_exhausted",{player_stopped:true,
    title:"Playback could not reconnect.",detail:note||"Your place is saved.",
    actions:playbackExhaustedActions(p)});
}
// The failure every per-frame counter forgives: hardware decode, a full
// buffer, and the presentation clock quietly running at three-quarter speed.
// No frame drops, no stall fires, and `slow` hitches accumulate on healthy
// sessions too — so the one session that LOOKED like buffering reported
// nothing. Seen live (reference film F, 2026-07-30): 18.2 fps rendered at 75.8% speed,
// 9 s buffered, VideoToolbox decoding in hardware per chrome://media-internals
// — and not one line in the log ring. The realized rate over the detector's
// ~10 s window is the clean signal: the window already clears on pause and
// seek, and requiring a healthy runway keeps supply stalls out (those belong
// to endWait, and only those should send anyone looking at pacing).
function reportRateChase(v){
  const p=PLAYER; if(!p||!p.hitches) return;
  const h=p.hitches, want=(v.playbackRate||1);
  if(h.rate==null || !(h.rate>0) || want<=0 || h.rate>=want*CHASE_RATE_MAX) return;
  const runway=bufferRunway(v);
  if(runway<CHASE_RUNWAY_SECS) return;       // starved — endWait's case, not this one
  const now=performance.now();
  if(p._chaseAt && now-p._chaseAt<CHASE_REPORT_MS) return;
  p._chaseAt=now;
  clientLog(Object.assign({level:"warn",event:"rate_chase",
    detail:`rate=${h.rate.toFixed(3)} of ${want} fps=${h.renderedFps!=null?h.renderedFps.toFixed(1):"?"} runway=${runway}s slow=${h.slow} late=${h.late||0}`,
    message:`presenting at ${(h.rate*100/want).toFixed(1)}% speed with ${runway}s buffered now — the window may be digesting a stall just before this (check stall/player_event lines)`},
    playbackContext(), {runway}));
}
// The buffer stopped growing, and this says why. `quota` is the interesting
// one: it is the browser refusing more data, which is the actual ceiling on a
// 4K forward buffer and the number the buffer settings can't argue with. Rate
// limited per session — a full buffer re-fires on every append attempt, and a
// thousand identical log lines would bury the first one.
// Reported once a pattern exists rather than per hitch: one beacon per frame
// would be a denial of service on the log ring, and a single hitch is noise —
// what identifies a cause is the count and the spacing.
function reportHitches(){
  const p=PLAYER; if(!playbackOwnsAttachedMedia(p)||!p.hitches) return;
  const h=p.hitches, n=h.back+h.held+(h.late||0)+(h.drop||0)+h.gap;
  if(n<5 || p._hitchReported===n) return;
  if(n!==5 && n%50!==0) return;
  p._hitchReported=n;
  const iv=hitchInterval();
  const blame=hitchBlame();
  clientLog(Object.assign({level:"warn",event:"hitch",
    detail:`back=${h.back} held=${h.held} late=${h.late||0} drop=${h.drop||0} skip=${h.gap} slow=${h.slow}${h.fps?` fps=${h.fps.toFixed(2)}`:''}${h.rate!=null?` rate=${h.rate.toFixed(3)}`:''}`,
    message:`${n} frame hitches${iv?`, one every ~${iv.toFixed(1)}s`:''}${blame?` — ${blame}`:''} — ${h.last||''}`},
    playbackContext()));
}
function reportBufferLimit(kind, d){
  const p=PLAYER; if(!p) return;
  p.bufferLimits=p.bufferLimits||{};
  p.bufferLimits[kind]=(p.bufferLimits[kind]||0)+1;
  const n=p.bufferLimits[kind];
  if(n!==1 && n%25!==0) return;
  const ctx=playbackContext();
  clientLog(Object.assign({level:"warn",event:"buffer_limit",detail:kind,
    message:`${kind} after ${ctx.runway}s buffered (x${n})`,
    // What the player ASKED for, beside what it got — the comparison that
    // says whether our seconds target or the browser's quota is the limit.
    // Read from the live config rather than restated: a hardcoded 60 here
    // would have kept claiming 60 long after the target became bitrate-aware.
    target:(p.hls&&p.hls.config&&p.hls.config.maxBufferLength)||null}, ctx));
}
// Durations. Home video made this care about seconds: a 6-second phone clip
// reading "0m" is worse than saying nothing.
function fmtDur(ms){ if(!ms) return ""; const s=Math.round(ms/1000),h=Math.floor(s/3600),m=Math.floor(s%3600/60);
  if(s<60) return `${Math.max(1,s)}s`;
  return h?`${h}h ${m}m`:`${m}m`; }
function fmtSize(b){ if(!b) return ""; const g=b/(1024**3); return g>=1?g.toFixed(1)+" GB":Math.round(b/(1024**2))+" MB"; }
function fmtUptime(s){ const d=Math.floor(s/86400),h=Math.floor(s%86400/3600),m=Math.floor(s%3600/60); return d?`${d}d ${h}h`:(h?`${h}h ${m}m`:`${m}m`); }
function fmtTs(ms){ const d=new Date(ms); return d.toLocaleTimeString([], {hour12:false})+"."+String(ms%1000).padStart(3,"0"); }
function fmtAgo(unix){ if(!unix) return ""; const s=Math.max(0,Math.floor(Date.now()/1000)-unix);
  if(s<60) return "just now"; if(s<3600) return Math.floor(s/60)+"m ago"; if(s<86400) return Math.floor(s/3600)+"h ago"; return Math.floor(s/86400)+"d ago"; }
// HTML-escape for text and attribute values. API DTOs contain numeric limits
// and counts, so normalize template scalars before applying string methods.
// Escapes BOTH quote characters:
// `'` matters because inline on* handlers are single-quoted, and an unescaped
// apostrophe in interpolated data (e.g. a title like "Can't") closes the
// attribute early and silently breaks the handler. Compose with JSON.stringify
// for a JS string inside an attribute: esc(JSON.stringify(value)).
function esc(s){ return String(s??"").replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c])); }
// Artwork with a graceful fallback: initials on a tinted card when there's
// no poster (so a seasons grid never shows blank rectangles).
function artHtml(it, cls){
  // A photo whose thumbnail hasn't been generated yet still has itself to
  // show — the endpoint falls back to the original.
  const src=it.poster||it.backdrop||(it.kind==='photo'?`/api/v1/items/${it.id}/photo?size=thumb`:null);
  // `decoding="async"` keeps a grid of posters off the main thread's critical
  // path: the browser may decode each image whenever it likes instead of
  // blocking the paint that reveals the card. It is advisory and understood
  // everywhere `loading="lazy"` is, so there is nothing to feature-detect.
  if(src) return `<img class="art ${cls||''}" loading="lazy" decoding="async" src="${esc(tok(src))}" alt="">`;
  if(it.kind==='season' && it.season_number!=null)
    return `<div class="art ph season ${cls||''}"><div class="snum">${it.season_number}</div><div class="sl">Season</div></div>`;
  if(it.kind==='audiobook') return `<div class="art ph ${cls||''}" aria-label="Audiobook">♫</div>`;
  if(it.kind==='book') return `<div class="art ph ${cls||''}" aria-label="Ebook">Aa</div>`;
  const label=(it.title||"?").split(/\s+/).slice(0,2).map(w=>w[0]||"").join("").toUpperCase()||"?";
  return `<div class="art ph ${cls||''}">${esc(label)}</div>`;
}

