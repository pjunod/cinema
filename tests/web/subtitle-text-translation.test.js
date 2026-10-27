"use strict";
const test=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const {webcrypto}=require("node:crypto");
const WEB=path.join(__dirname,"../../crates/plurxd/src/web");
const JOB="11111111-1111-4111-8111-111111111111",NEXT="22222222-2222-4222-8222-222222222222",BOOT="33333333-3333-4333-8333-333333333333",CAPTION="a".repeat(64);
const MODEL="opus-mt-mul-en",FILE="9007199254740993";
const response=(data,status=200)=>new Response(JSON.stringify(data),{status,headers:{"content-type":"application/json"}});
const config=()=>({version:1,revision:"9007199254740993",enabled:false,model_id:MODEL,target_language:"en"});
const worker=()=>({node_id:"worker-1",boot_id:BOOT,label:"Worker <one>",model_id:MODEL,status:"unverified_cache"});
const catalog=()=>({version:1,catalog:{version:1,model_id:MODEL,adapter_contract:"marian-fixture",bundle_digest:"b".repeat(64),model_sha256:"c".repeat(64)},workers:[worker()],server_now_ms:1000});
const options=()=>({version:1,file_id:FILE,source_size:"9007199254740995",source_mtime:"9007199254740997",subtitles:[{index:0,language:"ja",title:"Original <script>",text_eligible:true},{index:2,language:"ja",text_eligible:false},{index:3,language:"en",text_eligible:true,downloaded_subtitle_id:CAPTION}],configuration:config(),model_id:MODEL,input_languages:["ja","de"],workers:[worker()],permissions:{read:true,create:true},server_now_ms:1000});
const receipt=(id=JOB)=>({version:1,job_id:id,state:"queued"});
const job=(fields={})=>({version:1,job_id:JOB,state:"queued",phase:null,reason:null,retryable:false,progress:{completed:0,total:0},result:null,...fields});
const result=()=>({file_id:FILE,subtitle_index:3,downloaded_subtitle_id:CAPTION,language:"en"});
function node(){return {textContent:"",innerHTML:"",hidden:false,disabled:false,isConnected:true,value:"",checked:false,children:[],listeners:{},addEventListener(kind,fn){this.listeners[kind]=fn;},appendChild(child){this.children.push(child);},replaceChildren(){this.children=[];this.textContent="";},querySelector(selector){return this.fields?.[selector]||null;},scrollIntoView(){}};}
function harness(kind="watch"){
  let timerId=0;const timers=new Map(),calls=[],used=[],panel=node(),setupPanel=node(),fields={};
  for(const name of ["status","source","language","confirm","start","stop","retry","use","worker","install","test","refresh","refresh-choices","save","save-status","enabled"])fields[`[data-text-${name}]`]=node();fields.form=node();
  fields["[data-text-source]"].value="0";fields["[data-text-language]"].value="ja";fields["[data-text-confirm]"].checked=true;fields["[data-text-worker]"].value="worker-1";fields["[data-text-enabled]"].checked=true;
  panel.fields=fields;setupPanel.fields=fields;const nodes={"watch-subtitle-text-translation":panel,"text-subtitle-translation-settings":setupPanel};
  const ctx=vm.createContext({console,TextEncoder,TextDecoder,Uint8Array,Response,AbortController,Map,Set,BigInt,Date,crypto:webcrypto,performance:{now:()=>1000},API:"/api/v1",AUTH_GENERATION:1,TOKEN:"private-fixture",PAGE_RENDER_GENERATION:1,ME:{is_admin:true},location:{hash:"#/watch/1"},document:{getElementById:id=>nodes[id]||null,createElement:()=>node()},setTimeout:(fn,ms)=>{const id=++timerId;timers.set(id,{fn,ms});return id;},clearTimeout:id=>timers.delete(id),esc:value=>String(value).replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;").replaceAll('"',"&quot;"),toast(){},textTranslationUseResult:(state,value)=>used.push({state,value}),fetch:async(url,init)=>{calls.push({url,init});return ctx.respond(url,init);},respond:()=>{throw Error("Unexpected request");}});
  vm.runInContext(fs.readFileSync(path.join(WEB,"core/file-context.js"),"utf8"),ctx);
  vm.runInContext(fs.readFileSync(path.join(WEB,"player/subtitle-translation.js"),"utf8"),ctx);
  vm.runInContext(fs.readFileSync(path.join(WEB,"player/subtitle-text-translation.js"),"utf8"),ctx);
  // These contract tests isolate job/result selection from the separate
  // player-integration regression in caption-controls.test.js.
  const shippedUseResult=ctx.textTranslationUseResult;ctx.textTranslationUseResult=(state,value)=>used.push({state,value});
  ctx.PLAYER={fileId:FILE,fileContext:ctx.localPlaybackFileContext(FILE)};const state=ctx.textTranslationState(kind,kind==="setup"?setupPanel:panel);state.player=ctx.PLAYER;state.fileContext=ctx.PLAYER.fileContext;state.fileId=FILE;state.options=options();state.saved=config();state.catalog=catalog();ctx.seed=state;
  vm.runInContext(kind==="setup"?"TEXT_TRANSLATION_SETUP=seed":"SUBTITLE_TEXT_TRANSLATION=seed",ctx);
  return {ctx,state,panel:state.panel,fields,calls,timers,used,shippedUseResult,player:ctx.PLAYER};
}
function operation(h){h.state.operation={job_id:JOB,source_size:h.state.options.source_size,source_mtime:h.state.options.source_mtime,source_index:0,source_language:"ja",selectionKey:h.ctx.textTranslationSelectionKey(h.state)};return h.state.operation;}
const settle=()=>new Promise(resolve=>setImmediate(resolve));

test("save keeps exact CAS revision and is independent of missing model readiness",async()=>{
  const h=harness("setup");h.state.catalog=null;h.ctx.respond=()=>response({...config(),revision:"9007199254740994",enabled:true});
  await h.ctx.saveTextTranslationSettings();assert.equal(h.calls.length,1);const body=JSON.parse(h.calls[0].init.body);
  assert.equal(body.expected_revision,"9007199254740993");assert.equal(body.enabled,true);assert.equal(h.calls[0].init.method,"PUT");assert.equal(h.fields["[data-text-enabled]"].checked,true);
});
test("duplicate saves cannot send two overlapping CAS writes",async()=>{
  const h=harness("setup");let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);
  const first=h.ctx.saveTextTranslationSettings();await h.ctx.saveTextTranslationSettings();assert.equal(h.calls.length,1);finish(response({...config(),enabled:true}));await first;
});
test("model actions reuse one request UUID after ambiguous failure and bind worker boot and model identity",async()=>{
  const h=harness("setup");let failures=1;h.ctx.respond=(url,init)=>url.includes("/models/install")?(failures--?Promise.reject(Error("network")):response(receipt())):response(job());
  await h.ctx.textTranslationSetupModel("install");await h.ctx.textTranslationSetupModel("install");await settle();
  const posts=h.calls.filter(call=>call.init.method==="POST");assert.equal(posts.length,2);assert.equal(posts[0].init.body,posts[1].init.body);
  const body=JSON.parse(posts[0].init.body);assert.equal(body.node_id,"worker-1");assert.equal(body.boot_id,BOOT);assert.equal(body.model_id,MODEL);assert.match(body.request_id,/^[0-9a-f-]{36}$/);
  assert.equal(h.state.catalog.workers[0].status,"unverified_cache");
});
test("model setup duplicate clicks remain blocked until the accepted job is terminal",async()=>{
  const h=harness("setup");let finish;h.ctx.respond=url=>url.includes("/models/verify")?new Promise(resolve=>finish=resolve):response(job());
  const first=h.ctx.textTranslationSetupModel("verify");await h.ctx.textTranslationSetupModel("verify");assert.equal(h.calls.length,1);finish(response(receipt()));await first;await h.ctx.textTranslationSetupModel("verify");await settle();assert.equal(h.calls.filter(call=>call.init.method==="POST").length,1);
});
test("a worker restart fences the old model test observation without inventing new readiness",async()=>{
  const h=harness("setup");h.state.operation={job_id:JOB,identity:h.ctx.textTranslationModelKey(h.state),node_id:"worker-1",boot_id:BOOT};let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);
  const read=h.ctx.textTranslationPoll(h.state);h.state.catalog.workers[0].boot_id=NEXT;finish(response(job({state:"succeeded"})));await read;
  assert.equal(h.state.operation,null);assert.equal(h.state.job,null);assert.equal(h.state.catalog.workers[0].status,"unverified_cache");assert.match(h.fields["[data-text-status]"].textContent,/boot changed/);
});
test("text create uses exact source strings, explicit supported language, and no speech task",async()=>{
  const h=harness();h.ctx.respond=url=>url.endsWith("/translation")?response(receipt()):response(job());await h.ctx.textTranslationStart();await settle();
  const body=JSON.parse(h.calls[0].init.body);assert.equal(body.source_size,"9007199254740995");assert.equal(body.source_mtime,"9007199254740997");assert.equal(body.source_language,"ja");assert.equal(body.target_language,"en");assert.equal(body.model_id,MODEL);assert.equal("task" in body,false);assert.equal("audio_index" in body,false);assert.equal(h.used.length,0);
});
test("captured Local context retains exact file identity across same-file session handoffs",async()=>{
  const h=harness();h.state.retired=true;h.ctx.respond=()=>response(options());
  await h.ctx.openTextSubtitleTranslation();
  const state=vm.runInContext("SUBTITLE_TEXT_TRANSLATION",h.ctx),captured=state.fileContext;
  assert.equal(captured.source_ref.file_id,FILE);assert.equal(state.fileId,FILE);
  h.player.fileContext=h.ctx.withPlaybackFileSession(h.ctx.localPlaybackFileContext(FILE),JOB);
  assert.notEqual(h.player.fileContext,captured);assert.equal(state.current(),true);
  await h.ctx.textTranslationRefreshChoices();
  assert.equal(h.calls.length,2);assert.ok(h.calls.every(call=>call.url===`/api/v1/files/${FILE}/subtitle-text-translation/options`));
});
test("replacing a captured Local context refuses new work and fences a held create response",async()=>{
  const h=harness();let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);
  const create=h.ctx.textTranslationStart();assert.equal(h.calls[0].url,`/api/v1/files/${FILE}/subtitles/0/translation`);
  h.player.fileContext=h.ctx.localPlaybackFileContext("42");
  assert.equal(h.player.fileId,FILE);assert.equal(h.state.current(),false);
  finish(response(receipt()));await create;
  assert.equal(h.state.operation,null);assert.equal(h.state.job,null);assert.equal(h.used.length,0);
  await h.ctx.textTranslationStart();await h.ctx.textTranslationRefreshChoices();
  assert.equal(h.calls.length,1);assert.equal(h.timers.size,0);
});
test("a held Use decision stays bound to the captured source and cannot adopt into its replacement",async()=>{
  const h=harness();operation(h);const selected=[];h.ctx.setSub=async index=>selected.push(index);h.ctx.currentCapsDocument=()=>({version:2});
  let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);
  const use=h.shippedUseResult(h.state,result());
  assert.equal(h.calls[0].url,`/api/v1/files/${FILE}/decision?force=auto`);
  h.player.fileContext=h.ctx.localPlaybackFileContext("42");
  finish(response({subtitles:[{index:3,source:"downloaded",text:true,downloaded_subtitle_id:CAPTION}],server_now_ms:1000}));
  await assert.rejects(use,/Retired translation response/);
  assert.deepEqual(selected,[]);assert.equal(h.calls.length,1);assert.equal(h.used.length,0);
});
test("bitmap sources, unconfirmed language, and unsupported model codes cannot submit work",async()=>{
  for(const change of [h=>h.fields["[data-text-source]"].value="2",h=>h.fields["[data-text-confirm]"].checked=false,h=>h.fields["[data-text-language]"].value="und"]){const h=harness();change(h);await h.ctx.textTranslationStart();assert.equal(h.calls.length,0);}
});
test("create duplicate and ambiguous response replay keep one mutation identity",async()=>{
  const h=harness();let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const first=h.ctx.textTranslationStart();await h.ctx.textTranslationStart();assert.equal(h.calls.length,1);finish(Promise.reject(Error("network")));await first;
  const original=h.calls[0].init.body;h.ctx.respond=url=>url.endsWith("/translation")?response(receipt()):response(job());await h.ctx.textTranslationStart();await settle();assert.equal(h.calls.filter(call=>call.init.method==="POST").length,2);assert.equal(h.calls[1].init.body,original);
});
test("a source or language selection change fences a late create response",async()=>{
  const h=harness();let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const create=h.ctx.textTranslationStart();h.fields["[data-text-language]"].value="de";finish(response(receipt()));await create;assert.equal(h.state.operation,null);assert.equal(h.used.length,0);assert.equal(h.calls.length,1);
});
test("Stop during create latches until the durable job receipt and never selects its result",async()=>{
  const h=harness();let finish;h.ctx.respond=url=>url.endsWith("/translation")?new Promise(resolve=>finish=resolve):url.endsWith("/cancel")?response({state:"cancelled"}):response(job({state:"cancelled",retryable:true}));
  const create=h.ctx.textTranslationStart();assert.equal(h.fields["[data-text-stop]"].disabled,false);await h.ctx.textTranslationStop();finish(response(receipt()));await create;await settle();assert.equal(h.calls.filter(call=>call.url.endsWith(`/cluster/jobs/${JOB}/cancel`)).length,1);assert.equal(h.used.length,0);
});
test("Stop survives an ambiguous create replay and cancels its eventual receipt",async()=>{
  const h=harness();let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const first=h.ctx.textTranslationStart();await h.ctx.textTranslationStop();finish(Promise.reject(Error("network")));await first;const body=h.calls[0].init.body;
  h.ctx.respond=url=>url.endsWith("/translation")?response(receipt()):url.endsWith("/cancel")?response({state:"cancelled"}):response(job({state:"cancelled",retryable:true}));await h.ctx.textTranslationStart();await settle();assert.equal(h.calls.find((call,index)=>index>0&&call.url.endsWith("/translation")).init.body,body);assert.equal(h.calls.filter(call=>call.url.endsWith("/cancel")).length,1);assert.equal(h.used.length,0);
});
test("Stop during explicit retry cancels the replacement job instead of being undone by its response",async()=>{
  const h=harness();operation(h);h.state.job=job({state:"cancelled",retryable:true});let finish;h.ctx.respond=url=>url.endsWith("/retry")?new Promise(resolve=>finish=resolve):url.endsWith("/cancel")?response({state:"cancelled"}):response(job({job_id:NEXT,state:"cancelled",retryable:true}));const retry=h.ctx.textTranslationRetry();await h.ctx.textTranslationStop();finish(response(receipt(NEXT)));await retry;await settle();assert.equal(h.calls.filter(call=>call.url.endsWith(`/cluster/jobs/${NEXT}/cancel`)).length,1);assert.equal(h.used.length,0);
});
test("job polls use five-second minimum and capped exponential backoff",async()=>{
  const h=harness();operation(h);h.ctx.respond=()=>response(job());await h.ctx.textTranslationPoll(h.state);assert.deepEqual([...h.timers.values()].map(timer=>timer.ms),[5000]);
  h.ctx.respond=()=>Promise.reject(Error("network"));for(const delay of [10000,20000,30000,30000]){await h.ctx.textTranslationPoll(h.state);assert.deepEqual([...h.timers.values()].map(timer=>timer.ms),[delay]);}
});
test("auth or selected-file change rejects a pending job response and schedules no stale polling",async()=>{
  for(const change of [h=>h.ctx.AUTH_GENERATION++,h=>h.player.fileId="42"]){const h=harness();operation(h);let finish;h.ctx.respond=()=>new Promise(resolve=>finish=resolve);const read=h.ctx.textTranslationPoll(h.state);change(h);finish(response(job({state:"succeeded",result:result()})));await read;assert.equal(h.state.job,null);assert.equal(h.used.length,0);assert.equal(h.timers.size,0);}
});
test("completion leaves playback alone and explicit Use verifies current source plus immutable caption ID",async()=>{
  const h=harness();operation(h);h.ctx.respond=url=>url.endsWith("/options")?response(options()):response(job({state:"succeeded",result:result()}));await h.ctx.textTranslationPoll(h.state);assert.equal(h.used.length,0);await h.ctx.textTranslationUse();assert.equal(h.used.length,1);assert.equal(h.used[0].value.downloaded_subtitle_id,CAPTION);
});
test("explicit Use resolves an unchanged caption ID after its ordinal moves",async()=>{
  const h=harness();operation(h);h.state.job=job({state:"succeeded",result:result()});const latest=options();latest.subtitles[2].index=7;h.ctx.respond=()=>response(latest);await h.ctx.textTranslationUse();assert.equal(h.used.length,1);assert.equal(h.used[0].value.subtitle_index,7);assert.equal(h.used[0].value.downloaded_subtitle_id,CAPTION);
});
test("Use rejects a replaced caption at the same index, changed source, or changed language selection",async()=>{
  for(const change of [value=>value.subtitles[2].downloaded_subtitle_id="d".repeat(64),value=>value.source_size="9007199254740994",value=>value.source_mtime="9007199254740998"]){const h=harness();operation(h);h.state.job=job({state:"succeeded",result:result()});const latest=options();change(latest);h.ctx.respond=()=>response(latest);await h.ctx.textTranslationUse();assert.equal(h.used.length,0);}
  const h=harness();operation(h);h.state.job=job({state:"succeeded",result:result()});h.fields["[data-text-language]"].value="de";h.ctx.respond=()=>response(options());await h.ctx.textTranslationUse();assert.equal(h.used.length,0);
});
test("explicit retry reuses its request UUID after an ambiguous failure",async()=>{
  const h=harness();operation(h);h.state.job=job({state:"failed",retryable:true});let failures=1;h.ctx.respond=url=>url.endsWith("/retry")?(failures--?Promise.reject(Error("network")):response(receipt(NEXT))):response(job({job_id:NEXT}));
  await h.ctx.textTranslationRetry();await h.ctx.textTranslationRetry();await settle();const posts=h.calls.filter(call=>call.init.method==="POST");assert.equal(posts.length,2);assert.equal(posts[0].init.body,posts[1].init.body);assert.equal(h.state.operation.job_id,NEXT);assert.equal(h.used.length,0);
});
test("a response over the shared byte cap is refused before any job is adopted",async()=>{
  const h=harness();operation(h);h.ctx.respond=()=>response({padding:"x".repeat(65537)});await h.ctx.textTranslationPoll(h.state);assert.equal(h.state.job,null);assert.equal(h.used.length,0);assert.match(h.fields["[data-text-status]"].textContent,/unavailable/);
});

test("status explanations expose bounded known phases and never raw worker prose",()=>{
  const h=harness();assert.match(h.ctx.textTranslationExplain("text_input_too_long"),/input length/);
  const label=h.ctx.textTranslationJobLabel(job({state:"failed",phase:"<script>private path</script>",reason:"/srv/private/source.vtt"}));
  assert.equal(label.includes("<script>"),false);assert.equal(label.includes("/srv/private"),false);assert.match(label,/Failed/);
});
