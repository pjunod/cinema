"use strict";
// ---- library grids --------------------------------------------------------
// One page of items per request, and every page fetched.
//
// The server caps a page at 200 and should — a library is unbounded, and "send
// me everything at once" is a request that gets slower every time somebody adds
// a film. What was wrong was the client asking for one page, showing it, and
// saying "(first 200 of 321)" as though that were a state of affairs rather
// than a bug: 121 films with no way to reach them, no pagination, no scroll.
//
// The filter made it worse and quieter. Show → Unwatched filters what has been
// LOADED, so on a 321-item library it silently searched the first 200 — a film
// you own and have not watched simply not appearing in a list of films you have
// not watched, with nothing on screen to suggest the list was partial.
const LIB_PAGE=200;
// Which listing is current. Changing sort or filter, or leaving the page,
// abandons whatever pages are still in flight for the previous one — without
// this a slow second page from the old view lands in the new one.
let LIB_LOAD=0;

// Fetch every page of every listed library, handing each batch over as it
// arrives so the first screenful paints while the rest is still coming.
// Returns false if this load was superseded or failed.
async function eachItemPage(libIds, sort, gen, onBatch){
  // Summed across libraries as each one's first page reveals its size, so a
  // category's "of N" is right from its second batch rather than only at the
  // end.
  let total=0;
  for(const id of libIds){
    for(let offset=0;; offset+=LIB_PAGE){
      let page;
      try{
        page=await api(`/libraries/${id}/items?limit=${LIB_PAGE}&offset=${offset}&sort=${sort}`);
      }catch(e){
        if(LIB_LOAD===gen) toast(e.message);
        return false;
      }
      // Superseded, or the view is simply no longer on screen. Checking the
      // DOM as well as the token is what stops a big library fetching pages
      // into nothing after the user has navigated away.
      if(LIB_LOAD!==gen || !document.getElementById("libbody")) return false;
      const got=page.items||[];
      const libTotal=page.total||0;
      if(offset===0) total+=libTotal;
      onBatch(got, total);
      if(!got.length || offset+LIB_PAGE>=libTotal) break;
    }
  }
  return true;
}

// The shared body of both library views: a sort/filter bar, a grid that grows
// as pages land, and an A–Z rail. One function because the two differ only in
// which libraries they read and what their controls re-invoke — and a grid that
// pages correctly in one of them and not the other is exactly the kind of
// difference nobody notices until a library gets big.
// Classic's library shell: the sort/filter/per-page bar, the page head, and
// the three region mounts. Rendered ONCE per entry — see the note on the
// piecewise contract above libraryView().
function classicLibraryShell(m){
  const sort=m.sort;
  const bar=`<div class="libbar">
    <div class="row">
      <span class="lbl">Sort</span>
      <select onchange="LIB_SORT=this.value;${m.reload}">
        ${libOpt("title","Title (A–Z)",sort)}${libOpt("added","Recently added",sort)}${libOpt("recorded","Date recorded",sort)}${libOpt("year","Year",sort)}${libOpt("resolution","Resolution",sort)}</select>
      <span class="lbl" style="margin-left:8px">Show</span>
      <select data-library-watch-filter aria-label="Watch status" onchange="LIB_FILTER=this.value;${m.reload}">
        ${libOpt("all","Everything",LIB_FILTER)}${libOpt("unwatched","Unwatched",LIB_FILTER)}${libOpt("inprogress","In progress",LIB_FILTER)}${libOpt("watched","Watched",LIB_FILTER)}</select>
      <span class="lbl" style="margin-left:8px">Per page</span>
      <select onchange="setPerPage(this.value);libGoPage(0)">
        ${LIB_SIZES.map(n=>libOpt(String(n),String(n),String(LIB_PER))).join("")}${libOpt("all","All",String(LIB_PER))}</select>
    </div>
    <span class="muted" id="libcount" style="font-size:13px"></span></div>`;
  const head=pageHead([{href:"#/",label:"Home"},{label:m.title}],m.title);
  return `${head}${bar}<div id="libbody"><div class="empty">Loading…</div></div>`+
    `<div id="libpager"></div><div id="librail"></div>`;
}
// The items region. Re-rendered per arriving batch into #libbody, which is why
// it must never include the scroll container itself.
function classicLibraryItems(m){
  return m.items.length
    ? (m.sort==="resolution"? resSections(m.items) : grid(m.items))
    : (m.done? `<div class="empty">Nothing matches this filter.</div>`
             : `<div class="empty">Loading…</div>`);
}
// The count region. While loading it says so rather than showing a number about
// to change — a count that ticks upward reads as items appearing from nowhere.
function classicLibraryCount(m){
  return !m.done
    ? `<span class="muted">loading… ${m.loaded}${m.total?` of ${m.total}`:""}</span>`
    : m.pages>1
      ? `${m.from+1}–${m.from+m.items.length} of ${m.shown}${m.shown!==m.total?` (${m.total} in all)`:""}`
      : m.shown!==m.total
        ? `${m.shown} of ${m.total} items`
        : `${m.shown} item${m.shown===1?'':'s'}`;
}
// The shared body of both library views: a sort/filter bar, a grid that grows
// as pages land, and an A–Z rail.
//
// This is the app's one INCREMENTAL route, and that is why it uses the
// region-shaped seam (§3.2) rather than home's single body builder. Pages
// arrive in a loop and only the regions that changed are rewritten; a
// `body(model) → html` contract would replace the whole body on every batch
// and throw away the reader's scroll position each time a page landed, which
// is precisely the behaviour the piecewise draw exists to avoid.
async function libraryView(o){
  layoutChrome("home",`<div class="empty">Loading…</div>`);
  const gen=++LIB_LOAD;
  // Every entry here is a fresh listing — a different library, a new sort, a
  // new filter — and page 3 of a list you have just reordered is not a place.
  // Paging itself does not come through here; `libGoPage` redraws in place.
  LIB_PAGE_AT=0;LIB_SCOPE="";LIB_FIND="";
  const sort=o.sort;
  setOrigin(o.href,o.title);
  document.getElementById("main").innerHTML=
    layoutRegion("library","shell",{kind:"library",title:o.title,href:o.href,sort,reload:o.reload});
  const viewLibs=(await libsCached()).filter(l=>o.libIds.some(id=>String(id)===String(l.id)));
  if(LIB_LOAD!==gen)return;
  document.getElementById("libbody").insertAdjacentHTML("beforebegin",libraryBrowseTools(viewLibs));

  let all=[], total=0;
  const draw=(done)=>{
    const body=document.getElementById("libbody"); if(!body) return;
    let items=all;
    // Sorting the merged set matters only for a category, whose shares are
    // each sorted on their own; a single library arrives in order.
    if(o.resort) items=o.resort(items.slice(), sort);
    // Filter before paging, always. The other order gives "50 films, of which
    // the unwatched ones" — which is why an unwatched film could be missing
    // from a list of unwatched films.
    if(LIB_FILTER!=="all") items=items.filter(it=>matchWatch(it,LIB_FILTER));
    if(LIB_SCOPE)items=items.filter(it=>String(it.library_id)===LIB_SCOPE);
    if(LIB_FIND.trim())items=items.filter(it=>(it.title||"").toLocaleLowerCase().includes(LIB_FIND.trim().toLocaleLowerCase()));
    const shown=items.length;
    LIB_VIEW={draw, done, shown};
    const pages=libPageCount(shown);
    if(LIB_PAGE_AT>=pages) LIB_PAGE_AT=pages-1;   // the filter shrank the list under us
    const from=LIB_PER==="all"?0:LIB_PAGE_AT*LIB_PER;
    const page=LIB_PER==="all"?items:items.slice(from, from+LIB_PER);

    // One model, three regions. `loaded` is the raw arrival count and `shown`
    // the count after filtering — the loading line reports the former because
    // it is describing progress, not results.
    const m={kind:"library", title:o.title, href:o.href, sort, filter:LIB_FILTER,
      per:LIB_PER, pageAt:LIB_PAGE_AT, pages, from, items:page,
      loaded:all.length, shown, total, done, reload:o.reload};
    body.innerHTML=layoutRegion("library","items",m);
    const pager=document.getElementById("libpager");
    if(pager) pager.innerHTML = done? pagerHtml(shown) : "";
    // The A–Z rail maps to what is on screen, so it describes this page. Its
    // mount is placed by the shell, so a layout that wants it on the right edge
    // moves the mount rather than reimplementing the rail.
    const rail=document.getElementById("librail");
    if(rail) rail.innerHTML=alphaRailHtml(page, sort);
    const count=document.getElementById("libcount");
    if(count) count.innerHTML=layoutRegion("library","count",m);
    tagAlpha(page, sort);
  };

  const complete=await eachItemPage(o.libIds, sort, gen, (batch, t)=>{
    all=all.concat(batch);
    total=t;
    draw(false);
  });
  if(LIB_LOAD!==gen) return;
  draw(complete);
  restoreScroll();
}

async function viewLibrary(id){
  const lib=(await libsCached()).find(l=>l.id==id);
  await libraryView({
    libIds:[id],
    sort:sortFor(lib),
    title:lib?lib.name:'Library',
    href:`#/library/${id}`,
    reload:`viewLibrary(${Number(id)})`,
  });
}
// "View all" for a category that spans more than one share: merge their items,
// then apply the same client-side sort/filter as a single library.
async function viewCategory(key){
  const libs=(await libsCached()).filter(l=>libCategory(l).key===key);
  if(!libs.length){
    layoutChrome("home",`<div class="empty">No libraries in this category.</div>`);
    return;
  }
  const cat=libCategory(libs[0]);
  await libraryView({
    libIds:libs.map(l=>l.id),
    sort:sortFor(libs[0]),
    title:cat.name,
    href:`#/category/${encodeURIComponent(key)}`,
    reload:`viewCategory(${esc(JSON.stringify(key))})`,
    resort:(items,sort)=>{
      // Each share is server-sorted; the merged set needs re-sorting for the
      // fields the client holds.
      if(sort==="title") items.sort((a,b)=>sortKey(a.title).localeCompare(sortKey(b.title)));
      else if(sort==="year") items.sort((a,b)=>(b.year||0)-(a.year||0));
      else if(sort==="resolution") items.sort((a,b)=>(b.resolution||0)-(a.resolution||0));
      else if(sort==="recorded") items.sort((a,b)=>(b.recorded_at||"").localeCompare(a.recorded_at||""));
      return items;
    },
  });
}
async function viewSearch(q){
  const generation=PAGE_RENDER_GENERATION,route=location.hash;
  layoutChrome("home",`<div class="empty">Searching…</div>`);
  const r=await api(`/search?q=${encodeURIComponent(q)}`);
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route)return;
  setOrigin(`#/search/${encodeURIComponent(q)}`,`Search “${q}”`);
  SEARCH_DATA={query:q,results:r.results||[],related:[],route};
  const head=pageHead([{href:"#/",label:"Home"},{label:`Search “${q}”`}],`Results for “${q}”`);
  const kinds=[...new Set(SEARCH_DATA.results.map(it=>it.kind).filter(Boolean))];
  document.getElementById("main").innerHTML=`${head}<div class="search-tools"><span id="search-result-count" class="muted" aria-live="polite"></span><label>Type <select id="search-kind" onchange="paintSearchResults()"><option value="all">All types</option>${kinds.map(k=>`<option value="${esc(k)}">${esc(k==='show'?'Series':k)}</option>`).join('')}</select></label></div><div id="search-results"></div><div id="search-related"></div>`;
  paintSearchResults();
  loadSemanticResults(q,generation,route);
}

async function loadSemanticResults(query,generation,route){
  try{
    const r=await api(`/search/related?q=${encodeURIComponent(query)}`);
    if(generation!==PAGE_RENDER_GENERATION||location.hash!==route||!SEARCH_DATA)return;
    SEARCH_DATA.related=r.results||[];paintSemanticResults();
  }catch(e){/* Exact search is already visible and remains usable. */}
}
function paintSemanticResults(){
  const host=document.getElementById('search-related');if(!host||!SEARCH_DATA)return;
  const scope=document.getElementById('search-kind')?.value||'all';
  const exact=new Set(SEARCH_DATA.results.map(exactWireId));
  const list=(SEARCH_DATA.related||[]).filter(it=>!exact.has(exactWireId(it))&&(scope==='all'||it.kind===scope));
  host.innerHTML=list.length?`<section class="search-group"><h2>Related by meaning</h2><p class="hint">Suggestions from the embedded model. These do not change channel selections.</p>${grid(list)}</section>`:'';
}
function searchDialog(title){
  document.getElementById('search-dialog')?.remove();
  const dialog=document.createElement('dialog');dialog.id='search-dialog';dialog.className='card';dialog.style.cssText='width:min(640px,calc(100vw - 32px));max-height:85vh;overflow:auto';
  dialog.innerHTML=`<div class="row" style="justify-content:space-between"><h2 id="search-dialog-title">${esc(title)}</h2><button class="ghost" onclick="document.getElementById('search-dialog').close()">Close</button></div><div id="search-dialog-body">Loading…</div>`;
  dialog.setAttribute('aria-labelledby','search-dialog-title');const close=()=>dialog.close();window.addEventListener('hashchange',close);dialog.addEventListener('close',()=>{window.removeEventListener('hashchange',close);dialog.remove();});document.body.appendChild(dialog);dialog.showModal();return dialog;
}
async function showSearchSettings(){
  const dialog=searchDialog('Search and classification');
  try{const r=await api('/search/settings');if(!dialog.isConnected)return;
    dialog.querySelector('#search-dialog-body').innerHTML=`<p>Text search and channel rules use the local catalogue. A background service generates format and topic labels from existing metadata; TMDB keywords enrich them when a TMDB key is configured.</p><p>${Number(r.classification.processed)||0} titles checked in this pass; ${Number(r.classification.labelled)||0} labelled.${r.classification.scan_complete?' Pass complete.':''}</p>${r.classification.provider_error?`<p class="hint">${esc(r.classification.provider_error)}</p>`:''}<p>Optional semantic search downloads a roughly 91 MB model once per node, then runs inside plurx on CPU. It uses additional memory and CPU while indexing. Search queries and catalogue text stay on your server.</p><p>Model: ${esc(r.semantic.state||'disabled')}; ${Number(r.semantic.indexed)||0} titles indexed.</p><label class="row"><input id="semantic-enabled" type="checkbox" style="width:18px;height:18px;flex:none" ${r.semantic_enabled?'checked':''}>Enable embedded semantic search</label><p class="hint">Disabling unloads the model; cached files stay on disk. Local search remains available while the model is loading or unavailable.</p><button onclick="saveSearchSettings()">Save</button><p id="search-settings-error" role="status"></p>`;
  }catch(e){if(dialog.isConnected)dialog.querySelector('#search-dialog-body').textContent=e.message;}
}
async function saveSearchSettings(){
  const dialog=document.getElementById('search-dialog');if(!dialog)return;
  try{await api('/search/settings',{method:'PUT',body:{semantic_enabled:!!dialog.querySelector('#semantic-enabled')?.checked}});dialog.close();toast('Search settings saved');}
  catch(e){if(dialog.isConnected)dialog.querySelector('#search-settings-error').textContent=e.message;}
}
async function showClassification(id){
  const dialog=searchDialog('Media classification');
  try{const r=await api(`/items/${encodeURIComponent(id)}/classification`);if(!dialog.isConnected)return;
    const record=r.classification,labels=record?.classification?.labels||[],edits=record?.overrides||{include:[],exclude:[]};
    dialog.dataset.itemId=id;dialog.dataset.revision=String(record?.revision||0);
    dialog.querySelector('#search-dialog-body').innerHTML=`${r.pending?'<p>Metadata changed or has not been classified yet. Background classification is pending.</p>':''}<ul>${labels.map(l=>`<li><b>${esc(l.dimension+':'+l.value)}</b> — ${esc(l.source)}: ${esc(l.evidence)}${edits.exclude.includes(l.dimension+':'+l.value)?' (excluded by correction)':''}</li>`).join('')||'<li>No automatic labels yet.</li>'}</ul>${ME?.is_admin?`<p>Corrections survive future metadata refreshes. Use comma-separated labels such as format:stand-up or topic:space.</p><label for="classification-include">Add labels</label><input id="classification-include" value="${esc(edits.include.join(', '))}"><label for="classification-exclude">Suppress labels</label><input id="classification-exclude" value="${esc(edits.exclude.join(', '))}"><button onclick="saveClassification()">Save corrections</button><p id="classification-error" role="status"></p>`:''}`;
  }catch(e){if(dialog.isConnected)dialog.querySelector('#search-dialog-body').textContent=e.message;}
}
async function saveClassification(){
  const d=document.getElementById('search-dialog');if(!d)return;
  const read=id=>d.querySelector(id).value.split(',').map(s=>s.trim()).filter(Boolean);
  try{await api(`/items/${encodeURIComponent(d.dataset.itemId)}/classification`,{method:'PUT',body:{expected_revision:Number(d.dataset.revision),include:read('#classification-include'),exclude:read('#classification-exclude')}});d.close();toast('Classification corrections saved');}
  catch(e){if(d.isConnected)d.querySelector('#classification-error').textContent=e.message;}
}

