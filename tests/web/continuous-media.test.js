"use strict";
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm');
const {webcrypto}=require('node:crypto');
const context=vm.createContext({ArrayBuffer,Uint8Array,DataView,crypto:webcrypto});
vm.runInContext(fs.readFileSync('crates/plurxd/src/web/player/continuous-media.js','utf8'),context);
const {continuousMediaInspector:inspector,continuousSampleDigest:digest}=context;
const word=n=>{const b=Buffer.alloc(4);b.writeUInt32BE(n);return b;};
const join=(...parts)=>Buffer.concat(parts);
const box=(type,...parts)=>{const body=join(...parts);return join(word(body.length+8),Buffer.from(type),body);};
const full=(version,flags)=>word((version*0x1000000+flags)>>>0);
function init(id=1){
  const tkhd=Buffer.alloc(84);tkhd.writeUInt32BE(id,12);tkhd.writeUInt32BE(1280*65536,76);tkhd.writeUInt32BE(720*65536,80);
  const mdhd=Buffer.alloc(24);mdhd.writeUInt32BE(24000,12);
  const hdlr=Buffer.alloc(12);hdlr.write('vide',8);
  const avc=Buffer.alloc(78);avc.writeUInt16BE(1280,24);avc.writeUInt16BE(720,26);
  const entry=box('avc1',avc,box('avcC',Buffer.from([1,100,0,50,255,225,0])));
  const stbl=box('stbl',box('stsd',full(0,0),word(1),entry));
  return box('moov',box('trak',box('tkhd',tkhd),box('mdia',box('mdhd',mdhd),box('hdlr',hdlr),box('minf',stbl))));
}
function media({payload=Buffer.from([1,2,3,4,5,6,7,8]),offsetDelta=0,composition=0,sequence=1}={}){
  const tfhd=box('tfhd',full(0,0x020000),word(1));
  const tfdt=box('tfdt',full(0,0),word(24000));
  const signed=n=>{const b=Buffer.alloc(4);b.writeInt32BE(n);return b;};
  const run=offset=>box('trun',full(1,0xb01),word(2),signed(offset),word(1001),word(4),signed(composition),word(1001),word(4),signed(composition));
  const header=offset=>box('moof',box('mfhd',full(0,0),word(sequence)),box('traf',tfhd,tfdt,run(offset)));
  const moof=header(0);return join(header(moof.length+8+offsetDelta),box('mdat',payload));
}
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
