"use strict";
function setPreparedHandoff(on){
  setPreparedHandoffEnabled(!!on);
  // Honest about when: the capability is read on every exchange and the server
  // keeps the newest document it was sent, so this reaches it on the next
  // exchange of whatever is playing — not at the next stream.
  toast(on
    ?"This browser now offers prepared handoff — from the next control exchange"
    :"Prepared handoff switched off for this browser");
}

async function saveClusterBackup(btn){
  const err=document.getElementById("backup-settings-error"); if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      backup_destination:document.getElementById("backup-destination").value,
      backup_schedule_utc:document.getElementById("backup-schedule").value,
      backup_keep:Number(document.getElementById("backup-keep").value)
    }}));
    toast("Cluster backup settings saved"); if(btn)setCardSaved(btn);
  }catch(error){if(err)err.textContent=error.message;if(btn)btn.disabled=false;}
}

function clusterBackupCard(settings,readiness){
  const enabled=!!String(settings.backup_destination||"").trim();
  return setCard(`${cardHead("Portable cluster backup","Build a consistent, checksummed archive from a voter snapshot.",`<span class="pill${enabled?" ok":""}">${enabled?"scheduled":"destination empty"}</span>`)}
      <label class="setfield"><span>Destination</span><input id="backup-destination" value="${esc(settings.backup_destination||"")}" placeholder="/mnt/nas/plurx-backups"></label>
      <div class="setgrid"><label class="setfield"><span>Daily UTC time</span><input id="backup-schedule" value="${esc(settings.backup_schedule_utc||"02:30")}" placeholder="02:30"></label>
      <label class="setfield"><span>Artefacts to keep</span><input id="backup-keep" type="number" min="1" max="365" value="${Number(settings.backup_keep)||14}"></label></div>
      <div class="hint"><b>The destination controls scheduling directly.</b> Empty means no scheduled run. The checks below are advisory and never prevent saving or invoking a backup.</div>
      <details class="setdetails" open><summary>Readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"cluster_backup","destination","Existing absolute destination","The daemon must be able to write the configured directory.")}
      ${devReq(readiness,"cluster_backup","off_node_and_space","Off-node storage and free space","Use a different failure domain and retain at least two image sizes of free space.")}
      ${devReq(readiness,"cluster_backup","recent_success","Recent successful archive","A nightly backup is healthy when the last verified publish is less than 26 hours old.")}
      <p class="devcheck-note">Restore first onto an isolated instance. A file that has never been restored is not recovery evidence.</p></div></details>
      ${devGraduation("K-01's arm64 container-smoke leg (amd64 passed in run 3205) and its M5 loss drills (node and NAS loss, a physical restore, measured RPO and RTO) are recorded.","backup scheduling moves to Settings → Cluster beside restore guidance, as a permanent setting.")}<div class="err" id="backup-settings-error" role="alert"></div>${setCardFoot("saveClusterBackup")}`);
}

// A readiness status is evidence, not authority. The daemon can report facts
// it can observe on this node; the controls remain available regardless of
// the answer, including when the reading itself is unavailable.
const DEV_READINESS_LABEL={met:"met",unmet:"not met",unobservable:"not observable",unknown:"unknown",unavailable:"unavailable"};
function devReadinessRow(readiness,itemId,reqId){
  if(!readiness||!Array.isArray(readiness.items)) return null;
  const item=readiness.items.find(row=>row&&row.id===itemId);
  if(!item||!Array.isArray(item.requirements)) return null;
  return item.requirements.find(row=>row&&row.id===reqId)||null;
}
function devReadinessPill(found,readiness){
  if(readiness&&readiness.unavailable) return `<span class="pill warn">unavailable</span>`;
  if(!found) return readiness?`<span class="pill warn">not reported</span>`:`<span class="pill">checking…</span>`;
  const label=DEV_READINESS_LABEL[found.status]||"unknown";
  if(found.status==="met") return `<span class="pill" style="color:var(--good);border-color:var(--good)">${esc(label)}</span>`;
  if(found.status==="unmet") return `<span class="pill warn">${esc(label)}</span>`;
  return `<span class="pill">${esc(label)}</span>`;
}
function devReadinessEvidence(found,readiness){
  if(readiness&&readiness.unavailable) return `The prerequisite reading failed: ${readiness.unavailable}`;
  if(found) return found.evidence+(readiness?.observed_at_ms?` Observed ${new Date(readiness.observed_at_ms).toLocaleString()}.`:"");
  return readiness?"This server did not report on this prerequisite.":"";
}
function devReq(readiness,itemId,reqId,title,detail){
  const found=devReadinessRow(readiness,itemId,reqId);
  const key=`${itemId}:${reqId}`;
  return `<div class="devcheck">
    <div class="devcheck-head"><strong>${title}</strong><span class="devcheck-status" data-devstat="${esc(key)}">${devReadinessPill(found,readiness)}</span></div>
    <p class="devcheck-description">${detail}</p>
    <p class="devcheck-evidence" data-devev="${esc(key)}">${esc(devReadinessEvidence(found,readiness))}</p>
  </div>`;
}
// The last readiness document this page received, so a card that re-renders
// itself after its own save can redraw its advisory rows instead of dropping
// them back to "checking…". Set on both the success and the failure path, so
// "unavailable" survives a save too.
let DEVELOPER_READINESS=null;
function applyDeveloperReadiness(readiness){
  DEVELOPER_READINESS=readiness;
  document.querySelectorAll("[data-devstat]").forEach(node=>{
    const [itemId,reqId]=String(node.getAttribute("data-devstat")).split(":");
    node.innerHTML=devReadinessPill(devReadinessRow(readiness,itemId,reqId),readiness);
  });
  document.querySelectorAll("[data-devev]").forEach(node=>{
    const [itemId,reqId]=String(node.getAttribute("data-devev")).split(":");
    node.textContent=devReadinessEvidence(devReadinessRow(readiness,itemId,reqId),readiness);
  });
}

function devStaticReq(title,status,detail,tone){
  return `<div class="devcheck"><div class="devcheck-head"><strong>${title}</strong><span class="devcheck-status"><span class="pill${tone?` ${tone}`:""}">${status}</span></span></div><p class="devcheck-description">${detail}</p></div>`;
}
// Paul's Developer lifecycle (2026-09-28). A card is here only while its
// feature is not fully active or not fully tested, and it says what it is
// waiting on. When that lands the card graduates: to its proper settings
// section when a permanent on/off makes sense, otherwise the toggle (or the
// whole card) goes and the behaviour is simply how plurx works. Advisory
// text; nothing reads it, and it never gates the switch above it.
function devGraduation(waitingOn,then){
  return `<p class="devcheck-note devgrad"><b>Leaves Developer when:</b> ${waitingOn} <b>Then:</b> ${then}</p>`;
}
function clusterTransportRecoveryCard(readiness){
  return setCard(`${cardHead("Transport recovery","Automatic recovery for interrupted cluster transfers. No enable switch is required.",`<span class="pill ok">automatic</span>`)}
      <details class="setdetails"><summary>Deployment guidance and readiness</summary><div class="setdetails-body">
      <p class="hint"><b>Safe use is a deployment decision, not a code gate.</b> Keep a ready voter majority, deploy one node at a time, and expose the cluster API only on the trusted cluster network.</p>
      ${devReq(readiness,"cluster_transport_recovery","recovery_budgets","Recovery budgets","Keep chunk, whole-transfer, final-install, startup, and container-health allowances aligned. Defaults are 30 s, 1,200 s, and 120 s; measured state size may require a longer final-install allowance.")}
      ${devReq(readiness,"cluster_transport_recovery","cluster_api_advertised","Authenticated diagnostics","Every node must advertise its private Hiqlite API address, share the cluster API credential, and keep <code>/cluster/transport/sqlite</code> off the public Internet.")}
      ${devReq(readiness,"cluster_transport_recovery","cache_revocation_capability","Cache revocation","Every member of the exact committed configuration must prove the capability in its current heartbeat before cache-only recovery authorization opens.")}
      ${devReq(readiness,"cluster_transport_recovery","recovery_receipt","Recovery receipt","Qualify the real-TLS 3 MiB transport matrix, acknowledged writes, and twenty learner plus twenty voter recovery cycles before fleet rollout.")}
      <p class="devcheck-note">Use the supported one-node-at-a-time deployment when these checks are acceptable. Readiness is advisory; transport recovery remains automatic on clustered nodes.</p>
      </div></details>`);
}

function liveTvDeinterlaceCard(settings){
  const selected=settings.live_tv_deinterlace_output==="frame"?"frame":"field";
  return setCard(`${cardHead("Live TV deinterlace cadence","Choose whether software deinterlacing preserves frame cadence or emits one frame per field.",`<span class="pill">${selected==="field"?"Field rate":"Frame rate"}</span>`)}
      <label for="ltdeint">Output cadence</label>
      <select id="ltdeint"><option value="field"${selected==="field"?" selected":""}>Field rate (smoother motion)</option><option value="frame"${selected==="frame"?" selected":""}>Frame rate (lower bitrate)</option></select>
      <details class="setdetails" open><summary>Prerequisites and current status</summary><div class="setdetails-body">
      ${devStaticReq("Current selection",selected==="field"?"field rate":"frame rate","Applied to subsequent Live TV sessions that require software deinterlacing.","ok")}
      ${devStaticReq("Source identification","required","The tuner probe must report one of tt, bb, tb or bt. Unknown and future tokens remain unknown and do not opt into deinterlacing.","")}
      ${devStaticReq("Encoder readiness","not measured on this node","Field rate asks the software H.264 path to sustain up to 59.94 fps. Check the live FFmpeg log and playback stability on the target device.","warn")}
      <p class="devcheck-note">Advisory only. Readiness never disables this choice and this setting never gates Live TV enablement. If the player can copy an interlaced source, no deinterlace filter is inserted.</p>
      </div></details>${devGraduation("S-08's M5 media1 qualification (QSV/VAAPI idet, signalstats and wall time at field rate) is recorded.","the cadence choice moves to Settings → Live TV as a permanent option.")}<div class="err" id="ltdeinterr" role="alert"></div>
      ${setCardFoot("saveLiveTvDeinterlace")}`);
}

async function saveLiveTvDeinterlace(btn){
  const err=document.getElementById("ltdeinterr");if(err)err.textContent="";btn.disabled=true;
  try{
    const output=document.getElementById("ltdeint").value;
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{live_tv_deinterlace_output:output}}));
    const card=btn.closest(".setcard");if(card)card.outerHTML=liveTvDeinterlaceCard(saved);
    toast("Live TV deinterlace cadence saved");
  }catch(e){if(err)err.textContent=e.message||String(e);btn.disabled=false;}
}

// F-1: the same replicated switch, moved to Developer. Every readiness row is
// an observation or a dated qualification receipt; none is read on save or by
// the player. Enabling remains an operator choice even when rows are red.
function displayAwareAutoCard(settings){
  const enabled=!!settings.playback_display_aware_auto;
  return setCard(`${cardHead("Fit Auto to display","Choose a useful sustainable encode size for the fitted picture.",`<span class="pill" id="daqstate">${enabled?"Enabled":"Disabled"}</span>`)}
      ${togRow("pdisplayauto","Fit Auto to display","Compatible smooth originals remain preferred. When encoding is needed, use the active render area with at most 10% enlargement. Manual quality stays selectable.",enabled)}
      <div class="hint">This saved choice is authoritative. Readiness is advisory and never disables this switch or rejects Save.</div>
      ${devStaticReq("Combined source and runtime qualification","pending","Display-aware candidates, source-grade worker proofs and physical client recovery traces are being built and qualified together.","warn")}
      ${devGraduation("the combined display-aware Auto effort passes source-grade, device and runtime qualification on its exact candidate.","this control graduates to Playback if a permanent toggle remains useful, otherwise fitting Auto becomes the default.")}
      <div class="err" id="daqerr" role="alert"></div>${setCardFoot("saveDisplayAwareAuto")}`);
}

async function saveDisplayAwareAuto(btn){
  const err=document.getElementById("daqerr"); if(err) err.textContent="";
  const card=btn&&btn.closest?btn.closest(".setcard"):null;
  const revision=Number(card&&card.dataset?card.dataset.revision||0:0);
  const requested=(/** @type {HTMLInputElement} */ (document.getElementById("pdisplayauto"))).checked;
  if(btn) btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{playback_display_aware_auto:requested}}));
    if(SERVER) SERVER.playback_display_aware_auto=!!saved.playback_display_aware_auto;
    if(card&&!card.isConnected) return true;
    if(card&&Number(card.dataset.revision||0)!==revision){toast("Earlier display Auto choice saved; newer edit remains unsaved");return true;}
    const toggle=/** @type {HTMLInputElement|null} */ (document.getElementById("pdisplayauto"));if(toggle)toggle.checked=!!saved.playback_display_aware_auto;
    const state=document.getElementById("daqstate");if(state)state.textContent=saved.playback_display_aware_auto?"Enabled":"Disabled";
    toast("Display Auto saved");if(btn)setCardSaved(btn);
  }catch(error){if(err)err.textContent=error.message||String(error);if(btn)btn.disabled=false;}
}

function autoQualityCard(settings){
  const enabled=!!settings.playback_auto_abr;
  const webController=typeof autoControllerTick==="function";
  return setCard(`${cardHead("Adaptive Auto quality","Let Auto react to changing playback conditions during a stream.",`<span class="pill" id="aqstate">${enabled?"Enabled":"Disabled"}</span>`)}
      ${togRow("pabr",`Adjust Auto quality while playing <span class="pill warn">experimental</span>`,`Off keeps the server's first Auto choice and the full manual quality menu. On lets supported clients adjust rungs and recover supply stalls.`,enabled)}
      <div class="hint"><b>This switch is the enable path.</b> It is saved on the server and is never disabled or overridden by the readiness rows below. It affects eligible Auto sessions; a manual rung remains the viewer's choice.</div>
      <details class="setdetails" open><summary>Requirements and current evidence</summary><div class="setdetails-body">
      ${devStaticReq("Web controller implementation",webController?"present":"absent","This page can see the browser controller function. Presence confirms the implementation loaded; the dated recovery traces below describe runtime qualification.",webController?"ok":"warn")}
      ${devStaticReq("Native controllers","not met in this build","Apple and Android native adapters have not shipped; A-05, which builds them, is unclaimed. The server setting remains selectable and applies to a native client only when that client's controller exists.","warn")}
      ${devStaticReq("Chrome shaped-network recovery","passed · 2026-09-27","The strict 8 to 1.1 to 0.35 Mb/s two-cliff trace of the web tree merged in PR #569 downshifted once per cliff with 66.6 ms and 66.6 ms maximum frame gaps, inside the 100 ms limit, and no hitch, wait, stall or restart. Later main has not been re-traced, and the one-cliff 8 to 1.5 Mb/s profile has not been re-run on it.","ok")}
      ${devStaticReq("Firefox shaped-network recovery","passed · 2026-09-27","The same two-cliff trace on the same tree held 83.34 ms and 83.34 ms maximum gaps with no hitch, wait, stall or restart. Earlier Firefox runs failed the gap limit; this is one pass, not a repeat series.","ok")}
      ${devStaticReq("HDR playback","not measured · 2026-09-28","No HDR trace exists. The synthetic HDR fixture never presented a first frame on the non-HDR test host, where the server chose CPU tone-mapping to SDR, and the HDR prerequisites are still unresolved. A real HDR display trace remains owed.","warn")}
      ${devStaticReq("Safari and physical devices","not measured · 2026-09-28","Safari WebDriver still fails to create a session before playback. The iPhone Debug attempt produced no first frame in 60 s, and no Apple TV, iPhone or Android trace exists. Record first frame, gap, rung, restarts and unexpected SDR transitions per platform.","warn")}
      <p class="devcheck-note">Evidence is dated because this server cannot inspect another device's trace. Recheck the architecture-review fleet evidence before enabling broadly. These observations never gate this checkbox.</p>
      </div></details>${devGraduation("A-04's D3 matrix is complete (Safari, HDR and Apple and Android physical traces) and A-05's native controllers ship.","Paul chooses: the switch returns to Playback as a permanent toggle, or it is removed because adjusting while playing is simply how Auto works.")}<div class="err" id="aqerr" role="alert"></div>
      ${setCardFoot("saveAutoQuality")}`);
}

async function saveAutoQuality(btn){
  const err=document.getElementById("aqerr"); if(err) err.textContent="";
  const card=btn&&btn.closest?btn.closest(".setcard"):null;
  const revision=Number(card&&card.dataset?card.dataset.revision||0:0);
  const requested=document.getElementById("pabr").checked;
  if(btn) btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{playback_auto_abr:requested}}));
    if(SERVER) SERVER.playback_auto_abr=!!saved.playback_auto_abr;
    if(card&&!card.isConnected) return true;
    const currentRevision=Number(card&&card.dataset?card.dataset.revision||0:0);
    if(card&&currentRevision!==revision){
      toast("Earlier Auto quality choice saved; newer edit remains unsaved");
      return true;
    }
    const toggle=document.getElementById("pabr");
    if(toggle) toggle.checked=!!saved.playback_auto_abr;
    const state=document.getElementById("aqstate");
    if(state) state.textContent=saved.playback_auto_abr?"Enabled":"Disabled";
    toast("Auto quality saved"); if(btn) setCardSaved(btn);
  }catch(e){if(err) err.textContent=e.message||String(e); if(btn) btn.disabled=false;}
}

function preparedQualityCard(settings,readiness){
  return setCard(`${cardHead("Quality switching","Prepare the next stream before replacing the one you are watching.",`<span class="pill" id="pqhstate">${settings.prepared_quality_handoff?"Enabled":"Disabled"}</span>`)}
      ${togRow("pqh",`Prepare quality changes <span class="pill warn">experimental</span>`,`Uses a second player and temporary server capacity to reduce interruptions. Applies after saving to the next eligible quality change.`,settings.prepared_quality_handoff)}
      <div class="hint">Still experimental. This saved switch is authoritative; readiness is advisory and never overrides your choice. The browser-specific capability control is below.</div>
      <details class="setdetails"><summary>Readiness and device qualification</summary><div class="setdetails-body">
      <p class="hint">These readings describe this server. Missing evidence does not disable the setting.</p>
      <h3 class="devcheck-group">Server readiness</h3>
      ${devStaticReq("Control protocol",settings.playback_control_protocol_v1?"met":"not met","Enable protocol advertisement in Playback → Advanced server delivery for new sessions. An existing session without the control block cannot prepare a replacement.",settings.playback_control_protocol_v1?"ok":"warn")}
      ${devReq(readiness,"prepared_quality_handoff","server_preparation_is_real","Server preparation","The server must reserve capacity, warm the unpublished replacement, and release both the worker and capacity slot on cancellation.")}
      ${devStaticReq("Throughput measurement","supported","Client and server reporting exists. This is implementation support, not proof that a particular session has enough measured throughput.","")}
      <h3 class="devcheck-group">Device qualification</h3>
      ${devReq(readiness,"prepared_quality_handoff","client_two_player_handoff","Client handoff","Apple, Android, and web contain the adapter. Physical-device evidence must still demonstrate aligned playback and a visible first frame.")}
      ${devReq(readiness,"prepared_quality_handoff","fleet_receipt","Fleet observation","Consecutive physical-client handoffs, stable memory, and measured fallback interruption improve confidence; they do not unlock or block the switch.")}
      <p class="devcheck-note">Missing qualification does not disable the feature or override the saved choice. A failed preparation falls back to reopening the stream.</p>
      </div></details>
${devGraduation("the quality-switch continuity build finishes (M2-Android, M1-web, M3) and the physical-client fleet receipt is recorded.","the switch moves to Settings → Playback as a permanent on/off.")}      <div class="err" id="pqherr" role="alert"></div>
      ${setCardFoot("savePreparedQuality")}`);
}
function systemAttentionHtml(sys){
  const storage=sys.storage||{}, mounts=(storage.mounts||[]).filter(m=>m.read_bps==null||m.cache_suspect);
  const messages=mounts.map(m=>`<p><b>${esc((m.roots||[]).join(', ')||'Storage location')}</b><br>${esc(m.note||'No current read measurement is available.')} ${storage.measured_at?`<span class="muted">Observed ${fmtAgo(storage.measured_at)}.</span>`:''}</p>`);
  if(!sys.ffmpeg_version)messages.push('<p><b>Media tools unavailable.</b> Scanning and transcoding need the configured FFmpeg executable.</p>');
  if(sys.tone_map&&!sys.tone_map.ran&&sys.tone_map.verdicts?.some(v=>v.rejected))messages.push(`<p><b>HDR processing has unverified capabilities.</b> ${esc(sys.tone_map.verdicts.find(v=>v.rejected)?.rejected||'See the capability details below.')}</p>`);
  if(sys.login_proxy_advisory)messages.push('<p><b>Login throttling sees most clients at one proxy address.</b> Configure this node’s <code>server.trusted_proxies</code> only if that proxy overwrites or appends <code>X-Forwarded-For</code> correctly. This is advisory; sign-in remains available.</p>');
  return messages.length?`<section class="system-attention"><h2>Needs attention on this node</h2>${messages.join('')}<div class="row"><a class="ghost sm" href="#/settings/libraries">Review library locations</a><a class="ghost sm" href="#/settings/cluster">Cluster health</a></div></section>`:'';
}

function toggleMobileSearch(){
  const top=document.querySelector('header.top');if(!top)return;
  const open=!top.classList.contains('search-open');top.classList.toggle('search-open',open);
  const button=top.querySelector('.mobile-search');if(button)button.setAttribute('aria-expanded',String(open));
  if(open)document.getElementById('q')?.focus();
}

// Advisory rows about the last directed quality change, inside the card that
// already exists. Paul's standing rule: features are not gated in code, so
// nothing here is read by anything that decides anything -- it is a window
// onto state the player keeps regardless.
//
// Empty when there is no player and no history, which is what the settings
// page looks like at rest. The rows appear only once there is something true
// to say about a change that actually happened.
function directedChangeDeveloperRows(){
  const p=PLAYER;
  if(!p) return "";
  const change=p.directedChange;
  const outcome=change&&change.outcome;
  const said={committed:"committed",declined:"declined",timed_out:"timed out",
    failed:"fell back",superseded:"superseded"}[outcome]||null;
  const outcomeText=!change ? "no directed change yet"
    : said==null ? "waiting for an offer"
    : outcome==="committed"&&Number.isFinite(change.detail)
      ? `committed ${Math.round(change.detail)} ms` : said;
  const preparation=p.controlLastPreparation||"not reported";
  const requested=p.autoRequestedHeight>0?`${Math.round(p.autoRequestedHeight)}p`:"none";
  const delivered=p.autoHeight>0?`${Math.round(p.autoHeight)}p`:"unknown";
  if(!change&&!p.controlLastPreparation&&!(p.autoRequestedHeight>0)&&!(p.autoHeight>0))
    return "";
  return `<details class="setdetails"><summary>Last quality change</summary><div class="setdetails-body">
    ${devStaticReq("Outcome",esc(outcomeText),
      "What became of the most recent directed quality change in this player.")}
    ${devStaticReq("Server preparation",esc(String(preparation)),
      "The server's own last word on staging a successor for this session. An absent value means a relay peer that does not evaluate it, which is never a refusal.")}
    ${devStaticReq("Auto rung",`${esc(requested)} &rarr; ${esc(delivered)}`,
      "Requested by this browser's automatic controller, then delivered by the server. They are deliberately separate readings.")}
    <p class="devcheck-note">Advisory only. Nothing on these rows enables, disables or blocks a quality change.</p>
    </div></details>${preparedSwitchDeveloperRows(p)}`;
}

// M3's rows, and the one measurement this client does not take.
//
// The first three are what the last prepared commit in this player measured.
// The enable section below them says what an audible-seam probe would need and
// whether each condition is met -- and every answer is "no", which is why the
// probe is not built. It is advisory in the strict sense: there is no switch
// here, nothing reads these back, and the prepared handoff is unaffected by
// every word of it.
function preparedSwitchDeveloperRows(p){
  if(!p||!p.switchCommit) return "";
  const ledger=preparedSwitchLedger(p);
  return `<details class="setdetails"><summary>Measuring the last switch</summary><div class="setdetails-body">
    ${devStaticReq("Frames at the switch",esc(String(ledger.frames)),
      "Dropped frames on the predecessor over the two seconds before the commit plus the successor over the two seconds after, and how much of that window was sampled.")}
    ${devStaticReq("Tap to new quality",esc(String(ledger.visible)),
      "From the viewer's tap to the successor's first frame. Reported, never judged.")}
    ${devStaticReq("Audio at the switch",esc(String(ledger.audio)),"")}
    <p class="devcheck-note">An audible-seam probe would need all three of the following. None of them hold, so it is not built \u2014 and this section blocks nothing, because there is nothing here to enable.</p>
    ${preparedSwitchAudioEnable().map(([title,detail,met])=>
      devStaticReq(esc(title),met?"Met":"Not met",esc(detail),met?"ok":"")).join("")}
    <p class="devcheck-note">Advisory only. Instrumentation never gates a quality change, and an unmeasurable row is never read as a failure.</p>
    </div></details>`;
}

function seekScratchReservationsCard(){
  // The transport of the session that is actually open, which means asking
  // with the same `hevcCopy` the create asked with -- it is exactly the bit
  // that flips Safari's answer, so hardcoding it false would report hls.js
  // for an HEVC copy session the server has correctly put in the
  // conservative class. No player open means no session and no class.
  let transport=null;
  let playing=false;
  try{
    const p=typeof PLAYER!=="undefined"?PLAYER:null;
    playing=!!(p&&p.sessionId);
    if(playing&&typeof plannedHlsTransport==="function"){
      const codec=String((p.source&&p.source.video_codec)||"").toLowerCase();
      transport=plannedHlsTransport(!!p.copyHls&&["hevc","h265","hevc10"].includes(codec));
    }
  }catch(e){}
  const shortGrace=transport==="hlsjs";
  return setCard(`${cardHead("Seek scratch accounting",
      "One charge per stream, released when its bytes are gone. What repeated seeks are allowed to cost.",
      `<span class="pill ok">automatic</span>`)}
      <div class="hint">No switch here. Every row is an observation about this build or this browser; none of them enables, disables or hides anything, and none of them changes what the server charges.</div>
      <details class="setdetails" open><summary>Requirements and readiness</summary><div class="setdetails-body">
      <h3 class="devcheck-group">Accounting</h3>
      ${devStaticReq("One charge per stream","met",
        "Admission, growth, retirement and release all settle in one ledger, so a stream moving between the live and retired registries is counted once. The figures behind a refusal &mdash; charged, requested, configured &mdash; are in the server log beside it.","ok")}
      ${devStaticReq("Writers settle before capacity is returned","met",
        "The copy reader registers before it is spawned, so retirement waits for the segment it is still writing instead of measuring around it. A writer that outlives the wait keeps the whole producer charge and says why.","ok")}
      ${devStaticReq("A full budget is a temporary refusal","met",
        "Exhausted scratch answers 503 with a retryable notice beside the picture. The stream you are watching keeps playing, and the destination you asked for is kept for Try again.","ok")}
      <h3 class="devcheck-group">Shorter retention after you close a stream</h3>
      ${devStaticReq("This session's transport",
        transport===null?"no stream open":transport==="hlsjs"?"hls.js":"native HLS",
        transport===null
          ? "This is a property of a stream, not of a browser: it depends on the title's codec as well as on this browser, so there is nothing to report until something is playing. Open a title and come back."
          : "The web player destroys its hls.js instance before it releases the session, so the server may drop that stream's retired objects one segment later instead of a whole playlist later. Native HLS has no bounded retry tail and keeps the original promise.",
        shortGrace?"ok":"")}
      ${devStaticReq("Apple and unknown clients","original grace",
        "Apple releases the session before it replaces the item, so the old item still exists while the release runs, and no finite AVFoundation retry bound has been established. Those sessions keep the full promise &mdash; missing information never shortens one.","")}
      ${devStaticReq("Reads already in flight","protected",
        "A response that has already been accepted keeps its object open and charged until it finishes, whatever the deadline says. A new read after the deadline gets the ordinary gone answer.","ok")}
      <h3 class="devcheck-group">More than three streams at once</h3>
      ${devStaticReq("Copy and remux output","enforced write boundary",
        "Every object the native copy writer publishes &mdash; the initialization segment, each media segment, each playlist rewrite &mdash; is authorized before it exists, so these sessions start with a startup allowance and grow under a real bound instead of reserving the whole per-session ceiling up front.","ok")}
      ${devStaticReq("Transcoded output","enforced write boundary",
        "FFmpeg's HLS muxer uploads every object to a loopback endpoint in this server (<code>-method PUT</code>) instead of writing the file itself. Each piece is authorized against the scratch budget before it is written, so a transcode starts with a startup allowance and grows. A refusal stops reading the upload and marks the stream starved, and the flow controller suspends FFmpeg until the budget frees.","ok")}
      ${devStaticReq("Legacy copy writer and retries","enforced write boundary",
        "A copy the segmenter cannot cut, a cluster takeover, and the legacy retry a segmenter session keeps in reserve all use FFmpeg's muxer too. They upload through the same endpoint, each attempt on its own lane, so a replaced attempt can never overwrite its successor's objects.","ok")}
      ${devStaticReq("FFmpeg can write HLS over HTTP","met in the published image",
        "The endpoint needs the engine's <code>http</code> output protocol, which both engines in the published image include. A custom build without network protocols fails every transcode and every copy that uses FFmpeg's muxer, with &ldquo;Protocol not found&rdquo; in the server log. Nothing checks this ahead of time.","ok")}
      ${devStaticReq("Windows","same boundary, not yet run natively",
        "The endpoint is loopback TCP and ordinary file writes, with nothing platform-specific in it. It is built for Windows, but no native runtime receipt exists yet.","")}
      <p class="devcheck-note">Advisory only. Capacity, authorization and retention rules are unchanged by anything on this card; the global scratch ceiling is still the one on <a href="#/settings/playback">Playback</a>.</p>
      </div></details>${devGraduation("the seek-scratch physical acceptance in its status page (Safari, Apple and Android rapid-seek passes) is recorded.","this card is removed. The behaviour has no switch; the scratch ceiling stays in Playback.")}`);
}
function liveTvEnableCard(settings){
  queueMicrotask(()=>{const mount=document.getElementById("dev-live-tv-readiness");if(mount)refreshLiveTvEnableReadiness(mount);});
  return setCard(`${cardHead("Enable Live TV","Allow eligible servers to use the network tuner.",`<span class="pill">${settings.live_tv_enabled?"enabled":"off"}</span>`)}
    ${togRow("dev-live-tv-enable","Enable Live TV","This saved choice is authoritative. Readiness observations never disable the control.",!!settings.live_tv_enabled)}
    <p class="hint">For safe operation, upgrade cluster servers, keep their clocks synchronized, and keep a ready voter majority. At least one server needs tuner connectivity and writable scratch. DVR workers need writable recording storage; matching paths alone do not prove shared storage.</p>
    <div id="dev-live-tv-readiness" aria-live="polite">Checking current prerequisites…</div>
    <p class="hint">A lost worker can leave a physical tuner connection behind temporarily. Other free tuners remain available. DRM remains unsupported.</p>
    ${devGraduation("the Live TV plans' outstanding fleet prompts are recorded: L-02's leader-restart, cold/warm-start (L6) and scratch-fault (L9) prompts, and L-03's capacity offer and caption-positive pass.","the switch moves to Settings → Live TV as a permanent on/off.")}<div id="dev-live-tv-error" class="err" role="alert"></div>${setCardFoot("saveLiveTvEnable")}`);
}
async function refreshLiveTvEnableReadiness(mount){
  try{
    const r=await api("/live-tv/readiness",{signal:AbortSignal.timeout(40000)});
    if(!mount.isConnected)return;
    mount.innerHTML=`<ul>${r.checks.map(c=>`<li><strong>${c.ready?"Met":"Not met"}:</strong> ${esc(c.message)}</li>`).join("")}</ul><p class="hint">Advisory only. Unknown or unmet requirements do not prevent enabling.</p>`;
  }catch(e){if(mount.isConnected)mount.textContent=`Readiness unavailable: ${e.message}. You can still enable Live TV.`;}
}
async function saveLiveTvEnable(button){
  const err=document.getElementById("dev-live-tv-error");if(err)err.textContent="";
  if(button)button.disabled=true;
  try{
    const saved=await api("/settings",{method:"PUT",body:{live_tv_config_generation:SETTINGS.live_tv_config_generation,
      live_tv_enabled:/** @type {HTMLInputElement} */ (document.getElementById("dev-live-tv-enable")).checked},signal:AbortSignal.timeout(45000)});
    cacheSettings(saved);toast("Live TV enablement saved");if(button)setCardSaved(button);
  }catch(e){if(err&&err.isConnected)err.textContent=e.message;if(button&&button.isConnected)button.disabled=false;}
}
function clusterPlacementCard(settings){
  const ready=!!settings.cluster_media_pool_ready;
  return setCard(`${cardHead("Cluster media placement","Place new streams on compatible workers with spare capacity.",'<span class="pill">Cluster</span>')}
    ${togRow("cluster-placement-enabled","Enable remote media placement","Existing sessions keep their owner; new sessions can use another worker.",!!settings.cluster_media_pool_enabled)}
    ${togRow("cluster-takeover-enabled","Enable session takeover","Allow compatible expired sessions to acquire a new owner when remote placement is enabled.",!!settings.cluster_session_takeover_enabled)}
    ${devStaticReq("Peer protocol observations",ready?"met":"not fully met","Compatible reachable peers are considered individually. A stale or older peer never disables the other workers.",ready?"ok":"")}
    ${devStaticReq("Worker resources","checked for each start","A selected worker needs the requested codecs, readable source, writable scratch space and an available hardware or CPU reservation.","")}
    <p class="hint">Requirements are advisory and never prevent saving. Unknown performance measurements affect ranking only. Streams continue through their existing ingress proxy, so ingress bandwidth is still used.</p>
    ${devGraduation("the media-pool plan's physical-device corpus and its takeover budget (CLUSTER-MEDIA-POOL-PLAN §8.8 and §8.9) are measured.","both switches move to Settings → Cluster as permanent on/off controls.")}<div class="err" id="cluster-placement-error" role="alert"></div>${setCardFoot("saveClusterPlacement")}`,{id:"cluster-placement-settings"});
}
async function saveClusterPlacement(btn){
  const err=document.getElementById("cluster-placement-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{
      cluster_media_pool_enabled:/** @type {HTMLInputElement} */(document.getElementById("cluster-placement-enabled")).checked,
      cluster_session_takeover_enabled:/** @type {HTMLInputElement} */(document.getElementById("cluster-takeover-enabled")).checked
    }}));
    const card=document.getElementById("cluster-placement-settings");if(card)card.outerHTML=clusterPlacementCard(saved);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
function boundedCatalogueCard(settings,readiness){
  return setCard(`${cardHead("Local catalogue reads","Use each replica for browsing when its current consistency proof permits it.",'<span class="pill">Cluster reads</span>')}
    ${togRow("bounded-catalogue-reads","Enable local catalogue reads","Falls back to authority when a replica is stale or a watch-write position is unknown.",settings.bounded_replica_reads!==false)}
    ${devReq(readiness,"bounded_catalogue_reads","replica_proof","Fresh replica proof","The current term, quorum watermark and apply lag are checked for each read. Saving this preference does not require a readiness result.")}
    ${devReq(readiness,"bounded_catalogue_reads","watch_floor","Watch state consistency","The web client carries its latest acknowledged watch-write position across nodes for 60 seconds. Other clients retain authority reads until they implement the same echo.")}
    ${devGraduation("K-04's §5.1 lab readout and rolling-upgrade check are recorded. The web read-after echo and the on-by-default preference already shipped (5ba02212f).","Paul chooses: the switch moves to Settings → Cluster, or it is removed because every replica simply serves browsing this way.")}<div class="err" id="bounded-catalogue-error" role="alert"></div>${setCardFoot("saveBoundedCatalogueReads")}`,{id:"bounded-catalogue-settings"});
}
async function saveBoundedCatalogueReads(btn){
  const err=document.getElementById("bounded-catalogue-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{bounded_replica_reads:/** @type {HTMLInputElement} */(document.getElementById("bounded-catalogue-reads")).checked}}));
    const card=document.getElementById("bounded-catalogue-settings");if(card)card.outerHTML=boundedCatalogueCard(saved,DEVELOPER_READINESS);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
function clusterClockCard(readiness){
  return setCard(`${cardHead("Cluster clock observation","Authenticated peer offset and local clock continuity.",`<span class="pill">measurement only</span>`)}
    ${devReq(readiness,"cluster_clock","contract","Clock contract","This release observes clocks without changing acquisition or readiness.")}
    ${devReq(readiness,"cluster_clock","coverage","Observation coverage","Every committed remote member must answer a fresh authenticated probe.")}
    ${devReq(readiness,"cluster_clock","upper_bound","Worst observed upper bound","Absolute offset plus uncertainty; an unknown peer has no numeric offset.")}
    ${devReq(readiness,"cluster_clock","consequence","Readiness consequence","The fixed 2,000 ms contract awaits fleet evidence before enforcement.")}
    ${devGraduation("the identified measurement and enforcement releases have their fleet acceptance receipts.","clock diagnostics move to Settings → Cluster; there is no manual on/off control.")}`);
}
function developerPanel(settings,readiness){
  const destinations=`<nav class="setdestinations" aria-label="Everyday settings">
    <a href="#/settings/livetv"><strong>Live TV <span aria-hidden="true">↗</span></strong><span>Tuner, guide, recording and library channels</span></a>
    <a href="#/settings/playback"><strong>Playback <span aria-hidden="true">↗</span></strong><span>Defaults, streaming and compatibility</span></a>
    <a href="#/settings/cluster"><strong>Cluster <span aria-hidden="true">↗</span></strong><span>Health and recovery</span></a>
  </nav>`;
  const browser=setCard(`${cardHead("Second player in this browser","Advertise this browser's ability to prepare a replacement stream.",`<span class="pill acc">this browser</span>`)}
      ${togRow("pdp","Allow a second player","Uses additional device memory and decoder capacity. Saved automatically in this browser; the next reporter exchange sends the change.",preparedHandoffEnabled(),'onchange="setPreparedHandoff(this.checked)"')}
      <div class="hint">A failed preparation falls back to reopening the stream. Prepare quality changes above must also be enabled on the server.</div>${devGraduation("prepared quality handoff graduates.","this browser permission moves with it to Settings → Playback.")}
      ${directedChangeDeveloperRows()}`,{local:true});
  return `${setHead("Developer","Experimental features still awaiting device qualification.")}
      ${destinations}
      <div class="setsection" id="enable-live-tv"><h2>Enable Live TV</h2><p>Cluster use of the network tuner, with advisory prerequisites.</p></div>${liveTvEnableCard(settings)}
      <div class="setsection"><h2>Cluster work</h2><p>Remote media placement and replica catalogue reads.</p></div>${clusterPlacementCard(settings)}${boundedCatalogueCard(settings,readiness)}${clusterClockCard(readiness)}
      <div class="setsection" id="enable-hevc-copy"><h2>Enable HEVC copy</h2><p>The saved choice controls playback. Requirements below are advisory and never prevent enabling.</p></div>${hevcCopyCard(settings)}
      <div class="setsection"><h2>Live TV video</h2><p>Output choices whose device and node capacity evidence remains advisory.</p></div>${liveTvDeinterlaceCard(settings)}
      <div class="setsection"><h2>Recovery</h2><p>Portable backup scheduling and visible readiness. Saving is never gated by these observations.</p></div>${clusterBackupCard(settings,readiness)}
      <div class="setsection" id="enable-seek-scratch"><h2>Seek scratch accounting</h2><p>Recently shipped accounting and retention changes with unverified native-device behavior.</p></div>${seekScratchReservationsCard()}
      <div class="setsection" id="enable-auto-quality"><h2>Adaptive Auto quality</h2><p>One authoritative switch and dated qualification evidence for each client.</p></div>${autoQualityCard(settings)}
      <div class="setsection" id="enable-display-auto"><h2>Fit Auto to display</h2><p>One saved choice with advisory combined qualification evidence.</p></div>${displayAwareAutoCard(settings)}
      <div class="setsection" id="enable-quality"><h2>Prepared quality handoff</h2><p>Prepare a replacement stream using a second player. Device qualification is still incomplete.</p></div>${preparedQualityCard(settings,readiness)}${browser}
      <div class="setsection" id="enable-subtitle-refusal"><h2>Subtitle delivery</h2><p>Experimental error handling that still needs observations on each playback engine.</p></div>${subtitleNotReadyCard(settings,readiness)}
      <div class="setsection" id="enable-pgs-overlay"><h2>PGS subtitle overlay</h2><p>Serve bitmap subtitles separately from the video on capable clients.</p></div>${pgsOverlayCard(settings,readiness)}
      <div class="setsection" id="enable-subtitle-sources"><h2>Stored subtitle tracks</h2><p>Keep tracks during indexing and share verified tracks across the cluster.</p></div>${subtitlePlaybackRangesCard(readiness)}${subtitleStoredSourcesCard(settings,readiness)}${subtitleClusterSourcesCard(settings,readiness)}${subtitleBackfillCard(settings,readiness)}`;
}
// Refusing a subtitle segment whose extraction failed.
//
// A not-ready subtitle segment is answered with a valid but empty WebVTT
// body. While the sidecar is warming that is true. Once the extraction has
// failed it is a lie, and players keep the bytes in memory whatever
// `no-store` says — so the viewer is left with a track that is selected,
// silent, and never going to fill in.
//
// The switch is the enable path and the rows below never gate it. They are
// all `unobservable` on purpose: whether an engine keeps playing video
// through a subtitle 503 is a measurement on an Apple TV, an Android device
// and a browser, not something this server can read about itself.
function subtitleNotReadyCard(s,readiness){
  const enabled=!!s.subtitle_not_ready_503;
  const state=enabled
    ? `<span class="pill" style="color:var(--good);border-color:var(--good)">enabled</span>`
    : `<span class="pill">disabled</span>`;
  return setCard(`${cardHead("Refuse a subtitle segment that failed","Answer 503 with Retry-After when a subtitle track's extraction has failed, instead of an empty subtitle segment the player keeps.",state)}
      ${togRow("sub503",`Refuse instead of serving an empty subtitle segment <span class="pill warn">experimental</span>`,`Applies immediately to new segment requests. This checkbox is authoritative: an unobserved engine never turns it back off.`,enabled)}
      <div class="hint"><b>This checkbox is the enable path.</b> The observations below are advisory only. Only a <i>failed</i> extraction is refused; a track that is still warming keeps its empty segment and the client's readiness retry, because "not yet" and "not going to" are different answers.</div>
      <details class="setdetails" open><summary>What to confirm before enabling</summary><div class="setdetails-body">
      ${devReq(readiness,"subtitle_not_ready_503","avplayer_survives_subtitle_refusal","AVPlayer keeps the picture","Apple TV and iPad. AVPlayer allows a subtitle segment about two seconds and blocks the muxed video while it waits, so this is the refusal with a picture riding on it. Play a title whose subtitle extraction fails and confirm the video continues.")}
      ${devReq(readiness,"subtitle_not_ready_503","media3_survives_subtitle_refusal","Media3 keeps the picture","Android. Confirm a refused subtitle rendition surfaces as a text-track problem and not a fatal source error that stops playback.")}
      ${devReq(readiness,"subtitle_not_ready_503","hlsjs_survives_subtitle_refusal","hls.js keeps the picture","Any browser. Confirm the bundled hls.js treats a 503 with Retry-After on a subtitle rendition as recoverable rather than escalating to a fatal network error.")}
      <p class="devcheck-note">Advisory only: no result disables the switch or overrides your saved choice. These are device measurements; this server cannot take them for you, which is why all three read "not observable" rather than showing a tick nobody earned.</p>
      </div></details>
      ${devGraduation("the subtitle-reliability physical verification shows AVPlayer, Media3 and hls.js keep the picture through a refused subtitle segment.","the toggle is removed and refusing a failed extraction becomes the default, keeping an operator's explicit choice.")}<div class="err" id="sub503err" role="alert"></div>
      ${setCardFoot("saveSubtitleNotReady")}`,{id:"sub503card"});
}
function pgsOverlayCard(s,readiness){
  const enabled=!!s.pgs_overlay;
  const state=enabled
    ? `<span class="pill" style="color:var(--good);border-color:var(--good)">enabled</span>`
    : `<span class="pill">disabled</span>`;
  return setCard(`${cardHead("Serve PGS subtitles as an overlay","Give capable clients a bitmap subtitle overlay while keeping the video in its original grade.",state)}
      ${togRow("pgsoverlay",`Serve PGS subtitles as an overlay <span class="pill warn">experimental</span>`,`Applies immediately to new playback requests on every node. Only clients that advertise pgs-v1 receive an overlay.`,enabled)}
      <div class="hint"><b>This checkbox is the enable path.</b> Physical playback and seek checks are required before leaving it on. If they fail, turn it off here; no restart is needed.</div>
      <details class="setdetails" open><summary>Readiness and device qualification</summary><div class="setdetails-body">
      ${devReq(readiness,"pgs_overlay","clients_render_overlays","A client renders pgs-v1","A served manifest proves a client requested an overlay, not that it drew the cues correctly.")}
      ${devReq(readiness,"pgs_overlay","overlay_acceptance","Physical-client acceptance","Confirm cue timing, placement and seeking on Android and Apple hardware while video remains HDR or Dolby Vision.")}
      <p class="devcheck-note">These observations are advisory. The saved switch remains authoritative.</p>
      </div></details>
      ${devGraduation("the Apple PGS overlay acceptance and the Android cue timing, placement and seek checks on HDR and Dolby Vision video are recorded.","the switch moves to Settings → Playback as a permanent on/off.")}<div class="err" id="pgsoverlayerr" role="alert"></div>
      ${setCardFoot("savePgsOverlay")}`,{id:"pgsoverlaycard"});
}
// Stored PGS tracks.
//
// Both halves of the subtitle-source store, behind one switch: the
// fragment-index pass keeps every PGS track it reads, and the overlay and the
// burn path read a kept track instead of demuxing the whole source again. On
// by default. Off stops the pass keeping tracks and makes both paths ignore
// what is stored, which is how a wrong stored artifact is taken out of service
// without a redeploy. The startup self-test and cache probes are advisory
// readings for the operator; they never override the saved switch.
function subtitlePlaybackRangesCard(readiness){
  return setCard(`${cardHead("Parallel playback subtitle ranges","Prepare text near the playhead while peers prepare the next ranges.",`<span class="pill">automatic</span>`)}
      <p>For indexed Matroska text subtitles, this node prepares the current range and up to two reachable peers prepare the next ranges. Peer requests and responses are authenticated, and each completed range is served from this node's subtitle cache.</p>
      <div class="hint">Keep the subtitle cache on local storage. If peers are unavailable or refuse work, current local extraction and the existing playback fallback continue. PGS and styled subtitle burns still need complete tracks.</div>
      ${devReq(readiness,"subtitle_cluster_sources","reachable_peer","A media peer is reachable","This live directory reading is advisory; each range request still verifies its peer and source. It does not prove a successful range exchange.")}
      <p class="devcheck-note">Playback ranges are automatic. The switches below control durable stored tracks and their extraction queue; they do not enable or disable these temporary playback ranges.</p>${devGraduation("K-09's fleet evidence shows a successful peer range exchange.","this card is removed. Playback ranges have no switch.")}`);
}
function subtitleStoredSourcesCard(s,readiness){
  const enabled=s.subtitle_stored_sources!==false;
  const state=enabled
    ? `<span class="pill" style="color:var(--good);border-color:var(--good)">enabled</span>`
    : `<span class="pill">disabled</span>`;
  return setCard(`${cardHead("Keep and read stored PGS tracks","Keep each PGS track while a file is indexed, and serve the PGS overlay and burned PGS subtitles from it instead of reading the whole source again.",state)}
      ${togRow("subsrc",`Use stored PGS tracks <span class="pill">preview</span>`,`Applies immediately to the next index pass, new overlay preparations and burned sessions. This checkbox is authoritative: nothing below turns it on or off.`,enabled)}
      <div class="hint"><b>This checkbox is the enable path.</b> Off stops the index pass keeping tracks and makes both consumers ignore the store and read the source as they always have.</div>
      <details class="setdetails" open><summary>What it needs</summary><div class="setdetails-body">
      ${devReq(readiness,"subtitle_stored_sources","stored_source_producer","Something fills the store","The fragment-index pass keeps each PGS track as it reads the file, with the tracks attempted and their verdicts since this process started.")}
      ${devReq(readiness,"subtitle_stored_sources","stored_source_self_test","The startup self-test passed","At startup the configured ffmpeg runs the index argv with the tee over a tiny synthetic file with one corrupted track. Review its result before relying on stored tracks.")}
      ${devReq(readiness,"subtitle_stored_sources","stored_source_local_cache","The cache is on a local filesystem","A stage on NFS, SMB or FUSE could stall the demuxer shared with the index pass.")}
      ${devReq(readiness,"subtitle_stored_sources","stored_source_free_space","The cache has room for the stage","Keep at least 1 GiB or 2% free, whichever is larger; stages share the index cache disk.")}
      ${devReq(readiness,"subtitle_stored_sources","stored_source_lookups","Lookups answered from the store","How many overlay and burn lookups this process served from a stored track, answered as having no cues, or passed through to extraction.")}
      <p class="devcheck-note">Advisory only: no result disables the switch or overrides your saved choice.</p>
      </div></details>
      ${devGraduation("K-09's fleet and device evidence for stored tracks is recorded.","the switch moves to Settings → Maintenance beside the stored subtitle tracks, as the permanent way to take a bad stored track out of service.")}<div class="err" id="subsrcerr" role="alert"></div>
      ${setCardFoot("saveSubtitleStoredSources")}`,{id:"subsrccard"});
}
function subtitleClusterSourcesCard(s,readiness){
  const enabled=!!s.subtitle_cluster_sources;
  return setCard(`${cardHead("Share stored subtitle tracks","Join one cluster extraction and copy a verified track from a peer.",`<span class="pill${enabled?" ok":""}">${enabled?"enabled":"off"}</span>`)}
      ${togRow("subcluster",`Use cluster subtitle sources <span class="pill">preview</span>`,`Applies to new subtitle flights. Your saved choice controls use; the checks below only advise.`,enabled)}
      <details class="setdetails" open><summary>What to confirm before enabling</summary><div class="setdetails-body">
      ${devReq(readiness,"subtitle_cluster_sources","analysis_queue","Analysis queue is enabled","The cluster analysis queue must be available for cold sources.")}
      ${devReq(readiness,"subtitle_cluster_sources","schema_v47","All voters support schema 47","Deploy schema 47 on every voter before enqueuing the subtitle-source component.")}
      ${devReq(readiness,"subtitle_cluster_sources","reachable_peer","A media peer is reachable","A holder can serve verified tracks over the authenticated private media route.")}
      ${devReq(readiness,"subtitle_cluster_sources","local_cache","The subtitle store is local","Keep staging and stored tracks on a local filesystem.")}
      ${devReq(readiness,"subtitle_cluster_sources","free_space","The store has room","Leave capacity for stored tracks and a publish stage.")}
      <p class="devcheck-note">Advisory only. These observations never disable the switch or override your saved choice.</p>
      </div></details>${devGraduation("K-09's fleet evidence shows schema 47 on every voter and verified peer copies.","Paul chooses: the switch moves to Settings → Cluster, or it is removed and cluster sources become the default.")}<div class="err" id="subclustererr" role="alert"></div>${setCardFoot("saveSubtitleClusterSources")}`,{id:"subclustercard"});
}
function subtitleBackfillCard(s,readiness){
  const enabled=!!s.subtitle_backfill;
  return setCard(`${cardHead("Backfill subtitle tracks","Use otherwise idle analysis capacity to prepare uncovered subtitle tracks.",`<span class="pill${enabled?" ok":""}">${enabled?"enabled":"off"}</span>`)}
      ${togRow("subbackfill",`Backfill uncovered tracks <span class="pill">preview</span>`,`Enqueues at most eight files per discovery pass while workers are idle.`,enabled)}
      <details class="setdetails" open><summary>Current work and readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"subtitle_backfill","backfill_lease","Exclusive backfill lease","One node holds the cluster lease during each pass; no holder between passes is normal.")}
      ${devReq(readiness,"subtitle_backfill","backfill_enqueued","Files enqueued here","Count of files this process has added to the analysis queue since startup.")}
      ${devReq(readiness,"subtitle_backfill","backfill_remaining","Eligible files remaining","Files with an uncovered eligible subtitle ordinal, excluding active and cooling requests.")}
      ${devReq(readiness,"subtitle_backfill","backfill_bytes","Estimated source bytes","Source sizes for those eligible files; actual output is much smaller.")}
      <p class="devcheck-note">Advisory only. The switch remains available regardless of the reported status.</p>
      </div></details>${devGraduation("K-09's fleet evidence shows backfill passes completing under the cluster lease.","the switch moves to Settings → Analysis beside the queue it feeds, as a permanent on/off.")}<div class="err" id="subbackfillerr" role="alert"></div>${setCardFoot("saveSubtitleBackfill")}`,{id:"subbackfillcard"});
}
function hevcCopyCard(settings){
  const enabled=!!settings.hevc_unverified_copy;
  const trace=settings.hevc_header_trace_available;
  const indexing=!!settings.vod_index_cluster_cache||Number(settings.vod_index_mins)>0;
  return setCard(`${cardHead("Unverified HEVC copy","Allow a VOD copy to delete in-band parameter sets before the film's configuration is proved. Rolling and progressive copies keep them and never need this.",`<span class="pill${enabled?" warn":""}">${enabled?"Enabled":"Verified VOD only"}</span>`)}
    ${togRow("hevc-unverified","Enable unverified HEVC copy","Applies to new playback starts without a restart. Files whose parameter sets change are already copied with them kept; enabling this lets an unproved file be copied with them deleted, which can bring the pink/green corruption back.",enabled)}
    <details class="setdetails" open><summary>Requirements for safe use — advisory only</summary><div class="setdetails-body">
    ${devStaticReq("Original-header analysis",trace===true?"met":trace===false?"not met":"not observable",trace===true?"This node’s FFmpeg reports trace_headers. A complete scan of each title is still required.":"This node has not confirmed the trace_headers filter; enabling remains available.",trace===true?"ok":"warn")}
    ${devStaticReq("Background preparation",indexing?"configured":"not configured","Enable Content analysis indexing to prepare title-specific proofs. A configured worker does not mean every title has finished.",indexing?"ok":"warn")}
    ${devStaticReq("This title’s decoder configuration","not observable here","A complete scan must find one unchanged VPS/SPS/PPS configuration. A scan that finds changes means header-stripping copy can alter the decoded picture.","")}
    ${devStaticReq("Source held through playback and retries","VOD only","Verified VOD holds the source it analyzed. Rolling and progressive HEVC copies keep their in-band parameter sets, so they need no proof.","warn")}
    ${devStaticReq("Every worker updated","not observable here","Deploy the fix on all serving nodes and drain older sessions. This build uses media-worker protocol 7; a local settings page cannot certify every public ingress.","")}
    <p class="devcheck-note">These observations never disable the checkbox or reject its save. Turning the override off requires verified VOD for new HEVC copy starts; running sessions keep their existing policy.</p>
    </div></details>${devGraduation("complete scans prove every HEVC title that VOD copies. The proof-before-stripping containment itself is deployed (87ca67c0e).","Paul chooses: the override is removed because verified VOD covers every title, or it moves to Playback → Advanced server delivery as a permanent escape hatch.")}<div class="err" id="hevc-copy-error" role="alert"></div>${setCardFoot("saveHevcCopy")}`,{id:"hevc-copy-card"});
}
async function saveHevcCopy(btn){
  const err=document.getElementById("hevc-copy-error"); if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    const saved=await api("/settings",{method:"PUT",body:{hevc_unverified_copy:!!document.getElementById("hevc-unverified").checked}});
    cacheSettings(saved);
    const card=document.getElementById("hevc-copy-card"); if(card)card.outerHTML=hevcCopyCard(saved);
    toast("HEVC copy preference saved");
  }catch(error){if(err)err.textContent=error.message;if(btn)btn.disabled=false;}
}
