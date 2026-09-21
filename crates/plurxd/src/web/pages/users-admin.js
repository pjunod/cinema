"use strict";
// ---- users (admin) --------------------------------------------------------
function userRow(u){
  const me = ME&&ME.id===u.id;
  const open = USER_DRAWER===u.id;
  const created = new Date(u.created_at*1000).toLocaleDateString();
  const role = u.is_admin?`<span class="pill acc">admin</span>`:`<span class="muted">user</span>`;
  const btns=[`<button class="ghost sm" aria-expanded="${open?"true":"false"}"${open?` aria-controls="userdrawer-${u.id}"`:""} onclick="openUserDrawer(${u.id})">Reset password</button>`];
  btns.push(`<button class="ghost sm" onclick="setAdmin(${u.id},${u.is_admin?0:1})">${u.is_admin?'Revoke admin':'Make admin'}</button>`);
  btns.push(`<button class="ghost sm" onclick="setOpticalPlay(${u.id},${u.optical_play?0:1})">${u.optical_play?'Revoke disc access':'Grant disc access'}</button>`);
  if(!me) btns.push(`<button class="ghost sm" onclick='delUser(${u.id},${esc(JSON.stringify(u.username))})'>Delete</button>`);
  return `<tr${open?' class="setopen"':""}><td class="libname"><b>${esc(u.username)}</b>${me?' <span class="muted">(you)</span>':''}</td><td data-label="Role">${role}</td><td class="muted" data-label="Created">${created}</td>
    <td class="rowactions"><div class="row" style="flex-wrap:wrap;justify-content:flex-end">${btns.join("")}</div></td></tr>${open?userDrawerHtml(u):""}`;
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
async function setOpticalPlay(id, granted){
  try{
    await api(`/users/${id}`,{method:"PUT",body:{optical_play:!!granted}});
    toast(granted?"Disc playback granted":"Disc playback revoked");
    viewSettings();
  }catch(e){ toast(e.message); }
}
async function delUser(id, name){
  if(!confirm(`Delete ${name}? Their watch history is removed. Media files are untouched.`)) return;
  try{ await api(`/users/${id}`,{method:"DELETE"}); toast("User deleted"); viewSettings(); }catch(e){ toast(e.message); }
}
