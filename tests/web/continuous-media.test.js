"use strict";
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm');
const {webcrypto}=require('node:crypto');
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
