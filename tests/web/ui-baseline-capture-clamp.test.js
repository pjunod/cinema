"use strict";
const { test } = require("node:test");

// The capture harness's tick clamp, tested against the JavaScript
// scripts/ui-baseline actually injects rather than against a copy of it.
//
// What this protects: a number in tests/ui-structure.golden that is not
// structure. The Libraries tab fetches scan status on render and then polls
// every 2s, and the golden records three calls — render plus two ticks. If the
// third tick is ever allowed to run, an unchanged page fails the gate on
// `GET /api/v1/scan/status 3 -> 4`, which is what happened on 2026-09-07 and
// could not be reproduced afterwards because nothing recorded why.
//
// The clamp has to hold inside the page, synchronously, before the retained
// callback runs. Counting requests from Python cannot: on a starved runner the
// count is read after the interval has already fired again. So the assertions
// below drive the wrapper directly with a fake timer and prove the interval is
// dead before the second callback is invoked.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const HARNESS = path.join(__dirname, "../../scripts/ui-baseline");
const SOURCE = fs.readFileSync(HARNESS, "utf8");

// Extract the injected script by its assignment, so a rename or a rewrite of
// the block fails here instead of silently testing nothing.
function captureScript() {
  const marker = 'ACTIVITY_CAPTURE_JS = """';
  const start = SOURCE.indexOf(marker);
  assert.ok(start >= 0, "scripts/ui-baseline no longer defines ACTIVITY_CAPTURE_JS");
  const bodyStart = start + marker.length;
  const end = SOURCE.indexOf('"""', bodyStart);
  assert.ok(end > bodyStart, "ACTIVITY_CAPTURE_JS is not a closed triple-quoted string");
  const body = SOURCE.slice(bodyStart, end);
  assert.match(body, /window\.setInterval = /, "the block no longer wraps setInterval");
  return body;
}

// A page just real enough for the wrapper: an interval table, a hash, and the
// two timer globals the app declares. Intervals do not fire on their own —
// each test decides exactly when a tick happens, which is the whole point.
function fakePage(hash) {
  const intervals = new Map();
  let nextId = 1;
  const win = {
    setInterval(callback, delay, ...args) {
      const id = nextId++;
      intervals.set(id, { callback, delay, args });
      return id;
    },
    clearInterval(id) {
      intervals.delete(id);
    },
  };
  const sandbox = {
    window: win,
    location: { hash },
    PAGE_TIMER: null,
    ACT_TIMER: null,
    clearInterval: (id) => win.clearInterval(id),
    setInterval: (...a) => win.setInterval(...a),
    tick(id) {
      const entry = intervals.get(id);
      assert.ok(entry, `interval ${id} is not scheduled — it was already cleared`);
      entry.callback.apply(null, entry.args);
    },
    scheduled: () => intervals.size,
    isScheduled: (id) => intervals.has(id),
  };
  return sandbox;
}

// Run the extracted block with the fake page as its globals. `PAGE_TIMER` and
// `ACT_TIMER` are bare identifiers in the injected source (the app declares
// them with `let`), so they have to be real bindings, not window properties.
function install(sandbox) {
  const body = captureScript();
  const run = new Function(
    "window", "location", "clearInterval", "setInterval", "state",
    `let PAGE_TIMER = null, ACT_TIMER = null;
     const sync = () => { state.PAGE_TIMER = PAGE_TIMER; state.ACT_TIMER = ACT_TIMER; };
     const adopt = () => { PAGE_TIMER = state.PAGE_TIMER; ACT_TIMER = state.ACT_TIMER; };
     ${body}
     return { schedule: (fn, ms) => { adopt(); const id = window.setInterval(fn, ms); sync(); return id; },
              adopt, sync };`
  );
  return run(sandbox.window, sandbox.location, sandbox.clearInterval, sandbox.setInterval, sandbox);
}

test("the settings scan poll retains exactly two ticks and stops itself", () => {
  const page = fakePage("#/settings");
  const harness = install(page);
  let ran = 0;
  const id = harness.schedule(() => { ran += 1; }, 2000);
  page.PAGE_TIMER = id;
  harness.adopt();

  assert.equal(page.window.__plurxSettingsScanCaptureTick, false, "armed before any tick");

  page.tick(id);
  harness.sync();
  assert.equal(ran, 1, "the first retained callback runs");
  assert.ok(page.isScheduled(id), "the interval survives its first tick");
  assert.equal(page.window.__plurxSettingsScanCaptureTick, false);

  page.tick(id);
  harness.sync();
  assert.equal(ran, 2, "the second retained callback runs");
  assert.equal(
    page.isScheduled(id), false,
    "the interval must be cleared by the second tick — a third scan-status " +
    "call is what turns the golden's 3 into a 4");
  assert.equal(page.window.__plurxSettingsScanCaptureTick, true,
    "the capture waits on this flag; without it the harness hangs instead");
  assert.equal(page.PAGE_TIMER, null, "PAGE_TIMER is released with the interval");
  assert.equal(page.scheduled(), 0);
});

test("the clamp leaves other routes and other periods alone", () => {
  for (const [hash, delay, why] of [
    ["#/analysis", 2000, "the analysis route has its own 2s poll and its own freeze"],
    ["#/settings", 4000, "a 4s interval on settings is the activity poll, not this one"],
    ["#/library", 2000, "no other route fetches scan status"],
  ]) {
    const page = fakePage(hash);
    const harness = install(page);
    let ran = 0;
    const id = harness.schedule(() => { ran += 1; }, delay);
    page.PAGE_TIMER = id;
    harness.adopt();
    page.tick(id);
    harness.sync();
    page.tick(id);
    harness.sync();
    page.tick(id);
    harness.sync();
    assert.equal(ran, 3, why);
    assert.ok(page.isScheduled(id), `${hash} @${delay}ms must keep polling: ${why}`);
  }
});

test("the settings clamp only counts the interval PAGE_TIMER owns", () => {
  const page = fakePage("#/settings");
  const harness = install(page);
  let owned = 0, other = 0;
  const ownedId = harness.schedule(() => { owned += 1; }, 2000);
  const otherId = harness.schedule(() => { other += 1; }, 2000);
  page.PAGE_TIMER = ownedId;
  harness.adopt();

  page.tick(otherId); harness.sync();
  page.tick(otherId); harness.sync();
  page.tick(otherId); harness.sync();
  assert.equal(other, 3, "an unowned 2s interval is not the route poll");
  assert.ok(page.isScheduled(otherId), "and must not be clamped");
  assert.equal(page.window.__plurxSettingsScanCaptureTick, false,
    "nor may it arm the boundary the capture waits on");

  page.tick(ownedId); harness.sync();
  page.tick(ownedId); harness.sync();
  assert.equal(owned, 2);
  assert.equal(page.isScheduled(ownedId), false);
  assert.equal(page.window.__plurxSettingsScanCaptureTick, true);
});
