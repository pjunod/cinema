"use strict";
// ---- playback defaults + Trakt (settings page) -----------------------------
const LANG_CHOICES=[["eng","English"],["jpn","Japanese"],["spa","Spanish"],["fre","French"],
  ["ger","German"],["ita","Italian"],["por","Portuguese"],["rus","Russian"],["kor","Korean"],
  ["chi","Chinese"],["hin","Hindi"],["ara","Arabic"],["nld","Dutch"],["swe","Swedish"],
  ["pol","Polish"],["nor","Norwegian"],["dan","Danish"],["fin","Finnish"],["tur","Turkish"],
  ["tha","Thai"],["vie","Vietnamese"],["ukr","Ukrainian"],["ces","Czech"],["ell","Greek"],
  ["heb","Hebrew"],["hun","Hungarian"],["ron","Romanian"]];
function langOpts(cur){ return LANG_CHOICES.map(([v,l])=>`<option value="${v}" ${cur===v?'selected':''}>${l}</option>`).join(""); }
async function saveAnalysisSettings(btn){
  const err=document.getElementById("an-error"); if(err) err.textContent="";
  if(btn) btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      vod_index_mins:Number(document.getElementById("an-every").value),
      vod_index_cluster_cache:document.getElementById("an-enabled").checked,
      analysis_max_attempts:Number(document.getElementById("an-attempts").value),
      analysis_lease_secs:Number(document.getElementById("an-lease").value),
      analysis_backoff_base_secs:Number(document.getElementById("an-backoff-base").value),
      analysis_backoff_max_secs:Number(document.getElementById("an-backoff-max").value),
    }}));
    const snapshot=await api("/analysis/summary");
    SETTINGS_DATA.analysis=snapshot; SETTINGS_LOADED.add("analysis");
    if(settingsCurrent(PAGE_RENDER_GENERATION,"analysis")) renderSettings();
    toast("Analysis settings saved");
  }catch(error){ if(err) err.textContent=error.message; if(btn) btn.disabled=false; }
}
async function savePlaybackDefaults(btn){
  const err=document.getElementById("perr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      default_audio_lang:document.getElementById("pal").value,
      default_sub_lang:document.getElementById("psl").value,
      sub_mode:document.getElementById("psm").value}}));
    toast("Playback defaults saved"); if(btn) setCardSaved(btn);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function saveStreaming(btn){
  const err=document.getElementById("serr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      stream_readrate:document.getElementById("prr").value,
      hls_readrate:document.getElementById("phr").value,
      hls_burst_secs:document.getElementById("phb").value,
      hls_ahead_max_secs:document.getElementById("pha").value,
      vod_presentation:document.getElementById("pvod").checked,
      vod_working_set_bytes:document.getElementById("pvws").value,
      vod_block_budget_secs:"8",
      vod_materialize_budget_secs:document.getElementById("pvmb").value,
      vod_blocked_get_cap:document.getElementById("pvbg").value}}));
    toast("Streaming settings saved"); if(btn) setCardSaved(btn);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function savePlaybackCompatibility(btn){
  const err=document.getElementById("dverr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      playback_control_protocol_v1:document.getElementById("pcpv1").checked}}));
    toast("Playback compatibility saved"); if(btn) setCardSaved(btn);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function saveLiveHlsRecovery(btn){
  const err=document.getElementById("dvlrerr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      vod_live_recovery:document.getElementById("dvlr").checked}}));
    toast("Live HLS recovery setting saved"); if(btn) setCardSaved(btn);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function saveWindowsCompatibility(btn){
  const err=document.getElementById("winerr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      dolby_vision_convert:document.getElementById("dvwin").checked}}));
    toast("Windows compatibility setting saved"); if(btn) setCardSaved(btn);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
// The DVR fields go on their own, never mixed with the Live TV tuple: that
// tuple's write is a generation CAS, and a request carrying both would let one
// commit while the other reported 409. The server refuses the mixture with a
// 400 rather than let a client find that out the interesting way.
async function saveDvrSettings(btn){
  const err=document.getElementById("dvrerr"); err.textContent="";
  const number=(id,fallback)=>{
    const value=Number(document.getElementById(id).value);
    return Number.isFinite(value)?value:fallback;
  };
  if(btn) btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      dvr_enabled:document.getElementById("dvrenabled").checked,
      dvr_root:document.getElementById("dvrroot").value.trim(),
      dvr_free_floor_gb:number("dvrfloor",50),
      dvr_tuner_reserve:number("dvrreserve",1),
      dvr_pad_start_s:number("dvrpadstart",60),
      dvr_pad_end_s:number("dvrpadend",120),
      dvr_reminder_lead_s:number("dvrlead",300),
      dvr_webhook_url:document.getElementById("dvrwebhook").value.trim()}}));
    toast("Recording settings saved"); if(btn) setCardSaved(btn);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function saveLibraryChannelsSettings(btn){
  const err=document.getElementById("lcdeverr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      library_channel_subject_matching_enabled:document.getElementById("lcsubjectenabled").checked,
      library_channels_enabled:document.getElementById("lcenabled").checked}}));
    toast("Library-channel setting saved"); if(btn) setCardSaved(btn);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function saveVerifiedDecode(btn){
  const err=document.getElementById("dhqerr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    // Re-render from the response rather than the request: the server decides
    // whether the request is in force, and the operator needs to see that
    // answer and not their own click reflected back at them.
    const saved=await api("/settings",{method:"PUT",body:{
      decoder_health_qualified_artifacts:document.getElementById("dhqa").checked}});
    cacheSettings(saved);
    toast("Verified decode setting saved"); if(btn) setCardSaved(btn);
    // Replace this card only. A full panel re-render would rebuild the other
    // Playback cards and silently discard anything typed into them — Streaming
    // and the other Advanced server delivery cards stage unsaved edits.
    const card=document.getElementById("vdcard");
    if(card) card.outerHTML=verifiedDecodeCard(saved);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function saveChapterThumbnails(btn){
  const err=document.getElementById("chthumberr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    const saved=await api("/settings",{method:"PUT",body:{
      chapter_thumbnails:document.getElementById("chthumb").checked}});
    cacheSettings(saved);
    toast("Chapter thumbnail setting saved"); if(btn) setCardSaved(btn);
    const card=document.getElementById("chthumbcard");
    if(card) card.outerHTML=chapterThumbnailsCard(saved,DEVELOPER_READINESS);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function saveAutomaticDecoderRecovery(btn){
  const err=document.getElementById("adrerr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    const saved=await api("/settings",{method:"PUT",body:{
      automatic_decoder_recovery:document.getElementById("adr").checked}});
    cacheSettings(saved);
    toast("Automatic decoder recovery setting saved"); if(btn) setCardSaved(btn);
    const card=document.getElementById("drcard");
    if(card) card.outerHTML=decodeRecoveryCard(saved);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
function monarrCardHtml(){
  const url=(SETTINGS&&SETTINGS.monarr_url)||"";
  const key=(SETTINGS&&SETTINGS.monarr_api_key)||"";
  const on=(SETTINGS&&SETTINGS.monarr_configured);
  return setCard(`${cardHead("Curator","A <b>Coming soon</b> rail on the home screen from Curator's calendar — episodes airing and films due in the next four weeks. Optional; leave empty for no rail.",`<span class="pill${on?" ok":""}">${on?"paired":"not paired"}</span>`)}
    <div class="setfields">
      <div><label for="mnurl">Curator URL</label><input id="mnurl" placeholder="http://curator:7676" value="${esc(url)}" style="min-width:220px"></div>
      <div><label for="mnkey">API key <span class="muted">— Settings → Security in Curator</span></label><input id="mnkey" type="password" autocomplete="off" value="${esc(key)}" onfocus="this.type='text'" onblur="this.type='password'" style="min-width:220px"></div>
    </div>
    <div class="hint">plurx keeps the key server-side and calls Curator for you, so it is never sent to a browser.</div>
    ${togRow("mnsync","Send watch state to monarr","Off by default. When on, plurx tells Curator what was watched <b>and by which Cinema user</b>, so Curator can prefer upgrades for shows someone is following. That is viewing history leaving plurx — turn it on only if you want Curator to have it. Nothing is ever deleted as a result.",SETTINGS&&SETTINGS.monarr_watched_sync)}
    <div class="err" id="mnerr"></div>
    <div class="setfoot"><button class="primary sm" disabled onclick="saveMonarr(this)">Save</button>
      <button class="ghost sm" onclick="testMonarr()">Test connection</button>
      <span id="mnstat" class="muted" style="font-size:12px"></span><span class="setstate">Saved</span></div>
    <div id="mnqueue" class="hint"></div>`,{id:"monarrcard"});
}
async function testMonarr(){
  const el=document.getElementById("mnstat"), q=document.getElementById("mnqueue");
  el.textContent="checking…"; if(q) q.textContent="";
  try{
    const st=await api("/monarr/status");
    if(!st.configured){ el.textContent="✗ not configured"; return; }
    el.textContent = st.reachable
      ? `✓ connected${st.version?` · Curator ${st.version}`:""}`
      : `✗ ${st.error||"no answer"}`;
    // The outbox is the other half of "is it working": a reachable Curator
    // with a hundred pending notifications is a different problem from an
    // unreachable one, and both look identical without this.
    if(q && (st.watched_pending||st.watched_sent||st.watched_failed)){
      q.textContent = `Watch notifications — ${st.watched_sent} sent`
        + (st.watched_pending?`, ${st.watched_pending} waiting`:"")
        + (st.watched_failed?`, ${st.watched_failed} failed`:"");
    }
  }catch(e){ el.textContent="✗ "+(e.message||String(e)); }
}
async function saveMonarr(btn){
  const err=document.getElementById("mnerr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      monarr_url:document.getElementById("mnurl").value.trim(),
      monarr_api_key:document.getElementById("mnkey").value.trim(),
      monarr_watched_sync:document.getElementById("mnsync").checked}}));
    toast("monarr settings saved"); if(btn) setCardSaved(btn);
    testMonarr(); // saving without checking is how a typo survives a week
  }catch(e){ err.textContent=e.message||String(e); if(btn) btn.disabled=false; }
}
function traktCardHtml(t){
  if(t===null||t===undefined) return "";
  let body="", pill=`<span class="pill">not set up</span>`;
  if(!t.configured||TRAKT_EDIT){
    body=`<p class="sub" style="margin:0 0 4px">Create an API app at <a href="https://trakt.tv/oauth/applications" target="_blank" rel="noopener">trakt.tv/oauth/applications</a>
      (redirect URI <code>urn:ietf:wg:oauth:2.0:oob</code>), then paste its keys.</p>
      <div class="setfields">
        <div><label for="tcid">Client ID</label><input id="tcid" type="password" autocomplete="off" value="${esc((SETTINGS&&SETTINGS.trakt_client_id)||'')}" onfocus="this.type='text'" onblur="this.type='password'" style="min-width:240px"></div>
        <div><label for="tcs">Client secret</label><input id="tcs" type="password" autocomplete="off" value="${esc((SETTINGS&&SETTINGS.trakt_client_secret)||'')}" onfocus="this.type='text'" onblur="this.type='password'" style="min-width:240px"></div>
        <div class="err" id="tkerr"></div>
      </div>
      <div class="setfoot"><button class="sm" onclick="saveTraktKeys()">Save keys</button>${t.configured?`<button class="ghost sm" onclick="TRAKT_EDIT=false;paintTrakt()">Cancel</button>`:""}</div>`;
  } else if(t.pending && !t.pending.error){
    pill=`<span class="pill warn">waiting for approval</span>`;
    body=`<p class="muted" style="margin-top:0">Go to <a href="${esc(t.pending.verification_url)}" target="_blank" rel="noopener">${esc(t.pending.verification_url)}</a> and enter:</p>
      <div class="traktcode">${esc(t.pending.user_code)}</div>
      <p class="hint">Waiting for approval — this updates by itself.</p>
      <div class="row" style="margin-top:8px"><button class="ghost sm" onclick="traktUnlink()">Cancel</button></div>`;
  } else if(t.linked){
    pill=`<span class="pill ok">connected · ${esc(t.trakt_username||"?")}</span>`;
    body=`<p style="margin:4px 0 0">${t.last_sync_at?`<span class="muted">Last sync ${fmtAgo(t.last_sync_at)}</span>`:`<span class="muted">Not synced yet</span>`}${t.syncing?' <span class="muted">· syncing now…</span>':''}</p>
      ${t.note?`<p class="hint">${esc(t.note)}</p>`:''}
      <div class="setfoot"><button class="ghost sm" onclick="traktSyncNow()">Sync now</button>
        <button class="ghost sm" onclick="traktUnlink()">Disconnect</button>
        <button class="ghost sm" onclick="TRAKT_EDIT=true;paintTrakt()">Change keys</button></div>`;
  } else {
    pill=`<span class="pill warn">keys saved · not linked</span>`;
    body=`<p class="muted" style="margin-top:4px">Link your Trakt account with a one-time code.</p>
      ${t.pending&&t.pending.error?`<div class="err">${esc(t.pending.error)}</div>`:''}
      ${t.note?`<p class="hint">${esc(t.note)}</p>`:''}
      <div class="setfoot"><button class="sm" onclick="traktLink()">Connect Trakt</button>
        <button class="ghost sm" onclick="TRAKT_EDIT=true;paintTrakt()">Change keys</button></div>`;
  }
  return `<div class="card" id="traktcard">${cardHead("Trakt","Scrobble what you watch; sync watched history and resume points both ways.",pill)}${body}</div>`;
}
function paintTrakt(){ const el=document.getElementById("traktcard"); if(el) el.outerHTML=traktCardHtml(TRAKT); }
async function saveTraktKeys(){
  const err=document.getElementById("tkerr"); err.textContent="";
  try{
    cacheSettings(await api("/settings",{method:"PUT",body:{
      trakt_client_id:document.getElementById("tcid").value.trim(),
      trakt_client_secret:document.getElementById("tcs").value.trim()}}));
    TRAKT_EDIT=false;
    cacheTrakt(await api("/trakt/status"));
    paintTrakt(); toast("Trakt keys saved");
  }catch(e){ err.textContent=e.message; }
}
async function traktLink(){ try{ cacheTrakt(await api("/trakt/link",{method:"POST"})); paintTrakt(); }catch(e){ toast(e.message); } }
async function traktUnlink(){ try{ cacheTrakt(await api("/trakt/link",{method:"DELETE"})); paintTrakt(); toast("Trakt disconnected"); }catch(e){ toast(e.message); } }
async function traktSyncNow(){ try{ cacheTrakt(await api("/trakt/sync",{method:"POST"})); paintTrakt(); toast("Sync started"); }catch(e){ toast(e.message); } }

function playbackProtocolCard(settings,readiness){
  return setCard(`${cardHead("Playback control protocol","Allow compatible clients to report playback and request stream changes.",`<span class="pill">compatibility</span>`)}
      ${togRow("pcpv1","Advertise playback control protocol v1","Applies to new sessions. Quality switching needs this enabled.",settings.playback_control_protocol_v1)}
      <details class="setdetails"><summary>Client readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"playback_control_protocol_v1","clients_report","Client reporters","Only clients observed by this server can be reported here; silent clients remain unknown.")}
      </div></details>
      <div class="err" id="dverr" role="alert"></div>${setCardFoot("savePlaybackCompatibility")}`);
}

function liveHlsRecoveryCard(settings,readiness){
  const enabled=!!settings.vod_live_recovery;
  return setCard(`${cardHead("Sliding Live HLS recovery",
      "Use the bounded growing presentation when immutable VOD cannot serve a title yet.",
      enabled?`<span class="pill ok">enabled</span>`:`<span class="pill">disabled</span>`)}
      ${togRow("dvlr","Enable sliding Live HLS recovery",
        "Applies to new sessions. Titles without a usable VOD index can use a live timeline instead.",enabled)}
      <div class="hint">VOD remains preferred. Live recovery provides a bounded, growing timeline while analysis is pending or unavailable.</div>
      <details class="setdetails"><summary>Recovery diagnostics</summary><div class="setdetails-body">
      ${devReq(readiness,"live_hls_recovery","rolling_contract_built","Bounded sliding contract","Target, scheduler, served window and object grace must be present in this build.")}
      ${devReq(readiness,"live_hls_recovery","vod_coverage_replaces_it","Why recovery is still needed","Recent VOD refusals show which viewers would lose playback with recovery disabled.")}
      ${devReq(readiness,"live_hls_recovery","no_session_bypasses_the_switch","Peer takeover path","A relay takeover can require the existing rolling presentation independently of this fallback choice.")}
      ${devReq(readiness,"live_hls_recovery","rolling_clients_qualified","Physical-client qualification","Apple native HLS, Media3 and hls.js should be checked for startup, long playback, pause and resume.")}
      <p class="devcheck-note">Advisory only. Unmet, unavailable or unobservable rows do not gate this switch. Authorization, exact object ownership and scratch limits still protect each request.</p>
      </div></details><div class="err" id="dvlrerr" role="alert"></div>${setCardFoot("saveLiveHlsRecovery")}`);
}

// Chapter thumbnails. Graduated from Developer to Playback on 2026-09-28.
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
// Automatic decode recovery.
//
// An operator's opt-in with advisory evidence. Graduated from Developer to
// Playback → Advanced server delivery on 2026-09-28 (off by default). The retained diagnostic-contract
// coverage still tells an operator how much confidence to place in the
// decision, but it never disables or overrides the switch.
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
      ${togRow("adr",`Enable automatic decoder recovery`,`Applies immediately to new producer attempts. This checkbox is authoritative: missing measurements or retained contracts never turn it back off.`,enabled)}
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
// Verified decode artifacts. Graduated from Developer to Playback → Advanced
// server delivery on 2026-09-28 (off by default).
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
      ${togRow("dhqa",`Require a producer health receipt`,"Enables the path-scoped verified policy on the next start. Readiness checks are advisory and never disable this control.",s.decoder_health_qualified_artifacts)}
      <div class="hint">Applies after a server restart. Covered paths use a new cache identity, so affected titles may need transcoding again.</div>
      <details class="setdetails"><summary>Cache impact and diagnostic evidence</summary><div class="setdetails-body">
      ${cost}${checks}${refusal}${applies}
      </div></details>
      <div class="err" id="dhqerr" role="alert"></div>
      ${setCardFoot("saveVerifiedDecode")}`,{id:"vdcard"});
}

// Encoder rate control. The stored request is tri-state: unset (each encoder
// family's measured code default applies), or an explicit bitrate / quality
// choice that stays put if a family default later changes. The server reports
// unset as null and the default it resolves to separately; this card must
// round-trip null as null. Writing the displayed default back as "bitrate"
// was how a cluster ended up pinned to an explicit bitrate nobody chose.
//
// The quality value is sent only with an explicit Quality mode. It changes
// nothing effective under Bitrate or Default, but it is part of the
// speculative queue's policy key, so a stray value there would cancel queued
// background transcodes cluster-wide for no change in output.
function rateControlRequest(mode,quality){
  const m=String(mode==null?"":mode).trim();
  const q=m==="quality"?String(quality==null?"":quality).trim():"";
  return {transcode_rate_mode:m===""?null:m,transcode_quality:q===""?null:Number(q)};
}
function rateControlModeChanged(select){
  const input=/** @type {HTMLInputElement|null} */(document.getElementById("prq"));
  if(!input) return;
  const quality=select.value==="quality";
  input.disabled=!quality;
  if(!quality) input.value="";
}
function rateControlCard(s){
  const mode=s.transcode_rate_mode==null?"":String(s.transcode_rate_mode);
  const family=s.transcode_rate_mode_default_encoder||"this encoder";
  const familyMode=s.transcode_rate_mode_default||"bitrate";
  const qualityMode=mode==="quality";
  const quality=!qualityMode||s.transcode_quality==null?"":String(s.transcode_quality);
  const qualityDefault=s.transcode_quality_default==null?"family default":`family default (${s.transcode_quality_default})`;
  const opt=(value,label)=>`<option value="${value}" ${mode===value?"selected":""}>${esc(label)}</option>`;
  return setCard(`${cardHead("Encoder rate control","Server encoding · Applies to new transcodes. Default follows each encoder family's measured choice; an explicit choice stays put if a default changes.")}
      <div class="setfields">
        <div><label for="prc">Rate control</label><select id="prc" style="min-width:260px" onchange="rateControlModeChanged(this)">
          ${opt("",`Default (per encoder) — on this node, ${family} uses ${familyMode}`)}
          ${opt("bitrate","Bitrate — fixed target")}
          ${opt("quality","Quality — constant quality, capped")}</select></div>
        <div><label for="prq">Quality value</label><input id="prq" type="number" min="0" max="255" step="1" placeholder="${esc(qualityDefault)}" value="${esc(quality)}"${qualityMode?"":" disabled"}></div>
      </div>
      <div class="hint">The quality value applies only to Quality. Switching a family to or from Quality, or changing its quality value, moves the cache identity of its transcodes, so those titles are encoded again on next demand; clearing an explicit Bitrate to a Default that is also bitrate moves nothing. A quality request is tested on this node's encoder before it is saved; a driver that refuses it keeps bitrate.</div>
      <div class="err" id="rcerr" role="alert"></div>
      ${setCardFoot("saveRateControl")}`,{id:"rccard"});
}
async function saveRateControl(btn){
  const err=document.getElementById("rcerr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    const mode=/** @type {HTMLSelectElement} */(document.getElementById("prc"));
    const quality=/** @type {HTMLInputElement} */(document.getElementById("prq"));
    const saved=await api("/settings",{method:"PUT",body:rateControlRequest(mode.value,quality.value)});
    cacheSettings(saved);
    toast("Rate control saved"); if(btn) setCardSaved(btn);
    // Re-render this card from the response: the server's answer, including
    // the default it resolves an unset request to, is what is in force.
    const card=document.getElementById("rccard");
    if(card) card.outerHTML=rateControlCard(saved);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}

// Prepared quality handoff (server switch and this browser's second player)
// and the PGS overlay graduated from Developer on 2026-10-10.
function setPreparedHandoff(on){
  setPreparedHandoffEnabled(!!on);
  // Honest about when: the capability is read on every exchange and the server
  // keeps the newest document it was sent, so this reaches it on the next
  // exchange of whatever is playing — not at the next stream.
  toast(on
    ?"This browser now offers prepared handoff — from the next control exchange"
    :"Prepared handoff switched off for this browser");
}
function preparedQualityCard(settings,readiness){
  return setCard(`${cardHead("Quality switching","Prepare the next stream before replacing the one you are watching.",`<span class="pill" id="pqhstate">${settings.prepared_quality_handoff?"Enabled":"Disabled"}</span>`)}
      ${togRow("pqh",`Prepare quality changes`,`Uses a second player and temporary server capacity to reduce interruptions. Applies after saving to the next eligible quality change.`,settings.prepared_quality_handoff)}
      <div class="hint">On by default. This saved switch is authoritative; readiness is advisory and never overrides your choice. Each browser and device can still opt out of holding a second player.</div>
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
      </div></details><div class="err" id="pqherr" role="alert"></div>
      ${setCardFoot("savePreparedQuality")}`);
}
// This browser's half of prepared quality handoff: whether it advertises a
// second player. Saved locally on change; the server switch above decides
// whether any client is offered a prepared replacement at all.
function preparedHandoffBrowserCard(){
  return setCard(`${cardHead("Second player in this browser","Advertise this browser's ability to prepare a replacement stream.",`<span class="pill acc">this browser</span>`)}
      ${togRow("pdp","Allow a second player","Uses additional device memory and decoder capacity. Saved automatically in this browser; the next reporter exchange sends the change.",preparedHandoffEnabled(),'onchange="setPreparedHandoff(this.checked)"')}
      <div class="hint">A failed preparation falls back to reopening the stream. Prepare quality changes must also be on for the server.</div>
      ${directedChangeRows()}`,{local:true});
}
// Advisory rows about the last directed quality change, inside the card that
// already exists. Paul's standing rule: features are not gated in code, so
// nothing here is read by anything that decides anything -- it is a window
// onto state the player keeps regardless.
//
// Empty when there is no player and no history, which is what the settings
// page looks like at rest. The rows appear only once there is something true
// to say about a change that actually happened.
function directedChangeRows(){
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
    </div></details>${preparedSwitchRows(p)}`;
}
// M3's rows, and the one measurement this client does not take.
//
// The first three are what the last prepared commit in this player measured.
// The enable section below them says what an audible-seam probe would need and
// whether each condition is met -- and every answer is "no", which is why the
// probe is not built. It is advisory in the strict sense: there is no switch
// here, nothing reads these back, and the prepared handoff is unaffected by
// every word of it.
function preparedSwitchRows(p){
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
async function savePreparedQuality(btn){
  const err=document.getElementById("pqherr"); err.textContent="";
  const card=btn&&btn.closest?btn.closest(".setcard"):null;
  const revision=Number(card&&card.dataset?card.dataset.revision||0:0);
  const requested=document.getElementById("pqh").checked;
  if(btn) btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{
      prepared_quality_handoff:requested}}));
    // The input remains editable while the request is in flight. A later
    // change is a new draft: keep it visible and dirty instead of overwriting
    // it with the older response and claiming the newer choice was saved.
    const currentRevision=Number(card&&card.dataset?card.dataset.revision||0:0);
    if(card&&!card.isConnected) return true;
    if(card&&card.isConnected&&currentRevision!==revision){
      toast("Earlier quality change saved; newer edit remains unsaved");
      return true;
    }
    const toggle=document.getElementById("pqh");
    if(toggle) toggle.checked=!!saved.prepared_quality_handoff;
    const state=document.getElementById("pqhstate");
    if(state) state.textContent=saved.prepared_quality_handoff?"Enabled":"Disabled";
    toast("Quality switching saved"); if(btn) setCardSaved(btn);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
function pgsOverlayCard(s,readiness){
  const enabled=!!s.pgs_overlay;
  const state=enabled
    ? `<span class="pill" style="color:var(--good);border-color:var(--good)">enabled</span>`
    : `<span class="pill">disabled</span>`;
  return setCard(`${cardHead("Serve PGS subtitles as an overlay","Give capable clients a bitmap subtitle overlay while keeping the video in its original grade.",state)}
      ${togRow("pgsoverlay",`Serve PGS subtitles as an overlay`,`Applies immediately to new playback requests on every node. Only clients that advertise pgs-v1 receive an overlay.`,enabled)}
      <div class="hint">Off burns PGS subtitles into the picture as before. If a client draws overlay cues wrongly, turn this off here; no restart is needed.</div>
      <details class="setdetails"><summary>Readiness and device qualification</summary><div class="setdetails-body">
      ${devReq(readiness,"pgs_overlay","clients_render_overlays","A client renders pgs-v1","A served manifest proves a client requested an overlay, not that it drew the cues correctly.")}
      ${devReq(readiness,"pgs_overlay","overlay_acceptance","Physical-client acceptance","Confirm cue timing, placement and seeking on Android and Apple hardware while video remains HDR or Dolby Vision.")}
      <p class="devcheck-note">These observations are advisory. The saved switch remains authoritative.</p>
      </div></details>
      <div class="err" id="pgsoverlayerr" role="alert"></div>
      ${setCardFoot("savePgsOverlay")}`,{id:"pgsoverlaycard"});
}
async function savePgsOverlay(btn){
  const err=document.getElementById("pgsoverlayerr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    const saved=await api("/settings",{method:"PUT",body:{
      pgs_overlay:(/** @type {HTMLInputElement} */ (document.getElementById("pgsoverlay"))).checked}});
    cacheSettings(saved);
    toast("PGS overlay setting saved"); if(btn) setCardSaved(btn);
    const card=document.getElementById("pgsoverlaycard");
    if(card) card.outerHTML=pgsOverlayCard(saved,DEVELOPER_READINESS);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}

// Synthetic clip generated by scripts/generate-hdr-peak-pattern (BT.2020 PQ,
// Main10 hvc1, limited range; no copyrighted or externally sourced media).
const HDR_PEAK_PATTERN_URI="data:video/mp4;base64,AAAAHGZ0eXBpc29tAAACAGlzb21pc28ybXA0MQAADK5tb292AAAAbG12aGQAAAAAAAAAAAAAAAAAAAPoAAAH0AABAAABAAAAAAAAAAAAAAAAAQAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACAAAL/XRyYWsAAABcdGtoZAAAAAMAAAAAAAAAAAAAAAEAAAAAAAAH0AAAAAAAAAAAAAAAAAAAAAAAAQAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAEAAAAAEAAAAAQAAAAAAACRlZHRzAAAAHGVsc3QAAAAAAAAAAQAAB9AAAAAAAAEAAAAAC3VtZGlhAAAAIG1kaGQAAAAAAAAAAAAAAAAAAEAAAACAAFXEAAAAAAAtaGRscgAAAAAAAAAAdmlkZQAAAAAAAAAAAAAAAFZpZGVvSGFuZGxlcgAAAAsgbWluZgAAABR2bWhkAAAAAQAAAAAAAAAAAAAAJGRpbmYAAAAcZHJlZgAAAAAAAAABAAAADHVybCAAAAABAAAK4HN0YmwAAApgc3RzZAAAAAAAAAABAAAKUGh2YzEAAAAAAAAAAQAAAAAAAAAAAAAAAAAAAAAEAAEAAEgAAABIAAAAAAAAAAEMTGF2YyBsaWJ4MjY1AAAAAAAAAAAAAAAAAAAAAAAAAAAY//8AAAncaHZjQwECIAAAAJAAAAAAAP/wAPz9+voAAA8EoAABABhAAQwB//8CIAAAAwCQAAADAAADAP+VmAmhAAEALUIBAQIgAAADAJAAAAMAAAMA/6ACAIBATZZWaSTK5qEiASCAAAADAIAAAAMAhKIAAQAGRAHBcakSJwADAAlOAZAED6AAAIAAHk4BiRghNJuqGZYI/IpIOQg9E0BCAmJaAAADAAAygAkzTgEF////////////Liyi3gm1F0fbu1Wk/n/C/E54MjY1IChidWlsZCAyMTcpIC0gNC4zKzEtZTliODgxMjpbTWFjIE9TIFhdW2NsYW5nIDIxLjAuMF1bNjQgYml0XSAxMGJpdCAtIEguMjY1L0hFVkMgY29kZWMgLSBDb3B5cmlnaHQgMjAxMy0yMDE4IChjKSBNdWx0aWNvcmV3YXJlLCBJbmMgLSBodHRwOi8veDI2NS5vcmcgLSBvcHRpb25zOiBjcHVpZD05OCBmcmFtZS10aHJlYWRzPTEgbnVtYS1wb29scz1ub25lIG5vLXdwcCBuby1wbW9kZSBuby1wbWUgbm8tcHNuciBuby1zc2ltIGxvZy1sZXZlbD0wIGJpdGRlcHRoPTEwIGlucHV0LWNzcD0xIGZwcz0xLzEgaW5wdXQtcmVzPTEwMjR4MjU2IGludGVybGFjZT0wIHRvdGFsLWZyYW1lcz0wIGxldmVsLWlkYz0wIGhpZ2gtdGllcj0xIHVoZC1iZD0wIHJlZj0zIG5vLWFsbG93LW5vbi1jb25mb3JtYW5jZSByZXBlYXQtaGVhZGVycyBhbm5leGIgbm8tYXVkIG5vLWVvYiBuby1lb3Mgbm8taHJkIGluZm8gaGFzaD0wIHRlbXBvcmFsLWxheWVycz0wIG9wZW4tZ29wIG1pbi1rZXlpbnQ9MSBrZXlpbnQ9MjUwIGdvcC1sb29rYWhlYWQ9MCBiZnJhbWVzPTQgYi1hZGFwdD0yIGItcHlyYW1pZCBiZnJhbWUtYmlhcz0wIHJjLWxvb2thaGVhZD0yMCBsb29rYWhlYWQtc2xpY2VzPTAgc2NlbmVjdXQ9NDAgbm8taGlzdC1zY2VuZWN1dCByYWRsPTAgbm8tc3BsaWNlIG5vLWludHJhLXJlZnJlc2ggY3R1PTY0IG1pbi1jdS1zaXplPTggbm8tcmVjdCBuby1hbXAgbWF4LXR1LXNpemU9MzIgdHUtaW50ZXItZGVwdGg9MSB0dS1pbnRyYS1kZXB0aD0xIGxpbWl0LXR1PTAgcmRvcS1sZXZlbD0wIGR5bmFtaWMtcmQ9MC4wMCBuby1zc2ltLXJkIHNpZ25oaWRlIG5vLXRza2lwIG5yLWludHJhPTAgbnItaW50ZXI9MCBuby1jb25zdHJhaW5lZC1pbnRyYSBzdHJvbmctaW50cmEtc21vb3RoaW5nIG1heC1tZXJnZT0zIGxpbWl0LXJlZnM9MSBuby1saW1pdC1tb2RlcyBtZT0xIHN1Ym1lPTIgbWVyYW5nZT01NyB0ZW1wb3JhbC1tdnAgbm8tZnJhbWUtZHVwIG5vLWhtZSB3ZWlnaHRwIG5vLXdlaWdodGIgbm8tYW5hbHl6ZS1zcmMtcGljcyBkZWJsb2NrPTA6MCBzYW8gbm8tc2FvLW5vbi1kZWJsb2NrIHJkPTMgc2VsZWN0aXZlLXNhbz00IGVhcmx5LXNraXAgcnNraXAgbm8tZmFzdC1pbnRyYSBuby10c2tpcC1mYXN0IG5vLWN1LWxvc3NsZXNzIGItaW50cmEgbm8tc3BsaXRyZC1za2lwIHJkcGVuYWx0eT0wIHBzeS1yZD0yLjAwIHBzeS1yZG9xPTAuMDAgbm8tcmQtcmVmaW5lIGxvc3NsZXNzIGNicXBvZmZzPTAgY3JxcG9mZnM9MCByYz1jcXAgcXA9NCBpcHJhdGlvPTEuNDAgcGJyYXRpbz0xLjMwIGFxLW1vZGU9MCBhcS1zdHJlbmd0aD0wLjAwIG5vLWN1dHJlZSB6b25lLWNvdW50PTAgbm8tc3RyaWN0LWNiciBxZy1zaXplPTY0IG5vLXJjLWdyYWluIHFwbWF4PTY5IHFwbWluPTAgbm8tY29uc3QtdmJ2IHNhcj0wIG92ZXJzY2FuPTAgdmlkZW9mb3JtYXQ9NSByYW5nZT0wIGNvbG9ycHJpbT05IHRyYW5zZmVyPTE2IGNvbG9ybWF0cml4PTkgY2hyb21hbG9jPTAgZGlzcGxheS13aW5kb3c9MCBtYXN0ZXItZGlzcGxheT1HKDg1MDAsMzk4NTApQig2NTUwLDIzMDApUigzNTQwMCwxNDYwMClXUCgxNTYzNSwxNjQ1MClMKDQwMDAwMDAwLDUwKSBjbGw9NDAwMCwwIG1pbi1sdW1hPTAgbWF4LWx1bWE9MTAyMyBsb2cyLW1heC1wb2MtbHNiPTggdnVpLXRpbWluZy1pbmZvIHZ1aS1ocmQtaW5mbyBzbGljZXM9MSBuby1vcHQtcXAtcHBzIG5vLW9wdC1yZWYtbGlzdC1sZW5ndGgtcHBzIG5vLW11bHRpLXBhc3Mtb3B0LXJwcyBzY2VuZWN1dC1iaWFzPTAuMDUgbm8tb3B0LWN1LWRlbHRhLXFwIG5vLWFxLW1vdGlvbiBoZHIxMCBuby1oZHIxMC1vcHQgbm8tZGhkcjEwLW9wdCBuby1pZHItcmVjb3Zlcnktc2VpIGFuYWx5c2lzLXJldXNlLWxldmVsPTAgYW5hbHlzaXMtc2F2ZS1yZXVzZS1sZXZlbD0wIGFuYWx5c2lzLWxvYWQtcmV1c2UtbGV2ZWw9MCBzY2FsZS1mYWN0b3I9MCByZWZpbmUtaW50cmE9MCByZWZpbmUtaW50ZXI9MCByZWZpbmUtbXY9MSByZWZpbmUtY3R1LWRpc3RvcnRpb249MCBuby1saW1pdC1zYW8gY3R1LWluZm89MCBuby1sb3dwYXNzLWRjdCByZWZpbmUtYW5hbHlzaXMtdHlwZT0wIGNvcHktcGljPTEgbWF4LWF1c2l6ZS1mYWN0b3I9MS4wIG5vLWR5bmFtaWMtcmVmaW5lIG5vLXNpbmdsZS1zZWkgbm8taGV2Yy1hcSBuby1zdnQgbm8tZmllbGQgcXAtYWRhcHRhdGlvbi1yYW5nZT0xLjAwIHNjZW5lY3V0LWF3YXJlLXFwPTBjb25mb3JtYW5jZS13aW5kb3ctb2Zmc2V0cyByaWdodD0wIGJvdHRvbT0wIGRlY29kZXItbWF4LXJhdGU9MCBuby12YnYtbGl2ZS1tdWx0aS1wYXNzIG5vLW1jc3RmIG5vLXNicmMgbm8tZnJhbWUtcmOAAAAACmZpZWwBAAAAABRidHJ0AAAAAAAAsNgAAAAAAAAAGHN0dHMAAAAAAAAAAQAAAAIAAEAAAAAAFHN0c3MAAAAAAAAAAQAAAAEAAAAcc3RzYwAAAAAAAAABAAAAAQAAAAIAAAABAAAAHHN0c3oAAAAAAAAAAAAAAAIAACwLAAAAKwAAABRzdGNvAAAAAAAAAAEAAAzaAAAAPXVkdGEAAAA1bWV0YQAAAAAAAAAhaGRscgAAAAAAAAAAbWRpcmFwcGwAAAAAAAAAAAAAAAAIaWxzdAAAAAhmcmVlAAAsPm1kYXQAAAAJTgGQBA+gAACAAAAAHk4BiRghNJuqGZYI/IpIOQg9E0BCAmJaAAADAAAygAAACTNOAQX///////////8uLKLeCbUXR9u7VaT+f8L8TngyNjUgKGJ1aWxkIDIxNykgLSA0LjMrMS1lOWI4ODEyOltNYWMgT1MgWF1bY2xhbmcgMjEuMC4wXVs2NCBiaXRdIDEwYml0IC0gSC4yNjUvSEVWQyBjb2RlYyAtIENvcHlyaWdodCAyMDEzLTIwMTggKGMpIE11bHRpY29yZXdhcmUsIEluYyAtIGh0dHA6Ly94MjY1Lm9yZyAtIG9wdGlvbnM6IGNwdWlkPTk4IGZyYW1lLXRocmVhZHM9MSBudW1hLXBvb2xzPW5vbmUgbm8td3BwIG5vLXBtb2RlIG5vLXBtZSBuby1wc25yIG5vLXNzaW0gbG9nLWxldmVsPTAgYml0ZGVwdGg9MTAgaW5wdXQtY3NwPTEgZnBzPTEvMSBpbnB1dC1yZXM9MTAyNHgyNTYgaW50ZXJsYWNlPTAgdG90YWwtZnJhbWVzPTAgbGV2ZWwtaWRjPTAgaGlnaC10aWVyPTEgdWhkLWJkPTAgcmVmPTMgbm8tYWxsb3ctbm9uLWNvbmZvcm1hbmNlIHJlcGVhdC1oZWFkZXJzIGFubmV4YiBuby1hdWQgbm8tZW9iIG5vLWVvcyBuby1ocmQgaW5mbyBoYXNoPTAgdGVtcG9yYWwtbGF5ZXJzPTAgb3Blbi1nb3AgbWluLWtleWludD0xIGtleWludD0yNTAgZ29wLWxvb2thaGVhZD0wIGJmcmFtZXM9NCBiLWFkYXB0PTIgYi1weXJhbWlkIGJmcmFtZS1iaWFzPTAgcmMtbG9va2FoZWFkPTIwIGxvb2thaGVhZC1zbGljZXM9MCBzY2VuZWN1dD00MCBuby1oaXN0LXNjZW5lY3V0IHJhZGw9MCBuby1zcGxpY2Ugbm8taW50cmEtcmVmcmVzaCBjdHU9NjQgbWluLWN1LXNpemU9OCBuby1yZWN0IG5vLWFtcCBtYXgtdHUtc2l6ZT0zMiB0dS1pbnRlci1kZXB0aD0xIHR1LWludHJhLWRlcHRoPTEgbGltaXQtdHU9MCByZG9xLWxldmVsPTAgZHluYW1pYy1yZD0wLjAwIG5vLXNzaW0tcmQgc2lnbmhpZGUgbm8tdHNraXAgbnItaW50cmE9MCBuci1pbnRlcj0wIG5vLWNvbnN0cmFpbmVkLWludHJhIHN0cm9uZy1pbnRyYS1zbW9vdGhpbmcgbWF4LW1lcmdlPTMgbGltaXQtcmVmcz0xIG5vLWxpbWl0LW1vZGVzIG1lPTEgc3VibWU9MiBtZXJhbmdlPTU3IHRlbXBvcmFsLW12cCBuby1mcmFtZS1kdXAgbm8taG1lIHdlaWdodHAgbm8td2VpZ2h0YiBuby1hbmFseXplLXNyYy1waWNzIGRlYmxvY2s9MDowIHNhbyBuby1zYW8tbm9uLWRlYmxvY2sgcmQ9MyBzZWxlY3RpdmUtc2FvPTQgZWFybHktc2tpcCByc2tpcCBuby1mYXN0LWludHJhIG5vLXRza2lwLWZhc3Qgbm8tY3UtbG9zc2xlc3MgYi1pbnRyYSBuby1zcGxpdHJkLXNraXAgcmRwZW5hbHR5PTAgcHN5LXJkPTIuMDAgcHN5LXJkb3E9MC4wMCBuby1yZC1yZWZpbmUgbG9zc2xlc3MgY2JxcG9mZnM9MCBjcnFwb2Zmcz0wIHJjPWNxcCBxcD00IGlwcmF0aW89MS40MCBwYnJhdGlvPTEuMzAgYXEtbW9kZT0wIGFxLXN0cmVuZ3RoPTAuMDAgbm8tY3V0cmVlIHpvbmUtY291bnQ9MCBuby1zdHJpY3QtY2JyIHFnLXNpemU9NjQgbm8tcmMtZ3JhaW4gcXBtYXg9NjkgcXBtaW49MCBuby1jb25zdC12YnYgc2FyPTAgb3ZlcnNjYW49MCB2aWRlb2Zvcm1hdD01IHJhbmdlPTAgY29sb3JwcmltPTkgdHJhbnNmZXI9MTYgY29sb3JtYXRyaXg9OSBjaHJvbWFsb2M9MCBkaXNwbGF5LXdpbmRvdz0wIG1hc3Rlci1kaXNwbGF5PUcoODUwMCwzOTg1MClCKDY1NTAsMjMwMClSKDM1NDAwLDE0NjAwKVdQKDE1NjM1LDE2NDUwKUwoNDAwMDAwMDAsNTApIGNsbD00MDAwLDAgbWluLWx1bWE9MCBtYXgtbHVtYT0xMDIzIGxvZzItbWF4LXBvYy1sc2I9OCB2dWktdGltaW5nLWluZm8gdnVpLWhyZC1pbmZvIHNsaWNlcz0xIG5vLW9wdC1xcC1wcHMgbm8tb3B0LXJlZi1saXN0LWxlbmd0aC1wcHMgbm8tbXVsdGktcGFzcy1vcHQtcnBzIHNjZW5lY3V0LWJpYXM9MC4wNSBuby1vcHQtY3UtZGVsdGEtcXAgbm8tYXEtbW90aW9uIGhkcjEwIG5vLWhkcjEwLW9wdCBuby1kaGRyMTAtb3B0IG5vLWlkci1yZWNvdmVyeS1zZWkgYW5hbHlzaXMtcmV1c2UtbGV2ZWw9MCBhbmFseXNpcy1zYXZlLXJldXNlLWxldmVsPTAgYW5hbHlzaXMtbG9hZC1yZXVzZS1sZXZlbD0wIHNjYWxlLWZhY3Rvcj0wIHJlZmluZS1pbnRyYT0wIHJlZmluZS1pbnRlcj0wIHJlZmluZS1tdj0xIHJlZmluZS1jdHUtZGlzdG9ydGlvbj0wIG5vLWxpbWl0LXNhbyBjdHUt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";
function hdrDisplayPeakCard(){
  const peak=hdrDisplayPeakCaps().max_nits||"auto";
  const options=[["auto","Auto (unknown)"],...[400,600,1000,1600,2000,4000].map(n=>[n,`${n} nits`])];
  return setCard(`${cardHead("HDR display peak","Saved in this browser; reported to the server for new playback. Current playback and its prepared successors keep their target.")}
    <div class="setfields"><div><label for="hdr-display-peak">HDR display peak</label>
      <select id="hdr-display-peak" onchange="saveHdrDisplayPeak(this)">${options.map(([value,label])=>`<option value="${value}" ${String(peak)===String(value)?"selected":""}>${label}</option>`).join("")}</select></div></div>
    <p class="hint">Auto sends no peak: browsers cannot report nits. For Pro Display XDR or Studio Display XDR, start with <b>1000 nits</b> (sustained full-field), rather than the 1600-nit highlight peak. Choosing too high can bring clipping back.</p>
    <details><summary>PQ highlight comparison</summary>
      <p class="hint">Play this synthetic BT.2020 PQ video on your HDR display. Each column has a 90% left half and 100% right half. Pick the highest labeled step whose two halves you can still tell apart. The labels run left to right.</p>
      <video controls muted playsinline loop preload="none" aria-label="PQ highlight pairs from 100 to 4000 nits" style="width:100%;max-width:1024px" src="${HDR_PEAK_PATTERN_URI}"></video>
      <table style="width:100%;max-width:1024px;table-layout:fixed"><tr>${[100,200,400,600,1000,1600,2000,4000].map(n=>`<th scope="col">${n}</th>`).join("")}</tr></table>
      <p class="hint">Values are nits in the encoded signal. Requires HEVC Main10 playback and actual HDR output. This app cannot verify HDR presentation; browsers may reject this clip or tone-map it to SDR. If that happens or HDR output is uncertain, keep Auto or use a known sustained display specification. This visual comparison is not an optical calibration.</p>
    </details><div class="err" id="hdr-peak-error" role="status"></div>`,{local:true});
}
function saveHdrDisplayPeak(control){
  const err=document.getElementById("hdr-peak-error");
  if(err)err.textContent="";
  const value=String(control.value);
  if(!["auto","400","600","1000","1600","2000","4000"].includes(value))return;
  try{
    if(value==="auto")localStorage.removeItem("plurx.hdrDisplayPeak");
    else localStorage.setItem("plurx.hdrDisplayPeak",value);
    toast("HDR display peak saved for the next playback");
  }catch(e){
    control.value=String(hdrDisplayPeakCaps().max_nits||"auto");
    if(err)err.textContent="This browser could not save the HDR display peak.";
  }
}
