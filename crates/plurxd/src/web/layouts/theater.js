"use strict";
// ---- layout: theater — G4 ---------------------------------------------------
//
// Paste into the body <script> of crates/plurxd/src/web/index.html, after the
// catalog block and before the router (`let PAGE_TIMER=null;`).
//
// A `{name, surfaces}` stub must ALSO go into the <head> registry beside
// classic's and catalog's — see NOTES §1. Without it applyLayout() cannot resolve
// a stored "theater" preference, <html data-layout> stays "classic", and not
// one scoped rule in styles.css matches. It debugs like a stylesheet that
// failed to load; it bit the catalog port.
//
// What this file deliberately does NOT contain, same as the two drops before
// it: a card renderer, a rail renderer, a grid renderer, an escape function,
// or a fetch. Every view here is a composition of shipped helpers — card(),
// rail(), grid(), comingCard(), installBannerHtml(), groupToggleHtml(),
// specBadges(), specBlock(), unprobedNote(), episodeRow(), watchControls(),
// pageHead(), artHtml(), photoUrl(), progressPct(), watchLine(), fmtDur(),
// fmtDate(), endsAt(), esc(), tok(). Larger posters and hover captions are CSS
// on that same shared card (`--poster`, `.meta` opacity), not a second card
// function. A second card function is how five layouts become five apps, and
// G4 is where that thesis gets tested.
//
// And it re-derives NOTHING that the page models carry. R4 landed with the G3
// record precisely because catalogItemBody re-derived ~70 lines of decisions from
// classicItemBody — the year range, the resume threshold, the playable file,
// the version-header rule, the children label. theaterItemBody switches on
// `p.shape` and reads `p.resume` / `p.playable` / `p.meta` / `p.years` /
// `p.runtime` / `p.childLabel` / `p.best` / `p.multi` / `p.editable`. Where a
// decision is still missing from the model it is rendered AND logged as a
// model gap in NOTES §5 rather than quietly re-derived a third time.
//
// Three seam shapes, per §3.2:
//   chrome         (active, inner) -> assigns #app
//   views.item     whole-body      (page) -> html for #main
//   views.home     whole-body      (page) -> html for #main
//   views.library  region-shaped   {shell}, resolved PER REGION; `items` and
//                  `count` are inherited from classic untouched, because
//                  theater does not differ from classic about what a grid of
//                  posters or the word "items" is.

// ---------------------------------------------------------------------------
// 1. Chrome — a translucent bar over the art
// ---------------------------------------------------------------------------
// Same contract as classicChrome() and catalogChrome(): assign #app, produce
// <main id="main"> with `inner` inside it, wire the 300 ms search debounce,
// end with pollActivity().
//
// The bar is fixed and floats ON the content rather than above it (styles.css
// cancels main's lead padding inside the hero), which is the one structural
// thing theater does that neither other layout does.
//
// Navigation is emitted TWICE and CSS decides which copy is real: the desktop
// row (.th-nav) and the phone's floating pill group (.th-pills), per plan
// §2.5. Same reason catalog emits its sidebar and its tab bar on every surface —
// a window dragged narrow changes shape live, with no re-render and no stale
// JS branch. Both copies are anchors, both carry aria-current, and the two
// never disagree because they are built from one list.
//
// Every control in the bar calls the SHIPPED popover functions —
// toggleThemeMenu / toggleAppearanceMenu / toggleSizeMenu / toggleProfileMenu
// — rather than a theater copy of them, so the layout picker inside the theme
// menu keeps working from inside theater and a change to any of those menus
// lands in all three layouts at once.
function theaterNavItems(active){
  const hash=location.hash||"#/";
  // Library, category and search routes all call layoutChrome("home", …), so
  // `active==="home"` alone would light Home while the user is looking at a
  // library. Home means the home page.
  const homeOn = active==="home" && (hash==="#/"||hash==="#"||hash==="");
  const items=[{tab:"home", href:"#/", label:"Home", on:homeOn},
               {tab:"live-tv", href:"#/live-tv", label:"Live TV", on:active==="live-tv"},
               {tab:"recordings", href:"#/recordings", label:"Recordings", on:active==="recordings"},
               {tab:"library-channels", href:"#/library-channels", label:"Library channels", on:active==="library-channels"},
               {tab:"activity", href:"#/activity", label:"Activity", on:active==="activity"}];
  // Settings appears for admins only, and nowhere else in the shell — a
  // non-admin sees no Settings anywhere in theater's chrome. (The account
  // popover applies the same test to its own entry.)
  if(ME&&ME.is_admin) items.push({tab:"settings", href:"#/settings", label:"Settings", on:active==="settings"});
  return items;
}
function theaterNavHtml(active){
  return theaterNavItems(active).map(i=>
    `<a href="${i.href}" data-tab="${i.tab}" class="${i.on?"on":""}"${i.on?' aria-current="page"':""}>${esc(i.label)}</a>`
  ).join("");
}
function theaterChrome(active, inner){
  const searchFocus=captureSearchFocus();
  // ${APP_NAME}, never the literal product name — the shipped header's title
  // attribute still says "plurx" here; that is a bug this layout does not copy.
  const getapp = installAvailable()
    ? `<button class="ghost sm getappbtn" onclick="showInstall()" title="Install the ${APP_NAME} app">Get app</button>` : "";
  document.getElementById("app").innerHTML=`
   <header class="th-top">
     <a href="#/" class="logo">${APP_NAME}</a>
     <nav class="th-nav" aria-label="Main">${theaterNavHtml(active)}</nav>
     <span class="spacer"></span>
     <button class="dvr-global" id="dvr-global" onclick="location.hash='#/activity'" aria-label="Recording status"></button>
     <span class="activity" id="activity" onclick="location.hash='#/activity'"></span>
     <input class="search" id="q" placeholder="Search…" aria-label="Search ${APP_NAME}" value="${esc(getQ())}">
     <div class="th-ctl">
       <div class="th-pop">
         <button class="ghost sm lookbtn" onclick="toggleLookMenu(event)" title="Appearance — layout, theme, light or dark, poster size" aria-label="Appearance" aria-haspopup="menu"><span class="tsw"></span><span class="lookword">Appearance</span></button>
         <div class="amenu lookmenu" id="lookmenu"></div>
       </div>
       ${getapp}
       <div class="th-pop">
         <button class="ghost sm profile" onclick="toggleProfileMenu(event)" title="Account">
           <span class="pavatar">${esc((ME&&ME.username?ME.username[0]:"?").toUpperCase())}</span><span class="pname">${esc(ME?ME.username:"")}</span><span class="caret">▾</span>
         </button>
         <div class="amenu" id="profilemenu"></div>
       </div>
     </div>
   </header>
   <main id="main">${inner}</main>
   <nav class="th-pills" aria-label="Sections">${theaterNavHtml(active)}</nav>`;
  const q=document.getElementById("q");
  let t; q.addEventListener("input",()=>{ clearTimeout(t); t=setTimeout(()=>{ location.hash= q.value?("#/search/"+encodeURIComponent(q.value)):"#/"; },300); });
  restoreSearchFocus(searchFocus);
  theaterWireOnce();
  pollActivity();
}

// ---------------------------------------------------------------------------
// 2. The hero — which item, and why that one
// ---------------------------------------------------------------------------
// Plan §3.3 and the G4 entry conditions: the hero is your actual
// continue-watching item, not a promo; it falls back to the newest
// recently-added when the continue hub is empty; and it is PLAYABLE VIDEO
// ONLY — never a folder, a photo, or a show/season container. Home video and
// photo libraries still render as folders and grids below; they just never
// front the page.
//
// Derived from the home model with no extra fetch: `p.hubs.continue_watching`
// and `p.hubs.recently_added` are already on the page model, already ordered
// newest-first by the server, and this function only filters and takes the
// first survivor. Two hub arrays in, one item out.
//
// `next_up` is deliberately NOT a source. It is what you have not started, and
// a hero captioned "continue watching" pointing at an episode you have never
// seen is a lie about your own history; when nothing is in progress the honest
// alternative is "new in your library", which is what the fallback says.
const THEATER_HERO_KINDS={movie:1, episode:1, video:1};
function theaterHeroPick(p){
  const hubs=(p&&p.hubs)||{};
  const ok=it=>!!it && THEATER_HERO_KINDS[it.kind]===1;
  // Array order IS recency — the server returns both hubs newest-first, and
  // re-sorting them here would be this layout inventing an opinion about data
  // it does not own.
  const cont=(hubs.continue_watching||[]).filter(ok)[0];
  // "Resume" vs "Play" comes from WHICH HUB the item came from, never from a
  // position threshold recomputed here. The 3-second resume rule lives in
  // exactly one place — loadItem()'s `resume` — and the continue-watching hub
  // is the server's own answer to "what am I in the middle of". A layout that
  // rounded that threshold differently would offer to resume something the
  // player then restarts, which is the class of bug R4 exists to end.
  if(cont) return {item:cont, kick:"Continue watching", resuming:true};
  const fresh=(hubs.recently_added||[]).filter(ok)[0];
  if(fresh) return {item:fresh, kick:"New in your library", resuming:false};
  // Nothing playable anywhere: a photo-only server, a brand-new install, a
  // library of folders. The home body degrades to rails with no hero rather
  // than throwing or fronting a folder.
  return null;
}
function theaterHeroHtml(pick){
  const it=pick.item;
  // The backdrop the model already carries, and nothing else. artHtml() would
  // reach for `poster` first, which is a 2:3 image and the WRONG picture for a
  // 21:9 band — and on the cards below it is already being loaded at card
  // size, so using it here would be a second full-resolution decode of an
  // image the page has in another size. One URL, one request, and it is the
  // same URL the item's own detail page uses, so clicking through is a cache
  // hit rather than a fresh load.
  //
  // No backdrop — which is most home media, most episodes, and every photo —
  // degrades to the token-mixed wash painted by .th-hero itself. Nothing to
  // load, nothing to wait for, and it is a --bg/--accent mix so it is on-theme
  // in all twenty-two palettes rather than a grey rectangle.
  const bg=it.backdrop
    ? `<div class="th-bg" style="background-image:url(${esc(tok(it.backdrop))})" aria-hidden="true"></div>` : "";
  // The kicker carries the show name for an episode, so the headline can be
  // the episode's own title without card()'s show-title swap being copied
  // here — see NOTES §5.6.
  const kick = it.kind==='episode' && it.show_title
    ? `${pick.kick} · ${it.show_title}` : pick.kick;

  const bits=[];
  if(it.kind==='episode' && (it.season_number!=null||it.episode_number!=null))
    bits.push(`<span>S${it.season_number||0}E${it.episode_number||0}</span>`);
  if(it.kind==='video' && it.recorded_at) bits.push(`<span>${esc(fmtDate(it.recorded_at))}</span>`);
  else if(it.year) bits.push(`<span>${esc(String(it.year))}</span>`);
  // watchLine() is the shipped "23m left · ends ~10:45 PM" helper and it owns
  // the position threshold itself, so the hero gets the same sentence the
  // Continue-watching cards get, from the same code. It returns "" for a
  // finished item, which is correct: nothing to say.
  const left=watchLine(it, true);

  const pct=progressPct(it.watch);
  const prog = pct>0&&pct<100
    ? `<div class="th-hprog" role="progressbar" aria-label="Progress" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${Math.round(pct)}"><i style="width:${pct}%"></i></div>` : "";

  // The primary action plays. It does not reimplement play(): it sets the
  // shipped AUTOPLAY handoff and navigates, which is exactly what an episode
  // row's ▶ button does. viewItem() then starts the file loadItem() picked, at
  // the position loadItem() computed, with the metadata loadItem() built — so
  // the hero needs no file id, no resume value and no /items/N fetch of its
  // own, and the request log for the home route is unchanged.
  const label = pick.resuming ? "▶ Resume" : "▶ Play";
  return `<section class="th-hero${bg?'':' th-noart'}" aria-label="Featured">${bg}
    <div class="th-scrim" aria-hidden="true"></div>
    <div class="th-fg">
      <div class="th-kick">${esc(kick)}</div>
      <h1 class="th-title">${esc(it.title)}</h1>
      <div class="th-meta">${bits.join("")}${left}</div>
      ${it.overview?`<p class="th-desc">${esc(it.overview)}</p>`:""}
      ${prog}
      <div class="actions">
        <button class="btnplay" onclick="AUTOPLAY='${exactWireId(it)}';location.hash='#/item/${exactWireId(it)}'">${label}</button>
        <button class="ghost" onclick="location.hash='#/item/${exactWireId(it)}'">ⓘ Details</button>
      </div>
    </div>
  </section>`;
}

// Each Home layout retains its original composition and asynchronous regions.
function theaterHomeBody(p){
  const hub=k=>(p.hubs&&p.hubs[k])||[];
  const pick=theaterHeroPick(p);
  // The hero is emitted FIRST and outside .th-rails: styles.css cancels main's
  // gutter and lead padding on it so it runs full-bleed under the fixed bar.
  // With no hero, main's own padding already clears the bar and the page
  // simply starts with the install banner — the empty-continue-hub and
  // empty-server paths are the same code path, not a special case.
  let hero = pick ? theaterHeroHtml(pick) : "";
  let hubs=installBannerHtml(), soon="", previews="";

  // watchCtx on the first two rails: those rows are about where you are in
  // something, so the card carries "23m left · ends ~10:45 PM".
  if(hub("continue_watching").length) hubs+=rail("Continue watching", hub("continue_watching").map(i=>card(i,true)).join(""));
  if(hub("next_up").length)           hubs+=rail("Next up",           hub("next_up").map(i=>card(i,true)).join(""));
  if(hub("recently_added").length)    hubs+=rail("Recently added",    hub("recently_added").map(i=>card(i)).join(""));
  // Coming soon: absent entirely when nothing is paired or Curator had nothing
  // to say — an empty rail is worse than no rail.
  if(p.soon&&p.soon.entries&&p.soon.entries.length) soon+=rail("Coming soon", p.soon.entries.map(comingCard).join(""));

  if(p.hubsError) hubs+=`<div class="hint">Home recommendations are unavailable right now.</div>`;
  if(p.previewsPending){
    previews+=`<div class="empty">Loading library previews…</div>`;
  }else if(p.previewsError){
    previews+=`<div class="empty">Library previews are unavailable right now.</div>`;
  }else if(p.libs.length){
    previews+=groupToggleHtml(p.group);
    for(const s of p.sections){
      const count=s.total==null?"preview unavailable":`view all (${esc(String(s.total))})`;
      const head=`${esc(s.name)} <a class="muted" style="font-size:12px" href="${esc(s.href)}">· ${count}</a>`;
      // Two cases still take a grid rather than a rail, both inherited from
      // the catalog drop's reasoning. An EMPTY section, because rail() on an
      // empty row renders a heading over nothing while grid([]) renders the
      // "Nothing here yet." card the rest of the app uses; and any section
      // holding PHOTOS, because grid() is what populates PHOTO_SET and
      // without it the lightbox opens on one picture and cannot page to the
      // next.
      const photos=s.items.some(i=>i.kind==="photo");
      previews += (!s.items.length||photos)
        ? `<h2 class="section">${head}</h2>${grid(s.items)}`
        : rail(head, s.items.map(i=>card(i)).join(""));
      if(s.unavailable) previews+=`<div class="hint">Some library previews are unavailable right now.</div>`;
    }
  } else {
    previews+=`<div class="empty">No libraries yet.${ME&&ME.is_admin?' Add one in <a href="#/settings/libraries">Settings</a>.':""}</div>`;
  }
  return `<div data-home-region="hubs" data-home-slot="hero">${hero}</div>`+
    `<div class="th-rails"><div data-home-region="hubs" data-home-slot="hubs">${hubs}</div>`+
    `<div data-home-region="soon" data-home-slot="soon">${soon}</div>`+
    `<div data-home-region="previews" data-home-slot="previews">${previews}</div></div>`;
}
// ---------------------------------------------------------------------------
// 4. Library — shell only (items and count are inherited from classic)
// ---------------------------------------------------------------------------
// The shell is rendered ONCE per entry into #main and owns the frame: page
// head, the floating filter-chip bar, and the three region mounts (#libbody,
// #libpager, #librail). It must never be re-rendered per batch, and #libbody's
// CONTENTS — not #libbody itself — are what the loader replaces as pages land.
// That is the whole reason this route is region-shaped: a 200-item library
// redraws its grid five or six times while loading, and a body(model) contract
// would take the reader's scroll position with it every time.
//
// The scroller stays the WINDOW, as in classic and catalog. Making #libbody an
// overflow:auto box would satisfy a literal reading of "#libbody is the thing
// that scrolls" and would silently kill the shipped scroll-spy (one window
// listener; scroll events do not bubble).
//
// The chips ARE classic's three <select>s, re-emitted verbatim including their
// onchange strings — behaviour is not a layout's to retype, and a layout that
// retypes it is a layout that can get it subtly wrong. The pill shape is CSS.
// The `libbar` class stays on the bar because libGoPage() scrolls
// `#main .libbar` into view after a page change; dropping it would leave
// paging silently unanchored, which is a defect nobody would see in a
// screenshot.
function theaterLibraryShell(m){
  const sort=m.sort;
  const head=pageHead([{href:"#/",label:"Home"},{label:m.title}],m.title);
  const bar=`<div class="libbar th-chips">
      <span class="th-cpair"><span class="lbl">Sort</span>
        <select aria-label="Sort" onchange="LIB_SORT=this.value;${m.reload}">
          ${libOpt("title","Title (A–Z)",sort)}${libOpt("added","Recently added",sort)}${libOpt("recorded","Date recorded",sort)}${libOpt("year","Year",sort)}${libOpt("resolution","Resolution",sort)}</select></span>
      <span class="th-cpair"><span class="lbl">Show</span>
        <select data-library-watch-filter aria-label="Show" onchange="LIB_FILTER=this.value;${m.reload}">
          ${libOpt("all","Everything",LIB_FILTER)}${libOpt("unwatched","Unwatched",LIB_FILTER)}${libOpt("inprogress","In progress",LIB_FILTER)}${libOpt("watched","Watched",LIB_FILTER)}</select></span>
      <span class="th-cpair"><span class="lbl">Per page</span>
        <select aria-label="Per page" onchange="setPerPage(this.value);libGoPage(0)">
          ${LIB_SIZES.map(n=>libOpt(String(n),String(n),String(LIB_PER))).join("")}${libOpt("all","All",String(LIB_PER))}</select></span>
      <span class="muted" id="libcount"></span></div>`;
  return `${head}${bar}<div class="th-grid"><div id="libbody"><div class="empty">Loading…</div></div></div>`
    +`<div id="libpager"></div><div id="librail"></div>`;
}

// ---------------------------------------------------------------------------
// 5. Item detail — full-screen backdrop, facts in the lower third
// ---------------------------------------------------------------------------
// Every branch classic has is here — photo, playable file, missing file,
// multiple versions, season episode list, children grid, empty folder, rollup
// chips, tags, watch controls, the edit and refresh-artwork admin buttons,
// breadcrumbs — and every one of them is selected by `p.shape` rather than by
// re-running classic's cascade of length checks.
//
// The structural difference from classic: classic puts EVERYTHING inside the
// hero's text column, so a 26-episode list renders in a 1fr column beside a
// poster. Theater splits at the natural seam — the backdrop screen carries
// identity and the play lockup, and the detail below it (version cards,
// episode list, children grid) gets the full width. Nothing moves between the
// two that changes what is on the page; the file-missing warning stays WITH
// the play lockup because it is the answer to "can I play this", not a
// footnote.
function theaterItemBody(p){
  const it=p.item;

  // Admin affordances. .th-admin is the hook the television surface hides them
  // with (styles.css) — you do not reanalyze a file with a remote.
  const artBtn = ME&&ME.is_admin
    ? ` <button class="ghost sm th-admin" title="Re-fetch this item's poster and backdrop" aria-label="Refresh artwork" onclick="refreshArtwork(${it.id},this)">⟳ Refresh artwork</button>`
    : '';
  // `p.editable` — the model's answer, not a re-test of admin + library kind.
  const editBtn = p.editable
    ? ` <button class="ghost sm th-admin" title="Edit details" aria-label="Edit details" onclick='openEdit(${esc(JSON.stringify(it))})'>✎ Edit</button>`
    : '';

  const trail=[{href:"#/",label:"Home"}]
    .concat(NAV_ORIGIN?[{href:NAV_ORIGIN.href,label:NAV_ORIGIN.label}]:[])
    .concat((p.ancestors||[]).map(a=>({href:`#/item/${exactWireId(a)}`,label:a.title})))
    .concat([{label:it.title}]);
  const crumbs=pageHead(trail);

  // The kicker: kind above the title, the way a cinema detail page labels
  // itself, with the year range beside it. `p.years` is the MODEL's range —
  // gathered from the seasons' air dates plus the item's own — and is not
  // recomputed here. That single field is 12 lines of duplicated cascade in
  // classicItemBody and catalogItemBody, and it is the headline example in the
  // G3 record of the abstraction failing one layer down.
  const yrs = p.years ? `${p.years.from}${p.years.to>p.years.from?'–'+p.years.to:''}` : '';
  const kick=[it.kind?esc(it.kind):'', yrs].filter(Boolean).join(' · ');

  const chips=[];
  if(it.recorded_at) chips.push(`<span>${esc(fmtDate(it.recorded_at))}</span>`);
  if(p.runtime) chips.push(`<span>${esc(fmtDur(p.runtime))}</span>`);
  // A container has no watch flag of its own, so say what it is made of:
  // "3 of 10 watched" is the thing the mark-watched buttons act on. This
  // sentence is a DECISION and it is now written out for the third time in
  // this file's history — NOTES §5.1 asks for `p.rollupLabel`.
  if(it.rollup&&it.rollup.leaves){
    const r=it.rollup;
    chips.push(`<span>${r.watched===r.leaves?`all ${r.leaves} watched`:`${r.watched} of ${r.leaves} watched`}</span>`);
  }
  // Video badges show once: on the backdrop screen for a single version,
  // per-version below when there are several, so nothing is duplicated.
  // `p.best` and `p.multi` are the model's; the RULE that combines them is
  // not — NOTES §5.2.
  const heroBadges = (p.best && !p.multi) ? specBadges(p.best) : "";

  // `acts` lands on the backdrop screen (the play lockup and anything that
  // answers "can I play this"); `rest` lands below it at full width.
  let acts="", rest="";
  if(p.shape==="photo"){
    // A photo is a picture, not a playback session: show it, and let a click
    // open the same full-screen view the grid uses.
    PHOTO_SET=[{id:it.id,title:it.title,recorded_at:it.recorded_at}];
    acts=`<div class="actions"><button class="btnplay" onclick="openLightbox(${it.id})">View full size</button></div>`;
    rest=`<img class="art th-photo" style="aspect-ratio:auto;max-height:64vh;width:auto;border-radius:var(--radius);cursor:zoom-in"
           src="${esc(photoUrl(it.id,'thumb'))}" alt="" onclick="openLightbox(${it.id})">`;
  } else if(p.shape==="book"){
    acts=bookActions(p);
    rest=bookVersions(p);
  } else if(p.shape==="versions"){
    // Movie / episode / clip: play the best AVAILABLE version, plus labelled
    // specs. `p.playable` and `p.resume` and `p.meta` are all the model's —
    // the file pick, the 3-second threshold and the player's info payload are
    // decided once, in loadItem(), for every layout.
    const pmeta=p.meta, resume=p.resume, playable=p.playable;
    const watchBtn=watchControls(it);
    if(playable){
      acts=`<div class="actions">
        <button class="btnplay" onclick='play(${playable.id},${esc(JSON.stringify(it.title))},${resume},${playable.duration_ms||0},${esc(JSON.stringify(pmeta))})'>▶ ${resume?`Resume · ${fmtDur(resume)}`:'Play'}</button>
        ${resume?`<button class="ghost" onclick='play(${playable.id},${esc(JSON.stringify(it.title))},0,${playable.duration_ms||0},${esc(JSON.stringify(pmeta))})'>Start over</button>`:''}
        ${watchBtn}</div>`;
      const pdur=playable.duration_ms||p.runtime||0, prem=resume?Math.max(0,pdur-resume):pdur, pend=endsAt(prem);
      if(prem) acts+=`<div class="playinfo">${resume?fmtDur(prem)+" left":fmtDur(prem)}${pend?" · ends ~"+pend:""}</div>`;
    } else {
      // `mp`, not `p`: classic can shadow its page argument here because it
      // never uses it again. This function's `p` is still live.
      const mp=p.best?p.best.missing_path:null;
      acts=`<div class="filemissing">⚠ This file is missing on the server, so it can’t be played — its library path may be unmounted, moved, or renamed${ME.is_admin&&mp?`:<br><code>${esc(mp)}</code>`:'.'}</div><div class="actions">${watchBtn}</div>`;
    }
    rest=p.files.map(v=>{
      const miss=!v.available;
      // Header (badges + per-version play) only when there are several
      // versions to tell apart, or when a file is missing. A single version is
      // already badged on the backdrop screen, so its block is just the spec
      // detail. Third copy of that rule — NOTES §5.2.
      const header=(p.multi||miss)
        ? `<div class="vh"><span class="vbadges">${specBadges(v)||`<span class="vt">${esc(v.filename)}</span>`}${miss?'<span class="missbadge">missing</span>':''}</span>${(!miss&&p.multi)?`<button class="ghost sm" onclick='play(${v.id},${esc(JSON.stringify(it.title))},${resume},${v.duration_ms||0},${esc(JSON.stringify(pmeta))})'>▶ Play</button>`:''}</div>`
        : '';
      return `<div class="version">${header}${specBlock(v)}${miss&&ME.is_admin&&v.missing_path?`<div class="problem">${esc(v.missing_path)}</div>`:''}${unprobedNote(v,it.id)}${dvFileActionMount(v)}${fileAdminActions(v,it.id)}</div>`;
    }).join("");
  } else if(p.shape==="episodes"){
    const a=watchControls(it);
    if(a) acts=`<div class="actions">${a}</div>`;
    rest=`<h2 class="section">${p.children.length} episode${p.children.length>1?'s':''}</h2>
      <div class="eplist">${p.children.map(episodeRow).join("")}</div>`;
  } else if(p.shape==="children"){
    const a=watchControls(it);
    if(a) acts=`<div class="actions">${a}</div>`;
    // `p.childLabel` straight, with no folder re-test on top of it: the model
    // already answers 'Contents' for a folder, 'Seasons' for a show and
    // 'Episodes' otherwise. classic and catalog both wrap it in a ternary that
    // recomputes the folder case the model had already handled.
    rest=`<h2 class="section">${esc(p.childLabel)}</h2>${grid(p.children)}`;
  } else if(p.shape==="folder"){
    rest=`<div class="empty">This folder is empty.</div>`;
  }
  // p.shape==="empty" falls through with nothing below the fold, as classic
  // does: an item with no files, no children and no folder kind has nothing
  // truthful to put there.

  rest+=bookEditionSection(p);
  const bg=it.backdrop
    ? `<div class="th-bg" style="background-image:url(${esc(tok(it.backdrop))})" aria-hidden="true"></div>` : "";
  return `<div class="th-hero th-detail${bg?'':' th-noart'}">${bg}
    <div class="th-scrim" aria-hidden="true"></div>
    <div class="th-dwrap">
      <div class="th-dposter">${artHtml(it)}</div>
      <div class="th-fg">${crumbs}
        ${kick?`<div class="th-kick">${kick}</div>`:''}
        <h1>${esc(it.title)}${editBtn}${artBtn}</h1>${bookByline(it)}
        ${chips.length?`<div class="chips">${chips.join("")}</div>`:''}
        ${(it.tags&&it.tags.length)?`<div class="chips th-tags">${it.tags.map(t=>`<span>${esc(t)}</span>`).join("")}</div>`:''}
        ${heroBadges?`<div class="vbadges">${heroBadges}</div>`:''}
        ${it.overview?`<p class="overview">${esc(it.overview)}</p>`:''}
        ${acts}</div>
    </div></div>${rest?`<div class="th-rest">${rest}</div>`:''}`;
}

// ---------------------------------------------------------------------------
// 6. stickyFloor — where the layout's own chrome stops and content begins
// ---------------------------------------------------------------------------
// The A–Z scroll-spy asks "which letter is at the top of the reading area?"
// and needs a viewport y to measure against. classic answers with
// header.top's bottom; theater has no header.top at all, so the shipped
// fallback (56px) is a constant that happens to be close on a desktop and
// wrong everywhere else.
//
// Theater floors content with TWO stacked bands on the library route: the
// fixed top bar, and the sticky filter-chip bar that comes to rest against it.
// Both are measured rather than read off the --th-top/--th-gut constants,
// because the chip bar wraps to two rows on a narrow window and a television
// runs a taller bar — and a measurement is the same answer whether the window
// was resized or the page re-rendered, which a media-query branch in JS is
// not.
function theaterStickyFloor(){
  let floor=0;
  const top=document.querySelector(".th-top");
  if(top){
    const r=top.getBoundingClientRect();
    // A bar that has scrolled away or is hidden floors nothing. The theater
    // bar is position:fixed so this is normally always true, but the phone
    // query and any future variant are covered by the measurement rather than
    // by an assumption.
    if(r.bottom>0 && r.top<=2) floor=Math.max(floor, r.bottom);
  }
  const chips=document.querySelector(".th-chips");
  if(chips){
    const r=chips.getBoundingClientRect();
    // Only when the chip bar is actually PINNED — while it is still in normal
    // flow further down the page it is content, not chrome, and counting it
    // would push the spy line most of a screen down.
    if(r.bottom>0 && r.top<=floor+2) floor=Math.max(floor, r.bottom);
  }
  return floor;
}

// ---------------------------------------------------------------------------
// 7. One-time wiring: keyboard- and D-pad-operable cards
// ---------------------------------------------------------------------------
// A poster is a <div onclick=…>: clickable, but not focusable and not operable
// from the keyboard. Theater is the TV flagship (plan §2.5) — without this
// there is no D-pad path through the app at all, and the ten-foot focus
// treatment in styles.css has nothing to attach to.
//
// This used to be a near-copy of catalogWireOnce()/catalogEnhanceCards(), with
// a layoutId() guard on each so Classic kept emitting posters that nothing
// could focus — the golden churn was the reason, and being unreachable by
// keyboard was the price. NOTES §5.5 asked for shared wiring; it is
// navKeyboardWireOnce(), it covers episode rows too, and the golden was
// regenerated once for all three layouts.
let THEATER_WIRED=false;
function theaterWireOnce(){
  if(THEATER_WIRED) return;
  THEATER_WIRED=true;
  // Card reach is shared with every other layout — see navKeyboardWireOnce().
  navKeyboardWireOnce();
}

// ---- registration ---------------------------------------------------------
// `surfaces` includes tv: theater is the flagship television layout (§2.5).
// `library` is region-shaped and deliberately partial — `items` and `count`
// fall through to classic PER REGION, which is what the per-region fallback is
// for and the difference between overriding a detail and reimplementing a
// route.
LAYOUTS.theater={name:"Theater", surfaces:["desktop","mobile","tv"],
  chrome: theaterChrome,
  views: {home: theaterHomeBody, item: theaterItemBody,
          library: {shell: theaterLibraryShell}},
  stickyFloor: theaterStickyFloor};
// applyLayout() ran in the <head>, where the registry may not yet know this
// id. Re-applying here is invisible — boot() has not resolved /server yet, so
// #app is empty and there is nothing painted to reflow — and it is a belt to
// the <head> stub's braces, not a replacement for it: layoutMenuHtml() and
// ui-baseline's REGISTRY_JS both read LAYOUTS, but only the <head> copy runs
// before first paint.
applyLayout();

