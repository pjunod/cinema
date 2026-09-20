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
function windowsServerCard(settings,readiness){
  return setCard(`${cardHead("Windows server","Native runtime, media engine and measured hardware status.",`<span class="pill">advisory</span>`)}
      ${togRow("dvwin","Enable permanent Dolby Vision conversion","Allows Profile 7 media to be converted to a verified Profile 8.1 replacement on Windows. This saved choice is authoritative; readiness below is advice only.",settings.dolby_vision_convert)}
      <div class="hint">These readings never enable or disable a feature. A failed or absent hardware probe keeps the ordinary software encoder available.</div>
      <details class="setdetails" open><summary>Runtime and hardware readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"windows_server","native_runtime","Native Windows runtime","The Windows build requires x64 Windows 10 1809 or Server 2019+, NTFS/ReFS managed storage, Job Objects and the embedded long-path manifest.")}
      ${devReq(readiness,"windows_server","ffmpeg_runtime","FFmpeg runtime","Point PLURX_FFMPEG and PLURX_FFPROBE at the packaged jellyfin-ffmpeg build. The exact binaries are measured at startup.")}
      ${devReq(readiness,"windows_server","nvenc","NVIDIA NVENC","NVENC is admitted only when the installed driver and this FFmpeg pass the ordinary startup and forced-IDR probes.")}
      ${devReq(readiness,"windows_server","qsv","Intel Quick Sync","Quick Sync is admitted only when D3D11 device initialization, encode and forced-IDR probes succeed on this node.")}
      <p class="devcheck-note">Advisory only. Unmet hardware rows mean software fallback, not a hidden gate.</p>
      </div></details>
      <div class="err" id="winerr" role="alert"></div>${setCardFoot("saveWindowsCompatibility")}`);
}
function preparedQualityCard(settings,readiness){
  return setCard(`${cardHead("Quality switching","Prepare the next stream before replacing the one you are watching.",`<span class="pill" id="pqhstate">${settings.prepared_quality_handoff?"Enabled":"Disabled"}</span>`)}
      ${togRow("pqh",`Prepare quality changes <span class="pill warn">experimental</span>`,`Uses a second player and temporary server capacity to reduce interruptions. Applies after saving to the next eligible quality change.`,settings.prepared_quality_handoff)}
      <div class="hint">Still experimental. This saved switch is authoritative; readiness is advisory and never overrides your choice. The browser-specific capability control is below.</div>
      <details class="setdetails"><summary>Readiness and device qualification</summary><div class="setdetails-body">
      <p class="hint">These readings describe this server. Missing evidence does not disable the setting.</p>
      <h3 class="devcheck-group">Server readiness</h3>
      ${devStaticReq("Control protocol",settings.playback_control_protocol_v1?"met":"not met","Enable protocol advertisement in Developer for new sessions. An existing session without the control block cannot prepare a replacement.",settings.playback_control_protocol_v1?"ok":"warn")}
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
// Recording: one authoritative switch, the settings the engine reads, and six
// readings that are advice and nothing else. Two of the six are legitimately
// unobservable on a healthy server — whether every node mounts the root is a
// fact about a cluster, and whether the root is writable is a fact about the
// owner's filesystem — which is exactly why none of them may gate the switch.
function dvrCard(settings,readiness){
  return setCard(`${cardHead("Recording","Capture a programme from the tuner to a disk, and let the library scan pick it up.",settings.dvr_enabled?`<span class="pill ok">enabled</span>`:`<span class="pill">disabled</span>`)}
      ${togRow("dvrenabled","Enable recording","This checkbox is authoritative. Reminders, rules and the schedule stay readable while it is off, so a deployment can be inspected and repaired before the first capture.",settings.dvr_enabled)}
      <div class="setfields">
        <div><label for="dvrroot">Recording folder</label><input id="dvrroot" value="${esc(settings.dvr_root||"")}" placeholder="/srv/plurx/recordings"><div class="hint">The tuner owner writes here, and every node that serves a recording has to be able to read it.</div></div>
        <div><label for="dvrfloor">Keep free (GB)</label><input id="dvrfloor" type="number" min="0" max="1000000" value="${esc(String(settings.dvr_free_floor_gb??50))}"><div class="hint">A capture never starts under the floor; it shows the conflict instead of filling the disk.</div></div>
        <div><label for="dvrreserve">Tuners reserved for viewing</label><select id="dvrreserve">${[0,1,2,3,4].map(n=>`<option value="${n}"${n===Number(settings.dvr_tuner_reserve??1)?" selected":""}>${n}</option>`).join("")}</select><div class="hint">Recordings never take the last reserved session, so a schedule cannot lock the household out of the tuner.</div></div>
        <div><label for="dvrpadstart">Start early (seconds)</label><input id="dvrpadstart" type="number" min="0" max="3600" value="${esc(String(settings.dvr_pad_start_s??60))}"></div>
        <div><label for="dvrpadend">Run late (seconds)</label><input id="dvrpadend" type="number" min="0" max="3600" value="${esc(String(settings.dvr_pad_end_s??120))}"></div>
        <div><label for="dvrlead">Reminder lead (seconds)</label><input id="dvrlead" type="number" min="0" max="3600" value="${esc(String(settings.dvr_reminder_lead_s??300))}"><div class="hint">How long before a programme starts a reminder appears. “Watch at” sets one with no lead at all.</div></div>
        <div><label for="dvrwebhook">Webhook URL</label><input id="dvrwebhook" value="${esc(settings.dvr_webhook_url||"")}" placeholder="https://example.invalid/hook"><div class="hint">Optional. https anywhere, or plain http only to a private address.</div></div>
      </div>
      <details class="setdetails" open><summary>Requirements and readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"dvr","dvr_root_writable","Writable recording folder","The tuner owner creates and removes a probe file under the folder. A read-only remount, a full filesystem and an ACL that says yes but means no all look identical to a permission bit.")}
      ${devReq(readiness,"dvr","dvr_free_space","Room above the floor","Free space on the recording folder against the floor set above. Read this row on the tuner owner; another node cannot see that filesystem.")}
      ${devReq(readiness,"dvr","guide_horizon","A guide worth scheduling from","A four-hour guide can only record what is nearly on. Series rules, a schedule and a conflict worth resolving all need a horizon.")}
      ${devReq(readiness,"dvr","tuner_reserve","A tuner stays free for watching","Reserving every session for viewing means no capture could ever start; reserving none means a schedule can take the last tuner.")}
      ${devReq(readiness,"dvr","every_node_mounts_root","Every node can read the folder","The owner writes recordings and any node may serve them, so the folder has to be a mount every node has. This process can see its own filesystem and no peer's.")}
      ${settings.dvr_webhook_url?devReq(readiness,"dvr","webhook_url_approved","An approved webhook URL","The outbound policy allows https anywhere and plain http only to a private address. An unapproved URL is never called."):""}
      <p class="devcheck-note">Advisory only: no result disables the switch or overrides your saved choice. An operator who can see the deployment may turn recording on over any amount of red.</p>
      </div></details>
      <div class="err" id="dvrerr" role="alert"></div>${setCardFoot("saveDvrDeveloper")}`);
}
// The playback surface contract, as a readiness card.
//
// Advisory in the strongest sense: there is no switch here, no save, and
// nothing anywhere in the product reads these answers. The contract itself
// says so (PLAYBACK-SURFACE-CONTRACT.md §5: "Nothing here is gated"), and the
// point of the card is the opposite of a gate — the migration is merged and
// shipping, and this is the honest list of what has NOT been checked about
// it, in the one place an operator already looks for that kind of fact.
//
// Every row is a static sentence. The browser cannot read a client build
// number, a repository file or a device log, so a computed-looking pill here
// would be a computed pill that lies. Each row names the file to check
// instead.
//
// That is also the cost: nothing checks these numbers. `validation/doc_versions.py`
// sweeps the two client READMEs, `APPLE-CLIENT-PARITY.md` and `docs/STATUS.html`
// for build claims and has never read this file, so both client rows went stale
// the moment a client shipped again — the Apple row still said build 150 and a
// 532/518 run two builds after both had moved. Whoever moves a build number
// moves the sentence here too, until something sweeps it.
function playbackSurfaceReadinessCard(){
  return setCard(`${cardHead("Playback surface contract",
      "What the error overlay is allowed to be, on every client. Merged and shipping; this is what has not been verified about it.",
      `<span class="pill">advisory</span>`)}
      <div class="hint">Nothing on this card is a switch. No result here disables a control, hides one, or changes what any client does.</div>
      <details class="setdetails"><summary>Verification still owed</summary><div class="setdetails-body">
      <p class="hint">These readings are about the source tree and the devices, neither of which this server can see. Each row names the file that carries the answer.</p>
      <h3 class="devcheck-group">Client builds</h3>
      ${devStaticReq("Apple client build and unit suite","verified",
        "Both schemes compile and the whole suite passes on both destinations &mdash; iPhone 17 Pro (iOS 26.5), 549 tests, and Apple TV 4K 3rd generation (tvOS 26.5), 535 tests, zero failures on each, under Xcode 26.6. <code>testPlaybackSurfaceModelRunsEveryContractCase</code> ran and passed under each. <code>clients/apple/README.md</code> claims build 151, above the 150 that carried the first compile. Getting there found one compile error, three wrong tests and two shipped bugs &mdash; a 401 during a quality change demoted its own Sign in terminal to a &ldquo;Playback recovered&rdquo; banner, and <code>waitingToPlayAtSpecifiedRate</code> drove a second spinner of its own, so the <code>buffering</code> class could never be drawn. This row is about the unit suite only &mdash; &sect;6's eight simulator and device runs are still owed, and are counted by the rows below.","ok")}
      ${devStaticReq("Android client build and unit suite","JVM green, hardware owed",
        "Compiled and run on 2026-09-13 against <code>clients/android/README.md</code>'s claimed build 93: <code>:app:compileDebugKotlin</code>, <code>:app:testDebugUnitTest</code> (634 tests, 0 failures), <code>:app:lintDebug</code> and <code>:app:assembleDebug</code>, on the pinned Docker image and on a local SDK, with the same answer from each. Not met because <code>docs/clients/PLAYBACK-SURFACE-ANDROID-BUILD-PROMPT.md</code> §5 is still owed — the instrumented suite and the twelve device recipes have never been run.","")}      <h3 class="devcheck-group">Devices</h3>
      ${devStaticReq("Physical verification of the four recipes","not run",
        "The network drop, the failed change on a cold NAS, the Android remux seek and the iPhone lock screen — implementation plan §7, on an Apple TV, an iPhone and an Android TV. Met when a <code>docs/clients/PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-&lt;date&gt;.md</code> record exists; the script is <code>docs/clients/PLAYBACK-SURFACE-PHYSICAL-VERIFICATION-PROMPT.md</code>.","warn")}
      ${devStaticReq("surface_disagreement","none observed",
        "A disagreement row is a place where something started the player after its owner stopped it. None has been observed because no device run has happened yet — this is an absence of looking, not an absence of rows. One exception is documented: the iOS lock-screen play command. Read them in Playback debug on the client, section SURFACE, or from the client log.","")}
      <h3 class="devcheck-group">Gated on a measurement</h3>
      ${devStaticReq("Remux-origin measurement (M6)","not taken",
        "The Android progressive-remux landing is built only if a measurement says so: the achieved origin must differ from the requested start by more than 250 ms and the first frame must land at the origin. Nobody has measured it. The procedure is <code>docs/clients/PLAYBACK-SURFACE-REMUX-ORIGIN-MEASUREMENT-PROMPT.md</code>.","")}
      <p class="devcheck-note">Advisory only. The contract is merged and every client already renders through it; these rows say what has not been checked, and no answer here enables, disables or overrides anything.</p>
      </div></details>`);
}
function webHlsStartupRecoveryCard(){
  const loader=!!(window.Hls&&Hls.DefaultConfig&&Hls.DefaultConfig.loader);
  return setCard(`${cardHead("Web HLS startup recovery",
      "Recover a manifest that failed before hls.js could establish the stream.",
      `<span class="pill ok">automatic</span>`)}
      <div class="hint">Recovery is enabled for every hls.js attachment. These checks explain whether this browser can use the complete recovery path; they never enable, disable, or hide it.</div>
      <details class="setdetails" open><summary>Requirements and readiness</summary><div class="setdetails-body">
      ${devStaticReq("Final-send loader interception",loader?"met":"unavailable",
        "The vendored hls.js loader must expose its stock loader so plurx can count actual manifest sends, stop dispatch at the shared ceiling, and read typed refusal bodies before the library retries.",loader?"ok":"warn")}
      ${devStaticReq("Bounded startup policy","met",
        "Manifest first-byte, load, retry, application-reload, dispatch-ceiling, and absolute startup deadlines are configured from one browser policy. Fragment delivery keeps its existing policy.","ok")}
      ${devStaticReq("Truthful init inspection","met",
        "The server holds playlist publication to its existing five-second bound and reports pending, invalid, unsupported, or unreadable initialization media as distinct typed responses.","ok")}
      ${devStaticReq("Presentation evidence","met",
        "Recovery completes only after the media clock advances and, when available, the presented-frame counter advances. A playing event or parsed manifest is not success.","ok")}
      <p class="devcheck-note">Advisory only. Native-HLS playback continues to use the platform player; this card describes the hls.js/MSE path.</p>
      </div></details>`);
}
function hevcSampleEntryAdmissionCard(){
  const caps=currentCapsDocument();
  const entries=caps.progressive_hevc_sample_entries;
  const constrained=Array.isArray(entries);
  const hls=(caps.transports||[]).includes("hls");
  return setCard(`${cardHead("HEVC sample-entry admission",
      "Keep incompatible progressive HEVC packaging on a safe copy-HLS route.",
      `<span class="pill ok">automatic</span>`)}
      <div class="hint"><b>No feature flag is used.</b> Enforcement is active whenever a client sends the additive packaging claim. These checks are rollout advice only and never disable playback policy.</div>
      <details class="setdetails" open><summary>Safe enablement and rollout readiness</summary><div class="setdetails-body">
      ${devStaticReq("This browser's progressive claim",constrained?`settled · ${entries.length?entries.join(", "):"none"}`:"not reported",
        "Capability discovery must finish before playback. Each listed label is proven on the progressive file path across every advertised HEVC tier; MSE evidence cannot widen it.",constrained?"ok":"warn")}
      ${devStaticReq("HLS fallback transport",hls?"met":"not claimed",
        "A copy whose actual progressive label is not admitted requires an explicitly claimed HLS transport. Session creation refuses before allocation when no safe transport exists.",hls?"ok":"warn")}
      ${devStaticReq("Serving-fleet order","operator confirmation required",
        "Upgrade every serving node before publishing refreshed web or Apple clients. A pre-repair v2 server may silently ignore the additive field.","warn")}
      ${devStaticReq("Stored source-tag recovery","automatic · observe completion",
        "The bounded catalogue backfill reads stored probe JSON only, under its own lease. Confirm its completion receipt before treating unknown-tag remux volume as steady state.","")}
      ${devStaticReq("Physical browser and Apple evidence","not recorded by this page",
        "Record Safari, Chrome, iOS and tvOS observations separately. Missing device evidence never turns into permission to broaden a claim.","warn")}
      <p class="devcheck-note">Advisory only. Unmet or unknown rows do not gate the feature, rewrite capability claims, or override the server's typed refusal.</p>
      </div></details>`);
}
function contentAnalysisEnableCard(settings,readiness){
  return setCard(`${cardHead("Selected-video content analysis","Build durable VOD indexes against the exact video stream FFmpeg maps.",settings.vod_index_cluster_cache?`<span class="pill ok">enabled</span>`:`<span class="pill">disabled</span>`)}
      ${togRow("ca-enabled","Enable durable content analysis","This saved choice is authoritative. The checks below explain safe rollout conditions and never disable or override it.",settings.vod_index_cluster_cache)}
      <details class="setdetails" open><summary>Safe enablement and current readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"content_analysis_repair","media_tools","Measured media tools","FFmpeg builds the video-only index and FFprobe resolves timing for exactly the mapped stream.")}
      ${devReq(readiness,"content_analysis_repair","durable_queue","Durable analysis authority","The queue must persist typed failures, retry deadlines and bounded diagnostics across restarts and nodes.")}
      ${devReq(readiness,"content_analysis_repair","source_fencing","Held-source fencing","The metadata expectation and long index pass must describe the same attested source object.")}
      ${devReq(readiness,"content_analysis_repair","compatibility_inventory","Successful-file timing sample","Review a bounded compatibility sample before fleet rollout so valid files without selected-stream timing are visible.")}
      <p class="devcheck-note">Advisory only. Unmet, unavailable or unobservable rows do not gate this switch. Existing successful indexes remain usable while the queue is paused.</p>
      </div></details><div class="err" id="caerr" role="alert"></div>${setCardFoot("saveContentAnalysisDeveloper")}`);
}
function uiEnableAdvisory(){
  const browser=typeof CSS!=='undefined'&&CSS.supports('display','grid')&&typeof fetch==='function';
  const libs=SETTINGS_DATA&&SETTINGS_DATA.libs;
  const row=(title,state,detail)=>`<div><strong>${esc(title)} · ${esc(state)}</strong><small>${esc(detail)}</small></div>`;
  return `<section class="card" id="ui-enable"><h2>Enable</h2><p>The updated search and activity layouts are enabled in this build. They need no rollout flag.</p><div class="enable-advisory">${row('Browser support',browser?'Met':'Not confirmed','Responsive layout and API support. This observation is advisory.')}${row('Signed-in account',ME?'Met':'Not confirmed','Watch state and recording visibility use the signed-in account.')}${row('Library data',Array.isArray(libs)?libs.length?'Available':'No libraries configured':'Not loaded','Readable files and accurate metadata make continuation and preview useful. Missing data is shown in context.')}${row('Playback and recording','Your saved choices','Use the controls below to enable recovery, recording, channel playback or quality handoff. Readiness results never override your choice.')}</div><p class="hint">Advisory only: unmet or unknown checks do not hide features or disable enable switches. Existing authorization, file availability and operational safety rules still apply.</p><nav class="watch-browse" aria-label="Enable controls"><a href="#/settings/developer" onclick="event.preventDefault();document.getElementById('enable-live-hls')?.scrollIntoView({behavior:'smooth',block:'start'})">Live HLS ↓</a><a href="#/settings/developer" onclick="event.preventDefault();document.getElementById('enable-analysis')?.scrollIntoView({behavior:'smooth',block:'start'})">Content analysis ↓</a><a href="#/settings/developer" onclick="event.preventDefault();document.getElementById('enable-recording')?.scrollIntoView({behavior:'smooth',block:'start'})">Recording ↓</a><a href="#/settings/developer" onclick="event.preventDefault();document.getElementById('enable-channels')?.scrollIntoView({behavior:'smooth',block:'start'})">Library channels ↓</a><a href="#/settings/developer" onclick="event.preventDefault();document.getElementById('enable-quality')?.scrollIntoView({behavior:'smooth',block:'start'})">Quality handoff ↓</a><a href="#/settings/developer" onclick="event.preventDefault();document.getElementById('enable-hevc-sample-entry')?.scrollIntoView({behavior:'smooth',block:'start'})">HEVC admission ↓</a><a href="#/settings/developer" onclick="event.preventDefault();document.getElementById('enable-source-probe')?.scrollIntoView({behavior:'smooth',block:'start'})">Source verification ↓</a></nav></section>`;
}
// Source verification compatibility.
//
// Starting an encoded session re-probes the held source and compares it with
// the scan in the catalog. Those two reports can come from different FFprobe
// builds, and a newer build derives properties an older one never reported —
// an added description is not a replaced movie. The comparator admits the two
// measured Dolby Atmos profile omissions and nothing else. There is no switch
// here and there is no flag in the code: the rules are always active, and
// these rows say what is known about them on this node.
function sourceProbeCompatibilityCard(readiness){
  const sys=(SETTINGS_DATA&&SETTINGS_DATA.sys)||{};
  const tools=sys.ffmpeg_version?`reported`:"not reported";
  return setCard(`${cardHead("Source probe compatibility",
      "Let a newer media tool describe an older scan without calling it a replaced file.",
      `<span class="pill ok">automatic</span>`)}
      <div class="hint"><b>No feature flag is used.</b> Nothing on this card enables, disables, hides or overrides playback admission. Held-source, storage-object, recipe and generation fences are unchanged.</div>
      <details class="setdetails" open><summary>Safe enablement and readiness</summary><div class="setdetails-body">
      ${devStaticReq("Derived Atmos profile omissions","met",
        "A scan that omits <code>profile</code> on a TrueHD or E-AC-3 track is accepted against a report that carries exactly <code>Dolby TrueHD + Dolby Atmos</code> or <code>Dolby Digital Plus + Dolby Atmos</code>, in either direction, on a stream with the same explicit index. Two reported profiles are always compared, so a real profile change still refuses.","ok")}
      ${devStaticReq("Media tools on this node",tools,
        "Compatibility is only needed because the scanning engine and the playing engine can differ. Keeping every node on the same media tools removes most of the need for it; it does not repair metadata an older engine already wrote.",sys.ffmpeg_version?"ok":"warn")}
      ${devReq(readiness,"source_probe_comparison","probe_reporter_named","Scan provenance",
        "A probe document records the FFprobe build that wrote it. Two documents from the same build are compared whole; only a proved difference in build narrows the comparison, and a probe that cannot name itself proves nothing and stays on the whole-document comparison.")}
      ${devReq(readiness,"source_probe_comparison","sources_match_their_scan_whole","Held sources matching their scan whole",
        "An unmet row means this node has served sources admitted on the narrower comparison because their scan came from an older media tool. Reanalyze under More actions on the item rewrites that scan with this build and restores the stricter comparison for it.")}
      ${devStaticReq("Refusal diagnostics","met",
        "A refused comparison writes the normalized field paths that disagreed to the server log, bounded to eight paths, with no values, pathnames or container tag text. The client answer is unchanged.","ok")}
      ${devStaticReq("Compared media facts","built",
        "When the two documents came from different builds, the comparison is a declared projection of what the catalogue derives and the recipe reads: geometry, cadence, codec identity and tag, parameter-set layout, colour, every side-data record, the audio shape, track language and disposition, container size and format, and chapter timing. Every one of those is compared strictly, including when only one document reports it; a container duration is compared within a relative tolerance because two builds estimate it differently.","ok")}
      <p class="devcheck-note">Advisory only. An unmet row here means a repair is worth doing, never that playback is blocked. A file whose scan genuinely is stale is repaired with Reanalyze under More actions on the item.</p>
      </div></details>`);
}
function systemAttentionHtml(sys){
  const storage=sys.storage||{}, mounts=(storage.mounts||[]).filter(m=>m.read_bps==null||m.cache_suspect);
  const messages=mounts.map(m=>`<p><b>${esc((m.roots||[]).join(', ')||'Storage location')}</b><br>${esc(m.note||'No current read measurement is available.')} ${storage.measured_at?`<span class="muted">Observed ${fmtAgo(storage.measured_at)}.</span>`:''}</p>`);
  if(!sys.ffmpeg_version)messages.push('<p><b>Media tools unavailable.</b> Scanning and transcoding need the configured FFmpeg executable.</p>');
  if(sys.tone_map&&!sys.tone_map.ran&&sys.tone_map.verdicts?.some(v=>v.rejected))messages.push(`<p><b>HDR processing has unverified capabilities.</b> ${esc(sys.tone_map.verdicts.find(v=>v.rejected)?.rejected||'See the capability details below.')}</p>`);
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
function liveHlsRecoveryCard(settings,readiness){
  const enabled=!!settings.vod_live_recovery;
  return setCard(`${cardHead("Sliding Live HLS recovery",
      "Use the bounded growing presentation when immutable VOD cannot serve a title yet.",
      enabled?`<span class="pill ok">enabled</span>`:`<span class="pill">disabled</span>`)}
      ${togRow("dvlr","Enable sliding Live HLS recovery",
        "Applies to new sessions. This checkbox is authoritative; the readiness rows below never disable it or turn it back off.",enabled)}
      <div class="hint"><b>This checkbox is the enable path.</b> The engine uses a fixed 16-second playlist target, scheduled publication, a bounded 180-second served window, and object grace for URLs already promised to a player. Immutable VOD remains preferred.</div>
      <details class="setdetails" open><summary>Safe enablement and current state</summary><div class="setdetails-body">
      ${devReq(readiness,"live_hls_recovery","rolling_contract_built","Bounded sliding contract","Target, scheduler, served window and object grace must be present in this build.")}
      ${devReq(readiness,"live_hls_recovery","vod_coverage_replaces_it","Why recovery is still needed","Recent VOD refusals show which viewers would lose playback with recovery disabled.")}
      ${devReq(readiness,"live_hls_recovery","no_session_bypasses_the_switch","Peer takeover path","A relay takeover can require the existing rolling presentation independently of this fallback choice.")}
      ${devReq(readiness,"live_hls_recovery","rolling_clients_qualified","Physical-client qualification","Apple native HLS, Media3 and hls.js should be checked for startup, long playback, pause and resume.")}
      <p class="devcheck-note">Advisory only. Unmet, unavailable or unobservable rows do not gate this switch. Authorization, exact object ownership and scratch limits still protect each request.</p>
      </div></details><div class="err" id="dvlrerr" role="alert"></div>${setCardFoot("saveLiveHlsRecoveryDeveloper")}`);
}
function developerPanel(settings,readiness){
  const destinations=`<nav class="setdestinations" aria-label="Everyday settings">
    <a href="#/settings/livetv"><strong>Live TV <span aria-hidden="true">↗</span></strong><span>Tuner and guide</span></a>
    <a href="#/settings/playback"><strong>Playback <span aria-hidden="true">↗</span></strong><span>Quality and defaults</span></a>
    <a href="#/settings/cluster"><strong>Cluster <span aria-hidden="true">↗</span></strong><span>Health and recovery</span></a>
  </nav>`;
  const protocol=setCard(`${cardHead("Playback control protocol","Allow compatible clients to report playback and request stream changes.",`<span class="pill">compatibility</span>`)}
      ${togRow("pcpv1","Advertise playback control protocol v1","Applies to new sessions. Quality switching needs this enabled.",settings.playback_control_protocol_v1)}
      <details class="setdetails"><summary>Client readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"playback_control_protocol_v1","clients_report","Client reporters","Only clients observed by this server can be reported here; silent clients remain unknown.")}
      </div></details>
      <div class="err" id="dverr" role="alert"></div>${setCardFoot("saveDeveloper")}`);
  const browser=setCard(`${cardHead("Second player in this browser","Advertise this browser's ability to prepare a replacement stream.",`<span class="pill acc">this browser</span>`)}
      ${togRow("pdp","Allow a second player","Uses additional device memory and decoder capacity. Saved automatically in this browser; the next reporter exchange sends the change.",preparedHandoffEnabled(),'onchange="setPreparedHandoff(this.checked)"')}
      <div class="hint">A failed preparation falls back to reopening the stream. <a href="#/settings/playback">Quality switching</a> must also be enabled on the server.</div>
      ${directedChangeDeveloperRows()}`,{local:true});
  const libraryChannels=setCard(`${cardHead("Library channels","Build always-on schedules from probed movies and episodes. The feature is compiled into every ordinary server build.",settings.library_channels_enabled?`<span class="pill ok">enabled</span>`:`<span class="pill">disabled</span>`)}
      ${togRow("lcsubjectenabled","Enable subject matching","Runs local catalogue rules. Saving and existing playback remain available when paused.",settings.library_channel_subject_matching_enabled!==false)}
      ${devReq(readiness,"library_channel_subject_matching","provider","Local catalogue matcher","Text, metadata and classification rules run inside plurx without an inference service.")}
      ${devReq(readiness,"library_channel_subject_matching","metadata","Metadata coverage","Missing or vague descriptions can remain uncertain and can be included manually.")}
      ${devReq(readiness,"library_channel_subject_matching","batch","Recent batch and queued work","Operational failures never become negative classifications.")}
      ${togRow("lcenabled","Enable Library-channel playback","This checkbox is authoritative. Listing, preview and authoring stay available while off so readiness can be inspected and repaired.",settings.library_channels_enabled)}
      <details class="setdetails" open><summary>Requirements and readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"library_channels","authoritative_store","Authoritative Store","Definitions, schedules and favourites must come from current authority rather than a stale node-local copy.")}
      ${devReq(readiness,"library_channels","eligible_video","Schedulable video","A successful probe, video stream and positive duration are needed to publish a non-empty rotation. A draft may still be saved without one.")}
      ${devReq(readiness,"library_channels","compatible_clients","Compatible clients","Old clients retain VOD and Live TV, but do not know the dedicated channel-session purpose or boundary controller.")}
      <p class="devcheck-note">Advisory only: no result disables the switch or overrides your saved choice. No tuner, metadata service, AI key, or external provider is required.</p>
      </div></details><div class="err" id="lcdeverr" role="alert"></div>${setCardFoot("saveLibraryChannelsDeveloper")}`);
  return `${setHead("Developer","Compatibility controls and device diagnostics.")}
      ${destinations}${uiEnableAdvisory()}
      <div class="setsection" id="enable-live-hls"><h2>Sliding Live HLS</h2><p>Explicit recovery enablement with advisory build, usage and device evidence.</p></div>${liveHlsRecoveryCard(settings,readiness)}
      <div class="setsection" id="enable-analysis"><h2>Content analysis</h2><p>Explicit enablement for durable selected-video indexing with advisory rollout evidence.</p></div>${contentAnalysisEnableCard(settings,readiness)}
      <div class="setsection" id="enable-recording"><h2>Recording</h2><p>Explicit enablement for capturing the tuner to a disk, with advisory deployment facts.</p></div>${dvrCard(settings,readiness)}
      <div class="setsection" id="enable-channels"><h2>Library channels</h2><p>Explicit playback enablement with advisory deployment facts.</p></div>${libraryChannels}
      <div class="setsection" id="enable-quality"><h2>Prepared quality handoff</h2><p>Explicit server and browser enablement with advisory safety evidence.</p></div>${preparedQualityCard(settings,readiness)}${browser}
      <div class="setsection"><h2>Playback surface contract</h2><p>Advisory verification status for the one fault contract every client renders. Nothing here is a switch.</p></div>${playbackSurfaceReadinessCard()}
      <div class="setsection"><h2>Web HLS startup recovery</h2><p>Always-on manifest recovery with advisory browser and server readiness.</p></div>${webHlsStartupRecoveryCard()}
      <div class="setsection" id="enable-hevc-sample-entry"><h2>HEVC sample-entry admission</h2><p>Always-on packaging admission with advisory deployment and evidence status.</p></div>${hevcSampleEntryAdmissionCard()}
      <div class="setsection" id="enable-source-probe"><h2>Source verification</h2><p>Always-on probe-compatibility rules with advisory provenance and diagnostics status.</p></div>${sourceProbeCompatibilityCard(readiness)}
      <div class="setsection"><h2>Compatibility</h2><p>Native server and protocol controls for compatible playback clients.</p></div>${windowsServerCard(settings,readiness)}${protocol}
      <div class="setsection" id="enable-subtitle-refusal"><h2>Subtitle delivery</h2><p>Explicit enablement for refusing a subtitle segment this server cannot produce, with advisory per-engine observations.</p></div>${subtitleNotReadyCard(settings,readiness)}
      <div class="setsection"><h2>Decoder experiments</h2><p>Recovery and cache-policy experiments. Evidence is advisory; saved choices remain authoritative.</p></div>${verifiedDecodeCard(settings)}${decodeRecoveryCard(settings)}<div class="card"><h2>Search and classification</h2><p>Local text search, channel rules and automatic metadata labels are always available. Add optional search by meaning using an embedded model.</p>${devReq(readiness,"embedded_semantic_search","runtime","Embedded CPU runtime","Included in ordinary builds; no inference service is required.")}${devReq(readiness,"embedded_semantic_search","model","Verified model loaded","Enable to download approximately 91 MB once per node.")}${devReq(readiness,"embedded_semantic_search","index","Local semantic index","Indexing uses additional CPU and memory.")}<p class="hint">These observations are advisory. You can enable or disable semantic search at any time.</p><button class="ghost" onclick="showSearchSettings()">Enable and search settings</button></div>`;
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
