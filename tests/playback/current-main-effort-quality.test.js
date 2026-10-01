"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const policy = require("../../crates/plurxd/src/web/playback-policy.js");
const fs = require("node:fs");
const vm = require("node:vm");

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

test("synchronous decode adapter consumes actual hourly switch budget", () => {
  const source = fs.readFileSync("crates/plurxd/src/web/player/stall-diagnosis.js", "utf8");
  const begin = source.indexOf("function autoDecodeMediaError(");
  const end = source.indexOf("\nfunction ", begin + 1);
  assert.ok(begin >= 0 && end > begin, "actual shipped adapter exists");
  const now = 4000000;
  let switches = 0;
  const context = vm.createContext({PlaybackPolicy: policy, performance: {now: () => now},
    qualityForce: () => "auto", hasPendingPlaybackOpen: () => false,
    switchAutoRung: () => { switches += 1; return Promise.resolve(); }});
  vm.runInContext(source.slice(begin, end), context);
  const player = {method: "transcode", health: {target_height: 1080},
    ladder: [{height: 720, total_kbps: 3000}, {height: 1080, total_kbps: 6000}],
    abr: {switchBudgetTimes: Array(6).fill(now - 1000), failedHeights: new Set()}};
  assert.equal(context.autoDecodeMediaError(player, {videoHeight: 1080}), false);
  assert.equal(switches, 0);
  assert.equal(player.abr.decodeStepConsumed, undefined);
  player.abr.switchBudgetTimes[0] = now - 3600001;
  assert.equal(context.autoDecodeMediaError(player, {videoHeight: 1080}), true);
  assert.equal(switches, 1);
  assert.equal(player.abr.decodeStepConsumed, true);
});
