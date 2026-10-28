"use strict";
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
function macosVideoProcessingCard(settings,readiness){
  const enabled=!!settings.macos_video_processing_enabled;
  return setCard(`${cardHead("Mac video processing","Use independently available Mac scaling, HDR/HLG conversion, subtitle burns, deinterlacing, strict Dolby Vision and Live TV processing in new sessions.",`<span class="pill">${enabled?"Enabled":"Disabled"}</span>`)}
    ${togRow("pmacosvideo","Use Mac video processing","Applied to new sessions. Unsupported sources retain their existing processing.",enabled)}
    <div class="hint">The saved choice is always accepted. Runtime compatibility and qualification below are advisory. Dependency checks require a working /usr/bin/otool (Apple Command Line Tools may be needed on a clean Mac), Apple system-only dependencies for both FFmpeg tools, and no DYLD override.</div>
    ${devReq(readiness,"macos_video_processing","effective_encoder","Effective VideoToolbox encoder","Uses the existing Hardware acceleration selection. Auto can choose Software from its encode benchmark; these switches preserve that choice. Select VideoToolbox there when wanted.")}
    ${devReq(readiness,"macos_video_processing","sdr_scale","Progressive SDR scaling","This worker checks a small embedded 8-bit and 10-bit source corpus.")}
    ${devReq(readiness,"macos_video_processing","hdr10_metal","HDR10 to SDR processing","HDR10 is resized at 10-bit precision before mapping to SDR; each source grade has its own compatibility observation.")}
    ${devReq(readiness,"macos_video_processing","hlg_metal","HLG to SDR processing","HLG has a separate analytical reference-white and rendering check.")}
    ${devReq(readiness,"macos_video_processing","subtitle_burns","SDR, HDR10 and HLG subtitle burns","Text and bitmap cues use their independently checked composition graphs.")}
    ${devReq(readiness,"macos_video_processing","deinterlace","SDR frame and field deinterlacing","TFF/BFF moving-field controls verify native SDR deinterlacing and frame or field cadence. HDR interlaced sources keep the existing processing path.")}
    ${devReq(readiness,"macos_video_processing","dolby_vision","Strict Dolby Vision profile 5","Software/VideoToolbox decode and CPU/Metal renderers have separate metadata and fail-closed observations.")}
    ${devReq(readiness,"macos_video_processing","live_upload","Live TV software decode and native processing","Known SDR sources use explicit upload, optional native deinterlacing and scaling; Live TV keeps H264 output.")}
    ${devReq(readiness,"macos_video_processing","delivery_qualification","Visual and streaming qualification","Runtime smoke checks do not approve visual quality, startup, concurrency, a sustained soak or physical clients. Normalized VOD still requires supported source facts and decoder/encoder ceilings for each source.")}
    <div class="row"><button class="ghost sm" onclick="reprobeMacosVideoProcessing(this)">Check compatibility again</button></div>
    ${devGraduation("SDR, HDR10, HLG, subtitle burns, moving-field deinterlacing, strict Dolby Vision and Live TV pass their applicable visual and daemon delivery checks; rolling HLS and encoded VOD pass seek/resume, cancellation and recovery, cache and cluster revalidation; visual checks on a named display and target Mac/client startup, concurrency and soak evidence are recorded.","the permanent switch moves to Playback → Advanced server delivery, preserving the saved choice.")}
    <div class="err" id="macos-video-error" role="alert"></div>${setCardFoot("saveMacosVideoProcessing")}`,{id:"macos-video-card"});
}

async function saveMacosVideoProcessing(btn){
  const err=document.getElementById("macos-video-error");if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{macos_video_processing_enabled:!!/** @type {HTMLInputElement} */ (document.getElementById("pmacosvideo")).checked}}));
    const card=btn?.closest(".setcard");if(card)card.outerHTML=macosVideoProcessingCard(saved,DEVELOPER_READINESS);
    toast("Mac video processing saved; applies to new sessions");
  }catch(error){if(err)err.textContent=error.message||String(error);if(btn)btn.disabled=false;}
}

function macosHevcOutputCard(settings,readiness){
  const enabled=!!settings.macos_hevc_output_enabled;
  return setCard(`${cardHead("Mac HEVC output","Allow new sessions to negotiate independently supported SDR HEVC or HDR10 Main10 output.",`<span class="pill">${enabled?"Enabled":"Disabled"}</span>`)}
    ${togRow("pmacoshevc","Use Mac HEVC output","Applies to new sessions when the client and output presentation support the promised format.",enabled)}
    <div class="hint">The saved choice is always accepted. The processing switch and the HEVC output switch are separate preferences.</div>
    ${devReq(readiness,"macos_hevc_output","hevc_effective_encoder","Effective VideoToolbox encoder","Uses the existing Hardware acceleration selection. Auto can choose Software from its encode benchmark; these switches preserve that choice. Select VideoToolbox there when wanted.")}
    ${devReq(readiness,"macos_hevc_output","hevc_sdr","Negotiated SDR HEVC","This worker checks the complete SDR HEVC encoding graph independently.")}
    ${devReq(readiness,"macos_hevc_output","hevc_hdr10","Negotiated HDR10 Main10 HEVC","HDR10 output preserves 10-bit pixels, BT.2020/PQ signaling and effective static HDR metadata. This does not authorize Dolby passthrough.")}
    ${devReq(readiness,"macos_hevc_output","hevc_delivery_qualification","Client and HDR presentation qualification","Produced segments, negotiation, seek/resume, fallback and HDR presentation require independent client and display evidence.")}
    ${devGraduation("SDR HEVC and HDR10 Main10 produced segments, client negotiation, seek/resume and grade-preserving fallback pass; HDR signaling and static metadata are checked on a named HDR display and supported target clients.","the permanent HEVC output switch moves to Playback → Advanced server delivery, preserving the saved choice.")}
    <div class="err" id="macos-hevc-error" role="alert"></div>${setCardFoot("saveMacosHevcOutput")}`,{id:"macos-hevc-card"});
}

async function saveMacosHevcOutput(btn){
  const err=document.getElementById("macos-hevc-error");if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{macos_hevc_output_enabled:!!document.getElementById("pmacoshevc").checked}}));
    const card=btn?.closest(".setcard");if(card)card.outerHTML=macosHevcOutputCard(saved,DEVELOPER_READINESS);
    toast("Mac HEVC output saved; applies to new sessions");
  }catch(error){if(err)err.textContent=error.message||String(error);if(btn)btn.disabled=false;}
}

async function reprobeMacosVideoProcessing(btn){
  const err=document.getElementById("macos-video-error");if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    await api("/developer/macos-video-processing/reprobe",{method:"POST",body:{}});
    applyDeveloperReadiness(await api("/developer/readiness"));
    toast("Compatibility check requested; refresh Settings for its result");
  }catch(error){if(err)err.textContent=error.message||String(error);}
  finally{if(btn)btn.disabled=false;}
}
// F-1: the same replicated switch, moved to Developer. Every readiness row is
// an observation or a dated qualification receipt; none is read on save or by
// the player. Enabling remains an operator choice even when rows are red.
// D6: Auto moves quality up only on link evidence, and the server accepts link
// receipts only while network priors are on, only on the node that owns the
// session, and only for a client with an IPv4 network identity. The rows say
// so; none of them changes what Save does.
function displayAwareAutoCard(settings,readiness){
  const enabled=!!settings.playback_display_aware_auto;
  return setCard(`${cardHead("Fit Auto to display","Choose a useful sustainable encode size for the fitted picture.",`<span class="pill" id="daqstate">${enabled?"Enabled":"Disabled"}</span>`)}
      ${togRow("pdisplayauto","Fit Auto to display","Compatible smooth originals remain preferred. When encoding is needed, use the active render area with at most 10% enlargement. Manual quality stays selectable.",enabled)}
      <div class="hint">This saved choice is authoritative. Readiness is advisory and never disables this switch or rejects Save.</div>
      ${devReq(readiness,"display_aware_auto","auto_abr","Adaptive Auto quality","Fit Auto to display chooses the starting rung. Changing rungs during playback is Adaptive Auto quality's switch; turning off one does not stop the other.")}
      ${devReq(readiness,"display_aware_auto","network_priors","Network priors","Without priors Auto can only move down: it never upgrades, and a link stall retries the same quality. Producer and decoder recovery still work. The Network priors card below is its switch.")}
      ${devReq(readiness,"display_aware_auto","local_session_owner","Sessions owned by the serving node","Only the node that owns a session accepts its link receipts, so a session placed on a peer gets none.")}
      ${devReq(readiness,"display_aware_auto","ipv4_client","Clients on IPv4","Network identity is an IPv4 /24. A client on IPv6 with no forwarded IPv4 address gets no network identity and so never gets link evidence.")}
      ${devStaticReq("Combined source and runtime qualification","pending","Display-aware candidates, source-grade worker proofs and physical client recovery traces are being built and qualified together.","warn")}
      ${devGraduation("the combined display-aware Auto effort passes source-grade, device and runtime qualification on its exact candidate, and D6 (link evidence only with network priors on, only on the owning node, only for IPv4 clients) is fixed or accepted.","this control graduates to Playback if a permanent toggle remains useful, otherwise fitting Auto becomes the default.")}
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

// `playback.network_priors`, which had no Settings control before this card.
// Display-aware Auto needs it to move up (D6). Off by default.
function networkPriorsCard(settings,readiness){
  const enabled=!!settings.playback_network_priors;
  return setCard(`${cardHead("Network priors","Remember how each network has played, and let Auto use it.",enabled?`<span class="pill ok">on</span>`:`<span class="pill">off</span>`)}
    ${togRow("network-priors","Use network priors","Stores playback history per user, client and IPv4 /24 network on each node: a conservative throughput estimate and the lowest rung that starved. Turning it on also changes Auto's starting (cold-start) rung on a network with history, and lets link receipts through so Fit Auto to display can upgrade.",enabled)}
    ${devReq(readiness,"network_priors","priors_history","What turning it on stores","History is node-local and kept for 30 days. Nothing new is recorded while this is off.")}
    ${devReq(readiness,"network_priors","priors_cold_start","What it changes for Auto's starting rung","With history, Auto starts below a rung that starved on that network or at the highest peak-safe rung; with none, it keeps today's start.")}
    <p class="hint">Requirements are advisory and never prevent saving.</p>
    ${devGraduation("Fit Auto to display graduates and D6 is fixed or accepted, with a fleet run showing priors improving cold starts without upgrading into stalls.","Paul chooses: the switch moves to Settings → Playback beside Auto quality, or priors become how Auto works and the switch is removed.")}
    <div class="err" id="network-priors-error" role="alert"></div>${setCardFoot("saveNetworkPriors")}`,{id:"network-priors-card"});
}
async function saveNetworkPriors(btn){
  const err=document.getElementById("network-priors-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{playback_network_priors:!!(/** @type {HTMLInputElement} */(document.getElementById("network-priors"))).checked}}));
    const card=document.getElementById("network-priors-card");if(card)card.outerHTML=networkPriorsCard(saved,DEVELOPER_READINESS);
    toast("Network priors saved");
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
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

function systemAttentionHtml(sys){
  const storage=sys.storage||{}, mounts=(storage.mounts||[]).filter(m=>m.read_bps==null||m.cache_suspect);
  const messages=mounts.map(m=>`<p><b>${esc((m.roots||[]).join(', ')||'Storage location')}</b><br>${esc(m.note||'No current read measurement is available.')} ${storage.measured_at?`<span class="muted">Observed ${fmtAgo(storage.measured_at)}.</span>`:''}</p>`);
  if(!sys.ffmpeg_version)messages.push('<p><b>Media tools unavailable.</b> Scanning and transcoding need the configured FFmpeg executable.</p>');
  if(sys.tone_map&&!sys.tone_map.ran&&sys.tone_map.verdicts?.some(v=>v.rejected))messages.push(`<p><b>HDR processing checks have not run.</b> ${esc(sys.tone_map.verdicts.find(v=>v.rejected)?.rejected||'See the capability details below.')}</p>`);
  if(sys.login_proxy_advisory)messages.push('<p><b>Login throttling sees most clients at one proxy address.</b> Configure this node’s <code>server.trusted_proxies</code> only if that proxy overwrites or appends <code>X-Forwarded-For</code> correctly. This is advisory; sign-in remains available.</p>');
  return messages.length?`<section class="system-attention"><h2>Needs attention · ${messages.length} item${messages.length===1?'':'s'}</h2><details><summary>Storage, media or sign-in warnings</summary>${messages.join('')}<div class="row"><a class="ghost sm" href="#/settings/libraries">Review library locations</a><a class="ghost sm" href="#/settings/cluster">Cluster health</a></div></details></section>`:'';
}

function toggleMobileSearch(){
  const top=document.querySelector('header.top');if(!top)return;
  const open=!top.classList.contains('search-open');top.classList.toggle('search-open',open);
  const button=top.querySelector('.mobile-search');if(button)button.setAttribute('aria-expanded',String(open));
  if(open)document.getElementById('q')?.focus();
}

function cinemaSharingCard(settings,readiness){
  return setCard(`${cardHead("Cinema shared libraries","Invite another Cinema to selected libraries through your private Tailscale connection.",'<span class="pill">Qualification pending</span>')}
    ${togRow("cinema-sharing-enabled","Enable shared libraries","The saved switch applies to this Cinema. Readiness observations never prevent saving.",!!settings.sharing_enabled)}
    ${devReq(readiness,"cinema_sharing","source_activation","Current Source activation","Source activation is pending during the rolling upgrade. It completes automatically once cluster compatibility and authority checks pass; a coordinated restart is not required. Readiness never changes your saved choice.")}
    ${devReq(readiness,"cinema_sharing","listener","Private sharing listener","A separate TLS listener serves only scoped peer requests inside this process or container. Listening here does not prove the host private network path is configured.")}
    ${devReq(readiness,"cinema_sharing","tls","Node certificate and pin","Keep the node key and verify the invitation pin through your trusted channel.")}
    ${devReq(readiness,"cinema_sharing","endpoints","Configured private Sharing endpoints","Before creating an invitation, configure this Cinema’s private addresses and verified certificate pins in <a href=\"#/settings/sharing\">Settings → Sharing</a>. Saved endpoints do not prove connectivity.")}
    ${devReq(readiness,"cinema_sharing","network","Tailscale reachability and access policy","Each serving host needs Tailscale installed and signed in, private TCP Serve forwarding from port 32443 to the internal TLS listener on 32444, a working Docker-to-host path, qualified egress and recipient access policy. A container cannot infer host installation or remote reachability; unverified means host setup must be checked.")}
    ${devReq(readiness,"cinema_sharing","qualification","Catalogue and client qualification","Shared catalogue and playback are implemented; private network, cluster recovery and device qualification remain in progress.")}
    ${devGraduation("S3–S8 shared catalogue, playback, private history and device qualification are complete.","sharing controls move to Settings → Sharing after S8 evidence and promotion.")}<div class="err" id="cinema-sharing-error" role="alert"></div>${setCardFoot("saveCinemaSharing")}`,{id:"cinema-sharing-settings"});
}
async function saveCinemaSharing(btn){
  const err=document.getElementById("cinema-sharing-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{sharing_enabled:/** @type {HTMLInputElement} */(document.getElementById("cinema-sharing-enabled")).checked}}));
    const card=document.getElementById("cinema-sharing-settings");if(card)card.outerHTML=cinemaSharingCard(saved,DEVELOPER_READINESS);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
// Complete-output preparation queued by VOD starts. Three-valued, so it has
// its own selector rather than a readiness `setting` (the switch walk PUTs
// booleans). Off by default: one play must not start a whole-title encode.
function outputPreparationCard(settings,readiness){
  const mode=["off","copy","copy_and_encoded"].includes(settings.vod_output_preparation)?settings.vod_output_preparation:"off";
  const opt=(v,label)=>`<option value="${v}"${mode===v?" selected":""}>${label}</option>`;
  return setCard(`${cardHead("Complete-output preparation","Prepare a reusable copy or encode of a title after a VOD play.",mode==="off"?`<span class="pill">off</span>`:`<span class="pill warn">${esc(mode)}</span>`)}
    <label class="tog" for="vod-output-preparation"><span>What a VOD start may queue<small>Off queues nothing. Copy queues remux preparation. Copy and encoded also queues a whole-title background encode. Turning a kind off cancels its queued jobs within a minute.</small></span><select id="vod-output-preparation">${opt("off","Off")}${opt("copy","Copy")}${opt("copy_and_encoded","Copy and encoded")}</select></label>
    ${devReq(readiness,"output_preparation","output_budget","A retained-output budget","Preparations publish into the retained registry, bounded by the cache budget (unset is 50 GB).")}
    ${devReq(readiness,"output_preparation","output_jobs","Queued and running preparation","Every job is in Settings → Jobs, filtered by kind, with a Cancel.")}
    ${devReq(readiness,"output_preparation","output_node_idle","This node's background encoder is idle","A claimed preparation yields to foreground and offline work.")}
    ${devReq(readiness,"output_preparation","output_stop","What Stop in Activity does","Stop cancels the preparation running on the node that answers; the next play of the title may queue it again.")}
    <p class="hint">Requirements are advisory and never prevent saving.</p>
    ${devGraduation("copy and encoded preparation each have a qualified fleet run: jobs drain on every node, Stop cancels, and the retained registry stays inside its budget.","Paul chooses: the selector moves to Settings → Playback beside the cache budget, or preparation becomes how plurx works and the selector is removed.")}
    <div class="err" id="vod-output-preparation-error" role="alert"></div>${setCardFoot("saveOutputPreparation")}`,{id:"vod-output-preparation-card"});
}
async function saveOutputPreparation(btn){
  const err=document.getElementById("vod-output-preparation-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{vod_output_preparation:/** @type {HTMLSelectElement} */(document.getElementById("vod-output-preparation")).value}}));
    const card=document.getElementById("vod-output-preparation-card");if(card)card.outerHTML=outputPreparationCard(saved,DEVELOPER_READINESS);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
// Retaining a rolling session's complete output as a reusable artifact. The
// links pin disk the scratch ledger stops counting, so the rows below are the
// free-space headroom and what this node holds.
function rollingRetentionCard(settings,readiness){
  const enabled=!!settings.vod_rolling_retention;
  return setCard(`${cardHead("Rolling output retention","Keep a fully watched rolling session's segments as a reusable artifact.",enabled?`<span class="pill warn">on</span>`:`<span class="pill">off</span>`)}
    ${togRow("vod-rolling-retention","Retain rolling output","Hard-links each segment into the retained namespace while the session plays. Only a title played from the start to its verified end publishes. Activity lists each retained output on this node with a Stop.",enabled)}
    ${devReq(readiness,"rolling_retention","retention_same_filesystem","Session scratch and the retained namespace share a filesystem","Hard links need one filesystem; on another device a collection is refused before it reserves anything. Windows keeps retention off.")}
    ${devReq(readiness,"rolling_retention","retention_budget","A retained-output budget","Bounded by the cache budget (unset is 50 GB).")}
    ${devReq(readiness,"rolling_retention","retention_headroom","Free space beyond everything scratch may write","Sampled every 30 seconds. Below a 1 GiB reserve, every unpublished collection is abandoned.")}
    ${devReq(readiness,"rolling_retention","retention_live_bytes","What retention holds on this node","Collecting, retained and releasing artifacts.")}
    ${devReq(readiness,"rolling_retention","retention_cleanup_pending","Nothing waiting on the collector","Orphans from a crash and released artifacts are deleted in bounded slices each tick.")}
    <p class="hint">Requirements are advisory and never prevent saving.</p>
    ${devGraduation("a qualified fleet run shows retention staying inside its headroom on every node and released artifacts deleted within a tick.","Paul chooses: the switch moves to Settings → Playback beside the cache budget, or it is removed if retention becomes how plurx works.")}
    <div class="err" id="vod-rolling-retention-error" role="alert"></div>${setCardFoot("saveRollingRetention")}`,{id:"vod-rolling-retention-card"});
}
async function saveRollingRetention(btn){
  const err=document.getElementById("vod-rolling-retention-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{vod_rolling_retention:/** @type {HTMLInputElement} */(document.getElementById("vod-rolling-retention")).checked}}));
    const card=document.getElementById("vod-rolling-retention-card");if(card)card.outerHTML=rollingRetentionCard(saved,DEVELOPER_READINESS);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
function cinemaRemoteCard(settings,readiness){
  return setCard(`${cardHead("Cinema remote control","Pair a phone or tablet with one active Cinema screen.",'<span class="pill">Qualification pending</span>')}
    ${togRow("cinema-remote-control","Enable companion remotes","Only explicitly approved same-account devices can control a screen. Turning this off ends network remote sessions.",!!settings.cinema_remote_control)}
    ${devReq(readiness,"cinema_remote_control","clients","Receiver and companion qualification","Server support does not prove client or physical remote acceptance. Saving your choice always succeeds.")}
    ${devGraduation("receiver, companion and physical remote acceptance are recorded for the composed candidate.","this switch moves to Settings → Playback as a permanent network remote preference.")}
    <div class="err" id="cinema-remote-error" role="alert"></div>${setCardFoot("saveCinemaRemote")}`);
}
async function saveCinemaRemote(button){
  const err=document.getElementById("cinema-remote-error");if(err)err.textContent="";
  if(button)button.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{cinema_remote_control:/** @type {HTMLInputElement} */(document.getElementById("cinema-remote-control")).checked}}));
    toast("Companion remote preference saved");if(button)setCardSaved(button);
  }catch(error){if(err)err.textContent=error.message;if(button)button.disabled=false;}
}

function cinemaRemoteInvitationsCard(settings,readiness){
  return setCard(`${cardHead("Cinema background invitations","Let opted-in phones receive an invitation when their Cinema screen becomes available.",'<span class="pill">Qualification pending</span>')}
    ${togRow("cinema-remote-invitations","Enable background invitations","Requires companion remotes and explicit consent on each paired phone. This choice does not change either preference.",!!settings.cinema_remote_invitations)}
    ${devReq(readiness,"cinema_remote_invitations","broker","Notification delivery provider","Configure the broker and provider separately. Missing configuration never prevents saving this choice.")}
    ${devReq(readiness,"cinema_remote_invitations","consent","Paired phone consent","Each same-account phone must explicitly opt in to invitations for its approved screen.")}
    ${devReq(readiness,"cinema_remote_invitations","delivery","Physical notification and tap acceptance","Native background delivery and opening the correct screen require platform and device qualification.")}
    ${devGraduation("provider delivery and native background notification/tap acceptance are recorded.","this permanent preference moves to Settings → Playback.")}
    <div class="err" id="cinema-invitations-error" role="alert"></div>${setCardFoot("saveCinemaRemoteInvitations")}`);
}
async function saveCinemaRemoteInvitations(button){
  const err=document.getElementById("cinema-invitations-error");if(err)err.textContent="";
  if(button)button.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{cinema_remote_invitations:/** @type {HTMLInputElement} */(document.getElementById("cinema-remote-invitations")).checked}}));
    toast("Background invitation preference saved");if(button)setCardSaved(button);
  }catch(error){if(err)err.textContent=error.message;if(button)button.disabled=false;}
}

function diagnosticDeveloperCard(){return setCard(`${cardHead("Diagnostic ZIP reports","Bounded local evidence for bug and issue attachments.",'<span class="pill">Qualification pending</span>')}<p>Help → Report a problem saves a ZIP with a readable summary and structured evidence. Attach it to an issue using your usual reporting channel. Web download and phone sharing are available. Native TVs offer a temporary local QR download on trusted Wi-Fi; physical-device sampling remains.</p>${devGraduation("archive export, privacy and recovery have applicable client and device receipts, including native TV file transfer.","this advisory card is removed; reporting remains in Help and the player.")}<button type="button" onclick="showDiagnostics()">Open reporting</button>`);}
function developerPanel(settings,readiness,httpsCard=""){
  // Only features that are not yet fully active or fully tested wait here,
  // each saying what it waits on (devGraduation). Everything that graduated
  // lives in its own section; the links below are the way back to them.
  const clustered=settingsClusterEnabled();
  const destinations=`<nav class="setdestinations" aria-label="Everyday settings">
    <a href="#/settings/livetv"><strong>Live TV <span aria-hidden="true">↗</span></strong><span>On/off, tuner, guide, recording and library channels</span></a>
    <a href="#/settings/playback"><strong>Playback <span aria-hidden="true">↗</span></strong><span>Defaults, quality switching, subtitles and delivery</span></a>
    ${clustered?`<a href="#/settings/cluster"><strong>Cluster <span aria-hidden="true">↗</span></strong><span>Health, placement, clocks and backups</span></a>`:""}
  </nav>`;
  return `${setHead("Developer","Features still waiting on qualification. Each card says what it waits on; none of them is gated by it.")}
      ${destinations}${httpsCard}${diagnosticDeveloperCard()}
      <div class="setsection"><h2>Client connections</h2><p>Compatibility awaiting complete client qualification.</p></div>${jellyfinCompatibilityCard(settings,readiness)}
      <div class="setsection"><h2>Shared libraries and remotes</h2><p>Private sharing between separate Cinemas, and companion remotes.</p></div>${cinemaSharingCard(settings,readiness)}${cinemaRemoteCard(settings,readiness)}${cinemaRemoteInvitationsCard(settings,readiness)}
      <div class="setsection" id="enable-auto-quality"><h2>Adaptive Auto quality</h2><p>One authoritative switch and dated qualification evidence for each client.</p></div>${autoQualityCard(settings)}
      <div class="setsection" id="enable-display-auto"><h2>Fit Auto to display</h2><p>One saved choice with advisory combined qualification evidence. It moves quality up only with network priors on.</p></div>${displayAwareAutoCard(settings,readiness)}${networkPriorsCard(settings,readiness)}
      <div class="setsection" id="enable-dolby-vision-processing"><h2>Dolby Vision processing</h2><p>Independent opt-in preferences awaiting production qualification.</p></div>${dolbyVisionHdrProcessingCard(settings,readiness)}${dolbyVisionFelReencodeCard(settings,readiness)}
      <div class="setsection" id="enable-display-target-hdr10"><h2>Display-targeted HDR10-E / SDR</h2><p>Per-shot L1 mapping for the viewer's reported HDR peak or explicit SDR capability. Readiness is advisory; the saved choice always applies to new playback.</p></div>${displayTargetHdr10Card(settings,readiness)}
      <div class="setsection" id="enable-content-encoding"><h2>Content-aware encoding</h2><p>Measured choices for cached and offline video, with baseline fallback.</p></div>${contentEncodingCard(settings)}
      <div class="setsection" id="enable-vod-reorder"><h2>VOD compression</h2><p>Software x264 reordered frames.</p></div>${vodReorderCard(settings)}
      <div class="setsection" id="enable-sdr-codecs"><h2>Master playlist codecs</h2><p>Name SDR codecs to players before they fetch media. Device re-qualification is still outstanding.</p></div>${sdrMasterCodecsCard(settings,readiness)}
      <div class="setsection" id="enable-hevc-copy"><h2>Enable HEVC copy</h2><p>The saved choice controls playback. Requirements below are advisory and never prevent enabling.</p></div>${hevcCopyCard(settings)}
      <div class="setsection" id="enable-output-preparation"><h2>Complete output</h2><p>Background preparation and rolling retention. Both off by default; each is attributed and stoppable in Activity.</p></div>${outputPreparationCard(settings,readiness)}${rollingRetentionCard(settings,readiness)}
      <div class="setsection" id="enable-macos-video"><h2>Mac video processing</h2><p>Worker-local compatibility and outstanding visual and streaming qualification.</p></div>${macosVideoProcessingCard(settings,readiness)}
      <div class="setsection" id="enable-macos-hevc"><h2>Mac HEVC output</h2><p>Independent output negotiation and HDR presentation qualification.</p></div>${macosHevcOutputCard(settings,readiness)}`;
}
function subtitleTranscriptionCard(){
  return setCard(`${cardHead("Generate subtitles offline","Create machine-generated captions locally with whisper.cpp.")}
      <p>Runs as background work. You can follow progress and stop queued or running transcription in Activity.</p>
      <button class="sm" onclick="openSubtitleTranscription(this)">Configure transcription</button>
      <div id="subtitle-transcription-form"></div>`,{id:"enable-subtitle-transcription"});
}
// Async setup and generation results belong to the account and page that
// requested them. A connected node alone is insufficient after sign-out/sign-in.
/** @returns {HTMLElement & {_subtitleSetup?: any, _subtitleLoad?: object}|null} */
function subtitleSetupPanel(){return document.getElementById("subtitle-transcription-form");}
/** @returns {HTMLInputElement} */
function subtitleSetupInput(id){return /** @type {HTMLInputElement} */ (document.getElementById(id));}
/** @returns {HTMLInputElement[]} */
function subtitleSetupWorkers(checked=false){return [.../** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="st-worker"]'+(checked?":checked":"")))];}
function subtitleUiCapture(){return {auth:AUTH_GENERATION,token:TOKEN,page:PAGE_RENDER_GENERATION,route:location.hash};}
function subtitleUiCurrent(c){return !!ME?.is_admin&&c.auth===AUTH_GENERATION&&c.token===TOKEN&&c.page===PAGE_RENDER_GENERATION&&c.route===location.hash;}
function subtitleReason(reason){
  return ({disabled:"Paused: subtitle generation is disabled.",missing_model:"Install the selected model on a compatible worker.",insufficient_disk:"This worker could not safely reserve model storage. Free space or check its storage support, then retry Install.",model_download_failed:"The model download failed. Check this worker’s network access, then retry Install.",engine_unavailable:"This worker’s packaged engine is unavailable or unqualified. Update or repair its supported server package, then run Test.",queue_full:"The background queue is full. Retry after current work finishes.",model_verification_failed:"The model failed its integrity or transcription test. Retry Install.",waiting_for_worker:"Waiting for a compatible worker to take this job.",worker_source_unavailable:"Waiting for a verified worker with access to this media source.",worker_observation_unavailable:"Current worker status is unavailable. Refresh worker status or check the job in Activity.",worker_not_current:"This worker is no longer a current cluster member. Refresh worker status and choose a current worker.",no_compatible_worker:"Waiting for a compatible verified worker.",source_unavailable:"No compatible worker can read this source.",waiting_for_playback_capacity:"Waiting for playback capacity. An interrupted attempt starts again.",worker_interrupted:"The worker stopped. Waiting for another attempt.",decode_failed:"The audio could not be decoded.",no_speech_detected:"The selected audio is silent. Choose another audio track.",inference_failed:"Speech recognition failed.",inference_timeout:"Speech recognition exceeded its time limit.",output_invalid:"The generated captions failed validation.",output_too_large:"The generated captions exceed the size limit.",source_changed:"The media file changed. Refresh this title before trying again.",legacy_profile_incompatible:"This older request cannot run with the current setup. Cancel it and create a new request.",configuration_conflict:"The transcription configuration changed. Reload Settings before saving."})[reason]||"The worker has not reported an actionable reason. Check the job in Activity.";
}
function subtitleJobLink(id,label="Follow this job in Activity"){
  return typeof id==="string"&&/^[a-zA-Z0-9_-]{1,128}$/.test(id)?`<a href="#/activity/jobs/${encodeURIComponent(id)}">${esc(label)}</a>`:"";
}
function subtitleLanguageName(code){try{return new Intl.DisplayNames(["en"],{type:"language"}).of(code)||code;}catch(error){return code;}}
function subtitleLanguageOptions(languages,selected=""){
  const codes=[...new Set(languages||[])];
  return `<option value="">Choose the spoken language</option>${selected&&!codes.includes(selected)?`<option value="${esc(selected)}" selected>${esc(subtitleLanguageName(selected))} · unavailable for this model</option>`:""}${codes.map(code=>`<option value="${esc(code)}"${code===selected?" selected":""}>${esc(subtitleLanguageName(code))}</option>`).join("")}`;
}
function subtitleModelSize(bytes){const value=Number(bytes);return Number.isFinite(value)&&value>=0?(value>=1048576?`${(value/1048576).toFixed(1)} MiB`:`${Math.round(value/1024)} KiB`):"Size unknown";}
function subtitleSetupModelChanged(){
  const panel=subtitleSetupPanel(),setup=panel?._subtitleSetup,language=subtitleSetupInput("st-language");if(!setup||!language)return;
  const model=setup.catalog.models.find(row=>row.id===subtitleSetupInput("st-model-id").value);
  language.innerHTML=subtitleLanguageOptions(model?.languages||[],language.value);refreshSubtitleModels();
}
function subtitleModelOptions(models,selected){
  const rows=(models||[]).map(model=>`<option value="${esc(model.id)}"${model.id===selected?" selected":""}>${esc(model.label||model.id)} · ${esc(subtitleModelSize(model.download_bytes))}</option>`).join("");
  return (models||[]).some(model=>model.id===selected)?rows:`<option value="${esc(selected||"")}" selected>${esc(selected||"Choose a model")}${selected?" · unavailable in this catalog":""}</option>${rows}`;
}
function subtitleTranscriptionForm(settings,catalog={models:[],workers:[]}){
  const advanced=settings.advanced||{},managed=settings.mode!=="advanced";
  return `<div oninput="subtitleSetupEdited()" onchange="subtitleSetupEdited()">
      <label class="tog"><span>Enable offline subtitle generation</span><input id="st-enabled" type="checkbox"${settings.enabled?" checked":""}></label>
      <p class="hint">Disabled pauses queued work and refuses new requests. A running attempt may finish; Cancel it in Activity. Readiness never changes this saved choice.</p>
      <label for="st-mode">Setup</label><select id="st-mode" onchange="subtitleSetupMode()"><option value="managed"${managed?" selected":""}>Managed installation</option><option value="advanced"${managed?"":" selected"}>Advanced · existing local installation</option></select>
      <div id="st-managed"${managed?"":" hidden"}><label for="st-model-id">Model for new requests</label><select id="st-model-id" onchange="subtitleSetupModelChanged()">${subtitleModelOptions(catalog.models,settings.model_id)}</select><p class="hint">Models download only when you choose Download and test. Audio and captions stay on your servers.</p></div>
      <div id="st-advanced"${managed?" hidden":""}><label for="st-command">Existing whisper.cpp executable on the worker</label><input id="st-command" autocomplete="off" value="${esc(advanced.command||"whisper-cli")}"><label for="st-model">Existing local model file on the worker</label><input id="st-model" autocomplete="off" value="${esc(advanced.model_path||"")}"></div>
      <label for="st-language">Default spoken language</label><select id="st-language">${subtitleLanguageOptions((catalog.models||[]).find(model=>model.id===settings.model_id)?.languages||(catalog.models||[]).flatMap(model=>model.languages||[]),settings.language||"en")}</select>
      <label for="st-interval">Check for missing subtitles every (minutes; 0 for manual requests only)</label><input id="st-interval" type="number" min="0" max="10080" step="1" value="${esc(String(settings.interval_mins||0))}">
      <p class="hint">Generation transcribes speech; it does not translate. Machine-generated captions may contain mistakes.</p>
      ${settings.mode_status&&!["ok","advanced"].includes(settings.mode_status)?`<p class="hint">${esc(subtitleReason("configuration_conflict"))}</p>`:""}
      <button class="sm" onclick="saveSubtitleTranscription(this)">Save</button></div><div id="st-status" role="status" aria-live="polite"></div>
      <div id="st-model-evidence">${subtitleModelEvidence(catalog,settings.model_id)}</div><div id="st-operation-status" role="status" aria-live="polite"></div>`;
}
function subtitleModelStatus(status){
  return ({missing:"missing",installed:"installed",verified:"verified",verification_failed:"verification failed",unverified_cache:"Download cached · download and test to finish setup",unknown:"unknown"})[status]||"unknown";
}
function subtitleModelEvidence(catalog,modelId){
  const model=(catalog.models||[]).find(row=>row.id===modelId);
  return `<h3>Download and test a model</h3>${model?`<p>${esc(model.label||model.id)} · ${esc(subtitleModelSize(model.download_bytes))} · ${esc(model.license||"")}</p><details class="setdetails"><summary>Advanced model identity</summary><p class="hint">SHA-256: ${esc(model.sha256||"")} · ${esc(model.source||"")}</p></details>`:`<p class="hint">Select a catalog model to download and test.</p>`}
    <p class="hint">Choose named workers that can run this engine. Installation and testing use background jobs. Installed alone does not mean verified.</p>
    ${(catalog.workers||[]).map(worker=>{const fresh=Number.isFinite(worker.expires_at_ms)&&worker.expires_at_ms>Date.now()&&worker.observed_at_ms<=Date.now()+30000;const entry=(worker.models||[]).find(row=>row.model_id===modelId);const status=fresh?(entry?.status||"missing"):"unknown";return `<div><label class="chk"><input type="checkbox" name="st-worker" value="${esc(worker.node_id)}"${worker.eligible&&fresh&&worker.engine?.status==="available"?" checked":" disabled"}> ${esc(worker.label||worker.node_id)}</label> · ${esc(subtitleModelStatus(status))} · engine ${esc(fresh?worker.engine?.status||"unknown":"unknown")}${entry?.verified_at_ms?` · tested ${esc(new Date(entry.verified_at_ms).toLocaleString())}`:""}${entry?.reason?`<p class="hint">${esc(subtitleReason(entry.reason))}</p>`:""}${!fresh?'<p class="hint">Worker observation is stale; readiness is unknown.</p>':""}</div>`;}).join("")||'<p class="hint">No eligible worker observations are available.</p>'}
    <button class="sm" onclick="subtitleModelOperation('install',this)">Download and test</button> <button class="sm" onclick="subtitleModelOperation('verify',this)">Test selected model</button> <button class="ghost sm" onclick="refreshSubtitleModels()">Refresh worker status</button>`;
}
function subtitleSetupEdited(){const panel=subtitleSetupPanel();if(panel?._subtitleSetup)panel._subtitleSetup.edit++;}
function subtitleSetupMode(){const advanced=subtitleSetupInput("st-mode").value==="advanced";document.getElementById("st-managed").hidden=advanced;document.getElementById("st-advanced").hidden=!advanced;}
async function openSubtitleTranscription(button){
  const panel=subtitleSetupPanel();
  if(!panel||!ME?.is_admin)return;
  const capture=subtitleUiCapture(),request={};panel._subtitleLoad=request;
  if(button)button.disabled=true;
  try{
    const [settingsRead,catalogRead]=await Promise.allSettled([api("/subtitle-transcription/managed"),api("/subtitle-transcription/models")]);
    if(!subtitleUiCurrent(capture)||!panel.isConnected||panel._subtitleLoad!==request)return;
    if(settingsRead.status!=="fulfilled")throw settingsRead.reason;
    const settings=settingsRead.value,catalog=catalogRead.status==="fulfilled"?catalogRead.value:{models:[],workers:[]};
    panel._subtitleSetup={capture,catalog,edit:0,revision:settings.revision};panel.innerHTML=subtitleTranscriptionForm(settings,catalog);
    if(catalogRead.status!=="fulfilled")document.getElementById("st-operation-status").textContent="Model and worker observations are unavailable. Your settings remain editable. Use Refresh worker status to try again.";
  }catch(error){if(subtitleUiCurrent(capture)&&panel.isConnected&&panel._subtitleLoad===request)panel.textContent=error.message||"Could not load transcription settings.";}
  finally{if(subtitleUiCurrent(capture)&&button?.isConnected)button.disabled=false;}
}
async function saveSubtitleTranscription(button){
  const panel=subtitleSetupPanel(),status=document.getElementById("st-status"),setup=panel?._subtitleSetup;
  if(!setup||!status||!subtitleUiCurrent(setup.capture))return;
  const input=id=>/** @type {HTMLInputElement} */ (document.getElementById(id));
  const interval=Number(input("st-interval").value);
  if(!Number.isInteger(interval)||interval<0||interval>10080){status.textContent="Choose a whole number of minutes from 0 to 10080.";return;}
  const body={version:1,expected_revision:setup.revision,mode:input("st-mode").value,enabled:input("st-enabled").checked,model_id:input("st-model-id").value,language:input("st-language").value.trim(),interval_mins:interval};
  if(body.mode==="advanced")body.advanced={command:input("st-command").value.trim(),model_path:input("st-model").value.trim()};
  const revision=setup.edit,request={};setup.save=request;
  if(button)button.disabled=true;status.textContent="Saving…";
  try{
    const saved=await api("/subtitle-transcription/managed",{method:"PUT",body});
    if(!subtitleUiCurrent(setup.capture)||!panel.isConnected||panel._subtitleSetup!==setup||setup.save!==request)return;
    setup.revision=saved.revision;
    if(setup.edit===revision){
      input("st-enabled").checked=saved.enabled;
      for(const [id,value]of [["st-mode",saved.mode],["st-model-id",saved.model_id],["st-language",saved.language],["st-interval",String(saved.interval_mins)],["st-command",saved.advanced?.command||"whisper-cli"],["st-model",saved.advanced?.model_path||""]])input(id).value=value;
    }
    document.getElementById("st-status").textContent=setup.edit===revision?"Saved. Worker readiness does not change this choice.":"Saved the submitted choices. Your newer edits are still unsaved.";
  }catch(error){if(subtitleUiCurrent(setup.capture)&&status.isConnected&&panel._subtitleSetup===setup)status.textContent=error.message||"Could not save transcription settings.";}
  finally{if(subtitleUiCurrent(setup.capture)&&button?.isConnected)button.disabled=false;}
}
async function refreshSubtitleModels(){
  const panel=subtitleSetupPanel(),setup=panel?._subtitleSetup,mount=document.getElementById("st-model-evidence");
  if(!setup||!mount||!subtitleUiCurrent(setup.capture))return;
  const modelId=subtitleSetupInput("st-model-id").value,request={},operation=setup.operation;setup.refresh=request;
  const selected=subtitleSetupWorkers(true).map(node=>node.value);
  try{const catalog=await api("/subtitle-transcription/models");if(!subtitleUiCurrent(setup.capture)||panel._subtitleSetup!==setup||setup.refresh!==request||setup.operation!==operation||!mount.isConnected||subtitleSetupInput("st-model-id").value!==modelId)return;setup.catalog=catalog;subtitleSetupInput("st-model-id").innerHTML=subtitleModelOptions(catalog.models,modelId);mount.innerHTML=subtitleModelEvidence(catalog,modelId);for(const node of subtitleSetupWorkers())if(!node.disabled)node.checked=selected.includes(node.value);}
  catch(error){if(subtitleUiCurrent(setup.capture)&&mount.isConnected&&setup.refresh===request&&setup.operation===operation)document.getElementById("st-operation-status").innerHTML=`${setup.operationRows?.length?subtitleModelOperationsHtml(setup):""}<p>${esc(error.message||"Could not refresh worker status.")} <button class="ghost sm" onclick="refreshSubtitleModels()">Retry worker status</button></p>`;}
}
async function subtitleModelOperation(action,button){
  const panel=subtitleSetupPanel(),setup=panel?._subtitleSetup,status=document.getElementById("st-operation-status");
  if(!setup||!status||!subtitleUiCurrent(setup.capture)||!["install","verify"].includes(action))return;
  const modelId=subtitleSetupInput("st-model-id").value,nodeIds=subtitleSetupWorkers(true).filter(node=>!node.disabled).map(node=>node.value);
  if(!(setup.catalog.models||[]).some(model=>model.id===modelId)){status.textContent="Choose an available catalog model.";return;}
  if(!nodeIds.length){status.textContent="Choose at least one eligible worker.";return;}
  const request={};setup.operation=request;button.disabled=true;status.textContent=action==="install"?"Requesting model installation…":"Requesting model verification…";
  try{const result=await api(`/subtitle-transcription/models/${encodeURIComponent(modelId)}/${action}`,{method:"POST",body:{version:1,node_ids:nodeIds}});
    if(!subtitleUiCurrent(setup.capture)||panel._subtitleSetup!==setup||setup.operation!==request||!status.isConnected)return;
    const operations=result.operations||[];
    setup.operationRows=operations.map(operation=>({...operation,model_id:modelId}));
    status.innerHTML=subtitleModelOperationsHtml(setup);
    pollSubtitleModelOperations(setup,operations.filter(op=>["accepted","existing"].includes(op.outcome)).map(op=>op.job_id));
  }catch(error){if(subtitleUiCurrent(setup.capture)&&status.isConnected&&setup.operation===request)status.textContent=error.message||"Could not request model work.";}
  finally{if(subtitleUiCurrent(setup.capture)&&button.isConnected)button.disabled=false;}
}
function subtitleModelOutcome(outcome){
  return ({accepted:"queued",existing:"already queued",queue_full:"queue full",conflict:"configuration changed",source_changed:"source changed",no_demand:"no work needed",unsupported_worker:"unsupported worker",unavailable_worker:"worker unavailable",worker_not_current:"worker is no longer current",insufficient_disk:"storage unavailable",model_download_failed:"download failed",model_verification_failed:"verification failed",engine_unavailable:"engine unavailable",missing_model:"model missing",disabled:"disabled",configuration_conflict:"configuration changed"})[outcome]||"not accepted";
}
function subtitleModelOperationPhase(job){
  const phases={queued:"Queued",waiting_for_capacity:"Waiting for server capacity",downloading:"Downloading model",verifying:"Verifying model",decoding:"Decoding test audio",transcribing:"Running speech test",validating:"Validating",publishing:"Publishing",succeeded:"Ready",failed:"Failed",cancelled:"Cancelled",running:"Running",cancelling:"Cancelling"};
  if(["succeeded","failed","cancelled"].includes(job?.state))return phases[job.state];
  if(typeof job?.phase==="string"&&Object.hasOwn(phases,job.phase))return phases[job.phase];
  if(typeof job?.state==="string"&&Object.hasOwn(phases,job.state))return phases[job.state];
  return "Status unavailable";
}
function subtitleModelOperationReason(reason){
  if(reason==="output_invalid")return "The model test could not prepare or validate captions. Check server storage and logs, then retry Test.";
  if(reason==="insufficient_disk")return "This worker could not safely reserve model storage. Free space or check its storage support, then retry Download and test.";
  if(reason==="model_download_failed")return "The model download failed. Check this worker’s network access, then retry Download and test.";
  if(reason==="model_verification_failed")return "The model failed its integrity or transcription test. Retry Download and test.";
  return subtitleReason(reason);
}
function subtitleModelOperationsHtml(setup){
  return (setup.operationRows||[]).map(row=>{const job=row.job,label=(setup.catalog.workers||[]).find(worker=>worker.node_id===row.node_id)?.label||row.node_id,reason=job?.reason||row.reason||(!["accepted","existing"].includes(row.outcome)?row.outcome:null);return `<p>${esc(label)}: ${esc(job?subtitleModelOperationPhase(job):(["accepted","existing"].includes(row.outcome)?(row.outcome==="accepted"?"Queued":"Already queued"):subtitleModelOutcome(row.outcome)))}${reason?" · "+esc(subtitleModelOperationReason(reason)):""}. ${subtitleJobLink(row.job_id)}</p>`;}).join("")||"No operation was accepted.";
}
function retrySubtitleModelOperations(){
  const setup=subtitleSetupPanel()?._subtitleSetup;if(!setup||!subtitleUiCurrent(setup.capture))return;
  const ids=(setup.operationRows||[]).filter(row=>["accepted","existing"].includes(row.outcome)&&(!["succeeded","failed","cancelled"].includes(row.job?.state)||row.awaitingObservation)).map(row=>{row.observationDeadline=null;return row.job_id;});
  pollSubtitleModelOperations(setup,ids);
}
function subtitleModelObservationPending(setup,row){
  const job=row.job;
  if(job?.state!=="succeeded")return false;
  const worker=(setup.catalog.workers||[]).find(worker=>worker.node_id===row.node_id);
  const model=worker?.models?.find(model=>model.model_id===row.model_id);
  return !worker||!Number.isFinite(worker.observed_at_ms)||worker.observed_at_ms>Date.now()+30000||!Number.isFinite(worker.expires_at_ms)||worker.expires_at_ms<=Date.now()||
    model?.status!=="verified";
}
async function pollSubtitleModelOperations(setup,ids){
  if(!ids.length||!subtitleUiCurrent(setup.capture))return;
  const panel=subtitleSetupPanel();if(panel?._subtitleSetup!==setup)return;
  const request=setup.operation;
  try{const jobs=await Promise.all(ids.map(id=>api(`/subtitle-transcription/jobs/${encodeURIComponent(id)}`)));
    if(!subtitleUiCurrent(setup.capture)||panel._subtitleSetup!==setup||setup.operation!==request)return;
    for(const job of jobs){const row=(setup.operationRows||[]).find(operation=>operation.job_id===job.job_id);if(row)row.job=job;}
    document.getElementById("st-operation-status").innerHTML=subtitleModelOperationsHtml(setup);
    await refreshSubtitleModels();
    if(!subtitleUiCurrent(setup.capture)||panel._subtitleSetup!==setup||setup.operation!==request)return;
    const pending=jobs.filter(job=>!["succeeded","failed","cancelled"].includes(job.state)).map(job=>job.job_id);
    let updating=false;
    for(const job of jobs){
      const row=(setup.operationRows||[]).find(row=>row.job_id===job.job_id);if(!row)continue;
      row.awaitingObservation=subtitleModelObservationPending(setup,row);
      if(row.awaitingObservation){
        if(row.observationDeadline==null)row.observationDeadline=Date.now()+30000;
        if(Date.now()<row.observationDeadline)pending.push(job.job_id);
        updating=true;
      }
    }
    if(updating)document.getElementById("st-operation-status").innerHTML+=`<p>Verification status is updating. <button class="ghost sm" onclick="retrySubtitleModelOperations()">Refresh verification status</button></p>`;
    if(pending.length)setTimeout(()=>{if(setup.operation===request)pollSubtitleModelOperations(setup,pending);},3000);
  }catch(error){if(subtitleUiCurrent(setup.capture)&&panel._subtitleSetup===setup&&setup.operation===request)document.getElementById("st-operation-status").innerHTML=`${subtitleModelOperationsHtml(setup)}<p>${esc(error.message||"Could not refresh model work.")} <button class="ghost sm" onclick="retrySubtitleModelOperations()">Retry status</button></p>`;}
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
    const saved=await api("/settings",{method:"PUT",body:{hevc_unverified_copy:!!(/** @type {HTMLInputElement} */ (document.getElementById("hevc-unverified"))).checked}});
    cacheSettings(saved);
    const card=document.getElementById("hevc-copy-card"); if(card)card.outerHTML=hevcCopyCard(saved);
    toast("HEVC copy preference saved");
  }catch(error){if(err)err.textContent=error.message;if(btn)btn.disabled=false;}
}

function jellyfinCompatibilityCard(settings,readiness){
  const on=!!settings.jellyfin_compatibility_enabled;
  const connection=new URL("/jellyfin",window.location.href).href;
  const host=window.location.hostname;
  return setCard(`${cardHead("Jellyfin client compatibility","Connect the tested Infuse and Jellyfin Android TV clients to this server.",`<span class="pill${on?" ok":""}">${on?"enabled":"off"}</span>`)}
    <label class="checkrow"><input type="checkbox" id="jellyfin-compatibility-enabled" ${on?"checked":""}><span>Allow Jellyfin clients</span></label>
    <p class="hint">In the client, enter just this server's address: <code>${esc(host)}</code>. Jellyfin clients add their standard port (8096) themselves. If the readiness row below says that port is not reachable, use <code>${esc(connection)}</code>, which always works. Sign in with a Plurx account.</p>
    <p class="hint">Library posters and backdrops are served without sign-in while this is on: anyone who has an artwork link can read that image. A direct-play link for one title chosen after sign-in also works without a login header; it expires within 24 hours and stops working when playback stops or the login is revoked. Everything else requires sign-in.</p>
    <details class="setdetails" open><summary>Readiness</summary><div class="setdetails-body">
    ${devReq(readiness,"jellyfin_compatibility","client_qualification","Pinned clients qualified","The complete browsing, playback, track and recovery matrix must pass on the frozen candidate.")}
    ${devReq(readiness,"jellyfin_compatibility","standard_port","Reachable by host alone","Jellyfin clients given only a host connect to port 8096 at the server root.")}
    <p class="devcheck-note">Readiness is advisory. Your saved choice controls the connection surface.</p></div></details>
    ${devGraduation("J6 records the pinned Infuse and Android TV flows, source/profile refusals, identity and watch fences, cluster behavior and native regressions on the frozen candidate.","the permanent compatibility switch moves to Settings → Integrations beside connection settings.")}
    <div class="err" id="jellyfin-compatibility-error" role="alert"></div>${setCardFoot("saveJellyfinCompatibility")}`,{id:"jellyfin-compatibility-settings"});
}
async function saveJellyfinCompatibility(btn){
  const err=document.getElementById("jellyfin-compatibility-error");if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{jellyfin_compatibility_enabled:(/** @type {HTMLInputElement} */ (document.getElementById("jellyfin-compatibility-enabled"))).checked}}));
    toast("Jellyfin compatibility saved");if(btn)setCardSaved(btn);
  }catch(error){if(err)err.textContent=error.message;if(btn)btn.disabled=false;}
}
function dolbyVisionHdrProcessingCard(settings,readiness){
  return setCard(`${cardHead("Dolby Vision processing","Use supported Dolby Vision picture information when delivering HDR or display-targeted SDR.","")}
    ${togRow("dv-hdr-processing","Process Dolby Vision for HDR and targeted SDR","Default off. Applies to new playback when processing is available.",!!settings.dolby_vision_hdr_processing)}
    <p class="hint">New playback attempts supported Dolby Vision processing when the worker has the required tools and resources. Unsupported sources or processing failures use compatible playback. Native Dolby Vision and required color conversion are preserved. Readiness never prevents saving. A running movie is unchanged.</p>
    ${devReq(readiness,"dolby_vision_hdr_processing","dv_hdr_backend_reference","Qualified backend and reference","Validate HDR output against an independent reference.")}
    ${devReq(readiness,"dolby_vision_hdr_processing","dv_hdr_encoder_matrix","Production encoder matrix","Test processing with the encoders used by playback workers.")}
    ${devReq(readiness,"dolby_vision_hdr_processing","dv_hdr_bitrate_quality","Quality at delivery bitrates","Compare picture quality at delivery bitrates.")}
    ${devReq(readiness,"dolby_vision_hdr_processing","dv_hdr_physical_playback","Physical playback validation","Test HDR display output, startup, seeking, fallback and sustained playback on supported devices.")}
    ${devGraduation("the HDR backend/reference proof, production encoder matrix, bitrate quality results and physical playback validation are recorded.","this permanent preference moves independently to Settings → Playback.")}
    <div class="err" id="dv-hdr-processing-error" role="alert"></div>${setCardFoot("saveDolbyVisionHdrProcessing")}`,{id:"dv-hdr-processing-card"});
}
async function saveDolbyVisionHdrProcessing(btn){
  const card=document.getElementById("dv-hdr-processing-card");
  const revision=Number(card&&card.dataset?card.dataset.revision||0:0);
  const err=document.getElementById("dv-hdr-processing-error");if(err)err.textContent="";if(btn)btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{dolby_vision_hdr_processing:(/** @type {HTMLInputElement} */ (document.getElementById("dv-hdr-processing"))).checked}}));
    if(!card||document.getElementById("dv-hdr-processing-card")!==card)return;
    if(Number(card.dataset&&card.dataset.revision||0)!==revision){toast("Earlier preference saved; newer edit remains unsaved");return;}
    card.outerHTML=dolbyVisionHdrProcessingCard(saved,DEVELOPER_READINESS);
    toast("Dolby Vision HDR preference saved; applies to new playback decisions");
  }catch(error){if(err)err.textContent=error.message||String(error);}finally{if(btn)btn.disabled=false;}
}
function dolbyVisionFelReencodeCard(settings,readiness){
  return setCard(`${cardHead("FEL preservation for Profile 8.1","Use the enhancement layer when making a new Profile 8.1 picture.","")}
    ${togRow("dv-fel-reencode","Preserve FEL when converting to Profile 8.1","Default off. Dolby Vision conversion must also be allowed.",!!settings.dolby_vision_fel_reencode)}
    <p class="hint">This preference never enables conversion by itself. New playback attempts FEL reconstruction for supported Profile 7 sources when the worker has the required tools and resources. Unsupported sources or processing failures keep the existing permitted conversion fallback. Native Dolby Vision is preserved. Readiness never prevents saving. A running movie is unchanged.</p>
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_backend_reference","Qualified backend and reference","The processing helper on this node, the conversion permission the reconstructed 8.1 replaces, and the composition proof against the helper's FEL master render.")}
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_worker_budget","Worker CPU budget for one live reconstruction","Where this node's renderer decodes, and whether one FEL compose + encode graph fits its software budget.")}
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_live_slot","Live reconstruction slot","One live FEL reconstruction per worker; a play beyond it gets the plain conversion with the reason.")}
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_encoder_matrix","Production encoder matrix","Test reconstruction and conversion with the encoders used by playback workers.")}
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_bitrate_quality","Quality at delivery bitrates","Compare picture quality at delivery bitrates with the existing conversion.")}
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_physical_playback","Physical playback validation","Test Profile 8.1 output, startup, seeking, fallback and sustained playback on supported Dolby Vision devices.")}
    ${devGraduation("the FEL/P8.1 backend/reference proof, production encoder matrix, bitrate quality results and physical playback validation are recorded.","this permanent preference moves independently to Settings → Playback.")}
    <div class="err" id="dv-fel-reencode-error" role="alert"></div>${setCardFoot("saveDolbyVisionFelReencode")}`,{id:"dv-fel-reencode-card"});
}
async function saveDolbyVisionFelReencode(btn){
  const card=document.getElementById("dv-fel-reencode-card");
  const revision=Number(card&&card.dataset?card.dataset.revision||0:0);
  const err=document.getElementById("dv-fel-reencode-error");if(err)err.textContent="";if(btn)btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{dolby_vision_fel_reencode:(/** @type {HTMLInputElement} */ (document.getElementById("dv-fel-reencode"))).checked}}));
    if(!card||document.getElementById("dv-fel-reencode-card")!==card)return;
    if(Number(card.dataset&&card.dataset.revision||0)!==revision){toast("Earlier preference saved; newer edit remains unsaved");return;}
    card.outerHTML=dolbyVisionFelReencodeCard(saved,DEVELOPER_READINESS);
    toast("FEL preservation preference saved; applies to new playback decisions");
  }catch(error){if(err)err.textContent=error.message||String(error);}finally{if(btn)btn.disabled=false;}
}
function contentEncodingCard(settings){
  const ready=settings.content_encoding_scorer_ready;
  const applicability=settings.content_encoding_applicability||{};
  const selected=String(applicability.selected_encoder||"not reported");
  const override=applicability.explicit_rate_control;
  return setCard(`${cardHead("Measured per-title encoding","Idle producers compare three short SDR samples before choosing a quality recipe.","")}
    ${togRow("content-encoding","Enable content-aware encoding","Applies measured choices to cached outputs and subsequent offline requests. Explicit rate-control overrides are preserved.",!!settings.content_aware_encoding)}
    ${devStaticReq("VMAF scorer on this node",ready===true?"met":ready===false?"not available":"not checked yet","The container includes a separate static libvmaf scorer with the vmaf_v0.6.1 model. Native installs use their configured FFmpeg. The first background analysis checks automatically; missing scoring retains the normal recipe.",ready===false?"warn":"")}
    ${devStaticReq("Current encoder",esc(selected),"Only software x264 jobs use these measured choices. Hardware jobs retain their existing recipe.",selected==="software"?"":"warn")}
    ${devStaticReq("Operator rate control",override===true?"explicit override retained":override===false?"automatic policy":"not reported","An explicit bitrate or quality setting takes precedence. Content-aware encoding never changes your saved rate-control choice.",override===true?"warn":"")}
    ${devStaticReq("Software quality mode",applicability.software_quality_supported===true?"available":applicability.software_quality_supported===false?"not available":"not reported","Uses the node’s already-published encoder capability result. Saving this preference never runs a new probe.",applicability.software_quality_supported===false?"warn":"")}
    ${devStaticReq("Current scope","SDR software x264","Tagged BT.709, progressive square-pixel video without burned subtitles. Three two-second samples, the producer’s admitted thread count (at most six), and a three-minute budget. Every sample must preserve mean and lower-tail VMAF, with at least 10% byte savings.","")}
    <div class="hint">The first offline package keeps its accepted recipe and prepares evidence for later requests. Sampling is not a whole-title quality guarantee. These observations never prevent saving the switch.</div>
    ${devGraduation("representative real-title quality and cost evidence is recorded across supported sources and deployment FFmpeg builds.","the preference moves to Settings → Playback if a permanent switch remains useful.")}
    <div class="err" id="content-encoding-error" role="alert"></div>${setCardFoot("saveContentEncoding")}`,{id:"content-encoding-card"});
}
async function saveContentEncoding(btn){
  const err=document.getElementById("content-encoding-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{content_aware_encoding:(/** @type {HTMLInputElement} */ (document.getElementById("content-encoding"))).checked}}));
    const card=document.getElementById("content-encoding-card");if(card)card.outerHTML=contentEncodingCard(saved);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
function vodReorderCard(settings){
  return setCard(`${cardHead("Reordered VOD frames","Allow two B-frames in software x264 VOD encodes.","")}
    ${togRow("vod-reorder","Enable reordered VOD frames","Saves the requested recipe directly. Other encoders retain their existing frame order.",Number(settings.vod_reorder_frames)===2)}
    ${devStaticReq("Supported encoder","software x264","Hardware encoder qualification is separate. Reordered frames require both decode and presentation timestamps in the VOD fragments.","")}
    ${devStaticReq("Physical-client qualification","pending","Browser and native-client presentation continuity and measured compression receipts remain to be recorded.","warn")}
    ${devGraduation("the software x264 compression and client presentation matrix is recorded.","the switch is removed if reordered frames become the normal VOD recipe, otherwise it moves to Playback.")}
    <div class="err" id="vod-reorder-error" role="alert"></div>${setCardFoot("saveVodReorder")}`,{id:"vod-reorder-card"});
}
async function saveVodReorder(btn){
  const err=document.getElementById("vod-reorder-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{vod_reorder_frames:(/** @type {HTMLInputElement} */ (document.getElementById("vod-reorder"))).checked?2:0}}));
    const card=document.getElementById("vod-reorder-card");if(card)card.outerHTML=vodReorderCard(saved);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
// S-10: CODECS on SDR HLS master variants. AVPlayer filters variants on
// CODECS before it fetches a byte, so a string it dislikes silently removes a
// rung, and no device has re-qualified the SDR string. Off is the pre-S-10
// master every client already plays. The readiness row is advice: it never
// disables the checkbox or refuses the save.
function sdrMasterCodecsCard(settings,readiness){
  const on=!!settings.playback_sdr_master_codecs;
  return setCard(`${cardHead("CODECS on SDR master playlists","Name the exact H.264 and audio codecs on SDR HLS masters, as HDR masters already do.",`<span class="pill${on?" warn":""}">${on?"Enabled":"Off"}</span>`)}
    ${togRow("sdr-master-codecs","Print CODECS on SDR variants","Fixed when a playback session is created: a running session keeps the master it started with, and only a session rebuilt after an owner takeover or VOD resurrection reads the current value. Only complete codec facts are printed, and HDR and Dolby Vision masters are unchanged either way.",on)}
    <details class="setdetails" open><summary>Readiness — advisory only</summary><div class="setdetails-body">
    ${devReq(readiness,"sdr_master_codecs","sdr_codecs_device_requalification","Apple TV and iPhone device check","With CODECS printed, every SDR variant must still be offered and play on tvOS and iOS. A player that rejects the string drops the variant before requesting it, so this server cannot observe the failure.")}
    <p class="devcheck-note">Advisory only. The switch remains available and its save is never refused.</p>
    </div></details>${devGraduation("the Apple TV and iPhone device check confirms every SDR variant is still offered.","the default becomes on and this switch is removed.")}<div class="err" id="sdr-codecs-error" role="alert"></div>${setCardFoot("saveSdrMasterCodecs")}`,{id:"sdr-codecs-card"});
}
async function saveSdrMasterCodecs(btn){
  const err=document.getElementById("sdr-codecs-error");if(err)err.textContent="";
  if(btn)btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{playback_sdr_master_codecs:(/** @type {HTMLInputElement} */ (document.getElementById("sdr-master-codecs"))).checked}}));
    const card=document.getElementById("sdr-codecs-card");if(card)card.outerHTML=sdrMasterCodecsCard(saved,DEVELOPER_READINESS);
    toast("SDR master CODECS preference saved");
  }catch(error){if(err)err.textContent=error.message;if(btn)btn.disabled=false;}
}

// Addressed-node HTTPS controls are independent of replicated /settings.
function httpsSettingsMount(value,error){
  return `<div id="settings-https">${httpsSettingsCard(value,error)}</div>`;
}
function httpsConnectionUrl(value,href=location.href){
  const url=new URL(href);
  if(!value?.active?.enabled) return null;
  if(url.protocol!=="https:"){url.protocol="https:";url.port=String(value.active.port);}
  url.username="";url.password="";url.pathname="/";url.search="";url.hash="";
  return url.href;
}
function httpsSettingsCard(value,error){
  if(!value) return setCard(`${cardHead("HTTPS","Connections to this node; HTTP app links forward to HTTPS while active.","")}
    <p>${error?`HTTPS settings are unavailable: ${esc(error)}`:"Loading this node’s HTTPS settings…"}</p>
    <button class="ghost sm" onclick="reloadHttpsSettings(this)">Reload this node</button>`);
  const desired=value.desired,active=value.active,cert=value.certificate;
  const fields=new Set(value.overrides||[]),locked=name=>fields.has(name)?" disabled":"";
  const link=httpsConnectionUrl(value);
  return setCard(`${cardHead("HTTPS","Connections to this node; HTTP app links forward to HTTPS while active.",`<span class="pill">${active.enabled?"Active":"Inactive"}</span>`)}
    <p>Addressed node: <code>${esc(value.node_id)}</code> · revision <code>${esc(value.revision)}</code></p>
    <p>${value.restart_required?"Saved changes require this node to restart.":"The desired configuration matches this process."} Save never restarts the server. For Docker, apply with <code>make docker-up</code> (source) or <code>make docker-image-up</code> (prebuilt) to update published ports; restarting an existing container cannot change its ports.</p>
    <details><summary>Saved, desired and active configuration</summary><table><thead><tr><th>State</th><th>Enabled / port</th><th>Identity files or generated names</th></tr></thead><tbody>${[["Saved before environment",value.saved],["Desired next boot",desired],["Active process",active]].map(([label,config])=>`<tr><td>${label}</td><td>${config.enabled?"On":"Off"} / ${esc(config.port)}</td><td>${config.cert_file?`${esc(config.cert_file)}<br>${esc(config.key_file)}`:`Self-signed: ${esc(config.sans.join(", "))}`}</td></tr>`).join("")}</tbody></table></details>
    <label class="chk"><input id="https-enabled" type="checkbox"${desired.enabled?" checked":""}${locked("enabled")}> Enable HTTPS after restart</label>
    <div class="setrow"><label for="https-port">HTTPS port</label><input id="https-port" type="number" min="1" max="65535" value="${esc(desired.port)}"${locked("port")}></div>
    <p>Leave both paths empty for a persistent self-signed identity. Supplied paths refer to files on the server or inside its container, never this browser’s computer.</p>
    <div class="setrow"><label for="https-cert">Certificate chain PEM path</label><input id="https-cert" value="${esc(desired.cert_file)}"${locked("cert_file")}></div>
    <div class="setrow"><label for="https-key">Private key PEM path</label><input id="https-key" value="${esc(desired.key_file)}"${locked("key_file")}></div>
    <div class="setrow"><label for="https-sans">Self-signed DNS names and IP addresses (one per line)</label><textarea id="https-sans" rows="4"${locked("sans")}>${esc(desired.sans.join("\n"))}</textarea></div>
    <p>Generated identities retain their key and certificate across restarts and disable/enable. Changing their names or replacing an expired identity requires explicit file replacement while stopped. Add every hostname or address clients will use before first enable.</p>
    ${fields.size?`<p>Environment-controlled fields: ${esc([...fields].join(", "))}. Their saved underlying values are preserved.</p>`:""}
    ${cert?`<p>Active identity: ${esc(cert.mode)} · expires ${esc(new Date(cert.expires_at_seconds*1000).toISOString())}<br>SHA-256 <code>${esc(cert.sha256)}</code><br>Names: ${esc(cert.sans.join(", "))}</p><button class="ghost sm" onclick="downloadHttpsCertificate(this)">Download active public certificate chain</button>`:"<p>No HTTPS identity is active in this process.</p>"}
    ${link?`<p><a href="${esc(link)}" target="_blank" rel="noopener noreferrer">Open unverified HTTPS address</a> · This uses the browser’s current hostname and active port. Verify this node’s identity and trust first; the new origin needs its own sign-in.</p>`:""}
    ${value.durability_confirmed===false?'<p class="err" role="alert">The settings were published, but directory sync could not confirm crash durability. This revision is current; reload before another save and check local storage.</p>':""}
    ${devGraduation("browser and native client trust, streaming, cluster and physical-device qualification are recorded.","the permanent HTTPS controls move to Settings → System.")}
    <div class="err" id="https-error" role="alert"></div><button onclick="saveHttpsSettings(this)">Save HTTPS settings</button> <button class="ghost" onclick="reloadHttpsSettings(this)">Reload this node</button>`);
}
function applyHttpsSettings(value,error){
  const mount=document.getElementById("settings-https");
  if(mount) mount.innerHTML=httpsSettingsCard(value,error);
}
async function loadHttpsCard(generation){
  try{
    const value=await loadSettingsKey("https",generation);
    if(settingsCurrent(generation,"developer")) applyHttpsSettings(value);
  }catch(error){
    if(!error?.staleHttps&&settingsCurrent(generation,"developer")) applyHttpsSettings(null,error.message||"request failed");
  }
}
async function reloadHttpsSettings(btn){
  const generation=PAGE_RENDER_GENERATION;
  if(!settingsCurrent(generation,"developer")) return;
  if(btn)btn.disabled=true;
  SETTINGS_HTTPS_EPOCH++;SETTINGS_LOADED.delete("https");
  await loadHttpsCard(generation);
}
async function saveHttpsSettings(btn){
  const generation=PAGE_RENDER_GENERATION,mount=document.getElementById("settings-https"),snapshot=SETTINGS_DATA.https;
  if(!snapshot||!settingsCurrent(generation,"developer")||!mount) return;
  const err=document.getElementById("https-error"),desired={...snapshot.desired};
  // Env-owned fields are sent unchanged, preserving their masked saved values.
  const fields=new Set(snapshot.overrides||[]),read=(name,id,parse)=>{
    if(!fields.has(name)) desired[name]=parse(document.getElementById(id));
  };
  read("enabled","https-enabled",node=>node.checked);
  read("port","https-port",node=>Number(node.value));
  read("cert_file","https-cert",node=>node.value.trim());read("key_file","https-key",node=>node.value.trim());
  read("sans","https-sans",node=>node.value.split(/\r?\n/).map(name=>name.trim()).filter(Boolean));
  const epoch=++SETTINGS_HTTPS_EPOCH;
  if(err)err.textContent="";if(btn)btn.disabled=true;
  try{
    const value=await api("/system/https",{method:"PUT",body:{node_id:snapshot.node_id,revision:snapshot.revision,desired}});
    if(epoch!==SETTINGS_HTTPS_EPOCH||!settingsCurrent(generation,"developer")||document.getElementById("settings-https")!==mount) return;
    SETTINGS_HTTPS_EPOCH++;
    SETTINGS_DATA.https=value;SETTINGS_LOADED.add("https");applyHttpsSettings(value);
    toast("HTTPS settings saved; restart this node to apply changes");
  }catch(error){
    if(epoch!==SETTINGS_HTTPS_EPOCH||!settingsCurrent(generation,"developer")||document.getElementById("settings-https")!==mount) return;
    if(err)err.textContent=(error.message||"Save failed")+(error.status===409?" Reload this node before editing again.":"");
    if(btn)btn.disabled=false;
  }
}
async function downloadHttpsCertificate(btn){
  const generation=PAGE_RENDER_GENERATION;
  if(btn)btn.disabled=true;
  try{
    const response=await api("/system/https/certificate",{raw:true}),blob=await response.blob();
    if(!settingsCurrent(generation,"developer")) return;
    const url=URL.createObjectURL(blob),link=document.createElement("a");
    link.href=url;link.download="plurx-https-certificate.pem";link.click();URL.revokeObjectURL(url);
  }catch(error){
    if(settingsCurrent(generation,"developer")){
      const err=document.getElementById("https-error");if(err)err.textContent=error.message||"Certificate download failed";
    }
  }finally{if(btn&&btn.isConnected)btn.disabled=false;}
}

function subtitleTranslationCard(){
  return setCard(`${cardHead("AI captions and subtitle translation","Generate captions from speech, translate speech to English, or translate an existing text subtitle.",'<span class="pill">Accuracy needs checking</span>')}
    <p>Download and test the speech model on a worker, then choose Generate or translate speech in the player. A separate text model translates existing text subtitles. Each setup shows download progress, worker readiness and an explicit retry.</p>
    <p class="hint">Web, iPhone and Apple TV use the selected audio track and the current playback timeline. Shared viewers can read committed AI captions; a Source administrator requests missing coverage. Partial captions may lag or be wrong. Original captions and Off remain available.</p>
    <button type="button" class="sm" onclick="openTranslationSettings()">Set up speech captions</button><div id="subtitle-translation-form"></div>
    <button type="button" class="sm" onclick="openTextTranslationSettings()">Set up subtitle text translation</button><div id="text-subtitle-translation-settings"></div>`,{id:"enable-subtitle-translation"});
}

function displayTargetHdr10Card(settings,readiness){
  return setCard(`${cardHead("Display-targeted HDR10-E / SDR","Map each shot to a supported display bucket with Dolby display management. Shots carry the colourist's creative trims (CM v4 L8, CM v2.9 L2) when the title has them; Playback info says whether every shot, some shots or none carried them. Displays may still apply their own tone mapping.")}
    ${togRow("dv-display-target","Display-targeted HDR10-E / SDR","Default off. New HDR10-E sessions use a supported peak; explicit SDR displays use 100-nit BT.709 gamma 2.4. Unknown HDR peaks keep the master grade; ineligible SDR titles keep ordinary tone mapping.",!!settings.dolby_vision_display_target)}
    ${devReq(readiness,"dolby_vision_display_target","dv_target_helper","Helper installed and HDR10-E enabled","Readings describe this node and never disable this switch or its Save.")}
    ${devReq(readiness,"dolby_vision_display_target","dv_target_peak","Client peak reports","Platform and viewer reports select the same bucket; the source is attribution only.")}
    ${devReq(readiness,"dolby_vision_display_target","dv_target_scan","Complete-title L1 scan","A whole title must have valid L1; missing facts keep the existing HDR master or ordinary SDR route.")}
    ${devReq(readiness,"dolby_vision_display_target","dv_target_cache","Bucket storage on this node","Targeted playback renders live. Pre-encodes and retained VOD stay master-only. Private roots retain existing reader and reservation fences.")}
    ${devGraduation("output metadata, live delivery and named physical HDR and SDR display qualification pass with truthful L1-only reporting.","the permanent targeting switch moves to Playback, preserving the saved choice.")}
    <div class="err" id="dv-display-target-error" role="alert"></div>${setCardFoot("saveDisplayTargetHdr10")}`,{id:"dv-display-target-card"});
}
async function saveDisplayTargetHdr10(btn){
  const card=document.getElementById("dv-display-target-card");
  const revision=Number(card&&card.dataset?card.dataset.revision||0:0);
  const err=document.getElementById("dv-display-target-error");if(err)err.textContent="";if(btn)btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{dolby_vision_display_target:(/** @type {HTMLInputElement} */ (document.getElementById("dv-display-target"))).checked}}));
    if(!card||document.getElementById("dv-display-target-card")!==card)return;
    if(Number(card.dataset&&card.dataset.revision||0)!==revision){toast("Earlier preference saved; newer edit remains unsaved");return;}
    card.outerHTML=displayTargetHdr10Card(saved,DEVELOPER_READINESS);
    toast("Display targeting saved; applies to new playback");
  }catch(error){if(err)err.textContent=error.message||String(error);}finally{if(btn)btn.disabled=false;}
}
