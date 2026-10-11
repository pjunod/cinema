"use strict";
// Start a disposable native Chromium, then attach with noDefaults. Playwright's
// normal launcher forces every document focused; a second CDP session cannot
// undo that first session's override. Production focus eligibility is unchanged.
const fs=require("node:fs"),os=require("node:os"),path=require("node:path"),{spawn}=require("node:child_process");
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||"playwright");
async function cinemaRealFocusBrowser(){
  const profile=fs.mkdtempSync(path.join(os.tmpdir(),"cinema-remote-profile-"));
  const process=spawn(globalThis.process.env.CHROMIUM_EXECUTABLE||chromium.executablePath(),["--user-data-dir="+profile,"--remote-debugging-port=0","--no-first-run","--no-default-browser-check","about:blank"],{stdio:"ignore"});
  let browser;
  try{
    const deadline=Date.now()+15000,file=path.join(profile,"DevToolsActivePort");while(!fs.existsSync(file)){if(Date.now()>deadline||process.exitCode!==null)throw new Error("Disposable Chromium did not start");await new Promise(resolve=>setTimeout(resolve,100));}
    const port=Number(fs.readFileSync(file,"utf8").split("\n")[0]);if(!Number.isInteger(port)||port<1||port>65535)throw new Error("Invalid Chromium debugging port");
    browser=await chromium.connectOverCDP("http://127.0.0.1:"+port,{noDefaults:true});const context=browser.contexts()[0];
    return {browser,context,close:async()=>{await browser.close().catch(()=>{});process.kill("SIGTERM");await new Promise(resolve=>{if(process.exitCode!==null)return resolve();process.once("exit",resolve);setTimeout(()=>{process.kill("SIGKILL");resolve();},2000).unref();});fs.rmSync(profile,{recursive:true,force:true});}};
  }catch(error){await browser?.close().catch(()=>{});process.kill("SIGTERM");fs.rmSync(profile,{recursive:true,force:true});throw error;}
}
module.exports={cinemaRealFocusBrowser};
