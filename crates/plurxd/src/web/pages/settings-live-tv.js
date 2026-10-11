"use strict";
// The unsaved state of the guide panel. `null` means "no edit yet, show what
// is saved"; a value means the operator has chosen something the server has
// not been told about.
const LIVE_TV_GUIDE_DRAFT={source:null,url:null,hours:null,checked:null,revision:0};
function liveTvGuideFieldDraft(){
  const url=document.getElementById("ltgurl");
  const hours=document.getElementById("ltghours");
  if(url) LIVE_TV_GUIDE_DRAFT.url=url.value;
  if(hours) LIVE_TV_GUIDE_DRAFT.hours=Number(hours.value);
  LIVE_TV_GUIDE_DRAFT.revision+=1;
}
function liveTvGuideSourceDraft(value){
  liveTvGuideFieldDraft();
  LIVE_TV_GUIDE_DRAFT.source=value;
  // The card repaint replaces the readiness mount too. Let the replacement run
  // one current check instead of leaving its fresh "Checking…" row forever.
  LIVE_TV_GUIDE_DRAFT.checked=null;
  const card=document.getElementById("live-tv-guide-settings");
  if(card&&card.isConnected) card.outerHTML=liveTvGuideCard(SETTINGS);
  // `setCard`'s delegated change handler belongs to the card that the repaint
  // just removed. Mark the replacement card explicitly, or its Save remains
  // disabled even though the newly selected source is still visible.
  const replacement=document.getElementById("ltgsrc");
  markSetCard(replacement);
  if(replacement) replacement.focus();
}
function liveTvGuideCard(s){
  // The guide's own enable section. Everything here is advisory: it says what
  // has to be true for a source to work and whether it is true right now, and
  // then it lets the operator turn it on anyway. A feature that refuses to
  // switch on is a feature nobody can diagnose, and the guide takes no tuner
  // and changes no playback — a wrong setting leaves the rows blank, nothing
  // more. (Keep this comment inside the body: a leading comment lands in the
  // previous function's slice in tests/web/settings-sections.test.js.)
  // The dropdown edits a draft, not the saved settings. `onchange` repaints
  // this card from the draft, so reading the saved value here would snap
  // the control straight back and the XMLTV URL field — which only exists
  // while the draft says xmltv — could never be reached at all.
  const source=(LIVE_TV_GUIDE_DRAFT.source!==null?LIVE_TV_GUIDE_DRAFT.source:s.live_tv_guide_source)||"off";
  const on=source!=="off";
  const pill=on?`<span class="pill acc">${source==="xmltv"?"XMLTV":"HDHomeRun guide"}</span>`:`<span class="pill">Off</span>`;
  const opt=(value,label)=>`<option value="${value}"${source===value?' selected':''}>${label}</option>`;
  // The panel said "Checking…" forever because nothing ever checked. Do it
  // once per settings render — not on every draft keystroke, since readiness
  // builds a guide document on the owner.
  const renderId=typeof PAGE_RENDER_GENERATION==="undefined"?null:PAGE_RENDER_GENERATION;
  if(renderId!==null&&LIVE_TV_GUIDE_DRAFT.checked!==renderId){
    LIVE_TV_GUIDE_DRAFT.checked=renderId;
    queueMicrotask(()=>{ const mount=document.getElementById("ltgready"); if(mount&&mount.isConnected) checkLiveTvGuide(); });
  }
  return setCard(`${cardHead("Programme guide","What is on each channel now and next. Read-only: no recording, no reminders, no scheduling.",pill)}
    <p class="hint">The guide is optional and off by default. Enabling it lets the guide worker contact the selected guide source.</p>
    <details class="setdetails"><summary>Guide sources and privacy</summary><div class="setdetails-body"><div class="hint"><b>Guide requests are sent by the guide worker.</b> The HDHomeRun guide reads your device's <code>DeviceAuth</code> fresh at each refresh, sends it to <code>api.hdhomerun.com</code> over TLS, and forgets it — plurx never stores, logs or relays that credential, and only the guide worker ever holds it. XMLTV fetches the one URL you give and nothing else.</div></div></details>
    <div class="setfields">
      <div><label for="ltgsrc">Source</label><select id="ltgsrc" onchange="liveTvGuideSourceDraft(this.value)">${opt("off","Off")}${opt("hdhomerun","HDHomeRun guide")}${opt("xmltv","XMLTV")}</select></div>
      ${source==="xmltv"?`<div style="flex:1 1 340px"><label for="ltgurl">XMLTV URL</label><input id="ltgurl" value="${esc(LIVE_TV_GUIDE_DRAFT.url!==null?LIVE_TV_GUIDE_DRAFT.url:(s.live_tv_xmltv_url||""))}" placeholder="https://example/guide.xml.gz" maxlength="1024" oninput="liveTvGuideFieldDraft()"><div class="hint">Plain or gzipped. Matched to your lineup by display name, then LCN, then callsign.</div></div>`:""}
      <div><label for="ltghours">Look ahead</label><select id="ltghours" onchange="liveTvGuideFieldDraft()">${[4,8,12,24,48,72].map(n=>`<option value="${n}"${n===Number(LIVE_TV_GUIDE_DRAFT.hours!==null?LIVE_TV_GUIDE_DRAFT.hours:s.live_tv_guide_hours)?' selected':''}>${n} hours</option>`).join("")}</select></div>
    </div>
    <h2 class="section">Guide status</h2>
    <p class="hint"><b>Saved-configuration evidence.</b> These checks describe the source and look-ahead already stored by the server. An unsaved source or URL draft is not checked until you save it.</p>
    <div id="ltgready" aria-live="polite"><p class="muted">Checking…</p></div>
    <div class="err" id="ltgerr" role="alert"></div>

    ${setCardFoot("saveLiveTvGuide")}
    <div class="row"><button class="ghost sm" onclick="checkLiveTvGuide(this)">Check the guide</button><button class="ghost sm" onclick="refreshLiveTvGuide(this)">Refresh now</button><a href="#/live-tv">Browse channels</a></div>`,{id:"live-tv-guide-settings"});
}
async function saveLiveTvGuide(button){
  liveTvGuideFieldDraft();
  const revision=LIVE_TV_GUIDE_DRAFT.revision;
  const source=document.getElementById("ltgsrc").value;
  const url=document.getElementById("ltgurl");
  // A successful write repaints with the server's new source. Arm that paint
  // to verify the saved source rather than retaining the pre-save check id.
  LIVE_TV_GUIDE_DRAFT.checked=null;
  const saved=await liveTvSettingsWrite({live_tv_config_generation:SETTINGS.live_tv_config_generation,
    live_tv_guide_source:source,
    live_tv_xmltv_url:url?url.value.trim():(SETTINGS.live_tv_xmltv_url||""),
    live_tv_guide_hours:Number(document.getElementById("ltghours").value)},button,"ltgerr");
  // Only a save that landed retires the draft; a refused one keeps what the
  // operator typed so they can correct it rather than retype it.
  if(saved&&LIVE_TV_GUIDE_DRAFT.revision===revision){
    LIVE_TV_GUIDE_DRAFT.source=null;
    LIVE_TV_GUIDE_DRAFT.url=null;
    LIVE_TV_GUIDE_DRAFT.hours=null;
    if(typeof saved==="object") replaceLiveTvCard("live-tv-guide-settings",liveTvGuideCard(saved),"ltgsrc");
  }
  return !!saved;
}
function liveTvGuideReadyView(r){
  // Advisory rendering: a red row is a sentence, never a disabled Save.
  const source=r.source==="xmltv"?"XMLTV":r.source==="hdhomerun"?"HDHomeRun guide":"Off";
  const saved=`<p class="hint"><b>Saved configuration checked:</b> ${source} · ${Number(r.guide_hours)} hour look-ahead.</p>`;
  const rows=(r.checks||[]).map(c=>`<div class="tog"><span>${c.ready?"Met":"Not met yet"}<small>${esc(c.message)}</small></span></div>`).join("");
  const fresh=r.freshness==="fresh"?`<span class="pill ok">fresh · ${Math.round(r.age_seconds/60)} min old</span>`
    :r.freshness==="stale"?`<span class="pill warn">stale · ${Math.round(r.age_seconds/60)} min old</span>`
    :`<span class="pill">no data cached</span>`;
  const counts=r.source==="off"?"":`<p>${fresh} · matched ${r.matched_channels} of ${r.lineup_channels} lineup channels · ${r.programmes} programmes cached · refreshes every ${Math.round(r.refresh_interval_seconds/60)} minutes.</p>`;
  const error=r.refresh_error?`<p class="hint"><b>Last refresh failed:</b> ${esc(r.refresh_error)}</p>`:"";
  return `${saved}${rows}${counts}${error}<div class="hint"><b>None of this blocks the switch.</b> Every row above is advice. Save any source you like and read the result here; the guide takes no tuner, so a wrong setting leaves channel rows blank and nothing else.</div>`;
}
async function checkLiveTvGuide(button){
  const generation=PAGE_RENDER_GENERATION;
  const mount=document.getElementById("ltgready");
  if(button) button.disabled=true;
  try{
    const r=await api("/live-tv/guide/readiness",{signal:AbortSignal.timeout(30000)});
    if(!settingsCurrent(generation,"livetv")||!mount||!mount.isConnected) return;
    mount.innerHTML=liveTvGuideReadyView(r);
  }catch(e){ if(settingsCurrent(generation,"livetv")&&mount&&mount.isConnected) mount.textContent=e.message; }
  finally{ if(button&&button.isConnected) button.disabled=false; }
}
async function refreshLiveTvGuide(button){
  const generation=PAGE_RENDER_GENERATION;
  const error=document.getElementById("ltgerr"); if(error) error.textContent="";
  button.disabled=true;
  try{
    await api("/live-tv/guide/refresh",{method:"POST",signal:AbortSignal.timeout(45000)});
    if(settingsCurrent(generation,"livetv")) await checkLiveTvGuide(null);
  }catch(e){
    const currentError=document.getElementById("ltgerr");
    if(settingsCurrent(generation,"livetv")&&currentError&&currentError.isConnected) currentError.textContent=e.message;
  }
  finally{ if(button.isConnected) button.disabled=false; }
}
function liveTvPanel(settings,readiness){
  return `${setHead("Live TV","Turn Live TV on, and manage your tuner, programme guide, recordings and library channels.",`<a class="ghost sm" href="#/live-tv" style="text-decoration:none">Browse channels ↗</a>`)}${liveTvEnableCard(settings)}${liveTvSettingsCard(settings)}${liveTvDeinterlaceCard(settings)}${liveTvGuideCard(settings)}${dvrCard(settings,readiness)}${libraryChannelsSettingsCard(settings,readiness)}`;
}
function liveTvSettingsCard(s){
  return setCard(`${cardHead("HDHomeRun Live TV","One network device shared by eligible cluster servers.")}
    <p>Viewers and recordings on the same channel share one tuner stream. A session stays on its selected server; the tuner itself belongs to the cluster.</p>
    <div class="setfields">
      <div><label for="ltip">Tuner private IPv4</label><input id="ltip" value="${esc(s.live_tv_device_ipv4||"")}" placeholder="10.42.4.100" inputmode="decimal"></div>
      <div><label for="ltlimit">Maximum channel streams</label><select id="ltlimit">${[1,2,3,4].map(n=>`<option value="${n}"${n===Number(s.live_tv_max_sessions)?' selected':''}>${n}</option>`).join("")}</select></div>
      <div><label for="ltheight">Maximum quality</label><select id="ltheight">${[[0,"Original / Auto"],[480,"480p ceiling"],[720,"720p ceiling"],[1080,"1080p ceiling"],[2160,"2160p ceiling"]].map(([n,label])=>`<option value="${n}"${n===Number(s.live_tv_max_output_height||0)?' selected':''}>${label}</option>`).join("")}</select></div>
    </div>
    <p class="hint">Saving changes ends streams using the previous configuration. The on/off switch and its advisory prerequisites are the Live TV card above.</p>
    <div id="ltreadiness" aria-live="polite"></div><div class="err" id="lterr" role="alert"></div>
    ${setCardFoot("saveLiveTvSettings")}
    <div class="row"><button class="ghost sm" onclick="checkLiveTvReadiness(this)">Check saved configuration</button><a href="#/live-tv">Browse channels</a></div>`,{id:"live-tv-settings"});
}
function replaceLiveTvCard(cardId,html,focusId){
  const card=document.getElementById(cardId);
  if(!card||!card.isConnected) return;
  card.outerHTML=html;
  const focus=document.getElementById(focusId);
  if(focus) focus.focus();
}
async function liveTvSettingsWrite(body,button,errorId){
  if(!ME||!ME.is_admin) return false;
  const generation=PAGE_RENDER_GENERATION;
  if(button) button.disabled=true;
  // The caller names its own error slot: a failed guide save was reporting
  // itself under the tuner card, which is a different card about a different
  // setting.
  const slotId=errorId||"lterr";
  const error=document.getElementById(slotId); if(error) error.textContent="";
  try{
    const result=await api("/settings",{method:"PUT",body,signal:AbortSignal.timeout(45000)});
    // Settings section switches reuse the aggregate cache. Persist the server
    // snapshot even when this response must not touch the destination DOM, or
    // returning to Live TV can show an old generation and conflict on save.
    cacheSettings(result);
    if(!settingsCurrent(generation,"livetv")) return result;
    toast("Live TV settings saved");
    return result;
  }catch(e){
    if(settingsCurrent(generation,"livetv")){
      // A guide source edit replaces its card while this request is pending.
      // Resolve the owning slot now instead of writing into the detached node
      // captured before the await.
      const currentError=document.getElementById(slotId);
      if(currentError&&currentError.isConnected) currentError.textContent=e.code==="settings_conflict"?"The configuration changed elsewhere. Reload this section before trying again.":e.message;
      if(button&&button.isConnected) button.disabled=false;
    }
    return false;
  }
}
async function saveLiveTvSettings(button){
  const saved=await liveTvSettingsWrite({live_tv_config_generation:SETTINGS.live_tv_config_generation,
    live_tv_device_ipv4:document.getElementById("ltip").value.trim(),
    live_tv_max_sessions:Number(document.getElementById("ltlimit").value),
    live_tv_output_height:720,
    live_tv_max_output_height:Number(document.getElementById("ltheight").value)},button);
  if(saved&&typeof saved==="object") replaceLiveTvCard("live-tv-settings",liveTvSettingsCard(saved),"ltip");
  return !!saved;
}
async function checkLiveTvReadiness(button){
  const generation=PAGE_RENDER_GENERATION;
  const mount=document.getElementById("ltreadiness");
  button.disabled=true; mount.textContent="Checking the saved cluster configuration…";
  try{
    const r=await api("/live-tv/readiness/refresh",{method:"POST",signal:AbortSignal.timeout(40000)});
    if(!settingsCurrent(generation,"livetv")||!mount.isConnected) return;
    mount.innerHTML=`<h3>${r.ready?"Ready to enable":"Needs attention"}</h3><ul>${r.checks.map(c=>`<li><strong>${c.ready?"Pass":"Not ready"}:</strong> ${esc(c.message)}</li>`).join("")}</ul>${r.snapshot?`<p>${esc(r.snapshot.device.friendly_name)} · ${esc(r.snapshot.device.model_number)} · ${esc(r.snapshot.device.tuner_count)} tuners</p>`:""}<p class="hint">This result describes saved generation ${esc(r.generation)}. It is advisory: enabling is not blocked by a red check. Invalid addresses and stale configuration versions still refuse the save.</p>`;
  }catch(e){ if(settingsCurrent(generation,"livetv")&&mount.isConnected) mount.textContent=e.message; }
  finally{ if(button.isConnected) button.disabled=false; }
}

function dvrCard(settings,readiness){
  return setCard(`${cardHead("Recording","Capture a programme from the tuner to a disk, and let the library scan pick it up.",settings.dvr_enabled?`<span class="pill ok">enabled</span>`:`<span class="pill">disabled</span>`)}
      ${togRow("dvrenabled","Enable recording","This checkbox is authoritative. Reminders, rules and the schedule stay readable while it is off, so a deployment can be inspected and repaired before the first capture.",settings.dvr_enabled)}
      <div class="setfields">
        <div><label for="dvrroot">Recording folder</label><input id="dvrroot" value="${esc(settings.dvr_root||"")}" placeholder="/srv/plurx/recordings"><div class="hint">The guide worker writes here, and every node that serves a recording has to be able to read it.</div></div>
        <div><label for="dvrfloor">Keep free (GB)</label><input id="dvrfloor" type="number" min="0" max="1000000" value="${esc(String(settings.dvr_free_floor_gb??50))}"><div class="hint">A capture never starts under the floor; it shows the conflict instead of filling the disk.</div></div>
        <div><label for="dvrreserve">Tuners reserved for viewing</label><select id="dvrreserve">${[0,1,2,3,4].map(n=>`<option value="${n}"${n===Number(settings.dvr_tuner_reserve??1)?" selected":""}>${n}</option>`).join("")}</select><div class="hint">Recordings never take the last reserved session, so a schedule cannot lock the household out of the tuner.</div></div>
        <div><label for="dvrpadstart">Start early (seconds)</label><input id="dvrpadstart" type="number" min="0" max="3600" value="${esc(String(settings.dvr_pad_start_s??60))}"></div>
        <div><label for="dvrpadend">Run late (seconds)</label><input id="dvrpadend" type="number" min="0" max="3600" value="${esc(String(settings.dvr_pad_end_s??120))}"></div>
        <div><label for="dvrlead">Reminder lead (seconds)</label><input id="dvrlead" type="number" min="0" max="3600" value="${esc(String(settings.dvr_reminder_lead_s??300))}"><div class="hint">How long before a programme starts a reminder appears. “Watch at” sets one with no lead at all.</div></div>
        <div><label for="dvrwebhook">Webhook URL</label><input id="dvrwebhook" value="${esc(settings.dvr_webhook_url||"")}" placeholder="https://example.invalid/hook"><div class="hint">Optional. https anywhere, or plain http only to a private address.</div></div>
      </div>
      <details class="setdetails"><summary>Requirements and readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"dvr","dvr_root_writable","Writable recording folder","The guide worker creates and removes a probe file under the folder. A read-only remount, a full filesystem and an ACL that says yes but means no all look identical to a permission bit.")}
      ${devReq(readiness,"dvr","dvr_free_space","Room above the floor","Free space on the recording folder against the floor set above. Read this row on the guide worker; another node cannot see that filesystem.")}
      ${devReq(readiness,"dvr","guide_horizon","A guide worth scheduling from","A four-hour guide can only record what is nearly on. Series rules, a schedule and a conflict worth resolving all need a horizon.")}
      ${devReq(readiness,"dvr","tuner_reserve","A tuner stays free for watching","Reserving every session for viewing means no capture could ever start; reserving none means a schedule can take the last tuner.")}
      ${devReq(readiness,"dvr","every_node_mounts_root","Every node can read the folder","The owner writes recordings and any node may serve them, so the folder has to be a mount every node has. This process can see its own filesystem and no peer's.")}
      ${settings.dvr_webhook_url?devReq(readiness,"dvr","webhook_url_approved","An approved webhook URL","The outbound policy allows https anywhere and plain http only to a private address. An unapproved URL is never called."):""}
      <p class="devcheck-note">Advisory only: no result disables the switch or overrides your saved choice. An operator who can see the deployment may turn recording on over any amount of red.</p>
      </div></details>
      <div class="err" id="dvrerr" role="alert"></div>${setCardFoot("saveDvrSettings")}`);
}

function libraryChannelsSettingsCard(settings,readiness){
  return setCard(`${cardHead("Library channels","Build always-on schedules from movies and episodes in your library.",settings.library_channels_enabled?`<span class="pill ok">enabled</span>`:`<span class="pill">disabled</span>`)}
      ${togRow("lcsubjectenabled","Enable subject matching","Runs local catalogue rules. Saving and existing playback remain available when paused.",settings.library_channel_subject_matching_enabled!==false)}
      ${devReq(readiness,"library_channel_subject_matching","provider","Local catalogue matcher","Text, metadata and classification rules run inside plurx without an inference service.")}
      ${devReq(readiness,"library_channel_subject_matching","metadata","Metadata coverage","Missing or vague descriptions can remain uncertain and can be included manually.")}
      ${devReq(readiness,"library_channel_subject_matching","batch","Recent batch and queued work","Operational failures never become negative classifications.")}
      ${togRow("lcenabled","Enable Library-channel playback","This checkbox is authoritative. Listing, preview and authoring stay available while off so readiness can be inspected and repaired.",settings.library_channels_enabled)}
      <details class="setdetails"><summary>Requirements and readiness</summary><div class="setdetails-body">
      ${devReq(readiness,"library_channels","authoritative_store","Authoritative Store","Definitions, schedules and favourites must come from current authority rather than a stale node-local copy.")}
      ${devReq(readiness,"library_channels","eligible_video","Schedulable video","A successful probe, video stream and positive duration are needed to publish a non-empty rotation. A draft may still be saved without one.")}
      ${devReq(readiness,"library_channels","compatible_clients","Compatible clients","Old clients retain VOD and Live TV, but do not know the dedicated channel-session purpose or boundary controller.")}
      <p class="devcheck-note">Advisory only: no result disables the switch or overrides your saved choice. No tuner, metadata service, AI key, or external provider is required.</p>
      </div></details><div class="err" id="lcdeverr" role="alert"></div>${setCardFoot("saveLibraryChannelsSettings")}`);
}

// The on/off switch and the deinterlace cadence. Both waited in Developer
// until 2026-10-10; they are permanent Live TV settings now.
function liveTvEnableCard(settings){
  queueMicrotask(()=>{const mount=document.getElementById("lt-enable-readiness");if(mount)refreshLiveTvEnableReadiness(mount);});
  return setCard(`${cardHead("Live TV","Turn the network tuner on or off for every eligible server.",`<span class="pill">${settings.live_tv_enabled?"enabled":"off"}</span>`)}
    ${togRow("lt-enable","Enable Live TV","This saved choice is authoritative. Readiness observations never disable the control.",!!settings.live_tv_enabled)}
    <p class="hint">${settingsClusterEnabled()?"For safe operation, upgrade cluster servers, keep their clocks synchronized, and keep a ready voter majority. At least one server needs tuner connectivity and writable scratch. DVR workers need writable recording storage; matching paths alone do not prove shared storage.":"This server needs tuner connectivity and writable scratch. DVR recordings need writable recording storage."}</p>
    <div id="lt-enable-readiness" aria-live="polite">Checking current prerequisites…</div>
    <p class="hint">A lost worker can leave a physical tuner connection behind temporarily. Other free tuners remain available. DRM remains unsupported.</p>
    <div id="lt-enable-error" class="err" role="alert"></div>${setCardFoot("saveLiveTvEnable")}`,{id:"live-tv-enable"});
}
async function refreshLiveTvEnableReadiness(mount){
  try{
    const r=await api("/live-tv/readiness",{signal:AbortSignal.timeout(40000)});
    if(!mount.isConnected)return;
    mount.innerHTML=`<ul>${r.checks.map(c=>`<li><strong>${c.ready?"Met":"Not met"}:</strong> ${esc(c.message)}</li>`).join("")}</ul><p class="hint">Advisory only. Unknown or unmet requirements do not prevent enabling.</p>`;
  }catch(e){if(mount.isConnected)mount.textContent=`Readiness unavailable: ${e.message}. You can still enable Live TV.`;}
}
async function saveLiveTvEnable(button){
  // The same writer as the tuner card: it fences a reply that lands after the
  // operator left Live TV, and names a configuration-generation conflict.
  const saved=await liveTvSettingsWrite({live_tv_config_generation:SETTINGS.live_tv_config_generation,
    live_tv_enabled:/** @type {HTMLInputElement} */ (document.getElementById("lt-enable")).checked},button,"lt-enable-error");
  if(saved&&typeof saved==="object") replaceLiveTvCard("live-tv-enable",liveTvEnableCard(saved),"lt-enable");
  return !!saved;
}
function liveTvDeinterlaceCard(settings){
  const selected=settings.live_tv_deinterlace_output==="frame"?"frame":"field";
  return setCard(`${cardHead("Live TV deinterlace cadence","Choose whether software deinterlacing preserves frame cadence or emits one frame per field.",`<span class="pill">${selected==="field"?"Field rate":"Frame rate"}</span>`)}
      <label for="ltdeint">Output cadence</label>
      <select id="ltdeint"><option value="field"${selected==="field"?" selected":""}>Field rate (smoother motion)</option><option value="frame"${selected==="frame"?" selected":""}>Frame rate (lower bitrate)</option></select>
      <details class="setdetails"><summary>Prerequisites and current status</summary><div class="setdetails-body">
      ${devStaticReq("Current selection",selected==="field"?"field rate":"frame rate","Applied to subsequent Live TV sessions that require software deinterlacing.","ok")}
      ${devStaticReq("Source identification","required","The tuner probe must report one of tt, bb, tb or bt. Unknown and future tokens remain unknown and do not opt into deinterlacing.","")}
      ${devStaticReq("Encoder readiness","not measured on this node","Field rate asks the software H.264 path to sustain up to 59.94 fps. Check the live FFmpeg log and playback stability on the target device.","warn")}
      <p class="devcheck-note">Advisory only. Readiness never disables this choice and this setting never gates Live TV. If the player can copy an interlaced source, no deinterlace filter is inserted.</p>
      </div></details><div class="err" id="ltdeinterr" role="alert"></div>
      ${setCardFoot("saveLiveTvDeinterlace")}`,{id:"live-tv-deinterlace"});
}
async function saveLiveTvDeinterlace(btn){
  const err=document.getElementById("ltdeinterr");if(err)err.textContent="";btn.disabled=true;
  try{
    const output=(/** @type {HTMLSelectElement} */ (document.getElementById("ltdeint"))).value;
    const saved=cacheSettings(await api("/settings",{method:"PUT",body:{live_tv_deinterlace_output:output}}));
    const card=btn.closest(".setcard");if(card)card.outerHTML=liveTvDeinterlaceCard(saved);
    toast("Live TV deinterlace cadence saved");
  }catch(e){if(err)err.textContent=e.message||String(e);btn.disabled=false;}
}
