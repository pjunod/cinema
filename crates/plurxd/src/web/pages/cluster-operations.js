"use strict";
// ---- maintenance, restart preparation and the expansions ------------------
// The cluster-wide actions and the guarded restart flow. The rows themselves —
// and the preconditions they state — are the model's, in cluster-panel.js.
// Expanding is state, not markup: the panel is re-rendered from it, so a mint
// or a status repaint cannot close a credential someone is reading. It is never
// written to storage, and forgetJoinToken() clears it on the same three exits
// that drop the token itself.
function toggleClusterExpansion(id){
  CLUSTER_RAIL_EXPANDED=CLUSTER_RAIL_EXPANDED.indexOf(id)===-1
    ? CLUSTER_RAIL_EXPANDED.concat([id])
    : CLUSTER_RAIL_EXPANDED.filter(open=>open!==id);
  repaintClusterPreserving(renderSettings);
  const panel=document.getElementById("clexp-"+id);
  if(panel&&!panel.hidden&&panel.scrollIntoView) panel.scrollIntoView({block:"nearest"});
}
// Cluster-wide actions only: the rolling-restart verdict, a leader election
// and the diagnostics a report needs. Per-node actions live in that node's
// drawer. The election dialog is rendered here, once, whatever opens it.
function clusterMaintenanceCard(cluster,ops){
  const rows=PlurxClusterPanel.clusterMaintenanceRows(clenv(),cluster,ops);
  return `<section class="clcard" id="cluster-maintenance"><div class="clcardhead"><h2>Maintenance</h2></div>${
    rows.map(row=>`<div class="clact" data-row="${row.id}"><div><div class="t">${esc(row.title)}</div><div class="d">${row.text}</div></div><div class="row">${row.action}</div></div>`).join("")}
    ${forceElectionDialog(true)}</section>`;
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
  return `<div class="clalert" role="status"><div><b>Do not restart another voter.</b> The bounded operations status could not be collected: ${message||"request failed"}. Keep every available voter running and retry; a roster heartbeat is not a substitute for a direct peer observation.</div>
    <div class="row"><button class="ghost sm" onclick="refreshClusterOperations(this)">Retry status</button></div></div>`;
}
function clusterRestartPreparationHtml(localRow){
  const media=localRow&&localRow.status&&localRow.status.media;
  if(!media||!media.new_admissions_blocked) return "";
  const commands=media.restart_commands||[];
  const expires=media.preparation_expires_at_unix_ms
    ? `${Math.max(0,Math.ceil((media.preparation_expires_at_unix_ms-Date.now())/1000))}s`
    : "unknown";
  const commandHtml=media.drained&&commands.length
    ? `This node is drained. Remove it from new proxy traffic, confirm direct-play connections are zero, then run exactly one command for this host:
       <dl class="kvgrid">${commands.map(row=>`<dt>${esc(PlurxClusterPanel.clusterOperationReason(row.supervisor))}</dt><dd><code>${esc(row.command)}</code></dd>`).join("")}</dl>`
    : `Waiting for ${esc(String(media.local_active_sessions))} owned session${media.local_active_sessions===1?"":"s"} and ${esc(String(media.admissions_in_flight))} admission${media.admissions_in_flight===1?"":"s"} in flight. Existing streams keep serving.`;
  return `<div class="clalert${media.drained?" good":""}" role="status"><div><b>${media.drained?"Prepared and drained.":"Preparing this voter for restart."}</b>
    ${commandHtml} Preparation expires in ${esc(expires)}. Direct-play connections are not counted here; drain them at the proxy.</div>
    <div class="row"><button class="ghost sm" onclick='cancelLocalRestart(this,${esc(JSON.stringify(localRow.membership.node_id))})'>Cancel preparation</button></div></div>`;
}
// An election dialog blocks a new click, but it can open after this request has
// started. The response stays staged until the await completes so a late
// success or failure cannot repaint over that newer decision.
async function refreshClusterOperations(btn){
  const generation=PAGE_RENDER_GENERATION;
  if(btn) btn.disabled=true;
  SETTINGS_LOADED.delete("clusterOps");
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
