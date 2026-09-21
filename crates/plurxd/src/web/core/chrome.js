"use strict";
// ---- chrome ---------------------------------------------------------------
function classicChrome(active, inner){
  const searchFocus=captureSearchFocus();
  const admin = ME&&ME.is_admin ? `<a href="#/settings" class="${active==='settings'?'active':''}">Settings</a>`:"";
  const discs = typeof opticalNavEnabled==="function"&&opticalNavEnabled()
    ? `<a href="#/discs" class="${active==='discs'?'active':''}">Discs</a>`:"";
  const getapp = installAvailable() ? `<button class="ghost sm getappbtn" onclick="showInstall()" title="Install the ${APP_NAME} app">Get app</button>` : "";
  document.getElementById("app").innerHTML=`
   <header class="top${getQ()||searchFocus?' search-open':''}">
     <a href="#/" class="logo">${APP_NAME}</a>
     <nav><a href="#/" class="${active==='home'?'active':''}">Home</a><a href="#/live-tv" class="${active==='live-tv'?'active':''}">Live TV</a><a href="#/recordings" class="${active==='recordings'?'active':''}">Recordings</a><a href="#/library-channels" class="${active==='library-channels'?'active':''}">Library channels</a>${discs}<a href="#/activity" class="${active==='activity'?'active':''}">Activity</a>${admin}</nav>
     <span class="spacer"></span>
     <button class="dvr-global" id="dvr-global" onclick="location.hash='#/activity'" aria-label="Recording status"></button>
     <span class="activity" id="activity" onclick="location.hash='#/activity'"></span>
     <input class="search" id="q" placeholder="Search…" aria-label="Search ${APP_NAME}" value="${esc(getQ())}">
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
   </header><main id="main">${inner}<div class="versionstamp">Version ${esc(buildLabel())}</div></main>`;
  const q=document.getElementById("q");
  let t; q.addEventListener("input",()=>{ clearTimeout(t); t=setTimeout(()=>{ location.hash= q.value?("#/search/"+encodeURIComponent(q.value)):"#/"; },300); });
  restoreSearchFocus(searchFocus);
  navKeyboardWireOnce();
  pollActivity();
}
function getQ(){ const m=location.hash.match(/^#\/search\/(.*)$/); return m?decodeURIComponent(m[1]):""; }
