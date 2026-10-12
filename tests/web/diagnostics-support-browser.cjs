"use strict";
// Real support binary + shipped JS + phone/inbox UI. No production authority.
// NODE_PATH may point at the host's installed Playwright runtime.
const {chromium}=require('playwright');
const fs=require('node:fs'),path=require('node:path'),os=require('node:os');
const {spawn,spawnSync}=require('node:child_process');
const {randomUUID}=require('node:crypto');
const assert=require('node:assert/strict');
const binary=process.argv[2];if(!binary)throw new Error('Pass the compiled plurx-support executable');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'cinema-support-acceptance-'));
const state=path.join(root,'state'),key=path.join(root,'key');
const support='http://localhost:18987',operators='http://localhost:18988';
function cli(...args){const result=spawnSync(binary,['--state-dir',state,'--key-file',key,...args],{encoding:'utf8'});assert.equal(result.status,0,result.stderr);}
async function request(route,body,grant,method='POST',extra={}){
 const response=await fetch(support+'/support/v1/'+route,{method,headers:{...(body&&!Buffer.isBuffer(body)?{'Content-Type':'application/json'}:{}),...(grant?{Authorization:'Bearer '+grant}:{}),...extra},body:body?Buffer.isBuffer(body)?body:JSON.stringify(body):undefined});
 const value=response.status===204?{}:await response.json();assert(response.ok,route+': '+response.status+' '+value.error);return value;
}
let service,browser;
(async()=>{
 cli('init-key');cli('publisher','--id','fixture','--credential-out',path.join(root,'publisher'),'--origin',support);
 cli('operator','--name','fixture','--token-out',path.join(root,'operator'));
 service=spawn(binary,['--state-dir',state,'--key-file',key,'serve','--support-origin',support,'--operator-origin',operators,'--ingress','127.0.0.1:18987','--operator','127.0.0.1:18988','--development-local'],{stdio:['ignore','ignore',fs.openSync(path.join(root,'service.log'),'a')]});
 for(let i=0;i<50;i++){if(service.exitCode!==null)throw new Error('Support startup failed: '+fs.readFileSync(path.join(root,'service.log'),'utf8'));try{if((await fetch(support+'/healthz')).ok)break}catch{}await new Promise(r=>setTimeout(r,100));}
 browser=await chromium.launch({executablePath:process.env.CHROME_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true});
 const tv=await browser.newContext({bypassCSP:true}),tvPage=await tv.newPage();await tvPage.goto(support);
 for(const file of ['wire','hash','store','recorder','export','delivery'])await tvPage.addScriptTag({path:path.resolve('crates/plurxd/src/web/core/diagnostics-'+file+'.js')});
 const report=await tvPage.evaluate(async id=>{
  const capture=CinemaDiagnostics.freeze();CinemaDiagnostics.record('network','error',{failure:'tls',url:'https://CANARY_PRIVATE/?token=CANARY_SECRET',message:'CANARY_USER'});
  const events=CinemaDiagnostics.freeze().events;
  const manifest={version:CinemaDiagnosticWire.version,report_id:id,captured_at:capture.captured_at,category:'connection',scope:'anonymous',client:{platform:'browser_tv',app_version:'fixture',build:'fixture',os_version:'browser',device_model:'browser'},capture:{mode:'ordinary',window_ms:600000,dropped_events:'0',persistence:'memory_only'},components:[{name:'client',state:'complete'},{name:'server',state:'unavailable',reason:'context_unavailable'},{name:'platform',state:'unavailable',reason:'unsupported'}],privacy:{titles_included:false,attachments_included:false},files:[]};
  const archive=await CinemaDiagnosticExport.archive(manifest,{'description.txt':'TV incident','client-events.jsonl':events.map(e=>JSON.stringify(e)).join('\n')+'\n'});
  const bytes=Array.from(new Uint8Array(await archive.blob.arrayBuffer()));
  if(new TextDecoder().decode(new Uint8Array(bytes)).includes('CANARY'))throw new Error('Automatic canary leaked');
  return{bytes,binding:{report_id:id,length:bytes.length,sha256:await CinemaDiagnosticExport.digest(archive.blob),privacy:manifest.privacy}};
 },randomUUID());
 const publisher=fs.readFileSync(path.join(root,'publisher'),'utf8').trim();
 const grant=await request('grants',report.binding,publisher,'POST',{'X-Publisher-Id':'fixture'});
 const offer=await tvPage.evaluate(async data=>{
  window.acceptanceState={origin:location.origin,binding:data.binding,archive:new Blob([new Uint8Array(data.bytes)]),upload_grant:data.grant.upload_grant,grant_expires:Date.now()+data.grant.expires_in_ms,state:'local',consented:true};
  window.acceptanceController=new AbortController();window.acceptanceDelivery=createCinemaDiagnosticDelivery({state:window.acceptanceState,persist:async()=>{},current:()=>true,signal:window.acceptanceController.signal});
  await window.acceptanceDelivery.stage();return window.acceptanceDelivery.offer();
 },{...report,grant});
 assert(offer.manual_code&&offer.url);
 const phoneContext=await browser.newContext({viewport:{width:390,height:844}}),phone=await phoneContext.newPage();await phone.goto(offer.url);
 await phone.locator('#comparison').waitFor({state:'visible'});assert.equal(await phone.locator('#report').isVisible(),false);assert.equal(new URL(phone.url()).hash,'');
 const secondContext=await browser.newContext(),second=await secondContext.newPage();await second.goto(support);await second.fill('#code',offer.manual_code);await second.locator('#claim button').click();await second.waitForFunction(()=>document.getElementById('status').textContent.includes('Another phone'));assert.equal(await second.locator('#report').isVisible(),false);
 const phrase=(await phone.locator('#phrase').textContent()).trim();
 const creator=await tvPage.evaluate(()=>window.acceptanceState.management_grant);
 const status=await tvPage.evaluate(()=>window.acceptanceDelivery.handoffStatus());assert.equal(status.comparison,phrase);
 await tvPage.evaluate(phrase=>window.acceptanceDelivery.approve(phrase),phrase);await tv.close();
 await phone.locator('#report').waitFor({state:'visible'});await phone.fill('#description','Explicit user description <img src=x onerror=alert(1)>');await phone.locator('#submit button').click();await phone.locator('#done').waitFor({state:'visible'});const receipt=await phone.locator('#receipt').textContent();assert(receipt.includes('C-'));const completion=await request('handoffs/'+offer.id+'/status',null,creator,'GET');assert.equal(completion.state,'submitted');assert(receipt.includes(completion.reference));
 const operatorContext=await browser.newContext({acceptDownloads:true}),inbox=await operatorContext.newPage();const cdp=await operatorContext.newCDPSession(inbox);await cdp.send('WebAuthn.enable');await cdp.send('WebAuthn.addVirtualAuthenticator',{options:{protocol:'ctap2',transport:'internal',hasResidentKey:true,hasUserVerification:true,isUserVerified:true,automaticPresenceSimulation:true}});
 const authResponses=[];inbox.on('response',r=>{if(r.url().includes('/operator/v1/'))authResponses.push({path:new URL(r.url()).pathname,status:r.status()})});
 await inbox.goto(operators);await inbox.locator('summary').filter({hasText:'Enroll a new'}).click();await inbox.fill('#token',fs.readFileSync(path.join(root,'operator'),'utf8').trim());await inbox.locator('#enroll button').click();await inbox.waitForFunction(()=>document.getElementById('status').textContent.includes('Passkey enrolled'));
 await inbox.fill('#name','fixture');await inbox.locator('#login button').click();try{await inbox.locator('#inbox').waitFor({state:'visible',timeout:8000})}catch(error){console.error(JSON.stringify({authResponses,status:await inbox.locator('#status').textContent(),cookieNames:(await operatorContext.cookies()).map(c=>c.name)}));throw error;}await inbox.locator('#rows button').first().click();await inbox.locator('#detail').waitFor({state:'visible'});
 assert((await inbox.locator('#description').textContent()).includes('<img src=x'));assert.equal(await inbox.locator('#description img').count(),0);
 const downloadPromise=inbox.waitForEvent('download');await inbox.click('#download');const download=await downloadPromise;const downloaded=await download.path();const zip=fs.readFileSync(downloaded);assert.equal(zip.readUInt32LE(0),0x04034b50);assert(!zip.includes(Buffer.from('CANARY')));let offset=0,description;while(zip.readUInt32LE(offset)===0x04034b50){const method=zip.readUInt16LE(offset+8),length=zip.readUInt32LE(offset+18),nameLength=zip.readUInt16LE(offset+26),extra=zip.readUInt16LE(offset+28),name=zip.subarray(offset+30,offset+30+nameLength).toString(),start=offset+30+nameLength+extra,content=zip.subarray(start,start+length);if(name==='description.txt')description=(method===0?content:require('node:zlib').inflateRawSync(content)).toString();offset=start+length;}assert.equal(description,'Explicit user description <img src=x onerror=alert(1)>');
 const foreign=await fetch(support+'/operator/v1/reports');assert.equal(foreign.status,404);
 const unauth=await fetch(operators+'/operator/v1/reports');assert.equal(unauth.status,401);
 inbox.on('dialog',dialog=>dialog.accept());await inbox.click('#delete');await inbox.waitForFunction(()=>document.getElementById('status').textContent==='Report deleted.');assert.equal(await inbox.locator('#rows button').count(),0);
 for(const name of ['deletions.journal','revocations.journal']){const original=path.join(state,name),missing=original+'.lost';fs.renameSync(original,missing);const refused=spawnSync(binary,['--state-dir',state,'--key-file',key,'reconcile'],{encoding:'utf8'});assert.notEqual(refused.status,0,'Missing external journal must refuse an existing namespace');fs.renameSync(missing,original);}
 console.log(JSON.stringify({result:'passed',checks:['real shipped web export/upload','closed automatic canaries','two-phone claim denial','hidden until local creator approval','phone finish after TV disconnect','real WebAuthn enrollment/login','operator escaped user content','verified ZIP download with final edited description','separate listener authorization','operator deletion','missing external fence journals refuse restore'],receipt,source:process.env.DIAGNOSTIC_SOURCE||'working-tree',limits:'Software browser fixture; native/hardware/provider delivery not exercised.'}));
})().catch(error=>{console.error(error.stack);process.exitCode=1}).finally(async()=>{if(browser)await browser.close();if(service){service.kill('SIGINT');await new Promise(resolve=>{service.once('exit',resolve);setTimeout(()=>{service.kill('SIGKILL');resolve()},2000).unref()})}fs.rmSync(root,{recursive:true,force:true})});
