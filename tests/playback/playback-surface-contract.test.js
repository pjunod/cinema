"use strict";

// The playback surface contract is one JSON table of fault classes and
// sources plus a set of ORDERED EVENT SEQUENCES (docs/clients/PLAYBACK-SURFACE-CONTRACT.md
// §3). Every client runs the same sequences against its own presenter; this
// file runs them against the shipped web reducer, and keeps the fixture
// itself well-formed — a blocking class that does not require a stopped
// player, a source whose class does not exist, or a row no case ever exercises
// are all failures here rather than three quiet disagreements in the field.

const assert = require("node:assert/strict");
const fs = require("node:fs");

const table = require("../../scripts/player-contract-table");
const policy = require("../../crates/plurxd/src/web/playback-policy.js");

const contract = JSON.parse(fs.readFileSync(table.SURFACE_FIXTURE, "utf8"));

let failures = 0;
function test(name, fn) {
  try {
    fn();
    process.stdout.write(`ok - ${name}\n`);
  } catch (err) {
    failures += 1;
    process.stdout.write(`not ok - ${name}\n`);
    process.stdout.write(`  ${err && err.message ? err.message : err}\n`);
  }
}

// ---- fixture invariants -----------------------------------------------------

test("every source names a class the fixture defines", () => {
  for (const row of contract.sources) {
    if (row.class === null) continue;
    assert.ok(contract.classes[row.class], `${row.id} names undefined class ${row.class}`);
  }
});

test("a blocking class requires a player its owner has already stopped", () => {
  for (const [name, cls] of Object.entries(contract.classes)) {
    if (cls.blocking !== true) continue;
    assert.equal(cls.requires_player_stopped, true, `${name} is blocking without requiring a stopped player`);
  }
});

test("every action a class or a source names is in the vocabulary", () => {
  const vocabulary = new Set(contract.actions);
  for (const [name, cls] of Object.entries(contract.classes)) {
    for (const action of cls.default_actions || []) {
      assert.ok(vocabulary.has(action), `class ${name} names unknown action ${action}`);
    }
  }
  for (const row of contract.sources) {
    for (const action of row.actions || []) {
      assert.ok(vocabulary.has(action), `source ${row.id} names unknown action ${action}`);
    }
  }
});

test("every severity has a rank and every context is declared", () => {
  for (const [name, cls] of Object.entries(contract.classes)) {
    assert.ok(contract.severity_rank[cls.severity] != null, `class ${name} has severity ${cls.severity} with no rank`);
  }
  const contexts = new Set([...contract.contexts, "any"]);
  for (const row of contract.sources) {
    assert.ok(contexts.has(row.context), `source ${row.id} has undeclared context ${row.context}`);
  }
});

test("case names are unique and every expectation names a declared kind", () => {
  const seen = new Set();
  const kinds = new Set(Object.keys(contract.kinds));
  for (const item of contract.cases) {
    assert.ok(!seen.has(item.name), `duplicate case name ${item.name}`);
    seen.add(item.name);
    for (const expectation of item.expect) {
      if (expectation.surface == null) continue;
      assert.ok(kinds.has(expectation.surface), `${item.name} expects undeclared surface kind ${expectation.surface}`);
    }
  }
});

test("every source row is exercised by at least one case", () => {
  const raised = new Set();
  for (const item of contract.cases) {
    for (const event of item.events) {
      if (event.raise) raised.add(event.raise);
      if (event.system_paused === true) raised.add("system_interruption");
    }
  }
  for (const row of contract.sources) {
    assert.ok(raised.has(row.id), `source ${row.id} is never raised by a case`);
  }
});

test("every class is reachable from at least one source", () => {
  const used = new Set(contract.sources.map((row) => row.class).filter(Boolean));
  // `degraded` is also reached by the agreement rule demoting a blocking fault.
  for (const name of Object.keys(contract.classes)) {
    assert.ok(used.has(name), `class ${name} is reachable from no source`);
  }
});

test("every source mapping to a blocking class declares the stop it needs", () => {
  // The rendered source table is what an M1/M2/M3 author reads. A row that
  // maps to `stopped` but prints "requires a stopped player: no" sends them to
  // raise it over a running player, where the reducer answers with a log line
  // and no sign-in prompt at all.
  for (const row of contract.sources) {
    if (!row.class) continue;
    if (contract.classes[row.class].blocking !== true) continue;
    assert.equal(
      row.requires && row.requires.player_stopped,
      true,
      `source ${row.id} maps to blocking class ${row.class} without declaring requires.player_stopped`,
    );
  }
});

test("every action in the vocabulary is offered by at least one case", () => {
  const offered = new Set();
  for (const item of contract.cases) {
    for (const event of item.events) for (const action of event.actions || []) offered.add(action);
    for (const expectation of item.expect) for (const action of expectation.actions || []) offered.add(action);
  }
  for (const [name, cls] of Object.entries(contract.classes)) {
    for (const action of cls.default_actions || []) offered.add(action);
  }
  for (const action of contract.actions) {
    assert.ok(offered.has(action), `action ${action} is in the vocabulary and in no case`);
  }
});

test("every retirement reason a class names is reachable, and none is invented", () => {
  const known = new Set([
    "presenting", "presenting_after_raise", "presenting_new_attached", "presenting_continuous_ms",
    "intent_settled", "intent_superseded", "attached_retired", "owner_success", "timer", "user",
    // The viewer no longer wants media, so a fault about a player that wants it
    // is about nothing. Only `buffering` names it, and the assertion below says
    // so: a reason that starts spreading across the class table is a reason
    // somebody has stopped thinking about.
    "playback_not_requested",
    "system_resumed",
  ]);
  assert.deepEqual(
    Object.entries(contract.classes)
      .filter(([, cls]) => (cls.retired_by || []).includes("playback_not_requested"))
      .map(([name]) => name),
    ["buffering"],
    "`playback_not_requested` retires buffering and nothing else",
  );
  const continuous = { refused: "refused_progress_ms", degraded: "disagreement_notice_ms" };
  for (const [name, cls] of Object.entries(contract.classes)) {
    for (const reason of cls.retired_by || []) {
      assert.ok(known.has(reason), `class ${name} names unknown retirement reason ${reason}`);
      if (reason === "presenting_continuous_ms") {
        assert.ok(
          contract.timings[continuous[name]] != null,
          `class ${name} retires on continuous presenting with no timing to measure it against`,
        );
      }
      if (reason === "timer") {
        assert.ok(cls.timed_ms != null, `class ${name} retires on a timer it does not declare`);
      }
    }
  }
  for (const row of contract.sources) {
    for (const reason of row.retired_by || []) {
      assert.ok(known.has(reason), `source ${row.id} names unknown retirement reason ${reason}`);
    }
  }
});

test("the shipped policy carries the fixture verbatim", () => {
  assert.deepEqual(policy.SURFACE_CLASSES, contract.classes);
  assert.deepEqual(policy.SURFACE_SOURCES, contract.sources);
  assert.deepEqual(policy.SURFACE_TIMINGS, contract.timings);
  assert.deepEqual(policy.SURFACE_SEVERITY_RANK, contract.severity_rank);
});

// ---- the reducer runs every case -------------------------------------------

function runCase(item, presenter) {
  let state = presenter.initialSurfaceState();
  const byTime = new Map();
  for (const event of item.events) {
    const step = presenter.presentSurface(state, event);
    state = step.state;
    if (!byTime.has(event.t)) byTime.set(event.t, { log: [], surface: null });
    const slot = byTime.get(event.t);
    for (const entry of step.log) slot.log.push(entry);
    slot.surface = step.surface;
  }
  return byTime;
}

function checkCase(item, presenter, label) {
  const timeline = runCase(item, presenter);
  for (const expectation of item.expect) {
    const slot = timeline.get(expectation.at);
    assert.ok(slot, `${label} ${item.name}: nothing happened at t=${expectation.at}`);
    const where = `${label} ${item.name} @${expectation.at}`;
    if (expectation.surface != null) {
      assert.equal(slot.surface.kind, expectation.surface, `${where}: surface kind`);
    }
    if (expectation.class !== undefined) {
      assert.equal(slot.surface.class, expectation.class, `${where}: fault class`);
    }
    if (expectation.source !== undefined) {
      assert.equal(slot.surface.source, expectation.source, `${where}: fault source`);
    }
    if (expectation.actions !== undefined) {
      assert.deepEqual(slot.surface.actions, expectation.actions, `${where}: actions`);
    }
    if (expectation.title !== undefined) {
      assert.equal(slot.surface.title, expectation.title, `${where}: title`);
    }
    if (expectation.detail !== undefined) {
      assert.equal(slot.surface.detail, expectation.detail, `${where}: detail`);
    }
    if (expectation.position_ms !== undefined) {
      assert.equal(slot.surface.position_ms, expectation.position_ms, `${where}: position_ms`);
    }
    if (expectation.input_failed !== undefined) {
      assert.equal(slot.surface.input_failed, expectation.input_failed, `${where}: input contract failed state`);
      assert.equal(
        presenter.surfaceEntersFailedRouting(slot.surface),
        expectation.input_failed,
        `${where}: surfaceEntersFailedRouting disagrees with the surface`,
      );
    }
    if (expectation.error !== undefined) {
      const errors = slot.log.filter((entry) => entry.error).map((entry) => entry.error);
      assert.ok(errors.includes(expectation.error), `${where}: expected fixture error ${expectation.error}, got ${JSON.stringify(errors)}`);
    }
    if (expectation.log !== undefined) {
      assert.deepEqual(slot.log.map((entry) => entry.event), expectation.log, `${where}: log`);
    }
  }
}

test(`the shipped web reducer runs all ${contract.cases.length} cases`, () => {
  for (const item of contract.cases) checkCase(item, policy, "web");
});

test("a blocking surface is never rendered over a player nobody stopped", () => {
  // The whole point, asserted independently of any one case: replay every
  // case and fail if a blocking surface is ever drawn from a fault that did
  // not carry the owner's stop.
  for (const item of contract.cases) {
    let state = policy.initialSurfaceState();
    for (const event of item.events) {
      const step = policy.presentSurface(state, event);
      state = step.state;
      const surface = step.surface;
      if (surface.kind !== "blocking") continue;
      const cls = contract.classes[surface.class];
      if (cls.blocking !== true) continue; // a full-screen progress surface asks nothing
      assert.equal(
        surface.player_stopped,
        true,
        `${item.name} @${event.t}: ${surface.class} blocked the picture without the owner stopping the player`,
      );
    }
  }
});

test("the presenter is pure: replaying a case twice gives the same answer", () => {
  for (const item of contract.cases) {
    const first = JSON.stringify([...runCase(item, policy)].map(([t, slot]) => [t, slot.surface.kind, slot.surface.class]));
    const second = JSON.stringify([...runCase(item, policy)].map(([t, slot]) => [t, slot.surface.kind, slot.surface.class]));
    assert.equal(first, second, `${item.name} is not deterministic`);
  }
});

test("presentSurface does not mutate the state it was handed, faults and all", () => {
  // The weak version of this test started from an empty state, so the
  // fault-copying path — the one that actually matters, because the render and
  // the ledger both hold a fault — was never exercised.
  for (const item of contract.cases) {
    let state = policy.initialSurfaceState();
    for (const event of item.events) state = policy.presentSurface(state, event).state;
    const snapshot = JSON.stringify(state);
    const step = policy.presentSurface(state, { t: state.now + 1000, tick: true });
    assert.equal(JSON.stringify(state), snapshot, `${item.name}: the reducer mutated its input state`);
    // The surface's fault is a copy, not a window into the state behind it.
    if (step.surface.fault) {
      assert.throws(
        () => step.surface.fault.actions.push("hacked"),
        `${item.name}: the surface's actions are writable and shared`,
      );
    }
    assert.equal(JSON.stringify(state), snapshot, `${item.name}: the state moved after the fact`);
  }
});

test("replaying from an earlier state gives the same answer as the first pass", () => {
  for (const item of contract.cases) {
    let state = policy.initialSurfaceState();
    const checkpoints = [state];
    const kinds = [];
    for (const event of item.events) {
      const step = policy.presentSurface(state, event);
      state = step.state;
      checkpoints.push(state);
      kinds.push(`${step.surface.kind}/${step.surface.class}`);
    }
    for (let index = 0; index < item.events.length; index += 1) {
      const again = policy.presentSurface(checkpoints[index], item.events[index]);
      assert.equal(
        `${again.surface.kind}/${again.surface.class}`,
        kinds[index],
        `${item.name}: replaying event ${index} from its own checkpoint diverged`,
      );
    }
  }
});

if (failures > 0) {
  process.stdout.write(`\n${failures} surface contract check(s) failed\n`);
  process.exit(1);
}
process.stdout.write(`\nall ${contract.cases.length} playback surface cases pass\n`);
