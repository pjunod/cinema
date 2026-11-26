"use strict";
// ---- the replicated database ---------------------------------------------
// The store is the other half of this screen. Nothing here is a new
// measurement: the ledger's rows come from `cluster.replication` and this
// node's own row of the direct status, and the arithmetic that turns them into
// sentences lives in cluster-panel.js. This is the card they are painted into:
// a plain two-column ledger with the store's verdict in its header.
function clusterDatabasePanel(cluster,replication,ops){
  const health=PlurxClusterPanel.clusterDatabaseHealth(replication);
  const rows=PlurxClusterPanel.clusterDatabaseRows(clenv(),cluster,replication,ops)
    .map(([label,value,numeric,id])=>`<dt>${esc(label)}</dt><dd${numeric?' class="num"':""}${
      id?` id="${esc(id)}"`:""}>${value}</dd>`).join("");
  return `<section class="clcard" id="cluster-database"><div class="clcardhead"><h2>Replicated database</h2>${
      PlurxClusterPanel.clusterHealthPill(clenv(),health.tone,health.label)}</div>
    <dl class="clledger">${rows}</dl></section>`;
}
