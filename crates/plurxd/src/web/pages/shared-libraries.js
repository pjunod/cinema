"use strict";
// Shared catalogue routes never enter the local numeric item/library router.
function sharedCatalogueId(value){
  if(typeof value!=="string"||! /^(?:0|[1-9][0-9]{0,18})$/.test(value)
    ||value.length===19&&value>"9223372036854775807") throw new TypeError("Invalid shared identity");
  return value;
}
function sharedCatalogueUuid(value){
  if(typeof value!=="string"||!PLAYBACK_FILE_UUID.test(value)
    ||value==="00000000-0000-0000-0000-000000000000") throw new TypeError("Invalid shared identity");
  return value;
}
function sharedCatalogueLibraryReference(value){
  if(!value||typeof value!=="object") throw new TypeError("Invalid shared reference");
  return Object.freeze({import_id:sharedCatalogueUuid(value.import_id),server_id:sharedCatalogueUuid(value.server_id),
    catalogue_epoch:sharedCatalogueUuid(value.catalogue_epoch),library_id:sharedCatalogueId(value.library_id)});
}
function sharedCatalogueReference(value){
  return Object.freeze({...sharedCatalogueLibraryReference(value),item_id:sharedCatalogueId(value.item_id)});
}
function sharedCatalogueGroupKey(value){
  return JSON.stringify([sharedCatalogueUuid(value.import_id),sharedCatalogueUuid(value.server_id),sharedCatalogueUuid(value.catalogue_epoch)]);
}
function sharedCatalogueHref(value,kind="item"){
  if(!["item","library"].includes(kind)) throw new TypeError("Invalid shared route");
  const ref=sharedCatalogueLibraryReference(value);
  return `#/shared/${kind}/${ref.import_id}/${ref.server_id}/${ref.catalogue_epoch}/${ref.library_id}`+(kind==="item"?`/${sharedCatalogueId(value.item_id)}`:"");
}
/** @returns {{kind:"index"}|{kind:"library",q:string,reference:ReturnType<typeof sharedCatalogueLibraryReference>}|{kind:"item",q:string,reference:ReturnType<typeof sharedCatalogueReference>}} */
function sharedCatalogueRoute(hash){
  const parts=hash.split("?");if(parts.length>2) throw new TypeError("Invalid shared route");
  const params=new URLSearchParams(parts[1]||"");
  if(Array.from(params.keys()).some(key=>key!=="q")||params.getAll("q").length>1) throw new TypeError("Invalid shared query");
  const q=params.get("q")||"";
  if(q.length>512||/[\u0000-\u001f\u007f]/.test(q)) throw new TypeError("Invalid shared query");
  hash=parts[0];
  if(hash==="#/shared"){if(q) throw new TypeError("Invalid shared query");return {kind:"index"};}
  const match=/^#\/shared\/(item|library)\/([^/]+)\/([^/]+)\/([^/]+)\/([^/]+)(?:\/([^/]+))?$/.exec(hash);
  if(!match||match[1]==="item"&&!match[6]||match[1]==="library"&&match[6]) throw new TypeError("Invalid shared route");
  if(match[1]==="item"&&q) throw new TypeError("Invalid shared query");
  const library=sharedCatalogueLibraryReference({import_id:match[2],server_id:match[3],catalogue_epoch:match[4],library_id:match[5]});
  if(match[1]==="item") return {kind:"item",q,reference:sharedCatalogueReference({...library,item_id:match[6]})};
  return {kind:"library",q,reference:library};
}
function sharedCatalogueCurrent(capture){
  return capture.generation===PAGE_RENDER_GENERATION&&capture.auth===AUTH_GENERATION&&capture.token===TOKEN&&capture.origin===API&&capture.route===location.hash;
}
async function sharedCatalogueRead(path,capture){return SHARED_ARTWORK.metadata(path,capture);}
function sharedCatalogueItemHtml(item,expected,capture=null){
  if(!item||item.source!=="shared") throw new TypeError("Invalid shared item");
  const ref=sharedCatalogueReference(item.reference);
  if(sharedCatalogueGroupKey(ref)!==sharedCatalogueGroupKey(expected)
    ||expected.library_id&&ref.library_id!==expected.library_id) throw new TypeError("Shared source changed");
  if(typeof item.title!=="string"||item.title.length>16384) throw new TypeError("Invalid shared item");
  return `<a class="card" href="${esc(sharedCatalogueHref(ref))}">${SHARED_ARTWORK.markup(item,expected,capture)}<strong>${esc(item.title)}</strong><div class="muted">${esc(item.kind||"")}${item.year?` · ${esc(String(item.year))}`:""}</div></a>`;
}
function sharedCatalogueError(error){return `<div class="empty" role="status">Shared source unavailable. ${esc(error.message||"Try again.")} <button class="ghost sm" onclick="render()">Retry</button></div>`;}
async function viewSharedCatalogue(generation=++PAGE_RENDER_GENERATION){
  const capture={generation,auth:AUTH_GENERATION,token:TOKEN,origin:API,route:location.hash};
  const parsed=sharedCatalogueRoute(capture.route);
  layoutChrome("shared",`<h1>Shared libraries</h1><p><a href="#/shared">All shared sources</a></p><div id="shared-catalogue"><div class="empty">Loading…</div></div>`);
  setPagePhase(capture.route,generation,"shell");
  const paint=html=>{if(!sharedCatalogueCurrent(capture)) return false;const el=document.getElementById("shared-catalogue");if(!el) return false;el.innerHTML=html;SHARED_ARTWORK.hydrate(el);setPagePhase(capture.route,generation,"content");return true;};
  try{
    if(parsed.kind==="index"){
      const results=await Promise.allSettled([sharedCatalogueRead("/shared/libraries",capture),sharedCatalogueRead("/shared/continue-watching?limit=200",capture)]);
      if(results[0].status!=="fulfilled") throw results[0].reason;
      const assigned=results[0].value,history=new Map();
      if(results[1].status==="fulfilled"){try{const groups=results[1].value.groups;
        if(!Array.isArray(groups)||groups.length>32) throw new Error("Shared history unavailable");
        for(const group of groups){const key=sharedCatalogueGroupKey(group);if(history.has(key)) throw new Error("Shared history changed");history.set(key,group);}
      }catch(error){history.clear();}}
      if(!Array.isArray(assigned.libraries)||assigned.libraries.length>2048) throw new Error("Shared assignments unavailable");
      const groups=new Map();
      for(const row of assigned.libraries){const key=sharedCatalogueGroupKey(row);sharedCatalogueId(row.library_id);
        if(!groups.has(key)) groups.set(key,{...row,assigned:new Set()});groups.get(key).assigned.add(row.library_id);if(groups.get(key).assigned.size>64) throw new Error("Shared assignments unavailable");}
      if(groups.size>32) throw new Error("Shared assignments unavailable");
      const entries=Array.from(groups.values());
      paint(entries.length?entries.map((group,index)=>`<section class="card"><h2>${esc(group.source_name||"Shared source")}</h2><div id="shared-source-${index}">Loading source…</div><div id="shared-continue-${index}"></div></section>`).join(""):"<div class=\"empty\">No shared libraries assigned.</div>");
      // Each Source paints independently; one unreachable home cannot delay another.
      let nextGroup=0;
      const loadGroup=async(group,index)=>{
        let html;
        try{const reply=await sharedCatalogueRead(`/shared/imports/${group.import_id}/libraries`,capture);
          if(sharedCatalogueGroupKey(reply)!==sharedCatalogueGroupKey(group)||!Array.isArray(reply.libraries)||reply.libraries.length>64) throw new Error("Shared source changed");
          html=reply.libraries.filter(lib=>group.assigned.has(sharedCatalogueId(lib.library_id))).map(lib=>`<p><a href="${esc(sharedCatalogueHref({...group,library_id:lib.library_id},"library"))}">${esc(lib.name||"Shared library")}</a></p>`).join("")||"No currently available libraries.";
        }catch(error){html=sharedCatalogueError(error);}
        if(sharedCatalogueCurrent(capture)){const el=document.getElementById(`shared-source-${index}`);if(el) el.innerHTML=html;}
        if(history.has(sharedCatalogueGroupKey(group))){
          let recent;
          try{const reply=await sharedCatalogueRead(`/shared/imports/${group.import_id}/continue-watching?limit=200`,capture);
            if(sharedCatalogueGroupKey(reply)!==sharedCatalogueGroupKey(group)||!Array.isArray(reply.items)||reply.items.length>200) throw new Error("Shared history changed");
            if(!["online","unavailable","busy"].includes(reply.availability)||reply.availability!=="online"&&reply.items.length) throw new Error("Shared history unavailable");
            recent=reply.availability==="online"?reply.items.map(entry=>{const r=sharedCatalogueReference(entry.item?.reference);if(!group.assigned.has(r.library_id))throw new Error("Shared history assignment changed");return sharedCatalogueItemHtml(entry.item,{import_id:group.import_id,server_id:group.server_id,catalogue_epoch:group.catalogue_epoch},capture);}).join(""):'<p class="muted">Continue Watching unavailable for this Source.</p>';
          }catch(error){recent=sharedCatalogueError(error);}
          if(sharedCatalogueCurrent(capture)){const el=document.getElementById(`shared-continue-${index}`);if(el){el.innerHTML=`<h3>Continue Watching</h3>${recent}`;SHARED_ARTWORK.hydrate(el);}}
        }
      };
      await Promise.allSettled(Array.from({length:Math.min(4,entries.length)},async()=>{
        while(nextGroup<entries.length&&sharedCatalogueCurrent(capture)){const index=nextGroup++;await loadGroup(entries[index],index);}
      }));
    }else{
      const base=`/shared/imports/${parsed.reference.import_id}`;
      if(parsed.kind==="item"){
        const ref=parsed.reference;
        const detail=await sharedCatalogueRead(`${base}/items/${ref.item_id}`,capture);
        const item=detail.item,current=sharedCatalogueReference(item?.reference);
        if(sharedCatalogueGroupKey(current)!==sharedCatalogueGroupKey(ref)||current.library_id!==ref.library_id||current.item_id!==ref.item_id) throw new Error("Shared source changed");
        if(!Array.isArray(detail.files)||detail.files.length>64) throw new Error("Shared details unavailable");
        const launch=detail.delivery_status==="available";
        const watchable=item.kind==="movie"||item.kind==="episode",watched=!!detail.watch?.watched;
        paint(`${SHARED_ARTWORK.markup(item,ref,capture,true)}<h2>${esc(item.title||"")}</h2><p>${esc(item.overview||"")}</p>${launch?"":'<p class="muted">Playback is not available for this shared item yet.</p>'}${watchable?`<p><button class="ghost sm" data-shared-watched="${watched?0:1}">${watched?"Mark unwatched":"Mark watched"}</button></p>`:""}<div>${detail.files.map((file,index)=>{
          sharedCatalogueId(file.file_id);
          const fileRef=sharedCatalogueReference(file.reference?.item);
          if(JSON.stringify(fileRef)!==JSON.stringify(current)||file.reference.file_id!==file.file_id) throw new Error("Shared file changed");
          return `<p>${esc(file.video_codec||file.container||"Media file")}${file.duration_ms?` · ${esc(fmtDur(file.duration_ms/1000))}`:""} <button data-shared-play="${index}"${launch?"":" disabled"}>Play</button></p>`;
        }).join("")}</div><div id="shared-children"></div>`);
        const mount=document.getElementById("shared-catalogue");
        const watchButton=mount?.querySelector("button[data-shared-watched]");
        if(watchButton instanceof HTMLButtonElement)watchButton.onclick=async()=>{
          if(!sharedCatalogueCurrent(capture))return;watchButton.disabled=true;
          try{await sharedCatalogueSetWatched(ref,watchButton.dataset.sharedWatched==="1");if(sharedCatalogueCurrent(capture))void render();}
          catch(error){if(sharedCatalogueCurrent(capture)){watchButton.disabled=false;watchButton.title=error.message||"Shared watch state unavailable";}}
        };
        if(mount)for(const button of mount.querySelectorAll("button[data-shared-play]")){
          if(!(button instanceof HTMLButtonElement))continue;
          const index=Number(button.dataset.sharedPlay),file=detail.files[index];
          if(!file||!Number.isInteger(index)||index<0||index>=detail.files.length)continue;
          button.onclick=async()=>{if(!launch||!sharedCatalogueCurrent(capture))return;button.disabled=true;
            try{await sharedCataloguePlay(ref,file.file_id,capture);}catch(error){if(sharedCatalogueCurrent(capture))button.title=error.message||"Shared playback unavailable";}
            finally{if(sharedCatalogueCurrent(capture))button.disabled=!launch;}
          };
        }
        // Children are other items of the same library: bind the page to the
        // library reference, not to this parent's item identity.
        await sharedCatalogueLoadPage(`${base}/items/${ref.item_id}/children`,sharedCatalogueLibraryReference(ref),capture,"shared-children");
      }else{
        const ref=parsed.reference;
        paint(`<form id="shared-search"><label>Search this library <input name="q" maxlength="512" value="${esc(parsed.q)}"></label><button class="ghost" type="submit">Search</button></form><div id="shared-items"></div>`);
        const form=/** @type {HTMLFormElement|null} */(document.getElementById("shared-search"));
        if(form) form.onsubmit=event=>{event.preventDefault();if(!sharedCatalogueCurrent(capture)) return;
          const value=String(new FormData(form).get("q")||"");
          const href=sharedCatalogueHref(ref,"library")+(value?`?q=${encodeURIComponent(value)}`:"");
          if(location.hash===href) void render();else location.hash=href;
        };
        await sharedCatalogueLoadPage(`${base}/libraries/${ref.library_id}/items`,ref,capture,"shared-items",parsed.q);
      }
    }
  }catch(error){paint(sharedCatalogueError(error));}
  if(sharedCatalogueCurrent(capture)) setPagePhase(capture.route,generation,"settled");
}
async function sharedCatalogueLoadPage(path,ref,capture,mount,q=""){
  let cursor=null,loading=false,seen=new Set(),seenItems=new Set();
  const load=async()=>{
    if(loading||!sharedCatalogueCurrent(capture)) return;loading=true;
    const el=document.getElementById(mount);if(!el){loading=false;return;}
    let reopen=false;
    const prior=/** @type {HTMLButtonElement|null} */(el.querySelector("button[data-shared-more]"));if(prior) prior.disabled=true;
    try{
      const page=await sharedCatalogueRead(path+"?limit=100"+(q?`&q=${encodeURIComponent(q)}`:"")+(cursor?`&cursor=${encodeURIComponent(cursor)}`:""),capture);
      if(!Array.isArray(page.items)||page.items.length>100) throw new Error("Shared page unavailable");
      const next=page.next_cursor;
      if(next!==null&&next!==undefined&&(typeof next!=="string"||next.length>4096||!next||seen.has(next))) throw new Error("Shared cursor unavailable");
      const accepted=[],pageKeys=new Set();
      for(const item of page.items){const r=sharedCatalogueReference(item.reference),key=JSON.stringify([r.import_id,r.server_id,r.catalogue_epoch,r.library_id,r.item_id]);
        if(!seenItems.has(key)&&!pageKeys.has(key)){pageKeys.add(key);if(seenItems.size+accepted.length>=5000) throw new Error("Search this library to narrow the results.");accepted.push([key,sharedCatalogueItemHtml(item,ref,capture)]);}
      }
      const html=accepted.map(row=>row[1]).join("");
      if(!sharedCatalogueCurrent(capture)) return;
      for(const row of accepted) seenItems.add(row[0]);
      if(prior) prior.remove();el.insertAdjacentHTML("beforeend",html||(!cursor?'<div class="empty">No items.</div>':""));SHARED_ARTWORK.hydrate(el);
      cursor=next||null;
      if(cursor&&seenItems.size>=5000){el.insertAdjacentHTML("beforeend",'<p class="muted">Search this library to narrow the results.</p>');}
      else if(cursor){seen.add(cursor);const button=document.createElement("button");button.className="ghost";button.dataset.sharedMore="true";button.textContent="Load more";button.onclick=load;el.appendChild(button);}
    }catch(error){if(sharedCatalogueCurrent(capture)){
      if(cursor&&SHARED_CATALOGUE_REOPEN.includes(error?.code)) reopen=true;
      else if(prior) prior.disabled=false;else el.innerHTML=sharedCatalogueError(error);}}
    finally{loading=false;}
    // An expired, substituted or refused Source cursor is a typed fresh open:
    // retrying the same cursor can never succeed. One open per request, from
    // the first page, so this cannot become a restart loop.
    if(reopen&&sharedCatalogueCurrent(capture)){
      cursor=null;seen=new Set();seenItems=new Set();
      el.innerHTML='<p class="muted" role="status">This list changed or its place expired, so it was reopened from the start.</p>';
      await load();
    }
  };
  await load();
}
const SHARED_CATALOGUE_REOPEN=Object.freeze(["sharing_cursor_expired","sharing_query_changed","sharing_cursor_invalid"]);

// Fresh B details mint the opaque context. Displayed cached file facts never
// become a Play authority, and Source numbers never enter the Local router.
async function sharedCataloguePlay(reference,fileId,capture){
  return sharedCatalogueLaunch(reference,sharedCatalogueId(fileId),()=>sharedCatalogueCurrent(capture));
}
// A null file selects the first file of the fresh details (next episode).
async function sharedCatalogueLaunch(reference,fileId,stillCurrent){
  const ref=sharedCatalogueReference(reference),id=fileId===null?null:sharedCatalogueId(fileId);
  if(!stillCurrent())throw new Error("Shared page changed.");
  const fresh=await SHARED_DECISION.details(ref);
  if(!stillCurrent()||fresh.detail.delivery_status!=="available")throw new Error("Shared playback is not available yet.");
  const selected=id===null?fresh.files[0]:fresh.files.find(entry=>entry.context.source_file_id===id);
  if(!selected)throw new Error("Shared file changed.");
  const watch=fresh.detail.watch;
  const resume=watch&&!watch.watched?watch.position_ms:0,duration=selected.file.duration_ms??0;
  if(!Number.isSafeInteger(resume)||resume<0||!Number.isSafeInteger(duration)||duration<0)throw new Error("Shared timeline unavailable.");
  return play(selected.context.source_file_id,fresh.detail.item.title||"Shared item",resume,duration,
    {...fresh.detail.item,fileContext:selected.context,sharedReference:ref});
}
// B-private explicit watched state; the server takes the next history
// sequence, so a beat sent before this click cannot restore the old position.
async function sharedCatalogueSetWatched(reference,watched){
  const ref=sharedCatalogueReference(reference);
  if(typeof watched!=="boolean")throw new TypeError("Invalid shared watched state");
  return api(`/shared/imports/${ref.import_id}/items/${ref.item_id}/watched`,{method:"POST",body:{watched}});
}
// Next episode follows Source hierarchy and order through B: next in the
// season, else the first episode of the next season. It returns a full Shared
// reference for a new authorized start and never derives a Local item ID.
async function sharedCatalogueNextEpisode(reference,read){
  const ref=sharedCatalogueReference(reference),base=`/shared/imports/${ref.import_id}`,group=sharedCatalogueGroupKey(ref);
  const owned=value=>{const r=sharedCatalogueReference(value);if(sharedCatalogueGroupKey(r)!==group)throw new Error("Shared source changed");return r;};
  const detail=async id=>{const item=(await read(`${base}/items/${sharedCatalogueId(id)}`))?.item;
    if(!item||owned(item.reference).item_id!==id)throw new Error("Shared source changed");return item;};
  const children=async(id,kind)=>{
    const rows=[],ids=new Set();let cursor=null;
    for(let page=0;page<10;page++){
      const reply=await read(`${base}/items/${sharedCatalogueId(id)}/children?limit=200`+(cursor?`&cursor=${encodeURIComponent(cursor)}`:""));
      if(!Array.isArray(reply?.items)||reply.items.length>200)throw new Error("Shared children unavailable");
      for(const row of reply.items){const r=owned(row.reference);if(row.kind===kind&&!ids.has(r.item_id)){ids.add(r.item_id);rows.push(r);}}
      cursor=reply.next_cursor||null;if(!cursor)return rows;
    }
    throw new Error("Shared season unavailable");
  };
  const current=await detail(ref.item_id);
  if(current.kind!=="episode"||!current.parent)return null;
  const season=owned(current.parent),episodes=await children(season.item_id,"episode");
  const at=episodes.findIndex(row=>row.item_id===ref.item_id);
  if(at>=0&&episodes[at+1])return episodes[at+1];
  const seasonItem=await detail(season.item_id);
  if(!seasonItem.parent)return null;
  const seasons=await children(owned(seasonItem.parent).item_id,"season");
  const index=seasons.findIndex(row=>row.item_id===season.item_id),next=index<0?null:seasons[index+1];
  if(!next)return null;
  return (await children(next.item_id,"episode"))[0]||null;
}
