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
const DURABLE_ACTIVITY={state:"queued",open:true,repairsOpen:false,pageSize:20,history:[],cursor:null,next:null,rows:[],counts:[],repairs:[],observed:0,error:null,busy:false,epoch:0,detail:null,detailError:null,retries:new Map()};
function durableDuration(ms){
  const minutes=Math.floor(Math.max(0,Number(ms)||0)/60000);
  if(minutes<1)return "<1 min";
  if(minutes<60)return `${minutes} min`;
  return `${Math.floor(minutes/60)}h ${minutes%60}m`;
}
function durableLeaseExpired(job){
  return ["running","cancelling"].includes(job.state)&&job.lease_expires_ms!=null&&job.lease_expires_ms<=job.observed_at_ms;
}
function durableStateLabel(job){
  return durableLeaseExpired(job)?"Lease expired · awaiting recovery":job.state;
}
function durableQueueHtml(nodeNames={}){
  if(!ME||!ME.is_admin)return "";
  const q=DURABLE_ACTIVITY;
  const states=["queued","running","cancelling","failed","cancelled","succeeded"];
  const count=state=>(q.counts||[]).filter(row=>row.state===state).reduce((n,row)=>n+Number(row.count||0),0);
  const pageNumber=(q.history||[]).length+1;
  const summary=["running","queued","failed"].map(state=>`<span class="durable-count"><b>${count(state)}</b> ${state}</span>`).join("");
  const selected=q.selectedId||q.detail?.job.id;
  const expanded=id=>selected===id;
  const detailHtml=()=>{
    if(!q.detail)return `<section class="durable-detail" role="status">${q.detailError?`Could not load details: ${esc(q.detailError)} <button class="ghost sm" onclick="showDurableJob(${esc(JSON.stringify(selected))},true)">Try again</button>`:"Loading job details…"}</section>`;
    return `<section class="durable-detail"><b>${esc(q.detail.job.kind.replace(/_/g," "))} · ${esc(durableStateLabel(q.detail.job))}</b><p>${esc(q.detail.job.failed_attempts)} charged failures · ${esc(q.detail.job.yield_count)} yields · ${esc(q.detail.waiters.length)} interests shown${q.detail.more_waiters?" (more retained)":""}</p>
      <p>Details observed ${esc(new Date(q.detail.job.observed_at_ms).toLocaleTimeString())}.${q.detailError?` Refresh failed: ${esc(q.detailError)}`:""}</p>
      ${q.detail.attempts.map(attempt=>`<div>${esc(nodeLabel(nodeNames,attempt.node_id))} · ${esc(attempt.outcome||(durableLeaseExpired(q.detail.job)?"lease expired":"running"))} · ${esc(attempt.error_code||"")} · ${esc(durableDuration((attempt.finished_at_ms??(durableLeaseExpired(q.detail.job)?q.detail.job.lease_expires_ms:q.detail.job.observed_at_ms))-attempt.started_at_ms))} ${attempt.finished_at_ms?"elapsed":durableLeaseExpired(q.detail.job)?"until lease expired":"this attempt"} · started ${esc(new Date(attempt.started_at_ms).toLocaleString())}</div>`).join("")}
      <button class="ghost sm" data-durable-focus="close-detail" onclick="closeDurableJob()">Close details</button></section>`;
  };
  const jobs=q.rows.slice();
  // Keep an inspected job beside its details if it leaves the active filter.
  if(!q.repairSelected&&q.detail&&!jobs.some(job=>job.id===selected))jobs.splice(Math.min(q.selectedIndex||0,jobs.length),0,q.detail.job);
  const rows=jobs.map(job=>`<tr><td class="durable-work"><button class="ghost sm durable-title" data-durable-focus="${esc(job.id)}" aria-expanded="${expanded(job.id)&&!q.repairSelected}" aria-controls="durable-job-${esc(job.id)}" onclick="showDurableJob(${esc(JSON.stringify(job.id))})"><span aria-hidden="true">${expanded(job.id)&&!q.repairSelected?"▾":"▸"}</span> ${esc(job.title||job.kind.replace(/_/g," "))}</button><div class="durable-meta">${job.title?esc(job.kind.replace(/_/g," ")):""}${job.library?` · ${esc(job.library)}`:job.file_id?` · file ${esc(job.file_id)}`:""} · Priority ${esc(job.priority)}</div></td>
    <td data-label="State"><span class="pill durable-state ${durableLeaseExpired(job)?"expired":states.includes(job.state)?job.state:""}">${esc(durableStateLabel(job))}</span>${job.not_before_ms>q.observed?`<small> · retry ${esc(new Date(job.not_before_ms).toLocaleString())}</small>`:""}</td>
    <td data-label="Worker">${job.owner_node_id?`<span title="${esc(job.owner_node_id)}">${esc(nodeLabel(nodeNames,job.owner_node_id))}</span>${durableLeaseExpired(job)?'<small> · previous owner</small>':""}`:"Awaiting worker"}</td>
    <td class="durable-age" data-label="Since requested">${esc(durableDuration(job.age_ms))}</td><td class="durable-reason" data-label="Reason">${esc(job.error_code||(!job.supported?"Requires a worker that understands this payload":"—"))}</td>
    <td data-label="Actions">${["queued","running"].includes(job.state)?`<button class="ghost sm" data-durable-focus="cancel-${esc(job.id)}" onclick="cancelDurableJob(${esc(JSON.stringify(job.id))},this)">Cancel</button>`:job.retry_supported?`<button class="ghost sm" data-durable-focus="retry-${esc(job.id)}" onclick="retryDurableJob(${esc(JSON.stringify(job.id))},this)">Retry</button>`:""}</td></tr><tr class="durable-detail-row" id="durable-job-${esc(job.id)}"${expanded(job.id)&&!q.repairSelected?"":" hidden"}><td colspan="6">${expanded(job.id)&&!q.repairSelected?detailHtml():""}</td></tr>`).join("");
  return `<details class="card durable-queue"${q.open!==false?" open":""} ontoggle="if(this.isConnected)DURABLE_ACTIVITY.open=this.open">
    <summary data-durable-focus="section"><span class="durable-heading"><span class="durable-heading-title">Durable cluster work</span><span class="durable-overview">${q.observed?summary:"Awaiting observation"}${q.error?'<span class="err">Refresh failed</span>':""}</span></span></summary>
    <div class="durable-body"><div class="durable-toolbar"><label>State <select data-durable-focus="state" aria-label="Durable job state" onchange="filterDurableJobs(this.value)">${states.map(state=>`<option value="${state}"${q.state===state?" selected":""}>${state} (${count(state)})</option>`).join("")}</select></label>
    <label>Per page <select data-durable-focus="size" aria-label="Durable jobs per page" onchange="resizeDurableJobs(this.value)">${[10,20,50].map(size=>`<option value="${size}"${(q.pageSize||20)===size?" selected":""}>${size}</option>`).join("")}</select></label>
    <button class="ghost sm" data-durable-focus="refresh" onclick="refreshDurableActivity(true)"${q.busy?" disabled":""}>${q.busy?"Refreshing…":"Refresh"}</button></div>
    <p class="hint">${q.observed?`Durable state observed ${esc(new Date(q.observed).toLocaleTimeString())}. Since requested includes waiting and retries; it is not execution time. Live worker progress is shown separately above.`:"Loading durable work…"}</p>
    ${q.migration&&q.migration.accepted?`<p class="hint">Legacy cutover: ${esc(q.migration.awaiting_import)} awaiting import · ${esc(q.migration.materialized)} mapped · ${esc(q.migration.failed)} failed · ${esc(q.migration.cancelled)} cancelled. Accepted work remains in the sealed backlog while queue capacity is full.</p>`:""}
    ${q.error?`<p class="err" role="status">${esc(q.error)}${q.observed?" Showing the last observation.":""}</p>`:""}
    ${rows?`<div class="tbl durable-table"><table aria-label="Durable cluster jobs"><thead><tr><th>Work</th><th>State</th><th>Worker</th><th>Since requested</th><th>Reason</th><th>Actions</th></tr></thead><tbody>${rows}</tbody></table></div>`:q.observed?`<p class="durable-empty">No ${esc(q.state)} jobs on this page.${q.cursor?" Go back or refresh to check for work.":""}</p>`:""}
    <nav class="durable-pagination" aria-label="Durable work pages">
      <span role="status">Page ${pageNumber} · ${q.rows.length} job${q.rows.length===1?"":"s"} shown${q.observed?` · ${count(q.state)} ${esc(q.state)} in cluster`:""}</span>
      <div class="row"><button class="ghost sm" data-durable-focus="first" onclick="pageDurableJobs()"${q.cursor?"":" disabled"}>First</button>
      <button class="ghost sm" data-durable-focus="previous" onclick="previousDurableJobs()"${(q.history||[]).length?"":" disabled"}>Previous</button>
      <button class="ghost sm" data-durable-focus="next" onclick="pageDurableJobs(DURABLE_ACTIVITY.next)"${q.next&&!q.busy?"":" disabled"}>Next</button></div>
    </nav>
    ${(q.repairs||[]).length?`<details class="durable-repairs"${q.repairsOpen?" open":""} ontoggle="if(this.isConnected)DURABLE_ACTIVITY.repairsOpen=this.open"><summary data-durable-focus="repairs">Artifact repairs · ${(q.repairs||[]).length} recent plans</summary><p class="hint">Copy first, then one rebuild and delivery if needed. Failed plans stop automatically; inspect the work for its reason and retry controls.</p><div class="tbl"><table><thead><tr><th>Artifact</th><th>Destination</th><th>Phase</th><th>Age</th><th></th></tr></thead><tbody>${q.repairs.map(repair=>`<tr><td data-label="Artifact">${esc(repair.kind)}</td><td data-label="Destination">${esc(nodeLabel(nodeNames,repair.target_node_id))}</td><td data-label="Phase">${esc(repair.phase)}</td><td data-label="Age">${esc(Math.floor(repair.age_ms/60000))} min</td><td>${repair.job_id?`<button class="ghost sm" data-durable-focus="repair-${esc(repair.id)}" aria-expanded="${expanded(repair.job_id)&&q.repairSelected===repair.id}" onclick="showDurableJob(${esc(JSON.stringify(repair.job_id))},false,${esc(JSON.stringify(repair.id))})">Inspect work</button>`:""}</td></tr>${expanded(repair.job_id)&&q.repairSelected===repair.id?`<tr><td colspan="5">${detailHtml()}</td></tr>`:""}`).join("")}</tbody></table></div></details>`:""}
    </div></details>`;
}
// Replacement markup can temporarily disable the focused control. Retain its
// identity until the request settles, then use a nearby control at page edges.
function restoreDurableFocus(host,focus){
  const q=DURABLE_ACTIVITY;
  q.pendingFocus=null;
  if(!focus)return;
  const controls=[...(/** @type {NodeListOf<HTMLButtonElement>} */ (host.querySelectorAll("[data-durable-focus]")))];
  let control=controls.find(el=>el.dataset.durableFocus===focus);
  if(control?.disabled){
    if(q.busy||!q.observed){q.pendingFocus=focus;return;}
    control=["previous","next","state"].map(key=>controls.find(el=>el.dataset.durableFocus===key)).find(el=>el&&!el.disabled);
  }
  control?.focus({preventScroll:true});
}
function paintDurableActivity(){
  if(location.hash!=="#/activity")return;
  const host=document.getElementById("durable-activity");if(!host)return;
  const focus=(/** @type {HTMLElement} */ (document.activeElement))?.dataset?.durableFocus||(document.activeElement===document.body?DURABLE_ACTIVITY.pendingFocus:null);
  host.innerHTML=durableQueueHtml(ACTIVITY_SNAPSHOT?.node_hostnames||{});
  restoreDurableFocus(host,focus);
}
async function refreshDurableActivity(force=false){
  const q=DURABLE_ACTIVITY,generation=PAGE_RENDER_GENERATION,epoch=q.epoch,detailId=q.detail?.job.id,detailEpoch=q.detailEpoch;
  if(!ME||!ME.is_admin||location.hash!=="#/activity"||q.busy||(!force&&Date.now()-q.observed<15000))return;
  q.busy=true;paintDurableActivity();
  try{
    const query=new URLSearchParams({state:q.state});if(q.cursor)query.set("cursor",q.cursor);
    const page=await api(`/cluster/jobs?${query}`);
    if(epoch!==q.epoch||generation!==PAGE_RENDER_GENERATION||location.hash!=="#/activity")return;
    // The endpoint reads at most 100 rows in id order. Resume after the last
    // displayed id so smaller UI pages never skip the rest of that batch.
    q.rows=page.jobs.slice(0,q.pageSize||20);
    q.next=page.jobs.length>q.rows.length?q.rows[q.rows.length-1].id:page.next_cursor;
    q.repairs=page.repairs||[];q.counts=page.counts;q.migration=page.migration;q.observed=page.observed_at_ms;q.error=null;
    paintDurableActivity();
    if(detailId&&q.detail?.job.id===detailId&&q.selectedId===detailId&&q.detailEpoch===detailEpoch){
      try{
        const detail=await api(`/cluster/jobs/${encodeURIComponent(detailId)}`);
        if(epoch===q.epoch&&generation===PAGE_RENDER_GENERATION&&location.hash==="#/activity"&&q.detail?.job.id===detailId&&q.selectedId===detailId&&q.detailEpoch===detailEpoch){q.detail=detail;q.detailError=null;}
      }catch(error){if(epoch===q.epoch&&generation===PAGE_RENDER_GENERATION&&q.detail?.job.id===detailId&&q.selectedId===detailId&&q.detailEpoch===detailEpoch)q.detailError=error.message||String(error);}
    }
  }catch(error){if(epoch===q.epoch&&generation===PAGE_RENDER_GENERATION)q.error=error.message||String(error);}
  finally{q.busy=false;paintDurableActivity();if(epoch!==q.epoch)refreshDurableActivity(true);}
}
function filterDurableJobs(state){DURABLE_ACTIVITY.state=state;return pageDurableJobs(null);}
function pageDurableJobs(cursor=null,back=false){
  const q=DURABLE_ACTIVITY;
  if(!q.history)q.history=[];
  if(!back){if(cursor===null)q.history=[];else q.history.push(q.cursor);}
  q.cursor=cursor;q.next=null;q.rows=[];q.detail=null;q.selectedId=null;q.repairSelected=null;q.detailError=null;q.error=null;q.observed=0;q.epoch++;
  paintDurableActivity();return refreshDurableActivity(true);
}
function previousDurableJobs(){
  const q=DURABLE_ACTIVITY;
  if(q.history&&q.history.length)return pageDurableJobs(q.history.pop(),true);
}
function resizeDurableJobs(value){
  const size=Number(value);
  if(![10,20,50].includes(size))return;
  DURABLE_ACTIVITY.pageSize=size;return pageDurableJobs(null);
}
function closeDurableJob(){
  const q=DURABLE_ACTIVITY,focus=q.repairSelected?`repair-${q.repairSelected}`:q.selectedId;
  q.selectedId=null;q.repairSelected=null;q.detail=null;q.detailError=null;q.detailEpoch=(q.detailEpoch||0)+1;
  paintDurableActivity();
  const host=document.getElementById("durable-activity");
  if(host)[...(/** @type {NodeListOf<HTMLElement>} */ (host.querySelectorAll("[data-durable-focus]")))].find(el=>el.dataset.durableFocus===focus)?.focus({preventScroll:true});
}
async function showDurableJob(id,retry=false,repairId=null){
  const q=DURABLE_ACTIVITY;
  if(!retry&&q.selectedId===id&&q.repairSelected===repairId){closeDurableJob();return;}
  q.selectedIndex=Math.max(0,q.rows.findIndex(job=>job.id===id));
  q.selectedId=id;q.repairSelected=retry?q.repairSelected:repairId;q.detail=null;q.detailError=null;
  const epoch=q.epoch,generation=PAGE_RENDER_GENERATION,detailEpoch=q.detailEpoch=(q.detailEpoch||0)+1;
  const current=()=>epoch===q.epoch&&generation===PAGE_RENDER_GENERATION&&q.selectedId===id&&q.detailEpoch===detailEpoch;
  paintDurableActivity();
  try{const detail=await api(`/cluster/jobs/${encodeURIComponent(id)}`);if(!current())return;q.detail=detail;paintDurableActivity();}
  catch(error){if(current()){q.detailError=error.message||String(error);paintDurableActivity();}}
}
async function cancelDurableJob(id,button){
  button.disabled=true;
  try{await api(`/cluster/jobs/${encodeURIComponent(id)}/cancel`,{method:"POST"});toast("Cancellation requested");DURABLE_ACTIVITY.epoch++;DURABLE_ACTIVITY.observed=0;DURABLE_ACTIVITY.detail=null;DURABLE_ACTIVITY.selectedId=null;await refreshDurableActivity(true);}
  catch(error){toast(error.message||String(error));button.disabled=false;}
}

async function retryDurableJob(id,button){
  button.disabled=true;
  const q=DURABLE_ACTIVITY;
  if(!q.retries.has(id)){
    if(q.retries.size>=128)q.retries.delete(q.retries.keys().next().value);
    q.retries.set(id,crypto.randomUUID());
  }
  try{
    const result=await api(`/cluster/jobs/${encodeURIComponent(id)}/retry`,{method:"POST",body:{request_id:q.retries.get(id)}});
    q.retries.delete(id);
    toast(result.analysis_request_id?"New analysis generation requested":"New durable retry requested");
    q.epoch++;q.observed=0;q.detail=null;q.selectedId=null;
    await refreshDurableActivity(true);
  }catch(error){toast(error.message||String(error));button.disabled=false;}
}
