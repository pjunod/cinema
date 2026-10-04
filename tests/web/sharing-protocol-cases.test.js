"use strict";
// tests/sharing/protocol-cases.json, consumed row for row by the shipped web
// code. The same rows drive the Rust `sharing_protocol_fixture_*` tests, and
// Swift and Kotlin consume them too: see "Shared protocol fixture parity
// matrix" in docs/features/SHARED-LIBRARIES-IMPLEMENTATION.md. A row's
// `layer` says who validates it: `client` and `both` rows are this suite's.
const assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path"),vm=require("node:vm"),{test}=require("node:test");
const acorn=require("../vendor/acorn.js");
const {WEB}=require("./shell-source.js");
const control=require("../../crates/plurxd/src/web/playback-control.js");
const F=JSON.parse(fs.readFileSync("tests/sharing/protocol-cases.json","utf8"));
const clientLayer=row=>row.layer==="client"||row.layer==="both";
function shippedFunction(file,name){
  const text=fs.readFileSync(path.join(WEB,file),"utf8");
  const node=acorn.parse(text,{ecmaVersion:"latest"}).body.find(n=>n.type==="FunctionDeclaration"&&n.id.name===name);
  assert.ok(node,`${file}::${name} exists`);return text.slice(node.start,node.end);
}
function mutated(base,row,session){
  const value=structuredClone(base);let target=value;
  for(const key of row.path.slice(0,-1)) target=target[key];
  const last=row.path[row.path.length-1];
  if(row.op==="remove") delete target[last];
  else target[last]=typeof row.value==="string"?row.value.replaceAll("{session}",session):structuredClone(row.value);
  return value;
}
function harness(){
  const ctx=vm.createContext({AUTH_GENERATION:0});
  vm.runInContext(fs.readFileSync(path.join(WEB,"core/file-context.js"),"utf8")+
    "\nthis.h={sharedPlaybackFileContextFromDetail,withPlaybackFileSession,playbackFileDecimal,playbackFileUrl,"+
    "sharedPlaybackStatusToken,sharedPlaybackStatusMetrics,sharedPlaybackStartContext,sharedPlaybackDirectStartContext,"+
    "mimes:()=>[...SHARED_DIRECT_MIMES],words:()=>[...SHARED_STATUS_WORDS]};",ctx);
  const c=F.context,lifecycle=BigInt(c.lifecycle_generation);
  const file={file_base:c.file_base,file_id:c.file_id,revision:c.revision,
    reference:{item:{...c.reference},file_id:c.file_id,revision:c.revision,lifecycle_generation:lifecycle}};
  return {h:ctx.h,base:ctx.h.sharedPlaybackFileContextFromDetail({...c.reference},file,lifecycle)};
}
const accepts=fn=>{try{fn();return true;}catch(_){return false;}};

test("fixture Source IDs keep the exact decimal wire grammar",()=>{
  const {h}=harness();
  assert.ok(F.cases.length>=12);
  for(const row of F.cases)
    assert.equal(accepts(()=>h.playbackFileDecimal(row.input.source_id)),row.expected==="accepted",row.id);
});

test("fixture status tokens and the bound Shared status grammar",()=>{
  const {h,base}=harness();
  for(const row of F.status_tokens) assert.equal(h.sharedPlaybackStatusToken(row.token),row.expected==="accepted",row.id);
  const accepted=F.shared_status.accepted,bound=h.withPlaybackFileSession(base,accepted.session_id);
  assert.deepEqual([...h.words()],F.shared_status.word_fields);
  for(const field of F.shared_status.word_fields) for(const row of F.status_tokens){
    const reply=structuredClone(accepted);reply.status[field]=row.token;
    assert.equal(h.sharedPlaybackStatusMetrics(bound,reply)!==null,row.expected==="accepted",`${row.id} in ${field}`);
  }
  assert.deepEqual(h.sharedPlaybackStatusMetrics(bound,accepted),accepted.status);
  assert.equal(h.sharedPlaybackStatusMetrics(base,accepted),null,"no started session, no sample");
  const rows=F.shared_status.mutations.filter(clientLayer);
  assert.ok(rows.length>=10);
  for(const row of rows)
    assert.equal(h.sharedPlaybackStatusMetrics(bound,mutated(accepted,row,accepted.session_id))!==null,row.expected==="accepted",row.id);
});

const B=F.hls_start.receiver.session_id,BI=F.hls_start.receiver.incarnation_id,EPOCH=F.hls_start.receiver.control_epoch;
function snapshot(){
  return {demand:"active",position_ms:1000,buffered_from_ms:1000,buffered_through_ms:11000,playback_rate:1,
    render_state:"rendering",seek_target_ms:null,observed_download_bps:8000000,
    selection:{quality:{mode:"auto"},audio_track:0,subtitle:{mode:"off",track:null},audio_offset_ms:0,codec:"auto",dynamic_range:"auto"},
    capabilities:{platform:"web",max_height:1080,codecs:["h264"],dynamic_ranges:["sdr"],dual_player_preparation:false},
    observation:null,acknowledgement:null};
}
// One refused exchange through the shipped transport (`sendPlaybackControl`)
// into the shipped reporter: what does the player do with B's answer?
async function clientOutcome(answer){
  const body={code:answer.code,message:"fixture",generation:BI,control_epoch:EPOCH};
  if(answer.retry_after_ms!=null) body.retry_after_ms=answer.retry_after_ms;
  if(answer.invalid_field) body.invalid_field=answer.invalid_field;
  const ctx=vm.createContext({TOKEN:"login",Response,
    fetch:async()=>new Response(JSON.stringify(body),{status:answer.status,headers:{"content-type":"application/json"}})});
  vm.runInContext(shippedFunction("player/prepared-switch-measurement.js","sendPlaybackControl")+"\nthis.send=sendPlaybackControl;",ctx);
  const timers=[];
  const reporter=new control.Reporter({
    bootstrap:{protocol:control.PROTOCOL,url:`/api/v1/hls/${B}/control`,generation:BI,control_epoch:EPOCH,next_exchange_ms:5000,lease_timeout_ms:300000},
    clientInstanceId:"77777777-7777-4777-8777-777777777777",
    capture:()=>control.capture(snapshot(),0,{lifecycleId:"fixture-player",attachmentGeneration:1}),
    send:(url,request,signal)=>ctx.send(url,request,signal),
    setTimer:(run,ms)=>{timers.push({run,ms});return timers.length;},clearTimer:()=>{},now:()=>0,
  }).start();
  for(let i=0;i<20;i++) await new Promise(resolve=>setImmediate(resolve));
  const status=reporter.status();reporter.stop();
  const outcome=status.stopped?"stop":status.retrying?"retry":"other";
  if(outcome==="retry") assert.ok(timers.some(t=>t.ms===answer.retry_after_ms),`${answer.code} honours B's retry hint`);
  return outcome;
}
test("fixture B control refusals reach the shipped player as retry or stop",async()=>{
  const rows=F.control_refusals.source.filter(r=>r.valid).concat(F.control_refusals.b_precheck.filter(r=>r.b));
  assert.ok(rows.length>=10);
  for(const row of rows) assert.equal(await clientOutcome(row.b),row.client,row.id);
});

test("fixture preparation answers: none declines at once, absence stays armed",()=>{
  const ctx=vm.createContext({window:{},performance:{now:()=>0},clientLog(){},playbackContext:()=>({}),setTimeout:()=>1,PREPARED_OFFER_CADENCE_MS:250});
  vm.runInContext(["function preparedOfferSuperseded(){return false;}","function preparedOfferBuilt(){return false;}",
    shippedFunction("player/directed-change.js","settlePreparedOfferWaiter"),"this.settle=settlePreparedOfferWaiter;"].join("\n"),ctx);
  for(const row of F.control_preparation){
    let outcome="armed";
    const waiter={settled:false,player:null,tappedAt:0,firstStagingAtMs:null,cadence:null,settle(value){outcome=value;this.settled=true;}};
    ctx.settle({},waiter,true,{action:{type:"none"},delivery:row.b===null?{}:{preparation:row.b}},null);
    assert.equal(outcome,row.client,row.id);
  }
});

test("fixture shared HLS Start replies bind only B's VOD session and control tuple",()=>{
  const {h,base}=harness(),start=F.hls_start;
  assert.equal(h.sharedPlaybackStartContext(base,start.public).session_id,B);
  const rows=start.mutations.filter(clientLayer);
  assert.ok(rows.length>=15);
  for(const row of rows)
    assert.equal(accepts(()=>h.sharedPlaybackStartContext(base,mutated(start.public,row,B))),row.expected==="accepted",row.id);
});

test("fixture direct Start replies, MIME set and URL binding",()=>{
  const {h,base}=harness(),direct=F.direct;
  assert.deepEqual([...h.mimes()],direct.mimes);
  assert.equal(h.sharedPlaybackDirectStartContext(base,direct.public).session_id,direct.b_session);
  const rows=direct.mutations.filter(clientLayer);
  assert.ok(rows.length>=12);
  for(const row of rows)
    assert.equal(accepts(()=>h.sharedPlaybackDirectStartContext(base,mutated(direct.public,row,direct.b_session))),row.expected==="accepted",row.id);
  for(const row of F.direct_session_query){
    const reply={...direct.public,url:`${F.context.file_base}/direct?${row.query}`};
    assert.equal(accepts(()=>h.sharedPlaybackDirectStartContext(base,reply)),row.expected==="accepted",row.id);
  }
});

test("fixture shared file suffixes follow the closed Shared grammar",()=>{
  const {h,base}=harness();
  assert.ok(F.file_suffixes.length>=20);
  for(const row of F.file_suffixes)
    assert.equal(accepts(()=>h.playbackFileUrl(base,row.suffix)),row.expected==="accepted",JSON.stringify(row.suffix));
});

test("fixture pre-session assets: signed-in route first, then exactly B's session",()=>{
  const {h,base}=harness(),assets=F.presession_assets,bound=h.withPlaybackFileSession(base,F.direct.b_session);
  const exact=F.asset_session_query.find(row=>row.query===assets.bound_query);
  assert.ok(exact&&exact.expected==="accepted"&&exact.session===F.direct.b_session,"B accepts the bound query");
  for(const suffix of assets.suffixes){
    assert.equal(h.playbackFileUrl(base,suffix),`${F.context.file_base}/${suffix}`,suffix);
    assert.equal(h.playbackFileUrl(bound,suffix),`${F.context.file_base}/${suffix}?${assets.bound_query}`,suffix);
  }
});

test("fixture receiver_recovery renders on the Sharing node card",()=>{
  const ctx=vm.createContext({esc:v=>String(v).replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll('"',"&quot;")});
  vm.runInContext(shippedFunction("pages/sharing-management.js","sharingRecoveryHTML")+"\nthis.render=sharingRecoveryHTML;",ctx);
  const html=ctx.render({receiver_recovery:F.receiver_recovery.status});
  for(const text of F.receiver_recovery.rendered) assert.ok(html.includes(text),text);
});
