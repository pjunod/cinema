"use strict";
// ---- Now playing: the Stream cell -----------------------------------------
// One delivery, read as a card rather than as a " · "-joined run-on. The old
// cell printed every session fact it had in the order the code happened to
// test them — "explicit lease 30s · demand active · position 1693s · client
// runway 43s · production explicit_demand · producer attempt 1 · playlist
// ready · published segment 114 · next sequence 115 · produced ahead 59s ·
// target 73s · active · held (time) · 6 suspends" — so the three questions an
// operator actually brings to this page (is it playing, is the server keeping
// up, is it held and why) were answered somewhere in the middle of a sentence
// of sequence numbers. Now: a state pill answers the first, a strip of named
// meters answers the second (the same labels the player's info panel uses —
// Position, Server ahead, Client runway, Delivery rate, Suspends), the pill's
// clause answers the third, and every sequence number keeps its place behind
// a "Technical details" disclosure that stays open across the 4 s repaint.
//
// A direct play or a progressive remux has no session behind it and renders
// only the headline and whatever delivery numbers it has; nothing here
// invents a state for a stream that never reported one.
function activityMethodLabel(method,encoder){
  const name=({direct:"Direct play",remux:"Remux","hls-copy":"HLS copy",transcode:"Transcode"})[method]||method||"";
  return encoder==="cached"?`${name} · cached`:name;
}
function activityStreamState(session){
  if(!session) return null;
  // `lease_state` is "active" for a live lease and "unavailable" for a legacy
  // viewer with no actor at all; only the three terminal verdicts are news.
  const terminal=({ended:"ended",expired:"expired",authority_fenced:"authority fenced"})[session.lease_state];
  if(terminal) return {cls:"bad",label:`Lease ${terminal}`};
  const hold=!session.suspended?""
    :({demand:"viewer has enough",time:"reserve full",bytes:"per-stream byte limit",
       global:"global scratch limit"})[session.hold_reason]||session.hold_reason||"limit";
  // Failures outrank everything but a dead lease: the old sentence never
  // claimed a verdict, the pill does, so it must not say Active over a
  // producer that has already died.
  if(session.producer_state==="failed") return {cls:"bad",label:"Producer failed"};
  if(session.render_state==="failed") return {cls:"bad",label:"Client failed",why:hold?`held · ${hold}`:""};
  // A stalled client under a server hold is the freeze signature; the pill
  // leads with the client and carries the hold as its clause.
  if(session.render_state==="stalled") return {cls:"bad",label:"Client stalled",why:hold?`held · ${hold}`:""};
  // A viewer's own hold becomes a `demand` suspension on the server, so the
  // viewer's intent is read before the suspension it caused.
  if(session.control_demand==="hold") return {cls:"idle",label:"Paused",why:"viewer asked to hold"};
  if(session.control_demand==="end"||session.render_state==="ended") return {cls:"idle",label:"Ending"};
  if(hold) return {cls:"hold",label:"Holding",why:hold};
  if(session.playlist_ready===false) return {cls:"wait",label:"Starting",why:"playlist not published yet"};
  if(Number.isSafeInteger(session.production_ahead_seconds)&&session.production_ahead_seconds<0)
    return {cls:"bad",label:"Behind",why:`${Math.abs(session.production_ahead_seconds)} s deficit`};
  return {cls:"active",label:"Active"};
}
// The meters. Each is {k,v,of?,tone?,bar?}: a label, a tabular value, an
// optional "of N" clause, a tone for the value, and an optional 0..1 fill.
function activityStreamMeters(de,session){
  const out=[];
  const secs=ms=>`${Math.max(0,Math.round(ms/1000))} s`;
  if(session){
    if(Number.isSafeInteger(session.reported_position_ms))
      out.push({k:"Position",v:clockFromSec(session.reported_position_ms/1000)});
    // Two reserves, under the two labels the player's info panel uses for
    // them, so the pages never disagree about a number. Server ahead is the
    // physical published media beyond the client's download frontier; the
    // Demand window is the explicit policy's production against its target,
    // measured from the viewer's reported playhead, and can run negative.
    if(Number.isSafeInteger(session.ahead_seconds))
      out.push({k:"Server ahead",v:`${Math.max(0,session.ahead_seconds)} s`,
        tone:session.ahead_seconds<=0&&!session.suspended?"warn":""});
    if(session.production_policy==="explicit_demand"){
      const ahead=session.production_ahead_seconds;
      const target=Number.isSafeInteger(session.production_target_seconds)&&session.production_target_seconds>0
        ?session.production_target_seconds:null;
      if(Number.isSafeInteger(ahead)){
        const ratio=target?Math.max(0,ahead)/target:null;
        const tone=ahead<0?"bad":session.suspended||ratio==null?"":ratio<0.25?"warn":"good";
        out.push({k:"Demand window",v:ahead<0?`−${Math.abs(ahead)} s`:`${ahead} s`,
          of:target?`of ${target} s`:"",tone,bar:ratio==null?null:Math.min(1,ratio)});
      }else out.push({k:"Demand window",v:"—",of:"waiting for publication",tone:"warn"});
    }
    if(Number.isSafeInteger(session.client_runway_ms)){
      const runway=session.client_runway_ms;
      out.push({k:"Client runway",v:secs(runway),tone:runway<5000?"bad":runway<12000?"warn":""});
    }
    if(session.suspend_count)
      out.push({k:"Suspends",v:String(session.suspend_count),tone:session.suspended?"warn":""});
  }
  const bps=de.delivered_bps!=null?de.delivered_bps:session&&session.delivered_bps;
  const bytes=de.delivered_bytes!=null?de.delivered_bytes:session&&session.delivered_bytes;
  if(bps||bytes){
    const idle=session&&session.delivered_idle_ms>15000;
    out.push({k:"Delivery rate",v:bps?fmtMbps(bps):"—",of:[bytes?fmtBytes(bytes):"",idle?"idle":""].filter(Boolean).join(" · ")});
  }
  return out;
}
// Everything the run-on used to say, as label/value pairs. Sequence numbers,
// attempts and lease mechanics are what a bug report wants and what a glance
// does not, so they live behind the disclosure — none of them are lost.
function activityStreamDetails(session){
  if(!session) return [];
  const rows=[];
  if(session.lease_mode){
    const timeout=Number.isSafeInteger(session.lease_timeout_ms)?` · ${Math.round(session.lease_timeout_ms/1000)} s`:"";
    rows.push(["Lease",`${session.lease_mode}${timeout}${session.lease_state?` · ${session.lease_state}`:""}`]);
  }
  if(session.control_demand) rows.push(["Demand",session.control_demand]);
  if(session.production_policy) rows.push(["Policy",String(session.production_policy).replace(/_/g," ")]);
  if(Number.isSafeInteger(session.production_target_seconds)&&session.production_target_seconds>0) rows.push(["Target",`${session.production_target_seconds} s`]);
  if(session.suspended){
    const release=session.hold_reason==="time"&&session.resume_below_seconds!=null?`resumes below ${session.resume_below_seconds} s`
      :session.resume_below_bytes!=null?`resumes below ${fmtBytes(session.resume_below_bytes)||"0 B"}`:"";
    rows.push(["Hold reason",`${session.hold_reason||"limit"}${release?` · ${release}`:""}`]);
  }
  if(session.render_state) rows.push(["Render state",String(session.render_state).replace(/_/g," ")]);
  if(typeof session.recent_speed==="number") rows.push(["Encode speed",`${session.recent_speed.toFixed(2)}×`]);
  else if(typeof session.speed==="number") rows.push(["Encode speed",`${session.speed.toFixed(2)}× (avg)`]);
  if(Number.isSafeInteger(session.producer_attempt)) rows.push(["Producer attempt",String(session.producer_attempt)]);
  if(session.producer_state) rows.push(["Producer",String(session.producer_state).replace(/_/g," ")]);
  if(session.playlist_ready===true) rows.push(["Playlist","ready"]);
  else if(session.playlist_ready===false) rows.push(["Playlist","waiting"]);
  if(session.playlist_shape) rows.push(["Playlist shape",session.playlist_shape]);
  if(Number.isSafeInteger(session.published_segment)) rows.push(["Published segment",String(session.published_segment)]);
  if(Number.isSafeInteger(session.next_media_sequence)) rows.push(["Next sequence",String(session.next_media_sequence)]);
  if(Number.isSafeInteger(session.fetched_segment)) rows.push(["Fetched segment",String(session.fetched_segment)]);
  if(Number.isSafeInteger(session.pending_fetched_segment)) rows.push(["Fetched timing pending",String(session.pending_fetched_segment)]);
  if(Number.isSafeInteger(session.ahead_bytes)&&session.ahead_bytes>0) rows.push(["Ahead bytes",fmtBytes(session.ahead_bytes)]);
  if(session.last_request) rows.push(["Last request",session.last_request]);
  if(session.readrate) rows.push(["Pacing",`${Number(session.readrate).toFixed(2)}×`]);
  return rows;
}
// The cell. `open` is the set of disclosure keys that were open before this
// repaint, so a reader halfway through a sequence number is not snapped shut
// by the next poll — the same courtesy the analysis table extends.
function activityStreamCell(de,session,open){
  const presentation=de.presentation==="vod"?"VOD HLS":de.presentation==="live-recovery"?"Live HLS":"";
  const state=activityStreamState(session);
  // `deliveries[]` names the method; the rung and the encoder ride on the
  // session row beside it, so a stream with a session reads them from there.
  const height=session&&session.target_height||de.target_height;
  const encoder=session&&session.encoder||de.encoder;
  const sub=[de.session_id&&height?`${height}p`:"",de.session_id&&encoder&&encoder!=="cached"?encoder:""]
    .filter(Boolean).join(" · ");
  const head=`<div class="stream-head">${presentation?`<span class="mode-chip ${de.presentation==="vod"?'vod':'live'}">${esc(presentation)}</span>`:""}${
    state?`<span class="stream-state ${state.cls}">${esc(state.label)}${state.why?` <span class="why">· ${esc(state.why)}</span>`:""}</span>`:""}<span class="stream-method">${esc(activityMethodLabel(de.method,encoder))}${sub?` <span class="sub">· ${esc(sub)}</span>`:""}</span></div>`;
  const meters=activityStreamMeters(de,session);
  const strip=meters.length?`<div class="stream-meters">${meters.map(meter=>`<div class="stream-meter ${meter.tone||""}"><span class="k">${esc(meter.k)}</span><span class="v">${esc(meter.v)}${meter.of?`<span class="of">${esc(meter.of)}</span>`:""}</span>${
    meter.bar!=null?`<span class="stream-bar" aria-hidden="true"><i style="width:${Math.round(meter.bar*100)}%"></i></span>`:""}</div>`).join("")}</div>`:"";
  const details=activityStreamDetails(session);
  const key=de.session_id?String(de.session_id):"";
  const diag=details.length?`<details class="stream-diag" data-stream="${esc(key)}"${open&&open.has(key)?" open":""}><summary>Technical details</summary><div class="stream-diag-grid">${
    details.map(([label,value])=>`<div><b>${esc(label)}</b><span>${esc(value)}</span></div>`).join("")}</div></details>`:"";
  return head+strip+diag;
}
async function renderActivityBody(generation=PAGE_RENDER_GENERATION){
  if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/activity"||document.visibilityState==="hidden"||ACTIVITY_DETAIL_BUSY===generation) return;
  ACTIVITY_DETAIL_BUSY=generation;
  try{
  // `/activity/detail` carries the exact same bounded DVR projection as the
  // Recordings controller. Activity must not start a duplicate overview poll.
  let detail;
  try{
    detail=await api("/activity/detail");
  }
  catch(e){
    if(e&&e.status===401) return;
    if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/activity") return;
    const m=document.getElementById("main");
    if(ACTIVITY_SNAPSHOT){
      DVR_SHARED.error=e.message;dvrPaintGlobal();paintDvrHost();
      let stale=document.getElementById("activity-stale");
      if(!stale&&m){
        stale=document.createElement("div"); stale.id="activity-stale"; stale.className="hint";
        m.prepend(stale);
      }
      if(stale) stale.textContent=`Showing the last update — refresh failed: ${e.message}`;
    }else if(m) m.innerHTML=`<div class="empty">${esc(e.message)}</div>`;
    setPageFailure("#/activity",generation,"render_error");
    setPagePhase("#/activity",generation,"content"); setPagePhase("#/activity",generation,"settled"); return;
  }
  if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/activity") return;
  const d=detail;
  ACTIVITY_SNAPSHOT=d;
  try{
    if(d.dvr){dvrSetOverview(d.dvr);ACTIVITY_DVR.rows=d.dvr.active||[];}
    else ACTIVITY_DVR.rows=[];
    ACTIVITY_DVR.next=null;
    ACTIVITY_DVR.loaded=true; ACTIVITY_DVR.error=null;
  }catch(e){
    ACTIVITY_DVR.error=e&&e.message?e.message:"The recording list could not be loaded.";
  }
  if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/activity") return;
  paintActivityBody(d,ACTIVITY_DVR.rows,ACTIVITY_DVR);
  loadDvrRecent(generation);
  if(DVR_PAGE.pendingId){const id=DVR_PAGE.pendingId;DVR_PAGE.pendingId=null;selectDvrDetail(id);}
  else if(!DVR_PAGE.selectedId&&!DVR_PAGE.closedByUser&&ACTIVITY_DVR.rows.length&&matchMedia("(min-width:961px)").matches)selectDvrDetail(ACTIVITY_DVR.rows[0].recording_id);
  setPageFailure("#/activity",generation,null);
  setPagePhase("#/activity",generation,"content");
  setPagePhase("#/activity",generation,"settled");
  } finally {
    if(ACTIVITY_DETAIL_BUSY===generation) ACTIVITY_DETAIL_BUSY=0;
  }
}
function paintActivityBody(d,recording=[],dvrState={loaded:true,error:null,next:null}){
  // S2's consumer check. This table read `d.sessions`, which is the transcode
  // manager's list — so it showed transcodes and HLS copy-remuxes (mislabelled
  // as transcodes) and missed direct play and progressive remux entirely. It
  // now reads `d.deliveries`, which is every active delivery with its method
  // recorded rather than inferred: the `encoder` label says "cached" on a cache
  // hit and is rewritten by the hardware-to-software fallback, so a copy remux
  // read off that string would relabel itself mid-play.
  const dels=d.deliveries||(d.sessions||[]).map(se=>({method:"transcode",user:se.user_name,item_id:se.item_id,
    title:se.item_title,started_unix:se.started_unix,idle_seconds:se.idle_seconds,session_id:se.id,
    target_height:se.target_height,encoder:se.encoder}));
  paintActivity(detailActivitySummary(d,dels,recording));
  const missing=activityNodeFailures(d);
  const nodeNames=d.node_hostnames||{};
  const sessionsById=Object.fromEntries((d.sessions||[]).map(se=>[se.id,se]));
  // Disclosures the reader opened survive the repaint. `#main` is read here,
  // before the new markup replaces it, and nothing else on this page is.
  const m=document.getElementById("main"); if(!m) return;
  const dvrUi=dvrRememberUi(m);
  const activityFocus=document.activeElement?.dataset?.activityView;
  const open=new Set([...m.querySelectorAll("details.stream-diag[open]")].map(el=>el.dataset.stream));
  // And the disclosure a keyboard reader is standing on: `innerHTML=` below
  // destroys the focused summary, so it is found again after the paint.
  const focusedDiag=document.activeElement&&document.activeElement.closest
    ?document.activeElement.closest("details.stream-diag"):null;
  const focusKey=focusedDiag?focusedDiag.dataset.stream:null;
  const scans=d.scans.map(sc=>`<tr><td style="width:200px"><b>${esc(sc.library)}</b></td><td class="muted">${statusText(sc.status)}</td></tr>`).join("");
  const t=d.trakt||{};
  const trakt = t.configured
    ? `<p style="margin:0">${t.linked?`Linked as <b>${esc((t.linked.trakt_username)||"?")}</b>${t.linked.last_sync_at?` <span class="muted">· last sync ${fmtAgo(t.linked.last_sync_at)}</span>`:""}`:"Keys saved — not linked yet."}${t.syncing?' <span class="muted">· syncing now…</span>':''}</p>${t.note?`<p class="hint" style="margin:6px 0 0">${esc(t.note)}</p>`:''}`
    : `<p class="muted" style="margin:0">Not set up${ME&&ME.is_admin?' — add it in <a href="#/settings/integrations">Settings</a>':''}.</p>`;
  // The pre-transcode pass, which holds an encoder for as long as anything on
  // this page and until now appeared on none of it. Shown whenever one is
  // running rather than tucked behind a disclosure: an admin who has just
  // found a busy ffmpeg should see the answer without looking for it.
  const p=d.producing;
  const produce = p
    ? `<div class="card"><div class="row" style="justify-content:space-between;gap:12px;align-items:center">
         <div><b>${esc(p.title)}</b><div class="muted" style="font-size:12px">chosen because it is ${esc(p.reason)} · ${p.index} of ${p.total} this pass</div></div>
         ${ME&&ME.is_admin?`<button class="ghost sm" onclick="stopProducer()">Stop</button>`:''}
       </div>
       <p class="hint" style="margin:8px 0 0">Encoding ahead of time so the first press of play is instant. It gives the
       hardware up the moment somebody starts a stream, and stops for good after the current title if you ask it to.</p></div>`
    : `<div class="empty">Not pre-transcoding anything. A pass runs on the schedule in Settings and picks from Continue&nbsp;Watching, Next&nbsp;Up and Recently&nbsp;Added.</div>`;
  const offline=d.offline||[];
  const offlineRows=offline.map(ow=>{
    const title=ow.item_id
      ? `<a href="#/item/${ow.item_id}">${esc(ow.title)}</a>`
      : esc(ow.title);
    const phase=ow.kind==="send"
      ? "Sending to device"
      : (ow.state==="queued" ? "Waiting for encoder" : (ow.phase||"preparing").replace(/_/g," "));
    const progress=ow.kind==="send"
      ? `${fmtBytes(ow.bytes_sent)||"Starting"}${ow.bytes_total?` / ${fmtBytes(ow.bytes_total)}`:""}`
      : (ow.percent!=null ? `${ow.percent}%` : "—");
    return `<tr>
      <td><b>${title}</b></td><td>${esc(ow.user)}</td>
      <td>${esc(phase)} <span class="muted">· ${ow.target_height}p</span></td>
      <td>${progress}</td><td class="muted">${fmtAgo(ow.started_unix)}</td>
      <td style="text-align:right">${ME&&ME.is_admin?`<button class="ghost sm" onclick="stopOfflinePackage(${esc(JSON.stringify(ow.id))})">Stop</button>`:''}</td></tr>`;
  }).join("");
  m.innerHTML=`<div class="dvr-page-head"><div><h1>Activity</h1><p class="sub">What’s playing, recording and happening on your server.</p></div></div>
    <h2 class="section">Happening now · Watching</h2>
    ${missing.length?`<div class="clrefusal" role="status"><b>Activity is incomplete</b> ${esc(missing.map(node=>activityNodeFailureText(node,nodeNames)).join("; "))}. Viewers on these nodes may be missing.</div>`:""}
    ${activityWatchingHtml(dels,d.live_tv,nodeNames,sessionsById,open)}
    ${dvrActivityRows(recording,nodeNames,dvrState)}
    ${d.analysis?`<h2 class="section">Content analysis</h2>${analysisSummaryCard(d.analysis,"activity")}${analysisLiveProgress(d.analysis,nodeNames)}`:""}
    ${p?`<h2 class="section">Preparing media</h2>${produce}`:""}
    ${offline.length
      ? `<h2 class="section">Offline downloads</h2>`:""}
    ${offline.length
      ? `<table><thead><tr><th>Title</th><th>Profile</th><th>Work</th><th>Progress</th><th>Requested</th><th></th></tr></thead><tbody>${offlineRows}</tbody></table>`
      : ""}
    ${d.scans.length? `<h2 class="section">Library scans</h2><table><tbody>${scans}</tbody></table>` : ""}
    <div class="activity-idle">${[!d.scans.length?"No scans running":"",!p?"No media preparation running":"",!offline.length?"No downloads in progress":""].filter(Boolean).join(" · ")}</div>
    <h2 class="section">Trakt</h2>
    <div class="card">${trakt}</div>`;
  dvrRestoreUi(m,dvrUi);
  if(activityFocus)[...m.querySelectorAll("[data-activity-view]")].find(el=>el.dataset.activityView===activityFocus)?.focus({preventScroll:true});
  if(focusKey!=null){
    const again=[...m.querySelectorAll("details.stream-diag")].find(el=>el.dataset.stream===focusKey);
    const summary=again&&again.querySelector("summary");
    if(summary&&summary.focus) summary.focus();
  }
}
function liveTvActivityRows(rows,nodeNames){
  if(!Array.isArray(rows)||rows.length===0) return "";
  return `<h2 class="section">Live television</h2>
    <div class="tbl"><table><thead><tr><th>Channel</th><th>On now</th><th>Viewer</th>
      <th>Owner</th><th>Encoder</th><th>Age</th></tr></thead><tbody>
      ${rows.map(row=>`<tr>
        <td><b>${esc(row.channel_number)}</b> · ${esc(row.channel_name)}</td>
        <td>${row.programme_title?esc(row.programme_title):'<span class="muted">—</span>'}</td>
        <td>${esc(row.user)}</td>
        <td>${esc(nodeNames[row.owner_node_id]||row.owner_node_id)}</td>
        <td>${esc(row.encoder)} · ${esc(row.output_height)}p</td>
        <td>${esc(Math.round(row.age_seconds/60))} min · ${esc(row.state)}</td>
      </tr>`).join("")}
    </tbody></table></div>`;
}
// Anything holding a tuner and writing to a disk is attributable from inside
// the product, and stoppable from it. The Stop here is `DELETE /dvr/recordings`
// and nothing else: `stopSession` and `DELETE /activity/sessions/{id}` are the
// transcode and VOD verb, a Live TV viewer row has no Stop at all, and a
// capture is neither of those things.
function dvrRowPresentation(row){
  const observation=row.observation;
  const clientAge=DVR_SHARED.receivedAt?Math.max(0,performance.now()-DVR_SHARED.receivedAt):0;
  const fresh=!DVR_SHARED.error&&dvrOverviewFresh(DVR_SHARED.overview,DVR_SHARED.receivedAt)
    &&(!observation||Number(observation.observation_age_ms||0)+clientAge<=20000);
  const label=row.stop_requested_at_ms?"Stop requested":!fresh?"Status unavailable":row.display_state||"Status unavailable";
  const tone=label==="Recording"?"recording":["Reconnecting","Starting","Waiting to start","No tuner"].includes(label)?"warning":"";
  const bytes=row.total_bytes_written!=null?`${fmtBytes(row.total_bytes_written)||"0 B"} written`
    :row.last_confirmed_bytes!=null?`${fmtBytes(row.last_confirmed_bytes)||"0 B"} written this attempt`:"No confirmed bytes";
  return {label,tone,fresh,bytes,detail:!fresh?"Waiting for a fresh update from the recorder.":row.display_detail||""};
}
function dvrStatusMarkup(label,tone=""){
  return `<span class="dvr-status ${tone}">${esc(label)}</span>`;
}
function dvrProgressMarkup(row){
  const now=liveTvNowSeconds(),duration=Math.max(1,row.capture_end-row.capture_start);
  const elapsed=Math.max(0,Math.min(duration,now-row.capture_start));
  const percent=Math.round(elapsed/duration*100);
  return `<div class="dvr-progress"><div class="dvr-progress-label"><span>Capture window · ${percent}% elapsed</span><span>${now<row.capture_start?`Starts ${esc(liveTvClock(row.capture_start))}`:`Ends ${esc(liveTvClock(row.capture_end))}`}</span></div><div class="dvr-progress-track" role="meter" aria-label="Capture window elapsed; not saved video duration" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${percent}"><i style="width:${percent}%"></i></div></div>`;
}
function dvrCaptureCard(row){
  const state=dvrRowPresentation(row),id=row.recording_id||row.id;
  const selected=DVR_PAGE.selectedId===id;
  return `<article class="dvr-card${selected?" selected":""}" data-recording-id="${esc(id)}" data-dvr-recording="${esc(id)}"><div class="dvr-card-top"><span class="dvr-channel" aria-hidden="true">${esc(row.guide_number||"TV")}</span><div class="dvr-card-title"><button class="dvr-row-button" aria-label="Details for ${esc(row.title)}" aria-pressed="${selected}" onclick="selectDvrDetail(${esc(JSON.stringify(id))})"><h3>${esc(row.title)}</h3></button><p>${esc(row.channel_name||"Live TV")}${row.episode_title?` · ${esc(row.episode_title)}`:""}<br>${esc(liveTvClock(row.airing_start))}–${esc(liveTvClock(row.airing_end))}</p></div>${dvrStatusMarkup(state.label,state.tone)}</div>${dvrProgressMarkup(row)}<div class="dvr-card-foot"><span class="muted">${esc(state.bytes)}<br>${esc(state.detail)}</span><div class="row"><button class="ghost sm" data-dvr-focus="open-${esc(id)}" onclick="selectDvrDetail(${esc(JSON.stringify(id))})">Details</button>${row.can_stop&&!row.stop_requested_at_ms?`<button class="ghost sm" data-dvr-focus="stop-${esc(id)}" onclick="liveTvStopRecording(${esc(JSON.stringify(id))},${esc(JSON.stringify(row.title))})">Stop…</button>`:""}</div></div></article>`;
}
function dvrRestoreUi(main,saved){
  for(const key of saved.open){const el=main.querySelector(`[data-dvr-disclosure="${CSS.escape(key)}"]`);if(el)el.open=true;}
  const target=saved.key&&main.querySelector(`[data-dvr-focus="${CSS.escape(saved.key)}"]`)
    ||saved.row&&main.querySelector(`[data-recording-id="${CSS.escape(saved.row)}"] .dvr-row-button`);
  if(target)target.focus({preventScroll:true});
}
function dvrRememberUi(main){
  const active=document.activeElement,card=active&&active.closest&&active.closest("[data-recording-id]");
  return {open:[...main.querySelectorAll("[data-dvr-disclosure][open]")].map(el=>el.dataset.dvrDisclosure),
    key:active&&active.dataset&&active.dataset.dvrFocus,row:card&&card.dataset.recordingId};
}
function paintDvrHost(){
  if(location.hash==="#/activity"&&ACTIVITY_SNAPSHOT)paintActivityBody(ACTIVITY_SNAPSHOT,ACTIVITY_DVR.rows,ACTIVITY_DVR);
  else paintRecordingsPage();
}
function dvrDetailRoute(){return location.hash==="#/activity"||location.hash.startsWith("#/recordings");}
function openDvrActivity(id){
  if(location.hash==="#/activity")selectDvrDetail(id);
  else{DVR_PAGE.pendingId=id;location.hash="#/activity";}
}
async function loadDvrRecent(generation){
  if(ACTIVITY_DVR.recentBusy||Date.now()-(ACTIVITY_DVR.recentAt||0)<30000)return;
  ACTIVITY_DVR.recentBusy=true;const scope=TOKEN;
  try{
    const [saved,attention]=await Promise.all([api("/dvr/recordings?state=done,partial,failed,missed&limit=4"),api("/dvr/attention?limit=3")]);
    if(scope!==TOKEN)return;
    ACTIVITY_DVR.recent=dvrRecordingsPage(saved).rows;ACTIVITY_DVR.attention=attention.rows||[];
    ACTIVITY_DVR.recentError=null;ACTIVITY_DVR.recentAt=Date.now();
  }catch(e){if(scope===TOKEN){ACTIVITY_DVR.recentError=e.message;ACTIVITY_DVR.recentAt=Date.now();}}
  finally{ACTIVITY_DVR.recentBusy=false;if(scope===TOKEN&&generation===PAGE_RENDER_GENERATION&&location.hash==="#/activity")paintDvrHost();}
}
function dvrRecentMarkup(){
  const attention=ACTIVITY_DVR.attention||[],recent=ACTIVITY_DVR.recent||[];
  const item=(row,warning)=>`<div class="lt-rec" data-recording-id="${esc(row.id)}"><button class="dvr-row-button" onclick="selectDvrDetail(${esc(JSON.stringify(row.id))})"><b>${esc(row.title)}</b><span class="meta">${esc(row.channel_name||row.guide_number||"Live TV")} · ${esc(liveTvClock(row.airing_start))}</span>${warning&&row.state_reason?`<span class="why">${esc(row.state_reason)}</span>`:""}</button>${dvrStatusMarkup(row.state==="recording"?((DVR_SHARED.overview&&DVR_SHARED.overview.active||[]).find(active=>active.recording_id===row.id)?dvrRowPresentation(DVR_SHARED.overview.active.find(active=>active.recording_id===row.id)).label:"Status unavailable"):dvrSavedLabel(row),warning?"warning":row.state==="done"?"saved":"warning")}</div>`;
  return `${ACTIVITY_DVR.recentError?`<p class="hint" role="status">Recent recording activity could not be refreshed. ${esc(ACTIVITY_DVR.recentError)}</p>`:""}${attention.length?`<div class="dvr-section-head"><h2>Needs attention</h2><a href="#/recordings/attention">Review all</a></div><div class="lt-recs" style="height:auto">${attention.map(raw=>item(raw.recording,true)).join("")}</div>`:""}<div class="dvr-section-head"><h2>Recent recordings</h2><a href="#/recordings/saved">All recordings</a></div><div class="lt-recs" style="height:auto">${recent.map(row=>item(row,false)).join("")||'<p class="muted" style="padding:16px">No recent recordings to show.</p>'}</div>`;
}
function dvrActivityRows(rows,nodeNames,state={loaded:true,error:null,next:null}){
  if(!Array.isArray(rows))rows=[];
  const unavailable=state.error||DVR_SHARED.error||!dvrOverviewFresh(DVR_SHARED.overview,DVR_SHARED.receivedAt);
  const notice=unavailable?`<div class="clrefusal" role="status"><b>Recording status is unavailable.</b> ${esc(state.error||DVR_SHARED.error||"Waiting for a fresh update from the recorder.")}</div>`:"";
  return `<section aria-label="DVR recording activity"><div class="dvr-section-head"><h2>Recording activity</h2><a href="#/recordings/upcoming">Manage recordings →</a></div><div class="dvr-layout"><section>${notice}${rows.map(dvrCaptureCard).join("")||`<div class="empty">${unavailable?"No active recordings can be confirmed.":"Nothing recording right now. Your next recordings are in Upcoming."}</div>`}${DVR_SHARED.overview&&DVR_SHARED.overview.active_truncated?'<a href="#/recordings/upcoming">More active captures — view all</a>':""}${state.next?'<button class="ghost sm" onclick="loadMoreActivityDvr()">Load more</button>':""}${dvrRecentMarkup()}</section>${dvrDetailMarkup()}</div></section>`;
}

async function loadMoreActivityDvr(){
  if(!ACTIVITY_DVR.next||ACTIVITY_DVR.loading) return;
  const generation=PAGE_RENDER_GENERATION, cursor=ACTIVITY_DVR.next;
  ACTIVITY_DVR.loading=true;
  try{
    const page=dvrRecordingsPage(await api(`/dvr/recordings?state=recording&after=${encodeURIComponent(cursor)}`));
    if(generation!==PAGE_RENDER_GENERATION||location.hash!=="#/activity") return;
    const seen=new Set(ACTIVITY_DVR.rows.map(row=>row.id));
    ACTIVITY_DVR.rows=ACTIVITY_DVR.rows.concat(page.rows.filter(row=>!seen.has(row.id)));
    ACTIVITY_DVR.next=page.next; ACTIVITY_DVR.error=null; ACTIVITY_DVR.loaded=true;
  }catch(e){
    ACTIVITY_DVR.error=e&&e.message?e.message:"The next recording page could not be loaded.";
  }finally{
    ACTIVITY_DVR.loading=false;
    if(generation===PAGE_RENDER_GENERATION&&location.hash==="#/activity"&&ACTIVITY_SNAPSHOT)
      paintActivityBody(ACTIVITY_SNAPSHOT,ACTIVITY_DVR.rows,ACTIVITY_DVR);
  }
}
// 202, not 204: the capture stops when the owner's next tick closes the file,
// and only the owner may write the terminal state. So the button asks, and
// then watches the row until it is no longer `recording` — which is also the
// moment the tuner it was holding is free for somebody to watch.
function stopDvrRecording(id){
  const row=ACTIVITY_DVR.rows.find(row=>(row.recording_id||row.id)===id);
  return liveTvStopRecording(id,row&&row.title);
}

async function stopSession(id){
  try{ await api(`/activity/sessions/${id}`,{method:"DELETE"}); toast("Stream stopped"); renderActivityBody(); }
  catch(e){ toast(e.message); }
}
async function stopProducer(){
  try{ const r=await api("/activity/producer",{method:"DELETE"}); toast(r.note||"Stopping"); renderActivityBody(); }
  catch(e){ toast(e.message); }
}
async function stopOfflinePackage(id){
  try{ await api(`/activity/offline/${id}`,{method:"DELETE"}); toast("Offline download stopped"); renderActivityBody(); }
  catch(e){ toast(e.message); }
}

