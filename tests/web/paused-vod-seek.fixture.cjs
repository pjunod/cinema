'use strict';
// Interactive browser diagnostic: node tests/web/paused-vod-seek.fixture.cjs
// Open the printed loopback URL and press Run. The generated TS fixture is
// separate from genuine HDR hardware acceptance; all frame proof is rVFC.
const fs=require('node:fs'),path=require('node:path'),os=require('node:os'),http=require('node:http'),crypto=require('node:crypto');
const {execFileSync}=require('node:child_process');
const root=path.resolve(__dirname,'../..'),temp=fs.mkdtempSync(path.join(os.tmpdir(),'plurx-paused-seek-'));
const web=path.join(root,'crates/plurxd/src/web'),player=fs.readFileSync(path.join(web,'player/player.js'),'utf8');
function shipped(name){const at=player.indexOf(`function ${name}(`);if(at<0)throw Error(name);return player.slice(at,player.indexOf('\n}',at)+2);}
execFileSync(process.env.PLURX_FFMPEG||'ffmpeg',['-hide_banner','-loglevel','error','-f','lavfi','-i','testsrc2=s=320x180:r=25:d=52','-f','lavfi','-i','sine=frequency=440:sample_rate=48000:duration=52','-c:v','libx264','-pix_fmt','yuv420p','-g','50','-bf','3','-c:a','aac','-b:a','64k','-shortest','-f','hls','-hls_time','2','-hls_playlist_type','vod','-hls_segment_filename',path.join(temp,'seg%d.ts'),path.join(temp,'index.m3u8')]);
const bundle=fs.readFileSync(path.join(web,'hls.min.js')),requests=[];
const html=`<!doctype html><meta charset="utf-8"><title>Paused VOD seek diagnostic</title><h1>Paused VOD seek diagnostic</h1><p>Generated local HLS; actual paused target frames, baseline versus repaired loading.</p><button id="run">Run diagnostic</button><video muted width="640"></video><pre id="result">Ready</pre><script src="/hls.js"></script><script>
let PLAYER=null;const out=document.getElementById('result');
function playbackAttemptTerminallyStopped(){return false;}
function playbackSeekBufferCovers(v,p,target){const t=target/1000-(p.offset||0);for(let i=0;i<v.buffered.length;i++)if(v.buffered.start(i)<=t&&t<v.buffered.end(i))return true;return false;}
${['hlsStartupCurrent','pauseHlsStartup','resumePausedVodSeek'].map(shipped).join('\n')}
const ranges=v=>Array.from({length:v.buffered.length},(_,i)=>[v.buffered.start(i),v.buffered.end(i)]);
const wait=ms=>new Promise(resolve=>setTimeout(resolve,ms));
async function trial(repaired){
 const v=document.querySelector('video'),hls=new Hls({preferManagedMediaSource:false,enableWorker:true,maxBufferLength:8,maxMaxBufferLength:12,backBufferLength:2,startPosition:7});
 const token={},p={hls,vod:true,wantsPlayback:true,offset:0,controlIntentGeneration:1,mediaAttachment:token};PLAYER=p;
 p.hlsStartup={player:p,hls,state:'presenting',attachment:{current:()=>PLAYER===p&&p.mediaAttachment===token},retry:{state:'unused',timer:null}};
 hls.attachMedia(v);hls.loadSource('/index.m3u8');
 await Promise.race([new Promise(resolve=>v.addEventListener('canplay',resolve,{once:true})),wait(12000).then(()=>{throw Error('initial media timeout');})]);await v.play();await wait(300);
 p.wantsPlayback=false;p.controlIntentGeneration++;v.pause();pauseHlsStartup(p);
 const landings=[];
 for(const target of repaired?[35,24]:[35]){
  const before=ranges(v),started=performance.now();p.controlIntentGeneration++;
  const pending={executed:true,localVodSeek:true,targetMs:target*1000,sequence:landings.length+1,attachment:token,element:v};p.controlSeek=pending;
  let frame=null,id=null,landed;
  const observed=new Promise(resolve=>{const settled=()=>{if(frame!==null&&!v.seeking)resolve();};landed=settled;v.addEventListener('seeked',settled);const next=(_now,m)=>{if(Math.abs(m.mediaTime-target)<.25){frame=m.mediaTime;settled();}else id=v.requestVideoFrameCallback(next);};id=v.requestVideoFrameCallback(next);});
  v.currentTime=target;
  const cleanup=repaired?resumePausedVodSeek(v,p,pending):null;
  await Promise.race([observed,wait(repaired?12000:1500)]);
  if(id!=null)v.cancelVideoFrameCallback(id);v.removeEventListener('seeked',landed);
  const result={target,before,frame,ms:Math.round(performance.now()-started),paused:v.paused,seeking:v.seeking,after:ranges(v),passed:frame!==null&&v.paused&&!v.seeking};
  cleanup?.();p.controlSeek=null;landings.push(result);
  if(repaired&&!result.passed)break;
 }
 hls.destroy();PLAYER=null;v.removeAttribute('src');v.load();return {repaired,landings};
}
document.getElementById('run').onclick=async()=>{document.getElementById('run').disabled=true;try{out.textContent='Running baseline…';const baseline=await trial(false);out.textContent='Running repaired loader…';const repaired=await trial(true);const report={baseline,repaired};out.textContent=JSON.stringify(report,null,2);await fetch('/receipt',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(report)});}catch(error){out.textContent=String(error.stack||error);}};
</script>`;
const server=http.createServer((req,res)=>{
 const pathname=new URL(req.url,'http://fixture').pathname;requests.push({path:pathname,at:Date.now()});
 if(pathname==='/receipt'&&req.method==='POST'){let body='';req.on('data',b=>{body+=b;if(body.length>100000)req.destroy();});req.on('end',()=>{const report={...JSON.parse(body),bundle_sha256:crypto.createHash('sha256').update(bundle).digest('hex'),player_sha256:crypto.createHash('sha256').update(player).digest('hex'),requests};const receipt=path.resolve(process.env.PLURX_F1_RECEIPT||path.join(temp,'receipt.json'));fs.writeFileSync(receipt,JSON.stringify(report,null,2)+'\n');console.log('Receipt: '+receipt);res.end('saved');});return;}
 if(pathname==='/'){res.setHeader('Content-Type','text/html');res.end(html);return;}
 if(pathname==='/hls.js'){res.setHeader('Content-Type','text/javascript');res.end(bundle);return;}
 const file=path.join(temp,path.basename(pathname));if(!fs.existsSync(file)){res.statusCode=404;res.end();return;}
 res.setHeader('Content-Type',pathname.endsWith('.m3u8')?'application/vnd.apple.mpegurl':'video/mp2t');res.end(fs.readFileSync(file));
});
server.listen(0,'127.0.0.1',()=>console.log('Open http://127.0.0.1:'+server.address().port));
for(const signal of ['SIGINT','SIGTERM'])process.on(signal,()=>server.close(()=>{fs.rmSync(temp,{recursive:true,force:true});process.exit();}));
