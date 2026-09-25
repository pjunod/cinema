"use strict";
// The watch browser owns layout and item selection, never the playback owner.
let WATCH=null;
let WATCH_ITEM_PAGE=null;
let WATCH_GENERATION=0;
let WATCH_CLOSE_PROMISE=null;
let WATCH_FULLSCREEN_REQUEST=null;
const WATCH_CONTROLS={};
function watchNarrow(){ return window.innerWidth<=550; }
function watchBrowserMounted(){ return !!(WATCH?.page&&WATCH.slot&&WATCH.slot===document.getElementById("watch-slot")); }
function watchRouteInput(state,input){
  if(!WATCH) return PlaybackPolicy.routeInput(playerInputSurface(),state,input);
  const presentation=WATCH.mode==="full"?"fullscreen":"browser";
  const chrome=watchNarrow()&&WATCH.mode!=="full"?"inline":"overlay";
  return PlaybackPolicy.resolveWatchOutcome(PlaybackPolicy.routeWatchInput("desktop",state,input,presentation,chrome),watchBrowserMounted());
}
function watchSavedSize(){
  try{return localStorage.getItem("plurx_watch_player_size")==="wide"?"wide":"compact";}catch(e){return "compact";}
}
function watchPrepare(fileId,meta){
  WATCH_CLOSE_PROMISE=null;
  const page=meta?._watchPage||WATCH_ITEM_PAGE;
  const eligible=page&&["movie","episode"].includes(page.item.kind)&&page.files.some(f=>String(f.id)===String(fileId))
    &&(!!WATCH||location.hash===`#/item/${exactWireId(page.item)}`);
  // Audiobooks retain their existing modal owner and part continuation.
  if(meta?.audiobook) return null;
  if(!WATCH){
    WATCH={generation:++WATCH_GENERATION,mode:eligible?"slot":"full",size:watchSavedSize(),
      page:eligible?page:null,accepted:null,load:0,seasonLoad:0,route:location.hash,
      browserScroll:window.scrollY,fullOpener:null,observer:null,frame:null};
    if(eligible) watchMount(page);
  }
  const ticket={watch:WATCH,generation:WATCH.generation,page:eligible?page:null,fileId};
  const modal=document.getElementById("modal");
  modal.classList.add("watch-host");
  modal.setAttribute("role","region");modal.removeAttribute("aria-modal");
  document.getElementById("app").inert=false;
  watchLayout();
  return ticket;
}
function watchAccept(ticket,owner){
  if(!ticket||WATCH!==ticket.watch||WATCH.generation!==ticket.generation||PLAYER!==owner||String(owner.fileId)!==String(ticket.fileId))return;
  if(ticket.page){
    WATCH.page=ticket.page;
    WATCH.accepted=exactWireId(ticket.page.item);
    WATCH.route=`#/item/${WATCH.accepted}`;
    history.replaceState(history.state,"",WATCH.route);
    LAST_ROUTE=WATCH.route;
    WATCH_ITEM_PAGE=ticket.page;
    watchRenderTitle();watchRenderPlaying();watchRenderChapters();
  }
  watchLayout();
}
function watchMount(page){
  const main=document.getElementById("main");
  // The player's own ✕ in its bar is the one Close. The heading is the same
  // breadcrumb the item page shows (rendered by watchRenderTitle, since the
  // last crumb changes when another episode is played), so the page still
  // says where it is.
  main.innerHTML=`<section id="watch-browser" class="watch-browser" aria-label="Watch and browse"><div id="watch-head"></div><div class="watch-stage"><div class="watch-picture"><div id="watch-slot" class="watch-slot" aria-label="Player space"></div><section id="watch-rail" class="watch-rail" aria-label="Chapters" hidden></section></div><aside id="watch-title-panel" class="watch-title" aria-label="Now playing"><div id="watch-title"></div><div id="watch-ledger" class="watch-ledger-host"></div></aside></div><div id="watch-band" class="watch-band"><div id="watch-band-title" class="watch-band-title"></div><div id="watch-band-ledger" class="watch-ledger-host"></div></div><section id="watch-lower" class="watch-lower" aria-label="Browse" hidden></section><dialog id="watch-details" class="watch-details"></dialog></section>`;
  WATCH.slot=document.getElementById("watch-slot");
  WATCH.observer=new ResizeObserver(watchScheduleLayout);
  WATCH.observer.observe(document.getElementById("watch-slot"));
  watchRenderTitle();
  watchRenderChapters();
  if(page.item.kind==="episode") watchLoadSeasons(page);
}
function watchScheduleLayout(){
  if(!WATCH||WATCH.frame!=null)return;
  WATCH.frame=requestAnimationFrame(()=>{if(WATCH){WATCH.frame=null;watchLayout();}});
}
function watchLayout(){
  if(!WATCH)return;
  const modal=document.getElementById("modal"),p=document.getElementById("player");
  const browser=document.getElementById("watch-browser"),slot=document.getElementById("watch-slot");
  const full=WATCH.mode==="full",narrow=watchNarrow();
  if(browser)browser.classList.toggle("watch-wide",WATCH.size==="wide"&&!narrow);
  modal.classList.toggle("watch-full",full);
  p.classList.toggle("watch-inline",narrow&&!full);
  p.classList.toggle("watch-compact",!full&&WATCH.size==="compact");
  for(const [id,present] of [["pblarger",!narrow],["pbinfo",full||(!narrow&&WATCH.size==="wide")]]){
    const node=document.getElementById(id)||WATCH_CONTROLS[id];
    if(!node)continue;
    if(present&&!node.isConnected)document.getElementById("ptransport").insertBefore(node,document.getElementById(id==="pblarger"?"pbfs":"pbpip"));
    if(!present&&node.isConnected){WATCH_CONTROLS[id]=node;if(node===document.activeElement)document.getElementById("pbplay").focus({preventScroll:true});node.remove();}
  }
  const larger=document.getElementById("pblarger");
  if(larger){larger.textContent=full?"Smaller":WATCH.size==="compact"?"Larger":"Smaller";larger.setAttribute("aria-label",larger.textContent);}
  if(full){modal.style.cssText="";}
  else if(slot){
    slot.style.height=narrow?`${slot.getBoundingClientRect().width*9/16+132}px`:"";
    // The host is the slot's rectangle and nothing more. It sits under the
    // page's sticky and fixed chrome in the stacking order (app.css, the
    // `.modal.watch-host` z-index), so a picture scrolled up goes behind the
    // header the way the rest of the page does; nothing clips the host, so
    // the menus and the Playback info panel may hang outside the picture.
    const r=slot.getBoundingClientRect();
    modal.style.cssText=`left:${r.left}px;top:${r.top}px;width:${r.width}px;height:${r.height}px`;
  }
  renderPlayerInfo();
  watchPlacePopovers();
}
// The part of the viewport a player popover may occupy. The page keeps its
// chrome above the slot player — the classic sticky header, the theater top
// bar and chips, the phone tab bars — so anything drawn under that chrome is
// hidden. A menu or the Playback info panel that escapes the picture therefore
// stops at the chrome's edge, not the viewport's. A full presentation
// (fullscreen, or the page-filling player) covers the chrome and gets the
// whole viewport; so does the ordinary modal player, which has no WATCH.
function watchPopoverBounds(){
  const bounds={top:0,bottom:window.innerHeight};
  if(!WATCH||WATCH.mode==="full")return bounds;
  const player=document.getElementById("player");
  const pr=player?player.getBoundingClientRect():null;
  // The catalog layout's .px-side is its header only under 900 px; wider it
  // is a full-height column beside the page, which no popover reaches.
  for(const el of document.querySelectorAll("header.top,.th-top,.th-chips,.th-pills,.px-tabs,.px-side")){
    const cs=getComputedStyle(el);
    if(cs.display==="none"||(cs.position!=="fixed"&&cs.position!=="sticky"))continue;
    const r=el.getBoundingClientRect();
    if(r.height<=0||r.width<=0||r.height>window.innerHeight/2)continue;
    if(pr&&(r.right<=pr.left||r.left>=pr.right))continue;
    if(r.top<window.innerHeight/2){
      // A sticky bar only counts once it is stuck (or sits at its stuck
      // offset anyway); scrolled past, it is ordinary page content.
      if(cs.position==="sticky"&&r.top>(parseFloat(cs.top)||0)+1)continue;
      bounds.top=Math.max(bounds.top,r.bottom);
    } else bounds.bottom=Math.min(bounds.bottom,r.top);
  }
  return bounds;
}
// A menu or Playback info panel open while the page scrolls or the slot
// resizes is bounded against geometry that just moved.
function watchPlacePopovers(){
  if(typeof positionStats==="function")positionStats();
  const menu=document.getElementById("pmenu");
  if(menu&&menu.classList.contains("on")&&typeof positionMenu==="function")positionMenu(menu.dataset.kind,menu.classList.contains("low"));
}
function watchResize(){
  if(!WATCH)return;
  watchLayout();watchRenderTitle();
}
function toggleWatchSize(){
  if(!WATCH)return;
  if(WATCH.mode==="full"){WATCH.fullOpener="pblarger";watchReturnBrowser();return;}
  WATCH.size=WATCH.size==="compact"?"wide":"compact";
  try{localStorage.setItem("plurx_watch_player_size",WATCH.size);}catch(e){}
  watchLayout();watchRenderTitle();
}
function watchBeforeFullscreen(){
  if(!WATCH){WATCH_FULLSCREEN_REQUEST=null;return null;}
  WATCH_FULLSCREEN_REQUEST={owner:WATCH,allowed:true};
  WATCH.browserScroll=window.scrollY;
  WATCH.fullOpener=document.activeElement?.id||"pbfs";
  return WATCH_FULLSCREEN_REQUEST;
}
function watchFullscreenChanged(){
  const element=document.fullscreenElement||document.webkitFullscreenElement;
  const video=document.getElementById("video");
  const ownFullscreen=element===document.getElementById("player")||!!video?.webkitDisplayingFullscreen;
  if(element&&!ownFullscreen)return; // Live TV and other surfaces own their exits.
  if(WATCH_FULLSCREEN_REQUEST&&!WATCH_FULLSCREEN_REQUEST.allowed&&ownFullscreen){exitFullscreenAnywhere();return;}
  if(!WATCH)return;
  if(isFullscreenAnywhere())WATCH.mode="full";
  else {WATCH.mode=watchBrowserMounted()?"slot":"full";watchRestoreBrowserFocus();}
  watchLayout();watchRenderTitle();
}
function watchRestoreBrowserFocus(){
  if(!watchBrowserMounted())return;
  window.scrollTo({top:WATCH.browserScroll,behavior:"instant"});
  (document.getElementById(WATCH.fullOpener)||document.getElementById("pbplay"))?.focus({preventScroll:true});
}
function watchReturnBrowser(){
  if(!watchBrowserMounted()){closePlayer();return;}
  if(isFullscreenAnywhere()){exitFullscreenAnywhere();return;}
  WATCH.mode="slot";watchLayout();watchRestoreBrowserFocus();
}
function watchDetach(){
  const w=WATCH;if(!w)return null;
  WATCH=null;++WATCH_GENERATION;
  if(WATCH_FULLSCREEN_REQUEST?.owner===w)WATCH_FULLSCREEN_REQUEST.allowed=false;
  w.observer?.disconnect();if(w.frame!=null)cancelAnimationFrame(w.frame);
  document.getElementById("watch-details")?.close();
  const modal=document.getElementById("modal");modal.classList.remove("watch-host","watch-full");modal.style.cssText="";
  modal.setAttribute("role","dialog");modal.setAttribute("aria-modal","true");
  const p=document.getElementById("player");p.classList.remove("watch-inline","watch-compact");
  for(const id of ["pbinfo","pblarger"]){const node=WATCH_CONTROLS[id];if(node&&!node.isConnected)document.getElementById("ptransport").insertBefore(node,document.getElementById(id==="pbinfo"?"pbpip":"pbfs"));}
  return w;
}
window.addEventListener("resize",watchResize);
window.addEventListener("scroll",watchScheduleLayout,{passive:true});
document.addEventListener("fullscreenchange",watchFullscreenChanged);
document.addEventListener("webkitfullscreenchange",watchFullscreenChanged);
