"use strict";
// Meridian owns navigation geometry. Palette roles come from applyTheme();
// cards, Home choices, detail metadata and Play/Resume remain shared Cinema code.
let MERIDIAN_LIB_SIGNATURE=null;
let MERIDIAN_LIBRARIES=null,MERIDIAN_COMPACT=false,MERIDIAN_PANEL=null;
let MERIDIAN_WIRED=false;
let MERIDIAN_NAV_REVISION=0,MERIDIAN_FOCUS_TICKET=null,MERIDIAN_AUTH=null;
const MERIDIAN_FOCUS_MEMORY=new Map();
function meridianProfile(){
  if(MERIDIAN_AUTH===AUTH_GENERATION)return;
  MERIDIAN_AUTH=AUTH_GENERATION;MERIDIAN_LIBRARIES=null;MERIDIAN_LIB_SIGNATURE=null;MERIDIAN_FOCUS_MEMORY.clear();MERIDIAN_FOCUS_TICKET=null;
  meridianClosePanel(false);
}
function meridianLibraries(){meridianProfile();return MERIDIAN_LIBRARIES||LIBS_CACHE||[];}
function meridianPrimary(){
  const route=location.hash||"#/";
  if(/^#\/(library|category)\//.test(route))return "libraries";
  if(route.startsWith("#/item/"))return NAV_ORIGIN&&/^#\/(library|category)\//.test(NAV_ORIGIN.href)?"libraries":"home";
  if(route.startsWith("#/search/"))return "search";
  if(route==="#/")return "home";
  return route.split("/")[1]||"home";
}
function meridianDestinations(){
  const entries=[{id:"home",label:"Home",icon:"home",href:"#/"}];
  for(const [id,label,icon] of [["shared","Shared libraries","folder"],["live-tv","Live TV","tv"],["recordings","Recordings","cam"],["library-channels","Library channels","tv"]]){
    if(featureVisible(id))entries.push({id,label,icon,href:"#/"+id});
  }
  entries.push({id:"activity",label:"Activity",icon:"act",href:"#/activity"});
  if(ME&&ME.is_admin)entries.push({id:"settings",label:"Settings",icon:"gear",href:"#/settings"});
  entries.push({id:"device",label:"Device preferences",icon:"gear",href:"#/settings/developer"},{id:"remote",label:"Phone remote",icon:"tv",href:"#/remote"});
  return entries;
}
function meridianLink(entry){
  const current=entry.source?location.hash===entry.href:meridianPrimary()===entry.id;
  return `<a href="${esc(entry.href)}" data-meridian-destination="${esc(entry.id)}"${entry.source?' data-meridian-source="true"':''}${current?' aria-current="page"':''} onclick="return meridianNavigate(event,this)">${catalogIcon(entry.icon)}<span class="md-label">${esc(entry.label)}</span></a>`;
}
function meridianSources(){
  return meridianLibraries().map(l=>({id:"library:"+l.id,label:l.name,href:"#/library/"+l.id,source:true,icon:catalogLibGlyph(l.kind,l.is_anime)}));
}
function meridianNavHtml(panel=false){
  const destinations=meridianDestinations(),home=destinations[0];
  const primary=meridianLink(home)+`<button class="ghost" type="button" data-meridian-search onclick="meridianSearch()" aria-label="Search">${catalogIcon("search")}<span class="md-label">Search</span></button><button class="ghost" type="button" data-meridian-open onclick="meridianOpenPanel(this,'libraries')" aria-label="Libraries">${catalogIcon("stack")}<span class="md-label">Libraries</span></button>`;
  const sources=meridianSources();
  return primary+(sources.length?`<div class="md-divider"></div><div class="md-section">Your libraries</div>${sources.map(meridianLink).join("")}`:"")+`<div class="md-divider"></div>`+destinations.slice(1).filter(e=>panel||["live-tv","recordings"].includes(e.id)).map(meridianLink).join("")+(panel?`<button class="ghost" type="button" data-meridian-help onclick="meridianShowHelp()">${catalogIcon("book")}<span>Help · Report a problem</span></button>`:`<button class="ghost" type="button" data-meridian-open onclick="meridianOpenPanel(this,'all')" aria-label="More destinations">${catalogIcon("folder")}<span class="md-label">More</span></button>`);
}
function meridianFillNav(){
  const nav=document.getElementById("meridian-nav");if(nav)nav.innerHTML=meridianNavHtml();
  if(MERIDIAN_PANEL&&MERIDIAN_PANEL.dialog.open)meridianClosePanel(true);
}
function meridianLibsChanged(){MERIDIAN_LIBRARIES=null;MERIDIAN_LIB_SIGNATURE=null;meridianFillNav();}
function meridianNavChanged(){meridianFillNav();}
/** @returns {"applied"} */
function meridianShowHelp(){meridianClosePanel(false);showDiagnostics();return "applied";}
function meridianSearch(){
  meridianClosePanel(false);
  const shell=document.querySelector("[data-layout-shell]");if(shell)shell.classList.add("md-search-open");
  const q=document.getElementById("q");if(q)q.focus();
}
function meridianToggleNavigation(opener){
  if(surfaceClass()==="tv"||innerWidth<1100)return meridianOpenPanel(opener,"all");
  MERIDIAN_COMPACT=!MERIDIAN_COMPACT;
  document.querySelector("[data-layout-shell]")?.classList.toggle("md-compact",MERIDIAN_COMPACT);
  opener.setAttribute("aria-expanded",String(!MERIDIAN_COMPACT));
}
function meridianChrome(active,inner){
  meridianProfile();
  const search=captureSearchFocus(),home=meridianPrimary()==="home",library=meridianPrimary()==="libraries",searching=meridianPrimary()==="search";
  const getapp=installAvailable()?`<button class="ghost sm getappbtn" onclick="showInstall()">Get app</button>`:"";
  mountLayoutFrame(`<div class="md-shell${MERIDIAN_COMPACT?' md-compact':''}${searching||search?' md-search-open':''}" data-layout-shell>
    <aside class="md-side" aria-label="Cinema navigation"><a class="logo md-brand" href="#/">${APP_NAME}</a><nav class="md-nav" id="meridian-nav" aria-label="Sources">${meridianNavHtml()}</nav><button class="ghost md-account" onclick="toggleProfileMenu(event)" aria-label="Account"><span class="pavatar">${esc((ME&&ME.username?ME.username[0]:"?").toUpperCase())}</span><span class="md-label">${esc(ME?ME.username:"")}</span></button></aside>
    <header class="md-top"><button class="ghost md-opener" type="button" data-meridian-open aria-label="Open navigation" aria-haspopup="dialog" aria-expanded="${!MERIDIAN_COMPACT}" onclick="meridianToggleNavigation(this)">☰</button><a class="logo md-mobile-brand" href="#/">${APP_NAME}</a>
      <input class="search" id="q" type="search" name="q" autocomplete="off" placeholder="Search your libraries" aria-label="Search ${APP_NAME}" value="${esc(getQ())}"><span class="spacer"></span>
      <button class="dvr-global" id="dvr-global" onclick="location.hash='#/activity'" aria-label="Recording status"></button><span class="activity" id="activity" onclick="location.hash='#/activity'"></span>
      <div class="md-pop"><button class="ghost sm lookbtn" onclick="toggleLookMenu(event)" aria-label="Appearance" aria-haspopup="menu"><span class="tsw"></span><span class="md-appearance-label">Appearance</span></button><div class="amenu lookmenu" id="lookmenu"></div></div>${getapp}
      <div class="md-pop"><button class="ghost profile" onclick="toggleProfileMenu(event)" aria-label="Account"><span class="pavatar">${esc((ME&&ME.username?ME.username[0]:"?").toUpperCase())}</span></button><div class="amenu" id="profilemenu"></div></div>
    </header><main id="main">${inner}</main>
    <nav class="md-bottom" aria-label="Primary"><a href="#/"${home?' aria-current="page"':''} data-meridian-destination="home" onclick="return meridianNavigate(event,this)">${catalogIcon("home")}<span>Home</span></a><button class="ghost" type="button" data-meridian-open${library?' aria-current="page"':''} onclick="meridianOpenPanel(this,'libraries')">${catalogIcon("stack")}<span>Libraries</span></button><button class="ghost" type="button"${searching?' aria-current="page"':''} onclick="meridianSearch()">${catalogIcon("search")}<span>Search</span></button><button class="ghost" type="button" data-meridian-open onclick="meridianOpenPanel(this,'all')">${catalogIcon("folder")}<span>More</span></button></nav>
  </div>`);
  wireSearchInput(document.getElementById("q"));restoreSearchFocus(search);navKeyboardWireOnce();meridianWireOnce();
  if(!LAYOUT_RECOMPOSING)pollActivity();
}
function meridianHomeBody(page){
  if(!page.previewsPending&&!page.previewsError){
    const signature=JSON.stringify(page.libs.map(l=>[l.id,l.name,l.kind]));
    if(signature!==MERIDIAN_LIB_SIGNATURE){MERIDIAN_LIBRARIES=page.libs;MERIDIAN_LIB_SIGNATURE=signature;meridianFillNav();}
  }
  return `<div class="md-home"><h1 class="md-page-title">Home</h1>${theaterHomeBody(page)}</div>`;
}
function meridianItemBody(page){return `<div class="md-detail">${classicItemBody(page)}</div>`;}
function meridianLibraryShell(model){return `<div class="md-library">${classicLibraryShell(model)}</div>`;}
function meridianVisible(element){return !!(element&&element.isConnected&&!element.closest("[hidden],[inert]")&&element.getClientRects().length&&getComputedStyle(element).visibility!=="hidden");}
function meridianRememberFocus(){
  const selector=layoutFocusSelector(document.activeElement);
  if(selector&&document.getElementById("main")?.contains(document.activeElement))MERIDIAN_FOCUS_MEMORY.set(location.hash,selector);
}
function meridianPanelActive(){return !!(MERIDIAN_PANEL&&MERIDIAN_PANEL.dialog.open&&MERIDIAN_PANEL.dialog.isConnected&&CINEMA_REMOTE_PRESENTATION?.root===MERIDIAN_PANEL.dialog);}
function meridianOpenPanel(opener,mode="all"){
  closeMenus();
  if(meridianPanelActive()){if(MERIDIAN_PANEL.mode===mode)return;meridianClosePanel(false);}
  if(cinemaRemoteRestrictedOverlay()||cinemaRemoteVod())return;
  meridianRememberFocus();
  const dialog=document.createElement("dialog");dialog.className="md-panel";dialog.setAttribute("aria-label",mode==="libraries"?"Libraries":"Cinema navigation");
  dialog.innerHTML=`<div class="md-panel-head"><strong>${mode==="libraries"?"Libraries":"Cinema"}</strong><button class="ghost" type="button" aria-label="Close navigation">✕</button></div><nav class="md-nav" aria-label="All destinations">${mode==="libraries"?meridianSources().map(meridianLink).join("")+(featureVisible("shared")?meridianLink({id:"shared",label:"Shared libraries",icon:"folder",href:"#/shared"}):""):meridianNavHtml(true)}</nav>`;
  document.body.append(dialog);const revision=++MERIDIAN_NAV_REVISION;
  const scope=opener.closest(".md-side,.md-top,.md-bottom")||document.getElementById("main");
  const controls=Array.from(scope.querySelectorAll("a,button,input")).filter(meridianVisible),at=controls.indexOf(opener);
  const returns=[...controls.slice(at+1),...controls.slice(0,Math.max(0,at)).reverse()].map(layoutFocusSelector).filter(Boolean);
  MERIDIAN_PANEL={dialog,opener,route:location.hash,revision,mode,returns};
  const close=/** @type {HTMLButtonElement} */(dialog.querySelector('[aria-label="Close navigation"]'));close.onclick=()=>meridianClosePanel(true);
  dialog.addEventListener("cancel",event=>{event.preventDefault();meridianClosePanel(true);});
  // meridian-dialog-key-adapter:begin
  dialog.addEventListener("keydown",event=>{if(event.key==="Escape"){event.preventDefault();event.stopImmediatePropagation();meridianClosePanel(true);}});
  // meridian-dialog-key-adapter:end
  const entries=Array.from(dialog.querySelectorAll("a[data-meridian-destination]")).map((element,index)=>({id:"meridian:destination:"+index,element,label:element.textContent.trim(),activate:()=>meridianNavigate(null,element,true)}));
  entries.push({id:"meridian:close",element:close,label:"Close navigation",activate:()=>{meridianClosePanel(true);return "applied";}});
  const searchButton=dialog.querySelector("[data-meridian-search]");
  if(searchButton)entries.push({id:"meridian:search",element:searchButton,label:"Search",activate:()=>{meridianSearch();return "applied";}});
  const libraryButton=dialog.querySelector("[data-meridian-open]");
  if(libraryButton)entries.push({id:"meridian:libraries",element:libraryButton,label:"Libraries",activate:()=>{meridianClosePanel(false);meridianOpenPanel(opener,"libraries");return "applied";}});
  const helpButton=dialog.querySelector("[data-meridian-help]");
  if(helpButton)entries.push({id:"meridian:help",element:helpButton,label:"Help · Report a problem",activate:meridianShowHelp});
  dialog.showModal();
  cinemaRemoteOwnPresentation(dialog,"home",()=>meridianPanelActive()&&MERIDIAN_PANEL.revision===revision,()=>meridianClosePanel(true),entries);
  cinemaRemoteBind();
  const selected=/** @type {HTMLElement} */(dialog.querySelector('[aria-current="page"]')||dialog.querySelector("a")||close);selected.focus();
}
function meridianClosePanel(restore=true){
  const panel=MERIDIAN_PANEL;if(!panel)return;
  MERIDIAN_PANEL=null;MERIDIAN_NAV_REVISION++;MERIDIAN_FOCUS_TICKET=null;
  if(CINEMA_REMOTE_PRESENTATION?.root===panel.dialog)CINEMA_REMOTE_PRESENTATION=null;
  panel.dialog.close();panel.dialog.remove();CinemaRemote.invalidate("meridian_panel_closed");
  if(restore&&panel.route===location.hash){
    const remembered=MERIDIAN_FOCUS_MEMORY.get(location.hash);
    const target=[panel.opener,...panel.returns.map(selector=>document.querySelector(selector)),remembered?document.querySelector(remembered):null].find(meridianVisible);
    const recovery=target||Array.from(document.querySelectorAll('[data-meridian-open],#q')).find(meridianVisible);
    if(meridianVisible(recovery))recovery.focus({preventScroll:true});
  }
}
function meridianNavigate(event,element,companion=false){
  if(event){event.preventDefault();event.stopPropagation();}
  const href=element.getAttribute("href"),id=element.dataset.meridianDestination;
  if(!meridianDestinations().some(e=>e.id===id&&e.href===href)&&!meridianSources().some(e=>e.id===id&&e.href===href))return event?false:"stale_context";
  const current=element.hasAttribute("data-meridian-source")?location.hash===href:meridianPrimary()===id;
  meridianRememberFocus();
  const remembered=MERIDIAN_FOCUS_MEMORY.get(location.hash),returnElement=remembered?document.querySelector(remembered):null;
  const returnItem=returnElement?.closest("[data-remote-item]")?.getAttribute("data-remote-item");
  meridianClosePanel(false);
  const revision=++MERIDIAN_NAV_REVISION;
  MERIDIAN_FOCUS_TICKET={route:current?location.hash:href,revision,generation:PAGE_RENDER_GENERATION+(!current&&location.hash!==href?1:0)};
  if(!current&&location.hash!==href){
    if(companion)cinemaRemoteRouteIntent(href,returnItem?"item:"+returnItem:null);
    else location.hash=href;
  }
  meridianRealizeFocus();
  return event?false:"applied";
}
function meridianRealizeFocus(){
  const ticket=MERIDIAN_FOCUS_TICKET;if(!ticket||ticket.revision!==MERIDIAN_NAV_REVISION||ticket.route!==location.hash||ticket.generation!==PAGE_RENDER_GENERATION)return;
  const main=document.getElementById("main");if(!main)return;
  const owner=LAYOUT_PRESENTATION;
  if(owner){if(owner.route!==ticket.route||owner.generation!==ticket.generation||!owner.ready())return;}
  else if(main.dataset.pageGeneration!==String(ticket.generation)||main.dataset.phase==="shell")return;
  const remembered=MERIDIAN_FOCUS_MEMORY.get(ticket.route);
  const candidates=[remembered?document.querySelector(remembered):null,...main.querySelectorAll('[data-remote-item],.btnplay,button,input,a[href]')];
  const target=candidates.find(meridianVisible);if(!target)return;
  MERIDIAN_FOCUS_TICKET=null;target.focus({preventScroll:true});
}
function meridianSafeChromeActive(){
  return layoutId()==="meridian"&&!cinemaRemoteSafeRoute(location.hash||"#/")&&location.hash!=="#/live-tv"&&
    !!document.querySelector(".md-top")&&!cinemaRemoteRestrictedOverlay()&&!cinemaRemoteVod()&&!cinemaRemoteLive();
}
function meridianBindRestrictedChrome(){
  if(!meridianSafeChromeActive())return false;
  const root=document.querySelector(".md-top");
  // Ancillary content stays restricted. This scope exposes only its explicit
  // navigation opener, no text nonce or settings/recording/content actions.
  CinemaRemote.registerScope({id:"meridian:chrome:"+location.hash,root,category:"home",back:()=>"restricted_surface",home:()=>cinemaRemoteGo("#/")});
  const opener=root.querySelector("[data-meridian-open]");
  if(opener)cinemaRemoteRegister("meridian:open:0",opener,"Open navigation",()=>{meridianOpenPanel(opener,"all");return "applied";});
  return true;
}
function meridianBindChrome(){
  for(const [index,element] of Array.from(document.querySelectorAll('[data-layout-shell] [data-meridian-open]')).entries())cinemaRemoteRegister("meridian:open:"+index,element,element.getAttribute("aria-label")||element.textContent.trim(),()=>{meridianOpenPanel(element,element.textContent.trim()==="Libraries"?"libraries":"all");return "applied";});
}
function meridianStickyFloor(){const top=document.querySelector(".md-top");return top?Math.max(0,top.getBoundingClientRect().bottom):0;}
LAYOUTS.meridian={name:"Meridian",surfaces:["desktop","mobile","tv"],chrome:meridianChrome,views:{home:meridianHomeBody,item:meridianItemBody,library:{shell:meridianLibraryShell}},navChanged:meridianNavChanged,libsChanged:meridianLibsChanged,stickyFloor:meridianStickyFloor};
function meridianWireOnce(){
  if(MERIDIAN_WIRED)return;MERIDIAN_WIRED=true;
  document.addEventListener("focusin",()=>{if(layoutId()==="meridian")meridianRememberFocus();});
  new MutationObserver(()=>meridianRealizeFocus()).observe(document.getElementById("app"),{childList:true,subtree:true});
  for(const type of ["keydown","pointerdown"])document.addEventListener(type,event=>{if(event.isTrusted&&MERIDIAN_FOCUS_TICKET){MERIDIAN_FOCUS_TICKET=null;MERIDIAN_NAV_REVISION++;}},true);
  // meridian-opener-key-adapter:begin
  document.addEventListener("keydown",event=>{if(layoutId()==="meridian"&&event.key==="ArrowLeft"&&!event.defaultPrevented&&document.activeElement?.matches("[data-meridian-open]")){event.preventDefault();meridianOpenPanel(document.activeElement,"all");}});
  // meridian-opener-key-adapter:end
}
