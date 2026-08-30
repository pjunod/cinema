(function (root, factory) {
  const panel = factory();
  if (typeof module === "object" && module.exports) module.exports = panel;
  root.PlurxClusterPanel = panel;
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  // The cluster panel's model: what the Settings → Cluster screen KNOWS,
  // separated from how it paints. Everything here is pure — it reads the
  // `/cluster/nodes` roster and the `/cluster/status` direct fan-out and
  // returns data or an HTML fragment, and it touches neither the DOM nor the
  // app shell's module state. That is the whole boundary, and it is what makes
  // these decisions testable with `require()` instead of a regex that slices
  // functions out of index.html.
  //
  // The shell's own helpers (`esc`, `fmtAgo`, `fmtBytes`) are NOT redeclared
  // here — a second `esc` is a second escaping contract to review. Functions
  // that need them take an `env` object as their first argument; the shell
  // passes `clenv()` and tests pass their own. The dependency surface is
  // therefore written in the signatures.
  //
  // Templates that touch the document (the node cards, the panel shell, the
  // fold glue) deliberately stay inline in index.html: the boundary is "no
  // DOM, no module state", applied honestly rather than perfectly.

  // ---- quorum, state, and refusals ---------------------------------------
  // A Raft majority is floor(n/2)+1, so the number of voters that may be lost is
  // n minus that majority: 1 of 3, 1 of 4, 2 of 5. Said once, here.
  function clusterQuorum(voters){
    const majority=Math.floor(voters/2)+1;
    return {majority, tolerates:Math.max(0,voters-majority)};
  }
  // The state banner as sentences rather than as an enum. Pure on purpose: the
  // promise this panel makes is the words, so the words are what gets tested.
  function clusterStateView(cluster){
    if(cluster&&cluster.unavailable&&cluster.code==="membership_unavailable")
      return {tone:"calm", title:"Not clustered",
        body:"This server keeps watch state in its own database and is not part of a cluster. That is the "+
          "normal setup for a single machine — nothing is missing here and nothing is misconfigured. "+
          "Clustering is opt-in, and a second server is a deliberate step you can take later."};
    if(!cluster||cluster.unavailable)
      return {tone:"warn", title:"Cluster status unavailable",
        body:"The current membership roster could not be read. This does not mean this server has left its "+
          "cluster. Keep the available nodes running and retry the roster; if it still fails, read the "+
          "replicated database section below and inspect the server logs before restarting anything."};
    const nodes=cluster.nodes||[];
    const voters=nodes.filter(n=>n.is_voter).length;
    const learnerNodes=nodes.filter(n=>n.role==="learner"&&!n.is_voter);
    const learners=learnerNodes.length;
    const readyLearners=learnerNodes.filter(n=>n.bounded_read_ready).length;
    const joiningVoters=nodes.filter(n=>n.role==="voter"&&!n.is_voter).length;
    const catching=learners?` ${learners===1?"One node is":`${learners} nodes are`} `+
      `${learners===1?"a learner":"learners"}: ${learners===1?"it receives":"they receive"} `+
      `replication, ${learners===1?"holds":"hold"} no vote, and ${learners===1?"is":"are"} `+
      `not counted toward quorum. ${readyLearners===1?"One":readyLearners} ${readyLearners===1?"is":"are"} currently ready for `+
      `bounded reads and declared node-local media work.`:"";
    const joining=joiningVoters?` ${joiningVoters===1?"One voter is":`${joiningVoters} voters are`} `+
      `joining and catching up: ${joiningVoters===1?"it has":"they have"} no committed vote yet and `+
      `${joiningVoters===1?"is":"are"} not counted toward quorum until Raft promotes `+
      `${joiningVoters===1?"it":"them"}.`:"";
    if(cluster.availability==="degraded_reconfiguration")
      return {tone:"warn", title:"Reconfiguration in progress — not redundant",
        body:"Two voters is a waypoint while a node is being added or removed, never a redundant pair. "+
          "Both machines must be running and must agree on every write and every membership change, so "+
          "this cluster survives no failure — it is less resilient than a single server, not more. "+
          "Add a third node to finish the reconfiguration, and keep both of these running until three "+
          "are present and in sync."+catching+joining};
    if(cluster.availability==="high_availability"){
      const q=clusterQuorum(voters);
      return {tone:"good", title:`Redundant — ${voters} voters`,
        body:`${q.majority} of the ${voters} voters must agree on every write, so the cluster keeps serving `+
          `and recording watch state while ${q.tolerates===1?"one node is":`${q.tolerates} nodes are`} down. `+
          "Removing a node is accepted only while enough voters remain to keep that majority."+catching+joining};
    }
    return {tone:"calm", title:"One node",
      body:"This is the only node in this cluster. Watch state is durable here, and one node is a complete, "+
        "supported configuration rather than a half-finished cluster. Add a second and a third node only "+
        "if you want another machine carrying the same watch state."+catching+joining};
  }
  // Membership lifecycle refusals as sentences that say what to do next.
  // `node_owns_offline_work` is expected to be the common removal refusal, while
  // the learner proof codes are the expected promotion refusals; both need a
  // route forward rather than a statement of fact and a dead end.
  function membershipRefusalText(code,message){
    const REFUSALS={
      removal_would_lose_quorum:
        "This cluster has two voters, and dropping to one is refused. Every membership change from two "+
        "needs both machines, so plurx will not present an unproved downgrade as a rollback. Add a third "+
        "node first, then remove the one you don't want.",
      node_owns_offline_work:
        "This node still has offline download work that could not be resolved safely. Let any active "+
        "downloads from this node finish or delete those packages, stop clients from starting new "+
        "downloads here, then retry the removal. plurx will move work only when a surviving node proves "+
        "it can read the source; otherwise it fails the package cleanly so the client can request it again.",
      cluster_leader_removal_refused:
        "That node is the cluster's current leader, so another voter cannot remove it remotely. Open Cinema "+
        "on the leader itself and use Leave this cluster; it coordinates its own removal with the surviving "+
        "quorum and then shuts down.",
      self_removal_requires_leave:
        "That row is this server. Use Leave this cluster below so the daemon settles local work, commits its "+
        "own removal, drains active connections, and shuts down instead of leaving a tombstoned process online.",
      leave_node_mismatch:
        "The leave confirmation reached a different backend than the roster you confirmed, so nothing was "+
        "removed. Refresh this page and retry against the same node; configure sticky admin requests at the proxy.",
      cluster_node_not_found:
        "This node is not in the cluster's current membership — it may already have been removed from "+
        "another browser or another server. The current roster is being refreshed now.",
      membership_unavailable:
        "This server is not running a replicated store, so it has no cluster membership to change.",
      membership_upgrade_required:
        "Finish upgrading every active cluster node before changing membership, then retry. The reference-counted removal and learner-lifecycle fences are enabled only after every running binary proves it understands them.",
      membership_removal_pending:
        "This node is still protected by a pending removal fence because another, ambiguous, or older-version "+
        "attempt may still commit. Finish upgrading every cluster node and retry this same removal; do not return "+
        "the fenced node to service while its outcome is unresolved.",
      learner_not_ready:
        "This learner has not yet published a fresh zero-lag apply proof. Leave it running until its Read worker badge says ready, then retry promotion.",
      voter_storage_preflight_failed:
        "This learner can serve reads, but its authoritative filesystem did not prove the durable free-space reserve required of a voter. Free space or move the data root, then retry.",
      maintenance_conflict:
        "Maintenance and membership changes are mutually exclusive. Let the active operation settle, refresh the roster, and retry this action.",
      cluster_operation_pending:
        "Another node already owns the cluster's planned-outage lease. Finish or cancel that restart preparation or maintenance operation, then refresh and retry.",
      maintenance_would_lose_quorum:
        "Restarting this voter would leave fewer reachable voters than quorum requires. Restore the missing voter or add capacity before retrying maintenance.",
      maintenance_resume_unsafe:
        "The target process has not yet proved its maintenance acknowledgement, catch-up, and complete local workload drain. Open that node directly, let it settle, and retry there.",
      maintenance_node_mismatch:
        "Open the target node directly. Only that running process can fence and drain its own work or clear its durable maintenance fence.",
      maintenance_preflight_unsafe:
        "Direct cluster observations do not currently prove this node safe to maintain. Keep every available voter running and clear the Operations blockers first.",
      election_quorum_unavailable:
        "Raft cannot elect a leader without a reachable voter majority. Restore an original voter instead of repeatedly forcing elections.",
      election_candidate_unavailable:
        "No reachable follower has published a fresh zero-lag apply proof. Restore or catch up a follower, then retry the election.",
      election_lifecycle_pending:
        "A maintenance, promotion, join, or removal operation is still pending. Let it settle before changing leadership.",
    };
    if(REFUSALS[code]) return REFUSALS[code];
    return message?`The server refused this cluster operation: ${message}`
                  :"The server refused this cluster operation and gave no reason.";
  }

  // ---- reading the direct status -------------------------------------------
  // last_seen_at is Unix MILLISECONDS (docs/OPERATIONS.md); fmtAgo takes seconds.
  // Feeding it milliseconds prints a healthy node as decades stale.
  function clusterDirectMaintenanceStatus(ops,nodeId){
    const row=clusterDirectOperationRow(ops,nodeId);
    return row&&row.observation==="answered"&&row.status?row.status:null;
  }
  function clusterDirectOperationRow(ops,nodeId){
    if(!ops||ops.unavailable) return null;
    return (ops.nodes||[]).find(row=>row.membership&&row.membership.node_id===nodeId)||null;
  }
  function clusterMaintenanceEntryVerdict(ops,nodeId){
    if(!ops||ops.unavailable) return null;
    return (ops.maintenance||[]).find(verdict=>verdict.node_id===nodeId)||null;
  }
  function clusterMaintenanceReady(n,ops){
    const direct=clusterDirectMaintenanceStatus(ops,n.node_id);
    return Boolean(n.maintenance_ready&&direct&&direct.process&&direct.process.live&&
      direct.serving&&direct.serving.reason==="maintenance"&&direct.media&&
      direct.media.drained&&Number(direct.media.admissions_in_flight||0)===0);
  }
  function clusterMaintenanceResumeReady(n,ops){
    const direct=clusterDirectMaintenanceStatus(ops,n.node_id);
    return Boolean(n.maintenance&&n.maintenance_acknowledged&&n.reachable&&
      n.apply_lag_entries===0&&Number(n.active_media_sessions||0)===0&&direct&&
      direct.process&&direct.process.live&&direct.serving&&
      direct.serving.reason==="maintenance"&&direct.media&&direct.media.drained&&
      Number(direct.media.admissions_in_flight||0)===0);
  }
  function clusterRecoveryState(cluster){
    if(cluster.recovery) return cluster.recovery;
    const voters=(cluster.nodes||[]).filter(n=>n.is_voter);
    const required=Math.floor(voters.length/2)+1;
    const reachable=voters.filter(n=>n.reachable).length;
    const leader=voters.some(n=>n.is_leader);
    return {required:voters.length>1&&(reachable<required||!leader),quorum_available:reachable>=required,
      reachable_voters:reachable,required_voters:required,leader_elected:leader,
      permanent_majority_loss_supported:false};
  }

  // ---- the operations rail -------------------------------------------------
  // Every action this screen can take, with its precondition evaluated before
  // the click rather than after it. The server still refuses what it refuses —
  // these rows never widen what is allowed — but a refusal you can read while
  // deciding is worth more than the same paragraph arriving as a failure.
  //
  // Rows only claim what the roster and the direct status can prove. Offline
  // work owned by a node, for instance, is not in either, so "Remove" says what
  // it does know (quorum, leadership, pending changes) and leaves the rest to
  // the server's own answer.
  function clusterRailRow(row){
    const state=row.blocked?"blocked":row.destructive?"destructive":"ready";
    const chip=row.blocked
      ? `<span class="pill" style="color:var(--muted)">Blocked</span>`
      : row.destructive
        ? `<span class="pill" style="color:var(--bad);border-color:var(--bad)">Permanent</span>`
        : `<span class="pill" style="color:var(--good);border-color:var(--good)">Ready</span>`;
    const action=row.action||"";
    return `<div class="clrailrow ${state}"><div class="clrailtext"><div class="t">${row.title}</div>
        <div class="r">${row.reason}</div></div>
      <div class="clrailside">${chip}${action}</div></div>`;
  }
  // A server verdict ships the condition it actually hit. Naming it is the
  // whole point of stating a precondition, so a row reports the blocker rather
  // than a plausible one.
  function clusterRailBlocker(env,blockers,fallback){
    const {esc}=env;
    const first=(blockers||[])[0];
    if(!first) return fallback;
    return `${esc(clusterOperationReason(first.code))}${first.node_id?` on ${esc(first.node_id)}`:""}${
      (blockers||[]).length>1?`, and ${blockers.length-1} more`:""}.`;
  }
  function clusterOperationRows(env,cluster,ops){
    const {esc,fmtBytes}=env;
    const nodes=cluster.nodes||[];
    const name=n=>esc(n.hostname||n.node_id);
    const arg=n=>esc(JSON.stringify(n.node_id));
    const local=nodes.find(n=>n.node_id===cluster.local_node_id);
    const voters=nodes.filter(n=>n.is_voter);
    const q=clusterQuorum(voters.length);
    const pending=nodes.find(n=>n.removal_pending);
    const fenced=nodes.find(n=>n.maintenance);
    const lifecycle=fenced||pending;
    const recovery=clusterRecoveryState(cluster);
    const rows=[];

    // Adding a node mints a credential, so the row routes to the panel that
    // shows it once rather than minting from here.
    // Recovery locks every membership change: issuing a token and removing a
    // member both need a leader and a committed quorum read.
    const locked=recovery.required?{reason:"The cluster cannot prove both an elected leader and a reachable voter majority, so a membership change cannot commit."}:null;
    rows.push(locked
      ? {title:"Add a node",blocked:true,reason:locked.reason,
         action:`<button class="ghost sm" disabled>Add</button>`}
      : lifecycle
      ? {title:"Add a node",blocked:true,
         reason:`A membership change is already in flight on ${name(lifecycle)}. Finish it before admitting another node.`,
         action:`<button class="ghost sm" disabled>Add</button>`}
      : {title:"Add a node",
         reason:"Mints one single-use join token, shown once, for a machine with a fresh data directory.",
         action:`<button class="ghost sm" onclick="openClusterDanger()">Add</button>`});

    // Promotion is refused until the learner has published a fresh zero-lag
    // apply proof and its filesystem has proved the voter reserve. Both are on
    // the roster, so both can be said here instead of after the attempt.
    nodes.filter(n=>!n.is_voter&&n.role==="learner").forEach(n=>{
      const ready=n.bounded_read_ready&&n.voter_storage_ready&&!lifecycle;
      const why=lifecycle?`A membership change is in flight on ${name(lifecycle)}.`
        :!n.bounded_read_ready?"It has not published a fresh zero-lag apply proof yet — leave it running until its read-worker badge says ready."
        :`Its data root has not proved the durable free-space reserve a voter must hold${
            n.storage_headroom_bytes!=null?` (${esc(fmtBytes(n.storage_headroom_bytes)||"0 B")} free)`:""}. Free space or move the data root.`;
      rows.push({title:`Promote ${name(n)} to voter`,blocked:!ready,
        reason:ready?"Catch-up and the storage preflight both pass; this adds one vote to quorum.":why,
        action:`<button class="ghost sm" ${ready?"":"disabled"} onclick='promoteNode(${arg(n)})'>Promote</button>`});
    });

    // Maintenance is a local action by design: the fence has to be acknowledged
    // by the process being fenced.
    if(fenced){
      const resume=clusterMaintenanceResumeReady(fenced,ops);
      const here=fenced.node_id===cluster.local_node_id;
      rows.push({title:`Resume service on ${name(fenced)}`,blocked:!(here&&resume),
        reason:here?(resume?"Fenced, caught up, and directly observed with no local work left."
          :"Still draining: waiting for direct acknowledgement, leader handoff, catch-up, or drain proof.")
          :`Open ${name(fenced)} directly to resume it — a node lifts its own fence.`,
        action:`<button class="sm" ${here&&resume?"":"disabled"} onclick='setNodeMaintenance(${arg(fenced)},false)'>Resume</button>`});
    }else if(local){
      const verdict=clusterMaintenanceEntryVerdict(ops,local.node_id);
      const safe=Boolean(verdict&&verdict.safe_to_enter);
      const ready=safe&&local.reachable&&!local.removal_pending&&!pending;
      const why=!local.reachable?"This node's own heartbeat is not fresh."
        :pending?`A removal is pending on ${name(pending)}.`
        :verdict?clusterRailBlocker(env,verdict.blockers,"The direct maintenance preflight has not approved this node.")
        :ops&&ops.unavailable?"The direct status could not be collected, and a heartbeat is not a substitute for it."
        :"The direct maintenance preflight has not answered yet.";
      rows.push({title:`Enter maintenance on ${name(local)}`,blocked:!ready,
        reason:ready?"Fences new work, hands off leadership, and drains existing streams. Reversible; membership is unchanged."
          :why,
        action:`<button class="ghost sm" ${ready?"":"disabled"} onclick='setNodeMaintenance(${arg(local)},true)'>Fence</button>`});
    }

    // Election and restart preparation are cluster-wide and already carry a
    // server-side verdict; the rail just stops hiding it behind the button.
    if(voters.length>1){
      const candidate=voters.some(n=>!n.is_leader&&n.reachable&&n.apply_lag_entries===0&&!n.removal_pending);
      const ready=candidate&&!lifecycle&&recovery.quorum_available;
      const why=!recovery.quorum_available?"A voter majority cannot be proved, and an election cannot bypass quorum."
        :lifecycle?`A membership change is in flight on ${name(lifecycle)}.`
        :"No reachable, fully caught-up follower is available to campaign.";
      rows.push({title:"Force a leader election",blocked:!ready,
        reason:ready?"Asks a caught-up follower to campaign. Raft still chooses the winner; writes pause briefly."
          :why,
        action:`<button class="ghost sm" ${ready?"":"disabled"} onclick="openElectionDialog()">Elect</button>`});
    }

    const verdict=ops&&!ops.unavailable&&ops.verdict;
    if(verdict){
      const candidateHere=verdict.candidate_node_id&&local&&verdict.candidate_node_id===local.node_id;
      const ready=Boolean(verdict.safe_to_restart_one&&candidateHere);
      const blockers=(verdict.blockers||[]);
      const why=!verdict.safe_to_restart_one
        ? (blockers.length?`${esc(clusterOperationReason(blockers[0].code))}${blockers[0].node_id?` on ${esc(blockers[0].node_id)}`:""}${blockers.length>1?`, and ${blockers.length-1} more`:""}.`
           :"At least one direct safety condition is missing.")
        : `The safe candidate is ${esc(verdict.candidate_node_id)} — open that node directly to prepare it.`;
      rows.push({title:"Prepare this voter for restart",blocked:!ready,
        reason:ready?"Blocks new admissions here and drains owned sessions, leaving the majority intact."
          :why,
        action:`<button class="ghost sm" ${ready?"":"disabled"} ${ready?`onclick='prepareLocalRestart(this,${arg(local)})'`:""}>Prepare</button>`});
    }

    // Removal is the one action whose commonest refusal — offline work owned by
    // the node — is not in any payload this page reads. The row says what it can
    // prove and routes to the card that carries the per-node control.
    const removable=nodes.filter(n=>n.node_id!==cluster.local_node_id);
    if(removable.length){
      // A learner leaves by a different path and is not held to the voter
      // arithmetic, so the quorum bar only applies while a voter is removable.
      const voterRemovable=removable.some(n=>n.is_voter);
      const quorumHolds=voters.length>=3||!voterRemovable;
      const ready=quorumHolds&&!lifecycle&&!recovery.required;
      const leader=removable.find(n=>n.is_leader);
      rows.push({title:"Remove a node permanently",blocked:!ready,
        reason:ready
          ? `Use Remove permanently on that node's card. At least three voters must remain a voter removal, `+
            `${leader?`${name(leader)} is the current leader and can only leave from its own screen, `:""}`+
            `and a node still holding offline work is refused until that work is resolved.`
          : recovery.required
            ? locked.reason
            : !quorumHolds
              ? `A voter removal needs at least three voters and this cluster has ${voters.length}. Add a node first.`
              : `A membership change is in flight on ${name(lifecycle)}.`,
        action:""});
    }

    rows.push({title:"Leave this cluster",destructive:true,
      reason:"This node resolves its owned work, commits its own removal, drains and shuts down. Rejoining needs a fresh data directory and a new token.",
      action:`<button class="btn-danger sm" onclick="openClusterDanger()">Leave</button>`});
    return rows;
  }

  // ---- words -----------------------------------------------------------------
  function clusterOperationAge(ms){
    if(ms===null||ms===undefined) return "unknown age";
    if(ms<1000) return "now";
    const seconds=Math.floor(ms/1000);
    if(seconds<60) return `${seconds}s ago`;
    const minutes=Math.floor(seconds/60);
    return minutes<60?`${minutes}m ago`:`${Math.floor(minutes/60)}h ago`;
  }
  function clusterOperationReason(value){
    return String(value||"unknown").replaceAll("_"," ");
  }

  // ---- the replicated database ---------------------------------------------
  // The store is the other half of this screen, and until now it had no section
  // of its own: watch state was one run-on sentence ("Replicated in sync ·
  // applied term 7, index 118442 · …") wedged between the node cards, and every
  // other Raft reading was buried in per-node evidence or in /metrics. Nothing
  // here is a new measurement. `cluster.replication` is the same projection the
  // sentence already used, and the term / commit watermark / snapshot / WAL /
  // protocol readings come from this node's own row of the direct status the
  // node cards already render. Laid out as a ledger, they answer the question
  // the sentence could not: is a write that lands here durable, and how far
  // behind is this machine.
  // `sys` is not in the Cluster tab's manifest, so a cold load straight into
  // this tab has no replication projection to read. `membership_unavailable` is
  // the server saying this process is not running a replicated store, which is
  // the same fact the projection would have carried, so the panel states it
  // rather than reporting its own missing request as a cluster problem.
  function clusterSqliteReplication(){
    return {backend:"sqlite",health:"healthy",clustered:false,
      explanation:"Watch state is stored on this server only."};
  }
  function clusterDatabaseStatus(cluster,ops){
    const row=clusterDirectOperationRow(ops,cluster&&cluster.local_node_id);
    return row&&row.observation==="answered"&&row.status?row.status:null;
  }
  // Capacity and redundancy are different quantities and this panel must never
  // let one stand in for the other, so the arithmetic is said once, here.
  function clusterCapacityText(capacity){
    if(!capacity) return {headline:"Capacity details unavailable",
      detail:"Read capacity and voting redundancy could not be read."};
    return {
      headline:`${capacity.ready_read_workers} ready read worker${capacity.ready_read_workers===1?"":"s"} · `+
        `${capacity.non_voting_replicas} non-voting replicated cop${capacity.non_voting_replicas===1?"y":"ies"}`,
      detail:`${capacity.voting_nodes} voters · quorum ${capacity.voting_quorum} · tolerates `+
        `${capacity.voting_failure_tolerance} voter failure${capacity.voting_failure_tolerance===1?"":"s"}. `+
        "Read capacity and replicated copies do not increase voting redundancy."};
  }
  // SQLite is deliberately not "healthy" and not "synced": with no peer there is
  // nowhere for a pause to carry over to, and a green tick would be a claim
  // about redundancy this install does not have.
  function clusterDatabaseHealth(replication){
    if(!replication) return {tone:"warn",label:"Status unavailable"};
    if(replication.backend==="sqlite") return {tone:"calm",label:"Single node"};
    if(replication.health==="degraded") return {tone:"warn",label:"Degraded"};
    return {tone:"good",label:replication.clustered?"In sync":"One node"};
  }
  function clusterHealthPill(env,tone,label){
    const {esc}=env;
    const colour=tone==="good"?"color:var(--good);border-color:var(--good)"
      :tone==="warn"?"color:var(--warn);border-color:var(--warn)":"";
    return `<span class="pill" style="${colour}">${esc(label)}</span>`;
  }
  // What survives folding. Collapsing the section hides the detail, never the
  // verdict: the health pill stays in the header and this line keeps the six
  // readings somebody would check before deciding whether to expand it again.
  function clusterDatabaseSummary(env,cluster,replication,ops){
    const {esc,fmtAgo}=env;
    const status=clusterDatabaseStatus(cluster,ops), raft=(status&&status.raft)||{};
    const leader=(cluster.nodes||[]).find(n=>n.is_leader);
    const known=value=>value!==null&&value!==undefined;
    const parts=[`leader <b>${leader?esc(leader.hostname||leader.node_id):"unknown"}</b>`];
    if(known(raft.current_term)) parts.push(`term <b>${esc(String(raft.current_term))}</b>`);
    if(known(raft.commit_index)) parts.push(`commit <b>${esc(String(raft.commit_index))}</b>`);
    if(replication&&known(replication.last_applied_index))
      parts.push(`applied <b>${esc(String(replication.last_applied_index))}</b>`);
    if(known(raft.apply_lag_entries)) parts.push(`lag <b>${esc(String(raft.apply_lag_entries))}</b>`);
    if(replication&&replication.last_converged_at)
      parts.push(`converged <b>${esc(fmtAgo(replication.last_converged_at))}</b>`);
    return parts.join(" · ");
  }
  // The WAL says one of three things: it was never observed, it is proven
  // unavailable, or it answered — and an answer that is `open` with a live error
  // or unflushed entries is a fault, which is exactly the predicate the node
  // badge already uses.
  // Every age in the direct status is computed on the server and frozen into
  // the response, while this panel keeps rendering from that one fetch for as
  // long as the tab is open. Without the time since the aggregate was taken, the
  // freshness row says "now" indefinitely.
  function clusterSampleAge(ops,millis){
    if(millis===null||millis===undefined) return null;
    const taken=ops&&ops.observed_at_unix_ms;
    return taken?millis+Math.max(0,Date.now()-taken):millis;
  }
  function clusterDatabaseWal(env,status){
    const {esc}=env;
    if(!status) return "unknown";
    const wal=status.wal&&status.wal.snapshot;
    if(!wal) return `<span style="color:var(--bad)">unavailable${
      status.wal&&status.wal.reason?` · ${esc(clusterOperationReason(status.wal.reason))}`:""}</span>`;
    const healthy=wal.state==="open"&&wal.lock_owned&&!wal.last_error&&wal.last_log_index===wal.last_durable_index;
    const durable=wal.last_durable_index===null||wal.last_durable_index===undefined
      ? "unknown":esc(String(wal.last_durable_index));
    const fault=wal.last_error?` · ${esc(wal.last_error.message||"error")}`
      :!wal.lock_owned?" · lock not held"
      :wal.last_log_index!==wal.last_durable_index?` · ${esc(String(wal.last_log_index))} logged, ${durable} durable`:"";
    return `<span style="color:var(--${healthy?"good":"bad"})">${esc(clusterOperationReason(wal.state))}</span> · durable ${durable}${fault}`;
  }
  function clusterDatabaseRows(env,cluster,replication,ops){
    const {esc,fmtAgo}=env;
    if(replication&&replication.backend==="sqlite")
      return [["Backend","SQLite · single node",false],
        ["Peers","none — watch state is durable here and has nowhere to fall behind",false]];
    const status=clusterDatabaseStatus(cluster,ops);
    const raft=(status&&status.raft)||{}, snap=(status&&status.snapshot)||{};
    const wal=status&&status.wal&&status.wal.snapshot;
    const leader=(cluster.nodes||[]).find(n=>n.is_leader);
    const num=value=>(value===null||value===undefined)?"unknown":esc(String(value));
    const sampled=raft.applied_index!==null&&raft.applied_index!==undefined;
    const applied=sampled?esc(String(raft.applied_index))
      :(replication&&replication.last_applied_index!==null&&replication.last_applied_index!==undefined
        ? esc(String(replication.last_applied_index)):"unknown");
    const behindKnown=replication&&(replication.behind_by!==null&&replication.behind_by!==undefined
      ? true:replication.health!=="degraded");
    const behind=behindKnown?Number(replication.behind_by||0):null;
    const lag=raft.apply_lag_entries===null||raft.apply_lag_entries===undefined
      ? "unknown":`${esc(String(raft.apply_lag_entries))} entr${raft.apply_lag_entries===1?"y":"ies"}`;
    return [
      ["Backend",replication?(replication.backend==="sqlite"?"SQLite · single node":"hiqlite · Raft"):"unknown",false],
      ["Leader",leader?`${esc(leader.hostname||leader.node_id)}${leader.node_id===cluster.local_node_id?' <span class="muted">this node</span>':""}`:"none elected",false],
      ["Term",num(raft.current_term),true],
      ["Quorum commit",num(raft.commit_index),true],
      ["Applied here",`${applied}${!sampled&&replication&&replication.last_applied_term!==null&&replication.last_applied_term!==undefined?` <span class="muted">term ${esc(String(replication.last_applied_term))}</span>`:""}`,true],
      ["Apply lag",lag,true],
      ["Behind by",behind===null?"unknown":`${behind} change${behind===1?"":"s"}`,true],
      ["Last converged",replication&&replication.last_converged_at?esc(fmtAgo(replication.last_converged_at)):"not yet observed",true],
      ["Snapshots",status?(snap.available
        ? `build ${num(snap.build_ok_count)} ok / ${num(snap.build_error_count)} error · install ${num(snap.install_ok_count)} ok / ${num(snap.install_error_count)} error`
        : `<span style="color:var(--bad)">unavailable</span>`):"unknown",true],
      ["WAL",clusterDatabaseWal(env,status),true],
      ["Protocol",status?`${num(status.protocol_min)}–${num(status.protocol_max)}`:"unknown",true],
      ["Reading age",!status?"unknown":raft.sample_valid&&raft.watermark_valid
        ? `local ${esc(clusterOperationAge(clusterSampleAge(ops,(raft.sample_age_seconds||0)*1000)))} · `+
          `watermark ${esc(clusterOperationAge(clusterSampleAge(ops,raft.watermark_age_millis)))}`
        : `<span style="color:var(--bad)">stale or incomplete proof</span>`,false],
    ];
  }

  // ---- folding, and remembering it -----------------------------------------
  // Which sections are open is a per-browser convenience, so it is kept in this
  // browser and never in the cluster. Nodes are keyed by node id rather than by
  // position, so a node leaving the cluster drops out of the stored set instead
  // of collapsing whichever card inherits its row. Every read and write is
  // guarded: a browser with site data blocked throws on access, and the panel
  // still has to render with no stored value at all.
  // A function rather than a bare const so the key travels with the two helpers
  // that use it: tests extract these by declaration, and a free const between
  // functions is picked up only by accident.
  function clusterFoldKey(){ return "plurx.cluster.fold"; }
  function clusterFoldRead(storage){
    try{
      const raw=storage.getItem(clusterFoldKey());
      if(!raw) return null;
      const state=JSON.parse(raw);
      if(!state||typeof state!=="object"||Array.isArray(state)) return null;
      const kept={};
      if(typeof state.database==="boolean") kept.database=state.database;
      if(Array.isArray(state.nodes_closed)) kept.nodes_closed=state.nodes_closed.filter(id=>typeof id==="string");
      if(typeof state.tab==="string") kept.tab=state.tab;
      return kept;
    }catch(e){ return null; }
  }
  function clusterFoldWrite(storage,patch){
    try{
      storage.setItem(clusterFoldKey(),JSON.stringify(Object.assign({},clusterFoldRead(storage)||{},patch)));
    }catch(e){ /* storage blocked: the panel keeps working, it just forgets */ }
  }

  return Object.freeze({
    clusterQuorum,
    clusterStateView,
    membershipRefusalText,
    clusterDirectMaintenanceStatus,
    clusterDirectOperationRow,
    clusterMaintenanceEntryVerdict,
    clusterMaintenanceReady,
    clusterMaintenanceResumeReady,
    clusterRecoveryState,
    clusterRailRow,
    clusterRailBlocker,
    clusterOperationRows,
    clusterOperationAge,
    clusterOperationReason,
    clusterSqliteReplication,
    clusterDatabaseStatus,
    clusterCapacityText,
    clusterDatabaseHealth,
    clusterHealthPill,
    clusterDatabaseSummary,
    clusterSampleAge,
    clusterDatabaseWal,
    clusterDatabaseRows,
    clusterFoldKey,
    clusterFoldRead,
    clusterFoldWrite,
  });
});
