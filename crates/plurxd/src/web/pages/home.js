"use strict";
// ---- the page-model seam --------------------------------------------------
// Every converted route splits in two: a loader that fetches and returns a
// plain JSON-shaped object (no DOM, no HTML), and a layout renderer that turns
// that object into markup. Shared behaviour wiring runs after, once, for every
// layout.
//
//   hash route ─▶ loadPage(route) ─▶ page model ─▶ LAYOUTS[cur].views.x(page)
//                    (fetches)      (plain data)      (body HTML)
//                                                          │
//                                                          ▼
//                                                  shared wiring
//
// Why the split rather than a chrome callback around the existing views: the
// views own the whole sequence today — install chrome, fetch, assemble, replace
// #main, wire. A registry wrapped around that either fetches twice or has each
// layout renderer reaching into route-specific globals. Route by route, Home
// first; activity, settings, search, auth and the player keep their existing
// renderers behind the layout chrome until a gate converts them.
//
// Turn the fixed-size server batch into the layout-neutral Home model. The
// browser never fans out by library: every roster size is the same three HTTP
// requests (/hubs, /home/previews, and /coming-soon).
function buildHomeSections(batch,group=homeGroup()){
  const previews=(batch&&batch.libraries)||[];
  const libs=previews.map(preview=>preview.library);
  const sections=[];
  if(libs.length){
    const byId=Object.fromEntries(previews.map(preview=>[preview.library.id,preview]));
    if(group==="category"){
      // Bucket libraries by category, keeping Movies before TV before others.
      const order=[], byKey={};
      for(const lib of libs){ const c=libCategory(lib); if(!byKey[c.key]){ byKey[c.key]={cat:c,libs:[]}; order.push(c.key); } byKey[c.key].libs.push(lib); }
      order.sort((a,b)=>catOrder(a)-catOrder(b)||a.localeCompare(b));
      for(const key of order){
        const g=byKey[key];
        const pages=g.libs.map(l=>byId[l.id]);
        sections.push({
          key,
          name:g.cat.name,
          items:[].concat(...pages.map(p=>p.items)).slice(0,24),
          total:pages.reduce((n,p)=>n+(p.total||0),0),
          unavailable:false,
          // One share in this category → link straight to it; several → merged category page.
          href:g.libs.length===1 ? `#/library/${g.libs[0].id}` : `#/category/${encodeURIComponent(key)}`,
        });
      }
    } else {
      for(const lib of libs){
        const page=byId[lib.id];
        sections.push({key:String(lib.id), name:lib.name, items:page.items, total:page.total,
          unavailable:false, href:`#/library/${lib.id}`});
      }
    }
  }
  return {libs,group,sections};
}
async function viewHome(generation=++PAGE_RENDER_GENERATION){
  const route=location.hash;
  const page={kind:"home",
    hubs:{continue_watching:[],next_up:[],recently_added:[]},lists:{watchlist:[],favorite:[]},
    soon:{configured:false,entries:[]},libs:[],group:homeGroup(),sections:[],
    hubsPending:true,previewsPending:true,soonPending:true,
    hubsError:false,previewsError:false,soonError:false};
  registerLayoutPresentation(()=>recomposeHomePresentation(page),()=>!page.hubsPending||!page.previewsPending);
  const current=()=>generation===PAGE_RENDER_GENERATION&&location.hash===route;
  const commit=(name,content=false)=>{
    if(!current()) return;
    const main=document.getElementById("main"); if(!main) return;
    const template=document.createElement("template");
    const keepPhotos=name==="previews"?null:PHOTO_SET, keepLightboxAt=LB_AT;
    template.innerHTML=layoutView("home",page)||`<div class="empty">Nothing here yet.</div>`;
    if(keepPhotos){ PHOTO_SET=keepPhotos; LB_AT=keepLightboxAt; }
    const incoming=template.content.querySelectorAll(`[data-home-region="${name}"]`);
    for(const next of incoming){
      const slot=next.dataset.homeSlot;
      const old=main.querySelector(`[data-home-region="${name}"][data-home-slot="${slot}"]`);
      if(old) old.replaceWith(next);
    }
    wireRails();
    if(content) setPagePhase(route,generation,"content");
    if(name==="previews") restoreScroll();
  };
  layoutChrome("home",layoutView("home",page));
  setPagePhase(route,generation,"shell");
  const region=(name,promise,apply)=>promise.then(value=>{
    if(!current()) return;
    page[`${name}Pending`]=false; const meaningful=apply(value); commit(name,meaningful);
  }).catch(error=>{
    if(error&&error.status===401) throw error;
    if(!current()) return;
    page[`${name}Pending`]=false; page[`${name}Error`]=true;
    const visible=name==="hubs"||name==="previews";
    if(visible) setPageFailure(route,generation,`${name}_error`);
    commit(name,visible);
  });
  await Promise.allSettled([
    region("hubs",api("/hubs"),value=>{
      page.hubs=value;
      return ["continue_watching","next_up","recently_added"].some(key=>(value[key]||[]).length);
    }),
    region("previews",api("/home/previews"),value=>{
      Object.assign(page,buildHomeSections(value,page.group));
      return true; // even an empty roster paints the useful "No libraries" state
    }),
    region("soon",api("/coming-soon"),value=>{ page.soon=value; return !!(value.entries||[]).length; }),
    // The reader's lists paint into the hubs region, so a person with none
    // sees exactly the Home they always did.
    Promise.all([api("/lists/watchlist?limit=20"),api("/lists/favorite?limit=20")]).then(([w,f])=>{
      if(!current()) return;
      page.lists={watchlist:w.items||[],favorite:f.items||[]};
      if(page.lists.watchlist.length||page.lists.favorite.length) commit("hubs",!page.hubsPending);
    }).catch(()=>{ /* Home stays whole without the lists. */ }),
  ]);
  if(!current()) return;
  setPagePhase(route,generation,"settled");
}

// #/list/watchlist · #/list/favorite — one of the reader's lists, whole.
async function viewList(list){
  const generation=PAGE_RENDER_GENERATION,route=location.hash;
  const title=list==="favorite"?"Favorites":"Watchlist";
  layoutChrome("home",`<div class="empty">Loading…</div>`);
  let r;
  try{ r=await api(`/lists/${encodeURIComponent(list)}`); }
  catch(e){ if(generation===PAGE_RENDER_GENERATION&&location.hash===route) document.getElementById("main").innerHTML=`<div class="empty">${esc(e.message)}</div>`; return; }
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route) return;
  setOrigin(route,title);
  const items=r.items||[];
  const head=pageHead([{href:"#/",label:"Home"},{label:title}],title);
  const empty=list==="favorite"
    ? "Nothing here yet. Use the heart on a movie or show to add it."
    : "Nothing here yet. Add movies and shows with Watchlist on their page; a movie leaves once you have watched it.";
  document.getElementById("main").innerHTML=head+(items.length?grid(items):`<div class="empty">${empty}</div>`);
}
