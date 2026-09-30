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
const ACTIVITY_VIEW={tab:"status",inspector:null,detailTab:"summary"};
const DURABLE_ACTIVITY={state:"queued",kind:"",node:"",cursor:null,next:null,rows:[],counts:[],repairs:[],activeJobs:[],activeTruncated:false,observed:0,error:null,busy:false,request:0,epoch:0,detail:null,detailError:null,retries:new Map()};
function clusterWorkersHtml(d){
  if(!ME||!ME.is_admin)return "";
  const q=DURABLE_ACTIVITY,names=d.node_hostnames||{},workers=d.workers||{};
  const statuses=new Map((d.activity_nodes||[]).filter(n=>n.node_id).map(n=>[n.node_id,n.status]));
  const jobs=q.activeJobs||[];
  const ids=[...new Set([...Object.keys(names),...Object.keys(workers),...statuses.keys(),...jobs.map(j=>j.owner_node_id).filter(Boolean)])];
  const fresh=id=>workers[id]&&Date.now()-workers[id].observed_at_ms>=-30000&&Date.now()-workers[id].observed_at_ms<30000&&(!statuses.has(id)||statuses.get(id)==="answered");
  const known=ids.filter(fresh),sum=key=>known.reduce((n,id)=>n+Number(workers[id][key]||0),0);
  const queued=(q.counts||[]).filter(c=>c.state==="queued").reduce((n,c)=>n+Number(c.count||0),0);
  return `<h2 class="section">Cluster workers</h2><div class="cluster-worker-totals" aria-label="Observed cluster worker capacity">
    <div><b>${known.length} / ${ids.length}</b><span>nodes reporting capacity</span></div><div><b>${sum("heavy_in_use")} / ${sum("heavy_limit")}</b><span>heavy slots occupied${known.length<ids.length?" · partial total":""}</span></div><div><b>${sum("heavy_available")}</b><span>heavy slots available now</span></div><div><b>${q.observed?queued:"—"}</b><span>jobs queued${q.error?" · last observation":""}</span></div></div>
    <div class="tbl"><table class="activity-node-matrix"><thead><tr><th>Node</th><th>Heavy worker</th><th>Current work</th><th>CPU reserved</th><th>GPU sessions</th></tr></thead><tbody>${ids.sort((a,b)=>nodeLabel(names,a).localeCompare(nodeLabel(names,b))).map(id=>{
      const w=workers[id],known=fresh(id),assigned=jobs.filter(j=>j.owner_node_id===id);
      const status=!known?"Capacity unknown":!w.accepting_work?"Not accepting work":w.heavy_in_use?"Heavy worker busy":w.heavy_available?"Heavy worker available":"Waiting for media capacity";
      return `<tr><td><button class="ghost sm" data-durable-focus="node-${esc(id)}" onclick="showActivityNode(${esc(JSON.stringify(id))})">${esc(nodeLabel(names,id))}</button></td><td>${esc(status)}<small class="muted">${known?`${esc(w.heavy_in_use)} / ${esc(w.heavy_limit)} occupied`:esc(statuses.has(id)&&statuses.get(id)!=="answered"?activityNodeStatusText(statuses.get(id)):w?"Last capacity report is stale.":"This node has not supplied worker telemetry.")}</small></td><td>${assigned.length?assigned.map(job=>`<button class="ghost sm" data-durable-focus="worker-${esc(job.id)}" data-inspector-focus="job-${esc(job.id)}" onclick="showDurableJob(${esc(JSON.stringify(job.id))})">${esc(job.title||job.kind.replace(/_/g," "))}</button><small class="muted">${esc(durableStateLabel(job))}${durableLeaseExpired(job)?" · previous owner":" · durable assignment"}</small>`).join(""):known&&w.child_count?`${esc(w.child_count)} child processes`:'<span class="muted">No durable assignment in the latest queue observation.</span>'}</td><td>${known?`${esc(w.software_used)} / ${esc(w.software_limit)}`:"—"}</td><td>${known?`${esc(w.hardware_used)} / ${esc(w.hardware_limit)}`:"—"}</td></tr>`;
    }).join("")}</tbody></table></div><p class="hint">CPU counts are reserved threads, not measured utilization. Unknown capacity is excluded from totals. A free slot still needs compatible work and resource admission.</p>${q.activeTruncated?'<p class="hint">Assignment list is partial: more active jobs exist.</p>':""}<button class="ghost sm" onclick="setActivityTab('jobs')">Open jobs →</button>`;
}
function setActivityTab(tab){
  ACTIVITY_VIEW.tab=tab==="jobs"&&ME?.is_admin?"jobs":"status";
  for(const name of ["status","jobs"]){const pane=document.getElementById("activity-"+name);if(pane)pane.hidden=name!==ACTIVITY_VIEW.tab;const button=document.getElementById("activity-tab-"+name);if(button)button.setAttribute("aria-selected",String(name===ACTIVITY_VIEW.tab));}
  if(ACTIVITY_VIEW.tab==="jobs")refreshDurableActivity();
}
function activityTabsHtml(){
  if(!ME?.is_admin)return "";
  return `<div class="activity-tabs" role="tablist" aria-label="Activity views">${["status","jobs"].map(tab=>`<button class="ghost" role="tab" id="activity-tab-${tab}" data-activity-view="${tab}" aria-controls="activity-${tab}" aria-selected="${ACTIVITY_VIEW.tab===tab}" onclick="setActivityTab('${tab}')" onkeydown="if(event.key==='ArrowLeft'||event.key==='ArrowRight'){event.preventDefault();setActivityTab('${tab==="status"?"jobs":"status"}');document.getElementById('activity-tab-${tab==="status"?"jobs":"status"}').focus()}">${tab==="status"?"Status":"Jobs"}</button>`).join("")}</div>`;
}
function durableTypeBreakdownHtml(){
  const q=DURABLE_ACTIVITY,kinds=[...new Set((q.counts||[]).map(c=>c.kind))].sort();
  return `<aside class="activity-job-types"><h3>By job type</h3><button class="ghost sm" aria-pressed="${!q.kind}" onclick="filterDurableKind('')">All types</button>${kinds.map(kind=>{const counts=state=>(q.counts||[]).filter(c=>c.kind===kind&&c.state===state).reduce((n,c)=>n+Number(c.count||0),0);return `<button class="ghost sm" data-durable-focus="kind-${esc(kind)}" aria-pressed="${q.kind===kind}" onclick="filterDurableKind(${esc(JSON.stringify(kind))})"><b>${esc(kind.replace(/_/g," "))}</b><small>${counts("queued")} queued · ${counts("running")} running · ${counts("failed")} failed</small></button>`;}).join("")}<p class="hint">Cluster-wide totals, independent of filters.</p></aside>`;
}

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
  const rows=q.rows.map(job=>`<tr><td><button class="ghost sm" data-durable-focus="${esc(job.id)}" onclick="showDurableJob(${esc(JSON.stringify(job.id))})">${esc(job.kind.replace(/_/g," "))}</button>${job.title?`<div>${esc(job.title)}${job.library?` · ${esc(job.library)}`:""}</div>`:job.file_id?`<small class="muted"> · file ${esc(job.file_id)}</small>`:""}</td>
    <td>${esc(durableStateLabel(job))}${job.not_before_ms>q.observed?`<small> · retry ${esc(new Date(job.not_before_ms).toLocaleString())}</small>`:""}</td>
    <td>${job.owner_node_id?`<span title="${esc(job.owner_node_id)}">${esc(nodeLabel(nodeNames,job.owner_node_id))}</span>${durableLeaseExpired(job)?'<small> · previous owner</small>':""}`:job.target_node_id?`Destination: ${esc(nodeLabel(nodeNames,job.target_node_id))}`:"Awaiting worker"}</td><td>${esc(job.priority)}</td>
    <td>${esc(durableDuration(job.age_ms))}</td><td>${esc(job.error_code||(!job.supported?"Requires a worker that understands this payload":""))}</td>
    <td>${["queued","running"].includes(job.state)?`<button class="ghost sm" data-durable-focus="cancel-${esc(job.id)}" onclick="cancelDurableJob(${esc(JSON.stringify(job.id))},this)">Cancel</button>`:job.retry_supported?`<button class="ghost sm" data-durable-focus="retry-${esc(job.id)}" onclick="retryDurableJob(${esc(JSON.stringify(job.id))},this)">Retry</button>`:""}</td></tr>`).join("");
  return `<h2 class="section">Jobs</h2><div class="activity-jobs-layout">${durableTypeBreakdownHtml()}<div class="activity-job-list"><div class="card">
    <div class="row"><label>State <select aria-label="Durable job state" onchange="filterDurableJobs(this.value)">${states.map(state=>`<option value="${state}"${q.state===state?" selected":""}>${state} (${count(state)})</option>`).join("")}</select></label>
    <label>Node <select aria-label="Durable job node" onchange="filterDurableNode(this.value)"><option value="">All nodes</option>${Object.entries(nodeNames).map(([id,name])=>`<option value="${esc(id)}"${q.node===id?" selected":""}>${esc(nodeLabel(nodeNames,id))}</option>`).join("")}</select></label><button class="ghost sm" onclick="refreshDurableActivity(true)">Refresh</button></div>
    <p class="hint">${q.observed?`Durable state observed ${esc(new Date(q.observed).toLocaleTimeString())}. Since requested includes waiting and retries; it is not execution time. Select a job for worker observations and attempt history. Node scope includes assignments and explicit destinations; shared unassigned work remains under All nodes.`:"Loading durable work…"}</p>
    ${q.migration&&q.migration.accepted?`<p class="hint">Legacy cutover: ${esc(q.migration.awaiting_import)} awaiting import · ${esc(q.migration.materialized)} mapped · ${esc(q.migration.failed)} failed · ${esc(q.migration.cancelled)} cancelled. Accepted work remains in the sealed backlog while queue capacity is full.</p>`:""}
    ${q.error?`<p class="err" role="status">${esc(q.error)}${q.observed?" Showing the last observation.":""}</p>`:""}
    ${rows?`<div class="tbl"><table><thead><tr><th>Work</th><th>State</th><th>Owner</th><th>Priority</th><th>Since requested</th><th>Reason</th><th></th></tr></thead><tbody>${rows}</tbody></table></div>`:q.observed?'<p class="muted">No jobs on this page.</p>':""}
    <div class="row">${q.cursor?'<button class="ghost sm" onclick="pageDurableJobs(null)">First page</button>':""}${q.next?`<button class="ghost sm" onclick="pageDurableJobs(${esc(JSON.stringify(q.next))})">Next 25</button>`:""}</div>
    ${(q.repairs||[]).length?`<details><summary>Artifact repairs · ${(q.repairs||[]).length} recent plans</summary><p class="hint">Copy first, then one rebuild and delivery if needed. Failed plans stop automatically; inspect the work for its reason and retry controls.</p><div class="tbl"><table><thead><tr><th>Artifact</th><th>Destination</th><th>Phase</th><th>Age</th><th></th></tr></thead><tbody>${q.repairs.map(repair=>`<tr><td>${esc(repair.kind)}</td><td>${esc(nodeLabel(nodeNames,repair.target_node_id))}</td><td>${esc(repair.phase)}</td><td>${esc(Math.floor(repair.age_ms/60000))} min</td><td>${repair.job_id?`<button class="ghost sm" onclick="showDurableJob(${esc(JSON.stringify(repair.job_id))})">Inspect work</button>`:""}</td></tr>`).join("")}</tbody></table></div></details>`:""}
    </div></div></div>`;
}

function paintDurableActivity(){
  if(location.hash!=="#/activity")return;
  const host=document.getElementById("durable-activity");if(!host)return;
  const focus=(/** @type {HTMLElement} */ (document.activeElement))?.dataset?.durableFocus;
  host.innerHTML=durableQueueHtml(ACTIVITY_SNAPSHOT?.node_hostnames||{});
  const workers=document.getElementById("cluster-workers");
  if(workers)workers.innerHTML=clusterWorkersHtml(ACTIVITY_SNAPSHOT||{});
  const buttons=/** @type {NodeListOf<HTMLButtonElement>} */ (host.querySelectorAll("[data-durable-focus]"));
  paintActivityInspector();
  if(focus)[...buttons,...(workers?.querySelectorAll("[data-durable-focus]")||[])].find(el=>el.dataset.durableFocus===focus)?.focus({preventScroll:true});
}
async function refreshDurableActivity(force=false){
  const q=DURABLE_ACTIVITY,generation=PAGE_RENDER_GENERATION,epoch=q.epoch,detailId=q.selectedId;
  if(!ME||!ME.is_admin||location.hash!=="#/activity"||q.busy||(!force&&Date.now()-q.observed<15000))return;
  q.busy=true;const request=q.request=(q.request||0)+1;
  try{
    const query=new URLSearchParams({state:q.state,limit:"25"});if(q.kind)query.set("kind",q.kind);if(q.node)query.set("node_id",q.node);if(q.cursor)query.set("cursor",q.cursor);
    const page=await api(`/cluster/jobs?${query}`);
    if(epoch!==q.epoch||generation!==PAGE_RENDER_GENERATION||location.hash!=="#/activity")return;
    q.rows=page.jobs;q.repairs=page.repairs||[];q.counts=page.counts;q.migration=page.migration;q.next=page.next_cursor;q.observed=page.observed_at_ms;q.error=null;
    q.activeJobs=page.active_jobs||[];q.activeTruncated=!!page.active_truncated;
    paintDurableActivity();
    if(detailId&&q.selectedId===detailId){
      try{
        const detail=await api(`/cluster/jobs/${encodeURIComponent(detailId)}`);
        if(epoch===q.epoch&&generation===PAGE_RENDER_GENERATION&&location.hash==="#/activity"&&q.selectedId===detailId){q.detail=detail;q.detailError=null;}
      }catch(error){if(epoch===q.epoch&&generation===PAGE_RENDER_GENERATION&&q.selectedId===detailId)q.detailError=error.message||String(error);}
    }
  }catch(error){if(epoch===q.epoch&&generation===PAGE_RENDER_GENERATION)q.error=error.message||String(error);}
  finally{if(request===q.request){q.busy=false;paintDurableActivity();if(epoch!==q.epoch)refreshDurableActivity(true);}}
}
function filterDurableKind(kind){DURABLE_ACTIVITY.kind=kind;pageDurableJobs(null);}
function filterDurableNode(node){DURABLE_ACTIVITY.node=node;pageDurableJobs(null);}
function filterDurableJobs(state){DURABLE_ACTIVITY.state=state;pageDurableJobs(null);}
function pageDurableJobs(cursor){const q=DURABLE_ACTIVITY;q.cursor=cursor;q.next=null;q.rows=[];q.observed=0;q.epoch++;paintDurableActivity();refreshDurableActivity(true);}
async function showDurableJob(id){
  DURABLE_ACTIVITY.selectedId=id;
  ACTIVITY_VIEW.inspector={kind:"job",id};ACTIVITY_VIEW.detailTab="summary";openActivityInspector();
  const epoch=DURABLE_ACTIVITY.epoch,generation=PAGE_RENDER_GENERATION;
  try{const detail=await api(`/cluster/jobs/${encodeURIComponent(id)}`);if(epoch!==DURABLE_ACTIVITY.epoch||generation!==PAGE_RENDER_GENERATION||DURABLE_ACTIVITY.selectedId!==id)return;DURABLE_ACTIVITY.detail=detail;DURABLE_ACTIVITY.detailError=null;paintDurableActivity();}
  catch(error){if(epoch===DURABLE_ACTIVITY.epoch&&generation===PAGE_RENDER_GENERATION&&DURABLE_ACTIVITY.selectedId===id){DURABLE_ACTIVITY.detailError=error.message||String(error);paintActivityInspector();}}
}
async function cancelDurableJob(id,button){
  button.disabled=true;
  try{await api(`/cluster/jobs/${encodeURIComponent(id)}/cancel`,{method:"POST"});toast("Cancellation requested");DURABLE_ACTIVITY.epoch++;DURABLE_ACTIVITY.observed=0;DURABLE_ACTIVITY.detail=null;await refreshDurableActivity(true);}
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
    q.epoch++;q.observed=0;q.detail=null;
    await refreshDurableActivity(true);
  }catch(error){toast(error.message||String(error));button.disabled=false;}
}
function showActivityNode(id){ACTIVITY_VIEW.inspector={kind:"node",id};ACTIVITY_VIEW.detailTab="summary";openActivityInspector();}
function openActivityNodeJobs(id){closeActivityInspector();setActivityTab("jobs");filterDurableNode(id);}
function openActivityInspector(){
  if(!ME?.is_admin)return;
  let dialog=document.getElementById("activity-work-inspector");
  if(!dialog){dialog=document.createElement("dialog");dialog.id="activity-work-inspector";dialog.className="activity-work-inspector";dialog.setAttribute("aria-label","Cluster work details");dialog.innerHTML='<div class="activity-inspector-head"><b>Work details</b><button class="ghost sm" onclick="closeActivityInspector()">Close ×</button></div><div id="activity-inspector-body"></div>';document.body.append(dialog);dialog.addEventListener("cancel",()=>{ACTIVITY_VIEW.inspector=null;DURABLE_ACTIVITY.selectedId=null;DURABLE_ACTIVITY.detail=null;});window.addEventListener("hashchange",()=>{if(location.hash!=="#/activity")closeActivityInspector();});}
  if(!dialog.open){ACTIVITY_VIEW.returnFocus=document.activeElement?.dataset?.durableFocus||null;dialog.showModal();}
  paintActivityInspector();
}
function closeActivityInspector(){
  document.getElementById("activity-work-inspector")?.close();ACTIVITY_VIEW.inspector=null;DURABLE_ACTIVITY.selectedId=null;DURABLE_ACTIVITY.detail=null;
  const key=ACTIVITY_VIEW.returnFocus;if(key)[...document.querySelectorAll("[data-durable-focus]")].find(el=>el.dataset.durableFocus===key)?.focus({preventScroll:true});
}
function setActivityDetailTab(tab){ACTIVITY_VIEW.detailTab=tab;paintActivityInspector();}
function activityJobProgress(job){
  if(job.state!=="running"||durableLeaseExpired(job))return null;
  return (ACTIVITY_SNAPSHOT?.analysis?.progress||[]).find(row=>row.job_id===job.id&&row.node_id===job.owner_node_id&&Date.now()-row.updated_at_ms>=-30000&&Date.now()-row.updated_at_ms<30000)||null;
}
function activityInspectorHtml(){
  const selected=ACTIVITY_VIEW.inspector,d=ACTIVITY_SNAPSHOT||{},names=d.node_hostnames||{},q=DURABLE_ACTIVITY;
  if(!selected)return "";
  if(selected.kind==="node"){
    const id=selected.id,w=d.workers?.[id],status=(d.activity_nodes||[]).find(n=>n.node_id===id)?.status,fresh=w&&(!status||status==="answered")&&Date.now()-w.observed_at_ms<30000&&Date.now()-w.observed_at_ms>=-30000;
    const assignments=(q.activeJobs||[]).filter(job=>job.owner_node_id===id);
    return `<h2>${esc(nodeLabel(names,id))}</h2><p class="hint">${esc(id)}</p>${fresh?`<dl class="activity-facts"><dt>Heavy slots</dt><dd>${esc(w.heavy_in_use)} / ${esc(w.heavy_limit)} occupied · ${esc(w.heavy_available)} available</dd><dt>CPU reserved</dt><dd>${esc(w.software_used)} / ${esc(w.software_limit)} threads</dd><dt>GPU sessions</dt><dd>${esc(w.hardware_used)} / ${esc(w.hardware_limit)}</dd><dt>Admission</dt><dd>${w.accepting_work?w.heavy_available?"Available for compatible work":w.heavy_in_use?"Heavy work is active":"Waiting for media capacity":"Node is starting or in maintenance"}</dd><dt>Observed</dt><dd>${esc(new Date(w.observed_at_ms).toLocaleTimeString())}</dd></dl>`:'<p class="hint">Capacity unknown. This node has no fresh worker observation.</p>'}<h3>Assignments</h3>${assignments.map(job=>`<p><button class="ghost sm" data-inspector-focus="job-${esc(job.id)}" onclick="showDurableJob(${esc(JSON.stringify(job.id))})">${esc(job.title||job.kind.replace(/_/g," "))}</button> · ${esc(durableStateLabel(job))}${durableLeaseExpired(job)?" · previous owner":""}</p>`).join("")||'<p class="hint">No durable assignment reported.</p>'}${fresh?`<details><summary>${esc(w.child_count)} child processes</summary>${(w.children||[]).map(child=>`<p class="hint">${esc(child)}</p>`).join("")}</details>`:""}<button class="ghost sm" onclick="openActivityNodeJobs(${esc(JSON.stringify(id))})">View this node’s jobs →</button>`;
  }
  const detail=q.detail?.job.id===selected.id?q.detail:null;
  if(!detail)return `<p role="status">${q.detailError?esc(q.detailError):"Loading job details…"}</p>`;
  const job=detail.job,progress=activityJobProgress(job),view=ACTIVITY_VIEW.detailTab;
  const tabs=`<div class="activity-tabs" role="group" aria-label="Job detail sections">${["summary","stages","history"].map(tab=>`<button class="ghost sm" data-inspector-focus="${tab}" aria-pressed="${tab===view}" onclick="setActivityDetailTab('${tab}')">${tab[0].toUpperCase()+tab.slice(1)}</button>`).join("")}</div>`;
  let body="";
  if(view==="history")body=`<p>${esc(job.failed_attempts)} charged failures · ${esc(job.yield_count)} yields</p>${detail.attempts.map(a=>`<div class="activity-attempt"><b>${esc(nodeLabel(names,a.node_id))}</b> · ${esc(a.outcome||(durableLeaseExpired(job)?"lease expired":"running"))}<p class="hint">${esc(a.error_code||"")} · started ${esc(new Date(a.started_at_ms).toLocaleString())} · ${esc(durableDuration((a.finished_at_ms??(durableLeaseExpired(job)?job.lease_expires_ms:job.observed_at_ms))-a.started_at_ms))} elapsed</p></div>`).join("")||'<p class="hint">No attempts retained.</p>'}`;
  else if(view==="stages")body=`<dl class="activity-facts"><dt>Queue state</dt><dd>${esc(durableStateLabel(job))}</dd><dt>Worker stage</dt><dd>${progress?esc(progress.stage.replace(/_/g," ")):"Not reported"}</dd><dt>Since requested</dt><dd>${esc(durableDuration(job.age_ms))}</dd></dl>${progress?`<p>${esc(fmtBytes(progress.bytes_read)||"0 B")} / ${esc(fmtBytes(progress.total_bytes)||"unknown")} examined</p><p class="hint">${esc(fmtBytes(progress.throughput_bps)||"—")}/s · elapsed ${esc(fmtDur(progress.elapsed_ms)||"—")} · ETA ${progress.eta_ms!=null?esc(fmtDur(progress.eta_ms)):"not reported"}</p>`:'<p class="hint">No fresh execution-stage observation is available for this attempt. Queue age is not execution time.</p>'}`;
  else body=`<dl class="activity-facts"><dt>${durableLeaseExpired(job)?"Previous owner":"Owner"}</dt><dd>${job.owner_node_id?esc(nodeLabel(names,job.owner_node_id)):"Unassigned"}</dd><dt>Destination</dt><dd>${job.target_node_id?esc(nodeLabel(names,job.target_node_id)):"Shared queue"}</dd><dt>Requested by</dt><dd>${esc([...new Set(detail.waiters.map(w=>w.consumer_kind))].join(", ")||"Not reported")}</dd><dt>Priority</dt><dd>${esc(job.priority)}</dd><dt>Since requested</dt><dd>${esc(durableDuration(job.age_ms))}</dd><dt>Reason</dt><dd>${esc(job.error_code||(job.not_before_ms>job.observed_at_ms?"Waiting until "+new Date(job.not_before_ms).toLocaleString():"No refusal reported"))}</dd></dl><p class="hint">${detail.waiters.length} interests shown${detail.more_waiters?" · more retained":""}. Observed ${esc(new Date(job.observed_at_ms).toLocaleTimeString())}.</p>`;
  return `<h2>${esc(job.title||job.kind.replace(/_/g," "))}</h2><p class="hint">${esc(job.kind.replace(/_/g," "))} · ${esc(durableStateLabel(job))}${job.library?" · "+esc(job.library):""}</p>${tabs}${q.detailError?`<p class="err" role="status">Showing the last observation. ${esc(q.detailError)}</p>`:""}${body}`;
}
function paintActivityInspector(){
  const host=document.getElementById("activity-inspector-body");if(!host||!ACTIVITY_VIEW.inspector)return;
  const focus=document.activeElement?.dataset?.inspectorFocus,scroll=host.parentElement.scrollTop;
  host.innerHTML=activityInspectorHtml();host.parentElement.scrollTop=scroll;
  if(focus)[...host.querySelectorAll("[data-inspector-focus]")].find(el=>el.dataset.inspectorFocus===focus)?.focus({preventScroll:true});
}

function resetActivityWork(){
  closeActivityInspector();
  Object.assign(ACTIVITY_VIEW,{tab:"status",inspector:null,detailTab:"summary",returnFocus:null});
  Object.assign(DURABLE_ACTIVITY,{busy:false,request:(DURABLE_ACTIVITY.request||0)+1,migration:null,activeTruncated:false,rows:[],counts:[],activeJobs:[],detail:null,detailError:null,selectedId:null,cursor:null,next:null,observed:0,error:null,kind:"",node:"",state:"queued",repairs:[],epoch:DURABLE_ACTIVITY.epoch+1});
  DURABLE_ACTIVITY.retries.clear();
  document.getElementById("activity-work-inspector")?.remove();
}
