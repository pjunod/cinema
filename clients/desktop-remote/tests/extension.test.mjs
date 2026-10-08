// SPDX-License-Identifier: Apache-2.0
import test from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import {DesktopWorker,senderMatches,nativeInputValid} from "../extension/worker.js";
import {installBridge,disableBridge} from "../extension/bridge.js";
const epoch="84b4c59c-5e6b-4b66-8d4d-556c0c62e349",credit="49a2a11e-f6bb-4840-8d9b-4b16e0aac54e";
function worker(){
  const calls=[],now=[0],browser={scripting:{executeScript:async value=>{calls.push(value);return [{frameId:0,documentId:"document-1",result:"applied"}];}}};
  const w=new DesktopWorker(browser,()=>now[0],()=>credit);w.binding={tabId:1,windowId:1,documentId:"document-1",origin:"https://cinema.invalid",epoch};
  w.credits.set(credit,{issued:0,deadline:750,binding:w.binding});return {w,calls,now};
}
const message=()=>({type:"input",key:"select",epoch,credit,sequence:1});
test("native inputs reject old credits even when disconnect status remains queued",()=>{
  const {w,calls,now}=worker();now[0]=750;w.receive(message());assert.equal(calls.length,0);
  now[0]=-1;w.receive(message());assert.equal(calls.length,0);
});
test("document sender requires exact origin frame tab and document",()=>{
  const {w}=worker(),sender={frameId:0,tab:{id:1},documentId:"document-1",origin:"https://cinema.invalid"};
  assert.equal(senderMatches(sender,w.binding),true);
  for(const bad of [{...sender,frameId:1},{...sender,documentId:"old"},{...sender,origin:"https://evil.invalid"},{...sender,tab:{id:2}}])assert.equal(senderMatches(bad,w.binding),false);
});
test("native duplicates arbitrary fields and unsupported keys cannot enter MAIN",async()=>{
  const {w,calls}=worker();w.receive({...message(),command:"eval"});w.receive({...message(),key:"power"});assert.equal(calls.length,0);
  w.receive(message());w.receive(message());await Promise.resolve();assert.equal(calls.length,1);
  assert.deepEqual(calls[0].target,{tabId:1,documentIds:["document-1"]});assert.equal(calls[0].world,"MAIN");
  assert.equal(nativeInputValid({...message(),sequence:0}),false);
});
test("MAIN delivery checks its own credit after a page stall and invalidates before context capture",()=>{
  let now=0,wall=0;const calls=[];
  const c=vm.createContext({TOKEN:"bearer",ME:{id:1},API:"/api/v1",location:{origin:"https://cinema.invalid"},document:{visibilityState:"visible",hasFocus:()=>true},performance:{now:()=>now},Date:{now:()=>wall},
    CinemaRemote:{physicalInput:()=>calls.push("invalidate"),snapshot:()=>{calls.push("snapshot");return {context_revision:9};},dispatch:(action,context)=>{calls.push({action,context});return "applied";}}});
  vm.runInContext("("+installBridge.toString()+")('"+epoch+"')",c);
  c.CinemaDesktopBridge.probe(credit);assert.deepEqual(calls,["snapshot"]);calls.length=0;now=750;assert.equal(c.CinemaDesktopBridge.input("select",credit,epoch),"expired");assert.deepEqual(calls,[]);
  now=800;c.CinemaDesktopBridge.probe(credit);calls.length=0;assert.equal(c.CinemaDesktopBridge.input("left",credit,epoch),"applied");assert.deepEqual(calls.slice(0,2),["invalidate","snapshot"]);assert.equal(calls[2].context.source,"local_cec");
});
test("MAIN binding cannot survive logout account switch hidden page or wake gap",()=>{
  for(const change of [c=>{c.TOKEN=null;},c=>{c.ME={id:2};},c=>{c.document.visibilityState="hidden";},c=>{c.wall=2000;}]){
    const c=vm.createContext({TOKEN:"bearer",ME:{id:1},API:"/api/v1",wall:0,location:{origin:"https://cinema.invalid"},document:{visibilityState:"visible",hasFocus:()=>true},performance:{now:()=>0},CinemaRemote:{physicalInput:()=>{},snapshot:()=>({}),dispatch:()=>"applied"}});
    c.Date={now:()=>c.wall};vm.runInContext("("+installBridge.toString()+")('"+epoch+"')",c);c.CinemaDesktopBridge.probe(credit);change(c);assert.equal(c.CinemaDesktopBridge.input("select",credit,epoch),"unavailable");
  }
});
test("content ports cannot send actions or host commands",()=>{
  const {w}=worker();let listener,disconnected=false;
  const port={name:"cinema-cec-document",sender:{frameId:0,tab:{id:2},documentId:"document-1",origin:"https://cinema.invalid"},disconnect:()=>{disconnected=true;},onMessage:{addListener:fn=>{listener=fn;}},onDisconnect:{addListener:()=>{}}};
  w.documentConnection(port);assert.equal(disconnected,true);assert.equal(listener,undefined);
});

function deferred(){let resolve;const promise=new Promise(r=>{resolve=r;});return {promise,resolve};}
function bindFixture(){
  const permissions=[],ports=[],calls=[],listeners={},events=name=>({addListener:fn=>{listeners[name]=fn;}});
  let n=0;const nonce=()=>"00000000-0000-4000-8000-"+String(++n).padStart(12,"0");
  const browser={permissions:{contains:()=>{const d=deferred();permissions.push(d);return d.promise;}},storage:{local:{set:async()=>{},get:async()=>({enabled:true})}},
    scripting:{executeScript:async value=>{calls.push(value);const documentId=value.target.documentIds?.[0]||"document-"+value.target.tabId;
      return [{frameId:0,documentId,result:value.func===installBridge?{ready:true,epoch:value.args[0]}:{ready:true}}];}},
    runtime:{id:"extension",getURL:path=>path,onConnect:events("connect"),onMessage:events("message"),onStartup:events("startup"),connectNative:()=>{const port={disconnected:false,postMessage:()=>{},disconnect(){this.disconnected=true;},onMessage:{addListener:()=>{}},onDisconnect:{addListener:()=>{}}};ports.push(port);return port;}},
    tabs:{onRemoved:events("removed"),onActivated:events("activated")},webNavigation:{onCommitted:events("committed")},windows:{WINDOW_ID_NONE:-1,onFocusChanged:events("focus")}};
  const w=new DesktopWorker(browser,()=>0,nonce);w.enabled=true;return {w,permissions,ports,calls,listeners};
}
const tab=id=>({id,windowId:id,url:"https://cinema.invalid/"});
const settle=async()=>{for(let i=0;i<10;i++)await Promise.resolve();};
test("overlapping bind operations fence delayed permissions and unbind during setup",async()=>{
  const f=bindFixture();const a=f.w.bind(tab(1)).catch(e=>e.message);await settle();const b=f.w.bind(tab(2));await settle();
  f.permissions[1].resolve(true);await b;f.permissions[0].resolve(true);assert.equal(await a,"binding_superseded");assert.equal(f.w.binding.tabId,2);assert.equal(f.ports.length,1);
  const g=bindFixture();const pending=g.w.bind(tab(1)).catch(e=>e.message);await settle();await g.w.unbind();g.permissions[0].resolve(true);assert.equal(await pending,"binding_superseded");assert.equal(g.w.binding,null);assert.equal(g.ports.length,0);
});
test("window focus loss fences in-flight bind and content ticks cannot rearm background input",async()=>{
  const f=bindFixture();f.w.start();await settle();const pending=f.w.bind(tab(1)).catch(e=>e.message);await settle();f.listeners.focus(2);f.permissions[0].resolve(true);assert.equal(await pending,"binding_superseded");assert.equal(f.ports.length,0);
  const g=bindFixture();g.w.start();await settle();const bound=g.w.bind(tab(1));await settle();g.permissions[0].resolve(true);await bound;g.listeners.focus(-1);await settle();assert.equal(g.w.binding,null);assert.equal(g.ports[0].disconnected,true);const count=g.calls.length;await g.w.probe();assert.equal(g.calls.length,count);
});
test("late MAIN install or disable cannot replace a newer epoch in the same document",()=>{
  const c=vm.createContext({TOKEN:"bearer",ME:{id:1},location:{origin:"https://cinema.invalid"},document:{visibilityState:"visible",hasFocus:()=>true},performance:{now:()=>0},Date:{now:()=>0},CinemaRemote:{physicalInput:()=>{},snapshot:()=>({}),dispatch:()=>"applied"}});
  const invoke=(e,op)=>vm.runInContext("("+installBridge.toString()+")("+JSON.stringify(e)+",'worker',"+op+")",c);
  invoke("new",2);assert.equal(invoke("old",1).ready,false);assert.equal(c.CinemaDesktopBridge.epoch,"new");
  // Execute the actual exported epoch-specific cleanup, as Chrome would.
  vm.runInContext("("+disableBridge.toString()+")('old')",c);c.CinemaDesktopBridge.probe(credit);assert.equal(c.CinemaDesktopBridge.input("select",credit,"new"),"applied");
});

test("visible background document rejects queued MAIN input at the effect boundary",()=>{
  let focused=true;const calls=[];
  const c=vm.createContext({TOKEN:"bearer",ME:{id:1},location:{origin:"https://cinema.invalid"},document:{visibilityState:"visible",hasFocus:()=>focused},performance:{now:()=>0},Date:{now:()=>0},CinemaRemote:{physicalInput:()=>calls.push("invalidate"),snapshot:()=>({}),dispatch:()=>calls.push("dispatch")}});
  const invoke=()=>vm.runInContext("("+installBridge.toString()+")('"+epoch+"')",c);
  assert.equal(invoke().ready,true);c.CinemaDesktopBridge.probe(credit);focused=false;
  assert.equal(c.CinemaDesktopBridge.input("select",credit,epoch),"unavailable");assert.deepEqual(calls,[]);
  assert.equal(c.CinemaDesktopBridge.probe(credit).ready,false);assert.equal(invoke().ready,false);
});

test("explicit popup setup waits for focus and remains cancellable without opening native host",async()=>{
  const f=bindFixture(),original=f.w.browser.scripting.executeScript;let focusCalls=0;
  f.w.browser.scripting.executeScript=async value=>{
    if(value.func===installBridge&&++focusCalls===1)return [{frameId:0,documentId:"document-1",result:{ready:false,focus_pending:true}}];
    return original(value);
  };
  const pending=f.w.bind(tab(1)).catch(e=>e.message);await settle();f.permissions[0].resolve(true);await settle();assert.equal(f.ports.length,0);
  await f.w.unbind();assert.equal(await pending,"binding_superseded");assert.equal(f.ports.length,0);assert.equal(f.w.binding,null);
});

test("focus setup pins its first document and pending navigation invalidates the operation",async()=>{
  const f=bindFixture();let attempts=0;const targets=[];
  f.w.browser.scripting.executeScript=async value=>{
    if(value.func!==installBridge)return [{frameId:0,documentId:value.target.documentIds[0],result:true}];
    targets.push(value.target);attempts++;
    return [{frameId:0,documentId:attempts===1?"document-1":"replacement",result:attempts===1?{ready:false,focus_pending:true}:{ready:true,epoch:value.args[0]}}];
  };
  const pending=f.w.bind(tab(1)).catch(e=>e.message);await settle();f.permissions[0].resolve(true);assert.equal(await pending,"document_replaced");
  assert.deepEqual(targets[1],{tabId:1,documentIds:["document-1"]});assert.equal(f.ports.length,0);assert.equal(f.w.binding,null);
  const g=bindFixture();g.w.start();await settle();const navigation=g.w.bind(tab(1)).catch(e=>e.message);await settle();g.listeners.committed({frameId:0,tabId:1,documentId:"replacement"});g.permissions[0].resolve(true);assert.equal(await navigation,"binding_superseded");assert.equal(g.ports.length,0);
});

test("saved Cinema local CEC choice fences install and queued delivery independently of server",()=>{
  const c=vm.createContext({TOKEN:"bearer",ME:{id:1},SERVER:{instance_id:"one"},enabled:false,cinemaRemoteLocalEnabled:()=>c.enabled,location:{origin:"https://cinema.invalid"},document:{visibilityState:"visible",hasFocus:()=>true},performance:{now:()=>0},Date:{now:()=>0},CinemaRemote:{physicalInput:()=>{},snapshot:()=>({}),dispatch:()=>"applied"}});
  const install=()=>vm.runInContext("("+installBridge.toString()+")('"+epoch+"')",c);
  assert.equal(install().ready,false);c.enabled=true;assert.equal(install().ready,true);c.CinemaDesktopBridge.probe(credit);c.enabled=false;assert.equal(c.CinemaDesktopBridge.input("select",credit,epoch),"unavailable");
  c.enabled=true;install();c.CinemaDesktopBridge.probe(credit);c.SERVER.instance_id="replacement";assert.equal(c.CinemaDesktopBridge.input("select",credit,epoch),"unavailable");
});
test("fixed local disable notification unbinds the document port without accepting page actions",async()=>{
  const {w}=worker();let receive;w.host={disconnect:()=>{},postMessage:()=>{}};
  const port={name:"cinema-cec-document",sender:{frameId:0,tab:{id:1},documentId:"document-1",origin:"https://cinema.invalid"},disconnect:()=>{},onMessage:{addListener:fn=>{receive=fn;}},onDisconnect:{addListener:()=>{}}};
  w.documentConnection(port);receive({type:"disabled",action:"select"});assert.notEqual(w.binding,null);receive({type:"disabled"});await Promise.resolve();assert.equal(w.binding,null);
});

test("MAIN Select requires a fresh physical focus and pending identity credit",()=>{
  let generation=1,focus="close",pending=null;const effects=[];
  const c=vm.createContext({TOKEN:"bearer",ME:{id:1},location:{origin:"https://cinema.invalid"},document:{visibilityState:"visible",hasFocus:()=>true},performance:{now:()=>0},Date:{now:()=>0},
    CinemaRemote:{physicalInput:()=>effects.push("invalidate")},CinemaRemoteLocalPhysical:{snapshot:()=>({generation,focus,pending}),dispatch:()=>{effects.push("dispatch");return "applied";}}});
  vm.runInContext("("+installBridge.toString()+")('"+epoch+"')",c);c.CinemaDesktopBridge.probe(credit);
  focus="approve";pending="phone-one";assert.equal(c.CinemaDesktopBridge.input("select",credit,epoch),"stale_focus");assert.deepEqual(effects,[]);
  c.CinemaDesktopBridge.probe(credit);generation++;pending="phone-two";assert.equal(c.CinemaDesktopBridge.input("select",credit,epoch),"stale_focus");assert.deepEqual(effects,[]);
  c.CinemaDesktopBridge.probe(credit);assert.equal(c.CinemaDesktopBridge.input("select",credit,epoch),"applied");assert.deepEqual(effects,["invalidate","dispatch"]);
});
