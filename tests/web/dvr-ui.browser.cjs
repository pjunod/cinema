"use strict";
// Real shipped page + intercepted fixture API. No running server or tuner required.
// PLAYWRIGHT_MODULE may point to an existing Playwright installation.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const assert=require("node:assert/strict");
const fs=require('fs'),path=require('path');
const root=path.resolve(__dirname,'../../crates/plurxd/src/web');
const output=process.env.DVR_SCREENSHOTS;
if(output)fs.mkdirSync(output,{recursive:true});
const now=Math.floor(Date.now()/1000);
const active=(id,title,number,phase='writing')=>({recording_id:id,channel_id:'ch-'+id,airing_start:now-1200,airing_end:now+2400,capture_start:now-1260,capture_end:now+2520,title,episode_title:id==='r1'?'The great blue wilderness':null,guide_number:number,channel_name:id==='r1'?'National Geographic':'BBC Two',durable_state:'recording',stop_requested_at_ms:null,last_confirmed_bytes:860000000,total_bytes_written:860000000,display_state:phase==='writing'?'Recording':'Reconnecting',display_detail:phase==='writing'?'Data is being written':'A new capture attempt is starting',can_stop:true,can_view_diagnostics:true,observation:{recording_id:id,owner_node_id:'node1',phase,attempt:phase==='writing'?1:2,observation_age_ms:100,last_write_age_ms:phase==='writing'?200:14000,write_bps:720000,attempt_bytes_written:860000000,first_write_at_ms:(now-1200)*1000}});
const rows=[active('r1','Ocean giants','101'),active('r2','The repair shop','202','reconnecting')];
const durable=r=>({...r,id:r.recording_id,state:'recording',attempt:r.observation.attempt,bytes:860000000,gap_s:0,late_start_s:0});
const saved=[{...durable(rows[0]),id:'saved1',title:'The secret life of forests',state:'done',airing_start:now-86400,airing_end:now-83000,item_id:'item1',file_id:'file1'},{...durable(rows[1]),id:'saved2',title:'A weekend in Provence',state:'partial',stopped_early:true,gap_s:18,airing_start:now-90000}];
const failed={...durable(rows[1]),id:'failed1',title:'Coastal railways',state:'missed',state_reason:'No tuner was available',airing_start:now-4000};
const overview={version:1,availability:'complete',server_now_ms:Date.now(),observation_age_ms:100,counts:{recording:1,reconnecting:1,starting:0,finishing:0,unconfirmed:0,attention:1},active:rows,active_total:2,active_truncated:false};
const channels=rows.map(r=>({id:r.channel_id,guide_number:r.guide_number,guide_name:r.channel_name,enabled:true,available:true,support:'ready',drm:false,hd:true,video_codec:'h264',audio_codec:'aac',video_height:1080}));
const detail={deliveries:[],sessions:[],scans:[],offline:[],live_tv:[],trakt:{},node_hostnames:{node1:'Living room server'},dvr:overview};
(async()=>{
 const browser=await chromium.launch({headless:true});const page=await browser.newPage({viewport:{width:1440,height:1100}});
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.addInitScript(()=>{localStorage.setItem('plurx_token','fixture');localStorage.setItem('plurx_layout','classic');localStorage.setItem('plurx_theme','noirr');localStorage.setItem('plurx_appearance','dark');});
 await page.route('http://dvr.test/**',async route=>{
  const url=new URL(route.request().url());let data;const pathname=url.pathname;
  if(pathname==='/'||pathname==='/index.html')return route.fulfill({contentType:'text/html',body:fs.readFileSync(path.join(root,'index.html'))});
  if(pathname.startsWith('/assets/')){let file=path.join(root,path.basename(pathname));if(fs.existsSync(file))return route.fulfill({contentType:'text/javascript',body:fs.readFileSync(file)});return route.fulfill({status:404,body:''});}
  const key=pathname.replace('/api/v1','');
  if(key==='/server')data={name:'Cinema',version:'dev',setup_required:false};
  else if(key==='/me')data={id:'user1',username:'Paul',is_admin:true};
  else if(key==='/libraries')data=[];
  else if(key==='/activity/detail')data=detail;
  else if(key==='/activity')data={streams:0,scanning:0};
  else if(key==='/dvr/overview')data=overview;
  else if(key==='/dvr/attention')data={rows:[{recording:failed,latest_attention_sequence:5}],total:1};
  else if(key==='/dvr/recordings')data={rows:saved};
  else if(key==='/dvr/schedule')data={rows:rows.map(durable).concat([{...durable(rows[0]),id:'future1',title:'Wild islands',state:'scheduled',airing_start:now+5000,airing_end:now+8600,capture_start:now+4940,capture_end:now+8700}])};
  else if(key.endsWith('/events'))data={rows:[{event_id:'e3',sequence:3,kind:'first_bytes_written',occurred_at_ms:(now-1200)*1000},{event_id:'e2',sequence:2,kind:'attempt_started',occurred_at_ms:(now-1205)*1000},{event_id:'e1',sequence:1,kind:'scheduled',occurred_at_ms:(now-86400)*1000}],history_complete:true};
  else if(key.startsWith('/dvr/recordings/'))data=[...rows.map(durable),...saved,failed].find(r=>r.id===key.split('/')[3])||{};
  else if(key==='/dvr/reminders'||key==='/dvr/rules')data=[];
  else if(key==='/live-tv/channels')data={channels};
  else if(key==='/live-tv/guide')data={status:'ready',channels:rows.map(r=>({id:r.channel_id,programmes:[{title:r.title,start:r.airing_start,end:r.airing_end,synopsis:'Explore the remarkable world beneath the waves, where enormous creatures travel thousands of miles through the open ocean.'},{title:'Wild islands',start:r.airing_end,end:r.airing_end+3600}]}))};
  else if(key==='/dvr/status')data={enabled:true};
  else {throw new Error('Unmocked API request: '+key);}
  return route.fulfill({contentType:'application/json',body:JSON.stringify(data)});
 });
 const screenshot=async name=>{if(output)await page.screenshot({path:path.join(output,name+'.png'),fullPage:name.includes('phone')||name==='overflow'});};
 const noOverflow=async label=>{if(!await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1)){console.log(await page.evaluate(()=>[...document.querySelectorAll('body *')].filter(el=>el.getBoundingClientRect().right>innerWidth+1&&el.getBoundingClientRect().width>0).map(el=>[el.tagName,el.className,el.getBoundingClientRect().right]).slice(0,30)));await screenshot('overflow');throw new Error(label+' must fit viewport')}};
 await page.goto('http://dvr.test/#/activity');await page.waitForSelector('.dvr-card');await page.waitForSelector('.dvr-event');
 assert.equal(await page.locator('.dvr-card').count(),2);
 assert.match(await page.locator('.dvr-detail').innerText(),/Last write/);
 assert.match(await page.locator('.dvr-detail').innerText(),/720 KB\/s/);
 await screenshot('activity-desktop');
 // Selecting another capture stays in Activity and changes the timeline target.
 await page.locator('[data-dvr-focus="open-r2"]').click();
 await page.waitForFunction(()=>DVR_PAGE.selected?.id==='r2');
 assert.equal(new URL(page.url()).hash,'#/activity');
 await page.locator('[data-dvr-focus="detail-technical"]').click();
 await page.evaluate(()=>paintDvrHost());
 assert.equal(await page.locator('.dvr-tech').getAttribute('open'),'');
 assert.equal(await page.evaluate(()=>document.activeElement.dataset.dvrFocus),'detail-technical');
 // Shared refresh must update the current show, strip and guide without replacing video.
 await page.goto('http://dvr.test/#/live-tv');await page.waitForSelector('#live-tv-body');
 await page.waitForFunction(()=>LIVE_TV.guide&&LIVE_TV_DVR.schedule&&DVR_SHARED.overview);
 await page.evaluate(()=>{LIVE_TV.selected=LIVE_TV.channels[0].id;renderLiveTvChannels()});
 assert.match(await page.locator('[data-dvr-live-context]').innerText(),/Recording/);
 assert.match(await page.locator('[data-dvr-also]').innerText(),/The repair shop/);
 await screenshot('live-tv-desktop');
 await page.locator('[data-dvr-focus="live-stop-r1"]').focus();
 await page.evaluate(()=>{window.originalVideo=document.getElementById('live-tv-video');DVR_SHARED.error='Recorder offline';refreshLiveTvDvrUi()});
 assert.equal(await page.evaluate(()=>document.activeElement.dataset.dvrFocus),'live-stop-r1');
 assert.match(await page.locator('[data-dvr-live-context]').innerText(),/Status unavailable/);
 assert.match(await page.locator('[data-dvr-mark]').first().innerText(),/Status unavailable/);
 assert.equal(await page.evaluate(()=>window.originalVideo===document.getElementById('live-tv-video')),true);
 // Padding must name the actual recording rather than claim the next show is recording.
 const padding=await page.evaluate(()=>liveTvRecordingContext(LIVE_TV.channels[0],{start:liveTvNowSeconds()+600,end:liveTvNowSeconds()+3600,title:'Different show'}));
 assert.match(padding,/Ocean giants/);assert.doesNotMatch(padding,/This programme/);
 const fallback=await page.evaluate(()=>{const active=DVR_SHARED.overview.active;DVR_SHARED.overview.active=[];const html=liveTvRecordingContext(LIVE_TV.channels[0],liveTvProgramme(LIVE_TV.channels[0].id).now);DVR_SHARED.overview.active=active;return html});
 assert.match(fallback,/Status unavailable/);
 await page.locator('[data-dvr-live-context] a').click();
 await page.waitForFunction(()=>location.hash==='#/activity'&&DVR_PAGE.selected?.id==='r1');
 // Backgrounding suspends reads; foreground restores the selected timeline.
 await page.evaluate(()=>{Object.defineProperty(document,'visibilityState',{configurable:true,value:'hidden'});document.dispatchEvent(new Event('visibilitychange'));return refreshDvrHistory(DVR_PAGE.selectedId)});
 await page.evaluate(()=>{Object.defineProperty(document,'visibilityState',{configurable:true,value:'visible'});document.dispatchEvent(new Event('visibilitychange'))});
 await page.waitForFunction(()=>DVR_PAGE.historyTimer!==null);
 // A detail response arriving after navigation must not strand its loading flag.
 let releaseDetail,detailRequested;
 const requested=new Promise(resolve=>detailRequested=resolve);
 const release=new Promise(resolve=>releaseDetail=resolve);
 await page.route('http://dvr.test/api/v1/dvr/recordings/r2',async route=>{detailRequested();await release;await route.fulfill({contentType:'application/json',body:JSON.stringify(durable(rows[1]))})});
 await page.evaluate(()=>{void selectDvrDetail('r2')});await requested;
 await page.goto('http://dvr.test/#/live-tv');await page.waitForSelector('#live-tv-body');
 releaseDetail();await page.waitForFunction(()=>!DVR_PAGE.detailBusy);
 await page.unroute('http://dvr.test/api/v1/dvr/recordings/r2');
 await page.goto('http://dvr.test/#/activity');await page.waitForFunction(()=>DVR_PAGE.selected?.id==='r2');
 await page.locator('[data-dvr-focus="open-r1"]').click();await page.waitForFunction(()=>DVR_PAGE.selected?.id==='r1');
 // Stop cancellation sends no request; confirmation names the programme and capture window.
 let mutations=0;page.on('request',request=>{if(request.method()==='DELETE')mutations++});
 page.once('dialog',async dialog=>{assert.match(dialog.message(),/Ocean giants.*\(.+–.+\)/);await dialog.dismiss()});
 await page.locator('[data-dvr-focus="detail-stop"]').click();assert.equal(mutations,0);
 // Saved is a card collection; incomplete files cannot imply playable media.
 await page.goto('http://dvr.test/#/recordings/saved');await page.waitForSelector('.dvr-saved-card');
 assert.equal(await page.locator('.dvr-saved-card').count(),2);
 assert.equal(await page.locator('[data-recording-id="saved2"] a').count(),0);
 await screenshot('recordings-desktop');
 await page.locator('[data-recording-id="saved1"] .dvr-row-button').click();
 await page.waitForFunction(()=>DVR_PAGE.selected?.id==='saved1');
 assert.equal(await page.locator('.dvr-detail a').getAttribute('href'),'#/item/item1');
 // Every registered layout gets the same content and fits at desktop and phone widths.
 for(const layout of ['classic','catalog','theater']){
  await page.evaluate(layout=>{localStorage.setItem('plurx_layout',layout);applyLayout()},layout);
  for(const width of [1440,390,320]){
   await page.setViewportSize({width,height:1000});
   await page.goto('http://dvr.test/#/activity');await page.waitForSelector('.dvr-card');
   await noOverflow(layout+' Activity '+width);
   await page.locator('[data-dvr-focus="open-r1"]').click();await page.waitForSelector('[data-dvr-focus="detail-close"]');
   await noOverflow(layout+' details '+width);
   if(layout==='classic'&&width===390)await screenshot('activity-phone');
   await page.locator('[data-dvr-focus="detail-close"]').click();
   assert.equal(await page.evaluate(()=>document.activeElement.closest('[data-recording-id]')?.dataset.recordingId),'r1');
   await page.goto('http://dvr.test/#/recordings/saved');await page.waitForSelector('.dvr-saved-card');
   await noOverflow(layout+' Saved '+width);
   await page.goto('http://dvr.test/#/live-tv');await page.waitForSelector('#live-tv-body');
   await page.waitForFunction(()=>LIVE_TV.guide&&LIVE_TV_DVR.schedule);
   await page.evaluate(()=>{LIVE_TV.selected=LIVE_TV.channels[0].id;renderLiveTvChannels()});
   await noOverflow(layout+' Live TV '+width);
   if(layout==='classic'&&width===390)await screenshot('live-tv-phone');
  }
 }
 await page.setViewportSize({width:1440,height:1100});
 await page.evaluate(()=>{localStorage.setItem('plurx_layout','classic');applyLayout();localStorage.setItem('plurx_appearance','light');applyTheme()});
 await page.goto('http://dvr.test/#/activity');await page.waitForSelector('.dvr-card');await page.locator('[data-dvr-focus="open-r1"]').click();await page.waitForSelector('.dvr-event');await screenshot('activity-light');
 assert.deepEqual(errors,[],'No uncaught browser errors');
 await browser.close();console.log('DVR browser checks passed: routes, state, focus, cancellation, 3 layouts × 3 widths, dark/light.');
})().catch(e=>{console.error(e);process.exit(1)});
