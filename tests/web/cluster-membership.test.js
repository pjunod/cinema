"use strict";

// The admin cluster-membership panel, tested against the shipped index.html
// rather than against a copy of it.
//
// What this file is actually protecting: the WORDS. The membership API already
// has its own Rust gate for lifecycle, admin gating, and refusal codes. The
// thing only this surface can get wrong is telling an operator that two voters
// are redundancy — docs/CLUSTERING-PLAN.md §7.2 makes two-node HA a stated
// non-goal precisely because two voters need both machines for every write and
// therefore survive no failure. A settings screen that renders that as a green
// "highly available" is the single most damaging bug this panel can ship, and
// no assertion on the API field would catch it. So the assertions below read
// the rendered sentences.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const INDEX = path.join(__dirname, "../../crates/plurxd/src/web/index.html");
const SHIPPED_UI = fs.readFileSync(INDEX, "utf8");

// Same extraction contract as tests/playback/web-policy.test.js: every function
// borrowed here is declared at column zero in one inline <script>, so the next
// top-level declaration terminates it and no brace parsing is needed. A rename
// fails loudly rather than silently testing nothing.
const DECLARATIONS = ["\nfunction ", "\nasync function "];
function shippedSource(name) {
  const start = DECLARATIONS.map((kind) =>
    SHIPPED_UI.indexOf(`${kind}${name}(`),
  ).find((at) => at !== -1);
  assert.notEqual(start, undefined, `index.html no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = DECLARATIONS.map((kind) => rest.indexOf(kind, 1)).filter(
    (at) => at !== -1,
  );
  const end = ends.length ? Math.min(...ends) : -1;
  return (end === -1 ? rest : rest.slice(0, end)).trimEnd();
}

// The panel functions call a handful of the app's own helpers. Borrowing the
// shipped ones rather than stubbing them keeps the escaping and the "3m ago"
// formatting under test too.
const BORROWED = [
  "esc",
  "fmtAgo",
  "fmtBytes",
  "replicationText",
  "clusterQuorum",
  "clusterStateView",
  "membershipRefusalText",
  "clusterDirectOperationRow",
  "clusterDirectMaintenanceStatus",
  "clusterMaintenanceEntryVerdict",
  "clusterMaintenanceReady",
  "clusterMaintenanceResumeReady",
  "clusterNodeOperationsBadge",
  "clusterNodeOperationsHtml",
  "clusterNodeRow",
  "clusterOperationAge",
  "clusterOperationReason",
  "clusterOperationsUnavailable",
  "clusterRestartPreparationHtml",
  "clusterOperationsCard",
  "clusterRecoveryState",
  "clusterRecoveryPanel",
  "clusterMaintenanceProgress",
  "clusterOperationsPanel",
  "forceElectionDialog",
  "clusterRefusalHtml",
  "clusterDatabaseStatus",
  "clusterCapacityText",
  "clusterDatabaseHealth",
  "clusterHealthPill",
  "clusterDatabaseSummary",
  "clusterDatabaseRows",
  "clusterDatabasePanel",
  "clusterTabButton",
  "clusterDiagnosticsFacts",
  "clusterTroubleshootingPanel",
  "clusterFoldKey",
  "clusterFoldRead",
  "clusterFoldWrite",
  "clusterNodeFoldId",
  "clusterNodeFoldLater",
  "clusterNodeFoldSave",
  "applyClusterFolds",
  "setClusterDatabaseFold",
  "toggleClusterDatabase",
  "joinPanel",
  "leavePanel",
  "joinTokenHtml",
  "clusterPanel",
  "toggleClusterNodes",
  "syncClusterNodeToggle",
  "promoteNode",
];

// One sandbox per test so a mutation of ME or CLUSTER_REFUSAL cannot leak into
// the next assertion.
function sandbox({ isAdmin = true, refusal = null, token = null } = {}) {
  const source = `
    let ME = ${JSON.stringify({ is_admin: isAdmin })};
    let CLUSTER_REFUSAL = ${JSON.stringify(refusal)};
    let CLUSTER_TOKEN = ${JSON.stringify(token)};
    let CLUSTER_LEAVING = false;
    ${BORROWED.map(shippedSource).join("\n")}
    return { ${BORROWED.join(", ")} };
  `;
  return new Function(source)();
}

let failures = 0;
function test(name, run) {
  try {
    run();
    process.stdout.write(`PASS ${name}\n`);
  } catch (error) {
    failures += 1;
    process.stdout.write(`FAIL ${name}\n${error && error.stack}\n`);
  }
}

// Words that assert redundancy or fault tolerance. Any of these in the
// two-voter banner is the exact failure this panel exists to avoid.
const REDUNDANCY_CLAIMS = [
  "highly available",
  "high availability",
  "high-availability",
  " ha ",
  "fault tolerant",
  "fault-tolerant",
  "failover",
  "fully replicated",
  "healthy",
];

function assertNoRedundancyClaim(text, where) {
  const haystack = ` ${text.toLowerCase().replace(/[^a-z]+/g, " ")} `;
  for (const claim of REDUNDANCY_CLAIMS) {
    assert.equal(
      haystack.includes(claim),
      false,
      `${where} claims redundancy with ${JSON.stringify(claim)}: ${text}`,
    );
  }
}

const REPLICATION = {
  backend: "hiqlite",
  health: "healthy",
  clustered: true,
  last_applied_term: 4,
  last_applied_index: 912,
  checked_at: 1_760_000_000,
  explanation: "This node has applied every known entry.",
};

function node(id, raftId, role, extra = {}) {
  return {
    node_id: id,
    hostname: id,
    advertised_host: `${id}.lan`,
    raft_id: raftId,
    role,
    is_voter: role === "voter",
    is_leader: false,
    reachable: true,
    last_seen_at: Date.now(),
    apply_lag_entries: 0,
    active_media_sessions: 0,
    maintenance: false,
    maintenance_acknowledged: false,
    maintenance_ready: false,
    ...extra,
  };
}

function status(availability, nodes) {
  const voting_nodes = nodes.filter((entry) => entry.is_voter).length;
  const voting_quorum = Math.floor(voting_nodes / 2) + 1;
  return {
    local_node_id: nodes[0] && nodes[0].node_id,
    availability,
    nodes,
    replication: REPLICATION,
    capacity: {
      voting_nodes,
      voting_quorum,
      voting_failure_tolerance: Math.max(0, voting_nodes - voting_quorum),
      non_voting_replicas: nodes.filter((entry) => !entry.is_voter).length,
      ready_read_workers: nodes.filter(
        (entry) => !entry.is_voter && entry.bounded_read_ready,
      ).length,
    },
    recovery: {
      required: false,
      quorum_available: true,
      reachable_voters: voting_nodes,
      required_voters: voting_quorum,
      leader_elected: true,
      permanent_majority_loss_supported: false,
    },
  };
}

function maintenanceOps(nodeId, { drained = true, admissions = 0 } = {}) {
  return {
    nodes: [
      {
        observation: "answered",
        membership: { node_id: nodeId },
        status: {
          process: { live: true },
          serving: { ready: false, reason: "maintenance" },
          media: {
            local_active_sessions: drained ? 0 : 1,
            drained,
            admissions_in_flight: admissions,
          },
        },
      },
    ],
  };
}

function operationStatus(membership, { safe = true, unreachable = false } = {}) {
  const observations = membership.nodes.map((entry, index) => ({
    membership: entry,
    observation: unreachable && index === 1 ? "unreachable" : "answered",
    sample_age_ms: 120,
    error_class: unreachable && index === 1 ? "unreachable" : null,
    status:
      unreachable && index === 1
        ? null
        : {
            observed_at_unix_ms: Date.now(),
            node_id: entry.node_id,
            raft_id: entry.raft_id,
            hostname: entry.hostname,
            build: "v0.2.7-operations",
            protocol_min: 5,
            protocol_max: 6,
            process: { live: true },
            serving: { ready: true },
            raft: {
              sample_valid: true,
              watermark_valid: true,
              sample_age_seconds: 0,
              watermark_age_millis: 50,
              current_term: 81,
              leader_id: 1,
              is_leader: entry.raft_id === 1,
              applied_index: 482191,
              commit_index: 482191,
              apply_lag_entries: 0,
            },
            wal: {
              available: true,
              snapshot: {
                state: "open",
                lock_owned: true,
                segment_count: 3,
                allocated_bytes: 6291456,
                first_retained_index: 480000,
                last_log_index: 482191,
                last_purged_index: 479999,
                last_durable_index: 482191,
                last_sync_unix_ms: Date.now(),
                last_error: null,
                last_recovery: null,
              },
            },
            snapshot: {
              available: true,
              build_ok_count: 9,
              build_error_count: 0,
              install_ok_count: 2,
              install_error_count: 0,
            },
            media: { local_active_sessions: 0, drained: true },
          },
  }));
  return {
    membership,
    nodes: observations,
    verdict: {
      safe_to_restart_one: safe && !unreachable,
      candidate_node_id: safe && !unreachable ? membership.nodes[1].node_id : null,
      blockers: unreachable
        ? [
            {
              code: "voter_not_observed",
              node_id: membership.nodes[1].node_id,
              message: "a voter did not answer the direct authenticated status probe",
            },
          ]
        : [],
      warnings: [],
    },
    maintenance: membership.nodes.map((entry) => ({
      node_id: entry.node_id,
      safe_to_enter:
        !unreachable &&
        (entry.is_voter
          ? membership.capacity.voting_nodes === 1 ||
            membership.capacity.voting_nodes >= 3
          : entry.role === "learner"),
      blockers: [],
    })),
  };
}

// ---- the two-voter state is the point -------------------------------------

test("two voters render as a reconfiguration in progress, never as redundancy", () => {
  const ui = sandbox();
  const view = ui.clusterStateView(
    status("degraded_reconfiguration", [
      node("node-a", 1, "voter"),
      node("node-b", 2, "voter"),
    ]),
  );
  assert.equal(view.tone, "warn");
  assertNoRedundancyClaim(`${view.title} ${view.body}`, "the two-voter banner");
  // It must say what the state is, not merely avoid saying the wrong thing.
  assert.match(view.title, /reconfiguration/i);
  assert.match(view.body, /survives no failure/i);
  assert.match(view.body, /third node/i);
});

test("the two-voter state reaches the rendered panel as those words", () => {
  const ui = sandbox();
  const html = ui.clusterPanel({
    cluster: status("degraded_reconfiguration", [
      node("node-a", 1, "voter"),
      node("node-b", 2, "voter"),
    ]),
    sys: { replication: REPLICATION },
  });
  // Asserting on the HTML, not on the projection: the issue's acceptance is the
  // text an operator reads, and a banner computed correctly but dropped from
  // the template would pass a view-only assertion.
  assert.match(html, /Reconfiguration in progress/);
  assert.match(html, /survives no failure/);
  assert.equal(html.includes("clstate warn"), true);
  assertNoRedundancyClaim(html.replace(/<[^>]*>/g, " "), "the two-voter panel");
  assert.match(
    html,
    /disabled title="Direct maintenance preflight has not approved this node/,
  );
});

test("three voters may say redundant, and count the loss they survive", () => {
  const ui = sandbox();
  const view = ui.clusterStateView(
    status("high_availability", [
      node("node-a", 1, "voter"),
      node("node-b", 2, "voter"),
      node("node-c", 3, "voter"),
    ]),
  );
  assert.equal(view.tone, "good");
  assert.match(view.title, /3 voters/);
  assert.match(view.body, /2 of the 3 voters/);
  assert.match(view.body, /one node is down/i);
});

test("quorum arithmetic matches Raft majorities", () => {
  const ui = sandbox();
  assert.deepEqual(ui.clusterQuorum(1), { majority: 1, tolerates: 0 });
  assert.deepEqual(ui.clusterQuorum(3), { majority: 2, tolerates: 1 });
  assert.deepEqual(ui.clusterQuorum(4), { majority: 3, tolerates: 1 });
  assert.deepEqual(ui.clusterQuorum(5), { majority: 3, tolerates: 2 });
});

test("operations card renders the server rollout verdict and direct evidence", () => {
  const ui = sandbox();
  const membership = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const operations = operationStatus(membership);
  const summary = ui.clusterOperationsCard(operations);
  const nodeHtml = ui.clusterNodeOperationsHtml(membership.nodes[1], operations);
  assert.match(summary, /Ready to restart one voter/);
  assert.match(summary, /3 voters · majority 2 · 3 ready/);
  assert.match(nodeHtml, /Follower · term 81 · lag 0/);
  assert.match(nodeHtml, /open.*3 segments.*6\.0 MB/s);
  assert.match(nodeHtml, /WAL durable index[\s\S]*482191/);
  assert.match(nodeHtml, /Unknown here — drain proxy connections separately/);
  assert.match(nodeHtml, /v0\.2\.7-operations/);
});

test("an unreachable voter is written as a blocker, never a healthy row", () => {
  const ui = sandbox();
  const membership = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const operations = operationStatus(membership, { safe: false, unreachable: true });
  const html = ui.clusterOperationsCard(operations);
  const nodeHtml = ui.clusterNodeOperationsHtml(membership.nodes[1], operations);
  assert.match(html, /Do not restart another voter/);
  assert.match(html, /voter not observed/);
  assert.match(nodeHtml, /Not observed · unreachable/);
  assert.doesNotMatch(nodeHtml, />Ready</);
});

test("election and fenced fixtures state why rollout is blocked", () => {
  const ui = sandbox();
  const membership = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const election = operationStatus(membership, { safe: false });
  election.nodes[2].status.raft.current_term = 82;
  election.nodes[2].status.raft.sample_valid = false;
  election.verdict.blockers = [{
    code: "leader_term_disagreement",
    message: "voters do not agree on one current term and known leader",
  }];
  const electionHtml = ui.clusterOperationsCard(election);
  const electionNode = ui.clusterNodeOperationsHtml(membership.nodes[2], election);
  assert.match(electionHtml, /term disagreement/);
  assert.match(electionNode, /stale or incomplete proof/);
  assert.match(electionHtml, /leader term disagreement/);

  const fenced = operationStatus(membership, { safe: false });
  fenced.nodes[1].status.serving = { ready: false, reason: "quorum_stale" };
  fenced.verdict.blockers = [{
    code: "voter_not_ready",
    node_id: "node-b",
    message: "a voter is fenced from serving new work",
  }];
  const fencedHtml = ui.clusterOperationsCard(fenced);
  const fencedNode = ui.clusterNodeOperationsHtml(membership.nodes[1], fenced);
  assert.match(fencedNode, /Fenced: quorum stale/);
  assert.match(fencedHtml, /voter not ready.*node-b/s);
});

test("build skew and WAL errors remain visible in summary and node evidence", () => {
  const ui = sandbox();
  const membership = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const skew = operationStatus(membership);
  skew.nodes[2].status.build = "v0.2.8-next";
  skew.verdict.warnings = [{
    code: "mixed_builds",
    message: "voters are directly observed on mixed builds",
  }];
  const skewHtml = ui.clusterOperationsCard(skew);
  const skewNode = ui.clusterNodeOperationsHtml(membership.nodes[2], skew);
  assert.match(skewHtml, /2 observed builds/);
  assert.match(skewHtml, /mixed builds/);
  assert.match(skewNode, /v0\.2\.8-next/);

  const walError = operationStatus(membership, { safe: false });
  walError.nodes[1].status.wal.snapshot.state = "error";
  walError.nodes[1].status.wal.snapshot.last_error = {
    observed_at_unix_ms: Date.now(),
    message: "durability proof failed",
  };
  walError.verdict.blockers = [{
    code: "wal_not_healthy",
    node_id: "node-b",
    message: "a voter's live WAL reports an error",
  }];
  const walHtml = ui.clusterOperationsCard(walError);
  const walNode = ui.clusterNodeOperationsHtml(membership.nodes[1], walError);
  assert.match(walHtml, /wal not healthy.*node-b/s);
  assert.match(walNode, /durability proof failed/);
});

test("stale samples and four-voter-one-down fixtures never imply safety", () => {
  const ui = sandbox();
  const membership = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
    node("node-d", 4, "voter"),
  ]);
  const stale = operationStatus(membership, { safe: false });
  stale.nodes[1].observation = "invalid_response";
  stale.nodes[1].error_class = "stale_peer_sample";
  stale.verdict.blockers = [{
    code: "voter_not_observed",
    node_id: "node-b",
    message: "a voter did not answer with a fresh sample",
  }];
  const staleHtml = ui.clusterOperationsCard(stale);
  const staleNode = ui.clusterNodeOperationsHtml(membership.nodes[1], stale);
  assert.match(staleNode, /Not observed · stale peer sample/);
  assert.match(staleHtml, /Do not restart another voter/);

  const oneDown = operationStatus(membership, { safe: false, unreachable: true });
  const oneDownHtml = ui.clusterOperationsCard(oneDown);
  assert.match(oneDownHtml, /4 voters · majority 3 · 3 ready/);
  assert.match(oneDownHtml, /Do not restart another voter/);
});

// ---- the never-joined install is the common one ---------------------------

test("a never-joined SQLite install reads as normal, not as something missing", () => {
  const ui = sandbox();
  const view = ui.clusterStateView({
    unavailable: true,
    code: "membership_unavailable",
    message: "cluster membership is unavailable on this backend",
  });
  assert.equal(view.tone, "calm");
  assert.match(view.body, /nothing is missing/i);
  assert.match(view.body, /opt-in/i);
  // Nothing on this panel may read as a defect on a healthy single box.
  for (const alarm of ["error", "failed", "degraded", "warning", "problem"]) {
    assert.equal(
      view.body.toLowerCase().includes(alarm),
      false,
      `the never-joined body raises ${JSON.stringify(alarm)}: ${view.body}`,
    );
  }
});

test("a never-joined install is offered no roster and no join control", () => {
  const ui = sandbox();
  const html = ui.clusterPanel({
    cluster: { unavailable: true, code: "membership_unavailable" },
    sys: {
      replication: {
        backend: "sqlite",
        health: "healthy",
        clustered: false,
        explanation: "Watch state is stored on this server only.",
      },
    },
  });
  assert.match(html, /Not clustered/);
  // A "Create a join token" button here would mint nothing and refuse — the
  // dead end this panel is supposed to not have.
  assert.equal(html.includes("Create a join token"), false);
  assert.equal(html.includes("<table"), false);
  // The one lag answer is still the shared #233 projection and its renderer.
  assert.match(html, /SQLite single-node/);
});

test("a failed roster read never claims a clustered node is not clustered", () => {
  const ui = sandbox();
  for (const cluster of [
    {
      unavailable: true,
      code: "membership_internal",
      message: "cluster membership query failed",
    },
    {
      unavailable: true,
      code: null,
      message: "network request failed",
    },
  ]) {
    const view = ui.clusterStateView(cluster);
    assert.equal(view.tone, "warn");
    assert.match(view.title, /status unavailable/i);
    assert.match(view.body, /does not mean this server has left its cluster/i);
    assert.equal(view.body.includes("nothing is missing"), false);

    const html = ui.clusterPanel({
      cluster,
      sys: { replication: REPLICATION },
    });
    assert.match(html, /Retry roster/);
    assert.equal(html.includes("Not clustered"), false);
    assert.equal(html.includes("Create a join token"), false);
  }
});

test("a replicated one-node install says one node is a complete configuration", () => {
  const ui = sandbox();
  const view = ui.clusterStateView(
    status("single_node", [node("node-a", 1, "voter")]),
  );
  assert.equal(view.tone, "calm");
  assert.match(view.body, /complete, supported configuration/i);
});

test("a learner is named as a non-voting capacity role", () => {
  const ui = sandbox();
  const view = ui.clusterStateView(
    status("single_node", [
      node("node-a", 1, "voter"),
      node("node-b", 2, "learner", { bounded_read_ready: true }),
    ]),
  );
  assert.match(view.body, /is a learner/i);
  assert.match(view.body, /holds no vote/i);
  assert.match(view.body, /not counted toward quorum/i);
  assert.match(view.body, /one is currently ready for bounded reads/i);
});

// The banner sentence has two branches and only the singular one was read, so
// reverting the plural branch on its own to the withdrawn promise kept this
// file green. That is the half an operator with two learners actually sees.
test("two learners are named the same way the one-learner sentence is", () => {
  const ui = sandbox();
  const view = ui.clusterStateView(
    status("single_node", [
      node("node-a", 1, "voter"),
      node("node-b", 2, "learner", { bounded_read_ready: true }),
      node("node-c", 3, "learner"),
    ]),
  );
  assert.match(view.body, /2 nodes are learners/i);
  assert.match(view.body, /hold no vote/i);
  assert.match(view.body, /not counted toward quorum/i);
  assert.match(view.body, /one is currently ready for bounded reads/i);
});

// The roster pill repeats the promise in a `title` attribute, and that is the
// one an operator hovers rather than reads in a banner. Nothing asserted it,
// so it could be reverted to "Admitted and catching up" by itself.
test("the learner role pill says what the banner says", () => {
  const ui = sandbox();
  const row = ui.clusterNodeRow(
    node("node-b", 2, "learner", {
      bounded_read_ready: true,
      voter_storage_ready: true,
    }),
    "node-a",
  );
  assert.match(row, /non-voting capacity role/i);
  assert.match(row, /explicit promotion adds its vote/i);
  assert.match(row, /Read worker ready/i);
  assert.match(row, /Promote to voter/i);
  assert.doesNotMatch(row, /Promote to voter" disabled/);

  // And none of that leaks onto a voter's pill.
  const voter = ui.clusterNodeRow(node("node-a", 1, "voter"), "node-a");
  assert.match(voter, />voter</);
  assert.doesNotMatch(voter, /non-voting/i);
});

test("promotion stays disabled until both learner proofs are ready", () => {
  const ui = sandbox();
  for (const extra of [
    { bounded_read_ready: false, voter_storage_ready: true },
    { bounded_read_ready: true, voter_storage_ready: false },
  ]) {
    const row = ui.clusterNodeRow(node("node-b", 2, "learner", extra), "node-a");
    assert.match(row, /Promote to voter/);
    assert.match(row, /<button class="ghost sm" disabled/);
  }
});

test("capacity text separates read workers, copies, and voting tolerance", () => {
  const ui = sandbox();
  const html = ui.clusterPanel({
    cluster: status("high_availability", [
      node("node-a", 1, "voter"),
      node("node-b", 2, "voter"),
      node("node-c", 3, "voter"),
      node("node-d", 4, "learner", { bounded_read_ready: true }),
    ]),
    sys: { replication: REPLICATION },
  });
  assert.match(html, /1 ready read worker/);
  assert.match(html, /1 non-voting replicated copy/);
  assert.match(html, /3 voters · quorum 2 · tolerates 1 voter failure/);
  assert.match(html, /do not increase voting redundancy/i);
});

test("a voter catching up is not rendered as a learner capacity role", () => {
  const ui = sandbox();
  const joining = node("node-d", 4, "voter", { is_voter: false });
  const view = ui.clusterStateView(
    status("high_availability", [
      node("node-a", 1, "voter"),
      node("node-b", 2, "voter"),
      node("node-c", 3, "voter"),
      joining,
    ]),
  );
  assert.match(view.body, /voter is joining and catching up/i);
  assert.match(view.body, /no committed vote yet/i);
  assert.doesNotMatch(view.body, /node is a learner/i);

  const row = ui.clusterNodeRow(joining, "node-a");
  assert.match(row, /joining voter/i);
  assert.doesNotMatch(row, /non-voting capacity role/i);
});

// ---- every refusal is a sentence with a next step -------------------------

// These stable codes exist in crates/plurx-core/src/cluster/membership.rs.
const REFUSAL_CODES = [
  "removal_would_lose_quorum",
  "node_owns_offline_work",
  "cluster_leader_removal_refused",
  "cluster_node_not_found",
  "self_removal_requires_leave",
  "leave_node_mismatch",
  "membership_upgrade_required",
  "membership_removal_pending",
  "learner_not_ready",
  "voter_storage_preflight_failed",
  "maintenance_conflict",
  "cluster_operation_pending",
  "maintenance_would_lose_quorum",
  "maintenance_resume_unsafe",
  "election_quorum_unavailable",
  "election_candidate_unavailable",
  "election_lifecycle_pending",
];

test("each removal refusal renders as an actionable sentence, not a code", () => {
  const ui = sandbox();
  for (const code of REFUSAL_CODES) {
    const text = ui.membershipRefusalText(code, "raw wire message");
    assert.equal(
      text.includes(code),
      false,
      `${code} leaked its raw code into the operator's sentence`,
    );
    assert.equal(
      text.includes("raw wire message"),
      false,
      `${code} fell through to the raw wire message`,
    );
    assert.ok(text.length > 80, `${code} has no real explanation: ${text}`);
    assert.match(text, /\.$/, `${code} is not a sentence: ${text}`);
  }
});

test("a stale-node refusal refreshes the roster it says is stale", () => {
  const ui = sandbox();
  const text = ui.membershipRefusalText("cluster_node_not_found", "");
  assert.match(text, /current roster is being refreshed/i);

  const removeNode = shippedSource("removeNode");
  assert.match(removeNode, /renderSettings\(\)/);
  assert.match(removeNode, /if\(e\.code==="cluster_node_not_found"\)\{/);
  assert.match(removeNode, /SETTINGS_LOADED\.delete\("cluster"\)/);
  assert.match(removeNode, /loadSettingsKey\("cluster",generation\)/);
  assert.match(removeNode, /settingsCurrent\(generation,"cluster"\)/);
});

test("node_owns_offline_work tells the operator what to do next", () => {
  const ui = sandbox();
  const text = ui.membershipRefusalText("node_owns_offline_work", "");
  // Automatic resolution can still refuse for an active transfer or work
  // created during removal, so "this node owns offline work" is a dead end.
  assert.match(text, /offline download/i);
  assert.match(text, /active downloads.*finish or delete those packages/i);
  assert.match(text, /stop clients from starting new downloads/i);
  assert.match(text, /retry the removal/i);
});

test("an unknown refusal still says something true", () => {
  const ui = sandbox();
  assert.match(
    ui.membershipRefusalText("some_future_code", "the server said no"),
    /the server said no/,
  );
  assert.match(
    ui.membershipRefusalText(null, ""),
    /refused this cluster operation and gave no reason/,
  );
});

test("a refusal is rendered into the panel against the node it names", () => {
  const ui = sandbox({
    refusal: { node_id: "node-b", code: "node_owns_offline_work", message: "" },
  });
  const html = ui.clusterRefusalHtml();
  assert.match(html, /Cluster operation for node-b was refused/);
  assert.match(html, /offline download/i);
});

// ---- roster rendering -----------------------------------------------------

test("last_seen_at is read as milliseconds, not seconds", () => {
  const ui = sandbox();
  // The API documents Unix milliseconds. Feeding fmtAgo() — which takes seconds
  // — the raw field prints a node seen a minute ago as decades stale.
  const html = ui.clusterNodeRow(
    node("node-a", 1, "voter", { last_seen_at: Date.now() - 120_000 }),
  );
  assert.match(html, /2m ago/);
  assert.equal(/\d{2,}[yd] ago/.test(html), false, `stale-looking row: ${html}`);
});

test("a roster row shows hostname and advertised host without listener ports", () => {
  const ui = sandbox();
  const row = ui.clusterNodeRow(node("node-a", 7, "voter"));
  assert.match(row, /class="clhost">node-a/);
  assert.match(row, /Advertised host node-a\.lan/);
  assert.match(row, /node-a/);
  assert.match(row, />7</);
  assert.match(row, /voter/);
  assert.match(row, /fresh/);
  // Listener ports and internal field names remain private.
  for (const leak of [":32401", ":32402", "http://", "raft_address", "api_address"]) {
    assert.equal(row.includes(leak), false, `roster row exposes ${leak}`);
  }
});

test("a pending removal is visible and offers an idempotent retry", () => {
  const ui = sandbox();
  const row = ui.clusterNodeRow(
    node("node-b", 2, "voter", { removal_pending: true }),
    "node-a",
  );
  assert.match(row, /Removal pending/);
  assert.match(row, />Retry removal</);
  const refusal = ui.membershipRefusalText("membership_removal_pending", "");
  assert.match(refusal, /finish upgrading every cluster node/i);
  assert.match(refusal, /retry this same removal/i);
  assert.match(refusal, /do not return.*to service/i);
});

test("hostname leads the identity while node id stays secondary", () => {
  const ui = sandbox();
  const row = ui.clusterNodeRow(
    node("550e8400-e29b-41d4-a716-446655440000", 7, "voter", {
      hostname: "living-room-plurx",
      advertised_host: "192.168.1.20",
    }),
  );
  assert.match(row, /class="clhost">living-room-plurx/);
  assert.match(row, /Advertised host 192\.168\.1\.20/);
  assert.match(
    row,
    /class="clid">Node ID 550e8400-e29b-41d4-a716-446655440000/,
  );
  assert.ok(
    row.indexOf("living-room-plurx") < row.indexOf("550e8400-e29b"),
    `node id appeared before hostname: ${row}`,
  );
});

test("the current leader is labeled beside its hostname", () => {
  const ui = sandbox();
  const leader = ui.clusterNodeRow(
    node("node-b", 2, "voter", { is_leader: true }),
  );
  const follower = ui.clusterNodeRow(node("node-a", 1, "voter"));
  assert.match(leader, /class="clhost">node-b[\s\S]*>Leader</);
  assert.equal(follower.includes(">Leader<"), false);
});

test("node actions stay inside the node card footer", () => {
  const ui = sandbox();
  const row = ui.clusterNodeRow(node("node-a", 1, "voter"));
  assert.match(row, /<details class="clnode[^>]* open/);
  assert.match(row, /<summary>[\s\S]*class="clnodesummary"/);
  assert.match(row, /<div class="clnodefoot">[\s\S]*<div class="row">/);
  assert.equal(row.includes("<td"), false);
});

test("a stale node heartbeat says so rather than claiming a network probe", () => {
  const ui = sandbox();
  const row = ui.clusterNodeRow(node("node-c", 3, "voter", { reachable: false }));
  assert.match(row, /stale/);
});

test("loopback is shown as localhost beside the short hostname", () => {
  const ui = sandbox();
  const row = ui.clusterNodeRow(
    node("node-local", 1, "voter", {
      hostname: "media-1",
      advertised_host: "localhost",
    }),
  );
  assert.match(row, /class="clhost">media-1/);
  assert.match(row, /Advertised host localhost/);
  assert.equal(row.includes("127.0.0.1"), false);
});

test("cluster diagnostics have a separate log surface", () => {
  const ui = sandbox();
  const html = ui.clusterPanel({
    cluster: status("single_node", [node("node-a", 1, "voter")]),
    sys: { replication: REPLICATION },
  });
  assert.match(html, /Cluster log/);
  assert.match(html, /id="cllogbox"/);
  assert.match(html, /instead of the general System log/);
});

test("the healthy cluster layout combines node membership and operations evidence", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterPanel({
    cluster,
    clusterOps: operationStatus(cluster),
    sys: { replication: REPLICATION },
  });
  assert.match(html, /class="cluster-health"/);
  assert.match(html, /id="cluster-operations"/);
  assert.match(html, /<h3>Cluster nodes<\/h3>/);
  assert.match(html, /id="cluster-node-list"/);
  assert.match(html, /Membership/);
  assert.match(html, /Operations/);
  assert.match(html, /WAL, snapshot, and protocol details/);
  assert.match(html, /class="clcontextfact">[\s\S]*Watch state/);
  assert.match(html, /class="clcontextfact">[\s\S]*Capacity/);
  assert.match(html, /<h3>Maintenance<\/h3>/);
  assert.match(html, /<h3>Planned work<\/h3>/);
  assert.match(html, /Enter maintenance/);
  assert.match(html, /Force election/);
  assert.match(html, /Danger zone · membership changes/);
  assert.equal(html.includes("<table"), false);
});

test("node cards start expanded and the all-nodes control toggles both ways", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterPanel({ cluster, sys: { replication: REPLICATION } });
  assert.equal((html.match(/<details class="clnode[^>]*" open/g) || []).length, 3);
  assert.match(html, /id="clnodes-toggle"[^>]*aria-expanded="true"[^>]*>Collapse all</);

  const cards = [{ open: true }, { open: false }, { open: true }];
  const attributes = {};
  const button = {
    textContent: "Collapse all",
    setAttribute(name, value) { attributes[name] = value; },
    focus() {},
  };
  const previousDocument = global.document;
  global.document = {
    querySelectorAll() { return cards; },
    getElementById() { return button; },
  };
  try {
    ui.toggleClusterNodes(button);
    assert.deepEqual(cards.map((card) => card.open), [true, true, true]);
    assert.equal(button.textContent, "Collapse all");
    assert.equal(attributes["aria-expanded"], "true");
    ui.toggleClusterNodes(button);
    assert.deepEqual(cards.map((card) => card.open), [false, false, false]);
    assert.equal(button.textContent, "Expand all");
    assert.equal(attributes["aria-expanded"], "false");
  } finally {
    if (previousDocument === undefined) delete global.document;
    else global.document = previousDocument;
  }
});

test("maintenance entry is available directly on a leader, busy follower, and learner", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", { active_media_sessions: 2 }),
    node("node-c", 3, "voter"),
    node("node-d", 4, "learner", { bounded_read_ready: true }),
  ]);
  const ops = operationStatus(cluster);
  ops.nodes[1].status.media = { local_active_sessions: 2, drained: false };

  for (const nodeId of ["node-a", "node-b", "node-d"]) {
    cluster.local_node_id = nodeId;
    const target = cluster.nodes.find((entry) => entry.node_id === nodeId);
    const card = ui.clusterNodeRow(target, nodeId, true, {
      operations: ops,
      lifecycleLocked: false,
    });
    assert.match(card, /Enter maintenance/);
    assert.doesNotMatch(card, /disabled[^>]*>Enter maintenance/);
  }
});

test("maintenance renders the acknowledged handoff, catch-up, and drain workflow", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", {
      maintenance: true,
      maintenance_acknowledged: true,
      maintenance_ready: false,
      active_media_sessions: 2,
    }),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterOperationsPanel(cluster, maintenanceOps("node-b", { drained: false }));
  assert.match(html, /Maintenance in progress/);
  assert.match(html, /Target acknowledged the replicated work fence/);
  assert.match(html, /Direct process work drained \(1 local operation\)/);
  assert.match(html, /maintenance fence survives a process restart/i);
  assert.match(html, /<button class="sm" disabled[^>]*>Resume service/);

  cluster.nodes[1].active_media_sessions = 0;
  cluster.nodes[1].maintenance_ready = true;
  cluster.local_node_id = "node-b";
  const ready = ui.clusterOperationsPanel(cluster, maintenanceOps("node-b"));
  assert.match(ready, /Ready to update, reboot, or resume/);
  assert.doesNotMatch(ready, /<button class="sm" disabled[^>]*>Resume service/);
});

test("maintenance resume requires direct local process evidence", () => {
  const ui = sandbox();
  const maintained = node("node-b", 2, "voter", {
    maintenance: true,
    maintenance_acknowledged: true,
    maintenance_ready: true,
  });
  const remote = ui.clusterNodeRow(maintained, "node-a", false, {
    operations: maintenanceOps("node-b"),
    lifecycleLocked: true,
  });
  assert.match(remote, /Open this node directly to resume it/);
  assert.match(remote, /<button class="sm" disabled[^>]*>Resume service/);

  const local = ui.clusterNodeRow(maintained, "node-b", false, {
    operations: maintenanceOps("node-b"),
    lifecycleLocked: true,
  });
  assert.doesNotMatch(local, /<button class="sm" disabled[^>]*>Resume service/);

  const admission = ui.clusterNodeRow(maintained, "node-b", false, {
    operations: maintenanceOps("node-b", { admissions: 1 }),
    lifecycleLocked: true,
  });
  assert.match(admission, /<button class="sm" disabled[^>]*>Resume service/);

  const recoveredLeader = { ...maintained, is_leader: true, maintenance_ready: false };
  const handoff = ui.clusterOperationsPanel(
    status("high_availability", [
      recoveredLeader,
      node("node-a", 1, "voter"),
      node("node-c", 3, "voter"),
    ]),
    maintenanceOps("node-b"),
  );
  assert.match(handoff, /Ready to hand off leadership and resume/);
  assert.doesNotMatch(handoff, /<button class="sm" disabled[^>]*>Resume service/);

  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    maintained,
    node("node-c", 3, "voter"),
  ]);
  const panel = ui.clusterPanel({ cluster, sys: { replication: REPLICATION } });
  assert.match(panel, /disabled title="Finish maintenance before changing membership[^>]*>Create a join token/);
  assert.match(panel, /disabled title="Finish maintenance before changing membership[^>]*>Leave this cluster/);
  assert.match(panel, /disabled[^>]*>Remove permanently/);
});

test("recovery keeps an active maintenance target visible and blocks elections", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", {
      reachable: false,
      maintenance: true,
      maintenance_acknowledged: true,
    }),
    node("node-c", 3, "voter", { reachable: false }),
  ]);
  cluster.recovery = {
    required: true,
    quorum_available: false,
    reachable_voters: 1,
    required_voters: 2,
    leader_elected: false,
    permanent_majority_loss_supported: false,
  };
  const html = ui.clusterOperationsPanel(cluster, null);
  assert.match(html, /Restore the maintenance target first: node-b/);
  assert.match(html, /durable work fence is still active/i);
  assert.match(html, /Force election unavailable without quorum/);

  cluster.nodes[2].reachable = true;
  cluster.recovery = {
    ...cluster.recovery,
    quorum_available: true,
    reachable_voters: 2,
  };
  const leaderless = ui.clusterOperationsPanel(cluster, null);
  assert.match(leaderless, /<button class="ghost sm" disabled[^>]*>Force election/);
});

test("lost quorum offers restoration and preservation, never force reconfiguration", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter"),
    node("node-b", 2, "voter", { reachable: false }),
    node("node-c", 3, "voter", { reachable: false }),
  ]);
  cluster.recovery = {
    required: true,
    quorum_available: false,
    reachable_voters: 1,
    required_voters: 2,
    leader_elected: false,
    permanent_majority_loss_supported: false,
  };
  const html = ui.clusterPanel({ cluster, sys: { replication: REPLICATION } });
  assert.match(html, /Recovery required/);
  assert.match(html, /Restore any missing original voter/);
  assert.match(html, /does not support force-reconfiguring around lost voters/i);
  assert.match(html, /Export support bundle/);
  assert.match(html, /Download roster snapshot/);
  assert.match(html, /Force election unavailable without quorum/);
  assert.doesNotMatch(html, /Force reconfigure|Form new cluster|Ignore quorum/i);
});

test("a leaderless cluster with quorum may request a bounded election", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter"),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  cluster.recovery = {
    required: true,
    quorum_available: true,
    reachable_voters: 3,
    required_voters: 2,
    leader_elected: false,
    permanent_majority_loss_supported: false,
  };
  const html = ui.clusterPanel({ cluster, sys: { replication: REPLICATION } });
  assert.match(html, /Leader election required/);
  assert.match(html, /A voter majority is reachable/);
  assert.match(html, /onclick="openElectionDialog\(\)"/);
  assert.match(html, /Raft still requires the reachable majority/);
});

test("the local row cannot use generic removal", () => {
  const ui = sandbox();
  const local = ui.clusterNodeRow(node("node-a", 1, "voter"), "node-a");
  const peer = ui.clusterNodeRow(node("node-b", 2, "voter"), "node-a");
  assert.match(local, /This node/);
  assert.match(local, /Use graceful leave in Danger zone/);
  assert.equal(local.includes(">Remove<"), false);
  assert.match(peer, />Remove permanently</);
});

test("graceful leave binds the POST to the roster's local node", () => {
  const panel = sandbox().leavePanel("node-a");
  assert.match(panel, /leaveCluster\(this,&quot;node-a&quot;\)/);
  const handler = shippedSource("leaveCluster");
  assert.match(handler, /body:\{node_id:nodeId\}/);
  assert.match(sandbox().leavePanel(), /identity unavailable/i);
});

// ---- admin gating ---------------------------------------------------------

test("a non-admin gets no membership panel at all", () => {
  const ui = sandbox({ isAdmin: false });
  const html = ui.clusterPanel({
    cluster: status("high_availability", [
      node("node-a", 1, "voter"),
      node("node-b", 2, "voter"),
      node("node-c", 3, "voter"),
    ]),
    sys: { replication: REPLICATION },
  });
  // Not "an empty table" — nothing, including no node ids.
  assert.equal(html, "");
});

test("Settings itself still turns a non-admin away before any of this loads", () => {
  // The panel guard above is belt and braces; this is the door. Asserted on the
  // source because the redirect needs a location and a DOM to execute.
  const viewSettings = shippedSource("viewSettings");
  assert.match(viewSettings, /if\(!ME\.is_admin\)\{\s*location\.hash="#\/";\s*return;\s*\}/);
});

// ---- the join token is bearer material ------------------------------------

test("the join token is never written to browser storage or a URL", () => {
  // An absence property, so it is asserted over the source of every function
  // that touches the token rather than by running one of them.
  const handlers = [
    "loadCluster",
    "joinPanel",
    "joinTokenHtml",
    "mintJoinToken",
    "clearJoinToken",
    "copyJoinToken",
    "forgetJoinToken",
    "clusterPanel",
  ].map((name) => `${name}:\n${shippedSource(name)}`);
  for (const source of handlers) {
    for (const sink of [
      "localStorage",
      "sessionStorage",
      "indexedDB",
      "document.cookie",
      "location.hash",
      "location.search",
      "console.log",
      "console.error",
    ]) {
      assert.equal(
        source.includes(sink),
        false,
        `a join-token handler reaches ${sink}:\n${source}`,
      );
    }
  }
  // And nothing anywhere in the app stores the token under any key.
  assert.equal(
    /(local|session)Storage\.setItem\([^)]*(TOKEN|token)[^)]*\)/.test(
      SHIPPED_UI.replace(/localStorage\.setItem\("plurx_token"/g, ""),
    ),
    false,
    "some code path persists a token through Storage.setItem",
  );
});

test("the token is shown once, with what it is and how long it lasts", () => {
  const ui = sandbox({
    token: {
      token: "plxjoin:v1:aaaa:bbbb",
      expires_at: Date.now() + 600_000,
      raft_id: 4,
      role: "learner",
    },
  });
  const html = ui.joinPanel();
  assert.match(html, /plxjoin:v1:aaaa:bbbb/);
  assert.match(html, /Shown once/);
  assert.match(html, /10 minutes/);
  assert.match(html, /Raft id 4/);
  assert.match(html, /non-voting read worker/i);
  // It must say what the holder of the token can do, not just that it is secret.
  assert.match(html, /complete authority\s+to join a node/i);
  assert.match(html, /Done — clear it/);
});

test("clearing the token removes it from the rendered panel", () => {
  const ui = sandbox({ token: null });
  const html = ui.joinPanel();
  assert.equal(html.includes("plxjoin:"), false);
  assert.match(html, /Create a join token/);
  assert.match(html, /Voting member/);
  assert.match(html, /Read worker/);
});

test("learner issuance and promotion are wired to their admin APIs", () => {
  const mint = shippedSource("mintJoinToken");
  assert.match(mint, /clrole/);
  assert.match(mint, /role==="learner"/);
  assert.match(mint, /\/cluster\/learner-join-tokens/);
  assert.match(mint, /\/cluster\/join-tokens/);
  assert.equal(mint.includes("body:{expires_in_seconds,role}"), false);
  assert.match(mint, /CLUSTER_TOKEN=\{\.\.\.token,role\}/);

  const promote = shippedSource("promoteNode");
  assert.match(promote, /\/cluster\/nodes\/\$\{encodeURIComponent\(nodeId\)\}\/promote/);
  assert.match(promote, /method:"POST"/);
  assert.match(promote, /Learner promoted to voter/);
});

test("maintenance and election controls use their bounded admin APIs", () => {
  const maintenance = shippedSource("setNodeMaintenance");
  assert.match(
    maintenance,
    /\/cluster\/nodes\/\$\{encodeURIComponent\(nodeId\)\}\/maintenance/,
  );
  assert.match(maintenance, /method:enter\?"POST":"DELETE"/);
  assert.match(maintenance, /New work will be fenced and existing streams will drain/);

  const election = shippedSource("forceClusterElection");
  assert.match(election, /api\("\/cluster\/election",\{method:"POST"/);
  assert.match(election, /Leader election completed/);
  const dialog = sandbox().forceElectionDialog(true);
  assert.match(dialog, /Raft—not this screen—selects the next leader/);
  assert.match(dialog, /I understand this may briefly pause writes/);
  assert.match(dialog, /id="clelectiongo" disabled/);
});

test("routing away from Settings drops the in-memory join token", () => {
  const render = shippedSource("render");
  assert.match(
    render,
    /if\(h!=="#\/settings"&&h!=="#\/admin"\) forgetJoinToken\(\)/,
  );
});

// ---- wiring ---------------------------------------------------------------

test("the Cluster tab is registered and dispatched", () => {
  assert.match(SHIPPED_UI, /\["cluster","Cluster"\]/);
  assert.match(SHIPPED_UI, /if\(tab==="cluster"\)\s*return clusterPanel\(d\)/);
  // The active-tab manifest fetches this roster on first Cluster open; it is
  // absent from every other tab's dependency wave.
  assert.match(SHIPPED_UI, /cluster:\{required:\["cluster"\],secondary:\["clusterOps"\]\}/);
  assert.match(SHIPPED_UI, /cluster:\(\)=>api\("\/cluster\/nodes"\)/);
  assert.match(SHIPPED_UI, /clusterOps:\(\)=>api\("\/cluster\/status"\)/);
  assert.equal(
    /Promise\.all\(\[[^\]]*cluster\/nodes/.test(SHIPPED_UI),
    false,
    "the roster is fetched eagerly with the rest of Settings",
  );
});

test("late cluster work can repaint only a live Settings route", () => {
  // loadCluster, token minting, and removal all resolve after a network await.
  // Their shared render choke point must reject a stale completion after the
  // user has navigated elsewhere, even though the selected tab remains in
  // localStorage.
  const renderSettings = shippedSource("renderSettings");
  assert.match(
    renderSettings,
    /const h=location\.hash;\s*if\(h!=="#\/settings"&&h!=="#\/admin"\) return/,
  );
  for (const handler of ["loadCluster", "mintJoinToken", "removeNode"]) {
    assert.match(
      shippedSource(handler),
      /renderSettings\(\)/,
      `${handler} no longer renders through the guarded Settings choke point`,
    );
  }
});

test("this panel calls only the eleven cluster endpoints the node API ships", () => {
  const called = new Set();
  const CALL = /api\(\s*([`"'])(\/cluster[^`"']*)\1/g;
  for (const [, , route] of SHIPPED_UI.matchAll(CALL)) {
    // A template hole is a node id, and a node id is a fact about one cluster
    // rather than about the routes this panel speaks to.
    called.add(route.replace(/\$\{[^}]*\}/g, "<id>"));
  }
  assert.deepEqual(
    [...called].sort(),
    [
      "/cluster/election",
      "/cluster/join-tokens",
      "/cluster/learner-join-tokens",
      "/cluster/leave",
      "/cluster/nodes",
      "/cluster/nodes/<id>",
      "/cluster/nodes/<id>/maintenance",
      "/cluster/nodes/<id>/promote",
      "/cluster/nodes/<id>/restart-preparation",
      "/cluster/status",
      "/cluster/support-bundle",
    ],
  );
});

test("the typed refusal code survives the fetch helper", () => {
  // membershipRefusalText is keyed on the stable code, so an api() that drops it
  // would silently downgrade every refusal to the raw wire message.
  const api = shippedSource("api");
  assert.match(api, /code=b\.code\|\|null/);
  assert.match(api, /error\.code=code/);
});

// ---- the replicated database section --------------------------------------
// The store used to have no section of its own: one run-on sentence between the
// node cards, and everything else about Raft either inside a node's evidence or
// only in /metrics. These pin the section, and pin where each reading comes
// from — a wrong source here reads as a plausible number, which is worse than a
// blank.

test("the replicated database is its own section with the store's own readings", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterPanel({
    cluster,
    clusterOps: operationStatus(cluster),
    sys: { replication: REPLICATION },
  });
  assert.match(html, /<h3>Replicated database<\/h3>/);
  assert.match(html, /id="cluster-database"/);
  assert.match(html, /<dt>Quorum commit<\/dt><dd class="num">482191<\/dd>/);
  assert.match(html, /<dt>Term<\/dt><dd class="num">81<\/dd>/);
  assert.match(html, /<dt>Apply lag<\/dt><dd class="num">0 entries<\/dd>/);
  assert.match(html, /<dt>Protocol<\/dt><dd class="num">5–6<\/dd>/);
  assert.match(html, /<dt>Snapshots<\/dt><dd class="num">build 9 ok \/ 0 error · install 2 ok \/ 0 error<\/dd>/);
  assert.match(html, /<dt>Applied here<\/dt><dd class="num">912/);
  // The store's facts sit in the store's column: watch state must reach the
  // page before the node roster, not inside it.
  assert.ok(
    html.indexOf("Watch state") < html.indexOf("<h3>Cluster nodes</h3>"),
    "watch state is still rendered inside the node card",
  );
});

test("the database section is foldable and keeps its verdict folded", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const ops = operationStatus(cluster);
  const html = ui.clusterPanel({ cluster, clusterOps: ops, sys: { replication: REPLICATION } });
  // Ships open; the summary is the folded state and is hidden until then.
  assert.match(html, /id="cldb-toggle"[^>]*aria-expanded="true"[^>]*>Hide</);
  assert.match(html, /id="cluster-database-summary" hidden/);
  // Folding hides the detail, never the verdict: the pill stays in the header
  // and the summary keeps the readings somebody checks before expanding again.
  const summary = ui.clusterDatabaseSummary(cluster, REPLICATION, ops);
  assert.match(summary, /leader <b>node-a<\/b>/);
  assert.match(summary, /term <b>81<\/b>/);
  assert.match(summary, /commit <b>482191<\/b>/);
  assert.match(summary, /applied <b>912<\/b>/);
  assert.match(summary, /lag <b>0<\/b>/);
  assert.match(html, /class="pill"[^>]*>In sync</);
});

test("the quorum watermark is the quorum's, not this node's applied index", () => {
  // The fixture above has commit == applied, which would let the watermark be
  // read from the wrong field and still look right. Separate them: a follower
  // that is four entries behind must show the quorum's commit, its own applied
  // index, and the gap between them.
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const behind = operationStatus(cluster);
  const local = behind.nodes.find((row) => row.membership.node_id === cluster.local_node_id);
  local.status.raft.commit_index = 482195;
  local.status.raft.applied_index = 482191;
  local.status.raft.apply_lag_entries = 4;
  const rows = new Map(
    ui.clusterDatabaseRows(cluster, { ...REPLICATION, last_applied_index: 482191, behind_by: 4 }, behind)
      .map((row) => [row[0], row[1]]),
  );
  assert.equal(rows.get("Quorum commit"), "482195");
  assert.match(rows.get("Applied here"), /^482191/);
  assert.equal(rows.get("Apply lag"), "4 entries");
  assert.equal(rows.get("Behind by"), "4 changes");
});

test("database readings say unknown instead of inventing a number", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [node("node-a", 1, "voter", { is_leader: true })]);
  const rows = new Map(ui.clusterDatabaseRows(cluster, REPLICATION, null).map((row) => [row[0], row[1]]));
  assert.equal(rows.get("Term"), "unknown");
  assert.equal(rows.get("Quorum commit"), "unknown");
  assert.equal(rows.get("Apply lag"), "unknown");
  assert.equal(rows.get("Protocol"), "unknown");
  // The membership projection is a different source and is still present, so
  // the applied index must not be blanked along with the direct sample.
  assert.match(rows.get("Applied here"), /^912/);
  assert.match(rows.get("Reading age"), /stale or incomplete proof/);
});

test("a single-node install reads as one node, never as redundancy", () => {
  const ui = sandbox();
  const sqlite = {
    backend: "sqlite",
    health: "healthy",
    clustered: false,
    explanation: "Watch state is stored on this server only.",
  };
  const html = ui.clusterPanel({
    cluster: { unavailable: true, code: "membership_unavailable", nodes: [] },
    sys: { replication: sqlite },
  });
  assert.match(html, /<h3>Replicated database<\/h3>/);
  assert.match(html, /SQLite single-node/);
  assert.match(html, /class="pill"[^>]*>Single node</);
  assertNoRedundancyClaim(html, "the single-node database section");
});

// ---- folding, and remembering it ------------------------------------------
// The fold is a per-browser convenience. These pin the two properties that
// matter: it is keyed by node id rather than by row, and a browser that refuses
// storage still gets a working panel.

function foldDom({ cards = [], toggle = null, database = null } = {}) {
  const byId = {};
  if (toggle) byId["clnodes-toggle"] = toggle;
  if (database) Object.assign(byId, database);
  return {
    querySelectorAll: () => cards,
    getElementById: (id) => byId[id] || null,
  };
}

function foldCard(nodeId, open) {
  return {
    open,
    querySelector: () => ({ getAttribute: () => nodeId }),
  };
}

function withDom(document, localStorage, run) {
  const priorDocument = global.document;
  const priorStorage = Object.getOwnPropertyDescriptor(global, "localStorage");
  global.document = document;
  if (localStorage === "blocked") {
    Object.defineProperty(global, "localStorage", {
      configurable: true,
      get() {
        throw new Error("site data is blocked in this browser");
      },
    });
  } else {
    Object.defineProperty(global, "localStorage", { configurable: true, value: localStorage });
  }
  try {
    return run();
  } finally {
    global.document = priorDocument;
    delete global.localStorage;
    if (priorStorage) Object.defineProperty(global, "localStorage", priorStorage);
  }
}

function fakeStorage(seed = {}) {
  const map = new Map(Object.entries(seed));
  return {
    getItem: (key) => (map.has(key) ? map.get(key) : null),
    setItem: (key, value) => map.set(key, String(value)),
    read: (key) => map.get(key),
  };
}

test("a collapsed node is remembered by node id, not by its row", () => {
  const ui = sandbox();
  const storage = fakeStorage();
  const toggle = { textContent: "Collapse all", setAttribute() {}, focus() {} };
  const cards = [foldCard("node-a", true), foldCard("node-b", false), foldCard("node-c", true)];
  withDom(foldDom({ cards, toggle }), storage, () => ui.clusterNodeFoldSave());
  assert.deepEqual(JSON.parse(storage.read("plurx.cluster.fold")), { nodes_closed: ["node-b"] });

  // node-b has since left the cluster and node-d has joined. The stored set is
  // keyed by id, so node-d does not inherit node-b's collapsed row.
  const next = [foldCard("node-a", true), foldCard("node-d", true), foldCard("node-c", true)];
  withDom(foldDom({ cards: next, toggle }), storage, () => ui.applyClusterFolds());
  assert.deepEqual(
    next.map((card) => card.open),
    [true, true, true],
  );

  const again = [foldCard("node-a", true), foldCard("node-b", true)];
  withDom(foldDom({ cards: again, toggle }), storage, () => ui.applyClusterFolds());
  assert.deepEqual(
    again.map((card) => card.open),
    [true, false],
  );
});

test("a browser that refuses storage still gets the shipped defaults", () => {
  const ui = sandbox();
  const toggle = { textContent: "Collapse all", setAttribute() {}, focus() {} };
  const cards = [foldCard("node-a", true), foldCard("node-b", false)];
  withDom(foldDom({ cards, toggle }), "blocked", () => {
    // Neither the restore nor the save may throw, and neither may rewrite the
    // markup's own defaults on the way past.
    ui.applyClusterFolds();
    ui.clusterNodeFoldSave();
    assert.equal(ui.clusterFoldRead(), null);
  });
  assert.deepEqual(
    cards.map((card) => card.open),
    [true, false],
  );
});

test("folding the database flips its control and is remembered", () => {
  const ui = sandbox();
  const storage = fakeStorage();
  const body = { hidden: false };
  const summary = { hidden: true };
  const button = {
    textContent: "Hide",
    attrs: {},
    setAttribute(name, value) {
      this.attrs[name] = value;
    },
    focus() {},
  };
  const dom = foldDom({
    database: {
      "cluster-database": body,
      "cluster-database-summary": summary,
      "cldb-toggle": button,
    },
  });
  withDom(dom, storage, () => ui.toggleClusterDatabase(button));
  assert.equal(body.hidden, true);
  assert.equal(summary.hidden, false);
  assert.equal(button.textContent, "Show");
  assert.equal(button.attrs["aria-expanded"], "false");
  assert.deepEqual(JSON.parse(storage.read("plurx.cluster.fold")), { database: false });

  withDom(dom, storage, () => ui.toggleClusterDatabase(button));
  assert.equal(body.hidden, false);
  assert.equal(summary.hidden, true);
  assert.equal(button.textContent, "Hide");
  assert.equal(button.attrs["aria-expanded"], "true");
  assert.deepEqual(JSON.parse(storage.read("plurx.cluster.fold")), { database: true });
});

test("a page load cannot overwrite the remembered folds", () => {
  // Chrome fires one `toggle` per <details open> while the panel is parsed, so
  // persisting from `ontoggle` overwrites the stored set with "everything open"
  // on every visit. Measured in a real browser, not assumed. The card persists
  // from a click — a real gesture — and `ontoggle` stays presentational.
  const row = shippedSource("clusterNodeRow");
  assert.match(row, /ontoggle="syncClusterNodeToggle\(\)"/);
  assert.doesNotMatch(row, /ontoggle="[^"]*clusterNodeFoldSave/);
  assert.match(row, /onclick="clusterNodeFoldLater\(\)"/);
  // And the click saves after the card's open state has actually flipped.
  assert.match(shippedSource("clusterNodeFoldLater"), /setTimeout\(clusterNodeFoldSave,0\)/);
});

test("the fold never carries anything but the fold", () => {
  // The panel holds a live join credential in memory. The fold is the one thing
  // on this screen that is allowed to reach browser storage, so pin its shape.
  const write = shippedSource("clusterFoldWrite");
  assert.match(write, /localStorage\.setItem\(clusterFoldKey\(\),JSON\.stringify\(/);
  assert.match(shippedSource("clusterNodeFoldSave"), /nodes_closed/);
  assert.match(shippedSource("toggleClusterDatabase"), /clusterFoldWrite\(\{database:open\}\)/);
  assert.doesNotMatch(shippedSource("clusterDatabasePanel"), /localStorage/);
});

// ---- troubleshooting -------------------------------------------------------

test("troubleshooting is one section with exactly one visible pane", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterPanel({
    cluster,
    clusterOps: operationStatus(cluster),
    sys: { replication: REPLICATION },
  });
  assert.match(html, /<h3>Troubleshooting<\/h3>/);
  assert.equal((html.match(/role="tab"/g) || []).length, 3);
  // The log pane is the open one; the other two ship hidden so exactly one
  // answer is on screen at a time.
  assert.match(html, /id="cltab-log" aria-selected="true"/);
  assert.match(html, /id="clpane-log" role="tabpanel"[^>]*>/);
  assert.match(html, /id="clpane-readings"[^>]*hidden/);
  assert.match(html, /id="clpane-refusals"[^>]*hidden/);
  assert.match(html, /id="cllogbox"/);
  assert.match(html, /Heartbeat quorum/);
  assert.match(html, /2 of 3 voters must agree on every write/);
});

test("a refusal is kept in troubleshooting, not spent on a toast", () => {
  const ui = sandbox({
    refusal: { node_id: "node-b", code: "node_owns_offline_work", message: "" },
  });
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterPanel({ cluster, sys: { replication: REPLICATION } });
  const pane = html.slice(html.indexOf('id="clpane-refusals"'));
  assert.match(pane, /Cluster operation for node-b was refused/);
  assert.match(pane, /offline download/i);
});

process.exit(failures ? 1 : 0);
