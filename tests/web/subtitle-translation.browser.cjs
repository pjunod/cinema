"use strict";
// Actual shipped watch/player and TextTrack in Chrome, fixture API authority.
// This proves UI delivery and owner fences, not engine/translation quality.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),{createHash}=require('node:crypto');
const root=path.resolve(__dirname,'../../crates/plurxd/src/web');
assert(process.env.WATCH_MEDIA,'Set WATCH_MEDIA to the bounded generated H.264/AAC browser fixture (at least 60s).');
const media=fs.readFileSync(process.env.WATCH_MEDIA),output=process.env.TRANSLATION_SCREENSHOTS;
if(output)fs.mkdirSync(output,{recursive:true});
const digest=bytes=>createHash('sha256').update(bytes).digest('hex');
const A='a'.repeat(64),P='c'.repeat(64),M='d'.repeat(64),E='e'.repeat(64),S='f'.repeat(64),OP='11111111-1111-4111-8111-111111111111',DEMAND='22222222-2222-4222-8222-222222222222',LEASE='33333333-3333-4333-8333-333333333333',NODE='44444444-4444-4444-8444-444444444444',JOB='55555555-5555-4555-8555-555555555555';
// Fast fixture cadence is deliberately test authority, not a production budget.
const limits={segment_bytes:16384,segment_cues:128,page_bytes:65536,page_entries:64,coverage_intervals:64,concurrent_fetches:2,cue_memory_bytes:524288,cue_count:1024,cue_window_before_ms:1000,cue_window_after_ms:20000,poll_min_ms:100,poll_max_ms:200,retry_max_ms:2000,heartbeat_interval_ms:500,demand_ttl_ms:1500};
const profile={profile_id:P,model_id:'fixture-multilingual',model_sha256:M,task:'translate',output_language:'en',input_languages:['de','ja'],adapter_contract:'fixture-adapter',timeline_contract:'fixture-source-clock',activation_state:'active',limits};
const canonical=value=>JSON.stringify(value,(_key,row)=>row&&typeof row==='object'&&!Array.isArray(row)?Object.fromEntries(Object.entries(row).sort(([a],[b])=>a.localeCompare(b))):row);
const bodies=[0,2].map(core=>{const start=core*20000,id=digest('segment'+core);const body=Buffer.from(canonical({version:1,asset_id:A,core_index:core,start_ms:start,end_ms:start+20000,segment_id:id,cues:[{cue_id:digest('cue'+core),start_ms:start,end_ms:start+18000,text:core?'Later translated range.':'A & B <b>literal</b> <00:01>\nline'}],provenance:{engine_sha256:E,model_sha256:M,operation_epoch:'1',platform:'browser-fixture',profile_id:P,source_sha256:S,worker_fence:'1'}}));return {body,reference:{segment_id:id,digest:digest(body),start_ms:start,end_ms:start+20000}};});
test('progressive translation retains the shipped video, literal cues, original captions and Stop intent',{timeout:90000},async()=>{
 const browser=await chromium.launch({headless:true,...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE?{executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE}:{})});
 const page=await browser.newPage({viewport:{width:1440,height:1100}}),errors=[],mutations=[],decisions=[];
 let installed=false,modelReads=0,epoch='1',view='1',stopped=false,operationReads=0;
 let saved={version:1,revision:'0.settings',enabled:false,model_id:null,profile_id:null,preferred_spoken_language:null};
 const now=()=>Date.now();const catalog=()=>({version:1,catalog_revision:'fixture',models:[{id:profile.model_id,label:'Fixture multilingual model',download_bytes:100,sha256:M,license:'MIT'}],profiles:[profile],workers:[{node_id:NODE,label:'Named fixture worker',current:true,eligible:true,observed_at_ms:now(),expires_at_ms:now()+60000,engine:{adapter_contract:profile.adapter_contract,binary_sha256:E,platform:'fixture'},capabilities:[{model_id:profile.model_id,profile_id:P,status:installed&&++modelReads>1?'verified':'unverified_cache'}]}],server_now_ms:now()});
 const originals=[{index:0,language:'fra',title:'Original French captions',text:true,codec:'webvtt'}];
 const file={id:'120',filename:'Fixture.mp4',available:true,duration_ms:60000,size:media.length,video_codec:'h264',width:640,height:360,audio_streams:[{index:0,language:'deu',default:true}],subtitle_streams:originals};
 const item={id:'20',title:'Progressive translation UI fixture',kind:'movie',library_id:'1',year:2026,runtime_ms:60000,watch:{position_ms:0,watched:false}};
 const live=()=>({version:1,operation_id:OP,operation_epoch:epoch,view_revision:view,state:stopped?'stopped':'partial',reason:null,retryable:stopped,asset_id:++operationReads<2?null:A,manifest_revision:'7',file_id:'120',source_size:String(media.length),source_mtime:'1',content_revision:S,audio_index:0,spoken_language:'ja',output_language:'en',task:'translate',model_id:profile.model_id,model_sha256:M,profile_id:P,timeline_contract:profile.timeline_contract,source_check:{state:'verified',bytes_read:String(media.length),total_bytes:String(media.length)},active_job:null,failed_ranges:[],failed_ranges_next_cursor:null,server_now_ms:now()});
 page.on('pageerror',e=>errors.push(e.message));page.on('console',m=>{if(m.type()==='error')errors.push(m.text());});
 await page.addInitScript(()=>{localStorage.setItem('plurx_token','fixture');localStorage.setItem('plurx_layout','classic');});
 await page.route('https://translation.test/**',async route=>{
  const req=route.request(),u=new URL(req.url()),p=u.pathname,k=p.replace('/api/v1','');const json=(answer,headers={})=>route.fulfill({contentType:'application/json',headers,body:JSON.stringify(answer)});
  if(p==='/')return route.fulfill({contentType:'text/html',body:fs.readFileSync(path.join(root,'index.html'))});
  if(p.startsWith('/assets/')){const f=path.join(root,p.slice(8));return fs.existsSync(f)?route.fulfill({contentType:p.endsWith('.css')?'text/css':'text/javascript',body:fs.readFileSync(f)}):route.fulfill({status:404,body:''});}
  if(p==='/fixture.mp4'){const match=/bytes=(\d+)-(\d*)/.exec(req.headers().range||''),start=match?Number(match[1]):0,end=match&&match[2]?Math.min(Number(match[2]),media.length-1):media.length-1;return route.fulfill({status:match?206:200,contentType:'video/mp4',headers:{'accept-ranges':'bytes',...(match?{'content-range':`bytes ${start}-${end}/${media.length}`}:{})},body:media.subarray(start,end+1)});}
  if(k==='/server')return json({name:'Fixture',version:'dev',setup_required:false});
  if(k==='/me')return json({id:'1',username:'Admin fixture',is_admin:true});
  if(k==='/libraries')return json([{id:'1',name:'Fixture',kind:'movie'}]);
  if(k==='/items/20')return json({item,files:[file],children:[],ancestors:[]});
  if(k.includes('/decision')){decisions.push(k);return json({method:'direct_play',play_url:'/fixture.mp4',source:{video_codec:'h264',width:640,height:360,duration_ms:60000},audio:[{index:0,language:'deu',default:true}],subtitles:originals,ladder:[],reasons:[],delivered_dynamic_range:'sdr'});}
  if(k==='/subtitle-translation'){
   if(req.method()==='PUT'){const body=req.postDataJSON();mutations.push({path:k,body});saved={...saved,...body,revision:'1.settings'};}
   return json({...saved,server_now_ms:now()});
  }
  if(k==='/subtitle-translation/models')return json(catalog());
  if(k===`/subtitle-transcription/models/${profile.model_id}/install`){const body=req.postDataJSON();mutations.push({path:k,body});installed=true;return json({version:2,purpose:'translation',model_id:profile.model_id,profile_id:P,operations:[{node_id:NODE,job_id:JOB,outcome:'accepted'}],server_now_ms:now()});}
  if(k===`/subtitle-transcription/jobs/${JOB}`)return json({job_id:JOB,state:'succeeded',phase:'succeeded'});
  if(k==='/files/120/subtitle-translation/options')return json({version:1,file_id:'120',source_size:String(media.length),source_mtime:'1',duration_ms:60000,content_revision:null,timeline_contract:profile.timeline_contract,audio_tracks:[{index:0,language:'deu',normalized_language:'de',title:'Attached audio',default:true}],subtitle_streams:originals,models:[profile],capability:{supported:true,reason:null},permissions:{read:true,demand:true,stop:true,resume:true},assets:[],assets_next_cursor:null,server_now_ms:now()});
  if(k==='/files/120/subtitle-translation'){const body=req.postDataJSON();mutations.push({path:k,body});return json({version:1,outcome:'accepted',operation_id:OP,operation_epoch:epoch,view_revision:view,job_id:JOB,asset_id:null,state:'checking_source',demand_id:DEMAND,demand_generation:'1',server_now_ms:now()});}
  if(k===`/subtitle-translation/operations/${OP}`)return json(live());
  if(k.includes('/demands/')){const body=req.postDataJSON();mutations.push({path:k,body});return json({version:1,outcome:req.method()==='DELETE'?'released':'updated',operation_id:OP,operation_epoch:epoch,demand_id:DEMAND,demand_generation:body.demand_generation,view_revision:view,server_now_ms:now()});}
  if(k.endsWith('/stop')){mutations.push({path:k,body:req.postDataJSON()});stopped=true;epoch='2';view='9';return json({version:1,outcome:'stopped',operation_id:OP,operation_epoch:epoch,view_revision:view,state:'stopped',asset_id:A,server_now_ms:now()});}
  if(k===`/subtitle-translation/assets/${A}/manifest`){const index=Number(u.searchParams.get('position_ms'))>=20000?1:0,segment=bodies[index];return json({version:1,asset_id:A,file_id:'120',content_revision:S,profile_id:P,audio_index:0,spoken_language:'ja',output_language:'en',timeline_contract:profile.timeline_contract,manifest_revision:'7',view_revision:view,operation:{operation_id:OP,epoch,state:stopped?'stopped':'partial',reason:null},snapshot:{lease_id:LEASE,revision:'7',expires_in_ms:30000},coverage:[{start_ms:segment.reference.start_ms,end_ms:segment.reference.end_ms}],segments:[segment.reference],coverage_next_cursor:null,segment_next_cursor:null,server_now_ms:now()},{ETag:`"view-${view}"`,'X-Translation-Server-Now-Ms':String(now()),'X-Translation-Read-Lease-Expires-At-Ms':String(now()+30000)});}
  const segment=bodies.find(s=>k.endsWith('/segments/'+s.reference.segment_id));if(segment){assert.equal(req.headers()['x-translation-read-lease'],LEASE);return route.fulfill({contentType:'application/json',body:segment.body});}
  if(k==='/files/120/subs/0')return route.fulfill({contentType:'text/vtt',body:'WEBVTT\n\n00:00.000 --> 00:59.000\nOriginal French captions.\n'});
  if(k==='/dvr/overview')return json({counts:{},active:[],availability:'complete'});if(k==='/dvr/reminders')return json({rows:[]});
  return json({});
 });
 try{
  await page.goto('https://translation.test/#/settings/developer/enable-subtitle-translation');await page.getByRole('button',{name:'Configure English translation',exact:true}).click();
  assert.equal(await page.locator('[data-translation-setting-profile]').inputValue(),P,'first use suggests the sole active profile');
  await page.locator('[data-translation-setting-profile]').selectOption('');
  await page.locator('[data-translation-setting-enabled]').check();await page.locator('[data-translation-settings]').getByRole('button',{name:'Save',exact:true}).click();await page.getByText(/Saved\. Current unsaved controls/).waitFor();assert.equal(saved.enabled,true);assert.equal(saved.profile_id,null,'enabled unconfigured remains saveable');
  await page.locator('[data-translation-setting-profile]').selectOption(P);await page.getByRole('button',{name:'Download and test',exact:true}).click();await page.getByText('Verified for English translation',{exact:false}).waitFor();assert.equal(mutations.find(m=>m.path.endsWith('/install')).body.version,2);
  await page.goto('https://translation.test/#/item/20');await page.locator('.btnplay').first().click();await page.waitForFunction(()=>PLAYER?.started&&document.getElementById('video')?.readyState>=2);assert.equal(await page.evaluate(()=>PLAYER.method),'direct_play');
  await page.evaluate(()=>{window.originalVideo=document.getElementById('video');window.originalPlayer=PLAYER;window.originalAttachment=PLAYER.mediaAttachment;window.originalSrc=originalVideo.currentSrc;});
  await page.locator('#watch-ledger [data-watch-translate]').click();await page.locator('[data-translation-language]').selectOption('ja');await page.locator('[data-translation-confirm]').check();await page.locator('[data-translation-start]').click();await page.getByText('Partial English captions available.',{exact:true}).waitFor();assert.equal(await page.evaluate(()=>PLAYER.curSub),-1);assert.equal(await page.evaluate(()=>translationSelected(PLAYER)),false,'partial publication preserves Off');
  const opens=decisions.length;await page.locator('[data-translation-use]').click();await page.waitForFunction(()=>document.getElementById('video')._translationTrack?.activeCues?.length>0);
  assert.equal(await page.evaluate(()=>document.getElementById('video')._translationTrack.cues[0].getCueAsHTML().textContent),'A & B <b>literal</b> <00:01>\nline');
  assert.equal(mutations.find(m=>m.path==='/files/120/subtitle-translation').body.spoken_language,'ja','confirmed language corrects metadata');
  await page.locator('#player').hover();await page.locator('#pbsubs').click();await page.locator('#pmenu').getByRole('menuitem',{name:'Off',exact:true}).click();assert.equal(await page.evaluate(()=>translationSelected(PLAYER)),false);assert.equal(await page.evaluate(()=>document.getElementById('video')._translationTrack.mode),'disabled');
  await page.locator('[data-translation-use]').click();await page.locator('[data-translation-stop]').click();await page.getByText('Translation stopped. Committed captions remain available.',{exact:true}).waitFor();const heartbeats=mutations.filter(m=>m.path.includes('/demands/')).length;await page.waitForTimeout(1100);assert.equal(mutations.filter(m=>m.path.includes('/demands/')).length,heartbeats,'Stop never auto renews demand');
  assert.equal(await page.evaluate(()=>originalVideo===document.getElementById('video')&&originalPlayer===PLAYER&&originalAttachment===PLAYER.mediaAttachment&&originalSrc===originalVideo.currentSrc),true);assert.equal(decisions.length,opens,'translation does not reopen media');
  if(output)await page.screenshot({path:path.join(output,'translation-partial-stopped.png'),fullPage:true});assert.deepEqual(errors,[]);
 }catch(error){if(output)await page.screenshot({path:path.join(output,'translation-browser-failure.png'),fullPage:true}).catch(()=>{});throw error;}
 finally{if(output)fs.writeFileSync(path.join(output,'translation-browser-receipt.json'),JSON.stringify({scope:'UI fixture only, no engine or quality qualification',errors,mutations,decisions},null,2));await browser.close();}
});
