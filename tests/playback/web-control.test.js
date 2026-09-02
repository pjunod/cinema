"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const control = require("../../crates/plurxd/src/web/playback-control.js");

const SHIPPED_UI = fs.readFileSync(
  path.join(__dirname, "../../crates/plurxd/src/web/index.html"),
  "utf8",
);
const DECLARATIONS = ["\nfunction ", "\nasync function "];
const TERMINATORS = DECLARATIONS.concat(["\nconst ", "\nlet ", "\nwindow.", "\ndocument."]);
function shippedSource(name) {
  const start = DECLARATIONS.map((kind) => SHIPPED_UI.indexOf(`${kind}${name}(`))
    .find((at) => at !== -1);
  assert.notEqual(start, undefined, `index.html no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = TERMINATORS.map((kind) => rest.indexOf(kind, 1)).filter((at) => at !== -1);
  return (ends.length ? rest.slice(0, Math.min(...ends)) : rest).trimEnd();
}

function bootstrap() {
  return {
    protocol: control.PROTOCOL,
    url: "/api/v1/hls/session-1/control",
    generation: "11111111-1111-4111-8111-111111111111",
    control_epoch: 7,
    next_exchange_ms: 5_000,
    lease_timeout_ms: 300_000,
  };
}

function snapshot(position = 1_000, render = "rendering") {
  return {
    demand: "active",
    position_ms: position,
    buffered_from_ms: position,
    buffered_through_ms: position + 10_000,
    playback_rate: 1,
    render_state: render,
    seek_target_ms: null,
    observed_download_bps: 8_000_000,
    selection: {
      quality: { mode: "auto" },
      audio_track: 0,
      subtitle: { mode: "off", track: null },
      audio_offset_ms: 0,
      codec: "auto",
      dynamic_range: "auto",
    },
    capabilities: {
      platform: "web",
      max_height: 1080,
      codecs: ["h264"],
      dynamic_ranges: ["sdr"],
      dual_player_preparation: false,
    },
    observation: null,
    acknowledgement: null,
  };
}

function response(request) {
  return {
    protocol: control.PROTOCOL,
    generation: request.generation,
    control_epoch: request.control_epoch,
    accepted_sequence: request.sequence,
    action: { type: "none" },
  };
}

function controlError(status, code, fields = {}) {
  const error = new Error(code);
  Object.assign(error, { status, code }, fields);
  return error;
}

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

async function flush() {
  await Promise.resolve();
  await Promise.resolve();
}

async function main() {
  assert.equal(control.validBootstrap(bootstrap()), true);
  assert.equal(control.validBootstrap(Object.assign(bootstrap(), { control_epoch: 0 })), false);
  assert.equal(control.validBootstrap(Object.assign(bootstrap(), { url: "https://attacker/control" })), false);
  assert.equal(control.validBootstrap(Object.assign(bootstrap(), { lease_timeout_ms: 1_000 })), false);
  const secondRequest={generation:bootstrap().generation,control_epoch:7,sequence:2};
  assert.equal(control.validResponse(bootstrap(),secondRequest,
    Object.assign(response(secondRequest),{accepted_sequence:1})),false);

  const subtitleReadinessMeansReady=new Function([
    shippedSource("subtitleReadinessMeansReady"),
    "return subtitleReadinessMeansReady;",
  ].join("\n"))();
  for(const [label,delivery,expected] of [
    ["ready",{subtitle_readiness:"ready"},true],
    ["warming",{subtitle_readiness:"warming"},false],
    ["unavailable",{subtitle_readiness:"unavailable"},false],
    ["absent",{},false],
    ["unknown",{subtitle_readiness:"a_value_from_next_year"},false],
    ["empty",{subtitle_readiness:""},false],
  ]){
    assert.equal(subtitleReadinessMeansReady(delivery),expected,
      `${label} readiness has one closed consumer decision`);
  }
  const subtitleReadinessRetryTransition=new Function([
    shippedSource("subtitleReadinessMeansReady"),
    shippedSource("subtitleReadinessRetryTransition"),
    "return subtitleReadinessRetryTransition;",
  ].join("\n"))();
  const readinessState={};
  assert.equal(subtitleReadinessRetryTransition(readinessState,{}),false);
  assert.equal(subtitleReadinessRetryTransition(readinessState,
    {subtitle_readiness:"warming"}),false);
  assert.equal(subtitleReadinessRetryTransition(readinessState,
    {subtitle_readiness:"ready"}),true,"warming to ready directs one retry");
  assert.equal(subtitleReadinessRetryTransition(readinessState,
    {subtitle_readiness:"ready"}),false,"repeated ready cannot retry on cadence");
  for(const delivery of [
    {subtitle_readiness:"warming"},
    {subtitle_readiness:"unavailable"},
    {subtitle_readiness:"a_value_from_next_year"},
    {},
  ]){
    const isolated={};
    assert.equal(subtitleReadinessRetryTransition(isolated,delivery),false,
      "non-ready readiness never directs a retry");
  }
  const retryReadyNativeSubtitle=new Function([
    shippedSource("nativeHlsSubtitleOrdinal"),
    shippedSource("retryReadyNativeSubtitle"),
    "return retryReadyNativeSubtitle;",
  ].join("\n"))();
  const subtitleTrackWrites=[];
  const hls={};
  Object.defineProperty(hls,"subtitleTrack",{
    set(value){ subtitleTrackWrites.push(value); },
  });
  const nativePlayer={
    hls,
    sessionId:"same-video-session",
    burnedSub:null,
    curSub:7,
    subs:[
      {index:3,native:false},
      {index:5,native:true},
      {index:7,native:true},
    ],
  };
  const directed={};
  let retries=0;
  for(const delivery of [
    {subtitle_readiness:"warming"},
    {subtitle_readiness:"ready"},
    {subtitle_readiness:"ready"},
  ]){
    if(subtitleReadinessRetryTransition(directed,delivery)
      &&retryReadyNativeSubtitle(nativePlayer)) retries++;
  }
  assert.equal(retries,1,"one readiness edge performs one directed retry");
  assert.deepEqual(subtitleTrackWrites,[-1,1],
    "the retry toggles only the selected native rendition on the same HLS object");
  assert.equal(nativePlayer.sessionId,"same-video-session",
    "subtitle retry does not replace the video session");
  assert.equal(retryReadyNativeSubtitle(Object.assign({},nativePlayer,{curSub:3})),false,
    "a non-native text track cannot be driven through the HLS rendition path");
  assert.equal(retryReadyNativeSubtitle(Object.assign({},nativePlayer,{burnedSub:7})),false,
    "a burned selection is never independently retried");

  const first = deferred();
  const calls = [];
  const timers = [];
  let now = 0;
  const reporter = new control.Reporter({
    bootstrap: bootstrap(),
    clientInstanceId: "22222222-2222-4222-8222-222222222222",
    snapshot: () => snapshot(1_000),
    send: async (url, request) => {
      calls.push({ url, request });
      if (calls.length === 1) return first.promise;
      return response(request);
    },
    setTimer: (run, ms) => { timers.push({ run, ms }); return timers.length; },
    clearTimer: () => {},
    now: () => now,
  });

  reporter.start();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].request.sequence, 1);
  assert.equal(calls[0].request.capabilities.platform, "web");

  reporter.notify(snapshot(2_000, "waiting"));
  reporter.notify(snapshot(3_000, "stalled"));
  assert.equal(calls.length, 1, "only one exchange may be in flight");

  first.resolve(response(calls[0].request));
  await flush();
  assert.equal(calls.length, 1, "the client respects the server's admission floor");
  const cooldown=timers.find(timer => timer.ms === 250);
  assert.ok(cooldown, "a burst is coalesced behind the 250 ms control floor");
  now=250;
  cooldown.run();
  await flush();
  assert.equal(calls.length, 2);
  assert.equal(calls[1].request.sequence, 2);
  assert.equal(calls[1].request.position_ms, 3_000, "newest queued snapshot wins");
  assert.equal(calls[1].request.render_state, "stalled");
  assert.equal("capabilities" in calls[1].request, false, "capabilities ride sequence one only");
  await flush();
  assert.equal(timers.at(-1).ms, 5_000);
  assert.deepEqual(reporter.status(), {
    sequence: 2,
    accepted_sequence: 2,
    in_flight: false,
    pending: false,
    retrying: false,
    stopped: false,
    next_exchange_ms: 5_000,
    lease_timeout_ms: 300_000,
  });
  assert.deepEqual(reporter.legacyContext(), {
    generation: bootstrap().generation,
    control_epoch: 7,
    sequence: 2,
    demand: "active",
    render_state: "stalled",
    position_ms: 3_000,
    buffered_through_ms: 13_000,
    observed_download_bps: 8_000_000,
    observation: null,
  });
  const changedCapabilities=snapshot(4_000);
  changedCapabilities.capabilities.max_height=720;
  reporter.notify(changedCapabilities);
  const capabilityCooldown=timers.filter(timer=>timer.ms===250).at(-1);
  now=500;
  capabilityCooldown.run();
  await flush();
  assert.equal(calls.length,3);
  assert.equal(calls[2].request.capabilities.max_height,720,
    "changed dynamic capabilities ride a later sequence");

  const cancellation = deferred();
  let signal;
  let failures = 0;
  const canceled = new control.Reporter({
    bootstrap: bootstrap(),
    clientInstanceId: "33333333-3333-4333-8333-333333333333",
    snapshot: () => snapshot(),
    send: async (_url, _request, requestSignal) => {
      signal = requestSignal;
      await cancellation.promise;
      const error = new Error("canceled"); error.name = "AbortError"; throw error;
    },
    onExchange: ({ error }) => { if (error) failures += 1; },
  }).start();
  canceled.stop();
  assert.equal(signal.aborted, true);
  cancellation.resolve();
  await flush();
  assert.equal(failures, 0, "cancellation is control flow, not recovery evidence");
  assert.equal(canceled.legacyContext(), null);

  let invalidResponses = 0;
  const mismatched = new control.Reporter({
    bootstrap: bootstrap(),
    clientInstanceId: "44444444-4444-4444-8444-444444444444",
    snapshot: () => snapshot(),
    send: async (_url, request) => Object.assign(response(request), { action: { type: "replace" } }),
    onExchange: ({ error }) => { if (error) invalidResponses += 1; },
  }).start();
  await flush();
  assert.equal(invalidResponses, 1, "the passive reporter rejects an active control action");
  assert.equal(mismatched.status().stopped, true);
  mismatched.stop();

  const retryTimers=[];
  const retryCalls=[];
  let retryNow=0;
  const retrying=new control.Reporter({
    bootstrap:bootstrap(),
    clientInstanceId:"55555555-5555-4555-8555-555555555555",
    snapshot:()=>snapshot(7_000),
    send:async (_url,request)=>{
      retryCalls.push(request);
      if(retryCalls.length===1) throw new Error("connection reset before response");
      return response(request);
    },
    setTimer:(run,ms)=>{ retryTimers.push({run,ms}); return retryTimers.length; },
    clearTimer:()=>{},
    now:()=>retryNow,
  }).start();
  await flush();
  assert.equal(retrying.status().retrying,true);
  retryNow=5_000;
  retryTimers.find(timer=>timer.ms===5_000).run();
  await flush();
  assert.equal(retryCalls.length,2);
  assert.deepEqual(retryCalls[1],retryCalls[0],"an unacknowledged exchange retries exactly");
  assert.equal(retrying.status().accepted_sequence,1);
  retrying.stop();

  const deadlineTimers=[];
  let deadlineFailures=0;
  const timedOut=new control.Reporter({
    bootstrap:bootstrap(),
    clientInstanceId:"66666666-6666-4666-8666-666666666666",
    snapshot:()=>snapshot(),
    send:async (_url,_request,signal)=>new Promise((_resolve,reject)=>{
      signal.addEventListener("abort",()=>{
        const error=new Error("aborted"); error.name="AbortError"; reject(error);
      },{once:true});
    }),
    setTimer:(run,ms)=>{ deadlineTimers.push({run,ms}); return deadlineTimers.length; },
    clearTimer:()=>{},
    onExchange:({error})=>{ if(error&&error.name==="TimeoutError") deadlineFailures+=1; },
  }).start();
  deadlineTimers.find(timer=>timer.ms===6_000).run();
  await flush();
  assert.equal(deadlineFailures,1);
  assert.equal(timedOut.status().retrying,true);
  timedOut.stop();

  for (const temporary of [
    { status:425, code:"owner_transition", delay:700 },
    { status:429, code:"control_rate_limited", delay:900 },
    { status:503, code:"control_unavailable", delay:500 },
  ]) {
    const temporaryTimers=[];
    const temporaryCalls=[];
    let temporaryNow=0;
    const temporaryReporter=new control.Reporter({
      bootstrap:bootstrap(),
      clientInstanceId:"77777777-7777-4777-8777-777777777777",
      snapshot:()=>snapshot(8_000),
      send:async (_url,request)=>{
        temporaryCalls.push(request);
        if(temporaryCalls.length===1) throw controlError(temporary.status,temporary.code,
          {retryAfterMs:temporary.delay});
        return response(request);
      },
      setTimer:(run,ms)=>{ temporaryTimers.push({run,ms}); return temporaryTimers.length; },
      clearTimer:()=>{},
      now:()=>temporaryNow,
    }).start();
    await flush();
    assert.equal(temporaryReporter.status().retrying,true);
    const retryTimer=temporaryTimers.find(timer=>timer.ms===temporary.delay);
    assert.ok(retryTimer,`${temporary.code} honors the server retry hint`);
    temporaryNow=temporary.delay;
    retryTimer.run();
    await flush();
    assert.deepEqual(temporaryCalls[1],temporaryCalls[0],
      `${temporary.code} retries the exact unacknowledged request`);
    assert.equal(temporaryReporter.status().accepted_sequence,1);
    temporaryReporter.stop();
  }

  const ownerTimers=[];
  const ownerCalls=[];
  let ownerNow=0;
  const newGeneration="99999999-9999-4999-8999-999999999999";
  const ownerReporter=new control.Reporter({
    bootstrap:bootstrap(),
    clientInstanceId:"88888888-8888-4888-8888-888888888888",
    snapshot:()=>snapshot(9_000),
    send:async (_url,request)=>{
      ownerCalls.push(request);
      if(ownerCalls.length===1) throw controlError(409,"owner_changed",
        {generation:newGeneration,controlEpoch:8});
      return response(request);
    },
    setTimer:(run,ms)=>{ ownerTimers.push({run,ms}); return ownerTimers.length; },
    clearTimer:()=>{},
    now:()=>ownerNow,
  }).start();
  await flush();
  const ownerFence=ownerTimers.find(timer=>timer.ms===250);
  assert.ok(ownerFence,"owner change starts a fresh sequence behind the admission floor");
  ownerNow=250;
  ownerFence.run();
  await flush();
  assert.equal(ownerCalls[1].sequence,1);
  assert.equal(ownerCalls[1].generation,newGeneration);
  assert.equal(ownerCalls[1].control_epoch,8);
  assert.equal(ownerCalls[1].capabilities.platform,"web");
  ownerReporter.stop();

  const late=deferred();
  let lateCompletions=0;
  const predecessor=new control.Reporter({
    bootstrap:bootstrap(),
    clientInstanceId:"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    snapshot:()=>snapshot(),
    send:async ()=>late.promise,
    onExchange:()=>{ lateCompletions+=1; },
  }).start();
  predecessor.stop();
  late.resolve(response({generation:bootstrap().generation,control_epoch:7,sequence:1}));
  await flush();
  assert.equal(lateCompletions,0,"a stopped predecessor discards a late success");
  assert.equal(predecessor.status().accepted_sequence,0);

  const triggerDeferred=deferred();
  const triggerReporter=new control.Reporter({
    bootstrap:bootstrap(),
    clientInstanceId:"abababab-abab-4bab-8bab-abababababab",
    snapshot:()=>snapshot(),
    send:async ()=>triggerDeferred.promise,
  }).start();
  const typedTrigger=snapshot(12_000,"failed");
  typedTrigger.observation={
    decoder_state:"failed",error_code:"decoder",error_detail:"persistent_decode_stall",
    ignored_private_field:"must not enter log context",
  };
  const triggerContext=triggerReporter.notify(typedTrigger);
  assert.deepEqual(triggerContext.observation,{
    decoder_state:"failed",error_code:"decoder",error_detail:"persistent_decode_stall",
  },"the real reporter preserves bounded typed evidence in an immediate trigger context");
  triggerReporter.stop();
  triggerDeferred.resolve(response({generation:bootstrap().generation,control_epoch:7,sequence:1}));
  await flush();

  const acceptedTypedSnapshot=snapshot(13_000,"failed");
  acceptedTypedSnapshot.observation={
    decoder_state:"failed",error_code:"decoder",error_detail:"videoDecodeError",
  };
  const acceptedTypedReporter=new control.Reporter({
    bootstrap:bootstrap(),
    clientInstanceId:"acacacac-acac-4cac-8cac-acacacacacac",
    snapshot:()=>acceptedTypedSnapshot,
    send:async (_url,request)=>response(request),
  }).start();
  await flush();
  assert.deepEqual(acceptedTypedReporter.legacyContext().observation,{
    decoder_state:"failed",error_code:"decoder",error_detail:"videoDecodeError",
  },"an accepted typed observation survives in legacy recovery context");
  acceptedTypedReporter.stop();

  const endTimers=[];
  const endingSnapshot=snapshot(60_000,"ended");
  endingSnapshot.demand="end";
  const ending=new control.Reporter({
    bootstrap:bootstrap(),
    clientInstanceId:"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    snapshot:()=>endingSnapshot,
    send:async (_url,request)=>response(request),
    setTimer:(run,ms)=>{ endTimers.push({run,ms}); return endTimers.length; },
    clearTimer:()=>{},
  }).start();
  await flush();
  assert.equal(ending.status().stopped,true,"accepted end is terminal for this controller");
  assert.equal(endTimers.some(timer=>timer.ms===5_000),false,
    "accepted end never arms another cadence exchange");

  const endRaceResponse=deferred();
  const endRaceSnapshot=snapshot(60_000,"ended");
  endRaceSnapshot.demand="end";
  const oldReplayController=new control.Reporter({
    bootstrap:bootstrap(),
    clientInstanceId:"cccccccc-cccc-4ccc-8ccc-cccccccccccc",
    snapshot:()=>endRaceSnapshot,
    send:async ()=>endRaceResponse.promise,
  }).start();
  oldReplayController.notify(snapshot(0,"rendering"));
  assert.equal(oldReplayController.status().pending,true,
    "a replay signal can arrive while terminal end is still in flight");
  oldReplayController.stop();
  const replacementRequests=[];
  const freshReplayController=new control.Reporter({
    bootstrap:Object.assign(bootstrap(),{generation:"dddddddd-dddd-4ddd-8ddd-dddddddddddd"}),
    clientInstanceId:"eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
    snapshot:()=>snapshot(0,"rendering"),
    send:async (_url,request)=>{ replacementRequests.push(request); return response(request); },
  }).start();
  await flush();
  endRaceResponse.resolve(response({generation:bootstrap().generation,control_epoch:7,sequence:1}));
  await flush();
  assert.equal(oldReplayController.status().accepted_sequence,0,
    "the stopped terminal controller discards its late end response and queued active snapshot");
  assert.equal(replacementRequests.length,1);
  assert.equal(replacementRequests[0].sequence,1,
    "replay starts in a fresh controller sequence space");
  assert.equal(freshReplayController.status().accepted_sequence,1);
  freshReplayController.stop();

  const adapter = new Function(
    "PLAY_CAPS", "screen", "window", "playQuality", "selectedAudioIndex",
    "PERSISTENT_STALL_MS", "ENDED_SLACK_SEC", "Hls",
    [
      shippedSource("playbackControlCapabilities"),
      shippedSource("playbackControlSelection"),
      shippedSource("playbackControlBufferedRange"),
      shippedSource("playbackControlObservationOverride"),
      shippedSource("playbackControlHlsFatal"),
      shippedSource("playbackControlSnapshot"),
      "let PLAYER=null;",
      shippedSource("notifyPlaybackControl"),
      "return {playbackControlSnapshot,playbackControlHlsFatal,notify(player,render,observation){"+
        "PLAYER=player; const result=notifyPlaybackControl(render,observation); PLAYER=null; return result; }};",
    ].join("\n"),
  )(
    {vcodec:"h264,hevc",maxheight:2160,hdr10t:1,dv:0},
    {height:1080},
    {innerHeight:720,devicePixelRatio:1},
    ()=>"auto",
    ()=>0,
    8_000,
    15,
    {ErrorTypes:{MEDIA_ERROR:"mediaError",NETWORK_ERROR:"networkError"}},
  );
  const video={
    currentTime:10,paused:false,ended:false,seeking:false,readyState:4,playbackRate:1,error:null,
    buffered:{length:1,start:()=>8,end:()=>24},
    getVideoPlaybackQuality:()=>({droppedVideoFrames:2}),
  };
  const player={offset:5,bookOffset:0,durMs:60_000,knownDur:60_000,started:true,
    waitAt:null,_seekPreview:null,source:{height:1080},curSub:-1,burnedSub:null,aoffset:0,hls:null};
  assert.equal(adapter.playbackControlSnapshot(video,player).position_ms,15_000);
  video.paused=true;
  assert.equal(adapter.playbackControlSnapshot(video,player).demand,"hold");
  video.paused=false; video.seeking=true; player._seekPreview=20;
  const seeking=adapter.playbackControlSnapshot(video,player);
  assert.equal(seeking.render_state,"seeking");
  assert.equal(seeking.seek_target_ms,20_000);
  video.seeking=false; player._seekPreview=null;
  player.waitAt=performance.now()-9_000;
  const inferredSupply=adapter.playbackControlSnapshot(video,player);
  assert.equal(inferredSupply.render_state,"stalled");
  assert.equal(inferredSupply.observation.decoder_state,"starved");
  player.controlReporter={notify:()=>adapter.playbackControlSnapshot(video,player)};
  const supplyEvidence=adapter.notify(player,"stalled",{decoder_state:"starved"});
  assert.deepEqual(supplyEvidence.observation,
    {decoder_state:"starved",dropped_frames:2});
  const decodeEvidence=adapter.notify(player,"stalled",{
    decoder_state:"failed",error_code:"decoder",error_detail:"persistent_decode_stall",
  });
  assert.deepEqual(decodeEvidence.observation,{
    decoder_state:"failed",error_code:"decoder",error_detail:"persistent_decode_stall",
    dropped_frames:2,
  });
  player.controlObservationOverride=null;
  player.controlRenderOverride=null;
  player.waitAt=null; video.ended=true; video.paused=true; video.currentTime=20;
  const truncated=adapter.playbackControlSnapshot(video,player);
  assert.equal(truncated.demand,"active");
  assert.equal(truncated.render_state,"failed");
  video.currentTime=60;
  const complete=adapter.playbackControlSnapshot(video,player);
  assert.equal(complete.demand,"end");
  assert.equal(complete.render_state,"ended");

  assert.deepEqual(adapter.playbackControlHlsFatal(
    {type:"networkError",details:"fragLoadError"},true),{
    media_failure:false,
    observation:{decoder_state:"ready",error_code:"network",error_detail:"fragLoadError"},
  });
  assert.deepEqual(adapter.playbackControlHlsFatal(
    {type:"networkError",details:"manifestLoadError"},false),{
    media_failure:false,
    observation:{decoder_state:"unknown",error_code:"manifest",error_detail:"manifestLoadError"},
  });
  assert.deepEqual(adapter.playbackControlHlsFatal(
    {type:"mediaError",details:"bufferStalledError"},true),{
    media_failure:true,
    observation:{decoder_state:"failed",error_code:"media",error_detail:"bufferStalledError"},
  });
  assert.deepEqual(adapter.playbackControlHlsFatal(
    {type:"mediaError",details:"videoDecodeError"},true),{
    media_failure:true,
    observation:{decoder_state:"failed",error_code:"decoder",error_detail:"videoDecodeError"},
  });
  video.ended=false; video.paused=false; video.currentTime=30; video.error={code:3};
  player.controlRenderOverride="failed";
  player.controlObservationOverride={
    decoder_state:"ready",error_code:"network",error_detail:"fragLoadError",
  };
  assert.deepEqual(adapter.playbackControlSnapshot(video,player).observation,{
    decoder_state:"ready",error_code:"network",error_detail:"fragLoadError",dropped_frames:2,
  },"specific HLS evidence wins over the media element's generic error code");
  video.error=null; player.controlRenderOverride=null; player.controlObservationOverride=null;

  const replayCalls=[];
  const firstReplayDone=deferred(), secondReplayDone=deferred();
  const replayAdapter=new Function("playImpl",[
    "let PLAYER={fileId:'file-1',title:'Film',knownDur:90000,durMs:90000,meta:{kind:'movie'}};",
    "let PENDING_ATTEMPT_REASON=null;",
    shippedSource("createPlaybackOpenGate"),
    "const PLAY_OPEN_GATE=createPlaybackOpenGate();",
    shippedSource("takePlaybackAttemptReason"),
    "function play(...args){ return playImpl(args,takePlaybackAttemptReason()); }",
    shippedSource("replayEnded"),
    "return {replayEnded,replacePlayer(value){PLAYER=value;},"+
      "invalidate(){PLAY_OPEN_GATE.invalidate();},supersede(){PLAY_OPEN_GATE.begin('open');},"+
      "state:()=>({player:PLAYER,reason:PENDING_ATTEMPT_REASON})};",
  ].join("\n"))((args,reason)=>{
    replayCalls.push({args,reason});
    return replayCalls.length===1?firstReplayDone.promise:secondReplayDone.promise;
  });
  assert.equal(replayAdapter.replayEnded(),true);
  replayAdapter.replacePlayer({fileId:"file-1",title:"Film replacement",knownDur:90000});
  assert.equal(replayAdapter.replayEnded(),false,"double-click cannot open two replay sessions");
  assert.deepEqual(replayCalls[0].args.slice(0,5),
    ["file-1","Film",0,90000,{kind:"movie"}]);
  assert.equal(replayCalls[0].args[5].kind,"replay");
  assert.equal(replayCalls[0].reason,"replay");
  assert.equal(replayAdapter.state().reason,null);
  replayAdapter.invalidate();
  assert.equal(replayAdapter.replayEnded(),true,
    "close invalidation allows replay even if the superseded replay promise never settles");
  assert.equal(replayCalls.length,2);
  secondReplayDone.resolve();
  await flush();
  assert.equal(replayAdapter.state().reason,null);

  const reasonAdapter=new Function([
    "let PENDING_ATTEMPT_REASON=null;",
    shippedSource("takePlaybackAttemptReason"),
    "return {async begin(value,wait){PENDING_ATTEMPT_REASON=value;"+
      "const captured=takePlaybackAttemptReason(); await wait; return captured;}};",
  ].join("\n"))();
  const qualityDone=deferred(), subtitleDone=deferred();
  const qualityPending=reasonAdapter.begin("quality",qualityDone.promise);
  const subtitlePending=reasonAdapter.begin("subtitle-off",subtitleDone.promise);
  subtitleDone.resolve();
  const subtitleReason=await subtitlePending;
  qualityDone.resolve();
  const qualityReason=await qualityPending;
  assert.equal(qualityReason,"quality");
  assert.equal(subtitleReason,"subtitle-off");

  const gateFactory=new Function([
    shippedSource("createPlaybackOpenGate"),"return createPlaybackOpenGate;",
  ].join("\n"))();
  const replayGate=gateFactory();
  const staleReplayAttempt=replayGate.begin("replay");
  assert.equal(replayGate.begin("replay"),null);
  replayGate.begin("open");
  const successorReplayAttempt=replayGate.begin("replay");
  assert.ok(successorReplayAttempt,
    "a newer full open supersedes a never-settling replay claim");
  replayGate.finish(staleReplayAttempt);
  assert.equal(replayGate.current(successorReplayAttempt),true,
    "a stale replay completion cannot clear or supersede its successor");
  const decisionGate=gateFactory(), decisionEvents=[];
  const oldDecisionDone=deferred(), newDecisionDone=deferred();
  const oldDecisionAttempt=decisionGate.begin("open");
  const oldDecisionFlow=(async()=>{
    await oldDecisionDone.promise;
    if(decisionGate.current(oldDecisionAttempt)) decisionEvents.push("old");
  })();
  const newDecisionAttempt=decisionGate.begin("open");
  const newDecisionFlow=(async()=>{
    await newDecisionDone.promise;
    if(decisionGate.current(newDecisionAttempt)) decisionEvents.push("new");
  })();
  newDecisionDone.resolve(); await newDecisionFlow;
  oldDecisionDone.resolve(); await oldDecisionFlow;
  assert.deepEqual(decisionEvents,["new"],"reversed decisions cannot revive a superseded open");
  const closedDecisionDone=deferred();
  const closedDecisionAttempt=decisionGate.begin("open");
  const closedDecisionFlow=(async()=>{
    await closedDecisionDone.promise;
    if(decisionGate.current(closedDecisionAttempt)) decisionEvents.push("closed");
  })();
  decisionGate.invalidate(); closedDecisionDone.resolve(); await closedDecisionFlow;
  assert.deepEqual(decisionEvents,["new"],"close fences a decision still in flight");

  const sessionGate=gateFactory(), attachedSessions=[], releasedSessions=[];
  const sessionFlow=async(attempt,done)=>{
    const sessionId=await done.promise;
    if(sessionGate.acceptResource(attempt,sessionId,id=>releasedSessions.push(id)))
      attachedSessions.push(sessionId);
  };
  const oldSessionDone=deferred(), newSessionDone=deferred();
  const oldSessionAttempt=sessionGate.begin("open");
  const oldSessionFlow=sessionFlow(oldSessionAttempt,oldSessionDone);
  const newSessionAttempt=sessionGate.begin("open");
  const newSessionFlow=sessionFlow(newSessionAttempt,newSessionDone);
  newSessionDone.resolve("session-new"); await newSessionFlow;
  oldSessionDone.resolve("session-old"); await oldSessionFlow;
  assert.deepEqual(attachedSessions,["session-new"]);
  assert.deepEqual(releasedSessions,["session-old"],
    "a session returned to a stale open is released rather than attached");
  const closedSessionDone=deferred();
  const closedSessionAttempt=sessionGate.begin("open");
  const closedSessionFlow=sessionFlow(closedSessionAttempt,closedSessionDone);
  sessionGate.invalidate(); closedSessionDone.resolve("session-closed"); await closedSessionFlow;
  assert.deepEqual(releasedSessions,["session-old","session-closed"],
    "close releases a session whose open completed late");

  let replayClicks=0,elementPlays=0,activities=0;
  const endedToggle=new Function("document","replayEnded","playerActivity",[
    shippedSource("togglePlay"),"togglePlay();",
  ].join("\n"))(
    {getElementById:()=>({ended:true,paused:true,play:()=>{elementPlays+=1;},pause:()=>{}})},
    ()=>{replayClicks+=1;},()=>{activities+=1;},
  );
  assert.equal(endedToggle,undefined);
  assert.equal(replayClicks,1);
  assert.equal(elementPlays,0,"ended media is never restarted behind its stopped controller");
  assert.equal(activities,1);

  assert.match(shippedSource("wirePlayer"),/visibilitychange[^\n]*notifyPlaybackControl/);
  assert.match(shippedSource("persistentWait"),/error_code:"decoder"/);
  assert.match(shippedSource("persistentWait"),/askPlaybackControl\("stalled",controlObservation\)/);
  assert.match(shippedSource("attachHls"),/notifyPlaybackControl\("failed",hlsFailure\.observation\)/);
  assert.match(shippedSource("stallDiagnose"),/notifyPlaybackControl\("stalled"\)/);
  assert.match(shippedSource("handleEnded"),/control_trigger:controlTrigger/);
  assert.match(shippedSource("startPlaybackControl"),/p\.controlReporter!==reporter/);
  assert.match(shippedSource("togglePlay"),/if\(v\.ended\) replayEnded\(\)/);
  const playSource=shippedSource("play");
  // The reason is one-shot: whatever consumes it first wins, so `play` has to
  // take it before it yields to anything. This used to name `await api(`, the
  // call that happened to be first at the time; the request moved behind
  // `askDecision` and the assertion started reading -1 < -1 and passing
  // vacuously — and then failing outright once the reason moved. Ask the real
  // question instead: nothing at all may be awaited before the reason is taken.
  const firstAwait=playSource.indexOf("await ");
  assert.notEqual(firstAwait,-1,"play no longer awaits anything");
  assert.ok(playSource.indexOf("takePlaybackAttemptReason()")<firstAwait,
    "play captures its one-shot reason before its first await");
  assert.match(playSource,/PLAY_OPEN_GATE\.current\(openAttempt\)/);
  assert.match(playSource,/PLAY_OPEN_GATE\.acceptResource\(openAttempt/);
  assert.match(shippedSource("startCopyHls"),/PLAY_OPEN_GATE\.acceptResource/);

  // The action vocabulary. Declaring `hold` is what permits the server to send
  // it at all: an undeclared action is never sent, so a client that did not ask
  // cannot be silenced by one.
  let declared = null;
  const declaring = new control.Reporter({
    bootstrap: bootstrap(),
    clientInstanceId: "66666666-6666-4666-8666-666666666666",
    snapshot: () => snapshot(),
    send: async (_url, request) => { declared = request.supported_actions; return response(request); },
  }).start();
  await flush();
  assert.deepEqual(
    declared,
    ["hold", "retry_resource", "terminal"],
    "the request declares the actions this client accepts",
  );
  declaring.stop();

  // A hold is not a failure and not a reason to stop. This is the whole point:
  // the server is saying production is deliberately not advancing, and a client
  // that treated that as a protocol error would lose reporting for the rest of
  // the film exactly when the server had just explained itself.
  let holdExchanges = 0;
  let holdErrors = 0;
  const holding = new control.Reporter({
    bootstrap: bootstrap(),
    clientInstanceId: "77777777-7777-4777-8777-777777777777",
    snapshot: () => snapshot(),
    send: async (_url, request) =>
      Object.assign(response(request), { action: { type: "hold", reason: "working_set" } }),
    onExchange: ({ error }) => { if (error) holdErrors += 1; else holdExchanges += 1; },
  }).start();
  await flush();
  assert.equal(holdErrors, 0, "a hold is an explanation, not a protocol error");
  assert.ok(holdExchanges >= 1);
  assert.equal(holding.status().stopped, false, "a held client keeps reporting");
  holding.stop();

  // A reason this client has never heard of is a newer server, not a broken
  // one. The reason is diagnostic; refusing the exchange over one unknown word
  // would silence the client for the rest of the film.
  let unknownReasonErrors = 0;
  const unknownReason = new control.Reporter({
    bootstrap: bootstrap(),
    clientInstanceId: "88888888-8888-4888-8888-888888888888",
    snapshot: () => snapshot(),
    send: async (_url, request) =>
      Object.assign(response(request), { action: { type: "hold", reason: "a_reason_from_next_year" } }),
    onExchange: ({ error }) => { if (error) unknownReasonErrors += 1; },
  }).start();
  await flush();
  assert.equal(unknownReasonErrors, 0, "an unrecognised hold reason is still a hold");
  unknownReason.stop();

  // A hold without a reason is malformed rather than merely unfamiliar.
  let malformedHold = 0;
  const malformed = new control.Reporter({
    bootstrap: bootstrap(),
    clientInstanceId: "99999999-9999-4999-8999-999999999999",
    snapshot: () => snapshot(),
    send: async (_url, request) =>
      Object.assign(response(request), { action: { type: "hold" } }),
    onExchange: ({ error }) => { if (error) malformedHold += 1; },
  }).start();
  await flush();
  assert.equal(malformedHold, 1, "a hold must carry its reason");
  assert.equal(malformed.status().stopped, true);
  malformed.stop();

  // A retry is the server naming its own cadence. It is not a failure, so the
  // exchange still counts as good; it only says when to ask again.
  const pacedTimers = [];
  let pacedNowMs = 0;
  const paced = new control.Reporter({
    bootstrap: bootstrap(),
    clientInstanceId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    snapshot: () => snapshot(),
    send: async (_url, request) =>
      Object.assign(response(request), {
        action: { type: "retry_resource", after_ms: 9_000, reason: "reader_failed" },
      }),
    setTimer: (fn, delay) => { pacedTimers.push({ fn, delay }); return pacedTimers.length; },
    clearTimer: () => {},
    now: () => pacedNowMs,
    onExchange: ({ error }) => { assert.equal(error, null, "a retry is not an error"); },
  }).start();
  await flush();
  assert.equal(paced.status().stopped, false, "a retry keeps the reporter alive");
  // The next exchange is held to the server's interval rather than this
  // client's default.
  assert.ok(
    pacedTimers.some((entry) => entry.delay >= 9_000),
    `expected a timer at the server's 9000ms, saw ${JSON.stringify(pacedTimers.map((e) => e.delay))}`,
  );
  paced.stop();

  // A terminal verdict ends reporting. It does not tear the player down —
  // this reporter owns no recovery, and the buffer already fetched is still
  // worth playing.
  let terminalErrors = 0;
  let terminalSeen = null;
  const ended = new control.Reporter({
    bootstrap: bootstrap(),
    clientInstanceId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    snapshot: () => snapshot(),
    send: async (_url, request) =>
      Object.assign(response(request), {
        action: { type: "terminal", code: "unsupported", message: "cannot be carried" },
      }),
    onExchange: ({ response: seen, error }) => {
      if (error) terminalErrors += 1;
      else terminalSeen = seen.action;
    },
  }).start();
  await flush();
  assert.equal(terminalErrors, 0, "a terminal verdict is an answer, not a protocol error");
  assert.equal(terminalSeen && terminalSeen.code, "unsupported");
  assert.equal(ended.status().stopped, true, "a terminal verdict ends reporting");
  ended.stop();

  // Malformed verdicts are still refused: an action inside the declared
  // vocabulary but missing the field the client acts on is worse than one it
  // has never heard of, because it would be acted on.
  for (const [label, action] of [
    ["terminal without a code", { type: "terminal", message: "x" }],
    ["retry without an interval", { type: "retry_resource", reason: "reader_failed" }],
    ["retry with a negative interval",
      { type: "retry_resource", reason: "reader_failed", after_ms: -1 }],
  ]) {
    let refused = 0;
    const bad = new control.Reporter({
      bootstrap: bootstrap(),
      clientInstanceId: "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
      snapshot: () => snapshot(),
      send: async (_url, request) => Object.assign(response(request), { action }),
      onExchange: ({ error }) => { if (error) refused += 1; },
    }).start();
    await flush();
    assert.equal(refused, 1, label);
    bad.stop();
  }

  // ---- the ask: persistentWait defers to the server's verdict --------------
  //
  // Driven through the SHIPPED persistentWait AND the shipped
  // startPlaybackControl, over a real Reporter. An earlier version of this
  // block called settlePlaybackControlWaiters directly; deleting the one line
  // in onExchange that connects the reporter to the waiters — which would make
  // every ask time out and add the whole bound to every persistent stall in
  // the browser — stayed green. Nothing here settles a waiter by hand.
  // Ruling D3: the ask bound is short because its fallback is the branch the
  // whole fleet takes, so it is added to every real stall. The Apple client
  // carries the same pair of numbers; if these two ever drift, one platform is
  // making a viewer wait longer than the other for the same reason.
  // Lifted from the shipped source rather than restated here. A threshold the
  // harness declares itself is a threshold the tests can agree with while the
  // player disagrees — and the supply/decode split these fixtures straddle is
  // decided by exactly one of them.
  const askConstants = ["CONTROL_ASK_MS", "CONTROL_ASK_CAP_MS", "CONTROL_MIN_EXCHANGE_MS",
    "CONTROL_DEFER_LIMIT", "SUPPLY_RUNWAY_SECS"].map((name) => {
      const found = SHIPPED_UI.match(new RegExp(`const ${name}=\\d+(?:\\.\\d+)?;`));
      assert.notEqual(found, null, `index.html no longer declares ${name}`);
      return found[0];
    }).join("\n");

  const harnessCleanup = [];
  function stallHarness(options = {}) {
    const timers = new Map();
    let nextTimer = 1;
    const log = [];
    const loading = [];
    const reopened = [];
    const stalls = [];
    const sent = [];
    let answer = options.answer || (() => ({ type: "none" }));
    let hold = null;
    let snapshotFn = null;
    const stub = new Function(
      "setTimeout", "clearTimeout", "PERSISTENT_STALL_MS",
      "PlaybackPolicy", "playQuality", "recordWaitStall", "pbPosSec", "clientLog",
      "setLoading", "startTranscodeFallback", "seekTo", "endWait", "playbackContext",
      "clockFromSec", "CONTROL_CLIENT_ID", "playbackControlSnapshot",
      "sendPlaybackControl", "window", "PlurxPlaybackControl", "performance",
      [
        "let PLAYER=null;",
        askConstants,
        shippedSource("holdReasonText"),
        shippedSource("controlVerdictText"),
        shippedSource("playbackControlObservationOverride"),
        shippedSource("notifyPlaybackControl"),
        shippedSource("askPlaybackControl"),
        shippedSource("settlePlaybackControlWaiters"),
        shippedSource("clearPlaybackControlWaiters"),
        shippedSource("stopPlaybackControl"),
        shippedSource("startPlaybackControl"),
        shippedSource("persistentWait"),
        "return {",
        " attach(player,video,bootstrap){PLAYER=player;",
        "   return startPlaybackControl(video,player,bootstrap);},",
        " detach(player){PLAYER=player; stopPlaybackControl(player); PLAYER=null;},",
        " stall(player,video,began,generation){PLAYER=player;",
        "   return persistentWait(video,player,began,generation);},",
        " verdictText:controlVerdictText,",
        " askProbe(player){PLAYER=player;",
        "  try{ const a=askPlaybackControl('stalled',{decoder_state:'starved'});",
        "   return {trigger:a.trigger, w:(player.controlWaiters||[]).length}; }",
        "  catch(e){ return {err:String(e&&e.message||e)}; }}, ",
        " probe(player,video){PLAYER=player; const r=player.controlReporter;",
        "  return {hasReporter:!!r, stopped:r&&r.stopped, seq:r&&r.sequence,",
        "   snap:!!(r&&r.snapshot()), notify:r&&r.notify()};}};",
      ].join("\n"),
    )(
      (fn, ms) => { const id = nextTimer++; timers.set(id, { fn, ms }); return id; },
      (id) => { timers.delete(id); },
      8_000,
      { stallRecoveryAction: () => "reconnect", stallRecoveryTargetHeight: () => 720 },
      () => "auto",
      (player, kind, ms, runway, detail) => stalls.push(detail),
      () => 12,
      (entry) => log.push(entry),
      (on, title, detail, buttons) => loading.push({ on, title, detail, buttons }),
      (why) => reopened.push({ kind: "transcode", why }),
      (position) => reopened.push({ kind: "seek", position }),
      () => {},
      () => ({}),
      (sec) => `0:${sec}`,
      "33333333-3333-4333-8333-333333333333",
      (...args) => (snapshotFn ? snapshotFn(...args) : snapshot(1_000, "stalled")),
      async (_url, request) => {
        sent.push(request);
        const held = hold && hold(request);
        if (held) return held;
        return Object.assign(response(request), { action: answer(request) });
      },
      { PlurxPlaybackControl: control },
      control,
      performance,
    );
    const made = {
      stub, timers, log, loading, reopened, stalls, sent,
      answerWith(next) { answer = next; },
      holdWith(next) { hold = next; },
      snapshotWith(next) { snapshotFn = next; },
      // Run every timer the shipped code armed, newest first, once.
      fire() {
        for (const [id, timer] of Array.from(timers)) { timers.delete(id); timer.fn(); }
      },
      attached: [],
    };
    harnessCleanup.push(made);
    return made;
  }

  function stalledPlayer() {
    return {
      started: true, waitAt: 100, waitRunway: 1, waitReported: false, waitTimer: null,
      method: "remux", stallRecoveries: 0, _seekToken: 3, stallPrompt: false,
    };
  }
  const stalledVideo = { paused: false, seeking: false };

  // Resolves to true only if the promise settles without any fake timer being
  // fired — i.e. something in the shipped code settled it, not its own bound.
  async function settledPromptly(promise) {
    const pending = Symbol("pending");
    for (let i = 0; i < 4; i += 1) await flush();
    await new Promise((done) => setTimeout(done, 400));
    await flush();
    return (await Promise.race([promise, Promise.resolve(pending)])) !== pending;
  }

  // The shipped startPlaybackControl builds the Reporter on real timers, and
  // the Reporter rate-limits consecutive exchanges by MIN_EXCHANGE_MS. Waiting
  // that out is the price of driving the real wiring instead of a stub.
  async function settleExchange() {
    await new Promise((done) => setTimeout(done, 320));
    await flush(); await flush();
  }

  async function askWith(action, options = {}) {
    // The bootstrap exchange answers `none`. Answering it with the verdict
    // under test would stop the reporter before the ask exists — which is
    // real behaviour, and has its own test below.
    const h = stallHarness({ answer: () => ({ type: "none" }) });
    const player = Object.assign(stalledPlayer(), options.player || {});
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await flush();
    h.answerWith(() => action);
    // start() already spent sequence 1; the ask must be answered by its own.
    const before = h.sent.length;
    const running = h.stub.stall(player, stalledVideo, 100, 3);
    await settleExchange();
    // Settled by its own exchange, not by its bound: the line in onExchange
    // that connects the reporter to the waiters is what makes that true, and
    // deleting it would otherwise only show up as six extra seconds of stall.
    assert.equal(await settledPromptly(running), true,
      "the ask is answered by its exchange rather than timing out");
    await running;
    assert.ok(h.sent.length > before, "the ask put a request on the wire");
    h.stub.detach(player);
    return { h, player };
  }

  // A none verdict leaves today's behaviour exactly as it was. This is the
  // branch every node in the fleet actually takes: vocabulary_total is zero.
  {
    const { h, player } = await askWith({ type: "none" });
    assert.equal(h.reopened.length, 1, "a none verdict falls through to the legacy reopen");
    assert.equal(h.reopened[0].kind, "seek");
    assert.equal(player.stallRecoveries, 1, "and still spends the legacy attempt");
  }

  // A terminal verdict replaces the client's invented words with the server's
  // and nothing else. Ruling D1: the player is not torn down, and the viewer
  // keeps every option they had — a terminal recipe is not a terminal file.
  {
    const { h, player } = await askWith({
      type: "terminal", code: "unsupported",
      message: "This file's audio is not playable here.",
    });
    assert.equal(h.reopened.length, 0, "a terminal verdict reopens nothing");
    assert.equal(player.stallRecoveries, 0, "a terminal verdict spends no legacy attempt");
    assert.equal(h.loading.length, 1);
    assert.equal(h.loading[0].title, "This file's audio is not playable here.",
      "the viewer reads the server's verdict, not the client's guess");
    assert.match(h.loading[0].buttons, /retryPlayback/, "Try again survives a terminal verdict");
    assert.match(h.loading[0].buttons, /startTranscodeFallback/,
      "Force transcode survives it — it is a different recipe");
  }

  // Server text is bounded and stripped on the way in, the way the outbound
  // error_detail beside it already is — and at the call site, not only in the
  // helper. A helper the overlay does not use bounds nothing.
  {
    const h = stallHarness();
    assert.equal(h.stub.verdictText("a\r\nb"), "a b");
    assert.equal(h.stub.verdictText("   "), "Playback stopped.");
    assert.equal(h.stub.verdictText("x".repeat(400)).length, 160);
  }
  {
    const { h } = await askWith({
      type: "terminal", code: "unsupported", message: "line\r\nbreak " + "x".repeat(400),
    });
    assert.equal(h.loading[0].title.includes("\n"), false,
      "the overlay title carries no server-supplied line breaks");
    assert.equal(h.loading[0].title.length, 160,
      "and is bounded where it is shown, not only where it is computed");
  }

  // A supply-starved stall recovers through a hold. The hold says why the
  // producer paused; it never says whether published bytes can be fetched, and
  // a player whose buffer is empty has nothing left to wait for. The default
  // fixture began its wait with 1s buffered, under the shipped threshold.
  {
    const { h, player } = await askWith({ type: "hold", reason: "no_room" });
    assert.equal(h.reopened.length, 1, "a supply-starved stall reopens through a hold");
    assert.equal(h.reopened[0].kind, "seek");
    assert.equal(player.stallRecoveries, 1, "and spends the legacy attempt, exactly once");
    assert.equal(player.waitTimer, null, "the fall-through arms no deferral timer");
    assert.equal(player.stallDeferrals, 0, "and counts no deferral");
    assert.equal(h.loading.filter((l) => /Waiting for the server/.test(l.title || "")).length, 0,
      "the viewer is told it is reconnecting, not that it is waiting on the server");
    const fell = h.log.find((entry) => entry.detail === "fallthrough:hold");
    assert.notEqual(fell, undefined, "the beacon says the hold was recovered through");
    assert.match(fell.message, /no_room/, "and names the reason the producer gave");
  }

  // A decode stall still defers to a hold: it holds media it cannot render
  // rather than media it cannot get, so a reopen would churn against a server
  // that already knows better. The deferral is of the reopen and NOT of the
  // viewer's information — a client that only waited would leave a viewer eight
  // seconds into a frozen picture with no UI, forever.
  {
    // This wait began with more buffered than the shipped supply threshold:
    // plenty left, and still not playing.
    const { h, player } = await askWith({ type: "hold", reason: "no_room" },
      { player: { waitRunway: 8 } });
    assert.equal(h.reopened.length, 0, "a hold reopens nothing");
    assert.equal(player.stallRecoveries, 0, "a hold spends no legacy attempt");
    assert.equal(h.loading.length, 1, "a hold tells the viewer what is happening");
    assert.equal(h.loading[0].detail, "The server is short of space.",
      "in the viewer's words, not the wire's");
    assert.equal(h.loading[0].buttons.includes("startTranscodeFallback"), false,
      "a hold offers no recipe change: the server said the recipe is not the problem");
    assert.equal(h.stalls.length, 1,
      "the stall is recorded once, not once per deferral");
    assert.equal(player.waitReported, true,
      "the wait stays reported, so resuming does not emit a second record for it");
    assert.equal(h.timers.get(player.waitTimer).ms, 8_000, "and the deadline comes round again");
  }

  // retry_resource paces to the server's interval, clamped, and bounded: a
  // server that keeps saying "soon" is not distinguishable from here from one
  // that is never going to be ready.
  for (const [afterMs, expected, label] of [
    [1_500, 1_500, "the server's interval is honoured"],
    [59_000, 8_000, "an interval past the deadline is clamped to it"],
    [1, 250, "an interval below the exchange floor is raised to it"],
  ]) {
    const { h, player } = await askWith({
      type: "retry_resource", reason: "reader_failed", after_ms: afterMs,
    });
    assert.equal(h.reopened.length, 0, label);
    assert.equal(h.timers.get(player.waitTimer).ms, expected, label);
    assert.equal(player.stallDeferrals, 1, label);
  }
  {
    const { h, player } = await askWith({
      type: "retry_resource", reason: "reader_failed", after_ms: 1_000,
    }, { player: { stallDeferrals: 3, stallDeferralsAt: 100 } });
    assert.equal(h.reopened.length, 1,
      "past the deferral bound the legacy path is taken anyway");
    assert.equal(player.stallRecoveries, 1);
  }

  // The floor is only meaningful inside one owner: resetForOwner zeroes the
  // sequence, so a later unrelated request can reach it carrying a verdict
  // decided for a different observation, generation and epoch.
  {
    const h = stallHarness({ answer: () => ({ type: "none" }) });
    const player = stalledPlayer();
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await flush();
    // A hold, so nothing is armed: the only way it can reach the viewer is
    // through this ask's own waiter.
    h.answerWith(() => ({ type: "hold", reason: "global" }));
    const running = h.stub.stall(player, stalledVideo, 100, 3);
    await flush();
    for (const waiter of player.controlWaiters || []) waiter.generation = "other";
    await settleExchange();
    // Nothing settled it, so only its own timer can — fire the fake timers.
    h.fire();
    await running;
    assert.equal(h.loading.filter((l) => /server/i.test(l.title || "")).length, 0,
      "a verdict from another generation never reaches the viewer");
    assert.equal(h.reopened.length, 1, "the ask times out into today's path");
    assert.equal(player.stallRecoveries, 1, "and spends the legacy attempt, as today");
    h.stub.detach(player);
  }

  // A terminal verdict can arrive on any exchange, and the reporter stops on
  // it. Ruling D1 says the verdict is armed, not executed — so the stall an
  // hour later must still read the server's words, even though there is no
  // longer a reporter to ask.
  {
    const h = stallHarness({
      answer: () => ({ type: "terminal", code: "unsupported", message: "No decoder for this." }),
    });
    const player = stalledPlayer();
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await settleExchange();
    assert.equal(player.controlReporter.stopped, true,
      "the reporter stops on a terminal verdict, as it always has");
    assert.deepEqual(player.controlVerdict,
      { type: "terminal", code: "unsupported", message: "No decoder for this." },
      "and the verdict outlives it");
    const running = h.stub.stall(player, stalledVideo, 100, 3);
    await flush(); await flush();
    await running;
    assert.equal(h.reopened.length, 0, "the armed verdict still suppresses the guess");
    assert.equal(h.loading[0].title, "No decoder for this.",
      "and the viewer reads it rather than the client's invention");
    h.stub.detach(player);
  }

  // The floor must be > the sequence already spent, not >=. An exchange that
  // was ALREADY IN FLIGHT when the stall happened carries an observation taken
  // before the stall existed; settling on it hands this stall someone else's
  // verdict. This is the case a counting correlator gets wrong, and it is only
  // visible while an exchange is outstanding.
  {
    const held = deferred();
    let first = true;
    const h = stallHarness({ answer: () => ({ type: "none" }) });
    const player = stalledPlayer();
    h.attached.push(player);
    h.answerWith(() => ({ type: "none" }));
    // Request 1 is the one already in flight when the stall happens; it
    // answers `hold`. Request 2 is this ask's own, and never answers — so the
    // only verdict available to settle the waiter is one it must refuse.
    h.holdWith((request) => {
      if (first) {
        first = false;
        return held.promise.then(() => Object.assign(response(request), {
          action: { type: "hold", reason: "ahead" },
        }));
      }
      return new Promise(() => {});
    });
    h.stub.attach(player, stalledVideo, bootstrap());
    await flush();
    const running = h.stub.stall(player, stalledVideo, 100, 3);
    await flush();
    held.resolve();
    assert.equal(await settledPromptly(running), false,
      "the in-flight exchange's verdict does not answer this ask");
    h.fire();
    await running;
    assert.equal(h.loading.filter((l) => /Waiting for the server/.test(l.title || "")).length, 0,
      "and never reaches the viewer");
    assert.equal(h.reopened.length, 1, "the ask times out into today's path");
    h.stub.detach(player);
  }

  // Every ask settles even when nothing answers it. Without its own bound a
  // stalled viewer would wait on a promise no exchange is going to resolve.
  {
    const h = stallHarness({ answer: () => ({ type: "none" }) });
    const player = stalledPlayer();
    h.attached.push(player);
    h.holdWith(() => new Promise(() => {}));
    h.stub.attach(player, stalledVideo, bootstrap());
    await flush();
    const running = h.stub.stall(player, stalledVideo, 100, 3);
    assert.equal(await settledPromptly(running), false, "nothing has answered it");
    assert.equal(h.timers.size >= 1, true, "so the ask armed its own bound");
    h.fire();
    assert.equal(await settledPromptly(running), true, "and that bound settles it");
    h.stub.detach(player);
  }

  // A notify that enqueued nothing — an invalid snapshot — will never produce
  // a request carrying this evidence, so waiting the bound out for it would
  // add the whole bound to a stall for an exchange that is not going to happen.
  {
    const h = stallHarness({ answer: () => ({ type: "none" }) });
    const player = stalledPlayer();
    h.attached.push(player);
    h.holdWith(() => new Promise(() => {}));
    h.stub.attach(player, stalledVideo, bootstrap());
    await flush();
    h.snapshotWith(() => null);
    const running = h.stub.stall(player, stalledVideo, 100, 3);
    assert.equal(await settledPromptly(running), true,
      "an ask that enqueued nothing answers at once rather than after the bound");
    assert.equal((player.controlWaiters || []).length, 0, "and registers no waiter");
    h.stub.detach(player);
  }

  // A reporter that goes away mid-ask settles its waiters at once. Leaving
  // them to time out would add the whole bound to a stall whose exchange can
  // no longer happen.
  {
    const h = stallHarness({ answer: () => ({ type: "none" }) });
    const player = stalledPlayer();
    h.attached.push(player);
    h.holdWith(() => new Promise(() => {}));
    h.stub.attach(player, stalledVideo, bootstrap());
    await flush();
    const running = h.stub.stall(player, stalledVideo, 100, 3);
    await flush();
    h.stub.detach(player);
    assert.equal(await settledPromptly(running), true,
      "closing the reporter settles the ask rather than stranding it");
    assert.equal((player.controlWaiters || []).length, 0, "no waiter is left behind");
    assert.equal(h.timers.size, 0,
      "and its timer is cleared rather than left to fire into a closed player");
  }

  // And an ask made after the reporter has already stopped never registers a
  // waiter at all.
  {
    const h = stallHarness({
      answer: () => ({ type: "terminal", code: "unsupported", message: "done" }),
    });
    const player = stalledPlayer();
    h.attached.push(player);
    h.stub.attach(player, stalledVideo, bootstrap());
    await settleExchange();
    assert.equal(player.controlReporter.stopped, true);
    player.controlVerdict = null;
    const running = h.stub.stall(player, stalledVideo, 100, 3);
    assert.equal(await settledPromptly(running), true,
      "a stopped reporter answers immediately rather than after the bound");
    assert.equal((player.controlWaiters || []).length, 0);
    h.stub.detach(player);
  }

  // The viewer had the whole ask window to leave. Every condition the entry
  // guard checked is re-checked, because none of them survived the await.
  for (const [label, leave] of [
    ["the viewer seeked", (player) => { player._seekToken = 4; }],
    ["the viewer paused", () => { stalledVideo.paused = true; }],
    ["the wait already ended", (player) => { player.waitAt = null; }],
  ]) {
    const h = stallHarness();
    const player = stalledPlayer();
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await flush();
    h.answerWith(() => { leave(player); return { type: "none" }; });
    const running = h.stub.stall(player, stalledVideo, 100, 3);
    await settleExchange();
    await running;
    stalledVideo.paused = false;
    assert.equal(h.reopened.length, 0, `${label}: nothing acts on the stale generation`);
    assert.equal(h.loading.length, 0, label);
    h.stub.detach(player);
  }

  // Every harness reporter runs on real timers; a live one keeps the process
  // alive after the last assertion and reads in CI as a hung suite.
  for (const harness of harnessCleanup) {
    for (const player of harness.attached) harness.stub.detach(player);
  }

  // ---- the ask: handleEnded defers its truncated-stream decision -----------
  //
  // `endedTries > 3` is a guess standing in for the answer the server has: it
  // knows whether the producer was told to stop, ran out of something, or gave
  // a verdict about the source. When the server answers, the guess must stop
  // bounding anything; when it does not, the guess must still be there.
  function endedHarness(options = {}) {
    const timers = new Map();
    let nextTimer = 1;
    const log = [];
    const loading = [];
    const seeks = [];
    let answer = options.answer || (() => ({ type: "none" }));
    let hold = null;
    const video = { seeking: false };
    const stub = new Function(
      "setTimeout", "clearTimeout", "PERSISTENT_STALL_MS", "ENDED_SLACK_SEC",
      "pbTotalSec", "pbPosSec", "reportProgress", "finishPlayback",
      "playNextAudiobookPart", "clientLog", "setLoading", "seekTo", "clockFromSec",
      "CONTROL_CLIENT_ID", "playbackControlSnapshot", "sendPlaybackControl",
      "window", "PlurxPlaybackControl", "performance", "document",
      [
        "let PLAYER=null;",
        askConstants,
        shippedSource("controlVerdictText"),
        shippedSource("holdReasonText"),
        shippedSource("playbackControlObservationOverride"),
        shippedSource("notifyPlaybackControl"),
        shippedSource("askPlaybackControl"),
        shippedSource("settlePlaybackControlWaiters"),
        shippedSource("clearPlaybackControlWaiters"),
        shippedSource("stopPlaybackControl"),
        shippedSource("startPlaybackControl"),
        shippedSource("endedStillOurs"),
        shippedSource("handleEnded"),
        "return {",
        " attach(player,video,bootstrap){PLAYER=player;",
        "   return startPlaybackControl(video,player,bootstrap);},",
        " detach(player){PLAYER=player; stopPlaybackControl(player); PLAYER=null;},",
        " ended(player,fileId){PLAYER=player; return handleEnded(fileId);}};",
      ].join("\n"),
    )(
      (fn, ms) => { const id = nextTimer++; timers.set(id, { fn, ms }); return id; },
      (id) => { timers.delete(id); },
      8_000,
      15,
      () => 3_600,
      () => 1_200,
      async () => {},
      () => {},
      async () => false,
      (entry) => log.push(entry),
      (on, title, detail, buttons) => loading.push({ on, title, detail, buttons }),
      (sec) => seeks.push(sec),
      (sec) => `0:${Math.round(sec)}`,
      "44444444-4444-4444-8444-444444444444",
      () => snapshot(1_000, "failed"),
      async (_url, request) => {
        const held = hold && hold(request);
        if (held) return held;
        return Object.assign(response(request), { action: answer(request) });
      },
      { PlurxPlaybackControl: control },
      control,
      performance,
      { getElementById: () => video },
    );
    const made = {
      stub, timers, log, loading, seeks, video, attached: [],
      answerWith(next) { answer = next; },
      holdWith(next) { hold = next; },
      fire() {
        for (const [id, timer] of Array.from(timers)) { timers.delete(id); timer.fn(); }
      },
    };
    harnessCleanup.push(made);
    return made;
  }

  // A repeat only counts when the stream dies at the SAME position, so a
  // player with prior attempts has to carry the position it died at.
  function endedPlayer(tries = 0) {
    return {
      fileId: 7, method: "remux", endedAt: tries > 0 ? 1_200 : null,
      endedTries: tries, bookParts: null, durMs: 3_600_000, _seekToken: 5,
    };
  }

  async function endedWith(action, tries = 0) {
    const h = endedHarness();
    const player = endedPlayer(tries);
    h.stub.attach(player, {}, bootstrap());
    h.attached.push(player);
    await flush();
    h.answerWith(() => action);
    const running = h.stub.ended(player, 7);
    await settleExchange();
    // A retry_resource verdict paces with a timer; nothing else arms one.
    const paced = Array.from(h.timers.values()).map((timer) => timer.ms);
    h.fire();
    await flush();
    await running;
    h.stub.detach(player);
    return { h, player, paced };
  }

  // With no answer, today's behaviour is untouched — including the guess.
  {
    const { h, player } = await endedWith({ type: "none" });
    assert.equal(h.seeks.length, 1, "a none verdict resumes, as today");
    assert.equal(player.endedTries, 1, "and counts the attempt");
  }
  {
    const { h } = await endedWith({ type: "none" }, 3);
    assert.equal(h.seeks.length, 0, "past the local budget it gives up, as today");
    assert.equal(h.loading[0].title, "The stream stopped before the end.");
  }

  // When the server answers, the guess stops bounding anything. This is the
  // whole milestone: the budget existed because nothing better was available.
  // A hold reconnects NOTHING. This is the case that is easy to get backwards:
  // an ended media element looks like it can only go forward by reconnecting,
  // but a VOD session seeks in place, so resuming at the truncation point
  // re-fires `ended` immediately and the loop contains one round trip. Left
  // that way a held stream reconnects forever behind a buttonless overlay.
  {
    const { h, player, paced } = await endedWith({ type: "hold", reason: "no_room" }, 1);
    assert.equal(h.seeks.length, 0, "a hold reconnects nothing");
    assert.deepEqual(paced, [], "and arms no retry");
    assert.equal(h.loading.length, 1, "the viewer is told what is happening");
    assert.equal(h.loading[0].detail, "The server is short of space.");
    assert.match(h.loading[0].buttons, /retryPlayback/, "and can act on it");
    assert.match(h.loading[0].buttons, /closePlayer/);
    assert.match(h.log[0].detail, /deferred:hold/);
    assert.equal(player.endedTries, 2, "the count is kept as evidence");
  }

  // retry_resource resumes at the server's interval, clamped — and bounded.
  // A server that keeps saying "soon" is not distinguishable from here from
  // one that is never going to be ready.
  for (const [afterMs, expected] of [[400, 400], [1, 250], [59_000, 8_000]]) {
    const action = { type: "retry_resource", reason: "reader_failed", after_ms: afterMs };
    const { h, player, paced } = await endedWith(action, 1);
    assert.equal(h.seeks.length, 1, "a paced verdict resumes");
    assert.equal(h.seeks[0], 1_200);
    assert.match(h.log[0].detail, /deferred:retry_resource/);
    assert.equal(player.endedTries, 2);
    assert.deepEqual(paced, [expected],
      "and waits exactly as long as it is entitled to");
  }
  {
    // Past the deferral bound the local path is taken anyway.
    const { h } = await endedWith(
      { type: "retry_resource", reason: "reader_failed", after_ms: 400 }, 9,
    );
    assert.equal(h.seeks.length, 0, "an unbounded pacing verdict does not defer forever");
    assert.equal(h.loading[0].title, "The stream stopped before the end.");
  }

  // And the pacing window is a second chance to leave. A viewer who scrubs
  // away while a retry_resource interval is running must not be dragged back
  // to the truncation when it elapses.
  {
    // The bootstrap exchange answers `none`. Answering it with the verdict
    // under test would pace the reporter by `after_ms` before the ask's own
    // request could even start, and the ask would still be outstanding here.
    const h = endedHarness();
    const player = endedPlayer(1);
    h.stub.attach(player, {}, bootstrap());
    h.attached.push(player);
    await flush();
    h.answerWith(() => ({ type: "retry_resource", reason: "reader_failed", after_ms: 400 }));
    const running = h.stub.ended(player, 7);
    await settleExchange();
    // The armed interval is the proof the first guard has already passed, so
    // what follows can only be caught by the second one.
    // The interval's own duration is the proof: the ask's timer is
    // CONTROL_ASK_MS, so seeing 400 means the ask settled and the first guard
    // has already passed. Without this the test would be measuring the ask's
    // timeout and proving nothing about the second guard.
    // The interval's own duration is the proof: the ask's timer is
    // CONTROL_ASK_MS, so seeing 400 means the ask settled and the first guard
    // has already passed. Without this the test would be measuring the ask's
    // timeout and proving nothing about the second guard.
    assert.deepEqual(Array.from(h.timers.values()).map((t) => t.ms), [400],
      "the paced interval is armed, so the first guard has passed");
    assert.notEqual(400, Number(askConstants.match(/CONTROL_ASK_MS=(\d+)/)[1]),
      "and 400 is not the ask's own bound, or that proof is circular");
    player._seekToken = 6;
    h.fire();
    await running;
    assert.equal(h.seeks.length, 0,
      "leaving during the paced interval is as good as leaving during the ask");
    h.stub.detach(player);
  }

  // Direct play has no lower rung to reconnect to, so it gives up at once.
  {
    const h = endedHarness();
    const player = Object.assign(endedPlayer(), { method: "direct_play" });
    h.stub.attach(player, {}, bootstrap());
    h.attached.push(player);
    await flush();
    const running = h.stub.ended(player, 7);
    await settleExchange();
    h.fire();
    await running;
    assert.equal(h.seeks.length, 0, "direct play reconnects nothing");
    assert.equal(h.loading[0].title, "The stream stopped before the end.");
    h.stub.detach(player);
  }

  // A terminal that arrived on an ordinary exchange stopped the reporter, so
  // there is nothing left to ask when the stream ends. The verdict is armed,
  // and it still replaces the client's inference.
  {
    const h = endedHarness({
      answer: () => ({ type: "terminal", code: "unsupported", message: "Armed earlier." }),
    });
    const player = endedPlayer();
    h.stub.attach(player, {}, bootstrap());
    h.attached.push(player);
    await settleExchange();
    assert.equal(player.controlReporter.stopped, true);
    const running = h.stub.ended(player, 7);
    await flush(); await flush();
    await running;
    assert.equal(h.seeks.length, 0, "the armed verdict still suppresses the guess");
    assert.equal(h.loading[0].title, "Armed earlier.");
    h.stub.detach(player);
  }

  // The viewer had the ask window to leave, and the next `ended` event on a
  // stream that reopened while we waited re-enters this function.
  for (const [label, leave] of [
    ["the viewer opened another file", (player) => { player.fileId = 8; }],
    ["the viewer scrubbed away", (player) => { player._seekToken = 6; }],
    ["a seek is in flight", (player, h) => { h.video.seeking = true; }],
    ["the stream already reopened and ended again", (player) => { player.endedAt = 90; }],
  ]) {
    const h = endedHarness();
    const player = endedPlayer();
    h.stub.attach(player, {}, bootstrap());
    h.attached.push(player);
    await flush();
    h.answerWith(() => { leave(player, h); return { type: "none" }; });
    const running = h.stub.ended(player, 7);
    await settleExchange();
    h.fire();
    await running;
    assert.equal(h.seeks.length, 0, `${label}: nothing reconnects`);
    assert.equal(h.loading.length, 0, label);
    h.stub.detach(player);
  }

  // A terminal verdict replaces the client's inference from a runtime
  // mismatch with the server's reason, and reconnects nothing.
  {
    const { h } = await endedWith({
      type: "terminal", code: "unsupported", message: "The source stops here.",
    }, 0);
    assert.equal(h.seeks.length, 0, "a terminal verdict reconnects nothing");
    assert.equal(h.loading[0].title, "The source stops here.");
    assert.match(h.loading[0].buttons, /retryPlayback/,
      "and the viewer keeps every option they had");
  }

  reporter.stop();

  // The default timer, which every browser takes and no test took.
  //
  // `setTimeout` and `clearTimeout` are WindowTimers methods and every browser
  // brand-checks their receiver. Stored on the reporter and invoked as
  // `this.setTimer(...)` they throw `TypeError: Illegal invocation`, which is
  // what the M5 fleet run hit: the web reporter completed zero exchanges while
  // this whole file passed, because every case above injects its own timer.
  //
  // Node does not brand-check, so the check is installed here. The doubles are
  // exactly as strict as a browser and no stricter: they accept the global
  // receiver and refuse every other.
  {
    const realSetTimeout = globalThis.setTimeout;
    const realClearTimeout = globalThis.clearTimeout;
    let refusals = 0;
    globalThis.setTimeout = function (run, ms) {
      if (this !== globalThis) {
        refusals += 1;
        throw new TypeError("Illegal invocation");
      }
      return realSetTimeout(run, ms);
    };
    globalThis.clearTimeout = function (handle) {
      if (this !== globalThis) {
        refusals += 1;
        throw new TypeError("Illegal invocation");
      }
      return realClearTimeout(handle);
    };
    try {
      const strict = new control.Reporter({
        bootstrap: bootstrap(),
        clientInstanceId: "22222222-2222-4222-8222-222222222222",
        snapshot: () => snapshot(),
        send: async () => ({ accepted: true, action: { type: "none" } }),
      });
      // `notify` schedules through the default timer, and `stop` cancels
      // through it. Either one calling a bare WindowTimers function as a
      // method is the whole defect.
      strict.notify();
      strict.stop();
      assert.equal(
        refusals,
        0,
        "the reporter must not invoke setTimeout or clearTimeout as its own " +
          "method — a browser refuses that receiver and the exchange never runs",
      );
    } finally {
      globalThis.setTimeout = realSetTimeout;
      globalThis.clearTimeout = realClearTimeout;
    }
  }

  process.stdout.write("PASS passive web playback-control reporter\n");
}

main().catch((error) => {
  process.stderr.write(`${error.stack || error}\n`);
  process.exitCode = 1;
});
