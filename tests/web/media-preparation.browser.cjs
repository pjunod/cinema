"use strict";
// Actual shipped renderer and CSS, with server facts supplied by the fixture.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const {test}=require("node:test");
const assert=require("node:assert/strict");
const path=require("node:path");
const root=path.resolve(__dirname,"../../crates/plurxd/src/web");
test("media info folds status groups and contains long track labels",async()=>{
  const browser=await chromium.launch({headless:true});
  try{
    const page=await browser.newPage({viewport:{width:960,height:900}});
    await page.setContent('<div id="preview" style="max-width:880px;margin:24px auto;padding:20px"><div class="media-preparation-layout"><section class="media-preparation-main"><h2>Media preparation</h2><p class="prep-intro">Preparation status for this file</p><div id="prep-file-1"></div></section><aside class="media-preparation-side"><h2>File &amp; tracks</h2><dl class="specs" id="tracks"></dl></aside></div></div>');
    await page.addStyleTag({path:path.join(root,"app.css")});
    await page.addStyleTag({content:':root{--bg:#15130d;--panel:#1b180f;--panel2:#211d12;--line:#393220;--text:#eee9da;--prose:#eee9da;--muted:#b2a98e;--accent:#eaa63b;--good:#62ac78;--warn:#dc873d;--bad:#dc6464;--font:Arial,sans-serif} body{margin:0}'});
    await page.evaluate(()=>{
      window.esc=s=>String(s??"").replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll('"','&quot;');
      window.langName=s=>s==="eng"?"English":s;
      window.fmtChannels=()=>"7.1";
      window.exactWireId=f=>String(f.id);
      window.PAGE_RENDER_GENERATION=1;
      window.fixtureFile={id:1,subtitle_streams:[{index:0,language:"eng",codec:"srt",title:"English"}]};
      window.fixtureData={checked_at_ms:1,active:true,playback:{state:"queued",detail:"Waiting for a worker or retry window"},subtitles:{state:"unknown",ready:0,total:31,preferred_index:0},markers:{state:"done",detail:"1 verified skip marker available"},probe:{state:"done",detail:"Video, audio, subtitle tracks and chapters read"},metadata:{state:"done",detail:"Metadata matched · poster available · backdrop available"},versions:{state:"on_demand",detail:"Copies are verified when used"},thumbnails:{state:"on_demand",detail:"58 chapters · previews are generated when opened"},conversion:{state:"off",detail:"Optional on-disk Profile 7 → 8.1 conversion"},search:{state:"ready",detail:"Search representation matches the current metadata and model"},downloads:{state:"off",detail:"0 downloaded tracks"}};
      window.api=async()=>window.fixtureData;
    });
    await page.addScriptTag({path:path.join(root,"detail/track-facts.js")});
    await page.evaluate(async()=>{
      document.getElementById("tracks").innerHTML='<dt>Video</dt><dd>HEVC · 3840×2160 · Dolby Vision · Profile 7 (HDR10-compatible) · 10-bit · 53 Mb/s</dd>'+
        trackFactRow("Audio",[{index:0,title:"English · TRUEHD · 7.1 · TrueHD Atmos 7.1"},{index:1,title:"English · AC3 · 5.1"}],0,(t,on)=>trackChip(t.title,on),"",true)+
        trackFactRow("Subtitles",Array.from({length:31},(_,index)=>({index,title:"English · SRT · English"})),-1,(t,on)=>trackChip(t.title,on),"English subtitles available — the server’s default is another track",true);
      await hydrateMediaPreparation([fixtureFile]);
    });
    assert.equal(await page.locator('.prep-group[open]').count(),0);
    assert.deepEqual(await page.locator('.prep-count').allTextContents(),["1","2","4","1","2"]);
    await page.locator('[data-prep-disclosure="queued"] > summary').focus();
    await page.evaluate(async()=>{fixtureData.playback.state="running";await hydrateMediaPreparation([fixtureFile]);});
    assert.equal(await page.locator('[data-prep-disclosure="processing"] > summary').evaluate(e=>e===document.activeElement),true,"focus remains in the list after its group disappears");
    await page.evaluate(async()=>{fixtureData.playback.state="queued";await hydrateMediaPreparation([fixtureFile]);});
    const ready=page.locator('[data-prep-disclosure="done-ready"]');
    await ready.locator(':scope > summary').focus();await page.keyboard.press("Enter");
    assert.equal(await ready.evaluate(e=>e.open),true);
    await page.evaluate(async()=>{fixtureData.metadata.detail="Artwork refreshed";await hydrateMediaPreparation([fixtureFile]);});
    assert.equal(await ready.evaluate(e=>e.open),true);
    assert.equal(await ready.locator(':scope > summary').evaluate(e=>e===document.activeElement),true);
    await page.keyboard.press("Space");assert.equal(await ready.evaluate(e=>e.open),false);
    const unknown=page.locator('[data-prep-disclosure="unknown"]');
    await unknown.locator(':scope > summary').click();
    await page.locator('.prep-tracks > summary').click();
    await page.evaluate(async()=>{fixtureData.subtitles.state="done";await hydrateMediaPreparation([fixtureFile]);});
    assert.equal(await ready.evaluate(e=>e.open),true,"focused subtitle disclosure remains visible after moving groups");
    assert.equal(await page.locator('.prep-tracks').evaluate(e=>e.open),true);
    assert.equal(await page.locator('.prep-tracks > summary').evaluate(e=>e===document.activeElement),true);
    for(const width of [960,740,375,320]){
      await page.setViewportSize({width,height:900});
      for(const summary of await page.locator('.trkfold > summary').all()){
        const geometry=await summary.evaluate(e=>{
          const chip=e.querySelector('.trk'),more=e.querySelector('.trkmore');
          const c=chip.getBoundingClientRect(),m=more.getBoundingClientRect(),owner=e.closest('dd').getBoundingClientRect();
          return {singleBox:chip.getClientRects().length===1,within:c.right<=owner.right+1&&c.left>=owner.left-1,separate:m.top>=c.bottom,overflow:chip.scrollWidth>chip.clientWidth+1};
        });
        assert.deepEqual(geometry,{singleBox:true,within:true,separate:true,overflow:false},`track containment at ${width}px`);
      }
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,`page containment at ${width}px`);
    }
    await page.locator('.trkfold > summary').first().focus();await page.keyboard.press("Enter");
    assert.equal(await page.locator('.trkfold').first().evaluate(e=>e.open),true);
    if(process.env.MEDIA_PREPARATION_SCREENSHOT){
      await page.setViewportSize({width:960,height:900});
      await page.evaluate(async()=>{fixtureData.subtitles.state="unknown";await hydrateMediaPreparation([fixtureFile]);document.querySelectorAll('details').forEach(e=>e.open=false);document.activeElement.blur();});
      await page.locator('#preview').screenshot({path:process.env.MEDIA_PREPARATION_SCREENSHOT});
    }
  }finally{await browser.close();}
});
