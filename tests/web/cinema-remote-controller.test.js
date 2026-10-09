"use strict";
const test=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const fixture=JSON.parse(fs.readFileSync(path.join(__dirname,"../../crates/plurx-core/tests/fixtures/remote-control-v1.json"))),wire=fixture.valid[1].command;
const old=wire.control_epoch,next="00000000-0000-4000-8000-000000000099";
function deferred(){let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return {promise,resolve,reject};}
function harness(){
  const timers=new Map();let n=0;
  const context=vm.createContext({TextEncoder,document:{visibilityState:"visible"},setTimeout:fn=>{timers.set(++n,fn);return n;},clearTimeout:id=>timers.delete(id),setInterval:fn=>{timers.set(++n,fn);return n;},clearInterval:id=>timers.delete(id)});
  for(const file of ["remote-guard.js","remote-controller.js"])vm.runInContext(fs.readFileSync(path.join(__dirname,"../../crates/plurxd/src/web/core",file),"utf8"),context);
  const Controller=vm.runInContext("CinemaWebController",context),sequences=new Map(),controller=new Controller({clock:()=>0,changed:()=>{},sequences});
  const device={available:true,receiver_id:wire.grant_id,target:wire.target},grant={receiver_id:wire.grant_id,grant_id:wire.grant_id,grant_secret:"A".repeat(43)},control={control_epoch:old,active_grant_id:wire.grant_id,controller_name:"Phone"};
  const state={state_revision:1,context_revision:4,focus_revision:7,route:"home",capabilities:["select","navigate"],focused_label:"Movie",credits:[{nonce:wire.credit,kind:"interaction"}],text_nonce:null,playback:null};
  const calls=[],client={identity:null,current:()=>true,retire:()=>{},request:async(path,options)=>{calls.push({path,...options});if(path==="commands")return {version:"cinema.remote.v1",queued:true,control_epoch:options.body.control_epoch,sequence:options.body.sequence};return {target:wire.target,response_revision:2,control};}};
  const install=()=>Object.assign(controller,{client,device,grant,control,state,responseRevision:1});install();
  return {controller,client,calls,sequences,timers,state,control,install};
}
test("observed own lease and retained allocator never reacquire across controller generations",async()=>{
  const h=harness();h.sequences.set(h.controller.sequenceKey(),4);await h.controller.send({type:"select"});assert.equal(h.calls.length,0);assert.equal(h.controller.ownsControl(),false);
  await h.controller.acquire();assert.equal(h.controller.ownsControl(),true);h.controller.state=h.state;await h.controller.send({type:"select"});assert.equal(h.calls.at(-1).body.sequence,5);
  h.controller.retire({release:false});h.install();assert.equal(h.controller.ownsControl(),false);await h.controller.send({type:"select"});assert.equal(h.calls.length,2);
  await h.controller.acquire();h.controller.state=h.state;await h.controller.send({type:"select"});assert.equal(h.calls.at(-1).body.sequence,6);h.controller.retire({release:false});
});
test("acquire response unknown own epoch is conditionally rotated before sequence allocation",async()=>{
  const h=harness();h.controller.control=null;
  h.client.request=async(path,options)=>{h.calls.push({path,...options});return {target:wire.target,response_revision:h.calls.length+1,control:{...h.control,control_epoch:h.calls.length===1?old:next}};};
  await h.controller.acquire();assert.deepEqual(h.calls.map(v=>[v.body.action,v.body.control_epoch]),[["acquire",null],["takeover",old]]);assert.equal(h.controller.control.control_epoch,next);assert.equal(h.controller.ownsControl(),true);assert.equal(h.sequences.get(h.controller.sequenceKey()),0);h.controller.retire({release:false});
});
test("conditional takeover rejects a racing changed lease without seeding sequence zero",async()=>{
  const h=harness();h.controller.control=null;let calls=0;
  h.client.request=async()=>{if(++calls===1)return {target:wire.target,response_revision:2,control:h.control};throw Object.assign(new Error("Lease changed"),{code:"stale_control"});};
  await assert.rejects(h.controller.acquire(),/Lease changed/);assert.equal(h.sequences.size,0);assert.equal(h.controller.ownsControl(),false);assert.equal(h.controller.acquirePending,null);
});
test("late old command cannot overwrite status or clear a new generation pending send",async()=>{
  const h=harness(),first=deferred(),second=deferred();let calls=0;
  h.sequences.set(h.controller.sequenceKey(),0);h.controller.acquiredGeneration=h.controller.generation;h.client.request=()=>++calls===1?first.promise:second.promise;
  const oldSend=h.controller.send({type:"select"});h.controller.retire({release:false});h.install();h.controller.acquiredGeneration=h.controller.generation;
  const newSend=h.controller.send({type:"select"}),pending=h.controller.sendPending;first.resolve({version:"cinema.remote.v1",queued:true,control_epoch:old,sequence:1});await oldSend;assert.equal(h.controller.sendPending,pending);assert.equal(h.controller.message,"Sending…");
  second.resolve({version:"cinema.remote.v1",queued:true,control_epoch:old,sequence:2});await newSend;assert.equal(h.controller.sendPending,null);assert.match(h.controller.message,/Queued/);
});
test("same target epoch null state preserves prior state and lease change clears it",async()=>{
  const h=harness();h.controller.control=null;h.client.request=async()=>({target:wire.target,response_revision:2,control:null,state:null,outcomes:[]});await h.controller.readState(h.controller.generation,0);assert.equal(h.controller.state,h.state);
  h.controller.control=h.control;h.controller.acceptControl({...h.control,control_epoch:next});assert.equal(h.controller.state,null);assert.equal(h.controller.acquiredGeneration,null);
});
test("actual ACK arriving before202 keeps applied outcome and transport failure stops held repeats",async()=>{
  const h=harness(),pending=deferred();h.sequences.set(h.controller.sequenceKey(),0);h.controller.acquiredGeneration=h.controller.generation;
  h.client.request=async path=>path==="commands"?pending.promise:{target:wire.target,response_revision:2,control:h.control,state:null,outcomes:[{control_epoch:old,sequence:1,outcome:"applied"}]};
  const sending=h.controller.send({type:"select"});await h.controller.readState(h.controller.generation,0);assert.equal(h.controller.message,"applied");pending.resolve({version:"cinema.remote.v1",queued:true,control_epoch:old,sequence:1});await sending;assert.equal(h.controller.message,"applied");
  h.client.request=async()=>{throw new Error("offline");};h.controller.hold("left");for(let i=0;i<5;i++)await Promise.resolve();assert.equal(h.controller.holdTimer,null);assert.equal(h.controller.holdOperation,null);
});

test("retired send failure cannot stop a new generation held gesture",async()=>{
  const h=harness(),oldRequest=deferred(),newRequest=deferred();let requests=0;
  h.sequences.set(h.controller.sequenceKey(),0);h.controller.acquiredGeneration=h.controller.generation;h.client.request=()=>++requests===1?oldRequest.promise:newRequest.promise;
  const sending=h.controller.send({type:"select"});h.controller.retire({release:false});h.install();h.controller.acquiredGeneration=h.controller.generation;h.controller.hold("left");const operation=h.controller.holdOperation,timer=h.controller.holdTimer;
  oldRequest.reject(new Error("old offline"));await sending;assert.equal(h.controller.holdOperation,operation);assert.equal(h.controller.holdTimer,timer);
  newRequest.resolve({version:"cinema.remote.v1",queued:true,control_epoch:old,sequence:2});for(let i=0;i<4;i++)await Promise.resolve();h.controller.retire({release:false});
});
test("old epoch renewal failure after fresh acquisition cannot retire the new lease",async()=>{
  const h=harness(),oldRenewal=deferred();h.sequences.set(h.controller.sequenceKey(),0);
  let renew=false;h.client.request=async(path,options)=>{if(options.body.action==="renew"){renew=true;return oldRenewal.promise;}return {target:wire.target,response_revision:3,control:{...h.control,control_epoch:options.body.action==="takeover"?next:old}};};
  await h.controller.acquire();const renewal=h.controller.renew(h.controller.generation);assert.equal(renew,true);
  await h.controller.acquire(true);assert.equal(h.controller.control.control_epoch,next);const generation=h.controller.generation;oldRenewal.reject(new Error("old lease failed"));await renewal;
  assert.equal(h.controller.generation,generation);assert.equal(h.controller.ownsControl(),true);assert.equal(h.controller.control.control_epoch,next);h.controller.retire({release:false});
});
