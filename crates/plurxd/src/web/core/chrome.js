"use strict";
// ---- section visibility ---------------------------------------------------
// The top-level nav of every layout reflects the server's enable switches: a
// section whose switch is off is not offered as a tab. Only the nav — the
// routes stay reachable, so a deep link or bookmark to #/live-tv with Live TV
// off still lands on that page's own disabled state. Sections without a
// switch (Home, Activity, Settings, Libraries, Search) are always shown, and
// so is everything while FEATURES is unknown (see loadFeatures()).
const NAV_FEATURES={"shared":"shared_libraries","live-tv":"live_tv","recordings":"recordings","library-channels":"library_channels"};
// A layout change is presentation, not route disposal. Every chrome has the
// same persistent shell/main seam; replace only the siblings around #main.
let LAYOUT_RECOMPOSING=false;
const SEARCH_INPUTS=new WeakSet();
function mountLayoutFrame(markup){
  const app=document.getElementById("app");
  if(!LAYOUT_RECOMPOSING){ app.innerHTML=markup; return; }
  const current=app.querySelector("[data-layout-shell]");
  const main=current&&current.querySelector(":scope > #main");
  const template=document.createElement("template");template.innerHTML=markup;
  const next=template.content.querySelector("[data-layout-shell]");
  if(!current||!main||!next) return;
  const search=current.querySelector("#q"),incoming=next.querySelector("#q");
  if(search&&incoming) incoming.replaceWith(search);
  for(const attribute of Array.from(current.attributes)) current.removeAttribute(attribute.name);
  for(const attribute of Array.from(next.attributes)) current.setAttribute(attribute.name,attribute.value);
  for(const node of Array.from(current.childNodes)) if(node!==main) node.remove();
  let after=false;
  for(const node of Array.from(next.childNodes)){
    if(node.nodeType===1&&/** @type {HTMLElement} */(node).id==="main"){after=true;continue;}
    if(after) current.append(node);else current.insertBefore(node,main);
  }
}
function featureVisible(tab){
  const key=Object.prototype.hasOwnProperty.call(NAV_FEATURES,tab)?NAV_FEATURES[tab]:null;
  return !key||!FEATURES||FEATURES[key]!==false;
}
// ---- chrome ---------------------------------------------------------------
function classicNavHtml(active){
  const link=(tab,href,label)=>featureVisible(tab)?`<a href="${href}" class="${active===tab?'active':''}">${label}</a>`:"";
  return link("home","#/","Home")+link("shared","#/shared","Shared libraries")+link("live-tv","#/live-tv","Live TV")
    +link("recordings","#/recordings","Recordings")+link("library-channels","#/library-channels","Library channels")
    +link("activity","#/activity","Activity")+(ME&&ME.is_admin?link("settings","#/settings","Settings"):"");
}
// A switch flipped in Settings repaints the section links in place: the page
// under them (the Settings card the admin just saved) is left alone.
function classicNavChanged(active){
  const nav=document.querySelector("header.top > nav");
  if(nav) nav.innerHTML=classicNavHtml(active);
}
function classicChrome(active, inner){
  const searchFocus=captureSearchFocus();
  const getapp = installAvailable() ? `<button class="ghost sm getappbtn" onclick="showInstall()" title="Install the ${APP_NAME} app">Get app</button>` : "";
  mountLayoutFrame(`
   <div class="layout-frame" data-layout-shell><header class="top${getQ()||searchFocus?' search-open':''}">
     <a href="#/" class="logo">${APP_NAME}</a>
     <nav>${classicNavHtml(active)}</nav>
     <span class="spacer"></span>
     <button class="dvr-global" id="dvr-global" onclick="location.hash='#/activity'" aria-label="Recording status"></button>
     <span class="activity" id="activity" onclick="location.hash='#/activity'"></span>
     <input class="search" id="q" type="search" name="q" autocomplete="off" placeholder="Search…" aria-label="Search ${APP_NAME}" value="${esc(getQ())}">
     <div class="topctl"><button class="ghost sm mobile-search" onclick="toggleMobileSearch()" aria-label="Search" aria-expanded="${!!(getQ()||searchFocus)}" aria-controls="q">⌕</button>
       <div style="position:relative">
         <button class="ghost sm lookbtn" onclick="toggleLookMenu(event)" title="Appearance — layout, theme, light or dark, poster size" aria-label="Appearance" aria-haspopup="menu"><span class="tsw"></span><span class="lookword">Appearance</span></button>
         <div class="amenu lookmenu" id="lookmenu"></div>
       </div>
       ${getapp}
       <div style="position:relative">
         <button class="ghost sm profile" onclick="toggleProfileMenu(event)" title="Account">
           <span class="pavatar">${esc((ME&&ME.username?ME.username[0]:"?").toUpperCase())}</span><span class="pname">${esc(ME?ME.username:"")}</span><span class="caret">▾</span>
         </button>
         <div class="amenu" id="profilemenu"></div>
       </div>
     </div>
   </header><main id="main">${inner}<div class="versionstamp">Version ${esc(buildLabel())}</div></main></div>`);
  const q=document.getElementById("q");
  wireSearchInput(q);
  restoreSearchFocus(searchFocus);
  navKeyboardWireOnce();
  if(!LAYOUT_RECOMPOSING) pollActivity();
}
function getQ(){ const m=location.hash.match(/^#\/search\/(.*)$/); return m?decodeURIComponent(m[1]):""; }

function wireSearchInput(q){
  if(!q||SEARCH_INPUTS.has(q)) return;
  SEARCH_INPUTS.add(q);
  let timer;
  q.addEventListener("input",()=>{
    clearTimeout(timer);
    // Saved-login autofill can mistake the header for a username beside a
    // Settings API-key field. Only editing the search field may navigate.
    if(document.activeElement!==q) return;
    const route=location.hash, value=q.value;
    timer=setTimeout(()=>{
      // A pending search belongs to this header and route, not the page the
      // user opened while the debounce was waiting.
      if(!q.isConnected||location.hash!==route) return;
      location.hash=value?("#/search/"+encodeURIComponent(value)):"#/";
    },300);
  });
}
