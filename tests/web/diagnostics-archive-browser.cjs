"use strict";
// Actual shipped report dialog and downloaded ZIP, using disposable fixture
// authority. NODE_PATH supplies Playwright; no Cinema/support service is used.
const {chromium}=require('playwright');
const {createServer}=require('node:http');
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const {createHash}=require('node:crypto');
const requests=[];
const scripts=['core/diagnostics-wire.js','core/diagnostics-hash.js','core/diagnostics-store.js','core/diagnostics-recorder.js','core/diagnostics-export.js','pages/diagnostics.js'];
const root=path.resolve('crates/plurxd/src/web');
const server=createServer((req,res)=>{
  requests.push(req.url);
  const asset=scripts.find(file=>req.url==='/'+file);
  if(asset){res.setHeader('Content-Type','text/javascript');res.end(fs.readFileSync(path.join(root,asset)));return;}
  if(req.url==='/api/v1/diagnostics/capabilities'){
    res.setHeader('Content-Type','application/json');res.end(JSON.stringify({server_time_ms:Date.now(),support:{configured:true,origin:'https://support.example'}}));return;
  }
  if(req.url.startsWith('/api/')){res.writeHead(503,{'Content-Type':'application/json'});res.end('{"error":"offline"}');return;}
  res.setHeader('Content-Type','text/html');res.end('<!doctype html><body><button id="open" onclick="CinemaOpenDiagnostics()">Report a problem</button><script>let TOKEN=null,ME=null,AUTH_GENERATION=1,SERVER={version:"fixture",build:"fixture",instance_id:"archive-fixture"};function layoutId(){return "catalog"}function browserLabel(){return "browser"}async function api(p,o={}){const r=await fetch("/api/v1"+p,{...o,body:o.body?JSON.stringify(o.body):undefined});if(!r.ok)throw new Error("Offline");return r.json()}</script>'+scripts.map(file=>'<script src="/'+file+'"></script>').join(''));
});
function unzip(bytes){
  const files=new Map();let offset=0;
  while(bytes.readUInt32LE(offset)===0x04034b50){
    assert.equal(bytes.readUInt16LE(offset+8),0);const length=bytes.readUInt32LE(offset+18),nameLength=bytes.readUInt16LE(offset+26),extra=bytes.readUInt16LE(offset+28);
    const name=bytes.subarray(offset+30,offset+30+nameLength).toString(),start=offset+30+nameLength+extra;
    assert(!files.has(name));assert(!name.includes('..'));files.set(name,bytes.subarray(start,start+length));offset=start+length;
  }
  assert.equal(bytes.readUInt32LE(offset),0x02014b50);return files;
}
let browser;
(async()=>{
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  browser=await chromium.launch({executablePath:process.env.CHROME_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true});
  const results=[];
  for(const signedIn of [false,true]){
    const context=await browser.newContext({acceptDownloads:true}),page=await context.newPage();const errors=[];
    page.on('pageerror',error=>errors.push(error.message));
    await page.goto('http://127.0.0.1:'+server.address().port);
    if(signedIn)await page.evaluate(async()=>{TOKEN='disposable-fixture';ME={id:4,is_admin:false};await diagnosticProvisionForCurrentAccount()});
    await page.evaluate(()=>CinemaDiagnostics.record('network','error',{failure:'tls',url:'https://CANARY_ADDRESS/?token=CANARY_SECRET',message:'CANARY_PRIVATE'}));
    await page.click('#open');await page.locator('[data-diagnostic="export"]').waitFor();
    assert.equal(await page.getByText('Send report',{exact:true}).count(),0);
    assert.equal(await page.locator('[data-diagnostic="origin"]').count(),0);
    await page.fill('[data-diagnostic="description"]','Cannot sign in; expected the library');
    const identity=await page.evaluate(()=>DIAGNOSTIC_REPORT.capture.id);
    const downloadPromise=page.waitForEvent('download');await page.click('[data-diagnostic="export"]');
    const download=await downloadPromise;assert.equal(download.suggestedFilename(),'cinema-report-'+identity+'.zip');
    const bytes=fs.readFileSync(await download.path()),files=unzip(bytes),manifest=JSON.parse(files.get('manifest.json'));
    assert.equal(manifest.report_id,identity);assert.equal(manifest.scope,signedIn?'viewer':'anonymous');
    assert.equal(files.get('description.txt').toString(),'Cannot sign in; expected the library');
    assert(files.get('summary.txt').toString().includes(identity));assert(files.has('client-events.jsonl'));
    assert(!bytes.includes(Buffer.from('CANARY')));assert.equal(manifest.privacy.titles_included,false);
    for(const file of manifest.files){const content=files.get(file.name);assert(content);assert.equal(content.length,file.size_bytes);assert.equal(createHash('sha256').update(content).digest('hex'),file.sha256);}
    assert.deepEqual(errors,[]);results.push({scope:manifest.scope,report:identity,bytes:bytes.length,entries:files.size});await context.close();
  }
  assert(!requests.some(url=>/enrollments|support\/v1|uploads|handoffs/.test(url)));
  console.log(JSON.stringify({result:'passed',checks:['shipped report dialog','anonymous offline export','signed-in export without enrollment','actual ZIP download','selected incident identity','readable summary and manifest hashes','private canaries excluded','no support requests'],results}));
})().catch(error=>{console.error(error.stack);process.exitCode=1}).finally(async()=>{if(browser)await browser.close();await new Promise(resolve=>server.close(resolve));});
