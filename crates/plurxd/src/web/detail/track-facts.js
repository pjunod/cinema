"use strict";
// ---- detail-screen track facts (docs/CLIENTS.md §"Shared track facts") ----
// The item-detail payload already carries the server's cold-start answer in
// `playback_defaults`: which audio and subtitle stream would play, and how the
// configured preferred language relates to the tracks that exist. This renders
// that answer. It does not re-run `select_tracks` in JavaScript and it never
// reads admin settings — two clients that each invented the rule would tell the
// same file's story two different ways, which is the whole reason the server
// computes it.
//
// FIVE statuses, not four. `unknown` is a file with at least one untagged
// track: the preferred language may well be sitting right there under a missing
// tag, so "no English subtitles" would be a claim the data does not support.
// Folding it into `missing` is the specific mistake this note exists to stop.
function preferredLanguageNote(kind, d){
  if(!d) return "";
  const lang=langName(d.preferred_language)||d.preferred_language||"";
  const s=d.preferred_language_status;
  if(s==="available") return `${lang} ${kind} available — the server’s default is another track`;
  if(s==="missing")   return `no ${lang} ${kind}`;
  if(s==="unknown")   return `a track has no language tag, so ${lang} ${kind} can’t be ruled out`;
  // `selected` needs no sentence (the marked chip is the sentence) and
  // `no_tracks` is already said by the row itself.
  return "";
}
// One track chip. The marker is a word, not only a colour: a monochrome theme,
// a screen reader, and a printed page all have to survive it.
function trackChip(label, isDefault){
  return `<span class="trk${isDefault?" on":""}">${esc(label)}${
    isDefault?' · <span class="tdef">plays by default</span>':""}</span>`;
}
// Subtitle format as the container spells it, in the words a viewer recognises.
// Display only — nothing here decides delivery. Whether a track has to be
// burned into the picture is the server's answer (`selection.subtitle_requires_
// burn_in`), asked for in prePlayPreview() rather than guessed from a codec.
const SUB_FORMATS={subrip:"SRT",srt:"SRT",webvtt:"WebVTT",vtt:"WebVTT",ass:"ASS",ssa:"SSA",
  mov_text:"mov_text",text:"Text",hdmv_pgs_subtitle:"PGS",pgs:"PGS",dvd_subtitle:"VobSub",
  dvdsub:"VobSub",dvb_subtitle:"DVB",xsub:"XSUB",eia_608:"CEA-608",eia_708:"CEA-708"};
function subFormat(codec){
  const c=String(codec||"").toLowerCase();
  return SUB_FORMATS[c]||(c?c.toUpperCase():"");
}
// Language · format · markers. `forced` and `hearing_impaired` are the
// container's own dispositions, which is exactly what the in-player Subtitles
// menu shows — one story per track, wherever you read it.
function subFactLabel(s){
  return [langName(s.language)||"Untagged", subFormat(s.codec),
          s.forced?"forced":null, s.hearing_impaired?"SDH":null,
          s.title].filter(Boolean).join(" · ");
}
// Deliberately the same shape as subFactLabel: language first, then format,
// then what distinguishes this track from its neighbours. Two rows that read
// the same way are what makes "English audio, no English subtitles" legible in
// one glance — the older `audioLabel()` form led with the codec and left the
// language as a raw `eng` at the end, which is the one fact being compared.
function audioFactLabel(a){
  return [langName(a.language)||"Untagged", a.codec&&a.codec.toUpperCase(),
          fmtChannels(a.channels), a.title].filter(Boolean).join(" · ");
}
function trackFactRow(label, tracks, selectedIndex, chip, note, collapse=false){
  let chips=`<div class="trks">None</div>`;
  if(tracks.length){
    if(collapse&&tracks.length>1){
      // Lead with what actually plays, rather than assuming the container's
      // first track is the useful one. The remaining source-ordered tracks
      // stay present in the document and are one click/tap away.
      const primary=tracks.find(t=>t.index===selectedIndex)||tracks[0];
      const rest=tracks.filter(t=>t!==primary);
      chips=`<details class="trkfold"><summary>${chip(primary,primary.index===selectedIndex)}<span class="trkmore">${rest.length} more</span></summary><div class="trks trkrest">${
        rest.map(t=>chip(t,t.index===selectedIndex)).join("")}</div></details>`;
    }else{
      chips=`<div class="trks">${tracks.map(t=>chip(t,t.index===selectedIndex)).join("")}</div>`;
    }
  }
  return `<dt>${label}</dt><dd>${chips}${note?`<div class="langnote">${esc(note)}</div>`:""}</dd>`;
}
function specBlock(f){
  const vid=[f.video_codec&&f.video_codec.toUpperCase(),(f.width&&f.height)?`${f.width}×${f.height}`:null,(f.hdr_format||f.hdr),f.bit_depth?`${f.bit_depth}-bit`:null,fmtMbps(f.bitrate)].filter(Boolean).join(" · ");
  const file=[f.container&&f.container.toUpperCase(),fmtSize(f.size),fmtDur(f.duration_ms)].filter(Boolean).join(" · ");
  const auds=f.audio_streams||[], subs=f.subtitle_streams||[];
  const pd=f.playback_defaults||{};
  const ad=pd.audio, sd=pd.subtitle;
  // Audio keeps its existing one-line form when the server sent no defaults (an
  // older server, or a file that was never probed) — the row must not become
  // emptier than it was.
  const audRow=ad
    ? trackFactRow("Audio", auds, ad.selected_index==null?-1:ad.selected_index,
        (a,on)=>trackChip(audioFactLabel(a),on), preferredLanguageNote("audio",ad), true)
    : `<dt>Audio</dt><dd>${esc(auds.map(audioLabel).filter(Boolean).join("  /  ")||"—")}</dd>`;
  // Subtitles were previously not on this page at all: the only way to learn a
  // file's subtitles was to start playback and open the CC menu. A file with
  // none says so — an absent row would read as "not loaded yet".
  //
  // Skipped entirely for a file with neither video nor subtitles, which is an
  // audiobook part: "Subtitles: None" there is noise about something nobody
  // expected to exist.
  const subRow=(!subs.length && !f.video_codec) ? ""
    : trackFactRow("Subtitles", subs, sd&&sd.selected_index!=null?sd.selected_index:-1,
        (s,on)=>trackChip(subFactLabel(s),on),
        subs.length?preferredLanguageNote("subtitles",sd):"", true);
  // Lead with what a viewer will actually get. "VOD index" is an internal
  // prerequisite; VOD HLS versus Live HLS is the playback capability someone
  // is deciding whether to use.
  const hlsRow=f.vod_index_status==="indexed"
    ? `<dt>HLS capability</dt><dd><span class="mode-chip vod">VOD HLS</span><span class="mode-detail">Fixed, seekable timeline · analysis is ready for this file</span></dd>`
    : f.vod_index_status==="partial"
      ? `<dt>HLS capability</dt><dd><span class="mode-chip vod">VOD HLS on some devices</span><span class="mode-detail">Analysis is ready for some of this file's delivery routes but not all — a Dolby Vision device can still fall back to Live HLS until the rest is built</span></dd>`
      : f.vod_index_status==="pending"
        ? `<dt>HLS capability</dt><dd><span class="mode-chip live">Live HLS fallback</span><span class="mode-detail">${f.vod_index_refusal?esc(f.vod_index_refusal)+" — it will be tried again":"Can be used while VOD analysis is pending when live recovery is enabled in Playback settings"}</span></dd>`
      : f.vod_index_status==="refused"
        ? `<dt>HLS capability</dt><dd><span class="mode-chip live">Live HLS only</span><span class="mode-detail">VOD analysis ${f.vod_index_refusal?esc(f.vod_index_refusal):"was refused for this file"} — waiting will not change that; Live HLS requires live recovery to be enabled in Playback settings</span></dd>`
        : f.vod_index_status==="unsupported"
          ? `<dt>HLS capability</dt><dd><span class="mode-chip live">Live HLS fallback</span><span class="mode-detail">This codec cannot use the VOD indexer; Live HLS requires live recovery to be enabled in Playback settings</span></dd>`:"";
  const analysis=analysisFileControl(f);
  const facts=`<dl class="specs">
    ${vid||!auds.length?`<dt>Video</dt><dd>${esc(vid||"—")}</dd>`:''}
    ${audRow}
    ${subRow}
    ${hlsRow}
    <dt>File</dt><dd class="fn">${esc(f.filename)}${file?" · "+esc(file):""}</dd></dl>`;
  if(!f.video_codec) return `${facts}${prePlayPickers(f)}${analysis}`;
  const id=exactWireId(f);
  return `<div class="media-preparation-layout">
    <section class="media-preparation-main" aria-label="Media preparation">
      <div class="prep-heading"><div><div class="prep-eyebrow">BEFORE YOU PRESS PLAY</div><h2>Media preparation</h2></div>
        <button class="ghost sm" onclick='refreshMediaPreparation(${esc(JSON.stringify(id))},this)'>Refresh status</button></div>
      <p class="muted prep-intro">What is ready, what is still processing, and what happens on demand.</p>
      <div id="prep-file-${esc(id)}" class="prep-status"><p class="muted" role="status">Checking preparation…</p></div>${analysis}
    </section>
    <aside class="media-preparation-side" aria-label="Playback settings and file details">
      <div class="prep-eyebrow">YOUR CHOICES</div><h2>This playback</h2>${prePlayPickers(f)}
      <h3>File & tracks</h3>${facts}
    </aside></div>`;
}
function analysisFileControl(f){
  if(!ME||!ME.is_admin||!f.available||!f.video_codec) return "";
  const rebuild=f.vod_index_status==="indexed";
  const unsupported=f.vod_index_status==="unsupported";
  const fileId=exactWireId(f);
  const state=rebuild?"VOD HLS ready"
    :f.vod_index_status==="unsupported"?"VOD analysis unsupported"
    :f.vod_index_status==="partial"?"VOD analysis partly built"
    :"VOD analysis pending";
  return `<div class="analysis-brief px-admin th-admin" style="margin-top:12px">
    <div class="analysis-brief-head"><div><h2>Content analysis</h2>
      <p><b>${esc(state)}</b> · Build and inspect the structural index and exact skip markers for this file.</p></div>
      <div class="analysis-actions">${unsupported?"":`<button class="ghost sm" onclick='requestAnalysis(${esc(JSON.stringify(fileId))},${rebuild?'true':'false'},this)'>${rebuild?'Rebuild analysis':'Analyze now'}</button>`}
      <a class="ghost sm" href="#/analysis">View queue</a></div></div></div>`;
}
async function requestAnalysis(fileId,force,btn,stay=false){
  const label=btn&&btn.textContent;
  if(btn){ btn.disabled=true; btn.textContent=force?"Queueing rebuild…":"Queueing…"; }
  try{
    const response=await api(`/files/${fileId}/analysis`,{method:"POST",body:{force:!!force,components:["fragment_index","skip_markers"]}});
    toast(response.joined?"Analysis is already queued":"Analysis queued");
    if(stay&&location.hash==="#/analysis") await renderAnalysis(PAGE_RENDER_GENERATION,true);
    else location.hash="#/analysis";
  }catch(error){ toast(error.message||"Couldn’t queue analysis"); }
  finally{ if(btn&&document.body.contains(btn)){ btn.disabled=false; btn.textContent=label; } }
}

// Readiness is a projection of server processing facts, independent of PREPLAY.
// Only these mounts refresh; playback controls and hero artwork stay intact.
const PREPARATION_LABELS={ready:"Ready",done:"Done",pending:"Pending",queued:"Queued",running:"Processing",
  attention:"Needs attention",failed:"Failed",cancelled:"Cancelled",unknown:"Unknown",none:"No tracks",
  unsupported:"Unsupported",not_applicable:"Not needed",on_demand:"On demand",off:"Off",enabled:"Enabled",
  committed:"Converted",verified:"Publishing",succeeded:"Completed",cancelling:"Cancelling",idle:"Idle"};
function preparationBadge(state){
  const tone=["ready","done","committed","succeeded"].includes(state)?"good"
    :["attention","failed"].includes(state)?"bad":["queued","running","pending","verified"].includes(state)?"warn":"neutral";
  return `<span class="prep-badge ${tone}">${esc(PREPARATION_LABELS[state]||"Unknown")}</span>`;
}
function preparationRow(title,value,extra=""){
  const v=value||{state:"unknown",detail:"Status was not returned"};
  return `<div class="prep-row"><div><h3>${esc(title)}</h3><p>${esc(v.detail||"")}</p>${extra}</div>${preparationBadge(v.state)}</div>`;
}
function preparationGroups(rows){
  const groups=[
    ["attention","Needs attention",["attention","failed"]],
    ["processing","Processing",["running","verified","cancelling"]],
    ["queued","Queued",["queued"]],
    ["pending","Pending",["pending"]],
    ["cancelled","Cancelled",["cancelled"]],
    ["on-demand","On demand",["on_demand"]],
    ["done-ready","Done & ready",["done","ready","committed","succeeded"]],
    ["unknown","Unknown",["unknown"]],
    ["off","Off",["off"]],
    ["not-needed","Not needed",["none","not_applicable"]],
    ["unsupported","Unsupported",["unsupported"]],
    ["enabled","Enabled",["enabled"]],
    ["idle","Idle",["idle"]],
  ];
  const known=new Set(groups.flatMap(([, ,states])=>states));
  return groups.map(([id,label,states])=>{
    const members=rows.filter(([,value])=>states.includes(known.has(value?.state)?value.state:"unknown"));
    if(!members.length) return "";
    return `<details class="prep-group" data-prep-disclosure="${id}"><summary><span>${esc(label)}</span><span class="prep-count" aria-label="${members.length} parts">${members.length}</span></summary><div class="prep-group-items">${members.map(([title,value,extra])=>preparationRow(title,value,extra)).join("")}</div></details>`;
  }).join("");
}
function mediaPreparationHtml(f,data){
  const subs=data.subtitles||{}, tracks=f.subtitle_streams||[];
  const preferred=tracks.find(t=>t.index===subs.preferred_index);
  const counts=`${subs.ready||0} of ${subs.total||0} extracted${subs.empty?` · ${subs.empty} empty`:""}${subs.failed?` · ${subs.failed} need attention`:""}`;
  const rule=preferred?`Indicator follows configured default: ${subFactLabel(preferred)}`
    :subs.total?`Configured default ${langName(subs.preferred_language)||subs.preferred_language||"subtitle"} is absent — waiting for all tracks`
    :"This file has no subtitle tracks";
  const subDetail=`${rule}. ${counts}.`;
  const trackDetails=tracks.length?`<details class="prep-tracks" data-prep-disclosure="tracks"><summary>All subtitle tracks <span class="muted">${counts}</span></summary><div>${tracks.map(t=>{
    const result=(subs.tracks||[]).find(r=>r.index===t.index)||{};
    const label=result.state==="empty"?'<span class="prep-badge neutral">Empty</span>':preparationBadge(result.state==="ready"?"done":result.state);
    return `<div class="prep-track"><span>${esc(subFactLabel(t))}${t.index===subs.preferred_index?'<small>Configured default</small>':""}</span>${label}</div>`;
  }).join("")}</div></details>`:"";
  const subtitleWork=["queued","running","failed","cancelled"].includes((subs.work||{}).state)
    ?`<p class="prep-work">${esc(PREPARATION_LABELS[subs.work.state])}: ${esc(subs.work.detail)}</p>`:"";
  const essentials=[["Playback analysis",data.playback],["Subtitles",{state:subs.state,detail:subDetail},subtitleWork+trackDetails],
    ["Skip markers",data.markers],["File inspection",data.probe],["Metadata & artwork",data.metadata]];
  const other=[["Prepared versions",data.versions],["Chapter previews",data.thumbnails],
    ["Dolby Vision conversion",data.conversion],["Semantic search",data.search],["Subtitle downloads",data.downloads]];
  const states=essentials.map(([,v])=>v&&v.state);
  const headline=states.some(v=>["failed","attention"].includes(v))?"Some preparation needs attention"
    :states.some(v=>!v||v==="unknown")?"Preparation status is incomplete"
    :states.some(v=>["pending","running","queued"].includes(v))?"Preparation in progress":states.every(v=>["ready","done","none","not_applicable","unsupported"].includes(v))?"Core preparation complete":"Preparation is incomplete";
  return `<div class="prep-overview" role="status">${esc(headline)}<small>Preparation status does not prevent playback.</small></div>
    ${preparationGroups([...essentials,...other])}
    <p class="prep-checked">Checked ${esc(new Date(data.checked_at_ms).toLocaleTimeString())} · Recorded extraction results; delivery is verified when used.</p>`;
}
async function hydrateMediaPreparation(files,generation=PAGE_RENDER_GENERATION){
  let active=false;
  // Sequential reads bound concurrent database work for multi-version items.
  for(const f of files||[]){
    if(generation!==PAGE_RENDER_GENERATION) break;
    const id=exactWireId(f), mount=document.getElementById(`prep-file-${id}`);
    if(!mount) continue;
    try{
      const data=await api(`/files/${id}/preparation`);
      if(generation!==PAGE_RENDER_GENERATION||document.getElementById(`prep-file-${id}`)!==mount) continue;
      const signature=JSON.stringify({...data,checked_at_ms:0});
      if(mount.dataset.prepSnapshot!==signature){
        const disclosures=new Map(Array.from(mount.querySelectorAll('details[data-prep-disclosure]'),(/** @type {HTMLDetailsElement} */ prior)=>[
          prior.dataset.prepDisclosure,{open:prior.open,focused:document.activeElement===prior.querySelector("summary")}
        ]));
        mount.innerHTML=mediaPreparationHtml(f,data);
        mount.dataset.prepSnapshot=signature;
        let restoredFocus=false;
        for(const details of /** @type {NodeListOf<HTMLDetailsElement>} */ (mount.querySelectorAll('details[data-prep-disclosure]'))){
          const prior=disclosures.get(details.dataset.prepDisclosure);
          if(prior?.open) details.open=true;
          if(prior?.focused){
            // A subtitle status change can move its disclosure into another group.
            const parent=/** @type {HTMLDetailsElement|null} */ (details.parentElement.closest('details[data-prep-disclosure]'));
            if(parent) parent.open=true;
            details.querySelector("summary").focus({preventScroll:true});
            restoredFocus=true;
          }
        }
        // A group disappears when its last part changes state. Keep keyboard
        // navigation in the preparation list instead of dropping focus to body.
        if(!restoredFocus&&Array.from(disclosures.values()).some(prior=>prior.focused)){
          const fallback=/** @type {HTMLElement|null} */ (mount.querySelector('.prep-group>summary'));
          fallback?.focus({preventScroll:true});
        }
      }
      // Poll activity reported by the queue, never infer it from missing outputs.
      const pending=data.active===true;
      mount.dataset.prepActive=pending?"true":"false";
      active=active||pending;
    }catch(error){
      if(generation!==PAGE_RENDER_GENERATION||document.getElementById(`prep-file-${id}`)!==mount) continue;
      // A failed refresh invalidates a previous green snapshot. Keep selectors usable.
      mount.innerHTML=`<div class="prep-overview" role="status">Preparation status unavailable<small>${esc(error.message||"Could not read processing state")}. Use Refresh status to try again.</small></div>`;
      mount.dataset.prepActive="false";
      delete mount.dataset.prepSnapshot;
    }
  }
  return active;
}
async function refreshMediaPreparation(id,button){
  const generation=PAGE_RENDER_GENERATION;
  if(button) button.disabled=true;
  try{
    await pollDvFileActions(DV_FILE_PAGE_FILES,generation,true);
  }finally{ if(button) button.disabled=false; }
}
