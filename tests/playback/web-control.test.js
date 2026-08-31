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
  assert.match(shippedSource("persistentWait"),/notifyPlaybackControl\("stalled",controlObservation\)/);
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

  reporter.stop();
  process.stdout.write("PASS passive web playback-control reporter\n");
}

main().catch((error) => {
  process.stderr.write(`${error.stack || error}\n`);
  process.exitCode = 1;
});
