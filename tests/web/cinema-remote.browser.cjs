"use strict";
// Shipped assets in a real disposable browser. The receiver API is a bounded
// HTTP fixture here; the separately recorded live Router run proves B04 seams.
const {test}=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path");
const {cinemaRealFocusBrowser}=require("./cinema-remote-browser-support.cjs");
const web=path.resolve(__dirname,"../../crates/plurxd/src/web"),id="00000000-0000-4000-8000-000000000001",receiver={receiver_id:id,receiver_secret:"A".repeat(43),name:"TV"};
test("real same-profile tabs hand exclusive receiver ownership over without duplicate registration",async()=>{
  const runtime=await cinemaRealFocusBrowser(),context=runtime.context;const calls=[],errors=[];let sessions=0,offline=false;
  await context.addInitScript(({receiver})=>{localStorage.setItem("plurx_token","fixture");localStorage.setItem("plurx_layout","classic");const key=JSON.stringify([location.origin,"tab-fixture","1"]);if(!localStorage.getItem("cinema.remote.v1:"+key))localStorage.setItem("cinema.remote.v1:"+key,JSON.stringify({receiver,grants:[]}));if(!localStorage.getItem("cinema.remote.preferences:"+key))localStorage.setItem("cinema.remote.preferences:"+key,JSON.stringify({receiver:true,cec:false,companion:false}));},{receiver});
  await context.route("http://localhost:17862/**",async route=>{
    const url=new URL(route.request().url()),p=url.pathname;if(p==="/"||p.startsWith("/assets/")){const file=path.join(web,p==="/"?"index.html":p.slice(8));return fs.existsSync(file)?route.fulfill({contentType:p==="/"?"text/html":p.endsWith(".css")?"text/css":"text/javascript",body:fs.readFileSync(file)}):route.fulfill({status:404,body:""});}
    if(offline)return route.abort();let data={};const remote=p.startsWith("/api/remote/v1/");if(remote){const key=p.slice("/api/remote/v1/".length),body=route.request().postDataJSON();calls.push({key,body});data={version:"cinema.remote.v1"};
      if(key==="sessions"){sessions++;data.target={owner_node_id:"fixture",session_id:`00000000-0000-4000-8000-${String(sessions).padStart(12,"0")}`,receiver_epoch:id};}
      if(key==="presence"||key==="ack")data.accepted=true;
      if(key==="poll"){await new Promise(resolve=>setTimeout(resolve,250));Object.assign(data,{target:body.target,response_revision:1,delivery_id:0,control:null,commands:[],pairings:[]});}
      if(key==="receivers")data={version:"cinema.remote.v1",receivers:[],unavailable_nodes:[]};if(key==="grants")data={version:"cinema.remote.v1",grants:[]};
    }else{const key=p.replace("/api/v1","");if(key==="/server")data={name:"Cinema",instance_id:"tab-fixture",setup_required:false};if(key==="/me")data={id:"1",username:"Viewer",is_admin:false};if(key==="/libraries"||key==="/home")data=[];}
    await route.fulfill({contentType:"application/json",body:JSON.stringify(data)}).catch(()=>{});
  });
  try{
    const one=await context.newPage();await (await context.newCDPSession(one)).send("Emulation.setFocusEmulationEnabled",{enabled:false});one.on("pageerror",error=>errors.push(error.message));await one.goto("http://localhost:17862/#/");await one.bringToFront();await one.waitForFunction(()=>CINEMA_WEB_RECEIVER?.state==="available",null,{timeout:5000}).catch(async error=>{console.log(await one.evaluate(()=>({state:CINEMA_WEB_RECEIVER?.state,focus:document.hasFocus(),visible:document.visibilityState,me:!!ME,instance:SERVER?.instance_id,prefs:cinemaRemotePreferences(),main:document.getElementById("main")?.textContent})),errors,calls.map(v=>v.key));throw error;});assert.equal(sessions,1);
    const installation=await one.evaluate(()=>cinemaRemoteLoad(cinemaRemoteIdentity()).receiver.receiver_id);
    const two=await context.newPage();await (await context.newCDPSession(two)).send("Emulation.setFocusEmulationEnabled",{enabled:false});two.on("pageerror",error=>errors.push(error.message));await two.goto("http://localhost:17862/#/");await two.bringToFront();await two.waitForFunction(()=>CINEMA_WEB_RECEIVER?.state==="available",null,{timeout:5000}).catch(async error=>{console.log("two",await two.evaluate(()=>({state:CINEMA_WEB_RECEIVER?.state,focus:document.hasFocus(),lock:CINEMA_WEB_RECEIVER?.heldLock})),"one",await one.evaluate(()=>({state:CINEMA_WEB_RECEIVER?.state,focus:document.hasFocus(),lock:CINEMA_WEB_RECEIVER?.heldLock})),errors,calls.map(v=>v.key));throw error;});await one.waitForFunction(()=>!CINEMA_WEB_RECEIVER.heldLock);assert.equal(sessions,2);assert.equal(await two.evaluate(()=>cinemaRemoteLoad(cinemaRemoteIdentity()).receiver.receiver_id),installation);assert.equal(calls.filter(value=>value.key==="receivers"&&value.body).length,0);
    await one.bringToFront();await one.waitForFunction(()=>CINEMA_WEB_RECEIVER?.state==="available");await two.waitForFunction(()=>!CINEMA_WEB_RECEIVER.heldLock);assert.equal(sessions,3);
    await one.goto("http://localhost:17862/#/settings/developer");await one.waitForSelector("#dev-cinema-cec");assert.equal(await one.locator("#dev-cinema-cec").isChecked(),false);assert.equal(calls.some(value=>value.key==="settings"||value.key==="developer/readiness"),false);
    offline=true;await one.locator("#dev-cinema-cec").check();await one.locator("#dev-cinema-cec").locator("xpath=ancestor::div[contains(@class,'setcard')]").getByRole("button",{name:"Save",exact:true}).click();assert.equal(await one.evaluate(()=>cinemaRemoteLocalEnabled("cec")),true);assert.equal(await one.locator("#dev-cinema-cec").isEnabled(),true);
    assert.deepEqual(errors,[]);console.log("real WebLocks profile/origin ownership and offline non-admin Developer Save PASS; receiver HTTP authority fixture only");
  }finally{await runtime.close();}
});
