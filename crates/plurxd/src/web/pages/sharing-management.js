"use strict";
// Management keeps wire integers exact. This parser is deliberately separate
// from ordinary Local browse JSON: Source IDs remain quoted decimal strings.
function sharingJSON(text){
  let i=0,nodes=0;
  const fail=()=>{throw new Error("Invalid Sharing JSON");};
  const ws=()=>{while(/[ \t\r\n]/.test(text[i]||"x"))i++;};
  function value(depth){
    if(depth>32||++nodes>1000000)fail(); ws(); const c=text[i];
    if(c==='"'){
      const start=i++; let escaped=false;
      while(i<text.length){const ch=text[i++];if(ch==='"'&&!escaped)return JSON.parse(text.slice(start,i));if(ch==='\\'&&!escaped)escaped=true;else escaped=false;}
      return fail();
    }
    if(c==='['){i++;const a=[];ws();if(text[i]===']'){i++;return a;}while(true){a.push(value(depth+1));ws();if(text[i]===']'){i++;return a;}if(text[i++]!==',')fail();}}
    if(c==='{'){
      i++;const o=Object.create(null);ws();if(text[i]==='}'){i++;return o;}
      while(true){ws();if(text[i]!=='"')fail();const k=value(depth+1);if(Object.hasOwn(o,k))fail();ws();if(text[i++]!==':')fail();o[k]=value(depth+1);ws();if(text[i]==='}'){i++;return o;}if(text[i++]!==',')fail();}
    }
    if(text.startsWith("true",i)){i+=4;return true;}if(text.startsWith("false",i)){i+=5;return false;}if(text.startsWith("null",i)){i+=4;return null;}
    const m=/^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/.exec(text.slice(i));
    if(!m) return fail();i+=m[0].length;
    if(!/[.eE]/.test(m[0])){if(m[0].length>21)fail();return BigInt(m[0]);}
    const number=Number(m[0]);if(!Number.isFinite(number))fail();return number;
  }
  const result=value(0);ws();if(i!==text.length)fail();return result;
}
function sharingJSONString(value){
  if(typeof value==="bigint")return sharingInteger(value,0n).toString();
  if(value===null||typeof value==="string"||typeof value==="boolean")return JSON.stringify(value);
  if(Array.isArray(value))return "["+value.map(sharingJSONString).join(",")+"]";
  if(value&&typeof value==="object")return "{"+Object.entries(value).map(([k,v])=>JSON.stringify(k)+":"+sharingJSONString(v)).join(",")+"}";
  throw new Error("Sharing requests require exact typed values");
}
function sharingInteger(v,min=1n){if(typeof v!=="bigint"||v<min||v>9223372036854775807n)throw new Error("Invalid Sharing integer");return v;}
function sharingUUID(v){if(!sharedCatalogueUuid(v))throw new Error("Invalid Sharing identifier");return v;}
function sharingIds(rows){if(!Array.isArray(rows)||rows.length>64||new Set(rows).size!==rows.length||rows.some(v=>!sharedCatalogueId(v)))throw new Error("Invalid Source library scope");return rows;}
function sharingCode(v){if(typeof v!=="string"||!/^[0-9a-f]{16}$/.test(v))throw new Error("Enter the exact 16-character pairing code from the importing Cinema");return v;}
function sharingInvitation(v){if(typeof v!=="string"||!/^cinema-share-v1:[A-Za-z0-9_-]{1,10923}$/.test(v))throw new Error("Paste a valid cinema-share-v1 invitation");return v;}
function sharingIPv6(v){
  if(typeof v!=="string"||! /^[0-9a-fA-F:]+$/.test(v)||v.split("::").length>2)throw new Error("Invalid private Tailnet IPv6");
  const halves=v.split("::"),parts=halves.map(h=>h?h.split(":"):[]);
  if(parts.flat().some(s=>! /^[0-9a-fA-F]{1,4}$/.test(s)))throw new Error("Invalid IPv6 groups");
  const count=parts.flat().length;
  if((halves.length===1&&count!==8)||(halves.length===2&&count>=8))throw new Error("Invalid IPv6 length");
  const words=halves.length===1?parts[0]:[...parts[0],...Array(8-count).fill("0"),...parts[1]];
  if(words.slice(0,3).map(s=>parseInt(s,16)).join(",")!=="64890,4444,41440")throw new Error("IPv6 must be inside fd7a:115c:a1e0::/48");return v;
}
function sharingEndpoint(row){
  if(!row||typeof row.ipv4!=="string")throw new Error("Invalid Tailnet endpoint");
  const p=row.ipv4.split(".");if(p.length!==4||p.some(s=>! /^(0|[1-9][0-9]{0,2})$/.test(s)||Number(s)>255)||p[0]!=="100"||Number(p[1])<64||Number(p[1])>127)throw new Error("IPv4 must be inside 100.64.0.0/10");
  sharingInteger(row.port,1n);if(row.port>65535n)throw new Error("Port must be from 1 to 65535");
  if(typeof row.ts_fqdn!=="string"||!row.ts_fqdn.endsWith(".ts.net"))throw new Error("Enter the serving machine’s full Tailscale DNS name, such as cinema.tail123abc.ts.net, without a URL or port");
  const prefix=row.ts_fqdn.slice(0,-7),labels=prefix.split(".");
  if(labels.length===1&&/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(prefix))throw new Error("That is the tailnet DNS name. Add the serving machine’s name before it, such as cinema."+row.ts_fqdn+". Use the Machine name from Tailscale’s Machines page followed by the Tailnet DNS name from its DNS page.");
  if(prefix.length>240||labels.length<2||labels.some(s=>! /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(s)))throw new Error("Enter the serving machine’s full Tailscale DNS name in lowercase, such as cinema.tail123abc.ts.net, without a URL, port or trailing dot");
  if(typeof row.spki_sha256==="string"&&row.spki_sha256.startsWith("nodekey:"))throw new Error("That is a Tailscale node key. Copy the sharing certificate’s public-key pin from the serving Cinema’s Settings → Sharing page, under Before connecting Cinemas. Removing nodekey: does not make it a Cinema pin.");
  if(typeof row.spki_sha256!=="string"||! /^[0-9a-f]{64}$/.test(row.spki_sha256))throw new Error("Copy the sharing certificate’s 64-character lowercase public-key pin from the serving Cinema’s Settings → Sharing page, under Before connecting Cinemas");
  if(row.ipv6!=null)sharingIPv6(row.ipv6);
  return {ipv4:row.ipv4,ipv6:row.ipv6??null,ts_fqdn:row.ts_fqdn,port:row.port,spki_sha256:row.spki_sha256};
}
function sharingEndpoints(rows){if(!Array.isArray(rows)||rows.length<1||rows.length>4)throw new Error("Enter from one to four Tailnet endpoints");return rows.map(sharingEndpoint);}
function sharingImport(row){
  const r=row?.import;if(!r)throw new Error("Invalid import");
  for(const k of ["id","source_server_id","catalogue_epoch","claim_id"])sharingUUID(r[k]);
  if(r.remote_grant_id!=null)sharingUUID(r.remote_grant_id);
  if(!["claiming","pending","active","disabled","revoked"].includes(r.state)||(r.state==="active"&&!r.remote_grant_id))throw new Error("Invalid import state");
  for(const k of ["assignment_generation","lifecycle_generation","endpoint_generation"])sharingInteger(r[k]);
  if(r.observed_endpoint_revision!=null)sharingInteger(r.observed_endpoint_revision,0n);
  sharingEndpoints(r.endpoints);sharingCode(row.pairing_code);return row;
}
function sharingExport(row){
  const g=row?.grant;if(!g)throw new Error("Invalid export");
  for(const v of [g.id,g.recipient_server_id,row.invitation_id,row.claim_id])sharingUUID(v);
  for(const k of ["scope_generation","credential_generation","catalogue_generation","mutation_generation"])sharingInteger(g[k]);
  if(!["pending","active","disabled","revoked"].includes(g.state))throw new Error("Invalid export state");
  sharingIds(row.library_ids);sharingCode(row.pairing_code);return row;
}
function sharingRows(value,key,validate,max=32){const rows=value?.[key];if(!Array.isArray(rows)||rows.length>max)throw new Error("Invalid Sharing list");rows.forEach(validate);if(new Set(rows.map(r=>key==="imports"?r.import.id:r.grant.id)).size!==rows.length)throw new Error("Duplicate Sharing rows");return value;}
function sharingAssignments(groups){sharingIds(groups.map(g=>g.library_id));for(const g of groups){if(!Array.isArray(g.user_ids)||g.user_ids.length>256||new Set(g.user_ids).size!==g.user_ids.length)throw new Error("Invalid viewer assignments");g.user_ids.forEach(id=>sharingInteger(id,0n));}return groups;}
function sharingBoundSnapshot(snapshot,row){
  const r=row.import;
  if(snapshot?.import_id!==r.id||snapshot.server_id!==r.source_server_id||snapshot.catalogue_epoch!==r.catalogue_epoch||snapshot.lifecycle_generation!==r.lifecycle_generation||snapshot.expected_assignment_generation!==r.assignment_generation||snapshot.state!==r.state)throw new Error("Import changed. Reload before editing.");
  return snapshot;
}
function sharingMatrix(snapshot,scope,viewers,row){
  sharingBoundSnapshot(snapshot,row);sharingBoundSnapshot(scope,row);if(scope.state!=="active")throw new Error("Source must be active");
  if(!Array.isArray(scope.libraries))throw new Error("Source scope unavailable");sharingIds(scope.libraries.map(l=>l.library_id));
  scope.libraries.forEach(l=>{if(typeof l.name!=="string"||new TextEncoder().encode(l.name).length>256||!["movies","shows"].includes(l.kind)||typeof l.anime!=="boolean")throw new Error("Invalid Source library");});
  sharingAssignments(snapshot.assignments);sharingLocalRows(viewers,"username");
  const groups=snapshot.assignments.map(g=>({library_id:g.library_id,user_ids:[...g.user_ids]}));
  const libraries=scope.libraries.map(l=>({...l,outside:false})),ids=new Set(libraries.map(l=>l.library_id));
  for(const g of groups)if(!ids.has(g.library_id))libraries.push({library_id:g.library_id,name:"Outside current Source scope · "+g.library_id,outside:true});
  const users=viewers.map(v=>({...v})),known=new Set(users.map(v=>v.id));
  for(const id of new Set(groups.flatMap(g=>g.user_ids)))if(!known.has(id))users.push({id,username:"Unavailable viewer · "+id,is_admin:false});
  return {snapshot,groups,libraries,viewers:users};
}
function sharingLocalRows(rows,label){if(!Array.isArray(rows)||rows.length>4096||new Set(rows.map(r=>r.id)).size!==rows.length)throw new Error("Invalid local management list");rows.forEach(r=>{sharingInteger(r.id,0n);if(typeof r[label]!=="string")throw new Error("Invalid local label");});return rows;}
let SHARING_MANAGEMENT=null,SHARING_AUTH_EPOCH=0;
function sharingCapture(){if(API!=="/api/v1"||!/^https?:$/.test(new URL(location.origin).protocol))throw new Error("Invalid B management origin");return {epoch:SHARING_AUTH_EPOCH,auth:AUTH_GENERATION,token:TOKEN,origin:API,bOrigin:location.origin,route:location.hash,page:PAGE_RENDER_GENERATION};}
function sharingCurrent(c){return !!TOKEN&&c.epoch===SHARING_AUTH_EPOCH&&(!c.work||(SHARING_MANAGEMENT===c.state&&c.state.revision===c.stateRevision&&c.state.editor===c.editor&&(!c.editor||c.editor.revision===c.editRevision)))&&c.auth===AUTH_GENERATION&&c.token===TOKEN&&c.origin===API&&c.bOrigin===location.origin&&c.route===location.hash&&c.page===PAGE_RENDER_GENERATION;}
function sharingRetire(reason=""){
  SHARING_AUTH_EPOCH++;
  if(SHARING_MANAGEMENT){const e=SHARING_MANAGEMENT.editor;if(e){e.text="";e.row=null;e.endpoints=[];e.matrix=null;e.selected=[];e.libraries=[];e.ready=false;}for(const r of [...SHARING_MANAGEMENT.imports,...SHARING_MANAGEMENT.exports])r.pairing_code="";if(SHARING_MANAGEMENT.invitation)SHARING_MANAGEMENT.invitation.invitation="";SHARING_MANAGEMENT.alive=false;SHARING_MANAGEMENT.revision++;SHARING_MANAGEMENT.editor=null;SHARING_MANAGEMENT.invitation=null;SHARING_MANAGEMENT.imports=[];SHARING_MANAGEMENT.exports=[];SHARING_MANAGEMENT=null;}
  if(typeof SETTINGS_DATA!=="undefined"){delete SETTINGS_DATA.sharingImports;delete SETTINGS_DATA.sharingExports;delete SETTINGS_DATA.sharingStatus;for(const k of ["sharingImports","sharingExports","sharingStatus"]){SETTINGS_LOADED.delete(k);if(typeof SETTINGS_LOADS!=="undefined")SETTINGS_LOADS.delete(k);}}
  if(typeof document!=="undefined"){const node=document.getElementById("sharing-management");if(node){node.replaceChildren();if(reason)node.textContent=reason;}}
}
function sharingRouteChanged(){if(!/^#\/settings\/sharing(?:\/|$)/.test(location.hash)||(SHARING_MANAGEMENT&&!sharingCurrent(SHARING_MANAGEMENT.capture)))sharingRetire();}
async function sharingBody(res,limit,capture){
  const length=res.headers?.get("content-length");if(length!=null&&(!/^[0-9]{1,9}$/.test(length)||BigInt(length)>BigInt(limit)))throw new Error("Sharing response exceeds its bound");
  const reader=res.body?.getReader();if(!reader)throw new Error("Sharing response stream unavailable");
  const chunks=[];let size=0;
  try{while(true){const {value,done}=await reader.read();if(!sharingCurrent(capture))throw new Error("Sharing authorization changed");if(done)break;size+=value.byteLength;if(size>limit)throw new Error("Sharing response exceeds its bound");chunks.push(value);}}
  catch(error){await reader.cancel().catch(()=>{});throw error;}
  const bytes=new Uint8Array(size);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.byteLength;}
  return sharingJSON(new TextDecoder("utf-8",{fatal:true}).decode(bytes));
}
async function sharingRequest(path,{method="GET",body=null,capture=sharingCapture()}={}){
  if(!sharingCurrent(capture))throw new Error("Sharing authorization changed");
  const parts=path.split("?");if(parts.length>2)throw new Error("Invalid management route");
  const segments=parts[0].split("/").slice(1);
  let allowed=["/libraries","/users","/sharing/status","/sharing/imports","/sharing/exports","/sharing/invitations","/sharing/endpoints"].includes(parts[0]);
  if(segments[0]==="sharing"&&["imports","exports","invitations"].includes(segments[1])&&segments.length>=3&&segments.length<=4){
    sharingUUID(segments[2]);allowed=segments.length===3||(segments[1]==="imports"&&["re-pair","rotate","assignments","libraries","endpoints"].includes(segments[3]))||(segments[1]==="exports"&&["approve","libraries"].includes(segments[3]));
  }
  if(!allowed)throw new Error("Invalid management route");
  if(parts.length===2){if(parts[0]!=="/sharing/exports"||!/^after=[0-9a-f-]+$/.test(parts[1]))throw new Error("Invalid management cursor");sharingUUID(parts[1].slice(6));}

  const headers={authorization:"Bearer "+capture.token},floor=readAfterRequest();if(floor.index)headers["x-plurx-read-after"]=floor.index;
  const serialized=body===null?null:sharingJSONString(body);if(serialized!==null){if(new TextEncoder().encode(serialized).length>16384)throw new Error("Sharing update exceeds 16 KiB. No request sent.");headers["content-type"]="application/json";}
  if(method!=="GET"&&capture.work&&capture.editor)capture.editor.sent=true;
  let res;try{res=await fetch(capture.origin+path,{method,headers,body:serialized,redirect:"error"});}catch(error){if(method!=="GET")forgetReadAfter(floor);throw error;}
  if(!sharingCurrent(capture)){if(res.body)await res.body.cancel().catch(()=>{});throw new Error("Sharing authorization changed");}
  const expected=new URL(capture.origin+path,capture.bOrigin).href;if(res.redirected||(res.url&&res.url!==expected))throw new Error("Sharing response changed B origin");
  const index=res.headers?.get("x-plurx-commit-index")??null;if(index==null&&method!=="GET")forgetReadAfter(floor);else observeReadAfter(index,floor);
  if(res.status===401||res.status===403){sharingRetire("Sharing authorization refused. Leave and reopen Sharing after restoring access.");if(res.status===401)clearLocalSession(capture.auth,"Your session ended on the server.");const error=new Error("Sharing authorization refused");Object.assign(error,{status:res.status});throw error;}
  const limit=path==="/libraries"||path==="/users"||path.endsWith("/assignments")?4194304:131072;
  const data=await sharingBody(res,limit,capture);if(!sharingCurrent(capture))throw new Error("Sharing authorization changed");
  if(!res.ok){const error=new Error(typeof data?.message==="string"?data.message:typeof data?.error==="string"?data.error:"Sharing request refused. Reload before trying again.");Object.assign(error,{status:res.status,code:data?.code});throw error;}
  return data;
}
async function sharingManagementRead(path,capture=sharingCapture()){
  const value=await sharingRequest(path,{capture});
  if(path==="/sharing/status"){
    try{return {...value,address_manifest:await sharingReadManifest(capture)};}
    catch(error){if(!sharingCurrent(capture))throw error;return {...value,address_manifest_error:error.message};}
  }
  return path==="/sharing/imports"?sharingRows(value,"imports",sharingImport):path==="/sharing/exports"?sharingRows(value,"exports",sharingExport):value;
}
// Transient form state is page/account scoped; successful mutations require a
// fresh read before another edit. No request is automatically replayed.
function sharingManagementPanel(data){
  sharingRows(data.sharingImports,"imports",sharingImport);sharingRows(data.sharingExports,"exports",sharingExport);
  if(!SHARING_MANAGEMENT||!sharingCurrent(SHARING_MANAGEMENT.capture))SHARING_MANAGEMENT={capture:sharingCapture(),alive:true,revision:0,editor:null,invitation:null,imports:data.sharingImports.imports,exports:data.sharingExports.exports,next:data.sharingExports.next??null,error:"",busy:false};
  SHARING_MANAGEMENT.status=data.sharingStatus;
  return setHead("Sharing","Share selected libraries with another Cinema.","")+setCard(`<div id="sharing-management">${sharingManagementHTML()}</div>`)+setCard(`${cardHead("This node","Readiness is advisory. The saved switch remains available in Developer.","")}<p>${esc(data.sharingStatus?.listener||"Readiness unavailable")}</p>${sharingRecoveryHTML(data.sharingStatus)}<a href="#/settings/developer/cinema-sharing-settings">Enable shared libraries in Developer</a>`);
}
// Orphaned shared-playback routes this node recovers after a crash. An
// observation of this node only; a stranded route keeps its exact lineage.
function sharingRecoveryHTML(status){
  const r=status&&status.receiver_recovery;if(!r||typeof r!=="object")return "";
  const scanned=r.last_scan_at_ms==null?"not yet scanned":"last scan "+new Date(Number(r.last_scan_at_ms)).toLocaleTimeString();
  const stranded=Array.isArray(r.stranded)?r.stranded:[],flight=Array.isArray(r.in_flight)?r.in_flight.length:0;
  return `<p>Playback recovery: ${esc(scanned)} · ${esc(String(r.retired_total??0))} retired · ${esc(String(flight))} in progress · ${esc(String(stranded.length))} stranded</p>`+(stranded.length?`<ul>${stranded.map(s=>`<li>${esc(String(s.incarnation_id))}: ${esc(String(s.reason).replaceAll("_"," "))}</li>`).join("")}</ul>`:"");
}
function sharingRequirementsHTML(status){
  const known=status&&Object.hasOwn(status,"address_manifest"),manifest=known?status.address_manifest:null;
  const pin=status?.certificate?.spki_sha256,certificate=typeof pin==="string"&&/^[0-9a-f]{64}$/.test(pin);
  return `<section><h3>Before connecting Cinemas</h3><p>Both Cinemas need a private Tailscale connection and TCP Serve forwarding to the sharing TLS listener. Enabling sharing or saving addresses does not install or verify that host network setup.</p><ul><li>Local sharing TLS listener: ${status?.listener==="listening"?"Listening on this serving node":"Not confirmed listening"}${status?.listener?" · "+esc(status.listener.replaceAll("_"," ")):""}. This does not prove another Cinema can connect.</li><li>Sharing certificate: ${certificate?"Observed on this serving node · public-key pin <code>"+esc(pin)+"</code>":"Not observed on this serving node"}. Match each configured address to that host’s certificate.</li><li>This Cinema’s addresses: ${known?(manifest===null?"Missing — configure addresses before creating an invitation":"Configured — reachability has not been verified"):"Not verified — address information could not be read"}.</li><li>Private connection between Cinemas: Not verified by this Cinema. Check that each serving host has Tailscale installed and signed in, with private TCP forwarding configured. A local listener cannot confirm this host setup or remote connectivity.</li></ul><p>If the private connection is not configured, set up Tailscale and TCP Serve on each serving host first. Then enter its private addresses and verify its sharing certificate pin.</p>${SHARING_MANAGEMENT?.editor?.mode==="manifest"?"":sharingButton("Configure this Cinema’s addresses",SHARING_MANAGEMENT?.editor?.mode==="invite"?"sharingConfigureAddresses()":"sharingOpen('manifest')",SHARING_MANAGEMENT?.busy)}<p>The Developer sharing switch remains available while setup is incomplete.</p></section>`;
}
function sharingButton(label,action,disabled=false){return `<button class="ghost sm" onclick="${action}"${disabled?" disabled":""}>${esc(label)}</button>`;}
function sharingInput(label,key,value,kind="text"){
  const e=SHARING_MANAGEMENT.editor;return `<label>${esc(label)}<input type="${kind}" value="${esc(value||"")}" autocomplete="off" autocapitalize="none" spellcheck="false" oninput="sharingEdit('${key}',this.value)"${e.busy?" disabled":""}></label>`;
}
function sharingManagementHTML(){
  const s=SHARING_MANAGEMENT;if(!s)return "";
  if(s.editor)return sharingRequirementsHTML(s.status)+sharingEditorHTML(s.editor);
  const disabled=s.busy;
  return sharingRequirementsHTML(s.status)+`<h3>Connect Cinemas</h3>${sharingButton("Create invitation","sharingOpen('invite')",disabled)} ${sharingButton("Import invitation","sharingOpen('import')",disabled)} ${sharingButton("This Cinema's endpoints","sharingOpen('manifest')",disabled)} ${sharingButton("Reload","sharingReload()",disabled)}${s.error?`<p role="alert">${esc(s.error)}</p>`:""}${s.invitation?`<section id="sharing-invitation-result" class="sharing-invitation" tabindex="-1"><h4>Invitation created</h4><p role="status">Your invitation is ready.</p><p>Copy the invitation below. On the other Cinema, open Settings → Sharing → Import invitation and paste it there. This secret is cleared when you leave Sharing.</p><textarea id="sharing-invitation-text" aria-label="Invitation to copy" readonly autocomplete="off" spellcheck="false" onfocus="this.select()">${esc(s.invitation.invitation)}</textarea><p role="status" aria-live="polite">${esc(s.invitationCopyNotice||"")}</p>${sharingButton("Copy invitation","sharingCopyInvitation()",disabled)} ${sharingButton("Cancel invitation","sharingCancelInvitation()",disabled)} ${sharingButton("Clear secret","sharingClearInvitation()",disabled)}</section>`:""}<h3>Imports</h3>${s.imports.map((row,i)=>`<section><h4>${esc(row.import.source_name||"Shared source")}</h4><p>${esc(row.import.state)} · Source ${esc(row.import.source_server_id)}</p>${sharingButton("Pairing code",`sharingOpen('code',${i})`,disabled)} ${sharingButton("Viewer assignments",`sharingOpen('matrix',${i})`,disabled||row.import.state!=="active")} ${sharingButton("Source endpoints",`sharingOpen('endpoints',${i})`,disabled||!["claiming","pending","active"].includes(row.import.state))} ${sharingButton("Re-pair",`sharingOpen('repair',${i})`,disabled)} ${sharingButton("Rotate credential",`sharingOpen('rotate',${i})`,disabled||row.import.state!=="active")} ${sharingButton("Disconnect",`sharingOpen('disconnect',${i})`,disabled)}</section>`).join("")||'<p class="muted">No imports.</p>'}<h3>Exports</h3>${s.exports.map((row,i)=>`<section><h4>${esc(row.recipient_name||"Recipient")}</h4><p>${esc(row.grant.state)} · Recipient ${esc(row.grant.recipient_server_id)}</p>${sharingButton("Approve pairing",`sharingOpen('approve',${i})`,disabled||row.grant.state!=="pending")} ${sharingButton("Library scope",`sharingOpen('scope',${i})`,disabled||row.grant.state!=="active")} ${sharingButton("Revoke",`sharingOpen('revoke',${i})`,disabled)}</section>`).join("")||'<p class="muted">No exports.</p>'}${s.next?sharingButton("More exports","sharingMoreExports()",disabled):""}`;
}
function sharingPaint(){if(!SHARING_MANAGEMENT||!sharingCurrent(SHARING_MANAGEMENT.capture))return;const node=document.getElementById("sharing-management");if(node)node.innerHTML=sharingManagementHTML();}
function sharingEdit(key,value){const s=SHARING_MANAGEMENT,e=s?.editor;if(!e||e.busy||!sharingCurrent(s.capture))return;e[key]=value;e.revision++;}
function sharingCloseEditor(){const s=SHARING_MANAGEMENT;if(!s)return;if(s.editor?.returnInvite)return sharingReturnInvitation();s.revision++;s.editor=null;sharingPaint();}
async function sharingCopyInvitation(){
  const s=SHARING_MANAGEMENT,invitation=s?.invitation;if(!invitation||!sharingCurrent(s.capture))return;
  let copied=false;
  try{await navigator.clipboard.writeText(invitation.invitation);copied=true;}catch{}
  if(SHARING_MANAGEMENT!==s||s.invitation!==invitation||!sharingCurrent(s.capture))return;
  s.invitationCopyNotice=copied?"Invitation copied. Paste it into Import invitation on the other Cinema.":"Automatic copy is unavailable. Select the invitation text and copy it using your browser’s Copy command.";
  sharingPaint();
  if(!copied){const input=/** @type {HTMLTextAreaElement|null} */ (document.getElementById("sharing-invitation-text"));input?.focus?.();input?.select?.();}
}
function sharingClearInvitation(){if(SHARING_MANAGEMENT){SHARING_MANAGEMENT.invitation=null;sharingPaint();}}
async function sharingWork(work,action=""){
  const s=SHARING_MANAGEMENT;if(!s||s.busy||!sharingCurrent(s.capture))return;
  const e=s.editor,revision=e?e.revision:s.revision;s.requestCapture={...s.capture,work:true,state:s,stateRevision:s.revision,editor:e,editRevision:e?.revision};s.busy=true;if(e){e.busy=true;e.sent=false;e.action=action;if(action==="save")e.error="";}sharingPaint();
  const accepts=()=>SHARING_MANAGEMENT===s&&s.alive&&sharingCurrent(s.capture)&&(e?s.editor===e&&e.revision===revision:s.revision===revision);
  try{const result=await work(s,e);if(accepts())return result;}
  catch(error){if(accepts()){if(e){e.error=error.message;if(e.sent)e.ready=false;}else s.error=error.message;}}
  finally{
    if(SHARING_MANAGEMENT===s&&s.alive&&sharingCurrent(s.capture)){
      s.busy=false;let focus=null;
      if(s.editor===e&&e){
        e.busy=false;
        if(action==="save"&&e.mode==="invite"&&e.saved&&s.invitation){s.editor=null;s.revision++;focus="sharing-invitation-result";}
        else if(action==="save"&&["manifest","endpoints","invite"].includes(e.mode)&&(e.saved||e.error))focus="sharing-editor-feedback";
      }
      sharingPaint();if(focus)document.getElementById(focus)?.focus?.();
    }
  }
}
async function sharingOpen(mode,index=0,draft=null){
  const s=SHARING_MANAGEMENT;if(!s||s.busy||!sharingCurrent(s.capture))return;
  const row=["scope","approve","revoke"].includes(mode)?s.exports[index]:s.imports[index];
  if(!["invite","import","manifest"].includes(mode)&&!row)return;
  s.revision++;s.editor={mode,row,revision:0,ready:false,busy:false,error:"",text:"",selected:[],libraries:[],matrix:null,endpoints:[],manifestRevision:0n,needsEndpoints:false,returnInvite:mode==="manifest"?draft:null};
  if(mode==="invite"&&draft)s.editor.selected=[...draft.selected];
  if(["import","repair","approve","rotate","disconnect","revoke","code"].includes(mode)){s.editor.ready=true;sharingPaint();return;}
  await sharingEditorReload();
}
async function sharingEditorReload(){
  await sharingWork(async(s,e)=>{
    e.ready=false;
    if(e.mode==="invite"||e.mode==="scope"){
      const rows=sharingLocalRows(await sharingRequest("/libraries",{capture:s.requestCapture}),"name").filter(r=>["movies","shows"].includes(r.kind));
      e.libraries=rows.map(r=>({library_id:r.id.toString(),name:r.name,outside:false}));
      if(e.mode==="scope"){
        e.row=await sharingFreshExport(e.row.grant.id,s.requestCapture);if(e.row.grant.state!=="active")throw new Error("Export must be active to edit scope");
      }
      e.selected=e.mode==="scope"?[...e.row.library_ids]:[...e.selected];
      if(e.mode==="invite")e.needsEndpoints=(await sharingReadManifest(s.requestCapture))===null;
      const existing=new Set(e.libraries.map(l=>l.library_id));for(const id of e.selected)if(!existing.has(id))e.libraries.push({library_id:id,name:"Unavailable local library · "+id,outside:true});
    }else if(["matrix","endpoints"].includes(e.mode)){
      const all=sharingRows(await sharingRequest("/sharing/imports",{capture:s.requestCapture}),"imports",sharingImport);
      const row=all.imports.find(r=>r.import.id===e.row.import.id);
      if(!row||row.import.source_server_id!==e.row.import.source_server_id||row.import.catalogue_epoch!==e.row.import.catalogue_epoch)throw new Error("Source identity changed");
      e.row=row;
      if(e.mode==="matrix"){
        const base="/sharing/imports/"+sharingUUID(row.import.id);
        const [snapshot,scope,viewers]=await Promise.all([sharingRequest(base+"/assignments",{capture:s.requestCapture}),sharingRequest(base+"/libraries",{capture:s.requestCapture}),sharingRequest("/users",{capture:s.requestCapture})]);
        if(!sharingCurrent(s.requestCapture))throw new Error("Sharing edit changed");e.matrix=sharingMatrix(snapshot,scope,viewers,row);e.library=e.matrix.libraries[0]?.library_id||"";e.search="";
      }else{if(!["claiming","pending","active"].includes(row.import.state))throw new Error("Source is not accepting endpoint edits");e.endpoints=row.import.endpoints.map(sharingEndpoint);}
    }else if(e.mode==="manifest"){
      const m=await sharingReadManifest(s.requestCapture);
      e.manifestRevision=m?.revision??0n;e.endpoints=m?m.endpoints.map(sharingEndpoint):[sharingBlankEndpoint(true)];
    }
    e.error="";e.ready=true;e.saved=false;
  });
}
async function sharingReadManifest(capture){
  try{
    const value=await sharingRequest("/sharing/endpoints",{capture});
    if(!value||!Object.hasOwn(value,"manifest"))throw new Error("Cinema address information is unavailable. Reload before creating an invitation.");
    const m=value.manifest;if(m!==null){sharingInteger(m.revision);sharingEndpoints(m.endpoints);}
    if(SHARING_MANAGEMENT&&sharingCurrent(capture)){SHARING_MANAGEMENT.status={...SHARING_MANAGEMENT.status,address_manifest:m};delete SHARING_MANAGEMENT.status.address_manifest_error;}return m;
  }catch(error){
    if(SHARING_MANAGEMENT&&sharingCurrent(capture)){SHARING_MANAGEMENT.status={...SHARING_MANAGEMENT.status,address_manifest_error:error.message};delete SHARING_MANAGEMENT.status.address_manifest;}
    throw error;
  }
}
async function sharingConfigureAddresses(){
  const s=SHARING_MANAGEMENT,e=s?.editor;if(!e||e.mode!=="invite"||e.busy||s.busy)return;
  await sharingOpen("manifest",0,{selected:[...e.selected]});
}
async function sharingReturnInvitation(){
  const s=SHARING_MANAGEMENT,e=s?.editor;if(!e?.returnInvite||e.busy||s.busy)return;
  await sharingOpen("invite",0,e.returnInvite);
}
function sharingBlankEndpoint(useServingNodePin=false){
  const pin=SHARING_MANAGEMENT?.status?.certificate?.spki_sha256;
  const local=useServingNodePin&&typeof pin==="string"&&/^[0-9a-f]{64}$/.test(pin);
  return {ipv4:"",ipv6:null,ts_fqdn:"",port:32443n,spki_sha256:local?pin:"",pin_from_this_node:local};
}
function sharingSelect(index,checked){const e=SHARING_MANAGEMENT?.editor;if(!e||e.busy)return;const id=e.libraries[index]?.library_id;if(!id)return;const selected=new Set(e.selected);if(checked)selected.add(id);else selected.delete(id);e.selected=[...selected];e.revision++;}
function sharingEndpointDNSParts(row){
  if(row.dns_parts)return {...row.dns_parts};
  const labels=row.ts_fqdn.split(".");
  return {machine_name:labels.length>3?labels.slice(0,-3).join("."):"",tailnet_dns_name:labels.length>=3?labels.slice(-3).join("."):""};
}
function sharingEndpointEdit(index,key,value){
  const e=SHARING_MANAGEMENT?.editor;if(!e||e.busy||!e.endpoints[index])return;
  const row=e.endpoints[index];
  if(key==="machine_name"||key==="tailnet_dns_name"){
    const parts=sharingEndpointDNSParts(row);parts[key]=value;row.dns_parts=parts;
    row.ts_fqdn=parts.machine_name.trim().toLowerCase()+"."+parts.tailnet_dns_name.trim().toLowerCase().replace(/\.$/,"");
    const preview=document.getElementById("sharing-dns-preview-"+index);
    if(preview)preview.textContent=parts.machine_name.trim()&&parts.tailnet_dns_name.trim()?row.ts_fqdn:"Enter both names above";
  }else{row[key]=key==="ipv6"?(value||null):value;if(key==="ts_fqdn")delete row.dns_parts;if(key==="spki_sha256")row.pin_from_this_node=false;}
  e.revision++;
}
function sharingEndpointFieldsHTML(row,index,busy){
  const parts=sharingEndpointDNSParts(row);
  const field=(key,label,hint,placeholder="")=>`<label>${label}<br><span class="hint">${esc(hint)}</span><input value="${esc(String(parts[key]??row[key]??""))}" placeholder="${esc(placeholder)}" autocomplete="off" autocapitalize="none" spellcheck="false" oninput="sharingEndpointEdit(${index},'${key}',this.value)"${busy?" disabled":""}></label>`;
  return `<fieldset><legend>Endpoint ${index+1}</legend><h4>1. Copy these values from Tailscale</h4>`+
    field("ipv4","Tailscale IPv4","Tailscale → Machines → select the serving machine → Tailscale IPv4.","100.64.0.1")+
    field("ipv6","Tailscale IPv6 (optional)","Copy Tailscale IPv6 from the same machine page, or leave blank.")+
    field("machine_name","Machine name","Tailscale → Machines → select the serving machine → Machine name. Copy just the machine name.","cinema")+
    field("tailnet_dns_name","Tailnet DNS name","Tailscale → DNS → Tailnet DNS name. Copy the entire value ending in .ts.net.","tail123abc.ts.net")+
    `<p>Cinema joins the two names automatically: <code>cinema</code> + <code>tail123abc.ts.net</code> → <code>cinema.tail123abc.ts.net</code>.</p><p>Full machine address: <code id="sharing-dns-preview-${index}">${esc(parts.machine_name&&parts.tailnet_dns_name?row.ts_fqdn:"Enter both names above")}</code></p><p class="hint">Enable MagicDNS in Tailscale → DNS to resolve machine names.</p><h4>2. Enter the Cinema connection port</h4>`+
    field("port","Private connection port","Use the port configured for Tailscale TCP Serve, normally 32443. The internal Cinema TLS port is 32444.","32443")+
    `<h4>3. Cinema certificate</h4>`+
    field("spki_sha256","Cinema sharing certificate pin",row.pin_from_this_node?"Filled automatically from the Cinema serving this page. If you are configuring a different serving machine, replace this with the pin from that machine’s Cinema Sharing page.":"On the serving Cinema, open Settings → Sharing → Before connecting Cinemas → Sharing certificate. Copy the 64-character value after public-key pin. Tailscale’s Node key is a different key; removing nodekey: does not make it a Cinema pin.","64 characters from Cinema’s Sharing page")+
    sharingButton("Remove",`sharingEndpointCount(false,${index})`,busy||SHARING_MANAGEMENT.editor.endpoints.length<=1)+`</fieldset>`;
}
function sharingEndpointCount(add,index=0){const e=SHARING_MANAGEMENT?.editor;if(!e||e.busy)return;if(add&&e.endpoints.length<4)e.endpoints.push(sharingBlankEndpoint());if(!add&&e.endpoints.length>1)e.endpoints.splice(index,1);e.revision++;sharingPaint();}
function sharingMatrixEdit(id,checked){const e=SHARING_MANAGEMENT?.editor;if(!e||e.busy||!e.matrix)return;const user=BigInt(id),m=e.matrix;if(!m.viewers.some(v=>v.id===user)||!m.libraries.some(l=>l.library_id===e.library))return;let group=m.groups.find(g=>g.library_id===e.library);if(!group){group={library_id:e.library,user_ids:[]};m.groups.push(group);}const ids=new Set(group.user_ids);if(checked)ids.add(user);else ids.delete(user);const replacement=[...ids];if(replacement.length>256){e.error="A library may have at most 256 viewers";sharingPaint();return;}group.user_ids=replacement;e.revision++;}
function sharingRemoveOutside(){const e=SHARING_MANAGEMENT?.editor;if(!e||e.busy||!e.matrix?.libraries.some(l=>l.library_id===e.library&&l.outside))return;e.matrix.groups=e.matrix.groups.filter(g=>g.library_id!==e.library);e.revision++;sharingPaint();}
function sharingEditorHTML(e){
  const titles={invite:"Create invitation",import:"Import invitation",repair:"Re-pair Source",approve:"Approve pairing",scope:"Export library scope",matrix:"Viewer assignments",manifest:"This Cinema's addresses",endpoints:"Source endpoints",rotate:"Rotate Source credential",disconnect:"Disconnect Source",revoke:"Revoke export",code:"Pairing code"};
  let content="";
  if(e.mode==="code")content=`<p>Enter this code on the Source Cinema to approve this recipient:</p><code>${esc(e.row.pairing_code)}</code>`;
  else if(["import","repair"].includes(e.mode))content=sharingInput("Secret invitation","text",e.text);
  else if(e.mode==="approve")content=`<p>Compare the importing Cinema's pairing code and type it here. Approval is never automatic.</p>`+sharingInput("Pairing code","text",e.text);
  else if(["invite","scope"].includes(e.mode))content=(e.mode==="invite"?`<p>${e.needsEndpoints?"Set up this Cinema’s private addresses before creating an invitation. Your library selection is kept while you configure them.":"Saved addresses are included in the invitation. Saving them does not verify that another Cinema can connect."}</p>${sharingButton("Configure this Cinema’s addresses","sharingConfigureAddresses()",e.busy)}`:"")+e.libraries.map((l,i)=>`<label><input type="checkbox" onchange="sharingSelect(${i},this.checked)"${e.selected.includes(l.library_id)?" checked":""}${e.busy?" disabled":""}> ${esc(l.name)} · ${esc(l.library_id)}</label>`).join("");
  else if(e.mode==="matrix"&&e.matrix){
    const m=e.matrix,users=m.viewers.filter(v=>v.username.toLowerCase().includes((e.search||"").toLowerCase())||v.id.toString().includes(e.search||""));
    const group=m.groups.find(g=>g.library_id===e.library);
    content=`<p>The complete matrix is retained. Unavailable viewers and libraries outside current Source scope remain until you explicitly remove them.</p><label>Source library<select onchange="sharingEdit('library',this.value);sharingPaint()"${e.busy?" disabled":""}>${m.libraries.map(l=>`<option value="${esc(l.library_id)}"${l.library_id===e.library?" selected":""}>${esc(l.name)}</option>`).join("")}</select></label>`+sharingInput("Find viewers","search",e.search)+sharingButton("Find","sharingPaint()",e.busy)+users.slice(0,100).map(v=>`<label><input type="checkbox" onchange="sharingMatrixEdit('${v.id}',this.checked)"${group?.user_ids.includes(v.id)?" checked":""}${e.busy?" disabled":""}>${esc(v.username)} · ${v.id}</label>`).join("")+(users.length>100?`<p>Showing 100 of ${users.length} matching viewers. Refine the search; the complete matrix is retained.</p>`:"")+(m.libraries.some(l=>l.library_id===e.library&&l.outside)?sharingButton("Remove this outside-scope group","sharingRemoveOutside()",e.busy):"");
  }else if(["manifest","endpoints"].includes(e.mode)){
    content=`<p>${e.mode==="manifest"?"Configure the machine serving this Cinema’s libraries.":"Use the details of the other Cinema providing the libraries."} The addresses come from Tailscale. ${e.mode==="manifest"?"Cinema fills in this serving node’s certificate pin for its first endpoint when available.":"Use the certificate pin from the Cinema providing the libraries."} Saving addresses does not check connectivity or configure Tailscale.</p>`+e.endpoints.map((row,i)=>sharingEndpointFieldsHTML(row,i,e.busy||e.saved)).join("")+sharingButton("Add endpoint","sharingEndpointCount(true)",e.busy||e.saved||e.endpoints.length>=4)+`<p>Saving trusts the Cinema certificate pins entered above. Check that they match the serving Cinema’s Sharing page.</p>`;
  }else if(["rotate","disconnect","revoke"].includes(e.mode))content=`<p>${e.mode==="rotate"?"Replace this Source credential?":e.mode==="disconnect"?"Disconnect this Source and stop using its shared libraries?":"Revoke this recipient's library access?"}</p>`;
  const addresses=["manifest","endpoints"].includes(e.mode),inviting=e.mode==="invite",feedbackAtEnd=addresses||inviting,saving=e.busy&&e.action==="save";
  const error=e.error?`<p role="alert">${addresses&&!e.saved?(e.sent?"Save not confirmed. ":"Addresses not saved. "):inviting?(e.sent?"Invitation creation not confirmed. ":"Invitation not created. "):""}${esc(e.error)}</p>`:"";
  const status=e.saved?(addresses?"Addresses saved. Connectivity has not been tested.":"Saved. Reload before making another change."):saving?(inviting?"Creating invitation…":"Saving…"):"";
  const feedback=`<div id="sharing-editor-feedback" tabindex="-1">${error}<p role="status" aria-live="polite">${esc(status)}</p></div>`;
  const saveLabel=addresses?(e.saved?"Saved":saving?"Saving…":"Save"):inviting?(saving?"Creating…":"Create"):e.mode==="approve"?"Approve":"Save";
  return `<h3>${esc(titles[e.mode]||"Sharing")}</h3>${feedbackAtEnd?"":feedback}${content}${feedbackAtEnd?feedback:""}<p>${sharingButton(e.returnInvite?"Return to invitation":"Back","sharingCloseEditor()",e.busy)} ${e.mode!=="code"?sharingButton(saveLabel,"sharingSave()",e.busy||!e.ready||e.saved||(e.mode==="invite"&&e.needsEndpoints)):""} ${["invite","scope","matrix","manifest","endpoints"].includes(e.mode)?sharingButton(addresses&&e.saved?"Edit addresses":"Reload current data","sharingEditorReload()",e.busy):""}</p>`;
}
async function sharingMutation(path,method,body,capture,key="updated"){const reply=await sharingRequest(path,{method,body,capture});if(reply?.[key]!==true)throw new Error("Sharing update did not return a confirmed result");return reply;}
async function sharingSave(){
  await sharingWork(async(s,e)=>{
    if(!e.ready||e.saved)throw new Error("Reload current data before saving");
    const c=s.requestCapture,mode=e.mode;
    if(mode==="invite"){
      sharingIds(e.selected);if(!e.selected.length)throw new Error("Select at least one library");
      e.needsEndpoints=(await sharingReadManifest(c))===null;
      if(e.needsEndpoints)throw new Error("Set up this Cinema’s private addresses, then return to this invitation. Your selected libraries are kept.");
      let r;try{r=await sharingRequest("/sharing/invitations",{method:"POST",body:{library_ids:e.selected,ttl_seconds:86400n},capture:c});}
      catch(error){if(error.status===409&&error.code==="sharing_endpoints_unavailable"){e.needsEndpoints=true;throw new Error("This Cinema’s addresses are no longer configured. Configure them, then return to this invitation. Your selected libraries are kept.");}throw error;}
      sharingUUID(r.id);sharingInvitation(r.invitation);s.invitation=r;s.invitationCopyNotice="";
    }else if(mode==="import"||mode==="repair"){
      const path=mode==="import"?"/sharing/imports":"/sharing/imports/"+sharingUUID(e.row.import.id)+"/re-pair";
      const body={invitation:sharingInvitation(e.text)};if(mode==="repair")body.expected_lifecycle_generation=sharingInteger(e.row.import.lifecycle_generation);
      const r=sharingImport(await sharingRequest(path,{method:"POST",body,capture:c}));
      if(mode==="repair"&&(r.import.id!==e.row.import.id||r.import.source_server_id!==e.row.import.source_server_id||r.import.catalogue_epoch!==e.row.import.catalogue_epoch))throw new Error("Source identity changed");
      s.imports=[...s.imports.filter(v=>v.import.id!==r.import.id),r];e.text="";
    }else if(mode==="approve"){
      const r=e.row;sharingExport(r);if(sharingCode(e.text)!==r.pairing_code)throw new Error("Pairing code does not match");
      await sharingMutation("/sharing/exports/"+r.grant.id+"/approve","POST",{expected_mutation_generation:r.grant.mutation_generation,pairing_code:e.text},c);e.text="";
    }else if(mode==="scope"){
      sharingIds(e.selected);await sharingMutation("/sharing/exports/"+sharingUUID(e.row.grant.id)+"/libraries","PUT",{expected_mutation_generation:sharingInteger(e.row.grant.mutation_generation),library_ids:e.selected},c);
    }else if(mode==="matrix"){
      const m=e.matrix;sharingBoundSnapshot(m.snapshot,e.row);sharingAssignments(m.groups);
      await sharingMutation("/sharing/imports/"+sharingUUID(e.row.import.id)+"/assignments","PUT",{expected_assignment_generation:sharingInteger(m.snapshot.expected_assignment_generation),assignments:m.groups},c);
    }else if(mode==="manifest"||mode==="endpoints"){
      e.error="";
      for(const row of e.endpoints){
        if(!row.dns_parts)continue;
        const machine=row.dns_parts.machine_name.trim().toLowerCase(),tailnet=row.dns_parts.tailnet_dns_name.trim().toLowerCase().replace(/\.$/,"");
        if(!/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(machine))throw new Error("Copy just the Machine name from Tailscale → Machines, such as cinema. Put the tailnet name in the separate Tailnet DNS name field.");
        if(!/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.ts\.net$/.test(tailnet))throw new Error("Copy the Tailnet DNS name from Tailscale → DNS, such as tail123abc.ts.net. Cinema adds the machine name automatically.");
      }
      const rows=e.endpoints.map(r=>{let port=r.port;if(typeof port==="string"){if(!/^[1-9][0-9]{0,4}$/.test(port))throw new Error("Use a canonical port");port=BigInt(port);}return sharingEndpoint({...r,port});});sharingEndpoints(rows);
      const expected=mode==="manifest"?sharingInteger(e.manifestRevision,0n):sharingInteger(e.row.import.endpoint_generation);if(expected===9223372036854775807n)throw new Error("Endpoint generation is exhausted");
      await sharingMutation(mode==="manifest"?"/sharing/endpoints":"/sharing/imports/"+sharingUUID(e.row.import.id)+"/endpoints","PUT",mode==="manifest"?{expected_revision:expected,endpoints:rows}:{expected_endpoint_generation:expected,endpoints:rows,confirm_new_pins:true},c);
      if(mode==="manifest"){
        e.saved=true; // The PUT is confirmed, even when its follow-up read fails.
        try{await sharingReadManifest(c);}catch(error){throw new Error("Addresses were saved, but current address information could not be refreshed. Reload to check it; no save was retried.");}
      }
    }else if(mode==="rotate"){
      const r=sharingImport(await sharingRequest("/sharing/imports/"+sharingUUID(e.row.import.id)+"/rotate",{method:"POST",body:{},capture:c}));if(r.import.id!==e.row.import.id||r.import.source_server_id!==e.row.import.source_server_id||r.import.catalogue_epoch!==e.row.import.catalogue_epoch)throw new Error("Source identity changed");s.imports=s.imports.map(v=>v.import.id===r.import.id?r:v);
    }else if(mode==="disconnect"||mode==="revoke"){
      await sharingMutation(mode==="disconnect"?"/sharing/imports/"+sharingUUID(e.row.import.id):"/sharing/exports/"+sharingUUID(e.row.grant.id),"DELETE",null,c,mode==="disconnect"?"disabled":"revoked");
    }else throw new Error("Unsupported Sharing edit");
    e.saved=true;
  },"save");
}
async function sharingReload(){await sharingWork(async(s)=>{
  const [imports,exports,status]=await Promise.all([sharingManagementRead("/sharing/imports",s.requestCapture),sharingManagementRead("/sharing/exports",s.requestCapture),sharingManagementRead("/sharing/status",s.requestCapture)]);
  if(!sharingCurrent(s.requestCapture))throw new Error("Sharing edit changed");s.imports=imports.imports;s.exports=exports.exports;s.next=exports.next??null;s.status=status;s.error="";
});}
async function sharingMoreExports(){await sharingWork(async(s)=>{sharingUUID(s.next);const page=sharingRows(await sharingRequest("/sharing/exports?after="+s.next,{capture:s.requestCapture}),"exports",sharingExport);if(page.next!=null&&(sharingUUID(page.next)===s.next))throw new Error("Export cursor did not advance");if(s.exports.length+page.exports.length>4096)throw new Error("Export list exceeds its bound");const ids=new Set(s.exports.map(r=>r.grant.id));if(page.exports.some(r=>ids.has(r.grant.id)))throw new Error("Duplicate export page");s.exports.push(...page.exports);s.next=page.next??null;});}
async function sharingCancelInvitation(){await sharingWork(async(s)=>{if(!s.invitation)throw new Error("No current invitation");await sharingMutation("/sharing/invitations/"+sharingUUID(s.invitation.id),"DELETE",null,s.requestCapture,"cancelled");s.invitation=null;});}

async function sharingFreshExport(id,capture){
  sharingUUID(id);let next=null,seen=new Set();
  for(let page=0;page<128;page++){
    const reply=sharingRows(await sharingRequest("/sharing/exports"+(next?"?after="+sharingUUID(next):""),{capture}),"exports",sharingExport);
    for(const row of reply.exports){if(seen.has(row.grant.id))throw new Error("Repeated export page");seen.add(row.grant.id);if(row.grant.id===id)return row;}
    if(reply.next==null)break;if(sharingUUID(reply.next)===next)throw new Error("Export cursor did not advance");next=reply.next;
  }
  throw new Error("Export no longer available. Reload Sharing.");
}
