"use strict";
// Shipping assets + intercepted synthetic data. Native acceptance is separate.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright"),{test}=require("node:test");
const assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path"),cp=require("node:child_process");
const root=path.resolve(__dirname,"../../crates/plurxd/src/web"),output=process.env.MERIDIAN_SCREENSHOTS;
const names=["The Far Shore","Signal at Dawn","Northbound","Low Orbit","After the Rain","A Place Between"];
const libs=[{id:1,name:"Movies",kind:"movies",item_count:80},{id:2,name:"Shows",kind:"shows",item_count:12},{id:3,name:"Books & photographs — a deliberately long library name",kind:"books",item_count:6}];
const items=Array.from({length:80},(_,i)=>({id:i+1,library_id:1,kind:"movie",title:names[i%6]+(i>=6?" "+(i+1):""),year:2026,resolution:2160,runtime_ms:6480000,overview:"A cartographer returns to a forgotten coast, following a signal that should have faded years ago.",poster:"/fixture-art/"+(i%6)+".svg",backdrop:"/fixture-art/hero.svg",watch:{position_ms:i<4?1800000:0,duration_ms:6480000,watched:i%4===3}}));
const files=id=>[{id:100+Number(id),filename:"Synthetic coast.mp4",available:true,duration_ms:6480000,video_codec:"h264",width:3840,height:2160,size:1024,audio_streams:[{index:0,language:"eng",codec:"aac"},{index:1,language:"fra",codec:"aac"}],subtitle_streams:[{index:2,language:"eng",codec:"subrip",text:true}],subtitle_generation_enabled:true}];
const artwork=name=>`<svg xmlns="http://www.w3.org/2000/svg" width="960" height="600" viewBox="0 0 960 600"><defs><linearGradient id="sky" x2=".8" y2="1"><stop stop-color="#bb926c"/><stop offset=".5" stop-color="#426573"/><stop offset="1" stop-color="#183542"/></linearGradient></defs><rect width="960" height="600" fill="url(#sky)"/><circle cx="730" cy="110" r="65" fill="#ddc293"/><path d="M0 600L230 170 380 370 610 140 960 600Z" fill="#203f4c"/><path d="M0 600L360 330 600 500 810 280 960 600Z" fill="#152d3a"/><text x="60" y="535" fill="#fff" font-family="sans-serif" font-size="45" letter-spacing="6">${name.toUpperCase()}</text></svg>`;
async function fixture(browser,options={}){
 const page=await browser.newPage({viewport:options.viewport||{width:1440,height:1080},hasTouch:!!options.tv,reducedMotion:"reduce"});
 const errors=[],requests=[];page.on("pageerror",e=>errors.push(e.message));
 await page.addInitScript(options=>{localStorage.setItem("plurx_token","fixture");localStorage.setItem("plurx_layout",options.layout||"meridian");localStorage.setItem("plurx_theme",options.theme||"meridian");localStorage.setItem("plurx_appearance",options.mode||"dark");localStorage.setItem("plurx_library_view","grid");localStorage.setItem("plurx_perpage","50");},options);
 await page.route("**/*",async route=>{
  const u=new URL(route.request().url()),p=u.pathname,key=p.replace("/api/v1","");
  if(u.hostname!=="meridian.test")return route.abort();
  if(p==="/"||p.startsWith("/assets/")){
   const file=path.join(root,p==="/"?"index.html":p.slice(8));if(!fs.existsSync(file))return route.fulfill({status:404,body:""});
   const type=p==="/"?"text/html":p.endsWith(".css")?"text/css":p.endsWith(".png")?"image/png":"text/javascript";
   return route.fulfill({contentType:type,body:fs.readFileSync(file)});
  }
  if(p.startsWith("/fixture-art/"))return route.fulfill({contentType:"image/svg+xml",body:artwork(p.includes("hero")?"":names[Number(p.split('/').pop().split('.')[0])]||"Cinema fixture")});
  requests.push({key,method:route.request().method()});
  if(options.before)await options.before(key);
  let data={};
  if(key==="/server")data={name:"Cinema",version:"M2 synthetic preview",setup_required:false};
  else if(key==="/me")data={id:1,username:options.admin?"Administrator":"Viewer",is_admin:!!options.admin};
  else if(key==="/features")data={live_tv:true,recordings:true,library_channels:true,shared_libraries:true};
  else if(key==="/libraries")data=libs;
  else if(key==="/hubs")data={continue_watching:items.slice(0,4),next_up:items.slice(6,12),recently_added:items.slice(12,24)};
  else if(key==="/home/previews")data={libraries:libs.map(l=>({library:l,items:items.slice(0,12).map(i=>({...i,library_id:l.id})),total:80}))};
  else if(key==="/coming-soon")data={configured:false,entries:[]};
  else if(/^\/libraries\/\d+\/items$/.test(key))data={items,total:items.length};
  else if(/^\/items\/\d+$/.test(key)){const item=items.find(i=>i.id===Number(key.split('/')[2]));data={item,files:files(item.id),ancestors:[],children:[]};}
  else if(key==="/search")data={items,total:items.length};
  else if(key==="/dvr/reminders/due")data=[];
  else if(key==="/dvr/overview")data={counts:{},active:[],availability:"complete"};
  else if(key==="/activity")data={streams:0,scanning:0};
  return route.fulfill({contentType:"application/json",body:JSON.stringify(data)});
 });
 return {page,errors,requests};
}
function assertClean(errors){assert.deepEqual(errors,[],"shipping app has no script errors");}
async function capture(page,id,metadata){
 if(!output)return;
 fs.mkdirSync(output,{recursive:true});await page.screenshot({path:path.join(output,id+".png"),fullPage:false});
 const manifestPath=path.join(output,"manifest.json"),manifest=fs.existsSync(manifestPath)?JSON.parse(fs.readFileSync(manifestPath)):[];
 manifest.push({id,sourceCommit:cp.execFileSync("git",["rev-parse","HEAD"],{encoding:"utf8"}).trim(),sourceDirty:!!cp.execFileSync("git",["status","--porcelain"],{encoding:"utf8"}).trim(),platform:"web shipping assets / synthetic API fixture",viewport:page.viewportSize(),runtime:"Chromium "+page.context().browser().version(),route:new URL(page.url()).hash,...metadata,screenshot:id+".png",reproduction:"MERIDIAN_SCREENSHOTS=<output> node --test tests/web/meridian.browser.cjs"});fs.writeFileSync(manifestPath,JSON.stringify(manifest,null,2)+"\n");
}
test("Meridian renders responsive Home, library, detail and source navigation",async()=>{
 const browser=await chromium.launch({headless:true});
 try{
  for(const width of [320,390,768,1024,1440,1936]){
   const {page,errors}=await fixture(browser,{viewport:{width,height:1080}});
   await page.goto("http://meridian.test/#/");await page.waitForSelector(".md-home [data-remote-item]");await page.waitForFunction(()=>document.querySelector('.md-home [data-home-slot="hero"] .btnplay'));
   assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,"Home fits "+width);
   if(width===390||width===1440)await capture(page,"home-dark-"+width,{layout:"meridian",theme:"meridian",appearance:"dark"});
   await page.goto("http://meridian.test/#/library/1");await page.waitForSelector("#libbody [data-remote-item]");
   assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,"library fits "+width);
   await capture(page,"library-"+width,{layout:"meridian",theme:"meridian",appearance:"dark"});
   await page.goto("http://meridian.test/#/item/1");await page.waitForSelector(".md-detail .btnplay");
   assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,"detail fits "+width);
   if(width===390||width===1440)await capture(page,"detail-"+width,{layout:"meridian",theme:"meridian",appearance:"dark"});
   assertClean(errors);await page.close();
  }
 }finally{await browser.close();}
});
test("layout switching preserves library owners, query, paging and focus",async()=>{
 const browser=await chromium.launch({headless:true});
 try{
  const {page,errors,requests}=await fixture(browser,{layout:"classic"});await page.goto("http://meridian.test/#/library/1");await page.waitForSelector("#libbody [data-remote-item]");
  await page.evaluate(()=>{LIB_FIND="Shore";LIB_SCOPE="1";LIB_PER=5;LIB_PAGE_AT=1;LIB_VIEW.draw(true);window.retained={main:document.getElementById("main"),body:document.getElementById("libbody"),view:LIB_VIEW,generation:PAGE_RENDER_GENERATION,hash:location.hash,history:history.length,page:LIB_PAGE_AT,card:document.querySelector("#libbody [data-remote-item]")};});
  await page.locator("#libbody [data-remote-item]").first().focus();
  const before=requests.filter(r=>r.key.includes('/items')).length;
  for(const layout of ["meridian","catalog","theater","classic","meridian"]){
   const state=await page.evaluate(layout=>{setLayout(layout);return {sameMain:retained.main===document.getElementById("main"),sameBody:retained.body===document.getElementById("libbody"),sameView:retained.view===LIB_VIEW,generation:PAGE_RENDER_GENERATION,hash:location.hash,history:history.length,find:LIB_FIND,scope:LIB_SCOPE,page:LIB_PAGE_AT,focused:document.activeElement===retained.card};},layout);
   assert.ok(state.sameMain&&state.sameBody&&state.sameView,layout+" retains owners");assert.equal(state.find,"Shore");assert.equal(state.scope,"1");assert.equal(state.page,1);assert.equal(state.focused,true);
   const expected=await page.evaluate(()=>({generation:retained.generation,hash:retained.hash,history:retained.history}));assert.equal(state.generation,expected.generation);assert.equal(state.hash,expected.hash);assert.equal(state.history,expected.history);
  }
  assert.equal(requests.filter(r=>r.key.includes('/items')).length,before,"shell switch does not refetch");assertClean(errors);
 }finally{await browser.close();}
});
test("Home requests complete once into the new layout while still pending",async()=>{
 const browser=await chromium.launch({headless:true});let release;const pending=new Promise(resolve=>release=resolve);
 try{
  const {page,errors,requests}=await fixture(browser,{layout:"classic",before:key=>['/hubs','/home/previews','/coming-soon'].includes(key)?pending:undefined});
  await page.goto("http://meridian.test/#/");await page.waitForSelector("#main");await page.evaluate(()=>{window.generation=PAGE_RENDER_GENERATION;setLayout("meridian");});release();
  await page.waitForSelector(".md-home [data-remote-item]");assert.equal(await page.evaluate(()=>PAGE_RENDER_GENERATION===window.generation),true);
  for(const key of ['/hubs','/home/previews','/coming-soon'])assert.equal(requests.filter(r=>r.key===key).length,1,key);
  assertClean(errors);
 }finally{release();await browser.close();}
});
test("TV panel cancel/current/source selection owns physical browser focus and one Back",async()=>{
 const browser=await chromium.launch({headless:true});
 try{
  const {page,errors}=await fixture(browser,{tv:true,viewport:{width:1920,height:1080}});await page.goto("http://meridian.test/#/");await page.waitForSelector(".md-home [data-remote-item]");
  await page.locator(".md-opener").click();await page.waitForSelector(".md-panel[open]");assert.equal(await page.evaluate(()=>document.querySelector('.md-panel').contains(document.activeElement)),true);
  await capture(page,"tv-expanded",{layout:"meridian",theme:"meridian",appearance:"dark",focus:"actual browser focus in panel; not native focus"});
  const hash=await page.evaluate(()=>location.hash);await page.keyboard.press("Escape");assert.equal(await page.locator(".md-panel").count(),0);assert.equal(await page.evaluate(()=>location.hash),hash);assert.equal(await page.evaluate(()=>document.activeElement.matches(".md-opener")),true);
  await page.locator(".md-opener").click();await page.locator('.md-panel a[data-meridian-destination="home"]').click();assert.equal(await page.locator(".md-panel").count(),0);assert.equal(await page.evaluate(()=>location.hash),hash);
  await page.locator(".md-opener").click();await page.locator('.md-panel a[data-meridian-destination="library:1"]').click();await page.waitForSelector("#libbody [data-remote-item]");await page.waitForFunction(()=>document.getElementById("main").contains(document.activeElement),null,{timeout:3000}).catch(async error=>{console.error("FOCUS",await page.evaluate(()=>({ticket:MERIDIAN_FOCUS_TICKET,revision:MERIDIAN_NAV_REVISION,generation:PAGE_RENDER_GENERATION,hash:location.hash,active:document.activeElement.outerHTML.slice(0,350),errors:document.querySelector(".empty")?.textContent})));throw error;});
  await capture(page,"tv-library",{layout:"meridian",theme:"meridian",appearance:"dark",focus:"actual browser content focus; not native focus"});assertClean(errors);
 }finally{await browser.close();}
});
test("item layout switches retain PREPLAY and the exact asynchronous subtitle panel",async()=>{
 const browser=await chromium.launch({headless:true});
 try{
  const {page,errors,requests}=await fixture(browser,{layout:"classic"});await page.goto("http://meridian.test/#/item/1");await page.waitForSelector(".btnplay");
  await page.evaluate(()=>{setPrePlay(101,"subtitle","-1");window.preplay=PREPLAY;window.itemPage=WATCH_ITEM_PAGE;const version=document.querySelector(".version");const panel=document.createElement("div");panel.id="subtitle-generate-101";panel._subtitleGeneration={running:true};version.append(panel);window.subtitlePanel=panel;});
  const before=requests.filter(r=>/^\/items\/\d+$/.test(r.key)).length;
  for(const layout of ["meridian","catalog","theater","classic"]){await page.evaluate(layout=>setLayout(layout),layout);assert.equal(await page.evaluate(()=>PREPLAY===window.preplay&&WATCH_ITEM_PAGE===window.itemPage&&document.getElementById("subtitle-generate-101")===window.subtitlePanel&&window.subtitlePanel._subtitleGeneration.running),true,layout);}
  assert.equal(requests.filter(r=>/^\/items\/\d+$/.test(r.key)).length,before);assertClean(errors);
 }finally{await browser.close();}
});

test("Meridian roles render across themes and appearances without leaking on exit",async()=>{
 const browser=await chromium.launch({headless:true});
 try{
  const {page,errors}=await fixture(browser);await page.goto("http://meridian.test/#/");await page.waitForSelector(".md-home .btnplay");
  for(const theme of ["meridian","terminal","classic"])for(const mode of ["dark","light"]){
   await page.evaluate(({theme,mode})=>{setTheme(theme);setAppearance(mode);},{theme,mode});
   const roles=await page.evaluate(()=>{const root=getComputedStyle(document.documentElement),button=getComputedStyle(document.querySelector('.md-home .btnplay')),selected=getComputedStyle(document.querySelector('.md-nav [aria-current="page"]'));const hex=rgb=>"#"+rgb.match(/[0-9]+/g).slice(0,3).map(n=>Number(n).toString(16).padStart(2,"0")).join("");return {button:themeContrast(hex(button.color),hex(button.backgroundColor)),selected:themeContrast(hex(selected.color),hex(selected.backgroundColor)),heroInk:root.getPropertyValue('--hero-ink').trim(),text:root.getPropertyValue('--text').trim()};});
   assert.ok(roles.button>=4.5,theme+'/'+mode+' rendered action');assert.ok(roles.selected>=4.5,theme+'/'+mode+' rendered selection');assert.equal(roles.heroInk,roles.text);
   await capture(page,'home-'+theme+'-'+mode,{layout:'meridian',theme,appearance:mode});
  }
  await page.evaluate(()=>setTheme("meridian"));
  for(const layout of ["classic","catalog","theater","meridian"]){await page.evaluate(layout=>setLayout(layout),layout);await capture(page,'meridian-theme-'+layout,{layout,theme:'meridian',appearance:'light'});}
  assertClean(errors);
 }finally{await browser.close();}
});

test("Settings fields survive switches and companion navigation cannot control restricted content",async()=>{
 const browser=await chromium.launch({headless:true});
 try{
  const {page,errors}=await fixture(browser,{tv:true,viewport:{width:1920,height:1080}});await page.goto("http://meridian.test/#/settings/developer");await page.waitForSelector("#cinema-screen-name");
  await page.locator("#cinema-screen-name").fill("Unsaved screen name");await page.evaluate(()=>window.settingsField=document.getElementById("cinema-screen-name"));
  for(const layout of ["classic","meridian"]){await page.evaluate(layout=>setLayout(layout),layout);assert.equal(await page.evaluate(()=>document.getElementById("cinema-screen-name")===window.settingsField&&window.settingsField.value==="Unsaved screen name"),true);}
  const state=await page.evaluate(()=>{cinemaRemoteBind();return CinemaRemote.snapshot();});assert.equal(state.blocked,false);assert.equal(state.text_nonce,null);
  assert.equal(await page.evaluate(()=>{CinemaRemote.focusById("meridian:open:0");return CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot());}),"applied");
  await page.waitForSelector(".md-panel[open]");assert.equal(await page.evaluate(()=>document.querySelector('.md-panel').contains(document.activeElement)),true);
  assert.equal(await page.evaluate(()=>{CinemaRemote.focusById("meridian:destination:0");return CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot());}),"applied");
  await page.waitForSelector(".md-home [data-remote-item]");assertClean(errors);
 }finally{await browser.close();}
});

test("companion panel Libraries and Help actions are reachable and activate their owners",async()=>{
 const browser=await chromium.launch({headless:true});
 try{
  const {page,errors}=await fixture(browser,{tv:true,viewport:{width:1920,height:1080}});await page.goto("http://meridian.test/#/");await page.waitForSelector(".md-home [data-remote-item]");
  await page.locator(".md-opener").click();
  assert.equal(await page.evaluate(()=>{CinemaRemote.focusById("meridian:libraries");return CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot());}),"applied");
  await page.waitForSelector('.md-panel[aria-label="Libraries"][open]');assert.equal(await page.evaluate(()=>document.querySelector('.md-panel').contains(document.activeElement)),true);
  await page.keyboard.press("Escape");await page.locator(".md-opener").click();
  assert.equal(await page.evaluate(()=>{CinemaRemote.focusById("meridian:help");return CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot());}),"applied");
  await page.waitForSelector("#diagnostic-dialog");assert.equal(await page.locator(".md-panel").count(),0);assertClean(errors);
 }finally{await browser.close();}
});
