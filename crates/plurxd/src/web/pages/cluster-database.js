"use strict";
// ---- the replicated database ---------------------------------------------
// The store is the other half of this screen. Nothing here is a new
// measurement: the ledger's rows come from `cluster.replication` and this
// node's own row of the direct status, and the arithmetic that turns them into
// sentences lives in cluster-panel.js. This is the card they are painted into.
function clusterDatabasePanel(cluster,replication,capacity,ops){
  const health=PlurxClusterPanel.clusterDatabaseHealth(replication);
  const cap=PlurxClusterPanel.clusterCapacityText(capacity);
  const rows=PlurxClusterPanel.clusterDatabaseRows(clenv(),cluster,replication,ops)
    .map(([label,value,numeric,id])=>`<dt>${esc(label)}</dt><dd${numeric?' class="num"':""}${
      id?` id="${esc(id)}"`:""}>${value}</dd>`).join("");
  return `<section class="cluster-card"><div class="clophead"><div><h3>Replicated database</h3>
      <p>${replication&&replication.backend==="sqlite"?"Where watch state is stored on this machine."
        :"Where watch state is committed, how far behind this machine is, and what the store is doing to stay caught up."}</p></div>
      <div class="row">${PlurxClusterPanel.clusterHealthPill(clenv(),health.tone,health.label)}<button class="ghost sm" id="cldb-toggle"
        aria-controls="cluster-database" aria-expanded="true" onclick="toggleClusterDatabase(this)">Hide</button></div></div>
    <div class="cldbsumm" id="cluster-database-summary" hidden>${PlurxClusterPanel.clusterDatabaseSummary(clenv(),cluster,replication,ops)}</div>
    <div id="cluster-database"><div class="clcontextfacts">
      <div class="clcontextfact"><span>Watch state</span><b>${replication?replicationText(replication):"Status unavailable"}</b><small>The one authoritative reading; the System tab no longer repeats it.</small></div>
      ${capacity?`<div class="clcontextfact"><span>Capacity</span><b>${esc(cap.headline)}</b><small>${esc(cap.detail)}</small></div>`:""}</div>
      <dl class="cldbledger">${rows}</dl></div></section>`;
}
// ---- folding, and remembering it -----------------------------------------
// Which sections are open is a per-browser convenience, so it is kept in this
// browser and never in the cluster. The reading and writing themselves are in
// cluster-panel.js, which takes the storage object as a parameter; the shell
// supplies it here. Merely touching browser storage throws where site data is
// blocked, so that reference is guarded on its own.
function clusterStorage(){ try{ return localStorage; }catch(e){ return null; } }
function clusterFoldState(){
  const store=clusterStorage();
  return store?PlurxClusterPanel.clusterFoldRead(store):null;
}
function clusterFoldSave(patch){
  const store=clusterStorage();
  if(store) PlurxClusterPanel.clusterFoldWrite(store,patch);
}
function setClusterDatabaseFold(open){
  const body=document.getElementById("cluster-database");
  const summary=document.getElementById("cluster-database-summary");
  const btn=document.getElementById("cldb-toggle");
  if(!body||!summary||!btn) return;
  body.hidden=!open; summary.hidden=open;
  btn.textContent=open?"Hide":"Show";
  btn.setAttribute("aria-expanded",String(open));
}
function toggleClusterDatabase(btn){
  const body=document.getElementById("cluster-database");
  if(!body) return;
  const open=body.hidden;
  setClusterDatabaseFold(open);
  clusterFoldSave({database:open});
  if(btn) btn.focus();
}
// The node id lives on the card body rather than on the <details> itself so
// the card's markup still leads with the hostname a person recognizes.
function clusterNodeFoldId(card){
  const body=card&&card.querySelector?card.querySelector(".clnodebody"):null;
  return body?body.getAttribute("data-node"):null;
}
// Chrome fires one `toggle` per <details open> as the panel is parsed, so
// persisting from `ontoggle` would overwrite the stored set with "everything
// open" on every visit — measured, not assumed. A click is a real gesture, and
// the card's open state has flipped by the time the task runs.
function clusterNodeFoldLater(card){
  setTimeout(()=>{
    clusterNodeFoldSave();
    // Opening a card inside a scrolling column can leave its own body below
    // the fold. `nearest` scrolls the column, never the page.
    if(card&&card.open&&card.scrollIntoView) card.scrollIntoView({block:"nearest"});
  },0);
}
function clusterNodeFoldSave(){
  try{
    const closed=[...document.querySelectorAll("#cluster-node-list>.clnode")]
      .filter(card=>!card.open).map(clusterNodeFoldId).filter(Boolean);
    clusterFoldSave({nodes_closed:closed});
  }catch(e){ /* see clusterFoldSave */ }
}
// Run after the panel is written into the document: the markup ships with the
// defaults (database open, every node open) and this restores what the last
// visit left, so a fresh browser and a blocked-storage browser both get the
// defaults rather than an empty screen.
function applyClusterFolds(){
  try{
    const state=clusterFoldState();
    if(state&&typeof state.database==="boolean") setClusterDatabaseFold(state.database);
    if(state&&typeof state.tab==="string"&&document.getElementById("cltab-"+state.tab)) showClusterTab(state.tab);
    if(state&&Array.isArray(state.nodes_closed)){
      [...document.querySelectorAll("#cluster-node-list>.clnode")].forEach(card=>{
        const id=clusterNodeFoldId(card);
        if(id) card.open=state.nodes_closed.indexOf(id)===-1;
      });
    }
    syncClusterNodeToggle();
  }catch(e){ /* see clusterFoldSave */ }
}
