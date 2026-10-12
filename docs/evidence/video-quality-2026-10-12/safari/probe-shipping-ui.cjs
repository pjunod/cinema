// Actual embedded web player as top-level Safari document; appended observer only.
const fs=require('node:fs/promises'),path=require('node:path'),http=require('node:http'),crypto=require('node:crypto');
const {spawn}=require('node:child_process');
const root=path.resolve(__dirname,'../../../..');
const {startServer,api}=require(path.join(root,'scripts/playback-lab'));
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function command(args){return new Promise((resolve,reject)=>{const p=spawn('/usr/bin/osascript',args,{stdio:['ignore','pipe','pipe']});let out='',err='';p.stdout.on('data',x=>out+=x);p.stderr.on('data',x=>err+=x);const t=setTimeout(()=>p.kill('SIGKILL'),15000);p.on('close',c=>{clearTimeout(t);c===0?resolve(out):reject(Error(err||'AppleScript '+c))});p.on('error',reject)})}
async function browserRun(file){
 const report={userAgent:navigator.userAgent,phases:[],events:[],callbackErrors:[]};let w=window;
 const until=async(f,ms=60000,label='readiness')=>{const end=performance.now()+ms;while(performance.now()<end){if(await f())return;await new Promise(r=>setTimeout(r,25))}throw Error(label+' deadline')};
 const bounded=(promise,ms,label)=>{let timer;return Promise.race([promise,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error(label+' deadline')),ms)})]).finally(()=>clearTimeout(timer))};const evaluate=s=>w.eval(s);let v,callbackId=null,current=null;
 const canvas=document.createElement('canvas');canvas.width=32;canvas.height=18;const ctx=canvas.getContext('2d',{willReadFrequently:true});
 function callback(wall,m){try{if(current&&!v.seeking&&!v.paused){ctx.drawImage(v,0,0,32,18);let pixels=0;for(const n of ctx.getImageData(0,0,32,18).data)pixels=(pixels*31+n)>>>0;const row={wall,media:m.mediaTime,clock:v.currentTime,absolute:evaluate('pbPosSec()'),pixels,width:v.videoWidth,height:v.videoHeight};current.observations.push(row);if(current.admitted||Math.abs(row.absolute-current.target)<0.25&&Math.abs(row.media-row.clock)<0.25){current.admitted=true;current.frames.push(row)}else current.initialPreroll++;}}catch(e){report.callbackErrors.push(String(e))}finally{callbackId=v.requestVideoFrameCallback(callback)}}
 try{
  await until(()=>document.readyState==='complete');await until(()=>evaluate('typeof play==="function"&&typeof CAPS_Q==="string"'));
  await bounded(evaluate('PLAY_CAPS_READY'),30000,'capabilities');await until(()=>evaluate('!!ME&&!!SERVER&&!!LAST_ROUTE'));await bounded(evaluate('render()'),30000,'initial route');report.boot=evaluate('({authenticated:!!ME,serverReady:!!SERVER,route:LAST_ROUTE})');report.caps=evaluate('PLAY_CAPS');
  for(const [name,target]of [['cold-mid',6],['forward',18],['backward',3],['restart',0]]){
   if(callbackId!==null)v.cancelVideoFrameCallback(callbackId);callbackId=null;current=null;
   const p={name,target,trigger:w.performance.now(),frames:[],observations:[],initialPreroll:0,admitted:false};report.phases.push(p);
   if(name==='cold-mid'||name==='restart'){
    if(name==='restart')await evaluate('closePlayer()');
    // Muting the owned element permits script-driven playback without a fake user gesture.
    evaluate('document.getElementById("video").muted=true');
    await evaluate(`play(${JSON.stringify(file.id)},"Authored qualification",${target*1000},${file.duration_ms||24000},{})`);
   }else await evaluate(`seekTo(${target})`);
   p.stage='ownership';await until(()=>evaluate('!!PLAYER&&playbackOwnsAttachedMedia(PLAYER)'),60000,'ownership');
   v=w.document.getElementById('video');v.muted=true;
   p.stage='seek settlement';await until(()=>!v.seeking,60000,'seek settlement');current=p;callbackId=v.requestVideoFrameCallback(callback);p.stage='media play';await bounded(v.play(),15000,"media play");
   p.stage='frame observations';await until(()=>{if(report.callbackErrors.length)throw Error('frame callback: '+report.callbackErrors[0]);return p.frames.length>=30},60000,'frame observations');current=null;p.stage='complete';
   p.firstFrameMs=p.frames[0].wall-p.trigger;p.firstError=Math.abs(p.frames[0].absolute-target);p.maxClockError=Math.max(...p.frames.map(x=>Math.abs(x.media-x.clock)));p.reversals=p.frames.slice(1).filter((x,i)=>x.media<p.frames[i].media-0.001).length;p.uniquePixels=new Set(p.frames.map(x=>x.pixels)).size;p.player=evaluate('({method:PLAYER.method,presentation:PLAYER.presentation,sessionId:PLAYER.sessionId,route:PLAYER.route,transport:PLAYER.transport,nativeHlsCapability:useNativeHls(document.getElementById("video")),hlsJsAttached:!!PLAYER.hls,sourceProtocol:new URL(document.getElementById("video").currentSrc,location.href).protocol,sourcePath:new URL(document.getElementById("video").currentSrc,location.href).pathname,owns:playbackOwnsAttachedMedia(PLAYER),wantsPlayback:PLAYER.wantsPlayback})');p.pass=p.firstError<=0.25&&p.maxClockError<=0.25&&p.reversals===0&&p.uniquePixels>1&&p.player.owns;
  }
  report.pass=report.phases.length===4&&report.phases.every(p=>p.pass)&&report.callbackErrors.length===0;
 }catch(e){report.error=String(e);report.pass=false;try{report.diagnostic=evaluate('({surface:PLAYBACK_SURFACE,player:PLAYER&&{method:PLAYER.method,sessionId:PLAYER.sessionId,presentation:PLAYER.presentation,started:PLAYER.started,owns:playbackOwnsAttachedMedia(PLAYER),pendingOpen:!!PLAYER.pendingOpenAttempt,pendingChange:!!PLAYER.pendingMediaChange},video:{seeking:document.getElementById("video").seeking,paused:document.getElementById("video").paused,muted:document.getElementById("video").muted,currentTime:document.getElementById("video").currentTime,ready:document.getElementById("video").readyState,error:document.getElementById("video").error?.message}})')}catch{}}
 finally{current=null;if(callbackId!==null)v.cancelVideoFrameCallback(callbackId);try{await bounded(evaluate('closePlayer()'),10000,'player close');w.localStorage.clear()}catch{};await fetch('/qualifier-receipt',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(report)});}
}
(async()=>{
 const [binary,fixtures,output]=process.argv.slice(2);if(!output)throw Error('binary fixtures output required');let daemon,proxy,url;const report={source:process.env.QUAL_SOURCE||null,daemon_sha256:crypto.createHash('sha256').update(await fs.readFile(binary)).digest('hex'),requests:[],harness_sha256:crypto.createHash('sha256').update(await fs.readFile(__filename)).digest('hex'),playback_lab_sha256:crypto.createHash('sha256').update(await fs.readFile(path.join(root,'scripts/playback-lab'))).digest('hex'),fixtures:[]};let receipt;
 for(const name of await fs.readdir(fixtures)){const bytes=await fs.readFile(path.join(fixtures,name));report.fixtures.push({name,bytes:bytes.length,sha256:crypto.createHash('sha256').update(bytes).digest('hex')})}
 try{
  report.startup_requested_at=new Date().toISOString();daemon=await startServer({server:binary,enable_vod:true,startup_timeout_ms:180000},fixtures);report.prepared_at=new Date().toISOString();const system=await api(daemon.baseUrl,'/system',{token:daemon.token});report.bound_tools={ffprobe_build_digest:system.ffprobe_build_digest,ffmpeg_version:system.ffmpeg_version,hwaccel_pref:system.hwaccel_pref};await api(daemon.baseUrl,'/settings',{token:daemon.token,method:'PUT',body:{transcode_software_pool_threads:3}});report.resource_policy='Explicit isolated software pool3, meeting shared AAC admission floor';report.software_pool_threads=(await api(daemon.baseUrl,'/settings',{token:daemon.token})).transcode_software_pool_threads;
  const file=[...daemon.files.values()][0];if(!file)throw Error('no indexed file');report.file={id:file.id,filename:file.filename};
  const appResponse=await fetch(daemon.baseUrl+'/');if(!appResponse.ok)throw Error('embedded document '+appResponse.status);const originalDocument=await appResponse.text();report.embedded_document_sha256=crypto.createHash('sha256').update(originalDocument).digest('hex');report.instrumentation='Original top-level document with auth prelude and appended observer; original application scripts and CSP unchanged';const securityHeaders={};for(const name of ['content-security-policy','x-frame-options'])if(appResponse.headers.has(name))securityHeaders[name]=appResponse.headers.get(name);
  const prelude='<script>localStorage.setItem("plurx_token",'+JSON.stringify(daemon.token)+');localStorage.setItem("plurx_quality","240");localStorage.setItem("plurx_autonext","0");localStorage.setItem("plurx_autoskip","0");</script>';
  const html=originalDocument.replace('<head>','<head>'+prelude).replace('</body>','<script>('+browserRun.toString()+')('+JSON.stringify(file)+');</script></body>');
  proxy=http.createServer(async(req,res)=>{
   if(req.url==='/qualifier'){res.setHeader('Content-Type','text/html');for(const [k,v]of Object.entries(securityHeaders))res.setHeader(k,v);res.end(html);return}
   if(req.url==='/qualifier-receipt'){let body='';for await(const b of req){body+=b;if(body.length>2e6){res.writeHead(413);res.end();return}}receipt=JSON.parse(body);res.end('ok');return}
   const u=new URL(req.url,daemon.baseUrl);const requestRow={method:req.method,path:u.pathname};report.requests.push(requestRow);const upstream=http.request(u,{method:req.method,headers:{...req.headers,host:u.host}},r=>{requestRow.status=r.statusCode;if(r.statusCode>=400){let errorBody='';r.on('data',b=>{if(errorBody.length<8192)errorBody+=b});r.on('end',()=>requestRow.error=errorBody)}res.writeHead(r.statusCode,r.headers);r.pipe(res)});upstream.on('error',()=>{res.writeHead(502);res.end()});req.pipe(upstream);
  });await new Promise(r=>proxy.listen(0,'127.0.0.1',r));url='http://127.0.0.1:'+proxy.address().port+'/qualifier';await command(['-e','tell application "Safari"','-e','make new document with properties {URL:'+JSON.stringify(url)+'}','-e','activate','-e','end tell']);
  const end=Date.now()+240000;while(!receipt&&Date.now()<end)await sleep(100);if(!receipt)throw Error('Safari UI receipt deadline');report.receipt=receipt;
 }catch(e){report.error=String(e)}finally{
  if(url){try{await command(['-e','tell application "Safari"','-e','repeat with i from (count documents) to 1 by -1','-e','set d to document i','-e','if URL of d starts with '+JSON.stringify(url)+' then close d','-e','end repeat','-e','end tell']);report.owned_tab_closed=true}catch(e){report.cleanup_error=String(e)}}
  if(proxy)await new Promise(r=>proxy.close(r));if(daemon){const logs=await fs.readFile(daemon.logFile,'utf8').catch(()=>"");report.daemon_log_window={first:logs.split('\n').find(Boolean),last:logs.trim().split('\n').at(-1)};report.server_diagnostics=logs.split('\n').filter(line=>/FFprobe identity|session create failed|playback startup phase completed/.test(line));await daemon.close(false)}report.resources_closed=true;await fs.writeFile(output,JSON.stringify(report,null,2));console.log(JSON.stringify({pass:report.receipt?.pass,error:report.error||report.receipt?.error,resources_closed:report.resources_closed}));if(report.receipt?.pass!==true)process.exitCode=1;
 }
})().catch(e=>{console.error(e);process.exitCode=1});
