"use strict";
const {test}=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path");
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const web=path.resolve(__dirname,"../../crates/plurxd/src/web");
test("real browser owned preplay choices fence changed options and restore the exact opener",async()=>{
  const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE});
  try{
    const page=await browser.newPage(),errors=[];page.on("pageerror",error=>errors.push(error.message));
    await page.setContent('<div id="app"><input id="q"><main id="main"><select id="pp-a-11"><option value="">Default</option><option value="1">English</option><option value="2">French</option></select></main></div><div id="modal" style="display:none"><div id="player"></div></div>');
    await page.addScriptTag({content:`let TOKEN="synthetic",ME={id:1},PLAYER=null,WATCH=null,WATCH_ITEM_PAGE={id:"7",item:{id:"7",title:"Fixture"},shape:"versions",files:[{id:11,available:true}],playable:null},LIVE_TV={serial:1,pendingChannel:null},LIVE_TV_LEASE={current:null},LIB_PAGE_AT=0,LIB_VIEW=null,LIB_PER="all",AUTOPLAY=null;
      const selected=[];function liveTvHost(){return null;}function setPrePlay(file,kind,value){selected.push({file,kind,value});}function exactWireId(value){return value.id;}`});
    await page.addScriptTag({content:fs.readFileSync(path.join(web,"core/remote-navigation.js"),"utf8")});await page.addScriptTag({content:fs.readFileSync(path.join(web,"core/remote-router.js"),"utf8")});
    await page.evaluate(()=>{location.hash="#/item/7";CinemaRemote.snapshot();});
    await page.locator("#remote-preplay-audio-11").click();assert.equal(await page.locator('dialog[open]').count(),1);
    assert.equal(await page.evaluate(()=>CinemaRemote.focusById("choice:2")),"applied");
    assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot())),"applied");
    assert.deepEqual(await page.evaluate(()=>selected),[{file:11,kind:"audio",value:"2"}]);assert.equal(await page.evaluate(()=>document.activeElement.id),"remote-preplay-audio-11");
    await page.locator("#remote-preplay-audio-11").click();await page.evaluate(()=>{CinemaRemote.focusById("choice:1");globalThis.oldChoiceContext=CinemaRemote.snapshot();document.getElementById("pp-a-11").options[1].value="9";});
    assert.notEqual(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},oldChoiceContext)),"applied");assert.equal(await page.evaluate(()=>selected.length),1);assert.equal(await page.locator('dialog[open]').count(),0);
    await page.locator("#remote-preplay-audio-11").click();assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"back"},CinemaRemote.snapshot())),"applied");assert.equal(await page.evaluate(()=>document.activeElement.id),"remote-preplay-audio-11");
    assert.deepEqual(errors,[]);
  }finally{await browser.close();}
});

test("real MAIN physical Pair entry and approval retain network restriction and pending credit fence",async()=>{
  const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE});
  try{
    const page=await browser.newPage();await page.setContent('<div id="app"><input id="q"><main id="main"></main></div><div id="modal" style="display:none"><div id="player"></div></div>');
    await page.addScriptTag({content:`let TOKEN="synthetic",ME={id:1},SERVER={instance_id:"fixture"},PLAYER=null,WATCH=null,WATCH_ITEM_PAGE=null,LIVE_TV={serial:1,pendingChannel:null},LIVE_TV_LEASE={current:null},LIB_PAGE_AT=0,LIB_VIEW=null,LIB_PER="all",AUTOPLAY=null;
      const requests=[];const target={owner_node_id:"fixture",session_id:"00000000-0000-4000-8000-000000000001",receiver_epoch:"00000000-0000-4000-8000-000000000002"};
      let CINEMA_WEB_RECEIVER={target,generation:1,eligible:()=>true,proof:()=>({}),fence:()=>{},challenge:null,pairings:[],client:{request:async(path,options)=>{requests.push({path,body:options.body});return {};}}};
      function liveTvHost(){return null;}function cinemaRemoteIdentity(){return {instance:"fixture",user:"1"};}function cinemaRemoteLocalEnabled(){return true;}function esc(value){return String(value).replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('"','&quot;');}`});
    for(const file of ["core/remote-guard.js","core/remote-navigation.js","core/remote-router.js","pages/remote-pairing.js"])await page.addScriptTag({content:fs.readFileSync(path.join(web,file),"utf8")});
    await page.addScriptTag({content:fs.readFileSync(path.resolve(__dirname,"../../clients/desktop-remote/extension/bridge.js"),"utf8").replaceAll("export function ","function ")});
    await page.evaluate(()=>{location.hash="#/";CinemaRemote.snapshot();CinemaRemote.focusById("local:pair-entry");installBridge("fixture-epoch");});
    assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},{...CinemaRemote.snapshot(),source:"local_cec"})),"restricted_surface");
    assert.equal(await page.evaluate(()=>{CinemaDesktopBridge.probe("entry");return CinemaDesktopBridge.input("select","entry","fixture-epoch");}),"applied");assert.equal(await page.locator("#cinema-pair-dialog[open]").count(),1);
    await page.evaluate(()=>{CINEMA_WEB_RECEIVER.challenge={challenge_id:"00000000-0000-4000-8000-000000000003",deadline:performance.now()+10000};CINEMA_WEB_RECEIVER.pairings=[{pending_id:"00000000-0000-4000-8000-000000000004",controller_name:"Fixture phone"}];cinemaRemotePairingPrompt(CINEMA_WEB_RECEIVER);});
    assert.equal(await page.evaluate(()=>document.activeElement.dataset.localPair),"close");
    assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot())),"restricted_surface");
    await page.locator('[data-local-pair="approve"]').focus();await page.evaluate(()=>CinemaDesktopBridge.probe("old-phone"));
    await page.evaluate(()=>{CINEMA_WEB_RECEIVER.pairings=[{pending_id:"00000000-0000-4000-8000-000000000005",controller_name:"Replacement phone"}];cinemaRemotePairingPrompt(CINEMA_WEB_RECEIVER);document.querySelector('[data-local-pair="approve"]').focus();});
    assert.equal(await page.evaluate(()=>CinemaDesktopBridge.input("select","old-phone","fixture-epoch")),"stale_focus");assert.equal(await page.evaluate(()=>requests.length),0);
    assert.equal(await page.evaluate(()=>{CinemaDesktopBridge.probe("new-phone");return CinemaDesktopBridge.input("select","new-phone","fixture-epoch");}),"applied");
    await page.waitForFunction(()=>requests.length===1);assert.equal(await page.evaluate(()=>requests[0].path),"pairing/approve");assert.equal(await page.evaluate(()=>requests[0].body.pending_id),"00000000-0000-4000-8000-000000000005");
  }finally{await browser.close();}
});

test("real browser episode disclosure rows details Back and stale owner stay within watch scope",async()=>{
  const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE});
  try{
    const page=await browser.newPage();await page.setContent('<div id="app"><input id="q"><main id="main"><section id="watch-browser"><details class="watch-episode-disclosure"><summary>Episodes</summary><button id="watch-row-toggle">Rows</button><div id="watch-episodes"><button data-watch-details="8">Episode details</button><button data-watch-play="8">Play episode</button></div></details><dialog id="watch-details"></dialog></section></main></div><div id="modal" class="open" style="display:block"><div id="player"><button id="pbplay">Play</button></div></div>');
    await page.addScriptTag({content:`let TOKEN="synthetic",ME={id:1},PLAYER=null,WATCH_ITEM_PAGE=null,WATCH={mode:"slot",rows:false,episodes:[{id:"8",title:"Episode eight"}],page:{}},LIVE_TV={serial:1,pendingChannel:null},LIVE_TV_LEASE={current:null},LIB_PAGE_AT=0,LIB_VIEW=null,LIB_PER="all",AUTOPLAY=null;
      function liveTvHost(){return null;}function watchBrowserMounted(){return !!WATCH;}function exactWireId(value){return value.id;}function esc(value){return String(value).replaceAll('<','&lt;');}function playerInputState(){return "transport";}`});
    for(const file of ["core/remote-navigation.js","core/remote-router.js","detail/watch-browser.js"])await page.addScriptTag({content:fs.readFileSync(path.join(web,file),"utf8")});
    await page.evaluate(()=>{location.hash="#/item/7";CinemaRemote.focusById("watch:episodes");});assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot())),"applied");assert.equal(await page.locator("details").getAttribute("open"),"");
    await page.evaluate(()=>CinemaRemote.focusById("watch:rows"));assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot())),"applied");assert.equal(await page.locator("#watch-row-toggle").getAttribute("aria-pressed"),"true");
    await page.evaluate(()=>CinemaRemote.focusById("watch:details:8"));assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot())),"applied");assert.equal(await page.locator("#watch-details[open]").count(),1);
    assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"back"},CinemaRemote.snapshot())),"applied");assert.equal(await page.evaluate(()=>document.activeElement.dataset.watchDetails),"8");
    await page.evaluate(()=>{CinemaRemote.focusById("watch:details:8");globalThis.retiredContext=CinemaRemote.snapshot();WATCH={mode:"slot",page:{},episodes:[]};});assert.notEqual(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},retiredContext)),"applied");assert.equal(await page.locator("#watch-details[open]").count(),0);
  }finally{await browser.close();}
});

function sourceFunction(file,name){
  const source=fs.readFileSync(path.join(web,file),'utf8'),start=source.search(new RegExp('(?:async )?function '+name+'\\('));assert.ok(start>=0);const tail=source.slice(start),next=tail.slice(1).search(/\n(?:async )?function /);return next<0?tail:tail.slice(0,next+1);
}
test("real browser LiveTV channel and guide adapters fence guide replacement and restore opener",async()=>{
  const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE});
  try{
    const page=await browser.newPage();await page.setContent('<div id="app"><input id="q"><main id="main"><button data-channel="channel-1">Channel one</button><button class="lt-cell" data-channel="channel-1" data-cell="0">On now</button><button id="guide-opener">Guide</button></main></div><div id="modal" style="display:none"><div id="player"></div></div><div id="live-tv-sheet" hidden></div>');
    await page.addScriptTag({content:`let TOKEN="synthetic",ME={id:1},PLAYER=null,WATCH=null,WATCH_ITEM_PAGE=null,LIVE_TV={serial:1,channels:[{id:"channel-1",guide_name:"Channel one",guide_number:"1"}],guide:{generation:1},pendingChannel:null},LIVE_TV_LEASE={current:null},LIB_PAGE_AT=0,LIB_VIEW=null,LIB_PER="all",AUTOPLAY=null,LIVE_TV_POP_OPENER=null;
      const effects=[],programme={title:"On now",start:90,end:200};function liveTvHost(){return null;}function liveTvNowSeconds(){return 100;}function liveTvClock(value){return String(value);}function esc(value){return String(value);}function liveTvVisible(){return LIVE_TV.channels;}function liveTvProgramme(){return {now:programme};}function liveTvChannelById(id){return LIVE_TV.channels.find(value=>value.id===id);}function liveTvGridLayout(){return {rows:[{channel:LIVE_TV.channels[0],cells:[{programme}]}]};}function liveTvSetView(value){effects.push(["view",value]);}function liveTvSetFilter(value){effects.push(["filter",value]);}function liveTvSelect(id){effects.push(["channel",id]);}function liveTvPopoverActions(){return '<div class="lt-acts"><button>Watch</button><button id="record-unowned">Record</button></div>';}const PlurxLiveTv={channelView:()=>({disabled:false})};`});
    for(const file of ["core/remote-navigation.js","core/remote-router.js"])await page.addScriptTag({content:fs.readFileSync(path.join(web,file),"utf8")});
    for(const name of ["liveTvGridCell","liveTvPopover","liveTvGuideSheet"])await page.addScriptTag({content:sourceFunction("pages/live-tv.js",name)});
    await page.evaluate(()=>{location.hash="#/live-tv";CinemaRemote.focusById("live-channel:channel-1");});assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot())),"applied");assert.deepEqual(await page.evaluate(()=>effects),[["channel","channel-1"]]);
    await page.evaluate(()=>CinemaRemote.focusById("guide:channel-1:90"));assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot())),"applied");assert.equal(await page.locator("#live-tv-pop:not([hidden])").count(),1);
    await page.locator("#record-unowned").focus();assert.notEqual(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot())),"applied");assert.equal(await page.evaluate(()=>effects.length),1);
    assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"back"},CinemaRemote.snapshot())),"applied");assert.equal(await page.evaluate(()=>document.activeElement.dataset.cell),"0");
    await page.evaluate(()=>{document.getElementById("guide-opener").focus();liveTvGuideSheet();});assert.equal(await page.evaluate(()=>document.activeElement.dataset.remoteSheetClose),"");assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"back"},CinemaRemote.snapshot())),"applied");assert.equal(await page.evaluate(()=>document.activeElement.id),"guide-opener");
    await page.evaluate(()=>{CinemaRemote.focusById("guide:channel-1:90");CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot());globalThis.oldGuideContext=CinemaRemote.snapshot();LIVE_TV.guide={generation:2};});assert.notEqual(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},oldGuideContext)),"applied");assert.equal(await page.evaluate(()=>effects.length),1);
  }finally{await browser.close();}
});

test("actual sibling LiveTV fullscreen strip direction and Select tune its displayed channel",async()=>{
  const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE});
  try{
    const page=await browser.newPage();await page.setContent('<div id="app"><input id="q"><main id="main"><button data-channel="channel-1">App channel</button></main></div><div id="modal" style="display:none"><div id="player"></div></div><section id="live-tv-host" data-mode="full" style="padding:24px;background:#222"><button id="enter-full" onclick="document.getElementById(\'live-tv-host\').requestFullscreen()">Fullscreen</button><div class="lth-strip" style="display:flex;gap:20px"><button data-channel="channel-1">Channel one</button><button data-channel="channel-2">Channel two</button></div></section>');
    await page.addScriptTag({content:`let TOKEN="synthetic",ME={id:1},PLAYER=null,WATCH=null,WATCH_ITEM_PAGE=null,LIVE_TV={serial:1,channels:[{id:"channel-1",guide_name:"Channel one"},{id:"channel-2",guide_name:"Channel two"}],guide:{},pendingChannel:null},LIVE_TV_LEASE={current:{id:"lease-one"}},LIB_PAGE_AT=0,LIB_VIEW=null,LIB_PER="all",AUTOPLAY=null;
      const effects=[];function liveTvHost(){return document.getElementById("live-tv-host");}function liveTvSetView(){}function liveTvSetFilter(){}function liveTvSelect(id){effects.push(id);}function liveTvApplyOutcome(){throw Error("ten-foot strip must use registered controls");}function toggleLiveTvPlayback(){}function resumeLiveTv(){}const PlurxLiveTv={channelView:()=>({disabled:false})};`});
    await page.addScriptTag({content:fs.readFileSync(path.join(web,"playback-policy.js"),"utf8")});await page.addScriptTag({content:'const PlaybackPolicy=PlurxPlaybackPolicy;'});
    await page.addScriptTag({content:sourceFunction("pages/live-tv.js","liveTvInputState")});
    for(const file of ["core/remote-navigation.js","core/remote-router.js"])await page.addScriptTag({content:fs.readFileSync(path.join(web,file),"utf8")});
    await page.evaluate(()=>{location.hash="#/live-tv";});await page.locator("#enter-full").click();await page.waitForFunction(()=>document.fullscreenElement?.id==="live-tv-host");
    assert.equal(await page.evaluate(()=>liveTvInputState()),"fullscreen_controls");assert.equal(await page.evaluate(()=>CinemaRemote.focusById("live-channel:channel-1")),"unavailable");assert.equal(await page.evaluate(()=>CinemaRemote.focusById("live-strip:channel-1")),"applied");
    assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"navigate",direction:"right"},CinemaRemote.snapshot())),"applied");assert.equal(await page.evaluate(()=>document.activeElement.dataset.channel),"channel-2");assert.equal(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},CinemaRemote.snapshot())),"applied");assert.deepEqual(await page.evaluate(()=>effects),["channel-2"]);
    await page.evaluate(()=>{globalThis.oldStripContext=CinemaRemote.snapshot();LIVE_TV.channels=[...LIVE_TV.channels];});assert.notEqual(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},oldStripContext)),"applied");assert.deepEqual(await page.evaluate(()=>effects),["channel-2"]);
    await page.evaluate(()=>{CinemaRemote.focusById("live-strip:channel-2");globalThis.oldLeaseContext=CinemaRemote.snapshot();LIVE_TV_LEASE.current={id:"lease-two"};});assert.notEqual(await page.evaluate(()=>CinemaRemote.dispatch({type:"select"},oldLeaseContext)),"applied");assert.deepEqual(await page.evaluate(()=>effects),["channel-2"]);
  }finally{await browser.close();}
});
