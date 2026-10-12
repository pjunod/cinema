// Actual embedded web player in a same-origin Safari iframe; no product overrides.
const fs=require('node:fs/promises'),path=require('node:path'),http=require('node:http'),crypto=require('node:crypto');
const {spawn}=require('node:child_process');
const root=path.resolve(__dirname,'../../../..');
const {startServer,api}=require(path.join(root,'scripts/playback-lab'));
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function command(args){return new Promise((resolve,reject)=>{const p=spawn('/usr/bin/osascript',args,{stdio:['ignore','pipe','pipe']});let out='',err='';p.stdout.on('data',x=>out+=x);p.stderr.on('data',x=>err+=x);const t=setTimeout(()=>p.kill('SIGKILL'),15000);p.on('close',c=>{clearTimeout(t);c===0?resolve(out):reject(Error(err||'AppleScript '+c))});p.on('error',reject)})}
async function browserRun(file){
 const report={userAgent:navigator.userAgent,phases:[],events:[]};const frame=document.getElementById('appFrame');let w;
 const until=async(f,ms=60000)=>{const end=performance.now()+ms;while(performance.now()<end){if(await f())return;await new Promise(r=>setTimeout(r,25))}throw Error('deadline')};
 const evaluate=s=>w.eval(s);let v,callbackId=null,current=null;
 const canvas=document.createElement('canvas');canvas.width=32;canvas.height=18;const ctx=canvas.getContext('2d',{willReadFrequently:true});
 function callback(wall,m){if(current&&!v.seeking&&!v.paused){ctx.drawImage(v,0,0,32,18);let pixels=0;for(const n of ctx.getImageData(0,0,32,18).data)pixels=(pixels*31+n)>>>0;const row={wall,media:m.mediaTime,clock:v.currentTime,absolute:evaluate('pbNowSec()'),pixels,width:v.videoWidth,height:v.videoHeight};current.observations.push(row);if(current.admitted||Math.abs(row.absolute-current.target)<0.25&&Math.abs(row.media-row.clock)<0.25){current.admitted=true;current.frames.push(row)}else current.initialPreroll++;}callbackId=v.requestVideoFrameCallback(callback)}
 try{
  await until(()=>frame.contentWindow&&frame.contentWindow.document.readyState==='complete');w=frame.contentWindow;await until(()=>evaluate('typeof play==="function"&&typeof CAPS_Q==="string"'));
  report.caps=evaluate('PLAY_CAPS');
  for(const [name,target]of [['cold-mid',6],['forward',18],['backward',3],['restart',0]]){
   if(callbackId!==null)v.cancelVideoFrameCallback(callbackId);callbackId=null;current=null;
   const p={name,target,trigger:w.performance.now(),frames:[],observations:[],initialPreroll:0,admitted:false};report.phases.push(p);
   if(name==='cold-mid'||name==='restart'){
    if(name==='restart')await evaluate('closePlayer()');
    // Muting the owned element permits script-driven playback without a fake user gesture.
    evaluate('document.getElementById("video").muted=true');
    await evaluate(`play(${JSON.stringify(file.id)},"Authored qualification",${target*1000},${file.duration_ms||24000},{})`);
   }else await evaluate(`seekTo(${target})`);
   await until(()=>evaluate('!!PLAYER&&playbackOwnsAttachedMedia(PLAYER)'));
   v=w.document.getElementById('video');v.muted=true;
   await until(()=>!v.seeking);current=p;callbackId=v.requestVideoFrameCallback(callback);await v.play();
   await until(()=>p.frames.length>=30);current=null;
   p.firstFrameMs=p.frames[0].wall-p.trigger;p.firstError=Math.abs(p.frames[0].absolute-target);p.maxClockError=Math.max(...p.frames.map(x=>Math.abs(x.media-x.clock)));p.reversals=p.frames.slice(1).filter((x,i)=>x.media<p.frames[i].media-0.001).length;p.uniquePixels=new Set(p.frames.map(x=>x.pixels)).size;p.player=evaluate('({method:PLAYER.method,presentation:PLAYER.presentation,sessionId:PLAYER.sessionId,route:PLAYER.route,transport:PLAYER.transport,owns:playbackOwnsAttachedMedia(PLAYER),wantsPlayback:PLAYER.wantsPlayback})');p.pass=p.firstError<=0.25&&p.maxClockError<=0.25&&p.reversals===0&&p.uniquePixels>1&&p.player.owns;
  }
  report.pass=report.phases.length===4&&report.phases.every(p=>p.pass);
 }catch(e){report.error=String(e);report.pass=false;try{report.diagnostic=evaluate('({surface:PLAYBACK_SURFACE,player:PLAYER&&{method:PLAYER.method,sessionId:PLAYER.sessionId,presentation:PLAYER.presentation,started:PLAYER.started},video:{ready:document.getElementById("video").readyState,error:document.getElementById("video").error?.message}})')}catch{}}
 finally{current=null;if(callbackId!==null)v.cancelVideoFrameCallback(callbackId);try{await evaluate('closePlayer()');w.localStorage.clear()}catch{};await fetch('/qualifier-receipt',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(report)});}
}
(async()=>{
 const [binary,fixtures,output]=process.argv.slice(2);if(!output)throw Error('binary fixtures output required');let daemon,proxy,url;const report={source:process.env.QUAL_SOURCE||null,daemon_sha256:crypto.createHash('sha256').update(await fs.readFile(binary)).digest('hex'),requests:[]};let receipt;
 try{
  daemon=await startServer({server:binary,enable_vod:true,startup_timeout_ms:180000},fixtures);await api(daemon.baseUrl,'/settings',{token:daemon.token,method:'PUT',body:{transcode_software_pool_threads:2}});
  const file=[...daemon.files.values()][0];if(!file)throw Error('no indexed file');report.file={id:file.id,filename:file.filename};
  const html='<!doctype html><meta charset=utf-8><title>Owned Plurx Safari qualification</title><script>localStorage.setItem("plurx_token",'+JSON.stringify(daemon.token)+');localStorage.setItem("plurx_quality","240");localStorage.setItem("plurx_autonext","0");localStorage.setItem("plurx_autoskip","0");</script><iframe id=appFrame src="/" style="border:0;width:100vw;height:95vh"></iframe><script>('+browserRun.toString()+')('+JSON.stringify(file)+');</script>';
  proxy=http.createServer(async(req,res)=>{
   if(req.url==='/qualifier'){res.setHeader('Content-Type','text/html');res.end(html);return}
   if(req.url==='/qualifier-receipt'){let body='';for await(const b of req){body+=b;if(body.length>2e6){res.writeHead(413);res.end();return}}receipt=JSON.parse(body);res.end('ok');return}
   const u=new URL(req.url,daemon.baseUrl);report.requests.push({method:req.method,path:u.pathname});const upstream=http.request(u,{method:req.method,headers:{...req.headers,host:u.host}},r=>{res.writeHead(r.statusCode,r.headers);r.pipe(res)});upstream.on('error',()=>{res.writeHead(502);res.end()});req.pipe(upstream);
  });await new Promise(r=>proxy.listen(0,'127.0.0.1',r));url='http://127.0.0.1:'+proxy.address().port+'/qualifier';await command(['-e','tell application "Safari"','-e','make new document with properties {URL:'+JSON.stringify(url)+'}','-e','activate','-e','end tell']);
  const end=Date.now()+240000;while(!receipt&&Date.now()<end)await sleep(100);if(!receipt)throw Error('Safari UI receipt deadline');report.receipt=receipt;
 }catch(e){report.error=String(e)}finally{
  if(url){try{await command(['-e','tell application "Safari"','-e','repeat with i from (count documents) to 1 by -1','-e','set d to document i','-e','if URL of d starts with '+JSON.stringify(url)+' then close d','-e','end repeat','-e','end tell']);report.owned_tab_closed=true}catch(e){report.cleanup_error=String(e)}}
  if(proxy)await new Promise(r=>proxy.close(r));if(daemon)await daemon.close(false);report.resources_closed=true;await fs.writeFile(output,JSON.stringify(report,null,2));console.log(JSON.stringify({pass:report.receipt?.pass,error:report.error||report.receipt?.error,resources_closed:report.resources_closed}));
 }
})().catch(e=>{console.error(e);process.exitCode=1});
