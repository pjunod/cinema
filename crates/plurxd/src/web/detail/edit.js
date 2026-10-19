"use strict";
// ---- edit details (home libraries, admin) ---------------------------------
// The DB is the only truth for home items — there is no agent to overwrite
// what you type here, and plurx never writes back to the .nfo (or anything
// else) in your media folder.
let EDIT_TAGS=[];
let EDIT_OPENER=null;
function openEdit(it){
  EDIT_TAGS=(it.tags||[]).slice();
  const wrap=document.createElement("div");
  wrap.className="editwrap"; wrap.id="editwrap";
  // Same three promises the connect dialog already makes: it is a dialog, it
  // closes on Escape, and it gives focus back to the control that opened it.
  wrap.setAttribute("role","dialog");
  wrap.setAttribute("aria-modal","true");
  wrap.setAttribute("aria-label","Edit details");
  wrap.innerHTML=`<div class="editcard">
    <h2 class="section" style="margin-top:0">Edit details</h2>
    <label for="ed-title">Title</label>
    <input id="ed-title" value="${esc(it.title||"")}">
    <label for="ed-date">Date recorded</label>
    <input id="ed-date" type="date" value="${esc((it.recorded_at||"").slice(0,10))}">
    <label for="ed-over">Description</label>
    <textarea id="ed-over" rows="3">${esc(it.overview||"")}</textarea>
    <label for="ed-tag">Tags</label>
    <div class="chips-in" id="ed-chips" onclick="document.getElementById(\'ed-tag\').focus()"></div>
    <div class="hint">Enter or comma adds a tag; tags are searchable.</div>
    <div class="err" id="ed-err"></div>
    <div class="row" style="margin-top:14px;justify-content:flex-end">
      <button class="ghost" onclick="closeEdit()">Cancel</button>
      <button onclick="saveEdit(${it.id})">Save</button>
    </div></div>`;
  wrap.addEventListener("click",e=>{ if(e.target===wrap) closeEdit(); });
  EDIT_OPENER=document.activeElement;
  document.body.appendChild(wrap);
  paintTagChips();
  const t=document.getElementById("ed-title"); if(t) t.focus();
}
function paintTagChips(){
  const box=document.getElementById("ed-chips"); if(!box) return;
  box.innerHTML=EDIT_TAGS.map((t,i)=>`<span class="chip">${esc(t)}<b onclick="dropTag(${i})" title="Remove">×</b></span>`).join("")
    +`<input id="ed-tag" placeholder="${EDIT_TAGS.length?"":"beach, kids"}" onkeydown="tagKey(event)">`;
}
function dropTag(i){ EDIT_TAGS.splice(i,1); paintTagChips(); document.getElementById("ed-tag").focus(); }
function tagKey(event){
  const el=event.target;
  if(event.key==="Enter"||event.key===","){
    event.preventDefault();
    const v=(el.value||"").trim();
    if(v && !EDIT_TAGS.some(t=>t.toLowerCase()===v.toLowerCase())) EDIT_TAGS.push(v);
    el.value=""; paintTagChips(); document.getElementById("ed-tag").focus();
  } else if(event.key==="Backspace" && !el.value && EDIT_TAGS.length){
    EDIT_TAGS.pop(); paintTagChips(); document.getElementById("ed-tag").focus();
  }
}
function closeEdit(){
  const w=document.getElementById("editwrap"); if(w) w.remove();
  const opener=EDIT_OPENER; EDIT_OPENER=null;
  if(opener&&opener.focus&&document.contains(opener)) opener.focus({preventScroll:true});
}
async function saveEdit(id){
  const title=(document.getElementById("ed-title").value||"").trim();
  const date=(document.getElementById("ed-date").value||"").trim();
  const over=(document.getElementById("ed-over").value||"").trim();
  const pending=(document.getElementById("ed-tag").value||"").trim();
  const tags=EDIT_TAGS.slice();
  // Don't lose a tag someone typed but didn't commit with Enter.
  if(pending && !tags.some(t=>t.toLowerCase()===pending.toLowerCase())) tags.push(pending);
  const err=document.getElementById("ed-err"); if(err) err.textContent="";
  // null clears a field; an empty date/description means "clear it".
  const body={ title, recorded_at: date||null, overview: over||null, tags };
  try{
    await api(`/items/${id}`,{method:"PATCH",body});
    closeEdit(); toast("Saved"); viewItem(id);
  }catch(e){ if(err) err.textContent=e.message; }
}

// The mark-watched control(s) for a detail page. A movie or episode is binary,
// so it gets one button that flips it. A show, season, or folder is not: it can
// sit at seven of ten, where "Mark watched" and "Mark unwatched" both do
// something real and a single toggle would have to lie about one of them. So a
// container gets whichever of the two actually changes anything.
function watchControls(it){
  return watchedControls(it)+listToggleSlot(it);
}
function watchedControls(it){
  const r=it.rollup;
  if(r){
    if(!r.leaves) return "";   // nothing playable underneath — nothing to mark
    const noun = it.kind==='show'?'series' : it.kind==='season'?'season' : 'folder';
    const btns=[];
    if(r.watched<r.leaves) btns.push(`<button class="ghost sm" onclick="setWatched(${it.id},1)">Mark ${noun} watched</button>`);
    if(r.watched>0)        btns.push(`<button class="ghost sm" onclick="setWatched(${it.id},0)">Mark ${noun} unwatched</button>`);
    return btns.join("");
  }
  const w=!!(it.watch&&it.watch.watched);
  return `<button class="ghost sm" onclick="setWatched(${it.id},${w?0:1})">${w?'Mark unwatched':'Mark watched'}</button>`;
}

// ---- annotations: review-site scores, watchlist and favorites ----------------
// These ride beside the item page, not inside it: `/items/{id}` has a read
// budget and a score may need a provider round trip. The page paints empty
// slots; paintItemAnnotations fills them when `/items/{id}/annotations` lands.
const RATING_SOURCE_NAMES={imdb:"IMDb",rotten_tomatoes:"Rotten Tomatoes",metacritic:"Metacritic",tmdb:"TMDB"};
// Only movies and shows go on a watchlist or carry review-site scores.
function listToggleSlot(it){
  return (it.kind==="movie"||it.kind==="show")?`<span class="listtoggles" data-annotation-lists="${esc(it.id)}"></span>`:"";
}
function ratingsSlot(it){
  return (it.kind==="movie"||it.kind==="show")?`<div class="ratingrow" data-annotation-ratings="${esc(it.id)}"></div>`:"";
}
// How a score reads: IMDb out of ten with one decimal, the rest as a whole
// number, Rotten Tomatoes and TMDB as percentages.
function ratingText(r){
  if(r.scale===10) return {score:Number(r.value).toFixed(1),suffix:"/10"};
  const n=Math.round(Number(r.value));
  return r.source==="metacritic"?{score:String(n),suffix:"/100"}:{score:`${n}%`,suffix:""};
}
// The one place a source's mark is drawn. Original glyphs, keyed by source:
// change a source's artwork here and every rating on the web changes with it.
function ratingMark(r){
  const v=Number(r.value);
  switch(r.source){
    case "imdb":
      return `<svg class="rmark" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 2.8l2.8 5.9 6.4.8-4.7 4.4 1.2 6.4L12 17.2l-5.7 3.1 1.2-6.4-4.7-4.4 6.4-.8z" fill="#fbbf24"/></svg>`;
    case "rotten_tomatoes":
      return v>=60
        ? `<svg class="rmark" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10" fill="#ef4444"/><path d="M7.5 12.3l3 3 6-6.3" stroke="#fff" stroke-width="2.4" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>`
        : `<svg class="rmark" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10" fill="#65a30d"/><path d="M8.5 8.5l7 7M15.5 8.5l-7 7" stroke="#fff" stroke-width="2.4" stroke-linecap="round"/></svg>`;
    case "metacritic": {
      const band=v>=61?"#22c55e":v>=40?"#fbbf24":"#ef4444";
      return `<svg class="rmark" viewBox="0 0 24 24" aria-hidden="true"><rect x="1.5" y="1.5" width="21" height="21" rx="5" fill="${band}"/><text x="12" y="16.2" text-anchor="middle" font-size="10.5" font-weight="800" fill="#07101f">${Math.round(v)}</text></svg>`;
    }
    case "tmdb":
      return `<svg class="rmark" viewBox="0 0 24 24" aria-hidden="true"><circle cx="9" cy="8" r="3.4" fill="#6ea8fe"/><path d="M2.5 19.5c0-3.6 2.9-6 6.5-6s6.5 2.4 6.5 6z" fill="#6ea8fe"/><circle cx="17" cy="9" r="2.6" fill="#8b7cf6"/><path d="M14.8 13.9c3.4-.6 6.7 1.4 6.7 5.6h-4.6" fill="#8b7cf6"/></svg>`;
    default: return "";
  }
}
function ratingsHtml(ratings){
  return (ratings||[]).filter(r=>RATING_SOURCE_NAMES[r.source]).map(r=>{
    const name=RATING_SOURCE_NAMES[r.source], t=ratingText(r);
    const votes=r.votes?` from ${Number(r.votes).toLocaleString()} votes`:"";
    const label=`${name} · ${t.score}${t.suffix}${votes}`;
    return `<span class="rating" title="${esc(label)}" aria-label="${esc(label)}">${ratingMark(r)}<span class="rtext"><span class="rscore">${esc(t.score)}${t.suffix?`<span class="rsuffix">${t.suffix}</span>`:""}</span><span class="rsrc">${esc(name)}</span></span></span>`;
  }).join("");
}
function listTogglesHtml(id,a){
  const on=!!a.watchlist, fav=!!a.favorite;
  const mark=`<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 3h12v18l-6-4.5L6 21z" fill="${on?"currentColor":"none"}" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/></svg>`;
  const heart=`<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 20.5S3.5 15.4 3.5 9.3A4.6 4.6 0 0 1 12 6.9a4.6 4.6 0 0 1 8.5 2.4c0 6.1-8.5 11.2-8.5 11.2z" fill="${fav?"currentColor":"none"}" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/></svg>`;
  return `<button class="ghost sm listtoggle${on?" on":""}" aria-pressed="${on}" onclick="setListed(${id},'watchlist',${on?0:1})">${mark}${on?"On watchlist":"Watchlist"}</button>`+
    `<button class="ghost sm listtoggle fav${fav?" on":""}" aria-pressed="${fav}" aria-label="${fav?"Remove from favorites":"Add to favorites"}" title="${fav?"Remove from favorites":"Add to favorites"}" onclick="setListed(${id},'favorite',${fav?0:1})">${heart}</button>`;
}
async function paintItemAnnotations(id,generation=PAGE_RENDER_GENERATION){
  const route=location.hash;
  if(!document.querySelector(`[data-annotation-lists="${id}"],[data-annotation-ratings="${id}"]`)) return;
  let a;
  try{ a=await api(`/items/${id}/annotations`); }catch(e){ return; }
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route) return;
  for(const el of document.querySelectorAll(`[data-annotation-ratings="${id}"]`)) el.innerHTML=ratingsHtml(a.ratings);
  for(const el of document.querySelectorAll(`[data-annotation-lists="${id}"]`)) el.innerHTML=listTogglesHtml(id,a);
}
async function setListed(id,list,on){
  try{
    await api(`/items/${id}/${list}`,{method:on?"PUT":"DELETE"});
    toast(list==="watchlist"?(on?"Added to watchlist":"Removed from watchlist"):(on?"Added to favorites":"Removed from favorites"));
    paintItemAnnotations(id);
  }catch(e){ toast(e.message); }
}

// `watched` is the state to move to, not the state you're in — the container
// buttons name an outcome rather than a flip, and one meaning for the argument
// is worth more than a shorter call site.
async function setWatched(id, watched){
  try{
    const r=await api(`/items/${id}/${watched?'scrobble':'unscrobble'}`,{method:"POST"});
    // The count matters on a series: it's the difference between "did that do
    // anything?" and "26 episodes marked watched".
    const n=(r&&r.updated)||0;
    toast(n>1 ? `${n} items marked ${watched?'watched':'unwatched'}`
              : (watched?"Marked watched":"Marked unwatched"));
    viewItem(id);
  }catch(e){ toast(e.message); }
}

