"use strict";
// ---- the operations rail -------------------------------------------------
// Every action this screen can take, with its precondition evaluated before the
// click rather than after it. The rows themselves — and the preconditions they
// state — are the model's, in cluster-panel.js; this is the mount.
//
// Two rows carry their control inside them: the join credential and the
// graceful leave. Those panels read module state (the minted token, the leaving
// flag) that the model cannot see, so the shell renders them and hands them
// over. A blocked row gets none — the blocked reason is its whole content, and
// an unreachable expansion is a control that lies about being available.
function clusterOperationsRail(cluster,ops){
  const maintenanceActive=(cluster.nodes||[]).some(node=>node.maintenance);
  const controls={add:()=>joinPanel(maintenanceActive),
    leave:()=>leavePanel(cluster.local_node_id,maintenanceActive)};
  const rows=PlurxClusterPanel.clusterOperationRows(clenv(),cluster,ops,CLUSTER_RAIL_EXPANDED)
    .map(row=>row.expand&&!row.blocked&&controls[row.expand]
      ? Object.assign({},row,{expansion:controls[row.expand]()}):row);
  return `<div class="clrail">${rows.map(PlurxClusterPanel.clusterRailRow).join("")}</div>`;
}
// Expanding is state, not markup: the panel is re-rendered from it, so a mint
// or a status repaint cannot close a credential someone is reading. It is never
// written to storage, and forgetJoinToken() clears it on the same three exits
// that drop the token itself.
function toggleClusterRailPanel(id){
  CLUSTER_RAIL_EXPANDED=CLUSTER_RAIL_EXPANDED.indexOf(id)===-1
    ? CLUSTER_RAIL_EXPANDED.concat([id])
    : CLUSTER_RAIL_EXPANDED.filter(open=>open!==id);
  repaintClusterPreserving(renderSettings);
  const panel=document.getElementById("clrail-"+id);
  if(panel&&!panel.hidden&&panel.scrollIntoView) panel.scrollIntoView({block:"nearest"});
}
function clusterOperationsPanel(cluster,ops){
  const recovery=PlurxClusterPanel.clusterRecoveryState(cluster);
  const active=(cluster.nodes||[]).find(n=>n.maintenance);
  if(recovery.required) return clusterRecoveryPanel(cluster,recovery,active,ops);
  if(active){
    return `<section class="cluster-card"><div class="clophead"><div><h3>Maintenance in progress</h3><p>${esc(active.hostname||active.node_id)}</p></div><span class="pill" style="color:var(--warn);border-color:var(--warn)">Fenced</span></div>
      ${clusterMaintenanceProgress(active,ops,cluster.local_node_id)}
    </section>`;
  }
  // Planned work is now the operations rail, which states these same two
  // actions with their preconditions already evaluated.
  return "";
}
function forceElectionDialog(enabled){
  return `<dialog class="cldialog" id="clelection"><div class="cldialogbody"><h3>Force leader election</h3>
    <p>This briefly interrupts writes while a caught-up follower campaigns. The current voter majority stays unchanged, and Raft—not this screen—selects the next leader.</p>
    <ol class="clsteps"><li class="done">A voter quorum is reachable</li><li class="done">A caught-up follower is available</li><li class="done">No maintenance or removal is pending</li></ol>
    <label class="row" style="margin-top:16px;font-size:13px"><input id="clelectionconfirm" type="checkbox" style="width:auto" onchange="document.getElementById('clelectiongo').disabled=!this.checked"> I understand this may briefly pause writes</label>
    <div class="cldialogactions"><button class="ghost sm" onclick="this.closest('dialog').close()">Cancel</button><button id="clelectiongo" disabled onclick="forceClusterElection(this)">Force election</button></div>
  </div></dialog>`;
}
function clusterOperationsUnavailable(message){
  return `<div class="clopssummary"><div class="clopshead"><div><h4>Operational readiness</h4>
      <p class="muted" style="margin:0">Direct voter observations and the guarded rollout verdict.</p></div>
      <button class="ghost sm" onclick="refreshClusterOperations(this)">Retry status</button></div>
    <div class="clstate warn" role="status"><h3>Do not restart another voter</h3>
      <p>The bounded operations status could not be collected: ${message||"request failed"}. Keep every available voter running and retry; a roster heartbeat is not a substitute for a direct peer observation.</p></div></div>`;
}
function clusterRestartPreparationHtml(localRow){
  const media=localRow&&localRow.status&&localRow.status.media;
  if(!media||!media.new_admissions_blocked) return "";
  const commands=media.restart_commands||[];
  const expires=media.preparation_expires_at_unix_ms
    ? `${Math.max(0,Math.ceil((media.preparation_expires_at_unix_ms-Date.now())/1000))}s`
    : "unknown";
  const commandHtml=media.drained&&commands.length
    ? `<p><b>This node is drained.</b> Remove it from new proxy traffic, confirm direct-play connections are zero, then run exactly one command for this host:</p>
       <dl class="kvgrid">${commands.map(row=>`<dt>${esc(PlurxClusterPanel.clusterOperationReason(row.supervisor))}</dt><dd><code>${esc(row.command)}</code></dd>`).join("")}</dl>`
    : `<p>Waiting for ${esc(String(media.local_active_sessions))} owned session${media.local_active_sessions===1?"":"s"} and ${esc(String(media.admissions_in_flight))} admission${media.admissions_in_flight===1?"":"s"} in flight. Existing streams keep serving.</p>`;
  return `<div class="clstate ${media.drained?"good":"warn"}" role="status"><h3>${media.drained?"Prepared and drained":"Preparing this voter for restart"}</h3>
    ${commandHtml}<p>Preparation expires in ${esc(expires)}. Direct-play connections are not counted here; drain them at the proxy.</p>
    <button class="ghost sm" onclick='cancelLocalRestart(this,${esc(JSON.stringify(localRow.membership.node_id))})'>Cancel preparation</button></div>`;
}
function clusterOperationsCard(ops){
  if(!ops) return `<div class="clopssummary"><div class="clopshead"><div><h4>Operational readiness</h4>
      <p class="muted" style="margin:0">Collecting direct voter observations and the guarded rollout verdict…</p></div></div></div>`;
  if(ops.unavailable) return clusterOperationsUnavailable(esc(ops.message||"request failed"));
  const rows=ops.nodes||[], answered=rows.filter(row=>row.observation==="answered"&&row.status);
  const voters=rows.filter(row=>row.membership&&row.membership.is_voter);
  const ready=answered.filter(row=>row.membership.is_voter&&row.status.serving&&row.status.serving.ready).length;
  const q=PlurxClusterPanel.clusterQuorum(voters.length);
  const leader=answered.find(row=>row.status.raft&&row.status.raft.is_leader===true);
  const terms=[...new Set(answered.map(row=>row.status.raft&&row.status.raft.current_term).filter(value=>value!==null&&value!==undefined))];
  const builds=[...new Set(answered.map(row=>row.status.build))];
  const lags=answered.map(row=>row.status.raft&&row.status.raft.apply_lag_entries).filter(value=>value!==null&&value!==undefined);
  const sessions=answered.reduce((sum,row)=>sum+(row.status.media&&row.status.media.local_active_sessions||0),0);
  const oldest=rows.reduce((age,row)=>Math.max(age,row.sample_age_ms||0),0);
  const localRow=rows.find(row=>row.membership&&ops.membership&&row.membership.node_id===ops.membership.local_node_id);
  const safe=ops.verdict&&ops.verdict.safe_to_restart_one;
  const title=safe?"Ready to restart one voter":"Do not restart another voter";
  const blockers=(ops.verdict&&ops.verdict.blockers)||[];
  const warnings=(ops.verdict&&ops.verdict.warnings)||[];
  const findings=[...blockers,...warnings];
  const findingHtml=findings.length?`<ul class="clopsblockers">${findings.map(f=>`<li><b>${esc(PlurxClusterPanel.clusterOperationReason(f.code))}</b>${f.node_id?` on ${esc(f.node_id)}`:""}: ${esc(f.message)}</li>`).join("")}</ul>`
    : `<p class="hint">No rollout blockers are present. The candidate is ${esc(ops.verdict.candidate_node_id)}; prepare and drain that exact node before restarting it.</p>`;
  const canPrepare=safe&&localRow&&ops.verdict.candidate_node_id===localRow.membership.node_id;
  const prepareAction=canPrepare
    ? `<button class="ghost sm" onclick='prepareLocalRestart(this,${esc(JSON.stringify(localRow.membership.node_id))})'>Prepare this voter for restart</button>`
    : safe?`<span class="hint">Open ${esc(ops.verdict.candidate_node_id)} directly to prepare that voter.</span>`:"";
  return `<div class="clopssummary"><div class="clopshead"><div><h4>Operational readiness</h4>
      <p class="muted" style="margin:0">Cluster-wide verdict from the direct evidence shown inside each node.</p></div>
      <div class="row">${prepareAction}<button class="ghost sm" onclick="downloadClusterSupportBundle(this)">Export support bundle</button>
      <button class="ghost sm" onclick="refreshClusterOperations(this)">Refresh status</button></div></div>
    <div class="clstate ${safe?"good":"warn"}" role="status"><h3>${esc(title)}</h3>
      <p>${safe?`The observed voters preserve majority after one restart. Prepare only ${esc(ops.verdict.candidate_node_id)} and re-check before touching another node.`:
        "At least one direct safety condition is missing or unhealthy. Keep the available voters running until every blocker below clears."}</p>${findingHtml}</div>
    ${clusterRestartPreparationHtml(localRow)}
    <div class="clopsfacts"><div class="clopsfact"><span class="muted">Quorum</span><b>${voters.length} voters · majority ${q.majority} · ${ready} ready</b></div>
      <div class="clopsfact"><span class="muted">Leader and term</span><b>${leader?esc(leader.membership.hostname||leader.membership.node_id):"Unknown"} · ${terms.length===1?`term ${esc(String(terms[0]))}`:"term disagreement"}</b></div>
      <div class="clopsfact"><span class="muted">Build convergence</span><b>${builds.length===1?esc(builds[0]):`${builds.length} observed builds`}</b></div>
      <div class="clopsfact"><span class="muted">Commit / apply</span><b>maximum lag ${lags.length?esc(String(Math.max(...lags))):"unknown"}</b></div>
      <div class="clopsfact"><span class="muted">Owned media</span><b>${sessions} active session${sessions===1?"":"s"}</b></div>
      <div class="clopsfact"><span class="muted">Oldest direct sample</span><b>${esc(PlurxClusterPanel.clusterOperationAge(oldest))}</b></div></div></div>`;
}
// An election dialog blocks a new click, but it can open after this request has
// started. The response stays staged until the await completes so a late
// success or failure cannot repaint over that newer decision.
async function refreshClusterOperations(btn){
  const generation=PAGE_RENDER_GENERATION;
  if(btn) btn.disabled=true;
  SETTINGS_LOADED.delete("clusterOps");
  const mount=document.getElementById("cluster-operations");
  if(mount) mount.innerHTML=clusterOperationsCard(null);
  try{
    // The request may outlive the click that started it. Keep its response
    // staged until we know an election dialog has not opened in the meantime.
    const ops=await loadSettingsKey("clusterOps",generation,false);
    if(settingsCurrent(generation,"cluster")){
      if(clusterRepaintDeferred()){
        patchClusterReadingAge();
        return;
      }
      SETTINGS_DATA.clusterOps=ops;
      SETTINGS_LOADED.add("clusterOps");
      repaintClusterPreserving(renderSettings);
    }
  }catch(error){
    if(settingsCurrent(generation,"cluster")){
      // The retained node evidence has a client-local clock of its own. Repaint
      // it before mounting the request failure so a manual refresh cannot leave
      // an expired install rendered as active until the next 15-second gate.
      if(clusterRepaintDeferred()) patchClusterReadingAge();
      else repaintClusterPreserving(renderSettings);
      const current=document.getElementById("cluster-operations");
      if(current) current.innerHTML=clusterOperationsUnavailable(esc(error&&error.message||"request failed"));
    }
  }finally{ if(btn) btn.disabled=false; }
}
async function prepareLocalRestart(btn,nodeId){
  if(!confirm(`Prepare ${nodeId} for restart?\n\nThis blocks new mutable-media work on this process for 15 minutes. Existing sessions keep serving; membership does not change and plurxd will not invoke the host supervisor.`)) return;
  if(btn) btn.disabled=true;
  try{
    await api(`/cluster/nodes/${encodeURIComponent(nodeId)}/restart-preparation`,{method:"POST",body:{expires_in_seconds:900}});
    toast("Restart preparation started");
    await refreshClusterOperations();
    pollLocalRestart(nodeId,PAGE_RENDER_GENERATION);
  }catch(error){ toast(error&&error.message||"Restart preparation was refused"); }
  finally{ if(btn) btn.disabled=false; }
}
async function cancelLocalRestart(btn,nodeId){
  if(btn) btn.disabled=true;
  try{
    await api(`/cluster/nodes/${encodeURIComponent(nodeId)}/restart-preparation`,{method:"DELETE"});
    toast("Restart preparation canceled");
    await refreshClusterOperations();
  }catch(error){ toast(error&&error.message||"Cancellation failed"); }
  finally{ if(btn) btn.disabled=false; }
}
async function pollLocalRestart(nodeId,generation){
  while(settingsCurrent(generation,"cluster")){
    await new Promise(resolve=>setTimeout(resolve,2000));
    if(!settingsCurrent(generation,"cluster")) return;
    try{
      const ops=clusterOpsReceived(await api("/cluster/status",{keepSessionOn401:true}));
      if(!settingsCurrent(generation,"cluster")) return;
      // The restart poll owns freshness while it runs, so it stamps the same
      // clock the tick reads: the two never probe the fan-out at once. It
      // repaints every two seconds by design — the drain countdown is the point
      // of the view — but under an open modal it holds the sample like the tick
      // does, so what is stored is still what is painted. Its own loop condition
      // reads the fresh sample either way.
      clusterOpsStamp();
      if(!clusterRepaintDeferred()){
        SETTINGS_DATA.clusterOps=ops; SETTINGS_LOADED.add("clusterOps");
        repaintClusterPreserving(renderSettings);
      }
      const local=(ops.nodes||[]).find(row=>row.membership&&row.membership.node_id===nodeId);
      const media=local&&local.status&&local.status.media;
      if(!media||!media.new_admissions_blocked||media.drained) return;
    }catch(error){ return; }
  }
}
async function downloadClusterSupportBundle(btn){
  if(btn) btn.disabled=true;
  try{
    const response=await api("/cluster/support-bundle",{raw:true,keepSessionOn401:true});
    const blob=await response.blob();
    const url=URL.createObjectURL(blob), link=document.createElement("a");
    link.href=url; link.download="plurx-cluster-support.zip"; link.style.display="none";
    document.body.appendChild(link); link.click(); link.remove(); URL.revokeObjectURL(url);
    toast("Redacted support bundle downloaded");
  }catch(error){ toast(error&&error.message||"Support bundle failed"); }
  finally{ if(btn) btn.disabled=false; }
}
