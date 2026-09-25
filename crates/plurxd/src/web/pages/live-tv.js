"use strict";
// Live TV owns its own media element and capability. It never enters PLAYER,
// file progress, playback-control, watched state, autoskip, or Trakt.
const LIVE_TV={channels:[],serial:0,hls:null,timer:null,guideTimer:null,polling:false,lastChannel:null,hint:null,protocols:null,lineupRead:false,resumed:false,unplayedSince:null,lastPosition:0,starting:null,status:null,compatibility:null,compatibilityRetried:false};
// What the DVR says about the guide on screen: the status (tuner slots), the
// fortnight's schedule and this viewer's reminders. Read once per guide load
// and again after every mutation — never polled, because nothing here changes
// except when somebody presses something or a capture opens, and both of those
// already reach the page.
const LIVE_TV_DVR={status:null,schedule:null,reminders:[],library:null,libraryNext:null,
  libraryError:null,libraryLoading:false,rules:null,serial:0};
function liveTvNow(){
  return performance.now();
}
function liveTvRequestId(){
  // 32 lower-case hex. getRandomValues works on private-LAN HTTP origins;
  // randomUUID does not.
  return Array.from(crypto.getRandomValues(new Uint8Array(16)),b=>b.toString(16).padStart(2,"0")).join("");
}
const LIVE_TV_HINTS=new PlurxLiveTv.StartHints(()=>localStorage,()=>Date.now());
// Liveness decides what to RETIRE, never whether to start. A document that
// holds a live session — or is still starting one — claims its hint when
// another document asks who owns it, and an unclaimed hint is a candidate for
// retirement and nothing more. Web Locks would be cleaner and is unavailable
// on the LAN HTTP origin this page runs on.
const LIVE_TV_TABS=(()=>{ try{ return new BroadcastChannel("plurx-live-tv"); }catch(e){ return null; } })();
if(LIVE_TV_TABS) LIVE_TV_TABS.onmessage=event=>{
  const message=event&&event.data;
  if(!message||typeof message.who!=="string"||message.who!==LIVE_TV.hint) return;
  if(LIVE_TV_LEASE.current||LIVE_TV.starting) LIVE_TV_TABS.postMessage({alive:message.who});
};
function liveTvRecoveryEnabled(){
  // Protocol 3 is the client request id and the /live-tv/starts/* routes. The
  // answer is cached per lineup read, not per process: an ingress upgraded (or
  // rolled back) between two visits is believed the next time this page reads
  // /live-tv/channels.
  return Array.isArray(LIVE_TV.protocols)&&LIVE_TV.protocols.includes(3);
}
function liveTvAnswer(error){
  // What the reducer rules on, from whatever the request threw. A failure that
  // carries no body never arrived as an answer at all.
  return error&&error.answer?error.answer:{transport:"timeout"};
}
async function liveTvRetireHint(id){
  // Forget the hint on a typed 2xx and only then: anything else leaves the
  // handle in place for a later press to try again.
  try{
    const answer=await liveTvRequest(`/live-tv/starts/${encodeURIComponent(id)}`,"DELETE",8000,true);
    if(answer&&typeof answer.outcome==="string") LIVE_TV_HINTS.forget(id);
  }catch(e){ /* a hint that cannot be retired is simply left */ }
}
// Which stored hints nothing alive is holding. One probe answers that for both
// callers: the retire sweep before a press, and the open-time resume. A hint
// another document claims — or one touched inside the keepalive window — is
// still being driven by something, and it is neither this document's to retire
// nor its to rejoin. Resuming a sibling's session would put two documents on
// one capability, and the first of them to navigate away would DELETE the
// picture out from under the other.
//
// This decides what to RETIRE or REJOIN, never whether to start.
async function liveTvOrphanHints(){
  const hints=LIVE_TV_HINTS.list();
  if(!hints.length) return [];
  const alive=new Set();
  if(LIVE_TV_TABS){
    const claimed=event=>{ const message=event&&event.data; if(message&&typeof message.alive==="string") alive.add(message.alive); };
    LIVE_TV_TABS.addEventListener("message",claimed);
    try{
      for(const hint of hints) LIVE_TV_TABS.postMessage({who:hint.id});
      await new Promise(done=>setTimeout(done,PlaybackPolicy.liveContractTiming("retire_liveness_probe_ms")));
    }finally{ LIVE_TV_TABS.removeEventListener("message",claimed); }
  }
  // A hint touched more recently than this many keepalives is held by
  // something alive, so it is left alone. That rule reads the wall clock, so a
  // clock step can make a live tab's hint look orphaned: that tab then sees
  // `capability_expired` and its next press plays — annoying, not unsafe.
  const fresh=PlaybackPolicy.liveContractTiming("retire_orphan_after_keepalives")*5000, now=Date.now();
  return hints.filter(hint=>!alive.has(hint.id)&&now-hint.touchedAt>=fresh);
}
async function liveTvRetireOrphanHints(){
  for(const hint of await liveTvOrphanHints()) liveTvRetireHint(hint.id);
}
// Channels the browser's audio output reaches, between stereo and 5.1 (the
// most the server encodes). A fixed 2 is what folded every 5.1 broadcast to
// stereo; a browser on a multichannel output decodes 5.1 AAC natively.
function liveTvAacChannelCeiling(destinationChannels){
  const channels=Number(destinationChannels);
  return Number.isFinite(channels)?Math.min(6,Math.max(2,Math.floor(channels))):2;
}
let LIVE_TV_AUDIO_CONTEXT=null;
function liveTvOutputChannels(){
  try{
    const Ctx=window.AudioContext||window.webkitAudioContext;
    if(!Ctx) return 2;
    LIVE_TV_AUDIO_CONTEXT=LIVE_TV_AUDIO_CONTEXT||new Ctx();
    return LIVE_TV_AUDIO_CONTEXT.destination.maxChannelCount;
  }catch(e){ return 2; }
}
function liveTvPlaybackEnvelope(compatibility=null){
  const caps=currentCapsDocument();
  const aacChannels=liveTvAacChannelCeiling(liveTvOutputChannels());
  const audio=(caps.audio||[]).filter(codec=>["aac","ac3","eac3"].includes(codec));
  const formats=[{container:"mpegts",video:"h264",audio:"aac"}];
  for(const video of caps.video||[]){
    const container=video.codec==="hevc"?"fmp4":"mpegts";
    for(const codec of audio) formats.push({container,video:video.codec,audio:codec});
  }
  const unique=new Map(formats.map(value=>[`${value.container}|${value.video}|${value.audio}`,value]));
  const video_limits=[];
  for(const video of caps.video||[]){
    const profiles=video.profiles&&video.profiles.length?video.profiles:[null];
    for(const profile of profiles) video_limits.push({codec:video.codec,profile,
      max_width:3840,max_height:video.max_height||2160,max_frame_rate:{num:60,den:1},interlaced:false});
  }
  return {v:1,caps,hls_formats:Array.from(unique.values()),video_limits,
    audio_limits:audio.map(codec=>({codec,max_channels:codec==="aac"?aacChannels:8})),
    ...(compatibility?{compatibility}:{})};
}
const LIVE_TV_LEASE=new PlurxLiveTv.Lease({
  start:async channel=>{
    // The retire sweep is dispatched, never awaited: the liveness probe costs
    // a quarter second and this press must not spend it. The tuner request
    // leaves now and the sweep tidies up beside it, exactly as the Apple and
    // Android leases dispatch theirs. Nothing on this path may refuse or delay
    // a start — admission is the owner's, and it has four slots.
    const recovery=liveTvRecoveryEnabled();
    if(recovery){ try{ liveTvRetireOrphanHints().catch(()=>{}); }catch(e){} }
    // An ingress older than protocol 3 has no id to be told and no route to
    // retire one on, so it is sent none; hints already stored are kept for an
    // ingress that is upgraded later.
    const requestId=recovery?liveTvRequestId():null;
    if(requestId){
      LIVE_TV_HINTS.remember(requestId); // durable before the POST can leave
      // Claimed from this instant, so a sibling document probing during the
      // start window finds this one holding it rather than an orphan.
      LIVE_TV.hint=requestId;
    }
    const compatibility=LIVE_TV.compatibility; LIVE_TV.compatibility=null;
    const body={playback:liveTvPlaybackEnvelope(compatibility)};
    if(requestId) body.request_id=requestId;
    // A start that got no answer is replayed once with the SAME id, which the
    // owner joins to the same session. A start with no id is never replayed —
    // that is exactly how a second tuner gets opened.
    const attempts=1+(requestId?PlaybackPolicy.liveContractTiming("start_replay_attempts"):0);
    for(let attempt=0;;attempt++){
      let answer=null;
      try{
        const info=await liveTvRequest(`/live-tv/channels/${encodeURIComponent(channel)}/sessions`,"POST",45000,true,false,body);
        // The hint stays until a DELETE confirms the session is gone, so an
        // active-player crash still leaves a handle behind to retire.
        if(info&&typeof info.session_id==="string"&&info.session_id) return info;
        answer={body:{}}; // a 2xx that is not a session is not an answer either
      }
      catch(e){ answer=liveTvAnswer(e); }
      const outcome=PlurxLiveTv.startOutcome(answer);
      if(requestId&&!outcome.keepHint){
        LIVE_TV_HINTS.forget(requestId);
        if(LIVE_TV.hint===requestId) LIVE_TV.hint=null;
      }
      if(outcome.replay&&attempt+1<attempts) continue;
      const typed=answer.body||{};
      throw {code:outcome.render,retry:typed.retry,owner_decided:typed.owner_decided,status:answer.status,answer};
    }
  },
  release:async id=>{
    for(let attempt=0;attempt<2;attempt++){
      try{ await liveTvRequest(`/live-tv/sessions/${encodeURIComponent(id)}`,"DELETE",8000,false,LIVE_TV.unloading===true); break; }
      catch(e){ if(e.status===404||e.status===410||e.code==="capability_expired") break; if(attempt===1) throw e; }
    }
    // The owner has said this session is gone, so its handle is worth nothing.
    // A DELETE that never confirmed keeps its hint and its next press retires.
    LIVE_TV_HINTS.forget(LIVE_TV.hint); LIVE_TV.hint=null;
  },
  status:id=>liveTvRequest(`/live-tv/sessions/${encodeURIComponent(id)}/status`,"GET",15000),
  keepalive:id=>{
    LIVE_TV_HINTS.touch(LIVE_TV.hint);
    return liveTvRequest(`/live-tv/sessions/${encodeURIComponent(id)}/keepalive`,"PUT",15000);
  },
});
async function liveTvRequest(path,method,timeout,authenticated=false,keepalive=false,body=null){
  const headers=authenticated&&TOKEN?{authorization:"Bearer "+TOKEN}:{};
  if(body!==null) headers["content-type"]="application/json";
  // `keepalive` lets the unload DELETE outlive the document. Without it the
  // browser aborts the only release that could confirm the durable marker, so
  // every ordinary reload while watching cost a 90-second safety wait for a
  // tuner the server had in fact already released.
  const response=await fetch(API+path,{method,headers,body:body===null?undefined:JSON.stringify(body),cache:"no-store",keepalive,signal:AbortSignal.timeout(timeout)});
  if(!response.ok){
    let payload={}; try{ payload=await response.json(); }catch(e){}
    const error=new Error(payload.message||payload.error||"Live TV request failed");
    error.status=response.status; error.code=payload.code;
    // The two flat fields every typed Live TV refusal carries, plus the whole
    // answer the start reducer rules on. A rejection with no `answer` at all is
    // a request that never came back.
    error.retry=payload.retry; error.owner_decided=payload.owner_decided;
    error.answer={transport:"http",status:response.status,body:payload};
    throw error;
  }
  return response.status===204?null:response.json();
}
function liveTvMessage(message){
  const mount=document.getElementById("live-tv-message"); if(mount) mount.textContent=message;
}
function liveTvFailure(error){
  const view=PlurxLiveTv.errorView(error);
  liveTvMessage(`${view.title}. ${view.detail}`);
  const mount=document.getElementById("live-tv-message");
  if(!mount||!view.offers.length) return;
  for(const offer of view.offers){
    // Use the current lineup as the final authority: a stale capacity answer
    // must not create a button for a channel this client cannot play.
    if(!LIVE_TV.channels.some(channel=>channel.id===offer.channelId&&PlurxLiveTv.channelView(channel).disabled===false)) continue;
    const button=document.createElement("button");
    button.type="button"; button.textContent=offer.label;
    button.addEventListener("click",()=>liveTvSelect(offer.channelId));
    mount.append(" ",button);
  }
}
async function viewLiveTv(generation=PAGE_RENDER_GENERATION){
  if(generation!==PAGE_RENDER_GENERATION) return;
  const route=location.hash;
  liveTvWireHost();
  LIVE_TV.selected=LIVE_TV.selected||liveTvPref("plurx_live_tv_last",null,null);
  layoutChrome("live-tv",`<h1>Live TV</h1><p class="sub">Over-the-air channels from your HDHomeRun, with exact-airing recording status.</p>
    <p id="live-tv-message" role="status" aria-live="polite">Loading channels…</p>
    <div id="live-tv-toolbar"></div>
    <div id="live-tv-body"></div>`);
  setPagePhase(route,generation,"shell");
  // The host follows the route back into its slot; a dock returning here
  // never restarts the stream, it just moves.
  liveTvSetMode("slot");
  const host=liveTvHost(); if(host) host.hidden=!LIVE_TV_LEASE.current;
  try{
    const result=await api("/live-tv/channels",{signal:AbortSignal.timeout(30000)});
    if(generation!==PAGE_RENDER_GENERATION||location.hash!==route) return;
    LIVE_TV.channels=result.channels;
    // Negotiated per lineup read. No field at all is an ingress older than the
    // start-recovery contract: no request id, no retire, no resume. The flag
    // separates "this ingress has no protocols" from "nobody has answered
    // yet", which is what an open-time resume must not confuse.
    LIVE_TV.protocols=Array.isArray(result.protocols)?result.protocols:null;
    LIVE_TV.lineupRead=true;
    if(!liveTvChannelById(LIVE_TV.selected)) LIVE_TV.selected=null;
    const toolbar=document.getElementById("live-tv-toolbar");
    if(toolbar) toolbar.innerHTML=liveTvToolbar();
    renderLiveTvChannels();
    // A Watch pressed on the reminder overlay from another page. Consume-once,
    // so a later visit never re-tunes a channel nobody asked for again.
    const arriving=LIVE_TV.watchOnArrival; LIVE_TV.watchOnArrival=null;
    if(arriving) liveTvSelect(arriving);
    liveTvMessage(result.freshness==="stale"
      ?`Showing a cached lineup (${result.age_seconds} seconds old). The tuner is currently unreachable; starting a channel requires a fresh check.`
      :`${result.channels.length} channels. Choose an unprotected channel to watch.`);
  }catch(e){ if(generation===PAGE_RENDER_GENERATION) liveTvFailure(e); }
  finally{ if(generation===PAGE_RENDER_GENERATION){ setPagePhase(route,generation,"content"); setPagePhase(route,generation,"settled"); } }
  // The guide is the second read and never gates the first: the page is
  // usable, and a channel is tunable, before it answers.
  loadLiveTvGuide(generation,route);
  // Opening Live TV rejoins a session this viewer still owns — it never tunes
  // anything. A resume that cannot even be attempted is a page showing the
  // channel list, never a page that failed to render.
  try{ liveTvResumeStart(generation,route).catch(()=>{}); }catch(e){}
  // Everything on this page is a statement about the present tense — the red
  // now line, every progress bar, "18 min left", which programme is "on now".
  // Without a tick they were all frozen at first paint, so a page left open
  // kept insisting a programme that ended an hour ago was still on. A minute
  // is the resolution the page actually displays; it repaints from state and
  // fetches nothing.
  setPageTimer(()=>{
    if(location.hash!==route) return;
    renderLiveTvChannels();
  },60000,generation);
}
function liveTvHost(){
  return document.getElementById("live-tv-host");
}
function liveTvPref(key,fallback,allowed){
  let value=null; try{ value=localStorage.getItem(key); }catch(e){}
  return allowed&&!allowed.includes(value)?fallback:(value===null?fallback:value);
}
function liveTvSetPref(key,value){
  try{ localStorage.setItem(key,value); }catch(e){}
}
function liveTvView(){ return liveTvPref("plurx_live_tv_view","list",["list","grid"]); }
function liveTvPlayerSize(){ return liveTvPref("plurx_live_tv_player_size","compact",["compact","wide"]); }
function liveTvFilter(){ return liveTvPref("plurx_live_tv_filter","all",["all","favorites"]); }
function liveTvHideProtected(){ return liveTvPref("plurx_live_tv_hide_protected","0",["0","1"])==="1"; }
function liveTvNowSeconds(){ return Math.floor(Date.now()/1000); }
function liveTvSlotSeconds(){ return 1800; }
function liveTvPxPerSlot(){ return 240; }
function liveTvClock(unix){
  return new Date(unix*1000).toLocaleTimeString([],{hour:"numeric",minute:"2-digit"});
}
function liveTvChannelById(id){
  return LIVE_TV.channels.find(channel=>channel.id===id)||null;
}
function liveTvVisible(){
  return PlurxLiveTv.filterChannels(LIVE_TV.channels,LIVE_TV.guide,
    {query:LIVE_TV.query||"",filter:liveTvFilter(),hideProtected:liveTvHideProtected()},liveTvNowSeconds());
}
// The host is fixed and outside #app, so in `slot` mode the page draws an
// empty box and the host is positioned over it. One observer for the
// document's lifetime, rebound whenever the slot is re-rendered — the same
// discipline the rails use.
function liveTvTrackSlot(){
  const host=liveTvHost(); if(!host) return;
  const slot=document.getElementById("live-tv-slot");
  if(!slot||host.dataset.mode!=="slot"){ return; }
  const box=slot.getBoundingClientRect();
  host.style.left=`${box.left}px`; host.style.top=`${box.top}px`;
  host.style.width=`${box.width}px`; host.style.height=`${box.height}px`;
  const body=document.getElementById("live-tv-body");
  if(body) body.style.setProperty("--live-tv-slot-height",`${box.height}px`);
  host.hidden=false;
}
function liveTvWireSlot(){
  const slot=document.getElementById("live-tv-slot"); if(!slot) return;
  if(!LIVE_TV.slotObserver&&window.ResizeObserver){
    LIVE_TV.slotObserver=new ResizeObserver(()=>liveTvTrackSlot());
  }
  if(LIVE_TV.slotObserver){ LIVE_TV.slotObserver.disconnect(); LIVE_TV.slotObserver.observe(slot); }
  if(!LIVE_TV.slotWired){
    LIVE_TV.slotWired=true;
    window.addEventListener("scroll",()=>liveTvTrackSlot(),{passive:true});
    window.addEventListener("resize",()=>liveTvTrackSlot(),{passive:true});
  }
  liveTvTrackSlot();
}
function liveTvSetMode(mode){
  const host=liveTvHost(); if(!host) return;
  host.dataset.mode=mode;
  if(mode!=="slot"){ host.style.left=""; host.style.top=""; host.style.width=""; host.style.height=""; }
  if(mode==="dock"){
    let saved=null; try{ saved=JSON.parse(localStorage.getItem("plurx_live_tv_dock")||"null"); }catch(e){}
    if(saved&&Number.isFinite(saved.x)&&Number.isFinite(saved.y)){
      host.style.left=`${saved.x}px`; host.style.top=`${saved.y}px`; host.style.right="auto"; host.style.bottom="auto";
    }
  }
  host.hidden=false;
  if(mode==="slot") liveTvTrackSlot();
  liveTvPaint();
}
// Leaving the route no longer tears the stream down: it moves the picture into
// a dock. Stopping stays explicit — the stop button here, on the overlay, or
// the existing watchdogs.
function liveTvLeaveRoute(){
  // The guide loop belongs to the page, not to the stream: a docked picture
  // keeps its tuner watchdog (LIVE_TV.timer) on another route, but nothing off
  // this route can show a grid, so the poll stops here.
  clearTimeout(LIVE_TV.guideTimer); LIVE_TV.guideTimer=null;
  const host=liveTvHost(); if(!host) return;
  // `LIVE_TV.starting` matters as much as a current lease: during the start
  // POST there is no `current` yet, and hiding the host here left a tuner
  // about to be granted with nowhere to appear and no Stop control.
  if(LIVE_TV_LEASE.current||LIVE_TV.starting){ liveTvSetMode("dock"); return; }
  host.hidden=true;
}
function liveTvInPip(){
  const video=document.getElementById("live-tv-video");
  if(!video) return false;
  return document.pictureInPictureElement===video||video.webkitPresentationMode==="picture-in-picture";
}
function liveTvPipSupported(){
  const video=document.getElementById("live-tv-video");
  return !!(document.pictureInPictureEnabled||(video&&video.webkitSupportsPresentationMode));
}
async function toggleLiveTvPip(){
  const video=document.getElementById("live-tv-video"); if(!video) return;
  try{
    if(document.pictureInPictureElement===video){ await document.exitPictureInPicture(); }
    else if(video.webkitPresentationMode==="picture-in-picture"){ video.webkitSetPresentationMode("inline"); }
    else if(video.requestPictureInPicture){ await video.requestPictureInPicture(); }
    else if(video.webkitSetPresentationMode){ video.webkitSetPresentationMode("picture-in-picture"); }
  }catch(e){}
  liveTvPaint();
}
function liveTvProgramme(channelId){
  return PlurxLiveTv.programmeAt(LIVE_TV.guide,channelId,liveTvNowSeconds());
}
function liveTvSourceProgrammeEnd(channel){
  if(!channel) return null;
  const observed=channel&&channel.source_format&&channel.source_format.observed_at;
  const row=LIVE_TV.guide&&Array.isArray(LIVE_TV.guide.channels)
    ?LIVE_TV.guide.channels.find(entry=>entry.id===channel.id):null;
  const programme=row&&Array.isArray(row.programmes)
    ?row.programmes.find(entry=>entry.start<=observed&&observed<entry.end):null;
  return programme&&Number.isInteger(programme.end)?programme.end:null;
}
function liveTvFormatBadges(channel){
  const badges=PlurxLiveTv.channelBadges(channel,liveTvNowSeconds(),liveTvSourceProgrammeEnd(channel));
  return badges.length?`<span class="lt-badges" aria-label="Source format: ${esc(badges.join(", "))}">${badges.map(label=>`<span class="lt-fbadge">${esc(label)}</span>`).join("")}</span>`:"";
}
function liveTvTechnicalDetails(channel,status){
  if(!channel) return "";
  const sourceNow=typeof liveTvNowSeconds==="function"?liveTvNowSeconds():Math.floor(Date.now()/1000);
  const sourceEnd=typeof liveTvSourceProgrammeEnd==="function"?liveTvSourceProgrammeEnd(channel):null;
  const source=PlurxLiveTv.sourceDetails(channel,sourceNow,sourceEnd);
  const rows=[];
  if(source.exact.length) rows.push(`<div class="lt-tech-row"><b>Source</b><span>${esc(source.exact.join(" · "))}</span></div>`);
  if(source.observedAt) rows.push(`<div class="lt-tech-row"><b>Observed</b><span>${esc(new Date(source.observedAt*1000).toLocaleString())}</span></div>`);
  if(status){
    const plan=status.delivery;
    const delivery=plan
      ?[plan.video_action==="copy"?"Original video":String(plan.output.video_codec||"").toUpperCase()+" video",
        plan.audio_action==="copy"?"Original audio":String(plan.output.audio_codec||"").toUpperCase()+" audio",
        plan.output.width&&plan.output.height?`${plan.output.width}×${plan.output.height}`:null,
        String(plan.packaging||"").toUpperCase()].filter(Boolean).join(" · ")
      :["H.264",status.output_height?`${status.output_height}p`:null,"AAC"].filter(Boolean).join(" · ");
    const encoder=!plan&&status.encoder&&status.encoder!=="pending"?` · ${String(status.encoder).toUpperCase()} encoder`:"";
    rows.push(`<div class="lt-tech-row"><b>Stream format</b><span>${esc(delivery+encoder)}</span></div>`);
    const signal=status.signal;
    if(signal){
      const metric=(label,value)=>Number.isFinite(value)
        ?`<span><i>${esc(label)}</i><meter min="0" max="100" value="${value}">${value}%</meter><strong>${value}%</strong></span>`:"";
      const meters=[metric("Strength",signal.strength_percent),metric("Quality",signal.quality_percent),metric("Symbol",signal.symbol_quality_percent)].join("");
      if(meters) rows.push(`<div class="lt-tech-row"><b>Signal</b><span class="lt-signal">${meters}</span></div>`);
    }
  }
  return rows.length?`<div class="lt-tech" aria-label="Live stream details">${rows.join("")}</div>`:"";
}
let LIVE_TV_STATS_TIMER=null;
let LIVE_TV_STATS_OPENER=null;
function liveTvStatsTelemetry(){
  const v=document.getElementById("live-tv-video"),current=LIVE_TV_LEASE.current;
  const channel=current&&liveTvChannelById(current.channel?.id||LIVE_TV.selected);
  const attached=!!current&&!!v&&!!(v.currentSrc||v.src);
  const status=LIVE_TV.status,plan=status?.delivery||current?.delivery;
  const source=PlurxLiveTv.sourceDetails(channel,liveTvNowSeconds(),liveTvSourceProgrammeEnd(channel));
  const level=LIVE_TV.hls?.levels?.[LIVE_TV.hls.currentLevel];
  const signal=status?.signal;
  let edge=null,buffer=null,frames=null;
  if(attached){
    try{ buffer=`${bufferRunway(v).toFixed(1)} s`; }catch(e){}
    try{ if(v.seekable.length) edge=`${Math.max(0,v.seekable.end(v.seekable.length-1)-v.currentTime).toFixed(1)} s`; }catch(e){}
    try{ const q=v.getVideoPlaybackQuality?.(); if(q) frames=`${q.droppedVideoFrames} / ${q.totalVideoFrames} frames`; }catch(e){}
  }
  return {
    method:plan?(plan.video_action==="copy"?(plan.audio_action==="copy"?"Remux":"Audio converted for this player"):"Video converted for this player"):"Not reported",
    player_state:!attached?"Waiting for player":v.error?"Failed":v.ended?"Ended":v.paused?"Paused":v.readyState<3?"Buffering":"Playing",
    decode_resolution:attached&&v.videoWidth>0&&v.videoHeight>0?`${v.videoWidth}×${v.videoHeight}`:"Not reported",
    source_resolution:source.exact[0]||"Not reported",
    source_video:source.exact.slice(1).join(" · ")||null,
    source_resolution_note:source.observedAt?`Tuner source observed ${new Date(source.observedAt*1000).toLocaleString()}`:"Source measurement not reported.",
    stream_format:plan?.output?[plan.output.width>0&&plan.output.height>0?`${plan.output.width}×${plan.output.height}`:null,plan.output.video_codec,plan.output.hdr].filter(Boolean).join(" · "):null,
    decode_audio:plan?.output?[plan.output.audio_codec,plan.output.audio_channels>0?`${plan.audio_action==="encode"&&!(plan.source?.audio_channels>0)?"up to ":""}${plan.output.audio_channels} channels`:null].filter(Boolean).join(" · "):null,
    subtitles:attached&&v.textTracks?[...v.textTracks].filter(t=>t.mode==="showing").map(t=>t.label||t.language||"Selected").join(" · ")||"Off":"Not reported",
    client_loaded:buffer||"Not reported",live_edge:edge||"Not reported",frames,
    stalls:"Not reported",stalls_note:"This live player does not expose an interruption counter.",
    observed_rate:LIVE_TV.hls?.bandwidthEstimate?fmtMbps(LIVE_TV.hls.bandwidthEstimate):null,
    stream_rate:level?.bitrate?fmtMbps(level.bitrate):null,
    status:status?.state||"Not reported",encoder:status?.encoder||null,
    session:current?.session_id||null,
    reception:signal?[signal.strength_percent,signal.quality_percent,signal.symbol_quality_percent].map((v,i)=>Number.isFinite(v)?`${["Strength","Quality","Symbol"][i]} ${v}%`:null).filter(Boolean).join(" · "):"Not reported"
  };
}
function setLiveTvStatsMode(mode){
  if(!["mini","standard","details","debug"].includes(mode)) return;
  const panel=document.getElementById("live-tv-stats"); if(!panel) return;
  panel.dataset.mode=mode;
  panel.setAttribute("aria-modal",String(mode!=="mini"));
  const backdrop=document.getElementById("live-tv-stats-backdrop");if(backdrop)backdrop.hidden=mode==="mini";
  updateLiveTvStats();
}
function openLiveTvStats(){
  closeLiveTvStats(false);
  LIVE_TV_STATS_OPENER=document.activeElement;
  const panel=document.createElement("section");
  panel.id="live-tv-stats"; panel.className="statsov on live-stats"; panel.dataset.mode="standard";
  panel.setAttribute("role","dialog");panel.setAttribute("aria-label","Playback info");panel.setAttribute("aria-modal","true");
  panel.innerHTML=`<div class="statshd"><span>Playback info</span><div class="statsmodes" role="radiogroup" aria-label="Playback information level">${[["mini","Compact"],["standard","Overview"],["details","Details"],["debug","Diagnostics"]].map(([id,label])=>`<button type="button" data-live-stats-mode="${id}" role="radio" onclick="setLiveTvStatsMode('${id}')">${label}</button>`).join("")}</div><button class="statsx" aria-label="Close playback info" onclick="closeLiveTvStats()">×</button></div><div class="statsbody"></div>`;
  const parent=document.fullscreenElement||document.body;
  const backdrop=document.createElement("div");backdrop.id="live-tv-stats-backdrop";
  backdrop.style.cssText="position:fixed;inset:0;z-index:9998";
  backdrop.addEventListener("click",event=>{event.stopPropagation();closeLiveTvStats();});
  parent.append(backdrop,panel);
  panel.addEventListener("click",event=>event.stopPropagation());
  wireLiveTvStatsKeys(panel);
  updateLiveTvStats();panel.querySelector(".statsx").focus();
  LIVE_TV_STATS_TIMER=setInterval(updateLiveTvStats,1000);
}
function closeLiveTvStats(restore=true){
  clearInterval(LIVE_TV_STATS_TIMER);LIVE_TV_STATS_TIMER=null;
  document.getElementById("live-tv-stats")?.remove();
  document.getElementById("live-tv-stats-backdrop")?.remove();
  if(restore){
    const opener=LIVE_TV_STATS_OPENER?.isConnected?LIVE_TV_STATS_OPENER:document.querySelector("[data-live-info-opener]");
    opener?.focus();
  }
  LIVE_TV_STATS_OPENER=null;
}
function updateLiveTvStats(){
  const panel=document.getElementById("live-tv-stats");if(!panel)return;
  const mode=panel.dataset.mode,t=liveTvStatsTelemetry(),body=panel.querySelector(".statsbody");
  const rows=playbackInfoRows(mode,t);
  const extra=(id,label,section,note)=>{if(t[id])rows.push({id,label,section,value:t[id],note,placement:"grid",tone:""});};
  if(mode==="details"||mode==="debug"){

    extra("live_edge","Behind stream live edge","BUFFERING / DELIVERY","Behind latest available media; not broadcast delay.");
    extra("reception","Tuner reception","BUFFERING / DELIVERY","Strength, quality and symbol quality reported by the tuner.");
  }
  patchPlaybackInfoRows(body,mode,rows,"","");
  const overview=body.querySelector("[data-stats-overview]");
  if(overview)overview.innerHTML=mode==="mini"?[["Playing resolution",t.decode_resolution],["Playback",t.player_state],["Buffered on device",t.client_loaded]].map(([label,value])=>`<div class="pi-fact"><span class="pi-label">${esc(label)}</span><strong>${esc(value)}</strong></div>`).join(""):playbackInfoOverview(t,true);
  panel.querySelectorAll("[data-live-stats-mode]").forEach(button=>{const selected=button.dataset.liveStatsMode===mode;button.classList.toggle("on",selected);button.setAttribute("aria-checked",String(selected));});
}
function liveTvRefreshTechnicalDetails(){
  const channel=liveTvChannelById(LIVE_TV.selected);
  document.querySelectorAll("[data-live-tv-technical]").forEach(node=>{
    node.innerHTML=liveTvTechnicalDetails(channel,LIVE_TV.status);
  });
}
function liveTvOverlayTechnical(channel,status){
  if(!channel) return "";
  const source=PlurxLiveTv.channelBadges(channel,liveTvNowSeconds(),liveTvSourceProgrammeEnd(channel)).join(" · ");
  const signal=status&&status.signal;
  const rf=signal?[signal.strength_percent,signal.quality_percent,signal.symbol_quality_percent]
    .map((value,index)=>Number.isFinite(value)?`${["strength","quality","symbol"][index]} ${value}%`:null).filter(Boolean).join(" · "):"";
  return [source,rf].filter(Boolean).join(" · ");
}
function liveTvRowMarkup(channel,selectedId){
  const view=PlurxLiveTv.channelView(channel);
  const at=liveTvProgramme(channel.id);
  const pct=at.progress===null?0:Math.round(at.progress*100);
  const classes=["lt-row"];
  if(channel.id===selectedId) classes.push("on");
  if(view.disabled) classes.push("locked");
  const line=view.disabled?'<span class="prog">Protected channel · not playable</span>'
    :at.now?`<span class="prog">${esc(at.now.title)}</span>
       <span class="lt-mini"><i style="width:${pct}%"></i></span>
       <span class="next">${esc(liveTvClock(at.now.end))}${at.next?" · Next: "+esc(at.next.title):""}</span>`
    :'<span class="prog muted">No programme information</span>';
  return `<button type="button" class="${classes.join(" ")}" role="option" aria-selected="${channel.id===selectedId}"
      data-channel="${esc(channel.id)}" onclick="liveTvSelect('${esc(channel.id)}')"${view.disabled?' disabled':''}>
    <span class="lt-chip">${esc(channel.guide_name.slice(0,5))}</span>
    <span><b>${esc(channel.guide_number)} · ${esc(channel.guide_name)}</b>${liveTvFormatBadges(channel)}${line}</span>
    <span>${channel.favorite?'<span class="lt-star" title="Favorite">★</span>':""}${view.disabled?" 🔒":""}${liveTvMarks(channel.id,at.now,true)}</span>
  </button>`;
}
function liveTvToolbar(){
  const counts=`${LIVE_TV.channels.length} channels`;
  const tuners=LIVE_TV.tuners?` · ${esc(LIVE_TV.tuners)}`:"";
  const view=liveTvView(), recs=liveTvRecordingsTab();
  const conflicts=LIVE_TV_DVR.schedule?LIVE_TV_DVR.schedule.conflicts||0:0;
  return `<div class="lt-head">
    <div class="lt-switch" role="group" aria-label="Live TV layout">
      <button type="button" aria-pressed="${!recs&&view==="list"}" onclick="liveTvSetView('list')">≡ List</button>
      <button type="button" aria-pressed="${!recs&&view==="grid"}" onclick="liveTvSetView('grid')">▦ Grid</button>
    </div>
    <div class="lt-switch" role="group" aria-label="Recordings">
      <button type="button" aria-pressed="${recs==="library"}" onclick="liveTvSetRecordingsTab('library')">Library</button>
      <button type="button" aria-pressed="${recs==="scheduled"}" onclick="liveTvSetRecordingsTab('scheduled')">Scheduled${conflicts?` (${conflicts})`:""}</button>
      <button type="button" aria-pressed="${recs==="rules"}" onclick="liveTvSetRecordingsTab('rules')">Rules</button>
    </div>
    <span class="muted">${counts}${tuners}</span>
    <div class="lt-switch" role="group" aria-label="Channel filter">
      <button type="button" aria-pressed="${liveTvFilter()==="all"}" onclick="liveTvSetFilter('all')">All</button>
      <button type="button" aria-pressed="${liveTvFilter()==="favorites"}" onclick="liveTvSetFilter('favorites')">Favorites</button>
    </div>
    <label class="tog" for="lt-hideprot" style="border:0;padding:0;gap:8px"><span>Hide protected</span>
      <input type="checkbox" id="lt-hideprot"${liveTvHideProtected()?" checked":""} onchange="liveTvSetHideProtected(this.checked)"></label>
    <label for="live-tv-search" class="muted" style="font-size:12.5px">Find a channel</label>
    <input id="live-tv-search" type="search" value="${esc(LIVE_TV.query||"")}" oninput="liveTvSearch(this.value)" placeholder="Number, name, or what is on">
  </div>`;
}
function liveTvNowBar(channel){
  if(!channel) return `<div class="lt-nowbar"><span class="grow muted">Select a channel to watch.</span></div>`;
  const at=liveTvProgramme(channel.id);
  const pct=at.progress===null?0:Math.round(at.progress*100);
  const playing=!!LIVE_TV_LEASE.current;
  const left=at.now?Math.max(0,Math.round((at.now.end-liveTvNowSeconds())/60)):null;
  const muted=!!document.getElementById("live-tv-video")?.muted;
  const muteLabel=muted?"Unmute":"Mute";
  const wide=liveTvPlayerSize()==="wide";
  return `<div class="lt-nowbar">
    <span class="grow"><b>${at.now?esc(at.now.title):esc(channel.guide_name)}</b>
      ${playing?'<span class="pill" style="color:var(--bad)">● LIVE</span>':""}<span data-dvr-now-badge>${liveTvRecordingBadge(channel)}</span>
      <br><span class="muted">${esc(channel.guide_number)} · ${esc(channel.guide_name)}${at.now?" · "+esc(liveTvClock(at.now.start))+"–"+esc(liveTvClock(at.now.end)):""}${left!==null?" · "+left+" min left":""}${at.next?" · Next: "+esc(at.next.title):""}</span>
    </span>
    <span class="lt-mini" style="width:120px"><i style="width:${pct}%"></i></span>
    <button type="button" onclick="pauseLiveTv()" title="Pause" aria-label="Pause">⏸</button>
    <button type="button" onclick="liveTvTogglePlayerSize()" title="${wide?"Use compact player":"Use original-size player"}" aria-label="${wide?"Use compact player":"Use original-size player"}">${wide?"⤡ Smaller":"⤢ Larger"}</button>
    <button type="button" data-live-tv-mute onclick="muteLiveTv()" title="${muteLabel}" aria-label="${muteLabel}">${muted?"🔊":"🔇"}</button>
    ${liveTvPipSupported()?'<button type="button" onclick="toggleLiveTvPip()" title="Picture-in-picture (P)" aria-label="Picture-in-picture">⧉</button>':""}
    <button type="button" onclick="fullscreenLiveTv()" title="Fullscreen (F)" aria-label="Fullscreen">⛶</button>
    <button type="button" onclick="stopLiveTv().catch(liveTvFailure)" title="Stop" aria-label="Stop">■</button>
    <button type="button" data-live-info-opener onclick="openLiveTvStats()">Playback info</button>
    <div class="lt-nowtech" data-live-tv-technical>${liveTvTechnicalDetails(channel,LIVE_TV.status)}</div>
  </div>`;
}
function liveTvShowInfo(channel){
  if(!channel) return `<aside class="lt-showinfo"><span class="eyebrow">On now</span><h2>Select a channel</h2><p class="synopsis">Programme details will appear here while the channel plays.</p></aside>`;
  const at=liveTvProgramme(channel.id), row=at.now;
  const time=row?`${esc(liveTvClock(row.start))}–${esc(liveTvClock(row.end))}`:"Schedule unavailable";
  const episode=row&&[row.episode,row.episode_title].filter(Boolean).join(" · ");
  const next=at.next?`<div class="nextup"><b>Up next</b><br>${esc(liveTvClock(at.next.start))} · ${esc(at.next.title)}</div>`:"";
  return `<aside class="lt-showinfo" aria-label="Current programme details">
    <span class="eyebrow">On now</span>
    <h2>${row?esc(row.title):esc(channel.guide_name)}</h2>
    <div class="meta">${esc(channel.guide_number)} · ${esc(channel.guide_name)} · ${time}${episode?"<br>"+esc(episode):""}</div>
    <div data-dvr-live-context>${liveTvRecordingContext(channel,row)}</div>
    <p class="synopsis">${row&&row.synopsis?esc(row.synopsis):"No programme description is available."}</p>
    <div data-live-tv-technical>${liveTvTechnicalDetails(channel,LIVE_TV.status)}</div>
    ${next}
  </aside>`;
}
function liveTvRecordingContext(channel,programme){
  if(!channel)return "";
  const active=DVR_SHARED.overview&&DVR_SHARED.overview.active||[];
  const exact=programme&&active.find(row=>row.channel_id===channel.id&&row.airing_start===programme.start);
  const row=exact||active.find(row=>row.channel_id===channel.id);
  if(!row){
    if(!programme||programme.end<=liveTvNowSeconds())return "";
    const planned=liveTvDvrIndex().row(channel.id,programme.start);
    if(planned)return `<section class="dvr-capture" aria-label="Recording this channel">${dvrStatusMarkup(planned.state==="recording"?"Status unavailable":DVR_STATE_LABEL[planned.state]||planned.state)}<p>${esc(planned.state_reason||(planned.state==="recording"?"Waiting for a fresh update from the recorder.":"This airing is on your recording schedule."))}</p><a data-dvr-focus="live-planned" href="#/recordings/upcoming?selected=${encodeURIComponent(planned.id)}">View recording details →</a></section>`;
    return `<section class="dvr-capture" aria-label="Record this programme"><span class="eyebrow">Keep this programme</span><div class="row" style="flex-wrap:wrap;gap:8px"><button data-dvr-focus="live-record" onclick="liveTvRecord(${esc(JSON.stringify(channel.id))},${programme.start})">Record programme</button><button class="ghost sm" data-dvr-focus="live-series" onclick="liveTvRecordSeries(${esc(JSON.stringify(channel.id))},${programme.start})">Record series</button></div></section>`;
  }
  const state=dvrRowPresentation(row),now=liveTvNowSeconds();
  const context=!exact?(now<row.airing_start?`Early padding for ${row.title}`:now>=row.airing_end?`End padding for ${row.title}`:`Capturing ${row.title}`):"This programme";
  return `<section class="dvr-capture" aria-label="Recording this channel"><span class="eyebrow">${esc(context)}</span>${dvrStatusMarkup(state.label,state.tone)}<p>${esc(state.detail)} · ${esc(state.bytes)}</p>${dvrProgressMarkup(row)}<div class="dvr-card-foot"><a data-dvr-focus="live-activity-${esc(row.recording_id)}" href="#/activity" onclick="openDvrActivity(${esc(JSON.stringify(row.recording_id))});return false">View recording activity →</a>${row.can_stop&&!row.stop_requested_at_ms?`<button class="ghost sm" data-dvr-focus="live-stop-${esc(row.recording_id)}" onclick="liveTvStopRecording(${esc(JSON.stringify(row.recording_id))},${esc(JSON.stringify(row.title))})">Stop recording…</button>`:""}</div><p>Recording continues when you change channels or leave Live TV.</p></section>`;
}
function liveTvAlsoRecording(channel){
  const rows=(DVR_SHARED.overview&&DVR_SHARED.overview.active||[]).filter(row=>!channel||row.channel_id!==channel.id);
  if(!rows.length)return "";
  return `<div class="dvr-strip" aria-label="Other recordings"><b>${channel?"Also recording":"Recording now"}</b>${rows.map(row=>{const state=dvrRowPresentation(row);return `<a data-dvr-focus="live-activity-${esc(row.recording_id)}" href="#/activity" onclick="openDvrActivity(${esc(JSON.stringify(row.recording_id))});return false">${dvrStatusMarkup(state.label,state.tone)} <b>${esc(row.title)}</b><span class="muted">${esc(row.guide_number)} · ${esc(row.channel_name)} · ends ${esc(liveTvClock(row.capture_end))}</span></a>`;}).join("")}</div>`;
}
function refreshLiveTvDvrUi(){
  if(location.hash!=="#/live-tv")return;
  const channel=liveTvChannelById(LIVE_TV.selected)||null;
  const context=document.querySelector("[data-dvr-live-context]");
  const also=document.querySelector("[data-dvr-also]");
  if(context){const saved=dvrRememberUi(context);context.innerHTML=liveTvRecordingContext(channel,channel&&liveTvProgramme(channel.id).now);dvrRestoreUi(context,saved);}
  if(also){const saved=dvrRememberUi(also);also.innerHTML=liveTvAlsoRecording(channel);dvrRestoreUi(also,saved);}
  for(const mark of document.querySelectorAll("[data-dvr-mark]")){
    const rec=mark.querySelector(".rec");if(!rec)continue;
    const row=(DVR_SHARED.overview&&DVR_SHARED.overview.active||[]).find(row=>row.channel_id===mark.dataset.dvrChannel&&row.airing_start===Number(mark.dataset.dvrMark));
    const label=row?dvrRowPresentation(row).label:"Recording planned";
    rec.textContent=`● ${label}`;mark.title=label;mark.setAttribute("aria-label",label);
  }
  const badge=document.querySelector("[data-dvr-now-badge]");if(badge)badge.innerHTML=liveTvRecordingBadge(channel);
}
function liveTvRecordingBadge(channel){
  if(!channel)return "";
  const programme=liveTvProgramme(channel.id).now;
  const row=programme&&(DVR_SHARED.overview&&DVR_SHARED.overview.active||[]).find(row=>row.channel_id===channel.id&&row.airing_start===programme.start);
  if(!row)return "";
  const state=dvrRowPresentation(row);return dvrStatusMarkup(state.label,state.tone);
}

function liveTvStageMarkup(channel,withTonight){
  const wide=liveTvPlayerSize()==="wide";
  return `<div class="lt-stage${wide?" wide":""}">
    <div class="lt-stage-top"><div class="lt-slot" id="live-tv-slot"></div>${liveTvShowInfo(channel)}</div>
    ${liveTvNowBar(channel)}
    ${withTonight?liveTvTonight(channel):""}
  </div><div data-dvr-also>${liveTvAlsoRecording(channel)}</div>`;
}
function liveTvTonight(channel){
  if(!channel) return "";
  const source=(LIVE_TV.guide&&LIVE_TV.guide.channels||[]).find(entry=>entry.id===channel.id);
  const rows=source&&source.programmes?source.programmes:[];
  if(rows.length===0) return `<div class="lt-tonight"><span class="muted">No guide data for this channel.</span></div>`;
  const now=liveTvNowSeconds();
  return `<div class="lt-tonight" aria-label="Tonight on ${esc(channel.guide_name)}">${rows.map(row=>{
    const on=row.start<=now&&now<row.end;
    return `<div class="cell${on?" on":""}" title="${esc(liveTvClock(row.start))}–${esc(liveTvClock(row.end))}">
      <b>${esc(row.title)}</b><small>${esc(liveTvClock(row.start))}${on?" · NOW":""}</small></div>`;
  }).join("")}</div>`;
}
function liveTvListMarkup(visible,selected){
  return `<div class="lt-split">
    <div>${liveTvStageMarkup(selected,true)}</div>
    <div class="lt-list" role="listbox" aria-label="Channels">${
      visible.length?visible.map(channel=>liveTvRowMarkup(channel,selected&&selected.id)).join("")
        :'<div style="padding:18px" class="muted">No matching channels.</div>'}</div>
  </div>`;
}
function liveTvGridWindow(){
  const slot=liveTvSlotSeconds(), now=liveTvNowSeconds();
  const start=Math.floor(now/slot)*slot;
  return {start,end:start+8*slot};
}
/// The layout the grid is currently drawn from. A click handler must resolve
/// its cell through this, not through the guide document: the two arrays are
/// different lengths whenever the server sends backfill, which it always does.
function liveTvGridLayout(){
  const visible=liveTvVisible();
  return PlurxLiveTv.gridLayout(LIVE_TV.guide,visible,liveTvGridWindow(),liveTvNowSeconds(),
    liveTvSlotSeconds(),liveTvPxPerSlot());
}
function liveTvGridMarkup(visible,selected){
  const slot=liveTvSlotSeconds(), px=liveTvPxPerSlot(), now=liveTvNowSeconds();
  const window=liveTvGridWindow();
  const layout=PlurxLiveTv.gridLayout(LIVE_TV.guide,visible,window,now,slot,px);
  const times=PlurxLiveTv.gridSlots(window,slot).map(at=>
    `<div class="t" style="width:${px}px">${esc(liveTvClock(at))}</div>`).join("");
  const rows=layout.rows.map(row=>{
    const ends=PlurxLiveTv.guideEnds(LIVE_TV.guide,row.channel.id,window);
    const cells=row.cells.map((cell,index)=>{
      const playing=selected&&selected.id===row.channel.id&&cell.airing;
      return `<button type="button" class="lt-cell${cell.airing?" airing":""}${playing?" playing":""}"
        style="left:${cell.left}px;width:${Math.max(cell.width-4,12)}px"
        title="${esc(cell.programme.title)} · ${esc(liveTvClock(cell.programme.start))}–${esc(liveTvClock(cell.programme.end))}"
        data-channel="${esc(row.channel.id)}" data-cell="${index}"
        onclick="liveTvGridCell('${esc(row.channel.id)}',${index})">${esc(cell.programme.title)}${liveTvMarks(row.channel.id,cell.programme,false)}</button>`;
    }).join("");
    const marker=ends===null?"":`<div class="lt-cell ends" style="left:${((ends-window.start)/slot)*px}px;width:150px">Guide data ends ${esc(liveTvClock(ends))}</div>`;
    return `<div class="lt-grow">
      <div class="lt-gname"><span class="lt-chip">${esc(row.channel.guide_name.slice(0,5))}</span>
        <span class="lt-gmeta"><span>${esc(row.channel.guide_number)}${row.channel.favorite?' <span class="lt-star">★</span>':""}</span>${liveTvFormatBadges(row.channel)}</span></div>
      <div class="lt-gcells" style="width:${layout.totalWidth}px">${cells}${marker}</div>
    </div>`;
  }).join("");
  const nowLine=layout.nowX===null?"":`<div class="lt-now" style="left:${180+layout.nowX}px"></div>`;
  return `<div class="lt-split">
    ${liveTvStageMarkup(selected,false)}
    <div>
      <div class="lt-gridwrap"><div class="lt-grid">
        <div class="lt-times"><div class="pad"></div>${times}</div>
        ${rows||'<div style="padding:18px" class="muted">No matching channels.</div>'}
        ${nowLine}
      </div></div>
    </div>
  </div>`;
}
function renderLiveTvChannels(){
  const mount=document.getElementById("live-tv-body"); if(!mount) return;
  const visible=liveTvVisible();
  const selected=liveTvChannelById(LIVE_TV.selected)||null;
  const recordings=liveTvRecordingsTab();
  mount.innerHTML=recordings?liveTvRecordingsMarkup(recordings,selected)
    :liveTvView()==="grid"?liveTvGridMarkup(visible,selected):liveTvListMarkup(visible,selected);
  // The toolbar carries the state of both switches and the channel count, and
  // it was painted once at first render — so pressing List/Grid or
  // All/Favorites changed the body while the control kept showing, to a
  // sighted user and to a screen reader alike, the option that was NOT chosen.
  const head=document.getElementById("live-tv-toolbar");
  if(head){
    // Repainting replaces the search field, and this runs on every keystroke,
    // so carry the caret across or typing loses focus after one character.
    const active=document.activeElement;
    const searching=active&&active.id==="live-tv-search";
    const caret=searching?active.selectionStart:null;
    head.innerHTML=liveTvToolbar();
    if(searching){
      const next=document.getElementById("live-tv-search");
      if(next){ next.focus(); try{ next.setSelectionRange(caret,caret); }catch(_){} }
    }
  }
  liveTvWireSlot();
  liveTvPaint();
}
// Switching views is a re-render of the browse region and nothing else: it
// never stops the stream, never restarts it, and never refetches.
function liveTvSetView(view){
  liveTvSetPref("plurx_live_tv_view",view);
  // List and Grid are the channel browser; choosing one leaves the recordings
  // segment, which occupies the same region.
  liveTvSetPref("plurx_live_tv_recordings","");
  renderLiveTvChannels();
}
function liveTvTogglePlayerSize(){
  liveTvSetPref("plurx_live_tv_player_size",liveTvPlayerSize()==="wide"?"compact":"wide");
  renderLiveTvChannels();
}
function liveTvSetFilter(filter){
  liveTvSetPref("plurx_live_tv_filter",filter); renderLiveTvChannels();
}
function liveTvSetHideProtected(on){
  liveTvSetPref("plurx_live_tv_hide_protected",on?"1":"0"); renderLiveTvChannels();
}
function liveTvSearch(value){
  LIVE_TV.query=value; renderLiveTvChannels();
}
function liveTvSelect(id){
  const index=LIVE_TV.channels.findIndex(channel=>channel.id===id);
  if(index<0) return;
  cancelLiveTvChannelGesture();
  LIVE_TV.selected=id; liveTvSetPref("plurx_live_tv_last",id);
  watchLiveTv(index);
  renderLiveTvChannels();
}
function liveTvGridCell(channelId,index){
  // An airing cell used to tune the channel and return here, which predates
  // this cell having verbs at all. It meant the popover — and with it Record,
  // Record series and the tuner sentence — was reachable only on a FUTURE
  // programme, so the one case a viewer asks for most, "record what I am
  // watching", had no path on the web at all. `dvrAiringActions` has always
  // answered for an on-air programme (`watch: "now"`, `record: "record"`), and
  // the popover puts Watch first, so opening it costs one click and unlocks
  // the other three verbs.
  // Index into the SAME array the cell came from. `gridLayout` clips to the
  // drawn window while the server deliberately ships an hour of backfill, so
  // indexing the unclipped `programmes` was reliably off by however many
  // finished rows the server sent — every popover showed the wrong programme.
  const layout=liveTvGridLayout();
  const row=layout&&layout.rows?layout.rows.find(entry=>entry.channel&&entry.channel.id===channelId):null;
  const cell=row&&row.cells?row.cells[index]:null;
  liveTvPopover(cell?cell.programme:null,channelId);
}
// A cell's four verbs and the tuner sentence under them. Which of the four are
// offered is `dvrAiringActions`' answer, not this function's: every "no" there
// is a request the routes would refuse, and an action that can only be refused
// is worse than an absent one.
function liveTvPopover(row,channelId){
  let pop=document.getElementById("live-tv-pop");
  if(!pop){
    pop=document.createElement("div"); pop.id="live-tv-pop"; pop.className="lt-pop";
    // It behaves as a modal — it takes the keyboard and closes on Escape — so
    // it says it is one, the way the lightbox does. Without this a cell's
    // verbs were a panel in the middle of the screen that a keyboard could
    // only reach by tabbing past the whole page, and could not leave at all.
    pop.setAttribute("role","dialog");
    pop.setAttribute("aria-modal","true");
    pop.tabIndex=-1;
    pop.addEventListener("click",e=>{ if(e.target===pop) liveTvPopover(null); });
    document.body.appendChild(pop);
  }
  if(!row){
    pop.hidden=true;
    // Hand the keyboard back to the cell that opened it. The grid re-renders
    // on the guide poll, so the element may be gone; falling back to the grid
    // keeps focus inside the page rather than dropping it on <body>.
    const back=LIVE_TV_POP_OPENER&&document.contains(LIVE_TV_POP_OPENER)
      ? LIVE_TV_POP_OPENER : document.getElementById("live-tv-grid");
    LIVE_TV_POP_OPENER=null;
    if(back&&back.focus) try{ back.focus(); }catch(_){}
    return;
  }
  LIVE_TV_POP_OPENER=document.activeElement&&document.activeElement.classList
    &&document.activeElement.classList.contains("lt-cell")?document.activeElement:null;
  pop.setAttribute("aria-label",row.title||"Programme");
  const channel=liveTvChannelById(channelId);
  pop.innerHTML=`<h3>${esc(row.title)}</h3>
    <p>${esc(channel?channel.guide_number+" · "+channel.guide_name:"")} · ${esc(liveTvClock(row.start))}–${esc(liveTvClock(row.end))}${row.episode?" · "+esc(row.episode):""}</p>
    ${row.episode_title?`<p><b>${esc(row.episode_title)}</b></p>`:""}
    ${row.synopsis?`<p>${esc(row.synopsis)}</p>`:""}
    ${row.filters&&row.filters.length?`<div class="pills">${row.filters.map(f=>`<span class="pill">${esc(f)}</span>`).join("")}</div>`:""}
    ${liveTvPopoverActions(row,channelId)}
    <div class="row"><button class="ghost sm" onclick="liveTvPopover(null)">Close</button></div>`;
  pop.hidden=false;
  // Watch is the first button, so focusing it keeps the old one-Enter path to
  // tuning: Enter on a cell, Enter again to watch.
  const first=pop.querySelector("button");
  if(first) try{ first.focus(); }catch(_){ try{ pop.focus(); }catch(__){} }
  else try{ pop.focus(); }catch(_){}
  const box=pop.getBoundingClientRect();
  pop.style.left=`${Math.max(12,Math.min(window.innerWidth-box.width-12,(window.innerWidth-box.width)/2))}px`;
  pop.style.top=`${Math.max(12,(window.innerHeight-box.height)/2)}px`;
}
// The fullscreen overlay. Redrawn on every paint so the progress bar and the
// neighbour strip stay honest without a second timer.
function liveTvPaint(){
  const host=liveTvHost(); if(!host) return;
  const channel=liveTvChannelById(LIVE_TV.selected);
  const caption=document.getElementById("live-tv-caption");
  const at=channel?liveTvProgramme(channel.id):{now:null,next:null,progress:null};
  if(caption){
    caption.innerHTML=channel?`<span class="lt-chip">${esc(channel.guide_name.slice(0,5))}</span>
      <span class="grow"><b>${at.now?esc(at.now.title):esc(channel.guide_name)}</b> · ${esc(channel.guide_number)}</span>
      <button type="button" onclick="location.hash='#/live-tv'" title="Back to Live TV" aria-label="Back to Live TV">↩</button>
      ${liveTvPipSupported()?'<button type="button" onclick="toggleLiveTvPip()" title="Picture-in-picture" aria-label="Picture-in-picture in the dock">⧉</button>':""}
      <button type="button" onclick="stopLiveTv().catch(liveTvFailure)" title="Stop" aria-label="Stop from the dock">■</button>`:"";
  }
  const overlay=document.getElementById("live-tv-overlay");
  if(!overlay||host.dataset.mode!=="full") return;
  const visible=liveTvVisible();
  const around=liveTvStrip(visible,channel);
  const pct=at.progress===null?0:Math.round(at.progress*100);
  const left=at.now?Math.max(0,Math.round((at.now.end-liveTvNowSeconds())/60)):null;
  const muted=!!document.getElementById("live-tv-video")?.muted;
  const muteLabel=muted?"Unmute":"Mute";
  overlay.innerHTML=`<div class="lth-top">
      <div class="lth-now">
        <span class="lth-chip">${esc(channel?channel.guide_name.slice(0,5):"")}</span>
        <span><span class="lth-title">${at.now?esc(at.now.title):esc(channel?channel.guide_name:"Live TV")}<span class="lth-live">● LIVE</span></span>
          <span class="lth-sub">${esc(channel?channel.guide_number+" · "+channel.guide_name:"")}${at.now?" · "+esc(liveTvClock(at.now.start))+"–"+esc(liveTvClock(at.now.end)):""}${at.next?" · Next: "+esc(at.next.title):""}<span class="lth-tech">${esc(liveTvOverlayTechnical(channel,LIVE_TV.status))}</span></span></span>
      </div>
      <div class="lth-acts">
        <button type="button" data-live-tv-mute onclick="muteLiveTv()" title="${muteLabel}" aria-label="${muteLabel}">${muted?"🔊":"🔇"}</button>
        ${liveTvPipSupported()?'<button type="button" onclick="toggleLiveTvPip()">PiP</button>':""}
        <button type="button" onclick="liveTvGuideSheet()">Guide</button>
        <button type="button" data-live-info-opener onclick="openLiveTvStats()">Playback info</button>
        <button type="button" onclick="stopLiveTv().catch(liveTvFailure)" aria-label="Stop Live TV">Stop</button>
        <button type="button" onclick="exitLiveTvPresentation()">Exit</button>
      </div>
    </div>
    <div class="lth-bottom">
      <div class="lth-bar"><i style="width:${pct}%"></i></div>
      <div class="lth-left">${left!==null?left+" minutes left":"No programme information"}</div>
      <div class="lth-strip">${around.map(entry=>{
        const entryAt=liveTvProgramme(entry.id);
        const entryPct=entryAt.progress===null?0:Math.round(entryAt.progress*100);
        const on=channel&&entry.id===channel.id;
        return `<button type="button" class="lth-card${on?" on":""}${LIVE_TV.preview===entry.id&&!on?" prev":""}"
          onclick="liveTvSelect('${esc(entry.id)}')"><b>${esc(entry.guide_number)} ${esc(entry.guide_name)}</b>
          <small>${entryAt.now?esc(entryAt.now.title):"—"}</small>
          <span class="lth-bar" style="margin:6px 0 0"><i style="width:${entryPct}%"></i></span></button>`;
      }).join("")}</div>
      <div class="lth-hints">↑ ↓ change channel · ← → preview · Enter tune · F fullscreen · M mute · P picture-in-picture · G guide · Esc exit</div>
    </div>
    <div class="lth-sheet" id="live-tv-sheet" hidden></div>`;
}
// Seven cards, current centred: enough to see where you are in the lineup
// without turning the strip into a second channel list.
function liveTvStrip(visible,channel){
  if(visible.length===0) return [];
  const at=channel?visible.findIndex(entry=>entry.id===channel.id):0;
  const centre=at<0?0:at;
  const out=[];
  for(let offset=-3;offset<=3;offset++){
    const index=((centre+offset)%visible.length+visible.length)%visible.length;
    if(!out.includes(visible[index])) out.push(visible[index]);
  }
  return out;
}
function liveTvGuideSheet(){
  const sheet=document.getElementById("live-tv-sheet"); if(!sheet) return;
  if(!sheet.hidden){ sheet.hidden=true; return; }
  const visible=liveTvVisible(), now=liveTvNowSeconds();
  sheet.innerHTML=`<h2 class="section" style="color:#fff">On now</h2>
    ${visible.map(channel=>{
      const at=liveTvProgramme(channel.id);
      return `<div style="display:flex;gap:12px;padding:8px 0;border-top:1px solid rgba(255,255,255,.14)">
        <b style="width:110px;flex:none">${esc(channel.guide_number)} ${esc(channel.guide_name)}</b>
        <span style="flex:1">${at.now?esc(at.now.title):"—"}</span>
        <span style="opacity:.7">${at.now?esc(liveTvClock(at.now.end)):""}</span></div>`;
    }).join("")}
    <p style="opacity:.7;margin-top:14px">Press G or Escape to close. ${esc(liveTvClock(now))}</p>`;
  sheet.hidden=false;
}
// live-tv-input-adapter:begin
// Live TV owns the channel keys while its host is fullscreen or focused. The
// answers come from the shared contract table, not from this file: the same
// fixture drives PlayerInputRouting.swift and PlayerInputPolicy.kt.
function liveTvInputState(){
  const host=liveTvHost();
  if(!host||host.hidden) return null;
  if(host.dataset.mode==="full") return host.classList.contains("idle")?"fullscreen_hidden":"fullscreen_controls";
  // The contract's `browser` state is "inline on the Live TV page". A dock on
  // some other route is not that, and treating it as `page` claimed Space
  // application-wide: every button a keyboard user reached anywhere in the app
  // went dead while a session was docked, and the film player fought the dock
  // over the same key. Off the route and out of fullscreen, Live TV owns no
  // keys at all — its own controls are still clickable and focusable.
  if(location.hash!=="#/live-tv") return null;
  // A modal owns the keyboard while it is open, exactly as handlePlayerKeydown
  // requires for the finite player.
  const modal=document.getElementById("modal");
  if(modal&&!modal.hidden) return null;
  return "browser";
}
/// True where a key press is the element's own activation, not ours to take.
/// `preventDefault` on Space over a focused button suppresses the activation
/// click the browser would synthesise, which is how the overlay's own Mute,
/// PiP, Guide and Exit buttons became unusable by keyboard.
function liveTvTargetOwnsKey(target,key){
  if(!target||!target.closest) return false;
  if(key!=="Enter"&&key!==" ") return false;
  return !!target.closest("button,a[href],select,textarea,input,summary,[role=button],[role=option],[contenteditable=''],[contenteditable=true]");
}
function liveTvInputSurface(){
  return window.matchMedia&&window.matchMedia("(pointer:coarse)").matches?"touch":"desktop";
}
const LIVE_TV_CHANNEL_GESTURE=PlaybackPolicy.createHeldKeyCommitter({
  delayMs:PlaybackPolicy.liveContractTiming("channel_coalesce_ms"),
  commit:owner=>{
    if(owner!==LIVE_TV) return;
    const pending=LIVE_TV.pendingChannel;
    LIVE_TV.pendingChannel=null; LIVE_TV.preview=null;
    if(pending) liveTvSelect(pending);
  }
});
function cancelLiveTvChannelGesture(){
  LIVE_TV_CHANNEL_GESTURE.cancel();
  LIVE_TV.pendingChannel=null; LIVE_TV.preview=null;
}
function liveTvChangeChannel(delta,key){
  const visible=liveTvVisible();
  // Step from where the held key has already got to, not from the channel
  // still on screen. Reading `selected` on every repeat made ten presses
  // resolve to the one adjacent channel: the coalesce was right, the
  // accumulator was missing, and surfing by holding the key was impossible.
  const from=LIVE_TV.pendingChannel||LIVE_TV.preview||LIVE_TV.selected;
  const id=PlurxLiveTv.adjacentChannel(visible,from,delta);
  if(!id) return;
  // The quiet timer batches repeats, but physical key ownership decides when
  // the gesture may commit. Default desktop repeat delay exceeds 350 ms.
  LIVE_TV.pendingChannel=id;
  LIVE_TV_CHANNEL_GESTURE.press(key,LIVE_TV);
  LIVE_TV.preview=id; liveTvPaint();
}
function liveTvPreviewChannel(delta){
  const visible=liveTvVisible();
  const from=LIVE_TV.preview||LIVE_TV.selected;
  LIVE_TV.preview=PlurxLiveTv.adjacentChannel(visible,from,delta);
  liveTvReveal(); liveTvPaint();
}
function liveTvApplyOutcome(outcome,ctx={}){
  switch(outcome){
    case "reveal": liveTvReveal(); return true;
    case "hide": liveTvIdle(); return true;
    case "toggle_chrome":
      if(liveTvHost().classList.contains("idle")) liveTvReveal(); else liveTvIdle();
      return true;
    case "channel_up": liveTvChangeChannel(-1,ctx.key); return true;
    case "channel_down": liveTvChangeChannel(1,ctx.key); return true;
    case "strip_prev": liveTvPreviewChannel(-1); return true;
    case "strip_next": liveTvPreviewChannel(1); return true;
    case "tune": if(LIVE_TV.preview) liveTvSelect(LIVE_TV.preview); return true;
    case "toggle_play": {
      const video=document.getElementById("live-tv-video");
      if(video&&video.paused) resumeLiveTv(); else pauseLiveTv();
      return true;
    }
    case "exit": exitLiveTvPresentation(); return true;
    case "delegate":
    case "focus_control":
    case "focus_cell":
    case "focus_panel":
    case "activate": return false;
    case "close_panel": liveTvGuideSheet(); return true;
    case "return_browser": exitLiveTvPresentation(); return true;
    default: return false;
  }
}
function wireLiveTvStatsKeys(panel){
  panel.addEventListener("keydown",event=>{
    if(event.key==="Escape"){event.preventDefault();closeLiveTvStats();}
    if(event.key==="Tab"&&panel.dataset.mode!=="mini"){
      const items=[...panel.querySelectorAll("button,summary")].filter(node=>node.getClientRects().length);
      const at=items.indexOf(document.activeElement),next=event.shiftKey?(at-1+items.length)%items.length:(at+1)%items.length;
      event.preventDefault();items[next]?.focus();
    }
    event.stopPropagation();
  });
}
function liveTvKeydown(event){
  if(document.getElementById("live-tv-stats")) return;
  const state=liveTvInputState();
  if(state===null) return;
  if(event.target&&/^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName)) return;
  if(liveTvTargetOwnsKey(event.target,event.key)) return;
  const hotkey=PlaybackPolicy.liveHotkey(event.key);
  if(hotkey&&state!=="browser"){
    event.preventDefault();
    if(hotkey==="fullscreen") fullscreenLiveTv();
    else if(hotkey==="mute") muteLiveTv();
    else if(hotkey==="picture_in_picture") toggleLiveTvPip();
    else if(hotkey==="guide_sheet") liveTvGuideSheet();
    else if(hotkey==="exit") exitLiveTvPresentation();
    return;
  }
  const input={ArrowLeft:"left",ArrowRight:"right",ArrowUp:"up",ArrowDown:"down",
    Enter:"select",Escape:"back"," ":"play_pause"}[event.key];
  if(!input) return;
  const outcome=PlaybackPolicy.routeLiveInput(liveTvInputSurface(),state,input);
  if(outcome==="ignore") return;
  if(liveTvApplyOutcome(outcome,{key:event.key})) event.preventDefault();
}
function liveTvWireKeys(){
  window.addEventListener("keydown",liveTvKeydown);
  window.addEventListener("keyup",event=>LIVE_TV_CHANNEL_GESTURE.release(event.key,LIVE_TV));
  window.addEventListener("blur",()=>LIVE_TV_CHANNEL_GESTURE.blur(LIVE_TV));
}
// live-tv-input-adapter:end
function liveTvReveal(){
  const host=liveTvHost(); if(!host) return;
  host.classList.remove("idle");
  clearTimeout(LIVE_TV.idleTimer);
  const video=document.getElementById("live-tv-video");
  if(host.dataset.mode!=="full"||!video||video.paused) return;
  LIVE_TV.idleTimer=setTimeout(()=>{
    const still=document.getElementById("live-tv-video");
    if(still&&!still.paused&&!still.ended) liveTvIdle();
  },PlaybackPolicy.liveContractTiming("hide_after_ms"));
}
function liveTvIdle(){
  const host=liveTvHost(); if(host) host.classList.add("idle");
}
function liveTvWireHost(){
  if(LIVE_TV.hostWired) return;
  LIVE_TV.hostWired=true;
  // Deliberately no "keydown" here: the routing table is the one answer to
  // what a key does, and a second listener that also reveals would give every
  // `ignore` row an effect. Same choice the finite player made.
  ["mousemove","pointerdown","touchstart"].forEach(event=>window.addEventListener(event,()=>{
    if(liveTvInputState()==="fullscreen_hidden"||liveTvInputState()==="fullscreen_controls") liveTvReveal();
  },{passive:true}));
  liveTvWireKeys();
  const video=document.getElementById("live-tv-video");
  if(video){
    video.addEventListener("leavepictureinpicture",()=>liveTvPaint());
    video.addEventListener("enterpictureinpicture",()=>liveTvPaint());
  }
  document.addEventListener("fullscreenchange",()=>{
    const host=liveTvHost();
    if(!host) return;
    if(document.fullscreenElement===host){ liveTvSetMode("full"); liveTvReveal(); }
    else if(host.dataset.mode==="full"){ liveTvSetMode(location.hash==="#/live-tv"?"slot":"dock"); }
  });
}
function liveTvShowHost(){
  const host=liveTvHost(); if(!host) return;
  host.hidden=false;
  if(host.dataset.mode==="slot") liveTvTrackSlot();
  liveTvPaint();
}
async function loadLiveTvGuide(generation,route){
  let answered=null;
  try{
    const guide=await api("/live-tv/guide",{signal:AbortSignal.timeout(20000)});
    if(generation!==PAGE_RENDER_GENERATION||location.hash!==route) return;
    answered=guide;
    LIVE_TV.guide=guide;
    renderLiveTvChannels();
    if(guide.source!=="off"&&guide.freshness==="unavailable"){
      liveTvMessage(`${LIVE_TV.channels.length} channels. The programme guide has no data yet${guide.refresh_error?": "+guide.refresh_error:"."}`);
    }
  }catch(e){
    // A guide that will not load is a degraded page, never a broken one.
    if(generation===PAGE_RENDER_GENERATION&&location.hash===route) LIVE_TV.guide=null;
  }
  loadLiveTvDvr(generation,route);
  // And never a page that stops asking: the grid has to fill without the
  // viewer leaving and coming back. A loop that cannot be scheduled costs this
  // page its guide refresh and nothing else, so it is never thrown upwards.
  try{ scheduleLiveTvGuide(answered,generation,route); }catch(e){}
}
function scheduleLiveTvGuide(guide,generation,route){
  clearTimeout(LIVE_TV.guideTimer); LIVE_TV.guideTimer=null;
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route) return;
  // The owner's clock decides when there is something new to ask for; every
  // number in that decision comes from the shared contract table.
  const delay=PlurxLiveTv.guidePollDelayMs(guide,Math.floor(Date.now()/1000),{
    guide_poll_min_s:PlaybackPolicy.liveContractTiming("guide_poll_min_s"),
    guide_poll_after_next_refresh_s:PlaybackPolicy.liveContractTiming("guide_poll_after_next_refresh_s"),
    guide_poll_unavailable_s:PlaybackPolicy.liveContractTiming("guide_poll_unavailable_s"),
    guide_poll_ceiling_s:PlaybackPolicy.liveContractTiming("guide_poll_ceiling_s"),
  });
  LIVE_TV.guideTimer=setTimeout(()=>{
    LIVE_TV.guideTimer=null;
    loadLiveTvGuide(generation,route);
  },delay);
}
