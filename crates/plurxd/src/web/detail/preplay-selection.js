"use strict";
// ---- pre-play audio / subtitle selection ----------------------------------
// A choice made on the detail screen, before there is a player at all. It
// belongs to ONE playback: nothing here writes Settings → Playback defaults,
// and loadItem() empties the store on the way into another item, so a French
// track picked for one film is not silently picked for the next.
//
// Keyed by file, not by item. A multi-version item's two files have different
// streams, and one item-wide choice would be an index into the wrong file.
let PREPLAY={};
// Replaced, never mutated: prePlayPreview() compares identity after its await
// to notice that a later change superseded the answer it is holding.
function prePlaySelection(fileId){ return PREPLAY[fileId]||null; }
function clearPrePlay(){ PREPLAY={}; }
// Which selection a play() call runs with. An internal reopen — a quality
// change, a subtitles-off restart — is still THIS playback, and it keeps the
// tracks already on screen rather than falling back to the cold-start policy
// default. Anything else is a cold start and takes the detail screen's pickers.
//
// "Still this playback" is the whole predicate, and closing the player ends it.
// closePlayer() deliberately does not replace PLAYER, so without its matching
// `preplay=null` the identity check below would keep matching after the player
// was closed: playing the same file again would silently reuse the last
// playback's tracks while both pickers on the detail screen — reset by
// loadItem() — read Default, and an explicitly changed picker would be ignored
// for the rest of the session. The carry belongs to an open playback, not to
// the file.
function playbackSelection(player, fileId){
  return (player&&player.fileId===fileId&&player.preplay)
    ? player.preplay : prePlaySelection(fileId);
}
function prePlaySelectionQuery(sel){
  if(!sel) return "";
  return (sel.audio!=null?"&audio="+sel.audio:"")+(sel.subtitle!=null?"&subtitle="+sel.subtitle:"");
}
// Every `/decision` this app makes, so the selection cannot be wired into some
// of them and forgotten in the rest. An omitted parameter is load-bearing: a
// request that sends no `audio=` keeps its previous verdict byte-for-byte
// (docs/PLAYBACK.md), which is what stops an untouched picker from turning a
// direct play into a remux for nothing.
function decisionUrl(fileId, force, sel){
  return `/files/${fileId}/decision?${CAPS_Q}&force=${force}${prePlaySelectionQuery(sel)}`;
}
// The same question, with the capabilities as a document.
//
// POST rather than GET only because the body is JSON; it reads nothing and
// writes nothing. `force`, `audio` and `subtitle` stay on the query string —
// they are request-local choices, not capabilities.
//
// A legacy document may still fall back to GET on the old compatibility
// statuses. A document carrying the progressive packaging constraint never
// does: dropping even [] would silently turn a restrictive claim into the
// unrestricted legacy path. Serving nodes therefore roll before this client.
async function askDecision(fileId, force, sel, signal=null){
  const query=`force=${force}${prePlaySelectionQuery(sel)}`;
  const caps=currentCapsDocument();
  try{
    const decision=await api(`/files/${fileId}/decision?${query}`,
      {method:"POST", body:{caps},signal});
    // The create must act on the exact settled snapshot that produced this
    // decision, even if the page refreshes capability state in between.
    Object.defineProperty(decision,"_capsSnapshot",{value:caps,enumerable:false});
    return decision;
  }catch(e){
    const constrained=Object.prototype.hasOwnProperty.call(
      caps,"progressive_hevc_sample_entries")
      &&caps.progressive_hevc_sample_entries!==null;
    if(!constrained && e && (e.status===404 || e.status===405 || e.status===400)
      &&e.code!=="invalid_capabilities"&&e.code!=="unsupported_hevc_delivery"){
      return api(decisionUrl(fileId, force, sel),{signal});
    }
    throw e;
  }
}
// `null` for a field means "no explicit choice" and is NOT the same as picking
// the track the server would have picked anyway: a request that sends no
// `audio=` keeps its previous verdict byte-for-byte (docs/PLAYBACK.md), and
// that is what stops an untouched picker from turning a direct play into a
// remux for no reason.
function setPrePlay(fileId, kind, raw){
  const cur=PREPLAY[fileId]||{audio:null,subtitle:null};
  const next=Object.assign({},cur);
  next[kind] = raw===""||raw==null ? null : Number(raw);
  PREPLAY[fileId] = (next.audio==null&&next.subtitle==null) ? null : next;
  prePlayPreview(fileId);
}
function prePlayPickers(f){
  const auds=f.audio_streams||[], subs=f.subtitle_streams||[];
  // Nothing to choose between: one audio track and no subtitles at all.
  if(!f.available || (auds.length<2 && !subs.length)) return "";
  const pd=f.playback_defaults||{}, ad=pd.audio||{}, sd=pd.subtitle||{};
  const sel=prePlaySelection(f.id)||{audio:null,subtitle:null};
  const defAudio=auds.find(a=>a.index===ad.selected_index);
  const defSub=subs.find(s=>s.index===sd.selected_index);
  const opt=(value,label,on)=>`<option value="${esc(String(value))}"${on?" selected":""}>${esc(label)}</option>`;
  const audioField=auds.length>1?`<div class="ppfield">
      <label for="pp-a-${f.id}">Audio</label>
      <select id="pp-a-${f.id}" onchange="setPrePlay(${f.id},'audio',this.value)">
        ${opt("",defAudio?`Default · ${audioFactLabel(defAudio)}`:"Default",sel.audio==null)}
        ${auds.map(a=>opt(a.index,audioFactLabel(a),sel.audio===a.index)).join("")}
      </select></div>`:"";
  const subField=subs.length?`<div class="ppfield">
      <label for="pp-s-${f.id}">Subtitles</label>
      <select id="pp-s-${f.id}" onchange="setPrePlay(${f.id},'subtitle',this.value)">
        ${opt("",defSub?`Default · ${subFactLabel(defSub)}`:"Default · Off",sel.subtitle==null)}
        ${opt(-1,"Off",sel.subtitle===-1)}
        ${subs.map(s=>opt(s.index,subFactLabel(s),sel.subtitle===s.index)).join("")}
      </select></div>`:"";
  return `<div class="preplay">
    <div class="pprow">${audioField}${subField}</div>
    <div class="ppnote" id="pp-n-${f.id}" role="status">${esc(PREPLAY_SCOPE_NOTE)}</div></div>`;
}
// Criterion 7, said out loud rather than merely implemented: this is one
// playback's choice, and Settings → Playback defaults is untouched by it.
const PREPLAY_SCOPE_NOTE="Applies to this playback only — it doesn’t change your Playback defaults.";
// Does THIS player have to burn the chosen subtitle into the picture?
//
// Two authorities that answer two different questions, and the player owes the
// viewer whichever one says yes. `selection.subtitle_requires_burn_in` is about
// the server's delivery plan, and it is false when the PGS application overlay
// is enabled — a route the native clients have and this player does not.
// `subNeedsBurn()` is about the browser: a picture has nothing to hand a
// <track>. Trusting only the server's field would start a pre-play choice on a
// plan that cannot show it and then restart to fix that, which is precisely the
// re-buffer a pre-play choice exists to avoid.
function prePlayBurnNeeded(decision, index){
  if(index==null || index<0) return false;
  const sel=decision.selection||{};
  const server=sel.subtitle_index===index && !!sel.subtitle_requires_burn_in;
  return server || subNeedsBurn((decision.subtitles||[]).find(s=>s.index===index));
}
// What a pre-play selection means for the cold start, decided in one place so
// it can be reasoned about — and checked — without standing a player up.
//
// `subtitle` is null unless the viewer chose one explicitly. The server echoes
// its OWN policy subtitle in `selection` for an audio-only request, and reading
// that echo as a choice would burn a track nobody asked for into a film that
// was direct-playing.
function prePlayApplication(decision, selection){
  const sub=selection&&selection.subtitle!=null?selection.subtitle:null;
  const action=prePlayBurnNeeded(decision, sub)
    ? PlaybackPolicy.subtitleBurnAction({requiresBurn:true,
        deliveredRange:decision.delivered_dynamic_range})
    : "native";
  return {
    subtitle:sub,
    // The established HDR guard, reached before the stream exists rather than
    // after: a viewer choice is not permission to replace HDR with SDR.
    blockedByHdr:action==="keep_hdr",
    // A burn has to be part of the FIRST session open — `transcodeOpts()` reads
    // `PLAYER.burnedSub` — or applying it costs a second session and a visible
    // re-buffer to satisfy a choice made before playback began.
    burnedSub:action==="burn"?sub:null,
    // A text track is handed to a <track> once the route knows its offset. Off
    // (`-1`) and "no explicit choice" apply nothing here; the latter leaves the
    // cold-start server default alone.
    textSub:action==="native"&&sub!=null&&sub>=0?sub:null,
  };
}
// What the current selection would cost, in the server's own words, before
// anyone presses Play. This is the pre-play half of criterion 6: the in-player
// path already says "this format is a picture, so the server draws it in" as it
// restarts the stream, and finding that out afterwards is the thing being
// fixed.
async function prePlayPreview(fileId){
  const note=document.getElementById(`pp-n-${fileId}`); if(!note) return;
  const sel=prePlaySelection(fileId);
  if(!sel){ note.className="ppnote"; note.textContent=PREPLAY_SCOPE_NOTE; return; }
  note.className="ppnote"; note.textContent="Checking what this selection needs…";
  let d;
  try{ d=await askDecision(fileId, qualityForce(), sel); }
  catch(e){
    if(prePlaySelection(fileId)!==sel||document.getElementById(`pp-n-${fileId}`)!==note) return;
    note.className="ppnote warn"; note.textContent=e.message||"Couldn’t check that combination"; return;
  }
  if(prePlaySelection(fileId)!==sel||document.getElementById(`pp-n-${fileId}`)!==note) return;   // superseded while we were away
  const applied=prePlayApplication(d, sel);
  const blocked=applied.blockedByHdr, burn=blocked||applied.burnedSub!=null;
  const method=d.method==='direct_play'?"Direct play — no conversion"
    : d.method==='transcode'?"Transcode — the server re-encodes the video"
    : "Remux — the video is copied, only the container changes";
  const parts=[`<b>${esc(burn&&!blocked?"Transcode — the server re-encodes the video":method)}</b>.`];
  if(blocked) parts.push(esc("That subtitle is a picture and can only be shown by burning it into an SDR "+
    "re-encode. This file is HDR, so playback keeps its HDR picture and the track will not be shown."));
  else if(burn) parts.push(esc("That subtitle is a picture, so the server draws it into the frames — "+
    "it starts burned in and turning it off restarts the stream."));
  const why=(d.reasons||[]).filter(Boolean);
  if(why.length && !burn) parts.push(esc(why.join(" · ")));
  note.className="ppnote"+(blocked||burn?" warn":"");
  note.innerHTML=parts.join(" ");
}
function bookSpecBlock(f){
  const file=[f.container&&f.container.toUpperCase(),fmtSize(f.size)].filter(Boolean).join(" · ");
  return `<dl class="specs"><dt>Format</dt><dd>${esc((f.container||"book").toUpperCase())}</dd>
    <dt>File</dt><dd class="fn">${esc(f.filename)}${file?" · "+esc(file):""}</dd></dl>`;
}
// A file whose probe never succeeded shows "Video —  Audio —" and no reason.
// Say why the dashes are there, and — for an admin — offer the way out: the
// scan is keyed on size+mtime, so a `chmod` that fixes the cause changes
// nothing a rescan would notice.
function unprobedNote(f,itemId){
  if(f.probed!==false || !f.available) return '';
  const act = ME&&ME.is_admin
    ? `<button class="ghost sm" onclick="reanalyze(${itemId},this)">⟳ Reanalyze</button>`
    : '';
  return `<div class="unprobed">⚠ ${APP_NAME} couldn’t read this file’s media details — usually file
    permissions, a half-copied download, or an unmounted share at the time it was scanned. It
    still plays, but every playback decision for it is a guess.${act}</div>`;
}
// Reanalyze repairs a stale scan, not only a failed one. The scanner
// deliberately skips a file whose size and mtime are unchanged, so a probe
// written by an older FFprobe survives every ordinary library refresh — and
// when playback refuses because the stored probe no longer matches a fresh
// one, "rescan the library" is not the repair. This is the same admin action
// unprobedNote offers, behind More actions because a healthy file rarely
// needs it. px-admin/th-admin keep it off the television surfaces.
function fileAdminActions(f,itemId){
  if(!ME||!ME.is_admin||!f.available||f.probed===false) return '';
  return `<details class="setdetails px-admin th-admin"><summary>More actions</summary><div class="setdetails-body">
    <p class="hint">Read this file's media details again with the media tools installed now. Use it when playback says the source no longer matches its stored probe.</p>
    <button class="ghost sm" onclick="reanalyze(${itemId},this)">⟳ Reanalyze</button>
  </div></details>`;
}
async function reanalyze(itemId,btn){
  const label=btn.textContent; btn.disabled=true; btn.textContent="Analyzing…";
  try{
    const r=await api(`/items/${itemId}/reanalyze`,{method:"POST"});
    if(r.repaired){ toast(`Read ${r.repaired} file${r.repaired>1?"s":""} — details updated`); render(); }
    else if(r.gone) toast("The file isn’t readable at that path any more");
    else toast((r.problems&&r.problems[0])||"ffprobe still can’t read it");
  }catch(e){ toast(e.message||"Reanalyze failed"); }
  finally{ btn.disabled=false; btn.textContent=label; }
}
// Reanalyze's sibling, for pictures instead of media details. A poster that
// never downloaded leaves nothing on disk for a rescan to notice, so without
// this the only repair was refreshing the entire library.
async function refreshArtwork(itemId,btn){
  const label=btn.textContent; btn.disabled=true; btn.textContent="Fetching…";
  try{
    const r=await api(`/items/${itemId}/refresh-artwork`,{method:"POST"});
    if(r.poster){ toast("Artwork updated"); render(); }
    else toast(r.error?`Couldn’t fetch artwork — ${r.error}`:"The provider has no artwork for this");
  }catch(e){ toast(e.message||"Artwork refresh failed"); }
  finally{ btn.disabled=false; btn.textContent=label; }
}
function epThumb(ep){
  const art=ep.poster||ep.backdrop;
  const pct=progressPct(ep.watch);
  const prog=pct>0&&pct<100?`<span class="pbadge"><i style="width:${pct}%"></i></span>`:'';
  if(art) return `<div class="epthumb" style="background-image:url(${esc(tok(art))});background-size:cover;color:transparent">${prog}</div>`;
  return `<div class="epthumb">${ep.episode_number!=null?ep.episode_number:"?"}${prog}</div>`;
}
function episodeRow(ep){
  const itemId=exactWireId(ep);
  const bits=[ep.episode_number!=null?`Episode ${ep.episode_number}`:'',ep.air_date?fmtDate(ep.air_date):'',fmtDur(ep.runtime_ms)].filter(Boolean).map(m=>`<span>${esc(m)}</span>`);
  const epw=ep.watch, epdur=itemDurMs(ep);
  if(epw && !epw.watched && epw.position_ms>3000 && epdur) bits.push(`<span class="epleft">${esc(fmtDur(Math.max(0,epdur-epw.position_ms))+" left")}</span>`);
  if(ep.watch&&ep.watch.watched) bits.push('<span class="epwatched">✓ Watched</span>');
  return `<div class="eprow" onclick="location.hash='#/item/${itemId}'">
    ${epThumb(ep)}
    <div style="min-width:0">
      <div class="eptitle">${ep.episode_number!=null?esc(ep.episode_number+". "):""}${esc(ep.title)}</div>
      <div class="epmeta">${bits.join("")}</div>
      ${ep.overview?`<div class="epov">${esc(ep.overview)}</div>`:""}
    </div>
    <button class="ghost sm epplay" title="Play" onclick="event.stopPropagation();AUTOPLAY='${itemId}';location.hash='#/item/${itemId}'">▶</button>
  </div>`;
}
// Set by an episode-row play button so the target item auto-plays on arrival.
let AUTOPLAY=null;

// Item detail is a whole-body route (§3.2): one fetch, one model, one render.
// The file-to-item mapping is registered here rather than in a renderer — the
// player resolves it later regardless of which layout drew the page, so it is
// data, not presentation, and a layout that forgot to do it would break
// playback rather than just look different.
async function loadItem(id,isCurrent=()=>true){
  const [d, libs]=await Promise.all([api(`/items/${id}`), libsCached()]);
  if(!isCurrent())return null;
  const it=d.item;
  // A pre-play track choice belongs to the item it was made on. Arriving at
  // another one starts from the server's defaults again — the alternative is a
  // French audio track chosen for one film quietly applying to the next.
  clearPrePlay();
  const lib=libs.find(l=>l.id===it.library_id);
  d.files.forEach(f=>{
    ITEM_FOR_FILE[f.id]=String(id);
    // File DTOs are item-scoped and do not repeat their library id. The
    // conversion status endpoint publishes modes per library, so bind the
    // already-loaded item authority once instead of issuing per-file reads.
    f.library_id=it.library_id;
  });
  const files=d.files, children=d.children||[], ancestors=d.ancestors||[];
  const best=files[0], multi=files.length>1;
  const runtime=it.runtime_ms||(best&&best.duration_ms)||0;
  // Resume threshold in ONE place. Three seconds is "you actually started it"
  // rather than "you tapped it"; it appears on the button label, on the start-
  // over affordance and in the autoplay path, and a layout that rounded it
  // differently would offer to resume something the player restarts.
  const resume=it.watch&&it.watch.position_ms>3000&&!it.watch.watched?it.watch.position_ms:0;
  // The years a container spans, gathered from its seasons' air dates plus its
  // own. A show wears a range; a season wears one year; everything else wears
  // its year. Derived here because it is a fact about the item, not a choice
  // about how to draw it.
  let years=null;
  if(it.kind==='show'){
    const ys=children.map(c=>airYear(c.air_date)).filter(Boolean);
    if(it.year) ys.push(it.year);
    const y0=airYear(it.air_date); if(y0) ys.push(y0);
    if(ys.length) years={from:Math.min(...ys), to:Math.max(...ys)};
  } else if(it.kind==='season'){
    const y=it.year||airYear(it.air_date); if(y) years={from:y,to:y};
  } else if(it.year){ years={from:it.year,to:it.year}; }
  // What this page IS, so a body builder switches on a name instead of
  // re-deriving the same cascade of length checks. `folder` covers both the
  // empty folder and the mirrored one, distinguished by children.length.
  const shape = it.kind==='photo' ? "photo"
    : it.kind==='book' && files.length ? "book"
    : it.kind==='audiobook' && files.length ? "audiobook"
    : files.length ? "versions"
    : (children.length && children[0].kind==='episode') ? "episodes"
    : children.length ? "children"
    : it.kind==='folder' ? "folder" : "empty";
  const available=files.filter(f=>f.available);
  // A multipart audiobook owns one global timeline. Pick the part containing
  // the saved item position and hand the player a local offset inside it.
  let playable=available[0]||null, playStart=resume;
  // A book can carry several original formats. The server registry, not list
  // order or an extension guess, decides which file owns the built-in action.
  if(it.kind==='book') playable=available.find(canReadBookFile)||playable;
  if(it.kind==='audiobook' && available.length){
    playable=available[0];
    for(const f of available){
      const off=f.part_offset_ms||0, end=off+(f.duration_ms||0);
      if(resume>=off && (!end||resume<end)) { playable=f; break; }
      if(resume>=off) playable=f;
    }
    playStart=Math.max(0,resume-(playable.part_offset_ms||0));
  }
  // Route identifiers are opaque decimal strings. Cluster-backed stores may
  // allocate the full signed 64-bit range, which JavaScript cannot represent
  // exactly as a Number; rounding here made reader links target another item.
  return {kind:"item", id:String(id), item:it, files, ancestors, children,
    editions:d.editions||[], lib,
    best, multi, runtime, resume, years, shape, reading:d.reading||null,
    // The one file a Play button should reach for, and the label for the
    // children grid — both were computed identically in two bodies.
    playable, playStart,
    childLabel: it.kind==='folder' ? 'Contents'
      : (children[0]&&children[0].kind==='season') ? 'Seasons' : 'Episodes',
    meta: playerMeta(it, ancestors),
    // Metadata editing is a home-library affair: elsewhere the provider agent
    // owns the fields and would overwrite a hand edit on the next refresh.
    editable: !!(ME&&ME.is_admin&&lib&&lib.kind==="home")};
}

// Player metadata for an audiobook carries the complete ordered part list.
// That lets the existing player keep one resume/progress timeline while it
// changes source files at part boundaries or when the scrubber crosses one.
function playbackMetaFor(p,file){
  const m=Object.assign({},p.meta||{});
  if(p.item.kind!=='audiobook') return m;
  m.audiobook=true;
  m.part_offset_ms=file.part_offset_ms||0;
  m.book_duration_ms=p.runtime||0;
  m.poster=p.item.poster||p.item.backdrop||null;
  m.book_parts=p.files.filter(f=>f.available).map((f,i)=>({
    id:f.id, title:f.filename||`Part ${i+1}`, duration_ms:f.duration_ms||0,
    part_offset_ms:f.part_offset_ms||0
  }));
  return m;
}
function playCall(p,file,startMs){
  return `play(${file.id},${esc(JSON.stringify(p.item.title))},${Math.max(0,startMs||0)},${file.duration_ms||0},${esc(JSON.stringify(playbackMetaFor(p,file)))})`;
}
function exactWireId(value){
  return String(value&&value.id_text!=null?value.id_text:value&&value.id);
}
function bookContentUrl(file){ return tok(`/api/v1/files/${exactWireId(file)}/content`); }
function isEpubFile(file){ return !!(file&&/\.epub$/i.test(file.filename||"")); }
function readerAction(file,surface,mode){
  const capability=file&&file.reader&&file.reader[surface];
  if(capability&&typeof capability[mode]==="string") return capability[mode];
  // Compatibility with a server old enough not to advertise the registry.
  return isEpubFile(file)?"read":"open_in";
}
function canReadBookFile(file){ return !!(file&&file.available&&readerAction(file,"web","online")==="read"); }
function readerFile(p){ return p.files.find(canReadBookFile)||null; }
function readingLabel(p,file){
  const state=p.reading;
  const savedId=state&&String(state.file_id_text!=null?state.file_id_text:state.file_id);
  if(!state||savedId!==exactWireId(file)) return "Read";
  const pct=Math.max(0,Math.min(100,Math.round((state.progression||0)*100)));
  return state.completed?"Read again · finished":`Resume reading · ${pct}%`;
}
function bookActions(p){
  // Television surfaces deliberately have no reader or external-handler
  // action. A D-pad document reader is outside the product contract; phone,
  // tablet and desktop keep both the built-in action and the escape hatch.
  if(surfaceClass()==="tv") return "";
  const original=p.playable, readable=readerFile(p);
  if(!original) return `<div class="filemissing">⚠ No book file is currently available on the server.</div>`;
  const read=readable?`<button class="btnplay" onclick="location.hash='#/read/${p.id}/${exactWireId(readable)}'">${esc(readingLabel(p,readable))}</button>`:"";
  return `<div class="actions">${read}
    <a class="ghost" href="${esc(bookContentUrl(original))}" target="_blank" rel="noopener">Open in…</a>
    <a class="ghost" href="${esc(bookContentUrl(original))}" download="${esc(original.filename||p.item.title)}">Download original</a></div>`;
}
function bookVersions(p){
  return p.files.map(v=>`<div class="version">${bookSpecBlock(v)}${!v.available&&ME.is_admin&&v.missing_path?`<div class="problem">${esc(v.missing_path)}</div>`:''}</div>`).join("");
}
function bookEditionSection(p){
  return p.editions&&p.editions.length
    ? `<h2 class="section" style="margin-top:18px">Other editions</h2>${grid(p.editions)}`
    : '';
}
function bookByline(it){
  return (it.kind==='book'||it.kind==='audiobook')&&it.author
    ? `<div class="muted" style="margin-top:4px">By ${esc(it.author)}</div>` : '';
}
const DV_PROGRESS_POLL_MS=10000, DV_CONVERSION_LEDGER_READ_MAX=256;
const DV_CONVERSION_LEDGER_BATCH_MAX=4;
let DV_FILE_PAGE_FILES=[], DV_FILE_POLLING=false, DV_SETTINGS_POLL_AT=0;
function dvConversionIsActive(conversion){
  return !!conversion&&["queued","running","verified"].includes(conversion.state);
}
function dvSnapshotHasActive(snapshot){
  const progress=snapshot&&snapshot.progress;
  return !!progress&&(Number(progress.queued||0)+Number(progress.running||0)+Number(progress.verified||0)>0);
}
function dvRecoveryGuardStatusHtml(guard){
  if(!guard) return "";
  const state=String(guard.state||"");
  if(state==="intent"){
    return ` · recovery protection is being recorded for <code>${esc(guard.recovery_path||"unknown path")}</code>`;
  }
  if(state==="active"){
    return ` · recovery guard active at <code>${esc(guard.recovery_path||"unknown path")}</code>`;
  }
  if(state==="guard_removed") return " · recovery guard removed; private scratch cleanup pending";
  if(state==="scratch_removed") return " · recovery cleanup complete";
  return ` · recovery cleanup state ${esc(state||"unknown")}`;
}
function dvFileActionMount(file){
  if(!ME||!ME.is_admin||![7,8].includes(Number(file&&file.dv_profile))) return "";
  const id=exactWireId(file);
  return `<div class="dv-file-action px-admin th-admin" id="dv-file-${id}" aria-live="polite"><span class="muted">Checking on-disk conversion…</span></div>`;
}
function dvConversionStateHtml(file,snapshot){
  const conversion=snapshot.conversion;
  const capability=snapshot.capabilities||{};
  const id=exactWireId(file);
  const mode=(snapshot.library_modes||{})[String(file.library_id)]||"off";
  const modeOff=mode==="off";
  const modeReason="library Dolby Vision conversion mode is Off — choose Manual or Automatic in Settings → Libraries";
  if(!conversion){
    if(!snapshot.eligible) return "";
    const reason=modeOff?modeReason
      :capability.available===false?capability.reason||"dovi_tool or mkvmerge is unavailable":"";
    return `<div class="row" style="margin-top:8px;gap:8px"><span class="muted">Profile 7 can be permanently converted to Profile 8.1.</span>
      <button class="ghost sm" aria-label="Convert file ${esc(id)} from Dolby Vision Profile 7 to Profile 8.1 on disk" onclick='queueDvFile(${JSON.stringify(id)},this)'${reason?' disabled':''}>Convert on disk</button>
      ${reason?`<span class="problem">Unavailable: ${esc(reason)}</span>`:""}</div>`;
  }
  const state=conversion.state;
  const bytes=(conversion.bytes_before||conversion.bytes_after)
    ? ` · ${fmtBytes(conversion.bytes_before||0)}${conversion.bytes_after?` → ${fmtBytes(conversion.bytes_after)}`:""}`:"";
  if(state==="committed"){
    const layer=conversion.el_type?` · source ${String(conversion.el_type).toUpperCase()}`:"";
    const original=conversion.original_path?` · original retained at <code>${esc(conversion.original_path)}</code>`:" · original deleted by policy";
    return `<div class="muted" style="margin-top:8px">✓ Converted to Profile 8.1${layer}${bytes}${original}${dvRecoveryGuardStatusHtml(conversion.recovery_guard)}</div>`;
  }
  if(state==="failed"){
    const unavailable=capability.available===false;
    const ineligible=snapshot.eligible===false;
    const reason=ineligible?"the current scan no longer meets the Profile 7 conversion requirements"
      :modeOff?modeReason:unavailable?capability.reason||"conversion tools unavailable":"";
    const blocked=modeOff||unavailable;
    const retry=ineligible
      ?`<span class="problem">Retry unavailable: ${esc(reason)}.</span>`
      :`<button class="ghost sm" aria-label="Retry on-disk Dolby Vision conversion for file ${esc(id)}" onclick='queueDvFile(${JSON.stringify(id)},this)'${blocked?' disabled':''}>Retry conversion</button>${reason?` <span class="muted">${esc(reason)}</span>`:""}`;
    return `<div style="margin-top:8px"><div class="problem">Conversion failed: ${esc(conversion.error||"unknown error")}${dvRecoveryGuardStatusHtml(conversion.recovery_guard)}</div>
      ${retry}</div>`;
  }
  const labels={queued:"Queued for on-disk conversion",running:"Converting Profile 7",verified:"Replacement verified; publishing"};
  return `<div class="muted" style="margin-top:8px">${esc(labels[state]||state)}${bytes}${dvRecoveryGuardStatusHtml(conversion.recovery_guard)}</div>`;
}
async function hydrateDvFileActions(files){
  if(!ME||!ME.is_admin) return false;
  const targets=(files||[]).filter(file=>document.getElementById(`dv-file-${exactWireId(file)}`));
  if(!targets.length) return false;
  const uniqueIds=[...new Set(targets.map(exactWireId))];
  const cap=DV_CONVERSION_LEDGER_READ_MAX*DV_CONVERSION_LEDGER_BATCH_MAX;
  const ids=uniqueIds.slice(0,cap), selectedIds=new Set(ids);
  const selectedTargets=targets.filter(file=>selectedIds.has(exactWireId(file)));
  const cappedTargets=targets.filter(file=>!selectedIds.has(exactWireId(file)));
  for(const file of cappedTargets){
    const mount=document.getElementById(`dv-file-${exactWireId(file)}`);
    if(!mount) continue;
    mount.innerHTML=`<div class="problem">Conversion status not loaded for this file: this item exposes ${uniqueIds.length} Dolby Vision file actions; this page checks at most ${cap}.</div>`;
    mount.dataset.dvActive="false";
  }
  try{
    const batches=[];
    for(let at=0;at<ids.length;at+=DV_CONVERSION_LEDGER_READ_MAX){
      batches.push(ids.slice(at,at+DV_CONVERSION_LEDGER_READ_MAX));
    }
    // At most four sequential reads bound both the page's total Store work and
    // its request concurrency. Results stay private until every read succeeds.
    const snapshots=[];
    for(const batch of batches){
      snapshots.push(await api(`/dv-conversions?file_ids=${encodeURIComponent(batch.join(","))}`));
    }
    const conversions={}, eligibility={};
    for(const snapshot of snapshots){
      Object.assign(conversions,snapshot.conversions_by_file||{});
      Object.assign(eligibility,snapshot.eligible_by_file||{});
    }
    // Tool capability and configured library modes are process-wide; choosing
    // the first input-ordered batch makes the result independent of later
    // batch responses.
    const capabilities=snapshots[0].capabilities||{};
    const libraryModes=snapshots[0].library_modes||{};
    let anyActive=false;
    for(const file of selectedTargets){
      const id=exactWireId(file), mount=document.getElementById(`dv-file-${id}`);
      if(!mount) continue;
      const conversion=conversions[id]||null;
      const active=dvConversionIsActive(conversion);
      anyActive=anyActive||active;
      mount.innerHTML=dvConversionStateHtml(file,{
        conversion,
        eligible:eligibility[id]===true,
        capabilities,
        library_modes:libraryModes,
      });
      mount.dataset.dvActive=active?"true":"false";
    }
    return anyActive;
  }catch(e){
    let anyActive=false;
    for(const file of selectedTargets){
      const mount=document.getElementById(`dv-file-${exactWireId(file)}`);
      if(!mount) continue;
      mount.innerHTML=`<div class="problem">Conversion status unavailable: ${esc(e.message||"request failed")}</div>`;
      anyActive=anyActive||mount.dataset.dvActive==="true";
    }
    return anyActive;
  }
}
async function pollDvFileActions(files,generation){
  if(DV_FILE_POLLING||generation!==PAGE_RENDER_GENERATION||!location.hash.startsWith("#/item/")||document.visibilityState==="hidden") return;
  DV_FILE_POLLING=true;
  try{
    const active=await hydrateDvFileActions(files);
    if(!active&&generation===PAGE_RENDER_GENERATION&&location.hash.startsWith("#/item/")){
      clearInterval(PAGE_TIMER); PAGE_TIMER=null;
    }
  }finally{ DV_FILE_POLLING=false; }
}
function armDvFilePoll(files,generation=PAGE_RENDER_GENERATION){
  DV_FILE_PAGE_FILES=files||[];
  setPageTimer(()=>pollDvFileActions(DV_FILE_PAGE_FILES,generation),DV_PROGRESS_POLL_MS,generation);
}
async function queueDvFile(id,btn){
  btn.disabled=true;
  try{
    const result=await api(`/files/${id}/dv-conversion`,{method:"POST"});
    // The mutation response is the authoritative first active snapshot. Paint
    // and poll from it instead of making queue success depend on a second read.
    const file=DV_FILE_PAGE_FILES.find(candidate=>exactWireId(candidate)===String(id));
    const mount=document.getElementById(`dv-file-${id}`);
    const active=dvConversionIsActive(result&&result.conversion);
    const queued=result&&result.queued===true;
    if(file&&mount&&result&&result.conversion){
      mount.innerHTML=dvConversionStateHtml(file,{
        conversion:result.conversion,eligible:queued||active,capabilities:{available:true},
      });
      mount.dataset.dvActive=active?"true":"false";
    }
    toast(queued?"Dolby Vision conversion queued":active
      ?"Dolby Vision conversion is already active":"Dolby Vision conversion was not queued");
    if(active) armDvFilePoll(DV_FILE_PAGE_FILES);
  }catch(e){ toast(e.message||"Couldn't queue conversion"); }
  finally{ btn.disabled=false; }
}
function chapterList(p){
  const rows=[];
  for(const f of p.files){
    for(const c of (f.chapters||[])){
      const at=(f.part_offset_ms||0)+(c.start_ms||0);
      rows.push(`<button class="ghost sm" onclick='${playCall(p,f,c.start_ms||0)}'>${esc(c.title||`Chapter ${c.index+1}`)} <span class="muted">${esc(fmtDur(at))}</span></button>`);
    }
  }
  return rows.length?`<div class="chapters"><h2 class="section">Chapters</h2><div class="row" style="align-items:flex-start">${rows.join('')}</div></div>`:'';
}
async function viewItem(id,isCurrent=()=>true){
  const generation=PAGE_RENDER_GENERATION;
  layoutChrome("home",`<div class="empty">Loading…</div>`);
  const page=await loadItem(id,()=>isCurrent()&&generation===PAGE_RENDER_GENERATION&&location.hash===`#/item/${id}`);
  if(!page)return;
  if(!isCurrent()||generation!==PAGE_RENDER_GENERATION||location.hash!==`#/item/${id}`)return;
  WATCH_ITEM_PAGE=page;
  document.getElementById("main").innerHTML=layoutView("item",page);
  DV_FILE_PAGE_FILES=page.files||[];
  hydrateDvFileActions(DV_FILE_PAGE_FILES).then(active=>{
    if(active&&generation===PAGE_RENDER_GENERATION&&location.hash.startsWith("#/item/")) armDvFilePoll(DV_FILE_PAGE_FILES,generation);
  });
  restoreScroll();
  // One-click play from an episode list lands here and starts immediately.
  if(AUTOPLAY===String(id)){
    AUTOPLAY=null;
    // Same file, same resume point, same metadata the buttons would have used:
    // the autoplay path is a third caller of the model, not a third derivation.
    const it=page.item, pf=page.playable;
    if(pf) play(pf.id, it.title, page.playStart, pf.duration_ms||0, playbackMetaFor(page,pf));
  }
}
// Classic's item body — byte-for-byte what this app has always emitted.
function classicItemBody(p){
  const d={files:p.files, ancestors:p.ancestors, children:p.children};
  const it=p.item, id=p.id, editable=p.editable;
  const channelBtn=["movie","show","episode"].includes(it.kind)
    ? `<button class="ghost" onclick='makeLibraryChannelFromItem(${it.id},${esc(JSON.stringify(it.kind))},${esc(JSON.stringify(it.title))})'>Make a channel</button>`:"";
  // Offered on every item, not only the ones that look broken: an admin who
  // wants a different poster than the one that landed has no other way to ask
  // for it short of refreshing the whole library.
  const artBtn = ME&&ME.is_admin
    ? ` <button class="ghost sm" title="Re-fetch this item's poster and backdrop" aria-label="Refresh artwork" onclick="refreshArtwork('${exactWireId(it)}',this)">⟳ Refresh artwork</button>`
    : '';

  // Breadcrumb trail: Home / <list you came from> / Show / Season / …, every
  // level clickable. The middle entry is what makes "back" return to the full
  // Movies page rather than dumping you at Home.
  const trail=[{href:"#/",label:"Home"}]
    .concat(NAV_ORIGIN?[{href:NAV_ORIGIN.href,label:NAV_ORIGIN.label}]:[])
    .concat((d.ancestors||[]).map(a=>({href:`#/item/${exactWireId(a)}`,label:a.title})))
    .concat([{label:it.title}]);
  const crumbs=pageHead(trail);

  const best=p.best, multi=p.multi, runtime=p.runtime;
  const chips=[];
  if(it.recorded_at){ chips.push(`<span>${esc(fmtDate(it.recorded_at))}</span>`); }
  if(p.years){ const y=p.years; chips.push(`<span>${y.from}${y.to>y.from?'–'+y.to:''}</span>`); }
  if(runtime) chips.push(`<span>${fmtDur(runtime)}</span>`);
  if(it.kind) chips.push(`<span>${esc(it.kind)}</span>`);
  // A container has no watch flag of its own, so say what it's made of:
  // "3 of 10 watched" is the thing the mark-watched buttons below act on.
  if(it.rollup&&it.rollup.leaves){
    const r=it.rollup;
    chips.push(`<span>${r.watched===r.leaves?`all ${r.leaves} watched`:`${r.watched} of ${r.leaves} watched`}</span>`);
  }
  // Video badges show once: in the hero for a single version; per-version below
  // when there are several (so nothing is duplicated).
  const heroBadges = (best && !multi) ? specBadges(best) : "";

  let body="";
  if(p.shape==="photo"){
    // A photo is a picture, not a playback session: show it, and let a click
    // open the same full-screen view the grid uses.
    PHOTO_SET=[{id:it.id,title:it.title,recorded_at:it.recorded_at}];
    body+=`<div class="actions"><button class="btnplay" onclick="openLightbox(${it.id})">View full size</button></div>
      <img class="art" style="aspect-ratio:auto;max-height:60vh;width:auto;border-radius:var(--radius);cursor:zoom-in"
           src="${esc(photoUrl(it.id,'thumb'))}" alt="" onclick="openLightbox(${it.id})">`;
  } else if(p.shape==="book"){
    body+=bookActions(p)+bookVersions(p);
  } else if(p.shape==="audiobook"){
    const playable=p.playable, resume=p.resume, start=p.playStart;
    const watchBtn=watchControls(it);
    if(playable){
      body+=`<div class="actions">
        <button class="btnplay" onclick='${playCall(p,playable,start)}'>▶ ${resume?`Resume · ${fmtDur(resume)}`:'Play audiobook'}</button>
        ${resume?`<button class="ghost" onclick='${playCall(p,p.files.find(f=>f.available),0)}'>Start over</button>`:''}
        ${watchBtn}${channelBtn}</div>`;
      const rem=resume?Math.max(0,runtime-resume):runtime, pend=endsAt(rem);
      if(rem) body+=`<div class="playinfo">${resume?fmtDur(rem)+" left":fmtDur(rem)}${pend?" · ends ~"+pend:""}</div>`;
    } else {
      body+=`<div class="filemissing">⚠ No audiobook part is currently available on the server.</div><div class="actions">${watchBtn}</div>`;
    }
    body+=d.files.map((v,i)=>{
      const miss=!v.available, part=d.files.length>1?`Part ${i+1}`:'Audiobook';
      const head=`<div class="vh"><span class="vbadges"><span class="vt">${esc(part)}</span>${specBadges(v)}${miss?'<span class="missbadge">missing</span>':''}</span>${!miss?`<button class="ghost sm" onclick='${playCall(p,v,0)}'>▶ Play</button>`:''}</div>`;
      return `<div class="version">${head}${specBlock(v)}${miss&&ME.is_admin&&v.missing_path?`<div class="problem">${esc(v.missing_path)}</div>`:''}${unprobedNote(v,it.id)}</div>`;
    }).join("");
    body+=chapterList(p);
  } else if(p.shape==="versions"){
    // Movie / episode: play the best *available* version + labeled specs.
    // The player's info panel is fed from here — the detail response is the only
    // place that carries the synopsis and the show/season ancestry together.
    const pmeta=p.meta, resume=p.resume;
    const watchBtn=watchControls(it);
    const playable=p.playable;
    if(playable){
      body+=`<div class="actions">
        <button class="btnplay" onclick='${playCall(p,playable,resume)}'>▶ ${resume?`Resume · ${fmtDur(resume)}`:'Play'}</button>
        ${resume?`<button class="ghost" onclick='${playCall(p,playable,0)}'>Start over</button>`:''}
        ${watchBtn}${channelBtn}</div>`;
      const pdur=playable.duration_ms||runtime||0, prem=resume?Math.max(0,pdur-resume):pdur, pend=endsAt(prem);
      if(prem) body+=`<div class="playinfo">${resume?fmtDur(prem)+" left":fmtDur(prem)}${pend?" · ends ~"+pend:""}</div>`;
    } else {
      const p=best.missing_path;
      body+=`<div class="filemissing">⚠ This file is missing on the server, so it can’t be played — its library path may be unmounted, moved, or renamed${ME.is_admin&&p?`:<br><code>${esc(p)}</code>`:'.'}</div><div class="actions">${watchBtn}${channelBtn}</div>`;
    }
    body+=d.files.map(v=>{
      const miss=!v.available;
      // Header (badges + per-version play) only when there are multiple versions
      // to tell apart, or when a file is missing. A single version is already
      // badged in the hero, so its block is just the spec detail.
      const header=(multi||miss)
        ? `<div class="vh"><span class="vbadges">${specBadges(v)||`<span class="vt">${esc(v.filename)}</span>`}${miss?'<span class="missbadge">missing</span>':''}</span>${(!miss&&multi)?`<button class="ghost sm" onclick='${playCall(p,v,resume)}'>▶ Play</button>`:''}</div>`
        : '';
      return `<div class="version">${header}${specBlock(v)}${miss&&ME.is_admin&&v.missing_path?`<div class="problem">${esc(v.missing_path)}</div>`:''}${unprobedNote(v,it.id)}${dvFileActionMount(v)}${fileAdminActions(v,it.id)}</div>`;
    }).join("");
  } else if(p.shape==="episodes"){
    // Season: episodes as a list.
    const acts=watchControls(it);
    body+=`${acts?`<div class="actions">${acts}</div>`:''}
      <h2 class="section" style="margin:18px 0 0">${d.children.length} episode${d.children.length>1?'s':''}</h2>
      <div class="eplist">${d.children.map(episodeRow).join("")}</div>`;
  } else if(p.shape==="children"){
    // Show, season, or a mirrored folder: children as a grid.
    if(channelBtn)body+=`<div class="actions">${channelBtn}</div>`;
    const label = it.kind==='folder'? 'Contents'
                : p.childLabel==='Seasons'? 'Seasons' : 'Episodes';
    const acts=watchControls(it);
    body+=`${acts?`<div class="actions">${acts}</div>`:''}
      <h2 class="section" style="margin:18px 0 12px">${label}</h2>${grid(d.children)}`;
  } else if(p.shape==="folder"){
    body+=`<div class="empty">This folder is empty.</div>`;
  }
  body+=bookEditionSection(p);

  const bg=it.backdrop?`<div class="herobg" style="background-image:url(${esc(tok(it.backdrop))})"></div>`:'';
  return `<div class="hero">${bg}
    <div class="dwrap">
      <div class="poster">${artHtml(it)}</div>
      <div class="dhead">${crumbs}<h1>${esc(it.title)}${editable?` <button class="ghost sm" title="Edit details" aria-label="Edit details" onclick='openEdit(${esc(JSON.stringify(it))})'>✎ Edit</button>`:''}${artBtn}</h1>${bookByline(it)}
        <div class="chips">${chips.join("")}</div>
        ${(it.tags&&it.tags.length)?`<div class="chips">${it.tags.map(t=>`<span>${esc(t)}</span>`).join("")}</div>`:''}
        ${heroBadges?`<div class="vbadges" style="margin-top:10px">${heroBadges}</div>`:''}
        ${it.overview?`<p class="overview">${esc(it.overview)}</p>`:''}
        ${body}</div>
    </div></div>`;
}
