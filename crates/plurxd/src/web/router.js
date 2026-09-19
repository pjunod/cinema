"use strict";
// ---- router ---------------------------------------------------------------
// One page-scoped poller at a time; routing away always clears it.
let PAGE_TIMER=null, PAGE_RENDER_GENERATION=0;
function pagePhaseName(route){
  if(route==="#/live-tv") return "live-tv";
  if(route.startsWith("#/recordings")) return "recordings";
  if(route==="#/library-channels") return "library-channels";
  if(route==="#/activity") return "activity";
  if(route==="#/analysis") return "analysis";
  if(isSettingsRoute(route)) return "settings";
  return "home";
}
function setPagePhase(route,generation,phase){
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route) return false;
  const order={shell:0,content:1,settled:2};
  if(!(phase in order)) return false;
  const main=document.getElementById("main");
  if(!main) return false;
  const page=pagePhaseName(route);
  const sameGeneration=main.dataset.page===page&&main.dataset.pageGeneration===String(generation);
  if(sameGeneration&&order[phase]<=order[main.dataset.phase]) return phase===main.dataset.phase;
  if(phase==="shell"){
    delete main.dataset.pageFailure;
    if(typeof performance!=="undefined"&&performance.getEntriesByType&&performance.clearMarks){
      for(const entry of performance.getEntriesByType("mark")){
        if(entry.name.startsWith("plurx-page-phase:")) performance.clearMarks(entry.name);
      }
    }
  }
  main.dataset.page=page;
  main.dataset.pageGeneration=String(generation);
  main.dataset.phase=phase;
  if(typeof performance!=="undefined"&&performance.mark){
    performance.mark(`plurx-page-phase:${generation}:${page}:${phase}`);
  }
  return true;
}
function setPageFailure(route,generation,failure){
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route) return false;
  const main=document.getElementById("main");
  if(!main||main.dataset.pageGeneration!==String(generation)) return false;
  if(failure) main.dataset.pageFailure=failure;
  else delete main.dataset.pageFailure;
  return true;
}
function setPageTimer(fn,ms,generation=PAGE_RENDER_GENERATION){
  if(generation!==PAGE_RENDER_GENERATION) return;
  clearInterval(PAGE_TIMER);
  const timer=setInterval(()=>{
    if(generation!==PAGE_RENDER_GENERATION){ clearInterval(timer); if(PAGE_TIMER===timer) PAGE_TIMER=null; return; }
    fn();
  },ms);
  PAGE_TIMER=timer;
}
async function render(){
  const generation=++PAGE_RENDER_GENERATION;
  if(!TOKEN||!ME) return boot();
  clearInterval(PAGE_TIMER); PAGE_TIMER=null;
  if(!ACT_TIMER) ACT_TIMER=setInterval(pollActivity,4000);
  // One global timer for due reminders, beside the activity one and for the
  // same reason: the overlay belongs to every page, so a per-page poller would
  // be started and stopped by routes that have nothing to do with it. Thirty
  // seconds against a lead measured in minutes, and no immediate kick — a
  // route change must not become a request.
  if(!DVR_REMINDER_TIMER) DVR_REMINDER_TIMER=setInterval(pollDvrReminders,30000);
  let h=location.hash||"#/";
  if(!h.startsWith("#/recordings")&&h!=="#/activity"){clearTimeout(DVR_PAGE.historyTimer);DVR_PAGE.historyTimer=null;}
  if(h!=="#/live-tv") liveTvLeaveRoute();
  if(!h.startsWith("#/read/")&&READER) destroyReader(true);
  // A join token is bearer authority, so leaving Settings drops the only
  // in-memory copy even before a later Settings visit clears it again.
  if(!isSettingsRoute(h)) forgetJoinToken();
  // A Settings section switch is not a page load: the aggregate the previous
  // section fetched stays cached. Anything else arriving at Settings starts
  // from a clean read. The bare #/settings and the older #/admin are rewritten
  // to the section they land on, so the address bar always names one.
  const stayingInSettings=isSettingsRoute(h)&&isSettingsRoute(LAST_ROUTE);
  LAST_ROUTE=h;
  if(isSettingsRoute(h)&&!settingsRouteTab(h)&&ME.is_admin){
    h=`#/settings/${settingsTab()}`;
    try{ history.replaceState(null,"",h); }catch(e){}
  }
  // Item pages inherit the list you arrived from; everything else replaces it
  // (the list views re-set it as they render) or drops it.
  if(!h.startsWith("#/item/")) NAV_ORIGIN=null;
  dvrRouteChanged(h);
  try{
    if(h.startsWith("#/read/")) await viewReader(h.split("/")[2],h.split("/")[3]);
    else if(h.startsWith("#/item/")) await viewItem(h.split("/")[2]);
    else if(h.startsWith("#/library/")) await viewLibrary(h.split("/")[2]);
    else if(h.startsWith("#/category/")) await viewCategory(decodeURIComponent(h.split("/")[2]||""));
    else if(h.startsWith("#/search/")) await viewSearch(decodeURIComponent(h.split("/")[2]||""));
    else if(h==="#/activity") await viewActivity(generation);
    else if(h==="#/live-tv") await viewLiveTv(generation);
    else if(h.startsWith("#/recordings")) await viewRecordings(generation);
    else if(h==="#/library-channels") await viewLibraryChannels(generation);
    else if(h==="#/analysis") await viewAnalysis(generation);
    else if(isSettingsRoute(h)) await viewSettings(generation,!stayingInSettings);
    else await viewHome(generation);
    // Converted routes restore at the moment their content lands; this catches
    // the ones that do not, and — because it is consume-once — guarantees a
    // pending offset never survives the route that was on screen when it was
    // armed. Without it, switching layout on Settings scrolled the NEXT page.
    if(generation===PAGE_RENDER_GENERATION) restoreScroll();
  }catch(e){
    if(generation===PAGE_RENDER_GENERATION&&e.message!=="unauthorized"){ const m=document.getElementById("main"); if(m){ m.innerHTML=`<div class="empty">${esc(e.message)}</div>`; setPageFailure(h,generation,"render_error"); }else toast(e.message); }
  }
}
let LAST_ROUTE=null;
window.addEventListener("hashchange",render);

if(NATIVE_READER_BOOT){
  document.getElementById("app").innerHTML='<div class="center"><div class="card">Opening reader…</div></div>';
  nativeReaderPost("shell-ready");
}else boot();
