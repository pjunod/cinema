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
      await page.addScriptTag({content:fs.readFileSync(path.join(root,"pages/reader.js"),"utf8")});
      await page.evaluate(()=>{
        document.getElementById("app").innerHTML=readerShell({title:"A very long book title that should leave room for every reader control"},{});
        READER={open:{publication:{toc:[{href:"chapter.xhtml",title:"Chapter one"}]}},href:"chapter.xhtml"};
        window.ReaderCore={splitHref:href=>({path:href})};
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
