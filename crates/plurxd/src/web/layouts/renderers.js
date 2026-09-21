"use strict";
// ---- layout renderers -----------------------------------------------------
// Attached to the registry declared in the <head> script. A layout supplies a
// chrome() (the shell) and a views map (one body builder per converted route);
// anything it does not supply falls through to classic's, so a new layout is
// only as big as the parts it actually changes.
// Resolve through the active layout, then classic. Two lookups rather than one
// because a layout that only redefines Home must still get the classic body for
// every route it has not converted — a missing renderer is normal, not an error.
function layoutChrome(active, inner){
  const L=LAYOUTS[layoutId()];
  const answer=(L&&L.chrome?L.chrome:classicChrome)(active, inner);
  dvrPaintGlobal();
  return answer;
}
function layoutView(name, page){
  const L=LAYOUTS[layoutId()];
  const fn=(L&&L.views&&L.views[name])||LAYOUTS.classic.views[name];
  return fn(page);
}
// Region resolution for incremental routes. Falls back per REGION, not per
// view: a layout that only wants its own shell (to move the A-Z rail mount, or
// to add a tab strip) inherits classic's items and count untouched, which is
// the difference between overriding a detail and reimplementing a route.
function layoutRegion(name, region, model){
  const L=LAYOUTS[layoutId()];
  const own=L&&L.views&&L.views[name];
  const fn=(own&&own[region])||LAYOUTS.classic.views[name][region];
  return fn(model);
}
// Classic's Home body — byte-for-byte the markup this app has always emitted.
// It is the zero-diff baseline every other layout is measured against, so this
// function changes only when the shipped app is meant to change.
function classicHomeBody(p){
  let hubs=installBannerHtml(), soon="", previews="";
  // The first two rails pass watchCtx: those rows are about where you are in
  // something, so the card carries "23m left" / "watched Tuesday".
  if(p.hubs.continue_watching.length){ hubs+=rail("Continue watching", p.hubs.continue_watching.map(i=>card(i,true)).join("")); }
  if(p.hubs.next_up && p.hubs.next_up.length){ hubs+=rail("Next up", p.hubs.next_up.map(i=>card(i,true)).join("")); }
  if(p.hubs.recently_added.length){ hubs+=rail("Recently added", p.hubs.recently_added.map(i=>card(i)).join("")); }
  // Coming soon: what Curator expects. Absent entirely when nothing is paired
  // or Curator had nothing to say — an empty rail is worse than no rail.
  if(p.soon && p.soon.entries && p.soon.entries.length){
    soon+=rail("Coming soon", p.soon.entries.map(comingCard).join(""));
  }
  if(p.hubsError) hubs+=`<div class="hint">Home recommendations are unavailable right now.</div>`;
  if(p.previewsPending){
    previews+=`<div class="empty">Loading library previews…</div>`;
  }else if(p.previewsError){
    previews+=`<div class="empty">Library previews are unavailable right now.</div>`;
  }else if(p.libs.length){
    previews+=groupToggleHtml(p.group);
    for(const s of p.sections){
      const count=s.total==null?"preview unavailable":`view all (${s.total})`;
      const unavailable=s.unavailable?`<div class="hint">Some library previews are unavailable right now.</div>`:"";
      previews+=`<h2 class="section">${esc(s.name)} <a class="muted" style="font-size:12px" href="${s.href}">· ${count}</a></h2>${unavailable}${grid(s.items)}`;
    }
  } else {
    previews+=`<div class="empty">No libraries yet.${ME.is_admin?' Add one in <a href="#/settings/libraries">Settings</a>.':''}</div>`;
  }
  return opticalHomeHtml(p)+
    `<div data-home-region="hubs" data-home-slot="hubs">${hubs}</div>`+
    `<div data-home-region="soon" data-home-slot="soon">${soon}</div>`+
    `<div data-home-region="previews" data-home-slot="previews">${previews}</div>`;
}
// One rail: a heading, and a strip with an arrow at each end.
//
// A helper rather than four copies of the markup, because the arrows need a
// wrapper the strip cannot provide — they are positioned against it, and the
// hover that reveals the scrollbar has to cover them too.
function rail(heading, cards){
  return `<h2 class="section">${heading}</h2>
    <div class="railbox">
      <button class="railarrow l" aria-label="Scroll left" onclick="railScroll(this,-1)">‹</button>
      <div class="rowscroll">${cards}</div>
      <button class="railarrow r" aria-label="Scroll right" onclick="railScroll(this,1)">›</button>
    </div>`;
}
// Just under a full view, so the card at the edge stays on screen and you keep
// your place. A whole-width jump loses the thing you were looking at, which is
// the complaint people have about carousels that page.
function railScroll(btn, dir){
  const strip=btn.parentElement.querySelector(".rowscroll"); if(!strip) return;
  strip.scrollBy({left:dir*Math.max(160,strip.clientWidth*0.85),behavior:"smooth"});
}
// Which arrows apply right now. Recomputed on scroll and on resize: a window
// that grows can turn a scrollable rail into one that fits, and an arrow that
// scrolls nothing is worse than no arrow.
function railSync(box){
  const strip=box.querySelector(".rowscroll"); if(!strip) return;
  // A pixel of slack: fractional scroll positions and subpixel layout mean
  // scrollLeft rarely lands exactly on the end, and testing for equality
  // leaves a live arrow at the last position of every rail.
  const max=strip.scrollWidth-strip.clientWidth;
  box.classList.toggle("can-l", strip.scrollLeft>1);
  box.classList.toggle("can-r", strip.scrollLeft<max-1);
}
// Called after any render that emits rails. Idempotent: a re-render replaces
// the elements, so the flag rides on the node and dies with it — no listener
// outlives the DOM it was attached to, and no rail gets wired twice.
function wireRails(){
  for(const box of document.querySelectorAll(".railbox")){
    if(box._wired) continue;
    box._wired=true;
    const strip=box.querySelector(".rowscroll");
    if(strip) strip.addEventListener("scroll",()=>railSync(box),{passive:true});
    if(window.ResizeObserver){ new ResizeObserver(()=>railSync(box)).observe(box); }
    railSync(box);
  }
}
window.addEventListener("resize",()=>document.querySelectorAll(".railbox").forEach(railSync),{passive:true});
// "" means "whatever suits this library" — home libraries are about when
// something happened, everything else about what it is called. An explicit
// pick from the dropdown wins from then on.
let LIB_SORT="", LIB_FILTER="all";
// How many posters a page of a library shows. "all" is a real choice, not the
// default: 300 cards is a slow first paint on a TV browser and an easy scroll
// on a desktop, and only the person looking at it knows which they are.
// Remembered per browser — someone who wants 20 at a time wants it every time.
const LIB_SIZES=[20,50,100,200];
let LIB_PER=(function(){
  try{ const v=localStorage.getItem("plurx_perpage"); if(v==="all"||LIB_SIZES.includes(+v)) return v==="all"?"all":+v; }catch(e){}
  return 100;
})();
let LIB_PAGE_AT=0;   // which page of the filtered list is on screen
function setPerPage(v){
  LIB_PER = v==="all" ? "all" : (+v||100);
  LIB_PAGE_AT=0;     // a resize makes the old page number meaningless
  try{ localStorage.setItem("plurx_perpage", String(LIB_PER)); }catch(e){}
}
function libPageCount(n){ return LIB_PER==="all" ? 1 : Math.max(1, Math.ceil(n/LIB_PER)); }
// Move to another page of the current listing. Redraws in place rather than
// re-fetching — every item is already loaded, so paging is a slice.
function libGoPage(n){
  const pages=libPageCount(LIB_VIEW? LIB_VIEW.shown : 0);
  LIB_PAGE_AT=Math.min(Math.max(0,n), pages-1);
  if(LIB_VIEW) LIB_VIEW.draw(LIB_VIEW.done);
  const m=document.querySelector("#main .libbar");
  if(m) m.scrollIntoView({behavior:"smooth",block:"start"});
}
// The listing currently on screen, so the pager buttons can redraw it.
let LIB_VIEW=null;
function pagerHtml(shown){
  const pages=libPageCount(shown);
  if(pages<2) return "";
  const at=LIB_PAGE_AT;
  const btn=(n,label,dis,title)=>`<button class="ghost sm"${dis?" disabled":""}${title?` title="${title}"`:""} onclick="libGoPage(${n})">${label}</button>`;
  // First and last, because "sorted by date added, newest last" makes the far
  // end a destination and not an edge case — and getting there a page at a
  // time on a 300-title library is sixteen clicks. Hidden below three pages,
  // where they would duplicate Previous and Next exactly.
  const ends=pages>2;
  return `<div class="pager">
    ${ends?btn(0,"«",at<=0,"First page"):""}
    ${btn(at-1,"‹ Previous",at<=0)}
    <span class="muted">Page ${at+1} of ${pages}</span>
    ${btn(at+1,"Next ›",at>=pages-1)}
    ${ends?btn(pages-1,"»",at>=pages-1,"Last page"):""}
  </div>`;
}
function sortFor(lib){ return LIB_SORT || ((lib&&lib.kind==="home")?"recorded":"title"); }
function libOpt(v,label,cur){ return `<option value="${v}" ${cur===v?'selected':''}>${esc(label)}</option>`; }
function matchWatch(it,f){
  const w=it.watch, watched=w&&w.watched;
  const prog=w&&w.position_ms>3000&&!watched;
  if(f==="watched") return !!watched;
  if(f==="unwatched") return !watched && !prog;
  if(f==="inprogress") return !!prog;
  return true;
}
// A–Z jump rail for alphabetically-sorted library/category grids. sortKey
// mirrors the server's sort_title_for (lowercase, strip leading the/a/an) so the
// letter buckets line up with the order the list is actually in.
function sortKey(t){ const s=(t||"").toLowerCase(); for(const a of ["the ","a ","an "]){ if(s.startsWith(a)&&s.length>a.length) return s.slice(a.length); } return s; }
function alphaBucket(it){ const c=sortKey(it&&it.title)[0]||""; return (c>="a"&&c<="z")?c.toUpperCase():"#"; }
function alphaRailHtml(items, sort){
  // The EFFECTIVE sort, not the stored preference. LIB_SORT is "" until the
  // user touches the select, while sortFor() resolves that to "title" for
  // every non-home library — so reading the global here hid the rail on
  // exactly the libraries it is for, until someone re-picked the sort that
  // was already in force. Callers pass what they actually rendered.
  if((sort!==undefined?sort:LIB_SORT)!=="title") return "";  // rail only maps to A–Z order
  const present=new Set((items||[]).map(alphaBucket));
  if(present.size<2) return "";                              // pointless with one bucket
  const letters=["#"].concat("ABCDEFGHIJKLMNOPQRSTUVWXYZ".split(""));
  return `<div class="azbar" aria-label="Jump to letter">`+letters.map(L=>
    present.has(L)?`<button data-l="${L}" onclick="alphaJump('${L}')" title="Jump to ${L==='#'?'0–9':L}">${L}</button>`
                 :`<span class="off">${L}</span>`).join("")+`</div>`;
}
function tagAlpha(items, sort){
  // The effective sort, for the same reason alphaRailHtml takes it: LIB_SORT is
  // "" until the user touches the select, while sortFor() has already resolved
  // that to "title". Reading the global here after fixing the rail's own test
  // left the rail rendered and inert — every letter drawn, no card tagged,
  // alphaJump finding nothing. One feature, two call sites, one of them fixed.
  if((sort!==undefined?sort:LIB_SORT)!=="title") return;
  const g=document.querySelector("#main .grid"); if(!g) return;
  const cards=g.children;                                    // grid renders items[i] 1:1
  for(let i=0;i<cards.length&&i<items.length;i++) cards[i].dataset.alpha=alphaBucket(items[i]);
  updateAzActive();                                         // set the initial highlight
}
// Where the page's fixed chrome stops and content begins. The layout owns this
// number: classic has a sticky top header, catalog has a sidebar and no top bar at
// all, and a hardcoded `header.top` lookup silently measures nothing under catalog
// — the A–Z rail then highlights a letter that is already scrolled past.
function stickyFloor(){
  const L=LAYOUTS[layoutId()];
  if(L&&L.stickyFloor) return L.stickyFloor();
  const h=document.querySelector("header.top");
  return h?h.getBoundingClientRect().bottom:56;
}
function alphaJump(L){ const t=document.querySelector('#main .grid [data-alpha="'+L+'"]'); if(t) t.scrollIntoView({behavior:"smooth",block:"start"}); }
// Scroll-spy: highlight the letter whose posters sit at the top of the viewport
// as you scroll. One persistent, rAF-throttled window listener drives whatever
// rail is on screen (no-op when none), so it survives view swaps replacing #main.
function azCurrentBucket(){
  const g=document.querySelector("#main .grid"); if(!g) return null;
  const line=stickyFloor()+6;                              // just under the sticky header
  const cards=g.children;
  for(let i=0;i<cards.length;i++){ if(cards[i].getBoundingClientRect().bottom>line) return cards[i].dataset.alpha||null; }
  return null;
}
let AZ_RAF=0;
function updateAzActive(){
  AZ_RAF=0;
  const bar=document.querySelector(".azbar"); if(!bar) return;
  const cur=azCurrentBucket();
  bar.querySelectorAll("button").forEach(b=>b.classList.toggle("active", b.dataset.l===cur));
}
function azOnScroll(){ if(!AZ_RAF) AZ_RAF=requestAnimationFrame(updateAzActive); }
window.addEventListener("scroll",azOnScroll,{passive:true});
window.addEventListener("resize",azOnScroll,{passive:true});
