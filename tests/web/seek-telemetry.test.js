'use strict';
// Exercise the shipped command/presentation edges with a controlled monotonic
// clock. Preview and coalescing must not inflate the server seek denominator.
const {test}=require('node:test'),assert=require('node:assert/strict'),vm=require('node:vm');
const {shellSource}=require('./shell-source.js');
const html=shellSource().bodyScript;
function source(name){
  let at=html.indexOf(`async function ${name}(`);
  if(at<0) at=html.indexOf(`function ${name}(`);
  assert.ok(at>=0,`missing shipped ${name}`);
  return html.slice(at,html.indexOf('\n}',at)+2);
}
function harness(options={}){
  const posts=[],timers=[],listeners=new Map(),clock={ms:10};
  const p={fileId:7,method:'remux',started:true,wantsPlayback:true,offset:0,
    source:{video_codec:'h264'},controlHasFrameCallbacks:options.callbacks!==false,
    controlPresentedFrames:0,mediaAttachment:{id:1}};
  const video={readyState:4,seeking:false,paused:false,ended:false,currentTime:0,playbackRate:1,
    addEventListener(name,fn){if(!listeners.has(name))listeners.set(name,new Set());listeners.get(name).add(fn);},
    removeEventListener(name,fn){listeners.get(name)?.delete(fn);},
    getVideoPlaybackQuality(){return {totalVideoFrames:0};}};
  const c=vm.createContext({PLAYER:p,performance:{now:()=>clock.ms},Math,Number,Object,
    document:{hidden:false,getElementById:()=>video},
    playbackContext:()=>({attempt:'a1',method:p.method,file_id:p.fileId}),
    clientLog:r=>posts.push(JSON.parse(JSON.stringify(r))),
    playbackOwnsAttachedMedia:owner=>owner===c.PLAYER,
    supersedePlaybackControlIntent:owner=>++owner.controlIntentGeneration,
    playbackSurfaceStep:()=>{},notifyPlaybackControl:()=>false,
    pbTotalSec:()=>120,playbackChangeAlreadyInFlight:()=>false,endWait:()=>{},
    restartPendingPlaybackOpen:()=>false,hasPendingPlaybackOpen:()=>false,
    playbackSeekBufferedRangesMs:()=>[],playbackSeekPublishedRangeMs:()=>null,
    PlaybackPolicy:{seekRoute:({targetMs})=>({route:'local',basis:'direct',atMs:targetMs}),HLS_STARTUP:{seek_deadline_ms:8000}},
    setTimeout:(fn,ms)=>{timers.push({fn,ms});return timers.length;},clearTimeout:()=>{},
    armStall:()=>{},playerActivity:()=>{},
  });
  for(const name of ['dispatchPlaybackSeekTelemetry','finishPlaybackSeekTelemetry','watchPlaybackSeekTelemetry',
    'beginPlaybackControlSeek','markPlaybackControlSeekExecuted','samplePlaybackPresentationClock',
    'settlePlaybackControlSeek','seekTo'])vm.runInContext(source(name),c);
  function emit(name){for(const fn of [...(listeners.get(name)||[])])fn();}
  function present(at=clock.ms+50){clock.ms=at;video.seeking=false;emit('seeked');
    return c.settlePlaybackControlSeek(video,c.PLAYER,video.currentTime,++c.PLAYER.controlPresentedFrames);}
  async function dispatch(target){const promise=c.seekTo(target);const timer=timers.shift();assert.equal(timer.ms,100);
    clock.ms+=100;timer.fn();await promise;return c.PLAYER.controlSeek;}
  return {c,p,video,posts,timers,clock,emit,present,dispatch};
}
test('a dispatched seek reports its first target frame once with monotonic latency',async()=>{
  const h=harness();await h.dispatch(30);assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,0);
  assert.equal(h.present(),true);h.present();
  const reports=h.posts.filter(x=>x.event==='seek_resumed');assert.equal(reports.length,1);
  assert.equal(reports[0].ms,50);assert.equal(reports[0].method,'remux');assert.equal(reports[0].file_id,7);
});
test('a newer dispatched seek abandons its predecessor and resumes once',async()=>{
  const h=harness();await h.dispatch(20);await h.dispatch(40);h.present();
  assert.deepEqual(h.posts.filter(x=>/^seek_(resumed|abandoned)$/.test(x.event)).map(x=>x.event),['seek_abandoned','seek_resumed']);
});
test('coalesced commands and previews do not count as dispatched seeks',async()=>{
  const h=harness();h.p._seekPreview=10;
  const first=h.c.seekTo(20),second=h.c.seekTo(30);
  for(const timer of h.timers.splice(0)){h.clock.ms+=100;timer.fn();}
  await Promise.all([first,second]);h.present();
  assert.deepEqual(h.posts.filter(x=>/^seek_(resumed|abandoned)$/.test(x.event)).map(x=>x.event),['seek_resumed']);
});
test('no-rVFC fallback requires seeked and a ready current target before reporting',async()=>{
  const h=harness({callbacks:false});await h.dispatch(30);h.clock.ms+=50;
  h.emit('timeupdate');assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,0);
  h.emit('seeked');h.video.readyState=2;h.emit('timeupdate');
  assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,0);
  h.video.readyState=3;h.emit('timeupdate');h.emit('timeupdate');
  assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,1);
});
test('retired attachment callbacks cannot finish a replacement seek',async()=>{
  const h=harness({callbacks:false});await h.dispatch(30);h.p.mediaAttachment={id:2};
  h.clock.ms+=50;h.emit('seeked');h.emit('timeupdate');
  assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,0);
});
test('same-target forced repair retains the dispatched command measurement',async()=>{
  const h=harness();const original=await h.dispatch(30);
  // A retry while a full open is pending takes the real early dispatch path.
  h.c.restartPendingPlaybackOpen=()=>true;
  await h.c.seekTo(30,true);assert.equal(h.p.controlSeek.seekTelemetry,original.seekTelemetry);
  assert.equal(h.posts.filter(x=>x.event==='seek_abandoned').length,0);
  h.c.markPlaybackControlSeekExecuted(h.p,30,h.video);h.present();
  assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,1);
});
test('a cloned successor shares the one-shot outcome with its predecessor',async()=>{
  const h=harness();const pending=await h.dispatch(30);
  const successor=Object.assign({},h.p,{controlSeek:Object.assign({},pending),mediaAttachment:{id:2}});
  h.c.PLAYER=successor;h.c.markPlaybackControlSeekExecuted(successor,30,h.video);h.present();
  h.c.finishPlaybackSeekTelemetry(h.p,pending,'seek_abandoned');
  assert.equal(h.posts.filter(x=>/^seek_(resumed|abandoned)$/.test(x.event)).length,1);
});
test('close abandons an unpresented dispatched seek exactly once',async()=>{
  const h=harness();await h.dispatch(30);const stopped=new Error('after closing seek');
  const noop=()=>{};
  Object.assign(h.c,{WATCH_CLOSE_PROMISE:null,WATCH_GENERATION:1,location:{hash:'#watch'},
    watchDetach:noop,PLAY_OPEN_GATE:{invalidate:noop},play:{},reportProgress:()=>Promise.resolve(),
    beginPlaybackPreparation:{active:null},retirePlaybackPredecessor:noop,
    exitPresentationModes:noop,clearPlayerMediaSession:noop,supersedePlaybackControlIntent:noop,
    releaseSession:noop,cancelPendingSeek:noop,stopPlayerTimers:()=>{throw stopped;}});
  h.video.classList={remove:noop};h.video.style={};
  vm.runInContext(source('closePlayer'),h.c);
  assert.throws(()=>h.c.closePlayer(),e=>e===stopped);
  h.c.finishPlaybackSeekTelemetry(h.p,h.p.controlSeek,'seek_abandoned');
  assert.equal(h.posts.filter(x=>x.event==='seek_abandoned').length,1);
});
test('owner exhaustion abandons an unpresented seek before stopping timers',async()=>{
  const h=harness();await h.dispatch(30);Object.assign(h.c,{retireHlsTerminalAttempt:()=>{},
    pausePlaybackInternally:()=>{},stopPlayerTimers:()=>{}});
  vm.runInContext(source('stopPlayerForExhaustion'),h.c);
  h.c.stopPlayerForExhaustion();h.c.stopPlayerForExhaustion();
  assert.equal(h.posts.filter(x=>x.event==='seek_abandoned').length,1);
});
test('multipart navigation dispatches one seek for the destination part',async()=>{
  const h=harness();h.p.bookParts=[{id:7,part_offset_ms:0,duration_ms:10000},
    {id:8,part_offset_ms:10000,duration_ms:20000}];
  let intent;
  h.c.play=(id,title,resume,duration,meta,attempt,retry)=>{assert.equal(id,8);assert.equal(resume,5000);intent=retry.controlSeek;};
  await h.c.seekTo(15);
  assert.equal(intent.targetMs,5000);assert.equal(intent.seekTelemetry.context.file_id,8);
  assert.equal(intent.seekTelemetry.startedAt,10);
});
test('an internal restart without a viewer seek does not add a seek beacon',async()=>{
  const h=harness();h.c.restartPendingPlaybackOpen=()=>true;
  await h.c.seekTo(30,true,null,false);
  h.c.finishPlaybackSeekTelemetry(h.p,h.p.controlSeek,'seek_abandoned');
  assert.deepEqual(h.posts,[]);
});
test('seek attachment startup cannot inflate TTFF before or after its target frame',async()=>{
  const h=harness();vm.runInContext(source('reportTtff'),h.c);
  h.p.playStartedAt=1;h.p.ttffMs=123;h.p.attemptReason='seek';h.clock.ms=100;
  h.c.reportTtff();assert.equal(h.posts.length,0);assert.equal(h.p.playStartedAt,null);assert.equal(h.p.ttffMs,123);
  await h.dispatch(30);h.p.playStartedAt=1;h.p.attemptReason='stall-restart';
  h.c.reportTtff();assert.equal(h.posts.filter(x=>x.event==='ttff').length,0);
  h.p.playStartedAt=1;h.present();h.c.reportTtff();
  assert.equal(h.posts.filter(x=>x.event==='ttff').length,0);
  assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,1);
});
test('ordinary cold starts and saved-position resumes retain one TTFF beacon',()=>{
  for(const reason of ['cold-start','resume']){
    const h=harness();vm.runInContext(source('reportTtff'),h.c);
    h.p.playStartedAt=10;h.p.attemptReason=reason;h.clock.ms=110;
    h.c.reportTtff();h.c.reportTtff();
    assert.equal(h.posts.length,1);assert.equal(h.posts[0].event,'ttff');assert.equal(h.posts[0].ms,100);
  }
});

test('another title abandons the outgoing dispatched seek when its decoder retires',async()=>{
  const h=harness();await h.dispatch(30);let stopped=0;
  Object.assign(h.c,{stopPlayerTimers:()=>{stopped++;},teardownHls:()=>{},releaseSession:()=>{},
    clearSubs:()=>{},prePlayApplication:()=>({subtitle:null,burnedSub:null}),qualityForce:()=> 'original'});
  vm.runInContext(source('preparePlayOutgoing'),h.c);
  const replacement=h.c.preparePlayOutgoing({video:h.video,predecessor:h.p,meta:{},selection:{}},
    {method:'direct_play',source:{}});
  const incoming={fileId:8,controlSeek:null,mediaPredecessor:h.p};h.c.PLAYER=incoming;
  replacement.retireOutgoing();replacement.retireOutgoing();
  assert.equal(stopped,1);assert.equal(h.c.PLAYER,incoming);
  assert.equal(h.posts.filter(x=>x.event==='seek_abandoned').length,1);
  assert.equal(h.posts.find(x=>x.event==='seek_abandoned').file_id,7);
  h.c.finishPlaybackSeekTelemetry(h.p,h.p.controlSeek,'seek_resumed');
  assert.equal(h.posts.filter(x=>/^seek_(resumed|abandoned)$/.test(x.event)).length,1);
});
test('decoder retirement preserves the exact shared seek through reopen and multipart replacement',async()=>{
  for(const fileId of [7,8]){
    const h=harness();const pending=await h.dispatch(30);
    Object.assign(h.c,{stopPlayerTimers:()=>{},teardownHls:()=>{},releaseSession:()=>{},
      clearSubs:()=>{},prePlayApplication:()=>({subtitle:null,burnedSub:null}),qualityForce:()=> 'original'});
    vm.runInContext(source('preparePlayOutgoing'),h.c);
    const replacement=h.c.preparePlayOutgoing({video:h.video,predecessor:h.p,meta:{},selection:{}},
      {method:'direct_play',source:{}});
    const incoming=Object.assign({},h.p,{fileId,controlSeek:Object.assign({},pending)});h.c.PLAYER=incoming;
    replacement.retireOutgoing();assert.equal(h.posts.filter(x=>/^seek_(resumed|abandoned)$/.test(x.event)).length,0);
    h.clock.ms+=50;h.c.finishPlaybackSeekTelemetry(incoming,incoming.controlSeek,'seek_resumed');
    assert.deepEqual(h.posts.filter(x=>/^seek_(resumed|abandoned)$/.test(x.event)).map(x=>x.event),['seek_resumed']);
  }
});
