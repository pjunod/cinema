"use strict";
// ---- analysis status -----------------------------------------------------
// A dedicated operator workspace backed by the replicated request and index-
// job rows. It keeps live work, failures and recent history in one model, then
// provides smaller projections to Activity and Settings.
let ANALYSIS_BUSY=null, ANALYSIS_PENDING=null, ANALYSIS_SUMMARY_BUSY=null, ANALYSIS_SNAPSHOT=null, ANALYSIS_SUMMARY=null, ANALYSIS_ROW_LOOKUP=new Map(), ANALYSIS_SEARCH_TIMER=0;
let ANALYSIS_VIEW={filter:"all",query:"",page:1,pageSize:25,auto:true,cursors:[""]};
async function viewAnalysis(generation=++PAGE_RENDER_GENERATION){
  if(!ME||!ME.is_admin){ location.hash="#/activity"; return; }
  layoutChrome("activity",`<div class="analysis-head"><div><h1>Content analysis</h1>
      <p>VOD HLS readiness, active index work, and recent results across the cluster.</p></div></div>
    <div class="empty" aria-busy="true">Loading the analysis queue…</div>`);
  setPagePhase("#/analysis",generation,"shell");
  if(ANALYSIS_SNAPSHOT){ paintAnalysis(ANALYSIS_SNAPSHOT); setPagePhase("#/analysis",generation,"content"); }
  await renderAnalysis(generation);
  if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/analysis") return;
  setPageTimer(()=>{ if(ANALYSIS_VIEW.auto) renderAnalysisSummary(generation); },3000,generation);
}
function analysisPageUrl(){
  const params=new URLSearchParams({limit:String(ANALYSIS_VIEW.pageSize),filter:ANALYSIS_VIEW.filter});
  if(ANALYSIS_VIEW.query.trim()) params.set("q",ANALYSIS_VIEW.query.trim());
  const cursor=ANALYSIS_VIEW.cursors[ANALYSIS_VIEW.page-1]; if(cursor) params.set("cursor",cursor);
  return `/analysis/jobs?${params}`;
}
async function renderAnalysis(generation=PAGE_RENDER_GENERATION,force=false){
  if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/analysis"||document.visibilityState==="hidden") return;
  const requestKey=analysisPageUrl();
  if(ANALYSIS_BUSY){
    ANALYSIS_PENDING={generation,requestKey};
    if(force||ANALYSIS_BUSY.generation!==generation||ANALYSIS_BUSY.requestKey!==requestKey) ANALYSIS_BUSY.controller.abort();
    return;
  }
  const run={generation,requestKey,controller:new AbortController()}; ANALYSIS_BUSY=run; ANALYSIS_PENDING=null;
  try{
    const [snapshot,summary]=await Promise.all([
      api(requestKey,{signal:run.controller.signal}),
      api("/analysis/summary",{signal:run.controller.signal}).catch(error=>{
        if(error&&error.name==="AbortError") throw error;
        return {available:false,enabled:false};
      }),
    ]);
    if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/analysis") return;
    if(analysisPageUrl()!==requestKey){ ANALYSIS_PENDING={generation,requestKey:analysisPageUrl()}; return; }
    if(!snapshot.rows.length&&snapshot.filtered_total>0&&ANALYSIS_VIEW.page>1){
      ANALYSIS_VIEW.page=1; ANALYSIS_VIEW.cursors=[""]; ANALYSIS_PENDING={generation,requestKey:analysisPageUrl()}; return;
    }
    ANALYSIS_VIEW.cursors[ANALYSIS_VIEW.page]=snapshot.next_cursor||null;
    ANALYSIS_VIEW.cursors.length=ANALYSIS_VIEW.page+1;
    snapshot.summary=summary; ANALYSIS_SUMMARY=summary; ANALYSIS_SNAPSHOT=snapshot; paintAnalysis(snapshot);
    setPageFailure("#/analysis",generation,null); setPagePhase("#/analysis",generation,"content"); setPagePhase("#/analysis",generation,"settled");
  }catch(error){
    if(error&&error.status===401) return;
    if(error&&error.name==="AbortError") return;
    if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/analysis") return;
    if(analysisPageUrl()!==requestKey){ ANALYSIS_PENDING={generation,requestKey:analysisPageUrl()}; return; }
    const main=document.getElementById("main");
    if(main&&ANALYSIS_SNAPSHOT){
      let stale=document.getElementById("analysis-stale");
      if(!stale){ stale=document.createElement("div"); stale.id="analysis-stale"; stale.className="analysis-stale"; stale.setAttribute("role","status"); stale.setAttribute("aria-live","polite"); main.prepend(stale); }
      stale.textContent=`Showing the last update — refresh failed: ${error.message}`;
    }else if(main) main.innerHTML=`<div class="empty">${esc(error.message)}<div style="margin-top:12px"><button class="ghost sm" onclick="renderAnalysis()">Retry</button></div></div>`;
    setPageFailure("#/analysis",generation,"render_error"); setPagePhase("#/analysis",generation,"content"); setPagePhase("#/analysis",generation,"settled");
  }finally{
    if(ANALYSIS_BUSY===run) ANALYSIS_BUSY=null;
    const pending=ANALYSIS_PENDING;
    if(pending&&pending.generation===PAGE_RENDER_GENERATION&&location.hash==="#/analysis"&&document.visibilityState!=="hidden"){
      ANALYSIS_PENDING=null; Promise.resolve().then(()=>renderAnalysis(pending.generation,true));
    }
  }
}
async function renderAnalysisSummary(generation=PAGE_RENDER_GENERATION){
  if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/analysis"||document.visibilityState==="hidden"||!ANALYSIS_SNAPSHOT) return;
  if(ANALYSIS_SUMMARY_BUSY){ if(ANALYSIS_SUMMARY_BUSY.generation!==generation) ANALYSIS_SUMMARY_BUSY.controller.abort(); return; }
  const run={generation,controller:new AbortController()}; ANALYSIS_SUMMARY_BUSY=run;
  try{
    const summary=await api("/analysis/summary",{signal:run.controller.signal});
    if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/analysis") return;
    ANALYSIS_SUMMARY=summary;
    ANALYSIS_SNAPSHOT={...ANALYSIS_SNAPSHOT,summary,enabled:summary.enabled,now_ms:summary.now_ms};
    paintAnalysis(ANALYSIS_SNAPSHOT);
  }catch(error){
    if(error&&(error.status===401||error.name==="AbortError")) return;
    if(generation===PAGE_RENDER_GENERATION&&location.hash==="#/analysis"){
      ANALYSIS_SNAPSHOT.summary={...(ANALYSIS_SNAPSHOT.summary||{}),available:false}; paintAnalysis(ANALYSIS_SNAPSHOT);
    }
  }finally{ if(ANALYSIS_SUMMARY_BUSY===run) ANALYSIS_SUMMARY_BUSY=null; }
}
function analysisStateLabel(state){
  return ({queued:"Queued",claimed:"Claimed",running:"Running",retry_wait:"Retry wait",staged:"Staged",submitted:"Publishing",published:"Ready",ready:"Ready",failed:"Failed",canceled:"Canceled",cancelled:"Canceled",stale:"Stale"})[state]||state;
}
function analysisPhase(row){
  if(row.component==="skip_markers"&&row.request_state==="running") return "Correlating and persisting timeline markers";
  if(row.component==="skip_markers"&&row.state==="ready") return "Timeline markers verified and published";
  if(row.request_state==="running") return "Hashing and verifying the source file";
  if(row.job_state==="running") return "Building the fragment index";
  if(row.job_state==="queued") return "Waiting for an idle media worker";
  if(row.disposition==="automatic") return "Retry scheduled automatically";
  if(row.request_state==="queued") return "Waiting to inspect the source";
  if(row.request_state==="submitted"&&!row.job_id) return "Handed to the index worker";
  if(row.state==="failed") return "Stopped with an error";
  if(row.state==="ready") return "Published and available";
  if(row.state==="cancelled") return "Canceled before completion";
  return "Resolving analysis request";
}
function analysisErrorInfo(code){
  const known={
    foreground_preempted:["Paused for playback","A playback or offline job needed the shared media worker.","No action is needed; analysis retries automatically when the worker is idle.","warn"],
    source_unavailable:["Source file is unavailable","The node could not open the media file at its catalogued location.","Check that the library mount is online and the file is readable, then retry.","bad"],
    source_catalog_read_failed:["Catalog read failed","The server could not load the file or probe facts needed to plan analysis.","Check storage health and server logs, then retry.","bad"],
    source_attestation_failed:["Source verification failed","The file could not be read consistently enough to prove its content identity.","Check the mount and whether another process is replacing the file, then retry.","bad"],
    source_attestation_timeout:["Source verification timed out","Verification reads a fixed 64 MiB sample of the source, so ten minutes without finishing means the mount stopped answering rather than that the file is large.","Check that the library mount is online and responding, then retry. This spends an attempt, so a mount that stays unreachable will exhaust the retry budget.","bad"],
    source_record_failed:["Source identity could not be saved","The source was verified, but its identity could not be committed to the store.","Check database or cluster health, then retry.","bad"],
    source_superseded:["Source changed after this request","The catalog now points at a newer version of the file.","Queue a fresh analysis for the current file.","warn"],
    source_changed:["Source changed during analysis","The file changed while the index was being built, so the result was discarded.","Wait for the file copy or replacement to finish, then retry.","warn"],
    source_deleted:["Source was removed","The catalog file was deleted while analysis was pending.","No action is needed unless the deletion was unexpected.","warn"],
    pipeline_superseded:["Analysis pipeline changed","The server’s index pipeline changed while this job was running.","Retry to build with the current pipeline.","warn"],
    invalid_cache_identity:["Invalid cache identity","The server could not derive a safe content-addressed key for this source.","Inspect server logs; retry after correcting the source metadata.","bad"],
    queue_write_failed:["Queue update failed","The analysis request could not be handed to the shared index queue.","Check database or cluster health; the request will retry automatically.","bad"],
    queue_full_or_busy:["Analysis queue is busy","The shared index queue could not accept another handoff yet.","No action is needed; this request retries automatically.","warn"],
    job_terminal:["The index job for this file already failed","A worker job for exactly this source and pipeline is already terminal, so this request cannot hand off to it. Repeating would retry forever without ever getting further.","Read the attempt history on that job for what it hit, fix that, then reopen the failed work in bulk from the Attention filter.","bad"],
    lease_expired:["The claim lapsed","The node that claimed this work stopped renewing its lease before it finished.","Another node reclaims it automatically; repeated lapses mean the owner is overloaded, restarting, or losing the store.","warn"],
    queue_expired:["Waited too long for a slot","The job sat queued past its eligibility window without any node claiming it.","Check that the target node is up and indexing is enabled there, then retry.","warn"],
    node_local_refusal:["Returned by the node that held it","This node could not reach the source, so it handed the claim back without spending a retry.","No action: another node with the mount will take it. Persisting means no node can read the file.","warn"],
    attempt_limit:["Retry limit reached","Repeated source or store failures exhausted the automatic retry budget.","Every attempt is listed below; resolve what those codes name, then retry manually.","bad"],
    index_budget_exceeded:["Index build timed out","The selected-video index pass exceeded its per-file budget.","The bounded retry cycle will try again at the time shown below.","warn"],
    index_probe_timeout:["Video timing probe timed out","The bounded metadata probe could not establish the selected video duration in time.","The bounded retry cycle will try again at the time shown below.","warn"],
    index_source_io:["Source read failed","The held source could not be read while building the video index.","Transient I/O failures retry automatically; terminal failures need storage inspection.","bad"],
    index_process_failed:["Index process failed","The index process did not exit successfully; its bounded exit detail is shown in Technical details.","Transient resource failures retry automatically; other exits need server-log inspection.","bad"],
    index_video_shortfall:["Selected video ended early","The process exited successfully, but indexed video coverage did not reach the selected video duration.","Inspect the expected and covered durations before starting a new generation.","bad"],
    index_completion_unverified:["Video completion could not be verified","Selected-stream metadata or timeline semantics could not prove that the output covered the complete video.","Review the timing provenance; this is unavailable evidence, not proof of damaged media.","bad"],
    index_output_malformed:["Index output was malformed","The process exited successfully, but its fragment stream was incomplete or invalid.","Inspect the bounded process diagnostics before retrying.","bad"],
    index_retry_window_expired:["Index retry window ended","The fixed seven-day retry cycle ended before another safe attempt could complete.","Start an explicit administrator generation after resolving the underlying attempt history.","bad"],
    unsupported:["VOD analysis is not supported","The file’s codec or fragment structure cannot produce a safe VOD HLS index.","Live HLS can be used when live recovery is enabled in Playback settings; rebuilding will not help unless the source changes.","bad"],
    truncated:["Legacy index output was incomplete","The original record did not preserve enough detail to identify why analysis stopped early.","Preview the video-completion repair to resolve the exact pipeline without replacing successful siblings.","bad"],
    encode_failed:["Index encoding failed","The index was built but could not be serialized into the cache artifact.","Inspect server logs and retry; repeated failures need a server fix.","bad"],
    local_publish_failed:["Index could not be published","The completed artifact could not be written to this node’s index cache.","Check free space and permissions on the cache directory, then retry.","bad"],
  };
  const parts=known[code]||[String(code||"Analysis failed").replace(/_/g," "),"The analysis pipeline reported an unrecognized error code.","Copy the diagnostics and inspect the server logs before retrying.","bad"];
  return {title:parts[0],detail:parts[1],next:parts[2],tone:parts[3]};
}
function analysisErrorHtml(code,stage){
  if(!code) return "";
  const info=analysisErrorInfo(code);
  return `<div class="analysis-error ${info.tone}"><b>${esc(info.title)}</b><p>${esc(info.detail)} ${esc(info.next)}</p><code>${stage?esc(stage)+" · ":""}${esc(code)}</code></div>`;
}
function analysisRows(snapshot){
  return (snapshot&&snapshot.rows)||[];
}
function analysisCounts(value){
  const summary=value&&value.summary?value.summary:(value||{});
  return {...summary,active:summary.active??summary.working??0,attention:summary.attention??summary.failed??0,expected:summary.expected??0};
}
// The queue's own verdict, in one sentence, before any of the counts.
//
// The counts have always been here. What was missing is the thing that turns
// "12,193 claims, 9,915 lost leases, nothing built in three days" into a
// sentence somebody reads: a queue that fails every job it claims looks, in a
// list of jobs, exactly like a queue with a lot of history.
// Every figure here is fleet-wide: the jobs table is replicated and carries
// no record of which node watched a transition, so every node renders the same
// sentence. Saying "this node" would send an operator to restart the one
// machine they happen to be looking at.
function analysisVerdictLine(value){
  const health=value&&value.health;
  if(!health||!health.verdict) return "";
  const ready=health.ready_24h||0, claimed=health.claimed_24h||0, waiting=health.claimable||0;
  const age=health.last_ready_at_ms?fmtAgo(Math.floor(health.last_ready_at_ms/1000)):"never";
  const plural=(n,word)=>`${n} ${word}${n===1?'':'s'}`;
  // A queue that never got as far as a claim and a queue that claims and
  // fails are the same outage wearing different faces. Name whichever one the
  // numbers actually show, because they point at different first moves.
  const deadDetail=claimed===0&&waiting>0
    ? `${plural(waiting,'job')} waiting and nothing picked up in 24 hours. Last index built ${age}.`
    : `${plural(claimed,'job')} picked up in 24 hours and nothing finished. Last index built ${age}.`;
  const said={
    dead:["bad","The index queue is not producing",deadDetail],
    degraded:["warn","The index queue is struggling",
      `${ready} built in the last 24 hours against ${plural(claimed,'job')} picked up, with ${plural(health.lease_losses_since_start||0,'lost lease')} and ${health.attempt_limit_24h||0} out of retries. Last index built ${age}.`],
    healthy:["ready","The index queue is producing",
      `${ready} built in the last 24 hours. Last index built ${age}.`],
    idle:["cancelled","No index completions reported in the last 24 hours",
      "This health sample reports no claims or completions. Queued and running counts below describe retained work and can differ from this sample."],
  }[health.verdict];
  if(!said) return "";
  const [tone,title,detail]=said;
  return `<div class="analysis-verdict ${tone}"><b>${esc(title)}</b><p>${esc(detail)}</p></div>`;
}
function analysisSummaryCard(value,context="activity"){
  const s=analysisCounts(value), attention=s.attention||0, active=s.active||0, expected=s.expected||0;
  const headline=s.available===false?"Analysis status is unavailable"
    :!s.enabled?"Analysis is paused"
    :attention?`${attention} recent result${attention===1?' needs':'s need'} attention`
    :active?`${active} unfinished analysis job${active===1?'':'s'}`
    :s.ready?"Recent analysis results look healthy":"No recent analysis results";
  const detail=s.available===false?"Activity is still available. Refresh or open the workspace to retry analysis health separately."
    :!s.enabled?"Existing indexes still serve, but new VOD HLS indexes will not be built."
    :attention?"Open the workspace for this recent failure and the exact full-history Attention filter."
    :active?"Source verification and index builds yield to foreground playback automatically."
    :s.ready?`${s.ready} recent file${s.ready===1?' is':'s are'} ready for VOD HLS. The workspace retains the full searchable history.`
    :"This compact summary covers active work and recent terminal results; open the workspace for exact retained history.";
  const latest=s.latest_error&&s.latest_error.code?analysisErrorInfo(s.latest_error.code):null;
  return `<div class="analysis-brief">${analysisVerdictLine(value&&value.summary?value.summary:value)}<div class="analysis-brief-head"><div><h2>${esc(headline)}</h2><p>${esc(detail)}</p></div>
      <div class="analysis-actions"><a class="ghost sm" href="#/analysis">Open workspace</a>${context==="activity"?`<button class="ghost sm" onclick="openAnalysisSettings()">Settings</button>`:""}</div></div>
    <div class="analysis-brief-status">
      <span class="analysis-state running">${s.available===false?"—":s.running??"—"} running</span>
      <span class="analysis-state queued">${s.available===false?"—":s.queued??"—"} queued</span>
      <span class="analysis-state submitted">${s.available===false?"—":s.submitted??"—"} submitted</span>
      <span class="analysis-state failed">${attention} recent attention</span>
      <span class="analysis-state cancelled">${expected} recent expected</span>
      <span class="analysis-state ready">${s.ready||0} recent ready</span>
      <span class="analysis-state ${s.enabled?'ready':'cancelled'}">Queue ${s.enabled?'enabled':'paused'}</span>
    </div>${latest&&attention?`<div class="analysis-sub">Latest problem: <b>${esc(latest.title)}</b></div>`:""}</div>`;
}
// The index pass is also keeping this file's PGS tracks for the stored
// subtitle tracks: the row says so, with what it has written, and where that
// work is turned off — background work is attributable from where it shows.
function analysisRideAlong(row){
  const tracks=Number(row.pgs_tracks||0);
  if(!tracks) return "";
  return `<div class="analysis-sub">Also keeping ${tracks} PGS track${tracks===1?"":"s"} · ${fmtBytes(row.pgs_bytes_written)||"0 B"} written · <a href="#/settings/developer">turn off</a></div>`;
}
function analysisLiveProgress(value,names){
  const rows=(value&&value.progress)||[];
  if(!rows.length) return "";
  return `<div class="analysis-table-wrap"><table class="analysis-table"><thead><tr><th>File</th><th>Stage</th><th>Progress</th><th>Node</th></tr></thead><tbody>${rows.map(row=>{
    const title=row.item_id?`<a href="#/item/${row.item_id}">${esc(row.title||`File ${row.file_id}`)}</a>`:esc(row.title||`File ${row.file_id}`);
    const bytes=`${fmtBytes(row.bytes_read)||"0 B"}${row.total_bytes?` / ${fmtBytes(row.total_bytes)}`:""}`;
    const media=row.total_media_ms?`${fmtDur(row.media_ms_examined)||"0s"} / ${fmtDur(row.total_media_ms)}`:(fmtDur(row.media_ms_examined)||"—");
    const rate=row.throughput_bps?`${fmtBytes(row.throughput_bps)}/s`:"—";
    const eta=row.eta_ms!=null?fmtDur(row.eta_ms):"—";
    return `<tr><td><b>${title}</b><div class="analysis-sub">File ${row.file_id} · ${esc(row.component||"analysis")}</div></td>
      <td><span class="analysis-state running">${esc((row.stage||"running").replace(/_/g," "))}</span><div class="analysis-sub">${row.fragments_indexed||0} fragments</div>${analysisRideAlong(row)}</td>
      <td><b>${bytes}</b><div class="analysis-sub">Media ${media} · ${rate} · elapsed ${fmtDur(row.elapsed_ms)||"0s"} · ETA ${eta}</div></td>
      <td>${analysisNodeCell(names,row.node_id)}</td></tr>`;
  }).join("")}</tbody></table></div>`;
}
// The analysis workspace attributes every row to a node, and printed the id
// raw in three places. It shares `nodeLabel` with the Now playing table but not
// the cell shape: this row already carries a "Technical details" disclosure, so
// the name goes in the column and the id keeps a labelled line in there —
// visible, selectable, and in the Copy details text — instead of being repeated
// under every cell in a table that is already four rows deep per record.
function analysisNodeCell(names,nodeId){
  if(!nodeId) return `<span class="clid">—</span>`;
  const name=nodeLabel(names,nodeId);
  return name===nodeId
    ? `<span class="clid">${esc(nodeId)}</span>`
    : `<span class="nodename">${esc(name)}</span>`;
}
// Both, for the places an operator reads the row rather than scans it. A named
// node still shows its id, because the id is what a support question quotes.
function analysisNodeDetail(names,nodeId){
  if(!nodeId) return null;
  const name=nodeLabel(names,nodeId);
  return name===nodeId?nodeId:`${name} (${nodeId})`;
}
function analysisRowKey(row){
  return row.row_key;
}
function analysisDisposition(row){
  if(row.disposition) return row.disposition;
  const code=row.job_error_code||row.request_error_code||"";
  if(["queued","running","submitted"].includes(row.state)) return code?"automatic":"working";
  if(row.state==="ready") return "ready";
  if(code==="unsupported") return "unsupported";
  if(["source_deleted","source_superseded"].includes(code)) return "expected";
  return "attention";
}
function analysisErrors(row){ return [row.request_error_code?{stage:"Request",code:row.request_error_code}:null,row.job_error_code?{stage:"Index build",code:row.job_error_code}:null].filter(Boolean); }
// The code each charged attempt ended with, oldest first. `job_error_code` is
// only the terminal one, and for an exhausted budget that is always
// `attempt_limit`, which names no cause at all.
function analysisAttemptHistory(row){
  const history=row&&row.job_attempt_errors;
  return Array.isArray(history)?history.filter(Boolean):[];
}
function analysisAttemptHistoryHtml(row){
  const history=analysisAttemptHistory(row);
  if(!history.length) return "";
  const items=history.map((code,index)=>`<li><span>${index+1}</span><code>${esc(code)}</code> ${esc(analysisErrorInfo(code).title)}</li>`).join("");
  // The heading is not decoration: a row that eventually succeeded still
  // carries the attempts it took, and an unlabelled list under a green Ready
  // badge reads as a live failure.
  return `<div class="analysis-attempts-head">Attempts so far</div><ol class="analysis-attempts">${items}</ol>`;
}
function analysisAction(row){
  if(row.action) return row.action;
  const code=row.job_error_code||row.request_error_code||"";
  if(code==="source_superseded") return "analyze_current";
  if(row.state==="ready") return "rebuild";
  return analysisDisposition(row)==="attention"?"retry":"none";
}
function analysisCanRetry(row){ return ["retry","analyze_current"].includes(analysisAction(row)); }
function resetAnalysisPage(){ ANALYSIS_VIEW.page=1; ANALYSIS_VIEW.cursors=[""]; }
function setAnalysisFilter(filter){ ANALYSIS_VIEW.filter=filter; resetAnalysisPage(); renderAnalysis(PAGE_RENDER_GENERATION,true); }
function setAnalysisQuery(query){
  ANALYSIS_VIEW.query=query; resetAnalysisPage(); clearTimeout(ANALYSIS_SEARCH_TIMER);
  ANALYSIS_SEARCH_TIMER=setTimeout(()=>renderAnalysis(PAGE_RENDER_GENERATION,true),250);
}
function setAnalysisPage(page){
  page=Math.max(1,Number(page)||1); if(page>1&&!ANALYSIS_VIEW.cursors[page-1]) return;
  ANALYSIS_VIEW.page=page; renderAnalysis(PAGE_RENDER_GENERATION,true);
}
function setAnalysisPageSize(size){ ANALYSIS_VIEW.pageSize=Math.max(10,Math.min(100,Number(size)||25)); resetAnalysisPage(); renderAnalysis(PAGE_RENDER_GENERATION,true); }
function toggleAnalysisAuto(){ ANALYSIS_VIEW.auto=!ANALYSIS_VIEW.auto; if(ANALYSIS_SNAPSHOT) paintAnalysis(ANALYSIS_SNAPSHOT); }
async function refreshAnalysisNow(btn){
  const label=btn&&btn.textContent; if(btn){btn.disabled=true;btn.textContent="Refreshing…";}
  try{ await renderAnalysis(PAGE_RENDER_GENERATION,true); }
  finally{ if(btn&&document.body.contains(btn)){btn.disabled=false;btn.textContent=label;} }
}
function openAnalysisSettings(){
  try{ localStorage.setItem("plurx_settings_tab","analysis"); }catch(error){}
  if(settingsTab()==="analysis"&&isSettingsRoute(location.hash)) viewSettings(++PAGE_RENDER_GENERATION,false);
  else location.hash="#/settings/analysis";
}
function analysisDiagnosticText(row,names){
  const diagnostic=row.index_diagnostic||{};
  const trigger=row.request_id?(row.force_rebuild?"Operator rebuild":"Operator"):"Background";
  return [
    `Title: ${row.title||`File ${row.file_id}`}`,
    `File: ${row.file_id}`,
    `State: ${analysisStateLabel(row.state)}`,
    `Disposition: ${analysisDisposition(row)}`,
    `Phase: ${analysisPhase(row)}`,
    `Trigger: ${trigger}`,
    row.request_id?`Request: ${row.request_id}`:null,
    row.job_id?`Job: ${row.job_id}`:null,
    row.owner_node_id?`Owner: ${analysisNodeDetail(names,row.owner_node_id)}`:null,
    row.target_node_id?`Target: ${analysisNodeDetail(names,row.target_node_id)}`:null,
    row.pipeline_version?`Pipeline: ${row.pipeline_version}`:null,
    `Attempts: ${row.attempts||0}`,
    ...analysisErrors(row).map(error=>`${error.stage} error: ${error.code}`),
    analysisAttemptHistory(row).length?`Attempt history: ${analysisAttemptHistory(row).join(", ")}`:null,
    row.index_retry_deadline_ms?`Retry deadline: ${new Date(row.index_retry_deadline_ms).toISOString()}`:null,
    diagnostic.selected_stream!=null?`Selected stream: ${diagnostic.selected_stream}`:null,
    diagnostic.expectation_provenance?`Timing provenance: ${diagnostic.expectation_provenance}`:null,
    diagnostic.expected_ms!=null?`Expected video: ${diagnostic.expected_ms} ms`:null,
    diagnostic.covered_ms!=null?`Covered video: ${diagnostic.covered_ms} ms`:null,
    diagnostic.elapsed_ms!=null?`Elapsed / budget: ${diagnostic.elapsed_ms} / ${diagnostic.budget_ms??"?"} ms`:null,
    diagnostic.output_bytes!=null?`Index output: ${diagnostic.output_bytes} bytes`:null,
    diagnostic.exit_category?`Process exit: ${diagnostic.exit_category}`:null,
    Array.isArray(diagnostic.stderr_tail)&&diagnostic.stderr_tail.length?`stderr tail:\n${diagnostic.stderr_tail.join("\n")}`:null,
  ].filter(Boolean).join("\n");
}
async function copyAnalysisDetails(key,btn){
  const row=ANALYSIS_ROW_LOOKUP.get(key); if(!row) return;
  try{
    // The painted snapshot, which is the one whose rows this lookup holds.
    await navigator.clipboard.writeText(
      analysisDiagnosticText(row,(ANALYSIS_SNAPSHOT&&ANALYSIS_SNAPSHOT.node_hostnames)||{}));
    const label=btn.textContent; btn.textContent="Copied"; setTimeout(()=>{if(document.body.contains(btn))btn.textContent=label;},1200);
  }catch(error){ toast("Couldn’t copy diagnostics"); }
}
async function retryFailedAnalysis(){
  const rows=analysisRows(ANALYSIS_SNAPSHOT||{}).filter(analysisCanRetry);
  if(!rows.length||!confirm(`Retry ${rows.length} analysis ${rows.length===1?'item':'items'} needing attention on this page?`)) return;
  let queued=0;
  for(const row of rows){
    try{
      if(row.request_id) await api(`/analysis/jobs/${row.request_id}/retry`,{method:"POST",body:{}});
      else await api(`/files/${row.file_id}/analysis`,{method:"POST",body:{force:true,components:[row.component||"fragment_index"]}});
      queued++;
    }
    catch(error){}
  }
  toast(`${queued} of ${rows.length} ${queued===1?'item':'items'} queued`);
  await renderAnalysis(PAGE_RENDER_GENERATION,true);
}
// Bulk reopen, in two steps, because the destructive step is the second one.
//
// "Retry this page" walks the rows already painted and is bounded by what the
// operator can see. This is the tool for the other case: an outage left four
// figures of rows terminal for a reason that has since been fixed, and none of
// them are on this page. The server defaults to a dry run for the same reason
// this asks twice — the count comes back before anything moves.
async function reopenFailedAnalysis(btn){
  const label=btn&&btn.textContent;
  if(btn){btn.disabled=true;btn.textContent="Checking…";}
  try{
    const preview=await api("/analysis/reopen",{method:"POST",body:{dry_run:true,limit:500,component:"fragment_index",repair_revision:"video-completion-v1"}});
    const eligible=(preview.candidates||[]).filter(row=>row.eligibility==="eligible");
    const files=new Set(eligible.map(row=>row.file_id)).size, rows=eligible.length;
    // Nothing reopenable is the normal answer once the queue has recovered:
    // a file whose successor is queued or already built is not offered again,
    // which is what makes this safe to press twice.
    if(!files){ toast("No eligible legacy incomplete indexes found"); return; }
    const more=preview.scan_truncated?" There is more waiting than one run can take; run it again afterwards.":"";
    if(!confirm(`Reopen ${rows} analysis ${rows===1?'item':'items'} across ${files} ${files===1?'file':'files'}?${more}`)) return;
    if(btn) btn.textContent="Reopening…";
    const done=await api("/analysis/reopen",{method:"POST",body:{dry_run:false,limit:500,component:"fragment_index",repair_revision:"video-completion-v1",candidates:eligible}});
    const created=(done.results||[]).filter(row=>row.status==="created").length;
    const skipped=(done.results||[]).length-created;
    toast(`${created} repair ${created===1?'generation':'generations'} queued${skipped?`, ${skipped} changed or already existed`:""}`);
    await renderAnalysis(PAGE_RENDER_GENERATION,true);
  }catch(error){ toast(error.message||"Couldn’t reopen analysis"); }
  finally{ if(btn&&document.body.contains(btn)){btn.disabled=false;btn.textContent=label;} }
}
async function actOnAnalysisRow(key,btn){
  const row=ANALYSIS_ROW_LOOKUP.get(key); if(!row) return;
  const action=analysisAction(row); if(!action||action==="none") return;
  const label=btn&&btn.textContent;
  if(btn){btn.disabled=true;btn.textContent=action==="retry"?"Retrying…":"Queueing…";}
  try{
    if(action==="retry"&&row.request_id){
      await api(`/analysis/jobs/${row.request_id}/retry`,{method:"POST",body:{}});
      toast("Analysis retry queued");
    }else{
      await api(`/files/${row.file_id}/analysis`,{method:"POST",body:{
        force:action==="rebuild"||(action==="retry"&&!row.request_id),components:[row.component||"fragment_index"]}});
      toast("Analysis queued");
    }
    await renderAnalysis(PAGE_RENDER_GENERATION,true);
  }catch(error){ toast(error.message||"Couldn’t queue analysis"); }
  finally{ if(btn&&document.body.contains(btn)){btn.disabled=false;btn.textContent=label;} }
}
async function cancelAnalysisRow(key,btn){
  const row=ANALYSIS_ROW_LOOKUP.get(key); if(!row||!row.request_id) return;
  const label=btn&&btn.textContent; if(btn){btn.disabled=true;btn.textContent="Canceling…";}
  try{ await api(`/analysis/jobs/${row.request_id}`,{method:"DELETE",body:{}}); toast("Analysis canceled"); await renderAnalysis(PAGE_RENDER_GENERATION,true); }
  catch(error){ toast(error.message||"Couldn’t cancel analysis"); }
  finally{ if(btn&&document.body.contains(btn)){btn.disabled=false;btn.textContent=label;} }
}
function paintAnalysis(snapshot){
  const main=document.getElementById("main"); if(!main) return;
  const active=document.activeElement;
  const focusKey=active&&(active.dataset.analysisFocus||(active.id==="analysis-search"?"search":""));
  const selection=focusKey==="search"?[active.selectionStart,active.selectionEnd]:null;
  const open=new Set([...main.querySelectorAll("details[data-analysis-key][open]")].map(detail=>detail.dataset.analysisKey));
  const pageRows=analysisRows(snapshot), counts=analysisCounts(snapshot);
  const filteredTotal=Number(snapshot.filtered_total)||0;
  const pages=Math.max(1,Math.ceil(filteredTotal/ANALYSIS_VIEW.pageSize));
  ANALYSIS_VIEW.page=Math.min(ANALYSIS_VIEW.page,pages);
  ANALYSIS_ROW_LOOKUP=new Map(pageRows.map(row=>[analysisRowKey(row),row]));
  const nodeNames=snapshot.node_hostnames||{};
  const rows=pageRows.map(row=>{
    const owner=row.owner_node_id||row.target_node_id||"";
    const attempt=row.attempts||0, lease=row.lease_expires_ms||0, retryAt=row.not_before_ms||0;
    
    const key=analysisRowKey(row), action=analysisAction(row), displayState=row.durable_state||row.state;
    const actionLabel={retry:"Retry",rebuild:"Rebuild",analyze_current:"Analyze current file"}[action]||"";
    const title=row.item_id?`<a data-analysis-focus="item:${esc(key)}" href="#/item/${row.item_id}">${esc(row.title||`File ${row.file_id}`)}</a>`:esc(row.title||`File ${row.file_id}`);
    const priorityLabel=({normal:"Normal",forced:"Forced",foreground:"Foreground"})[row.priority]||row.priority||"Normal";
    const triggerLabel=({admin:"Admin",background:"Background",foreground:"Foreground"})[row.trigger]||row.trigger||"Background";
    const details=[
      ["File",row.file_id],["Request",row.request_id],["Worker job",row.job_id],
      ["Target node",analysisNodeDetail(nodeNames,row.target_node_id)],
      ["Owner",analysisNodeDetail(nodeNames,row.owner_node_id)],
      ["Lease",lease?(lease>snapshot.now_ms?`expires in ${Math.ceil((lease-snapshot.now_ms)/1000)}s`:"expired"):null],
      ["Retry",row.state==="queued"&&retryAt>snapshot.now_ms?`in ${Math.ceil((retryAt-snapshot.now_ms)/1000)}s`:null],
      ["Priority",priorityLabel],["Trigger",triggerLabel],
      ["Source size",row.source_size!=null?fmtBytes(row.source_size):null],
      ["Pipeline",row.pipeline_version],
      ["Retry deadline",row.index_retry_deadline_ms?new Date(row.index_retry_deadline_ms).toLocaleString():null],
      ["Selected video stream",row.index_diagnostic&&row.index_diagnostic.selected_stream],
      ["Timing provenance",row.index_diagnostic&&row.index_diagnostic.expectation_provenance],
      ["Video coverage",row.index_diagnostic&&row.index_diagnostic.covered_ms!=null?`${row.index_diagnostic.covered_ms} / ${row.index_diagnostic.expected_ms??"?"} ms`:null],
      ["Index output",row.index_diagnostic&&row.index_diagnostic.output_bytes!=null?fmtBytes(row.index_diagnostic.output_bytes):null],
      ["Elapsed / budget",row.index_diagnostic&&row.index_diagnostic.elapsed_ms!=null?`${row.index_diagnostic.elapsed_ms} / ${row.index_diagnostic.budget_ms??"?"} ms`:null],
      ["Process exit",row.index_diagnostic&&row.index_diagnostic.exit_category],
    ].filter(([,value])=>value!=null&&value!=="");
    const errorHtml=analysisErrors(row).map(error=>analysisErrorHtml(error.code,error.stage)).join("");
    return `<tr>
      <td><span class="analysis-file">${title}</span><div class="analysis-sub">${esc(row.component==='skip_markers'?'Timeline markers':'Media index')}</div></td>
      <td><span class="analysis-state ${esc(displayState)}">${esc(analysisStateLabel(displayState))}</span><div class="analysis-phase">${esc(analysisPhase(row))}</div>${errorHtml}${analysisAttemptHistoryHtml(row)}
        <details class="analysis-diag" data-analysis-key="${esc(key)}"${open.has(key)?" open":""}><summary data-analysis-focus="details:${esc(key)}">Technical details</summary><div class="analysis-diag-grid">
          ${details.map(([label,value])=>`<b>${esc(label)}</b><span>${esc(value)}</span>`).join("")}
          <b>Disposition</b><span>${esc(analysisDisposition(row))}</span><b>Attempts</b><span>${attempt}</span><b>Updated</b><span>${row.updated_at_ms?fmtAgo(Math.floor(row.updated_at_ms/1000)):"—"}</span>
        </div></details></td>
      <td>${analysisNodeCell(nodeNames,owner)}<div class="analysis-sub">${row.updated_at_ms?fmtAgo(Math.floor(row.updated_at_ms/1000)):"—"}</div></td>
      <td style="text-align:right"><div class="analysis-actions">${actionLabel?`<button class="ghost sm" data-analysis-focus="action:${esc(key)}" onclick='actOnAnalysisRow(${esc(JSON.stringify(key))},this)'>${actionLabel}</button>`:""}${row.request_id&&["queued","running","submitted"].includes(row.state)?`<button class="ghost sm" data-analysis-focus="cancel:${esc(key)}" onclick='cancelAnalysisRow(${esc(JSON.stringify(key))},this)'>Cancel</button>`:""}<button class="ghost sm" data-analysis-focus="copy:${esc(key)}" onclick='copyAnalysisDetails(${esc(JSON.stringify(key))},this)'>Copy details</button></div></td></tr>`;
  }).join("");
  const attention=counts.attention||0, working=counts.active||0, expected=counts.expected||0;
  const filterButton=(id,label)=>`<button class="${ANALYSIS_VIEW.filter===id?'on':''}" data-analysis-focus="filter:${id}" aria-pressed="${ANALYSIS_VIEW.filter===id}" onclick="setAnalysisFilter('${id}')">${label}${ANALYSIS_VIEW.filter===id?` <b>${filteredTotal}</b>`:""}</button>`;
  const shownFrom=filteredTotal?(ANALYSIS_VIEW.page-1)*ANALYSIS_VIEW.pageSize+1:0;
  const shownTo=pageRows.length?Math.min(shownFrom+pageRows.length-1,filteredTotal):0;
  const summaryWarning=counts.available===false?`<div class="analysis-stale" role="status" aria-live="polite">The recent summary is temporarily unavailable. The history and selected-filter total below are still current.</div>`:"";
  main.innerHTML=`<div class="analysis-head"><div><h1>Content analysis</h1><p>Everything that determines whether a file can use fixed-timeline VOD HLS: source verification, index builds, failures, and recent published results.</p></div>
      <div class="analysis-actions"><span class="analysis-live ${ANALYSIS_VIEW.auto?'':'paused'}" role="status" aria-live="polite">${ANALYSIS_VIEW.auto?'Summary auto-refreshing':'Summary auto-refresh paused'}</span><button class="ghost sm" data-analysis-focus="auto" aria-pressed="${ANALYSIS_VIEW.auto}" onclick="toggleAnalysisAuto()">${ANALYSIS_VIEW.auto?'Pause':'Resume'}</button><button class="ghost sm" data-analysis-focus="refresh" onclick="refreshAnalysisNow(this)">Refresh history</button><button class="ghost sm" data-analysis-focus="settings" onclick="openAnalysisSettings()">Settings</button></div></div>
    ${summaryWarning}
    ${!snapshot.enabled?`<div class="clrefusal"><b>New analysis is paused.</b> Existing VOD HLS indexes still serve. Open <button class="ghost sm" data-analysis-focus="paused-settings" onclick="openAnalysisSettings()">Analysis settings</button> to enable the queue.</div>`:""}
    <div class="analysis-metrics"><div class="analysis-metric ${working?'warn':''}"><span class="n">${counts.available===false?"—":counts.running??"—"}</span><span class="k">Running</span></div><div class="analysis-metric"><span class="n">${counts.available===false?"—":counts.queued??"—"}</span><span class="k">Queued</span></div><div class="analysis-metric ${attention?'bad':''}"><span class="n">${attention}</span><span class="k">Recent attention</span></div><div class="analysis-metric"><span class="n">${expected}</span><span class="k">Recent expected</span></div><div class="analysis-metric good"><span class="n">${counts.ready||0}</span><span class="k">Recent ready</span></div><div class="analysis-metric"><span class="n">${counts.total||0}</span><span class="k">Summary window</span></div></div>
    <div class="analysis-sub">Summary metrics include all active work and up to ${counts.terminal_window||8192} recent terminal worker results. The selected filter count below is exact across retained history.</div>
    <div class="analysis-controls"><div class="analysis-search"><input id="analysis-search" data-analysis-focus="search" aria-label="Search analysis work" placeholder="Search title, file, node id, or error…" value="${esc(ANALYSIS_VIEW.query)}" oninput="setAnalysisQuery(this.value)"></div>
      <div class="analysis-filters">${filterButton("all","All")}${filterButton("working","Unfinished")}${filterButton("attention","Attention")}${filterButton("expected","Expected")}${filterButton("ready","Ready")}</div>
      ${ANALYSIS_VIEW.filter==="attention"&&pageRows.some(analysisCanRetry)?`<button class="ghost sm" data-analysis-focus="retry-page" onclick="retryFailedAnalysis()">Retry this page</button><button class="ghost sm" data-analysis-focus="reopen-all" onclick="reopenFailedAnalysis(this)">Repair legacy incomplete indexes…</button>`:`<span></span>`}</div>
    ${analysisAttentionGroups(pageRows)}
    ${rows?`<div class="analysis-table-wrap"><table class="analysis-table"><thead><tr><th scope="col">File</th><th scope="col">Analysis</th><th scope="col">Node / updated</th><th scope="col">Actions</th></tr></thead><tbody>${rows}</tbody></table></div>`
      :`<div class="empty">${ANALYSIS_VIEW.query||ANALYSIS_VIEW.filter!=="all"?"No analysis work matches these filters.":"Nothing has been queued yet. Use Analyze now on a video’s detail page."}</div>`}
    <div class="analysis-pages"><span>Showing ${shownFrom}–${shownTo} of ${filteredTotal}</span><div class="row"><label>Rows <select data-analysis-focus="page-size" onchange="setAnalysisPageSize(this.value)">${[10,25,50,100].map(size=>`<option value="${size}"${ANALYSIS_VIEW.pageSize===size?' selected':''}>${size}</option>`).join("")}</select></label><button class="ghost sm" data-analysis-focus="previous" ${ANALYSIS_VIEW.page<=1?'disabled':''} onclick="setAnalysisPage(${ANALYSIS_VIEW.page-1})">Previous</button><span>Page ${ANALYSIS_VIEW.page} of ${pages}</span><button class="ghost sm" data-analysis-focus="next" ${!snapshot.next_cursor?'disabled':''} onclick="setAnalysisPage(${ANALYSIS_VIEW.page+1})">Next</button></div></div>`;
  if(focusKey){
    const target=[...main.querySelectorAll("[data-analysis-focus]")].find(element=>element.dataset.analysisFocus===focusKey);
    if(target){ target.focus(); if(selection){try{target.setSelectionRange(selection[0],selection[1]);}catch(error){}} }
  }
}
let ACTIVITY_VIEWING_ID=null;
function selectActivityViewing(id){ACTIVITY_VIEWING_ID=id;if(ACTIVITY_SNAPSHOT)paintActivityBody(ACTIVITY_SNAPSHOT,ACTIVITY_DVR.rows,ACTIVITY_DVR);}
function activityWatchingHtml(deliveries,live,names,sessions,opened){
  const entries=deliveries.map(de=>({key:`delivery:${de.session_id||de.request_id||[de.node_id,de.item_id,de.started_unix].join(':')}`,title:de.title||'Untitled stream',subtitle:de.user||'Viewer unavailable',de}));
  for(const [index,row] of (live||[]).entries())entries.push({key:`live:${row.session_id||row.id||[row.owner_node_id,row.user,row.channel_number,index].join(':')}`,title:row.programme_title||row.channel_name||'Live television',subtitle:[row.user,row.channel_name].filter(Boolean).join(' · '),live:row});
  if(!entries.length)return '<div class="activity-idle">No viewers reported by the responding nodes.</div>';
  const selected=entries.find(e=>e.key===ACTIVITY_VIEWING_ID)||entries[0];ACTIVITY_VIEWING_ID=selected.key;
  const cards=entries.map(e=>`<button class="activity-viewer ${selected===e?'selected':''}" data-activity-view="${esc(e.key)}" aria-pressed="${selected===e}" onclick='selectActivityViewing(${esc(JSON.stringify(e.key))})'><span class="activity-viewer-mark" aria-hidden="true">▶</span><span><strong>${esc(e.title)}</strong><small>${esc(e.subtitle)}</small></span><span class="activity-viewer-state">${e.live?'Live TV':esc(e.de.method?.replace(/_/g,' ')||'Watching')}</span></button>`).join('');
  const de=selected.de,row=selected.live;
  const detail=de?`${de.item_id?`<a class="ghost sm" href="#/item/${de.item_id}">Title details</a>`:''}<dl class="specs"><dt>Viewer</dt><dd>${esc(de.user||'Unavailable')}</dd><dt>Serving node</dt><dd>${activityNodeCell(names,de.node_id)}</dd><dt>Started</dt><dd>${fmtAgo(de.started_unix)}</dd></dl>${activityStreamCell(de,sessions[de.session_id],opened)}${ME&&ME.is_admin&&de.session_id?`<button class="ghost sm" onclick='stopSession(${esc(JSON.stringify(de.session_id))})'>Stop playback</button>`:''}`
    :`<dl class="specs"><dt>Viewer</dt><dd>${esc(row.user||'Unavailable')}</dd><dt>Channel</dt><dd>${esc([row.channel_number,row.channel_name].filter(Boolean).join(' · '))}</dd><dt>Serving node</dt><dd>${activityNodeCell(names,row.owner_node_id)}</dd><dt>State</dt><dd>${esc(row.state||'Unavailable')}</dd><dt>Duration</dt><dd>${esc(Math.round((row.age_seconds||0)/60))} minutes</dd></dl><details><summary>Delivery details</summary><p>${esc(row.encoder||'Encoder unavailable')}${row.output_height?` · ${esc(row.output_height)}p`:''}</p></details><a class="ghost sm" href="#/live-tv">Open Live TV</a>`;
  return `<div class="activity-viewing"><div>${cards}</div><aside class="card activity-viewer-detail"><span class="watch-kicker">Watching</span><h2>${esc(selected.title)}</h2>${detail}</aside></div>`;
}
function analysisAttentionGroups(rows){
  const groups=new Map();
  for(const row of rows){for(const code of new Set(analysisErrors(row).map(e=>e.code).filter(Boolean))){groups.set(code,(groups.get(code)||0)+1);}}
  if(!groups.size)return '';
  return `<section class="analysis-causes"><h2>Problems on this page</h2><p class="muted">These counts cover the displayed rows. Search a cause to inspect its retained history.</p><div class="row">${[...groups].map(([code,count])=>`<button class="ghost" data-analysis-focus="cause:${esc(code)}" onclick='setAnalysisQuery(${esc(JSON.stringify(code))})'>${esc(analysisErrorInfo(code).title)} · ${count}</button>`).join('')}</div></section>`;
}
let SEARCH_DATA=null;
function paintSearchResults(){
  if(!SEARCH_DATA||location.hash!==SEARCH_DATA.route)return;
  const {query,results}=SEARCH_DATA;
  const scope=document.getElementById('search-kind')?.value||'all';
  const list=scope==='all'?results:results.filter(it=>it.kind===scope);
  const equal=it=>String(it.title||'').trim().toLocaleLowerCase()===query.trim().toLocaleLowerCase();
  const exact=list.filter(equal),other=list.filter(it=>!equal(it));
  const groups=[['Exact title matches',exact],...['show','movie','episode','book','audiobook','video','photo','folder'].map(kind=>[({show:'Series',movie:'Movies',episode:'Episodes',book:'Books',audiobook:'Audiobooks',video:'Videos',photo:'Photos',folder:'Folders'})[kind],other.filter(it=>it.kind===kind)])];
  const known=new Set(groups.flatMap(([,items])=>items));const remaining=other.filter(it=>!known.has(it));if(remaining.length)groups.push(['Other results',remaining]);
  document.getElementById('search-results').innerHTML=groups.filter(([,items])=>items.length).map(([name,items])=>`<section class="search-group"><h2>${name} <span class="muted">· ${items.length}</span></h2>${grid(items)}</section>`).join('')||'<div class="empty">No matching titles. Try another search term or choose All types.</div>';
  document.getElementById('search-result-count').textContent=`${list.length} result${list.length===1?'':'s'}`;
  paintSemanticResults();
}
function libraryBrowseTools(libs){
  return `<div class="search-tools"><label>Library <select id="library-scope" onchange="setLibraryScope(this.value)"><option value="">All libraries in this view</option>${libs.map(l=>`<option value="${l.id}">${esc(l.name)}</option>`).join('')}</select></label><label>Find <input id="library-find" type="search" placeholder="Title…" oninput="setLibraryFind(this.value)"></label><label>Posters <select onchange="setIconSize(this.value)">${POSTER_SIZES.map(size=>`<option value="${size[0]}"${posterSize()===size[0]?' selected':''}>${esc(size[1])}</option>`).join('')}</select></label><button class="ghost sm" onclick="clearLibraryBrowse()">Clear filters</button></div>`;
}
let LIB_SCOPE='',LIB_FIND='';
function setLibraryScope(value){LIB_SCOPE=value;LIB_PAGE_AT=0;if(LIB_VIEW)LIB_VIEW.draw(LIB_VIEW.done);}
function setLibraryFind(value){LIB_FIND=value;LIB_PAGE_AT=0;if(LIB_VIEW)LIB_VIEW.draw(LIB_VIEW.done);}
function clearLibraryBrowse(){LIB_SCOPE='';LIB_FIND='';LIB_FILTER='all';LIB_PAGE_AT=0;for(const id of ['library-scope','library-find']){const el=document.getElementById(id);if(el)el.value='';}document.querySelectorAll('[data-library-watch-filter]').forEach(el=>el.value='all');if(LIB_VIEW)LIB_VIEW.draw(LIB_VIEW.done);}

let ACTIVITY_DETAIL_BUSY=0, ACTIVITY_SNAPSHOT=null;
const ACTIVITY_DVR={rows:[],next:null,loaded:false,error:null,loading:false};
const DVR_SHARED={overview:null,error:null,receivedAt:0,busy:null,queued:false,timer:null,
  failures:0,serial:0,scope:null,lastLiveMarkRefresh:0,lastActiveKey:""};
function dvrOverviewValid(value){
  return value&&value.version===1&&["complete","partial","unavailable"].includes(value.availability)
    &&Array.isArray(value.active)&&value.counts&&typeof value.counts==="object";
}
function dvrActiveKey(value){
  return (value&&value.active||[]).map(row=>`${row.recording_id}:${row.display_state}:${row.observation&&row.observation.attempt||0}`).join("|");
}
function dvrSetOverview(value){
  if(!dvrOverviewValid(value)) throw new Error("The DVR overview response is invalid.");
  const prior=DVR_SHARED.lastActiveKey, next=dvrActiveKey(value);
  DVR_SHARED.overview=value;DVR_SHARED.error=null;DVR_SHARED.failures=0;
  DVR_SHARED.receivedAt=performance.now();DVR_SHARED.lastActiveKey=next;
  dvrPaintGlobal();refreshLiveTvDvrUi();
  if(location.hash.startsWith("#/recordings")) paintRecordingsPage();
  // Exact-airing marks come from the schedule document. When an active row
  // appears or disappears, refresh that document at most once in 30 seconds;
  // the five-second overview itself never refetches the guide provider.
  if(location.hash==="#/live-tv"&&prior!==next&&Date.now()-DVR_SHARED.lastLiveMarkRefresh>30000){
    DVR_SHARED.lastLiveMarkRefresh=Date.now();
    loadLiveTvDvr(PAGE_RENDER_GENERATION,location.hash);
  }
}
function dvrOverviewFresh(value,receivedAt,now=performance.now()){
  if(!value||value.availability==="unavailable")return false;
  if(value.availability==="partial"&&value.observation_age_ms==null)return false;
  const clientAge=receivedAt?Math.max(0,now-receivedAt):0;
  return Number(value.observation_age_ms||0)+clientAge<=20000;
}
function dvrIndicatorText(value,fresh=dvrOverviewFresh(value,DVR_SHARED.receivedAt)){
  const counts=value&&value.counts||{};
  const active=["recording","starting","reconnecting","finishing","unconfirmed"]
    .reduce((total,key)=>total+Number(counts[key]||0),0);
  if(!fresh)return active?"Status unavailable":"";
  const parts=[];
  if(counts.recording)parts.push(`${counts.recording} recording`);
  if(counts.starting)parts.push(`${counts.starting} starting`);
  if(counts.reconnecting)parts.push(`${counts.reconnecting} reconnecting`);
  if(counts.finishing)parts.push(`${counts.finishing} finishing`);
  if(counts.unconfirmed)parts.push(`${counts.unconfirmed} unconfirmed`);
  return parts.join(" · ");
}
function dvrPaintGlobal(){
  const mount=document.getElementById("dvr-global");if(!mount)return;
  const value=DVR_SHARED.overview, fresh=dvrOverviewFresh(value,DVR_SHARED.receivedAt);
  const unavailable=!fresh;
  const text=dvrIndicatorText(value,fresh);
  mount.className=`dvr-global${text?" on":""}${unavailable?" unavailable":""}`;
  mount.innerHTML=text?`<span class="rec-dot"></span><span>${esc(text)}</span>`:"";
  mount.title=value&&value.observation_age_ms!=null?`DVR observed ${Math.round(value.observation_age_ms/1000)} seconds ago`:"DVR status";
}
function dvrPollDelay(){
  const value=DVR_SHARED.overview,counts=value&&value.counts||{};
  const active=["recording","starting","reconnecting","finishing","unconfirmed"].some(key=>Number(counts[key])>0);
  const backoff=Math.min(30000,5000*Math.pow(2,Math.min(DVR_SHARED.failures,3)));
  if(active)return Math.max(5000,backoff);
  const start=value&&Number(value.next_capture_start);
  const base=Number.isFinite(start)?Math.max(1000,Math.min(30000,start*1000-Date.now())):30000;
  return Math.max(base,backoff);
}
function dvrSchedulePoll(){
  clearTimeout(DVR_SHARED.timer);DVR_SHARED.timer=null;
  if(document.visibilityState==="hidden"||!TOKEN||!ME)return;
  const route=location.hash||"#/";
  if(route==="#/activity")return; // activity/detail already carries this projection
  DVR_SHARED.timer=setTimeout(()=>dvrRefreshOverview(),dvrPollDelay());
}
async function dvrRefreshOverview(force=false){
  if(!TOKEN||!ME||document.visibilityState==="hidden")return;
  if(DVR_SHARED.busy){DVR_SHARED.queued=DVR_SHARED.queued||force;return DVR_SHARED.busy;}
  const serial=++DVR_SHARED.serial,scope=TOKEN;
  const request=api("/dvr/overview",{signal:AbortSignal.timeout(4000)}).then(value=>{
    if(serial!==DVR_SHARED.serial||scope!==TOKEN)return;
    dvrSetOverview(value);
  }).catch(error=>{
    if(serial!==DVR_SHARED.serial||scope!==TOKEN)return;
    DVR_SHARED.failures++;
    DVR_SHARED.error=error&&error.message||"DVR status could not be loaded";dvrPaintGlobal();refreshLiveTvDvrUi();
    if(location.hash.startsWith("#/recordings"))paintRecordingsPage();
  }).finally(()=>{
    if(DVR_SHARED.busy===request)DVR_SHARED.busy=null;
    if(DVR_SHARED.queued){DVR_SHARED.queued=false;dvrRefreshOverview(true);}else dvrSchedulePoll();
  });
  DVR_SHARED.busy=request;return request;
}
function dvrRouteChanged(route){
  clearTimeout(DVR_SHARED.timer);DVR_SHARED.timer=null;
  if(DVR_SHARED.scope!==TOKEN){
    DVR_SHARED.scope=TOKEN;DVR_SHARED.overview=null;DVR_SHARED.error=null;DVR_SHARED.failures=0;DVR_SHARED.lastActiveKey="";
    DVR_SHARED.serial++;
  }
  if(route==="#/activity")return;
  if(route==="#/live-tv"||route.startsWith("#/recordings"))dvrRefreshOverview(true);
  else dvrSchedulePoll();
}
document.addEventListener("visibilitychange",()=>{
  if(document.visibilityState==="hidden"){clearTimeout(DVR_SHARED.timer);DVR_SHARED.timer=null;}
  else{
    dvrRouteChanged(location.hash||"#/");
    if(dvrDetailRoute()&&DVR_PAGE.selectedId){clearTimeout(DVR_PAGE.historyTimer);refreshDvrHistory(DVR_PAGE.selectedId);}
  }
});
// `/dvr/recordings` deliberately has its own page envelope. Schedule is
// `{conflicts,rows}` and rules are an array, so accepting any object with an
// array somewhere inside it would hide a server/client contract mismatch.
function dvrRecordingsPage(value){
  if(!value||typeof value!=="object"||Array.isArray(value)||!Array.isArray(value.rows))
    throw new Error("The recording list response is invalid.");
  if(value.next!=null&&(typeof value.next!=="string"||value.next.length===0))
    throw new Error("The recording list cursor is invalid.");
  return {rows:value.rows,next:value.next||null};
}
function activityNodeFailures(d){
  return (d.activity_nodes||[]).filter(node=>node.status&&node.status!=="answered");
}
function activityNodeStatusText(status){
  return ({unhealthy:"unhealthy",unreachable:"unreachable",timed_out:"timed out",
    refused:"refused the activity request",http_error:"returned an HTTP error",
    unsupported:"does not publish Activity HTTP",
    invalid_response:"sent an invalid response",unavailable:"directory unavailable"})[status]||"unavailable";
}
// A node id is stable but names nothing: nobody recognizes a machine from
// `5deeeebc-…`. The response carries the roster's id -> short-hostname map, so
// every node the page mentions is labelled with the name its operator uses at a
// shell. The map is passed in rather than kept in a module variable, so a
// snapshot that arrives without one (a household member's read, a roster read
// that failed) cannot be painted with the names of the snapshot before it.
//
// Callers guarantee a node id; the map is what may be missing.
function nodeLabel(names,nodeId){
  const name=names&&names[nodeId];
  return typeof name==="string"&&name?name:nodeId;
}
// The id stays on the page in the same `.clid` treatment it has always had —
// visible, selectable, and read aloud — with the machine name added above it.
// A tooltip would have been shorter and would have taken the id away from
// touch, from selection, and from assistive technology, which is the opposite
// of what the cluster page's own design note asks for: lead with the short
// hostname, keep the node id as a copyable detail.
function activityNodeCell(names,nodeId){
  if(!nodeId) return `<span class="clid">Unknown</span>`;
  const name=nodeLabel(names,nodeId);
  return name===nodeId
    ? `<span class="clid">${esc(nodeId)}</span>`
    : `<div class="nodename">${esc(name)}</div><div class="clid">${esc(nodeId)}</div>`;
}
function activityNodeFailureText(node,names){
  const name=node.node_id?`Node ${nodeLabel(names,node.node_id)}`:"The cluster peer directory";
  return `${name} · ${activityNodeStatusText(node.status)}`;
}
function detailActivitySummary(d,deliveries,recording=[]){
  const acts=[];
  const missing=activityNodeFailures(d);
  if(missing.length) acts.push({label:"Activity incomplete",
    detail:`${missing.length} cluster ${missing.length===1?'node':'nodes'} did not answer`});
  // `/activity` builds these rows itself, with the same `record` kind; the
  // Activity page reads `/activity/detail`, which carries no DVR section, so
  // the pill would lose the one activity that is holding a tuner and a disk.
  for(const row of recording){
    const now=liveTvNowSeconds();
    acts.push({kind:"record",label:`Recording ${row.title}`,
      detail:PlurxLiveTv.dvrRecordingDetail(row,now),
      percent:Math.round(PlurxLiveTv.dvrCaptureProgress(row,now)*100)});
  }
  for(const scan of (d.scans||[])){
    if(scan.status&&scan.status.running) acts.push({label:"Scanning library",detail:scan.library});
  }
  if(deliveries.length) acts.push({label:`${deliveries.length} active stream${deliveries.length===1?'':'s'}`});
  if(d.producing) acts.push({label:"Pre-transcoding",detail:d.producing.title});
  if((d.offline||[]).length) acts.push({label:`${d.offline.length} offline download${d.offline.length===1?'':'s'}`});
  if(d.trakt&&d.trakt.syncing) acts.push({label:"Syncing Trakt"});
  return acts;
}
