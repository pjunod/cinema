"use strict";
const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm');
const {Worker:Thread}=require('node:worker_threads');
const {webcrypto,createHash}=require('node:crypto');
const {init,media}=require('./continuous-media.fixture.js');
const source=fs.readFileSync('crates/plurxd/src/web/player/continuous-media.js','utf8');
function environment(stalled=false){
 const urls=new Map(),workers=[];let nextUrl=0;
 const stall=typeof stalled==='function'?stalled:()=>stalled;
 class PageWorker {
  constructor(url){
   this.index=workers.length;workers.push(this);this.terminated=false;
   this.ready=urls.get(url).text().then(body=>{
    if(this.terminated)return null;
    const prelude="const {parentPort}=require('node:worker_threads');globalThis.onmessage=null;globalThis.postMessage=value=>parentPort.postMessage(value);parentPort.on('message',data=>onmessage({data}));";
    this.child=new Thread(prelude+body,{eval:true});
    this.child.on('message',data=>this.onmessage?.({data}));
    this.child.on('error',()=>this.onerror?.());
    return this.child;
   });
  }
  postMessage(message,transfer){
   // Match the browser's immediate ownership transfer, including before
   // the worker has finished starting. No payload is borrowed from HLS.
   const copy=structuredClone(message,{transfer});
   if(stall(this.index))return;
   this.ready.then(child=>{
    if(!child||this.terminated)return;
    const bytes=copy.kind==='digest'?copy.bytes:copy.inspection.bytes;
    child.postMessage(copy,[bytes.buffer]);
   });
  }
  terminate(){this.terminated=true;if(this.child)this.exit=this.child.terminate();}
 }
 const context=vm.createContext({ArrayBuffer,Uint8Array,DataView,crypto:webcrypto,
  AbortController,setTimeout,clearTimeout,Blob,Worker:PageWorker,
  URL:{createObjectURL(blob){const url='owned:'+ ++nextUrl;urls.set(url,blob);return url;},revokeObjectURL(url){urls.delete(url);}}});
 vm.runInContext(source,context);
 return {context,urls,workers,async retired(){for(const worker of workers){await worker.ready;await worker.exit;}}};
}
test('attachment worker preserves verified bytes and sample provenance',async t=>{
 const env=environment(),owner=env.context.continuousMediaVerifier();
 t.after(async()=>{owner.close();await env.retired();assert.equal(env.urls.size,0);});
 const inspect=env.context.continuousMediaInspector();inspect(init());
 const payload=media(),inspection=inspect(payload),before=Buffer.from(payload);
 const verified=await owner.verify(inspection);
 assert.equal(verified.artifact,createHash('sha256').update(payload).digest('hex'));
 assert.deepEqual(payload,before);assert.ok(inspection.bytes.byteLength>0);
 const expected=await env.context.continuousSampleFacts(inspection);
 assert.equal(verified.facts.fingerprint,expected.fingerprint);
 assert.equal(verified.facts.configuration_digest,expected.configuration_digest);
 const rewritten=await owner.verify(inspect(media({sequence:99})));
 assert.equal(rewritten.facts.fingerprint,verified.facts.fingerprint);
 assert.notEqual(rewritten.artifact,verified.artifact);
 const altered=await owner.verify(inspect(media({payload:Buffer.from([8,7,6,5,4,3,2,1])})));
 assert.notEqual(altered.facts.fingerprint,verified.facts.fingerprint);
 const owned=inspect(new Uint8Array(payload).slice());
 const result=owner.facts(owned,true);
 assert.equal(owned.bytes.byteLength,0);
 assert.equal((await result).fingerprint,verified.facts.fingerprint);
 owner.close();await env.retired();
 assert.ok(env.workers.every(worker=>worker.terminated));
 await assert.rejects(owner.digest(new Uint8Array([1])),/verification ended/);
});
test('detachment cancels bounded worker jobs and releases its object URL',async()=>{
 const env=environment(true),owner=env.context.continuousMediaVerifier();
 const jobs=Array.from({length:4},()=>owner.digest(new Uint8Array(16*1024*1024)));
 const checked=jobs.map(job=>assert.rejects(job,/verification ended/));
 await assert.rejects(owner.digest(new Uint8Array([1])),/capacity/);
 owner.close();await Promise.all(checked);await env.retired();
 assert.equal(env.urls.size,0);assert.ok(env.workers.every(worker=>worker.terminated));
});
test('worker unavailable fallback preserves provenance and attachment cancellation',async()=>{
 const context=vm.createContext({ArrayBuffer,Uint8Array,DataView,crypto:webcrypto,AbortController,setTimeout,clearTimeout});
 vm.runInContext(source,context);
 const inspect=context.continuousMediaInspector();inspect(init());const payload=media();
 const owner=context.continuousMediaVerifier(),verified=await owner.verify(inspect(payload));
 assert.equal(verified.artifact,createHash('sha256').update(payload).digest('hex'));
 assert.equal(verified.facts.fingerprint,(await context.continuousSampleFacts(inspect(payload))).fingerprint);
 const pending=owner.verify(inspect(media()));const checked=assert.rejects(pending,/verification ended/);
 owner.close();await checked;
});
test('a stuck worker fails its own jobs and the next job uses a fresh worker',async()=>{
 const env=environment(index=>index===0),owner=env.context.continuousMediaVerifier({deadlineMs:50});
 const payload=new Uint8Array([1,2,3]);
 await assert.rejects(owner.digest(payload),/verification deadline/);
 assert.equal(env.workers.length,1);assert.ok(env.workers[0].terminated);
 assert.equal(await owner.digest(payload),createHash('sha256').update(payload).digest('hex'));
 assert.equal(env.workers.length,2,'the attachment kept verifying with a new worker');
 owner.close();await env.retired();assert.equal(env.urls.size,0);
});
