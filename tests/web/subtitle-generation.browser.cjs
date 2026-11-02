"use strict";
// Production DOM handlers with a deterministic local API fixture. This proves
// browser interactions and selection preservation, not engine/fleet acceptance.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const {test}=require("node:test");
const assert=require("node:assert/strict");
const http=require("node:http");
const fs=require("node:fs");
const path=require("node:path");
const WEB=path.resolve(__dirname,"../../crates/plurxd/src/web");
test("managed subtitle setup and generation work through browser controls without replacing playback",async()=>{
  const server=http.createServer((_req,res)=>{res.writeHead(200,{"content-type":"text/html"});res.end('<!doctype html><html><body><main style="max-width:850px;margin:24px auto;padding:16px"><h1>Subtitle generation fixture</h1><div id="surface"></div></main></body></html>');});
  await new Promise(resolve=>server.listen(0,"127.0.0.1",resolve));
  let browser;
  try{
    browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE}:{})});
    const page=await browser.newPage({viewport:{width:1100,height:1000}}),errors=[];
    page.on("pageerror",error=>errors.push(error.message));page.on("console",message=>{if(message.type()==="error")errors.push(message.text());});
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.addStyleTag({path:path.join(WEB,"app.css")});
    await page.addStyleTag({content:':root{--bg:#15130d;--panel:#1b180f;--panel2:#211d12;--line:#393220;--text:#eee9da;--prose:#eee9da;--muted:#b2a98e;--accent:#eaa63b;--good:#62ac78;--warn:#dc873d;--bad:#dc6464;--font:Arial,sans-serif} body{margin:0} .card{padding:16px} select,input{display:block;margin:8px 0 12px} input[type=checkbox]{display:inline}'});
    await page.evaluate(()=>{
      Object.assign(window,{AUTH_GENERATION:1,TOKEN:"fixture-admin",ME:{id:1,username:"fixture-admin",created_at:1,is_admin:true},PAGE_RENDER_GENERATION:1,PLAYER:null,WATCH_ITEM_PAGE:null,API:"/api/v1",esc:value=>String(value).replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;").replaceAll('"',"&quot;").replaceAll("'","&#39;"),samePlaybackFile:(player,c)=>!!player&&player.fileId===c.source_ref.file_id,subFactLabel:row=>row.title||row.language,audioFactLabel:row=>row.title||row.language});
      window.fixture={verified:false,installReads:0,generationReads:0,published:false,settings:{version:1,revision:"0",mode:"managed",enabled:false,interval_mins:0,language:"en",model_id:"base",advanced:{command:"whisper-cli",model_path:""},mode_status:"ok"},model:{id:"base",label:"Base multilingual",languages:["en","pt","sv"],download_bytes:147951465,sha256:"a".repeat(64),source:"https://example.invalid/reviewed-model",license:"MIT"},requests:[]};
      window.api=async(url,init={})=>{
        fixture.requests.push({url,body:init.body});
        if(url==="/subtitle-transcription/managed"){if(init.method==="PUT")fixture.settings={...init.body,revision:"1"};return fixture.settings;}
        if(url==="/subtitle-transcription/models")return {version:1,models:[fixture.model],workers:[{node_id:"worker-one",label:"Media worker",eligible:true,observed_at_ms:Date.now(),expires_at_ms:Date.now()+30000,engine:{status:"available"},models:[{model_id:"base",status:fixture.verified?"verified":"missing",verified_at_ms:fixture.verified?Date.now():null}]}]};
        if(url.endsWith("/install"))return {operations:[{node_id:"worker-one",job_id:"install-one",outcome:"accepted"}]};
        if(url==="/subtitle-transcription/jobs/install-one"){fixture.verified=++fixture.installReads>1;return {job_id:"install-one",node_id:"worker-one",state:fixture.verified?"succeeded":"running",phase:fixture.verified?"succeeded":"verifying"};}
        if(url==="/files/42/subtitles/transcribe"){
          if(init.method==="POST"){fixture.generationBody=init.body;return {outcome:"accepted",job_id:"generation-one",artifact_key:"artifact-one",file_id:"42",source_size:"1000",source_mtime:"2000"};}
          return {version:1,file_id:"42",source_size:"1000",source_mtime:"2000",audio_tracks:[{index:0,language:"eng",normalized_language:"en",title:"Original English",default:true}],mode:"managed",model_id:"base",enabled:fixture.settings.enabled,subtitle_streams:[{index:0,language:"en",title:"Existing English",codec:"webvtt"},...(fixture.published?[{index:1,language:"en",title:"Machine generated · Base",codec:"webvtt"}]:[])],captions:fixture.published?[{index:1,artifact_key:"artifact-one",source_size:"1000",source_mtime:"2000"}]:[]};
        }
        if(url==="/subtitle-transcription/jobs/generation-one"){const count=++fixture.generationReads;fixture.published=count>=3;return {job_id:"generation-one",kind:"subtitle_transcribe",state:fixture.published?"succeeded":"running",phase:fixture.published?"succeeded":count===1?"decoding":"transcribing",node_id:"worker-one",attempt:1,fence:1,started_at_ms:Date.now()-5000,updated_at_ms:Date.now(),file_id:"42",source_size:"1000",source_mtime:"2000",artifact_key:"artifact-one",result_ref:fixture.published?"artifact-one":null};}
        throw Error("Unimplemented fixture route: "+url);
      };
      location.hash="#/settings/maintenance";
    });
    for(const file of ["core/file-context.js","pages/settings-panels.js","pages/settings-developer.js","detail/preplay-selection.js","pages/activity.js"])await page.addScriptTag({path:path.join(WEB,file)});
    await page.evaluate(()=>{document.getElementById("surface").innerHTML=subtitleTranscriptionCard();window.prePlayPreview=()=>{};});
    await page.getByRole("button",{name:"Configure transcription",exact:true}).click();
    await page.getByRole("button",{name:"Download and test",exact:true}).waitFor();
    assert.equal(await page.getByLabel("Media worker",{exact:true}).isChecked(),true);
    await page.getByRole("button",{name:"Download and test",exact:true}).click();
    await page.locator("#st-language").selectOption("sv");
    await page.waitForFunction(()=>fixture.verified);
    assert.equal(await page.locator("#st-language").inputValue(),"sv","worker status does not erase a newer language draft");
    await page.locator("#st-language").selectOption("en");
    await page.getByLabel("Enable offline subtitle generation",{exact:true}).check();
    await page.getByRole("button",{name:"Save",exact:true}).click();
    await page.locator("#st-status").getByText(/Saved\./).waitFor();
    if(process.env.SUBTITLE_GENERATION_SCREENSHOT_DIR){fs.mkdirSync(process.env.SUBTITLE_GENERATION_SCREENSHOT_DIR,{recursive:true});await page.screenshot({path:path.join(process.env.SUBTITLE_GENERATION_SCREENSHOT_DIR,"settings-verified.png"),fullPage:true});}
    await page.evaluate(()=>{
      PAGE_RENDER_GENERATION++;location.hash="#/item/7";
      const file={id:"42",fileContext:localPlaybackFileContext("42"),available:true,video_codec:"h264",audio_streams:[{index:0,language:"en",title:"Original English"}],subtitle_streams:[{index:0,language:"en",title:"Existing English",codec:"webvtt"}],playback_defaults:{audio:{selected_index:0},subtitle:{selected_index:0}}};
      WATCH_ITEM_PAGE={id:"7",files:[file]};
      document.getElementById("surface").innerHTML='<h2>Example movie</h2><video id="video" aria-label="Existing playback"></video>'+prePlayPickers(file);
      setPrePlay("42","subtitle","-1");document.getElementById("pp-s-42").value="-1";
      window.fixtureVideo=document.getElementById("video");PLAYER={fileId:"42",started:true,preplay:{audio:0,subtitle:-1},curAudio:0,curSub:-1,audio:[{index:0}],subs:file.subtitle_streams};window.setSub=index=>{PLAYER.curSub=index;fixtureVideo.dataset.selectedSubtitle=String(index);};
    });
    await page.getByRole("button",{name:"Generate subtitles",exact:true}).click();
    await page.getByRole("button",{name:"Generate from this audio",exact:true}).click();
    await page.getByRole("link",{name:"Follow this job in Activity",exact:true}).waitFor();
    await page.getByRole("button",{name:"Use generated subtitles",exact:true}).waitFor({timeout:15000});
    assert.equal(await page.locator("#pp-s-42").inputValue(),"-1");
    assert.equal(await page.evaluate(()=>PLAYER.curSub),-1);
    assert.equal(await page.evaluate(()=>fixtureVideo===document.getElementById("video")),true);
    assert.equal(await page.getByRole("link",{name:"Follow this job in Activity",exact:true}).getAttribute("href"),"#/activity/jobs/generation-one");
    assert.equal(await page.evaluate(()=>fixture.generationBody.audio_index),0);
    if(process.env.SUBTITLE_GENERATION_SCREENSHOT_DIR)await page.screenshot({path:path.join(process.env.SUBTITLE_GENERATION_SCREENSHOT_DIR,"captions-ready-off-preserved.png"),fullPage:true});
    await page.getByRole("button",{name:"Use generated subtitles",exact:true}).click();
    assert.equal(await page.evaluate(()=>PLAYER.curSub),1);
    assert.equal(await page.evaluate(()=>fixtureVideo===document.getElementById("video")),true);
    assert.deepEqual(errors,[],"page and console errors");
    if(process.env.SUBTITLE_GENERATION_SCREENSHOT_DIR)fs.writeFileSync(path.join(process.env.SUBTITLE_GENERATION_SCREENSHOT_DIR,"browser-errors.json"),JSON.stringify(errors));
  }finally{await browser?.close();await new Promise(resolve=>server.close(resolve));}
});
