"use strict";
// Exercise the shipped durable panel with the endpoint's bounded, id-ordered pages.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const assert=require("node:assert/strict");
const path=require("node:path");
const root=path.resolve(__dirname,"../../crates/plurxd/src/web");
(async()=>{
 const browser=await chromium.launch({headless:true});
 try{
 const page=await browser.newPage({viewport:{width:1280,height:1000}});
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.setContent('<main style="padding:24px"><div id="durable-activity"></div></main>');
 await page.addStyleTag({path:path.join(root,'app.css')});
 await page.evaluate(()=>{
  location.hash='#/activity';
  window.ME={is_admin:true};window.PAGE_RENDER_GENERATION=1;
  window.ACTIVITY_SNAPSHOT={node_hostnames:{node:'Living room'}};
  window.esc=s=>String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  window.nodeLabel=(names,id)=>names[id]||id;
  window.jobs=Array.from({length:105},(_,i)=>({id:`00000000-0000-4000-8000-${String(i+1).padStart(12,'0')}`,kind:'fragment_index_build',title:`Film ${i+1}`,library:'Movies',state:'queued',owner_node_id:'node',priority:2,age_ms:120000,supported:true,failed_attempts:0,yield_count:0,observed_at_ms:Date.now()}));
  window.api=async url=>{
   if(window.fail)throw new Error('<offline>');
   if(url.includes('?')){
    const query=new URL(url,'http://fixture').searchParams;
    const rows=window.jobs.filter(j=>j.state===query.get('state')&&j.id>(query.get('cursor')||''));
    const limit=Number(query.get('limit'));
    return {jobs:rows.slice(0,limit),next_cursor:rows.length>limit?rows[limit-1].id:null,counts:[{kind:'fragment_index_build',state:'queued',count:window.jobs.length}],observed_at_ms:Date.now(),repairs:[{id:'repair1',job_id:window.jobs[1].id,kind:'subtitle',target_node_id:'node',phase:'copying',age_ms:1000}]};
   }
   if(window.hold)await new Promise(resolve=>window.release=resolve);
   return {job:window.jobs.find(j=>url.endsWith(j.id)),attempts:[],waiters:[]};
  };
 });
 await page.addScriptTag({path:path.join(root,'pages/activity.js')});
 await page.evaluate(()=>refreshDurableActivity(true));
 assert.equal(await page.locator('.durable-title').count(),20);
 // Keyboard pagination and refresh retain focus through disabled loading states.
 await page.getByRole('button',{name:'Next',exact:true}).focus();
 await page.keyboard.press('Enter');
 await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);
 assert.equal(await page.evaluate(()=>document.activeElement.dataset.durableFocus),'next');
 await page.keyboard.press('Enter');
 await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);
 assert.match(await page.locator('.durable-title').first().innerText(),/Film 41$/);
 await page.getByRole('button',{name:'Refresh',exact:true}).focus();
 await page.keyboard.press('Enter');
 await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);
 assert.equal(await page.evaluate(()=>document.activeElement.dataset.durableFocus),'refresh');
 await page.getByRole('button',{name:'First',exact:true}).click();
 await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);
 assert.equal(await page.evaluate(()=>document.activeElement.dataset.durableFocus),'next');
 const ids=[];
 for(let n=0;n<6;n++){
  ids.push(...await page.locator('.durable-title').evaluateAll(els=>els.map(el=>el.dataset.durableFocus)));
  if(n<5){await page.getByRole('button',{name:'Next',exact:true}).click();await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);}
 }
 assert.equal(ids.length,105);assert.equal(new Set(ids).size,105);
 assert.equal(await page.getByRole('button',{name:'Next',exact:true}).isDisabled(),true);
 assert.equal(await page.evaluate(()=>document.activeElement.dataset.durableFocus),'previous');
 await page.getByRole('button',{name:'Previous',exact:true}).click();
 await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);
 assert.match(await page.locator('.durable-title').first().innerText(),/Film 81$/);
 await page.getByRole('button',{name:'First',exact:true}).click();
 await page.getByLabel('Durable jobs per page').selectOption('10');
 await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);
 assert.equal(await page.locator('.durable-title').count(),10);
 // Details open in a bounded dialog and polling preserves its selected section.
 await page.evaluate(()=>window.hold=true);
 await page.locator('.durable-title').first().click();
 const dialog=page.getByRole('dialog',{name:'Cluster work details'});
 assert.match(await dialog.innerText(),/Loading job details/);
 await page.evaluate(()=>{window.hold=false;window.release()});
 await dialog.getByRole('button',{name:'History',exact:true}).click();
 assert.match(await dialog.innerText(),/charged failures/);
 await page.evaluate(()=>refreshDurableActivity(true));
 assert.equal(await dialog.getByRole('button',{name:'History',exact:true}).getAttribute('aria-pressed'),'true');
 await dialog.getByRole('button',{name:'Close ×'}).click();
 assert.equal(await dialog.isVisible(),false);
 assert.equal(await page.evaluate(()=>document.activeElement.classList.contains('durable-title')),true);
 // Closing an in-flight detail must not reopen it when the response arrives.
 await page.evaluate(()=>window.hold=true);
 await page.locator('.durable-title').first().click();
 await page.keyboard.press('Escape');
 await page.evaluate(()=>{window.hold=false;window.release()});
 await page.evaluate(()=>new Promise(resolve=>setTimeout(resolve,0)));
 assert.equal(await dialog.isVisible(),false);
 // Failed detail reads can be retried without leaving the dialog.
 await page.evaluate(()=>window.fail=true);
 await page.locator('.durable-title').first().click();
 assert.match(await dialog.innerText(),/Could not load details: <offline>/);
 await page.evaluate(()=>window.fail=false);
 await dialog.getByRole('button',{name:'Try again'}).click();
 await dialog.getByRole('button',{name:'Stages',exact:true}).click();
 assert.match(await dialog.innerText(),/No fresh execution-stage observation/);
 await dialog.getByRole('button',{name:'Close ×'}).click();
 // Repair disclosure survives polling; repair details use the same inspector.
 await page.locator('[data-durable-focus="repairs"]').click();
 await page.getByRole('button',{name:'Inspect work'}).click();
 await dialog.getByRole('button',{name:'History',exact:true}).click();
 assert.match(await dialog.innerText(),/charged failures/);
 await dialog.getByRole('button',{name:'Close ×'}).click();
 await page.evaluate(()=>refreshDurableActivity(true));
 assert.equal(await page.locator('.durable-repairs').getAttribute('open'),'');
 await page.getByLabel('Durable job state').selectOption('failed');
 await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);
 assert.match(await page.locator('.durable-empty').innerText(),/No failed jobs/);
 await page.getByLabel('Durable job state').selectOption('queued');
 await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);
 await page.evaluate(()=>{window.fail=true;return refreshDurableActivity(true)});
 assert.match(await page.locator('.durable-body>.err').innerText(),/<offline>.*last observation/);
 assert.equal(await page.locator('.durable-title').count(),10);
 await page.evaluate(()=>{window.fail=false;return refreshDurableActivity(true)});
 await page.locator('.durable-title').first().click();
 for(const width of [1280,390]){
  await page.setViewportSize({width,height:1000});
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true,`viewport overflow at ${width}`);
  if(process.env.DURABLE_SCREENSHOTS)await page.screenshot({path:path.join(process.env.DURABLE_SCREENSHOTS,`durable-${width}.png`),fullPage:true});
 }
 assert.deepEqual(errors,[]);
 console.log('PASS durable work pagination, inspector, repair disclosure, refresh, errors and responsive layout');
 }finally{await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
