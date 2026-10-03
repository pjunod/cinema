"use strict";
// Decisions describe delivery; they never acquire a B session or start media.
const SHARED_DECISION=(()=>{
  const MAX=4*1024*1024,REQUEST_MAX=128*1024;
  let contexts=new WeakMap();const jobs=new Set();
  const fail=()=>{throw new Error("Shared decision unavailable");};
  function current(c){return c.auth===AUTH_GENERATION&&c.token===TOKEN&&c.origin===API&&c.generation===PAGE_RENDER_GENERATION&&c.route===location.hash;}
  function capture(){const c=Object.freeze({auth:AUTH_GENERATION,token:TOKEN,origin:API,generation:PAGE_RENDER_GENERATION,route:location.hash});if(!c.token||!current(c))fail();origin(c);return c;}
  function origin(c){const u=new URL(c.origin,location.href);if(!["http:","https:"].includes(u.protocol)||u.username||u.password||u.search||u.hash||u.pathname!=="/api/v1")fail();return u.origin;}
  function retire(){contexts=new WeakMap();for(const j of jobs){j.controller.abort();j.reader?.cancel().catch(()=>{});}}
  function copy(value){if(Array.isArray(value))return Object.freeze(value.map(copy));if(value&&typeof value==="object"){const o=Object.create(null);for(const [k,v]of Object.entries(value))o[k]=copy(v);return Object.freeze(o);}return value;}
  // Engine fields stay numbers where exact, while future/identity integers that
  // cannot be represented by Number remain lossless BigInts.
  function engine(value){if(typeof value==="bigint"&&value>=BigInt(Number.MIN_SAFE_INTEGER)&&value<=BigInt(Number.MAX_SAFE_INTEGER))return Number(value);if(Array.isArray(value))return value.map(engine);if(value&&typeof value==="object"){const o=Object.create(null);for(const [k,v]of Object.entries(value))o[k]=engine(v);return o;}return value;}
  async function read(path,c,body=null,signal=null){
    if(!current(c)||signal?.aborted)fail();const controller=new AbortController(),job={controller,reader:null};jobs.add(job);
    const abort=()=>controller.abort();signal?.addEventListener("abort",abort,{once:true});const timer=setTimeout(abort,30000);
    try{
      const expected=origin(c)+path,response=await fetch(expected,{method:body===null?"GET":"POST",headers:{authorization:"Bearer "+c.token,...(body===null?{}:{"content-type":"application/json"})},...(body===null?{}:{body}),credentials:"omit",cache:"no-store",redirect:"error",signal:controller.signal});
      if(!current(c)||controller.signal.aborted||response.redirected||response.url!==expected){await response.body?.cancel();fail();}
      if(response.status!==200){await response.body?.cancel();if(response.status===401||response.status===403)retire();fail();}
      const declared=response.headers.get("content-length");if(declared!==null&&(!/^(0|[1-9][0-9]*)$/.test(declared)||BigInt(declared)>BigInt(MAX))){await response.body?.cancel();fail();}
      const reader=response.body?.getReader();if(!reader)fail();job.reader=reader;const chunks=[];let length=0;
      try{for(;;){const p=await reader.read();if(!current(c)||controller.signal.aborted)fail();if(p.done)break;if(!(p.value instanceof Uint8Array))fail();length+=p.value.byteLength;if(length>MAX)fail();chunks.push(p.value);}}
      catch(e){await reader.cancel().catch(()=>{});throw e;}finally{job.reader=null;reader.releaseLock();}
      if(!current(c)||controller.signal.aborted)fail();const bytes=new Uint8Array(length);let offset=0;for(const p of chunks){bytes.set(p,offset);offset+=p.byteLength;}
      return sharingJSON(new TextDecoder("utf-8",{fatal:true}).decode(bytes));
    }finally{clearTimeout(timer);signal?.removeEventListener("abort",abort);jobs.delete(job);}
  }
  async function details(reference,signal=null){
    const ref=sharedCatalogueReference(reference),c=capture();
    const detail=await read(`/api/v1/shared/imports/${ref.import_id}/items/${ref.item_id}`,c,null,signal);
    if(!current(c)||detail.item?.source!=="shared"||JSON.stringify(sharedCatalogueReference(detail.item?.reference))!==JSON.stringify(ref)||!Array.isArray(detail.files)||detail.files.length>64)fail();
    const lifecycle=sharingInteger(detail.lifecycle_generation),files=detail.files.map(file=>{
      const context=sharedPlaybackFileContextFromDetail(ref,file,lifecycle);contexts.set(context,c);return Object.freeze({file:copy(engine(file)),context});
    });
    return Object.freeze({detail:copy(engine(detail)),files:Object.freeze(files)});
  }
  function query(value){
    if(!value||typeof value!=="object"||Array.isArray(value))fail();const q={...value};
    for(const [k,v]of Object.entries(q)){
      if(k==="force"){if(!["auto","original","transcode"].includes(v))fail();}
      else{const min=k==="subtitle"?-1:k==="audio_offset_ms"?-15000:0,max=k==="audio_offset_ms"?15000:4095;
        if(!["audio","subtitle","audio_offset_ms"].includes(k)||typeof v!=="number"||!Number.isInteger(v)||v<min||v>max)fail();}
    }return q;
  }
  function descriptive(url,context){
    if(typeof url!=="string")fail();if(url===context.file_base+"/direct"||url===context.file_base+"/hls/sessions")return;
    const prefix=context.file_base+"/stream.mp4";if(url===prefix||url.startsWith(prefix+"?audio=")&&/^(0|[1-9][0-9]{0,3})$/.test(url.slice(prefix.length+7))&&Number(url.slice(prefix.length+7))<=4095)return;fail();
  }
  function validate(raw,context){
    const ref=raw?.reference;
    if(raw?.file_id!==context.source_file_id||!ref||ref.file_id!==context.source_file_id||ref.revision!==context.file_revision||ref.lifecycle_generation!==context.lifecycle_generation||JSON.stringify(sharedCatalogueReference(ref.item))!==JSON.stringify(context.source_ref))fail();
    if(!["direct_play","remux","transcode"].includes(raw.method)||raw.delivery?.mode!==(raw.method==="direct_play"?"direct":raw.method))fail();
    descriptive(raw.play_url,context);
    for(const k of ["url","sessions_url"]){if(raw.delivery[k]!=null){descriptive(raw.delivery[k],context);if(k==="sessions_url"&&raw.delivery[k]!==context.file_base+"/hls/sessions")fail();}}
    const decision=engine(raw);decision.reference.lifecycle_generation=ref.lifecycle_generation;return decision;
  }
  async function decision(context,selection={force:"auto"},signal=null){
    const c=contexts.get(context);if(!c||!current(c))fail();playbackFileContext(context);const q=query(selection);
    const caps=currentCapsDocument();if(caps?.v!==2)fail();const body=JSON.stringify({caps});if(new TextEncoder().encode(body).length>REQUEST_MAX)fail();
    const snapshot=copy(JSON.parse(body).caps),raw=await read(playbackFileUrl(context,"decision",q),c,body,signal);
    if(contexts.get(context)!==c||!current(c))fail();const result=validate(raw,context);
    Object.defineProperty(result,"_capsSnapshot",{value:snapshot,enumerable:false});return result;
  }
  return Object.freeze({details,decision,retire});
})();
function sharedDecisionRetire(){SHARED_DECISION.retire();}
