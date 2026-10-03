"use strict";
// These are owned-resource allowances, not a claim about opaque browser decoder
// workspace or GPU caches. Actual close/reset precedes returning pixel credits.
const SHARED_ARTWORK=(()=>{
  const ASSET=15*1024*1024,PIXELS=16*1024*1024;
  const proofs=new WeakMap(),canvasOwners=new WeakMap(),contexts=new Map(),latest=new Map(),jobs=new Set(),unresolved=new Set(),observed=new Set(),metadataJobs=new Set();
  let observer=null,rescan=false;
  let serial=0;
  function budget(limit,operations){
    let bytes=0,active=0;
    return {take(count){if(!Number.isSafeInteger(count)||count<=0||count>limit||active>=operations||bytes>limit-count)throw Object.assign(new Error("Shared artwork capacity is busy"),{sharedArtCapacity:true});
      bytes+=count;active++;let held=true;return {release(){if(held){held=false;bytes-=count;active--;}}};},snapshot(){return {bytes,active};}};
  }
  // Buffer plus Blob copy; four is an operation ceiling, not a queued promise.
  const compressed=budget(64*1024*1024,4),pixels=budget(64*1024*1024,Infinity);
  function current(c){return !!c&&sharedCatalogueCurrent(c);}
  function equal(a,b){return a.generation===b.generation&&a.auth===b.auth&&a.token===b.token&&a.origin===b.origin&&a.route===b.route;}
  function origin(c){const url=new URL(c.origin,location.href);if(!["http:","https:"].includes(url.protocol)||url.username||url.password||url.search||url.hash||url.pathname!=="/api/v1")throw new Error("Invalid B origin");return url.origin;}
  function metadataPath(path){
    if(typeof path!=="string"||!path.startsWith("/shared/"))throw new Error("Invalid Shared metadata route");
    const url=new URL(path,"https://metadata.invalid");if(url.pathname!==path.split("?")[0]||url.hash)throw new Error("Invalid Shared metadata route");
    const id="(?:0|[1-9][0-9]{0,18})",uuid="[0-9a-f-]{36}";
    if(!new RegExp(`^/shared/(?:libraries|continue-watching|imports/${uuid}/(?:libraries|continue-watching|items/${id}(?:/children)?|libraries/${id}/items))$`).test(url.pathname))throw new Error("Invalid Shared metadata route");
    if(url.pathname.startsWith("/shared/imports/")){const parts=url.pathname.split("/");sharedCatalogueUuid(parts[3]);if(parts[4]==="items"||parts[4]==="libraries"&&parts.length>5)sharedCatalogueId(parts[5]);}
    const allowed=url.pathname.includes("/items")?["q","cursor","limit"]:url.pathname.endsWith("continue-watching")?["limit"]:[];
    for(const key of url.searchParams.keys())if(!allowed.includes(key)||url.searchParams.getAll(key).length!==1)throw new Error("Invalid Shared metadata query");
    const limit=url.searchParams.get("limit"),q=url.searchParams.get("q"),cursor=url.searchParams.get("cursor");
    if(limit!==null&&!/^(?:100|200)$/.test(limit)||q!==null&&(q.length>512||/[\u0000-\u001f\u007f]/.test(q))||cursor!==null&&(!cursor||cursor.length>4096))throw new Error("Invalid Shared metadata query");
    return path;
  }
  async function metadata(path,capture){
    metadataPath(path);if(!current(capture))throw new Error("stale shared catalogue");
    const operation={capture,importId:path.startsWith("/shared/imports/")?path.split("/")[3]:null,controller:new AbortController(),reader:null};metadataJobs.add(operation);
    try{
    const expected=new URL(origin(capture)+"/api/v1"+path).href,response=await api(path,{raw:true,signal:operation.controller.signal});
    if(operation.controller.signal.aborted||!current(capture)||response.redirected||response.url!==expected){await response.body?.cancel();throw new Error("stale or redirected Shared metadata");}
    const reader=response.body?.getReader();if(!reader)throw new Error("Shared catalogue body unavailable");operation.reader=reader;
    const chunks=[];let length=0;
    try{for(;;){const part=await reader.read();if(part.done)break;length+=part.value.byteLength;
      if(length>4*1024*1024||operation.controller.signal.aborted||!current(capture))throw new Error("Shared catalogue response unavailable");chunks.push(part.value);}}
    catch(error){await reader.cancel().catch(()=>{});throw error;}finally{operation.reader=null;reader.releaseLock();}
    if(operation.controller.signal.aborted||!current(capture))throw new Error("stale shared catalogue");
    const bytes=new Uint8Array(length);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.byteLength;}
    const reply=JSON.parse(new TextDecoder("utf-8",{fatal:true}).decode(bytes)),proof=Object.freeze({...capture});
    const record=item=>{if(item&&typeof item==="object"&&item.source==="shared")proofs.set(item,proof);};
    record(reply.item);if(Array.isArray(reply.items)){for(const row of reply.items){record(row);record(row?.item);}}
    return reply;
    }finally{metadataJobs.delete(operation);}
  }
  function rows(item,ref){
    const values=item.art??[];if(!Array.isArray(values)||values.length>8)throw new Error("Invalid Shared artwork");
    const seen=new Set(),prefix=`/api/v1/shared/imports/${ref.import_id}/art/`;
    const copied=values.map(row=>{if(!row||!["poster","backdrop"].includes(row.kind)||!["original","w300","w500","w780"].includes(row.variant)||typeof row.url!=="string"||!row.url.startsWith(prefix)||!/^[A-Za-z0-9_-]{272}$/.test(row.url.slice(prefix.length)))throw new Error("Invalid Shared artwork");
      const key=row.kind+"|"+row.variant;if(seen.has(key))throw new Error("Duplicate Shared artwork");seen.add(key);return Object.freeze({kind:row.kind,variant:row.variant,url:row.url});});
    for(const [key,kind,variant]of [["poster_url","poster","w300"],["backdrop_url","backdrop","w780"]]){
      const alias=item[key];if(alias!==null&&alias!==undefined&&!copied.some(row=>row.kind===kind&&row.variant===variant&&row.url===alias))throw new Error("Invalid Shared artwork alias");}
    return copied;
  }
  function live(ctx){return current(ctx.capture)&&contexts.get(ctx.id)===ctx&&latest.get(ctx.key)===ctx;}
  function retireJob(job){
    const owns=canvasOwners.get(job.canvas)===job;
    job.controller.abort();if(job.reader){void job.reader.cancel().catch(()=>{});job.reader=null;}else if(job.response?.body&&!job.response.body.locked)void job.response.body.cancel().catch(()=>{});
    if(job.bitmap){try{job.bitmap.close();if(job.bitmap.width!==0||job.bitmap.height!==0)throw new Error("Bitmap retirement unconfirmed");job.bitmapLease?.release();}catch(error){unresolved.add({resource:job.bitmap,lease:job.bitmapLease});}job.bitmap=null;job.bitmapLease=null;}
    if(job.canvasLease){try{if(!owns)throw new Error("Canvas ownership changed");job.canvas.width=0;job.canvas.height=0;if(job.canvas.width!==0||job.canvas.height!==0)throw new Error("Canvas retirement unconfirmed");job.canvasLease.release();}catch(error){unresolved.add({resource:job.canvas,lease:job.canvasLease});}job.canvasLease=null;}
    if(owns){delete job.canvas.dataset.sharedArtReady;canvasOwners.delete(job.canvas);}jobs.delete(job);
  }
  function retire(importId=null){for(const operation of metadataJobs)if(importId===null||operation.importId===importId){operation.controller.abort();void operation.reader?.cancel().catch(()=>{});}if(importId===null){observer?.disconnect();observed.clear();}
    for(const job of [...jobs])if(importId===null||job.ctx.reference.import_id===importId)retireJob(job);
    for(const [id,ctx]of contexts)if(importId===null||ctx.reference.import_id===importId){contexts.delete(id);if(latest.get(ctx.key)===ctx)latest.delete(ctx.key);}}
  function markup(item,expected,capture,backdrop=false){
    if(!item||item.source!=="shared")throw new Error("Invalid Shared item");const ref=sharedCatalogueReference(item.reference),bound=sharedCatalogueGroupKey(expected);
    if(sharedCatalogueGroupKey(ref)!==bound||expected.library_id&&ref.library_id!==expected.library_id||expected.item_id&&ref.item_id!==expected.item_id)throw new Error("Shared artwork source changed");
    const values=rows(item,ref),alias=backdrop?item.backdrop_url:item.poster_url,row=values.find(v=>v.url===alias&&v.kind===(backdrop?"backdrop":"poster")&&v.variant===(backdrop?"w780":"w300"));
    if(!row)return "";const proof=proofs.get(item);if(!proof||!capture||!equal(proof,capture)||!current(capture))throw new Error("Artwork requires authenticated B metadata");
    origin(capture);const key=JSON.stringify([ref.import_id,ref.server_id,ref.catalogue_epoch,ref.library_id,ref.item_id,backdrop]);
    let ctx=latest.get(key);
    if(!ctx||ctx.row.url!==row.url||!equal(ctx.capture,capture)){
      if(ctx){for(const job of [...jobs])if(job.ctx===ctx)retireJob(job);contexts.delete(ctx.id);}
      if(contexts.size>=8192)throw new Error("Shared artwork presentation capacity reached");
      ctx=Object.freeze({id:"sa-"+(++serial),key,reference:ref,row,capture:Object.freeze({...proof}),maximum:backdrop?780:300});contexts.set(ctx.id,ctx);latest.set(key,ctx);
    }
    return `<canvas data-shared-art="${ctx.id}" width="0" height="0" aria-hidden="true" style="${backdrop?"width:100%;max-width:780px;max-height:300px;object-fit:contain":"width:72px;height:108px;object-fit:cover"}"></canvas>`;
  }
  function dimensions(bytes,mime){
    const view=new DataView(bytes.buffer,bytes.byteOffset,bytes.byteLength);let width=0,height=0;
    const valid=(w,h)=>{if(!Number.isInteger(w)||!Number.isInteger(h)||w<1||h<1||w>8192||h>8192||w*h>PIXELS)throw new Error("Shared artwork dimensions unavailable");if(width&&(width!==w||height!==h))throw new Error("Shared artwork dimensions changed");width=w;height=h;};
    const tag=offset=>String.fromCharCode(...bytes.subarray(offset,offset+4));
    if(mime==="image/png"){
      if(bytes.length<33||![137,80,78,71,13,10,26,10].every((v,i)=>bytes[i]===v)||view.getUint32(8)!==13||tag(12)!=="IHDR")throw new Error("Invalid PNG artwork");
      valid(view.getUint32(16),view.getUint32(20));let count=0,ended=false;
      for(let offset=8;offset<bytes.length;){if(++count>65536||offset+12>bytes.length)throw new Error("Invalid PNG artwork");const size=view.getUint32(offset),kind=tag(offset+4);if(size>bytes.length-offset-12||kind==="acTL"||kind==="IHDR"&&offset!==8)throw new Error("Animated or invalid PNG artwork");offset+=size+12;if(kind==="IEND"){ended=true;if(size!==0||offset!==bytes.length)throw new Error("Invalid PNG artwork");break;}}if(!ended)throw new Error("Incomplete PNG artwork");
    }else if(mime==="image/jpeg"){
      if(bytes.length<4||bytes[0]!==255||bytes[1]!==216)throw new Error("Invalid JPEG artwork");let count=0;
      for(let offset=2;offset<bytes.length;){if(++count>65536||bytes[offset++]!==255)throw new Error("Invalid JPEG artwork");while(bytes[offset]===255)offset++;const marker=bytes[offset++];if(marker===218||marker===217)break;if(marker===1||marker>=208&&marker<=215)continue;
        if(offset+2>bytes.length)throw new Error("Invalid JPEG artwork");const size=view.getUint16(offset);if(size<2||size>bytes.length-offset)throw new Error("Invalid JPEG artwork");
        if(marker>=192&&marker<=207&&![196,200,204].includes(marker)){if(size<8)throw new Error("Invalid JPEG artwork");valid(view.getUint16(offset+5),view.getUint16(offset+3));}offset+=size;
      }
    }else if(mime==="image/webp"){
      if(bytes.length<20||tag(0)!=="RIFF"||tag(8)!=="WEBP"||view.getUint32(4,true)!==bytes.length-8)throw new Error("Invalid WebP artwork");let count=0,frames=0;
      for(let offset=12;offset<bytes.length;){if(++count>65536||offset+8>bytes.length)throw new Error("Invalid WebP artwork");const kind=tag(offset),size=view.getUint32(offset+4,true),p=offset+8;if(size>bytes.length-p)throw new Error("Invalid WebP artwork");
        if(kind==="ANIM"||kind==="ANMF")throw new Error("Animated WebP artwork");
        if(kind==="VP8X"){if(size!==10||bytes[p]&2)throw new Error("Animated WebP artwork");valid(1+bytes[p+4]+bytes[p+5]*256+bytes[p+6]*65536,1+bytes[p+7]+bytes[p+8]*256+bytes[p+9]*65536);}
        if(kind==="VP8 "){if(++frames!==1||size<10||bytes[p]&1||bytes[p+3]!==157||bytes[p+4]!==1||bytes[p+5]!==42)throw new Error("Invalid WebP artwork");valid(view.getUint16(p+6,true)&16383,view.getUint16(p+8,true)&16383);}
        if(kind==="VP8L"){if(++frames!==1||size<5||bytes[p]!==47)throw new Error("Invalid WebP artwork");const bits=view.getUint32(p+1,true);if(bits>>>29)throw new Error("Invalid WebP artwork");valid(1+(bits&16383),1+((bits>>>14)&16383));}
        offset=p+size+(size&1);if(offset>bytes.length)throw new Error("Invalid WebP artwork");
      }if(frames!==1)throw new Error("Invalid WebP artwork");
    }else throw new Error("Invalid Shared artwork MIME");if(!width)throw new Error("Shared artwork dimensions unavailable");return {width,height};
  }
  async function load(job){
    let credit=null;
    try{
      if(!live(job.ctx)||typeof createImageBitmap!=="function")throw new Error("Shared artwork decoder unavailable");credit=compressed.take(ASSET*2);job.canvas.dataset.sharedArtAttempted="true";
      const response=await fetch(origin(job.ctx.capture)+job.ctx.row.url,{headers:{authorization:"Bearer "+job.ctx.capture.token},signal:job.controller.signal,credentials:"omit",redirect:"error",cache:"no-store"});job.response=response;
      if(!live(job.ctx)||response.redirected||response.url!==origin(job.ctx.capture)+job.ctx.row.url)throw new Error("stale Shared artwork");
      if(response.status===401||response.status===403){await response.body?.cancel();if(live(job.ctx)){if(response.status===401)clearLocalSession(job.ctx.capture.auth,"Your session ended on the server.");else retire(job.ctx.reference.import_id);}throw new Error("Shared artwork authorization refused");}
      const mime=(response.headers.get("content-type")||"").split(";")[0].trim().toLowerCase(),declared=response.headers.get("content-length");
      if(response.status!==200||!["image/png","image/jpeg","image/webp"].includes(mime)||declared!==null&&(!/^(?:0|[1-9][0-9]*)$/.test(declared)||Number(declared)>ASSET)){await response.body?.cancel();throw new Error("Shared artwork response unavailable");}
      const reader=response.body?.getReader();if(!reader)throw new Error("Shared artwork body unavailable");job.reader=reader;const buffer=new Uint8Array(ASSET);let length=0;
      try{for(;;){const part=await reader.read();if(part.done)break;if(!live(job.ctx)||job.controller.signal.aborted||part.value.byteLength>ASSET-length)throw new Error("Shared artwork response unavailable");buffer.set(part.value,length);length+=part.value.byteLength;}}
      catch(error){await reader.cancel().catch(()=>{});throw error;}finally{job.reader=null;reader.releaseLock();}
      if(!length||!live(job.ctx)||job.controller.signal.aborted)throw new Error("stale Shared artwork");
      const bounds=dimensions(buffer.subarray(0,length),mime),scale=Math.min(1,job.ctx.maximum/Math.max(bounds.width,bounds.height));
      // Eight bytes per output pixel is a logical allowance. Browser backing
      // formats/decoder workspace are opaque and need separate runtime evidence.
      job.bitmapLease=pixels.take(job.ctx.maximum*job.ctx.maximum*8);
      job.bitmap=await createImageBitmap(new Blob([buffer.subarray(0,length)],{type:mime}),{resizeWidth:Math.max(1,Math.floor(bounds.width*scale)),resizeHeight:Math.max(1,Math.floor(bounds.height*scale)),resizeQuality:"high"});
      if(!live(job.ctx)||job.controller.signal.aborted||job.bitmap.width<1||job.bitmap.height<1||job.bitmap.width>job.ctx.maximum||job.bitmap.height>job.ctx.maximum)throw new Error("Shared artwork decoder changed dimensions");
      job.canvasLease=pixels.take(job.bitmap.width*job.bitmap.height*8);job.canvas.width=job.bitmap.width;job.canvas.height=job.bitmap.height;
      const draw=job.canvas.getContext("2d",{colorSpace:"srgb"});if(!draw)throw new Error("Shared artwork canvas unavailable");draw.drawImage(job.bitmap,0,0);
      job.bitmap.close();if(job.bitmap.width!==0||job.bitmap.height!==0)throw new Error("Bitmap retirement unconfirmed");job.bitmap=null;job.bitmapLease.release();job.bitmapLease=null;
      if(!live(job.ctx)||job.controller.signal.aborted)throw new Error("stale Shared artwork");job.canvas.dataset.sharedArtReady="true";
    }catch(error){const owns=canvasOwners.get(job.canvas)===job,cancelled=job.controller.signal.aborted;retireJob(job);if(owns){if(cancelled||!credit&&error.sharedArtCapacity)delete job.canvas.dataset.sharedArtAttempted;else job.canvas.dataset.sharedArtAttempted="true";}}
    finally{credit?.release();if(!job.bitmap&&job.bitmapLease){job.bitmapLease.release();job.bitmapLease=null;}if(credit)schedule();}
  }
  function visible(canvas){if(!canvas.isConnected)return false;const r=canvas.getBoundingClientRect();return r.bottom>0&&r.top<innerHeight&&r.right>0&&r.left<innerWidth;}
  function schedule(){if(rescan)return;rescan=true;Promise.resolve().then(()=>{rescan=false;hydrate(document.getElementById("shared-catalogue"));});}
  function hydrate(root){
    if(!observer&&typeof IntersectionObserver!=="undefined")observer=new IntersectionObserver(entries=>{for(const entry of entries){
      const canvas=/** @type {HTMLCanvasElement} */(entry.target);
      if(entry.isIntersecting)hydrate(canvas.parentElement);else if(!visible(canvas)){for(const job of [...jobs])if(job.canvas===canvas)retireJob(job);delete canvas.dataset.sharedArtAttempted;}
    }});
    for(const canvas of root?.querySelectorAll?.("canvas[data-shared-art]")||[]){
      const ctx=contexts.get(canvas.dataset.sharedArt);if(!ctx||!live(ctx))continue;
      if(!observed.has(canvas)){observed.add(canvas);observer?.observe(canvas);}
      if(!visible(canvas)||canvas.dataset.sharedArtAttempted||[...jobs].some(job=>job.canvas===canvas))continue;
      const job={canvas,ctx,controller:new AbortController(),response:null,reader:null,bitmap:null,bitmapLease:null,canvasLease:null};canvasOwners.set(canvas,job);jobs.add(job);void load(job);
    }
  }
  return Object.freeze({metadata,markup,hydrate,retire,snapshot(){return {compressed:compressed.snapshot(),pixels:pixels.snapshot(),jobs:jobs.size,contexts:contexts.size,unresolved:unresolved.size,metadata:metadataJobs.size};}});
})();
function sharedArtworkRetire(){SHARED_ARTWORK.retire();}
