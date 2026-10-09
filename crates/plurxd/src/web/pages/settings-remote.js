"use strict";
// Device-local settings are safe for every signed-in viewer. No server settings
// or privileged readiness endpoint is used to render/save these cards.
function cinemaRemoteLocalCards(){
  const prefs=cinemaRemotePreferences(),status=CINEMA_WEB_RECEIVER?.state||"off";
  return setCard(`${cardHead("Local TV remote","Receive HDMI-CEC input through the independently installed desktop extension.")}
    ${togRow("dev-cinema-cec","Enable local CEC","Saved on this device and independent of the server remote-control setting.",prefs.cec)}
    <p class="hint">Install the source-reviewed Chromium extension and native host, then use its Bind this Cinema tab button. Local pause and Stop keep using this browser's existing player even when the server is unavailable.</p>
    ${devStaticReq("Physical acceptance","pending","Linux/Pi and Mac adapters, native browser registration, permission prompts and Windows launcher qualification remain open.","")}
    ${devGraduation("the desktop CEC hardware and installation acceptance is recorded.","this device-local switch moves to the normal playback preferences.")}${setCardFoot("saveCinemaCec")}`)+
    setCard(`${cardHead("Receive phone controls","Let this foreground Cinema screen receive approved same-account commands.")}
    ${togRow("dev-cinema-receiver","Enable this screen as a receiver","Saved choice stays authoritative; availability is advisory.",prefs.receiver)}
    <label class="setfield"><span>Screen name</span><input id="cinema-screen-name" maxlength="80" value="${esc(cinemaRemoteLoad(cinemaRemoteIdentity()).receiver?.name||"Cinema browser")}"></label>
    <p class="hint">One tab owns this installation through an exclusive browser Web Lock. A browser without Web Locks needs a secure context before it can receive; the phone remote and local CEC remain independent.</p>
    <div id="cinema-receiver-status" class="hint" role="status">${esc(status)}</div>
    <button class="ghost" onclick="cinemaRemoteRegisterScreen(event)">Register this screen</button>
    <button class="ghost" onclick="cinemaRemoteShowPairing(event)">Show pairing code</button>
    ${devGraduation("the two-device receiver, owner-loss and platform acceptance is recorded.","this device-local switch moves to the normal device preferences.")}${setCardFoot("saveCinemaReceiver")}`)+
    setCard(`${cardHead("Phone remote","Use approved grants to control another foreground Cinema screen.")}
    ${togRow("dev-cinema-companion","Enable the phone remote","Pairing still requires the TV's visible local approval.",prefs.companion)}
    ${togRow("dev-cinema-suggestions","Suggest paired screens","Only screens with a grant saved on this device can be suggested.",prefs.suggestions)}
    <p class="hint">No background or operating-system wake is promised. Dismissed cards stay dismissed until this app session ends.</p>
    <a class="ghost" href="#/remote">Open phone remote</a>
    ${devGraduation("foreground browser pairing, control and revocation acceptance is recorded.","these choices move to the normal device preferences.")}${setCardFoot("saveCinemaCompanion")}`)+
    setCard(`${cardHead("Screens and grants","Manage this signed-in account's registered screens and approved phones.")}
    <div id="cinema-remote-management" role="status">Loading this account's screens…</div><button class="ghost" onclick="cinemaRemoteLoadManagement()">Refresh</button>`);
}
function saveCinemaRemotePreference(button,kind){
  const identity=cinemaRemoteIdentity();if(!identity)return;
  const fields={cec:"dev-cinema-cec",receiver:"dev-cinema-receiver",companion:"dev-cinema-companion"};if(!Object.hasOwn(fields,kind))return;
  const prefs=cinemaRemotePreferences(identity);prefs[kind]=/** @type {HTMLInputElement} */(document.getElementById(fields[kind])).checked;
  if(kind==="companion")prefs.suggestions=/** @type {HTMLInputElement} */(document.getElementById("dev-cinema-suggestions")).checked;
  try{
    localStorage.setItem("cinema.remote.preferences:"+cinemaRemoteIdentityKey(identity),JSON.stringify(prefs));
    if(kind==="cec"&&!prefs.cec){globalThis.CinemaDesktopBridge?.disable();document.dispatchEvent(new Event("cinema-cec-disabled"));}
    if(kind==="receiver")cinemaRemoteReceiverSync();
    if(kind==="companion"&&!prefs.companion){cinemaRemoteCancelPairFlow();cinemaRemoteControllerRetire();cinemaRemoteClearSuggestion();}
    toast("Saved on this device");if(button)setCardSaved(button);cinemaRemoteUiChanged();
  }catch(error){toast("This device could not save the preference: "+error.message);}
}
function saveCinemaCec(button){saveCinemaRemotePreference(button,"cec");}
function saveCinemaReceiver(button){saveCinemaRemotePreference(button,"receiver");}
function saveCinemaCompanion(button){saveCinemaRemotePreference(button,"companion");}
function viewLocalRemoteSettings(generation){
  layoutChrome("settings",setHead("Developer","Preferences for this device only.")+cinemaRemoteLocalCards());
  setPagePhase(location.hash,generation,"shell");setPagePhase(location.hash,generation,"content");setPagePhase(location.hash,generation,"settled");cinemaRemoteLoadManagement();
}
async function cinemaRemoteRegisterScreen(event){
  if(!event?.isTrusted||!document.hasFocus())return;const identity=cinemaRemoteIdentity();if(!identity)return;
  if(!navigator.locks?.request){toast("Receiver unavailable: this browser needs Web Locks. Your saved choice is unchanged.");return;}
  const name=cinemaRemoteText((/** @type {HTMLInputElement|null} */(document.getElementById("cinema-screen-name")))?.value||"Cinema browser",80);
  return cinemaRemoteRegisterScreenOwner(identity,name);
}
async function cinemaRemoteRegisterScreenOwner(identity,name,current=()=>true){
  if(!current()||!cinemaRemoteSameIdentity(identity,cinemaRemoteIdentity())||!document.hasFocus()||!navigator.locks?.request)return;
  try{
    await navigator.locks.request("cinema.remote.receiver:"+cinemaRemoteIdentityKey(identity),{mode:"exclusive",ifAvailable:true},async lock=>{
      if(!lock){toast("Another tab owns this screen; register from that tab or return after it leaves the foreground.");return;}
      if(!current()||!cinemaRemoteSameIdentity(identity,cinemaRemoteIdentity()))return;
      const stored=cinemaRemoteLoad(identity);if(stored.receiver&&CinemaRemoteWire.id(stored.receiver.receiver_id)&&cinemaRemoteSecret(stored.receiver.receiver_secret)){toast("This screen is already registered.");return;}
      const client=new CinemaRemoteClient(identity);
      try{
        const result=await client.request("receivers",{body:{name,platform:"web"}});
        if(!CinemaRemoteWire.id(result.receiver_id)||!cinemaRemoteSecret(result.receiver_secret))throw new Error("Invalid registration response");
        if(!current()||!client.current()||!document.hasFocus()){if(client.current())await client.request("receivers/"+result.receiver_id,{method:"DELETE"}).catch(()=>{});throw new Error("Registration context changed; inspect Screens and grants before trying again.");}
        stored.receiver={receiver_id:result.receiver_id,receiver_secret:result.receiver_secret,name};
        try{cinemaRemoteSave(identity,stored);}catch(error){await client.request("receivers/"+result.receiver_id,{method:"DELETE"}).catch(()=>{});throw error;}
        toast("Screen registered. Enable receiving and show its pairing code.");
      }finally{client.retire();}
    });
    cinemaRemoteReceiverSync();cinemaRemoteLoadManagement();
  }catch(error){toast(error.message);}
}
async function cinemaRemoteLoadManagement(){
  const mount=document.getElementById("cinema-remote-management"),identity=cinemaRemoteIdentity();if(!mount||!identity)return;
  const client=new CinemaRemoteClient(identity);
  try{
    const [screens,grants]=await Promise.all([client.request("receivers",{method:"GET"}),client.request("grants",{method:"GET"})]);
    if(!mount.isConnected||!client.current())return;
    const devices=cinemaRemoteValidateDevices(screens),rows=grants.grants;
    if(!Array.isArray(rows)||rows.length>160||rows.some(value=>!CinemaRemoteWire.id(value.id)||!CinemaRemoteWire.id(value.receiver_id)||CinemaRemoteWire.bytes(value.name)>80))throw new Error("Invalid grant list");
    mount.innerHTML=devices.map(value=>`<p>${esc(value.name)} · ${value.available?"available":"offline"} <button class="ghost sm" onclick="cinemaRemoteResetInstallation('${value.receiver_id}',event)">Remove screen and grants</button></p>`).join("")+rows.map(value=>`<p>${esc(value.name)} <button class="ghost sm" onclick="cinemaRemoteRevokeGrant('${value.id}',event)">Revoke grant</button></p>`).join("")||'<p>No screens or grants yet.</p>';
  }catch(error){if(mount.isConnected&&client.current())mount.textContent=error.message;}finally{client.retire();}
}
async function cinemaRemoteResetInstallation(id,event){
  if(!event?.isTrusted||!CinemaRemoteWire.id(id))return;const identity=cinemaRemoteIdentity();if(!identity)return;const stored=cinemaRemoteLoad(identity);
  if(stored.receiver?.receiver_id===id){CINEMA_WEB_RECEIVER?.retire("reset",false);delete stored.receiver;}
  stored.grants=stored.grants.filter(value=>value.receiver_id!==id);cinemaRemoteSave(identity,stored);
  if(CINEMA_WEB_CONTROLLER?.grant?.receiver_id===id)cinemaRemoteControllerRetire();
  const client=new CinemaRemoteClient(identity);try{await client.request("receivers/"+id,{method:"DELETE"});toast("Screen and grants removed");cinemaRemoteLoadManagement();}catch(error){toast(error.message);}finally{client.retire();}
}
async function cinemaRemoteRevokeGrant(id,event){
  if(!event?.isTrusted||!CinemaRemoteWire.id(id))return;const identity=cinemaRemoteIdentity();if(!identity)return;
  const stored=cinemaRemoteLoad(identity);stored.grants=stored.grants.filter(value=>value.grant_id!==id);cinemaRemoteSave(identity,stored);if(CINEMA_WEB_CONTROLLER?.grant?.grant_id===id)cinemaRemoteControllerRetire();
  const client=new CinemaRemoteClient(identity);try{await client.request("grants/"+id,{method:"DELETE"});toast("Grant revoked");cinemaRemoteLoadManagement();}catch(error){toast(error.message);}finally{client.retire();}
}
