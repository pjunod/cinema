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
function sharedCatalogueItemHtml(item,expected,capture=null,sourceName=""){
  if(!item||item.source!=="shared") throw new TypeError("Invalid shared item");
  const ref=sharedCatalogueReference(item.reference);
  if(sharedCatalogueGroupKey(ref)!==sharedCatalogueGroupKey(expected)
    ||expected.library_id&&ref.library_id!==expected.library_id) throw new TypeError("Shared source changed");
  if(typeof item.title!=="string"||item.title.length>16384) throw new TypeError("Invalid shared item");
  const art=SHARED_ARTWORK.markup(item,expected,capture);
  return `<a class="poster shared-poster" href="${esc(sharedCatalogueHref(ref))}" title="${esc(item.title)}"><div class="artbox"><div class="art">${art||'<span class="shared-art-placeholder" aria-hidden="true">◇</span>'}</div></div><div class="meta"><strong class="t">${esc(item.title)}</strong><div class="s"><span class="stxt">${esc(item.kind||"")}${item.year?` · ${esc(String(item.year))}`:""}</span></div>${sourceName?`<div class="shared-card-source" title="${esc(sourceName)}">${esc(sourceName)}</div>`:""}</div></a>`;
}
function sharedCatalogueError(error){return `<div class="empty" role="status">Shared source unavailable. ${esc(error.message||"Try again.")} <button class="ghost sm" onclick="render()">Retry</button></div>`;}
function sharedCatalogueSourceName(group){return typeof group.source_name==="string"&&group.source_name.trim()?group.source_name:"Shared source";}
function sharedCatalogueInitials(name){return name.trim().split(/\s+/).slice(0,2).map(part=>Array.from(part)[0]).join("").toLocaleUpperCase();}
function sharedCatalogueSearchText(value){return String(value||"").normalize("NFKD").replace(/[\u0300-\u036f]/g,"").toLocaleLowerCase();}
function sharedCatalogueHistoryMoreHtml(group){const name=sharedCatalogueSourceName(group);return `<button type="button" class="ghost shared-history-more" data-shared-history-more aria-label="More titles from ${esc(name)}"><span aria-hidden="true">＋</span><strong>More titles</strong><span class="shared-card-source">${esc(name)}</span></button>`;}
function sharedCatalogueLibraryHtml(lib,group){
  const name=typeof lib.name==="string"&&lib.name?lib.name:"Shared library",tv=lib.kind==="shows";
  const icon=tv?'<rect x="3" y="5" width="18" height="14" rx="3"/><path d="m9 2 3 3 3-3M8 22h8"/>':'<rect x="4" y="3" width="16" height="18" rx="2"/><path d="M8 3v18M16 3v18M4 8h4m8 0h4M4 16h4m8 0h4"/>';
  return `<a class="shared-library-link" href="${esc(sharedCatalogueHref({...group,library_id:lib.library_id},"library"))}" title="${esc(name)}"><span class="shared-library-symbol" aria-hidden="true"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5">${icon}</svg></span><span>${esc(name)}</span><span class="shared-library-arrow" aria-hidden="true">›</span></a>`;
}
function sharedCatalogueSourceHtml(group,index,admin){
  const name=sharedCatalogueSourceName(group),count=group.assigned.size,tone=parseInt(group.import_id.slice(0,8),16)%6;
  return `<article class="shared-source" id="shared-source-card-${index}" aria-labelledby="shared-source-title-${index}"><header class="shared-source-head"><span class="shared-source-mark shared-tone-${tone}" aria-hidden="true">${esc(sharedCatalogueInitials(name))}</span><div class="shared-source-identity"><h3 id="shared-source-title-${index}" title="${esc(name)}">${esc(name)}</h3><p>${count} ${count===1?"library":"libraries"} shared with you</p></div>${admin?`<button type="button" class="ghost sm shared-rename" data-shared-rename="${index}" aria-label="Rename ${esc(name)}">Rename</button>`:""}</header><div id="shared-source-${index}" class="shared-library-links"><p class="shared-source-loading">Loading libraries…</p></div><footer class="shared-source-foot"><span id="shared-source-status-${index}" class="shared-source-status" role="status">Checking share…</span><p id="shared-history-status-${index}" class="shared-history-status"></p></footer></article>`;
}
function sharedCatalogueHubHtml(entries,admin){
  const libraries=entries.reduce((total,group)=>total+group.assigned.size,0);
  if(!entries.length)return `<div class="shared-hub-empty"><span aria-hidden="true">◇</span><h2>Your next great library could be a shared one.</h2><p>${admin?"Import an invitation to bring someone’s library into your Cinema.":"Libraries shared with your household will appear here once you have access."}</p>${admin?'<a class="shared-import-link" href="#/settings/sharing">Import a share <span aria-hidden="true">↗</span></a>':""}</div>`;
  return `<div class="shared-hub"><div class="shared-hub-summary"><span><strong>${entries.length}</strong> ${entries.length===1?"share":"shares"}</span><span><strong>${libraries}</strong> ${libraries===1?"library":"libraries"}</span></div><section id="shared-continue-section" class="shared-continue-section" hidden><div class="shared-section-heading"><div><p class="shared-eyebrow">PICK UP WHERE YOU LEFT OFF</p><h2>Continue Watching</h2></div></div><div class="railbox"><button class="railarrow l" aria-label="Scroll shared titles left" onclick="railScroll(this,-1)">‹</button><div class="rowscroll shared-continue-rail" data-shelf-key="Shared Continue Watching" data-shelf-kind="shared">${entries.map((group,index)=>`<div id="shared-continue-${index}" class="shared-continue-group"></div>`).join("")}</div><button class="railarrow r" aria-label="Scroll shared titles right" onclick="railScroll(this,1)">›</button></div></section><section class="shared-directory" aria-labelledby="shared-directory-title"><div class="shared-directory-toolbar"><div><p class="shared-eyebrow">FROM THE PEOPLE YOU KNOW</p><h2 id="shared-directory-title">Your shares</h2><p id="shared-source-count" class="shared-source-count" aria-live="polite">${entries.length} ${entries.length===1?"share":"shares"}</p></div><div class="shared-directory-tools"><label class="shared-source-search"><span>Find a share or library</span><input id="shared-source-search" type="search" placeholder="Search names or libraries…" maxlength="256" autocomplete="off"></label><label class="shared-source-availability"><span>Show</span><select id="shared-source-availability"><option value="all">All shares</option><option value="ready">Available</option><option value="unavailable">Needs attention</option></select></label></div></div><p id="shared-hub-feedback" class="shared-hub-feedback" role="status"></p><div class="shared-sources-grid${entries.length===1?" shared-sources-single":""}">${entries.map((group,index)=>sharedCatalogueSourceHtml(group,index,admin)).join("")}</div><div id="shared-no-matches" class="shared-no-matches" hidden><h3>No matching shares</h3><p>Try another name or choose All shares.</p></div></section></div>`;
}
function sharedCatalogueHubRefresh(entries,capture){
  if(!sharedCatalogueCurrent(capture))return;
  const query=sharedCatalogueSearchText((/** @type {HTMLInputElement|null} */(document.getElementById("shared-source-search")))?.value),availability=(/** @type {HTMLSelectElement|null} */(document.getElementById("shared-source-availability")))?.value||"all";
  let visible=0,recent=0;
  for(const [index,group]of entries.entries()){
    const search=[sharedCatalogueSourceName(group),...(group.libraries||[]).map(lib=>lib.name)].join(" ");
    const match=sharedCatalogueSearchText(search).includes(query)&&(availability==="all"||availability===group.hubState);
    const card=document.getElementById(`shared-source-card-${index}`),continuing=document.getElementById(`shared-continue-${index}`);
    if(card)card.hidden=!match;if(continuing)continuing.hidden=!match;
    if(match){visible++;recent+=group.historyCount||0;}
  }
  const count=document.getElementById("shared-source-count"),empty=document.getElementById("shared-no-matches"),section=document.getElementById("shared-continue-section");
  if(count)count.textContent=visible===entries.length?`${visible} ${visible===1?"share":"shares"}`:`${visible} of ${entries.length} shares`;
  if(empty)empty.hidden=visible!==0;if(section)section.hidden=recent===0;
  if(typeof wireRails==="function")wireRails();
  const box=section?.querySelector(".railbox");if(box&&typeof railSync==="function")railSync(box);
}
function sharedCatalogueFileHtml(file,index,launch){
  const video=[file.video_codec&&codecLabel(file.video_codec),file.width&&file.height?`${file.width}×${file.height}`:"",file.hdr_format||hdrLabel(file.hdr),file.bit_depth?`${file.bit_depth}-bit`:"",fmtMbps(file.bitrate)].filter(Boolean).join(" · ");
  const facts=[file.container&&file.container.toUpperCase(),fmtSize(Number(file.size)),file.duration_ms?fmtDur(file.duration_ms):""].filter(Boolean).join(" · ");
  const audio=(file.audio_streams||[]).map(a=>`<span class="trk">${esc(audioFactLabel(a))}</span>`).join("");
  const subs=(file.subtitle_streams||[]).map(s=>`<span class="trk">${esc(subFactLabel(s))}</span>`).join("");
  return `<section class="version shared-version"><div class="vh"><h3>Version ${index+1}</h3><button data-shared-play="${index}"${launch?"":" disabled"}>Play</button></div><div class="vbadges">${specBadges(file)}</div><dl class="specs"><dt>Video</dt><dd>${esc(video||"—")}</dd><dt>Audio</dt><dd><div class="trks">${audio||"None"}</div></dd><dt>Subtitles</dt><dd><div class="trks">${subs||"None"}</div></dd><dt>File</dt><dd>${esc(facts||"—")}</dd></dl></section>`;
}
function sharedCatalogueDetailHtml(detail,ref,capture){
  const item=detail.item,launch=detail.delivery_status==="available",watched=!!detail.watch?.watched;
  const presentation=item.presentation||{},runtime=presentation.runtime_ms||detail.files.find(f=>f.duration_ms)?.duration_ms;
  const chips=[item.year,item.kind==="episode"&&item.season_number!=null?`Season ${item.season_number}`:"",item.episode_number!=null?`Episode ${item.episode_number}`:"",runtime?fmtDur(runtime):"",...(item.genres||[])].filter(v=>v!==null&&v!==undefined&&v!=="");
  const ratings=ratingsHtml(presentation.ratings);
  const poster=SHARED_ARTWORK.markup(item,ref,capture),backdrop=SHARED_ARTWORK.markup(item,ref,capture,true);
  const watchable=item.kind==="movie"||item.kind==="episode";
  return `<article class="shared-detail"><div class="shared-detail-hero">${backdrop?`<div class="shared-backdrop" aria-hidden="true">${backdrop}</div>`:""}<div class="shared-detail-heading"><div class="poster shared-detail-poster"><div class="art">${poster||'<span class="shared-art-placeholder" aria-hidden="true">◇</span>'}</div></div><div class="shared-detail-copy"><p class="crumbs"><a href="${esc(sharedCatalogueHref(ref,"library"))}">Library</a></p><h2>${esc(item.title||"")}</h2><div class="chips">${chips.map(v=>`<span>${esc(String(v))}</span>`).join("")}</div>${presentation.air_date?`<p class="muted">Released ${esc(presentation.air_date)}</p>`:""}${ratings?`<div class="shared-ratings" aria-label="Ratings">${ratings}</div>`:""}${item.overview?`<p class="overview">${esc(item.overview)}</p>`:""}<div class="actions">${watchable?`<button class="ghost sm" data-shared-watched="${watched?0:1}">${watched?"Mark unwatched":"Mark watched"}</button>`:""}</div>${launch?"":'<p class="muted">Playback is not available for this shared item yet.</p>'}</div></div></div>${detail.files.length?`<section class="shared-versions"><h2 class="section">Playback versions</h2>${detail.files.map((file,index)=>{
    sharedCatalogueId(file.file_id);
    const fileRef=sharedCatalogueReference(file.reference?.item);
    if(JSON.stringify(fileRef)!==JSON.stringify(ref)||file.reference.file_id!==file.file_id) throw new Error("Shared file changed");
    return sharedCatalogueFileHtml(file,index,launch);
  }).join("")}</section>`:""}${["show","season"].includes(item.kind)?`<h2 class="section">${item.kind==="show"?"Seasons":"Episodes"}</h2>`:""}<div id="shared-children" class="grid shared-grid"></div></article>`;
}
async function viewSharedCatalogue(generation=++PAGE_RENDER_GENERATION){
  const capture={generation,auth:AUTH_GENERATION,token:TOKEN,origin:API,route:location.hash};
  const parsed=sharedCatalogueRoute(capture.route);
  const admin=typeof ME!=="undefined"&&!!ME?.is_admin;
  const heading=parsed.kind==="index"?`<header class="shared-page-head"><div><p class="shared-eyebrow">SHARED WITH YOU</p><h1>Shared libraries</h1><p class="shared-page-intro">Explore the libraries people have shared with your Cinema.</p></div>${admin?'<a class="shared-import-link" href="#/settings/sharing">Import a share <span aria-hidden="true">↗</span></a>':""}</header>`:'<header class="shared-page-head shared-page-head-compact"><div><a class="shared-back-link" href="#/shared">‹ All shared sources</a><h1>Shared libraries</h1></div></header>';
  layoutChrome("shared",`${heading}<div id="shared-catalogue"><div class="empty">Loading…</div></div>`);
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
      const entries=Array.from(groups.values()).sort((a,b)=>sharedCatalogueSourceName(a).localeCompare(sharedCatalogueSourceName(b),undefined,{numeric:true,sensitivity:"base"})||a.import_id.localeCompare(b.import_id));
      for(const group of entries){group.hubState="checking";group.libraries=[];group.historyCount=0;}
      if(!paint(sharedCatalogueHubHtml(entries,admin)))return;
      const refresh=()=>sharedCatalogueHubRefresh(entries,capture),search=document.getElementById("shared-source-search"),filter=document.getElementById("shared-source-availability");
      if(search)search.oninput=refresh;if(filter)filter.onchange=refresh;
      if(admin)for(const button of document.getElementById("shared-catalogue")?.querySelectorAll("button[data-shared-rename]")||[]){
        if(!(button instanceof HTMLButtonElement))continue;
        button.onclick=async()=>{
          if(!sharedCatalogueCurrent(capture))return;
          const index=Number(button.dataset.sharedRename),group=entries[index];if(!group)return;
          try{
            const name=await sharingRenameImport(group.import_id,group.source_name);
            if(typeof name!=="string"||!sharedCatalogueCurrent(capture))return;
            group.source_name=name;
            const title=document.getElementById(`shared-source-title-${index}`),card=document.getElementById(`shared-source-card-${index}`),recent=document.getElementById(`shared-continue-${index}`);
            if(title){title.textContent=name;title.setAttribute("title",name);}
            const mark=card?.querySelector(".shared-source-mark");if(mark)mark.textContent=sharedCatalogueInitials(name);
            button.setAttribute("aria-label",`Rename ${name}`);
            for(const label of recent?.querySelectorAll(".shared-card-source")||[]){label.textContent=name;label.setAttribute("title",name);}
            recent?.querySelector("[data-shared-history-more]")?.setAttribute("aria-label",`More titles from ${name}`);
            const feedback=document.getElementById("shared-hub-feedback");if(feedback)feedback.textContent=`Share renamed to ${name}.`;
            refresh();if(card?.hidden)search?.focus();
          }catch(error){if(sharedCatalogueCurrent(capture)){const feedback=document.getElementById("shared-hub-feedback");if(feedback)feedback.textContent=error.message||"Could not rename this share.";}}
        };
      }
      refresh();
      // Each Source paints independently; one unreachable home cannot delay another.
      let nextGroup=0;
      const loadGroup=async(group,index)=>{
        let html;
        try{const reply=await sharedCatalogueRead(`/shared/imports/${group.import_id}/libraries`,capture);
          if(sharedCatalogueGroupKey(reply)!==sharedCatalogueGroupKey(group)||!Array.isArray(reply.libraries)||reply.libraries.length>64) throw new Error("Shared source changed");
          group.libraries=reply.libraries.filter(lib=>group.assigned.has(sharedCatalogueId(lib.library_id)));
          html=group.libraries.map(lib=>sharedCatalogueLibraryHtml(lib,group)).join("")||'<p class="shared-source-loading">No libraries are currently available.</p>';
          group.hubState="ready";
        }catch(error){html=sharedCatalogueError(error);group.hubState="unavailable";}
        if(!sharedCatalogueCurrent(capture))return;
        const el=document.getElementById(`shared-source-${index}`),status=document.getElementById(`shared-source-status-${index}`);
        if(el)el.innerHTML=html;
        if(status){status.textContent=group.hubState==="ready"?"Available":"Needs attention";status.dataset.state=group.hubState;}
        refresh();
        if(history.has(sharedCatalogueGroupKey(group))){
          let recent,remaining=[];
          try{const reply=await sharedCatalogueRead(`/shared/imports/${group.import_id}/continue-watching?limit=200`,capture);
            if(sharedCatalogueGroupKey(reply)!==sharedCatalogueGroupKey(group)||!Array.isArray(reply.items)||reply.items.length>200) throw new Error("Shared history changed");
            if(!["online","unavailable","busy"].includes(reply.availability)||reply.availability!=="online"&&reply.items.length) throw new Error("Shared history unavailable");
            for(const entry of reply.items){const item=entry.item,r=sharedCatalogueReference(item?.reference);if(item.source!=="shared"||typeof item.title!=="string"||item.title.length>16384||sharedCatalogueGroupKey(r)!==sharedCatalogueGroupKey(group)||!group.assigned.has(r.library_id))throw new Error("Shared history assignment changed");}
            // The hub is a bounded overview. Give every share room without
            // mounting thousands of posters when many people have history.
            const shown=reply.items.slice(0,Math.max(1,Math.floor(40/Math.max(1,history.size))));
            remaining=reply.items.slice(shown.length);
            recent=reply.availability==="online"?shown.map(entry=>sharedCatalogueItemHtml(entry.item,{import_id:group.import_id,server_id:group.server_id,catalogue_epoch:group.catalogue_epoch},capture,sharedCatalogueSourceName(group))).join(""):"";
            if(remaining.length)recent+=sharedCatalogueHistoryMoreHtml(group);
            group.historyCount=shown.length;
            if(reply.availability!=="online"){const note=document.getElementById(`shared-history-status-${index}`);if(note)note.textContent="Continue Watching unavailable.";}
          }catch(error){recent="";group.historyCount=0;if(sharedCatalogueCurrent(capture)){const note=document.getElementById(`shared-history-status-${index}`);if(note)note.textContent="Continue Watching unavailable.";}}
          if(sharedCatalogueCurrent(capture)){const el=document.getElementById(`shared-continue-${index}`);if(el){
            el.innerHTML=recent;SHARED_ARTWORK.hydrate(el);
            const wireMore=()=>{const button=el.querySelector("[data-shared-history-more]");if(!(button instanceof HTMLButtonElement))return;
              button.onclick=()=>{if(!sharedCatalogueCurrent(capture))return;const batch=remaining.splice(0,10);button.remove();
                el.insertAdjacentHTML("beforeend",batch.map(entry=>sharedCatalogueItemHtml(entry.item,{import_id:group.import_id,server_id:group.server_id,catalogue_epoch:group.catalogue_epoch},capture,sharedCatalogueSourceName(group))).join("")+(remaining.length?sharedCatalogueHistoryMoreHtml(group):""));
                group.historyCount+=batch.length;SHARED_ARTWORK.hydrate(el);wireMore();refresh();
              };};
            wireMore();
          }refresh();}
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
        paint(sharedCatalogueDetailHtml(detail,ref,capture));
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
        if(["show","season"].includes(item.kind))
          await sharedCatalogueLoadPage(`${base}/items/${ref.item_id}/children`,sharedCatalogueLibraryReference(ref),capture,"shared-children");
      }else{
        const ref=parsed.reference;
        paint(`<form id="shared-search" role="search"><label for="shared-query">Search this library</label><div class="shared-search-controls"><input id="shared-query" name="q" type="search" maxlength="512" value="${esc(parsed.q)}"><button class="ghost" type="submit">Search</button></div></form><div id="shared-items" class="grid shared-grid"></div>`);
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
