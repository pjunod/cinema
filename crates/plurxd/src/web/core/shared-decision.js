"use strict";
// Decisions describe delivery; they never acquire a B session or start media.
const SHARED_DECISION=(()=>{
  const MAX=4*1024*1024,REQUEST_MAX=128*1024;
  let contexts=new WeakMap();const accepted=new WeakMap(),watches=new Map(),jobs=new Set();let watchAuth=AUTH_GENERATION;
  const fail=()=>{throw new Error("Shared decision unavailable");};
  function authorized(c){return c.auth===AUTH_GENERATION&&c.token===TOKEN&&c.origin===API;}
  function current(c){return c.auth===AUTH_GENERATION&&c.token===TOKEN&&c.origin===API&&c.generation===PAGE_RENDER_GENERATION&&c.route===location.hash;}
  function capture(){const c=Object.freeze({auth:AUTH_GENERATION,token:TOKEN,origin:API,generation:PAGE_RENDER_GENERATION,route:location.hash});if(!c.token||!current(c))fail();origin(c);return c;}
  function origin(c){const u=new URL(c.origin,location.href);if(!["http:","https:"].includes(u.protocol)||u.username||u.password||u.search||u.hash||u.pathname!=="/api/v1")fail();return u.origin;}
  function retire(){contexts=new WeakMap();for(const j of jobs){j.controller.abort();j.reader?.cancel().catch(()=>{});}}
  function copy(value){if(Array.isArray(value))return Object.freeze(value.map(copy));if(value&&typeof value==="object"){const o=Object.create(null);for(const [k,v]of Object.entries(value))o[k]=copy(v);return Object.freeze(o);}return value;}
  // Engine fields stay numbers where exact, while future/identity integers that
  // cannot be represented by Number remain lossless BigInts.
  function engine(value){if(typeof value==="bigint"&&value>=BigInt(Number.MIN_SAFE_INTEGER)&&value<=BigInt(Number.MAX_SAFE_INTEGER))return Number(value);if(Array.isArray(value))return value.map(engine);if(value&&typeof value==="object"){const o=Object.create(null);for(const [k,v]of Object.entries(value))o[k]=engine(v);return o;}return value;}
  async function read(path,c,body=null,signal=null,mode="page",conflict=false){
    const valid=()=>mode==="session"?authorized(c):current(c);
    if(!valid()||signal?.aborted)fail();const controller=new AbortController(),job={controller,reader:null};jobs.add(job);
    const abort=()=>controller.abort();signal?.addEventListener("abort",abort,{once:true});const timer=setTimeout(abort,mode==="start"?310000:30000);
    try{
      const expected=origin(c)+path,response=await fetch(expected,{method:body===null?"GET":"POST",headers:{authorization:"Bearer "+c.token,...(body===null?{}:{"content-type":"application/json"})},...(body===null?{}:{body}),credentials:"omit",cache:"no-store",redirect:"error",signal:controller.signal});
      if(!valid()||controller.signal.aborted||response.redirected||response.url!==expected){await response.body?.cancel();fail();}
      if(response.status!==200&&!(conflict&&response.status===409)){await response.body?.cancel();if(response.status===401||response.status===403)retire();fail();}
      const declared=response.headers.get("content-length");if(declared!==null&&(!/^(0|[1-9][0-9]*)$/.test(declared)||BigInt(declared)>BigInt(MAX))){await response.body?.cancel();fail();}
      const reader=response.body?.getReader();if(!reader)fail();job.reader=reader;const chunks=[];let length=0;
      try{for(;;){const p=await reader.read();if(!valid()||controller.signal.aborted)fail();if(p.done)break;if(!(p.value instanceof Uint8Array))fail();length+=p.value.byteLength;if(length>MAX)fail();chunks.push(p.value);}}
      catch(e){await reader.cancel().catch(()=>{});throw e;}finally{job.reader=null;reader.releaseLock();}
      if(!valid()||controller.signal.aborted)fail();const bytes=new Uint8Array(length);let offset=0;for(const p of chunks){bytes.set(p,offset);offset+=p.byteLength;}
      const value=sharingJSON(new TextDecoder("utf-8",{fatal:true}).decode(bytes));
      return conflict?{status:response.status,value}:value;
    }finally{clearTimeout(timer);signal?.removeEventListener("abort",abort);jobs.delete(job);}
  }
  async function details(reference,signal=null){
    const ref=sharedCatalogueReference(reference),c=capture();
    const detail=await read(`/api/v1/shared/imports/${ref.import_id}/items/${ref.item_id}`,c,null,signal);
    if(!current(c)||detail.item?.source!=="shared"||JSON.stringify(sharedCatalogueReference(detail.item?.reference))!==JSON.stringify(ref)||!Array.isArray(detail.files)||detail.files.length>64)fail();
    const lifecycle=sharingInteger(detail.lifecycle_generation),watch=watchState(ref,detail.watch),files=detail.files.map(file=>{
      const context=sharedPlaybackFileContextFromDetail(ref,file,lifecycle);contexts.set(context,{...c,watch});return Object.freeze({file:copy(engine(file)),context});
    });
    return Object.freeze({detail:copy(engine(detail)),files:Object.freeze(files)});
  }
  function integer(value){const n=typeof value==="bigint"?value:typeof value==="number"&&Number.isSafeInteger(value)?BigInt(value):null;if(n===null||n<0n||n>BigInt(Number.MAX_SAFE_INTEGER))fail();return Number(n);}
  function watchState(ref,watch){
    if(watchAuth!==AUTH_GENERATION){watches.clear();watchAuth=AUTH_GENERATION;}
    const key=JSON.stringify([ref.server_id,ref.catalogue_epoch,ref.item_id]);
    let state=watches.get(key);if(!state){if(watches.size>=256)fail();state={sequence:0,pending:null,busy:false,resync:false,floor:null};watches.set(key,state);}
    if(watch!=null)state.sequence=Math.max(state.sequence,integer(watch.sequence));return state;
  }
  function unsupported(){throw Object.assign(new Error("This shared playback change is not available yet."),{code:"sharing_start_unsupported"});}
  async function start(context,body,signal=null){
    const c=contexts.get(context);if(!c||!current(c)||context.session_id)fail();playbackFileContext(context);
    if(!body||typeof body!=="object"||Array.isArray(body))fail();
    for(const field of ["previous_session_id","control_sequence","reopen_reason","intent","candidate_id"])if(body[field]!=null)unsupported();
    if(body.subtitle_burn!=null||body.hdr10===true||body.preserve_dolby_vision===true)unsupported();
    if(body.caps?.v!==2||typeof body.playback_id!=="string"||!body.playback_id||body.playback_id.length>128||/[\u0000-\u001f\u007f]/.test(body.playback_id)
      ||typeof body.request_id!=="string"||! /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(body.request_id))fail();
    const text=JSON.stringify(body);if(new TextEncoder().encode(text).length>24*1024)fail();
    const raw=await read(playbackFileUrl(context,"hls/sessions"),c,text,signal,"start"),response=engine(raw);
    if(!current(c))fail();const bound=sharedPlaybackStartContext(context,response);accepted.set(bound,c);
    if(c.watch.pending&&c.watch.pending.session_id!==bound.session_id)c.watch.pending=null;
    Object.defineProperty(response,"_sharedContext",{value:bound,enumerable:false});return response;
  }
  async function resync(context,c,state){
    const fresh=await read(`/api/v1/shared/imports/${context.source_ref.import_id}/items/${context.source_ref.item_id}`,c,null,null,"session");
    if(fresh.item?.source!=="shared"||JSON.stringify(sharedCatalogueReference(fresh.item.reference))!==JSON.stringify(context.source_ref)||sharingInteger(fresh.lifecycle_generation)!==context.lifecycle_generation)fail();
    if(fresh.watch!=null)state.sequence=Math.max(state.sequence,integer(fresh.watch.sequence));
    if(state.floor!=null)state.sequence=Math.max(state.sequence,state.floor);state.resync=false;state.floor=null;
  }
  async function progress(context,position,duration,watched=false){
    const c=accepted.get(context);if(!c||!authorized(c)||!context.session_id)fail();playbackFileContext(context);
    const state=c.watch;if(!state||state.busy)return false;
    const position_ms=integer(position),duration_ms=duration==null?null:integer(duration);if(typeof watched!=="boolean")fail();
    state.busy=true;
    try{
      if(state.resync){await resync(context,c,state);return false;}
      if(!state.pending){if(state.sequence>=Number.MAX_SAFE_INTEGER)fail();state.pending=Object.freeze({session_id:context.session_id,sequence:++state.sequence,position_ms,duration_ms,watched});}
      // An uncertain send retries exactly; a later beat never renumbers the
      // retained payload or overwrites newer Source/item history after409.
      const pending=state.pending,result=await read(`/api/v1/shared/imports/${context.source_ref.import_id}/items/${context.source_ref.item_id}/progress`,c,JSON.stringify(pending),null,"session",true);
      if(result.status===409){
        if(!["sharing_progress_stale","sharing_progress_conflict"].includes(result.value?.code))fail();
        state.floor=result.value.current_sequence==null?null:integer(result.value.current_sequence);
        state.pending=null;state.resync=true;await resync(context,c,state);return false;
      }
      if(state.pending===pending)state.pending=null;return pending.position_ms===position_ms&&pending.duration_ms===duration_ms&&pending.watched===watched;
    }finally{state.busy=false;}
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
  return Object.freeze({details,decision,start,progress,retire});
})();
function sharedDecisionRetire(){SHARED_DECISION.retire();}
