"use strict";
const {test}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm');
const {webcrypto,createHash}=require('node:crypto');
function environment(){
  const events={},timers=new Map();let next=0;
  const window={crypto:webcrypto,addEventListener:(name,fn)=>events[name]=fn};
  const document={addEventListener:(name,fn)=>events[name]=fn};
  const context=vm.createContext({window,document,crypto:webcrypto,TextEncoder,TextDecoder,Uint8Array,Uint32Array,DataView,ArrayBuffer,Blob,URL,AbortController,structuredClone,performance,setTimeout,clearTimeout,console});
  for(const name of ['wire','hash','store','recorder','export'])vm.runInContext(fs.readFileSync('crates/plurxd/src/web/core/diagnostics-'+name+'.js','utf8'),context);
  return {context,create:vm.runInContext('createCinemaDiagnosticRecorder',context),exporter:vm.runInContext('CinemaDiagnosticExport',context),timers,next};
}
test('typed recorder excludes raw canaries and preserves exact counters',()=>{
  const {create}=environment();let clock=Date.parse('2026-10-12T16:00:00Z');const recorder=create({now:()=>clock,monotonic:()=>0});
  recorder.record('network','error',{route:'auth',failure:'browser_network',url:'https://CANARY_HOST/?token=CANARY_SECRET',headers:{Authorization:'CANARY_SECRET'},message:'CANARY_USER'});
  recorder.record('playback','sample',{frame_count:'18446744073709551615',dropped_frames:'9007199254740993',title:'CANARY_TITLE'});
  const capture=recorder.freeze(),wire=JSON.stringify(capture.events);assert(!wire.includes('CANARY'));assert(wire.includes('18446744073709551615'));assert(wire.includes('9007199254740993'));
});
test('sample floods retain terminal markers within count and byte limits',()=>{
  const {create}=environment();const recorder=create({now:()=>Date.parse('2026-10-12T16:00:00Z'),monotonic:()=>1});
  recorder.record('playback','terminal',{state:'failed',failure:'decode'});
  for(let i=0;i<20000;i++)recorder.record('playback','sample',{playhead_ms:i,frame_count:String(i)});
  const capture=recorder.freeze();assert(capture.events.length<=10000);assert(JSON.stringify(capture.events).length<2097152);assert(BigInt(capture.dropped)>0n);assert(capture.events.some(e=>e.kind==='terminal'&&e.facts.failure==='decode'));
});
test('matching account reload recovers its own ring while account transition erases it',async()=>{
  const {create}=environment();const rows=new Map();const store={get:async key=>rows.get(key),put:async row=>rows.set(row.key,structuredClone(row)),clear:async()=>rows.clear(),remove:async key=>rows.delete(key),persistent:()=>true};
  const scope='a'.repeat(64),nextScope='b'.repeat(64),clock=Date.parse('2026-10-12T16:00:00Z');
  const first=create({now:()=>clock,monotonic:()=>0,store});await first.bind(scope);first.record('playback','terminal',{failure:'decode',state:'failed'});await first.persist();
  const reloaded=create({now:()=>clock,monotonic:()=>0,store});await reloaded.bind(scope);assert(reloaded.freeze().events.some(e=>e.facts.failure==='decode'));
  await reloaded.bind(nextScope);assert(!reloaded.freeze().events.some(e=>e.facts.failure==='decode'));assert(rows.has('ring:'+scope));await reloaded.bind(scope);assert(reloaded.freeze().events.some(e=>e.facts.failure==='decode'));
});
test('LAN HTTP hashing and portable ZIP preserve standard checksums',async()=>{
  const {context,exporter}=environment();context.crypto=undefined;
  const hash=vm.runInContext('cinemaDiagnosticSHA256',context);for(const value of [Buffer.alloc(0),Buffer.from('abc'),Buffer.alloc(100000,0xa5)])assert.equal(Buffer.from(await hash(new Uint8Array(value))).toString('hex'),createHash('sha256').update(value).digest('hex'));
  const fixture=JSON.parse(fs.readFileSync('crates/plurx-core/tests/fixtures/diagnostics-v1.json'));
  const manifest=JSON.parse(fixture.cases[0].json),result=await exporter.archive(manifest,{'description.txt':'A local fixture','client-events.jsonl':''});
  const bytes=Buffer.from(await result.blob.arrayBuffer());assert.equal(bytes.readUInt32LE(0),0x04034b50);
  const entries=new Map();let offset=0;while(bytes.readUInt32LE(offset)===0x04034b50){const length=bytes.readUInt32LE(offset+18),nameLength=bytes.readUInt16LE(offset+26),extra=bytes.readUInt16LE(offset+28),name=bytes.subarray(offset+30,offset+30+nameLength).toString(),start=offset+30+nameLength+extra;entries.set(name,bytes.subarray(start,start+length));offset=start+length;}
  const decoded=JSON.parse(entries.get('manifest.json'));for(const row of decoded.files)assert.equal(row.sha256,createHash('sha256').update(entries.get(row.name)).digest('hex'));assert(!decoded.files.some(row=>row.name==='manifest.json'));
});

test('finished reproduction relinquishes its identity for a new incident',async()=>{
  const {create}=environment();const rows=new Map();const store={get:async key=>rows.get(key),put:async row=>rows.set(row.key,structuredClone(row)),remove:async key=>rows.delete(key),persistent:()=>true};
  const recorder=create({store});const incident=recorder.freeze();recorder.startReproduction(null,incident);const completed=recorder.stopReproduction();assert.equal(completed.id,incident.id);assert.equal(recorder.current().completed.id,incident.id);
  await recorder.forgetReproduction(incident.id);assert.equal(recorder.current().completed,null);assert(!rows.has('reproduction:'+incident.scope));assert.notEqual(recorder.freeze().id,incident.id);
});
function reportingFunction(name) {
  const source=fs.readFileSync('crates/plurxd/src/web/pages/diagnostics.js','utf8');
  const program=require('acorn').parse(source,{ecmaVersion:'latest'});const fn=program.body.find(node=>node.type==='FunctionDeclaration'&&node.id.name===name);assert(fn);return source.slice(fn.start,fn.end);
}
test('account replacement during IndexedDB report recovery cannot adopt old evidence',async()=>{
  let recover,appended=0;let current={epoch:1,scope:'a'.repeat(64),completed:null,reproduction:null};
  const context=vm.createContext({AUTH_GENERATION:1,TOKEN:'account-a',DIAGNOSTIC_REPORT:null,DIAGNOSTIC_AUTH_READY:Promise.resolve(),document:{createElement:()=>({setAttribute(){}}),body:{append(){appended++}}},CinemaDiagnostics:{current:()=>({...current})},CinemaDiagnosticStore:{get:()=>new Promise(resolve=>{recover=resolve})}});
  vm.runInContext(reportingFunction('showDiagnostics'),context);const opening=context.showDiagnostics();await Promise.resolve();assert(recover);
  context.AUTH_GENERATION=2;context.TOKEN='account-b';current={...current,epoch:2,scope:'b'.repeat(64)};
  recover({expires:Date.now()+60000,capture:{scope:'a'.repeat(64),id:'old-account-report',events:[{private:'old evidence'}]}});await opening;
  assert.equal(context.DIAGNOSTIC_REPORT,null);assert.equal(appended,0);
});
test('resumed delivery confirms and displays its immutable destination',async()=>{
  const fields={origin:{value:'https://changed.example',disabled:false},category:{},titles:{}};let confirmation='';
  const run={deliveryState:{origin:'https://approved.example',state:'staged'},controller:new AbortController()};
  const context=vm.createContext({CinemaDiagnostics:{current:()=>({reproduction:null})},diagnosticElement:(_,name)=>fields[name],diagnosticMessage(){},confirm:text=>{confirmation=text;return false},diagnosticCurrent:()=>true,createCinemaDiagnosticDelivery:({state})=>({state})});
  for(const name of ['diagnosticSend','diagnosticPrepareDelivery'])vm.runInContext(reportingFunction(name),context);
  await context.diagnosticSend(run,false);assert(confirmation.includes('https://approved.example'));assert(!confirmation.includes('https://changed.example'));
  await context.diagnosticPrepareDelivery(run,'https://changed.example');assert.equal(fields.origin.value,'https://approved.example');assert.equal(fields.origin.disabled,true);assert.equal(run.deliveryState.origin,'https://approved.example');
});
