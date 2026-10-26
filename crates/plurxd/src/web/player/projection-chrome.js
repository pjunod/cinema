"use strict";
// ---- projection chrome, skip intro/credits --------------------------------
// Reveal the title bar / cursor on activity; hide them after a quiet spell
// while the video is actually playing (projection feel).
function playerActivity(){
  const p=document.getElementById("player"); if(!p) return;
  const wasIdle=p.classList.contains("idle");
  p.classList.remove("idle");
  if(wasIdle&&document.activeElement===document.body){
    const last=document.getElementById((PLAYER&&PLAYER._lastFocusedControl)||"pbplay")||document.getElementById("pbplay");
    if(last) last.focus({preventScroll:true});
  }
  clearTimeout(PLAYER.idleTimer);
  // The idle tick is an input like any other. Asking the routing table which
  // states may hide keeps one answer in one place: the hand-written
  // suppression list that used to live here had already drifted from the
  // contract (it did not know about a pointer scrub).
  PLAYER.idleTimer=setTimeout(function idle(){
    const v=document.getElementById("video");
    const outcome=(v&&!v.paused&&!v.ended)
      ? watchRouteInput(playerInputState(),"idle")
      : "ignore";
    if(outcome==="ignore"){ PLAYER.idleTimer=setTimeout(idle,4000); return; }
    applyPlayerOutcome(outcome,{direction:"idle"});
  },4000);
}
// Web input stays desktop at every width and pointer capability.
function playerInputSurface(){ return "desktop"; }
// One pending position, however it was started: arrows and J/L set
// `_seekPending`, a pointer drag sets `_seekPreview`. Either one means the
// bar is showing a time the film is not at yet.
function playerSeekPending(){ return !!(PLAYER&&(PLAYER._seekPending!=null||PLAYER._seekPreview!=null)); }
// Intrinsic dimensions belong to the presented picture. Requested rungs and
// source metadata can describe another stream while a change is still pending.
function playbackQualityResolution(width,height){
  return [144,240,360,480,540,576,720,1080,1440,2160,4320].includes(height)
    ?`${height}p`:`${width}×${height}`;
}
function updatePlaybackQualityChrome(v,p){
  if(!v||!p||PLAYER!==p||document.getElementById("video")!==v) return;
  const now=performance.now(),presentation=p.transportPresentation;
  let current=p.qualityChrome;
  const width=Math.round(Number(v.videoWidth)||0),height=Math.round(Number(v.videoHeight)||0);
  const owns=playbackOwnsAttachedMedia(p)&&!p.terminalStop;
  if(owns&&!p.preparedCommitting&&!v.seeking&&width>0&&height>0
    &&presentation?.element===v&&presentation.attachment===p.mediaAttachment
    &&Number.isFinite(p.presentationAdvancedAt)&&current?.proof!==p.presentationAdvancedAt){
    const label=playbackQualityResolution(width,height);
    const grade=["sdr","hdr10","hdr10plus","hlg","dolby_vision"].includes(p.deliveredRange)?p.deliveredRange:null;
    const candidate=(p.qualityCandidates||[]).find(row=>row.id===p.qualityCandidateId);
    const codec=["h264","hevc","av1","vp9","vp8"].includes(candidate?.planned_codec)?candidate.planned_codec:null;
    const resized=!!current&&(current.width!==width||current.height!==height);
    const changed=resized||!!current&&(current.grade&&grade&&current.grade!==grade
      ||current.codec&&codec&&current.codec!==codec);
    const range=grade&&grade!=="sdr"?` · ${grade==="dolby_vision"?"Dolby Vision":grade.toUpperCase()}`:"";
    current=p.qualityChrome={width,height,label,grade,codec,proof:p.presentationAdvancedAt,
      notice:changed?resized?`Quality changed: ${current.label} → ${label}`:`Quality changed: ${label}${range}${codec?` · ${codec.toUpperCase()}`:""}`:current?.notice||"",
      until:changed?now+3500:current?.until||0};
  }
  const selected=playQuality();
  const mode=selected==="auto"?"Auto":["original","nomse"].includes(selected)?"Original":"Manual";
  const modeNode=document.getElementById("pbqualitymode"),resolution=document.getElementById("pbqualitycurrent");
  if(modeNode) modeNode.textContent=mode;
  if(resolution) resolution.textContent=current?.label||"—";
  const button=document.getElementById("pbquality");
  if(button){
    const detail=current?`${mode} · ${current.label} (${current.width}×${current.height})`:`${mode} · awaiting picture`;
    button.title=`Playback quality: ${detail}`;
    button.setAttribute("aria-label",`Playback quality: ${detail}`);
  }
  const notice=document.getElementById("pqualitynotice");
  if(!notice) return;
  const surface=PLAYBACK_SURFACE.surface;
  const clear=owns&&!p.preparedCommitting&&!p.controlSeek&&(!surface||surface.kind==="none");
  if(current&&!clear) current.until=0;
  const shown=clear&&current?.until>now;
  notice.hidden=!shown;
  const message=shown?current.notice:"";
  if(notice.textContent!==message) notice.textContent=message;
}
function playerInputState(){
  const p=document.getElementById("player");
  // The input contract's `failed` state is the presenter's answer, not a DOM
  // class four call sites could set: a BLOCKING surface whose class is a
  // prompt or a terminal — one with an answer the viewer has to give. A
  // full-screen `preparing` covers the picture and asks nothing, so it keeps
  // today's routing (PLAYBACK-SURFACE-CONTRACT.md §4).
  // The model, not `PLAYER.surfaceState`: they are the same object whenever
  // there is a player, and when there is not — a cold start whose create was
  // refused before `play()` built one — the prompt on screen still has to
  // route as `failed` rather than as ordinary transport.
  if(PlaybackPolicy.surfaceEntersFailedRouting(PLAYBACK_SURFACE.surface)) return "failed";
  // Contract §2.2 orders the precedence scrub → menu → info: a pending seek
  // is the most local thing on screen, so Escape cancels it rather than
  // closing whatever is behind it.
  const seek=document.getElementById("pseek");
  const onSeek=document.activeElement===seek||!!(PLAYER&&PLAYER._seekDragging);
  if(onSeek&&playerSeekPending()) return "scrub";
  const menu=document.getElementById("pmenu");
  if(menu&&menu.classList.contains("on")) return "menu";
  const so=document.getElementById("statsov");
  if(so&&so.classList.contains("on")&&so.dataset.mode!=="mini") return "info";
  if(onSeek) return "timeline";
  if(p&&p.classList.contains("idle")) return "hidden";
  return "transport";
}
// ---- title info panel -----------------------------------------------------
// What you're watching, sat above the transport: title (series + SxEyy for an
// episode), release year or air date, runtime, and the synopsis. It rides the
// same auto-hide as the rest of the chrome, so it's there when you reach for the
// controls and gone while you're actually watching. Toggle with the ⓘ button;
// the choice sticks per browser.
function playerInfoOn(){ try{ return localStorage.getItem("plurx_playerinfo")!=="0"; }catch(e){ return true; } }
function togglePlayerInfo(){
  if(WATCH&&WATCH.mode!=="full"){watchShowTitleInfo();return;}
  const on=!playerInfoOn();
  try{ localStorage.setItem("plurx_playerinfo", on?"1":"0"); }catch(e){}
  renderPlayerInfo(); playerActivity();
  toast(on?"Title info shown":"Title info hidden");
}
function renderPlayerInfo(){
  if(typeof watchRefreshFactBadges==="function")watchRefreshFactBadges();
  const c=document.getElementById("pinfo"); if(!c) return;
  const b=document.getElementById("pbinfo"); if(b) b.classList.toggle("on", playerInfoOn());
  const m=(PLAYER&&PLAYER.meta)||null;
  if((WATCH&&WATCH.mode!=="full")||!m || !playerInfoOn()){ c.innerHTML=""; return; }
  // An episode reads "Show · S02E05 · Episode title"; a movie is just its title.
  const se=(m.season!=null&&m.episode!=null)
    ? `S${String(m.season).padStart(2,"0")}E${String(m.episode).padStart(2,"0")}` : "";
  const head=[m.show?esc(m.show):"", se, esc(m.title||"")].filter(Boolean);
  const bits=[];
  // Episodes are placed by air date; a movie by its release year.
  if(m.air_date) bits.push(esc(fmtDate(m.air_date)));
  else if(m.year) bits.push(String(m.year));
  if(m.runtime_ms) bits.push(fmtDur(m.runtime_ms));
  const facts=playerFactBadges();
  c.innerHTML=`<h3>${head.join(' <i>·</i> ')}</h3>`
    +(facts?`<div class="vbadges pifacts">${facts}</div>`:``)
    +(bits.length?`<div class="pimeta">${bits.join(' <i>·</i> ')}</div>`:``)
    +(m.overview?`<p>${esc(m.overview)}</p>`:``)
    +(PLAYER&&PLAYER.libraryChannel?`<div class="lc-actions"><button onclick="libraryChannelWatchFromStart()">Watch from start</button><span class="pill">FOLLOWING</span></div>`:``)
    +(WATCH?.page?watchGenerationAction(watchCurrentFile())+watchTranslationAction(watchCurrentFile()):"")
    +(m.return_channel_id?`<div class="lc-actions"><button onclick="returnToLibraryChannel()">Return to channel</button></div>`:``);
  c.querySelector("[data-watch-translate]")?.addEventListener("click",event=>watchTranslateSubtitles(event.currentTarget));
  c.querySelector("[data-watch-generate]")?.addEventListener("click",event=>watchGenerateSubtitles(event.currentTarget));
}
// Build the panel's payload from an item (plus its ancestors, so an episode can
// name its show). Kept next to the renderer so the two stay in step.
function playerMeta(it, ancestors){
  if(!it) return null;
  const show=(ancestors||[]).find(a=>a.kind==='show');
  return { item_id:it.id, title:it.title||"", overview:it.overview||"", year:it.year||null,
    air_date:it.air_date||null, runtime_ms:it.runtime_ms||0, kind:it.kind||"",
    show:(it.kind==='episode'&&show)?show.title:(it.show_title||null),
    season:(it.season_number!=null)?it.season_number:null,
    episode:(it.episode_number!=null)?it.episode_number:null };
}
