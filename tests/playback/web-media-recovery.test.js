"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const policy = require("../../crates/plurxd/src/web/playback-policy.js");
const {shellSource} = require("../web/shell-source.js");

function declaration(name) {
  const source = shellSource().bodyScript;
  const plain = source.indexOf(`\nfunction ${name}(`);
  const start = plain >= 0 ? plain : source.indexOf(`\nasync function ${name}(`);
  assert.ok(start >= 0, `${name} must remain in the served shell`);
  const rest = source.slice(start + 1);
  const next = rest.search(/\n(?:async )?function /);
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
  assert.match(toggle, /seekTo\(at,true,null,false(?:,|\))/, "the replacement is a forced reopen");

  const shell = shellSource().bodyScript;
  const errorListener = shell.slice(shell.indexOf('v.addEventListener("error",async()=>{'));
  const nativePark = errorListener.indexOf("PlaybackPolicy.parksPausedPlaybackError(");
  assert.ok(nativePark >= 0 && nativePark < errorListener.indexOf("PlaybackPolicy.fallbackAction("),
    "Safari's native network error parks before the rescue ladder");

  const fatal = declaration("onHlsError");
  const park = fatal.indexOf("PlaybackPolicy.parksPausedPlaybackError(");
  assert.ok(park >= 0 && park < fatal.indexOf("playbackControlHlsFatal("),
    "a paused rolling fatal parks before any recovery or surface");
});

// Play after a pause of a minute or more is a natural quality boundary. It
// used to run through seekTo() at the position Play was pressed at; the Auto
// ask took about a second, so a retained route still seeked the attached VOD
// media back by that second (Safari, 2026-10-04: every `hls_loader_resumed`
// followed 1.2 s later by a `seek_local` with no viewer seek).
test("Play after a long pause never seeks when Auto keeps the route", async () => {
  const toggle = declaration("togglePlay");
  assert.doesNotMatch(toggle, /if\(qualityBoundary\) seekTo\(/,
    "the resume boundary does not seek before it knows the route changes");
  assert.match(toggle, /if\(qualityBoundary\) resumeQualityBoundary\(PLAYER\)/);

  const run = async ({picked, during = () => {}}) => {
    const seeks = [];
    const player = {controlSeek: undefined, mediaAttachment: 7, wantsPlayback: true};
    const scope = {
      PLAYER: player, position: 100,
      qualityForce: () => "auto",
      hasPendingPlaybackOpen: () => false,
      pbPosSec: () => scope.position,
      naturalBoundaryQualityCandidate: async () => {
        scope.position = 101.2;   // playback ran on while the ask was out
        during(scope, player);
        return picked;
      },
      seekTo: async (...args) => { seeks.push(args); },
    };
    const names = Object.keys(scope).filter((name) => name !== "position");
    const body = `${declaration("resumeQualityBoundary")}
      return (p) => resumeQualityBoundary(p);`;
    const make = new Function("scope", `with(scope){${body}}`);
    const moved = await make(scope)(player);
    assert.ok(names.length > 0);
    return {moved, seeks};
  };

  const kept = await run({picked: null});
  assert.equal(kept.moved, false);
  assert.deepEqual(kept.seeks, [], "a retained route leaves the playing media alone");

  const candidate = {id: "2", route: "encode", target_height: 720};
  const changed = await run({picked: candidate});
  assert.equal(changed.moved, true);
  assert.equal(changed.seeks.length, 1);
  assert.equal(changed.seeks[0][0], 101.2, "a picked route opens where playback has reached");
  assert.equal(changed.seeks[0][1], false);
  assert.equal(changed.seeks[0][3], false, "it is not a viewer seek");
  assert.equal(changed.seeks[0][6], candidate, "the picked route is handed over, not asked again");

  for (const during of [
    (scope, player) => { player.wantsPlayback = false; },
    (scope, player) => { player.controlSeek = {sequence: 1}; },
    (scope, player) => { player.mediaAttachment = 8; },
    (scope) => { scope.PLAYER = {}; },
  ]) {
    const superseded = await run({picked: candidate, during});
    assert.deepEqual(superseded.seeks, [], "a pause, seek, reattach or new player supersedes it");
  }

  const seek = declaration("seekTo");
  assert.match(seek, /const candidate=boundaryCandidate\|\|await naturalBoundaryQualityCandidate\(me,seekIntent\);/);
});
