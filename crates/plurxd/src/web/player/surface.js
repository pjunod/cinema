"use strict";
// ---- the playback surface: one presenter, one render ----------------------
//
// docs/clients/PLAYBACK-SURFACE-CONTRACT.md. The overlay used to be an
// imperative message channel — 55 sites pushing a string, a spinner and
// sometimes a `failed` class at one element, each answering "what should the
// viewer see" for itself. It is a projection now: the recovery owner raises a
// typed fault, `PlaybackPolicy.presentSurface` (pure, driven by
// tests/playback/playback-surface-contract.json) decides what is drawn, and
// `renderPlaybackSurface` paints it. Nothing on this side of the line pauses,
// plays, seeks, reopens, cancels a timer or reports to the control plane.

// One presenter for one player surface. `play()` builds a new PLAYER object on
// every start and this page has exactly one overlay with one history, so the
// model outlives the object: identity is the generation each fault carries,
// not which object happened to be holding the state when it was raised.
// `PLAYER.surfaceState` is this, which is why reading `p.surfaceState.surface`
// is the same answer wherever you read it from.
const PLAYBACK_SURFACE={state:PlaybackPolicy.initialSurfaceState(),surface:null,history:[]};
const SURFACE_HISTORY_MAX=16;
const SURFACE_ACTION_LABELS=Object.freeze({keep_waiting:"Keep waiting",retry:"Try again",
  close:"Close",force_transcode:"Force transcode",sign_in:"Sign in"});

// The media generation a fault is about. The contract names the web’s
// generation as the open attempt (§3.1), and `newAttempt` is where one begins:
// every cold start, every seek that reopens, every quality or subtitle change.
// `beginPlaybackMediaAttachment` is where that generation takes the picture, so
// a fault raised while a replacement is being prepared already belongs to the
// replacement and a predecessor keeps its own.
function playbackSurfaceGeneration(p){ return (p&&p.attemptId)||null; }
// The viewer request a fault belongs to, when it belongs to one. A committed
// destination outlives the media element that was carrying it, which is exactly
// what `controlSeek` already is.
function playbackSurfaceIntent(p){ const seek=p&&p.controlSeek; return seek?seek.sequence:null; }
// `start` before this playback has presented anything, `change` while a
// viewer-requested replacement is pending over a predecessor, `attached`
// otherwise — the three the fixture declares.
function playbackSurfaceContext(p){
  if(p&&p.pendingMediaChange) return "change";
  return p&&p.started?"attached":"start";
}
function playbackSurfaceClassOf(source,context){
  for(const row of PlaybackPolicy.SURFACE_SOURCES)
    if(row.id===source&&(row.context==="any"||row.context===context)) return row.class||null;
  return null;
}
function playbackSurfaceClassBlocks(cls){
  const definition=cls?PlaybackPolicy.SURFACE_CLASSES[cls]:null;
  return !!definition&&definition.blocking===true;
}
// The old `PLAYER.stallPrompt` flag, read off the presenter rather than kept as
// a second copy of the same truth: something on screen is asking the viewer to
// answer. A NOTICE with actions counts — a server hold says why production
// stopped and offers Try again, and burying that under a full-screen
// "Buffering…" is the wrong answer twice over: it hides the only explanation
// there is and it hides the buttons that act on it.
function playbackSurfacePromptUp(){
  const surface=PLAYBACK_SURFACE.surface;
  return !!surface&&surface.kind!=="none"&&(surface.actions||[]).length>0;
}
function playbackSurfaceHasLiveFault(source,attached){
  return PLAYBACK_SURFACE.state.faults.some(
    fault=>fault.source===source&&fault.attached===attached);
}
// The buttons a stalled recipe offers. Force transcode is not offered on a
// session that already IS one — the same condition the hand-written HTML had —
// and it carries the reason its former inline `onclick` carried, because
// `fallbackResetBeforeOpen` distinguishes the two and the action vocabulary
// cannot. Recorded on the player rather than on the fault: the presenter's
// state is the contract's, and this is the owner's own bookkeeping.
function playbackStallActions(p,transcodeReason){
  if(p) p.surfaceTranscodeReason=transcodeReason||"stall-manual";
  return ["retry"].concat(p&&p.method!=='transcode'?["force_transcode"]:[]).concat(["close"]);
}
// The same list for an `exhausted` PROMPT, which leads with one more bounded
// attempt. Ruled 2026-09-13: the web offers Keep waiting. The class's default
// actions are `keep_waiting · retry · close` and the label and handler have
// been in place since M1 — the only thing missing was a site that offered it.
// Deliberately not folded into `playbackStallActions`: that list is also what a
// `stopped` terminal carries, and a server verdict that ended the recipe has
// nothing left to wait for, so re-arming a detector over it would be a button
// that lies.
function playbackExhaustedActions(p,transcodeReason){
  return ["keep_waiting"].concat(playbackStallActions(p,transcodeReason));
}
// One step of the presenter: feed the event, keep the state, emit what it says,
// render what it returns. The only way a surface changes.
function playbackSurfaceStep(event){
  const step=PlaybackPolicy.presentSurface(PLAYBACK_SURFACE.state,
    Object.assign({t:Math.round(performance.now())},event));
  PLAYBACK_SURFACE.state=step.state;
  PLAYBACK_SURFACE.surface=step.surface;
  for(const entry of step.log) recordPlaybackSurfaceLog(entry);
  renderPlaybackSurface(step.surface);
  return step.surface;
}
function resetPlaybackSurface(){
  PLAYBACK_SURFACE.state=PlaybackPolicy.initialSurfaceState();
  PLAYBACK_SURFACE.history=[];
  return playbackSurfaceStep({});
}
// The owner saying what happened. `source` is a row in the fixture; everything
// else is what the owner knows about it. This function decides nothing about
// what is shown — that is the reducer’s, and it is the whole point.
function raisePlaybackSurface(source,fault,options){
  const p=PLAYER, given=fault||{};
  const attached=given.attached!==undefined?given.attached:playbackSurfaceGeneration(p);
  // A fact the owner is restating rather than a new one: a `waiting` that fires
  // at every fragment boundary must not push a fault per boundary at the ring.
  if(options&&options.once&&playbackSurfaceHasLiveFault(source,attached)) return null;
  return playbackSurfaceStep({
    raise:source,
    context:given.context||playbackSurfaceContext(p),
    attached,
    intent:given.intent!==undefined?given.intent:null,
    player_stopped:!!given.player_stopped,
    title:given.title!==undefined?given.title:null,
    detail:given.detail!==undefined?given.detail:null,
    position_ms:given.position_ms!==undefined?given.position_ms:null,
    actions:given.actions,
  });
}
// Does the shared table answer this refusal with a class that covers the
// picture? A READ of the table and nothing else — it says what the surface
// would be, never what the player should do. The decision to stop is the
// raising site's, because the class table is not allowed to move the player.
function playbackSurfaceSourceIsBlocking(source,context){
  return playbackSurfaceClassBlocks(playbackSurfaceClassOf(source,context));
}
// The recovery owner’s one new obligation (§3.4): when it has nothing left to
// try it stops the player, and only then raises. `pausePlaybackInternally`
// keeps `wantsPlayback`, so Try again still knows the viewer wanted this
// playing. Nothing else about the sites that call this changes.
function stopPlayerForExhaustion(){
  const p=PLAYER;
  if(p) retireHlsTerminalAttempt(p);
  const v=document.getElementById("video");
  if(v) pausePlaybackInternally(v);
  stopPlayerTimers();
}
function retireHlsTerminalAttempt(p){
  if(!p) return;
  const mediaAttachment=p.mediaAttachment||null;
  const episode=p.hlsStartup;
  const ownsEpisode=!!(episode&&p.hls===episode.hls&&episode.attachment.current());
  p.terminalStop={attachment:mediaAttachment,attemptId:p.attemptId||null,
    intentGeneration:p.controlIntentGeneration||0};
  if(ownsEpisode){
    episode.state='cancelled';
    episode.cancelledReason='owner_stopped';
    episode.establishedSuspension=null;
    clearTimeout(episode.retry.timer);episode.retry.timer=null;
    abortHlsStartupLoaders(episode);
  }
  try{if(p.hls)p.hls.stopLoad();}catch(e){}
}
// A button the presenter drew. The reducer clears the fault the action belonged
// to; the EFFECT is the recovery owner’s, which is why it happens here and not
// inside the render (§3.2).
function playbackSurfaceAction(action){
  const p=PLAYER;
  playbackSurfaceStep({user_action:action});
  if(action==="retry") return retryPlayback();
  if(action==="close") return closePlayer();
  if(action==="force_transcode")
    return startTranscodeFallback((p&&p.surfaceTranscodeReason)||"stall-manual");
  if(action==="keep_waiting"){
    // §3.1’s web definition of Keep waiting, and nothing more: one more bounded
    // attempt from the existing ladder.
    if(p){ p.recoveringStall=null; armStall(pbPosSec()); }
    return undefined;
  }
  if(action==="sign_in"){
    // `api()` already ends the session on a 401, so this is the defensive path
    // for a refusal body that reached the player another way. Local only: the
    // bearer this player was using is the one the server refused.
    closePlayer();
    return logout({revoke:false,notice:"Sign in again to keep watching."});
  }
  return undefined;
}
// §3.6: the same four events with the same field names on every client, so the
// server-side session log joins on `session_id`/`attempt` rather than on a
// generation only this browser understands.
function recordPlaybackSurfaceLog(entry){
  if(!entry||!entry.event) return;
  const p=PLAYER;
  const identity={session_id:(p&&p.sessionId)||null,attempt:(p&&p.attemptId)||null};
  if(entry.event==="surface_error"){
    clientLog(Object.assign({level:"error",
      message:`the presenter refused ${entry.source}: ${entry.error}`},entry,identity));
    return;
  }
  if(entry.event==="surface_raised") pushPlaybackSurfaceHistory(entry,identity);
  else if(entry.event==="surface_cleared"||entry.event==="surface_disagreement")
    closePlaybackSurfaceHistory(entry);
  clientLog(Object.assign({
    level:entry.event==="surface_disagreement"?"warn":"info",
    message:playbackSurfaceLogMessage(entry)},entry,identity));
}
function playbackSurfaceLogMessage(entry){
  if(entry.event==="surface_raised") return `${entry.class} surface raised by ${entry.source}`;
  if(entry.event==="surface_cleared") return `${entry.class} surface cleared by ${entry.by}`;
  if(entry.event==="surface_log_only") return `${entry.source} is log only — no surface`;
  if(entry.event==="surface_disagreement")
    return `the picture moved under a ${entry.class} surface — the picture wins`;
  return entry.event;
}
// The bounded ring the Playback debug ledger reads. "What was that overlay" has
// an answer from inside the product, which is the whole of §5.
function pushPlaybackSurfaceHistory(entry,identity){
  const v=document.getElementById("video");
  PLAYBACK_SURFACE.history.push({
    class:entry.class,source:entry.source,attached:entry.attached,intent:entry.intent,
    session_id:identity.session_id,attempt:identity.attempt,
    raised_at:Date.now(),cleared_at:null,cleared_by:null,
    player_at_raise:{
      rate:v?(v.paused?0:(Number(v.playbackRate)||1)):null,
      position_ms:Math.round(pbPosSec()*1000),
      presenting:!!PLAYBACK_SURFACE.state.presenting,
      stopped_by_owner:!!entry.player_stopped}});
  while(PLAYBACK_SURFACE.history.length>SURFACE_HISTORY_MAX) PLAYBACK_SURFACE.history.shift();
}
function closePlaybackSurfaceHistory(entry){
  const history=PLAYBACK_SURFACE.history;
  for(let index=history.length-1;index>=0;index--){
    const row=history[index];
    if(row.cleared_at!=null) continue;
    if(row.source!==entry.source||row.attached!==entry.attached) continue;
    row.cleared_at=Date.now();
    row.cleared_by=entry.event==="surface_disagreement"?"disagreement":(entry.by||null);
    return;
  }
}
function playbackSurfaceHistoryText(){
  const history=PLAYBACK_SURFACE.history;
  if(!history.length) return null;
  return history.slice().reverse().map(row=>{
    const at=row.player_at_raise||{};
    const span=row.cleared_at==null?"open":`${((row.cleared_at-row.raised_at)/1000).toFixed(1)} s → ${row.cleared_by||"cleared"}`;
    return `${row.class} · ${row.source} · ${span} · rate ${at.rate==null?"?":at.rate}`+
      ` · ${clockFromSec((at.position_ms||0)/1000)} · ${at.presenting?"presenting":"not presenting"}`+
      ` · ${at.stopped_by_owner?"owner stopped":"owner running"}`;
  }).join("\n");
}
// playback-surface-render:begin
// The whole of the presenter’s output, and the only code in this file that
// writes the overlay, the `failed` class, #ploadAct or the two notice
// elements. It is a projection and nothing else: no pause, no play, no seek,
// no reopen, no timer, no report. scripts/playback-surface-fence keeps it so.
function renderPlaybackSurface(surface){
  const kind=(surface&&surface.kind)||"none";
  const title=(surface&&surface.title)||"";
  // A media wait's sentence is a live reading, refreshed by the sampling tick
  // into `waitDetail`; every other fault says what its owner said.
  const live=surface&&surface.source==="media_waiting"?renderPlaybackSurface.waitDetail:null;
  const detail=live||(surface&&surface.detail)||"";
  const actions=(surface&&surface.actions)||[];
  // The presenter runs on every evidence sample — twice a second while a film
  // plays — and repainting an unchanged surface would rebuild the action
  // buttons under whichever one the viewer had focused. Paint on change, and
  // nothing else reaches these elements, so "unchanged" is the truth.
  const signature=[kind,surface&&surface.class,title,detail,actions.join("\u241f"),
    !!(surface&&surface.input_failed)].join("\u241e");
  if(renderPlaybackSurface.painted===signature) return;
  renderPlaybackSurface.painted=signature;
  setLoading(kind==="blocking",title,detail,
    kind==="blocking"?playbackSurfaceActionHtml(actions):"",
    !!(surface&&surface.input_failed));
  renderPlaybackSurfaceNotice(kind==="banner",title,detail,actions);
  renderPlaybackSurfaceIndicator(kind==="indicator",live?`${title} · ${live}`:title);
}
function playbackSurfaceActionHtml(actions){
  return (actions||[]).map(action=>{
    const label=SURFACE_ACTION_LABELS[action];
    if(!label) return "";
    return `<button onclick="playbackSurfaceAction('${action}')">${esc(label)}</button>`;
  }).join("");
}
// A notice strip beside the picture: the picture itself is untouched.
function renderPlaybackSurfaceNotice(on,title,detail,actions){
  const box=document.getElementById("psurface"); if(!box) return;
  const text=document.getElementById("psurfText");
  const sub=document.getElementById("psurfSub");
  const act=document.getElementById("psurfAct");
  if(on){
    if(text) text.textContent=title;
    if(sub) sub.textContent=detail||"";
    if(act) act.innerHTML=playbackSurfaceActionHtml(actions);
  } else if(act) act.innerHTML="";
  box.classList.toggle("on",!!on);
}
// The in-chrome indicator: the player is working on it and the picture behind
// this is presenting, so nothing is allowed to cover it.
function renderPlaybackSurfaceIndicator(on,title){
  const box=document.getElementById("pindicator"); if(!box) return;
  const text=document.getElementById("pindText");
  if(on&&text) text.textContent=title||"Working…";
  box.classList.toggle("on",!!on);
}
// The staged loading overlay: "reading media → starting transcoder → buffering"
// instead of a blank gray screen. It is the presenter’s private paint helper
// now rather than a channel call sites can push a string at, and `failed` is
// the input contract’s `failed` state (§4) — a blocking surface with an answer
// to give — rather than a flag four sites set by hand.
function setLoading(on,text,sub,actionHtml,failed){
  const l=document.getElementById("ploading"); if(!l) return;
  const a=document.getElementById("ploadAct");
  if(on){
    l.classList.toggle("failed",!!failed);
    const t=document.getElementById("ploadText"), s=document.getElementById("ploadSub");
    if(t&&text!=null) t.textContent=text;
    if(s) s.textContent=sub||"";
    if(a) a.innerHTML=actionHtml||"";
    l.classList.add("on");
  } else { l.classList.remove("on","failed"); if(a) a.innerHTML=""; }
}
// playback-surface-render:end
