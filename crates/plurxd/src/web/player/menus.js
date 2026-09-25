"use strict";
// ---- audio-language + subtitle menus --------------------------------------
const LANGS={eng:"English",en:"English",jpn:"Japanese",ja:"Japanese",spa:"Spanish",es:"Spanish",
  fre:"French",fra:"French",fr:"French",ger:"German",deu:"German",de:"German",ita:"Italian",it:"Italian",
  por:"Portuguese",pt:"Portuguese",rus:"Russian",ru:"Russian",kor:"Korean",ko:"Korean",
  chi:"Chinese",zho:"Chinese",zh:"Chinese",hin:"Hindi",ara:"Arabic",nld:"Dutch",swe:"Swedish",pol:"Polish",und:"Unknown"};
function langName(c){ return c?(LANGS[c.toLowerCase()]||c.toUpperCase()):null; }
function audioLabelMenu(a,i){ return (langName(a.language)||("Track "+(i+1)))+(a.channels?" · "+fmtChannels(a.channels):"")+(a.title?" · "+a.title:""); }
// Which subtitle tracks this player has to have BURNED into the picture.
//
// `text` is the right field here and `native` is not, and the difference is a
// functional regression waiting to be introduced. The server sends both, and
// they answer different questions: `native` (= subrip/srt/webvtt/vtt) is the
// narrow set that converts cleanly enough to be published as a WebVTT HLS
// *rendition*, while `text` (= not a bitmap) is everything that can be
// extracted as text at all — which also covers mov_text and styled ASS/SSA.
//
// HLS sessions use `native` for their rendition path; direct and progressive
// delivery still use `text` and the whole-track `/files/.../subs/...` route.
// Keeping the predicates separate preserves mov_text and styled ASS/SSA on
// those non-HLS routes without asking the HLS master for a rendition it cannot
// publish.
function subNeedsBurn(s){ return !!s && s.text===false; }
// A bitmap track is labelled as one. Selecting it restarts the stream as a
// transcode, and somebody choosing between two English tracks deserves to know
// which one costs that — the alternative is an unexplained pause and a picture
// they cannot switch off without another one.
function subLabelMenu(s){
  return (langName(s.language)||("Subtitle "+(s.index+1)))
    +(s.forced?" (forced)":"")
    +(s.title?" · "+s.title:"")
    +(subNeedsBurn(s)?" · burned in":"");
}
function setupTrackMenus(){
  const m=document.getElementById("pmenu");
  if(m){ closeMenu(false); m.innerHTML=""; }
  // Track selection has one door per option in the transport row. Duplicating
  // the same menus in the header made the focus order depend on the client.
  const multi=!!(PLAYER.audio&&PLAYER.audio.length>1);
  const pa=document.getElementById("pbaudio"); if(pa) pa.style.display=multi?"":"none";
  // Subtitles are offered in every playback mode. A stream that starts at an
  // offset (transcode, copy-HLS, a resumed remux) shifts the cues to match —
  // see setSub() — so the track lines up wherever playback actually began.
  const has=!!(PLAYER.subs&&PLAYER.subs.length);
  const cc=document.getElementById("pbsubs"); if(cc) cc.style.display=has?"":"none";
  pbSyncSubIcon();
}
// Light the CC button while a subtitle track is showing.
function pbSyncSubIcon(){ const cc=document.getElementById("pbsubs");
  if(cc) cc.classList.toggle("on", !!(PLAYER&&PLAYER.curSub>=0)); }
// `low` = opened from the bottom transport, so the menu anchors above that
// button instead of jumping to the top-right corner of the screen.
function closeMenu(restoreFocus=true){
  const m=document.getElementById("pmenu"); if(!m) return;
  m.classList.remove("on","low");
  m.style.left=""; m.style.top=""; m.style.bottom=""; m.style.maxHeight="";
  const opener=PLAYER&&PLAYER._menuOpener;
  if(opener&&opener.setAttribute) opener.setAttribute("aria-expanded","false");
  if(restoreFocus&&opener&&opener.focus) opener.focus({preventScroll:true});
  if(PLAYER) PLAYER._menuOpener=null;
  playerActivity();
}
function settingsMenuHtml(){
  return `<div class="amsec">Playback settings</div>
    <button id="pmenu-autoskip" role="menuitemcheckbox" aria-checked="${autoskipOn()}" class="${autoskipOn()?'sel':''}" onclick="togglePlayerAutoskip(); refreshPlayerSettingsMenu('pmenu-autoskip')">Auto-skip intro and credits</button>
    <button id="pmenu-autoplay" role="menuitemcheckbox" aria-checked="${autoNextOn()}" class="${autoNextOn()?'sel':''}" onclick="togglePlayerAutonext(); refreshPlayerSettingsMenu('pmenu-autoplay')">Autoplay next episode</button>
    ${syncMenuHtml()}`;
}
function refreshPlayerSettingsMenu(focusId){
  const m=document.getElementById("pmenu");
  if(!m||!m.classList.contains("on")||m.dataset.kind!=="settings") return;
  m.innerHTML=settingsMenuHtml();
  m.querySelectorAll("button").forEach(button=>{
    if(!button.hasAttribute("role")) button.setAttribute("role","menuitem");
  });
  const target=focusId&&document.getElementById(focusId);
  if(target) target.focus({preventScroll:true});
}
function toggleMenu(kind,low){
  const m=document.getElementById("pmenu"); if(!m) return;
  if(m.classList.contains("on") && m.dataset.kind===kind){ closeMenu(); return; }
  if(m.classList.contains("on")) closeMenu(false);
  // On a phone the two panels take turns: each wants most of the screen, and
  // stacking them left neither one dismissible. On a mouse there is room for
  // both, and wanting both is the normal case — you open the readout to watch
  // what a quality change does to it, and having the menu dismiss it meant the
  // numbers you came to compare were gone before you could change anything.
  // positionMenu() slides the menu clear of the panel rather than over it.
  const anchorId=MENU_ANCHORS[kind]&&MENU_ANCHORS[kind][low?1:0];
  // Read the opener before anything else moves focus: closing the info panel
  // below focuses its own button, and recording that meant Escape handed
  // focus back to the wrong control.
  const opener=document.activeElement&&document.activeElement!==document.body
    ?document.activeElement:(anchorId?document.getElementById(anchorId):null);
  const sov=document.getElementById("statsov");
  if(coarsePointer() && sov && sov.classList.contains("on")) toggleStats();
  if(PLAYER) PLAYER._menuOpener=opener;
  if(opener&&opener.setAttribute) opener.setAttribute("aria-expanded","true");
  m.dataset.kind=kind; m.classList.toggle("low",!!low);
  if(kind==="audio"){
    m.innerHTML=PLAYER.audio.map((a,i)=>`<button class="${PLAYER.curAudio===i?'sel':''}" onclick="switchAudio(${i})">${esc(audioLabelMenu(a,i))}</button>`).join("");
  } else if(kind==="settings"){
    m.innerHTML=settingsMenuHtml();
  } else if(kind==="quality"){
    // If a remembered measurement is why Auto is not giving this viewer the
    // source, say so HERE — this menu is where somebody goes when they wonder
    // why the picture is soft — and give them the button rather than a
    // console incantation.
    const lim=PLAYER?decodeLimitFor(PLAYER.source||{}):null;
    // Two sentences, because during a re-test the entry still exists but is
    // NOT what Auto is doing right now — telling somebody their picture is
    // being transcoded while they are watching the source is worse than
    // saying nothing.
    const limWhat=(PLAYER&&PLAYER.decodeRetest)
      ? `Auto is re-testing it right now — if this plays cleanly it clears itself.`
      : `Auto transcodes it; Original bypasses the learned limit.`;
    const limView=lim?((PLAYER&&PLAYER.learnedLimit)||PlaybackPolicy.learnedDecodeLimitView({
      source:(PLAYER&&PLAYER.source)||{},limit:lim,
      ordinaryRange:PLAYER&&PLAYER.deliveredRange,
      deliveredRange:PLAYER&&PLAYER.deliveredRange
    })):null;
    const rangeNote=limView&&limView.rangeConsequence
      ? ` <b>${esc(limView.rangeConsequence)}</b>: the learned client-performance limit replaces the ordinary HDR route with SDR.`
      : "";
    const limNote=lim?`<div class="amsec">Learned client-performance limit</div>
      <div class="menunote">On ${esc(limView.loadLabel)} this browser
        ${esc(decodeLimitWhy(lim))}, ${esc(agoLabel(Math.round(lim.at/1000)))}.
        ${limWhat}${rangeNote}</div>
      <button onclick="forgetDecodeLimitHere()">Forget this measurement</button>`:"";
    m.innerHTML=`<div class="amsec">Quality</div>`+qualityOptions().map(([v,l])=>{
      const note=v==="auto"?" — automatic":v==="original"?" — max, no transcode":"";
      return `<button class="${playQuality()===v?'sel':''}" onclick="setQuality('${v}')">${l}${note}</button>`;
    }).join("")+limNote;
  } else {
    m.innerHTML=`<button class="${PLAYER.curSub<0?'sel':''}" onclick="setSub(-1)">Off</button>`+
      PLAYER.subs.map(s=>`<button class="${PLAYER.curSub===s.index?'sel':''}" onclick="setSub(${s.index})">${esc(subLabelMenu(s))}</button>`).join("");
  }
  m.querySelectorAll("button").forEach(button=>{
    if(!button.hasAttribute("role")) button.setAttribute("role","menuitem");
  });
  m.classList.add("on");
  positionMenu(kind,low);
  const initial=m.querySelector(".sel")||m.querySelector("button");
  if(initial){
    initial.focus({preventScroll:true});
    // The menu scrolls now; the entry that took focus (the ✓ one) has to be
    // in the part that shows. Menu-local — scrollIntoView could move the
    // page under a fixed host.
    const above=initial.offsetTop, below=above+initial.offsetHeight;
    if(above<m.scrollTop) m.scrollTop=above;
    else if(below>m.scrollTop+m.clientHeight) m.scrollTop=below-m.clientHeight;
  }
  playerActivity();
}

// Which control owns each menu. The menu is one shared element, so the button
// that opened it is the only thing that says where it belongs.
const MENU_ANCHORS={audio:[null,"pbaudio"],subs:[null,"pbsubs"],
                    quality:[null,"pbquality"],settings:[null,"pbsettings"]};

// Put the menu under (or above) the button that opened it.
//
// It used to fly to the top-right corner of the player whatever you clicked,
// because the stylesheet pins one shared element to `right:20px`. With four
// buttons spread across the bar that means the menu for Audio opens nowhere
// near Audio, and on a wide screen it is a long way to look.
//
// Horizontal: align the menu's left edge with the button's, then clamp so it
// cannot hang off either side. Vertical: below the button for the top bar,
// above it for the transport — measured, because the top row wraps to two
// lines on a narrow window and a fixed offset then lands on top of the row it
// was supposed to sit under.
//
// Height: a menu is as tall as its entries until the room runs out, and then
// it scrolls. The room is measured from the button to the edge of what the
// viewer can see — the viewport in a full presentation, the viewport minus
// the page's own chrome when the player sits in the watch slot (see
// watchPopoverBounds) — never from the picture, which a subtitle menu with
// thirty tracks was taller than, so its top was cut off and half of it could
// not be chosen.
// The clamp is for a button within a hand's width of the ceiling — a picture
// scrolled almost under the header — where the menu is moot anyway; it
// corrects itself on the next scroll frame.
const MENU_MIN_HEIGHT=120;
function popoverBounds(){
  if(typeof watchPopoverBounds==="function") return watchPopoverBounds();
  return {top:0,bottom:window.innerHeight};
}
function positionMenu(kind,low){
  const m=document.getElementById("pmenu"), player=document.getElementById("player");
  if(!m||!player) return;
  const anchor=MENU_ANCHORS[kind]&&MENU_ANCHORS[kind][low?1:0];
  const btn=anchor?document.getElementById(anchor):null;
  // No anchor (or a hidden one): leave the stylesheet's corner placement,
  // which is still a sane place for a menu to be.
  if(!btn||!btn.offsetParent){ m.style.left=m.style.right=m.style.top=m.style.bottom=m.style.maxHeight=""; return; }
  const pr=player.getBoundingClientRect(), br=btn.getBoundingClientRect(), bounds=popoverBounds();
  const gap=8, edge=12;
  // A full presentation on a touch screen keeps the top bar clear of a menu
  // grown upward — the stylesheet's coarse band promises Close stays
  // reachable with a panel open. In the slot the bar is at the picture's
  // top and the menu may pass it (raised above it there), so the room above
  // the picture is not wasted on a tablet.
  const slotted=typeof WATCH!=="undefined"&&!!WATCH&&WATCH.mode!=="full";
  if(low&&!slotted&&coarsePointer()){
    const bar=document.getElementById("pbar");
    if(bar) bounds.top=Math.max(bounds.top, bar.getBoundingClientRect().bottom+gap);
  }
  // Left edges aligned, flipping to right edges only when that would hang off
  // the player. Consistency is the point: choosing per-button by which half of
  // the screen it sits in made two ADJACENT buttons open their menus opposite
  // ways, which looks like a bug even though each one was individually
  // sensible.
  let want=br.left-pr.left;
  if(want+m.offsetWidth > pr.width-edge) want=br.right-pr.left-m.offsetWidth;
  const left=Math.max(edge, Math.min(want, pr.width-m.offsetWidth-edge));
  m.style.left=left+"px";
  m.style.right="auto";
  if(low){
    m.style.top="auto";
    m.style.bottom=Math.max(edge, pr.bottom-br.top+gap)+"px";
    m.style.maxHeight=Math.max(MENU_MIN_HEIGHT, Math.floor(br.top-gap-bounds.top-edge))+"px";
  } else {
    m.style.bottom="auto";
    m.style.top=Math.max(edge, br.bottom-pr.top+gap)+"px";
    m.style.maxHeight=Math.max(MENU_MIN_HEIGHT, Math.floor(bounds.bottom-edge-br.bottom-gap))+"px";
  }
  dodgeStats(m, pr, edge);
}
// The two panels may be open together on a mouse, and the buttons that open
// the menus sit directly above where the readout lives — so the menu's natural
// place is on top of the numbers somebody just asked to keep watching. Slide it
// clear when there is room beside the panel, preferring the side away from it.
//
// When there is no room, leave it overlapping: the menu has the higher
// z-index, and a menu you cannot reach is a worse outcome than a number you
// cannot read for a moment.
function dodgeStats(m, pr, edge){
  const ov=document.getElementById("statsov");
  if(!ov || !ov.classList.contains("on")) return;
  const mr=m.getBoundingClientRect(), sr=ov.getBoundingClientRect();
  if(!(mr.left < sr.right && mr.right > sr.left && mr.top < sr.bottom && mr.bottom > sr.top)) return;
  const gap=10;
  const beside=sr.right-pr.left+gap;
  if(beside+mr.width <= pr.width-edge){ m.style.left=beside+"px"; return; }
  const before=sr.left-pr.left-gap-mr.width;
  if(before >= edge) m.style.left=before+"px";
}
function coarsePointer(){ try{ return matchMedia("(pointer:coarse)").matches; }catch(e){ return false; } }
// A menu left open across a resize or a fullscreen change is anchored to where
// its button used to be.
window.addEventListener("resize",()=>{
  const m=document.getElementById("pmenu");
  if(m&&m.classList.contains("on")) positionMenu(m.dataset.kind, m.classList.contains("low"));
},{passive:true});

// ---- quality override -----------------------------------------------------
// Auto follows the automatic ladder; Original forces direct/remux (no video
// transcode); a height forces a transcode. Changing it restarts at position.
function setQuality(q){
  let from="auto"; try{ from=localStorage.getItem("plurx_quality")||"auto"; }catch(e){}
  try{ localStorage.setItem("plurx_quality", q); }catch(e){}
  closeMenu();
  toast("Quality: "+qualityLabel());
  if(PLAYER && PLAYER.fileId){
    clientLog(Object.assign({level:"warn",event:"quality_switch",
      message:`quality ${from} → ${q}`,detail:`from=${from} to=${q}`,reason:"manual"},
      playbackContext()));
    // A manual quality choice replaces an automatic/compatibility recipe.
    // In particular Original must not inherit a pending forced transcode.
    PLAYER.pendingMediaChange=null;
    // The viewer picked a rung by hand. Whatever the automatic controller was
    // asking for is not what they want.
    PLAYER.autoRequestedHeight=null;
    // A quality choice is not a seek, and marking one is what used to bump the
    // control intent generation here -- which retires every outstanding ask,
    // including the one this change is about to make. Only a destination that
    // has not landed is republished.
    if(PLAYER.controlSeek&&!PLAYER.controlSeek.executed){
      const v=document.getElementById("video");
      beginPlaybackControlSeek(PLAYER,positionForPlaybackIntent(v,PLAYER));
    }
    // Publishes the new selection and keeps the incumbent playing while the
    // server builds a successor. `PENDING_ATTEMPT_REASON` is set by the reopen
    // itself now: a switch that commits opens no session, and leaving the
    // reason armed would label the next unrelated open a quality change.
    const running=requestQualityChange(PLAYER,"manual");
    if(running&&running.catch) running.catch(()=>{});
  }
}

