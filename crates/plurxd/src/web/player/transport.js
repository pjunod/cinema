"use strict";
// ---- custom transport (scrubber + times) ----------------------------------
// Total from the known duration (PLAYER.durMs), position from offset+currentTime
// — the values the native <video> bar gets wrong on a growing remux / offset HLS
// stream. Seeking routes through the method-aware seekTo().
function pbTotalSec(){ const v=document.getElementById("video");
  if(PLAYER&&PLAYER.bookDuration) return PLAYER.bookDuration/1000;
  if(PLAYER&&PLAYER.durMs) return PLAYER.durMs/1000;
  if(PLAYER&&PLAYER.knownDur) return PLAYER.knownDur/1000;
  // A remux/transcode stream's `duration` is only what has arrived so far — it
  // grows as ffmpeg writes. Scaling the scrubber by it would put the thumb near
  // the end at all times and map a drag to a target that moves under the finger
  // (dragging "forward" would seek backwards). With no trustworthy total, report
  // none: pbTick draws an indeterminate bar and seekTo refuses fraction seeks.
  if(PLAYER&&PLAYER.method&&PLAYER.method!=='direct_play') return 0;
  return (v&&isFinite(v.duration)&&v.duration>0)?((PLAYER&&PLAYER.offset)||0)+v.duration:0; }
function pbPosSec(){ const v=document.getElementById("video"); return ((PLAYER&&PLAYER.bookOffset)||0)/1000+((PLAYER&&PLAYER.offset)||0)+((v&&v.currentTime)||0); }
// A stream-changing command issued while a seek is still landing belongs at
// the seek destination, not at the predecessor element's last observed clock.
// The committed target is the playback intent; currentTime is only evidence
// about how far the outgoing presentation happened to get.
function positionForPlaybackIntent(v,p){
  const target=p&&p.controlSeek&&Number(p.controlSeek.targetMs);
  if(Number.isFinite(target)) return Math.max(0,target/1000);
  return Math.max(0,((p&&p.offset)||0)+((v&&v.currentTime)||0));
}
// A committed destination no attachment has executed yet, on the scrubber's
// global timeline. `executed` is the split the whole repair turns on: it is
// set when a local seek is applied to the attached element or when a
// successor attaches, so an unexecuted destination is a *desired* replacement
// and the picture on screen is still the predecessor's.
function unexecutedPlaybackDestinationSec(p){
  const pending=p&&p.controlSeek;
  if(!pending||pending.executed) return null;
  // ONLY while a replacement is actually pending or retained after a refusal.
  // `controlSeek` outlives the element by design and `play()` carries it into
  // the successor with `executed:false`, so a destination can survive into a
  // playback that is nowhere near it -- `replayEnded` restarts at zero with a
  // 2:54 destination still attached. Without this the scrubber would freeze
  // at 2:54 and every +10 would count from there. `pendingMediaChange` is the
  // retained route: set before the create, kept on refusal for Try again,
  // cleared when a successor attaches.
  if(!p.pendingMediaChange) return null;
  // Multipart audiobooks address the scrubber globally and the element
  // locally, and `controlSeek` is in the part-local timeline, so the two
  // numbers are not comparable. `bookOffset` is that delta, but a book that
  // needed a replacement would need it applied in three places; the honest
  // answer for now is to leave their arithmetic exactly as it was.
  if(p.bookParts&&p.bookParts.length) return null;
  const target=Number(pending.targetMs);
  return Number.isFinite(target)?Math.max(0,target/1000):null;
}
// Where a relative seek counts from. Not the attached element's clock: while
// a replacement is pending the incumbent is still showing 100 seconds, so
// three separately committed +10 taps would each land on 110. The desired
// destination is the base until an attachment actually executes it.
function pbRelativeSeekBase(){
  const me=PLAYER;
  if(!me) return 0;
  if(me._seekPending!=null) return me._seekPending;
  const desired=unexecutedPlaybackDestinationSec(me);
  return desired!=null?desired:pbPosSec();
}
// The identity of one media change, normalized: what the create will ask the
// server for, not what the player currently is.
//
// Both halves matter. Position alone is not identity -- an audio or subtitle
// switch at the same second is a different request and must still execute.
// And the player's own fields are not identity either: `p.method`,
// `p.copyHls` and `p.autoHeight` are what the change is about to *replace*,
// and they are not written until after the create returns, so keying on them
// would make an in-flight rung switch indistinguishable from a plain seek on
// the incumbent recipe. `change` carries what is actually going on the wire.
function playbackChangeRecipeKey(p,targetSec,change){
  if(!p) return null;
  const c=change||{};
  return JSON.stringify([
    p.fileId,
    Math.max(0,Math.round(Number(targetSec||0)*1000)),
    c.method===undefined?(p.method||null):(c.method||null),
    c.copyHls===undefined?!!p.copyHls:!!c.copyHls,
    c.height==null?null:c.height,
    !!c.forceReopen,
    c.previousSessionId||null,
    c.recoveryCause||null,
    !!c.automatic,
    typeof selectedAudioIndex==="function"?selectedAudioIndex(p):null,
    p.curSub==null?null:p.curSub,
    p.burnedSub==null?null:p.burnedSub,
    typeof transcodeHeight==="function"?(transcodeHeight()??null):null,
    typeof qualityForce==="function"?qualityForce():null,
    p.autoHeight==null?null:p.autoHeight,
    !!p.requestHdr10,
    !!p.preserveDolbyVision,
    Number(p.aoffset)||0,
  ]);
}
// R2. True only for a request identical to one whose create is still open.
// Checked *before* the intent generation moves, because bumping it first and
// then noticing the duplicate would cancel the very execution it matched.
function playbackChangeAlreadyInFlight(p,targetSec,change){
  if(!p||!p.inFlightChangeKey) return false;
  return p.inFlightChangeKey===playbackChangeRecipeKey(p,targetSec,change);
}
// Same number, clamped to the runtime, for anything the viewer reads. A remux
// seek to N starts ffmpeg at the keyframe *at or before* N while we count from
// N, so the sum drifts a second or two long and the clock ends up reading past
// the end of the film. Clamp the display; leave pbPosSec itself honest, since
// resume points and seeks want the raw figure.
function pbShownSec(){ const p=pbPosSec(), t=pbTotalSec(); return t>0?Math.min(p,t):p; }
// A committed destination outlives the media element/session it replaces.
// `_seekPreview` is only pointer UI and is cleared before seekTo(); this is the
// control-plane intent, monotonically superseded by later seeks and cleared
// only after the requested timeline is actually presenting.
function beginPlaybackControlSeek(p,targetSec,supersedeIntent=true){
  if(!p) return null;
  const intentGeneration=supersedeIntent
    ? supersedePlaybackControlIntent(p) : (p.controlIntentGeneration||0);
  const sequence=(p.controlSeekSequence||0)+1;
  // The destination this replaces is superseded, which is the one thing that
  // retires a fault about a pending destination (contract §3.4).
  if(p.controlSeek) playbackSurfaceStep({intent_superseded:p.controlSeek.sequence});
  p.controlSeekSequence=sequence;
  let frameFloor=Number(p.controlPresentedFrames)||0;
  if(!p.controlHasFrameCallbacks){
    try{
      const q=document.getElementById("video")?.getVideoPlaybackQuality?.();
      if(q&&Number.isFinite(q.totalVideoFrames)) frameFloor=q.totalVideoFrames;
    }catch(e){}
  }
  p.controlSeek={sequence,intentGeneration,
    targetMs:Math.max(0,Math.round(targetSec*1000)),
    executed:false,frameFloor,audioPositionMs:null};
  const reporter=p.controlReporter;
  const predicted=reporter&&!reporter.stopped?(Number(reporter.sequence)||0)+1:null;
  const reported=notifyPlaybackControl();
  if(reported&&predicted!=null){
    p.controlSequenceFloor=Math.max(Number(p.controlSequenceFloor)||0,
      predicted,Number(reporter.sequence)||0);
  }
  return p.controlSeek;
}
function markPlaybackControlSeekExecuted(p,targetSec,v){
  const pending=p&&p.controlSeek;
  const targetMs=Math.max(0,Math.round(Number(targetSec||0)*1000));
  if(!pending||pending.targetMs!==targetMs) return false;
  // The destination landed. A fault that was about it is retired by this and
  // by nothing else — evidence about the picture never speaks for a pending
  // request (contract §3.4).
  playbackSurfaceStep({intent_settled:pending.sequence});
  pending.executed=true;
  pending.executedAt=performance.now();
  p.controlPresentationEpoch=(p.controlPresentationEpoch||0)+1;
  pending.frameFloor=Number(p.controlPresentedFrames)||pending.frameFloor||0;
  pending.audioPositionMs=null;
  pending.sampleAt=pending.executedAt;
  pending.sampleRate=0;
  pending.activeSampleMs=0;
  pending.playedSampleMs=0;
  if(!v) try{v=document.getElementById("video");}catch(e){}
  samplePlaybackPresentationClock(v,p);
  if(!p.controlHasFrameCallbacks) try{
    const q=v?.getVideoPlaybackQuality?.();
    if(Number.isFinite(q?.totalVideoFrames)) pending.frameFloor=q.totalVideoFrames;
  }catch(e){}
  return true;
}
// Sample at play/pause/rate/visibility edges as well as at media observations.
// A 1 Hz audio clock need not hit a fixed 250 ms target window: the expected
// timeline has advanced since execution. Only active monotonic time counts;
// pause/background time cannot expand the landing allowance indefinitely.
function samplePlaybackPresentationClock(v,p){
  if(!playbackOwnsAttachedMedia(p))return 0;
  const pending=p?.controlSeek;
  if(!pending?.executed) return 0;
  const now=performance.now();
  const dt=Math.max(0,now-(pending.sampleAt??now));
  if(pending.sampleRate>0){
    const active=Math.min(dt,Math.max(0,8000-(pending.activeSampleMs||0)));
    pending.activeSampleMs=(pending.activeSampleMs||0)+active;
    pending.playedSampleMs=(pending.playedSampleMs||0)+active*pending.sampleRate;
  }
  pending.sampleAt=Math.max(now,pending.sampleAt??now);
  let hidden=false;
  try{hidden=document.hidden;}catch(e){}
  const rate=Number(v?.playbackRate??1);
  pending.sampleRate=v&&!v.paused&&!v.ended&&!hidden&&Number.isFinite(rate)&&rate>0 ? rate : 0;
  return pending.playedSampleMs||0;
}
// Media elements can remain READY/playing while their decoder has stopped.
// This clock runs independently of waiting/playing events and frame callbacks.
// A pending destination has its own presentation deadline even if the old
// picture or audio clock continues to move during the replacement.
function playbackProgressTick(v,p){
  // The presenter's timers run off the tick the player already has, per §4.2:
  // a timed notice expires because this ran, not because a watchdog of its own
  // woke up. It is fed before every guard below, because a surface must keep
  // ageing while the picture it is about is being replaced.
  playbackSurfaceStep({tick:true});
  if(!v||!p||PLAYER!==p) return;
  // No proof either way about a predecessor this player does not own: the
  // reducer is told nothing rather than told something false.
  if(!playbackOwnsAttachedMedia(p)) return;
  samplePlaybackPresentationClock(v,p);
  // M3. A getter on the attached element, on a tick that already runs: the
  // cheapest honest place to take the frame counter a prepared commit is
  // measured from.
  samplePreparedSwitchFrames(p,v);
  const pending=p.controlSeek;
  const active=(p.wantsPlayback??!v.paused)&&!v.ended&&!document.hidden
    &&(p.started||pending?.executed);
  if(!active){
    p.progressWatch=null;
    playbackSurfaceStep({presenting:false,attached:playbackSurfaceGeneration(p)});
    return;
  }
  const now=performance.now();
  const clock=Number(v.currentTime)||0;
  let frames=null, frameSource="none";
  if(streamHasVideo(p,v)){
    if(p.controlHasFrameCallbacks){
      // Callback support is itself the observation contract. Zero presented
      // callbacks is real evidence and must not fall through to a decoded-frame
      // counter that can advance while the compositor remains frozen.
      frames=Number(p.controlPresentedFrames)||0; frameSource="callbacks";
    }else try{
      const q=v.getVideoPlaybackQuality?.();
      if(Number.isFinite(q?.totalVideoFrames)){frames=q.totalVideoFrames; frameSource="quality";}
    }catch(e){}
  }
  const key=`${p._seekToken||0}:${p.controlIntentGeneration||0}:${pending?.sequence||0}:${frameSource}`;
  let watch=p.progressWatch;
  if(!watch||watch.key!==key){
    watch=p.progressWatch={key,clock,frames,at:now,startedAt:now,fired:false};
  }
  const moved=clock>watch.clock+0.01 && (frames==null||frames>watch.frames);
  // THE evidence (contract §3.4): the film clock advanced and, where a frame
  // counter exists, so did the frames. Not `playing`, not `canplay` — a
  // decoder emits both and can still present nothing.
  playbackSurfaceStep({presenting:moved,attached:playbackSurfaceGeneration(p)});
  if(moved){
    watch.clock=clock; watch.frames=frames; watch.at=now;
    p.presentationAdvancedAt=now;
    if(pending) settlePlaybackControlSeek(v,p,clock,frames);
    completeHlsStartup(p);
    if(!p.controlSeek){
      watch.fired=false;
      if(p.waitAt!=null){
        endWait(true);
        clearStall(); finishStallRecovery("recovered");
      }
      else if(p.recoveringStall){ clearStall(); finishStallRecovery("recovered"); }
    }
  }
  // Exclude time spent paused or backgrounded. A new active watch starts a
  // fresh observation window, while an active replacement cannot reset it.
  const age=now-watch.at;
  const landingAge=pending?.executed
    ? now-Math.max(pending.executedAt??now,watch.startedAt) : 0;
  if(watch.fired||p.waitAt!=null||Math.max(age,landingAge)<PERSISTENT_STALL_MS) return;
  watch.fired=true;
  p.waitAt=now-Math.max(age,landingAge);
  p.waitStartedRunway=bufferRunway(v);
  p.waitReported=false;
  persistentWait(v,p,p.waitAt,p._seekToken||0,p.controlIntentGeneration||0).catch(()=>{});
}
function settlePlaybackControlSeek(v,p,presentedMediaTime,presentedFrameSequence){
  if(!playbackOwnsAttachedMedia(p))return false;
  const pending=p&&p.controlSeek;
  const playedMs=samplePlaybackPresentationClock(v,p);
  if(!pending||!pending.executed||!v||!p.started||v.seeking) return false;
  const hasVideo=!!(p.source&&p.source.video_codec);
  const inLandingWindow=position=>position>=pending.targetMs-250
    &&position<=pending.targetMs+playedMs+250;
  let positionMs=null, frameSequence=null;
  if(presentedMediaTime!=null&&Number.isFinite(Number(presentedMediaTime))){
    positionMs=Math.max(0,Math.round(((p.offset||0)+Number(presentedMediaTime))*1000));
    frameSequence=Number(presentedFrameSequence);
  }else if(!p.controlHasFrameCallbacks&&hasVideo){
    try{
      const q=v.getVideoPlaybackQuality&&v.getVideoPlaybackQuality();
      if(q&&Number.isFinite(q.totalVideoFrames)) frameSequence=q.totalVideoFrames;
    }catch(e){}
    positionMs=Math.max(0,Math.round(((p.offset||0)+(v.currentTime||0))*1000));
  }else if(!hasVideo && !v.paused){
    positionMs=Math.max(0,Math.round(((p.offset||0)+(v.currentTime||0))*1000));
    const previous=pending.audioPositionMs;
    if(previous==null){
      if(!inLandingWindow(positionMs)) return false;
      pending.audioPositionMs=positionMs;
      return false;
    }
    if(positionMs<=previous) return false;
    frameSequence=pending.frameFloor+1;
  }else return false;
  if(!Number.isFinite(frameSequence)||frameSequence<=pending.frameFloor) return false;
  if(!inLandingWindow(positionMs)) return false;
  p.controlSeek=null;
  notifyPlaybackControl();
  return true;
}
function pbTick(){
  watchMarkChapter();
  if(!PLAYER) return;
  const v=document.getElementById("video");
  const tot=pbTotalSec();
  // Desired first, then the drag preview, then the picture. A destination
  // the viewer committed and no attachment has executed is where they asked
  // to be; letting the thumb snap back to the incumbent's clock would make
  // the next +10 read as +10 from the wrong place.
  const desired=unexecutedPlaybackDestinationSec(PLAYER);
  const pos=PLAYER._seekPending!=null?PLAYER._seekPending
    :(PLAYER._seekPreview!=null?PLAYER._seekPreview
    :(desired!=null?(desired>0&&tot>0?Math.min(desired,tot):desired):pbShownSec()));
  const f=tot>0?Math.min(1,Math.max(0,pos/tot)):0;
  const fill=document.getElementById("pseekfill"), thumb=document.getElementById("pseekthumb"), buf=document.getElementById("pseekbuf");
  if(fill) fill.style.width=(f*100)+"%";
  if(thumb) thumb.style.left=(f*100)+"%";
  const cur=document.getElementById("ptcur"); if(cur) cur.textContent=clockFromSec(pos);
  const dur=document.getElementById("ptdur"); if(dur) dur.textContent=tot>0?clockFromSec(tot):"--:--";
  if(buf){ let bf=0; try{ if(v&&v.buffered&&v.buffered.length){ const be=((PLAYER.bookOffset)||0)/1000+((PLAYER.offset)||0)+v.buffered.end(v.buffered.length-1); bf=tot>0?Math.min(1,be/tot):0; } }catch(e){} buf.style.width=(bf*100)+"%"; }
  const sk=document.getElementById("pseek");
  if(sk){ sk.setAttribute("aria-valuenow", Math.round(f*100)); sk.setAttribute("aria-valuetext", clockFromSec(pos)+(tot>0?" of "+clockFromSec(tot):"")); }
  // Seeking a transcode restarts the session at a new offset, which moves the
  // whole subtitle timeline — re-shift the cues whenever that happens.
  if(playbackOwnsAttachedMedia(PLAYER)&&!PLAYER.controlSeek&&PLAYER.curSub>=0 && PLAYER._subOff!=null && PLAYER._subOff!==(PLAYER.offset||0)) setSub(PLAYER.curSub);
}
function pbSyncPlayIcon(){ const v=document.getElementById("video"), b=document.getElementById("pbplay");
  if(b&&v){ const paused=(v.paused||v.ended),following=!!(PLAYER&&PLAYER.libraryChannel); b.textContent=paused?"▶":"❚❚"; b.setAttribute("aria-label", paused&&following?"Resume live":paused?"Play":"Pause"); b.title=paused&&following?"Resume live":"Play / Pause (space)"; } }
// A control generation is terminal once the server accepts demand=end. The
// ended media element can technically be played again, but that would reuse a
// reporter which has deliberately stopped and, for HLS, a completed session.
// Replay therefore takes the normal cold-open path and gets a fresh delivery
// session, control generation, and sequence space before the first frame.
function replayEnded(){
  const p=PLAYER;
  if(!p||!p.fileId) return false;
  const openAttempt=PLAY_OPEN_GATE.begin("replay");
  if(!openAttempt) return false;
  p.wantsPlayback=true;
  PENDING_ATTEMPT_REASON="replay";
  const opening=Promise.resolve(
    play(p.fileId,p.title||"",0,p.knownDur||p.durMs||0,p.meta,openAttempt));
  opening.catch(()=>{}).finally(()=>{
    PLAY_OPEN_GATE.finish(openAttempt);
  });
  return true;
}
function togglePlay(){
  const v=document.getElementById("video"); if(!v) return;
  if(PLAYER&&PLAYER.libraryChannel&&v.paused
    &&LIBRARY_CHANNEL_CLOCK.now()>=Number(PLAYER.libraryChannel.ends_at_ms||0)){
    libraryChannelBoundary().catch(()=>{}); return;
  }
  endWait(false);
  supersedePlaybackControlIntent(PLAYER);
  const pending=typeof play==='function'&&play.pendingIntent;
  if(pending&&PLAY_OPEN_GATE.current(pending.attempt)){
    pending.wantsPlayback=!pending.wantsPlayback;
    if(PLAYER){PLAYER.wantsPlayback=pending.wantsPlayback;applyPlaybackTransportIntent(v,PLAYER);}
  }else if(v.ended) replayEnded();
  else if(PLAYER){
    rememberPlaybackTransportIntent(v,PLAYER);
    PLAYER.wantsPlayback=!PLAYER.wantsPlayback;
    if(PLAYER.wantsPlayback) resumeHlsStartup(v,PLAYER);
    else pauseHlsStartup(PLAYER);
    applyPlaybackTransportIntent(v,PLAYER);
  }
  if(PLAYER)playerActivity();
  notifyPlaybackControl();
}
// Pointer nudges coalesce after a short quiet. Keyboard arrows use physical
// key ownership below: the first repeat may arrive after this timer, so quiet
// alone cannot mean that a held key was released.
let NUDGE_T=null;
const PLAYER_SEEK_GESTURE=PlaybackPolicy.createHeldKeyCommitter({
  delayMs:PlaybackPolicy.contractTiming("desktop_hotkey_coalesce_ms"),
  commit:owner=>{ if(PLAYER===owner) commitPendingSeek(); }
});
// A skip and a scrub preview are the same pending position with different
// commit rules — the skip commits itself after a quiet, the scrub waits for
// Select. Keeping them in one field is what stops a debounced skip from
// firing over a committed scrub (and from seeking a player that has closed).
function clearPointerSeekTimer(){ clearTimeout(NUDGE_T); NUDGE_T=null; }
function clearPendingSeekTimer(){ clearPointerSeekTimer(); PLAYER_SEEK_GESTURE.cancel(); }
function commitPendingSeek(){
  const me=PLAYER; if(!me) return;
  clearPendingSeekTimer();
  const t=me._seekPending; me._seekPending=null;
  if(t!=null) seekTo(t); else pbTick();
}
function cancelPendingSeek(){
  const me=PLAYER; if(!me) return;
  clearPendingSeekTimer();
  me._seekPending=null; me._seekPreview=null;
  pbTick();
}
function nudge(d){
  const me=PLAYER; if(!me) return;
  if(me.libraryChannel){ toast("Resume live or Watch from start to change position."); return; }
  // A pointer press takes ownership of the visible pending target. A later
  // keyup from an older keyboard gesture must not commit it a second time.
  PLAYER_SEEK_GESTURE.cancel();
  const total=pbTotalSec();
  const base=pbRelativeSeekBase();
  let t=Math.max(0,base+d); if(total>0) t=Math.min(total,t);
  me._seekPending=t; pbTick(); playerActivity();
  clearPointerSeekTimer();
  NUDGE_T=setTimeout(()=>{ if(PLAYER!==me) return; commitPendingSeek(); },
    PlaybackPolicy.contractTiming("desktop_hotkey_coalesce_ms"));
}
function nudgeKeyboard(d,key){
  const me=PLAYER; if(!me) return;
  if(me.libraryChannel){ toast("Resume live or Watch from start to change position."); return; }
  clearPointerSeekTimer();
  PLAYER_SEEK_GESTURE.press(key,me);
  const total=pbTotalSec();
  const base=pbRelativeSeekBase();
  let t=Math.max(0,base+d); if(total>0) t=Math.min(total,t);
  me._seekPending=t; pbTick(); playerActivity();
}
// §3.3 row 17's PiP failure. The browser refuses picture-in-picture for its own
// reasons — no user gesture left, a disabled policy, an element it will not
// detach — and this used to swallow the sentence it gives. Nothing about the
// playback changed, so it is a notice beside the picture and never over it.
async function togglePip(){
  const v=document.getElementById("video");
  try{ if(document.pictureInPictureElement) await document.exitPictureInPicture();
       else if(v) await v.requestPictureInPicture(); }
  catch(e){ raisePlaybackSurface("degraded_notice",
    {title:"Picture-in-picture did not start.",detail:String(e&&e.message||e)}); }
}
// Is the player fullscreen by any of the three routes into it? The document
// API doesn't know about the iOS one: `webkitEnterFullscreen` on the video
// element opens a separate mode that `document.fullscreenElement` never
// reports, so asking only the document would answer "no" while the screen is
// plainly full of video.
function isFullscreenAnywhere(){
  const v=document.getElementById("video");
  return !!(document.fullscreenElement||document.webkitFullscreenElement||(v&&v.webkitDisplayingFullscreen));
}
// Hand the screen back. Fullscreen belongs to the browser rather than to the
// modal, so hiding `#modal` does not release it — and each of the three ways in
// has its own way out.
function exitFullscreenAnywhere(){
  const v=document.getElementById("video");
  try{
    if(document.fullscreenElement&&document.exitFullscreen){ const r=document.exitFullscreen(); if(r&&r.catch) r.catch(()=>{}); }
    else if(document.webkitFullscreenElement&&document.webkitExitFullscreen) document.webkitExitFullscreen();
  }catch(e){}
  try{ if(v&&v.webkitDisplayingFullscreen&&v.webkitExitFullscreen) v.webkitExitFullscreen(); }catch(e){}
}
// Everything the player can still be occupying once its own UI is gone.
// Closing has to undo these explicitly: a picture-in-picture window is the
// browser's, not the page's, and outlives having its media emptied, so it would
// otherwise sit there playing nothing.
function exitPresentationModes(){
  const v=document.getElementById("video");
  exitFullscreenAnywhere();
  try{
    if(document.pictureInPictureElement&&document.exitPictureInPicture){ const r=document.exitPictureInPicture(); if(r&&r.catch) r.catch(()=>{}); }
    else if(v&&v.webkitPresentationMode==='picture-in-picture'&&v.webkitSetPresentationMode) v.webkitSetPresentationMode('inline');
  }catch(e){}
}
function toggleFullscreen(){ const p=document.getElementById("player"), v=document.getElementById("video");
  try{
    if(isFullscreenAnywhere()){ if(WATCH)WATCH.fullOpener="pbfs"; exitFullscreenAnywhere(); return; }
    watchBeforeFullscreen();
    if(p&&p.requestFullscreen){const request=p.requestFullscreen();if(request?.catch)request.catch(()=>watchFullscreenChanged());}
    else if(p&&p.webkitRequestFullscreen) p.webkitRequestFullscreen();
    else if(v&&v.webkitEnterFullscreen){v.addEventListener("webkitbeginfullscreen",watchFullscreenChanged,{once:true});v.addEventListener("webkitendfullscreen",watchFullscreenChanged,{once:true});v.webkitEnterFullscreen();}   // iOS iPhone: video-only fullscreen
  }catch(e){}
}
// Drag the scrubber (pointer events cover mouse + touch). Dragging previews the
// time without touching the stream; the actual seek fires on release.
function pbWireSeek(){
  const seek=document.getElementById("pseek"); if(!seek||seek._wired) return; seek._wired=true;
  const frac=e=>{ const r=seek.getBoundingClientRect(); return r.width?Math.min(1,Math.max(0,((e.clientX||0)-r.left)/r.width)):0; };
  let drag=false;
  seek.addEventListener("pointerdown",e=>{ drag=true; try{seek.setPointerCapture(e.pointerId);}catch(_){}
    // A finger on the bar replaces whatever the keyboard had pending; leaving
    // it behind froze the clock at a preview nobody could see any more.
    clearPendingSeekTimer(); PLAYER._seekPending=null; PLAYER._seekDragging=true;
    PLAYER._seekPreview=frac(e)*pbTotalSec(); pbTick(); playerActivity(); });
  seek.addEventListener("pointermove",e=>{ if(!drag)return; PLAYER._seekPreview=frac(e)*pbTotalSec(); pbTick(); playerActivity(); });
  // With no trustworthy total (see pbTotalSec) a fraction means nothing — drop
  // the drag rather than seeking to a number we made up.
  const done=e=>{ if(!drag)return; drag=false; const tot=pbTotalSec(); const t=frac(e)*tot;
    clearPendingSeekTimer();
    PLAYER._seekDragging=false; PLAYER._seekPreview=null; PLAYER._seekPending=null;
    if(tot>0) seekTo(t); else pbTick(); };
  seek.addEventListener("pointerup",done);
  seek.addEventListener("pointercancel",()=>{ if(!drag)return; drag=false; PLAYER._seekDragging=false; cancelPendingSeek(); });
  seek.addEventListener("blur",()=>{ if(!PLAYER||PLAYER._seekDragging)return; cancelPendingSeek(); });
}
function playbackWaitNeedsProgress(p){
  return !!(p&&(p.pendingMediaChange||hasPendingPlaybackOpen(p)||
    p.controlSeek||p.recoveringStall||(p.progressWatch?.fired&&p.waitAt!=null)));
}
function handlePlaybackPlaying(v,p){
  if(!p||PLAYER!==p) return;
  if(p.wantsPlayback===false){ pausePlaybackInternally(v); return; }
  if(!playbackOwnsAttachedMedia(p)) return;
  const first=!p.started, needsProgress=playbackWaitNeedsProgress(p);
  p.started=true;
  // A decoder may emit playing without producing another frame or clock
  // advance. Only the independent progress monitor can retire its wait.
  if(!needsProgress&&!hlsStartupIncomplete(p)){
    endWait(true);
    p.controlPositionHintSec=null; p.controlRenderOverride=null; p.controlObservationOverride=null;
    clearStall(); finishStallRecovery("recovered");
    // Inert by contract (§3.2). `playing` prompts a look; the progress tick's
    // own evidence is what retires whatever is on screen.
    playbackSurfaceStep({event:"playing"});
  }else playbackSurfaceStep({event:"playing"});
  playerActivity(); settlePlaybackControlSeek(v,p); pbTick(); pbSyncPlayIcon(); notifyPlaybackControl();
  if(first&&!hlsStartupIncomplete(p)) reportTtff();
}
// Attach the persistent listeners once; they read live PLAYER state each fire.
function wirePlayer(){
  if(PLAYER_WIRED) return; PLAYER_WIRED=true;
  const p=document.getElementById("player"), v=document.getElementById("video");
  if(!p||!v) return;
  // Deliberately no "keydown": a second listener that reveals the chrome is a
  // second answer to what a key does, and it made every `ignore` row in the
  // routing table have an effect anyway. The adapter reveals where the
  // contract says to.
  ["mousemove","pointerdown","touchstart"].forEach(ev=>window.addEventListener(ev,()=>{
    const modal=document.getElementById("modal"); if(modal&&modal.classList.contains("open")) playerActivity();
  }));
  // Page-level, and therefore bound once here rather than per element: they
  // read whichever element currently owns the picture.
  document.addEventListener("visibilitychange",()=>{
    const current=document.getElementById("video");
    if(current) samplePlaybackPresentationClock(current,PLAYER);
    // A hidden page samples nothing and expires nothing: the presenter carries
    // the interval forward rather than letting wall time run under a frozen
    // surface (contract §3.4).
    let hidden=false;
    try{ hidden=document.hidden; }catch(e){}
    playbackSurfaceStep({hidden});
  });
  document.addEventListener("visibilitychange",notifyPlaybackControl);
  pbWireSeek();
  if(document.pictureInPictureEnabled){ const pip=document.getElementById("pbpip"); if(pip) pip.style.display=""; }
  wirePlayerMedia(v);
}
// Everything bound to the media element ITSELF, keyed to that element.
//
// This used to live inside `wirePlayer`, which runs once per page and captured
// one element forever. That was correct while the page had exactly one
// `<video>` for its whole life. A prepared handoff replaces which element owns
// the picture, and a listener set that stayed on the retired node would leave
// the scrubber frozen, the stall watchdog deaf, the decode-error rescue silent
// and the end of the film unhandled — every one of them a symptom nobody would
// trace back to a quality change. So the set moves with the element, and the
// guard is on the element rather than on a page-level flag.
function wirePlayerMedia(v){
  if(!v||v._plurxMediaWired) return;
  v._plurxMediaWired=true;
  const p=document.getElementById("player");
  if(!p) return;
  v.addEventListener("playing",()=>handlePlaybackPlaying(v,PLAYER));
  // Inert by contract (§3.2): `canplay` says the element COULD start, not that
  // a frame reached the screen. Feeding it as evidence is the mutation
  // tests/playback/web-policy.test.js pins.
  v.addEventListener("canplay",()=>{ playbackSurfaceStep({event:"canplay"}); });
  v.addEventListener("loadedmetadata",()=>{ if(PLAYER) PLAYER.internalMediaReset=false; });
  // Only a stream that HAS started can "buffer". Before that, the loading
  // overlay owns the message (and a watchdog owns the deadline), so a stalled
  // start or seek reports what actually went wrong instead of showing a
  // spinner that never resolves.
  //
  // The wait is only NOTED here. Whether it was a stall — and which kind — is
  // decided when playback resumes, because that is the first moment its
  // duration is known. See beginWait/endWait.
  v.addEventListener("waiting",()=>{
    if(!playbackOwnsAttachedMedia(PLAYER)) return;
    const action=PlaybackPolicy.waitingOverlayAction({
      started:PLAYER.started, promptUp:playbackSurfacePromptUp()
    });
    if(action!=='buffer') return;
    beginWait(v);
    const copy=playbackWaitCopy(bufferRunway(v),PLAYER.health&&PLAYER.health.http_wait_count);
    // Raised the moment the wait begins and DRAWN only once it has lasted
    // `buffering_min_ms`: Safari fires `waiting` at every fMP4 boundary on
    // healthy 4K, and the undebounced repaint was §2.1's fifth defect.
    raisePlaybackSurface("media_waiting",{context:"attached",
      title:copy.title,detail:copy.detail},{once:true});
    notifyPlaybackControl();
  });
  // Pausing during a wait means the viewer stopped it, not the stream.
  const samplePresentation=()=>samplePlaybackPresentationClock(v,PLAYER);
  for(const event of ["pause","play","seeking","seeked","ratechange","ended"])
    v.addEventListener(event,samplePresentation);
  v.addEventListener("pause",()=>{
    p.classList.remove("idle");
    handlePlaybackTransportEvent(v,PLAYER,"pause");
    notifyPlaybackControl();
  });
  v.addEventListener("play",()=>{
    handlePlaybackTransportEvent(v,PLAYER,"play");
    notifyPlaybackControl();
  });
  v.addEventListener("seeking",()=>{ notifyPlaybackControl(); });
  v.addEventListener("seeked",notifyPlaybackControl);
  v.addEventListener("ratechange",notifyPlaybackControl);
  v.addEventListener("ended",notifyPlaybackControl);
  // A media error never reaches the console on its own — surface it, always.
  // And a remux/direct stream the browser rejects gets one automatic rescue:
  // restart as a transcode session (the server re-encodes to plain H.264/AAC).
  v.addEventListener("error",()=>{
    if(!playbackOwnsAttachedMedia(PLAYER)) return;
    // Detaching the source on purpose (the reset a remux seek does before
    // handing over the next stream) can raise an error event with no source
    // attached. That is us, not a broken file — never rescue-transcode on it.
    if(!v.getAttribute("src")&&!v.currentSrc) return;
    const controlTrigger=notifyPlaybackControl("failed");
    clearStall();
    const err=v.error, code=err?err.code:0, msg=(err&&err.message)||"";
    const src=(v.currentSrc||"").split("?")[0];
    console.warn("[cinema] video error",{code,msg,method:PLAYER&&PLAYER.method,src});
    if(finishStallRecovery("failed",msg||"video element error "+code)){
      showStallRecoveryFailure(msg||"The browser reported video error "+code+".");
      return;
    }
    // `playbackIsReal()` and not `PLAYER.started`: the guard means "we already
    // got real playback going, don't churn", and audio alone used to satisfy
    // it — which disabled this rescue in precisely the black-picture-with-
    // sound case it exists for.
    // Real playback is already going; there is nothing to report and nothing
    // to clear — the tick's own evidence retires whatever was up.
    if(!PLAYER || playbackIsReal()) return;
    const names={1:"aborted",2:"network error",3:"the browser's decoder failed",4:"format not supported by this browser"};
    const fallback=PLAYER && PlaybackPolicy.fallbackAction({
      method:PLAYER.method,
      alreadyTried:PLAYER.triedFallback,
      playbackIsReal:playbackIsReal(),
      // A failed transfer or caller abort does not identify a decoder limit.
      // Match hls.js's media/network distinction using the native error code.
      mediaFailure:code===3||code===4
    });
    if(fallback==='transcode'){
      PLAYER.triedFallback=true;
      const rejection=streamRejectionFacts();
      // Only decode/unsupported-format errors reach this compatible rescue;
      // keep that attribution in the report as well as the action.
      const causeIsDecode=(code===3||code===4);
      const cause="code "+code+": "+(names[code]||("error "+code));
      const note=streamRejectionNote(rejection,PLAYER.method,cause+(msg?": "+msg:""),causeIsDecode);
      // The join rides in `streamRejectionReport`. Without `session` the server
      // cannot tie this report to the session it superseded, and this path had
      // never sent one.
      clientLog(streamRejectionReport(rejection,{code,src,
        message:streamRejectionMessage(rejection,PLAYER.method,cause,causeIsDecode),
        control_trigger:controlTrigger}));
      raisePlaybackSurface("owner_recovery_step",{
        title:"Stream rejected — switching to transcode…",detail:note});
      startTranscodeFallback("stream-rejected",note);
      return;
    }
    clientLog({level:"error",event:"playback_failed",code,src,
      message:(names[code]||("error "+code))+(msg?" ("+msg+")":""),
      control_trigger:controlTrigger});
    // The element failed and the rescue ladder has nothing left. Contract
    // §3.4: stop the player, then raise.
    stopPlayerForExhaustion();
    raisePlaybackSurface("owner_stopped",{player_stopped:true,
      title:"Playback failed: "+(names[code]||("error "+code))+".",
      detail:(msg?msg+" — ":"")+"see Settings → Logs for the server side"});
    toast("Playback failed — "+(names[code]||("error "+code)));
  });
  v.addEventListener("timeupdate",()=>{ settlePlaybackControlSeek(v,PLAYER); checkMarkers(); });
  // Click the picture to play/pause and double-click for fullscreen — the
  // behaviour the native controls used to give us, gone now that we draw our
  // own transport. (Two clicks toggle play twice, so a double-click only
  // changes the fullscreen state.) On touch, a tap that wakes hidden chrome
  // just wakes it: reaching for the controls shouldn't pause the film.
  let ptrKind="mouse", ptrWoke=false;
  v.addEventListener("pointerdown",e=>{ ptrKind=e.pointerType||"mouse"; ptrWoke=p.classList.contains("idle"); });
  v.addEventListener("click",()=>{
    if(PLAYER&&PLAYER._menuDismissedByPointer){ PLAYER._menuDismissedByPointer=false; return; }
    if(ptrKind==="touch"&&ptrWoke){ playerActivity(); return; }
    applyPlayerOutcome(watchRouteInput(playerInputState(),"tap_surface"),{direction:"tap_surface",repeatCount:0});
  });
  v.addEventListener("dblclick",e=>{ e.preventDefault(); toggleFullscreen(); });
  // Custom transport: keep the scrubber, times, buffered bar, and play icon in
  // sync. The seek drag handler and the PiP button belong to the chrome, not to
  // the element, and are wired once in `wirePlayer`.
  ["timeupdate","progress","durationchange","loadedmetadata"].forEach(ev=>v.addEventListener(ev,pbTick));
  ["play","pause","playing","ended","emptied"].forEach(ev=>v.addEventListener(ev,pbSyncPlayIcon));
}
function markerNowMs(){ const v=document.getElementById("video"); return ((PLAYER.offset||0)+(v?v.currentTime||0:0))*1000; }
function markerIsEstimated(m){
  if(m&&m.provenance!=null) return m.provenance==="estimated";
  return !!m&&m.chapter===false;
}
function markerAutoSkipEligible(m){
  if(!m) return false;
  // A preview is never automatic. The preference this gates is spelled
  // "Auto-skip intro & credits" on every surface that offers it, and a
  // preview is the one marker kind that is new footage every week: skipping
  // it is a choice about *this* episode's ending, not a standing preference
  // about repeated material. The button still appears, so a viewer who wants
  // it gone presses it — what is withheld is the seek nobody asked for.
  if(m.kind==="preview") return false;
  // M1-M4 has no configured detector confidence floor. Keep automatic seeks
  // to exact authored/manual evidence; old servers fall back to `chapter`.
  if(m.provenance!=null) return m.provenance==="authored"||m.provenance==="manual";
  return m.chapter!==false;
}
function playbackMarkersUsable(p){
  if(p&&p.libraryChannel)return false;
  return playbackOwnsAttachedMedia(p)&&!p.controlSeek;
}
function checkMarkers(){
  if(!playbackMarkersUsable(PLAYER)){renderSkip(null);return;}
  if(!PLAYER.markers||!PLAYER.markers.length){renderSkip(null);return;}
  const pos=markerNowMs();
  const m=PLAYER.markers.find(mk=> pos>=mk.start_ms && pos < mk.end_ms-400);
  renderSkip(m||null);
  if(m && PLAYER.autoskip && markerAutoSkipEligible(m) && !m._auto){ m._auto=true; skipMarker(m,true); }
}
function renderSkip(m){
  const c=document.getElementById("pskip"); if(!c) return;
  if(!playbackMarkersUsable(PLAYER))m=null;
  if(!m){ if(c.dataset.k){ c.innerHTML=""; c.dataset.k=""; } return; }
  const estimated=markerIsEstimated(m);
  const key=m.kind+m.start_ms+(estimated?"e":"");
  if(c.dataset.k===key) return; // avoid rebuilding every timeupdate tick
  c.dataset.k=key;
  // The server sends the label; the fallback is for a marker from a node too
  // old to send one, where only intro and credits existed.
  const label=m.label||(m.kind==="intro"?"Skip Intro":m.kind==="preview"?"Skip Preview":"Skip Credits");
  c.innerHTML=`<button onclick="skipCurrent()">${esc(label)}${estimated?" · Estimated":""} ›</button>`;
  const offerKey=(m.kind||"unknown")+":"+m.start_ms;
  const automatic=PLAYER.autoskip&&markerAutoSkipEligible(m);
  if(!automatic&&!PLAYER._markerOffers.has(offerKey)){
    PLAYER._markerOffers.add(offerKey);
    clientLog({level:"info",event:"marker_offer",detail:m.kind||"unknown",
      message:"playback marker offered"});
  }
}
function skipCurrent(){
  if(!playbackMarkersUsable(PLAYER)){renderSkip(null);return;}
  const pos=markerNowMs();
  const m=(PLAYER.markers||[]).find(mk=> pos>=mk.start_ms && pos<mk.end_ms);
  if(m) skipMarker(m,false);
}
function skipMarker(m,automatic=false){
  const c=document.getElementById("pskip"); if(c){ c.innerHTML=""; c.dataset.k=""; }
  if(!playbackMarkersUsable(PLAYER))return;
  clientLog({level:"info",event:automatic?"marker_automatic_skip":"marker_manual_skip",
    detail:m.kind||"unknown",message:"playback marker skipped"});
  // A tail marker that runs to the very end → treat "skip" as "finished".
  // Keyed on where the region ENDS rather than on its kind: an episode whose
  // last chapter is "Next Episode Preview" is just as over as one that ends in
  // credits, and before the preview kind existed that file reached here as
  // credits and finished correctly. Kind-keying it now would quietly stop
  // marking those episodes watched.
  // Listed rather than excluded: `!== "intro"` also caught `recap`, and any
  // marker whose kind is absent or unrecognised by this build. Marking an
  // episode watched and advancing to the next one is not a thing to do on a
  // kind this build does not know.
  if((m.kind==="credits"||m.kind==="preview") && PLAYER.durMs && m.end_ms>=PLAYER.durMs-2500){
    reportProgress(PLAYER.fileId,true); return finishPlayback(true);
  }
  clientLog({level:"info",event:"marker_prewarm",detail:"miss",
    message:"skip destination was not prewarmed"});
  PLAYER._lastMarkerSkipEndMs=m.end_ms;
  seekTo(m.end_ms/1000);
}
function playbackSeekBufferedRangesMs(v,p){
  const ranges=[];
  const offsetMs=(Number(p&&p.offset)||0)*1000;
  try{
    for(let index=0;v&&v.buffered&&index<v.buffered.length;index+=1){
      const from=offsetMs+v.buffered.start(index)*1000;
      const through=offsetMs+v.buffered.end(index)*1000;
      if(Number.isFinite(from)&&Number.isFinite(through)&&through>=from)
        ranges.push({from,through});
    }
  }catch(e){}
  return ranges;
}
function playbackSeekPublishedRangeMs(p){
  if(!p||!p.hls||!p.hls.levels) return null;
  const level=p.hls.levels[p.hls.currentLevel];
  const details=level&&level.details, fragments=details&&details.fragments;
  if(!details||!fragments||!fragments.length) return null;
  const offsetMs=(Number(p.offset)||0)*1000;
  const from=offsetMs+Number(fragments[0].start)*1000;
  const through=offsetMs+Number(details.edge)*1000;
  const targetdurationMs=Math.max(0,Number(details.targetduration)||0)*1000;
  if(!Number.isFinite(from)||!Number.isFinite(through)||through<from) return null;
  return {range:{from,through},holdbackMs:targetdurationMs};
}
function playbackSeekBufferCovers(v,p,targetMs){
  return playbackSeekBufferedRangesMs(v,p).some(range=>
    targetMs>=range.from&&targetMs<=range.through);
}
// Seek that works for every method: direct/VOD and safe rolling/progressive
// destinations seek the attached element; everything else reopens at film time.
async function seekTo(targetSec, forceReopen=false, autoHeightOverride=null, viewerInitiated=true,
  recoveryEpisode=null){
  const v=document.getElementById("video"); if(!v||!PLAYER) return;
  targetSec=Math.max(0,targetSec);
  const markerEnd=Number(PLAYER._lastMarkerSkipEndMs)||0;
  if(markerEnd && targetSec*1000<markerEnd-1000 && markerNowMs()>=markerEnd-1000){
    clientLog({level:"info",event:"marker_seek_back",detail:"undo",
      message:"viewer sought behind the last marker destination"});
    PLAYER._lastMarkerSkipEndMs=0;
  }
  // Never aim past the end: a stream started beyond the last frame produces no
  // packets, so it would "buffer" forever with nothing wrong on either side.
  const total=pbTotalSec();
  if(total>0) targetSec=Math.min(targetSec, Math.max(0,total-2));
  // The scrubber is global for multipart audiobooks. Crossing a part boundary
  // changes files through the same play() path; staying in this part converts
  // the global target back to the local timeline the media element owns.
  if(PLAYER.bookParts&&PLAYER.bookParts.length){
    let part=PLAYER.bookParts[0];
    for(const p of PLAYER.bookParts){
      const off=(p.part_offset_ms||0)/1000, end=off+(p.duration_ms||0)/1000;
      if(targetSec>=off && (!end||targetSec<end)){ part=p; break; }
      if(targetSec>=off) part=p;
    }
    const local=Math.max(0,targetSec-(part.part_offset_ms||0)/1000);
    if(part.id!==PLAYER.fileId){
      const m=Object.assign({},PLAYER.meta||{},{part_offset_ms:part.part_offset_ms||0});
      return play(part.id,PLAYER.title,Math.round(local*1000),part.duration_ms||0,m);
    }
    targetSec=local;
  }
  // An identical request whose create is still open is the same command, not
  // a new one. Checked here rather than at the top of the function because
  // the target has only just finished being clamped to the runtime and
  // converted out of the audiobook timeline -- the in-flight key was built
  // from the clamped number, so comparing the raw argument would miss every
  // duplicate in the last two seconds of a title. `forceReopen` is the
  // explicit Try again / stall-restart path and is always a fresh attempt.
  // Built exactly as `requestPlaybackMediaChange` will build it, merge
  // included: a change inherits the unspecified fields of the one it
  // supersedes, so a probe that ignored the merge would compare a different
  // recipe than the one in flight and never match.
  if(!forceReopen&&playbackChangeAlreadyInFlight(PLAYER,targetSec,
      Object.assign({},PLAYER.pendingMediaChange||{},{
        method:PLAYER.method,copyHls:!!PLAYER.copyHls,reason:"seek",
        height:autoHeightOverride,forceReopen:false,
        previousSessionId:PLAYER.sessionId,
        recoveryCause:recoveryEpisode&&recoveryEpisode.kind==="supply"&&autoHeightOverride>0
          ?"network":"unknown"}))){
    playerActivity();
    return;
  }
  // Publish the destination while the old media and reporter still exist.
  // Everything below can detach a source, destroy hls.js, or replace the
  // server session; none of those operations is allowed to erase the intent.
  const seekIntent=beginPlaybackControlSeek(PLAYER,targetSec,viewerInitiated);
  endWait(false);
  if(restartPendingPlaybackOpen(PLAYER,forceReopen?"stall-restart":"seek")) return;
  // One execution for a scrub burst, including native/VOD seeks. The earlier
  // implementation coalesced only UI nudges, so two independent inputs still
  // issued two media mutations and let their completion events race.
  await new Promise(done=>setTimeout(done,100));
  if(!PLAYER||PLAYER.controlSeek!==seekIntent||hasPendingPlaybackOpen(PLAYER)) return;
  const me=PLAYER;
  const bufferedMs=playbackSeekBufferedRangesMs(v,me);
  const published=playbackSeekPublishedRangeMs(me);
  const route=PlaybackPolicy.seekRoute({
    method:me.method,copyHls:!!me.copyHls,vod:!!me.vod,forceReopen,
    changing:!!me.pendingMediaChange,targetMs:targetSec*1000,bufferedMs,
    publishedMs:published&&published.range,
    holdbackMs:published&&published.holdbackMs,
  });
  if(route.route==='local'){
    const attachment=me.mediaAttachment, atMs=route.atMs;
    let settled=false, timer=null;
    const current=()=>PLAYER===me&&me.mediaAttachment===attachment&&me.controlSeek===seekIntent;
    const cleanup=()=>{
      if(timer!=null) clearTimeout(timer);
      try{v.removeEventListener('seeked',onSeeked);}catch(e){}
    };
    const onSeeked=()=>{ if(!current()) return; settled=true; cleanup(); };
    try{v.addEventListener('seeked',onSeeked,{once:true});}catch(e){}
    try{ v.currentTime=Math.max(0,atMs/1000-(me.offset||0)); }catch(e){}
    markPlaybackControlSeekExecuted(me,targetSec);
    clientLog({level:'info',event:'seek_local',
      detail:`${me.copyHls?'copy_hls':me.method||'unknown'}:${route.basis}`,
      message:'seek stayed on the attached media'});
    armStall(targetSec,PlaybackPolicy.HLS_STARTUP.seek_deadline_ms);
    playerActivity();
    if(route.basis==='direct'||route.basis==='vod') { cleanup(); return; }
    timer=setTimeout(()=>{
      if(settled||!current()) { cleanup(); return; }
      if(playbackSeekBufferCovers(v,me,atMs)) { cleanup(); return; }
      cleanup();
      clientLog({level:'warn',event:'seek_local_fallback',
        detail:me.copyHls?'copy_hls':me.method||'unknown',
        message:'local seek did not settle; reopening at the same target'});
      Promise.resolve(seekTo(
        targetSec,true,autoHeightOverride,viewerInitiated,recoveryEpisode
      )).catch(()=>{});
    },PlaybackPolicy.SEEK_LOCAL_SETTLE_MS);
    return;
  }
  // A retry must reconnect, not repeat the local seek that ordinary direct and
  // cached-VOD navigation uses. The old Try again button did exactly that: it
  // assigned the already-stalled currentTime to itself and changed no request.
  if(forceReopen && PLAYER.method==='direct_play'){
    const {me,live}=streamGeneration();
    newAttempt("stall-restart");
    me.started=false; me.offset=0;
    const url=me.directUrl;
    if(!url){
      if(finishStallRecovery("failed","direct-play URL is unavailable"))
        showStallRecoveryFailure("The direct-play URL is unavailable. Your place is saved.");
      return;
    }
    resetMediaSource(v);
    if(!live()) return;
    const attachment=beginPlaybackMediaAttachment(me);
    v.onloadedmetadata=()=>{
      if(!attachment.current()) return;
      v.onloadedmetadata=null;
      applyPlaybackAttachmentPosition(v,me,attachment,targetSec); };
    setPlaybackMediaSource(v,url); markPlaybackControlSeekExecuted(me,targetSec);
    armStall(targetSec,20000); applyPlaybackTransportIntent(v,me);
    return;
  }
  // Every non-direct seek restarts the server stream, so overlapping seeks (a
  // double-clicked ±10, or a drag landing near a keyboard nudge) would tear down
  // and re-attach hls.js twice. Token the call and bail any superseded one after
  // its await, so only the last seek attaches.
  return requestPlaybackMediaChange(PLAYER,{method:PLAYER.method,copyHls:!!PLAYER.copyHls,
    reason:forceReopen?"stall-restart":"seek",height:autoHeightOverride,
    forceReopen,previousSessionId:PLAYER.sessionId,
    // Only an independently selected lower rung turns supply pressure into a
    // capacity verdict. MEDIA_ERR_NETWORK alone is a failed transfer and keeps
    // this create on the same recipe without the legacy rung-lowering ticket.
    recoveryCause:recoveryEpisode&&recoveryEpisode.kind==="supply"&&autoHeightOverride>0
      ?"network":"unknown"});
}
function autoskipOn(){ try{ return localStorage.getItem("plurx_autoskip")==="1"; }catch(e){ return false; } }
function setAutoskip(on){ try{ localStorage.setItem("plurx_autoskip", on?"1":"0"); }catch(e){}
  if(PLAYER) PLAYER.autoskip=on;
  const b=document.getElementById("skipbtn"); if(b){ b.classList.toggle("on", on); b.title=(on?"Auto-skip on — ":"Auto-skip off — ")+"intro & credits"; } }
function togglePlayerAutoskip(){ setAutoskip(!autoskipOn()); toast(autoskipOn()?"Auto-skip on":"Auto-skip off"); }
