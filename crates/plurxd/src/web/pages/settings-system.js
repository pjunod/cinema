"use strict";
function usersPanel(users,settings){
  const admins=users.filter(u=>u.is_admin).length;
  const summary=`${users.length} account${users.length===1?"":"s"} · ${admins} admin${admins===1?"":"s"}`;
  return `${setHead("Users",summary,`<button class="sm" onclick="openUserDrawer('new')">+ Add user</button>`)}
      ${USER_DRAWER==="new"?userDrawerHtml(null):""}
      <div class="card" style="padding:8px 10px 4px"><table><thead><tr><th>User</th><th>Role</th><th>Created</th><th></th></tr></thead>
      <tbody>${users.map(u=>userRow(u)).join("")}</tbody></table></div>
      ${signInExpiryCard(settings)}`;
}
// Which user row has its drawer open ("new" for the add form, a user id for
// a password reset, or null). Same rules as the library drawer.
let USER_DRAWER=null;
function openUserDrawer(id){
  USER_DRAWER=(USER_DRAWER===id)?null:id;
  renderSettings();
  const first=/** @type {HTMLInputElement} */(document.querySelector(".setdrawer input")); if(first) first.focus();
}
// The add form and the password reset are the same drawer: a password typed
// twice. Reset used to be two prompt() dialogs; a form with a confirm field
// is what the other two password forms already were.
function userDrawerHtml(u){
  const id=u?u.id:"new";
  const fields=`<div class="setfields">
      ${u?"":`<div><label for="un">Username</label><input id="un" placeholder="Username" autocomplete="off" style="min-width:180px"></div>`}
      <div><label for="up-${id}">${u?"New password":"Password"} <span class="muted">(min 8)</span></label><input id="up-${id}" type="password" autocomplete="new-password" style="min-width:200px"></div>
      <div><label for="up2-${id}">Confirm password</label><input id="up2-${id}" type="password" autocomplete="new-password" style="min-width:200px"></div>
      ${u?"":`<div><label>&nbsp;</label><label class="chk" for="ua"><input type="checkbox" id="ua"> Administrator</label></div>`}
      <div class="err" id="uerr"></div>
    </div>`;
  const body=u
    ? `${fields}<div class="hint">Their current sessions are signed out when the password changes.</div>
       <div class="setfoot"><button class="sm" onclick="resetPw(${u.id},${esc(JSON.stringify(u.username))},this)">Set password</button><button class="ghost sm" onclick="openUserDrawer(null)">Cancel</button></div>`
    : `${fields}<div class="setfoot"><button class="sm" onclick="addUser(this)">Add user</button><button class="ghost sm" onclick="openUserDrawer(null)">Cancel</button></div>`;
  return u
    ? `<tr class="setdrawer" id="userdrawer-${id}"><td colspan="4">${body}</td></tr>`
    : `<div class="card setdrawer" id="userdrawer-new" style="margin-bottom:14px"><h2 class="section" style="margin:0 0 6px;padding:0;border:0">Add user</h2>${body}</div>`;
}
function buildTag(sys){
  const b=sys.build;
  if(!b || b==="unknown")
    return ` <span class="muted" style="font-size:12px" title="Built from a context with no .git and no PLURX_BUILD_REF, so the image cannot name its commit — its source date below (the commit time when the build was given one, otherwise the compile time) is the next best thing. Deploy with 'make docker-up', which stamps it.">(unstamped · dated ${esc(sourceDateLabel(sys.built_at))})</span>`;
  if(b==="v"+sys.version || b===sys.version) return "";
  return ` <span class="muted" style="font-size:12px">(${esc(b)})</span>`;
}
// The build's source date (`built_at`: SOURCE_DATE_EPOCH, else the commit
// time, else the compile time; crates/plurxd/build_support/source_date.rs),
// shortened for a UI: the date distinguishes one deploy from another, the
// seconds never do.
function sourceDateLabel(s){
  if(!s) return "unknown";
  const m=/^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(s);
  return m? `${m[3]} ${["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"][+m[2]-1]} ${m[4]}:${m[5]}Z` : s;
}
// One string that answers "which build am I looking at". Prefers the commit,
// falls back to the build's source date -- because "unknown" cannot answer the
// only question anybody asks this for, and a date can. For an exact-tag
// release that date is the tagged commit's, not when the binary was compiled.
function buildLabel(){
  const s=SERVER||{};
  const v=s.version||"?";
  const b=s.build;
  if(b && b!=="unknown" && b!=="v"+v && b!==v) return `${v} · ${b}`;
  return `${v} · dated ${sourceDateLabel(s.built_at)}`;
}
// What the boot probe found out about tone-mapping HDR on this machine.
//
// Worth a line of its own because falling back to the CPU chain is *silent*:
// everything still plays, 4K just stays slow, and there is nothing on screen to
// distinguish "this box has no GPU tone-map" from "the driver refused the
// graph" — which are the difference between shrugging and installing a package.
// Rejected candidates remain available in a collapsed disclosure, so a working
// GPU selection is not buried under diagnostics from the other probes.
//
// The Metal graph is proved here like any other, but sessions reach it only
// through Mac video processing, so node selection stays on the CPU chain when
// it passes. The HDR line follows that context: green Metal when it is on and
// has observed the graph; otherwise the node's own selection, with a note that
// Metal matched the CPU picture and what it is waiting on.
function toneMapHtml(tm, mac){
  if(!tm) return `<span class="muted">—</span>`;
  const verdicts=tm.verdicts||[];
  const metal=verdicts.find(v=>v.pipeline==="vt_tonemap_metal"&&v.passed);
  const macMetal=mac&&mac.hdr10_metal||{};
  const macReady=!!(metal&&mac&&mac.enabled&&macMetal.availability==="available");
  const gpu=macReady||!!(tm.selected&&tm.selected!=="cpu");
  // Everything the probe rejected. The CPU chain is the reference the others
  // are measured against rather than a candidate, so it never appears as one —
  // "CPU tone-map: 1.0× — not selected" is a line that tells nobody anything.
  // A candidate this FFmpeg build cannot express is a platform fact, listed
  // after the rejections and never counted as a reason for falling back.
  const rejected=verdicts.filter(v=>!v.passed && !v.unavailable && v.pipeline!==tm.selected);
  const unavailable=verdicts.filter(v=>v.unavailable);
  const head = gpu
    ? `<span style="color:var(--good)">${esc(macReady?metal.label:tm.selected_label)}</span>`
    : esc(tm.selected_label||"CPU tone-map");
  if(!tm.ran){
    // Not probed is not the same as everything failing, and the reason says
    // which — usually "software encoder", where a GPU graph would have to
    // download every frame anyway and there is nothing to gain.
    const why=(tm.verdicts||[])[0];
    return `${head} <span class="muted">(${esc(why&&why.rejected||"not probed")})</span>`;
  }
  // A CPU selection after a real probe is a *fallback*, and saying so is the
  // whole point of this line: everything still plays, 4K just stays slow, and
  // without this there is nothing on screen to distinguish a box with no GPU
  // tone-map from one whose driver refused the graph.
  let note="";
  if(metal&&!macReady){
    const speed=Number.isFinite(metal.speedup)?` at ${metal.speedup.toFixed(2)}× the CPU chain`:"";
    const waiting=!(mac&&mac.enabled)
      ? "HDR uses it once Mac video processing is on in Settings → Developer"
      : macMetal.availability==="unavailable"
        ? `Mac video processing reports it unavailable (${esc(macMetal.reason||"unknown")})`
        : "Mac video processing is still checking it";
    note=` <span class="muted">— ${esc(metal.label)} matched the CPU picture${speed}; ${waiting}</span>`;
  } else if(!gpu && rejected.length){
    note=` <span class="muted">— fell back</span>`;
  }
  const rows=rejected.concat(unavailable).map(v=>
    `<div class="muted" style="font-size:12px">${esc(v.label)}: ${esc(v.rejected||"rejected")}</div>`
  ).join("");
  const details=rows
    ? `<details class="muted" style="margin-top:4px;font-size:12px"><summary style="cursor:pointer">Probe details</summary><div style="margin-top:6px;overflow-wrap:anywhere">${rows}</div></details>`
    : "";
  return `${head}${note}${details}${tm.census?censusMount(tm.census):""}`;
}
function censusMount(census){
  return census?`<div id="tone-map-census" style="margin-top:8px">${toneMapCensusHtml(census)}</div>`:"";
}
// The tone-map census (docs/streaming/TONE-MAP-CENSUS.md): every GPU graph
// this node could pair with its encoder, measured against the CPU chain on
// authored PQ signals, with a still of each. GPU work on a production node, so
// it runs only from this button, waits behind and yields to any viewer, says
// which graph it is on, and has a Stop.
//
// Distances are 8-bit levels of 16x9 block means, Y/U/V, and they are
// distance from the CPU chain — a baseline, not a reference picture.
function toneMapCensusHtml(c){
  const active=c.state==="queued"||c.state==="running";
  const ago=c.since?fmtAgo(Math.floor(c.since/1000)):"";
  const words={
    idle:`<span class="muted">Not run on this node.</span>`,
    queued:`Queued <span class="muted">— ${esc(c.step||"waiting for a free hardware slot")}</span>`,
    running:`Running <span class="muted">— ${esc(c.step||"")}</span>`,
    finished:`Finished <span class="muted">${esc(ago)}</span>`,
    yielded:`<span style="color:var(--warn,#fbbf24)">Yielded to playback</span> <span class="muted">${esc(ago)} — run it again when the node is idle</span>`,
    stopped:`Stopped <span class="muted">${esc(ago)}</span>`,
    failed:`<span style="color:var(--bad)">Failed</span> <span class="muted">— ${esc(c.reason||"")}</span>`,
  };
  const control=active
    ? `<button class="ghost sm" onclick="stopToneMapCensus(this)">Stop</button>`
    : `<button class="ghost sm" onclick="startToneMapCensus(this)">Run census</button>`;
  const head=`<div class="row" style="gap:10px;align-items:center"><span>Census: ${words[c.state]||esc(c.state)}</span>${control}</div>`;
  const hint=`<div class="muted" style="font-size:12px">Uses the GPU for a minute or two. Waits for and gives way to any viewer.</div>`;
  return head+hint+(c.result?toneMapCensusResultHtml(c.result):"");
}
function toneMapCensusResultHtml(r){
  const lv=n=>n==null?"—":Number(n).toFixed(1);
  const trio=a=>a?a.map(lv).join(" / "):"—";
  const still=name=>name?`<button class="ghost sm" onclick="showCensusStill(this,${esc(JSON.stringify(name))})">still</button>`:"";
  const rows=[];
  (r.baseline||[]).forEach(b=>rows.push(`<tr><td>CPU chain <span class="muted">(baseline)</span></td><td>${esc(b.signal)}</td><td>noise ${lv(b.noise_floor)}</td><td></td><td>${lv(b.brightest_block_luma)}</td><td>${still(b.still)}</td></tr>`));
  (r.candidates||[]).forEach(c=>(c.signals||[]).forEach(s=>rows.push(`<tr><td>${esc(c.label)}${c.selected?` <span class="muted">(selected)</span>`:""}</td><td>${esc(s.signal)}</td>${
    s.distance
      ? `<td>${trio(s.distance.max)}</td><td>${trio(s.distance.p95)}</td><td>${lv(s.brightest_block_luma)}</td><td>${still(s.still)}</td>`
      : `<td colspan="4" class="muted" style="overflow-wrap:anywhere">${s.started?"ran, not measurable":"did not start"}: ${esc(s.reason||"")}</td>`
  }</tr>`)));
  const floor=r.noise_floor_zero?"The CPU chain reproduced itself exactly (noise floor 0).":"The CPU chain differed from itself between runs; read distances against its noise figure.";
  return `<details class="muted" style="margin-top:4px;font-size:12px"><summary style="cursor:pointer">Census results <span class="muted">(${esc(r.key&&r.key.encoder_family||"")} · ${esc(r.key&&r.key.gpu_device||"")})</span></summary>
    <div style="margin-top:6px;overflow-x:auto"><table><thead><tr><th>Graph</th><th>Signal</th><th>Max Δ Y/U/V</th><th>p95 Δ Y/U/V</th><th>Brightest Y</th><th></th></tr></thead><tbody>${rows.join("")}</tbody></table>
    <div>${floor} Distances are from the CPU chain in 8-bit levels of 16×9 block means.</div><div id="tone-map-census-still"></div></div></details>`;
}
async function startToneMapCensus(btn){
  btn.disabled=true;
  try{ paintToneMapCensus(await api("/system/tone-map/census",{method:"POST"})); }
  catch(e){ toast(e.message); btn.disabled=false; }
}
async function stopToneMapCensus(btn){
  btn.disabled=true;
  try{ paintToneMapCensus(await api("/system/tone-map/census/stop",{method:"POST"})); }
  catch(e){ toast(e.message); btn.disabled=false; }
}
// The row describes the node Settings loaded. A poll or a Stop that a
// balanced ingress answered from another node is not this node's census.
function censusFromThisNode(census){
  const sys=SETTINGS_DATA&&SETTINGS_DATA.sys;
  return !!(census&&sys&&(!census.node_id||census.node_id===sys.node_id));
}
function paintToneMapCensus(census){
  if(!censusFromThisNode(census)){
    const mount=document.getElementById("tone-map-census");
    if(mount&&!mount.querySelector(".census-elsewhere")) mount.insertAdjacentHTML("beforeend",`<div class="muted census-elsewhere" style="font-size:12px">Another node answered; reload Settings to see this node's census.</div>`);
    return;
  }
  if(SETTINGS_DATA&&SETTINGS_DATA.sys&&SETTINGS_DATA.sys.tone_map) SETTINGS_DATA.sys.tone_map.census=census;
  const mount=document.getElementById("tone-map-census");
  if(mount) mount.innerHTML=toneMapCensusHtml(census);
}
function toneMapCensusActive(){
  const c=SETTINGS_DATA&&SETTINGS_DATA.sys&&SETTINGS_DATA.sys.tone_map&&SETTINGS_DATA.sys.tone_map.census;
  return !!c&&(c.state==="queued"||c.state==="running");
}
// A still is an authenticated route, so it is fetched with the session's
// credential and shown from a blob rather than linked.
async function showCensusStill(btn,name){
  const mount=document.getElementById("tone-map-census-still");
  if(!mount) return;
  try{
    const res=await api(`/system/tone-map/census/stills/${encodeURIComponent(name)}`,{raw:true});
    if(!res.ok) throw new Error(`still unavailable (${res.status})`);
    const url=URL.createObjectURL(await res.blob());
    // One still is shown at a time; release the one it replaces.
    if(mount.dataset.stillUrl) URL.revokeObjectURL(mount.dataset.stillUrl);
    mount.dataset.stillUrl=url;
    mount.innerHTML=`<div style="margin-top:6px">${esc(name)}</div><img alt="${esc(name)}" style="max-width:100%;margin-top:4px" src="${url}">`;
  }catch(e){ toast(e.message); }
}
// What each library's storage reads at, in the unit the rest of the page uses
// for bitrates — so "this mount does 240 Mb/s" and "this file is 69 Mb/s" can
// be compared by looking at them.
//
// The input side of the pipeline, and the last part of it with no number. A
// source the server cannot read fast enough never says so: it surfaces as a
// client stall, which reads as a network or encoder problem and sends people
// to tune the two halves that were working.
function storageHtml(st){
  if(!st || !st.ran) return `<span class="muted">measuring…</span> <button class="ghost sm" onclick="remeasureStorage(this)">Measure now</button>`;
  // One line per mount, in columns. The earlier version put "feeds up to 2462
  // Mb/s at realtime" under every row, which restated the number immediately
  // above it in different words and doubled the height of the block for no
  // information. The read rate IS the ceiling a source bitrate is compared
  // against; saying so once below the list is enough.
  const rows=(st.mounts||[]).map(m=>{
    const roots=esc((m.roots||[]).join(", "));
    if(m.read_bps==null){
      return `<span class="stgpath">${roots}</span><span class="stgrate" style="color:var(--bad)">${esc(m.note||"not measured")}</span>`;
    }
    // MB/s alongside Mb/s because the two get confused constantly and every
    // other tool an operator reaches for (dd, iostat) reports bytes.
    const rate=`${(m.read_bps/1e6).toFixed(0)} Mb/s <span class="muted">(${(m.read_bps/8e6).toFixed(0)} MB/s)</span>`;
    const seek=m.seek_ms!=null?` <span class="muted">· seek ${m.seek_ms.toFixed(1)} ms</span>`:"";
    const warn=m.cache_suspect?`<span class="stgpath"></span><span class="muted" style="color:var(--warn,#fbbf24)">${esc(m.note||"")}</span>`:"";
    return `<span class="stgpath">${roots}</span><span class="stgrate">${rate}${seek}</span>${warn}`;
  }).join("");
  const hint=rows?`<div class="muted" style="font-size:12px;margin-top:4px">A remux or direct play needs its own bitrate sustained from here.</div>`:"";
  const when=st.measured_at?`<span class="muted" style="font-size:12px">measured ${fmtAgo(st.measured_at)}</span>`:"";
  return `${rows?`<div class="stgtable storage-table">${rows}</div>`:`<span class="muted">no libraries to measure</span>`}${hint}
    <div class="row" style="margin-top:8px;gap:10px;align-items:center">
      <button class="ghost sm" onclick="remeasureStorage(this)">Re-measure</button>${when}</div>`;
}
// Synchronous by design: the button asked for a measurement, and it may take
// as long as a sleeping array takes to answer. Re-rendering immediately with
// the previous numbers would look like it worked and be wrong.
async function remeasureStorage(btn){
  const was=btn.textContent, node_id=SETTINGS_DATA?.sys?.node_id; btn.disabled=true; btn.textContent="Measuring…";
  try{
    const st=await api("/system/storage",{method:"POST"});
    if(node_id&&SETTINGS_DATA?.sys?.node_id===node_id){
      SETTINGS_DATA.sys.storage=st;
      replaceSystemCard("system-storage-card",systemStorageCard(SETTINGS_DATA.sys));
    }
  }catch(e){ toast(e.message); btn.disabled=false; btn.textContent=was; }
}
// Watch-state durability in operator language. SQLite is deliberately not
// labelled "synced": with no peer, there is nowhere for a pause to carry over
// to. This returns text only so adding the status does not invent a second
// Settings card or membership surface; M3 owns those controls.
function replicationText(r){
  if(!r) return "status unavailable";
  const sqlite=r.backend==="sqlite";
  const degraded=r.health==="degraded";
  const label=sqlite?"SQLite single-node":degraded?"Replicated DEGRADED":r.clustered?"Replicated in sync":"Replicated one node";
  const point=r.last_applied_index==null?"":` · applied term ${r.last_applied_term??"?"}, index ${r.last_applied_index}`;
  const lag=r.behind_by?` · ${r.behind_by} change${r.behind_by===1?'':'s'} behind`:"";
  const converged=r.last_converged_at?` · last observed in sync ${fmtAgo(r.last_converged_at)}`:"";
  return esc(`${label}${point}${lag} — ${r.explanation||""}${converged}`);
}
function playbackSummaryCard(events){
  const rows=Array.isArray(events)?events:[];
  const starts={};
  rows.filter(e=>e.event==="ttff"&&Number.isFinite(e.ms)).forEach(e=>
    (starts[e.method||"unknown"]||(starts[e.method||"unknown"]=[])).push(Number(e.ms)));
  const pct=(a,p)=>{ const s=a.slice().sort((x,y)=>x-y); return s.length?s[Math.min(s.length-1,Math.floor((s.length-1)*p))]:null; };
  const ttff=Object.entries(starts).sort().map(([method,values])=>
    `<span class="stgpath">${esc(method)}</span><span>p50 ${pct(values,.5)} ms · p95 ${pct(values,.95)} ms <span class="muted">(${values.length})</span></span>`).join("");
  const stalls={supply:0,decode:0,other:0};
  rows.filter(e=>e.event==="stall").forEach(e=>{ const d=e.detail||""; stalls[d.includes("supply")?"supply":d.includes("decode")?"decode":"other"]++; });
  const suspended=rows.filter(e=>e.event==="resume"&&Number.isFinite(e.ms)).reduce((n,e)=>n+e.ms,0)/1000;
  return `<div class="card"><h2 class="section" style="margin-top:0">Playback <span class="muted">(7 days)</span></h2>
    ${ttff?`<div class="stgtable">${ttff}</div>`:`<span class="muted">No time-to-first-frame rows yet.</span>`}
    <div class="hint">Stalls: ${stalls.supply} supply · ${stalls.decode} decode · ${stalls.other} other<br>
      Encoder suspended: ${suspended.toFixed(1)} seconds · ${rows.length} stored event${rows.length===1?'':'s'} shown</div></div>`;
}
// A segment request that arrives before its media exists waits rather than
// failing, under a per-viewer cap and a node-wide one. When either refuses, the
// player gets a 503 — and this row is the only place on the node that says how
// many are parked, what ceiling they are against, and which cap turned anything
// away.
//
// The two refusal classes are named apart on purpose, because they argue for
// opposite actions. Refusals against one viewer's cap are that cap doing its
// job on a client asking for too much at once; raising the node setting fixes
// nothing. Refusals against the node cap are the ones that say the ceiling is
// sized for a smaller machine, so this is the one that points at the setting,
// and it says so.
function blockedGetsHtml(b){
  if(!b) return `<span class="muted">not reported by this server</span>`;
  const waiting=Number(b.waiting)||0, cap=Number(b.cap)||0;
  const viewer=Number(b.refused_session_busy)||0, node=Number(b.refused_pool_full)||0;
  const head=`${waiting} of ${cap} waiting`;
  if(!viewer&&!node) return `${head} <span class="muted">· none refused since start-up</span>`;
  const parts=[];
  if(viewer) parts.push(`${viewer} refused for a single player's limit`);
  if(node) parts.push(`<span style="color:var(--bad)">${node} refused because this server was full</span>`);
  const advice=node?`<div class="muted" style="margin-top:2px">Raising “Waiting segment requests” under Streaming is what changes the second number. The first one it will not change.</div>`:"";
  return `${head} <span class="muted">· since start-up:</span> ${parts.join(" · ")}${advice}`;
}
function ffmpegSupportWarning(sys){
  const support=sys.ffmpeg_support;
  if(!support)return "";
  const missing=(support.rows||[]).filter(row=>row.required&&!row.included), failed=(support.rows||[]).filter(row=>row.required&&row.included&&["failed","unavailable"].includes(row.runtime)), problems=support.package_problems||[];
  if(!missing.length&&!problems.length&&!failed.length)return "";
  const title=missing.length?"FFmpeg lacks required support":problems.length?"FFmpeg package is not verified":"Required media hardware check failed";
  const message=missing.length?`Missing ${missing.map(row=>row.label).join(", ")}.`:problems.length?"Install the required Jellyfin FFmpeg package and restart Cinema.":`Execution failed for ${failed.map(row=>row.label).join(", ")}.`;
  return `<section class="system-notice" role="alert"><strong>${esc(title)}</strong><p>${esc(message)}</p><details><summary>Package and probe details</summary><div class="system-detail">${problems.length?`<p>${esc(problems.join(" · "))}</p>`:""}<p>Using <code>${esc(support.executable)}</code></p><p>${missing.length?"Install the complete Jellyfin FFmpeg package and restart Cinema.":failed.length?"Check the failed probe and GPU driver or device access, then restart Cinema.":""}</p></div></details></section>`;
}
function ffmpegSupportCard(sys){
  const support=sys.ffmpeg_support;
  if(!support)return "";
  const states={passed:"Passed",failed:"Failed",blocked:"Blocked",unavailable:"Unavailable",not_tested:"Not tested",not_applicable:"Not applicable"};
  const rows=support.rows||[], active=rows.filter(row=>row.applicable), others=rows.filter(row=>!row.applicable);
  const render=row=>`<tr><th scope="row">${esc(row.label)}${row.missing?.length&&row.applicable?`<small>${esc(row.missing.join(" · "))}</small>`:""}</th><td>${esc(row.required_for)}${row.required?"":" · optional"}</td><td class="${row.included?'media-support-good':row.required?'media-support-bad':''}">${row.included?"✓ Included":"✕ Missing"}</td><td class="${row.runtime==='passed'?'media-support-good':row.runtime==='failed'?'media-support-bad':''}">${esc(states[row.runtime]||"Not tested")}${row.runtime_detail?`<small>${esc(row.runtime_detail)}</small>`:""}</td></tr>`;
  return `<section class="card media-support" id="ffmpeg-support-card"><div class="media-support-heading"><h2 class="section">Media runtime</h2><span class="${support.package_verified?'media-support-good':'muted'}">${support.package_verified?"✓ Required build verified":"Package identity not verified"}</span></div>
    <dl class="kvgrid"><dt>Using</dt><dd><code>${esc(support.executable)}</code></dd>${support.expected_executable?`<dt>Expected</dt><dd><code>${esc(support.expected_executable)}</code></dd>`:""}</dl>
    <h3>Support for this node</h3><p class="muted">Requirements follow this node’s hardware. Build support and execution results are separate.</p>
    <div class="media-support-table"><table><thead><tr><th scope="col">Capability</th><th scope="col">Required for</th><th scope="col">In build</th><th scope="col">Execution probe</th></tr></thead><tbody>${active.map(render).join("")}</tbody></table></div>
    <p class="muted">Included means the required feature and options exist. Passed means its named test completed. Not tested is not a pass. Real-source playback and picture quality qualification remain separate.</p>
    ${others.length?`<details><summary>Other hardware · not required on this node</summary><ul>${others.map(row=>`<li>${esc(row.label)} — not required</li>`).join("")}</ul></details>`:""}
    <p class="muted">Checked ${esc(new Date(support.checked_at*1000).toLocaleString())}. Checks run at startup; reading this page does not launch new tests.</p></section>`;
}
// System presents configured capabilities. A playback session may choose a
// different path for its codec; live activity belongs on the Activity page.
function systemCapabilityHtml(fact,extra=""){
  const enabled=fact.enabled===true;
  const detail=String(fact.detail||"").split("\n").map(esc).join("<br>");
  return `<details class="system-capability" data-capability="${esc(fact.id)}"><summary><span>${esc(fact.label)}</span><span class="system-state ${enabled?'system-good':'muted'}">${enabled?"Enabled":"Disabled"}</span></summary><div class="system-detail">${detail}${extra}</div></details>`;
}
function systemVersionsCard(sys){
  const support=sys.ffmpeg_support||{}, version=support.executable_version||sys.ffmpeg_version||"Not found";
  const required=support.required_package, expected=support.expected_version;
  const verified=support.package_verified===true;
  const identity=verified?"✓ Required build verified":support.package_verification==="unsupported_platform"?"Package identity not verified on this platform":"Package identity not verified";
  const kv=(label,value)=>value?`<span>${esc(label)}</span><code>${esc(value)}</code>`:"";
  return `<section class="card system-card" id="system-versions"><h2>Versions</h2>
    <div class="system-version"><div class="system-versionline"><strong>Cinema</strong><span>${esc(sys.version||"Unknown")}</span><span class="system-state muted">Running</span></div>
      <details class="system-package"><summary>Build &amp; location</summary><div class="system-detail system-kv">${kv("Build",sys.build&&sys.build!=="unknown"?sys.build:`Unstamped · ${sourceDateLabel(sys.built_at)}`)}${kv("Executable",sys.application_executable)}${kv("Runtime directory",sys.runtime_location)}${kv("Data directory",sys.data_dir)}</div></details></div>
    <div class="system-version"><div class="system-versionline"><strong>Jellyfin FFmpeg</strong><span>${esc(version)}</span><span class="system-state ${verified?'system-good':'muted'}">${esc(identity)}</span></div>
      <details class="system-package"><summary>Required package &amp; location</summary><div class="system-detail"><div class="system-kv">${kv("Required package",required?`Jellyfin FFmpeg ${required}`:null)}${kv("Expected FFmpeg version",expected)}${kv("Required source revision",support.source_revision)}${kv("Executable",support.executable||sys.ffmpeg)}${support.expected_executable?kv("Expected executable",support.expected_executable):""}<span>Integrity checks</span><span>${verified?"Pinned source, Cinema patches and companion files verified":esc((support.package_problems||[]).join(" · ")||"Package identity has not been verified.")}</span></div><details class="system-probes"><summary>Build support &amp; execution checks</summary>${ffmpegSupportCard(sys)}</details></div></details></div>
    </section>`;
}
function systemGraphicsHtml(sys){
  const devices=sys.graphics_devices||[];
  if(!devices.length)return "";
  const vendors=Array.from(new Set(devices.map(device=>device.vendor).filter(Boolean))).join(" + ");
  const rows=devices.map(device=>`<span>${esc(device.vendor||"Graphics device")}</span><span>${esc(device.model||device.device_id||device.id)}${device.render_node?` · <code>${esc(device.render_node)}</code>`:""}</span>`).join("");
  const bindings=(sys.encoder_device_bindings||[]).map(binding=>`<p>${esc(binding.detail||binding.backend)}</p>`).join("");
  return `<details class="system-capability"><summary><span>Graphics devices</span><span class="system-state muted">${esc(vendors||`${devices.length} detected`)}</span></summary><div class="system-detail"><div class="system-kv">${rows}</div>${bindings}</div></details>`;
}
function transcoderCard(sys){
  if(!sys.node_id)return "";
  const enc=sys.encoders||{}, active=sys.hwaccel_pref||"auto", requested=sys.hwaccel_requested||active;
  const names={nvenc:"NVIDIA · NVENC",qsv:"Intel · Quick Sync",vaapi:"VA-API",videotoolbox:"Apple VideoToolbox",software:"CPU (software)"};
  const choices=[["auto",sys.encoder_selected?`Auto (${sys.encoder_selected})`:"Auto"]];
  for(const [value,label] of Object.entries(names))if(value==="software"||enc[value])choices.push([value,label]);
  const available=choices.some(([value])=>value===requested);
  const pending=sys.hwaccel_restart_required||requested!==active||sys.transcoder_optimization?.restart_required;
  const labels={hardware_decoding:"Hardware decoding",hardware_encoding:"Hardware encoding",tone_mapping:"HDR → SDR tone mapping",hardware_scaling:"Hardware scaling",subtitle_burn_in:"Subtitle burn-in",deinterlacing:"Deinterlacing"};
  const facts=sys.transcoding_facts||[];
  const rows=Object.entries(labels).map(([id,label])=>{
    const fact=facts.find(row=>row.id===id)||{id,label,enabled:false,detail:"Capability information has not been reported by this server."};
    const extra=id==="tone_mapping"?systemToneMapDetails(sys.tone_map):"";
    return systemCapabilityHtml(fact,extra);
  }).join("");
  return `<section class="card system-card" id="transcoder-card"><h2>Transcoding</h2>
    <div class="system-preference"><label for="node-hwaccel">Encoding preference<select id="node-hwaccel" data-node-id="${esc(sys.node_id)}" onchange="nodeHwaccelChanged()">${choices.map(([value,label])=>`<option value="${value}"${value===(available?requested:"auto")?" selected":""}>${esc(label)}</option>`).join("")}</select></label><button onclick="saveNodeHwaccel(this)">Save</button></div>
    <p class="system-pref-status muted" id="node-hwaccel-status" aria-live="polite">${!available?"The saved backend is unavailable. Choose and save an available option. ":""}${pending?"Saved choice is waiting for this node to restart.":"Changes take effect after restarting Cinema."}</p>
    ${systemGraphicsHtml(sys)}${rows}</section>`;
}
function systemToneMapDetails(tm){
  if(!tm)return "";
  const verdicts=(tm.verdicts||[]).filter(row=>row.pipeline!=="cpu");
  const probes=verdicts.length?`<details class="system-probes"><summary>Processing checks</summary><div class="system-detail">${verdicts.map(row=>`<p>${esc(row.label||row.pipeline)} · ${row.passed?"Passed":row.unavailable?"Unavailable":"Not passed"}${row.rejected?` · ${esc(row.rejected)}`:""}</p>`).join("")}</div></details>`:"";
  return `${probes}${tm.census?`<details class="system-probes"><summary>Picture comparison census</summary>${censusMount(tm.census)}</details>`:""}`;
}
function systemAdditionalSupportCard(sys){
  const facts=sys.additional_media_support||[];
  const labels={ac4:"AC-4 audio",dolby_vision:"Dolby Vision",vulkan:"Vulkan / libplacebo",opencl:"OpenCL tone mapping"};
  const rows=Object.entries(labels).map(([id,label])=>{
    const fact=facts.find(row=>row.id===id)||{support_available:false,detail:"Support information has not been reported by this server."};
    return `<details class="system-capability"><summary><span>${esc(fact.label||label)}</span><span class="system-state ${fact.support_available?'system-good':'muted'}">${fact.support_available?"Available":"Unavailable"}</span></summary><div class="system-detail">${String(fact.detail||"").split("\n").map(esc).join("<br>")}</div></details>`;
  }).join("");
  return `<section class="card system-card"><h2>Additional media support</h2>${rows}</section>`;
}
function systemPerformanceCard(sys){
  const optimization=sys.transcoder_optimization||{}, comparison=optimization.comparison||optimization.report?.comparison;
  const defaults=[{id:"1080p-h264-sdr",label:"1080p H.264 SDR → 720p SDR"},{id:"4k-hevc-sdr",label:"4K HEVC SDR → 1080p SDR"},{id:"4k-hevc-hdr10",label:"4K HEVC HDR10 → 1080p SDR"}];
  const cases=comparison?.cases?.length?comparison.cases:defaults;
  const results=comparison?.results||[];
  const names={software:"CPU",nvenc:"NVIDIA NVENC",qsv:"Intel Quick Sync",vaapi:"VA-API",videotoolbox:"VideoToolbox"};
  const backends=["software",...Object.keys(names).filter(backend=>backend!=="software"&&(sys.encoders?.[backend]||results.some(row=>row.backend===backend)))];
  const complete=row=>row&&row.completion==="complete"&&!row.fallback&&Number.isFinite(row.median_seconds)&&row.median_seconds>0;
  const value=(item,backend)=>{
    if(comparison?.status==="stale")return '<span class="muted">Out of date</span>';
    const rows=results.filter(row=>row.case_id===item.id&&row.backend===backend);
    const row=rows.length===1?rows[0]:null;
    if(complete(row))return `${row.median_seconds.toFixed(2)}`;
    return row?`<span class="muted">${esc(row.fallback?"Fallback":row.completion||"Not measured")}</span>`:rows.length?'<span class="muted">See test details</span>':"—";
  };
  const path=row=>{
    const decoders={software:"CPU",videotoolbox:"VideoToolbox",cuda:"NVDEC",qsv:"Quick Sync",vaapi:"VA-API"};
    const processing={cpu:"CPU processing",vt_scale_sdr:"VideoToolbox scaling",vt_scale_hdr10:"VideoToolbox scaling",vt_tonemap_metal:"Metal tone mapping",tonemap_cuda:"CUDA tone mapping",vpp_qsv:"Quick Sync processing",tonemap_vaapi:"VA-API tone mapping",libplacebo:"Vulkan / libplacebo",libplacebo_software:"Vulkan / CPU frame transfer",libplacebo_vaapi:"Vulkan / VA-API",tonemap_opencl:"OpenCL tone mapping"};
    return [row.decoder?`${decoders[row.decoder]||row.decoder} decode`:"",row.pipeline?processing[row.pipeline]||row.pipeline:""].filter(Boolean).map(esc).join(" · ");
  };
  const details=results.map(row=>`<p><strong>${esc(cases.find(item=>item.id===row.case_id)?.label||row.case_id)} · ${esc(names[row.backend]||row.backend)}</strong><br>${path(row)}${row.decoder||row.pipeline?"<br>":""}${complete(row)?`${row.median_seconds.toFixed(2)} s${Number.isFinite(row.realtime_ratio)?` · ${row.realtime_ratio.toFixed(2)}× realtime`:""}${Number.isFinite(row.relative_to_cpu)?` · ${row.relative_to_cpu.toFixed(2)}× CPU baseline`:""}${Number.isFinite(row.cpu_percent)?` · FFmpeg process CPU ${row.cpu_percent.toFixed(0)}%`:""}`:esc(row.error||row.completion||"Not measured")}${row.device_id?` · ${esc(row.device_id)}`:""}${Number.isFinite(row.median_pipeline_seconds)?`<br>FFmpeg interval ${row.median_pipeline_seconds.toFixed(2)} s`:""}${Number.isFinite(row.median_startup_seconds)?` · Process overhead estimate ${row.median_startup_seconds.toFixed(2)} s`:""}</p>${row.command?.length?`<details class="system-probes"><summary>Command</summary><pre>${esc(row.command.map(arg=>JSON.stringify(arg)).join(" "))}</pre></details>`:""}`).join("");
  const definition=cases[0]||{}, frames=definition.frame_count||240, fps=(definition.fps_num||24)/(definition.fps_den||1), seconds=frames/fps;
  const codec=String(definition.output_codec||"h264").toUpperCase().replace("H264","H.264"), bitrate=(definition.bitrate_kbps||4000)/1000;
  const source=comparison?.fixture_source;
  const link=(url,label)=>typeof url==="string"&&/^https:\/\//.test(url)?`<a href="${esc(url)}" target="_blank" rel="noopener noreferrer">${esc(label)}</a>`:esc(label);
  const attribution=source?`<span>Source</span><span>${link(source.source_url,source.title)} · ${esc(source.attribution)} · ${link(source.license_url,source.license)} · Adapted excerpt</span>`:"";

  const status=optimization.running?"Comparison running · playback stops the test.":optimization.error||comparison?.error||"";
  return `<section class="card system-card system-performance" id="system-performance"><div class="system-heading"><h2>Playback performance</h2><span class="muted system-small">${optimization.running?"Running…":comparison?.status==="complete"?"Real-media tests complete":comparison?"Comparison incomplete":"Real-media tests not run"}</span></div>
    <div class="system-comparison-scroll"><table aria-label="Representative media comparison"><thead><tr><th>Source → output</th>${backends.map(backend=>`<th>${esc(names[backend])} (s)</th>`).join("")}</tr></thead><tbody>${cases.map(item=>`<tr><td>${esc(item.label)}</td>${backends.map(backend=>`<td>${value(item,backend)}</td>`).join("")}</tr>`).join("")}</tbody></table></div>
    <details class="system-probes"><summary>Test details</summary><div class="system-detail"><div class="system-kv"><span>Workload</span><span>${frames} frames @ ${Number.isInteger(fps)?fps:fps.toFixed(2)} fps (${Number.isInteger(seconds)?seconds:seconds.toFixed(2)} seconds)</span><span>Output</span><span>${esc(codec)} · ${bitrate} Mb/s</span><span>Pipeline</span><span>Decode → process → encode</span><span>Timing</span><span>Warm-up + median of 3 runs</span><span>Comparison</span><span>Elapsed time, FFmpeg process CPU and software fallback</span>${attribution}</div>${details}<p>FFmpeg process CPU: 100% = one CPU core; excludes GPU driver and operating-system services.</p></div></details>
    <div class="system-runline"><span class="system-small muted">Compare the same work on CPU and hardware pipelines.</span><button onclick="optimizeNodeTranscoder(this)"${optimization.running?" disabled":""}>${optimization.running?"Running comparison…":"Run comparison"}</button></div><p class="system-small ${optimization.error?'system-warn':'muted'}" id="system-comparison-status" role="status" aria-live="polite"${status?"":" hidden"}>${esc(status)}</p></section>`;
}
function nodeHwaccelChanged(){
  const status=document.getElementById("node-hwaccel-status");
  if(status)status.textContent="Unsaved preference · applies after restarting Cinema.";
}
function replaceSystemCard(id,html){
  const card=document.getElementById(id);
  if(!card)return;
  const open=Array.from(card.querySelectorAll("details")).map((node,index)=>node.open?index:-1).filter(index=>index>=0);
  card.outerHTML=html;
  const next=document.getElementById(id);
  if(next)Array.from(next.querySelectorAll("details")).forEach((node,index)=>{node.open=open.includes(index);});
}
async function optimizeNodeTranscoder(button){
  const field=/** @type {HTMLSelectElement} */(document.getElementById("node-hwaccel")), node_id=field.dataset.nodeId;
  button.disabled=true;
  try{
    await api("/system/transcoder",{method:"POST",body:{node_id}});
    if(SETTINGS_DATA.sys&&SETTINGS_DATA.sys.node_id===node_id){
      SETTINGS_DATA.sys.transcoder_optimization={...(SETTINGS_DATA.sys.transcoder_optimization||{}),running:true,error:null};
      replaceSystemCard("system-performance",systemPerformanceCard(SETTINGS_DATA.sys));
    }
  }catch(error){toast(error.message||"Could not start comparison");button.disabled=false;}
}
async function pollTranscoderOptimization(node_id,generation){
  if(!settingsCurrent(generation,"system"))return;
  try{
    const sys=await api("/system");
    if(!settingsCurrent(generation,"system")||!SETTINGS_DATA.sys||SETTINGS_DATA.sys.node_id!==node_id||sys.node_id!==node_id)return;
    SETTINGS_DATA.sys=sys;
    const field=/** @type {HTMLSelectElement|null} */(document.getElementById("node-hwaccel"));
    const draft=field?.value, focused=field&&document.activeElement===field;
    replaceSystemCard("system-versions",systemVersionsCard(sys));
    replaceSystemCard("transcoder-card",transcoderCard(sys));
    const next=/** @type {HTMLSelectElement|null} */(document.getElementById("node-hwaccel"));
    if(next&&draft&&Array.from(next.options).some(option=>option.value===draft)){
      next.value=draft;
      if(draft!==(sys.hwaccel_requested||sys.hwaccel_pref||"auto"))nodeHwaccelChanged();
      if(focused)next.focus();
    }
    replaceSystemCard("system-performance",systemPerformanceCard(sys));
  }catch(error){
    if(error&&error.status===401)throw error;
    // The Settings timer retries transient reads; retain the running state.
  }
}
async function saveNodeHwaccel(button){
  const field=/** @type {HTMLSelectElement} */(document.getElementById("node-hwaccel")), preference=field.value, node_id=field.dataset.nodeId;
  button.disabled=true;
  try{
    const result=await api("/system/transcoder",{method:"PUT",body:{node_id,preference}});
    if(SETTINGS_DATA.sys&&SETTINGS_DATA.sys.node_id===node_id){SETTINGS_DATA.sys.hwaccel_requested=preference;SETTINGS_DATA.sys.hwaccel_restart_required=result.restart_required;}
    const status=document.getElementById("node-hwaccel-status");
    if(status)status.textContent=result.restart_required?"Saved. Restart this node to apply the encoding preference.":"Saved. This encoding preference is already in effect.";
    toast("Transcoding preference saved");
  }catch(error){toast(error.message||"Could not save encoding preference");}
  finally{button.disabled=false;}
}
function systemStorageCard(sys){
  const storage=sys.storage||{}, mounts=storage.mounts||[];
  const missing=mounts.filter(mount=>mount.read_bps==null).length, suspect=mounts.some(mount=>mount.cache_suspect);
  const summary=missing?`${missing} location${missing===1?"":"s"} not measured`:suspect?"Read measurement needs attention":"Details";
  return `<section class="card system-card system-compact" id="system-storage-card"><details id="system-storage"><summary><span>Storage</span><span class="system-small ${missing||suspect?'system-warn':'muted'}">${esc(summary)}</span></summary><div class="system-detail">${storageHtml(storage)}</div></details></section>`;
}
function systemLogsCard(){
  return `<section class="card system-card system-compact"><details id="system-logs" ontoggle="systemLogsToggled(this)"><summary><span>Logs</span><span class="system-small muted">Diagnostics</span></summary><div class="system-detail"><div class="row"><select id="loglvl" aria-label="Log severity" style="max-width:120px" onchange="refreshLogs()"><option value="info" selected>Info+</option><option value="warn">Warnings+</option><option value="error">Errors</option><option value="debug">Debug+</option></select><label class="row" style="margin:0;font-size:13px;color:var(--muted)"><input type="checkbox" id="logauto" checked style="width:auto"> auto-refresh</label><button class="ghost sm" onclick="refreshLogs()">Refresh</button></div><div class="logbox" id="logbox">Expand to load logs.</div></div></details></section>`;
}
function systemLogsToggled(details){
  if(details.open)refreshLogs();
}
function systemPanel(sys){
  const platform=sys.platform==="macos"?"macOS":sys.platform==="linux"?"Linux":sys.platform||"";
  return `<div class="system-settings"><div class="system-title"><h1>System</h1><span class="muted system-small">${esc(platform)}</span></div>${ffmpegSupportWarning(sys)}${systemAttentionHtml(sys)}${systemVersionsCard(sys)}${transcoderCard(sys)}${systemAdditionalSupportCard(sys)}${systemPerformanceCard(sys)}${systemStorageCard(sys)}${systemLogsCard()}</div>`;
}
