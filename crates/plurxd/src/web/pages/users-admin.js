"use strict";
// ---- users (admin) --------------------------------------------------------
function userRow(u){
  const me = ME&&ME.id===u.id;
  const open = USER_DRAWER===u.id;
  const created = new Date(u.created_at*1000).toLocaleDateString();
  const role = u.is_admin?`<span class="pill acc">admin</span>`:`<span class="muted">user</span>`;
  const devicesOpen = DEVICE_DRAWER&&DEVICE_DRAWER.user===u.id;
  const btns=[`<button class="ghost sm" aria-expanded="${devicesOpen?"true":"false"}"${devicesOpen?` aria-controls="devicedrawer-${u.id}"`:""} onclick="openDeviceDrawer(${u.id})">Devices</button>`,
    `<button class="ghost sm" aria-expanded="${open?"true":"false"}"${open?` aria-controls="userdrawer-${u.id}"`:""} onclick="openUserDrawer(${u.id})">Reset password</button>`];
  btns.push(`<button class="ghost sm" onclick="setAdmin(${u.id},${u.is_admin?0:1})">${u.is_admin?'Revoke admin':'Make admin'}</button>`);
  if(!me) btns.push(`<button class="ghost sm" onclick='delUser(${u.id},${esc(JSON.stringify(u.username))})'>Delete</button>`);
  return `<tr${open?' class="setopen"':""}><td class="libname"><b>${esc(u.username)}</b>${me?' <span class="muted">(you)</span>':''}</td><td data-label="Role">${role}</td><td class="muted" data-label="Created">${created}</td>
    <td class="rowactions"><div class="row" style="flex-wrap:wrap;justify-content:flex-end">${btns.join("")}</div></td></tr>${open?userDrawerHtml(u):""}${devicesOpen?deviceDrawerHtml(u):""}`;
}
// ---- sign-ins: expiry and each account's devices --------------------------
// Which user's devices are listed: {user, rows, error}; rows is null while the
// list is loading. One drawer at a time, like the password drawer.
let DEVICE_DRAWER=null;
const SIGN_IN_DAYS=[[7,"7 days"],[30,"30 days"],[90,"90 days"],[180,"180 days"],[365,"1 year"]];
function signInExpiryCard(settings){
  if(!settings) return "";
  const on=settings.auth_token_expiry!==false;
  const days=Number(settings.auth_token_idle_days)||90;
  const options=SIGN_IN_DAYS.some(([d])=>d===days)?SIGN_IN_DAYS
    :SIGN_IN_DAYS.concat([[days,`${days} days`]]).sort((a,b)=>a[0]-b[0]);
  return setCard(`${cardHead("Sign-ins","For every account on this server. Each account's devices, and when each one will be signed out, are under Devices above.")}
    ${togRow("signin-expire","Sign-ins expire","A device nobody uses for the time below is signed out and asks for its password again; a device in regular use stays signed in. Turning this on signs nothing out on the spot — every device gets the full time from then.",on)}
    ${togSelect("signin-days","Sign out after","How long a device may go unused",options,days,"How long a device may go unused before it is signed out")}
    ${setCardFoot("saveSignIns")}`,{id:"sign-ins"});
}
async function saveSignIns(btn){
  btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      auth_token_expiry:document.getElementById("signin-expire").checked,
      auth_token_idle_days:Number(document.getElementById("signin-days").value)}}));
    toast("Sign-in settings saved"); setCardSaved(btn);
    if(DEVICE_DRAWER) loadDeviceDrawer(DEVICE_DRAWER.user);
  }catch(e){ toast(e.message||"Couldn't save"); btn.disabled=false; }
}
// What a devices row says about expiry. `expires_at` is absent while sign-ins
// do not expire; otherwise it is when the device is signed out if it stays
// unused, which moves every time the device is used.
function deviceExpiryLabel(row,nowSecs){
  if(!row||row.expires_at==null) return "Doesn't expire";
  if(row.expired||row.expires_at<=nowSecs) return "Signed out — unused too long";
  const days=Math.ceil((row.expires_at-nowSecs)/86400);
  const when=new Date(row.expires_at*1000).toLocaleDateString();
  return days<=1?`Signs out within a day if unused (${when})`:`Signs out in ${days} days if unused (${when})`;
}
function openDeviceDrawer(id){
  if(DEVICE_DRAWER&&DEVICE_DRAWER.user===id){ DEVICE_DRAWER=null; renderSettings(); return; }
  loadDeviceDrawer(id);
}
async function loadDeviceDrawer(id){
  DEVICE_DRAWER={user:id,rows:null,error:null};
  renderSettings();
  try{
    const rows=await api(`/users/${id}/devices`);
    if(DEVICE_DRAWER&&DEVICE_DRAWER.user===id){ DEVICE_DRAWER.rows=rows; renderSettings(); }
  }catch(e){
    if(e&&e.status===401) return;
    if(DEVICE_DRAWER&&DEVICE_DRAWER.user===id){ DEVICE_DRAWER.error=e.message||"request failed"; renderSettings(); }
  }
}
function deviceDrawerHtml(u){
  const d=DEVICE_DRAWER, now=Math.floor(Date.now()/1000);
  let body;
  if(d.error) body=`<div class="err">${esc(d.error)}</div>`;
  else if(!d.rows) body=`<div class="muted">Loading devices…</div>`;
  else if(!d.rows.length) body=`<div class="muted">No devices are signed in.</div>`;
  else body=`<table><thead><tr><th>Device</th><th>Last used</th><th>Expiry</th><th></th></tr></thead><tbody>${d.rows.map(row=>
    `<tr><td>${esc(row.device||"Unnamed device")}</td><td class="muted" data-label="Last used">${esc(agoLabel(row.last_seen_at))}</td>
     <td class="muted" data-label="Expiry">${esc(deviceExpiryLabel(row,now))}</td>
     <td class="rowactions"><button class="ghost sm" onclick='revokeDevice(${u.id},${esc(JSON.stringify(row.token_hash_prefix))},this)'>Sign out</button></td></tr>`).join("")}</tbody></table>`;
  return `<tr class="setdrawer" id="devicedrawer-${u.id}"><td colspan="4"><h2 class="section" style="margin:0 0 6px;padding:0;border:0">${esc(u.username)}'s devices</h2>${body}
    <div class="setfoot"><button class="ghost sm" onclick="openDeviceDrawer(${u.id})">Close</button></div></td></tr>`;
}
async function revokeDevice(id,prefix,btn){
  if(btn) btn.disabled=true;
  try{ await api(`/users/${id}/devices/${prefix}`,{method:"DELETE"}); toast("Device signed out"); loadDeviceDrawer(id); }
  catch(e){ toast(e.message||"Couldn't sign the device out"); if(btn) btn.disabled=false; }
}
async function addUser(btn){
  const username=document.getElementById("un").value.trim(), password=document.getElementById("up-new").value, is_admin=document.getElementById("ua").checked;
  const again=document.getElementById("up2-new");
  const err=document.getElementById("uerr"); err.textContent="";
  // Cheaper to catch here than to have someone discover it at their first
  // sign-in — a mistyped password creates an account nobody can get into,
  // and the person who has to fix it isn't the one who made the mistake.
  if(!username){ err.textContent="Pick a username."; return; }
  if(password.length<8){ err.textContent="Password must be at least 8 characters."; return; }
  if(password!==again.value){ again.value=""; again.focus(); err.textContent="Those two passwords don't match."; return; }
  if(btn) btn.disabled=true;
  try{ await api("/users",{method:"POST",body:{username,password,is_admin}}); USER_DRAWER=null; toast("User added"); viewSettings(); }
  catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function resetPw(id, name, btn){
  const pw=document.getElementById(`up-${id}`).value, again=document.getElementById(`up2-${id}`).value;
  const err=document.getElementById("uerr"); if(err) err.textContent="";
  if(pw.length<8){ if(err) err.textContent="Password must be at least 8 characters."; return; }
  // Asked twice for the same reason the other two forms have a confirm field,
  // and it matters more here: this signs every one of their sessions out, so a
  // typo doesn't just set a password nobody knows — it takes away the one they
  // were already using.
  if(again!==pw){ if(err) err.textContent="Those two passwords don't match — nothing was changed."; return; }
  if(btn) btn.disabled=true;
  try{ await api(`/users/${id}`,{method:"PUT",body:{password:pw}}); USER_DRAWER=null; toast(`Password reset for ${name}`); renderSettings(); }
  catch(e){ if(err) err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function setAdmin(id, makeAdmin){
  try{ await api(`/users/${id}`,{method:"PUT",body:{is_admin:!!makeAdmin}}); toast(makeAdmin?"Now an admin":"Admin revoked"); viewSettings(); }catch(e){ toast(e.message); }
}
async function delUser(id, name){
  if(!confirm(`Delete ${name}? Their watch history is removed. Media files are untouched.`)) return;
  try{ await api(`/users/${id}`,{method:"DELETE"}); toast("User deleted"); viewSettings(); }catch(e){ toast(e.message); }
}

