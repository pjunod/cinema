"use strict";
// ---- cluster membership --------------------------------------------------
// The operator's whole view of who is in this cluster, over the admin-only node
// API. It adds no endpoint and decides no membership policy: the server refuses
// what it refuses, and this renders the refusal in words somebody can act on.
//
// The one thing this panel must never do is present two voters as redundancy.
// docs/cluster/CLUSTERING-PLAN.md §7.2 makes two-node HA a stated non-goal because two
// voters need *both* machines for every write and therefore survive no failure
// — strictly worse than one node, while looking like an upgrade. A settings
// screen is the surface most likely to imply otherwise, so the wording below is
// the deliverable, not decoration around the API fields.
//
// Loaded only when this tab is opened. The overwhelmingly common install is one
// SQLite box where the endpoint answers `membership_unavailable`; spending an
// admin request on every Settings visit to learn that would be noise.
let CLUSTER_LOADED=false;
// When /cluster/status last left the server, so the Cluster tab can keep it
// fresh without a timer of its own. Every path that stores a new aggregate
// stamps this; the tick reads it. See clusterOpsInterval().
let CLUSTER_OPS_FETCHED_AT=0;
// Receipt age is browser-local and monotonic. The daemon's aggregate timestamp
// belongs to another wall clock, while adding a client timestamp to the JSON
// object would leak UI-only metadata into projections and support exports.
const CLUSTER_OPS_RECEIVED_AT=new WeakMap();
// The last removal refusal, held so its sentence stays readable. A toast is
// 2.2 seconds and these answers are paragraphs.
let CLUSTER_REFUSAL=null;
let CLUSTER_LEAVING=false;
// A minted join token lives here and nowhere else — never localStorage, never
// the URL, never a log. Until it is redeemed or expires it is the complete
// authority to join a node to this cluster (docs/SECURITY.md), so it is held
// for exactly as long as the panel showing it is on screen, and dropped on tab
// change and on every re-entry into Settings.
let CLUSTER_TOKEN=null;
// Which rail rows have their membership control showing. Module state, never
// storage: a credential surface must not reopen itself on the next visit, and a
// fresh page is always collapsed. Because it is state rather than markup, a
// repaint cannot yank the form shut under someone mid-mint.
let CLUSTER_RAIL_EXPANDED=[];
function forgetJoinToken(){ CLUSTER_TOKEN=null; CLUSTER_REFUSAL=null; CLUSTER_RAIL_EXPANDED=[]; }
async function loadCluster(){
  const generation=PAGE_RENDER_GENERATION;
  SETTINGS_LOADED.delete("cluster"); CLUSTER_LOADED=false;
  await loadSettingsKey("cluster",generation);
  if(settingsCurrent(generation,"cluster")) renderSettings();
}
// The model in cluster-panel.js never reaches for a global: the shell's own
// escaping and formatting helpers are passed to it. One `esc` implementation,
// one escaping contract to review, and the module loads under Node with no
// setup at all.
function clenv(){ return {esc,fmtAgo,fmtBytes}; }
// /cluster/status is a bounded direct fan-out that probes every voter, so it is
// not a 2s reading. Fifteen seconds is slow enough that the probe cost stays
// where it was measured and fast enough that a rail row cannot state a verdict
// off a nine-minute-old sample. The restart flow, which does need 2s freshness,
// keeps its own poll and stamps this clock as it goes.
function clusterOpsInterval(){ return 15000; }
function clusterOpsStamp(){ CLUSTER_OPS_FETCHED_AT=Date.now(); }
function clusterOpsDue(){ return Date.now()-CLUSTER_OPS_FETCHED_AT>=clusterOpsInterval(); }
function clusterOpsReceived(ops){
  if(ops&&typeof ops==="object") CLUSTER_OPS_RECEIVED_AT.set(ops,performance.now());
  return ops;
}
function clusterOpsElapsed(ops){
  if(!ops||typeof ops!=="object") return 0;
  const receivedAt=CLUSTER_OPS_RECEIVED_AT.get(ops);
  return receivedAt===undefined?0:Math.max(0,performance.now()-receivedAt);
}
// A modal is a decision in progress. renderSettings() rewrites the tab, and the
// force-election dialog lives in that markup, so repainting under it would close
// it mid-sentence.
//
// What the panel holds in SETTINGS_DATA is therefore exactly what it painted: a
// sample that arrives while a dialog is open is dropped rather than stored, so
// the next collection sees the same difference and pays the repaint then. One
// invariant — never store what you did not paint — instead of a second copy of
// the payload for the screen to drift from.
function clusterRepaintDeferred(){
  return Boolean(document.querySelector(".clusterwrap dialog[open]"));
}
// Which node cards have their "WAL, snapshot, and protocol details" open. A
// drill-down is not layout, so it is never persisted to storage — it just has
// to survive the repaint happening underneath it.
function clusterOpenDetailNodes(){
  const open=[];
  document.querySelectorAll("#cluster-node-list .clnodebody").forEach(body=>{
    const detail=body.querySelector(".clnodedetail");
    const id=body.getAttribute("data-node");
    if(detail&&detail.open&&id) open.push(id);
  });
  return open;
}
// renderSettings() rewrites the whole tab. The folds and the troubleshooting tab
// already come back (applyClusterFolds restores them from storage) and the join
// token is module state, but the roster's scroll position and the open
// drill-downs are in the markup that just got thrown away. Every repaint path on
// this tab goes through here so all three behave the same way. Text selection
// cannot be preserved across innerHTML at all — which is exactly why repaints
// are change-driven rather than periodic.
function repaintClusterPreserving(paint){
  const list=document.getElementById("cluster-node-list");
  const scroll=list?list.scrollTop:0;
  const open=clusterOpenDetailNodes();
  const controls=clusterControlValues();
  paint();
  const painted=document.getElementById("cluster-node-list");
  if(painted) painted.scrollTop=scroll;
  clusterRestoreControlValues(controls);
  if(!open.length) return;
  document.querySelectorAll("#cluster-node-list .clnodebody").forEach(body=>{
    const detail=body.querySelector(".clnodedetail");
    if(detail&&open.indexOf(body.getAttribute("data-node"))!==-1) detail.open=true;
  });
}
// Every choice the operator has made in this panel that lives only in the
// markup: the token's lifetime and role, the log level, the auto-refresh box.
// A repaint rebuilds them from their defaults, so a status refresh arriving
// between choosing "1 hour · read worker" and clicking Create would silently
// mint a ten-minute VOTER token. Captured by id and put back.
function clusterControlValues(){
  const values=[];
  document.querySelectorAll(".clusterwrap select[id],.clusterwrap input[id]").forEach(control=>{
    values.push({id:control.id,value:control.value,checked:control.checked});
  });
  return values;
}
function clusterRestoreControlValues(values){
  (values||[]).forEach(saved=>{
    const control=document.getElementById(saved.id);
    if(!control) return;
    if(control.type==="checkbox"||control.type==="radio") control.checked=saved.checked;
    else if(saved.value!==undefined&&saved.value!==null) control.value=saved.value;
  });
}
// The one reading that moves without a new fetch. When the projection has not
// changed there is no repaint, so this cell is patched on its own; without it
// the row whose only job is freshness would say "now" indefinitely.
function patchClusterReadingAge(){
  const cell=document.getElementById("cldb-reading-age");
  if(!cell) return;
  const data=SETTINGS_DATA||{};
  cell.innerHTML=PlurxClusterPanel.clusterReadingAge(clenv(),data.cluster,data.clusterOps);
}
function clusterNodeOperationsBadge(n,ops){
  if(!ops) return `<span class="pill">Collecting status</span>`;
  if(ops.unavailable) return `<span class="pill" style="color:var(--bad);border-color:var(--bad)">Status unavailable</span>`;
  const row=PlurxClusterPanel.clusterDirectOperationRow(ops,n.node_id), s=row&&row.status;
  if(!row||row.observation!=="answered"||!s)
    return `<span class="pill" style="color:var(--bad);border-color:var(--bad)">Not observed</span>`;
  const wal=s.wal&&s.wal.snapshot;
  const walHealthy=wal&&wal.state==="open"&&wal.lock_owned&&!wal.last_error&&wal.last_log_index===wal.last_durable_index;
  if(!s.process||!s.process.live||!s.serving||!s.serving.ready||!s.raft||!s.raft.sample_valid||!s.raft.watermark_valid||!walHealthy)
    return `<span class="pill" style="color:var(--warn);border-color:var(--warn)">Needs attention</span>`;
  return `<span class="pill" style="color:var(--good);border-color:var(--good)">Direct status ready</span>`;
}
function clusterNodeOperationsHtml(n,ops){
  if(!ops) return `<section class="clnodegroup"><div class="clgrouptitle">Operations</div>
    <div class="clnodeunknown">Collecting direct process, serving, Raft, WAL, snapshot, media, and build evidence…</div></section>`;
  if(ops.unavailable) return `<section class="clnodegroup"><div class="clgrouptitle">Operations</div>
    <div class="clnodeunknown" style="color:var(--bad)">Direct status is unavailable. Keep every available voter running and retry status above.</div></section>`;
  const row=PlurxClusterPanel.clusterDirectOperationRow(ops,n.node_id), s=row&&row.status;
  const transport=PlurxClusterPanel.clusterTransportExplanation(
    ops,n.raft_id,n.bounded_read_ready,clusterOpsElapsed(ops));
  const transportObservation=transport.observation;
  const transportText=transportObservation
    ? `${esc(transport.text)} · observer ${esc(String(transportObservation.observing_node_id))} · ${esc(PlurxClusterPanel.clusterOperationAge(transportObservation.sample_age_ms))}`
    : `<span class="muted">${esc(transport.text)}</span>`;
  if(!row||row.observation!=="answered"||!s){
    const why=PlurxClusterPanel.clusterObservationReason(row&&(row.error_class||row.observation)||"not_observed");
    return `<section class="clnodegroup"><div class="clgrouptitle">Operations</div>
      <div class="clnodeunknown" style="color:var(--bad)">Not observed · ${esc(why)}. Membership heartbeat state is not a substitute for a direct peer sample.</div>
      <div class="clnodeopmeta"><div><span>Transport</span>${transportText}</div></div></section>`;
  }
  const raft=s.raft||{}, wal=s.wal&&s.wal.snapshot, snap=s.snapshot||{}, media=s.media||{};
  const process=s.process&&s.process.live
    ? `<span style="color:var(--good)">Live</span> · ${esc(PlurxClusterPanel.clusterOperationAge(row.sample_age_ms))}`
    : `<span style="color:var(--bad)">Not live</span>`;
  const serving=s.serving&&s.serving.ready
    ? `<span style="color:var(--good)">Ready</span>`
    : `<span style="color:var(--bad)">Fenced: ${esc(PlurxClusterPanel.clusterOperationReason(s.serving&&s.serving.reason))}</span>`;
  const raftRole=raft.is_leader===true?"Leader":raft.is_leader===false?"Follower":"Unknown role";
  const raftText=raft.sample_valid&&raft.watermark_valid
    ? `${raftRole} · term ${esc(String(raft.current_term))} · lag ${raft.apply_lag_entries===null?"unknown":esc(String(raft.apply_lag_entries))}`
    : `<span style="color:var(--bad)">${raftRole} · stale or incomplete proof</span>`;
  const walHealthy=wal&&wal.state==="open"&&wal.lock_owned&&!wal.last_error&&wal.last_log_index===wal.last_durable_index;
  const walText=wal
    ? `<span style="color:var(--${walHealthy?"good":"bad"})">${esc(PlurxClusterPanel.clusterOperationReason(wal.state))}</span> · ${esc(String(wal.segment_count))} segment${wal.segment_count===1?"":"s"} · ${esc(fmtBytes(wal.allocated_bytes)||"0 B")}`
    : `<span style="color:var(--bad)">Unavailable</span>`;
  const snapshotText=snap.available
    ? snap.last_build_outcome
      ? `build ${esc(snap.last_build_outcome)} ${snap.last_build_unix_ms?esc(fmtAgo(Math.floor(snap.last_build_unix_ms/1000))):"at unknown time"} · DB ${snap.db_bytes===null||snap.db_bytes===undefined?"unknown":esc(fmtBytes(snap.db_bytes)||"0 B")}`
      : `builds ${esc(String(snap.build_ok_count))} ok · ${esc(String(snap.build_error_count))} errors · DB ${snap.db_bytes===null||snap.db_bytes===undefined?"unknown":esc(fmtBytes(snap.db_bytes)||"0 B")}`
    : `<span style="color:var(--bad)">Unavailable</span>`;
  const mediaText=`${esc(String(media.local_active_sessions||0))} session${Number(media.local_active_sessions||0)===1?"":"s"} · ${media.drained?"drained":"draining"}`;
  const lastError=wal&&wal.last_error?wal.last_error.message:"None";
  const recovery=wal&&wal.last_recovery?`${wal.last_recovery.operation}: ${wal.last_recovery.outcome}`:"None observed";
  return `<section class="clnodegroup"><div class="clgrouptitle">Operations</div>
    <div class="clnodeopmeta"><div><span>Process</span>${process}</div><div><span>Serving</span>${serving}</div>
      <div><span>Raft</span>${raftText}</div><div><span>WAL</span>${walText}</div>
      <div><span>Snapshot</span>${snapshotText}</div><div><span>Transport</span>${transportText}</div>
      <div><span>Media / build</span>${mediaText}<br><span class="clopscode">${esc(s.build)}</span></div></div>
    <details class="clnodedetail"><summary>WAL, snapshot, and protocol details</summary>
      <dl class="kvgrid"><dt>Node / Raft id</dt><dd class="clopscode">${esc(n.node_id)} / ${esc(String(n.raft_id))}</dd>
        <dt>Protocol range</dt><dd>${s.protocol_min===undefined?"unknown":esc(String(s.protocol_min))}–${s.protocol_max===undefined?"unknown":esc(String(s.protocol_max))}</dd>
        <dt>Applied / commit</dt><dd>${raft.applied_index===null||raft.applied_index===undefined?"unknown":esc(String(raft.applied_index))} / ${raft.commit_index===null||raft.commit_index===undefined?"unknown":esc(String(raft.commit_index))}</dd>
        <dt>Raft sample</dt><dd>${esc(PlurxClusterPanel.clusterOperationAge((raft.sample_age_seconds||0)*1000))}; watermark ${esc(PlurxClusterPanel.clusterOperationAge(raft.watermark_age_millis))}</dd>
        <dt>WAL retained / purged</dt><dd>${wal?(wal.first_retained_index===null?"unknown":esc(String(wal.first_retained_index))):"unknown"} / ${wal?(wal.last_purged_index===null?"none":esc(String(wal.last_purged_index))):"unknown"}</dd>
        <dt>WAL durable index</dt><dd>${wal&&wal.last_durable_index!==null?esc(String(wal.last_durable_index)):"unknown"}</dd>
        <dt>Last WAL sync</dt><dd>${wal&&wal.last_sync_unix_ms?esc(fmtAgo(Math.floor(wal.last_sync_unix_ms/1000))):"unknown"}</dd>
        <dt>Last WAL error</dt><dd>${esc(lastError)}</dd><dt>Last recovery</dt><dd>${esc(recovery)}</dd>
        <dt>Snapshot outcomes</dt><dd>build ${esc(String(snap.build_ok_count||0))} ok / ${esc(String(snap.build_error_count||0))} error; install ${esc(String(snap.install_ok_count||0))} ok / ${esc(String(snap.install_error_count||0))} error</dd>
        <dt>State-machine database</dt><dd>${snap.db_bytes===null||snap.db_bytes===undefined?"unknown":esc(fmtBytes(snap.db_bytes)||"0 B")}</dd>
        <dt>Last snapshot install</dt><dd>${snap.last_install_outcome?`${esc(snap.last_install_outcome)} ${snap.last_install_unix_ms?esc(fmtAgo(Math.floor(snap.last_install_unix_ms/1000))):"at unknown time"}`:"none observed"}</dd>
        <dt>Bounded-read recovery evidence</dt><dd>${transportText}</dd>
        <dt>Direct play</dt><dd>Unknown here — drain proxy connections separately.</dd></dl></details>
  </section>`;
}
function clusterNodeRow(n,localNodeId,maintenanceAllowed=true,context={}){
  const voter=n.is_voter;
  const admittedVoter=n.role==="voter";
  const role=voter
    ? `<span class="pill" style="color:var(--accent);border-color:var(--accent)">voter</span>`
    : admittedVoter
      ? `<span class="pill" title="Admitted as a voter and currently joining or catching up. It is not counted toward quorum until Raft commits its vote.">joining voter</span>`
      : `<span class="pill" title="A non-voting capacity role. A learner receives replication and may serve gated work; explicit promotion adds its vote only after catch-up and storage preflight.">learner</span>`;
  const heartbeat=n.reachable
    ? `<span style="color:var(--good)">fresh</span>`
    : `<span style="color:var(--bad)">stale</span>`;
  const seen=n.last_seen_at?esc(fmtAgo(Math.floor(n.last_seen_at/1000))):`<span class="muted">never</span>`;
  const leader=n.is_leader?`<span class="pill clleader">Leader</span>`:"";
  const local=n.node_id===localNodeId?`<span class="pill">This node</span>`:"";
  const pending=n.removal_pending?`<span class="pill" style="color:var(--warn);border-color:var(--warn)">Removal pending</span>`:"";
  const maintenance=n.maintenance?`<span class="pill" style="color:var(--warn);border-color:var(--warn)">Maintenance</span>`:"";
  const worker=!voter&&n.role==="learner"
    ? n.bounded_read_ready
      ? `<span class="pill" style="color:var(--good);border-color:var(--good)">Read worker ready</span>`
      : `<span class="pill" style="color:var(--warn);border-color:var(--warn)">Catching up</span>`
    : "";
  const hostname=n.hostname||n.node_id;
  const advertised=n.advertised_host
    ? `<div class="claddr">Advertised host ${esc(n.advertised_host)}</div>`:"";
  const lifecycleLocked=Boolean(context.lifecycleLocked);
  const localTarget=n.node_id===localNodeId;
  const directReady=PlurxClusterPanel.clusterMaintenanceReady(n,context.operations);
  const resumeReady=PlurxClusterPanel.clusterMaintenanceResumeReady(n,context.operations);
  const promote=!voter&&n.role==="learner"
    ? `<button class="ghost sm" ${n.bounded_read_ready&&n.voter_storage_ready&&!lifecycleLocked?"":"disabled"} onclick='promoteNode(${esc(JSON.stringify(n.node_id))})'>Promote to voter</button>`
    : "";
  const maintenanceAction=n.maintenance
    ? `<button class="sm" ${localTarget&&resumeReady?"":"disabled"} title="${localTarget?'Resume only after direct process drain proof is complete; leadership is handed off first when needed.':'Open this node directly to resume it.'}" onclick='setNodeMaintenance(${esc(JSON.stringify(n.node_id))},false)'>Resume service</button>`
    : `<button class="ghost sm" ${!maintenanceAllowed||!localTarget||!n.reachable||n.removal_pending?"disabled":""} title="${!localTarget?'Open this node directly to fence and drain it.':!maintenanceAllowed?'Direct maintenance preflight has not approved this node.':'Fence new work, hand off leadership when needed, and let existing work drain.'}" onclick='setNodeMaintenance(${esc(JSON.stringify(n.node_id))},true)'>Enter maintenance</button>`;
  const remove=n.node_id===localNodeId
    ? `<span class="muted">Use Leave this cluster on the Maintenance card</span>`
    : `<button class="ghost sm" ${lifecycleLocked?"disabled":""} title="${lifecycleLocked?'Finish maintenance before changing membership.':'Permanently remove this member.'}" onclick='removeNode(${esc(JSON.stringify(n.node_id))})'>${n.removal_pending?"Retry removal":"Remove permanently"}</button>`;
  const direct=PlurxClusterPanel.clusterDirectMaintenanceStatus(context.operations,n.node_id);
  const sessions=direct&&direct.media?Number(direct.media.local_active_sessions||0):Number(n.active_media_sessions||0);
  const lag=n.apply_lag_entries==null?"unavailable":`${n.apply_lag_entries} entr${n.apply_lag_entries===1?"y":"ies"}`;
  const transportProgress=PlurxClusterPanel.clusterTransportExplanation(
    context.operations,n.raft_id,n.bounded_read_ready,clusterOpsElapsed(context.operations));
  const note=n.maintenance
    ? directReady
      ? "Fenced, caught up, and directly observed with no local work. Resume from this node after the update or reboot."
      : `New work is fenced. ${sessions?`${sessions} local media operation${sessions===1?" is":"s are"} still draining.`:"Waiting for direct acknowledgement, leader handoff, catch-up, or drain proof."}`
    : !n.bounded_read_ready&&n.role==="learner"
      ? `Bounded-read permit remains off: ${transportProgress.text}. Transport progress never grants serving authority.`
      : "Maintenance is reversible and does not remove this node from membership.";
  return `<details class="clnode${n.maintenance?" maintenance":""}${n.reachable?"":" stale"}" open ontoggle="syncClusterNodeToggle()" onclick="clusterNodeFoldLater(this)">
    <summary><div class="clnodesummary"><div class="clidentity"><div class="clhostline"><span class="clhost">${esc(hostname)}</span>${leader}${local}${pending}${maintenance}${worker}</div>
      ${advertised}<div class="clid">Node ID ${esc(n.node_id)}</div></div><div class="clnodesummaryright">${role}${clusterNodeOperationsBadge(n,context.operations)}</div></div></summary>
    <div class="clnodebody" data-node="${esc(n.node_id)}"><div class="clnodegroups"><section class="clnodegroup"><div class="clgrouptitle">Membership</div>
      <div class="clnodemeta"><div><span>Heartbeat</span>${heartbeat} · ${seen}</div><div><span>Raft ID</span>${esc(String(n.raft_id))}</div>
        <div><span>Apply lag</span>${esc(lag)}</div><div><span>Active streams</span>${sessions}</div></div></section>
      ${clusterNodeOperationsHtml(n,context.operations)}</div>
      <div class="clnodefoot"><div class="hint">${esc(note)}</div><div class="row">${promote}${maintenanceAction}${remove}</div></div></div>
  </details>`;
}
function clusterRecoveryPanel(cluster,recovery,active,ops){
  const voters=(cluster.nodes||[]).filter(n=>n.is_voter);
  const maintenanceActive=Boolean(active);
  const electionCandidate=voters.some(n=>n.reachable&&n.apply_lag_entries===0&&!n.removal_pending);
  const electionReady=recovery.quorum_available&&electionCandidate&&!maintenanceActive;
  const title=recovery.quorum_available?"Leader election required":"Recovery required";
  const badge=recovery.quorum_available?"Leader unavailable":"Quorum unavailable";
  const rows=voters.map(n=>`<div class="clvoter"><span>${esc(n.hostname||n.node_id)}${n.is_leader?" · last known leader":""}</span>
    <b style="color:${n.reachable?'var(--good)':'var(--bad)'}">${n.reachable?"heartbeat fresh":"unreachable / stale"}</b></div>`).join("");
  const recoverySummary=recovery.quorum_available
    ? maintenanceActive
      ? "A voter majority is reachable, but maintenance is still active. Restore and resume the fenced target before requesting an election."
      : "A voter majority is reachable, but no leader is currently elected. A bounded election is safe to request."
    : "The voter majority cannot be proved. Normal cluster mutations are locked.";
  const maintained=active?`<div class="claction"><b>Restore the maintenance target first: ${esc(active.hostname||active.node_id)}</b><p>Its durable work fence is still active. Start this original node with its existing data and identity, then open it directly to verify catch-up and local drain before resuming service.</p></div>${clusterMaintenanceProgress(active,ops,cluster.local_node_id,true)}`:"";
  return `<section class="cluster-card clrecovery"><div class="clophead"><div><h3>${title}</h3>
      <p>${recoverySummary}</p></div><span class="pill" style="color:var(--bad);border-color:var(--bad)">${badge}</span></div>
    <div class="recovery-count">${recovery.reachable_voters} / ${recovery.required_voters}</div><p>reachable voters needed for quorum</p>
    <div class="clvoters">${rows}</div>
    ${maintained}
    ${recovery.quorum_available?`<div class="claction"><b>Request an election</b><p>A healthy caught-up voter will campaign. Raft still requires the reachable majority and chooses the winner.</p><div class="row"><button class="ghost sm" ${electionReady?"":"disabled"} onclick="openElectionDialog()">Force election</button></div></div>${forceElectionDialog(electionReady)}`:
    `<div class="claction"><b>Recommended recovery</b><p>Restore any missing original voter with its existing data directory and identity. Do not initialize a replacement cluster over these files.</p></div>
    <div class="claction"><b>Permanent majority loss</b><p>This release does not support force-reconfiguring around lost voters. Preserve every surviving data directory before taking any recovery action.</p>
      <div class="row"><button class="ghost sm" onclick="downloadClusterSupportBundle(this)">Export support bundle</button>
      <button class="ghost sm" onclick="downloadClusterDiagnostics()">Download roster snapshot</button>
      <button class="ghost sm" onclick="toggleRecoveryChecklist(this)">Preservation checklist</button></div>
      <div id="clpreserve" class="hint" hidden>Stop writes, copy each surviving data directory independently, record node IDs and Raft IDs, and keep the originals untouched until recovery is verified.</div></div>
    <button class="ghost sm" disabled title="Raft cannot elect a leader without quorum">Force election unavailable without quorum</button>`}
  </section>`;
}
function clusterMaintenanceProgress(active,ops,localNodeId,recovering=false){
  const direct=PlurxClusterPanel.clusterDirectMaintenanceStatus(ops,active.node_id);
  const media=direct&&direct.media;
  const sessions=media?Number(media.local_active_sessions||0):Number(active.active_media_sessions||0);
  const directReady=PlurxClusterPanel.clusterMaintenanceReady(active,ops);
  const resumeReady=PlurxClusterPanel.clusterMaintenanceResumeReady(active,ops);
  const localTarget=active.node_id===localNodeId;
  return `<ol class="clsteps"><li class="${active.maintenance_acknowledged?'done':''}">Target acknowledged the replicated work fence</li>
    <li class="${!active.is_leader?'done':''}">Leadership is on another voter</li>
    <li class="${active.reachable&&active.apply_lag_entries===0?'done':''}">Node is reachable and fully caught up</li>
    <li class="${media&&media.drained&&Number(media.admissions_in_flight||0)===0?'done':''}">Direct process work drained (${sessions} local operation${sessions===1?'':'s'})</li></ol>
    <div class="claction"><b>${directReady?"Ready to update, reboot, or resume":resumeReady&&active.is_leader?"Ready to hand off leadership and resume":recovering?"Restore this fenced node":"Keep the node running while it drains"}</b><p>The maintenance fence survives a process restart. Resume only from the target node after direct process evidence is current; if it became leader during recovery, resume first hands leadership to a caught-up voter.</p>
      <div class="row"><button class="sm" ${localTarget&&resumeReady?"":"disabled"} title="${localTarget?'Direct drain proof is required.':'Open the maintenance target directly to resume it.'}" onclick='setNodeMaintenance(${esc(JSON.stringify(active.node_id))},false)'>Resume service</button></div></div>`;
}

// ---- cluster settings ----------------------------------------------------
// Saved cluster-wide choices, below the health board. Each graduated from
// Developer on 2026-10-10 with its switch, saved value and advisory rows intact.
function clusterPlacementCard(settings){
  const ready=!!settings.cluster_media_pool_ready;
  return setCard(`${cardHead("Cluster media placement","Place new streams on compatible workers with spare capacity.",'<span class="pill">Cluster</span>')}
    ${togRow("cluster-placement-enabled","Enable remote media placement","Existing sessions keep their owner; new sessions can use another worker.",!!settings.cluster_media_pool_enabled)}
    ${togRow("cluster-takeover-enabled","Enable session takeover","Allow compatible expired sessions to acquire a new owner when remote placement is enabled.",!!settings.cluster_session_takeover_enabled)}
    ${devStaticReq("Peer protocol observations",ready?"met":"not fully met","Compatible reachable peers are considered individually. A stale or older peer never disables the other workers.",ready?"ok":"")}
    ${devStaticReq("Worker resources","checked for each start","A selected worker needs the requested codecs, readable source, writable scratch space and an available hardware or CPU reservation.","")}
    <p class="hint">Requirements are advisory and never prevent saving. Unknown performance measurements affect ranking only. Streams continue through their existing ingress proxy, so ingress bandwidth is still used.</p>
    <div class="err" id="cluster-placement-error" role="alert"></div>${setCardFoot("saveClusterPlacement")}`,{id:"cluster-placement-settings"});
}
async function saveClusterPlacement(btn){
  const err=document.getElementById("cluster-placement-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{
      cluster_media_pool_enabled:/** @type {HTMLInputElement} */(document.getElementById("cluster-placement-enabled")).checked,
      cluster_session_takeover_enabled:/** @type {HTMLInputElement} */(document.getElementById("cluster-takeover-enabled")).checked
    }}));
    const card=document.getElementById("cluster-placement-settings");if(card)card.outerHTML=clusterPlacementCard(saved);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
function boundedCatalogueCard(settings,readiness){
  return setCard(`${cardHead("Local catalogue reads","Use each replica for browsing when its current consistency proof permits it.",'<span class="pill">Cluster reads</span>')}
    ${togRow("bounded-catalogue-reads","Enable local catalogue reads","Falls back to authority when a replica is stale or a watch-write position is unknown.",settings.bounded_replica_reads!==false)}
    ${devReq(readiness,"bounded_catalogue_reads","replica_proof","Fresh replica proof","The current term, quorum watermark and apply lag are checked for each read. Saving this preference does not require a readiness result.")}
    ${devReq(readiness,"bounded_catalogue_reads","watch_floor","Watch state consistency","The web client carries its latest acknowledged watch-write position across nodes for 60 seconds. Other clients retain authority reads until they implement the same echo.")}
    <div class="err" id="bounded-catalogue-error" role="alert"></div>${setCardFoot("saveBoundedCatalogueReads")}`,{id:"bounded-catalogue-settings"});
}
async function saveBoundedCatalogueReads(btn){
  const err=document.getElementById("bounded-catalogue-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{bounded_replica_reads:/** @type {HTMLInputElement} */(document.getElementById("bounded-catalogue-reads")).checked}}));
    const card=document.getElementById("bounded-catalogue-settings");if(card)card.outerHTML=boundedCatalogueCard(saved,DEVELOPER_READINESS);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
function clusterClockCard(settings,readiness){
  const enabled=!!settings.cluster_clock_guard_enforced;
  return setCard(`${cardHead("Cluster clock guard","Refuse clock-dependent cluster decisions when peer clock offsets cannot be bounded.",enabled?`<span class="pill warn">enforced</span>`:`<span class="pill">advisory</span>`)}
    ${togRow("cluster-clock-enforced","Enforce the cluster clock guard","When on, session takeover, the expired-session scan, membership changes and /readyz are refused while clock evidence is unknown or above 2,000 ms. When off, offsets are still measured and what would have been refused is counted. Startup is never refused.",enabled)}
    ${devReq(readiness,"cluster_clock","coverage","Every voter reachable and observed","Every committed remote voter must answer a fresh authenticated probe. A learner that does not answer is reported but not required; a measured learner still counts, and promoting one needs its own bound.")}
    ${devReq(readiness,"cluster_clock","upper_bound","Worst observed offset within 2 s","Absolute offset plus uncertainty; an unknown member has no numeric offset.")}
    ${devReq(readiness,"cluster_clock","ntp","NTP running on every node","Check chronyd or systemd-timesyncd on each node; this server cannot see them.")}
    ${devReq(readiness,"cluster_clock","consequence","What enforcement refuses","While enforced, one down voter blocks takeover, the expiry scan and membership changes on every node until it returns or is removed. A down learner blocks only the takeover of its own sessions, until it returns or is removed.")}
    <p class="hint">Requirements are advisory and never prevent saving, in either direction.</p>
    <div class="err" id="cluster-clock-error" role="alert"></div>${setCardFoot("saveClusterClockGuard")}`,{id:"cluster-clock-settings"});
}
async function saveClusterClockGuard(btn){
  const err=document.getElementById("cluster-clock-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{cluster_clock_guard_enforced:/** @type {HTMLInputElement} */(document.getElementById("cluster-clock-enforced")).checked}}));
    const card=document.getElementById("cluster-clock-settings");if(card)card.outerHTML=clusterClockCard(saved,DEVELOPER_READINESS);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
function clusterBackupCard(settings,readiness){
  const enabled=!!String(settings.backup_destination||"").trim();
  return setCard(`${cardHead("Portable cluster backup","Build a consistent, checksummed archive from a voter snapshot.",`<span class="pill${enabled?" ok":""}">${enabled?"scheduled":"destination empty"}</span>`)}
      <label class="setfield"><span>Destination</span><input id="backup-destination" value="${esc(settings.backup_destination||"")}" placeholder="/mnt/nas/plurx-backups"></label>
      <div class="setgrid"><label class="setfield"><span>Daily UTC time</span><input id="backup-schedule" value="${esc(settings.backup_schedule_utc||"02:30")}" placeholder="02:30"></label>
      <label class="setfield"><span>Artefacts to keep</span><input id="backup-keep" type="number" min="1" max="365" value="${Number(settings.backup_keep)||14}"></label></div>
      <div class="hint"><b>The destination controls scheduling directly.</b> Empty means no scheduled run. The checks below are advisory and never prevent saving or invoking a backup.</div>
      <details class="setdetails" open><summary>Readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"cluster_backup","destination","Existing absolute destination","The daemon must be able to write the configured directory.")}
      ${devReq(readiness,"cluster_backup","off_node_and_space","Off-node storage and free space","Use a different failure domain and retain at least two image sizes of free space.")}
      ${devReq(readiness,"cluster_backup","recent_success","Recent successful archive","A nightly backup is healthy when the last verified publish is less than 26 hours old.")}
      <p class="devcheck-note">Restore first onto an isolated instance. A file that has never been restored is not recovery evidence.</p></div></details>
      <div class="err" id="backup-settings-error" role="alert"></div>${setCardFoot("saveClusterBackup")}`,{id:"cluster-backup-settings"});
}
async function saveClusterBackup(btn){
  const err=document.getElementById("backup-settings-error"); if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      backup_destination:(/** @type {HTMLInputElement} */ (document.getElementById("backup-destination"))).value,
      backup_schedule_utc:(/** @type {HTMLInputElement} */ (document.getElementById("backup-schedule"))).value,
      backup_keep:Number((/** @type {HTMLInputElement} */ (document.getElementById("backup-keep"))).value)
    }}));
    toast("Cluster backup settings saved"); if(btn)setCardSaved(btn);
  }catch(error){if(err)err.textContent=error.message;if(btn)btn.disabled=false;}
}
function subtitleClusterSourcesCard(s,readiness){
  const enabled=!!s.subtitle_cluster_sources;
  return setCard(`${cardHead("Share stored subtitle tracks","Join one cluster extraction and copy a verified track from a peer.",`<span class="pill${enabled?" ok":""}">${enabled?"enabled":"off"}</span>`)}
      ${togRow("subcluster",`Use cluster subtitle sources`,`Applies to new subtitle flights. Your saved choice controls use; the checks below only advise.`,enabled)}
      <details class="setdetails"><summary>What to confirm before enabling</summary><div class="setdetails-body">
      ${devReq(readiness,"subtitle_cluster_sources","analysis_queue","Analysis queue is enabled","The cluster analysis queue must be available for cold sources.")}
      ${devReq(readiness,"subtitle_cluster_sources","schema_v47","All voters support schema 47","Deploy schema 47 on every voter before enqueuing the subtitle-source component.")}
      ${devReq(readiness,"subtitle_cluster_sources","reachable_peer","A media peer is reachable","A holder can serve verified tracks over the authenticated private media route.")}
      ${devReq(readiness,"subtitle_cluster_sources","local_cache","The subtitle store is local","Keep staging and stored tracks on a local filesystem.")}
      ${devReq(readiness,"subtitle_cluster_sources","free_space","The store has room","Leave capacity for stored tracks and a publish stage.")}
      <p class="devcheck-note">Advisory only. These observations never disable the switch or override your saved choice.</p>
      </div></details><div class="err" id="subclustererr" role="alert"></div>${setCardFoot("saveSubtitleClusterSources")}`,{id:"subclustercard"});
}
async function saveSubtitleClusterSources(btn){
  const err=document.getElementById("subclustererr");err.textContent="";if(btn)btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{subtitle_cluster_sources:(/** @type {HTMLInputElement} */ (document.getElementById("subcluster"))).checked}}));
    const card=document.getElementById("subclustercard");if(card)card.outerHTML=subtitleClusterSourcesCard(saved,DEVELOPER_READINESS);
    toast("Cluster subtitle source setting saved");if(btn)setCardSaved(btn);
  }catch(error){err.textContent=error.message;if(btn)btn.disabled=false;}
}
// The health board paints from the roster alone; these cards need the saved
// settings, which arrive as a secondary read and fill this mount when they do.
function clusterSettingsSection(d){
  // A failed readiness read is not cached as data, so fall back to the last
  // document the page received (which records the failure) rather than draw
  // every row as "checking…" forever.
  const settings=d&&d.settings;
  const head=`<div class="setsection"><h2 id="cluster-settings-title">Cluster settings</h2><p>Where new streams run, how replicas serve browsing, clock enforcement, backups and shared subtitle tracks. Readiness rows are advisory and never block a Save.</p></div>`;
  if(!settings) return `<section class="cluster-settings" id="cluster-settings" data-state="loading" aria-labelledby="cluster-settings-title">${head}<div class="empty">Loading cluster settings…</div></section>`;
  const readiness=d.developerReadiness||DEVELOPER_READINESS;
  return `<section class="cluster-settings" id="cluster-settings" data-state="ready" aria-labelledby="cluster-settings-title">${head}
    ${clusterPlacementCard(settings)}${boundedCatalogueCard(settings,readiness)}${clusterClockCard(settings,readiness)}${clusterBackupCard(settings,readiness)}${subtitleClusterSourcesCard(settings,readiness)}</section>`;
}
function clusterTransportRecoveryCard(readiness){
  return setCard(`${cardHead("Transport recovery","Automatic recovery for interrupted cluster transfers. No enable switch is required.",`<span class="pill ok">automatic</span>`)}
      <details class="setdetails"><summary>Deployment guidance and readiness</summary><div class="setdetails-body">
      <p class="hint"><b>Safe use is a deployment decision, not a code gate.</b> Keep a ready voter majority, deploy one node at a time, and expose the cluster API only on the trusted cluster network.</p>
      ${devReq(readiness,"cluster_transport_recovery","recovery_budgets","Recovery budgets","Keep chunk, whole-transfer, final-install, startup, and container-health allowances aligned. Defaults are 30 s, 1,200 s, and 120 s; measured state size may require a longer final-install allowance.")}
      ${devReq(readiness,"cluster_transport_recovery","cluster_api_advertised","Authenticated diagnostics","Every node must advertise its private Hiqlite API address, share the cluster API credential, and keep <code>/cluster/transport/sqlite</code> off the public Internet.")}
      ${devReq(readiness,"cluster_transport_recovery","cache_revocation_capability","Cache revocation","Every member of the exact committed configuration must prove the capability in its current heartbeat before cache-only recovery authorization opens.")}
      ${devReq(readiness,"cluster_transport_recovery","recovery_receipt","Recovery receipt","Qualify the real-TLS 3 MiB transport matrix, acknowledged writes, and twenty learner plus twenty voter recovery cycles before fleet rollout.")}
      <p class="devcheck-note">Use the supported one-node-at-a-time deployment when these checks are acceptable. Readiness is advisory; transport recovery remains automatic on clustered nodes.</p>
      </div></details>`);
}
// The section's read failed. Its own retry, so the board above never reloads
// for it; the state marker keeps a repaint from drawing "Loading…" over it.
function clusterSettingsUnavailable(message){
  return `<section class="cluster-settings" id="cluster-settings" data-state="error" aria-labelledby="cluster-settings-title"><div class="setsection"><h2 id="cluster-settings-title">Cluster settings</h2></div><div class="hint">Cluster settings are unavailable: ${message}</div><div class="row"><button class="ghost sm" onclick="retryClusterSettings(this)">Retry</button></div></section>`;
}
async function retryClusterSettings(button){
  const generation=PAGE_RENDER_GENERATION;
  if(button) button.disabled=true;
  try{
    await loadSettingsKey("settings",generation);
    if(settingsCurrent(generation,"cluster")) patchSettingsSecondary("cluster","settings",SETTINGS_DATA.settings);
  }catch(error){
    if(error&&error.status===401) throw error;
    if(settingsCurrent(generation,"cluster")) patchSettingsSecondaryError("cluster","settings",error);
  }
}
