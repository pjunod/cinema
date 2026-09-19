"use strict";
// ---- page header: back control + breadcrumb trail -------------------------
// The list page you came from is not derivable from an item's own data (an item
// belongs to a library, but you may have reached it from a category or a
// search), so the router remembers it. Item→item hops keep it, which is what
// makes Movies → Show → Season → Episode still offer "Movies"; anything that
// leaves the drill-in (home, activity, settings) clears it.
let NAV_ORIGIN=null;
function setOrigin(href,label){ NAV_ORIGIN={href,label}; }
// Build one header for every drill-in page. `trail` is [{href,label},…] ending
// with the current page; back points at the level above it, so the button and
// the trail can never disagree about where "up" is.
function pageHead(trail,title){
  const parent=trail.length>1? trail[trail.length-2] : {href:"#/",label:"Home"};
  const back=`<a class="pgback" href="${parent.href}" aria-label="Back to ${esc(parent.label)}" title="Back to ${esc(parent.label)}">←</a>`;
  const crumbs=trail.map((c,i)=> (i===trail.length-1||!c.href)
    ? `<span class="cur">${esc(c.label)}</span>`
    : `<a href="${c.href}">${esc(c.label)}</a>`).join('<span class="sep">/</span>');
  return `<div class="pagehead"><div class="phnav">${back}<div class="crumbs">${crumbs}</div></div>`+
    (title?`<h2 class="phtitle">${esc(title)}</h2>`:``)+`</div>`;
}
