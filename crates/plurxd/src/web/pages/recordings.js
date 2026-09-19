"use strict";
// ---- Recordings ----------------------------------------------------------
const DVR_PAGE={tab:"upcoming",rows:[],next:null,total:0,loading:false,error:null,
  selectedId:null,selected:null,events:[],eventsNext:null,historyComplete:false,truncatedBefore:null,
  detailError:null,detailBusy:false,channels:[],historyTimer:null,scheduleFrom:0,scheduleTo:0};
const DVR_TABS=[["upcoming","Upcoming"],["saved","Saved"],["attention","Needs attention"],
  ["rules","Series rules"],["manual","Manual recording"],["skipped","Skipped"]];
function recordingsRouteTab(hash=location.hash){
  const raw=(hash.split("/")[2]||"upcoming").split("?")[0];
  return ({library:"saved",scheduled:"upcoming"})[raw]||
    (DVR_TABS.some(([id])=>id===raw)?raw:"upcoming");
}
function recordingsRoute(){return `#/recordings/${DVR_PAGE.tab}`;}
function dvrSchedulePagePath(after){
  const cursor=after?`&after=${encodeURIComponent(after)}`:"";
  return `/dvr/schedule?from=${DVR_PAGE.scheduleFrom}&to=${DVR_PAGE.scheduleTo}&limit=50${cursor}`;
}
function dvrUpcomingRow(row){return ["scheduled","conflict","withdrawn","stale"].includes(row&&row.state);}
function dvrPagePath(tab,after){
  const suffix=after?`&after=${encodeURIComponent(after)}`:"";
  if(tab==="saved")return `/dvr/recordings?state=done,partial${suffix}`;
  if(tab==="attention")return `/dvr/attention?limit=50${after?`&after=${encodeURIComponent(after)}`:""}`;
  if(tab==="rules")return "/dvr/rules";
  if(tab==="skipped")return dvrSchedulePagePath(after);
  if(tab==="upcoming")return dvrSchedulePagePath(after);
  return null;
}
async function viewRecordings(generation=PAGE_RENDER_GENERATION){
  const nextTab=recordingsRouteTab();
  if(nextTab!==DVR_PAGE.tab||!LAST_ROUTE?.startsWith("#/recordings")){
    DVR_PAGE.selectedId=null;DVR_PAGE.selected=null;DVR_PAGE.events=[];
  }
  DVR_PAGE.tab=nextTab;DVR_PAGE.error=null;DVR_PAGE.loading=true;
  clearTimeout(DVR_PAGE.historyTimer);DVR_PAGE.historyTimer=null;
  layoutChrome("recordings",`<div class="dvr-page-head"><div><h1>Recordings</h1>
    <p class="sub">Your recordings, upcoming programmes and anything that needs attention.</p></div></div>
    <div class="empty" aria-busy="true">Loading recordings…</div>`);
  setPagePhase(location.hash,generation,"shell");
  const read=path=>api(path,{signal:AbortSignal.timeout(20000)});
  try{
    const now=Math.floor(Date.now()/1000);DVR_PAGE.scheduleFrom=now-43200;DVR_PAGE.scheduleTo=now+43200;
    const common=[read(dvrSchedulePagePath(null)),read("/dvr/reminders"),
      read("/live-tv/channels").catch(()=>({channels:[]}))];
    const path=DVR_PAGE.tab==="upcoming"?null:dvrPagePath(DVR_PAGE.tab,null);
    const [schedule,reminders,channels,answer]=await Promise.all([...common,path?read(path):Promise.resolve(null)]);
    if(generation!==PAGE_RENDER_GENERATION||!location.hash.startsWith("#/recordings"))return;
    LIVE_TV_DVR.schedule=schedule;LIVE_TV_DVR.reminders=Array.isArray(reminders)?reminders:[];
    DVR_PAGE.channels=Array.isArray(channels.channels)?channels.channels:[];
    if(DVR_PAGE.tab==="upcoming"){
      DVR_PAGE.rows=(schedule.rows||[]).filter(dvrUpcomingRow);DVR_PAGE.next=schedule.next||null;
    }else if(DVR_PAGE.tab==="manual"){DVR_PAGE.rows=[];DVR_PAGE.next=null;
    }else if(DVR_PAGE.tab==="rules"){
      if(!Array.isArray(answer))throw new Error("The series-rule response is invalid.");
      DVR_PAGE.rows=answer;DVR_PAGE.next=null;
    }else if(DVR_PAGE.tab==="attention"){
      if(!answer||!Array.isArray(answer.rows))throw new Error("The attention response is invalid.");
      DVR_PAGE.rows=answer.rows;DVR_PAGE.next=answer.next||null;DVR_PAGE.total=Number(answer.total)||0;
    }else{
      const page=dvrRecordingsPage(answer);DVR_PAGE.rows=DVR_PAGE.tab==="skipped"?page.rows.filter(row=>row.state==="cancelled"):page.rows;DVR_PAGE.next=page.next;
    }
    DVR_PAGE.error=null;
  }catch(e){DVR_PAGE.error=e&&e.message||"Recordings could not be loaded";}
  finally{
    DVR_PAGE.loading=false;
    if(generation===PAGE_RENDER_GENERATION&&location.hash.startsWith("#/recordings")){
      paintRecordingsPage();setPagePhase(location.hash,generation,"content");setPagePhase(location.hash,generation,"settled");
      const selected=new URLSearchParams((location.hash.split("?")[1]||"")).get("selected");
      if(selected&&DVR_PAGE.selectedId!==selected)selectDvrDetail(selected);
      else if(DVR_PAGE.selectedId)selectDvrDetail(DVR_PAGE.selectedId);
    }
  }
}
function dvrSummaryMarkup(){
  const overview=DVR_SHARED.overview,fresh=!DVR_SHARED.error&&dvrOverviewFresh(overview,DVR_SHARED.receivedAt),counts=fresh&&overview&&overview.counts||{};
  const metric=(value,label)=>`<div><b>${value==null?"—":esc(String(value))}</b><span>${esc(label)}</span></div>`;
  const availability=DVR_SHARED.error?`<div class="clrefusal" role="status"><b>Recording status is stale.</b> ${esc(DVR_SHARED.error)}</div>`
    :overview&&overview.availability!=="complete"?`<div class="clrefusal" role="status"><b>Recording status is incomplete.</b> Some recorders have not reported their current status.</div>`:"";
  return `${availability}<div class="dvr-summary">${metric(counts.recording,"Writing now")}${metric(counts.starting,"Starting")}${metric(counts.reconnecting,"Reconnecting")}${metric(counts.finishing,"Finishing")}${metric(counts.unconfirmed,"Status unavailable")}${metric(counts.attention,"Needs attention")}</div>`;
}
function dvrTabsMarkup(){
  return `<nav class="dvr-tabs" aria-label="Recording sections">${DVR_TABS.map(([id,label])=>
    `<a href="#/recordings/${id}" class="${DVR_PAGE.tab===id?'on':''}"${DVR_PAGE.tab===id?' aria-current="page"':''}>${esc(label)}${id==="attention"&&DVR_PAGE.total?` (${DVR_PAGE.total})`:""}</a>`).join("")}</nav>`;
}
function dvrOverviewRows(){
  const overview=DVR_SHARED.overview,rows=overview&&overview.active||[];
  if(!rows.length)return "";
  return `<div class="dvr-strip" aria-label="Active recordings"><b>Happening now</b>${rows.slice(0,4).map(row=>{const state=dvrRowPresentation(row);return `<a href="#/activity" onclick="openDvrActivity(${esc(JSON.stringify(row.recording_id))});return false">${dvrStatusMarkup(state.label,state.tone)} <b>${esc(row.title)}</b><span class="muted">${esc(state.bytes)}</span></a>`;}).join("")}${rows.length>4||overview.active_truncated?'<a href="#/activity">View all activity →</a>':""}</div>`;
}

function dvrSavedLabel(row){
  if(!["done","partial"].includes(row.state))return DVR_STATE_LABEL[row.state]||row.state||"Recording";
  if(row.stopped_early===true||row.stopped_by_user_id!=null)return row.state==="partial"||row.gap_s>0||row.late_start_s>0
    ?"Stopped early · incomplete":"Stopped early";
  if(row.state==="partial"||row.gap_s>0||row.late_start_s>0)return "Incomplete";
  return row.item_id&&row.file_id?"Recorded":"Recorded · preparing playback";
}
function dvrPageRowsMarkup(){
  if(DVR_PAGE.loading)return liveTvEmptyRecs("Loading recordings…");
  if(DVR_PAGE.error)return `<div class="clrefusal" role="status"><b>Showing the last successful data when available.</b> ${esc(DVR_PAGE.error)} <button class="ghost sm" onclick="viewRecordings(++PAGE_RENDER_GENERATION)">Retry</button></div>`;
  if(DVR_PAGE.tab==="manual")return dvrManualMarkup();
  if(!DVR_PAGE.rows.length)return liveTvEmptyRecs(DVR_PAGE.tab==="attention"?"Nothing needs review.":"Nothing in this section yet.");
  const rows=DVR_PAGE.rows.map(raw=>{
    const row=DVR_PAGE.tab==="attention"?raw.recording:raw;
    if(DVR_PAGE.tab==="rules"){
      const keep=row.keep_mode==="last_n"?`keep the last ${row.keep_value}`:row.keep_mode==="days"?`keep ${row.keep_value} days`:row.keep_mode==="until_watched"?"keep until watched":"keep everything";
      return `<div class="lt-rec"><span><b>${esc(row.name)}</b>${row.enabled?"":' <span class="pill">off</span>'}<span class="meta">${esc(row.match_value)} · ${esc(keep)} · priority ${row.priority}</span></span><span class="acts"><button onclick="liveTvDeleteRule(${esc(JSON.stringify(row.id))})">Delete</button></span></div>`;
    }
    const state=DVR_PAGE.tab==="saved"?dvrSavedLabel(row):(DVR_STATE_LABEL[row.state]||row.state);
    const actions=[];
    if(row.state==="cancelled")actions.push(`<button onclick="liveTvRestore(${esc(JSON.stringify(row.id))})">Restore</button>`);
    else if(row.state==="recording"&&!row.stop_requested_at_ms)actions.push(`<button onclick="liveTvStopRecording(${esc(JSON.stringify(row.id))},${esc(JSON.stringify(row.title))})">Stop…</button>`);
    else if(row.state==="done"||row.state==="partial")actions.push(`<button onclick="liveTvDeleteRecording(${esc(JSON.stringify(row.id))},${esc(JSON.stringify(row.title))})">Delete…</button>`);
    if(DVR_PAGE.tab==="saved")return `<article class="dvr-saved-card" data-recording-id="${esc(row.id)}"><button class="dvr-row-button" onclick="selectDvrDetail(${esc(JSON.stringify(row.id))})"><div class="dvr-saved-art"><small>${esc(row.guide_number||"TV")} · ${esc(row.channel_name||"Recorded television")}</small><strong>${esc(row.title)}</strong></div></button><div class="dvr-saved-body">${dvrStatusMarkup(state,row.state==="done"&&!row.stopped_early?"saved":"warning")}<p>${new Date(row.airing_start*1000).toLocaleDateString()} · ${esc(fmtBytes(row.bytes)||"Size unavailable")}${row.episode_title?`<br>${esc(row.episode_title)}`:""}</p><div class="row" style="flex-wrap:wrap">${row.item_id&&row.file_id?`<a class="ghost sm" href="#/item/${row.item_id}">Play recording</a>`:""}<button class="ghost sm" onclick="selectDvrDetail(${esc(JSON.stringify(row.id))})">Details</button></div></div></article>`;
    const attention=DVR_PAGE.tab==="attention"?`<span class="why">Review event ${raw.latest_attention_sequence||"legacy outcome"}</span>`:"";
    return `<div class="lt-rec" data-recording-id="${esc(row.id)}"><button class="dvr-row-button" onclick="selectDvrDetail(${esc(JSON.stringify(row.id))})"><b>${esc(row.title)}</b> <span class="pill">${esc(state)}</span>${liveTvRecordingLine(row)}${row.state_reason?`<span class="why">${esc(row.state_reason)}</span>`:""}${attention}</button><span class="acts">${row.item_id&&row.file_id?`<a class="ghost sm" href="#/item/${row.item_id}">Play</a>`:""}${actions.join("")}</span></div>`;
  }).join("");
  const more=DVR_PAGE.next?`<div class="row" style="justify-content:flex-end"><button class="ghost sm" onclick="loadMoreDvrPage()">Load more</button></div>`:"";
  return `<div class="${DVR_PAGE.tab==="saved"?"dvr-saved-grid":"lt-recs"}" style="height:auto">${rows}</div>${more}`;
}
function dvrManualMarkup(){
  const options=DVR_PAGE.channels.map(row=>`<option value="${esc(row.id)}">${esc(row.guide_number)} · ${esc(row.guide_name)}</option>`).join("");
  return `<form class="dvr-manual lt-recs" style="height:auto" onsubmit="submitManualRecording(event)"><label class="wide">Channel<select id="dvr-manual-channel" required>${options}</select></label><label>Capture starts<input id="dvr-manual-start" type="datetime-local" required></label><label>Capture ends<input id="dvr-manual-end" type="datetime-local" required></label><label class="wide">Title<input id="dvr-manual-title" maxlength="200" required placeholder="Programme title"></label><div class="wide"><button type="submit">Schedule recording</button></div></form>`;
}
function dvrEventLabel(kind){return ({scheduled:"Scheduled",schedule_changed:"Schedule changed",conflict:"No tuner",withdrawn:"Withdrawn",cancelled:"Skipped",restored:"Restored",attempt_started:"Capture attempt started",first_bytes_written:"First bytes written",capture_interrupted:"Capture interrupted",retry_started:"Retry started",stop_requested:"Stop requested",finishing:"Finishing",finished:"Capture finished",failed:"Capture failed",missed:"Capture missed",media_linked:"Playback prepared",deleted:"Deleted",legacy_outcome:"Review baseline (earlier details were not collected)"})[kind]||`Recording event · ${String(kind||"unknown").replace(/_/g," ")}`;}
function dvrDetailMarkup(){
  const row=DVR_PAGE.selected;
  if(!row)return `<aside class="dvr-detail${DVR_PAGE.selectedId?"":" empty-detail"}"><span class="eyebrow">Recording details</span><h2>${DVR_PAGE.detailBusy?"Loading recording…":"Every recording has a story."}</h2><p class="muted">${DVR_PAGE.detailError?esc(DVR_PAGE.detailError):"Select a recording to see write health, capture times and its event history."}</p>${DVR_PAGE.detailError?`<button class="ghost sm" onclick="selectDvrDetail(${esc(JSON.stringify(DVR_PAGE.selectedId))})">Retry</button>`:""}</aside>`;
  const active=(DVR_SHARED.overview&&DVR_SHARED.overview.active||[]).find(item=>item.recording_id===row.id);
  const state=active?dvrRowPresentation(active):{label:row.state==="recording"?"Status unavailable":dvrSavedLabel(row),tone:["failed","missed","partial","conflict"].includes(row.state)?"warning":row.state==="done"?"saved":"",fresh:false};
  const obs=active&&active.observation;
  const clientAge=DVR_SHARED.receivedAt?Math.max(0,performance.now()-DVR_SHARED.receivedAt):0;
  const lastWrite=state.fresh&&obs&&obs.last_write_age_ms!=null?`${Math.floor((obs.last_write_age_ms+clientAge)/1000)}s ago`:"Unavailable";
  const rate=state.fresh&&obs&&obs.write_bps!=null?`${fmtBytes(obs.write_bps)||"0 B"}/s`:"—";
  const total=active?state.bytes:`${fmtBytes(row.bytes)||"0 B"} written`;
  const attention=[...(ACTIVITY_DVR.attention||[]),...DVR_PAGE.rows].find(raw=>raw.recording&&raw.recording.id===row.id);
  const through=attention&&attention.latest_attention_sequence!=null?attention.latest_attention_sequence:0;
  const events=DVR_PAGE.events.map(event=>`<div class="dvr-event"><b>${esc(dvrEventLabel(event.kind))}</b>${event.reason_code?`<small>${esc(event.reason_code.replace(/_/g," "))}</small>`:""}<time>${new Date(event.occurred_at_ms).toLocaleString()}</time></div>`).join("");
  const metric=(value,label)=>`<div><b>${esc(value)}</b><span>${esc(label)}</span></div>`;
  const diagnostics=active&&active.can_view_diagnostics;
  const owner=diagnostics&&obs&&obs.owner_node_id;
  const ownerName=owner&&(ACTIVITY_SNAPSHOT&&ACTIVITY_SNAPSHOT.node_hostnames||{})[owner]||owner;
  const stop=active?active.can_stop&&!active.stop_requested_at_ms:row.state==="recording"&&!row.stop_requested_at_ms;
  return `<aside class="dvr-detail" aria-label="Recording details"><span class="eyebrow">Recording details</span><div class="row" style="justify-content:space-between;align-items:flex-start;gap:12px"><h2 style="margin:0">${esc(row.title)}</h2><button class="ghost sm" data-dvr-focus="detail-close" onclick="closeDvrDetail()">Close</button></div><p>${dvrStatusMarkup(state.label,state.tone)}</p>${liveTvRecordingLine(row)}
    ${active?dvrProgressMarkup(active):""}${active?`<p class="hint">${esc(state.detail)}</p>`:""}
    <div class="dvr-health">${metric(total,"Confirmed bytes · not playable duration")}${row.state==="recording"?metric(lastWrite,"Last write")+metric(rate,"Write rate"):metric(String(row.gap_s||0)+"s","Known capture gaps")}${metric(String(obs&&obs.attempt||row.attempt||0),"Capture attempts")}</div>
    <div class="row" style="flex-wrap:wrap;gap:8px">${stop?`<button class="ghost sm" data-dvr-focus="detail-stop" onclick="liveTvStopRecording(${esc(JSON.stringify(row.id))},${esc(JSON.stringify(row.title))})">Stop recording…</button>`:""}${row.item_id&&row.file_id?`<a class="ghost sm" data-dvr-focus="detail-play" href="#/item/${row.item_id}">Play recording</a>`:""}${attention?`<button class="ghost sm" data-dvr-focus="detail-review" onclick="reviewDvrRecording(${through})">Mark reviewed</button>`:""}${["done","partial"].includes(row.state)?`<button class="ghost sm" data-dvr-focus="detail-delete" onclick="liveTvDeleteRecording(${esc(JSON.stringify(row.id))},${esc(JSON.stringify(row.title))})">Delete…</button>`:""}</div>
    ${DVR_PAGE.detailError?`<div class="clrefusal" role="status">${esc(DVR_PAGE.detailError)}</div>`:""}
    <h3>Recording timeline</h3>${!DVR_PAGE.historyComplete?`<p class="hint">${DVR_PAGE.truncatedBefore?`Earlier events were pruned through sequence ${DVR_PAGE.truncatedBefore}.`:"Detailed history was not collected for all of this recording."}</p>`:""}${events||'<p class="muted">No detailed events were collected.</p>'}
    ${DVR_PAGE.eventsNext?`<button class="ghost sm" data-dvr-focus="detail-more" onclick="loadOlderDvrEvents()">Load older events</button>`:""}
    <details class="dvr-tech" data-dvr-disclosure="capture"><summary data-dvr-focus="detail-technical">Capture details${diagnostics?" & diagnostics":""}</summary><dl><dt>Airing</dt><dd>${new Date(row.airing_start*1000).toLocaleString()} – ${esc(liveTvClock(row.airing_end))}</dd><dt>Capture window</dt><dd>${new Date(row.capture_start*1000).toLocaleString()} – ${esc(liveTvClock(row.capture_end))}</dd><dt>Known gaps</dt><dd>${row.gap_s||0}s</dd><dt>Late start</dt><dd>${row.late_start_s||0}s</dd>${ownerName?`<dt>Recorder owner</dt><dd>${esc(ownerName)}</dd>`:""}${diagnostics&&obs?`<dt>Runtime phase</dt><dd>${esc(obs.phase)}</dd><dt>Observation age</dt><dd>${Math.floor((Number(obs.observation_age_ms||0)+clientAge)/1000)}s</dd>`:""}</dl></details></aside>`;
}

function paintRecordingsPage(){
  if(!location.hash.startsWith("#/recordings"))return;
  const main=document.getElementById("main");if(!main)return;
  const dvrUi=dvrRememberUi(main);
  const active=document.activeElement;
  const focus=active&&active.closest&&active.closest("[data-recording-id]");
  const focusId=focus&&focus.dataset.recordingId;
  const focusKey=active&&(active.dataset&&active.dataset.dvrFocus||active.id)||null;
  const selection=active&&typeof active.selectionStart==="number"?[active.selectionStart,active.selectionEnd]:null;
  const fields=new Map([...main.querySelectorAll("input[id],select[id],textarea[id]")].map(field=>[field.id,field.value]));
  main.innerHTML=`<div class="dvr-page-head"><div><h1>Recordings</h1><p class="sub">Your recordings, upcoming programmes and anything that needs attention.</p></div><a class="ghost sm" href="#/activity">View activity →</a></div>${dvrSummaryMarkup()}${dvrTabsMarkup()}${dvrOverviewRows()}<h2 class="section">${esc((DVR_TABS.find(([id])=>id===DVR_PAGE.tab)||[])[1]||"Recordings")}</h2><div class="dvr-layout dvr-browse"><section>${dvrPageRowsMarkup()}</section>${dvrDetailMarkup()}</div>`;
  dvrRestoreUi(main,dvrUi);
  for(const [id,value] of fields){const field=document.getElementById(id);if(field)field.value=value;}
  const again=focusKey?(document.getElementById(focusKey)||main.querySelector(`[data-dvr-focus="${CSS.escape(focusKey)}"]`))
    :focusId&&main.querySelector(`[data-recording-id="${CSS.escape(focusId)}"] .dvr-row-button`);
  if(again){again.focus({preventScroll:true});if(selection&&again.setSelectionRange){try{again.setSelectionRange(...selection)}catch(error){}}}
}
async function selectDvrDetail(id){
  const scope=TOKEN;
  clearTimeout(DVR_PAGE.historyTimer);DVR_PAGE.historyTimer=null;
  DVR_PAGE.selectedId=id;DVR_PAGE.selected=null;DVR_PAGE.events=[];DVR_PAGE.detailBusy=true;DVR_PAGE.detailError=null;paintDvrHost();
  try{
    const [row,page]=await Promise.all([api(`/dvr/recordings/${encodeURIComponent(id)}`),api(`/dvr/recordings/${encodeURIComponent(id)}/events?limit=50`)]);
    if(scope!==TOKEN||DVR_PAGE.selectedId!==id||!dvrDetailRoute())return;
    DVR_PAGE.selected=row;DVR_PAGE.events=page.rows||[];DVR_PAGE.eventsNext=page.next||null;
    DVR_PAGE.historyComplete=!!page.history_complete;DVR_PAGE.truncatedBefore=page.truncated_before_sequence||null;
    scheduleDvrHistoryRefresh();
  }catch(e){if(scope===TOKEN&&DVR_PAGE.selectedId===id)DVR_PAGE.detailError=e&&e.message||"Recording details could not be loaded";}
  finally{if(scope===TOKEN&&DVR_PAGE.selectedId===id){DVR_PAGE.detailBusy=false;if(!dvrDetailRoute())return;paintDvrHost();if(matchMedia("(max-width:960px)").matches){const close=document.querySelector('[data-dvr-focus="detail-close"]');if(close){close.focus({preventScroll:true});close.closest(".dvr-detail").scrollIntoView({block:"start",behavior:"smooth"});}}}}
}

function scheduleDvrHistoryRefresh(){
  clearTimeout(DVR_PAGE.historyTimer);DVR_PAGE.historyTimer=null;
  if(!DVR_PAGE.selected||DVR_PAGE.selected.state!=="recording"||document.visibilityState==="hidden")return;
  const id=DVR_PAGE.selectedId;
  DVR_PAGE.historyTimer=setTimeout(()=>refreshDvrHistory(id),5000);
}
async function refreshDvrHistory(id){
  if(id!==DVR_PAGE.selectedId||!dvrDetailRoute()||document.visibilityState==="hidden")return;
  const scope=TOKEN,after=DVR_PAGE.events.reduce((max,event)=>Math.max(max,Number(event.sequence)||0),0);
  try{
    const [page,row]=await Promise.all([api(`/dvr/recordings/${encodeURIComponent(id)}/events?limit=50&after=${after}`),api(`/dvr/recordings/${encodeURIComponent(id)}`)]);
    if(scope!==TOKEN||id!==DVR_PAGE.selectedId||!dvrDetailRoute())return;
    const seen=new Set(DVR_PAGE.events.map(event=>event.event_id));
    DVR_PAGE.events=(page.rows||[]).filter(event=>!seen.has(event.event_id)).reverse().concat(DVR_PAGE.events);
    DVR_PAGE.selected=row;DVR_PAGE.detailError=null;
    DVR_PAGE.historyComplete=!!page.history_complete;DVR_PAGE.truncatedBefore=page.truncated_before_sequence||null;
    paintDvrHost();
    if(page.next){DVR_PAGE.historyTimer=setTimeout(()=>refreshDvrHistory(id),0);return;}
  }catch(e){if(scope!==TOKEN||id!==DVR_PAGE.selectedId)return;DVR_PAGE.detailError=e&&e.message||"Lifecycle refresh failed";paintDvrHost();}
  scheduleDvrHistoryRefresh();
}

function closeDvrDetail(){DVR_PAGE.closedByUser=true;clearTimeout(DVR_PAGE.historyTimer);DVR_PAGE.historyTimer=null;const id=DVR_PAGE.selectedId;DVR_PAGE.selectedId=null;DVR_PAGE.selected=null;DVR_PAGE.events=[];paintDvrHost();const row=id&&document.querySelector(`[data-recording-id="${CSS.escape(id)}"] .dvr-row-button`);if(row)row.focus();}
async function loadOlderDvrEvents(){
  const id=DVR_PAGE.selectedId,scope=TOKEN,cursor=DVR_PAGE.eventsNext;
  if(!id||!cursor)return;
  try{const page=await api(`/dvr/recordings/${encodeURIComponent(id)}/events?limit=50&before=${encodeURIComponent(cursor)}`);
    if(scope!==TOKEN||id!==DVR_PAGE.selectedId||!dvrDetailRoute())return;
    const seen=new Set(DVR_PAGE.events.map(e=>e.event_id));DVR_PAGE.events=DVR_PAGE.events.concat((page.rows||[]).filter(e=>!seen.has(e.event_id)));DVR_PAGE.eventsNext=page.next||null;paintDvrHost();
  }catch(e){toast(e.message);}
}

async function reviewDvrRecording(through){
  if(!DVR_PAGE.selectedId)return;
  try{await api(`/dvr/recordings/${encodeURIComponent(DVR_PAGE.selectedId)}/attention/ack`,{method:"POST",body:{through_sequence:Number(through)||0}});toast("Marked reviewed");ACTIVITY_DVR.recentAt=0;await liveTvDvrReload();}catch(e){toast(e.message);}
}

async function loadMoreDvrPage(){
  const path=dvrPagePath(DVR_PAGE.tab,DVR_PAGE.next);if(!path)return;
  try{const answer=await api(path);const page=DVR_PAGE.tab==="attention"?answer:dvrRecordingsPage(answer);const seen=new Set(DVR_PAGE.rows.map(raw=>(raw.recording||raw).id));const additions=(page.rows||[]).filter(raw=>DVR_PAGE.tab!=="upcoming"||dvrUpcomingRow(raw)).filter(raw=>DVR_PAGE.tab!=="skipped"||raw.state==="cancelled").filter(raw=>!seen.has((raw.recording||raw).id));DVR_PAGE.rows=DVR_PAGE.rows.concat(additions);DVR_PAGE.next=page.next||null;paintRecordingsPage();}catch(e){toast(e.message);}
}
async function submitManualRecording(event){
  event.preventDefault();const channel=document.getElementById("dvr-manual-channel").value;
  const start=Math.floor(new Date(document.getElementById("dvr-manual-start").value).getTime()/1000);
  const end=Math.floor(new Date(document.getElementById("dvr-manual-end").value).getTime()/1000);
  const title=document.getElementById("dvr-manual-title").value.trim();
  if(!Number.isFinite(start)||!Number.isFinite(end)||end<=start){toast("Capture end must be after its start");return;}
  try{const row=await api("/dvr/recordings",{method:"POST",body:{channel_id:channel,capture_start:start,capture_end:end,title}});toast("Scheduled to record");location.hash="#/recordings/upcoming";setTimeout(()=>selectDvrDetail(row.id),0);}catch(e){toast(e.message);}
}

