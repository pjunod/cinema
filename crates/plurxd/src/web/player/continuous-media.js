"use strict";
// Exact sample provenance for the narrow, init-verified AVC/AAC family.
// Callers retain immutable results; neither this parser nor its hashes mutate
// container bytes, playback state, or SourceBuffer prototypes.
function continuousMediaInspector(){
  const tracks=new Map();
  const fail=message=>{throw new Error(`Continuous media: ${message}`);};
  const integer=value=>Number.isSafeInteger(value)&&value>=0;
  function inspect(input){
    const bytes=input instanceof ArrayBuffer?new Uint8Array(input):
      ArrayBuffer.isView(input)?new Uint8Array(input.buffer,input.byteOffset,input.byteLength):null;
    if(!bytes||!bytes.length||bytes.length>16*1024*1024) fail('payload bound');
    const view=new DataView(bytes.buffer,bytes.byteOffset,bytes.byteLength);
    let boxCount=0,sampleCount=0;
    function need(at,length,end=bytes.length){
      if(!integer(at)||!integer(length)||at+length>end) fail('truncated field');
    }
    const u32=(at,end)=>{need(at,4,end);return view.getUint32(at);};
    const i32=(at,end)=>{need(at,4,end);return view.getInt32(at);};
    const u64=(at,end)=>{need(at,8,end);const n=Number(view.getBigUint64(at));if(!integer(n))fail('unsafe integer');return n;};
    function boxes(from,end){
      const rows=[];
      while(from<end){
        need(from,8,end);
        if(++boxCount>4096) fail('box count bound');
        let size=u32(from,end),header=8;
        if(size===1){size=u64(from+8,end);header=16;}
        if(size===0) size=end-from;
        if(size<header) fail('invalid box size');
        need(from,size,end);
        const type=String.fromCharCode(...bytes.subarray(from+4,from+8));
        rows.push({type,offset:from,start:from+header,end:from+size});
        from+=size;
      }
      return rows;
    }
    const of=(box,type)=>boxes(box.start,box.end).filter(row=>row.type===type);
    const one=(box,type)=>{const rows=of(box,type);if(rows.length!==1)fail(`expected one ${type}`);return rows[0];};
    const full=(box,allowedVersions)=>{
      need(box.start,4,box.end);const version=bytes[box.start];
      if(!allowedVersions.includes(version))fail('box version');
      return {version,flags:u32(box.start,box.end)&0xffffff};
    };
    const top=boxes(0,bytes.length),initializations=[],fragments=[];
    const nextTracks=top.some(row=>row.type==='moov')?new Map():new Map(tracks);
    if(top.filter(row=>row.type==='moov').length>1)fail('multiple initialization objects');
    for(const moov of top.filter(row=>row.type==='moov')){
      const traks=of(moov,'trak');
      if(traks.length!==1)fail('family objects must carry one track');
      const defaults=new Map();
      for(const mvex of of(moov,'mvex')) for(const trex of of(mvex,'trex')){
        if(full(trex,[0]).flags!==0)fail('trex flags');need(trex.start,24,trex.end);
        const id=u32(trex.start+4,trex.end);
        if(defaults.has(id))fail('duplicate trex');
        defaults.set(id,{duration:u32(trex.start+12,trex.end),size:u32(trex.start+16,trex.end)});
      }
      for(const trak of traks){
        const tkhd=one(trak,'tkhd'),tv=full(tkhd,[0,1]).version;
        need(tkhd.start,tv===1?96:84,tkhd.end);
        const id=u32(tkhd.start+(tv===1?20:12),tkhd.end);
        const mdia=one(trak,'mdia'),mdhd=one(mdia,'mdhd'),mv=full(mdhd,[0,1]).version;
        const timescale=u32(mdhd.start+(mv===1?20:12),mdhd.end);
        if(!id||timescale<1||timescale>1000000)fail('track clock');
        const handler=one(mdia,'hdlr');need(handler.start,12,handler.end);
        const type=String.fromCharCode(...bytes.subarray(handler.start+8,handler.start+12));
        if(!['vide','soun'].includes(type))fail('unsupported track handler');
        const stsd=one(one(one(mdia,'minf'),'stbl'),'stsd');
        if(full(stsd,[0]).flags!==0||u32(stsd.start+4,stsd.end)!==1)fail('sample description');
        const entries=boxes(stsd.start+8,stsd.end);
        if(entries.length!==1)fail('multiple sample entries');
        const entry=entries[0];let width=0,height=0,channels=0,configuration=null;
        if(type==='vide'){
          if(entry.type!=='avc1')fail('video codec');need(entry.start,78,entry.end);
          width=view.getUint16(entry.start+24);height=view.getUint16(entry.start+26);
          if(!width||!height||width>8192||height>8192)fail('video raster');
          const configs=boxes(entry.start+78,entry.end).filter(row=>row.type==='avcC');
          if(configs.length!==1)fail('AVC configuration');
          const config=configs[0];need(config.start,7,config.end);
          if(bytes[config.start]!==1)fail('AVC configuration version');
          configuration=bytes.slice(config.start,config.end);
          if(width!==u32(tkhd.end-8,tkhd.end)/65536||height!==u32(tkhd.end-4,tkhd.end)/65536)fail('inconsistent raster');
        }else{
          if(entry.type!=='mp4a')fail('audio codec');need(entry.start,28,entry.end);
          if(view.getUint16(entry.start+8)!==0)fail('audio sample entry version');
          channels=view.getUint16(entry.start+16);
          if(!channels||channels>8||timescale!==48000||u32(entry.start+24,entry.end)/65536!==48000)fail('audio format');
          const configs=boxes(entry.start+28,entry.end).filter(row=>row.type==='esds');
          if(configs.length!==1)fail('AAC configuration');configuration=bytes.slice(configs[0].start,configs[0].end);
        }
        const track={id,type,timescale,width,height,channels,configuration,
          defaults:defaults.get(id)||{duration:0,size:0}};
        nextTracks.set(id,track);initializations.push(track);
      }
    }
    const mdats=top.filter(row=>row.type==='mdat');
    const occupied=[];
    for(const moof of top.filter(row=>row.type==='moof')){
      const trafs=of(moof,'traf');if(trafs.length!==1)fail('multiple fragment tracks');
      for(const traf of trafs){
        const tfhd=one(traf,'tfhd'),header=full(tfhd,[0]);
        if(header.flags&~0x02003b)fail('unsupported tfhd flags');
        const track=nextTracks.get(u32(tfhd.start+4,tfhd.end));if(!track)fail('missing init');
        let cursor=tfhd.start+8,base=moof.offset;
        if(header.flags&1){base=u64(cursor,tfhd.end);cursor+=8;}
        else if(!(header.flags&0x020000))fail('ambiguous fragment base');
        if(header.flags&2){if(u32(cursor,tfhd.end)!==1)fail('sample description index');cursor+=4;}
        let duration=track.defaults.duration,size=track.defaults.size;
        if(header.flags&8){duration=u32(cursor,tfhd.end);cursor+=4;}
        if(header.flags&16){size=u32(cursor,tfhd.end);cursor+=4;}
        if(header.flags&32){need(cursor,4,tfhd.end);cursor+=4;}
        if(cursor!==tfhd.end)fail('tfhd tail');
        const tfdt=one(traf,'tfdt'),clock=full(tfdt,[0,1]);if(clock.flags!==0)fail('tfdt flags');
        let dts=clock.version===1?u64(tfdt.start+4,tfdt.end):u32(tfdt.start+4,tfdt.end);
        if(tfdt.end-tfdt.start!==(clock.version===1?12:8))fail('tfdt tail');
        let dataEnd=null;const samples=[];
        const runs=of(traf,'trun');if(!runs.length)fail('no samples');
        for(const trun of runs){
          const run=full(trun,[0,1]);if(run.flags&~0x000f05)fail('trun flags');
          const count=u32(trun.start+4,trun.end);sampleCount+=count;
          if(!count||sampleCount>65536)fail('sample count bound');
          cursor=trun.start+8;
          let data=dataEnd;
          if(run.flags&1){data=base+i32(cursor,trun.end);cursor+=4;}
          if(data==null||!integer(data))fail('sample data offset');
          if(run.flags&4){need(cursor,4,trun.end);cursor+=4;}
          for(let i=0;i<count;i++){
            let dt=duration,length=size,composition=0;
            if(run.flags&0x100){dt=u32(cursor,trun.end);cursor+=4;}
            if(run.flags&0x200){length=u32(cursor,trun.end);cursor+=4;}
            if(run.flags&0x400){need(cursor,4,trun.end);cursor+=4;}
            if(run.flags&0x800){composition=run.version===1?i32(cursor,trun.end):u32(cursor,trun.end);cursor+=4;}
            if(!dt||!length||!integer(dts+composition)||!integer(dts+composition+dt)||!integer(dts+dt))fail('sample clock/size');
            need(data,length);
            if(!mdats.some(mdat=>mdat.start<=data&&data+length<=mdat.end))fail('sample outside mdat');
            occupied.push([data,data+length]);
            samples.push({pts:dts+composition,duration:dt,offset:data,size:length});
            data+=length;dts+=dt;
          }
          if(cursor!==trun.end)fail('trun tail');dataEnd=data;
        }
        let fromTick=Infinity,throughTick=0;
        for(const sample of samples){fromTick=Math.min(fromTick,sample.pts);throughTick=Math.max(throughTick,sample.pts+sample.duration);}
        fragments.push({track,samples,from_tick:fromTick,through_tick:throughTick});
      }
    }
    occupied.sort((a,b)=>a[0]-b[0]);
    for(let i=1;i<occupied.length;i++)if(occupied[i][0]<occupied[i-1][1])fail('overlapping sample payload');
    // A failed parse cannot poison the next fragment's init context.
    for(const [id,track] of nextTracks)tracks.set(id,track);
    return {bytes,initializations,fragments};
  }
  return inspect;
}
async function continuousMediaDigest(bytes){
  const hash=await crypto.subtle.digest('SHA-256',bytes);
  return Array.from(new Uint8Array(hash),byte=>byte.toString(16).padStart(2,'0')).join('');
}
async function continuousSampleDigest(inspection,fragment){
  // Hash lengths as well as payloads, so a different sample partition cannot
  // inherit a match even if its concatenated elementary bytes are identical.
  const length=fragment.samples.reduce((sum,row)=>sum+4+row.size,0);
  if(length>17*1024*1024)throw new Error('Continuous sample hash bound');
  const payload=new Uint8Array(length),view=new DataView(payload.buffer);let cursor=0;
  for(const sample of fragment.samples){
    view.setUint32(cursor,sample.size);cursor+=4;
    payload.set(inspection.bytes.subarray(sample.offset,sample.offset+sample.size),cursor);cursor+=sample.size;
  }
  return continuousMediaDigest(payload);
}
async function continuousSampleFacts(inspection){
  if(!inspection.fragments.length)throw new Error('Continuous media has no samples');
  const track=inspection.fragments[0].track,samples=[];
  for(const fragment of inspection.fragments){
    if(fragment.track.id!==track.id||fragment.track.timescale!==track.timescale
      ||fragment.track.type!==track.type)throw new Error('Continuous media mixed tracks');
    samples.push(...fragment.samples);
  }
  const presentation=samples.slice().sort((a,b)=>a.pts-b.pts);let through=presentation[0].pts;
  for(const sample of presentation){
    if(sample.pts!==through)throw new Error('Continuous media sample timeline gap or overlap');
    through+=sample.duration;
  }
  const fingerprint=await continuousSampleDigest(inspection,{samples});
  return {type:track.type,timescale:track.timescale,from_tick:presentation[0].pts,
    through_tick:through,width:track.width,height:track.height,channels:track.channels,
    sample_count:samples.length,fingerprint};
}
