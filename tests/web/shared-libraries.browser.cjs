"use strict";
// Exercise the shipped shell: inline panel links cannot pass geometry checks.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const {test}=require("node:test"),assert=require("node:assert/strict");
const fs=require("node:fs"),path=require("node:path"),zlib=require("node:zlib");
const root=path.resolve(__dirname,"../../crates/plurxd/src/web");
const ref={import_id:"11111111-1111-4111-8111-111111111111",server_id:"22222222-2222-4222-8222-222222222222",catalogue_epoch:"33333333-3333-4333-8333-333333333333",library_id:"9007199254740993",item_id:"9223372036854775807"};
const base=`/shared/imports/${ref.import_id}`;
const art=`/api/v1${base}/art/${"A".repeat(272)}`;
const backdrop=`/api/v1${base}/art/${"B".repeat(272)}`;
const chunk=(kind,bytes)=>{const type=Buffer.from(kind),length=Buffer.alloc(4),crc=Buffer.alloc(4);length.writeUInt32BE(bytes.length);crc.writeUInt32BE(zlib.crc32(Buffer.concat([type,bytes])));return Buffer.concat([length,type,bytes,crc]);};
const ihdr=Buffer.alloc(13);ihdr.writeUInt32BE(60);ihdr.writeUInt32BE(90,4);ihdr[8]=8;ihdr[9]=6;
const scanline=Buffer.concat([Buffer.from([0]),Buffer.from(Array.from({length:60},()=>[220,110,70,255]).flat())]);
const png=Buffer.concat([Buffer.from("89504e470d0a1a0a","hex"),chunk("IHDR",ihdr),chunk("IDAT",zlib.deflateSync(Buffer.concat(Array(90).fill(scanline)))),chunk("IEND",Buffer.alloc(0))]);
const movie={source:"shared",reference:ref,title:"13 Hours: The Secret Soldiers of Benghazi",kind:"movie",year:2016,genres:["Drama","War"],overview:"A security team struggles to make sense of the chaos.",art:[{kind:"poster",variant:"w300",url:art},{kind:"backdrop",variant:"w780",url:backdrop}],poster_url:art,backdrop_url:backdrop};
const file={file_id:"9007199254740993",reference:{item:ref,file_id:"9007199254740993"},video_codec:"hevc",width:3840,height:2160,bit_depth:10,hdr:"hdr10",duration_ms:8640000,container:"mkv",size:"12000000000",audio_streams:[{index:0,codec:"eac3",channels:8,language:"eng",title:"Surround"}],subtitle_streams:[{index:1,codec:"subrip",language:"eng",forced:true}]};
const items=Array.from({length:24},(_,n)=>({...movie,reference:{...ref,item_id:String(n+1)},title:n===0?movie.title:`Film ${n+1}`,art:n===1?[]:movie.art,poster_url:n===1?null:art,backdrop_url:n===1?null:backdrop}));

test("shared library grids and enriched details fit every shipped layout on desktop and phone",async()=>{
 const browser=await chromium.launch({headless:true});
 try{
  for(const layout of ["catalog","classic","theater"]){
   const page=await browser.newPage(),errors=[],reads=[];
   page.on("pageerror",e=>errors.push(e.message));
   await page.addInitScript(layout=>{localStorage.setItem("plurx_token","fixture");localStorage.setItem("plurx_layout",layout);},layout);
   await page.route("**/*",async route=>{
    const url=new URL(route.request().url()),p=url.pathname;
    if(url.hostname!=="shared.test")return route.abort();
    if(p==="/"||p.startsWith("/assets/")){
     const filePath=path.join(root,p==="/"?"index.html":p.slice(8));
     if(!fs.existsSync(filePath))return route.fulfill({status:404,body:""});
     return route.fulfill({contentType:p==="/"?"text/html":p.endsWith(".css")?"text/css":"text/javascript",body:fs.readFileSync(filePath)});
    }
    if(p===art||p===backdrop)return route.fulfill({contentType:"image/png",body:png});
    const key=p.replace("/api/v1","");reads.push(key+url.search);
    let data={};
    if(key==="/server")data={name:"Cinema",setup_required:false};
    else if(key==="/me")data={id:1,username:"viewer",is_admin:false};
    else if(key==="/libraries"||key==="/dvr/reminders/due")data=[];
    else if(key==="/shared/libraries")data={libraries:[{...ref,source_name:"Shared source"}]};
    else if(key==="/shared/continue-watching")data={groups:[ref]};
    else if(key===base+"/libraries")data={...ref,libraries:[{library_id:ref.library_id,name:"Movies"}]};
    else if(key===base+"/continue-watching")data={...ref,availability:"online",items:items.slice(0,4).map(item=>({item}))};
    else if(key===base+`/libraries/${ref.library_id}/items`)data={items:url.searchParams.has("q")?[items[0]]:items,next_cursor:url.searchParams.has("q")||url.searchParams.has("cursor")?null:"next-page"};
    else if(key===base+`/items/${ref.item_id}`)data={item:{...movie,presentation:{runtime_ms:8640000,air_date:"2016-01-15",ratings:[{source:"imdb",value:7.3,scale:10,votes:12000}]}},files:[file],watch:{watched:false},delivery_status:"available"};
    else if(key===base+"/items/100")data={item:{...movie,reference:{...ref,item_id:"100"},kind:"show"},files:[],delivery_status:"unavailable"};
    else if(key===base+"/items/100/children")data={items:items.slice(0,4).map(item=>({...item,kind:"season"})),next_cursor:null};
    return route.fulfill({contentType:"application/json",body:JSON.stringify(data)});
   });
   for(const width of [1646,1024,768,390]){
    const label=`${layout} at ${width}px`;
    await page.setViewportSize({width,height:1000});
    const library=`#/shared/library/${ref.import_id}/${ref.server_id}/${ref.catalogue_epoch}/${ref.library_id}`;
    await page.goto("http://shared.test/"+library);
    await page.waitForFunction(()=>document.querySelectorAll("#shared-items .poster").length===24);
    await page.waitForSelector('#shared-items canvas[data-shared-art-ready="true"]');
    if(process.env.SHARED_SCREENSHOTS){fs.mkdirSync(process.env.SHARED_SCREENSHOTS,{recursive:true});await page.screenshot({path:path.join(process.env.SHARED_SCREENSHOTS,`${layout}-library-${width}.png`),fullPage:true});}
    const boxes=await page.evaluate(()=>{
     const box=el=>{const r=el.getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height,right:r.right,bottom:r.bottom};};
     const cards=[...document.querySelectorAll("#shared-items .poster")];
     const mount=document.querySelector("#shared-catalogue");
     return {search:box(document.querySelector("#shared-search")),grid:box(document.querySelector("#shared-items")),cards:cards.slice(0,4).map(box),art:box(cards[0].querySelector(".art")),canvas:box(cards[0].querySelector("canvas")),title:box(cards[0].querySelector(".t")),more:box(document.querySelector("[data-shared-more]")),overflow:mount.scrollWidth>mount.clientWidth||mount.getBoundingClientRect().right>innerWidth};
    });
    assert.ok(!boxes.overflow,`${label}: no horizontal overflow: ${JSON.stringify(boxes)}`);
    assert.ok(boxes.search.bottom<=boxes.grid.y,`${label}: search above grid`);
    assert.equal(boxes.cards[0].y,boxes.cards[1].y,`${label}: multiple columns`);
    assert.ok(boxes.cards[1].x>=boxes.cards[0].right,`${label}: no overlapping cards`);
    assert.ok(Math.abs(boxes.art.height/boxes.art.width-1.5)<.03,`${label}: poster aspect ratio`);
    assert.ok(Math.abs(boxes.canvas.width-boxes.art.width)<1,`${label}: artwork fills poster`);
    assert.ok(boxes.title.y>=boxes.art.bottom-1,`${label}: title below art`);
    assert.ok(boxes.more.y>boxes.cards[0].bottom,`${label}: pagination below cards`);
    await page.getByRole("button",{name:"Load more",exact:true}).click();
    assert.equal(await page.locator("#shared-items .poster").count(),24,"pagination deduplicates items");
    await page.getByLabel("Search this library").fill("13 Hours");
    await page.locator("#shared-search").getByRole("button",{name:"Search",exact:true}).click();
    await page.waitForFunction(()=>document.querySelectorAll("#shared-items .poster").length===1);
    assert.ok(reads.some(r=>r.includes("q=13%20Hours")),"search remains on shared API");
    await page.goto(`http://shared.test/#/shared/item/${ref.import_id}/${ref.server_id}/${ref.catalogue_epoch}/${ref.library_id}/${ref.item_id}`);
    await page.getByRole("heading",{name:movie.title,exact:true}).waitFor();
    await page.waitForSelector('.shared-backdrop canvas[data-shared-art-ready="true"]');
    assert.ok(await page.evaluate(()=>{const canvas=document.querySelector(".shared-backdrop canvas").getBoundingClientRect(),hero=document.querySelector(".shared-detail-hero").getBoundingClientRect();return Math.abs(canvas.width-(hero.width-2))<2&&Math.abs(canvas.height-(hero.height-2))<2;}),`${label}: backdrop fills hero`);
    const detail=await page.locator("#shared-catalogue").innerText();
    for(const text of ["2016","Drama","War","IMDb","7.3","2160P","HEVC","HDR10","7.1","English","forced","MKV"])assert.ok(detail.includes(text),`${label}: ${text} displayed: ${detail}`);
    assert.equal(await page.locator("[data-shared-play]").count(),1);
    assert.ok(await page.evaluate(()=>{const el=document.querySelector("#shared-catalogue");return el.scrollWidth<=el.clientWidth&&el.getBoundingClientRect().right<=innerWidth;}),`${label}: detail fits`);
    if(process.env.SHARED_SCREENSHOTS){fs.mkdirSync(process.env.SHARED_SCREENSHOTS,{recursive:true});await page.screenshot({path:path.join(process.env.SHARED_SCREENSHOTS,`${layout}-detail-${width}.png`),fullPage:true});}
    await page.goto("http://shared.test/#/shared");
    await page.waitForFunction(()=>document.querySelectorAll('[id^="shared-continue-"] .poster').length===4);
    assert.equal(await page.locator(".shared-continue-rail").count(),1);
    assert.ok(await page.locator(".shared-source").evaluate(el=>el.getBoundingClientRect().width<=560),`${label}: one share stays compact`);
    assert.equal(await page.locator("[data-shared-rename]").count(),0,"viewer cannot rename shares");
    if(process.env.SHARED_SCREENSHOTS&&layout==="classic"){await page.screenshot({path:path.join(process.env.SHARED_SCREENSHOTS,`hub-single-${width}.png`),fullPage:true});}
    await page.goto(`http://shared.test/#/shared/item/${ref.import_id}/${ref.server_id}/${ref.catalogue_epoch}/${ref.library_id}/100`);
    await page.waitForFunction(()=>document.querySelectorAll("#shared-children .poster").length===4);
    assert.equal(await page.locator("#shared-children.grid").count(),1);
    assert.ok(await page.evaluate(()=>{const el=document.querySelector("#shared-catalogue");return el.scrollWidth<=el.clientWidth&&el.getBoundingClientRect().right<=innerWidth;}),`${label}: seasons fit`);
   }
   assert.deepEqual(errors,[],`${layout}: no runtime errors`);
   assert.ok(!reads.some(r=>/^\/items\//.test(r)),"Shared UI never calls local item APIs");
   await page.close();
  }
 }finally{await browser.close();}
});

test("shared hub scales to many named shares, filters locally, and renames without replacing artwork",async()=>{
 const browser=await chromium.launch({headless:true});
 const names=["Alex’s Cinema","André’s Film Club","Blair & Family","Casey’s Collection","Drew’s Movies","Emery’s Vault","Finley’s Cinema","Harper’s Library","Indigo TV","Jordan’s Shelf","Kai’s Cinema","Lane’s Media","Morgan’s Movies","Noel’s Share","Oakley’s Library","Parker’s Place","Quinn’s Queue","Reese’s Reels","Riley’s Family","Sam’s Cinema","Taylor’s TV","Val’s Vault","Wren’s Library","Zuri’s Collection"];
 const groups=names.map((source_name,n)=>({import_id:`${String(n+1).padStart(8,"0")}-1111-4111-8111-111111111111`,server_id:`${String(n+101).padStart(8,"0")}-2222-4222-8222-222222222222`,catalogue_epoch:ref.catalogue_epoch,source_name}));
 const fixtureLibraries=[{library_id:"1",name:"Movies",kind:"movies"},{library_id:"2",name:"TV shows",kind:"shows"},{library_id:"3",name:"Documentaries",kind:"movies"}];
 const requests=[],errors=[];let active=0,maximum=0;
 try{
  const page=await browser.newPage();await page.emulateMedia({colorScheme:"dark"});
  page.on("pageerror",error=>errors.push(error.message));
  await page.addInitScript(()=>{localStorage.setItem("plurx_token","fixture");localStorage.setItem("plurx_layout","classic");});
  await page.route("**/*",async route=>{
   const url=new URL(route.request().url()),p=url.pathname;
   if(url.hostname!=="hub.test")return route.abort();
   if(p==="/"||p.startsWith("/assets/")){
    const filePath=path.join(root,p==="/"?"index.html":p.slice(8));if(!fs.existsSync(filePath))return route.fulfill({status:404,body:""});
    return route.fulfill({contentType:p==="/"?"text/html":p.endsWith(".css")?"text/css":"text/javascript",body:fs.readFileSync(filePath)});
   }
   if(p.includes("/art/"))return route.fulfill({contentType:"image/png",body:png});
   const key=p.replace("/api/v1","");requests.push({path:key,method:route.request().method(),body:route.request().postDataJSON()});
   let data={};
   if(key==="/server")data={name:"Cinema",setup_required:false};
   else if(key==="/me")data={id:1,username:"owner",is_admin:true};
   else if(key==="/libraries"||key==="/dvr/reminders/due")data=[];
   else if(key==="/shared/libraries")data={libraries:groups.flatMap(group=>fixtureLibraries.map(lib=>({...group,library_id:lib.library_id})))};
   else if(key==="/shared/continue-watching")data={groups};
   else if(key.startsWith("/sharing/imports/")&&key.endsWith("/name")){
    const group=groups.find(group=>key.includes(group.import_id)),body=route.request().postDataJSON();
    assert.ok(group);assert.equal(route.request().method(),"PUT");assert.equal(body.expected_name,group.source_name);group.source_name=body.name;
    return route.fulfill({contentType:"application/json",body:JSON.stringify({updated:true})});
   }else if(key.startsWith("/shared/imports/")){
    const group=groups.find(group=>key.includes(group.import_id));assert.ok(group);
    active++;maximum=Math.max(maximum,active);await new Promise(resolve=>setTimeout(resolve,10));active--;
    if(group===groups[5])return route.fulfill({status:503,contentType:"application/json",body:JSON.stringify({message:"Share unavailable"})});
    if(key.endsWith("/libraries"))data={...group,libraries:fixtureLibraries};
    else if(key.endsWith("/continue-watching"))data={...group,availability:"online",items:Array.from({length:80},(_,n)=>{
     const poster=`/api/v1/shared/imports/${group.import_id}/art/${"A".repeat(272)}`;
     return {item:{source:"shared",reference:{import_id:group.import_id,server_id:group.server_id,catalogue_epoch:group.catalogue_epoch,library_id:"1",item_id:String(n+1)},title:`Film ${n+1}`,kind:"movie",year:2024,art:[{kind:"poster",variant:"w300",url:poster}],poster_url:poster}};
    })};
   }
   return route.fulfill({contentType:"application/json",body:JSON.stringify(data)});
  });
  for(const width of [1646,1024,768,390]){
   await page.setViewportSize({width,height:1100});await page.goto("http://hub.test/#/shared");
   await page.waitForFunction(()=>document.querySelectorAll('.shared-source-status[data-state="ready"]').length===23);
   await page.waitForFunction(()=>document.querySelectorAll(".shared-continue-rail .poster").length===23);
   assert.equal(await page.locator(".shared-source").count(),24);assert.equal(await page.locator(".shared-library-link").count(),69);
   assert.ok(maximum<=4,"Source metadata requests remain bounded");
   assert.ok(await page.evaluate(()=>{const el=document.querySelector("#shared-catalogue");return el.scrollWidth<=el.clientWidth&&el.getBoundingClientRect().right<=innerWidth;}),`${width}px: hub stays inside viewport`);
   const columns=await page.locator(".shared-sources-grid").evaluate(el=>getComputedStyle(el).gridTemplateColumns.split(" ").length);
   assert.ok(columns>=(width>=768?2:1),`${width}px: sources form a responsive directory`);
   const before=requests.length;
   await page.locator("#shared-source-search").fill("Andre");assert.equal(await page.locator(".shared-source:visible").count(),1);
   assert.ok((await page.locator(".shared-source:visible").innerText()).includes("André’s Film Club"));
   assert.equal(await page.locator(".shared-continue-rail .poster:visible").count(),1);
   await page.locator("#shared-source-search").fill("Documentaries");assert.equal(await page.locator(".shared-source:visible").count(),23);
   await page.locator("#shared-source-search").fill("nothing matches this");await page.locator("#shared-no-matches").waitFor({state:"visible"});
   assert.equal(await page.locator("#shared-continue-section").isVisible(),false);
   await page.locator("#shared-source-search").fill("");await page.locator("#shared-source-availability").selectOption("unavailable");
   assert.equal(await page.locator(".shared-source:visible").count(),1);assert.ok((await page.locator(".shared-source:visible").innerText()).includes("Emery’s Vault"));
   await page.locator("#shared-source-availability").selectOption("all");assert.equal(requests.length,before,"filters do not refetch remote shares");
   if(process.env.SHARED_SCREENSHOTS){fs.mkdirSync(process.env.SHARED_SCREENSHOTS,{recursive:true});await page.screenshot({path:path.join(process.env.SHARED_SCREENSHOTS,`hub-many-${width}.png`),fullPage:true});}
  }
  await page.setViewportSize({width:1646,height:1100});
  const canvas=await page.locator("#shared-continue-0 canvas").elementHandle();assert.ok(canvas);
  await page.getByRole("button",{name:"More titles from Alex’s Cinema",exact:true}).click();
  assert.equal(await page.locator("#shared-continue-0 .poster").count(),11,"bounded batches keep the rest of a share’s history discoverable");
  assert.ok(await canvas.evaluate(el=>el.isConnected),"expanding history keeps existing artwork");
  const writesBefore=requests.filter(request=>request.method==="PUT").length;
  await page.getByRole("button",{name:"Rename Alex’s Cinema",exact:true}).click();
  await page.getByRole("dialog").getByLabel("Share name").fill("Alex’s Weekend Cinema");
  await page.getByRole("button",{name:"Save name",exact:true}).click();
  await page.getByRole("heading",{name:"Alex’s Weekend Cinema",exact:true}).waitFor();
  assert.equal(requests.filter(request=>request.method==="PUT").length,writesBefore+1);
  assert.ok(await canvas.evaluate(el=>el.isConnected),"renaming preserves the authenticated canvas node");
  assert.ok((await page.locator("#shared-continue-0 .shared-card-source").allTextContents()).every(name=>name==="Alex’s Weekend Cinema"));
  await page.getByRole("button",{name:"Rename Alex’s Weekend Cinema",exact:true}).click();await page.getByRole("button",{name:"Cancel",exact:true}).click();
  assert.equal(requests.filter(request=>request.method==="PUT").length,writesBefore+1,"cancel does not save");
  groups[0].source_name="";
  await page.reload();await page.getByRole("button",{name:"Rename Shared source",exact:true}).click();
  assert.equal(await page.getByRole("dialog").getByLabel("Share name").inputValue(),"","rename compares the exact stored label, not its display fallback");
  await page.getByRole("dialog").getByLabel("Share name").fill("Alex’s Cinema");
  await page.getByRole("button",{name:"Save name",exact:true}).click();
  await page.getByRole("heading",{name:"Alex’s Cinema",exact:true}).waitFor();
  assert.deepEqual(errors,[]);assert.ok(!requests.some(request=>/^\/items\//.test(request.path)),"all source navigation keeps shared identities");
 }finally{await browser.close();}
});
