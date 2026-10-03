"use strict";
const CONTINUOUS_QUALITY_MAX_INTERVALS=128;
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
    &&arrays.every(key=>Array.isArray(value[key])&&value[key].length<=CONTINUOUS_QUALITY_MAX_INTERVALS
      &&value[key].every(row=>continuousQualityInterval(row)&&row.rendition_id===value.target_rendition_id)
      &&new Set(value[key].map(row=>row.artifact_id)).size===value[key].length)
    &&value.appended.every(row=>value.reserved.some(pin=>continuousQualitySameInterval(pin,row)))
    &&Array.isArray(value.disposed)&&value.disposed.length<=CONTINUOUS_QUALITY_MAX_INTERVALS&&value.disposed.every(continuousQualityArtifact)
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
    ||!continuousQualityInteger(value.revision,1)
    ||(value.terminal!=null&&typeof value.terminal!=='boolean')
    ||(value.terminal&&(request.window||request.frontier||['prepare','scheduled'].includes(request.transition?.operation.kind))))return false;
  const ledger=value.ledger;
  if(!ledger||ledger.version!==1||ledger.generation!==request.generation||ledger.control_epoch!==request.control_epoch
    ||!continuousQualitySameAttachment(ledger.attachment,request.attachment)
    ||!continuousQualityInteger(ledger.accepted_sequence)||!continuousQualityInteger(ledger.latest_intent_revision)
    ||!Array.isArray(ledger.transactions)||ledger.transactions.length>16
    ||!ledger.transactions.every(row=>continuousQualityTransaction(row)&&row.intent_revision<=ledger.latest_intent_revision)
    ||new Set(ledger.transactions.map(row=>row.transaction_id)).size!==ledger.transactions.length)return false;
  const audio=ledger.shared_audio_reserved||[];
  if(!Array.isArray(audio)||audio.length>CONTINUOUS_QUALITY_MAX_INTERVALS||!audio.every(row=>continuousQualityInterval(row)
    &&row.timescale===48000&&row.rendition_id===ledger.shared_audio_rendition_id)
    ||new Set(audio.map(row=>row.artifact_id)).size!==audio.length)return false;
  const pins=ledger.transactions.flatMap(row=>row.reserved).concat(audio);
  if(pins.length>CONTINUOUS_QUALITY_MAX_INTERVALS||pins.reduce((sum,row)=>sum+row.byte_length,0)>256*1024*1024)return false;
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
  if(!value||value.version!==1||!['autonomous_reserved','controlled'].includes(value.mode)||!continuousQualityArtifact(value.family_id)
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
    if(!response.ok){
      const retry=response.headers.get('retry-after');
      const retryAfterMs=retry!=null&&/^\d+$/.test(retry)?Math.min(1000,Number(retry)*1000):1000;
      throw Object.assign(new Error(`Continuous quality request refused (${response.status})`),
        {status:response.status,retryAfterMs});
    }
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
function continuousQualityProtocol(bootstrap,attachment,exchange=continuousQualityFetch,
  wait=ms=>new Promise(resolve=>setTimeout(resolve,ms))){
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
    }catch(error){
      failure=error;if(error.status&&error.status<500&&error.status!==429)break;
      if(error.status===429&&attempt===0)await wait(Math.max(1,Math.min(1000,error.retryAfterMs||1000)));
    }
    throw failure;
  }
  function own(action){
    if(queued>=32)return Promise.reject(new Error('Continuous quality exchange queue bound'));
    queued++;const result=tail.then(action);
    tail=result.catch(()=>{}).finally(()=>queued--);return result;
  }
  function queue(make){return own(async()=>{
    if(pending)await send(pending);
    const request=make();pending=request;return send(request);
  });}

  return {
    get ledger(){return ledger;},get revision(){return revision;},get pending(){return pending;},
    recover:()=>own(async()=>{if(pending)await send(pending);return ledger;}),
    reconcileTerminal:()=>own(async()=>{
      const request={...identity,transition:null,frontier:null};
      const response=await exchange(bootstrap.schedule_url,request);
      if(response?.terminal!==true||!continuousQualityResponse(response,request)||response.revision<revision)
        throw new Error('Continuous reservation reconciliation needs durable End proof');
      // End prevents late Prepare/Scheduled writes. Use a sequence beyond an
      // uncertain completed-fact request too, so its later settlement cannot
      // overtake the detach disposal that follows this proof.
      sequence=Math.max(sequence,response.ledger.accepted_sequence,pending?.transition?.sequence||0);
      ledger=response.ledger;revision=response.revision;pending=null;return ledger;
    }),
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

function continuousQualityNewIdentity(){
  const bytes=new Uint8Array(16);crypto.getRandomValues(bytes);bytes[6]=(bytes[6]&15)|64;bytes[8]=(bytes[8]&63)|128;
  const hex=Array.from(bytes,byte=>byte.toString(16).padStart(2,'0')).join('');
  return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
}
function continuousQualityCandidate(player,selection){
  const quality=selection?.quality;
  if(!quality||quality.mode==='original')return null;
  const catalog=player.qualityCandidates||[];
  return quality.candidate_id?catalog.find(row=>row.id===quality.candidate_id):
    catalog.find(row=>row.route==='encode'&&row.target_height===(quality.height||player.autoRequestedHeight||player.autoHeight));
}
function continuousQualitySelectionCompatible(player,selection){
  const controller=player.continuousQuality,base=player.continuousQualityBootstrap?.selection;
  if(!controller||controller.closed||!base)return false;
  if(!['audio_track','audio_offset_ms','codec','dynamic_range'].every(key=>selection[key]===base[key])
    ||JSON.stringify(selection.subtitle)!==JSON.stringify(base.subtitle))return false;
  const candidate=continuousQualityCandidate(player,selection);
  return !!candidate&&controller.family.video.some(row=>row.candidate_id===candidate.id);
}
async function openContinuousQualitySession(fileId,body,player,signal){
  if(!player||player.sessionId||player.libraryChannel||body.transport!=='hlsjs'
    ||body.copy===true||body.hdr10===true||body.subtitle_burn!=null||!body.caps
    ||!window.Hls||!Hls.isSupported())return null;
  if(playbackControlSelection(player).quality.mode==='original')return null;
  let catalog;
  try{catalog=await api(`/files/${fileId}/hls/continuous-candidates`,{method:'POST',body:{version:1,start:body},signal});}
  catch(error){if(error.status===404||error.status===405)return null;throw error;}
  if(catalog?.version!==1||!Array.isArray(catalog.candidates)||catalog.candidates.length>32
    ||!Array.isArray(catalog.pairs)||catalog.pairs.length>32)throw new Error('Continuous candidate catalog shape');
  const requested=body.intent?.selection?.quality?.candidate_id||player.abr?.requestedCandidateId||player.qualityCandidateId;
  const candidate=catalog.candidates.find(row=>row.id===requested&&row.target_height===body.height)
    ||catalog.candidates.find(row=>row.route==='encode'&&row.target_height===body.height);
  if(!candidate)return null;
  const pair=catalog.pairs.find(row=>row.primary_candidate_id===candidate.id);
  if(!pair||!/^[0-9a-f]{32}$/.test(pair.primary_candidate_id)||!/^[0-9a-f]{32}$/.test(pair.companion_candidate_id)
    ||pair.primary_candidate_id===pair.companion_candidate_id)return null;
  const selection=playbackControlSelection(player);
  selection.quality=selection.quality.mode==='manual'?{mode:'manual',height:candidate.target_height}:
    {mode:'auto',height:candidate.target_height,candidate_id:candidate.id};
  const start={...body,intent:qualityMediaIntent(player,selection,true),quality_auto:selection.quality.mode==='auto'};
  const attemptKey=body.request_id;
  const attempts=player.continuousQualityStarts||(player.continuousQualityStarts=new Map());
  if(!attempts.has(attemptKey)){
    if(attempts.size>=16)attempts.delete(attempts.keys().next().value);
    attempts.set(attemptKey,continuousQualityNewIdentity());
  }
  let response;
  try{response=await api(`/files/${fileId}/hls/continuous-sessions`,{method:'POST',signal,
    body:{version:1,controlled:true,family_generation:attempts.get(attemptKey),...pair,start}});}
  catch(error){if(error.status===404||error.status===405)return null;throw error;}
  const playback=response?.playback,bootstrap=response?.quality;
  try{
    if(response.version!==1||!playback||!continuousQualityIdentity(playback.session_id)
      ||!bootstrap||!continuousQualityIdentity(bootstrap.generation)||!continuousQualityInteger(bootstrap.control_epoch,1)
      ||bootstrap.schedule_url!==`/api/v1/hls/${playback.session_id}/quality-schedule`
      ||bootstrap.family_url!==`/api/v1/hls/${playback.session_id}/quality-family`)
      throw new Error('Continuous family bootstrap shape');
    const family=await continuousQualityFetch(bootstrap.family_url,null,{signal,limit:32768});
    if(!continuousQualityFamily(family)||!family.video.some(row=>row.candidate_id===candidate.id)
      ||!family.video.some(row=>row.candidate_id===pair.companion_candidate_id))throw new Error('Continuous family catalog binding');
    return {...playback,quality_candidates:catalog.candidates,
      continuous_quality:{...bootstrap,family,primary_candidate_id:candidate.id,selection}};
  }catch(error){if(playback?.session_id)releaseSession(playback.session_id);throw error;}
}
// Attachment-owned hls.js adapter. Optional choices affect future loads only;
// already exposed or appended bytes retain their fact/disposal owner.
function continuousQualityAdapter(player,video,attachment,bootstrap,exchange=continuousQualityFetch){
  const family=bootstrap.family;
  if(!continuousQualityFamily(family))throw new Error('Continuous quality family metadata');
  const holder=player.abr||player;
  const lifetime=holder.mediaIntent?.lifetimeId||newRequestId();
  if(!holder.mediaIntent)holder.mediaIntent={lifetimeId:lifetime,recipeRevision:0,transportRevision:0};
  const protocol=continuousQualityProtocol(bootstrap,{client_instance_id:CONTROL_CLIENT_ID,
    lifetime_id:lifetime,attachment_id:continuousQualityNewIdentity(),family_id:family.family_id},exchange);
  const readers=new Map(),records=new Map(),buffers=new Set(),loaders=new Set();
  let hls=null,closed=false,mediaDetached=false,transaction=null,revision=0,frontier=0,frameToken=null;
  const pendingAppends=new Map(),pendingDisposals=new Set();
  let disposalTimer=null,seekRevision=0,seekBoundary=null;
  let work=Promise.resolve(),queued=0;
  const current=()=>!closed&&attachment.current()&&player.continuousQuality===adapter;
  let wanted=family.video.find(row=>row.candidate_id===bootstrap.primary_candidate_id);
  if(!wanted)throw new Error('Continuous quality initial candidate');
  const origin=new URL(bootstrap.schedule_url,location.href).origin;
  const parentPath=new URL(bootstrap.schedule_url,location.href).pathname.replace(/quality-schedule$/,'');
  function resource(url){
    const parsed=new URL(url,location.href);
    if(parsed.origin!==origin||!parsed.pathname.startsWith(parentPath))return null;
    const match=/^(video|audio)\/([0-9a-f]{64})\/(?:init\/([0-9a-f]{64})\.mp4|segment\/(\d+)\.m4s)$/.exec(parsed.pathname.slice(parentPath.length));
    if(!match)return null;
    const row=match[1]==='video'?family.video.find(row=>row.rendition_id===match[2]):family.audio;
    if(!row||row.rendition_id!==match[2]||(match[3]&&row.init_id!==match[3]))throw new Error('Continuous media outside family');
    return {row,type:match[1],init:!!match[3]};
  }
  function serial(action){
    if(queued>=32)return Promise.reject(new Error('Continuous media work queue bound'));
    queued++;const result=work.then(action);work=result.catch(()=>{}).finally(()=>queued--);return result;
  }
  function note(error){
    player.continuousQualityObservation=String(error?.message||error).slice(0,160);
    player.continuousQualityObservationStack=String(error?.stack||'').slice(0,2048);
  }
  function tx(id=transaction){return protocol.ledger?.transactions.find(row=>row.transaction_id===id);}
  function pins(){return (protocol.ledger?.transactions||[]).flatMap(row=>row.reserved)
    .concat(protocol.ledger?.shared_audio_reserved||[]);}
  async function prepare(row,through){
    transaction=continuousQualityNewIdentity();revision++;
    const response=await protocol.transition(transaction,{kind:'prepare',intent_revision:revision,
      target_rendition_id:row.rendition_id},{timescale:row.timescale,through_tick:through});
    const ready=response.ledger.transactions.find(row=>row.transaction_id===transaction);
    if(!ready||!ready.ready.length||ready.cancel_requested||ready.intent_superseded)throw new Error('Continuous target retained current');
    return ready;
  }
  async function reserveWindow(through){
    if(!transaction)await prepare(wanted,through);
    let ready=tx();
    if(!ready||ready.intent_superseded||ready.cancel_requested)throw new Error('Continuous target superseded');
    if(!ready.ready.some(row=>row.from_tick<=through&&through<row.through_tick)){
      await protocol.window(transaction,{timescale:wanted.timescale,through_tick:through});ready=tx();
    }
    if(!ready?.ready.length)throw new Error('Continuous target has no ready samples');
    if(ready.ready.some(row=>ready.disposed.includes(row.artifact_id)))ready=await prepare(wanted,through);
    const missing=ready.ready.filter(row=>!ready.reserved.some(pin=>continuousQualitySameInterval(pin,row)));
    if(missing.length)await protocol.transition(transaction,{kind:'scheduled',intervals:missing});
  }
  async function authorize(found,data,loadTransaction){
    if(!current())throw new Error('Continuous media attachment ended');
    if(!(data instanceof ArrayBuffer)&&!ArrayBuffer.isView(data))throw new Error('Continuous media payload type');
    const read=found.init?continuousMediaInspector():readers.get(found.row.rendition_id)||continuousMediaInspector();
    const inspection=read(data);
    if(found.init){
      if(inspection.fragments.length||inspection.initializations.length!==1
        ||await continuousMediaDigest(inspection.bytes)!==found.row.init_id)throw new Error('Continuous init identity');
      const track=inspection.initializations[0];
      if(track.timescale!==found.row.timescale||(found.type==='video'
        ?track.type!=='vide'||track.width!==found.row.width||track.height!==found.row.height
        :track.type!=='soun'||track.channels!==found.row.channels))throw new Error('Continuous init format');
      readers.set(found.row.rendition_id,read);return null;
    }
    if(!readers.has(found.row.rendition_id))throw new Error('Continuous fragment before verified init');
    const [facts,artifact]=await Promise.all([continuousSampleFacts(inspection),continuousMediaDigest(inspection.bytes)]);
    if(facts.timescale!==found.row.timescale||(found.type==='video'
      ?facts.type!=='vide'||facts.width!==found.row.width||facts.height!==found.row.height
      :facts.type!=='soun'||facts.channels!==found.row.channels))throw new Error('Continuous fragment format');
    const previousRecord=records.get(`${found.row.rendition_id}:${artifact}`);
    if(previousRecord?.removed)await flushDisposals(true);
    let pin=pins().find(row=>row.artifact_id===artifact&&row.rendition_id===found.row.rendition_id);
    if(!pin){
      if(found.type==='video'){
        if(found.row.rendition_id!==wanted.rendition_id)throw new Error('Unreserved superseded video load');
        await reserveWindow(facts.from_tick);
      }else{
        // Select the entry containing this AAC interval's actual start.
        // The owner's two-entry projection includes boundary-crossing AAC.
        // Selecting the preceding entry could reuse a ready window that ends
        // before this fragment and never reserve its immutable hash.
        const tick=Math.floor(facts.from_tick*wanted.timescale/facts.timescale);
        const entry=Math.floor(tick/wanted.segment_ticks)*wanted.segment_ticks;
        await reserveWindow(entry);
      }
      pin=pins().find(row=>row.artifact_id===artifact&&row.rendition_id===found.row.rendition_id);
    }
    if(!pin||pin.byte_length!==inspection.bytes.length||pin.timescale!==facts.timescale
      ||pin.from_tick!==facts.from_tick||pin.through_tick!==facts.through_tick)throw new Error('Continuous fragment is not reserved');
    if(!current())throw new Error('Continuous media attachment ended');
    const recordKey=`${pin.rendition_id}:${artifact}`;
    let record=records.get(recordKey);
    if(!record){
      if(records.size>=CONTINUOUS_QUALITY_MAX_INTERVALS)throw new Error('Continuous media provenance bound');
      record={interval:{...pin},facts,transactions:new Set(),type:found.type,
        exposed:false,appended:false,presented:false,removed:false,disposed:false};records.set(recordKey,record);
    }
    if(found.type==='video')for(const owner of protocol.ledger.transactions){
      if(owner.reserved.some(row=>row.artifact_id===artifact))record.transactions.add(owner.transaction_id);
    }
    // A loader's captured transaction is never substituted for server facts.
    if(loadTransaction&&found.type==='video'&&!record.transactions.size)throw new Error('Continuous fragment transaction lost');
    return record;
  }
  function bindBuffer(buffer,type){
    if(buffers.has(buffer))return;buffers.add(buffer);
    let inspect=continuousMediaInspector();
    const append=buffer.appendBuffer.bind(buffer),remove=buffer.remove.bind(buffer);
    let pending=null;
    const failed=()=>{if(pending)pending.failed=true;inspect=continuousMediaInspector();};
    const completed=()=>{
      const operation=pending;pending=null;if(!operation||operation.failed)return;
      const completedRemoval=operation.kind==='remove'?Array.from(records.values()).filter(row=>!row.disposed&&row.exposed&&row.type===type
        &&operation.from<=row.interval.from_tick/row.interval.timescale
        &&operation.through>=row.interval.through_tick/row.interval.timescale):[];
      // Capture absence at the actual completion, before later frame callbacks
      // or a loader can mistake this record for currently buffered media.
      for(const row of completedRemoval)row.removed=true;
      if(operation.kind==='append')serial(async()=>{
        const facts=await operation.facts;if(!facts)return;
        const matches=Array.from(records.values()).filter(row=>!row.disposed&&row.exposed&&row.type===type
          &&row.facts.fingerprint===facts.fingerprint&&row.facts.configuration_digest===facts.configuration_digest
          &&row.facts.sample_count===facts.sample_count
          &&row.facts.timescale===facts.timescale&&row.facts.from_tick===facts.from_tick
          &&row.facts.through_tick===facts.through_tick&&row.facts.width===facts.width&&row.facts.height===facts.height);
        if(matches.length!==1)throw new Error('Completed append has no exact reserved sample provenance');
        const record=matches[0];record.appended=true;
        if(type==='video'){
          frontier=Math.max(frontier,record.interval.through_tick);
          for(const owner of record.transactions){
            let pending=pendingAppends.get(owner);
            if(!pending){pending=new Set();pendingAppends.set(owner,pending);}
            pending.add(record);
            if(!tx(owner)?.ever_appended||pending.size>=2)await flushAppends(owner);
          }
        }
      }).catch(note);
      else serial(async()=>{
        for(const row of completedRemoval.filter(row=>row.appended&&!row.disposed))pendingDisposals.add(row);
        await flushDisposals(false);
      }).catch(note);
    };
    buffer.addEventListener('error',failed,true);buffer.addEventListener('abort',failed,true);
    // Observe completion before hls.js's ordinary updateend listener starts
    // its next queued append and replaces our pending operation.
    buffer.addEventListener('updateend',completed,true);
    buffer.appendBuffer=data=>{
      if(buffer.updating)return append(data);
      let facts;
      try{
        if(data.byteLength>16*1024*1024)throw new Error('Continuous append payload bound');
        const snapshot=new Uint8Array(data instanceof ArrayBuffer?data:new Uint8Array(data.buffer,data.byteOffset,data.byteLength)).slice();
        const inspection=inspect(snapshot);
        facts=inspection.fragments.length?continuousSampleFacts(inspection).catch(error=>{note(error);return null;}):Promise.resolve(null);
      }catch(error){note(error);facts=Promise.resolve(null);}
      const operation={kind:'append',facts,failed:false};pending=operation;
      try{return append(data);}catch(error){if(pending===operation)pending=null;inspect=continuousMediaInspector();throw error;}
    };
    buffer.remove=(from,through)=>{
      if(buffer.updating)return remove(from,through);
      const operation={kind:'remove',from,through,failed:false};pending=operation;
      try{return remove(from,through);}catch(error){if(pending===operation)pending=null;throw error;}
    };
  }
  async function flushAppends(owner=null){
    for(const [id,pending] of pendingAppends){
      if(owner&&id!==owner)continue;
      const rows=Array.from(pending);
      if(rows.length)await protocol.transition(id,{kind:'appended',intervals:rows.map(row=>row.interval)});
      for(const row of rows)pending.delete(row);
      if(!pending.size)pendingAppends.delete(id);
    }
  }
  async function flushDisposals(force){
    if(!pendingDisposals.size)return;
    if(!force&&pendingDisposals.size<4){
      if(disposalTimer==null)disposalTimer=setTimeout(()=>{
        disposalTimer=null;if(!closed)serial(()=>flushDisposals(true)).catch(note);
      },10000);
      return;
    }
    if(disposalTimer!=null){clearTimeout(disposalTimer);disposalTimer=null;}
    const rows=Array.from(pendingDisposals);
    await flushAppends();
    await dispose(rows);
    for(const row of rows)pendingDisposals.delete(row);
  }
  async function dispose(rows){
    const videoRows=rows.filter(row=>row.type==='video'),audioRows=rows.filter(row=>row.type==='audio');
    const owners=new Set(videoRows.flatMap(row=>Array.from(row.transactions)));
    if(audioRows.length&&transaction)owners.add(transaction);
    for(const owner of owners){
      const artifacts=videoRows.filter(row=>row.transactions.has(owner)).map(row=>row.interval.artifact_id);
      if(owner===transaction)artifacts.push(...audioRows.map(row=>row.interval.artifact_id));
      if(artifacts.length)await protocol.transition(owner,{kind:'disposed',artifacts});
    }
    for(const row of rows){row.disposed=true;records.delete(`${row.interval.rendition_id}:${row.interval.artifact_id}`);}
  }
  function frames(){
    if(closed||typeof video.requestVideoFrameCallback!=='function')return;
    frameToken=video.requestVideoFrameCallback((wall,metadata)=>{
      frameToken=null;if(!current()){frames();return;}
      const width=metadata.width||video.videoWidth,height=metadata.height||video.videoHeight;
      const matches=Array.from(records.values()).filter(row=>row.type==='video'&&row.appended&&!row.removed&&!row.disposed&&!row.presented
        &&row.facts.width===width&&row.facts.height===height
        &&metadata.mediaTime*row.interval.timescale>=row.interval.from_tick
        &&metadata.mediaTime*row.interval.timescale<row.interval.through_tick);
      if(matches.length===1){
        const record=matches[0],tick=Math.round(metadata.mediaTime*record.interval.timescale),observedAt=Date.now();
        if(tick>=record.interval.from_tick&&tick<record.interval.through_tick){
          record.presented=true;
          serial(async()=>{
            if(record.removed||record.disposed)return;
            for(const owner of record.transactions)if(tx(owner)?.first_presented_tick==null){
              await flushAppends(owner);
              await protocol.transition(owner,{kind:'presented',artifact_id:record.interval.artifact_id,
                film_tick:tick,observed_at_ms:observedAt});
            }
            if(current()){
              const row=family.video.find(row=>row.rendition_id===record.interval.rendition_id);
              player.qualityCandidateId=row.candidate_id;
              const candidate=(player.qualityCandidates||[]).find(candidate=>candidate.id===row.candidate_id);
              player.autoHeight=candidate?.target_height||row.height;
              player.continuousQualityPresented={candidate_id:row.candidate_id,width,height,film_tick:tick,timescale:row.timescale};
              if(player.directedChange?.outcome==='continuous')settleContinuousDirectedChange(player);
            }
          }).catch(note);
        }
      }
      frames();
    });
  }
  const adapter={
    protocol,family,get wanted(){return wanted;},get frontier(){return frontier;},get closed(){return closed;},
    loader:Base=>class {
      constructor(config){this.base=new Base(config);this.aborted=false;this.destroyed=false;this.context=null;loaders.add(this);}
      get stats(){return this.base.stats;}
      load(context,config,callbacks){
        this.context=context;const captured=transaction;
        const guarded={...callbacks,onProgress:()=>{},onSuccess:(response,stats,ctx,network)=>{
          if(this.aborted||this.destroyed||!current())return;
          let found;try{found=resource(ctx.url);}catch(error){callbacks.onError({code:0,text:error.message},ctx,network,stats);return;}
          if(!found){if(!this.aborted)callbacks.onSuccess(response,stats,ctx,network);return;}
          serial(()=>this.aborted||this.destroyed||!current()?null:authorize(found,response.data,captured)).then(record=>{
            if(this.aborted||!current())return;
            if(record){record.exposed=true;if(found.type==='video')frontier=Math.max(frontier,record.interval.through_tick);}
            callbacks.onSuccess(response,stats,ctx,network);
          }).catch(error=>{
            if(this.aborted||this.destroyed||!current())return;
            note(error);callbacks.onError({code:0,text:error.message},ctx,network,stats);
          });
        }};
        this.base.load(context,config,guarded);
      }
      abort(){
        if(this.aborted||this.destroyed)return;this.aborted=true;
        // The XHR may have finished while authorization is still queued.
        // hls.js leaves FRAG_LOADING only when the logical fragment stats say
        // it was aborted, even when there is no remaining network to cancel.
        this.base.stats.aborted=true;this.base.abort();
      }
      destroy(){
        if(this.destroyed)return;this.destroyed=true;this.aborted=true;loaders.delete(this);
        // hls.js resets a loader from its abort callback. Base destruction
        // cancels transport silently; invoking abort here re-enters that reset.
        this.base.destroy();
      }
      getCacheAge(){return this.base.getCacheAge?.()||null;}
      getResponseHeader(name){return this.base.getResponseHeader?.(name)||null;}
    },
    bind(instance,startAt){
      hls=instance;
      hls.on(Hls.Events.MANIFEST_PARSED,()=>{
        if(!current())return;
        const level=hls.levels.findIndex(level=>level.url.some(url=>new URL(url,location.href).pathname===parentPath+wanted.playlist));
        if(level<0){note(new Error('Continuous primary level missing'));return;}
        hls.loadLevel=level;hls.startLoad(startAt>0?startAt:-1);
      });
      hls.on(Hls.Events.BUFFER_CREATED,(event,data)=>{
        if(!current())return;
        for(const [type,track] of Object.entries(data.tracks||{}))if((type==='video'||type==='audio')&&track.buffer)bindBuffer(track.buffer,type);
      });
      hls.on(Hls.Events.MEDIA_DETACHED,(event,data)=>{
        if(data?.transferMedia)return; // A live transferred MediaSource is not disposal.
        mediaDetached=true;adapter.detached();
      });
      frames();
    },
    noteSeek(seconds){
      if(!current()||!Number.isFinite(seconds)||seconds<0)return false;
      // A seek changes the future-load interval, not ownership of old bytes.
      // Protect the contiguous exposed video at this destination; a disjoint
      // old forward range must not drag a backward seek's choice to its tail.
      let through=Math.floor(seconds*wanted.timescale/wanted.segment_ticks)*wanted.segment_ticks;
      if(!continuousQualityInteger(through))return false;
      if(through===seekBoundary)return true; // Identical frontier cannot renew preparation.
      seekBoundary=through;
      for(;;){
        const next=Array.from(records.values()).find(row=>row.type==='video'
          &&(row.exposed||row.appended)&&!row.removed&&!row.disposed
          &&row.interval.from_tick<=through&&through<row.interval.through_tick);
        if(!next)break;through=next.interval.through_tick;
      }
      frontier=through;seekRevision++;return true;
    },
    choose(candidateId,live=()=>true){return serial(async()=>{
      if(!current())return 'superseded';
      const next=family.video.find(row=>row.candidate_id===candidateId);if(!next)return 'outside_family';
      if(next.rendition_id===wanted.rendition_id)return 'continuous';
      const previous=wanted,old=transaction;
      try{
        await flushAppends();await flushDisposals(true);
        const level=hls.levels.findIndex(level=>level.url.some(url=>new URL(url,location.href).pathname===parentPath+next.playlist));
        if(level<0)throw new Error('Continuous target level missing');
        for(;;){
          const observedSeek=seekRevision;
          try{await prepare(next,frontier);}
          catch(error){
            if(!current()||!live()||observedSeek===seekRevision)throw error;
            // The old destination may have lost preparation demand while
            // this exchange was pending. Only a newer seek permits retry.
            continue;
          }
          if(!current()||!live()||observedSeek===seekRevision)break;
          await protocol.transition(transaction,{kind:'cancel_unappended',completed:[]});
        }
        if(!current())return 'superseded';
        if(!live()){
          await protocol.transition(transaction,{kind:'cancel_unappended',completed:[]});
          await prepare(previous,frontier);return 'superseded';
        }
        // Readiness succeeds before retiring optional old loaders. Payloads
        // already delivered keep their own append and disposal ownership.
        wanted=next;hls.loadLevel=level;
        for(const loader of loaders){let found;try{found=resource(loader.context?.url||'');}catch(e){}
          if(found?.type==='video'&&found.row.rendition_id===previous.rendition_id)loader.abort();}
        if(old)try{
          await protocol.transition(old,{kind:'cancel_unappended',completed:Array.from(records.values())
            .filter(row=>row.appended&&row.transactions.has(old)).map(row=>row.interval)});
          // Exact loader abort plus no onSuccess exposure is an absence
          // barrier for these video objects. Shared audio never follows it.
          const unexposed=(tx(old)?.reserved||[]).filter(pin=>!records.get(`${pin.rendition_id}:${pin.artifact_id}`)?.exposed);
          if(unexposed.length){
            await protocol.transition(old,{kind:'disposed',artifacts:unexposed.map(pin=>pin.artifact_id)});
            for(const pin of unexposed)records.delete(`${pin.rendition_id}:${pin.artifact_id}`);
          }
        }catch(error){note(error);}
        return 'continuous';
      }catch(error){
        note(error);wanted=previous;
        // A failed Prepare may already supersede the old optional transaction.
        // Restore future incumbent scheduling under a new revision, retaining
        // the old reservations/facts rather than trying to revive its intent.
        if(current()&&transaction!==old)try{await prepare(previous,frontier);}catch(restore){note(restore);}
        else transaction=old;
        return 'retained_current';
      }
    });},
    detached(){
      if(!mediaDetached)return Promise.reject(new Error('Continuous media disposal needs detach receipt'));
      if(closed)return;closed=true;
      if(disposalTimer!=null){clearTimeout(disposalTimer);disposalTimer=null;}
      if(frameToken!=null)try{video.cancelVideoFrameCallback(frameToken);}catch(e){}
      for(const loader of loaders)loader.abort();loaders.clear();
      // Called only after the hls.js MediaSource has detached. Include pins
      // never delivered to the loader, but never use End as this barrier.
      return serial(async()=>{
        try{await protocol.recover();}catch(error){await protocol.reconcileTerminal();}
        const ledger=protocol.ledger;if(!ledger)return;
        const audio=(ledger.shared_audio_reserved||[]).map(row=>row.artifact_id);
        const audioOwner=ledger.transactions.at(-1)?.transaction_id;
        for(const owner of ledger.transactions){
          const artifacts=owner.reserved.map(row=>row.artifact_id);
          if(owner.transaction_id===audioOwner)artifacts.push(...audio);
          if(artifacts.length)await protocol.transition(owner.transaction_id,{kind:'disposed',artifacts});
        }
        records.clear();buffers.clear();readers.clear();pendingAppends.clear();pendingDisposals.clear();
      }).catch(note);
    },
  };
  return adapter;
}
