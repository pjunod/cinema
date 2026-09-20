"use strict";

// Real client capture and refusal handling. The Rust HTTP regression feeds
// these old-buffer/new-destination shapes through validation and the actor.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const control = require("../../crates/plurxd/src/web/playback-control.js");
const {shellSource} = require("../web/shell-source.js");
const ui = shellSource().bodyScript;
function source(name) {
  const declarations = ["\nfunction ", "\nasync function "];
  const start = declarations.map(prefix => ui.indexOf(`${prefix}${name}(`)).find(i => i >= 0);
  assert.notEqual(start, undefined);
  const rest = ui.slice(start + 1);
  const ends = [...declarations, "\nconst ", "\nlet ", "\nwindow.", "\ndocument.", "\nsetInterval("]
    .map(prefix => rest.indexOf(prefix, 1)).filter(i => i >= 0);
  return rest.slice(0, Math.min(...ends));
}
const snapshot = new Function(`
  const ENDED_SLACK_SEC=3, PERSISTENT_STALL_MS=8000;
  function playbackControlSelection(){return {quality:{mode:'auto'},audio_track:0,
    subtitle:{mode:'off',track:null},audio_offset_ms:0,codec:'auto',dynamic_range:'auto'};}
  function playbackControlCapabilities(){return {platform:'web',max_height:1080,
    codecs:['h264'],dynamic_ranges:['sdr'],dual_player_preparation:false};}
  function pendingPlaybackControlAcknowledgement(){return null;}
  ${source("playbackControlBufferedRange")}
  ${source("playbackControlObservationOverride")}
  ${source("playbackControlSnapshot")}
  return playbackControlSnapshot;
`)();

// `executed` defaults true: a destination the attached element is actually
// seeking to. An unexecuted one belongs to a replacement that has not
// attached, and the R1 cases below are about exactly that difference.
function seekSnapshot(position, from, through, target, executed = true) {
  return snapshot({currentTime:position/1000,paused:false,seeking:false,readyState:4,
    playbackRate:1,buffered:{length:1,start:()=>from/1000,end:()=>through/1000}},
    {started:true,offset:0,wantsPlayback:true,durMs:6000000,
      controlSeek:{targetMs:target,executed}});
}

test("real mapper reports old buffer and new target independently", () => {
  const forward=seekSnapshot(10000,8000,24000,600000);
  assert.equal(forward.position_ms,10000);
  assert.equal(forward.buffered_through_ms,24000);
  assert.equal(forward.seek_target_ms,600000);
  const backward=seekSnapshot(600000,598000,620000,10000);
  assert.equal(backward.buffered_from_ms,598000);
  assert.equal(backward.seek_target_ms,10000);
});

// ---- R1: a pending or refused replacement is not the incumbent's seek -----

const SNAPSHOT_FIXTURES = JSON.parse(
  fs.readFileSync(path.join(__dirname, "seek-scratch-snapshots.json"), "utf8"),
);

function incumbentSnapshot(positionSec, pending) {
  return snapshot(
    {currentTime:positionSec,paused:false,seeking:false,readyState:4,playbackRate:1,
      buffered:{length:1,start:()=>98,end:()=>130}},
    {started:true,offset:0,wantsPlayback:true,durMs:6000000,
      source:{video_codec:"h264"},
      controlSeek:pending});
}

test("a destination no attachment has executed stays out of the incumbent's snapshot", () => {
  // The incident: the create for 174 s was refused, `controlSeek` stayed set,
  // and the incumbent kept reporting render_state=seeking with a foreign
  // seek_target_ms. Every such exchange reset the server's startup baseline,
  // so a visibly playing stream was reaped at first_served_at + 30 s.
  const pending=incumbentSnapshot(100,{targetMs:174000,executed:false});
  assert.equal(pending.render_state,"rendering",
    "the picture on screen is the incumbent's, and that is what it must report");
  assert.equal(pending.seek_target_ms,null,
    "a target no attached element is seeking to is not this stream's target");
  assert.equal(pending.position_ms,100000);

  const executing=incumbentSnapshot(100,{targetMs:130000,executed:true});
  assert.equal(executing.render_state,"seeking",
    "a local seek the element is actually performing still reports honestly");
  assert.equal(executing.seek_target_ms,130000);

  const none=incumbentSnapshot(100,null);
  assert.equal(none.render_state,"rendering");
  assert.equal(none.seek_target_ms,null);
});

test("the recorded snapshots the server regression replays are the ones this client emits", () => {
  // Both halves of R1 are tested against the same bytes: this asserts the
  // shipped mapper produces them, and
  // `seek_scratch_incumbent_survives_a_pending_replacement` feeds the same
  // file to the real startup actor. Neither side can drift alone.
  const pending=incumbentSnapshot(100,{targetMs:174000,executed:false});
  assert.deepEqual(pending,SNAPSHOT_FIXTURES.incumbent_while_replacement_pending);

  const progressed=incumbentSnapshot(100.3,{targetMs:174000,executed:false});
  assert.deepEqual(progressed,SNAPSHOT_FIXTURES.incumbent_after_250ms_of_progress);

  const seeking=incumbentSnapshot(100,{targetMs:130000,executed:true});
  assert.deepEqual(seeking,SNAPSHOT_FIXTURES.attached_local_seek_in_progress);

  const starting=snapshot(
    {currentTime:0,paused:false,seeking:false,readyState:0,playbackRate:1,
      buffered:{length:0,start:()=>0,end:()=>0}},
    {started:false,offset:0,wantsPlayback:true,durMs:6000000,
      source:{video_codec:"h264"},controlSeek:null});
  assert.deepEqual(starting,SNAPSHOT_FIXTURES.genuinely_unpresented_start);
});

test("genuinely malformed requests remain terminal rather than retrying forever", async () => {
  let current=seekSnapshot(10000,8000,24000,600000), sent=0;
  current.playback_rate=0; // Active demand requires at least 0.25 on the server.
  const reporter=new control.Reporter({
    bootstrap:{protocol:control.PROTOCOL,url:'/api/v1/hls/diagnosis/control',
      generation:'11111111-1111-4111-8111-111111111111',control_epoch:1,
      next_exchange_ms:5000,lease_timeout_ms:30000},
    clientInstanceId:'22222222-2222-4222-8222-222222222222',
    capture:()=>control.capture(current,1,{lifecycleId:'diagnosis',attachmentGeneration:1}),
    send:async()=>{sent++;throw Object.assign(new Error('invalid_control'),
      {status:400,code:'invalid_control',invalid_field:'playback_rate'});},
    setTimer:()=>1,clearTimer:()=>{},
  });
  reporter.start();
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(reporter.stopped,true);
  current=seekSnapshot(600000,598000,620000,600000);
  assert.equal(reporter.notify(),null);
  assert.equal(sent,1);
});

test("wire failures retain a bounded invalid field in the durable log message", async () => {
  const transport = new Function('fetch', `const TOKEN=null;${source('sendPlaybackControl')};return sendPlaybackControl;`);
  for (const invalidField of ['buffered_through_ms','buffered_from_ms','acknowledgement.state']) {
    const send=transport(async()=>({ok:false,status:400,json:async()=>({code:'invalid_control',invalid_field:invalidField})}));
    await assert.rejects(send('/control',{},null),error=>error.invalidField===invalidField
      &&error.message===`playback control invalid_control (${invalidField})`);
  }
  const send=transport(async()=>({ok:false,status:400,json:async()=>({code:'invalid_control',invalid_field:'bad\nfield'})}));
  await assert.rejects(send('/control',{},null),error=>error.invalidField===undefined
    &&error.message==='playback control invalid_control');
});

// ---- R2: relative input counts from the destination, and an identical -----
// ---- in-flight request is suppressed --------------------------------------

// The shipped relative-seek helpers, over a player that has committed a
// destination its attachment has not executed. No DOM: the two functions read
// `PLAYER`, `pbPosSec` and `pbTotalSec`, and all three are injected.
function seekBaseHarness(player, positionSec, totalSec) {
  return new Function(
    "PLAYER", "pbPosSec", "pbTotalSec",
    [
      source("unexecutedPlaybackDestinationSec"),
      source("pbRelativeSeekBase"),
      "return {base:pbRelativeSeekBase, destination:unexecutedPlaybackDestinationSec};",
    ].join("\n"),
  )(player, () => positionSec, () => totalSec);
}

test("a relative seek counts from the destination while a replacement is pending", () => {
  // The incident: the picture is still at 100 s because the create for 110 has
  // not landed, so the old base -- the attached element's clock -- made every
  // committed +10 land on 110 again.
  const player = {
    controlSeek: {targetMs: 110_000, executed: false},
    pendingMediaChange: {reason: "seek"},
    _seekPending: null,
  };
  const pending = seekBaseHarness(player, 100, 7200);
  assert.equal(pending.base(), 110,
    "three separately committed +10 taps must reach 130, not 110 three times");

  // A destination the attachment executed is where the picture is going, so
  // the element's own clock is the honest base again once it lands.
  player.controlSeek.executed = true;
  assert.equal(seekBaseHarness(player, 100, 7200).base(), 100);

  // And a destination with no replacement behind it says nothing at all:
  // `controlSeek` outlives the element, and `play()` carries it into a
  // successor with executed:false, so a replay from zero would otherwise
  // freeze the scrubber at the old destination forever.
  player.controlSeek = {targetMs: 174_000, executed: false};
  player.pendingMediaChange = null;
  assert.equal(seekBaseHarness(player, 0, 7200).destination(), null);
  assert.equal(seekBaseHarness(player, 0, 7200).base(), 0);

  // An uncommitted UI pending target still wins: that is the coalescing
  // window, and it is newer than anything committed.
  player.pendingMediaChange = {reason: "seek"};
  player._seekPending = 42;
  assert.equal(seekBaseHarness(player, 0, 7200).base(), 42);

  // Multipart audiobooks address the scrubber globally and `controlSeek`
  // locally; mixing the two would be worse than leaving them alone.
  assert.equal(
    seekBaseHarness(
      {controlSeek: {targetMs: 110_000, executed: false},
       pendingMediaChange: {reason: "seek"}, bookParts: [{id: 1}], _seekPending: null},
      100, 7200,
    ).destination(),
    null,
  );
});

function recipeHarness(player) {
  return new Function(
    "PLAYER", "selectedAudioIndex", "transcodeHeight", "qualityForce",
    [
      source("playbackChangeRecipeKey"),
      source("playbackChangeAlreadyInFlight"),
      "return {key:playbackChangeRecipeKey, inFlight:playbackChangeAlreadyInFlight};",
    ].join("\n"),
  )(player, () => 0, () => null, () => "auto");
}

test("an identical in-flight request is suppressed and a changed one is not", () => {
  const player = {fileId: 7, method: "remux", copyHls: true, curSub: -1, burnedSub: null,
    autoHeight: null, requestHdr10: false, preserveDolbyVision: false, aoffset: 0};
  const recipe = recipeHarness(player);
  const change = {method: "remux", copyHls: true, reason: "seek", height: null,
    forceReopen: false, previousSessionId: "s1", recoveryCause: "unknown"};

  player.inFlightChangeKey = recipe.key(player, 110, change);
  assert.equal(recipe.inFlight(player, 110, change), true,
    "the same command arriving twice is one command");

  assert.equal(recipe.inFlight(player, 120, change), false,
    "a different destination is a different request");

  // The whole point of keying the recipe and not the position: a track change
  // at the same second must still execute.
  const other = Object.assign({}, change);
  assert.equal(
    recipe.inFlight(
      Object.assign({}, player, {curSub: 3, inFlightChangeKey: player.inFlightChangeKey}),
      110, other),
    false,
    "a subtitle switch at the same second is not the seek already in flight");

  // And the key describes the request, not the player: a rung switch in
  // flight must not look like a plain seek on the incumbent recipe.
  assert.notEqual(
    recipe.key(player, 110, Object.assign({}, change, {height: 720})),
    recipe.key(player, 110, change),
  );
  assert.notEqual(
    recipe.key(player, 110, Object.assign({}, change, {forceReopen: true})),
    recipe.key(player, 110, change),
  );

  // Nothing in flight suppresses nothing.
  player.inFlightChangeKey = null;
  assert.equal(recipe.inFlight(player, 110, change), false);
});

test("the last two seconds of a title still deduplicate", () => {
  // `nudge` clamps to the runtime and `seekTo` clamps to runtime - 2, so a
  // guard that compared the raw argument would build a key from 7200 while
  // the in-flight execution was keyed on 7198 -- and every tap at the end of
  // a film would open another create for a byte-identical recipe.
  const player = {fileId: 7, method: "remux", copyHls: true, curSub: -1, burnedSub: null,
    autoHeight: null, requestHdr10: false, preserveDolbyVision: false, aoffset: 0};
  const recipe = recipeHarness(player);
  const change = {method: "remux", copyHls: true, reason: "seek", height: null,
    forceReopen: false, previousSessionId: "s1", recoveryCause: "unknown"};
  player.inFlightChangeKey = recipe.key(player, 7198, change);
  assert.equal(recipe.inFlight(player, 7198, change), true,
    "the guard has to run on the clamped target the execution was keyed on");
});
