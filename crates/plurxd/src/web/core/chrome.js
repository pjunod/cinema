"use strict";
// ---- chrome ---------------------------------------------------------------
function classicChrome(active, inner){
  const searchFocus=captureSearchFocus();
  const admin = ME&&ME.is_admin ? `<a href="#/settings" class="${active==='settings'?'active':''}">Settings</a>`:"";
  const getapp = installAvailable() ? `<button class="ghost sm getappbtn" onclick="showInstall()" title="Install the ${APP_NAME} app">Get app</button>` : "";
  document.getElementById("app").innerHTML=`
   <header class="top${getQ()||searchFocus?' search-open':''}">
     <a href="#/" class="logo">${APP_NAME}</a>
     <nav><a href="#/" class="${active==='home'?'active':''}">Home</a><a href="#/shared" class="${active==='shared'?'active':''}">Shared libraries</a><a href="#/live-tv" class="${active==='live-tv'?'active':''}">Live TV</a><a href="#/recordings" class="${active==='recordings'?'active':''}">Recordings</a><a href="#/library-channels" class="${active==='library-channels'?'active':''}">Library channels</a><a href="#/activity" class="${active==='activity'?'active':''}">Activity</a>${admin}</nav>
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
   </header><main id="main">${inner}<div class="versionstamp">Version ${esc(buildLabel())}</div></main>`;
  const q=document.getElementById("q");
  wireSearchInput(q);
  restoreSearchFocus(searchFocus);
  navKeyboardWireOnce();
  pollActivity();
}
function getQ(){ const m=location.hash.match(/^#\/search\/(.*)$/); return m?decodeURIComponent(m[1]):""; }

function wireSearchInput(q){
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
