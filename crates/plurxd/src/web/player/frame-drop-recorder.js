"use strict";

// Observations only. Never registers a frame callback or controls transport.
const FRAME_DROP_LIMITS=Object.freeze({windowMs:60000,postMs:10000,samples:144,frames:512,
  postFrames:128,events:64,resources:16,bytes:49152,cooldownMs:60000,incidents:3});
const FRAME_DROP_EVENT=Object.freeze({play:1,pause:2,seeking:3,seeked:4,waiting:5,playing:6,
  stalled:7,loadedmetadata:8,loadeddata:9,canplay:10,emptied:11,error:12,ratechange:13,
  visibilitychange:14,focus:15,blur:16,longtask:17,counter_reset:18});
/**
 * @typedef {Object} FrameDropRecorderOptions
 * @property {() => number} now
 * @property {() => number} wall
 * @property {(incident: Object) => void} emit
 * @property {() => {attachment_id?: string}} [context]
 * @property {() => unknown[]} [resources]
 * @property {{count: number, last: number}} [budget]
 * @property {() => string} id
 */
/** @param {FrameDropRecorderOptions} options */
function createFrameDropRecorder({now,wall,emit,context=()=>({}),resources=()=>[],budget={count:0,last:-Infinity},id}){
  const limits=FRAME_DROP_LIMITS;
  const numeric=value=>typeof value==="number"&&Number.isFinite(value)&&Math.abs(value)<=Number.MAX_SAFE_INTEGER
    ?Math.round(value*1000)/1000:null;
  const clockOrigin=numeric(wall()-now());
  let samples=[],frames=[],events=[],pending=null,previous=null,changedAt=null,lastTick=null,stopped=false,pendingEvents=0;
  const overflow=[0,0,0,0];
  function trim(rows,max,at,index){
    while(rows.length&&(rows.length>max||rows[0][0]<at-limits.windowMs)){rows.shift();overflow[index]++;}
  }
  function append(which,row,max,index){
    const at=now();which.push(row);trim(which,max,at,index);
  }
  function event(code,detail=null,at=now()){
    if(stopped||!Number.isInteger(code)||code<1||code>18)return;
    const row=[numeric(at),code,numeric(detail)];append(events,row,limits.events,2);
    if(pending){if(pendingEvents++<limits.events)pending.events.push(row.slice());else pending.trimmed[2]++;}
  }
  function frame(callbackNow,meta){
    if(stopped)return;
    const at=now();
    const row=[numeric(at),numeric(callbackNow),numeric(meta?.mediaTime==null?null:meta.mediaTime*1000),
      numeric(meta?.presentedFrames),numeric(meta?.expectedDisplayTime),numeric(meta?.presentationTime),
      numeric(meta?.processingDuration==null?null:meta.processingDuration*1000)];
    append(frames,row,limits.frames,1);
    if(pending){if(pending.post_frames.length<limits.postFrames)pending.post_frames.push(row.slice());else pending.trimmed[3]++;}
  }
  function finish(reason="complete"){
    if(!pending)return null;
    const incident=pending;pending=null;
    const extra=resources();incident.resources=Array.isArray(extra)?extra.slice(-limits.resources):[];
    incident.end_ms=numeric(now());incident.end_reason=reason;
    // Keep a complete JSON document. Disclose removed rows instead of cutting bytes.
    let bytes=JSON.stringify(incident).length;
    while(bytes>limits.bytes){
      const reduce=(rows,index,keep=0)=>{const n=Math.min(rows.length-keep,Math.max(1,Math.ceil(rows.length*(1-limits.bytes/bytes))+8));rows.splice(0,n);incident.trimmed[index]+=n;};
      if(incident.frames.length)reduce(incident.frames,1);
      else if(incident.post_frames.length)reduce(incident.post_frames,3);
      else if(incident.samples.length>1)reduce(incident.samples,0,1);
      else break;
      bytes=JSON.stringify(incident).length;
    }
    try{emit(incident);}catch(_){}return incident;
  }
  function sample(s){
    if(stopped)return;
    const at=now(),total=numeric(s.total),dropped=numeric(s.dropped),corrupted=numeric(s.corrupted);
    const row=[numeric(at),numeric(s.time==null?null:s.time*1000),total,dropped,corrupted,
      numeric(s.ready),numeric(s.flags),numeric(s.runway==null?null:s.runway*1000),
      lastTick==null?null:numeric(Math.max(0,at-lastTick-500)),numeric(s.receipts),numeric(s.seek)];
    lastTick=at;append(samples,row,limits.samples,0);
    if(pending&&pending.samples.length<limits.samples+24)pending.samples.push(row.slice());
    if(total!=null&&dropped!=null&&total>=0&&dropped>=0){
      const reset=previous&&(total<previous.total||dropped<previous.dropped);
      if(reset){finish("counter_reset");event(FRAME_DROP_EVENT.counter_reset);changedAt=at;}
      if(previous&&!reset&&dropped>previous.dropped){
        if(pending)pending.drop_to=dropped;
        else if(budget.count<limits.incidents&&at-budget.last>=limits.cooldownMs){
          budget.count++;budget.last=at;pendingEvents=0;
          pending={version:1,id:id(),attachment_id:context().attachment_id,
            clock_origin_ms:clockOrigin,trigger_ms:numeric(at),observed_from_ms:numeric(changedAt??previous.at),
            drop_from:previous.dropped,drop_to:dropped,end_ms:null,end_reason:"complete",
            samples:samples.map(x=>x.slice()),frames:frames.map(x=>x.slice()),post_frames:[],
            events:events.map(x=>x.slice()),resources:[],trimmed:overflow.slice(),
            long_tasks_supported:!!s.longTasksSupported};
        }
      }
      if(!previous||reset||total!==previous.total||dropped!==previous.dropped||corrupted!==previous.corrupted)changedAt=at;
      previous={total,dropped,corrupted,at};
    }
    if(pending&&at-pending.trigger_ms>=limits.postMs)finish();
  }
  function stop(reason="stopped",discard=false){
    if(stopped)return;stopped=true;if(!discard)finish(reason);pending=null;samples=[];frames=[];events=[];
  }
  return {sample,frame,event,finish,stop,pending:()=>pending};
}

// Separate bounded store: never competes with consented support archives.
// Last three incidents for the current account, expiring after 24 hours.
const FrameDropIncidentStore=(()=>{
  let database=null,memory=[];
  function open(){
    if(database)return database;
    database=new Promise(resolve=>{
      try{
        if(typeof indexedDB==="undefined")return resolve(null);
        const request=indexedDB.open("cinema-frame-drop-v1",1);
        request.onupgradeneeded=()=>request.result.createObjectStore("incidents",{keyPath:"id"});
        request.onsuccess=()=>resolve(request.result);request.onerror=request.onblocked=()=>resolve(null);
      }catch(_){resolve(null);}
    });return database;
  }
  async function operate(owner,record=null,clear=false,current=()=>true){
    const at=Date.now(),db=await open();if(!current())return [];
    const retain=stored=>{
      const rows=clear?[]:stored.filter(x=>x.owner===owner&&x.expires>at&&x.id!==record?.id);
      if(record)rows.push(record);
      rows.sort((a,b)=>a.expires-b.expires||a.id.localeCompare(b.id));return rows.slice(-3);
    };
    const fallback=()=>{if(!current())return [];memory=retain(memory);return memory.slice();};
    if(!db)return fallback();
    return new Promise(resolve=>{try{
      const transaction=db.transaction("incidents","readwrite"),store=transaction.objectStore("incidents"),get=store.getAll();let rows=[];
      get.onsuccess=()=>{
        if(!current()){transaction.abort();return;}
        rows=retain([...get.result.filter(row=>!memory.some(local=>local.id===row.id)),...memory]);memory=rows.slice();store.clear();rows.forEach(row=>store.put(row));
      };
      transaction.oncomplete=()=>resolve(current()?rows:[]);
      transaction.onabort=transaction.onerror=()=>resolve(fallback());
    }catch(_){resolve(fallback());}});
  }
  return {operate};
})();
let ACTIVE_FRAME_DROP_RECORDER=null,FRAME_DROP_PRIVACY_EPOCH=0;
const FRAME_DROP_UPLOADS=new Set(),FRAME_DROP_UPLOAD_IDS=new Set();
let FRAME_DROP_HISTORY_ENABLED=frameDropRecorderEnabled();
function frameDropRecorderEnabled(){
  try{return localStorage.getItem("cinema_diagnostic_history")!=="off";}catch(_){return true;}
}
function frameDropResourceRows(v,started,transportUrl=null){
  try{
    if(typeof qualityResourceTimingRows==="undefined")return [];
    const source=new URL(transportUrl||v.currentSrc||v.src,location.href),playlist=/\.m3u8$/.test(source.pathname);
    const prefix=source.pathname.slice(0,source.pathname.lastIndexOf("/")+1);
    if(playlist&&prefix==="/")return [];
    const rows=[];
    for(const value of qualityResourceTimingRows.values()){
      const url=new URL(value.name,location.href);
      if(url.origin!==source.origin||value.startTime<started||value.responseEnd<=0)continue;
      if(playlist?!url.pathname.startsWith(prefix):url.pathname!==source.pathname)continue;
      const segment=url.pathname.match(/\/seg(\d+)\.m4s$/);
      const kind=segment?1:/\/init\.mp4$/.test(url.pathname)?0:/\.m3u8$/.test(url.pathname)?2:3;
      rows.push([kind,segment?Number(segment[1]):null,value.startTime,value.responseStart,
        value.responseEnd,value.encodedBodySize,value.transferSize].map(x=>typeof x==="number"&&Number.isFinite(x)&&Math.abs(x)<=Number.MAX_SAFE_INTEGER?Math.round(x*1000)/1000:null));
    }return rows.slice(-FRAME_DROP_LIMITS.resources);
  }catch(_){return [];}
}
function stopFrameDropRecorder(p,reason="stopped",discard=false){
  const active=ACTIVE_FRAME_DROP_RECORDER;
  if(!active||p&&active.player!==p)return;
  ACTIVE_FRAME_DROP_RECORDER=null;active.dispose(reason,discard);
}
function startFrameDropRecorder(v,p){
  try{
    if(!p||!v||PLAYER!==p||document.getElementById("video")!==v||!playbackOwnsAttachedMedia(p))return;
    if(ACTIVE_FRAME_DROP_RECORDER?.player===p&&ACTIVE_FRAME_DROP_RECORDER.attachment===p.mediaAttachment
      &&ACTIVE_FRAME_DROP_RECORDER.element===v)return;
    stopFrameDropRecorder(null,"replaced");
    FRAME_DROP_HISTORY_ENABLED=frameDropRecorderEnabled();
    p.frameDropAuthGeneration=AUTH_GENERATION;
    if(!TOKEN||!ME||!p.mediaAttachment||!FRAME_DROP_HISTORY_ENABLED||!playbackOwnsAttachedMedia(p))return;
    const privacy=FRAME_DROP_PRIVACY_EPOCH,generation=AUTH_GENERATION,owner=String(ME.id),attachment=p.mediaAttachment,started=performance.now();
    const authorized=()=>!!TOKEN&&String(ME?.id)===owner&&AUTH_GENERATION===generation&&FRAME_DROP_HISTORY_ENABLED&&privacy===FRAME_DROP_PRIVACY_EPOCH;
    const startup=p.hlsStartup;
    const transportUrl=p.probeUrl||(startup?.mediaAttachment===attachment?startup.playlistUrl:(v.currentSrc||v.src));
    const current=()=>authorized()&&PLAYER===p&&p.mediaAttachment===attachment
      &&document.getElementById("video")===v&&playbackOwnsAttachedMedia(p);
    // Every field is frozen here. A post-tail/close must not borrow a successor's identity.
    const base={event:"frame_drop",level:"warn",message:"Automatic frame-drop incident",
      file_id:p.fileId||null,session_id:p.sessionId||p.sharedDirect?.session_id||null,
      attempt:p.attemptId||null,method:p.method||null,ua:browserLabel()};
    async function submit(row,reserved=false){
      if(!authorized()){if(reserved)FRAME_DROP_UPLOAD_IDS.delete(row.id);return;}
      if(!reserved){if(FRAME_DROP_UPLOAD_IDS.has(row.id))return;FRAME_DROP_UPLOAD_IDS.add(row.id);}
      try{
        const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),8000);FRAME_DROP_UPLOADS.add(controller);
        try{
          const response=await fetch(API+"/client-log",{method:"POST",signal:controller.signal,
            headers:{"content-type":"application/json",authorization:"Bearer "+TOKEN},body:JSON.stringify(row.body)});
          if(response.ok&&authorized())await FrameDropIncidentStore.operate(owner,{...row,submitted:true},false,authorized);
        }finally{clearTimeout(timer);FRAME_DROP_UPLOADS.delete(controller);}
      }catch(_){}finally{FRAME_DROP_UPLOAD_IDS.delete(row.id);} // Retry retained failures on the next attachment.
    }
    const emit=incident=>{
      if(!authorized())return;
      const row={id:incident.id,owner,expires:Date.now()+86400000,submitted:false,body:{...base,frame_drop:incident}};
      FRAME_DROP_UPLOAD_IDS.add(row.id);
      void FrameDropIncidentStore.operate(owner,row,false,authorized).then(()=>submit(row,true));
    };
    const recorder=createFrameDropRecorder({now:()=>performance.now(),wall:()=>Date.now(),emit,
      context:()=>({attachment_id:attachment.id}),resources:()=>frameDropResourceRows(v,started,transportUrl),
      budget:p.frameDropBudget||(p.frameDropBudget={count:0,last:-Infinity}),
      id:()=>"fd-"+(typeof crypto.randomUUID==="function"?crypto.randomUUID():Date.now().toString(36)+"-"+Math.random().toString(36).slice(2))});
    let observer=null,longTasksSupported=false;
    const listener=e=>{if(current())recorder.event(FRAME_DROP_EVENT[e.type]);};
    const media=["play","pause","seeking","seeked","waiting","playing","stalled","loadedmetadata","loadeddata","canplay","emptied","error","ratechange"];
    media.forEach(name=>v.addEventListener(name,listener));document.addEventListener("visibilitychange",listener);
    window.addEventListener("focus",listener);window.addEventListener("blur",listener);
    if(typeof PerformanceObserver!=="undefined"&&PerformanceObserver.supportedEntryTypes?.includes("longtask")){
      try{observer=new PerformanceObserver(list=>{if(current())for(const entry of list.getEntries())recorder.event(17,entry.duration,entry.startTime);});observer.observe({type:"longtask"});longTasksSupported=true;}catch(_){observer?.disconnect();observer=null;}
    }
    ACTIVE_FRAME_DROP_RECORDER={player:p,attachment,element:v,recorder,current,longTasksSupported,
      dispose:(reason,discard)=>{media.forEach(name=>v.removeEventListener(name,listener));document.removeEventListener("visibilitychange",listener);
        window.removeEventListener("focus",listener);window.removeEventListener("blur",listener);observer?.disconnect();recorder.stop(reason,discard);}};
    void FrameDropIncidentStore.operate(owner,null,false,authorized).then(rows=>{const pending=rows.find(x=>!x.submitted);if(pending)void submit(pending);});
  }catch(_){} // Diagnostics must not prevent an attachment or timer from starting.
}
function sampleFrameDropRecorder(v,p){
  try{
    if((!ACTIVE_FRAME_DROP_RECORDER||ACTIVE_FRAME_DROP_RECORDER.player===p&&ACTIVE_FRAME_DROP_RECORDER.element!==v)
      &&FRAME_DROP_HISTORY_ENABLED&&p?.frameDropAuthGeneration===AUTH_GENERATION)startFrameDropRecorder(v,p);
    const active=ACTIVE_FRAME_DROP_RECORDER;if(!active||active.player!==p||active.element!==v)return;
    if(!active.current()){stopFrameDropRecorder(p,"replaced",!FRAME_DROP_HISTORY_ENABLED);return;}
    const q=v.getVideoPlaybackQuality?.();let runway=null;
    try{runway=bufferRunway(v);}catch(_){}
    const flags=(v.paused?1:0)|(v.seeking?2:0)|(document.hidden?4:0)|(document.hasFocus()?8:0)|(p.waitAt?16:0);
    active.recorder.sample({time:v.currentTime,total:q?.totalVideoFrames,dropped:q?.droppedVideoFrames,
      corrupted:q?.corruptedVideoFrames,ready:v.readyState,flags,runway,receipts:p.controlPresentedFrames,
      seek:p.controlSeek?.sequence??null,longTasksSupported:active.longTasksSupported});
  }catch(_){}
}
function recordFrameDropFrame(v,p,now,meta){
  try{const active=ACTIVE_FRAME_DROP_RECORDER;if(active&&active.player===p&&active.element===v&&active.current())active.recorder.frame(now,meta);}catch(_){}
}
function clearFrameDropRecorder(){
  FRAME_DROP_PRIVACY_EPOCH++;
  stopFrameDropRecorder(null,"disabled",true);
  for(const controller of FRAME_DROP_UPLOADS)controller.abort();FRAME_DROP_UPLOADS.clear();
  void FrameDropIncidentStore.operate(null,null,true);
}
window.addEventListener("cinema-diagnostic-auth",()=>{
  stopFrameDropRecorder(null,"auth_changed",true);
  for(const controller of FRAME_DROP_UPLOADS)controller.abort();FRAME_DROP_UPLOADS.clear();
  void FrameDropIncidentStore.operate(TOKEN&&ME?String(ME.id):null,null,!TOKEN||!ME);
});
function frameDropHistoryChanged(){FRAME_DROP_HISTORY_ENABLED=frameDropRecorderEnabled();if(!FRAME_DROP_HISTORY_ENABLED)clearFrameDropRecorder();}
window.addEventListener("cinema-diagnostic-history",frameDropHistoryChanged);
window.addEventListener("cinema-diagnostic-clear",clearFrameDropRecorder);
window.addEventListener("storage",e=>{if(e.key==="cinema_diagnostic_history")frameDropHistoryChanged();});
window.addEventListener("pagehide",()=>stopFrameDropRecorder(null,"pagehide"));
