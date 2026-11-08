"use strict";
// ---- Activity → Starts ----------------------------------------------------
// One row per media-session create, across the cluster: when, which title,
// from which client build, by which route, why it was refused (if it was),
// where the time went, which node owns it, how long the picture took, and
// whether the title has the index a fast start needs — with the one action
// that fixes that. The rows are written by the node that handled each create
// (a node-local sidecar), so the page also says which nodes did not answer:
// a start missing from this page because its node was unreachable must never
// read as a start that did not happen.
const ACTIVITY_STARTS={rows:[],nodes:[],unavailable:[],observation:null,next:null,history:[],cursor:null,names:{},observed:0,busy:false,error:null,epoch:0,acting:null,actionError:null,directoryUnavailable:false};
const ACTIVITY_STARTS_OUTCOMES={presented:"Presented",failed:"Failed",refused:"Refused",pending:"Waiting for the client",unobserved:"No client report"};
const ACTIVITY_STARTS_INDEX_STATES={indexed:"Indexed",partial:"Partly indexed",pending:"Not indexed",refused:"Cannot be indexed",unsupported:"Not applicable"};
const ACTIVITY_STARTS_REASONS={unhealthy:"marked unhealthy",unsupported:"cannot be asked",unreachable:"unreachable",timed_out:"timed out",refused:"refused the request",http_error:"returned an error",invalid_response:"sent an unreadable answer"};

function activityStartsMs(ms){
  if(!Number.isFinite(ms))return "—";
  return ms>=10000?`${(ms/1000).toFixed(0)} s`:ms>=1000?`${(ms/1000).toFixed(1)} s`:`${Math.round(ms)} ms`;
}
function activityStartsPhasesHtml(row){
  const phases=Array.isArray(row.phases)?row.phases:[];
  const cost=[["authority reads",row.authority_reads],["local reads",row.local_reads],["writes",row.mutations],["store time",row.store_ms==null?null:activityStartsMs(row.store_ms)]]
    .filter(([,value])=>value!=null).map(([label,value])=>`${esc(label)} ${esc(value)}`).join(" · ");
  if(!phases.length&&!cost)return "";
  const total=Number.isFinite(row.elapsed_ms)?row.elapsed_ms:phases.reduce((n,p)=>n+(Number(p.ms)||0),0);
  return `<details class="starts-phases"><summary>Where the time went · ${esc(activityStartsMs(total))}</summary>
    ${phases.length?`<ol>${phases.map(p=>`<li><b>${esc(p.name)}</b> ${esc(activityStartsMs(Number(p.ms)))}<small class="muted"> · ${esc(p.authority_reads)} authority · ${esc(p.local_reads)} local · ${esc(p.mutations)} writes</small></li>`).join("")}</ol>`:""}
    ${cost?`<p class="hint">${cost}</p>`:""}</details>`;
}
function activityStartsOutcomeHtml(row){
  const label=ACTIVITY_STARTS_OUTCOMES[row.outcome]||row.outcome;
  const why=row.outcome==="refused"
    ?` <code>${esc(row.refusal)}</code>${row.retryable?' <small class="muted">retried by the client</small>':""}${row.refusal_message?`<small class="muted"> ${esc(row.refusal_message)}</small>`:""}`
    :row.outcome==="failed"?` <code>${esc(row.failure||"")}</code>`:"";
  return `<span class="pill starts-${esc(row.outcome)}">${esc(label)}</span>${why}`;
}
function activityStartsIndexHtml(row,i){
  const index=row.index||{},action=index.action||{kind:"unknown"};
  const state=index.status?ACTIVITY_STARTS_INDEX_STATES[index.status]||index.status:"Unknown";
  const detail=index.refusal?`<small class="muted"> ${esc(index.refusal)}</small>`:index.identities?`<small class="muted"> ${esc(index.present)} of ${esc(index.identities)} copy pipelines</small>`:"";
  let act="";
  if(action.kind==="joining")act=`<small class="muted">Indexing already ${esc(action.state==="queued"?"queued":"in progress")} — a start joins it</small>`;
  else if(action.kind==="retry"||action.kind==="enqueue"){
    const busy=ACTIVITY_STARTS.acting===i;
    act=`<button class="ghost sm" data-starts-action="${i}"${busy?" disabled":""} onclick="runActivityStartsAction(${i})">${busy?"Requesting…":action.kind==="retry"?"Index now (retry the failed job)":"Index now"}</button>`;
  }
  return `<div>${esc(state)}${detail}</div>${act}`;
}
function activityStartsRowHtml(row,i,names){
  const when=new Date(row.at_unix_ms);
  const title=row.title?esc(row.title):`<span class="muted">file ${esc(row.file_id)}</span>`;
  const owner=row.owner_node?esc(nodeLabel(names,row.owner_node)):"—";
  return `<tr data-starts-attempt="${esc(row.attempt)}">
    <td>${esc(when.toLocaleTimeString())}<small class="muted"><br>${esc(when.toLocaleDateString())}</small></td>
    <td><b>${title}</b>${row.user?`<small class="muted"><br>${esc(row.user)}</small>`:""}</td>
    <td>${esc(row.client_build)}</td>
    <td>${esc(row.route||"—")}</td>
    <td>${activityStartsOutcomeHtml(row)}${activityStartsPhasesHtml(row)}</td>
    <td>${owner}<small class="muted"><br>via ${esc(nodeLabel(names,row.node_id))}</small></td>
    <td>${row.ttff_ms==null?"—":esc(activityStartsMs(row.ttff_ms))}</td>
    <td>${activityStartsIndexHtml(row,i)}</td></tr>`;
}
function activityStartsObservationHtml(s){
  const o=s.observation;
  if(!o)return "";
  const part=(n,label)=>n?`<span><b>${esc(n)}</b> ${esc(label)}</span>`:"";
  return `<p class="starts-observation" role="status">${[part(o.attempts,"starts"),part(o.presented,"presented"),part(o.failed,"failed"),part(o.refused,"refused"),part(o.pending,"waiting for the client"),part(o.unobserved,"with no client report"),part(o.unavailable_nodes,"nodes did not answer")].filter(Boolean).join(" · ")||"No starts recorded yet."}</p>
    ${o.unobserved?'<p class="hint">A start with no client report is a missing observation: the client never said it got a picture or failed — it may have left, or reported to another node.</p>':""}`;
}
function activityStartsNodesHtml(s,names){
  const missing=(s.nodes||[]).filter(n=>n.status==="unavailable");
  if(s.directoryUnavailable)return '<div class="clrefusal" role="status"><b>Starts are incomplete</b> The cluster directory could not be read, so other nodes’ starts are missing from this page.</div>';
  if(!missing.length)return "";
  return `<div class="clrefusal" role="status" aria-live="polite"><b>Starts are incomplete</b> ${esc(missing.map(n=>`${nodeLabel(names,n.node_id)} ${ACTIVITY_STARTS_REASONS[n.reason]||n.reason}`).join("; "))}. Starts handled by those nodes are missing from this page.</div>`;
}
// State in, markup out, so the shipped painter and its test are one thing.
function activityStartsHtml(s,names){
  const rows=s.rows||[];
  const nav=`<div class="row">${s.history.length?'<button class="ghost sm" onclick="previousActivityStarts()">Newer</button>':""}${s.next?'<button class="ghost sm" onclick="pageActivityStarts(ACTIVITY_STARTS.next)">Older</button>':""}<button class="ghost sm" onclick="refreshActivityStarts(true)"${s.busy?" disabled":""}>${s.busy?"Reading…":"Refresh"}</button></div>`;
  const body=s.error?`<div class="clrefusal" role="alert">${esc(s.error)}</div>`
    :!rows.length?`<div class="empty">${s.observed?"No starts recorded on the nodes that answered.":"Reading starts from every node…"}</div>`
    :`<div class="tbl"><table class="activity-starts"><thead><tr><th>When</th><th>Title</th><th>Client</th><th>Route</th><th>Outcome</th><th>Owner</th><th>To first frame</th><th>Index</th></tr></thead><tbody>${rows.map((row,i)=>activityStartsRowHtml(row,i,names||{})).join("")}</tbody></table></div>`;
  return `<h2 class="section">Starts</h2><p class="sub">Every playback start the cluster handled, newest first, with where its time went.</p>
    ${activityStartsNodesHtml(s,names||{})}${activityStartsObservationHtml(s)}${s.actionError?`<div class="clrefusal" role="alert">${esc(s.actionError)}</div>`:""}${body}${nav}`;
}
function activityStartsPaneHtml(names){
  if(!ME?.is_admin)return "";
  return `<section id="activity-starts" role="tabpanel" aria-labelledby="activity-tab-starts"${ACTIVITY_VIEW.tab!=="starts"?" hidden":""}><div id="activity-starts-body">${activityStartsHtml(ACTIVITY_STARTS,names)}</div></section>`;
}
function paintActivityStarts(){
  if(location.hash!=="#/activity")return;
  const host=document.getElementById("activity-starts-body");
  if(host)host.innerHTML=activityStartsHtml(ACTIVITY_STARTS,ACTIVITY_STARTS.names);
}
async function refreshActivityStarts(force=false){
  const s=ACTIVITY_STARTS,epoch=s.epoch;
  if(!ME||!ME.is_admin||location.hash!=="#/activity"||s.busy||(!force&&Date.now()-s.observed<10000))return;
  s.busy=true;paintActivityStarts();
  try{
    const query=new URLSearchParams({limit:"25"});if(s.cursor)query.set("before",String(s.cursor));
    const page=await api(`/activity/starts?${query}`);
    if(epoch!==s.epoch||location.hash!=="#/activity")return;
    s.rows=page.rows||[];s.nodes=page.nodes||[];s.unavailable=page.unavailable||[];s.observation=page.observation||null;
    s.next=page.next_before||null;s.names=page.node_hostnames||{};s.directoryUnavailable=!!page.directory_unavailable;
    s.observed=Date.now();s.error=null;
  }catch(error){if(epoch===s.epoch)s.error=error.message||String(error);}
  finally{s.busy=false;paintActivityStarts();}
}
function pageActivityStarts(cursor,back=false){
  const s=ACTIVITY_STARTS;
  if(!back){if(cursor===null)s.history=[];else s.history.push(s.cursor);}
  s.cursor=cursor;s.next=null;s.rows=[];s.observed=0;s.epoch++;s.busy=false;
  paintActivityStarts();return refreshActivityStarts(true);
}
function previousActivityStarts(){
  const s=ACTIVITY_STARTS;
  if(s.history.length)return pageActivityStarts(s.history.pop(),true);
}
// The action is the server's own: a retry of the failed job or a request for
// the index. Nothing here decides what to run, so a stale button cannot start
// work the server did not offer for that row.
async function runActivityStartsAction(i){
  const s=ACTIVITY_STARTS,row=s.rows[i],action=row?.index?.action;
  if(!ME?.is_admin||!row||s.acting!=null||!action||!action.path||!/^\/api\/v1\//.test(action.path))return;
  s.acting=i;s.actionError=null;paintActivityStarts();
  try{
    await api(action.path.replace(/^\/api\/v1/,""),{method:action.method||"POST",body:action.body||{}});
    s.observed=0;
  }catch(error){s.actionError=`Could not request indexing: ${error.message||String(error)}`;}
  finally{s.acting=null;paintActivityStarts();}
  return refreshActivityStarts(true);
}
