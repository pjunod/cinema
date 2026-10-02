"use strict";
const word=n=>{const b=Buffer.alloc(4);b.writeUInt32BE(n);return b;};
const join=(...parts)=>Buffer.concat(parts);
const box=(type,...parts)=>{const body=join(...parts);return join(word(body.length+8),Buffer.from(type),body);};
const full=(version,flags)=>word((version*0x1000000+flags)>>>0);
function init(id=1,{width=1280,height=720}={}){
  const tkhd=Buffer.alloc(84);tkhd.writeUInt32BE(id,12);tkhd.writeUInt32BE(width*65536,76);tkhd.writeUInt32BE(height*65536,80);
  const mdhd=Buffer.alloc(24);mdhd.writeUInt32BE(24000,12);
  const hdlr=Buffer.alloc(12);hdlr.write('vide',8);
  const avc=Buffer.alloc(78);avc.writeUInt16BE(width,24);avc.writeUInt16BE(height,26);
  const entry=box('avc1',avc,box('avcC',Buffer.from([1,100,0,50,255,225,0])));
  const stbl=box('stbl',box('stsd',full(0,0),word(1),entry));
  return box('moov',box('trak',box('tkhd',tkhd),box('mdia',box('mdhd',mdhd),box('hdlr',hdlr),box('minf',stbl))));
}
function media({payload=Buffer.from([1,2,3,4,5,6,7,8]),offsetDelta=0,composition=0,sequence=1,start=24000}={}){
  const tfhd=box('tfhd',full(0,0x020000),word(1));
  const tfdt=box('tfdt',full(0,0),word(start));
  const signed=n=>{const b=Buffer.alloc(4);b.writeInt32BE(n);return b;};
  const run=offset=>box('trun',full(1,0xb01),word(2),signed(offset),word(1001),word(4),signed(composition),word(1001),word(4),signed(composition));
  const header=offset=>box('moof',box('mfhd',full(0,0),word(sequence)),box('traf',tfhd,tfdt,run(offset)));
  const moof=header(0);return join(header(moof.length+8+offsetDelta),box('mdat',payload));
}

module.exports={init,media,word,join,box,full};
