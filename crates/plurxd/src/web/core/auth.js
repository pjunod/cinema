"use strict";
// ---- auth flows -----------------------------------------------------------
function clearLocalSession(expectedGeneration,notice){
  // A logout response can arrive after another sign-in. It belongs to the
  // captured bearer, never to whichever credential happens to be current now.
  if(expectedGeneration!==AUTH_GENERATION) return false;
  stopLiveTv().catch(()=>{});
  clearLibraryChannelDraft(true);
  LIBRARY_CHANNEL_TUNE.stop();
  if(READER) destroyReader(false);
  PAGE_RENDER_GENERATION++;
  clearInterval(PAGE_TIMER); PAGE_TIMER=null;
  ACTIVITY_SNAPSHOT=null; ACTIVITY_DETAIL_BUSY=0;
  Object.assign(ACTIVITY_DVR,{rows:[],next:null,loaded:false,error:null,loading:false,recent:[],attention:[],recentAt:0,recentError:null});
  clearTimeout(DVR_PAGE.historyTimer);Object.assign(DVR_PAGE,{selectedId:null,selected:null,events:[],pendingId:null,historyTimer:null});
  SETTINGS=null; TRAKT=null; TRAKT_EDIT=false; SETTINGS_TICKING=null; SETTINGS_DATA={};
  SETTINGS_LOADED.clear(); SETTINGS_LOADS.clear();
  LOGS_RUN=null; CLUSTER_LOGS_RUN=null;
  CLUSTER_LOADED=false; CLUSTER_LEAVING=false; forgetJoinToken();
  const protectedMain=document.getElementById("main"); if(protectedMain) protectedMain.innerHTML="";
  TOKEN=null; AUTH_GENERATION++; ME=null;
  AUTH_NOTICE=notice||"";
  if(!NATIVE_READER_BOOT) localStorage.removeItem("plurx_token");
  clearInterval(ACT_TIMER); ACT_TIMER=null; document.documentElement.classList.remove("working");
  clearInterval(DVR_REMINDER_TIMER); DVR_REMINDER_TIMER=null;
  DVR_DUE=[]; paintDvrReminder();
  if(NATIVE_READER_BOOT){ nativeReaderPost("session-ended"); return; }
  render();
  return true;
}
async function logout({revoke=true,notice=""}={}){
  const token=TOKEN, authGeneration=AUTH_GENERATION;
  if(SIGN_OUT_RUN&&SIGN_OUT_RUN.generation===authGeneration) return SIGN_OUT_RUN.promise;
  if(!revoke||!token){
    clearLocalSession(authGeneration,notice);
    return false;
  }
  // Keep the request bound to the origin and bearer selected at the click.
  // `api()` intentionally follows live session state, which is wrong for an
  // operation whose whole purpose is ending that exact state.
  const controller=new AbortController();
  const timer=setTimeout(()=>controller.abort(),5000);
  const run=(async()=>{
    let confirmed=false;
    try{
      const response=await fetch(location.origin+API+"/auth/logout",{
        method:"POST",headers:{authorization:"Bearer "+token},signal:controller.signal
      });
      // This endpoint uses the authentication extractor, so 401 proves this
      // bearer no longer authorizes API access. A proxy-generated 403 does not.
      confirmed=response.ok||response.status===401;
    }catch(e){ void e; }
    finally{ clearTimeout(timer); }
    clearLocalSession(authGeneration,confirmed
      ?"Sign-out was confirmed by the server."
      :"Signed out on this device. The server could not confirm revocation.");
    return confirmed;
  })();
  SIGN_OUT_RUN={generation:authGeneration,promise:run};
  try{ return await run; }
  finally{ if(SIGN_OUT_RUN&&SIGN_OUT_RUN.promise===run) SIGN_OUT_RUN=null; }
}
let SERVER=null;   // last /server response (name, version, android_app, …)
async function boot(){
  // Before anything can build a /decision URL: the MediaCapabilities refinement
  // of PLAY_CAPS is async, and the server hears one capability answer per
  // session with no way to correct it afterwards. Awaiting here — ahead of even
  // the /server call — means the setup and login paths, which reach render()
  // without coming back through boot(), are covered too.
  try{ await PLAY_CAPS_READY; }catch(e){}
  let server;
  try{ server=await api("/server"); SERVER=server; }catch(e){
    document.getElementById("app").innerHTML=`<div class="center"><div class="card">Server unreachable.</div></div>`;
    if(NATIVE_READER_BOOT) nativeReaderPost("error","Cinema server unreachable.");
    return;
  }
  if(server.setup_required){
    if(NATIVE_READER_BOOT){ nativeReaderPost("error","Finish setting up this Cinema server before reading."); return; }
    return renderSetup(server);
  }
  if(!TOKEN){
    if(NATIVE_READER_BOOT){ nativeReaderPost("session-ended"); return; }
    return renderLogin(server);
  }
  try{ ME=await api("/me"); }catch(e){ return NATIVE_READER_BOOT?undefined:renderLogin(server); }
  location.hash=location.hash||"#/";
  render();
}
function authShell(title, sub, fields, btn, onsubmit){
  document.getElementById("app").innerHTML=`<div class="center"><form class="card auth" id="af">
    <div class="logo" style="font-size:30px">${APP_NAME}</div>
    <h1>${title}</h1><p class="muted" style="margin-top:0">${sub}</p>
    ${fields}<div class="err" id="aerr"></div>
    <button style="width:100%;margin-top:16px" id="ab">${btn}</button>
    <div class="versionstamp">Version ${esc(buildLabel())}</div></form></div>`;
  document.getElementById("af").addEventListener("submit",async e=>{
    e.preventDefault(); const b=document.getElementById("ab"); b.disabled=true;
    try{ await onsubmit(); }catch(err){ document.getElementById("aerr").textContent=err.message; b.disabled=false; }
  });
}
// The first account, and the one place a typo is unrecoverable from inside the
// product: there is no other admin to reset it and no email to recover to. A
// mistyped password here locks the owner out of their own server; the legacy
// direct-store console reset is deliberately refused because it cannot revoke
// daemon-local recovery proofs. Hence the confirm field, and
// hence the length check running here rather than only at the server: the
// server's "at least 8 characters" arrives as a red line under a form whose
// contents are already gone.
function renderSetup(){
  authShell("Welcome","Create your admin account to get started.",
    `<label for="u">Username</label><input id="u" autocomplete="username" required>
     <label for="p">Password</label><input id="p" type="password" autocomplete="new-password" required placeholder="at least 8 characters">
     <label for="p2">Confirm password</label><input id="p2" type="password" autocomplete="new-password" required placeholder="type it again">`,
    "Create account", async()=>{
      if(u.value.trim()==="") throw new Error("Pick a username.");
      if(p.value.length<8) throw new Error("Password must be at least 8 characters.");
      if(p.value!==p2.value){ p2.value=""; p2.focus(); throw new Error("Those two passwords don't match. Try again."); }
      const r=await api("/setup",{method:"POST",body:{username:u.value.trim(),password:p.value}});
      TOKEN=r.token; AUTH_GENERATION++; localStorage.setItem("plurx_token",TOKEN); ME=r.user; location.hash="#/"; render();
    });
}
function renderLogin(){
  const subtitle=AUTH_NOTICE||"Welcome back.";
  AUTH_NOTICE="";
  authShell("Sign in",esc(subtitle),
    `<label for="u">Username</label><input id="u" autocomplete="username" required>
     <label for="p">Password</label><input id="p" type="password" autocomplete="current-password" required>
     <details class="connectqr"><summary>Connect an app with a QR code</summary>
       <img src="/connect.svg?origin=${encodeURIComponent(location.origin)}" alt="QR code for ${esc(location.origin)}">
       <p>Scan this from the ${APP_NAME} iPhone, iPad, or Android app to add this server.</p>
       <p>The code contains only the server address. Sign in inside the app with your usual account.</p>
     </details>`,
    "Sign in", async()=>{
      const r=await api("/auth/login",{method:"POST",body:{username:u.value.trim(),password:p.value,device:"Web"}});
      TOKEN=r.token; AUTH_GENERATION++; localStorage.setItem("plurx_token",TOKEN); ME=r.user; location.hash="#/"; render();
    });
}

