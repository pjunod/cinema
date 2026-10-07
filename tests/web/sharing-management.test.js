"use strict";
const fs=require("node:fs"),vm=require("node:vm"),assert=require("node:assert/strict"),{test}=require("node:test");
const source=fs.readFileSync("crates/plurxd/src/web/pages/sharing-management.js","utf8"),catalogue=fs.readFileSync("crates/plurxd/src/web/pages/shared-libraries.js","utf8");
const id="11111111-1111-4111-8111-111111111111",server="22222222-2222-4222-8222-222222222222",epoch="33333333-3333-4333-8333-333333333333",claim="44444444-4444-4444-8444-444444444444",grant="55555555-5555-4555-8555-555555555555",large=9007199254740993n;
const endpoint={ipv4:"100.127.255.254",ipv6:"fd7a:115c:a1e0::1",ts_fqdn:"source.private.ts.net",port:8443n,spki_sha256:"a".repeat(64)};
function imported(){return {import:{id,source_server_id:server,catalogue_epoch:epoch,claim_id:claim,remote_grant_id:grant,source_name:"Source",state:"active",assignment_generation:large,lifecycle_generation:large,endpoint_generation:large,endpoints:[{...endpoint}]},pairing_code:"0123456789abcdef"};}
function exported(){return {grant:{id:grant,recipient_server_id:server,state:"pending",scope_generation:large,credential_generation:large,catalogue_generation:large,mutation_generation:large},recipient_name:"Recipient",invitation_id:id,claim_id:claim,library_ids:[large.toString()],pairing_code:"0123456789abcdef"};}
function stringify(v){return JSON.stringify(v,(_,x)=>typeof x==="bigint"?"__INTEGER"+x:x).replace(/"__INTEGER([0-9]+)"/g,"$1");}
function reply(v,status=200){return new Response(stringify(v),{status,headers:{"content-type":"application/json"}});}
function harness(fetcher=async()=>reply({updated:true})){
 const requests=[],element={innerHTML:"",replaceChildren(){this.innerHTML="";}};
 const c=vm.createContext({TextDecoder,TextEncoder,Uint8Array,URL,URLSearchParams,Response,AUTH_GENERATION:1,PAGE_RENDER_GENERATION:1,TOKEN:"login-a",API:"/api/v1",location:{hash:"#/settings/sharing",origin:"https://b.example"},PLAYBACK_FILE_UUID:/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
  SETTINGS_DATA:{},SETTINGS_LOADED:new Set(),readAfterRequest:()=>({index:"",generation:1,epoch:0}),observeReadAfter(){},forgetReadAfter(){},clearLocalSession(){c.AUTH_GENERATION++;c.TOKEN=null;},
  esc:v=>String(v).replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll('"',"&quot;"),setHead:()=>"",setCard:v=>v,cardHead:()=>"",document:{getElementById(){return element;}},fetch:async(path,options)=>{requests.push({path,options});return fetcher(path,options,c);}});
 vm.runInContext(catalogue+"\n"+source+"\nthis.m={parse:sharingJSON,stringify:sharingJSONString,endpoint:sharingEndpoint,endpoints:sharingEndpoints,recovery:sharingRecoveryHTML,mainReload:sharingReload,matrix:sharingMatrix,assignments:sharingAssignments,cancel:sharingCancelInvitation,endpointEdit:sharingEndpointEdit,request:sharingRequest,read:sharingManagementRead,panel:sharingManagementPanel,capture:sharingCapture,retire:sharingRetire,route:sharingRouteChanged,open:sharingOpen,save:sharingSave,reload:sharingEditorReload,configure:sharingConfigureAddresses,back:sharingCloseEditor,select:sharingSelect,edit:sharingEdit,matrixEdit:sharingMatrixEdit,removeOutside:sharingRemoveOutside,work:sharingWork,state:()=>SHARING_MANAGEMENT};",c);
 const data={sharingImports:{imports:[imported()]},sharingExports:{exports:[exported()],next:null},sharingStatus:{listener:"ready"}};
 c.m.panel(data);return {c,m:c.m,requests,element,data};
}
test("sharing management parses and serializes exact wire integers without rounding Local or Source identities",()=>{
 const {m}=harness();const value=m.parse('{"generation":9007199254740993,"user":9223372036854775807,"library":"9007199254740993","zero":0,"ignored":1.5}');
 assert.equal(value.generation,large);assert.equal(value.user,9223372036854775807n);assert.equal(value.library,large.toString());
 assert.equal(m.stringify({expected_assignment_generation:value.generation,assignments:[{library_id:value.library,user_ids:[value.user,0n]}]}),'{"expected_assignment_generation":9007199254740993,"assignments":[{"library_id":"9007199254740993","user_ids":[9223372036854775807,0]}]}');
 for(const text of ['{"id":1,"id":2}','[01]','1 trailing','[1e999]','{"x":'+"[".repeat(34)+'0'+"]".repeat(34)+'}'])assert.throws(()=>m.parse(text));
 assert.throws(()=>m.stringify({id:9007199254740992}));assert.throws(()=>m.stringify({id:9223372036854775808n}));
});
test("sharing initial settings budget remains four reads including address setup and editor lookups are lazy",async()=>{
 const {m,requests}=harness(async(path)=>reply(path.endsWith("imports")?{imports:[imported()]}:path.endsWith("exports")?{exports:[exported()],next:null}:{}));
 await Promise.all([m.read("/sharing/status"),m.read("/sharing/imports"),m.read("/sharing/exports")]);assert.deepEqual(requests.map(r=>r.path),["/api/v1/sharing/status","/api/v1/sharing/imports","/api/v1/sharing/exports","/api/v1/sharing/endpoints"]);
 const settings=fs.readFileSync("crates/plurxd/src/web/pages/settings.js","utf8");assert.match(settings,/sharing:\{required:\["sharingStatus","sharingImports","sharingExports"\],secondary:\[\]/);
});
test("pair approval requires an explicitly entered matching code and keeps exact mutation generation with no409 retry",async()=>{
 const {m,requests}=harness(async()=>reply({message:"stale"},409));await m.open("approve",0);m.edit("text","aaaaaaaaaaaaaaaa");await m.save();assert.equal(requests.length,0);
 await m.open("approve",0);m.edit("text","0123456789abcdef");await m.save();assert.equal(requests.length,1);assert.equal(requests[0].path,"/api/v1/sharing/exports/"+grant+"/approve");assert.equal(requests[0].options.method,"POST");assert.equal(requests[0].options.body,'{"expected_mutation_generation":9007199254740993,"pairing_code":"0123456789abcdef"}');assert.equal(m.state().editor.ready,false);
});
test("complete assignment matrix preserves outside scope groups and missing users until explicit removal",async()=>{
 const r=imported(),snapshot={state:"active",import_id:id,server_id:server,catalogue_epoch:epoch,lifecycle_generation:large,expected_assignment_generation:large,assignments:[{library_id:"7",user_ids:[large,9223372036854775807n]},{library_id:"8",user_ids:[0n]}]},scope={...snapshot,libraries:[{library_id:"8",name:"Source Movies",kind:"movies",anime:false}]};
 const {m,requests}=harness(async(path)=>reply(path==="/api/v1/sharing/imports"?{imports:[r]}:path.endsWith("/assignments")?snapshot:path.endsWith("/libraries")?scope:path==="/api/v1/users"?[{id:0n,username:"Current",is_admin:false}]:{updated:true}));
 await m.open("matrix",0);const e=m.state().editor;assert.equal(e.ready,true);assert.equal(e.matrix.libraries.length,2);assert.equal(e.matrix.viewers.length,3);
 e.library="8";m.matrixEdit(large.toString(),true);await m.save();const write=requests.find(r=>r.options.method==="PUT");assert.ok(write);const body=m.parse(write.options.body);assert.equal(body.expected_assignment_generation,large);assert.equal(body.assignments.find(g=>g.library_id==="7").user_ids[1],9223372036854775807n);
 e.library="7";m.removeOutside();assert.equal(e.matrix.groups.some(g=>g.library_id==="7"),false);
 assert.throws(()=>m.matrix({...snapshot,expected_assignment_generation:large+1n},scope,[],r));assert.throws(()=>m.matrix(snapshot,{...scope,libraries:[{library_id:"8",name:"😀".repeat(65),kind:"movies",anime:false}]},[],r));
});
test("endpoint edits validate private Tailnet fields before requests and require explicit new pins with exact revision",async()=>{
 const {m,requests}=harness(async(path)=>reply(path.endsWith("/endpoints")?{manifest:{revision:large,endpoints:[endpoint]}}:{updated:true}));
 await m.open("manifest");await m.save();assert.equal(requests.length,1);m.edit("confirm",true);await m.save();assert.equal(requests.length,2);assert.equal(m.parse(requests[1].options.body).expected_revision,large);
 for(const patch of [{ipv4:"127.0.0.1"},{ipv4:"100.128.0.1"},{ipv4:"100.064.0.1"},{ipv6:"2001:db8::1"},{ipv6:"fd7a:115c:a1e0::1::2"},{ts_fqdn:"source.ts.net"},{ts_fqdn:"source.other.example"},{port:65536n},{spki_sha256:"A".repeat(64)}])assert.throws(()=>m.endpoint({...endpoint,...patch}));
 assert.throws(()=>m.endpoints([]));assert.throws(()=>m.endpoints(Array(5).fill(endpoint)));assert.equal(m.endpoint(endpoint).ipv4,"100.127.255.254");
});
test("current401 and403 retire secrets before oversized response bodies while old account responses cannot retire new drafts",async()=>{
 for(const status of [401,403]){
  const {m,c,element}=harness(async()=>new Response("x".repeat(131073),{status}));await m.open("import");m.edit("text","cinema-share-v1:secret");const editor=m.state().editor;m.state().invitation={id,invitation:"cinema-share-v1:other"};
  await assert.rejects(m.request("/sharing/status"));assert.equal(m.state(),null);assert.equal(editor.text,"");assert.equal(element.innerHTML,"");if(status===403)assert.equal(c.TOKEN,"login-a");
 }
 let release;const gate=new Promise(resolve=>release=resolve);const h=harness(async()=>{await gate;return new Response("",{status:403});});const pending=h.m.request("/sharing/status");h.c.AUTH_GENERATION=2;h.c.TOKEN="login-b";h.m.retire();h.m.panel({sharingImports:{imports:[imported()]},sharingExports:{exports:[exported()],next:null},sharingStatus:{}});await h.m.open("import");h.m.edit("text","cinema-share-v1:newsecret");release();await assert.rejects(pending);assert.equal(h.m.state().editor.text,"cinema-share-v1:newsecret");
});
test("page leave and late edit responses cannot restore retired secrets or overwrite a newer draft",async()=>{
 let release;const gate=new Promise(resolve=>release=resolve);const h=harness(async(path)=>{if(path==="/api/v1/sharing/endpoints")return reply({manifest:{revision:1n,endpoints:[endpoint]}});await gate;return reply({id,invitation:"cinema-share-v1:returned",expires_at_ms:1n});});
 h.m.state().editor={mode:"invite",libraries:[],selected:["7"],revision:0,ready:true,busy:false,error:""};const pending=h.m.save();h.c.location.hash="#/";h.m.route();release();await pending;assert.equal(h.m.state(),null);
 let finish;const delay=new Promise(resolve=>finish=resolve);const newer=harness(async()=>{await delay;return reply({manifest:{revision:large,endpoints:[endpoint]}});});const opening=newer.m.open("manifest");newer.m.state().editor.revision++;finish();await opening;assert.equal(newer.m.state().editor.ready,false);assert.equal(newer.m.state().busy,false);
});
test("bounded management bodies refuse oversized mutation and complete reads without partial publish",async()=>{
 const h=harness();await assert.rejects(h.m.request("/sharing/imports",{method:"POST",body:{invitation:"x".repeat(16385)}}));assert.equal(h.requests.length,0);
 const big=harness(async()=>new Response("x".repeat(131073)));await assert.rejects(big.m.request("/sharing/status"));
 const invalid=harness(async()=>new Response(new Uint8Array([0xff])));await assert.rejects(invalid.m.request("/sharing/status"));
});

test("actual invitation import repair rotation cancellation and disconnect requests preserve complete authority",async()=>{
 const made="cinema-share-v1:generated",input="cinema-share-v1:input";
 const h=harness(async(path,options)=>{
  if(path==="/api/v1/libraries")return reply([{id:large,name:"Movies",kind:"movies"}]);
  if(path==="/api/v1/sharing/endpoints")return reply({manifest:{revision:1n,endpoints:[endpoint]}});
  if(path==="/api/v1/sharing/invitations"&&options.method==="POST")return reply({id,invitation:made,expires_at_ms:large});
  if(path.includes("/invitations/"))return reply({cancelled:true});
  if(path.endsWith("/re-pair")||path.endsWith("/rotate")||(path==="/api/v1/sharing/imports"&&options.method==="POST"))return reply(imported());
  return reply({disabled:true,revoked:true});
 });
 await h.m.open("invite");h.m.select(0,true);await h.m.save();assert.equal(h.m.state().invitation.invitation,made);assert.equal(h.m.parse(h.requests.find(r=>r.options.method==="POST").options.body).library_ids[0],large.toString());await h.m.cancel();assert.equal(h.m.state().invitation,null);
 for(const mode of ["import","repair","rotate","disconnect","revoke"]){await h.m.open(mode,0);if(mode==="import"||mode==="repair")h.m.edit("text",input);await h.m.save();assert.equal(h.m.state().editor.saved,true,mode);}
 const repair=h.requests.find(r=>r.path.endsWith("/re-pair"));assert.equal(h.m.parse(repair.options.body).expected_lifecycle_generation,large);
 assert.equal(h.requests.find(r=>r.path.endsWith("/rotate")).options.body,"{}");assert.equal(h.requests.filter(r=>r.options.method==="DELETE").length,3);
});
test("fresh paginated export scope retains missing libraries and uses only the newest mutation generation",async()=>{
 const fresh=exported();fresh.grant.state="active";fresh.grant.mutation_generation=large+1n;fresh.library_ids=["7",large.toString()];
 const h=harness(async(path)=>reply(path==="/api/v1/libraries"?[{id:large,name:"Movies",kind:"movies"}]:path==="/api/v1/sharing/exports"?{exports:[fresh],next:null}:{updated:true}));
 await h.m.open("scope",0);assert.equal(h.m.state().editor.ready,true);assert.equal(h.m.state().editor.libraries.length,2);assert.equal(h.m.state().editor.selected.includes("7"),true);
 await h.m.save();const write=h.requests.find(r=>r.options.method==="PUT"),body=h.m.parse(write.options.body);assert.equal(body.expected_mutation_generation,large+1n);assert.equal(body.library_ids.includes("7"),true);assert.equal(h.requests.some(r=>r.path==="/api/v1/sharing/exports/"+grant),false);
});
test("missing matrix scope and failed endpoint read never create an empty replacement or revision zero",async()=>{
 for(const mode of ["matrix","manifest"]){const h=harness(async(path)=>reply(path==="/api/v1/sharing/imports"?{imports:[imported()]}:{message:"unavailable"},path==="/api/v1/sharing/imports"?200:503));await h.m.open(mode,0);assert.equal(h.m.state().editor.ready,false);await h.m.save();assert.equal(h.requests.some(r=>r.options.method!=="GET"),false);}
});
test("route closure and account change during streamed response fail closed and cancel the reader",async()=>{
 const h=harness();for(const path of ["https://source/files","/sharing/imports/7/assignments","/sharing/imports/"+id+"/../rotate","/sharing/imports/"+id+"?token=secret","/sharing/exports?after="+id+"&x=1"])await assert.rejects(h.m.request(path));assert.equal(h.requests.length,0);
 let release,cancelled=false;const gate=new Promise(resolve=>release=resolve);const stale=harness(async()=>new Response(new ReadableStream({async pull(controller){await gate;controller.enqueue(new TextEncoder().encode('{"updated":true}'));},cancel(){cancelled=true;}})));const pending=stale.m.request("/sharing/status");await Promise.resolve();stale.c.TOKEN="login-b";release();await assert.rejects(pending);assert.equal(cancelled,true);
});

test("complete4096 actual Local library DTOs with paths remain within the bounded editor read",async()=>{
 const libraries=Array.from({length:4096},(_,i)=>({id:large+BigInt(i),name:"Movies "+i,kind:"movies",paths:Array.from({length:8},(_,j)=>"/library/"+i+"/"+j),anime:false,created_at:large,scan_interval_mins:0n,refresh_interval_mins:0n,last_scan_at:null,last_refresh_at:null}));
 const wire=stringify(libraries);assert.ok(Buffer.byteLength(wire)<4194304);
 const h=harness(async(path)=>reply(path==="/api/v1/sharing/endpoints"?{manifest:{revision:1n,endpoints:[endpoint]}}:libraries));await h.m.open("invite");const e=h.m.state().editor;assert.equal(e.ready,true);assert.equal(e.libraries.length,4096);assert.equal(e.libraries[4095].library_id,(large+4095n).toString());h.m.select(4095,true);assert.equal(e.selected[0],(large+4095n).toString());
});
test("sharing node card reports orphaned playback recovery without inventing state",()=>{
 const {m}=harness();
 const base={sharingImports:{imports:[imported()]},sharingExports:{exports:[exported()],next:null}};
 assert.doesNotMatch(m.panel({...base,sharingStatus:{listener:"ready"}}),/Playback recovery/);
 const html=m.panel({...base,sharingStatus:{listener:"ready",receiver_recovery:{last_scan_at_ms:null,in_flight:[id],retired_total:3n,stranded:[{incarnation_id:id,reason:"dispatch_unknown",observed_at_ms:1}]}}});
 assert.match(html,/Playback recovery: not yet scanned · 3 retired · 1 in progress · 1 stranded/);
 assert.match(html,new RegExp(id+": dispatch unknown"));
 assert.doesNotMatch(m.recovery({receiver_recovery:{last_scan_at_ms:5,in_flight:[],retired_total:0,stranded:[]}}),/<ul>/);
});

test("invitation address setup preserves selected libraries and never posts until configured",async()=>{
 let manifest=null;const h=harness(async(path,options)=>{
  if(path==="/api/v1/libraries")return reply([{id:large,name:"Movies",kind:"movies"}]);
  if(path==="/api/v1/sharing/endpoints"&&options.method==="GET")return reply({manifest});
  if(path==="/api/v1/sharing/endpoints"&&options.method==="PUT"){const body=h.m.parse(options.body);assert.equal(body.expected_revision,0n);manifest={revision:1n,endpoints:body.endpoints};return reply({updated:true});}
  return reply({id,invitation:"cinema-share-v1:created",expires_at_ms:large});
 });
 await h.m.open("invite");h.m.select(0,true);assert.match(h.element.innerHTML,/Configure this Cinema’s addresses/);await h.m.save();assert.equal(h.requests.some(r=>r.options.method==="POST"),false);
 await h.m.configure();assert.equal(h.m.state().editor.endpoints[0].port,32443n);assert.match(h.element.innerHTML,/Saving addresses does not check connectivity/);
 for(const [key,value] of Object.entries(endpoint))h.m.endpointEdit(0,key,typeof value==="bigint"?value.toString():value);
 h.m.edit("confirm",true);await h.m.save();assert.equal(h.m.state().editor.saved,true);await h.m.back();assert.equal(h.m.state().editor.selected[0],large.toString());assert.equal(h.m.state().editor.needsEndpoints,false);assert.equal(h.requests.some(r=>r.options.method==="POST"),false);
 await h.m.save();assert.equal(h.m.state().invitation.invitation,"cinema-share-v1:created");assert.equal(h.requests.filter(r=>r.options.method==="POST").length,1);
});
test("missing-manifest invitation conflict offers setup without replay or losing draft",async()=>{
 const h=harness(async(path,options)=>path==="/api/v1/libraries"?reply([{id:large,name:"Movies",kind:"movies"}]):path==="/api/v1/sharing/endpoints"?reply({manifest:{revision:1n,endpoints:[endpoint]}}):reply({code:"sharing_endpoints_unavailable",message:"sharing endpoints unavailable"},409));
 await h.m.open("invite");h.m.select(0,true);await h.m.save();assert.equal(h.m.state().editor.ready,false);assert.equal(h.m.state().editor.needsEndpoints,true);assert.match(h.element.innerHTML,/Configure this Cinema’s addresses/);assert.match(h.element.innerHTML,/selected libraries are kept/);
 await h.m.configure();await h.m.back();assert.equal(h.m.state().editor.selected[0],large.toString());assert.equal(h.requests.filter(r=>r.options.method==="POST").length,1);
 await h.m.reload();assert.equal(h.m.state().editor.selected[0],large.toString());assert.equal(h.requests.filter(r=>r.options.method==="POST").length,1);
});

test("Sharing requirements distinguish missing addresses and observed TLS from unverified host network",async()=>{
 const h=harness(async(path)=>reply(path==="/api/v1/sharing/endpoints"?{manifest:null}:{listener:"listening",certificate:{spki_sha256:endpoint.spki_sha256,expires_at_ms:large},serve:"unknown"}));
 const status=await h.m.read("/sharing/status");const html=h.m.panel({...h.data,sharingStatus:status});
 assert.match(html,/This Cinema’s addresses: Missing/);assert.match(html,/Listening on this serving node/);assert.match(html,/Sharing certificate: Observed/);assert.match(html,/Not verified by this Cinema/);assert.doesNotMatch(html,/Tailscale is installed|Network ready/);assert.equal(h.requests.some(r=>r.options.method!=="GET"),false);
 const unavailable=harness(async(path)=>reply(path==="/api/v1/sharing/endpoints"?{message:"unavailable"}:{listener:"listening"},path==="/api/v1/sharing/endpoints"?503:200));const unknown=await unavailable.m.read("/sharing/status");assert.match(unavailable.m.panel({...unavailable.data,sharingStatus:unknown}),/address information could not be read/);assert.doesNotMatch(unavailable.element.innerHTML,/This Cinema’s addresses: Missing/);
});

test("confirmed Cinema address save refreshes requirements and failed latest read becomes unknown without replay",async()=>{
 let manifest=null,failRead=false;const h=harness(async(path,options)=>{
  if(path==="/api/v1/sharing/endpoints"){
   if(options.method==="PUT"){manifest={revision:1n,endpoints:[endpoint]};return reply({updated:true});}
   return failRead?reply({message:"unavailable"},503):reply({manifest});
  }
  return reply(path.endsWith("imports")?{imports:[imported()]}:path.endsWith("exports")?{exports:[exported()],next:null}:{listener:"listening"});
 });
 await h.m.mainReload();assert.match(h.element.innerHTML,/This Cinema’s addresses: Missing/);
 await h.m.open("manifest");for(const [key,value] of Object.entries(endpoint))h.m.endpointEdit(0,key,typeof value==="bigint"?value.toString():value);h.m.edit("confirm",true);await h.m.save();await h.m.back();assert.match(h.element.innerHTML,/This Cinema’s addresses: Configured/);
 failRead=true;await h.m.mainReload();assert.match(h.element.innerHTML,/address information could not be read/);assert.doesNotMatch(h.element.innerHTML,/This Cinema’s addresses: Configured/);assert.equal(h.requests.filter(r=>r.options.method==="PUT").length,1);
 failRead=false;await h.m.open("manifest");h.m.edit("confirm",true);failRead=true;await h.m.save();assert.equal(h.m.state().editor.saved,true);assert.match(h.m.state().editor.error,/Addresses were saved/);await h.m.save();assert.equal(h.requests.filter(r=>r.options.method==="PUT").length,2);await h.m.back();assert.match(h.element.innerHTML,/address information could not be read/);
});
