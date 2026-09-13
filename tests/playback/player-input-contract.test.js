"use strict";

// The player input contract is one JSON table every client's reducer must
// reproduce (docs/clients/PLAYER-INPUT-CONTRACT.md §2). This test keeps the table
// itself well-formed — every surface × state × input has exactly one outcome
// and every outcome is a defined one — and keeps the doc's rendered copy of
// it identical to the fixture, so nobody can change the contract in prose
// without changing the thing the clients are tested against.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const table = require("../../scripts/player-contract-table");
const webPolicy = require("../../crates/plurxd/src/web/playback-policy.js");

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

test("the served web routing table is the fixture routing table", () => {
  assert.deepEqual(webPolicy.INPUT_ROUTING, contract.routing);
});

test("the close control leaves the player from every state", () => {
  // `close` is a button, not the `back` key. Routed through `back`, the iOS ✕
  // hid the chrome in `transport` and exited in no state a viewer could tap it
  // from (2026-09-03). Every row closes what is open and then ends in `exit`.
  const close = contract.close_control;
  assert.ok(close, "no close_control section");
  const outcomes = new Set(Object.keys(contract.outcomes));
  const states = Object.keys(contract.states);
  assert.deepEqual(Object.keys(close).filter((k) => k !== "notes").sort(), [...states].sort(), "close_control: state set");
  for (const state of states) {
    const steps = close[state];
    assert.ok(Array.isArray(steps) && steps.length > 0, `close_control/${state}: no steps`);
    for (const step of steps) assert.ok(outcomes.has(step), `close_control/${state}/${step} is not a defined outcome`);
    assert.equal(steps.indexOf("exit"), steps.length - 1, `close_control/${state} must end in exit and run nothing after it`);
    assert.ok(!steps.includes("hide"), `close_control/${state} hides — that is the defect`);
  }
  assert.deepEqual(close.scrub, ["cancel", "exit"]);
  assert.deepEqual(close.menu, ["close_menu", "exit"]);
  assert.deepEqual(close.info, ["close_info", "exit"]);
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

// ---- Live TV: the sibling table (docs/clients/PLAYER-INPUT-CONTRACT.md §4a) ----
// Live television is the same contract with every timeline row removed and a
// channel added. It is a second table in the same fixture rather than a fourth
// surface, because live browsing has its own presentation states and outcomes.

test("the live table routes every surface, state and input to a defined live outcome", () => {
  const live = contract.live;
  assert.ok(live, "the fixture lost its live section");
  const states = Object.keys(live.states);
  const outcomes = new Set(Object.keys(live.outcomes));
  assert.deepEqual(Object.keys(live.routing).sort(), Object.keys(live.surfaces).sort(), "a live routing table has no surface description");
  for (const surface of Object.keys(live.surfaces)) {
    const routing = live.routing[surface];
    assert.ok(routing, `live surface ${surface} has no routing table`);
    assert.deepEqual(Object.keys(routing).sort(), [...states].sort(), `live ${surface}: state set`);
    for (const state of states) {
      const row = routing[state];
      assert.deepEqual(Object.keys(row).sort(), [...live.inputs].sort(), `live ${surface}/${state}: input set`);
      for (const input of live.inputs) {
        assert.ok(outcomes.has(row[input]), `live ${surface}/${state}/${input} -> ${row[input]} is not a defined live outcome`);
      }
    }
  }
});

test("nothing in the live table seeks, scrubs or opens a timeline", () => {
  const live = contract.live;
  // Structural, not incidental: the seek outcomes are not defined for this
  // surface at all, so no row can name one even by accident.
  for (const banned of ["skip", "preview", "commit", "cancel", "commit_then_toggle_play"]) {
    assert.ok(!(banned in live.outcomes), `live outcome ${banned} exists — a live stream has no timeline`);
  }
  for (const banned of ["timeline", "scrub", "info", "failed", "transport"]) {
    assert.ok(!(banned in live.states), `live state ${banned} exists — that is a finite-player state`);
  }
  for (const banned of ["skip_back", "skip_forward"]) {
    assert.ok(!live.inputs.includes(banned), `live input ${banned} exists — nothing to skip`);
  }
});

test("the 2026-09-02 rulings survive on the live surface", () => {
  const live = contract.live.routing;
  // Ruling 1, unchanged: a direction on a hidden ten-foot overlay only reveals.
  for (const d of ["left", "right", "up", "down", "select"]) {
    assert.equal(
      live["ten-foot"].fullscreen_hidden[d],
      "reveal",
      `ten-foot/fullscreen_hidden/${d} does something other than reveal`,
    );
  }
  // Nothing on a hidden ten-foot overlay changes channel.
  for (const input of contract.live.inputs) {
    assert.ok(
      !["tune", "channel_up", "channel_down", "strip_prev", "strip_next"].includes(
        live["ten-foot"].fullscreen_hidden[input],
      ),
      `ten-foot/fullscreen_hidden/${input} changes channel behind hidden controls`,
    );
  }
  // Ruling 2 applied to fullscreen chrome: arrows move focus and Select alone
  // activates the chosen control.
  for (const d of ["left", "right", "up", "down"]) {
    assert.equal(live["ten-foot"].fullscreen_controls[d], "focus_control");
  }
  assert.equal(live["ten-foot"].fullscreen_controls.select, "activate");
  assert.equal(live["ten-foot"].fullscreen_controls.back, "hide");
  assert.equal(contract.live.timings.preview_auto_commit_ms, null);
  // The deliberate divergence: a desktop keyboard has no focus ring, so
  // vertical tunes directly. Recorded here so it cannot be "fixed" silently.
  assert.equal(live.desktop.fullscreen_hidden.up, "channel_up");
  assert.equal(live.desktop.fullscreen_hidden.down, "channel_down");
  assert.equal(live.desktop.fullscreen_controls.select, "tune");
});

test("the live overlay hides only from a visible overlay, and a held channel key is one tuner start", () => {
  const live = contract.live;
  for (const surface of Object.keys(live.routing)) {
    for (const state of Object.keys(live.states)) {
      if (state === "fullscreen_controls") continue;
      assert.equal(live.routing[surface][state].idle, "ignore", `${surface}/${state}: non-chrome state auto-hides`);
    }
  }
  assert.equal(live.routing["ten-foot"].fullscreen_controls.idle, "hide");
  assert.equal(live.routing.desktop.fullscreen_controls.idle, "hide");
  assert.equal(live.routing.touch.fullscreen_controls.idle, "hide");
  // The same numbers as the finite player, deliberately.
  assert.equal(live.timings.hide_after_ms, contract.timings.hide_after_ms);
  assert.equal(live.timings.hidden_only_while_playing, contract.timings.hidden_only_while_playing);
  // One tuner GET per held key: the coalescing window is the guardrail.
  assert.ok(live.timings.channel_coalesce_ms >= 350, "channel coalescing is shorter than one held-key repeat");
});

test("the root live browser never retunes or controls playback behind native browsing", () => {
  const tuning = new Set(["tune", "channel_up", "channel_down", "strip_prev", "strip_next"]);
  for (const surface of Object.keys(contract.live.routing)) {
    const browser = contract.live.routing[surface].browser;
    for (const input of contract.live.inputs) {
      assert.ok(!tuning.has(browser[input]), `live ${surface}/browser/${input} retunes behind browsing`);
    }
    assert.equal(browser.play_pause, "ignore", `live ${surface}/browser owns the transport key`);
  }
  for (const surface of ["ten-foot", "desktop"]) {
    for (const input of ["left", "right", "up", "down", "select"]) {
      assert.equal(contract.live.routing[surface].browser[input], "delegate");
    }
  }
});

test("the served web live routing table is the fixture live table", () => {
  assert.deepEqual(webPolicy.LIVE_INPUT_ROUTING, contract.live.routing);
  assert.equal(webPolicy.routeLiveInput("desktop", "fullscreen_controls", "select"), "tune");
  assert.equal(webPolicy.liveContractTiming("hide_after_ms"), contract.live.timings.hide_after_ms);
  assert.equal(webPolicy.liveHotkey("G"), "guide_sheet");
  assert.equal(webPolicy.liveHotkey("q"), null);
  assert.throws(() => webPolicy.routeLiveInput("desktop", "scrub", "left"), /no live route/);
});

const fields = JSON.parse(fs.readFileSync(table.FIELDS, "utf8"));

test("the served web playback-info field list is the fixture field list", () => {
  const web = fs.readFileSync(table.WEB, "utf8");
  assert.ok(web.includes(table.renderFieldsEmbedBlock(fields)));
});

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
  for (const required of [
    "container",
    "server_ready",
    "http_wait",
    "client_loaded",
    "presentation",
    "status",
    "subtitles",
    "stalls",
  ]) {
    assert.ok(ids.has(required), `field ${required} missing`);
  }
});

test("docs/clients/PLAYER-INPUT-CONTRACT.md embeds every generated block verbatim", () => {
  const doc = fs.readFileSync(table.DOC, "utf8");
  for (const [begin, end, rendered, label] of [
    [table.BEGIN, table.END, table.renderBlock(contract), "routing"],
    [table.OUTCOMES_BEGIN, table.OUTCOMES_END, table.renderOutcomesBlock(contract), "outcomes"],
    [table.ROWS_BEGIN, table.ROWS_END, table.renderRowsBlock(contract), "rows"],
    [table.TIMINGS_BEGIN, table.TIMINGS_END, table.renderTimingsBlock(contract), "timings"],
    [table.LIVE_BEGIN, table.LIVE_END, table.renderLiveBlock(contract), "live"],
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

test("docs/clients/PLAYBACK-SURFACE-CONTRACT.md embeds the generated surface blocks verbatim", () => {
  const surface = JSON.parse(fs.readFileSync(table.SURFACE_FIXTURE, "utf8"));
  const doc = fs.readFileSync(table.SURFACE_DOC, "utf8");
  for (const [begin, end, rendered, label] of [
    [table.SURFACE_CLASSES_BEGIN, table.SURFACE_CLASSES_END, table.renderSurfaceClassesBlock(surface), "surface classes"],
    [table.SURFACE_SOURCES_BEGIN, table.SURFACE_SOURCES_END, table.renderSurfaceSourcesBlock(surface), "surface sources"],
  ]) {
    const start = doc.indexOf(begin);
    const stop = doc.indexOf(end);
    assert.ok(start >= 0 && stop > start, `the surface doc lost its ${label} block markers`);
    assert.equal(
      doc.slice(start, stop + end.length),
      rendered,
      `doc and ${label} fixture disagree — run scripts/player-contract-table --write`,
    );
  }
});

test("the served web policy embeds the surface fixture verbatim", () => {
  const surface = JSON.parse(fs.readFileSync(table.SURFACE_FIXTURE, "utf8"));
  const policy = fs.readFileSync(table.POLICY, "utf8");
  const rendered = table.renderSurfaceEmbedBlock(surface);
  const start = policy.indexOf(table.SURFACE_EMBED_BEGIN);
  const stop = policy.indexOf(table.SURFACE_EMBED_END);
  assert.ok(start >= 0 && stop > start, "playback-policy.js lost its surface embed markers");
  assert.equal(
    policy.slice(start, stop + table.SURFACE_EMBED_END.length),
    rendered,
    "the served surface table is not the fixture — run scripts/player-contract-table --embed",
  );
  assert.deepEqual(webPolicy.SURFACE_CLASSES, surface.classes);
  assert.deepEqual(webPolicy.SURFACE_SOURCES, surface.sources);
});
