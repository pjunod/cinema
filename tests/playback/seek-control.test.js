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
