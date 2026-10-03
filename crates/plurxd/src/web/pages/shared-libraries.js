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
async function sharedCatalogueRead(path,capture){
  if(!sharedCatalogueCurrent(capture)) throw new Error("stale shared catalogue");
  const response=await api(path,{raw:true});
  if(!sharedCatalogueCurrent(capture)){await response.body?.cancel();throw new Error("stale shared catalogue");}
  const reader=response.body?.getReader();
  if(!reader) throw new Error("Shared catalogue body unavailable");
  const chunks=[];let length=0;
  try{
    for(;;){const part=await reader.read();if(part.done) break;
      length+=part.value.byteLength;
      if(length>4*1024*1024||!sharedCatalogueCurrent(capture)) throw new Error("Shared catalogue response unavailable");
      chunks.push(part.value);
    }
  }catch(error){await reader.cancel().catch(()=>{});throw error;}finally{reader.releaseLock();}
  if(!sharedCatalogueCurrent(capture)) throw new Error("stale shared catalogue");
  const bytes=new Uint8Array(length);let offset=0;
  for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.byteLength;}
  return JSON.parse(new TextDecoder("utf-8",{fatal:true}).decode(bytes));
}
function sharedCatalogueItemHtml(item,expected){
  if(!item||item.source!=="shared") throw new TypeError("Invalid shared item");
  const ref=sharedCatalogueReference(item.reference);
  if(sharedCatalogueGroupKey(ref)!==sharedCatalogueGroupKey(expected)
    ||expected.library_id&&ref.library_id!==expected.library_id) throw new TypeError("Shared source changed");
  if(typeof item.title!=="string"||item.title.length>16384) throw new TypeError("Invalid shared item");
  return `<a class="card" href="${esc(sharedCatalogueHref(ref))}"><strong>${esc(item.title)}</strong><div class="muted">${esc(item.kind||"")}${item.year?` · ${esc(String(item.year))}`:""}</div></a>`;
}
function sharedCatalogueError(error){return `<div class="empty" role="status">Shared source unavailable. ${esc(error.message||"Try again.")} <button class="ghost sm" onclick="render()">Retry</button></div>`;}
async function viewSharedCatalogue(generation=++PAGE_RENDER_GENERATION){
  const capture={generation,auth:AUTH_GENERATION,token:TOKEN,origin:API,route:location.hash};
  const parsed=sharedCatalogueRoute(capture.route);
  layoutChrome("shared",`<h1>Shared libraries</h1><p><a href="#/shared">All shared sources</a></p><div id="shared-catalogue"><div class="empty">Loading…</div></div>`);
  setPagePhase(capture.route,generation,"shell");
  const paint=html=>{if(!sharedCatalogueCurrent(capture)) return false;const el=document.getElementById("shared-catalogue");if(!el) return false;el.innerHTML=html;setPagePhase(capture.route,generation,"content");return true;};
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
            recent=reply.availability==="online"?reply.items.map(entry=>sharedCatalogueItemHtml(entry.item,group)).join(""):'<p class="muted">Continue Watching unavailable for this Source.</p>';
          }catch(error){recent=sharedCatalogueError(error);}
          if(sharedCatalogueCurrent(capture)){const el=document.getElementById(`shared-continue-${index}`);if(el)el.innerHTML=`<h3>Continue Watching</h3>${recent}`;}
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
        paint(`<h2>${esc(item.title||"")}</h2><p>${esc(item.overview||"")}</p><p class="muted">Playback is not available for this shared item yet.</p><div>${detail.files.map(file=>{
          sharedCatalogueId(file.file_id);
          const fileRef=sharedCatalogueReference(file.reference?.item);
          if(JSON.stringify(fileRef)!==JSON.stringify(current)||file.reference.file_id!==file.file_id) throw new Error("Shared file changed");
          return `<p>${esc(file.video_codec||file.container||"Media file")}${file.duration_ms?` · ${esc(fmtDur(file.duration_ms/1000))}`:""}</p>`;
        }).join("")}</div><div id="shared-children"></div>`);
        await sharedCatalogueLoadPage(`${base}/items/${ref.item_id}/children`,ref,capture,"shared-children");
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
    const prior=/** @type {HTMLButtonElement|null} */(el.querySelector("button[data-shared-more]"));if(prior) prior.disabled=true;
    try{
      const page=await sharedCatalogueRead(path+"?limit=100"+(q?`&q=${encodeURIComponent(q)}`:"")+(cursor?`&cursor=${encodeURIComponent(cursor)}`:""),capture);
      if(!Array.isArray(page.items)||page.items.length>100) throw new Error("Shared page unavailable");
      const next=page.next_cursor;
      if(next!==null&&next!==undefined&&(typeof next!=="string"||next.length>4096||!next||seen.has(next))) throw new Error("Shared cursor unavailable");
      const accepted=[],pageKeys=new Set();
      for(const item of page.items){const html=sharedCatalogueItemHtml(item,ref),r=sharedCatalogueReference(item.reference),key=JSON.stringify([r.import_id,r.server_id,r.catalogue_epoch,r.library_id,r.item_id]);
        if(!seenItems.has(key)&&!pageKeys.has(key)){pageKeys.add(key);if(seenItems.size+accepted.length>=5000) throw new Error("Search this library to narrow the results.");accepted.push([key,html]);}
      }
      const html=accepted.map(row=>row[1]).join("");
      if(!sharedCatalogueCurrent(capture)) return;
      for(const row of accepted) seenItems.add(row[0]);
      if(prior) prior.remove();el.insertAdjacentHTML("beforeend",html||(!cursor?'<div class="empty">No items.</div>':""));
      cursor=next||null;
      if(cursor&&seenItems.size>=5000){el.insertAdjacentHTML("beforeend",'<p class="muted">Search this library to narrow the results.</p>');}
      else if(cursor){seen.add(cursor);const button=document.createElement("button");button.className="ghost";button.dataset.sharedMore="true";button.textContent="Load more";button.onclick=load;el.appendChild(button);}
    }catch(error){if(sharedCatalogueCurrent(capture)){if(prior) prior.disabled=false;else el.innerHTML=sharedCatalogueError(error);}}
    finally{loading=false;}
  };
  await load();
}

function sharingManagementRead(path){
  return sharedCatalogueRead(path,{generation:PAGE_RENDER_GENERATION,auth:AUTH_GENERATION,token:TOKEN,origin:API,route:location.hash});
}
function sharingManagementPanel(data){
  const imports=data.sharingImports?.imports,exports=data.sharingExports?.exports;
  if(!Array.isArray(imports)||imports.length>32||!Array.isArray(exports)||exports.length>32) throw new Error("Sharing management unavailable");
  return setHead("Sharing","Share selected libraries with another Cinema.","")+setCard(`${cardHead("Connected Cinemas","Each recipient has its own approved scope.","")}<h3>Imports</h3>${imports.map(row=>`<p>${esc(row.import?.source_name||"Shared source")} · ${esc(row.import?.state||"unavailable")}</p>`).join("")||'<p class="muted">No imports.</p>'}<h3>Exports</h3>${exports.map(row=>`<p>${esc(row.recipient_name||"Recipient")} · ${esc(row.grant?.state||"unavailable")}</p>`).join("")||'<p class="muted">No exports.</p>'}${data.sharingExports.next?'<p class="muted">More exports are available.</p>':""}<p><a href="#/settings/developer/cinema-sharing-settings">Enable shared libraries in Developer</a></p>`)+setCard(`${cardHead("This node","Readiness is advisory. The saved switch remains available in Developer.","")}<p>${esc(data.sharingStatus?.listener||"Readiness unavailable")}</p>`);
}
