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

