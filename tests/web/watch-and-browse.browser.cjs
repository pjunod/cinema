"use strict";
// Shipped assets, real browser/media element, intercepted server authority.
// WATCH_MEDIA must be a local H.264 MP4 at least 120 s long. This is browser
// regression evidence, not a substitute for the real-server/media acceptance.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
const root=path.resolve(__dirname,'../../crates/plurxd/src/web');
const media=fs.readFileSync(process.env.WATCH_MEDIA);
const output=process.env.WATCH_SCREENSHOTS;
if(output)fs.mkdirSync(output,{recursive:true});
const episodes=[1,2,3,4].map(id=>({id:String(id),title:`Episode ${id}: Across the water`,kind:'episode',library_id:'1',season_number:1,episode_number:id,runtime_ms:120000,overview:'A journey along the coast reveals an unexpected meeting. The crew explores the shore while the tide turns.',watch:{position_ms:0,watched:false}}));
const show={id:'10',title:'The distant shore',kind:'show',library_id:'1'},season={id:'11',title:'Season 1',kind:'season',library_id:'1'};
const movie={id:'20',title:'Beyond the horizon',kind:'movie',library_id:'1',year:2026,runtime_ms:120000,overview:'Two travellers set out across an unfamiliar coastline, discovering stories in the places they thought they knew.',watch:{position_ms:0,watched:false}};
const file=id=>({id:String(100+Number(id)),filename:'Coast.mp4',available:true,duration_ms:120000,size:media.length,video_codec:'h264',width:640,height:360,chapters:[{index:0,title:'Departure',start_ms:0},{index:1,title:'Open water',start_ms:40000},{index:2,title:'Arrival',start_ms:80000}]});
const data=id=>id==='10'?{item:show,files:[],children:[season],ancestors:[]}:id==='11'?{item:season,files:[],children:episodes,ancestors:[show]}:{item:id==='20'?movie:episodes.find(e=>e.id===id),files:[file(id)],children:[],ancestors:id==='20'?[]:[show,season]};
(async()=>{
 const browser=await chromium.launch({headless:true});
 const page=await browser.newPage({viewport:{width:1440,height:1000},hasTouch:!!process.env.WATCH_COARSE});
 const touchSession=process.env.WATCH_COARSE?await page.context().newCDPSession(page):null;
 const errors=[],posts=[],decisions=[];let blockedSave=null,delayItem=null,failedFile=null,onProgress=null;
 page.on('pageerror',e=>{errors.push(e.message);console.error('PAGE:',e.message);});
 await page.addInitScript(()=>{localStorage.setItem('plurx_token','fixture');localStorage.setItem('plurx_layout','classic');localStorage.setItem('plurx_theme','noirr');localStorage.setItem('plurx_appearance','dark');});
 await page.route('http://watch.test/**',async route=>{
  const u=new URL(route.request().url()),p=u.pathname,k=p.replace('/api/v1','');
  if(p==='/')return route.fulfill({contentType:'text/html',body:fs.readFileSync(path.join(root,'index.html'))});
  if(p.startsWith('/assets/')){const f=path.join(root,p.slice(8));return fs.existsSync(f)?route.fulfill({contentType:p.endsWith('.css')?'text/css':'text/javascript',body:fs.readFileSync(f)}):route.fulfill({status:404,body:''});}
  if(p==='/fixture.mp4'){
   const range=route.request().headers()['range'];
   if(range){const match=/bytes=(\d+)-(\d*)/.exec(range),start=Number(match[1]),end=match[2]?Math.min(Number(match[2]),media.length-1):media.length-1;return route.fulfill({status:206,contentType:'video/mp4',headers:{'accept-ranges':'bytes','content-range':`bytes ${start}-${end}/${media.length}`},body:media.subarray(start,end+1)});}
   return route.fulfill({contentType:'video/mp4',headers:{'accept-ranges':'bytes'},body:media});
  }
  let answer={};
  if(k==='/server')answer={name:'Cinema',version:'dev',setup_required:false};
  else if(k==='/me')answer={id:'1',username:'Viewer',is_admin:false};
  else if(k==='/libraries')answer=[{id:'1',name:'Cinema',kind:'movie'}];
  else if(/^\/items\/\d+$/.test(k)){const id=k.split('/')[2];if(delayItem?.id===id){delayItem.entered?.();await delayItem.wait;}answer=data(id);}
  else if(k.endsWith('/progress')){const body=route.request().postDataJSON(),id=k.split('/')[2];posts.push({id,...body});onProgress?.();(id==='20'?movie:episodes.find(e=>e.id===id)).watch={position_ms:body.position_ms,watched:false};if(blockedSave)await blockedSave.wait;}
  else if(k.includes('/decision')){const id=k.split('/')[2];decisions.push(id);if(id===failedFile)return route.fulfill({status:400,contentType:'application/json',body:JSON.stringify({message:'Fixture refused'})});answer={method:'direct_play',play_url:'/fixture.mp4',source:{video_codec:'h264',width:640,height:360,duration_ms:120000},audio:[],subtitles:[],ladder:[],reasons:[],delivered_dynamic_range:'sdr'};}
  else if(k==='/activity')answer={streams:0,scanning:0};
  else if(k==='/dvr/overview')answer={counts:{},active:[],availability:'complete'};
  else if(k==='/dvr/reminders')answer={rows:[]};
  return route.fulfill({contentType:'application/json',body:JSON.stringify(answer)});
 });
 try{
  await page.goto('http://watch.test/#/item/1');
  await page.waitForFunction(()=>WATCH_ITEM_PAGE?.item.id==='1');
  await page.evaluate(()=>{const p=WATCH_ITEM_PAGE;return play(p.playable.id,p.item.title,p.playStart,p.playable.duration_ms,playbackMetaFor(p,p.playable));});
  await page.waitForFunction(()=>WATCH?.accepted==='1');
  await page.waitForFunction(()=>document.getElementById('video').readyState>=2);
  await page.evaluate(()=>document.getElementById('video').play());
  await page.waitForFunction(()=>PLAYER.started&&!document.getElementById('ploading').classList.contains('on'));
  await page.waitForSelector('[data-watch-play="2"]');
  assert.equal(await page.locator('#app').evaluate(e=>e.inert),false);
  assert.equal(await page.locator('#modal').getAttribute('aria-modal'),null);
  await page.evaluate(()=>{window.watchOriginalVideo=document.getElementById('video');window.watchOriginalPlayer=PLAYER;window.watchOriginalAttachment=PLAYER.mediaAttachment;togglePlay();});
  const initialOpens=decisions.length;
  for(let i=0;i<20;i++){
   await page.evaluate(()=>toggleWatchSize());
   await page.evaluate(()=>toggleWatchSize());
  }
  assert.equal(decisions.length,initialOpens);
  assert.equal(await page.evaluate(()=>PLAYER===window.watchOriginalPlayer&&document.getElementById('video')===window.watchOriginalVideo&&PLAYER.mediaAttachment===window.watchOriginalAttachment),true);
  await page.locator('[data-watch-details="2"]').click();
  assert.equal(decisions.length,initialOpens);assert.equal(await page.evaluate(()=>WATCH.accepted),'1');
  await page.locator('#watch-details button').first().click();
  await page.locator('#pbplay').focus();await page.keyboard.press('Escape');
  assert.equal(await page.evaluate(()=>WATCH.accepted),'1');
  // Browser API, not a manufactured fullscreenchange event.
  await page.locator('#pbfs').click();
  await page.waitForFunction(()=>!!document.fullscreenElement&&WATCH.mode==='full');
  await page.evaluate(()=>document.exitFullscreen());
  await page.waitForFunction(()=>!document.fullscreenElement&&WATCH.mode==='slot');
  assert.equal(decisions.length,initialOpens);
  await page.locator('#pbfs').click();
  await page.waitForFunction(()=>WATCH.mode==='full');
  await page.locator('#pblarger').click();
  await page.waitForFunction(()=>WATCH.mode==='slot'&&!document.fullscreenElement);
  assert.equal(await page.evaluate(()=>document.activeElement.id),'pblarger');
  for(const width of [320,390,500,550,551,600,700,760,761,1024,1440]){
   await page.setViewportSize({width,height:1000});
   if(touchSession)await touchSession.send("Emulation.setTouchEmulationEnabled",{enabled:true,maxTouchPoints:1});
   await page.waitForFunction(()=>{const a=document.getElementById('watch-slot').getBoundingClientRect(),b=document.getElementById('modal').getBoundingClientRect();return Math.abs(a.width-b.width)<1;});
   await page.evaluate(()=>new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r))));
   const dims=await page.evaluate(()=>({overflow:document.documentElement.scrollWidth>innerWidth,larger:!!document.getElementById('pblarger'),titleInfo:!!document.getElementById('pbinfo'),surface:playerInputSurface(),slot:document.getElementById('watch-slot').getBoundingClientRect().toJSON(),video:document.getElementById('video').getBoundingClientRect().toJSON()}));
   assert.equal(dims.overflow,false,`overflow at ${width}`);assert.equal(dims.larger,width>550);assert.equal(dims.titleInfo,false);assert.equal(dims.surface,'desktop');
   assert.ok(Math.abs(dims.video.width/dims.video.height-16/9)<.02,`16:9 at ${width}`);
   if(process.env.WATCH_COARSE){
    assert.equal(await page.evaluate(()=>matchMedia("(pointer:coarse)").matches),true,"coarse emulation must be active");
    const targets=await page.locator('#player .picon:visible,#pbar button:visible').evaluateAll(nodes=>nodes.map(n=>({id:n.id,w:n.getBoundingClientRect().width,h:n.getBoundingClientRect().height,min:getComputedStyle(n).minHeight,coarse:matchMedia("(pointer:coarse)").matches})));
    for(const t of targets){assert.ok(t.w>=44&&t.h>=44,`${JSON.stringify(t)}: coarse target at ${width}`);}
   }
   if(output&&[390,600,1440].includes(width))await page.screenshot({path:path.join(output,`series-${width}.png`),fullPage:!process.env.WATCH_COARSE});
  }
  await page.setViewportSize({width:1440,height:1000});
  let releaseOld,oldRequested;
  const requested=new Promise(r=>{oldRequested=r;});
  delayItem={id:'3',wait:new Promise(r=>{releaseOld=r;}),entered:oldRequested};
  await page.evaluate(()=>{window.oldEpisodeLoad=watchPlayEpisode('3');});
  await requested;
  await page.locator('#watch-episodes [data-watch-play="2"]').click();
  await page.waitForFunction(()=>WATCH.accepted==='2');
  releaseOld();delayItem=null;await page.evaluate(()=>window.oldEpisodeLoad);
  assert.equal(await page.evaluate(()=>ITEM_FOR_FILE['103']),undefined,'stale load must not mutate file identity');
  assert.equal(new URL(page.url()).hash,'#/item/2');
  assert.equal(await page.locator('#watch-episodes [data-watch-play="2"]').isDisabled(),true);
  await page.waitForFunction(()=>document.getElementById('video').readyState>=2);
  await page.evaluate(()=>seekTo(14));
  await page.waitForFunction(()=>document.getElementById('video').currentTime>=14&&!document.getElementById('video').seeking);
  await page.evaluate(()=>reportProgress(PLAYER.fileId,false,PLAYER));
  assert.ok(posts.some(p=>p.id==='2'&&p.position_ms>=14000));
  await page.evaluate(()=>playNextEpisode());await page.waitForFunction(()=>WATCH.accepted==='3');
  // The final save at Close is a zero-position beat until this episode has
  // played, and a zero beat needs a witness: the current attachment must have
  // reached a timeline (readyState >= 1) or reportProgress suppresses it, so
  // the resume point of an unplayable start is never overwritten with 0.
  // A failed preparation below swaps the element under this player, so the
  // witness has to be recorded on the player now, by a beat taken while the
  // element is ready — the same beat the periodic heartbeat would take.
  await page.waitForFunction(()=>document.getElementById('video').readyState>=1);
  await page.evaluate(()=>reportProgress(PLAYER.fileId,false,PLAYER));
  failedFile='104';await page.evaluate(()=>watchPlayEpisode('4'));
  assert.equal(await page.evaluate(()=>WATCH.accepted),'3');assert.equal(new URL(page.url()).hash,'#/item/3');
  failedFile=null;
  // Final save must settle before Close re-reads the last accepted item.
  let unblock;blockedSave={wait:new Promise(r=>{unblock=r;})};
  const beforeClose=posts.length;
  const saved=new Promise(r=>{onProgress=r;});
  await page.evaluate(()=>{window.watchClosing=closePlayer();closePlayer();});
  await saved;onProgress=null;
  assert.equal(await page.locator('#modal').evaluate(e=>e.classList.contains('open')),false);
  assert.equal(posts.length,beforeClose+1);
  assert.equal(await page.locator('#watch-browser').count(),1);
  unblock();blockedSave=null;
  await page.evaluate(()=>window.watchClosing);
  await page.waitForFunction(()=>!document.getElementById('watch-browser'));
  assert.equal(new URL(page.url()).hash,'#/item/3');
  await page.goto('http://watch.test/#/item/20');await page.waitForFunction(()=>WATCH_ITEM_PAGE?.item.id==='20');
  await page.evaluate(()=>{const p=WATCH_ITEM_PAGE;return play(p.playable.id,p.item.title,0,p.playable.duration_ms,playbackMetaFor(p,p.playable));});
  await page.waitForFunction(()=>WATCH?.accepted==='20');
  await page.waitForFunction(()=>document.getElementById('video').readyState>=2&&!document.getElementById('ploading').classList.contains('on'));
  await page.locator('[data-watch-chapter="1"]').click();
  await page.waitForFunction(()=>document.getElementById('video').currentTime>=39);
  await page.locator('#toast.on').waitFor({state:'hidden'});
  if(output)await page.screenshot({path:path.join(output,'movie-1440.png'),fullPage:!process.env.WATCH_COARSE});
  await page.evaluate(()=>closePlayer({routeLeave:true}));
  await page.evaluate(()=>{history.replaceState(null,"","#/library-channels");const p=WATCH_ITEM_PAGE;return play(p.playable.id,p.item.title,0,p.playable.duration_ms,playbackMetaFor(p,p.playable));});
  assert.equal(await page.evaluate(()=>WATCH.mode),'full');
  assert.equal(await page.evaluate(()=>!!document.fullscreenElement),false);
  await page.locator('#pbplay').focus();await page.keyboard.press('Escape');
  await page.waitForFunction(()=>!WATCH&&!document.getElementById('modal').classList.contains('open'));
  assert.equal(new URL(page.url()).hash,'#/library-channels');
  // A late-watch fence must never dismiss a later Live TV/other host fullscreen.
  await page.evaluate(()=>{const b=document.createElement('button');b.id='other-fullscreen';b.textContent='Other fullscreen';b.onclick=()=>document.getElementById('app').requestFullscreen();document.getElementById('app').prepend(b);});
  await page.locator('#other-fullscreen').click();
  await page.waitForFunction(()=>document.fullscreenElement===document.getElementById('app'));
  await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
  assert.equal(await page.evaluate(()=>document.fullscreenElement===document.getElementById('app')),true);
  await page.evaluate(()=>document.exitFullscreen());
  assert.deepEqual(errors,[]);
  console.log('PASS shipped watch browser: persistent media, fullscreen return, 11 widths, episode acceptance/failure, Play next, progress, Close ordering and chapter seek');
 }catch(e){console.error(e);console.error({errors,decisions});console.error(await page.evaluate(()=>({surface:PLAYBACK_SURFACE.surface,loading:document.getElementById('ploadText').textContent,paused:document.getElementById('video').paused,watch:WATCH&&{accepted:WATCH.accepted,page:WATCH.page?.item.id,mode:WATCH.mode},player:PLAYER&&{fileId:PLAYER.fileId,pending:PLAYER.pendingOpenAttempt,started:PLAYER.started,seek:PLAYER.controlSeek},time:document.getElementById('video').currentTime,chapters:Array.from(document.querySelectorAll('[data-watch-chapter]')).map(b=>b.dataset.startMs)})));if(output)await page.screenshot({path:path.join(output,'failure.png')});throw e;}finally{await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;});
