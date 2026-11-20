"use strict";
// ---- custom tags, saved rows and Home order (CUSTOM-TAGS-PLAN.md §5) --------
// Tags are the reader's own labels on titles (household or private), rows are
// saved rules over them, and Home is drawn in the order the reader chose. The
// server decides who may see and change what; this file only draws it.

const CURATION={tags:null,at:0,canManage:false};
const TAG_SWATCH={violet:"#a78bfa",blue:"#60a5fa",teal:"#2dd4bf",green:"#4ade80",lime:"#a3e635",
  amber:"#fbbf24",orange:"#fb923c",red:"#f87171",pink:"#f472b6",slate:"#94a3b8"};
const HOME_BUILTIN=["continue_watching","next_up","watchlist","favorites","recently_added","coming_soon","libraries"];
const HOME_BUILTIN_NAMES={continue_watching:"Continue watching",next_up:"Next up",watchlist:"Watchlist",
  favorites:"Favorites",recently_added:"Recently added",coming_soon:"Coming soon",libraries:"Library previews"};
// Per person: the next account signed in on this browser must not paint in
// the previous one's order.
function homeOrderKey(){return `plurx.home.order.v1.${ME&&ME.id!=null?ME.id:"anon"}`;}
/** @param {string} id */
function dialogById(id){return /** @type {HTMLDialogElement|null} */(document.getElementById(id));}
/** @param {string} id */
function fieldValue(id){const el=/** @type {HTMLInputElement|HTMLSelectElement|null} */(document.getElementById(id));return el?el.value:"";}
/** @param {ParentNode} root @param {string} selector */
function inputIn(root,selector){return /** @type {HTMLInputElement|null} */(root.querySelector(selector));}
/** @param {ParentNode} root @param {string} selector */
function inputsIn(root,selector){return /** @type {NodeListOf<HTMLInputElement>} */(root.querySelectorAll(selector));}
const LOCK_ICON=`<svg class="tag-lock" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" aria-label="Only you"><rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg>`;

async function curationTags(force){
  if(!force&&CURATION.tags&&Date.now()-CURATION.at<30000) return CURATION.tags;
  const r=await api("/tags");
  CURATION.tags=r.tags||[];CURATION.canManage=!!r.can_manage_household;CURATION.at=Date.now();
  return CURATION.tags;
}
function tagChip(t,extra=""){
  const color=TAG_SWATCH[t.color]||TAG_SWATCH.slate;
  return `<span class="tag-chip${t.archived?" archived":""}" style="--tag:${color}"><span class="tag-dot" aria-hidden="true"></span>${t.scope==="private"?LOCK_ICON:""}${esc(t.name)}${extra}</span>`;
}

// ---- Home ------------------------------------------------------------------
// The keys Home draws, in order. The server's answer wins; before it lands the
// last answer this browser saw is used, so a rearranged Home does not paint in
// the default order first and then jump.
function homeOrder(p){
  if(p.homeRows&&Array.isArray(p.homeRows.layout)) return p.homeRows.layout.map(e=>e.key);
  try{const cached=JSON.parse(localStorage.getItem(homeOrderKey())||"null");if(Array.isArray(cached))return cached;}catch(e){/* storage may be unavailable */}
  return HOME_BUILTIN;
}
function homeRowRail(id,row){
  const head=`${esc(row.title)} <a class="muted" style="font-size:12px" href="#/row/${Number(id)}">· see all (${Number(row.total)||0})</a>`;
  const more=row.total>row.items.length?`<a class="seeall" href="#/row/${Number(id)}">See all</a>`:"";
  return rail(head,row.items.map(i=>card(i)).join("")+more,"row");
}
// Every Home layout's shelves, in the reader's order, one region slot each.
// A slot is emitted even when empty so a region that lands later (lists,
// Coming soon, a custom row) has its place to paint into.
function homeRegions(p,previews){
  const hub=k=>(p.hubs&&p.hubs[k])||[];
  const lists=p.lists||{};
  const listRail=(key,heading)=>(lists[key]||[]).length
    ? rail(heading,lists[key].map(i=>card(i)).join("")+`<a class="seeall" href="#/list/${key}">See all</a>`,"list") : "";
  const soon=(p.soon&&p.soon.entries&&p.soon.entries.length)?rail("Coming soon",p.soon.entries.map(comingCard).join("")):"";
  const shelves={
    continue_watching:["hubs",hub("continue_watching").length?rail("Continue watching",hub("continue_watching").map(i=>card(i,true)).join(""),"watch"):""],
    next_up:["hubs",hub("next_up").length?rail("Next up",hub("next_up").map(i=>card(i,true)).join(""),"watch"):""],
    watchlist:["hubs",listRail("watchlist","Watchlist")],
    favorites:["hubs",listRail("favorite","Favorites")],
    recently_added:["hubs",hub("recently_added").length?rail("Recently added",hub("recently_added").map(i=>card(i)).join("")):""],
    coming_soon:["soon",soon],
    libraries:["previews",previews],
  };
  const lead=installBannerHtml()+(p.hubsError?`<div class="hint">Home recommendations are unavailable right now.</div>`:"")
    +`<div class="home-customize"><button class="ghost sm" onclick="openCustomizeHome()">Customize Home</button></div>`;
  const rowsStatus=p.rowsError?`<div class="hint">Custom rows and Home order are unavailable right now.</div>`:"";
  let out=`<div data-home-region="hubs" data-home-slot="lead">${lead}</div><div data-home-region="rows" data-home-slot="status">${rowsStatus}</div>`;
  for(const key of homeOrder(p)){
    if(shelves[key]) out+=`<div data-home-region="${shelves[key][0]}" data-home-slot="${key}">${shelves[key][1]}</div>`;
    else if(/^row:\d+$/.test(key)){
      const id=key.slice(4),row=p.homeRows&&p.homeRows.rows&&p.homeRows.rows[id];
      out+=`<div data-home-region="rows" data-home-slot="${key}">${row?homeRowRail(id,row):""}</div>`;
    }
  }
  return out;
}
// Called when /home/rows lands: remember the order, and say whether what is
// on screen was drawn in a different one (then the page is redrawn whole).
function homeRowsArrived(p,value){
  const before=homeOrder(p).join(",");
  p.homeRows=value;
  try{localStorage.setItem(homeOrderKey(),JSON.stringify((value.layout||[]).map(e=>e.key)));}catch(e){/* best effort */}
  return homeOrder(p).join(",")!==before;
}

// ---- Customize Home ----------------------------------------------------------
let HOME_EDIT=null;
function curationDialog(id,title,body){
  document.getElementById(id)?.remove();
  const d=document.createElement("dialog");d.id=id;d.className="card curation-dialog";
  d.setAttribute("aria-labelledby",`${id}-title`);
  d.innerHTML=`<div class="row" style="justify-content:space-between;align-items:center"><h2 id="${id}-title">${esc(title)}</h2><button class="ghost sm" aria-label="Close" onclick="this.closest('dialog').close()">✕</button></div><div class="curation-body">${body}</div>`;
  const close=()=>d.close();window.addEventListener("hashchange",close);
  d.addEventListener("close",()=>{window.removeEventListener("hashchange",close);d.remove();});
  document.body.appendChild(d);d.showModal();return d;
}
async function openCustomizeHome(){
  const d=curationDialog("customize-home","Customize Home",`<p class="muted">Loading…</p>`);
  try{
    const r=await api("/home/layout");
    if(!d.open)return;
    HOME_EDIT={entries:r.entries.map(e=>({key:e.key,hidden:!!e.hidden})),rows:r.rows||[],canManage:r.can_manage_household,isAdmin:r.is_admin,source:r.source};
    paintCustomizeHome();
  }catch(e){if(d.isConnected)d.querySelector(".curation-body").textContent=e.message;}
}
function homeEntryLabel(key){
  if(HOME_BUILTIN_NAMES[key]) return [HOME_BUILTIN_NAMES[key],"built-in",null];
  const row=HOME_EDIT.rows.find(r=>`row:${r.id}`===key);
  return row?[row.title,row.scope==="household"?"household row":"my row",row]:[key,"",null];
}
function paintCustomizeHome(){
  const d=dialogById("customize-home");if(!d||!HOME_EDIT)return;
  const n=HOME_EDIT.entries.length;
  const rows=HOME_EDIT.entries.map((e,i)=>{
    const [label,kind,row]=homeEntryLabel(e.key);
    return `<li class="home-entry${e.hidden?" hidden-entry":""}">
      <span class="home-entry-name">${esc(label)} <span class="muted">${esc(kind)}</span></span>
      <span class="row" style="gap:4px">
        <button class="ghost sm" aria-label="Move ${esc(label)} up" ${i===0?"disabled":""} onclick="moveHomeEntry(${i},-1)">↑</button>
        <button class="ghost sm" aria-label="Move ${esc(label)} down" ${i===n-1?"disabled":""} onclick="moveHomeEntry(${i},1)">↓</button>
        <label class="row" style="gap:4px"><input type="checkbox" ${e.hidden?"":"checked"} onchange="HOME_EDIT.entries[${i}].hidden=!this.checked;paintCustomizeHome()">Show</label>
        ${row&&row.editable?`<button class="ghost sm" onclick="openRowEditor(${row.id})">Edit</button>`:""}
      </span></li>`;
  }).join("");
  const source=HOME_EDIT.source==="household"?"You are using the household default.":HOME_EDIT.source==="user"?"This is your own arrangement.":"This is the standard Home.";
  d.querySelector(".curation-body").innerHTML=`<p class="muted">${source} Hidden rows stay here so you can bring them back.</p>
    <ol class="home-entries">${rows}</ol>
    <div class="row" style="gap:8px;flex-wrap:wrap;margin-top:12px">
      <button onclick="openRowEditor()">Add a row</button>
      <a class="ghost sm" href="#/tags" onclick="this.closest('dialog').close()">Manage tags</a>
      <span style="flex:1"></span>
      ${HOME_EDIT.source==="user"?`<button class="ghost" onclick="resetHomeLayout()">Reset</button>`:""}
      ${HOME_EDIT.isAdmin?`<button class="ghost" onclick="saveHomeLayout(true)">Save as household default</button>`:""}
      <button onclick="saveHomeLayout(false)">Save</button>
    </div><p id="customize-home-error" role="status"></p>`;
}
function moveHomeEntry(i,step){
  const e=HOME_EDIT.entries,j=i+step;if(j<0||j>=e.length)return;
  [e[i],e[j]]=[e[j],e[i]];paintCustomizeHome();
}
async function saveHomeLayout(household){
  const d=dialogById("customize-home");
  try{
    // The household default can only place household rows: the admin's own
    // private rows stay in their own layout, not everyone's.
    const privateKeys=new Set(HOME_EDIT.rows.filter(r=>r.scope==="private").map(r=>`row:${r.id}`));
    const entries=household?HOME_EDIT.entries.filter(e=>!privateKeys.has(e.key)):HOME_EDIT.entries;
    await api(household?"/home/default-layout":"/home/layout",{method:"PUT",body:{entries}});
    if(d)d.close();toast(household?"Household default saved":"Home saved");
    if(location.hash==="#/"||location.hash===""||location.hash==="#") render();
  }catch(e){const el=d&&d.querySelector("#customize-home-error");if(el)el.textContent=e.message;}
}
async function resetHomeLayout(){
  try{await api("/home/layout",{method:"DELETE"});dialogById("customize-home")?.close();toast("Home reset");render();}
  catch(e){toast(e.message);}
}

// ---- row editor --------------------------------------------------------------
let ROW_EDIT=null;
async function openRowEditor(rowId){
  if(ROW_EDIT?.saving)return;
  const [tags,libs]=await Promise.all([curationTags(true),libsCached()]);
  if(ROW_EDIT?.saving||!dialogById("customize-home")?.open)return;
  const existing=rowId?(HOME_EDIT&&HOME_EDIT.rows||[]).find(r=>r.id===rowId):null;
  const spec=existing?JSON.parse(JSON.stringify(existing.spec)):{all_of:[],any_of:[],none_of:[],libraries:[],kinds:[],sort:"recently_tagged",unwatched:"first"};
  // A deleted tag's condition matches nothing; editing the row drops it.
  const known=new Set(tags.map(t=>t.id));
  for(const g of ["all_of","any_of","none_of"]) spec[g]=(spec[g]||[]).filter(t=>t.tag==null||known.has(t.tag));
  ROW_EDIT={id:rowId||null,title:existing?existing.title:"",scope:existing?existing.scope:"private",spec,tags:tags.filter(t=>!t.archived),libs,saving:false};
  const d=curationDialog("row-editor",rowId?"Edit row":"New row","");
  paintRowEditor(d);
}
function rowTermGroup(group,label){
  const on=new Set((ROW_EDIT.spec[group]||[]).filter(t=>t.tag!=null).map(t=>t.tag));
  const genres=(ROW_EDIT.spec[group]||[]).filter(t=>t.genre!=null).map(t=>t.genre);
  return `<fieldset class="row-group"><legend>${label}</legend>
    <div class="row" style="flex-wrap:wrap;gap:6px">${ROW_EDIT.tags.map(t=>`<label class="tag-pick"><input type="checkbox" ${on.has(t.id)?"checked":""} onchange="toggleRowTerm('${group}',${t.id},this.checked)">${tagChip(t)}</label>`).join("")||'<span class="muted">No tags yet.</span>'}</div>
    <label class="muted">Genres (comma separated) <input value="${esc(genres.join(", "))}" onchange="setRowGenres('${group}',this.value)"></label></fieldset>`;
}
function paintRowEditor(d){
  d=d||dialogById("row-editor");if(!d||!ROW_EDIT)return;
  const s=ROW_EDIT.spec,opt=(v,l,cur)=>`<option value="${v}"${v===cur?" selected":""}>${l}</option>`;
  const libs=new Set(s.libraries||[]),kinds=new Set(s.kinds||[]);
  d.querySelector(".curation-body").innerHTML=`
    <label>Row title <input id="row-title" maxlength="80" value="${esc(ROW_EDIT.title)}" oninput="ROW_EDIT.title=this.value"></label>
    ${rowTermGroup("all_of","Must have all of")}${rowTermGroup("any_of","Has any of")}${rowTermGroup("none_of","Leave out")}
    <fieldset class="row-group"><legend>Where</legend><div class="row" style="flex-wrap:wrap;gap:10px">
      ${ROW_EDIT.libs.map(l=>`<label class="row" style="gap:4px"><input type="checkbox" ${libs.has(l.id)?"checked":""} onchange="toggleRowList('libraries',${l.id},this.checked)">${esc(l.name)}</label>`).join("")}
      <span class="muted">(none ticked = every library)</span></div>
      <div class="row" style="gap:10px">${["movie","show"].map(k=>`<label class="row" style="gap:4px"><input type="checkbox" ${kinds.has(k)?"checked":""} onchange="toggleRowList('kinds','${k}',this.checked)">${k==="show"?"Shows":"Movies"}</label>`).join("")}</div></fieldset>
    <div class="row" style="gap:12px;flex-wrap:wrap">
      <label>Sort <select onchange="ROW_EDIT.spec.sort=this.value;rowPreview()">${opt("recently_tagged","Recently tagged",s.sort)}${opt("added","Date added",s.sort)}${opt("released","Release year",s.sort)}${opt("title","Title",s.sort)}${opt("shuffle_daily","Shuffle (daily)",s.sort)}</select></label>
      <label>Watched <select onchange="ROW_EDIT.spec.unwatched=this.value;rowPreview()">${opt("any","Show everything",s.unwatched)}${opt("first","Unwatched first",s.unwatched)}${opt("only","Unwatched only",s.unwatched)}</select></label>
      <label>Show <input type="number" min="1" max="50" style="width:70px" value="${Number(s.limit)||20}" onchange="ROW_EDIT.spec.limit=Math.max(1,Math.min(50,Number(this.value)||20))"></label>
    </div>
    ${CURATION.canManage&&!ROW_EDIT.id?`<div class="row" style="gap:12px"><span>Who sees it</span><label class="row" style="gap:4px"><input type="radio" name="row-scope" ${ROW_EDIT.scope!=="household"?"checked":""} onchange="ROW_EDIT.scope='private'">Just me</label><label class="row" style="gap:4px"><input type="radio" name="row-scope" ${ROW_EDIT.scope==="household"?"checked":""} onchange="ROW_EDIT.scope='household'">Household</label></div>`:""}
    <div class="row" style="gap:8px;margin-top:10px"><span id="row-preview" class="muted" aria-live="polite"></span><span style="flex:1"></span>
      ${ROW_EDIT.id?`<button class="ghost" onclick="deleteRow(${ROW_EDIT.id})">Delete row</button>`:""}
      <button id="row-save" ${ROW_EDIT.saving?"disabled":""} onclick="saveRow()">${ROW_EDIT.id?"Save row":"Add to Home"}</button></div>
    <p id="row-editor-error" role="status"></p>`;
  rowPreview();
}
function toggleRowTerm(group,id,on){
  const list=ROW_EDIT.spec[group]=(ROW_EDIT.spec[group]||[]).filter(t=>t.tag!==id);
  if(on){for(const g of ["all_of","any_of","none_of"]) if(g!==group) ROW_EDIT.spec[g]=(ROW_EDIT.spec[g]||[]).filter(t=>t.tag!==id);list.push({tag:id});}
  paintRowEditor();
}
function setRowGenres(group,value){
  const genres=value.split(",").map(s=>s.trim()).filter(Boolean);
  ROW_EDIT.spec[group]=(ROW_EDIT.spec[group]||[]).filter(t=>t.genre==null).concat(genres.map(g=>({genre:g})));
  rowPreview();
}
function toggleRowList(field,value,on){
  const list=new Set(ROW_EDIT.spec[field]||[]);on?list.add(value):list.delete(value);
  ROW_EDIT.spec[field]=[...list];rowPreview();
}
let ROW_PREVIEW_TIMER=0;
function rowPreview(){
  clearTimeout(ROW_PREVIEW_TIMER);
  ROW_PREVIEW_TIMER=setTimeout(async()=>{
    const el=document.getElementById("row-preview");if(!el)return;
    try{const r=await api("/rows/preview",{method:"POST",body:{spec:ROW_EDIT.spec,limit:1}});
      el.textContent=r.total?`${r.total} matching title${r.total===1?"":"s"}`:"No playable titles match. This row will appear on Home when a title matches.";}
    catch(e){el.textContent=e.message;}
  },250);
}
async function saveRow(){
  const d=dialogById("row-editor"),edit=ROW_EDIT,home=HOME_EDIT;
  if(!d?.open||!edit||edit.saving)return;
  edit.saving=true;
  const button=inputIn(d,"#row-save");
  if(button)button.disabled=true;
  try{
    const body={title:edit.title,spec:edit.spec};
    const row=edit.id?await api(`/rows/${edit.id}`,{method:"PATCH",body})
      :await api("/rows",{method:"POST",body:{...body,scope:edit.scope}});
    d.close();
    if(HOME_EDIT===home&&home){
      home.rows=home.rows.filter(r=>r.id!==row.id).concat([row]);
      if(!home.entries.some(e=>e.key===`row:${row.id}`)) home.entries.push({key:`row:${row.id}`,hidden:false});
      paintCustomizeHome();
      render();
    }
    toast(edit.id?"Row saved":"Row added. Save in Customize Home to keep any changes to the order.");
  }catch(e){
    const err=d.querySelector("#row-editor-error");if(err)err.textContent=e.message;
  }finally{
    edit.saving=false;
    const currentButton=inputIn(d,"#row-save");
    if(currentButton)currentButton.disabled=false;
  }
}
async function deleteRow(id){
  if(!confirm("Delete this row? Its tags stay as they are."))return;
  try{await api(`/rows/${id}`,{method:"DELETE"});dialogById("row-editor")?.close();
    if(HOME_EDIT){HOME_EDIT.rows=HOME_EDIT.rows.filter(r=>r.id!==id);HOME_EDIT.entries=HOME_EDIT.entries.filter(e=>e.key!==`row:${id}`);paintCustomizeHome();}
    toast("Row deleted");}
  catch(e){toast(e.message);}
}

// ---- #/row/<id>: a row's whole result ----------------------------------------
async function viewRow(id){
  const generation=PAGE_RENDER_GENERATION,route=location.hash;
  layoutChrome("home",`<div class="empty">Loading…</div>`);
  let r,rows;
  try{[r,rows]=await Promise.all([api(`/rows/${encodeURIComponent(id)}/items?limit=200`),api("/rows")]);}
  catch(e){if(generation===PAGE_RENDER_GENERATION&&location.hash===route)document.getElementById("main").innerHTML=`<div class="empty">${esc(e.message)}</div>`;return;}
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route)return;
  const row=(rows.rows||[]).find(x=>String(x.id)===String(id));
  const title=row?row.title:"Row";
  setOrigin(route,title);
  document.getElementById("main").innerHTML=pageHead([{href:"#/",label:"Home"},{label:title}],title)
    +`<p class="muted">${Number(r.total)||0} titles${r.total>r.items.length?` · showing the first ${r.items.length}`:""}</p>`
    +(r.items.length?grid(r.items):`<div class="empty">Nothing matches this row right now.</div>`);
}

// ---- detail page: the title's tags -----------------------------------------
async function paintItemTags(id,generation){
  const page=WATCH_ITEM_PAGE;
  if(!page||!page.item||["season","episode"].includes(page.item.kind))return;
  let on,tags;
  try{[on,tags]=await Promise.all([api(`/items/${encodeURIComponent(id)}/tags`),curationTags()]);}
  catch(e){return;}
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==`#/item/${id}`)return;
  const main=document.getElementById("main");if(!main)return;
  let host=document.getElementById("item-tags");
  if(!host){
    host=document.createElement("div");host.id="item-tags";host.className="item-tags";
    const anchor=main.querySelector(".actions");
    if(anchor)anchor.parentNode.insertBefore(host,anchor);else (main.querySelector("h1")||main.firstElementChild||main).insertAdjacentElement("afterend",host);
  }
  host.dataset.itemId=String(id);
  host.innerHTML=`<span class="item-tags-label">Tags</span>`
    +(on.tags||[]).map(t=>`<a class="tag-link" href="#/tag/${t.id}">${tagChip(t)}</a>${t.editable?`<button class="tag-x" aria-label="Remove ${esc(t.name)}" onclick="setItemTag(${Number(id)},${t.id},false)">✕</button>`:""}`).join("")
    +`<button class="ghost sm tag-add" aria-haspopup="dialog" onclick="openTagPicker(${Number(id)})">+ Add tag</button>`;
}
async function setItemTag(itemId,tagId,on){
  try{
    const current=await api(`/items/${itemId}/tags`);
    const ids=new Set((current.tags||[]).filter(t=>t.editable).map(t=>t.id));
    on?ids.add(tagId):ids.delete(tagId);
    await api(`/items/${itemId}/tags`,{method:"PUT",body:{tag_ids:[...ids]}});
    LIB_TAGS.sets.delete(tagId);CURATION.at=0;paintItemTags(itemId,PAGE_RENDER_GENERATION);
  }catch(e){toast(e.message);}
}
async function openTagPicker(itemId){
  const tags=await curationTags(true);
  const d=curationDialog("tag-picker","Add a tag",`<form onsubmit="event.preventDefault();tagPickerEnter(${itemId})"><label>Tag <input id="tag-picker-q" autocomplete="off" enterkeyhint="done" placeholder="Type to find or create…" oninput="paintTagPicker(${itemId})"></label></form><div id="tag-picker-list"></div>${CURATION.canManage?`<div class="row" style="gap:12px"><span class="muted">New tags are</span><label class="row" style="gap:4px"><input type="radio" name="tag-new-scope" value="private" checked>Just me</label><label class="row" style="gap:4px"><input type="radio" name="tag-new-scope" value="household">Household</label></div>`:""}<p id="tag-picker-error" role="status"></p>`);
  d.dataset.tags=JSON.stringify(tags.filter(t=>t.editable&&!t.archived));
  paintTagPicker(itemId);inputIn(d,"#tag-picker-q")?.focus();
}
function paintTagPicker(itemId){
  const d=dialogById("tag-picker");if(!d)return;
  const q=(inputIn(d,"#tag-picker-q")?.value||"").trim(),key=q.toLocaleLowerCase();
  const tags=JSON.parse(d.dataset.tags||"[]").filter(t=>!key||t.name.toLocaleLowerCase().includes(key));
  const exact=tags.some(t=>t.name.toLocaleLowerCase()===key);
  d.querySelector("#tag-picker-list").innerHTML=tags.slice(0,30).map(t=>`<button class="ghost tag-option" onclick="setItemTag(${itemId},${t.id},true);document.getElementById('tag-picker')?.close()">${tagChip(t)} <span class="muted">${t.item_count}</span></button>`).join("")
    +(q&&!exact?`<button class="tag-option tag-create" onclick="tagPickerEnter(${itemId})">Create “${esc(q)}”</button>`:"");
}
async function tagPickerEnter(itemId){
  const d=dialogById("tag-picker");if(!d)return;
  const q=(inputIn(d,"#tag-picker-q")?.value||"").trim();if(!q)return;
  const existing=JSON.parse(d.dataset.tags||"[]").find(t=>t.name.toLocaleLowerCase()===q.toLocaleLowerCase());
  try{
    const scope=inputIn(d,'input[name="tag-new-scope"]:checked')?.value||"private";
    const tag=existing||await api("/tags",{method:"POST",body:{name:q,scope}});
    d.close();await setItemTag(itemId,tag.id,true);
  }catch(e){const el=d.querySelector("#tag-picker-error");if(el)el.textContent=e.message;}
}

// ---- library grid: tag filter and bulk tagging -------------------------------
// The grid loads the whole library and filters in the browser, so a tag filter
// is one id set per chosen tag, applied in the same pass as the other filters.
const LIB_TAGS={include:new Set(),exclude:new Set(),mode:"any",sets:new Map(),select:null,shown:[]};
async function tagItemIds(id){
  if(!LIB_TAGS.sets.has(id)){
    const r=await api(`/tags/${id}/item-ids`);LIB_TAGS.sets.set(id,new Set(r.item_ids||[]));
  }
  return LIB_TAGS.sets.get(id);
}
function libraryTagFilterActive(){return LIB_TAGS.include.size>0||LIB_TAGS.exclude.size>0;}
function libraryTagMatch(it){
  const has=id=>(LIB_TAGS.sets.get(id)||new Set()).has(it.id);
  for(const id of LIB_TAGS.exclude) if(has(id)) return false;
  if(!LIB_TAGS.include.size) return true;
  const inc=[...LIB_TAGS.include];
  return LIB_TAGS.mode==="all"?inc.every(has):inc.some(has);
}
// Called from the grid's draw with the filtered list, so "select all" means
// what is on screen after every filter.
function libraryTagFilter(items){
  const out=libraryTagFilterActive()?items.filter(libraryTagMatch):items;
  LIB_TAGS.shown=out.map(i=>i.id);
  return out;
}
function curationLibraryTools(){
  LIB_TAGS.include.clear();LIB_TAGS.exclude.clear();LIB_TAGS.sets.clear();LIB_TAGS.select=null;
  return `<div class="search-tools library-tag-tools"><button class="ghost sm" id="library-tag-button" aria-haspopup="dialog" onclick="openLibraryTagFilter()">Tags ▾</button><span id="library-tag-chips" class="row" style="gap:6px;flex-wrap:wrap"></span><button class="ghost sm" id="library-select-button" onclick="toggleLibrarySelect()">Select</button></div>`;
}
async function openLibraryTagFilter(){
  const tags=await curationTags(true);
  const d=curationDialog("library-tag-filter","Filter by tags",`<p class="muted">Tick to show titles with a tag; choose “Leave out” to hide them.</p>
    <div class="row" style="gap:12px"><label class="row" style="gap:4px"><input type="radio" name="tag-mode" ${LIB_TAGS.mode!=="all"?"checked":""} onchange="LIB_TAGS.mode='any'">Any of them</label><label class="row" style="gap:4px"><input type="radio" name="tag-mode" ${LIB_TAGS.mode==="all"?"checked":""} onchange="LIB_TAGS.mode='all'">All of them</label></div>
    <ul class="tag-filter-list">${tags.map(t=>{const v=LIB_TAGS.include.has(t.id)?"in":LIB_TAGS.exclude.has(t.id)?"out":"";
      return `<li>${tagChip(t)} <span class="muted">${t.item_count}</span><select aria-label="${esc(t.name)}" data-tag="${t.id}"><option value=""${v===""?" selected":""}>—</option><option value="in"${v==="in"?" selected":""}>Show</option><option value="out"${v==="out"?" selected":""}>Leave out</option></select></li>`;}).join("")||'<li class="muted">No tags yet. Add them on a title, or select titles here and tag them.</li>'}</ul>
    <div class="row" style="gap:8px"><button class="ghost" onclick="clearLibraryTags()">Clear</button><span style="flex:1"></span><button onclick="applyLibraryTagFilter()">Apply</button></div>`);
  return d;
}
async function applyLibraryTagFilter(){
  const d=dialogById("library-tag-filter");if(!d)return;
  LIB_TAGS.include.clear();LIB_TAGS.exclude.clear();
  for(const s of /** @type {NodeListOf<HTMLSelectElement>} */(d.querySelectorAll("select[data-tag]"))){const id=Number(s.dataset.tag);if(s.value==="in")LIB_TAGS.include.add(id);if(s.value==="out")LIB_TAGS.exclude.add(id);}
  try{await Promise.all([...LIB_TAGS.include,...LIB_TAGS.exclude].map(tagItemIds));}catch(e){toast(e.message);return;}
  d.close();paintLibraryTagChips();LIB_PAGE_AT=0;if(LIB_VIEW)LIB_VIEW.draw(LIB_VIEW.done);
}
function clearLibraryTags(){
  LIB_TAGS.include.clear();LIB_TAGS.exclude.clear();dialogById("library-tag-filter")?.close();
  paintLibraryTagChips();if(LIB_VIEW)LIB_VIEW.draw(LIB_VIEW.done);
}
function paintLibraryTagChips(){
  const host=document.getElementById("library-tag-chips");if(!host)return;
  const byId=new Map((CURATION.tags||[]).map(t=>[t.id,t]));
  const chip=(id,neg)=>{const t=byId.get(id);if(!t)return"";return `<button class="tag-filter-chip" aria-label="Remove filter ${esc(t.name)}" onclick="LIB_TAGS.${neg?"exclude":"include"}.delete(${id});paintLibraryTagChips();LIB_VIEW&&LIB_VIEW.draw(LIB_VIEW.done)">${neg?"not ":""}${tagChip(t," ✕")}</button>`;};
  host.innerHTML=[...LIB_TAGS.include].map(id=>chip(id,false)).join("")+[...LIB_TAGS.exclude].map(id=>chip(id,true)).join("")
    +(LIB_TAGS.include.size>1?`<span class="muted">${LIB_TAGS.mode==="all"?"all of these":"any of these"}</span>`:"");
}
function libraryPickClick(ev){
  if(!LIB_TAGS.select)return;
  const card=ev.target.closest("[data-remote-item]");if(!card)return;
  ev.preventDefault();ev.stopPropagation();
  const id=Number(card.dataset.remoteItem);
  LIB_TAGS.select.has(id)?LIB_TAGS.select.delete(id):LIB_TAGS.select.add(id);
  card.classList.toggle("tag-picked",LIB_TAGS.select.has(id));
  paintLibrarySelectBar();
}
function toggleLibrarySelect(){
  const body=document.getElementById("libbody");if(!body)return;
  if(LIB_TAGS.select){
    LIB_TAGS.select=null;body.classList.remove("tag-select-mode");
    body.removeEventListener("click",libraryPickClick,true);
    body.querySelectorAll(".tag-picked").forEach(el=>el.classList.remove("tag-picked"));
    document.getElementById("library-select-bar")?.remove();
  }else{
    LIB_TAGS.select=new Set();body.classList.add("tag-select-mode");
    body.addEventListener("click",libraryPickClick,true);
    paintLibrarySelectBar();
  }
  const b=document.getElementById("library-select-button");if(b)b.textContent=LIB_TAGS.select?"Done selecting":"Select";
}
function paintLibrarySelectBar(){
  let bar=document.getElementById("library-select-bar");
  if(!LIB_TAGS.select){bar?.remove();return;}
  if(!bar){bar=document.createElement("div");bar.id="library-select-bar";bar.className="select-bar";bar.setAttribute("role","toolbar");document.body.appendChild(bar);}
  const n=LIB_TAGS.select.size,all=LIB_TAGS.shown.length;
  bar.innerHTML=`<b>${n} selected</b>${all>n?`<button class="linklike" onclick="LIB_TAGS.shown.forEach(id=>LIB_TAGS.select.add(id));document.querySelectorAll('#libbody [data-remote-item]').forEach(c=>c.classList.add('tag-picked'));paintLibrarySelectBar()">Select all ${all} shown</button>`:""}
    <button ${n?"":"disabled"} onclick="openBulkTag()">Tag…</button><button class="ghost" onclick="LIB_TAGS.select.clear();document.querySelectorAll('#libbody .tag-picked').forEach(c=>c.classList.remove('tag-picked'));paintLibrarySelectBar()">Clear</button><button class="ghost" onclick="toggleLibrarySelect()">Done</button>`;
}
async function openBulkTag(){
  const tags=(await curationTags(true)).filter(t=>t.editable&&!t.archived);
  try{await Promise.all(tags.map(t=>tagItemIds(t.id)));}catch(e){toast(e.message);return;}
  const picked=[...LIB_TAGS.select];
  const state=t=>{const set=LIB_TAGS.sets.get(t.id);const c=picked.filter(id=>set.has(id)).length;return c===0?"none":c===picked.length?"all":"some";};
  const d=curationDialog("bulk-tag",`Tag ${picked.length} title${picked.length===1?"":"s"}`,`<ul class="tag-filter-list">${tags.map(t=>{const s=state(t);
      return `<li><label class="row" style="gap:6px"><input type="checkbox" data-tag="${t.id}" data-was="${s}" ${s==="all"?"checked":""}>${tagChip(t)}</label><span class="muted">${s==="some"?`on ${picked.filter(id=>LIB_TAGS.sets.get(t.id).has(id)).length} of ${picked.length}`:s==="all"?`on all ${picked.length}`:""}</span></li>`;}).join("")}</ul>
    <label>New tag <input id="bulk-new-tag" placeholder="Name"></label>
    <div class="row" style="gap:8px"><span style="flex:1"></span><button onclick="applyBulkTag()">Apply</button></div><p id="bulk-tag-error" role="status"></p>`);
  inputsIn(d,'input[data-was="some"]').forEach(i=>{i.indeterminate=true;});
}
async function applyBulkTag(){
  const d=dialogById("bulk-tag");if(!d)return;
  const picked=[...LIB_TAGS.select];
  try{
    const newName=(inputIn(d,"#bulk-new-tag")?.value||"").trim();
    const jobs=[];
    // The server takes at most 5000 titles a request; a large selection is
    // sent in slices.
    const send=(id,field)=>{for(let i=0;i<picked.length;i+=5000)jobs.push(api(`/tags/${id}/items`,{method:"POST",body:{[field]:picked.slice(i,i+5000)}}));};
    if(newName){const t=await api("/tags",{method:"POST",body:{name:newName,scope:"private"}});send(t.id,"add");}
    for(const box of inputsIn(d,"input[data-tag]")){
      if(box.indeterminate)continue;
      const id=Number(box.dataset.tag),was=box.dataset.was;
      if(box.checked&&was!=="all")send(id,"add");
      if(!box.checked&&was!=="none")send(id,"remove");
    }
    await Promise.all(jobs);
    LIB_TAGS.sets.clear();CURATION.at=0;
    await Promise.all([...LIB_TAGS.include,...LIB_TAGS.exclude].map(tagItemIds));
    d.close();toast("Tags updated");if(LIB_VIEW)LIB_VIEW.draw(LIB_VIEW.done);
  }catch(e){const el=d.querySelector("#bulk-tag-error");if(el)el.textContent=e.message;}
}

// ---- #/tags and #/tag/<id>[/suggested|/rejected|/settings] -------------------
async function viewTags(){
  const generation=PAGE_RENDER_GENERATION,route=location.hash;
  layoutChrome("home",`<div class="empty">Loading…</div>`);
  let tags;try{tags=await curationTags(true);}catch(e){document.getElementById("main").innerHTML=`<div class="empty">${esc(e.message)}</div>`;return;}
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route)return;
  setOrigin(route,"Tags");
  const list=tags.map(t=>`<li><a href="#/tag/${t.id}">${tagChip(t)}</a> <span class="muted">${t.item_count} title${t.item_count===1?"":"s"}${t.scope==="private"?" · only you":""}${t.archived?" · archived":""}</span></li>`).join("");
  document.getElementById("main").innerHTML=pageHead([{href:"#/",label:"Home"},{label:"Tags"}],"Tags")
    +`<div class="row" style="gap:8px;margin-bottom:12px"><input id="new-tag-name" placeholder="New tag name" style="max-width:260px">${CURATION.canManage?`<select id="new-tag-scope"><option value="private">Just me</option><option value="household">Household</option></select>`:""}<button onclick="createTagFromIndex()">Create</button></div>`
    +(list?`<ul class="tag-index">${list}</ul>`:`<div class="empty">No tags yet. Open a movie or show and use “Add tag”, or select titles in a library.</div>`);
}
async function createTagFromIndex(){
  const name=fieldValue("new-tag-name").trim();if(!name)return;
  try{const t=await api("/tags",{method:"POST",body:{name,scope:fieldValue("new-tag-scope")||"private"}});location.hash=`#/tag/${t.id}/settings`;}
  catch(e){toast(e.message);}
}
async function viewTag(id,tab="items"){
  const generation=PAGE_RENDER_GENERATION,route=location.hash;
  layoutChrome("home",`<div class="empty">Loading…</div>`);
  let tag;
  try{tag=(await curationTags(true)).find(t=>String(t.id)===String(id));}catch(e){tag=null;}
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route)return;
  const main=document.getElementById("main");
  if(!tag){main.innerHTML=`<div class="empty">That tag does not exist, or is not yours to see.</div>`;return;}
  setOrigin(route,tag.name);
  const tabs=[["items",`Tagged (${tag.item_count})`]];
  if(tag.editable)tabs.push(["suggested","Suggested"],["rejected","Rejected"],["settings","Settings"]);
  if(!tabs.some(([k])=>k===tab))tab="items";
  main.innerHTML=pageHead([{href:"#/",label:"Home"},{href:"#/tags",label:"Tags"},{label:tag.name}],tag.name)
    +`<p>${tagChip(tag)} <span class="muted">${tag.scope==="private"?"Only you see this tag.":"Everyone in the household sees this tag."}</span></p>`
    +`<nav class="tag-tabs" aria-label="Tag sections">${tabs.map(([k,l])=>`<a href="#/tag/${tag.id}${k==="items"?"":"/"+k}" ${k===tab?'aria-current="page"':""}>${l}</a>`).join("")}</nav><div id="tag-tab"><div class="empty">Loading…</div></div>`;
  const host=document.getElementById("tag-tab");
  try{
    if(tab==="items"){const r=await api(`/tags/${tag.id}/items?limit=200`);host.innerHTML=r.items.length?grid(r.items):`<div class="empty">Nothing carries this tag yet.</div>`;}
    else if(tab==="suggested")await paintSuggestions(tag,host);
    else if(tab==="rejected"){const r=await api(`/tags/${tag.id}/rejected?limit=200`);host.innerHTML=r.items.length?`<p class="muted">These never come back as suggestions unless you restore them.</p><ul class="suggestions">${r.items.map(i=>`<li>${card(i)}<button class="ghost sm" onclick="reviewSuggestion(${tag.id},{restore:[${i.id}]})">Restore</button></li>`).join("")}</ul>`:`<div class="empty">Nothing rejected.</div>`;}
    else host.innerHTML=tagSettingsHtml(tag);
  }catch(e){host.innerHTML=`<div class="empty">${esc(e.message)}</div>`;}
}
async function paintSuggestions(tag,host){
  const r=await api(`/tags/${tag.id}/suggestions?limit=120`);
  const terms=`<div class="card tag-terms"><label>Suggest titles whose TMDB keywords or labels include <input id="tag-terms" value="${esc((tag.suggest_terms||[]).join(", "))}" placeholder="gay theme, lgbt, queer"></label><button class="ghost sm" onclick="saveTagTerms(${tag.id})">Save terms</button><p class="hint">Nothing is tagged until you accept it. Broad keywords can match a single minor character — check before accepting.</p></div>`;
  if(!(tag.suggest_terms||[]).length){host.innerHTML=terms+`<div class="empty">Add a few keywords above to see suggested titles.</div>`;return;}
  host.innerHTML=terms+(r.total?`<div class="row" style="gap:8px;margin:10px 0"><b>${r.total} suggested</b><span style="flex:1"></span><button onclick="if(confirm('Tag all ${r.total} suggested titles?'))reviewSuggestion(${tag.id},{accept_all:true})">Accept all ${r.total}</button></div>
    <ul class="suggestions">${r.suggestions.map(s=>`<li>${card(s.item)}<span class="muted">matched <b>${esc(s.matched.join(", "))}</b></span><span class="row" style="gap:6px"><button class="ghost sm" onclick="reviewSuggestion(${tag.id},{reject:[${s.item.id}]})">Reject</button><button class="sm" onclick="reviewSuggestion(${tag.id},{accept:[${s.item.id}]})">Accept</button></span></li>`).join("")}</ul>`
    :`<div class="empty">No open suggestions. New titles that match will show up here.</div>`);
}
async function reviewSuggestion(tagId,body){
  try{await api(`/tags/${tagId}/suggestions`,{method:"POST",body});CURATION.at=0;render();}catch(e){toast(e.message);}
}
async function saveTagTerms(tagId){
  const terms=fieldValue("tag-terms").split(",").map(s=>s.trim()).filter(Boolean);
  try{await api(`/tags/${tagId}`,{method:"PATCH",body:{suggest_terms:terms}});CURATION.at=0;render();}catch(e){toast(e.message);}
}
function tagSettingsHtml(tag){
  const others=(CURATION.tags||[]).filter(t=>t.id!==tag.id&&t.editable&&t.scope===tag.scope);
  return `<div class="card tag-settings">
    <label>Name <input id="tag-name" maxlength="64" value="${esc(tag.name)}"></label>
    <fieldset><legend>Colour</legend><div class="row" style="gap:6px;flex-wrap:wrap">${Object.keys(TAG_SWATCH).map(c=>`<label class="swatch" style="--tag:${TAG_SWATCH[c]}"><input type="radio" name="tag-color" value="${c}" ${tag.color===c?"checked":""}><span aria-hidden="true"></span><span class="sr-only">${c}</span></label>`).join("")}</div></fieldset>
    <div class="row" style="gap:8px"><button onclick="saveTagSettings(${tag.id})">Save</button><button class="ghost" onclick="archiveTag(${tag.id},${!tag.archived})">${tag.archived?"Unarchive":"Archive"}</button></div>
    <p class="hint">Archiving hides a tag from pickers but keeps it on its titles and in its rows.</p>
    ${others.length?`<div class="row" style="gap:8px"><label>Merge into <select id="tag-merge">${others.map(t=>`<option value="${t.id}">${esc(t.name)}</option>`).join("")}</select></label><button class="ghost" onclick="mergeTag(${tag.id})">Merge</button></div>`:""}
    <div><button class="ghost danger" onclick="deleteTag(${tag.id})">Delete tag</button></div><p id="tag-settings-error" role="status"></p></div>`;
}
async function saveTagSettings(id){
  const name=fieldValue("tag-name"),color=inputIn(document,'input[name="tag-color"]:checked')?.value;
  try{await api(`/tags/${id}`,{method:"PATCH",body:{name,color}});CURATION.at=0;toast("Tag saved");render();}
  catch(e){const el=document.getElementById("tag-settings-error");if(el)el.textContent=e.message;}
}
async function archiveTag(id,archived){try{await api(`/tags/${id}`,{method:"PATCH",body:{archived}});CURATION.at=0;render();}catch(e){toast(e.message);}}
async function mergeTag(id){
  const into=Number(fieldValue("tag-merge"));if(!into||!confirm("Merge this tag into the chosen one? Its titles and rows move over and this tag is deleted."))return;
  try{await api(`/tags/${id}/merge`,{method:"POST",body:{into}});CURATION.at=0;location.hash=`#/tag/${into}`;}catch(e){toast(e.message);}
}
async function deleteTag(id){
  try{await api(`/tags/${id}`,{method:"DELETE"});CURATION.at=0;location.hash="#/tags";}
  catch(e){
    if(e.status===409&&confirm(`${e.message}\n\nDelete anyway?`)){
      try{await api(`/tags/${id}?force=1`,{method:"DELETE"});CURATION.at=0;location.hash="#/tags";}catch(err){toast(err.message);}
    }else if(e.status!==409)toast(e.message);
  }
}

// ---- Settings → Users: the manage_tags grant ---------------------------------
async function toggleManageTags(userId,grant){
  try{await api(`/users/${userId}/grants`,{method:"PUT",body:{grants:grant?["manage_tags"]:[]}});toast(grant?"Can now manage household tags":"Household tag management removed");render();}
  catch(e){toast(e.message);}
}
async function paintUserGrants(){
  for(const cell of /** @type {NodeListOf<HTMLElement>} */(document.querySelectorAll("[data-user-grants]"))){
    try{const r=await api(`/users/${cell.dataset.userGrants}/grants`);const on=(r.grants||[]).includes("manage_tags");
      cell.innerHTML=`<button class="ghost sm" onclick="toggleManageTags(${Number(cell.dataset.userGrants)},${!on})">${on?"Remove tag management":"Allow tag management"}</button>`;}
    catch(e){/* the row keeps its other controls */}
  }
}
// After the users table is painted (same task), fill each grant button.
function queueUserGrants(){queueMicrotask(paintUserGrants);return "";}
// A selection bar belongs to the grid it was opened on.
window.addEventListener("hashchange",()=>{document.getElementById("library-select-bar")?.remove();LIB_TAGS.select=null;});
// A grid redraw rebuilds the cards; selected ones keep their highlight.
function curationRepaintPicks(){
  if(!LIB_TAGS.select)return;
  document.querySelectorAll("#libbody [data-remote-item]").forEach(c=>{
    const el=/** @type {HTMLElement} */(c);
    el.classList.toggle("tag-picked",LIB_TAGS.select.has(Number(el.dataset.remoteItem)));
  });
}
