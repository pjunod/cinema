"use strict";
// ---- stall self-diagnosis --------------------------------------------------
// A remux/transcode that never fires `playing` used to sit on a mystery gray
// screen forever. If nothing has started after a generous grace, probe the
// actual stream URL and turn the blank overlay into a specific verdict:
// blocked-by-extension vs server-error vs reaches-browser-but-won't-decode.
function clearStall(){ if(PLAYER&&PLAYER.stallTimer){ clearTimeout(PLAYER.stallTimer); PLAYER.stallTimer=null; } }
// Does this stream have a video track at all?
//
// `videoWidth` answers it once the element has decoded a frame or read the
// init segment; before that, the decision's own source summary does. A file
// with neither is the audio-only case, and it must not be held to a frame
// count it can never produce.
function streamHasVideo(p, v){
  if(v && (v.videoWidth>0 || v.videoHeight>0)) return true;
  const s=(p&&p.source)||{};
  return !!(s.video_codec || s.width || s.height);
}
// Is this playback ACTUALLY underway — as in, is there a picture?
//
// `PLAYER.started` is set by the first `playing` event, and audio alone fires
// it. That is the whole shape of the 2160p Main10 incident: E-AC-3 played,
// `started` went true, and the two rescues that exist both read it as "we are
// fine, do not churn" — so a stream the decoder was rendering as nothing sat
// black for the length of the film with a working scrubber.
//
// Decoded frames are the signal, from the browser's own counter: it is the one
// number that separates "the decoder produced pictures" from "the pipeline is
// running". `totalVideoFrames` counts DECODED frames rather than presented
// ones (Safari decodes ahead in bursts — see the stats panel), which is
// exactly right here: anything above zero means the decoder took the stream,
// and the black-screen case is a hard zero.
//
// Two deliberate fallbacks, both to today's behaviour rather than to a rescue:
// audio-only content has no frames to count and is judged by `started` as
// before, and a browser that does not implement getVideoPlaybackQuality is
// judged by whether a picture exists at all. Neither guesses a failure.
function playbackIsReal(){
  const p=PLAYER; if(!p || !p.started) return false;
  const v=document.getElementById("video");
  if(!streamHasVideo(p, v)) return true;     // audio-only: `playing` IS the answer
  if(!v) return true;
  let q=null; try{ q=v.getVideoPlaybackQuality&&v.getVideoPlaybackQuality(); }catch(e){}
  if(q && typeof q.totalVideoFrames==="number") return q.totalVideoFrames>0;
  return v.videoWidth>0;                     // no counter: do not rescue on a guess
}
// One in-flight claim shared by every AUTOMATIC session-open: the decode
// rescue, the supply rescue, and an Auto rung switch. `abr.switching` cannot
// carry this. The decode rescue never sets it, and both rescues yield at
// `openSession` before anything marks the player — so one 5-second interval
// could run `maybeDecodeRescue()` (unawaited) and then `autoControllerTick()`,
// see the still-remux method and `switching === false` in both, and open two
// replacement sessions. The later generation only supersedes the earlier one
// AFTER the earlier request has already created its server session, so the
// at-most-one-automatic-restart guarantee was lost and a decode verdict could
// cross-trigger a supply rescue. Deliberately not applied to the manual Force
// transcode, the stall watchdog, or a browser stream rejection: those are the
// viewer's or the watchdog's decision and must never be swallowed by an
// automatic move already in flight.
function claimAutoFallback(p){
  if(p&&p.pendingMediaChange) return false;
  if(hasPendingPlaybackOpen(p)) return false;
  if(!p || p.autoFallbackInFlight) return false;
  p.autoFallbackInFlight=true;
  return true;
}
function hasPendingPlaybackOpen(p){
  return !!(p&&((p.pendingOpenAttempt&&PLAY_OPEN_GATE.current(p.pendingOpenAttempt))
    ||(p.retiringOpenAttempt&&PLAY_OPEN_GATE.current(p.retiringOpenAttempt))));
}
// PLAYER is the requested destination during preparation; the shared video
// can still belong to its predecessor. Only the attached owner may interpret
// media events/clock samples as playback evidence or trigger automatic work.
// Transport/selection commands deliberately use desired PLAYER independently.
function playbackOwnsAttachedMedia(p){
  return !!p&&PLAYER===p&&!p.pendingMediaChange&&!hasPendingPlaybackOpen(p);
}
// Always released, including on failure: a 5xx on the replacement session must
// leave Auto able to try again rather than wedging every later rescue.
function releaseAutoFallback(p){ if(p) p.autoFallbackInFlight=false; }
// Rescue path: the browser rejected a remux/direct stream outright, so restart
// this playback as a server transcode (guaranteed-compatible H.264/AAC HLS).
async function startTranscodeFallback(reason, note){
  const v=document.getElementById("video");
  if(!PLAYER || !PLAYER.fileId || !v) return;
  if(play.pendingIntent&&play.pendingIntent.fileId!==PLAYER.fileId
     &&hasPendingPlaybackOpen(PLAYER)) return false;
  if(hasPendingPlaybackOpen(PLAYER)
     &&reason!=="stall-manual"&&reason!=="stall-terminal") return false;
  const startSec=positionForPlaybackIntent(v,PLAYER);
  endWait(false);
  beginPlaybackControlSeek(PLAYER,startSec,
    reason==="stall-manual"||reason==="stall-terminal");
  const me=PLAYER;
  // Before the session is opened, because `transcodeOpts` reads it: a stream
  // the browser refused is not a stream it lacked the pixels for, so the
  // replacement keeps the source's resolution (see sessionHeight). Set only
  // for that reason — the decode rescue and the manual Force-transcode button
  // both arrive here meaning something else entirely.
  if(reason==="stream-rejected") me.refusedOriginal=true;
  // A silent switch is a lie of omission. Without this the overlay keeps the
  // decision's remux Reason over a transcode session picked mid-flight — and
  // a cache hit makes the swap fast enough to miss entirely, which read as
  // "this film only plays 1080p" with nothing anywhere saying why. The
  // Reason row is the server's verdict on the FILE and stays; the Switched
  // row is the standing answer for what the client did about it. The
  // decode-rescue writes its own note before calling here, so only fill one
  // in when this caller brought one.
  return requestPlaybackMediaChange(me,{method:'transcode',copyHls:false,
    reason:reason||"fallback",note:note||me.rescuedNote,
    automatic:reason==='decode-rescue'||reason==='auto-supply',
    from:reason==='auto-supply'?me.method:null,switchReason:"supply stalls"});
}
// The decode-margin rescue, run from the 5s tick. When an AUTO session on a
// copy path has measured itself out of decoder headroom AND the viewer is
// seeing hitches, switch this session to a transcode at position and remember
// the limit so the next Auto play routes straight there. Explicit quality
// choices are never overridden: Original means Original, stutters and all —
// and an explicit-Original session that measures comfortable margin for a
// minute CLEARS the remembered limit, so a GPU upgrade is noticed.
function maybeDecodeRescue(){
  const p=PLAYER, v=/** @type {HTMLVideoElement|null} */ (document.getElementById("video"));
  if(!playbackOwnsAttachedMedia(p)) return;
  if(!p || !v || !p.started || !p.hitches || v.paused) return;
  const routeAuto=!!(SERVER&&SERVER.playback_display_aware_auto
    &&p.qualityProtocol==='route-v1'&&Array.isArray(p.qualityCandidates));
  if(p.method!=='remux' && p.method!=='direct_play'&&!routeAuto) return;
  const h=p.hitches, secs=playedSecs(v), q=qualityForce();
  // An Auto session that is deliberately re-testing an old limit gets judged
  // by the same rule as an explicit Original — that is what makes the re-test
  // able to clear the entry without the viewer choosing anything.
  if(q==='original' || (q==='auto' && p.decodeRetest)){
    // Clearing is the rescue's own trigger inverted — literally, now: the same
    // lost-frame rate against the same threshold, the other way round. It used
    // to be a second heuristic (fewer than four faults of ANY kind in a
    // minute), and the two drifted apart badly enough that a session the
    // viewer described as flawless still could not clear its entry: six
    // compositor holds and three lost frames over two and a half minutes is
    // nine "faults", and the bar was four.
    //
    // Before that it was a decode number under 60% of the frame budget, which
    // reads `processingDuration` as decode cost — the misread guardrail 8
    // exists for, and worse than usual here, because on a pipelined decoder
    // that figure measures how DEEP the pipeline is. A client holding a
    // healthy reserve reports a LARGER one. On the 4K remux it went from
    // 41.6 ms to 91 ms against a 41.7 ms budget once the `dvcC` fix restored
    // the hardware path — playback improved and the condition receded.
    const x=lostFrameRate(h, secs);
    if(secs>=MARGIN_CLEAR_SECS && x && x.rate<MARGIN_LOST_PER_MIN && clearDecodeLimit(p.source)){
      p.decodeRetest=false;
      clientLog(Object.assign({level:"info",event:"decode_limit_cleared",
        message:`this device played ${decodeLimitLabel(p.source)} for ${Math.round(secs)}s `+
                `losing ${x.lost} frame(s), ${x.rate}/min — Auto may use Original again`},
        playbackContext()));
      toast("This browser handles the original now — Auto will use it");
    }
    // A re-test that goes badly must still be able to rescue and re-stamp the
    // entry, so it falls through to the trigger below rather than returning.
    if(q==='original') return;
  }
  if(q!=='auto' || p.decodeRescued&&!routeAuto) return;
  const verdict=decodeMarginVerdict(h, secs);
  if(!verdict) return;
  if(routeAuto&&p.abr){
    const current=p.qualityCandidates.find(row=>row.id===p.qualityCandidateId);
    if(current){
      const now=performance.now();
      const result=PlaybackPolicy.decideCandidateTransition({state:p.abr.candidateState||{},
        candidates:p.qualityCandidates,currentId:current.id,target:measuredPresentationTarget(),
        aspect:current.width/current.height,sample:{now_ms:now,automatic:true,presenting:true,
          paused:v.paused,seeking:v.seeking||!!p.controlSeek,move_in_flight:!!p.pendingMediaChange
            ||p.abr.switching||p.autoFallbackInFlight,cause:'decode',cause_age_ms:0,
          runway_ms:bufferRunway(v)*1000,stalled:!!p.waitAt,natural_boundary:false}});
      p.abr.candidateState=result.state;
      if(result.candidate){
        if(!((p.bufferLimits&&p.bufferLimits.quota)|0)) rememberDecodeLimit(p.source,verdict);
        void switchAutoCandidate(p,v,current,result).catch(()=>{});
        return;
      }
      // The route reducer permits one decode recovery per exact recipe. The
      // existing compatibility/error owner handles a repeated failure.
      if(p.method==='transcode'||result.reason==='decoder recovery owns repeated failure') return;
    }
  }
  // A supply rescue already opening a replacement owns this playback's one
  // automatic move. Return without latching `decodeRescued` or writing a note:
  // if that rescue lands the session becomes a transcode and the method check
  // above ends this path anyway, and if it fails the verdict is re-evaluated on
  // the next sample with nothing consumed.
  if(!claimAutoFallback(p)) return;
  p.decodeRescued=true;
  // A session the browser refused data on has an explanation for its hitches
  // that is not the decoder, and a decode limit is the wrong thing to learn
  // from it. Even the narrowed media-load identity is evidence about client
  // presentation, while the actual constraint was the size of this stream
  // against this browser's buffer quota — which a transcode fixes by being
  // smaller, and which says nothing about what the GPU can decode.
  // Getting this wrong is how Chrome taught itself that an M3 Max cannot play
  // HEVC 2160p. Fall back, because falling back genuinely helps; just do not
  // remember it as something it wasn't.
  const quota=(p.bufferLimits&&p.bufferLimits.quota)|0;
  if(!quota) rememberDecodeLimit(p.source, verdict);
  // Why this session is not the picture the viewer asked for, kept where they
  // can find it later. The toast says it once and then it is gone — which is
  // exactly what happened on reference film F: dropped to 1080p mid-film with a message
  // that vanished, and nothing anywhere afterwards that said why.
  p.rescuedNote=quota
    ? `switched to a transcode: this browser refused buffered data ${quota}x on the original`
    : `switched to a transcode: ${verdict.lost} frames lost in ${verdict.secs}s `+
      `(${verdict.rate}/min) playing the original`;
  clientLog(Object.assign({level:"warn",event:"decode_rescue",
    detail:`lost=${verdict.lost} rate=${verdict.rate}/min secs=${verdict.secs} `+
           `decode=${verdict.decodeMs}ms budget=${verdict.budgetMs}ms quota=${quota}`,
    message:quota
      ? `this browser refused buffer ${quota}x on ${decodeLimitLabel(p.source)||'this stream'} `+
        `and lost ${verdict.lost} frames with it — switching to transcode, but not `+
        `remembering a decode limit: the constraint was buffer quota, not the decoder`
      : `this browser lost ${verdict.lost} frames in ${verdict.secs}s (${verdict.rate}/min) `+
        `playing ${decodeLimitLabel(p.source)||'this stream'} — switching to transcode`},
    playbackContext()));
  raisePlaybackSurface("degraded_notice",
    {title:"This browser can't hold Original smoothly — switching to optimized"});
  startTranscodeFallback("decode-rescue")
    .finally(()=>releaseAutoFallback(p)).catch(()=>{});
}
function autoSwitchLabel(value){
  return typeof value==='number'?`${value}p`:String(value||'Auto');
}
function recordAutoSwitch(p,from,to,reason,position,targetSessionId){
  const atMs=performance.now();
  const fromHeight=typeof from==='number'&&Number.isFinite(from)?from:null;
  const toHeight=typeof to==='number'&&Number.isFinite(to)?to:null;
  const entry={seq:++AUTO_SWITCH_SEQ,at_ms:atMs,
    from:autoSwitchLabel(from),to:autoSwitchLabel(to),from_height:fromHeight,to_height:toHeight,reason,
    position:Math.max(0,position||0),target_method:p.method||null,
    target_session_id:targetSessionId||p.sessionId||null,target_attempt_id:p.attemptId||null};
  p.abr.switches.push(entry);
  p.abr.switchBudgetTimes=(p.abr.switchBudgetTimes||[])
    .filter(at=>atMs-at<3600000).concat(atMs).slice(-6);
  if(reason==='bandwidth cliff') p.abr.lastCliffAtMs=atMs;
  if(p.abr.switches.length>8) p.abr.switches.shift();
  clientLog(Object.assign({level:"warn",event:"quality_switch",reason:"auto",
    detail:`from=${entry.from} to=${entry.to} cause=${reason}`,
    message:`Auto quality ${entry.from} → ${entry.to} — ${reason}`},
    playbackContext()));
}
function autoCauseEvidence(p,nowMs){
  const maxAge=PlaybackPolicy.AUTO_DEFAULTS.causeMaxAgeMs;
  if(p&&p.abr&&nowMs<Number(p.abr.stallVerdictUntilMs||0))
    return {kind:"control-stall-verdict",ageMs:0};
  const episode=p&&p.hlsStartup;
  if(episode&&episode.establishedSuspension
    &&episode.establishedSuspension.attachment===p.mediaAttachment){
    return {kind:"loader-suspended",ageMs:0};
  }
  const failure=STREAM_FAILURE;
  if(failure&&(!failure.attachment||failure.attachment===p.mediaAttachment)){
    const age=Math.max(0,Date.now()-Number(failure.at||Date.now()));
    if(age<=maxAge){
      const code=String(failure.code||"");
      if(failure.status===401||failure.status===403||failure.status===410
        ||/authority|owner_(?:lost|transition)|node_removal_fenced|learner_route_ineligible/.test(code))
        return {kind:"authority-refused",ageMs:age,code};
      if(PlaybackPolicy.HLS_STARTUP_TERMINAL_CODES.includes(code))
        return {kind:"producer-failed",ageMs:age,code};
      if(/publication|segment_|response_/.test(code))
        return {kind:"delivery-refused",ageMs:age,code};
    }
  }
  const healthAge=p&&p.healthObservedAt!=null?Math.max(0,nowMs-p.healthObservedAt):null;
  const health=p&&p.health;
  if(health&&healthAge<=maxAge){
    if(health.producer_state==="failed") return {kind:"producer-failed",ageMs:healthAge};
    if(Number(health.recent_speed)>0&&Number(health.recent_speed)<1
      &&["running","held"].includes(String(health.producer_state||"")))
      return {kind:"capacity-shortfall",ageMs:healthAge};
  }
  const abr=p&&p.abr;
  const sampleAge=abr&&abr.recentEstimateAtMs!=null
    ?Math.max(0,nowMs-abr.recentEstimateAtMs):null;
  const sourceKbps=Number(p&&p.source&&p.source.bitrate)/1000;
  if(sampleAge!=null&&sampleAge<=PlaybackPolicy.AUTO_DEFAULTS.recentSampleMaxAgeMs
    &&Number(abr.recentEstimateKbps)>0&&sourceKbps>0
    &&abr.recentEstimateKbps<sourceKbps*PlaybackPolicy.AUTO_DEFAULTS.severeEstimateRatio)
    return {kind:"bandwidth-limited",ageMs:sampleAge};
  return {kind:healthAge==null?"unknown":"stale",ageMs:healthAge};
}
function recordAutoDecision(p,currentHeight,result,runway,throughputKbps){
  if(!p||!p.abr) return;
  if(!result||result.action!=="suppressed"){
    p.abr.lastDecisionKey=null;
    return;
  }
  const evidence=result.evidence||{};
  const key=`${result.action}:${result.reason}:${currentHeight}`;
  if(p.abr.lastDecisionKey===key) return;
  p.abr.lastDecisionKey=key;
  clientLog(Object.assign({level:"info",event:"auto_quality_decision",
    action:result.action,detail:`${result.reason}; ${currentHeight}p retained`,
    current_height:currentHeight,target_height:currentHeight,
    evidence_age_ms:evidence.age_ms==null?null:Math.round(evidence.age_ms),
    runway:runway==null?null:Number(runway),
    throughput_kbps:throughputKbps==null?null:Math.round(throughputKbps),
    message:`Auto quality retained ${currentHeight}p — ${result.reason}`},
    playbackContext()));
}
// A media-fragment progress delta can establish a severe link cliff just after
// the periodic Auto tick. Queue the same decision immediately for that fresh
// observation, without running two health polls or two selection asks at once.
function queueAutoControllerTick(p,urgent){
  if(!p||PLAYER!==p||!p.abr||!playbackOwnsAttachedMedia(p)) return;
  if(p.abr.controllerTickRunning){
    if(urgent) p.abr.controllerUrgentPending=true;
    return;
  }
  p.abr.controllerTickRunning=true;
  Promise.resolve().then(()=>autoControllerTick()).catch(()=>{}).finally(()=>{
    p.abr.controllerTickRunning=false;
    if(p.abr.controllerUrgentPending){
      p.abr.controllerUrgentPending=false;
      queueAutoControllerTick(p,false);
    }
  });
}
function scheduleUrgentAutoControllerTick(p,kbps,now){
  if(p&&PLAYER===p&&p.abr&&!p.abr.switching&&qualityForce()==='auto'
    &&SERVER&&SERVER.playback_auto_abr&&SERVER.playback_display_aware_auto
    &&p.qualityProtocol==='route-v1'&&Array.isArray(p.qualityCandidates)){
    const current=p.qualityCandidates.find(candidate=>candidate.id===p.qualityCandidateId);
    const sample=p.abr.qualityPressureTransfer;
    if(current&&sample&&sample.attachment===p.mediaAttachment&&now-sample.atMs<=15000
      &&sample.bps<PlaybackPolicy.qualityPressureCostBps(current,candidateTransferEvidence(p,now),sample.mediaBps)*0.7
      &&(p.abr.lastUrgentAutoTickAtMs==null||now-p.abr.lastUrgentAutoTickAtMs>=1000)){
      p.abr.lastUrgentAutoTickAtMs=now;queueAutoControllerTick(p,true);
    }
    return;
  }
  if(!p||PLAYER!==p||!p.abr||p.abr.switching||p.method!=="transcode"
    ||qualityForce()!=="auto"||!(SERVER&&SERVER.playback_auto_abr)) return;
  const v=/** @type {HTMLVideoElement|null} */ (document.getElementById("video"));
  const height=Number(p.health&&p.health.target_height||p.autoHeight||v&&v.videoHeight);
  const ladder=PlaybackPolicy.normalizedLadder(p.ladder);
  const index=ladder.findIndex(rung=>rung.height===height);
  if(index<=0) return;
  const current=ladder[index];
  const ratio=index<PlaybackPolicy.AUTO_DEFAULTS.lowRungEmergencyCount
    ?1:PlaybackPolicy.AUTO_DEFAULTS.severeEstimateRatio;
  if(!(kbps>0&&kbps<current.total_kbps*ratio)) return;
  const prior=p.abr.lastUrgentAutoTickAtMs;
  if(prior!=null&&now-prior<PlaybackPolicy.AUTO_DEFAULTS.decisionMs) return;
  p.abr.lastUrgentAutoTickAtMs=now;
  queueAutoControllerTick(p,true);
}
async function switchAutoRung(currentHeight,decision){
  const p=PLAYER, v=document.getElementById("video");
  if(!p||!v||!p.abr||p.abr.switching||!(decision&&decision.height>0)) return;
  // The rung switch opens a session like either rescue does, so it takes the
  // same single automatic claim rather than a guard of its own.
  if(!claimAutoFallback(p)) return;
  // Today's move, kept verbatim, and the answer to every way the handoff can
  // fail. The create carries the rung explicitly, which is why the standing
  // ask is cleared before this runs.
  const reopen=async()=>{
    const pos=positionForPlaybackIntent(v,p);
    beginPlaybackControlSeek(p,pos,false);
    const attached=await requestPlaybackMediaChange(p,{method:'transcode',copyHls:false,
      height:decision.height,reason:"auto-quality",automatic:true,
      from:currentHeight,switchReason:decision.reason});
    if(attached) raisePlaybackSurface("degraded_notice",
      {title:`Quality → ${decision.height}p — ${decision.reason}`});
    return attached;
  };
  let retainClaim=false;
  try{
    // A stalled incumbent has nothing to hand off from. The server cancels a
    // preparation on `waiting` or `stalled` anyway, so asking would spend the
    // whole offer bound to be told no and leave the viewer stalled for it.
    if(!preparedHandoffOffered(p)||!directedChangeIncumbentReady(p,v)){
      await reopen();
      return;
    }
    // Set BEFORE the ask, so the very first exchange after this decision
    // already carries the rung and the server sees a selection change rather
    // than the same Auto it has been reading all along.
    p.autoRequestedHeight=decision.height;
    p.abr.switching=true;
    if(decision.reason==="bandwidth cliff"){
      const decidedAtMs=performance.now();
      clientLog(Object.assign({level:"info",event:"auto_quality_signal",
        detail:"severe_estimate",
        message:`severe Auto estimate sample_at_ms=${Math.round(p.abr.recentEstimateAtMs)} `+
          `decision_at_ms=${Math.round(decidedAtMs)} `+
          `sample_source=${p.abr.recentEstimateSource||"unknown"} `+
          `sample_kbps=${Math.round(p.abr.recentEstimateKbps)} `+
          `from=${currentHeight}p to=${decision.height}p`},playbackContext()));
    }
    const outcome=await requestQualityChange(p,"auto-quality",reopen,
      {from:currentHeight,to:decision.height,switchReason:decision.reason});
    if(outcome==="continuous"&&p.directedChange&&!p.directedChange.settled){
      retainClaim=true;
      return;
    }
    if(outcome==="prepared"&&p.directedChange&&!p.directedChange.settled){
      const change=p.directedChange;
      change.commitTimer=setTimeout(()=>fallBackDirectedChange(p,change,"commit_timeout"),
        Math.max(0,10000-(performance.now()-change.tappedAt)));
      retainClaim=true;
      return;
    }
  } finally { if(!retainClaim) releaseAutoFallback(p); }
}
async function rescueAutoSupply(causeEvidence){
  const p=PLAYER, v=document.getElementById("video");
  if(!p||!v||!p.abr||p.abr.switching||p.abr.supplyRescued) return;
  const cause=causeEvidence||autoCauseEvidence(p,performance.now());
  if(!["bandwidth-limited","capacity-shortfall"].includes(cause.kind)){
    recordAutoDecision(p,Number(p.autoHeight||v.videoHeight)||null,
      {action:"suppressed",reason:cause.kind==="producer-failed"||cause.kind==="authority-refused"
        ?cause.kind:"insufficient-evidence",evidence:{age_ms:cause.ageMs}},
      bufferRunway(v),p.abr.recentEstimateKbps);
    return;
  }
  // A decode rescue started earlier in this same interval is already opening a
  // replacement session and has not reached the point where it marks the
  // player. Claim before any state changes or overlay text, so a refused claim
  // costs nothing and this path can run untouched on the next sample.
  if(!claimAutoFallback(p)) return;
  const before=p.method, pos=(p.offset||0)+(v.currentTime||0);
  p.abr.switching=true;
  p.abr.supplyRescued=true;
  raisePlaybackSurface("owner_recovery_step",{title:"Adjusting quality…",
    detail:"repeated supply stalls — starting transcode Auto"});
  const reopen=()=>startTranscodeFallback("auto-supply",
    "switched to transcode Auto after 3 supply stalls in 60 seconds");
  let handedOff=false;
  try{
    // The same owner as the rung switch, and the same gate. This path is
    // reached BY repeated supply stalls, so the incumbent is almost never
    // ready and this is almost always the reopen it has always been -- which
    // is the point: a stalled viewer must not wait out an offer bound. It
    // chooses Auto rather than a rung, so it publishes no requested height.
    if(!preparedHandoffOffered(p)||!directedChangeIncumbentReady(p,v)) await reopen();
    else handedOff=await requestQualityChange(p,"auto-supply",reopen)==="prepared";
  } finally { releaseAutoFallback(p); }
  if(PLAYER!==p) return;
  // The retained recipe owns completion even if a newer command superseded
  // this invocation. It records the switch exactly once, at actual attachment.
  if(p.pendingMediaChange) return;
  // A prepared successor owns completion for the same reason, and the checks
  // below cannot speak for it: a handoff replaces the session without touching
  // `p.method`, so reading the method here would find the predecessor's and
  // report that the switch did not land.
  if(handedOff) return;
  p.abr.switching=false;
  if(p.method!=='transcode'){
    p.abr.supplyRescued=false;
    return;
  }
  // Said once the switch has LANDED, the same gate `switchAutoRung` uses. The
  // `recovering` step above outranks a notice for as long as it is up, so a
  // raise beside it is a sentence whose 5 s timer expires behind a spinner —
  // which is worse than the toast it replaced, not better.
  raisePlaybackSurface("degraded_notice",{title:"Quality → Auto transcode — supply stalls"});
  const now=performance.now();
  p.abr.lastSwitchAtMs=now;
  p.abr.stableSinceMs=now;
  p.abr.stallEvents.supply=[];
}
async function autoControllerTick(){
  const p=PLAYER, v=/** @type {HTMLVideoElement|null} */ (document.getElementById("video"));
  if(p&&p.abr&&(document.hidden||v&&v.paused||v&&v.seeking)){
    if(p.abr.candidateState) p.abr.candidateState.upgradeSinceMs=null;
    p.abr.upgradeSinceMs=null;
    return;
  }
  const attachment=p?.mediaAttachment, session=p?.sessionId, stream=p?.streamId;
  if(!playbackOwnsAttachedMedia(p)) return;
  if(!(SERVER&&SERVER.playback_auto_abr)||!p||!v||!p.abr||qualityForce()!=='auto'||!p.started||v.paused||p.abr.switching) return;
  if(p.autoFallbackInFlight) return;
  if(p.healthObservedAt==null
    ||performance.now()-p.healthObservedAt>=PlaybackPolicy.AUTO_DEFAULTS.sampleMs)
    await pollSessionHealth(true);
  // Re-checked after the poll for the same reason it is checked before it: the
  // unawaited `maybeDecodeRescue()` earlier in this interval can have claimed
  // the automatic move while this tick was waiting on the health request.
  if(PLAYER!==p||p.mediaAttachment!==attachment||p.sessionId!==session||p.streamId!==stream
     ||!p.abr||p.abr.switching||p.autoFallbackInFlight||!playbackOwnsAttachedMedia(p)) return;
  const now=performance.now(), windowMs=PlaybackPolicy.AUTO_DEFAULTS.stallWindowMs;
  for(const kind of ['supply','decode']){
    p.abr.stallEvents[kind]=(p.abr.stallEvents[kind]||[]).filter(at=>now-at<windowMs);
  }
  const causeEvidence=autoCauseEvidence(p,now);
  if(SERVER.playback_display_aware_auto&&p.qualityProtocol==='route-v1'
    &&Array.isArray(p.qualityCandidates)){
    await candidateAutoControllerTick(p,v,causeEvidence,now);
    return;
  }
  if(causeEvidence.kind==="loader-suspended"){
    const resumed=resumeHlsStartup(v,p);
    recordAutoDecision(p,Number(p.autoHeight||v.videoHeight)||null,
      {action:"suppressed",reason:"loader-suspended",
        evidence:{kind:"loader-suspended",age_ms:causeEvidence.ageMs}},
      bufferRunway(v),p.abr.recentEstimateKbps);
    if(resumed) clientLog(Object.assign({level:"info",event:"auto_transport_repair",
      detail:"loader-suspended",message:"resumed the established HLS loader without changing quality"},
      playbackContext()));
    return;
  }
  if(p.method!=='transcode'){
    if(p.abr.stallEvents.supply.length>=3) await rescueAutoSupply(causeEvidence);
    return;
  }
  const ladder=PlaybackPolicy.normalizedLadder(p.ladder);
  if(!ladder.length) return;
  const health=p.health||{};
  const currentHeight=Number(health.target_height||p.autoHeight||v.videoHeight);
  if(!(currentHeight>0)) return;
  p.autoHeight=currentHeight;
  const estimateBps=(p.hls&&p.hls.bandwidthEstimate)||p.bandwidthSeedBps;
  const estimateKbps=estimateBps>0?estimateBps/1000:(p.priorKbps||null);
  const runway=bufferRunway(v);
  const recentMediaDeliveryKbps=PlaybackPolicy.completedMediaWindowKbps(
    p.abr.completedTransfers,now);
  const activeSupplyStall=!!p.waitAt && runway<SUPPLY_RUNWAY_SECS;
  const result=PlaybackPolicy.decideRung({
    ladder,currentHeight,estimateKbps,
    recentEstimateKbps:p.abr.recentEstimateKbps,
    recentEstimateAtMs:p.abr.recentEstimateAtMs,runwaySeconds:runway,
    recentMediaDeliveryKbps,
    previousRunwaySeconds:p.abr.previousRunway,
    recentSpeed:health.recent_speed,
    activeSupplyStall,supplyStalls:p.abr.stallEvents.supply.length,
    lastStallAtMs:p.abr.lastStallAtMs,nowMs:now,
    lastSwitchAtMs:p.abr.lastSwitchAtMs,mildSamples:p.abr.mildSamples,
    lastCliffAtMs:p.abr.lastCliffAtMs,
    upgradeSinceMs:p.abr.upgradeSinceMs,playerHeight:playerPixelHeight(v),
    switchesThisPlaybackHour:(p.abr.switchBudgetTimes||[])
      .filter(at=>now-at<3600000).length,
    blockedHeights:p.abr.failedHeights,causeEvidence
  });
  p.abr.previousRunway=runway;
  p.abr.mildSamples=result.mildSamples;
  p.abr.upgradeSinceMs=result.upgradeSinceMs;
  recordAutoDecision(p,currentHeight,result,runway,p.abr.recentEstimateKbps);
  if(result.reason && result.height!==currentHeight){
    await switchAutoRung(currentHeight,result);
    return;
  }
  const stallFree=p.abr.lastStallAtMs==null||now-p.abr.lastStallAtMs>=windowMs;
  if(stallFree&&now-p.abr.stableSinceMs>=windowMs) rememberAutoRung(currentHeight);
}
function candidateQualityContext(p){
  const caps=currentCapsDocument();
  return JSON.stringify({video:caps.video,display:caps.display,
    audio:selectedAudioIndex(p),subtitle:p.curSub,burn:p.burnedSub,offset:p.aoffset});
}
async function refreshQualityCandidates(p){
  const key=candidateQualityContext(p);
  if(p.abr.candidateContext===key) return true;
  const now=performance.now();
  if(p.abr.pendingCandidateContext!==key){
    p.abr.pendingCandidateContext=key;
    p.abr.candidateContextChangedAt=now;
    if(p.abr.candidateState) p.abr.candidateState.upgradeSinceMs=null;
    if(p.abr.candidateContext!=null) return false;
  }
  if(p.abr.candidateContext!=null&&now-p.abr.candidateContextChangedAt<5000) return false;
  if(p.abr.catalogRefreshing||p.directedChange&&!p.directedChange.settled) return false;
  p.abr.catalogRefreshing=true;
  const attachment=p.mediaAttachment, intent=p.controlIntentGeneration;
  try{
    const decision=await askDecision(p.fileId,qualityForce(),
      {audio:selectedAudioIndex(p),subtitle:p.curSub>=0?p.curSub:-1,audio_offset_ms:p.aoffset||0});
    if(PLAYER!==p||p.mediaAttachment!==attachment||p.controlIntentGeneration!==intent
      ||candidateQualityContext(p)!==key) return false;
    p.qualityCandidates=Array.isArray(decision.quality_candidates)
      ?continuousQualityBoundCatalog(decision.quality_candidates,p.continuousQuality&&!p.continuousQuality.closed?p.continuousQuality.family:null):null;
    p.capsSnapshot=decision._capsSnapshot||currentCapsDocument();
    p.abr.candidateContext=key;
    p.abr.catalogSelectionKey=qualityCatalogSelectionKey(p);
    p.abr.requestedCandidateId=null;
    if(p.abr.candidateState) p.abr.candidateState.upgradeSinceMs=null;
    return Array.isArray(p.qualityCandidates);
  }catch(e){ return false; }
  finally{ p.abr.catalogRefreshing=false; }
}
function candidateTransferEvidence(p,now){
  const transfer=p.abr&&p.abr.qualityTransfer;
  return transfer&&transfer.attachment===p.mediaAttachment
    ? {...transfer,age_ms:now-transfer.atMs}:null;
}
async function naturalBoundaryQualityCandidate(p,seekIntent){
  if(!p.abr||qualityForce()!=='auto'||!SERVER||!SERVER.playback_display_aware_auto
    ||p.qualityProtocol!=='route-v1'||!Array.isArray(p.qualityCandidates)) return null;
  const controller=new AbortController(), timer=setTimeout(()=>controller.abort(),1500);
  const generation=p.controlIntentGeneration;
  try{
    const decision=await askDecision(p.fileId,'auto',
      {audio:selectedAudioIndex(p),subtitle:p.curSub>=0?p.curSub:-1,audio_offset_ms:p.aoffset||0},controller.signal);
    if(PLAYER!==p||p.controlSeek!==seekIntent||p.controlIntentGeneration!==generation
      ||qualityForce()!=='auto'||!Array.isArray(decision.quality_candidates)) return null;
    const candidates=continuousQualityBoundCatalog(decision.quality_candidates,
      p.continuousQuality&&!p.continuousQuality.closed?p.continuousQuality.family:null);
    const current=(p.qualityCandidates||[]).find(candidate=>candidate.id===p.qualityCandidateId);
    const progress=p.abr.qualityPressureTransfer, now=performance.now();
    const limit=progress&&progress.attachment===p.mediaAttachment&&now-progress.atMs<=15000?progress.bps:null;
    const blocked=p.abr.candidateState?.blockedCandidates||[];
    const picked=PlaybackPolicy.selectQualityCandidate({candidates:candidates.filter(row=>!blocked.includes(row.id)),target:measuredPresentationTarget(),
      aspect:current?current.width/current.height:null,
      transfer:candidateTransferEvidence(p,now),linkLimitBps:limit});
    p.qualityCandidates=candidates;
    p.capsSnapshot=decision._capsSnapshot||currentCapsDocument();
    p.abr.candidateContext=candidateQualityContext(p);
    p.abr.catalogSelectionKey=qualityCatalogSelectionKey(p);
    p.abr.candidateState={...(p.abr.candidateState||{}),switchTimesMs:[],upgradeSinceMs:null,mildSamples:0};
    return picked&&picked.id!==p.qualityCandidateId?picked:null;
  }catch(e){return null;}
  finally{clearTimeout(timer);}
}
async function candidateAutoControllerTick(p,v,causeEvidence,now){
  if(!await refreshQualityCandidates(p)||PLAYER!==p||!playbackOwnsAttachedMedia(p)) return;
  now=performance.now();
  const transfer=candidateTransferEvidence(p,now);
  const current=p.qualityCandidates.find(candidate=>candidate.id===p.qualityCandidateId);
  if(!current) return;
  let cause='unknown';
  if(['authority-refused','producer-failed','delivery-refused'].includes(causeEvidence.kind)) cause='authority';
  else if(['control-stall-verdict','loader-suspended'].includes(causeEvidence.kind)) cause='hold';
  const link=PlaybackPolicy.qualityTransferBps(transfer);
  const progress=p.abr.qualityPressureTransfer;
  const pressureBps=progress&&progress.attachment===p.mediaAttachment&&now-progress.atMs<=15000?progress.bps:null;
  const conservative=pressureBps>0?Math.min(link||Infinity,pressureBps):link;
  const demand=PlaybackPolicy.qualityPressureCostBps(current,transfer,pressureBps>0?progress.mediaBps:null);
  if(cause==='unknown'&&conservative>0&&demand>0&&conservative<demand*1.2) cause='link';
  // recent_speed includes pacing. Only the new active-production measurement
  // can attribute an encoder shortage rather than a deliberate producer hold.
  const speed=p.health&&p.health.active_encode_milli_realtime;
  if(cause==='unknown'&&current.route==='encode'&&!current.complete_cache
    &&p.healthObservedAt!=null
    &&PlaybackPolicy.qualityEncodeProof(p.health,current.id,now-p.healthObservedAt)
    &&speed<1000) cause='encode';
  const result=PlaybackPolicy.decideCandidateTransition({
    state:p.abr.candidateState||{},candidates:p.qualityCandidates,currentId:current.id,
    aspect:current.width/current.height,target:measuredPresentationTarget(),
    sample:{now_ms:now,automatic:qualityForce()==='auto',presenting:p.started,
      paused:v.paused,seeking:v.seeking||!!p.controlSeek,move_in_flight:!!p.pendingMediaChange||p.abr.switching,
      cause,cause_age_ms:cause==='link'?(pressureBps>0?now-progress.atMs:transfer.age_ms):(causeEvidence.ageMs||0),
      link_pressure_bps:pressureBps,link_pressure_media_bps:pressureBps>0?progress.mediaBps:null,link_pressure_age_ms:pressureBps>0?now-progress.atMs:null,
      runway_ms:bufferRunway(v)*1000,stalled:!!p.waitAt,last_stall_ms:p.abr.lastStallAtMs,
      natural_boundary:false,transfer}
  });
  p.abr.candidateState=result.state;
  recordAutoDecision(p,current.target_height||current.height,
    {action:result.candidate?'change':'suppressed',reason:result.reason,
      evidence:{age_ms:transfer?transfer.age_ms:null}},bufferRunway(v),link>0?link/1000:null);
  if(result.candidate) await switchAutoCandidate(p,v,current,result);
}
async function switchAutoCandidate(p,v,current,decision){
  if(PLAYER!==p||!decision.candidate||!claimAutoFallback(p)) return false;
  const standingSelection=playbackControlSelection(p);
  const chosen=decision.candidate, previousCandidateId=p.abr.requestedCandidateId||current.id;
  const copy=chosen.route!=='encode';
  const recovery=decision.emergency||decision.transition==='recover';
  const move={from:current.route==='encode'?current.target_height:'Original',
    to:copy?'Original':chosen.target_height,switchReason:decision.reason,
    candidateId:chosen.id,previousCandidateId,retainIncumbent:!recovery,
    emergency:!!decision.emergency};
  const reopen=()=>{
    p.abr.requestedCandidateId=chosen.id;
    const pos=positionForPlaybackIntent(v,p);
    beginPlaybackControlSeek(p,pos,false);
    return requestPlaybackMediaChange(p,{method:copy?'remux':'transcode',copyHls:copy,
      height:copy?null:chosen.target_height,candidateId:chosen.id,reason:'auto-quality',automatic:true,
      from:move.from,switchReason:decision.reason});
  };
  let retained=false;
  try{
    if(!preparedHandoffOffered(p)||!directedChangeIncumbentReady(p,v)){
      if(recovery) return await reopen();
      return false;
    }
    p.abr.requestedCandidateId=chosen.id;
    p.autoRequestedHeight=null;
    p.abr.switching=true;
    const outcome=await requestQualityChange(p,'auto-quality',reopen,move,standingSelection);
    if(outcome==='continuous'&&p.directedChange&&!p.directedChange.settled){
      retained=true;
      return true;
    }
    if(outcome==='prepared'&&p.directedChange&&!p.directedChange.settled){
      const change=p.directedChange;
      change.commitTimer=setTimeout(()=>fallBackDirectedChange(p,change,'commit_timeout'),
        Math.max(0,10000-(performance.now()-change.tappedAt)));
      retained=true;
    }
    return outcome==='prepared'||outcome==='continuous';
  }finally{
    if(!retained){p.abr.switching=false;releaseAutoFallback(p);}
  }
}
// Arm the watchdog for a stream we have just (re)started at `fromSec`. Recording
// the intended position is what makes this work after a *seek*: the old check
// ("has currentTime moved off zero?") is always true on a seeked stream, so a
// seek that never resumed silently disarmed its own watchdog and the overlay sat
// on "Buffering…" forever. A seek gets a shorter grace than a cold start: the
// file is already open on the server, so 20s of nothing is already wrong.
function armStall(fromSec, graceMs){
  clearStall(); if(!PLAYER) return;
  configureHlsStartupDeadline(PLAYER,graceMs||PlaybackPolicy.HLS_STARTUP.cold_deadline_ms);
  if(PLAYER.samplingStopped) armPlaybackSampling(document.getElementById("video"),PLAYER);
  PLAYER.stallFrom=(fromSec!=null)?fromSec:pbPosSec();
  PLAYER.stallTimer=setTimeout(()=>stallDiagnose().catch(()=>{}),
    graceMs||PlaybackPolicy.HLS_STARTUP.cold_deadline_ms);
}
// Recovery offered by a diagnosed stall: restart the stream at the position we
// were trying to reach, rather than making the user close and re-open the item.
function retryPlayback(){
  if(play.failedPreparation){
    const retry=play.failedPreparation;play.failedPreparation=null;
    PENDING_ATTEMPT_REASON="retry";
    if(PLAYER?.fileId===retry.fileId){
      retry.wantsPlayback=PLAYER.wantsPlayback;
      beginPlaybackControlSeek(PLAYER,retry.resumeMs/1000);
    }
    return play(retry.fileId,retry.title,retry.resumeMs,retry.knownDurMs,retry.meta,null,retry);
  }
  if(!PLAYER) return;
  const t=(PLAYER.stallFrom!=null)?PLAYER.stallFrom:pbPosSec();
  raisePlaybackSurface("owner_recovery_step",{title:"Retrying…",detail:null});
  seekTo(t,true);
}
async function stallDiagnose(){
  const v=document.getElementById("video");
  const p=PLAYER;
  // `armStall` and the VOD local fallback share the 20 s seek clock. If
  // both timers mature together, only the seek fallback may reopen; this
  // watchdog must not stop the attachment first.
  const pending=p&&p.controlSeek;
  if(pending?.localVodSeekFallbackPending&&pending.executed
     &&!playbackSeekBufferCovers(v,p,pending.targetMs)) return;
  const hlsStartup=hlsStartupIncomplete(p);
  if(!playbackOwnsAttachedMedia(p) || (!hlsStartup&&p.started)) return;
  if(hlsStartup&&p.hlsStartup.state==='paused') return;
  const generation=p._seekToken||0, action=p.controlIntentGeneration||0, attachment=p.mediaAttachment;
  const current=()=>playbackOwnsAttachedMedia(p)&&(hlsStartupIncomplete(p)||!p.started)
    &&p.mediaAttachment===attachment
    &&(p._seekToken||0)===generation
    &&(p.controlIntentGeneration||0)===action;
  const from=(p.stallFrom!=null)?p.stallFrom:0;
  if(!hlsStartup&&pbPosSec()>from+0.35) return; // progressive playback moved
  const controlTrigger=notifyPlaybackControl("stalled");
  const hdrs={}; if(TOKEN) hdrs["authorization"]="Bearer "+TOKEN;
  const url=p.probeUrl;
  const explained=currentStreamFailureOverlay();
  const terminalFailure=!!explained;
  let verdict=terminalFailure?explained.title:"Playback hasn't started.";
  let detail=terminalFailure?explained.detail:"Open Settings → Logs and read the last plurxd::stream / transcode line.";
  if(hlsStartup&&!terminalFailure){
    const diagnosis=diagnoseHlsStartup(p);
    verdict=diagnosis.title; detail=diagnosis.detail;
  }else if(url&&!terminalFailure){
    try{
      const ordinal=p._streamProbeOrdinal=(p._streamProbeOrdinal||0)+1;
      const {status,segState,body,failureStatus}=await probePlaybackSource(url,hdrs,{inspectMedia:p.method==='transcode'});
      if(!current()) return;
      if(body!=null) noteStreamFailure(failureStatus,body,
        {attachment,resource:'playback_probe',request_ordinal:ordinal});
      const serverErr = status>=500 || (typeof segState==='number'&&segState>=500);
      const reached = status>=200 && status<400 && !(typeof segState==='number'&&segState>=400);
      const refused = typeof segState==='number'&&segState>=400?segState:status;
      if(serverErr){ verdict="The server could not provide the stream."; detail=`The playback request returned HTTP ${refused}. Retry, or inspect Settings → Logs for the server's cause.`; }
      else if(reached){ verdict="The stream reaches the browser but playback did not start."; detail="No presentation progress was observed. Retry, or inspect Settings → Logs; the decoder cause is not known."; }
      else { verdict="The stream request was refused."; detail=`The playback request returned HTTP ${refused}. Your place is saved; retry, or inspect Settings → Logs.`; }
    }catch(e){
      if(!current()) return;
      verdict=e.name==='TimeoutError'?"The stream probe timed out.":"The stream request failed.";
      detail="The server or network did not answer the playback probe. Your place is saved; retry, or inspect Settings → Logs.";
    }
  }
  if(!current()) return;
  // Authority may have arrived during the probe. Its same-attachment sentence
  // wins over status-only guesses and remains armed for the reopen owner.
  const authoritative=currentStreamFailureOverlay();
  if(authoritative){verdict=authoritative.title;detail=authoritative.detail;}
  if(hlsStartup){
    const episode=p.hlsStartup;
    episode.state='exhausted';
    clearTimeout(episode.retry.timer); episode.retry.timer=null;
    abortHlsStartupLoaders(episode,true);
    try{episode.hls.stopLoad()}catch(e){}
  }
  const episode=hlsStartup?p.hlsStartup:null;
  const now=performance.now();
  console.warn("[cinema] playback stall diagnosis",{method:p.method,
    resource_class:hlsStartup?"hls_startup":"playback_source",
    networkState:v&&v.networkState,readyState:v&&v.readyState,
    error:v&&v.error&&v.error.code,verdict});
  finishStallRecovery("failed",verdict);
  clientLog({level:"warn",event:"stall",
    resource_class:hlsStartup?"hls_startup":"playback_source",
    attachment_id:p.mediaAttachment&&p.mediaAttachment.id,
    manifest_loaded:!!(episode&&['loaded','parsed'].includes(episode.manifestState)),
    media_loaded:!!(episode&&episode.mediaLoaded),
    decoder_failed:!!(episode&&episode.decoderFailed),presenting:false,
    request_count:episode?episode.dispatches:null,
    app_retry_state:episode?episode.retry.state:null,
    elapsed_ms:episode?Math.max(0,Math.round(now-episode.startedAt)):null,
    remaining_ms:episode?Math.max(0,Math.round(episode.deadlineMs-now)):null,
    message:verdict,detail:detail,control_trigger:controlTrigger});
  // A diagnosed stall is the owner out of rungs, and the verdict is its own.
  // Contract §3.4: stop the player, then raise; §3.3 row 15 is the row these
  // probe verdicts belong to. The sentence and the buttons are unchanged.
  stopPlayerForExhaustion();
  if(hlsStartup){
    raisePlaybackSurface("startup_exhausted",{player_stopped:true,
      title:verdict,detail:detail,actions:["close","retry"]});
  }else{
    raisePlaybackSurface("decoder_failed",{player_stopped:true,
      title:verdict,detail:detail,actions:playbackStallActions(PLAYER)});
  }
  toast(verdict);
}
// One deadline covers response headers, playlist body and the sample segment.
// Abort releases network work; the race also bounds transports ignoring abort.
async function probePlaybackSource(url,headers,evidence){
  const ctl=new AbortController();
  let timer;
  const deadline=new Promise((_,reject)=>{timer=setTimeout(()=>{
    const error=new Error("playback probe timed out"); error.name="TimeoutError";
    reject(error); ctl.abort();
  },8000);});
  try{
    return await Promise.race([deadline,(async()=>{
      const res=await fetch(url,{headers,signal:ctl.signal});
      const status=res.status;
      let segState=null, body=null, failureStatus=status;
      // Error responses are small typed refusals. Read at most the parser's
      // bound and cancel the body under this probe's existing deadline.
      async function refusalBody(response){
        if(!response.body||!response.body.getReader) return null;
        const reader=response.body.getReader(), chunks=[];
        let bytes=0;
        try{
          while(true){
            const part=await reader.read();
            if(part.done) break;
            bytes+=part.value.byteLength;
            if(bytes>PlaybackPolicy.STREAM_FAILURE_BODY_MAX_CHARS) return null;
            chunks.push(part.value);
          }
          const data=new Uint8Array(bytes);let offset=0;
          for(const chunk of chunks){data.set(chunk,offset);offset+=chunk.byteLength;}
          return new TextDecoder().decode(data);
        }finally{await reader.cancel().catch(()=>{});}
      }
      if(status>=400) body=await refusalBody(res);
      else if(evidence&&evidence.inspectMedia){
        const txt=await res.text();
        const seg=(txt.split('\n').find(l=>l&&!l.startsWith('#')&&/\.(?:ts|m4s)(\?|$)/.test(l))||"").trim();
        if(seg){
          const segUrl=(seg[0]==='/'||/^https?:/.test(seg))?seg:url.replace(/[^/]*$/,seg);
          const response=await fetch(segUrl,{headers,signal:ctl.signal});
          segState=response.status;
          if(segState>=400){body=await refusalBody(response);failureStatus=segState;}
          if(response.body) response.body.cancel().catch(()=>{});
        }
      }
      return {status,segState,body,failureStatus};
    })()]);
  }finally{clearTimeout(timer);ctl.abort();}
}
