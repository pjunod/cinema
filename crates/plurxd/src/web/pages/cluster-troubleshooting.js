"use strict";
// ---- the page --------------------------------------------------------------
// Header, status strip, alerts, the nodes table, then settings beside the
// store, cluster-wide maintenance beside backup, and the log. Each fact is
// stated once: the strip summarizes, the table itemizes, and an alert appears
// only when something needs action.
function clusterStrip(cluster,ops){
  const cells=PlurxClusterPanel.clusterStripCells(clenv(),cluster,ops);
  const tone=cells[0].tone||"calm";
  return `<div class="clcard clstrip" data-tone="${tone}">${cells.map((cell,index)=>`<div data-cell="${cell.key}"><div class="clk">${esc(cell.label)}</div>
    <div class="clv${index&&cell.tone?` ${clusterTone(cell.tone)}`:""}">${index?"":`<span class="cldot ${tone}"></span>`}${cell.value}</div><div class="cls">${cell.sub}</div></div>`).join("")}</div>`;
}
function clusterAlerts(cluster,ops){
  const P=PlurxClusterPanel;
  const kinds=P.clusterAlertKinds(cluster,ops,{refusal:Boolean(CLUSTER_REFUSAL)});
  const nodes=(cluster&&!cluster.unavailable&&cluster.nodes)||[];
  const active=nodes.find(n=>n.maintenance);
  const plain=(tone,body,controls)=>`<div class="clalert${tone==="bad"?" bad":tone==="good"?" good":""}" role="status"><div>${body}</div>${controls?`<div class="row">${controls}</div>`:""}</div>`;
  let opsMount=false;
  const html=kinds.map(({kind,tone})=>{
    if(kind==="recovery") return clusterRecoveryAlert(cluster,P.clusterRecoveryState(cluster),active,ops);
    if(kind==="maintenance") return clusterMaintenanceAlert(active,ops,cluster.local_node_id);
    if(kind==="restart") return clusterRestartPreparationHtml(P.clusterDirectOperationRow(ops,cluster.local_node_id));
    if(kind==="voter_down") return plain(tone,P.clusterVoterDownText(clenv(),cluster,ops));
    if(kind==="restart_blocked") return plain(tone,`<b>Do not restart another voter.</b> Rolling restart is blocked: ${
      P.clusterBlockerText(clenv(),ops.verdict.blockers,cluster)||"a direct safety condition is missing"}.`);
    if(kind==="ops_unavailable"){ opsMount=true; return `<div id="cluster-operations">${clusterOperationsUnavailable(esc(ops.message||"request failed"))}</div>`; }
    if(kind==="reconfiguration"){
      // The deliverable of the two-voter state is these words, unshortened.
      const state=P.clusterStateView(cluster);
      return plain(tone,`<b>${esc(state.title)}.</b> ${esc(state.body)}`);
    }
    if(kind==="refusal") return clusterRefusalHtml();
    if(kind==="roster_failed"){
      const state=P.clusterStateView(cluster);
      return plain(tone,`<b>${esc(state.title)}.</b> ${esc(state.body)}`,`<button class="ghost sm" onclick="retryCluster()">Retry roster</button>`);
    }
    return "";
  }).join("");
  // A failed manual status refresh reports into this mount, so it exists
  // whether or not the stored status is a failure.
  return html+(opsMount||!nodes.length?"":`<div id="cluster-operations"></div>`);
}
function clusterLogCard(){
  return `<section class="clcard" id="cluster-log"><div class="clcardhead"><h2 title="Membership, Hiqlite, and Raft events are kept here instead of the general System log.">Cluster log</h2>
    <div class="row"><select id="clloglvl" onchange="refreshClusterLogs()">
        <option value="info" selected>Info+</option><option value="warn">Warnings+</option>
        <option value="error">Errors</option><option value="debug">Debug+</option></select>
      <label class="chk"><input type="checkbox" id="cllogauto" checked> auto-refresh</label>
      <button class="ghost sm" onclick="refreshClusterLogs()">Refresh</button></div></div>
    <div class="logbox cllog" id="cllogbox">Loading…</div></section>`;
}
function clusterPanel(d){
  // Belt and braces: viewSettings already turns a non-admin away at the door,
  // and the endpoints behind this are admin-only on their own account.
  if(!ME||!ME.is_admin) return "";
  if(!settingsClusterEnabled(d)) return setHead("Cluster","Clustering is not enabled.");
  const cluster=d.cluster;
  if(!cluster) return `${setHead("Cluster","")}<div class="empty">Loading…</div>`;
  const clustered=!cluster.unavailable;
  const notClustered=cluster.unavailable&&cluster.code==="membership_unavailable";
  const ops=clustered?d.clusterOps:null;
  const nodes=clustered?(cluster.nodes||[]):[];
  // One lag answer, not a second competing one: the Replicated database card
  // is the only place in Cinema that reports it.
  const replication=(clustered&&cluster.replication)||(d.sys&&d.sys.replication)
    ||(notClustered?PlurxClusterPanel.clusterSqliteReplication():null);
  const summary=clustered
    ? `${nodes.length} node${nodes.length===1?"":"s"} · <span id="cl-sampled">${esc(PlurxClusterPanel.clusterSampledText(ops))}</span>`
    : notClustered?"Not clustered":"Roster unavailable";
  const head=setHead("Cluster",summary,`<button class="ghost sm" onclick="refreshClusterPage(this)">Refresh</button>`);
  const store=clusterDatabasePanel(cluster,replication,ops);
  const settings=clusterSettingsSection(d);
  const backup=clusterBackupCard(d.settings,d.developerReadiness||DEVELOPER_READINESS);
  if(notClustered){
    const state=PlurxClusterPanel.clusterStateView(cluster);
    return `<div class="clpage">${head}<div class="clstate calm"><h3>${esc(state.title)}</h3><p>${esc(state.body)}</p></div>${clusterAlerts(cluster,null)}
      <div class="cltwo">${settings}<div class="clstack">${store}${backup}</div></div>${clusterLogCard()}</div>`;
  }
  return `<div class="clpage">${head}${clusterStrip(cluster,ops)}${clusterAlerts(cluster,ops)}
    ${clustered?clusterNodesCard(cluster,ops):""}
    <div class="cltwo">${settings}<div class="clstack">${store}</div></div>
    <div class="cltwo">${clustered?clusterMaintenanceCard(cluster,ops):backup}${clustered?`<div class="clstack">${backup}</div>`:""}</div>
    ${clusterLogCard()}</div>`;
}
function retryCluster(){
  SETTINGS_LOADED.delete("cluster"); CLUSTER_LOADED=false;
  viewSettings(++PAGE_RENDER_GENERATION,false);
}
// The header's Refresh is the tick's gate, on demand: the roster and the
// direct status through the reads they already have, painted once through the
// preserving repaint. No reload through the loading shell and no generation
// bump, so the token form, the log controls, focus, open drawers and
// expansions, an unsaved setting, and an operation already in flight all keep
// what they had. An open dialog holds both samples, as the tick does. The
// status read stamps the gate, so the tick does not repeat it at once.
async function refreshClusterPage(btn){
  const generation=PAGE_RENDER_GENERATION;
  if(btn) btn.disabled=true;
  try{
    const cluster=await SETTINGS_ENDPOINTS.cluster();
    if(!settingsCurrent(generation,"cluster")) return;
    // A single machine has no fan-out to collect (see settingsTick).
    const ops=cluster&&!cluster.unavailable?await SETTINGS_ENDPOINTS.clusterOps():null;
    if(!settingsCurrent(generation,"cluster")) return;
    if(clusterRepaintDeferred()){ patchClusterReadingAge(); return; }
    SETTINGS_DATA.cluster=cluster; SETTINGS_LOADED.add("cluster"); CLUSTER_LOADED=true;
    if(ops){ SETTINGS_DATA.clusterOps=ops; SETTINGS_LOADED.add("clusterOps"); }
    repaintClusterPreserving(renderSettings);
  }catch(error){
    if(error&&error.status===401) return;
  }finally{ if(btn) btn.disabled=false; }
}
async function setNodeMaintenance(nodeId,enter){
  if(enter&&!confirm(`Enter maintenance on ${nodeId}?\n\nNew work will be fenced and existing streams will drain. If this node is leader, leadership moves before it is marked ready.`)) return;
  CLUSTER_REFUSAL=null;
  const generation=PAGE_RENDER_GENERATION;
  let changed=false;
  try{
    const cluster=await api(`/cluster/nodes/${encodeURIComponent(nodeId)}/maintenance`,{method:enter?"POST":"DELETE",body:enter?{}:undefined});
    if(!settingsCurrent(generation,"cluster")) return;
    SETTINGS_DATA.cluster=cluster; SETTINGS_LOADED.add("cluster"); CLUSTER_LOADED=true;
    SETTINGS_LOADED.delete("clusterOps"); delete SETTINGS_DATA.clusterOps; changed=true;
    toast(enter?"Maintenance requested":"Node returned to service");
  }catch(e){
    if(e&&e.status===401) return;
    if(!settingsCurrent(generation,"cluster")) return;
    CLUSTER_REFUSAL={node_id:nodeId,code:e.code,message:e.message};
  }
  if(settingsCurrent(generation,"cluster")){
    renderSettings();
    if(changed) await refreshClusterOperations();
  }
}
function openElectionDialog(){
  const dialog=document.getElementById("clelection");
  if(dialog) dialog.showModal();
}
async function forceClusterElection(btn){
  CLUSTER_REFUSAL=null;
  const generation=PAGE_RENDER_GENERATION;
  btn.disabled=true; btn.textContent="Electing…";
  try{
    const cluster=await api("/cluster/election",{method:"POST",body:{}});
    if(!settingsCurrent(generation,"cluster")) return;
    SETTINGS_DATA.cluster=cluster; SETTINGS_LOADED.add("cluster"); CLUSTER_LOADED=true;
    toast("Leader election completed");
  }catch(e){
    if(e&&e.status===401) return;
    if(!settingsCurrent(generation,"cluster")) return;
    CLUSTER_REFUSAL={node_id:"Cluster",code:e.code,message:e.message};
  }
  if(settingsCurrent(generation,"cluster")) renderSettings();
}
function downloadClusterDiagnostics(){
  const cluster=SETTINGS_DATA&&SETTINGS_DATA.cluster;
  if(!cluster) return;
  const logs=document.getElementById("cllogbox")?.textContent||"Cluster log is not loaded.";
  const blob=new Blob([JSON.stringify({captured_at:new Date().toISOString(),cluster,logs},null,2)],{type:"application/json"});
  const url=URL.createObjectURL(blob),a=document.createElement("a");
  a.href=url; a.download=`plurx-cluster-diagnostics-${Date.now()}.json`; a.click();
  setTimeout(()=>URL.revokeObjectURL(url),0);
}
function toggleRecoveryChecklist(btn){
  const checklist=document.getElementById("clpreserve");
  if(!checklist) return;
  checklist.hidden=!checklist.hidden;
  btn.textContent=checklist.hidden?"Preservation checklist":"Hide checklist";
}
// A refused operation is kept in the alert slot with the sentence explaining
// what to do next, because a toast is 2.2 seconds and these answers are
// paragraphs. It stays until dismissed or the next operation starts.
function clusterRefusalHtml(){
  if(!CLUSTER_REFUSAL) return "";
  // Announced once, when it first appears; every later repaint redraws the
  // same refusal without asking a screen reader to read it out again.
  const announce=!CLUSTER_REFUSAL.announced;
  CLUSTER_REFUSAL.announced=true;
  return `<div class="clalert bad"${announce?' role="alert"':""}><div><b>Cluster operation for ${esc(CLUSTER_REFUSAL.node_id)} was refused.</b> ${
    esc(PlurxClusterPanel.membershipRefusalText(CLUSTER_REFUSAL.code,CLUSTER_REFUSAL.message))}</div>
    <div class="row"><button class="ghost sm" onclick="dismissClusterRefusal()">Dismiss</button></div></div>`;
}
function dismissClusterRefusal(){ CLUSTER_REFUSAL=null; repaintClusterPreserving(renderSettings); }
// Minting is separated from showing because the token exists exactly once: the
// server hands back the only copy and keeps nothing but its digest.
function joinPanel(){
  if(CLUSTER_TOKEN) return joinTokenHtml(CLUSTER_TOKEN);
  return `<div class="row" style="gap:10px;align-items:center">
      <span class="schedpair">Token valid for
        <select id="clttl" style="max-width:130px;font-size:12px;padding:3px 6px">
          <option value="600" selected>10 minutes</option>
          <option value="1800">30 minutes</option>
          <option value="3600">1 hour</option></select></span>
      <span class="schedpair">Role
        <select id="clrole" style="max-width:150px;font-size:12px;padding:3px 6px">
          <option value="voter" selected>Voting member</option>
          <option value="learner">Read worker</option></select></span>
      <button onclick="mintJoinToken(this)">Create a join token</button></div>
    <div class="hint">A join token is single-use and short-lived, and it is shown once. Create it when the
      other machine is ready to start, not before.</div>`;
}
function leavePanel(localNodeId){
  if(CLUSTER_LEAVING) return `<div class="clstate calm"><h3>Leaving the cluster</h3><p>The membership change committed. This
      Cinema node is draining connections and will shut down.</p></div>`;
  if(!localNodeId) return `<div class="clstate warn"><h3>Local node identity unavailable</h3><p>Refresh the roster before
      leaving. The server will not guess which backend this destructive action should remove.</p></div>`;
  return `<button class="ghost sm" onclick='leaveCluster(this,${esc(JSON.stringify(localNodeId))})'>Leave this cluster</button>
    <div class="hint">A graceful leave is permanent, not an update or restart action. This node resolves
      its owned offline work and coordinates a safe membership change with the surviving quorum. It then
      drains and shuts down. Rejoining this machine requires a fresh data directory and
      a new join token.</div>`;
}
// Deliberately loud. Anyone holding the complete token can join a node to this
// cluster until it is redeemed or expires, so this says that plainly rather
// than presenting it as an identifier to leave on a screen.
function joinTokenHtml(issued){
  const mins=Math.max(0,Math.round((issued.expires_at-Date.now())/60000));
  const role=issued.role==="learner"?"a non-voting read worker":"a voting member";
  return `<div class="jointoken" id="cltoken">${esc(issued.token)}</div>
    <div class="hint"><b>Shown once.</b> plurx keeps only this token's digest, so closing this is the end of
      it — create another if you lose it. Until it is redeemed or expires it carries the complete authority
      to join a node to this cluster: treat it like a password, and do not paste it into a chat, an issue, or
      a shell transcript. It expires in about ${mins} minute${mins===1?"":"s"}
      and admits exactly one node as ${role}, Raft id ${esc(String(issued.raft_id))}.</div>
    <div class="hint">On the new machine — a fresh data directory with no existing plurx database — save this
      into an owner-only file, point <code>cluster.join_token_file</code> at it, give that node its own
      reachable <code>cluster.advertise_host</code>, and start plurxd. It joins on its own and deletes the
      file once it has finished. Full runbook: docs/OPERATIONS.md.</div>
    <div class="row" style="gap:10px;margin-top:10px">
      <button class="ghost sm" onclick="copyJoinToken()">Copy</button>
      <button class="ghost sm" onclick="clearJoinToken()">Done — clear it</button></div>`;
}
async function mintJoinToken(btn){
  const generation=PAGE_RENDER_GENERATION;
  const sel=document.getElementById("clttl");
  const expires_in_seconds=sel?Number(sel.value):600;
  const role=document.getElementById("clrole")?.value||"voter";
  const was=btn.textContent; btn.disabled=true; btn.textContent="Creating…";
  try{
    const token=role==="learner"
      ?await api("/cluster/learner-join-tokens",{method:"POST",body:{expires_in_seconds}})
      :await api("/cluster/join-tokens",{method:"POST",body:{expires_in_seconds}});
    if(!settingsCurrent(generation,"cluster")) return;
    CLUSTER_TOKEN={...token,role};
    CLUSTER_REFUSAL=null;
    repaintClusterPreserving(renderSettings);
  }catch(e){
    if(e&&e.status===401) throw e;
    if(!settingsCurrent(generation,"cluster")) return;
    btn.disabled=false; btn.textContent=was;
    toast(e.message||"Couldn't create a join token");
  }
}
function clearJoinToken(){ forgetJoinToken(); repaintClusterPreserving(renderSettings); }
async function copyJoinToken(){
  if(!CLUSTER_TOKEN) return;
  try{
    await navigator.clipboard.writeText(CLUSTER_TOKEN.token);
    toast("Join token copied");
  }catch(e){
    // Plain HTTP is not a secure context, so a LAN install has no clipboard
    // API at all. Say which gesture works instead of failing quietly.
    toast("This browser won't copy over plain HTTP — click the token to select it");
  }
}
// Removal is not reversible for the machine on the other end: its data
// directory is tombstoned and rejoining means discarding it. The confirm says
// so, because the roster gives no other hint that Remove is that heavy.
async function removeNode(nodeId){
  if(!confirm(`Remove ${nodeId} from this cluster?\n\nThat machine's plurx data directory is tombstoned. `+
    `Bringing it back means stopping it, discarding that directory, and joining it again with a new token. `+
    `Media files are not touched.`)) return;
  CLUSTER_REFUSAL=null;
  const generation=PAGE_RENDER_GENERATION;
  try{
    const cluster=await api(`/cluster/nodes/${encodeURIComponent(nodeId)}`,{method:"DELETE"});
    SETTINGS_LOADED.delete("cluster"); CLUSTER_LOADED=false;
    if(!settingsCurrent(generation,"cluster")) return;
    SETTINGS_DATA.cluster=cluster; SETTINGS_LOADED.add("cluster"); CLUSTER_LOADED=true;
    toast("Node removed");
  }catch(e){
    if(e&&e.status===401) return;
    if(!settingsCurrent(generation,"cluster")) return;
    CLUSTER_REFUSAL={node_id:nodeId, code:e.code, message:e.message};
    // Ordinary policy refusals belong beside the roster that produced them.
    // A not-found refusal proves that roster lost a race, so paint the reason
    // first and then replace only that cached roster with a guarded fresh read.
    renderSettings();
    if(e.code==="cluster_node_not_found"){
      SETTINGS_LOADED.delete("cluster"); CLUSTER_LOADED=false;
      loadSettingsKey("cluster",generation).then(()=>{
        if(settingsCurrent(generation,"cluster")) renderSettings();
      }).catch(()=>{});
    }
    return;
  }
  if(settingsCurrent(generation,"cluster")) renderSettings();
}
async function promoteNode(nodeId){
  if(!confirm(`Promote ${nodeId} to a voting member?\n\nThis increases quorum size only after the node proves zero apply lag and voter-grade durable storage headroom.`)) return;
  CLUSTER_REFUSAL=null;
  const generation=PAGE_RENDER_GENERATION;
  try{
    const cluster=await api(`/cluster/nodes/${encodeURIComponent(nodeId)}/promote`,{method:"POST",body:{}});
    if(!settingsCurrent(generation,"cluster")) return;
    SETTINGS_DATA.cluster=cluster; SETTINGS_LOADED.add("cluster"); CLUSTER_LOADED=true;
    toast("Learner promoted to voter");
  }catch(e){
    if(e&&e.status===401) return;
    if(!settingsCurrent(generation,"cluster")) return;
    CLUSTER_REFUSAL={node_id:nodeId,code:e.code,message:e.message};
  }
  if(settingsCurrent(generation,"cluster")) renderSettings();
}
async function leaveCluster(btn,nodeId){
  if(!confirm(`Permanently remove this Cinema node from the cluster and shut it down?\n\n`+
    `Do not use this for an update or restart. Rejoining requires discarding this node's old Cinema `+
    `data directory and using a new join token. Media files are not touched.`)) return;
  CLUSTER_REFUSAL=null;
  const generation=PAGE_RENDER_GENERATION;
  const was=btn.textContent; btn.disabled=true; btn.textContent="Leaving…";
  try{
    await api("/cluster/leave",{method:"POST",body:{node_id:nodeId}});
    if(!settingsCurrent(generation,"cluster")) return;
    CLUSTER_LEAVING=true;
  }catch(e){
    if(e&&e.status===401) return;
    if(!settingsCurrent(generation,"cluster")) return;
    btn.disabled=false; btn.textContent=was;
    CLUSTER_REFUSAL={node_id:"This node",code:e.code,message:e.message};
  }
  if(settingsCurrent(generation,"cluster")) renderSettings();
}
// Inline library editor: the row becomes an editable form (name · kind · paths).
// The drawer's Save: identity first, then the schedule. Two endpoints, one
// button — a failure stops before the second write and is shown in the drawer.
async function saveLibDrawer(id,btn){
  const name=(document.getElementById("eln-"+id).value||"").trim();
  const s=document.getElementById("elk-"+id).value;
  const paths=(document.getElementById("elp-"+id).value||"").split(",").map(x=>x.trim()).filter(Boolean);
  const kind=s==="anime"?"shows":s, anime=s==="anime";
  const scan=Number(document.getElementById(`sched-scan-${id}`).value);
  const ref=Number(document.getElementById(`sched-ref-${id}`).value);
  const err=document.getElementById("ele-"+id); if(err) err.textContent="";
  btn.disabled=true;
  const dvSelect=document.getElementById(`dv-mode-${id}`);
  try{
    await api(`/libraries/${id}`,{method:"PUT",body:{name,kind,paths,anime}});
    await api(`/libraries/${id}/schedule`,{method:"PUT",body:{scan_interval_mins:scan,refresh_interval_mins:ref}});
    // The Dolby Vision mode is part of the same drawer, so one Save writes it
    // too — the select is only present when the conversion tools are available.
    if(dvSelect) await api(`/libraries/${id}/dv-conversion`,{method:"PUT",body:{mode:dvSelect.value}});
    invalidateLibs(); LIB_DRAWER=null; toast("Library saved"); viewSettings();
  }catch(e){ if(err) err.textContent=e.message; btn.disabled=false; }
}
async function enableReconcile(id,btn){
  // One click from Never to the six-hourly reconcile, keeping the library's
  // refresh interval; the schedule endpoint never starts a scan by itself.
  const refresh=/** @type {HTMLInputElement|HTMLSelectElement|null} */ (document.getElementById(`sched-ref-${id}`));
  const ref=Number(refresh?.value||0);
  const err=document.getElementById("ele-"+id); if(err) err.textContent="";
  btn.disabled=true;
  try{
    await api(`/libraries/${id}/schedule`,{method:"PUT",body:{scan_interval_mins:360,refresh_interval_mins:ref}});
    invalidateLibs(); toast("Scans every 6 hours"); viewSettings();
  }catch(e){ if(err) err.textContent=e.message; btn.disabled=false; }
}
async function scanAll(){
  const libs=(SETTINGS_DATA&&SETTINGS_DATA.libs)||[]; if(!libs.length) return;
  await Promise.all(libs.map(l=>api(`/libraries/${l.id}/scan`,{method:"POST"}).catch(()=>{})));
  toast(`Scanning ${libs.length} librar${libs.length===1?'y':'ies'}…`); setTimeout(settingsTick,400);
}
async function refreshAll(){
  const libs=(SETTINGS_DATA&&SETTINGS_DATA.libs)||[]; if(!libs.length) return;
  if(!confirm(`Re-fetch metadata & artwork for all ${libs.length} libraries? This can take a while.`)) return;
  await Promise.all(libs.map(l=>api(`/libraries/${l.id}/refresh`,{method:"POST"}).catch(()=>{})));
  toast("Refreshing all libraries…"); setTimeout(settingsTick,400);
}
async function settingsTick(generation=PAGE_RENDER_GENERATION,tab=settingsTab()){
  const h=location.hash;
  if(tab==="cluster"&&!settingsClusterEnabled()) return;
  if(generation!==PAGE_RENDER_GENERATION||!isSettingsRoute(h)||settingsTab()!==tab||document.visibilityState==="hidden"||
     (SETTINGS_TICKING&&SETTINGS_TICKING.generation===generation&&SETTINGS_TICKING.authGeneration===AUTH_GENERATION)) return;
  const owner={generation,authGeneration:AUTH_GENERATION};
  SETTINGS_TICKING=owner;
  try{
    if(tab==="libraries"){
      const status=await api("/scan/status");
      if(!settingsCurrent(generation,tab)) return;
      SETTINGS_DATA.status=status; SETTINGS_LOADED.add("status");
      const lastScans=new Map((SETTINGS_DATA.libs||[]).map(l=>[String(l.id),l.last_scan_at]));
      for(const [id,st] of Object.entries(status)){
        const el=document.getElementById("st-"+id);
        if(el) el.innerHTML=statusText(st,lastScans.get(String(id)));
      }
      // The per-library conversion line only exists while a row drawer is open;
      // refresh it there, deliberately colder than scan status, and only while
      // durable work is active.
      if(LIB_DRAWER!==null&&dvSnapshotHasActive(SETTINGS_DATA.dvConversions)&&Date.now()>=DV_SETTINGS_POLL_AT){
        try{
          const snapshot=await refreshDvConversions();
          if(!settingsCurrent(generation,tab)) return;
          paintDvConversionProgress(snapshot);
        }catch(error){ if(error&&error.status===401) throw error; }
      }
    }
    if(tab==="maintenance"){
      // Dolby Vision on disk — the guard lifecycle and parallel work — lives
      // here now, so its live refresh does too. Same cold cadence and same
      // stop-when-terminal rule as the old Libraries branch.
      if(dvSnapshotHasActive(SETTINGS_DATA.dvConversions)&&Date.now()>=DV_SETTINGS_POLL_AT){
        try{
          const snapshot=await refreshDvConversions();
          if(!settingsCurrent(generation,tab)) return;
          const guards=document.getElementById("dv-recovery-guards");
          if(guards) guards.innerHTML=dvRecoveryGuardsHtml(snapshot);
        }catch(error){ if(error&&error.status===401) throw error; }
      }
    }
    if(tab==="system"&&SETTINGS_DATA.sys&&SETTINGS_DATA.sys.transcoder_optimization&&SETTINGS_DATA.sys.transcoder_optimization.running){
      await pollTranscoderOptimization(SETTINGS_DATA.sys.node_id,generation);
      if(!settingsCurrent(generation,tab)) return;
    }
    // A queued or running tone-map census reports its step on the System row.
    if(tab==="system"&&toneMapCensusActive()){
      const census=await api("/system/tone-map/census");
      if(!settingsCurrent(generation,tab)) return;
      paintToneMapCensus(census);
    }
    const secondary=[];
    const auto=document.getElementById("logauto");
    if(auto&&auto.checked) secondary.push(refreshLogs());
    const clusterAuto=document.getElementById("cllogauto");
    if(clusterAuto&&clusterAuto.checked) secondary.push(refreshClusterLogs());
    // Live Trakt card: the device-link poll and sync state resolve server-side.
    if(tab==="integrations"&&!TRAKT_EDIT){
      const t=await api("/trakt/status");
      if(!settingsCurrent(generation,tab)) return;
      if(JSON.stringify(t)!==JSON.stringify(TRAKT)){ cacheTrakt(t); paintTrakt(); }
    }
    if(tab==="analysis"){
      const snapshot=await api("/analysis/summary");
      if(!settingsCurrent(generation,tab)) return;
      SETTINGS_DATA.analysis=snapshot; SETTINGS_LOADED.add("analysis");
      const mount=document.getElementById("analysis-settings-summary");
      if(mount) mount.innerHTML=analysisSummaryCard(snapshot,"settings");
    }
    if(tab==="cluster"){
      const current=SETTINGS_DATA.cluster;
      const clustered=Boolean(current&&!current.unavailable);
      const recovery=clustered?PlurxClusterPanel.clusterRecoveryState(current):null;
      const live=clustered&&((current.nodes||[]).some(n=>n.maintenance)||(recovery&&recovery.required));
      // The direct status ages the same way the roster does, and every strip
      // cell, drawer and ledger reading is read off it. It is refreshed here
      // rather than on a timer of its own because this loop already owns the
      // cadence, the generation guard, the visibility check, and the
      // re-entrancy latch, and a second loop would have to reimplement all four.
      //
      // Only on a machine that is actually in a cluster: a single SQLite box
      // renders no strip, no roster and no direct readings, so collecting a
      // fan-out it will not paint is a request spent to learn nothing.
      //
      // Stamped BEFORE the first await: a probe slower than the gate must not
      // stack a second fan-out behind itself. A refusal leaves it stamped too —
      // the retry is the next gate, not the next tick.
      const due=clustered&&clusterOpsDue();
      if(due) clusterOpsStamp();
      // The roster is re-read on the same gate, so "Nodes reachable", the
      // strip's verdict and the voter-down alert follow a voter that stops
      // heartbeating; and every two seconds while something is in flight (a
      // node in maintenance, a recovery), which is when it moves fastest. A
      // failed re-read keeps the roster on screen; the next tick tries again.
      let snapshot=null;
      if(live||due){
        snapshot=await api("/cluster/nodes").catch(error=>{ if(error&&error.status===401) throw error; return null; });
        if(!settingsCurrent(generation,tab)) return;
      }
      const rosterChanged=Boolean(snapshot)&&PlurxClusterPanel.clusterRosterProjection(current)
        !==PlurxClusterPanel.clusterRosterProjection(snapshot);
      if(snapshot){
        // The roster poll runs precisely while a node is fenced or a recovery
        // is required — which is when the force-election dialog is most likely
        // to be open. A roster that arrives under a dialog is dropped, not
        // stored; the next read sees the same difference and pays the repaint.
        if(clusterRepaintDeferred()) snapshot=null;
        else{ SETTINGS_DATA.cluster=snapshot; SETTINGS_LOADED.add("cluster"); CLUSTER_LOADED=true; }
      }
      let repaint=rosterChanged&&Boolean(snapshot);
      if(due){
        try{
          const previous=SETTINGS_DATA.clusterOps;
          const ops=clusterOpsReceived(await api("/cluster/status",{keepSessionOn401:true}));
          if(!settingsCurrent(generation,tab)) return;
          const changed=PlurxClusterPanel.clusterOpsProjection(previous)
            !==PlurxClusterPanel.clusterOpsProjection(ops);
          // Held, not stored, when it changed under a dialog: the freshness row
          // keeps reporting the age of the sample actually on screen, which is
          // the only thing it can honestly describe.
          if(!(changed&&clusterRepaintDeferred())){
            SETTINGS_DATA.clusterOps=ops; SETTINGS_LOADED.add("clusterOps");
            if(changed) repaint=true;
          }
        }catch(error){
          // No refusal here ends the session any more, including a 401: the
          // guard on this read answers from a process-local proof cache, so
          // its refusal describes the cluster and not the credential. Every
          // failure takes the same path — keep the retained evidence, keep
          // its age honest, and let the next tick try again.
          if(!settingsCurrent(generation,tab)) return;
          // Retained transport evidence advances locally even when the refresh
          // fails: active work crosses its deadline into Stalled and eventually
          // expires. Re-render that projection unless an open decision dialog
          // owns the current DOM; the next tick will try the preserving repaint.
          repaint=true;
        }
      }
      // One paint for whatever changed, never under a dialog.
      if(repaint&&!clusterRepaintDeferred()) repaintClusterPreserving(renderSettings);
      // Every tick, fetch or no fetch: the sample age, the reading age and the
      // heartbeats are ages against the clock, and patching them is free.
      patchClusterReadingAge();
    }
    await Promise.all(secondary);
  }catch(e){ if(e&&e.status===401) return; }
  finally{ if(SETTINGS_TICKING===owner) SETTINGS_TICKING=null; }
}
function sameLogRequest(left,right){ return !!left&&left.box===right.box&&left.level===right.level; }
async function runLogRequest(request,cluster){
  const {box,level}=request;
  const boxId=cluster?"cllogbox":"logbox", levelId=cluster?"clloglvl":"loglvl";
  try{
    const scope=cluster?"scope=cluster&":"";
    const lines=await api(`/system/logs?${scope}level=${level}&limit=300`);
    const currentBox=document.getElementById(boxId);
    const currentLevel=document.getElementById(levelId);
    if(currentBox!==box||(currentLevel?currentLevel.value:"info")!==level) return;
    const stick = box.scrollHeight-box.scrollTop-box.clientHeight < 40;
    box.innerHTML=lines.map(l=>`<div class="log-${esc(l.level)}">${fmtTs(l.ts_ms)} <b>${esc(l.level)}</b> ${esc(l.target)} — ${esc(l.message)}</div>`).join("")
      ||'<span class="muted">Nothing at this level yet.</span>';
    if(stick) box.scrollTop=box.scrollHeight;
  }catch(e){
    if(e&&e.status===401) throw e;
    const currentBox=document.getElementById(boxId);
    const currentLevel=document.getElementById(levelId);
    if(currentBox===box&&(currentLevel?currentLevel.value:"info")===level){
      box.textContent=`Failed to load ${cluster?"cluster ":""}logs: ${e.message}`;
    }
  }
}
function refreshLogStream(cluster){
  const boxId=cluster?"cllogbox":"logbox", levelId=cluster?"clloglvl":"loglvl";
  const box=document.getElementById(boxId); if(!box) return Promise.resolve();
  const sel=document.getElementById(levelId);
  const desired={box,level:sel?sel.value:"info"};
  let run=cluster?CLUSTER_LOGS_RUN:LOGS_RUN;
  if(run){
    if(sameLogRequest(run.active,desired)) run.pending=null;
    else run.pending=desired;
    return run.promise;
  }
  run={active:null,pending:desired,promise:null,authGeneration:AUTH_GENERATION};
  if(cluster) CLUSTER_LOGS_RUN=run; else LOGS_RUN=run;
  run.promise=(async()=>{
    try{
      while(run.pending&&run.authGeneration===AUTH_GENERATION){
        run.active=run.pending; run.pending=null;
        await runLogRequest(run.active,cluster);
      }
    }finally{
      if(cluster&&CLUSTER_LOGS_RUN===run) CLUSTER_LOGS_RUN=null;
      if(!cluster&&LOGS_RUN===run) LOGS_RUN=null;
    }
  })();
  return run.promise;
}
function refreshLogs(){
  const disclosure=/** @type {HTMLDetailsElement|null} */(document.getElementById("system-logs"));
  if(disclosure&&!disclosure.open)return Promise.resolve();
  return refreshLogStream(false);
}
function refreshClusterLogs(){ return refreshLogStream(true); }
async function addLib(btn){
  const name=document.getElementById("eln-new").value.trim(), sel=document.getElementById("elk-new").value;
  const paths=document.getElementById("elp-new").value.split(",").map(s=>s.trim()).filter(Boolean);
  // "Anime" is a shows library flagged anime (absolute numbering + AniList).
  const kind = sel==="anime" ? "shows" : sel;
  const anime = sel==="anime";
  const err=document.getElementById("lerr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{ await api("/libraries",{method:"POST",body:{name,kind,paths,anime}}); invalidateLibs(); LIB_DRAWER=null; toast("Library added — scanning"); viewSettings(); }
  catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function scanLib(id){ try{ await api(`/libraries/${id}/scan`,{method:"POST"}); toast("Scan started"); setTimeout(settingsTick,400);}catch(e){toast(e.message)} }
async function refreshLib(id){ try{ await api(`/libraries/${id}/refresh`,{method:"POST"}); toast("Refreshing metadata & artwork…"); setTimeout(settingsTick,400);}catch(e){toast(e.message)} }
async function delLib(id){ if(!confirm("Delete this library? Media files are not touched.")) return; try{ await api(`/libraries/${id}`,{method:"DELETE"}); invalidateLibs(); toast("Deleted"); viewSettings(); }catch(e){toast(e.message)} }
// Saving a key over libraries that already exist needs the backfill notice;
// saving one on a fresh server does not, because the first scan will pick it
// up on its own. Decided before the save, since `viewSettings` refetches and
// `tmdb_configured` is true either way afterwards.
function noteKeySaved(label, value, wasConfigured){
  const libs=(SETTINGS_DATA&&SETTINGS_DATA.libs)||[];
  if(value && !wasConfigured && libs.length) KEY_NEEDS_BACKFILL=label;
}
async function saveTmdb(btn){
  const err=document.getElementById("terr"); err.textContent="";
  const was=!!(SETTINGS_DATA&&SETTINGS_DATA.settings&&SETTINGS_DATA.settings.tmdb_configured);
  const v=document.getElementById("tk").value.trim();
  if(btn) btn.disabled=true;
  try{ await api("/settings",{method:"PUT",body:{tmdb_api_key:v}}); noteKeySaved("TMDB",v,was); toast("Saved"); viewSettings(); }
  catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function saveOmdb(btn){
  const err=document.getElementById("oerr"); err.textContent="";
  const was=!!(SETTINGS_DATA&&SETTINGS_DATA.settings&&SETTINGS_DATA.settings.omdb_configured);
  const v=document.getElementById("ok").value.trim();
  if(btn) btn.disabled=true;
  try{ await api("/settings",{method:"PUT",body:{omdb_api_key:v}}); noteKeySaved("OMDb",v,was); toast("Saved"); viewSettings(); }
  catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
