const fs=require('node:fs/promises'),path=require('node:path'),crypto=require('node:crypto');
const {startServer,api}=require('../../scripts/playback-lab');
const ROOT=path.resolve(__dirname,'../..');
const OUT=path.join(__dirname,'served');
(async()=>{
 await fs.mkdir(OUT,{recursive:true});
 const s=await startServer({server:process.env.PLURX_QUALIFICATION_SERVER,enable_vod:true,vod_filenames:['live-action.mkv','animation.mkv'],server_log:'debug',keep_runtime:true},path.join(__dirname,'public-clips'));
 await fs.writeFile(path.join(__dirname,'private-server.json'),JSON.stringify({baseUrl:s.baseUrl,token:s.token,runtime:s.runtime,files:[...s.files]}));
 console.log('server-ready',s.baseUrl);
 const receipts=[];
 try{
  await api(s.baseUrl,'/settings',{token:s.token,method:'PUT',body:{transcode_software_pool_threads:2}});
  for(const frames of [0,2]){
   await api(s.baseUrl,'/settings',{token:s.token,method:'PUT',body:{vod_reorder_frames:frames}});
   for(const [name,file] of s.files){
    const id=path.basename(name,'.mkv')+'-bf'+frames,dir=path.join(OUT,id);await fs.mkdir(dir,{recursive:true});
    const body={playback_id:crypto.randomUUID(),request_id:crypto.randomUUID(),height:480,quality_auto:false,start:0,copy:false,aac:true,presentation:'vod',transport:'native',block_budget_secs:30};
    const started=performance.now(),session=await api(s.baseUrl,`/files/${file.id}/hls/sessions`,{token:s.token,method:'POST',body,timeout:90000});
    console.log('session-created',id,JSON.stringify(session));
    const url=new URL(session.playlist_url,s.baseUrl);url.searchParams.set('token',s.token);
    const response=await fetch(url);if(!response.ok)throw Error('playlist '+response.status);
    let playlist=await response.text();const lines=playlist.split('\n'),objects=[];
    const names=new Map();let n=0;
    async function capture(uri){
      if(names.has(uri))return names.get(uri);
      const remote=new URL(uri,url);remote.searchParams.set('token',s.token);
      const r=await fetch(remote,{signal:AbortSignal.timeout(90000)});if(!r.ok)throw Error('media '+r.status+' '+uri);
      const b=Buffer.from(await r.arrayBuffer());if(b.length>32*1024**2)throw Error('object budget');
      const ext=uri.includes('init')?'.mp4':'.m4s',local=String(n++).padStart(3,'0')+ext;
      await fs.writeFile(path.join(dir,local),b);names.set(uri,local);objects.push({file:local,bytes:b.length,sha256:crypto.createHash('sha256').update(b).digest('hex')});return local;
    }
    for(let i=0;i<lines.length;i++){
     const map=lines[i].match(/URI="([^"]+)"/);if(map)lines[i]=lines[i].replace(map[1],await capture(map[1]));
     else if(lines[i]&&!lines[i].startsWith('#'))lines[i]=await capture(lines[i]);
    }
    await fs.writeFile(path.join(dir,'index.m3u8'),lines.join('\n'));
    await api(s.baseUrl,`/hls/${session.session_id}`,{token:s.token,method:'DELETE'}).catch(e=>console.log('release',e.message));
    const row={id,requested_body:body,session,capture_ms:performance.now()-started,objects};delete row.session.playlist_url;receipts.push(row);
    await fs.writeFile(path.join(OUT,'capture.json'),JSON.stringify(receipts,null,2));console.log('captured',id,objects.length);
   }
  }
 }finally{await fs.copyFile(s.logFile,path.join(__dirname,'private-daemon.log'));await s.close(false);}
})().catch(e=>{console.error(e);process.exitCode=1});
