"use strict";
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const {test}=require("node:test");
const assert=require("node:assert/strict");
const fs=require("node:fs"),path=require("node:path");
const root=path.resolve(__dirname,"../../crates/plurxd/src/web");

// Render the shipped reader markup with both shipped stylesheets: app.css
// supplies the control metrics and can affect the reader's available space.
test("reader uses the available viewport with navigation open and closed",async(t)=>{
  const browser=await chromium.launch({headless:true,executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE});
  try{
    for(const width of [1440,1024,768,720,390,320]) await t.test(`${width}px`,async()=>{
      const page=await browser.newPage({viewport:{width,height:800}});
      await page.setContent('<div id="app"></div>');
      for(const file of ["reader.css","app.css"]) await page.addStyleTag({content:fs.readFileSync(path.join(root,file),"utf8")});
      await page.evaluate(()=>{
        window.esc=value=>String(value);
        window.bookContentUrl=()=>"/book.epub";
      });
      await page.addScriptTag({content:fs.readFileSync(path.join(root,"reader.js"),"utf8")});
      await page.evaluate(()=>{window.ReaderCore=window.PlurxReaderCore;});
      await page.addScriptTag({content:fs.readFileSync(path.join(root,"pages/reader.js"),"utf8")});
      await page.evaluate(()=>{
        document.getElementById("app").innerHTML=readerShell({title:"A very long book title that should leave room for every reader control"},{});
        READER={open:{publication:{toc:[{href:"chapter.xhtml",title:"Chapter one"}]}},href:"chapter.xhtml"};
        document.getElementById("reader-loading").hidden=true;
        document.getElementById("reader-location").textContent="Chapter 123 — A long chapter title with a detailed reading location";
        document.getElementById("reader-frame").srcdoc="<p>A readable chapter</p>";
      });
      for(const open of [false,true,false]){
        if(open!==await page.locator("#reader-panel").isVisible()) await page.evaluate(()=>toggleReaderPanel("toc"));
        const boxes=await page.evaluate(()=>{
          const box=s=>{const r=document.querySelector(s).getBoundingClientRect();return {x:r.x,y:r.y,right:r.right,bottom:r.bottom,width:r.width,height:r.height};};
          return {frame:box(".reader-frame"),top:box(".reader-top"),bottom:box(".reader-bottom"),panel:box(".reader-panel"),
            controls:[...document.querySelectorAll(".reader-top button,.reader-top a,.reader-bottom button")].map(e=>{const r=e.getBoundingClientRect();return {text:e.textContent,x:r.x,right:r.right,y:r.y,bottom:r.bottom};})};
        });
        const label=`${width}px, navigation ${open?"open":"closed"}`;
        const sidebar=open&&width>720?boxes.panel.width:0;
        assert.ok(Math.abs(boxes.frame.width-(width-sidebar))<2,`${label}: chapter fills the space beside the sidebar (${boxes.frame.width}px)`);
        assert.ok(Math.abs(boxes.frame.x-sidebar)<2,`${label}: chapter starts beside the sidebar`);
        assert.ok(boxes.frame.height>500,`${label}: chapter retains usable height`);
        assert.ok(boxes.frame.y>=boxes.top.bottom,`${label}: chapter starts below toolbar`);
        assert.ok(boxes.frame.bottom<=boxes.bottom.y+1,`${label}: chapter ends above page controls`);
        assert.ok(boxes.bottom.bottom<=800,`${label}: page controls remain onscreen`);
        for(const c of boxes.controls) assert.ok(c.x>=0&&c.right<=width+1&&c.y>=0&&c.bottom<=800,`${label}: ${c.text} stays onscreen`);
        if(open&&width<=720){
          assert.ok(boxes.panel.y>=boxes.top.bottom,`${label}: navigation leaves toolbar accessible`);
          assert.ok(boxes.panel.bottom<=boxes.bottom.y+1,`${label}: navigation leaves page controls accessible`);
        }
      }
      await page.close();
    });
  }finally{await browser.close();}
});


test("mobile reader centers full pages and distinguishes section navigation",async()=>{
  const browser=await chromium.launch({headless:true,executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE});
  try{
    const page=await browser.newPage({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
    const chapter='<html><head></head><body>'+Array.from({length:50},(_,i)=>`<p id="p${i}">Paragraph ${i}. ${"A readable sentence fills the page with ordinary text. ".repeat(8)}</p>`).join("")+'</body></html>';
    await page.route("http://reader.test/**",route=>route.fulfill({contentType:"text/html",body:route.request().url().includes("/publication/")?chapter:'<meta name="viewport" content="width=device-width,initial-scale=1"><div id="app"></div>'}));
    await page.goto("http://reader.test/");
    for(const file of ["reader.css","app.css","reader.js"]){
      const content=fs.readFileSync(path.join(root,file),"utf8");
      if(file.endsWith(".css")) await page.addStyleTag({content}); else await page.addScriptTag({content});
    }
    await page.evaluate(()=>{
      window.ReaderCore=window.PlurxReaderCore;
      window.esc=value=>String(value);
      window.bookContentUrl=()=>"/book.epub";
    });
    await page.addScriptTag({content:fs.readFileSync(path.join(root,"pages/reader.js"),"utf8")});
    await page.evaluate(async()=>{
      document.getElementById("app").innerHTML=readerShell({title:"A mobile book"},{});
      READER={index:0,href:"one.xhtml",prefs:ReaderCore.normalizePrefs(),panel:null,open:{resource_base:"http://reader.test/publication/",publication:{toc:[],readingOrder:[{href:"one.xhtml",title:"Chapter one"},{href:"two.xhtml",title:"Chapter two"}]}}};
      READER.navigator=new ReaderCore.FrameNavigator(document.getElementById("reader-frame"),{onLocation:locator=>{
        READER.locator=locator; READER.progression=locator.locations.totalProgression; updateReaderLocation();
      }});
      await navigateReader("one.xhtml");
    });
    const position=()=>page.evaluate(()=>({left:READER.navigator.scrollRoot().scrollLeft,width:READER.navigator.scrollRoot().clientWidth,...READER.navigator.pageInfo()}));
    const first=await position();
    assert.equal(first.left,0,"first page starts centered, without snapping to a paragraph");
    assert.ok(first.pages>3,"fixture spans several pages in a section");
    await page.locator("#reader-next").click();
    assert.equal((await position()).left,first.width,"Next advances one full page");
    await page.locator("#reader-next").click();
    assert.equal((await position()).left,2*first.width,"repeated Next advances another full page");
    await page.locator("#reader-previous").click();
    assert.equal((await position()).left,first.width,"Previous reverses one page");
    assert.match(await page.locator("#reader-location").textContent(),/Section 1 of 2 · Page 2 of/);
    await page.evaluate(()=>{
      READER.navigator.document().dispatchEvent(new Event("touchstart"));
      READER.navigator.scrollRoot().scrollLeft=READER.navigator.scrollRoot().clientWidth*1.35;
    });
    await page.waitForTimeout(300);
    assert.ok((await position()).left>first.width,"a held touch does not snap underneath the reader's finger");
    await page.evaluate(()=>READER.navigator.document().dispatchEvent(new Event("touchend")));
    await page.waitForTimeout(300);
    assert.equal((await position()).left,first.width,"touch scrolling settles on a centered page after release");
    await page.getByRole("button",{name:"Table of contents",exact:true}).click();
    assert.equal(await page.locator(".reader-toc button").count(),2,"missing authored contents falls back to publication sections");
    await page.getByRole("button",{name:"Chapter two",exact:true}).click();
    await page.waitForFunction(()=>READER.index===1&&READER.navigator.ready&&document.getElementById("reader-panel").hidden);
    assert.equal((await position()).page,1);
    assert.equal(await page.locator("#reader-previous").textContent(),"← Previous section");
    await page.locator("#reader-previous").click();
    await page.waitForFunction(()=>READER.index===0&&READER.navigator.ready);
    const previous=await position();
    assert.equal(previous.page,previous.pages,"crossing backwards opens the previous section's last page");
    assert.equal(await page.locator("#reader-next").textContent(),"Next section →");
    await page.evaluate(()=>{
      const doc=READER.navigator.document();
      doc.body.innerHTML='<pre>'+"X".repeat(220)+'</pre>';
      doc.body.scrollLeft=0;
    });
    const overflow=await page.evaluate(()=>({extent:READER.navigator.scrollRoot().scrollWidth-READER.navigator.scrollRoot().clientWidth,width:READER.navigator.scrollRoot().clientWidth}));
    assert.ok(overflow.extent%overflow.width>2,"publisher overflow creates a partial final page");
    await page.evaluate(()=>{while(READER.navigator.move(1)){};});
    await page.waitForTimeout(300);
    const last=await position();
    assert.equal(last.left,overflow.extent,"the final partial page remains reachable after scroll settling");
    assert.equal(last.page,last.pages,"the final partial page is labelled as the last page");
    await page.evaluate(()=>READER.navigator.move(-1));
    assert.equal((await position()).page,last.pages-1,"Previous from a partial final page does not skip a page");
    await page.evaluate(()=>READER.navigator.destroy());
  }finally{await browser.close();}
});
