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
        ? `<dt>HLS capability</dt><dd><span class="mode-chip live">Live HLS fallback</span><span class="mode-detail">${f.vod_index_refusal?esc(f.vod_index_refusal)+" — it will be tried again":"Can be used while VOD analysis is pending when live recovery is enabled in Developer settings"}</span></dd>`
      : f.vod_index_status==="refused"
        ? `<dt>HLS capability</dt><dd><span class="mode-chip live">Live HLS only</span><span class="mode-detail">VOD analysis ${f.vod_index_refusal?esc(f.vod_index_refusal):"was refused for this file"} — waiting will not change that; Live HLS requires live recovery to be enabled in Developer settings</span></dd>`
        : f.vod_index_status==="unsupported"
          ? `<dt>HLS capability</dt><dd><span class="mode-chip live">Live HLS fallback</span><span class="mode-detail">This codec cannot use the VOD indexer; Live HLS requires live recovery to be enabled in Developer settings</span></dd>`:"";
  const analysis=analysisFileControl(f);
  return `<dl class="specs">
    ${vid||!auds.length?`<dt>Video</dt><dd>${esc(vid||"—")}</dd>`:''}
    ${audRow}
    ${subRow}
    ${hlsRow}
    <dt>File</dt><dd class="fn">${esc(f.filename)}${file?" · "+esc(file):""}</dd></dl>${prePlayPickers(f)}${analysis}`;
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
