"use strict";
const fs=require("node:fs"),vm=require("node:vm"),assert=require("node:assert/strict"),{test}=require("node:test");
const artworkSource=fs.readFileSync("crates/plurxd/src/web/pages/shared-artwork.js","utf8");
const source=fs.readFileSync("crates/plurxd/src/web/pages/shared-libraries.js","utf8");
const ref={import_id:"11111111-1111-4111-8111-111111111111",server_id:"22222222-2222-4222-8222-222222222222",catalogue_epoch:"33333333-3333-4333-8333-333333333333",library_id:"9007199254740993",item_id:"9223372036854775807"};
function harness(read){
 const elements=new Map(),requests=[];
 const context=vm.createContext({TextDecoder,Uint8Array,URLSearchParams,URL,AbortController,AUTH_GENERATION:1,PAGE_RENDER_GENERATION:1,TOKEN:"login-a",API:"/api/v1",location:{hash:"#/shared",href:"https://b.test/#/shared"},
  PLAYBACK_FILE_UUID:/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,HTMLButtonElement:class{},
  esc:v=>String(v).replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll('"',"&quot;"),fmtDur:String,
  api:async(path)=>{requests.push(path);const response=await read(path);Object.defineProperty(response,"url",{value:new URL("/api/v1"+path,"https://b.test").href});return response;},layoutChrome(){},setPagePhase(){},
  document:{getElementById(id){if(!elements.has(id))elements.set(id,{innerHTML:"",more:null,querySelector(){return this.more;},querySelectorAll(){return [];},insertAdjacentHTML(where,html){this.innerHTML+=html;},appendChild(button){this.more=button;button.remove=()=>{this.more=null;};}});return elements.get(id);},createElement(){return {dataset:{}};}}
 });
 vm.runInContext(artworkSource,context);
 vm.runInContext(source+"\nthis.shared={id:sharedCatalogueId,key:sharedCatalogueGroupKey,href:sharedCatalogueHref,route:sharedCatalogueRoute,item:sharedCatalogueItemHtml,read:sharedCatalogueRead,view:viewSharedCatalogue,page:sharedCatalogueLoadPage};",context);
 return {context,h:context.shared,elements,requests,capture(){return {generation:1,auth:1,token:"login-a",origin:"/api/v1",route:"#/shared"};}};
}
function reply(value){return new Response(JSON.stringify(value),{headers:{"content-type":"application/json"}});}
test("shared browse carries lossless compound references and never emits a local numeric route",()=>{
 const {h}=harness();const href=h.href(ref);assert.equal(h.route(href).reference.item_id,ref.item_id);
 assert.equal(h.route(h.href(ref,"library")).reference.item_id,undefined);
 assert.notEqual(h.key(ref),h.key({...ref,server_id:"44444444-4444-4444-8444-444444444444"}));
 assert.equal(h.route(h.href(ref,"library")+"?q=title%2Bspace").q,"title+space");
 assert.throws(()=>h.route(h.href(ref,"library")+"?q=x&q=y"));
 assert.ok(!href.includes("#/item/"));assert.ok(!href.includes("#/library/"));
 assert.equal(h.id("0"),"0");assert.equal(h.route(h.href({...ref,library_id:"0",item_id:"0"})).reference.item_id,"0");
 for(const id of [7,"01","-1","9223372036854775808","1/2"])assert.throws(()=>h.id(id));
 for(const hash of [href+"/extra",href.replace(ref.import_id,"%2f"),"#/shared/library/7",href.replace(ref.item_id,"01")])assert.throws(()=>h.route(hash));
});
test("shared cards escape source titles and reject foreign library and source substitution",()=>{
 const {h}=harness(),item={source:"shared",reference:ref,title:'<script>x</script>',kind:"movie"};
 assert.match(h.item(item,ref),/&lt;script>/);assert.throws(()=>h.item({...item,reference:{...ref,library_id:"7"}},ref));
 assert.throws(()=>h.item({...item,reference:{...ref,server_id:"44444444-4444-4444-8444-444444444444"}},ref));
});
test("catalogue body refuses account or B-origin changes during streaming and cancels its reader",async()=>{
 for(const field of ["AUTH_GENERATION","TOKEN","API","PAGE_RENDER_GENERATION"]){
  let resume,cancelled=false;
  const gate=new Promise(resolve=>resume=resolve);
  const {h,context,capture}=harness(async()=>new Response(new ReadableStream({async pull(controller){await gate;controller.enqueue(new TextEncoder().encode('{"libraries":[]}'));},cancel(){cancelled=true;}})));
  const pending=h.read("/shared/libraries",capture());await Promise.resolve();context[field]=field==="TOKEN"?"login-b":field==="API"?"https://other":2;resume();
  await assert.rejects(pending);assert.equal(cancelled,true);
 }
});
test("catalogue body enforces the actual4MiB limit without publishing a partial JSON response",async()=>{
 let cancelled=false;const {h,capture}=harness(async()=>new Response(new ReadableStream({start(c){c.enqueue(new Uint8Array(4*1024*1024+1));},cancel(){cancelled=true;}})));
 await assert.rejects(h.read("/shared/libraries",capture()));assert.equal(cancelled,true);
 const valid=harness(async()=>reply({title:"x".repeat(1024*1024+1)}));assert.ok((await valid.h.read("/shared/libraries",valid.capture())).title.length>1024*1024);
});
test("an unavailable Source cannot erase an independently loaded assigned group",async()=>{
 const other={...ref,import_id:"44444444-4444-4444-8444-444444444444",server_id:"55555555-5555-4555-8555-555555555555"};
 const fixture=harness(async path=>{
  if(path==="/shared/continue-watching?limit=200")return reply({groups:[]});
  if(path==="/shared/libraries")return reply({libraries:[{...ref,source_name:"Offline"},{...other,source_name:"Online"}]});
  if(path.includes(ref.import_id))throw new Error("source offline");
  return reply({...other,libraries:[{library_id:other.library_id,name:"Good library"}]});
 });
 await fixture.h.view(1);
 assert.match(fixture.elements.get("shared-source-0").innerHTML,/source offline/);
 assert.match(fixture.elements.get("shared-source-1").innerHTML,/Good library/);
 assert.ok(fixture.requests.every(path=>path.startsWith("/shared/")));
});
test("shared pagination encodes opaque cursors, keeps full-reference deduplication and refuses repeated cursors",async()=>{
 const item={source:"shared",reference:ref,title:"One",kind:"movie"},cursor="opaque+/=cursor";
 let calls=0;const fixture=harness(async path=>{calls++;return reply({items:[item,item],next_cursor:calls<3?cursor:null});});
 await fixture.h.page("/shared/imports/"+ref.import_id+"/libraries/"+ref.library_id+"/items",ref,fixture.capture(),"rows","title+space");
 const rows=fixture.elements.get("rows");assert.equal((rows.innerHTML.match(/<strong>One/g)||[]).length,1);
 await rows.more.onclick();assert.equal(calls,2);assert.ok(fixture.requests[1].includes("cursor=opaque%2B%2F%3Dcursor"));
 assert.ok(fixture.requests[0].includes("q=title%2Bspace"));assert.equal((rows.innerHTML.match(/<strong>One/g)||[]).length,1);
 fixture.context.AUTH_GENERATION++;await rows.more.onclick();assert.equal(calls,2,"old-account continuation cannot make another request");
});
test("Source metadata loads use at most four concurrent groups",async()=>{
 const groups=Array.from({length:8},(_,n)=>({...ref,import_id:`${String(n+1).padStart(8,"0")}-1111-4111-8111-111111111111`,source_name:`Source${n}`}));
 let active=0,maximum=0;const fixture=harness(async path=>{
  if(path==="/shared/libraries")return reply({libraries:groups});
  if(path.includes("continue-watching"))return reply({groups:[]});
  active++;maximum=Math.max(maximum,active);await new Promise(resolve=>setImmediate(resolve));active--;
  const group=groups.find(row=>path.includes(row.import_id));return reply({...group,libraries:[{library_id:group.library_id,name:group.source_name}]});
 });
 await fixture.h.view(1);assert.equal(maximum,4);assert.equal(active,0);
});
test("Continue Watching unavailable groups never publish stale items or erase another Source",async()=>{
 const other={...ref,import_id:"44444444-4444-4444-8444-444444444444",server_id:"55555555-5555-4555-8555-555555555555"};
 const fixture=harness(async path=>{
  if(path==="/shared/libraries")return reply({libraries:[ref,other]});
  if(path==="/shared/continue-watching?limit=200")return reply({groups:[ref,other]});
  const group=path.includes(ref.import_id)?ref:other;
  if(path.includes("continue-watching"))return reply({...group,availability:group===ref?"unavailable":"online",items:group===ref?[]:[{item:{source:"shared",reference:other,title:"Current title",kind:"movie"}}]});
  return reply({...group,libraries:[{library_id:group.library_id,name:"Library"}]});
 });
 await fixture.h.view(1);assert.match(fixture.elements.get("shared-continue-0").innerHTML,/unavailable/);
 assert.match(fixture.elements.get("shared-continue-1").innerHTML,/Current title/);
});
test("Continue Watching preserves two assigned libraries from the same Source",async()=>{
 const second={...ref,library_id:"7",item_id:"9"};
 const fixture=harness(async path=>{
  if(path==="/shared/libraries")return reply({libraries:[ref,second]});
  if(path==="/shared/continue-watching?limit=200")return reply({groups:[ref]});
  if(path.includes("continue-watching"))return reply({...ref,availability:"online",items:[ref,second].map((reference,n)=>({item:{source:"shared",reference,title:"Library title "+n,kind:"movie"}}))});
  return reply({...ref,libraries:[{library_id:ref.library_id,name:"First"},{library_id:"7",name:"Second"}]});
 });
 await fixture.h.view(1);const html=fixture.elements.get("shared-continue-0").innerHTML;
 assert.match(html,/Library title 0/);assert.match(html,/Library title 1/);assert.ok(!html.includes("Local"));
});

test("Shared launch refetches current delivery and retains string file identity and resume",async()=>{
 const fixture=harness(),launches=[],fileContext={source_file_id:"9007199254740993"};
 fixture.context.SHARED_DECISION={details:async()=>({detail:{delivery_status:"available",item:{title:"Fresh",reference:ref},watch:{position_ms:12500,watched:false}},files:[{context:fileContext,file:{duration_ms:90000}}]})};
 fixture.context.play=(...args)=>{launches.push(args);};
 vm.runInContext("this.launch=sharedCataloguePlay",fixture.context);
 await fixture.context.launch(ref,"9007199254740993",fixture.capture());
 assert.equal(launches[0][0],"9007199254740993");assert.equal(launches[0][2],12500);assert.equal(launches[0][4].fileContext,fileContext);
 fixture.context.SHARED_DECISION.details=async()=>({detail:{delivery_status:"unavailable"},files:[]});
 await assert.rejects(fixture.context.launch(ref,"9007199254740993",fixture.capture()));assert.equal(launches.length,1);
 fixture.context.AUTH_GENERATION++;await assert.rejects(fixture.context.launch(ref,"9007199254740993",fixture.capture()));assert.equal(launches.length,1);
});

test("an expired or substituted Source cursor reopens the list once instead of retrying a dead cursor",async()=>{
 for(const code of ["sharing_cursor_expired","sharing_query_changed","sharing_cursor_invalid"]){
  const item=n=>({source:"shared",reference:{...ref,item_id:String(n)},title:"Item "+n,kind:"movie"});
  let calls=0;const fixture=harness(async path=>{calls++;
   if(path.includes("cursor="))throw Object.assign(new Error("cursor refused"),{status:410,code});
   return reply({items:[item(1),item(2)],next_cursor:calls===1?"opaque-1":null});});
  const {item_id:_,...library}=ref;
  await fixture.h.page("/shared/imports/"+ref.import_id+"/libraries/"+ref.library_id+"/items",library,fixture.capture(),"rows");
  const rows=fixture.elements.get("rows");await rows.more.onclick();
  assert.equal(calls,3,code);assert.ok(fixture.requests[1].includes("cursor=opaque-1"));assert.ok(!fixture.requests[2].includes("cursor="),"fresh open");
  assert.match(rows.innerHTML,/reopened from the start/);assert.equal((rows.innerHTML.match(/<strong>Item 1/g)||[]).length,1);
  assert.equal(rows.more,null,"the reopened list ended without another dead cursor");
 }
 // Other refusals keep the existing retry affordance and never reopen.
 let calls=0;const fixture=harness(async path=>{calls++;if(path.includes("cursor="))throw Object.assign(new Error("offline"),{status:503,code:"sharing_source_unavailable"});
  return reply({items:[{source:"shared",reference:ref,title:"Kept",kind:"movie"}],next_cursor:"opaque"});});
 await fixture.h.page("/shared/imports/"+ref.import_id+"/libraries/"+ref.library_id+"/items",ref,fixture.capture(),"rows");
 const rows=fixture.elements.get("rows"),button=rows.more;button.disabled=false;await button.onclick();
 assert.equal(calls,2);assert.equal(button.disabled,false);assert.match(rows.innerHTML,/Kept/);
});
