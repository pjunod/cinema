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
// Which node rows have their drawer open. Module state like the rail's
// expansions, so a status repaint redraws them open; never written to storage,
// and dropped when the operator leaves the section.
let CLUSTER_OPEN_NODES=[];
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
// renderSettings() rewrites the whole tab. Open drawers, the expansions and the
// join token are module state and come back on their own; what lives only in
// the markup — every select and input the operator has touched, and which of
// them has focus — is captured here and put back. Every repaint path on this
// tab goes through here so all of them behave the same way. Text selection
// cannot be preserved across innerHTML at all — which is exactly why repaints
// are change-driven rather than periodic.
function repaintClusterPreserving(paint){
  const controls=clusterControlValues();
  const active=document.activeElement;
  const focused=active&&active.id&&active.closest&&active.closest(".clusterwrap")?active.id:null;
  paint();
  clusterRestoreControlValues(controls);
  if(focused){
    const again=document.getElementById(focused);
    if(again&&again!==document.activeElement&&again.focus) again.focus();
  }
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
// The readings that move without a new fetch. When neither projection has
// changed there is no repaint, so these cells are patched on their own, on
// every tick; without it the rows whose only job is freshness would say "now"
// (or "4m ago") indefinitely.
function patchClusterReadingAge(){
  const data=SETTINGS_DATA||{};
  const cell=document.getElementById("cldb-reading-age");
  if(cell) cell.innerHTML=PlurxClusterPanel.clusterReadingAge(clenv(),data.cluster,data.clusterOps);
  const sampled=document.getElementById("cl-sampled");
  if(sampled) sampled.textContent=PlurxClusterPanel.clusterSampledText(data.clusterOps);
  const cluster=data.cluster;
  if(!cluster||cluster.unavailable) return;
  const converged=document.getElementById("cldb-converged");
  const at=cluster.replication&&cluster.replication.last_converged_at;
  if(converged&&at) converged.textContent=fmtAgo(at);
  const nodes=cluster.nodes||[];
  document.querySelectorAll("[data-clhb]").forEach(el=>{
    const n=nodes.find(row=>row.node_id===el.getAttribute("data-clhb"));
    if(!n) return;
    if(el.tagName==="TD") el.innerHTML=PlurxClusterPanel.clusterNodeCells(clenv(),n,data.clusterOps).heartbeat.text;
    else el.textContent=n.last_seen_at?fmtAgo(Math.floor(n.last_seen_at/1000)):"never";
  });
}
// One node's drawer: the three ledgers that used to be its card — membership,
// Raft and WAL, snapshot and media — then that node's own actions. Every
// reading is the direct status's; the roster's heartbeat is never a
// substitute for a direct peer sample, so an unobserved node says so instead.
function clusterNodeLedger(title,rows){
  return `<div><h4>${title}</h4><dl class="kvgrid">${rows.map(([label,value])=>`<dt>${label}</dt><dd>${value}</dd>`).join("")}</dl></div>`;
}
function clusterNodeDrawer(n,cluster,ops){
  const P=PlurxClusterPanel;
  const unknown=`<span class="muted">unknown</span>`;
  const ago=ms=>ms?esc(fmtAgo(Math.floor(ms/1000))):"";
  const lag=n.apply_lag_entries==null?"unavailable":`${n.apply_lag_entries} entr${n.apply_lag_entries===1?"y":"ies"}`;
  const membership=[["Node ID",`<span class="clmono">${esc(n.node_id)}</span>`],["Raft ID",esc(String(n.raft_id))],
    ["Advertised",n.advertised_host?esc(n.advertised_host):unknown],
    ["Heartbeat",`${n.reachable?`<span class="clgood">fresh</span>`:`<span class="clbad">stale</span>`} · <span data-clhb="${esc(n.node_id)}">${n.last_seen_at?ago(n.last_seen_at):"never"}</span>`],
    ["Apply lag",esc(lag)]];
  const transport=P.clusterTransportExplanation(ops,n.raft_id,n.bounded_read_ready,clusterOpsElapsed(ops));
  const seen=transport.observation;
  const transportText=seen
    ? `${esc(transport.text)} · observer ${esc(String(seen.observing_node_id))} · ${esc(P.clusterOperationAge(seen.sample_age_ms))}`
    : `<span class="muted">${esc(transport.text)}</span>`;
  const row=P.clusterDirectOperationRow(ops,n.node_id), s=row&&row.observation==="answered"?row.status:null;
  let body;
  if(!s){
    const why=!ops?"Collecting direct process, serving, Raft, WAL, snapshot, media, and build evidence…"
      :ops.unavailable?"Direct status is unavailable. Keep every available voter running and retry status above."
      :`Not observed · ${esc(P.clusterObservationReason(row&&(row.error_class||row.observation)||"not_observed"))}. Membership heartbeat state is not a substitute for a direct peer sample.`;
    body=`${clusterNodeLedger("Membership",membership)}${clusterNodeLedger("Raft &amp; WAL",[["Direct status",`<span class="${ops&&!ops.unavailable?"clbad":"muted"}">${why}</span>`]])}${
      clusterNodeLedger("Snapshot &amp; media",[["Transport",transportText]])}`;
  }else{
    const raft=s.raft||{}, wal=s.wal&&s.wal.snapshot, snap=s.snapshot||{}, media=s.media||{};
    const known=value=>value!==null&&value!==undefined;
    const num=value=>known(value)?esc(String(value)):unknown;
    const wire=P.clusterWireFacts(ops,n.node_id), skew=P.clusterWireSkew(ops);
    const raftRole=raft.is_leader===true?"Leader":raft.is_leader===false?"Follower":"Unknown role";
    membership.push(
      ["Process",s.process&&s.process.live?`<span class="clgood">live</span> · ${esc(P.clusterOperationAge(row.sample_age_ms))}`:`<span class="clbad">not live</span>`],
      ["Serving",s.serving&&s.serving.ready?`<span class="clgood">ready</span>`:`<span class="clbad">fenced: ${esc(P.clusterOperationReason(s.serving&&s.serving.reason))}</span>`],
      ["Protocol",`${num(s.protocol_min)}–${num(s.protocol_max)} · ${wire.protocol===null?`<span class="clwarn">media protocol unreported</span>`:`media protocol ${esc(String(wire.protocol))}`}${
        skew.protocolSkew?` · <span class="clbad">Protocol skew: ${esc(skew.protocols.join(" / "))} — placement splits into cohorts</span>`:""}`],
      ["Build",`<span class="clmono">${esc(s.build)}</span>`]);
    const walHealthy=P.clusterWalHealthy(wal);
    const raftRows=[
      ["Raft",raft.sample_valid&&raft.watermark_valid
        ? `${raftRole} · term ${num(raft.current_term)} · lag ${known(raft.apply_lag_entries)?esc(String(raft.apply_lag_entries)):"unknown"}`
        : `<span class="clbad">${raftRole} · stale or incomplete proof</span>`],
      ["Applied / commit",`${num(raft.applied_index)} / ${num(raft.commit_index)}`],
      ["Raft sample",`${esc(P.clusterOperationAge((raft.sample_age_seconds||0)*1000))}; watermark ${esc(P.clusterOperationAge(raft.watermark_age_millis))}`],
      ["WAL",wal?`<span class="${walHealthy?"clgood":"clbad"}">${esc(P.clusterOperationReason(wal.state))}</span> · ${esc(String(wal.segment_count))} segment${wal.segment_count===1?"":"s"} · ${esc(fmtBytes(wal.allocated_bytes)||"0 B")}`:`<span class="clbad">unavailable</span>`],
      ["Durable index",wal&&known(wal.last_durable_index)?esc(String(wal.last_durable_index)):unknown],
      ["Retained / purged",wal?`${known(wal.first_retained_index)?esc(String(wal.first_retained_index)):"unknown"} / ${known(wal.last_purged_index)?esc(String(wal.last_purged_index)):"none"}`:unknown],
      ["Last WAL sync",wal&&wal.last_sync_unix_ms?ago(wal.last_sync_unix_ms):unknown],
      ["Last WAL error",esc(wal&&wal.last_error?wal.last_error.message:"none")],
      ["Last recovery",esc(wal&&wal.last_recovery?`${wal.last_recovery.operation}: ${wal.last_recovery.outcome}`:"none observed")]];
    const db=known(snap.db_bytes)?esc(fmtBytes(snap.db_bytes)||"0 B"):unknown;
    const sessions=Number(media.local_active_sessions||0);
    const snapshotRows=snap.available?[
      ["Database",db],
      ["Last snapshot",snap.last_build_outcome?`build ${esc(snap.last_build_outcome)} · ${snap.last_build_unix_ms?ago(snap.last_build_unix_ms):"at unknown time"}`:"none observed"],
      ["Snapshot totals",`build ${esc(String(snap.build_ok_count||0))} ok / ${esc(String(snap.build_error_count||0))} error; install ${esc(String(snap.install_ok_count||0))} ok / ${esc(String(snap.install_error_count||0))} error`],
      ["Last install",snap.last_install_outcome?`${esc(snap.last_install_outcome)} · ${snap.last_install_unix_ms?ago(snap.last_install_unix_ms):"at unknown time"}`:"none observed"]]
      : [["Snapshot",`<span class="clbad">unavailable</span>`]];
    snapshotRows.push(
      ["Streams owned",`${esc(String(sessions))} · ${media.drained?"drained":"draining"}`],
      ["Placement",wire.eligible===null?unknown:wire.eligible?`<span class="clgood">eligible owner</span>`:`<span class="clwarn">not an owner: ${esc(wire.reason||"unknown")}</span>`],
      ["Transport",transportText],
      ["Direct play","unknown here — drain proxy connections separately"]);
    body=`${clusterNodeLedger("Membership",membership)}${clusterNodeLedger("Raft &amp; WAL",raftRows)}${clusterNodeLedger("Snapshot &amp; media",snapshotRows)}`;
  }
  return `<div class="cldrawer">${body}<div class="cldraweract">${clusterNodeActionsHtml(n,cluster,ops)}</div></div>`;
}
// The action row. A blocked action shows its reason beside it; a blocked
// expansion renders no expansion (its reason is its whole content).
function clusterNodeActionsHtml(n,cluster,ops){
  const actions=PlurxClusterPanel.clusterNodeActions(clenv(),cluster,ops,n);
  const local=n.node_id===cluster.local_node_id;
  const why=a=>a.blocked&&a.reason?`<span class="clwhy">${a.reason}</span>`:"";
  // Each action is one item, its button and its reason together, so a wrap
  // never parts a blocked button from the sentence that explains it.
  const item=(a,html,cls="")=>`<span class="clactitem${cls}">${html}${why(a)}</span>`;
  const lead=local?"":`<span class="clwhy">Maintenance and restart run from the node itself.</span>`;
  const plain=actions.filter(a=>!a.destructive).map(a=>item(a,a.html)).join("");
  let first=true;
  const destructive=actions.filter(a=>a.destructive).map(a=>{
    const cls=first?" clpush":""; first=false;
    if(a.expand!=="leave") return item(a,a.html,cls);
    const open=!a.blocked&&CLUSTER_RAIL_EXPANDED.indexOf("leave")!==-1;
    const button=`<button class="danger sm" ${a.blocked?"disabled":`aria-controls="clexp-leave" aria-expanded="${open}"`} onclick="toggleClusterExpansion('leave')">${open?"Hide":"Leave cluster…"}</button>`;
    return `${item(a,button,cls)}${a.blocked?"":`<div class="clexpansion" id="clexp-leave"${open?"":" hidden"}>${leavePanel(cluster.local_node_id)}</div>`}`;
  }).join("");
  return `${lead}${plain}${destructive}`;
}
// The nodes table: one row per member, every column a direct reading, a dash
// where there is none. A real table in the .adminwrap conventions, so the
// phone-width rules reflow each row into a labelled card. It grows with the
// roster; nothing here scrolls inside the page.
function clusterTone(tone){ return tone==="good"?"clgood":tone==="warn"?"clwarn":tone==="bad"?"clbad":""; }
function clusterNodeTableRow(n,cluster,ops){
  const cells=PlurxClusterPanel.clusterNodeCells(clenv(),n,ops);
  const open=CLUSTER_OPEN_NODES.indexOf(n.node_id)!==-1;
  const id=esc(n.node_id);
  const role=n.is_voter
    ? `<span class="pill acc">voter</span>`
    : n.role==="voter"
      ? `<span class="pill" title="Admitted as a voter and currently joining or catching up. It is not counted toward quorum until Raft commits its vote.">joining voter</span>`
      : `<span class="pill" title="A non-voting capacity role. A learner receives replication and may serve gated work; explicit promotion adds its vote only after catch-up and storage preflight.">learner</span>`;
  const pills=`${n.is_leader?`<span class="pill acc">Leader</span>`:""}${n.node_id===cluster.local_node_id?`<span class="pill">This node</span>`:""}${
    n.removal_pending?`<span class="pill warn">Removal pending</span>`:""}${!n.is_voter&&n.role==="learner"&&n.bounded_read_ready?`<span class="pill ok">read worker</span>`:""}`;
  const cell=(label,c,attrs="")=>`<td data-label="${label}" class="num${c.tone?` ${clusterTone(c.tone)}`:""}"${attrs}>${c.text}</td>`;
  const status=cells.status;
  return `<tr class="clrow${open?" setopen":""}${n.reachable?"":" stale"}" id="clrow-${id}" data-node="${id}" onclick="clusterRowClick(event,this)">
    <td class="clname"><button type="button" class="clnodebtn" data-node="${id}" aria-expanded="${open}" aria-controls="cldrawer-${id}" onclick="toggleClusterNode(this)"><span class="clchev" aria-hidden="true">${open?"▾":"▸"}</span><span class="clhost">${esc(n.hostname||n.node_id)}</span></button>${pills}${
      n.advertised_host?`<div class="clsub">${esc(n.advertised_host)}</div>`:""}</td>
    <td data-label="Role">${role}</td>
    <td data-label="Status"${status.why?` title="${esc(status.why)}"`:""}><span class="cldot ${status.tone}"></span><span class="${clusterTone(status.tone==="good"?"":status.tone)}">${esc(status.label)}</span></td>
    ${cell("Heartbeat",cells.heartbeat,` data-clhb="${id}"`)}${cell("Raft",cells.raft)}${cell("WAL",cells.wal)}${cell("Database",cells.database)}${cell("Streams",cells.streams)}${cell("Build",cells.build)}</tr>
    <tr class="setdrawer" id="cldrawer-${id}" data-node="${id}"${open?"":" hidden"}><td colspan="9">${clusterNodeDrawer(n,cluster,ops)}</td></tr>`;
}
function clusterNodesCard(cluster,ops){
  const nodes=cluster.nodes||[];
  const voters=nodes.filter(n=>n.is_voter).length, others=nodes.length-voters;
  const add=PlurxClusterPanel.clusterAddNodeAction(clenv(),cluster);
  const open=!add.blocked&&CLUSTER_RAIL_EXPANDED.indexOf("add")!==-1;
  return `<section class="clcard" id="cluster-nodes"><div class="clcardhead"><h2>Nodes<small>${nodes.length} member${nodes.length===1?"":"s"} · ${voters} voter${voters===1?"":"s"} · ${others} learner${others===1?"":"s"}</small></h2>
      <div class="row">${add.blocked?`<span class="clwhy">${add.reason}</span>`:""}<button class="ghost sm" ${add.blocked?"disabled":`aria-controls="clexp-add" aria-expanded="${open}"`} onclick="toggleClusterExpansion('add')">${open?"Hide":"+ Add node"}</button></div></div>
    ${add.blocked?"":`<div class="clexpansion" id="clexp-add"${open?"":" hidden"}>${joinPanel()}</div>`}
    ${nodes.length?`<table class="clnodetable"><thead><tr><th>Node</th><th>Role</th><th>Status</th><th>Heartbeat</th><th>Raft</th><th>WAL</th><th>Database</th><th>Streams</th><th>Build</th></tr></thead>
      <tbody id="cluster-node-list">${nodes.map(n=>clusterNodeTableRow(n,cluster,ops)).join("")}</tbody></table>`
      :`<div class="empty">No node records yet.</div>`}</section>`;
}
// Opening a drawer is state, not a repaint: the row and its drawer flip in
// place and the module state records it, so the next status repaint draws it
// open again. More than one may be open. The control is the Node cell's
// button, so the keyboard and a screen reader get a real one.
function toggleClusterNode(button){
  const id=button&&button.getAttribute("data-node");
  if(!id) return;
  const open=CLUSTER_OPEN_NODES.indexOf(id)===-1;
  CLUSTER_OPEN_NODES=open?CLUSTER_OPEN_NODES.concat([id]):CLUSTER_OPEN_NODES.filter(node=>node!==id);
  const drawer=document.getElementById("cldrawer-"+id);
  if(drawer) drawer.hidden=!open;
  const row=document.getElementById("clrow-"+id);
  if(row) row.classList.toggle("setopen",open);
  button.setAttribute("aria-expanded",String(open));
  const chev=button.querySelector(".clchev");
  if(chev) chev.textContent=open?"▾":"▸";
}
// A click anywhere else on the row is the same toggle — unless it landed on a
// control of its own, or it ended a text selection (copying a hostname).
function clusterRowClick(event,row){
  const target=event&&event.target;
  if(target&&target.closest&&target.closest("button,a,input,select,label")) return;
  const selection=typeof getSelection==="function"?getSelection():null;
  if(selection&&!selection.isCollapsed&&String(selection).trim()) return;
  const button=row.querySelector(".clnodebtn");
  if(button) toggleClusterNode(button);
}
// Recovery is the most severe thing this page can say, so it leads the alert
// slot. The voter list is the table's now; this keeps the arithmetic, the
// sentence and the controls.
function clusterRecoveryAlert(cluster,recovery,active,ops){
  const voters=(cluster.nodes||[]).filter(n=>n.is_voter);
  const maintenanceActive=Boolean(active);
  const electionCandidate=voters.some(n=>n.reachable&&n.apply_lag_entries===0&&!n.removal_pending);
  const electionReady=recovery.quorum_available&&electionCandidate&&!maintenanceActive;
  const title=recovery.quorum_available?"Leader election required":"Recovery required";
  const recoverySummary=recovery.quorum_available
    ? maintenanceActive
      ? "A voter majority is reachable, but maintenance is still active. Restore and resume the fenced target before requesting an election."
      : "A voter majority is reachable, but no leader is currently elected. A bounded election is safe to request."
    : "The voter majority cannot be proved. Normal cluster mutations are locked.";
  const maintained=active?`<p><b>Restore the maintenance target first: ${esc(active.hostname||active.node_id)}.</b> Its durable work fence is still active. Start this original node with its existing data and identity, then open it directly to verify catch-up and local drain before resuming service.</p>${clusterMaintenanceProgress(active,ops,cluster.local_node_id,true)}`:"";
  const actions=recovery.quorum_available
    ? `<button class="ghost sm" ${electionReady?"":"disabled"} onclick="openElectionDialog()">Force election…</button>`
    : `<button class="ghost sm" onclick="downloadClusterSupportBundle(this)">Export support bundle</button>
      <button class="ghost sm" onclick="downloadClusterDiagnostics()">Download roster snapshot</button>
      <button class="ghost sm" onclick="toggleRecoveryChecklist(this)">Preservation checklist</button>
      <button class="ghost sm" disabled title="Raft cannot elect a leader without quorum">Force election unavailable without quorum</button>`;
  const guidance=recovery.quorum_available?`<p>A healthy caught-up voter will campaign. Raft still requires the reachable majority and chooses the winner.</p>`:`<p>Restore any missing original voter with its existing data directory and identity. Do not initialize a replacement cluster over these files.
      This release does not support force-reconfiguring around lost voters. Preserve every surviving data directory before taking any recovery action.</p>
    <div id="clpreserve" class="hint" hidden>Stop writes, copy each surviving data directory independently, record node IDs and Raft IDs, and keep the originals untouched until recovery is verified.</div>`;
  return `<div class="clalert bad" role="status"><div><b>${title}: ${recovery.reachable_voters} voter${recovery.reachable_voters===1?"":"s"} reachable, ${recovery.required_voters} needed for quorum.</b> ${recoverySummary}
    ${guidance}${maintained}</div><div class="row">${actions}</div></div>`;
}
// The four steps a maintenance target walks through, inline, then the state
// they add up to and the one control that ends it.
function clusterMaintenanceProgress(active,ops,localNodeId,recovering=false){
  const direct=PlurxClusterPanel.clusterDirectMaintenanceStatus(ops,active.node_id);
  const media=direct&&direct.media;
  const sessions=media?Number(media.local_active_sessions||0):Number(active.active_media_sessions||0);
  const directReady=PlurxClusterPanel.clusterMaintenanceReady(active,ops);
  const resumeReady=PlurxClusterPanel.clusterMaintenanceResumeReady(active,ops);
  const localTarget=active.node_id===localNodeId;
  const step=(done,text)=>`<span class="${done?"done":"wait"}">${text}</span>`;
  return `<div class="clalertsteps">${step(active.maintenance_acknowledged,"Target acknowledged the replicated work fence")}${
      step(!active.is_leader,"Leadership is on another voter")}${
      step(active.reachable&&active.apply_lag_entries===0,"Node is reachable and fully caught up")}${
      step(media&&media.drained&&Number(media.admissions_in_flight||0)===0,`Direct process work drained (${sessions} local operation${sessions===1?"":"s"})`)}</div>
    <div class="clalertnext"><b>${directReady?"Ready to update, reboot, or resume.":resumeReady&&active.is_leader?"Ready to hand off leadership and resume.":recovering?"Restore this fenced node.":"Keep the node running while it drains."}</b>
      <button class="sm" ${localTarget&&resumeReady?"":"disabled"} title="${localTarget?"Direct drain proof is required.":"Open the maintenance target directly to resume it."}" onclick='setNodeMaintenance(${esc(JSON.stringify(active.node_id))},false)'>Resume service</button></div>`;
}
function clusterMaintenanceAlert(active,ops,localNodeId){
  return `<div class="clalert" role="status"><div><b>Maintenance in progress on ${esc(active.hostname||active.node_id)}${active.node_id===localNodeId?" (this node)":""}.</b>
    Membership changes are locked until it resumes. The fence survives a process restart; resume only from the target node.
    ${clusterMaintenanceProgress(active,ops,localNodeId)}</div></div>`;
}

// ---- cluster settings ----------------------------------------------------
// The saved cluster-wide choices, as one list with one Save. Each row is one
// line; its readiness chip counts what the server reported, and expands in
// place to the long description and one line per requirement with the
// server's evidence verbatim. Readiness is advisory: it never disables a
// switch or the Save. Each graduated from Developer on 2026-10-10; the
// sentences below are the ones those cards carried.
// A function rather than a bare const so the definitions travel with the
// painters that read them (tests borrow declarations, not free statements).
function clusterSettingRows(){ return [
  {id:"cluster-placement-enabled",key:"cluster_media_pool_enabled",title:"Remote media placement",
    line:"Start new streams on any compatible worker with spare capacity.",
    more:"Existing sessions keep their owner; new sessions can use another worker. Unknown performance measurements affect ranking only. Streams continue through their existing ingress proxy, so ingress bandwidth is still used. Requirements are advisory and never prevent saving.",
    // Placement's readiness is a saved-settings fact, not a readiness item.
    checks:settings=>[
      {title:"Peer protocol observations",status:settings.cluster_media_pool_ready?"met":"unmet",
        evidence:settings.cluster_media_pool_ready?"met":"not fully met",
        detail:"Compatible reachable peers are considered individually. A stale or older peer never disables the other workers."},
      {title:"Worker resources",status:null,evidence:"checked for each start",
        detail:"A selected worker needs the requested codecs, readable source, writable scratch space and an available hardware or CPU reservation."}]},
  {id:"cluster-takeover-enabled",key:"cluster_session_takeover_enabled",title:"Session takeover",child:true,
    line:"Let another worker pick up an expired session."},
  {id:"bounded-catalogue-reads",key:"bounded_replica_reads",title:"Local catalogue reads",defaultOn:true,
    line:"Serve browsing from each replica while its consistency proof is fresh.",
    more:"Uses each replica for browsing when its current consistency proof permits it, and falls back to authority when a replica is stale or a watch-write position is unknown. Saving this preference does not require a readiness result.",
    item:"bounded_catalogue_reads",reqs:[
      ["replica_proof","Fresh replica proof","The current term, quorum watermark and apply lag are checked for each read."],
      ["watch_floor","Watch state consistency","The web client carries its latest acknowledged watch-write position across nodes for 60 seconds. Other clients retain authority reads until they implement the same echo."]]},
  {id:"cluster-clock-enforced",key:"cluster_clock_guard_enforced",title:"Enforce clock guard",
    line:"Refuse takeover, expiry scans and membership changes when peer clocks can't be bounded to 2 s.",
    more:"When on, session takeover, the expired-session scan, membership changes and /readyz are refused while clock evidence is unknown or above 2,000 ms. When off, offsets are still measured and what would have been refused is counted. Startup is never refused. Requirements are advisory and never prevent saving, in either direction.",
    item:"cluster_clock",reqs:[
      ["coverage","Every voter reachable and observed","Every committed remote voter must answer a fresh authenticated probe. A learner that does not answer is reported but not required; a learner beyond the bound refuses only the takeover of its own sessions and its own promotion."],
      ["upper_bound","Worst observed offset within 2 s","Absolute offset plus uncertainty over voters; an unknown member has no numeric offset."],
      ["ntp","NTP running on every node","Check chronyd or systemd-timesyncd on each node; this server cannot see them."],
      ["consequence","What enforcement refuses","While enforced, one down voter blocks takeover, the expiry scan and membership changes on every node until it returns or is removed. A down or out-of-bound learner blocks only the takeover of its own sessions and its own promotion, until it returns, is back within the bound, or is removed."]]},
  {id:"subcluster",key:"subtitle_cluster_sources",title:"Shared subtitle tracks",
    line:"Copy a verified track from a peer instead of extracting it again.",
    more:"Joins one cluster extraction and copies a verified track from a peer. Applies to new subtitle flights. Your saved choice controls use; the checks below only advise and never disable the switch or override your saved choice.",
    item:"subtitle_cluster_sources",reqs:[
      ["analysis_queue","Analysis queue is enabled","The cluster analysis queue must be available for cold sources."],
      ["schema_v47","All voters support schema 47","Deploy schema 47 on every voter before enqueuing the subtitle-source component."],
      ["reachable_peer","A media peer is reachable","A holder can serve verified tracks over the authenticated private media route."],
      ["local_cache","The subtitle store is local","Keep staging and stored tracks on a local filesystem."],
      ["free_space","The store has room","Leave capacity for stored tracks and a publish stage."]]},
  {id:"cluster-transport-recovery",title:"Transport recovery",always:true,
    line:"Resumes interrupted cluster transfers.",
    more:"Automatic on clustered nodes; no enable switch is required. Safe use is a deployment decision, not a code gate: keep a ready voter majority, deploy one node at a time, and expose the cluster API only on the trusted cluster network. Readiness is advisory.",
    item:"cluster_transport_recovery",reqs:[
      ["recovery_budgets","Recovery budgets","Keep chunk, whole-transfer, final-install, startup, and container-health allowances aligned. Defaults are 30 s, 1,200 s, and 120 s; measured state size may require a longer final-install allowance."],
      ["cluster_api_advertised","Authenticated diagnostics","Every node must advertise its private Hiqlite API address, share the cluster API credential, and keep /cluster/transport/sqlite off the public Internet."],
      ["cache_revocation_capability","Cache revocation","Every member of the exact committed configuration must prove the capability in its current heartbeat before cache-only recovery authorization opens."],
      ["recovery_receipt","Recovery receipt","Qualify the real-TLS 3 MiB transport matrix, acknowledged writes, and twenty learner plus twenty voter recovery cycles before fleet rollout."]]},
]; }
function clusterBackupChecks(){ return {id:"cluster-backup",item:"cluster_backup",
  more:"Builds a consistent, checksummed archive from a voter snapshot. The destination controls scheduling directly: empty means no scheduled run. The checks are advisory and never prevent saving or invoking a backup. Restore first onto an isolated instance — a file that has never been restored is not recovery evidence.",
  reqs:[["destination","Existing absolute destination","The daemon must be able to write the configured directory."],
    ["off_node_and_space","Off-node storage and free space","Use a different failure domain and retain at least two image sizes of free space."],
    ["recent_success","Recent successful archive","A nightly backup is healthy when the last verified publish is less than 26 hours old."]]}; }
// One row's requirement lines: the server's row for each listed requirement,
// or the row's own static facts. `status` null means nothing was reported.
function clusterSettingChecks(row,settings,readiness){
  if(row.checks) return row.checks(settings);
  return (row.reqs||[]).map(([reqId,title,detail])=>{
    const found=devReadinessRow(readiness,row.item,reqId);
    return {title,detail,status:found?found.status:null,evidence:devReadinessEvidence(found,readiness)};
  });
}
// The chip and the text it opens. Both are drawn from the row definition, so
// a readiness document arriving later redraws exactly these and never the
// switches beside them.
function clusterChecksChip(row,checks,readiness){
  const chip=PlurxClusterPanel.clusterReadinessChip(row.checks?{}:readiness,checks);
  return `<span class="clchip${chip.tone?` ${chip.tone}`:""}">${esc(chip.label)}</span>`;
}
function clusterChecksMore(row,checks){
  const mark=status=>status==="met"?`<i class="clgood">✓</i>`:status==="unmet"?`<i class="clwarn">!</i>`:`<i class="muted">·</i>`;
  return `<p>${esc(row.more)}</p>${checks.map(check=>`<div class="clcheck">${mark(check.status)}<div><b>${esc(check.title)}</b>${
    check.evidence?` <span>— ${esc(check.evidence)}</span>`:""}${check.detail?`<small>${esc(check.detail)}</small>`:""}</div></div>`).join("")}`;
}
function clusterChecksHtml(row,settings,readiness){
  if(!row.more) return {chip:"",more:""};
  const checks=clusterSettingChecks(row,settings,readiness);
  return {
    chip:`<button type="button" class="clchipbtn" data-clchip="${row.id}" aria-expanded="false" aria-controls="clmore-${row.id}" onclick="toggleClusterChecks(this)">${clusterChecksChip(row,checks,readiness)}</button>`,
    more:`<div class="clmore" id="clmore-${row.id}" data-clmore="${row.id}" hidden>${clusterChecksMore(row,checks)}</div>`,
  };
}
function toggleClusterChecks(button){
  const more=document.getElementById(button.getAttribute("aria-controls"));
  if(!more) return;
  more.hidden=!more.hidden;
  button.setAttribute("aria-expanded",String(!more.hidden));
}
// A readiness document arrived (or failed) after the settings were drawn:
// redraw the chips and their text in place. The switches, an unsaved edit and
// an open expansion stay exactly as they are.
function patchClusterReadiness(readiness){
  const settings=SETTINGS_DATA&&SETTINGS_DATA.settings;
  if(!settings) return;
  for(const row of [...clusterSettingRows(),clusterBackupChecks()]){
    if(!row.more) continue;
    const checks=clusterSettingChecks(row,settings,readiness);
    const chip=document.querySelector(`[data-clchip="${row.id}"]`);
    if(chip) chip.innerHTML=clusterChecksChip(row,checks,readiness);
    const more=document.querySelector(`[data-clmore="${row.id}"]`);
    if(more) more.innerHTML=clusterChecksMore(row,checks);
  }
}
function clusterSettingRow(row,settings,readiness){
  const checks=clusterChecksHtml(row,settings,readiness);
  const control=row.always
    ? `<span class="clauto">always on</span>`
    : `<input type="checkbox" id="${row.id}"${(row.defaultOn?settings[row.key]!==false:!!settings[row.key])?" checked":""}>`;
  return `<div class="clset${row.child?" child":""}"><label${row.always?"":` for="${row.id}"`}><span class="t">${esc(row.title)}</span><span class="d">${esc(row.line)}</span></label>${checks.chip||"<span></span>"}${control}</div>${checks.more}`;
}
// The mount and its three states. A repaint keeps the live node (see
// SETTINGS_KEPT_MOUNTS), so an unsaved switch, the Save it enabled and an
// open expansion survive every status repaint and a Refresh.
function clusterSettingsSection(d){
  const settings=d&&d.settings;
  const head=`<div class="clcardhead"><h2 id="cluster-settings-title">Cluster settings</h2></div>`;
  if(!settings) return `<section class="clcard" id="cluster-settings" data-state="loading" aria-labelledby="cluster-settings-title">${head}<div class="empty">Loading cluster settings…</div></section>`;
  // A failed readiness read is not cached as data, so fall back to the last
  // document the page received (which records the failure) rather than draw
  // every chip as "checking…" forever.
  const readiness=d.developerReadiness||DEVELOPER_READINESS;
  return `<section class="clcard setcard" id="cluster-settings" data-state="ready" aria-labelledby="cluster-settings-title" oninput="markSetCard(this)" onchange="markSetCard(this)">${head}
    ${clusterSettingRows().map(row=>clusterSettingRow(row,settings,readiness)).join("")}
    <div class="err" id="cluster-settings-error" role="alert"></div>${setCardFoot("saveClusterSettings")}</section>`;
}
// One Save, one PUT, every key on the list.
async function saveClusterSettings(btn){
  const err=document.getElementById("cluster-settings-error"); if(err) err.textContent="";
  btn.disabled=true;
  try{
    /** @type {Record<string,boolean>} */
    const body={};
    for(const row of clusterSettingRows()){
      if(!row.key) continue;
      body[row.key]=/** @type {HTMLInputElement} */(document.getElementById(row.id)).checked;
    }
    const saved=cacheSettings(await api("/settings",{method:"PUT",body}));
    const card=document.getElementById("cluster-settings");
    if(card) card.outerHTML=clusterSettingsSection({settings:saved,developerReadiness:DEVELOPER_READINESS});
    toast("Cluster settings saved");
  }catch(error){ if(err) err.textContent=error.message; btn.disabled=false; }
}
function clusterBackupCard(settings,readiness){
  const head=`<div class="clcardhead"><h2 id="cluster-backup-title">Backup</h2>`;
  if(!settings) return `<section class="clcard" id="cluster-backup" data-state="loading" aria-labelledby="cluster-backup-title">${head}</div><div class="empty">Loading…</div></section>`;
  const enabled=!!String(settings.backup_destination||"").trim();
  const checks=clusterChecksHtml(clusterBackupChecks(),settings,readiness);
  return `<section class="clcard setcard" id="cluster-backup" data-state="ready" aria-labelledby="cluster-backup-title" oninput="markSetCard(this)" onchange="markSetCard(this)">${head}
      <div class="row">${checks.chip}<span class="pill${enabled?" ok":""}">${enabled?"scheduled":"destination empty"}</span></div></div>
    ${checks.more}
    <div class="clfields"><label class="setfield"><span>Destination</span><input id="backup-destination" value="${esc(settings.backup_destination||"")}" placeholder="/mnt/nas/plurx-backups"></label>
      <label class="setfield"><span>Daily (UTC)</span><input id="backup-schedule" value="${esc(settings.backup_schedule_utc||"02:30")}" placeholder="02:30"></label>
      <label class="setfield"><span>Keep</span><input id="backup-keep" type="number" min="1" max="365" value="${Number(settings.backup_keep)||14}"></label></div>
    <div class="err" id="backup-settings-error" role="alert"></div>${setCardFoot("saveClusterBackup")}</section>`;
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
// The settings read failed. Both mounts say so with their own retry, so the
// page above never reloads for it; the state marker keeps a repaint from
// drawing "Loading…" over it.
function clusterSettingsUnavailable(message,id="cluster-settings",title="Cluster settings"){
  return `<section class="clcard" id="${id}" data-state="error"><div class="clcardhead"><h2>${title}</h2></div><div class="clnote">${title} are unavailable: ${message}</div><div class="clfoot"><button class="ghost sm" onclick="retryClusterSettings(this)">Retry</button></div></section>`;
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
