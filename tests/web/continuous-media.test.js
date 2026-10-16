"use strict";
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm');
const {webcrypto,createHash}=require('node:crypto');
const context=vm.createContext({ArrayBuffer,Uint8Array,DataView,crypto:webcrypto,setTimeout});
vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-media.js','utf8'),context);
const {continuousMediaInspector:inspector,continuousSampleDigest:digest}=context;
const {init,media,word,join}=require('./continuous-media.fixture.js');
test('actual samples preserve rational PTS and elementary provenance across container rewrites',async()=>{
  const inspect=inspector();inspect(init());
  const a=inspect(media()),b=inspect(media({sequence:99})),c=inspect(media({payload:Buffer.from([8,7,6,5,4,3,2,1])}));
  assert.equal(a.fragments.length,1);const fragment=a.fragments[0];
  assert.equal(fragment.from_tick,24000);assert.equal(fragment.through_tick,26002);
  assert.equal(fragment.track.width,1280);assert.equal(fragment.track.height,720);
  assert.equal(fragment.samples.length,2);
  assert.equal(await digest(a,fragment),await digest(b,b.fragments[0]));
  assert.notEqual(await digest(a,fragment),await digest(c,c.fragments[0]));
});
test('signed composition offsets come from trun bytes',()=>{
  const inspect=inspector();inspect(init());const result=inspect(media({composition:-1001}));
  assert.equal(result.fragments[0].from_tick,22999);assert.equal(result.fragments[0].through_tick,25001);
});
test('out-of-mdat and truncated samples cannot become append evidence',()=>{
  const inspect=inspector();inspect(init());
  assert.throws(()=>inspect(media({offsetDelta:-8})),/outside mdat/);
  assert.throws(()=>inspect(media({payload:Buffer.alloc(4)})),/truncated field/);
  const valid=media();assert.throws(()=>inspect(valid.subarray(0,valid.length-1)),/truncated/);
});
test('a failed initialization cannot poison retained track ownership',()=>{
  const inspect=inspector();inspect(init());
  const malformed=join(init(2),Buffer.from([0]));assert.throws(()=>inspect(malformed),/truncated/);
  assert.equal(inspect(media()).fragments[0].track.id,1);
  inspect(init(2));assert.throws(()=>inspect(media()),/missing init/);
});
test('unsafe and unbounded objects refuse before sample iteration',()=>{
  const inspect=inspector();assert.throws(()=>inspect(Buffer.alloc(16*1024*1024+1)),/payload bound/);
  const extended=join(word(1),Buffer.from('mdat'),Buffer.from('ffffffffffffffff','hex'));
  assert.throws(()=>inspect(extended),/unsafe integer/);
});

test('LAN HTTP SHA fallback preserves standard and binary payload digests',async()=>{
  for(const payload of [Buffer.alloc(0),Buffer.from('abc'),Buffer.alloc(100000,0xa5)]){
    const expected=Buffer.from(await webcrypto.subtle.digest('SHA-256',payload)).toString('hex');
    const actual=Buffer.from(await context.continuousSoftwareSHA256(new Uint8Array(payload))).toString('hex');
    assert.equal(actual,expected);
  }
});


test('LAN HTTP accelerated SHA preserves large concurrent binary digests',async()=>{
  const lane=vm.createContext({ArrayBuffer,Uint8Array,Uint32Array,DataView,
    WebAssembly,atob,AbortController,setTimeout,clearTimeout});
  vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-media.js','utf8'),lane);
  assert.equal(typeof await lane.continuousSHA256Accelerator(),'function');
  const payloads=[55,56,63,64,65,131073,1048577,4194317].map(length=>{
    const bytes=Buffer.alloc(length);for(let i=0;i<length;i++)bytes[i]=(i*31+(i>>>8))&255;return bytes;
  });
  await Promise.all(payloads.map(async bytes=>{
    const actual=await lane.continuousSoftwareSHA256(new Uint8Array(bytes),new AbortController().signal);
    assert.equal(Buffer.from(actual).toString('hex'),createHash('sha256').update(bytes).digest('hex'));
  }));
});

test('LAN checksum remains available when WebAssembly is denied',async()=>{
  const lane=vm.createContext({ArrayBuffer,Uint8Array,Uint32Array,DataView,atob,setTimeout,
    WebAssembly:{instantiate:async()=>{throw new Error('unavailable');}}});
  vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-media.js','utf8'),lane);
  assert.equal(await lane.continuousSHA256Accelerator(),null);
  const bytes=Buffer.alloc(131073,0xa5);
  const actual=await lane.continuousSoftwareSHA256(new Uint8Array(bytes));
  assert.equal(Buffer.from(actual).toString('hex'),createHash('sha256').update(bytes).digest('hex'));
});
