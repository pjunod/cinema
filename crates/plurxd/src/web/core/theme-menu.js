"use strict";
// ---- theme + appearance menu (per-user, header) ---------------------------
// Two independent header menus: theme (the palette) and appearance (light/dark).
// Auto-skip is a playback preference and lives on the player, not here.
function themeMenuHtml(){
  let theme="classic"; try{ theme=localStorage.getItem("plurx_theme")||"classic"; }catch(e){}
  const themes=Object.keys(THEMES).map(id=>{
    // The midnight-only marker rides on the entry itself, so the constraint is
    // legible before the choice is made — not discovered afterwards when the
    // appearance toggle appears to do nothing.
    const only=THEMES[id].darkOnly?`<span class="amnote">midnight only</span>`:"";
    return `<button class="${theme===id?'sel':''}" onclick="setTheme('${id}')">${esc(THEMES[id].name)}${only}</button>`;
  }).join("");
  return `${layoutMenuHtml()}<div class="amsec">Theme</div><div class="amthemes">${themes}</div>`;
}
// Layout sits above Theme in the same popover: structure first, then paint.
// Only layouts this surface can actually run are offered — an entry the device
// would silently fall back from is worse than no entry, because the user picks
// it, nothing changes, and the menu goes on claiming it is selected.
function layoutMenuHtml(){
  const cur=layoutId(), here=surfaceClass();
  const btns=Object.keys(LAYOUTS).filter(id=>{
    const s=LAYOUTS[id].surfaces;
    return !s||s.indexOf(here)>=0;
  }).map(id=>`<button class="${cur===id?'sel':''}" onclick="setLayout('${id}')">${esc(LAYOUTS[id].name)}</button>`).join("");
  return `<div class="amsec">Layout</div><div class="amseg">${btns}</div>`;
}
// A layout change repaints everything, chrome included — unlike a theme, which
// only swaps CSS variables on an already-painted page. render() is the honest
// way to do that: it re-runs the current route through the new layout.
// Where the reader was, carried across a re-render that is not a navigation.
// Consume-once: a scroll position restored on a route the user actually
// navigated to would be worse than the reset it replaces.
let PENDING_SCROLL=null;
function restoreScroll(){
  if(PENDING_SCROLL===null) return;
  const y=PENDING_SCROLL; PENDING_SCROLL=null;
  // After the frame that painted the content, or there is nothing to scroll
  // through yet and the browser clamps the offset to zero.
  requestAnimationFrame(()=>requestAnimationFrame(()=>window.scrollTo(0,y)));
}
function setLayout(id){
  try{ localStorage.setItem("plurx_layout",id); }catch(e){}
  // A layout switch is a change of clothes, not a change of place: the same
  // route with the same content is about to be rebuilt from scratch, and
  // dumping the reader at the top of a 200-item library is a real loss.
  PENDING_SCROLL=window.scrollY||0;
  applyLayout();
  render();
}
function appMenuHtml(){
  let app="auto"; try{ app=localStorage.getItem("plurx_appearance")||"auto"; }catch(e){}
  const modes=`<div class="amseg">${[["auto","Auto"],["light","Light"],["dark","Dark"]].map(([v,l])=>`<button class="${app===v?'sel':''}" onclick="setAppearance('${v}')" title="${v==="auto"?"Follow the system setting":l}">${l}</button>`).join("")}</div>`;
  // If the current theme pins itself to midnight, say so here too. Without
  // this the control silently does nothing and the only available conclusion
  // is that the app is broken.
  let cur="classic"; try{ cur=localStorage.getItem("plurx_theme")||"classic"; }catch(e){}
  const t=THEMES[cur];
  const note=(t&&t.darkOnly)?`<div class="amwarn">${esc(t.name)} is a midnight-only theme — it stays dark whichever you pick.</div>`:"";
  return `<div class="amsec">Light or dark</div>${modes}${note}`;
}
function closeMenus(except){ ["lookmenu","profilemenu"].forEach(idn=>{ if(idn!==except){ const m=document.getElementById(idn); if(m) m.classList.remove("on"); } }); }
// One popover for everything about how this browser looks — layout, theme,
// light/dark, poster size — under the name the mobile apps already use for
// the same four choices. It used to be three unnamed buttons.
function lookMenuHtml(){ return `${themeMenuHtml()}${appMenuHtml()}${sizeMenuHtml()}`; }
function toggleLookMenu(e){ if(e) e.stopPropagation(); closeMenus("lookmenu"); const m=document.getElementById("lookmenu"); if(!m) return;
  const open=!m.classList.contains("on"); m.classList.toggle("on",open); if(open) m.innerHTML=lookMenuHtml(); }
function repaintLookMenu(){ const m=document.getElementById("lookmenu"); if(m&&m.classList.contains("on")) m.innerHTML=lookMenuHtml(); }
// The three older entry points route to the one menu; a layout's chrome may
// still name them.
function toggleThemeMenu(e){ toggleLookMenu(e); }
function toggleAppearanceMenu(e){ toggleLookMenu(e); }
function setTheme(id){ try{localStorage.setItem("plurx_theme",id)}catch(e){} applyTheme(); repaintLookMenu(); }
function setAppearance(v){ try{localStorage.setItem("plurx_appearance",v)}catch(e){} applyTheme(); repaintLookMenu(); }
// POSTER_SIZES / posterSize / applyPosterSize live in the <head> script — the
// size has to be on the root element before first paint, and a definition down
// here would not exist yet when that script runs.
function sizeMenuHtml(){ const v=posterSize(); return `<div class="amsec">Poster size</div><div class="amseg">`+POSTER_SIZES.map(s=>`<button class="${v===s[0]?'sel':''}" onclick="setIconSize('${s[0]}')">${s[1]}</button>`).join("")+`</div>`; }
function toggleSizeMenu(e){ toggleLookMenu(e); }
function setIconSize(v){ try{localStorage.setItem("plurx_iconsize",v)}catch(e){} applyPosterSize(); repaintLookMenu(); }
// Account menu, anchored far-right: shows who you're signed in as, plus the
// per-account actions (admins also get a Settings shortcut) and Sign out.
// The server QR belongs here too: it contains no credential, and requiring a
// person to sign out before pairing another device breaks the session they are
// using to perform the pairing.
function profileMenuHtml(){
  const name=ME?ME.username:"", role=ME&&ME.is_admin?"Administrator":"Signed in";
  const settings = ME&&ME.is_admin ? `<button onclick="location.hash='#/settings';closeMenus()">Settings</button>`:"";
  return `<div class="pident"><span class="pavatar">${esc((name[0]||"?").toUpperCase())}</span>`
    +`<span><b>${esc(name||"—")}</b><small>${role}</small></span></div>`
    +`<button onclick="showConnectQr()">Show server QR code</button>`
    +settings
    +`<button onclick="logout()">Sign out</button>`;
}
function toggleProfileMenu(e){ if(e) e.stopPropagation(); closeMenus("profilemenu"); const m=document.getElementById("profilemenu"); if(!m) return;
  const open=!m.classList.contains("on"); m.classList.toggle("on",open); if(open) m.innerHTML=profileMenuHtml(); }
document.addEventListener("click",()=>{ closeMenus(); });

let CONNECT_RETURN_FOCUS=null, CONNECT_PREVIOUS_OVERFLOW="";
function showConnectQr(){
  // The menu action disappears when closeMenus() runs; return to its visible
  // account-menu trigger, not to a now-hidden button inside the popover.
  CONNECT_RETURN_FOCUS=document.querySelector(".profile")||document.activeElement;
  closeMenus();
  if(document.getElementById("connectdialog")) return;
  CONNECT_PREVIOUS_OVERFLOW=document.body.style.overflow;
  const dialog=document.createElement("div");
  dialog.id="connectdialog"; dialog.className="connectdialog";
  dialog.setAttribute("role","dialog"); dialog.setAttribute("aria-modal","true");
  dialog.setAttribute("aria-labelledby","connecttitle");
  dialog.innerHTML=`<div class="card connectcard">
    <button class="connectx" type="button" aria-label="Close server QR code" onclick="closeConnectQr()">✕</button>
    <h2 id="connecttitle">Connect mobile app</h2>
    <p>In the ${APP_NAME} mobile app, choose <b>Scan server QR code</b>.</p>
    <img src="/connect.svg?origin=${encodeURIComponent(location.origin)}" alt="QR code for ${esc(location.origin)}">
    <p>This code contains only this server's address. You will sign in normally on the mobile app.</p>
  </div>`;
  dialog.addEventListener("click",e=>{ if(e.target===dialog) closeConnectQr(); });
  document.body.appendChild(dialog);
  document.body.style.overflow="hidden";
  dialog.querySelector(".connectx").focus();
}
function closeConnectQr(){
  const dialog=document.getElementById("connectdialog"); if(!dialog) return;
  dialog.remove(); document.body.style.overflow=CONNECT_PREVIOUS_OVERFLOW;
  const target=CONNECT_RETURN_FOCUS; CONNECT_RETURN_FOCUS=null;
  if(target&&target.isConnected&&target.focus) target.focus();
}
function connectQrOpen(){ return !!document.getElementById("connectdialog"); }
// The cell a popover was opened from, so closing it can hand the keyboard back.
let LIVE_TV_POP_OPENER=null;
function liveTvPopoverOpen(){
  const pop=document.getElementById("live-tv-pop");
  return !!(pop&&!pop.hidden);
}
// Capture Escape before layout-specific sheet handlers so one key closes only
// the topmost modal, never it and something behind it. The edit dialog is
// here rather than in its own listener for the same reason: two Escape
// handlers is how a key ends up closing two things.
window.addEventListener("keydown",e=>{
  if(e.key!=="Escape") return;
  if(connectQrOpen()){ e.preventDefault(); e.stopImmediatePropagation(); closeConnectQr(); return; }
  if(document.getElementById("editwrap")){ e.preventDefault(); e.stopImmediatePropagation(); closeEdit(); return; }
  if(liveTvPopoverOpen()){ e.preventDefault(); e.stopImmediatePropagation(); liveTvPopover(null); }
},true);

