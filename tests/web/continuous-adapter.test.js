"use strict";
const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm');
const {webcrypto,createHash}=require('node:crypto');
const {init,media}=require('./continuous-media.fixture.js');
const digest=bytes=>createHash('sha256').update(bytes).digest('hex');
const copy=value=>JSON.parse(JSON.stringify(value));
const uuid=n=>`00000000-0000-4000-8000-${String(n).padStart(12,'0')}`;
const bytes=value=>value.buffer.slice(value.byteOffset,value.byteOffset+value.byteLength);
const pause=()=>new Promise(resolve=>setImmediate(resolve));
async function waitFor(condition){for(let i=0;i<50;i++){if(condition())return;await pause();}assert.fail('expected protocol observation');}
class BufferSurface {
 constructor(){this.listeners=[];this.updating=false;this.appends=0;this.removes=[];}
 addEventListener(type,handler,capture){this.listeners.push({type,handler,capture:!!capture});}
 appendBuffer(){if(this.updating)throw new Error('already updating');this.updating=true;this.appends++;}
 remove(from,through){if(this.updating)throw new Error('already updating');this.updating=true;this.removes.push([from,through]);}
 emit(type){if(type==='updateend')this.updating=false;
  for(const row of this.listeners.filter(row=>row.type===type).sort((a,b)=>Number(b.capture)-Number(a.capture)))row.handler();}
}
function fixture({holdScheduled=false}={}){
 const firstInit=init(),secondInit=init(1,{width:1920,height:1080});
 const firstMedia=media({start:0}),secondMedia=media({start:0,payload:Buffer.from([8,7,6,5,4,3,2,1])});
 const primary={candidate_id:'1'.repeat(32),rendition_id:'a'.repeat(64),init_id:digest(firstInit),width:1280,height:720,
  codec:'avc1.640032',timescale:24000,frame_ticks:1001,segment_ticks:2002,peak_bps:1000000,playlist:`video/${'a'.repeat(64)}/index.m3u8`};
 const target={...primary,candidate_id:'2'.repeat(32),rendition_id:'b'.repeat(64),init_id:digest(secondInit),width:1920,height:1080,
  playlist:`video/${'b'.repeat(64)}/index.m3u8`};
 const family={version:1,mode:'autonomous_reserved',family_id:'c'.repeat(64),master:'master.m3u8',video:[primary,target],audio:null};
 const prefix=`/api/v1/hls/${uuid(1)}/`,bootstrap={generation:uuid(2),control_epoch:1,
  schedule_url:prefix+'quality-schedule',family_url:prefix+'quality-family',family,primary_candidate_id:primary.candidate_id};
 const resources=new Map([[prefix+`video/${primary.rendition_id}/init/${primary.init_id}.mp4`,firstInit],
  [prefix+`video/${target.rendition_id}/init/${target.init_id}.mp4`,secondInit],
  [prefix+`video/${primary.rendition_id}/segment/0.m4s`,firstMedia],[prefix+`video/${target.rendition_id}/segment/0.m4s`,secondMedia]]);
 let frame=null,ledger=null,revision=0,releaseScheduled=null;
 const requests=[],events={},surface=new BufferSurface();
 const video={videoWidth:1280,videoHeight:720,requestVideoFrameCallback(callback){frame=callback;return 1;},cancelVideoFrameCallback(){frame=null;}};
 const player={abr:{},qualityCandidates:[{id:primary.candidate_id,target_height:720},{id:target.candidate_id,target_height:1080}]};
 const Hls={Events:{MANIFEST_PARSED:'manifest',BUFFER_CREATED:'buffers',MEDIA_DETACHED:'detached'}};
 const context=vm.createContext({ArrayBuffer,Uint8Array,DataView,crypto:webcrypto,URL,location:{href:'http://localhost/'},
  Hls,CONTROL_CLIENT_ID:uuid(3),newRequestId:()=>uuid(4),setTimeout,clearTimeout,AbortController,TextDecoder});
 for(const path of ['continuous-media.js','continuous-quality.js'])vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/'+path,'utf8'),context);
 const intervals=family.video.flatMap((row,index)=>Array.from({length:4},(_,ordinal)=>{
  const data=ordinal===0?(index?secondMedia:firstMedia):media({start:ordinal*2002,
   payload:Buffer.from([ordinal,index,6,5,4,3,2,1])});
  resources.set(prefix+`video/${row.rendition_id}/segment/${ordinal}.m4s`,data);
  return {artifact_id:digest(data),rendition_id:row.rendition_id,timescale:24000,
   from_tick:ordinal*2002,through_tick:(ordinal+1)*2002,byte_length:data.length};
 }));
 const exchange=async(url,request)=>{
  requests.push(copy(request));
  if(!ledger)ledger={version:1,generation:request.generation,control_epoch:request.control_epoch,attachment:copy(request.attachment),
   latest_intent_revision:0,accepted_sequence:0,transactions:[]};
  const command=request.transition;let transaction,receipt=null;
  if(command){
   transaction=ledger.transactions.find(row=>row.transaction_id===command.transaction_id);
   const operation=command.operation;
   if(operation.kind==='prepare'){
    ledger.transactions.forEach(row=>row.intent_superseded=true);
    transaction={transaction_id:command.transaction_id,intent_revision:operation.intent_revision,target_rendition_id:operation.target_rendition_id,
     state:'ready',intent_superseded:false,cancel_requested:false,ready:copy(intervals.filter(row=>row.rendition_id===operation.target_rendition_id).slice(0,2)),
     reserved:[],appended:[],ever_appended:false,disposed:[],first_presented_tick:null,first_presented_at_ms:null};
    ledger.transactions.push(transaction);ledger.latest_intent_revision=operation.intent_revision;
   }else if(operation.kind==='scheduled'){
    if(holdScheduled)await new Promise(resolve=>releaseScheduled=()=>{holdScheduled=false;resolve();});
    transaction.reserved.push(...copy(operation.intervals));transaction.state='scheduled';
   }else if(operation.kind==='appended'){
    transaction.appended.push(...copy(operation.intervals));transaction.ever_appended=true;transaction.state='appended';
   }else if(operation.kind==='presented'){
    transaction.first_presented_tick=operation.film_tick;transaction.first_presented_at_ms=operation.observed_at_ms;transaction.state='presented';
   }else if(operation.kind==='cancel_unappended')transaction.cancel_requested=true;
   else if(operation.kind==='disposed'){
    transaction.reserved=transaction.reserved.filter(row=>!operation.artifacts.includes(row.artifact_id));
    transaction.appended=transaction.appended.filter(row=>!operation.artifacts.includes(row.artifact_id));
    transaction.disposed.push(...operation.artifacts);if(transaction.ever_appended&&!transaction.reserved.length)transaction.state='disposed';
   }
   ledger.accepted_sequence=command.sequence;
   receipt={version:1,generation:request.generation,control_epoch:request.control_epoch,attachment:copy(request.attachment),
    accepted_sequence:command.sequence,transaction:copy(transaction)};
  }
  if(request.window){
   transaction=ledger.transactions.find(row=>row.transaction_id===request.window.transaction_id);
   transaction.ready=copy(intervals.filter(row=>row.rendition_id===transaction.target_rendition_id
    &&row.from_tick>=request.window.frontier.through_tick).slice(0,2));
  }
  return {version:1,generation:request.generation,control_epoch:request.control_epoch,attachment:copy(request.attachment),revision:++revision,
   receipt,ledger:copy(ledger)};
 };
 const adapter=context.continuousQualityAdapter(player,video,{current:()=>true},bootstrap,exchange);player.continuousQuality=adapter;
 const hls={levels:family.video.map(row=>({url:['http://localhost'+prefix+row.playlist]})),loadLevel:-1,
  on(event,callback){events[event]=callback;},startLoad(){},emit(event,data){events[event]?.(event,data);}};
 adapter.bind(hls,0);hls.emit('manifest');hls.emit('buffers',{tracks:{video:{buffer:surface}}});
 class BaseLoader {
  constructor(){this.stats={};}
  load(ctx,config,callbacks){callbacks.onProgress?.({},ctx,bytes(resources.get(ctx.url)),{});callbacks.onSuccess({data:bytes(resources.get(ctx.url))},{},ctx,{});}
  abort(){}destroy(){}
 }
 const Loader=adapter.loader(BaseLoader);
 const load=(url,{progress=()=>{}}={})=>new Promise((resolve,reject)=>{
  new Loader({}).load({url},{},{onProgress:progress,onSuccess:response=>resolve(response.data),onError:error=>reject(new Error(error.text))});
 });
 return {adapter,player,surface,hls,requests,load,prefix,primary,target,firstInit,firstMedia,secondInit,secondMedia,
  releaseScheduled:()=>releaseScheduled?.(),present(time,width=1280,height=720){const callback=frame;frame=null;callback?.(0,{mediaTime:time,width,height});}};
}
test('fragment data remains private until exact scheduling acknowledges it',async()=>{
 const f=fixture({holdScheduled:true});await f.load(f.prefix+`video/${f.primary.rendition_id}/init/${f.primary.init_id}.mp4`);
 let released=false,progress=false;
 const loading=f.load(f.prefix+`video/${f.primary.rendition_id}/segment/0.m4s`,{progress:()=>progress=true}).then(()=>released=true);
 await waitFor(()=>f.requests.some(row=>row.transition?.operation.kind==='scheduled'));
 assert.equal(released,false);assert.equal(progress,false);f.releaseScheduled();await loading;assert.equal(released,true);
});
test('completed appends and decoded frames own separate receipts',async()=>{
 const f=fixture();const initBytes=await f.load(f.prefix+`video/${f.primary.rendition_id}/init/${f.primary.init_id}.mp4`);
 const fragment=await f.load(f.prefix+`video/${f.primary.rendition_id}/segment/0.m4s`);
 f.surface.appendBuffer(initBytes);f.surface.emit('updateend');f.surface.appendBuffer(fragment);
 f.present(1001/24000);await pause();assert.equal(f.requests.some(row=>row.transition?.operation.kind==='appended'),false);
 f.surface.emit('updateend');await waitFor(()=>f.requests.some(row=>row.transition?.operation.kind==='appended'));
 assert.equal(f.requests.some(row=>row.transition?.operation.kind==='presented'),false);
 f.present(1001/24000);await waitFor(()=>f.requests.some(row=>row.transition?.operation.kind==='presented'));
 await waitFor(()=>!!f.player.continuousQualityPresented);assert.equal(f.player.continuousQualityPresented.film_tick,1001);
});
test('failed append completion cannot manufacture appended facts',async()=>{
 const f=fixture();const initBytes=await f.load(f.prefix+`video/${f.primary.rendition_id}/init/${f.primary.init_id}.mp4`);
 const fragment=await f.load(f.prefix+`video/${f.primary.rendition_id}/segment/0.m4s`);
 f.surface.appendBuffer(initBytes);f.surface.emit('updateend');f.surface.appendBuffer(fragment);
 f.surface.emit('error');f.surface.emit('updateend');await pause();await pause();
 assert.equal(f.requests.some(row=>row.transition?.operation.kind==='appended'),false);
});
test('optional quality selection preserves the media element and does not remove buffers',async()=>{
 const f=fixture();await f.load(f.prefix+`video/${f.primary.rendition_id}/init/${f.primary.init_id}.mp4`);
 await f.load(f.prefix+`video/${f.primary.rendition_id}/segment/0.m4s`);
 const controller=f.player.continuousQuality;
 assert.equal(await f.adapter.choose(f.target.candidate_id),'continuous');
 assert.equal(f.player.continuousQuality,controller);assert.equal(f.hls.loadLevel,1);assert.deepEqual(f.surface.removes,[]);
 assert.equal(f.player.qualityCandidateId,undefined,'selection is not delivered quality');
});
test('live MediaSource transfer cannot settle disposal',async()=>{
 const f=fixture();await f.load(f.prefix+`video/${f.primary.rendition_id}/init/${f.primary.init_id}.mp4`);
 await f.load(f.prefix+`video/${f.primary.rendition_id}/segment/0.m4s`);
 await assert.rejects(f.adapter.detached(),/detach receipt/);
 f.hls.emit('detached',{transferMedia:true});await pause();assert.equal(f.requests.some(row=>row.transition?.operation.kind==='disposed'),false);
 f.hls.emit('detached',{});await waitFor(()=>f.requests.some(row=>row.transition?.operation.kind==='disposed'));
});

test('rolling appends batch actual facts and present each transaction once',async()=>{
 const f=fixture();const prefix=f.prefix+`video/${f.primary.rendition_id}/`;
 const firstInit=await f.load(prefix+`init/${f.primary.init_id}.mp4`);
 f.surface.appendBuffer(firstInit);f.surface.emit('updateend');
 for(let ordinal=0;ordinal<3;ordinal++){
  const fragment=await f.load(prefix+`segment/${ordinal}.m4s`);
  f.surface.appendBuffer(fragment);f.surface.emit('updateend');
  if(ordinal===0)await waitFor(()=>f.requests.some(row=>row.transition?.operation.kind==='appended'));
  else await pause();
  f.present((ordinal*2002+1001)/24000);
  await waitFor(()=>f.player.continuousQualityPresented?.film_tick===ordinal*2002+1001);
 }
 const commands=f.requests.filter(row=>row.transition).map(row=>row.transition.operation);
 assert.equal(commands.filter(row=>row.kind==='presented').length,1);
 assert.deepEqual(commands.filter(row=>row.kind==='appended').map(row=>row.intervals.length),[1,2]);
});
test('reloading an actually removed artifact flushes disposal before a fresh reservation',async()=>{
 const f=fixture();const prefix=f.prefix+`video/${f.primary.rendition_id}/`;
 const firstInit=await f.load(prefix+`init/${f.primary.init_id}.mp4`),fragment=await f.load(prefix+'segment/0.m4s');
 f.surface.appendBuffer(firstInit);f.surface.emit('updateend');f.surface.appendBuffer(fragment);f.surface.emit('updateend');
 await waitFor(()=>f.requests.some(row=>row.transition?.operation.kind==='appended'));
 f.surface.remove(0,2002/24000);f.surface.emit('updateend');
 await f.load(prefix+'segment/0.m4s');
 const kinds=f.requests.filter(row=>row.transition).map(row=>row.transition.operation.kind);
 const disposed=kinds.indexOf('disposed');assert.ok(disposed>0);
 assert.equal(kinds[disposed+1],'prepare');assert.equal(kinds[disposed+2],'scheduled');
 f.hls.emit('detached',{});await pause();
});
