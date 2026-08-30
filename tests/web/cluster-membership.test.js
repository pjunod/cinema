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

// The model has a file boundary now, so it is required rather than sliced out
// of the shell. Everything still declared inline — the templates, the fold DOM
// glue — keeps the extraction harness below; that harness shrinks as the
// boundary grows, which is the point of moving it.
const PANEL = require("../../crates/plurxd/src/web/cluster-panel.js");

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
  "clenv",
  "fmtAgo",
  "fmtBytes",
  "replicationText",
  "clusterNodeOperationsBadge",
  "clusterNodeOperationsHtml",
  "clusterNodeRow",
  "clusterOperationsUnavailable",
  "clusterRestartPreparationHtml",
  "clusterOperationsCard",
  "clusterRecoveryPanel",
  "clusterMaintenanceProgress",
  "clusterOperationsPanel",
  "forceElectionDialog",
  "clusterRefusalHtml",
  "clusterOperationsRail",
  "clusterDatabasePanel",
  "clusterTabButton",
  "clusterDiagnosticsFacts",
  "clusterTroubleshootingPanel",
  "showClusterTab",
  "selectClusterTab",
  "clusterNodeFoldId",
  "clusterNodeFoldLater",
  "clusterNodeFoldSave",
  "clusterStorage",
  "clusterFoldState",
  "clusterFoldSave",
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
function sandbox({ isAdmin = true, refusal = null, token = null, expanded = [] } = {}) {
  const source = `
    let ME = ${JSON.stringify({ is_admin: isAdmin })};
    let CLUSTER_REFUSAL = ${JSON.stringify(refusal)};
    let CLUSTER_TOKEN = ${JSON.stringify(token)};
    let CLUSTER_RAIL_EXPANDED = ${JSON.stringify(expanded)};
    let CLUSTER_LEAVING = false;
    ${BORROWED.map(shippedSource).join("\n")}
    // The shell's own esc/fmtAgo/fmtBytes, handed to the model exactly the way
    // the browser hands them over. Borrowing the real ones rather than stubbing
    // keeps the escaping and the "3m ago" formatting under test too.
    return Object.assign({}, PlurxClusterPanel, { ${BORROWED.join(", ")} }, { env: clenv() });
  `;
  return new Function("PlurxClusterPanel", source)(PANEL);
}

let failures = 0;
// Awaited, and the exit code deferred to beforeExit — an async test whose body
// is dropped on the floor prints PASS with zero assertions run, which is how
// four behaviours once shipped with their coverage written and not executing.
// Same harness as tests/web/page-read-budget.test.js; keep them identical.
async function test(name, run) {
  try {
    await run();
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
  // Planned work is now the rail, which states the same two actions with
  // their preconditions filled in rather than as prose.
  assert.match(html, /class="clrail"/);
  assert.match(html, /Enter maintenance on node-a/);
  assert.match(html, /Force a leader election/);
  // The membership controls are the rail's own rows now, not a second card.
  assert.equal(html.includes("cldanger"), false);
  assert.match(html, /id="clrail-add"/);
  assert.match(html, /id="clrail-leave"/);
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
  // A membership change is in flight, so the rows that would start another one
  // are blocked — and a blocked row carries no expansion at all. An unreachable
  // control is better than a disabled control the operator has to open to find.
  assert.equal(panel.includes('id="clrail-add"'), false);
  assert.equal(panel.includes("Create a join token"), false);
  assert.match(panel, /A membership change is already in flight on node-b/);
  const leaving = ui.leavePanel("node-a", true);
  assert.match(leaving, /disabled title="Finish maintenance before changing membership[^>]*>Leave this cluster/);
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
  assert.match(local, /Use Leave this cluster on the Maintenance card/);
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
      /renderSettings\(\)|repaintClusterPreserving\(renderSettings\)/,
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
  // The panel reports THIS machine's store, so every peer gets readings that
  // would be obviously wrong here: a follower reading the leader's row is the
  // mistake this fixture exists to catch.
  const ops = operationStatus(cluster);
  ops.nodes.forEach((row) => {
    if (row.membership.node_id === cluster.local_node_id) return;
    row.status.raft.current_term = 77;
    row.status.raft.commit_index = 111111;
    row.status.raft.applied_index = 111111;
    row.status.protocol_min = 1;
    row.status.protocol_max = 2;
    row.status.snapshot.build_ok_count = 999;
  });
  const html = ui.clusterPanel({ cluster, clusterOps: ops, sys: { replication: REPLICATION } });
  assert.match(html, /<h3>Replicated database<\/h3>/);
  assert.match(html, /id="cluster-database"/);
  assert.match(html, /<dt>Quorum commit<\/dt><dd class="num">482191<\/dd>/);
  assert.match(html, /<dt>Term<\/dt><dd class="num">81<\/dd>/);
  assert.match(html, /<dt>Apply lag<\/dt><dd class="num">0 entries<\/dd>/);
  assert.match(html, /<dt>Protocol<\/dt><dd class="num">5–6<\/dd>/);
  assert.match(html, /<dt>Snapshots<\/dt><dd class="num">build 9 ok \/ 0 error · install 2 ok \/ 0 error<\/dd>/);
  // The watermark and the applied index have to come from one sample or their
  // difference is not a lag, so both are the direct status's, not the
  // membership projection's separately-fetched index.
  assert.match(html, /<dt>Applied here<\/dt><dd class="num">482191<\/dd>/);
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
  const summary = ui.clusterDatabaseSummary(ui.env, cluster, REPLICATION, ops);
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
    ui.clusterDatabaseRows(ui.env, cluster, { ...REPLICATION, last_applied_index: 482191, behind_by: 4 }, behind)
      .map((row) => [row[0], row[1]]),
  );
  assert.equal(rows.get("Quorum commit"), "482195");
  assert.match(rows.get("Applied here"), /^482191/);
  assert.equal(rows.get("Apply lag"), "4 entries");
  assert.equal(rows.get("Behind by"), "4 changes");
});

test("the database reports this machine's store, not the leader's", () => {
  // On a follower these are two different rows of the same response, and the
  // panel's whole claim is that it describes the machine you are looking at:
  // its applied index, its lag, its WAL, its protocol range. A fixture where
  // the local node is also the leader cannot tell the two apart.
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter"),
    node("node-b", 2, "voter", { is_leader: true }),
    node("node-c", 3, "voter"),
  ]);
  assert.equal(cluster.local_node_id, "node-a");
  const ops = operationStatus(cluster);
  ops.nodes.forEach((row) => {
    const local = row.membership.node_id === cluster.local_node_id;
    row.status.raft.is_leader = row.membership.raft_id === 2;
    row.status.raft.applied_index = local ? 482100 : 482191;
    row.status.raft.commit_index = local ? 482191 : 482191;
    row.status.raft.apply_lag_entries = local ? 91 : 0;
    row.status.protocol_max = local ? 6 : 9;
  });
  const rows = new Map(ui.clusterDatabaseRows(ui.env, cluster, REPLICATION, ops).map((row) => [row[0], row[1]]));
  assert.equal(rows.get("Applied here"), "482100");
  assert.equal(rows.get("Apply lag"), "91 entries");
  assert.equal(rows.get("Protocol"), "5–6");
  // The leader row is still the leader's: naming the elected leader is a
  // membership fact, and it must not drag its readings along with it.
  assert.match(rows.get("Leader"), /node-b/);
});

test("database readings say unknown instead of inventing a number", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [node("node-a", 1, "voter", { is_leader: true })]);
  const rows = new Map(ui.clusterDatabaseRows(ui.env, cluster, REPLICATION, null).map((row) => [row[0], row[1]]));
  assert.equal(rows.get("Term"), "unknown");
  assert.equal(rows.get("Quorum commit"), "unknown");
  assert.equal(rows.get("Apply lag"), "unknown");
  assert.equal(rows.get("Protocol"), "unknown");
  // The membership projection is a different source and is still present, so
  // the applied index must not be blanked along with the direct sample.
  assert.match(rows.get("Applied here"), /^912/);
  assert.equal(rows.get("Reading age"), "unknown");
  // The counting rows are the tempting ones: `build 0 ok / 0 error` reads as a
  // measurement, and "0 changes behind" beside a degraded pill is a claim.
  assert.equal(rows.get("Snapshots"), "unknown");
  assert.equal(rows.get("WAL"), "unknown");
  const degraded = new Map(
    ui.clusterDatabaseRows(ui.env, cluster, { backend: "hiqlite", health: "degraded", clustered: true }, null)
      .map((row) => [row[0], row[1]]),
  );
  assert.equal(degraded.get("Behind by"), "unknown");
});

test("a proven-unavailable store reads as broken, not as unobserved", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
  ]);
  const ops = operationStatus(cluster);
  const local = ops.nodes[0];
  local.status.snapshot = { available: false, build_ok_count: 0, build_error_count: 0, install_ok_count: 0, install_error_count: 0 };
  local.status.wal = { available: false, reason: "lock_contended" };
  const rows = new Map(ui.clusterDatabaseRows(ui.env, cluster, REPLICATION, ops).map((row) => [row[0], row[1]]));
  assert.match(rows.get("Snapshots"), /unavailable/);
  assert.match(rows.get("WAL"), /unavailable/);
  assert.match(rows.get("WAL"), /lock contended/);
  assert.match(rows.get("WAL"), /var\(--bad\)/);

  // An open WAL that is holding unflushed entries or carrying a live error is
  // a fault, and the ledger must say so with the same predicate the node badge
  // uses rather than printing a neutral "open".
  const faulted = operationStatus(cluster);
  faulted.nodes[0].status.wal.snapshot.last_log_index = 482195;
  const walRow = new Map(ui.clusterDatabaseRows(ui.env, cluster, REPLICATION, faulted).map((row) => [row[0], row[1]])).get("WAL");
  assert.match(walRow, /var\(--bad\)/);
  assert.match(walRow, /482195 logged, 482191 durable/);
});

test("the freshness row ages while the tab stays open", () => {
  // Every age in the direct status is computed on the server and frozen into
  // the response, and this panel renders from one fetch for as long as the tab
  // is open. Without the time since the aggregate was taken the row that
  // exists to report staleness says "now" indefinitely.
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
  ]);
  const ops = operationStatus(cluster);
  ops.observed_at_unix_ms = Date.now() - 600_000;
  const rows = new Map(ui.clusterDatabaseRows(ui.env, cluster, REPLICATION, ops).map((row) => [row[0], row[1]]));
  assert.match(rows.get("Reading age"), /local 10m ago/);
  assert.match(rows.get("Reading age"), /watermark 10m ago/);
  assert.equal(ui.clusterSampleAge(ops, null), null);
});

test("a cold load into the Cluster tab still knows it is a single-node install", () => {
  // `sys` is not in this tab's manifest, so opening Settings straight onto
  // Cluster has no replication projection at all. Reporting that as "status
  // unavailable" under a banner saying nothing is wrong made the verdict
  // depend on which tab you happened to visit first.
  const ui = sandbox();
  const html = ui.clusterPanel({ cluster: { unavailable: true, code: "membership_unavailable", nodes: [] } });
  assert.match(html, /class="pill"[^>]*>Single node</);
  assert.match(html, /SQLite single-node/);
  assert.doesNotMatch(html, /Status unavailable/);
  assert.match(html, /<dt>Backend<\/dt><dd>SQLite · single node<\/dd>/);
});

test("a machine that is not clustered is not told to preserve a voter", () => {
  // The restart-safety verdict and the roster both belong to a cluster. Left
  // mounted on a SQLite install, /cluster/status's 503 was patched into the
  // page as "Do not restart another voter", beside an empty node card.
  const ui = sandbox();
  const html = ui.clusterPanel({ cluster: { unavailable: true, code: "membership_unavailable", nodes: [] } });
  assert.doesNotMatch(html, /id="cluster-operations"/);
  assert.doesNotMatch(html, /<h3>Cluster nodes<\/h3>/);
  assert.doesNotMatch(html, /<h3>Maintenance<\/h3>/);
  // Nor a ledger of eleven "unknown" rows: there is no Raft here to fail to
  // read, so the section says what is true about a single machine.
  assert.match(html, /<dt>Peers<\/dt><dd>none — watch state is durable here/);
  assert.doesNotMatch(html, /<dt>Quorum commit<\/dt>/);
  // And the readings pane must not answer with arithmetic over zero voters.
  assert.match(html, /No committed voter roster is readable/);
  assert.doesNotMatch(html, /of 0 voters must agree/);
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
  // Reads like the browser does: the id lives on the card body, under the
  // attribute the markup writes. A stub that answers any selector with any
  // attribute would let both of those be renamed without a failure.
  const body = { getAttribute: (name) => (name === "data-node" ? nodeId : null) };
  return {
    open,
    querySelector: (selector) => (selector === ".clnodebody" ? body : null),
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
    assert.equal(ui.clusterFoldState(), null);
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
  assert.match(row, /onclick="clusterNodeFoldLater\(this\)"/);
  // And the card carries the id the fold is keyed by, where the reader looks.
  assert.match(row, /<div class="clnodebody" data-node="\$\{esc\(n\.node_id\)\}">/);
  // And the click saves after the card's open state has actually flipped.
  assert.match(shippedSource("clusterNodeFoldLater"), /setTimeout\(\(\)=>\{\s*clusterNodeFoldSave\(\);/);
});

test("the fold is restored where the panel is written, and saved by every control", () => {
  // Both are call sites: the helpers can be perfect while the panel never
  // calls them. Deleting either line leaves the feature dead with the rest of
  // these tests green.
  assert.match(shippedSource("renderSettings"), /if\(tab==="cluster"\)\{ applyClusterFolds\(\); return refreshClusterLogs\(\); \}/);
  assert.match(shippedSource("toggleClusterNodes"), /clusterNodeFoldSave\(\);/);
  assert.match(shippedSource("selectClusterTab"), /clusterFoldSave\(\{tab:id\}\)/);
});

test("the troubleshooting tab is a fold like any other", () => {
  const ui = sandbox();
  const storage = fakeStorage();
  const panes = {
    "clpane-log": { hidden: false },
    "clpane-readings": { hidden: true },
    "clpane-refusals": { hidden: true },
  };
  const tabs = [
    { id: "cltab-log", attrs: {}, controls: "clpane-log" },
    { id: "cltab-readings", attrs: {}, controls: "clpane-readings" },
    { id: "cltab-refusals", attrs: {}, controls: "clpane-refusals" },
  ].map((tab) => ({
    ...tab,
    setAttribute(name, value) { this.attrs[name] = value; },
    getAttribute: () => tab.controls,
  }));
  const document = {
    querySelectorAll: () => tabs,
    getElementById: (id) => panes[id] || tabs.find((tab) => tab.id === id) || null,
  };
  withDom(document, storage, () => ui.selectClusterTab("readings"));
  assert.deepEqual(
    Object.entries(panes).map(([id, pane]) => [id, pane.hidden]),
    [["clpane-log", true], ["clpane-readings", false], ["clpane-refusals", true]],
  );
  assert.deepEqual(tabs.map((tab) => tab.attrs["aria-selected"]), ["false", "true", "false"]);
  // Every operational change repaints this panel from scratch, so a chosen tab
  // that is not remembered snaps back to the log every few seconds while you
  // watch a node drain.
  assert.equal(JSON.parse(storage.read("plurx.cluster.fold")).tab, "readings");
  panes["clpane-readings"].hidden = true;
  panes["clpane-log"].hidden = false;
  withDom(document, storage, () => ui.applyClusterFolds());
  assert.equal(panes["clpane-readings"].hidden, false);
  assert.equal(panes["clpane-log"].hidden, true);
});

test("the fold never carries anything but the fold", () => {
  // The panel holds a live join credential in memory. The fold is the one thing
  // on this screen that is allowed to reach browser storage, so pin its shape.
  const write = PANEL.clusterFoldWrite.toString();
  assert.match(write, /storage\.setItem\(clusterFoldKey\(\),JSON\.stringify\(/);
  assert.match(shippedSource("clusterNodeFoldSave"), /nodes_closed/);
  assert.match(shippedSource("toggleClusterDatabase"), /clusterFoldSave\(\{database:open\}\)/);
  assert.doesNotMatch(shippedSource("clusterDatabasePanel"), /localStorage/);
  // Only the shell may hold the storage handle: the model takes it as an
  // argument, so a test never stubs a global to exercise it.
  assert.doesNotMatch(PANEL.clusterFoldRead.toString(), /localStorage/);
});

test("a stored fold that is not a fold is ignored, not carried forward", () => {
  const ui = sandbox();
  const junk = fakeStorage({ "plurx.cluster.fold": JSON.stringify([1, 2, 3]) });
  withDom(foldDom(), junk, () => assert.equal(ui.clusterFoldState(), null));
  const stray = fakeStorage({
    "plurx.cluster.fold": JSON.stringify({ database: false, nodes_closed: ["a", 7], tab: "log", stowaway: "x" }),
  });
  withDom(foldDom(), stray, () => {
    assert.deepEqual(ui.clusterFoldState(), { database: false, nodes_closed: ["a"], tab: "log" });
    ui.clusterFoldSave({ database: true });
  });
  assert.deepEqual(JSON.parse(stray.read("plurx.cluster.fold")), {
    database: true, nodes_closed: ["a"], tab: "log",
  });
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

// ---- the operations rail ---------------------------------------------------
// The rail's whole claim is that a refusal is readable before the click. These
// pin two halves of that: the precondition is stated, and the control is
// actually disabled when it does not hold — a row that says "Blocked" beside a
// live button is worse than no row at all.

function railRows(ui, cluster, ops) {
  return new Map(
    ui.clusterOperationRows(ui.env, cluster, ops).map((row) => [row.title.replace(/<[^>]*>/g, ""), row]),
  );
}

test("every action on the rail carries its precondition", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
    node("node-d", 4, "learner", { bounded_read_ready: true, voter_storage_ready: false,
      storage_headroom_bytes: 22 * 1000 * 1000 * 1000 }),
  ]);
  const rows = railRows(ui, cluster, operationStatus(cluster));
  assert.ok(rows.has("Add a node"));
  assert.equal(rows.get("Add a node").blocked, undefined);

  // The learner is caught up but its filesystem has not proved the reserve, so
  // the row says which of the two preflights failed and what to do about it —
  // before the promotion is attempted and refused.
  const promote = rows.get("Promote node-d to voter");
  assert.equal(promote.blocked, true);
  assert.match(promote.reason, /durable free-space reserve/);
  assert.match(promote.reason, /20 GB free/);
  assert.match(promote.action, /disabled/);

  // Leaving is always available and always permanent; it is never "ready".
  const leave = rows.get("Leave this cluster");
  assert.equal(leave.destructive, true);
  assert.match(leave.reason, /fresh data directory/);
});

test("a learner that has proved both preflights is offered promotion", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
    node("node-d", 4, "learner", { bounded_read_ready: true, voter_storage_ready: true }),
  ]);
  const promote = railRows(ui, cluster, operationStatus(cluster)).get("Promote node-d to voter");
  assert.equal(promote.blocked, false);
  assert.doesNotMatch(promote.action, /disabled/);
  assert.match(promote.action, /promoteNode\(&quot;node-d&quot;\)/);

  // Catch-up is the other half, and it names the badge to wait for.
  const catching = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
    node("node-d", 4, "learner", { bounded_read_ready: false, voter_storage_ready: true }),
  ]);
  const waiting = railRows(ui, catching, operationStatus(catching)).get("Promote node-d to voter");
  assert.equal(waiting.blocked, true);
  assert.match(waiting.reason, /zero-lag apply proof/);
  assert.match(waiting.action, /disabled/);
});

test("the rail never offers what the server would refuse", () => {
  const ui = sandbox();
  // Two voters: removal drops to one and is refused, and the rail says so
  // instead of leaving it to the failed attempt.
  const pair = status("degraded_reconfiguration", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
  ]);
  const two = railRows(ui, pair, operationStatus(pair)).get("Remove a node permanently");
  assert.equal(two.blocked, true);
  // The server's bar is three voters, not "not two": one voter is refused as
  // well, and so is a roster whose other members are still joining.
  assert.match(two.reason, /at least three voters and this cluster has 2/);

  // A membership change in flight locks the rest of the lifecycle.
  const fenced = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", { maintenance: true, maintenance_acknowledged: true }),
    node("node-c", 3, "voter"),
    node("node-d", 4, "learner", { bounded_read_ready: true, voter_storage_ready: true }),
  ]);
  const locked = railRows(ui, fenced, operationStatus(fenced));
  assert.equal(locked.get("Add a node").blocked, true);
  assert.match(locked.get("Add a node").reason, /already in flight on node-b/);
  assert.equal(locked.get("Promote node-d to voter").blocked, true);
  assert.equal(locked.get("Force a leader election").blocked, true);
  // The fenced node's own resume replaces the maintenance row, and it is only
  // offered from the node that owns the fence.
  assert.ok(!locked.has("Enter maintenance on node-a"));
  const resume = locked.get("Resume service on node-b");
  assert.equal(resume.blocked, true);
  assert.match(resume.reason, /Open node-b directly/);
});

test("the rail routes to the credential rather than minting a second one", () => {
  // A join token is shown exactly once. Two places that mint one is two places
  // to leak it, so these rows open the panel that owns it.
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const rows = railRows(ui, cluster, operationStatus(cluster));
  assert.match(rows.get("Add a node").action, /toggleClusterRailPanel\('add'\)/);
  assert.match(rows.get("Leave this cluster").action, /toggleClusterRailPanel\('leave'\)/);

  // The row that decides the change carries the control for it, and it is the
  // only place either one exists. Two surfaces that mint one credential is the
  // failure this panel must never ship.
  const html = ui.clusterPanel({ cluster, clusterOps: operationStatus(cluster), sys: { replication: REPLICATION } });
  assert.equal((html.match(/mintJoinToken\(this\)/g) || []).length, 1);
  assert.equal((html.match(/leaveCluster\(this,/g) || []).length, 1);
  const rail = html.slice(html.indexOf('<div class="clrail">'));
  assert.match(rail, /mintJoinToken\(this\)/);
  assert.match(rail, /leaveCluster\(this,/);
  assert.doesNotMatch(PANEL.clusterOperationRows.toString(), /localStorage/);
});

test("a stale roster cannot make the rail claim a verdict", () => {
  // With no direct status there is no restart verdict to report, so the row is
  // absent rather than guessing at safety.
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const rows = railRows(ui, cluster, null);
  assert.ok(!rows.has("Prepare this voter for restart"));
  // And maintenance is refused while the preflight has not answered.
  const maintain = rows.get("Enter maintenance on node-a");
  assert.equal(maintain.blocked, true);
  assert.match(maintain.reason, /preflight has not answered/);
});

// ---- the layout holding still ----------------------------------------------

test("the roster is a fixed-height scroller once there are nodes to fill it", () => {
  // Opening a node card used to move every section below it — measured at
  // 1259px on a four-node cluster. The list gets a fixed height so its own
  // size no longer depends on what is expanded; a maximum would still shrink
  // when everything is collapsed and move the page the other way.
  const ui = sandbox();
  const three = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  assert.match(ui.clusterPanel({ cluster: three, sys: { replication: REPLICATION } }),
    /class="clnodes clnodes-capped" id="cluster-node-list"/);

  // Two nodes cannot fill it, and an empty box below the last card is worse
  // than a list that grows a little.
  const pair = status("degraded_reconfiguration", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
  ]);
  assert.match(ui.clusterPanel({ cluster: pair, sys: { replication: REPLICATION } }),
    /class="clnodes" id="cluster-node-list"/);

  // The height and the scroller belong to the same rule, and it is scoped to
  // windows with room for it.
  assert.match(SHIPPED_UI, /@media\(min-width:1100px\) and \(min-height:720px\)\{[\s\S]*?\.clnodes\.clnodes-capped\{display:flex/);
  // `height`, not `max-height`: the substring would match either, and a
  // maximum is exactly the bug this rule replaced.
  // The box is the size of the list when every card is closed, capped by the
  // viewport — a fixed fraction of the window left a three-node roster with
  // 394px of nothing under the last card, which is the void this rule exists
  // to avoid. `height`, not `max-height`: a maximum shrinks when everything is
  // collapsed and moves the page the other way.
  assert.match(SHIPPED_UI, /\.clnodes\.clnodes-capped\{[^}]*[;\s]height:min\(var\(--clnodes-h[^}]*overflow-y:auto/);
  assert.doesNotMatch(SHIPPED_UI, /\.clnodes\.clnodes-capped\{[^}]*max-height:/);
  // And the viewport cap keeps a floor: a scroller shorter than a card is not
  // a list, it is a keyhole.
  assert.match(SHIPPED_UI, /\.clnodes\.clnodes-capped\{[^}]*max\(360px,calc\(100vh - 330px\)\)/);
  assert.match(SHIPPED_UI, /\.clnodes\.clnodes-capped>\.clnode\{[^}]*flex:0 0 auto/);
  const sized = ui.clusterPanel({ cluster: three, sys: { replication: REPLICATION } });
  assert.match(sized, /id="cluster-node-list" style="--clnodes-h:312px"/);
});

test("the rail's verdict reaches the markup, not just the model", () => {
  // The chips are the feature. Every precondition in clusterOperationRows can
  // be correct while clusterRailRow paints them all green, so pin the rendered
  // row rather than only the object behind it.
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
    node("node-d", 4, "learner", { bounded_read_ready: false, voter_storage_ready: false }),
  ]);
  const html = ui.clusterOperationsRail(cluster, operationStatus(cluster));
  const rowOf = (title) => {
    const at = html.indexOf(title);
    assert.notEqual(at, -1, `no row titled ${title}`);
    const start = html.lastIndexOf('<div class="clrailrow', at);
    return html.slice(start, html.indexOf('<div class="clrailrow', at + 1) === -1 ? undefined : html.indexOf('<div class="clrailrow', at + 1));
  };
  const promote = rowOf("Promote node-d to voter");
  assert.match(promote, /class="clrailrow blocked"/);
  assert.match(promote, />Blocked</);
  assert.doesNotMatch(promote, />Ready</);
  const add = rowOf("Add a node");
  assert.match(add, /class="clrailrow ready"/);
  assert.match(add, />Ready</);
  const leave = html.slice(html.indexOf("Leave this cluster"));
  assert.match(leave, />Permanent</);
  assert.match(html, /class="clrailrow destructive"/);
});

test("entering maintenance is offered only when the preflight approved it", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const approved = operationStatus(cluster);
  const ok = railRows(ui, cluster, approved).get("Enter maintenance on node-a");
  assert.equal(ok.blocked, false);
  assert.doesNotMatch(ok.action, /disabled/);

  // safe_to_enter is false for a dozen different reasons and the verdict ships
  // the one it hit. Substituting a plausible reason — quorum, say — sends the
  // operator after a voter that is not missing.
  const refused = operationStatus(cluster);
  refused.maintenance = refused.maintenance.map((entry) => ({
    ...entry,
    safe_to_enter: false,
    blockers: [{ code: "restart_preparation_active", node_id: "node-b", message: "" }],
  }));
  const no = railRows(ui, cluster, refused).get("Enter maintenance on node-a");
  assert.equal(no.blocked, true);
  assert.match(no.reason, /restart preparation active on node-b/);
  assert.doesNotMatch(no.reason, /quorum/);
  assert.match(no.action, /disabled/);
});

test("restart preparation is offered only on the node the server named", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  // operationStatus names nodes[1] as the candidate, and the local node is
  // nodes[0]: preparing here is refused as a candidate mismatch.
  const elsewhere = railRows(ui, cluster, operationStatus(cluster)).get("Prepare this voter for restart");
  assert.equal(elsewhere.blocked, true);
  assert.match(elsewhere.reason, /candidate is node-b/);
  assert.match(elsewhere.action, /disabled/);

  const here = operationStatus(cluster);
  here.verdict.candidate_node_id = cluster.local_node_id;
  const mine = railRows(ui, cluster, here).get("Prepare this voter for restart");
  assert.equal(mine.blocked, false);
  assert.match(mine.action, /prepareLocalRestart/);
  assert.doesNotMatch(mine.action, /disabled/);
});

test("an election is offered only with a quorum and a caught-up follower", () => {
  const ui = sandbox();
  const base = () => [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ];
  const healthy = status("high_availability", base());
  assert.equal(railRows(ui, healthy, operationStatus(healthy)).get("Force a leader election").blocked, false);

  // No follower is both reachable and fully applied, so nobody can campaign.
  const behind = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", { apply_lag_entries: 91 }),
    node("node-c", 3, "voter", { reachable: false }),
  ]);
  const stale = railRows(ui, behind, operationStatus(behind)).get("Force a leader election");
  assert.equal(stale.blocked, true);
  assert.match(stale.reason, /caught-up follower/);

  // And an election cannot manufacture the majority it needs.
  const lost = status("high_availability", base());
  lost.recovery = { required: true, quorum_available: false, reachable_voters: 1, required_voters: 2, leader_elected: false };
  const noQuorum = railRows(ui, lost, operationStatus(lost)).get("Force a leader election");
  assert.equal(noQuorum.blocked, true);
  assert.match(noQuorum.reason, /cannot bypass quorum/);
});

test("recovery locks the membership changes it cannot commit", () => {
  // The banner above the rail says membership changes are locked; the rows
  // underneath it must not read Ready.
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", { reachable: false }),
    node("node-c", 3, "voter", { reachable: false }),
  ]);
  cluster.recovery = { required: true, quorum_available: false, reachable_voters: 1, required_voters: 2, leader_elected: false };
  const rows = railRows(ui, cluster, operationStatus(cluster));
  for (const title of ["Add a node", "Remove a node permanently"]) {
    assert.equal(rows.get(title).blocked, true, `${title} still reads ready under recovery`);
    assert.match(rows.get(title).reason, /elected leader and a reachable voter majority/);
  }
});

test("a learner can be removed from a cluster too small to lose a voter", () => {
  // The server holds voter removal to three voters and sends a learner down a
  // different path, so blocking both would refuse something that works.
  const ui = sandbox();
  const small = status("degraded_reconfiguration", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "learner", { bounded_read_ready: true }),
  ]);
  const rows = railRows(ui, small, operationStatus(small));
  assert.equal(rows.get("Remove a node permanently").blocked, true);
  const learnerOnly = status("degraded_reconfiguration", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-c", 3, "learner", { bounded_read_ready: true }),
  ]);
  const only = railRows(ui, learnerOnly, operationStatus(learnerOnly)).get("Remove a node permanently");
  assert.equal(only.blocked, false);
  // The same roster read from the learner: now a voter is the removable one,
  // and one voter is below the server's bar just as two is.
  const fromLearner = status("degraded_reconfiguration", [
    node("node-c", 3, "learner", { bounded_read_ready: true }),
    node("node-a", 1, "voter", { is_leader: true }),
  ]);
  const single = railRows(ui, fromLearner, operationStatus(fromLearner)).get("Remove a node permanently");
  assert.equal(single.blocked, true);
  assert.match(single.reason, /at least three voters and this cluster has 1/);
  // And the leader's own removal is refused from anywhere but its own screen.
  const remote = status("high_availability", [
    node("node-a", 1, "voter"),
    node("node-b", 2, "voter", { is_leader: true }),
    node("node-c", 3, "voter"),
  ]);
  assert.match(railRows(ui, remote, operationStatus(remote)).get("Remove a node permanently").reason,
    /node-b is the current leader and can only leave from its own screen/);
});

test("the two components stay in their own columns, and the rail with the cluster", () => {
  // The layout is the deliverable here: asserting the headings exist would
  // pass with every card back in one flow, which is what this replaced.
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterPanel({ cluster, clusterOps: operationStatus(cluster), sys: { replication: REPLICATION } });
  const columns = html.split('<div class="clcolumn">');
  assert.equal(columns.length, 3, "expected exactly two columns in the board");
  const [, left, right] = columns;
  assert.match(left, /<h3>Replicated database<\/h3>/);
  assert.match(left, /<h3>Maintenance<\/h3>/);
  assert.match(left, /class="clrail"/);
  assert.match(left, /id="clrail-add"/);
  assert.doesNotMatch(left, /id="cluster-node-list"/);
  assert.match(right, /<h3>Cluster nodes<\/h3>/);
  assert.match(right, /id="cluster-node-list"/);
  // The restart verdict names one of these machines, so it sits under them.
  assert.match(right, /id="cluster-operations"/);
  assert.doesNotMatch(left, /id="cluster-operations"/);
  // One election dialog, once: the recovery panel ships its own.
  assert.equal((html.match(/id="clelection"/g) || []).length, 1);
  const recovering = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", { reachable: false }),
    node("node-c", 3, "voter", { reachable: false }),
  ]);
  recovering.recovery = { required: true, quorum_available: true, reachable_voters: 2, required_voters: 2, leader_elected: false };
  const lost = ui.clusterPanel({ cluster: recovering, sys: { replication: REPLICATION } });
  assert.equal((lost.match(/id="clelection"/g) || []).length, 1);
});

test("the controls the rail routes to actually do something", () => {
  // Both of these are one line each and both were deletable with the suite
  // green: the credential panel would never open, and a card opened near the
  // bottom of the scroller would open below the fold.
  assert.match(shippedSource("toggleClusterRailPanel"), /scrollIntoView\(\{block:"nearest"\}\)/);
  assert.match(shippedSource("clusterNodeFoldLater"), /scrollIntoView\(\{block:"nearest"\}\)/);
});

// ---- one credential surface ------------------------------------------------

test("the credential is an expansion of the row that decides it", () => {
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const ops = operationStatus(cluster);
  const collapsed = sandbox().clusterOperationsRail(cluster, ops);
  // Shipped state: the panel exists in the markup and is not showing. A fresh
  // page must never open a credential surface by itself.
  assert.match(collapsed, /<div class="clrailpanel" id="clrail-add" hidden>/);
  assert.match(collapsed, /aria-controls="clrail-add" aria-expanded="false"/);
  assert.match(collapsed, /<div class="clrailpanel" id="clrail-leave" hidden>/);

  const open = sandbox({ expanded: ["add"] }).clusterOperationsRail(cluster, ops);
  assert.match(open, /<div class="clrailpanel" id="clrail-add">/);
  assert.match(open, /aria-controls="clrail-add" aria-expanded="true"/);
  assert.match(open, />Hide</);
  // Opening one does not open the other.
  assert.match(open, /<div class="clrailpanel" id="clrail-leave" hidden>/);
});

test("the token survives a repaint and never reaches storage", () => {
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const ui = sandbox({
    expanded: ["add"],
    token: { token: "plxjoin:v1:aaaa:bbbb", expires_at: Date.now() + 600_000, raft_id: 4, role: "voter" },
  });
  const rail = ui.clusterOperationsRail(cluster, operationStatus(cluster));
  // Rendered from state rather than captured from the DOM, so the 15s status
  // repaint cannot pull a half-read credential shut underneath someone.
  assert.match(rail, /plxjoin:v1:aaaa:bbbb/);
  assert.equal((rail.match(/plxjoin:v1:aaaa:bbbb/g) || []).length, 1, "shown once means once");
  assert.match(rail, /Shown once/);

  // The expansion is module state, and the same three exits that drop the token
  // drop it too — so returning to this tab starts collapsed, with nothing to
  // reopen onto.
  const forget = shippedSource("forgetJoinToken");
  assert.match(forget, /CLUSTER_TOKEN=null/);
  assert.match(forget, /CLUSTER_RAIL_EXPANDED=\[\]/);
  const toggle = shippedSource("toggleClusterRailPanel");
  assert.doesNotMatch(toggle, /forgetJoinToken|CLUSTER_TOKEN/);
  for (const sink of ["localStorage", "sessionStorage", "clusterFoldSave"]) {
    assert.equal(toggle.includes(sink), false, `the rail expansion reaches ${sink}`);
  }
  // The fold's whitelist is still exactly the fold: nothing credential-adjacent
  // can ride into storage on it.
  const read = PANEL.clusterFoldRead.toString();
  for (const key of ["database", "nodes_closed", "tab"]) assert.ok(read.includes(key));
  assert.doesNotMatch(read, /rail|expand|token/i);
});

test("the expansion is a toggle over module state, and writes nothing down", () => {
  // The fold's read side is a whitelist, but its write side Object.assigns any
  // key it is handed — so the guarantee that a credential surface never reaches
  // storage has to be asserted against the storage, not against a grep.
  const storage = fakeStorage({ "plurx.cluster.fold": JSON.stringify({ database: true }) });
  const panel = { hidden: true, scrollIntoView() {} };
  const document = { getElementById: (id) => (id === "clrail-add" ? panel : null) };
  const harness = new Function(
    "document", "renderSettings", "repaintClusterPreserving", "localStorage",
    `let CLUSTER_RAIL_EXPANDED=[];
     ${shippedSource("toggleClusterRailPanel")}
     return {toggle:toggleClusterRailPanel,state:()=>CLUSTER_RAIL_EXPANDED};`,
  )(document, () => {}, (paint) => paint(), storage);

  harness.toggle("add");
  assert.deepEqual(harness.state(), ["add"]);
  harness.toggle("add");
  assert.deepEqual(harness.state(), [], "toggling twice closes it again");
  assert.deepEqual(
    JSON.parse(storage.read("plurx.cluster.fold")),
    { database: true },
    "the rail expansion left nothing behind in browser storage",
  );
});

test("both membership rows carry an evaluated precondition", () => {
  // Leave is a membership change like any other: it commits through the same
  // quorum and cannot start while another lifecycle change is in flight. It was
  // the one row on a card headed "with its precondition already evaluated" that
  // had none.
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const ui = sandbox({ expanded: ["leave"] });
  const open = ui.clusterOperationsRail(cluster, operationStatus(cluster));
  assert.match(open, /<div class="clrailpanel" id="clrail-leave">/, "leave expands too, not just add");
  assert.match(open, /leaveCluster\(this,/);

  const recovering = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", { reachable: false }),
    node("node-c", 3, "voter", { reachable: false }),
  ]);
  recovering.recovery = { required: true, quorum_available: false, reachable_voters: 1, required_voters: 2, leader_elected: false };
  const locked = railRows(ui, recovering, operationStatus(recovering)).get("Leave this cluster");
  assert.equal(locked.blocked, true);
  assert.match(locked.reason, /elected leader and a reachable voter majority/);
  const lockedHtml = sandbox({ expanded: ["leave"] }).clusterOperationsRail(recovering, operationStatus(recovering));
  assert.equal(lockedHtml.includes('id="clrail-leave"'), false, "a blocked row has no reachable control");
  assert.equal(lockedHtml.includes("leaveCluster(this,"), false);

  // A fenced node is a lifecycle change in flight, and the leave control the
  // rail mounts still receives that state, so its own button stays disabled if
  // the row is ever reached.
  assert.match(shippedSource("clusterOperationsRail"), /leavePanel\(cluster\.local_node_id,maintenanceActive\)/);
  assert.match(shippedSource("clusterOperationsRail"), /joinPanel\(maintenanceActive\)/);
});

test("the danger zone is gone, not renamed", () => {
  for (const trace of ["cldanger", "cldangerbody", "openClusterDanger", "Danger zone"]) {
    assert.equal(SHIPPED_UI.includes(trace), false, `index.html still ships ${trace}`);
  }
  // Leave keeps its friction and its binding: the graceful-leave wording, the
  // roster-bound node_id, and the destructive treatment all moved as they were.
  const leave = sandbox().leavePanel("node-a");
  assert.match(leave, /leaveCluster\(this,&quot;node-a&quot;\)/);
  assert.match(leave, /permanent, not an update or restart action/);
  assert.match(leave, /fresh data directory/);
});

// ---- the direct status keeps itself fresh ----------------------------------

test("the projection ignores what moves on its own and nothing else", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const first = operationStatus(cluster);
  const later = JSON.parse(JSON.stringify(first));
  // Everything here is the passage of time, not news.
  later.observed_at_unix_ms = (first.observed_at_unix_ms || 0) + 60_000;
  later.nodes.forEach((row) => {
    row.sample_age_ms = (row.sample_age_ms || 0) + 60_000;
    if (row.membership) row.membership.last_seen_at = (row.membership.last_seen_at || 0) + 60_000;
    if (row.status) {
      row.status.observed_at_unix_ms = (row.status.observed_at_unix_ms || 0) + 60_000;
      row.status.raft.sample_age_seconds = (row.status.raft.sample_age_seconds || 0) + 60;
      row.status.raft.watermark_age_millis = (row.status.raft.watermark_age_millis || 0) + 60_000;
    }
  });
  assert.equal(
    ui.clusterOpsProjection(later),
    ui.clusterOpsProjection(first),
    "an aggregate that only got older must not repaint the panel",
  );

  // The commit watermark advancing is exactly what an operator leaves this
  // ledger open to watch, so it is news.
  const advanced = JSON.parse(JSON.stringify(later));
  advanced.nodes[0].status.raft.commit_index += 1;
  assert.notEqual(ui.clusterOpsProjection(advanced), ui.clusterOpsProjection(first));
  const fenced = JSON.parse(JSON.stringify(later));
  fenced.verdict.safe_to_restart_one = !fenced.verdict.safe_to_restart_one;
  assert.notEqual(ui.clusterOpsProjection(fenced), ui.clusterOpsProjection(first));
  // Key order in the response is not a change either.
  const shuffled = JSON.parse(JSON.stringify(first));
  shuffled.nodes = shuffled.nodes.map((row) =>
    Object.fromEntries(Object.entries(row).reverse()),
  );
  assert.equal(ui.clusterOpsProjection(shuffled), ui.clusterOpsProjection(first));
});

// A settingsTick harness that can actually run the cluster branch: the tick
// itself is shipped source, everything it reaches for is supplied here.
function tickHarness({ cluster, ops, now }) {
  const requests = [];
  const painted = [];
  const readingAge = { innerHTML: "local now · watermark now" };
  const dialogs = [];
  const document = {
    visibilityState: "visible",
    getElementById: (id) => (id === "cldb-reading-age" ? readingAge : null),
    querySelectorAll: () => [],
    querySelector: (selector) =>
      selector.includes("dialog[open]") ? dialogs.find((open) => open) || null : null,
  };
  const harness = new Function(
    "document", "location", "api", "settingsTab", "settingsCurrent", "refreshLogs",
    "refreshClusterLogs", "paintTrakt", "renderSettings", "PlurxClusterPanel", "clock", "dialogs",
    `let PAGE_RENDER_GENERATION=1,AUTH_GENERATION=1,SETTINGS_TICKING=null,TRAKT_EDIT=false,
       TRAKT=null,CLUSTER_LOADED=true,CLUSTER_OPS_FETCHED_AT=0,
       SETTINGS_DATA=${JSON.stringify({ cluster, clusterOps: ops })},SETTINGS_LOADED=new Set(["cluster","clusterOps"]);
     // What the tab painted when it opened, exactly as patchSettingsSecondary
     // records it.
     let CLUSTER_OPS_PAINTED=PlurxClusterPanel.clusterOpsProjection(SETTINGS_DATA.clusterOps);
     const cacheTrakt=(value)=>value;
     const Date={now:clock};
     ${shippedSource("clusterOpsInterval")}
     ${shippedSource("clusterOpsStamp")}
     ${shippedSource("clusterOpsDue")}
     ${shippedSource("clusterOpsPainted")}
     ${shippedSource("clusterOpsChanged")}
     ${shippedSource("clusterRepaintDeferred")}
     ${shippedSource("repaintClusterPreserving")}
     ${shippedSource("clusterControlValues")}
     ${shippedSource("clusterRestoreControlValues")}
     ${shippedSource("clusterOpenDetailNodes")}
     ${shippedSource("patchClusterReadingAge")}
     ${shippedSource("clenv")}
     ${shippedSource("esc")}
     ${shippedSource("fmtAgo")}
     ${shippedSource("fmtBytes")}
     ${shippedSource("settingsTick")}
     return {settingsTick,stamped:()=>CLUSTER_OPS_FETCHED_AT,ops:()=>SETTINGS_DATA.clusterOps,
       openDialog:(open)=>{dialogs.length=0;if(open)dialogs.push({});}};`,
  )(
    document,
    { hash: "#/settings" },
    (url) => new Promise((resolve, reject) => requests.push({ url, resolve, reject })),
    () => "cluster",
    () => true,
    async () => {},
    async () => {},
    () => {},
    () => painted.push("render"),
    PANEL,
    () => now.value,
    dialogs,
  );
  return { harness, requests, painted, readingAge, dialogs };
}

test("the direct status collects on its own gate, and never overlaps", async () => {
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const ops = operationStatus(cluster);
  const now = { value: 1_000_000 };
  const { harness, requests } = tickHarness({ cluster, ops, now });

  // Fifteen seconds since the tab painted, so this tick collects — and stamps
  // the clock BEFORE the await, at request time rather than at completion.
  now.value += 15_000;
  const first = harness.settingsTick(1, "cluster");
  assert.deepEqual(requests.map((r) => r.url), ["/cluster/status"]);
  assert.equal(harness.stamped(), now.value, "the clock is stamped before the await");

  // A probe slower than the gate must not put a second fan-out behind itself.
  now.value += 20_000;
  await harness.settingsTick(1, "cluster");
  assert.equal(requests.length, 1, "a slow probe is never overlapped");
  requests[0].resolve(ops);
  await first;

  // …and because the clock was stamped at request time, a probe that took
  // twenty seconds does not also push the next collection twenty seconds out.
  const prompt = harness.settingsTick(1, "cluster");
  assert.equal(requests.length, 2, "a slow probe does not delay the next one by its own duration");
  requests[1].resolve(ops);
  await prompt;

  now.value += 9_000;
  await harness.settingsTick(1, "cluster");
  assert.equal(requests.length, 2, "the 2s tick does not become a 2s fan-out");
  now.value += 7_000;
  const dueAgain = harness.settingsTick(1, "cluster");
  assert.equal(requests.length, 3, "fifteen seconds later it collects again");
  requests[2].resolve(ops);
  await dueAgain;
});

test("the gate is fifteen seconds, and a single machine is never polled", async () => {
  // The interval is a measured trade-off, not an incidental number: the fan-out
  // probes every voter, and the flow that needs 2s freshness has its own poll.
  assert.match(shippedSource("clusterOpsInterval"), /return 15000;/);

  // The overwhelmingly common install is one SQLite box. It renders no rail, no
  // roster and no direct readings, so collecting a fan-out for it would be a
  // request spent to learn nothing.
  const solo = { unavailable: true, code: "membership_unavailable" };
  const now = { value: 1_000_000 };
  const { harness, requests } = tickHarness({ cluster: solo, ops: undefined, now });
  now.value += 60_000;
  await harness.settingsTick(1, "cluster");
  assert.deepEqual(requests, [], "a non-clustered install polls nothing");
});

test("a refused collection still moves the freshness row", async () => {
  // A failing collection is exactly when the age of the reading on screen
  // matters most, so the row whose only job is freshness must not be the one
  // that freezes.
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const now = { value: 1_000_000 };
  const { harness, requests, painted, readingAge } = tickHarness({
    cluster, ops: operationStatus(cluster), now,
  });
  readingAge.innerHTML = "frozen";
  now.value += 15_000;
  const refused = harness.settingsTick(1, "cluster");
  requests[0].reject(Object.assign(new Error("gateway"), { status: 502 }));
  await refused;
  assert.notEqual(readingAge.innerHTML, "frozen");
  assert.deepEqual(painted, [], "a refusal does not repaint the tab");
  assert.equal(harness.stamped(), now.value, "the retry is the next gate, not the next tick");
});

test("a decision in progress is never repainted out from under the operator", async () => {
  // renderSettings() rewrites the whole tab, and the force-election dialog lives
  // in that markup. A modal is a decision in progress; the repaint waits.
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const ops = operationStatus(cluster);
  const now = { value: 1_000_000 };
  const { harness, requests, painted } = tickHarness({ cluster, ops, now });
  const changed = JSON.parse(JSON.stringify(ops));
  changed.verdict.safe_to_restart_one = !changed.verdict.safe_to_restart_one;

  harness.openDialog(true);
  now.value += 15_000;
  const held = harness.settingsTick(1, "cluster");
  requests[0].resolve(changed);
  await held;
  assert.deepEqual(painted, [], "the dialog was still open");

  // Deferred, not dropped: the next collection after it closes pays the repaint,
  // even though the payload has not changed again since.
  harness.openDialog(false);
  now.value += 15_000;
  const paid = harness.settingsTick(1, "cluster");
  requests[1].resolve(changed);
  await paid;
  assert.deepEqual(painted, ["render"], "the owed repaint landed once the modal closed");
});

test("a fetch is not a repaint, and the freshness row still ages", async () => {
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const ops = operationStatus(cluster);
  const now = { value: 1_000_000 };
  const { harness, requests, painted, readingAge } = tickHarness({ cluster, ops, now });

  const older = JSON.parse(JSON.stringify(ops));
  older.observed_at_unix_ms = (older.observed_at_unix_ms || 0) + 15_000;
  older.nodes.forEach((row) => {
    row.sample_age_ms = (row.sample_age_ms || 0) + 15_000;
  });
  readingAge.innerHTML = "stale text";
  const quiet = harness.settingsTick(1, "cluster");
  requests[0].resolve(older);
  await quiet;
  assert.deepEqual(painted, [], "nothing changed, so nothing was rewritten");
  assert.notEqual(readingAge.innerHTML, "stale text", "the freshness row was patched in place");
  assert.equal(harness.ops(), older, "the newer sample is still what the panel reads");

  now.value += 16_000;
  const changed = JSON.parse(JSON.stringify(older));
  changed.verdict.safe_to_restart_one = !changed.verdict.safe_to_restart_one;
  const loud = harness.settingsTick(1, "cluster");
  requests[1].resolve(changed);
  await loud;
  assert.deepEqual(painted, ["render"], "a changed verdict repaints exactly once");
});

test("every repaint on this tab preserves what the operator was looking at", () => {
  // Three paths repaint the Cluster tab and all three must behave the same,
  // because a 15s cadence turns "the scroll jumps" from a papercut into an
  // unusable panel. Pin the call sites: a helper nothing calls is the failure
  // mode this suite has already caught once.
  assert.match(shippedSource("settingsTick"), /repaintClusterPreserving\(renderSettings\)/);
  assert.match(shippedSource("refreshClusterOperations"), /repaintClusterPreserving\(renderSettings\)/);
  assert.match(shippedSource("pollLocalRestart"), /repaintClusterPreserving\(renderSettings\)/);
  // …and the restart poll stamps the same clock, so the two never probe at once.
  assert.match(shippedSource("pollLocalRestart"), /clusterOpsStamp\(\)/);

  const detail = (open) => ({ open });
  const bodies = [
    { node: "node-a", detail: detail(true) },
    { node: "node-b", detail: detail(false) },
  ].map((entry) => ({
    getAttribute: () => entry.node,
    querySelector: () => entry.detail,
    detail: entry.detail,
  }));
  const list = { scrollTop: 420 };
  const ttl = { id: "clttl", value: "3600" };
  const role = { id: "clrole", value: "learner" };
  const auto = { id: "cllogauto", type: "checkbox", checked: false, value: "on" };
  const controls = { clttl: ttl, clrole: role, cllogauto: auto };
  const document = {
    getElementById: (id) => (id === "cluster-node-list" ? list : controls[id] || null),
    querySelectorAll: (selector) => (selector.includes("clnodebody") ? bodies : [ttl, role, auto]),
  };
  const repaint = new Function(
    "document",
    `${shippedSource("clusterOpenDetailNodes")}
     ${shippedSource("clusterControlValues")}
     ${shippedSource("clusterRestoreControlValues")}
     ${shippedSource("repaintClusterPreserving")}
     return repaintClusterPreserving;`,
  )(document);

  repaint(() => {
    // renderSettings() rewrites the markup: a fresh list at the top, every
    // drill-down back to its shipped default, and every control back to the
    // value in the template.
    list.scrollTop = 0;
    bodies.forEach((body) => {
      body.detail.open = false;
    });
    ttl.value = "600";
    role.value = "voter";
    auto.checked = true;
  });
  assert.equal(list.scrollTop, 420, "the roster scroller kept its place");
  // A repaint between choosing a token's lifetime and clicking Create must not
  // silently mint a ten-minute voter token instead of the hour-long read worker
  // that was selected.
  assert.equal(ttl.value, "3600");
  assert.equal(role.value, "learner");
  assert.equal(auto.checked, false);
  assert.deepEqual(
    bodies.map((body) => body.detail.open),
    [true, false],
    "the open drill-down came back and the closed one stayed closed",
  );
});

// ---- the module boundary ---------------------------------------------------

test("the ledger's freshness cell is addressable, and the clock is stamped where the request is", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  // patchClusterReadingAge finds this cell by id; without the id in the rendered
  // markup the patch is a silent no-op and the row freezes.
  const rows = PANEL.clusterDatabaseRows(ui.env, cluster, REPLICATION, operationStatus(cluster));
  const age = rows.find((row) => row[0] === "Reading age");
  assert.equal(age[3], "cldb-reading-age");
  const panel = ui.clusterDatabasePanel(cluster, REPLICATION, cluster.capacity, operationStatus(cluster));
  assert.match(panel, /<dd id="cldb-reading-age">/);
  assert.match(shippedSource("patchClusterReadingAge"), /getElementById\("cldb-reading-age"\)/);
  assert.match(shippedSource("patchClusterReadingAge"), /clusterReadingAge\(clenv\(\)/);

  // Stamped where the request happens. loadSettingsKey serves a cached
  // aggregate without a request, so stamping where the value is PAINTED would
  // let a tab switch every ten seconds starve the refresh indefinitely.
  assert.match(SHIPPED_UI, /clusterOps:\(\)=>api\("\/cluster\/status"\)\.then\(ops=>\{ clusterOpsStamp\(\); return ops; \}\)/);
  assert.doesNotMatch(shippedSource("patchSettingsSecondary"), /clusterOpsStamp\(\)/);
  assert.match(shippedSource("patchSettingsSecondary"), /clusterOpsPainted\(SETTINGS_DATA\.clusterOps\)/);
});

test("every path that rewrites this tab goes through the preserving repaint", () => {
  // A bare renderSettings() on the Cluster tab throws away the roster scroll,
  // the open drill-downs, and the half-filled token form. Both branches of the
  // tick, the manual refresh, the restart poll, and the three controls that
  // repaint from inside the rail all use the helper.
  const tick = shippedSource("settingsTick");
  assert.equal(
    (tick.match(/repaintClusterPreserving\(renderSettings\)/g) || []).length,
    2,
    "both the roster branch and the direct-status branch preserve",
  );
  for (const handler of [
    "refreshClusterOperations",
    "pollLocalRestart",
    "toggleClusterRailPanel",
    "mintJoinToken",
    "clearJoinToken",
  ]) {
    assert.match(
      shippedSource(handler),
      /repaintClusterPreserving\(renderSettings\)/,
      `${handler} repaints without preserving what the operator was looking at`,
    );
  }
});

test("the model is a file, and the shell actually mounts it", () => {
  // A module nobody calls is the dead-feature failure mode this suite already
  // paid for once, so the boundary is pinned from both sides: the shell loads
  // the served file before its own inline script, and the call sites that read
  // the model name it.
  const tag = SHIPPED_UI.indexOf('<script src="/assets/cluster-panel.js">');
  assert.notEqual(tag, -1, "index.html does not load the served model");
  assert.ok(tag < SHIPPED_UI.indexOf("\nfunction clusterPanel("));
  assert.match(shippedSource("clusterOperationsRail"), /PlurxClusterPanel\.clusterOperationRows\(clenv\(\),/);
  assert.match(shippedSource("clusterDatabasePanel"), /PlurxClusterPanel\.clusterDatabaseRows\(clenv\(\),/);
  assert.match(shippedSource("clusterPanel"), /PlurxClusterPanel\.clusterStateView\(/);
  assert.match(shippedSource("clusterRefusalHtml"), /PlurxClusterPanel\.membershipRefusalText\(/);
});

test("the model reaches for nothing the shell owns", () => {
  // The whole value of the boundary is that these decisions load under Node
  // with no setup: no document, no localStorage, no SETTINGS_DATA, no ME, no
  // CLUSTER_* state. A helper it needs arrives as an argument.
  // Comments discuss the DOM the module deliberately does not touch, so the
  // check reads the code and not the prose around it.
  const source = fs
    .readFileSync(path.join(__dirname, "../../crates/plurxd/src/web/cluster-panel.js"), "utf8")
    .split("\n")
    .filter((line) => !/^\s*\/\//.test(line))
    .join("\n");
  for (const forbidden of [/\bdocument\b/, /\blocalStorage\b/, /\bSETTINGS_[A-Z]/, /\bCLUSTER_[A-Z]/, /\bME\b/]) {
    assert.doesNotMatch(source, forbidden, `cluster-panel.js reaches for ${forbidden}`);
  }
  // One esc, one escaping contract to review.
  assert.doesNotMatch(source, /function esc\(/);
  assert.match(shippedSource("clenv"), /return \{esc,fmtAgo,fmtBytes\};/);
});

process.on("beforeExit", () => {
  if (failures) process.exitCode = 1;
});
