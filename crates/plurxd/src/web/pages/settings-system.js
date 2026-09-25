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
  const first=document.querySelector(".setdrawer input"); if(first) first.focus();
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
    return ` <span class="muted" style="font-size:12px" title="Built from a context with no .git and no PLURX_BUILD_REF, so the image cannot name its commit — the build time below is the next best thing. Deploy with 'make docker-up', which stamps it.">(unstamped · built ${esc(builtAtLabel(sys.built_at))})</span>`;
  if(b==="v"+sys.version || b===sys.version) return "";
  return ` <span class="muted" style="font-size:12px">(${esc(b)})</span>`;
}
// The compile timestamp, shortened for a UI: the date is what distinguishes
// one deploy from another, the seconds never are.
function builtAtLabel(s){
  if(!s) return "unknown";
  const m=/^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(s);
  return m? `${m[3]} ${["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"][+m[2]-1]} ${m[4]}:${m[5]}Z` : s;
}
// One string that answers "which build am I looking at". Prefers the commit,
// falls back to when it was compiled — because "unknown" cannot answer the
// only question anybody asks this for, and a build time can.
function buildLabel(){
  const s=SERVER||{};
  const v=s.version||"?";
  const b=s.build;
  if(b && b!=="unknown" && b!=="v"+v && b!==v) return `${v} · ${b}`;
  return `${v} · built ${builtAtLabel(s.built_at)}`;
}
// What the boot probe found out about tone-mapping HDR on this machine.
//
// Worth a line of its own because falling back to the CPU chain is *silent*:
// everything still plays, 4K just stays slow, and there is nothing on screen to
// distinguish "this box has no GPU tone-map" from "the driver refused the
// graph" — which are the difference between shrugging and installing a package.
// Each rejected candidate says what it failed on, so the answer is here rather
// than in a log line from startup.
function toneMapHtml(tm){
  if(!tm) return `<span class="muted">—</span>`;
  const gpu = tm.selected && tm.selected!=="cpu";
  // Everything the probe rejected. The CPU chain is the reference the others
  // are measured against rather than a candidate, so it never appears as one —
  // "CPU tone-map: 1.0× — not selected" is a line that tells nobody anything.
  const rejected=(tm.verdicts||[]).filter(v=>!v.passed && v.pipeline!==tm.selected);
  const head = gpu
    ? `<span style="color:var(--good)">${esc(tm.selected_label)}</span>`
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
  const note = !gpu && rejected.length ? ` <span class="muted">— fell back</span>` : "";
  const rows=rejected.map(v=>
    `<div class="muted" style="font-size:12px">${esc(v.label)}: ${esc(v.rejected||"rejected")}</div>`
  ).join("");
  return `${head}${note}${rows}`;
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
  const was=btn.textContent; btn.disabled=true; btn.textContent="Measuring…";
  try{
    const st=await api("/system/storage",{method:"POST"});
    if(SETTINGS_DATA&&SETTINGS_DATA.sys) SETTINGS_DATA.sys.storage=st;
    renderSettings();
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
function systemPanel(sys,playbackEvents){
  const enc=sys.encoders||{};
  const pills=[["NVENC",enc.nvenc],["QuickSync",enc.qsv],["VA-API",enc.vaapi],["VideoToolbox",enc.videotoolbox]]
    .map(([n,ok])=>`<span class="pill" style="${ok?'color:var(--good);border-color:var(--good)':''}">${n} ${ok?'✓':'—'}</span>`).join(" ");
  return `${setHead("System",`${esc(sys.name)} · ${APP_NAME} ${esc(sys.version)}${buildTag(sys)} · up ${fmtUptime(sys.uptime_seconds)}`,pills)}${systemAttentionHtml(sys)}<div class="card"><h2 class="section" style="margin-top:0">This node</h2>
      <dl class="kvgrid system-grid">
        <dt>Data dir</dt><dd>${esc(sys.data_dir)}</dd>
        <dt>ffmpeg</dt><dd>${sys.ffmpeg_version?esc(sys.ffmpeg_version):`<span style="color:var(--bad)">not found at \`${esc(sys.ffmpeg)}\` — scanning and transcoding will fail</span>`}</dd>
        <dt>Transcoder</dt><dd>${esc(sys.encoder_selected)} <span class="muted">(preference: ${esc(sys.hwaccel_pref)})</span></dd>
        <dt>HDR tone-map</dt><dd>${toneMapHtml(sys.tone_map)}</dd>
        <dt>Storage</dt><dd>${storageHtml(sys.storage)}</dd>
        <dt>On this node</dt><dd><div class="system-now">
          <div class="system-now-counts">${sys.active_transcodes} managed transcode/remux session${sys.active_transcodes===1?'':'s'} · ${sys.libraries} librar${sys.libraries===1?'y':'ies'} · ${sys.users} user${sys.users===1?'':'s'}</div>
        </div></dd>
        <dt>Waiting fetches</dt><dd>${blockedGetsHtml(sys.blocked_gets)}</dd>
      </dl></div>
    <div id="settings-playback-events">${SETTINGS_LOADED.has("playbackEvents")?playbackSummaryCard(playbackEvents):`<div class="card"><h2 class="section" style="margin-top:0">Playback <span class="muted">(7 days)</span></h2><span class="muted">Loading playback history…</span></div>`}</div>
    <div class="card"><h2 class="section" style="margin-top:0">Logs</h2>
      <div class="row"><select id="loglvl" style="max-width:120px" onchange="refreshLogs()">
          <option value="info" selected>Info+</option><option value="warn">Warnings+</option>
          <option value="error">Errors</option><option value="debug">Debug+</option></select>
        <label class="row" style="margin:0;font-size:13px;color:var(--muted)"><input type="checkbox" id="logauto" checked style="width:auto"> auto-refresh</label>
        <button class="ghost sm" onclick="refreshLogs()">Refresh</button></div>
      <div class="logbox" id="logbox">Loading…</div></div>`;
}
