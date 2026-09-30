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
    return {jobs:rows.slice(0,100),next_cursor:rows.length>100?rows[99].id:null,counts:[{state:'queued',count:window.jobs.length}],observed_at_ms:Date.now(),repairs:[{id:'repair1',job_id:window.jobs[1].id,kind:'subtitle',target_node_id:'node',phase:'copying',age_ms:1000}]};
   }
   if(window.hold)await new Promise(resolve=>window.release=resolve);
   return {job:window.jobs.find(j=>url.endsWith(j.id)),attempts:[],waiters:[]};
  };
 });
 await page.addScriptTag({path:path.join(root,'pages/activity.js')});
 await page.evaluate(()=>refreshDurableActivity(true));
 assert.equal(await page.locator('.durable-title').count(),20);
 const ids=[];
 for(let n=0;n<6;n++){
  ids.push(...await page.locator('.durable-title').evaluateAll(els=>els.map(el=>el.dataset.durableFocus)));
  if(n<5){await page.getByRole('button',{name:'Next',exact:true}).click();await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);}
 }
 assert.equal(ids.length,105);assert.equal(new Set(ids).size,105);
 assert.equal(await page.getByRole('button',{name:'Next',exact:true}).isDisabled(),true);
 await page.getByRole('button',{name:'Previous',exact:true}).click();
 await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);
 assert.match(await page.locator('.durable-title').first().innerText(),/Film 81$/);
 await page.getByRole('button',{name:'First',exact:true}).click();
 await page.getByLabel('Durable jobs per page').selectOption('10');
 await page.waitForFunction(()=>!DURABLE_ACTIVITY.busy);
 assert.equal(await page.locator('.durable-title').count(),10);
 // A detail must load inside the selected item's next row, with immediate feedback.
 await page.evaluate(()=>window.hold=true);
 await page.locator('.durable-title').first().click();
 assert.match(await page.locator('.durable-title').first().locator('xpath=ancestor::tr/following-sibling::tr[1]').innerText(),/Loading job details/);
 await page.evaluate(()=>{window.hold=false;window.release()});
 await page.waitForSelector('.durable-detail button');
 assert.match(await page.locator('.durable-title').first().locator('xpath=ancestor::tr/following-sibling::tr[1]').innerText(),/charged failures/);
 await page.evaluate(()=>refreshDurableActivity(true));
 assert.equal(await page.locator('.durable-title').first().getAttribute('aria-expanded'),'true');
 await page.getByRole('button',{name:'Close details'}).click();
 assert.equal(await page.locator('.durable-detail').count(),0);
 assert.equal(await page.evaluate(()=>document.activeElement.classList.contains('durable-title')),true);
 // Closing an in-flight detail must not reopen it when the response arrives.
 await page.evaluate(()=>window.hold=true);
 await page.locator('.durable-title').first().click();
 await page.locator('.durable-title').first().click();
 await page.evaluate(()=>{window.hold=false;window.release()});
 await page.evaluate(()=>new Promise(resolve=>setTimeout(resolve,0)));
 assert.equal(await page.locator('.durable-detail').count(),0);
 // Failed detail reads stay beside the item and can be retried there.
 await page.evaluate(()=>window.fail=true);
 await page.locator('.durable-title').first().click();
 assert.match(await page.locator('.durable-title').first().locator('xpath=ancestor::tr/following-sibling::tr[1]').innerText(),/Could not load details: <offline>/);
 await page.evaluate(()=>window.fail=false);
 await page.getByRole('button',{name:'Try again'}).click();
 await page.getByRole('button',{name:'Close details'}).click();
 // Collapse, repair disclosure, selected page, and focus survive replacement markup.
 await page.locator('[data-durable-focus="repairs"]').click();
 await page.getByRole('button',{name:'Inspect work'}).click();
 assert.match(await page.locator('.durable-repairs tbody tr').nth(1).innerText(),/charged failures/);
 await page.locator('[data-durable-focus="section"]').click();
 await page.waitForFunction(()=>DURABLE_ACTIVITY.open===false);
 await page.evaluate(()=>refreshDurableActivity(true));
 assert.equal(await page.locator('.durable-queue').getAttribute('open'),null);
 await page.locator('[data-durable-focus="section"]').press('Enter');
 await page.waitForFunction(()=>DURABLE_ACTIVITY.open===true);
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
 console.log('PASS durable work pagination, inline details, folds, refresh, errors and responsive layout');
 }finally{await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
