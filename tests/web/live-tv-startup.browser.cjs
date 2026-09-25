"use strict";
// Browser UI regression with the shipped shell and a controlled tuner/media
// boundary. This checks interactions and layout, not physical HLS decoding.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path");
const root=path.resolve(__dirname,"../../crates/plurxd/src/web");
(async()=>{
  const browser=await chromium.launch({headless:true,...(process.env.BROWSER_EXECUTABLE?{executablePath:process.env.BROWSER_EXECUTABLE}:{})});
  try{
    const page=await browser.newPage({viewport:{width:1440,height:1100}}),errors=[];
    page.on("pageerror",error=>errors.push(error.message));
    await page.addInitScript(()=>{
      localStorage.setItem("plurx_token","fixture");localStorage.setItem("plurx_layout","classic");
      localStorage.setItem("plurx_theme","noirr");localStorage.setItem("plurx_appearance","dark");
      localStorage.setItem("plurx_live_tv_view","grid");
      window.testAutoplayBlocked=true;window.testPlayCalls=[];
      HTMLMediaElement.prototype.play=function(){
        testPlayCalls.push(navigator.userActivation.isActive);
        return testAutoplayBlocked?Promise.reject(new DOMException("Blocked","NotAllowedError")):Promise.resolve();
      };
    });
    let finishStart,startCount=0;
    const grant=new Promise(resolve=>{finishStart=resolve;});
    const now=Math.floor(Date.now()/1000);
    const channels=[{id:"one",guide_number:"6.1",guide_name:"WTVR-HD",support:"ready",drm:false}];
    await page.route("http://live-tv.test/**",async route=>{
      const pathname=new URL(route.request().url()).pathname;
      if(pathname==="/")return route.fulfill({contentType:"text/html",body:fs.readFileSync(path.join(root,"index.html"))});
      if(pathname==="/assets/hls.min.js")return route.fulfill({contentType:"text/javascript",body:`window.Hls=class {
        static isSupported(){return true;} static Events={MANIFEST_PARSED:'manifest',ERROR:'error'};static ErrorTypes={MEDIA_ERROR:'media'};
        constructor(){this.events={};}on(name,fn){this.events[name]=fn;}loadSource(){}
        attachMedia(){queueMicrotask(()=>this.events.manifest());}destroy(){}
      };`});
      if(pathname.startsWith("/assets/")){
        const file=path.join(root,pathname.slice(8));
        return route.fulfill({status:fs.existsSync(file)?200:404,contentType:file.endsWith(".css")?"text/css":"text/javascript",body:fs.existsSync(file)?fs.readFileSync(file):""});
      }
      const key=pathname.replace("/api/v1","");let data={};
      if(key==="/server")data={name:"Cinema",version:"fixture",setup_required:false};
      else if(key==="/me")data={id:"viewer",username:"Viewer",is_admin:true};
      else if(key==="/libraries"||key==="/dvr/reminders"||key==="/dvr/rules")data=[];
      else if(key==="/live-tv/channels")data={channels,protocols:[1,2],freshness:"fresh"};
      else if(key==="/live-tv/guide")data={source:"xmltv",freshness:"fresh",channels:[{id:"one",programmes:[{title:"CBS6 News at 4:00p",start:now-600,end:now+2400,synopsis:"Local news and weather."}]}]};
      else if(key==="/live-tv/channels/one/sessions"){
        startCount++;await grant;data={session_id:"cap-one",channel:channels[0]};
      }else if(key==="/dvr/overview")data={version:1,availability:"complete",active:[],counts:{},active_total:0};
      else if(key==="/dvr/schedule"||key==="/dvr/recordings")data={rows:[]};
      else if(key==="/dvr/status")data={enabled:false};
      else if(key==="/activity")data={streams:0,scanning:0};
      return route.fulfill({contentType:"application/json",body:JSON.stringify(data)});
    });
    await page.goto("http://live-tv.test/#/live-tv");
    await page.waitForFunction(()=>typeof LIVE_TV!=="undefined"&&LIVE_TV.guide&&LIVE_TV.channels.length);
    await page.evaluate(()=>liveTvSelect("one"));
    await page.waitForFunction(()=>LIVE_TV.playbackState==="starting");
    assert.equal(await page.locator("#live-tv-status").isVisible(),true);
    assert.match(await page.locator("#live-tv-status-text").innerText(),/Tuning 6.1/);
    await page.evaluate(()=>liveTvSelect("one"));
    assert.equal(startCount,1);
    if(process.env.LIVE_TV_SCREENSHOTS)await page.screenshot({path:path.join(process.env.LIVE_TV_SCREENSHOTS,"tuning.png")});
    finishStart();
    await page.waitForFunction(()=>LIVE_TV.playbackState==="blocked");
    assert.equal(await page.locator("#live-tv-status-play").isVisible(),true);
    await page.evaluate(()=>{testAutoplayBlocked=false;});
    await page.locator("#live-tv-status-play").click();
    assert.equal(await page.evaluate(()=>testPlayCalls.at(-1)),true,"Play retains the click's user activation");
    assert.equal(startCount,1);
    await page.evaluate(()=>document.getElementById("live-tv-video").dispatchEvent(new Event("playing")));
    assert.equal(await page.locator("#live-tv-status").isVisible(),false);
    // Real TextTrack API; the caption selector belongs below the inline picture.
    await page.evaluate(()=>{document.getElementById("live-tv-video").addTextTrack("captions","English CC1","en");liveTvRefreshCaptionControls();});
    assert.equal(await page.locator(".lth-captions").isVisible(),false);
    assert.equal(await page.locator(".lt-captions").isVisible(),true);
    await page.locator(".lt-captions select").selectOption("0");
    assert.equal(await page.evaluate(()=>document.getElementById("live-tv-video").textTracks[0].mode),"showing");
    await page.locator(".lt-captions select").focus();
    await page.evaluate(()=>{
      window.originalCaptionSelect=document.querySelector(".lt-captions select");
      renderLiveTvChannels();renderLiveTvChannels();
    });
    assert.equal(await page.evaluate(()=>document.activeElement===originalCaptionSelect&&originalCaptionSelect.isConnected),true,
      "guide and minute refreshes keep the open/focused inline selector");
    await page.locator("#live-tv-search").focus();
    await page.waitForFunction(()=>!LIVE_TV.captionRenderPending&&!originalCaptionSelect.isConnected);
    assert.equal(await page.locator(".lt-captions select").inputValue(),"0","deferred rendering keeps the selected caption");
    for(const width of [1440,390]){
      await page.setViewportSize({width,height:1100});
      await page.evaluate(()=>liveTvTrackSlot());
      const boxes=await page.evaluate(()=>({video:document.getElementById("live-tv-video").getBoundingClientRect().toJSON(),captions:document.querySelector(".lt-captions").getBoundingClientRect().toJSON()}));
      assert.ok(boxes.captions.top>=boxes.video.bottom-1,`captions cover the picture at ${width}`);
      if(process.env.LIVE_TV_SCREENSHOTS)await page.screenshot({path:path.join(process.env.LIVE_TV_SCREENSHOTS,`controls-${width}.png`)});
    }
    await page.setViewportSize({width:1440,height:1100});
    await page.evaluate(()=>{liveTvSetMode("full");liveTvIdle();});
    assert.equal(await page.locator(".lth-captions").evaluate(el=>getComputedStyle(el).opacity),"0");
    await page.locator("#live-tv-captions").focus();
    assert.equal(await page.locator(".lth-captions").evaluate(el=>getComputedStyle(el).opacity),"1","keyboard focus reveals captions");
    await page.evaluate(()=>Object.defineProperty(document.getElementById("live-tv-video"),"paused",{configurable:true,get:()=>false}));
    await page.locator(".lth-acts [data-live-tv-mute]").click();
    await page.waitForFunction(()=>document.getElementById("live-tv-host").classList.contains("idle"));
    assert.equal(await page.locator(".lth-captions").evaluate(el=>getComputedStyle(el).opacity),"0",
      "pointer focus must not leave captions permanently over the picture");
    await page.locator("#live-tv-captions").focus();
    await page.keyboard.press("ArrowDown");
    const idleDelay=await page.evaluate(()=>PlaybackPolicy.liveContractTiming("hide_after_ms"));
    await page.waitForTimeout(idleDelay+100);
    assert.equal(await page.locator("#live-tv-host").evaluate(el=>el.classList.contains("idle")),false);
    await page.locator("#live-tv-search").focus();
    await page.waitForFunction(()=>document.getElementById("live-tv-host").classList.contains("idle"));
    assert.deepEqual(errors,[]);
    console.log("PASS Live TV tuning, autoplay action, caption selection and inline/fullscreen/mobile geometry");
  }finally{await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
