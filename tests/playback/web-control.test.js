"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const control = require("../../crates/plurxd/src/web/playback-control.js");
const TEST_CAPTURE_OWNER=Object.freeze({lifecycleId:"test-player",attachmentGeneration:1});
const captureSnapshot=(value,intentGeneration=0,owner=TEST_CAPTURE_OWNER)=>
  control.capture(value,intentGeneration,owner);

const SHIPPED_UI = fs.readFileSync(
  path.join(__dirname, "../../crates/plurxd/src/web/index.html"),
  "utf8",
);
const DECLARATIONS = ["\nfunction ", "\nasync function "];
const TERMINATORS = DECLARATIONS.concat(["\nconst ", "\nlet ", "\nwindow.", "\ndocument.", "\nsetInterval("]);
function shippedSource(name) {
  const start = DECLARATIONS.map((kind) => SHIPPED_UI.indexOf(`${kind}${name}(`))
    .find((at) => at !== -1);
  assert.notEqual(start, undefined, `index.html no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = TERMINATORS.map((kind) => rest.indexOf(kind, 1)).filter((at) => at !== -1);
  return (ends.length ? rest.slice(0, Math.min(...ends)) : rest).trimEnd();
}
// A shipped top-level constant, so a scope built out of source cannot drift
// from the value the page actually uses.
function shippedConst(name) {
  const match = SHIPPED_UI.match(new RegExp(`\\nconst ${name}=([\\s\\S]*?);\\n`));
  assert.ok(match, `index.html no longer declares const ${name}`);
  return match[0];
}


// The playback surface presenter is the subject of `web-policy.test.js`, which
// runs the shipped reducer and the shipped render against every fixture case.
// In THIS file it is a seam: these harnesses slice owner code that raises typed
// faults now, so the seam records what was raised and what the owner stopped,
// and the overlay-copy assertions below read the fault instead of a
// `setLoading` call that no longer exists. `playbackStallActions` is the
// shipped one, because the buttons a stalled recipe offers are exactly what
// several of those assertions are about.
function surfaceSeam() {
  return [
    "const raised=[],surfaceEvents=[],surfaceStops=[];",
    "const PLAYBACK_SURFACE={state:{faults:[],presenting:false},surface:null,history:[]};",
    "function playbackSurfaceStep(event){surfaceEvents.push(event);return null;}",
    "function raisePlaybackSurface(source,fault,options){raised.push({source,fault:fault||{},options:options||null});return null;}",
    "function playbackSurfaceGeneration(p){return (p&&p.attemptId)||null;}",
    "function playbackSurfaceIntent(p){return p&&p.controlSeek?p.controlSeek.sequence:null;}",
    "function playbackSurfaceContext(p){return p&&p.pendingMediaChange?'change':(p&&p.started?'attached':'start');}",
    "function playbackSurfacePromptUp(){return false;}",
    "function playbackSurfaceClassOf(){return null;}",
    "function playbackSurfaceClassBlocks(){return false;}",
    "function playbackSurfaceSourceIsBlocking(){return false;}",
    "function stopPlayerForExhaustion(){surfaceStops.push(true);}",
    "function resetPlaybackSurface(){}",
    "function armPlaybackSampling(){}",
    shippedSource("playbackStallActions"),
    // The old `{on, title, detail, buttons}` shape the copy assertions read,
    // built from the fault the owner raised.
    "function surfacePainted(){return raised.map(entry=>({on:true,source:entry.source,"+
      "title:entry.fault.title,detail:entry.fault.detail,"+
      "actions:entry.fault.actions||[],player_stopped:!!entry.fault.player_stopped}));}",
  ].join("\n");
}

// Run the real full-open and menu code with a controllable decision boundary.
// Media/server facilities are replaced; intent capture and replacement are not.
function fullOpenHarness() {
  const policy = require("../../crates/plurxd/src/web/playback-policy.js");
  return new Function("PlaybackPolicy", [
    "let PLAYER=null,PENDING_ATTEMPT_REASON=null,PENDING_DEFAULT_SUB_OFF=false,PENDING_LIBRARY_CHANNEL_PLAYBACK=null,STATS_TIMER=null; const decisions=[],sessions=[],released=[],media=[];",
    "let quality='auto';const localStorage={getItem:()=>quality,setItem:(key,value)=>{quality=value;}}; const DECODE_LIMIT_TTL_MS=1,DECODE_LIMIT_RETEST_MS=1;",
    "let modalOpen=true; const node={classList:{contains:()=>modalOpen,add(){modalOpen=true;},toggle(){},remove(...names){if(names.includes('open'))modalOpen=false;}},style:{},dataset:{},focus(){},setAttribute(){}};",
    "const video={paused:false,ended:false,seeking:false,currentTime:10,playbackRate:1,textTracks:[{mode:'disabled'}],querySelectorAll:()=>[],addEventListener(){},removeEventListener(){},removeAttribute(){},load(){},play(){this.paused=false;media.push('play');return Promise.resolve();},pause(){this.paused=true;media.push('pause');}};",
    "const document={activeElement:node,getElementById:id=>id==='video'?video:node};",
    "let now=0;const performance={now:()=>now},timers=new Map();let timerId=0;const window={}; function setTimeout(fn,ms){if(ms===100){Promise.resolve().then(fn);return 0;}const id=++timerId;timers.set(id,{fn,at:now+ms});return id;}function clearTimeout(id){timers.delete(id);} function setInterval(){return 0;} function clearInterval(){}",
    "function qualityForce(){return PlaybackPolicy.qualityForce(quality);} function playQuality(){return quality;} function qualityLabel(){return '720p';} function prePlaySelection(){return null;}",
    surfaceSeam(),
    "const loading=[],posted=[],ITEM_FOR_FILE={film:'film-item','new-title':'new-item'};function api(path,{body}={}){posted.push({path,body});return Promise.resolve({});}function wirePlayer(){} function setLoading(...args){loading.push(args);} function toast(){} function closeMenu(){} const location={hash:'#/'};function exitPresentationModes(){}function cancelPendingSeek(){}",
    "function clientLog(){} function playbackContext(){return {};} function decodeLimits(){return {};} function playerPixelHeight(){return 1080;}",
    // Capability probing is a seam here; play retains this snapshot for routing.
    "function currentCapsDocument(){return {video:[],audio:[]};}",
    "function askDecision(file,force,selection,signal){return new Promise(resolve=>decisions.push({file,selection:selection&&{...selection},signal,resolve}));}",
    "function openSession(file,opts,signal,requestId){return new Promise((resolve,reject)=>sessions.push({file,opts,signal,requestId,reject,resolve(info={}){resolve({...info,session_id:info.session_id||'session-'+sessions.length,opts});}}));}",
    "function attachSession(v,p,info,pos){p.sessionId=info.session_id;p.offset=0;p.vod=true;media.push({attached:info.opts});markPlaybackControlSeekExecuted(p,pos);return pos;}",
    "function stopPlayerTimers(){} function releaseSession(id){released.push(id);} function teardownHls(){} function clearSubs(){}",
    // M5's create-retry owner is shipped code, so the harness runs it rather
    // than a copy: `requestIds` is what pins that a sequence replays ONE
    // identity instead of minting a create per attempt.
    "const requestIds=[];function newRequestId(){const id='rq-'+requestIds.length;requestIds.push(id);return id;}",
    shippedSource("playbackRetryDelay"),shippedSource("playbackCreateRetryContext"),
    shippedSource("openSessionRetryingNotYet"),
    "function prePlayApplication(d,s){return {subtitle:s?.subtitle??null,burnedSub:null,textSub:null};} function autoskipOn(){return false;}",
    "function newAttempt(){} function selectedAudioIndex(p){return p.audio[p.curAudio]?.index||0;} function setupAirplay(){} function setupTrackMenus(){}",
    "function renderPlayerInfo(){} function autoNextOn(){return false;} function useNativeHls(){return false;} function segmentedRemuxOk(){return false;}",
    "function tok(url){return url;} function armStall(){} function remuxUrl(url,audio,pos){return '/remux?audio='+audio+'&start='+pos;}",
    shippedSource("markPlaybackControlSeekExecuted"),shippedSource("samplePlaybackPresentationClock"),shippedSource("settlePlaybackControlSeek"),
    "function probeDecode(){} function armHitchDetector(){} function pbSyncPlayIcon(){} function playerActivity(){} function clockFromSec(t){return String(t);}",
    shippedSource("pbTick"),shippedSource("pbPosSec"),shippedSource("pbShownSec"),
    "function clearPlaybackControlWaiters(){} function notifyPlaybackControl(){} function endWait(){} function pbTotalSec(){return 600;}",
    "function subNeedsBurn(s){return !!s?.burn;} function pbSyncSubIcon(){} function subLabelFor(){return 'subtitle';} function langName(){return 'audio';} function nativeHlsSubtitleOrdinal(){return 0;}",
    "function showSessionOpenFailure(){return false;} function showStallRecoveryFailure(){} function recordAutoSwitch(p,from,to,reason,pos,id){p.abr.switches.push({from,to,reason,pos,id});}",
    "function autoLastGoodHeight(){return null;} function transcodeReason(){return '';} function finishStallRecovery(){return false;}",
    "const PLAY_CAPS={acodec:'aac'};function hlsJsSupported(){return true;}function mseCanTake(){return true;}function clearStreamFailure(){}",
    "const hlsInstances=[];class Hls{static Events={MANIFEST_PARSED:'manifest',ERROR:'error',LEVEL_LOADED:'level',BUFFER_FLUSHING:'flush',FRAG_CHANGED:'frag',FRAG_LOADED:'loaded',BUFFER_APPENDED:'append'};static ErrorDetails={BUFFER_FULL_ERROR:'quota',BUFFER_APPEND_ERROR:'append-error'};static isSupported(){return true;}constructor(config){this.config=config;this.events={};hlsInstances.push(this);}on(event,fn){this.events[event]=fn;}loadSource(){}startLoad(){}stopLoad(){}attachMedia(){}destroy(){}}window.Hls=Hls;const TOKEN=null;function preferNativeHls(){return false;}function bufferTargets(){return {fwd:20,back:10};}function vodClientContract(){return {};}",
    shippedSource("createPlaybackOpenGate"), "const PLAY_OPEN_GATE=createPlaybackOpenGate();",
    shippedSource("beginPlaybackPreparation"),
    shippedSource("takePlaybackAttemptReason"), shippedSource("playbackSelection"),
    shippedSource("positionForPlaybackIntent"), shippedSource("supersedePlaybackControlIntent"),
    shippedSource("beginPlaybackControlSeek"), shippedSource("rememberPlaybackSelection"),
    shippedSource("noSegments"), shippedSource("copyHlsMseOk"),
    shippedSource("playbackInitialRoute"), shippedSource("restartPendingPlaybackOpen"),
    shippedSource("requestPlaybackMediaChange"),shippedSource("executePlaybackMediaChange"),
    shippedSource("streamGeneration"),shippedSource("transcodeOpts"),shippedSource("transcodeHeight"),shippedSource("sessionHeight"),
    shippedSource("startCopyHls"),
    shippedSource("startTranscodeFallback"),shippedSource("switchAutoRung"),
    shippedSource("autoSwitchLabel"),
    shippedSource("claimAutoFallback"),shippedSource("releaseAutoFallback"),
    shippedSource("hasPendingPlaybackOpen"),shippedSource("playbackOwnsAttachedMedia"),
    shippedSource("rememberPlaybackTransportIntent"), shippedSource("applyPlaybackTransportIntent"),
    shippedSource("pausePlaybackInternally"),
    shippedSource("resetPlaybackTransportEvents"),shippedSource("playbackTransportEvents"),
    shippedSource("setPlaybackMediaSource"),
    shippedSource("retirePlaybackPredecessor"),
    shippedSource("handlePlaybackTransportEvent"),
    shippedSource("beginPlaybackMediaAttachment"), shippedSource("applyPlaybackAttachmentPosition"),
    shippedSource("playbackAttemptTerminallyStopped"),shippedSource("hlsStartupCurrent"),shippedSource("hlsStartupIncomplete"),
    shippedSource("hlsStartupManifestRequest"),shippedSource("abortHlsStartupLoaders"),
    shippedSource("cancelHlsStartup"),shippedSource("completeHlsStartup"),
    shippedSource("configureHlsStartupDeadline"),shippedSource("diagnoseHlsStartup"),
    shippedSource("exhaustHlsStartup"),shippedSource("armHlsStartupRetry"),
    shippedSource("pauseHlsStartup"),shippedSource("resumeHlsStartup"),
    shippedSource("boundStreamFailureBody"),shippedSource("decodeStreamFailureBytes"),
    shippedSource("streamFailureResponseBodyNow"),shippedSource("streamFailureResponseBody"),
    shippedSource("observeStreamFailureResponse"),
    shippedSource("createHlsStartupLoader"),shippedSource("scheduleHlsNetworkRetry"),
    shippedSource("attachHls"),
    shippedSource("resetMediaSource"), shippedSource("play"), shippedSource("setQuality"),
    shippedSource("seekTo"), shippedSource("switchAudio"), shippedSource("setSub"),shippedSource("burnSub"),
    shippedSource("offsetLabel"), shippedSource("setSync"), shippedSource("togglePlay"),
    shippedSource("retryPlayback"),
    shippedSource("closePlayer"),
    shippedSource("reportProgress"),
    "const ttff=[];function reportTtff(){ttff.push(PLAYER.fileId);}function esc(x){return x;}function finishPlayback(){throw Error('unattached autoplay');}function clearStall(){}function bufferRunway(){return 0;}const PERSISTENT_STALL_MS=8000;",
    shippedSource("playbackMarkersUsable"),shippedSource("markerNowMs"),
    shippedSource("markerIsEstimated"),shippedSource("markerAutoSkipEligible"),
    shippedSource("renderSkip"),shippedSource("checkMarkers"),shippedSource("skipMarker"),shippedSource("skipCurrent"),
    shippedSource("playbackWaitNeedsProgress"),shippedSource("handlePlaybackPlaying"),shippedSource("beginWait"),
    "function incumbentWaiting(){let handler;const v=Object.create(video);v.addEventListener=(_,fn)=>{handler=fn;};"+
      shippedSource('wirePlayerMedia').match(/v\.addEventListener\("waiting",\(\)=>\{[\s\S]*?\n  \}\);/)[0]+"handler();}",
    "function incumbentError(){let handler;const v=Object.create(video);v.addEventListener=(_,fn)=>{handler=fn;};"+
      shippedSource('wirePlayerMedia').match(/v\.addEventListener\("error",\(\)=>\{[\s\S]*?\n  \}\);/)[0]+"handler();}",
    "return {attach(p){PLAYER=p;},setOpen(value){modalOpen=value;},isOpen:()=>modalOpen,node,current:()=>PLAYER,decisions,sessions,released,media,loading,requestIds,surface:surfacePainted,stops:()=>surfaceStops.length,posted,video,play,setQuality,seekTo,switchAudio,setSub,setSync,togglePlay,retryPlayback,closePlayer,incumbentError,incumbentWaiting,checkMarkers,skipCurrent,skipMarker,ttff,pbTick,reportProgress,attachHls:()=>attachHls(video,'/A/index.m3u8',10),spendHlsRetry:()=>scheduleHlsNetworkRetry(video,PLAYER,'network'),hlsInstances,settle:()=>settlePlaybackControlSeek(video,PLAYER,video.currentTime,100),playing:()=>handlePlaybackPlaying(video,PLAYER),startTranscodeFallback,switchAutoRung,resetMediaSource,applyPlaybackTransportIntent,handlePlaybackTransportEvent,advance(ms){now+=ms;for(const [id,timer] of [...timers])if(timer.at<=now){timers.delete(id);timer.fn();}}};",
  ].join("\n"))(policy);
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

// M5's create-retry owner sits between `startCopyHls`/`play` and
// `openSession` — a race against its own deadline around an async sequence —
// so the await chain past a resolved create is deeper than two microtask
// turns. Deepening `flush` itself would change the timing every other case in
// this file runs at for the sake of one section, which is how a suite stops
// pinning what it used to; this is local to the section that needs it.
async function flushDeep() {
  for (let turn = 0; turn < 8; turn += 1) await Promise.resolve();
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
    capture: () => captureSnapshot(snapshot(1_000)),
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

  {
    const requests=[],exchanges=[],held=deferred();
    let clock=0;
    const source=snapshot(4_000),sourceOwner={lifecycleId:'capture-test',attachmentGeneration:1};
    const captured=captureSnapshot(source,3,sourceOwner);
    let latest=captureSnapshot(snapshot(9_000),4,{...sourceOwner,attachmentGeneration:2});
    source.selection.audio_track=7;source.capabilities.codecs.push('mutated');
    sourceOwner.attachmentGeneration=99;
    const subject=new control.Reporter({bootstrap:bootstrap(),
      clientInstanceId:'23232323-2323-4323-8323-232323232323',capture:()=>latest,
      send:async(_,request)=>{requests.push(request);return requests.length===1?held.promise:response(request);},
      now:()=>clock,setTimer:()=>1,clearTimer:()=>{},onExchange:event=>exchanges.push(event)});
    // Enqueue A only after its source has moved to B. No metadata may be
    // recaptured from B, nor may source array mutation alter A's wire body.
    subject.notify(captured);
    assert.equal(requests[0].position_ms,4_000);
    assert.equal(requests[0].selection.audio_track,0);
    assert.equal(requests[0].capabilities.codecs.includes('mutated'),false);
    subject.notify(latest);
    const newest=captureSnapshot(snapshot(12_000),5,latest.owner);
    latest=newest;subject.notify(newest);
    held.resolve(Promise.reject(Object.assign(new Error('retry'),{status:429,code:'control_rate_limited'})));
    await flush();await flush();clock=500;await subject.drain();
    assert.strictEqual(requests[0],requests[1],'retry retains exact request body');
    assert.strictEqual(exchanges[0].capture,captured);
    assert.strictEqual(exchanges[1].capture,captured,'retry retains exact captured owner/intent');
    clock=750;await subject.drain();
    assert.equal(requests[2].position_ms,12_000);
    assert.strictEqual(exchanges[2].capture,newest,'coalescing carries one whole source value');
    subject.stop();
  }

  {
    const held=deferred(),events=[];let sent;
    let latest=captureSnapshot(snapshot(4_000),0,{lifecycleId:'attachment',attachmentGeneration:1});
    const original=latest;
    const subject=new control.Reporter({bootstrap:bootstrap(),
      clientInstanceId:'24242424-2424-4424-8424-242424242424',capture:()=>latest,
      send:async(_,request)=>{sent=request;return held.promise;},
      setTimer:()=>1,clearTimer:()=>{},onExchange:event=>events.push(event)}).start();
    latest=captureSnapshot(snapshot(8_000),0,{lifecycleId:'attachment',attachmentGeneration:2});
    held.resolve({...response(sent),action:{type:'terminal',code:'unsupported',message:'old attachment'}});
    await flush();
    assert.strictEqual(events[0].capture,original);
    assert.equal(subject.stopped,false,'same numerical intent does not make a previous attachment terminal current');
    subject.stop();
  }

  {
    const held=deferred(),requests=[],events=[];let clock=0;
    let latest=captureSnapshot(snapshot(1_000),1);
    const subject=new control.Reporter({bootstrap:bootstrap(),
      clientInstanceId:'25252525-2525-4525-8525-252525252525',capture:()=>latest,
      send:async(_,request)=>{requests.push(request);return requests.length===1?held.promise:response(request);},
      now:()=>clock,setTimer:()=>1,clearTimer:()=>{},onExchange:event=>events.push(event)}).start();
    latest=captureSnapshot(snapshot(8_000),2,{lifecycleId:'new-source',attachmentGeneration:3});
    held.resolve(Promise.reject(Object.assign(new Error('owner changed'),{status:409,code:'owner_changed',
      generation:'26262626-2626-4626-8626-262626262626',controlEpoch:8})));
    await flush();await flush();clock=250;await subject.drain();
    assert.equal(requests[1].position_ms,8_000);
    assert.equal(requests[1].sequence,1);
    assert.strictEqual(events[1].capture,latest,'server owner reset adopts one fresh local envelope');
    subject.stop();
  }

  reporter.notify(captureSnapshot(snapshot(2_000, "waiting")));
  reporter.notify(captureSnapshot(snapshot(3_000, "stalled")));
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
  reporter.notify(captureSnapshot(changedCapabilities));
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
    capture: () => captureSnapshot(snapshot()),
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
    capture: () => captureSnapshot(snapshot()),
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
    capture: () => captureSnapshot(snapshot(7_000)),
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
    capture: () => captureSnapshot(snapshot()),
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
      capture: () => captureSnapshot(snapshot(8_000)),
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
    capture: () => captureSnapshot(snapshot(9_000)),
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
    capture: () => captureSnapshot(snapshot()),
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
    capture: () => captureSnapshot(snapshot()),
    send:async ()=>triggerDeferred.promise,
  }).start();
  const typedTrigger=snapshot(12_000,"failed");
  typedTrigger.observation={
    decoder_state:"failed",error_code:"decoder",error_detail:"persistent_decode_stall",
    ignored_private_field:"must not enter log context",
  };
  const triggerContext=triggerReporter.notify(captureSnapshot(typedTrigger));
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
    capture: () => captureSnapshot(acceptedTypedSnapshot),
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
    capture: () => captureSnapshot(endingSnapshot),
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
    capture: () => captureSnapshot(endRaceSnapshot),
    send:async ()=>endRaceResponse.promise,
  }).start();
  oldReplayController.notify(captureSnapshot(snapshot(0,"rendering")));
  assert.equal(oldReplayController.status().pending,true,
    "a replay signal can arrive while terminal end is still in flight");
  oldReplayController.stop();
  const replacementRequests=[];
  const freshReplayController=new control.Reporter({
    bootstrap:Object.assign(bootstrap(),{generation:"dddddddd-dddd-4ddd-8ddd-dddddddddddd"}),
    clientInstanceId:"eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
    capture: () => captureSnapshot(snapshot(0,"rendering")),
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
    "PERSISTENT_STALL_MS", "ENDED_SLACK_SEC", "Hls", "document",
    [
      // `preparedHandoffEnabled` reads storage this scope does not have and
      // therefore uses the default-on value.
      shippedSource("streamHasVideo"),
      shippedSource("preparedHandoffEnabled"), shippedSource("preparedHandoffOffered"),
      shippedSource("pendingPlaybackControlAcknowledgement"),
      shippedSource("playbackControlCapabilities"),
      shippedSource("playbackControlSelection"),
      shippedSource("playbackControlBufferedRange"),
      shippedSource("playbackControlObservationOverride"),
      shippedSource("playbackControlHlsFatal"),
      shippedSource("playbackControlSnapshot"),
      "let PLAYER=null;",
      shippedSource("notifyPlaybackControl"),
      shippedSource("hasPendingPlaybackOpen"),shippedSource("playbackOwnsAttachedMedia"),
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
    {getElementById:()=>video},
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
  player.wantsPlayback=true;
  assert.equal(adapter.playbackControlSnapshot(video,player).demand,"active",
    "an internal reset pause must not tell the producer to hold the requested replacement");
  delete player.wantsPlayback;
  video.paused=false; video.seeking=true; player._seekPreview=20;
  const seeking=adapter.playbackControlSnapshot(video,player);
  assert.equal(seeking.render_state,"seeking");
  assert.equal(seeking.seek_target_ms,20_000);
  video.seeking=false; player._seekPreview=null;
  player.controlSeek={sequence:7,targetMs:45_000};
  const committedSeek=adapter.playbackControlSnapshot(video,player);
  assert.equal(committedSeek.position_ms,15_000, "the current playhead stays current");
  assert.equal(committedSeek.seek_target_ms,45_000,
    "a committed destination survives after preview state is cleared");
  player.controlSeek=null;
  let presentationNow=0;
  const seekIntentAdapter=new Function("performance",[
    "let PLAYER=null; let notifications=0;",
    surfaceSeam(),
    "function clearPlaybackControlWaiters(){}",
    "function notifyPlaybackControl(){notifications+=1;}",
    shippedSource("supersedePlaybackControlIntent"),
    shippedSource("beginPlaybackControlSeek"),
    shippedSource("markPlaybackControlSeekExecuted"),
    shippedSource("samplePlaybackPresentationClock"),
    shippedSource("settlePlaybackControlSeek"),
    shippedSource("hasPendingPlaybackOpen"),shippedSource("playbackOwnsAttachedMedia"),
    "return {begin(p,target){PLAYER=p;return beginPlaybackControlSeek(p,target);},mark:markPlaybackControlSeekExecuted,"+
      "settle:settlePlaybackControlSeek,sample:samplePlaybackPresentationClock,"+
      "notifications:()=>notifications};",
  ].join("\n"))({now:()=>presentationNow});
  const intentPlayer={started:true,offset:0,source:{video_codec:"h264"},
    controlPresentedFrames:0,controlHasFrameCallbacks:true};
  const intentVideo={currentTime:10,seeking:false,readyState:4,paused:false,playbackRate:1};
  const firstIntent=seekIntentAdapter.begin(intentPlayer,30);
  const finalIntent=seekIntentAdapter.begin(intentPlayer,90);
  assert.equal(finalIntent.sequence,firstIntent.sequence+1,"later seeks supersede monotonically");
  assert.equal(seekIntentAdapter.settle(intentVideo,intentPlayer,90,1),false,
    "a target-looking frame before execution cannot settle the intent");
  assert.equal(intentPlayer.controlSeek.targetMs,90_000);
  assert.equal(seekIntentAdapter.mark(intentPlayer,90,intentVideo),true);
  assert.equal(seekIntentAdapter.settle(intentVideo,intentPlayer,90.4,1),false,
    "the old loose landing tolerance is not accepted");
  assert.equal(seekIntentAdapter.settle(intentVideo,intentPlayer,90.2,1),true,
    "a new presented frame at the destination clears its own intent");
  assert.equal(intentPlayer.controlSeek,null);
  assert.equal(seekIntentAdapter.notifications(),3,
    "both intents and the presented landing are published");

  // A delayed audio sample can be a second beyond the landing window. The
  // first sample proves the destination; the second must advance, not retreat.
  const audioIntentPlayer={started:true,offset:0,source:{},controlHasFrameCallbacks:true};
  const audioIntentVideo={currentTime:45,paused:false,seeking:false};
  seekIntentAdapter.begin(audioIntentPlayer,45);
  seekIntentAdapter.mark(audioIntentPlayer,45,audioIntentVideo);
  assert.equal(seekIntentAdapter.settle(audioIntentVideo,audioIntentPlayer),false);
  audioIntentVideo.currentTime=44.999;
  assert.equal(seekIntentAdapter.settle(audioIntentVideo,audioIntentPlayer),false);
  audioIntentVideo.currentTime=45;
  assert.equal(seekIntentAdapter.settle(audioIntentVideo,audioIntentPlayer),false,
    "a backward sample cannot lower the proof threshold");
  audioIntentVideo.currentTime=46;
  presentationNow+=1000;
  assert.equal(seekIntentAdapter.settle(audioIntentVideo,audioIntentPlayer),true);

  for(const rate of [1,2]){
    const p={started:true,offset:0,source:{},controlHasFrameCallbacks:true};
    const v={currentTime:60,paused:false,seeking:false,playbackRate:rate};
    seekIntentAdapter.begin(p,60);
    seekIntentAdapter.mark(p,60,v);
    presentationNow+=1000; v.currentTime=60+rate;
    assert.equal(seekIntentAdapter.settle(v,p),false,
      `the first ${rate}x audio sample at 1 Hz records landing without settling`);
    presentationNow+=1000; v.currentTime=60+2*rate;
    assert.equal(seekIntentAdapter.settle(v,p),true,
      `a later advancing ${rate}x sample settles without needing a 250 ms poll`);
  }
  {
    const p={started:true,offset:0,source:{},controlHasFrameCallbacks:true};
    const v={currentTime:30,paused:false,seeking:false,playbackRate:1};
    seekIntentAdapter.begin(p,30); seekIntentAdapter.mark(p,30,v);
    presentationNow+=500; v.paused=true; seekIntentAdapter.sample(v,p);
    presentationNow+=60_000; v.paused=false; seekIntentAdapter.sample(v,p);
    presentationNow+=500; v.currentTime=32;
    assert.equal(seekIntentAdapter.settle(v,p),false);
    assert.equal(p.controlSeek.audioPositionMs,null,"paused time cannot justify a wrong landing");
    v.currentTime=31;
    assert.equal(seekIntentAdapter.settle(v,p),false);
    presentationNow+=500; v.playbackRate=2; seekIntentAdapter.sample(v,p);
    presentationNow+=500; v.currentTime=32.5;
    assert.equal(seekIntentAdapter.settle(v,p),true,"rate edges integrate the prior active rate");
    seekIntentAdapter.begin(p,90); seekIntentAdapter.mark(p,90,v);
    presentationNow+=30_000; v.currentTime=110;
    assert.equal(seekIntentAdapter.settle(v,p),false);
    assert.equal(p.controlSeek.audioPositionMs,null,"landing allowance is bounded to eight active seconds");
  }
  {
    const callbacks=[];
    const queue=new Function(shippedSource("queuePlaybackFrame")+"; return queuePlaybackFrame;")();
    const p={started:true,offset:0,source:{video_codec:"h264"},controlHasFrameCallbacks:true};
    const v={currentTime:80,paused:false,seeking:false,
      requestVideoFrameCallback:callback=>callbacks.push(callback)};
    const epochs=[];
    queue(v,p,(_now,_meta,epoch)=>epochs.push(epoch));
    seekIntentAdapter.begin(p,80); seekIntentAdapter.mark(p,80,v);
    queue(v,p,(_now,_meta,epoch)=>epochs.push(epoch));
    callbacks.forEach(callback=>callback(presentationNow,{mediaTime:80}));
    assert.notEqual(epochs[0],p.controlPresentationEpoch,
      "a callback queued by the predecessor cannot settle the new execution");
    assert.equal(epochs[1],p.controlPresentationEpoch);
    presentationNow+=1000;
    assert.equal(seekIntentAdapter.settle(v,p,81,1),true,
      "delayed video presentation is checked against the active media timeline");
    assert.match(SHIPPED_UI,/if\(epoch===\(p\.controlPresentationEpoch\|\|0\)\)\s*settlePlaybackControlSeek/,
      "the shipped detector enforces callback ownership before settlement");
  }

  // Run the actual menu operations, including the progressive audio branch
  // that previously threw ReferenceError before marking its intent executed.
  function replacementHarness(){
    return new Function([
      "let PLAYER=null; const calls=[],released=[],pending=[]; let held=false,failure=null; let PENDING_ATTEMPT_REASON=null; let PENDING_DEFAULT_SUB_OFF=false;",
      surfaceSeam(),
      "const video={currentTime:10,querySelectorAll:()=>[],pause(){},play:()=>Promise.resolve()};",
      "const document={getElementById:id=>id==='video'?video:null};",
      "const localStorage={getItem:()=> 'auto',setItem:()=>{}};",
      "const PlaybackPolicy={subtitleBurnAction:()=> 'burn',hlsTransport:()=> 'mse',copyAudioNeedsTranscode:()=>false,indexPendingFallback:()=> 'progressive_remux',stallReopenSessionOptions:({options})=>options,"+
        "CREATE_RETRY:{deadline_ms:60000,backoff_ms:[1000,2000,4000]},createRetryStep:()=>({action:'fail'}),classifyStreamFailure:()=>null,streamFailureOverlay:()=>null}; const PLAY_CAPS={acodec:'aac'};",
      "function newRequestId(){return 'rq';}",
      shippedSource("playbackRetryDelay"),shippedSource("playbackCreateRetryContext"),
      shippedSource("openSessionRetryingNotYet"),
      "function closeMenu(){} function toast(){} function qualityLabel(){return '720p';}",
      "function clientLog(){} function playbackContext(){return {};} function renderPlayerInfo(){}",
      "function clearPlaybackControlWaiters(){} function notifyPlaybackControl(){} function endWait(){}",
      "function play(id,title,position){calls.push({kind:'play',position});}",
      "function newAttempt(){} function teardownHls(){} function resetMediaSource(){}",
      "function clearSubs(){} function pbSyncSubIcon(){} function setLoading(){} function armStall(){}",
      "function subNeedsBurn(){return true;} function subLabelFor(){return 'PGS';}",
      "function sessionHeight(){return null;} function langName(){return 'English';}",
      "function useNativeHls(){return false;} function showSessionOpenFailure(){return false;}",
      "function showStallRecoveryFailure(){}",
      "function selectedAudioIndex(){return 0;} function hlsJsSupported(){return true;} function mseCanTake(){return true;} function clearStreamFailure(){}",
      "function transcodeOpts(position){return {position};}",
      "async function openSession(id,opts){calls.push({kind:'session',position:(opts.position??opts.start)*1000});if(failure)throw failure;if(held)return new Promise(resolve=>pending.push(resolve));return {session_id:'replacement'};}",
      "function releaseSession(id){released.push(id);}",
      "function attachSession(v,p,info,pos){markPlaybackControlSeekExecuted(p,pos);return pos;}",
      "function remuxUrl(path,audio,pos){calls.push({kind:'remux',position:pos*1000});return '/remux';}",
      shippedSource("supersedePlaybackControlIntent"),
      shippedSource("positionForPlaybackIntent"),
      shippedSource("beginPlaybackControlSeek"),
      shippedSource("markPlaybackControlSeekExecuted"),
      shippedSource("samplePlaybackPresentationClock"),
      shippedSource("streamGeneration"),
      shippedSource("restartPendingPlaybackOpen"),
      shippedSource("requestPlaybackMediaChange"),shippedSource("executePlaybackMediaChange"),
      shippedSource("hasPendingPlaybackOpen"),shippedSource("playbackOwnsAttachedMedia"),
      shippedSource("beginPlaybackPreparation"),
      shippedSource("rememberPlaybackTransportIntent"),
      shippedSource("applyPlaybackTransportIntent"),
      shippedSource("pausePlaybackInternally"),
      shippedSource("resetPlaybackTransportEvents"),shippedSource("playbackTransportEvents"),
      shippedSource("setPlaybackMediaSource"),
      shippedSource("retirePlaybackPredecessor"),
      shippedSource("rememberPlaybackSelection"),
      shippedSource("offsetLabel"),
      shippedSource("startCopyHls"),
      shippedSource("setQuality"),shippedSource("setSync"),
      shippedSource("setSub"),shippedSource("burnSub"),shippedSource("switchAudio"),
      "return {attach(p){PLAYER=p;},hold(){held=true;},rejectWith(error){failure=error;},released,pending,calls,video,setQuality,setSync,setSub,switchAudio,startCopyHls,streamGeneration,beginPlaybackControlSeek};",
    ].join("\n"))();
  }
  for(const operation of ["quality","audio-sync","subtitle-burn","audio-track"]){
    const h=replacementHarness();
    const p={fileId:'film',title:'Film',offset:0,started:true,method:'remux',vod:true,
      source:{video_codec:'h264'},controlHasFrameCallbacks:true,
      controlSeek:{sequence:1,targetMs:90_000,executed:true},controlSeekSequence:1,
      audio:[{index:0},{index:1}],curAudio:0,curSub:-1,subs:[{index:2}],burnedSub:null};
    h.attach(p);
    if(operation==='quality') h.setQuality('720');
    if(operation==='audio-sync') await h.setSync(250);
    if(operation==='subtitle-burn') await h.setSub(2);
    if(operation==='audio-track') await h.switchAudio(1);
    assert.equal(h.calls.length,1,`${operation} makes one replacement`);
    assert.equal(h.calls[0].position,90_000,`${operation} retains the unpresented seek destination`);
    assert.equal(p.controlSeek.targetMs,90_000);
    if(operation==='audio-track') assert.equal(p.controlSeek.executed,true);
  }
  {
    const h=replacementHarness();
    const p={controlHasFrameCallbacks:true,controlSeekSequence:1}; h.attach(p);
    const old=h.streamGeneration();
    h.beginPlaybackControlSeek(p,90);
    assert.equal(old.live(),false,"a new destination immediately fences an older open, before seek debounce");
  }
  for(const command of ['audio','burn']){
    const h=replacementHarness();
    const p={fileId:'film',method:'transcode',audio:[{index:0},{index:1}],curAudio:0,
      subs:[{index:2}],curSub:-1,burnedSub:null,offset:0,controlHasFrameCallbacks:true};
    h.attach(p);h.hold();
    const opening=command==='audio'?h.switchAudio(1):h.setSub(2);
    assert.equal(h.pending.length,1);
    h.beginPlaybackControlSeek(p,120);
    h.pending[0]({session_id:'abandoned-'+command});await opening;
    assert.deepEqual(h.released,['abandoned-'+command],'an obsolete successful create releases its resource');
  }
  {
    const h=replacementHarness();
    const p={fileId:'film',method:'remux',source:{video_codec:'h264'},audio:[{index:0,codec:'aac'}],curAudio:0,
      controlHasFrameCallbacks:true};h.attach(p);h.beginPlaybackControlSeek(p,90);
    h.rejectWith({code:'vod_index_pending'});
    assert.equal(await h.startCopyHls(h.video,90,()=>true),true);
    assert.equal(p.copyHls,false);
    assert.equal(p.controlSeek.executed,true,'progressive index fallback must execute the same destination');
  }
  const fullPlayer=()=>({fileId:'film',title:'Film',offset:0,started:true,method:'direct_play',
    knownDur:600_000,source:{video_codec:'h264'},audio:[{index:0},{index:1}],curAudio:0,
    subs:[{index:2,text:true}],curSub:-1,burnedSub:null,preplay:{audio:0,subtitle:-1},
    controlHasFrameCallbacks:true,wantsPlayback:true});
  // Execute the shipped command -> retained recipe -> session-await -> attach
  // chain. Only the server response and media element are simulated here.
  for(const first of ['audio','copy-audio','burn','fallback','auto-rung']){
    const h=fullOpenHarness(),p=fullPlayer();
    Object.assign(p,{method:first==='fallback'||first==='copy-audio'?'remux':'transcode',
      copyHls:first==='copy-audio',vod:true,autoHeight:720,
      subs:[{index:2,burn:true}],abr:{switching:false,switches:[],stallEvents:{supply:[],decode:[]}}});
    h.attach(p);
    const opening=first==='audio'||first==='copy-audio'?h.switchAudio(1):first==='burn'?h.setSub(2):
      first==='fallback'?h.startTranscodeFallback('decode-rescue'):
      h.switchAutoRung(720,{height:360,reason:'supply'});
    assert.equal(h.sessions.length,1,first);
    await h.seekTo(120);
    assert.equal(h.sessions.length,2,`${first} → seek reexecutes the unattached media recipe`);
    const latest=h.sessions[1].opts;
    assert.equal(latest.start,120);
    if(first==='audio'||first==='copy-audio')assert.equal(latest.audio,1);
    if(first==='copy-audio')assert.equal(latest.copy,true);
    if(first==='burn')assert.equal(latest.subtitle_burn,2);
    if(first==='auto-rung')assert.equal(latest.height,360);
    h.sessions[0].resolve({session_id:'obsolete'});await opening;await flush();
    assert.deepEqual(h.released,['obsolete']);
    assert.equal(h.media.filter(x=>x.attached).length,0,'stale success cannot attach');
    // M5 put the retry wrapper between this create and its attachment, on both
    // branches of a pending change, so the chain past a resolved create is
    // deeper than two microtask turns.
    h.sessions[1].resolve({session_id:'current'});await flushDeep();
    assert.equal(p.pendingMediaChange,null);
    assert.equal(p.controlSeek.targetMs,120_000);
    assert.equal(p.controlSeek.executed,true);
    assert.equal(h.media.filter(x=>x.attached).length,1);
    if(first==='auto-rung'){
      assert.equal(p.abr.switching,false);
      assert.deepEqual(p.abr.switches,[{from:720,to:360,reason:'supply',pos:120,id:'current'}]);
    }
    await h.seekTo(150);
    assert.equal(h.sessions.length,2,'clean VOD keeps its native seek fast path');
    assert.equal(h.video.currentTime,150);
  }
  for(const later of ['burn','native','offset']){
    const h=fullOpenHarness(),p=fullPlayer();
    Object.assign(p,{method:'transcode',vod:true,subs:[{index:2,burn:later==='burn'}]});h.attach(p);
    const first=h.switchAudio(1);
    const second=later==='offset'?h.setSync(250):h.setSub(2);
    assert.equal(h.sessions.length,2);
    assert.equal(h.sessions[1].opts.audio,1,`audio → ${later} retains the requested audio`);
    if(later==='burn')assert.equal(h.sessions[1].opts.subtitle_burn,2);
    if(later==='offset')assert.equal(h.sessions[1].opts.audio_offset_ms,250);
    h.sessions[0].resolve({session_id:'old'});await first;
    h.sessions[1].resolve({session_id:'new'});await second;await flush();await flush();
    assert.equal(p.pendingMediaChange,null);
    assert.equal(p.curSub,later==='offset'?-1:2);
  }
  {
    const h=fullOpenHarness(),p=fullPlayer();p.subs=[{index:2,burn:true}];h.attach(p);
    const burn=h.setSub(2);assert.equal(h.sessions.length,1);
    await h.setSub(-1);
    assert.equal(h.decisions.length,1,'burn → Off rebuilds without burned pixels');
    assert.equal(h.decisions[0].selection.subtitle,-1);
    assert.equal(p.burnedSub,null);
    h.sessions[0].resolve({session_id:'obsolete-burn'});await burn;
    assert.deepEqual(h.released,['obsolete-burn']);
    assert.equal(h.media.filter(x=>x.attached).length,0);
  }
  {
    const h=fullOpenHarness(),p=fullPlayer();h.attach(p);
    h.setQuality('original');
    await h.startTranscodeFallback('stream-rejected');
    assert.equal(h.sessions.length,0,'a failed incumbent cannot steal a pending manual Original decision');
    assert.equal(p.pendingMediaChange,null);
  }
  {
    const h=fullOpenHarness(),p=fullPlayer();p.method='transcode';h.attach(p);
    const old=h.switchAudio(1);
    h.play('different-title','New film',0,600_000,null);
    assert.equal(h.decisions.length,1);
    h.sessions[0].resolve({session_id:'old-title'});await old;
    assert.deepEqual(h.released,['old-title']);
    assert.equal(h.media.filter(x=>x.attached).length,0,'a late partial create cannot attach during the new title decision');
  }
  {
    const h=fullOpenHarness(),p=fullPlayer();Object.assign(p,{method:'transcode',vod:false,abr:{switching:false}});h.attach(p);
    const oldSeek=h.seekTo(120);
    h.play('different-title','New film',0,600_000,null);
    await oldSeek;
    await h.startTranscodeFallback('stream-rejected');
    await h.switchAutoRung(720,{height:360,reason:'supply'});
    const painted=h.surface().length,stopped=h.stops();h.incumbentError();
    assert.equal(h.sessions.length,0,'delayed old seek and newly fired fallback/ABR cannot steal the next title decision');
    assert.equal(h.surface().length,painted,'the complete incumbent error handler is fenced before it raises anything');
    assert.equal(h.stops(),stopped,'and before it stops a player that is not its own');
  }
  for(const native of [false,true]){
    const h=new Function("native",[
      surfaceSeam(),
      "let PLAYER={offset:0,wantsPlayback:true,controlSeekSequence:1}; const instances=[],metadata=[];",
      "const video={paused:false,currentTime:10,playCount:0,pause(){this.paused=true;},play(){this.paused=false;this.playCount++;return Promise.resolve();},addEventListener(e,f){metadata.push(f);},removeEventListener(){}};",
      "const document={getElementById:()=>video}; const window={Hls:true}; const TOKEN=null;",
      "class Hls{static Events={MANIFEST_PARSED:'manifest',ERROR:'error',LEVEL_LOADED:'level',BUFFER_FLUSHING:'flush',FRAG_CHANGED:'frag',BUFFER_APPENDED:'append'}; static isSupported(){return true;} constructor(){this.events={};instances.push(this);} on(e,f){this.events[e]=f;} loadSource(){} attachMedia(){} destroy(){}}",
      "const PlaybackPolicy={bandwidthSeedBps:()=>null,HLS_STARTUP:{cold_deadline_ms:40000,manifest_load_policy:{default:{}}}}; function preferNativeHls(){return native;} function bufferTargets(){return {fwd:20,back:10};} function vodClientContract(){return {};}",
      "function clearStreamFailure(){} function refreshSegTimes(){} function tok(url){return url;} function clearStreamFailureFor(){} function retuneBuffer(){throw Error('stale retune');} function markEvent(){throw Error('stale telemetry');}",
      // Teardown now settles a staged successor before it destroys the
      // incumbent, so the scope carries that path rather than a stub of it.
      "function clientLog(){} function playbackContext(){return {};} function notifyPlaybackControl(){}",
      shippedConst("PREPARED_TERMINAL_STATES"),
      shippedSource("preparedVideoElement"), shippedSource("preparedState"),
      shippedSource("queuePlaybackControlAcknowledgement"),
      shippedSource("destroyHlsInstance"),
      shippedSource("freePreparedReplacement"), shippedSource("abandonPreparedReplacement"),
      "function cancelPreparedFirstFrame(){} function cancelHlsStartup(){}",
      shippedSource("rememberPlaybackTransportIntent"),shippedSource("pausePlaybackInternally"),
      shippedSource("resetPlaybackTransportEvents"),shippedSource("playbackTransportEvents"),
      shippedSource("setPlaybackMediaSource"),
      shippedSource("applyPlaybackTransportIntent"),shippedSource("teardownHls"),
      shippedSource("beginPlaybackMediaAttachment"),shippedSource("applyPlaybackAttachmentPosition"),
      shippedSource("playbackAttemptTerminallyStopped"),
      shippedSource("attachHls"),
      shippedSource("hasPendingPlaybackOpen"),shippedSource("playbackOwnsAttachedMedia"),
      "return {p:PLAYER,video,instances,metadata,attach:()=>attachHls(video,'/session/index.m3u8',30),teardownHls};",
    ].join("\n"))(native);
    h.attach();
    h.p.controlSeekSequence=2;
    h.p.controlSeek={sequence:2,targetMs:90_000,executed:true};
    if(native){
      h.metadata[0]();
      assert.equal(h.video.currentTime,90,'late native metadata retains the newest seek, not its original start');
    }else{
      h.instances[0].events.manifest();
      assert.equal(h.video.paused,false,'a seek during manifest loading cannot suppress playback startup');
      h.p.wantsPlayback=false;
      h.instances[0].events.manifest();
      assert.equal(h.video.paused,true,'the same attachment reads the latest Pause');
    }
    const oldPlays=h.video.playCount;
    h.teardownHls();h.video.currentTime=120;
    if(native)h.metadata[0]();else h.instances[0].events.manifest();
    assert.equal(h.video.currentTime,120,'departed metadata cannot rewind the next attachment');
    assert.equal(h.video.playCount,oldPlays,'departed manifest cannot play the next attachment');
    if(!native) assert.doesNotThrow(()=>h.instances[0].events.error(null,{fatal:true}),
      'a departed decoder failure cannot recover the replacement');
    if(!native) for(const name of ['level','flush','frag','append'])assert.doesNotThrow(
      ()=>h.instances[0].events[name](null,{details:{targetduration:2}}),
      `departed ${name} cannot mutate the replacement or its telemetry`);
  }
  const resolveDecision=call=>call.resolve({method:'direct_play',play_url:'/original.mp4',
    source:{video_codec:'h264',duration_ms:600_000},
    audio:[{index:0,default:call.selection?.audio!==1},{index:1,default:call.selection?.audio===1}],
    subtitles:[{index:2,text:true}],ladder:[]});
  {
    const h=fullOpenHarness(),p=fullPlayer();p.method='transcode';h.attach(p);h.attachHls();
    const incumbent=h.hlsInstances[0],opening=h.switchAudio(1);
    const painted=h.surface().length,stopped=h.stops();
    for(const event of ['flush','frag','level','loaded','append'])assert.doesNotThrow(
      ()=>incumbent.events[event](null,{details:{targetduration:2},frag:{duration:2,stats:{total:4096,loading:{start:0,end:1}}}}),
      `incumbent HLS ${event} cannot mutate or attribute the pending partial recipe`);
    for(const details of [{fatal:true,type:'mediaError',details:'codec'},
      {fatal:false,details:'bufferStalledError'}, {fatal:true,details:'quota'}])
      assert.doesNotThrow(()=>incumbent.events.error(null,details));
    let load;incumbent.config.xhrSetup({status:503,responseText:'old error',addEventListener(_,callback){load=callback;}});
    assert.doesNotThrow(()=>load());
    assert.equal(h.surface().length,painted,
      'an incumbent HLS instance cannot raise a fault about the pending recipe');
    assert.equal(h.stops(),stopped,'nor stop the player under it');
    assert.equal(h.sessions.length,1);
    assert.equal(p._hlsEvt,undefined);assert.equal(p.segBytes,undefined);assert.equal(p._levelAt,undefined);
    h.advance(20_000);await opening;
    assert.equal(h.sessions[0].signal.aborted,true,'HLS callbacks cannot replace the original preparation deadline');
  }
  {
    const h=fullOpenHarness();
    const opening=h.play('cold','Cold film',0,600_000,null);
    h.advance(20_000);await opening;
    assert.equal(h.decisions[0].signal.aborted,true);
    // Cold: no predecessor holds the picture, so the failed start is the owner
    // out of rungs — a blocking fault over a player it stopped first.
    assert.equal(h.surface().at(-1).title,'Playback could not prepare.');
    assert.equal(h.surface().at(-1).source,'owner_stopped');
    assert.equal(h.surface().at(-1).player_stopped,true);
    assert.equal(h.stops(),1,'a blocking surface is only drawn over a stopped player');
    const retry=h.retryPlayback();
    assert.equal(h.decisions.length,2,'cold decision timeout has a working Retry without PLAYER');
    resolveDecision(h.decisions[1]);await retry;
    assert.equal(h.current().fileId,'cold');
  }
  for(const prior of ['cold','different-title']){
    const h=fullOpenHarness();if(prior==='different-title')h.attach(fullPlayer());
    const opening=h.play('new-title','New film',0,600_000,null);
    h.togglePlay();resolveDecision(h.decisions[0]);await opening;
    assert.equal(h.current().wantsPlayback,false,`Pause during ${prior} decision belongs to the new full-open intent`);
    assert.equal(h.video.paused,true);
  }
  for(const moment of ['pending','timed-out']){
    const h=fullOpenHarness(),opening=h.play('cold','Cold film',0,600_000,null);
    if(moment==='timed-out'){h.advance(20_000);await opening;}
    assert.doesNotThrow(()=>h.closePlayer(),`actual Close is available during cold ${moment}`);
    await opening;
    assert.equal(h.isOpen(),false);assert.equal(h.node.inert,false);
    assert.deepEqual(h.posted,[],'unattached cold media has no watch progress');
  }
  {
    const h=fullOpenHarness(),p=fullPlayer();p.knownDur=10_800_000;p.sessionId='A';h.attach(p);h.video.currentTime=5400;
    const opening=h.play('new-title','New film',0,3_600_000,null);
    h.decisions[0].resolve({method:'transcode',source:{video_codec:'h264',duration_ms:3_600_000},audio:[{index:0,default:true}],subtitles:[],ladder:[]});
    for(let i=0;i<4;i++)await flush();
    h.closePlayer();await opening;
    assert.equal(h.posted.length,1);
    assert.equal(h.posted[0].path,'/items/film-item/progress');
    assert.equal(h.posted[0].body.position_ms,5_400_000,'Close attributes the shared media clock to its actual predecessor, never the pending next title');
    assert.deepEqual(h.released,['A']);
  }
  {
    const h=fullOpenHarness(),p=fullPlayer();p.sessionId='working';h.attach(p);
    const opening=h.play('film','Film',10_000,600_000,null);
    h.advance(19_000);
    h.decisions[0].resolve({method:'transcode',source:{video_codec:'h264'},audio:[{index:0,default:true}],subtitles:[],ladder:[]});
    for(let i=0;i<4;i++)await flush();
    assert.equal(h.sessions.length,1);
    assert.deepEqual(h.released,[],'the working predecessor survives session preparation');
    h.togglePlay();assert.equal(h.current().wantsPlayback,false);
    h.advance(1000);await opening;
    assert.equal(h.current(),p,'timeout keeps the actual working predecessor');
    assert.equal(p.wantsPlayback,false,'timeout preserves Pause issued on the pending player');
    assert.equal(h.sessions[0].signal.aborted,true,'decision and create share one absolute20s budget');
    h.sessions[0].resolve({session_id:'late-create'});for(let i=0;i<3;i++)await flush();
    assert.deepEqual(h.released,['late-create'],'a late timed-out session is released without attachment');
    assert.equal(h.media.filter(x=>x.attached).length,0);
    const retry=h.retryPlayback();resolveDecision(h.decisions[1]);await retry;
    assert.equal(h.current().wantsPlayback,false);
    assert.equal(h.video.paused,true);
  }
  {
    const h=fullOpenHarness(),p=fullPlayer();p.sessionId='incumbent';h.attach(p);
    const first=h.play('film','Film',10_000,600_000,null);
    const transcode={method:'transcode',source:{video_codec:'h264'},audio:[{index:0,default:true}],subtitles:[],ladder:[]};
    h.decisions[0].resolve(transcode);for(let i=0;i<4;i++)await flush();
    await h.seekTo(90);
    assert.equal(h.decisions.length,2);
    h.sessions[0].resolve({session_id:'superseded'});await first;
    h.decisions[1].resolve(transcode);for(let i=0;i<4;i++)await flush();
    h.sessions[1].resolve({session_id:'current'});for(let i=0;i<4;i++)await flush();
    assert.deepEqual(h.released,['superseded','incumbent'],'superseded pending players retain and retire the real predecessor exactly once');
    assert.equal(h.current().mediaPredecessor,null);
  }
  for(const endpoint of ['decision','create'])for(const boundary of ['headers','body']){
    const h=new Function([
      "let now=0,timer,bodyResolve;const performance={now:()=>now},requests=[],released=[];let TOKEN=null,AUTH_GENERATION=0,PLAYER=null;const API='/api',PLAYBACK_ID='playback';const PlaybackPolicy={};",
      "function setTimeout(fn){timer=fn;return 1;}function clearTimeout(){}function fetch(url,options){return new Promise(resolve=>requests.push({url,options,resolve}));}function logout(){}",
      "function currentCapsDocument(){return {};}function capsDocumentIsUsable(){return false;}function prePlaySelectionQuery(){return '';}function decisionUrl(){return '/decision';}function vodClientContract(){return {session:{}};}function newRequestId(){return 'request';}",
      shippedSource('api'),shippedSource('askDecision'),shippedSource('openSession'),shippedSource('beginPlaybackPreparation'),
      "return {requests,released,start(endpoint){const owner=beginPlaybackPreparation(()=>true);return owner.run(signal=>endpoint==='decision'?askDecision('f','auto',null,signal):openSession('f',{start:0},signal),value=>{if(value.session_id)released.push(value.session_id);});},headers(bodyHeld){requests[0].resolve({status:200,ok:true,json:()=>bodyHeld?new Promise(resolve=>bodyResolve=resolve):Promise.resolve({session_id:'late'})});},body(){bodyResolve({session_id:'late'});},expire(){now=20000;timer();}};",
    ].join('\n'))();
    const opening=h.start(endpoint);const outcome=opening.catch(error=>error);
    if(boundary==='body'){h.headers(true);await flush();await flush();}
    h.expire();assert.equal((await outcome).name,'TimeoutError',`${endpoint} ${boundary}`);
    assert.equal(h.requests[0].options.signal.aborted,true);
    if(boundary==='headers')h.headers(false);else h.body();
    for(let i=0;i<5;i++)await flush();
    assert.deepEqual(h.released,['late'],'late complete resources are disposed even when fetch/body ignores abort');
  }
  for(const phase of ['full-decision','full-create','partial-create','unlanded']){
    const h=fullOpenHarness(),p=fullPlayer();p.method='transcode';h.attach(p);h.video.currentTime=100;
    let opening;
    if(phase.startsWith('full')){
      opening=h.play('new-title','New film',0,120_000,null);
      if(phase==='full-create'){
        h.decisions[0].resolve({method:'transcode',source:{video_codec:'h264'},audio:[{index:0,default:true}],subtitles:[],ladder:[]});
        for(let i=0;i<5;i++)await flush();
      }
    }else if(phase==='partial-create')opening=h.switchAudio(1);
    const pending=h.current();
    const marker={kind:'credits',start_ms:90_000,end_ms:120_000,provenance:'authored'};
    Object.assign(pending,{durMs:120_000,autoskip:true,markers:[marker],_markerOffers:new Set(),
      controlSeek:{sequence:3,targetMs:100_000,executed:true,frameFloor:0},curSub:2,_subOff:90});
    h.node.dataset.k='old-marker';h.node.innerHTML='old offer';
    const creates=h.sessions.length,decisions=h.decisions.length,started=pending.started;
    h.checkMarkers();h.skipCurrent();h.skipMarker(marker);h.pbTick();
    assert.equal(h.node.dataset.k,'');assert.equal(h.node.innerHTML,'');
    assert.equal(marker._auto,undefined);assert.deepEqual(h.posted,[]);
    assert.equal(h.sessions.length,creates);assert.equal(h.decisions.length,decisions);
    if(phase!=='unlanded'){
      assert.equal(h.settle(),false,'predecessor timeupdate/frame evidence cannot settle pending replacement');
      h.playing();h.incumbentWaiting();await h.reportProgress(pending.fileId);
      assert.equal(pending.started,started,'predecessor playing cannot establish the requested player');
      assert.deepEqual(h.ttff,[]);assert.deepEqual(h.posted,[]);
      h.advance(9000);for(let i=0;i<4;i++)await flush();
      assert.equal(h.sessions.length,creates);assert.equal(h.decisions.length,decisions,'old waiting cannot reset preparation');
      h.advance(11_000);await opening;
      assert.equal((h.sessions[0]||h.decisions[0]).signal.aborted,true,'original preparation retains its20s bound');
    }
  }
  for(const delayedBoundary of [0,1,2,3]){
    for(const interruption of ['title','seek','open']){
      const h=new Function([
        "let PLAYER={fileId:'f',attemptId:'a1'},AUTOPLAY=null,closed=0;const pending=[],loading=[],location={hash:''},ITEM_FOR_FILE={f:'e1'};",
        surfaceSeam(),
        "function api(){return new Promise(resolve=>pending.push(resolve));}function exactWireId(x){return x.id;}function toast(){}function autoNextOn(){return true;}function setLoading(value){loading.push(value);}function closePlayer(){closed++;}",
        shippedSource("playbackContinuation"),shippedSource("playNextEpisode"),shippedSource("finishPlayback"),
        shippedSource("beginPlaybackPreparation"),
        "return {pending,location,loading,surface:surfacePainted,events:()=>surfaceEvents,start:()=>finishPlayback(true),result:()=>({AUTOPLAY,closed}),interrupt(kind){if(kind==='title')PLAYER={fileId:'other'};else if(kind==='seek')PLAYER.controlIntentGeneration=1;else PLAYER.pendingOpenAttempt={};}};",
      ].join("\n"))();
      const replies=[{item:{kind:'episode'},ancestors:[{id:'show'},{id:'s1'}]},
        {children:[{id:'e1',kind:'episode'}]},
        {children:[{id:'s1',kind:'season'},{id:'s2',kind:'season'}]},
        {children:[{id:'e2',kind:'episode',title:'Next'}]}];
      const opening=h.start();
      for(let i=0;i<delayedBoundary;i++){h.pending[i](replies[i]);await flush();await flush();}
      h.interrupt(interruption);h.pending[delayedBoundary](replies[delayedBoundary]);await opening;
      assert.deepEqual(h.result(),{AUTOPLAY:null,closed:0},`${interruption} at next-episode await ${delayedBoundary}`);
      assert.equal(h.location.hash,'');
      // The "Up next…" fault is raised and left standing: an obsolete autoplay
      // lookup must not settle an intent that belongs to the newer player.
      assert.deepEqual(h.surface().map((entry) => entry.title), ['Up next…'],
        'obsolete autoplay cannot hide the newer player loading state');
      assert.deepEqual(h.events(), [],
        'and cannot retire it either — the intent it belongs to never settled');
    }
  }
  for(const delayedBoundary of [0,1,2,3])for(const boundary of ['headers','body']){
    const h=new Function([
      "let PLAYER={fileId:'f',attemptId:'a1'},AUTOPLAY=null,closed=0,now=0,timerId=0;const pending=[],loading=[],notices=[],timers=new Map(),location={hash:''},ITEM_FOR_FILE={f:'e1'};",
      surfaceSeam(),
      "const performance={now:()=>now};const API='/api',TOKEN=null,AUTH_GENERATION=0;function logout(){}function fetch(url,options){return new Promise(resolve=>pending.push({url,options,resolve}));}",
      "function setTimeout(fn,ms){const id=++timerId;timers.set(id,{fn,at:now+ms});return id;}function clearTimeout(id){timers.delete(id);}function exactWireId(x){return x.id;}function toast(text){notices.push(text);}function autoNextOn(){return true;}function setLoading(value){loading.push(value);}function closePlayer(){closed++;}",
      shippedSource('api'),shippedSource('beginPlaybackPreparation'),
      shippedSource('playbackContinuation'),shippedSource('playNextEpisode'),shippedSource('finishPlayback'),
      "return {pending,loading,notices,surface:surfacePainted,events:()=>surfaceEvents,location,start:()=>finishPlayback(true),result:()=>({AUTOPLAY,closed}),advance(ms){now+=ms;for(const[id,t]of [...timers])if(t.at<=now){timers.delete(id);t.fn();}}};",
    ].join('\n'))();
    const replies=[{item:{kind:'episode'},ancestors:[{id:'show'},{id:'s1'}]},
      {children:[{id:'e1',kind:'episode'}]},
      {children:[{id:'s1',kind:'season'},{id:'s2',kind:'season'}]},
      {children:[{id:'e2',kind:'episode',title:'Next'}]}];
    const opening=h.start();
    for(let i=0;i<delayedBoundary;i++){
      if(i===0)h.advance(19_000);
      h.pending[i].resolve({status:200,ok:true,json:()=>Promise.resolve(replies[i])});
      for(let n=0;n<5;n++)await flush();
    }
    const held=h.pending[delayedBoundary];let releaseBody;
    if(boundary==='body'){
      held.resolve({status:200,ok:true,json:()=>new Promise(resolve=>releaseBody=resolve)});
      for(let n=0;n<5;n++)await flush();
    }
    h.advance(delayedBoundary?1000:20_000);await opening;
    assert.equal(held.options.signal.aborted,true,`next episode lookup ${delayedBoundary} ${boundary} shares the original20s limit`);
    // Raised, then retired by the intent it belongs to settling — "Up next…"
    // is a pending destination, so nothing about the picture may clear it.
    assert.deepEqual(h.surface().map((entry) => entry.title), ['Up next…'],
      'a nonresponsive autoplay lookup cannot leave Up next visible forever');
    assert.deepEqual(h.events().map((event) => Object.keys(event)[0]), ['intent_settled']);
    assert.deepEqual(h.result(),{AUTOPLAY:null,closed:1});
    assert.equal(h.notices.length,1);
    if(boundary==='body')releaseBody(replies[delayedBoundary]);
    else held.resolve({status:200,ok:true,json:()=>Promise.resolve(replies[delayedBoundary])});
    for(let n=0;n<5;n++)await flush();
    assert.equal(h.location.hash,'','a late timed-out lookup cannot navigate');
    assert.equal(h.pending.length,delayedBoundary+1,'a late timed-out lookup cannot start the next lookup');
  }
  for(const failureAt of ['headers','body','segment']){
    const h=new Function([
      "let PLAYER={method:'transcode',probeUrl:'/index.m3u8',started:false},timer;const requests=[],loading=[];const TOKEN=null;",
      surfaceSeam(),
      "const document={getElementById:()=>({classList:{add(){}},currentTime:0})};const console={warn(){}};",
      "const performance={now:()=>0};function hlsStartupIncomplete(){return false;}function diagnoseHlsStartup(){return {};}function abortHlsStartupLoaders(){}",
      "function setTimeout(fn){timer=fn;return 1;}function clearTimeout(){}function fetch(url){return new Promise(resolve=>requests.push({url,resolve}));}",
      "function pbPosSec(){return 0;}function currentStreamFailureOverlay(){return null;}function notifyPlaybackControl(){}function finishStallRecovery(){}function clientLog(){}function toast(){}function setLoading(...args){loading.push(args);}",
      shippedSource("probePlaybackSource"),shippedSource("stallDiagnose"),
      shippedSource("hasPendingPlaybackOpen"),shippedSource("playbackOwnsAttachedMedia"),
      "return {requests,loading,surface:surfacePainted,stops:()=>surfaceStops.length,start:stallDiagnose,timeout:()=>timer(),replace(){PLAYER={started:false,method:'remux'};}};",
    ].join("\n"))();
    const probe=h.start();
    if(failureAt!=='headers'){
      h.requests[0].resolve({status:200,text:()=>failureAt==='body'?new Promise(()=>{}):Promise.resolve('segment.m4s')});
      await flush();await flush();
    }
    if(failureAt==='segment')assert.equal(h.requests.length,2);
    h.timeout();await probe;
    assert.equal(h.surface().length,1);
    assert.equal(h.surface()[0].title,'The stream probe timed out.',`${failureAt} is bounded by the same probe deadline`);
    // A diagnosed stall is the owner out of rungs: it stops the player before
    // the verdict goes over it (contract §3.4).
    assert.equal(h.stops(),1,`${failureAt} stops the player before it raises`);
    assert.equal(h.surface()[0].player_stopped,true);
    assert.deepEqual(h.surface()[0].actions,['retry','close'],
      'Force transcode is not offered on a session that already is one');
  }
  {
    const h=new Function([
      "let PLAYER={method:'remux',probeUrl:'/stream.mp4',started:false};let requests=0;const loading=[];const TOKEN=null;",
      surfaceSeam(),
      "let STREAM_FAILURE={code:'session_failed',status:502,at:Date.now()};const PlaybackPolicy={streamFailureOverlay:()=>({title:'The server could not start playback.',detail:'copy output validation failed: unusable decoder configuration',retryable:false})};",
      "const document={getElementById:()=>({classList:{add(){}},currentTime:0})};const console={warn(){}};function fetch(){requests++;throw Error('terminal refusal must suppress generic probe');}",
      "const performance={now:()=>0};function hlsStartupIncomplete(){return false;}function diagnoseHlsStartup(){return {};}function abortHlsStartupLoaders(){}",
      "function pbPosSec(){return 0;}function notifyPlaybackControl(){}function finishStallRecovery(){}function clientLog(){}function toast(){}function setLoading(...args){loading.push(args);}",
      shippedSource("currentStreamFailureOverlay"),shippedSource("probePlaybackSource"),shippedSource("stallDiagnose"),
      shippedSource("hasPendingPlaybackOpen"),shippedSource("playbackOwnsAttachedMedia"),
      "return {start:stallDiagnose,result:()=>({requests,loading,surface:surfacePainted(),stops:surfaceStops.length})};",
    ].join("\n"))();
    await h.start();
    assert.equal(h.result().requests,0,'a typed terminal refusal remains the authoritative diagnosis');
    assert.equal(h.result().surface[0].title,'The server could not start playback.');
    assert.match(h.result().surface[0].detail,/unusable decoder configuration/);
    assert.equal(h.result().stops,1);
    assert.deepEqual(h.result().surface[0].actions,['retry','force_transcode','close']);
  }
  for(const replacement of ['title','close','seek']){
    const h=new Function([
      "let PLAYER={method:'remux',probeUrl:'/stream.mp4',started:false},resolve;const loading=[];const TOKEN=null;",
      surfaceSeam(),
      "const document={getElementById:()=>({})};const console={warn(){}};function fetch(){return new Promise(done=>resolve=done);}",
      "const performance={now:()=>0};function hlsStartupIncomplete(){return false;}function diagnoseHlsStartup(){return {};}function abortHlsStartupLoaders(){}",
      "function pbPosSec(){return 0;}function currentStreamFailureOverlay(){return null;}function notifyPlaybackControl(){}function finishStallRecovery(){throw Error('stale recovery');}function clientLog(){}function toast(){}function setLoading(...args){loading.push(args);}",
      shippedSource("probePlaybackSource"),shippedSource("stallDiagnose"),
      shippedSource("hasPendingPlaybackOpen"),shippedSource("playbackOwnsAttachedMedia"),
      "return {loading,surface:surfacePainted,start:stallDiagnose,resolve:()=>resolve({status:500}),replace(kind){if(kind==='close')PLAYER=null;else if(kind==='seek')PLAYER.controlIntentGeneration=1;else PLAYER={started:false,method:'transcode'};}};",
    ].join("\n"))();
    const probe=h.start();h.replace(replacement);h.resolve();await probe;
    assert.equal(h.surface().length,0,`stale probe after ${replacement} cannot label another playback failed`);
  }
  for(const outcome of ['success','failure']){
    const h=new Function([
      "let PLAYER={method:'transcode',started:true,sessionId:'A',mediaAttachment:{},abr:{switching:false}},resolve,reject;const health={recent_speed:2},SERVER={playback_auto_abr:true};let updated=0;",
      "const document={getElementById:()=>({paused:false})};function api(){return new Promise((yes,no)=>{resolve=yes;reject=no;});}function updateStats(){updated++;}function qualityForce(){return 'auto';}",
      shippedSource('hasPendingPlaybackOpen'),shippedSource('playbackOwnsAttachedMedia'),shippedSource('pollSessionHealth'),shippedSource('autoControllerTick'),
      "return {start:autoControllerTick,replace(){PLAYER.sessionId='B';PLAYER.mediaAttachment={};PLAYER.health=health;},finish(ok){if(ok)resolve({recent_speed:0});else reject(Error('old session gone'));},result:()=>({same:PLAYER.health===health,updated,final:health.final})};",
    ].join('\n'))();
    const poll=h.start();h.replace();h.finish(outcome==='success');await poll;
    assert.deepEqual(h.result(),{same:true,updated:0,final:undefined},`old health ${outcome} cannot mutate or trigger ABR on a new attachment`);
  }
  {
    const h=new Function([
      "let PLAYER={segSrc:'/A.m3u8',mediaAttachment:{},segTimes:[2]},resolve;function fetch(){return new Promise(done=>resolve=done);}",
      shippedSource('parseSegTimes'),shippedSource('refreshSegTimes'),
      shippedSource('hasPendingPlaybackOpen'),shippedSource('playbackOwnsAttachedMedia'),
      "return {p:PLAYER,start:refreshSegTimes,finish(){resolve({ok:true,text:async()=> '#EXTINF:9,\\nold.ts'});}};",
    ].join('\n'))();
    const poll=h.start();h.p.segSrc='/B.m3u8';h.p.mediaAttachment={};h.p.segTimes=[1];h.finish();await poll;
    assert.deepEqual(h.p.segTimes,[1],'old native playlist cannot install segment timing on the new attachment');
  }
  for(const laterCommand of ['seek','audio','subtitle']){
    const h=fullOpenHarness(), before=fullPlayer(); h.attach(before);
    h.setQuality('720');
    assert.equal(h.decisions.length,1);
    if(laterCommand==='seek') await h.seekTo(90);
    if(laterCommand==='audio') await h.switchAudio(1);
    if(laterCommand==='subtitle') await h.setSub(2);
    assert.equal(h.decisions.length,2,'a newer command creates a coherent replacement snapshot');
    resolveDecision(h.decisions[0]); await flush();
    assert.equal(h.current(),before,'the old decision must not replace the newer intent');
    resolveDecision(h.decisions[1]); await flush(); await flush();
    assert.notEqual(h.current(),before);
    assert.equal(h.current().controlSeek.targetMs,laterCommand==='seek'?90_000:10_000);
    if(laterCommand==='audio') assert.equal(h.current().preplay.audio,1);
    if(laterCommand==='subtitle') assert.equal(h.current().preplay.subtitle,2);
  }
  {
    const h=fullOpenHarness();h.attach(fullPlayer());
    await h.setSync(250); resolveDecision(h.decisions[0]); await flush(); await flush();
    assert.equal(h.current().aoffset,250);
    assert.equal(h.current().method,'remux','a direct-play verdict must not discard the requested audio correction');
    assert.match(h.video.src,/^\/remux/);
  }
  for(const pauseTiming of ['before-open','during-decision']){
    const h=fullOpenHarness(), p=fullPlayer(); h.attach(p);
    if(pauseTiming==='before-open'){p.wantsPlayback=false;h.video.paused=true;}
    h.setQuality('720');
    if(pauseTiming==='during-decision')h.togglePlay();
    resolveDecision(h.decisions[0]);await flush();await flush();
    assert.equal(h.current().wantsPlayback,false,pauseTiming);
    assert.equal(h.video.paused,true,pauseTiming);
    assert.equal(h.media.includes('play'),false,'a stream replacement cannot override Pause');
  }
  {
    const h=fullOpenHarness();h.attach(fullPlayer());h.setQuality('720');
    resolveDecision(h.decisions[0]);await flush();await flush();
    h.current().controlSeek={sequence:2,targetMs:90_000,executed:true};
    h.video.onloadedmetadata();
    assert.equal(h.video.currentTime,90,'late direct metadata cannot restore an older quality-change position');
  }
  {
    const h=fullOpenHarness(),p=fullPlayer();h.attach(p);p.wantsPlayback=false;h.video.paused=true;h.setOpen(false);
    const opening=h.play('film','Film',0,600_000,null);
    resolveDecision(h.decisions[0]);await opening;
    assert.equal(h.current().wantsPlayback,true,'a new click after Close starts playback even for the same file');
    assert.equal(h.video.paused,false);
  }
  {
    const h=fullOpenHarness(),p=fullPlayer();h.attach(p);
    h.resetMediaSource(h.video);
    assert.equal(p.wantsPlayback,true,'internal reset pause is not a viewer pause');
    assert.equal(p.internalMediaReset,true);
    h.applyPlaybackTransportIntent(h.video,p);
    assert.equal(h.video.paused,false);
  }
  {
    const h=fullOpenHarness(),old=fullPlayer();h.attach(old);h.resetMediaSource(h.video);
    const next=fullPlayer();next.internalMediaReset=true;h.attach(next);
    assert.equal(h.video._plurxTransportEvents.pause.length,0,'load cancels the old queued pause expectation');
    h.handlePlaybackTransportEvent(h.video,next,'pause');
    assert.equal(next.wantsPlayback,false,'native Pause during preparation remains a real command');
    assert.equal(next.controlIntentGeneration,1);
    h.video.paused=false;
    h.handlePlaybackTransportEvent(h.video,next,'play');
    assert.equal(next.wantsPlayback,true);assert.equal(next.controlIntentGeneration,2);
    h.video.paused=true;h.applyPlaybackTransportIntent(h.video,next);
    next.wantsPlayback=false;
    h.handlePlaybackTransportEvent(h.video,next,'play');
    assert.equal(next.wantsPlayback,false,'a delayed internal play event cannot overwrite a newer Pause');
  }
  {
    // Model HTML's queued media tasks: load/src remove old event tasks, while
    // pending play promise rejections can arrive after the new load has begun.
    const h=new Function([
      "let PLAYER={wantsPlayback:true},queue=[];const plays=[];",
      "const v={paused:false,ended:false,removeAttribute(){},load(){queue=[];this.paused=true;},pause(){if(!this.paused){this.paused=true;queue.push('pause');}},play(){if(this.paused){this.paused=false;queue.push('play');}return new Promise((resolve,reject)=>plays.push({resolve,reject}));}};",
      "Object.defineProperty(v,'src',{set(){v.load();}});",
      "function supersedePlaybackControlIntent(p){p.generation=(p.generation||0)+1;}function endWait(){}",
      "function cancelHlsStartup(){}function pauseHlsStartup(){}function resumeHlsStartup(){}",
      // §3.1: the viewer's own transport intent is what tells the presenter a
      // `buffering` fault is about nothing any more. Recorded, not stubbed away.
      "const surfaceEvents=[];function playbackSurfaceStep(event){surfaceEvents.push(event);return null;}",
      shippedSource("rememberPlaybackTransportIntent"),shippedSource("pausePlaybackInternally"),
      shippedSource("playbackTransportEvents"),shippedSource("resetPlaybackTransportEvents"),
      shippedSource("setPlaybackMediaSource"),shippedSource("resetMediaSource"),
      shippedSource("applyPlaybackTransportIntent"),shippedSource("handlePlaybackTransportEvent"),
      "return {p:PLAYER,v,plays,surfaceEvents,reset:()=>resetMediaSource(v),source:()=>setPlaybackMediaSource(v,'new'),apply:()=>applyPlaybackTransportIntent(v,PLAYER),flush(){while(queue.length)handlePlaybackTransportEvent(v,PLAYER,queue.shift());}};",
    ].join("\n"))();
    h.reset();h.apply();h.flush();
    h.v.pause();h.flush();
    assert.equal(h.p.wantsPlayback,false,'load-canceled internal pause cannot consume the next native Pause');
    h.p.wantsPlayback=true;h.apply();const old=h.plays.at(-1);
    h.source();h.apply();
    assert.equal(h.v._plurxTransportEvents.play.length,1);
    old.reject(new Error('old load aborted'));await flush();
    assert.equal(h.v._plurxTransportEvents.play.length,1,'old play rejection cannot consume the new load request');
    h.p.wantsPlayback=false;h.apply();h.flush();
    assert.equal(h.p.wantsPlayback,false);
    h.v.play();h.p.wantsPlayback=false;h.apply();h.flush();
    assert.equal(h.p.wantsPlayback,false,'queued native Play cannot overwrite a newer explicit Pause');
    h.p.wantsPlayback=true;h.apply();h.flush();
    h.v.pause();h.p.wantsPlayback=true;h.apply();h.flush();
    assert.equal(h.p.wantsPlayback,true,'queued native Pause cannot overwrite a newer explicit Play');
    // §3.1: only the VIEWER's transport edges reach the presenter, and each
    // carries the intent that edge established — so an owner's own
    // `pausePlaybackInternally`, whose token the filter above consumes, can
    // never retire a fault. Two viewer presses, driven straight at the element.
    const before=h.surfaceEvents.length;
    h.v.pause();h.flush();
    h.v.play();h.flush();
    const edges=h.surfaceEvents.slice(before).map(event=>event.playback_requested);
    assert.ok(edges.includes(false),'a viewer pause tells the presenter media is not wanted');
    assert.ok(edges.includes(true),'and a viewer play tells it the opposite');
    for(const event of h.surfaceEvents)
      assert.deepEqual(Object.keys(event),['playback_requested'],
        'the transport path tells the presenter one fact and nothing else');
    for(const play of h.plays)play.resolve();
  }
  {
    const h=new Function([
      "let now=1000,attempts=0,recovered=0; const performance={now:()=>now},document={hidden:false};",
      surfaceSeam(),
      "const PERSISTENT_STALL_MS=8000,STALL_MIN_MS=350,SUPPLY_RUNWAY_SECS=1.5; const p={started:true,waitAt:null,controlHasFrameCallbacks:true,controlPresentedFrames:10,source:{video_codec:'h264'},wantsPlayback:true}; let PLAYER=p;",
      "const v={currentTime:10,paused:false,ended:false,pause(){this.paused=true;}};",
      "function bufferRunway(){return 10;} function persistentWait(){attempts++;return new Promise(()=>{});}",
      "function clearStall(){} function finishStallRecovery(){recovered++;} function setLoading(){} function recordWaitStall(){}",
      "function playerActivity(){} function settlePlaybackControlSeek(){} function completeHlsStartup(){} function hlsStartupIncomplete(){return false;} function pbTick(){} function pbSyncPlayIcon(){} function notifyPlaybackControl(){} function reportTtff(){}",
      shippedSource("streamHasVideo"),shippedSource("persistentWaitEvidence"),shippedSource("endWait"),
      shippedSource("samplePlaybackPresentationClock"),shippedSource("pausePlaybackInternally"),
      shippedSource("playbackTransportEvents"),
      shippedSource("playbackProgressTick"),shippedSource("playbackWaitNeedsProgress"),shippedSource("handlePlaybackPlaying"),
      shippedSource("hasPendingPlaybackOpen"),shippedSource("playbackOwnsAttachedMedia"),
      "return {p,v,tick(time){now=time;playbackProgressTick(v,p);},playing(){handlePlaybackPlaying(v,p);},attempts:()=>attempts,recovered:()=>recovered};",
    ].join("\n"))();
    h.tick(1000);h.tick(9000);
    assert.equal(h.attempts(),1);const began=h.p.waitAt;
    h.playing();h.tick(20000);
    assert.equal(h.p.waitAt,began,'playing without frame/clock movement cannot cancel the recovery owner');
    assert.equal(h.recovered(),0,'an event alone must not report recovered');
    h.v.currentTime=11;h.p.controlPresentedFrames=11;h.tick(20500);
    assert.equal(h.p.waitAt,null);assert.equal(h.recovered(),1,'actual output progress retires the wait');
    h.p.progressWatch=null;h.p.waitAt=null;h.p.recoveringStall={at:20500,action:'restart'};
    h.v.currentTime=20;h.tick(21000);h.playing();
    assert.equal(h.recovered(),1,'playing without presentation progress cannot complete a replacement');
    h.v.currentTime=21;h.p.controlPresentedFrames=12;h.tick(21500);
    assert.equal(h.recovered(),2,'new presentation progress completes the replacement episode');
    h.p.progressWatch=null;h.p.recoveringStall={at:21500,action:'restart'};
    h.p.controlHasFrameCallbacks=false;delete h.v.getVideoPlaybackQuality;
    h.v.currentTime=30;h.tick(22000);h.playing();h.v.currentTime=31;h.tick(22500);
    assert.equal(h.recovered(),3,
      'the explicit clock fallback completes recovery when frame counters are unavailable');
    h.p.progressWatch=null;h.p.recoveringStall={at:22500,action:'restart'};
    h.p.controlHasFrameCallbacks=true;h.p.controlPresentedFrames=0;
    h.v.getVideoPlaybackQuality=()=>({totalVideoFrames:500});
    h.v.currentTime=40;h.tick(23000);h.v.currentTime=41;h.tick(23500);
    assert.equal(h.recovered(),3,
      'decoded-ahead frames cannot settle recovery while presentation callbacks stay at zero');
    h.p.controlPresentedFrames=1;h.v.currentTime=42;h.tick(24000);
    assert.equal(h.recovered(),4,'the supported presentation callback settles recovery');
  }
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
  const endedToggle=new Function("document","replayEnded","playerActivity",
    "supersedePlaybackControlIntent","notifyPlaybackControl",[
    "let PLAYER={}; function endWait(){}",shippedSource("togglePlay"),"togglePlay();",
  ].join("\n"))(
    {getElementById:()=>({ended:true,paused:true,play:()=>{elementPlays+=1;},pause:()=>{}})},
    ()=>{replayClicks+=1;},()=>{activities+=1;},()=>{},()=>{},
  );
  assert.equal(endedToggle,undefined);
  assert.equal(replayClicks,1);
  assert.equal(elementPlays,0,"ended media is never restarted behind its stopped controller");
  assert.equal(activities,1);

  assert.match(shippedSource("wirePlayer"),/visibilitychange[^\n]*notifyPlaybackControl/);
  // The media listener set is keyed to the ELEMENT, not to the page, because a
  // prepared handoff replaces which element owns the picture. A set bound once
  // per page would leave the scrubber, the stall watchdog and the decode rescue
  // listening to the retired node for the rest of the session.
  assert.match(shippedSource("wirePlayer"),/wirePlayerMedia\(v\)/);
  assert.match(shippedSource("wirePlayerMedia"),/v\._plurxMediaWired/);
  assert.match(shippedSource("adoptPlaybackMediaElement"),/wirePlayerMedia\(v\)/);
  for(const binding of ["onended","armHitchDetector","setupAirplay","progressTimer"]){
    assert.match(shippedSource("adoptPlaybackMediaElement"),new RegExp(binding),
      `a switch must re-bind ${binding} to the element that now owns the picture`);
  }
  assert.match(shippedSource("persistentWaitEvidence"),/error_code:"decoder"/);
  assert.doesNotMatch(shippedSource("persistentWait"),/persistent_decode_stall/,
    "elapsed time must never be serialized as a decoder failure");
  assert.match(shippedSource("persistentWait"),/askPlaybackControl\("stalled",controlObservation,began\+CONTROL_STALL_DEFER_DEADLINE_MS\)/);
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
    capture: () => captureSnapshot(snapshot()),
    send: async (_url, request) => { declared = request.supported_actions; return response(request); },
  }).start();
  await flush();
  assert.deepEqual(
    declared,
    ["hold", "retry_resource", "terminal", "prepare_replacement"],
    "the request declares the actions this client accepts",
  );
  // Asserted on the SERIALIZED body, not the in-memory list: the serializer is
  // what ships, and `prepare_replacement` is a promise made to the server in
  // JSON. Declaring it is the whole of Gate B (§C2) — the server stages a
  // successor and then says nothing about it to a client that never named it.
  assert.deepEqual(
    JSON.parse(JSON.stringify({ supported_actions: declared })).supported_actions,
    ["hold", "retry_resource", "terminal", "prepare_replacement"],
    "the serialized request carries the declared vocabulary verbatim",
  );
  declaring.stop();

  // ---- the prepared replacement action ------------------------------------
  //
  // A literal-JSON fixture, asserted key by key, because nothing in the tree
  // pinned the payload before this: the server asserts `["action"]["type"]`
  // and eight sites read `action_id`, and `playlist_url`, `media_origin_ms`
  // and `effective_selection` are asserted nowhere. This fixture is what
  // catches the `prepare` / `prepare_replacement` trap in both directions.
  const PREPARE_SESSION = "44444444-4444-4444-8444-444444444444";
  const PREPARE_ACTION_ID = "33333333-3333-4333-8333-333333333333";
  const PREPARE_FIXTURE = JSON.parse(`{
    "type":"prepare",
    "action_id":"${PREPARE_ACTION_ID}",
    "session_id":"${PREPARE_SESSION}",
    "playlist_url":"/api/v1/hls/${PREPARE_SESSION}/index.m3u8",
    "media_origin_ms":0,
    "effective_selection":{"quality_auto":true,"height":1080,
      "audio_track":null,"subtitle_burn":null,"audio_offset_ms":0,
      "codec":"server_selected","dynamic_range":"sdr"}}`);
  assert.equal(PREPARE_FIXTURE.type, "prepare", "the wire tag is `prepare`");
  assert.equal(control.PREPARE_REPLACEMENT_ACTION, "prepare_replacement",
    "the DECLARED name is `prepare_replacement` — the two differ, and only here");
  assert.equal(control.PREPARE_ACTION_TAG, "prepare");
  assert.ok(control.SUPPORTED_ACTIONS.includes(control.PREPARE_REPLACEMENT_ACTION));
  assert.equal(control.validPreparation(PREPARE_FIXTURE), true,
    "the shipped validator accepts the contract's own fixture");
  assert.equal(PREPARE_FIXTURE.effective_selection.codec, "server_selected");
  assert.equal(PREPARE_FIXTURE.media_origin_ms, 0);
  assert.equal(PREPARE_FIXTURE.playlist_url,
    `/api/v1/hls/${PREPARE_SESSION}/index.m3u8`);
  {
    // A whole exchange, so the fixture is proven through `validResponse` and
    // not only through the validator it happens to call.
    const seen = [];
    const preparing = new control.Reporter({
      bootstrap: bootstrap(),
      clientInstanceId: "88888888-8888-4888-8888-888888888888",
      capture: () => captureSnapshot(snapshot()),
      send: async (_url, request) =>
        Object.assign(response(request), { action: PREPARE_FIXTURE }),
      onExchange: ({ response: answered, error }) => seen.push({ answered, error }),
      setTimer: () => 0,
      clearTimer: () => {},
    }).start();
    await flush();
    assert.equal(seen.length, 1);
    assert.equal(seen[0].error, null, "a valid preparation is not a protocol error");
    assert.equal(seen[0].answered.action.action_id, PREPARE_ACTION_ID);
    preparing.stop();
  }

  // The trap, asserted rather than commented: the DECLARED name arriving as a
  // wire tag is not an action, and an unknown type stays fatal. A client that
  // switched on `prepare_replacement` here would simply never fire.
  assert.equal(control.validPreparation(
    Object.assign({}, PREPARE_FIXTURE, { type: "prepare_replacement" })), false);

  // §C12.4 — a playlist that is not node-relative is refused by the client,
  // and refused as a protocol error rather than followed. No request is made:
  // `validPreparation` is a pure predicate over the parsed action, and the
  // only network this client performs for a preparation is the one the player
  // makes AFTER this passes.
  for (const [label, url] of [
    ["absolute", `https://attacker.example/api/v1/hls/${PREPARE_SESSION}/index.m3u8`],
    ["protocol-relative", `//attacker.example/api/v1/hls/${PREPARE_SESSION}/index.m3u8`],
    ["another session", "/api/v1/hls/55555555-5555-4555-8555-555555555555/index.m3u8"],
    ["traversal", `/api/v1/hls/${PREPARE_SESSION}/../../../etc/index.m3u8`],
    ["not a playlist", `/api/v1/hls/${PREPARE_SESSION}/segment.ts`],
  ]) {
    assert.equal(
      control.validPreparation(Object.assign({}, PREPARE_FIXTURE, { playlist_url: url })),
      false,
      `a ${label} playlist_url is refused`,
    );
    assert.equal(control.preparedPlaylistUrl(PREPARE_SESSION, url), null);
  }
  assert.equal(
    control.preparedPlaylistUrl(PREPARE_SESSION, `/api/v1/hls/${PREPARE_SESSION}/master.m3u8`),
    `/api/v1/hls/${PREPARE_SESSION}/master.m3u8`,
    "a master playlist is the other legal shape",
  );
  assert.equal(
    control.preparedPlaylistUrl(PREPARE_SESSION, `/api/v1/hls/${PREPARE_SESSION}/index.m3u8?x=1`),
    `/api/v1/hls/${PREPARE_SESSION}/index.m3u8?x=1`,
    "a query is allowed and ignored",
  );

  // Field bounds. Each of these is a 400 the server would answer, and a
  // preparation this client half-understood is worse than one it refused.
  for (const [label, patch] of [
    ["a missing action_id", { action_id: undefined }],
    ["a non-UUID action_id", { action_id: "not-a-uuid" }],
    ["a truncated action_id", { action_id: "3333-4333-8333-333333333333" }],
    ["an empty session_id", { session_id: "" }],
    ["a negative media_origin_ms", { media_origin_ms: -1 }],
    ["a fractional media_origin_ms", { media_origin_ms: 1.5 }],
    ["a codec name in `codec`", {
      effective_selection: { ...PREPARE_FIXTURE.effective_selection, codec: "hevc" } }],
    ["an out-of-range height", {
      effective_selection: { ...PREPARE_FIXTURE.effective_selection, height: 4321 } }],
    ["an out-of-range audio offset", {
      effective_selection: { ...PREPARE_FIXTURE.effective_selection, audio_offset_ms: 15_001 } }],
    ["an unknown dynamic range", {
      effective_selection: { ...PREPARE_FIXTURE.effective_selection, dynamic_range: "hdr12" } }],
    ["no effective_selection", { effective_selection: undefined }],
  ]) {
    assert.equal(control.validPreparation(Object.assign({}, PREPARE_FIXTURE, patch)), false,
      `${label} is refused`);
  }
  // …and a key this build has never heard of is tolerated: `ControlAction`
  // carries no `deny_unknown_fields`, so a later server may add one.
  assert.equal(control.validPreparation(
    Object.assign({}, PREPARE_FIXTURE, { a_field_from_next_year: 7 })), true);
  // …and so is an `action_id` from a later UUID version. The server accepts
  // whatever `uuid::Uuid::parse_str` takes and only mints v4 today; refusing a
  // v7 here would turn a legitimate staging into a protocol error that stops
  // this reporter for the rest of the session.
  assert.equal(control.validPreparation(Object.assign({}, PREPARE_FIXTURE,
    { action_id: "01930000-0000-7000-a000-000000000000" })), true,
    "a v7 action_id is a staging, not a protocol error");
  assert.equal(control.validPreparation(Object.assign({}, PREPARE_FIXTURE, {
    effective_selection: { ...PREPARE_FIXTURE.effective_selection, later_key: 1 } })), true);

  // §C12.3 — the new vocabulary did not weaken the old rule.
  {
    let stopped = null;
    const unknown = new control.Reporter({
      bootstrap: bootstrap(),
      clientInstanceId: "99999999-9999-4999-8999-999999999999",
      capture: () => captureSnapshot(snapshot()),
      send: async (_url, request) =>
        Object.assign(response(request), { action: { type: "commit_replacement" } }),
      onExchange: ({ error }) => { stopped = error; },
      setTimer: () => 0,
      clearTimer: () => {},
    }).start();
    await flush();
    assert.equal(stopped && stopped.name, "PlaybackControlProtocolError",
      "an action outside the vocabulary is still fatal");
    assert.equal(unknown.stopped, true);
  }

  // ---- acknowledgements ---------------------------------------------------
  //
  // §C7's rules, and the one that is a client design rule: a `committed` may
  // not share an exchange with `demand: "end"`.
  const ack = (state, extra = {}) =>
    Object.assign({ action_id: PREPARE_ACTION_ID, state }, extra);
  assert.equal(control.validAcknowledgement(ack("metadata_ready"), "active"), true);
  assert.equal(control.validAcknowledgement(
    ack("buffer_ready", { buffered_through_ms: 42_000 }), "active"), true);
  assert.equal(control.validAcknowledgement(ack("buffer_ready"), "active"), false,
    "buffer_ready without buffered_through_ms is a 400");
  // A commit owes BOTH the timestamp and the origin it was offered. The origin
  // is the field a client gets wrong by omission — the state machine reads as
  // though a commit owes only a timestamp — and omitting it is a
  // `400 acknowledgement.committed_media_origin_ms` that stops the reporter for
  // the rest of the session. `playback_control.rs` validates it twice: once in
  // `ActionAcknowledgement::validate`, and again in
  // `bound_preparation_acknowledgement`, which SILENTLY drops a commit naming a
  // different origin than the offer.
  const commit = (extra = {}) => ack("committed", Object.assign({
    first_frame_unix_ms: 1_757_000_000_000,
    committed_media_origin_ms: 600_000,
  }, extra));
  assert.equal(control.validAcknowledgement(commit(), "active"), true);
  assert.equal(control.validAcknowledgement(ack("committed"), "active"), false,
    "committed without first_frame_unix_ms is a 400");
  assert.equal(control.validAcknowledgement(
    ack("committed", { committed_media_origin_ms: 600_000 }), "active"), false,
    "committed without first_frame_unix_ms is a 400 even with the origin");
  assert.equal(control.validAcknowledgement(
    ack("committed", { first_frame_unix_ms: 1_757_000_000_000 }), "active"), false,
    "committed without committed_media_origin_ms is a 400 — the field the state "
      + "machine does not make obvious");
  assert.equal(control.validAcknowledgement(commit({ first_frame_unix_ms: 0 }), "active"), false);
  assert.equal(control.validAcknowledgement(
    commit({ committed_media_origin_ms: -1 }), "active"), false);
  assert.equal(control.validAcknowledgement(
    commit({ committed_media_origin_ms: 0 }), "active"), true,
    "an origin of zero is a value, not an omission");
  assert.equal(control.validAcknowledgement(commit(), "end"), false,
    "a commit may not ride the exchange that ends the session");
  assert.equal(control.validAcknowledgement(ack("failed"), "active"), true);
  assert.equal(control.validAcknowledgement(ack("aborted"), "end"), true,
    "an abort may ride the end — only `committed` may not");
  assert.equal(control.validAcknowledgement(ack("prepared"), "active"), false,
    "there is no sixth state");
  assert.equal(control.validAcknowledgement(
    Object.assign(ack("failed"), { action_id: "nope" }), "active"), false);

  // Round-tripped through the reporter, on the serialized body, because the
  // serializer is what ships.
  for (const [label, state, extra] of [
    ["metadata", "metadata_ready", {}],
    ["buffer", "buffer_ready", { buffered_through_ms: 42_000 }],
    ["commit", "committed", { first_frame_unix_ms: 1_757_000_000_000,
      committed_media_origin_ms: 600_000 }],
    ["failure", "failed", {}],
    ["abort", "aborted", {}],
  ]) {
    let body = null;
    const acknowledging = new control.Reporter({
      bootstrap: bootstrap(),
      clientInstanceId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      capture: () => captureSnapshot(
        Object.assign(snapshot(), { acknowledgement: ack(state, extra) })),
      send: async (_url, request) => {
        body = JSON.parse(JSON.stringify(request));
        return response(request);
      },
      setTimer: () => 0,
      clearTimer: () => {},
    }).start();
    await flush();
    assert.equal(body && body.acknowledgement.state, state, `${label} reaches the wire`);
    assert.equal(body.acknowledgement.action_id, PREPARE_ACTION_ID);
    for (const [key, value] of Object.entries(extra)) {
      assert.equal(body.acknowledgement[key], value, `${label} carries ${key}`);
    }
    acknowledging.stop();
  }

  // …and the combination the server answers 400 to is never CONSTRUCTED: an
  // end-demand snapshot carrying a commit is not a valid snapshot, so no
  // request is built from it and nothing is sent.
  {
    let sends = 0;
    const ending = new control.Reporter({
      bootstrap: bootstrap(),
      clientInstanceId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
      capture: () => captureSnapshot(Object.assign(snapshot(), {
        demand: "end",
        acknowledgement: commit(),
      })),
      send: async (_url, request) => { sends += 1; return response(request); },
      setTimer: () => 0,
      clearTimer: () => {},
    }).start();
    await flush();
    assert.equal(sends, 0, "a commit on an ending exchange is never sent");
    ending.stop();
  }

  // A hold is not a failure and not a reason to stop. This is the whole point:
  // the server is saying production is deliberately not advancing, and a client
  // that treated that as a protocol error would lose reporting for the rest of
  // the film exactly when the server had just explained itself.
  let holdExchanges = 0;
  let holdErrors = 0;
  const holding = new control.Reporter({
    bootstrap: bootstrap(),
    clientInstanceId: "77777777-7777-4777-8777-777777777777",
    capture: () => captureSnapshot(snapshot()),
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
    capture: () => captureSnapshot(snapshot()),
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
    capture: () => captureSnapshot(snapshot()),
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
    capture: () => captureSnapshot(snapshot()),
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
    capture: () => captureSnapshot(snapshot()),
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
      capture: () => captureSnapshot(snapshot()),
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
    "CONTROL_DEFER_LIMIT", "CONTROL_STALL_DEFER_DEADLINE_MS", "SUPPLY_RUNWAY_SECS",
    "STALL_MIN_MS"].map((name) => {
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
      "startTranscodeFallback", "seekTo", "playbackContext",
      "clockFromSec", "CONTROL_CLIENT_ID", "playbackControlSnapshot",
      "sendPlaybackControl", "window", "PlurxPlaybackControl", "performance", "bufferRunway",
      [
        "let PLAYER=null; const document={hidden:false,getElementById:()=>null};",
        surfaceSeam(),
        shippedSource("createPlaybackOpenGate"),"const PLAY_OPEN_GATE=createPlaybackOpenGate();",
        askConstants,
        shippedSource("supersedePlaybackControlIntent"),
        shippedSource("holdReasonText"),
        shippedSource("controlVerdictText"),
        shippedSource("armedPlaybackControlVerdict"),
        shippedSource("playbackControlObservationOverride"),
        shippedSource("notifyPlaybackControl"),
        // The reporter settles a carried acknowledgement on every exchange and
        // frees a staged successor when it stops, so both paths are real here
        // rather than stubbed.
        shippedConst("PREPARED_TERMINAL_STATES"), shippedConst("PREPARED_ACK_QUEUE_MAX"),
        shippedConst("PREPARED_SETTLED_MEMORY"), shippedConst("PREPARED_FINALIZATION_MS"),
        shippedSource("preparedVideoElement"), shippedSource("preparedState"),
        shippedSource("preparedSettlementDone"), shippedSource("markPreparedSettlement"),
        shippedSource("queuePlaybackControlAcknowledgement"),
        shippedSource("pendingPlaybackControlAcknowledgement"),
        shippedSource("settlePlaybackControlAcknowledgement"),
        shippedSource("flushPreparedSettlement"), shippedSource("cancelPreparedFirstFrame"),
        shippedSource("endingPlaybackControlSnapshot"),
        shippedSource("finishStoppingPlaybackControl"),
        shippedSource("continueStoppingPlaybackControl"),
        shippedSource("destroyHlsInstance"),
        shippedSource("freePreparedReplacement"), shippedSource("abandonPreparedReplacement"),
        // The dispatch under test: a `prepare` answer must reach the player's
        // state machine. Recorded rather than run — the pipeline itself has its
        // own harness — because what this scope proves is that the SHIPPED
        // reporter callback calls it at all, and calls it on the wire tag.
        "const preparedSeen=[];function handlePreparedReplacementAction(p,action){preparedSeen.push(action);return null;}",
        shippedSource("askPlaybackControl"),
        shippedSource("settlePlaybackControlWaiters"),
        shippedSource("clearPlaybackControlWaiters"),
        shippedSource("flushPreparedSettlement"), shippedSource("cancelPreparedFirstFrame"),
        shippedSource("stopPlaybackControl"),
        shippedSource("startPlaybackControl"),
        shippedSource("persistentWaitEvidence"),
        shippedSource("endWait"),
        shippedSource("stallRecoverySnapshot"),
        shippedSource("persistentWait"),
        "function settlePlaybackControlSeek(){} function completeHlsStartup(){} function clearStall(){} function finishStallRecovery(){return false;}",
        shippedSource("streamHasVideo"),
        shippedSource("samplePlaybackPresentationClock"),
        shippedSource("playbackProgressTick"),
        shippedSource("hasPendingPlaybackOpen"),shippedSource("playbackOwnsAttachedMedia"),
        "return {",
        " surface:surfacePainted, stops:()=>surfaceStops.length, events:()=>surfaceEvents,",
        " preparation(player,kind){PLAYER=player;if(kind==='full')player.retiringOpenAttempt=PLAY_OPEN_GATE.begin('open');else player.pendingMediaChange={};},",
        " attachedAgain(player){PLAYER=player;player.pendingMediaChange=null;player.retiringOpenAttempt=null;return notifyPlaybackControl();},",
        " monitor(player,video){PLAYER=player; playbackProgressTick(video,player);},",
        " hidden(value){document.hidden=value;},",
        " attach(player,video,bootstrap){PLAYER=player;",
        "   return startPlaybackControl(video,player,bootstrap);},",
        " detach(player){PLAYER=player; stopPlaybackControl(player); PLAYER=null;},",
        " stall(player,video,began,generation){PLAYER=player;",
        "   return persistentWait(video,player,began,generation,player.controlIntentGeneration||0);},",
        " verdictText:controlVerdictText,",
        " supersede(player){PLAYER=player; return supersedePlaybackControlIntent(player);},",
        " armedVerdict(player){PLAYER=player; return armedPlaybackControlVerdict(player);},",
        " askProbe(player){PLAYER=player;",
        "  try{ const a=askPlaybackControl('stalled',{decoder_state:'starved'});",
        "   return {trigger:a.trigger, w:(player.controlWaiters||[]).length}; }",
        "  catch(e){ return {err:String(e&&e.message||e)}; }}, ",
        " prepared(){return preparedSeen;},",
        " probe(player,video){PLAYER=player; const r=player.controlReporter;",
        "  return {hasReporter:!!r, stopped:r&&r.stopped, seq:r&&r.sequence,",
        "   snap:!!(r&&r.capture()), notify:r&&r.notify()};}};",
      ].join("\n"),
    )(
      (fn, ms) => { const id = nextTimer++; timers.set(id, { fn, ms }); return id; },
      (id) => { timers.delete(id); },
      8_000,
      options.policy || { stallRecoveryAction: () => "reconnect", stallRecoveryTargetHeight: () => 720 },
      () => "auto",
      (player, kind, ms, runway, detail) => stalls.push(detail),
      () => 12,
      (entry) => log.push(entry),
      (why) => reopened.push({ kind: "transcode", why }),
      (position) => reopened.push({ kind: "seek", position }),
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
      options.clock||performance,
      () => options.runway??1,
    );
    const made = {
      stub, timers, log, reopened, stalls, sent,
      // The overlay copy, read off the faults the owner raised. `buttons` was
      // a string of inline HTML; `actions` is the contract's vocabulary, which
      // is the same question asked in the form the presenter answers.
      get loading() { return stub.surface(); },
      get stops() { return stub.stops(); },
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
  {
    const classify = new Function(
      "SUPPLY_RUNWAY_SECS",
      `${shippedSource("persistentWaitEvidence")}\nreturn persistentWaitEvidence;`,
    )(1.5);
    assert.deepEqual(classify({ error: null }, 18.310), {
      kind: "presentation", cause: "unknown",
      observation: { decoder_state: "unknown" },
    }, "loaded runway plus no progress remains an unattributed presentation wait");
    assert.deepEqual(classify({ error: { code: 2, message: "fetch failed" } }, 18.310), {
      kind: "supply", cause: "network",
      observation: {
        decoder_state: "unknown", error_code: "network", error_detail: "fetch failed",
      },
    }, "a typed media network error remains independent adaptation evidence");
    assert.deepEqual(classify({ error: { code: 3, message: "decode failed" } }, 18.310), {
      kind: "decode", cause: "decoder",
      observation: {
        decoder_state: "failed", error_code: "decoder", error_detail: "decode failed",
      },
    }, "a typed media decoder error still qualifies decoder attribution");
    assert.deepEqual(classify({ error: null }, 1), {
      kind: "supply", cause: "unknown",
      observation: { decoder_state: "starved" },
    }, "an empty buffer identifies supply pressure without inventing a network cause");
  }
  {
    let now=20_001;
    const policyInputs=[];
    const h=stallHarness({clock:{now:()=>now},policy:{
      stallRecoveryAction(input){policyInputs.push(input);return 'restart';},
      stallRecoveryTargetHeight(){return null;},
    }});
    const player=Object.assign(stalledPlayer(),{
      waitAt:1,waitRunway:18.310,controlIntentGeneration:7,
      controlPresentationEpoch:11,sessionId:'session-original',copyHls:true,
      curAudio:1,audio:[{index:0},{index:4}],curSub:9,burnedSub:null,aoffset:125,
      source:{video_codec:'hevc',video_profile:'Main 10',width:3840,height:2160,
        hdr_format:'Dolby Vision Profile 8'},
    });
    const video=Object.assign({},stalledVideo,{error:null,videoHeight:2160});
    h.stub.attach(player,video,bootstrap());h.attached.push(player);await flush();
    await h.stub.stall(player,video,1,3);
    assert.equal(policyInputs.length,1);
    assert.equal(policyInputs[0].cause,'unknown');
    assert.deepEqual(h.reopened,[{kind:'seek',position:12}],
      'the 18.310s loaded wait performs one same-recipe repair, never a transcode');
    assert.equal(player.recoveringStall.kind,'presentation');
    assert.deepEqual(player.recoveringStall.recipe,{
      method:'remux',copy_hls:true,quality:'auto',height:2160,
      video_codec:'hevc',video_profile:'Main 10',width:3840,dynamic_range:'Dolby Vision Profile 8',
      audio_track:4,subtitle_track:9,subtitle_mode:'native',audio_offset_ms:125,
    },'the episode freezes the selected quality and tracks before mutation');
    assert.equal(player.recoveringStall.sessionId,'session-original');
    assert.equal(player.recoveringStall.intentGeneration,7);
    h.stub.supersede(player);
    assert.equal(player.recoveringStall,null,
      'a newer viewer intent cancels the old recovery episode before a callback can act');
    h.stub.detach(player);
  }
  {
    const h=stallHarness(),player=stalledPlayer(),held=deferred();
    player.mediaAttachment={};player.controlIntentGeneration=0;
    h.holdWith(()=>held.promise);h.stub.attach(player,stalledVideo,bootstrap());h.attached.push(player);
    await flush();const old=h.sent[0];
    player.mediaAttachment={};player.subtitleReadinessReady=false;
    held.resolve({...response(old),action:{type:'terminal',code:'unsupported',message:'old attachment'},
      delivery:{subtitle_readiness:'ready'}});
    await settleExchange();
    assert.equal(player.controlVerdict,undefined,'actual reporter callback cannot publish across an attachment change');
    assert.equal(player.controlLastResponse,null);
    assert.equal(player.subtitleReadinessReady,false);
    h.stub.detach(player);
  }
  for(const kind of ['full','partial']){
    const h=stallHarness(),player=stalledPlayer(),held=deferred();
    h.holdWith(()=>held.promise);h.stub.attach(player,stalledVideo,bootstrap());h.attached.push(player);
    await flush();const old=h.sent[0];
    h.stub.preparation(player,kind);player.subtitleReadinessReady=false;
    assert.equal(player.controlReporter.capture(),null,'retained media cannot be snapshotted as desired replacement evidence');
    assert.equal(h.stub.askProbe(player).trigger,null,'native event notification cannot send old ended/error while preparing');
    held.resolve(Object.assign(response(old),{delivery:{subtitle_readiness:'ready'}}));
    await settleExchange();
    assert.equal(player.controlLastResponse,null,'late predecessor exchange does not update replacement observations');
    assert.equal(player.subtitleReadinessReady,false,'late readiness cannot trigger subtitle mutation across preparation');
    h.holdWith(null);h.stub.attachedAgain(player);await settleExchange();
    assert.equal(h.sent.length,2,'actual attachment explicitly restarts the suspended reporter');
    h.stub.detach(player);
  }
  {
    let now=1000;
    const h=stallHarness({clock:{now:()=>now}});
    const player=Object.assign(stalledPlayer(),{waitAt:null,wantsPlayback:true});
    const video={paused:false,seeking:false,ended:false,currentTime:10};
    h.stub.monitor(player,video);now=9000;h.stub.monitor(player,video);
    const listener=SHIPPED_UI.match(/v\.addEventListener\("seeking",[^\n]+/)[0];
    const seeking=new Function('PLAYER','performance',[
      'let callback;const v={addEventListener(_,fn){callback=fn;}};const STALL_MIN_MS=350;function clearTimeout(){}function notifyPlaybackControl(){}',
      shippedSource('endWait'),listener,'return callback;',
    ].join('\n'))(player,{now:()=>now});
    video.seeking=true;seeking();
    assert.notEqual(player.waitAt,null,'native seeking cannot abandon an independently fired deadline');
    await flush();await flush();
    assert.equal(h.reopened.length,1,'the awaited recovery retains its absolute owner across native seeking');
  }

  // Drive the shipped independent progress clock into the shipped recovery
  // owner with no waiting event and no control reporter to hide the result.
  for(const fault of ['silent-clock','silent-video','unlanded-seek','wanted-but-paused']){
    let now=1_000;
    const h=stallHarness({clock:{now:()=>now}});
    const player=Object.assign(stalledPlayer(),{waitAt:null,controlHasFrameCallbacks:true,
      controlPresentedFrames:10,source:{video_codec:'h264'}});
    const video={paused:false,seeking:false,ended:false,currentTime:10};
    if(fault==='wanted-but-paused'){player.wantsPlayback=true;video.paused=true;}
    if(fault==='unlanded-seek'){
      player.started=false; video.seeking=true;
      player.controlSeek={sequence:1,targetMs:90_000,executed:true,executedAt:now};
    }
    h.stub.monitor(player,video);
    now+=7_500;
    if(fault!=='silent-clock') video.currentTime+=7.5;
    if(fault==='unlanded-seek') player.controlPresentedFrames+=100;
    h.stub.monitor(player,video);
    await flush();
    assert.equal(h.reopened.length,0,`${fault} has not spent its observation budget`);
    now+=500;
    h.stub.monitor(player,video);
    await flush(); await flush();
    assert.equal(h.reopened.length,1,`${fault} recovers at eight seconds without a waiting event`);
    if(fault==='unlanded-seek') assert.equal(h.reopened[0].position,90);
  }
  for(const suppression of ['paused','ended','hidden']){
    let now=1_000;
    const h=stallHarness({clock:{now:()=>now}});
    const player=Object.assign(stalledPlayer(),{waitAt:null});
    const video={paused:false,seeking:false,ended:false,currentTime:10};
    h.stub.monitor(player,video);
    if(suppression==='hidden') h.stub.hidden(true); else video[suppression]=true;
    now+=60_000; h.stub.monitor(player,video);
    await flush();
    assert.equal(h.reopened.length,0,`${suppression} is not playback failure`);
    if(suppression==='hidden') h.stub.hidden(false); else video[suppression]=false;
    h.stub.monitor(player,video);
    await flush();
    assert.equal(h.reopened.length,0,'returning starts a new observation window');
  }

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
    const began=options.began==null?100:options.began;
    // The clamp below is `deadline - elapsed`, and `elapsed` is
    // `performance.now() - began`. Left on the real clock it is however long
    // this file has taken to reach this line, so `an interval past the
    // deadline is clamped to it` asserted 8000 and got whatever the machine
    // had spent — 7504 on a pristine `main` here. The harness already accepts
    // an injected clock; these cases are about the clamp, not about elapsed
    // time, so they get one frozen at the moment the wait began. A case that
    // wants time to move passes its own.
    const clock = options.clock || { now: () => began };
    const h = stallHarness({ answer: () => ({ type: "none" }), clock });
    const player = Object.assign(stalledPlayer(), options.player || {});
    const video = options.video || stalledVideo;
    player.waitAt=began;
    h.stub.attach(player, video, bootstrap());
    h.attached.push(player);
    await flush();
    h.answerWith(() => action);
    // start() already spent sequence 1; the ask must be answered by its own.
    const before = h.sent.length;
    const running = h.stub.stall(player, video, began, 3);
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

  // §3.3 row 18: a refused control exchange — `session_gone` on a committed
  // successor's first one is the row's named case — leaves the incumbent
  // exactly where it was, because this reporter owns no recovery. It gets a
  // ledger row and never a surface, and the row is the only trace there is.
  {
    const h = stallHarness({ answer: () => ({ type: "none" }) });
    const player = stalledPlayer();
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await flush();
    h.holdWith(() => {
      throw Object.assign(new Error("playback control session_gone"),
        { status: 404, code: "session_gone" });
    });
    h.stub.probe(player, stalledVideo);
    await settleExchange();
    const raised = h.loading;
    assert.deepEqual(raised.map((entry) => entry.source), ["log_only"],
      "a failed exchange is log-only — nothing is drawn over a playing picture");
    assert.equal(h.stops, 0, "and nothing about the player is stopped");
    h.stub.detach(player);
  }

  // An invocation scheduled on the absolute boundary has no remaining
  // control budget. It must recover without putting another request on the
  // wire or waiting through another ask window.
  {
    const h = stallHarness();
    const player = stalledPlayer();
    const began = performance.now() - 20_000;
    player.waitAt = began;
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await flush();
    const before = h.sent.length;

    await h.stub.stall(player, stalledVideo, began, 3);

    assert.equal(h.sent.length, before, "the expired deadline starts no control ask");
    assert.equal(h.reopened.length, 1, "the absolute boundary falls through to recovery");
    h.stub.detach(player);
  }

  // A terminal response belongs to the viewer intent captured with its
  // request. Clearing while that request is in flight must neither re-arm the
  // verdict nor stop the reporter that now owns the newer intent.
  {
    let releaseTerminal;
    const h = stallHarness({ answer: () => ({ type: "none" }) });
    const player = stalledPlayer();
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await flush();
    h.holdWith((request) => request.sequence === 2
      ? new Promise((resolve) => { releaseTerminal = () => resolve(Object.assign(
        response(request),
        { action: { type: "terminal", code: "unsupported", message: "stale intent" } },
      )); })
      : null);
    h.stub.askProbe(player);
    await settleExchange();
    assert.equal(typeof releaseTerminal, "function", "the terminal exchange is in flight");

    h.stub.supersede(player);
    h.answerWith(() => ({ type: "none" }));
    h.stub.probe(player, stalledVideo);
    releaseTerminal();
    await settleExchange();

    assert.equal(h.stub.armedVerdict(player), null, "the stale terminal does not re-arm");
    assert.equal(h.stub.probe(player, stalledVideo).stopped, false,
      "the stale terminal does not stop the new intent reporter");
    assert.ok(h.sent.length >= 3, "the newer intent still exchanges");
    h.stub.detach(player);
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
    assert.ok(h.loading[0].actions.includes("retry"), "Try again survives a terminal verdict");
    assert.ok(h.loading[0].actions.includes("force_transcode"),
      "Force transcode survives it — it is a different recipe");
    assert.equal(player.surfaceTranscodeReason, "stall-terminal",
      "and reopens as the terminal recipe change it always was");
    // D1 is unchanged, but the surface it produces is not drawn over a running
    // player: this branch returns with nothing left to try (contract §3.4).
    assert.equal(h.loading[0].source, "owner_stopped");
    assert.equal(h.loading[0].player_stopped, true);
    assert.equal(h.stops, 1, "a terminal verdict stops the player before it says so");
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

  // An old server can still return a producer hold for a loaded presentation
  // wait. It cannot suppress the client-owned repair: the browser has already
  // completed its one native reevaluation and the hold describes production,
  // not whether loaded bytes reached the display.
  {
    // This wait began with more buffered than the shipped supply threshold:
    // plenty left, and still not playing.
    let nudges=0;
    const video=Object.assign({},stalledVideo,{play(){nudges++;return Promise.resolve();}});
    const { h, player } = await askWith({ type: "hold", reason: "no_room" },
      { player: { waitRunway: 22 }, video });
    assert.equal(nudges,1,"loaded media receives one native reevaluation before repair");
    assert.notEqual(h.log.find((entry)=>entry.detail==="native_reevaluation:wait"),undefined);
    assert.deepEqual(h.reopened,[{kind:"seek",position:12}],
      "an old-server hold reaches one same-recipe local repair");
    assert.equal(player.stallRecoveries,1,"the repair spends the one automatic attempt");
    assert.equal(player.waitTimer,null,"the completed reevaluation leaves no deferral timer");
    assert.equal(h.loading[0].source,"owner_recovery_step",
      "the viewer is told the local repair, not an obsolete server wait");
    assert.equal(h.stops,0,"the bounded repair keeps the owner active");
    assert.equal(h.stalls.length, 1,
      "the stall is recorded once, not once per control response");
  }

  // A passive response leaves no pending work after the one native
  // reevaluation. The repair starts now rather than marching the same episode
  // through another eight-second timer callback.
  {
    let nudges=0;
    const video=Object.assign({},stalledVideo,{play(){nudges++;return Promise.resolve();}});
    const { h, player } = await askWith({ type: "none" },
      { player: { waitRunway: 22 }, video });
    assert.equal(nudges,1,"passive control preserves one native reevaluation");
    assert.deepEqual(h.reopened,[{kind:"seek",position:12}],
      "passive control reaches one same-recipe repair");
    assert.equal(player.stallRecoveries,1,"the episode spends one attempt");
    assert.equal(player.waitTimer,null,"passive control arms no repeated callback");
  }

  // Presentation progress while the bounded control ask is outstanding clears
  // the wait identity. Its eventual passive response cannot act on the retired
  // episode or open a replacement.
  {
    let now=9_000,release=null;
    const h=stallHarness({clock:{now:()=>now}});
    const player=Object.assign(stalledPlayer(),{waitAt:1,waitRunway:22,wantsPlayback:true,
      controlHasFrameCallbacks:true,controlPresentedFrames:4,source:{video_codec:"h264"}});
    const video={paused:false,seeking:false,ended:false,currentTime:10,error:null,
      play(){return Promise.resolve();}};
    h.stub.attach(player,video,bootstrap());h.attached.push(player);await flush();
    h.holdWith(request=>new Promise(resolve=>{release=()=>resolve(Object.assign(response(request),
      {action:{type:"none"}}));}));
    const running=h.stub.stall(player,video,1,3);
    await settleExchange();
    assert.equal(typeof release,"function","the passive control response is outstanding");
    h.stub.monitor(player,video);now=9_500;video.currentTime=11;
    player.controlPresentedFrames=5;h.stub.monitor(player,video);
    assert.equal(player.waitAt,null,"actual presentation progress retires the wait identity");
    release();await settleExchange();await running;
    assert.equal(h.reopened.length,0,"the late passive response cannot repair a recovered episode");
    h.stub.detach(player);
  }

  {
    let now=9_500;
    const h=stallHarness({clock:{now:()=>now}});
    const player=Object.assign(stalledPlayer(),{waitAt:1,waitRunway:8});
    const decoderVideo=Object.assign({},stalledVideo,{error:{code:3,message:"decode failed"}});
    h.stub.attach(player,decoderVideo,bootstrap()); h.attached.push(player);
    await flush();
    h.answerWith(()=>({type:'hold',reason:'no_room'}));
    const first=h.stub.stall(player,decoderVideo,1,3);
    await settleExchange(); await first;
    assert.equal(h.reopened.length,0);
    now=17_500;
    h.holdWith(()=>new Promise(()=>{}));
    const second=h.stub.stall(player,decoderVideo,1,3);
    await settleExchange();
    const waiter=player.controlWaiters[0];
    assert.equal(waiter.hardExpiry,20_001,'the ask cap is the remaining absolute budget');
    now=19_900; waiter.extend();
    assert.equal(h.timers.get(waiter.timer).ms,101,'an extension cannot exceed the stall deadline');
    now=20_001;
    h.timers.get(waiter.timer).fn();
    await second;
    assert.equal(h.reopened.length,1,'a blocked control request cannot extend a twenty-second freeze');
    h.stub.detach(player);
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

  // A handoff is not a verdict from the replacement owner. The reporter
  // adopts it independently; the old recovery falls through immediately.
  {
    const h = stallHarness({ answer: () => ({ type: "none" }) });
    const player = stalledPlayer();
    h.attached.push(player);
    h.stub.attach(player, stalledVideo, bootstrap());
    await flush();
    h.holdWith(() => Promise.reject(controlError(409,"owner_changed",{
      generation:"44444444-4444-4444-8444-444444444444",controlEpoch:9,
    })));
    const running=h.stub.stall(player,stalledVideo,100,3);
    await settleExchange();
    assert.equal(await settledPromptly(running),true,
      "owner_changed settles the old owner's ask immediately");
    await running;
    assert.equal(h.reopened.length,1,"recovery does not wait on the new owner");
    h.stub.detach(player);
  }

  // A terminal verdict can arrive on any exchange, and the reporter stops on
  // it. Ruling D1 says the verdict is armed, not executed — so a later stall
  // inside the lease must still read the server's words, even though there is
  // no longer a reporter to ask.
  {
    assert.doesNotMatch(shippedSource("attachSession"), /controlVerdict\s*=\s*null/,
      "a same-title session replacement preserves the armed diagnosis");
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
    assert.ok(player.controlVerdictExpiresAt>performance.now(),
      "the armed diagnosis is bounded by the server lease");
    const running = h.stub.stall(player, stalledVideo, 100, 3);
    await flush(); await flush();
    await running;
    assert.equal(h.reopened.length, 0, "the armed verdict still suppresses the guess");
    assert.equal(h.loading[0].title, "No decoder for this.",
      "and the viewer reads it rather than the client's invention");
    player.controlVerdictExpiresAt=performance.now()-1;
    const afterLease=h.stub.stall(player,stalledVideo,100,3);
    await flush(); await afterLease;
    assert.equal(h.reopened.length,1,"an expired diagnosis cannot suppress client recovery");
    assert.equal(player.controlVerdict,null,"expiration clears the stale verdict");
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
    let release=null;
    h.holdWith(request=>new Promise(resolve=>{release=()=>{
      leave(player);
      resolve(Object.assign(response(request),{action:{type:"none"}}));
    };}));
    const running = h.stub.stall(player, stalledVideo, 100, 3);
    await settleExchange();
    assert.equal(typeof release,"function",`${label}: the ask response is held`);
    release();
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
      "playNextAudiobookPart", "clientLog", "seekTo", "clockFromSec",
      "CONTROL_CLIENT_ID", "playbackControlSnapshot", "sendPlaybackControl",
      "window", "PlurxPlaybackControl", "performance", "document",
      [
        "let PLAYER=null;",
        surfaceSeam(),
        askConstants,
        shippedSource("controlVerdictText"),
        shippedSource("armedPlaybackControlVerdict"),
        shippedSource("holdReasonText"),
        shippedSource("playbackControlObservationOverride"),
        shippedSource("notifyPlaybackControl"),
        // The reporter settles a carried acknowledgement on every exchange and
        // frees a staged successor when it stops, so both paths are real here
        // rather than stubbed.
        shippedConst("PREPARED_TERMINAL_STATES"), shippedConst("PREPARED_ACK_QUEUE_MAX"),
        shippedConst("PREPARED_SETTLED_MEMORY"), shippedConst("PREPARED_FINALIZATION_MS"),
        shippedSource("preparedVideoElement"), shippedSource("preparedState"),
        shippedSource("preparedSettlementDone"), shippedSource("markPreparedSettlement"),
        shippedSource("queuePlaybackControlAcknowledgement"),
        shippedSource("pendingPlaybackControlAcknowledgement"),
        shippedSource("settlePlaybackControlAcknowledgement"),
        shippedSource("flushPreparedSettlement"), shippedSource("cancelPreparedFirstFrame"),
        shippedSource("endingPlaybackControlSnapshot"),
        shippedSource("finishStoppingPlaybackControl"),
        shippedSource("continueStoppingPlaybackControl"),
        shippedSource("destroyHlsInstance"),
        shippedSource("freePreparedReplacement"), shippedSource("abandonPreparedReplacement"),
        shippedSource("askPlaybackControl"),
        shippedSource("settlePlaybackControlWaiters"),
        shippedSource("clearPlaybackControlWaiters"),
        shippedSource("stopPlaybackControl"),
        shippedSource("startPlaybackControl"),
        shippedSource("endedStillOurs"),
        shippedSource("handleEnded"),
        shippedSource("hasPendingPlaybackOpen"),shippedSource("playbackOwnsAttachedMedia"),
        shippedSource("playbackContinuation"),
        "return {",
        " attach(player,video,bootstrap){PLAYER=player;",
        "   return startPlaybackControl(video,player,bootstrap);},",
        " detach(player){PLAYER=player; stopPlaybackControl(player); PLAYER=null;},",
        " surface:surfacePainted, stops:()=>surfaceStops.length,",
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
      stub, timers, log, seeks, video, attached: [],
      get loading() { return stub.surface(); },
      get stops() { return stub.stops(); },
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
    // Giving up is exhaustion, so the claim that the player is stopped is one
    // the owner makes by stopping it (contract §3.4), not one inferred from
    // `ended` across an ask that can take seconds.
    assert.equal(h.loading[0].source, "repeated_early_end");
    assert.equal(h.loading[0].player_stopped, true);
    assert.equal(h.stops, 1);
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
    assert.deepEqual(h.loading[0].actions, ["retry", "close"], "and can act on it");
    assert.equal(h.loading[0].source, "control_hold",
      "a hold is a notice, not a screen: D1 arms wording, it does not tear down");
    assert.equal(h.stops, 0, "and it stops nothing");
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
    assert.ok(h.loading[0].actions.includes("retry"),
      "and the viewer keeps every option they had");
    assert.equal(h.loading[0].source, "owner_stopped");
    assert.equal(h.stops, 1, "the owner stops before the verdict goes over it");
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
        capture: () => captureSnapshot(snapshot()),
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


  // ---- the second pipeline, the alignment, and the switch ------------------
  //
  // `PLAYER.hls` is a scalar and `attachHls` opens by destroying, so a prepared
  // handoff is the first thing in this file that needs two live pipelines. The
  // harness below is deliberately built out of the SHIPPED functions: a stub of
  // the preparation state machine would prove the fixture, not the page.
  function preparedHarness(options = {}) {
    const notifies = [];
    const logs = [];
    const wired = [];
    const adopted = [];
    const removed = [];
    const timers = new Map();
    const intervals = [];
    let nextTimer = 1;
    const element = (id) => ({
      id, muted: id !== "video", volume: 1, paused: true, currentTime: 0,
      playbackRate: 1, defaultPlaybackRate: 1,
      videoWidth: options.hasVideo === false ? 0 : 1920,
      style: { display: id === "video" ? "" : "none" },
      attributes: {}, listeners: {}, src: null, loads: 0, plays: 0,
      ranges: [], parentNode: null,
      buffered: {
        get length() { return this.owner.ranges.length; },
        start(i) { return this.owner.ranges[i][0]; },
        end(i) { return this.owner.ranges[i][1]; },
      },
      play() { this.paused = false; this.plays += 1; return Promise.resolve(); },
      pause() { this.paused = true; },
      load() { this.loads += 1; },
      removeAttribute(name) { if (name === "src") this.src = null; delete this.attributes[name]; },
      setAttribute(name, value) { this.attributes[name] = value; },
      addEventListener(name, fn) { (this.listeners[name] = this.listeners[name] || []).push(fn); },
      removeEventListener(name, fn) {
        this.listeners[name] = (this.listeners[name] || []).filter((f) => f !== fn);
      },
      emit(name) { for (const fn of (this.listeners[name] || []).slice()) fn(); },
    });
    const live = element("video");
    live.buffered.owner = live;
    // The prepared element is created on first use, so the harness's document
    // has to create and attach one — and must not find it before. A second
    // preparation after a switch creates another, because the retired element
    // leaves the page with its listeners.
    const created = [];
    const attached = [live];
    const parent = {
      appendChild(node) { node.parentNode = parent; attached.push(node); },
      removeChild(node) {
        node.parentNode = null;
        removed.push(node);
        const at = attached.indexOf(node);
        if (at !== -1) attached.splice(at, 1);
      },
    };
    live.parentNode = parent;
    const document = {
      getElementById: (id) => attached.find((node) => node.id === id) || null,
      createElement: () => {
        const node = element("");
        node.buffered.owner = node;
        if (options.videoFrameCallback !== false) {
          node.requestVideoFrameCallback = (fn) => { node.frameCallback = fn; };
        }
        created.push(node);
        return node;
      },
    };
    if (options.videoFrameCallback !== false) {
      live.requestVideoFrameCallback = (fn) => { live.frameCallback = fn; };
    }
    const instances = [];
    class FakeHls {
      static Events = { MANIFEST_PARSED: "manifest", ERROR: "error",
        BUFFER_APPENDED: "append", LEVEL_LOADED: "level" };
      static isSupported() { return true; }
      constructor(config) {
        this.config = config; this.events = {}; this.destroyed = false;
        this.bandwidthEstimate = 9_000_000; instances.push(this);
      }
      on(event, fn) { this.events[event] = fn; }
      loadSource(url) { this.source = url; }
      attachMedia(media) { this.media = media; }
      destroy() { this.destroyed = true; }
    }
    const scope = new Function(
      "document", "window", "Hls", "PlaybackPolicy", "PlurxPlaybackControl",
      "TOKEN", "setTimeout", "clearTimeout", "setInterval", "clearInterval",
      "notifyPlaybackControl", "clientLog", "nativeHls", "wirePlayerMedia",
      "armHitchDetector", "setupAirplay", "playbackProgressTick", "probeDecode",
      "handleEnded", "renderPlayerInfo", "pbTick", "pbSyncPlayIcon",
      [
        "let PLAYER=null;",
        "function playbackContext(){return {};}",
        "function cancelHlsStartup(){}",
        // §3.3 row 18: a staging nobody took up raises a log-only fault, and
        // the seam records it so the assertions below can read it.
        "const surfaceRaised=[];",
        "function raisePlaybackSurface(source,fault){surfaceRaised.push({source,fault:fault||{}});return null;}",
        "function playbackSurfaceGeneration(p){return (p&&p.attemptId)||null;}",
        "function tok(url){return url;}",
        "function preferNativeHls(){return nativeHls;}",
        "function bufferTargets(){return {fwd:20,back:10,budgeted:false};}",
        "function vodClientContract(){return {fragLoadPolicy:{}};}",
        shippedConst("PREPARED_HANDOFF_KEY"),
        shippedConst("PREPARED_BUFFER_LEAD_MS"),
        shippedConst("PREPARED_ALIGN_SLACK_MS"),
        shippedConst("PREPARED_FIRST_FRAME_MS"),
        shippedConst("PREPARED_TERMINAL_STATES"),
        shippedConst("PREPARED_ACK_QUEUE_MAX"),
        shippedConst("PREPARED_SETTLED_MEMORY"),
        shippedSource("preparedHandoffEnabled"), shippedSource("setPreparedHandoffEnabled"),
        shippedSource("sessionMediaOriginMs"), shippedSource("realMediaPositionMs"),
        shippedSource("preparedLocalPositionMs"), shippedSource("playbackFilmPositionMs"),
        shippedSource("streamHasVideo"),
        shippedSource("pendingPlaybackControlAcknowledgement"),
        shippedSource("queuePlaybackControlAcknowledgement"),
        shippedSource("settlePlaybackControlAcknowledgement"),
        shippedSource("preparedVideoElement"), shippedSource("ensurePreparedVideoElement"),
        shippedSource("preparedState"),
        shippedSource("preparedSettlementDone"), shippedSource("markPreparedSettlement"),
        shippedSource("handlePreparedReplacementAction"),
        shippedSource("beginPreparedReplacement"), shippedSource("preparedSelectionText"),
        shippedSource("preparedHlsAttach"), shippedSource("preparedNativeAttach"),
        shippedSource("notePreparedMetadata"), shippedSource("preparedBufferedThroughMs"),
        shippedSource("notePreparedBuffer"), shippedSource("commitPreparedReplacement"),
        shippedSource("retirePreparedPredecessor"), shippedSource("rollbackPreparedReplacement"),
        shippedSource("adoptPlaybackMediaElement"), shippedSource("disposeRetiredMediaElement"),
        shippedSource("preparedFirstFrame"), shippedSource("cancelPreparedFirstFrame"),
        shippedSource("failPreparedReplacement"),
        shippedSource("abandonPreparedReplacement"), shippedSource("freePreparedReplacement"),
        shippedSource("destroyHlsInstance"),
        shippedSource("resetPlaybackTransportEvents"), shippedSource("playbackTransportEvents"),
        shippedSource("beginPlaybackMediaAttachment"),
        shippedSource("hasPendingPlaybackOpen"), shippedSource("playbackOwnsAttachedMedia"),
        shippedSource("rememberPlaybackTransportIntent"),
        shippedSource("pausePlaybackInternally"),
        shippedSource("teardownHls"),
        "return {set(p){PLAYER=p;return p;},current:()=>PLAYER,",
        " handle(action){return handlePreparedReplacementAction(PLAYER,action);},",
        " abandon(state,reason){return abandonPreparedReplacement(PLAYER,state,reason);},",
        " failPrepared(detail){return failPreparedReplacement(PLAYER,preparedState(PLAYER),detail);},",
        " teardown(){return teardownHls();},",
        " cancelFrame(){return cancelPreparedFirstFrame(PLAYER);},",
        " pending(demand){return pendingPlaybackControlAcknowledgement(PLAYER,demand);},",
        " settle(request){return settlePlaybackControlAcknowledgement(PLAYER,request);},",
        " origin:sessionMediaOriginMs, film:realMediaPositionMs, local:preparedLocalPositionMs,",
        " surfaceRaised};",
      ].join("\n"),
    );
    const api = scope(
      document, { Hls: FakeHls, PlurxPlaybackControl: control }, FakeHls,
      { bandwidthSeedBps: () => null }, control, null,
      (fn, ms) => { const id = nextTimer++; timers.set(id, { fn, ms }); return id; },
      (id) => { timers.delete(id); },
      (fn, ms) => { intervals.push({ fn, ms }); return intervals.length; },
      () => {},
      () => { notifies.push(current()); },
      (entry) => { logs.push(entry); },
      !!options.native,
      (v) => wired.push(v),
      (v) => adopted.push(["hitch", v]),
      (v) => adopted.push(["airplay", v]),
      (v) => adopted.push(["progress", v]),
      () => adopted.push(["probe"]),
      async () => { adopted.push(["ended"]); },
      () => adopted.push(["render"]),
      () => {}, () => {},
    );
    const current = () => {
      const player = api.current();
      const queue = player && player.controlAcknowledgements;
      return queue && queue.length ? queue[queue.length - 1] : null;
    };
    // `spare` is a live lookup, not a value: the element is created on the first
    // preparation and replaced after every switch, so a snapshot taken when the
    // harness is built would be null forever. Defined rather than assigned —
    // `Object.assign` would copy what the getter returns right now.
    Object.defineProperty(api, "spare", {
      get: () => (api.current() && api.current().preparedCommitting
        && api.current().preparedCommitting.frameElement)
        || attached.find((node) => node.id === "video-prepared")
        || created[created.length - 1] || null,
    });
    return Object.assign(api, {
      live, instances, notifies, logs, attached, created, removed, wired, adopted,
      intervals,
      queue: () => (api.current() && api.current().controlAcknowledgements) || [],
      head: () => api.pending("active"),
      fire(id) { const t = timers.get(id); if (t) { timers.delete(id); t.fn(); } },
      fireAll() { for (const [id] of [...timers]) this.fire(id); },
      timers,
    });
  }

  // The newest settlement this player has queued. There is a QUEUE now, not a
  // slot: a supersede leaves the old staging's `aborted` waiting for an
  // exchange while the new one is already reporting progress, and a slot loses
  // one of them.
  const latest = (h) => { const q = h.queue(); return q.length ? q[q.length - 1] : null; };
  const PREPARE_ORIGIN_MS = 600_000;
  const prepareAction = (overrides = {}) => Object.assign({}, PREPARE_FIXTURE, overrides);
  const preparedPlayer = (overrides = {}) => Object.assign({
    offset: 0, vod: true, hls: null, prepared: null, priorKbps: 0,
    controlAcknowledgement: null, pendingMediaChange: null, wantsPlayback: true,
  }, overrides);

  // §4 — the alignment functions, as pure functions, with no player at all.
  {
    const h = preparedHarness();
    assert.equal(h.origin({ vod: true, media_origin_ms: 900_000, start_seconds: 60 }), 0,
      "a VOD playlist is the whole title, so its zero is the source's zero");
    assert.equal(h.origin({ vod: false, media_origin_ms: 900_000, start_seconds: 60 }), 900_000,
      "media_origin_ms is the server saying where this session's zero lands");
    assert.equal(h.origin({ vod: false, start_seconds: 60 }), 60_000,
      "start_seconds is the older field that says the same thing");
    assert.equal(h.origin({ vod: false, media_origin_ms: -5 }), 0, "an origin is never negative");
    assert.equal(h.origin(null), 0);
    assert.equal(h.film(10_000, 900_000, false), 910_000, "film time is origin plus local time");
    assert.equal(h.film(10_000, 900_000, true), 10_000, "a VOD session's local time IS film time");
    assert.equal(h.film(Number.NaN, 900_000, false), 0);
    assert.equal(h.local(910_000, 900_000), 10_000, "and the inverse puts the successor there");
    assert.equal(h.local(10_000, 900_000), 0, "a successor never starts before its own zero");
  }

  // Building the second pipeline: one instance, on the hidden element, and the
  // incumbent is not touched. `attachHls` opens by DESTROYING the incumbent
  // (index.html's teardown-first rule), which is why this path exists beside it
  // rather than through it.
  {
    const h = preparedHarness();
    const incumbent = { bandwidthEstimate: 5_000_000, destroyed: false, destroy() { this.destroyed = true; } };
    const predecessorHealth = { id: "incumbent", server_ready_seconds: 12 };
    const p = h.set(preparedPlayer({ hls: incumbent, vod: false, offset: 600,
      health: predecessorHealth, healthObservedAt: 75, presentationAdvancedAt: 80 }));
    h.live.currentTime = 300;                       // film 900 s
    const state = h.handle(prepareAction({ media_origin_ms: PREPARE_ORIGIN_MS }));
    assert.ok(state, "a valid preparation builds");
    assert.equal(h.attached.length, 2, "the successor's element is created on first use");
    assert.equal(h.spare.id, "video-prepared");
    assert.equal(h.spare.attributes["aria-hidden"], "true",
      "…and is out of the accessibility tree while it is a successor");
    assert.equal(h.instances.length, 1, "exactly one prepared pipeline");
    assert.equal(h.instances[0].media, h.spare, "…attached to the hidden element");
    assert.equal(h.instances[0].source, PREPARE_FIXTURE.playlist_url);
    assert.equal(p.hls, incumbent, "the incumbent instance is untouched");
    assert.equal(incumbent.destroyed, false, "…and specifically not destroyed");
    assert.equal(h.spare.muted, true, "the successor is muted until the switch");
    assert.equal(h.spare.style.display, "none", "…and invisible until the switch");
    // §C6 — the successor's local zero is `media_origin_ms` in film time, so a
    // successor primed for film 900 s whose zero is 600 s starts at 300 s.
    assert.equal(h.instances[0].config.startPosition, 300,
      "the successor is primed at the incumbent's film position on its own timeline");
    assert.equal(latest(h), null, "nothing is acknowledged before the manifest");

    // §C12.7 — a repeated action_id is the SAME preparation. The server
    // replays it byte-identically on every exchange until it settles.
    const again = h.handle(prepareAction({ media_origin_ms: PREPARE_ORIGIN_MS }));
    assert.equal(again, state, "a repeated action_id is the same preparation");
    assert.equal(h.instances.length, 1, "…and does not build a second pipeline");

    h.instances[0].events.manifest();
    assert.deepEqual(latest(h),
      { action_id: PREPARE_FIXTURE.action_id, state: "metadata_ready" });
    assert.equal(h.spare.paused, false, "the successor primes with its clock running");

    // Buffered short of the lead is not ready…
    h.spare.currentTime = 300;
    h.spare.ranges = [[300, 302]];
    h.instances[0].events.append();
    assert.equal(latest(h).state, "metadata_ready",
      "buffered to the playhead is not buffered past the switch point");
    // Intent can change while the successor is preparing. The commit must
    // sample this boundary, not replay the snapshot from preparation start.
    p.wantsPlayback = false;
    h.live.muted = true;
    h.live.volume = 0.35;
    h.live.defaultPlaybackRate = 1.5;
    h.live.playbackRate = 1.5;
    // …and buffered past it is, in FILM time.
    h.spare.ranges = [[300, 320]];
    h.instances[0].events.append();
    const buffered = h.notifies.find((ack) => ack && ack.state === "buffer_ready");
    assert.ok(buffered, "buffer_ready is reported on its own exchange");
    assert.equal(buffered.buffered_through_ms, PREPARE_ORIGIN_MS + 320_000,
      "buffered_through_ms is film time, not the successor's local time");

    // The switch itself — which readiness leads straight into, because the
    // client decides when to switch and the server never orders it.
    assert.equal(p.hls, h.instances[0], "the prepared instance is now authoritative");
    assert.equal(incumbent.destroyed, false,
      "the predecessor remains recoverable until the successor proves a frame");
    assert.equal(p.sessionId, PREPARE_FIXTURE.session_id);
    assert.equal(p.offset, PREPARE_ORIGIN_MS / 1000, "the player's origin follows the session");
    assert.equal(p.health, null, "the successor never inherits predecessor server telemetry");
    assert.equal(p.healthObservedAt, null, "the successor starts without a server-sample age");
    assert.equal(p.presentationAdvancedAt, null, "the successor must observe its own presentation advance");
    assert.equal(h.spare.id, "video", "the successor is the element the page addresses");
    assert.equal(h.live.id, "video-prepared", "and the predecessor is the spare");
    assert.equal(h.spare.muted, true, "a muted incumbent cannot leak an audible successor frame");
    assert.equal(h.spare.volume, 0.35);
    assert.equal(h.spare.defaultPlaybackRate, 1.5);
    assert.equal(h.spare.playbackRate, 1.5);
    assert.equal(h.spare.paused, true, "a pause during preparation survives the switch");
    assert.equal(h.spare.style.display, "", "…and visible only after the switch");
    assert.equal(h.live.style.display, "none");
    assert.equal(h.live.muted, true);
    assert.equal(p.prepared, null, "the slot is free once the switch is made");
    assert.equal(latest(h).state, "buffer_ready",
      "and the commit waits for a frame rather than being claimed at the swap");
    h.spare.frameCallback();
    assert.equal(latest(h).state, "committed");
    assert.ok(latest(h).first_frame_unix_ms > 0);
    assert.equal(incumbent.destroyed, true,
      "first-frame proof retires the predecessor through the normal teardown");
  }

  // The commit is sent only after a frame renders. It is a claim that the
  // switch already happened, and the CAS behind it moves the playback pointer.
  {
    const h = preparedHarness();
    const p = h.set(preparedPlayer({ hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.handle(prepareAction());
    h.instances[0].events.manifest();
    h.spare.ranges = [[0, 30]];
    h.instances[0].events.append();
    assert.equal(latest(h).state, "buffer_ready",
      "no commit before a frame — requestVideoFrameCallback has not fired");
    assert.equal(p.hls, h.instances[0], "…even though the switch itself has happened");
    h.spare.frameCallback();
    assert.equal(latest(h).state, "committed");
    assert.ok(latest(h).first_frame_unix_ms > 1_600_000_000_000,
      "committed carries the wall clock of the first qualifying frame");
  }

  // A switch that renders nothing is a failed preparation, not a silent one.
  // Leaving it to the 330-second deadline costs the session its only slot.
  {
    const h = preparedHarness();
    const incumbent = { bandwidthEstimate: 1, destroyed: false,
      destroy() { this.destroyed = true; } };
    const predecessorHealth = { id: "incumbent", server_ready_seconds: 9 };
    const p = h.set(preparedPlayer({ hls: incumbent, sessionId: "incumbent",
      probeUrl: "/incumbent/probe", offset: 12, wantsPlayback: false,
      health: predecessorHealth, healthObservedAt: 25, presentationAdvancedAt: 30 }));
    h.live.muted = true;
    h.live.volume = 0.4;
    h.live.defaultPlaybackRate = 1.25;
    h.live.playbackRate = 1.25;
    h.handle(prepareAction());
    const successor = h.instances[0];
    h.instances[0].events.manifest();
    h.spare.ranges = [[0, 30]];
    h.instances[0].events.append();
    assert.equal(incumbent.destroyed, false,
      "the watchdog window keeps the proven predecessor alive");
    h.live.volume = 0;
    h.live.defaultPlaybackRate = 0.5;
    h.live.playbackRate = 0.5;
    h.fireAll();
    assert.equal(latest(h).state, "failed",
      "the first-frame watchdog settles the staging rather than leaving it to the deadline");
    assert.equal(p.hls, incumbent, "timeout restores the predecessor as authoritative");
    assert.equal(p.sessionId, "incumbent");
    assert.equal(p.probeUrl, "/incumbent/probe");
    assert.equal(p.offset, 12);
    assert.equal(p.health, predecessorHealth, "rollback restores predecessor server telemetry");
    assert.equal(p.healthObservedAt, 25, "rollback restores predecessor sample ownership");
    assert.equal(p.presentationAdvancedAt, 30, "rollback restores predecessor presentation age");
    assert.equal(incumbent.destroyed, false, "rollback keeps the proven pipeline alive");
    assert.equal(successor.destroyed, true, "rollback destroys the frame-less successor");
    assert.equal(h.live.id, "video");
    assert.equal(h.live.style.display, "");
    assert.equal(h.live.muted, true);
    assert.equal(h.live.volume, 0.4);
    assert.equal(h.live.defaultPlaybackRate, 1.25);
    assert.equal(h.live.playbackRate, 1.25);
    assert.equal(h.live.paused, true);
    assert.equal(h.live.parentNode !== null, true);
    assert.deepEqual(h.removed, [h.created[0]],
      "the discarded successor, not the recovered predecessor, leaves the page");
  }

  // §C12.6 — an abandoned preparation is settled, and the instance is freed.
  for (const [label, drive] of [
    ["a new attach on the authoritative element", (h) => h.teardown()],
    ["an explicit abandon", (h) => h.abandon("aborted", "the viewer seeked")],
  ]) {
    const h = preparedHarness();
    const incumbent = { bandwidthEstimate: 5_000_000, destroyed: false, destroy() { this.destroyed = true; } };
    const p = h.set(preparedPlayer({ hls: incumbent }));
    h.handle(prepareAction());
    const prepared = h.instances[0];
    drive(h);
    assert.equal(latest(h).state, "aborted", `${label} reports an abort`);
    assert.equal(latest(h).action_id, PREPARE_FIXTURE.action_id);
    assert.equal(prepared.destroyed, true, `${label} destroys the prepared instance`);
    assert.equal(p.prepared, null, `${label} frees the slot`);
    assert.equal(h.spare.muted, true);
    assert.equal(h.spare.style.display, "none");
  }
  {
    // …and an abandon does not touch the incumbent, which is the whole reason
    // this path exists beside `attachHls` instead of inside it.
    const h = preparedHarness();
    const incumbent = { bandwidthEstimate: 5_000_000, destroyed: false, destroy() { this.destroyed = true; } };
    const p = h.set(preparedPlayer({ hls: incumbent }));
    h.handle(prepareAction());
    h.abandon("aborted", "the viewer changed quality");
    assert.equal(p.hls, incumbent, "PLAYER.hls was never touched");
    assert.equal(incumbent.destroyed, false);
  }

  // A pipeline that will not build says so.
  {
    const h = preparedHarness();
    const p = h.set(preparedPlayer({ hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.handle(prepareAction());
    h.instances[0].events.error(null, { fatal: true, details: "manifestLoadError" });
    assert.equal(latest(h).state, "failed");
    assert.equal(p.prepared, null, "a failed preparation frees its slot too");
    h.handle(prepareAction({ action_id: "77777777-7777-4777-8777-777777777777" }));
    assert.equal(h.instances.length, 2,
      "a later explicit action may retry after the failed action settled");
    h.instances[0].events.error(null, { fatal: false, details: "bufferStalledError" });
    assert.equal(latest(h).state, "failed", "a non-fatal error settles nothing");
  }

  // A second staging while one is live aborts the first. The server allows one
  // preparation per session; dropping the first silently would hold its slot.
  {
    const h = preparedHarness();
    const p = h.set(preparedPlayer({ hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.handle(prepareAction());
    const first = h.instances[0];
    const second = "77777777-7777-4777-8777-777777777777";
    h.handle(prepareAction({ action_id: second }));
    assert.equal(first.destroyed, true, "the superseded preparation is torn down");
    assert.equal(h.instances.length, 2);
    assert.equal(p.prepared.actionId, second);
    const aborted = h.notifies.find((ack) => ack && ack.state === "aborted");
    assert.equal(aborted.action_id, PREPARE_FIXTURE.action_id,
      "the abort names the staging it settles, not the one that replaced it");
  }

  // A preparation this client refuses never reaches the player at all.
  {
    const h = preparedHarness();
    h.set(preparedPlayer());
    assert.equal(h.handle(prepareAction({ playlist_url: "https://attacker.example/x.m3u8" })), null);
    assert.equal(h.instances.length, 0, "no pipeline is built for a refused preparation");
    assert.equal(h.handle(prepareAction({ type: "prepare_replacement" })), null,
      "the declared name is not a wire tag");
    assert.equal(h.instances.length, 0);
  }

  // The commit remains at the queue head even if the player ends in the same
  // turn. The snapshot builder gives it an active exchange before the end.
  {
    const h = preparedHarness();
    const p = h.set(preparedPlayer());
    p.controlAcknowledgements = [{ action_id: PREPARE_FIXTURE.action_id, state: "committed",
      first_frame_unix_ms: 1_757_000_000_000, committed_media_origin_ms: PREPARE_ORIGIN_MS }];
    assert.ok(h.pending("end"), "the commit is never dropped by terminal demand");
    assert.ok(h.pending("active"), "…and remains the head of the settlement queue");
    h.settle({ acknowledgement: { action_id: PREPARE_FIXTURE.action_id, state: "committed" } });
    assert.equal(h.queue().length, 0, "an accepted acknowledgement is spent");
    p.controlAcknowledgements = [{ action_id: PREPARE_FIXTURE.action_id, state: "buffer_ready",
      buffered_through_ms: 1 }];
    h.settle({ acknowledgement: { action_id: PREPARE_FIXTURE.action_id, state: "metadata_ready" } });
    assert.equal(h.queue().length, 1, "an older state does not settle a newer one");
    // Two stagings unsettled at once: the abort of the one that was superseded
    // is still queued behind the newer one's progress, and neither is lost.
    p.controlAcknowledgements = [];
    const other = "77777777-7777-4777-8777-777777777777";
    h.set(p);
    p.controlAcknowledgements = [
      { action_id: PREPARE_FIXTURE.action_id, state: "aborted" },
      { action_id: other, state: "metadata_ready" },
    ];
    assert.equal(h.pending("active").action_id, PREPARE_FIXTURE.action_id,
      "the older settlement goes first");
    h.settle({ acknowledgement: { action_id: PREPARE_FIXTURE.action_id, state: "aborted" } });
    assert.equal(h.pending("active").action_id, other, "…and the newer one follows it");
  }

  // The native-HLS path builds no hls.js instance and primes the same element.
  {
    const h = preparedHarness({ native: true });
    const p = h.set(preparedPlayer({ vod: false, offset: 600 }));
    h.live.currentTime = 300;
    h.handle(prepareAction({ media_origin_ms: PREPARE_ORIGIN_MS }));
    assert.equal(h.instances.length, 0, "native HLS needs no second hls.js instance");
    assert.equal(h.spare.src, PREPARE_FIXTURE.playlist_url);
    h.spare.emit("loadedmetadata");
    assert.equal(h.spare.currentTime, 300, "the native successor is seeked onto film time");
    assert.equal(latest(h).state, "metadata_ready");
  }

  // Gate A is a switch this browser owns, and it is what the server reads.
  {
    const capabilityVideo = {
      videoWidth: 1_920,
      requestVideoFrameCallback() {},
    };
    const capabilities = new Function(
      "localStorage", "PLAY_CAPS", "screen", "window", "document", [
      shippedConst("PREPARED_HANDOFF_KEY"),
      shippedSource("streamHasVideo"),
      shippedSource("preparedHandoffEnabled"), shippedSource("preparedHandoffOffered"),
      shippedSource("setPreparedHandoffEnabled"),
      shippedSource("playbackControlCapabilities"),
      "let PLAYER=null;",
      "return {capabilities:playbackControlCapabilities,enable:setPreparedHandoffEnabled,"
        + "enabled:preparedHandoffEnabled,player(p){PLAYER=p;}};",
    ].join("\n"))(
      (() => {
        const store = new Map();
        return { getItem: (key) => (store.has(key) ? store.get(key) : null),
          setItem: (key, value) => store.set(key, String(value)) };
      })(),
      { vcodec: "h264", maxheight: 1080 }, { height: 1080 },
      { innerHeight: 1080, devicePixelRatio: 1 },
      { getElementById: () => capabilityVideo },
    );
    capabilities.player({ source: { video_codec: "h264" } });
    assert.equal(capabilities.capabilities().dual_player_preparation, true,
      "on by default for a fresh browser with presentation evidence");
    capabilities.enable(false);
    assert.equal(capabilities.enabled(), false,
      "an explicit browser opt-out persists and remains authoritative");
    assert.equal(capabilities.capabilities().dual_player_preparation, false,
      "the explicit opt-out reaches the protocol capability");
    capabilities.enable(true);
    assert.equal(capabilities.enabled(), true);
    assert.equal(capabilities.capabilities().dual_player_preparation, true,
      "an enabled video player with real presentation evidence offers preparation");
    delete capabilityVideo.requestVideoFrameCallback;
    assert.equal(capabilities.capabilities().dual_player_preparation, true,
      "the frame callback API is not an enablement prerequisite");
    capabilityVideo.videoWidth = 0;
    capabilities.player({ source: {} });
    assert.equal(capabilities.capabilities().dual_player_preparation, true,
      "audio-only playback has honest advancing-clock evidence instead");
    // A stale field from an older client cannot become a hidden gate.
    capabilityVideo.videoWidth = 1_920;
    capabilityVideo.requestVideoFrameCallback = () => {};
    capabilities.player({ source: { video_codec: "h264" }, preparedRefused: true });
    assert.equal(capabilities.capabilities().dual_player_preparation, true,
      "a previous runtime failure never overrides the saved choice");
    capabilities.player(null);
    assert.equal(capabilities.capabilities().dual_player_preparation, false,
      "there is no runtime handoff offer without an attached player");
    capabilities.enable(false);
    assert.equal(capabilities.capabilities().dual_player_preparation, false);
  }

  // §C12.8 — `observed_download_bps` is populated wherever the platform can
  // measure it. This is advisory telemetry; missing or low throughput no
  // longer vetoes an explicit preparation request.
  {
    const measured = adapter.playbackControlSnapshot(video,
      Object.assign({}, player, { hls: { bandwidthEstimate: 12_345_678 } }));
    assert.equal(JSON.parse(JSON.stringify(measured)).observed_download_bps, 12_345_678,
      "the serialized body carries this client's own throughput estimate");
    const unmeasured = adapter.playbackControlSnapshot(video,
      Object.assign({}, player, { hls: null }));
    assert.equal(unmeasured.observed_download_bps, null,
      "…and null before hls.js has an estimate, rather than an invented number");
  }


  // ---- the wiring the harnesses used to stub ------------------------------
  //
  // Everything above proves the preparation functions in isolation. These
  // prove the SHIPPED page calls them: the dispatch, the snapshot seam, the
  // settlement, and the teardown flush. A mutation run found each of these
  // deletable with the whole suite green, which is the same failure the
  // browser check exists for — logic that is correct and never reached.
  {
    // The dispatch. The wire tag is `prepare`; the declared name is
    // `prepare_replacement`. Switching on the declared name here is the
    // mistake that produces a silent no-op, and it is invisible to every test
    // that calls the handler directly.
    const h = stallHarness(), player = stalledPlayer();
    player.mediaAttachment = {}; player.controlIntentGeneration = 0;
    h.answerWith(() => PREPARE_FIXTURE);
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await settleExchange();
    assert.equal(h.stub.prepared().length, 1,
      "a `prepare` answer reaches the player's preparation state machine");
    assert.equal(h.stub.prepared()[0].action_id, PREPARE_ACTION_ID);
    assert.equal(h.stub.prepared()[0].type, "prepare");
    h.stub.detach(player);
  }
  {
    // The settlement. An acknowledgement the server accepted is spent; one
    // whose exchange failed stays queued for the next.
    const h = stallHarness(), player = stalledPlayer();
    player.mediaAttachment = {}; player.controlIntentGeneration = 0;
    h.snapshotWith((_v, p) => Object.assign(snapshot(), {
      acknowledgement: (p.controlAcknowledgements || [])[0] || null,
    }));
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await settleExchange();
    // Queued after the attach on purpose: starting the reporter clears the
    // queue, because a settlement belongs to the control stream it was earned
    // on and a new one has never heard of it.
    player.controlAcknowledgements = [{ action_id: PREPARE_ACTION_ID, state: "metadata_ready" }];
    h.stub.attachedAgain(player);
    await settleExchange();
    const carried = h.sent.find((request) => request.acknowledgement);
    assert.ok(carried, "the queued settlement rides an exchange");
    assert.equal(carried.acknowledgement.state, "metadata_ready");
    assert.equal(player.controlAcknowledgements.length, 0,
      "…and an accepted one is spent rather than sent forever");
    h.stub.detach(player);
  }
  {
    // A live reporter must not wait for ordinary cadence after the synthetic
    // active commit. The accepted callback spends it and immediately wakes the
    // true end snapshot as the next sequence.
    const h = stallHarness(), player = stalledPlayer();
    player.mediaAttachment = {}; player.controlIntentGeneration = 0;
    let closing = false;
    h.snapshotWith((_v, p) => {
      const value = snapshot(55_000, closing ? "ended" : "rendering");
      value.demand = closing ? "end" : "active";
      value.acknowledgement = (p.controlAcknowledgements || [])[0] || null;
      if (value.acknowledgement?.state === "committed") value.demand = "active";
      return value;
    });
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await flush(); await flush();
    const before = h.sent.length;
    player.controlAcknowledgements = [{ action_id: PREPARE_ACTION_ID, state: "committed",
      first_frame_unix_ms: 1_757_000_000_000, committed_media_origin_ms: 0 }];
    closing = true;
    h.stub.attachedAgain(player);
    await settleExchange();
    h.fire();
    await flush(); await flush();
    assert.equal(h.sent[before].demand, "active");
    assert.equal(h.sent[before].acknowledgement.state, "committed");
    await settleExchange();
    h.fire();
    await flush(); await flush();
    assert.equal(h.sent[before + 1].demand, "end");
    assert.equal(h.sent[before + 1].acknowledgement, null);
    assert.equal(h.sent[before + 1].sequence, h.sent[before].sequence + 1);
    h.stub.detach(player);
  }
  {
    // The teardown flush. The reporter's own capture refuses once the player
    // stops owning the media — which is the state every teardown path is in —
    // so a queued `aborted` reaches the wire only because `stopPlaybackControl`
    // builds the capture itself and lets the reporter live long enough to send
    // it. Stopping in the same turn aborts the request in flight.
    const h = stallHarness(), player = stalledPlayer();
    player.mediaAttachment = {}; player.controlIntentGeneration = 0;
    h.snapshotWith((_v, p) => Object.assign(snapshot(), {
      acknowledgement: (p.controlAcknowledgements || [])[0] || null,
    }));
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await settleExchange();
    const before = h.sent.length;
    player.prepared = { actionId: PREPARE_ACTION_ID, sessionId: PREPARE_SESSION, state: "buffer_ready" };
    player.pendingMediaChange = {};        // the player no longer owns the media
    h.stub.detach(player);
    await settleExchange();
    const settled = h.sent.slice(before).find((request) => request.acknowledgement);
    assert.ok(settled, "a teardown settles the staging rather than leaving it to the deadline");
    assert.equal(settled.acknowledgement.state, "aborted");
    assert.equal(settled.acknowledgement.action_id, PREPARE_ACTION_ID);
    assert.equal(settled.demand, "end",
      "a non-commit settlement closes in the same retryable exchange");
  }
  {
    // A commit cannot share end, and a stopping reporter cannot rely on its
    // ordinary cadence (the server may set that to a minute). A lost first
    // response retries the exact active commit, then an accepted response
    // immediately queues end as the next sequence before stopping.
    const h = stallHarness();
    const player = stalledPlayer();
    player.mediaAttachment = {}; player.controlIntentGeneration = 0;
    let closing = false;
    h.snapshotWith((_v, p) => {
      const value = snapshot(55_000, closing ? "ended" : "rendering");
      value.demand = closing ? "end" : "active";
      value.acknowledgement = (p.controlAcknowledgements || [])[0] || null;
      if (value.acknowledgement && value.acknowledgement.state === "committed") {
        value.demand = "active";
      }
      return value;
    });
    let dropped = false;
    let droppedEnd = false;
    h.holdWith((request) => {
      if (request.acknowledgement?.state === "committed" && !dropped) {
        dropped = true;
        return Promise.reject(Object.assign(new Error("connection reset after request"),
          { retryAfterMs: 250 }));
      }
      if (request.demand === "end" && !droppedEnd) {
        droppedEnd = true;
        return Promise.reject(Object.assign(new Error("connection reset after end"),
          { retryAfterMs: 250 }));
      }
      return null;
    });
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await flush(); await flush();
    const before = h.sent.length;
    player.controlAcknowledgements = [{ action_id: PREPARE_ACTION_ID, state: "committed",
      first_frame_unix_ms: 1_757_000_000_000, committed_media_origin_ms: 0 }];
    closing = true;
    h.stub.detach(player);
    await settleExchange();
    h.fire();
    await flush(); await flush();
    assert.equal(h.sent.length, before + 1, "the stopping reporter sends the active commit first");
    const firstCommit = h.sent[before];
    assert.equal(firstCommit.demand, "active");
    assert.equal(firstCommit.acknowledgement.state, "committed");
    await settleExchange();
    h.fire();
    await flush(); await flush();
    const retriedCommit = h.sent[before + 1];
    assert.strictEqual(retriedCommit, firstCommit,
      "a missing response retries the exact commit request and sequence");
    await settleExchange();
    h.fire();
    await flush(); await flush();
    const end = h.sent[before + 2];
    assert.equal(end.sequence, firstCommit.sequence + 1);
    assert.equal(end.demand, "end");
    assert.equal(end.acknowledgement, null);
    await settleExchange();
    h.fire();
    await flush(); await flush();
    assert.strictEqual(h.sent[before + 3], end,
      "a missing terminal response retries the exact end request too");
    assert.deepEqual(h.sent.slice(before).map((request) => request.demand),
      ["active", "active", "end", "end"], "commit retry completes before terminal demand");
  }
  {
    // Stopping can race an ordinary exchange that was already in flight. Its
    // response must not stop the reporter and drop the queued settlement; it
    // merely clears the lane so commit, then end, can drain behind it.
    const held = deferred(), h = stallHarness(), player = stalledPlayer();
    player.mediaAttachment = {}; player.controlIntentGeneration = 0;
    let closing = false, heldOrdinary = false;
    h.snapshotWith((_v, p) => {
      const value = snapshot(55_000, closing ? "ended" : "rendering");
      value.demand = closing ? "end" : "active";
      value.acknowledgement = (p.controlAcknowledgements || [])[0] || null;
      if (value.acknowledgement?.state === "committed") value.demand = "active";
      return value;
    });
    h.holdWith((request) => {
      if (!heldOrdinary && !request.acknowledgement) {
        heldOrdinary = true;
        return held.promise;
      }
      return null;
    });
    h.stub.attach(player, stalledVideo, bootstrap());
    h.attached.push(player);
    await flush(); await flush();
    const ordinary = h.sent[0];
    player.controlAcknowledgements = [{ action_id: PREPARE_ACTION_ID, state: "committed",
      first_frame_unix_ms: 1_757_000_000_000, committed_media_origin_ms: 0 }];
    closing = true;
    h.stub.detach(player);
    assert.equal(h.sent.length, 1, "the settlement waits behind the in-flight request");
    held.resolve(response(ordinary));
    await flush(); await flush();
    await settleExchange();
    h.fire();
    await flush(); await flush();
    assert.equal(h.sent[1].acknowledgement.state, "committed",
      "the ordinary response leaves the queued commit intact");
    await settleExchange();
    h.fire();
    await flush(); await flush();
    assert.equal(h.sent[2].demand, "end");
    assert.deepEqual(h.sent.map((request) => [request.demand,
      request.acknowledgement?.state || null]),
    [["active", null], ["active", "committed"], ["end", null]],
    "the in-flight request, commit, and end retain protocol order");
  }
  {
    // Permanent transport failure during teardown has a hard ownership
    // deadline. Drive the shipped callback with a fake clock at that deadline:
    // the detached reporter must clear its timer and every retained marker
    // instead of retrying a player closure forever.
    const boundedFinalizer = new Function("reporter", [
      shippedConst("PREPARED_FINALIZATION_MS"),
      shippedSource("finishStoppingPlaybackControl"),
      shippedSource("continueStoppingPlaybackControl"),
      "return {bound:PREPARED_FINALIZATION_MS,handled:continueStoppingPlaybackControl("+
        "null,reporter,null,{demand:'active',acknowledgement:{state:'committed'}},null,null)};",
    ].join("\n"));
    const cleared = [];
    let stopped = 0;
    const reporter = {
      plurxSettling: true,
      plurxSettlingUntil: 3_000,
      plurxSettlingTimer: 17,
      now: () => 3_000,
      clearTimer: (timer) => cleared.push(timer),
      stop: () => { stopped += 1; },
    };
    const result = boundedFinalizer(reporter);
    assert.equal(result.bound, 3_000);
    assert.equal(result.handled, true);
    assert.deepEqual(cleared, [17]);
    assert.equal(stopped, 1, "permanent outage cannot retain a detached reporter");
    assert.equal(reporter.plurxSettling, false);
    assert.equal(reporter.plurxSettlingUntil, null);
    assert.equal(reporter.plurxSettlingTimer, null);
  }
  {
    // The snapshot seam: the shipped snapshot builder carries the queue's head,
    // and splits a same-turn commit+end into the required active commit first.
    const queued = Object.assign({}, player, {
      controlAcknowledgements: [{ action_id: PREPARE_ACTION_ID, state: "buffer_ready",
        buffered_through_ms: 42_000 }],
    });
    const carried = adapter.playbackControlSnapshot(video, queued);
    assert.equal(carried.acknowledgement.state, "buffer_ready",
      "a queued settlement reaches the snapshot the reporter serializes");
    assert.equal(JSON.parse(JSON.stringify(carried)).acknowledgement.buffered_through_ms, 42_000);
    const ending = Object.assign({}, player, {
      wantsPlayback: false,
      controlAcknowledgements: [{ action_id: PREPARE_ACTION_ID, state: "committed",
        first_frame_unix_ms: 1_757_000_000_000, committed_media_origin_ms: 0 }],
    });
    // Ended AND within the completion slack, which is what `end` means here: a
    // truncated stream is active failed demand, not the end of the title.
    const endingVideo = Object.assign({}, video, { ended: true, currentTime: 55 });
    const endSnapshot = adapter.playbackControlSnapshot(endingVideo, ending);
    assert.equal(endSnapshot.demand, "active",
      "the commit is published before terminal demand closes the new session");
    assert.equal(endSnapshot.playback_rate, 1);
    assert.equal(endSnapshot.render_state, "ended",
      "the synthetic demand does not falsify the renderer observation");
    assert.equal(endSnapshot.acknowledgement.state, "committed");
    assert.ok(control.capture(endSnapshot, 0,
      { lifecycleId: "commit-before-end", attachmentGeneration: 1 }),
      "the synthetic active commit is accepted by the shipped wire validator");
    const afterCommit = adapter.playbackControlSnapshot(endingVideo,
      Object.assign({}, ending, { controlAcknowledgements: [] }));
    assert.equal(afterCommit.demand, "end",
      "once the commit is spent, the next snapshot carries terminal demand");
    assert.equal(afterCommit.acknowledgement, null);
  }

  // ---- what the commit owes, and what it must not break -------------------
  {
    // A commit carries the origin it was OFFERED, verbatim. The server drops a
    // commit naming a different one — silently — and a VOD successor is the
    // case where the two numbers differ: it aligns against zero, because a VOD
    // playlist is the whole immutable title, while the offer still names the
    // resume position.
    const h = preparedHarness();
    const p = h.set(preparedPlayer({ vod: true, offset: 0,
      hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.live.currentTime = 900;
    const state = h.handle(prepareAction({ media_origin_ms: PREPARE_ORIGIN_MS }));
    assert.equal(state.mediaOriginMs, 0,
      "a VOD successor's own zero is the source's zero — adding the resume "
        + "position would seek it twice");
    assert.equal(state.offeredOriginMs, PREPARE_ORIGIN_MS);
    assert.equal(h.instances[0].config.startPosition, 900,
      "so it is primed at the film position directly");
    h.instances[0].events.manifest();
    h.spare.currentTime = 900;
    h.spare.ranges = [[900, 930]];
    h.instances[0].events.append();
    h.spare.frameCallback();
    assert.equal(latest(h).state, "committed");
    assert.equal(latest(h).committed_media_origin_ms, PREPARE_ORIGIN_MS,
      "the commit echoes the offer, not the number this client aligned against");
    assert.equal(control.validAcknowledgement(latest(h), "active"), true,
      "…and the body the server would accept");
  }
  {
    // The commit-time corrective seek, on the hls.js path. Without it the
    // successor lands wherever it drifted to while it was priming.
    const h = preparedHarness();
    const p = h.set(preparedPlayer({ vod: false, offset: 600,
      hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.live.currentTime = 300;                 // film 900 s
    h.handle(prepareAction({ media_origin_ms: PREPARE_ORIGIN_MS }));
    h.instances[0].events.manifest();
    h.spare.currentTime = 306;                // six seconds of drift while priming
    h.spare.ranges = [[306, 330]];
    h.live.currentTime = 301;                 // film 901 s
    h.instances[0].events.append();
    assert.equal(h.spare.currentTime, 301,
      "the successor is put on the incumbent's second before the picture changes");
  }
  {
    // Readiness is measured through the range that CONTAINS the successor's
    // playhead. A disjoint range further ahead is not runway; it is a gap the
    // decoder stops at.
    const h = preparedHarness();
    const p = h.set(preparedPlayer({ hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.handle(prepareAction());
    h.instances[0].events.manifest();
    h.spare.currentTime = 0;
    h.spare.ranges = [[60, 600]];
    h.instances[0].events.append();
    assert.equal(latest(h).state, "metadata_ready",
      "a buffered range the playhead is not inside is not readiness");
    assert.equal(p.prepared.state, "metadata_ready", "…and nothing switched");
    h.spare.ranges = [[0, 30], [60, 600]];
    h.instances[0].events.append();
    assert.equal(p.prepared, null, "the range containing the playhead is the one that counts");
  }
  {
    // A transient stall on the successor is not a failed preparation.
    const h = preparedHarness();
    const p = h.set(preparedPlayer({ hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.handle(prepareAction());
    h.instances[0].events.error(null, { fatal: false, details: "bufferStalledError" });
    assert.equal(latest(h), null, "a non-fatal error settles nothing");
    assert.ok(p.prepared, "…and does not free the slot");
    h.instances[0].events.error(null, { fatal: true, details: "manifestLoadError" });
    assert.equal(latest(h).state, "failed");
  }
  {
    // A terminal settlement outranks a progress one for the same staging.
    const h = preparedHarness();
    const p = h.set(preparedPlayer({ hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.handle(prepareAction());
    const state = p.prepared;
    h.instances[0].events.error(null, { fatal: true, details: "manifestLoadError" });
    assert.equal(latest(h).state, "failed");
    // The manifest event that was already queued when the pipeline died.
    h.instances[0].events.manifest();
    assert.equal(latest(h).state, "failed",
      "a `failed` is not overwritten by progress that was already in flight");
  }
  {
    // Audio-only: `requestVideoFrameCallback` exists and never fires, so a
    // healthy handoff would sit out the watchdog and report `failed`.
    const h = preparedHarness({ hasVideo: false });
    const p = h.set(preparedPlayer({ source: {}, hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.handle(prepareAction());
    h.instances[0].events.manifest();
    h.spare.ranges = [[0, 30]];
    h.instances[0].events.append();
    assert.equal(latest(h).state, "buffer_ready", "no commit before the picture — or the sound — moves");
    h.spare.emit("timeupdate");
    assert.equal(latest(h).state, "committed",
      "an advancing clock is the first-frame evidence an audio-only title has");
  }
  {
    // A video handoff without a presented-frame callback is not safely
    // observable. A timeupdate only proves the media clock advanced; it does
    // not prove the successor ever reached the screen.
    const h = preparedHarness({ videoFrameCallback: false });
    const p = h.set(preparedPlayer({ hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.handle(prepareAction());
    h.instances[0].events.manifest();
    h.spare.ranges = [[0, 30]];
    h.instances[0].events.append();
    h.fireAll();
    assert.equal(latest(h).state, "failed",
      "video preparation fails closed when presented-frame evidence is unavailable");
    h.spare.emit("timeupdate");
    assert.equal(latest(h).state, "failed", "timeupdate alone can never commit video");
    assert.equal(p.prepared, null, "the unobservable successor is released");
  }

  // ---- the element the page is talking to ---------------------------------
  {
    const h = preparedHarness();
    const p = h.set(preparedPlayer({ hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.handle(prepareAction());
    const successor = h.spare;
    h.instances[0].events.manifest();
    h.spare.ranges = [[0, 30]];
    h.instances[0].events.append();
    // The listener set and the per-playback bindings move to the element that
    // now owns the picture. Without this the scrubber freezes, the stall
    // watchdog goes deaf, the decode rescue goes silent and the end of the
    // film does nothing — for the rest of the page's life, because `wirePlayer`
    // runs once.
    assert.deepEqual(h.wired, [successor], "the media listener set follows the picture");
    for (const binding of ["hitch", "airplay", "render"]) {
      assert.ok(h.adopted.some((entry) => entry[0] === binding),
        `the switch re-binds ${binding}`);
    }
    assert.equal(h.intervals.length, 1, "the progress tick is re-armed");
    h.intervals[0].fn();
    assert.deepEqual(h.adopted.filter((entry) => entry[0] === "progress"), [["progress", successor]],
      "…on the element that now owns the picture, not the retired one");
    assert.equal(typeof successor.onended, "function");
    successor.onended();
    assert.ok(h.adopted.some((entry) => entry[0] === "ended"),
      "and the end of the film is handled on it — autoplay-next and watched "
        + "state both hang off this one listener");
    assert.equal(successor.plays >= 1, true,
      "and it is asked to play again after being un-muted — WebKit pauses an "
        + "element that loses its mute without a gesture");
    // The predecessor stays available until the successor proves a frame.
    assert.deepEqual(h.removed, [], "nothing is disposed during the rollback window");
    assert.equal(h.live.parentNode !== null, true);
    successor.frameCallback();
    // The retired element then leaves the page. Keeping it as the next
    // successor's host would have a hidden pipeline driving the visible
    // stream's overlays.
    assert.deepEqual(h.removed, [h.live], "first-frame proof disposes the predecessor");
    assert.equal(h.live.parentNode, null);
    const second = "77777777-7777-4777-8777-777777777777";
    h.handle(prepareAction({ action_id: second }));
    assert.notEqual(h.spare, h.live, "a second preparation never restages the retired element");
    assert.equal(h.created.length, 2, "it gets a clean one");
  }
  {
    // A commit whose first frame never arrives, on a player that goes away
    // first. The watchdog is the only thing still holding that staging, and
    // left armed it files a failure against a session that ended for an
    // unrelated reason — on a player whose queue has already been cleared.
    const h = preparedHarness();
    const incumbent = { bandwidthEstimate: 1, destroyed: false,
      destroy() { this.destroyed = true; } };
    const p = h.set(preparedPlayer({ hls: incumbent }));
    h.handle(prepareAction());
    const successor = h.instances[0];
    h.instances[0].events.manifest();
    h.spare.ranges = [[0, 30]];
    h.instances[0].events.append();
    assert.ok(p.preparedCommitting, "the staging is still this player's until a frame settles");
    assert.equal(h.timers.size, 1, "…and the watchdog is armed");
    h.cancelFrame();
    assert.equal(p.preparedCommitting, null);
    assert.equal(h.timers.size, 0, "a teardown disarms it");
    assert.equal(p.hls, incumbent, "cancelling the proof window restores the predecessor");
    assert.equal(incumbent.destroyed, false);
    assert.equal(successor.destroyed, true);
    h.fireAll();
    assert.equal(latest(h).state, "failed",
      "the cancelled, unproven switch has one honest terminal settlement");
    h.created[0].frameCallback();
    assert.equal(latest(h).state, "failed",
      "a stale frame callback cannot turn the cancelled switch into a commit");
  }
  {
    // A full media teardown during the proof window owns both pipelines. It
    // settles failure, discards the successor through rollback, then tears
    // down the restored incumbent through the ordinary path.
    const h = preparedHarness();
    const incumbent = { bandwidthEstimate: 1, destroyed: false,
      destroy() { this.destroyed = true; } };
    const p = h.set(preparedPlayer({ hls: incumbent }));
    h.handle(prepareAction());
    const successor = h.instances[0];
    h.instances[0].events.manifest();
    h.spare.ranges = [[0, 30]];
    h.instances[0].events.append();
    h.teardown();
    assert.equal(latest(h).state, "failed");
    assert.equal(successor.destroyed, true, "teardown destroys the unproven successor");
    assert.equal(incumbent.destroyed, true, "teardown also destroys the retained predecessor");
    assert.equal(p.hls, null);
  }
  {
    // A staging this client settled is replayed until the server processes the
    // acknowledgement. Answering the replay by building it again would put a
    // third pipeline on the element the viewer is watching.
    const h = preparedHarness();
    const p = h.set(preparedPlayer({ hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.handle(prepareAction());
    h.abandon("aborted", "the viewer seeked");
    assert.equal(latest(h).state, "aborted");
    assert.equal(h.handle(prepareAction()), null, "a settled staging is not rebuilt");
    assert.equal(h.instances.length, 1);
    assert.equal(p.prepared, null);
    // §3.3 row 18. The incumbent never moved — the viewer is watching the same
    // picture they were — so the abandonment gets a ledger row and no surface.
    assert.deepEqual(
      h.surfaceRaised.map((entry) => entry.source),
      ["log_only"],
      "an abandoned successor is log-only, and it is not nothing",
    );
    assert.equal(h.surfaceRaised[0].fault.attached, p.attemptId ?? null,
      "the row names the generation the incumbent is on");
  }

  {
    // …and so is a successor that failed on its own: `playFailure`'s web twin
    // would have been a full screen over a stream that never stopped.
    const h = preparedHarness();
    const p = h.set(preparedPlayer({ attemptId: "a4", hls: { bandwidthEstimate: 1, destroy() {} } }));
    h.handle(prepareAction());
    h.failPrepared("the successor element reported an error");
    assert.equal(latest(h).state, "failed");
    assert.deepEqual(h.surfaceRaised.map((entry) => entry.source), ["log_only"]);
    assert.equal(h.surfaceRaised[0].fault.attached, "a4");
    assert.equal(p.prepared, null);
  }


  // ---- M5: the three bounded recovery additions -----------------------------
  //
  // All three run the SHIPPED owner sliced out of index.html — the retry, its
  // ladder, its absolute deadline and the shared hls.js budget are the page's
  // own code here, so a mutation that deletes one fails these rather than a
  // copy of it.
  function createRetryHarness() {
    const policy = require("../../crates/plurxd/src/web/playback-policy.js");
    return new Function("PlaybackPolicy", [
      "let PLAYER={attemptId:'a1',started:false,method:'transcode'};",
      "let now=0;const timers=new Map();let nextTimer=0;",
      "const performance={now:()=>now};",
      "function setTimeout(fn,ms){const id=++nextTimer;timers.set(id,{fn,at:now+ms});return id;}",
      "function clearTimeout(id){timers.delete(id);}",
      // Every create this sequence sends, with the identity it sent and when.
      "const posts=[],released=[],raised=[],stops=[],logs=[];const answers=[];",
      "function newRequestId(){return 'rq-'+(posts.length+1);}",
      "function clientLog(entry){logs.push(entry);}function playbackContext(){return {};}",
      "function releaseSession(id){released.push(id);}",
      "function stopPlayerForExhaustion(){stops.push(now);}",
      "function raisePlaybackSurface(source,fault,options){",
      "  if(options&&options.once&&raised.some(entry=>entry.source===source)) return null;",
      "  raised.push({source,at:now,player_stopped:!!(fault&&fault.player_stopped),",
      "    actions:(fault&&fault.actions)||null,context:fault&&fault.context,",
      "    title:fault&&fault.title,detail:fault&&fault.detail});return null;}",
      // The server: one scripted answer per attempt. `hold` never settles until
      // the harness settles it, which is how a slow server is expressed.
      "function openSession(fileId,opts,signal,requestId){",
      "  const index=posts.length;posts.push({requestId,at:now});",
      "  const answer=answers[index]||answers[answers.length-1];",
      "  if(answer&&answer.hold) return new Promise(resolve=>{answer.settle=resolve;});",
      "  if(answer&&answer.error) return Promise.reject(answer.error);",
      "  return Promise.resolve({session_id:'session-'+(index+1)});}",
      shippedSource("playbackRetryDelay"),
      shippedSource("playbackCreateRetryContext"),
      // Ruling 2: the sequence has no absolute bound of its own any more — it
      // runs inside the SHIPPED preparation owner, whose 20 s is the bound, and
      // the honest ending is the one that owner's deadline produces. Slicing
      // the owner in is what makes that a test of the wiring rather than of a
      // copy of it.
      shippedSource("beginPlaybackPreparation"),
      shippedSource("openSessionRetryingNotYet"),
      "const preparations=[];",
      "return {posts,released,raised,stops,logs,answers,now:()=>now,player:()=>PLAYER,",
      "  setPlayer(p){PLAYER=p;},context:()=>playbackCreateRetryContext(),",
      "  bare(options){return openSessionRetryingNotYet(7,{start:0},null,options);},",
      "  open(options){const preparation=beginPlaybackPreparation(()=>true);",
      "    preparations.push(preparation);",
      "    return preparation.run(signal=>openSessionRetryingNotYet(7,{start:0},signal,",
      "      Object.assign({preparation},options)),late=>releaseSession(late&&late.session_id));},",
      "  cancel(){preparations[preparations.length-1].cancel();},",
      "  settle(index,info){const answer=answers[index];if(answer&&answer.settle)answer.settle(info);},",
      "  advance(ms){now+=ms;for(const [id,timer] of [...timers])if(timer.at<=now){timers.delete(id);timer.fn();}}};",
    ].join("\n"))(policy);
  }

  const refusal503 = (code) => Object.assign(new Error("still building"), {
    status: 503, code,
    streamFailure: { status: 503, code, message: "the transcoder is still starting", position_ms: null },
  });

  {
    // m5_create_retry_deadline, on the client it applies to: the ladder is
    // 1 s · 2 s · 4 s under ONE request identity, and the surface is
    // `preparing` for the whole of it.
    const h = createRetryHarness();
    for (const code of ["startup_timeout", "media_owner_transition", "vod_index_pending", "vod_engine_unattested"]) {
      h.answers.push({ error: refusal503(code) });
    }
    h.answers.push({ error: refusal503("startup_timeout") });
    const opening = h.open({ context: "start" });
    let settled = null;
    opening.then((value) => { settled = { value }; }, (error) => { settled = { error }; });
    await flushDeep();
    assert.equal(h.posts.length, 1, "the first attempt is immediate");
    for (const [delay, attempts] of [[1000, 2], [2000, 3], [4000, 4]]) {
      h.advance(delay - 1); await flushDeep();
      assert.equal(h.posts.length, attempts - 1, `nothing is re-posted before ${delay}ms`);
      h.advance(1); await flushDeep();
      assert.equal(h.posts.length, attempts, `the ladder's next rung fires at ${delay}ms`);
    }
    assert.deepEqual(
      h.posts.map((post) => post.requestId),
      ["rq-1", "rq-1", "rq-1", "rq-1"],
      "every attempt replays the SAME request identity",
    );
    assert.deepEqual(h.posts.map((post) => post.at), [0, 1000, 3000, 7000],
      "1 s · 2 s · 4 s, and the backoff does not restart");
    await flushDeep();
    assert.equal(h.raised.filter((entry) => entry.source === "create_503_not_yet").length, 1,
      "one `preparing` fault for the sequence, not one per attempt");
    assert.equal(h.stops.length, 1, "the owner stops the player before it raises the prompt");
    const exhausted = h.raised.filter((entry) => entry.source === "owner_exhausted");
    assert.equal(exhausted.length, 1);
    assert.equal(exhausted[0].player_stopped, true, "`exhausted` carries the owner's stop");
    // Ruled 2026-09-13: the two STALL prompts lead with Keep waiting; this one
    // does not, because nothing is attached and there is no detector to re-arm.
    assert.deepEqual(exhausted[0].actions, ["retry", "close"]);
    assert.equal(h.posts.length, 4, "the ladder is three retries and stops");
    assert.ok(settled && settled.error, "the caller is told the sequence is over");
    assert.equal(settled.error.surfaceRaised, true,
      "the owner already raised, so `failPreparation` must not raise again");
  }
  {
    // Ruling 2. The bound is the PREPARATION owner's 20 s — a pre-existing
    // threshold §6 forbids moving — and what it produces is now the sequence's
    // own exhaustion with the server's own sentence, instead of the generic
    // "Playback could not prepare." a bare `AbortError` used to produce.
    const h = createRetryHarness();
    h.answers.push({ error: refusal503("startup_timeout") }, { hold: true });
    const opening = h.open({ context: "start" });
    let settled = null;
    opening.then((value) => { settled = { value }; }, (error) => { settled = { error }; });
    await flushDeep();
    h.advance(1000); await flushDeep();
    assert.equal(h.posts.length, 2, "the first refusal put the sequence on its ladder");
    h.advance(18_999); await flushDeep();
    assert.equal(h.stops.length, 0, "nothing is raised one millisecond early");
    h.advance(1); await flushDeep();
    assert.equal(h.stops.length, 1, "the owner stops the player on the preparation clock");
    const exhausted = h.raised.filter((entry) => entry.source === "owner_exhausted");
    assert.equal(exhausted.length, 1, "…and raises the exhaustion M5 designed");
    assert.equal(exhausted[0].player_stopped, true);
    assert.equal(exhausted[0].detail, "the transcoder is still starting",
      "the viewer reads the server's own sentence, not a generic preparation failure");
    assert.ok(settled && settled.error);
    assert.equal(settled.error.createRetryReason, "preparation_deadline");
    assert.equal(settled.error.surfaceRaised, true,
      "`failPreparation` must leave this prompt standing");
    // …and the create that finally lands is RELEASED, never attached. On the
    // web that is a property of the browser now, not only of this test.
    h.settle(1, { session_id: "late-session" });
    await flushDeep();
    assert.deepEqual(h.released, ["late-session"], "a late success is released, not attached");
    assert.equal(h.raised.filter((entry) => entry.source === "owner_exhausted").length, 1,
      "releasing the late session does not raise a second prompt");
  }
  {
    // A create that is merely SLOW — no "not yet" answer behind it — is not
    // this owner's exhaustion, and must not borrow its sentence. The
    // preparation deadline says what it has always said.
    const h = createRetryHarness();
    h.answers.push({ hold: true });
    const opening = h.open({ context: "start" });
    let settled = null;
    opening.then((value) => { settled = { value }; }, (error) => { settled = { error }; });
    await flushDeep();
    h.advance(20_000); await flushDeep();
    assert.equal(h.stops.length, 0, "nothing was exhausted — nothing had refused");
    assert.equal(h.raised.filter((entry) => entry.source === "owner_exhausted").length, 0);
    assert.ok(settled && settled.error);
    assert.equal(settled.error.name, "TimeoutError");
    assert.equal(settled.error.surfaceRaised, undefined,
      "so `failPreparation` still owns this one");
  }
  {
    // A sequence with no preparation owner is refused rather than left
    // unbounded: deleting M5's own 60 s watchdog is only safe because every
    // create runs inside one.
    const h = createRetryHarness();
    let settled = null;
    await Promise.resolve(h.bare({ context: "start" }))
      .then((value) => { settled = { value }; }, (error) => { settled = { error }; });
    assert.equal(settled.error.name, "TypeError");
    assert.equal(h.posts.length, 0, "and it never reached the wire");
  }
  {
    // A newer intent cancels the whole sequence: the attempt in flight, the
    // sleep between attempts, and the deadline watchdog with them. Nothing is
    // raised — the newer intent owns the surface now.
    const h = createRetryHarness();
    h.answers.push({ error: refusal503("startup_timeout") });
    const opening = h.open({ context: "start" });
    let settled = null;
    opening.then((value) => { settled = { value }; }, (error) => { settled = { error }; });
    await flushDeep();
    assert.equal(h.posts.length, 1);
    h.cancel();
    await flushDeep();
    assert.ok(settled && settled.error, "the sequence ends when its intent does");
    assert.equal(settled.error.name, "AbortError");
    h.advance(120_000); await flushDeep();
    assert.equal(h.posts.length, 1, "a cancelled sequence never posts again");
    assert.equal(h.stops.length, 0, "a cancelled sequence is not an exhausted one");
    assert.equal(h.raised.filter((entry) => entry.source === "owner_exhausted").length, 0);
  }
  {
    // A refusal no "not yet" row claims is not retried at all: the caller's
    // existing handling is what the contract leaves standing.
    const h = createRetryHarness();
    h.answers.push({ error: Object.assign(new Error("gone"), { status: 410, code: "media_owner_lost",
      streamFailure: { status: 410, code: "media_owner_lost", message: "the node is gone", position_ms: 90_000 } }) });
    let settled = null;
    h.open({ context: "start" }).then((value) => { settled = { value }; }, (error) => { settled = { error }; });
    await flushDeep();
    h.advance(120_000); await flushDeep();
    assert.equal(h.posts.length, 1, "only a `create_503_not_yet` answer is retried");
    assert.equal(settled.error.status, 410);
    assert.equal(settled.error.surfaceRaised, undefined, "the caller still owns this failure");
  }
  {
    // A create refused during a pending CHANGE is a refused change, not a
    // retry: row 7 beats row 6 and the predecessor keeps playing.
    const h = createRetryHarness();
    h.answers.push({ error: refusal503("startup_timeout") });
    let settled = null;
    h.open({ context: "change" }).then((value) => { settled = { value }; }, (error) => { settled = { error }; });
    await flushDeep();
    h.advance(120_000); await flushDeep();
    assert.equal(h.posts.length, 1, "the change context is not retried");
    assert.equal(h.stops.length, 0);
    assert.ok(settled.error);
  }
  {
    // A call site that answers a code better than waiting keeps its answer.
    const h = createRetryHarness();
    h.answers.push({ error: refusal503("vod_index_pending") });
    let settled = null;
    h.open({ context: "start",
      answeredLocally: (failure) => failure.code === "vod_index_pending" })
      .then((value) => { settled = { value }; }, (error) => { settled = { error }; });
    await flushDeep();
    h.advance(120_000); await flushDeep();
    assert.equal(h.posts.length, 1, "a locally answered refusal is not retried");
    assert.equal(settled.error.code, "vod_index_pending");
  }
  {
    // A sequence that succeeds on a retry attaches that session and releases
    // nothing.
    const h = createRetryHarness();
    h.answers.push({ error: refusal503("startup_timeout") }, { error: null });
    let settled = null;
    h.open({ context: "start" }).then((value) => { settled = { value }; }, (error) => { settled = { error }; });
    await flushDeep();
    h.advance(1000); await flushDeep();
    assert.equal(h.posts.length, 2);
    assert.equal(settled.value.session_id, "session-2");
    assert.deepEqual(h.released, []);
    assert.equal(h.stops.length, 0, "a sequence that worked never stopped the player");
    h.advance(120_000); await flushDeep();
    assert.equal(h.raised.filter((entry) => entry.source === "owner_exhausted").length, 0,
      "the deadline watchdog is disarmed when the sequence settles");
  }

  // ---- M5 addition 2: ONE hls.js retry per attach ----------------------------
  function hlsRetryHarness() {
    const policy = require("../../crates/plurxd/src/web/playback-policy.js");
    return new Function("PlaybackPolicy", [
      "let now=0;const timers=new Map();let nextTimer=0;const loads=[],logs=[];",
      "function setTimeout(fn,ms){const id=++nextTimer;timers.set(id,{fn,at:now+ms});return id;}",
      "function clientLog(entry){logs.push(entry);}function playbackContext(){return {};}",
      "const hls={startLoad(at){loads.push({at,now});}};",
      "const video={currentTime:0};",
      "let PLAYER={attemptId:'a1',hls,hlsRetryUsed:0};",
      shippedSource("scheduleHlsNetworkRetry"),
      "return {loads,logs,player:()=>PLAYER,video,hls,",
      "  retry(detail){return scheduleHlsNetworkRetry(video,PLAYER,detail);},",
      "  seekTo(seconds){video.currentTime=seconds;},",
      "  reattach(){PLAYER={attemptId:'a2',hls,hlsRetryUsed:0};},",
      "  replaceInstance(){PLAYER.hls={startLoad(){loads.push({at:'wrong-instance'});}};},",
      "  advance(ms){now+=ms;for(const [id,timer] of [...timers])if(timer.at<=now){timers.delete(id);timer.fn();}}};",
    ].join("\n"))(policy);
  }
  {
    // m5_hls_retry_once: one reload, two seconds later, at the position the
    // picture actually reached — and at most ONE per attach however many
    // fatals arrive.
    const h = hlsRetryHarness();
    assert.equal(h.retry("network"), true, "the first fatal spends the attach's budget");
    h.advance(1999);
    assert.deepEqual(h.loads, [], "the reload waits its two seconds");
    h.seekTo(412.5);
    h.advance(1);
    assert.deepEqual(h.loads, [{ at: 412.5, now: 2000 }],
      "the reload carries the position the element is at, not zero");
    assert.equal(h.retry("segment-503"), false,
      "the `segment_503_not_yet` row shares the SAME per-attach budget");
    assert.equal(h.retry("network"), false, "and so does a second network fatal");
    h.advance(60_000);
    assert.equal(h.loads.length, 1, "at most one startLoad per attach");
  }
  {
    // The budget is per ATTACH: a new attach gets its own single retry, and a
    // retry scheduled by the attach before it never touches the new instance.
    const h = hlsRetryHarness();
    assert.equal(h.retry("network"), true);
    h.reattach();
    h.advance(2000);
    assert.deepEqual(h.loads, [], "a retry whose attach is gone does not reload the new one");
    assert.equal(h.retry("network"), true, "the new attach has its own budget");
    h.advance(2000);
    assert.equal(h.loads.length, 1);
  }
  {
    // A teardown between the fatal and the reload leaves the element alone.
    const h = hlsRetryHarness();
    assert.equal(h.retry("network"), true);
    h.replaceInstance();
    h.advance(2000);
    assert.deepEqual(h.loads, [], "the reload belongs to the instance that failed");
  }

  {
    // The row 6 / row 7 boundary, computed by the SHIPPED function rather than
    // supplied by every case. Answer "start" over an attached predecessor and a
    // create refusal gets four retries and then `stopPlayerForExhaustion()` on
    // a stream the viewer is watching.
    const h = createRetryHarness();
    assert.equal(h.context(), "start", "a cold start has nothing behind it");
    h.setPlayer({ attemptId: "a1", started: true });
    assert.equal(h.context(), "change", "an attached predecessor makes this a refused change");
    h.setPlayer({ attemptId: "a2", started: false, mediaPredecessor: { started: true } });
    assert.equal(h.context(), "change", "…and the PREDECESSOR answers, not the incoming player");
    h.setPlayer({ attemptId: "a3", started: false, mediaPredecessor: { started: false } });
    assert.equal(h.context(), "start");
    // …and the sequence uses it when no context is supplied: a refused change
    // is not retried, and nobody is stopped.
    h.setPlayer({ attemptId: "a4", started: true });
    h.answers.push({ error: refusal503("startup_timeout") });
    let settled = null;
    h.open({}).then((value) => { settled = { value }; }, (error) => { settled = { error }; });
    await flushDeep();
    h.advance(120_000);
    await flushDeep();
    assert.equal(h.posts.length, 1, "a create over an attached predecessor is not retried");
    assert.equal(h.stops.length, 0, "and the predecessor is never stopped");
    assert.ok(settled && settled.error);
  }
  {
    // The line that makes the hls.js budget per ATTACH, exercised through the
    // SHIPPED `attachHls` rather than a hand-made PLAYER.
    const h = fullOpenHarness();
    const p = fullPlayer();
    h.attach(p);
    h.attachHls();
    assert.equal(p.hlsRetryUsed, 0, "an attach arrives with a fresh budget");
    assert.equal(h.spendHlsRetry(), true);
    assert.equal(p.hlsRetryUsed, 1);
    assert.equal(h.spendHlsRetry(), false, "one retry per attach");
    h.attachHls();
    assert.equal(p.hlsRetryUsed, 0, "the shipped attach restores the budget");
    assert.equal(h.spendHlsRetry(), true, "…and the new attach gets its own single retry");
  }
  {
    // `failPreparation` must not raise over a fault the create-retry owner has
    // already raised: without its `surfaceRaised` guard an `owner_stopped`
    // lands on top of the owner's `exhausted` prompt and tells the viewer a
    // different story about the same failure.
    const notYet = () => Object.assign(new Error("still building"), {
      status: 503, code: "startup_timeout",
      streamFailure: { status: 503, code: "startup_timeout", message: "the transcoder is still starting", position_ms: null },
    });
    // A COLD start: the ladder is the `start` context and nothing else, so
    // there is deliberately no predecessor attached here.
    const h = fullOpenHarness();
    const opening = h.play("cold", "Cold film", 0, 600_000, null);
    await flushDeep();
    h.decisions[0].resolve({ method: "transcode", source: { video_codec: "h264" }, audio: [{ index: 0, default: true }], subtitles: [], ladder: [] });
    await flushDeep();
    assert.equal(h.sessions.length, 1, "the transcode create is issued");
    h.sessions[0].reject(notYet());
    for (const delay of [1_000, 2_000, 4_000]) {
      await flushDeep();
      h.advance(delay);
      await flushDeep();
      h.sessions[h.sessions.length - 1].reject(notYet());
    }
    await flushDeep();
    await opening;
    assert.equal(h.sessions.length, 4, "the shipped ladder is 1 s · 2 s · 4 s through play()");
    const identities = new Set(h.sessions.map((session) => session.requestId));
    assert.equal(identities.size, 1, "every attempt replays ONE request identity");
    assert.ok([...identities][0], "…and it is a real identity, not undefined");
    const raised = h.surface();
    assert.deepEqual(
      raised.filter((entry) => entry.source === "owner_exhausted").length,
      1,
      "the owner raises its prompt exactly once",
    );
    assert.deepEqual(
      raised.filter((entry) => entry.source === "owner_stopped"),
      [],
      "`failPreparation` must not stack `owner_stopped` on the owner's prompt",
    );
    assert.equal(h.stops(), 1, "and the player was stopped before it was raised");
  }

  // Fast state-machine coverage for the shipped startup owner. The stock
  // loader and real-browser cases below are the acceptance evidence; this
  // surrogate deliberately claims only the transitions it executes.
  function startupHarness({ manifestState = "unknown", deadlineMs = 40_000 } = {}) {
    return new Function("PlaybackPolicy", [
      "let now=0,STREAM_FAILURE=null;const timers=new Map();let nextTimer=0;",
      "const sends=[],loads=[],starts=[],logs=[],diagnoses=[];",
      "const performance={now:()=>now};function setTimeout(fn,ms){const id=++nextTimer;timers.set(id,{fn,at:now+ms});return id;}function clearTimeout(id){timers.delete(id);}",
      "function clientLog(entry){logs.push(entry);}function playbackContext(){return {};}",
      "function stallDiagnose(){diagnoses.push('stall');return Promise.resolve();}function reportTtff(){}",
      "function noteStreamFailure(status,body,evidence){const parsed=PlaybackPolicy.parseStreamFailure({status,body});if(!parsed)return null;Object.assign(parsed,{at:Date.now()},evidence||{});STREAM_FAILURE=parsed;return parsed;}",
      "const attachment={current:()=>true};const video={currentTime:91};",
      "const hls={loadSource(url){loads.push({url,at:now});},startLoad(at){starts.push({at,now});},stopLoad(){starts.push({stop:true,now});}};",
      `let PLAYER={hls,hlsRetryUsed:0,wantsPlayback:true,mediaAttachment:attachment,controlIntentGeneration:1,stallTimer:null};`,
      `const episode={player:PLAYER,attachment,playlistUrl:'/captured/index.m3u8',hls,state:'active',manifestState:${JSON.stringify(manifestState)},mediaLoaded:false,startedAt:0,deadlineMs:${deadlineMs},dispatches:0,loaders:new Set(),latestFailure:null,establishedSuspension:null,retry:{state:'unused',dueMs:null,detail:null,timer:null,intentGeneration:null}};PLAYER.hlsStartup=episode;`,
      "class StockLoader{constructor(){this.aborted=0;this.destroyed=0;this.reads=0;}openAndSendXhr(xhr,context){sends.push({xhr,context,at:now});}readystatechange(){this.reads++;}abort(){this.aborted++;}destroy(){this.destroyed++;}}",
      shippedSource("playbackAttemptTerminallyStopped"),shippedSource("hlsStartupCurrent"),shippedSource("hlsStartupIncomplete"),
      shippedSource("hlsStartupManifestRequest"),shippedSource("abortHlsStartupLoaders"),
      shippedSource("cancelHlsStartup"),shippedSource("completeHlsStartup"),
      shippedSource("configureHlsStartupDeadline"),shippedSource("diagnoseHlsStartup"),
      shippedSource("exhaustHlsStartup"),shippedSource("armHlsStartupRetry"),
      shippedSource("pauseHlsStartup"),shippedSource("resumeHlsStartup"),
      shippedSource("boundStreamFailureBody"),shippedSource("decodeStreamFailureBytes"),
      shippedSource("streamFailureResponseBodyNow"),shippedSource("streamFailureResponseBody"),
      shippedSource("observeStreamFailureResponse"),
      shippedSource("createHlsStartupLoader"),shippedSource("scheduleHlsNetworkRetry"),
      "const Loader=createHlsStartupLoader(StockLoader,episode);",
      "return {episode,player:PLAYER,video,sends,loads,starts,logs,diagnoses,",
      " reserve(detail){return scheduleHlsNetworkRetry(video,PLAYER,detail);},",
      " pause(){return pauseHlsStartup(PLAYER);},resume(){return resumeHlsStartup(video,PLAYER);},",
      " complete(){return completeHlsStartup(PLAYER);},cancel(){return cancelHlsStartup(PLAYER,'test');},",
      " loader(){return new Loader({});},",
      " send(loader,status=200,body=''){const xhr={status,responseText:body,statusText:'status',readyState:4,onreadystatechange:null,onprogress:null};loader.loader=xhr;loader.context={type:'manifest',url:'/captured/index.m3u8'};loader.plurxIntentGeneration=PLAYER.controlIntentGeneration||0;loader.callbacks={onError(...args){loader.error=args;}};loader.stats={};loader.openAndSendXhr(xhr,loader.context,{});if(status>=400){xhr.status=status;loader.readystatechange();}return loader;},",
      " advance(ms){now+=ms;for(const [id,timer] of [...timers].sort((a,b)=>a[1].at-b[1].at)){if(timer.at<=now){timers.delete(id);timer.fn();}}},setState(state){episode.state=state;},setManifest(state){episode.manifestState=state;},setDeadline(ms){episode.deadlineMs=ms;},setCurrent(value){attachment.current=()=>value;},setWants(value){PLAYER.wantsPlayback=value;},now:()=>now};",
    ].join("\n"))(require("../../crates/plurxd/src/web/playback-policy.js"));
  }
  {
    const h=startupHarness();
    assert.equal(h.reserve({details:"manifestLoadError"}),true);
    assert.equal(h.episode.retry.state,"reserved");
    h.advance(1_999);assert.equal(h.loads.length,0);
    h.advance(1);assert.deepEqual(h.loads,[{url:"/captured/index.m3u8",at:2_000}],
      "an unloaded manifest reloads the captured URL after the shared delay");
    assert.equal(h.episode.retry.state,"dispatched");
    assert.equal(h.reserve("again"),false,"the corrective credit is shared and one-shot");
  }
  {
    const h=startupHarness({manifestState:"parsed"});
    assert.equal(h.reserve("fragLoadError"),true);h.advance(2_000);
    assert.deepEqual(h.starts,[{at:91,now:2_000}],
      "an established stream resumes at the current player coordinate");
  }
  {
    const h=startupHarness({manifestState:"parsed"});
    h.setState("presenting");assert.equal(h.reserve("levelLoadError"),true);
    h.pause();h.advance(2_000);
    assert.equal(h.starts.filter(entry=>!entry.stop).length,0,
      "a pause cancels an established-stream reservation before it can restart loading");
    assert.equal(h.episode.retry.state,"cancelled",
      "the shared established-stream credit remains spent after pause");
    h.player.controlIntentGeneration+=1;
    h.video.currentTime=123;
    assert.equal(h.resume(),true,"the same established attachment resumes its loader");
    assert.deepEqual(h.starts.filter(entry=>!entry.stop),[{at:123,now:2_000}],
      "resume starts loading at the current media-local position");
    assert.equal(h.resume(),false,"duplicate Play cannot restart the established loader twice");
  }
  {
    const h=startupHarness({deadlineMs:2_000});
    assert.equal(h.reserve("network"),true);h.advance(2_000);
    assert.equal(h.loads.length,0);assert.equal(h.episode.state,"exhausted",
      "the absolute boundary rejects a dispatch at the deadline");
  }
  {
    const h=startupHarness();const loader=h.loader();
    for(let i=0;i<16;i++)h.send(loader);
    assert.equal(h.sends.length,16);h.send(loader);
    assert.equal(h.sends.length,16);assert.equal(h.episode.state,"exhausted",
      "the final transport gate enforces sixteen actual sends");
  }
  {
    const h=startupHarness();const loader=h.loader();
    h.pause();h.send(loader);
    assert.equal(h.sends.length,0);assert.ok(loader.aborted&&loader.destroyed,
      "a paused dispatch is destroyed before the wire");
    h.resume();h.advance(0);assert.equal(h.loads.length,1,
      "unused credit becomes the one immediate corrective reload on resume");
    h.pause();assert.equal(h.episode.state,"paused");
    h.resume();assert.equal(h.episode.state,"exhausted",
      "interrupting a dispatched corrective reload exhausts startup");
  }
  {
    const h=startupHarness();const loader=h.loader();
    h.send(loader,503,JSON.stringify({code:"producer_failed",message:"ffmpeg exited"}));
    assert.ok(loader.error,"a terminal typed body reaches hls.js before stock retry");
    assert.equal(h.episode.latestFailure.code,"producer_failed");
    const auth=h.loader();h.send(auth,401,"");
    assert.ok(auth.error);assert.match(h.episode.latestFailure.message,/Sign in/,
      "auth is terminal even without a typed body");
  }
  {
    const h=startupHarness();const old=h.loader();h.setCurrent(false);h.send(old);
    assert.equal(h.sends.length,0,"a predecessor completion cannot dispatch");
    const current=startupHarness();current.complete();assert.equal(current.episode.state,"presenting",
      "only the presentation owner retires startup");
  }

  await streamFailureBodyTests();
  await vendoredHlsStartupTests();

  process.stdout.write("PASS the M5 recovery additions: create retry, hls retry, shared budget\n");
  process.stdout.write("PASS web HLS startup recovery and final-send ownership\n");
  process.stdout.write("PASS passive web playback-control reporter\n");
}

async function streamFailureBodyTests(){
  const policy=require("../../crates/plurxd/src/web/playback-policy.js");
  const h=new Function("PlaybackPolicy",[
    "let STREAM_FAILURE=null;",
    shippedSource("boundStreamFailureBody"),shippedSource("decodeStreamFailureBytes"),
    shippedSource("streamFailureResponseBodyNow"),shippedSource("streamFailureResponseBody"),
    shippedSource("noteStreamFailure"),shippedSource("observeStreamFailureResponse"),
    "return {body:streamFailureResponseBody,observe:observeStreamFailureResponse,current:()=>STREAM_FAILURE};",
  ].join("\n"))(policy);
  const typed=JSON.stringify({code:"producer_failed",message:"encoder exited"});
  const bytes=new TextEncoder().encode(typed);
  const cases=[
    {name:"text",xhr:{status:502,responseType:"text",responseText:typed}},
    {name:"json",xhr:{status:502,responseType:"json",response:{code:"producer_failed",message:"encoder exited"}}},
    {name:"arraybuffer",xhr:{status:502,responseType:"arraybuffer",response:bytes.buffer,
      get responseText(){throw new Error("InvalidStateError");}}},
    {name:"blob",xhr:{status:502,responseType:"blob",response:new Blob([bytes])}},
  ];
  let ordinal=0;
  for(const {name,xhr} of cases){
    const failure=await h.observe(xhr,{attachment:"A",resource:"media",request_ordinal:++ordinal},()=>true);
    assert.equal(failure.code,"producer_failed",name);
    assert.equal(failure.message,"encoder exited",name);
  }
  assert.equal(await h.observe({status:502,responseType:"arraybuffer",
    response:new Uint8Array(policy.STREAM_FAILURE_BODY_MAX_CHARS+1).buffer},
    {attachment:"A",resource:"media",request_ordinal:++ordinal},()=>true),null,
  "an oversized binary body becomes bounded unknown evidence");
  assert.equal(await h.observe({status:502,responseType:"arraybuffer",get response(){throw Error("unreadable");}},
    {attachment:"A",resource:"media",request_ordinal:++ordinal},()=>true),null,
  "a response getter failure does not escape the hls.js callback");
  assert.equal(await h.observe({status:0,responseType:"",responseText:""},
    {attachment:"A",resource:"media",request_ordinal:++ordinal},()=>true),null,
  "a no-body network failure remains untyped");

  let release;
  const slow={status:502,responseType:"blob",response:{size:bytes.byteLength,
    arrayBuffer:()=>new Promise(resolve=>{release=()=>resolve(bytes.buffer);})}};
  let current=true;
  const pending=h.observe(slow,{attachment:"A",intent_generation:1,
    resource:"media",request_ordinal:++ordinal},()=>current);
  current=false;release();
  assert.equal(await pending,null,"replacement during Blob decode cannot mutate its successor");

  const newer=await h.observe({status:502,responseType:"text",
    responseText:JSON.stringify({code:"session_failed",message:"newer"})},
    {attachment:"A",resource:"media",request_ordinal:100},()=>true);
  const older=await h.observe({status:502,responseType:"text",responseText:typed},
    {attachment:"A",resource:"media",request_ordinal:99},()=>true);
  assert.equal(newer.message,"newer");
  assert.equal(older.message,"newer","a slow older refusal cannot overwrite newer evidence");
  assert.equal(h.current().request_ordinal,100);
}

function establishedHlsResumeTests(){
  const h=new Function([
    "let PLAYER=null;const calls=[];const performance={now:()=>1000};",
    "function clientLog(){}function playbackContext(){return {};}",
    "const attachment={current:()=>true};const hls={stopLoad(){calls.push(['stop']);},startLoad(at){calls.push(['start',at]);}};",
    "const player={hls,mediaAttachment:attachment,wantsPlayback:true,controlIntentGeneration:1};PLAYER=player;",
    "const episode={player,attachment,hls,state:'presenting',establishedSuspension:null,retry:{state:'reserved',timer:null}};player.hlsStartup=episode;",
    "const video={currentTime:80};function clearTimeout(){}",
    shippedSource("playbackAttemptTerminallyStopped"),shippedSource("hlsStartupCurrent"),shippedSource("pauseHlsStartup"),shippedSource("resumeHlsStartup"),
    "return {calls,player,episode,video,pause:()=>pauseHlsStartup(player),resume:()=>resumeHlsStartup(video,player),replace(){attachment.current=()=>false;}};",
  ].join("\n"))();
  assert.equal(h.pause(),true);
  assert.deepEqual(h.calls,[["stop"]]);
  assert.equal(h.episode.state,"presenting","established suspension is not startup suspension");
  h.player.controlIntentGeneration=2;
  h.video.currentTime=123;
  assert.equal(h.resume(),true);
  assert.deepEqual(h.calls,[["stop"],["start",123]]);
  assert.equal(h.resume(),false,"duplicate Play is idempotent");
  assert.equal(h.pause(),true);
  h.replace();
  assert.equal(h.resume(),false,"a replaced attachment cannot be restarted");

  const edges=new Function([
    "let PLAYER=null;const calls=[];function clearTimeout(){}function clientLog(){}function playbackContext(){return {};}",
    "function clearPlaybackControlWaiters(){}function endWait(){}function playbackSurfaceStep(){}function playerActivity(){}function notifyPlaybackControl(){}",
    "function supersedePlaybackControlIntent(p){p.controlIntentGeneration=(p.controlIntentGeneration||0)+1;return p.controlIntentGeneration;}",
    "function play(){}play.pendingIntent=null;",
    "const attachment={current:()=>true};const hls={stopLoad(){calls.push(['stop']);},startLoad(at){calls.push(['start',at]);}};",
    "const player={hls,mediaAttachment:attachment,wantsPlayback:true,controlIntentGeneration:1};PLAYER=player;",
    "const episode={player,attachment,hls,state:'presenting',establishedSuspension:null,retry:{state:'unused',timer:null}};player.hlsStartup=episode;",
    "const video={paused:false,ended:false,error:null,currentTime:44,pause(){this.paused=true;},play(){this.paused=false;return Promise.resolve();}};",
    "const document={getElementById:id=>id==='video'?video:null};",
    shippedSource("playbackAttemptTerminallyStopped"),shippedSource("hlsStartupCurrent"),shippedSource("pauseHlsStartup"),shippedSource("resumeHlsStartup"),
    shippedSource("resetPlaybackTransportEvents"),shippedSource("playbackTransportEvents"),
    shippedSource("pausePlaybackInternally"),shippedSource("rememberPlaybackTransportIntent"),
    shippedSource("applyPlaybackTransportIntent"),shippedSource("handlePlaybackTransportEvent"),
    shippedSource("togglePlay"),
    "return {calls,player,episode,video,toggle:togglePlay,native:event=>handlePlaybackTransportEvent(video,player,event),reset(){calls.length=0;episode.establishedSuspension=null;player.wantsPlayback=true;player.controlIntentGeneration=1;video.paused=false;resetPlaybackTransportEvents(video);}};",
  ].join("\n"))();
  edges.toggle();
  edges.toggle();
  assert.deepEqual(edges.calls,[["stop"],["start",44]],
    "the manual button stops then resumes the established loader once");
  edges.reset();
  edges.video.paused=true;
  edges.native("pause");
  edges.video.paused=false;
  edges.native("play");
  assert.deepEqual(edges.calls,[["stop"],["start",44]],
    "native media events use the same established-loader resume owner");
}

function terminalHlsStopTests(){
  const policy=require("../../crates/plurxd/src/web/playback-policy.js");
  const h=new Function("PlaybackPolicy",[
    "let PLAYER=null;const calls=[];let surface='terminal';",
    "function clearTimeout(timer){calls.push(['clear',timer]);}function setTimeout(fn){calls.push(['timer']);return 9;}",
    "function playbackSurfaceGeneration(p){return p&&p.attemptId;}function playbackSurfaceStep(event){calls.push(['surface',event.attach]);}",
    "const token={id:1};const attachment={current:()=>PLAYER===player&&player.mediaAttachment===token};",
    "const loader={abort(){calls.push(['abort']);},destroy(){calls.push(['destroy']);}};",
    "const hls={stopLoad(){calls.push(['stop']);},startLoad(at){calls.push(['start',at]);}};",
    "const player={hls,mediaAttachment:token,attemptId:'a1',controlIntentGeneration:4,hlsRetryUsed:0};PLAYER=player;",
    "const episode={player,attachment,mediaAttachment:token,hls,state:'presenting',establishedSuspension:{},loaders:new Set([loader]),retry:{state:'reserved',timer:17}};player.hlsStartup=episode;",
    "const video={currentTime:52};const document={getElementById:()=>video};",
    "function pausePlaybackInternally(){calls.push(['pause']);}function stopPlayerTimers(){calls.push(['timers']);}",
    shippedSource("playbackAttemptTerminallyStopped"),shippedSource("hlsStartupCurrent"),
    shippedSource("abortHlsStartupLoaders"),shippedSource("retireHlsTerminalAttempt"),
    shippedSource("stopPlayerForExhaustion"),shippedSource("scheduleHlsNetworkRetry"),
    shippedSource("beginPlaybackMediaAttachment"),
    "const owned=()=>attachment.current()&&player.hls===hls&&!playbackAttemptTerminallyStopped(player,token);",
    "return {calls,player,episode,stop:stopPlayerForExhaustion,current:()=>hlsStartupCurrent(player,episode),retry:()=>scheduleHlsNetworkRetry(video,player,'late fatal'),late(){if(owned())surface='recovering';return surface;},reopen(){player.attemptId='a2';const next=beginPlaybackMediaAttachment(player);return {current:next.current(),terminal:player.terminalStop};}};",
  ].join("\n"))(policy);

  assert.equal(h.current(),true);
  h.stop();
  assert.equal(h.episode.state,"cancelled");
  assert.equal(h.episode.cancelledReason,"owner_stopped");
  assert.equal(h.current(),false,"the stopped attempt no longer owns callbacks");
  assert.equal(h.retry(),false,"a reserved or late network retry cannot restart it");
  assert.equal(h.late(),"terminal","a late fatal cannot replace the terminal surface");
  assert.equal(h.calls.some(call=>call[0]==="start"),false);
  for(const event of ["abort","destroy","stop","pause","timers"])
    assert.equal(h.calls.some(call=>call[0]===event),true,event);

  const reopened=h.reopen();
  assert.equal(reopened.current,true,"a deliberate retry owns a fresh attachment");
  assert.equal(reopened.terminal,null,"the fresh attachment is not fenced by its predecessor");
}

async function freeFallPlaybackTests(){
  establishedHlsResumeTests();
  terminalHlsStopTests();
  await streamFailureBodyTests();
  await vendoredHlsStartupTests();
  process.stdout.write("PASS Free Fall loader resume and binary refusal regressions\n");
}

async function vendoredHlsStartupTests(){
  const VendoredHls=require("../../crates/plurxd/src/web/hls.min.js");
  assert.equal(typeof VendoredHls.DefaultConfig.loader.prototype.openAndSendXhr,"function",
    "the vendored stock loader exposes the final-send seam the adapter wraps");

  async function actualVendoredLoaderCase({xhrSetup=null}={}){
    const policy=require("../../crates/plurxd/src/web/playback-policy.js");
    let now=0,nextTimer=0;
    const timers=new Map(),sends=[],errors=[];
    class FakeXHR{
      constructor(){this.readyState=0;this.status=0;this.statusText="";this.responseText="";this.responseType="";this.listeners={};}
      open(){this.readyState=1;}
      setRequestHeader(){}
      addEventListener(name,fn){this.listeners[name]=fn;}
      send(){sends.push(this);}
      abort(){this.aborted=true;}
      answer(status,body=""){
        this.status=status;this.statusText=String(status);this.responseText=body;
        this.responseType="text";this.readyState=4;
        if(this.onreadystatechange)this.onreadystatechange();
        if(this.listeners.load)this.listeners.load();
      }
    }
    const priorSelf=global.self;
    global.self={performance:{now:()=>now},XMLHttpRequest:FakeXHR,
      setTimeout(fn,ms){const id=++nextTimer;timers.set(id,{fn,at:now+ms});return id;},
      clearTimeout(id){timers.delete(id);}};
    const advance=ms=>{
      now+=ms;
      for(;;){
        const due=[...timers].filter(([,timer])=>timer.at<=now)
          .sort((a,b)=>a[1].at-b[1].at)[0];
        if(!due)break;
        timers.delete(due[0]);due[1].fn();
      }
    };
    try{
      const make=new Function("PlaybackPolicy","StockLoader","xhrSetup","errors",[
        "let STREAM_FAILURE=null;const performance=self.performance;const setTimeout=self.setTimeout;const clearTimeout=self.clearTimeout;",
        "function clientLog(){}function playbackContext(){return {};}function stallDiagnose(){return Promise.resolve();}function reportTtff(){}",
        "function noteStreamFailure(status,body,evidence){const parsed=PlaybackPolicy.parseStreamFailure({status,body});if(!parsed)return null;Object.assign(parsed,{at:Date.now()},evidence||{});STREAM_FAILURE=parsed;return parsed;}",
        "const attachment={current:()=>true};const video={currentTime:91,appendChild(){},querySelectorAll:()=>[],textTracks:[]};",
        "const document={getElementById:()=>video,createElement:()=>({dataset:{},track:{}})};",
        "function closeMenu(){}function endWait(){}function subNeedsBurn(s){return !!s?.burn;}function positionForPlaybackIntent(){return 91;}function notifyPlaybackControl(){}function rememberPlaybackSelection(){}function restartPendingPlaybackOpen(){return false;}function pbSyncSubIcon(){}function clearSubs(){}function subLabelFor(){return 'English';}function subUrl(){return '/subtitle';}function nativeHlsSubtitleOrdinal(){return -1;}function clearPlaybackControlWaiters(){}",
        "const hls={loadSource(){},startLoad(){},stopLoad(){}};let PLAYER={hls,hlsRetryUsed:0,wantsPlayback:true,mediaAttachment:attachment,controlIntentGeneration:1};",
        "const episode={player:PLAYER,attachment,playlistUrl:'/delayed/index.m3u8',hls,state:'active',manifestState:'unknown',mediaLoaded:false,decoderFailed:false,startedAt:0,deadlineMs:40000,dispatches:0,loaders:new Set(),latestFailure:null,establishedSuspension:null,retry:{state:'unused',dueMs:null,detail:null,timer:null,intentGeneration:null}};PLAYER.hlsStartup=episode;",
        shippedSource("playbackAttemptTerminallyStopped"),shippedSource("hlsStartupCurrent"),shippedSource("hlsStartupIncomplete"),
        shippedSource("hlsStartupManifestRequest"),shippedSource("abortHlsStartupLoaders"),
        shippedSource("cancelHlsStartup"),shippedSource("completeHlsStartup"),
        shippedSource("configureHlsStartupDeadline"),shippedSource("diagnoseHlsStartup"),
        shippedSource("exhaustHlsStartup"),shippedSource("armHlsStartupRetry"),
        shippedSource("pauseHlsStartup"),shippedSource("resumeHlsStartup"),
        shippedSource("boundStreamFailureBody"),shippedSource("decodeStreamFailureBytes"),
        shippedSource("streamFailureResponseBodyNow"),shippedSource("streamFailureResponseBody"),
        shippedSource("observeStreamFailureResponse"),
        shippedSource("createHlsStartupLoader"),
        shippedSource("supersedePlaybackControlIntent"),shippedSource("setSub"),
        shippedSource("scheduleHlsNetworkRetry"),
        "const Loader=createHlsStartupLoader(StockLoader,episode);const loader=new Loader({xhrSetup});",
        "loader.load({type:'manifest',url:'/delayed/index.m3u8',responseType:'text'},",
        " {loadPolicy:PlaybackPolicy.HLS_STARTUP.manifest_load_policy.default,timeout:10000},",
        " {onSuccess(){},onProgress(){},onAbort(){},onTimeout(){errors.push('timeout');},onError(error){errors.push(error);}});",
        "return {episode,loader,player:PLAYER,setSub,retry:()=>scheduleHlsNetworkRetry(video,PLAYER,'test'),pause:()=>pauseHlsStartup(PLAYER),supersede:()=>supersedePlaybackControlIntent(PLAYER)};",
      ].join("\n"))(policy,VendoredHls.DefaultConfig.loader,xhrSetup,errors);
      return {make,sends,errors,advance,restore:()=>{global.self=priorSelf;}};
    }catch(error){global.self=priorSelf;throw error;}
  }
  {
    const h=await actualVendoredLoaderCase();
    try{
      assert.equal(h.sends.length,1,"T01/T03: the actual vendored loader reaches the final-send adapter");
      h.sends[0].answer(503,JSON.stringify({code:"producer_ended",message:"producer ended"}));
      h.advance(20_000);
      assert.equal(h.sends.length,1,"T08: a terminal typed 503 suppresses the stock retry timer");
      assert.equal(h.errors.length,1,"T08: the terminal refusal reaches hls.js once");
    }finally{h.restore();}
  }
  {
    let releaseSetup;
    const setup=new Promise(resolve=>{releaseSetup=resolve;});
    const h=await actualVendoredLoaderCase({xhrSetup:()=>setup});
    try{
      assert.equal(h.sends.length,0,"T17: asynchronous xhrSetup has not crossed the final-send gate");
      h.make.pause();releaseSetup();
      await Promise.resolve();await Promise.resolve();await Promise.resolve();
      assert.equal(h.sends.length,0,"T05/T17: pause destroys the actual loader before async setup can send");
    }finally{h.restore();}
  }

  for(const subtitle of [0,-1]){
    let releaseSetup;
    const setup=new Promise(resolve=>{releaseSetup=resolve;});
    const h=await actualVendoredLoaderCase({xhrSetup:()=>setup});
    try{
      h.make.player.subs=[{index:0,codec:'mov_text'}];
      h.make.player.offset=0;
      assert.equal(h.sends.length,0,'manifest dispatch is still pending in the real bundled loader');
      await h.make.setSub(subtitle);
      releaseSetup();
      await Promise.resolve();await Promise.resolve();await Promise.resolve();
      assert.equal(h.sends.length,1,'a local text-subtitle change must not cancel unchanged video');
      assert.equal(h.make.episode.state,'active');
      assert.equal(h.make.player.controlIntentGeneration,2,'old control replies remain fenced');
    }finally{h.make.loader.destroy();h.restore();}
  }

  {
    let releaseSetup;
    const setup=new Promise(resolve=>{releaseSetup=resolve;});
    const h=await actualVendoredLoaderCase({xhrSetup:()=>setup});
    try{
      h.make.supersede();
      await h.make.setSub(-1);
      releaseSetup();
      await Promise.resolve();await Promise.resolve();await Promise.resolve();
      assert.equal(h.sends.length,0,'a subtitle change must not revive a request fenced by an earlier command');
    }finally{h.make.loader.destroy();h.restore();}
  }

  {
    const h=await actualVendoredLoaderCase();
    try{
      assert.equal(h.make.retry(),true);
      const retryTimer=h.make.episode.retry.timer;
      await h.make.setSub(-1);
      assert.equal(h.make.episode.retry.timer,retryTimer,'text tracks preserve the pending video retry timer');
      h.advance(require('../../crates/plurxd/src/web/playback-policy.js').HLS_RETRY.delay_ms);
      assert.equal(h.make.episode.retry.state,'dispatched','the unchanged video still receives its one retry');
      assert.equal(h.make.retry(),false,'a subtitle change does not replenish the retry budget');
    }finally{h.make.loader.destroy();h.restore();}
  }

  for(const pending of ['pendingOpenAttempt','retiringOpenAttempt','pendingMediaChange']){
    let releaseSetup;
    const setup=new Promise(resolve=>{releaseSetup=resolve;});
    const h=await actualVendoredLoaderCase({xhrSetup:()=>setup});
    try{
      h.make.player[pending]={};
      await h.make.setSub(-1);
      releaseSetup();
      await Promise.resolve();await Promise.resolve();await Promise.resolve();
      assert.equal(h.sends.length,0,`${pending}: a subtitle change must not carry forward a replaced video's request`);
    }finally{h.make.loader.destroy();h.restore();}
  }

  process.stdout.write("PASS vendored HLS startup, local subtitles, retry ownership and stale-request fences\n");
}

const focused=process.argv.includes('--free-fall')
  ?freeFallPlaybackTests()
  :process.argv.includes('--hls-startup')?vendoredHlsStartupTests():main();
focused.catch((error) => {
  process.stderr.write(`${error.stack || error}\n`);
  process.exitCode = 1;
});
