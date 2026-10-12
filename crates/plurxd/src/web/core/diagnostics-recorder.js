"use strict";

function createCinemaDiagnosticRecorder({ now = () => Date.now(), monotonic = () => performance.now(), store = null } = {}) {
  const attachments=new WeakMap();
  const counters = ['frame_count', 'dropped_frames', 'dropped_events'];
  const measurements = ['duration_ms', 'playhead_ms', 'headroom_ms', 'width', 'height', 'retry_count', 'status'];
  const enums = {
    source_codec: ['unknown','h264','hevc','av1','vp9','aac','ac3','eac3','dts','truehd','opus','flac','pcm','other'],
    delivered_codec: ['unknown','h264','hevc','av1','vp9','aac','ac3','eac3','dts','truehd','opus','flac','pcm','other'],
    audio_codec: ['unknown','h264','hevc','av1','vp9','aac','ac3','eac3','dts','truehd','opus','flac','pcm','other'],
    source_range: ['unknown','sdr','hdr10','hlg','dv'], delivered_range: ['unknown','sdr','hdr10','hlg','dv'], render_range: ['unknown','sdr','hdr10','hlg','dv'],
    mode: ['unknown','direct','remux','transcode'],
    failure: [...CinemaDiagnosticWire.reasons,'network','tls','dns','connect','authentication','http','decode','delivery','process_exit','browser_network'],
    reason: [...CinemaDiagnosticWire.reasons,'network','tls','dns','connect','authentication','http','decode','delivery','process_exit','browser_network'],
    action: ['play','pause','stop','seek','audio','subtitle','quality','navigate','report','reproduce','cancel','check'],
    state: ['unknown','starting','playing','paused','waiting','stalled','recovered','failed','stopped','foreground','background','interrupted','clean_shutdown'],
    route: ['auth','browse','playback','settings','diagnostics','support','other'], method: ['GET','POST','PUT','DELETE','HEAD'],
    phase: ['dns','connect','tls','auth','readiness','delivery','unavailable'],
  };
  function uuid() {
    const values = new Uint8Array(16);
    if (window.crypto && window.crypto.getRandomValues) window.crypto.getRandomValues(values);
    else for (let index = 0; index < 16; index++) values[index] = Math.floor(Math.random() * 256);
    values[6] = (values[6] & 15) | 64; values[8] = (values[8] & 63) | 128;
    const hex = Array.from(values, v => v.toString(16).padStart(2, '0')).join('');
    return hex.slice(0,8)+'-'+hex.slice(8,12)+'-'+hex.slice(12,16)+'-'+hex.slice(16,20)+'-'+hex.slice(20);
  }
  function facts(input) {
    const result = {};
    for (const [key, value] of Object.entries(input || {})) {
      if (enums[key] && enums[key].includes(value)) result[key] = value;
      else if (measurements.includes(key) && Number.isInteger(value) && value >= 0 && value <= (['width','height'].includes(key) ? 16384 : key === 'status' ? 599 : key === 'retry_count' ? 1000 : 604800000)) result[key] = value;
      else if (counters.includes(key) && typeof value === 'string' && CinemaDiagnosticWire.decimal(value)) result[key] = value;
      else if (key === 'observed' && typeof value === 'boolean') result[key] = value;
      else if (key === 'correlation' && typeof value === 'string' && /^s-[a-f0-9]{64}$/.test(value)) result[key] = value;
      else if (key === 'node' && typeof value==='string' && /^node-[1-8]$/.test(value)) result[key] = value;
    }
    return Object.fromEntries(Object.entries(result).slice(0,16));
  }
  let rows = [], bytes = 0, dropped = 0n, sequence = 0n, scope = 'anonymous', epoch = 0, flushTimer = null, enabled = true;
  let completedReproduction = null, reproduction = null, reproductionTimer = null, hydrated = false;
  try{if(localStorage.getItem('cinema_diagnostic_history')==='off')enabled=false;}catch(_){}
  const started = monotonic(), run = uuid();
  function trim() {
    const floor = now() - 600000;
    while (rows.length && rows[0].at < floor) bytes -= rows.shift().bytes;
    while (rows.length > 10000 || bytes > 2097152) { let index = rows.findIndex(row => row.event.kind === 'sample'); if (index < 0) index = 0; const row = rows.splice(index,1)[0]; bytes -= row.bytes; if(dropped<18446744073709551615n)dropped++; }
  }
  function record(domain,kind,input={}){try{return writeEvent(domain,kind,input);}catch(_){return false;}}
  function writeEvent(domain, kind, input = {}) {
    if (!['ui','network','playback','producer','lifecycle','coverage'].includes(domain) || !['action','state','sample','error','terminal','request','availability'].includes(kind)) return false;
    if(reproduction && monotonic()>=reproduction.deadline)stopReproduction();
    if (!enabled && !reproduction) return false;
    const event = { run_id:run, sequence:String(sequence++), utc:new Date(now()).toISOString().slice(0,19)+'Z', elapsed_ms:Math.min(604800000,Math.max(0,Math.floor(monotonic()-started))), domain, kind, facts:facts(input) };
    const size = JSON.stringify(event).length + 1;
    if (kind === 'sample' && (rows.length >= 8000 || bytes + size > 1677721)) { if(dropped<18446744073709551615n)dropped++; return false; }
    rows.push({ at:now(), bytes:size, event }); bytes += size; trim();
    if (reproduction && now() < reproduction.ends) { reproduction.rows.push(event); reproduction.bytes += size; while (reproduction.rows.length > 10000 || reproduction.bytes > 2097152) { let index=reproduction.rows.findIndex(e=>e.kind==='sample'); if(index<0)index=0; reproduction.bytes-=JSON.stringify(reproduction.rows.splice(index,1)[0]).length+1; reproduction.dropped++; } }
    if (store && !flushTimer) flushTimer = setTimeout(() => { flushTimer = null; persist(); }, 3000);
    return true;
  }
  async function persist() {
    if (!store) return;
    const owner = epoch, key = 'ring:' + scope; trim();
    const value={ key, type:'ring', at:now(), expires:now()+600000, bytes, rows:rows.slice(), dropped:String(dropped), enabled, run };
    if (owner === epoch) await store.put(value);
  }
  async function bind(nextScope) {
    if (nextScope === scope && hydrated) return;
    const previous = scope, fresh = !hydrated && scope==='anonymous' ? rows.slice() : []; const owner = ++epoch; clearTimeout(flushTimer); flushTimer = null;
    scope = /^[a-f0-9]{64}$/.test(nextScope) ? nextScope : 'anonymous'; rows=fresh; bytes=fresh.reduce((n,r)=>n+r.bytes,0); dropped=0n; reproduction=null; completedReproduction=null; clearTimeout(reproductionTimer); reproductionTimer=null; hydrated=false;
    const marker=store ? await store.get('owner') : null;if(owner!==epoch)return;
    if(store && marker && marker.scope!==scope)await store.clear();if(owner!==epoch)return;
    if(store)await store.put({key:'owner',type:'owner',at:now(),expires:now()+86400000,bytes:64,scope});if(owner!==epoch)return;
    const value=store ? await store.get('ring:' + scope) : null;
    if (owner !== epoch) return;
    if (value && value.expires > now() && Array.isArray(value.rows) && value.rows.length <= 10000) {
      const recovered=[];
      for (const row of value.rows) {
        const e=row.event;
        if (!e || !CinemaDiagnosticWire.uuid(e.run_id) || !CinemaDiagnosticWire.decimal(e.sequence) || !['ui','network','playback','producer','lifecycle','coverage'].includes(e.domain) || !['action','state','sample','error','terminal','request','availability'].includes(e.kind) || typeof e.utc!=='string' || !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/.test(e.utc) || !Number.isInteger(e.elapsed_ms) || e.elapsed_ms<0 || e.elapsed_ms>604800000) continue;
        const clean={run_id:e.run_id,sequence:e.sequence,utc:e.utc,elapsed_ms:e.elapsed_ms,domain:e.domain,kind:e.kind,facts:facts(e.facts)};
        const size=JSON.stringify(clean).length+1; if(size>2048)continue;
        recovered.push({at:Date.parse(clean.utc),bytes:size,event:clean});
      }
      // New events observed during the database read retain their current run order.
      rows=recovered.concat(rows); bytes=rows.reduce((n,r)=>n+r.bytes,0); trim();
      if (CinemaDiagnosticWire.decimal(value.dropped)) dropped=BigInt(value.dropped)>18446744073709551615n-dropped?18446744073709551615n:dropped+BigInt(value.dropped);
    }
    hydrated=true; record('lifecycle','availability',{phase:'unavailable',reason:'unsupported',observed:false});
  }
  function freeze() { trim(); record('ui','action',{action:'report'}); return { id:uuid(), captured_at:new Date(now()).toISOString().slice(0,19)+'Z', events:rows.map(row=>structuredClone(row.event)), dropped:String(dropped), window_ms:600000, persistence:store && store.persistent() ? 'available' : 'memory_only', history_enabled:enabled, epoch, scope }; }
  function startReproduction(onComplete=null) { if(reproduction)return reproduction.id; const id=uuid();reproduction={id,ends:now()+300000,rows:[],bytes:0,dropped:0n,deadline:monotonic()+300000};record('ui','action',{action:'reproduce'});reproductionTimer=setTimeout(()=>{const capture=stopReproduction();if(onComplete)onComplete(capture);},300000);return id; }
  function stopReproduction() { if(!reproduction)return completedReproduction;const capture=reproduction;reproduction=null;clearTimeout(reproductionTimer);reproductionTimer=null;
    const terminal={run_id:run,sequence:String(sequence++),utc:new Date(now()).toISOString().slice(0,19)+'Z',elapsed_ms:Math.min(604800000,Math.max(0,Math.floor(monotonic()-started))),domain:'ui',kind:'terminal',facts:{action:'stop',state:'stopped'}};capture.rows.push(terminal);capture.bytes+=JSON.stringify(terminal).length+1;
    while(capture.rows.length>10000||capture.bytes>2097152){let i=capture.rows.findIndex(e=>e.kind==='sample');if(i<0)i=0;capture.bytes-=JSON.stringify(capture.rows.splice(i,1)[0]).length+1;capture.dropped++;}
    completedReproduction={id:capture.id,captured_at:new Date(now()).toISOString().slice(0,19)+'Z',events:capture.rows.slice(),dropped:String(capture.dropped),window_ms:300000,persistence:store&&store.persistent()?'available':'memory_only',mode:'reproduction',epoch,scope};if(store)store.put({key:'reproduction:'+scope,type:'capture',at:now(),expires:now()+86400000,bytes:capture.bytes,capture:completedReproduction});return completedReproduction; }
  function disable(value) { enabled=!value;try{localStorage.setItem('cinema_diagnostic_history',enabled?'on':'off');}catch(_){} if(!enabled){rows=[];bytes=0;}persist(); }
  function clear() { epoch++;rows=[];bytes=0;dropped=0n;reproduction=null;completedReproduction=null;clearTimeout(reproductionTimer);reproductionTimer=null;clearTimeout(flushTimer);flushTimer=null;if(store)store.clear(); }
  return Object.freeze({attach:(attachment,generation)=>{if(attachment&&typeof attachment==='object')attachments.set(attachment,{generation,correlation:null,source:null});},owns:(attachment,generation)=>!!attachment&&attachments.get(attachment)?.generation===generation,correlation:(attachment,generation)=>attachments.get(attachment)?.generation===generation?attachments.get(attachment).correlation:null,correlate:async(attachment,generation,source)=>{const owner=attachments.get(attachment);if(!owner||owner.generation!==generation||typeof source!=='string'||source.length>256||owner.source===source)return;owner.source=source;owner.correlation=null;const marker=epoch;try{const bytes=new Uint8Array(await cinemaDiagnosticSHA256(new TextEncoder().encode(source)));if(marker===epoch&&attachments.get(attachment)===owner&&owner.source===source)owner.correlation='s-'+Array.from(bytes,b=>b.toString(16).padStart(2,'0')).join('');}catch(_){}}, record, freeze, persist, bind, clear, disable, startReproduction, stopReproduction, uuid, facts, current:()=>({epoch,scope,enabled,completed:completedReproduction,reproduction:reproduction ? {id:reproduction.id,ends:reproduction.ends} : null}) });
}

const CinemaDiagnostics = createCinemaDiagnosticRecorder({ store:CinemaDiagnosticStore });
window.CinemaDiagnostics = CinemaDiagnostics;
window.addEventListener('pagehide',()=>{ CinemaDiagnostics.record('lifecycle','state',{state:'background'});CinemaDiagnostics.persist(); });
document.addEventListener('visibilitychange',()=>CinemaDiagnostics.record('lifecycle','state',{state:document.hidden?'background':'foreground'}));
