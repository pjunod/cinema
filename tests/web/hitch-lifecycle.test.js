'use strict';
const {test}=require('node:test'),assert=require('node:assert/strict'),vm=require('node:vm'),fs=require('node:fs');
const source=fs.readFileSync('crates/plurxd/src/web/player/decode-tiers.js','utf8');
const start=source.indexOf('function armHitchDetector('),end=source.indexOf('\n// The spacing between hitches',start);
const boundary=source.indexOf('const HITCH_LIFECYCLE_BOUNDARIES=');
const setup=boundary>=0&&boundary<start?source.slice(boundary,start):'';
assert.ok(start>=0&&end>start,'shipped detector is present');
function detector(){
 const listeners=new Map();let pending,clock=0;
 const video={paused:false,seeking:false,currentTime:0,playbackRate:1,dataset:{},
  requestVideoFrameCallback(){},addEventListener(name,fn){if(!listeners.has(name))listeners.set(name,new Set());listeners.get(name).add(fn);},
  removeEventListener(name,fn){listeners.get(name)?.delete(fn);}};
 const player={};
 const context=vm.createContext({PLAYER:player,document:{getElementById:()=>video},performance:{now:()=>clock},
  PLAYBACK_LIFETIME_HITCHES:0,HITCH_WARMUP:12,HITCH_WINDOW:120,HITCH_NEAR_MS:100,
  HITCH_GAP_FRAMES:2,HITCH_LATE_FLOOR_MS:100,HITCH_LATE_FRACTION:2,HITCH_SLOW_FACTOR:3,
  PlaybackPolicy:{frameMetadataAheadOfClock:()=>false},playbackOwnsAttachedMedia:()=>true,
  settlePlaybackControlSeek(){},reportRateChase(){},queuePlaybackFrame(_v,_p,fn){pending=fn;}});
 vm.runInContext(setup+source.slice(start,end),context);context.armHitchDetector(video);
 const frame=(media,wall,presented)=>{clock=wall;video.currentTime=media;
  pending(wall,{mediaTime:media,presentedFrames:presented,expectedDisplayTime:wall},0,{});};
 const event=name=>{for(const fn of [...(listeners.get(name)||[])])fn();};
 return {video,player,context,frame,event,listeners};
}
test('pause without frame callbacks resets resume timing and preserves genuine faults',()=>{
 const d=detector();
 for(let i=0;i<=15;i++)d.frame(i/24,i*1000/24,i+1);
 d.frame(15/24,16*1000/24,17); // an ordinary duplicate remains a held fault
 assert.equal(d.player.hitches.held,1);
 const lifetime=d.context.PLAYBACK_LIFETIME_HITCHES;
 d.player.hitches.rate=.8;d.player.hitches.renderedFps=19;
 d.video.paused=true;d.event('pause');
 assert.equal(d.player.hitches.rate,null);
 // No frame callback occurs anywhere in the eight-second intentional pause.
 d.video.paused=false;d.event('play');
 d.frame(16/24,8000+17*1000/24,18);
 assert.equal(d.player.hitches.held,1);
 assert.equal(d.player.hitches.late,0);
 assert.equal(d.context.PLAYBACK_LIFETIME_HITCHES,lifetime);
 assert.equal(d.player.hitches.rate,null,'paused wall time does not enter resumed rate');
 d.frame(16/24,8000+18*1000/24,19);
 assert.equal(d.player.hitches.held,2,'an unrelated resumed duplicate is still a fault');
 assert.equal(d.context.PLAYBACK_LIFETIME_HITCHES,lifetime+1);
 for(let i=17;i<=120;i++)d.frame(i/24,8000+(i+2)*1000/24,i+3);
 assert.ok(d.player.hitches.rate>.98&&d.player.hitches.rate<=1.01,
  'resumed rate uses only moving playback');
 d.context.armHitchDetector(d.video);
 assert.equal(d.listeners.get('pause').size,1,'rearming retires old lifecycle listeners');
 assert.equal(d.listeners.get('play').size,1);
 d.player.hitches.rate=1;
 d.context.document.getElementById=()=>({});
 d.event('pause');
 assert.equal(d.player.hitches.rate,1,'a retired element cannot reset the current detector');
 assert.equal(d.listeners.get('pause').size,0);
 assert.equal(d.listeners.get('play').size,0);
});
