'use strict';
const {test}=require('node:test'),assert=require('node:assert/strict'),vm=require('node:vm'),fs=require('node:fs');
const {shellSource}=require('./shell-source.js');
const ui=shellSource().bodyScript;
function source(name){const at=ui.indexOf(`function ${name}(`);assert.ok(at>=0,name);return ui.slice(at,ui.indexOf('\n}',at)+2);}
class Events{
  constructor(){this.events=new Map();}
  addEventListener(type,fn){if(!this.events.has(type))this.events.set(type,new Set());this.events.get(type).add(fn);}
  removeEventListener(type,fn){this.events.get(type)?.delete(fn);}
  emit(type){for(const fn of [...(this.events.get(type)||[])])fn({type});}
}
function library({mse=true,mms=true}={}){
  class Standard extends Events{constructor(){super();this.readyState='open';this.sourceBuffers=[];}static isTypeSupported(){return true;}endOfStream(){} }
  class Managed extends Standard{static isTypeSupported(){return false;}}
  const world={URL:{createObjectURL:()=> 'blob:fixture',revokeObjectURL(){}},navigator:{userAgent:'fixture'},
    MediaSource:mse?Standard:undefined,ManagedMediaSource:mms?Managed:undefined};
  const c=vm.createContext({self:world,window:world,navigator:world.navigator,exports:{},module:{exports:{}},
    console,performance,setTimeout,clearTimeout,setInterval,clearInterval,URL:world.URL});
  vm.runInContext(fs.readFileSync('crates/plurxd/src/web/hls.min.js','utf8'),c);
  return {Hls:c.module.exports,Standard,Managed,world,c};
}
function media(){const v=new Events();Object.assign(v,{seeking:false,src:'',currentTime:0,readyState:4,paused:true,
  removeAttribute(){},getAttribute(){return null;},querySelectorAll(){return [];},appendChild(){},load(){}});return v;}
test('bundled MMS resumes seeking and fences late endstreaming per attachment',()=>{
  const {Hls}=library({mse:false});assert.equal(Hls.version,'1.6.19');
  const incumbent=new Hls({enableWorker:false,preferManagedMediaSource:false});
  const successor=new Hls({enableWorker:false,preferManagedMediaSource:false});
  const a=media(),b=media();
  // Execute the real bundled controller, including its attached media listener.
  incumbent.bufferController.onMediaAttaching('',{media:a});
  successor.bufferController.onMediaAttaching('',{media:b});
  a.emit('seeking');incumbent.bufferController._onEndStreaming();
  assert.equal(incumbent.bufferingEnabled,false,'ordinary endstreaming suspends buffering');
  a.seeking=true;a.emit('seeking');assert.equal(incumbent.bufferingEnabled,true);
  incumbent.bufferController._onEndStreaming();assert.equal(incumbent.bufferingEnabled,true,'late endstreaming cannot stop target loading');
  incumbent.bufferController.onMediaDetaching('',{});
  assert.equal(a.events.get('seeking').size,0);
  successor.pauseBuffering();b.seeking=true;b.emit('seeking');assert.equal(successor.bufferingEnabled,true,'retirement leaves the spare listener owned');
  successor.bufferController.onMediaDetaching('',{});assert.equal(b.events.get('seeking').size,0);
  incumbent.destroy();successor.destroy();
});
test('codec probe and main prepared Live TV constructors agree on MSE first and MMS fallback',()=>{
  for(const mse of [true,false]){
    const {Hls,Standard,Managed,world,c}=library({mse});
    c.MSE_VIDEO={h264:['avc1.64001f']};c.MSE_AUDIO={aac:'mp4a.40.2'};c.mseCodecKey=()=> 'h264';
    vm.runInContext(source('mseCanTake'),c);
    assert.equal(c.mseCanTake('h264','aac',8),mse,'probe follows the class with different codec support');
    const hls=new Hls({enableWorker:false,preferManagedMediaSource:false});
    assert.equal(hls.bufferController.appendSource,!mse);
    if(mse) assert.equal(Hls.getMediaSource(),Managed,'static helper remains the MMS-first trap');
    hls.destroy();assert.equal(world.MediaSource,mse?Standard:undefined);
  }
  for(const file of ['player/player.js','player/prepared-replacement.js','pages/live-tv-controls.js'])
    assert.match(fs.readFileSync(`crates/plurxd/src/web/${file}`,'utf8'),/new Hls\(\{preferManagedMediaSource:false,/);
});
test('forward and backward execution reset progress and reject assigned clocks and old frames',()=>{
  for(const target of [1030,970]){
    const v={currentTime:1000,seeking:false,paused:false,ended:false,readyState:4,playbackRate:1};
    const pending={sequence:1,targetMs:target*1000,executed:false,frameFloor:380};
    const p={source:{video_codec:"h264"},started:true,wantsPlayback:true,controlSeek:pending,controlHasFrameCallbacks:true,controlPresentedFrames:380};
    const c=vm.createContext({PLAYER:p,performance:{now:()=>c.now},now:0,document:{hidden:false,getElementById:()=>v},
      PlaybackPolicy:require("../../crates/plurxd/src/web/playback-policy.js"),PERSISTENT_STALL_MS:8000,playbackSurfaceStep(){},playbackSurfaceGeneration:()=>1,playbackOwnsAttachedMedia:()=>true,
      samplePreparedSwitchFrames(){},streamHasVideo:()=>true,watchPlaybackSeekTelemetry(){},completeHlsStartup(){},
      finishPlaybackSeekTelemetry(){},endWait(){},clearStall(){},finishStallRecovery(){},playbackSeekBufferCovers:()=>false,
      bufferRunway:()=>0,persistentWait(){throw Error('generic recovery cannot own missing target');},notifyPlaybackControl(){}});
    for(const name of ['markPlaybackControlSeekExecuted','samplePlaybackPresentationClock','settlePlaybackControlSeek','playbackProgressTick'])vm.runInContext(source(name),c);
    c.playbackProgressTick(v,p);p.controlPresentedFrames=392;v.currentTime=1000.5;c.now=500;c.playbackProgressTick(v,p);
    p.controlPresentedFrames=398;c.now=750;c.markPlaybackControlSeekExecuted(p,target,v);
    pending.localVodSeek=true;pending.localVodSeekFallbackPending=true;pending.localVodSeekCleanup=()=>{pending.localVodSeekFallbackPending=false;};
    v.currentTime=target;v.seeking=true;c.now=1000;c.playbackProgressTick(v,p);
    assert.equal(pending.frameFloor,398);assert.notEqual(pending.localVodPresented,true);assert.equal(pending.localVodSeekFallbackPending,true);
    assert.equal(c.settlePlaybackControlSeek(v,p,target,399),false,'unidentified callback evidence is rejected');
    const observation={epoch:p.controlPresentationEpoch,attachment:p.mediaAttachment,intent:1};
    assert.equal(c.settlePlaybackControlSeek(v,p,1000.75,399,observation),false,'late old timeline frame cannot complete');
    assert.equal(c.settlePlaybackControlSeek(v,p,target,400,observation),false,'frame before seeked is retained');
    v.seeking=false;assert.equal(c.settlePlaybackControlSeek(v,p),true);assert.equal(p.controlSeek,null);
  }
});
test('paused superseding seek renews one observer even while seeking and fences cancelled delivery',()=>{
  const p={controlPresentationEpoch:0,mediaAttachment:{id:1},controlSeek:{sequence:1}},v=media(),callbacks=[],delivered=[];
  v.requestVideoFrameCallback=fn=>{callbacks.push(fn);return callbacks.length;};v.cancelVideoFrameCallback=()=>{};
  const c=vm.createContext({PLAYER:p});vm.runInContext(source('queuePlaybackFrame'),c);
  c.queuePlaybackFrame(v,p,(_now,meta,epoch,identity)=>delivered.push({time:meta.mediaTime,epoch,identity}));
  v.seeking=true;p.controlPresentationEpoch=1;p.controlSeek={sequence:2};p.controlFrameRearm();
  assert.equal(callbacks.length,2,'current attached data can register during a superseding paused seek');
  callbacks[0](1,{mediaTime:10});assert.equal(delivered.length,0);
  callbacks[1](2,{mediaTime:60});assert.equal(delivered.length,1);assert.equal(delivered[0].epoch,1);assert.equal(delivered[0].identity.intent,2);
});
