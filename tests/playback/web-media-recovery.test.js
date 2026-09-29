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

// The server retires a paused rolling session after 180 s (ROLLING_PAUSE_GRACE)
// and answers 410 pause_grace_expired. The contract (§9.5): stay paused, and on
// Play open one replacement at the saved position rather than resume a loader
// into a playlist that now answers 404/410.
test("a paused rolling session's failure parks and Play reopens it", () => {
  const parks = (o) => policy.parksPausedPlaybackError(Object.assign(
    {wantsPlayback: false, sessionId: "s1", vod: false, networkFailure: true}, o));
  assert.equal(parks({}), true);
  assert.equal(parks({wantsPlayback: true}), false, "a viewer who is watching keeps recovery");
  assert.equal(parks({wantsPlayback: undefined}), false, "unknown intent is not a pause");
  assert.equal(parks({vod: true}), false, "VOD stays alive while paused");
  assert.equal(parks({sessionId: null}), false, "direct play has no session to retire");
  assert.equal(parks({networkFailure: false}), false, "a media fatal is not a retirement");

  assert.equal(policy.isPauseGraceExpiry({status: 410, code: "pause_grace_expired"}), true);
  assert.equal(policy.isPauseGraceExpiry({status: 410, code: "media_session_ended"}), false);
  assert.equal(policy.isPauseGraceExpiry(null), false);

  assert.equal(policy.pausedRetirementCurrent({sessionId: "s1"}, "s1"), true);
  assert.equal(policy.pausedRetirementCurrent({sessionId: "s1"}, "s2"), false,
    "a successor retires the latch by identity");
  assert.equal(policy.pausedRetirementCurrent(null, "s1"), false);

  const toggle = declaration("togglePlay");
  const latch = toggle.indexOf("PlaybackPolicy.pausedRetirementCurrent(");
  assert.ok(latch >= 0 && latch < toggle.indexOf("resumeHlsStartup(v,PLAYER)"),
    "Play checks the latch before resuming the loader in place");
  assert.ok(toggle.includes("seekTo(at,true,null,false)"), "the replacement is a forced reopen");

  const shell = shellSource().bodyScript;
  const errorListener = shell.slice(shell.indexOf('v.addEventListener("error",()=>{'));
  const nativePark = errorListener.indexOf("PlaybackPolicy.parksPausedPlaybackError(");
  assert.ok(nativePark >= 0 && nativePark < errorListener.indexOf("PlaybackPolicy.fallbackAction("),
    "Safari's native network error parks before the rescue ladder");

  const fatal = declaration("onHlsError");
  const park = fatal.indexOf("PlaybackPolicy.parksPausedPlaybackError(");
  assert.ok(park >= 0 && park < fatal.indexOf("playbackControlHlsFatal("),
    "a paused rolling fatal parks before any recovery or surface");
});
