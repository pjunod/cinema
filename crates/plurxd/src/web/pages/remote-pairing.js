"use strict";
// Pairing is a physically operated restricted surface. No semantic remote
// action is registered for these buttons, and every approval checks trust.
let CINEMA_PAIR_DIALOG=null,CINEMA_PAIR_FLOW=null,CINEMA_PAIR_GENERATION=0;
let CINEMA_PAIR_CODE_ATTEMPT=0;
let CINEMA_PAIR_LOCAL_GENERATION=0,CINEMA_PAIR_LOCAL_SIGNATURE=null,CINEMA_PAIR_OPENER=null;
function cinemaRemoteClosePairDialog(event){
  if(event&&!event.isTrusted)return;CINEMA_PAIR_DIALOG?.remove();CINEMA_PAIR_DIALOG=null;CINEMA_PAIR_LOCAL_GENERATION++;CINEMA_PAIR_CODE_ATTEMPT++;CINEMA_PAIR_LOCAL_SIGNATURE=null;
  if(CINEMA_PAIR_OPENER?.isConnected)CINEMA_PAIR_OPENER.focus({preventScroll:true});else document.getElementById("q")?.focus();CINEMA_PAIR_OPENER=null;
  if(CINEMA_WEB_RECEIVER)CINEMA_WEB_RECEIVER.challenge=null;CinemaRemote.invalidate("pairing_closed");
}
function cinemaRemoteQr(canvas,rows){
  if(!Array.isArray(rows)||rows.length<21||rows.length>177||rows.some(row=>typeof row!=="string"||row.length!==rows.length||!/^[01]+$/.test(row)))return false;
  const scale=Math.max(2,Math.floor(320/(rows.length+8))),size=(rows.length+8)*scale;canvas.width=size;canvas.height=size;
  const ctx=canvas.getContext("2d");if(!ctx)return false;ctx.fillStyle="#fff";ctx.fillRect(0,0,size,size);ctx.fillStyle="#000";
  rows.forEach((row,y)=>Array.from(row).forEach((bit,x)=>{if(bit==="1")ctx.fillRect((x+4)*scale,(y+4)*scale,scale,scale);}));return true;
}
function cinemaRemotePairDialog(receiver){
  if(CINEMA_PAIR_DIALOG)return CINEMA_PAIR_DIALOG;
  const dialog=document.createElement("dialog");dialog.id="cinema-pair-dialog";dialog.setAttribute("aria-modal","true");dialog.setAttribute("aria-label","Approve phone remote");
  CINEMA_PAIR_OPENER=document.activeElement;CINEMA_PAIR_LOCAL_GENERATION++;
  dialog.innerHTML='<h2>Pair a phone remote</h2><p>Only approve a phone you are physically pairing now.</p><button data-local-pair="enable" type="button">Enable receiving on this device</button><button data-local-pair="register" type="button">Register this screen</button><button data-local-pair="code" type="button">Show pairing code</button><div id="cinema-pair-code"></div><div id="cinema-pair-pending"></div><p id="cinema-pair-status" role="status"></p><button class="ghost" data-local-pair="close" onclick="cinemaRemoteClosePairDialog(event)">Close</button>';
  document.body.append(dialog);CINEMA_PAIR_DIALOG=dialog;dialog.addEventListener("cancel",event=>{event.preventDefault();cinemaRemoteClosePairDialog(event);});(/** @type {NodeListOf<HTMLButtonElement>} */(dialog.querySelectorAll("[data-local-pair]"))).forEach(button=>{if(button.dataset.localPair!=="close")button.onclick=event=>{if(event.isTrusted)cinemaRemotePairLocalAction(button.dataset.localPair,button);};});dialog.showModal();(/** @type {HTMLButtonElement} */(dialog.querySelector('[data-local-pair="close"]'))).focus();CinemaRemote.invalidate("physical_pairing");return dialog;
}
async function cinemaRemoteShowPairing(event){
  if(!event?.isTrusted||!document.hasFocus())return;return cinemaRemoteShowPairingOwner();
}
async function cinemaRemoteShowPairingOwner(){
  if(!document.hasFocus())return;const receiver=CINEMA_WEB_RECEIVER;
  if(!receiver?.target||!receiver.eligible()){toast("Enable receiving and register this foreground screen first.");return;}
  const generation=receiver.generation,targetKey=CinemaRemoteWire.targetKey(receiver.target),started=performance.now(),dialog=cinemaRemotePairDialog(receiver),attempt=++CINEMA_PAIR_CODE_ATTEMPT;
  receiver.challenge=null;receiver.pairings=[];CINEMA_PAIR_LOCAL_SIGNATURE=null;CINEMA_PAIR_LOCAL_GENERATION++;CinemaRemote.invalidate("new_pairing_code");
  document.getElementById("cinema-pair-pending").textContent="";document.getElementById("cinema-pair-code").textContent="Requesting a fresh code…";
  (/** @type {HTMLButtonElement|null} */(dialog.querySelector('[data-local-pair="close"]')))?.focus();
  try{
    const result=await receiver.client.request("pairing/start",{body:{target:receiver.target},proof:receiver.proof()});receiver.fence(generation);
    if(attempt!==CINEMA_PAIR_CODE_ATTEMPT)return;
    if(dialog!==CINEMA_PAIR_DIALOG||CinemaRemoteWire.targetKey(result.target)!==targetKey||!CinemaRemoteWire.id(result.challenge_id)||!/^[0-9]{8}$/.test(result.code)||!CinemaRemoteWire.positive(result.expires_in_ms)||result.expires_in_ms>120000)throw new Error("Pairing response unavailable");
    receiver.challenge={...result,deadline:started+result.expires_in_ms};
    const mount=document.getElementById("cinema-pair-code");mount.innerHTML=`<p>Choose this screen on the signed-in phone and enter <strong>${result.code}</strong>.</p><canvas id="cinema-pair-qr" aria-label="Pairing QR code"></canvas><p>Code expires in two minutes. Approval on this screen is still required.</p>`;
    const canvas=/** @type {HTMLCanvasElement} */(document.getElementById("cinema-pair-qr"));if(!cinemaRemoteQr(canvas,result.qr_modules))canvas.remove();
  }catch(error){if(attempt===CINEMA_PAIR_CODE_ATTEMPT&&dialog===CINEMA_PAIR_DIALOG)document.getElementById("cinema-pair-status").textContent=error.message;}
}
function cinemaRemotePairingPrompt(receiver){
  // A claimed code may be shown only while the physical code surface is open.
  if(!CINEMA_PAIR_DIALOG||!receiver.challenge||performance.now()>=receiver.challenge.deadline)return;
  const mount=document.getElementById("cinema-pair-pending");if(!mount)return;
  const signature=JSON.stringify(receiver.pairings.map(value=>[value.pending_id,value.controller_name]));
  if(signature===CINEMA_PAIR_LOCAL_SIGNATURE)return;CINEMA_PAIR_LOCAL_SIGNATURE=signature;CINEMA_PAIR_LOCAL_GENERATION++;CinemaRemote.invalidate("pairing_list");
  mount.innerHTML=receiver.pairings.map(value=>`<p>${esc(value.controller_name)} <button class="primary" data-local-pair="approve" data-local-pending="${value.pending_id}" onclick="cinemaRemoteApprove('${value.pending_id}',true,event)">Approve</button> <button class="ghost" data-local-pair="deny" data-local-pending="${value.pending_id}" onclick="cinemaRemoteApprove('${value.pending_id}',false,event)">Deny</button></p>`).join("");
}
async function cinemaRemoteApprove(pending,approve,event){
  if(!event?.isTrusted)return;return cinemaRemoteApproveOwner(pending,approve);
}
async function cinemaRemoteApproveOwner(pending,approve){
  const receiver=CINEMA_WEB_RECEIVER;
  if(!document.hasFocus()||!CINEMA_PAIR_DIALOG||!receiver?.eligible()||!receiver.challenge||performance.now()>=receiver.challenge.deadline||!receiver.pairings.some(value=>value.pending_id===pending)||typeof approve!=="boolean")return;
  const generation=receiver.generation,dialog=CINEMA_PAIR_DIALOG,challenge=receiver.challenge,attempt=CINEMA_PAIR_CODE_ATTEMPT;
  const current=()=>dialog===CINEMA_PAIR_DIALOG&&attempt===CINEMA_PAIR_CODE_ATTEMPT&&receiver.challenge===challenge&&receiver.generation===generation;
  try{await receiver.client.request("pairing/approve",{body:{target:receiver.target,pending_id:pending,approve},proof:receiver.proof()});receiver.fence(generation);if(current()){document.getElementById("cinema-pair-status").textContent=approve?"Approved. The phone must explicitly choose Use as remote.":"Denied";receiver.pairings=receiver.pairings.filter(value=>value.pending_id!==pending);cinemaRemotePairingPrompt(receiver);}}
  catch(error){if(current())document.getElementById("cinema-pair-status").textContent=error.message;}
}
function cinemaRemoteParsePairLink(text,identity=cinemaRemoteIdentity()){
  if(typeof text!=="string"||CinemaRemoteWire.bytes(text)>2048||!identity)throw new Error("Invalid pairing link");
  const url=new URL(text),keys=["server_instance_id","owner_node_id","session_id","receiver_epoch","challenge_id"];
  if(url.protocol!=="cinema-remote:"||url.hostname!=="pair"||url.pathname&&url.pathname!=="/"||url.username||url.password||url.port||Array.from(url.searchParams.keys()).length!==5||keys.some(key=>url.searchParams.getAll(key).length!==1)||url.searchParams.get("server_instance_id")!==identity.instance)throw new Error("Pairing link belongs to another server or is invalid");
  const fragment=new URLSearchParams(url.hash.slice(1));if(Array.from(fragment.keys()).length!==1||fragment.getAll("code").length!==1||!/^[0-9]{8}$/.test(fragment.get("code")||""))throw new Error("Invalid pairing code");
  const target={owner_node_id:url.searchParams.get("owner_node_id"),session_id:url.searchParams.get("session_id"),receiver_epoch:url.searchParams.get("receiver_epoch")},challenge_id=url.searchParams.get("challenge_id");
  if(!CinemaRemoteWire.targetValid(target)||!CinemaRemoteWire.id(challenge_id))throw new Error("Invalid pairing target");return {target,challenge_id,code:fragment.get("code")};
}
function cinemaRemoteCancelPairFlow(){CINEMA_PAIR_GENERATION++;CINEMA_PAIR_FLOW?.client.retire();clearTimeout(CINEMA_PAIR_FLOW?.timer);clearTimeout(CINEMA_PAIR_FLOW?.expiryTimer);CINEMA_PAIR_FLOW=null;}
class CinemaRemotePairFlow{
  constructor(device,{client=new CinemaRemoteClient(),changed=()=>cinemaRemoteUiChanged(),generation=++CINEMA_PAIR_GENERATION}={}){this.device=device;this.client=client;this.changed=changed;this.generation=generation;this.timer=null;this.pending=null;this.message="Enter the TV's eight-digit code";this.deadline=Infinity;this.expiryTimer=null;}
  current(){return CINEMA_PAIR_FLOW===this&&this.generation===CINEMA_PAIR_GENERATION&&this.client.current()&&document.visibilityState==="visible";}
  alive(){return this.current()&&performance.now()<this.deadline;}
  expire(){if(!this.current()||performance.now()<this.deadline)return;clearTimeout(this.timer);clearTimeout(this.expiryTimer);this.client.retire();this.pending=null;this.message="Pairing expired. Show a fresh TV code and enter it here to try again.";this.changed();}
  async claim(code,challenge_id=null){
    if(!this.current()||this.pending||!/^[0-9]{8}$/.test(code)||challenge_id!==null&&!CinemaRemoteWire.id(challenge_id))throw new Error("Invalid or expired pairing code");
    const pending={};this.pending=pending;this.deadline=performance.now()+120000;clearTimeout(this.expiryTimer);this.expiryTimer=setTimeout(()=>this.expire(),120000);
    try{
      const result=await this.client.request("pairing/claim",{body:{target:this.device.target,challenge_id,code,controller_name:"Cinema phone"}});
      if(!this.alive()||this.pending!==pending){this.expire();return;}
      if(!CinemaRemoteWire.id(result.pending_id)||!cinemaRemoteSecret(result.poll_secret))throw new Error("Invalid pairing response");
      this.pending={pending_id:result.pending_id,secret:result.poll_secret};this.message="Waiting for physical approval on the TV";this.changed();await this.result();
    }catch(error){if(this.alive()){this.message=error.code||error.message;clearTimeout(this.expiryTimer);this.pending=null;this.changed();}}
  }
  async result(){
    if(!this.alive()){this.expire();return;}const pending=this.pending;
    try{
      const reply=await this.client.request("pairing/result",{body:{target:this.device.target,pending_id:pending.pending_id},proof:{kind:"pairing",secret:pending.secret}});
      if(!this.alive()||this.pending!==pending){this.expire();return;}
      if(!["pending","denied","approved"].includes(reply.status))throw new Error("Invalid pairing result");
      if(reply.status==="pending"){this.timer=setTimeout(()=>this.result(),1000);return;}
      if(reply.status==="denied"){this.message="TV denied pairing";clearTimeout(this.expiryTimer);this.pending=null;this.changed();return;}
      if(!CinemaRemoteWire.id(reply.grant_id)||!cinemaRemoteSecret(reply.grant_secret)||reply.receiver_id!==this.device.receiver_id)throw new Error("Invalid approved grant");
      const stored=cinemaRemoteLoad(this.client.identity),grant={grant_id:reply.grant_id,grant_secret:reply.grant_secret,receiver_id:reply.receiver_id,name:this.device.name};
      stored.grants=stored.grants.filter(value=>value.receiver_id!==grant.receiver_id);if(stored.grants.length>=20)throw new Error("Forget an old screen before saving another grant");stored.grants.push(grant);cinemaRemoteSave(this.client.identity,stored);
      this.message="Paired. Choose Use as remote to take control.";clearTimeout(this.expiryTimer);this.pending=null;this.changed();
      // A late result cannot reconnect a retired/changed selection, and pairing
      // never acquires control automatically.
      if(this.alive())await cinemaRemoteConnectSelected(this.device,grant);
    }catch(error){if(this.alive()){this.message="Pairing outcome unavailable; show a new code and pair again.";clearTimeout(this.expiryTimer);this.pending=null;this.changed();}}
  }
}

function cinemaRemotePairLocalAction(kind,button){
  if(!CINEMA_PAIR_DIALOG||!document.hasFocus()||document.visibilityState!=="visible"||!button?.isConnected||!CINEMA_PAIR_DIALOG.contains(button))return "unavailable";
  const identity=cinemaRemoteIdentity();if(!identity)return "unavailable";
  if(kind==="close"){cinemaRemoteClosePairDialog();return "applied";}
  if(kind==="enable"){
    try{const preferences=cinemaRemotePreferences(identity);preferences.receiver=true;localStorage.setItem("cinema.remote.preferences:"+cinemaRemoteIdentityKey(identity),JSON.stringify(preferences));cinemaRemoteReceiverSync();document.getElementById("cinema-pair-status").textContent="Receiving enabled on this device. Register this screen, then show its code. Companion remotes must also be enabled by the server administrator.";}catch(error){document.getElementById("cinema-pair-status").textContent=error.message;}return "applied";
  }
  if(kind==="register"){const dialog=CINEMA_PAIR_DIALOG;cinemaRemoteRegisterScreenOwner(identity,"Cinema browser",()=>CINEMA_PAIR_DIALOG===dialog);return "applied";}
  if(kind==="code"){cinemaRemoteShowPairingOwner();return "applied";}
  if(kind==="approve"||kind==="deny"){
    if(document.activeElement!==button)return "stale_focus";
    const receiver=CINEMA_WEB_RECEIVER;if(!receiver?.eligible()||!receiver.challenge||performance.now()>=receiver.challenge.deadline||!receiver.pairings.some(value=>value.pending_id===button.dataset.localPending))return "unavailable";
    cinemaRemoteApproveOwner(button.dataset.localPending,kind==="approve");return "applied";
  }
  return "unsupported";
}
function cinemaRemoteBindLocalPairEntry(root){
  if((location.hash||"#/")!=="#/"||!TOKEN||!ME)return;
  let button=/** @type {HTMLButtonElement|null} */(document.getElementById("cinema-local-pair-entry"));
  if(!button){button=document.createElement("button");button.id="cinema-local-pair-entry";button.type="button";button.className="ghost";button.textContent="Pair a phone";document.getElementById("main").prepend(button);button.onclick=event=>{if(event.isTrusted&&document.hasFocus())cinemaRemotePairDialog(CINEMA_WEB_RECEIVER);};}
  CinemaRemote.registerAction({id:"local:pair-entry",element:button,label:"Pair a phone",localOnly:true,activate:()=>{cinemaRemotePairDialog(CINEMA_WEB_RECEIVER);return "applied";}});
}
const CinemaRemoteLocalPhysical={
  snapshot(){
    if(!CINEMA_PAIR_DIALOG)return {surface:"ordinary",...CinemaRemote.snapshotPhysical()};
    const button=/** @type {HTMLElement|null} */(document.activeElement),receiver=CINEMA_WEB_RECEIVER;
    return {surface:"pairing",generation:CINEMA_PAIR_LOCAL_GENERATION,action:CINEMA_PAIR_DIALOG.contains(button)?button?.dataset.localPair||null:null,pending:button?.dataset.localPending||null,target:receiver?.target?CinemaRemoteWire.targetKey(receiver.target):null,challenge:receiver?.challenge?.challenge_id||null};
  },
  dispatch(action,offered){
    if(!CINEMA_PAIR_DIALOG)return CinemaRemote.dispatchPhysical(action,{...CinemaRemote.snapshotPhysical(),source:"local_cec"});
    if(!document.hasFocus()||document.visibilityState!=="visible")return "unavailable";
    const current=this.snapshot();
    if(action.type==="select"){
      if(JSON.stringify(current)!==JSON.stringify(offered))return "stale_focus";
      return cinemaRemotePairLocalAction(current.action,document.activeElement);
    }
    if(action.type==="back"){cinemaRemoteClosePairDialog();return "applied";}
    if(action.type!=="navigate")return "restricted_surface";
    const buttons=Array.from(/** @type {NodeListOf<HTMLButtonElement>} */(CINEMA_PAIR_DIALOG.querySelectorAll("[data-local-pair]"))).filter(button=>!button.disabled&&cinemaRemoteVisible(button));
    const index=buttons.findIndex(button=>button===document.activeElement),delta=["left","up"].includes(action.direction)?-1:1;
    const next=index<0?buttons.find(button=>button.dataset.localPair==="close"):buttons[Math.max(0,Math.min(buttons.length-1,index+delta))];
    if(!next)return "unavailable";next.focus();next.scrollIntoView({block:"nearest"});return "applied";
  }
};
