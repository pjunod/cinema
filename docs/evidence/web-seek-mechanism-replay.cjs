'use strict';
// Diagnostic of the October 2 incident source, not a post-fix acceptance test.
// Run: node docs/evidence/web-seek-mechanism-replay.cjs /path/to/source
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const root = path.resolve(process.argv[2] || '.');
const file = path.join(root, 'crates/plurxd/src/web/player/transport.js');
const source = fs.readFileSync(file, 'utf8');
const start = source.indexOf('function playbackProgressTick(');
const end = source.indexOf('function settlePlaybackControlSeek(', start);
assert.ok(start >= 0 && end > start);
const tick = new Function('p', 'v', 'nowMs', `
  const PLAYER=p,document={hidden:false},performance={now:()=>nowMs??1000};
  const PlaybackPolicy={recoveryHealthObservation:()=>({state:null,rearm:false})};
  const PERSISTENT_STALL_MS=8000;
  function playbackSurfaceStep(){} function playbackSurfaceGeneration(){return 1}
  function playbackOwnsAttachedMedia(){return true}
  function samplePlaybackPresentationClock(){} function samplePreparedSwitchFrames(){}
  function streamHasVideo(){return true} function finishPlaybackSeekTelemetry(){}
  function settlePlaybackControlSeek(){return false} function completeHlsStartup(){}
  function endWait(){} function clearStall(){} function finishStallRecovery(){}
  function playbackSeekBufferCovers(){return false} function bufferRunway(){return 0}
  function persistentWait(){return Promise.resolve()}
  ${source.slice(start,end)}
  playbackProgressTick(v,p);
`);
const pending={sequence:9,targetMs:1198106,executed:true,executedAt:900,
  frameFloor:405,localVodSeek:true,localVodSeekFallbackPending:true};
pending.localVodSeekCleanup=()=>{pending.localVodSeekFallbackPending=false};
const player={started:true,wantsPlayback:true,controlSeek:pending,
  controlHasFrameCallbacks:true,controlPresentedFrames:405,controlIntentGeneration:8,
  progressWatch:{key:'0:8:9:callbacks',clock:1049.989,frames:404,at:800,
    startedAt:800,fired:false}};
const video={currentTime:1198.106,seeking:true,paused:false,ended:false,readyState:4};
tick(player,video);
assert.equal(pending.localVodPresented,true);
assert.equal(pending.localVodSeekFallbackPending,false);
assert.equal(player.controlPresentedFrames,pending.frameFloor);
// Replay the normal ordering, not only a hand-seeded watch/floor mismatch.
const markStart=source.indexOf('function markPlaybackControlSeekExecuted(');
const markEnd=source.indexOf('function samplePlaybackPresentationClock(',markStart);
assert.ok(markStart>=0&&markEnd>markStart);
const execute=new Function('p','v','target',`
  const performance={now:()=>750};
  function playbackSurfaceStep(){} function watchPlaybackSeekTelemetry(){}
  function samplePlaybackPresentationClock(){}
  ${source.slice(markStart,markEnd)}
  return markPlaybackControlSeekExecuted(p,target,v);
`);
function sequence(target){
  const intent={sequence:1,targetMs:target*1000,executed:false,frameFloor:380};
  const p={started:true,wantsPlayback:true,controlSeek:intent,
    controlHasFrameCallbacks:true,controlPresentedFrames:380};
  const v={currentTime:1000,seeking:false,paused:false,ended:false,readyState:4};
  tick(p,v,0); // published intent starts a watch while old media plays
  p.controlPresentedFrames=392;v.currentTime=1000.5;tick(p,v,500);
  p.controlPresentedFrames=398;v.currentTime=1000.75;
  assert.equal(execute(p,v,target),true);
  intent.localVodSeek=true;intent.localVodSeekFallbackPending=true;
  intent.localVodSeekCleanup=()=>{intent.localVodSeekFallbackPending=false};
  v.currentTime=target;v.seeking=true;tick(p,v,1000);
  assert.equal(p.controlPresentedFrames,intent.frameFloor);
  return {presented:!!intent.localVodPresented,fallbackPending:intent.localVodSeekFallbackPending};
}
const forwardSequence=sequence(1030),backwardSequence=sequence(970);
assert.deepEqual(forwardSequence,{presented:true,fallbackPending:false});
assert.deepEqual(backwardSequence,{presented:false,fallbackPending:true});
const Hls=require(path.join(root,'crates/plurxd/src/web/hls.min.js'));
const hls=new Hls({enableWorker:false});
const buffer=hls.bufferController;
buffer.mediaSource={readyState:'open'};
buffer.media={seeking:true};
hls.resumeBuffering();
buffer._onEndStreaming();
const pausedWhileSeeking=!hls.bufferingEnabled;
buffer._onStartStreaming();
const resumed=hls.bufferingEnabled;
buffer.mediaSource=null;buffer.media=null;hls.destroy();
assert.equal(pausedWhileSeeking,true);
assert.equal(resumed,true);
console.log(JSON.stringify({source:file,hlsVersion:Hls.version,
  oldFramesThenExecution:{forward:forwardSequence,backward:backwardSequence},
  falsePresentation:{presented:pending.localVodPresented,
    fallbackPending:pending.localVodSeekFallbackPending,
    presentedFrames:player.controlPresentedFrames,dispatchFrameFloor:pending.frameFloor,
    stillSeeking:video.seeking},
  managedMediaSource:{endStreamingPausesDuringSeek:pausedWhileSeeking,
    startStreamingResumes:resumed}},null,2));
