"use strict";
// ---- settings sections ----------------------------------------------------
// Grouped by who acts on them and how often: content you manage, playback
// you tune, the server you operate, and outside services. The order here is
// the rail's order and the chip strip's order.
const SET_GROUPS=[
  ["Content",[["libraries","Libraries"],["metadata","Metadata"],["livetv","Live TV"]]],
  ["Playback",[["playback","Playback"],["analysis","Analysis"]]],
  ["Server",[["maintenance","Maintenance"],["users","Users"],["system","System"],["cluster","Cluster"]]],
  ["Outside",[["integrations","Integrations"]]],
  ["Developer",[["developer","Developer"]]],
];
const SET_TABS=SET_GROUPS.flatMap(([,tabs])=>tabs);
// Every section is a route: #/settings/<section>. The bare #/settings (and
// the older #/admin) land on the last section this browser visited, so the
// address bar names the section and the back button walks between them.
function isSettingsRoute(h){ return h==="#/settings"||h==="#/admin"||(typeof h==="string"&&h.startsWith("#/settings/")); }
function settingsRouteTab(h){
  const m=/^#\/settings\/([a-z]+)(?:\/[a-z0-9-]+)?$/.exec(h||"");
  return m&&SET_TABS.some(x=>x[0]===m[1])?m[1]:null;
}
// A section route may name one element after it —
// #/settings/developer/enable-subtitle-sources — so a link from another page
// lands on the card it means rather than the top of a long section.
function settingsRouteAnchor(h){
  const m=/^#\/settings\/[a-z]+\/([a-z0-9-]+)$/.exec(h||"");
  return m?m[1]:null;
}
// Once: after the section paints, scroll to the anchor and rewrite the address
// to the bare section, so a repaint (every refresh timer) does not scroll the
// page back under the reader.
function revealSettingsAnchor(tab){
  const anchor=settingsRouteAnchor(location.hash);
  if(!anchor) return;
  const target=document.getElementById(anchor);
  if(!target) return;
  try{ history.replaceState(null,"",`#/settings/${tab}`); }catch(e){}
  if(target.scrollIntoView) target.scrollIntoView({block:"start"});
}
function settingsTab(){
  const routed=settingsRouteTab(location.hash);
  if(routed) return routed;
  let t="libraries"; try{ t=localStorage.getItem("plurx_settings_tab")||"libraries"; }catch(e){}
  return SET_TABS.some(x=>x[0]===t)?t:"libraries";
}
// Which section the shell last painted. viewSettings compares against it to
// drop the join token on a section change, and render() uses the route to
// keep the cached aggregate across one (a tab switch is not a page load).
let SETTINGS_SHOWN_TAB=null;
function setSettingsTab(t){
  if(t===settingsTab()) return;
  if(!SET_TABS.some(x=>x[0]===t)) return;
  location.hash=`#/settings/${t}`;
}
function retrySettingsTab(){
  clearInterval(PAGE_TIMER); PAGE_TIMER=null;
  viewSettings(++PAGE_RENDER_GENERATION,false);
}
function renderSettings(){
  // Settings work resolves asynchronously. The selected tab survives in
  // localStorage after navigation, so it cannot prove this route is still on
  // screen; a late cluster read or mutation must not repaint another page.
  if(!isSettingsRoute(location.hash)) return;
  const d=SETTINGS_DATA;
  const tab=settingsTab();
  const manifest=SETTINGS_MANIFEST[tab];
  if(!manifest.required.every(key=>SETTINGS_LOADED.has(key))) return;
  document.getElementById("main").innerHTML=`<div class="setlayout"><nav class="settabs" aria-label="Settings sections">${settingsTabsHtml(tab)}</nav><div class="adminwrap${tab==="cluster"?" clusterwrap":""}" id="setbody">${settingsPanel(tab,d)}</div></div>`;
  revealSettingsAnchor(tab);
  if(tab==="system") return refreshLogs(); // its initial loading row is part of settled
  if(tab==="cluster"){ applyClusterFolds(); return refreshClusterLogs(); }
}
function settingsPanel(tab,d){
  if(tab==="metadata")     return metadataPanel(d.settings,d.developerReadiness);
  if(tab==="playback")     return playbackPanel(d.settings,d.developerReadiness);
  if(tab==="livetv")       return liveTvPanel(d.settings,d.developerReadiness);
  if(tab==="analysis")     return analysisSettingsPanel(d.settings,d.analysis);
  if(tab==="maintenance")  return maintenancePanel(d.settings,d.dvConversions,d.developerReadiness);
  if(tab==="users")        return usersPanel(d.users,d.settings);
  if(tab==="system")       return systemPanel(d.sys,d.playbackEvents);
  if(tab==="cluster")      return clusterPanel(d);
  if(tab==="integrations") return integrationsPanel(d.settings,d.trakt);
  if(tab==="developer")    return developerPanel(d.settings,d.developerReadiness);
  return librariesPanel(d.libs,d.status,d.settings,d.dvConversions);
}
// The section header: name, one line of summary, the section's bulk actions.
function setHead(title,summary,tools){
  return `<div class="sethead"><div><h1>${esc(title)}</h1>${summary?`<p>${summary}</p>`:""}</div>${tools?`<div class="row">${tools}</div>`:""}</div>`;
}
// A card is the unit of saving. Its Save stays disabled until something inside
// it changes; the footer names the state so a half-edited page cannot be
// mistaken for a saved one. Cards marked `local` save on change and opt out.
function markSetCard(el){
  const card=el&&el.closest?el.closest(".setcard"):null;
  if(!card||card.classList.contains("local")) return;
  card.dataset.revision=String(Number(card.dataset.revision||0)+1);
  card.classList.add("dirty");
  const save=card.querySelector(".setfoot button.primary"); if(save) save.disabled=false;
  const state=card.querySelector(".setfoot .setstate"); if(state) state.textContent="Unsaved changes";
}
function setCardSaved(el,label){
  const card=el&&el.closest?el.closest(".setcard"):null;
  if(!card) return;
  card.classList.remove("dirty");
  const save=card.querySelector(".setfoot button.primary"); if(save) save.disabled=true;
  const state=card.querySelector(".setfoot .setstate"); if(state) state.textContent=label||"Saved";
}
function setCardFoot(saveFn,extra){
  return `<div class="setfoot"><button class="primary sm" disabled onclick="${saveFn}(this)">Save</button>${extra||""}<span class="setstate">Saved</span></div>`;
}
function setCard(body,opts){
  const o=opts||{};
  return `<div class="card setcard${o.local?" local":""}"${o.id?` id="${o.id}"`:""} oninput="markSetCard(this)" onchange="markSetCard(this)">${body}</div>`;
}
function cardHead(title,sub,tools){
  return `<div class="cardhead"><div><h2 class="section">${title}</h2>${sub?`<p class="sub">${sub}</p>`:""}</div>${tools?`<div class="cardtools">${tools}</div>`:""}</div>`;
}
function togRow(id,label,note,checked,attrs){
  return `<label class="tog" for="${id}"><span>${label}${note?`<small>${note}</small>`:""}</span><input type="checkbox" id="${id}"${checked?" checked":""}${attrs?" "+attrs:""}></label>`;
}
function togSelect(id,label,note,options,value,title){
  return `<label class="tog" for="${id}"><span>${label}${note?`<small>${note}</small>`:""}</span>${everySelect(id,options,value,title||label)}</label>`;
}
// Intervals in minutes. Nothing under 15 — the server refuses it, because a
// real library scanned every minute is a NAS denial-of-service on a timer.
const SCAN_EVERY=[[0,"Never"],[15,"15 min"],[30,"30 min"],[60,"1 hour"],[180,"3 hours"],[360,"6 hours"],[720,"12 hours"],[1440,"Daily"]];
const REFRESH_EVERY=[[0,"Never"],[1440,"Daily"],[10080,"Weekly"],[43200,"Monthly"]];
function everySelect(id,options,value,title){
  return `<select id="${id}" class="every" title="${esc(title)}">${
    options.map(([v,label])=>`<option value="${v}"${Number(value)===v?' selected':''}>${esc(label)}</option>`).join("")}</select>`;
}
// The per-library schedule lives in the row's drawer with everything else
// that configures the library; the row itself only reports.
function libScheduleFields(l){
  const last=l.last_scan_at?`<div><label>&nbsp;</label><span class="muted" style="font-size:12px">last scan ${esc(agoLabel(l.last_scan_at))}</span></div>`:'';
  return `<div><label for="sched-scan-${l.id}">Scan for changes</label>${everySelect(`sched-scan-${l.id}`,SCAN_EVERY,l.scan_interval_mins,"How often to look for new & changed files")}</div>
    <div><label for="sched-ref-${l.id}">Refresh art</label>${everySelect(`sched-ref-${l.id}`,REFRESH_EVERY,l.refresh_interval_mins,"How often to re-fetch metadata for everything — heavy; days, not hours")}</div>${last}`;
}
function agoLabel(ts){
  const s=Math.max(0,Math.floor(Date.now()/1000)-ts);
  if(s<90) return "just now";
  if(s<5400) return `${Math.round(s/60)} min ago`;
  if(s<172800) return `${Math.round(s/3600)}h ago`;
  return `${Math.round(s/86400)}d ago`;
}
const RETRY_EVERY=[[0,"Never"],[360,"6 hours"],[720,"12 hours"],[1440,"Daily"],[10080,"Weekly"]];
// Shorter buckets than the rest: this job is on by default, and a missing
// poster is visible on every screen the item appears on.
const ART_EVERY=[[0,"Never"],[30,"30 minutes"],[60,"Hourly"],[360,"6 hours"],[1440,"Daily"]];
const CLEAN_EVERY=[[0,"Never"],[60,"Hourly"],[360,"6 hours"],[1440,"Daily"]];
function maintenancePanel(settings,dv,readiness){
  if(!ME||!ME.is_admin||!settings) return '';
  const jobs=setCard(`${cardHead("Scheduled jobs","All off by default except missing-artwork retries. Each one leaves no trace a normal scan would notice, which is why it needs a timer of its own.")}
    ${togSelect("job-probe","Retry unreadable files","Re-probe anything scanned without codec or duration — the fix (permissions, a remounted share) is invisible to a scan.",RETRY_EVERY,settings.probe_retry_mins,"Re-run ffprobe on files whose media details were never read")}
    ${togSelect("job-art","Retry missing artwork","Re-fetch posters a provider matched but never delivered, so a blank card does not stay blank until someone refreshes the library by hand.",ART_EVERY,settings.artwork_retry_mins,"Re-fetch posters for items a provider matched but that never got an image")}
    ${togSelect("job-clean","Clean transcode cache","Delete working directories a crash or a host reboot left behind, which no live session will ever claim.",CLEAN_EVERY,settings.transcode_cleanup_mins,"Delete leftover transcode working directories no session owns")}
    ${togRow("job-boot","Scan every library at startup","About 30 seconds after the server starts — a server switched off while files landed doesn't notice them until its next scheduled run.",settings.scan_on_startup)}
    ${setCardFoot("saveMaintenance")}`);
  return `${setHead("Maintenance","Background work this node does on its own: what is scheduled, what it costs, and where to turn it off.")}
    <div class="setgrid2">${jobs}${precachePanel(settings)}${subtitleStorePanel(settings)}${dvDiskPanel(settings,dv)}${windowsServerCard(settings,readiness)}${telemetryPanel(settings)}</div>`;
}
const PRODUCE_EVERY=[[0,"Never"],[360,"6 hours"],[720,"12 hours"],[1440,"Daily"]];
const CACHE_SIZES=[0,10,25,50,100,250,500,1000];
function precachePanel(settings){
  const used=settings.cache_used_bytes||0;
  const state = settings.cache_max_gb
    ? `${fmtBytes(used)} of ${settings.cache_max_gb} GB used`
    : `off${used?` — ${fmtBytes(used)} still stored, cleared on the next sweep`:""}`;
  return setCard(`${cardHead("Pre-transcoding","Encodes ahead of time what you're most likely to play next — what you're part-way through, what's up next in a show, then anything 4K or HDR that arrived recently — so pressing play costs nothing.",`<span class="pill${settings.cache_max_gb?" ok":""}">${esc(state)}</span>`)}
    <div class="setfields">
      <div><label for="cache-produce">Look for work</label>${everySelect("cache-produce",PRODUCE_EVERY,settings.cache_produce_mins,"How often to check what's worth encoding ahead of time")}</div>
      <div><label for="cache-gb">Disk budget</label><select id="cache-gb" class="every" title="How much disk finished pre-transcodes may occupy before the oldest are evicted">
          ${CACHE_SIZES.map(n=>`<option value="${n}"${Number(settings.cache_max_gb)===n?' selected':''}>${n?n+" GB":"Off"}</option>`).join("")}
        </select></div>
    </div>
    <div class="hint"><b>It shares the encoder with live playback and always loses:</b> the moment anyone presses play it stops and hands the hardware over, picking up later from where it stopped. Only sources nothing plays natively are worth it (4K, HDR, HEVC); an ordinary 1080p file is skipped. A budget of <b>Off</b> stops production and clears what's stored. What it is doing right now is on Activity.</div>
    ${setCardFoot("savePrecache")}`);
}
// Stored subtitle tracks: the subtitle-source store's footprint on this node,
// and what its producer — the fragment-index pass keeping each PGS track it
// reads — is doing now. Background work on real disks says here what it is,
// why it chose the work, what it costs, whether it can run, and where to turn
// it off. Read-only: the switch lives on Developer beside its readiness rows,
// and the link lands on it.
function subtitleStorePanel(settings){
  const SUBSRC_SWITCH_LINK=`<a href="#/settings/developer/enable-subtitle-sources">Developer → Stored subtitle tracks</a>`;
  const d=settings.subtitle_store||{};
  const on=settings.subtitle_stored_sources!==false;
  const gate=d.gate||{open:true};
  const blocked=on&&gate.open===false;
  const fp=d.footprint;
  const plural=(n,word)=>`${n} ${word}${n===1?"":"s"}`;
  const size=fp?`${fmtBytes(fp.bytes)||"0 B"} · ${plural(fp.directories,"file")}`:"not measured yet";
  const pill=!on?`<span class="pill">off</span>`
    :blocked?`<span class="pill bad">not keeping tracks</span>`
    :`<span class="pill ok">${esc(size)}</span>`;
  const why=blocked?`<div class="setwarn">⚠ <b>The index pass on this node keeps no PGS tracks:</b> ${esc(gate.reason||"a requirement is not met")}. Each requirement is checked on ${SUBSRC_SWITCH_LINK}.</div>`:"";
  const ago=d.footprint_age_ms!=null?` (measured ${fmtDur(d.footprint_age_ms)||"just now"}${d.footprint_age_ms>=1000?" ago":""})`:"";
  const measured=fp
    ? `<p class="hint">On this node the store holds <b>${esc(size)}</b>${esc(ago)}, of a ${fmtBytes(d.cap_bytes)||"—"} cap; the least recently used files go first.</p>`
    : Number(settings.vod_index_mins)===0
      ? `<p class="hint">The store's size on this node is not measured: background analysis is paused (Settings → Analysis), and the sweep that measures the store runs with each analysis pass. The store is capped at ${fmtBytes(d.cap_bytes)||"—"}.</p>`
      : `<p class="hint">The store's size on this node is not measured yet: the next background analysis pass's sweep measures it. The store is capped at ${fmtBytes(d.cap_bytes)||"—"}.</p>`;
  const riding=d.riding||[];
  const rideTitle=r=>{
    const name=esc(r.title||`File ${r.file_id}`);
    return r.item_id?`<a href="#/item/${esc(r.item_id)}">${name}</a>`:name;
  };
  const rides=riding.length
    ? `<ul class="subsrc-rides">${riding.map(r=>`<li><b>${rideTitle(r)}</b>: keeping ${plural(r.tracks,"PGS track")}, ${fmtBytes(r.bytes_written)||"0 B"} written so far · running ${fmtDur(r.running_ms)||"just now"}</li>`).join("")}</ul>`
    : `<p class="hint">No index pass on this node is keeping PGS tracks right now.</p>`;
  const since=`Since this process started: ${plural(d.tracks_attempted||0,"track")} attempted — ${d.kept||0} kept, ${d.empty||0} with no cues, ${d.malformed||0} malformed, ${d.transient||0} to retry — and ${fmtBytes(d.bytes_written)||"0 B"} written${d.files_not_riding?`; ${plural(d.files_not_riding,"file")} indexed without it after a riding pass failed`:""}${d.discarded_switch_off?`; ${d.discarded_switch_off} riding pass${d.discarded_switch_off===1?"":"es"} finished after it was turned off and kept nothing`:""}.`;
  return setCard(`${cardHead("Stored subtitle tracks","While a file is indexed on this node, the index pass also keeps each PGS subtitle track it reads, so a burned or overlaid PGS subtitle never has to read the whole file again.",pill)}
    ${why}
    <div class="hint"><b>Why this work:</b> the index pass already reads every packet of the file, so keeping the subtitle packets costs a few megabytes of disk per film and no extra read.</div>
    ${measured}
    ${rides}
    <p class="hint">${esc(since)}</p>
    <div class="hint"><b>To turn it off:</b> ${SUBSRC_SWITCH_LINK}. Off takes effect at once: playback stops reading stored tracks, a pass already running finishes its index but publishes none of the tracks it kept, and later passes keep none. What is already stored stays on disk until the size cap or the sweep removes it.</div>`,{id:"subsrcstore"});
}
function telemetryPanel(settings){
  return setCard(`${cardHead("Playback telemetry","Bounded, node-local playback measurements behind the Playback (7 days) card on System.")}
    <div class="setfields"><div><label for="telemetry-retain">Retain events</label>
        <select id="telemetry-retain" class="every" title="How long node-local playback measurements are kept">
          ${[[0,"Off"],[7,"7 days"],[14,"14 days"],[30,"30 days"],[60,"60 days"],[90,"90 days"]].map(([n,label])=>`<option value="${n}"${Number(settings.telemetry_retain_days)===n?' selected':''}>${label}</option>`).join("")}
        </select></div></div>
    <div class="hint">Off keeps the existing client log lines but writes and prunes no telemetry rows.</div>
    ${setCardFoot("saveTelemetry")}`);
}
async function saveTelemetry(btn){
  btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      telemetry_retain_days:Number(document.getElementById("telemetry-retain").value)}}));
    toast("Playback telemetry saved"); setCardSaved(btn);
  }catch(e){ toast(e.message||"Couldn't save"); btn.disabled=false; }
}
async function saveMaintenance(btn){
  btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      probe_retry_mins:Number(document.getElementById("job-probe").value),
      artwork_retry_mins:Number(document.getElementById("job-art").value),
      transcode_cleanup_mins:Number(document.getElementById("job-clean").value),
      scan_on_startup:document.getElementById("job-boot").checked}}));
    toast("Maintenance schedule saved"); setCardSaved(btn);
  }catch(e){ toast(e.message||"Couldn't save"); btn.disabled=false; }
}
async function savePrecache(btn){
  btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      cache_produce_mins:Number(document.getElementById("cache-produce").value),
      cache_max_gb:Number(document.getElementById("cache-gb").value)}}));
    toast("Pre-transcoding saved"); setCardSaved(btn);
  }catch(e){ toast(e.message||"Couldn't save"); btn.disabled=false; }
}
function dvProgressText(progress){
  if(!progress) return "No eligible files found";
  const active=Number(progress.queued||0)+Number(progress.running||0)+Number(progress.verified||0);
  const parts=[];
  if(active) parts.push(`${active} active`);
  if(progress.committed) parts.push(`${progress.committed} converted`);
  if(progress.failed) parts.push(`${progress.failed} failed`);
  if(!parts.length) parts.push(`${progress.eligible||0} eligible`);
  return parts.join(" · ");
}
function dvModeSelect(l,dv){
  const mode=(dv&&dv.library_modes&&dv.library_modes[String(l.id)])||"off";
  const unavailable=!(dv&&dv.capabilities&&dv.capabilities.available);
  const reason=unavailable?(dv&&dv.capabilities&&dv.capabilities.reason)||"dovi_tool or mkvmerge is unavailable":"";
  return `<div class="setfields" title="${esc(reason)}"><div><label for="dv-mode-${l.id}">Dolby Vision conversion</label>
    <select id="dv-mode-${l.id}" aria-label="Dolby Vision conversion mode for ${esc(l.name)}" aria-describedby="dv-progress-${l.id}" data-dv-tools-available="${unavailable?'false':'true'}" onchange="updateDvModeControls(${l.id})">
      ${[["off","Off"],["manual","Manual"],["auto","Automatic"]].map(([v,label])=>`<option value="${v}"${mode===v?' selected':''}${unavailable&&v!=="off"?' disabled':''}>${label}</option>`).join("")}
    </select></div>
    <div><label>&nbsp;</label><button class="ghost sm" id="dv-convert-${l.id}" aria-label="Convert Dolby Vision files in ${esc(l.name)} now" aria-describedby="dv-progress-${l.id}" onclick="convertDvLibrary(${l.id},this)"${unavailable||mode==="off"?' disabled':''}>Convert now</button></div>
    <div><label>&nbsp;</label><span class="muted" id="dv-progress-${l.id}" aria-live="polite" style="font-size:12px">${esc(unavailable?`Unavailable: ${reason}`:dvProgressText(dv.progress_by_library&&dv.progress_by_library[String(l.id)]))}</span></div></div>`;
}
function updateDvModeControls(id){
  const select=document.getElementById(`dv-mode-${id}`);
  if(!select) return;
  const unavailable=select.dataset.dvToolsAvailable!=="true";
  const convert=document.getElementById(`dv-convert-${id}`);
  if(convert) convert.disabled=unavailable||select.value==="off";
}
async function refreshDvConversions(){
  // Stamp before the request so a slow or failed read cannot stack another
  // conversion snapshot behind itself on the two-second settings tick.
  DV_SETTINGS_POLL_AT=Date.now()+DV_PROGRESS_POLL_MS;
  const snapshot=await api("/dv-conversions");
  SETTINGS_DATA.dvConversions=snapshot; SETTINGS_LOADED.add("dvConversions");
  return snapshot;
}
function paintDvConversionProgress(snapshot){
  const unavailable=!(snapshot&&snapshot.capabilities&&snapshot.capabilities.available);
  const reason=unavailable?(snapshot&&snapshot.capabilities&&snapshot.capabilities.reason)||"dovi_tool or mkvmerge is unavailable":"";
  for(const library of SETTINGS_DATA.libs||[]){
    const mount=document.getElementById(`dv-progress-${library.id}`);
    if(mount) mount.textContent=unavailable?`Unavailable: ${reason}`:dvProgressText(snapshot.progress_by_library&&snapshot.progress_by_library[String(library.id)]);
  }
  const recovery=document.getElementById("dv-recovery-guards");
  if(recovery) recovery.innerHTML=dvRecoveryGuardsHtml(snapshot);
}
function noteDvLibraryQueueResult(id,result){
  const queued=Math.max(0,Number(result&&result.queued||0));
  const snapshot=SETTINGS_DATA.dvConversions;
  if(!snapshot||!queued) return false;
  snapshot.progress=snapshot.progress||{};
  snapshot.progress.queued=Number(snapshot.progress.queued||0)+queued;
  snapshot.progress_by_library=snapshot.progress_by_library||{};
  const progress=snapshot.progress_by_library[String(id)]||{};
  progress.queued=Number(progress.queued||0)+queued;
  snapshot.progress_by_library[String(id)]=progress;
  // settingsTick owns the bounded ten-second reads. Make its next two-second
  // tick eligible; the first terminal snapshot turns polling off again.
  DV_SETTINGS_POLL_AT=0;
  return true;
}
async function convertDvLibrary(id,btn){
  btn.disabled=true;
  try{
    const result=await api(`/libraries/${id}/dv-conversions`,{method:"POST",body:{retry_failed:true}});
    noteDvLibraryQueueResult(id,result);
    const more=result.saturated?"; more eligible files may remain — run Convert now again":"";
    toast(`${result.queued||0} Dolby Vision file${Number(result.queued)===1?'':'s'} queued${more}`);
    renderSettings();
  }catch(e){ toast(e.message||"Couldn't queue Dolby Vision conversion"); }
  finally{ btn.disabled=false; }
}
async function saveDvDiskSettings(btn){
  btn.disabled=true;
  try{
    const saved=await api("/settings",{method:"PUT",body:{
      dv_disk_keep_original:document.getElementById("dv-keep-original").checked,
      dv_disk_convert_parallel:Number(document.getElementById("dv-parallel").value)}});
    cacheSettings(saved);
    const snapshot=SETTINGS_DATA.dvConversions;
    if(snapshot){
      snapshot.keep_original=saved.dv_disk_keep_original;
      snapshot.parallel=saved.dv_disk_convert_parallel;
    }
    toast("Dolby Vision conversion settings saved"); setCardSaved(btn);
  }catch(e){ toast(e.message||"Couldn't save Dolby Vision settings"); btn.disabled=false; }
}
function dvRecoveryGuardsHtml(dv){
  const guards=dv&&dv.recovery_guards||{}, summary=guards.summary||{};
  const pending=Number(summary.intent||0)+Number(summary.guard_removed||0);
  const orphaned=Math.max(0,Number(summary.orphaned||0));
  const orphans=Array.isArray(guards.orphans)?guards.orphans:[];
  // The server supplies these from one Store snapshot. Keep the renderer
  // fail-visible as well: a future projection regression must never hide a
  // concrete recovery record merely because its count disagrees.
  const visibleOrphaned=Math.max(orphaned,orphans.length);
  const orphanRows=orphans.map(guard=>`<li><b>${esc(guard.state||"unknown")}</b> · former source <code>${esc(guard.source_path||"unknown")}</code><br>
      <span class="muted">Recorded recovery path <code>${esc(guard.recovery_path||"unknown")}</code> · guard <code>${esc(guard.guard_id||"unknown")}</code></span></li>`).join("");
  const orphanList=visibleOrphaned?`<details open><summary>Review orphaned recovery records (${visibleOrphaned})</summary>
      <p class="hint">An orphan no longer belongs to a conversion row. Leave its recorded path alone while the recovery worker automatically verifies and retires it; deleting it manually can remove the last verified link.</p>
      ${orphanRows?`<ul>${orphanRows}</ul>`:`<div class="problem">The orphan count is non-zero, but this bounded snapshot returned no records.</div>`}
      ${guards.orphans_truncated?`<div class="muted">Showing ${orphans.length} of ${visibleOrphaned} orphaned records in this bounded snapshot.</div>`:""}</details>`
    :`<div class="hint">No orphaned recovery records.</div>`;
  return `<h3 class="section">Recovery guard lifecycle</h3>
    <div aria-live="polite"><b>${Number(summary.active||0)}</b> active guards · <b>${pending}</b> transitions pending · ${Number(summary.intent||0)} recording · ${Number(summary.guard_removed||0)} awaiting scratch cleanup · ${Number(summary.scratch_removed||0)} ready for ledger retirement</div>
    ${orphanList}`;
}
function dvDiskPanel(settings,dv){
  const capabilities=dv&&dv.capabilities;
  const reason=capabilities&&!capabilities.available?capabilities.reason:"";
  const pill=capabilities?(capabilities.available?`<span class="pill ok">tools available</span>`:`<span class="pill bad">conversion unavailable</span>`):"";
  return setCard(`${cardHead("Dolby Vision on disk","Profile 7 files are copied to a verified Profile 8.1 replacement before their public path changes. Each library's mode is set from its row in Libraries.",pill)}
    ${reason?`<div class="setwarn">⚠ <b>Conversion unavailable.</b> ${esc(reason)}</div>`:""}
    ${togRow("dv-keep-original","Keep the Profile 7 original as <code>.p7.orig</code>","Safest, and on by default.",settings&&settings.dv_disk_keep_original)}
    <div class="setfields">
      <label class="schedpair" for="dv-parallel">Parallel files on this node <select id="dv-parallel" class="every">${[1,2,3,4,5,6,7,8].map(n=>`<option value="${n}"${Number(settings&&settings.dv_disk_convert_parallel||1)===n?' selected':''}>${n}</option>`).join("")}</select></label>
    </div>
    <div class="hint">Each file is leased cluster-wide, while parallelism limits work on this node.</div>
    <div id="dv-recovery-guards">${dvRecoveryGuardsHtml(dv)}</div>
    <div class="setfoot"><button class="primary sm" disabled aria-label="Save Dolby Vision conversion settings" onclick="saveDvDiskSettings(this)">Save</button><span class="setstate">Saved</span></div>`);
}
function libRow(l,status,dv){
  const paths=(l.paths||[]).map(p=>`<span class="pathchip">${esc(p)}</span>`).join(" ")||'<span class="muted">—</span>';
  const open=LIB_DRAWER===l.id;
  const provider=l.kind!=="home"&&l.kind!=="books";
  return `<tr id="librow-${l.id}"${open?' class="setopen"':""}><td class="libname"><b>${esc(l.name)}</b></td><td data-label="Kind">${esc(l.anime?'anime':l.kind)}</td>
      <td data-label="Paths"><div class="paths">${paths}</div></td>
      <td class="muted" data-label="Status"><div id="st-${l.id}">${statusText(status[l.id],l.last_scan_at)}</div></td>
      <td class="rowactions"><div class="row" style="flex-wrap:wrap;justify-content:flex-end;gap:5px">
        <button class="ghost sm" onclick="scanLib(${l.id})">Scan</button>
        <button class="ghost sm" onclick="refreshLib(${l.id})"${provider?"":' disabled title="This kind of library has no metadata provider"'} title="Re-fetch all metadata & artwork (incl. season posters)">Refresh art</button>
        <button class="ghost sm" aria-expanded="${open?"true":"false"}"${open?` aria-controls="libdrawer-${l.id}"`:""} onclick="openLibDrawer(${l.id})">Configure ${open?"⌃":"⌄"}</button></div></td></tr>${open?libDrawerHtml(l,dv):""}`;
}
function librariesPanel(libs,status,settings,dv){
  const rows=libs.map(l=>libRow(l,status,dv)).join("")||(LIB_DRAWER==="new"?"":`<tr><td colspan="5" class="muted">No libraries yet.</td></tr>`);
  const tools=`${libs.length?`<button class="ghost sm" onclick="scanAll()" title="Scan every library for new & changed files">Scan all</button>
        <button class="ghost sm" onclick="refreshAll()" title="Re-fetch metadata & artwork for every library">Refresh all art</button>`:""}
        <button class="sm" onclick="openLibDrawer('new')">+ Add library</button>`;
  const lastScan=libs.map(l=>l.last_scan_at||0).reduce((a,b)=>Math.max(a,b),0);
  const summary=libs.length
    ? `${libs.length} librar${libs.length===1?'y':'ies'}${lastScan?` · last scan ${esc(agoLabel(lastScan))}`:""}`
    : "What plurx scans, how often, and what it does with what it finds.";
  // Movies/TV need a TMDB key for artwork; without one the scan finds files but
  // downloads nothing. Surface that right here, where libraries are managed.
  // Home and Books libraries have no provider, so they don't want a key either.
  // Two different moments, and the useful thing to say differs between them.
  //
  // Before any library exists this is *advice*: set the key first and the very
  // first scan brings artwork down with it. After one exists it is a *repair
  // notice*: the scan already ran and found nothing to illustrate it with, so
  // the key alone won't fix what's on screen — a refresh has to follow.
  //
  // The old version only ever showed the second, which meant the one person
  // who could still act on the advice — someone setting the server up, on an
  // empty Libraries tab — was the only one who never saw it.
  const wantsProvider = l => !l.anime && l.kind!=="home" && l.kind!=="books";
  const noKey = settings && !settings.tmdb_configured;
  let warn = "";
  if(noKey && libs.some(wantsProvider)){
    warn = `<div class="setwarn">⚠ <b>No TMDB API key set.</b> Your Movies/TV libraries scan fine, but posters, overviews, and episode data won't download without one. <a href="#/settings/metadata">Add a free key in Metadata</a>, then come back and use <b>Refresh all art</b> — adding the key does not backfill what the earlier scans missed. (Anime uses AniList; Books and home-video libraries use no provider.)</div>`;
  } else if(noKey && !libs.some(wantsProvider)){
    warn = `<div class="setwarn">⚠ <b>Set up a metadata key before you add a Movies or TV library.</b> Without one the scan finds every file and downloads no posters, overviews or episode data — and adding the key afterwards doesn't backfill it, so you'd have to re-run a full refresh over the whole library. <a href="#/settings/metadata">Add a free TMDB key in Metadata</a> first; it takes a minute. (Anime, Books, and home-video libraries need no TMDB key.)</div>`;
  }
  return `${setHead("Libraries",summary,tools)}${warn}${LIB_DRAWER==="new"?libDrawerHtml(null,dv):""}
    <div class="card" style="padding:8px 10px 4px">
      <table><thead><tr><th>Name</th><th>Kind</th><th>Paths</th><th>Status</th><th></th></tr></thead>
      <tbody>${rows}</tbody></table>
    </div>`;
}
// Which row has its drawer open: a library id, "new" for the add form, or
// null. One at a time. Module state, so a status tick cannot shut it; reset
// on every section paint so it never reopens on a later visit.
let LIB_DRAWER=null;
function openLibDrawer(id){
  LIB_DRAWER=(LIB_DRAWER===id)?null:id;
  renderSettings();
  const first=document.querySelector(".setdrawer input"); if(first) first.focus();
}
function libKindSelect(id,sel){
  return `<select id="${id}">${[["movies","Movies"],["shows","TV Shows"],["anime","Anime"],["books","Books &amp; audiobooks"],["home","Home videos &amp; photos"],["recordings","Recordings"]]
    .map(([v,label])=>`<option value="${v}"${sel===v?" selected":""}>${label}</option>`).join("")}</select>`;
}
// The drawer holds everything that configures one library: its identity, its
// schedule, and its Dolby Vision mode. Empty, it is the add form. It is a
// table row so the phone layout reflows it under its parent like any other.
function libDrawerHtml(l,dv){
  const id=l?l.id:"new", sel=l?(l.anime?"anime":l.kind):"movies";
  const identity=`<div class="setfields">
      <div><label for="eln-${id}">Name</label><input id="eln-${id}" value="${esc(l?l.name:"")}" placeholder="Name" style="min-width:180px"></div>
      <div><label for="elk-${id}">Kind</label>${libKindSelect(`elk-${id}`,sel)}</div>
      <div style="flex:1 1 320px"><label for="elp-${id}">Paths <span class="muted">(comma-separated, as the server process sees them)</span></label><input id="elp-${id}" value="${esc(l?(l.paths||[]).join(", "):"")}" placeholder="/media/movies, /media/more" style="min-width:100%"></div>
    </div>`;
  const body=l
    ? `${identity}<div class="setfields">${libScheduleFields(l)}</div>${dvModeSelect(l,dv)}
       <div class="err" id="ele-${id}"></div>
       <div class="setfoot"><button class="sm" onclick="saveLibDrawer(${l.id},this)">Save library</button>
         <button class="ghost sm" onclick="openLibDrawer(null)">Cancel</button>
         <button class="danger sm" onclick="delLib(${l.id})">Delete library…</button></div>`
    : `${identity}<div class="hint">Running in Docker, a path is the one inside the container (e.g. <code>/media/movies</code>), which must be mounted in your compose file. The first scan starts as soon as the library is added.</div>
       <div class="err" id="lerr"></div>
       <div class="setfoot"><button class="sm" onclick="addLib(this)">Add &amp; scan</button>
         <button class="ghost sm" onclick="openLibDrawer(null)">Cancel</button></div>`;
  return l
    ? `<tr class="setdrawer" id="libdrawer-${id}"><td colspan="5">${body}</td></tr>`
    : `<div class="card setdrawer" id="libdrawer-new" style="margin-bottom:14px"><h2 class="section" style="margin:0 0 6px;padding:0;border:0">Add library</h2>${body}</div>`;
}
let KEY_NEEDS_BACKFILL=null;
function keyBackfillHtml(libs){
  if(!KEY_NEEDS_BACKFILL || !libs.length) return "";
  return `<div class="setwarn">⚠ <b>${esc(KEY_NEEDS_BACKFILL)} key saved — but it won't fill in what's already scanned.</b>
    Everything in your ${libs.length} librar${libs.length===1?'y':'ies'} was matched before the key existed, so posters and overviews stay blank until they're re-fetched. This runs in the background and can take a while on a big library.
    <div style="margin-top:8px"><button class="ghost sm" onclick="backfillFromKey()">Refresh all art now</button>
    <button class="ghost sm" onclick="KEY_NEEDS_BACKFILL=null;renderSettings()">Not now</button></div></div>`;
}
async function backfillFromKey(){
  KEY_NEEDS_BACKFILL=null;
  const libs=(SETTINGS_DATA&&SETTINGS_DATA.libs)||[];
  await Promise.all(libs.map(l=>api(`/libraries/${l.id}/refresh`,{method:"POST"}).catch(()=>{})));
  toast("Refreshing metadata & artwork for every library…");
  setSettingsTab("libraries");
  setTimeout(settingsTick,400);
}
function metadataPanel(settings,readiness){
  const libs=(SETTINGS_DATA&&SETTINGS_DATA.libs)||[];
  const keyCard=(id,title,sub,configured,value,link,saveFn,errId)=>setCard(`${cardHead(title,sub,`<span class="pill ${configured?"ok":"bad"}">${configured?"configured":"not set"}</span>`)}
      <div class="setfields">
        <div style="flex:1 1 320px"><label for="${id}">API key${configured?` <span class="muted">— click to reveal, or type a new key to replace it</span>`:` <span class="muted">— free from ${link}</span>`}</label>
          <input id="${id}" placeholder="${esc(title)} API key" type="password" autocomplete="off" value="${esc(value||'')}" onfocus="this.type='text'" onblur="this.type='password'" style="min-width:100%"></div>
        <div class="err" id="${errId}"></div>
      </div>
      ${setCardFoot(saveFn)}`);
  return `${setHead("Metadata","Metadata providers, classification and search.")}
    <div id="metadata-libraries">${keyBackfillHtml(libs)}</div>
    ${keyCard("tk","TMDB","Posters, overviews and episode data for Movies and TV.",settings.tmdb_configured,settings.tmdb_api_key,`<a href="https://www.themoviedb.org/settings/api" target="_blank" rel="noopener">themoviedb.org</a>`,"saveTmdb","terr")}
    ${keyCard("ok","OMDb","Rotten Tomatoes, Metacritic and IMDb ratings on the item page — TMDB doesn't carry these.",settings.omdb_configured,settings.omdb_api_key,`<a href="https://www.omdbapi.com/apikey.aspx" target="_blank" rel="noopener">omdbapi.com</a>`,"saveOmdb","oerr")}
    <div class="card">${cardHead("Providers with no key","Anime uses AniList. Books and home-video libraries use no provider.")}</div>${searchSettingsCard(readiness)}`;
}
// The section for services plurx talks to. Trakt is a two-way sync, Curator a
// calendar seam; both are the opposite direction of the Metadata providers,
// which is why they no longer share a page with them.
function integrationsPanel(settings,trakt){
  return `${setHead("Integrations","Other services plurx talks to.")}${traktCardHtml(trakt)}${monarrCardHtml()}<div class="card">${cardHead("OpenSubtitles","Find and download missing movie and episode subtitles.")}<div style="padding:16px"><button class="sm" onclick="openSubtitleProvider(this)">Configure OpenSubtitles</button><div id="subtitle-provider-form"></div></div></div>`;
}

async function openSubtitleProvider(button){
  const panel=document.getElementById("subtitle-provider-form");
  button.disabled=true;
  try{
    const settings=await api("/subtitle-provider");
    if(!panel.isConnected)return;
    panel.innerHTML=`<p>${settings.configured?"OpenSubtitles is configured. Leave secrets blank to keep them.":"Add an OpenSubtitles API key to enable subtitle downloads."} <a href="https://www.opensubtitles.com/consumers" target="_blank" rel="noopener">Create an API key</a>.</p>
      <label for="os-key">API key</label><input id="os-key" type="password" autocomplete="off">
      <label for="os-user">Account username (optional)</label><input id="os-user" autocomplete="off" value="${esc(settings.username||"")}">
      <label for="os-password">Account password (optional)</label><input id="os-password" type="password" autocomplete="new-password">
      <label><input id="os-auto" type="checkbox"${settings.automatic?" checked":""}> Automatically download missing subtitles when the file matches</label>
      <label for="os-languages">Automatic languages (up to three codes, separated by commas)</label><input id="os-languages" value="${esc((settings.languages||["en"]).join(", "))}" placeholder="en, fr">
      <p class="muted">Downloads use the provider’s allowance. An account can provide a higher allowance.</p>
      <button class="sm" onclick="saveSubtitleProvider(false)">Save</button>
      <button class="ghost sm" onclick="saveSubtitleProvider(true)">Disable and clear credentials</button><div id="os-status" role="status"></div>`;
  }catch(e){if(panel.isConnected)panel.textContent=e.message||"Could not load OpenSubtitles settings.";}
  finally{button.disabled=false;}
}

async function saveSubtitleProvider(clear){
  const status=document.getElementById("os-status");
  const key=document.getElementById("os-key"),password=document.getElementById("os-password");
  const body=clear?{api_key:"",username:"",password:"",automatic:false}:{username:document.getElementById("os-user").value.trim(),automatic:document.getElementById("os-auto").checked,languages:document.getElementById("os-languages").value.split(",").map(v=>v.trim()).filter(Boolean)};
  if(!clear&&key.value.trim())body.api_key=key.value.trim();
  if(!clear&&password.value)body.password=password.value;
  status.textContent="Saving…";
  try{await api("/subtitle-provider",{method:"PUT",body});key.value="";password.value="";status.textContent=clear?"OpenSubtitles disabled.":"Saved.";}
  catch(e){status.textContent=e.message||"Could not save OpenSubtitles settings.";}
}
function presetOpts(pairs, cur, label){
  cur=(cur==null?"":String(cur));
  const opts=pairs.slice();
  if(cur!=="" && !opts.some(([v])=>v===cur)) opts.unshift([cur, label?label(cur):cur]);
  return opts.map(([v,l])=>`<option value="${esc(v)}" ${cur===v?'selected':''}>${esc(l)}</option>`).join("");
}
function analysisSettingsPanel(settings,snapshot){
  const card=setCard(`${cardHead("Index analysis","Foreground playback always wins: analysis only runs while the media worker is idle.")}
      ${togRow("an-enabled","Durable analysis queue and shared index cache","Manual Analyze and Retry actions use the queue; background analysis discovers eligible files on the cadence below. Pausing either keeps completed indexes; titles without one use Live HLS when live recovery is on in Playback.",settings.vod_index_cluster_cache)}
      <div class="setfields">
        <div><label for="an-every">Background analysis</label><select id="an-every" style="min-width:220px">${presetOpts([["0","Paused"],["15","Every 15 minutes"],["60","Hourly"],["360","Every 6 hours"],["1440","Daily"]],settings.vod_index_mins)}</select></div>
      </div>
      <h2 class="section">Retry policy</h2>
      <div class="setfields">
        <div><label for="an-attempts">Maximum attempts</label><input id="an-attempts" type="number" min="1" max="20" value="${esc(settings.analysis_max_attempts||5)}"></div>
        <div><label for="an-lease">Claim lease (seconds)</label><input id="an-lease" type="number" min="15" max="600" value="${esc(settings.analysis_lease_secs||60)}"></div>
        <div><label for="an-backoff-base">Initial backoff (seconds)</label><input id="an-backoff-base" type="number" min="1" max="300" value="${esc(settings.analysis_backoff_base_secs||5)}"></div>
        <div><label for="an-backoff-max">Maximum backoff (seconds)</label><input id="an-backoff-max" type="number" min="1" max="3600" value="${esc(settings.analysis_backoff_max_secs||300)}"></div>
      </div>
      <div class="err" id="an-error"></div>
      ${setCardFoot("saveAnalysisSettings")}`);
  return `${setHead("Analysis","Content analysis verifies each source and builds the fragment index VOD HLS plays from.",`<a class="ghost sm" href="#/analysis" style="text-decoration:none">Open analysis workspace</a>`)}<div id="analysis-settings-summary">${analysisSummaryCard(snapshot||{enabled:false},"settings")}</div>
    ${card}
    <div class="card"><h2 class="section" style="margin-top:0">How a file becomes VOD HLS ready</h2>
      <div class="analysis-diag-grid"><b>1 · Verify</b><span>Read and hash the exact source without blocking playback.</span><b>2 · Index</b><span>Build a complete fragment timeline on an eligible media worker.</span><b>3 · Publish</b><span>Store the content-addressed artifact and mark the file VOD HLS ready.</span></div>
    </div>`;
}
function playbackPanel(settings,readiness){
  const vodWorking=Number(settings.vod_working_set_bytes)||(8*1024*1024*1024);
  const vodMaterialize=Number(settings.vod_materialize_budget_secs)||30;
  const vodBlockedGets=Number(settings.vod_blocked_get_cap)||64;
  const active=SERVER&&Number.isFinite(SERVER.active_transcodes)?`<span class="pill">${SERVER.active_transcodes} active stream${SERVER.active_transcodes===1?"":"s"}</span>`:"";
  const defaults=setCard(`${cardHead("Defaults for every player","All players · Applies to new playback on web, Apple and Android. Anime dual-audio still prefers the original audio with subtitles.")}
      <div class="setfields">
        <div><label for="pal">Audio language</label><select id="pal">${langOpts(settings.default_audio_lang)}</select></div>
        <div><label for="psl">Subtitle language</label><select id="psl">${langOpts(settings.default_sub_lang)}</select></div>
        <div><label for="psm">Show subtitles</label><select id="psm" style="min-width:230px">
          <option value="auto" ${settings.sub_mode==='auto'?'selected':''}>Auto — only for foreign audio</option>
          <option value="always" ${settings.sub_mode==='always'?'selected':''}>Always on</option>
          <option value="off" ${settings.sub_mode==='off'?'selected':''}>Off</option></select></div>
      </div>
      <div class="err" id="perr"></div>
      ${setCardFoot("savePlaybackDefaults")}`);
  const streaming=setCard(`${cardHead("Streaming","Server delivery · Changes apply to new sessions. Existing playback keeps its current settings.",active)}
      ${togRow("pvod","VOD HLS — a fixed, seekable timeline","Preferred. Needs the file's analysis index; schedules and queue health are in Analysis.",settings.vod_presentation)}
      <div class="setfields">
        <div><label for="pvws">VOD working set</label><select id="pvws" style="min-width:190px">${presetOpts([[String(2*1024**3),"2 GB"],[String(4*1024**3),"4 GB"],[String(8*1024**3),"8 GB — recommended"],[String(16*1024**3),"16 GB"],[String(32*1024**3),"32 GB"]],vodWorking)}</select></div>
        <div><label for="pvmb">Producer deadline</label><select id="pvmb" style="min-width:190px">${presetOpts([["30","30 seconds — recommended"],["45","45 seconds"],["60","60 seconds"]],vodMaterialize,v=>`${v} seconds`)}</select></div>
        <div><label for="pvbg">Waiting segment requests</label><select id="pvbg" style="min-width:230px">${presetOpts([["16","16 — a few viewers"],["64","64 — recommended"],["128","128"],["256","256 — a busy household"]],vodBlockedGets,v=>`${v} requests`)}</select></div>
        <div><label for="prr">Stream pace after the first 30 s</label><select id="prr" style="min-width:230px">
          <option value="2" ${settings.stream_readrate==='2'?'selected':''}>2× — gentlest</option>
          <option value="4" ${settings.stream_readrate==='4'?'selected':''}>4× — recommended</option>
          <option value="8" ${settings.stream_readrate==='8'?'selected':''}>8×</option>
          <option value="0" ${settings.stream_readrate==='0'?'selected':''}>Unlimited — wired networks only</option></select></div>
      </div>
      <div class="hint">The first 30 seconds always arrive at full speed so starting and seeking stay instant; the pace applies after that. Unlimited lets one stream take the entire link, which on Wi-Fi can crowd out everything else on the network.</div>
      <h2 class="section">Transcode buffering</h2>
      <p class="sub" style="margin:0">How much of a head start a transcoded or repackaged stream builds before it settles down. The head start <em>is</em> the buffer a player has to survive a network hiccup: raise it if 4K stutters, lower it if streams eat too much disk. The transcoder pauses at the buffer limit and resumes when you catch up.</p>
      <div class="setfields">
        <div><label for="phb">Head start</label><select id="phb" style="min-width:200px">${presetOpts([["30","30 seconds"],["90","90 seconds — recommended"],["180","3 minutes"],["0","None"]], settings.hls_burst_secs, v=>`${v} seconds`)}</select></div>
        <div><label for="phr">Then pace at</label><select id="phr" style="min-width:200px">${presetOpts([["1.5","1.5× — gentlest"],["2","2× — recommended"],["4","4×"],["0","Unlimited"]], settings.hls_readrate, v=>`${v}×`)}</select></div>
        <div><label for="pha">Buffer limit</label><select id="pha" style="min-width:200px">${presetOpts([["60","1 minute"],["180","3 minutes — recommended"],["600","10 minutes"],["0","No limit — needs disk"]], settings.hls_ahead_max_secs, v=>`${v} seconds`)}</select></div>
      </div>
      <div class="err" id="serr"></div>
      ${setCardFoot("saveStreaming")}`);
  const local=setCard(`${cardHead("This browser only","Saved in this browser. Nothing here reaches the server or another viewer.",`<span class="pill acc">local</span>`)}
      ${togRow("autonext","Auto-play the next episode when one finishes","",autoNextOn(),'onchange="setAutoNext(this.checked)"')}
      <div class="tog"><span>Measured playback limits<small>When an original stream loses visible frames, this browser remembers that exact media load — codec/profile, resolution, bit depth, dynamic range, and a 10 Mb/s bitrate band — and lets <b>Auto</b> transcode it next time instead of stuttering first. A measurement expires after 30 days; <b>Quality → Original</b> always bypasses it.</small></span></div>
      <div class="row" id="dlrow">${decodeLimitsSummary()}</div>`,{local:true});
  return `${setHead("Playback","How streams start, how they are delivered, and what every player picks by default.")}${defaults}${local}<details class="setdetails"><summary>Advanced server delivery</summary>${streaming}${liveHlsRecoveryCard(settings,readiness)}${playbackProtocolCard(settings,readiness)}</details>`;
}

function searchSettingsCard(readiness){
  return setCard(`${cardHead("Search and classification","Local text search, channel rules and automatic metadata labels are always available. Optionally add search by meaning using an embedded model.")}
    <p class="hint">Semantic search downloads approximately 91 MB once per node and uses additional CPU and memory for indexing.</p>
    <details class="setdetails"><summary>Search diagnostics</summary><div class="setdetails-body">
      ${devReq(readiness,"embedded_semantic_search","runtime","Embedded CPU runtime","Included in ordinary builds; no inference service is required.")}
      ${devReq(readiness,"embedded_semantic_search","model","Verified model loaded","The model is downloaded when semantic search is enabled.")}
      ${devReq(readiness,"embedded_semantic_search","index","Local semantic index","Readiness is advisory and does not override your saved choice.")}
    </div></details>
    <button class="ghost" onclick="showSearchSettings()">Search settings</button>`);
}

function windowsServerCard(settings,readiness){
  return setCard(`${cardHead("Windows server","Native runtime, media engine and measured hardware status.",`<span class="pill">advisory</span>`)}
      ${togRow("dvwin","Enable permanent Dolby Vision conversion","Allows Profile 7 media to be converted to a verified Profile 8.1 replacement on Windows. This saved choice is authoritative; readiness below is advice only.",settings.dolby_vision_convert)}
      <div class="hint">These readings never enable or disable a feature. A failed or absent hardware probe keeps the ordinary software encoder available.</div>
      <details class="setdetails"><summary>Runtime and hardware readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"windows_server","native_runtime","Native Windows runtime","The Windows build requires x64 Windows 10 1809 or Server 2019+, NTFS/ReFS managed storage, Job Objects and the embedded long-path manifest.")}
      ${devReq(readiness,"windows_server","ffmpeg_runtime","FFmpeg runtime","Point PLURX_FFMPEG and PLURX_FFPROBE at the packaged jellyfin-ffmpeg build. The exact binaries are measured at startup.")}
      ${devReq(readiness,"windows_server","nvenc","NVIDIA NVENC","NVENC is admitted only when the installed driver and this FFmpeg pass the ordinary startup and forced-IDR probes.")}
      ${devReq(readiness,"windows_server","qsv","Intel Quick Sync","Quick Sync is admitted only when D3D11 device initialization, encode and forced-IDR probes succeed on this node.")}
      <p class="devcheck-note">Advisory only. Unmet hardware rows mean software fallback, not a hidden gate.</p>
      </div></details>
      <div class="err" id="winerr" role="alert"></div>${setCardFoot("saveWindowsCompatibility")}`);
}
