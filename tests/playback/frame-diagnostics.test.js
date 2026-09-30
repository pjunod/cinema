"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const {diagnosticSwitchCommit} = require("../../scripts/playback-lab");

test("frame diagnostics never attribute a previous switch to the next preparation", () => {
  const first = {at: 22_427, firstFrameAt: 22_428};
  const second = {at: 96_500, firstFrameAt: 96_540};
  assert.equal(diagnosticSwitchCommit(first, 18_000), first);
  assert.equal(diagnosticSwitchCommit(first, 92_000), null,
    "second-cliff preparation still retains the first commit on PLAYER");
  assert.equal(diagnosticSwitchCommit(second, 92_000), second,
    "exposure of the current successor supplies its own timestamps");
  assert.equal(diagnosticSwitchCommit(second, 100_000), null,
    "a later preparation must not inherit the second commit either");
  assert.equal(diagnosticSwitchCommit(null, 92_000), null);
  assert.equal(diagnosticSwitchCommit(first, null), null,
    "unavailable preparation timing cannot establish ownership");
  assert.equal(diagnosticSwitchCommit({at: NaN}, 92_000), null);
});
