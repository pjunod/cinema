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
      ? PlaybackPolicy.routeInput(playerInputSurface(),playerInputState(),"idle")
      : "ignore";
    if(outcome==="ignore"){ PLAYER.idleTimer=setTimeout(idle,4000); return; }
    applyPlayerOutcome(outcome,{direction:"idle"});
  },4000);
}
// A pointer that can hover is a desktop; a coarse one follows the touch
// table, on the same page. `tap_surface` and the scrub gestures are the two
// places where the tables disagree, and an iPad opening the desktop page used
// to get the desktop answer for both.
function playerInputSurface(){ return coarsePointer()?"touch":"desktop"; }
// One pending position, however it was started: arrows and J/L set
// `_seekPending`, a pointer drag sets `_seekPreview`. Either one means the
// bar is showing a time the film is not at yet.
function playerSeekPending(){ return !!(PLAYER&&(PLAYER._seekPending!=null||PLAYER._seekPreview!=null)); }
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
  const on=!playerInfoOn();
  try{ localStorage.setItem("plurx_playerinfo", on?"1":"0"); }catch(e){}
  renderPlayerInfo(); playerActivity();
  toast(on?"Title info shown":"Title info hidden");
}
function renderPlayerInfo(){
  const c=document.getElementById("pinfo"); if(!c) return;
  const b=document.getElementById("pbinfo"); if(b) b.classList.toggle("on", playerInfoOn());
  const m=(PLAYER&&PLAYER.meta)||null;
  if(!m || !playerInfoOn()){ c.innerHTML=""; return; }
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
    +(m.return_channel_id?`<div class="lc-actions"><button onclick="returnToLibraryChannel()">Return to channel</button></div>`:``);
}
// Build the panel's payload from an item (plus its ancestors, so an episode can
// name its show). Kept next to the renderer so the two stay in step.
function playerMeta(it, ancestors){
  if(!it) return null;
  const show=(ancestors||[]).find(a=>a.kind==='show');
  return { title:it.title||"", overview:it.overview||"", year:it.year||null,
    air_date:it.air_date||null, runtime_ms:it.runtime_ms||0, kind:it.kind||"",
    show:(it.kind==='episode'&&show)?show.title:(it.show_title||null),
    season:(it.season_number!=null)?it.season_number:null,
    episode:(it.episode_number!=null)?it.episode_number:null };
}
