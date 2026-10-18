"use strict";

/*
 * The native shipping-app campaign is evidence only when the shaper carried
 * the media. A run whose app rediscovered the real server, or whose proxy
 * never saw a segment, must fail whatever the UI test reported — otherwise an
 * unshaped run reads as a pass, which is worse than no harness at all.
 */

const assert = require("node:assert/strict");
const { test } = require("node:test");
const path = require("node:path");

const check = require(path.join(__dirname, "..", "..", "scripts", "cq-native-shipping", "check-evidence.js"));

const stage = (index, label, kbps, media, entered = index * 60_000) => ({
  index, label, kbps, entered_at_ms: entered, bytes: media + 1_000, media_bytes: media,
});
const log = (...stages) => stages.map((s) => `CQEVT {"kind":"link_request","stage":${s},"mbps":10}`).join("\n");

test("every requested stage, and stage 0, must carry media through the proxy", () => {
  const shaped = { shaping: { stages: [stage(0, "start", 100_000, 316_000_000), stage(1, "pressure", 10_000, 65_000_000), stage(2, "recovery", 25_000, 187_000_000)] } };
  assert.deepEqual(check.requestedStages(log(1, 2), "auto"), [0, 1, 2]);
  assert.deepEqual(check.verify(shaped, log(1, 2), "auto"), []);
  assert.deepEqual(check.verify(shaped, log(1, 2), "campaign"), []);
});

test("a stage the test requested that carried no media fails the run", () => {
  const unshaped = { shaping: { stages: [stage(0, "start", 100_000, 316_000_000), stage(1, "pressure", 10_000, 0)] } };
  const problems = check.verify(unshaped, log(1), "auto");
  assert.equal(problems.length, 1);
  assert.match(problems[0], /stage 1 .*no media through the proxy/);
});

test("a stage the test requested that the shaper never applied fails the run", () => {
  const stages = [stage(0, "start", 100_000, 316_000_000), { index: 1, label: "pressure", kbps: 10_000, entered_at_ms: null, bytes: 0, media_bytes: 0 }];
  assert.match(check.verify({ shaping: { stages } }, log(1), "auto").join(";"), /stage 1 .*never applied/);
  assert.match(check.verify({ shaping: { stages: stages.slice(0, 1) } }, log(1), "auto").join(";"), /no such stage/);
});

test("a run the app took around the proxy fails even when nothing was requested", () => {
  const empty = { shaping: { stages: [stage(0, "start", 100_000, 0, 0)] } };
  empty.shaping.stages[0].bytes = 0;
  assert.match(check.verify(empty, "", "home").join(";"), /no bytes at all/);
  assert.match(check.verify(empty, "", "campaign").join(";"), /no bytes at all/);
  assert.match(check.verify({}, "", "campaign").join(";"), /no bytes at all/);
  // Media is owed by a playing phase from its first stage; a library page is not media.
  const library = { shaping: { stages: [{ index: 0, label: "start", kbps: 100_000, entered_at_ms: 0, bytes: 40_000, media_bytes: 0 }] } };
  assert.deepEqual(check.verify(library, "", "home"), []);
  assert.match(check.verify(library, "", "campaign").join(";"), /stage 0 .*no media/);
});
