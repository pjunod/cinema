"use strict";
const fs=require("node:fs"),vm=require("node:vm"),assert=require("node:assert/strict"),{test}=require("node:test");
const ref={import_id:"11111111-1111-4111-8111-111111111111",server_id:"22222222-2222-4222-8222-222222222222",catalogue_epoch:"33333333-3333-4333-8333-333333333333",library_id:"9007199254740993",item_id:"9223372036854775807"};
const asset="/api/v1/shared/imports/"+ref.import_id+"/art/"+"A".repeat(272);
function item(reference=ref,url=asset){return {source:"shared",reference,title:"Source artwork",kind:"movie",art:[{kind:"poster",variant:"w300",url}],poster_url:url};}
function png(width=600,height=400){const b=new Uint8Array(45),v=new DataView(b.buffer);b.set([137,80,78,71,13,10,26,10]);v.setUint32(8,13);b.set(Buffer.from("IHDR"),12);v.setUint32(16,width);v.setUint32(20,height);b.set(Buffer.from("IEND"),37);return b;}
function response(body,url="https://b.test"+asset,headers={"content-type":"image/png"},status=200){const r=new Response(body,{headers,status});Object.defineProperty(r,"url",{value:url});return r;}
function harness(){
 const requests=[],bitmaps=[],canvases=[];let art=async()=>response(png()),metadata=async()=>({item:item(),files:[],delivery_status:"unavailable"}),decode=null,metadataResponse=null;
 const root={querySelectorAll(){return canvases;}},capture={generation:1,auth:1,token:"bearer-a",origin:"/api/v1",route:"#/shared"};
 const context=vm.createContext({URL,URLSearchParams,TextDecoder,Uint8Array,DataView,Blob,AbortController,console,AUTH_GENERATION:1,PAGE_RENDER_GENERATION:1,TOKEN:"bearer-a",API:"/api/v1",location:{hash:"#/shared",href:"https://b.test/#/shared"},innerHeight:900,innerWidth:1000,
  PLAYBACK_FILE_UUID:/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,esc:String,fmtDur:String,
  document:{getElementById(){return root;}},
  api:async path=>metadataResponse?metadataResponse(path):response(JSON.stringify(await metadata(path)),new URL("/api/v1"+path,"https://b.test").href,{"content-type":"application/json"}),
  fetch:async(url,options)=>{requests.push({url,options});return art(url,options);},
  createImageBitmap:async(blob,options)=>{if(decode)return decode(blob,options);const bitmap={width:options.resizeWidth,height:options.resizeHeight,close(){this.width=0;this.height=0;this.closed=true;}};bitmaps.push(bitmap);return bitmap;},
  clearLocalSession(generation){if(generation===context.AUTH_GENERATION){context.SHARED_ARTWORK.retire();context.TOKEN=null;context.AUTH_GENERATION++;}}
 });
 for(const file of ["pages/shared-artwork.js","pages/shared-libraries.js"])vm.runInContext(fs.readFileSync("crates/plurxd/src/web/"+file,"utf8"),context);
 vm.runInContext("this.SHARED_ARTWORK=SHARED_ARTWORK",context);
 const h=context.SHARED_ARTWORK;
 return {context,h,root,requests,bitmaps,canvases,capture,setArt(fn){art=fn;},setMetadata(fn){metadata=fn;},setMetadataResponse(fn){metadataResponse=fn;},setDecode(fn){decode=fn;},async trusted(value=item()){
   metadata=async()=>({item:value,files:[],delivery_status:"unavailable"});return (await h.metadata("/shared/imports/"+value.reference.import_id+"/items/"+value.reference.item_id,capture)).item;},
  canvas(html){const id=/data-shared-art="([^"]+)"/.exec(html)[1],canvas={dataset:{sharedArt:id},width:0,height:0,isConnected:true,parentElement:root,draws:0,getBoundingClientRect(){return {top:0,bottom:108,left:0,right:72};},getContext(){return {drawImage(){canvas.draws++;}};}};canvases.push(canvas);return canvas;},
  async idle(){for(let n=0;n<200;n++){await new Promise(r=>setTimeout(r,1));if(h.snapshot().compressed.active===0&&h.snapshot().metadata===0)return;}throw new Error("artwork did not settle");}
 };
}
test("artwork context requires actual authenticated B metadata and closed advertised aliases",async()=>{
 const f=harness();assert.throws(()=>f.h.markup(item(),ref,f.capture),/authenticated/);
 const trusted=await f.trusted();assert.match(f.h.markup(trusted,ref,f.capture),/data-shared-art/);
 for(const url of ["https://a.test"+asset,asset+"?auth=x",asset+"#x",asset.replaceAll("A","%41"),asset.slice(0,-129),asset.replace(ref.import_id,"44444444-4444-4444-8444-444444444444")]){
  const bad=await f.trusted(item(ref,url));assert.throws(()=>f.h.markup(bad,ref,f.capture));
 }
 const missing=await f.trusted({...item(),art:[]});assert.throws(()=>f.h.markup(missing,ref,f.capture));
 const duplicates=await f.trusted({...item(),art:[...item().art,...item().art]});assert.throws(()=>f.h.markup(duplicates,ref,f.capture));
 const foreign=await f.trusted(item({...ref,server_id:"44444444-4444-4444-8444-444444444444"}));assert.throws(()=>f.h.markup(foreign,ref,f.capture));
});
test("B-only request preserves full string subject and closes bitmap before retaining canvas credit",async()=>{
 const f=harness(),value=await f.trusted(),canvas=f.canvas(f.h.markup(value,ref,f.capture));f.h.hydrate(f.root);await f.idle();
 assert.equal(f.requests.length,1);assert.equal(f.requests[0].url,"https://b.test"+asset);assert.equal(f.requests[0].options.headers.authorization,"Bearer bearer-a");assert.equal(f.requests[0].options.redirect,"error");assert.equal(f.requests[0].options.credentials,"omit");
 assert.equal(canvas.width,300);assert.equal(canvas.height,200);assert.equal(canvas.draws,1);assert.equal(canvas.dataset.sharedArtReady,"true");assert.equal(f.bitmaps[0].closed,true);
 assert.equal(f.h.snapshot().compressed.bytes,0);assert.equal(f.h.snapshot().pixels.bytes,300*200*8);
 f.h.retire();assert.equal(canvas.width,0);assert.equal(canvas.height,0);assert.equal(f.h.snapshot().pixels.bytes,0);
});
test("captured auth/page/origin and an obsolete object completion cannot publish",async()=>{
 for(const field of ["AUTH_GENERATION","TOKEN","API","PAGE_RENDER_GENERATION"]){
  const f=harness(),value=await f.trusted(),canvas=f.canvas(f.h.markup(value,ref,f.capture));let resolve;
  f.setDecode((blob,options)=>new Promise(r=>{resolve=()=>{const b={width:options.resizeWidth,height:options.resizeHeight,close(){this.width=0;this.height=0;this.closed=true;}};f.bitmaps.push(b);r(b);};}));f.h.hydrate(f.root);
  for(let n=0;n<100&&!resolve;n++)await new Promise(r=>setTimeout(r,1));assert.ok(resolve);
  f.context[field]=typeof f.context[field]==="number"?2:"other-account-or-origin";f.h.retire();assert.ok(f.h.snapshot().compressed.bytes>0,"decoder's unresolved Blob remains charged");resolve();await f.idle();
  assert.equal(canvas.draws,0);assert.equal(f.bitmaps[0].closed,true);assert.equal(f.h.snapshot().pixels.bytes,0);assert.equal(f.h.snapshot().compressed.bytes,0);
 }
 const f=harness(),old=await f.trusted(),oldCanvas=f.canvas(f.h.markup(old,ref,f.capture));let release;
 f.setArt(()=>new Promise(r=>{release=r;}));f.h.hydrate(f.root);await Promise.resolve();
 const fresh=await f.trusted(item(ref,asset.slice(0,-1)+"B"));f.h.markup(fresh,ref,f.capture);
 release(response(png()));await f.idle();assert.equal(oldCanvas.draws,0);
});
test("retirement cancels a blocked actual reader and old401 cannot clear new authorization",async()=>{
 const f=harness(),value=await f.trusted(),canvas=f.canvas(f.h.markup(value,ref,f.capture));let cancelled=false;
 f.setArt(()=>response(new ReadableStream({pull(){return new Promise(()=>{});},cancel(){cancelled=true;}})));f.h.hydrate(f.root);
 for(let n=0;n<10;n++)await Promise.resolve();f.h.retire();await f.idle();assert.equal(cancelled,true);assert.equal(canvas.draws,0);assert.equal(f.h.snapshot().compressed.bytes,0);
 const g=harness(),v=await g.trusted();g.canvas(g.h.markup(v,ref,g.capture));let finish;
 g.setArt(()=>new Promise(r=>finish=r));g.h.hydrate(g.root);await Promise.resolve();g.context.TOKEN="new-bearer";g.context.AUTH_GENERATION=2;
 finish(response("huge forbidden body","https://b.test"+asset,{"content-type":"image/png"},401));await g.idle();assert.equal(g.context.TOKEN,"new-bearer");assert.equal(g.context.AUTH_GENERATION,2);
});
test("declared/streamed bytes, MIME and source dimensions refuse before publication",async()=>{
 const cases=[()=>response(png(),"https://b.test"+asset,{"content-type":"image/png","content-length":String(15*1024*1024+1)}),()=>response(new Uint8Array(15*1024*1024+1)),()=>response(png(),"https://b.test"+asset,{"content-type":"text/html"}),()=>response(png(8193,1)),()=>response(png(8192,8192)),()=>response(png(),"https://a.test"+asset)];
 for(const make of cases){const f=harness(),value=await f.trusted(),canvas=f.canvas(f.h.markup(value,ref,f.capture));f.setArt(make);f.h.hydrate(f.root);await f.idle();assert.equal(canvas.draws,0);assert.equal(f.bitmaps.length,0);assert.equal(f.h.snapshot().compressed.bytes,0);assert.equal(f.h.snapshot().pixels.bytes,0);}
});
test("compressed admission never queues a request and rechecks visible DOM only after credit return",async()=>{
 const f=harness(),releases=[];f.setArt((url)=>new Promise(resolve=>releases.push(()=>resolve(response(png(),url)))));
 for(let n=0;n<3;n++){const r={...ref,item_id:String(n+1)},value=await f.trusted(item(r));f.canvas(f.h.markup(value,r,f.capture));}
 f.h.hydrate(f.root);await Promise.resolve();assert.equal(f.requests.length,2);assert.equal(f.h.snapshot().compressed.bytes,60*1024*1024);assert.equal(f.h.snapshot().compressed.active,2);
 releases.shift()();for(let n=0;n<100&&f.requests.length<3;n++)await new Promise(r=>setTimeout(r,1));assert.equal(f.requests.length,3);
 while(releases.length)releases.shift()();await f.idle();assert.equal(f.canvases.filter(c=>c.draws===1).length,3);f.h.retire();assert.equal(f.h.snapshot().pixels.bytes,0);
});
test("actual logout and route entry retire artwork before current authority changes",async()=>{
 let retired=0;const sentinel=new Error("remaining logout already covered by auth suite");
 const auth=vm.createContext({AUTH_GENERATION:4,sharedArtworkRetire(){retired++;},stopLiveTv(){throw sentinel;}});
 vm.runInContext(fs.readFileSync("crates/plurxd/src/web/core/auth.js","utf8"),auth);
 assert.equal(vm.runInContext("clearLocalSession(3)",auth),false);assert.equal(retired,0);
 assert.throws(()=>vm.runInContext("clearLocalSession(4)",auth),e=>e===sentinel);assert.equal(retired,1);
 const router=vm.createContext({window:{addEventListener(){}},NATIVE_READER_BOOT:false,boot(){},TOKEN:null,ME:null,WATCH:null,PLAYER:null,PAGE_RENDER_GENERATION:0,sharedArtworkRetire(){retired++;}});
 vm.runInContext(fs.readFileSync("crates/plurxd/src/web/player/autoplay-next.js","utf8"),router);
 vm.runInContext(fs.readFileSync("crates/plurxd/src/web/router.js","utf8"),router);await vm.runInContext("render()",router);assert.equal(retired,2);
});
test("unconfirmed bitmap retirement remains charged and current403 retires only its Source",async()=>{
 const f=harness(),value=await f.trusted(),canvas=f.canvas(f.h.markup(value,ref,f.capture));
 f.setDecode(async(blob,options)=>({width:options.resizeWidth,height:options.resizeHeight,close(){throw new Error("physical release unconfirmed");}}));f.h.hydrate(f.root);await f.idle();
 assert.equal(canvas.width,0);assert.equal(f.h.snapshot().unresolved,1);assert.ok(f.h.snapshot().pixels.bytes>0);f.h.retire();assert.ok(f.h.snapshot().pixels.bytes>0);
 const g=harness(),one=await g.trusted(),otherRef={...ref,import_id:"44444444-4444-4444-8444-444444444444"},other=await g.trusted(item(otherRef,asset.replace(ref.import_id,otherRef.import_id)));
 g.canvas(g.h.markup(one,ref,g.capture));g.h.markup(other,otherRef,g.capture);g.setArt(()=>response("forbidden","https://b.test"+asset,{"content-type":"text/plain"},403));g.h.hydrate(g.root);await g.idle();
 assert.equal(g.h.snapshot().contexts,1);assert.equal(g.context.TOKEN,"bearer-a");assert.throws(()=>g.h.markup(one,ref,g.capture),/authenticated/);assert.match(g.h.markup(other,otherRef,g.capture),/data-shared-art/);
});

test("retirement cancels actual metadata reader even before auth generation changes",async()=>{
 const f=harness();let cancelled=false;f.setMetadataResponse(path=>response(new ReadableStream({pull(){return new Promise(()=>{});},cancel(){cancelled=true;}}),"https://b.test/api/v1"+path,{"content-type":"application/json"}));
 const pending=f.h.metadata("/shared/libraries",f.capture);for(let n=0;n<5;n++)await Promise.resolve();f.h.retire();await assert.rejects(pending);assert.equal(cancelled,true);assert.equal(f.h.snapshot().metadata,0);assert.equal(f.context.AUTH_GENERATION,1);
});

test("leaving retires metadata proofs even before page generation changes",async()=>{const f=harness(),value=await f.trusted();f.h.markup(value,ref,f.capture);f.h.retire();assert.throws(()=>f.h.markup(value,ref,f.capture),/authenticated/);const fresh=await f.trusted();assert.match(f.h.markup(fresh,ref,f.capture),/data-shared-art/);});
