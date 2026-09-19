"use strict";
// ---- autoplay next episode (per browser, default on) ----------------------
function autoNextOn(){ try{ return localStorage.getItem("plurx_autonext")!=="0"; }catch(e){ return true; } }
function setAutoNext(on){ try{ localStorage.setItem("plurx_autonext", on?"1":"0"); }catch(e){}
  const b=document.getElementById("autonextbtn"); if(b) b.classList.toggle("on", on);
  const c=document.getElementById("autonext"); if(c) c.checked=on; }
function togglePlayerAutonext(){ setAutoNext(!autoNextOn()); toast(autoNextOn()?"Autoplay next: on":"Autoplay next: off"); }
// Find and play the episode after the one that just finished — next in the
// season, else the first episode of the next season. Reuses AUTOPLAY-on-navigate
// so the page behind the player follows along. Returns true if it started one.
async function playNextEpisode(){
  const current=playbackContinuation(PLAYER);
  const itemId=ITEM_FOR_FILE[PLAYER&&PLAYER.fileId]; if(!itemId) return false;
  const preparation=beginPlaybackPreparation(current);
  const read=path=>preparation.run(signal=>api(path,{signal}));
  try{
  const cur=await read(`/items/${itemId}`);
  if(!current()) return false;
  if(!cur.item || cur.item.kind!=='episode') return false;   // movies don't chain
  const anc=cur.ancestors||[], season=anc[anc.length-1], show=anc[anc.length-2];
  if(!season) return false;
  let next=null;
  const sd=await read(`/items/${exactWireId(season)}`);
  if(!current()) return false;
  if(sd){
    const eps=(sd.children||[]).filter(c=>c.kind==='episode');
    const i=eps.findIndex(e=>exactWireId(e)===itemId);
    if(i>=0 && eps[i+1]) next=eps[i+1];
  }
  if(!next && show){                                         // roll over to next season
    const shd=await read(`/items/${exactWireId(show)}`);
    if(!current()) return false;
    if(shd){
      const seasons=(shd.children||[]).filter(c=>c.kind==='season');
      const si=seasons.findIndex(s=>exactWireId(s)===exactWireId(season));
      if(si>=0 && seasons[si+1]){
        const nsd=await read(`/items/${exactWireId(seasons[si+1])}`);
        if(!current()) return false;
        if(nsd) next=(nsd.children||[]).filter(c=>c.kind==='episode')[0]||null;
      }
    }
  }
  if(!next) return false;                                    // end of the series
  AUTOPLAY=exactWireId(next);
  toast("▶ Up next: "+next.title);
  location.hash="#/item/"+exactWireId(next);
  return true;
  }catch(error){
    if(current())toast("Could not load the next episode. Choose it from the library to retry.");
    return false;
  }finally{preparation.finish();}
}
function playbackContinuation(p){
  const action=p?.controlIntentGeneration||0, generation=p?._seekToken||0;
  const open=p?.pendingOpenAttempt;
  return ()=>!!p&&PLAYER===p&&(p.controlIntentGeneration||0)===action
    &&(p._seekToken||0)===generation&&p.pendingOpenAttempt===open;
}
async function playNextAudiobookPart(){
  if(!PLAYER||!PLAYER.bookParts||PLAYER.bookParts.length<2) return false;
  const i=PLAYER.bookParts.findIndex(p=>p.id===PLAYER.fileId), next=PLAYER.bookParts[i+1];
  if(i<0||!next) return false;
  const m=Object.assign({},PLAYER.meta||{},{part_offset_ms:next.part_offset_ms||0});
  toast(`▶ Continuing ${next.title||`part ${i+2}`}`);
  await play(next.id,PLAYER.title,0,next.duration_ms||0,m);
  return true;
}
// Video reached its natural end (or auto-skipped credits hit the end): play the
// next episode if autoplay is on, otherwise fall back to prior behavior.
async function finishPlayback(closeIfNoNext){
  const current=playbackContinuation(PLAYER);
  if(PLAYER&&PLAYER.libraryChannel){ await libraryChannelBoundary(); return; }
  if(PLAYER&&PLAYER.bookParts){ closePlayer(); return; }
  if(autoNextOn()){
    // A destination the viewer asked for by letting autoplay run: it has an
    // intent, so the search settling is what retires it, not a frame.
    const intent=`autonext:${(PLAYER&&PLAYER.attemptId)||"?"}`;
    raisePlaybackSurface("client_preparing",{intent,
      title:"Up next…",detail:"finding the next episode"});
    if(await playNextEpisode()) return;                      // navigation + play take over
    if(!current()) return;
    playbackSurfaceStep({intent_settled:intent});
  }
  if(closeIfNoNext) closePlayer();
}
// How far short of the runtime still counts as "they watched it". A remux
// seek lands on the keyframe at or before the target, containers pad their
// last fragment, and a transcode's final segment is rounded to a segment
// boundary, so the honest end of playback sits a few seconds shy of the
// probed duration even on a clean finish.
const ENDED_SLACK_SEC=15;
// `ended` fires at the end of the media the browser HAS, which is not the end
// of the film. A progressive remux is a one-directional pipe that can stop
// early — the session gets reaped, ffmpeg dies, a paced connection drops — and
// an HLS playlist that stops growing reads as complete to hls.js because the
// server never writes #EXT-X-ENDLIST. Taking `ended` at its word posted the
// full runtime as the position, which cleared the server's 95% watched
// threshold from a few seconds in; the flag is sticky, so a single truncated
// stream marked a film watched permanently and told Trakt so.
async function handleEnded(fileId){
  const p=PLAYER, generation=(PLAYER&&PLAYER._seekToken)||0;
  if(!playbackOwnsAttachedMedia(p)) return;
  const current=playbackContinuation(p);
  const actionGeneration=(PLAYER&&PLAYER.controlIntentGeneration)||0;
  const total=pbTotalSec(), pos=pbPosSec();
  if(PLAYER&&PLAYER.bookParts&&PLAYER.bookParts.length){
    const i=PLAYER.bookParts.findIndex(p=>p.id===PLAYER.fileId);
    if(i>=0&&PLAYER.bookParts[i+1]){
      await reportProgress(fileId,false);
      if(!current()) return;
      if(await playNextAudiobookPart()) return;
      if(!current()) return;
    }
  }
  // total===0 means no runtime is known for this file (nothing probed, and a
  // growing stream's own duration is not a substitute). There is nothing
  // better to judge by, so `ended` stands.
  if(!(total>0) || pos>=total-ENDED_SLACK_SEC){
    await reportProgress(fileId,true);
    if(!current()) return;
    return finishPlayback(false);
  }
  // Truncated. Record where playback actually reached — never the runtime —
  // so resume lands here rather than at the end.
  await reportProgress(fileId,false);
  if(!current()) return;
  const prev=PLAYER.endedAt; PLAYER.endedAt=pos;
  // Dying at the same spot means retrying will die there again. Count only
  // repeats at the same position, so a stream that gets further each time is
  // making progress and keeps its budget.
  PLAYER.endedTries=(prev!=null&&Math.abs(pos-prev)<1)?(PLAYER.endedTries||0)+1:1;
  // A stream that stopped short is the clearest evidence this client has that
  // production ended, and the one the server can most often explain: it knows
  // whether the producer was told to stop, ran out of a resource, or gave a
  // verdict about the source. The budget below is a guess standing in for
  // exactly that answer.
  const ask=askPlaybackControl("failed",
    {decoder_state:"failed",error_code:"media",error_detail:"truncated_stream"});
  const controlTrigger=ask.trigger;
  const verdict=(await ask.action)||armedPlaybackControlVerdict(PLAYER);
  // The viewer had the whole ask window — up to CONTROL_ASK_CAP_MS — to seek,
  // pause or close, and `handleEnded` can be re-entered by the next `ended`
  // event on a stream that reopened while we waited. `endedAt` alone is a poor
  // token here by construction: a truncated stream repeats at the SAME
  // position, which is what `endedTries` counts, so two overlapping
  // invocations would both see it unchanged. Identity and the seek token are
  // the checks that hold.
  if(!endedStillOurs(p,generation,actionGeneration,fileId,pos)) return;
  if(verdict&&verdict.type==="terminal"){
    // Ruling D1: the verdict is armed, not executed. The stream is already
    // over — the only thing this changes is whether the viewer reads the
    // server's reason or the client's inference from a runtime mismatch.
    clientLog({level:"warn",event:"truncated_stream",detail:"terminal:"+verdict.code,
      message:`server verdict ended this recipe — ${controlVerdictText(verdict.message)}`,
      control_trigger:controlTrigger});
    // `ended` already means the element stopped on its own, but `player_stopped`
    // is a claim the owner makes rather than one the presenter should have to
    // infer from an event: this branch returns with nothing left to try, so it
    // stops like every other exhaustion site (§3.4) and the claim is true by
    // construction. The ask above can have taken seconds, and a viewer who
    // pressed play in them is exactly the disagreement §3.2 is written for.
    stopPlayerForExhaustion();
    raisePlaybackSurface("owner_stopped",{context:"attached",player_stopped:true,
      title:controlVerdictText(verdict.message),
      detail:"It ended "+clockFromSec(total-pos)+" short of the runtime. Your place is saved.",
      actions:playbackStallActions(PLAYER)});
    return;
  }
  if(verdict&&verdict.type==="hold"){
    // Production is deliberately not advancing, so reconnecting is exactly
    // what a hold says not to do — and here it is worse than churn. A VOD
    // session seeks in place, so resuming at the truncation point re-fires
    // `ended` at once and the loop has nothing in it but one round trip. The
    // reopen is what a hold suppresses; the explanation is what it earns.
    clientLog({level:"warn",event:"truncated_stream",detail:"deferred:hold",
      message:`server holds this stream (${verdict.reason}) — resume deferred`,
      control_trigger:controlTrigger});
    raisePlaybackSurface("control_hold",{context:"attached",
      title:"Waiting for the server.",detail:holdReasonText(verdict.reason),
      actions:["retry","close"]});
    return;
  }
  // A server that named an interval owns the decision for that long, so the
  // local budget does not bound it — but the deferral is itself bounded. A
  // server that keeps saying "soon" is not distinguishable, from here, from
  // one that is never going to be ready, and today's path is better than an
  // unbounded reconnect loop.
  const answered=!!(verdict&&verdict.type==="retry_resource"&&
    PLAYER.endedTries<=CONTROL_DEFER_LIMIT);
  const hopeless = !answered &&
    (PLAYER.method==='direct_play' || PLAYER.endedTries>3);
  clientLog({level:"warn",event:"truncated_stream",
    message:`stream ended at ${Math.round(pos)}s of ${Math.round(total)}s`,
    detail:`method=${PLAYER.method} attempt=${PLAYER.endedTries}`+
      (answered?` deferred:${verdict.type}`:(hopeless?" giving-up":" resuming")),
    control_trigger:controlTrigger});
  if(hopeless){
    // Same as the terminal branch above: the owner is giving up, so it stops.
    stopPlayerForExhaustion();
    raisePlaybackSurface("repeated_early_end",{context:"attached",player_stopped:true,
      title:"The stream stopped before the end.",
      detail:"It ended "+clockFromSec(total-pos)+" short of the runtime. Your place is saved, so resuming picks up here.",
      actions:playbackStallActions(PLAYER)});
    return;
  }
  raisePlaybackSurface("owner_recovery_step",{context:"attached",title:"Reconnecting…",
    detail:"the stream ended early — resuming from "+clockFromSec(pos)});
  PLAYER.stallFrom=pos;   // so "Try again" from a later stall aims here too
  if(answered){
    const after=Math.min(Math.max(verdict.after_ms,CONTROL_MIN_EXCHANGE_MS),PERSISTENT_STALL_MS);
    await new Promise((done)=>setTimeout(done,after));
    // The same window, again: the viewer had `after` milliseconds to leave.
    if(!endedStillOurs(p,generation,actionGeneration,fileId,pos)) return;
  }
  seekTo(pos,false,null,false);
}
// Is the player this `handleEnded` was entered for still the one on screen,
// still on the same file, still on the same stream generation, and still
// waiting at the position it ended at?
//
// Every clause was checked at entry and none of them survives an await on its
// own. Identity rather than file id, because `seekTo` and `attachSession`
// reuse the same PLAYER object, and the seek token because a viewer who
// scrubbed away must not be dragged back to a truncation they have left.
function endedStillOurs(p,generation,actionGeneration,fileId,pos){
  const v=document.getElementById("video");
  return !!p && PLAYER===p && p.fileId===fileId && p.endedAt===pos
    && (p._seekToken||0)===generation
    && (p.controlIntentGeneration||0)===actionGeneration
    && !!v && !v.seeking;
}

