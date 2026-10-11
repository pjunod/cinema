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

function developerPanel(settings,readiness){
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
      ${destinations}
      <div class="setsection"><h2>Client connections</h2><p>Compatibility awaiting complete client qualification.</p></div>${jellyfinCompatibilityCard(settings,readiness)}
      <div class="setsection"><h2>Shared libraries and remotes</h2><p>Private sharing between separate Cinemas, and companion remotes.</p></div>${cinemaSharingCard(settings,readiness)}${cinemaRemoteCard(settings,readiness)}${cinemaRemoteInvitationsCard(settings,readiness)}
      <div class="setsection" id="enable-auto-quality"><h2>Adaptive Auto quality</h2><p>One authoritative switch and dated qualification evidence for each client.</p></div>${autoQualityCard(settings)}
      <div class="setsection" id="enable-display-auto"><h2>Fit Auto to display</h2><p>One saved choice with advisory combined qualification evidence. It moves quality up only with network priors on.</p></div>${displayAwareAutoCard(settings,readiness)}${networkPriorsCard(settings,readiness)}
      <div class="setsection" id="enable-dolby-vision-processing"><h2>Dolby Vision processing</h2><p>Independent opt-in preferences awaiting production qualification.</p></div>${dolbyVisionHdrProcessingCard(settings,readiness)}${dolbyVisionFelReencodeCard(settings,readiness)}
      <div class="setsection" id="enable-content-encoding"><h2>Content-aware encoding</h2><p>Measured choices for cached and offline video, with baseline fallback.</p></div>${contentEncodingCard(settings)}
      <div class="setsection" id="enable-vod-reorder"><h2>VOD compression</h2><p>Software x264 reordered frames.</p></div>${vodReorderCard(settings)}
      <div class="setsection" id="enable-sdr-codecs"><h2>Master playlist codecs</h2><p>Name SDR codecs to players before they fetch media. Device re-qualification is still outstanding.</p></div>${sdrMasterCodecsCard(settings,readiness)}
      <div class="setsection" id="enable-hevc-copy"><h2>Enable HEVC copy</h2><p>The saved choice controls playback. Requirements below are advisory and never prevent enabling.</p></div>${hevcCopyCard(settings)}
      <div class="setsection" id="enable-output-preparation"><h2>Complete output</h2><p>Background preparation and rolling retention. Both off by default; each is attributed and stoppable in Activity.</p></div>${outputPreparationCard(settings,readiness)}${rollingRetentionCard(settings,readiness)}
      <div class="setsection" id="enable-macos-video"><h2>Mac video processing</h2><p>Worker-local compatibility and outstanding visual and streaming qualification.</p></div>${macosVideoProcessingCard(settings,readiness)}
      <div class="setsection" id="enable-macos-hevc"><h2>Mac HEVC output</h2><p>Independent output negotiation and HDR presentation qualification.</p></div>${macosHevcOutputCard(settings,readiness)}
      <div class="setsection" id="enable-subtitle-transcription"><h2>Generate subtitles</h2><p>Optional local transcription for media without suitable captions.</p></div>${subtitleTranscriptionCard()}`;
}
function subtitleTranscriptionCard(){
  return setCard(`${cardHead("Generate subtitles offline","Create machine-generated captions locally with whisper.cpp.")}
      <p>Runs as background work. You can follow progress and stop queued or running transcription in Activity.</p>
      <button class="sm" onclick="openSubtitleTranscription(this)">Configure transcription</button>
      <div id="subtitle-transcription-form"></div>
      ${devGraduation("real transcription, cancellation, source replacement and generated-caption playback are qualified on the supported workers and clients.","the permanent controls move to Settings → Maintenance.")}`);
}
function subtitleTranscriptionForm(settings){
  const readiness=settings.readiness||{};
  return `<label><input id="st-enabled" type="checkbox"${settings.enabled?" checked":""}> Enable offline subtitle generation</label>
      <p class="hint">Applies to new requests. Stop existing work in Activity. The checks below are advisory and never change your saved choice.</p>
      <label for="st-command">whisper.cpp executable on the worker</label><input id="st-command" autocomplete="off" value="${esc(settings.command||"whisper-cli")}">
      <label for="st-model">Local model file on the worker</label><input id="st-model" autocomplete="off" value="${esc(settings.model_path||"")}">
      <label for="st-language">Spoken language code</label><input id="st-language" maxlength="12" value="${esc(settings.language||"en")}">
      <label for="st-interval">Check for missing subtitles every (minutes; 0 for manual requests only)</label><input id="st-interval" type="number" min="0" max="10080" step="1" value="${esc(String(settings.interval_mins||0))}">
      <p class="hint">Audio and captions stay on your servers. Models must already be installed. Captions are labelled as machine-generated and may contain mistakes.</p>
      <div role="status"><b>Worker readiness:</b> ${readiness.ready?"Ready":"Not ready"}. ${esc(readiness.reason||"")}</div>
      <ul><li>Executable: ${readiness.command_available?"available":"not found"}</li><li>Model: ${readiness.model_available?"available":"not found"}</li></ul>
      <button class="sm" onclick="saveSubtitleTranscription(this)">Save</button><div id="st-status" role="status" aria-live="polite"></div>`;
}
async function openSubtitleTranscription(button){
  const panel=document.getElementById("subtitle-transcription-form");
  if(!panel)return;
  if(button)button.disabled=true;
  try{
    const settings=await api("/subtitle-transcription");
    if(panel.isConnected)panel.innerHTML=subtitleTranscriptionForm(settings);
  }catch(error){if(panel.isConnected)panel.textContent=error.message||"Could not load transcription settings.";}
  finally{if(button&&button.isConnected)button.disabled=false;}
}
async function saveSubtitleTranscription(button){
  const panel=document.getElementById("subtitle-transcription-form"),status=document.getElementById("st-status");
  if(!panel||!status)return;
  const input=id=>/** @type {HTMLInputElement} */ (document.getElementById(id));
  const interval=Number(input("st-interval").value);
  if(!Number.isInteger(interval)||interval<0||interval>10080){status.textContent="Choose a whole number of minutes from 0 to 10080.";return;}
  const body={enabled:input("st-enabled").checked,
    command:input("st-command").value.trim(),
    model_path:input("st-model").value.trim(),
    language:input("st-language").value.trim(),interval_mins:interval};
  if(button)button.disabled=true;
  status.textContent="Saving…";
  try{
    const saved=await api("/subtitle-transcription",{method:"PUT",body});
    if(panel.isConnected){panel.innerHTML=subtitleTranscriptionForm(saved);document.getElementById("st-status").textContent="Saved. Worker readiness does not change this choice.";}
  }catch(error){if(status.isConnected)status.textContent=error.message||"Could not save transcription settings.";}
  finally{if(button&&button.isConnected)button.disabled=false;}
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
  return setCard(`${cardHead("Dolby Vision for HDR output","Use supported Dolby Vision picture information when delivering HDR.","")}
    ${togRow("dv-hdr-processing","Process Dolby Vision for HDR output","Default off. Applies to new playback when processing is available.",!!settings.dolby_vision_hdr_processing)}
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
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_backend_reference","Qualified backend and reference","Independently validate the reconstructed picture and converted output, including HDR10 compatibility.")}
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
