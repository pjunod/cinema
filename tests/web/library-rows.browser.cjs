"use strict";
// Full shipped shell and real browser layout. No daemon or private media needed.
// PLAYWRIGHT_MODULE points at an existing installation when supplied.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const {test}=require("node:test"),assert=require("node:assert/strict");
const fs=require("node:fs"),path=require("node:path");
const root=path.resolve(__dirname,"../../crates/plurxd/src/web");
const movie=(id,title,extra={})=>({id,library_id:1,kind:"movie",title,sort_title:title.toLowerCase(),year:2024,added_at:1700000000,resolution:1080,...extra});
const large=[...Array.from({length:420},(_,i)=>movie(i+1,`Atlas ${String(i).padStart(3,"0")}`)),movie(421,"Zodiac")];
async function setup(browser,{layout="catalog",width=1280,items=large,libraries=[{id:1,name:"Movies",kind:"movies"}],reply=null}={}){
  const page=await browser.newPage({viewport:{width,height:1000},reducedMotion:"reduce"}),errors=[],requests=[];
  page.on("pageerror",e=>errors.push(e.message));
  await page.addInitScript(layout=>{
    localStorage.setItem("plurx_token","fixture");localStorage.setItem("plurx_layout",layout);
    localStorage.setItem("plurx_theme","noirr");localStorage.setItem("plurx_appearance","dark");localStorage.setItem("plurx_perpage","20");
  },layout);
  await page.route("**/*",async route=>{
    const url=new URL(route.request().url()),p=url.pathname;
    if(url.hostname!=="library.test")return route.abort();
    if(p==="/"||p.startsWith("/assets/")){
      const file=path.join(root,p==="/"?"index.html":p.slice(8));
      return fs.existsSync(file)?route.fulfill({contentType:p==="/"?"text/html":p.endsWith(".css")?"text/css":"text/javascript",body:fs.readFileSync(file)}):route.fulfill({status:404,body:""});
    }
    const key=p.replace("/api/v1","");let data={};
    if(key==="/server")data={name:"Cinema",setup_required:false};
    else if(key==="/me")data={id:1,username:"viewer",is_admin:false};
    else if(key==="/libraries")data=libraries;
    else if(/^\/libraries\/\d+\/items$/.test(key)){
      requests.push(url.href);
      if(reply){const response=await reply(url);if(response)return route.fulfill(response);}
      const id=Number(key.split("/")[2]),offset=Number(url.searchParams.get("offset")||0);
      const list=items.filter(it=>it.library_id===id);
      data={items:list.slice(offset,offset+200),total:list.length};
    }else if(key==="/dvr/reminders/due")data=[];
    return route.fulfill({contentType:"application/json",body:JSON.stringify(data)});
  });
  return {page,errors,requests};
}
async function loaded(page){await page.waitForFunction(()=>LIB_VIEW&&LIB_VIEW.done);}
function snapshot(page,name){
  if(!process.env.LIBRARY_SCREENSHOTS)return;
  fs.mkdirSync(process.env.LIBRARY_SCREENSHOTS,{recursive:true});
  return page.screenshot({path:path.join(process.env.LIBRARY_SCREENSHOTS,`${name}.png`),fullPage:false});
}

test("grouped rows reach every item and View all preserves the row position",async()=>{
  const browser=await chromium.launch({headless:true});
  try{
    const {page,errors,requests}=await setup(browser);
    await page.goto("http://library.test/#/library/1");await loaded(page);
    assert.equal(await page.locator(".library-group").count(),2);
    assert.equal(await page.locator("#library-row-A .library-group-count").textContent(),"420");
    assert.equal(await page.locator("#library-row-A .poster").count(),40,"initial DOM is bounded per row");
    assert.equal(requests.length,3,"all server pages are fetched independently of the 20-item grid preference");
    const first=page.locator("#library-row-A .poster").first();
    await first.focus();await page.keyboard.press("End");
    await page.waitForFunction(()=>document.activeElement?.textContent.includes("Atlas 419"));
    assert.equal(await page.locator("#library-row-A .poster").count(),420);
    const position=await page.locator("#library-row-A .rowscroll").evaluate(el=>el.scrollLeft);
    assert.ok(position>0);
    await page.getByRole("button",{name:"View all A items",exact:true}).click();
    assert.equal(await page.locator("#libbody .grid .poster").count(),20);
    assert.match(await page.locator("#libcount").textContent(),/1–20 of 420/);
    await page.getByRole("button",{name:"Next ›",exact:true}).click();
    assert.match(await page.locator("#libcount").textContent(),/21–40 of 420/);
    await page.getByRole("button",{name:"‹ All rows",exact:true}).click();
    assert.equal(await page.locator("#library-row-A .rowscroll").evaluate(el=>el.scrollLeft),position);
    await page.locator('#library-group-index [data-jump="Z"]').click();
    await page.waitForFunction(()=>document.activeElement?.id==="library-row-Z-title");
    assert.equal(await page.locator("#library-row-Z .poster").count(),1);
    assert.deepEqual(errors,[]);await page.close();
  }finally{await browser.close();}
});

test("rows preserve cards, focus, and horizontal scroll as later pages arrive",async()=>{
  const browser=await chromium.launch({headless:true});let release;
  const blocked=new Promise(resolve=>{release=resolve;});
  try{
    const {page,errors}=await setup(browser,{reply:async url=>{if(url.searchParams.get("offset")==="200")await blocked;}});
    await page.goto("http://library.test/#/library/1");
    await page.waitForSelector("#library-row-A .poster");
    await page.evaluate(()=>{
      const card=document.querySelector("#library-row-A .poster:nth-child(5)");
      card.setAttribute("data-same-node","yes");card.focus();
      document.querySelector("#library-row-A .rowscroll").scrollLeft=200;
    });
    await page.waitForFunction(()=>document.querySelector("#library-row-A .rowscroll").scrollLeft>0);
    const position=await page.locator("#library-row-A .rowscroll").evaluate(el=>el.scrollLeft);
    release();await loaded(page);
    assert.equal(await page.locator('[data-same-node="yes"]').count(),1);
    assert.equal(await page.evaluate(()=>document.activeElement?.getAttribute("data-same-node")),"yes");
    assert.equal(await page.locator("#library-row-A .rowscroll").evaluate(el=>el.scrollLeft),position);
    assert.deepEqual(errors,[]);await page.close();
  }finally{release();await browser.close();}
});

test("arriving groups preserve the focused jump-index button",async()=>{
  const browser=await chromium.launch({headless:true});let release;
  const blocked=new Promise(resolve=>{release=resolve;});
  const items=[...large.slice(0,200),movie(999,"Zodiac")];
  try{
    const {page,errors}=await setup(browser,{items,reply:async url=>{if(url.searchParams.get("offset")==="200")await blocked;}});
    await page.goto("http://library.test/#/library/1");
    const button=page.locator('#library-group-index [data-jump="A"]');await button.waitFor();
    await button.evaluate(el=>el.setAttribute("data-original-index","yes"));await button.focus();
    release();await loaded(page);
    assert.equal(await page.locator('#library-group-index [data-jump="Z"]').count(),1);
    assert.equal(await button.getAttribute("data-original-index"),"yes");
    assert.equal(await page.evaluate(()=>document.activeElement?.getAttribute("data-original-index")),"yes");
    assert.deepEqual(errors,[]);await page.close();
  }finally{release();await browser.close();}
});

test("View all opens at the group heading and returns to the originating row",async()=>{
  const browser=await chromium.launch({headless:true});
  const items=Array.from({length:1560},(_,i)=>movie(i+1,`${String.fromCharCode(65+Math.floor(i/60))} Film ${i}`));
  try{
    const {page,errors}=await setup(browser,{items});
    await page.goto("http://library.test/#/library/1");await loaded(page);
    await page.evaluate(()=>setPerPage("all"));
    await page.locator('#library-group-index [data-jump="T"]').click();
    const initial=await page.evaluate(()=>scrollY);assert.ok(initial>1500);
    await page.getByRole("button",{name:"View all T items",exact:true}).click();
    const expanded=await page.locator("#library-group-back").boundingBox();
    assert.ok(expanded.y>=0&&expanded.y<150,`group heading starts near the viewport top: ${JSON.stringify({expanded,metrics:await page.evaluate(()=>({y:scrollY,height:document.documentElement.scrollHeight,view:innerHeight}))})}`);
    await page.getByRole("button",{name:"‹ All rows",exact:true}).click();
    const returned=await page.locator("#library-row-T h2").boundingBox();
    assert.ok(returned.y>=0&&returned.y<200,"return restores the originating row");
    assert.equal(await page.evaluate(()=>document.activeElement?.id),"library-row-T-title");
    assert.deepEqual(errors,[]);await page.close();
  }finally{await browser.close();}
});

test("row filters, sorts, and remembered Grid choice work across desktop and mobile layouts",async()=>{
  const browser=await chromium.launch({headless:true});
  try{
    const items=Array.from({length:30},(_,i)=>movie(i+1,`${String.fromCharCode(65+i%3)} Film ${i+1}`,{year:i%2?2024:2023,watch:i%2?{watched:true}:null}));
    for(const layout of ["catalog","classic","theater"]){
      const {page,errors}=await setup(browser,{layout,items});
      for(const width of [1280,768,390]){
        await page.setViewportSize({width,height:1000});await page.goto("http://library.test/#/library/1");await loaded(page);
        if(await page.locator('#libbody.library-rows').count()===0)await page.getByRole("button",{name:"Rows",exact:true}).click();
        assert.equal(await page.locator(".library-group").count(),3);
        const geometry=await page.evaluate(()=>({viewport:innerWidth,right:document.getElementById("libbody").getBoundingClientRect().right,
          index:document.getElementById("library-group-index").getBoundingClientRect().width,
          row:document.querySelector(".rowscroll").getBoundingClientRect().width}));
        assert.ok(geometry.right<=geometry.viewport+1,`${layout} ${width}: the library body stays inside the viewport`);
        assert.ok(geometry.row>geometry.index-70,`${layout} ${width}: rows use available width`);
        await page.locator("#library-find").fill("Film 30");
        assert.equal(await page.locator("#libbody .poster").count(),1);
        await page.getByRole("button",{name:"Clear filters",exact:true}).click();
        assert.equal(await page.locator("#libbody .poster").count(),30);
        await snapshot(page,`rows-${layout}-${width}`);
        await page.locator(".libbar select").first().selectOption("year");await loaded(page);
        assert.equal(await page.locator(".library-group h2").first().textContent(),"2024");
        await page.locator("[data-library-watch-filter]").selectOption("unwatched");await loaded(page);
        assert.equal(await page.locator(".library-group").count(),1);
        await page.getByRole("button",{name:"Clear filters",exact:true}).click();
        await page.getByRole("button",{name:"Grid",exact:true}).click();
        assert.equal(await page.locator("#libbody .grid .poster").count(),20);
        await page.reload();await loaded(page);
        assert.equal(await page.getByRole("button",{name:"Grid",exact:true}).getAttribute("aria-pressed"),"true");
        await page.getByRole("button",{name:"Rows",exact:true}).click();
        await page.locator(".libbar select").first().selectOption("title");await loaded(page);
      }
      assert.deepEqual(errors,[],layout);await page.close();
    }
  }finally{await browser.close();}
});

test("incomplete library loads remain visible and Retry replaces the partial listing",async()=>{
  const browser=await chromium.launch({headless:true});let fail=true;
  try{
    const {page,errors}=await setup(browser,{reply:async url=>fail&&url.searchParams.get("offset")==="200"?{status:503,contentType:"application/json",body:'{"error":"fixture unavailable"}'}:null});
    await page.goto("http://library.test/#/library/1");await loaded(page);
    assert.match(await page.locator("#libcount").textContent(),/Incomplete results/);
    assert.match(await page.locator("#library-load-error").textContent(),/incomplete/);
    assert.equal(await page.locator("#library-row-A .library-group-count").textContent(),"200 loaded");
    fail=false;await page.getByRole("button",{name:"Retry",exact:true}).click();await loaded(page);
    assert.equal(await page.locator("#library-load-error").textContent(),"");
    assert.equal(await page.locator("#library-row-A .library-group-count").textContent(),"420");
    assert.deepEqual(errors,[]);await page.close();
  }finally{await browser.close();}
});

test("merged categories regroup late libraries without duplicates",async()=>{
  const browser=await chromium.launch({headless:true});
  try{
    const items=[movie(1,"Atlas",{added_at:1800000000}),movie(2,"Beta",{library_id:2,added_at:1700000000}),movie(3,"Arrival",{library_id:2,added_at:1900000000})];
    const {page,errors}=await setup(browser,{items,libraries:[{id:1,name:"Movies One",kind:"movies"},{id:2,name:"Movies Two",kind:"movies"}]});
    await page.goto("http://library.test/#/category/movie");await loaded(page);
    assert.deepEqual(await page.locator("#library-row-A .t").allTextContents(),["Arrival","Atlas"]);
    await page.locator("#library-scope").selectOption("2");
    assert.deepEqual(await page.locator("#libbody .t").allTextContents(),["Arrival","Beta"]);
    await page.getByRole("button",{name:"Clear filters",exact:true}).click();
    assert.equal(await page.locator("#libbody .poster").count(),3);
    assert.deepEqual(errors,[]);await page.close();
  }finally{await browser.close();}
});
