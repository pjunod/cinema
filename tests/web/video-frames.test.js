'use strict';
const {test}=require('node:test'),assert=require('node:assert/strict'),vm=require('node:vm'),fs=require('node:fs');
const source=fs.readFileSync('crates/plurxd/src/web/player/video-frames.js','utf8');

class Video{
 constructor({src='https://media.example/movie.m3u8',readyState=4,paused=false,seeking=false}={}){
  Object.assign(this,{src,currentSrc:src,readyState,paused,seeking});
  this.listeners=new Map();this.registrations=[];this.callbacks=new Map();this.pending=new Map();this.cancelled=[];this.nextId=1;
 }
 requestVideoFrameCallback(callback){
  const id=this.nextId++;this.registrations.push(id);this.callbacks.set(id,callback);this.pending.set(id,callback);return id;
 }
 cancelVideoFrameCallback(id){this.cancelled.push(id);this.pending.delete(id);}
 addEventListener(type,callback){if(!this.listeners.has(type))this.listeners.set(type,new Set());this.listeners.get(type).add(callback);}
 removeEventListener(type,callback){this.listeners.get(type)?.delete(callback);}
 emit(type){for(const callback of [...(this.listeners.get(type)||[])])callback({type});}
 deliver(id,metadata={mediaTime:1}){
  const callback=this.callbacks.get(id);assert.ok(callback,`request ${id} was registered`);
  this.pending.delete(id);callback(10,metadata);
 }
 listenerCount(){return [...this.listeners.values()].reduce((count,set)=>count+set.size,0);}
}
function fixture(options){
 const video=new Video(options),context=vm.createContext({});
 vm.runInContext(source,context,{filename:'video-frames.js'});
 return {video,request:(callback)=>context.requestPlaybackVideoFrame(video,callback),
  cancel:(token)=>context.cancelPlaybackVideoFrame(video,token)};
}

test('one native request serves subscribers and canceling a sibling preserves the remaining owner',()=>{
 const f=fixture(),delivered=[];
 const first=f.request(()=>delivered.push('first'));
 f.request(()=>delivered.push('second'));
 assert.deepEqual(f.video.registrations,[1]);
 f.cancel(first);
 assert.deepEqual(f.video.cancelled,[],'the shared native request remains while a subscriber is active');
 f.video.deliver(1);
 assert.deepEqual(delivered,['second']);
 assert.equal(f.video.listenerCount(),0,'the final one-shot delivery releases lifecycle listeners');
});

test('reentrant subscription joins only the next native frame after current fan-out completes',()=>{
 const f=fixture(),delivered=[];
 f.request(()=>{delivered.push('first');f.request(()=>delivered.push('reentrant'));});
 f.request(()=>delivered.push('sibling'));
 assert.deepEqual(f.video.registrations,[1]);
 f.video.deliver(1);
 assert.deepEqual(delivered,['first','sibling']);
 assert.deepEqual(f.video.registrations,[1,2],'reentrant work gets one request after fan-out');
 f.video.deliver(2);
 assert.deepEqual(delivered,['first','sibling','reentrant']);
 assert.equal(f.video.listenerCount(),0);
});

test('a canceled native delivery is generation-fenced from a replacement subscriber',()=>{
 const f=fixture(),delivered=[];
 const old=f.request(()=>delivered.push('old'));
 f.cancel(old);
 const current=f.request(()=>delivered.push('current'));
 assert.deepEqual(f.video.registrations,[1,2]);
 f.video.deliver(1);
 assert.deepEqual(delivered,[],'a queued delivery from the canceled native request is stale');
 f.video.deliver(2);
 assert.deepEqual(delivered,['current']);
 assert.equal(f.video.listenerCount(),0);
 f.cancel(current);
});

test('native active playback waits through seeking and HAVE_CURRENT_DATA, then renews once at readiness',()=>{
 const f=fixture({readyState:4,paused:false,seeking:false}),delivered=[];
 f.request((_,metadata)=>delivered.push(metadata.mediaTime));
 assert.deepEqual(f.video.registrations,[1]);
 f.video.seeking=true;f.video.emit('seeking');
 assert.deepEqual(f.video.cancelled,[1]);
 f.video.readyState=2;f.video.emit('loadeddata');
 assert.deepEqual(f.video.registrations,[1],'loadeddata during a seek cannot rearm active playback');
 f.video.seeking=false;f.video.emit('seeked');f.video.emit('playing');
 assert.deepEqual(f.video.registrations,[1],'active playback needs HAVE_FUTURE_DATA after seeked');
 f.video.readyState=3;f.video.emit('canplay');f.video.emit('canplay');f.video.emit('playing');
 assert.deepEqual(f.video.registrations,[1,2],'duplicate readiness for one discontinuity creates one request');
 f.video.deliver(2,{mediaTime:42});
 assert.deepEqual(delivered,[42]);

 // A later source-data discontinuity gets one new empty-list transition too.
 f.request(()=>delivered.push('next-discontinuity'));
 assert.deepEqual(f.video.registrations,[1,2,3]);
 f.video.readyState=2;f.video.emit('loadeddata');
 assert.deepEqual(f.video.cancelled,[1,3]);
 f.video.readyState=3;f.video.emit('canplay');f.video.emit('canplay');
 assert.deepEqual(f.video.registrations,[1,2,3,4]);
 f.video.readyState=0;f.video.emit('emptied');
 f.video.readyState=3;f.video.emit('canplay');f.video.emit('canplay');
 assert.deepEqual(f.video.registrations,[1,2,3,4,5]);
});

test('paused native seek retains a valid landing delivered before seeked',()=>{
 const f=fixture({readyState:2,paused:true,seeking:true}),delivered=[];
 f.request((_,metadata)=>delivered.push(metadata.mediaTime));
 assert.deepEqual(f.video.registrations,[1]);
 f.video.deliver(1,{mediaTime:9});
 assert.deepEqual(delivered,[9],'the actual landing callback is preserved for subscriber proof');
 f.video.seeking=false;f.video.emit('seeked');
 assert.deepEqual(f.video.registrations,[1]);
});
test('a paused request still pending from seeking is renewed once at seek completion',()=>{
 const f=fixture({readyState:2,paused:true,seeking:true}),delivered=[];
 f.request((_,metadata)=>delivered.push(metadata.mediaTime));
 f.video.seeking=false;f.video.emit('seeked');f.video.emit('canplay');
 assert.deepEqual(f.video.cancelled,[1]);assert.deepEqual(f.video.registrations,[1,2]);
 f.video.deliver(1,{mediaTime:1});assert.deepEqual(delivered,[]);
 f.video.deliver(2,{mediaTime:9});assert.deepEqual(delivered,[9]);
});

test('MSE blob sources keep their callback across seek events',()=>{
 const f=fixture({src:'blob:fixture',readyState:4,paused:false,seeking:false}),delivered=[];
 f.request((_,metadata)=>delivered.push(metadata.mediaTime));
 f.video.seeking=true;f.video.emit('seeking');
 f.video.readyState=2;f.video.emit('loadeddata');
 f.video.seeking=false;f.video.emit('seeked');f.video.emit('canplay');
 assert.deepEqual(f.video.registrations,[1]);
 assert.deepEqual(f.video.cancelled,[]);
 assert.deepEqual([...f.video.pending.keys()],[1]);
 f.video.deliver(1,{mediaTime:3});
 assert.deepEqual(delivered,[3]);
});

test('canceling the last subscriber cancels the request and removes every event listener',()=>{
 const f=fixture(),token=f.request(()=>{});
 assert.equal(f.video.listenerCount(),7);
 f.cancel(token);
 assert.deepEqual(f.video.cancelled,[1]);
 assert.equal(f.video.pending.size,0);
 assert.equal(f.video.listenerCount(),0);
});

test('request ID zero is owned and canceled exactly like any other native request',()=>{
 const f=fixture();f.video.nextId=0;
 const token=f.request(()=>assert.fail('canceled request cannot deliver'));
 assert.deepEqual(f.video.registrations,[0]);f.cancel(token);
 assert.deepEqual(f.video.cancelled,[0]);assert.equal(f.video.listenerCount(),0);
 f.video.deliver(0);assert.equal(f.video.registrations.length,1);
});
test('a failed cancellation retains the sole browser request until its stale delivery settles',()=>{
 const f=fixture(),delivered=[];
 f.request(()=>delivered.push('first'));
 const cancel=f.video.cancelVideoFrameCallback.bind(f.video);
 f.video.cancelVideoFrameCallback=()=>{throw Error('cancellation unavailable');};
 f.video.seeking=true;f.video.emit('seeking');
 f.request(()=>delivered.push('second'));
 f.video.seeking=false;f.video.emit('seeked');
 assert.deepEqual(f.video.registrations,[1],'unconfirmed cancellation cannot authorize a second request');
 f.video.deliver(1);assert.deepEqual(delivered,[],'old delivery never fans out as current proof');
 assert.deepEqual(f.video.registrations,[1,2]);
 f.video.cancelVideoFrameCallback=cancel;
 f.video.deliver(2);assert.deepEqual(delivered,['first','second']);
 assert.equal(f.video.listenerCount(),0);
});
test('registration exceptions retire only the failed subscription and leave no native ownership',()=>{
 const f=fixture();f.video.requestVideoFrameCallback=()=>{throw Error('request unavailable');};
 assert.throws(()=>f.request(()=>{}),/request unavailable/);
 assert.equal(f.video.listenerCount(),0);
 f.video.requestVideoFrameCallback=Video.prototype.requestVideoFrameCallback;
 let delivered=0;f.request(()=>delivered++);f.video.deliver(1);
 assert.equal(delivered,1);assert.equal(f.video.listenerCount(),0);
});
test('a discontinuity during fan-out fences remaining subscribers from the delivered old frame',()=>{
 const f=fixture(),delivered=[];
 f.request(()=>{delivered.push('first');f.video.seeking=true;f.video.emit('seeking');});
 f.request(()=>delivered.push('second'));
 f.video.deliver(1);assert.deepEqual(delivered,['first']);
 f.video.seeking=false;f.video.emit('seeked');f.video.deliver(2);
 assert.deepEqual(delivered,['first','second']);
});
