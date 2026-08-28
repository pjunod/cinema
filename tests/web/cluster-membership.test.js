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
  "clusterDirectMaintenanceStatus",
  "clusterMaintenanceReady",
  "clusterMaintenanceResumeReady",
  "clusterNodeRow",
  "clusterOperationAge",
  "clusterOperationReason",
  "clusterOperationsUnavailable",
  "clusterOperationsNodeRow",
  "clusterRestartPreparationHtml",
  "clusterOperationsCard",
  "clusterRecoveryState",
  "clusterRecoveryPanel",
  "clusterMaintenanceProgress",
  "clusterOperationsPanel",
  "forceElectionDialog",
  "clusterRefusalHtml",
  "joinPanel",
  "leavePanel",
  "joinTokenHtml",
  "clusterPanel",
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
    /disabled title="Operations has not selected this node as the safe restart candidate/,
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
  const html = ui.clusterOperationsCard(operationStatus(membership));
  assert.match(html, /Ready to restart one voter/);
  assert.match(html, /3 voters · majority 2 · 3 ready/);
  assert.match(html, /Follower · term 81 · lag 0/);
  assert.match(html, /open.*3 segments.*6\.0 MB.*durable 482191/s);
  assert.match(html, /Unknown here — drain proxy connections separately/);
  assert.match(html, /v0\.2\.7-operations/);
});

test("an unreachable voter is written as a blocker, never a healthy row", () => {
  const ui = sandbox();
  const membership = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterOperationsCard(
    operationStatus(membership, { safe: false, unreachable: true }),
  );
  assert.match(html, /Do not restart another voter/);
  assert.match(html, /voter not observed/);
  assert.match(html, /Not observed · unreachable/);
  const nodeBAt = html.indexOf('<div class="clhost">node-b');
  const nodeBRow = html.slice(
    html.lastIndexOf("<tr>", nodeBAt),
    html.indexOf("</tr>", nodeBAt) + 5,
  );
  assert.doesNotMatch(nodeBRow, /Ready/);
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
  assert.match(electionHtml, /term disagreement/);
  assert.match(electionHtml, /stale or incomplete proof/);
  assert.match(electionHtml, /leader term disagreement/);

  const fenced = operationStatus(membership, { safe: false });
  fenced.nodes[1].status.serving = { ready: false, reason: "quorum_stale" };
  fenced.verdict.blockers = [{
    code: "voter_not_ready",
    node_id: "node-b",
    message: "a voter is fenced from serving new work",
  }];
  const fencedHtml = ui.clusterOperationsCard(fenced);
  assert.match(fencedHtml, /Fenced: quorum stale/);
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
  assert.match(skewHtml, /2 observed builds/);
  assert.match(skewHtml, /mixed builds/);
  assert.match(skewHtml, /v0\.2\.8-next/);

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
  assert.match(walHtml, /wal not healthy.*node-b/s);
  assert.match(walHtml, /durability proof failed/);
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
  assert.match(staleHtml, /Not observed · stale peer sample/);
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
  assert.match(html, /1 ready non-voting read worker/);
  assert.match(html, /1 non-voting replicated copy/);
  assert.match(html, /3 voters, quorum 2, tolerates 1 voter failure/);
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

// All four exist in crates/plurx-core/src/cluster/membership.rs today.
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
  assert.match(row, /<article class="clnode/);
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

test("the healthy cluster layout separates health, nodes, operations, and danger", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterPanel({ cluster, sys: { replication: REPLICATION } });
  assert.match(html, /class="cluster-health"/);
  assert.match(html, /id="cluster-operations"/);
  assert.match(html, /<h3>Nodes<\/h3>/);
  assert.match(html, /<h3>Maintenance &amp; leadership<\/h3>/);
  assert.match(html, /Enter maintenance/);
  assert.match(html, /Force election/);
  assert.match(html, /Danger zone · membership changes/);
  assert.equal(html.includes("<table"), false);
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

process.exit(failures ? 1 : 0);
