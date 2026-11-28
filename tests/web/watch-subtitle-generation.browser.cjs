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
const stored={index:2,language:"en",title:"English",codec:"webvtt",text:true,source:"downloaded",ai:true,track_id:"a".repeat(64),downloaded_subtitle_id:"a".repeat(64)};
const existing=[{index:0,language:'fra',title:'Existing French captions',text:true,codec:'webvtt'}];
const data=id=>({item:{...movie,id},files:[file(id)],children:[],ancestors:[]});
test('AI item action publishes a stable stored track without reopening playback',{timeout:35000},async()=>{
 const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE}:{})});
 const page=await browser.newPage({viewport:{width:1440,height:1000},hasTouch:!!process.env.WATCH_COARSE});
 const errors=[],decisions=[];let generationReads=0,generationPublished=false,started=false;
 const mutations=[];
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
  else if(/^\/items\/\d+$/.test(k)){
   answer=data(k.split('/')[2]);
   answer.files[0].subtitle_streams=[...existing,...(generationPublished?[stored]:[])];
  }
  else if(k.includes('/decision')){
   decisions.push(k.split('/')[2]);
   answer={method:'direct_play',play_url:'/fixture.mp4',source:{video_codec:'h264',width:640,height:360,duration_ms:120000},
    audio:[{index:0,language:'eng',default:true}],subtitles:[...existing,...(generationPublished?[stored]:[])],ladder:[],reasons:[],delivered_dynamic_range:'sdr'};
  }
  else if(k==='/files/120/ai-subtitles'){
   if(route.request().method()==='POST'){started=true;mutations.push(route.request().postDataJSON());}
   else if(started){generationPublished=++generationReads>=3;}
   const op={id:'generation-one',route:'audio',state:generationPublished?'complete':'working',progress:generationPublished?1:.3,track_id:generationPublished?stored.track_id:null};
   answer=route.request().method()==='POST'?op:{target:'en',allowed:!generationPublished,route:'audio',
    label:'Create from the audio',existing_index:generationPublished?stored.index:null,operation:started?op:null,alternatives:[]};
  }
  else if(/^\/files\/120\/subs\/2(?:\.vtt)?$/.test(k))return route.fulfill({
   contentType:'text/vtt',body:'WEBVTT\n\n00:00.000 --> 00:50.000\nGenerated English captions.\n'});
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
  await page.evaluate(()=>{window.watchGenerationVideo=document.getElementById('video');window.watchGenerationOwner=PLAYER;window.watchGenerationAttachment=PLAYER.mediaAttachment;});
  const ledger=page.locator('#watch-ledger');
  await ledger.getByRole('button',{name:'+ English',exact:true}).click();
  await ledger.getByRole('button',{name:'Create from the audio',exact:true}).click();
  const generationOpens=decisions.length;
  await page.waitForFunction(()=>document.querySelector('#watch-ledger [data-ai-caption-track="2"]'));
  assert.equal(mutations.length,1,'the visible action starts one durable item operation');
  assert.equal(mutations[0].route,'auto');
  assert.equal(Object.hasOwn(mutations[0],'playback_id'),false,'item generation carries no viewer demand');
  assert.equal(await page.evaluate(()=>PLAYER.curSub),-1,'item generation preserves Off');
  assert.equal(await page.evaluate(()=>watchGenerationVideo===document.getElementById('video')&&watchGenerationOwner===PLAYER&&watchGenerationAttachment===PLAYER.mediaAttachment),true,'stored captions leave the existing video attached');
  assert.equal(decisions.length,generationOpens,'publishing the track does not open another playback');
  assert.equal(await ledger.locator('[data-watch-generate]').count(),0,'the retired generation panel is absent');
  if(output)await page.screenshot({path:path.join(output,'watch-ai-subtitle-ready.png'),fullPage:true});
  assert.deepEqual(errors,[]);

  await page.evaluate(async()=>{await closePlayer({routeLeave:true});location.hash='#/item/21';});
  await page.waitForFunction(()=>WATCH_ITEM_PAGE?.item.id==='21');
  assert.equal(await page.locator('[data-ai-caption-file="120"]').count(),0,'leaving the title retires its controls');
 }catch(error){console.error({errors,decisions});if(output)await page.screenshot({path:path.join(output,'watch-generation-failure.png')});throw error;}finally{if(output)fs.writeFileSync(path.join(output,'watch-generation-browser-errors.json'),JSON.stringify(errors));await browser.close();}
});
