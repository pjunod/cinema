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

test("first-match precedence has no unreachable row: every source is exercised", () => {
  const raised = new Set();
  for (const item of contract.cases) {
    for (const event of item.events) if (event.raise) raised.add(event.raise);
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

test("presentSurface does not mutate the state it was handed", () => {
  const before = policy.initialSurfaceState();
  const frozen = JSON.stringify(before);
  policy.presentSurface(before, { t: 0, attach: "g1" });
  policy.presentSurface(before, { t: 10, raise: "control_hold", attached: "g1", context: "attached" });
  assert.equal(JSON.stringify(before), frozen, "the reducer mutated its input state");
});

if (failures > 0) {
  process.stdout.write(`\n${failures} surface contract check(s) failed\n`);
  process.exit(1);
}
process.stdout.write(`\nall ${contract.cases.length} playback surface cases pass\n`);
