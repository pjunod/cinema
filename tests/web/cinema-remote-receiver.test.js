"use strict";
const test=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const fixture=JSON.parse(fs.readFileSync(path.join(__dirname,"../../crates/plurx-core/tests/fixtures/remote-control-v1.json"))),command=fixture.valid[1].command;
function harness(){
  const storage=new Map(),document={visibilityState:"visible",focused:true,hasFocus(){return this.focused;}},listeners=[],identity={origin:"https://cinema.test",instance:"one",user:"1",token:"bearer",generation:1};
  const context=vm.createContext({TextEncoder,TextDecoder,AbortController,crypto:require("node:crypto").webcrypto,setTimeout,clearTimeout,setInterval:()=>1,clearInterval:()=>{},fetch:()=>{},document,CinemaRemote:{onInvalidate:fn=>{listeners.push(fn);return ()=>{};}},CinemaRemoteCancelGestures:()=>{},localStorage:{getItem:key=>storage.get(key)||null,setItem:(key,value)=>storage.set(key,value)}});
  for(const file of ["remote-guard.js","remote-client.js","remote-receiver.js"])vm.runInContext(fs.readFileSync(path.join(__dirname,"../../crates/plurxd/src/web/core",file),"utf8"),context);
  const types=vm.runInContext("({Receiver:CinemaWebReceiver,save:cinemaRemoteSave,bound:cinemaRemoteBoundPublishedState,safeState:cinemaRemoteSafeState,summary:cinemaRemotePlaybackSummary})",context),now=[100],wall=[100],effects=[],state={context_revision:4,focus_revision:7,text_nonce:null,focused_id:"item",focused_label:"Movie",blocked:false,route:"home",capabilities:["select"],playback:null};
  const options={identityReader:()=>identity,clock:()=>now[0],wall:()=>wall[0],preferences:()=>({receiver:true}),stateReader:()=>state,effect:action=>{effects.push(action);return "applied";}};
  types.save(identity,{receiver:{receiver_id:command.grant_id,receiver_secret:"A".repeat(43)},grants:[]});
  return {...types,context,document,listeners,identity,now,wall,effects,state,options};
}
const flush=async()=>{for(let i=0;i<15;i++)await Promise.resolve();};
test("atomic Web Lock gives one installation owner and releases on foreground retirement",async()=>{
  const h=harness();let owned=false;const calls=[];
  const locks={request:async(name,options,fn)=>{assert.equal(options.ifAvailable,true);if(owned)return fn(null);owned=true;try{return await fn({name});}finally{owned=false;}}};
  const clientFactory=()=>({retire:()=>{},request:async(path)=>{calls.push(path);if(path==="sessions")return {target:command.target};if(path==="presence")return {accepted:true};return new Promise(()=>{});}});
  const one=new h.Receiver({...h.options,locks,clientFactory}),two=new h.Receiver({...h.options,locks,clientFactory});const first=one.start();await flush();await two.start();assert.equal(one.state,"available");assert.equal(two.state,"owned_by_another_tab");assert.equal(calls.filter(v=>v==="sessions").length,1);
  one.retire("background",true);await first;assert.equal(owned,false);assert.equal(one.foreground,null);const second=two.start();await flush();assert.equal(two.state,"available");two.retire();await second;
});
test("missing Web Locks reports receiver unavailable without altering saved preferences or registering",async()=>{
  const h=harness();let clients=0;const receiver=new h.Receiver({...h.options,locks:null,clientFactory:()=>{clients++;}});await receiver.start();assert.equal(receiver.state,"web_locks_unavailable");assert.equal(h.options.preferences().receiver,true);assert.equal(clients,0);
});
function admitted(){const h=harness(),receiver=new h.Receiver({...h.options,locks:null});receiver.identity={...h.identity};receiver.heldLock=true;receiver.target=command.target;receiver.control={active_grant_id:command.grant_id,control_epoch:command.control_epoch,controller_name:"Phone"};receiver.guard.setContext(receiver.context(h.state));receiver.guard.mint("interaction",command.credit,h.now[0]);return {...h,receiver};}
test("visible unfocused identity-changed and wake-gap receivers reject effects synchronously",()=>{
  for(const change of [h=>{h.document.focused=false;},h=>{h.identity.token="new";},h=>{h.wall[0]=2200;}]){
    const h=admitted();change(h);assert.equal(h.receiver.apply(command,h.receiver.generation),"unavailable");assert.equal(h.effects.length,0);
  }
});
test("physical input clears credits before network delivery without resetting consumed sequence",()=>{
  const h=admitted();assert.equal(h.receiver.apply(command,h.receiver.generation),"applied");for(const fn of h.listeners)fn("physical_input");h.receiver.guard.mint("interaction",command.credit,h.now[0]);assert.equal(h.receiver.apply(command,h.receiver.generation),"duplicate_or_old");assert.equal(h.effects.length,1);
  const fresh=admitted();for(const fn of fresh.listeners)fn("physical_input");assert.equal(fresh.receiver.apply(command,fresh.receiver.generation),"expired");assert.equal(fresh.effects.length,0);
});
test("restricted local modal rejects and never consumes a Select sequence",()=>{
  const h=admitted();h.state.route="restricted";h.state.blocked=true;h.state.capabilities=[];assert.equal(h.receiver.apply(command,h.receiver.generation),"restricted_surface");assert.equal(h.receiver.guard.lastSequence,0);assert.equal(h.effects.length,0);
});

test("transient blur preserves foreground identity while actual hidden lifecycle rotates it",async()=>{
  const h=harness(),locks={request:async(name,options,fn)=>fn({name})},clientFactory=()=>({retire:()=>{},request:async path=>path==="sessions"?{target:command.target}:path==="presence"?{accepted:true}:new Promise(()=>{})});
  const receiver=new h.Receiver({...h.options,locks,clientFactory});let pending=receiver.start();await flush();const foreground=receiver.foreground;
  h.document.focused=false;receiver.retire("background",false);await pending;h.document.focused=true;pending=receiver.start();await flush();assert.equal(receiver.foreground,foreground);
  h.document.visibilityState="hidden";receiver.retire("background",true);await pending;h.document.visibilityState="visible";pending=receiver.start();await flush();assert.notEqual(receiver.foreground,foreground);receiver.retire();await pending;
});

test("escaped metadata stays under normalized48KiB state budget with unchanged offered option IDs",()=>{
  const h=harness(),tracks=Array.from({length:64},(_,index)=>({kind:"audio",option_id:String(index).padStart(8,"0")+'"\\'.repeat(60),label:'"\\'.repeat(128)}));
  const state={state_revision:1,context_revision:1,focus_revision:1,route:"tracks",capabilities:["choose_track"],focused_label:'"\\'.repeat(128),text_nonce:null,credits:[],playback:{media:{type:"item",item_id:1},title:'"\\'.repeat(128),playing:false,position_ms:0,duration_ms:1000,tracks}};
  assert.ok(Buffer.byteLength(JSON.stringify(state))>49152);const bounded=h.bound(state);assert.ok(Buffer.byteLength(JSON.stringify(bounded))<=48640);assert.ok(bounded.playback.tracks.length>0&&bounded.playback.tracks.length<64);assert.deepEqual(Array.from(bounded.playback.tracks,value=>value.option_id),tracks.slice(0,bounded.playback.tracks.length).map(value=>value.option_id));assert.equal(state.playback.tracks.length,64);assert.deepEqual(bounded.capabilities,["choose_track"]);
});

test("actual publisher uses null for absent focus and normalizes stored title and track labels",()=>{
  const h=harness(),snapshot={context_revision:1,focus_revision:1,text_nonce:null,focused_id:null,focused_label:"",blocked:false,route_category:"home"};h.context.CinemaRemote.snapshot=()=>snapshot;h.context.location={hash:"#/"};h.context.cinemaRemoteVod=()=>false;h.context.cinemaRemoteLive=()=>false;assert.equal(h.safeState().focused_label,null);
  snapshot.focused_id="item:1";snapshot.focused_label="Title\nwith\ttabs";assert.equal(h.safeState().focused_label,"Title with tabs");
  h.context.cinemaRemoteVod=()=>true;h.context.PLAYER={meta:{item_id:"1"},title:"B05\nFixture\tTitle",audio:[{title:"Audio\ntrack\tlabel"}],subs:[]};h.document.getElementById=()=>({paused:true});h.context.playerWantsPlayback=()=>false;h.context.pbPosSec=()=>0;h.context.pbTotalSec=()=>12;h.context.audioLabelMenu=value=>value.title;h.context.qualityOptions=()=>[];
  const summary=h.summary();assert.equal(summary.title,"B05 Fixture Title");assert.equal(summary.tracks[0].label,"Audio track label");assert.equal(summary.tracks[0].option_id,"0");
});
