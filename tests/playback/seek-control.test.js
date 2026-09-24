"use strict";

// Real client capture and refusal handling. The Rust HTTP regression feeds
// these old-buffer/new-destination shapes through validation and the actor.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const control = require("../../crates/plurxd/src/web/playback-control.js");
const policy = require("../../crates/plurxd/src/web/playback-policy.js");
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

test("rolling and progressive seeks stay local only inside advertised coverage", () => {
  const base = {method:"transcode", targetMs:50_000,
    bufferedMs:[{from:10_000,through:40_000}],
    publishedMs:{from:10_000,through:80_000}, holdbackMs:10_000};
  assert.deepEqual(policy.seekRoute({...base,targetMs:30_000}),
    {route:"local",atMs:30_000,basis:"buffered"});
  assert.deepEqual(policy.seekRoute(base),
    {route:"local",atMs:50_000,basis:"published"});
  assert.deepEqual(policy.seekRoute({...base,targetMs:75_000}),{route:"reopen"},
    "the last target duration is not yet safe to request");
  assert.deepEqual(policy.seekRoute({...base,targetMs:5_000}),{route:"reopen"},
    "evicted publication cannot be rediscovered locally");

  assert.equal(policy.seekRoute({method:"remux",targetMs:20_000,
    bufferedMs:[{from:10_000,through:30_000}]}).route,"local");
  assert.deepEqual(policy.seekRoute({method:"remux",targetMs:40_000,
    bufferedMs:[{from:10_000,through:30_000}]}),{route:"reopen"});
  assert.equal(policy.seekRoute({method:"direct_play",targetMs:40_000}).route,"local");
  assert.equal(policy.seekRoute({method:"transcode",vod:true,targetMs:40_000}).route,"local");
  assert.deepEqual(policy.seekRoute({...base,forceReopen:true}),{route:"reopen"});
  assert.deepEqual(policy.seekRoute({...base,changing:true}),{route:"reopen"});
});

function localSeekHarness({buffered, published, vod=false}) {
  const listeners=new Map(), timers=new Map(), changes=[], logs=[], stalls=[];
  const video={currentTime:10,buffered,seeking:false,paused:false,ended:false,
    addEventListener(name,fn){listeners.set(name,fn);},
    removeEventListener(name,fn){if(listeners.get(name)===fn)listeners.delete(name);}};
  const player={method:"transcode",copyHls:false,vod,offset:0,durMs:600_000,
    started:true,wantsPlayback:true,source:{video_codec:"h264"},controlHasFrameCallbacks:false,
    mediaAttachment:{id:1},pendingMediaChange:null,stallRecoveries:1,
    hls:{currentLevel:0,levels:[{details:published}]}};
  const api=new Function("policy","video","player","timers","changes","logs","stalls",[
    "let PLAYER=player,now=0;const performance={now:()=>now};",
    "const document={getElementById:()=>video,hidden:false};",
    "const PlaybackPolicy=policy,PERSISTENT_STALL_MS=8000;",
    "let nextTimerId=0;function setTimeout(fn,ms){if(ms===100){fn();return -1;}const id=++nextTimerId;timers.set(id,{fn,ms});return id;}",
    "function clearTimeout(id){timers.delete(id);} function markerNowMs(){return 0;} function pbTotalSec(){return 600;}",
    "function playbackChangeAlreadyInFlight(){return false;} function endWait(){}",
    "function restartPendingPlaybackOpen(){return false;} function hasPendingPlaybackOpen(){return false;}",
    "function beginPlaybackControlSeek(p,target){const value={targetMs:target*1000,executed:false};p.controlSeek=value;return value;}",
    "function markPlaybackControlSeekExecuted(p){p.controlSeek.executed=true;p.controlSeek.executedAt=now;p.controlSeek.frameFloor=0;}",
    "function playerActivity(){} function armStall(target,deadline){stalls.push({target,deadline});}",
    "function clientLog(value){logs.push(value);}",
    "function requestPlaybackMediaChange(p,change){changes.push({target:p.controlSeek.targetMs,change});}",
    "function play(){throw new Error('multipart route not expected');}",
    "function playbackSurfaceStep(){}function playbackSurfaceGeneration(){return 1;}",
    "function playbackOwnsAttachedMedia(){return true;}function samplePlaybackPresentationClock(){return 0;}",
    "function samplePreparedSwitchFrames(){}function streamHasVideo(){return true;}",
    "function completeHlsStartup(){}function clearStall(){}function finishStallRecovery(){}",
    "function bufferRunway(){return 0;}function persistentWait(){throw Error('unexpected persistent wait');}",
    "function notifyPlaybackControl(){}",
    source("playbackSeekBufferedRangesMs"),source("playbackSeekPublishedRangeMs"),
    source("playbackSeekBufferCovers"),source("settlePlaybackControlSeek"),
    source("playbackProgressTick"),source("seekTo"),
    "return {seekTo,logs,changes,stalls,tick(at){now=at;playbackProgressTick(video,player);},timerDelays(){return [...timers.values()].map(t=>t.ms);},async expire(ms=Infinity){for(const [id,timer] of [...timers])if(timer.ms<=ms){timers.delete(id);timer.fn();}for(let i=0;i<5;i+=1)await Promise.resolve();}};",
  ].join("\n"))(policy,video,player,timers,changes,logs,stalls);
  return {api,video,player,listeners};
}

test("a local seek that does not settle reopens once at the same target", async () => {
  const h=localSeekHarness({
    buffered:{length:0,start:()=>0,end:()=>0},
    published:{fragments:[{start:0}],edge:120,targetduration:10},
  });
  await h.api.seekTo(50);
  assert.equal(h.video.currentTime,50);
  assert.equal(h.api.logs[0].event,"seek_local");
  assert.match(h.api.logs[0].detail,/published$/);
  await h.api.expire();
  assert.equal(h.api.logs[1].event,"seek_local_fallback");
  assert.equal(h.api.changes.length,1);
  assert.equal(h.api.changes[0].target,50_000);
  assert.equal(h.api.changes[0].change.forceReopen,true);
});

test("buffer growth or seeked retires the local fallback", async () => {
  let through=60;
  const grown=localSeekHarness({
    buffered:{length:1,start:()=>40,end:()=>through},
    published:{fragments:[{start:0}],edge:120,targetduration:10},
  });
  await grown.api.seekTo(50);
  await grown.api.expire();
  assert.equal(grown.api.changes.length,0,"coverage at expiry proves the local seek can settle");

  const seeked=localSeekHarness({
    buffered:{length:0,start:()=>0,end:()=>0},
    published:{fragments:[{start:0}],edge:120,targetduration:10},
  });
  await seeked.api.seekTo(50);
  const event=seeked.listeners.get("seeked");
  if(event) event();
  await seeked.api.expire();
  assert.equal(seeked.api.changes.length,0,"seeked retires the timer without buffer polling");
});

test("an uncovered VOD seek stays local for 20 seconds and seeked alone does not settle it", async () => {
  const h=localSeekHarness({vod:true,
    buffered:{length:0,start:()=>0,end:()=>0},
    published:{fragments:[{start:0}],edge:120,targetduration:10},
  });
  await h.api.seekTo(50);
  assert.equal(h.api.logs[0].detail,"transcode:vod");
  assert.equal(h.api.changes.length,0);
  assert.deepEqual(h.api.timerDelays(),[policy.HLS_STARTUP.seek_deadline_ms]);
  assert.equal(h.player.controlSeek.localVodSeek,true);
  assert.equal(h.listeners.has("seeked"),false,
    "the browser event is not evidence that the target segment arrived");
  await h.api.expire(19_999);
  assert.equal(h.api.changes.length,0);
  await h.api.expire(20_000);
  assert.equal(h.api.changes.length,1);
  assert.equal(h.api.changes[0].target,50_000);
  assert.equal(h.api.changes[0].change.forceReopen,true);
  assert.equal(h.player.stallRecoveries,1);
  await h.api.expire();
  assert.equal(h.api.changes.length,1,"one local intent has one fallback");
});

test("target coverage or presentation retires the VOD fallback", async () => {
  let through=40;
  const covered=localSeekHarness({vod:true,
    buffered:{length:1,start:()=>30,end:()=>through},
    published:{fragments:[{start:0}],edge:120,targetduration:10},
  });
  await covered.api.seekTo(50);
  through=60;
  await covered.api.expire(20_000);
  assert.equal(covered.api.changes.length,0,"target bytes arrived on the attached rendition");

  const presented=localSeekHarness({vod:true,
    buffered:{length:0,start:()=>0,end:()=>0},
    published:{fragments:[{start:0}],edge:120,targetduration:10},
  });
  await presented.api.seekTo(50);
  presented.api.tick(0);
  presented.video.currentTime=50.1;
  presented.api.tick(100);
  assert.ok(presented.player.controlSeek,
    "control settlement can remain pending without a usable frame sequence");
  assert.equal(presented.player.controlSeek.localVodPresented,true);
  assert.deepEqual(presented.api.timerDelays(),[],"actual target progress clears the fallback");
  await presented.api.expire(20_000);
  assert.equal(presented.api.changes.length,0,"actual presentation cannot reopen a playing stream");
});

test("a newer VOD seek fences the prior seek's fallback", async () => {
  const h=localSeekHarness({vod:true,
    buffered:{length:0,start:()=>0,end:()=>0},
    published:{fragments:[{start:0}],edge:120,targetduration:10},
  });
  await h.api.seekTo(50);
  await h.api.seekTo(60);
  await h.api.expire(20_000);
  assert.deepEqual(h.api.changes.map(change=>change.target),[60_000]);
  assert.equal(h.player.stallRecoveries,1);
});

function vodWaitHarness() {
  return new Function("policy",[
    "let now=0,attempts=0;const performance={now:()=>now},document={hidden:false,getElementById:()=>v};",
    "const PlaybackPolicy=policy,PERSISTENT_STALL_MS=8000,STALL_MIN_MS=350;const timers=[];",
    "function setTimeout(fn,ms){timers.push({fn,ms,active:true});return timers.length;}function clearTimeout(id){if(timers[id-1])timers[id-1].active=false;}",
    "const v={currentTime:50,seeking:false,paused:false,ended:false,buffered:{length:0,start:()=>0,end:()=>0}};",
    "const p={vod:true,started:true,wantsPlayback:true,offset:0,waitAt:null,source:{video_codec:'h264'},controlHasFrameCallbacks:true,controlPresentedFrames:1,stallRecoveries:1,",
    "  controlSeek:{targetMs:50000,executed:true,executedAt:0,localVodSeek:true,localVodSeekFallbackPending:true,sequence:1}};let PLAYER=p;",
    "function playbackSurfaceStep(){}function playbackSurfaceGeneration(){return 1;}",
    "function playbackOwnsAttachedMedia(){return true;}function samplePlaybackPresentationClock(){}",
    "function samplePreparedSwitchFrames(){}function streamHasVideo(){return true;}",
    "function settlePlaybackControlSeek(){}function completeHlsStartup(){}",
    "function bufferRunway(){return 0;}function persistentWait(){attempts++;return Promise.resolve();}",
    "function recordWaitStall(){}function persistentWaitEvidence(){return {kind:'supply'};}",
    "function clearStall(){}function finishStallRecovery(){}",
    source("playbackSeekBufferedRangesMs"),source("playbackSeekBufferCovers"),
    source("endWait"),source("playbackProgressTick"),source("beginWait"),
    "return {p,v,timers,attempts:()=>attempts,tick(at){now=at;playbackProgressTick(v,p);},wait(at){now=at;beginWait(v);}};",
  ].join("\n"))(policy);
}

test("an unlanded VOD seek owns waiting and progress-watch clocks until its fallback", () => {
  const h=vodWaitHarness();
  h.tick(0);
  h.tick(8_100);
  h.wait(8_100);
  h.tick(19_999);
  h.tick(20_000);
  assert.equal(h.attempts(),0,"the generic stall cannot win the fallback deadline");
  assert.equal(h.p.waitAt,null,"seeked then waiting must not arm an 8 s timer");
  assert.equal(h.timers.length,0);

  h.p.controlSeek=null;
  h.p.progressWatch=null;
  h.tick(21_000);
  h.tick(29_001);
  assert.equal(h.attempts(),1,"a later genuine stall keeps the 8 s rule");
  assert.equal(h.p.stallRecoveries,1);
});

test("late target coverage starts a fresh 8-second presentation clock", () => {
  const h=vodWaitHarness();
  h.tick(0);
  h.tick(14_999);
  h.v.buffered={length:1,start:()=>40,end:()=>60};
  h.tick(15_000);
  h.tick(20_000);
  assert.equal(h.attempts(),0,"delivery time is not charged as decoder stall time");
  h.p.controlSeek.localVodSeekFallbackPending=false;
  h.tick(23_001);
  assert.equal(h.attempts(),1,"presentation still has the ordinary 8 s bound");
});

test("loss of target coverage cancels an earlier waiting timer", () => {
  const h=vodWaitHarness();
  h.tick(0);
  h.v.buffered={length:1,start:()=>40,end:()=>60};
  h.tick(1_000);
  h.wait(1_000);
  assert.equal(h.p.waitAt,1_000);
  assert.equal(h.timers[0].active,true);
  h.v.buffered={length:0,start:()=>0,end:()=>0};
  h.tick(1_500);
  assert.equal(h.p.waitAt,null);
  assert.equal(h.timers[0].active,false);
  assert.equal(h.attempts(),0);
});

test("an already due waiting timer rechecks missing VOD media before recovery", async () => {
  const h=new Function([
    "let ended=0;const p={started:true,waitAt:1000,wantsPlayback:true,",
    "controlSeek:{targetMs:50000,executed:true,localVodSeekFallbackPending:true}};let PLAYER=p;",
    "const v={seeking:false,paused:false};function playbackOwnsAttachedMedia(){return true;}",
    "function playbackSeekBufferCovers(){return false;}function endWait(){ended++;p.waitAt=null;}",
    source("persistentWait"),
    "return {run:()=>persistentWait(v,p,1000,0,0),ended:()=>ended,p};",
  ].join("\n"))();
  await h.run();
  assert.equal(h.ended(),1);
  assert.equal(h.p.waitAt,null);
});

test("the startup watchdog yields the shared 20-second boundary to the VOD fallback", async () => {
  let diagnosed=0;
  const diagnose=new Function("player","video","onDiagnose",[
    "const document={getElementById:()=>video};let PLAYER=player;",
    "function playbackSeekBufferCovers(){return false;}",
    "function hlsStartupIncomplete(){onDiagnose();return true;}",
    source("stallDiagnose"),"return stallDiagnose;",
  ].join("\n"))(
    {controlSeek:{targetMs:50_000,executed:true,localVodSeek:true,localVodSeekFallbackPending:true}},
    {currentTime:50},()=>{diagnosed++;});
  await diagnose();
  assert.equal(diagnosed,0,"the watchdog must not stop the attachment first");
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
