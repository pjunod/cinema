"use strict";
const test=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
function harness(){
  const storage=new Map(),identity={origin:"https://cinema.test",instance:"instance",user:"1",token:"bearer",generation:1};
  const context=vm.createContext({TextEncoder,TextDecoder,AbortController,Response,setTimeout,clearTimeout,fetch:()=>{},localStorage:{getItem:key=>storage.get(key)||null,setItem:(key,value)=>storage.set(key,value),removeItem:key=>storage.delete(key)}});
  for(const file of ["remote-guard.js","remote-client.js"])vm.runInContext(fs.readFileSync(path.join(__dirname,"../../crates/plurxd/src/web/core",file),"utf8"),context);
  return {...vm.runInContext("({Client:CinemaRemoteClient,load:cinemaRemoteLoad,save:cinemaRemoteSave,forget:cinemaRemoteForget,key:cinemaRemoteStorageKey,text:cinemaRemoteText,actionValid:CinemaRemoteWire.actionValid})",context),identity,storage};
}
const response=value=>new Response(JSON.stringify({version:"cinema.remote.v1",...value}),{headers:{"content-type":"application/json"}});
test("remote transport binds bearer and proof to current origin with no redirect or query forwarding",async()=>{
  const h=harness(),calls=[],client=new h.Client(h.identity,()=>h.identity,async(url,options)=>{calls.push({url,options});return response({accepted:true});});
  assert.equal((await client.request("presence",{body:{target:{}},proof:{kind:"receiver",secret:"A".repeat(43)}})).accepted,true);
  assert.equal(calls[0].url,"https://cinema.test/api/remote/v1/presence");assert.equal(calls[0].options.headers.authorization,"Bearer bearer");assert.equal(calls[0].options.headers["X-Cinema-Receiver-Secret"],"A".repeat(43));assert.equal(calls[0].options.redirect,"error");assert.equal(calls[0].options.cache,"no-store");
  for(const path of ["https://evil.test/commands","commands?token=bearer","../commands","grants/foreign"])await assert.rejects(client.request(path),/invalid remote endpoint/);assert.equal(calls.length,1);
});
test("identity and request generation reject late old-account responses",async()=>{
  const h=harness();let resolve;const client=new h.Client(h.identity,()=>h.identity,()=>new Promise(r=>{resolve=r;}));
  const request=client.request("receivers",{method:"GET"});h.identity={...h.identity,user:"2",token:"new",generation:2};resolve(response({receivers:[]}));await assert.rejects(request,/stale_identity/);
  const g=harness();let finish;const other=new g.Client(g.identity,()=>g.identity,()=>new Promise(r=>{finish=r;}));const stale=other.request("receivers",{method:"GET"});other.retire();finish(response({receivers:[]}));await assert.rejects(stale,/stale_identity/);assert.equal(other.requests.size,0);
});
test("transport caps streamed response before parse and rejects duplicate protocol fields",async()=>{
  const h=harness();const client=new h.Client(h.identity,()=>h.identity,async()=>new Response(" ".repeat(65537),{headers:{"content-type":"application/json"}}));await assert.rejects(client.request("receivers",{method:"GET"}),/too large/);
  const duplicate=new h.Client(h.identity,()=>h.identity,async()=>new Response('{"version":"cinema.remote.v1","version":"cinema.remote.v1"}',{headers:{"content-type":"application/json"}}));await assert.rejects(duplicate.request("receivers",{method:"GET"}),/invalid/);
});
test("installation and grant secrets remain scoped to origin account and server instance",()=>{
  const h=harness(),value={receiver:{receiver_id:"id",secret:"secret"},grants:[]};h.save(h.identity,value);assert.equal(h.load(h.identity).receiver.secret,"secret");
  for(const change of [{origin:"https://other.test"},{user:"2"},{instance:"replacement"}])assert.equal(h.load({...h.identity,...change}).receiver,undefined);
  h.forget(h.identity);assert.equal(h.load(h.identity).receiver,undefined);
});
test("invalid proofs and oversized requests do not retain unused abort controllers",async()=>{
  const h=harness(),client=new h.Client(h.identity,()=>h.identity,async()=>response({}));
  await assert.rejects(client.request("presence",{proof:{kind:"eval",secret:"A".repeat(43)}}),/invalid remote proof/);
  await assert.rejects(client.request("commands",{body:{text:"x".repeat(17000)}}),/too large/);assert.equal(client.requests.size,0);
});

test("presence accepts the64KiB state budget while command bodies remain16KiB",async()=>{
  const h=harness(),calls=[],client=new h.Client(h.identity,()=>h.identity,async(url,options)=>{calls.push(options.body.length);return response({accepted:true});});
  await client.request("presence",{body:{state:{title:"x".repeat(17000)}}});assert.equal(calls.length,1);
  await assert.rejects(client.request("presence",{body:{state:{title:"x".repeat(65537)}}}),/too large/);
  await assert.rejects(client.request("commands",{body:{text:"x".repeat(17000)}}),/too large/);
});

test("presentation labels replace Unicode controls without modifying ordinary search payloads",()=>{
  const h=harness();const label=h.text("Title\nwith\ttabs\u0085and controls",256);assert.equal(label,"Title with tabs and controls");const action={type:"text_replace",text:"query\nwith\ttabs",text_nonce:"00000000-0000-4000-8000-000000000001"};assert.equal(h.actionValid(action),true);assert.equal(action.text,"query\nwith\ttabs");
});
