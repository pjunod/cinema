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
async function saveSubtitleStoredSources(btn){
  const err=document.getElementById("subsrcerr"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    const saved=await api("/settings",{method:"PUT",body:{
      subtitle_stored_sources:document.getElementById("subsrc").checked}});
    cacheSettings(saved);
    toast("Stored subtitle track setting saved"); if(btn) setCardSaved(btn);
    const card=document.getElementById("subsrccard");
    if(card) card.outerHTML=subtitleStoredSourcesCard(saved,DEVELOPER_READINESS);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
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
async function saveSubtitleClusterSources(btn){
  const err=document.getElementById("subclustererr");err.textContent="";if(btn)btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{subtitle_cluster_sources:document.getElementById("subcluster").checked}}));
    const card=document.getElementById("subclustercard");if(card)card.outerHTML=subtitleClusterSourcesCard(saved,DEVELOPER_READINESS);
    toast("Cluster subtitle source setting saved");if(btn)setCardSaved(btn);
  }catch(error){err.textContent=error.message;if(btn)btn.disabled=false;}
}
async function saveSubtitleBackfill(btn){
  const err=document.getElementById("subbackfillerr");err.textContent="";if(btn)btn.disabled=true;
  try{
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{subtitle_backfill:document.getElementById("subbackfill").checked}}));
    const card=document.getElementById("subbackfillcard");if(card)card.outerHTML=subtitleBackfillCard(saved,DEVELOPER_READINESS);
    toast("Subtitle backfill setting saved");if(btn)setCardSaved(btn);
  }catch(error){err.textContent=error.message;if(btn)btn.disabled=false;}
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
