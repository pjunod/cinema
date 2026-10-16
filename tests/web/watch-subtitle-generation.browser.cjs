"use strict";
// Shipped assets, real browser/media element, intercepted server authority.
// WATCH_MEDIA must be a local H.264 MP4 at least 120 s long. This is browser
// regression evidence, not a substitute for the real-server/media acceptance.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const {test}=require('node:test');
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path');
const root=path.resolve(__dirname,'../../crates/plurxd/src/web');
const media=fs.readFileSync(process.env.WATCH_MEDIA);
const output=process.env.WATCH_SCREENSHOTS;
if(output)fs.mkdirSync(output,{recursive:true});
const movie={id:'20',title:'English speech with existing French captions',kind:'movie',library_id:'1',year:2026,runtime_ms:120000,watch:{position_ms:0,watched:false}};
const file=id=>({id:String(100+Number(id)),filename:'Speech.mp4',available:true,duration_ms:120000,size:media.length,video_codec:'h264',width:640,height:360});
const existing=[{index:0,language:'fra',title:'Existing French captions',text:true,codec:'webvtt'}];
const data=id=>({item:{...movie,id},files:[file(id)],children:[],ancestors:[]});
test('watch generation survives real controller ledger refresh and preserves attached video',async()=>{
 const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE}:{})});
 const page=await browser.newPage({viewport:{width:1440,height:1000},hasTouch:!!process.env.WATCH_COARSE});
 const errors=[],decisions=[];let generationReads=0,generationPublished=false;
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
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
  else if(/^\/items\/\d+$/.test(k))answer=data(k.split('/')[2]);
  else if(k.includes('/decision')){decisions.push(k.split('/')[2]);answer={method:'direct_play',play_url:'/fixture.mp4',source:{video_codec:'h264',width:640,height:360,duration_ms:120000},audio:[],subtitles:existing,ladder:[],reasons:[],delivered_dynamic_range:'sdr'};}
  else if(k==='/subtitle-transcription/models')answer={version:1,models:[{id:'base',label:'Base multilingual',languages:['en']}],workers:[]};
  else if(k==='/files/120/subtitles/transcribe'){
   if(route.request().method()==='POST')answer={outcome:'accepted',job_id:'watch-generation',artifact_key:'artifact-watch',file_id:'120',source_size:String(media.length),source_mtime:'1'};
   else answer={version:1,file_id:'120',source_size:String(media.length),source_mtime:'1',audio_tracks:[{index:0,language:'eng',normalized_language:'en',title:'Original English',default:true}],mode:'managed',model_id:'base',enabled:true,subtitle_streams:[...existing,...(generationPublished?[{index:1,language:'en',title:'Generated English',codec:'webvtt',text:true}]:[])],captions:generationPublished?[{index:1,artifact_key:'artifact-watch',source_size:String(media.length),source_mtime:'1'}]:[]};
  }
  else if(k==='/subtitle-transcription/jobs/watch-generation'){generationPublished=++generationReads>=3;answer={job_id:'watch-generation',state:generationPublished?'succeeded':'running',phase:generationPublished?'succeeded':'transcribing',file_id:'120',source_size:String(media.length),source_mtime:'1',artifact_key:'artifact-watch',result_ref:generationPublished?'artifact-watch':null,attempt:1,fence:1,updated_at_ms:generationReads};}
  else if(k==='/files/120/subs/1')return route.fulfill({contentType:'text/vtt',body:'WEBVTT\n\n00:00.000 --> 00:50.000\nGenerated English captions.\n'});
  else if(k==='/activity')answer={streams:0,scanning:0};
  else if(k==='/dvr/overview')answer={counts:{},active:[],availability:'complete'};
  else if(k==='/dvr/reminders')answer={rows:[]};
  return route.fulfill({contentType:'application/json',body:JSON.stringify(answer)});
 });
 try{
  await page.goto('http://watch.test/#/item/20');
  await page.waitForFunction(()=>WATCH_ITEM_PAGE?.item.id==='20');
  await page.locator('.btnplay').first().click();
  await page.waitForFunction(()=>WATCH?.accepted==='20'&&PLAYER.started&&document.getElementById('video').readyState>=2);
  // The actual watch controller replaces the preparation DOM on Play. Its
  // mirrored ledgers must all target one persistent admin generation panel.
  assert.equal(await page.locator('[data-watch-generate]').count(),0,'nonadmin watch has no generation action');
  await page.evaluate(()=>{ME={...ME,is_admin:true};watchRenderLedger();});
  assert.equal(await page.locator('#watch-subtitle-generation [id^="subtitle-generate-"]').count(),1);
  const generate=page.locator('#watch-ledger [data-watch-generate]');
  await generate.focus();await page.evaluate(()=>watchRenderLedger());
  assert.equal(await page.evaluate(()=>document.activeElement?.dataset.watchGenerate),'120','ledger refresh restores generation action focus');
  await page.evaluate(()=>setSub(-1));
  await page.locator('#pblarger').click();
  await page.locator('#pbinfo').click();
  await page.locator('#watch-details [data-watch-generate]').click();
  await page.locator('#sg-a-120').waitFor();
  assert.equal(await page.locator('#watch-details').evaluate(d=>d.open),false,'dialog action reaches the persistent page form');
  assert.equal(await page.evaluate(()=>WATCH.mode),'slot','wide title-info action reaches visible generation controls');
  await page.locator('#sg-a-120').click();
  await page.evaluate(()=>playerActivity());
  await page.locator('#pbfs').click();
  await page.waitForFunction(()=>!!document.fullscreenElement&&WATCH.mode==='full');
  await page.locator('#pbinfo').click();
  if(!await page.locator('#pinfo [data-watch-generate]').isVisible())await page.locator('#pbinfo').click();
  if(output)await page.screenshot({path:path.join(output,'watch-fullscreen-generation-action.png')});
  await page.locator('#pinfo [data-watch-generate]').click();
  await page.waitForFunction(()=>!document.fullscreenElement&&WATCH.mode==='slot');
  await page.locator('#sg-a-120').click();
  assert.equal(await page.locator('[id="subtitle-generate-120"]').count(),1,'no duplicate mounts in mirrored ledgers or dialog');
  await page.evaluate(()=>{window.watchGenerationPanel=document.getElementById('subtitle-generate-120');window.watchGenerationVideo=document.getElementById('video');window.watchGenerationOwner=PLAYER;window.watchGenerationAttachment=PLAYER.mediaAttachment;});
  await page.locator('#sg-l-120').focus();await page.evaluate(()=>{watchRenderLedger();watchRenderTitle();});
  assert.equal(await page.evaluate(()=>document.activeElement?.id),'sg-l-120','ledger/title refresh leaves form focus intact');
  await page.getByRole('button',{name:'Generate from this audio',exact:true}).click();
  const generationOpens=decisions.length;
  await page.evaluate(()=>{watchRenderLedger();watchRenderLedger();});
  await page.getByRole('button',{name:'Use generated subtitles',exact:true}).waitFor({timeout:15000});
  assert.equal(await page.evaluate(()=>PLAYER.curSub),-1,'published captions preserve Off');
  assert.equal(await page.evaluate(()=>watchGenerationPanel===document.getElementById('subtitle-generate-120')&&watchGenerationVideo===document.getElementById('video')&&watchGenerationOwner===PLAYER&&watchGenerationAttachment===PLAYER.mediaAttachment),true,'real watch mount and playback ownership survive completion');
  await page.locator('.watch-ledger-host [data-watch-fold="subs"]:visible').first().click();
  assert.equal(await page.locator('#watch-ledger [data-watch-sub="1"]').count(),1,'published caption is selectable in refreshed watch ledger');
  if(output)await page.screenshot({path:path.join(output,'watch-generation-ready-off.png'),fullPage:true});
  await page.getByRole('button',{name:'Use generated subtitles',exact:true}).click();
  await page.waitForFunction(()=>PLAYER.curSub===1);
  assert.equal(decisions.length,generationOpens,'text-caption Use never opens another playback');
  assert.equal(await page.evaluate(()=>watchGenerationVideo===document.getElementById('video')&&watchGenerationOwner===PLAYER&&watchGenerationAttachment===PLAYER.mediaAttachment),true);

  assert.deepEqual(errors,[]);
  await page.evaluate(async()=>{await closePlayer({routeLeave:true});location.hash='#/item/21';});
  await page.waitForFunction(()=>WATCH_ITEM_PAGE?.item.id==='21');
  assert.equal(await page.locator('[id="subtitle-generate-120"]').count(),0,'leaving watch disconnects the generation panel');
  console.log('PASS actual watch controller: named admin action, compact/full dialog, persistent mount/focus, Off, exact publication and explicit Use without playback reopen');
 }catch(error){console.error({errors,decisions});if(output)await page.screenshot({path:path.join(output,'watch-generation-failure.png')});throw error;}finally{if(output)fs.writeFileSync(path.join(output,'watch-generation-browser-errors.json'),JSON.stringify(errors));await browser.close();}
});
