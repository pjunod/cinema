const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
const source=fs.readFileSync('crates/plurxd/src/web/player/frame-drop-recorder.js','utf8');
function harness(options={}){
  let time=0,serial=0;
  const listeners=new Map(),prefs=new Map();
  const events=()=>({addEventListener:(n,f)=>{if(!listeners.has(n))listeners.set(n,new Set());listeners.get(n).add(f);},
    removeEventListener:(n,f)=>listeners.get(n)?.delete(f)});
  let element;
  const requests=[];
  const ctx=vm.createContext({console,URL,Date,Number,Math,JSON,Set,Promise,AbortController,setTimeout,clearTimeout,
    location:{href:"https://fixture.invalid/"},
    window:events(),document:{...events(),getElementById:()=>element,hasFocus:()=>true,hidden:false},
    localStorage:{getItem:k=>prefs.get(k)??null},performance:{now:()=>time},
    crypto:{randomUUID:()=>`00000000-0000-4000-8000-${String(++serial).padStart(12,'0')}`},
    TOKEN:'canary-token',ME:{id:1},AUTH_GENERATION:1,API:'/api/v1',
    browserLabel:()=> 'Safari',bufferRunway:()=>5,
    fetch:async(url,options)=>{requests.push({url,options,body:JSON.parse(options.body)});return {ok:true};},...options});
  ctx.playbackOwnsAttachedMedia=p=>ctx.PLAYER===p;
  vm.runInContext(source,ctx);
  const emitted=[],create=vm.runInContext('createFrameDropRecorder',ctx);
  const recorder=create({now:()=>time,wall:()=>1700000000000+time,id:()=>`fd-${++serial}`,
    context:()=>({attachment_id:1}),emit:e=>emitted.push(JSON.parse(JSON.stringify(e)))});
  const sample=(total=0,dropped=0,extra={})=>recorder.sample({time:time/1000,total,dropped,corrupted:0,ready:4,flags:8,runway:5,receipts:total,...extra});
  function video(){return {...events(),currentTime:30,readyState:4,paused:false,seeking:false,
    currentSrc:'https://fixture.invalid/api/v1/hls/session/index.m3u8?token=CANARY_URL_TOKEN',
    getVideoPlaybackQuality:()=>({totalVideoFrames:0,droppedVideoFrames:0,corruptedVideoFrames:0})};}
  function install(v=video(),p={fileId:'9007199254740993',sessionId:'first-session',attemptId:'first-attempt',method:'remux',mediaAttachment:{id:1}}){element=v;ctx.PLAYER=p;ctx.startFrameDropRecorder(v,p);return {v,p};}
  return {ctx,recorder,emitted,sample,requests,listeners,prefs,install,video,setElement:v=>element=v,at:t=>time=t,emit:n=>{for(const f of listeners.get(n)||[])f({type:n});}};
}
const drain=async()=>{for(let n=0;n<16;n++)await Promise.resolve();};

test('frame recorder freezes prehistory and post-tail with honest batched-counter timing',()=>{
  const h=harness();
  for(let t=0;t<=60000;t+=500){h.at(t);h.sample(Math.floor(t/2000)*48,t===60000?1:0);}
  assert.equal(h.emitted.length,0);
  const pending=h.recorder.pending();assert.equal(pending.observed_from_ms,58000);assert.equal(pending.trigger_ms,60000);
  for(let t=60500;t<=70000;t+=500){h.at(t);h.sample(Math.floor(t/2000)*48,1);}
  const report=h.emitted[0];assert.equal(report.samples[0][0],0);assert.equal(report.samples.at(-1)[0],70000);
  assert.equal(report.end_reason,'complete');assert.equal(report.drop_from,0);assert.equal(report.drop_to,1);
  assert.equal(report.clock_origin_ms,1700000000000);
});

test('initial totals establish baseline and source counter reset cannot create an incident',()=>{
  const h=harness();h.sample(100,9);h.at(500);h.sample(110,9);assert.equal(h.recorder.pending(),null);
  h.at(1000);h.sample(0,0);assert.equal(h.recorder.pending(),null);
  h.at(1500);h.sample(10,1);h.recorder.stop('replaced');
  assert.equal(h.emitted[0].drop_from,0);assert.equal(h.emitted[0].drop_to,1);
  assert.equal(h.emitted[0].end_reason,'replaced');assert(h.emitted[0].events.some(e=>e[1]===18));
});

test('counter growth coalesces while cooldown and per-play budget prevent report storms',()=>{
  const h=harness();h.sample();
  for(let t=500;t<=180000;t+=500){h.at(t);h.sample(t/10,t/500);}
  h.recorder.stop();assert.equal(h.emitted.length,3);
  assert(h.emitted[0].drop_to>h.emitted[0].drop_from+1);
  assert.equal(h.emitted[1].trigger_ms-h.emitted[0].trigger_ms,60000);
});

test('history, frame rows and wire size stay bounded without leaking input strings',()=>{
  const h=harness();h.sample();
  for(let i=0;i<10000;i++){h.at(i);h.recorder.frame(i,{mediaTime:i/24,presentedFrames:i,
    expectedDisplayTime:i+16.666,presentationTime:i,processingDuration:.013,title:'CANARY_SECRET'});
    h.recorder.event(1);}
  h.sample(240,1,{src:'CANARY_URL',title:'CANARY_TITLE'});
  for(let i=0;i<300;i++)h.recorder.frame(10000+i,{mediaTime:4500+i/24,presentedFrames:240+i});
  h.recorder.stop();const r=h.emitted[0],wire=JSON.stringify(r);
  assert(r.frames.length<=512);assert(r.post_frames.length<=128);assert(r.events.length<=64);
  assert(Buffer.byteLength(wire)<=49152);assert(!wire.includes('CANARY'));assert(r.trimmed[1]>0);assert(r.trimmed[3]>0);
});

test('unavailable measurements remain null and disabling discards a pending capture',()=>{
  const h=harness();h.sample(null,null);h.at(500);h.sample(1,0);h.at(1000);h.sample(2,1,{runway:NaN});
  h.recorder.frame(1000,{mediaTime:Infinity,presentedFrames:undefined});
  const pending=h.recorder.pending();assert.equal(pending.samples.at(-1)[7],null);
  assert.equal(pending.post_frames[0][2],null);h.recorder.stop('stopped',true);assert.equal(h.emitted.length,0);
});

test('adapter saves frozen ownership on replacement without adding frame subscriptions',async()=>{
  const h=harness();const {v,p}=h.install();let dropped=0;
  v.requestVideoFrameCallback=()=>{throw Error('recorder must reuse existing callbacks');};
  v.getVideoPlaybackQuality=()=>({totalVideoFrames:30,droppedVideoFrames:dropped,corruptedVideoFrames:0});
  h.ctx.sampleFrameDropRecorder(v,p);h.at(500);dropped=1;h.ctx.sampleFrameDropRecorder(v,p);
  h.ctx.recordFrameDropFrame(v,p,500,{mediaTime:30,presentedFrames:30});
  h.ctx.stopFrameDropRecorder(p,'replaced');
  h.ctx.PLAYER={fileId:2,sessionId:'successor-secret',mediaAttachment:{id:2}};
  await drain();assert.equal(h.requests.length,1);
  const body=h.requests[0].body;assert.equal(body.file_id,'9007199254740993');assert.equal(body.session_id,'first-session');
  assert.equal(body.frame_drop.post_frames.length,1);assert(!JSON.stringify(body).includes('CANARY'));
  assert(!JSON.stringify(body).includes('successor'));assert.equal([...h.listeners.get('waiting')].length,0);
});

test('stale arms and callbacks cannot retire or contaminate the current attachment',async()=>{
  const h=harness(),first=h.install(),second=h.install(h.video(),{fileId:2,sessionId:'second',mediaAttachment:{id:2}});
  h.ctx.startFrameDropRecorder(first.v,first.p);h.ctx.recordFrameDropFrame(first.v,first.p,0,{mediaTime:999});
  let dropped=0;second.v.getVideoPlaybackQuality=()=>({totalVideoFrames:10,droppedVideoFrames:dropped});
  h.ctx.sampleFrameDropRecorder(second.v,second.p);h.at(500);dropped=1;h.ctx.sampleFrameDropRecorder(second.v,second.p);
  h.ctx.stopFrameDropRecorder(second.p);await drain();assert.equal(h.requests.length,1);
  assert.equal(h.requests[0].body.session_id,'second');assert.equal(h.requests[0].body.frame_drop.frames.length,0);
});

test('history opt-out and account transition discard pending data and listeners',async()=>{
  const h=harness(),{v,p}=h.install();let dropped=0;
  v.getVideoPlaybackQuality=()=>({totalVideoFrames:10,droppedVideoFrames:dropped});
  h.ctx.sampleFrameDropRecorder(v,p);h.at(500);dropped=1;h.ctx.sampleFrameDropRecorder(v,p);
  h.prefs.set('cinema_diagnostic_history','off');h.emit('cinema-diagnostic-history');await drain();
  assert.equal(h.requests.length,0);assert.equal([...h.listeners.get('waiting')].length,0);
  h.ctx.ME={id:2};h.ctx.AUTH_GENERATION++;h.emit('cinema-diagnostic-auth');await drain();
  const rows=await vm.runInContext('FrameDropIncidentStore.operate("2")',h.ctx);assert.equal(rows.length,0);
});

test('resource evidence retains current transport only and strips every URL',()=>{
  const h=harness(),{v}=h.install();
  vm.runInContext('const qualityResourceTimingRows=new Map()',h.ctx);
  h.ctx.entries=[
    {name:'https://fixture.invalid/api/v1/hls/session/seg00042.m4s?token=CANARY',startTime:10,responseStart:20,responseEnd:30,encodedBodySize:100,transferSize:130},
    {name:'https://fixture.invalid/api/v1/hls/other/seg00099.m4s',startTime:11,responseStart:21,responseEnd:31,encodedBodySize:200},
    {name:'https://other.invalid/api/v1/hls/session/seg00043.m4s',startTime:12,responseEnd:32},
    {name:'https://fixture.invalid/api/v1/hls/session/seg00040.m4s',startTime:0,responseEnd:10},
  ];
  vm.runInContext('entries.forEach((row,i)=>qualityResourceTimingRows.set(i,row))',h.ctx);
  const rows=h.ctx.frameDropResourceRows(v,5);assert.equal(rows.length,1);assert.equal(rows[0][1],42);
  assert(!JSON.stringify(rows).includes('CANARY'));
});

// Clearing while the asynchronous local write is pending must invalidate its upload.
test('clear diagnostic history invalidates queued incident writes and uploads',async()=>{
  const h=harness(),{v,p}=h.install();let dropped=0;
  v.getVideoPlaybackQuality=()=>({totalVideoFrames:100+dropped,droppedVideoFrames:dropped});
  h.ctx.sampleFrameDropRecorder(v,p);h.at(500);dropped=1;h.ctx.sampleFrameDropRecorder(v,p);
  h.ctx.stopFrameDropRecorder(p);h.emit('cinema-diagnostic-clear');await drain();
  assert.equal(h.requests.length,0);
  const rows=await vm.runInContext('FrameDropIncidentStore.operate("1")',h.ctx);assert.equal(rows.length,0);
});


test('full pre-trigger event history preserves a separately bounded post-trigger tail',()=>{
  const h=harness();h.sample();
  for(let i=0;i<64;i++)h.recorder.event(15);
  h.at(500);h.sample(10,1);
  for(let i=0;i<80;i++)h.recorder.event(17,100);
  h.recorder.stop();const incident=h.emitted[0];
  assert.equal(incident.events.length,128);
  assert.equal(incident.events.filter(row=>row[1]===17).length,64);
  assert(incident.trimmed[2]>=16);
});

function indexedFixture(){
  let records=new Map();const fixture={failure:null};
  const db={transaction(){
    if(fixture.failure==='throw')throw new Error('database unavailable');
    let staged=new Map(records);const transaction={abort(){transaction.onabort?.();}};
    const store={clear(){staged.clear();},put(row){staged.set(row.id,row);},getAll(){
      const request={};Promise.resolve().then(()=>{
        request.result=[...records.values()].sort((a,b)=>a.id.localeCompare(b.id));request.onsuccess?.();
        if(fixture.failure==='abort')transaction.onabort?.();else {records=staged;transaction.oncomplete?.();}
      });return request;
    }};
    transaction.objectStore=()=>store;return transaction;
  }};
  fixture.indexedDB={open(){const request={result:db};Promise.resolve().then(()=>request.onsuccess?.());return request;}};
  return fixture;
}

test('local retention follows capture age across UUID order and submission rewrites',async()=>{
  for(const persistent of [false,true]){
    const fixture=indexedFixture(),h=harness(persistent?{indexedDB:fixture.indexedDB}:{});
    const store=vm.runInContext('FrameDropIncidentStore',h.ctx),at=Date.now();
    const rows=['fd-z','fd-y','fd-x','fd-w'].map((id,i)=>({id,owner:'1',expires:at+10000+i,submitted:false}));
    for(const row of rows)await store.operate('1',row);
    let retained=await store.operate('1');assert.deepEqual(Array.from(retained,row=>row.id),['fd-y','fd-x','fd-w']);
    retained=await store.operate('1',{...rows[1],submitted:true});
    assert.deepEqual(Array.from(retained,row=>row.id),['fd-y','fd-x','fd-w']);
  }
});

test('transaction abort and later recovery preserve the bounded memory fallback',async()=>{
  const fixture=indexedFixture(),h=harness({indexedDB:fixture.indexedDB});
  const store=vm.runInContext('FrameDropIncidentStore',h.ctx),at=Date.now();
  await store.operate('1',{id:'fd-a',owner:'1',expires:at+10000});
  fixture.failure='abort';let rows=await store.operate('1',{id:'fd-b',owner:'1',expires:at+10001});
  assert.deepEqual(Array.from(rows,row=>row.id),['fd-a','fd-b']);
  fixture.failure='throw';rows=await store.operate('1',{id:'fd-c',owner:'1',expires:at+10002});
  assert.equal(rows.length,3);
  fixture.failure=null;rows=await store.operate('1');
  assert.deepEqual(Array.from(rows,row=>row.id),['fd-a','fd-b','fd-c']);
});

test('blob-backed HLS records the frozen owned playlist resources',async()=>{
  const h=harness(),v=h.video(),attachment={id:1};v.currentSrc='blob:https://fixture.invalid/opaque';
  const p={fileId:'1',mediaAttachment:attachment,hlsStartup:{mediaAttachment:attachment,
    playlistUrl:'https://fixture.invalid/api/v1/hls/owned/index.m3u8?token=CANARY'}};
  h.install(v,p);vm.runInContext('const qualityResourceTimingRows=new Map()',h.ctx);
  h.ctx.entry={name:'https://fixture.invalid/api/v1/hls/owned/seg00042.m4s?token=CANARY',startTime:10,responseStart:20,responseEnd:30};
  vm.runInContext('qualityResourceTimingRows.set("one",entry)',h.ctx);
  let dropped=0;v.getVideoPlaybackQuality=()=>({totalVideoFrames:10+dropped,droppedVideoFrames:dropped});
  h.ctx.sampleFrameDropRecorder(v,p);h.at(500);dropped=1;h.ctx.sampleFrameDropRecorder(v,p);
  p.hlsStartup.playlistUrl='https://fixture.invalid/api/v1/hls/successor/index.m3u8';
  h.ctx.stopFrameDropRecorder(p);await drain();
  assert.equal(h.requests[0].body.frame_drop.resources[0][1],42);
  assert(!JSON.stringify(h.requests[0].body).includes('CANARY'));
});


test('prepared element handoff restarts observation with frozen successor identity',async()=>{
  const h=harness(),{v,p}=h.install();let dropped=0;
  v.getVideoPlaybackQuality=()=>({totalVideoFrames:100+dropped,droppedVideoFrames:dropped});
  h.ctx.sampleFrameDropRecorder(v,p);h.at(500);dropped=1;h.ctx.sampleFrameDropRecorder(v,p);
  const next=h.video();h.setElement(next);p.sessionId='successor-session';
  p.probeUrl='https://fixture.invalid/api/v1/hls/successor/index.m3u8';
  let nextDropped=0;next.getVideoPlaybackQuality=()=>({totalVideoFrames:200+nextDropped,droppedVideoFrames:nextDropped});
  h.at(1000);h.ctx.sampleFrameDropRecorder(next,p);await drain();
  assert.equal(h.requests[0].body.session_id,'first-session');
  assert.equal(h.requests[0].body.frame_drop.end_reason,'replaced');
  h.at(61000);nextDropped=1;h.ctx.sampleFrameDropRecorder(next,p);h.ctx.stopFrameDropRecorder(p);await drain();
  assert.equal(h.requests.length,2);assert.equal(h.requests[1].body.session_id,'successor-session');
  assert.equal(h.requests[1].body.frame_drop.drop_from,0);
  assert.equal([...h.listeners.get('waiting')].length,0);
});
