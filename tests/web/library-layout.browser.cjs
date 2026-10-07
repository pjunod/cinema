"use strict";
// Render the shipped shell against fixture data, including the shared browse tools.
// PLAYWRIGHT_MODULE may point to an existing Playwright installation.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const {test}=require("node:test");
const assert=require("node:assert/strict");
const fs=require("node:fs"),path=require("node:path");
const root=path.resolve(__dirname,"../../crates/plurxd/src/web");
const items=Array.from({length:30},(_,i)=>({id:i+1,library_id:1,kind:"movie",
  title:`${String.fromCharCode(65+i%26)} Film ${i+1}`,year:2024,resolution:1080}));

test("library browse controls stay above the full-width poster grid",async()=>{
  const browser=await chromium.launch({headless:true});
  try{
    for(const layout of ["catalog","classic","theater"]){
      const page=await browser.newPage();
      const errors=[];
      page.on("pageerror",e=>errors.push(e.message));
      await page.addInitScript(layout=>{
        localStorage.setItem("plurx_token","fixture");
        localStorage.setItem("plurx_layout",layout);
        localStorage.setItem("plurx_perpage","all");
        localStorage.setItem("plurx_library_view","grid");
      },layout);
      await page.route("**/*",async route=>{
        const url=new URL(route.request().url()),p=url.pathname;
        if(url.hostname!=="library.test")return route.abort();
        if(p==="/"||p.startsWith("/assets/")){
          const file=path.join(root,p==="/"?"index.html":p.slice(8));
          if(!fs.existsSync(file))return route.fulfill({status:404,body:""});
          return route.fulfill({contentType:p==="/"?"text/html":p.endsWith(".css")?"text/css":"text/javascript",body:fs.readFileSync(file)});
        }
        const key=p.replace("/api/v1","");
        let data={};
        if(key==="/server")data={name:"Cinema",setup_required:false};
        else if(key==="/me")data={id:1,username:"viewer",is_admin:false};
        else if(key==="/libraries")data=[{id:1,name:"Movies",kind:"movies",item_count:items.length}];
        else if(key==="/libraries/1/items")data={items,total:items.length};
        else if(key==="/dvr/reminders/due")data=[];
        return route.fulfill({contentType:"application/json",body:JSON.stringify(data)});
      });
      for(const width of [1936,1024,768,390]){
        await page.setViewportSize({width,height:1000});
        await page.goto("http://library.test/#/library/1");
        await page.waitForFunction(()=>document.querySelectorAll("#libbody .poster").length===30);
        const bounds=await page.evaluate(()=>{
          const box=s=>{const r=document.querySelector(s).getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,bottom:r.bottom};};
          return {tools:box("#main .search-tools"),body:box("#libbody"),grid:box("#libbody .grid"),
            viewport:innerWidth};
        });
        const label=`${layout} at ${width}px`;
        assert.ok(bounds.tools.bottom<=bounds.body.y,`${label}: filters must be above posters`);
        assert.ok(Math.abs(bounds.tools.x-bounds.body.x)<=1,`${label}: posters must start at the controls' left edge`);
        assert.ok(bounds.grid.width>=bounds.tools.width-60,`${label}: controls must not consume a grid column`);
        assert.ok(bounds.grid.x+bounds.grid.width<=bounds.viewport,`${label}: posters fit inside the viewport`);
        if(process.env.LIBRARY_SCREENSHOTS){
          fs.mkdirSync(process.env.LIBRARY_SCREENSHOTS,{recursive:true});
          await page.screenshot({path:path.join(process.env.LIBRARY_SCREENSHOTS,`${layout}-${width}.png`)});
        }
        await page.locator("#library-find").fill("Film 30");
        assert.equal(await page.locator("#libbody .poster").count(),1);
        await page.getByRole("button",{name:"Clear filters",exact:true}).click();
        assert.equal(await page.locator("#libbody .poster").count(),30);
      }
      assert.deepEqual(errors,[],`${layout}: no runtime errors`);
      await page.close();
    }
  }finally{await browser.close();}
});
