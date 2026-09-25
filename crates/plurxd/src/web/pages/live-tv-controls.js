"use strict";
function detachLiveTvMedia(){
  closeLiveTvStats(false);
  clearInterval(LIVE_TV.timer); LIVE_TV.timer=null;
  LIVE_TV.status=null;
  if(LIVE_TV.hls){ LIVE_TV.hls.destroy(); LIVE_TV.hls=null; }
  const video=document.getElementById("live-tv-video");
  if(video){
    video.onerror=null; video.onended=null; video.pause();
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
  ++LIVE_TV.serial;
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
  await exiting; detachLiveTvMedia();
  if(host) host.hidden=true;
  const stopped=await stopping; if(!stopped.ok) throw stopped.error;
  return stopped.value;
}
async function watchLiveTv(index,isCompatibilityRetry=false){
  const channel=LIVE_TV.channels[index];
  if(!channel||PlurxLiveTv.channelView(channel).disabled||location.hash!=="#/live-tv") return;
  if(!isCompatibilityRetry){ LIVE_TV.compatibilityRetried=false; LIVE_TV.compatibility=null; }
  const serial=++LIVE_TV.serial, generation=PAGE_RENDER_GENERATION;
  // Two different questions, and conflating them is what stopped the tuner
  // watchdog the moment the picture docked. `owned` asks "is this still the
  // session I started" — the only thing the keepalive, the status poll and the
  // 30-second no-progress release should ever depend on, because they own a
  // physical tuner and the tuner does not care which route is showing.
  // `onPage` additionally asks whether this render is still the one on screen,
  // and gates writes into page DOM that no longer exists.
  const owned=()=>serial===LIVE_TV.serial;
  const onPage=()=>owned()&&generation===PAGE_RENDER_GENERATION&&location.hash==="#/live-tv";
  detachLiveTvMedia();
  if(PLAYER&&PLAYER.fileId) closePlayer();
  LIVE_TV.lastChannel=index; liveTvMessage(`Starting ${channel.guide_number} · ${channel.guide_name}…`);
  LIVE_TV.starting=serial;
  try{
    const info=await LIVE_TV_LEASE.start(channel.id);
    // Deliberately `owned`, not `onPage`. Navigating away during the ~45 s
    // start window used to abandon the reply: the lease landed with no media,
    // no poll and no keepalive attached, and the tuner was held with nothing
    // on screen able to release it. A start that completes is always wired up;
    // if the route moved on, it is wired up into the dock.
    if(!info||!owned()) return;
    await liveTvAttachSession(info,index,serial,generation);
  }catch(e){ if(onPage()) liveTvFailure(e); }
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
    const play=()=>{ if(owned()) video.play().then(()=>{ if(owned()) liveTvMessage("Playing live"); }).catch(()=>{ if(owned()) liveTvMessage("Ready — press Play live within 30 seconds to start audio and video."); }); };
    const failed=async (error,compatibility=null,retryFresh=false)=>{
      if(!owned()) return;
      try{ await LIVE_TV_LEASE.status(); }catch(typed){ error=typed; }
      if(!owned()) return;
      if((compatibility||retryFresh)&&!LIVE_TV.compatibilityRetried){
        LIVE_TV.compatibilityRetried=true; LIVE_TV.compatibility=compatibility;
        await stopLiveTv();
        if(location.hash==="#/live-tv") await watchLiveTv(index,true);
        return;
      }
      liveTvFailure(error);
      stopLiveTv().catch(()=>liveTvMessage("Stream stopped. Owner cleanup is unconfirmed; retry Stop before opening another channel."));
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
            await stopLiveTv(); liveTvMessage("Live TV stopped after 30 seconds without playback. Select a channel to resume.");
          }
          return;
        }
        if(frames!==null) LIVE_TV.lastFrames=frames;
        LIVE_TV.lastPosition=position; LIVE_TV.unplayedSince=liveTvNow();
        if(document.visibilityState!=="hidden"||liveTvInPip()) await LIVE_TV_LEASE.keepalive();
        const status=await LIVE_TV_LEASE.status();
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
  }catch(e){ if(onPage()) liveTvFailure(e); }
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
function resumeLiveTv(){
  const video=document.getElementById("live-tv-video");
  if(video&&LIVE_TV_LEASE.current) video.play().catch(()=>liveTvMessage("Playback could not start. Stop and select the channel again."));
}
function pauseLiveTv(){
  const video=document.getElementById("live-tv-video");
  if(video&&LIVE_TV_LEASE.current){ video.pause(); liveTvMessage("Paused. The tuner is released after 30 seconds without playback; resuming has no rewind guarantee."); }
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
