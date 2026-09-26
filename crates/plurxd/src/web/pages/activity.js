"use strict";
// ---- activity page --------------------------------------------------------
// Where the header pill lands: live streams (with a stop for admins),
// per-library scan state, and the Trakt story — refreshed every 3s.
async function viewActivity(generation=++PAGE_RENDER_GENERATION){
  layoutChrome("activity", `<h2 class="section" style="margin-top:6px">Now playing</h2>
    <div class="empty" aria-busy="true">Checking current activity…</div>
    ${typeof ME!=="undefined"&&ME&&ME.is_admin?'<h2 class="section">Content analysis</h2>':""}
    <h2 class="section">Pre-transcoding</h2><h2 class="section">Offline downloads</h2>
    <h2 class="section">Library</h2><h2 class="section">Trakt</h2>`);
  setPagePhase("#/activity",generation,"shell");
  if(ACTIVITY_SNAPSHOT){
    paintActivityBody(ACTIVITY_SNAPSHOT,ACTIVITY_DVR.rows,ACTIVITY_DVR);
    setPagePhase("#/activity",generation,"content");
  }
  await renderActivityBody(generation);
  if(DVR_PAGE.selectedId&&!DVR_PAGE.detailBusy){if(!DVR_PAGE.selected)selectDvrDetail(DVR_PAGE.selectedId);else scheduleDvrHistoryRefresh();}
  if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/activity") return;
  setPageTimer(()=>renderActivityBody(generation), 3000, generation);
}


// Durable status is separate from node-local progress and has its own bounded
// refresh. A slow queue read never holds up the live Activity overview.
const DURABLE_ACTIVITY={state:"queued",cursor:null,next:null,rows:[],counts:[],observed:0,error:null,busy:false,epoch:0,detail:null};
function durableQueueHtml(){
  if(!ME||!ME.is_admin)return "";
  const q=DURABLE_ACTIVITY;
  const states=["queued","running","cancelling","failed","cancelled","succeeded"];
  const count=state=>(q.counts||[]).filter(row=>row.state===state).reduce((n,row)=>n+Number(row.count||0),0);
  const rows=q.rows.map(job=>`<tr><td><button class="ghost sm" data-durable-focus="${esc(job.id)}" onclick="showDurableJob(${esc(JSON.stringify(job.id))})">${esc(job.kind.replace(/_/g," "))}</button>${job.title?`<div>${esc(job.title)}${job.library?` · ${esc(job.library)}`:""}</div>`:job.file_id?`<small class="muted"> · file ${esc(job.file_id)}</small>`:""}</td>
    <td>${esc(job.state)}${job.not_before_ms>q.observed?`<small> · retry ${esc(new Date(job.not_before_ms).toLocaleString())}</small>`:""}</td>
    <td>${esc(job.owner_node_id||"Awaiting worker")}</td><td>${esc(job.priority)}</td>
    <td>${esc(Math.floor(job.age_ms/60000))} min</td><td>${esc(job.error_code||(!job.supported?"Requires a worker that understands this payload":""))}</td>
    <td>${["queued","running"].includes(job.state)?`<button class="ghost sm" data-durable-focus="cancel-${esc(job.id)}" onclick="cancelDurableJob(${esc(JSON.stringify(job.id))},this)">Cancel</button>`:""}</td></tr>`).join("");
  return `<h2 class="section">Durable cluster work</h2><div class="card">
    <div class="row"><label>State <select aria-label="Durable job state" onchange="filterDurableJobs(this.value)">${states.map(state=>`<option value="${state}"${q.state===state?" selected":""}>${state} (${count(state)})</option>`).join("")}</select></label>
    <button class="ghost sm" onclick="refreshDurableActivity(true)">Refresh</button></div>
    <p class="hint">${q.observed?`Durable state observed ${esc(new Date(q.observed).toLocaleTimeString())}. Live worker progress is shown separately above.`:"Loading durable work…"}</p>
    ${q.migration&&q.migration.accepted?`<p class="hint">Legacy cutover: ${esc(q.migration.awaiting_import)} awaiting import · ${esc(q.migration.materialized)} mapped · ${esc(q.migration.failed)} failed · ${esc(q.migration.cancelled)} cancelled. Accepted work remains in the sealed backlog while queue capacity is full.</p>`:""}
    ${q.error?`<p class="err" role="status">${esc(q.error)}${q.observed?" Showing the last observation.":""}</p>`:""}
    ${rows?`<div class="tbl"><table><thead><tr><th>Work</th><th>State</th><th>Owner</th><th>Priority</th><th>Age</th><th>Reason</th><th></th></tr></thead><tbody>${rows}</tbody></table></div>`:q.observed?'<p class="muted">No jobs on this page.</p>':""}
    <div class="row">${q.cursor?'<button class="ghost sm" onclick="pageDurableJobs(null)">First page</button>':""}${q.next?`<button class="ghost sm" onclick="pageDurableJobs(${esc(JSON.stringify(q.next))})">Next 100</button>`:""}</div>
    ${q.detail?`<div class="hint"><b>${esc(q.detail.job.kind.replace(/_/g," "))} · ${esc(q.detail.job.state)}</b><p>${esc(q.detail.job.failed_attempts)} charged failures · ${esc(q.detail.job.yield_count)} yields · ${esc(q.detail.waiters.length)} interests shown${q.detail.more_waiters?" (more retained)":""}</p>
      ${q.detail.attempts.map(attempt=>`<div>${esc(attempt.node_id)} · ${esc(attempt.outcome||"running")} · ${esc(attempt.error_code||"")} · started ${esc(new Date(attempt.started_at_ms).toLocaleString())}</div>`).join("")}
      <button class="ghost sm" onclick="DURABLE_ACTIVITY.detail=null;paintDurableActivity()">Close details</button></div>`:""}</div>`;
}
function paintDurableActivity(){
  if(location.hash!=="#/activity")return;
  const host=document.getElementById("durable-activity");if(!host)return;
  const focus=document.activeElement?.dataset?.durableFocus;
  host.innerHTML=durableQueueHtml();
  if(focus)[...host.querySelectorAll("[data-durable-focus]")].find(el=>el.dataset.durableFocus===focus)?.focus({preventScroll:true});
}
async function refreshDurableActivity(force=false){
  const q=DURABLE_ACTIVITY,generation=PAGE_RENDER_GENERATION,epoch=q.epoch;
  if(!ME||!ME.is_admin||location.hash!=="#/activity"||q.busy||(!force&&Date.now()-q.observed<15000))return;
  q.busy=true;
  try{
    const query=new URLSearchParams({state:q.state});if(q.cursor)query.set("cursor",q.cursor);
    const page=await api(`/cluster/jobs?${query}`);
    if(epoch!==q.epoch||generation!==PAGE_RENDER_GENERATION||location.hash!=="#/activity")return;
    q.rows=page.jobs;q.counts=page.counts;q.migration=page.migration;q.next=page.next_cursor;q.observed=page.observed_at_ms;q.error=null;
  }catch(error){if(epoch===q.epoch&&generation===PAGE_RENDER_GENERATION)q.error=error.message||String(error);}
  finally{q.busy=false;paintDurableActivity();if(epoch!==q.epoch)refreshDurableActivity(true);}
}
function filterDurableJobs(state){DURABLE_ACTIVITY.state=state;pageDurableJobs(null);}
function pageDurableJobs(cursor){const q=DURABLE_ACTIVITY;q.cursor=cursor;q.next=null;q.rows=[];q.detail=null;q.observed=0;q.epoch++;paintDurableActivity();refreshDurableActivity(true);}
async function showDurableJob(id){
  DURABLE_ACTIVITY.selectedId=id;
  const epoch=DURABLE_ACTIVITY.epoch,generation=PAGE_RENDER_GENERATION;
  try{const detail=await api(`/cluster/jobs/${encodeURIComponent(id)}`);if(epoch!==DURABLE_ACTIVITY.epoch||generation!==PAGE_RENDER_GENERATION||DURABLE_ACTIVITY.selectedId!==id)return;DURABLE_ACTIVITY.detail=detail;paintDurableActivity();}
  catch(error){toast(error.message||String(error));}
}
async function cancelDurableJob(id,button){
  button.disabled=true;
  try{await api(`/cluster/jobs/${encodeURIComponent(id)}/cancel`,{method:"POST"});toast("Cancellation requested");DURABLE_ACTIVITY.epoch++;DURABLE_ACTIVITY.observed=0;DURABLE_ACTIVITY.detail=null;await refreshDurableActivity(true);}
  catch(error){toast(error.message||String(error));button.disabled=false;}
}
