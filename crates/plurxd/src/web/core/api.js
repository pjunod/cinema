"use strict";
// ---- api helpers ----------------------------------------------------------
async function api(path, {method="GET", body=null, raw=false, signal=null, keepSessionOn401=false}={}){
  const authGeneration=AUTH_GENERATION;
  const headers={};
  if(TOKEN) headers["authorization"]="Bearer "+TOKEN;
  if(body){ headers["content-type"]="application/json"; }
  const res=await fetch(API+path,{method,headers,body:body?JSON.stringify(body):null,signal});
  if(authGeneration!==AUTH_GENERATION){
    const error=new Error("stale authorization"); error.status=401; error.staleAuth=true;
    throw error;
  }
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
    // Parallel requests from an expired credential may finish after the user
    // has already signed in again. Only the credential generation that sent
    // this request may clear the current session.
    if(authGeneration===AUTH_GENERATION) void logout({revoke:false,notice:"Your session ended on the server."});
    const error=new Error("unauthorized"); error.status=401;
    throw error;
  }
  // `{error}` is the legacy body; `{code, message}` is the typed one routes
  // migrate to as their client contracts need a machine-readable cause. Read
  // both, or a typed refusal — "this server's ffmpeg has no subtitles filter"
  // — reaches the viewer as the useless "request failed".
  if(!res.ok){
    let m="request failed", responseBody="", code=null;
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
  return raw?res:res.json();
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
}
// Forward a client-side playback problem to the server so it lands in
// Settings → Logs. Fire-and-forget: a browser that rejects a stream produces no
// server log on its own (nothing ran server-side to fail), so without this the
// failures a user hits in Safari are invisible unless they open dev tools. Never
// throws, never blocks playback; auto-fills context from the active PLAYER.
function clientLog(ev){
  try{
    const p=PLAYER||{}, s=p.source||{};
    const body=Object.assign({
      ua:browserLabel(),
      method:p.method||null,
      title:p.title||null,
      file_id:p.fileId||null,
      vcodec:s.video_codec||null,
    }, ev||{});
    const control=p.controlReporter&&p.controlReporter.legacyContext();
    if(control && body.control==null) body.control=control;
    fetch(API+"/client-log",{method:"POST",keepalive:true,
      headers:Object.assign({"content-type":"application/json"},TOKEN?{"authorization":"Bearer "+TOKEN}:{}),
      body:JSON.stringify(body)}).catch(()=>{});
  }catch(e){}
}
refreshClientErrorReporterAuth();
