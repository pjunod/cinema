'use strict';
// Generated, unencrypted MPEG-TS exercises the vendored remuxer and worker.
// NODE_PATH must point to an installed playwright package when not on PATH.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),os=require('node:os'),http=require('node:http'),crypto=require('node:crypto');
const {execFileSync}=require('node:child_process'),{chromium}=require('playwright');
const root=path.resolve(__dirname,'../..'),temporary=fs.mkdtempSync(path.join(os.tmpdir(),'plurx-hls-seek-'));
let browser,server;
(async()=>{
 for(const offset of [0,17]){
  const dir=path.join(temporary,String(offset));fs.mkdirSync(dir);
  execFileSync('ffmpeg',['-hide_banner','-loglevel','error','-y','-f','lavfi','-i','testsrc2=s=320x180:r=25:d=52','-f','lavfi','-i','sine=frequency=440:sample_rate=48000:duration=52','-c:v','libx264','-pix_fmt','yuv420p','-g','50','-bf','3','-c:a','aac','-b:a','64k','-shortest','-output_ts_offset',String(offset),'-f','hls','-hls_time','2','-hls_playlist_type','vod','-hls_segment_filename',path.join(dir,'seg%d.ts'),path.join(dir,'index.m3u8')]);
 }
 const requests=[];
 server=http.createServer((req,res)=>{
  const url=new URL(req.url,'http://fixture');requests.push(url.pathname);
  if(url.pathname==='/'){res.setHeader('Content-Type','text/html');return res.end('<video muted></video><script src="/hls.js"></script>');}
  const file=url.pathname==='/hls.js'?path.join(root,'crates/plurxd/src/web/hls.min.js'):path.join(temporary,url.pathname);
  if(!fs.existsSync(file)){res.statusCode=404;return res.end();}
  res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.m3u8')?'application/vnd.apple.mpegurl':'video/mp2t');
  let content=fs.readFileSync(file);
  if(url.searchParams.has('rolling'))content=Buffer.from(content.toString().replace('#EXT-X-PLAYLIST-TYPE:VOD\n','').replace('#EXT-X-ENDLIST',''));
  res.end(content);
 });
 await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
 browser=await chromium.launch({executablePath:process.env.PLURX_CHROME||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true});
 const reports=[];
 for(const offset of [0,17])for(const worker of [true,false]){
  const page=await browser.newPage();await page.goto(`http://127.0.0.1:${server.address().port}/`);
  const report=await page.evaluate(async({offset,worker})=>{
   const v=document.querySelector('video'),fatal=[],pts=[],appends=[];
   const hls=new Hls({preferManagedMediaSource:false,enableWorker:worker,maxBufferLength:8,maxMaxBufferLength:12,backBufferLength:2,startPosition:7});
   hls.on(Hls.Events.ERROR,(_e,d)=>{if(d.fatal)fatal.push(d.details);});
   hls.on(Hls.Events.INIT_PTS_FOUND,(_e,d)=>pts.push({id:d.id,initPTS:d.initPTS,timescale:d.timescale}));
   hls.on(Hls.Events.BUFFER_APPENDED,(_e,d)=>appends.push(d.type));
   const timeout=p=>Promise.race([p,new Promise((_,reject)=>setTimeout(()=>reject(Error('media timeout')),12000))]);
   const frameAt=target=>timeout(new Promise(resolve=>{
    const step=(_now,m)=>Math.abs(m.mediaTime-target)<.3?resolve(m.mediaTime):v.requestVideoFrameCallback(step);v.requestVideoFrameCallback(step);
   }));
   hls.attachMedia(v);hls.loadSource(`/${offset}/index.m3u8`);
   await timeout(new Promise(resolve=>v.addEventListener('canplay',resolve,{once:true})));await v.play();
   const landings=[];
   for(const target of [35,12,40]){const frame=frameAt(target);v.currentTime=target;landings.push({target,frame:await frame});}
   v.pause();const sole=frameAt(20);v.currentTime=20;landings.push({target:20,frame:await sole,paused:v.paused});
   const buffers=hls.bufferController.sourceBuffers.filter(x=>x[1]).map(([type,sb])=>({type,ranges:Array.from({length:sb.buffered.length},(_,i)=>[sb.buffered.start(i),sb.buffered.end(i)])}));
   const result={version:Hls.version,source:hls.bufferController.mediaSource.constructor.name,offset,worker,landings,pts,appends:[...new Set(appends)],buffers,fatal};
   hls.destroy();return result;
  },{offset,worker});
  assert.equal(report.fatal.length,0);assert.equal(report.landings.length,4);assert.equal(report.landings.at(-1).paused,true);assert.ok(report.appends.includes('video')&&report.appends.includes('audio'));
  // Compare appended track boundaries; this is remux timestamp coverage,
  // not a physical speaker/display sync measurement.
  const audio=report.buffers.find(x=>x.type==='audio'),video=report.buffers.find(x=>x.type==='video');
  assert.ok(audio&&video);assert.ok(Math.abs(audio.ranges.at(-1)[1]-video.ranges.at(-1)[1])<.15);
  reports.push(report);await page.close();
 }
 const page=await browser.newPage();await page.goto(`http://127.0.0.1:${server.address().port}/`);
 const refresh=await page.evaluate(async()=>{
  const hls=new Hls({preferManagedMediaSource:false,enableWorker:true,liveSyncDurationCount:2});let loaded=0;
  hls.on(Hls.Events.LEVEL_LOADED,()=>loaded++);hls.attachMedia(document.querySelector('video'));hls.loadSource('/0/index.m3u8?rolling=1');
  await new Promise((resolve,reject)=>{const t=setInterval(()=>{if(loaded>=3){clearInterval(t);resolve();}},100);setTimeout(()=>{clearInterval(t);reject(Error('live reload timeout'));},10000);});
  hls.destroy();return loaded;
 });assert.ok(refresh>=3);await page.close();
 const bundle=fs.readFileSync(path.join(root,'crates/plurxd/src/web/hls.min.js'));
 console.log(JSON.stringify({bundle_sha256:crypto.createHash('sha256').update(bundle).digest('hex'),reports,playlist_refreshes:refresh,encrypted:false,ll_hls:false},null,2));
})().catch(error=>{console.error(error);process.exitCode=1;}).finally(async()=>{if(browser)await browser.close();if(server)await new Promise(resolve=>server.close(resolve));fs.rmSync(temporary,{recursive:true,force:true});});
