"use strict";
// ---- a directed selection change ------------------------------------------
//
// The prepared handoff has been built on the server and on all three clients
// for a milestone and no viewer action has ever reached it, because the one
// path that could reach it reopened the stream synchronously instead: `play()`
// nulls `PLAYER.mediaAttachment`, and both the reporter's `capture` and its
// `onExchange` refuse to speak for any other attachment, so the exchange that
// carried the ask never left and a `Prepare` could not have been handled if it
// had. The functions below are that path -- publish the new selection, keep
// the incumbent playing, wait for the successor the server builds -- with
// exactly ONE reopen as the answer to every way it can go wrong.
const PREPARED_OFFER_BOUND_MS=12000;
const PREPARED_OFFER_CADENCE_MS=1000;
// `askPlaybackControl` cannot serve this and cannot be made to. Its waiter
// settles on the FIRST exchange at or after its floor, and a `Prepare` arrives
// on a later exchange than the one that carried the ask -- the server has to
// read the new selection, decide, and build a candidate first. This waiter
// lives in the same list under the same floor discipline, stays armed across
// exchanges, and is settled by rule rather than by arrival.
//
// It deliberately does NOT touch `p.mediaAttachment`. That guard is exactly
// what dropped every `Prepare` this client was ever offered; the reporter must
// keep owning the media for the whole wait, and the incumbent keeps playing
// and keeps being heard while it runs.
async function awaitPreparedOffer(p,tappedAt){
  if(!p) return "timed_out";
  const reporter=p.controlReporter;
  if(!reporter||reporter.stopped) return "timed_out";
  // Read BEFORE the notify, exactly as `askPlaybackControl` does: the request
  // carrying this selection is the next NEW one the reporter starts, and a
  // replayed retry keeps its old sequence. The identity is pinned with it,
  // because `resetForOwner` zeroes the counter on a 409.
  const minSequence=(Number(reporter.sequence)||0)+1;
  const generation=reporter.bootstrap&&reporter.bootstrap.generation;
  const controlEpoch=reporter.bootstrap&&reporter.bootstrap.control_epoch;
  const intentGeneration=p.controlIntentGeneration||0;
  const trigger=notifyPlaybackControl();
  // A notify that enqueued nothing will never produce a request carrying this
  // selection, so waiting the bound out for it spends twelve seconds on an
  // exchange that is not going to happen.
  if(!trigger) return "timed_out";
  if(!p.controlWaiters) p.controlWaiters=[];
  const waiters=p.controlWaiters;
  return await new Promise((resolve)=>{
    const waiter={kind:"offer",minSequence,generation,controlEpoch,
      intentGeneration,tappedAt,player:p,preparedActionId:null,settled:false,
      timer:null,cadence:null,confirm:null,
      settle:(outcome)=>{
        if(waiter.settled) return;
        waiter.settled=true;
        clearTimeout(waiter.timer); waiter.timer=null;
        clearTimeout(waiter.cadence); waiter.cadence=null;
        clearTimeout(waiter.confirm); waiter.confirm=null;
        const at=waiters.indexOf(waiter);
        if(at!==-1) waiters.splice(at,1);
        // `clearPlaybackControlWaiters` settles everything with null, and it
        // is called for an owner change, a reporter that stopped, and every
        // viewer command. None of those is this ask's answer, and all of them
        // mean something newer owns the player -- so none of them earns a
        // reopen on this ask's behalf.
        resolve(outcome||"superseded");
      },
      // An ordinary waiter gets a fresh window on every exchange that is not
      // its own. This one is bounded from the tap and ignores the extension.
      extend:()=>{},
      // The preparation is built after `settlePlaybackControlWaiters` returns,
      // further down the same `onExchange`. Confirming on the next turn is
      // what lets the bound below tell "offered and built" from "offered and
      // dropped" instead of reporting a handoff that does not exist.
      confirmPrepared:()=>{
        if(waiter.settled) return;
        waiter.confirm=null;
        if(preparedOfferSuperseded(waiter)){ waiter.settle("superseded"); return; }
        if(preparedOfferBuilt(waiter.player,waiter.preparedActionId))
          waiter.settle("prepared");
      }};
    waiter.timer=setTimeout(()=>{
      waiter.timer=null;
      // A staging this client was offered and never built still holds the
      // session's one preparation slot until the server's deadline, and the
      // reopen's successor would inherit a session that can never prepare
      // again. Hand it back before giving up on it.
      if(waiter.preparedActionId&&!preparedOfferBuilt(p,waiter.preparedActionId))
        queuePlaybackControlAcknowledgement(p,waiter.preparedActionId,"aborted");
      waiter.settle("timed_out");
    },Math.max(0,PREPARED_OFFER_BOUND_MS-(performance.now()-tappedAt)));
    waiters.push(waiter);
    // A reporter that stopped between the notify and here will never exchange
    // again, so nothing but the bound would ever settle this.
    if(reporter.stopped) waiter.settle("superseded");
  });
}
// A seek, another menu choice, a close: anything that replaces viewer intent
// retires this ask without earning it a reopen of its own.
function preparedOfferSuperseded(waiter){
  const p=waiter&&waiter.player;
  return !p||(p.controlIntentGeneration||0)!==waiter.intentGeneration;
}
// "This client built what it was offered." True while the preparation is live,
// while its commit is settling, and after a commit that has already settled --
// a staging whose media is already buffered passes through all three before
// the confirmation above runs.
function preparedOfferBuilt(p,actionId){
  if(!p||!actionId) return false;
  const live=preparedState(p);
  if(live&&live.actionId===actionId) return true;
  if(p.preparedCommitting&&p.preparedCommitting.actionId===actionId) return true;
  return preparedSettlementDone(p,actionId);
}
// The rules, in one place, run for every offer waiter on every exchange.
function settlePreparedOfferWaiter(p,waiter,mine,response,error){
  if(preparedOfferSuperseded(waiter)){ waiter.settle("superseded"); return; }
  // An exchange FAILURE does not settle this one, and that is the difference
  // from the stall waiter beside it. A stalled viewer cannot wait, so "no
  // answer" is an answer they can act on; here the incumbent is playing
  // perfectly and only the ask is unanswered, so the bound decides.
  if(error||!response||!mine) return;
  const action=response.action||null;
  if(action&&window.PlurxPlaybackControl
     &&action.type===PlurxPlaybackControl.PREPARE_ACTION_TAG){
    waiter.preparedActionId=action.action_id||null;
    // The server replays a `Prepare` byte-identically until it processes the
    // acknowledgement, so a replay of one this client already built settles at
    // once. A first offer waits for `onExchange` to have its turn -- this must
    // not build a second pipeline on the element the viewer is watching.
    if(preparedOfferBuilt(p,waiter.preparedActionId)){ waiter.settle("prepared"); return; }
    if(waiter.confirm==null) waiter.confirm=setTimeout(waiter.confirmPrepared,0);
    return;
  }
  const preparation=response.delivery&&response.delivery.preparation;
  // ABSENT IS NOT A DECLINE. A relay peer running an older build does not
  // evaluate this field at all, and reading its silence as "no" would turn
  // every switch against one into the immediate reopen this milestone exists
  // to remove. Only the explicit value declines; absent stays armed and lets
  // the bound decide.
  if(preparation==="none"){ waiter.settle("declined"); return; }
  if(preparation==="staging"&&waiter.cadence==null){
    // Staging is progress, not an answer. Come back sooner than ordinary
    // cadence so the offer is collected as soon as it exists rather than up to
    // a whole exchange interval later.
    waiter.cadence=setTimeout(()=>{
      waiter.cadence=null;
      const live=waiter.player&&waiter.player.controlReporter;
      if(!waiter.settled&&live&&!live.stopped) try{ live.notify(); }catch(e){}
    },PREPARED_OFFER_CADENCE_MS);
  }
}
// One owner per directed change, from the tap to a commit or ONE reopen.
//
// Every way this can fail converges on the same single answer -- reopen the
// stream where the viewer actually is -- and `change.settled` is what keeps it
// single. Without it a decline racing the bound, or a failed commit racing a
// supersede, reopens twice.
//
// `fallback` is optional and is how Auto keeps its own reopen: the automatic
// controller goes through `requestPlaybackMediaChange` so the create carries
// the rung, where the menu goes through `play()`.
async function requestQualityChange(p,reason,fallback,autoMove){
  if(!p) return "superseded";
  const change={intentGeneration:p.controlIntentGeneration||0,
    tappedAt:performance.now(),settled:false,reason:reason||"manual",
    fallback:fallback||null,autoMove:autoMove||null,commitTimer:null,
    outcome:null,outcomeAt:null};
  p.directedChange=change;
  let outcome="timed_out";
  try{ outcome=await awaitPreparedOffer(p,change.tappedAt); }catch(e){ outcome="timed_out"; }
  if(p.directedChange===change){ change.outcome=outcome; change.outcomeAt=performance.now(); }
  // A prepared successor is now this change's business: its commit settles it,
  // and its failure reopens through the same owner.
  if(outcome==="prepared") return outcome;
  if(outcome==="superseded"){ settleDirectedChange(p,change,"superseded"); return outcome; }
  fallBackDirectedChange(p,change,outcome);
  return outcome;
}
// The one reopen.
//
// Position is sampled NOW rather than at the tap. The incumbent kept playing
// for the whole wait -- up to the full bound -- and reopening at where the
// viewer was twelve seconds ago is a visible jump backwards on top of a
// quality change that already cost them a rebuffer.
function fallBackDirectedChange(p,change,why){
  if(!p||!change) return false;
  if(change.settled||p.directedChange!==change
     ||(p.controlIntentGeneration||0)!==change.intentGeneration) return false;
  change.settled=true;
  if(change.commitTimer!=null){ clearTimeout(change.commitTimer); change.commitTimer=null; }
  change.outcome=why;
  change.outcomeAt=performance.now();
  const v=document.getElementById("video");
  const pos=positionForPlaybackIntent(v,p);
  // The reopen's own create carries the rung explicitly, so the ask on the
  // wire has done its job and would otherwise keep re-announcing itself.
  p.autoRequestedHeight=null;
  if(change.autoMove&&p.abr){ p.abr.switching=false; releaseAutoFallback(p); }
  clientLog(Object.assign({level:"warn",event:"quality_switch",
    detail:`via=fallback why=${why}`,reason:change.reason||"manual",
    message:`prepared handoff did not carry the change (${why}); reopening the stream`},
    playbackContext()));
  if(change.fallback){
    try{ const running=change.fallback(why); if(running&&running.catch) running.catch(()=>{}); }
    catch(e){}
    return true;
  }
  PENDING_ATTEMPT_REASON="quality";
  // NOW the destination is published, and not a moment earlier. This is the
  // call `setQuality` used to make at the tap, where it marked a seek that was
  // not one and retired the very ask the change was about. Here the stream is
  // genuinely being reopened at a position, which is exactly what it is for,
  // and the change is already settled so nothing re-enters.
  beginPlaybackControlSeek(p,pos);
  play(p.fileId,p.title||"",Math.round(pos*1000),p.knownDur||0,p.meta);
  return true;
}
// A directed change that ended without a reopen: the successor took the
// picture, or something newer replaced the whole ask.
function settleDirectedChange(p,change,why,detail){
  const owned=change||(p&&p.directedChange);
  if(!p||!owned||owned.settled||p.directedChange!==owned) return false;
  owned.settled=true;
  if(owned.commitTimer!=null){ clearTimeout(owned.commitTimer); owned.commitTimer=null; }
  owned.outcome=why;
  owned.outcomeAt=performance.now();
  if(detail!=null) owned.detail=detail;
  // The successor is visible, but the server has not accepted its committed
  // acknowledgement yet. Keep the exact selection that staged it until that
  // exchange succeeds: changing to plain Auto here makes the server reject
  // the commit as a different ask and retire the stream we just exposed.
  if(why!=="committed"||!owned.autoMove) p.autoRequestedHeight=null;
  if(owned.autoMove&&p.abr){
    if(why==="committed"){
      const now=performance.now(), move=owned.autoMove;
      p.autoHeight=move.to;
      Object.assign(p.abr,{lastSwitchAtMs:now,stableSinceMs:now,
        mildSamples:0,upgradeSinceMs:null,previousRunway:null});
      recordAutoSwitch(p,move.from,move.to,move.switchReason,
        positionForPlaybackIntent(document.getElementById("video"),p),p.sessionId);
    }
    p.abr.switching=false;
    releaseAutoFallback(p);
  }
  return true;
}
// A viewer command, a teardown, a stream replacement. The ask is retired and
// nothing owes it a reopen.
function supersedeDirectedChange(p){
  return settleDirectedChange(p,p&&p.directedChange,"superseded");
}
// The server cancels a preparation the moment the incumbent reports `waiting`
// or `stalled`, so an automatic move made from a stalled picture would spend
// the whole offer bound to be told no. A stalled reopen stays a reopen.
function directedChangeIncumbentReady(p,v){
  if(!p||!v||!p.started) return false;
  if(v.error||v.ended||p.waitAt) return false;
  if(!playbackOwnsAttachedMedia(p)) return false;
  return v.paused||Number(v.readyState)>=3;
}
// Settle every waiter whose request has now been answered. An exchange that
// failed settles them too, with null: "no answer" is an answer a stalled
// viewer can act on immediately, and making them wait out the full bound for
// it would add six seconds to a stall for nothing.
function settlePlaybackControlWaiters(p,request,response,error){
  const waiters=p&&p.controlWaiters;
  if(!waiters||!waiters.length) return;
  // The new owner has its own sequence space and did not answer this
  // observation. Let the reporter adopt it, but release every current recovery
  // owner immediately rather than extending a frozen wait to the hard cap.
  if(error&&error.code==="owner_changed"){
    clearPlaybackControlWaiters(p);
    return;
  }
  const sequence=Number(request&&request.sequence);
  if(!Number.isSafeInteger(sequence)) return;
  const action=(response&&response.action)||null;
  for(const waiter of waiters.slice()){
    const mine=sequence>=waiter.minSequence
      && request.generation===waiter.generation
      && request.control_epoch===waiter.controlEpoch;
    // An offer waiter is settled by rule, never by arrival: the exchange that
    // answers a directed change is rarely the one that carried it.
    if(waiter.kind==="offer"){ settlePreparedOfferWaiter(p,waiter,mine,response,error); continue; }
    if(mine) waiter.settle(action); else waiter.extend();
  }
  // A stop that follows its own last exchange leaves a reporter that will never
  // exchange again; anything still waiting on it is waiting on nothing.
  if(p.controlReporter&&p.controlReporter.stopped) clearPlaybackControlWaiters(p);
}
// Abandon every outstanding ask. Called when the reporter goes away, so a
// stalled owner falls through to its own behaviour at once rather than waiting
// out a bound for an exchange that can no longer happen.
function clearPlaybackControlWaiters(p){
  const waiters=p&&p.controlWaiters;
  if(!waiters) return;
  for(const waiter of waiters.slice()) waiter.settle(null);
}
// Readiness is an extensible relay value. Only the exact value this client
// understands authorizes a retry; absent and future values are both silence,
// never an inferred ready state.
function subtitleReadinessMeansReady(delivery){
  return !!delivery&&delivery.subtitle_readiness==="ready";
}
// One transition, one retry. The state belongs to the reporter generation and
// is reset by `startPlaybackControl`; repeated control cadence at `ready`
// cannot turn into a subtitle request storm.
function subtitleReadinessRetryTransition(state,delivery){
  const ready=subtitleReadinessMeansReady(delivery);
  const retry=state.subtitleReadinessReady===false&&ready;
  state.subtitleReadinessReady=ready;
  return retry;
}
function nativeHlsSubtitleOrdinal(player,index){
  const native=(player&&player.subs||[]).filter(s=>s.native===true);
  return native.findIndex(s=>s.index===index);
}
// Re-selecting a subtitle rendition asks the HLS engine for the current
// segment again. Video stays attached throughout; this is subtitle I/O only.
// Put the cues on screen once the server says the track is ready, without
// touching the video.
//
// The obvious move — re-select the rendition so hls.js fetches the segment
// again — does not work, and that is measured rather than assumed. Against
// the bundled hls.js 1.6.16, `subtitleTrack = -1` then back, the same wrapped
// in `subtitleDisplay` off/on, a bounce through a sibling rendition, a purge
// of the fragment tracker, and a reselect with a nudge seek all leave the
// cue count at zero and produce exactly ONE request for the segment. hls.js
// does not re-request a subtitle fragment it has already processed, and the
// minified build exposes no stream controller to reach past it. So the empty
// `WEBVTT` body the server published while the sidecar was warming is what
// that rendition shows for the rest of the session.
//
// What does work — 0 cues to 1 in the same harness, with the session id, the
// video element and the segment count all unchanged — is to stop asking hls.js
// and read the whole-track sidecar instead. By the time readiness says
// `ready` that sidecar is exactly what exists, and `/files/{id}/subs/{i}.vtt`
// is a route this player already uses for offset sessions. The rendition is
// switched off so its empty track cannot sit on top of the cues.
function retryReadyNativeSubtitle(player){
  if(!player||player.burnedSub!=null||player.curSub==null||player.curSub<0) return false;
  const ordinal=nativeHlsSubtitleOrdinal(player,player.curSub);
  if(ordinal<0) return false;
  if(player.hls){
    try{ player.hls.subtitleTrack=-1; }catch(err){}
    // Re-entering `setSub` would be the wrong move: it would take the
    // rendition path again and land on the same cached empty fragment. Force
    // the sidecar branch by clearing the marker `setSub` reads, then let it
    // rebuild the script cue list at the session's own offset.
    player._subOff=null;
    applyReadySubtitleSidecar(player,player.curSub);
    return true;
  }
  if(!player.sessionId) return false;
  const video=document.getElementById("video");
  const track=video&&video.textTracks&&video.textTracks[ordinal];
  if(!track) return false;
  track.mode="disabled";
  track.mode="showing";
  return true;
}
// Fetch the whole-track sidecar and hand its cues to the one script text
// track this player reuses. Shifted by the session's media origin for the
// same reason `setSub` shifts it: a transcode's timeline starts at its own
// offset, and sidecar cue times are absolute source time.
async function applyReadySubtitleSidecar(player,index){
  const video=document.getElementById("video");
  if(!video) return;
  const off=player.offset||0;
  let text;
  try{
    const r=await fetch(subUrl(index));
    if(!r.ok) throw new Error("HTTP "+r.status);
    text=await r.text();
  }catch(err){ return; }              // still warming, or gone: leave it alone
  if(PLAYER!==player||player.curSub!==index||(player.offset||0)!==off) return;
  if(!video._vsubs) video._vsubs=video.addTextTrack("subtitles","Subtitles");
  const track=video._vsubs;
  if(track.mode==="disabled") track.mode="hidden";
  try{ while(track.cues&&track.cues.length) track.removeCue(track.cues[0]); }catch(err){}
  const Cue=window.VTTCue||window.TextTrackCue;
  let added=0;
  for(const c of vttParse(text)){
    const en=c.end-off;
    if(en<=0) continue;
    try{ track.addCue(new Cue(Math.max(0,c.start-off),en,c.text)); added++; }catch(err){}
  }
  if(!added) return;                  // nothing to show; do not claim otherwise
  track.mode="showing";
  player._subOff=off;
}
function startPlaybackControl(v,p,bootstrap){
  stopPlaybackControl(p);
  p.controlLastRequest=null;
  p.controlLastResponse=null;
  p.controlLastExchangeAt=null;
  p.controlLastError=null;
  p.controlLastFailureLogAt=null;
  p.subtitleReadinessReady=undefined;
  if(!bootstrap || !window.PlurxPlaybackControl) return null;
  try{
    const attachment=p.mediaAttachment;
    const owner=Object.freeze({lifecycleId:CONTROL_CLIENT_ID,
      attachmentGeneration:startPlaybackControl.captureGeneration=(startPlaybackControl.captureGeneration||0)+1});
    // A prepared commit swaps the DOM video while retaining this reporter to
    // deliver the acknowledgement on the predecessor's control session.
    // Sample the visible successor after that swap, never the retired node.
    const capture=()=>p.mediaAttachment===attachment&&playbackOwnsAttachedMedia(p)
      ?PlurxPlaybackControl.capture(playbackControlSnapshot(document.getElementById("video"),p),
        p.controlIntentGeneration||0,owner):null;
    const reporter=new PlurxPlaybackControl.Reporter({bootstrap,
      clientInstanceId:CONTROL_CLIENT_ID,
      capture,
      send:sendPlaybackControl,
      onExchange:({request,response,error,capture:captured})=>{
        if(continueStoppingPlaybackControl(p,reporter,v,request,response,captured)) return;
        if(!playbackOwnsAttachedMedia(p)||p.controlReporter!==reporter
          ||p.mediaAttachment!==attachment||captured.owner.lifecycleId!==owner.lifecycleId
          ||captured.owner.attachmentGeneration!==owner.attachmentGeneration) return;
        p.controlLastRequest=request;
        settlePlaybackControlWaiters(p,request,response,error);
        // An acknowledgement the server accepted is spent; one whose exchange
        // failed stays queued for the next.
        const committedBeforeEnd=!!(response&&request.demand==="active"
          &&request.render_state==="ended"
          &&request.acknowledgement&&request.acknowledgement.state==="committed");
        if(response) settlePlaybackControlAcknowledgement(p,request);
        // The active demand above was synthetic solely because commit+end is
        // invalid. Wake the real end snapshot immediately after the server
        // accepts that commit; ordinary cadence may be a full minute.
        if(committedBeforeEnd) reporter.notify();
        // The wire tag is `prepare`; the DECLARED name is
        // `prepare_replacement`, and switching on the declared name here is
        // the mistake that produces a silent no-op rather than a failure.
        if(response&&response.action
          &&response.action.type===PlurxPlaybackControl.PREPARE_ACTION_TAG
          &&captured.intentGeneration===(p.controlIntentGeneration||0)){
          handlePreparedReplacementAction(p,response.action);
        }
        if(captured.intentGeneration===(p.controlIntentGeneration||0)&&response&&response.delivery
          &&subtitleReadinessRetryTransition(p,response.delivery)){
          retryReadyNativeSubtitle(p);
        }
        // A terminal verdict can arrive on any exchange, and the reporter
        // stops on it — correctly, since it owns no recovery. But a stall an
        // later in the same lease would then find no reporter to ask and fall
        // through to the client's own guess, discarding the one answer the
        // server was sure of. Ruling D1 says the verdict is armed rather than
        // executed; this is where it is armed. It outlives the reporter and a
        // same-title session replacement, but not the lease.
        if(response&&response.action&&response.action.type==="terminal"
          &&captured.intentGeneration===(p.controlIntentGeneration||0)){
          p.controlVerdict=response.action;
          p.controlVerdictExpiresAt=performance.now()+bootstrap.lease_timeout_ms;
        }
        if(response){
          const recovered=p.controlLastError;
          // Advisory only, and read by nothing that decides anything: the
          // Developer card shows the server's own last word on preparing so an
          // operator can tell "declined" from "never asked".
          p.controlLastPreparation=(response.delivery&&response.delivery.preparation)||null;
          p.controlLastResponse=response;
          p.controlLastExchangeAt=Date.now();
          p.controlLastError=null;
          if(recovered) clientLog({level:"info",event:"playback_control",detail:"recovered",
            message:"playback-control exchange recovered; legacy recovery remained authoritative"});
        }
        if(error){
          const now=Date.now(), message=String(error.message||error);
          p.controlLastError={at:now,message};
          if(!p.controlLastFailureLogAt||now-p.controlLastFailureLogAt>=60000){
            p.controlLastFailureLogAt=now;
            // §3.3 row 18: the reporter owns no recovery, so a refused exchange
            // — `session_gone` on a committed successor's first one included —
            // changes nothing the viewer can see. The event is the only trace.
            raisePlaybackSurface("log_only",{attached:playbackSurfaceGeneration(p)});
            clientLog({level:"warn",event:"playback_control",
              detail:String(error.code||error.name||"exchange_failed"),message});
          }
        }
      }});
    p.controlReporter=reporter;
    // Kept so `flushPreparedSettlement` can build a capture this reporter will
    // accept after the player has stopped owning the media. Nothing else reads
    // it, and it is replaced with the reporter it belongs to.
    p.controlOwner=owner;
    reporter.start();
    return reporter;
  }catch(error){
    p.controlLastError={at:Date.now(),message:String(error&&error.message||error)};
    return null;
  }
}
// `requestId` is the SAME identity across a retry sequence (M5 §4.6.1). The
// server persists a create's answer under `request_id`, so replaying one
// recovers the session it already made instead of spawning a second encoder —
// which is what makes retrying a "still building" 503 safe at all. Absent, a
// fresh identity is minted, exactly as every caller had before.
async function openSession(fileId, opts, signal=null, requestId=null){
  const contract=vodClientContract();
  const body=Object.assign({},opts||{},
    {playback_id:PLAYBACK_ID,request_id:requestId||newRequestId()},contract.session);
  // The capabilities the plan was derived from, on the request that acts on
  // it. Without this the server has only the body's echo of its own decision
  // to go on, and an echo cannot carry `convert_dolby_vision` — no client has
  // ever been able to ask for a conversion, so the field is not on the wire
  // and the session keeps the `false` it was built with unless the server
  // derives it blind. Every create goes through here, so the remux open, the
  // transcode fallback, seeks and audio switches all send it from one place.
  // It is also the last build on the fleet holding
  // `plan_derivation.legacy_trusted` off zero.
  const caps=(typeof PLAYER!=="undefined"&&PLAYER&&PLAYER.capsSnapshot)
    ||currentCapsDocument();
  if(capsDocumentIsUsable(caps)){
    body.caps=caps;
    // The other half of the same question. `/decision` is asked with the
    // quality menu's force on the query string; a create that hands the server
    // the document but not the force is re-derived under `Force::Auto`, which
    // is a DIFFERENT question — and for the two cases where Auto and Original
    // disagree it answers worse. On `Quality → Original` for a 4K title above
    // this browser's HEVC ceiling, Auto derives a transcode and therefore
    // `preserve_dolby_vision = false`, so the viewer who asked for the
    // untouched original gets the Dolby Vision stripped and the server logs a
    // `plan_mismatch` against its own player. The same happens wherever a
    // learned decode limit applies, which Original exists to bypass.
    //
    // Sent only alongside the caps because `overrides` is read only by
    // `review_client_plan`, which only runs when the document is usable.
    const force=(typeof qualityForce==="function")?qualityForce():"auto";
    if(force && force!=="auto") body.overrides=Object.assign({force},body.overrides||{});
  }
  // HLS sessions carry native WebVTT renditions whenever this server says a
  // track can become one. Publishing the group up front lets a later subtitle
  // selection and a readiness-directed re-fetch stay inside the current video
  // item rather than reopening it.
  const player=typeof PLAYER!=="undefined"?PLAYER:null;
  const natives=(player&&player.subs||[]).filter(s=>s.native===true);
  if(natives.length){
    body.native_subtitles=true;
    if(natives.some(s=>s.index===player.curSub)) body.subtitle=player.curSub;
  }
  // Where this start sits in the control ordering, so a server that has since
  // accepted a later seek can refuse to produce a destination the viewer has
  // already scrolled past. The client's own `_seekToken` discards the response
  // either way; this saves the server the work rather than the client the
  // confusion. Omitted when no exchange has been accepted yet — a first play
  // has no earlier ask to be stale against.
  const acceptedSequence=player&&Math.max(
    Number(player.controlReporter&&player.controlReporter.sequence)||0,
    Number(player.controlSequenceFloor)||0);
  if(Number.isFinite(acceptedSequence)&&acceptedSequence>0) body.control_sequence=acceptedSequence;
  // How this client will play and tear down the stream. The web player
  // destroys its hls.js instance before it sends the release, so the server
  // may drop this session's retired objects a segment after the DELETE
  // instead of a whole advertised playlist later. Native HLS has no bounded
  // retry tail, so it says so and keeps the original promise.
  const hevcCopy=!!(opts&&opts.copy)&&["hevc","h265","hevc10"].includes(
    String((typeof PLAYER!=="undefined"&&PLAYER&&PLAYER.source&&PLAYER.source.video_codec)||"")
      .toLowerCase());
  const transport=typeof plannedHlsTransport==="function"?plannedHlsTransport(hevcCopy):null;
  if(transport) body.transport=transport;
  // A null height is the *absence* of a request, not a request for nothing:
  // sending the key would have the server clamp null to its floor.
  if(body.height==null) delete body.height;
  const following=PLAYER&&PLAYER.libraryChannel;
  if(following){
    const result=await api(`/library-channels/${encodeURIComponent(following.channel_id)}/sessions`,{
      method:"POST",signal,body:{generation_id:following.generation_id,
        occurrence:following.occurrence,tune_sequence:following.tune_sequence,playback:body}});
    if(!result||!result.playback||!result.library_channel
      ||Number(result.library_channel.tune_sequence)!==Number(following.tune_sequence))
      throw Object.assign(new Error("The channel tune response was stale."),{code:"channel_occurrence_changed"});
    PLAYER.libraryChannel=Object.assign({},following,result.library_channel);
    return result.playback;
  }
  return api(`/files/${fileId}/hls/sessions`,{method:"POST",body,signal});
}
// A cancellable wait. The newer intent's abort is the same signal the create
// itself is carrying, so a retry sleeping between attempts is cancelled by the
// thing that cancels the attempts (M5: "any newer intent cancels the whole
// sequence").
function playbackRetryDelay(ms, signal){
  return new Promise((resolve,reject)=>{
    const superseded=()=>Object.assign(new Error("Playback preparation superseded."),
      {name:"AbortError"});
    if(signal&&signal.aborted) return reject(superseded());
    let timer=null;
    const cancel=()=>{ if(timer!=null){clearTimeout(timer);timer=null;} reject(superseded()); };
    timer=setTimeout(()=>{
      timer=null;
      if(signal) signal.removeEventListener("abort",cancel);
      resolve();
    },ms);
    if(signal) signal.addEventListener("abort",cancel,{once:true});
  });
}
// The context a failed create will be classified in, computed the way
// `failPreparation` computes it — from the PREDECESSOR, because that is the
// player still standing when the failure is surfaced. A create over an
// attached predecessor is a refused CHANGE (§3.3 row 7) and keeps today's
// banner; row 6's retry is the START context and nothing else.
function playbackCreateRetryContext(){
  const p=PLAYER, predecessor=(p&&p.mediaPredecessor)||p;
  return predecessor&&predecessor.started?"change":"start";
}
// M5 addition 1 — the create "not yet" retry (contract §3.3 row 6).
//
// The same create, under the same request identity, on the review's ladder
// (1 s · 2 s · 4 s), bounded by an ABSOLUTE 60 s deadline from the first
// attempt. Three properties the review asked for, and each is a line here:
//
//   * the deadline is absolute, so a server that holds every create for a
//     minute cannot stretch the sequence — the watchdog fires on the clock,
//     not between attempts;
//   * a success that lands AFTER the deadline is released, never attached: the
//     viewer has already been told this attempt is over, and an encoder nobody
//     is watching is a hardware slot held for nobody;
//   * a newer intent aborts the signal, which cancels the attempt in flight
//     and the sleep between attempts alike.
//
// When the ladder or the deadline is spent the OWNER stops the player and
// raises `exhausted` (§3.0, §3.4) — this function is that owner, which is why
// the stop is here and not in the presenter.
//
// `answeredLocally(failure)` lets a call site that has a better answer than
// waiting keep it: `startCopyHls` turns `vod_index_pending` into a progressive
// remux immediately, and making it wait seven seconds for a stream it can
// already play would be a regression dressed as a recovery.
async function openSessionRetryingNotYet(fileId, opts, signal, options){
  const settings=options||{};
  const where=settings.context||playbackCreateRetryContext();
  const answeredLocally=settings.answeredLocally||null;
  // The preparation owner this sequence runs inside, and the only absolute
  // bound it has (ruling 2). Every shipped create already runs inside one —
  // `startCopyHls` refuses to open without it — and M5's own 60 s watchdog
  // could never fire behind a 20 s one, so it is gone rather than left as a
  // branch that reads like a bound and is not. Refuse an ownerless sequence
  // rather than leave the next caller an unbounded one.
  const preparation=settings.preparation||null;
  if(!preparation) throw Object.assign(
    new Error("A create-retry sequence needs a preparation owner to bound and cancel it."),
    {name:"TypeError"});
  const requestId=newRequestId();
  const began=Date.now();
  const sequence={raised:false,refusal:null};
  const superseded=()=>Object.assign(new Error("Playback preparation superseded."),
    {name:"AbortError"});
  // Two things about the raise below, both of them decisions rather than
  // accidents. The DETAIL is the server's own sentence, the last thing it said
  // about this stream: both ways the sequence ends have one — the ladder is
  // spent only after a refusal, and the preparation deadline ends the SEQUENCE
  // only when one has already arrived — so the viewer reads why the stream is
  // not here rather than "Playback could not prepare." And the class's
  // `keep_waiting` is deliberately ABSENT from the ACTIONS; putting it back
  // would be a regression rather than a fix. §3.1's web Keep waiting is
  // `armStall` re-armed, and on this path nothing is attached: the watchdog it
  // arms calls `stallDiagnose`, which returns on its first line
  // (`!playbackOwnsAttachedMedia(p)`). The button would clear the prompt and do
  // nothing, leaving a stopped black player whose only way out is the chrome's
  // Close. Try again re-runs the whole open, which IS the honest "one more
  // bounded attempt" here. Ruled 2026-09-13, alongside the ruling that put the
  // action on the two STALL sites, where there is a detector to re-arm.
  const exhaust=reason=>{
    if(!sequence.raised){
      sequence.raised=true;
      stopPlayerForExhaustion();
      raisePlaybackSurface("owner_exhausted",{context:where,player_stopped:true,
        title:"Playback is stalled.",
        detail:(sequence.refusal&&sequence.refusal.message)
          ||"the server did not finish starting this stream",
        actions:["retry","close"]});
    }
    return Object.assign(new Error("The server did not finish starting this stream."),
      {name:"PlaybackCreateExhausted",surfaceRaised:true,createRetryReason:reason});
  };
  // What the preparation deadline MEANS to a sequence the server has been
  // answering "not yet" to: this owner's ladder ending, on the owner's clock.
  // Nothing to answer with before the first refusal — a create that is merely
  // slow is a preparation that timed out, and says so.
  preparation.expiry=()=>(sequence.refusal?exhaust("preparation_deadline"):null);
  try{
    for(let attempt=0;;attempt+=1){
      if(signal&&signal.aborted) throw superseded();
      let info;
      try{
        info=await openSession(fileId,opts,signal,requestId);
      }catch(error){
        if(signal&&signal.aborted) throw superseded();
        const failure=error&&error.streamFailure;
        if(answeredLocally&&failure&&answeredLocally(failure)) throw error;
        const source=failure?PlaybackPolicy.classifyStreamFailure({
          status:failure.status,code:failure.code,context:where}):null;
        const step=PlaybackPolicy.createRetryStep({
          attempt,elapsedMs:Date.now()-began,source});
        if(step.action==="fail") throw error;
        // The refusal this sequence is about, kept before either ending can
        // use it: the ladder's own exhaustion and the preparation deadline's
        // read the same sentence.
        sequence.refusal=failure;
        if(step.action==="exhausted") throw exhaust(step.reason);
        // Still building. One fault for the sequence, not one per attempt: the
        // ring is for distinct faults, and a viewer watching a spinner does not
        // need four identical rows behind it.
        const explained=PlaybackPolicy.streamFailureOverlay(failure);
        raisePlaybackSurface("create_503_not_yet",{context:where,
          title:explained?explained.title:"Still preparing this stream…",
          detail:explained?explained.detail:failure.message},{once:true});
        clientLog(Object.assign({level:"info",event:"create_retry",
          detail:failure.code||String(failure.status),
          message:`the server is still building this stream — retrying in ${step.delayMs}ms`},
          playbackContext()));
        await playbackRetryDelay(step.delayMs,signal);
        continue;
      }
      // A session the sequence no longer owns belongs to nobody. Reachable now
      // that the preparation deadline is the one that ends this: it aborts the
      // signal, and the create already in flight still lands.
      if(signal&&signal.aborted){
        releaseSession(info&&info.session_id);
        throw superseded();
      }
      return info;
    }
  }finally{
    // A hook left behind would answer the NEXT operation's deadline with this
    // sequence's exhaustion.
    preparation.expiry=null;
  }
}
// Take on a newly-opened session's identity, and attach it.
//
// One function for the same reason `transcodeOpts` is one function: SIX paths
// open a session — cold start, seek, audio switch, subtitle switch, the
// copy-HLS path, the transcode fallback — and a response field wired into five
// of them is a bug that only appears on the sixth. That is exactly how seeking
// came to drop the burned subtitle, and `vod` would have gone the same way.
//
// `wantSec` is where the viewer expects to be in film time. Every HLS response
// is now the whole immutable title, so the media element seeks there directly.
// The returned absolute position is also what the stall watchdog is armed with.
function attachSession(v, t, info, wantSec){
  stopPlaybackControl(t);
  t.controlRenderOverride=null;
  t.controlObservationOverride=null;
  t.offset=info.start_seconds||0;
  t.encoder=info.encoder||null;
  t.sessionId=info.session_id||null;
  t.streamId=null;
  t.vod=!!info.vod;
  if(Array.isArray(info.ladder)&&info.ladder.length) t.ladder=info.ladder;
  if(info.prior_kbps>0) t.priorKbps=info.prior_kbps;
  // The one site every session passes through — transcode start, startCopyHls,
  // startTranscodeFallback, the subtitle burn, rung and audio re-opens — so the
  // badge follows the session and not the decision. Absent (an old server, or a
  // create whose source lookup came back None) keeps whatever we had: the
  // decision's value is still the best answer. MEDIA-BADGES-PLAN.md §3.2.
  if(info.delivered_dynamic_range) t.deliveredRange=info.delivered_dynamic_range;
  // Absent means "this session carries no Dolby Vision", which is an answer,
  // so it is written unconditionally where the range beside it is not: a
  // session that stripped must clear a profile an earlier decision reported.
  if(info.delivered_dynamic_range) t.deliveredDvProfile=info.delivered_dolby_vision_profile||null;
  // …and repaint, or the chip keeps its pre-session HTML forever while the
  // one-second stats panel follows the new truth. That is how "DV P7 → HDR10"
  // sat beside "Dynamic range: SDR" on a tone-mapped reference episode I: both
  // surfaces call dynamicRangeBadge() with the same arguments, but only the
  // panel repaints. renderPlayerInfo() is otherwise painted once, at session
  // open — before the route is chosen and before the forced-burn override.
  // MEDIA-BADGES-PLAN.md requires the chip and the panel row to agree.
  if(t===PLAYER) renderPlayerInfo();
  t.probeUrl=info.playlist_url;
  const into = t.vod ? Math.max(0, wantSec||0) : 0;
  t.controlPositionHintSec=(t.offset||0)+into;
  attachHls(v, info.playlist_url, into);
  startPlaybackControl(v,t,info.control||null);
  markPlaybackControlSeekExecuted(t,wantSec||0);
  return into + (t.offset||0);
}
// Tell the server we are finished with a stream. Without this the encoder
// lives on for the idle timeout plus a reaper tick — over a minute of a
// hardware slot held for nobody — every time a player closes or replaces its
// stream. `keepalive` is what makes it survive the page going away, and is
// also why the route authenticates by session id rather than by header.
function releaseSession(sessionId){
  if(!sessionId) return;
  try{
    fetch(API+`/hls/${sessionId}`,{method:"DELETE",keepalive:true,
      headers:TOKEN?{"authorization":"Bearer "+TOKEN}:{}}).catch(()=>{});
  }catch(e){}
}
