"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const policy = require("../../crates/plurxd/src/web/playback-policy.js");

test("current main switch budget composes with effort decode-once protection", () => {
  const sample = {
    ladder: [
      {height: 720, total_kbps: 3000, peak_kbps: 4000},
      {height: 1080, total_kbps: 6000, peak_kbps: 8000},
    ],
    currentHeight: 1080,
    decodeStalls: 1,
    causeEvidence: {kind: "decode-failed", ageMs: 0},
    nowMs: 1000,
  };
  const exhausted = policy.decideRung({...sample, switchesThisPlaybackHour: 6});
  assert.equal(exhausted.height, 1080);
  assert.equal(exhausted.reason, "switch-budget");
  assert.equal(exhausted.action, "suppressed");
  const first = policy.decideRung(sample);
  assert.equal(first.height, 720);
  assert.equal(first.action, "switch");
  assert.deepEqual(first.blockedHeights, [1080]);
  const repeated = policy.decideRung({...sample, decodeStepConsumed: true});
  assert.equal(repeated.height, 1080);
  assert.equal(repeated.action, "suppressed");
});
