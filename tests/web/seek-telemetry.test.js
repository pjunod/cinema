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
    getVideoPlaybackQuality(){return {totalVideoFrames:this.frames||0};}};
  const c=vm.createContext({PLAYER:p,performance:{now:()=>clock.ms},Math,Number,Object,
    document:{hidden:false,getElementById:()=>video},
    playbackContext:()=>({attempt:'a1',method:p.method,file_id:p.fileId}),
    clientLog:r=>posts.push(JSON.parse(JSON.stringify(r))),
    playbackOwnsAttachedMedia:owner=>owner===c.PLAYER,
    playbackAttemptTerminallyStopped:()=>false,
    supersedePlaybackControlIntent:owner=>++owner.controlIntentGeneration,
    playbackSurfaceStep:()=>{},notifyPlaybackControl:()=>false,
    pbTotalSec:()=>120,playbackChangeAlreadyInFlight:()=>false,endWait:()=>{},
    restartPendingPlaybackOpen:()=>false,hasPendingPlaybackOpen:()=>false,
    playbackSeekBufferedRangesMs:()=>[],playbackSeekSeekableRangesMs:()=>null,playbackSeekPublishedRangeMs:()=>null,
    PlaybackPolicy:{seekRoute:({targetMs})=>({route:'local',basis:'direct',atMs:targetMs}),HLS_STARTUP:{seek_deadline_ms:8000}},
    setTimeout:(fn,ms)=>{timers.push({fn,ms});return timers.length;},clearTimeout:()=>{},
    armStall:()=>{},playerActivity:()=>{},qualityForce:()=>'original',
    playbackSeekBufferCovers:()=>true,
  });
  for(const name of ['dispatchPlaybackSeekTelemetry','playbackSeekTraceRanges',
    'recordPlaybackSeekRoute','finishPlaybackSeekTelemetry','watchPlaybackSeekTelemetry',
    'beginPlaybackControlSeek','markPlaybackControlSeekExecuted','samplePlaybackPresentationClock',
    'hlsStartupCurrent','pauseHlsStartup','resumePausedVodSeek','resumeHlsStartup',
    'settlePlaybackControlSeek','seekTo','clearEffectiveProcessing'])vm.runInContext(source(name),c);
  function emit(name){for(const fn of [...(listeners.get(name)||[])])fn();}
  function present(at=clock.ms+50){clock.ms=at;video.seeking=false;emit('seeked');
    return c.settlePlaybackControlSeek(video,c.PLAYER,video.currentTime,++c.PLAYER.controlPresentedFrames,{epoch:c.PLAYER.controlPresentationEpoch,attachment:c.PLAYER.mediaAttachment,intent:c.PLAYER.controlSeek?.sequence});}
  async function dispatch(target){const promise=c.seekTo(target);const at=timers.findIndex(timer=>timer.ms===100);const [timer]=timers.splice(at,1);assert.equal(timer.ms,100);
    clock.ms+=100;timer.fn();await promise;return c.PLAYER.controlSeek;}
  return {c,p,video,posts,timers,clock,emit,present,dispatch};
}
test('a dispatched seek reports its first target frame once with monotonic latency',async()=>{
  const h=harness();await h.dispatch(30);assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,0);
  assert.equal(h.present(),true);h.present();
  const reports=h.posts.filter(x=>x.event==='seek_resumed');assert.equal(reports.length,1);
  assert.equal(reports[0].ms,50);assert.equal(reports[0].method,'remux');assert.equal(reports[0].file_id,7);
});
test('route and settlement retain bounded film-coordinate evidence across a nonzero origin',async()=>{
  const h=harness();h.p.offset=30;h.p.sessionId='session-seven';h.video.currentTime=2;
  h.c.playbackSeekBufferedRangesMs=()=>[{from:30_000,through:80_000}];
  h.c.playbackSeekSeekableRangesMs=()=>[{from:30_000,through:75_000}];
  await h.dispatch(50);
  const route=JSON.parse(h.posts.find(post=>post.event==='seek_route').seek_trace);
  assert.equal(route.target_ms,50_000);assert.equal(route.origin_ms,30_000);
  assert.equal(route.session,'session-seven');
  assert.equal(route.element_ms,32_000);assert.equal(route.path,'native_hls');
  assert.deepEqual(route.buffered_ms,[[30_000,80_000]]);
  assert.deepEqual(route.seekable_ms,[[30_000,75_000]]);
  assert.equal(route.produced_end_ms,null);
  h.present();
  const settled=JSON.parse(h.posts.find(post=>post.event==='seek_resumed').seek_trace);
  assert.equal(settled.id,route.id);assert.equal(settled.landing_ms,50_000);
  assert.equal(settled.outcome,'seek_resumed');
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
  h.video.readyState=3;h.emit('timeupdate');
  assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,0);
  h.video.frames=1;h.video.currentTime=30.1;h.emit('timeupdate');
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
  h.c.reportTtff();assert.equal(h.posts.filter(x=>x.event==='ttff').length,0);
  assert.equal(h.p.playStartedAt,null);assert.equal(h.p.ttffMs,123);
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

for(const beforeSeeked of [true,false]) test(`paused sole target frame ${beforeSeeked?'before':'after'} seeked settles without Play`,async()=>{
  const h=harness();h.video.paused=true;h.p.wantsPlayback=false;
  await h.dispatch(30);h.video.seeking=beforeSeeked;
  const observation={epoch:h.p.controlPresentationEpoch,attachment:h.p.mediaAttachment,intent:h.p.controlSeek.sequence};
  const settled=h.c.settlePlaybackControlSeek(h.video,h.p,30,1,observation);
  assert.equal(settled,!beforeSeeked);
  if(beforeSeeked){assert.ok(h.p.controlSeek.targetFrame);h.video.seeking=false;h.emit('seeked');}
  assert.equal(h.p.controlSeek,null);assert.equal(h.video.paused,true);assert.equal(h.p.wantsPlayback,false);
  assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,1);
});
test('late old frame, epoch, intent and attachment cannot cancel current delivery',async()=>{
  const h=harness();await h.dispatch(30);
  const pending=h.p.controlSeek;
  const current={epoch:h.p.controlPresentationEpoch,attachment:h.p.mediaAttachment,intent:pending.sequence};
  for(const observation of [{...current,epoch:current.epoch-1},{...current,intent:current.intent-1},{...current,attachment:{id:0}}])
    assert.equal(h.c.settlePlaybackControlSeek(h.video,h.p,30,1,observation),false);
  assert.equal(h.c.settlePlaybackControlSeek(h.video,h.p,10,1,current),false);
  assert.equal(h.p.controlSeek,pending);assert.equal(pending.targetFrame,null);
  assert.equal(h.c.settlePlaybackControlSeek(h.video,h.p,30,1,current),true);
});
test('paused no-callback positioning labels weaker proof and rejects a clamp',async()=>{
  const h=harness({callbacks:false});h.video.paused=true;h.p.wantsPlayback=false;
  await h.dispatch(30);h.p.playStartedAt=100;h.video.currentTime=10;h.emit('seeked');assert.ok(h.p.controlSeek);
  h.video.currentTime=30;h.emit('seeked');assert.equal(h.p.controlSeek,null);
  assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,0);
  assert.equal(h.posts.filter(x=>x.event==='seek_positioned').length,1);
  assert.equal(h.p.playStartedAt,null,'positioning cannot mint a later cold-start TTFF');
  assert.equal(h.video.paused,true);
});

test('ready paused successor settles its only frame before any playing event',async()=>{
  const h=harness();await h.dispatch(30);h.p.started=false;h.p.wantsPlayback=false;h.video.paused=true;
  const identity={epoch:h.p.controlPresentationEpoch,attachment:h.p.mediaAttachment,intent:h.p.controlSeek.sequence};
  assert.equal(h.c.settlePlaybackControlSeek(h.video,h.p,30,1,identity),true);
  assert.equal(h.p.controlSeek,null);assert.equal(h.video.paused,true);assert.equal(h.p.started,false);
});

function pausedVodHarness(covered=false){
  const h=harness(),loads=[];
  h.p.vod=true;h.p.method='transcode';h.p.wantsPlayback=false;h.video.paused=true;
  h.c.PlaybackPolicy.seekRoute=({targetMs})=>({route:'local',basis:'vod',atMs:targetMs});
  h.c.playbackSeekBufferCovers=()=>covered;
  h.p.hls={stopLoad(){loads.push(['stop']);},startLoad(at){loads.push(['start',at]);}};
  h.p.hlsStartup={player:h.p,hls:h.p.hls,state:'presenting',
    attachment:{current:()=>h.c.PLAYER===h.p},retry:{state:'unused',timer:null}};
  h.p.controlIntentGeneration=1;
  h.c.pauseHlsStartup(h.p);
  return {...h,loads};
}
test('paused unbuffered VOD seek fetches its target without Play and suspends after the target frame',async()=>{
  const h=pausedVodHarness();await h.dispatch(30);
  assert.deepEqual(h.loads,[['stop'],['start',30]],'Pause stopped the loader; the seek must issue fresh target loading');
  assert.equal(h.video.paused,true);assert.equal(h.p.wantsPlayback,false);
  assert.ok(h.p.controlSeek,'clock assignment cannot settle the seek');
  h.present();
  assert.equal(h.p.controlSeek,null);assert.deepEqual(h.loads.at(-1),['stop']);
  assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,1);
  h.p.wantsPlayback=true;h.p.controlIntentGeneration++;
  assert.equal(h.c.resumeHlsStartup(h.video,h.p),true,'Play after a landed paused seek must restart the loader');
  assert.deepEqual(h.loads.at(-1),['start',30]);
});

test('buffered paused VOD seek preserves suspension and permits the next Play',async()=>{
  const h=pausedVodHarness(true);await h.dispatch(30);
  assert.deepEqual(h.loads,[['stop']],'already buffered target needs no download');
  h.present();h.p.wantsPlayback=true;h.p.controlIntentGeneration++;
  assert.equal(h.c.resumeHlsStartup(h.video,h.p),true);
  assert.deepEqual(h.loads.at(-1),['start',30]);
});
test('a superseding paused VOD seek retires old loading and cannot be stopped by its old cleanup',async()=>{
  const h=pausedVodHarness();const old=await h.dispatch(30),cleanup=old.localVodSeekCleanup;
  await h.dispatch(60);const count=h.loads.length;
  assert.deepEqual(h.loads,[['stop'],['start',30],['stop'],['start',60]]);
  cleanup();assert.equal(h.loads.length,count,'stale cleanup cannot stop current target loading');
  h.present();assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,1);
  assert.equal(h.posts.filter(x=>x.event==='seek_abandoned').length,1);
});
test('Play during paused target loading retains the loader after settlement',async()=>{
  const h=pausedVodHarness();await h.dispatch(30);
  h.p.wantsPlayback=true;h.p.controlIntentGeneration++;h.video.paused=false;
  h.present();assert.deepEqual(h.loads,[['stop'],['start',30]]);
  assert.equal(h.p.hlsStartup.pausedSeek,null);
});
test('retired paused seek cleanup cannot stop a replacement attachment loader',async()=>{
  const h=pausedVodHarness();const pending=await h.dispatch(30),count=h.loads.length;
  h.p.mediaAttachment={id:2};pending.localVodSeekCleanup();
  assert.equal(h.loads.length,count);
});
test('unbuffered paused VOD seek deadline suspends target loading before bounded reopen',async()=>{
  const h=pausedVodHarness();await h.dispatch(30);
  const deadline=h.timers.find(timer=>timer.ms===h.c.PlaybackPolicy.HLS_STARTUP.seek_deadline_ms);
  assert.ok(deadline);h.c.seekTo=(target,force)=>{h.reopen={target,force};};deadline.fn();
  assert.deepEqual(h.reopen,{target:30,force:true});assert.deepEqual(h.loads.at(-1),['stop']);
});

test('Pause during an unbuffered VOD seek keeps fetching only the pending target',async()=>{
  const h=pausedVodHarness();h.p.wantsPlayback=true;h.video.paused=false;await h.dispatch(30);
  h.p.wantsPlayback=false;h.p.controlIntentGeneration++;h.video.paused=true;
  h.c.pauseHlsStartup(h.p);assert.deepEqual(h.loads.at(-1),['start',30]);
  h.present();assert.deepEqual(h.loads.at(-1),['stop']);assert.equal(h.p.hlsStartup.pausedSeek,null);
});

async function playBeforeBufferedSeekSettlement(initiallyPaused){
  const h=pausedVodHarness(true);
  if(!initiallyPaused){h.p.wantsPlayback=true;h.video.paused=false;}
  await h.dispatch(30);
  if(!initiallyPaused){
    h.p.wantsPlayback=false;h.p.controlIntentGeneration++;h.video.paused=true;
    h.c.pauseHlsStartup(h.p);
  }
  h.p.wantsPlayback=true;h.p.controlIntentGeneration++;h.video.paused=false;
  assert.equal(h.c.resumeHlsStartup(h.video,h.p),true,'buffered target left loading stopped, so Play must restart it');
  assert.deepEqual(h.loads.at(-1),['start',30]);
  const count=h.loads.length;h.present();assert.equal(h.loads.length,count,'settlement after Play must retain loading');
  assert.equal(h.p.controlSeek,null);
}
test('buffered VOD seek resumes loading when Play precedes settlement (seek while paused)',()=>playBeforeBufferedSeekSettlement(true));
test('buffered VOD seek resumes loading when Play precedes settlement (Pause during seek)',()=>playBeforeBufferedSeekSettlement(false));
test('paused VOD seek coverage without a target frame still invokes bounded recovery',async()=>{
  const h=pausedVodHarness();await h.dispatch(30);
  h.c.playbackSeekBufferCovers=()=>true;
  const deadline=h.timers.find(timer=>timer.ms===h.c.PlaybackPolicy.HLS_STARTUP.seek_deadline_ms);
  h.c.seekTo=(target,force)=>{h.reopen={target,force};};deadline.fn();
  assert.deepEqual(h.reopen,{target:30,force:true},'bytes alone cannot terminate target-frame recovery');
  assert.deepEqual(h.loads.at(-1),['stop']);
  assert.equal(h.posts.filter(x=>x.event==='seek_resumed').length,0);
});

// The real waiting producer, surface owner and reducer together. Assignment,
// frame proof and fault retirement must agree on one destination identity.
function waitSurfaceHarness(options={}){
  const h=harness(options),c=h.c,events=[],inputs=[];
  c.PlaybackPolicy={...require('../../crates/plurxd/src/web/playback-policy.js'),
    seekRoute:c.PlaybackPolicy.seekRoute};
  h.p.attemptId='a1';
  c.recordPlaybackSurfaceLog=entry=>events.push(entry);
  c.renderPlaybackSurface=()=>{};
  c.bufferRunway=()=>3;
  c.playbackWaitCopy=()=>({title:'Buffering…',detail:'3s loaded'});
  c.PERSISTENT_STALL_MS=8000;
  vm.runInContext('const PLAYBACK_SURFACE={state:PlaybackPolicy.initialSurfaceState(),surface:null,history:[]};',c);
  for(const name of ['playbackSurfaceGeneration','playbackSurfaceIntent',
    'playbackSurfaceObservationOwner','playbackSurfaceContext','playbackSurfacePromptUp',
    'playbackSurfaceHasLiveFault','playbackSurfaceStep','raisePlaybackSurface','beginWait'])
    vm.runInContext(source(name),c);
  const surfaceStep=c.playbackSurfaceStep;
  c.playbackSurfaceStep=event=>{inputs.push(event);return surfaceStep(event);};
  c.playbackSurfaceStep({attach:h.p.attemptId});
  const wait=source('wirePlayerMedia').match(/v\.addEventListener\("waiting",\(\)=>\{([\s\S]*?)\n  \}\);/);
  assert.ok(wait,'the shipped element waiting listener is present');
  c.waitVideo=h.video;
  vm.runInContext(`function waiting(v=waitVideo){${wait[1]}
}`,c);
  return {...h,events,inputs,
    faults:()=>vm.runInContext('PLAYBACK_SURFACE.state.faults',c),
    surface:()=>vm.runInContext('PLAYBACK_SURFACE.surface',c),
    wait:video=>c.waiting(video),
    step:event=>c.playbackSurfaceStep(event)};
}

test('destination wait raised after execution survives predecessor movement until target frame proof',async()=>{
  const h=waitSurfaceHarness();await h.dispatch(30);
  const pending=h.p.controlSeek;
  assert.equal(h.inputs.filter(e=>Object.hasOwn(e,'intent_settled')).length,0,
    'the execution edge cannot emit early settlement');
  h.wait();h.wait();
  assert.equal(h.faults().length,1,'repeated waiting keeps one destination fault');
  assert.equal(h.faults()[0].intent,pending.sequence);
  h.step({presenting:true,attached:'a0'});
  assert.equal(h.faults().length,1,'retired attachment evidence cannot clear the destination');
  h.step({presenting:true,attached:'a1'});
  assert.equal(h.faults().length,1,'even current attachment movement cannot forgive an unproven target');
  const identity={epoch:h.p.controlPresentationEpoch,attachment:h.p.mediaAttachment,intent:pending.sequence};
  for(const observation of [{...identity,epoch:identity.epoch-1},
    {...identity,attachment:{}},{...identity,intent:identity.intent+1}])
    assert.equal(h.c.settlePlaybackControlSeek(h.video,h.p,30,1,observation),false);
  assert.equal(h.c.settlePlaybackControlSeek(h.video,h.p,10,1,identity),false,
    'a timestamped predecessor frame is still outside the destination');
  h.step({raise:'media_waiting',context:'attached',attached:'a1',intent:pending.sequence+1});
  assert.equal(h.c.settlePlaybackControlSeek(h.video,h.p,30,1,identity),true);
  assert.deepEqual(Array.from(h.faults(),f=>f.intent),[pending.sequence+1],
    'target proof retires only its own intent');
  assert.equal(h.events.filter(e=>e.by==='intent_settled').length,1);
});

test('wait raised after an early settlement event still ends on the actual destination proof',async()=>{
  const h=waitSurfaceHarness();await h.dispatch(30);
  // Replay the old production ordering: settlement was reported at assignment,
  // and the element raised waiting afterwards while the target was unproven.
  h.step({intent_settled:h.p.controlSeek.sequence});
  h.wait();assert.equal(h.faults().length,1);
  assert.equal(h.present(),true);assert.equal(h.faults().length,0);
  h.clock.ms+=500;h.step({tick:true});assert.equal(h.surface().kind,'none');
});

test('attached wait after destination settlement has no inherited intent and retires on presentation',async()=>{
  const h=waitSurfaceHarness();await h.dispatch(30);assert.equal(h.present(),true);
  h.wait();assert.equal(h.faults()[0].intent,null);
  assert.equal(h.faults()[0].observationOwner,'playback-observation:a1');
  h.step({presenting:true,attached:'a1'});
  assert.equal(h.faults().length,0);
  h.step({presenting:false,attached:'a1'});
  await h.dispatch(60);
  h.c.raisePlaybackSurface('media_waiting',{context:'attached'},{once:true});
  assert.equal(h.faults()[0].intent,null,'observation ownership alone never supplies wait intent');
  h.wait();assert.deepEqual(Array.from(h.faults(),f=>f.intent),[null,h.p.controlSeek.sequence],
    'an attached wait cannot deduplicate a new destination wait');
});

test('supersession retires the old wait and only new destination proof retires its replacement',async()=>{
  const h=waitSurfaceHarness();await h.dispatch(30);h.wait();
  const old={epoch:h.p.controlPresentationEpoch,attachment:h.p.mediaAttachment,intent:h.p.controlSeek.sequence};
  await h.dispatch(60);
  assert.equal(h.faults().length,0);
  assert.ok(h.events.some(e=>e.by==='intent_superseded'));
  h.wait();assert.equal(h.faults()[0].intent,h.p.controlSeek.sequence);
  assert.equal(h.c.settlePlaybackControlSeek(h.video,h.p,30,1,old),false);
  assert.equal(h.faults().length,1);
  assert.equal(h.present(),true);assert.equal(h.faults().length,0);
});

test('paused and retired-element waiting cannot cover a paused pending destination',async()=>{
  const h=waitSurfaceHarness();h.video.paused=true;h.p.wantsPlayback=false;
  await h.dispatch(30);
  h.wait();assert.equal(h.faults().length,0);assert.equal(h.p.waitAt,undefined);
  h.video.paused=false;h.p.wantsPlayback=true;
  h.wait({...h.video});assert.equal(h.faults().length,0,'retired element is not the current video');
  h.wait();assert.equal(h.faults().length,1);
  h.p.wantsPlayback=false;h.video.paused=true;h.step({playback_requested:false});
  h.wait();assert.equal(h.faults().length,0,'pause retires the wait and delayed waiting cannot recreate it');
});

test('expired-session Play reopens at the saved destination and its later wait ends at target proof',()=>{
  const h=waitSurfaceHarness(),calls=[];
  h.p.sessionId='expired';h.p.pausedRetirement={sessionId:'expired'};
  h.p.wantsPlayback=false;h.video.paused=true;h.video.currentTime=42;
  Object.assign(h.c,{queuePlaybackTransportCommand(){},rememberPlaybackTransportIntent(){},
    playerWantsPlayback:()=>false,pbPosSec:()=>42,
    seekTo:(...args)=>{calls.push(args);h.c.beginPlaybackControlSeek(h.p,args[0],false);}});
  vm.runInContext(source('togglePlay'),h.c);
  h.c.togglePlay();assert.equal(calls.length,1);assert.equal(calls[0][0],42);
  assert.equal(calls[0][1],true,'expired session requires a reopen');
  assert.equal(h.p.wantsPlayback,true);assert.equal(h.p.pausedRetirement,null);
  // The network/session seam hands the saved intent to the new attachment.
  h.p.attemptId='a2';h.p.mediaAttachment={id:2};h.step({attach:'a2'});
  h.video.paused=false;h.c.markPlaybackControlSeekExecuted(h.p,42,h.video);
  h.wait();assert.equal(h.faults()[0].attached,'a2');
  assert.equal(h.faults()[0].intent,h.p.controlSeek.sequence);
  h.clock.ms+=400;h.step({tick:true});assert.equal(h.surface().kind,'blocking');
  assert.equal(h.present(),true);assert.equal(h.faults().length,0);
  h.step({presenting:false,attached:'a2'});h.video.paused=true;h.p.wantsPlayback=false;
  h.wait();assert.equal(h.surface().kind,'none','a later pause has no surviving buffering overlay');
});
