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
// WebCrypto digests settle on the libuv thread pool, not within a fixed number
// of event-loop turns; poll against a wall-clock bound instead.
async function waitFor(condition,ms=5000){
 const deadline=Date.now()+ms;
 for(;;){if(condition())return;if(Date.now()>deadline)break;await new Promise(resolve=>setTimeout(resolve,1));}
 assert.fail('expected protocol observation');
}
class BufferSurface {
 constructor(){this.listeners=[];this.updating=false;this.appends=0;this.removes=[];}
 addEventListener(type,handler,capture){this.listeners.push({type,handler,capture:!!capture});}
 appendBuffer(){if(this.updating)throw new Error('already updating');this.updating=true;this.appends++;}
 remove(from,through){if(this.updating)throw new Error('already updating');this.updating=true;this.removes.push([from,through]);}
 emit(type){if(type==='updateend')this.updating=false;
  for(const row of this.listeners.filter(row=>row.type===type).sort((a,b)=>Number(b.capture)-Number(a.capture)))row.handler();}
}
function fixture({holdScheduled=false,sharedAudio=false,startLevel=undefined}={}){
 const firstInit=init(),secondInit=init(1,{width:1920,height:1080});
 const firstMedia=media({start:0}),secondMedia=media({start:0,payload:Buffer.from([8,7,6,5,4,3,2,1])});
 const primary={candidate_id:'1'.repeat(32),rendition_id:'a'.repeat(64),init_id:digest(firstInit),width:1280,height:720,
  codec:'avc1.640032',timescale:24000,frame_ticks:1001,segment_ticks:2002,peak_bps:1000000,playlist:`video/${'a'.repeat(64)}/index.m3u8`};
 const target={...primary,candidate_id:'2'.repeat(32),rendition_id:'b'.repeat(64),init_id:digest(secondInit),width:1920,height:1080,
  playlist:`video/${'b'.repeat(64)}/index.m3u8`};
 const audioInit=init(1,{type:'soun'}),audio={rendition_id:'d'.repeat(64),init_id:digest(audioInit),
  codec:'mp4a.40.2',timescale:48000,channels:2,peak_bps:192000,playlist:`audio/${'d'.repeat(64)}/index.m3u8`};
 const family={version:1,mode:'autonomous_reserved',family_id:'c'.repeat(64),master:'master.m3u8',video:[primary,target],audio:sharedAudio?audio:null};
 const prefix=`/api/v1/hls/${uuid(1)}/`,bootstrap={generation:uuid(2),control_epoch:1,
  schedule_url:prefix+'quality-schedule',family_url:prefix+'quality-family',family,primary_candidate_id:primary.candidate_id};
 const resources=new Map([[prefix+`video/${primary.rendition_id}/init/${primary.init_id}.mp4`,firstInit],
  [prefix+`video/${target.rendition_id}/init/${target.init_id}.mp4`,secondInit],
  [prefix+`video/${primary.rendition_id}/segment/0.m4s`,firstMedia],[prefix+`video/${target.rendition_id}/segment/0.m4s`,secondMedia]]);
 let frame=null,ledger=null,revision=0,releaseScheduled=null,refusePrepares=0;
 const requests=[],events={},surface=new BufferSurface();
 const video={videoWidth:1280,videoHeight:720,requestVideoFrameCallback(callback){frame=callback;return 1;},cancelVideoFrameCallback(){frame=null;}};
 const player={abr:{},qualityCandidates:[{id:primary.candidate_id,target_height:720},{id:target.candidate_id,target_height:1080}]};
 const Hls={Events:{MANIFEST_PARSED:'manifest',BUFFER_CREATED:'buffers',MEDIA_DETACHED:'detached'}};
 const context=vm.createContext({ArrayBuffer,Uint8Array,DataView,crypto:webcrypto,URL,location:{href:'http://localhost/'},
  Hls,performance:{now:()=>0},CONTROL_CLIENT_ID:uuid(3),newRequestId:()=>uuid(4),setTimeout,clearTimeout,AbortController,TextDecoder});
 for(const path of ['continuous-media.js','continuous-quality.js'])vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/'+path,'utf8'),context);
 // The shipped settlement owner the adapter calls on presentation.
 const directed=fs.readFileSync('crates/plurxd/src/web/player/directed-change.js','utf8');
 for(const name of ['settleContinuousDirectedChange','settleDirectedChange']){
  const begin=directed.indexOf(`\nfunction ${name}(`),end=directed.indexOf('\nfunction ',begin+1);
  vm.runInContext(directed.slice(begin,end),context);
 }
 const intervals=family.video.flatMap((row,index)=>Array.from({length:4},(_,ordinal)=>{
  const data=ordinal===0?(index?secondMedia:firstMedia):media({start:ordinal*2002,
   payload:Buffer.from([ordinal,index,6,5,4,3,2,1])});
  resources.set(prefix+`video/${row.rendition_id}/segment/${ordinal}.m4s`,data);
  return {artifact_id:digest(data),rendition_id:row.rendition_id,timescale:24000,
   from_tick:ordinal*2002,through_tick:(ordinal+1)*2002,byte_length:data.length};
 }));
 const audioIntervals=sharedAudio?Array.from({length:4},(_,ordinal)=>{
  const data=media({start:ordinal*4004+16,sampleTicks:2002,payload:Buffer.from([ordinal,9,6,5,4,3,2,1])});
  resources.set(prefix+`audio/${audio.rendition_id}/segment/${ordinal}.m4s`,data);
  return {artifact_id:digest(data),rendition_id:audio.rendition_id,timescale:48000,
   from_tick:ordinal*4004+16,through_tick:(ordinal+1)*4004+16,byte_length:data.length};
 }):[];
 if(sharedAudio)resources.set(prefix+`audio/${audio.rendition_id}/init/${audio.init_id}.mp4`,audioInit);
 const exchange=async(url,request)=>{
  requests.push(copy(request));
  if(!ledger)ledger={version:1,generation:request.generation,control_epoch:request.control_epoch,attachment:copy(request.attachment),
   latest_intent_revision:0,accepted_sequence:0,transactions:[]};
  const command=request.transition;let transaction,receipt=null;
  // An owner refusal (e.g. transaction Capacity) applies nothing.
  if(command?.operation.kind==='prepare'&&refusePrepares>0){
   refusePrepares--;throw Object.assign(new Error('Continuous quality request refused (409)'),{status:409});
  }
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
  if(sharedAudio&&transaction){
   ledger.shared_audio_rendition_id=audio.rendition_id;ledger.shared_audio_reserved??=[];
   for(const pin of audioIntervals.filter(row=>transaction.ready.some(video=>row.from_tick<video.through_tick*2
    &&row.through_tick>video.from_tick*2)))if(!ledger.shared_audio_reserved.some(old=>old.artifact_id===pin.artifact_id)){
     ledger.shared_audio_reserved.push(copy(pin));}
  }
  return {version:1,generation:request.generation,control_epoch:request.control_epoch,attachment:copy(request.attachment),revision:++revision,
   receipt,ledger:copy(ledger)};
 };
 const adapter=context.continuousQualityAdapter(player,video,{current:()=>true},bootstrap,exchange);player.continuousQuality=adapter;
 const hls={levels:family.video.map(row=>({url:['http://localhost'+prefix+row.playlist]})),loadLevel:-1,startLevel,
  on(event,callback){events[event]=callback;},startLoad(){this.startedLevel=this.startLevel??this.loadLevel;},emit(event,data){events[event]?.(event,data);}};
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
 return {adapter,player,surface,hls,requests,load,prefix,refusePrepares:count=>{refusePrepares=count;},primary,target,audio,audioIntervals,firstInit,firstMedia,secondInit,secondMedia,
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

test('a refused Prepare never becomes the attachment owner at startup',async()=>{
 const f=fixture();const prefix=f.prefix+`video/${f.primary.rendition_id}/`;
 await f.load(prefix+`init/${f.primary.init_id}.mp4`);
 f.refusePrepares(1);
 await assert.rejects(f.load(prefix+'segment/0.m4s'),/refused \(409\)/);
 assert.equal(f.adapter.protocol.ledger.transactions.length,0,'the refusal applied nothing');
 // The hls.js retry of the same fragment prepares afresh; it does not name an
 // identity the ledger never accepted.
 await f.load(prefix+'segment/0.m4s');
 assert.equal(f.adapter.protocol.ledger.transactions.length,1);
});
test('a refused quality Prepare and refused restore keep the incumbent owner loading',async()=>{
 const f=fixture();const prefix=f.prefix+`video/${f.primary.rendition_id}/`;
 await f.load(prefix+`init/${f.primary.init_id}.mp4`);await f.load(prefix+'segment/0.m4s');
 const owner=f.adapter.protocol.ledger.transactions[0].transaction_id;
 f.refusePrepares(2); // the target Prepare and the incumbent restore both refused
 assert.equal(await f.adapter.choose(f.target.candidate_id),'retained_current');
 assert.equal(f.adapter.wanted.rendition_id,f.primary.rendition_id);
 assert.deepEqual(f.adapter.protocol.ledger.transactions.map(row=>row.transaction_id),[owner]);
 // Later incumbent fragments beyond the reserved window still reserve under
 // the accepted owner instead of failing locally as superseded.
 await f.load(prefix+'segment/2.m4s');await f.load(prefix+'segment/3.m4s');
 const scheduled=f.requests.filter(row=>row.transition?.operation.kind==='scheduled');
 assert.ok(scheduled.length>=2);assert.ok(scheduled.every(row=>row.transition.transaction_id===owner));
});
test('a re-Prepare of the chosen target settles the choice when its rendition is presented',async()=>{
 const f=fixture();const primary=f.prefix+`video/${f.primary.rendition_id}/`,target=f.prefix+`video/${f.target.rendition_id}/`;
 f.surface.appendBuffer(await f.load(primary+`init/${f.primary.init_id}.mp4`));f.surface.emit('updateend');
 f.surface.appendBuffer(await f.load(primary+'segment/0.m4s'));f.surface.emit('updateend');
 assert.equal(await f.adapter.choose(f.target.candidate_id),'continuous');
 const ledger=f.adapter.protocol.ledger,chosen=ledger.transactions.find(row=>row.intent_revision===ledger.latest_intent_revision);
 f.player.directedChange={settled:false,outcome:'continuous',continuousCandidateId:f.target.candidate_id,
  continuousTransactionId:chosen.transaction_id,autoMove:null};
 f.surface.appendBuffer(await f.load(target+`init/${f.target.init_id}.mp4`));f.surface.emit('updateend');
 f.surface.appendBuffer(await f.load(target+'segment/0.m4s'));f.surface.emit('updateend');
 await waitFor(()=>f.requests.some(row=>row.transition?.operation.kind==='appended'&&row.transition.transaction_id===chosen.transaction_id));
 // An independent removal disposes the target interval; reloading it makes
 // the adapter re-Prepare the unchanged target under a new transaction.
 f.surface.remove(0,2002/24000);f.surface.emit('updateend');
 f.surface.appendBuffer(await f.load(target+'segment/0.m4s'));f.surface.emit('updateend');
 const successor=f.adapter.protocol.ledger.transactions.at(-1);
 assert.notEqual(successor.transaction_id,chosen.transaction_id);
 assert.equal(successor.target_rendition_id,f.target.rendition_id);
 await waitFor(()=>{f.present(1001/24000,1920,1080);return f.player.directedChange.settled;});
 assert.equal(f.player.directedChange.outcome,'committed','presentation of the chosen rendition settles the choice');
 f.hls.emit('detached',{});await pause();
});
test('rolling appends batch actual facts and present each transaction once',async()=>{
 const f=fixture();const prefix=f.prefix+`video/${f.primary.rendition_id}/`;
 const firstInit=await f.load(prefix+`init/${f.primary.init_id}.mp4`);
 f.surface.appendBuffer(firstInit);f.surface.emit('updateend');
 for(let ordinal=0;ordinal<3;ordinal++){
  const fragment=await f.load(prefix+`segment/${ordinal}.m4s`);
  f.surface.appendBuffer(fragment);f.surface.emit('updateend');
  if(ordinal===0)await waitFor(()=>f.requests.some(row=>row.transition?.operation.kind==='appended'));
  // requestVideoFrameCallback fires on every displayed frame. Completed
  // append facts resolve asynchronously (SHA-256), so a frame displayed
  // before they settle is matched by a later callback, not dropped forever.
  await waitFor(()=>{f.present((ordinal*2002+1001)/24000);
   return f.player.continuousQualityPresented?.film_tick===ordinal*2002+1001;});
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


test('controlled metadata keeps the same strict immutable family contract',()=>{
 const source=fixture().adapter.family;
 const context=vm.createContext({AbortController,TextDecoder,setTimeout,clearTimeout});
 vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-quality.js','utf8'),context);
 assert.equal(context.continuousQualityFamily({...source,mode:'controlled'}),true);
 assert.equal(context.continuousQualityFamily({...source,mode:'autonomous_reserved'}),true);
 assert.equal(context.continuousQualityFamily({...source,mode:'unbounded'}),false);
 const invalid=copy(source);invalid.mode='controlled';invalid.video[1].init_id='unverified';
 assert.equal(context.continuousQualityFamily(invalid),false);
});


test('fragment loader destruction stays silent across hls abort re-entry',()=>{
 const f=fixture();let aborts=0,destroys=0,callbacks;let loader;
 class Base {
  constructor(){this.stats={};}
  load(ctx,config,handlers){callbacks=handlers;}
  abort(){aborts++;callbacks.onAbort();}
  destroy(){destroys++;}
 }
 const Loader=f.adapter.loader(Base);loader=new Loader({});
 loader.load({url:f.prefix+'pending'}, {}, {onAbort(){loader.destroy();}});
 loader.abort();loader.abort();loader.destroy();
 assert.equal(aborts,1);assert.equal(destroys,1);
 loader=new Loader({});loader.load({url:f.prefix+'completed'}, {}, {onAbort(){assert.fail('destroy must not emit abort');}});
 loader.destroy();loader.destroy();loader.abort();
 assert.equal(aborts,1);assert.equal(destroys,2);
});


test('aborted outgoing fragment callbacks cannot authorize after a quality switch',async()=>{
 const f=fixture();await f.load(f.prefix+`video/${f.primary.rendition_id}/init/${f.primary.init_id}.mp4`);
 let callbacks,ctx,loader,aborts=0;
 class Base {
  constructor(){this.stats={};}
  load(context,config,handlers){ctx=context;callbacks=handlers;}
  abort(){aborts++;callbacks.onAbort();}
  destroy(){}
 }
 const Loader=f.adapter.loader(Base);loader=new Loader({});
 loader.load({url:f.prefix+`video/${f.primary.rendition_id}/segment/0.m4s`},{},{
  onAbort(){assert.equal(f.hls.loadLevel,1,'future level is selected before abort notification');loader.destroy();},
  onSuccess(){assert.fail('outgoing payload must stay private');},
  onError(){assert.fail('an aborted outgoing payload must not fail the current stream');},
 });
 assert.equal(await f.adapter.choose(f.target.candidate_id),'continuous');assert.equal(aborts,1);
 const count=f.requests.length;callbacks.onSuccess({data:bytes(f.firstMedia)},{},ctx,{});
 await pause();await pause();assert.equal(f.requests.length,count);
 assert.equal(f.player.continuousQualityObservation,undefined);
});


test('AAC just beyond a video boundary reserves a forward owner window',async()=>{
 const f=fixture({sharedAudio:true});
 await f.load(f.prefix+`video/${f.primary.rendition_id}/init/${f.primary.init_id}.mp4`);
 await f.load(f.prefix+`audio/${f.audio.rendition_id}/init/${f.audio.init_id}.mp4`);
 await f.load(f.prefix+`video/${f.primary.rendition_id}/segment/0.m4s`);
 assert.equal(f.adapter.protocol.ledger.shared_audio_reserved.some(row=>row.artifact_id===f.audioIntervals[2].artifact_id),false);
 await f.load(f.prefix+`audio/${f.audio.rendition_id}/segment/2.m4s`);
 assert.equal(f.requests.find(row=>row.window)?.window.frontier.through_tick,4004);
 assert.equal(f.adapter.protocol.ledger.shared_audio_reserved.some(row=>row.artifact_id===f.audioIntervals[2].artifact_id),true);
 assert.equal(f.player.continuousQualityObservation,undefined);
});


test('completed network awaiting authorization still aborts the hls fragment state',async()=>{
 const f=fixture();await f.load(f.prefix+`video/${f.primary.rendition_id}/init/${f.primary.init_id}.mp4`);
 await f.load(f.prefix+`video/${f.primary.rendition_id}/segment/0.m4s`);
 let callbacks,ctx,loader,streamState='frag_loading',exposed=false;
 class FinishedNetwork {
  constructor(){this.stats={aborted:false,loading:{end:10}};}
  load(context,config,handlers){ctx=context;callbacks=handlers;}
  // A completed XHR abort does not set network stats. hls.js nevertheless
  // needs the wrapper's logical fragment abort to release FRAG_LOADING.
  abort(){callbacks.onAbort(this.stats,ctx);}
  destroy(){}
 }
 const Loader=f.adapter.loader(FinishedNetwork);loader=new Loader({});
 loader.load({url:f.prefix+`video/${f.primary.rendition_id}/segment/1.m4s`},{},{
  onAbort(stats){if(stats.aborted)streamState='idle';loader.destroy();},
  onSuccess(){exposed=true;},onError(){assert.fail('aborted authorization cannot fail the stream');},
 });
 const choosing=f.adapter.choose(f.target.candidate_id);
 callbacks.onSuccess({data:bytes(f.firstMedia)},loader.stats,ctx,{});
 assert.equal(await choosing,'continuous');await pause();await pause();
 assert.equal(streamState,'idle');assert.equal(exposed,false);
 assert.equal(f.player.continuousQualityObservation,undefined);
});

test('continuous startup overrides a seeded start level with its reserved primary',()=>{
 // hls.js keeps an already seeded startLevel when loadLevel changes.
 const f=fixture({startLevel:1});
 assert.equal(f.hls.startLevel,0);
 assert.equal(f.hls.startedLevel,0);
 assert.equal(f.hls.loadLevel,0);
});
test('future loader resumes its retained quality rather than a stale startup level',async()=>{
 const f=fixture();
 assert.equal(await f.adapter.choose(f.target.candidate_id),'continuous');
 f.hls.startLoad();
 assert.equal(f.hls.startLevel,1);
 assert.equal(f.hls.startedLevel,1);
 assert.equal(f.hls.loadLevel,1);
 assert.equal(f.player.continuousQuality,f.adapter);
 assert.deepEqual(f.surface.removes,[]);
});

test('controlled HLS errors preserve Plurx quality authority',()=>{
 const source=fs.readFileSync('crates/plurxd/src/web/player/player.js','utf8');
 const begin=source.indexOf('function constructHls('),end=source.indexOf('\nfunction wireHlsObservers(',begin);
 assert.ok(begin>=0&&end>begin);
 const player={continuousQualityBootstrap:{},abr:{}};
 const adapter={loader:base=>base,bind(){}};
 class HlsFixture{
  static DefaultConfig={loader:class {}};
  constructor(config){this.config=config;}
  loadSource(){}attachMedia(){}
 }
 const construct=new Function('Hls','continuousQualityAdapter','PlaybackPolicy',
  'vodClientContract','createHlsStartupLoader','PLAYER','TOKEN','observeQualityResourceTimings',
  source.slice(begin,end)+';return constructHls;')(HlsFixture,()=>adapter,
   {HLS_STARTUP:{manifest_load_policy:{}},bandwidthSeedBps:()=>0},
   ()=>({fragLoadPolicy:{}}),base=>base,player,null,()=>{});
 const built=construct({player,playlistUrl:'/owned/master.m3u8',attachment:{}},
  {fwd:60,back:90}, {},990,()=>true);
 const VendoredHls=require('../../crates/plurxd/src/web/hls.min.js');
 let loaded=1,manual=1;
 const hls={config:built.config,levels:[0,1].map(()=>({loadError:0,fragmentError:0,
   codecSet:'avc1',audioCodec:'mp4a.40.2',attrs:{}})),minAutoLevel:0,maxAutoLevel:1,
  logger:{log(){},warn(){},info(){},debug(){},trace(){},error(){}},on(){},off(){},
  get loadLevel(){return loaded;},set loadLevel(value){loaded=value;manual=value;},
  get manualLevel(){return manual;},get autoLevelEnabled(){return manual===-1;}};
 const controller=new VendoredHls.DefaultConfig.errorController(hls);
 controller.getLevelSwitchAction({details:VendoredHls.ErrorDetails.FRAG_LOAD_ERROR,frag:{type:'main'}});
 assert.equal(hls.manualLevel,1,'a library retry must not replace the owned rung with library Auto');
});


test('continuous Auto selection uses bound video and shared audio delivery budgets',()=>{
 const context=vm.createContext({});
 vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-quality.js','utf8'),context);
 const policy=require('../../crates/plurxd/src/web/playback-policy.js');
 const row=(digit,height,peak)=>({id:digit.repeat(32),recipe_digest:Array(32).fill(0),route:'encode',
  width:height===720?1280:1920,height,target_height:height,peak_bps:peak,
  decoder_compatible:true,sustainable:true});
 const raw=[row('1',720,6160000),row('2',1080,12160000)];
 const family={video:[{candidate_id:raw[0].id,peak_bps:17600000},
  {candidate_id:raw[1].id,peak_bps:33600000}],audio:{peak_bps:199734}};
 const bound=context.continuousQualityBoundCatalog(raw,family);
 const select=candidates=>policy.selectQualityCandidate({candidates,linkLimitBps:20000000});
 assert.equal(select(raw).id,raw[1].id);
 assert.equal(select(bound).id,raw[0].id);
 assert.equal(bound[0].peak_bps,17799734);
 assert.equal(bound[1].peak_bps,33799734);
 assert.equal(raw[0].peak_bps,6160000);
 assert.notEqual(bound[0],raw[0]);
 const unrelated={id:'3'.repeat(32),route:'original',peak_bps:5000000};
 assert.equal(context.continuousQualityBoundCatalog([unrelated],family)[0],unrelated);
 assert.equal(context.continuousQualityBoundCatalog(raw,null),raw);
 assert.throws(()=>context.continuousQualityBoundCatalog(raw,{...family,audio:{peak_bps:Number.MAX_SAFE_INTEGER}}),/budget shape/);
});
