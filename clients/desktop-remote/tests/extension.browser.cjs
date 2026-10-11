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
  fs.writeFileSync(workerFile,fs.readFileSync(workerFile,"utf8").replace('if(typeof chrome!=="undefined")new DesktopWorker(chrome).start();',"globalThis.smokeWorker=new DesktopWorker(chrome);smokeWorker.start();"));
  let context;
  try{
    context=await chromium.launchPersistentContext(path.join(temp,"profile"),{headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE,args:["--disable-extensions-except="+extension,"--load-extension="+extension],viewport:{width:1280,height:800}});
    const worker=context.serviceWorkers()[0]||await context.waitForEvent("serviceworker",{timeout:15000});
    const page=await context.newPage(),errors=[];page.on("pageerror",error=>errors.push(error.message));
    await page.addInitScript(()=>{localStorage.setItem("plurx_token","fixture");localStorage.setItem("plurx_layout","ten-foot");localStorage.setItem("cinema.remote.preferences:"+JSON.stringify([location.origin,"desktop-smoke","1"]),JSON.stringify({cec:true}));});
    await page.route("**/*",route=>{
      const url=new URL(route.request().url()),p=url.pathname;
      if(url.hostname!=="cinema.test")return route.abort();
      if(p==="/"||p.startsWith("/assets/")){
        const file=path.join(web,p==="/"?"index.html":p.slice(8));
        return fs.existsSync(file)?route.fulfill({contentType:p==="/"?"text/html":p.endsWith(".css")?"text/css":"text/javascript",body:fs.readFileSync(file)}):route.fulfill({status:404,body:""});
      }
      const key=p.replace("/api/v1","");let data={};
      if(key==="/server")data={name:"Cinema",instance_id:"desktop-smoke",setup_required:false};
      if(key==="/me")data={id:1,username:"viewer",is_admin:false};
      if(key==="/libraries")data=[{id:1,name:"Movies",kind:"movies"}];
      if(key==="/home"||key==="/dvr/reminders/due")data=[];
      return route.fulfill({contentType:"application/json",body:JSON.stringify(data)});
    });
    await page.goto("http://cinema.test/#/");await page.bringToFront();await page.waitForFunction(()=>typeof CinemaRemote!=="undefined"&&ME&&document.getElementById("main")?.dataset.phase==="settled");
    await worker.evaluate(()=>{
      globalThis.smokePosts=[];globalThis.smokeNativeListeners=[];
      chrome.runtime.connectNative=()=>({postMessage:message=>smokePosts.push(message),disconnect:()=>{},onMessage:{addListener:fn=>smokeNativeListeners.push(fn)},onDisconnect:{addListener:()=>{}}});
    });
    // Exercise the production action popup. Playwright does not expose this
    // popup as a Page; use its actual CDP target, with native port mocked only.
    await worker.evaluate(()=>chrome.action.openPopup());
    const cdp=await context.newCDPSession(page);
    const targets=await cdp.send("Target.getTargets"),popup=targets.targetInfos.find(t=>t.url.endsWith("/popup.html"));assert.ok(popup);
    const {sessionId}=await cdp.send("Target.attachToTarget",{targetId:popup.targetId,flatten:false});
    let command=0;
    async function popupCall(method,params){
      const id=++command;
      const result=new Promise((resolve,reject)=>{
        const listener=event=>{if(event.sessionId!==sessionId)return;const message=JSON.parse(event.message);if(message.id!==id)return;cdp.off("Target.receivedMessageFromTarget",listener);message.error?reject(new Error(message.error.message)):resolve(message.result);};
        cdp.on("Target.receivedMessageFromTarget",listener);
      });
      await cdp.send("Target.sendMessageToTarget",{sessionId,message:JSON.stringify({id,method,params})});return result;
    }
    async function popupClick(id){
      const rect=await popupCall("Runtime.evaluate",{expression:`(()=>{const r=document.getElementById(${JSON.stringify(id)}).getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`,returnByValue:true});
      const {x,y}=rect.result.value;
      await popupCall("Input.dispatchMouseEvent",{type:"mousePressed",x,y,button:"left",clickCount:1});
      // Popup may close after release; this dispatch acknowledges independently.
      await popupCall("Input.dispatchMouseEvent",{type:"mouseReleased",x,y,button:"left",clickCount:1});
    }
    await popupClick("enabled");
    for(let i=0;i<20&&!await worker.evaluate(()=>smokeWorker.enabled);i++)await page.waitForTimeout(25);
    assert.equal(await worker.evaluate(()=>smokeWorker.enabled),true);
    await popupClick("bind");
    for(let i=0;i<40&&!await worker.evaluate(()=>!!smokeWorker.binding);i++)await page.waitForTimeout(25);
    const outcome=await worker.evaluate(()=>({bound:!!smokeWorker.binding,documentId:smokeWorker.binding?.documentId,credit:smokePosts.findLast(m=>m.type==="heartbeat")?.credit,epoch:smokeWorker.binding?.epoch}));
    assert.equal(await page.evaluate(()=>document.hasFocus()),true,"popup closure restores actual Cinema focus");
    assert.equal(outcome.bound,true);assert.ok(outcome.documentId);assert.ok(outcome.credit);
    await page.waitForFunction(()=>globalThis.CinemaDesktopBridge?.epoch);
    const before=await page.evaluate(()=>CinemaRemote.snapshot().context_revision);
    await worker.evaluate(({credit,epoch})=>smokeNativeListeners[0]({type:"input",key:"right",credit,epoch,sequence:1}),outcome);
    await page.waitForFunction(before=>CinemaRemote.snapshot().context_revision>before,before);
    assert.equal(await worker.evaluate(()=>smokeWorker.lastOutcome),"applied");
    // The actual extension/MAIN path reaches the separate physical pairing
    // owner. Only the synthetic receiver transport is injected; no hardware,
    // server approval, or browser permission-dialog claim is made here.
    await page.evaluate(()=>{
      cinemaRemoteReceiverSync=()=>{};CINEMA_WEB_RECEIVER?.retire("fixture");
      globalThis.pairRequests=[];const target={owner_node_id:"fixture",session_id:"00000000-0000-4000-8000-000000000001",receiver_epoch:"00000000-0000-4000-8000-000000000002"};
      CINEMA_WEB_RECEIVER={target,generation:1,pairings:[],challenge:null,eligible:()=>true,proof:()=>({}),fence:()=>{},client:{request:async(path,options)=>{pairRequests.push({path,body:options.body});return path==="pairing/start"?{target,challenge_id:"00000000-0000-4000-8000-000000000003",code:"12345678",expires_in_ms:120000,qr_modules:null}:{};}}};
      CinemaRemote.focusById("local:pair-entry");
    });
    async function freshCredit(){await worker.evaluate(async()=>{while(smokeWorker.probing)await new Promise(resolve=>setTimeout(resolve,5));await smokeWorker.probe();});return worker.evaluate(()=>({credit:smokePosts.findLast(message=>message.type==="heartbeat").credit,epoch:smokeWorker.binding.epoch}));}
    async function physicalSelect(sequence,offered){await worker.evaluate(({sequence,offered})=>{smokeWorker.lastOutcome=null;smokeNativeListeners[0]({type:"input",key:"select",sequence,...offered});},{sequence,offered});await page.waitForTimeout(25);for(let i=0;i<30&&!await worker.evaluate(()=>smokeWorker.lastOutcome);i++)await page.waitForTimeout(10);return worker.evaluate(()=>smokeWorker.lastOutcome);}
    assert.equal(await physicalSelect(2,await freshCredit()),"applied");assert.equal(await page.locator("#cinema-pair-dialog[open]").count(),1);
    await page.evaluate(()=>{CINEMA_WEB_RECEIVER.challenge={challenge_id:"00000000-0000-4000-8000-000000000006",deadline:performance.now()+10000};CINEMA_WEB_RECEIVER.pairings=[{pending_id:"00000000-0000-4000-8000-000000000004",controller_name:"Phone one"}];cinemaRemotePairingPrompt(CINEMA_WEB_RECEIVER);});
    assert.equal(await page.evaluate(()=>document.activeElement.dataset.localPair),"close");await page.locator('[data-local-pair="approve"]').focus();const oldPhone=await freshCredit();
    await page.evaluate(()=>{CINEMA_WEB_RECEIVER.pairings=[{pending_id:"00000000-0000-4000-8000-000000000005",controller_name:"Phone two"}];cinemaRemotePairingPrompt(CINEMA_WEB_RECEIVER);document.querySelector('[data-local-pair="approve"]').focus();});assert.equal(await physicalSelect(3,oldPhone),"stale_focus");
    const oldCode=await freshCredit();await page.evaluate(async()=>{await cinemaRemoteShowPairingOwner();CINEMA_WEB_RECEIVER.pairings=[{pending_id:"00000000-0000-4000-8000-000000000007",controller_name:"Phone three"}];cinemaRemotePairingPrompt(CINEMA_WEB_RECEIVER);document.querySelector('[data-local-pair="approve"]').focus();});assert.equal(await physicalSelect(4,oldCode),"stale_focus");assert.equal(await page.evaluate(()=>pairRequests.filter(request=>request.path==="pairing/approve").length),0);
    assert.equal(await physicalSelect(5,await freshCredit()),"applied");assert.equal(await page.evaluate(()=>pairRequests.find(request=>request.path==="pairing/approve").body.pending_id),"00000000-0000-4000-8000-000000000007");
    await page.reload();for(let i=0;i<30&&await worker.evaluate(()=>!!smokeWorker.binding);i++)await page.waitForTimeout(100);assert.equal(await worker.evaluate(()=>smokeWorker.binding),null);
    assert.equal(await page.evaluate(()=>!!globalThis.CinemaDesktopBridge),false);
    assert.deepEqual(errors,[]);
    console.log("real production popup/MAIN/document lifecycle PASS; native port injected; fixture permission pregranted");
  }finally{if(context)await context.close();fs.rmSync(temp,{recursive:true,force:true});}
});
