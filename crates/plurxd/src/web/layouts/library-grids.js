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

// Rows are a presentation of the complete filtered listing, never a page slice.
// Grid pagination remains independent, including when View all opens one group.
let LIB_PRESENTATION=(function(){
  try{return localStorage.getItem("plurx_library_view")==="grid"?"grid":"rows";}catch(e){return "rows";}
})();
let LIB_GROUP="";
const LIB_ROW_CHUNK=40;
const LIB_ROW_NODES=new WeakMap();
function libraryViewToggle(){
  return `<span class="library-view-toggle" role="group" aria-label="Library view">${["rows","grid"].map(v=>
    `<button class="ghost sm" data-library-view="${v}" aria-pressed="${LIB_PRESENTATION===v}" onclick="setLibraryPresentation('${v}')">${v==="rows"?"Rows":"Grid"}</button>`).join("")}</span>`;
}
function libraryPageSize(){
  return `<label class="library-page-size"${LIB_PRESENTATION==="rows"?' hidden':''}>Per page <select aria-label="Per page" onchange="setPerPage(this.value);libGoPage(0)">${LIB_SIZES.map(n=>libOpt(String(n),String(n),String(LIB_PER))).join("")}${libOpt("all","All",String(LIB_PER))}</select></label>`;
}
function setLibraryPresentation(value){
  if(value!=="rows"&&value!=="grid")return;
  LIB_PRESENTATION=value;LIB_GROUP="";LIB_PAGE_AT=0;
  try{localStorage.setItem("plurx_library_view",value);}catch(e){}
  if(LIB_VIEW)LIB_VIEW.draw(LIB_VIEW.done);
}
function libraryShowGroup(key){
  const previous=LIB_GROUP;
  LIB_GROUP=key;LIB_PAGE_AT=0;
  if(LIB_VIEW)LIB_VIEW.draw(LIB_VIEW.done);
  if(!key&&document.getElementById(`library-row-${previous}`)){libraryJump(previous);return;}
  const target=libraryElement(document,key?"#library-group-back":"#library-group-index button");
  if(target){
    window.scrollTo({top:window.scrollY+target.getBoundingClientRect().top-Math.max(0,stickyFloor())-12,behavior:libraryScrollBehavior()});
    target.focus({preventScroll:true});
  }
}

/** @returns {HTMLElement|null} */
function libraryElement(root,selector){return root.querySelector(selector);}
function libraryTitleKey(it){return it.sort_title??sortKey(it.title);}
// SQLite's BINARY text order is Unicode scalar order, not locale order. Walk
// code points so astral characters also agree with the server's UTF-8 keys.
function libraryTextCompare(a,b){
  const aa=Array.from(a),bb=Array.from(b);
  for(let i=0;i<Math.min(aa.length,bb.length);i++){
    const d=aa[i].codePointAt(0)-bb[i].codePointAt(0);if(d)return d;
  }
  return aa.length-bb.length;
}
function libraryItemCompare(a,b,sort){
  const desc=(x,y)=>x===y?0:x==null?1:y==null?-1:x>y?-1:1;
  const id=(x,y)=>{const aa=BigInt(exactWireId(x)),bb=BigInt(exactWireId(y));return aa===bb?0:aa<bb?-1:1;};
  if(sort==="added")return desc(a.added_at,b.added_at)||id(b,a);
  let by=0;
  if(sort==="year")by=desc(a.year,b.year);
  if(sort==="recorded")by=desc(a.recorded_at,b.recorded_at);
  if(sort==="resolution")by=desc(a.resolution??-1,b.resolution??-1);
  return by||libraryTextCompare(libraryTitleKey(a),libraryTitleKey(b))||id(a,b);
}
function libraryGroupFor(it,sort,now){
  if(sort==="title"){
    const c=libraryTitleKey(it)[0]||"";
    const label=/^[a-z]$/i.test(c)?c.toUpperCase():"#";
    return {key:label,label,order:label==="#"?0:label.charCodeAt(0)};
  }
  if(sort==="resolution"){
    const b=resBucket(it);return {key:`res-${b.r}`,label:b.l,order:b.r};
  }
  if(sort==="added"){
    const seconds=Number(it.added_at),date=new Date(seconds*1000);
    if(it.added_at==null||!Number.isFinite(seconds)||!Number.isFinite(date.getTime()))
      return {key:"unknown",label:"Unknown date added",order:Infinity};
    const today=new Date(now.getFullYear(),now.getMonth(),now.getDate());
    const week=new Date(today);week.setDate(week.getDate()-6);
    const month=new Date(now.getFullYear(),now.getMonth(),1);
    if(date>now)return {key:"future",label:"Future dates",order:-1};
    if(date>=today)return {key:"today",label:"Today",order:0};
    if(date>=week)return {key:"week",label:"Previous 6 days",order:1};
    if(date>=month)return {key:"month",label:"Earlier this month",order:2};
    const y=date.getFullYear(),m=date.getMonth();
    return {key:`added-${y}-${m+1}`,label:date.toLocaleDateString(undefined,{month:"long",year:"numeric"}),order:3+(now.getFullYear()-y)*12+now.getMonth()-m};
  }
  const year=sort==="recorded"?airYear(it.recorded_at):Number(it.year);
  return year>0&&Number.isInteger(year)
    ?{key:`year-${year}`,label:String(year),order:-year}
    :{key:"unknown",label:sort==="recorded"?"Unknown recording date":"Unknown year",order:Infinity};
}
function libraryGroups(items,sort,now){
  const groups=new Map();
  for(const it of items){
    const b=libraryGroupFor(it,sort,now);
    if(!groups.has(b.key))groups.set(b.key,{...b,items:[]});
    groups.get(b.key).items.push(it);
  }
  return [...groups.values()].sort((a,b)=>a.order-b.order);
}
function libraryRowScroll(button,direction){
  if(button.getAttribute("aria-disabled")==="true")return;
  const strip=libraryElement(button.closest(".library-group"),".rowscroll");
  if(strip)strip.scrollBy({left:direction*Math.max(160,strip.clientWidth*.85),behavior:libraryScrollBehavior()});
}
function libraryScrollBehavior(){return window.matchMedia("(prefers-reduced-motion: reduce)").matches?"instant":"smooth";}
function libraryRowSync(section){
  const strip=libraryElement(section,".rowscroll");if(!strip)return;
  const left=libraryElement(section,"[data-row-prev]"),right=libraryElement(section,"[data-row-next]");
  left.setAttribute("aria-disabled",String(strip.scrollLeft<=1));
  right.setAttribute("aria-disabled",String(strip.scrollLeft>=strip.scrollWidth-strip.clientWidth-1&&(!LIB_ROW_NODES.get(strip)||LIB_ROW_NODES.get(strip).limit>=LIB_ROW_NODES.get(strip).items.length)));
  const cards=strip.children;
  const step=cards.length?cards[0].getBoundingClientRect().width+(parseFloat(getComputedStyle(strip).columnGap)||0):0;
  const first=step?Math.min(cards.length,Math.floor(strip.scrollLeft/step)+1):0;
  const last=step?Math.min(cards.length,Math.ceil((strip.scrollLeft+strip.clientWidth)/step)):0;
  libraryElement(section,".library-row-position").textContent=first?`${first}–${last}`:"";
}
function libraryJump(key){
  const row=document.getElementById(`library-row-${key}`);if(!row)return;
  const index=document.getElementById("library-group-index");
  const top=Math.max(0,stickyFloor());
  const offset=(index?index.getBoundingClientRect().height:0)+top+12;
  window.scrollTo({top:window.scrollY+row.getBoundingClientRect().top-offset,behavior:libraryScrollBehavior()});
  libraryElement(row,"h2").focus({preventScroll:true});
}
// Keep groups and cards keyed by identity. Reordering a merged category only
// moves the affected nodes; arrivals do not replace decoded images, focus, or
// a row's scroll container. Detached rows are retained while View all is open.
function libraryPaintRows(body,groups,done,state){
  if(!body.classList.contains("library-rows")){body.replaceChildren();body.classList.add("library-rows");}
  const focused=document.activeElement;
  let before=body.firstElementChild;
  for(const group of groups){
    let row=state.rows.get(group.key);
    if(!row){
      const section=document.createElement("section");section.className="library-group";
      section.id=`library-row-${group.key}`;section.dataset.group=group.key;
      section.innerHTML=`<div class="library-group-head"><h2 id="${section.id}-title" tabindex="-1">${esc(group.label)}</h2><span class="library-group-count muted"></span><div class="library-group-actions"><span class="library-row-position muted"></span><button class="ghost sm" onclick="libraryShowGroup('${group.key}')" aria-label="View all ${esc(group.label)} items">View all</button><button class="ghost sm" data-row-prev aria-label="Scroll ${esc(group.label)} left" onclick="libraryRowScroll(this,-1)">‹</button><button class="ghost sm" data-row-next aria-label="Scroll ${esc(group.label)} right" onclick="libraryRowScroll(this,1)">›</button></div></div><div class="rowscroll" role="group" aria-labelledby="${section.id}-title"></div>`;
      const strip=libraryElement(section,".rowscroll");
      strip.addEventListener("scroll",()=>{
        if(!section.isConnected)return;
        row.position=strip.scrollLeft;
        if(row.limit<row.items.length&&strip.scrollLeft+strip.clientWidth>=strip.scrollWidth-strip.clientWidth){
          row.limit+=LIB_ROW_CHUNK;libraryPaintRowCards(row);
        }
        libraryRowSync(section);
      },{passive:true});
      strip.addEventListener("keydown",libraryRowKey);
      row={section,strip,cards:new Map(),position:0,limit:LIB_ROW_CHUNK,items:[]};
      LIB_ROW_NODES.set(strip,row);state.rows.set(group.key,row);
    }
    if(row.section!==before)body.insertBefore(row.section,before);
    before=row.section.nextElementSibling;
    libraryElement(row.section,".library-group-count").textContent=`${group.items.length}${done?"":" loaded"}`;
    row.items=group.items;
    libraryPaintRowCards(row);
    libraryRowSync(row.section);
  }
  const keys=new Set(groups.map(g=>g.key));
  for(const child of Array.from(body.children)){if(!keys.has(child.getAttribute("data-group")))child.remove();}
  if(!groups.length)body.innerHTML=`<div class="empty">${done?"Nothing matches this filter.":"Loading…"}</div>`;
  if(focused instanceof HTMLElement&&body.contains(focused)&&document.activeElement!==focused)focused.focus({preventScroll:true});
  rememberGridPhotos(groups.flatMap(g=>g.items));
}
// Bound the initial DOM for large groups; scrolling near the end appends the
// next chunk. End explicitly reaches the final card; View all uses pagination.
function libraryPaintRowCards(row){
  const focused=document.activeElement;
  if(focused&&row.strip.contains(focused)){
    for(const [id,node] of row.cards){if(node===focused)row.limit=Math.max(row.limit,row.items.findIndex(it=>exactWireId(it)===id)+1);}
  }
  const visible=row.items.slice(0,row.limit),ids=new Set(visible.map(exactWireId));
  for(const [id,node] of row.cards){if(!ids.has(id)){node.remove();row.cards.delete(id);}}
  let at=row.strip.firstElementChild;
  for(const it of visible){
    const id=exactWireId(it);let node=row.cards.get(id);
    if(!node){
      const holder=document.createElement("div");holder.innerHTML=card(it);
      node=holder.firstElementChild;row.cards.set(id,node);
    }
    if(node!==at)row.strip.insertBefore(node,at);
    at=node.nextElementSibling;
  }
  navEnhanceClickables(row.strip);
  if(focused instanceof HTMLElement&&row.strip.contains(focused)&&document.activeElement!==focused)focused.focus({preventScroll:true});
  row.strip.scrollLeft=row.position;
}
function libraryPaintControls(groups,rows,done){
  document.querySelectorAll("[data-library-view]").forEach(b=>b.setAttribute("aria-pressed",String(b.getAttribute("data-library-view")===LIB_PRESENTATION)));
  const per=libraryElement(document,".library-page-size");if(per)per.hidden=rows;
  const nav=document.getElementById("library-group-index");if(!nav)return;
  nav.hidden=!rows;
  const focused=document.activeElement,scroll=nav.scrollLeft;
  const buttons=new Map(Array.from(nav.children).map(b=>[b.getAttribute("data-jump"),b]));
  let before=nav.firstElementChild;
  for(const group of rows?groups:[]){
    let button=buttons.get(group.key);
    if(!button){
      button=document.createElement("button");button.className="ghost sm";
      button.setAttribute("data-jump",group.key);
      button.setAttribute("onclick",`libraryJump('${group.key}')`);
      button.textContent=group.label;
    }
    if(button!==before)nav.insertBefore(button,before);
    before=button.nextElementSibling;
    buttons.delete(group.key);
  }
  for(const button of buttons.values())button.remove();
  if(focused instanceof HTMLElement&&nav.contains(focused)&&document.activeElement!==focused)focused.focus({preventScroll:true});
  nav.scrollLeft=scroll;
  nav.setAttribute("aria-label",done?"Jump to group":"Jump to loaded group; more groups may appear");
  const detail=document.getElementById("library-group-detail");
  if(detail){
    const group=groups.find(g=>g.key===LIB_GROUP);
    const detailHtml=LIB_GROUP?`<button id="library-group-back" class="ghost sm" onclick="libraryShowGroup('')">‹ All rows</button><h2>${esc(group?group.label:"No matching items in this group")}</h2>`:"";
    if(detail.innerHTML!==detailHtml)detail.innerHTML=detailHtml;
  }
  libraryUpdateIndex();
}
function libraryUpdateIndex(){
  const nav=document.getElementById("library-group-index");if(!nav||nav.hidden)return;
  const top=Math.max(0,stickyFloor());nav.style.top=`${top}px`;
  const line=top+nav.getBoundingClientRect().height+14;
  const rows=document.querySelectorAll("#libbody .library-group");
  let current=null;
  for(const row of rows){if(row.getBoundingClientRect().bottom>line){current=row.getAttribute("data-group");break;}}
  nav.querySelectorAll("button").forEach(b=>{const on=b.dataset.jump===current;b.setAttribute("aria-current",String(on));});
}
// One window listener, no per-render observers or timers retaining old views.
let LIB_INDEX_FRAME=0;
window.addEventListener("scroll",()=>{
  if(!LIB_INDEX_FRAME)LIB_INDEX_FRAME=requestAnimationFrame(()=>{LIB_INDEX_FRAME=0;libraryUpdateIndex();});
},{passive:true});
window.addEventListener("resize",()=>{
  libraryUpdateIndex();document.querySelectorAll("#libbody .library-group").forEach(libraryRowSync);
},{passive:true});

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
      ${libraryViewToggle()}
      <span class="lbl">Sort</span>
      <select onchange="LIB_SORT=this.value;${m.reload}">
        ${libOpt("title","Title (A–Z)",sort)}${libOpt("added","Recently added",sort)}${libOpt("recorded","Date recorded",sort)}${libOpt("year","Year",sort)}${libOpt("resolution","Resolution",sort)}</select>
      <span class="lbl" style="margin-left:8px">Show</span>
      <select data-library-watch-filter aria-label="Watch status" onchange="LIB_FILTER=this.value;${m.reload}">
        ${libOpt("all","Everything",LIB_FILTER)}${libOpt("unwatched","Unwatched",LIB_FILTER)}${libOpt("inprogress","In progress",LIB_FILTER)}${libOpt("watched","Watched",LIB_FILTER)}</select>
      ${libraryPageSize()}
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
  LIB_PAGE_AT=0;LIB_SCOPE="";LIB_FIND="";LIB_GROUP="";LIB_VIEW=null;
  const sort=o.sort,now=new Date(),rowState={rows:new Map()};
  const route=location.hash;
  setOrigin(o.href,o.title);
  document.getElementById("main").innerHTML=
    layoutRegion("library","shell",{kind:"library",title:o.title,href:o.href,sort,reload:o.reload});
  const viewLibs=(await libsCached()).filter(l=>o.libIds.some(id=>String(id)===String(l.id)));
  if(LIB_LOAD!==gen||location.hash!==route||!document.getElementById("libbody"))return;
  // Keep controls outside layout-owned grid wrappers: catalog puts #libbody
  // beside the A–Z rail, so inserting before it would steal a poster column.
  document.querySelector("#main .libbar").insertAdjacentHTML("afterend",libraryBrowseTools(viewLibs)+`<nav id="library-group-index" aria-label="Jump to group" hidden></nav><div id="library-group-detail"></div><div id="library-load-error" role="status"></div>`);

  let all=[], total=0,failed=false;
  // How many cards the grid in #libbody currently holds, and 0 whenever what is
  // on screen is not the head of `page` — an empty state, a filtered view, a
  // rebuild. Only a draw that can prove the next page begins with exactly these
  // cards may append to them.
  let painted=0;
  const draw=(done)=>{
    const body=document.getElementById("libbody"); if(!body||LIB_LOAD!==gen||location.hash!==route) return;
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
    const groups=libraryGroups(items,sort,now);
    const rows=LIB_PRESENTATION==="rows"&&!LIB_GROUP;
    if(LIB_GROUP)items=groups.find(g=>g.key===LIB_GROUP)?.items||[];
    libraryPaintControls(groups,rows,done);
    const shown=items.length;
    LIB_VIEW={draw, done, shown};
    const pages=rows?1:libPageCount(shown);
    if(LIB_PAGE_AT>=pages) LIB_PAGE_AT=pages-1;   // the filter shrank the list under us
    const from=rows||LIB_PER==="all"?0:LIB_PAGE_AT*LIB_PER;
    const page=rows||LIB_PER==="all"?items:items.slice(from, from+LIB_PER);

    // One model, three regions. `loaded` is the raw arrival count and `shown`
    // the count after filtering — the loading line reports the former because
    // it is describing progress, not results.
    const m={kind:"library", title:o.title, href:o.href, sort, filter:LIB_FILTER,
      per:LIB_PER, pageAt:LIB_PAGE_AT, pages, from, items:page,
      loaded:all.length, shown, total, done, reload:o.reload};
    // The one place a batch can extend the grid instead of replacing it
    // (F-web-11). Everything that can reorder or hide what is already painted
    // keeps the rebuild, because appending is only correct when the page that
    // exists is a PREFIX of the page that should exist:
    //   o.resort      a category re-sorts the merged set, so batch 3 of a
    //                 four-library category may belong before batch 1's cards
    //   sort          resolution renders sections, not one flat grid
    //   filter/scope/find   change which items are visible, not just how many
    //   LIB_PER       a page slice moves its window as the list grows
    // The alpha rail, the count and the pager still redraw from the whole page;
    // only the cards are spared. Keeping the existing nodes is the point: an
    // innerHTML rebuild drops the reader's focus and every decoded poster.
    const extendable=!rows&&!LIB_GROUP&&!o.resort && sort!=="resolution" && LIB_FILTER==="all"
      && !LIB_SCOPE && !LIB_FIND.trim() && LIB_PER==="all";
    const grown=extendable && painted>0 && page.length>=painted
      ? body.querySelector(".grid") : null;
    if(rows){
      libraryPaintRows(body,groups,done&&!failed,rowState);
    }else if(grown){
      if(page.length>painted)
        grown.insertAdjacentHTML("beforeend", page.slice(painted).map(i=>card(i)).join(""));
      // `grid()` did not run, so the lightbox's photo set is this draw's job.
      rememberGridPhotos(page);
    }else{body.classList.remove("library-rows");body.innerHTML=layoutRegion("library","items",m);}
    painted=extendable? page.length : 0;
    const pager=document.getElementById("libpager");
    if(pager) pager.innerHTML = done&&!rows? pagerHtml(shown) : "";
    // The A–Z rail maps to what is on screen, so it describes this page. Its
    // mount is placed by the shell, so a layout that wants it on the right edge
    // moves the mount rather than reimplementing the rail.
    const rail=document.getElementById("librail");
    if(rail) rail.innerHTML=rows||LIB_GROUP?"":alphaRailHtml(page, sort);
    const count=document.getElementById("libcount");
    if(count) count.innerHTML=failed?`Incomplete results · ${all.length} items loaded`:layoutRegion("library","count",m);
    if(!rows)tagAlpha(page, sort);
    else libraryUpdateIndex();
    const error=document.getElementById("library-load-error");
    if(error)error.innerHTML=failed?`<p>Some items could not be loaded. The groups and counts are incomplete. <button class="ghost sm" onclick="${o.reload}">Retry</button></p>`:"";
  };

  const complete=await eachItemPage(o.libIds, sort, gen, (batch, t)=>{
    all=all.concat(batch);
    total=t;
    draw(false);
  });
  if(LIB_LOAD!==gen||location.hash!==route) return;
  failed=!complete;
  draw(true);
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
    resort:(items,sort)=>items.sort((a,b)=>libraryItemCompare(a,b,sort)),
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

