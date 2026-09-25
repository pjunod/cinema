"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const policy = require("../../crates/plurxd/src/web/playback-policy.js");
const {shellSource} = require("../web/shell-source.js");

function declaration(name) {
  const source = shellSource().bodyScript;
  const start = source.indexOf(`\nfunction ${name}(`);
  assert.ok(start >= 0, `${name} must remain in the served shell`);
  const rest = source.slice(start + 1);
  const next = rest.indexOf("\nfunction ", 1);
  return next < 0 ? rest : rest.slice(0, next);
}

test("media recovery shares the attach budget and never loops across reopens", () => {
  const decide = (overrides = {}) => policy.hlsMediaFatalAction({
    type: "mediaError",
    details: "fragParsingError",
    retryUsed: 0,
    itemRecoveries: 0,
    recoveredAtMs: null,
    nowMs: 10_000,
    ...overrides,
  });
  assert.equal(decide(), "recover");
  assert.equal(decide({ retryUsed: 1 }), "fallback");
  assert.equal(decide({ itemRecoveries: 1, recoveredAtMs: 9_000 }), "fallback");
  assert.equal(decide({ itemRecoveries: 2 }), "fallback");
  assert.equal(decide({ details: "bufferIncompatibleCodecsError" }), "fallback");
  assert.equal(decide({ details: "bufferAddCodecError" }), "fallback");
  assert.equal(decide({
    details: "bufferAppendError",
    sourceBufferName: "audio",
    itemRecoveries: 1,
    recoveredAtMs: 5_000,
  }), "swap_audio");
  assert.equal(decide({
    details: "bufferAppendError",
    sourceBufferName: "video",
    itemRecoveries: 1,
    recoveredAtMs: 5_000,
  }), "fallback");
  assert.equal(decide({ type: "networkError" }), "none");
});

test("the current fatal handler rescues before reporting terminal failure", () => {
  const handler = declaration("onHlsError");
  const recovery = handler.indexOf("PlaybackPolicy.hlsMediaFatalAction");
  const terminal = handler.indexOf('notifyPlaybackControl("failed"', recovery);
  assert.ok(recovery >= 0);
  assert.ok(terminal > recovery);
  assert.match(handler, /sourceBufferName:d\.sourceBufferName\|\|null/);
  assert.match(handler, /attachedPlayer\.hlsRetryUsed=.*\+1/);
});
