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
  // Templates that read module state or touch the document (the node table
  // and its drawers, the alerts, the panel shell) deliberately stay in the
  // shell's pages/cluster*.js: the boundary is "no DOM, no module state",
  // applied honestly rather than perfectly.

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
        "on the leader itself and use Leave cluster in its own row; it coordinates its own removal with the surviving "+
        "quorum and then shuts down.",
      self_removal_requires_leave:
        "That row is this server. Use Leave cluster in this node's own row so the daemon settles local "+
        "work, commits its own removal, drains active connections, and shuts down instead of leaving a "+
      "tombstoned process online.",
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
  function clusterTransportObservation(ops,raftId,clientElapsedMs=0){
    if(!ops||ops.unavailable||raftId===null||raftId===undefined) return null;
    // The aggregate's timestamp belongs to the daemon's wall clock. An admin
    // browser may be ahead or behind that clock, so it cannot safely measure
    // retained age. The shell passes only elapsed monotonic time since this
    // exact response was received; direct/model callers default to no added
    // age rather than inventing a cross-machine clock relationship.
    clientElapsedMs=Number(clientElapsedMs);
    const elapsed=Number.isFinite(clientElapsedMs)?Math.max(0,clientElapsedMs):0;
    const ageByElapsed=value=>value===null||value===undefined
      ?value:Number(value)+elapsed;
    const projectAgeAndDeadline=observation=>{
      const sourceAge=Number(observation.sample_age_ms||0);
      const serializedRemaining=observation.active_deadline_remaining_ms===null||
        observation.active_deadline_remaining_ms===undefined
        ?null:Number(observation.active_deadline_remaining_ms);
      const stalledPhases=["connecting","transferring","awaiting_acknowledgement",
        "installing","retrying"];
      const fallbackBoundary=serializedRemaining===null&&stalledPhases.includes(observation.phase);
      const remaining=fallbackBoundary?Math.max(0,30000-sourceAge):serializedRemaining;
      const deadlineStillActive=remaining!==null&&remaining>elapsed;
      const deadlineCrossed=remaining!==null&&remaining<=elapsed&&
        stalledPhases.includes(observation.phase);
      const stalledAge=fallbackBoundary
        ?Math.max(0,sourceAge+elapsed-30000):Math.max(0,elapsed-remaining);
      const projectedStalledRetention=deadlineCrossed&&stalledAge<=300000;
      if(sourceAge+elapsed>300000&&!deadlineStillActive&&!projectedStalledRetention)
        return null;
      if(!elapsed&&!deadlineCrossed) return observation;
      const projected=Object.assign({},observation,{
        sample_age_ms:deadlineCrossed?stalledAge:sourceAge+elapsed,
      });
      for(const field of ["attempt_age_ms","last_acknowledgement_age_ms",
        "last_local_receive_age_ms"])
        projected[field]=ageByElapsed(observation[field]);
      if(remaining!==null)
        projected.active_deadline_remaining_ms=Math.max(0,remaining-elapsed);
      if(deadlineCrossed){
        projected.phase="stalled";
        projected.last_error_category="snapshot_stalled";
      }
      return projected;
    };
    const candidates=[];
    (ops.nodes||[]).forEach(row=>{
      // The private cluster listener remains available while a learner's
      // public daemon listener is intentionally closed during catch-up. The
      // server exposes one sanitized transport projection on the row; never
      // fall back to the unsanitized public-status payload.
      const transport=row&&row.transport;
      (transport&&transport.observations||[]).forEach(observation=>{
        if(Number(observation.observing_node_id)!==Number(transport.observing_node_id)) return;
        if(observation.raft_group!=="sqlite") return;
        const receiverId=observation.direction==="inbound"
          ?observation.observing_node_id
          :observation.direction==="outbound"
            ?observation.peer_node_id:null;
        if(Number(receiverId)!==Number(raftId)) return;
        const projected=projectAgeAndDeadline(observation);
        if(projected) candidates.push(projected);
      });
    });
    const priority={stalled:8,installing:7,awaiting_acknowledgement:6,retrying:5,
      transferring:4,connecting:3,failed:2,complete:1};
    const age=observation=>Number(observation.sample_age_ms||0);
    const ageUncertainty=observation=>{
      if(observation.age_uncertainty_ms===null||
        observation.age_uncertainty_ms===undefined) return 1000;
      const value=Number(observation.age_uncertainty_ms);
      return Number.isFinite(value)&&value>=0?value:1000;
    };
    const attemptAge=observation=>{
      if(observation.attempt_age_ms===null||observation.attempt_age_ms===undefined)
        return null;
      const value=Number(observation.attempt_age_ms);
      return Number.isFinite(value)&&value>=0?value:null;
    };
    const fingerprint=observation=>typeof observation.snapshot_fingerprint==="string"&&
      /^[0-9a-f]{64}$/.test(observation.snapshot_fingerprint)
      ?observation.snapshot_fingerprint:null;
    const attemptIdentity=observation=>
      `attempt:${observation.observing_node_id}:${observation.boot_id||""}:`+
      `${observation.direction}:${observation.peer_node_id}:`+
      `${observation.attempt_id}:${observation.socket_epoch}`;
    // snapshot_id is a bounded display label. It can be the digest rendering
    // of a long ID or an ordinary short ID with the same spelling, so it is
    // never semantic identity. Older peers have no fingerprint and remain
    // attempt-local rather than gaining unsafe cross-observer correlation.
    const identity=observation=>fingerprint(observation)
      ?`snapshot:${fingerprint(observation)}`:attemptIdentity(observation);
    const textCompare=(left,right)=>{
      const stable=value=>value===undefined?"0:undefined":value===null?"1:null":
        `${typeof value}:${String(value)}`;
      left=stable(left);right=stable(right);
      return left<right?-1:left>right?1:0;
    };
    const acknowledgedComplete=observation=>observation.phase==="complete"&&
      observation.direction==="outbound"&&observation.acknowledged_offset!==null&&
      observation.acknowledged_offset!==undefined;
    const tieBreak=(left,right)=>{
      const acknowledged=Number(acknowledgedComplete(right))-Number(acknowledgedComplete(left));
      if(acknowledged) return acknowledged;
      for(const field of ["direction","observing_node_id","peer_node_id","raft_group",
        "boot_id","attempt_id","socket_epoch","snapshot_id","snapshot_fingerprint",
        "attempted_offset","acknowledged_offset","locally_received_bytes","total_bytes",
        "attempt_age_ms","age_uncertainty_ms","last_acknowledgement_age_ms","last_local_receive_age_ms",
        "active_deadline_remaining_ms","reconnect_count","retry_count",
        "last_error_category","operation_owns_work"]){
        const compared=textCompare(left[field],right[field]);
        if(compared) return compared;
      }
      return 0;
    };
    const withinIdentity=(left,right)=>
      (priority[right.phase]||0)-(priority[left.phase]||0)||
      age(left)-age(right)||tieBreak(left,right);
    const chronologyOrder=observations=>{
      // A remote age was sampled at an unknown instant between this process's
      // request start and receipt. Sort only non-overlapping local-monotonic
      // intervals. Each rank is the non-dominated frontier: an observation is
      // excluded whenever another interval is provably newer. Do not merge
      // connected overlap components; a broad uncertain interval can overlap
      // two intervals that are themselves provably ordered.
      const useAttemptAge=observations.length>0&&
        observations.every(observation=>attemptAge(observation)!==null);
      const range=observation=>{
        const upper=useAttemptAge?attemptAge(observation):age(observation);
        return {lower:Math.max(0,upper-ageUncertainty(observation)),upper};
      };
      let remaining=observations.map(observation=>({observation,...range(observation)}));
      const ranks=new Map();
      let rank=0;
      while(remaining.length){
        const frontier=remaining.filter(entry=>!remaining.some(other=>
          other!==entry&&other.upper<entry.lower));
        frontier.forEach(entry=>ranks.set(entry.observation,rank));
        const frontierSet=new Set(frontier);
        remaining=remaining.filter(entry=>!frontierSet.has(entry));
        rank+=1;
      }
      return {
        rank:observation=>ranks.get(observation)||0,
        compare:(left,right)=>(ranks.get(left)||0)-(ranks.get(right)||0)||
          (priority[right.phase]||0)-(priority[left.phase]||0)||
          age(left)-age(right)||tieBreak(left,right),
      };
    };
    const groups=new Map();
    candidates.forEach(observation=>{
      const key=identity(observation);
      if(!groups.has(key)) groups.set(key,{
        fingerprint:fingerprint(observation),observations:[],
      });
      groups.get(key).observations.push(observation);
    });
    const representatives=[];
    groups.forEach(group=>{
      // Reduce concrete attempts and establish successor chronology before
      // considering either sender or receiver completion authority. A former
      // leader may publish a real acknowledgement after the receiver has
      // restarted into a newer attempt for the same snapshot.
      const attempts=new Map();
      group.observations.forEach(observation=>{
        const key=attemptIdentity(observation);
        if(!attempts.has(key)) attempts.set(key,[]);
        attempts.get(key).push(observation);
      });
      const attemptRepresentatives=[];
      attempts.forEach(observations=>{
        observations.sort(withinIdentity);
        attemptRepresentatives.push(observations[0]);
      });
      const chronology=chronologyOrder(attemptRepresentatives);
      attemptRepresentatives.sort(chronology.compare);
      const newestAttempts=new Set(attemptRepresentatives
        .filter(observation=>chronology.rank(observation)===0)
        .map(attemptIdentity));
      const sampleRange=observation=>({
        lower:Math.max(0,age(observation)-ageUncertainty(observation)),
        upper:age(observation),
      });
      const freshest=Math.min(...group.observations.map(observation=>sampleRange(observation).lower));
      const completionCohort=group.observations.filter(
        observation=>newestAttempts.has(attemptIdentity(observation))&&
          sampleRange(observation).upper<=freshest+5000);
      // A flushed sender acknowledgement is causally later than every
      // contemporaneous receiver-side phase for the same exact snapshot: the
      // receiver had to finish the install and return that response before the
      // sender could publish Complete. Apply that authority only to a validated
      // fingerprint and this identity's five-second freshness cohort. An old
      // leader's retained acknowledgement must not hide a newer attempt after a
      // receiver restart or leadership change; legacy and malformed identities
      // remain attempt-local.
      const nonComplete=completionCohort.filter(observation=>observation.phase!=="complete");
      const completionCanSupersede=observation=>nonComplete.every(other=>
        sampleRange(observation).upper<=sampleRange(other).lower);
      const acknowledged=group.fingerprint
        ?completionCohort.filter(observation=>
          acknowledgedComplete(observation)&&completionCanSupersede(observation)):[];
      if(acknowledged.length){
        acknowledged.sort((left,right)=>age(left)-age(right)||tieBreak(left,right));
        representatives.push(acknowledged[0]);
        return;
      }
      // A receiver that durably completed this exact fingerprint disproves
      // every unacknowledged sender-side predecessor: the sender may still say
      // installing, waiting, retrying, stalled, or failed because the reply did
      // not reach it. An acknowledged sender Complete is causally stronger and
      // already returned above. Apply this exception once at the group boundary;
      // it does not cover inbound failures or unrelated fingerprints.
      const receiverComplete=completionCohort.some(observation=>observation.phase==="complete"&&
        observation.direction==="inbound"&&completionCanSupersede(observation));
      if(group.fingerprint&&receiverComplete){
        const completed=completionCohort.filter(observation=>
          observation.phase==="complete"&&observation.direction==="inbound"&&
          completionCanSupersede(observation));
        completed.sort((left,right)=>age(left)-age(right)||tieBreak(left,right));
        representatives.push(completed[0]);
        return;
      }
      representatives.push(attemptRepresentatives[0]);
    });
    // Completion authority stays fingerprint-local above. Every surviving
    // identity representative shares one total chronology order here, so a
    // former leader's late terminal event cannot hide a successor snapshot.
    representatives.sort(chronologyOrder(representatives).compare);
    return representatives[0]||null;
  }
  function clusterTransportExplanation(ops,raftId,boundedReadReady,clientElapsedMs=0){
    const observation=clusterTransportObservation(ops,raftId,clientElapsedMs);
    if(!observation)
      return boundedReadReady
        ? {code:"idle",text:"transport idle; bounded-read proof ready",observation:null}
        : {code:"unavailable",text:"progress unavailable",observation:null};
    const labels={
      connecting:"connecting snapshot transport",
      transferring:observation.direction==="inbound"
        ?"receiving snapshot":"transferring snapshot",
      awaiting_acknowledgement:"waiting for snapshot acknowledgement",
      installing:"installing snapshot",
      retrying:"retrying snapshot",
      stalled:"snapshot stalled",
      failed:"snapshot failed",
      complete:observation.direction==="inbound"&&observation.acknowledged_offset==null
        ?(boundedReadReady?"snapshot recovered":"snapshot received; sender acknowledgement unconfirmed")
        :(boundedReadReady?"snapshot recovered":"waiting for startup watermark"),
    };
    const code=observation.phase==="complete"&&!boundedReadReady?"startup_watermark":observation.phase;
    return {code,text:labels[observation.phase]||"progress unavailable",observation};
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
  // Why a node has no direct sample, in words that name an action.
  //
  // These codes used to reach the operator as the raw identifier with its
  // underscores swapped for spaces, so the commonest one printed the single
  // word "unreachable" on a card that was, one row above, reporting a fresh
  // heartbeat and zero apply lag. The two readings come from different
  // sources -- the roster and the direct fan-out -- and when they disagree the
  // operator needs to know which one to act on. "Refused" and "unreachable"
  // in particular call for opposite next steps: a refusal is proof the node is
  // up and answering, so the fault is in the proof, not the machine.
  //
  // Unknown codes fall back to the mechanical transform rather than to a
  // sentence that would guess.
  const OBSERVATION_REASONS={
    unreachable:"no route to it from this node",
    timeout:"it did not answer before the deadline",
    refused:"it answered and refused the signed request from this node",
    http_error:"it answered with an unexpected error status",
    invalid_response:"its answer could not be read",
    identity_mismatch:"the answer came back under a different node identity",
    raft_identity_mismatch:"the answer came back under a different Raft id",
    stale_peer_sample:"its answer was older than one refresh window",
    unavailable:"no cache-backed direct observation is available yet",
    cache_unavailable:"the background direct-observation cache is not ready yet",
    cache_stale:"the background direct-observation cache has expired",
    peer_limit:"the roster is larger than one refresh may sample",
    not_observed:"this refresh has not sampled it",
  };
  function clusterObservationReason(value){
    const code=String(value||"not_observed");
    return OBSERVATION_REASONS[code]||clusterOperationReason(code);
  }

  // ---- rolling-upgrade facts -------------------------------------------------
  // Placement compares build, media protocol and eligibility independently
  // (docs/OPERATIONS.md "Rolling upgrades"); a node on another protocol is
  // absent from placement rather than refused, so the card has to say so.
  const PLACEMENT_REASONS={
    no_fresh_snapshot:"no fresh media advertisement from it",
    protocol_unreported:"its build predates media-protocol reporting",
    egress_unavailable:"its egress is unavailable",
    no_encoder:"it advertises no encoder",
  };
  function clusterPlacementReason(reason){
    const code=String(reason||"");
    const skew=/^protocol_mismatch(?::(\d+))?$/.exec(code);
    if(skew) return skew[1]?`it speaks media protocol ${skew[1]}`:"its media protocol differs";
    return PLACEMENT_REASONS[code]||clusterOperationReason(code);
  }
  function clusterWireFacts(ops,nodeId){
    const row=clusterDirectOperationRow(ops,nodeId);
    const status=row&&row.observation==="answered"?row.status:null;
    const placement=row&&row.placement;
    return {
      build:status?status.build:null,
      protocol:status&&Number.isInteger(status.media_protocol)?status.media_protocol:null,
      eligible:placement?placement.eligible_owner===true:null,
      reason:placement&&placement.reason?clusterPlacementReason(placement.reason):null,
    };
  }
  function clusterWireSkew(ops){
    const builds=new Set(), protocols=new Set();
    for(const row of (ops&&!ops.unavailable&&ops.nodes)||[]){
      if(row.observation!=="answered"||!row.status) continue;
      builds.add(String(row.status.build));
      protocols.add(Number.isInteger(row.status.media_protocol)?String(row.status.media_protocol):"unreported");
    }
    return {builds:[...builds].sort(),protocols:[...protocols].sort(),buildSkew:builds.size>1,protocolSkew:protocols.size>1};
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
    const healthy=clusterWalHealthy(wal);
    const durable=wal.last_durable_index===null||wal.last_durable_index===undefined
      ? "unknown":esc(String(wal.last_durable_index));
    const fault=wal.last_error?` · ${esc(wal.last_error.message||"error")}`
      :!wal.lock_owned?" · lock not held"
      :wal.last_log_index!==wal.last_durable_index?` · ${esc(String(wal.last_log_index))} logged, ${durable} durable`:"";
    return `<span style="color:var(--${healthy?"good":"bad"})">${esc(clusterOperationReason(wal.state))}</span> · durable ${durable}${fault}`;
  }
  // The one row whose value moves on its own. Every other reading in the ledger
  // is a number the server measured; this one is "how long ago", recomputed from
  // `Date.now()` at render time. It therefore has to be patchable without a
  // repaint — a freshness row that itself goes stale is worse than none — so it
  // is a function of its own and the cell it lands in carries an id.
  function clusterReadingAge(env,cluster,ops){
    const {esc}=env;
    const status=clusterDatabaseStatus(cluster,ops), raft=(status&&status.raft)||{};
    if(!status) return "unknown";
    if(!(raft.sample_valid&&raft.watermark_valid))
      return `<span style="color:var(--bad)">stale or incomplete proof</span>`;
    return `local ${esc(clusterOperationAge(clusterSampleAge(ops,(raft.sample_age_seconds||0)*1000)))} · `+
      `watermark ${esc(clusterOperationAge(clusterSampleAge(ops,raft.watermark_age_millis)))}`;
  }
  function clusterDatabaseRows(env,cluster,replication,ops){
    const {esc,fmtAgo,fmtBytes}=env;
    if(replication&&replication.backend==="sqlite")
      return [["Backend","SQLite · single node",false],
        ["Peers","none — watch state is durable here and has nowhere to fall behind",false]];
    const status=clusterDatabaseStatus(cluster,ops);
    const raft=(status&&status.raft)||{}, snap=(status&&status.snapshot)||{};
    const leader=(cluster.nodes||[]).find(n=>n.is_leader);
    const known=value=>value!==null&&value!==undefined;
    const num=value=>known(value)?esc(String(value)):"unknown";
    const sampled=known(raft.applied_index);
    const applied=sampled?esc(String(raft.applied_index))
      :(replication&&known(replication.last_applied_index)?esc(String(replication.last_applied_index)):"unknown");
    const behindKnown=replication&&(known(replication.behind_by)?true:replication.health!=="degraded");
    const behind=behindKnown?Number(replication.behind_by||0):null;
    const lag=known(raft.apply_lag_entries)?`lag ${esc(String(raft.apply_lag_entries))}`:"lag unknown";
    const capacity=cluster.capacity?clusterCapacityText(cluster.capacity):null;
    return [
      ["Backend",replication?"hiqlite · Raft":"unknown",false],
      ["Leader · term",`${leader?`${esc(leader.hostname||leader.node_id)}${leader.node_id===cluster.local_node_id?' <span class="muted">this node</span>':""}`:"none elected"} · ${num(raft.current_term)}`,true],
      ["Quorum commit",num(raft.commit_index),true],
      ["Applied on this node",`${applied}${!sampled&&replication&&known(replication.last_applied_term)?` <span class="muted">term ${esc(String(replication.last_applied_term))}</span>`:""} · ${lag}`,true],
      ["Behind by",behind===null?"unknown":`${behind} change${behind===1?"":"s"}`,true],
      // In sync needs no sentence; degraded does, and the server's says which
      // node is behind and what to do about it.
      ...(replication&&replication.health==="degraded"&&replication.explanation
        ?[["Watch state",esc(replication.explanation),false]]:[]),
      ["Last converged",replication&&replication.last_converged_at?esc(fmtAgo(replication.last_converged_at)):"not yet observed",true,"cldb-converged"],
      ["WAL",clusterDatabaseWal(env,status),true],
      ["Snapshots",status?(snap.available
        ? `build ${num(snap.build_ok_count)} ok / ${num(snap.build_error_count)} error · install ${num(snap.install_ok_count)} ok / ${num(snap.install_error_count)} error`
        : `<span style="color:var(--bad)">unavailable</span>`):"unknown",true],
      ["Database size",status&&snap.available&&known(snap.db_bytes)?esc(fmtBytes(snap.db_bytes)||"0 B"):"unknown",true],
      ["Read capacity",capacity?`<span title="${esc(capacity.detail)}">${esc(capacity.headline)}</span>`:"unknown",false],
      ["Protocol",status?`${num(status.protocol_min)}–${num(status.protocol_max)}`:"unknown",true],
      ["Reading age",clusterReadingAge(env,cluster,ops),false,"cldb-reading-age"],
    ];
  }

  // ---- the page's summary readings -------------------------------------------
  // The status strip, the node table and the readiness chips are the same
  // facts the node cards, the operations card and the banner used to state
  // three times over. Each is derived once here, from the roster and the direct
  // status, so the arithmetic is tested in one place and painted in one place.
  function clusterHost(cluster,nodeId){
    const n=((cluster&&cluster.nodes)||[]).find(row=>row.node_id===nodeId);
    return n?(n.hostname||n.node_id):nodeId;
  }
  function clusterAnswered(ops){
    if(!ops||ops.unavailable) return [];
    return (ops.nodes||[]).filter(row=>row.observation==="answered"&&row.status);
  }
  // The WAL is healthy only when it is open, this process holds its lock, it
  // carries no error and everything logged is durable. One predicate: the node
  // status, the WAL cell and the database ledger all colour by it.
  function clusterWalHealthy(wal){
    return Boolean(wal&&wal.state==="open"&&wal.lock_owned&&!wal.last_error&&wal.last_log_index===wal.last_durable_index);
  }
  // A server verdict names the condition it actually hit; this says it with the
  // machine's hostname rather than its node id. No trailing period, so a strip
  // cell and a sentence can both use it.
  function clusterBlockerText(env,blockers,cluster){
    const {esc}=env;
    const list=blockers||[], first=list[0];
    if(!first) return null;
    return `${esc(clusterOperationReason(first.code))}${first.node_id?` on ${esc(clusterHost(cluster,first.node_id))}`:""}${
      list.length>1?`, and ${list.length-1} more`:""}`;
  }
  // The strip's six cells, left to right. `tone` is good / warn / bad / calm or
  // empty; the first cell's tone is the page's.
  function clusterStripCells(env,cluster,ops){
    const {esc}=env;
    const plural=(k,one,many)=>`${k} ${k===1?one:many}`;
    const dash="—";
    if(!cluster||cluster.unavailable)
      return [
        {key:"cluster",label:"Cluster",value:"Status unavailable",sub:"the roster could not be read",tone:"warn"},
        {key:"leader",label:"Leader",value:dash,sub:"",tone:""},
        {key:"reachable",label:"Nodes reachable",value:dash,sub:"",tone:""},
        {key:"replication",label:"Replication",value:dash,sub:"",tone:""},
        {key:"build",label:"Build",value:dash,sub:"",tone:""},
        {key:"restart",label:"Rolling restart",value:dash,sub:"",tone:""},
      ];
    const nodes=cluster.nodes||[];
    const name=n=>esc(n.hostname||n.node_id);
    const voters=nodes.filter(n=>n.is_voter);
    const reachableVoters=voters.filter(n=>n.reachable).length;
    const cap=cluster.capacity;
    const q=cap?{voters:cap.voting_nodes,majority:cap.voting_quorum,tolerates:cap.voting_failure_tolerance}
      :Object.assign({voters:voters.length},clusterQuorum(voters.length));
    const recovery=clusterRecoveryState(cluster);
    const fenced=nodes.find(n=>n.maintenance);
    const answered=clusterAnswered(ops);

    const unobserved=clusterUnobservedVoters(cluster,ops);
    const verdict=ops&&!ops.unavailable&&ops.verdict;
    let health;
    if(recovery.required)
      health={value:"Recovery required",tone:"bad",
        sub:`${recovery.reachable_voters} reachable · ${recovery.required_voters} needed for quorum`};
    else if(fenced){
      const direct=clusterDirectMaintenanceStatus(ops,fenced.node_id);
      const sessions=direct&&direct.media?Number(direct.media.local_active_sessions||0):Number(fenced.active_media_sessions||0);
      health={value:"Maintenance",tone:"warn",
        sub:`${name(fenced)} fenced · ${sessions?`draining ${plural(sessions,"stream","streams")}`:"drained"}`};
    }else if(cluster.availability==="degraded_reconfiguration")
      health={value:"Reconfiguring",tone:"warn",sub:`${plural(q.voters,"voter","voters")} · quorum ${q.majority} · survives no failure`};
    else if(reachableVoters<voters.length)
      health={value:"Degraded",tone:"warn",
        sub:`${reachableVoters} of ${voters.length} voters reachable · tolerates ${Math.max(0,reachableVoters-q.majority)} more`};
    // The roster says every voter is reachable; the direct status, already
    // fetched, may know better. A collection that failed, a voter it could not
    // observe, a blocked rollout verdict or degraded replication is not
    // "Healthy". Still collecting (`ops === null`) degrades nothing.
    else if(cluster.availability==="high_availability"&&ops&&ops.unavailable)
      health={value:"Status unavailable",tone:"warn",sub:"direct status could not be collected"};
    else if(cluster.availability==="high_availability"&&(unobserved.length||(verdict&&!verdict.safe_to_restart_one)))
      health={value:"Degraded",tone:"warn",
        sub:(verdict&&!verdict.safe_to_restart_one&&clusterBlockerText(env,verdict.blockers,cluster))
          ||`${voters.length-unobserved.length} of ${voters.length} voters answered`};
    else if(cluster.availability==="high_availability"&&cluster.replication&&cluster.replication.health==="degraded")
      health={value:"Degraded",tone:"warn",sub:"replication degraded"};
    else if(cluster.availability==="high_availability")
      health={value:"Healthy",tone:"good",
        sub:`${plural(q.voters,"voter","voters")} · quorum ${q.majority} · tolerates ${plural(q.tolerates,"failure","failures")}`};
    else health={value:"One node",tone:"calm",sub:`${plural(q.voters,"voter","voters")} · quorum ${q.majority}`};

    const leader=nodes.find(n=>n.is_leader);
    const terms=[...new Set(answered.map(row=>row.status.raft&&row.status.raft.current_term)
      .filter(value=>value!==null&&value!==undefined))];

    const reach=nodes.filter(n=>n.reachable).length;
    const down=nodes.filter(n=>!n.reachable);
    const others=nodes.length-voters.length;

    const dbHealth=clusterDatabaseHealth(cluster.replication);
    const lags=answered.map(row=>row.status.raft&&row.status.raft.apply_lag_entries)
      .filter(value=>value!==null&&value!==undefined);

    // A build string breaks after its "+" before it breaks anywhere else, so
    // a narrow cell keeps the version and the commit hash whole.
    let build;
    if(!ops) build={value:dash,sub:"collecting",tone:""};
    else if(ops.unavailable) build={value:dash,sub:"status unavailable",tone:"warn"};
    else{
      const builds=[...new Set(answered.map(row=>String(row.status.build)))].sort();
      const skew=clusterWireSkew(ops);
      const counted=answered.length===nodes.length?`all ${plural(nodes.length,"node","nodes")} match`
        :`${answered.length} of ${nodes.length} answered`;
      build=!builds.length?{value:dash,sub:`0 of ${nodes.length} answered`,tone:"warn"}
        :builds.length===1?{value:esc(builds[0]).replace(/\+/g,"+<wbr>"),sub:counted,tone:answered.length===nodes.length?"":"warn"}
        :{value:`${builds.length} builds`,sub:`${answered.length} of ${nodes.length} answered`,tone:"warn"};
      if(skew.protocolSkew) build=Object.assign(build,{tone:"bad",sub:`protocol skew ${esc(skew.protocols.join(" / "))}`});
    }

    let restart;
    if(!ops) restart={value:"Collecting…",sub:"direct status not collected yet",tone:""};
    else if(ops.unavailable||!verdict) restart={value:"Unavailable",sub:"direct status could not be collected",tone:"warn"};
    else if(verdict.safe_to_restart_one)
      restart={value:"Safe",tone:"good",
        sub:verdict.candidate_node_id?`next: ${esc(clusterHost(cluster,verdict.candidate_node_id))}`:"no candidate named"};
    else restart={value:"Blocked",tone:"warn",
      sub:clusterBlockerText(env,verdict.blockers,cluster)||"a direct safety condition is missing"};

    return [
      Object.assign({key:"cluster",label:"Cluster"},health),
      {key:"leader",label:"Leader",value:leader?name(leader):dash,
        sub:terms.length===1?`term ${esc(String(terms[0]))}`:terms.length>1?"term disagreement":"term unknown",
        tone:terms.length>1||!leader?"warn":""},
      {key:"reachable",label:"Nodes reachable",value:`${reach} / ${nodes.length}`,tone:down.length?"warn":"",
        sub:down.length?(down.length===1?`${name(down[0])} unreachable`:`${down.length} unreachable`)
          :`${plural(voters.length,"voter","voters")} · ${plural(others,"learner","learners")}`},
      {key:"replication",label:"Replication",value:esc(dbHealth.label),tone:dbHealth.tone==="good"?"":dbHealth.tone,
        sub:lags.length?`max apply lag ${Math.max(...lags)}`:"apply lag unknown"},
      Object.assign({key:"build",label:"Build"},build),
      Object.assign({key:"restart",label:"Rolling restart"},restart),
    ];
  }
  // One row of the nodes table. A cell the direct status did not sample shows
  // a dash: a roster heartbeat is not a substitute for a direct peer
  // observation, so nothing here is filled in from it.
  function clusterNodeCells(env,n,ops){
    const {esc,fmtAgo,fmtBytes}=env;
    const dash="—";
    const row=clusterDirectOperationRow(ops,n.node_id);
    const s=row&&row.observation==="answered"?row.status:null;
    const raft=s&&s.raft, wal=s&&s.wal&&s.wal.snapshot;
    let status;
    if(n.maintenance) status={label:"Maintenance",tone:"warn"};
    else if(!n.reachable) status={label:"Unreachable",tone:"bad"};
    else if(!ops) status={label:"Collecting…",tone:""};
    else if(ops.unavailable) status={label:"Status unavailable",tone:"warn"};
    else if(!s) status={label:"Not observed",tone:"bad",
      why:clusterObservationReason(row&&(row.error_class||row.observation)||"not_observed")};
    else if(!s.process||!s.process.live||!s.serving||!s.serving.ready||!raft||!raft.sample_valid
      ||!raft.watermark_valid||!clusterWalHealthy(wal)) status={label:"Needs attention",tone:"warn"};
    else if(!n.is_voter&&!n.bounded_read_ready) status={label:"Catching up",tone:"warn"};
    else status={label:"Ready",tone:"good"};
    const roleWord=raft&&raft.is_leader===true?"leader":n.is_voter?"follower":"learner";
    const raftCell=raft
      ? (raft.sample_valid&&raft.watermark_valid
        ? {text:`${roleWord} · lag ${raft.apply_lag_entries===null||raft.apply_lag_entries===undefined?"unknown":esc(String(raft.apply_lag_entries))}`,tone:""}
        : {text:`${roleWord} · stale proof`,tone:"bad"})
      : n.reachable&&n.apply_lag_entries!==null&&n.apply_lag_entries!==undefined
        ? {text:`${n.is_leader?"leader":n.is_voter?"follower":"learner"} · lag ${esc(String(n.apply_lag_entries))}`,tone:""}
        : {text:dash,tone:""};
    const walCell=wal?{text:`${esc(clusterOperationReason(wal.state))} · ${esc(fmtBytes(wal.allocated_bytes)||"0 B")}`,
        tone:clusterWalHealthy(wal)?"":"bad"}
      : s?{text:"unavailable",tone:"bad"}:{text:dash,tone:""};
    const snap=s&&s.snapshot;
    return {
      role:n.is_voter?"voter":n.role==="voter"?"joining voter":"learner",
      status,
      heartbeat:{text:n.last_seen_at?esc(fmtAgo(Math.floor(n.last_seen_at/1000))):"never",tone:n.reachable?"":"bad"},
      raft:raftCell,
      wal:walCell,
      database:{text:snap&&snap.db_bytes!==null&&snap.db_bytes!==undefined?esc(fmtBytes(snap.db_bytes)||"0 B"):dash,tone:""},
      streams:{text:s&&s.media?esc(String(Number(s.media.local_active_sessions||0)))
        :n.reachable?esc(String(Number(n.active_media_sessions||0))):dash,tone:""},
      build:{text:s?esc(String(s.build)).replace(/\+/g,"+<wbr>"):dash,tone:""},
    };
  }
  // A readiness chip counts only the requirements the server reported a
  // status for; a requirement it said nothing about is neither met nor unmet.
  // `rows` are the requirement rows a card lists, each the server's row or null.
  function clusterReadinessChip(readiness,rows){
    if(!readiness) return {tone:"",label:"checking…"};
    if(readiness.unavailable) return {tone:"warn",label:"checks unavailable"};
    const reported=(rows||[]).filter(row=>row&&row.status);
    if(!reported.length) return {tone:"",label:"not reported"};
    const met=reported.filter(row=>row.status==="met").length;
    return {tone:met===reported.length?"ok":"warn",label:`${met} of ${reported.length} checks`};
  }
  // Voters the roster calls reachable that the direct status did not answer
  // for. Nothing while the status is collecting or failed: those are stated
  // on their own.
  function clusterUnobservedVoters(cluster,ops){
    if(!ops||ops.unavailable) return [];
    const answered=clusterAnswered(ops);
    return ((cluster&&cluster.nodes)||[]).filter(n=>n.is_voter&&n.reachable&&!n.maintenance
      &&!answered.some(row=>row.membership&&row.membership.node_id===n.node_id));
  }
  // What needs saying above the table, most severe first. Recovery carries the
  // maintenance target and the down voters inside it, so neither is repeated
  // beside it. `context.refusal` is the shell's module state.
  function clusterAlertKinds(cluster,ops,context={}){
    const kinds=[];
    if(cluster&&!cluster.unavailable){
      const nodes=cluster.nodes||[];
      const recovery=clusterRecoveryState(cluster);
      if(recovery.required) kinds.push({kind:"recovery",tone:"bad"});
      if(!recovery.required&&nodes.some(n=>n.maintenance)) kinds.push({kind:"maintenance",tone:"warn"});
      const local=clusterDirectMaintenanceStatus(ops,cluster.local_node_id);
      const media=local&&local.media;
      if(media&&media.new_admissions_blocked) kinds.push({kind:"restart",tone:media.drained?"good":"warn"});
      const down=!recovery.required&&nodes.some(n=>n.is_voter&&!n.reachable&&!n.maintenance);
      if(down) kinds.push({kind:"voter_down",tone:"warn"});
      // The direct verdict alone can say "do not restart": a voter the status
      // could not observe, a WAL fault, a stale proof. The voter-down block
      // already names the blocked restart, and maintenance and recovery
      // explain their own; a one- or two-voter cluster states its own shape.
      const verdict=ops&&!ops.unavailable&&ops.verdict;
      if(verdict&&!verdict.safe_to_restart_one&&!down&&!recovery.required&&!nodes.some(n=>n.maintenance)
        &&cluster.availability==="high_availability") kinds.push({kind:"restart_blocked",tone:"warn"});
      if(ops&&ops.unavailable) kinds.push({kind:"ops_unavailable",tone:"warn"});
      if(cluster.availability==="degraded_reconfiguration") kinds.push({kind:"reconfiguration",tone:"warn"});
    }
    if(context.refusal) kinds.push({kind:"refusal",tone:"bad"});
    if(!cluster||(cluster.unavailable&&cluster.code!=="membership_unavailable")) kinds.push({kind:"roster_failed",tone:"warn"});
    return kinds.filter(k=>k.tone==="bad").concat(kinds.filter(k=>k.tone!=="bad"));
  }
  // Which voter stopped answering, for how long, and what that now blocks.
  // Quorum arithmetic is the roster's; the blocked operation is named only
  // when the server's own verdict says it is blocked.
  function clusterVoterDownText(env,cluster,ops){
    const {esc,fmtAgo}=env;
    const nodes=cluster.nodes||[];
    const voters=nodes.filter(n=>n.is_voter);
    const down=voters.filter(n=>!n.reachable&&!n.maintenance);
    if(!down.length) return "";
    const q=clusterQuorum(voters.length);
    const up=voters.length-down.length;
    const spare=up-q.majority;
    const who=down.map(n=>`${esc(n.hostname||n.node_id)}${n.last_seen_at?` (last heartbeat ${esc(fmtAgo(Math.floor(n.last_seen_at/1000)))})`:""}`).join(", ");
    const verdict=ops&&!ops.unavailable&&ops.verdict;
    const blocked=verdict&&!verdict.safe_to_restart_one
      ? ` Rolling restart is blocked: ${clusterBlockerText(env,verdict.blockers,cluster)||"a direct safety condition is missing"}.`:"";
    return `<b>${down.length===1?"A voter is":`${down.length} voters are`} not answering: ${who}.</b> Quorum holds at ${up} of ${voters.length}; ${
      spare<=0?"one more voter failure stops writes":`${spare} more voter failure${spare===1?" is":"s are"} tolerated`}.${blocked}`;
  }
  // The header's freshness: the oldest direct sample, aged by the time since
  // the aggregate was taken so it moves without a fetch.
  function clusterSampledText(ops){
    if(!ops) return "collecting status";
    if(ops.unavailable) return "status unavailable";
    const ages=(ops.nodes||[]).map(row=>row.sample_age_ms).filter(age=>age!==null&&age!==undefined);
    if(!ages.length) return "not sampled";
    return `sampled ${clusterOperationAge(clusterSampleAge(ops,Math.max(...ages)))}`;
  }

  // ---- actions, with their preconditions already evaluated ---------------------
  // Every action this page can take states its precondition before the click.
  // The server still refuses what it refuses — nothing here widens what is
  // allowed — but a blocked button carries the reason beside it. Membership
  // actions live on the node they act on; the cluster-wide ones are the
  // Maintenance card's rows.
  function clusterLifecycle(cluster){
    const nodes=cluster.nodes||[];
    const pending=nodes.find(n=>n.removal_pending);
    const fenced=nodes.find(n=>n.maintenance);
    const recovery=clusterRecoveryState(cluster);
    // Recovery locks every membership change: issuing a token and removing a
    // member both need a leader and a committed quorum read.
    return {pending,fenced,lifecycle:fenced||pending,recovery,
      locked:recovery.required?"The cluster cannot prove both an elected leader and a reachable voter majority, so a membership change cannot commit.":null};
  }
  // Adding a node mints a credential; the control that shows it once is this
  // action's own expansion.
  function clusterAddNodeAction(env,cluster){
    const {esc}=env;
    const {lifecycle,locked}=clusterLifecycle(cluster);
    if(locked) return {blocked:true,reason:locked};
    if(lifecycle) return {blocked:true,
      reason:`A membership change is already in flight on ${esc(lifecycle.hostname||lifecycle.node_id)}. Finish it before admitting another node.`};
    return {blocked:false,reason:""};
  }
  // The actions in one node's drawer. This node: maintenance, restart
  // preparation and leave. Another member: remove, and promote for a learner.
  // Maintenance is a local action by design — the fence has to be
  // acknowledged by the process being fenced.
  function clusterNodeActions(env,cluster,ops,n){
    const {esc,fmtBytes}=env;
    const nodes=cluster.nodes||[];
    const name=x=>esc(x.hostname||x.node_id);
    const arg=x=>esc(JSON.stringify(x.node_id));
    const voters=nodes.filter(x=>x.is_voter);
    const {pending,fenced,lifecycle,locked}=clusterLifecycle(cluster);
    const button=(cls,label,ready,onclick)=>`<button class="${cls?`${cls} `:""}sm" ${ready?"":"disabled"} ${ready?`onclick='${onclick}'`:""}>${label}</button>`;
    const actions=[];
    if(n.node_id===cluster.local_node_id){
      if(n.maintenance){
        const resume=clusterMaintenanceResumeReady(n,ops);
        actions.push({id:"resume",blocked:!resume,
          reason:resume?"":"Still draining: waiting for direct acknowledgement, leader handoff, catch-up, or drain proof.",
          html:button("","Resume service",resume,`setNodeMaintenance(${arg(n)},false)`)});
      }else{
        const verdict=clusterMaintenanceEntryVerdict(ops,n.node_id);
        const safe=Boolean(verdict&&verdict.safe_to_enter);
        const ready=safe&&n.reachable&&!n.removal_pending&&!pending;
        const why=!n.reachable?"This node's own heartbeat is not fresh."
          :pending?`A removal is pending on ${name(pending)}.`
          :verdict?(blockerText=>blockerText?`${blockerText}.`:"The direct maintenance preflight has not approved this node.")(clusterBlockerText(env,verdict.blockers,cluster))
          // With a verdict, the server names a conflicting fence itself
          // (maintenance_already_active); without one, the roster still can.
          :fenced?`Maintenance is already active on ${name(fenced)}; resume it before fencing another node.`
          :ops&&ops.unavailable?"The direct status could not be collected, and a heartbeat is not a substitute for it."
          :"The direct maintenance preflight has not answered yet.";
        actions.push({id:"maintenance",blocked:!ready,reason:ready?"":why,
          html:button("ghost","Enter maintenance",ready,`setNodeMaintenance(${arg(n)},true)`)});
      }
      const verdict=ops&&!ops.unavailable&&ops.verdict;
      const candidate=verdict&&verdict.candidate_node_id;
      const here=Boolean(verdict&&verdict.safe_to_restart_one&&candidate===n.node_id);
      const why=!ops?"The direct rollout verdict has not answered yet."
        :!verdict?"The direct status could not be collected, and a heartbeat is not a substitute for it."
        :!verdict.safe_to_restart_one?`${clusterBlockerText(env,verdict.blockers,cluster)||"At least one direct safety condition is missing"}.`
        :candidate?`The safe candidate is ${esc(clusterHost(cluster,candidate))} — open that node directly to prepare it.`
        :"The direct rollout verdict names no candidate.";
      actions.push({id:"restart",blocked:!here,reason:here?"":why,
        html:button("ghost","Prepare for restart",here,`prepareLocalRestart(this,${arg(n)})`)});
      // A graceful leave is a membership change like any other: it needs a
      // current leader, a voter self-leave keeps three voters, and a
      // maintenance fence is a cluster-wide conflict. A removal pending on some
      // OTHER node is deliberately not here — the server's fence is per-node.
      const leaveQuorum=!n.is_voter||voters.length>=3;
      const leaveWhy=locked?locked
        :fenced?`Maintenance is active on ${name(fenced)}, and membership changes are refused while it is. Resume that node first.`
        :!leaveQuorum?`A voter can only leave while at least three voters remain, and this cluster has ${voters.length}. Add a node first.`:"";
      actions.push({id:"leave",destructive:true,expand:"leave",blocked:Boolean(leaveWhy),reason:leaveWhy,html:""});
    }
    // A learner is promoted from any node's screen, its own included.
    if(!n.is_voter&&n.role==="learner"){
      // Promotion is refused until the learner has published a fresh zero-lag
      // apply proof and its filesystem has proved the voter reserve.
      const ready=n.bounded_read_ready&&n.voter_storage_ready&&!lifecycle;
      const why=lifecycle?`A membership change is in flight on ${name(lifecycle)}.`
        :!n.bounded_read_ready?"It has not published a fresh zero-lag apply proof yet — leave it running until its read-worker badge says ready."
        :`Its data root has not proved the durable free-space reserve a voter must hold${
            n.storage_headroom_bytes!=null?` (${esc(fmtBytes(n.storage_headroom_bytes)||"0 B")} free)`:""}. Free space or move the data root.`;
      actions.push({id:"promote",blocked:!ready,reason:ready?"":why,
        html:button("ghost","Promote to voter",ready,`promoteNode(${arg(n)})`)});
    }
    // This node leaves; it is not removed from its own screen.
    if(n.node_id===cluster.local_node_id) return actions;
    // A learner leaves by a different path and is not held to the voter
    // arithmetic. A removal pending elsewhere is not a reason here: the
    // server's removal fence is per node and it refuses what it refuses.
    const quorumHolds=voters.length>=3||!n.is_voter;
    const removeWhy=locked?locked
      :fenced?"Finish maintenance before changing membership."
      :!quorumHolds?`A voter removal needs at least three voters and this cluster has ${voters.length}. Add a node first.`
      :n.is_leader?`${name(n)} is the current leader and can only leave from its own screen.`:"";
    actions.push({id:"remove",destructive:true,blocked:Boolean(removeWhy),reason:removeWhy,
      html:button("danger",n.removal_pending?"Retry removal":"Remove permanently…",!removeWhy,`removeNode(${arg(n)})`)});
    return actions;
  }
  // The Maintenance card: cluster-wide actions only.
  function clusterMaintenanceRows(env,cluster,ops){
    const {esc}=env;
    const nodes=cluster.nodes||[];
    const voters=nodes.filter(n=>n.is_voter);
    const {lifecycle,recovery}=clusterLifecycle(cluster);
    const blocked=why=>`<span style="color:var(--warn)">Blocked</span> — ${why}`;
    const rows=[];
    const verdict=ops&&!ops.unavailable&&ops.verdict;
    // Every finding the server reported, blockers and warnings alike: a
    // warning on a safe verdict is still something to read before restarting.
    const findingList=findings=>findings.length?`<ul class="clopsblockers">${findings.map(f=>`<li><b>${esc(clusterOperationReason(f.code))}</b>${
      f.node_id?` on ${esc(clusterHost(cluster,f.node_id))}`:""}: ${esc(f.message)}</li>`).join("")}</ul>`:"";
    const prepare=ready=>`<button class="ghost sm" ${ready?`onclick='prepareLocalRestart(this,${esc(JSON.stringify(cluster.local_node_id))})'`:"disabled"}>Prepare</button>`;
    if(!ops) rows.push({id:"restart",title:"Rolling restart",text:"Collecting the direct rollout verdict…",action:prepare(false)});
    else if(!verdict) rows.push({id:"restart",title:"Rolling restart",
      text:blocked("the direct status could not be collected, and a heartbeat is not a substitute for it."),action:prepare(false)});
    else if(verdict.safe_to_restart_one){
      const named=Boolean(verdict.candidate_node_id);
      const candidate=esc(clusterHost(cluster,verdict.candidate_node_id));
      const here=named&&verdict.candidate_node_id===cluster.local_node_id;
      rows.push({id:"restart",title:"Rolling restart",
        text:`The observed voters preserve majority after one restart.${named?` Prepare only <b>${candidate}</b> and re-check before touching another node.`:""}`+
          findingList(verdict.warnings||[]),
        action:here||!named?prepare(here):`<span class="clwhy">Open ${candidate} to prepare it</span>`});
    }else{
      // The verdict's own sentences, then every finding it reported.
      rows.push({id:"restart",title:"Rolling restart",
        text:blocked("Do not restart another voter.")+
          " At least one direct safety condition is missing or unhealthy. Keep the available voters running until every blocker below clears."+
          findingList([...(verdict.blockers||[]),...(verdict.warnings||[])]),
        action:prepare(false)});
    }
    if(voters.length>1){
      const candidate=voters.some(n=>!n.is_leader&&n.reachable&&n.apply_lag_entries===0&&!n.removal_pending);
      const ready=candidate&&!lifecycle&&recovery.quorum_available;
      const why=!recovery.quorum_available?"A voter majority cannot be proved, and an election cannot bypass quorum."
        :lifecycle?`A membership change is in flight on ${esc(lifecycle.hostname||lifecycle.node_id)}.`
        :"No reachable, fully caught-up follower is available to campaign.";
      rows.push({id:"election",title:"Leader election",
        text:ready?"Asks a caught-up follower to campaign. Raft still chooses the winner; writes pause briefly.":blocked(why),
        action:`<button class="ghost sm" ${ready?"":"disabled"} onclick="openElectionDialog()">Force election…</button>`});
    }
    rows.push({id:"diagnostics",title:"Diagnostics",text:"Support bundle and roster snapshot for a report.",
      action:`<button class="ghost sm" onclick="downloadClusterSupportBundle(this)">Support bundle</button><button class="ghost sm" onclick="downloadClusterDiagnostics()">Roster snapshot</button>`});
    return rows;
  }

  // ---- what a repaint is actually for --------------------------------------
  // The panel refetches /cluster/status on a gate of its own, and a fetch is not
  // a repaint: `renderSettings()` rewrites the whole tab, so painting on every
  // fetch would take an operator's scroll position, their open drill-downs, and
  // their text selection away every few seconds for nothing.
  //
  // So the tick compares this projection of the payload and paints only when it
  // moves. The projection is the WHOLE response minus the fields that change by
  // themselves: the aggregate's and each status's `observed_at_unix_ms`, the
  // per-node `sample_age_ms`, and the Raft sample and watermark ages. Those are
  // what the "Reading age" row reports, and it is patched in place instead.
  // A transport observation's `sample_age_ms` stays in the projection: its
  // observer age is rendered in the recovery ledger and has no separate patch.
  // `last_seen_at` on the embedded membership record goes too: the roster owns
  // that reading and the panel never renders it from here, so a heartbeat is not
  // a reason to rewrite the screen.
  //
  // Everything else stays in, deliberately. The commit watermark advancing IS a
  // change an operator watches this ledger for, and a projection covering only
  // the rail's verdicts would leave the database section aging off one fetch —
  // the defect this workstream exists to fix.
  // `last_sync_unix_ms` is the odd one out: it IS rendered, as the node card's
  // "Last WAL sync … ago". It is excluded anyway, because it advances on every
  // fsync — so on a cluster doing any work at all it would repaint the tab every
  // interval on its own. It costs nothing to leave out: a WAL that is syncing is
  // a cluster that is committing, and the commit watermark beside it is in the
  // projection, so the screen still moves. On a quiet cluster nothing is syncing
  // and there is nothing to miss.
  function clusterOpsSelfTicking(key){
    return key==="observed_at_unix_ms"||key==="sample_age_ms"||key==="sample_age_seconds"
      ||key==="watermark_age_millis"||key==="last_seen_at"||key==="last_sync_unix_ms";
  }
  // The aggregate's top-level `membership` is the whole committed roster again.
  // The panel reads exactly one field from it — `local_node_id` — and renders
  // every other reading in it from /cluster/nodes instead, so its apply lag and
  // session counts would trigger repaints that change nothing on screen. It is
  // dropped at the root only: the per-node `membership` record inside `nodes[]`
  // carries the identity those rows are matched and labelled by, and a node
  // joining, leaving or being renamed IS news.
  function clusterOpsProjection(ops){
    const walk=value=>{
      if(Array.isArray(value)) return value.map(walk);
      if(!value||typeof value!=="object") return value;
      const transportObservation=(value.direction==="inbound"||value.direction==="outbound")
        &&typeof value.raft_group==="string"&&Object.prototype.hasOwnProperty.call(value,"observing_node_id");
      const out={};
      for(const key of Object.keys(value).sort()){
        if(clusterOpsSelfTicking(key)&&!(transportObservation&&key==="sample_age_ms")) continue;
        out[key]=walk(value[key]);
      }
      return out;
    };
    if(!ops||typeof ops!=="object"||Array.isArray(ops))
      return JSON.stringify(walk(ops===undefined?null:ops));
    const root={};
    for(const key of Object.keys(ops).sort()){
      if(clusterOpsSelfTicking(key)) continue;
      root[key]=key==="membership"
        ? (ops.membership&&ops.membership.local_node_id)||null
        : walk(ops[key]);
    }
    return JSON.stringify(root);
  }

  // The roster's counterpart of the projection above, for the tick that
  // re-reads it on the same gate. Its self-ticking fields: each node's
  // heartbeat `last_seen_at` and the store's `last_converged_at`, both shown
  // as ages and patched in place (patchClusterReadingAge), and the store's
  // `last_applied_index`, which advances on every commit and is shown only
  // when the direct sample has no applied index of its own — the direct
  // sample's commit watermark, in the projection above, already moves the
  // screen when the store does.
  function clusterRosterProjection(cluster){
    const skip=key=>key==="last_seen_at"||key==="last_converged_at"||key==="last_applied_index";
    const walk=value=>{
      if(Array.isArray(value)) return value.map(walk);
      if(!value||typeof value!=="object") return value;
      const out={};
      for(const key of Object.keys(value).sort()) if(!skip(key)) out[key]=walk(value[key]);
      return out;
    };
    return JSON.stringify(walk(cluster===undefined?null:cluster));
  }

  return Object.freeze({
    clusterQuorum,
    clusterStateView,
    membershipRefusalText,
    clusterDirectMaintenanceStatus,
    clusterDirectOperationRow,
    clusterTransportObservation,
    clusterTransportExplanation,
    clusterMaintenanceEntryVerdict,
    clusterMaintenanceReady,
    clusterMaintenanceResumeReady,
    clusterRecoveryState,
    clusterOperationAge,
    clusterOperationReason,
    clusterObservationReason,
    clusterPlacementReason,
    clusterWireFacts,
    clusterWireSkew,
    clusterSqliteReplication,
    clusterDatabaseStatus,
    clusterCapacityText,
    clusterDatabaseHealth,
    clusterHealthPill,
    clusterSampleAge,
    clusterDatabaseWal,
    clusterReadingAge,
    clusterDatabaseRows,
    clusterHost,
    clusterAnswered,
    clusterWalHealthy,
    clusterBlockerText,
    clusterStripCells,
    clusterNodeCells,
    clusterReadinessChip,
    clusterAlertKinds,
    clusterUnobservedVoters,
    clusterVoterDownText,
    clusterRosterProjection,
    clusterSampledText,
    clusterLifecycle,
    clusterAddNodeAction,
    clusterNodeActions,
    clusterMaintenanceRows,
    clusterOpsProjection,
  });
});
