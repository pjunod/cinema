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
      playback_auto_abr:document.getElementById("pabr").checked,
      hls_readrate:document.getElementById("phr").value,
      hls_burst_secs:document.getElementById("phb").value,
      hls_ahead_max_secs:document.getElementById("pha").value,
      vod_presentation:document.getElementById("pvod").checked,
      vod_working_set_bytes:document.getElementById("pvws").value,
      vod_block_budget_secs:"8",
      vod_materialize_budget_secs:document.getElementById("pvmb").value,
      vod_blocked_get_cap:document.getElementById("pvbg").value}}));
    if(SERVER) SERVER.playback_auto_abr=SETTINGS.playback_auto_abr;
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
    // Replace this card only. A full panel re-render would rebuild the four
    // cards beside it and silently discard anything typed into them — the Live
    // TV card next door stages a whole configuration before its own Save.
    const card=document.getElementById("vdcard");
    if(card) card.outerHTML=verifiedDecodeCard(saved);
  }catch(e){ err.textContent=e.message; if(btn) btn.disabled=false; }
}
async function saveSubtitleNotReady(btn){
  const err=document.getElementById("sub503err"); err.textContent="";
  if(btn) btn.disabled=true;
  try{
    const saved=await api("/settings",{method:"PUT",body:{
      subtitle_not_ready_503:document.getElementById("sub503").checked}});
    cacheSettings(saved);
    toast("Subtitle refusal setting saved"); if(btn) setCardSaved(btn);
    const card=document.getElementById("sub503card");
    if(card) card.outerHTML=subtitleNotReadyCard(saved,DEVELOPER_READINESS);
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
      pgs_overlay:document.getElementById("pgsoverlay").checked}});
    cacheSettings(saved);
    toast("PGS overlay setting saved"); if(btn) setCardSaved(btn);
    const card=document.getElementById("pgsoverlaycard");
    if(card) card.outerHTML=pgsOverlayCard(saved,DEVELOPER_READINESS);
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
