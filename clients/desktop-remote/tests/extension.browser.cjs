// SPDX-License-Identifier: Apache-2.0
// One disposable-profile smoke. Hardware/native port is explicitly injected.
const {test}=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),os=require("node:os"),path=require("node:path");
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
const root=path.resolve(__dirname,"../../.."),web=path.join(root,"crates/plurxd/src/web");
test("real Chromium extension targets MAIN document and invalidates local context without native hardware",async()=>{
  const temp=fs.mkdtempSync(path.join(os.tmpdir(),"cinema-cec-browser-")),extension=path.join(temp,"extension");
  fs.cpSync(path.join(__dirname,"../extension"),extension,{recursive:true});
  // Grant only fixture origin in the disposable copy. Production requests it
  // through the popup; browser permission prompts remain separate acceptance.
  const manifest=JSON.parse(fs.readFileSync(path.join(extension,"manifest.json")));manifest.host_permissions=["http://cinema.test/*"];
  fs.writeFileSync(path.join(extension,"manifest.json"),JSON.stringify(manifest));
  const workerFile=path.join(extension,"worker.js");
  fs.writeFileSync(workerFile,fs.readFileSync(workerFile,"utf8").replace('if(typeof chrome!=="undefined")new DesktopWorker(chrome).start();',"globalThis.SmokeDesktopWorker=DesktopWorker;"));
  let context;
  try{
    context=await chromium.launchPersistentContext(path.join(temp,"profile"),{headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE,args:["--disable-extensions-except="+extension,"--load-extension="+extension],viewport:{width:1280,height:800}});
    const worker=context.serviceWorkers()[0]||await context.waitForEvent("serviceworker",{timeout:15000});
    const page=await context.newPage(),errors=[];page.on("pageerror",error=>errors.push(error.message));
    await page.addInitScript(()=>{localStorage.setItem("plurx_token","fixture");localStorage.setItem("plurx_layout","ten-foot");});
    await page.route("**/*",route=>{
      const url=new URL(route.request().url()),p=url.pathname;
      if(url.hostname!=="cinema.test")return route.abort();
      if(p==="/"||p.startsWith("/assets/")){
        const file=path.join(web,p==="/"?"index.html":p.slice(8));
        return fs.existsSync(file)?route.fulfill({contentType:p==="/"?"text/html":p.endsWith(".css")?"text/css":"text/javascript",body:fs.readFileSync(file)}):route.fulfill({status:404,body:""});
      }
      const key=p.replace("/api/v1","");let data={};
      if(key==="/server")data={name:"Cinema",setup_required:false};
      if(key==="/me")data={id:1,username:"viewer",is_admin:false};
      if(key==="/libraries")data=[{id:1,name:"Movies",kind:"movies"}];
      if(key==="/home"||key==="/dvr/reminders/due")data=[];
      return route.fulfill({contentType:"application/json",body:JSON.stringify(data)});
    });
    await page.goto("http://cinema.test/#/");await page.waitForFunction(()=>typeof CinemaRemote!=="undefined"&&ME&&document.getElementById("main")?.dataset.phase==="settled");
    const outcome=await worker.evaluate(async()=>{
      const DesktopWorker=globalThis.SmokeDesktopWorker;
      const listeners=[],posts=[];
      const fake={postMessage:message=>posts.push(message),disconnect:()=>{},onMessage:{addListener:fn=>listeners.push(fn)},onDisconnect:{addListener:()=>{}}};
      const browser={...chrome,runtime:{...chrome.runtime,connectNative:()=>fake}};
      const w=new DesktopWorker(browser);await chrome.storage.local.set({enabled:true});w.start();await Promise.resolve();w.enabled=true;
      const tabs=await chrome.tabs.query({url:"http://cinema.test/*"});await w.bind(tabs[0]);
      globalThis.smokeWorker=w;globalThis.smokePosts=posts;
      return {bound:!!w.binding,documentId:w.binding.documentId,credit:posts.findLast(m=>m.type==="heartbeat")?.credit,epoch:w.binding.epoch};
    });
    assert.equal(outcome.bound,true);assert.ok(outcome.documentId);assert.ok(outcome.credit);
    await page.waitForFunction(()=>globalThis.CinemaDesktopBridge?.epoch);
    const before=await page.evaluate(()=>CinemaRemote.snapshot().context_revision);
    await worker.evaluate(({credit,epoch})=>smokeWorker.receive({type:"input",key:"right",credit,epoch,sequence:1}),outcome);
    await page.waitForFunction(before=>CinemaRemote.snapshot().context_revision>before,before);
    assert.equal(await worker.evaluate(()=>smokeWorker.lastOutcome),"applied");
    await page.reload();for(let i=0;i<30&&await worker.evaluate(()=>!!smokeWorker.binding);i++)await page.waitForTimeout(100);assert.equal(await worker.evaluate(()=>smokeWorker.binding),null);
    assert.equal(await page.evaluate(()=>!!globalThis.CinemaDesktopBridge),false);
    assert.deepEqual(errors,[]);
    console.log("real extension/MAIN/document lifecycle PASS; native port injected; fixture permission pregranted");
  }finally{if(context)await context.close();fs.rmSync(temp,{recursive:true,force:true});}
});
