"use strict";
const assert=require("node:assert/strict");
const test=require("node:test");
const vm=require("node:vm");
const {shellSource}=require("../web/shell-source.js");
const source=shellSource().bodyScript;
function declaration(name){
  const start=source.search(new RegExp("\\n(?:async )?function "+name+"\\("));
  assert.ok(start>=0,"missing shipped function "+name);
  const rest=source.slice(start+1), end=rest.slice(1).search(/\n(?:async )?function |\nconst |\nlet |\nwindow\./);
  return end<0?rest:rest.slice(0,end+1);
}
const generation="11111111-1111-4111-8111-111111111111";
const client="22222222-2222-4222-8222-222222222222";
function harness(){
  const calls=[], replies=[], timers=new Set();
  const scope={AbortController,CONTROL_CLIENT_ID:client,JSON,Number,String,Array,Object,
    setTimeout(fn){const token={fn};timers.add(token);return token;},clearTimeout(token){timers.delete(token);},
    fetch:async(url,options)=>{
      calls.push({url,body:JSON.parse(options.body),signal:options.signal});
      const reply=replies.shift();assert.ok(reply,"unexpected fetch");
      if(typeof reply==="function") return reply();
      return reply;
    }};
  vm.createContext(scope);
  const names=["qualityControlOwnerKey","qualityControlSupported","validQualityControlIdentity",
    "exchangeQualityControl","discoverQualityControl","cancelUnappendedQualityIntent"];
  vm.runInContext(names.map(declaration).join("\n"),scope);
  const player={sessionId:"session",controlReporter:{stopped:false,bootstrap:{
    url:"/api/v1/hls/session/control",generation,control_epoch:1}}};
  function reply(outcome="supported",identity=null,status=200){
    return {status,ok:status===200,text:async()=>JSON.stringify({version:1,generation,
      control_epoch:1,features:outcome==="unsupported"?[]:["quality_cancel_v1"],outcome,pending_identity:identity})};
  }
  return {scope,player,calls,replies,timers,reply};
}
function identity(revision=3){return {generation,control_epoch:1,client_instance_id:client,
  lifetime_id:"movie",recipe_revision:revision,accepted_sequence:7};}

test("old ingress discovery leaves cancellation unsupported and clears its timer",async()=>{
  const h=harness();h.replies.push(h.reply("unsupported",null,404));
  const result=await h.scope.discoverQualityControl(h.player);
  assert.equal(result.outcome,"unsupported");
  assert.equal(h.scope.qualityControlSupported(h.player),false);
  assert.equal(h.calls.length,1);assert.equal(h.timers.size,0);
  assert.equal(h.calls[0].url,"/api/v1/hls/session/quality-control");
});

test("an owner epoch change invalidates an in-flight negotiation",async()=>{
  const h=harness();let finish;
  h.replies.push(()=>new Promise(resolve=>{finish=resolve;}));
  const pending=h.scope.discoverQualityControl(h.player);
  h.player.controlReporter.bootstrap.control_epoch=2;
  finish(h.reply());
  assert.equal(await pending,null);
  assert.equal(h.scope.qualityControlSupported(h.player),false);
  assert.equal(h.timers.size,0);
});

test("a late cancel cannot use discovery for a newer quality intent",async()=>{
  const h=harness();h.replies.push(h.reply("supported",identity(4)));
  const outcome=await h.scope.cancelUnappendedQualityIntent(h.player,{qualityIntent:{lifetime_id:"movie",recipe_revision:3}});
  assert.equal(outcome,"observation_unknown");assert.equal(h.calls.length,1);
});

test("cancel carries the owner's exact planning sequence and only initiates cleanup",async()=>{
  const h=harness(), exact=identity();
  h.replies.push(h.reply("supported",exact),h.reply("cancel_requested",exact));
  const outcome=await h.scope.cancelUnappendedQualityIntent(h.player,{qualityIntent:{lifetime_id:"movie",recipe_revision:3}});
  assert.equal(outcome,"cancel_requested");assert.equal(h.calls.length,2);
  assert.deepEqual(h.calls[1].body.identity,exact);
  assert.equal(h.calls[1].body.operation,"cancel_unappended");
  assert.equal(h.timers.size,0);
});

test("unknown response fields cannot opt a client into extended control",async()=>{
  const h=harness();h.replies.push({status:200,ok:true,text:async()=>JSON.stringify({version:1,
    generation,control_epoch:1,features:["quality_cancel_v1"],outcome:"supported",pending_identity:null,unexpected:true})});
  assert.equal(await h.scope.discoverQualityControl(h.player),null);
  assert.equal(h.scope.qualityControlSupported(h.player),false);
});

test("discovery deduplicates requests for the same owner",async()=>{
  const h=harness();let finish;
  h.replies.push(()=>new Promise(resolve=>{finish=resolve;}));
  const one=h.scope.discoverQualityControl(h.player), two=h.scope.discoverQualityControl(h.player);
  assert.equal(h.calls.length,1);finish(h.reply());
  const [a,b]=await Promise.all([one,two]);assert.equal(a,b);
  assert.equal(h.scope.qualityControlSupported(h.player),true);
  assert.equal(h.timers.size,0);
});
