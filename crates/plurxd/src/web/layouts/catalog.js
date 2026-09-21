"use strict";
// ---- layout: catalog ---------------------------------------------------------
// The Catalog shell, composed from the shipped app's existing parts:
// a fixed left sidebar (wordmark, search, Home, the libraries with counts,
// Activity/Settings under a divider, scan status and the account chip pinned
// to the bottom), and a Home made of hub rows.
//
// What this file deliberately does NOT contain: a card renderer, a rail
// renderer, a grid renderer, an escape function or a fetch. Home is composed
// out of card()/rail()/grid()/comingCard()/installBannerHtml()/groupToggleHtml()
// exactly as classicHomeBody() composes it — the difference between the two
// layouts is which of those go in which order and what CSS does to them. A
// second card function is how five layouts become five apps, which is the one
// outcome the layout split exists to prevent.

// Sidebar iconography. One 24×24 stroked set, drawn with currentColor so a row
// tints with its label instead of needing a second colour rule, and inline
// rather than a sprite because this app ships as a single file with no asset
// pipeline to hang a sprite off.
const CATALOG_GLYPHS={
  home:'<path d="M3 11l9-8 9 8M5 10v10h14V10"/>',
  film:'<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7 4v16M17 4v16M3 9h4M3 15h4M17 9h4M17 15h4"/>',
  tv:'<rect x="3" y="5" width="18" height="12" rx="2"/><path d="M8 21h8M12 17v4"/>',
  cam:'<rect x="2" y="7" width="13" height="10" rx="2"/><path d="M15 10l7-3v10l-7-3z"/>',
  anime:'<path d="M12 3l2.4 5.2 5.6.6-4.2 3.8 1.2 5.6-5-2.9-5 2.9 1.2-5.6L4 8.8l5.6-.6z"/>',
  book:'<path d="M4 5.5A3.5 3.5 0 0 1 7.5 2H11v17H7.5A3.5 3.5 0 0 0 4 22zM20 5.5A3.5 3.5 0 0 0 16.5 2H13v17h3.5A3.5 3.5 0 0 1 20 22z"/>',
  folder:'<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  stack:'<rect x="3" y="4" width="7" height="7" rx="1.5"/><rect x="14" y="4" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="6" rx="1.5"/><rect x="14" y="14" width="7" height="6" rx="1.5"/>',
  search:'<circle cx="11" cy="11" r="7"/><path d="M20 20l-3.4-3.4"/>',
  act:'<path d="M3 12h4l3-8 4 16 3-8h4"/>',
  gear:'<circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M19.1 4.9L17 7M7 17l-2.1 2.1"/>',
};
// aria-hidden because every icon here sits next to its own text label; an
// unlabelled decorative SVG announced by a screen reader is noise. focusable
// is the legacy-Edge guard that keeps SVGs out of the tab order.
function catalogIcon(name){
  return `<svg class="px-ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"`
    +` stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">`
    +(CATALOG_GLYPHS[name]||CATALOG_GLYPHS.folder)+`</svg>`;
}
// Both spellings on purpose: LibraryDto.kind is plural ("movies"/"shows"/
// "home") while libCategory() hands back singular section keys ("movie" /
// "show" / "home"), and the sidebar is fed from whichever of the two is
// available. Anything unrecognised gets the folder, never a blank gutter.
function catalogLibGlyph(kind, anime){
  if(anime) return "anime";
  if(kind==="movie"||kind==="movies") return "film";
  if(kind==="show"||kind==="shows") return "tv";
  if(kind==="book"||kind==="books") return "book";
  if(kind==="home") return "cam";
  return "folder";
}

// ---- the sidebar's data problem -------------------------------------------
// The sidebar wants library names and item counts on every route, but chrome
// is synchronous (classicChrome is, and the router calls it the same way) and
// it may not fetch: the gate compares the request log per route, so a chrome
// that pulls /libraries on the item page is a failure even though the data is
// harmless. So the shell is painted from whatever is already in memory and
// filled in afterwards, the same shape pollActivity() uses for the status pill
// — render the container, fill it when the answer exists, be safe to run twice.
//
// Two sources, best first:
//   CATALOG_NAV    the home page model, which carries names, hrefs AND counts,
//               already grouped the way the user chose (category or share).
//   LIBS_CACHE  whatever a previously-visited item/library/category page left
//               behind. Names and hrefs, no counts. Read directly and never
//               through libsCached(), which fetches when cold.
// Cold first paint (a hard reload straight onto any route) has neither, and
// the sidebar renders with Home, Manage and the footer but no Libraries
// section at all — not a spinner and not a skeleton, because a heading over
// two grey bars is a claim about content we have not got. On the home route
// the gap closes as soon as loadHome() resolves; on other routes it closes on
// the next navigation that populates LIBS_CACHE.
let CATALOG_NAV=null;

function catalogNavFromHome(p){
  const byId={};
  for(const l of (p.libs||[])) byId[String(l.id)]={kind:l.kind, anime:!!l.anime};
  // Section keys are library ids under "share" grouping and category keys
  // under "category" grouping; the lookup covers the first and the fallthrough
  // to s.key covers the second.
  const secs=(p.sections||[]).map(s=>{
    const lib=byId[String(s.key)];
    return {name:s.name, href:s.href, count:s.total,
            glyph:catalogLibGlyph(lib?lib.kind:s.key, lib?lib.anime:false)};
  });
  // An empty array is an answer ("this server has no libraries"), not a miss,
  // so it is stored as one — otherwise the stale LIBS_CACHE fallback below
  // would resurrect libraries the user just deleted.
  CATALOG_NAV = secs.length ? secs : (p.libs||[]).map(l=>({
    name:l.name, href:`#/library/${l.id}`, count:null, glyph:catalogLibGlyph(l.kind, l.anime)}));
}
function catalogNavItems(){
  if(CATALOG_NAV) return CATALOG_NAV;
  if(LIBS_CACHE) return LIBS_CACHE.map(l=>({
    name:l.name, href:`#/library/${l.id}`, count:null, glyph:catalogLibGlyph(l.kind, l.anime)}));
  return null;
}
function catalogNavHtml(){
  const items=catalogNavItems();
  if(!items||!items.length) return "";
  const here=location.hash||"#/";
  const rows=items.map(it=>{
    // Exact-match highlight. An item page does not light its library up,
    // matching what classic's top nav does with the same information.
    const on = here===it.href;
    return `<a href="${esc(it.href)}" class="${on?"on":""}"${on?' aria-current="page"':""}>`
      +catalogIcon(it.glyph)
      // title= as well as the text: the label is clipped to one line, so a
      // library called "Kids — French dubs (external drive)" is otherwise
      // unreadable at 236px with no way to see the rest.
      +`<span class="px-lbl" title="${esc(it.name)}">${esc(it.name)}</span>`
      +(it.count==null?"":`<span class="px-n">${esc(String(it.count))}</span>`)
      +`</a>`;
  }).join("");
  return `<div class="px-sec">Libraries</div><nav class="px-nav">${rows}</nav>`;
}
// Idempotent by construction: it replaces the container's contents from the
// current best source, so calling it from chrome and again from the home body
// (and again on the next render) can only ever converge.
function catalogFillNav(){
  const box=document.getElementById("pxlibs");
  if(box) box.innerHTML=catalogNavHtml();
}

// ---- one-time wiring ------------------------------------------------------
// Two things the shipped markup cannot give a keyboard user, installed once
// per session and guarded so they can never touch another layout.
//
// A poster is a <div onclick=…>: clickable, but not focusable and not
// operable from the keyboard. The G2 acceptance list requires walking
// home → library → detail → play with a visible ring, so catalog adds tabindex
// and a role to every card the app renders, including the ones on routes it
// has not converted (library, search, item). A MutationObserver rather than a
// call at the end of each view because catalog does not own those views.
//
// The layoutId() guard is the important line: switching layout re-runs
// render(), which repaints classic's DOM under the same live observer. Without
// the check, classic would start emitting posters carrying tabindex — a diff
// in the byte-identical baseline the gate re-checks after every layout lands.
let CATALOG_WIRED=false;
function catalogWireOnce(){
  if(CATALOG_WIRED) return;
  CATALOG_WIRED=true;
  // Card reach is navKeyboardWireOnce()'s job on every layout now. What is
  // left here is the one thing only this layout has: the phone library sheet,
  // which Escape closes.
  navKeyboardWireOnce();
  document.addEventListener("keydown",e=>{
    if(layoutId()!=="catalog"||e.key!=="Escape") return;
    const modal=document.getElementById("modal");
    if(lightboxOpen()||(modal&&modal.classList.contains("open"))) return;
    const shell=document.getElementById("pxshell");
    if(shell&&shell.classList.contains("px-open")){ e.preventDefault(); catalogToggleLibs(); }
  });
}

// ---- phone: the library sheet and the search tab --------------------------
// The bottom tab bar exists in the DOM on every surface and CSS decides
// whether it is visible, so a resized window changes shape without a
// re-render. Two of the four tabs are routes; these are the other two.
//
// "Libraries" has no route to point at — there is no #/libraries index — so it
// reveals the sidebar's own library list as a full-screen sheet. Full screen
// rather than a drawer because a drawer has an "outside" to mis-tap on a phone,
// and it reuses the same nav element rather than a phone-only copy of it.
// Nothing has to close the sheet on navigation: routing repaints the chrome
// and the class goes with it.
function catalogToggleLibs(e){
  if(e){ e.preventDefault(); e.stopPropagation(); }
  const shell=document.getElementById("pxshell"); if(!shell) return;
  const open=!shell.classList.contains("px-open");
  shell.classList.toggle("px-open",open);
  const btn=document.querySelector('.px-tab[data-tab="libs"]');
  if(btn) btn.setAttribute("aria-expanded",open?"true":"false");
  // The page behind a full-screen sheet must not scroll under it — the same
  // lock openLightbox() takes, released the same way.
  document.body.style.overflow=open?"hidden":"";
  // Focus follows the sheet, or a keyboard user is left tabbing through a
  // panel that is no longer on screen.
  const target=open?document.querySelector("#pxnav .px-nav a"):btn;
  if(target&&target.focus) target.focus();
}
// The search field is the sidebar's, which on a phone is the header's. The tab
// puts the caret in it rather than routing anywhere, so there is exactly one
// #q in the document and getQ()/the debounce wiring keep working untouched.
function catalogFocusSearch(e){
  if(e){ e.preventDefault(); }
  const shell=document.getElementById("pxshell");
  if(shell&&shell.classList.contains("px-open")) catalogToggleLibs();   // the sheet covers the header
  const q=document.getElementById("q");
  if(q){ q.focus(); try{ q.select(); }catch(err){} }
}
// Two of the four are anchors and two are buttons, which is the honest markup:
// a tab that routes is a link and a tab that opens a panel is a button, and
// the difference is what a screen reader announces.
function catalogTabsHtml(active, homeOn){
  const on=b=>b?' class="px-tab on" aria-current="page"':' class="px-tab"';
  const discs=typeof opticalNavEnabled==="function"&&opticalNavEnabled()
    ? `<a href="#/discs" data-tab="discs"${on(active==="discs")}>${catalogIcon("film")}<span>Discs</span></a>`:"";
  return `<nav class="px-tabs" aria-label="Sections">
    <a href="#/" data-tab="home"${on(homeOn)}>${catalogIcon("home")}<span>Home</span></a>
    <a href="#/library-channels" data-tab="library-channels"${on(active==="library-channels")}>${catalogIcon("film")}<span>Channels</span></a>
    <a href="#/recordings" data-tab="recordings"${on(active==="recordings")}>${catalogIcon("act")}<span>Recordings</span></a>
    ${discs}
    <button type="button" data-tab="libs" class="px-tab" aria-expanded="false" aria-controls="pxnav" onclick="catalogToggleLibs(event)">${catalogIcon("stack")}<span>Libraries</span></button>
    <button type="button" data-tab="search" class="px-tab" onclick="catalogFocusSearch(event)">${catalogIcon("search")}<span>Search</span></button>
    <a href="#/activity" data-tab="activity"${on(active==="activity")}>${catalogIcon("act")}<span>Activity</span></a>
  </nav>`;
}

// ---- chrome ---------------------------------------------------------------
// Same contract as classicChrome(): assign #app, produce <main id="main"> with
// `inner` inside it, wire the 300 ms search debounce, end with pollActivity().
// The header's controls all move into the sidebar footer and every one of them
// calls the shipped popover functions — toggleThemeMenu / toggleAppearanceMenu
// / toggleSizeMenu / toggleProfileMenu — rather than a catalog copy of them, so
// the layout picker inside the theme menu keeps working from inside catalog and a
// change to any of those menus lands in both layouts at once.
function catalogChrome(active, inner){
  const searchFocus=captureSearchFocus();
  const hash=location.hash||"#/";
  // Library, category and search routes all call layoutChrome("home", …), so
  // `active==="home"` alone would light Home up while the user is looking at a
  // library that has its own row two lines below it. In a sidebar that lists
  // the libraries, Home means the home page.
  const homeOn = active==="home" && (hash==="#/"||hash==="#"||hash==="");
  // Settings appears for admins only, and nowhere else in the shell — the
  // acceptance list checks a non-admin sees no Settings anywhere in the
  // sidebar. (The account popover applies the same test to its own entry.)
  const admin = ME&&ME.is_admin
    ? `<a href="#/settings" class="${active==="settings"?"on":""}"${active==="settings"?' aria-current="page"':""}>`
      +catalogIcon("gear")+`<span class="px-lbl">Settings</span></a>`
    : "";
  const discs=typeof opticalNavEnabled==="function"&&opticalNavEnabled()
    ? `<a href="#/discs" class="${active==="discs"?"on":""}"${active==="discs"?' aria-current="page"':""}>${catalogIcon("film")}<span class="px-lbl">Discs</span></a>`:"";
  // ${APP_NAME}, never the literal product name — the shipped header's title
  // attribute still says "plurx" here; that is a bug this layout does not copy.
  const getapp = installAvailable()
    ? `<button class="ghost sm getappbtn" onclick="showInstall()" title="Install the ${APP_NAME} app">Get app</button>` : "";
  document.getElementById("app").innerHTML=`
   <div class="px-shell" id="pxshell">
     <aside class="px-side">
       <div class="px-head">
         <div class="px-brand"><a href="#/" class="logo">${APP_NAME}</a></div>
         <input class="search" id="q" placeholder="Search…" aria-label="Search ${APP_NAME}" value="${esc(getQ())}">
       </div>
       <div class="px-scroll" id="pxnav">
         <button class="ghost sm px-sheetx" type="button" aria-label="Close libraries" onclick="catalogToggleLibs(event)">✕</button>
         <nav class="px-nav" aria-label="Main">
           <a href="#/" class="${homeOn?"on":""}"${homeOn?' aria-current="page"':""}>${catalogIcon("home")}<span class="px-lbl">Home</span></a>
           <a href="#/live-tv" class="${active==="live-tv"?"on":""}"${active==="live-tv"?' aria-current="page"':""}>${catalogIcon("tv")}<span class="px-lbl">Live TV</span></a>
           <a href="#/recordings" class="${active==="recordings"?"on":""}"${active==="recordings"?' aria-current="page"':""}>${catalogIcon("act")}<span class="px-lbl">Recordings</span></a>
           <a href="#/library-channels" class="${active==="library-channels"?"on":""}"${active==="library-channels"?' aria-current="page"':""}>${catalogIcon("film")}<span class="px-lbl">Library channels</span></a>
           ${discs}
         </nav>
         <div id="pxlibs"></div>
         <div class="px-sec">Manage</div>
         <nav class="px-nav" aria-label="Manage">
           <a href="#/activity" class="${active==="activity"?"on":""}"${active==="activity"?' aria-current="page"':""}>${catalogIcon("act")}<span class="px-lbl">Activity</span></a>
           ${admin}
         </nav>
       </div>
       <div class="px-foot">
         <button class="dvr-global" id="dvr-global" onclick="location.hash='#/activity'" aria-label="Recording status"></button>
         <span class="activity" id="activity" onclick="location.hash='#/activity'"></span>
         <div class="px-ctl">
           <div class="px-pop">
             <button class="ghost sm lookbtn" onclick="toggleLookMenu(event)" title="Appearance — layout, theme, light or dark, poster size" aria-label="Appearance" aria-haspopup="menu"><span class="tsw"></span><span class="lookword">Appearance</span></button>
             <div class="amenu lookmenu" id="lookmenu"></div>
           </div>
           ${getapp}
         </div>
         <div class="px-user">
           <div class="px-pop" style="flex:1;min-width:0;display:flex">
             <button class="ghost sm profile" onclick="toggleProfileMenu(event)" title="Account">
               <span class="pavatar">${esc((ME&&ME.username?ME.username[0]:"?").toUpperCase())}</span><span class="pname">${esc(ME?ME.username:"")}</span><span class="caret">▾</span>
             </button>
             <div class="amenu" id="profilemenu"></div>
           </div>
         </div>
       </div>
     </aside>
     <main id="main">${inner}</main>
     ${catalogTabsHtml(active, homeOn)}
   </div>`;
  // Synchronous fill from whatever is cached; the home body calls it again
  // with the real thing a moment later.
  catalogFillNav();
  const q=document.getElementById("q");
  let t; q.addEventListener("input",()=>{ clearTimeout(t); t=setTimeout(()=>{ location.hash= q.value?("#/search/"+encodeURIComponent(q.value)):"#/"; },300); });
  restoreSearchFocus(searchFocus);
  catalogWireOnce();
  pollActivity();
}

// ---- home -----------------------------------------------------------------
// Same model, same helpers, different composition. The one structural change
// from classic is that a library section is a hub ROW with a "See all" rather
// than a wall of grid — that row, repeated per library, is what the catalog
// sidebar shell is built around, and it is the difference between a home page
// you scan and a home page you scroll.
//
// Hub headings keep the shipped wording ("Continue watching", "Next up"):
// user-facing strings are explicitly not a layout's to change, and a layout
// switch that renames familiar concepts would be needless churn.
function catalogHomeBody(p){
  // The sidebar is outside #main, so it survives the assignment that is about
  // to replace #main with this string — filling it here is what closes the
  // cold-paint gap on the home route.
  catalogNavFromHome(p);
  catalogFillNav();

  const hub=k=>(p.hubs&&p.hubs[k])||[];
  let hubs=installBannerHtml(), soon="", previews="";
  // watchCtx on the first two rails: those rows are about where you are in
  // something, so the card carries "23m left · ends ~10:45 PM".
  if(hub("continue_watching").length) hubs+=rail("Continue watching", hub("continue_watching").map(i=>card(i,true)).join(""));
  if(hub("next_up").length)           hubs+=rail("Next up",           hub("next_up").map(i=>card(i,true)).join(""));
  if(hub("recently_added").length)    hubs+=rail("Recently added",    hub("recently_added").map(i=>card(i)).join(""));
  if(p.soon&&p.soon.entries&&p.soon.entries.length) soon+=rail("Coming soon", p.soon.entries.map(comingCard).join(""));

  if(p.hubsError) hubs+=`<div class="hint">Home recommendations are unavailable right now.</div>`;
  if(p.previewsPending){
    previews+=`<div class="empty">Loading library previews…</div>`;
  }else if(p.previewsError){
    previews+=`<div class="empty">Library previews are unavailable right now.</div>`;
  }else if(p.libs.length){
    previews+=groupToggleHtml(p.group);
    for(const s of p.sections){
      const total=s.total==null?"":`<span class="px-n">${esc(String(s.total))}</span>`;
      // rail() interpolates its heading as HTML (classic passes plain text),
      // which is what lets the "see all" link ride in the heading instead of
      // needing a second rail function. Both interpolations are esc()'d.
      const head=`${esc(s.name)}<a class="px-seeall" href="${esc(s.href)}">See all${total}</a>`;
      // Two cases still take a grid. An empty section, because rail() on an
      // empty row renders a heading over nothing while grid([]) renders the
      // "Nothing here yet." card the rest of the app uses; and any section
      // holding photos, because grid() is what populates PHOTO_SET and without
      // it the lightbox opens on one picture and cannot page to the next.
      const photos=s.items.some(i=>i.kind==="photo");
      previews += (!s.items.length||photos)
        ? `<h2 class="section">${head}</h2>${grid(s.items)}`
        : rail(head, s.items.map(i=>card(i)).join(""));
      if(s.unavailable) previews+=`<div class="hint">Some library previews are unavailable right now.</div>`;
    }
  }else{
    previews+=`<div class="empty">No libraries yet.${ME&&ME.is_admin?' Add one in <a href="#/settings/libraries">Settings</a>.':""}</div>`;
  }
  return opticalHomeHtml(p)+
    `<div data-home-region="hubs" data-home-slot="hubs">${hubs}</div>`+
    `<div data-home-region="soon" data-home-slot="soon">${soon}</div>`+
    `<div data-home-region="previews" data-home-slot="previews">${previews}</div>`;
}
LAYOUTS.catalog={name:"Catalog", surfaces:["desktop","mobile","tv"], chrome:catalogChrome, views:{home:catalogHomeBody}};
// applyLayout() ran in the <head>, where the registry holds classic and
// nothing else — so layoutId() could not recognise a stored "catalog" preference
// and left <html data-layout="classic">. Every rule in this layout's
// stylesheet is scoped under [data-layout=catalog]; without this line the catalog
// chrome renders wearing none of them, which looks exactly like a stylesheet
// that failed to load. Re-applying here is invisible: boot() has not resolved
// /server yet, so #app is empty and there is nothing painted to reflow.
// The tidier fix is a {name,surfaces} stub for catalog in the <head> registry
// beside classic's, which is where the pre-paint pass expects to find it —
// that is an edit to the <head> script, outside this fragment.

// ---- layout: catalog — G2b: item detail, library, and the three logged defects
//
// Paste into the body <script>, after the G2 catalog block (it extends the same
// LAYOUTS.catalog object) and before boot().
//
// What this file deliberately does NOT contain, same as the G2 drop: a card
// renderer, a rail renderer, a grid renderer, an escape function, or a fetch.
// Both new views are compositions of shipped helpers — pageHead, artHtml,
// specBadges, specBlock, unprobedNote, watchControls, episodeRow, grid,
// playerMeta, fmtDur, endsAt, esc — wearing catalog CSS. A second card function
// is how five layouts become five apps.
//
// Two seam shapes, per §3.2:
//   views.item     whole-body     (page) -> html for #main
//   views.library  region-shaped  {shell, items, count}, resolved PER REGION
// catalog overrides `shell` only. `items` and `count` are inherited from classic
// untouched, because catalog does not differ from classic about what a grid of
// posters or the word "items" is — see NOTES §3.

// ---------------------------------------------------------------------------
// 1. stickyFloor — where the layout's own chrome stops and content begins.
// ---------------------------------------------------------------------------
// The A–Z scroll-spy asks "which letter is at the top of the reading area?" and
// needs a viewport y to measure against. classic answers with header.top's
// bottom; catalog has no header.top on desktop, so the shipped fallback (56px)
// highlights a letter a row too late.
//
// catalog has three shapes and this returns the truth for each:
//   desktop  the sidebar is a full-height column BESIDE the content and
//            nothing is pinned above the grid           -> 0
//   narrow   the same element re-flows into a sticky top header (the CSS at
//            max-width:899px)                           -> its measured bottom
//   tv       the sidebar is a fixed full-height rail on the LEFT -> 0
//
// Measured, not a constant and not a media-query branch: the narrow header
// wraps to two or three rows depending on whether the status pill and the
// Get-app button are in it, so the only honest answer is the element's own
// rect. It is also the same answer whether the window was resized or the page
// re-rendered, which a JS surface branch would not be.
function catalogStickyFloor(){
  const side=document.querySelector(".px-side");
  if(!side) return 0;                                  // chrome not painted yet
  const r=side.getBoundingClientRect();
  const vh=window.innerHeight||0;
  // A full-height element is the desktop column or the TV rail: it is beside
  // the content, not above it, so it floors nothing.
  if(!vh || r.height>=vh*0.6) return 0;
  // A short band that is not pinned to the top edge is not acting as chrome
  // above the content either (it has scrolled away, or it is hidden).
  if(r.top>2 || r.bottom<=0) return 0;
  return Math.max(0, r.bottom);
}

// ---------------------------------------------------------------------------
// 2. CATALOG_NAV staleness after invalidateLibs()  (logged defect, NOTES §5.7)
// ---------------------------------------------------------------------------
// invalidateLibs() is the app's single "the library list changed" signal —
// renaming, adding or deleting a library in Settings all call it and then
// re-enter viewSettings(), which calls layoutChrome() and therefore
// catalogFillNav(). Everything needed to repaint already happens; the only thing
// missing is that CATALOG_NAV, a cache DERIVED from the home model, is not part of
// what gets invalidated, so the sidebar repaints from a list that still
// contains the library you just deleted.
//
// The mechanism is the one the app just grew for stickyFloor: ask the layout if
// it defines a hook. `LAYOUTS[cur].libsChanged()` is the sibling of
// `LAYOUTS[cur].stickyFloor()` — optional, absent on classic, and it keeps
// layout-specific names out of shared code (the alternative, `CATALOG_NAV=null`
// inside invalidateLibs(), puts one layout's variable in a function every
// layout calls). See NOTES §4 for the two other candidates and why not.
function catalogLibsChanged(){
  CATALOG_NAV=null;        // the derived cache, not the source
  catalogFillNav();        // idempotent by construction — safe here and again on render
}

// ---------------------------------------------------------------------------
// 4. Library — shell only (items and count are inherited from classic)
// ---------------------------------------------------------------------------
// The shell is rendered ONCE per entry into #main and owns the frame: page
// head, the Library/Collections/Categories tabs, the toolbar, and the three
// region mounts (#libbody, #libpager, #librail). It must never be re-rendered
// per batch, and #libbody's contents — not #libbody itself — are what the
// loader replaces as pages land. That is the whole reason this route is
// region-shaped: a 200-item library redraws its grid five or six times while
// loading, and a body(model) contract would take the reader's scroll position
// with it every time.
//
// catalog moves the A–Z rail out of the viewport gutter and into the content row
// (mount placement, not a reimplementation — the rail markup is still
// alphaRailHtml()'s), which is the mount-moving the seam exists to allow.
//
// The scroller stays the WINDOW, as in classic. Making #libbody an
// `overflow:auto` box would satisfy a literal reading of "#libbody is the thing
// that scrolls", but it would silently kill the shipped scroll-spy (one window
// listener, and scroll events do not bubble), and it would make the page head
// and the pager unreachable while reading the grid. What the contract is
// actually protecting is that the scroll CONTAINER survives a batch, and it
// does: the container is the document, the shell is not rebuilt, and only
// #libbody's innerHTML changes. Proven in the harness, not asserted — NOTES §8.

// Why a tab or a control is dead, in plain words, in one place. These strings
// are user-facing: they say what is missing and when it arrives, because a tab
// that looks live and does nothing is worse than one that admits it isn't
// ready.
const CATALOG_WHY_COLLECTIONS="Collections don’t exist on the server yet — there is nothing behind this tab to list. It arrives when the server learns what a collection is.";
const CATALOG_WHY_CATEGORIES="Categories needs genre data — coming with the metadata milestone.";
const CATALOG_WHY_LIST="List view needs the codec, HDR, audio and size columns, and the library response doesn’t carry them yet — coming with the browse-data milestone. Asking for them per row would be one request per item, so it stays off until the list itself carries them.";

// A control that is visibly, announcedly and structurally not ready.
//  - a <span>, so it is not focusable and not an action: there is nothing to
//    press and nothing to land on with Tab
//  - aria-disabled, so anything that does reach it is told
//  - a "soon" chip, so "disabled" is not carried by grey alone
//  - the reason in title= for the pointer, and in a visually-hidden span for
//    everyone who never sees a tooltip
function catalogSoon(cls, label, why){
  return `<span class="${cls} off" aria-disabled="true" title="${esc(why)}">`
    +`${esc(label)}<span class="px-soon" aria-hidden="true">soon</span>`
    +`<span class="px-sr"> — ${esc(why)}</span></span>`;
}

function catalogLibraryShell(m){
  const sort=m.sort;
  // Library is the live tab and is the page you are on, so it is an <a> to
  // this same route with aria-current — not a button that re-fetches.
  const tabs=`<nav class="px-ltabs" aria-label="Library views">
      <a class="px-ltab on" href="${esc(m.href)}" aria-current="page">Library</a>
      ${catalogSoon("px-ltab","Collections",CATALOG_WHY_COLLECTIONS)}
      ${catalogSoon("px-ltab","Categories",CATALOG_WHY_CATEGORIES)}
    </nav>`;
  // Grid is not a button either: it is the only view, so it is a state, not a
  // choice. Making it pressable would promise a toggle that has one position.
  const view=`<span class="px-view" role="group" aria-label="View">
      <span class="px-vt on" aria-current="true"><span aria-hidden="true">▦</span> Grid</span>
      ${catalogSoon("px-vt","☰ List",CATALOG_WHY_LIST)}
    </span>`;
  // The selects are classic's, verbatim, including the onchange strings: they
  // are behaviour, and a layout that retypes them is a layout that can get
  // them subtly wrong. The wrapper keeps the shipped `libbar` class because
  // libGoPage() scrolls `#main .libbar` into view after a page change.
  const bar=`<div class="libbar px-libbar">
    <div class="row">
      ${view}
      <span class="lbl">Sort</span>
      <select aria-label="Sort" onchange="LIB_SORT=this.value;${m.reload}">
        ${libOpt("title","Title (A–Z)",sort)}${libOpt("added","Recently added",sort)}${libOpt("recorded","Date recorded",sort)}${libOpt("year","Year",sort)}${libOpt("resolution","Resolution",sort)}</select>
      <span class="lbl" style="margin-left:8px">Show</span>
      <select data-library-watch-filter aria-label="Show" onchange="LIB_FILTER=this.value;${m.reload}">
        ${libOpt("all","Everything",LIB_FILTER)}${libOpt("unwatched","Unwatched",LIB_FILTER)}${libOpt("inprogress","In progress",LIB_FILTER)}${libOpt("watched","Watched",LIB_FILTER)}</select>
      <span class="lbl" style="margin-left:8px">Per page</span>
      <select aria-label="Per page" onchange="setPerPage(this.value);libGoPage(0)">
        ${LIB_SIZES.map(n=>libOpt(String(n),String(n),String(LIB_PER))).join("")}${libOpt("all","All",String(LIB_PER))}</select>
    </div>
    <span class="muted" id="libcount"></span></div>`;
  const head=pageHead([{href:"#/",label:"Home"},{label:m.title}],m.title);
  // #libbody and #librail are siblings in a row: that is how the scrubber gets
  // to the right edge of the grid instead of the right edge of the window.
  // #libpager sits outside the row so it stays centred under the grid.
  return `${head}${tabs}${bar}`
    +`<div class="px-libwrap">`
    +`<div id="libbody"><div class="empty">Loading…</div></div>`
    +`<div class="px-librail" id="librail"></div>`
    +`</div><div id="libpager"></div>`;
}

// ---------------------------------------------------------------------------
// 5. Item detail — whole body
// ---------------------------------------------------------------------------
// Same model, same branches, same helpers as classicItemBody, re-composed as
// an art-forward hero: full-width backdrop under a scrim, poster + oversized
// title, metadata as pills, accent Play lockup (§3.2). Every branch classic
// has is here — photo, playable file, missing file, multiple versions, season
// episode list, children grid, empty folder, rollup chips, tags, watch
// controls, the edit and refresh-artwork admin buttons, breadcrumbs.
//
// The one structural difference: classic puts EVERYTHING inside the hero's
// text column, so a 26-episode list renders in a 1fr column beside a poster.
// catalog splits the page at the natural seam — the hero carries identity and the
// play lockup, and the detail below it (version cards, episode list, children
// grid) gets the full width. Nothing moves between the two that changes what
// is on the page; the file-missing warning stays WITH the play lockup because
// it is the answer to "can I play this", not a footnote.
function catalogItemBody(p){
  const d={files:p.files, ancestors:p.ancestors, children:p.children};
  const it=p.item, id=p.id, editable=p.editable;

  // Admin affordances. px-admin is the hook the TV surface hides them with —
  // see the television block in styles.css and NOTES §5.
  const artBtn = ME&&ME.is_admin
    ? ` <button class="ghost sm px-admin" title="Re-fetch this item's poster and backdrop" aria-label="Refresh artwork" onclick="refreshArtwork(${it.id},this)">⟳ Refresh artwork</button>`
    : '';
  const editBtn = editable
    ? ` <button class="ghost sm px-admin" title="Edit details" aria-label="Edit details" onclick='openEdit(${esc(JSON.stringify(it))})'>✎ Edit</button>`
    : '';

  const trail=[{href:"#/",label:"Home"}]
    .concat(NAV_ORIGIN?[{href:NAV_ORIGIN.href,label:NAV_ORIGIN.label}]:[])
    .concat((d.ancestors||[]).map(a=>({href:`#/item/${exactWireId(a)}`,label:a.title})))
    .concat([{label:it.title}]);
  const crumbs=pageHead(trail);

  const best=d.files[0];
  const multi=d.files.length>1;
  const runtime=it.runtime_ms||(best&&best.duration_ms);

  // Pills. Identical content to classic's chips with one move: the KIND rides
  // above the title as the kicker ("MOVIE", "EPISODE") the way catalog labels a
  // detail page, instead of sitting in the middle of the pill row. It is moved,
  // never dropped — an item with a kind still says so on the page.
  const chips=[];
  if(it.recorded_at){ chips.push(`<span>${esc(fmtDate(it.recorded_at))}</span>`); }
  if(it.kind==='show'){
    const ys=(d.children||[]).map(c=>airYear(c.air_date)).filter(Boolean);
    if(it.year) ys.push(it.year);
    const y0=airYear(it.air_date); if(y0) ys.push(y0);
    if(ys.length){ const lo=Math.min(...ys), hi=Math.max(...ys); chips.push(`<span>${lo}${hi>lo?'–'+hi:''}</span>`); }
  } else if(it.kind==='season'){
    const y=it.year||airYear(it.air_date); if(y) chips.push(`<span>${y}</span>`);
  } else if(it.year){ chips.push(`<span>${it.year}</span>`); }
  if(runtime) chips.push(`<span>${fmtDur(runtime)}</span>`);
  if(it.rollup&&it.rollup.leaves){
    const r=it.rollup;
    chips.push(`<span>${r.watched===r.leaves?`all ${r.leaves} watched`:`${r.watched} of ${r.leaves} watched`}</span>`);
  }
  const kick = it.kind ? `<div class="px-kick">${esc(it.kind)}</div>` : '';
  // Video badges show once: in the hero for a single version; per-version
  // below when there are several.
  const heroBadges = (best && !multi) ? specBadges(best) : "";

  // `acts` lands in the hero (the play lockup and anything that answers "can I
  // play this"); `rest` lands below it at full width.
  let acts="", rest="";
  if(p.shape==="photo"){
    // A photo is a picture, not a playback session.
    PHOTO_SET=[{id:it.id,title:it.title,recorded_at:it.recorded_at}];
    acts=`<div class="actions"><button class="btnplay" onclick="openLightbox(${it.id})">View full size</button></div>`;
    rest=`<img class="art px-photo" style="aspect-ratio:auto;max-height:60vh;width:auto;border-radius:var(--radius);cursor:zoom-in"
           src="${esc(photoUrl(it.id,'thumb'))}" alt="" onclick="openLightbox(${it.id})">`;
  } else if(p.shape==="book"){
    acts=bookActions(p);
    rest=bookVersions(p);
  } else if(p.shape==="versions"){
    const pmeta=p.meta, resume=p.resume;
    const watchBtn=watchControls(it);
    const playable=p.playable;
    if(playable){
      acts=`<div class="actions">
        <button class="btnplay" onclick='play(${playable.id},${esc(JSON.stringify(it.title))},${resume},${playable.duration_ms||0},${esc(JSON.stringify(pmeta))})'>▶ ${resume?`Resume · ${fmtDur(resume)}`:'Play'}</button>
        ${resume?`<button class="ghost" onclick='play(${playable.id},${esc(JSON.stringify(it.title))},0,${playable.duration_ms||0},${esc(JSON.stringify(pmeta))})'>Start over</button>`:''}
        ${watchBtn}</div>`;
      const pdur=playable.duration_ms||runtime||0, prem=resume?Math.max(0,pdur-resume):pdur, pend=endsAt(prem);
      if(prem) acts+=`<div class="playinfo">${resume?fmtDur(prem)+" left":fmtDur(prem)}${pend?" · ends ~"+pend:""}</div>`;
    } else {
      // `mp`, not `p`: classic can shadow its page argument here because it
      // never uses it again. This function's `p` is still live.
      const mp=best.missing_path;
      acts=`<div class="filemissing">⚠ This file is missing on the server, so it can’t be played — its library path may be unmounted, moved, or renamed${ME.is_admin&&mp?`:<br><code>${esc(mp)}</code>`:'.'}</div><div class="actions">${watchBtn}</div>`;
    }
    rest=d.files.map(v=>{
      const miss=!v.available;
      const header=(multi||miss)
        ? `<div class="vh"><span class="vbadges">${specBadges(v)||`<span class="vt">${esc(v.filename)}</span>`}${miss?'<span class="missbadge">missing</span>':''}</span>${(!miss&&multi)?`<button class="ghost sm" onclick='play(${v.id},${esc(JSON.stringify(it.title))},${resume},${v.duration_ms||0},${esc(JSON.stringify(pmeta))})'>▶ Play</button>`:''}</div>`
        : '';
      return `<div class="version">${header}${specBlock(v)}${miss&&ME.is_admin&&v.missing_path?`<div class="problem">${esc(v.missing_path)}</div>`:''}${unprobedNote(v,it.id)}${dvFileActionMount(v)}${fileAdminActions(v,it.id)}</div>`;
    }).join("");
  } else if(p.shape==="episodes"){
    const a=watchControls(it);
    if(a) acts=`<div class="actions">${a}</div>`;
    rest=`<h2 class="section">${d.children.length} episode${d.children.length>1?'s':''}</h2>
      <div class="eplist">${d.children.map(episodeRow).join("")}</div>`;
  } else if(p.shape==="children"){
    const label = it.kind==='folder'? 'Contents'
                : p.childLabel==='Seasons'? 'Seasons' : 'Episodes';
    const a=watchControls(it);
    if(a) acts=`<div class="actions">${a}</div>`;
    rest=`<h2 class="section">${label}</h2>${grid(d.children)}`;
  } else if(it.kind==='folder'){
    rest=`<div class="empty">This folder is empty.</div>`;
  }
  rest+=bookEditionSection(p);

  // The backdrop is the shipped .herobg (mask, opacity and the noirr grain
  // treatment all keep working); .px-scrim is catalog's extra wash, and it is a
  // --bg mix, so on a light theme it lightens rather than painting a black
  // band across a white page.
  const bg=it.backdrop?`<div class="herobg" style="background-image:url(${esc(tok(it.backdrop))})"></div>`:'';
  return `<div class="hero px-hero${it.backdrop?'':' px-noart'}">${bg}
    <div class="px-scrim" aria-hidden="true"></div>
    <div class="dwrap">
      <div class="poster">${artHtml(it)}</div>
      <div class="dhead">${crumbs}${kick}
        <h1>${esc(it.title)}${editBtn}${artBtn}</h1>${bookByline(it)}
        ${chips.length?`<div class="chips">${chips.join("")}</div>`:''}
        ${(it.tags&&it.tags.length)?`<div class="chips px-tags">${it.tags.map(t=>`<span>${esc(t)}</span>`).join("")}</div>`:''}
        ${heroBadges?`<div class="vbadges" style="margin-top:10px">${heroBadges}</div>`:''}
        ${it.overview?`<p class="overview">${esc(it.overview)}</p>`:''}
        ${acts}</div>
    </div></div>${rest?`<div class="px-detail">${rest}</div>`:''}`;
}

// ---- registration ---------------------------------------------------------
// Extends the object the G2 block registered; `views` already exists there,
// but the guard makes the paste order one less thing to get wrong.
LAYOUTS.catalog.views = LAYOUTS.catalog.views || {};
LAYOUTS.catalog.views.item = catalogItemBody;
// Region-shaped, and deliberately partial: `items` and `count` fall through to
// classic per region, which is what the per-region fallback is for.
LAYOUTS.catalog.views.library = {shell:catalogLibraryShell};
LAYOUTS.catalog.stickyFloor = catalogStickyFloor;
LAYOUTS.catalog.libsChanged = catalogLibsChanged;
