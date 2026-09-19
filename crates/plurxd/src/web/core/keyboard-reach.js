"use strict";
// ---- keyboard reach for click-only cards ----------------------------------
// nav-keyboard-adapter:begin — posters and episode rows are emitted as
// `<div onclick>`, so without this they are mouse-only: no tab stop, nothing
// announced, and Enter does nothing. Catalog and Theater each carried a
// near-copy of this (their own comment said so) and Classic carried none, so
// the layout you picked decided whether the library was reachable at all.
function navEnhanceClickables(){
  for(const el of document.querySelectorAll(
    ".poster[onclick]:not([tabindex]),.eprow[onclick]:not([tabindex])")){
    el.setAttribute("tabindex","0");
    // Cards that navigate are links; the photo cards call openLightbox() and
    // are buttons. Announcing every card as a link would promise a page that
    // a lightbox never delivers.
    el.setAttribute("role", /location\.hash/.test(el.getAttribute("onclick")||"") ? "link" : "button");
  }
}
let NAV_KEYS_WIRED=false;
function navKeyboardWireOnce(){
  if(NAV_KEYS_WIRED) return;
  NAV_KEYS_WIRED=true;
  document.addEventListener("keydown",e=>{
    // The player and the lightbox own the keyboard while they are up.
    const modal=document.getElementById("modal");
    if(lightboxOpen()||(modal&&modal.classList.contains("open"))) return;
    const el=e.target;
    if(!el||!el.classList) return;
    if(!el.classList.contains("poster")&&!el.classList.contains("eprow")) return;
    // Enter and Space are what a role=link/button promises. "Spacebar" is the
    // legacy key name older WebKit still reports.
    if(e.key!=="Enter"&&e.key!==" "&&e.key!=="Spacebar") return;
    e.preventDefault();          // Space would otherwise page the grid down
    el.click();                  // fires the inline handler the card emitted
  });
  // #app itself is never replaced (every view assigns to its innerHTML), so
  // one observer on it outlives every route change.
  const app=document.getElementById("app");
  if(app&&window.MutationObserver) new MutationObserver(navEnhanceClickables).observe(app,{childList:true,subtree:true});
  navEnhanceClickables();
}
// nav-keyboard-adapter:end

// The header is re-emitted on every route change, and a search field is typed
// into WHILE the route changes — every keystroke re-renders and the element
// the viewer was typing in is gone. Carry focus and the caret across the
// rebuild; the value already survives, because it is read back out of the
// hash.
function captureSearchFocus(){
  const q=document.getElementById("q");
  if(!q||document.activeElement!==q) return null;
  return {start:q.selectionStart,end:q.selectionEnd};
}
function restoreSearchFocus(state){
  if(!state) return;
  const q=document.getElementById("q");
  if(!q) return;
  q.focus({preventScroll:true});
  try{ q.setSelectionRange(state.start,state.end); }catch(e){}
}

