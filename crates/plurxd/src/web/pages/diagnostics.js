"use strict";

let DIAGNOSTIC_ENROLLMENT_CONTROLLER=null, DIAGNOSTIC_INSTALLATION=null;
let DIAGNOSTIC_REPORT=null, DIAGNOSTIC_AUTH_SERIAL=0, DIAGNOSTIC_AUTH_READY=Promise.resolve();
async function diagnosticBindAccount() {
  const serial=++DIAGNOSTIC_AUTH_SERIAL,token=TOKEN;
  const identity=token&&ME&&SERVER?.instance_id?location.origin+'\n'+SERVER.instance_id+'\n'+String(ME.id):null;
  const bytes=new TextEncoder().encode(identity||'anonymous');
  const scope=identity?Array.from(new Uint8Array(await cinemaDiagnosticSHA256(bytes)),v=>v.toString(16).padStart(2,'0')).join(''):'anonymous';
  if(serial!==DIAGNOSTIC_AUTH_SERIAL||token!==TOKEN)return;
  if(DIAGNOSTIC_REPORT && DIAGNOSTIC_REPORT.token!==token) closeDiagnostics(true);
  await CinemaDiagnostics.bind(scope);
}
function diagnosticAuthChanged(){DIAGNOSTIC_ENROLLMENT_CONTROLLER?.abort();DIAGNOSTIC_ENROLLMENT_CONTROLLER=null;DIAGNOSTIC_AUTH_READY=diagnosticBindAccount().catch(()=>{});}
function diagnosticInstallation(){if(DIAGNOSTIC_INSTALLATION)return DIAGNOSTIC_INSTALLATION;try{const saved=localStorage.getItem('cinema_diagnostic_installation');if(CinemaDiagnosticWire.uuid(saved))return DIAGNOSTIC_INSTALLATION=saved;}catch(_){}DIAGNOSTIC_INSTALLATION=CinemaDiagnostics.uuid();try{localStorage.setItem('cinema_diagnostic_installation',DIAGNOSTIC_INSTALLATION);}catch(_){}return DIAGNOSTIC_INSTALLATION;}
async function diagnosticProvisionForCurrentAccount(){
  if(!TOKEN||!ME)return;const token=TOKEN,generation=AUTH_GENERATION;diagnosticAuthChanged();await DIAGNOSTIC_AUTH_READY;if(token!==TOKEN||generation!==AUTH_GENERATION)return;
  const scope=CinemaDiagnostics.current().scope,key='enrollment:'+scope;const cached=await CinemaDiagnosticStore.get(key);if(cached&&cached.expires>Date.now()+60000)return;
  const controller=new AbortController();DIAGNOSTIC_ENROLLMENT_CONTROLLER?.abort();DIAGNOSTIC_ENROLLMENT_CONTROLLER=controller;const timer=setTimeout(()=>controller.abort(),10000);
  try{const caps=await api('/diagnostics/capabilities',{signal:controller.signal});if(!caps.support?.configured)return;const value=await api('/diagnostics/enrollments',{method:'POST',body:{installation_id:diagnosticInstallation()},signal:controller.signal});if(token!==TOKEN||generation!==AUTH_GENERATION||controller.signal.aborted)return;
    if(!/^[a-f0-9]{64}$/.test(value.authorization?.enrollment)||typeof value.origin!=='string')return;await CinemaDiagnosticStore.put({key,type:'enrollment',at:Date.now(),expires:Date.now()+Math.min(86400000,value.authorization.expires_in_ms),bytes:512,origin:value.origin,enrollment:value.authorization.enrollment});
  }catch(_){}finally{clearTimeout(timer);if(DIAGNOSTIC_ENROLLMENT_CONTROLLER===controller)DIAGNOSTIC_ENROLLMENT_CONTROLLER=null;}
}
function diagnosticRevokeEnrollment(token){
  if(!token)return;const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),5000);
  fetch('/api/v1/diagnostics/enrollments',{method:'DELETE',headers:{Authorization:'Bearer '+token,'Content-Type':'application/json'},body:JSON.stringify({installation_id:diagnosticInstallation()}),credentials:'omit',redirect:'error',signal:controller.signal}).catch(()=>{}).finally(()=>clearTimeout(timer));
}
function diagnosticCurrent(run){return DIAGNOSTIC_REPORT===run&&!run.controller.signal.aborted&&run.generation===AUTH_GENERATION&&run.token===TOKEN;}
function diagnosticMessage(run,message){if(diagnosticCurrent(run))run.root.querySelector('[data-diagnostic-status]').textContent=message;}
function diagnosticElement(run,name){return run.root.querySelector('[data-diagnostic="'+name+'"]');}
function diagnosticPlayback() {
  let p;try{p=PLAYER;}catch(_){return{};}const video=/** @type {HTMLVideoElement} */(document.getElementById('video'));if(!p||!video||!TOKEN||!ME||!CinemaDiagnostics.owns(p.mediaAttachment,AUTH_GENERATION))return{};
  const facts=CinemaDiagnostics.facts({correlation:CinemaDiagnostics.correlation(p.mediaAttachment,AUTH_GENERATION),mode:p.method||'unknown',playhead_ms:Math.max(0,Math.round((video.currentTime||0)*1000)),width:video.videoWidth||0,height:video.videoHeight||0,state:video.paused?'paused':video.readyState>=3?'playing':'waiting'});
  const source=p.source||{};const codec=value=>['h264','hevc','av1','vp9','aac','ac3','eac3','dts','truehd','opus','flac','pcm'].includes(value)?value:'unknown';
  facts.source_codec=codec(source.video_codec);facts.delivered_codec='unknown';facts.render_range='unknown';
  return{facts,title:typeof p.title==='string'?Array.from(p.title).slice(0,256).join('').replace(/[\x00-\x1f\x7f-\x9f]/g,''):null};
}
async function showDiagnostics() {
  if(DIAGNOSTIC_REPORT)return;
  if(typeof closeMenus==='function')closeMenus();
  await DIAGNOSTIC_AUTH_READY;
  const root=document.createElement('div');root.id='diagnostic-dialog';root.className='connectdialog diagnostic-dialog';root.setAttribute('role','dialog');root.setAttribute('aria-modal','true');root.setAttribute('aria-labelledby','diagnostic-heading');
  const active=CinemaDiagnostics.current();const recovered=await CinemaDiagnosticStore.get('reproduction:'+active.scope);const capture=active.reproduction?.capture||active.completed||(recovered?.expires>Date.now()&&recovered.capture?.scope===active.scope?{...recovered.capture,epoch:active.epoch}:CinemaDiagnostics.freeze()),playback=diagnosticPlayback();
  if(Object.keys(playback.facts||{}).length){const row=structuredClone(capture.events[capture.events.length-1]||{run_id:CinemaDiagnostics.uuid(),sequence:'0',utc:capture.captured_at,elapsed_ms:0,domain:'playback',kind:'sample',facts:{}});row.run_id=CinemaDiagnostics.uuid();row.sequence='0';row.domain='playback';row.kind='sample';row.facts=playback.facts;capture.events.push(row);}
  const run={root,capture,playback,generation:AUTH_GENERATION,token:TOKEN,controller:new AbortController(),opener:document.activeElement,previousOverflow:document.body.style.overflow,busy:false,server:null,connection:[],delivery:null,deliveryState:null,pollTimer:null,qrUrl:null,reproduction:null};DIAGNOSTIC_REPORT=run;
  root.innerHTML=`<div class="card diagnostic-card"><button type="button" class="connectx" data-diagnostic="close" aria-label="Close report">✕</button><h2 id="diagnostic-heading">Report a problem</h2>
    <p>Your recent diagnostic window was frozen before this form opened. Automatic evidence excludes credentials, addresses, request bodies and raw error text. Titles are optional; your description is included explicitly. Browser crash stacks and DNS/TLS details are unavailable.</p>
    <p data-diagnostic-status role="status" aria-live="polite">Local capture ready. Server evidence is being collected if signed in.</p>
    <label>Category <select data-diagnostic="category"><option value="playback">Playback</option><option value="connection">Connection</option><option value="connection">Sign-in</option><option value="interface">Interface</option><option value="interface">Crash or restart</option><option value="other">Other</option></select></label>
    <label>Description <textarea data-diagnostic="description" maxlength="4000" rows="4" placeholder="What happened, and what did you expect?"></textarea></label>
    <label class="row"><input type="checkbox" data-diagnostic="titles" style="width:auto"> Include the selected title</label>
    <p class="hint" data-diagnostic="coverage"></p><div class="row diagnostic-actions"><button type="button" data-diagnostic="export">Save report file</button><button type="button" data-diagnostic="send">Send report</button><button type="button" data-diagnostic="phone">Continue on phone</button></div>
    <details><summary>Reproduce and check connection</summary><p>Reproduction records at most five minutes. Checks observe your current connection for at most fifteen seconds; they do not repair playback.</p><div class="row"><button type="button" data-diagnostic="reproduce">Start reproduction</button><button type="button" data-diagnostic="stop" hidden>Stop and review</button><button type="button" data-diagnostic="check">Run connection checks</button><button type="button" data-diagnostic="system">Collect administrator system snapshot</button></div></details>
    <details><summary>Support destination and disconnected sending</summary><p>Upload goes to an independent support service. A cached installation enrollment can send only reports and expires within 24 hours. It cannot read your server. A new offline installation needs an operator-issued invitation.</p><label>HTTPS support origin <input data-diagnostic="origin" type="url" placeholder="https://support.example"></label><label>One-use support invitation <input data-diagnostic="invitation" autocomplete="off" maxlength="10"></label><button type="button" data-diagnostic="redeem">Use invitation</button></details>
    <section data-diagnostic="handoff" hidden><h3>Finish on your phone</h3><img data-diagnostic="qr" alt="Scan to claim this report" hidden><p data-diagnostic="code"></p><p data-diagnostic="comparison"></p><div class="row"><button type="button" data-diagnostic="approve" hidden>Approve this phone</button><button type="button" data-diagnostic="reject">Reject phone, keep draft</button><button type="button" data-diagnostic="cancel">Cancel and delete staged draft</button></div><p>Approval must be performed on this screen. A paired network remote cannot approve.</p></section>
    <details><summary>Local diagnostic privacy</summary><label class="row"><input type="checkbox" data-diagnostic="history" style="width:auto"> Keep bounded recent diagnostic history</label><button type="button" data-diagnostic="clear">Clear local diagnostics</button><p>Disabling history leaves this already frozen report available; Clear removes it. Local drafts expire after 24 hours. Clearing removes local evidence and cached delivery authority; it does not undo a submitted support report.</p></details>
  </div>`;
  document.body.append(root);document.body.style.overflow='hidden';
  diagnosticElement(run,'titles').disabled=!playback.title;diagnosticElement(run,'history').checked=CinemaDiagnostics.current().enabled;
  diagnosticElement(run,'system').hidden=!ME?.is_admin;diagnosticElement(run,'system').addEventListener('click',()=>diagnosticAction(run,r=>diagnosticSnapshot(r,true)));
  diagnosticElement(run,'close').addEventListener('click',()=>closeDiagnostics());
  for(const [name,fn]of [['export',diagnosticExport],['send',r=>diagnosticSend(r,false)],['phone',r=>diagnosticSend(r,true)],['redeem',diagnosticRedeem],['check',diagnosticCheck]])diagnosticElement(run,name).addEventListener('click',()=>diagnosticAction(run,fn));
  diagnosticElement(run,'reproduce').addEventListener('click',()=>diagnosticReproduce(run));diagnosticElement(run,'stop').addEventListener('click',()=>diagnosticFinishReproduction(run,CinemaDiagnostics.stopReproduction()));
  diagnosticElement(run,'approve').addEventListener('click',()=>diagnosticAction(run,async r=>{await r.delivery.approve(r.comparison);diagnosticMessage(r,'Phone approved. It can finish while this screen is offline.');diagnosticElement(r,'approve').hidden=true;}));
  diagnosticElement(run,'reject').addEventListener('click',()=>diagnosticAction(run,async r=>{await r.delivery.cancel(true);diagnosticElement(r,'handoff').hidden=true;diagnosticMessage(r,'Phone rejected. The staged draft remains for a new handoff.');}));
  diagnosticElement(run,'cancel').addEventListener('click',()=>diagnosticAction(run,async r=>{await r.delivery.cancel(false);diagnosticElement(r,'handoff').hidden=true;diagnosticMessage(r,'Staged report deleted. Your local capture remains available.');}));
  diagnosticElement(run,'history').addEventListener('change',()=>CinemaDiagnostics.disable(!diagnosticElement(run,'history').checked));
  diagnosticElement(run,'clear').addEventListener('click',()=>{CinemaDiagnostics.clear();closeDiagnostics(true);});
  root.addEventListener('keydown',event=>{if(event.key==='Tab'){const controls=Array.from(/** @type {NodeListOf<HTMLElement & {disabled?:boolean}>} */(root.querySelectorAll('button,input,textarea,select,summary'))).filter(el=>!el.disabled&&!el.closest('[hidden]')&&el.getClientRects().length);const first=controls[0],last=controls[controls.length-1];if(event.shiftKey&&document.activeElement===first){event.preventDefault();last.focus();}else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first.focus();}}});
  if(CinemaDiagnostics.current().reproduction){run.reproduction=CinemaDiagnostics.current().reproduction.id;diagnosticElement(run,'reproduce').hidden=true;diagnosticElement(run,'stop').hidden=false;}
  diagnosticElement(run,'close').focus();diagnosticCapabilities(run);diagnosticSnapshot(run);
  const enrollment=await CinemaDiagnosticStore.get('enrollment:'+capture.scope);if(diagnosticCurrent(run)&&enrollment&&enrollment.expires>Date.now()){run.enrollment=enrollment.enrollment;diagnosticElement(run,'origin').value=enrollment.origin;run.trustedOrigin=enrollment.origin;}
  const previous=await CinemaDiagnosticStore.get('draft:'+capture.scope);if(diagnosticCurrent(run)&&previous&&previous.expires>Date.now()&&previous.capture&&previous.capture.scope===capture.scope){const button=document.createElement('button');button.type='button';button.textContent='Resume saved local draft';button.addEventListener('click',()=>{run.capture=previous.capture;run.playback.title=previous.consentedTitle||null;run.deliveryState=previous.delivery||null;diagnosticElement(run,'description').value=previous.description||'';diagnosticElement(run,'category').value=previous.category||'other';diagnosticElement(run,'titles').checked=!!previous.titles;diagnosticMessage(run,'Saved local draft restored. Sending requires your explicit action.');});root.querySelector('.diagnostic-actions').append(button);}
}
async function diagnosticAction(run,fn) {
  if(!diagnosticCurrent(run)||run.busy)return;run.busy=true;run.root.setAttribute('aria-busy','true');
  try{await fn(run);}catch(error){if(diagnosticCurrent(run))diagnosticMessage(run,error.code==='quota_exceeded'?'Support storage or rate quota reached. Save locally or retry later.':error.code==='expired'?'Support authority expired. Your local capture remains available.':'The operation could not finish. Your local capture remains available; retry explicitly.');}finally{run.busy=false;if(diagnosticCurrent(run))run.root.removeAttribute('aria-busy');}
}
async function diagnosticCapabilities(run) {try{const value=await api('/diagnostics/capabilities',{signal:run.controller.signal});if(!diagnosticCurrent(run))return;if(value.support?.configured){diagnosticElement(run,'origin').value=value.support.origin;run.trustedOrigin=value.support.origin;}}catch(_){}diagnosticCoverage(run);}
function diagnosticCoverage(run) {if(!diagnosticCurrent(run))return;diagnosticElement(run,'coverage').textContent='Client: '+run.capture.events.length+' events; dropped '+run.capture.dropped+'. Persistence: '+run.capture.persistence+'. Server: '+(run.server?.state||'not available')+'. Render range: unknown. Browser platform crash evidence: unsupported.';}
async function diagnosticSnapshot(run,includeSystem=false) {
  if(!run.token)return;
  try{const job=await api('/diagnostics/snapshots',{method:'POST',body:{incident_at_ms:Date.parse(run.capture.captured_at),window_ms:Math.min(600000,run.capture.window_ms),include_system:includeSystem,...(run.playback.facts?.correlation?{correlation:run.playback.facts.correlation}:{})},signal:run.controller.signal});if(!diagnosticCurrent(run))return;run.snapshotId=job.id;
    const deadline=performance.now()+6000;while(diagnosticCurrent(run)&&performance.now()<deadline){const result=await api('/diagnostics/snapshots/'+job.id,{signal:run.controller.signal});if(!diagnosticCurrent(run))return;if(result.state!=='pending'){if(result.version!==CinemaDiagnosticWire.version||!Array.isArray(result.events)||result.events.length>10000)throw new Error('Invalid projection');result.events=result.events.map(e=>CinemaDiagnosticWire.event(JSON.stringify(e)));run.server=result;break;}await new Promise(resolve=>setTimeout(resolve,250));}
  }catch(_){if(diagnosticCurrent(run))run.server={state:'unavailable',components:[{name:'server',state:'unavailable',reason:'connection_failed'}],events:[]};}finally{const id=run.snapshotId;run.snapshotId=null;if(/^[a-f0-9-]{36}$/.test(id||'')&&run.generation===AUTH_GENERATION){const cleanup=new AbortController();const timer=setTimeout(()=>cleanup.abort(),1000);try{await api('/diagnostics/snapshots/'+id,{method:'DELETE',signal:cleanup.signal});}catch(_){}finally{clearTimeout(timer);}}}diagnosticCoverage(run);
}
async function diagnosticBundle(run) {
  const category=diagnosticElement(run,'category').value,description=diagnosticElement(run,'description').value;
  const title=diagnosticElement(run,'titles').checked&&run.playback.title;const components=[{name:'client',state:'complete'},{name:'platform',state:'unavailable',reason:'unsupported'},{name:'clock',state:'unavailable',reason:'clock_unknown'}];
  if(!run.capture.history_enabled&&run.capture.mode!=='reproduction')components[0]={name:'client',state:'partial',reason:'history_disabled'};else if(run.capture.dropped!=='0')components[0]={name:'client',state:'partial',reason:'overflow'};
  const events=run.capture.events.filter(e=>e.run_id&&e.facts).map(e=>({...e,facts:CinemaDiagnostics.facts(e.facts)}));
  const files={'description.txt':description,'client-events.jsonl':events.map(e=>JSON.stringify(e)).join('\n')+(events.length?'\n':'')};
  if(title)files['playback.json']=JSON.stringify({events:events.filter(e=>e.domain==='playback'),title});
  if(run.server?.events?.some(e=>e.facts.active_producers!==undefined))files['server-snapshot.json']=JSON.stringify(run.server.events.filter(e=>e.facts.active_producers!==undefined));
  if(run.server?.events){files['server-events.jsonl']=run.server.events.map(e=>JSON.stringify({...e,facts:CinemaDiagnostics.facts(e.facts)})).join('\n')+(run.server.events.length?'\n':'');for(const c of run.server.components||[])if(['server','workers'].includes(c.name)&&!components.some(v=>v.name===c.name))components.push(c);}
  if(!components.some(c=>c.name==='server'))components.push({name:'server',state:'unavailable',reason:run.token?'connection_failed':'context_unavailable'});
  if(run.connection.length){files['connection.json']=JSON.stringify(run.connection);components.push({name:'connection',state:'complete'});}else components.push({name:'connection',state:'unavailable',reason:'not_requested'});
  const label=value=>typeof value==='string'&&/^[A-Za-z0-9._+-]{1,64}$/.test(value)?value:'unknown';
  const manifest={version:CinemaDiagnosticWire.version,report_id:run.capture.id,captured_at:run.capture.captured_at,category,scope:run.token?(ME?.is_admin?'administrator':'viewer'):'anonymous',client:{platform:layoutId()==='theater'?'browser_tv':'web',app_version:label(SERVER?.version),build:label(SERVER?.build),os_version:label(browserLabel()),device_model:'browser'},capture:{mode:run.capture.mode||'ordinary',window_ms:run.capture.window_ms,dropped_events:run.capture.dropped,persistence:run.capture.persistence},components,privacy:{titles_included:!!title,attachments_included:false},files:[]};
  const value=await CinemaDiagnosticExport.archive(manifest,files,run.controller.signal);if(!diagnosticCurrent(run))throw new Error('Report operation ended');return value;
}
async function diagnosticPersist(run) {
  if(!diagnosticCurrent(run))return;const record={key:'draft:'+run.capture.scope,type:'capture',at:Date.now(),expires:Date.now()+86400000,bytes:JSON.stringify(run.capture).length+(run.deliveryState?.archive?.size||0),capture:run.capture,description:diagnosticElement(run,'description').value,category:diagnosticElement(run,'category').value,titles:diagnosticElement(run,'titles').checked,consentedTitle:diagnosticElement(run,'titles').checked?run.playback.title:null,delivery:run.deliveryState};await CinemaDiagnosticStore.put(record);
}
async function diagnosticExport(run) {const value=await diagnosticBundle(run);await diagnosticPersist(run);if(!diagnosticCurrent(run))return;CinemaDiagnosticExport.save(value.blob,run.capture.id);diagnosticMessage(run,'Report saved. This did not upload it.');}
async function diagnosticPrepareDelivery(run) {
  if(run.deliveryState?.state==='deleted'){run.capture.id=CinemaDiagnostics.uuid();run.deliveryState=null;}
  if(!run.deliveryState){const value=await diagnosticBundle(run),digest=await CinemaDiagnosticExport.digest(value.blob,run.controller.signal);run.deliveryState={origin:diagnosticElement(run,'origin').value,consented:true,archive:value.blob,binding:{report_id:run.capture.id,sha256:digest,length:value.blob.size,privacy:value.manifest.privacy},state:'local',enrollment:run.enrollment||null};await diagnosticPersist(run);}
  run.delivery=createCinemaDiagnosticDelivery({state:run.deliveryState,persist:()=>diagnosticPersist(run),current:()=>diagnosticCurrent(run),signal:run.controller.signal});
  for(const name of ['category','titles'])diagnosticElement(run,name).disabled=true;
}
async function diagnosticSend(run,phone) {
  if(CinemaDiagnostics.current().reproduction)throw new Error('Stop reproduction before delivery');
  const destination=diagnosticElement(run,'origin').value;if(!destination){diagnosticMessage(run,'This Cinema has no support destination. Save the report or enter an operator-issued invitation and its HTTPS origin.');return;}
  if(!confirm((phone?'Temporarily upload this selected report so an approved phone can finish it at ':'Send this selected report to ')+destination+'?'))return;
  await diagnosticPrepareDelivery(run);diagnosticMessage(run,'Uploading the consented report. Local capture is retained.');
  const result=phone?await run.delivery.offer():await run.delivery.submit(diagnosticElement(run,'description').value);
  if(!diagnosticCurrent(run))return;if(result.state==='submitted'){diagnosticMessage(run,'Report sent. Support reference: '+result.reference);return;}
  run.handoffDeadline=performance.now()+Math.min(1800000,result.expires_in_ms);diagnosticElement(run,'handoff').hidden=false;diagnosticElement(run,'code').textContent='Manual code: '+result.manual_code+' · expires in ten minutes. Open '+run.deliveryState.origin+' on the phone.';
  try{const qr=await run.delivery.request('handoffs/'+result.id+'/qr',null,run.deliveryState.management_grant);if(diagnosticCurrent(run)){if(run.qrUrl)URL.revokeObjectURL(run.qrUrl);run.qrUrl=URL.createObjectURL(new Blob([qr.svg],{type:'image/svg+xml'}));diagnosticElement(run,'qr').src=run.qrUrl;diagnosticElement(run,'qr').hidden=false;}}catch(_){}
  diagnosticPollPhone(run);
}
async function diagnosticPollPhone(run) {
  if(!diagnosticCurrent(run)||!run.deliveryState?.handoff)return;
  if(performance.now()>=run.handoffDeadline){diagnosticElement(run,'approve').hidden=true;diagnosticMessage(run,'Handoff monitoring expired. Your local capture remains available.');return;}
  try{const result=await run.delivery.handoffStatus();if(!diagnosticCurrent(run))return;
    if(result.state==='claim_pending'){run.comparison=result.comparison;diagnosticElement(run,'comparison').textContent='Compare with your phone: '+result.comparison;diagnosticElement(run,'approve').hidden=false;}
    else if(result.state==='approved'){if(!run.approvedSeen){run.approvedSeen=true;run.handoffDeadline=performance.now()+1800000;}diagnosticElement(run,'approve').hidden=true;diagnosticMessage(run,'Phone approved. Waiting for its explicit submission.');}
    else if(result.state==='submitted'){const receipt=await run.delivery.request('reports/'+run.capture.id+'/receipt',null,run.deliveryState.management_grant,'GET');run.deliveryState.state='submitted';run.deliveryState.reference=receipt.reference;await diagnosticPersist(run);diagnosticMessage(run,'Report sent. Support reference: '+receipt.reference);return;}
    else if(['expired','revoked'].includes(result.state)){diagnosticElement(run,'approve').hidden=true;diagnosticMessage(run,'Handoff expired or canceled. Your local capture remains available.');return;}
  }catch(_){diagnosticMessage(run,'Support connection interrupted. The handoff remains subject to its expiry.');}
  run.pollTimer=setTimeout(()=>diagnosticPollPhone(run),2000);
}
async function diagnosticRedeem(run) {
  const selected=diagnosticElement(run,'origin').value;if(!confirm('Use an upload-only invitation at '+selected+'? No Cinema credential will be sent.'))return;
  const state={origin:selected};const delivery=createCinemaDiagnosticDelivery({state,persist:async()=>{},current:()=>diagnosticCurrent(run),signal:run.controller.signal});
  const proof=Array.from(crypto.getRandomValues(new Uint8Array(32)),v=>v.toString(16).padStart(2,'0')).join('');const value=await delivery.request('invitations/redeem',{code:diagnosticElement(run,'invitation').value.trim().toUpperCase(),proof});run.enrollment=value.invitation;diagnosticElement(run,'invitation').value='';diagnosticMessage(run,'Invitation ready for one explicitly consented report.');
}
function diagnosticReproduce(run) {
  if(!diagnosticCurrent(run)||run.deliveryState)return;run.reproduction=CinemaDiagnostics.startReproduction(capture=>diagnosticFinishReproduction(run,capture),run.capture);diagnosticElement(run,'reproduce').hidden=true;diagnosticElement(run,'stop').hidden=false;diagnosticMessage(run,'Reproduction is recording for at most five minutes. Dismiss this form, reproduce the issue, then reopen Report a problem to stop and review.');
}
function diagnosticFinishReproduction(run,capture) {if(!capture||!diagnosticCurrent(run))return;run.capture=capture;run.reproduction=null;diagnosticElement(run,'stop').hidden=true;diagnosticElement(run,'reproduce').hidden=false;diagnosticMessage(run,'Reproduction stopped. The original incident and report identity were retained. Review and save or send explicitly.');diagnosticPersist(run);diagnosticSnapshot(run);}
async function diagnosticCheck(run) {
  const began=performance.now(),controller=new AbortController(),abort=()=>controller.abort();run.controller.signal.addEventListener('abort',abort,{once:true});const timer=setTimeout(abort,15000);let budget=0;
  try{for(const [path,phase,authenticated]of /** @type {Array<[string,string,boolean]>} */([['/healthz','connect',false],['/readyz','readiness',false],['/api/v1/me','auth',true]])){if(authenticated&&!run.token)continue;if(controller.signal.aborted||performance.now()-began>=15000)break;
      const start=performance.now();let status=0,failure=null;try{const response=await fetch(path,{headers:authenticated?{Authorization:'Bearer '+run.token}:{},credentials:'omit',redirect:'error',cache:'no-store',signal:controller.signal});status=response.status;const reader=response.body?.getReader();if(reader){try{for(;;){const value=await reader.read();if(value.done)break;budget+=value.value.length;if(budget>2097152){controller.abort();break;}}}finally{reader.cancel().catch(()=>{});reader.releaseLock();}}}catch(_){failure=controller.signal.aborted?'cancelled':'browser_network';}
      if(!diagnosticCurrent(run))return;const base=run.capture.events[run.capture.events.length-1]||{run_id:CinemaDiagnostics.uuid(),sequence:'0',utc:run.capture.captured_at,elapsed_ms:0};const event={...base,run_id:run.checkRun||(run.checkRun=CinemaDiagnostics.uuid()),sequence:String(run.checkSequence=(run.checkSequence||0)+1),utc:new Date().toISOString().slice(0,19)+'Z',domain:'network',kind:'request',facts:CinemaDiagnostics.facts({phase,status,duration_ms:Math.round(performance.now()-start),observed:!!status,...(failure?{failure}:{})})};run.capture.events.push(event);run.connection.push(event);
    }diagnosticMessage(run,'Checks recorded. Browser APIs cannot distinguish DNS, TCP and TLS failures; an HTTP response proves only the observed request.');
  }finally{clearTimeout(timer);run.controller.signal.removeEventListener('abort',abort);diagnosticCoverage(run);}
}
function closeDiagnostics(retire=false) {
  const run=DIAGNOSTIC_REPORT;if(!run)return;
  if(!retire&&run.deliveryState&&!['local','submitted','deleted'].includes(run.deliveryState.state)&&!confirm('A staged report may remain at support until its expiry. Dismiss and keep that draft? Use Cancel and delete to erase it instead.'))return;
  if(!retire)diagnosticPersist(run);run.controller.abort();clearTimeout(run.pollTimer);if(run.qrUrl)URL.revokeObjectURL(run.qrUrl);run.root.remove();document.body.style.overflow=run.previousOverflow;DIAGNOSTIC_REPORT=null;
  if(retire)CinemaDiagnostics.stopReproduction();if(run.opener?.isConnected)run.opener.focus();
}
function cinemaDiagnosticsRemoteBind() {
  const run=DIAGNOSTIC_REPORT;if(!run||!diagnosticCurrent(run))return false;
  CinemaRemote.registerScope({id:'diagnostics:'+run.capture.id,root:run.root,category:'unsupported',back:()=>{closeDiagnostics();return'applied';},home:()=>{closeDiagnostics();return'applied';}});
  for(const element of run.root.querySelectorAll('button,input,textarea,select,summary')){if(!element.dataset.diagnostic)continue;CinemaRemote.registerAction({id:'diagnostics:'+element.dataset.diagnostic,element,label:element.textContent||element.getAttribute('aria-label')||element.dataset.diagnostic,localOnly:true,activate:()=>{element.click();return'applied';}});}return true;
}
window.addEventListener("cinema-diagnostic-auth",diagnosticAuthChanged);
window.CinemaOpenDiagnostics=showDiagnostics;
diagnosticAuthChanged();
