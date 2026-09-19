"use strict";
// ---- EPUB reader ---------------------------------------------------------
// The package parser and capability boundary live on the server. This route
// only ever sees normalized manifest hrefs and capability resource URLs; the
// account bearer stays in the parent page and is never appended to a chapter.
let READER=null;
const READER_PREFS_KEY="plurx_reader_prefs";

function readerPrefs(){
  let raw=null; try{ raw=JSON.parse(localStorage.getItem(READER_PREFS_KEY)||"null"); }catch(e){}
  return ReaderCore.normalizePrefs(raw);
}
function storeReaderPrefs(value){
  const prefs=ReaderCore.normalizePrefs(value);
  try{ localStorage.setItem(READER_PREFS_KEY,JSON.stringify(prefs)); }catch(e){}
  return prefs;
}
function readerProgressLabel(value){
  const pct=Math.max(0,Math.min(100,Math.round((value||0)*100)));
  return `${pct}%`;
}
function readerShell(item,file){
  return `<div class="reader-shell" role="application" aria-label="Ebook reader">
    <div class="reader-top">
      <button class="ghost sm" onclick="closeReader()" aria-label="Back to book detail">← Back</button>
      <h1 id="reader-title">${esc(item.title)}</h1><span class="reader-progress" id="reader-progress">0%</span>
      <span class="reader-spacer"></span>
      <button class="ghost sm" onclick="toggleReaderPanel('toc')" aria-expanded="false" id="reader-toc-button">Contents</button>
      <button class="ghost sm" onclick="toggleReaderPanel('search')" aria-expanded="false" id="reader-search-button">Search</button>
      <button class="ghost sm" onclick="toggleReaderSettings()" aria-expanded="false" id="reader-settings-button">Aa</button>
      <button class="ghost sm" onclick="setReaderCompleted(true)" id="reader-finish">Mark finished</button>
      <a class="ghost sm" href="${esc(bookContentUrl(file))}" target="_blank" rel="noopener">Open in…</a>
    </div>
    <div class="reader-body">
      <aside class="reader-panel" id="reader-panel" hidden aria-label="Reader navigation"></aside>
      <section class="reader-stage">
        <div class="reader-loading" id="reader-loading">Opening publication…</div>
        <iframe class="reader-frame" id="reader-frame" sandbox="allow-same-origin" referrerpolicy="no-referrer" title="${esc(item.title)}"></iframe>
        <div class="reader-pop" id="reader-settings" hidden></div>
        <div class="reader-status" id="reader-status" role="status" aria-live="polite"></div>
      </section>
    </div>
    <div class="reader-bottom">
      <button class="ghost sm" onclick="readerStep(-1)" aria-label="Previous page or chapter">← Previous</button>
      <span class="reader-location" id="reader-location">Opening…</span>
      <button class="ghost sm" onclick="readerStep(1)" aria-label="Next page or chapter">Next →</button>
    </div>
  </div>`;
}
function readerStatus(message){
  const status=document.getElementById("reader-status"); if(!status) return;
  status.textContent=message; status.classList.add("on");
  clearTimeout(READER&&READER.statusTimer);
  if(READER) READER.statusTimer=setTimeout(()=>status.classList.remove("on"),1800);
}
function readerError(message){
  const loading=document.getElementById("reader-loading"); if(!loading) return;
  const r=READER, file=r&&r.file;
  loading.className="reader-error";
  loading.hidden=false;
  loading.innerHTML=`<strong>${esc(message||"Cinema could not open this book.")}</strong>
    <p>The original file is unchanged. You can open it in another reading app or download it.</p>
    ${file?`<div class="actions"><a class="ghost" href="${esc(bookContentUrl(file))}" target="_blank" rel="noopener">Open in…</a>
      <a class="ghost" href="${esc(bookContentUrl(file))}" download="${esc(file.filename||"book.epub")}">Download original</a></div>`:""}`;
}
function showReaderLoading(message){
  const loading=document.getElementById("reader-loading"); if(!loading) return;
  loading.className="reader-loading"; loading.textContent=message||"Opening chapter…"; loading.hidden=false;
}
function hideReaderLoading(){ const loading=document.getElementById("reader-loading"); if(loading) loading.hidden=true; }

function readerTocRows(entries,depth){
  return (entries||[]).map(entry=>{
    const href=String(entry.href||"");
    return `<button style="padding-left:${8+depth*16}px" data-reader-href="${esc(href)}" onclick='navigateReader(${esc(JSON.stringify(href))})'>${esc(entry.title||"Untitled")}</button>${readerTocRows(entry.children||[],depth+1)}`;
  }).join("");
}
function showReaderToc(){
  const panel=document.getElementById("reader-panel"); if(!panel||!READER) return;
  panel.innerHTML=`<h2>Contents</h2><div class="reader-toc">${readerTocRows(READER.open.publication.toc,0)||'<div class="muted">This EPUB has no authored table of contents.</div>'}</div>`;
  updateReaderTocCurrent();
}
function showReaderSearch(){
  const panel=document.getElementById("reader-panel"); if(!panel) return;
  panel.innerHTML=`<h2>Search this book</h2><div class="reader-search"><input id="reader-search-query" type="search" minlength="2" autocomplete="off" placeholder="Words in this publication" aria-label="Search this book"><button class="sm" onclick="readerSearch()">Search</button></div><div class="reader-search-results" id="reader-search-results"><div class="muted">Search stays on this Cinema server and in this browser.</div></div>`;
  const q=document.getElementById("reader-search-query");
  if(q){ q.addEventListener("keydown",event=>{if(event.key==="Enter") readerSearch();}); q.focus(); }
}
function toggleReaderPanel(mode){
  const panel=document.getElementById("reader-panel"); if(!panel) return;
  const same=READER&&READER.panel===mode&&!panel.hidden;
  panel.hidden=same; if(READER) READER.panel=same?null:mode;
  for(const name of ["toc","search"]){ const button=document.getElementById(`reader-${name}-button`); if(button) button.setAttribute("aria-expanded",String(!same&&name===mode)); }
  if(same) return;
  if(mode==="search") showReaderSearch(); else showReaderToc();
}
function updateReaderTocCurrent(){
  if(!READER) return;
  const path=ReaderCore.splitHref(READER.href).path;
  document.querySelectorAll("[data-reader-href]").forEach(button=>{
    button.setAttribute("aria-current",String(ReaderCore.splitHref(button.dataset.readerHref).path===path));
  });
}
function readerSettingsHtml(){
  const p=READER.prefs;
  return `<div class="reader-pref-grid">
    <label for="reader-font">Typeface</label><select id="reader-font" onchange="updateReaderPref('font',this.value)">
      <option value="publisher" ${p.font==='publisher'?'selected':''}>Publisher</option><option value="serif" ${p.font==='serif'?'selected':''}>Serif</option><option value="sans" ${p.font==='sans'?'selected':''}>Sans serif</option><option value="accessible" ${p.font==='accessible'?'selected':''}>Accessible</option></select>
    <label for="reader-size">Text size</label><input id="reader-size" type="range" min="70" max="220" step="5" value="${p.fontSize}" oninput="updateReaderPref('fontSize',Number(this.value))">
    <label for="reader-line">Line height</label><input id="reader-line" type="range" min="1.1" max="2.4" step=".05" value="${p.lineHeight}" oninput="updateReaderPref('lineHeight',Number(this.value))">
    <label for="reader-margin">Margins</label><input id="reader-margin" type="range" min="2" max="18" step="1" value="${p.margin}" oninput="updateReaderPref('margin',Number(this.value))">
    <label for="reader-theme">Page</label><select id="reader-theme" onchange="updateReaderPref('theme',this.value)"><option value="light" ${p.theme==='light'?'selected':''}>Light</option><option value="sepia" ${p.theme==='sepia'?'selected':''}>Sepia</option><option value="dark" ${p.theme==='dark'?'selected':''}>Dark</option></select>
    <label for="reader-flow">Layout</label><select id="reader-flow" onchange="updateReaderPref('flow',this.value)"><option value="paginated" ${p.flow==='paginated'?'selected':''}>Paginated</option><option value="scrolled" ${p.flow==='scrolled'?'selected':''}>Scrolled</option></select>
  </div>`;
}
function toggleReaderSettings(){
  const pop=document.getElementById("reader-settings"), button=document.getElementById("reader-settings-button"); if(!pop||!READER) return;
  const opening=pop.hidden; pop.hidden=!opening; button&&button.setAttribute("aria-expanded",String(opening));
  if(opening) pop.innerHTML=readerSettingsHtml();
}
async function updateReaderPref(key,value){
  if(!READER||!READER.navigator) return;
  READER.prefs=storeReaderPrefs(Object.assign({},READER.prefs,{[key]:value}));
  await READER.navigator.applyPrefs(READER.prefs);
  READER.dirty=true; scheduleReaderSave();
}

function readingOrderIndex(href){
  if(!READER) return -1;
  const path=ReaderCore.splitHref(href).path;
  return READER.open.publication.readingOrder.findIndex(link=>ReaderCore.splitHref(link.href).path===path);
}
async function navigateReader(href,locator){
  if(!READER) return;
  const r=READER, generation=(r.navigationGeneration||0)+1;
  r.navigationGeneration=generation;
  // Manifest/TOC/search hrefs are already publication-root-normalized. Only
  // an authored link intercepted inside the current chapter is relative.
  const resolved=readingOrderIndex(href)>=0?href:(ReaderCore.resolveHref(READER.href||href,href)||href);
  const index=readingOrderIndex(resolved);
  if(index<0){ readerStatus("That link is outside this publication."); return; }
  const link=READER.open.publication.readingOrder[index];
  const destination=locator||(ReaderCore.splitHref(resolved).fragment?{version:1,href:resolved}:null);
  showReaderLoading(`Opening ${link.title||`section ${index+1}`}…`);
  try{
    r.index=index; r.href=resolved;
    const url=ReaderCore.resourceUrl(r.open.resource_base,resolved);
    if(!url) throw new Error("The chapter link is invalid.");
    await r.navigator.load(url,resolved,index,r.open.publication.readingOrder.length,r.prefs,destination);
    if(READER!==r||r.navigationGeneration!==generation) return;
    hideReaderLoading(); updateReaderTocCurrent(); updateReaderLocation();
  }catch(error){ if(READER===r&&r.navigationGeneration===generation) readerError(error.message); }
}
function updateReaderLocation(){
  if(!READER) return;
  const total=READER.progression||0, current=READER.index+1, count=READER.open.publication.readingOrder.length;
  const progress=document.getElementById("reader-progress"), location=document.getElementById("reader-location");
  if(progress) progress.textContent=readerProgressLabel(total);
  if(location) location.textContent=`${current} of ${count} · ${readerProgressLabel(total)}`;
}
function readerLocated(locator){
  if(!READER||!locator) return;
  READER.locator=locator;
  READER.progression=locator.locations&&locator.locations.totalProgression||0;
  READER.dirty=true; updateReaderLocation(); scheduleReaderSave();
}
async function readerStep(direction){
  if(!READER||!READER.navigator) return;
  if(READER.navigator.move(direction)) return;
  const next=READER.index+direction;
  if(next<0){ readerStatus("Beginning of book"); return; }
  if(next>=READER.open.publication.readingOrder.length){ readerStatus("End of book · use Mark finished when you are done"); return; }
  await navigateReader(READER.open.publication.readingOrder[next].href);
}
function scheduleReaderSave(){
  if(!READER) return; clearTimeout(READER.saveTimer);
  READER.saveTimer=setTimeout(()=>saveReaderState(),3000);
}
async function saveReaderState(force){
  const r=READER; if(!r||!r.locator||(!r.dirty&&!force)) return;
  if(force) r.forceSave=true;
  if(r.saving){
    r.saveAgain=true;
    // Close/explicit completion must not outrun the write already in flight.
    // Its promise includes any coalesced follow-up snapshot.
    if(force&&r.savePromise) await r.savePromise;
    return;
  }
  r.saving=true;
  r.savePromise=(async()=>{
    try{
      do{
        r.saveAgain=false;
        const forceThis=!!r.forceSave; r.forceSave=false;
        if(!r.locator||(!r.dirty&&!forceThis)) continue;
        const locator=JSON.parse(JSON.stringify(r.locator)), fingerprint=JSON.stringify(locator), completed=!!r.completed;
        try{
          const saved=await api(`/items/${r.itemId}/reading-state`,{method:"PUT",body:{
            file_id:r.fileId,revision:r.open.revision,locator,
            progression:r.progression||0,completed,recorded_at:Math.floor(Date.now()/1000)
          }});
          if(READER===r){
            // A second gesture may have changed completion or location while
            // this request was in flight. Never overwrite that newer intent
            // with the response to the older snapshot.
            const unchanged=JSON.stringify(r.locator)===fingerprint&&r.completed===completed;
            if(unchanged){ r.completed=!!saved.completed; r.dirty=false; }
            updateReaderFinish();
          }
        }catch(error){
          if(READER===r){
            if(error.status===409||error.code==="publication_changed") readerError("The book file changed. Reopen it before continuing.");
            else readerStatus("Reading position will retry");
          }
        }
      }while(READER===r&&(r.saveAgain||r.forceSave));
    }finally{
      r.saving=false; r.savePromise=null;
    }
  })();
  await r.savePromise;
}
function updateReaderFinish(){
  const button=document.getElementById("reader-finish"); if(!button||!READER) return;
  button.textContent=READER.completed?"Mark unfinished":"Mark finished";
  button.onclick=()=>setReaderCompleted(!READER.completed);
}
async function setReaderCompleted(completed){
  if(!READER) return;
  READER.completed=!!completed; READER.dirty=true; updateReaderFinish();
  await saveReaderState(true); readerStatus(completed?"Marked finished":"Marked unfinished");
}

async function readerSearch(){
  const input=document.getElementById("reader-search-query"), out=document.getElementById("reader-search-results");
  if(!input||!out||!READER) return;
  const query=input.value.trim(); if(query.length<2){ out.innerHTML='<div class="muted">Enter at least two characters.</div>'; return; }
  const generation=++READER.searchGeneration; out.innerHTML='<div class="muted">Searching this publication…</div>';
  const results=[], order=READER.open.publication.readingOrder, max=READER.open.limits.markup_bytes||8388608;
  for(let index=0;index<order.length&&results.length<100;index++){
    if(!READER||generation!==READER.searchGeneration) return;
    const link=order[index], url=ReaderCore.resourceUrl(READER.open.resource_base,link.href);
    try{
      const response=await fetch(url,{headers:{accept:"application/xhtml+xml,text/html;q=.9"},referrerPolicy:"no-referrer"});
      const bytes=Number(response.headers.get("content-length")||0), type=response.headers.get("content-type")||"";
      if(!response.ok||bytes>max||!/html|xhtml|xml/i.test(type)){ if(response.body) response.body.cancel(); continue; }
      const source=await response.text(); if(source.length>max) continue;
      let doc=new DOMParser().parseFromString(source,/xhtml|xml/i.test(type)?"application/xhtml+xml":"text/html");
      if(doc.querySelector("parsererror")) doc=new DOMParser().parseFromString(source,"text/html");
      const blocks=Array.from(doc.body?doc.body.querySelectorAll("h1,h2,h3,h4,h5,h6,p,li,blockquote,pre,figcaption"):[]);
      for(const element of blocks){
        const excerpt=ReaderCore.snippet(element.textContent,query); if(!excerpt) continue;
        const within=blocks.length>1?blocks.indexOf(element)/(blocks.length-1):0;
        results.push({title:link.title||`Section ${index+1}`,snippet:excerpt,
          locator:ReaderCore.makeLocator(link.href,index,order.length,within,element)});
        if(results.length>=100) break;
      }
    }catch(error){ /* a missing optional spine document does not end the search */ }
  }
  if(!READER||generation!==READER.searchGeneration) return;
  READER.searchResults=results;
  out.innerHTML=results.length?results.map((result,index)=>`<button onclick="readerSearchGo(${index})"><span class="reader-result-title">${esc(result.title)}</span><span class="reader-result-snippet">${esc(result.snippet)}</span></button>`).join(""):'<div class="muted">No matches in this publication.</div>';
}
async function readerSearchGo(index){
  const result=READER&&READER.searchResults&&READER.searchResults[index]; if(!result) return;
  await navigateReader(result.locator.href,result.locator);
  const panel=document.getElementById("reader-panel"); if(panel&&innerWidth<720) panel.hidden=true;
}

function readerFlushKeepalive(r){
  if(!r||!r.open||!r.locator||!TOKEN) return;
  const body=JSON.stringify({file_id:r.fileId,revision:r.open.revision,locator:r.locator,
    progression:r.progression||0,completed:!!r.completed,recorded_at:Math.floor(Date.now()/1000)});
  fetch(`${API}/items/${r.itemId}/reading-state`,{method:"PUT",headers:{authorization:"Bearer "+TOKEN,"content-type":"application/json"},body,keepalive:true}).catch(()=>{});
}
function closeReaderSession(r){
  if(!r||!r.open||!r.open.session_id||!TOKEN) return;
  fetch(`${API}/publication/${encodeURIComponent(r.open.session_id)}`,{method:"DELETE",headers:{authorization:"Bearer "+TOKEN},keepalive:true}).catch(()=>{});
}
function destroyReader(flush){
  const r=READER; if(!r) return;
  READER=null; clearTimeout(r.saveTimer); clearInterval(r.heartbeat); clearTimeout(r.statusTimer);
  if(flush) readerFlushKeepalive(r);
  if(r.navigator) r.navigator.destroy();
  closeReaderSession(r);
}
async function closeReader(){
  const r=READER; if(!r){ history.back(); return; }
  await saveReaderState(true); const itemId=r.itemId; destroyReader(false);
  if(NATIVE_READER_BOOT){ TOKEN=null; AUTH_GENERATION++; ME=null; nativeReaderPost("close"); return; }
  location.hash=`#/item/${itemId}`;
}
async function viewReader(itemId,fileId){
  if(surfaceClass()==="tv") throw new Error("The ebook reader is available on desktop, phone, and tablet screens.");
  const routeItemId=String(itemId), routeFileId=String(fileId);
  if(READER&&(READER.itemId!==routeItemId||READER.fileId!==routeFileId)) destroyReader(true);
  if(READER) return;
  const detail=await api(`/items/${routeItemId}`), item=detail.item;
  const file=(detail.files||[]).find(candidate=>exactWireId(candidate)===routeFileId);
  if(!canReadBookFile(file)) throw new Error("This ebook format is not available in Cinema's reader.");
  document.getElementById("app").innerHTML=readerShell(item,file);
  try{
    const [opened,state]=await Promise.all([
      api(`/files/${routeFileId}/publication`,{method:"POST"}),
      api(`/items/${routeItemId}/reading-state?file_id=${routeFileId}`)
    ]);
    const saved=state&&!state.stale?state.state:null, prefs=readerPrefs();
    READER={itemId:routeItemId,fileId:routeFileId,item,file,detail,open:opened,prefs,locator:saved&&saved.locator||null,
      progression:saved&&saved.progression||0,completed:!!(saved&&saved.completed),dirty:false,
      index:0,href:opened.publication.readingOrder[0].href,searchGeneration:0,navigationGeneration:0};
    const frame=document.getElementById("reader-frame");
    READER.navigator=new ReaderCore.FrameNavigator(frame,{
      onLocation:readerLocated,
      onNavigate:href=>navigateReader(href),
      onBlockedLink:()=>readerStatus("External publication links are blocked. Use Open in… for the original file."),
      onBoundary:direction=>readerStep(direction),
    });
    updateReaderFinish(); showReaderToc(); updateReaderLocation();
    const start=saved&&saved.locator&&readingOrderIndex(saved.locator.href)>=0?saved.locator.href:READER.href;
    await navigateReader(start,saved&&saved.locator);
    READER.heartbeat=setInterval(()=>saveReaderState(true),30000);
    nativeReaderPost("ready");
  }catch(error){
    if(!READER) READER={itemId:routeItemId,fileId:routeFileId,item,file,detail,open:null,locator:null,progression:0,completed:false};
    readerError(error.message);
    nativeReaderPost("error",error.message);
  }
}
document.addEventListener("visibilitychange",()=>{ if(document.visibilityState==="hidden"&&READER) readerFlushKeepalive(READER); });
window.addEventListener("beforeunload",()=>{ if(READER){ readerFlushKeepalive(READER); closeReaderSession(READER); } });

