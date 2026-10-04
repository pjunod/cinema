"use strict";
const assert=require("node:assert/strict"),fs=require("node:fs"),vm=require("node:vm"),{test}=require("node:test"),acorn=require("../vendor/acorn.js");
const WEB="crates/plurxd/src/web/";
function fn(file,name){const s=fs.readFileSync(WEB+file,"utf8"),n=acorn.parse(s,{ecmaVersion:"latest"}).body.find(n=>n.type==="FunctionDeclaration"&&n.id.name===name);assert.ok(n);return s.slice(n.start,n.end);}
const ref={import_id:"11111111-1111-4111-8111-111111111111",server_id:"22222222-2222-4222-8222-222222222222",catalogue_epoch:"33333333-3333-4333-8333-333333333333",library_id:"0",item_id:"9223372036854775807"};
const base=`/api/v1/shared/imports/${ref.import_id}/files/${"L".repeat(236)}`,revision="a".repeat(64);
function detail(id="9007199254740993",life="9223372036854775807"){return `{"item":{"source":"shared","reference":${JSON.stringify(ref)}},"lifecycle_generation":${life},"files":[{"file_id":"${id}","revision":"${revision}","file_base":"${base}","reference":{"item":${JSON.stringify(ref)},"file_id":"${id}","revision":"${revision}","lifecycle_generation":${life}}}]}`;}
function wire(id="9007199254740993",life="9223372036854775807"){return `{"file_id":"${id}","reference":{"item":${JSON.stringify(ref)},"file_id":"${id}","revision":"${revision}","lifecycle_generation":${life}},"method":"remux","play_url":"${base}/stream.mp4?audio=4095","delivery":{"mode":"remux","url":"${base}/stream.mp4?audio=4095","sessions_url":"${base}/hls/sessions"},"audio_tracks":[{"index":3,"codec":"aac","channels":6}],"subtitle_tracks":[{"index":1,"protocol":"vtt","format":"subrip"}],"quality_ladder":[{"height":1080,"bitrate":8000000}],"timeline_origin_ms":1234,"delivered_dynamic_range":"sdr","future_exact":9223372036854775807}`;}
function response(text,url,status=200,options={}){const r=new Response(text,{status,headers:options.headers||{}});Object.defineProperty(r,"url",{value:options.url||url});Object.defineProperty(r,"redirected",{value:!!options.redirected});return r;}
function harness(handler=null){const requests=[],ctx=vm.createContext({console,URL,URLSearchParams,Uint8Array,TextEncoder,TextDecoder,AbortController,setTimeout,clearTimeout,fetch:async(url,options)=>{requests.push({url,options});return handler?handler(url,options,requests.length):response(options.method==="GET"?detail():wire(),url);},location:{href:"https://b.test/#/shared",hash:"#/shared"},navigator:{userAgent:"actual-browser-fixture"}});
 const functions=[['pages/sharing-management.js',['sharingJSON','sharingInteger']],['pages/shared-libraries.js',['sharedCatalogueId','sharedCatalogueUuid','sharedCatalogueLibraryReference','sharedCatalogueReference']],['player/decode-tiers.js',['capsDocument','currentCapsDocument']]].flatMap(([file,names])=>names.map(n=>fn(file,n))).join('\n');
 vm.runInContext(`let TOKEN="first",API="/api/v1",AUTH_GENERATION=2,PAGE_RENDER_GENERATION=7;
 const SERVER={build:"test",playback_display_aware_auto:false},PLAY_CAPS={vcodec:"h264,hevc,hevc10",acodec:"aac",container:"mp4",maxheight:1080,progressiveHevcSampleEntries:["hvc1"]};
 function decodeLimits(){return [];}function measuredPresentationTarget(){return null;}
 `+fs.readFileSync(WEB+'core/file-context.js','utf8')+'\n'+functions+'\n'+fs.readFileSync(WEB+'core/shared-decision.js','utf8')+`\nthis.h={details:SHARED_DECISION.details,decision:SHARED_DECISION.decision,start:SHARED_DECISION.start,progress:SHARED_DECISION.progress,retire:sharedDecisionRetire,caps:currentCapsDocument,factory:sharedPlaybackFileContextFromDetail,key:playbackFileKey,change(){TOKEN="next";AUTH_GENERATION++;sharedDecisionRetire();},leave(){PAGE_RENDER_GENERATION++;sharedDecisionRetire();},origin(){API="https://other.test/api/v1";},overflow(){PLAY_CAPS.acodec="a".repeat(128*1024);},};`,ctx);return {...ctx.h,requests};}
test("actual v2 builder preserves complete engine fields and exact Source identities",async()=>{
 for(const id of ['0','9007199254740993','9223372036854775807']){
  const h=harness((url,o)=>response(o.method==='GET'?detail(id):wire(id),url)),r=await h.details(ref),c=r.files[0].context,d=await h.decision(c,{force:'original',audio:4095,subtitle:-1,audio_offset_ms:-15000});
  assert.equal(c.source_file_id,id);assert.equal(c.lifecycle_generation,9223372036854775807n);assert.equal(d.file_id,id);assert.equal(d.reference.lifecycle_generation,9223372036854775807n);assert.equal(d.future_exact,9223372036854775807n);assert.equal(d.audio_tracks[0].channels,6);assert.equal(d.subtitle_tracks[0].protocol,'vtt');assert.equal(d.quality_ladder[0].height,1080);assert.equal(d.timeline_origin_ms,1234);
  const request=h.requests[1];assert.equal(request.url,'https://b.test'+base+'/decision?force=original&audio=4095&subtitle=-1&audio_offset_ms=-15000');assert.equal(request.options.redirect,'error');assert.equal(request.options.credentials,'omit');assert.equal(request.options.headers.authorization,'Bearer first');assert.deepEqual(JSON.parse(request.options.body).caps,JSON.parse(JSON.stringify(h.caps())));assert.equal(d._capsSnapshot.v,2);assert.ok(!Object.keys(d).includes('_capsSnapshot'));assert.equal(c.session_id,null);
 }
});
test("details require exact full reference lifecycle and236 alias without numeric or Local fallback",async()=>{
 const changes=[s=>s.replace('"lifecycle_generation":9223372036854775807','"lifecycle_generation":"9223372036854775807"'),s=>s.replaceAll('9223372036854775807,"files"','0,"files"'),s=>s.replace("L".repeat(236),"L".repeat(235)),s=>s.replace(base,'/api/v1/files/7'),s=>s.replace('"file_id":"9007199254740993"','"file_id":9007199254740993'),s=>s.replace('"revision":"'+revision+'"','"revision":"'+"b".repeat(64)+'"'),s=>s.replace('"item_id":"9223372036854775807"','"item_id":"1"')];
 for(const change of changes){const h=harness((u)=>response(change(detail()),u));await assert.rejects(h.details(ref));assert.equal(h.requests.length,1);}
 const h=harness(),r=await h.details(ref),untrusted=h.factory(ref,{file_base:base,file_id:'7',revision,reference:{item:ref,file_id:'7',revision,lifecycle_generation:1n}});await assert.rejects(h.decision(untrusted));assert.equal(h.requests.length,1);assert.notEqual(h.key(untrusted),h.key(r.files[0].context));
});
test("closed query grammar refuses before request and context cannot cross account origin or page",async()=>{
 const h=harness(),c=(await h.details(ref)).files[0].context;
 for(const q of [{audio:4096},{subtitle:-2},{force:'remux'},{start:0},{token:'x'},{audio:'3'},{audio_offset_ms:15001}])await assert.rejects(h.decision(c,q));assert.equal(h.requests.length,1);
 h.origin();await assert.rejects(h.decision(c));assert.equal(h.requests.length,1);
 const fresh=harness(),fc=(await fresh.details(ref)).files[0].context;fresh.change();await assert.rejects(fresh.decision(fc));assert.equal(fresh.requests.length,1);
 const page=harness(),pc=(await page.details(ref)).files[0].context;page.leave();await assert.rejects(page.decision(pc));assert.equal(page.requests.length,1);
});
test("actual4MiB streaming bound allows complete response and refuses plus1 without retry",async()=>{
 for(const extra of [0,1]){const raw=wire(),target=4*1024*1024+extra,text=raw.slice(0,-1)+',"padding":"'+'x'.repeat(target-raw.length-13)+'"}';assert.equal(Buffer.byteLength(text),target);const h=harness((u,o)=>response(o.method==='GET'?detail():text,u)),c=(await h.details(ref)).files[0].context;if(extra)await assert.rejects(h.decision(c));else assert.equal((await h.decision(c)).file_id,'9007199254740993');assert.equal(h.requests.length,2);}
 const h=harness((u,o)=>response(o.method==='GET'?detail():wire(),u,200,o.method==='GET'?{}:{headers:{'content-length':String(4*1024*1024+1)}})),c=(await h.details(ref)).files[0].context;await assert.rejects(h.decision(c));
});
test("redirects refusal statuses and foreign complete decision fields cannot retry or acquire session",async()=>{
 for(const status of [302,401,403,409,500]){const h=harness((u,o)=>response(o.method==='GET'?detail():wire(),u,o.method==='GET'?200:status)),c=(await h.details(ref)).files[0].context;await assert.rejects(h.decision(c));assert.equal(h.requests.length,2);if(status===401||status===403){await assert.rejects(h.decision(c));assert.equal(h.requests.length,2);}}
 for(const change of [s=>s.replace(base,'https://source.test/files/7'),s=>s.replace('"file_id":"9007199254740993"','"file_id":"7"'),s=>s.replace('"lifecycle_generation":9223372036854775807','"lifecycle_generation":1'),s=>s.replace('"sessions_url":"'+base+'/hls/sessions"','"sessions_url":"'+base+'/direct"')]){const h=harness((u,o)=>response(o.method==='GET'?detail():change(wire()),u)),c=(await h.details(ref)).files[0].context;await assert.rejects(h.decision(c));assert.equal(c.session_id,null);}
 const h=harness((u,o)=>response(o.method==='GET'?detail():wire(),u,200,o.method==='GET'?{}:{url:'https://source.test/decision',redirected:true})),c=(await h.details(ref)).files[0].context;await assert.rejects(h.decision(c));
});
test("blocked actual fetch aborts on leave auth replacement and explicit cancellation",async()=>{
 for(const mode of ['leave','change','cancel']){let started,aborted=false;const ready=new Promise(r=>started=r);const h=harness((u,o)=>{if(o.method==='GET')return response(detail(),u);started();return new Promise((_,reject)=>o.signal.addEventListener('abort',()=>{aborted=true;reject(new Error('aborted'));},{once:true}));}),c=(await h.details(ref)).files[0].context,abort=new AbortController(),pending=h.decision(c,{},abort.signal);await ready;if(mode==='cancel')abort.abort();else h[mode]();await assert.rejects(pending);assert.equal(aborted,true);assert.equal(h.requests.length,2);}
});

test("oversized request and malformed duplicate or UTF8 bodies refuse without fallback",async()=>{
 const h=harness(),c=(await h.details(ref)).files[0].context;h.overflow();await assert.rejects(h.decision(c));assert.equal(h.requests.length,1);
 for(const bad of [wire().replace('"method":"remux"','"method":"remux","method":"direct_play"'),new Uint8Array([0xff,0xfe])]){const x=harness((u,o)=>response(o.method==='GET'?detail():bad,u)),f=(await x.details(ref)).files[0].context;await assert.rejects(x.decision(f));assert.equal(x.requests.length,2);}
});
test("old account refusal cannot retire new account decision context",async()=>{
 let release,started;const ready=new Promise(r=>started=r);let posts=0;const h=harness((u,o)=>{if(o.method==='GET')return response(detail(),u);if(++posts===1){started();return new Promise(r=>release=()=>r(response(wire(),u,401)));}return response(wire(),u);});
 const old=(await h.details(ref)).files[0].context,pending=h.decision(old);await ready;h.change();const fresh=(await h.details(ref)).files[0].context;release();await assert.rejects(pending);assert.equal((await h.decision(fresh)).file_id,'9007199254740993');assert.equal(h.requests.at(-1).options.headers.authorization,'Bearer next');
});

// Synthetic B protocol envelopes exercise the shipped caller; these tests do
// not claim physical Source production or accepted socket settlement.
const sid="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
function startReply(){return {session_id:sid,playlist_url:`/api/v1/hls/${sid}/master.m3u8`,vod:true,start_seconds:12.5,duration_ms:90000,media_origin_ms:0,control:{protocol:"plurx-playback-control-v1",url:`/api/v1/hls/${sid}/control`,generation:"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",control_epoch:1,next_exchange_ms:5000,lease_timeout_ms:300000}};}
function startBody(h){return {request_id:"cccccccc-cccc-4ccc-8ccc-cccccccccccc",playback_id:"browser-fixture",caps:h.caps(),start:12.5,force:"original",height:null,previous_session_id:null,control_sequence:null,reopen_reason:null,intent:null};}
test("synthetic complete B Start retains original caps resume and explicit null fields",async()=>{
 const h=harness((u,o)=>response(o.method==='GET'?detail():JSON.stringify(startReply()),u)),c=(await h.details(ref)).files[0].context,b=startBody(h),r=await h.start(c,b);
 assert.deepEqual(JSON.parse(h.requests[1].options.body),JSON.parse(JSON.stringify(b)));
 assert.equal(r._sharedContext.session_id,sid);assert.equal(r._sharedContext.source_file_id,'9007199254740993');assert.equal(r._sharedContext.source_ref.item_id,ref.item_id);assert.ok(!Object.keys(r).includes('_sharedContext'));
 for(const field of ['previous_session_id','control_sequence','reopen_reason','intent']){const before=h.requests.length;await assert.rejects(h.start(c,{...b,[field]:field==='control_sequence'?0:'unsupported'}),e=>e.code==='sharing_start_unsupported');assert.equal(h.requests.length,before);}
});
test("synthetic B Start rejects foreign session URLs malformed generation and late login",async()=>{
 for(const mutate of [r=>r.playlist_url='/api/v1/hls/'+ref.import_id+'/master.m3u8',r=>r.control.url='https://source/control',r=>r.control.generation='7',r=>r.control.control_epoch=0,r=>r.vod=false]){
  const reply=startReply();mutate(reply);const h=harness((u,o)=>response(o.method==='GET'?detail():JSON.stringify(reply),u)),c=(await h.details(ref)).files[0].context;await assert.rejects(h.start(c,startBody(h)));assert.equal(c.session_id,null);
 }
});
test("ordered Shared beats retry identical payload then resync409 without replaying old position",async()=>{
 let posts=0,gets=0;const h=harness((u,o)=>{
  if(o.method==='GET'){gets++;const raw=JSON.parse(detail('9007199254740993','1'));raw.watch={sequence:gets===1?4:20};return response(JSON.stringify(raw,(_,v)=>v),u);}
  if(u.endsWith('/hls/sessions'))return response(JSON.stringify(startReply()),u);
  if(++posts===1)throw new Error('uncertain network');
  if(posts===3)return response(JSON.stringify({code:'sharing_progress_stale',current_sequence:20}),u,409);
  return response('{}',u);
 });
 // Use a safe lifecycle here because JSON.parse/stringify is only fixture setup.
 const first=await h.details(ref),c=(await h.start(first.files[0].context,startBody(h)))._sharedContext;
 await assert.rejects(h.progress(c,0,90000,false));assert.equal(await h.progress(c,1000,90000,false),false);
 const beats=h.requests.filter(r=>r.url.endsWith('/progress'));assert.equal(beats[0].options.body,beats[1].options.body);assert.equal(JSON.parse(beats[0].options.body).position_ms,0);
 assert.equal(await h.progress(c,2000,90000,false),false);assert.equal(gets,2);
 h.leave();assert.equal(await h.progress(c,3000,90000,false),true);
 const last=JSON.parse(h.requests.at(-1).options.body);assert.equal(last.sequence,21);assert.equal(last.position_ms,3000);assert.equal(last.session_id,sid);
 h.change();const n=h.requests.length;await assert.rejects(h.progress(c,4000,90000,false));assert.equal(h.requests.length,n);
});

test("late synthetic Start cannot attach after original account replacement",async()=>{
 let release,started;const ready=new Promise(r=>started=r);
 const h=harness((u,o)=>{if(o.method==='GET')return response(detail(),u);started();return new Promise(r=>release=()=>r(response(JSON.stringify(startReply()),u)));});
 const c=(await h.details(ref)).files[0].context,pending=h.start(c,startBody(h));await ready;h.change();release();await assert.rejects(pending);assert.equal(c.session_id,null);
});

test("two imports of the same Source item share ordered beats across B sessions",async()=>{
 const other={...ref,import_id:'44444444-4444-4444-8444-444444444444'},second='dddddddd-dddd-4ddd-8ddd-dddddddddddd';let starts=0;
 const h=harness((u,o)=>{
  if(o.method==='GET'){let raw=detail('9007199254740993','1');if(u.includes(other.import_id))raw=raw.replaceAll(ref.import_id,other.import_id);return response(raw.slice(0,-1)+',"watch":{"sequence":4}}',u);}
  if(u.endsWith('/hls/sessions')){const r=startReply();if(++starts===2){r.session_id=second;r.playlist_url=r.playlist_url.replaceAll(sid,second);r.control.url=r.control.url.replaceAll(sid,second);}return response(JSON.stringify(r),u);}
  return response('{}',u);
 });
 const a=(await h.start((await h.details(ref)).files[0].context,startBody(h)))._sharedContext;
 assert.equal(await h.progress(a,1000,90000,false),true);
 const b=(await h.start((await h.details(other)).files[0].context,startBody(h)))._sharedContext;
 assert.equal(await h.progress(b,2000,90000,false),true);
 const beats=h.requests.filter(r=>r.url.endsWith('/progress')).map(r=>JSON.parse(r.options.body));
 assert.deepEqual(beats.map(r=>r.sequence),[5,6]);assert.deepEqual(beats.map(r=>r.session_id),[sid,second]);
});

test("ordinary B Start playlist retains only closed unique native subtitle diagnostic query",async()=>{
 for(const query of ['?native=1&subtitle=2&diagnostic=video-only','?subtitle=-1','?native=0','?subtitle=00','?token=x','?native=1&native=0','?native=%31','?subtitle=4096','?','?diagnostic=upstream','\n']){
  const reply=startReply();reply.playlist_url+=query;
  const h=harness((u,o)=>response(o.method==='GET'?detail():JSON.stringify(reply),u)),c=(await h.details(ref)).files[0].context;
  if(['?native=1&subtitle=2&diagnostic=video-only','?subtitle=-1','?native=0'].includes(query))assert.equal((await h.start(c,startBody(h))).playlist_url,reply.playlist_url);
  else await assert.rejects(h.start(c,startBody(h)));
 }
});

// Shared direct play: the same Start route, `presentation:"direct"`, and B's
// closed five-field reply. Synthetic envelopes, as above.
const dsid="eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
function directReply(id=dsid){return {presentation:"direct",session_id:id,url:`${base}/direct?session=${id}`,length:9007199254740991,mime:"video/mp4"};}
function directBody(h){return {...startBody(h),presentation:"direct"};}
test("synthetic direct Start binds only the exact five-field B reply to its own file alias",async()=>{
 const h=harness((u,o)=>response(o.method==='GET'?detail():JSON.stringify(directReply()),u)),c=(await h.details(ref)).files[0].context,r=await h.start(c,directBody(h));
 assert.equal(h.requests[1].url,'https://b.test'+base+'/hls/sessions');assert.equal(JSON.parse(h.requests[1].options.body).presentation,'direct');
 assert.equal(r._sharedContext.session_id,dsid);assert.equal(r.url,base+'/direct?session='+dsid);assert.equal(r.length,9007199254740991);assert.equal(c.session_id,null);
 assert.equal(h.key(r._sharedContext),h.key(c));
 for(const mutate of [r=>r.presentation='vod',r=>r.url=`${base}/direct?session=${sid}`,r=>r.url=`/api/v1/files/7/direct?session=${dsid}`,
   r=>r.url=`${base}/direct?session=${dsid}&token=x`,r=>r.url=`https://source.test${base}/direct?session=${dsid}`,r=>r.url=`${base}/stream.mp4?session=${dsid}`,
   r=>r.session_id=dsid.toUpperCase(),r=>r.session_id=dsid.replace('4eee','1eee'),r=>r.length=-1,r=>r.length=1.5,r=>r.length='7',r=>r.length=9007199254740992,
   r=>r.mime='text/html',r=>r.control={url:`/api/v1/hls/${dsid}/control`},r=>delete r.mime,r=>r.playlist_url=`/api/v1/hls/${dsid}/master.m3u8`]){
  const reply=directReply();mutate(reply);const x=harness((u,o)=>response(o.method==='GET'?detail():JSON.stringify(reply),u)),xc=(await x.details(ref)).files[0].context;
  await assert.rejects(x.start(xc,directBody(x)));assert.equal(xc.session_id,null);
 }
 // A direct ask never accepts an HLS reply, and an HLS ask never a direct one.
 const a=harness((u,o)=>response(o.method==='GET'?detail():JSON.stringify(startReply()),u));await assert.rejects(a.start((await a.details(ref)).files[0].context,directBody(a)));
 const b=harness((u,o)=>response(o.method==='GET'?detail():JSON.stringify(directReply()),u));await assert.rejects(b.start((await b.details(ref)).files[0].context,startBody(b)));
 // Closed presentation vocabulary and no subtitle ask on raw bytes, refused before any request.
 const d=harness(),dc=(await d.details(ref)).files[0].context;
 for(const body of [{...directBody(d),presentation:'live'},{...directBody(d),native_subtitles:true}])await assert.rejects(d.start(dc,body),e=>e.code==='sharing_start_unsupported');
 assert.equal(d.requests.length,1);
});
test("an expired direct play restarts as a fresh Start of its base file under the accepted login only",async()=>{
 const second='ffffffff-ffff-4fff-8fff-ffffffffffff';let starts=0;
 const h=harness((u,o)=>{if(o.method==='GET')return response(detail('9007199254740993','1'),u);if(u.endsWith('/hls/sessions'))return response(JSON.stringify(++starts===1?directReply():starts===2?directReply(second):startReply()),u);return response('{}',u);});
 const c=(await h.details(ref)).files[0].context,first=(await h.start(c,directBody(h)))._sharedContext;
 assert.equal(await h.progress(first,1000,90000,false),true);
 h.leave();
 const next=(await h.start(first,{...directBody(h),request_id:'dddddddd-dddd-4ddd-8ddd-dddddddddddd'}))._sharedContext;
 assert.equal(h.requests.at(-1).url,'https://b.test'+base+'/hls/sessions');assert.equal(next.session_id,second);assert.equal(h.key(next),h.key(c));
 assert.equal(await h.progress(next,2000,90000,false),true);
 assert.deepEqual(h.requests.filter(r=>r.url.endsWith('/progress')).map(r=>JSON.parse(r.options.body).session_id),[dsid,second]);
 // A compatibility move from direct to Copy HLS is also a fresh Start; an
 // HLS-bound context is not a direct session and gets no generic replacement.
 const hls=(await h.start(next,startBody(h)))._sharedContext;assert.equal(hls.session_id,sid);
 const n=h.requests.length;await assert.rejects(h.start(hls,startBody(h)));assert.equal(h.requests.length,n);
 h.change();await assert.rejects(h.start(next,directBody(h)));assert.equal(h.requests.length,n);
});
