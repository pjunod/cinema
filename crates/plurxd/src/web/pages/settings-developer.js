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
      <div class="err" id="backup-settings-error" role="alert"></div>${setCardFoot("saveClusterBackup")}`);
}

// A readiness status is evidence, not authority. The daemon can report facts
// it can observe on this node; the controls remain available regardless of
// the answer, including when the reading itself is unavailable.
const DEV_READINESS_LABEL={met:"met",unmet:"not met",unobservable:"not observable"};
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
  if(found) return found.evidence;
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
      </div></details><div class="err" id="ltdeinterr" role="alert"></div>
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
      <div class="err" id="pqherr" role="alert"></div>
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
      </div></details>`);
}
function developerPanel(settings,readiness){
  const destinations=`<nav class="setdestinations" aria-label="Everyday settings">
    <a href="#/settings/livetv"><strong>Live TV <span aria-hidden="true">↗</span></strong><span>Tuner, guide, recording and library channels</span></a>
    <a href="#/settings/playback"><strong>Playback <span aria-hidden="true">↗</span></strong><span>Defaults, streaming and compatibility</span></a>
    <a href="#/settings/cluster"><strong>Cluster <span aria-hidden="true">↗</span></strong><span>Health and recovery</span></a>
  </nav>`;
  const browser=setCard(`${cardHead("Second player in this browser","Advertise this browser's ability to prepare a replacement stream.",`<span class="pill acc">this browser</span>`)}
      ${togRow("pdp","Allow a second player","Uses additional device memory and decoder capacity. Saved automatically in this browser; the next reporter exchange sends the change.",preparedHandoffEnabled(),'onchange="setPreparedHandoff(this.checked)"')}
      <div class="hint">A failed preparation falls back to reopening the stream. Prepare quality changes above must also be enabled on the server.</div>
      ${directedChangeDeveloperRows()}`,{local:true});
  return `${setHead("Developer","Experimental features still awaiting device qualification.")}
      ${destinations}
      <div class="setsection"><h2>Live TV video</h2><p>Output choices whose device and node capacity evidence remains advisory.</p></div>${liveTvDeinterlaceCard(settings)}
      <div class="setsection"><h2>Recovery</h2><p>Portable backup scheduling and visible readiness. Saving is never gated by these observations.</p></div>${clusterBackupCard(settings,readiness)}
      <div class="setsection" id="enable-seek-scratch"><h2>Seek scratch accounting</h2><p>Recently shipped accounting and retention changes with unverified native-device behavior.</p></div>${seekScratchReservationsCard()}
      <div class="setsection" id="enable-quality"><h2>Prepared quality handoff</h2><p>Prepare a replacement stream using a second player. Device qualification is still incomplete.</p></div>${preparedQualityCard(settings,readiness)}${browser}
      <div class="setsection" id="enable-subtitle-refusal"><h2>Subtitle delivery</h2><p>Experimental error handling that still needs observations on each playback engine.</p></div>${subtitleNotReadyCard(settings,readiness)}
      <div class="setsection" id="enable-subtitle-sources"><h2>Stored subtitle tracks</h2><p>Keep tracks during indexing and share verified tracks across the cluster.</p></div>${subtitleStoredSourcesCard(settings,readiness)}${subtitleClusterSourcesCard(settings,readiness)}${subtitleBackfillCard(settings,readiness)}
      <div class="setsection" id="enable-chapter-thumbnails"><h2>Chapter thumbnails</h2><p>A frame per chapter for the watch view's chapter rail, made the first time a page asks for it.</p></div>${chapterThumbnailsCard(settings,readiness)}
      <div class="setsection"><h2>Decoder experiments</h2><p>Recovery and cache-policy experiments. Evidence is advisory; saved choices remain authoritative.</p></div>${verifiedDecodeCard(settings)}${decodeRecoveryCard(settings)}`;
}
// Automatic decode recovery.
//
// Direct opt-in with advisory evidence. The retained diagnostic-contract
// coverage still tells an operator how much confidence to place in the
// decision, but it never disables or overrides the switch.
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
      <div class="err" id="sub503err" role="alert"></div>
      ${setCardFoot("saveSubtitleNotReady")}`,{id:"sub503card"});
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
      <div class="err" id="subsrcerr" role="alert"></div>
      ${setCardFoot("saveSubtitleStoredSources")}`,{id:"subsrccard"});
}
function subtitleClusterSourcesCard(s,readiness){
  const enabled=!!s.subtitle_cluster_sources;
  return setCard(`${cardHead("Share stored subtitle tracks","Join one cluster extraction and copy a verified track from a peer.",`<span class="pill${enabled?" ok":""}">${enabled?"enabled":"off"}</span>`)}
      ${togRow("subcluster",`Use cluster subtitle sources <span class="pill">preview</span>`,`Applies to new subtitle flights. Your saved choice controls use; the checks below only advise.`,enabled)}
      <details class="setdetails" open><summary>What to confirm before enabling</summary><div class="setdetails-body">
      ${devReq(readiness,"subtitle_cluster_sources","analysis_queue","Analysis queue is enabled","The cluster analysis queue must be available for cold sources.")}
      ${devReq(readiness,"subtitle_cluster_sources","schema_v46","All voters support schema 46","Deploy schema 46 on every voter before enqueuing the subtitle-source component.")}
      ${devReq(readiness,"subtitle_cluster_sources","reachable_peer","A media peer is reachable","A holder can serve verified tracks over the authenticated private media route.")}
      ${devReq(readiness,"subtitle_cluster_sources","local_cache","The subtitle store is local","Keep staging and stored tracks on a local filesystem.")}
      ${devReq(readiness,"subtitle_cluster_sources","free_space","The store has room","Leave capacity for stored tracks and a publish stage.")}
      <p class="devcheck-note">Advisory only. These observations never disable the switch or override your saved choice.</p>
      </div></details><div class="err" id="subclustererr" role="alert"></div>${setCardFoot("saveSubtitleClusterSources")}`,{id:"subclustercard"});
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
      </div></details><div class="err" id="subbackfillerr" role="alert"></div>${setCardFoot("saveSubtitleBackfill")}`,{id:"subbackfillcard"});
}
// Chapter thumbnails.
//
// One ffmpeg seek per chapter, on request, kept under the runtime cache.
// There is no background producer: nothing runs unless a watch page asks
// for that chapter, and the rows below say how much has run. The switch is
// the enable path; off answers the route 404 and the rail shows numbered
// tiles instead of pictures.
function chapterThumbnailsCard(s,readiness){
  const enabled=s.chapter_thumbnails!==false;
  const state=enabled
    ? `<span class="pill" style="color:var(--good);border-color:var(--good)">enabled</span>`
    : `<span class="pill">disabled</span>`;
  return setCard(`${cardHead("Make chapter thumbnails","Extract one small frame per chapter the first time the watch view asks for it, and keep it on this node.",state)}
      ${togRow("chthumb",`Make chapter thumbnails on request`,`Applies to the next thumbnail a watch page asks for. This checkbox is authoritative: nothing below turns it on or off.`,enabled)}
      <div class="hint"><b>This checkbox is the enable path.</b> Off runs no ffmpeg for thumbnails and the chapter rail shows numbered tiles; what is already cached stays on disk and is served again when the switch comes back.</div>
      <details class="setdetails" open><summary>What it needs</summary><div class="setdetails-body">
      ${devReq(readiness,"chapter_thumbnails","chapter_thumbs_ffmpeg","ffmpeg can decode a frame","The configured ffmpeg answered -version at startup. Every thumbnail is one seek into the source and one decoded frame scaled to 320 px, CPU only.")}
      ${devReq(readiness,"chapter_thumbnails","chapter_thumbs_cache_space","The runtime cache has room","Thumbnails are tens of kilobytes each and live under the runtime cache beside the other node-local stores; a full cache fails every write.")}
      ${devReq(readiness,"chapter_thumbnails","chapter_thumbs_work","What this process has extracted","Made, served from cache, failed and running now since this process started, and what the cache holds on disk. At most two extractions run at once, each bounded to 15 seconds.")}
      <p class="devcheck-note">Advisory only: no result disables the switch or overrides your saved choice.</p>
      </div></details>
      <div class="err" id="chthumberr" role="alert"></div>
      ${setCardFoot("saveChapterThumbnails")}`,{id:"chthumbcard"});
}
function decodeRecoveryCard(s){
  const q=s.decoder_health_qualification||{};
  // FFmpeg's strings again — a build banner and decoder names.
  const list=v=>`<code>${v&&v.length?v.map(esc).join(" · "):"none"}</code>`;
  const covered=q.covered_paths_v2||q.covered_decoders||[];
  const paths=covered.map(value=>{
    const fields=String(value).split("/");
    return fields.length===3?{value:String(value),codec:fields[0],backend:fields[1]}:null;
  }).filter(Boolean);
  const software=paths.filter(path=>path.backend==="software");
  const hardware=paths.filter(path=>path.backend!=="software");
  const recoverable=hardware.filter(path=>software.some(peer=>peer.codec===path.codec));
  const enabled=!!s.automatic_decoder_recovery;
  const ok=b=>b?"✓":"✗";
  const state=enabled
    ? `<span class="pill" style="color:var(--good);border-color:var(--good)">enabled</span>`
    : `<span class="pill">disabled</span>`;
  return setCard(`${cardHead("Automatic decode recovery","Retry a failed hardware decode in software while keeping the hardware encoder. Recovery increases CPU use and prevents a reopen loop.",state)}
      ${togRow("adr",`Enable automatic decoder recovery <span class="pill warn">experimental</span>`,`Applies immediately to new producer attempts. This checkbox is authoritative: missing measurements or retained contracts never turn it back off.`,enabled)}
      <div class="hint"><b>This checkbox is the enable path.</b> The checks below are advisory only. With incomplete coverage, the server uses its bounded selected-stream diagnostic matcher and never treats that best-effort observation as permission to reuse an artifact.</div>
      <details class="setdetails"><summary>Recovery limits and diagnostic evidence</summary><div class="setdetails-body">
      <h2 class="section">Safety evidence and current state</h2>
      <div class="tog"><span>${ok(!!q.measured_build)} This node measured its own FFmpeg<small>${q.measured_build?`<code>${esc(q.measured_build)}</code>`:"Not measured. Recovery can still be enabled; diagnostics use the selected input and stream without claiming build-qualified evidence."}</small></span></div>
      <div class="tog"><span>${ok(software.length>0)} A retained contract covers this build's software decoders<small>${list(software.map(path=>path.value))}<br>A covered software path increases confidence in the successor; absence is advisory and does not block the switch.</small></span></div>
      <div class="tog"><span>${ok(recoverable.length>0)} The same codec has covered hardware and software paths<small>${list(recoverable.map(path=>path.value))}<br>${recoverable.length?"Each listed hardware path has a covered software alternate for the same codec.":"No matched retained pair exists yet. Recovery remains available when explicitly enabled, using best-effort selected-stream diagnostics; this row records the missing fleet evidence."}</small></span></div>
      <div class="tog"><span>The session carries a durable recovery budget<small>Cluster sessions do. A legacy process-local start, a relayed worker start, and any session predating the recovery epoch keep the older in-process limit — one recovery per <i>session</i> rather than one per playback, which a reopen resets.</small></span></div>
      <h2 class="section">What it costs when it does fire</h2>
      <div class="hint"><b>One recovery per playback, and it is never given back.</b> The budget is durable and keyed by the playback, so a reopen, a seek or a track change does not buy another. A playback that has spent it is told its source did not decode — a permanent answer the client shows once, rather than an impermanent one it will follow forever. Two paths give a budget up without using it, both on purpose: an executor cancelled mid-install, and a node that cannot read the budget. Each costs that playback its automatic recovery and nothing else.</div>
      <div class="hint">The successor keeps the hardware encoder and its slot; what grows is the CPU the pipeline spends decoding. That difference has to be available on the node at the moment of the recovery, and a recovery that cannot get it fails rather than waiting on a viewer's behalf indefinitely.</div>
      </div></details>
      <div class="err" id="adrerr" role="alert"></div>
      ${setCardFoot("saveAutomaticDecoderRecovery")}`,{id:"drcard"});
}
// Verified decode artifacts.
//
// This control changes the identity of each covered decode path. Existing
// transcodes on that path become unreachable and are made again; uncovered
// paths do not move. Its prerequisites are node-measurable, so show them as
// advice rather than asking the operator to infer them or disabling the switch.
function verifiedDecodeCard(s){
  const q=s.decoder_health_qualification||{};
  const policyEnabled=q.policy_enabled===undefined?!!q.enforcing:!!q.policy_enabled;
  const measuredPaths=q.measured_paths_v2||q.measured_decoders||[];
  const coveredPaths=q.covered_paths_v2||q.covered_decoders||[];
  const pill=q.pending_restart
    ? `<span class="pill warn">Saved · restart to apply</span>`
    : (policyEnabled
        ? `<span class="pill">Enabled · covered paths</span>`
        : (s.decoder_health_qualified_artifacts
            ? `<span class="pill warn">Saved · restart to apply</span>`
            : `<span class="pill">Off</span>`));
  // Everything below comes from FFmpeg — a version banner and decoder names —
  // so it is somebody else's string in this page's markup.
  const list=v=>`<code>${v&&v.length?v.map(esc).join(" · "):"none"}</code>`;
  const ok=b=>b?"✓":"✗";
  const checks=`
      <h2 class="section">What this node measured</h2>
      <div class="tog"><span>${ok(!!q.measured_build)} This node measured its own FFmpeg<small>${q.measured_build?`<code>${esc(q.measured_build)}</code>`:"Not measured. Nothing this build prints can be matched to a contract, so nothing it prints is evidence."}</small></span></div>
      <div class="tog"><span>${ok(measuredPaths.length>0)} The startup probe named the decode paths this build selects<small>${list(measuredPaths)}<br>Current responses use <code>codec/backend/decoder</code>; older nodes report software-only <code>codec/decoder</code> pairs. Hardware appears only when FFmpeg positively confirms it used the requested backend. FFmpeg can drop every frame of a file and still exit successfully, so a clean exit is not evidence.</small></span></div>
      <div class="tog"><span>${ok(coveredPaths.length>0)} A retained diagnostic contract covers this build<small>${list(coveredPaths)}<br>Covered means covered as this node runs it: the same binary, backend, decoder, and qualified log flags. A contract qualified under different flags or a different backend describes a different log.</small></span></div>`;
  const refusal=q.explanation
    ? `<div class="hint"><b>Coverage advisory:</b> ${esc(q.explanation)}</div>`
    : "";
  // Conditional, because a node with no covered path pays no cache rename yet.
  // The setting is still enabled; this is a cost projection, not a gate.
  const cost=q.eligible
    ? `<div class="hint"><b>This node has covered decode paths, so enabling the policy renames cached transcodes that use those paths.</b> A covered path's identity includes whether its producer health was verified, so its existing cache becomes unreachable and affected titles are transcoded again on next demand. Turning the policy off later makes those paths pay the same rename a second time. Nothing is deleted; old generations age out of the cache like any other unused generation. Newly covered paths rotate when their contracts arrive. Apply it when the node is not busy.</div>`
    : `<div class="hint"><b>No measured path can produce a verified artifact today.</b> You may still enable the policy; it remains advisory and uncovered paths keep their current cache identity. As contracts are added, each newly covered path begins using the verified identity and pays its cache rename then.</div>`;
  const applies=q.pending_restart
    ? `<div class="hint"><b>Saved, and not yet in force.</b> This node applies the request when it next starts. Qualification is part of each covered path's cache key, so applying it while sessions are running could move an affected key space under work already in flight.</div>`
    : `<div class="hint">The request takes effect when this node next starts and is stored regardless of readiness. Each exact decode path uses it only when that path has one unique covering contract; uncovered or ambiguous paths keep serving under their present identity.</div>`;
  return setCard(`${cardHead("Verified decode artifacts","Require evidence of a clean decode before reusing transcodes on covered paths.",pill)}
      ${togRow("dhqa",`Require a producer health receipt <span class="pill">preview</span>`,"Enables the path-scoped verified policy on the next start. Readiness checks are advisory and never disable this control.",s.decoder_health_qualified_artifacts)}
      <div class="hint">Applies after a server restart. Covered paths use a new cache identity, so affected titles may need transcoding again.</div>
      <details class="setdetails"><summary>Cache impact and diagnostic evidence</summary><div class="setdetails-body">
      ${cost}${checks}${refusal}${applies}
      </div></details>
      <div class="err" id="dhqerr" role="alert"></div>
      ${setCardFoot("saveVerifiedDecode")}`,{id:"vdcard"});
}
