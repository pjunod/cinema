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
 `+fs.readFileSync(WEB+'core/file-context.js','utf8')+'\n'+functions+'\n'+fs.readFileSync(WEB+'core/shared-decision.js','utf8')+`\nthis.h={details:SHARED_DECISION.details,decision:SHARED_DECISION.decision,retire:sharedDecisionRetire,caps:currentCapsDocument,factory:sharedPlaybackFileContextFromDetail,key:playbackFileKey,change(){TOKEN="next";AUTH_GENERATION++;sharedDecisionRetire();},leave(){PAGE_RENDER_GENERATION++;sharedDecisionRetire();},origin(){API="https://other.test/api/v1";},overflow(){PLAY_CAPS.acodec="a".repeat(128*1024);},};`,ctx);return {...ctx.h,requests};}
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
