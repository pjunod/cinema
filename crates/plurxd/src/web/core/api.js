"use strict";
// ---- api helpers ----------------------------------------------------------
function observeDiagnosticRequest(domain,kind,facts){try{window.CinemaDiagnostics?.record(domain,kind,facts);}catch(_){} }
// Session-scoped watch-write floor. Keep the decimal string intact: Raft
// indexes may exceed JavaScript's exact Number range. Expiry is monotonic.
let READ_AFTER={generation:-1,epoch:0,index:"",expires:0};
function readAfterRequest(){
  if(READ_AFTER.generation!==AUTH_GENERATION){
    READ_AFTER={generation:AUTH_GENERATION,epoch:READ_AFTER.epoch+1,index:"",expires:0};
  }
  if(performance.now()>=READ_AFTER.expires) READ_AFTER.index="";
  return {generation:AUTH_GENERATION,epoch:READ_AFTER.epoch,index:READ_AFTER.index};
}
function forgetReadAfter(request){
  if(request.generation!==AUTH_GENERATION) return;
  READ_AFTER={generation:AUTH_GENERATION,epoch:READ_AFTER.epoch+1,index:"",expires:0};
}
function observeReadAfter(value,request){
  if(request.generation!==AUTH_GENERATION||value==null) return;
  if(!/^[1-9][0-9]{0,19}$/.test(value)||
      (value.length===20&&value>"18446744073709551615")){
    forgetReadAfter(request);
    return;
  }
  // An unknown write invalidates already-in-flight indexed replies too.
  if(request.epoch!==READ_AFTER.epoch) return;
  const previous=READ_AFTER.index;
  if(!previous||value.length>previous.length||(value.length===previous.length&&value>=previous)){
    READ_AFTER.index=value;
    READ_AFTER.expires=performance.now()+60000;
  }
}
// `body` is the request's JSON value; api() serializes it. Hand it the
// object, never a string: a pre-serialized body is encoded a second time, the
// server receives a JSON string where it expects an object, refuses it with a
// plain-text 422, and the caller can only say "request failed" — which is what
// the transcoding backend's Optimize and Save buttons did (2026-10-11). The
// type below makes scripts/web-types refuse a string it can see; the check at
// the top of the body refuses one it cannot (a value typed `any`, a wrapper
// that passes its argument through), before anything is sent.
/**
 * @param {string} path
 * @param {{method?:string, body?:Record<string, unknown>|unknown[]|null, raw?:boolean, signal?:AbortSignal|null, keepSessionOn401?:boolean, linkReceipt?:string|null}} [options]
 */
async function api(path, {method="GET", body=null, raw=false, signal=null, keepSessionOn401=false, linkReceipt=null}={}){
  if(typeof body==="string"||body instanceof String)
    throw new TypeError(`api(${path}): pass the body value, not a JSON string; api() serializes it`);
  const authGeneration=AUTH_GENERATION;
  const diagnosticStarted=performance.now();
  const diagnosticRoute=path.startsWith("/auth")||path==="/me"?"auth":path.startsWith("/diagnostics")?"diagnostics":path.startsWith("/sessions")||path.startsWith("/stream")?"playback":path.startsWith("/settings")?"settings":"browse";
  const readAfter=readAfterRequest();
  const headers={};
  if(typeof linkReceipt==="string"&&/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(linkReceipt))
    headers["x-plurx-link-receipt"]=linkReceipt;
  if(readAfter.index) headers["x-plurx-read-after"]=readAfter.index;
  if(TOKEN) headers["authorization"]="Bearer "+TOKEN;
  if(body){ headers["content-type"]="application/json"; }
  let res;
  try{
    res=await fetch(API+path,{method,headers,body:body?JSON.stringify(body):null,signal});
  }catch(error){
    if(authGeneration===AUTH_GENERATION)observeDiagnosticRequest("network","error",{route:diagnosticRoute,method,duration_ms:Math.round(performance.now()-diagnosticStarted),failure:signal?.aborted?"cancelled":"browser_network"});
    if(method!=="GET"&&method!=="HEAD") forgetReadAfter(readAfter);
    throw error;
  }
  if(authGeneration!==AUTH_GENERATION){
    const error=new Error("stale authorization"); error.status=401; error.staleAuth=true;
    throw error;
  }
  observeDiagnosticRequest("network","request",{route:diagnosticRoute,method,status:res.status,duration_ms:Math.round(performance.now()-diagnosticStarted)});
  const commitIndex=res.headers?.get("x-plurx-commit-index")??null;
  // Older peers may acknowledge a write without an indexed receipt.
  if(commitIndex==null&&method!=="GET"&&method!=="HEAD") forgetReadAfter(readAfter);
  else observeReadAfter(commitIndex,readAfter);
  // `keepSessionOn401` belongs to the two cluster recovery reads, whose guard
  // answers from a process-local proof cache. A closed cache there is a
  // statement about the cluster, not about this credential, and ending the
  // session over it is how a stuck protocol activation threw an administrator
  // at the login screen on every visit to Settings.
  //
  // So don't guess: ask. `/me` is the credential's own route and has no
  // cluster opinion. If it answers, the session is alive and the refusal
  // belongs to the panel; if it refuses, that call ends the session on the
  // ordinary path and this one just reports. Guessing either way is wrong in
  // a real case — a node still running the old build refuses these reads with
  // 401 while the credential is fine, and a token that genuinely died mid-tick
  // must not leave the Cluster tab painting forever.
  if(res.status===401&&keepSessionOn401&&authGeneration===AUTH_GENERATION){
    // The probe is the whole mechanism: `/me` refused by an expired credential
    // ends the session on the ordinary path above, inside this very call, and
    // `/me` answered means the session is alive and the refusal is the
    // cluster's. Either way this request then reports its own refusal to its
    // caller, and the panel paints it.
    try{ await api("/me"); }catch(probe){ void probe; }
  }else if(res.status===401){
    // A sign-in that sat unused past the server's idle window says so
    // (`session_expired`), and the login screen repeats the server's sentence
    // — "Signed out after 90 days of inactivity" — instead of the generic one.
    // Never throws: an unreadable body is an ordinary session end.
    let expired=null;
    try{
      const reason=JSON.parse(await res.text());
      if(reason&&reason.code==="session_expired"&&typeof reason.message==="string"&&reason.message) expired=reason.message;
    }catch(e){ void e; }
    // Parallel requests from an expired credential may finish after the user
    // has already signed in again. Only the credential generation that sent
    // this request may clear the current session.
    if(authGeneration===AUTH_GENERATION) void logout({revoke:false,notice:expired||"Your session ended on the server."});
    const error=new Error("unauthorized"); error.status=401;
    if(expired) error.code="session_expired";
    throw error;
  }
  // `{error}` is the legacy body; `{code, message}` is the typed one routes
  // migrate to as their client contracts need a machine-readable cause. Read
  // both, or a typed refusal — "this server's ffmpeg has no subtitles filter"
  // — reaches the viewer as the useless "request failed".
  if(!res.ok){
    // A body with no sentence in it — an extractor's plain-text rejection, a
    // proxy's HTML error page — still names the status, so "request failed"
    // is never the whole story.
    let m=`request failed (HTTP ${res.status})`, responseBody="", code=null;
    try{
      responseBody=await res.text();
      const b=JSON.parse(responseBody);
      m=b.error||b.message||m;
      code=b.code||null;
    }catch(e){}
    const error=new Error(m);
    error.status=res.status;
    // The stable refusal code, when the route sent one. A caller that has a
    // better sentence for a known cause needs to recognise the cause, and the
    // human-readable message is not a contract it can match on.
    error.code=code;
    // Preserve the wire refusal on the Error. Session creation can fail
    // before hls.js exists, so the XHR hook below will never see this body;
    // without carrying it through the rejected `openSession` promise the
    // persistent player overlay cannot name an unsupported ffmpeg build.
    error.streamFailure=PlaybackPolicy.parseStreamFailure({
      status:res.status, body:responseBody
    });
    throw error;
  }
  if(raw) return res;
  // A 204 has no body, and `res.json()` on an empty body rejects — WebKit's
  // wording is "The string did not match the expected pattern." Every
  // no-content write (deleting a Library channel, favouriting one) reached its
  // caller as that failure after the server had already applied it.
  if(res.status===204||res.status===205) return null;
  return res.json();
}
const tok = u => u + (u.includes("?")?"&":"?") + "token=" + encodeURIComponent(TOKEN||"");
// The library list is small, changes only from Settings, and is needed on
// pages that otherwise wouldn't fetch it (an item page has to know whether its
// library is a home library before it offers an edit button).
let LIBS_CACHE=null;
// Filling the cache is a change to the library list too, not just clearing it.
// Chrome paints before a route fetches, so on a cold load — someone opening an
// item deep-link in a new tab — a layout whose navigation is derived from this
// list draws it empty and would never hear that the answer had arrived. Same
// hook as invalidateLibs(); it fires on the transition either way.
async function libsCached(){
  if(!LIBS_CACHE){
    LIBS_CACHE=await api("/libraries");
    const L=LAYOUTS[layoutId()];
    if(L&&L.libsChanged) L.libsChanged();
  }
  return LIBS_CACHE;
}
// Dropping the cache is only half the job: a layout may hold its own view
// DERIVED from that list (catalog's sidebar), and a derived cache nobody
// invalidates repaints the library you just deleted. The hook is generic and
// optional — classic defines none — so one layout's variable never has to
// appear in a function every layout calls.
function invalidateLibs(){
  LIBS_CACHE=null;
  const L=LAYOUTS[layoutId()];
  if(L&&L.libsChanged) L.libsChanged();
}
function toast(msg){ const t=document.getElementById("toast"); t.textContent=msg; t.classList.add("show"); setTimeout(()=>t.classList.remove("show"),2200); }
// Short browser label for diagnostics. Order matters: Edge's UA contains
// "Chrome"+"Safari"; Chrome's contains "Safari" — so test most-specific first.
function browserLabel(){
  const ua=navigator.userAgent||"";
  if(/\bedg(a|ios|)\//i.test(ua)) return "Edge";
  if(/(firefox|fxios)\//i.test(ua)) return "Firefox";
  if(/(chrome|crios|chromium)\//i.test(ua)) return "Chrome";
  if(/safari\//i.test(ua) && /version\//i.test(ua)) return "Safari";
  return "browser";
}
function refreshClientErrorReporterAuth(){
  if(window.PlurxErrorReporter) window.PlurxErrorReporter.setAuth(TOKEN,browserLabel());
  if(typeof window.dispatchEvent==="function")window.dispatchEvent(new Event("cinema-diagnostic-auth"));
}
// Forward a client-side playback problem to the server so it lands in
// Settings → Logs. Fire-and-forget: a browser that rejects a stream produces no
// server log on its own (nothing ran server-side to fail), so without this the
// failures a user hits in Safari are invisible unless they open dev tools. Never
// throws, never blocks playback; auto-fills context from the active PLAYER.
function clientLog(ev){
  try{
    const state=ev?.event==="stall"?"stalled":ev?.event==="stall_end"||ev?.event==="stall_recovery"?"recovered":ev?.level==="error"?"failed":"unknown";
    const hasSessionId=ev!=null&&Object.prototype.hasOwnProperty.call(ev,"session_id");
    const reportedSession=hasSessionId?ev.session_id:ev?.session;
    const hasReportedSession=hasSessionId||(ev!=null&&Object.prototype.hasOwnProperty.call(ev,"session"));
    const ownsReportedAttempt=ev?.attempt!=null&&ev.attempt===PLAYER?.attemptId
      &&(ev.file_id==null||String(ev.file_id)===String(PLAYER?.fileId))
      &&(ev.attachment_id==null||ev.attachment_id===PLAYER?.mediaAttachment?.id);
    const ownsReportedSession=!hasReportedSession||(reportedSession!=null
      ?reportedSession===PLAYER?.sessionId||reportedSession===PLAYER?.sharedDirect?.session_id
      :PLAYER?.sessionId==null&&PLAYER?.sharedDirect==null&&ownsReportedAttempt);
    // A late failure still reaches the server under its original session,
    // but must not mark the replacement's local diagnostic attachment failed.
    if(TOKEN&&ME&&ownsReportedSession&&window.CinemaDiagnostics?.owns(PLAYER?.mediaAttachment,AUTH_GENERATION))observeDiagnosticRequest("playback",ev?.level==="error"?"error":"state",{state,mode:PLAYER?.method||"unknown",...(state==="failed"?{failure:"delivery"}:{})});
    const p=/** @type {Player} */(PLAYER||{}), s=p.source||{};
    const body=Object.assign({
      ua:browserLabel(),
      method:p.method||null,
      title:p.title||null,
      file_id:p.fileId||null,
      vcodec:s.video_codec||null,
    }, ev||{});
    const control=p.controlReporter&&p.controlReporter.legacyContext();
    if(control && body.control==null && !body.transport) body.control=control;
    fetch(API+"/client-log",{method:"POST",keepalive:true,
      headers:Object.assign({"content-type":"application/json"},TOKEN?{"authorization":"Bearer "+TOKEN}:{}),
      body:JSON.stringify(body)}).catch(()=>{});
  }catch(e){}
}
refreshClientErrorReporterAuth();
