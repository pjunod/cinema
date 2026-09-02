"use strict";

// The player input contract is one JSON table every client's reducer must
// reproduce (docs/PLAYER-INPUT-CONTRACT.md §2). This test keeps the table
// itself well-formed — every surface × state × input has exactly one outcome
// and every outcome is a defined one — and keeps the doc's rendered copy of
// it identical to the fixture, so nobody can change the contract in prose
// without changing the thing the clients are tested against.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const table = require("../../scripts/player-contract-table");

const contract = JSON.parse(fs.readFileSync(table.FIXTURE, "utf8"));

function test(name, fn) {
  try {
    fn();
    process.stdout.write(`ok - ${name}\n`);
  } catch (err) {
    process.stdout.write(`not ok - ${name}\n`);
    throw err;
  }
}

test("every surface routes every state and every input to a defined outcome", () => {
  const states = Object.keys(contract.states);
  const outcomes = new Set(Object.keys(contract.outcomes));
  assert.ok(states.length >= 7, "the state list lost a state");
  assert.ok(contract.inputs.length >= 11, "the input list lost an input");
  for (const surface of Object.keys(contract.surfaces)) {
    const routing = contract.routing[surface];
    assert.ok(routing, `surface ${surface} has no routing table`);
    assert.deepEqual(Object.keys(routing).sort(), [...states].sort(), `${surface}: state set`);
    for (const state of states) {
      const row = routing[state];
      assert.deepEqual(Object.keys(row).sort(), [...contract.inputs].sort(), `${surface}/${state}: input set`);
      for (const input of contract.inputs) {
        assert.ok(outcomes.has(row[input]), `${surface}/${state}/${input} -> ${row[input]} is not a defined outcome`);
      }
    }
  }
  assert.deepEqual(Object.keys(contract.routing).sort(), Object.keys(contract.surfaces).sort(), "a routing table has no surface description");
});

test("the rulings of 2026-09-02 are encoded, not just narrated", () => {
  const tf = contract.routing["ten-foot"];
  // Ruling 1: a directional press on hidden chrome reveals; it never seeks.
  for (const d of ["left", "right", "up", "down"]) assert.equal(tf.hidden[d], "reveal");
  // Ruling 2: the timeline is preview-then-commit; Select commits, Back cancels.
  assert.equal(tf.timeline.left, "preview");
  assert.equal(tf.timeline.right, "preview");
  assert.equal(tf.scrub.select, "commit");
  assert.equal(tf.scrub.back, "cancel");
  assert.equal(contract.timings.preview_auto_commit_ms, null);
  // Ruling 3: vertical never seeks, on any surface.
  assert.equal(contract.steps.vertical_seek_seconds, null);
  for (const surface of Object.keys(contract.routing)) {
    for (const state of Object.keys(contract.states)) {
      for (const d of ["up", "down"]) {
        assert.notEqual(contract.routing[surface][state][d], "skip", `${surface}/${state}/${d} seeks`);
        assert.notEqual(contract.routing[surface][state][d], "preview", `${surface}/${state}/${d} scrubs`);
      }
    }
  }
  // The hardware Play/Pause button works whenever chrome is up (tvOS drops it today).
  assert.equal(tf.transport.play_pause, "toggle_play");
  assert.equal(tf.timeline.play_pause, "toggle_play");
  // Chrome never hides under a pending preview, a menu, the info panel, or a failure.
  for (const state of ["scrub", "menu", "info", "failed"]) {
    for (const surface of Object.keys(contract.routing)) {
      assert.equal(contract.routing[surface][state].idle, "ignore", `${surface}/${state} auto-hides`);
    }
  }
});

test("the timeline is never a horizontal neighbour of a button", () => {
  const rows = contract.controls.rows;
  const timelineRow = rows.find((r) => r.id === "timeline");
  assert.ok(timelineRow, "no timeline row");
  assert.deepEqual(timelineRow.focusable, ["timeline"], "something else in the timeline row takes focus");
  const seen = new Set();
  for (const row of rows) {
    for (const item of row.items) {
      assert.ok(!seen.has(item) || item === "spacer", `control ${item} appears in two rows`);
      seen.add(item);
    }
  }
  assert.ok(seen.has(contract.controls.initial_focus), "initial focus names an unknown control");
});

test("the preview acceleration ladder only ever gets coarser", () => {
  const ladder = contract.steps.preview_acceleration;
  assert.equal(ladder[0].from_repeat, 0);
  assert.equal(ladder[0].step_seconds, contract.steps.preview_step_seconds);
  for (let i = 1; i < ladder.length; i += 1) {
    assert.ok(ladder[i].from_repeat > ladder[i - 1].from_repeat, "repeat thresholds must ascend");
    assert.ok(ladder[i].step_seconds > ladder[i - 1].step_seconds, "steps must ascend");
  }
});

const fields = JSON.parse(fs.readFileSync(table.FIELDS, "utf8"));

test("every playback-info row has a unique id, a known section, a known format, and at least one mode", () => {
  const modes = new Set(Object.keys(fields.modes));
  const formats = new Set(Object.keys(fields.formats));
  const sections = new Set(fields.sections);
  const ids = new Set();
  const labelsBySection = new Map();
  for (const f of fields.fields) {
    assert.ok(!ids.has(f.id), `duplicate field id ${f.id}`);
    ids.add(f.id);
    assert.ok(sections.has(f.section), `${f.id}: unknown section ${f.section}`);
    assert.ok(formats.has(f.format), `${f.id}: unknown format ${f.format}`);
    assert.ok(Array.isArray(f.modes) && f.modes.length > 0, `${f.id}: no modes`);
    for (const m of f.modes) assert.ok(modes.has(m), `${f.id}: unknown mode ${m}`);
    // Mini is a subset of Standard is a subset of Debug — a row cannot appear
    // in the small panel and vanish from the big one.
    if (f.modes.includes("mini")) assert.ok(f.modes.includes("standard"), `${f.id}: in mini but not standard`);
    if (f.modes.includes("standard")) assert.ok(f.modes.includes("debug"), `${f.id}: in standard but not debug`);
    // The same label may not mean two things inside one section.
    const key = `${f.section}/${f.label}`;
    assert.ok(!labelsBySection.has(key), `${key} is used by ${labelsBySection.get(key)} and ${f.id}`);
    labelsBySection.set(key, f.id);
    if (f.available_on) {
      for (const c of f.available_on) assert.ok(["web", "apple", "android"].includes(c), `${f.id}: unknown client ${c}`);
    }
  }
  // The labels the audit found meaning two different things are gone.
  const labels = fields.fields.map((f) => f.label);
  for (const banned of ["Ahead", "Buffer ahead", "Encode", "Delivery", "Output", "Range"]) {
    assert.ok(!labels.includes(banned), `label ${banned} is back — the audit retired it`);
  }
  // Rows the audit found missing on some client are present in the contract.
  for (const required of ["container", "buffer", "status", "subtitles", "stalls"]) {
    assert.ok(ids.has(required), `field ${required} missing`);
  }
});

test("docs/PLAYER-INPUT-CONTRACT.md embeds both fixtures verbatim", () => {
  const doc = fs.readFileSync(table.DOC, "utf8");
  for (const [begin, end, rendered, label] of [
    [table.BEGIN, table.END, table.renderBlock(contract), "routing"],
    [table.INFO_BEGIN, table.INFO_END, table.renderInfoBlock(fields), "info"],
  ]) {
    const start = doc.indexOf(begin);
    const stop = doc.indexOf(end);
    assert.ok(start >= 0 && stop > start, `the doc lost its ${label} block markers`);
    const embedded = doc.slice(start, stop + end.length);
    assert.equal(
      embedded,
      rendered,
      `doc and ${label} fixture disagree — run scripts/player-contract-table --write`,
    );
  }
});
