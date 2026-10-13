"use strict";

const CinemaDiagnosticExport = (() => {
  async function build(input,signal=null) {
    const { manifest, files, summary } = input;
    const encoder=new TextEncoder(), entries=[]; let total=0;
    for (const [name, value] of Object.entries(Object.assign({},files,{'summary.txt':summary}))) {
      const bytes=value instanceof Uint8Array?value:encoder.encode(value);
      total+=bytes.length;if(bytes.length>8388608||total>16700000||entries.length>=30)throw new Error('Report size limit');
      const digest=new Uint8Array(await cinemaDiagnosticSHA256(bytes,signal));
      entries.push({name,bytes,digest:Array.from(digest,v=>v.toString(16).padStart(2,'0')).join('')});
    }
    manifest.files=entries.map(row=>({name:row.name,size_bytes:row.bytes.length,sha256:row.digest}));
    entries.push({name:'manifest.json',bytes:encoder.encode(JSON.stringify(manifest))});
    const table=new Uint32Array(256);
    for(let n=0;n<256;n++){let c=n;for(let k=0;k<8;k++)c=(c&1)?0xedb88320^(c>>>1):c>>>1;table[n]=c>>>0;}
    const parts=[],central=[];let offset=0,centralBytes=0;
    for(const row of entries){
      let crc=0xffffffff;
      for(let start=0;start<row.bytes.length;start+=65536){if(signal?.aborted)throw new Error('Report cancelled');const end=Math.min(row.bytes.length,start+65536);for(let i=start;i<end;i++)crc=table[(crc^row.bytes[i])&255]^(crc>>>8);await new Promise(resolve=>setTimeout(resolve,0));}
      crc=(crc^0xffffffff)>>>0;const name=encoder.encode(row.name),header=new Uint8Array(30+name.length),view=new DataView(header.buffer);
      view.setUint32(0,0x04034b50,true);view.setUint16(4,20,true);view.setUint16(6,0x800,true);view.setUint32(14,crc,true);view.setUint32(18,row.bytes.length,true);view.setUint32(22,row.bytes.length,true);view.setUint16(26,name.length,true);header.set(name,30);
      const directory=new Uint8Array(46+name.length),dv=new DataView(directory.buffer);dv.setUint32(0,0x02014b50,true);dv.setUint16(4,20,true);dv.setUint16(6,20,true);dv.setUint16(8,0x800,true);dv.setUint32(16,crc,true);dv.setUint32(20,row.bytes.length,true);dv.setUint32(24,row.bytes.length,true);dv.setUint16(28,name.length,true);dv.setUint32(42,offset,true);directory.set(name,46);
      parts.push(header,row.bytes);central.push(directory);offset+=header.length+row.bytes.length;centralBytes+=directory.length;
    }
    const end=new Uint8Array(22),ev=new DataView(end.buffer);ev.setUint32(0,0x06054b50,true);ev.setUint16(8,entries.length,true);ev.setUint16(10,entries.length,true);ev.setUint32(12,centralBytes,true);ev.setUint32(16,offset,true);
    const blob=new Blob(parts.concat(central,[end]).map(part=>part.slice().buffer),{type:'application/zip'});if(blob.size>16777216)throw new Error('Report size limit');
    return {blob,manifest};
  }
  async function archive(manifest,files,signal=null) {
    const summary=CinemaDiagnosticWire.summary(manifest),input={manifest:structuredClone(manifest),files,summary};
    if(signal?.aborted)throw new Error('Report cancelled');
    let worker=null,url=null;
    if(typeof Worker==='function'&&typeof URL.createObjectURL==='function') {
      try {const source='const cinemaDiagnosticSHA256='+cinemaDiagnosticSHA256.toString()+';const build='+build.toString()+';onmessage=async e=>{try{postMessage({value:await build(e.data)})}catch(_){postMessage({error:true})}}';url=URL.createObjectURL(new Blob([source],{type:'application/javascript'}));worker=new Worker(url);}catch(_){if(url)URL.revokeObjectURL(url);url=null;worker=null;}
    }
    let result;
    if(worker) {
      try { result=await new Promise((resolve,reject)=>{let settled=false;const finish=(value,error)=>{if(settled)return;settled=true;clearTimeout(timeout);signal?.removeEventListener('abort',cancel);error?reject(error):resolve(value);};const timeout=setTimeout(()=>finish(null,new Error('Report export timed out')),15000);const cancel=()=>finish(null,new Error('Report cancelled'));signal?.addEventListener('abort',cancel,{once:true});if(signal?.aborted){cancel();return;}worker.onmessage=e=>finish(e.data.value,e.data.error?new Error('Report export failed'):null);worker.onerror=()=>finish(null,new Error('Report worker unavailable'));worker.postMessage(input);}); }
      finally {worker.terminate();URL.revokeObjectURL(url);}
    } else result=await build(input,signal);
    if(signal?.aborted)throw new Error('Report cancelled');CinemaDiagnosticWire.manifest(JSON.stringify(result.manifest));
    return result;
  }
  async function digest(value,signal=null) {const bytes=value instanceof Blob?await value.arrayBuffer():value;const hash=new Uint8Array(await cinemaDiagnosticSHA256(bytes,signal));return Array.from(hash,v=>v.toString(16).padStart(2,'0')).join('');}
  function save(blob,id) {const url=URL.createObjectURL(blob),link=document.createElement('a');link.href=url;link.download='cinema-report-'+id+'.zip';link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}
  function emergency() {
    if(document.getElementById('diagnostic-emergency'))return;
    const root=document.createElement('div');root.id='diagnostic-emergency';root.className='connectdialog';root.setAttribute('role','dialog');root.setAttribute('aria-modal','true');
    const card=document.createElement('div');card.className='card';const heading=document.createElement('h2');heading.textContent='Save a startup report';const detail=document.createElement('p');detail.textContent='The full app could not start. This local report contains only bounded typed startup observations and your description. It is not uploaded.';
    const description=document.createElement('textarea');description.maxLength=4000;description.setAttribute('aria-label','Description');const button=document.createElement('button');button.type='button';button.textContent='Save diagnostic ZIP';const close=document.createElement('button');close.type='button';close.textContent='Close';close.addEventListener('click',()=>root.remove());
    button.addEventListener('click',async()=>{button.disabled=true;try{const capture=CinemaDiagnostics.freeze();const manifest={version:CinemaDiagnosticWire.version,report_id:capture.id,captured_at:capture.captured_at,category:'interface',scope:'anonymous',client:{platform:'web',app_version:'unknown',build:'unknown',os_version:'browser',device_model:'browser'},capture:{mode:'ordinary',window_ms:capture.window_ms,dropped_events:capture.dropped,persistence:capture.persistence},components:[{name:'client',state:'partial',reason:'context_unavailable'},{name:'server',state:'unavailable',reason:'context_unavailable'},{name:'platform',state:'unavailable',reason:'unsupported'}],privacy:{titles_included:false,attachments_included:false},files:[]};const report=await archive(manifest,{'description.txt':description.value,'client-events.jsonl':capture.events.map(e=>JSON.stringify(e)).join('\n')+(capture.events.length?'\n':'')});save(report.blob,capture.id);detail.textContent='Diagnostic ZIP saved. Attach it to your bug or issue report.';}catch(_){detail.textContent='Reporting code is unavailable. Reload this page to retry; no report was uploaded.';}finally{button.disabled=false;}});
    card.append(heading,detail,description,button,close);root.append(card);document.body.append(root);description.focus();
  }
  return Object.freeze({archive,digest,save,emergency});
})();
