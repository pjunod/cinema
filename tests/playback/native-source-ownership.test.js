"use strict";
const test=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const control=require('../../crates/plurxd/src/web/playback-control.js');
function shipped(file,name){
 const text=fs.readFileSync('crates/plurxd/src/web/player/'+file,'utf8');
 const found=new RegExp(`(?:async )?function ${name}\\(`).exec(text);assert.ok(found,name);const start=found.index;
 const tail=text.slice(start),next=/\n(?:async )?function /.exec(tail);
 return next?tail.slice(0,next.index):tail;
}
test('first real control POST during delayed native readiness uses the target and no predecessor buffer',async()=>{
 const requests=[],timers=new Map();let ordinal=0;
 const attachment={nativePositionPending:true},player={started:true,wantsPlayback:true,sessionId:'successor',
  offset:1233.816,controlPositionHintSec:1234.749,mediaAttachment:attachment,
  controlSeek:{sequence:1,executed:true,targetMs:1234749},durMs:6000000};
 const video={currentTime:183.395,paused:true,seeking:false,readyState:4,ended:false,playbackRate:1,
  buffered:{length:1,start:()=>180,end:()=>223.195},getVideoPlaybackQuality:()=>({totalVideoFrames:9000,droppedVideoFrames:2})};
 class Reporter extends control.Reporter{
  constructor(config){super({...config,now:()=>0,setTimer(fn){const id=++ordinal;timers.set(id,fn);return id;},clearTimer:id=>timers.delete(id)});}
 }
 const protocol={...control,Reporter};
 const c=vm.createContext({PLAYER:player,window:{PlurxPlaybackControl:protocol},PlurxPlaybackControl:protocol,CONTROL_CLIENT_ID:'22222222-2222-4222-8222-222222222222',
  document:{getElementById:()=>video},performance:{now:()=>0},PERSISTENT_STALL_MS:8000,ENDED_SLACK_SEC:5,TOKEN:null,
  stopPlaybackControl(){},hasPendingPlaybackOpen:()=>false,candidateLinkReceipt:()=>null,
  playbackControlSelection:()=>({quality:{mode:'auto'},audio_track:0,subtitle:{mode:'off',track:null},audio_offset_ms:0,codec:'auto',dynamic_range:'auto'}),
  playbackControlCapabilities:()=>({platform:'web',max_height:2160,codecs:['h264'],dynamic_ranges:['sdr'],dual_player_preparation:false}),
  pendingPlaybackControlAcknowledgement:()=>null,qualityControlSupported:()=>false,
  fetch:(_url,options)=>{requests.push(JSON.parse(options.body));return new Promise(()=>{});}});
 for(const [file,names] of [
  ['stall-diagnosis.js',['playbackOwnsAttachedMedia']],
  ['session.js',['playbackControlBufferedRange','playbackControlObservationOverride','playbackControlSnapshot']],
  ['prepared-switch-measurement.js',['sendPlaybackControl']],
  ['directed-change.js',['startPlaybackControl']],
  ['transport.js',['handlePlaybackPlaying','playbackProgressTick']],
 ])for(const name of names)vm.runInContext(shipped(file,name),c);
 const bootstrap={protocol:control.PROTOCOL,url:'/api/v1/hls/successor/control',generation:'11111111-1111-4111-8111-111111111111',
  control_epoch:1,next_exchange_ms:5000,lease_timeout_ms:300000};
 const reporter=c.startPlaybackControl(video,player,bootstrap);
 assert.ok(reporter,player.controlLastError?.message);
 try{
  assert.equal(requests.length,1,'the real Reporter emits while the source loader owns readiness');
  const first=requests[0];
  assert.equal(first.position_ms,1234749);assert.equal(first.seek_target_ms,1234749);
  assert.equal(first.buffered_from_ms,null);assert.equal(first.buffered_through_ms,1234749);
  assert.equal(first.render_state,'seeking');assert.equal(first.observation.decoder_state,'unknown');
  assert.equal(first.observation.dropped_frames,undefined);
  player.started=false;
  c.handlePlaybackPlaying(video,player);c.playbackProgressTick(video,player);
  assert.equal(player.started,false,'predecessor playing cannot start the successor');
  assert.equal(player.progressWatch,undefined,'predecessor clock does not diagnose successor presentation');
  const blocked=reporter.capture();assert.equal(blocked.snapshot.position_ms,1234749);
  attachment.nativePositionPending=false;player.started=true;video.currentTime=.933;video.buffered={length:1,start:()=>0,end:()=>5};
  const executed=reporter.capture().snapshot;
  assert.equal(executed.position_ms,1234749);assert.equal(executed.buffered_through_ms,1238816);
 }finally{reporter.stop();}
});
