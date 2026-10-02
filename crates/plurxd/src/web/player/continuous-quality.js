"use strict";
function continuousQualityIdentity(value){
  return typeof value==='string'&&/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value);
}
function continuousQualityArtifact(value){return typeof value==='string'&&/^[0-9a-f]{64}$/.test(value);}
function continuousQualityInteger(value,minimum=0){return Number.isSafeInteger(value)&&value>=minimum;}
function continuousQualityInterval(value){
  return !!value&&continuousQualityArtifact(value.artifact_id)&&continuousQualityArtifact(value.rendition_id)
    &&continuousQualityInteger(value.timescale,1)&&value.timescale<=1000000
    &&continuousQualityInteger(value.from_tick)&&continuousQualityInteger(value.through_tick,1)
    &&value.from_tick<value.through_tick&&continuousQualityInteger(value.byte_length,1)
    &&value.byte_length<=256*1024*1024;
}
function continuousQualityTransaction(value){
  const arrays=['ready','reserved','appended'];
  return !!value&&continuousQualityIdentity(value.transaction_id)&&continuousQualityInteger(value.intent_revision,1)
    &&continuousQualityArtifact(value.target_rendition_id)
    &&['preparing','ready','scheduled','appended','presented','cancelling','retained_current','superseded','recovery_owned','disposed'].includes(value.state)
    &&typeof value.intent_superseded==='boolean'&&typeof value.cancel_requested==='boolean'
    &&typeof value.ever_appended==='boolean'
    &&arrays.every(key=>Array.isArray(value[key])&&value[key].length<=64
      &&value[key].every(row=>continuousQualityInterval(row)&&row.rendition_id===value.target_rendition_id)
      &&new Set(value[key].map(row=>row.artifact_id)).size===value[key].length)
    &&value.appended.every(row=>value.reserved.some(pin=>continuousQualitySameInterval(pin,row)))
    &&Array.isArray(value.disposed)&&value.disposed.length<=64&&value.disposed.every(continuousQualityArtifact)
    &&(value.first_presented_tick==null?value.first_presented_at_ms==null:
      continuousQualityInteger(value.first_presented_tick)&&continuousQualityInteger(value.first_presented_at_ms,1))
    &&(value.ever_appended||(!value.appended.length&&value.first_presented_tick==null));
}
function continuousQualitySameInterval(a,b){
  return ['artifact_id','rendition_id','timescale','from_tick','through_tick','byte_length'].every(key=>a[key]===b[key]);
}
function continuousQualitySameAttachment(a,b){
  return !!a&&!!b&&['client_instance_id','lifetime_id','attachment_id','family_id'].every(key=>a[key]===b[key]);
}
function continuousQualityResponse(value,request){
  if(!value||value.version!==1||value.generation!==request.generation
    ||value.control_epoch!==request.control_epoch||!continuousQualitySameAttachment(value.attachment,request.attachment)
    ||!continuousQualityInteger(value.revision,1))return false;
  const ledger=value.ledger;
  if(!ledger||ledger.version!==1||ledger.generation!==request.generation||ledger.control_epoch!==request.control_epoch
    ||!continuousQualitySameAttachment(ledger.attachment,request.attachment)
    ||!continuousQualityInteger(ledger.accepted_sequence)||!continuousQualityInteger(ledger.latest_intent_revision)
    ||!Array.isArray(ledger.transactions)||ledger.transactions.length>16
    ||!ledger.transactions.every(row=>continuousQualityTransaction(row)&&row.intent_revision<=ledger.latest_intent_revision)
    ||new Set(ledger.transactions.map(row=>row.transaction_id)).size!==ledger.transactions.length)return false;
  const audio=ledger.shared_audio_reserved||[];
  if(!Array.isArray(audio)||audio.length>64||!audio.every(row=>continuousQualityInterval(row)
    &&row.timescale===48000&&row.rendition_id===ledger.shared_audio_rendition_id)
    ||new Set(audio.map(row=>row.artifact_id)).size!==audio.length)return false;
  const pins=ledger.transactions.flatMap(row=>row.reserved).concat(audio);
  if(pins.length>64||pins.reduce((sum,row)=>sum+row.byte_length,0)>256*1024*1024)return false;
  const receipt=value.receipt,transition=request.transition;
  if(!transition)return receipt==null;
  if(!receipt||receipt.version!==1||receipt.generation!==request.generation
    ||receipt.control_epoch!==request.control_epoch||!continuousQualitySameAttachment(receipt.attachment,request.attachment)
    ||receipt.accepted_sequence!==transition.sequence||ledger.accepted_sequence<transition.sequence
    ||!continuousQualityTransaction(receipt.transaction)||receipt.transaction.transaction_id!==transition.transaction_id)return false;
  return transition.operation.kind!=='prepare'||(receipt.transaction.intent_revision===transition.operation.intent_revision
    &&receipt.transaction.target_rendition_id===transition.operation.target_rendition_id);
}
function continuousQualityFamily(value){
  if(!value||value.version!==1||value.mode!=='autonomous_reserved'||!continuousQualityArtifact(value.family_id)
    ||value.master!=='master.m3u8'||!Array.isArray(value.video)||value.video.length<2||value.video.length>8)return false;
  const rows=value.video;
  if(!rows.every(row=>row&&typeof row.candidate_id==='string'&&/^[0-9a-f]{32}$/.test(row.candidate_id)
    &&continuousQualityArtifact(row.rendition_id)&&continuousQualityArtifact(row.init_id)
    &&row.codec==='avc1.640032'&&continuousQualityInteger(row.width,1)&&row.width<=8192
    &&continuousQualityInteger(row.height,1)&&row.height<=8192
    &&continuousQualityInteger(row.timescale,1)&&row.timescale<=1000000
    &&continuousQualityInteger(row.frame_ticks,1)&&continuousQualityInteger(row.segment_ticks,1)
    &&row.segment_ticks%row.frame_ticks===0&&continuousQualityInteger(row.peak_bps,1)
    &&row.playlist===`video/${row.rendition_id}/index.m3u8`))return false;
  if(new Set(rows.map(row=>row.rendition_id)).size!==rows.length
    ||new Set(rows.map(row=>row.candidate_id)).size!==rows.length
    ||new Set(rows.map(row=>`${row.width}x${row.height}`)).size!==rows.length
    ||!rows.every(row=>row.timescale===rows[0].timescale&&row.frame_ticks===rows[0].frame_ticks
      &&row.segment_ticks===rows[0].segment_ticks))return false;
  const audio=value.audio;
  return audio==null||(continuousQualityArtifact(audio.rendition_id)&&continuousQualityArtifact(audio.init_id)
    &&audio.codec==='mp4a.40.2'&&audio.timescale===48000&&continuousQualityInteger(audio.channels,1)&&audio.channels<=8
    &&continuousQualityInteger(audio.peak_bps,1)&&audio.playlist===`audio/${audio.rendition_id}/index.m3u8`
    &&!rows.some(row=>row.rendition_id===audio.rendition_id));
}
async function continuousQualityFetch(url,body,{signal=null,limit=270336,timeoutMs=14000}={}){
  const controller=new AbortController(),abort=()=>controller.abort();
  const timer=setTimeout(abort,timeoutMs);let reader=null;
  if(signal){if(signal.aborted)abort();else signal.addEventListener('abort',abort,{once:true});}
  try{
    const response=await fetch(url,{method:body?'POST':'GET',signal:controller.signal,
      cache:'no-store',headers:body?{'content-type':'application/json'}:{},body:body?JSON.stringify(body):null});
    if(!response.ok)throw Object.assign(new Error(`Continuous quality request refused (${response.status})`),{status:response.status});
    const declared=response.headers.get('content-length');
    if(declared!=null&&(!/^\d+$/.test(declared)||Number(declared)>limit))throw new Error('Continuous quality response bound');
    if(!response.body||!response.body.getReader)throw new Error('Continuous quality response stream missing');
    reader=response.body.getReader();const chunks=[];let length=0;
    for(;;){const {done,value}=await reader.read();if(done)break;length+=value.length;
      if(length>limit)throw new Error('Continuous quality response bound');chunks.push(value);}
    const bytes=new Uint8Array(length);let at=0;for(const chunk of chunks){bytes.set(chunk,at);at+=chunk.length;}
    return JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(bytes));
  }finally{
    clearTimeout(timer);if(signal)signal.removeEventListener('abort',abort);
    if(reader){try{await reader.cancel();}catch(e){}reader.releaseLock();}
  }
}
function continuousQualityProtocol(bootstrap,attachment,exchange=continuousQualityFetch){
  if(!bootstrap||!continuousQualityIdentity(bootstrap.generation)||!continuousQualityInteger(bootstrap.control_epoch,1)
    ||!continuousQualityIdentity(attachment.client_instance_id)||!continuousQualityIdentity(attachment.attachment_id)
    ||!continuousQualityArtifact(attachment.family_id)||typeof attachment.lifetime_id!=='string'
    ||!attachment.lifetime_id.length||attachment.lifetime_id.length>128||/[\x00-\x1f\x7f]/.test(attachment.lifetime_id))
    throw new Error('Continuous quality bootstrap identity');
  const identity={version:1,generation:bootstrap.generation,control_epoch:bootstrap.control_epoch,
    attachment:Object.freeze({...attachment})};
  let sequence=0,revision=0,ledger=null,tail=Promise.resolve(),pending=null,queued=0;
  async function send(request){
    let failure;
    // A lost acknowledgement retries exactly the same sequence and operation.
    // No newer command may cross an exchange whose durable outcome is unknown.
    for(let attempt=0;attempt<2;attempt++)try{
      const response=await exchange(bootstrap.schedule_url,request);
      if(!continuousQualityResponse(response,request)||response.revision<revision)throw new Error('Continuous quality response identity');
      revision=response.revision;ledger=response.ledger;
      sequence=Math.max(sequence,ledger.accepted_sequence);pending=null;return response;
    }catch(error){failure=error;if(error.status&&error.status<500&&error.status!==429)break;}
    throw failure;
  }
  function queue(make){
    if(queued>=32)return Promise.reject(new Error('Continuous quality exchange queue bound'));
    queued++;
    const result=tail.then(async()=>{
      if(pending)await send(pending);
      const request=make();pending=request;return send(request);
    });
    tail=result.catch(()=>{}).finally(()=>queued--);return result;
  }
  return {
    get ledger(){return ledger;},get revision(){return revision;},get pending(){return pending;},
    snapshot:()=>queue(()=>({...identity,transition:null,frontier:null})),
    transition:(transactionId,operation,frontier=null)=>{
      const retained=JSON.parse(JSON.stringify({operation,frontier}));
      return queue(()=>{
        const next=sequence+1;if(!continuousQualityInteger(next,1))throw new Error('Continuous quality sequence exhausted');
        return {...identity,frontier:retained.frontier,transition:{...identity,sequence:next,transaction_id:transactionId,operation:retained.operation}};
      });
    },
    window:(transactionId,frontier)=>{
      const retained={...frontier};return queue(()=>({...identity,transition:null,frontier:null,
        window:{transaction_id:transactionId,frontier:retained}}));
    },
  };
}
