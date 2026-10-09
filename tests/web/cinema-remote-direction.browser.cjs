"use strict";
const {test}=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path");
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
test("real browser directional pointer and keyboard clicks do not duplicate gestures",async()=>{
  const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_EXECUTABLE});
  try{
    const page=await browser.newPage();await page.setContent('<button id="direction" data-remote-direction="right">Right</button>');
    await page.addScriptTag({content:'const directionCalls=[];let CINEMA_WEB_CONTROLLER={hold:direction=>directionCalls.push({type:"hold",direction}),stopHold:()=>{},send:async action=>directionCalls.push({type:"send",action})};function cinemaRemoteIdentity(){return null;}function cinemaRemoteReceiverSync(){}'});
    await page.addScriptTag({content:fs.readFileSync(path.resolve(__dirname,"../../crates/plurxd/src/web/pages/remote.js"),"utf8")});await page.evaluate(()=>cinemaRemoteWireDirection(document.getElementById("direction")));
    await page.locator("#direction").click();await page.locator("#direction").focus();await page.keyboard.press("Enter");await page.keyboard.press("Space");await page.evaluate(()=>document.getElementById("direction").click());
    const calls=await page.evaluate(()=>directionCalls);assert.equal(calls.filter(value=>value.type==="hold").length,3);assert.equal(calls.filter(value=>value.type==="send").length,0);assert.ok(calls.every(value=>value.direction==="right"));
  }finally{await browser.close();}
});
