"use strict";
// Run the shipped page with controlled artwork responses, including failed and
// delayed images. No tuner, external artwork service or media decoding needed.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const {test}=require("node:test");
const assert=require("node:assert/strict");
const fs=require("node:fs"),path=require("node:path");
const root=path.resolve(__dirname,"../../crates/plurxd/src/web");

test("station logos render in list, grid and details with safe fallbacks and aligned guide geometry",async()=>{
  const browser=await chromium.launch({headless:true});
  try{
    const page=await browser.newPage({viewport:{width:1440,height:1100}});
    const errors=[];page.on("pageerror",error=>errors.push(error.message));
    await page.addInitScript(()=>{
      localStorage.setItem("plurx_token","fixture");
      localStorage.setItem("plurx_layout","classic");
      localStorage.setItem("plurx_theme","noirr");
      localStorage.setItem("plurx_appearance","dark");
      localStorage.setItem("plurx_live_tv_view","grid");
    });
    const channels=["WTVR-HD","Broken Logo","No Artwork","Unsafe Logo"].map((name,i)=>({
      id:String(i),guide_number:`6.${i+1}`,guide_name:name,support:"ready",drm:false,
      video_codec:"MPEG2",audio_codec:"AC3",hd:true}));
    const now=Math.floor(Date.now()/1000);
    const logos=["https://artwork.test/cbs6.svg","https://artwork.test/broken.png",null,"javascript:alert(1)"];
    let releaseLogo;
    const logoReady=new Promise(resolve=>{releaseLogo=resolve;});
    const artworkRequests=[];
    await page.route("**/*",async route=>{
      const url=new URL(route.request().url()),p=url.pathname;
      if(url.hostname==="artwork.test"){
        artworkRequests.push(route.request());
        if(p==="/broken.png")return route.fulfill({status:404,body:"missing"});
        await logoReady;
        return route.fulfill({contentType:"image/svg+xml",body:'<svg xmlns="http://www.w3.org/2000/svg" width="180" height="60"><rect width="180" height="60" fill="#426acb"/><text x="20" y="43" fill="white" font-size="38">CBS 6</text></svg>'});
      }
      if(url.hostname!=="live-tv.test")return route.abort();
      if(p==="/"||p.startsWith("/assets/")){
        const file=path.join(root,p==="/"?"index.html":p.slice(8));
        return route.fulfill({status:fs.existsSync(file)?200:404,
          contentType:p==="/"?"text/html":p.endsWith(".css")?"text/css":"text/javascript",
          body:fs.existsSync(file)?fs.readFileSync(file):""});
      }
      const key=p.replace("/api/v1","");let data={};
      if(key==="/server")data={name:"Cinema",setup_required:false};
      else if(key==="/me")data={id:"viewer",username:"Viewer",is_admin:false};
      else if(["/libraries","/dvr/reminders","/dvr/reminders/due","/dvr/rules"].includes(key))data=[];
      else if(key==="/live-tv/channels")data={channels,protocols:[1,2],freshness:"fresh"};
      else if(key==="/live-tv/guide")data={source:"hdhomerun",freshness:"fresh",channels:channels.map((channel,i)=>({
        id:channel.id,image_url:logos[i],programmes:[{title:"Evening programme",start:now-600,end:now+5400}]}))};
      else if(key==="/dvr/overview")data={version:1,availability:"complete",active:[],counts:{},active_total:0};
      else if(key==="/dvr/schedule"||key==="/dvr/recordings")data={rows:[]};
      else if(key==="/dvr/status")data={enabled:false};
      return route.fulfill({contentType:"application/json",body:JSON.stringify(data)});
    });
    await page.goto("http://live-tv.test/#/live-tv",{waitUntil:"domcontentloaded"});
    await page.waitForFunction(()=>typeof LIVE_TV!=="undefined"&&LIVE_TV.guide&&document.querySelectorAll(".lt-grow").length===4);
    const first=page.locator(".lt-gname").first();
    await first.scrollIntoViewIfNeeded();
    assert.equal(await first.locator(".lt-chip-fallback").isVisible(),true,"callsign stays visible while artwork loads");
    releaseLogo();
    await page.waitForFunction(()=>document.querySelector(".lt-gname .lt-chip.has-logo img")?.naturalWidth>0);
    assert.equal(await first.locator(".lt-chip-fallback").isVisible(),false);
    await page.waitForFunction(()=>!document.querySelectorAll(".lt-gname")[1].querySelector("img"));
    for(const i of [1,2,3])assert.equal(await page.locator(".lt-gname").nth(i).locator(".lt-chip-fallback").isVisible(),true);
    assert.equal(await page.locator(".lt-gname").nth(3).locator("img").count(),0);
    await page.evaluate(()=>{LIVE_TV.selected="0";renderLiveTvChannels();});
    await page.locator(".lt-showinfo .lt-chip").scrollIntoViewIfNeeded();
    await page.waitForFunction(()=>document.querySelector(".lt-showinfo .lt-chip.has-logo img")?.naturalWidth>0);
    for(const width of [1440,390]){
      await page.setViewportSize({width,height:1100});
      await page.locator(".lt-gridwrap").scrollIntoViewIfNeeded();
      await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
      const geometry=await page.evaluate(()=>{
        const bounds=s=>document.querySelector(s).getBoundingClientRect();
        const station=bounds(".lt-gname"),header=bounds(".lt-times .pad"),cells=bounds(".lt-gcells"),
          image=bounds(".lt-gname img"),tile=bounds(".lt-gname .lt-chip"),marker=bounds(".lt-now");
        return {stationWidth:station.width,headerWidth:header.width,cellsX:cells.x,stationRight:station.right,
          imageFits:image.width<=tile.width&&image.height<=tile.height,
          markerX:marker.x,expectedMarker:cells.x+liveTvGridLayout().nowX,
          pageWidth:document.documentElement.scrollWidth,viewport:innerWidth};
      });
      assert.equal(geometry.stationWidth,geometry.headerWidth,`header alignment at ${width}`);
      assert.ok(Math.abs(geometry.cellsX-geometry.stationRight)<1);
      assert.ok(Math.abs(geometry.markerX-geometry.expectedMarker)<1,"now line aligns with programme time");
      assert.equal(geometry.imageFits,true,"logos fit without stretching their tile");
      assert.ok(geometry.pageWidth<=geometry.viewport,`no page overflow at ${width}`);
      if(process.env.LIVE_TV_SCREENSHOTS){
        fs.mkdirSync(process.env.LIVE_TV_SCREENSHOTS,{recursive:true});
        await page.screenshot({path:path.join(process.env.LIVE_TV_SCREENSHOTS,`logos-${width}.png`)});
      }
    }
    await page.evaluate(()=>liveTvSetView("list"));
    await page.locator(".lt-row").first().scrollIntoViewIfNeeded();
    await page.waitForFunction(()=>document.querySelector(".lt-row .lt-chip.has-logo img")?.naturalWidth>0);
    assert.match(await page.locator(".lt-row").first().innerText(),/6.1 · WTVR-HD/);
    await page.evaluate(()=>{LIVE_TV.guide=null;renderLiveTvChannels();});
    assert.equal(await page.locator(".lt-row img").count(),0,"unavailable guide leaves usable channel rows");
    assert.equal(await page.locator(".lt-row").count(),4);
    assert.ok(artworkRequests.length>0);
    for(const request of artworkRequests)assert.equal(request.headers().referer,undefined,"artwork omits referrer");
    assert.deepEqual(errors,[]);
  }finally{await browser.close();}
});
