"use strict";
// ---- settings -------------------------------------------------------------
function currentScanStatus(st,lastScanAt){
  if(!st||st.running||!st.error||st.finished_at==null||lastScanAt==null) return st;
  // Scan status is node-local, while last_scan_at is replicated. If another
  // node completed this library after the local failure, the failure is old
  // news and must not remain as the cluster's apparent current state.
  return Number(lastScanAt)>=Number(st.finished_at)?null:st;
}
function statusText(st,lastScanAt){
  st=currentScanStatus(st,lastScanAt);
  if(!st) return lastScanAt?`Last scan ${fmtAgo(lastScanAt)} · result unavailable`:"No scan result available";
  if(st.running){
    if(st.phase==="enriching") return "fetching metadata…";
    const p=st.progress;
    return "scanning…"+(p&&p.found?` ${p.processed} / ${p.found} files`:"");
  }
  if(st.error) return `<span style="color:var(--bad)">error: ${esc(st.error)}</span>`;
  if(st.last_scan){
    const r=st.last_scan;
    // added/updated/unchanged/skipped/unreadable partition the files found;
    // `degraded` is a subset of added+updated, so it is rendered *attached* to
    // them rather than as a sibling number — "2 added (2 incomplete)" says what
    // "2 added … 2 errors" left the reader to guess.
    let s=`${r.added} added`;
    if(r.degraded) s+=` <span style="color:var(--bad)">(${r.degraded} incomplete)</span>`;
    s+=`, ${r.updated} updated, ${r.unchanged} unchanged`;
    if(r.removed_files) s+=`, ${r.removed_files} removed`;
    if(r.skipped) s+=`, ${r.skipped} skipped`;
    if(r.unreadable) s+=`, <span style="color:var(--bad)">${r.unreadable} unreadable</span>`;
    if(r.errors) s+=`, <span style="color:var(--bad)">${r.errors} error${r.errors>1?'s':''}</span>`;
    if(st.finished_at) s+=` <span style="opacity:.7">· ${fmtAgo(st.finished_at)}</span>`;
    // Every error carries a problem line (scan/mod.rs keeps that invariant),
    // so this list is where a red count becomes actionable. Skips and the
    // "…and N more" truncation notice are informational, and read as such.
    const probs=r.problems||[];
    // Skips arrive grouped by folder (scan/mod.rs): one mis-named show is one
    // row, not one row per episode. Twenty-four lines differing only in the
    // episode number were never twenty-four problems.
    const groups=r.skip_groups||[];
    if(probs.length||groups.length){
      const isInfo=p=>/^(skipped |…and )/.test(p);
      const lines=probs.map(p=>{
        const info=isInfo(p);
        return `<div class="problem${info?" info":""}">${info?"·":"⚠"} ${esc(p)}</div>`;
      }).join("");
      const grouped=groups.map(g=>{
        const n=g.count||0;
        const files=n===1?"file":"files";
        // The samples are a taste of the shape, not the list this replaces —
        // so they are behind a disclosure rather than printed by default.
        const samples=(g.samples||[]).length
          ? `<div class="skipsamples">${(g.samples||[]).map(esc).join("<br>")}${
              n>(g.samples||[]).length?`<br>…and ${n-(g.samples||[]).length} more`:""}</div>`
          : "";
        return `<details class="skipgroup"><summary>· skipped <span class="count">${n}</span> `+
          `${files} in <code>${esc(g.folder)}</code> — ${esc(g.reason)}</summary>${samples}</details>`;
      }).join("");
      // Real failures first: the operator sees what to act on without
      // scrolling, and skips are informational by definition.
      const infoOnly=probs.every(isInfo);
      s+=`<div class="problems${infoOnly?" info-only":""}">${lines}${grouped}</div>`;
    }
    return s;
  }
  return "idle";
}
let SETTINGS=null, TRAKT=null, TRAKT_EDIT=false, SETTINGS_TICKING=null;
let LOGS_RUN=null, CLUSTER_LOGS_RUN=null;
let SETTINGS_DATA={}, SETTINGS_LOADED=new Set(), SETTINGS_LOADS=new Map();
const SETTINGS_ENDPOINTS={
  libs:()=>api("/libraries"),settings:()=>api("/settings"),status:()=>api("/scan/status"),
  dvConversions:()=>api("/dv-conversions"),
  sys:()=>api("/system"),users:()=>api("/users"),trakt:()=>api("/trakt/status"),
  developerReadiness:()=>api("/developer/readiness"),
  analysis:()=>api("/analysis/summary"),
  playbackEvents:()=>api(`/system/playback-events?since=${Date.now()-7*24*60*60*1000}&limit=2000`),
  cluster:()=>api("/cluster/nodes").catch(error=>{
    if(error&&error.status===401) throw error;
    return {unavailable:true,code:error.code,message:error.message};
  }),
  // Stamped here rather than where the value is painted: loadSettingsKey serves
  // a cached aggregate without a request, and stamping that would let a tab
  // switch every ten seconds starve the refresh indefinitely.
  clusterOps:()=>api("/cluster/status",{keepSessionOn401:true}).then(ops=>{
    clusterOpsStamp(); return clusterOpsReceived(ops);
  }).catch(error=>{
    // A request from a credential generation that has already been replaced
    // describes nothing about the current session; let it stay an error rather
    // than painting it into the new one.
    if(error&&error.staleAuth) throw error;
    // A refused recovery read is a fact about the cluster, so it is reported
    // by the panel that reports the cluster — named, dated and retryable —
    // rather than as a page-level failure or, as it once was, as the end of
    // the operator's session. `clusterOperationsCard` already knows this
    // shape; it is the same one `/cluster/nodes` uses when it cannot answer.
    clusterOpsStamp();
    return {unavailable:true,code:(error&&error.code)||null,
            message:(error&&error.message)||"request failed"};
  }),
};
const SETTINGS_MANIFEST={
  libraries:{required:["settings","libs","status","dvConversions"],secondary:[]},
  metadata:{required:["settings"],secondary:["libs"]},
  playback:{required:["settings"],secondary:["developerReadiness"]},
  livetv:{required:["settings"],secondary:[]},
  analysis:{required:["settings","analysis"],secondary:[]},
  maintenance:{required:["settings","dvConversions"],secondary:[]},
  users:{required:["users"],secondary:[]},
  system:{required:["sys"],secondary:["playbackEvents"]},
  cluster:{required:["cluster"],secondary:["clusterOps","developerReadiness"]},
  integrations:{required:["settings","trakt"],secondary:[]},
  developer:{required:["settings"],secondary:["developerReadiness"]},
};
function cacheSettings(value){
  SETTINGS=value; SETTINGS_DATA.settings=value; SETTINGS_LOADED.add("settings");
  return value;
}
function cacheTrakt(value){
  TRAKT=value; SETTINGS_DATA.trakt=value; SETTINGS_LOADED.add("trakt");
  return value;
}
function settingsCurrent(generation,tab){
  return generation===PAGE_RENDER_GENERATION&&isSettingsRoute(location.hash)&&settingsTab()===tab;
}
function loadSettingsKey(key,generation,cacheResult=true){
  if(SETTINGS_LOADED.has(key)) return Promise.resolve(SETTINGS_DATA[key]);
  const active=SETTINGS_LOADS.get(key);
  let transport=active&&active.promise;
  if(!transport){
    transport=SETTINGS_ENDPOINTS[key]();
    const entry={promise:transport};
    SETTINGS_LOADS.set(key,entry);
    transport.finally(()=>{
      if(SETTINGS_LOADS.get(key)===entry) SETTINGS_LOADS.delete(key);
    }).catch(()=>{});
  }
  return transport.then(value=>{
    if(generation!==PAGE_RENDER_GENERATION||!cacheResult) return value;
    SETTINGS_DATA[key]=value; SETTINGS_LOADED.add(key);
    if(key==="dvConversions") DV_SETTINGS_POLL_AT=Date.now()+DV_PROGRESS_POLL_MS;
    if(key==="settings") cacheSettings(value);
    if(key==="trakt") cacheTrakt(value);
    if(key==="cluster") CLUSTER_LOADED=true;
    return value;
  });
}
function settingsTabsHtml(tab){
  // The rail. Every entry routes to #/settings/<section>, so the back button
  // walks between sections; render() keeps the cached aggregate across the
  // switch because the previous route was also Settings.
  const d=SETTINGS_DATA||{};
  return SET_GROUPS.map(([group,tabs])=>`<div class="setgroup">${esc(group)}</div>`+tabs.map(([id,label])=>{
    const aside=settingsTabAside(id,d);
    return `<button class="settab${tab===id?' active':''}" role="link" aria-current="${tab===id?'page':'false'}" onclick="setSettingsTab('${id}')">${esc(label)}${aside}</button>`;
  }).join("")).join("");
}
// What a rail entry knows before it is opened: a count, a one-word status, or
// a dot for "something here needs you". Only from data already loaded — the
// rail never spends a request of its own.
function settingsTabAside(id,d){
  if(id==="libraries"&&Array.isArray(d.libs)) return `<span class="setn">${d.libs.length}</span>`;
  if(id==="users"&&Array.isArray(d.users)) return `<span class="setn">${d.users.length}</span>`;
  if(id==="metadata"&&d.settings&&!d.settings.tmdb_configured&&Array.isArray(d.libs)
     &&d.libs.some(l=>!l.anime&&l.kind!=="home"&&l.kind!=="books"))
    return `<span class="setdot" title="No TMDB key — Movies and TV get no artwork"></span>`;
  if(id==="cluster"&&d.cluster){
    if(d.cluster.unavailable) return d.cluster.code==="membership_unavailable"?`<span class="setn">sqlite</span>`:"";
    const state=PlurxClusterPanel.clusterStateView(d.cluster);
    return `<span class="setn">${esc(state.tone==="good"?"in sync":state.tone==="warn"?"attention":"")}</span>`;
  }
  return "";
}
function settingsShell(tab){
  return `<div class="setlayout"><nav class="settabs" aria-label="Settings sections">${settingsTabsHtml(tab)}</nav><div class="adminwrap${tab==="cluster"?" clusterwrap":""}" id="setbody"><div class="empty">Loading ${esc(SET_TABS.find(row=>row[0]===tab)[1])}…</div></div></div>`;
}
function patchSettingsSecondary(tab,key,value){
  if(tab==="metadata"&&key==="libs"){
    const mount=document.getElementById("metadata-libraries");
    if(mount) mount.innerHTML=keyBackfillHtml(SETTINGS_DATA.libs||[]);
  }
  if(tab==="system"&&key==="playbackEvents"){
    const mount=document.getElementById("settings-playback-events");
    if(mount) mount.innerHTML=playbackSummaryCard(SETTINGS_DATA.playbackEvents||[]);
  }
  if(["developer","playback","cluster"].includes(tab)&&key==="developerReadiness"){
    // Patch only the evidence rows. Re-rendering the entire panel here could
    // overwrite a local prepared-handoff toggle while its change event is in
    // flight.
    applyDeveloperReadiness(value);
  }
  if(tab==="cluster"&&key==="clusterOps"){
    // The roster is already interactive while this secondary request is in
    // flight. Hold a result that arrives under an election decision rather
    // than storing data the page did not paint; the next gated collection will
    // pay the update after the dialog closes.
    if(clusterRepaintDeferred()){
      patchClusterReadingAge();
      return;
    }
    SETTINGS_DATA.clusterOps=value; SETTINGS_LOADED.add("clusterOps");
    repaintClusterPreserving(renderSettings);
  }
}
function patchSettingsSecondaryError(tab,key,error){
  const message=esc(error&&error.message||"request failed");
  if(tab==="metadata"&&key==="libs"){
    const mount=document.getElementById("metadata-libraries");
    if(mount) mount.innerHTML=`<div class="hint">Library context is unavailable: ${message}</div>`;
  }
  if(tab==="system"&&key==="playbackEvents"){
    const mount=document.getElementById("settings-playback-events");
    if(mount) mount.innerHTML=`<div class="card"><h2 class="section" style="margin-top:0">Playback <span class="muted">(7 days)</span></h2><span class="muted">Playback history is unavailable: ${message}</span></div>`;
  }
  if(["developer","playback","cluster"].includes(tab)&&key==="developerReadiness"){
    applyDeveloperReadiness({unavailable:error&&error.message||"request failed"});
  }
  if(tab==="cluster"&&key==="clusterOps"){
    const mount=document.getElementById("cluster-operations");
    if(mount) mount.innerHTML=clusterOperationsUnavailable(message);
  }
}
async function loadSettingsTab(generation,tab){
  const route=location.hash, manifest=SETTINGS_MANIFEST[tab];
  try{
    await Promise.all(manifest.required.map(key=>loadSettingsKey(key,generation)));
  }catch(error){
    if(error&&error.status===401) return;
    if(!settingsCurrent(generation,tab)) return;
    const body=document.getElementById("setbody");
    if(body) body.innerHTML=`<div class="empty">${esc(error.message)}<div style="margin-top:12px"><button class="ghost sm" onclick="retrySettingsTab()">Retry</button></div></div>`;
    setPageFailure(route,generation,"render_error");
    setPagePhase(route,generation,"content"); setPagePhase(route,generation,"settled");
    return false;
  }
  if(!settingsCurrent(generation,tab)) return false;
  renderSettings();
  setPageFailure(route,generation,null); setPagePhase(route,generation,"content");
  const secondary=manifest.secondary.map(key=>{
    // Cluster status owns an atomic store-and-paint boundary because the
    // already-rendered roster can open a decision dialog before this secondary
    // response arrives. Other secondary data can keep the ordinary eager cache.
    const staged=tab==="cluster"&&key==="clusterOps";
    return loadSettingsKey(key,generation,!staged).then(value=>{
      if(settingsCurrent(generation,tab)) patchSettingsSecondary(tab,key,value);
    }).catch(error=>{
      if(error&&error.status===401) throw error;
      if(settingsCurrent(generation,tab)){
        patchSettingsSecondaryError(tab,key,error);
        setPageFailure(route,generation,`${key}_error`);
      }
    });
  });
  if(tab==="system") secondary.push(refreshLogs());
  if(tab==="cluster") secondary.push(refreshClusterLogs());
  await Promise.allSettled(secondary);
  if(settingsCurrent(generation,tab)) setPagePhase(route,generation,"settled");
  return settingsCurrent(generation,tab);
}
async function viewSettings(generation=++PAGE_RENDER_GENERATION,reset=true){
  if(!ME.is_admin){ location.hash="#/"; return; }
  const route=location.hash;
  if(reset){
    SETTINGS=null; TRAKT=null; TRAKT_EDIT=false; SETTINGS_DATA={};
    SETTINGS_LOADED.clear(); SETTINGS_LOADS.clear();
    DV_SETTINGS_POLL_AT=0;
    CLUSTER_LOADED=false; forgetJoinToken();
  }
  const tab=settingsTab();
  // Leaving a section drops any minted join token: it is bearer material, and
  // the operator who switched away is done with it whether or not they said so.
  if(SETTINGS_SHOWN_TAB!==tab) forgetJoinToken();
  SETTINGS_SHOWN_TAB=tab; LIB_DRAWER=null; USER_DRAWER=null;
  try{ localStorage.setItem("plurx_settings_tab",tab); }catch(e){}
  layoutChrome("settings",settingsShell(tab));
  setPagePhase(route,generation,"shell");
  const loaded=await loadSettingsTab(generation,tab);
  if(loaded) setPageTimer(()=>settingsTick(generation,tab), 2000, generation);
}
