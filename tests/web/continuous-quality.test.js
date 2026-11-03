"use strict";
const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm');
const context=vm.createContext({AbortController,TextDecoder,setTimeout,clearTimeout});
vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-quality.js','utf8'),context);
const {continuousQualityProtocol:protocol,continuousQualityResponse:valid}=context;
const uuid=n=>`00000000-0000-4000-8000-${String(n).padStart(12,'0')}`;
const bootstrap={generation:uuid(1),control_epoch:3,schedule_url:'/api/v1/hls/session/quality-schedule'};
const attachment={client_instance_id:uuid(2),lifetime_id:'one-film',attachment_id:uuid(3),family_id:'a'.repeat(64)};
const interval={artifact_id:'b'.repeat(64),rendition_id:'c'.repeat(64),timescale:24000,from_tick:0,through_tick:48048,byte_length:1000};
const clone=value=>JSON.parse(JSON.stringify(value));
function answer(request,revision=1){
 const transition=request.transition;
 const transaction={transaction_id:transition?.transaction_id||uuid(4),intent_revision:1,target_rendition_id:interval.rendition_id,
  state:'ready',intent_superseded:false,cancel_requested:false,ready:[interval],reserved:[],appended:[],
  ever_appended:false,disposed:[],first_presented_tick:null,first_presented_at_ms:null};
 const ledger={version:1,generation:request.generation,control_epoch:request.control_epoch,attachment:clone(request.attachment),
  accepted_sequence:transition?.sequence||0,latest_intent_revision:1,transactions:[clone(transaction)]};
 const receipt=transition?{version:1,generation:request.generation,control_epoch:request.control_epoch,
  accepted_sequence:transition.sequence,attachment:clone(request.attachment),transaction}:null;
 return {version:1,generation:request.generation,control_epoch:request.control_epoch,attachment:clone(request.attachment),revision,ledger,receipt};
}
test('lost acknowledgement retries exactly before a newer sequence',async()=>{
 const requests=[];let lost=true;
 const client=protocol(bootstrap,attachment,async(url,request)=>{
  requests.push(clone(request));if(lost){lost=false;throw new Error('ack lost');}return answer(request,requests.length);
 });
 const first=client.transition(uuid(4),{kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id},{timescale:24000,through_tick:0});
 const second=client.transition(uuid(4),{kind:'scheduled',intervals:[interval]});
 await Promise.all([first,second]);
 assert.deepEqual(requests[0],requests[1]);assert.equal(requests[2].transition.sequence,2);
 assert.equal(client.pending,null);
});
test('queued operation mutation cannot change the transmitted command',async()=>{
 let release;const paused=new Promise(resolve=>release=resolve),requests=[];
 const client=protocol(bootstrap,attachment,async(url,request)=>{requests.push(clone(request));if(requests.length===1)await paused;return answer(request,requests.length);});
 const first=client.snapshot();const operation={kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id};
 const frontier={timescale:24000,through_tick:0};const next=client.transition(uuid(4),operation,frontier);
 operation.target_rendition_id='d'.repeat(64);frontier.through_tick=999;
 release();await Promise.all([first,next]);
 assert.equal(requests[1].transition.operation.target_rendition_id,interval.rendition_id);assert.equal(requests[1].frontier.through_tick,0);
});
test('an unresolved exchange stays ahead of newer work',async()=>{
 let refuse=true;const requests=[];
 const client=protocol(bootstrap,attachment,async(url,request)=>{requests.push(clone(request));if(refuse)throw new Error('disconnected');return answer(request,requests.length);});
 await assert.rejects(client.transition(uuid(4),{kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id},{timescale:24000,through_tick:0}));
 assert.ok(client.pending);refuse=false;
 await client.transition(uuid(4),{kind:'scheduled',intervals:[interval]});
 assert.deepEqual(requests[0],requests[2]);assert.equal(requests[3].transition.sequence,2);
});
test('wrong owner, attachment and unreserved append facts refuse receipts',()=>{
 const request={version:1,...bootstrap,attachment,transition:{sequence:1,transaction_id:uuid(4),operation:{kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id}}};
 const response=answer(request);assert.equal(valid(response,request),true);
 const owner=clone(response);owner.control_epoch++;assert.equal(valid(owner,request),false);
 const wrong=clone(response);wrong.ledger.attachment.attachment_id=uuid(99);assert.equal(valid(wrong,request),false);
 const unreserved=clone(response);unreserved.ledger.transactions[0].ever_appended=true;unreserved.ledger.transactions[0].appended=[interval];
 assert.equal(valid(unreserved,request),false);
});

test('durable End proof reconciles a lost reservation ack before named disposal',async()=>{
 let durable=null,ended=false,revision=0;const requests=[];
 const client=protocol(bootstrap,attachment,async(url,request)=>{
  requests.push(clone(request));const command=request.transition;
  if(command?.operation.kind==='prepare'){const reply=answer(request,++revision);durable=clone(reply.ledger);return reply;}
  if(command?.operation.kind==='scheduled'){
   // The first send applies durably and End follows; the identical replay is
   // answered idempotently, but both acknowledgements are lost in transit, so
   // the command stays pending and only a durable End proof can settle it.
   if(!ended){
    durable.accepted_sequence=command.sequence;durable.transactions[0].reserved=[clone(interval)];durable.transactions[0].state='scheduled';
    ended=true;
   }
   throw new Error('reservation accepted but acknowledgement lost');
  }
  let receipt=null;
  if(command?.operation.kind==='disposed'){
   durable.accepted_sequence=command.sequence;durable.transactions[0].reserved=[];durable.transactions[0].disposed=command.operation.artifacts;
   receipt={version:1,generation:request.generation,control_epoch:request.control_epoch,attachment:clone(request.attachment),
    accepted_sequence:command.sequence,transaction:clone(durable.transactions[0])};
  }
  return {version:1,generation:request.generation,control_epoch:request.control_epoch,attachment:clone(request.attachment),
   revision:++revision,terminal:true,receipt,ledger:clone(durable)};
 });
 await client.transition(uuid(4),{kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id},{timescale:24000,through_tick:0});
 await assert.rejects(client.transition(uuid(4),{kind:'scheduled',intervals:[interval]}),/acknowledgement lost/);
 assert.deepEqual(requests[1],requests[2],'a lost acknowledgement retries the identical command once');
 assert.ok(client.pending);assert.equal(client.ledger.transactions[0].reserved.length,0,'lost ack is not absence evidence');
 const observed=await client.reconcileTerminal();assert.equal(client.pending,null);assert.equal(observed.transactions[0].reserved.length,1);
 await client.transition(uuid(4),{kind:'disposed',artifacts:[interval.artifact_id]});
 assert.equal(requests.at(-1).transition.sequence,3);assert.equal(client.ledger.transactions[0].reserved.length,0);
});

test('an active snapshot cannot clear an uncertain command as terminal',async()=>{
 let snapshots=false;
 const client=protocol(bootstrap,attachment,async(url,request)=>{
  if(!snapshots)throw new Error('disconnected');return {...answer(request,3),terminal:false};
 });
 await assert.rejects(client.transition(uuid(4),{kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id},{timescale:24000,through_tick:0}));
 const pending=clone(client.pending);snapshots=true;
 await assert.rejects(client.reconcileTerminal(),/durable End proof/);assert.deepEqual(clone(client.pending),pending);
});


test('normal ninety-second video and AAC buffers fit the bounded receipt',()=>{
 const request={version:1,...bootstrap,attachment};const response=answer(request);
 const segments=(rendition,timescale,first)=>Array.from({length:45},(_,index)=>({...interval,
  artifact_id:(first+index).toString(16).padStart(64,'0'),rendition_id:rendition,timescale,
  from_tick:index*timescale*2,through_tick:(index+1)*timescale*2}));
 const tx=response.ledger.transactions[0];tx.reserved=segments(interval.rendition_id,24000,1);
 tx.appended=clone(tx.reserved);tx.ever_appended=true;tx.state='appended';
 response.ledger.shared_audio_rendition_id='e'.repeat(64);
 response.ledger.shared_audio_reserved=segments('e'.repeat(64),48000,100);
 assert.equal(valid(response,request),true);
 const extra=segments('e'.repeat(64),48000,200).slice(0,39);
 response.ledger.shared_audio_reserved.push(...extra);assert.equal(valid(response,request),false);
});


test('a refused command is settled by one ledger read and never resent',async()=>{
 const prepare={kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id};
 const refused=status=>Object.assign(new Error(`refused ${status}`),{status});
 // Applied before the owner refused: the ledger read proves acceptance.
 let requests=[];let first=null;
 let client=protocol(bootstrap,attachment,async(url,request)=>{
  requests.push(clone(request));
  if(requests.length===1){first=clone(request);throw refused(409);}
  if(requests.length===2){const applied=answer(first,2);applied.receipt=null;return applied;}
  return answer(request,requests.length+1);
 });
 await client.transition(uuid(4),prepare);
 await client.transition(uuid(4),{kind:'scheduled',intervals:[interval]});
 assert.deepEqual(requests.map(r=>r.transition?.sequence??null),[1,null,2]);
 // Refused and absent: one failure, then the next command is not a replay.
 requests=[];
 client=protocol(bootstrap,attachment,async(url,request)=>{
  requests.push(clone(request));if(requests.length===1)throw refused(409);return answer(request,requests.length);
 });
 await assert.rejects(client.transition(uuid(4),prepare),/refused 409/);
 assert.equal(client.pending,null);
 await client.transition(uuid(5),prepare);
 assert.deepEqual(requests.map(r=>r.transition?.sequence??null),[1,null,1]);
 assert.equal(requests[2].transition.transaction_id,uuid(5));
 // An unreadable ledger keeps the command uncertain and replayable.
 requests=[];
 client=protocol(bootstrap,attachment,async(url,request)=>{
  requests.push(clone(request));
  if(requests.length===1)throw refused(409);if(requests.length===2)throw new Error('ledger unreadable');
  return answer(request,requests.length);
 });
 await assert.rejects(client.transition(uuid(4),prepare),/ledger unreadable/);
 assert.ok(client.pending);await client.recover();
 assert.deepEqual(requests[2],requests[0]);
});

test('rate refusal waits for admission before retrying the identical reservation',async()=>{
 const events=[],requests=[];
 const client=protocol(bootstrap,attachment,async(url,request)=>{
  events.push('send');requests.push(clone(request));
  if(requests.length===1)throw Object.assign(new Error('burst'),{status:429,retryAfterMs:1000});
  return answer(request,requests.length);
 },async ms=>events.push(ms));
 await client.transition(uuid(4),{kind:'prepare',intent_revision:1,target_rendition_id:interval.rendition_id},{timescale:24000,through_tick:0});
 assert.deepEqual(events,['send',1000,'send']);assert.deepEqual(requests[0],requests[1]);
 assert.equal(client.pending,null);assert.equal(client.ledger.accepted_sequence,1);
});


test('attached quality preparation follows a newer cold seek without reviving a fenced ask',async()=>{
 for(const firstReady of [false,true])for(const fenced of [false,true]){
  let release,began,releaseSecond,beganSecond,identity=20;const held=new Promise(resolve=>release=resolve);
  const secondHeld=new Promise(resolve=>releaseSecond=resolve),secondStarted=new Promise(resolve=>beganSecond=resolve);
  const started=new Promise(resolve=>began=resolve),calls=[],transactions=[];
  const row=(height,id)=>({candidate_id:id.repeat(32),rendition_id:id.repeat(64),
   init_id:'e'.repeat(64),codec:'avc1.640032',width:height===720?1280:1920,height,
   timescale:24,frame_ticks:1,segment_ticks:48,peak_bps:1000000,
   playlist:`video/${id.repeat(64)}/index.m3u8`});
  const videoRows=[row(720,'b'),row(1080,'c')];
  const family={version:1,mode:'controlled',family_id:'a'.repeat(64),master:'master.m3u8',video:videoRows,audio:null};
  const mock={get ledger(){return {transactions};},async transition(id,operation,frontier){
   calls.push({id,operation:clone(operation),frontier:clone(frontier||null)});
   if(operation.kind==='prepare'){
    const count=calls.filter(call=>call.operation.kind==='prepare').length;
    if(count===1){began();await held;}
    if(count===2){beganSecond();await secondHeld;}
    const ready=count>1||firstReady;
    transactions.push({transaction_id:id,state:ready?'ready':'retained_current',
     ready:ready?[{from_tick:frontier.through_tick,through_tick:frontier.through_tick+48}]:[],
     reserved:[],cancel_requested:false,intent_superseded:false});
   }else if(operation.kind==='cancel_unappended'){
    transactions.find(tx=>tx.transaction_id===id).cancel_requested=true;
   }
   return {ledger:{transactions}};
  }};
  const scope=vm.createContext({AbortController,TextDecoder,setTimeout,clearTimeout,URL,
   location:{href:'http://localhost/'},CONTROL_CLIENT_ID:uuid(2),newRequestId:()=>uuid(9),
   crypto:{getRandomValues:bytes=>{bytes.fill(0);bytes[15]=identity++;return bytes;}},performance:{now:()=>0},mock,
   Hls:{Events:{MANIFEST_PARSED:'manifest',BUFFER_CREATED:'buffers',MEDIA_DETACHED:'detach'}}});
  vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-media.js','utf8'),scope);
  vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-quality.js','utf8'),scope);
  vm.runInContext('continuousQualityProtocol=()=>mock;',scope);
  const player={},attachment={current:()=>true},media={currentTime:0,seeking:false};
  const adapter=scope.continuousQualityAdapter(player,media,attachment,
   {...bootstrap,family,primary_candidate_id:videoRows[0].candidate_id});
  player.continuousQuality=adapter;
  const network=[];
  const hls={levels:videoRows.map(row=>({url:['http://localhost/api/v1/hls/session/'+row.playlist]})),
   on(){},loadLevel:0,stopLoad(){network.push('stop');},startLoad(at,skipSeek){network.push({at,skipSeek});}};
  adapter.bind(hls,0);
  const choice=adapter.choose(videoRows[1].candidate_id,()=>!fenced);
  await started;assert.equal(adapter.noteSeek(900),true);media.currentTime=900;media.seeking=true;release();
  await secondStarted;assert.equal(adapter.noteSeek(900),true);releaseSecond();
  assert.equal(await choice,fenced?(firstReady?'superseded':'retained_current'):'continuous');
  const prepares=calls.filter(call=>call.operation.kind==='prepare');
  assert.deepEqual(prepares.map(call=>call.frontier.through_tick),[0,21600]);
  assert.equal(adapter.wanted.rendition_id,videoRows[fenced?0:1].rendition_id);
  assert.equal(hls.loadLevel,fenced?0:1);
  assert.deepEqual(network,fenced?[]:['stop',{at:900,skipSeek:true}],
   "cold seek resets only network loading and skips a second element seek");
  assert.deepEqual(prepares.map(call=>call.operation.target_rendition_id),
   [videoRows[1].rendition_id,videoRows[fenced?0:1].rendition_id]);
  assert.equal(calls.filter(call=>call.operation.kind==='cancel_unappended').length,firstReady?1:0);
  assert.equal(adapter.noteSeek(30),true);assert.equal(adapter.frontier,720,'backward seek ignores the disjoint old frontier');
  assert.equal(adapter.noteSeek(NaN),false);assert.equal(adapter.frontier,720);
  player.continuousQuality={};assert.equal(adapter.noteSeek(100),false,'replaced attachment cannot mutate this owner');
 }
});

test('typed pending family retries the same session under the original preparation signal',async()=>{
 const owner=new AbortController(),requests=[],waits=[];let calls=0;
 const family={version:2,family_revision:1};
 const result=await context.continuousQualityBootstrapFamily('/api/v1/hls/retained/quality-family',owner.signal,
  async(url,body,options)=>{requests.push({url,body,signal:options.signal});if(++calls<3)
    throw Object.assign(new Error('still producing'),{status:503,streamFailure:{code:'segment_pending'},retryAfterMs:10});return family;},
  async(ms,signal)=>waits.push({ms,signal}));
 assert.equal(result,family);assert.equal(requests.length,3);
 assert.ok(requests.every(row=>row.url===requests[0].url&&row.body===null&&row.signal===owner.signal));
 assert.ok(waits.every(row=>row.signal===owner.signal&&row.ms===10));
});
test('pending family abort and terminal refusals do not create another retry budget',async()=>{
 const owner=new AbortController();let calls=0;
 await assert.rejects(context.continuousQualityBootstrapFamily('/same-family',owner.signal,async()=>{
  calls++;throw Object.assign(new Error('pending'),{status:503,streamFailure:{code:'segment_pending'}});
 },async()=>owner.abort()),{name:'AbortError'});
 assert.equal(calls,1);
 for(const refusal of [{status:500,streamFailure:{code:'segment_pending'}},{status:503,streamFailure:{code:'media_owner_transition'}},{status:503}]){
  calls=0;await assert.rejects(context.continuousQualityBootstrapFamily('/same-family',new AbortController().signal,async()=>{
   calls++;throw Object.assign(new Error('terminal'),refusal);
  },async()=>assert.fail('terminal refusals never wait')),/terminal/);assert.equal(calls,1);
 }
});
test('continuous fetch preserves bounded typed refusal and retry-after',async()=>{
 const scope=vm.createContext({AbortController,TextDecoder,setTimeout,clearTimeout,
  fetch:async()=>new Response(JSON.stringify({code:'segment_pending',message:'Verified initialization is pending.'}),
   {status:503,headers:{'retry-after':'1'}}),PlaybackPolicy:{parseStreamFailure:({status,body})=>({status,...JSON.parse(body)})}});
 vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-quality.js','utf8'),scope);
 await assert.rejects(scope.continuousQualityFetch('/family'),error=>error.status===503&&error.retryAfterMs===1000
  &&error.streamFailure.code==='segment_pending'&&error.message==='Verified initialization is pending.');
 scope.fetch=async()=>new Response('x'.repeat(16385),{status:503});
 await assert.rejects(scope.continuousQualityFetch('/family'),/response bound/);
});
function startupFamily(){
 const row=(id,height)=>({candidate_id:id.repeat(32),rendition_id:id.repeat(64),init_id:'e'.repeat(64),codec:'avc1.640032',
  width:height===720?1280:1920,height,timescale:24000,frame_ticks:1001,segment_ticks:48048,peak_bps:1000000,
  playlist:`video/${id.repeat(64)}/index.m3u8`});
 return {version:2,family_revision:1,mode:'controlled',family_id:'f'.repeat(64),master:'master.m3u8',video:[row('a',720)],audio:null,
  companion:row('b',1080)};
}
test('versioned family accepts primary-only startup and only additive verified updates',()=>{
 const {companion,...first}=startupFamily(),next={...clone(first),family_revision:2,video:[...clone(first.video),companion]};
 assert.equal(context.continuousQualityFamily(first),true);
 assert.equal(context.continuousQualityFamily({...first,version:1}),false);
 assert.equal(context.continuousQualityFamilyUpdate(first,next),true);
 assert.equal(context.continuousQualityFamilyUpdate(next,first),false);
 assert.equal(context.continuousQualityFamilyUpdate(first,{...next,family_id:'0'.repeat(64)}),false);
 const replaced=clone(next);replaced.video[0].init_id='0'.repeat(64);
 assert.equal(context.continuousQualityFamilyUpdate(first,replaced),false);
 assert.equal(context.continuousQualityFamilyUpdate(first,{...next,family_revision:1}),false);
});
test('primary-only continuous bootstrap retains work until its owner aborts or a terminal refusal',async()=>{
 for(const outcome of ['ready','abort','terminal']){
  const {companion,...family}=startupFamily(),session=uuid(71),owner=new AbortController(),released=[],gets=[],waits=[];
  const candidate={id:family.video[0].candidate_id,route:'encode',target_height:720};
  const pair={primary_candidate_id:candidate.id,companion_candidate_id:companion.candidate_id};
  const selection={quality:{mode:'manual',height:720}},player={};let count=0;
  const scope=vm.createContext({AbortController,TextDecoder,setTimeout,clearTimeout,crypto:require('node:crypto').webcrypto,
   Hls:{isSupported:()=>true},window:{Hls:{}},playbackControlSelection:()=>clone(selection),qualityMediaIntent:()=>({}),
   releaseSession:id=>released.push(id),playbackRetryDelay:async(ms,signal)=>{waits.push(signal);if(outcome==='abort')owner.abort();},
   api:async(url,opts)=>url.endsWith('continuous-candidates')?{version:1,candidates:[candidate],pairs:[pair]}:
    {version:1,playback:{session_id:session},quality:{generation:uuid(72),control_epoch:1,
     schedule_url:`/api/v1/hls/${session}/quality-schedule`,family_url:`/api/v1/hls/${session}/quality-family`}}});
  vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-quality.js','utf8'),scope);
  scope.continuousQualityFetch=async(url,body,options)=>{
   gets.push({url,signal:options.signal});if(++count===1)throw Object.assign(new Error('pending'),
    {status:503,streamFailure:{code:outcome==='terminal'?'response_state_changed':'segment_pending'},retryAfterMs:1});
   return clone(family);
  };
  const opening=scope.openContinuousQualitySession(1,{transport:'hlsjs',height:720,caps:{},request_id:'one'},player,owner.signal);
  if(outcome==='ready'){
   const info=await opening;assert.equal(info.session_id,session);assert.equal(info.continuous_quality.family.video.length,1);
   assert.equal(info.continuous_quality.companion_candidate_id,companion.candidate_id);assert.deepEqual(released,[]);
   assert.equal(gets.length,2);assert.ok(gets.every(row=>row.signal===owner.signal));
  }else{await assert.rejects(opening);assert.deepEqual(released,[session]);assert.equal(gets.length,1);}
 }
});

test('bundled hls additive family levels retain incumbent indexes audio groups and media ownership',()=>{
 class Surface{addEventListener(){}removeEventListener(){}static isTypeSupported(){return true;}}
 const world={navigator:{userAgent:'fixture'},MediaSource:Surface,location:{href:'http://localhost/'},URL};
 const scope=vm.createContext({self:world,window:world,navigator:world.navigator,exports:{},module:{exports:{}},
  console,performance,setTimeout,clearTimeout,setInterval,clearInterval,URL,location:world.location});
 vm.runInContext(fs.readFileSync('crates/plurxd/src/web/hls.min.js','utf8'),scope);
 scope.Hls=scope.module.exports;
 vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-quality.js','utf8'),scope);
 const hls=new scope.Hls({autoStartLoad:false,enableWorker:false,preferManagedMediaSource:false});
 assert.equal(scope.Hls.version,'1.6.19');
 const {companion,...first}=startupFamily(),prefix='/api/v1/hls/retained/';
 const parser=hls.networkControllers.find(controller=>typeof controller.handleMasterPlaylist==='function');assert.ok(parser);
 const master=`#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="shared",NAME="Audio",DEFAULT=YES,AUTOSELECT=YES,URI="audio/shared/index.m3u8"\n#EXT-X-STREAM-INF:BANDWIDTH=1192000,RESOLUTION=1280x720,CODECS="avc1.640032,mp4a.40.2",AUDIO="shared"\n${first.video[0].playlist}\n`;
 parser.handleMasterPlaylist({data:master,url:'http://localhost'+prefix+'master.m3u8'},
  {parsing:{start:0,end:0},loading:{start:0,end:0}},{url:'http://localhost'+prefix+'master.m3u8',type:'manifest'},null);
 assert.equal(hls.levels.length,1);const incumbent=hls.levels[0];hls.startLevel=0;
 let detached=0,manifest=0;hls.on(scope.Hls.Events.MEDIA_DETACHING,()=>detached++);hls.on(scope.Hls.Events.MANIFEST_LOADING,()=>manifest++);
 const next={...first,family_revision:2,video:[...first.video,companion]};
 const updated=scope.continuousQualityFamilyLevels(hls,next,first.video[0],prefix);
 hls.trigger(scope.Hls.Events.LEVELS_UPDATED,{levels:updated});
 assert.equal(hls.levels.length,2);assert.equal(hls.levels[0],incumbent);assert.equal(hls.startLevel,0);
 assert.equal(hls.levels[1].url[0],'http://localhost'+prefix+companion.playlist);
 assert.deepEqual(clone(hls.levels[1].audioGroups),clone(incumbent.audioGroups));
 assert.equal(hls.levels[1].videoCodec,companion.codec);assert.equal(detached,0);assert.equal(manifest,0);
 hls.destroy();
});
