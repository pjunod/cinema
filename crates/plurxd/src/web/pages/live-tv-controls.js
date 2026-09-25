"use strict";
// Playback feedback belongs to the persistent video host, including while a
// start is pending or the viewer has navigated to another page.
function liveTvPlaybackState(state,message,actions={}){
  LIVE_TV.playbackState=state;
  if(message) liveTvMessage(message);
  const surface=document.getElementById("live-tv-status");
  if(!surface) return;
  surface.dataset.state=state;
  surface.hidden=state==="idle"||state==="playing";
  const text=document.getElementById("live-tv-status-text");
  if(text) text.textContent=message||"";
  const play=document.getElementById("live-tv-status-play");
  if(play) play.hidden=state!=="blocked"&&state!=="paused";
  const retry=document.getElementById("live-tv-status-retry");
  if(retry) retry.hidden=state!=="error"||actions.retryable===false;
  const offers=document.getElementById("live-tv-status-offers");
  if(offers){
    offers.replaceChildren();
    for(const offer of actions.offers||[]){
      // The lineup may have changed since the owner sent this capacity answer.
      if(!LIVE_TV.channels.some(channel=>channel.id===offer.channelId&&!PlurxLiveTv.channelView(channel).disabled)) continue;
      const button=document.createElement("button");
      button.type="button"; button.textContent=offer.label;
      button.addEventListener("click",()=>liveTvWatchChannel(offer.channelId));
      offers.append(button);
    }
  }
  const host=document.getElementById("live-tv-host");
  if(host&&!surface.hidden) host.hidden=false;
}
function liveTvPlaybackFailure(error){
  const view=PlurxLiveTv.errorView(error);
  liveTvPlaybackState("error",`${view.title}. ${view.detail}`,view);
}
function detachLiveTvMedia(){
  closeLiveTvStats(false);
  clearInterval(LIVE_TV.timer); LIVE_TV.timer=null;
  LIVE_TV.status=null;
  if(LIVE_TV.hls){ LIVE_TV.hls.destroy(); LIVE_TV.hls=null; }
  const video=document.getElementById("live-tv-video");
  if(video){
    video.onerror=null; video.onended=null; video.onplaying=null; video.onwaiting=null; video.onpause=null; video.pause();
    for(const {track} of liveTvCaptionTracks()) track.mode="disabled";
    video.removeAttribute("src"); video.load();
  }
  liveTvRefreshCaptionControls();
}
async function stopLiveTv(){
  // Stop owns the tuner immediately. A keyup delivered after this point must
  // not commit a preview that was accumulated before Stop and reacquire it.
  if(typeof cancelLiveTvChannelGesture==="function") cancelLiveTvChannelGesture();
  else { LIVE_TV.pendingChannel=null; LIVE_TV.preview=null; }
  const serial=++LIVE_TV.serial;
  LIVE_TV.starting=null; LIVE_TV.tuningChannel=null;
  liveTvPlaybackState("idle","");
  const host=document.getElementById("live-tv-host");
  // `document.exitFullscreen()` resolves before Chromium dispatches its
  // fullscreenchange event. Move out of `full` first so that late event cannot
  // interpret this stop as an ordinary Exit and reveal the host again.
  if(host&&host.dataset&&host.dataset.mode==="full") host.dataset.mode=location.hash==="#/live-tv"?"slot":"dock";
  // Releasing the physical tuner must start immediately, but removing the
  // fullscreen element before the browser clears fullscreenElement can strand
  // Chromium in fullscreen indefinitely. Keep those operations concurrent,
  // then dismantle the media tree only after presentation state settles.
  const exiting=exitLiveTvPresentation();
  const stopping=LIVE_TV_LEASE.stop().then(value=>({ok:true,value}),error=>({ok:false,error}));
  await exiting;
  // A channel selected during fullscreen exit owns the media now. An older
  // Stop must not detach it or hide its startup feedback when that exit lands.
  if(serial===LIVE_TV.serial){
    detachLiveTvMedia();
    if(host) host.hidden=true;
  }
  const stopped=await stopping; if(!stopped.ok) throw stopped.error;
  return stopped.value;
}
async function watchLiveTv(index,isCompatibilityRetry=false){
  const channel=LIVE_TV.channels[index];
  if(!channel||PlurxLiveTv.channelView(channel).disabled||location.hash!=="#/live-tv") return;
  // A repeated press joins the visible attempt rather than cancelling its
  // eventual reply and paying for a second release/tune cycle.
  if(!isCompatibilityRetry&&LIVE_TV.tuningChannel===channel.id&&
      (LIVE_TV.starting||(LIVE_TV_LEASE.current&&LIVE_TV.playbackState!=="error"))){
    if(LIVE_TV.playbackState==="blocked"||LIVE_TV.playbackState==="paused") resumeLiveTv();
    return;
  }
  if(!isCompatibilityRetry){ LIVE_TV.compatibilityRetried=false; LIVE_TV.compatibility=null; }
  const serial=++LIVE_TV.serial, generation=PAGE_RENDER_GENERATION;
  // Session ownership survives route changes: the persistent host carries
  // startup feedback, media and the tuner watchdog into the dock.
  const owned=()=>serial===LIVE_TV.serial;
  detachLiveTvMedia();
  if(PLAYER&&PLAYER.fileId) closePlayer();
  LIVE_TV.lastChannel=index; LIVE_TV.selected=channel.id; LIVE_TV.tuningChannel=channel.id;
  LIVE_TV.starting=serial;
  liveTvShowHost();
  liveTvPlaybackState("starting",`Tuning ${channel.guide_number} · ${channel.guide_name}…`);
  try{
    const info=await LIVE_TV_LEASE.start(channel.id);
    // Deliberately `owned`, not `onPage`. Navigating away during the ~45 s
    // start window used to abandon the reply: the lease landed with no media,
    // no poll and no keepalive attached, and the tuner was held with nothing
    // on screen able to release it. A start that completes is always wired up;
    // if the route moved on, it is wired up into the dock.
    if(!info||!owned()) return;
    await liveTvAttachSession(info,index,serial,generation);
  }catch(e){ if(owned()) liveTvPlaybackFailure(e); }
  finally{ if(LIVE_TV.starting===serial) LIVE_TV.starting=null; }
}
// Everything a live capability needs around it — playlist, autoplay, failure
// routing, the keepalive and the tuner watchdog. A session recovered by
// `resume` enters here exactly as a fresh start does; that is what makes
// reopening the page show the picture instead of the list.
async function liveTvAttachSession(info,index,serial,generation){
  const owned=()=>serial===LIVE_TV.serial;
  const onPage=()=>owned()&&generation===PAGE_RENDER_GENERATION&&location.hash==="#/live-tv";
  try{
    const channel=LIVE_TV.channels[index]||null;
    const video=document.getElementById("live-tv-video");
    LIVE_TV.unplayedSince=liveTvNow(); LIVE_TV.lastPosition=0; LIVE_TV.lastFrames=0;
    // Watching is the channel this capability is on, whether the press named
    // it or the owner did when it handed the session back.
    LIVE_TV.selected=(channel&&channel.id)||(info.channel&&info.channel.id)||LIVE_TV.selected;
    if(onPage()) liveTvShowHost(); else liveTvSetMode("dock");
    // Rebuild the same-origin master URL from the capability; its caption
    // declarations must reach both native HLS and hls.js players.
    const playlist=API+`/live-tv/sessions/${encodeURIComponent(info.session_id)}/master.m3u8`;
    LIVE_TV.tuningChannel=LIVE_TV.selected;
    liveTvPlaybackState("buffering","Channel connected. Loading video…");
    let failing=false;
    const play=()=>{
      if(!owned()) return;
      video.play().catch(error=>{
        if(!owned()||failing||error.name==="AbortError") return;
        if(error.name==="NotSupportedError"){
          failed({code:"codec_unsupported"},{failed_video:true,failed_audio:true,failed_container:true});
        }else liveTvPlaybackState("blocked","Ready — press Play live to start audio and video. The tuner is released after 30 seconds without playback.");
      });
    };
    const failed=async (error,compatibility=null,retryFresh=false)=>{
      if(!owned()||failing) return;
      failing=true;
      try{ await LIVE_TV_LEASE.status(); }catch(typed){ error=typed; }
      if(!owned()) return;
      const retry=(compatibility||retryFresh)&&!LIVE_TV.compatibilityRetried&&location.hash==="#/live-tv";
      if(retry){ LIVE_TV.compatibilityRetried=true; LIVE_TV.compatibility=compatibility; }
      try{ await stopLiveTv(); }
      catch(e){
        if(LIVE_TV.serial===serial+1) liveTvPlaybackState("error","Stream stopped. Owner cleanup is unconfirmed; retry Stop before opening another channel.",{retryable:false});
        return;
      }
      if(LIVE_TV.serial!==serial+1) return;
      if(retry&&location.hash==="#/live-tv") await watchLiveTv(index,true);
      else liveTvPlaybackFailure(error);
    };
    video.onplaying=()=>{ if(owned()&&!failing) liveTvPlaybackState("playing","Playing live"); };
    video.onwaiting=()=>{
      if(owned()&&!failing&&LIVE_TV.playbackState!=="blocked"&&LIVE_TV.playbackState!=="paused")
        liveTvPlaybackState("buffering","Buffering live video…");
    };
    video.onpause=()=>{
      if(owned()&&!failing&&video.paused) liveTvPlaybackState("paused","Paused. Press Play live to resume; the tuner is released after 30 seconds without playback.");
    };
    video.onerror=()=>failed({code:video.error&&[3,4].includes(video.error.code)?"codec_unsupported":"stream_failed"},
      video.error&&[3,4].includes(video.error.code)?{failed_video:true,failed_audio:true,failed_container:true}:null);
    video.onended=()=>failed({code:"stream_failed"});
    if(window.Hls&&Hls.isSupported()){
      const hls=new Hls({liveSyncDurationCount:2,liveMaxLatencyDurationCount:4,maxBufferLength:12,maxMaxBufferLength:18,backBufferLength:0});
      LIVE_TV.hls=hls;
      hls.on(Hls.Events.MANIFEST_PARSED,()=>{ liveTvRefreshCaptionControls(); play(); });
      hls.on(Hls.Events.ERROR,(_,data)=>{ if(data.fatal) failed({code:data.type===Hls.ErrorTypes.MEDIA_ERROR?"codec_unsupported":"stream_failed"},
        data.type===Hls.ErrorTypes.MEDIA_ERROR?{failed_video:true,failed_audio:true,failed_container:true}:null); });
      hls.loadSource(playlist); hls.attachMedia(video);
    }else if(video.canPlayType("application/vnd.apple.mpegurl")){
      video.src=playlist; liveTvRefreshCaptionControls(); play();
    }else{ await failed({code:"codec_unsupported"}); return; }
    LIVE_TV.timer=setInterval(async()=>{
      if(!owned()||LIVE_TV.polling) return;
      LIVE_TV.polling=true;
      try{
        // Decoded frames, not position. A live-window slide, a gap jump or a
        // level switch can move currentTime backwards on a healthy stream, and
        // a high-water mark then refuses to renew until the timeline climbs
        // back past its old peak — stopping playback that was never stuck.
        // Position remains the fallback where the quality API is absent.
        const quality=video.getVideoPlaybackQuality?video.getVideoPlaybackQuality():null;
        const frames=quality&&Number.isFinite(quality.totalVideoFrames)?quality.totalVideoFrames:null;
        const position=video.currentTime;
        const advancing=!video.paused&&(frames!==null?frames>LIVE_TV.lastFrames
          :Number.isFinite(position)&&position>LIVE_TV.lastPosition+0.1);
        if(!advancing){
          if(LIVE_TV.unplayedSince===null) LIVE_TV.unplayedSince=liveTvNow();
          if(liveTvNow()-LIVE_TV.unplayedSince>=30000){
            await stopLiveTv();
            if(LIVE_TV.serial===serial+1) liveTvPlaybackState("error","Live TV stopped after 30 seconds without playback. Try again to reconnect.");
          }
          return;
        }
        if(frames!==null) LIVE_TV.lastFrames=frames;
        LIVE_TV.lastPosition=position; LIVE_TV.unplayedSince=liveTvNow();
        if(document.visibilityState!=="hidden"||liveTvInPip()) await LIVE_TV_LEASE.keepalive();
        if(!owned()) return;
        const status=await LIVE_TV_LEASE.status();
        if(!owned()) return;
        if(status&&status.state!=="active") await failed({code:"stream_failed"});
        else if(status){
          if(status.channel&&status.channel.id===LIVE_TV.selected){
            LIVE_TV.channels=LIVE_TV.channels.map(channel=>channel.id===status.channel.id
              ?{...channel,...status.channel,source_format:status.channel.source_format||null}:channel);
          }
          LIVE_TV.status=status;
          if(typeof liveTvRefreshTechnicalDetails==="function") liveTvRefreshTechnicalDetails();
          if(typeof liveTvPaint==="function") liveTvPaint();
        }
      }catch(e){ await failed(e,null,e&&e.code==="source_format_changed"); }
      finally{ LIVE_TV.polling=false; }
    },10000);
  }catch(e){
    if(!owned()) return;
    try{ await stopLiveTv(); }catch(_){}
    if(LIVE_TV.serial===serial+1) liveTvPlaybackFailure(e);
  }
}
// Opening Live TV asks the owner about every start this browser still holds a
// handle for. It rejoins one; it never tunes one. An owner that says the start
// ended or was retired takes the handle away, an owner that says anything else
// — or says nothing — leaves it, and the viewer sees the channel list.
async function liveTvResumeStart(generation,route){
  // Once per document — but only once something was actually decided. A
  // lineup read that failed says nothing about whether this ingress speaks the
  // recovery protocol, and a document that marked itself resumed on that
  // silence would never rejoin a session again, however many times the viewer
  // came back to the page.
  if(LIVE_TV.resumed||!LIVE_TV.lineupRead) return;
  if(!liveTvRecoveryEnabled()){ LIVE_TV.resumed=true; return; }
  // Nothing to rejoin while this document is already driving a session; the
  // next visit asks again, since that answer can change.
  if(LIVE_TV_LEASE.current||LIVE_TV.starting) return;
  LIVE_TV.resumed=true;
  // Only a hint nothing else is holding. A sibling tab that is watching right
  // now answers the probe, and rejoining its session would hand two documents
  // one capability — the first to navigate away would then DELETE the other's
  // picture mid-programme.
  for(const hint of await liveTvOrphanHints()){
    let answer=null;
    try{ answer=await liveTvRequest(`/live-tv/starts/${encodeURIComponent(hint.id)}/resume`,"POST",15000,true); }
    catch(e){
      // The ingress's own typed refusals say this id will never be answered
      // here; everything else keeps the handle and shows the list.
      if(!PlurxLiveTv.startOutcome(liveTvAnswer(e)).keepHint) LIVE_TV_HINTS.forget(hint.id);
      continue;
    }
    const outcome=answer&&typeof answer.outcome==="string"?answer.outcome:"";
    if(outcome==="ended"||outcome==="retired"){ LIVE_TV_HINTS.forget(hint.id); continue; }
    const session=outcome==="live"?answer.session:null;
    if(!session||typeof session.session_id!=="string"||!session.session_id) continue;
    if(generation!==PAGE_RENDER_GENERATION||location.hash!==route||LIVE_TV_LEASE.current||LIVE_TV.starting) return;
    const channelId=(session.channel&&session.channel.id)||session.channel_id||null;
    const index=LIVE_TV.channels.findIndex(channel=>channel&&channel.id===channelId);
    const serial=++LIVE_TV.serial;
    LIVE_TV.hint=hint.id;
    LIVE_TV.starting=serial;
    try{
      const adopted=await LIVE_TV_LEASE.adopt(session);
      if(!adopted||serial!==LIVE_TV.serial) return;
      if(index>=0) LIVE_TV.lastChannel=index;
      await liveTvAttachSession(adopted,index,serial,generation);
    }
    finally{ if(LIVE_TV.starting===serial) LIVE_TV.starting=null; }
    return;
  }
}
function liveTvWatchChannel(id){
  const channel=LIVE_TV.channels.find(channel=>channel.id===id);
  if(!channel||PlurxLiveTv.channelView(channel).disabled) return;
  if(location.hash!=="#/live-tv"){
    LIVE_TV.watchOnArrival=id; location.hash="#/live-tv";
  }else liveTvSelect(id);
}
function retryLiveTv(){
  if(LIVE_TV.selected) liveTvWatchChannel(LIVE_TV.selected);
}
function resumeLiveTv(){
  const video=document.getElementById("live-tv-video"),serial=LIVE_TV.serial;
  if(!video||!LIVE_TV_LEASE.current||LIVE_TV.playbackState==="starting") return;
  if(!video.paused&&LIVE_TV.playbackState==="playing") return;
  liveTvPlaybackState("buffering","Starting live video…");
  // Called directly by a click, preserving the browser's user activation.
  video.play().catch(error=>{
    if(serial===LIVE_TV.serial&&error.name!=="AbortError")
      liveTvPlaybackState("blocked","Playback could not start. Press Play live to try again, or Stop to release the tuner.");
  });
}
function pauseLiveTv(){
  const video=document.getElementById("live-tv-video");
  if(video&&LIVE_TV_LEASE.current){
    video.pause();
    liveTvPlaybackState("paused","Paused. Press Play live to resume; the tuner is released after 30 seconds without playback.");
  }
}
function liveTvSyncMuteButtons(){
  const video=document.getElementById("live-tv-video"); if(!video) return;
  const label=video.muted?"Unmute":"Mute", icon=video.muted?"🔊":"🔇";
  document.querySelectorAll("[data-live-tv-mute]").forEach(button=>{
    button.textContent=icon; button.title=label; button.setAttribute("aria-label",label);
  });
}
function muteLiveTv(){
  const video=document.getElementById("live-tv-video");
  if(video){ video.muted=!video.muted; liveTvSyncMuteButtons(); }
}
function fullscreenLiveTv(){
  const panel=document.getElementById("live-tv-host"), video=document.getElementById("live-tv-video");
  try{
    if(document.fullscreenElement===panel||document.webkitFullscreenElement===panel||(video&&video.webkitDisplayingFullscreen)){ exitLiveTvPresentation(); return; }
    if(panel&&panel.requestFullscreen){ const r=panel.requestFullscreen(); if(r&&r.catch) r.catch(()=>{}); }
    else if(panel&&panel.webkitRequestFullscreen) panel.webkitRequestFullscreen();
    else if(video&&video.webkitEnterFullscreen) video.webkitEnterFullscreen();
  }catch(e){}
}
async function exitLiveTvPresentation(){
  const panel=document.getElementById("live-tv-host"), video=document.getElementById("live-tv-video");
  try{
    const full=document.fullscreenElement||document.webkitFullscreenElement;
    if(panel&&full&&(full===panel||panel.contains(full))){
      const exit=document.exitFullscreen||document.webkitExitFullscreen;
      if(exit){
        // Chromium can clear fullscreenElement while leaving the returned
        // promise pending. Observe the state-change event rather than letting
        // that promise wedge Stop and retain the tuner forever. The timer is a
        // bounded fallback for engines which clear state without an event.
        let settle=()=>{};
        const stateExit=typeof document.addEventListener==="function"
          ?new Promise(resolve=>{
            let done=false;
            settle=()=>{
              if(done) return; done=true;
              if(document.removeEventListener){
                document.removeEventListener("fullscreenchange",settle);
                document.removeEventListener("webkitfullscreenchange",settle);
              }
              resolve();
            };
            document.addEventListener("fullscreenchange",settle);
            document.addEventListener("webkitfullscreenchange",settle);
            setTimeout(settle,1000);
          }):Promise.resolve();
        const result=exit.call(document); if(result&&result.catch) result.catch(()=>{});
        const active=document.fullscreenElement||document.webkitFullscreenElement;
        if(!active||!(active===panel||panel.contains(active))) settle();
        await stateExit;
      }
    }
  }catch(e){}
  try{ if(video&&video.webkitDisplayingFullscreen&&video.webkitExitFullscreen) video.webkitExitFullscreen(); }catch(e){}
  try{ if(video&&document.pictureInPictureElement===video&&document.exitPictureInPicture) await document.exitPictureInPicture(); }catch(e){}
  try{ if(video&&video.webkitPresentationMode==="picture-in-picture"&&video.webkitSetPresentationMode) video.webkitSetPresentationMode("inline"); }catch(e){}
}
window.addEventListener("pagehide",()=>{
  // A reload that lands mid-DELETE must still find a handle for this start.
  if(LIVE_TV_LEASE.current) LIVE_TV_HINTS.touch(LIVE_TV.hint);
  // One DELETE, not two. The release path now carries `keepalive` itself, so
  // the request that is actually delivered is also the one whose result can
  // forget the hint; a duplicate raw fetch only spent a second request that
  // nothing could observe.
  LIVE_TV.unloading=true;
  stopLiveTv().catch(()=>{});
});
// Hints are read from storage at the moment they are used, so no document has
// a cached view of them to keep in step: the `storage` listener the barrier
// needed is gone with it.
document.addEventListener("visibilitychange",()=>{
  if(document.visibilityState==="hidden"&&location.hash==="#/live-tv"&&!liveTvInPip()){
    stopLiveTv().catch(()=>{}); liveTvMessage("Live TV stopped while the page was in the background. Select a channel to resume.");
  }
});
