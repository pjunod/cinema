"use strict";

// This owner retries only an explicitly consented frozen report. No player API.
function createCinemaDiagnosticDelivery({ state, persist, current, signal }) {
  const assertCurrent=()=>{if(signal.aborted||!current())throw new Error('Report operation ended');};
  function origin(value) {
    const url=new URL(value);if(url.username||url.password||url.search||url.hash||url.pathname!=='/'||!(url.protocol==='https:'||(url.protocol==='http:'&&['localhost','127.0.0.1','[::1]'].includes(url.hostname))))throw new Error('A fixed HTTPS support origin is required');
    return url.origin;
  }
  async function request(path,body=null,grant=null,method='POST',extra={}) {
    assertCurrent();const destination=origin(state.origin);const timeout=new AbortController();const abort=()=>timeout.abort();signal.addEventListener('abort',abort,{once:true});const timer=setTimeout(abort,10000);
    try {
      const response=await fetch(destination+'/support/v1/'+path,{method,credentials:'omit',redirect:'error',cache:'no-store',referrerPolicy:'no-referrer',signal:timeout.signal,headers:{...(body!==null&&!(body instanceof Blob)?{'Content-Type':'application/json'}:{}),...(grant?{Authorization:'Bearer '+grant}:{}),...extra},body:body===null?undefined:body instanceof Blob?body:JSON.stringify(body)});
      assertCurrent();if(response.status===204)return{};
      if(Number(response.headers.get('content-length')||0)>262144)throw new Error('Support reply too large');
      const reader=response.body.getReader();const chunks=[];let length=0;
      try{for(;;){const row=await reader.read();if(row.done)break;length+=row.value.length;if(length>262144)throw new Error('Support reply too large');chunks.push(row.value);}}finally{reader.cancel().catch(()=>{});reader.releaseLock();}
      const bytes=new Uint8Array(length);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.length;}
      const value=CinemaDiagnosticWire.strict(new TextDecoder().decode(bytes),262144);
      if(!response.ok){const error=Object.assign(new Error('Support request refused'),{status:0,code:'unavailable',retryable:false});error.status=response.status;error.code=typeof value.error==='string'?value.error:'unavailable';error.retryable=!!value.retryable;throw error;}
      assertCurrent();return value;
    } catch(error) {if(!error.status)error.retryable=!signal.aborted;throw error;}finally{clearTimeout(timer);signal.removeEventListener('abort',abort);}
  }
  async function remember() {assertCurrent();await persist(state);assertCurrent();}
  async function grant() {
    if(state.upload_grant)return;
    let result;
    if(state.enrollment)result=await request('grants',state.binding,state.enrollment);
    else {
      const response=await api('/diagnostics/upload-grants',{method:'POST',body:state.binding,signal});assertCurrent();
      const selected=origin(response.origin);if(state.origin&&origin(state.origin)!==selected)throw new Error('Support destination changed');state.origin=selected;result=response.grant;
    }
    if(!/^[a-f0-9]{64}$/.test(result.upload_grant))throw new Error('Invalid support grant');state.upload_grant=result.upload_grant;state.grant_expires=Date.now()+result.expires_in_ms;await remember();
  }
  async function stageAttempt() {
    await grant();
    if(!state.management_grant){const value=await request('uploads',state.binding,state.upload_grant);if(!/^[a-f0-9]{64}$/.test(value.management_grant)||value.upload_id!==state.binding.report_id||value.chunk_bytes!==1048576)throw new Error('Invalid support reservation');state.management_grant=value.management_grant;await remember();}
    let progress=await request('uploads/'+state.binding.report_id+'/status',null,state.management_grant,'GET');
    if(['staged','submitted'].includes(progress.state)){state.state=progress.state;state.reference=progress.reference;await remember();return progress;}
    if(Date.now()>=state.grant_expires){const value=await request('uploads/'+state.binding.report_id+'/refresh',null,state.management_grant);state.upload_grant=value.upload_grant;state.grant_expires=Date.now()+value.expires_in_ms;await remember();}
    const received=new Set(progress.received||[]);
    for(let index=0;index<Math.ceil(state.archive.size/1048576);index++){
      assertCurrent();if(received.has(index))continue;
      const chunk=state.archive.slice(index*1048576,(index+1)*1048576),digest=await CinemaDiagnosticExport.digest(chunk,signal);assertCurrent();
      await request('uploads/'+state.binding.report_id+'/chunks/'+index,chunk,state.upload_grant,'PUT',{'X-Chunk-Sha256':digest});state.state='uploading';await remember();
    }
    const complete=await request('uploads/'+state.binding.report_id+'/complete',null,state.upload_grant);state.state=complete.state;await remember();return complete;
  }
  async function stage() {
    if(!state.consented)throw new Error('Upload consent is required');
    for(let attempt=0;attempt<5;attempt++){
      try{return await stageAttempt();}catch(error){assertCurrent();if(!error.retryable||document.hidden||attempt===4)throw error;
        await new Promise((resolve,reject)=>{const abort=()=>{clearTimeout(timer);reject(new Error('Report operation ended'));};const timer=setTimeout(()=>{signal.removeEventListener('abort',abort);resolve();},Math.min(8000,500*2**attempt)+Math.floor(Math.random()*300));signal.addEventListener('abort',abort,{once:true});});}
    }
  }
  async function submit(description) {
    await stage();if(state.state==='submitted')return{state:'submitted',reference:state.reference};
    const value=await request('reports/'+state.binding.report_id+'/submit',{description},state.management_grant);state.state=value.state;state.reference=value.reference;await remember();return value;
  }
  async function offer() {await stage();if(state.state==='submitted')return{state:'submitted',reference:state.reference};const value=await request('handoffs',{report_id:state.binding.report_id},state.management_grant);state.handoff=value;await remember();return value;}
  async function handoffStatus() {return request('handoffs/'+state.handoff.id+'/status',null,state.management_grant,'GET');}
  async function approve(comparison) {return request('handoffs/'+state.handoff.id+'/approve',{comparison},state.management_grant);}
  async function cancel(keep=false) {if(state.handoff)await request('handoffs/'+state.handoff.id,{keep_draft:keep},state.management_grant,'DELETE');else if(state.management_grant&&!keep)await request('reports/'+state.binding.report_id,null,state.management_grant,'DELETE');state.handoff=null;if(!keep){state.state='deleted';state.management_grant=null;state.upload_grant=null;}await remember();}
  return Object.freeze({stage,submit,offer,handoffStatus,approve,cancel,request,origin});
}
