"use strict";
// ---- keyboard reach for click-only cards ----------------------------------
// nav-keyboard-adapter:begin — posters and episode rows are emitted as
// `<div onclick>`, so without this they are mouse-only: no tab stop, nothing
// announced, and Enter does nothing. Catalog and Theater each carried a
// near-copy of this (their own comment said so) and Classic carried none, so
// the layout you picked decided whether the library was reachable at all.
/** @param {ParentNode} [root] */
function navEnhanceClickables(root=document){
  for(const el of root.querySelectorAll(
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
  if(app&&window.MutationObserver) new MutationObserver(()=>navEnhanceClickables()).observe(app,{childList:true,subtree:true});
  navEnhanceClickables();
}
// Horizontal library navigation shares the card keyboard adapter.
function libraryRowKey(event){
  if(event.altKey||event.ctrlKey||event.metaKey||event.shiftKey)return;
  const poster=event.target.closest(".poster");if(!poster)return;
  let next=null;
  if(event.key==="ArrowRight"){
    const row=LIB_ROW_NODES.get(poster.parentElement);
    if(!poster.nextElementSibling&&row&&row.limit<row.items.length){row.limit+=LIB_ROW_CHUNK;libraryPaintRowCards(row);}
    next=poster.nextElementSibling;
  }
  else if(event.key==="ArrowLeft")next=poster.previousElementSibling;
  else if(event.key==="Home")next=poster.parentElement.firstElementChild;
  else if(event.key==="End"){
    const row=LIB_ROW_NODES.get(poster.parentElement);
    if(row){row.limit=row.items.length;libraryPaintRowCards(row);}
    next=poster.parentElement.lastElementChild;
  }
  else return;
  event.preventDefault();event.stopPropagation();
  if(next){next.focus({preventScroll:true});next.scrollIntoView({block:"nearest",inline:"nearest",behavior:libraryScrollBehavior()});}
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

