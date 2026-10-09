"use strict";
// ---- autoplay next episode (per browser, default on) ----------------------
function autoNextOn(){ try{ return localStorage.getItem("plurx_autonext")!=="0"; }catch(e){ return true; } }
function setAutoNext(on){
  if(!on){cancelNextEpisodePreparation(PLAYER);clearAutoplayNextPreparation();}
  try{ localStorage.setItem("plurx_autonext", on?"1":"0"); }catch(e){}
  const b=document.getElementById("autonextbtn"); if(b) b.classList.toggle("on", on);
  const c=document.getElementById("autonext"); if(c) c.checked=on;
  // The OS transport's Next exists only while autoplay-next is on.
  syncPlayerNextTrack(); }
function togglePlayerAutonext(){ setAutoNext(!autoNextOn()); toast(autoNextOn()?"Autoplay next: on":"Autoplay next: off"); }
// The existing progress tick can prepare one successor inside the last 30 s.
// This owns only metadata reads: no session, encoder, watched mark or media
// fetch is started before the actual transition. Playback still asks the
// server for its current policy and capabilities decision when it opens.
const NEXT_EPISODE_PREPARE_SEC=30;
const NEXT_EPISODE_METADATA_MS=60000;
let AUTOPLAY_NEXT_PREPARED=null;
function clearAutoplayNextPreparation(){AUTOPLAY_NEXT_PREPARED=null;}
function cancelNextEpisodePreparation(p){
  if(!p?.nextEpisodePreparation)return;
  p.nextEpisodePreparation.owner.cancel();
  p.nextEpisodePreparation=null;
}
function nextEpisodePreparationCurrent(p,state){
  return p.wantsPlayback&&p.nextEpisodePreparation===state&&state.current()&&autoNextOn()
    &&performance.now()-state.began<NEXT_EPISODE_METADATA_MS;
}
function takeAutoplayNextPreparation(id){
  const prepared=AUTOPLAY_NEXT_PREPARED;
  clearAutoplayNextPreparation();
  return prepared&&autoNextOn()&&prepared.page.id===String(id)
    &&performance.now()-prepared.began<NEXT_EPISODE_METADATA_MS?prepared:null;
}
async function resolveNextEpisodePage(itemId,preparation){
  const read=path=>preparation.run(signal=>api(path,{signal}));
  const cur=await read(`/items/${itemId}`);
  if(cur.item?.kind!=="episode")return null;
  const anc=cur.ancestors||[],season=anc[anc.length-1],show=anc[anc.length-2];
  if(!season)return null;
  const sd=await read(`/items/${exactWireId(season)}`);
  const eps=(sd?.children||[]).filter(c=>c.kind==="episode");
  const i=eps.findIndex(e=>exactWireId(e)===itemId);
  let next=i>=0?eps[i+1]:null;
  if(!next&&show){
    const shd=await read(`/items/${exactWireId(show)}`);
    const seasons=(shd?.children||[]).filter(c=>c.kind==="season");
    const si=seasons.findIndex(s=>exactWireId(s)===exactWireId(season));
    if(si>=0&&seasons[si+1]){
      const nsd=await read(`/items/${exactWireId(seasons[si+1])}`);
      next=(nsd?.children||[]).find(c=>c.kind==="episode");
    }
  }
  if(!next)return null;
  const id=exactWireId(next);
  const data=await read(`/items/${id}`);
  const libs=await preparation.run(()=>libsCached());
  return itemPageModel(id,data,libs);
}
function prepareNextEpisodeIfNearEnd(p,video){
  const state=p?.nextEpisodePreparation;
  if(state&&(!p.wantsPlayback||!state.current()||!autoNextOn()))cancelNextEpisodePreparation(p);
  // A cold start can fail before it has a file-bound player. Progress/Close
  // still tick the initial object, which has no episode to prepare. Close also
  // ticks while cancelling a pending seek, before resetting the attached video;
  // only the owner's playback intent can admit new work during that teardown.
  if(!p?.wantsPlayback||!p.fileId||playbackFileContextForPlayer(p).source_ref.kind!=="local"||!autoNextOn()||p.libraryChannel||p.bookParts||video.paused||video.seeking
    ||!playbackOwnsAttachedMedia(p))return;
  const remaining=pbTotalSec()-pbPosSec();
  if(!(pbTotalSec()>0&&remaining>0&&remaining<=NEXT_EPISODE_PREPARE_SEC))return;
  if(p.nextEpisodePreparation)return;
  const itemId=ITEM_FOR_FILE[playbackFileKey(playbackFileContextForPlayer(p))];
  if(!itemId||p.meta?.kind!=="episode")return;
  const current=playbackContinuation(p);
  const next={began:performance.now(),current,owner:null,promise:null,page:null};
  next.owner=beginPlaybackPreparation(()=>nextEpisodePreparationCurrent(p,next),{background:true});
  p.nextEpisodePreparation=next;
  next.promise=resolveNextEpisodePage(itemId,next.owner).then(page=>{
    if(nextEpisodePreparationCurrent(p,next))next.page=page;
    return next.page;
  }).catch(()=>null).finally(()=>next.owner.finish());
}
// A cold/manual Next uses the same resolver. A ready or in-flight successor is
// consumed once; errors in background preparation leave the normal path intact.
async function playNextEpisode(){
  const p=PLAYER,continuation=playbackContinuation(p);
  const current=()=>continuation()&&autoNextOn();
  if(p&&playbackFileContextForPlayer(p).source_ref.kind!=="local")return playNextSharedEpisode(current);
  if(!p)return false;
  const itemId=ITEM_FOR_FILE[playbackFileKey(playbackFileContextForPlayer(p))];if(!itemId)return false;
  const state=p.nextEpisodePreparation;
  const preparation=beginPlaybackPreparation(current);
  try{
    let page=null;
    if(state&&nextEpisodePreparationCurrent(p,state)){
      page=await preparation.run(()=>state.promise);
      if(!nextEpisodePreparationCurrent(p,state))page=null;
    }
    if(!current())return false;
    cancelNextEpisodePreparation(p);
    if(!page)page=await resolveNextEpisodePage(itemId,preparation);
    if(!current()||!page)return false;
    const prepared={page,began:performance.now()};
    if(WATCH&&watchBrowserMounted())return await watchPlayEpisode(page.id,prepared);
    AUTOPLAY_NEXT_PREPARED=prepared;
    AUTOPLAY=page.id;
    toast("▶ Up next: "+page.item.title);
    location.hash="#/item/"+page.id;
    return true;
  }catch(error){
    if(current())toast("Could not load the next episode. Choose it from the library to retry.");
    return false;
  }finally{preparation.finish();}
}
// Shared next episode: Source order read through B, then a new authorized
// start from fresh details. A Source ID never reaches the Local router.
async function playNextSharedEpisode(current){
  const reference=PLAYER?.meta?.sharedReference;if(!reference)return false;
  const preparation=beginPlaybackPreparation(current);
  let next=null;
  try{next=await sharedCatalogueNextEpisode(reference,path=>preparation.run(signal=>api(path,{signal})));}
  catch(error){if(current())toast("Could not load the next shared episode. Choose it from the library to retry.");return false;}
  finally{preparation.finish();}
  if(!next||!current())return false;
  try{toast("▶ Up next");await sharedCatalogueLaunch(next,null,current);return true;}
  catch(error){if(current())toast(error.message||"The next shared episode is not available.");return false;}
}
function playbackContinuation(p){
  const action=p?.controlIntentGeneration||0, generation=p?._seekToken||0;
  const open=p?.pendingOpenAttempt;
  return ()=>!!p&&PLAYER===p&&(p.controlIntentGeneration||0)===action
    &&(p._seekToken||0)===generation&&p.pendingOpenAttempt===open;
}
async function playNextAudiobookPart(){
  if(!PLAYER||!PLAYER.bookParts||PLAYER.bookParts.length<2) return false;
  if(playbackFileContextForPlayer(PLAYER).source_ref.kind!=="local")return false;
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
