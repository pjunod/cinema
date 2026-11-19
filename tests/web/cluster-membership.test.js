"use strict";

// The admin cluster-membership panel, tested against the shipped index.html
// rather than against a copy of it.
//
// What this file is actually protecting: the WORDS. The membership API already
// has its own Rust gate for lifecycle, admin gating, and refusal codes. The
// thing only this surface can get wrong is telling an operator that two voters
// are redundancy — docs/cluster/CLUSTERING-PLAN.md §7.2 makes two-node HA a stated
// non-goal precisely because two voters need both machines for every write and
// therefore survive no failure. A settings screen that renders that as a green
// "highly available" is the single most damaging bug this panel can ship, and
// no assertion on the API field would catch it. So the assertions below read
// the rendered sentences.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const {shellSource} = require("./shell-source.js");
// The shipped app is a tree now, so the string these assertions slice is
// the app's body rows, joined in served order. See tests/web/shell-source.js.
// These assertions read across all three halves of the shipped app — the
// markup, the stylesheet and the body rows — so they get the whole of it.
const SHIPPED_UI = shellSource().everything;

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
  "clusterOpsElapsed",
  "clusterTone",
  "clusterNodeLedger",
  "clusterNodeDrawer",
  "clusterNodeActionsHtml",
  "clusterNodeTableRow",
  "clusterNodesCard",
  "clusterOperationsUnavailable",
  "clusterRestartPreparationHtml",
  "clusterRecoveryAlert",
  "clusterMaintenanceProgress",
  "clusterMaintenanceAlert",
  "clusterMaintenanceCard",
  "forceElectionDialog",
  "clusterRefusalHtml",
  "clusterDatabasePanel",
  "clusterStrip",
  "clusterAlerts",
  "clusterLogCard",
  "joinPanel",
  "leavePanel",
  "joinTokenHtml",
  "clusterPanel",
  "settingsClusterEnabled",
  "setHead",
  "clusterBackupCard",
  "clusterSettingRows",
  "clusterBackupChecks",
  "clusterSettingChecks",
  "clusterChecksChip",
  "clusterChecksMore",
  "clusterChecksHtml",
  "clusterSettingRow",
  "setCard",
  "cardHead",
  "devReq",
  "devReadinessRow",
  "devReadinessPill",
  "devReadinessEvidence",
  "promoteNode",
  "clusterSettingsSection",
];

// One sandbox per test so a mutation of ME or CLUSTER_REFUSAL cannot leak into
// the next assertion.
function sandbox({ isAdmin = true, refusal = null, token = null, expanded = [], open = [], clustered } = {}) {
  const source = `
    const CLUSTER_OPS_RECEIVED_AT = new WeakMap();
    const SERVER={cluster_enabled:${JSON.stringify(clustered)}},SETTINGS_DATA={};
    let ME = ${JSON.stringify({ is_admin: isAdmin })};
    let CLUSTER_REFUSAL = ${JSON.stringify(refusal)};
    let CLUSTER_TOKEN = ${JSON.stringify(token)};
    let CLUSTER_RAIL_EXPANDED = ${JSON.stringify(expanded)};
    let CLUSTER_LEAVING = false;
    let CLUSTER_OPEN_NODES = ${JSON.stringify(open)};
    ${BORROWED.map(shippedSource).join("\n")}
    // The shell's own esc/fmtAgo/fmtBytes, handed to the model exactly the way
    // the browser hands them over. Borrowing the real ones rather than stubbing
    // keeps the escaping and the "3m ago" formatting under test too.
    return Object.assign({}, PlurxClusterPanel, { ${BORROWED.join(", ")} }, { env: clenv() });
  `;
  return new Function("PlurxClusterPanel", source)(PANEL);
}

// The cluster-wide readings an operator acts on: the strip, the alerts and the
// Maintenance card, as the page paints them.
function opsPage(ui, cluster, ops) {
  return ui.clusterStrip(cluster, ops) + ui.clusterAlerts(cluster, ops) + ui.clusterMaintenanceCard(cluster, ops);
}

// One node's table row and its drawer, in a roster of just that node unless a
// cluster is given.
function nodeRow(ui, n, localNodeId = "node-a", ops = null, cluster = null) {
  return ui.clusterNodeTableRow(n, cluster || { local_node_id: localNodeId, nodes: [n] }, ops);
}

let failures = 0;
let started = 0;
let finished = 0;
// Awaited, counted, and the exit code deferred to beforeExit. Two ways an async
// test can pass without proving anything, both of which this file has shipped:
// a body that is never awaited runs zero assertions, and a body that never
// settles — a harness promise nobody resolves — prints neither PASS nor FAIL and
// simply disappears from the run. Counting starts against finishes catches the
// second; the count is asserted below. Keep this identical to the harness in
// tests/web/page-read-budget.test.js.
// Queued and run in order, not fired off as they are declared. A bare
// `async function test()` called without `await` yields at the first `await`
// inside the body and lets the next declaration start, so these ran
// concurrently and in a different order each time. Nothing here shares mutable
// state today, which is the only reason it was harmless — and it is not a
// property this file can keep on purpose, because each new test would have to
// re-establish it.
const QUEUE = [];
function test(name, run) {
  QUEUE.push({ name, run });
}

async function main() {
  for (const { name, run } of QUEUE) {
    started += 1;
    try {
      await run();
      process.stdout.write(`PASS ${name}\n`);
    } catch (error) {
      failures += 1;
      process.stdout.write(`FAIL ${name}\n${error && error.stack}\n`);
    }
    finished += 1;
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
              db_bytes: 2097152,
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
    // As the server does (http/cluster_operations.rs): a fence already
    // active anywhere is a blocker on every other node's entry verdict.
    maintenance: membership.nodes.map((entry) => {
      const fences = membership.nodes
        .filter((other) => other.maintenance)
        .map((other) => ({
          code: "maintenance_already_active",
          node_id: other.node_id,
          message: "another durable maintenance fence is already active",
        }));
      return {
        node_id: entry.node_id,
        safe_to_enter:
          !unreachable &&
          !fences.length &&
          (entry.is_voter
            ? membership.capacity.voting_nodes === 1 ||
              membership.capacity.voting_nodes >= 3
            : entry.role === "learner"),
        blockers: fences,
      };
    }),
  };
}

function transportOps(observations) {
  return {
    nodes: [
      {
        observation: "answered",
        status: {},
        transport: {
          observing_node_id: observations[0]?.observing_node_id ?? 2,
          observations,
        },
      },
    ],
  };
}

function privateTransportOps(observations) {
  return {
    nodes: [
      {
        observation: "unreachable",
        status: null,
        transport: {
          observing_node_id: observations[0]?.observing_node_id ?? 2,
          observations,
        },
      },
    ],
  };
}

function transportObservation(phase, extra = {}) {
  return {
    observing_node_id: 2,
    peer_node_id: 1,
    raft_group: "sqlite",
    direction: "inbound",
    sample_age_ms: 40,
    age_uncertainty_ms: 0,
    phase,
    ...extra,
  };
}

test("transport recovery phases stay distinct from bounded-read authority", () => {
  const ui = sandbox();
  const active = ui.clusterTransportExplanation(
    transportOps([transportObservation("transferring")]),
    2,
    false,
  );
  const installing = ui.clusterTransportExplanation(
    transportOps([transportObservation("installing")]),
    2,
    false,
  );
  const retrying = ui.clusterTransportExplanation(
    transportOps([transportObservation("retrying")]),
    2,
    false,
  );
  const recovered = ui.clusterTransportExplanation(
    transportOps([transportObservation("complete")]),
    2,
    true,
  );
  const idle = ui.clusterTransportExplanation(transportOps([]), 2, true);
  const unavailable = ui.clusterTransportExplanation(transportOps([]), 2, false);

  assert.deepEqual(
    [active.text, installing.text, retrying.text, recovered.text, idle.text, unavailable.text],
    [
      "receiving snapshot",
      "installing snapshot",
      "retrying snapshot",
      "snapshot recovered",
      "transport idle; bounded-read proof ready",
      "progress unavailable",
    ],
  );
  assert.equal(active.observation.observing_node_id, 2);
  assert.equal(active.code, "transferring");
  assert.equal(idle.code, "idle");
  assert.equal(unavailable.code, "unavailable");
});

test("transport direction keeps sender and receiver explanations honest", () => {
  const ui = sandbox();
  const outbound = ui.clusterTransportExplanation(
    transportOps([
      transportObservation("transferring", {
        observing_node_id: 1,
        peer_node_id: 2,
        direction: "outbound",
      }),
    ]),
    2,
    false,
  );
  const inboundComplete = ui.clusterTransportExplanation(
    transportOps([
      transportObservation("complete", {
        direction: "inbound",
        locally_received_bytes: 3145728,
        acknowledged_offset: null,
      }),
    ]),
    2,
    false,
  );

  assert.equal(outbound.text, "transferring snapshot");
  assert.equal(inboundComplete.text, "snapshot received; sender acknowledgement unconfirmed");
});

test("transport recovery is attributed only to the receiving node", () => {
  const ui = sandbox();
  const inbound = transportObservation("installing", {
    observing_node_id: 2,
    peer_node_id: 1,
    direction: "inbound",
  });
  const outbound = transportObservation("transferring", {
    observing_node_id: 1,
    peer_node_id: 2,
    direction: "outbound",
  });

  assert.equal(
    ui.clusterTransportExplanation(transportOps([inbound]), 1, false).observation,
    null,
  );
  assert.equal(
    ui.clusterTransportExplanation(transportOps([outbound]), 1, false).observation,
    null,
  );
  assert.equal(
    ui.clusterTransportExplanation(transportOps([inbound]), 2, false).observation,
    inbound,
  );
  assert.equal(
    ui.clusterTransportExplanation(transportOps([outbound]), 2, false).observation,
    outbound,
  );
});

test("private cluster-listener evidence survives a closed public listener", () => {
  const ui = sandbox();
  const recovery = ui.clusterTransportExplanation(
    privateTransportOps([transportObservation("installing")]),
    2,
    false,
  );
  assert.equal(recovery.code, "installing");
  assert.equal(recovery.text, "installing snapshot");
});

test("private transport renders while public status is closed for learners and voters", () => {
  const ui = sandbox();
  for (const role of ["learner", "voter"]) {
    const target = node("node-b", 2, role, { bounded_read_ready: false });
    const membership = status("degraded", [
      node("node-a", 1, "voter", { is_leader: true }),
      target,
    ]);
    const operations = operationStatus(membership, { unreachable: true });
    operations.nodes[1].transport = {
      observing_node_id: 2,
      observations: [
        transportObservation("installing", {
          sample_age_ms: 65_000,
          snapshot_id: "snapshot-private",
        }),
      ],
    };

    const operationsHtml = ui.clusterNodeDrawer(target, membership, operations);
    assert.match(operationsHtml, /Not observed/);
    assert.match(
      operationsHtml,
      /<dt>Transport<\/dt><dd>snapshot stalled · observer 2 · 35s ago/,
    );
    const nodeHtml = ui.clusterNodeTableRow(target, membership, operations);
    assert.match(
      nodeHtml,
      /<dt>Transport<\/dt><dd>snapshot stalled · observer 2 · 35s ago/,
      `${role} node card hides private transport evidence`,
    );
  }
});

test("rejected public and wrong-observer transport evidence cannot reappear", () => {
  const ui = sandbox();
  const rejectedPublic = {
    nodes: [{
      observation: "answered",
      transport: null,
      status: {
        transport: {
          observing_node_id: 7,
          observations: [transportObservation("installing", { observing_node_id: 7 })],
        },
      },
    }],
  };
  assert.equal(ui.clusterTransportExplanation(rejectedPublic, 2, false).code, "unavailable");

  const wrongNestedObserver = {
    nodes: [{
      observation: "answered",
      status: {},
      transport: {
        observing_node_id: 2,
        observations: [transportObservation("installing", { observing_node_id: 7 })],
      },
    }],
  };
  assert.equal(
    ui.clusterTransportExplanation(wrongNestedObserver, 2, false).code,
    "unavailable",
  );
});

test("transport status shows outbound evidence and expires stale samples", () => {
  const ui = sandbox();
  const outbound = ui.clusterTransportExplanation(
    transportOps([
      transportObservation("awaiting_acknowledgement", {
        observing_node_id: 1,
        peer_node_id: 2,
        direction: "outbound",
      }),
    ]),
    2,
    false,
  );
  assert.equal(outbound.text, "waiting for snapshot acknowledgement");
  assert.equal(outbound.observation.observing_node_id, 1);

  const expired = ui.clusterTransportExplanation(
    transportOps([transportObservation("transferring", { sample_age_ms: 330001 })]),
    2,
    false,
  );
  assert.equal(expired.code, "unavailable");
  assert.equal(expired.observation, null);

  const watermark = ui.clusterTransportExplanation(
    transportOps([
      transportObservation("complete", {
        observing_node_id: 1,
        peer_node_id: 2,
        direction: "outbound",
        acknowledged_offset: 3145728,
      }),
    ]),
    2,
    false,
  );
  assert.equal(watermark.code, "startup_watermark");
  assert.equal(watermark.text, "waiting for startup watermark");
});

test("retained transport evidence ages, stalls, and expires while refreshes fail", () => {
  const ui = sandbox();
  const inactiveObservation = transportObservation("failed", {
    sample_age_ms: 40,
    active_deadline_remaining_ms: null,
  });
  const inactive = transportOps([inactiveObservation]);

  let current = ui.clusterTransportExplanation(inactive, 2, false, 299_960);
  assert.equal(current.code, "failed");
  assert.equal(current.observation.sample_age_ms, 300_000);
  assert.equal(
    ui.clusterTransportExplanation(inactive, 2, false, 299_961).code,
    "unavailable",
  );
  assert.equal(inactiveObservation.sample_age_ms, 40, "client aging must not renew the retained payload");

  const fallbackObservation = transportObservation("retrying", {
    sample_age_ms: 1_000,
    active_deadline_remaining_ms: null,
    operation_owns_work: true,
  });
  const fallback = transportOps([fallbackObservation]);
  current = ui.clusterTransportExplanation(fallback, 2, false, 28_999);
  assert.equal(current.code, "retrying");
  assert.equal(current.observation.active_deadline_remaining_ms, 1);
  current = ui.clusterTransportExplanation(fallback, 2, false, 29_000);
  assert.equal(current.code, "stalled");
  assert.equal(current.observation.sample_age_ms, 0);
  assert.equal(current.observation.active_deadline_remaining_ms, 0);
  assert.equal(
    ui.clusterTransportExplanation(fallback, 2, false, 329_000).code,
    "stalled",
  );
  assert.equal(
    ui.clusterTransportExplanation(fallback, 2, false, 329_001).code,
    "unavailable",
  );

  const activeObservation = transportObservation("installing", {
    sample_age_ms: 301_000,
    attempt_age_ms: 302_000,
    last_local_receive_age_ms: 50,
    active_deadline_remaining_ms: 1_000,
    operation_owns_work: true,
  });
  const active = transportOps([activeObservation]);

  current = ui.clusterTransportExplanation(active, 2, false, 999);
  assert.equal(current.code, "installing");
  assert.equal(current.observation.sample_age_ms, 301_999);
  assert.equal(current.observation.attempt_age_ms, 302_999);
  assert.equal(current.observation.last_local_receive_age_ms, 1_049);
  assert.equal(current.observation.active_deadline_remaining_ms, 1);

  current = ui.clusterTransportExplanation(active, 2, false, 1_000);
  assert.equal(current.code, "stalled");
  assert.equal(current.observation.sample_age_ms, 0);
  assert.equal(current.observation.active_deadline_remaining_ms, 0);
  assert.equal(current.observation.last_error_category, "snapshot_stalled");

  assert.equal(
    ui.clusterTransportExplanation(active, 2, false, 301_000).code,
    "stalled",
    "the projected stall remains visible through its complete diagnostic window",
  );
  assert.equal(
    ui.clusterTransportExplanation(active, 2, false, 301_001).code,
    "unavailable",
  );
  assert.equal(activeObservation.phase, "installing", "projection must not rewrite cached evidence");

  const serverProjectedStall = transportOps([
    transportObservation("stalled", {
      sample_age_ms: 0,
      active_deadline_remaining_ms: 0,
      operation_owns_work: true,
    }),
  ]);
  assert.equal(
    ui.clusterTransportExplanation(serverProjectedStall, 2, false, 300_000).code,
    "stalled",
    "a server-projected stall keeps its complete post-deadline diagnostic window",
  );
  assert.equal(
    ui.clusterTransportExplanation(serverProjectedStall, 2, false, 300_001).code,
    "unavailable",
  );
});

test("client-local receipt time ignores browser clocks ahead or behind the daemon", () => {
  const realNow = Date.now;
  try {
    for (const [label, browserWall, serverWall] of [
      ["browser ahead", 9_000_000, 1_000_000],
      ["browser behind", 1_000_000, 9_000_000],
    ]) {
      let monotonicNow = 100;
      const clock = { now: () => monotonicNow };
      const receipt = new Function(
        "performance",
        `const CLUSTER_OPS_RECEIVED_AT=new WeakMap();
         ${shippedSource("clusterOpsReceived")}
         ${shippedSource("clusterOpsElapsed")}
         return {received:clusterOpsReceived,elapsed:clusterOpsElapsed};`,
      )(clock);
      const operations = transportOps([
        transportObservation("installing", {
          active_deadline_remaining_ms: 1_000,
          operation_owns_work: true,
        }),
      ]);
      operations.observed_at_unix_ms = serverWall;
      const serialized = JSON.stringify(operations);
      Date.now = () => browserWall;
      receipt.received(operations);
      assert.equal(JSON.stringify(operations), serialized, `${label}: receipt metadata entered JSON`);

      monotonicNow += 1_000;
      assert.equal(receipt.elapsed(operations), 1_000, label);
      assert.equal(
        PANEL.clusterTransportExplanation(operations, 2, false, receipt.elapsed(operations)).code,
        "stalled",
        label,
      );
      monotonicNow += 300_001;
      assert.equal(
        PANEL.clusterTransportExplanation(operations, 2, false, receipt.elapsed(operations)).code,
        "unavailable",
        `${label}: retained evidence did not expire from client elapsed time`,
      );
    }
  } finally {
    Date.now = realNow;
  }
});

test("receiver completion outranks every unacknowledged sender predecessor", () => {
  const ui = sandbox();
  const snapshotId = "snapshot-split-view";
  const snapshotFingerprint = "1".repeat(64);
  const receiverComplete = transportObservation("complete", {
    observing_node_id: 2,
    peer_node_id: 1,
    direction: "inbound",
    snapshot_id: snapshotId,
    snapshot_fingerprint: snapshotFingerprint,
    acknowledged_offset: null,
  });
  const node = (observation) => ({
    transport: {
      observing_node_id: observation.observing_node_id,
      observations: [observation],
    },
  });

  for (const phase of [
    "failed",
    "retrying",
    "stalled",
    "installing",
    "awaiting_acknowledgement",
  ]) {
    const senderPredecessor = transportObservation(phase, {
      observing_node_id: 1,
      peer_node_id: 2,
      direction: "outbound",
      snapshot_id: snapshotId,
      snapshot_fingerprint: snapshotFingerprint,
      acknowledged_offset: null,
    });
    for (const observations of [
      [senderPredecessor, receiverComplete],
      [receiverComplete, senderPredecessor],
    ]) {
      const operations = { nodes: observations.map(node) };
      const waitingForAck = ui.clusterTransportExplanation(operations, 2, false);
      assert.equal(waitingForAck.observation.phase, "complete", `${phase}: phase`);
      assert.equal(waitingForAck.observation.direction, "inbound", `${phase}: direction`);
      assert.equal(
        waitingForAck.text,
        "snapshot received; sender acknowledgement unconfirmed",
        phase,
      );
      const recovered = ui.clusterTransportExplanation(operations, 2, true);
      assert.equal(recovered.observation.phase, "complete", `${phase}: recovered phase`);
      assert.equal(recovered.text, "snapshot recovered", `${phase}: recovered text`);
    }
  }

  // A sender Complete carrying the flushed acknowledgement remains causally
  // stronger than the receiver's local completion in either row order.
  const senderAcknowledged = transportObservation("complete", {
    observing_node_id: 1,
    peer_node_id: 2,
    direction: "outbound",
    snapshot_id: snapshotId,
    snapshot_fingerprint: snapshotFingerprint,
    acknowledged_offset: 4096,
  });
  for (const observations of [
    [senderAcknowledged, receiverComplete],
    [receiverComplete, senderAcknowledged],
  ]) {
    const acknowledged = ui.clusterTransportExplanation(
      { nodes: observations.map(node) },
      2,
      false,
    );
    assert.equal(acknowledged.observation.phase, "complete");
    assert.equal(acknowledged.observation.direction, "outbound");
    assert.equal(acknowledged.observation.acknowledged_offset, 4096);
    assert.equal(acknowledged.text, "waiting for startup watermark");
  }
});

test("fresh snapshot identity outranks stale failure evidence from another observer", () => {
  const ui = sandbox();
  const operations = {
    nodes: [
      {
        transport: {
          observing_node_id: 1,
          observations: [
            transportObservation("stalled", {
              observing_node_id: 1,
              peer_node_id: 2,
              direction: "outbound",
              snapshot_id: "former-leader-snapshot",
              snapshot_fingerprint: "2".repeat(64),
              sample_age_ms: 299_000,
            }),
          ],
        },
      },
      {
        transport: {
          observing_node_id: 2,
          observations: [
            transportObservation("installing", {
              observing_node_id: 2,
              peer_node_id: 1,
              direction: "inbound",
              snapshot_id: "current-leader-snapshot",
              snapshot_fingerprint: "3".repeat(64),
              sample_age_ms: 40,
            }),
          ],
        },
      },
    ],
  };

  const current = ui.clusterTransportExplanation(operations, 2, false);
  assert.equal(current.code, "installing");
  assert.equal(current.observation.snapshot_id, "current-leader-snapshot");
  assert.equal(current.observation.observing_node_id, 2);
});

test("fresh attempt wins over a stale stall for the same snapshot fingerprint", () => {
  const ui = sandbox();
  const operations = {
    nodes: [
      {
        transport: {
          observing_node_id: 1,
          observations: [
            transportObservation("stalled", {
              observing_node_id: 1,
              peer_node_id: 2,
              direction: "outbound",
              snapshot_id: "same-snapshot",
              snapshot_fingerprint: "4".repeat(64),
              boot_id: "former-boot",
              attempt_id: 4,
              socket_epoch: 2,
              sample_age_ms: 299_000,
            }),
          ],
        },
      },
      {
        transport: {
          observing_node_id: 2,
          observations: [
            transportObservation("installing", {
              snapshot_id: "same-snapshot",
              snapshot_fingerprint: "4".repeat(64),
              boot_id: "current-boot",
              attempt_id: 9,
              socket_epoch: 7,
              sample_age_ms: 40,
            }),
          ],
        },
      },
    ],
  };

  const current = ui.clusterTransportExplanation(operations, 2, false);
  assert.equal(current.code, "installing");
  assert.equal(current.observation.boot_id, "current-boot");
});

test("sub-five-second successor attempt outranks same-fingerprint stalled predecessor", () => {
  const ui = sandbox();
  const snapshotFingerprint = "e".repeat(64);
  const predecessor = transportObservation("stalled", {
    observing_node_id: 1,
    peer_node_id: 2,
    direction: "outbound",
    snapshot_id: "restarted-snapshot",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "former-boot",
    attempt_id: 4,
    socket_epoch: 2,
    sample_age_ms: 4_000,
    attempt_age_ms: 60_000,
  });
  const successor = transportObservation("installing", {
    observing_node_id: 2,
    peer_node_id: 1,
    direction: "inbound",
    snapshot_id: "restarted-snapshot",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "current-boot",
    attempt_id: 9,
    socket_epoch: 7,
    sample_age_ms: 0,
    attempt_age_ms: 1_000,
    active_deadline_remaining_ms: 60_000,
  });

  for (const observations of [[predecessor, successor], [successor, predecessor]]) {
    const operations = {
      nodes: observations.map((observation) => ({
        transport: {
          observing_node_id: observation.observing_node_id,
          observations: [observation],
        },
      })),
    };
    const current = ui.clusterTransportExplanation(operations, 2, false);
    assert.equal(current.code, "installing");
    assert.equal(current.observation.boot_id, "current-boot");
    assert.equal(current.observation.attempt_id, 9);
  }
});

test("late predecessor failure cannot hide an already-started successor attempt", () => {
  const ui = sandbox();
  const snapshotFingerprint = "f".repeat(64);
  const predecessor = transportObservation("failed", {
    observing_node_id: 1,
    peer_node_id: 2,
    direction: "outbound",
    snapshot_id: "late-cancelled-snapshot",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "former-leader",
    attempt_id: 4,
    socket_epoch: 2,
    sample_age_ms: 0,
    attempt_age_ms: 60_000,
  });
  const successor = transportObservation("installing", {
    observing_node_id: 2,
    peer_node_id: 1,
    direction: "inbound",
    snapshot_id: "late-cancelled-snapshot",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "current-receiver",
    attempt_id: 9,
    socket_epoch: 7,
    sample_age_ms: 6_000,
    attempt_age_ms: 10_000,
    active_deadline_remaining_ms: 60_000,
  });

  for (const observations of [[predecessor, successor], [successor, predecessor]]) {
    const operations = {
      nodes: observations.map((observation) => ({
        transport: {
          observing_node_id: observation.observing_node_id,
          observations: [observation],
        },
      })),
    };
    const current = ui.clusterTransportExplanation(operations, 2, false);
    assert.equal(current.code, "installing");
    assert.equal(current.observation.boot_id, "current-receiver");
    assert.equal(current.observation.attempt_id, 9);
  }
});

test("late predecessor failure cannot hide a newer different-snapshot successor", () => {
  const ui = sandbox();
  const predecessor = transportObservation("failed", {
    observing_node_id: 1,
    peer_node_id: 2,
    direction: "outbound",
    snapshot_id: "former-snapshot",
    snapshot_fingerprint: "1".repeat(64),
    boot_id: "former-leader",
    attempt_id: 4,
    socket_epoch: 2,
    sample_age_ms: 0,
    attempt_age_ms: 60_000,
  });
  const successor = transportObservation("installing", {
    observing_node_id: 2,
    peer_node_id: 1,
    direction: "inbound",
    snapshot_id: "successor-snapshot",
    snapshot_fingerprint: "2".repeat(64),
    boot_id: "current-receiver",
    attempt_id: 9,
    socket_epoch: 7,
    sample_age_ms: 500,
    attempt_age_ms: 1_000,
    active_deadline_remaining_ms: 60_000,
  });

  for (const observations of [[predecessor, successor], [successor, predecessor]]) {
    const operations = {
      nodes: observations.map((observation) => ({
        transport: {
          observing_node_id: observation.observing_node_id,
          observations: [observation],
        },
      })),
    };
    const current = ui.clusterTransportExplanation(operations, 2, false);
    assert.equal(current.code, "installing");
    assert.equal(current.observation.snapshot_id, "successor-snapshot");
  }
});

test("response transit uncertainty cannot make a delayed predecessor look newer", () => {
  const ui = sandbox();
  const snapshotFingerprint = "9".repeat(64);
  const delayedPredecessor = transportObservation("failed", {
    observing_node_id: 1,
    peer_node_id: 2,
    direction: "outbound",
    snapshot_id: "transit-delayed-snapshot",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "former-leader",
    attempt_id: 4,
    socket_epoch: 2,
    sample_age_ms: 900,
    attempt_age_ms: 1_900,
    age_uncertainty_ms: 900,
  });
  const successor = transportObservation("installing", {
    snapshot_id: "transit-delayed-snapshot",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "current-receiver",
    attempt_id: 9,
    socket_epoch: 7,
    sample_age_ms: 900,
    attempt_age_ms: 1_400,
    active_deadline_remaining_ms: 60_000,
  });

  for (const observations of [
    [delayedPredecessor, successor],
    [successor, delayedPredecessor],
  ]) {
    const operations = {
      nodes: observations.map((observation) => ({
        transport: {
          observing_node_id: observation.observing_node_id,
          observations: [observation],
        },
      })),
    };
    const current = ui.clusterTransportExplanation(operations, 2, false);
    assert.equal(current.code, "installing");
    assert.equal(current.observation.boot_id, "current-receiver");
  }
});

test("an uncertain bridge cannot promote a provably older attempt", () => {
  const ui = sandbox();
  const snapshotFingerprint = "b".repeat(64);
  const newest = transportObservation("installing", {
    snapshot_id: "partial-order-frontier",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "newest",
    attempt_id: 3,
    socket_epoch: 3,
    sample_age_ms: 0,
    attempt_age_ms: 0,
    age_uncertainty_ms: 0,
    active_deadline_remaining_ms: 60_000,
  });
  const uncertainBridge = transportObservation("failed", {
    snapshot_id: "partial-order-frontier",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "uncertain-bridge",
    attempt_id: 2,
    socket_epoch: 2,
    sample_age_ms: 100,
    attempt_age_ms: 100,
    age_uncertainty_ms: 100,
  });
  const provablyOlder = transportObservation("stalled", {
    snapshot_id: "partial-order-frontier",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "provably-older",
    attempt_id: 1,
    socket_epoch: 1,
    sample_age_ms: 100,
    attempt_age_ms: 100,
    age_uncertainty_ms: 0,
  });

  for (const observations of [
    [newest, uncertainBridge, provablyOlder],
    [newest, provablyOlder, uncertainBridge],
    [uncertainBridge, newest, provablyOlder],
    [uncertainBridge, provablyOlder, newest],
    [provablyOlder, newest, uncertainBridge],
    [provablyOlder, uncertainBridge, newest],
  ]) {
    const operations = {
      nodes: observations.map((observation) => ({
        transport: {
          observing_node_id: observation.observing_node_id,
          observations: [observation],
        },
      })),
    };
    const current = ui.clusterTransportExplanation(operations, 2, false);
    assert.equal(current.code, "installing");
    assert.equal(current.observation.boot_id, "newest");
  }
});

test("late predecessor acknowledgement cannot hide a restarted receiver", () => {
  const ui = sandbox();
  const snapshotFingerprint = "a".repeat(64);
  const predecessor = transportObservation("complete", {
    observing_node_id: 1,
    peer_node_id: 2,
    direction: "outbound",
    snapshot_id: "restarted-after-ack",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "former-leader",
    attempt_id: 4,
    socket_epoch: 2,
    sample_age_ms: 0,
    attempt_age_ms: 60_000,
    acknowledged_offset: 4_096,
  });
  const successor = transportObservation("installing", {
    snapshot_id: "restarted-after-ack",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "current-receiver",
    attempt_id: 9,
    socket_epoch: 7,
    sample_age_ms: 500,
    attempt_age_ms: 1_000,
    active_deadline_remaining_ms: 60_000,
  });

  for (const observations of [[predecessor, successor], [successor, predecessor]]) {
    const operations = {
      nodes: observations.map((observation) => ({
        transport: {
          observing_node_id: observation.observing_node_id,
          observations: [observation],
        },
      })),
    };
    const current = ui.clusterTransportExplanation(operations, 2, false);
    assert.equal(current.code, "installing");
    assert.equal(current.observation.boot_id, "current-receiver");
  }
});

test("mixed-version attempt ages use one permutation-invariant event-age fallback", () => {
  const ui = sandbox();
  const snapshotFingerprint = "3".repeat(64);
  const evidence = {
    currentA: transportObservation("retrying", {
      snapshot_fingerprint: snapshotFingerprint,
      boot_id: "current-a",
      attempt_id: 11,
      socket_epoch: 5,
      sample_age_ms: 100,
      attempt_age_ms: 1_000,
    }),
    currentB: transportObservation("retrying", {
      snapshot_fingerprint: snapshotFingerprint,
      boot_id: "current-b",
      attempt_id: 12,
      socket_epoch: 6,
      sample_age_ms: 0,
      attempt_age_ms: 2_000,
    }),
    legacy: transportObservation("retrying", {
      snapshot_fingerprint: snapshotFingerprint,
      boot_id: "legacy",
      attempt_id: 3,
      socket_epoch: 2,
      sample_age_ms: 50,
      attempt_age_ms: null,
    }),
  };
  const permutations = [
    ["currentA", "currentB", "legacy"],
    ["currentA", "legacy", "currentB"],
    ["currentB", "currentA", "legacy"],
    ["currentB", "legacy", "currentA"],
    ["legacy", "currentA", "currentB"],
    ["legacy", "currentB", "currentA"],
  ];

  for (const order of permutations) {
    const operations = {
      nodes: order.map((name) => ({
        transport: {
          observing_node_id: evidence[name].observing_node_id,
          observations: [evidence[name]],
        },
      })),
    };
    const current = ui.clusterTransportExplanation(operations, 2, false);
    assert.equal(current.observation.boot_id, "current-b", order.join(","));
  }
});

test("bounded display collisions do not correlate distinct snapshot fingerprints", () => {
  const ui = sandbox();
  const collidingDisplay = `sha256:${"a".repeat(64)}`;
  const operations = {
    nodes: [
      {
        transport: {
          observing_node_id: 1,
          observations: [
            transportObservation("failed", {
              observing_node_id: 1,
              peer_node_id: 2,
              direction: "outbound",
              snapshot_id: collidingDisplay,
              snapshot_fingerprint: "5".repeat(64),
              sample_age_ms: 40,
            }),
          ],
        },
      },
      {
        transport: {
          observing_node_id: 2,
          observations: [
            transportObservation("complete", {
              snapshot_id: collidingDisplay,
              snapshot_fingerprint: "6".repeat(64),
              sample_age_ms: 299_000,
            }),
          ],
        },
      },
    ],
  };

  const current = ui.clusterTransportExplanation(operations, 2, false);
  assert.equal(current.code, "failed");
  assert.equal(current.observation.snapshot_fingerprint, "5".repeat(64));
});

test("legacy snapshot labels without fingerprints remain attempt-local", () => {
  const ui = sandbox();
  const operations = {
    nodes: [
      {
        transport: {
          observing_node_id: 1,
          observations: [
            transportObservation("failed", {
              observing_node_id: 1,
              peer_node_id: 2,
              direction: "outbound",
              snapshot_id: "legacy-shared-label",
              boot_id: "current-sender",
              attempt_id: 12,
              socket_epoch: 8,
              sample_age_ms: 40,
            }),
          ],
        },
      },
      {
        transport: {
          observing_node_id: 2,
          observations: [
            transportObservation("complete", {
              snapshot_id: "legacy-shared-label",
              boot_id: "former-receiver",
              attempt_id: 3,
              socket_epoch: 1,
              sample_age_ms: 299_000,
            }),
          ],
        },
      },
    ],
  };

  const current = ui.clusterTransportExplanation(operations, 2, false);
  assert.equal(current.code, "failed");
  assert.equal(current.observation.boot_id, "current-sender");
});

test("transport selection is permutation invariant across fingerprint cohorts", () => {
  const ui = sandbox();
  const evidence = {
    stale: transportObservation("stalled", {
      observing_node_id: 1,
      peer_node_id: 2,
      direction: "outbound",
      snapshot_id: "snapshot-a",
      snapshot_fingerprint: "7".repeat(64),
      boot_id: "stale-a",
      attempt_id: 1,
      socket_epoch: 1,
      sample_age_ms: 299_000,
    }),
    freshA: transportObservation("transferring", {
      snapshot_id: "snapshot-a",
      snapshot_fingerprint: "7".repeat(64),
      boot_id: "fresh-a",
      attempt_id: 2,
      socket_epoch: 2,
      sample_age_ms: 40,
    }),
    freshB: transportObservation("installing", {
      snapshot_id: "snapshot-b",
      snapshot_fingerprint: "8".repeat(64),
      boot_id: "fresh-b",
      attempt_id: 3,
      socket_epoch: 3,
      sample_age_ms: 40,
    }),
  };
  const permutations = [
    ["stale", "freshA", "freshB"],
    ["stale", "freshB", "freshA"],
    ["freshA", "stale", "freshB"],
    ["freshA", "freshB", "stale"],
    ["freshB", "stale", "freshA"],
    ["freshB", "freshA", "stale"],
  ];

  for(const order of permutations){
    const operations = {
      nodes: order.map(name=>({
        transport: {
          observing_node_id: evidence[name].observing_node_id,
          observations: [evidence[name]],
        },
      })),
    };
    const current = ui.clusterTransportExplanation(operations, 2, false);
    assert.equal(current.code, "installing", order.join(","));
    assert.equal(current.observation.boot_id, "fresh-b", order.join(","));
  }
});

test("fresh acknowledged sender completion outranks stale receiver completion", () => {
  const ui = sandbox();
  const snapshotFingerprint = "9".repeat(64);
  const operations = {
    nodes: [
      {
        transport: {
          observing_node_id: 1,
          observations: [
            transportObservation("complete", {
              observing_node_id: 1,
              peer_node_id: 2,
              direction: "outbound",
              snapshot_id: "completed-snapshot",
              snapshot_fingerprint: snapshotFingerprint,
              acknowledged_offset: 4096,
              sample_age_ms: 40,
            }),
          ],
        },
      },
      {
        transport: {
          observing_node_id: 2,
          observations: [
            transportObservation("complete", {
              snapshot_id: "completed-snapshot",
              snapshot_fingerprint: snapshotFingerprint,
              acknowledged_offset: null,
              sample_age_ms: 299_000,
            }),
          ],
        },
      },
    ],
  };

  const current = ui.clusterTransportExplanation(operations, 2, false);
  assert.equal(current.code, "startup_watermark");
  assert.equal(current.text, "waiting for startup watermark");
  assert.equal(current.observation.direction, "outbound");
  assert.equal(current.observation.acknowledged_offset, 4096);
});

test("acknowledged completion supersedes every cached predecessor phase in any row order", () => {
  const ui = sandbox();
  const snapshotFingerprint = "a".repeat(64);
  const acknowledged = transportObservation("complete", {
    observing_node_id: 1,
    peer_node_id: 2,
    direction: "outbound",
    snapshot_id: "causally-complete-snapshot",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "current-sender",
    attempt_id: 20,
    socket_epoch: 9,
    acknowledged_offset: 4096,
    sample_age_ms: 40,
  });
  const phases = [
    ["connecting", "outbound"],
    ["transferring", "inbound"],
    ["awaiting_acknowledgement", "outbound"],
    ["installing", "inbound"],
    ["retrying", "inbound"],
    ["stalled", "inbound"],
    ["failed", "inbound"],
    ["complete", "inbound"],
  ];

  for (const [phase, direction] of phases) {
    const predecessor = transportObservation(phase, {
      observing_node_id: direction === "outbound" ? 3 : 2,
      peer_node_id: direction === "outbound" ? 2 : 1,
      direction,
      snapshot_id: "causally-complete-snapshot",
      snapshot_fingerprint: snapshotFingerprint,
      boot_id: `cached-${phase}`,
      attempt_id: 4,
      socket_epoch: 2,
      acknowledged_offset: null,
      sample_age_ms: 4_000,
      active_deadline_remaining_ms: phase === "complete" || phase === "failed" ? null : 60_000,
    });
    for (const observations of [[acknowledged, predecessor], [predecessor, acknowledged]]) {
      const operations = {
        nodes: observations.map((observation) => ({
          transport: {
            observing_node_id: observation.observing_node_id,
            observations: [observation],
          },
        })),
      };
      const current = ui.clusterTransportExplanation(operations, 2, false);
      assert.equal(current.code, "startup_watermark", `${phase}: ${observations[0].direction} first`);
      assert.equal(current.observation.direction, "outbound", phase);
      assert.equal(current.observation.acknowledged_offset, 4096, phase);
    }
  }
});

test("acknowledged completion never crosses distinct, legacy, or malformed identities", () => {
  const ui = sandbox();
  const identities = [
    ["b".repeat(64), "c".repeat(64), "distinct fingerprints"],
    [undefined, undefined, "legacy attempts"],
    ["not-a-fingerprint", "not-a-fingerprint", "malformed fingerprints"],
  ];

  for (const [ackFingerprint, stalledFingerprint, label] of identities) {
    const acknowledged = transportObservation("complete", {
      observing_node_id: 1,
      peer_node_id: 2,
      direction: "outbound",
      snapshot_id: "shared-display-label",
      snapshot_fingerprint: ackFingerprint,
      boot_id: "sender",
      attempt_id: 7,
      socket_epoch: 4,
      acknowledged_offset: 4096,
      sample_age_ms: 40,
    });
    const stalled = transportObservation("stalled", {
      snapshot_id: "shared-display-label",
      snapshot_fingerprint: stalledFingerprint,
      boot_id: "receiver",
      attempt_id: 3,
      socket_epoch: 2,
      sample_age_ms: 40,
    });
    for (const observations of [[acknowledged, stalled], [stalled, acknowledged]]) {
      const operations = {
        nodes: observations.map((observation) => ({
          transport: {
            observing_node_id: observation.observing_node_id,
            observations: [observation],
          },
        })),
      };
      const current = ui.clusterTransportExplanation(operations, 2, false);
      assert.equal(current.code, "stalled", label);
      assert.equal(current.observation.direction, "inbound", label);
    }
  }
});

test("stale completion evidence never suppresses a newer same-fingerprint successor", () => {
  const ui = sandbox();
  const snapshotFingerprint = "d".repeat(64);
  const staleAcknowledged = transportObservation("complete", {
    observing_node_id: 1,
    peer_node_id: 2,
    direction: "outbound",
    snapshot_id: "retried-snapshot",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "former-leader",
    attempt_id: 4,
    socket_epoch: 2,
    acknowledged_offset: 4096,
    sample_age_ms: 4_000,
  });
  const successors = [
    transportObservation("installing", {
      snapshot_id: "retried-snapshot",
      snapshot_fingerprint: snapshotFingerprint,
      boot_id: "current-receiver",
      attempt_id: 9,
      socket_epoch: 7,
      sample_age_ms: 40,
      active_deadline_remaining_ms: 60_000,
    }),
    transportObservation("failed", {
      snapshot_id: "retried-snapshot",
      snapshot_fingerprint: snapshotFingerprint,
      boot_id: "current-receiver",
      attempt_id: 10,
      socket_epoch: 8,
      sample_age_ms: 40,
      active_deadline_remaining_ms: null,
    }),
  ];

  for (const successor of successors) {
    for (const observations of [[staleAcknowledged, successor], [successor, staleAcknowledged]]) {
      const operations = {
        nodes: observations.map((observation) => ({
          transport: {
            observing_node_id: observation.observing_node_id,
            observations: [observation],
          },
        })),
      };
      const current = ui.clusterTransportExplanation(operations, 2, false);
      assert.equal(current.code, successor.phase, `${successor.phase}: ${observations[0].boot_id} first`);
      assert.equal(current.observation.boot_id, "current-receiver", successor.phase);
    }
  }
});

test("older receiver completion never suppresses a newer outbound failure", () => {
  const ui = sandbox();
  const snapshotFingerprint = "e".repeat(64);
  const staleReceiver = transportObservation("complete", {
    snapshot_id: "retried-snapshot",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "former-receiver",
    attempt_id: 4,
    socket_epoch: 2,
    sample_age_ms: 4_000,
  });
  const currentFailure = transportObservation("failed", {
    observing_node_id: 1,
    peer_node_id: 2,
    direction: "outbound",
    snapshot_id: "retried-snapshot",
    snapshot_fingerprint: snapshotFingerprint,
    boot_id: "current-sender",
    attempt_id: 10,
    socket_epoch: 8,
    sample_age_ms: 40,
  });

  for (const observations of [[staleReceiver, currentFailure], [currentFailure, staleReceiver]]) {
    const operations = {
      nodes: observations.map((observation) => ({
        transport: {
          observing_node_id: observation.observing_node_id,
          observations: [observation],
        },
      })),
    };
    const current = ui.clusterTransportExplanation(operations, 2, false);
    assert.equal(current.code, "failed", observations[0].direction);
    assert.equal(current.observation.boot_id, "current-sender");
  }
});

test("stalled transport renders its observer and sample age", () => {
  const ui = sandbox();
  const membership = status("degraded", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "learner", { bounded_read_ready: false }),
  ]);
  const operations = operationStatus(membership);
  operations.nodes[0].transport = {
    observing_node_id: 1,
    observations: [
      transportObservation("stalled", {
        observing_node_id: 1,
        peer_node_id: 2,
        direction: "outbound",
        sample_age_ms: 65_000,
      }),
    ],
  };

  const html = ui.clusterNodeDrawer(membership.nodes[1], membership, operations);
  assert.match(html, /<dt>Transport<\/dt><dd>snapshot stalled · observer 1 · 1m ago/);
});

test("active transport stays visible through its server-projected deadline", () => {
  const ui = sandbox();
  const installing = ui.clusterTransportExplanation(
    transportOps([
      transportObservation("installing", {
        sample_age_ms: 300001,
        active_deadline_remaining_ms: 3299000,
        operation_owns_work: true,
      }),
    ]),
    2,
    false,
  );

  assert.equal(installing.code, "installing");
  assert.equal(installing.text, "installing snapshot");
});

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
  assert.match(html, /<div class="clalert" role="status"><div><b>Reconfiguration in progress — not redundant\.<\/b>/);
  assertNoRedundancyClaim(html.replace(/<[^>]*>/g, " "), "the two-voter panel");
  assert.match(
    html,
    /disabled >Enter maintenance<\/button><span class="clwhy">The direct maintenance preflight has not answered yet\./,
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
  const summary = opsPage(ui, membership, operations);
  const nodeHtml = ui.clusterNodeDrawer(membership.nodes[1], membership, operations);
  assert.match(summary, /The observed voters preserve majority after one restart/);
  assert.match(summary, /3 voters · quorum 2 · tolerates 1 failure/);
  assert.match(nodeHtml, /Follower · term 81 · lag 0/);
  assert.match(nodeHtml, /open.*3 segments.*6\.3 MB/s);
  assert.match(nodeHtml, /<dt>Durable index<\/dt><dd>482191<\/dd>/);
  assert.match(nodeHtml, /unknown here — drain proxy connections separately/);
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
  const html = opsPage(ui, membership, operations);
  const nodeHtml = ui.clusterNodeDrawer(membership.nodes[1], membership, operations);
  assert.match(html, /<div class="clv clwarn">Blocked<\/div>/);
  assert.match(html, /voter not observed/);
  assert.match(nodeHtml, /Not observed · no route to it from this node/);
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
  const electionHtml = opsPage(ui, membership, election);
  const electionNode = ui.clusterNodeDrawer(membership.nodes[2], membership, election);
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
  const fencedHtml = opsPage(ui, membership, fenced);
  const fencedNode = ui.clusterNodeDrawer(membership.nodes[1], membership, fenced);
  assert.match(fencedNode, /fenced: quorum stale/);
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
  const skewHtml = opsPage(ui, membership, skew);
  const skewNode = ui.clusterNodeDrawer(membership.nodes[2], membership, skew);
  assert.match(skewHtml, />2 builds</);
  assert.match(skewHtml, /mixed builds/);
  assert.match(skewNode, /v0\.2\.8-next/);

  // A two-build fleet mid-upgrade: build, media protocol and eligibility are
  // shown side by side, and a protocol skew names the cohort split.
  const wire = operationStatus(membership);
  wire.nodes.forEach((row) => { row.status.media_protocol = 9; row.placement = { eligible_owner: true }; });
  wire.nodes[2].status.build = "v0.2.8-next";
  wire.nodes[2].status.media_protocol = 10;
  wire.nodes[2].placement = { eligible_owner: false, reason: "protocol_mismatch:10" };
  delete wire.nodes[1].status.media_protocol;
  wire.nodes[1].placement = { eligible_owner: false, reason: "protocol_unreported" };
  const ownerNode = ui.clusterNodeDrawer(membership.nodes[0], membership, wire);
  const skewedNode = ui.clusterNodeDrawer(membership.nodes[2], membership, wire);
  const oldNode = ui.clusterNodeDrawer(membership.nodes[1], membership, wire);
  assert.match(ownerNode, /media protocol 9/);
  assert.match(ownerNode, /eligible owner/);
  assert.match(skewedNode, /media protocol 10/);
  assert.match(skewedNode, /not an owner: it speaks media protocol 10/);
  assert.match(skewedNode, /Protocol skew: 10 \/ 9 \/ unreported/);
  assert.match(oldNode, /media protocol unreported/);
  assert.match(oldNode, /not an owner: its build predates media-protocol reporting/);
  const uniform = operationStatus(membership);
  uniform.nodes.forEach((row) => { row.status.media_protocol = 9; });
  assert.doesNotMatch(ui.clusterNodeDrawer(membership.nodes[0], membership, uniform), /Protocol skew/);

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
  const walHtml = opsPage(ui, membership, walError);
  const walNode = ui.clusterNodeDrawer(membership.nodes[1], membership, walError);
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
  const staleHtml = opsPage(ui, membership, stale);
  const staleNode = ui.clusterNodeDrawer(membership.nodes[1], membership, stale);
  assert.match(staleNode, /Not observed · its answer was older than one refresh window/);
  assert.match(staleHtml, /<div class="clv clwarn">Blocked<\/div>/);

  const oneDown = operationStatus(membership, { safe: false, unreachable: true });
  const oneDownHtml = opsPage(ui, membership, oneDown);
  assert.match(oneDownHtml, /4 of 4 answered|3 of 4 answered/);
  assert.match(oneDownHtml, /<div class="clv clwarn">Blocked<\/div>/);
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
  assert.match(html, /Clustering is not enabled\./);
  // A "Create a join token" button here would mint nothing and refuse — the
  // dead end this panel is supposed to not have.
  assert.equal(html.includes("Create a join token"), false);
  assert.equal(html.includes("<table"), false);
  assert.doesNotMatch(html, /SQLite|Replication|<button|<input/);
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
// A read-only member with a fresh heartbeat, zero apply lag and a green "Read
// worker ready" pill was simultaneously reporting, one row down, "Not observed ·
// unreachable". Both halves were true readings of different sources — the roster
// and the direct fan-out — and the fan-out's single word was the wrong noun: the
// node had answered and refused the proof. The words are the whole product here,
// so each observation code says what happened and what it implies about the box.
test("a refusal says the node answered; only silence says unreachable", () => {
  const ui = sandbox();
  const membership = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
    node("node-d", 7, "learner", { bounded_read_ready: true }),
  ]);
  const refused = operationStatus(membership, { safe: true });
  refused.nodes[3].observation = "refused";
  refused.nodes[3].error_class = "refused";
  refused.nodes[3].status = null;

  const html = ui.clusterNodeDrawer(membership.nodes[3], membership, refused);
  assert.match(html, /Not observed · it answered and refused the signed request from this node/);
  // The specific regression: a node that replied must never be described as
  // one that could not be reached.
  assert.doesNotMatch(html, /unreachable/);
  // And the invariant the panel is built on is not softened by nicer wording.
  assert.match(html, /Membership heartbeat state is not a substitute/);

  const silent = operationStatus(membership, { safe: true });
  silent.nodes[3].observation = "unreachable";
  silent.nodes[3].error_class = "unreachable";
  silent.nodes[3].status = null;
  assert.match(
    ui.clusterNodeDrawer(membership.nodes[3], membership, silent),
    /Not observed · no route to it from this node/,
  );
});

// The sentences are only worth having if the server cannot introduce a code
// that has none. The aggregator's error classes are string literals in one
// match arm, so read them and require a sentence for each: a new
// ObservationState with no panel entry fails here rather than shipping the bare
// identifier to an operator.
test("every observation code the aggregator can emit has a sentence", () => {
  const source = fs.readFileSync(
    path.join(__dirname, "../../crates/plurxd/src/http/cluster_operations.rs"),
    "utf8",
  );
  const arm = source.split("fn observation_error_class(")[1];
  assert.notEqual(arm, undefined, "observation_error_class was renamed or removed");
  const codes = [...arm.split("\n}")[0].matchAll(/=>\s*"([a-z_]+)"/g)].map((m) => m[1]);
  assert.ok(codes.includes("refused"), `refused missing from ${codes.join(", ")}`);
  assert.ok(codes.includes("unreachable"));
  assert.ok(codes.length >= 7, `only found ${codes.length} codes`);
  // Plus the classes the join stamps directly rather than through the map,
  // and the panel's own "no row at all" case.
  for (const code of [
    ...codes,
    "raft_identity_mismatch",
    "stale_peer_sample",
    "cache_unavailable",
    "cache_stale",
    "not_observed",
  ]) {
    if (code === "none") continue;
    const sentence = PANEL.clusterObservationReason(code);
    assert.notEqual(
      sentence,
      code.replaceAll("_", " "),
      `${code} still reaches the operator as its raw identifier`,
    );
    assert.ok(sentence.length > code.length, `${code} has no real sentence`);
  }
});

// one an operator hovers rather than reads in a banner. Nothing asserted it,
// so it could be reverted to "Admitted and catching up" by itself.
test("the learner role pill says what the banner says", () => {
  const ui = sandbox();
  const row = nodeRow(ui, 
    node("node-b", 2, "learner", {
      bounded_read_ready: true,
      voter_storage_ready: true,
    }),
    "node-a",
  );
  assert.match(row, /non-voting capacity role/i);
  assert.match(row, /explicit promotion adds its vote/i);
  assert.match(row, /read worker/i);
  assert.match(row, /Promote to voter/i);
  assert.doesNotMatch(row, /Promote to voter" disabled/);

  // And none of that leaks onto a voter's pill.
  const voter = nodeRow(ui, node("node-a", 1, "voter"), "node-a");
  assert.match(voter, />voter</);
  assert.doesNotMatch(voter, /non-voting/i);
});

test("promotion stays disabled until both learner proofs are ready", () => {
  const ui = sandbox();
  for (const extra of [
    { bounded_read_ready: false, voter_storage_ready: true },
    { bounded_read_ready: true, voter_storage_ready: false },
  ]) {
    const row = nodeRow(ui, node("node-b", 2, "learner", extra), "node-a");
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

  const row = nodeRow(ui, joining, "node-a");
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
  const html = nodeRow(ui, 
    node("node-a", 1, "voter", { last_seen_at: Date.now() - 120_000 }),
  );
  assert.match(html, /2m ago/);
  assert.equal(/\d{2,}[yd] ago/.test(html), false, `stale-looking row: ${html}`);
});

test("a roster row shows hostname and advertised host without listener ports", () => {
  const ui = sandbox();
  const row = nodeRow(ui, node("node-a", 7, "voter"));
  assert.match(row, /class="clhost">node-a/);
  assert.match(row, /class="clsub">node-a\.lan<\/div>/);
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
  const row = nodeRow(ui, 
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
  const row = nodeRow(ui, 
    node("550e8400-e29b-41d4-a716-446655440000", 7, "voter", {
      hostname: "living-room-plurx",
      advertised_host: "10.42.1.20",
    }),
  );
  assert.match(row, /class="clhost">living-room-plurx/);
  assert.match(row, /class="clsub">10\.42\.1\.20<\/div>/);
  assert.match(
    row,
    /<dt>Node ID<\/dt><dd><span class="clmono">550e8400-e29b-41d4-a716-446655440000/,
  );
  assert.ok(
    row.replace(/<[^>]*>/g, " ").indexOf("living-room-plurx") < row.replace(/<[^>]*>/g, " ").indexOf("550e8400-e29b"),
    `node id appeared before hostname: ${row}`,
  );
});

test("the current leader is labeled beside its hostname", () => {
  const ui = sandbox();
  const leader = nodeRow(ui, 
    node("node-b", 2, "voter", { is_leader: true }),
  );
  const follower = nodeRow(ui, node("node-a", 1, "voter"));
  assert.match(leader, /class="clhost">node-b[\s\S]*>Leader</);
  assert.equal(follower.includes(">Leader<"), false);
});

test("node actions stay inside the node card footer", () => {
  const ui = sandbox();
  const row = nodeRow(ui, node("node-a", 1, "voter"));
  // The row itself is a summary to click; its one control is the drawer
  // toggle, and every action is in the drawer's action row, under the
  // readings it acts on.
  const [summary, drawer] = row.split('<tr class="setdrawer"');
  assert.deepEqual(summary.match(/<button[^>]*>/g),
    ['<button type="button" class="clnodebtn" data-node="node-a" aria-expanded="false" aria-controls="cldrawer-node-a" onclick="toggleClusterNode(this)">']);
  assert.match(drawer, /<div class="cldraweract">[\s\S]*>Enter maintenance</);
  assert.ok(drawer.indexOf("<h4>Membership</h4>") < drawer.indexOf('class="cldraweract"'));
});

test("a stale node heartbeat says so rather than claiming a network probe", () => {
  const ui = sandbox();
  const row = nodeRow(ui, node("node-c", 3, "voter", { reachable: false }));
  assert.match(row, /stale/);
});

test("loopback is shown as localhost beside the short hostname", () => {
  const ui = sandbox();
  const row = nodeRow(ui, 
    node("node-local", 1, "voter", {
      hostname: "media-1",
      advertised_host: "localhost",
    }),
  );
  assert.match(row, /class="clhost">media-1/);
  assert.match(row, /class="clsub">localhost<\/div>/);
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
  // Top to bottom: header, strip, the (empty) alert mount, the nodes table
  // with a drawer per node, settings beside the store, maintenance, the log.
  const order = ['class="sethead"', 'class="clcard clstrip"', 'id="cluster-operations"', 'id="cluster-nodes"',
    'id="cluster-settings"', 'id="cluster-database"', 'id="cluster-maintenance"', 'id="cluster-log"'];
  const at = order.map((marker) => html.indexOf(marker));
  assert.ok(at.every((index) => index !== -1), `missing a region: ${order.filter((_, i) => at[i] === -1)}`);
  assert.deepEqual([...at].sort((x, y) => x - y), at, "regions are out of order");
  assert.match(html, /<table class="clnodetable">[\s\S]*<tbody id="cluster-node-list">/);
  assert.equal((html.match(/<tr class="clrow/g) || []).length, 3);
  assert.equal((html.match(/<tr class="setdrawer" id="cldrawer-[^"]*" data-node="[^"]*" hidden>/g) || []).length, 3,
    "every drawer ships closed");
  assert.match(html, /<h4>Membership<\/h4>[\s\S]*<h4>Raft &amp; WAL<\/h4>[\s\S]*<h4>Snapshot &amp; media<\/h4>/);
  // Healthy means no alert at all.
  assert.equal(html.includes('class="clalert'), false);
  // Cluster-wide actions are the Maintenance card's rows; the membership
  // controls are the nodes card's and the local drawer's own expansions.
  assert.match(html, /data-row="restart"[\s\S]*data-row="election"[\s\S]*data-row="diagnostics"/);
  assert.match(html, /id="clexp-add" hidden/);
  assert.match(html, /id="clexp-leave" hidden/);
  // Nothing on this page scrolls inside itself any more.
  for (const gone of ["clnodes-capped", "--clnodes-h", "cluster-health", "clrail", "cltab", "cldb-toggle"])
    assert.equal(html.includes(gone), false, `${gone} is still rendered`);
});

test("a row click opens its drawer in place and records it as module state", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [node("node-a", 1, "voter"), node("node-b", 2, "voter")]);
  const drawer = { hidden: true };
  const chev = { textContent: "▸" };
  const attributes = {};
  const classes = new Set(["clrow"]);
  const row = { classList: { toggle: (name, on) => (on ? classes.add(name) : classes.delete(name)) } };
  const button = {
    getAttribute: (name) => (name === "data-node" ? "node-b" : null),
    setAttribute: (name, value) => { attributes[name] = value; },
    querySelector: () => chev,
  };
  const toggle = new Function(
    "document",
    `let CLUSTER_OPEN_NODES=[];
     ${shippedSource("toggleClusterNode")}
     return {toggle:toggleClusterNode,open:()=>CLUSTER_OPEN_NODES};`,
  )({ getElementById: (id) => (id === "cldrawer-node-b" ? drawer : id === "clrow-node-b" ? row : null) });
  toggle.toggle(button);
  assert.deepEqual(toggle.open(), ["node-b"]);
  assert.equal(drawer.hidden, false);
  assert.equal(attributes["aria-expanded"], "true");
  assert.ok(classes.has("setopen"));
  assert.equal(chev.textContent, "▾");
  toggle.toggle(button);
  assert.deepEqual(toggle.open(), []);
  assert.equal(drawer.hidden, true);
  assert.equal(attributes["aria-expanded"], "false");
  // A drawer is not a credential surface and not layout: nothing is stored.
  for (const sink of ["localStorage", "sessionStorage", "renderSettings"])
    assert.equal(shippedSource("toggleClusterNode").includes(sink), false, sink);
  // And the module state is what the painter reads.
  assert.match(sandbox({ open: ["node-b"] }).clusterNodesCard(cluster, null), /id="cldrawer-node-b" data-node="node-b">/);
  assert.match(ui.clusterNodesCard(cluster, null), /id="cldrawer-node-b" data-node="node-b" hidden>/);
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
    const card = nodeRow(ui, target, nodeId, ops, cluster);
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
  const html = ui.clusterAlerts(cluster, maintenanceOps("node-b", { drained: false }));
  assert.match(html, /Maintenance in progress on node-b\./);
  assert.match(html, /Target acknowledged the replicated work fence/);
  assert.match(html, /Direct process work drained \(1 local operation\)/);
  assert.match(html, /fence survives a process restart/i);
  assert.match(html, /<button class="sm" disabled[^>]*>Resume service/);

  cluster.nodes[1].active_media_sessions = 0;
  cluster.nodes[1].maintenance_ready = true;
  cluster.local_node_id = "node-b";
  const ready = ui.clusterAlerts(cluster, maintenanceOps("node-b"));
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
  const clusterOf = (local) => ({
    ...status("high_availability", [node("node-a", 1, "voter", { is_leader: true }), maintained, node("node-c", 3, "voter")]),
    local_node_id: local,
  });
  // Seen from another node: the alert names the target and its Resume is
  // disabled; that node's drawer offers no maintenance control at all.
  const remote = ui.clusterAlerts(clusterOf("node-a"), maintenanceOps("node-b"));
  assert.match(remote, /Open the maintenance target directly to resume it/);
  assert.match(remote, /<button class="sm" disabled[^>]*>Resume service/);
  assert.doesNotMatch(nodeRow(ui, maintained, "node-a", maintenanceOps("node-b"), clusterOf("node-a")), /Resume service/);

  const local = nodeRow(ui, maintained, "node-b", maintenanceOps("node-b"), clusterOf("node-b"));
  assert.match(local, /<button class="sm"  onclick='setNodeMaintenance\(&quot;node-b&quot;,false\)'>Resume service/);

  const admission = nodeRow(ui, maintained, "node-b", maintenanceOps("node-b", { admissions: 1 }), clusterOf("node-b"));
  assert.match(admission, /<button class="sm" disabled >Resume service/);

  const recoveredLeader = { ...maintained, is_leader: true, maintenance_ready: false };
  const handoff = ui.clusterAlerts(
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
  assert.equal(panel.includes('id="clexp-add"'), false);
  assert.equal(panel.includes("Create a join token"), false);
  assert.match(panel, /A membership change is already in flight on node-b/);
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
  const html = ui.clusterAlerts(cluster, null);
  assert.match(html, /Restore the maintenance target first: node-b/);
  assert.match(html, /durable work fence is still active/i);
  assert.match(html, /Force election unavailable without quorum/);

  cluster.nodes[2].reachable = true;
  cluster.recovery = {
    ...cluster.recovery,
    quorum_available: true,
    reachable_voters: 2,
  };
  const leaderless = ui.clusterAlerts(cluster, null);
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
  const local = nodeRow(ui, node("node-a", 1, "voter"), "node-a");
  const peer = nodeRow(ui, node("node-b", 2, "voter"), "node-a");
  assert.match(local, /This node/);
  assert.match(local, />Leave cluster…</);
  assert.equal(local.includes("removeNode"), false);
  assert.match(peer, />Remove permanently…</);
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

test("Settings itself still turns a non-admin away before any of this loads", async () => {
  const source = shippedSource("viewSettings");
  const viewSettings = source.slice(0, source.indexOf("\n}") + 2);
  assert.ok(viewSettings.endsWith("\n}"), "the shipped route function is complete");
  const execute = new Function("ME", "location", "settingsRouteTab", "viewLocalRemoteSettings", "api",
    `${viewSettings}; return viewSettings(1);`);
  for (const tab of ["cluster", "maintenance", "developer"]) {
    const location = { hash: `#/settings/${tab}` };
    const local = [];
    await execute({ is_admin: false }, location, () => tab,
      generation => local.push(generation), () => { throw new Error("non-admin must not load admin settings"); });
    assert.equal(location.hash, tab === "developer" ? "#/settings/developer" : "#/");
    assert.deepEqual(local, tab === "developer" ? [1] : []);
  }
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
    "clusterNodesCard",
    "toggleClusterExpansion",
    "setCard",
    "cardHead",
    "devReq",
    "devReadinessRow",
    "devReadinessPill",
    "devReadinessEvidence",
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
  assert.match(render, /if\(!isSettingsRoute\(h\)\) forgetJoinToken\(\)/);
  // A section switch stays inside Settings, so render() does not drop the
  // token there; viewSettings does, on the section change itself.
  const view = shippedSource("viewSettings");
  assert.match(view, /if\(SETTINGS_SHOWN_TAB!==tab\) forgetJoinToken\(\)/);
  const isSettingsRoute = new Function(`${shippedSource("isSettingsRoute")}; return isSettingsRoute;`)();
  for (const route of ["#/settings", "#/admin", "#/settings/cluster", "#/settings/libraries"])
    assert.equal(isSettingsRoute(route), true, route);
  for (const route of ["#/", "#/activity", "#/settingsx", "#/item/settings", "", undefined])
    assert.equal(isSettingsRoute(route), false, String(route));
});

// ---- wiring ---------------------------------------------------------------

test("the Cluster tab is registered and dispatched", () => {
  assert.match(SHIPPED_UI, /\["cluster","Cluster"\]/);
  assert.match(SHIPPED_UI, /if\(tab==="cluster"\)\s*return clusterPanel\(d\)/);
  // The active-tab manifest fetches this roster on first Cluster open; it is
  // absent from every other tab's dependency wave.
  assert.match(SHIPPED_UI, /cluster:\{required:\["cluster"\],secondary:\["clusterOps","developerReadiness","settings"\]\}/);
  assert.match(SHIPPED_UI, /cluster:\(\)=>api\("\/cluster\/nodes"\)/);
  assert.match(SHIPPED_UI, /clusterOps:\(\)=>api\("\/cluster\/status",\{keepSessionOn401:true\}\)/);
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
  assert.match(renderSettings, /if\(!isSettingsRoute\(location\.hash\)\) return/);
  for (const handler of ["loadCluster", "mintJoinToken", "removeNode"]) {
    assert.match(
      shippedSource(handler),
      /renderSettings\(\)|repaintClusterPreserving\(renderSettings\)/,
      `${handler} no longer renders through the guarded Settings choke point`,
    );
  }
});

test("the web app calls only the cluster endpoints the node API ships", () => {
  const called = new Set();
  const CALL = /api\(\s*([`"'])(\/cluster[^`"']*)\1/g;
  for (const [, , route] of SHIPPED_UI.matchAll(CALL)) {
    // Query filters do not change the route; template holes in the path are
    // node or durable-job IDs rather than additional API endpoints.
    called.add(route.split("?")[0].replace(/\$\{[^}]*\}/g, "<id>"));
  }
  assert.deepEqual(
    [...called].sort(),
    [
      "/cluster/election",
      "/cluster/jobs",
      "/cluster/jobs/<id>",
      "/cluster/jobs/<id>/cancel",
      "/cluster/jobs/<id>/retry",
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
      "/cluster/work/storage-domains",
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
  assert.match(html, /<h2>Replicated database<\/h2>/);
  assert.match(html, /id="cluster-database"/);
  assert.match(html, /<dt>Quorum commit<\/dt><dd class="num">482191<\/dd>/);
  assert.match(html, /<dt>Leader · term<\/dt><dd class="num">node-a <span class="muted">this node<\/span> · 81<\/dd>/);
  assert.match(html, /<dt>Protocol<\/dt><dd class="num">5–6<\/dd>/);
  assert.match(html, /<dt>Snapshots<\/dt><dd class="num">build 9 ok \/ 0 error · install 2 ok \/ 0 error<\/dd>/);
  assert.match(html, /<dt>Database size<\/dt><dd class="num">2\.1 MB<\/dd>/);
  // The watermark and the applied index have to come from one sample or their
  // difference is not a lag, so both are the direct status's, not the
  // membership projection's separately-fetched index.
  assert.match(html, /<dt>Applied on this node<\/dt><dd class="num">482191 · lag 0<\/dd>/);
  // The store's facts sit in the store's own card, never inside the nodes
  // table: the ledger's first reading comes after the card opens.
  assert.ok(
    html.indexOf("<dt>Quorum commit</dt>") > html.indexOf('id="cluster-database"'),
    "the quorum watermark is rendered outside the database card",
  );
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
  assert.equal(rows.get("Applied on this node"), "482191 · lag 4");
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
  assert.equal(rows.get("Applied on this node"), "482100 · lag 91");
  assert.equal(rows.get("Protocol"), "5–6");
  // The leader row is still the leader's: naming the elected leader is a
  // membership fact, and it must not drag its readings along with it.
  assert.match(rows.get("Leader · term"), /^node-b · 81$/);
});

test("database readings say unknown instead of inventing a number", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [node("node-a", 1, "voter", { is_leader: true })]);
  const rows = new Map(ui.clusterDatabaseRows(ui.env, cluster, REPLICATION, null).map((row) => [row[0], row[1]]));
  assert.match(rows.get("Leader · term"), / · unknown$/);
  assert.equal(rows.get("Quorum commit"), "unknown");
  assert.match(rows.get("Applied on this node"), / · lag unknown$/);
  assert.equal(rows.get("Protocol"), "unknown");
  assert.equal(rows.get("Database size"), "unknown");
  // The membership projection is a different source and is still present, so
  // the applied index must not be blanked along with the direct sample.
  assert.match(rows.get("Applied on this node"), /^912/);
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

test("standalone Cluster settings contain only the disabled message", () => {
  for(const d of [{}, {cluster:{nodes:[{node_id:"local",is_leader:true}],capacity:{voting_nodes:1}}}]){
    const html=sandbox({clustered:false}).clusterPanel(d);
    assert.match(html,/<h1>Cluster<\/h1><p>Clustering is not enabled\.<\/p>/);
    assert.doesNotMatch(html,/button|input|select|textarea|Maintenance|Replicated|cluster-dashboard|cluster-operations/);
  }
});

test("configured cluster settings remain visible during membership recovery", () => {
  const html=sandbox({clustered:true}).clusterPanel({cluster:{unavailable:true,code:"membership_unavailable"}});
  assert.doesNotMatch(html,/Clustering is not enabled/);
  assert.match(html,/class="clpage"/);
});

test("a SQLite membership refusal shows only that clustering is not enabled", () => {
  const html=sandbox().clusterPanel({cluster:{unavailable:true,code:"membership_unavailable",nodes:[]}});
  assert.match(html,/Clustering is not enabled\./);
  assert.doesNotMatch(html,/button|input|select|textarea|SQLite|voter|Replicated|Status unavailable/);
});

// ---- folding, and remembering it ------------------------------------------
function fakeStorage(seed = {}) {
  const map = new Map(Object.entries(seed));
  return {
    getItem: (key) => (map.has(key) ? map.get(key) : null),
    setItem: (key, value) => map.set(key, String(value)),
    read: (key) => map.get(key),
  };
}

test("a refusal is kept in the alert slot, not spent on a toast", () => {
  const ui = sandbox({
    refusal: { node_id: "node-b", code: "node_owns_offline_work", message: "" },
  });
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterPanel({ cluster, sys: { replication: REPLICATION } });
  // In the alert slot, above the table, with its sentence and a way to clear
  // it — not a toast, and not a tab nobody opens.
  const alert = /<div class="clalert bad" role="alert">[\s\S]*?<\/div><\/div>/.exec(html);
  assert.ok(alert, "the refusal is an alert");
  assert.match(alert[0], /Cluster operation for node-b was refused/);
  assert.match(alert[0], /offline download/i);
  assert.match(alert[0], /onclick="dismissClusterRefusal\(\)">Dismiss</);
  assert.ok(html.indexOf("was refused") < html.indexOf('id="cluster-nodes"'));
  assert.match(shippedSource("dismissClusterRefusal"), /CLUSTER_REFUSAL=null; repaintClusterPreserving\(renderSettings\)/);
});

// ---- actions carry their preconditions ---------------------------------------
// A refusal is readable before the click. These pin both halves of that: the
// precondition is stated, and the control is actually disabled when it does
// not hold — a reason beside a live button is worse than no reason at all.
// Every action is keyed by what it does, wherever the page draws it: the nodes
// card (add), a node's drawer (promote, remove, maintenance, restart, leave)
// or the Maintenance card (rolling restart, election).

function railRows(ui, cluster, ops) {
  const rows = new Map([["Add a node", ui.clusterAddNodeAction(ui.env, cluster)]]);
  const names = { promote: "Promote", remove: "Remove", maintenance: "Enter maintenance", resume: "Resume service",
    restart: "Prepare for restart", leave: "Leave this cluster" };
  for (const n of cluster.nodes) {
    for (const action of ui.clusterNodeActions(ui.env, cluster, ops, n)) {
      const key = action.id === "promote" || action.id === "remove" ? `${names[action.id]} ${n.hostname}` : names[action.id];
      rows.set(key, { ...action, action: action.html });
    }
  }
  for (const row of ui.clusterMaintenanceRows(ui.env, cluster, ops)) {
    const key = { restart: "Rolling restart", election: "Force a leader election", diagnostics: "Diagnostics" }[row.id];
    rows.set(key, { ...row, blocked: /disabled/.test(row.action), reason: row.text });
  }
  return rows;
}

test("every action carries its precondition", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
    node("node-d", 4, "learner", { bounded_read_ready: true, voter_storage_ready: false,
      storage_headroom_bytes: 22 * 1000 * 1000 * 1000 }),
  ]);
  const rows = railRows(ui, cluster, operationStatus(cluster));
  assert.equal(rows.get("Add a node").blocked, false);

  // The learner is caught up but its filesystem has not proved the reserve, so
  // the action says which of the two preflights failed and what to do about
  // it — before the promotion is attempted and refused.
  const promote = rows.get("Promote node-d");
  assert.equal(promote.blocked, true);
  assert.match(promote.reason, /durable free-space reserve/);
  assert.match(promote.reason, /22 GB free/);
  assert.match(promote.action, /disabled/);

  // Leaving is permanent and is an expansion of its own action.
  const leave = rows.get("Leave this cluster");
  assert.equal(leave.destructive, true);
  assert.equal(leave.expand, "leave");
  assert.match(ui.leavePanel("node-a"), /fresh data directory/);
});

test("a learner that has proved both preflights is offered promotion", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
    node("node-d", 4, "learner", { bounded_read_ready: true, voter_storage_ready: true }),
  ]);
  const promote = railRows(ui, cluster, operationStatus(cluster)).get("Promote node-d");
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
  const waiting = railRows(ui, catching, operationStatus(catching)).get("Promote node-d");
  assert.equal(waiting.blocked, true);
  assert.match(waiting.reason, /zero-lag apply proof/);
  assert.match(waiting.action, /disabled/);
});

test("no action offers what the server would refuse", () => {
  const ui = sandbox();
  // Two voters: removal drops to one and is refused, and the action says so
  // instead of leaving it to the failed attempt.
  const pair = status("degraded_reconfiguration", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
  ]);
  const two = railRows(ui, pair, operationStatus(pair)).get("Remove node-b");
  assert.equal(two.blocked, true);
  // The server's bar is three voters, not "not two".
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
  assert.equal(locked.get("Promote node-d").blocked, true);
  assert.equal(locked.get("Force a leader election").blocked, true);
  // One planned outage at a time: this node may not fence itself while
  // another is fenced, and only the fenced node can lift its own fence.
  assert.equal(locked.get("Enter maintenance").blocked, true);
  // The reason is the server's own blocker, named by hostname.
  assert.equal(locked.get("Enter maintenance").reason, "maintenance already active on node-b.");
  assert.ok(!locked.has("Resume service"));
  assert.match(ui.clusterAlerts(fenced, operationStatus(fenced)),
    /title="Open the maintenance target directly to resume it\." [^>]*>Resume service/);
  // And the leader cannot be removed from anywhere but its own screen.
  const remote = status("high_availability", [
    node("node-a", 1, "voter"),
    node("node-b", 2, "voter", { is_leader: true }),
    node("node-c", 3, "voter"),
  ]);
  const leader = railRows(ui, remote, operationStatus(remote)).get("Remove node-b");
  assert.equal(leader.blocked, true);
  assert.match(leader.reason, /node-b is the current leader and can only leave from its own screen/);
});

test("the credential and the leave each have exactly one control", () => {
  // A join token is shown exactly once. Two places that mint one is two places
  // to leak it, so the add action opens the panel that owns it, and so does
  // the leave.
  const ui = sandbox({ expanded: ["add", "leave"] });
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterPanel({ cluster, clusterOps: operationStatus(cluster), sys: { replication: REPLICATION } });
  assert.equal((html.match(/mintJoinToken\(this\)/g) || []).length, 1);
  assert.equal((html.match(/leaveCluster\(this,/g) || []).length, 1);
  const add = html.slice(html.indexOf('id="clexp-add"'));
  assert.ok(add.indexOf("mintJoinToken(this)") < add.indexOf("</div></div>") + 400, "the mint lives in the add expansion");
  assert.match(html, /onclick="toggleClusterExpansion\('add'\)"/);
  assert.match(html, /onclick="toggleClusterExpansion\('leave'\)"/);
  assert.match(html.slice(html.indexOf('id="clexp-leave"')), /^id="clexp-leave">[\s\S]*?leaveCluster\(this,/);
  assert.doesNotMatch(PANEL.clusterNodeActions.toString(), /localStorage/);
});

test("a stale roster cannot make an action claim a verdict", () => {
  // With no direct status there is no restart verdict to report, so nothing
  // guesses at safety.
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const rows = railRows(ui, cluster, null);
  assert.equal(rows.get("Rolling restart").blocked, true);
  assert.match(rows.get("Rolling restart").reason, /Collecting the direct rollout verdict/);
  assert.equal(rows.get("Prepare for restart").blocked, true);
  // And maintenance is refused while the preflight has not answered.
  const maintain = rows.get("Enter maintenance");
  assert.equal(maintain.blocked, true);
  assert.match(maintain.reason, /preflight has not answered/);
});

// ---- the layout holding still ----------------------------------------------

test("the roster grows with its table and nothing scrolls inside the page", () => {
  // The fixed-height node scroller is gone: the table is a table, and an open
  // drawer pushes the page down rather than a keyhole scrolling. A hidden
  // drawer stays hidden even where the phone rules make rows blocks.
  const ui = sandbox();
  const three = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterPanel({ cluster: three, sys: { replication: REPLICATION } });
  assert.match(html, /<tbody id="cluster-node-list">/);
  assert.doesNotMatch(html, /--clnodes-h|clnodes-capped/);
  assert.doesNotMatch(SHIPPED_UI, /clnodes-capped|--clnodes-h/);
  assert.doesNotMatch(SHIPPED_UI, /\.clnodetable[^{]*\{[^}]*(max-height|overflow-y)/);
  assert.match(SHIPPED_UI, /\.clnodetable tr\[hidden\]\{display:none\}/);
});

test("an action's verdict reaches the markup, not just the model", () => {
  // Every precondition can be right in the model while the painter draws a
  // live button, so pin the rendered drawer.
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
    node("node-d", 4, "learner", { bounded_read_ready: false, voter_storage_ready: false }),
  ]);
  const learner = ui.clusterNodeDrawer(cluster.nodes[3], cluster, operationStatus(cluster));
  assert.match(learner, /<button class="ghost sm" disabled >Promote to voter<\/button><span class="clwhy">It has not published a fresh zero-lag apply proof yet/);
  const local = ui.clusterNodeDrawer(cluster.nodes[0], cluster, operationStatus(cluster));
  assert.match(local, /<button class="ghost sm"  onclick='setNodeMaintenance\(&quot;node-a&quot;,true\)'>Enter maintenance<\/button>(?!<span class="clwhy">)/);
  assert.match(local, /<button class="ghost sm" disabled >Prepare for restart<\/button><span class="clwhy">The safe candidate is node-b/);
  assert.match(local, /<span class="clactitem clpush"><button class="danger sm" aria-controls="clexp-leave" aria-expanded="false"/);
});

test("entering maintenance is offered only when the preflight approved it", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const approved = operationStatus(cluster);
  const ok = railRows(ui, cluster, approved).get("Enter maintenance");
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
  const no = railRows(ui, cluster, refused).get("Enter maintenance");
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
  const rows = railRows(ui, cluster, operationStatus(cluster));
  const elsewhere = rows.get("Prepare for restart");
  assert.equal(elsewhere.blocked, true);
  assert.match(elsewhere.reason, /candidate is node-b/);
  assert.match(elsewhere.action, /disabled/);
  assert.doesNotMatch(rows.get("Rolling restart").action, /prepareLocalRestart/);

  const here = operationStatus(cluster);
  here.verdict.candidate_node_id = cluster.local_node_id;
  const mine = railRows(ui, cluster, here);
  assert.equal(mine.get("Prepare for restart").blocked, false);
  assert.match(mine.get("Prepare for restart").action, /prepareLocalRestart/);
  assert.match(mine.get("Rolling restart").action, /prepareLocalRestart/);
  assert.doesNotMatch(mine.get("Rolling restart").action, /disabled/);
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
  // The alert says membership changes are locked; the actions underneath it
  // must not be live.
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", { reachable: false }),
    node("node-c", 3, "voter", { reachable: false }),
  ]);
  cluster.recovery = { required: true, quorum_available: false, reachable_voters: 1, required_voters: 2, leader_elected: false };
  const rows = railRows(ui, cluster, operationStatus(cluster));
  for (const title of ["Add a node", "Remove node-b", "Leave this cluster"]) {
    assert.equal(rows.get(title).blocked, true, `${title} is still live under recovery`);
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
  assert.equal(rows.get("Remove node-b").blocked, true);
  assert.equal(rows.get("Remove node-c").blocked, false);
  // The same roster read from the learner: now a voter is the removable one,
  // and one voter is below the server's bar just as two is.
  const fromLearner = status("degraded_reconfiguration", [
    node("node-c", 3, "learner", { bounded_read_ready: true }),
    node("node-a", 1, "voter"),
  ]);
  const single = railRows(ui, fromLearner, operationStatus(fromLearner)).get("Remove node-a");
  assert.equal(single.blocked, true);
  assert.match(single.reason, /at least three voters and this cluster has 1/);
});

test("the page is one column of regions, with the alerts above the table", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = ui.clusterPanel({ cluster, clusterOps: { unavailable: true, message: "timeout" }, sys: { replication: REPLICATION } });
  // The status failure is an alert above the table, carrying its own retry.
  assert.ok(html.indexOf('id="cluster-operations"') < html.indexOf('id="cluster-nodes"'));
  assert.match(html, /<div id="cluster-operations"><div class="clalert" role="status"><div><b>Do not restart another voter\.<\/b>[\s\S]*timeout/);
  assert.equal((html.match(/id="cluster-operations"/g) || []).length, 1);
  // Two two-up rows: settings beside the store, maintenance beside backup.
  assert.equal((html.match(/<div class="cltwo">/g) || []).length, 2);
  // One election dialog, once — inside the Maintenance card — whatever opens it.
  assert.equal((html.match(/id="clelection"/g) || []).length, 1);
  const recovering = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", { reachable: false }),
    node("node-c", 3, "voter", { reachable: false }),
  ]);
  recovering.recovery = { required: true, quorum_available: true, reachable_voters: 2, required_voters: 2, leader_elected: false };
  const lost = ui.clusterPanel({ cluster: recovering, sys: { replication: REPLICATION } });
  assert.equal((lost.match(/id="clelection"/g) || []).length, 1);
  assert.ok(lost.indexOf("Leader election required") < lost.indexOf('id="cluster-nodes"'));
});

test("the controls the page routes to actually do something", () => {
  // One line each, and deletable with the suite green: the credential panel
  // would open below the fold.
  assert.match(shippedSource("toggleClusterExpansion"), /scrollIntoView\(\{block:"nearest"\}\)/);
});

// ---- one credential surface ------------------------------------------------

test("the credential is an expansion of the action that decides it", () => {
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const ops = operationStatus(cluster);
  const collapsed = sandbox().clusterNodesCard(cluster, ops);
  // Shipped state: the panel exists in the markup and is not showing. A fresh
  // page must never open a credential surface by itself.
  assert.match(collapsed, /<div class="clexpansion" id="clexp-add" hidden>/);
  assert.match(collapsed, /aria-controls="clexp-add" aria-expanded="false"[^>]*>\+ Add node</);
  assert.match(collapsed, /<div class="clexpansion" id="clexp-leave" hidden>/);

  const open = sandbox({ expanded: ["add"] }).clusterNodesCard(cluster, ops);
  assert.match(open, /<div class="clexpansion" id="clexp-add">/);
  assert.match(open, /aria-controls="clexp-add" aria-expanded="true"[^>]*>Hide</);
  // Opening one does not open the other.
  assert.match(open, /<div class="clexpansion" id="clexp-leave" hidden>/);
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
  const card = ui.clusterNodesCard(cluster, operationStatus(cluster));
  // Rendered from state rather than captured from the DOM, so the 15s status
  // repaint cannot pull a half-read credential shut underneath someone.
  assert.match(card, /plxjoin:v1:aaaa:bbbb/);
  assert.equal((card.match(/plxjoin:v1:aaaa:bbbb/g) || []).length, 1, "shown once means once");
  assert.match(card, /Shown once/);

  // The expansion is module state, and the same three exits that drop the token
  // drop it too — so returning to this tab starts collapsed, with nothing to
  // reopen onto.
  const forget = shippedSource("forgetJoinToken");
  assert.match(forget, /CLUSTER_TOKEN=null/);
  assert.match(forget, /CLUSTER_RAIL_EXPANDED=\[\]/);
  const toggle = shippedSource("toggleClusterExpansion");
  assert.doesNotMatch(toggle, /forgetJoinToken|CLUSTER_TOKEN/);
  for (const sink of ["localStorage", "sessionStorage"]) {
    assert.equal(toggle.includes(sink), false, `the expansion reaches ${sink}`);
  }
  // The page keeps nothing in browser storage at all any more: no fold for
  // anything credential-adjacent to ride into.
  for (const name of ["clusterFoldRead", "clusterFoldWrite", "clusterFoldKey"]) assert.equal(PANEL[name], undefined, name);
  assert.equal(SHIPPED_UI.includes("plurx.cluster.fold"), false);
});

test("the expansion is a toggle over module state, and writes nothing down", () => {
  const storage = fakeStorage({ unrelated: "kept" });
  const panel = { hidden: true, scrollIntoView() {} };
  const document = { getElementById: (id) => (id === "clexp-add" ? panel : null) };
  const harness = new Function(
    "document", "renderSettings", "repaintClusterPreserving", "localStorage",
    `let CLUSTER_RAIL_EXPANDED=[];
     ${shippedSource("toggleClusterExpansion")}
     return {toggle:toggleClusterExpansion,state:()=>CLUSTER_RAIL_EXPANDED};`,
  )(document, () => {}, (paint) => paint(), storage);

  harness.toggle("add");
  assert.deepEqual(harness.state(), ["add"]);
  harness.toggle("add");
  assert.deepEqual(harness.state(), [], "toggling twice closes it again");
  assert.equal(storage.read("unrelated"), "kept");
  assert.equal(storage.read("plurx.cluster.fold"), undefined, "the expansion left nothing behind in browser storage");
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
  const open = ui.clusterNodeDrawer(cluster.nodes[0], cluster, operationStatus(cluster));
  assert.match(open, /<div class="clexpansion" id="clexp-leave">/, "leave expands too, not just add");
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
  const lockedHtml = sandbox({ expanded: ["leave"] }).clusterNodeDrawer(recovering.nodes[0], recovering, operationStatus(recovering));
  assert.equal(lockedHtml.includes('id="clexp-leave"'), false, "a blocked action has no reachable control");
  // …and no aria-controls naming a panel that is not there.
  assert.match(lockedHtml, /<button class="danger sm" disabled onclick="toggleClusterExpansion\('leave'\)">/);
  assert.equal(lockedHtml.includes('aria-controls="clexp-leave"'), false);
  assert.equal(lockedHtml.includes("leaveCluster(this,"), false);


  // Maintenance is a cluster-wide conflict for membership changes, so a fenced
  // node blocks a leave and the row says which node to resume.
  const fenced = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", { maintenance: true, maintenance_acknowledged: true }),
    node("node-c", 3, "voter"),
  ]);
  const fencedRow = railRows(ui, fenced, operationStatus(fenced)).get("Leave this cluster");
  assert.equal(fencedRow.blocked, true);
  assert.match(fencedRow.reason, /Maintenance is active on node-b/);

  // The server refuses a voter self-leave below three voters with the same
  // arithmetic the Remove row already reports. A removal pending on some OTHER
  // node is deliberately NOT a blocker — that fence is per-node, and refusing
  // here would refuse something the server allows.
  const pair = status("degraded_reconfiguration", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
  ]);
  const small = railRows(ui, pair, operationStatus(pair)).get("Leave this cluster");
  assert.equal(small.blocked, true);
  assert.match(small.reason, /at least three voters remain, and this cluster has 2/);

  const pendingElsewhere = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter", { removal_pending: true }),
  ]);
  const stillFree = railRows(ui, pendingElsewhere, operationStatus(pendingElsewhere)).get("Leave this cluster");
  assert.equal(stillFree.blocked, false, "another node's pending removal does not fence this one");

  // A learner leaving is not held to the voter arithmetic — the server sends it
  // down the learner path.
  const learnerLocal = status("high_availability", [
    node("node-w", 9, "learner", { bounded_read_ready: true, voter_storage_ready: true }),
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
  ]);
  const learnerRow = railRows(ui, learnerLocal, operationStatus(learnerLocal)).get("Leave this cluster");
  assert.equal(learnerRow.blocked, false);

  // The join control is only ever rendered when Add is not blocked, so it
  // carries no disabled branch of its own to drift from the action's reason.
  assert.match(shippedSource("clusterNodesCard"), /add\.blocked\?"":`<div class="clexpansion" id="clexp-add"[^`]*\$\{joinPanel\(\)\}/);
  assert.equal(shippedSource("joinPanel").includes("disabled"), false);
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
      // Rendered, but excluded: a WAL fsync alone is not news worth rewriting
      // the screen for, and the commit watermark beside it is in the projection.
      if (row.status.wal && row.status.wal.snapshot)
        row.status.wal.snapshot.last_sync_unix_ms =
          (row.status.wal.snapshot.last_sync_unix_ms || 0) + 60_000;
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
  // The aggregate's top-level `membership` is the committed roster again: the
  // panel reads one stable field from it and renders the rest from
  // /cluster/nodes, so its lag and session counts must not repaint anything.
  const roster = JSON.parse(JSON.stringify(later));
  if (roster.membership && roster.membership.nodes && roster.membership.nodes.length) {
    roster.membership.nodes[0].apply_lag_entries = 41;
    roster.membership.nodes[0].active_media_sessions = 7;
    assert.equal(ui.clusterOpsProjection(roster), ui.clusterOpsProjection(first));
  }
  const relocal = JSON.parse(JSON.stringify(later));
  relocal.membership = Object.assign({}, relocal.membership, { local_node_id: "somewhere-else" });
  assert.notEqual(ui.clusterOpsProjection(relocal), ui.clusterOpsProjection(first),
    "which node this is remains part of the projection");
  // …but the per-node membership record inside nodes[] identifies the rows, and
  // a node joining, leaving or being renamed IS news.
  const renamed = JSON.parse(JSON.stringify(later));
  renamed.nodes[0].membership.hostname = "renamed-host";
  assert.notEqual(ui.clusterOpsProjection(renamed), ui.clusterOpsProjection(first));

  // Key order in the response is not a change either.
  const shuffled = JSON.parse(JSON.stringify(first));
  shuffled.nodes = shuffled.nodes.map((row) =>
    Object.fromEntries(Object.entries(row).reverse()),
  );
  assert.equal(ui.clusterOpsProjection(shuffled), ui.clusterOpsProjection(first));
});

test("transport observer age advances the repaint projection", () => {
  const ui = sandbox();
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const first = operationStatus(cluster);
  first.nodes[0].transport = {
    observing_node_id: 1,
    observations: [transportObservation("transferring", { sample_age_ms: 1_000 })],
  };
  const later = JSON.parse(JSON.stringify(first));
  later.nodes[0].transport.observations[0].sample_age_ms = 61_000;

  assert.notEqual(
    ui.clusterOpsProjection(later),
    ui.clusterOpsProjection(first),
    "rendered transport observer age must repaint even when all other evidence is identical",
  );
});

// A settingsTick harness that can actually run the cluster branch: the tick
// itself is shipped source, everything it reaches for is supplied here.
// The tick reads the roster on the status gate too. Unless a test asks to
// answer it by hand (`manualRoster`), the roster read is answered at once with
// the roster the panel already holds, and only the status reads are queued.
// The roster read answers on a microtask; let the status request follow it.
const tickSettle = () => new Promise((resolve) => setImmediate(resolve));
function tickHarness({ cluster, ops, now, clustered=true, manualRoster=false }) {
  const requests = [];
  const painted = [];
  const paintedTransport = [];
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
    "refreshClusterLogs", "paintTrakt", "recordRender", "PlurxClusterPanel", "clock", "dialogs",
    `let PAGE_RENDER_GENERATION=1,AUTH_GENERATION=1,SETTINGS_TICKING=null,TRAKT_EDIT=false,
       TRAKT=null,CLUSTER_LOADED=true,CLUSTER_OPS_FETCHED_AT=0,
       SETTINGS_DATA=${JSON.stringify({ cluster, clusterOps: ops })},SETTINGS_LOADED=new Set(["cluster","clusterOps"]);
     const SERVER={cluster_enabled:${JSON.stringify(clustered)}};
     ${shippedSource("settingsClusterEnabled")}
     const cacheTrakt=(value)=>value;
     const Date={now:clock};
     const performance={now:clock};
     const CLUSTER_OPS_RECEIVED_AT=new WeakMap();
     ${shippedSource("clusterOpsReceived")}
     ${shippedSource("clusterOpsElapsed")}
     clusterOpsReceived(SETTINGS_DATA.clusterOps);
     const renderSettings=()=>recordRender(
       SETTINGS_DATA.clusterOps,clusterOpsElapsed(SETTINGS_DATA.clusterOps));
     ${shippedSource("isSettingsRoute")}
     ${shippedSource("clusterOpsInterval")}
     ${shippedSource("clusterOpsStamp")}
     ${shippedSource("clusterOpsDue")}
     ${shippedSource("clusterRepaintDeferred")}
     ${shippedSource("repaintClusterPreserving")}
     ${shippedSource("clusterControlValues")}
     ${shippedSource("clusterRestoreControlValues")}
     ${shippedSource("patchClusterReadingAge")}
     ${shippedSource("clenv")}
     ${shippedSource("esc")}
     ${shippedSource("fmtAgo")}
     ${shippedSource("fmtBytes")}
     ${shippedSource("settingsTick")}
     return {settingsTick,stamped:()=>CLUSTER_OPS_FETCHED_AT,ops:()=>SETTINGS_DATA.clusterOps,
       cluster:()=>SETTINGS_DATA.cluster,
       openDialog:(open)=>{dialogs.length=0;if(open)dialogs.push({});}};`,
  )(
    document,
    { hash: "#/settings" },
    (url) => (url === "/cluster/nodes" && !manualRoster
      ? Promise.resolve(JSON.parse(JSON.stringify(cluster)))
      : new Promise((resolve, reject) => requests.push({ url, resolve, reject }))),
    () => "cluster",
    () => true,
    async () => {},
    async () => {},
    () => {},
    (renderedOps, elapsed) => {
      painted.push("render");
      paintedTransport.push(
        PANEL.clusterTransportExplanation(renderedOps, 2, false, elapsed).code,
      );
      readingAge.innerHTML = "rendered";
    },
    PANEL,
    () => now.value,
    dialogs,
  );
  return { harness, requests, painted, paintedTransport, readingAge, dialogs };
}

test("the direct status collects on its own gate, and never overlaps", async () => {
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const ops = operationStatus(cluster);
  const now = { value: 1_000_000 };
  const { harness, requests } = tickHarness({ cluster, ops, now, manualRoster: true });

  const settle = () => new Promise((resolve) => setImmediate(resolve));
  // Fifteen seconds since the tab painted, so this tick collects: the roster,
  // then the direct status — and it stamps the clock BEFORE the first await,
  // at request time rather than at completion.
  now.value += 15_000;
  const first = harness.settingsTick(1, "cluster");
  assert.deepEqual(requests.map((r) => r.url), ["/cluster/nodes"]);
  assert.equal(harness.stamped(), now.value, "the clock is stamped before the await");

  // A probe slower than the gate must not put a second fan-out behind itself.
  now.value += 20_000;
  await harness.settingsTick(1, "cluster");
  assert.equal(requests.length, 1, "a slow probe is never overlapped");
  requests[0].resolve(cluster);
  await settle();
  assert.deepEqual(requests.map((r) => r.url), ["/cluster/nodes", "/cluster/status"],
    "one roster read and one fan-out per gate");
  requests[1].resolve(ops);
  await first;

  // …and because the clock was stamped at request time, a probe that took
  // twenty seconds does not also push the next collection twenty seconds out.
  const prompt = harness.settingsTick(1, "cluster");
  assert.equal(requests.length, 3, "a slow probe does not delay the next one by its own duration");
  requests[2].resolve(cluster);
  await settle();
  requests[3].resolve(ops);
  await prompt;

  now.value += 9_000;
  await harness.settingsTick(1, "cluster");
  assert.equal(requests.length, 4, "the 2s tick does not become a 2s fan-out, nor a 2s roster read");
  now.value += 7_000;
  const dueAgain = harness.settingsTick(1, "cluster");
  assert.equal(requests.length, 5, "fifteen seconds later it collects again");
  requests[4].resolve(cluster);
  await settle();
  requests[5].resolve(ops);
  await dueAgain;
  assert.deepEqual(requests.map((r) => r.url),
    ["/cluster/nodes", "/cluster/status", "/cluster/nodes", "/cluster/status", "/cluster/nodes", "/cluster/status"]);
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

test("a refused collection repaints retained transport unless a dialog owns the DOM", async () => {
  // A failing collection is exactly when the age of the reading on screen
  // matters most, so the row whose only job is freshness must not be the one
  // that freezes.
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const ops = operationStatus(cluster);
  ops.nodes[0].transport = {
    observing_node_id: 1,
    observations: [transportObservation("installing", {
      observing_node_id: 1,
      peer_node_id: 2,
      direction: "outbound",
      active_deadline_remaining_ms: 1_000,
      operation_owns_work: true,
    })],
  };
  const now = { value: 1_000_000 };
  const { harness, requests, painted, paintedTransport, readingAge } = tickHarness({
    cluster, ops, now, manualRoster: true,
  });
  readingAge.innerHTML = "frozen";
  // Each gate reads the roster first; answer it unchanged, then the status.
  const settle = () => new Promise((resolve) => setImmediate(resolve));
  const rosterThen = async () => { requests.at(-1).resolve(cluster); await settle(); };
  now.value += 15_000;
  const refused = harness.settingsTick(1, "cluster");
  await rosterThen();
  requests.at(-1).reject(Object.assign(new Error("gateway"), { status: 502 }));
  await refused;
  assert.notEqual(readingAge.innerHTML, "frozen");
  assert.deepEqual(painted, ["render"], "the locally projected stall is repainted");
  assert.deepEqual(paintedTransport, ["stalled"]);
  assert.equal(harness.stamped(), now.value, "the retry is the next gate, not the next tick");

  // A modal is an operator decision in progress. Keep it intact even after
  // the retained observation expires, while continuing to patch its age row.
  harness.openDialog(true);
  readingAge.innerHTML = "frozen under modal";
  now.value += 286_001;
  const held = harness.settingsTick(1, "cluster");
  await rosterThen();
  requests.at(-1).reject(Object.assign(new Error("gateway"), { status: 502 }));
  await held;
  assert.deepEqual(painted, ["render"], "the open modal prevented a repaint");
  assert.notEqual(readingAge.innerHTML, "frozen under modal");

  // Once the modal closes, another failed refresh must pay the deferred local
  // projection and remove the expired transport evidence from the visible UI.
  harness.openDialog(false);
  now.value += 15_000;
  const expired = harness.settingsTick(1, "cluster");
  await rosterThen();
  requests.at(-1).reject(Object.assign(new Error("gateway"), { status: 502 }));
  await expired;
  assert.deepEqual(painted, ["render", "render"]);
  assert.deepEqual(paintedTransport, ["stalled", "unavailable"]);

  // A 401 from THIS read is not the logout transition. Its guard answers from
  // a process-local proof cache, so a refusal describes the cluster — and when
  // the cluster had not finished activating its credential-revocation protocol
  // the refusal was permanent, arrived on every tick, and used to end the
  // operator's session on a Settings page they had every right to be on.
  // It takes the ordinary path now: keep the evidence, keep its age honest,
  // try again on the next gate.
  now.value += 15_000;
  const unauthorized = harness.settingsTick(1, "cluster");
  await rosterThen();
  readingAge.innerHTML = "frozen after a refusal";
  requests.at(-1).reject(Object.assign(new Error("unauthorized"), { status: 401 }));
  await unauthorized;
  assert.deepEqual(
    painted,
    ["render", "render", "render"],
    "a refused recovery read repaints like any other refusal",
  );
  assert.notEqual(readingAge.innerHTML, "frozen after a refusal");
  // Scoped to this branch: the sibling branches poll ordinary admin routes,
  // where a 401 really is the credential and must still end the session.
  const clusterBranch = shippedSource("settingsTick")
    .split('api("/cluster/status"')[1]
    .split("await Promise.all(secondary)")[0];
  assert.doesNotMatch(
    clusterBranch,
    /status===401\) throw error;/,
    "rethrowing here only ever existed to reach a logout that must not happen",
  );
});

test("the two-second roster poll holds its sample under a dialog too", async () => {
  // This branch runs precisely while a node is fenced or a recovery is
  // required — which is when the force-election dialog is most likely open. The
  // dangerous shape is storing a roster that was never painted: the next poll
  // then finds no difference and the repaint is lost for good.
  const fenced = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter", { maintenance: true, maintenance_acknowledged: true }),
    node("node-c", 3, "voter"),
  ]);
  const ops = operationStatus(fenced);
  const now = { value: 1_000_000 };
  const { harness, requests, painted } = tickHarness({ cluster: fenced, ops, now, manualRoster: true });
  const moved = JSON.parse(JSON.stringify(fenced));
  moved.nodes[2].reachable = false;

  const settle = () => new Promise((resolve) => setImmediate(resolve));
  harness.openDialog(true);
  const held = harness.settingsTick(1, "cluster");
  requests[0].resolve(moved);
  await settle();
  requests[1].resolve(ops);
  await held;
  assert.deepEqual(painted, [], "the dialog was open");
  assert.deepEqual(harness.cluster(), fenced, "a roster the panel did not paint was not stored");

  harness.openDialog(false);
  now.value += 15_000;
  const paid = harness.settingsTick(1, "cluster");
  requests[2].resolve(moved);
  await settle();
  requests[3].resolve(ops);
  await paid;
  assert.deepEqual(painted, ["render"], "the owed roster repaint landed");
  assert.deepEqual(harness.cluster(), moved);
});

test("the restart poll holds its sample under a dialog, and still reads it", async () => {
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const draining = operationStatus(cluster);
  draining.nodes[0].status.media.new_admissions_blocked = true;
  draining.nodes[0].status.media.drained = false;
  const drained = JSON.parse(JSON.stringify(draining));
  drained.nodes[0].status.media.drained = true;

  // A modal is open for the first iteration and closed for the second, so the
  // sequencing is deterministic rather than a race with the microtask queue.
  const queue = [draining, drained];
  const painted = [];
  let served = 0;
  const state = { clusterOps: null };
  const poll = new Function(
    "api", "settingsCurrent", "SETTINGS_DATA", "SETTINGS_LOADED", "renderSettings",
    "repaintClusterPreserving", "clusterOpsStamp", "clusterOpsReceived",
    "clusterRepaintDeferred", "setTimeout",
    `${shippedSource("pollLocalRestart")} return pollLocalRestart;`,
  )(
    () => {
      served += 1;
      return Promise.resolve(queue.shift());
    },
    () => true,
    state,
    new Set(),
    () => painted.push("render"),
    (paint) => paint(),
    () => {},
    (value) => value,
    () => served === 1,
    (resolve) => resolve(),
  );

  // First iteration: a modal is open. The sample is held — but the loop's own
  // exit condition still reads the fresh one, so the poll keeps going.
  await poll("node-a", 1);
  assert.equal(served, 2, "holding a sample did not stop the poll");
  assert.deepEqual(painted, ["render"], "exactly the iteration that was not deferred painted");
  assert.deepEqual(state.clusterOps, drained, "and it stored exactly what it painted");
});

test("initial secondary status hydration holds its staged result under a force-election decision", () => {
  const loader = shippedSource("loadSettingsTab");
  assert.match(loader, /const staged=tab==="cluster"&&key==="clusterOps"/);
  assert.match(loader, /loadSettingsKey\(key,generation,!staged\)/);
  assert.match(loader, /patchSettingsSecondary\(tab,key,value\)/);
  let dialogOpen = true;
  let readingAgePatches = 0;
  const loaded = new Set();
  const painted = [];
  const next = { sample: "new" };
  const harness = new Function(
    "clusterRepaintDeferred", "patchClusterReadingAge", "repaintClusterPreserving",
    "loaded", "record",
    `let SETTINGS_DATA={};
     const SETTINGS_LOADED=loaded;
     const renderSettings=()=>record("render");
     ${shippedSource("patchSettingsSecondary")}
     return {patch:patchSettingsSecondary,ops:()=>SETTINGS_DATA.clusterOps};`,
  )(
    () => dialogOpen,
    () => { readingAgePatches += 1; },
    (paint) => paint(),
    loaded,
    (value) => painted.push(value),
  );

  harness.patch("cluster", "clusterOps", next);
  assert.equal(harness.ops(), undefined, "the held initial sample was cached without a paint");
  assert.equal(loaded.has("clusterOps"), false);
  assert.deepEqual(painted, []);
  assert.equal(readingAgePatches, 1);

  dialogOpen = false;
  harness.patch("cluster", "clusterOps", next);
  assert.equal(harness.ops(), next);
  assert.equal(loaded.has("clusterOps"), true);
  assert.deepEqual(painted, ["render"]);
});

test("a manual refresh holds a success if force election opens after the request starts", async () => {
  let resolveRequest;
  let dialogOpen = false;
  let readingAgePatches = 0;
  let requestStarted = false;
  const previous = { sample: "painted" };
  const next = { sample: "new" };
  const loaded = new Set(["clusterOps"]);
  const painted = [];
  const mount = { innerHTML: "previous" };
  const request = new Promise((resolve) => {
    resolveRequest = resolve;
  });
  const harness = new Function(
    "document", "loadSettingsKey", "settingsCurrent", "repaintClusterPreserving",
    "clusterOperationsUnavailable", "esc",
    "clusterRepaintDeferred", "patchClusterReadingAge", "initialOps", "loaded", "record",
    `let PAGE_RENDER_GENERATION=1;
     let SETTINGS_DATA={clusterOps:initialOps};
     const SETTINGS_LOADED=loaded;
     const renderSettings=()=>record("render");
     ${shippedSource("refreshClusterOperations")}
     return {refresh:refreshClusterOperations,ops:()=>SETTINGS_DATA.clusterOps};`,
  )(
    { getElementById: () => mount },
    (_key, _generation, cacheResult) => {
      assert.equal(cacheResult, false, "the response was stored before the modal check");
      requestStarted = true;
      return request;
    },
    () => true,
    (paint) => paint(),
    (message) => `UNAVAILABLE:${message}`,
    (value) => value,
    () => dialogOpen,
    () => { readingAgePatches += 1; },
    previous,
    loaded,
    (value) => painted.push(value),
  );

  const pending = harness.refresh(null);
  assert.equal(requestStarted, true);
  dialogOpen = true;
  resolveRequest(next);
  await pending;
  assert.deepEqual(painted, [], "the late success repainted over the decision");
  assert.equal(harness.ops(), previous, "the held success was stored without being painted");
  assert.equal(loaded.has("clusterOps"), false);
  assert.equal(readingAgePatches, 1);

  dialogOpen = false;
  await harness.refresh(null);
  assert.deepEqual(painted, ["render"], "the retry after the dialog closed did not repaint");
  assert.equal(harness.ops(), next);
  assert.equal(loaded.has("clusterOps"), true);
});

test("a failed manual refresh ages transport unless force election opens after request start", async () => {
  const run = async (elapsed, expected, openAfterStart = false) => {
    const operations = transportOps([
      transportObservation("installing", {
        active_deadline_remaining_ms: 1_000,
        operation_owns_work: true,
      }),
    ]);
    let monotonicNow = 100;
    let dialogOpen = false;
    let rejectRequest;
    let readingAgePatches = 0;
    const mount = { innerHTML: "previous" };
    const painted = [];
    const request = new Promise((_resolve, reject) => {
      rejectRequest = reject;
    });
    const refresh = new Function(
      "document", "loadSettingsKey", "settingsCurrent", "repaintClusterPreserving",
      "clusterOperationsUnavailable", "esc", "record",
      "PlurxClusterPanel", "performance", "initialOps", "clusterRepaintDeferred",
      "patchClusterReadingAge",
      `let PAGE_RENDER_GENERATION=1;
       let SETTINGS_DATA={clusterOps:initialOps};
       const SETTINGS_LOADED=new Set(["clusterOps"]);
       const CLUSTER_OPS_RECEIVED_AT=new WeakMap();
       ${shippedSource("clusterOpsReceived")}
       ${shippedSource("clusterOpsElapsed")}
       clusterOpsReceived(initialOps);
       const renderSettings=()=>record(PlurxClusterPanel.clusterTransportExplanation(
         SETTINGS_DATA.clusterOps,2,false,clusterOpsElapsed(SETTINGS_DATA.clusterOps)).code);
       ${shippedSource("refreshClusterOperations")}
       return refreshClusterOperations;`,
    )(
      { getElementById: () => mount },
      (_key, _generation, cacheResult) => {
        assert.equal(cacheResult, false, "the response was stored before the modal check");
        return request;
      },
      () => true,
      (paint) => paint(),
      (message) => `UNAVAILABLE:${message}`,
      (value) => value,
      (code) => painted.push(code),
      PANEL,
      { now: () => monotonicNow },
      operations,
      () => dialogOpen,
      () => { readingAgePatches += 1; },
    );

    monotonicNow += elapsed;
    const pending = refresh(null);
    dialogOpen = openAfterStart;
    rejectRequest(new Error("gateway"));
    await pending;
    assert.deepEqual(painted, openAfterStart ? [] : [expected]);
    assert.equal(readingAgePatches, openAfterStart ? 1 : 0);
    assert.equal(mount.innerHTML, "UNAVAILABLE:gateway");
  };

  await run(1_000, "stalled");
  await run(301_001, "unavailable");
  await run(1_000, null, true);
});

test("every cluster status success receives a client-local monotonic baseline", () => {
  assert.match(
    SHIPPED_UI,
    /clusterOps:\(\)=>api\("\/cluster\/status",\{keepSessionOn401:true\}\)\.then\(ops=>\{\s*clusterOpsStamp\(\); return clusterOpsReceived\(ops\);/,
  );
  assert.match(
    shippedSource("settingsTick"),
    /const ops=clusterOpsReceived\(await api\("\/cluster\/status",\{keepSessionOn401:true\}\)\);/,
  );
  assert.match(
    shippedSource("pollLocalRestart"),
    /const ops=clusterOpsReceived\(await api\("\/cluster\/status",\{keepSessionOn401:true\}\)\);/,
  );
  assert.doesNotMatch(shippedSource("clusterOpsReceived"), /observed_at_unix_ms|Object\.defineProperty/);
});

test("the panel's own controls and dialogs are the only ones it reaches for", () => {
  // Both new helpers hang off one class emitted 1,700 lines away. If the shell
  // stops marking the Cluster tab, the control capture silently finds nothing
  // and a repaint stops deferring — with no other symptom.
  assert.match(shippedSource("settingsShell"), /clusterwrap/);
  assert.match(shippedSource("settingsShell"), /tab==="cluster"\?" clusterwrap":""/);
  // Scoped, so an unrelated modal elsewhere in the app cannot freeze this tab's
  // refresh, and an unrelated input cannot be captured and put back.
  assert.match(shippedSource("clusterRepaintDeferred"), /\.clusterwrap dialog\[open\]/);
  assert.match(shippedSource("clusterControlValues"), /\.clusterwrap select\[id\],\.clusterwrap input\[id\]/);
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
  await tickSettle();
  requests[0].resolve(changed);
  await held;
  assert.deepEqual(painted, [], "the dialog was still open");
  // Held, not stored. What the panel holds is exactly what it painted, so the
  // next collection sees the same difference and there is no second copy for
  // the screen to drift from.
  assert.deepEqual(harness.ops(), ops, "a sample the panel did not paint was not stored");

  // Deferred, not dropped: the next collection after it closes pays the repaint,
  // even though the payload has not changed again since.
  harness.openDialog(false);
  now.value += 15_000;
  const paid = harness.settingsTick(1, "cluster");
  await tickSettle();
  requests[1].resolve(changed);
  await paid;
  assert.deepEqual(painted, ["render"], "the owed repaint landed once the modal closed");
  assert.deepEqual(harness.ops(), changed);

  // An unchanged sample is stored even under a dialog — there is nothing to
  // repaint, and holding it would make the freshness row describe the older
  // one — and the freshness row keeps moving either way.
  harness.openDialog(true);
  now.value += 15_000;
  const quiet = harness.settingsTick(1, "cluster");
  await tickSettle();
  requests[2].resolve(changed);
  await quiet;
  assert.deepEqual(painted, ["render"], "nothing changed, so nothing repainted");
  assert.deepEqual(harness.ops(), changed, "an unchanged sample is not held");
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
  await tickSettle();
  requests[0].resolve(older);
  await quiet;
  assert.deepEqual(painted, [], "nothing changed, so nothing was rewritten");
  assert.notEqual(readingAge.innerHTML, "stale text", "the freshness row was patched in place");
  assert.equal(harness.ops(), older, "the newer sample is still what the panel reads");

  now.value += 16_000;
  const changed = JSON.parse(JSON.stringify(older));
  changed.verdict.safe_to_restart_one = !changed.verdict.safe_to_restart_one;
  const loud = harness.settingsTick(1, "cluster");
  await tickSettle();
  requests[1].resolve(changed);
  await loud;
  assert.deepEqual(painted, ["render"], "a changed verdict repaints exactly once");
});

test("every repaint on this tab preserves what the operator was looking at", () => {
  // Every asynchronous status path repaints the Cluster tab the same way,
  // because a 15s cadence turns "the scroll jumps" from a papercut into an
  // unusable panel. Pin the call sites: a helper nothing calls is the failure
  // mode this suite has already caught once.
  assert.match(shippedSource("settingsTick"), /repaintClusterPreserving\(renderSettings\)/);
  assert.match(shippedSource("patchSettingsSecondary"), /repaintClusterPreserving\(renderSettings\)/);
  assert.match(shippedSource("refreshClusterOperations"), /repaintClusterPreserving\(renderSettings\)/);
  assert.match(shippedSource("pollLocalRestart"), /repaintClusterPreserving\(renderSettings\)/);
  // …and the restart poll stamps the same clock, so the two never probe at once.
  assert.match(shippedSource("pollLocalRestart"), /clusterOpsStamp\(\)/);

  const ttl = { id: "clttl", value: "3600" };
  const role = { id: "clrole", value: "learner" };
  const auto = { id: "cllogauto", type: "checkbox", checked: false, value: "on" };
  let focused = null;
  const focusable = (id) => ({ id, focus() { focused = this; }, closest: () => ({}) });
  const before = focusable("clfocus");
  const after = focusable("clfocus");
  const controls = { clttl: ttl, clrole: role, cllogauto: auto };
  const document = {
    activeElement: before,
    getElementById: (id) => (id === "clfocus" ? (document.painted ? after : before) : controls[id] || null),
    querySelectorAll: () => [ttl, role, auto],
  };
  const repaint = new Function(
    "document",
    `${shippedSource("clusterControlValues")}
     ${shippedSource("clusterRestoreControlValues")}
     ${shippedSource("repaintClusterPreserving")}
     return repaintClusterPreserving;`,
  )(document);

  repaint(() => {
    // renderSettings() rewrites the markup: every control back to the value
    // in the template, and the focused element replaced by a fresh one.
    ttl.value = "600";
    role.value = "voter";
    auto.checked = true;
    document.painted = true;
    document.activeElement = null;
  });
  // A repaint between choosing a token's lifetime and clicking Create must not
  // silently mint a ten-minute voter token instead of the hour-long read worker
  // that was selected.
  assert.equal(ttl.value, "3600");
  assert.equal(role.value, "learner");
  assert.equal(auto.checked, false);
  assert.equal(focused, after, "focus returns to the repainted control with the same id");

  // Open drawers are module state, so the repaint draws them open again
  // without anything to restore: the open one comes back, the closed one
  // stays closed.
  const cluster = status("high_availability", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
    node("node-c", 3, "voter"),
  ]);
  const html = sandbox({ open: ["node-b"] }).clusterNodesCard(cluster, operationStatus(cluster));
  assert.match(html, /<tr class="setdrawer" id="cldrawer-node-b" data-node="node-b">/);
  assert.match(html, /<tr class="setdrawer" id="cldrawer-node-a" data-node="node-a" hidden>/);
  assert.match(html, /<tr class="clrow setopen" id="clrow-node-b"[\s\S]*?<button type="button" class="clnodebtn" data-node="node-b" aria-expanded="true" aria-controls="cldrawer-node-b"/);
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
  const clusterOpsEndpoint = SHIPPED_UI
    .split("clusterOps:()=>")[1]
    .split("\n};")[0];
  assert.match(
    clusterOpsEndpoint,
    /^api\("\/cluster\/status",\{keepSessionOn401:true\}\)\.then\(ops=>\{\s*clusterOpsStamp\(\); return clusterOpsReceived\(ops\);\s*\}\)/,
  );
  // A refused fan-out stamps too: the retry is the next gate, not the next
  // tick, and the age of the reading on screen still has to move.
  assert.match(clusterOpsEndpoint, /\.catch\(error=>\{[\s\S]*clusterOpsStamp\(\);/);
  assert.match(clusterOpsEndpoint, /unavailable:true/);
  assert.doesNotMatch(shippedSource("patchSettingsSecondary"), /clusterOpsStamp\(\)/);
});

test("every path that rewrites this tab goes through the preserving repaint", () => {
  // A bare renderSettings() on the Cluster tab throws away the roster scroll,
  // the open drill-downs, and the half-filled token form. Both branches of the
  // tick, initial secondary hydration, the manual refresh, the header Refresh,
  // the restart poll, and the controls that repaint from inside the page all
  // use the helper. The tick paints once for the roster and the status
  // together, whichever of them changed or failed.
  const tick = shippedSource("settingsTick");
  assert.equal(
    (tick.match(/repaintClusterPreserving\(renderSettings\)/g) || []).length,
    1,
    "the tick's one paint preserves",
  );
  assert.match(tick, /if\(repaint&&!clusterRepaintDeferred\(\)\) repaintClusterPreserving\(renderSettings\);/);
  assert.equal(/\brenderSettings\(\)/.test(shippedSource("refreshClusterPage")), false,
    "the header Refresh never repaints bare");
  for (const handler of [
    "refreshClusterPage",
    "patchSettingsSecondary",
    "refreshClusterOperations",
    "pollLocalRestart",
    "toggleClusterExpansion",
    "dismissClusterRefusal",
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
  // Both sides as *served positions*, not as offsets into a string that
  // happens to put the markup first: in `everything` the shell always precedes
  // the rows, so an offset comparison is true for every possible shell and
  // proves nothing.
  const {rows} = shellSource();
  const order = [...rows.sidecars, ...rows.head, ...rows.body];
  const model = order.indexOf("cluster-panel.js");
  assert.notEqual(model, -1, "the shell does not load the served model");
  const caller = order.findIndex((row) => row.endsWith("cluster.js"));
  assert.ok(
    model < caller,
    `cluster-panel.js is served at ${model} but its caller at ${caller}`,
  );
  // Every derivation the page paints from is called by the painter that
  // draws it — a model function the page never reads would pass every model
  // test while the screen showed something else.
  for (const [painter, call] of [
    ["clusterStrip", "clusterStripCells(clenv(),"],
    ["clusterAlerts", "clusterAlertKinds(cluster,ops,"],
    ["clusterAlerts", "clusterVoterDownText(clenv(),"],
    ["clusterNodeTableRow", "clusterNodeCells(clenv(),"],
    ["clusterNodeActionsHtml", "clusterNodeActions(clenv(),"],
    ["clusterNodesCard", "clusterAddNodeAction(clenv(),"],
    ["clusterMaintenanceCard", "clusterMaintenanceRows(clenv(),"],
    ["clusterPanel", "clusterSampledText(ops)"],
    ["patchClusterReadingAge", "clusterSampledText(data.clusterOps)"],
  ]) assert.ok(shippedSource(painter).includes(call), `${painter} does not call ${call}`);
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

test("standalone Settings never polls cluster status even with a cached roster", async () => {
  const h=tickHarness({clustered:false,cluster:{nodes:[{node_id:"local",maintenance:true}]},ops:{},now:{value:100000}});
  await h.harness.settingsTick(1,"cluster");
  assert.deepEqual(h.requests,[]);
});

// ---- the page's summary readings (status strip, node table, chips) ----------
// These pin the derivations the new layout paints from. Each fixture is built
// so the wrong source gives a visibly different answer: hostnames differ from
// node ids, the roster and the direct status disagree where it matters.

function labCluster(extra = {}) {
  const nodes = [
    node("node-1", 1, "voter", { hostname: "lab1", is_leader: true, active_media_sessions: 2 }),
    node("node-2", 2, "voter", { hostname: "lab2" }),
    node("node-3", 3, "voter", { hostname: "lab3" }),
    node("node-4", 4, "voter", { hostname: "lab4" }),
    node("node-5", 5, "learner", { hostname: "lab5", bounded_read_ready: true }),
  ];
  return Object.assign(status("high_availability", nodes), extra);
}
function stripMap(cells) {
  return new Map(cells.map((cell) => [cell.key, cell]));
}

test("the status strip states the healthy cluster from roster and direct status", () => {
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  const cells = PANEL.clusterStripCells(sandbox().env, cluster, ops);
  assert.deepEqual(cells.map((cell) => cell.label),
    ["Cluster", "Leader", "Nodes reachable", "Replication", "Build", "Rolling restart"]);
  const s = stripMap(cells);
  assert.equal(s.get("cluster").value, "Healthy");
  assert.equal(s.get("cluster").tone, "good");
  assert.equal(s.get("cluster").sub, "4 voters · quorum 3 · tolerates 1 failure");
  assert.equal(s.get("leader").value, "lab1");
  assert.equal(s.get("leader").sub, "term 81");
  assert.equal(s.get("reachable").value, "5 / 5");
  assert.equal(s.get("reachable").sub, "4 voters · 1 learner");
  assert.equal(s.get("replication").value, "In sync");
  assert.equal(s.get("replication").sub, "max apply lag 0");
  assert.equal(s.get("build").value, "v0.2.7-operations");
  assert.equal(s.get("build").sub, "all 5 nodes match");
  // The candidate is named by hostname: the old card printed the node id.
  assert.equal(s.get("restart").value, "Safe");
  assert.equal(s.get("restart").sub, "next: lab2");
});

test("the strip's leader and lag cells read the direct status, not the roster", () => {
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  ops.nodes[2].status.raft.current_term = 82;
  ops.nodes[3].status.raft.apply_lag_entries = 17;
  const s = stripMap(PANEL.clusterStripCells(sandbox().env, cluster, ops));
  assert.equal(s.get("leader").sub, "term disagreement");
  assert.equal(s.get("leader").tone, "warn");
  assert.equal(s.get("replication").sub, "max apply lag 17");
});

test("a down voter degrades the strip, names the host, and the server's blocker", () => {
  const cluster = labCluster();
  cluster.nodes[3].reachable = false;
  const ops = operationStatus(cluster);
  ops.nodes[3] = { membership: cluster.nodes[3], observation: "timeout", error_class: "timeout", sample_age_ms: 900, status: null };
  ops.verdict = { safe_to_restart_one: false, candidate_node_id: null, warnings: [],
    blockers: [{ code: "voter_unreachable", node_id: "node-4", message: "no answer" }] };
  const s = stripMap(PANEL.clusterStripCells(sandbox().env, cluster, ops));
  assert.equal(s.get("cluster").value, "Degraded");
  assert.equal(s.get("cluster").tone, "warn");
  assert.equal(s.get("cluster").sub, "3 of 4 voters reachable · tolerates 0 more");
  assert.equal(s.get("reachable").value, "4 / 5");
  assert.equal(s.get("reachable").sub, "lab4 unreachable");
  assert.equal(s.get("build").sub, "4 of 5 answered");
  assert.equal(s.get("restart").value, "Blocked");
  assert.equal(s.get("restart").sub, "voter unreachable on lab4");
});

test("two voters are reconfiguring in the strip, never healthy", () => {
  const cluster = status("degraded_reconfiguration", [
    node("node-1", 1, "voter", { is_leader: true }),
    node("node-2", 2, "voter"),
  ]);
  const cell = PANEL.clusterStripCells(sandbox().env, cluster, null)[0];
  assert.equal(cell.value, "Reconfiguring");
  assert.equal(cell.tone, "warn");
  assertNoRedundancyClaim(`${cell.value} ${cell.sub}`, "two-voter strip cell");
  assert.match(cell.sub, /survives no failure/);
});

test("maintenance and recovery outrank a healthy availability in the strip", () => {
  const fenced = labCluster();
  fenced.nodes[0].maintenance = true;
  const ops = maintenanceOps("node-1", { drained: false });
  const m = PANEL.clusterStripCells(sandbox().env, fenced, ops)[0];
  assert.equal(m.value, "Maintenance");
  assert.equal(m.sub, "lab1 fenced · draining 1 stream");
  const lost = labCluster();
  lost.recovery = { required: true, quorum_available: false, reachable_voters: 2, required_voters: 3, leader_elected: false };
  const r = PANEL.clusterStripCells(sandbox().env, lost, null)[0];
  assert.equal(r.value, "Recovery required");
  assert.equal(r.tone, "bad");
  assert.equal(r.sub, "2 reachable · 3 needed for quorum");
});

test("build skew and protocol skew are visible in the strip", () => {
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  ops.nodes[1].status.build = "v0.2.8";
  const s = stripMap(PANEL.clusterStripCells(sandbox().env, cluster, ops));
  assert.equal(s.get("build").value, "2 builds");
  assert.equal(s.get("build").tone, "warn");
  ops.nodes[1].status.media_protocol = 10;
  ops.nodes[2].status.media_protocol = 11;
  const skewed = stripMap(PANEL.clusterStripCells(sandbox().env, cluster, ops));
  assert.equal(skewed.get("build").tone, "bad");
  assert.match(skewed.get("build").sub, /^protocol skew 10 \/ 11/);
});

test("the strip says collecting and unavailable rather than guessing a verdict", () => {
  const cluster = labCluster();
  const collecting = stripMap(PANEL.clusterStripCells(sandbox().env, cluster, null));
  assert.equal(collecting.get("restart").value, "Collecting…");
  assert.equal(collecting.get("build").value, "—");
  assert.equal(collecting.get("leader").sub, "term unknown");
  const failed = stripMap(PANEL.clusterStripCells(sandbox().env, cluster, { unavailable: true, message: "x" }));
  assert.equal(failed.get("restart").value, "Unavailable");
  const roster = PANEL.clusterStripCells(sandbox().env, { unavailable: true, code: "cluster_unavailable" }, null);
  assert.equal(roster[0].value, "Status unavailable");
  assert.ok(roster.slice(1).every((cell) => cell.value === "—"));
});

test("a node row reads the direct status, and a dash where there is none", () => {
  const env = sandbox().env;
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  const ready = PANEL.clusterNodeCells(env, cluster.nodes[0], ops);
  assert.deepEqual(ready.status, { label: "Ready", tone: "good" });
  assert.equal(ready.role, "voter");
  assert.equal(ready.raft.text, "leader · lag 0");
  assert.equal(ready.wal.text, "open · 6.3 MB");
  assert.equal(ready.database.text, "2.1 MB");
  assert.equal(ready.build.text, "v0.2.7-operations");
  // The direct status is the source for streams, not the roster's count.
  assert.equal(ready.streams.text, "0");
  const follower = PANEL.clusterNodeCells(env, cluster.nodes[1], ops);
  assert.equal(follower.raft.text, "follower · lag 0");
  // Unreachable: stale heartbeat in bad tone, and every sampled cell a dash —
  // never filled in from the roster's heartbeat.
  cluster.nodes[3].reachable = false;
  cluster.nodes[3].last_seen_at = Date.now() - 14 * 60 * 1000;
  ops.nodes[3] = { membership: cluster.nodes[3], observation: "timeout", status: null };
  const down = PANEL.clusterNodeCells(env, cluster.nodes[3], ops);
  assert.deepEqual(down.status, { label: "Unreachable", tone: "bad" });
  assert.equal(down.heartbeat.text, "14m ago");
  assert.equal(down.heartbeat.tone, "bad");
  for (const cell of ["raft", "wal", "database", "streams", "build"]) assert.equal(down[cell].text, "—", cell);
});

test("a node row's status follows the badge logic", () => {
  const env = sandbox().env;
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  ops.nodes[1] = { membership: cluster.nodes[1], observation: "refused", error_class: "refused", status: null };
  const refused = PANEL.clusterNodeCells(env, cluster.nodes[1], ops);
  assert.equal(refused.status.label, "Not observed");
  assert.match(refused.status.why, /answered and refused/);
  ops.nodes[2].status.wal.snapshot.last_error = { message: "fsync failed" };
  const wal = PANEL.clusterNodeCells(env, cluster.nodes[2], ops);
  assert.deepEqual(wal.status, { label: "Needs attention", tone: "warn" });
  assert.equal(wal.wal.tone, "bad");
  ops.nodes[3].status.raft.watermark_valid = false;
  const stale = PANEL.clusterNodeCells(env, cluster.nodes[3], ops);
  assert.equal(stale.status.label, "Needs attention");
  assert.equal(stale.raft.text, "follower · stale proof");
  cluster.nodes[4].bounded_read_ready = false;
  assert.equal(PANEL.clusterNodeCells(env, cluster.nodes[4], ops).status.label, "Catching up");
  cluster.nodes[0].maintenance = true;
  assert.equal(PANEL.clusterNodeCells(env, cluster.nodes[0], ops).status.label, "Maintenance");
  assert.equal(PANEL.clusterNodeCells(env, cluster.nodes[1], null).status.label, "Collecting…");
  const joining = node("node-6", 6, "voter", { is_voter: false });
  assert.equal(PANEL.clusterNodeCells(env, joining, ops).role, "joining voter");
});

test("a readiness chip counts only what the server reported", () => {
  assert.deepEqual(PANEL.clusterReadinessChip(null, [null]), { tone: "", label: "checking…" });
  assert.deepEqual(PANEL.clusterReadinessChip({ unavailable: "boom" }, [{ status: "met" }]),
    { tone: "warn", label: "checks unavailable" });
  assert.deepEqual(PANEL.clusterReadinessChip({}, [{ status: "met" }, null, { status: "met" }]),
    { tone: "ok", label: "2 of 2 checks" });
  assert.deepEqual(PANEL.clusterReadinessChip({}, [{ status: "met" }, { status: "unobservable" }, { status: "unmet" }]),
    { tone: "warn", label: "1 of 3 checks" });
  assert.deepEqual(PANEL.clusterReadinessChip({}, [null, null]), { tone: "", label: "not reported" });
});

test("alerts are empty when healthy and most severe first otherwise", () => {
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  assert.deepEqual(PANEL.clusterAlertKinds(cluster, ops, {}), []);
  cluster.nodes[3].reachable = false;
  cluster.availability = "degraded_reconfiguration";
  const kinds = PANEL.clusterAlertKinds(cluster, { unavailable: true }, { refusal: true }).map((k) => k.kind);
  assert.deepEqual(kinds, ["refusal", "voter_down", "ops_unavailable", "reconfiguration"]);
  // Recovery carries the maintenance target and the down voters inside it.
  cluster.nodes[0].maintenance = true;
  cluster.recovery = { required: true, quorum_available: false, reachable_voters: 2, required_voters: 3 };
  assert.deepEqual(PANEL.clusterAlertKinds(cluster, null, { refusal: true }).map((k) => k.kind), ["recovery", "refusal", "reconfiguration"]);
  cluster.recovery.required = false;
  assert.deepEqual(PANEL.clusterAlertKinds(cluster, null, {}).map((k) => k.kind),
    ["maintenance", "voter_down", "reconfiguration"]);
  assert.deepEqual(PANEL.clusterAlertKinds({ unavailable: true, code: "cluster_unavailable" }, null, {}).map((k) => k.kind),
    ["roster_failed"]);
  assert.deepEqual(PANEL.clusterAlertKinds({ unavailable: true, code: "membership_unavailable" }, null, {}), []);
});

test("restart preparation on this node raises an alert with the drain tone", () => {
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  ops.nodes[0].status.media = { local_active_sessions: 1, drained: false, new_admissions_blocked: true };
  assert.deepEqual(PANEL.clusterAlertKinds(cluster, ops, {}), [{ kind: "restart", tone: "warn" }]);
  ops.nodes[0].status.media = { local_active_sessions: 0, drained: true, new_admissions_blocked: true };
  assert.deepEqual(PANEL.clusterAlertKinds(cluster, ops, {}), [{ kind: "restart", tone: "good" }]);
});

test("the voter-down sentence names the host, the quorum left, and only the server's blocker", () => {
  const env = sandbox().env;
  const cluster = labCluster();
  cluster.nodes[3].reachable = false;
  cluster.nodes[3].last_seen_at = Date.now() - 14 * 60 * 1000;
  const safe = PANEL.clusterVoterDownText(env, cluster, operationStatus(cluster));
  assert.match(safe, /lab4 \(last heartbeat 14m ago\)/);
  assert.match(safe, /Quorum holds at 3 of 4; one more voter failure stops writes\./);
  assert.doesNotMatch(safe, /blocked/, "no blocked claim without the server's verdict");
  const ops = operationStatus(cluster);
  ops.verdict = { safe_to_restart_one: false, blockers: [{ code: "voter_unreachable", node_id: "node-4" }] };
  assert.match(PANEL.clusterVoterDownText(env, cluster, ops), /Rolling restart is blocked: voter unreachable on lab4\./);
  cluster.nodes[3].reachable = true;
  assert.equal(PANEL.clusterVoterDownText(env, cluster, ops), "");
});

test("the header's sample age is the oldest sample, aged since the aggregate", () => {
  const ops = { observed_at_unix_ms: Date.now() - 5000, nodes: [{ sample_age_ms: 1000 }, { sample_age_ms: 3000 }, { sample_age_ms: null }] };
  assert.equal(PANEL.clusterSampledText(ops), "sampled 8s ago");
  assert.equal(PANEL.clusterSampledText(null), "collecting status");
  assert.equal(PANEL.clusterSampledText({ unavailable: true }), "status unavailable");
});

test("this node's drawer carries maintenance, restart preparation and leave", () => {
  const env = sandbox().env;
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  const actions = new Map(PANEL.clusterNodeActions(env, cluster, ops, cluster.nodes[0]).map((a) => [a.id, a]));
  assert.deepEqual([...actions.keys()], ["maintenance", "restart", "leave"]);
  assert.equal(actions.get("maintenance").blocked, false);
  assert.match(actions.get("maintenance").html, /onclick='setNodeMaintenance\(&quot;node-1&quot;,true\)'/);
  // The verdict names lab2 as the candidate, so this node may not prepare.
  assert.equal(actions.get("restart").blocked, true);
  assert.match(actions.get("restart").reason, /The safe candidate is lab2/);
  assert.match(actions.get("restart").html, /disabled/);
  assert.equal(actions.get("leave").blocked, false);
  assert.equal(actions.get("leave").expand, "leave");
  ops.verdict.candidate_node_id = "node-1";
  const prepare = PANEL.clusterNodeActions(env, cluster, ops, cluster.nodes[0]).find((a) => a.id === "restart");
  assert.equal(prepare.blocked, false);
  assert.match(prepare.html, /prepareLocalRestart\(this,&quot;node-1&quot;\)/);
  ops.maintenance[0] = { node_id: "node-1", safe_to_enter: false, blockers: [{ code: "leader_transfer_pending", node_id: "node-2" }] };
  const fence = PANEL.clusterNodeActions(env, cluster, ops, cluster.nodes[0])[0];
  assert.equal(fence.blocked, true);
  assert.equal(fence.reason, "leader transfer pending on lab2.");
});

test("another member's drawer offers removal, and promotion only to a learner", () => {
  const env = sandbox().env;
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  const voter = PANEL.clusterNodeActions(env, cluster, ops, cluster.nodes[1]);
  assert.deepEqual(voter.map((a) => a.id), ["remove"]);
  assert.match(voter[0].html, /onclick='removeNode\(&quot;node-2&quot;\)'/);
  cluster.nodes[4].voter_storage_ready = false;
  cluster.nodes[4].storage_headroom_bytes = 21e9;
  const learner = PANEL.clusterNodeActions(env, cluster, ops, cluster.nodes[4]);
  assert.deepEqual(learner.map((a) => a.id), ["promote", "remove"]);
  assert.equal(learner[0].blocked, true);
  assert.match(learner[0].reason, /durable free-space reserve a voter must hold \(21 GB free\)/);
  cluster.nodes[4].voter_storage_ready = true;
  assert.equal(PANEL.clusterNodeActions(env, cluster, ops, cluster.nodes[4])[0].blocked, false);
});

test("removal keeps the voter arithmetic per node and allows retrying its own pending removal", () => {
  const env = sandbox().env;
  const two = status("degraded_reconfiguration", [
    node("node-1", 1, "voter", { is_leader: true }),
    node("node-2", 2, "voter"),
    node("node-3", 3, "learner"),
  ]);
  const remove = (cluster, n) => PANEL.clusterNodeActions(env, cluster, null, n).find((a) => a.id === "remove");
  assert.equal(remove(two, two.nodes[1]).blocked, true);
  assert.match(remove(two, two.nodes[1]).reason, /needs at least three voters and this cluster has 2/);
  assert.equal(remove(two, two.nodes[2]).blocked, false, "a learner is not held to the voter arithmetic");
  const pending = labCluster();
  pending.nodes[1].removal_pending = true;
  assert.equal(remove(pending, pending.nodes[1]).blocked, false);
  assert.match(remove(pending, pending.nodes[1]).html, />Retry removal</);
  // A removal pending on another node does not block this one: the server's
  // removal fence is per node, and the server refuses what it refuses.
  assert.equal(remove(pending, pending.nodes[2]).blocked, false);
  assert.equal(remove(pending, pending.nodes[2]).reason, "");
  pending.nodes[1].removal_pending = false;
  pending.nodes[0].maintenance = true;
  assert.equal(remove(pending, pending.nodes[2]).reason, "Finish maintenance before changing membership.");
});

test("adding a node is blocked by recovery and by an in-flight membership change", () => {
  const env = sandbox().env;
  const cluster = labCluster();
  assert.equal(PANEL.clusterAddNodeAction(env, cluster).blocked, false);
  cluster.nodes[2].removal_pending = true;
  assert.match(PANEL.clusterAddNodeAction(env, cluster).reason, /in flight on lab3/);
  cluster.recovery = { required: true, quorum_available: false, reachable_voters: 1, required_voters: 3 };
  assert.match(PANEL.clusterAddNodeAction(env, cluster).reason, /membership change cannot commit/);
});

test("the maintenance card's rows are cluster-wide and carry the server's verdict", () => {
  const env = sandbox().env;
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  const rows = new Map(PANEL.clusterMaintenanceRows(env, cluster, ops).map((row) => [row.id, row]));
  assert.deepEqual([...rows.keys()], ["restart", "election", "diagnostics"]);
  assert.match(rows.get("restart").text, /Prepare only <b>lab2<\/b>/);
  assert.match(rows.get("restart").action, /Open lab2 to prepare it/);
  assert.doesNotMatch(rows.get("restart").action, /<button/);
  assert.match(rows.get("election").action, /onclick="openElectionDialog\(\)"/);
  assert.doesNotMatch(rows.get("election").action, /disabled/);
  ops.verdict = { safe_to_restart_one: false, blockers: [{ code: "voter_unreachable", node_id: "node-4", message: "no answer" }],
    warnings: [{ code: "build_skew", node_id: null, message: "two builds" }] };
  const blocked = PANEL.clusterMaintenanceRows(env, cluster, ops)[0];
  // The verdict's own sentences, as the operations card said them.
  assert.match(blocked.text, /Blocked<\/span> — Do not restart another voter\. At least one direct safety condition is missing or unhealthy\. Keep the available voters running until every blocker below clears\./);
  assert.match(blocked.text, /<li><b>voter unreachable<\/b> on lab4: no answer<\/li><li><b>build skew<\/b>: two builds<\/li>/);
  assert.match(blocked.action, /disabled/);
  cluster.nodes[2].removal_pending = true;
  const election = PANEL.clusterMaintenanceRows(env, cluster, ops).find((row) => row.id === "election");
  assert.match(election.text, /in flight on lab3/);
  assert.match(election.action, /disabled/);
  const single = status("single_node", [node("node-1", 1, "voter", { is_leader: true })]);
  assert.deepEqual(PANEL.clusterMaintenanceRows(env, single, null).map((row) => row.id), ["restart", "diagnostics"]);
});

test("late readiness redraws only the chips and their text, never a switch", () => {
  const chips = new Map(), mores = new Map();
  const document = {
    querySelector: (selector) => {
      const chip = /data-clchip="([^"]+)"/.exec(selector), more = /data-clmore="([^"]+)"/.exec(selector);
      if (chip) { if (!chips.has(chip[1])) chips.set(chip[1], { innerHTML: "" }); return chips.get(chip[1]); }
      if (more) { if (!mores.has(more[1])) mores.set(more[1], { innerHTML: "" }); return mores.get(more[1]); }
      return null;
    },
    getElementById: () => { throw new Error("a switch was reached for"); },
  };
  const patch = new Function("document", "PlurxClusterPanel",
    `const SETTINGS_DATA={settings:{cluster_media_pool_ready:true}};
     ${["esc", "devReadinessRow", "devReadinessEvidence", "clusterSettingRows", "clusterBackupChecks", "clusterSettingChecks",
        "clusterChecksChip", "clusterChecksMore", "patchClusterReadiness"].map(shippedSource).join("\n")}
     return patchClusterReadiness;`)(document, PANEL);
  patch({ items: [{ id: "cluster_clock", requirements: [
    { id: "coverage", status: "met", evidence: "4 of 4 answered." }, { id: "ntp", status: "unobservable", evidence: "Not visible." }] }] });
  assert.equal(chips.get("cluster-clock-enforced").innerHTML, '<span class="clchip warn">1 of 2 checks</span>');
  assert.match(mores.get("cluster-clock-enforced").innerHTML, /<b>Every voter reachable and observed<\/b> <span>— 4 of 4 answered\./);
  assert.equal(chips.get("cluster-placement-enabled").innerHTML, '<span class="clchip ok">1 of 1 checks</span>');
  assert.equal(chips.get("cluster-backup").innerHTML, '<span class="clchip">not reported</span>');
  assert.equal(chips.has("cluster-takeover-enabled"), false, "a row without checks has no chip");
  patch({ unavailable: "offline" });
  assert.equal(chips.get("subcluster").innerHTML, '<span class="clchip warn">checks unavailable</span>');
  assert.match(mores.get("subcluster").innerHTML, /The prerequisite reading failed: offline/);
});

// ---- review fixes: what the page does, pinned where it is painted ----------

// The header's Refresh, run against a page whose controls are rebuilt from
// their defaults on every paint — exactly what renderSettings() does to them.
function refreshHarness({ cluster, ops, fresh, freshOps, dialog = false }) {
  const calls = [];
  let active = null;
  const control = (props) => Object.assign({
    closest: () => ({}),
    focus() { active = this; },
  }, props);
  const defaults = () => ({
    clttl: control({ id: "clttl", value: "600" }),
    clrole: control({ id: "clrole", value: "voter" }),
    clloglvl: control({ id: "clloglvl", value: "info" }),
    cllogauto: control({ id: "cllogauto", type: "checkbox", checked: true, value: "on" }),
    "cluster-clock-enforced": control({ id: "cluster-clock-enforced", type: "checkbox", checked: false, value: "on" }),
  });
  let dom = defaults();
  const document = {
    querySelectorAll: () => Object.values(dom),
    getElementById: (id) => dom[id] || null,
    querySelector: (selector) => (selector.includes("dialog[open]") && dialog ? {} : null),
    get activeElement() { return active; },
  };
  const page = new Function(
    "document", "calls", "rebuild", "fresh", "freshOps", "initial",
    `let PAGE_RENDER_GENERATION=7,CLUSTER_LOADED=true;
     const SETTINGS_DATA={cluster:initial.cluster,clusterOps:initial.ops};
     const SETTINGS_LOADED=new Set(["cluster","clusterOps"]);
     const SETTINGS_ENDPOINTS={
       cluster:()=>{calls.push("/cluster/nodes");return Promise.resolve(fresh);},
       clusterOps:()=>{calls.push("/cluster/status");return Promise.resolve(freshOps);},
     };
     const settingsCurrent=(generation,tab)=>generation===PAGE_RENDER_GENERATION&&tab==="cluster";
     const renderSettings=()=>{calls.push("paint");rebuild();};
     const viewSettings=()=>calls.push("viewSettings");
     const layoutChrome=()=>calls.push("layoutChrome");
     const patchClusterReadingAge=()=>calls.push("age");
     ${shippedSource("clusterRepaintDeferred")}
     ${shippedSource("repaintClusterPreserving")}
     ${shippedSource("clusterControlValues")}
     ${shippedSource("clusterRestoreControlValues")}
     ${shippedSource("refreshClusterPage")}
     return {refresh:refreshClusterPage,data:()=>SETTINGS_DATA,generation:()=>PAGE_RENDER_GENERATION};`,
  )(document, calls, () => { dom = defaults(); }, fresh, freshOps, { cluster, ops });
  return { page, calls, dom: () => dom, active: () => active };
}

test("Refresh re-reads roster and status and keeps every choice on the page", async () => {
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  const fresh = labCluster();
  fresh.nodes[3].reachable = false;
  const freshOps = operationStatus(fresh);
  const { page, calls, dom, active } = refreshHarness({ cluster, ops, fresh, freshOps });
  // The operator chose a one-hour read-worker token, a log level, turned off
  // auto-refresh, flipped a setting without saving, and is on the role select.
  dom().clttl.value = "3600";
  dom().clrole.value = "learner";
  dom().clloglvl.value = "warn";
  dom().cllogauto.checked = false;
  dom()["cluster-clock-enforced"].checked = true;
  dom().clrole.focus();
  const button = { disabled: false };
  const done = page.refresh(button);
  assert.equal(button.disabled, true, "the button says it is working");
  await done;
  assert.deepEqual(calls, ["/cluster/nodes", "/cluster/status", "paint"],
    "both reads, then one preserving paint — never the loading shell");
  assert.equal(page.generation(), 7, "no generation bump: an operation in flight keeps its answer");
  assert.equal(page.data().cluster, fresh);
  assert.equal(page.data().clusterOps, freshOps);
  assert.equal(dom().clttl.value, "3600");
  assert.equal(dom().clrole.value, "learner", "a repaint must never turn this into a voter token");
  assert.equal(dom().clloglvl.value, "warn");
  assert.equal(dom().cllogauto.checked, false);
  assert.equal(dom()["cluster-clock-enforced"].checked, true, "the unsaved toggle survives");
  assert.equal(active(), dom().clrole, "focus is put back on the rebuilt control");
  assert.equal(button.disabled, false);
});

test("Refresh holds both samples under an open dialog, and polls nothing on one machine", async () => {
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  const fresh = labCluster();
  fresh.nodes[3].reachable = false;
  const held = refreshHarness({ cluster, ops, fresh, freshOps: operationStatus(fresh), dialog: true });
  await held.page.refresh(null);
  assert.deepEqual(held.calls, ["/cluster/nodes", "/cluster/status", "age"]);
  assert.equal(held.page.data().cluster, cluster, "a roster the page did not paint is not stored");
  assert.equal(held.page.data().clusterOps, ops);

  const solo = { unavailable: true, code: "membership_unavailable" };
  const single = refreshHarness({ cluster: solo, ops: null, fresh: solo, freshOps: null });
  await single.page.refresh(null);
  assert.deepEqual(single.calls, ["/cluster/nodes", "paint"], "no fan-out for a machine that is not clustered");
});

test("recovery outranks maintenance in the strip and the alert slot", () => {
  const cluster = labCluster();
  cluster.nodes[2].maintenance = true;
  cluster.recovery = { required: true, quorum_available: false, reachable_voters: 1, required_voters: 3, leader_elected: false };
  const ops = operationStatus(cluster);
  const health = stripMap(PANEL.clusterStripCells(sandbox().env, cluster, ops)).get("cluster");
  assert.equal(health.value, "Recovery required");
  assert.equal(health.tone, "bad");
  const kinds = PANEL.clusterAlertKinds(cluster, ops).map((k) => k.kind);
  assert.equal(kinds[0], "recovery");
  assert.equal(kinds.includes("maintenance"), false, "recovery carries the fenced target inside it");
});

test("the two-voter sentence reaches the alert slot whole", () => {
  const ui = sandbox();
  const two = status("degraded_reconfiguration", [
    node("node-a", 1, "voter", { is_leader: true }),
    node("node-b", 2, "voter"),
  ]);
  const state = PANEL.clusterStateView(two);
  assert.ok(state.body.length > 80, "the state carries its full sentence");
  const html = ui.clusterAlerts(two, operationStatus(two));
  assert.ok(html.includes(`<b>${ui.esc(state.title)}.</b> ${ui.esc(state.body)}`),
    "the painted alert carries the unshortened two-voter body");
});

test("a drawer is opened by a real button, and the row only delegates to it", () => {
  const ui = sandbox();
  const cluster = labCluster();
  const row = ui.clusterNodeTableRow(cluster.nodes[1], cluster, null);
  const [summary] = row.split('<tr class="setdrawer"');
  assert.doesNotMatch(summary, /<tr[^>]*(tabindex|onkeydown|aria-expanded|aria-controls)/,
    "the row is not a control; the button is");
  assert.match(summary, /<button type="button" class="clnodebtn" data-node="node-2" aria-expanded="false" aria-controls="cldrawer-node-2" onclick="toggleClusterNode\(this\)">[\s\S]*lab2<\/span><\/button>/);
  assert.match(row, /<tr class="setdrawer" id="cldrawer-node-2"/, "aria-controls names a panel that exists");

  const toggled = [];
  const click = (selection) => new Function(
    "getSelection", "toggleClusterNode",
    `${shippedSource("clusterRowClick")}
     return clusterRowClick;`,
  )(() => selection, (button) => toggled.push(button));
  const button = { id: "toggle" };
  const rowEl = { querySelector: (selector) => (selector === ".clnodebtn" ? button : null) };
  const plain = { target: { closest: () => null } };
  click({ isCollapsed: true, toString: () => "" })(plain, rowEl);
  assert.deepEqual(toggled, [button], "a click on the row is the button's toggle");
  click({ isCollapsed: false, toString: () => "lab2" })(plain, rowEl);
  assert.equal(toggled.length, 1, "finishing a text selection does not toggle the drawer");
  click({ isCollapsed: true, toString: () => "" })({ target: { closest: () => ({}) } }, rowEl);
  assert.equal(toggled.length, 1, "a control inside the row handles its own click");
});

test("open drawers are dropped when the operator leaves the section", async () => {
  const run = async ({ shown, tab, reset }) => {
    const view = new Function(
      "shown", "tab",
      `let SETTINGS=null,TRAKT=null,TRAKT_EDIT=false,SETTINGS_DATA={},DV_SETTINGS_POLL_AT=0,CLUSTER_LOADED=true;
       let SETTINGS_PENDING_KEEP=null,SETTINGS_SHOWN_TAB=shown,LIB_DRAWER=null,USER_DRAWER=null,DEVICE_DRAWER=null;
       let CLUSTER_OPEN_NODES=["node-2"],PAGE_RENDER_GENERATION=1;
       const SETTINGS_LOADED=new Set(),SETTINGS_LOADS=new Map();
       const ME={is_admin:true},location={hash:"#/settings/"+tab},localStorage={setItem(){}};
       const settingsTab=()=>tab,captureSettingsMounts=()=>null,forgetJoinToken=()=>{};
       const layoutChrome=()=>{},settingsShell=()=>"",setPagePhase=()=>{},setPageTimer=()=>{};
       const loadSettingsTab=async()=>false,settingsClusterEnabled=()=>true;
       ${shippedSource("viewSettings")}
       return async(reset)=>{await viewSettings(1,reset);return CLUSTER_OPEN_NODES;};`,
    )(shown, tab);
    return view(reset);
  };
  assert.deepEqual(await run({ shown: "cluster", tab: "cluster", reset: false }), ["node-2"],
    "a same-section reload keeps them");
  assert.deepEqual(await run({ shown: "cluster", tab: "system", reset: false }), [],
    "another section drops them");
  assert.deepEqual(await run({ shown: "cluster", tab: "cluster", reset: true }), [],
    "a fresh visit starts closed");
});

test("the strip is not Healthy over a status that says otherwise", () => {
  const env = sandbox().env;
  const health = (cluster, ops) => stripMap(PANEL.clusterStripCells(env, cluster, ops)).get("cluster");
  const cluster = labCluster();
  // Still collecting degrades nothing.
  assert.equal(health(cluster, null).value, "Healthy");
  // A failed collection.
  const failed = health(cluster, { unavailable: true, message: "gateway" });
  assert.deepEqual([failed.value, failed.tone, failed.sub],
    ["Status unavailable", "warn", "direct status could not be collected"]);
  // A voter the status did not observe, with the server's blocker as the reason.
  const unobserved = operationStatus(cluster, { unreachable: true });
  const down = health(cluster, unobserved);
  assert.deepEqual([down.value, down.tone], ["Degraded", "warn"]);
  assert.equal(down.sub, "voter not observed on lab2");
  // …and with no blocker to name, the count.
  unobserved.verdict = { safe_to_restart_one: true, candidate_node_id: "node-3", blockers: [], warnings: [] };
  assert.equal(health(cluster, unobserved).sub, "3 of 4 voters answered");
  assert.equal(PANEL.clusterUnobservedVoters(cluster, null).length, 0);
  // Degraded replication.
  const behind = labCluster({ replication: { ...REPLICATION, health: "degraded", explanation: "This node is behind." } });
  assert.deepEqual([health(behind, operationStatus(behind)).value, health(behind, operationStatus(behind)).sub],
    ["Degraded", "replication degraded"]);
});

test("a blocked verdict alone puts the base warning in the alert slot", () => {
  const ui = sandbox();
  const cluster = labCluster();
  const ops = operationStatus(cluster, { unreachable: true });
  assert.ok(PANEL.clusterAlertKinds(cluster, ops).some((k) => k.kind === "restart_blocked"));
  const html = ui.clusterAlerts(cluster, ops);
  assert.match(html, /<b>Do not restart another voter\.<\/b> Rolling restart is blocked: voter not observed on lab2\./);
  // Not repeated where another block already says it, and not on a safe verdict.
  assert.equal(PANEL.clusterAlertKinds(cluster, operationStatus(cluster)).length, 0);
  const downCluster = labCluster();
  downCluster.nodes[1].reachable = false;
  const kinds = PANEL.clusterAlertKinds(downCluster, operationStatus(downCluster, { unreachable: true })).map((k) => k.kind);
  assert.deepEqual(kinds, ["voter_down"]);
});

test("degraded replication shows the server's explanation in the ledger", () => {
  const env = sandbox().env;
  const behind = { ...REPLICATION, health: "degraded", explanation: "This node is behind. Keep it running while it catches up." };
  const cluster = labCluster({ replication: behind });
  const rows = PANEL.clusterDatabaseRows(env, cluster, behind, operationStatus(cluster));
  const watch = rows.find((row) => row[0] === "Watch state");
  assert.ok(watch, "a degraded store says why");
  assert.equal(watch[1], "This node is behind. Keep it running while it catches up.");
  const healthy = PANEL.clusterDatabaseRows(env, labCluster(), REPLICATION, null);
  assert.equal(healthy.some((row) => row[0] === "Watch state"), false, "in sync needs no sentence");
});

test("a learner can be promoted from its own screen", () => {
  const env = sandbox().env;
  const cluster = labCluster();
  cluster.local_node_id = "node-5";
  cluster.nodes[4].voter_storage_ready = true;
  const actions = PANEL.clusterNodeActions(env, cluster, operationStatus(cluster), cluster.nodes[4]);
  assert.deepEqual(actions.map((a) => a.id), ["maintenance", "restart", "leave", "promote"]);
  assert.equal(actions.find((a) => a.id === "promote").blocked, false);
});

test("requirement details are printed, not hidden in a tooltip", () => {
  const ui = sandbox();
  const row = ui.clusterSettingRows().find((r) => r.reqs && r.reqs.length);
  const checks = row.reqs.map(([, title, detail]) => ({ title, detail, status: "met", evidence: "observed" }));
  const html = ui.clusterChecksMore(row, checks);
  assert.equal(/title="/.test(html), false);
  for (const [, , detail] of row.reqs) assert.ok(html.includes(`<small>${ui.esc(detail)}</small>`), detail);
});

test("the ages move on every tick, fetch or no fetch", async () => {
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  const now = { value: 1_000_000 };
  const { harness, requests, readingAge } = tickHarness({ cluster, ops, now });
  const first = harness.settingsTick(1, "cluster");
  await tickSettle();
  requests[0].resolve(ops);
  await first;
  readingAge.innerHTML = "stale";
  now.value += 2_000;
  await harness.settingsTick(1, "cluster");
  assert.equal(requests.length, 1, "nothing was due");
  assert.notEqual(readingAge.innerHTML, "stale", "the age was patched without a fetch");
});

test("the roster on the status gate repaints for news, not for a heartbeat", async () => {
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  const now = { value: 1_000_000 };
  const { harness, requests, painted } = tickHarness({ cluster, ops, now, manualRoster: true });
  const gate = async (roster) => {
    now.value += 15_000;
    const tick = harness.settingsTick(1, "cluster");
    requests.at(-1).resolve(roster);
    await tickSettle();
    requests.at(-1).resolve(ops);
    await tick;
  };
  const beat = JSON.parse(JSON.stringify(cluster));
  beat.nodes.forEach((n) => { n.last_seen_at += 15_000; });
  beat.replication.last_converged_at = 1_760_000_100;
  await gate(beat);
  assert.deepEqual(painted, [], "a heartbeat is not a reason to rewrite the page");
  assert.deepEqual(harness.cluster(), beat, "it is stored, and its ages are patched in place");

  const lost = JSON.parse(JSON.stringify(beat));
  lost.nodes[3].reachable = false;
  harness.openDialog(true);
  await gate(lost);
  assert.deepEqual(painted, [], "never under a dialog");
  assert.deepEqual(harness.cluster(), beat, "a roster that arrives under a dialog is dropped");
  harness.openDialog(false);
  await gate(lost);
  assert.deepEqual(painted, ["render"], "a voter that stopped heartbeating repaints the page once");
  assert.deepEqual(harness.cluster(), lost);
  assert.deepEqual(requests.map((r) => r.url).filter((u) => u === "/cluster/nodes").length, 3);
});

test("heartbeat cells and the last-converged age are patched in place", () => {
  const cluster = labCluster();
  cluster.nodes[1].last_seen_at = Date.now() - 125_000;
  cluster.replication = { ...REPLICATION, last_converged_at: Math.floor(Date.now() / 1000) - 600 };
  const td = { tagName: "TD", innerHTML: "4s ago", getAttribute: () => "node-2" };
  const span = { tagName: "SPAN", textContent: "4s ago", getAttribute: () => "node-2" };
  const converged = { textContent: "3s ago" };
  const patch = new Function(
    "document", "SETTINGS_DATA", "PlurxClusterPanel",
    `${shippedSource("esc")}
     ${shippedSource("fmtAgo")}
     ${shippedSource("fmtBytes")}
     ${shippedSource("clenv")}
     ${shippedSource("patchClusterReadingAge")}
     return patchClusterReadingAge;`,
  )({
    getElementById: (id) => (id === "cldb-converged" ? converged : null),
    querySelectorAll: (selector) => (selector === "[data-clhb]" ? [td, span] : []),
  }, { cluster, clusterOps: null }, PANEL);
  patch();
  assert.match(td.innerHTML, /^2m/);
  assert.match(span.textContent, /^2m/);
  assert.match(converged.textContent, /^10m/);
});

test("aria-controls names only panels that exist, and a refusal is announced once", () => {
  const recovering = labCluster();
  recovering.recovery = { required: true, quorum_available: false, reachable_voters: 1, required_voters: 3, leader_elected: false };
  const blocked = sandbox({ expanded: ["add"] }).clusterNodesCard(recovering, null);
  assert.equal(blocked.includes('aria-controls="clexp-add"'), false);
  assert.equal(blocked.includes('id="clexp-add"'), false);
  const open = sandbox().clusterNodesCard(labCluster(), null);
  assert.match(open, /aria-controls="clexp-add" aria-expanded="false"/);

  const ui = sandbox({ refusal: { node_id: "node-2", code: "cluster_leader_removal_refused", message: "no" } });
  assert.match(ui.clusterRefusalHtml(), /^<div class="clalert bad" role="alert">/);
  assert.match(ui.clusterRefusalHtml(), /^<div class="clalert bad"><div>/, "a repaint does not re-announce it");
});

test("a safe verdict that names no candidate prints no empty name", () => {
  const env = sandbox().env;
  const cluster = labCluster();
  const ops = operationStatus(cluster);
  ops.verdict = { safe_to_restart_one: true, candidate_node_id: null, blockers: [], warnings: [] };
  const row = PANEL.clusterMaintenanceRows(env, cluster, ops)[0];
  assert.equal(row.text, "The observed voters preserve majority after one restart.");
  assert.doesNotMatch(row.action, /Open/);
  const restart = PANEL.clusterNodeActions(env, cluster, ops, cluster.nodes[0]).find((a) => a.id === "restart");
  assert.equal(restart.reason, "The direct rollout verdict names no candidate.");
});

test("a learner with a fault reads Needs attention, not Catching up", () => {
  const env = sandbox().env;
  const cluster = labCluster();
  cluster.nodes[4].bounded_read_ready = false;
  const ops = operationStatus(cluster);
  assert.equal(PANEL.clusterNodeCells(env, cluster.nodes[4], ops).status.label, "Catching up");
  ops.nodes[4].status.process.live = false;
  assert.equal(PANEL.clusterNodeCells(env, cluster.nodes[4], ops).status.label, "Needs attention");
});

main().catch((error) => {
  failures += 1;
  process.stdout.write(`FAIL the suite itself threw\n${error && error.stack}\n`);
});

let reported = false;
process.on("beforeExit", () => {
  if (reported) return;
  reported = true;
  if (started !== finished) {
    failures += started - finished;
    process.stdout.write(
      `FAIL ${started - finished} test(s) never finished — an async body is waiting on ` +
        `something the test never resolves, so it printed no result at all\n`,
    );
  }
  if (failures) process.exitCode = 1;
});
