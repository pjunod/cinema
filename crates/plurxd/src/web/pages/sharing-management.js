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
  if(typeof row.ts_fqdn!=="string"||!row.ts_fqdn.endsWith(".ts.net"))throw new Error("Use a private .ts.net FQDN");
  const prefix=row.ts_fqdn.slice(0,-7),labels=prefix.split(".");
  if(prefix.length>240||labels.length<2||labels.some(s=>! /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(s)))throw new Error("Invalid private Tailnet FQDN");
  if(typeof row.spki_sha256!=="string"||! /^[0-9a-f]{64}$/.test(row.spki_sha256))throw new Error("Use the exact lowercase SHA-256 SPKI pin");
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
function sharingManagementRead(path){return sharingRequest(path).then(v=>path==="/sharing/imports"?sharingRows(v,"imports",sharingImport):path==="/sharing/exports"?sharingRows(v,"exports",sharingExport):v);}
// Transient form state is page/account scoped; successful mutations require a
// fresh read before another edit. No request is automatically replayed.
function sharingManagementPanel(data){
  sharingRows(data.sharingImports,"imports",sharingImport);sharingRows(data.sharingExports,"exports",sharingExport);
  if(!SHARING_MANAGEMENT||!sharingCurrent(SHARING_MANAGEMENT.capture))SHARING_MANAGEMENT={capture:sharingCapture(),alive:true,revision:0,editor:null,invitation:null,imports:data.sharingImports.imports,exports:data.sharingExports.exports,next:data.sharingExports.next??null,error:"",busy:false};
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
function sharingButton(label,action,disabled=false){return `<button class="ghost sm" onclick="${action}"${disabled?" disabled":""}>${esc(label)}</button>`;}
function sharingInput(label,key,value,kind="text"){
  const e=SHARING_MANAGEMENT.editor;return `<label>${esc(label)}<input type="${kind}" value="${esc(value||"")}" autocomplete="off" autocapitalize="none" spellcheck="false" oninput="sharingEdit('${key}',this.value)"${e.busy?" disabled":""}></label>`;
}
function sharingManagementHTML(){
  const s=SHARING_MANAGEMENT;if(!s)return "";
  if(s.editor)return sharingEditorHTML(s.editor);
  const disabled=s.busy;
  return `<h3>Connect Cinemas</h3>${sharingButton("Create invitation","sharingOpen('invite')",disabled)} ${sharingButton("Import invitation","sharingOpen('import')",disabled)} ${sharingButton("This Cinema's endpoints","sharingOpen('manifest')",disabled)} ${sharingButton("Reload","sharingReload()",disabled)}${s.error?`<p role="alert">${esc(s.error)}</p>`:""}${s.invitation?`<section><h4>Invitation</h4><p>Copy this secret invitation to the recipient. It is cleared when you leave Sharing.</p><textarea readonly autocomplete="off">${esc(s.invitation.invitation)}</textarea>${sharingButton("Cancel invitation","sharingCancelInvitation()",disabled)} ${sharingButton("Clear secret","sharingClearInvitation()",disabled)}</section>`:""}<h3>Imports</h3>${s.imports.map((row,i)=>`<section><h4>${esc(row.import.source_name||"Shared source")}</h4><p>${esc(row.import.state)} · Source ${esc(row.import.source_server_id)}</p>${sharingButton("Pairing code",`sharingOpen('code',${i})`,disabled)} ${sharingButton("Viewer assignments",`sharingOpen('matrix',${i})`,disabled||row.import.state!=="active")} ${sharingButton("Source endpoints",`sharingOpen('endpoints',${i})`,disabled||!["claiming","pending","active"].includes(row.import.state))} ${sharingButton("Re-pair",`sharingOpen('repair',${i})`,disabled)} ${sharingButton("Rotate credential",`sharingOpen('rotate',${i})`,disabled||row.import.state!=="active")} ${sharingButton("Disconnect",`sharingOpen('disconnect',${i})`,disabled)}</section>`).join("")||'<p class="muted">No imports.</p>'}<h3>Exports</h3>${s.exports.map((row,i)=>`<section><h4>${esc(row.recipient_name||"Recipient")}</h4><p>${esc(row.grant.state)} · Recipient ${esc(row.grant.recipient_server_id)}</p>${sharingButton("Approve pairing",`sharingOpen('approve',${i})`,disabled||row.grant.state!=="pending")} ${sharingButton("Library scope",`sharingOpen('scope',${i})`,disabled||row.grant.state!=="active")} ${sharingButton("Revoke",`sharingOpen('revoke',${i})`,disabled)}</section>`).join("")||'<p class="muted">No exports.</p>'}${s.next?sharingButton("More exports","sharingMoreExports()",disabled):""}`;
}
function sharingPaint(){if(!SHARING_MANAGEMENT||!sharingCurrent(SHARING_MANAGEMENT.capture))return;const node=document.getElementById("sharing-management");if(node)node.innerHTML=sharingManagementHTML();}
function sharingEdit(key,value){const s=SHARING_MANAGEMENT,e=s?.editor;if(!e||e.busy||!sharingCurrent(s.capture))return;e[key]=value;e.revision++;if(key!=="confirm")e.confirm=false;}
function sharingCloseEditor(){const s=SHARING_MANAGEMENT;if(!s)return;s.revision++;s.editor=null;sharingPaint();}
function sharingClearInvitation(){if(SHARING_MANAGEMENT){SHARING_MANAGEMENT.invitation=null;sharingPaint();}}
async function sharingWork(work){
  const s=SHARING_MANAGEMENT;if(!s||s.busy||!sharingCurrent(s.capture))return;
  const e=s.editor,revision=e?e.revision:s.revision;s.requestCapture={...s.capture,work:true,state:s,stateRevision:s.revision,editor:e,editRevision:e?.revision};s.busy=true;if(e){e.busy=true;e.sent=false;}sharingPaint();
  const accepts=()=>SHARING_MANAGEMENT===s&&s.alive&&sharingCurrent(s.capture)&&(e?s.editor===e&&e.revision===revision:s.revision===revision);
  try{const result=await work(s,e);if(accepts())return result;}
  catch(error){if(accepts()){if(e){e.error=error.message;if(e.sent)e.ready=false;}else s.error=error.message;}}
  finally{if(SHARING_MANAGEMENT===s&&s.alive&&sharingCurrent(s.capture)){s.busy=false;if(s.editor===e&&e)e.busy=false;sharingPaint();}}
}
async function sharingOpen(mode,index=0){
  const s=SHARING_MANAGEMENT;if(!s||s.busy||!sharingCurrent(s.capture))return;
  const row=["scope","approve","revoke"].includes(mode)?s.exports[index]:s.imports[index];
  if(!["invite","import","manifest"].includes(mode)&&!row)return;
  s.revision++;s.editor={mode,row,revision:0,ready:false,busy:false,error:"",text:"",confirm:false,selected:[],libraries:[],matrix:null,endpoints:[],manifestRevision:0n};
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
      e.selected=e.mode==="scope"?[...e.row.library_ids]:[];
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
      const value=await sharingRequest("/sharing/endpoints",{capture:s.requestCapture});
      if(!Object.hasOwn(value,"manifest"))throw new Error("Endpoint manifest read is incomplete");
      const m=value.manifest;if(m!==null){sharingInteger(m.revision);sharingEndpoints(m.endpoints);}
      e.manifestRevision=m?.revision??0n;e.endpoints=m?m.endpoints.map(sharingEndpoint):[sharingBlankEndpoint()];
    }
    e.error="";e.ready=true;e.saved=false;e.confirm=false;
  });
}
function sharingBlankEndpoint(){return {ipv4:"",ipv6:null,ts_fqdn:"",port:8443n,spki_sha256:""};}
function sharingSelect(index,checked){const e=SHARING_MANAGEMENT?.editor;if(!e||e.busy)return;const id=e.libraries[index]?.library_id;if(!id)return;const selected=new Set(e.selected);if(checked)selected.add(id);else selected.delete(id);e.selected=[...selected];e.revision++;}
function sharingEndpointEdit(index,key,value){const e=SHARING_MANAGEMENT?.editor;if(!e||e.busy||!e.endpoints[index])return;e.endpoints[index][key]=key==="ipv6"?(value||null):value;e.confirm=false;e.revision++;}
function sharingEndpointCount(add,index=0){const e=SHARING_MANAGEMENT?.editor;if(!e||e.busy)return;if(add&&e.endpoints.length<4)e.endpoints.push(sharingBlankEndpoint());if(!add&&e.endpoints.length>1)e.endpoints.splice(index,1);e.confirm=false;e.revision++;sharingPaint();}
function sharingMatrixEdit(id,checked){const e=SHARING_MANAGEMENT?.editor;if(!e||e.busy||!e.matrix)return;const user=BigInt(id),m=e.matrix;if(!m.viewers.some(v=>v.id===user)||!m.libraries.some(l=>l.library_id===e.library))return;let group=m.groups.find(g=>g.library_id===e.library);if(!group){group={library_id:e.library,user_ids:[]};m.groups.push(group);}const ids=new Set(group.user_ids);if(checked)ids.add(user);else ids.delete(user);const replacement=[...ids];if(replacement.length>256){e.error="A library may have at most 256 viewers";sharingPaint();return;}group.user_ids=replacement;e.revision++;}
function sharingRemoveOutside(){const e=SHARING_MANAGEMENT?.editor;if(!e||e.busy||!e.matrix?.libraries.some(l=>l.library_id===e.library&&l.outside))return;e.matrix.groups=e.matrix.groups.filter(g=>g.library_id!==e.library);e.revision++;sharingPaint();}
function sharingEditorHTML(e){
  const titles={invite:"Create invitation",import:"Import invitation",repair:"Re-pair Source",approve:"Approve pairing",scope:"Export library scope",matrix:"Viewer assignments",manifest:"This Cinema's endpoints",endpoints:"Source endpoints",rotate:"Rotate Source credential",disconnect:"Disconnect Source",revoke:"Revoke export",code:"Pairing code"};
  let content="";
  if(e.mode==="code")content=`<p>Enter this code on the Source Cinema to approve this recipient:</p><code>${esc(e.row.pairing_code)}</code>`;
  else if(["import","repair"].includes(e.mode))content=sharingInput("Secret invitation","text",e.text);
  else if(e.mode==="approve")content=`<p>Compare the importing Cinema's pairing code and type it here. Approval is never automatic.</p>`+sharingInput("Pairing code","text",e.text);
  else if(["invite","scope"].includes(e.mode))content=e.libraries.map((l,i)=>`<label><input type="checkbox" onchange="sharingSelect(${i},this.checked)"${e.selected.includes(l.library_id)?" checked":""}${e.busy?" disabled":""}> ${esc(l.name)} · ${esc(l.library_id)}</label>`).join("");
  else if(e.mode==="matrix"&&e.matrix){
    const m=e.matrix,users=m.viewers.filter(v=>v.username.toLowerCase().includes((e.search||"").toLowerCase())||v.id.toString().includes(e.search||""));
    const group=m.groups.find(g=>g.library_id===e.library);
    content=`<p>The complete matrix is retained. Unavailable viewers and libraries outside current Source scope remain until you explicitly remove them.</p><label>Source library<select onchange="sharingEdit('library',this.value);sharingPaint()"${e.busy?" disabled":""}>${m.libraries.map(l=>`<option value="${esc(l.library_id)}"${l.library_id===e.library?" selected":""}>${esc(l.name)}</option>`).join("")}</select></label>`+sharingInput("Find viewers","search",e.search)+sharingButton("Find","sharingPaint()",e.busy)+users.slice(0,100).map(v=>`<label><input type="checkbox" onchange="sharingMatrixEdit('${v.id}',this.checked)"${group?.user_ids.includes(v.id)?" checked":""}${e.busy?" disabled":""}>${esc(v.username)} · ${v.id}</label>`).join("")+(users.length>100?`<p>Showing 100 of ${users.length} matching viewers. Refine the search; the complete matrix is retained.</p>`:"")+(m.libraries.some(l=>l.library_id===e.library&&l.outside)?sharingButton("Remove this outside-scope group","sharingRemoveOutside()",e.busy):"");
  }else if(["manifest","endpoints"].includes(e.mode)){
    content=`<p>Only private Tailnet addresses and exact certificate pins are accepted. A new pin requires your explicit confirmation.</p>`+e.endpoints.map((row,i)=>`<fieldset><legend>Endpoint ${i+1}</legend>${[["ipv4","Tailnet IPv4"],["ipv6","Tailnet IPv6 (optional)"],["ts_fqdn","Private .ts.net FQDN"],["port","Port"],["spki_sha256","SHA-256 SPKI pin"]].map(([key,label])=>`<label>${label}<input value="${esc(String(row[key]??""))}" autocomplete="off" autocapitalize="none" spellcheck="false" oninput="sharingEndpointEdit(${i},'${key}',this.value)"${e.busy?" disabled":""}></label>`).join("")}${sharingButton("Remove",`sharingEndpointCount(false,${i})`,e.busy||e.endpoints.length<=1)}</fieldset>`).join("")+sharingButton("Add endpoint","sharingEndpointCount(true)",e.busy||e.endpoints.length>=4)+`<label><input type="checkbox" onchange="sharingEdit('confirm',this.checked)"${e.confirm?" checked":""}${e.busy?" disabled":""}>I checked these new certificate pins against the Source.</label>`;
  }else if(["rotate","disconnect","revoke"].includes(e.mode))content=`<p>${e.mode==="rotate"?"Replace this Source credential?":e.mode==="disconnect"?"Disconnect this Source and stop using its shared libraries?":"Revoke this recipient's library access?"}</p>`;
  return `<h3>${esc(titles[e.mode]||"Sharing")}</h3>${e.error?`<p role="alert">${esc(e.error)}</p>`:""}${e.saved?'<p role="status">Saved. Reload before making another change.</p>':""}${content}<p>${sharingButton("Back","sharingCloseEditor()",e.busy)} ${!["code"].includes(e.mode)?sharingButton(e.mode==="invite"?"Create":e.mode==="approve"?"Approve":"Save","sharingSave()",e.busy||!e.ready||e.saved):""} ${["invite","scope","matrix","manifest","endpoints"].includes(e.mode)?sharingButton("Reload current data","sharingEditorReload()",e.busy):""}</p>`;
}
async function sharingMutation(path,method,body,capture,key="updated"){const reply=await sharingRequest(path,{method,body,capture});if(reply?.[key]!==true)throw new Error("Sharing update did not return a confirmed result");return reply;}
async function sharingSave(){
  await sharingWork(async(s,e)=>{
    if(!e.ready||e.saved)throw new Error("Reload current data before saving");
    const c=s.requestCapture,mode=e.mode;
    if(mode==="invite"){
      sharingIds(e.selected);if(!e.selected.length)throw new Error("Select at least one library");
      const r=await sharingRequest("/sharing/invitations",{method:"POST",body:{library_ids:e.selected,ttl_seconds:86400n},capture:c});sharingUUID(r.id);sharingInvitation(r.invitation);s.invitation=r;
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
      const rows=e.endpoints.map(r=>{let port=r.port;if(typeof port==="string"){if(!/^[1-9][0-9]{0,4}$/.test(port))throw new Error("Use a canonical port");port=BigInt(port);}return sharingEndpoint({...r,port});});sharingEndpoints(rows);
      const old=mode==="manifest"?new Set():new Set(e.row.import.endpoints.map(r=>r.spki_sha256));const newPin=rows.some(r=>!old.has(r.spki_sha256));if(newPin&&!e.confirm)throw new Error("Confirm the new certificate pins before saving");
      const expected=mode==="manifest"?sharingInteger(e.manifestRevision,0n):sharingInteger(e.row.import.endpoint_generation);if(expected===9223372036854775807n)throw new Error("Endpoint generation is exhausted");
      await sharingMutation(mode==="manifest"?"/sharing/endpoints":"/sharing/imports/"+sharingUUID(e.row.import.id)+"/endpoints","PUT",mode==="manifest"?{expected_revision:expected,endpoints:rows}:{expected_endpoint_generation:expected,endpoints:rows,confirm_new_pins:!!e.confirm},c);
    }else if(mode==="rotate"){
      const r=sharingImport(await sharingRequest("/sharing/imports/"+sharingUUID(e.row.import.id)+"/rotate",{method:"POST",body:{},capture:c}));if(r.import.id!==e.row.import.id||r.import.source_server_id!==e.row.import.source_server_id||r.import.catalogue_epoch!==e.row.import.catalogue_epoch)throw new Error("Source identity changed");s.imports=s.imports.map(v=>v.import.id===r.import.id?r:v);
    }else if(mode==="disconnect"||mode==="revoke"){
      await sharingMutation(mode==="disconnect"?"/sharing/imports/"+sharingUUID(e.row.import.id):"/sharing/exports/"+sharingUUID(e.row.grant.id),"DELETE",null,c,mode==="disconnect"?"disabled":"revoked");
    }else throw new Error("Unsupported Sharing edit");
    e.saved=true;
  });
}
async function sharingReload(){await sharingWork(async(s)=>{const [imports,exports]=await Promise.all([sharingRequest("/sharing/imports",{capture:s.requestCapture}).then(v=>sharingRows(v,"imports",sharingImport)),sharingRequest("/sharing/exports",{capture:s.requestCapture}).then(v=>sharingRows(v,"exports",sharingExport))]);if(!sharingCurrent(s.requestCapture))throw new Error("Sharing edit changed");s.imports=imports.imports;s.exports=exports.exports;s.next=exports.next??null;s.error="";});}
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
