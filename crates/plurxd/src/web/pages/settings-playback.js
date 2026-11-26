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
    <div id="mnqueue" class="hint"></div><p><a href="#/curator-connections">Manage optional Curator sign-in connections</a> · separate from calendar and watch-state integration credentials.</p>`,{id:"monarrcard"});
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
const HDR_PEAK_PATTERN_URI="data:video/mp4;base64,AAAAHGZ0eXBpc29tAAACAGlzb21pc28ybXA0MQAADMFtb292AAAAbG12aGQAAAAAAAAAAAAAAAAAAAPoAAAH0AABAAABAAAAAAAAAAAAAAAAAQAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACAAAMEHRyYWsAAABcdGtoZAAAAAMAAAAAAAAAAAAAAAEAAAAAAAAH0AAAAAAAAAAAAAAAAAAAAAAAAQAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAEAAAAAEAAAAAQAAAAAAACRlZHRzAAAAHGVsc3QAAAAAAAAAAQAAB9AAAAAAAAEAAAAAC4htZGlhAAAAIG1kaGQAAAAAAAAAAAAAAAAAAEAAAACAAFXEAAAAAAAtaGRscgAAAAAAAAAAdmlkZQAAAAAAAAAAAAAAAFZpZGVvSGFuZGxlcgAAAAszbWluZgAAABR2bWhkAAAAAQAAAAAAAAAAAAAAJGRpbmYAAAAcZHJlZgAAAAAAAAABAAAADHVybCAAAAABAAAK83N0YmwAAApzc3RzZAAAAAAAAAABAAAKY2h2YzEAAAAAAAAAAQAAAAAAAAAAAAAAAAAAAAAEAAEAAEgAAABIAAAAAAAAAAEMTGF2YyBsaWJ4MjY1AAAAAAAAAAAAAAAAAAAAAAAAAAAY//8AAAncaHZjQwECIAAAAJAAAAAAAP/wAPz9+voAAA8EoAABABhAAQwB//8CIAAAAwCQAAADAAADAP+VmAmhAAEALUIBAQIgAAADAJAAAAMAAAMA/6ACAIBATZZWaSTK5qEiASCAAAADAIAAAAMAhKIAAQAGRAHBcakSJwADAAlOAZAED6AAAIAAHk4BiRghNJuqGZYI/IpIOQg9E0BCAmJaAAADAAAygAkzTgEF////////////Liyi3gm1F0fbu1Wk/n/C/E54MjY1IChidWlsZCAyMTcpIC0gNC4zKzEtZTliODgxMjpbTWFjIE9TIFhdW2NsYW5nIDIxLjAuMF1bNjQgYml0XSAxMGJpdCAtIEguMjY1L0hFVkMgY29kZWMgLSBDb3B5cmlnaHQgMjAxMy0yMDE4IChjKSBNdWx0aWNvcmV3YXJlLCBJbmMgLSBodHRwOi8veDI2NS5vcmcgLSBvcHRpb25zOiBjcHVpZD05OCBmcmFtZS10aHJlYWRzPTEgbnVtYS1wb29scz1ub25lIG5vLXdwcCBuby1wbW9kZSBuby1wbWUgbm8tcHNuciBuby1zc2ltIGxvZy1sZXZlbD0wIGJpdGRlcHRoPTEwIGlucHV0LWNzcD0xIGZwcz0xLzEgaW5wdXQtcmVzPTEwMjR4MjU2IGludGVybGFjZT0wIHRvdGFsLWZyYW1lcz0wIGxldmVsLWlkYz0wIGhpZ2gtdGllcj0xIHVoZC1iZD0wIHJlZj0zIG5vLWFsbG93LW5vbi1jb25mb3JtYW5jZSByZXBlYXQtaGVhZGVycyBhbm5leGIgbm8tYXVkIG5vLWVvYiBuby1lb3Mgbm8taHJkIGluZm8gaGFzaD0wIHRlbXBvcmFsLWxheWVycz0wIG9wZW4tZ29wIG1pbi1rZXlpbnQ9MSBrZXlpbnQ9MjUwIGdvcC1sb29rYWhlYWQ9MCBiZnJhbWVzPTQgYi1hZGFwdD0yIGItcHlyYW1pZCBiZnJhbWUtYmlhcz0wIHJjLWxvb2thaGVhZD0yMCBsb29rYWhlYWQtc2xpY2VzPTAgc2NlbmVjdXQ9NDAgbm8taGlzdC1zY2VuZWN1dCByYWRsPTAgbm8tc3BsaWNlIG5vLWludHJhLXJlZnJlc2ggY3R1PTY0IG1pbi1jdS1zaXplPTggbm8tcmVjdCBuby1hbXAgbWF4LXR1LXNpemU9MzIgdHUtaW50ZXItZGVwdGg9MSB0dS1pbnRyYS1kZXB0aD0xIGxpbWl0LXR1PTAgcmRvcS1sZXZlbD0wIGR5bmFtaWMtcmQ9MC4wMCBuby1zc2ltLXJkIHNpZ25oaWRlIG5vLXRza2lwIG5yLWludHJhPTAgbnItaW50ZXI9MCBuby1jb25zdHJhaW5lZC1pbnRyYSBzdHJvbmctaW50cmEtc21vb3RoaW5nIG1heC1tZXJnZT0zIGxpbWl0LXJlZnM9MSBuby1saW1pdC1tb2RlcyBtZT0xIHN1Ym1lPTIgbWVyYW5nZT01NyB0ZW1wb3JhbC1tdnAgbm8tZnJhbWUtZHVwIG5vLWhtZSB3ZWlnaHRwIG5vLXdlaWdodGIgbm8tYW5hbHl6ZS1zcmMtcGljcyBkZWJsb2NrPTA6MCBzYW8gbm8tc2FvLW5vbi1kZWJsb2NrIHJkPTMgc2VsZWN0aXZlLXNhbz00IGVhcmx5LXNraXAgcnNraXAgbm8tZmFzdC1pbnRyYSBuby10c2tpcC1mYXN0IG5vLWN1LWxvc3NsZXNzIGItaW50cmEgbm8tc3BsaXRyZC1za2lwIHJkcGVuYWx0eT0wIHBzeS1yZD0yLjAwIHBzeS1yZG9xPTAuMDAgbm8tcmQtcmVmaW5lIGxvc3NsZXNzIGNicXBvZmZzPTAgY3JxcG9mZnM9MCByYz1jcXAgcXA9NCBpcHJhdGlvPTEuNDAgcGJyYXRpbz0xLjMwIGFxLW1vZGU9MCBhcS1zdHJlbmd0aD0wLjAwIG5vLWN1dHJlZSB6b25lLWNvdW50PTAgbm8tc3RyaWN0LWNiciBxZy1zaXplPTY0IG5vLXJjLWdyYWluIHFwbWF4PTY5IHFwbWluPTAgbm8tY29uc3QtdmJ2IHNhcj0wIG92ZXJzY2FuPTAgdmlkZW9mb3JtYXQ9NSByYW5nZT0wIGNvbG9ycHJpbT05IHRyYW5zZmVyPTE2IGNvbG9ybWF0cml4PTkgY2hyb21hbG9jPTAgZGlzcGxheS13aW5kb3c9MCBtYXN0ZXItZGlzcGxheT1HKDg1MDAsMzk4NTApQig2NTUwLDIzMDApUigzNTQwMCwxNDYwMClXUCgxNTYzNSwxNjQ1MClMKDQwMDAwMDAwLDUwKSBjbGw9NDAwMCwwIG1pbi1sdW1hPTAgbWF4LWx1bWE9MTAyMyBsb2cyLW1heC1wb2MtbHNiPTggdnVpLXRpbWluZy1pbmZvIHZ1aS1ocmQtaW5mbyBzbGljZXM9MSBuby1vcHQtcXAtcHBzIG5vLW9wdC1yZWYtbGlzdC1sZW5ndGgtcHBzIG5vLW11bHRpLXBhc3Mtb3B0LXJwcyBzY2VuZWN1dC1iaWFzPTAuMDUgbm8tb3B0LWN1LWRlbHRhLXFwIG5vLWFxLW1vdGlvbiBoZHIxMCBuby1oZHIxMC1vcHQgbm8tZGhkcjEwLW9wdCBuby1pZHItcmVjb3Zlcnktc2VpIGFuYWx5c2lzLXJldXNlLWxldmVsPTAgYW5hbHlzaXMtc2F2ZS1yZXVzZS1sZXZlbD0wIGFuYWx5c2lzLWxvYWQtcmV1c2UtbGV2ZWw9MCBzY2FsZS1mYWN0b3I9MCByZWZpbmUtaW50cmE9MCByZWZpbmUtaW50ZXI9MCByZWZpbmUtbXY9MSByZWZpbmUtY3R1LWRpc3RvcnRpb249MCBuby1saW1pdC1zYW8gY3R1LWluZm89MCBuby1sb3dwYXNzLWRjdCByZWZpbmUtYW5hbHlzaXMtdHlwZT0wIGNvcHktcGljPTEgbWF4LWF1c2l6ZS1mYWN0b3I9MS4wIG5vLWR5bmFtaWMtcmVmaW5lIG5vLXNpbmdsZS1zZWkgbm8taGV2Yy1hcSBuby1zdnQgbm8tZmllbGQgcXAtYWRhcHRhdGlvbi1yYW5nZT0xLjAwIHNjZW5lY3V0LWF3YXJlLXFwPTBjb25mb3JtYW5jZS13aW5kb3ctb2Zmc2V0cyByaWdodD0wIGJvdHRvbT0wIGRlY29kZXItbWF4LXJhdGU9MCBuby12YnYtbGl2ZS1tdWx0aS1wYXNzIG5vLW1jc3RmIG5vLXNicmMgbm8tZnJhbWUtcmOAAAAACmZpZWwBAAAAABNjb2xybmNseAAJABAACQAAAAAUYnRydAAAAAAAALDYAAAAAAAAABhzdHRzAAAAAAAAAAEAAAACAABAAAAAABRzdHNzAAAAAAAAAAEAAAABAAAAHHN0c2MAAAAAAAAAAQAAAAEAAAACAAAAAQAAABxzdHN6AAAAAAAAAAAAAAACAAAsCwAAACsAAAAUc3RjbwAAAAAAAAABAAAM7QAAAD11ZHRhAAAANW1ldGEAAAAAAAAAIWhkbHIAAAAAAAAAAG1kaXJhcHBsAAAAAAAAAAAAAAAACGlsc3QAAAAIZnJlZQAALD5tZGF0AAAACU4BkAQPoAAAgAAAAB5OAYkYITSbqhmWCPyKSDkIPRNAQgJiWgAAAwAAMoAAAAkzTgEF////////////Liyi3gm1F0fbu1Wk/n/C/E54MjY1IChidWlsZCAyMTcpIC0gNC4zKzEtZTliODgxMjpbTWFjIE9TIFhdW2NsYW5nIDIxLjAuMF1bNjQgYml0XSAxMGJpdCAtIEguMjY1L0hFVkMgY29kZWMgLSBDb3B5cmlnaHQgMjAxMy0yMDE4IChjKSBNdWx0aWNvcmV3YXJlLCBJbmMgLSBodHRwOi8veDI2NS5vcmcgLSBvcHRpb25zOiBjcHVpZD05OCBmcmFtZS10aHJlYWRzPTEgbnVtYS1wb29scz1ub25lIG5vLXdwcCBuby1wbW9kZSBuby1wbWUgbm8tcHNuciBuby1zc2ltIGxvZy1sZXZlbD0wIGJpdGRlcHRoPTEwIGlucHV0LWNzcD0xIGZwcz0xLzEgaW5wdXQtcmVzPTEwMjR4MjU2IGludGVybGFjZT0wIHRvdGFsLWZyYW1lcz0wIGxldmVsLWlkYz0wIGhpZ2gtdGllcj0xIHVoZC1iZD0wIHJlZj0zIG5vLWFsbG93LW5vbi1jb25mb3JtYW5jZSByZXBlYXQtaGVhZGVycyBhbm5leGIgbm8tYXVkIG5vLWVvYiBuby1lb3Mgbm8taHJkIGluZm8gaGFzaD0wIHRlbXBvcmFsLWxheWVycz0wIG9wZW4tZ29wIG1pbi1rZXlpbnQ9MSBrZXlpbnQ9MjUwIGdvcC1sb29rYWhlYWQ9MCBiZnJhbWVzPTQgYi1hZGFwdD0yIGItcHlyYW1pZCBiZnJhbWUtYmlhcz0wIHJjLWxvb2thaGVhZD0yMCBsb29rYWhlYWQtc2xpY2VzPTAgc2NlbmVjdXQ9NDAgbm8taGlzdC1zY2VuZWN1dCByYWRsPTAgbm8tc3BsaWNlIG5vLWludHJhLXJlZnJlc2ggY3R1PTY0IG1pbi1jdS1zaXplPTggbm8tcmVjdCBuby1hbXAgbWF4LXR1LXNpemU9MzIgdHUtaW50ZXItZGVwdGg9MSB0dS1pbnRyYS1kZXB0aD0xIGxpbWl0LXR1PTAgcmRvcS1sZXZlbD0wIGR5bmFtaWMtcmQ9MC4wMCBuby1zc2ltLXJkIHNpZ25oaWRlIG5vLXRza2lwIG5yLWludHJhPTAgbnItaW50ZXI9MCBuby1jb25zdHJhaW5lZC1pbnRyYSBzdHJvbmctaW50cmEtc21vb3RoaW5nIG1heC1tZXJnZT0zIGxpbWl0LXJlZnM9MSBuby1saW1pdC1tb2RlcyBtZT0xIHN1Ym1lPTIgbWVyYW5nZT01NyB0ZW1wb3JhbC1tdnAgbm8tZnJhbWUtZHVwIG5vLWhtZSB3ZWlnaHRwIG5vLXdlaWdodGIgbm8tYW5hbHl6ZS1zcmMtcGljcyBkZWJsb2NrPTA6MCBzYW8gbm8tc2FvLW5vbi1kZWJsb2NrIHJkPTMgc2VsZWN0aXZlLXNhbz00IGVhcmx5LXNraXAgcnNraXAgbm8tZmFzdC1pbnRyYSBuby10c2tpcC1mYXN0IG5vLWN1LWxvc3NsZXNzIGItaW50cmEgbm8tc3BsaXRyZC1za2lwIHJkcGVuYWx0eT0wIHBzeS1yZD0yLjAwIHBzeS1yZG9xPTAuMDAgbm8tcmQtcmVmaW5lIGxvc3NsZXNzIGNicXBvZmZzPTAgY3JxcG9mZnM9MCByYz1jcXAgcXA9NCBpcHJhdGlvPTEuNDAgcGJyYXRpbz0xLjMwIGFxLW1vZGU9MCBhcS1zdHJlbmd0aD0wLjAwIG5vLWN1dHJlZSB6b25lLWNvdW50PTAgbm8tc3RyaWN0LWNiciBxZy1zaXplPTY0IG5vLXJjLWdyYWluIHFwbWF4PTY5IHFwbWluPTAgbm8tY29uc3QtdmJ2IHNhcj0wIG92ZXJzY2FuPTAgdmlkZW9mb3JtYXQ9NSByYW5nZT0wIGNvbG9ycHJpbT05IHRyYW5zZmVyPTE2IGNvbG9ybWF0cml4PTkgY2hyb21hbG9jPTAgZGlzcGxheS13aW5kb3c9MCBtYXN0ZXItZGlzcGxheT1HKDg1MDAsMzk4NTApQig2NTUwLDIzMDApUigzNTQwMCwxNDYwMClXUCgxNTYzNSwxNjQ1MClMKDQwMDAwMDAwLDUwKSBjbGw9NDAwMCwwIG1pbi1sdW1hPTAgbWF4LWx1bWE9MTAyMyBsb2cyLW1heC1wb2MtbHNiPTggdnVpLXRpbWluZy1pbmZvIHZ1aS1ocmQtaW5mbyBzbGljZXM9MSBuby1vcHQtcXAtcHBzIG5vLW9wdC1yZWYtbGlzdC1sZW5ndGgtcHBzIG5vLW11bHRpLXBhc3Mtb3B0LXJwcyBzY2VuZWN1dC1iaWFzPTAuMDUgbm8tb3B0LWN1LWRlbHRhLXFwIG5vLWFxLW1vdGlvbiBoZHIxMCBuby1oZHIxMC1vcHQgbm8tZGhkcjEwLW9wdCBuby1pZHItcmVjb3Zlcnktc2VpIGFuYWx5c2lzLXJldXNlLWxldmVsPTAgYW5hbHlzaXMtc2F2ZS1yZXVzZS1sZXZlbD0wIGFuYWx5c2lzLWxvYWQtcmV1c2UtbGV2ZWw9MCBzY2FsZS1mYWN0b3I9MCByZWZpbmUtaW50cmE9MCByZWZpbmUtaW50ZXI9MCByZWZpbmUtbXY9MSByZWZpbmUtY3R1LWRpc3RvcnRpb249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";
// Where Linux Play starts a Dolby Vision title (LINUX-PLAYER-PLAN.md §4.1). Saved in
// this browser only; the handoff itself lives with play() in preplay-selection.
function openInCard(){
  const linux=linuxPlayerClient(), current=openInPreference();
  const options=[["browser","This browser"],["player","plurx Player (Linux)"]];
  return setCard(`${cardHead("Dolby Vision playback on Linux","Use the native player for Dolby Vision on Linux. Saved in this browser.")}
    <div class="setfields"><div><label for="open-in">Open Dolby Vision titles in</label>
      <select id="open-in" ${linux?"":"disabled"} onchange="saveOpenIn(this)">${options.map(([value,label])=>`<option value="${value}" ${current===value?"selected":""}>${label}</option>`).join("")}</select></div></div>
    <p class="hint">${linux?"With plurx Player selected, only Dolby Vision titles open in plurx Player (mpv) on this Linux computer. Other titles play in this browser.":"Available only in a Linux browser. All titles play in this browser on this computer."} plurx Player preserves Dolby Vision that a browser cannot show. Browsing stays here; eligible Play actions use a one-time code. Signing out of the web app also stops plurx Player playback, because its access is revoked with this sign-in.</p>
    <div class="err" id="open-in-error" role="status"></div>`,{local:true});
}
function saveOpenIn(control){
  const err=document.getElementById("open-in-error");
  if(err)err.textContent="";
  if(!linuxPlayerClient()){control.value="browser";return;}
  const value=String(control.value);
  if(!["browser","player"].includes(value))return;
  try{
    if(value==="browser")localStorage.removeItem(OPEN_IN_KEY);
    else localStorage.setItem(OPEN_IN_KEY,"player");
    toast(value==="player"?"Dolby Vision titles now open in plurx Player on Linux":"Dolby Vision titles now play in this browser");
  }catch(e){
    control.value=openInPreference();
    if(err)err.textContent="This browser could not save where titles open.";
  }
}
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
// Graduated from Developer on 2026-11-18 (Paul): Dolby Vision and HDR
// processing, server delivery choices and Jellyfin compatibility.
function dolbyVisionHdrProcessingCard(settings,readiness){
  return setCard(`${cardHead("Dolby Vision processing","Use supported Dolby Vision picture information when delivering HDR or display-targeted SDR.","")}
    ${togRow("dv-hdr-processing","Process Dolby Vision for HDR and targeted SDR","Default off. Applies to new playback when processing is available.",!!settings.dolby_vision_hdr_processing)}
    <details class="setdetails"><summary>Details</summary><div class="setdetails-body">
    <p class="hint">New playback attempts supported Dolby Vision processing when the worker has the required tools and resources. Unsupported sources or processing failures use compatible playback. Native Dolby Vision and required color conversion are preserved. Readiness never prevents saving. A running movie is unchanged.</p>
    ${devReq(readiness,"dolby_vision_hdr_processing","dv_hdr_backend_reference","Qualified backend and reference","Validate HDR output against an independent reference.")}
    ${devReq(readiness,"dolby_vision_hdr_processing","dv_hdr_encoder_matrix","Production encoder matrix","Test processing with the encoders used by playback workers.")}
    ${devReq(readiness,"dolby_vision_hdr_processing","dv_hdr_bitrate_quality","Quality at delivery bitrates","Compare picture quality at delivery bitrates.")}
    ${devReq(readiness,"dolby_vision_hdr_processing","dv_hdr_physical_playback","Physical playback validation","Test HDR display output, startup, seeking, fallback and sustained playback on supported devices.")}
    </div></details>
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
    <details class="setdetails"><summary>Details</summary><div class="setdetails-body">
    <p class="hint">This preference never enables conversion by itself. New playback attempts FEL reconstruction for supported Profile 7 sources when the worker has the required tools and resources. Unsupported sources or processing failures keep the existing permitted conversion fallback. Native Dolby Vision is preserved. Readiness never prevents saving. A running movie is unchanged.</p>
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_backend_reference","Qualified backend and reference","The processing helper on this node, the conversion permission the reconstructed 8.1 replaces, and the composition proof against the helper's FEL master render.")}
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_worker_budget","Worker CPU budget for one live reconstruction","Where this node's renderer decodes, and whether one FEL compose + encode graph fits its software budget.")}
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_live_slot","Live reconstruction slot","One live FEL reconstruction per worker; a play beyond it gets the plain conversion with the reason.")}
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_encoder_matrix","Production encoder matrix","Test reconstruction and conversion with the encoders used by playback workers.")}
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_bitrate_quality","Quality at delivery bitrates","Compare picture quality at delivery bitrates with the existing conversion.")}
    ${devReq(readiness,"dolby_vision_fel_reencode","dv_fel_physical_playback","Physical playback validation","Test Profile 8.1 output, startup, seeking, fallback and sustained playback on supported Dolby Vision devices.")}
    </div></details>
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
// F+ curves (FEL-RPU-REFIT-BUILD.md): one switch per client platform the
// server can tell apart (playback_control::ClientPlatform). Any one on lets
// nodes fit Profile 7 FEL titles in the background; copy playback (milestone
// 4) serves the curves only to the platforms switched on.
const DV_REFIT_PLATFORMS=[["web","Web"],["apple","Apple"],["android","Android"],["linux","Linux (plurx Player)"]];
function dolbyVisionRefitCard(settings,readiness){
  const rows=DV_REFIT_PLATFORMS.map(([key,label])=>togRow(`dv-refit-${key}`,`Serve F+ curves to ${label} clients`,"Default off.",!!settings[`dolby_vision_refit_${key}`])).join("");
  return setCard(`${cardHead("Dolby Vision F+ curves","Fit each Profile 7 FEL title's Profile 8.1 mapping so copy playback carries its enhancement layer's effect.","")}
    ${rows}
    <details class="setdetails"><summary>Details</summary><div class="setdetails-body">
    <p class="hint">With any switch on, nodes fit Profile 7 FEL titles in the background, beside playback, and resume where they stopped when a viewer needs the capacity. A failed or cancelled title is not fitted again until its source changes or you retry it in Activity. Readiness never prevents saving.</p>
    <p><button class="ghost sm" id="dv-refit-spool-clear" onclick="clearDolbyVisionRefitSpool(this)">Delete this node's spool</button></p>
    ${devReq(readiness,"dolby_vision_refit_apple","refit_helper","Helper on this node","Whether this node's DV processing bundle supports --refit-stats.")}
    ${devReq(readiness,"dolby_vision_refit_apple","refit_coverage","Titles fitted","How many Profile 7 FEL titles have a curve table for their current source.")}
    ${devReq(readiness,"dolby_vision_refit_apple","refit_clients","Clients that apply the mapping","What each client platform was measured to do with a served 8.1 mapping.")}
    ${devReq(readiness,"dolby_vision_refit_apple","refit_spool","Statistics spool on this node","The node-local helper output fits are solved from, kept so a new fit method need not decode again.")}
    </div></details>
    <div class="err" id="dv-refit-error" role="alert"></div>${setCardFoot("saveDolbyVisionRefit")}`,{id:"dv-refit-card"});
}
async function saveDolbyVisionRefit(btn){
  const card=document.getElementById("dv-refit-card");
  const revision=Number(card&&card.dataset?card.dataset.revision||0:0);
  const err=document.getElementById("dv-refit-error");if(err)err.textContent="";if(btn)btn.disabled=true;
  try{
    /** @type {Record<string, unknown>} */
    const body={};
    for(const [key] of DV_REFIT_PLATFORMS)body[`dolby_vision_refit_${key}`]=(/** @type {HTMLInputElement} */ (document.getElementById(`dv-refit-${key}`))).checked;
    const saved=cacheSettings(await api("/settings",{method:"PUT",body}));
    if(!card||document.getElementById("dv-refit-card")!==card)return;
    if(Number(card.dataset&&card.dataset.revision||0)!==revision){toast("Earlier preference saved; newer edit remains unsaved");return;}
    card.outerHTML=dolbyVisionRefitCard(saved,DEVELOPER_READINESS);
    toast("F+ curve preferences saved");
  }catch(error){if(err)err.textContent=error.message||String(error);}finally{if(btn)btn.disabled=false;}
}
async function clearDolbyVisionRefitSpool(btn){
  const err=document.getElementById("dv-refit-error");if(err)err.textContent="";if(btn)btn.disabled=true;
  try{
    const done=await api("/developer/dv-refit/spool",{method:"DELETE"});
    toast(`Deleted ${Number(done&&done.removed||0)} spool files on this node`);
  }catch(error){if(err)err.textContent=error.message||String(error);}finally{if(btn)btn.disabled=false;}
}
function displayTargetHdr10Card(settings,readiness){
  return setCard(`${cardHead("Display-targeted HDR10-E / SDR","Map each shot to the viewer's display with Dolby display management and the title's creative trims.")}
    ${togRow("dv-display-target","Display-targeted HDR10-E / SDR","Default off. New HDR10-E sessions use a supported peak; explicit SDR displays use 100-nit BT.709 gamma 2.4. Unknown HDR peaks keep the master grade; ineligible SDR titles keep ordinary tone mapping.",!!settings.dolby_vision_display_target)}
    <details class="setdetails"><summary>Details</summary><div class="setdetails-body">
    ${devReq(readiness,"dolby_vision_display_target","dv_target_helper","Helper installed and HDR10-E enabled","Readings describe this node and never disable this switch or its Save.")}
    ${devReq(readiness,"dolby_vision_display_target","dv_target_peak","Client peak reports","Platform and viewer reports select the same bucket; the source is attribution only.")}
    ${devReq(readiness,"dolby_vision_display_target","dv_target_scan","Complete-title L1 scan","A whole title must have valid L1; missing facts keep the existing HDR master or ordinary SDR route.")}
    ${devReq(readiness,"dolby_vision_display_target","dv_target_cache","Bucket storage on this node","Targeted playback renders live. Pre-encodes and retained VOD stay master-only. Private roots retain existing reader and reservation fences.")}
    </div></details>
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

// Complete-output preparation queued by VOD starts. Three-valued, so it has
// its own selector rather than a readiness `setting` (the switch walk PUTs
// booleans). Off by default: one play must not start a whole-title encode.
function outputPreparationCard(settings,readiness){
  const mode=["off","copy","copy_and_encoded"].includes(settings.vod_output_preparation)?settings.vod_output_preparation:"off";
  const opt=(v,label)=>`<option value="${v}"${mode===v?" selected":""}>${label}</option>`;
  return setCard(`${cardHead("Complete-output preparation","Prepare a reusable copy or encode of a title after a VOD play.",mode==="off"?`<span class="pill">off</span>`:`<span class="pill warn">${esc(mode)}</span>`)}
    <label class="tog" for="vod-output-preparation"><span>What a VOD start may queue<small>Off queues nothing. Copy queues remux preparation. Copy and encoded also queues a whole-title background encode. Turning a kind off cancels its queued jobs within a minute.</small></span><select id="vod-output-preparation">${opt("off","Off")}${opt("copy","Copy")}${opt("copy_and_encoded","Copy and encoded")}</select></label>
    <details class="setdetails"><summary>Details</summary><div class="setdetails-body">
    ${devReq(readiness,"output_preparation","output_budget","A retained-output budget","Preparations publish into the retained registry, bounded by the cache budget (unset is 50 GB).")}
    ${devReq(readiness,"output_preparation","output_jobs","Queued and running preparation","Every job is in Settings → Jobs, filtered by kind, with a Cancel.")}
    ${devReq(readiness,"output_preparation","output_node_idle","This node's background encoder is idle","A claimed preparation yields to foreground and offline work.")}
    ${devReq(readiness,"output_preparation","output_stop","What Stop in Activity does","Stop cancels the preparation running on the node that answers; the next play of the title may queue it again.")}
    </div></details>
    <div class="err" id="vod-output-preparation-error" role="alert"></div>${setCardFoot("saveOutputPreparation")}`,{id:"vod-output-preparation-card"});
}
async function saveOutputPreparation(btn){
  const err=document.getElementById("vod-output-preparation-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{vod_output_preparation:/** @type {HTMLSelectElement} */(document.getElementById("vod-output-preparation")).value}}));
    const card=document.getElementById("vod-output-preparation-card");if(card)card.outerHTML=outputPreparationCard(saved,DEVELOPER_READINESS);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
function vodReorderCard(settings){
  return setCard(`${cardHead("Reordered VOD frames","Allow two B-frames in software x264 VOD encodes.","")}
    ${togRow("vod-reorder","Enable reordered VOD frames","Saves the requested recipe directly. Other encoders retain their existing frame order.",Number(settings.vod_reorder_frames)===2)}
    <details class="setdetails"><summary>Details</summary><div class="setdetails-body">
    ${devStaticReq("Supported encoder","software x264","Hardware encoder qualification is separate. Reordered frames require both decode and presentation timestamps in the VOD fragments.","")}
    ${devStaticReq("Physical-client qualification","pending","Browser and native-client presentation continuity and measured compression receipts remain to be recorded.","warn")}
    </div></details>
    <div class="err" id="vod-reorder-error" role="alert"></div>${setCardFoot("saveVodReorder")}`,{id:"vod-reorder-card"});
}
async function saveVodReorder(btn){
  const err=document.getElementById("vod-reorder-error");if(err)err.textContent="";btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{vod_reorder_frames:(/** @type {HTMLInputElement} */ (document.getElementById("vod-reorder"))).checked?2:0}}));
    const card=document.getElementById("vod-reorder-card");if(card)card.outerHTML=vodReorderCard(saved);
  }catch(error){if(err)err.textContent=error.message;}finally{btn.disabled=false;}
}
function jellyfinCompatibilityCard(settings,readiness){
  const on=!!settings.jellyfin_compatibility_enabled;
  const connection=new URL("/jellyfin",window.location.href).href;
  const host=window.location.hostname;
  return setCard(`${cardHead("Jellyfin client compatibility","Connect the tested Infuse and Jellyfin Android TV clients to this server.",`<span class="pill${on?" ok":""}">${on?"enabled":"off"}</span>`)}
    <label class="checkrow"><input type="checkbox" id="jellyfin-compatibility-enabled" ${on?"checked":""}><span>Allow Jellyfin clients</span></label>
    <p class="hint">In the client, enter just this server's address: <code>${esc(host)}</code>. Jellyfin clients add their standard port (8096) themselves. If port 8096 is not reachable, use <code>${esc(connection)}</code>, which always works. Sign in with a Plurx account.</p>
    <details class="setdetails"><summary>Details</summary><div class="setdetails-body">
    <p class="hint">Library posters and backdrops are served without sign-in while this is on: anyone who has an artwork link can read that image. A direct-play link for one title chosen after sign-in also works without a login header; it expires within 24 hours and stops working when playback stops or the login is revoked. Everything else requires sign-in.</p>
    ${devReq(readiness,"jellyfin_compatibility","client_qualification","Pinned clients qualified","The complete browsing, playback, track and recovery matrix must pass on the frozen candidate.")}
    ${devReq(readiness,"jellyfin_compatibility","standard_port","Reachable by host alone","Jellyfin clients given only a host connect to port 8096 at the server root.")}
    <p class="devcheck-note">Readiness is advisory. Your saved choice controls the connection surface.</p></div></details>
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
