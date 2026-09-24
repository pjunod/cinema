"use strict";
// ---- playback stats overlay (press i, or the Stats button) ----------------
// ---- generated from tests/playback/playback-info-fields.json by
// scripts/player-contract-table --embed; do not edit by hand ----
const PLAYBACK_INFO_FIELDS=[{"id":"method","label":"Method","section":"PLAYBACK","modes":["standard","details","debug"],"format":"text","note":"The delivery verdict — the same vocabulary on every client: Direct play · Remux · Transcode · Transcode · cached; the encoder and rung follow as a clause (\"Transcode · nvenc · 1080p\")."},{"id":"playback_mode","label":"Playback mode","section":"PLAYBACK","modes":["debug"],"format":"text","available_on":["web"],"note":"Live HLS / VOD HLS / progressive — the web presentation kind. Native players have one presentation."},{"id":"position","label":"Position","section":"PLAYBACK","modes":["standard","details","debug"],"format":"position"},{"id":"reason","label":"Reason","section":"PLAYBACK","modes":["standard","details","debug"],"format":"list","placement":"notes","note":"Why the server chose this method."},{"id":"build","label":"Build","section":"PLAYBACK","modes":["debug"],"format":"text","always":true},{"id":"transport","label":"Transport","section":"PLAYBACK","modes":["debug"],"format":"text","placement":"notes"},{"id":"file_id","label":"File ID","section":"PLAYBACK","modes":["debug"],"format":"text"},{"id":"session","label":"Session","section":"PLAYBACK","modes":["debug"],"format":"text","placement":"notes"},{"id":"switched","label":"Switched","section":"PLAYBACK","modes":["debug"],"format":"list","placement":"notes","available_on":["web"],"note":"Auto-quality moves this session."},{"id":"source_video","label":"Original video","section":"SOURCE","modes":["standard","details","debug"],"format":"list","placement":"notes","note":"codec · profile · bit depth · HDR format."},{"id":"source_resolution","label":"Original resolution","section":"SOURCE","modes":["standard","details","debug"],"format":"resolution"},{"id":"source_bitrate","label":"Source bitrate","section":"SOURCE","modes":["standard","details","debug"],"format":"bitrate"},{"id":"container","label":"Container","section":"SOURCE","modes":["standard","details","debug"],"format":"text"},{"id":"source_audio","label":"Source audio track","section":"SOURCE","modes":["standard","details","debug"],"format":"list","placement":"notes","note":"codec · channels · language, \"+N tracks\" when more exist."},{"id":"source_file","label":"File","section":"SOURCE","modes":["debug"],"format":"text","placement":"notes"},{"id":"av_offset","label":"AV offset","section":"SOURCE","modes":["debug"],"format":"millis","always":true,"note":"Applied offset; the container-declared value follows in parentheses when it differs."},{"id":"decode_resolution","label":"Playing resolution","section":"NOW DECODING","modes":["mini","standard","details","debug"],"format":"resolution","always":true,"note":"Positive dimensions reported by the attached player. Not reported is not zero or the original file size."},{"id":"stream_format","label":"Stream format","section":"NOW DECODING","modes":["standard","details","debug"],"format":"text","always":true,"note":"Stream or manifest metadata. Not a player picture measurement."},{"id":"device_audio","label":"Device audio output","section":"NOW DECODING","modes":["standard","details","debug"],"format":"text","always":true,"note":"Speaker or HDMI output only when reported by the platform; never inferred from the audio track."},{"id":"dynamic_range","label":"Dynamic range","section":"NOW DECODING","modes":["standard","details","debug"],"format":"text","placement":"notes","note":"Mini shows the chip form (\"DV P7 → HDR10\"); the ledger shows the sentence."},{"id":"decode_audio","label":"Stream audio track","section":"NOW DECODING","modes":["standard","details","debug"],"format":"list","placement":"notes","note":"Selected stream audio track metadata; not a claim about speaker or HDMI output."},{"id":"frames","label":"Frames","section":"NOW DECODING","modes":["standard","details","debug"],"format":"fraction","available_on":["web","android"],"note":"dropped / total. AVPlayer does not expose it."},{"id":"frame_rate","label":"Frame rate","section":"NOW DECODING","modes":["debug"],"format":"text","available_on":["web"]},{"id":"player_state","label":"Player state","section":"NOW DECODING","modes":["mini","standard","details","debug"],"format":"text","note":"One vocabulary: Playing · Paused · Buffering · Ended · Failed."},{"id":"waiting_reason","label":"Waiting reason","section":"NOW DECODING","modes":["debug"],"format":"text","placement":"notes","available_on":["apple"]},{"id":"decoder","label":"Decoder","section":"NOW DECODING","modes":["debug"],"format":"text","available_on":["web","android"],"note":"hardware / software, with the reason when software."},{"id":"stalls","label":"Buffering interruptions","section":"NOW DECODING","modes":["standard","details","debug"],"format":"text","note":"\"2 (1 supply · 1 decode)\" — player-side stall count this session."},{"id":"subtitles","label":"Subtitles","section":"NOW DECODING","modes":["standard","details","debug"],"format":"text","note":"Track name; the delivery clause (native · burned · overlay) is a note under the same label."},{"id":"source_read","label":"Source read","section":"BUFFERING / DELIVERY","modes":["debug"],"format":"text","always":true,"note":"Measured source read activity only. Configured input pacing is not a measurement; unsupported clients show Unavailable."},{"id":"server_ready","label":"Ready on server","section":"BUFFERING / DELIVERY","modes":["standard","details","debug"],"format":"seconds","always":true,"note":"Contiguous complete media beginning at the latest accepted absolute playhead or seek anchor. Missing is 0.0 s; unobservable or evicted coverage is Unavailable."},{"id":"ready_state","label":"Ready state","section":"BUFFERING / DELIVERY","modes":["debug"],"format":"text","always":true,"note":"Ready · Missing · Unavailable; kept separate so unknown is never rendered as zero."},{"id":"ready_anchor","label":"Ready anchor","section":"BUFFERING / DELIVERY","modes":["debug"],"format":"millis"},{"id":"ready_end","label":"Ready end","section":"BUFFERING / DELIVERY","modes":["debug"],"format":"millis"},{"id":"later_ready","label":"Later ready","section":"BUFFERING / DELIVERY","modes":["debug"],"format":"text","placement":"notes","note":"A later contiguous ready island is diagnostic context, never runway at the current playhead."},{"id":"http_wait","label":"HTTP wait","section":"BUFFERING / DELIVERY","modes":["standard","details","debug"],"format":"text","note":"Active server-side media responses waiting for publication. This does not describe client buffering."},{"id":"client_loaded","label":"Buffered on device","section":"BUFFERING / DELIVERY","modes":["mini","standard","details","debug"],"format":"seconds","note":"Contiguous native/browser loaded media ahead of the attached current playhead. Prepared successors never contribute."},{"id":"presentation","label":"Presentation","section":"BUFFERING / DELIVERY","modes":["standard","details","debug"],"format":"text","note":"The player's observed presentation state; it does not infer a server or network cause."},{"id":"presentation_age","label":"Last advance","section":"BUFFERING / DELIVERY","modes":["standard","details","debug"],"format":"millis","note":"Age of the last observed film-clock or frame advance. Unavailable until an advance has been observed."},{"id":"delivery_rate","label":"Server response rate","section":"BUFFERING / DELIVERY","modes":["standard","details","debug"],"format":"bitrate","note":"Server-reported completed-response rate; \"· idle\" appended when delivery has gone quiet. Completion is not proof of receipt or decode."},{"id":"delivered","label":"Server responses completed","section":"BUFFERING / DELIVERY","modes":["standard","details","debug"],"format":"bytes","note":"Server-reported completed response bytes for this session. Never labelled Transferred."},{"id":"delivery_idle","label":"Delivery idle","section":"BUFFERING / DELIVERY","modes":["debug"],"format":"millis"},{"id":"status_age","label":"Status sample age","section":"BUFFERING / DELIVERY","modes":["debug"],"format":"millis","note":"Age since this client received the currently displayed server sample."},{"id":"observed_rate","label":"Observed download rate","section":"NETWORK","modes":["standard","details","debug"],"format":"bitrate","note":"The player's own throughput estimate."},{"id":"stream_rate","label":"Stream rate","section":"NETWORK","modes":["standard","details","debug"],"format":"bitrate","note":"The declared rate of the rendition being played."},{"id":"transferred","label":"Transferred","section":"NETWORK","modes":["debug"],"format":"bytes","available_on":["apple"],"note":"Client access-log bytes — a different number from Delivered, so a different label."},{"id":"requests","label":"Requests","section":"NETWORK","modes":["debug"],"format":"count","available_on":["apple"]},{"id":"started_in","label":"Started in","section":"NETWORK","modes":["debug"],"format":"seconds","note":"Time to first frame."},{"id":"status","label":"Server state","section":"SERVER","modes":["standard","details","debug"],"format":"text","always":true,"note":"One vocabulary: No server-side session · Active · Holding buffer · Served from cache (+ the VOD states on the web). Debug shows it too."},{"id":"encoder","label":"Encoder","section":"SERVER","modes":["standard","details","debug"],"format":"text"},{"id":"encode_speed","label":"Encode speed","section":"SERVER","modes":["standard","details","debug"],"format":"speed","note":"\"(avg)\" appended when only the cumulative figure exists. Never abbreviated to Encode."},{"id":"production_actual","label":"Production actual","section":"SERVER","modes":["standard","details","debug"],"format":"seconds","note":"Actor-owned media produced beyond accepted client demand. This is producer control, not a client or server-ready buffer."},{"id":"production_target","label":"Production target","section":"SERVER","modes":["standard","details","debug"],"format":"seconds","note":"Actor-owned pacing target. Advisory policy is never relabelled as measured buffer."},{"id":"producer_state","label":"Producer","section":"SERVER","modes":["standard","details","debug"],"format":"text"},{"id":"fetch_reserve","label":"Fetch reserve","section":"SERVER","modes":["debug"],"format":"seconds","note":"Compatibility frontier measured beyond the last fetched segment, not beyond the playhead."},{"id":"ahead_bytes","label":"Fetch reserve bytes","section":"SERVER","modes":["debug"],"format":"bytes"},{"id":"produced","label":"Produced","section":"SERVER","modes":["debug"],"format":"clock"},{"id":"pacing","label":"Pacing","section":"SERVER","modes":["debug"],"format":"speed"},{"id":"held","label":"Held","section":"SERVER","modes":["debug"],"format":"yesno"},{"id":"hold_reason","label":"Hold reason","section":"SERVER","modes":["debug"],"format":"text"},{"id":"suspend_count","label":"Suspend count","section":"SERVER","modes":["debug"],"format":"count"},{"id":"demand_window","label":"Demand window","section":"SERVER","modes":["debug"],"format":"text","available_on":["web"]},{"id":"request_idle","label":"Request idle","section":"SERVER","modes":["debug"],"format":"seconds"},{"id":"last_request","label":"Last request","section":"SERVER","modes":["debug"],"format":"text","placement":"notes"},{"id":"playlist","label":"Playlist","section":"SERVER","modes":["debug"],"format":"text"},{"id":"published_end","label":"Published end","section":"SERVER","modes":["debug"],"format":"millis"},{"id":"fetched_end","label":"Fetched end","section":"SERVER","modes":["debug"],"format":"millis"},{"id":"control","label":"Control","section":"SERVER","modes":["standard","details","debug"],"format":"text","note":"Playback-control session owner and state; timing and failure are notes under the same label in Debug."},{"id":"surface_kind","label":"Surface","section":"SURFACE","modes":["standard","details","debug"],"format":"text","note":"What is drawn over the picture right now — none · indicator · banner · blocking."},{"id":"surface_class","label":"Fault","section":"SURFACE","modes":["standard","details","debug"],"format":"text","note":"The fault class the surface is projecting: preparing · buffering · recovering · hold · degraded · refused · exhausted · stopped."},{"id":"surface_source","label":"Source","section":"SURFACE","modes":["debug"],"format":"text","note":"The raw event the fault came from, as named in tests/playback/playback-surface-contract.json."},{"id":"surface_ids","label":"Attached/intent","section":"SURFACE","modes":["debug"],"format":"text","note":"The media generation the fault is about and the viewer request it belongs to, if any."},{"id":"surface_history","label":"History","section":"SURFACE","modes":["debug"],"format":"text","placement":"notes","note":"Last 16 faults: class · source · raised → cleared (by) · player at raise (rate, position, presenting, stopped_by_owner)."},{"id":"switch_frames","label":"Frames at the switch","section":"PREPARED SWITCH","modes":["debug"],"format":"text","note":"Dropped frames counted over the two seconds either side of a prepared commit, with how much of that window the samples cover. An observation, never a verdict: the bar it is measured against lives in docs/playback-control/QUALITY-SWITCH-CONTINUITY-RESULTS.md."},{"id":"switch_audio","label":"Audio at the switch","section":"PREPARED SWITCH","modes":["debug"],"format":"text","note":"Audio discontinuity over the same window — Apple access-log stalls, Android audio-sink underruns, and on the web the reason the analyser probe is deliberately not on the audible path."},{"id":"switch_visible_in","label":"Tap to new quality","section":"PREPARED SWITCH","modes":["debug"],"format":"millis","note":"Wall time from the viewer's tap to the successor's first frame. Reported, never judged."}];
// ---- end playback info fields ----
const STATS_ROWS=Object.freeze(Object.fromEntries(PLAYBACK_INFO_FIELDS.map(field=>[
  field.id,Object.freeze({
    id:field.id,label:field.label,section:field.section,modes:field.modes,
    placement:field.placement||"grid",always:!!field.always,
    build:(telemetry,mode)=>field.id==="dynamic_range"&&mode==="mini"
      ?telemetry.dynamic_range_mini:telemetry[field.id],
    note:(telemetry)=>telemetry[`${field.id}_note`]??null
  })
])));
function playbackInfoRows(mode,telemetry){
  return PLAYBACK_INFO_FIELDS.filter(field=>
    field.modes.includes(mode)&&(!field.available_on||field.available_on.includes("web")))
    .map(field=>{
      const spec=STATS_ROWS[field.id];
      let value=spec.build(telemetry,mode);
      if(value==null&&spec.always) value=["decode_resolution","stream_format","device_audio"].includes(field.id)?"Not reported":"—";
      if(value==null) return null;
      return {id:spec.id,label:spec.label,section:spec.section,placement:spec.placement,
        value:String(value),note:spec.note(telemetry),tone:telemetry[`${field.id}_tone`]||""};
    }).filter(Boolean);
}
// Overview is deliberately separate from the diagnostic field ledger. Values
// still come from the same attached-player snapshot, including cached VOD.
function playbackInfoOverview(t,live=false){
  const fact=(label,value,help="",hero=false)=>`<div class="pi-fact${hero?' pi-picture':''}"><span class="pi-label">${esc(label)}</span><strong>${esc(value||"Not reported")}</strong>${help?`<p>${esc(help)}</p>`:""}</div>`;
  const reason=t.reason||(/cached/i.test(t.method||"")?"Playing a prepared copy. Original and player picture sizes are reported separately.":/direct/i.test(t.method||"")?"The original media is delivered without server conversion.":/remux/i.test(t.method||"")?"Media is repackaged for this player; the video is not re-encoded.":"The server selected this delivery method. No further reason was reported.");
  return `<div class="pi-state">${esc(t.player_state||"Not reported")}${live?' · Live TV':''}</div><div class="pi-hero">`+
    fact("Playing resolution",t.decode_resolution,"Size reported by the attached player.",true)+
    fact(live?"Broadcast source":"Original file",t.source_resolution,t.source_video)+`</div>`+
    `<div class="pi-explanation"><strong>${esc(t.method||"Method not reported")}</strong><p>${esc(reason)}</p></div>`+
    `<div class="pi-tracks"><div>${fact("Stream audio track",t.decode_audio,"Track metadata; device output is not reported.")+(t.decode_audio?"":t.source_audio?fact("Original audio track",t.source_audio):"")}</div>${fact("Subtitles",t.subtitles,t.subtitles_note)}</div>`+
    `<div class="pi-metrics">${fact("Buffered on this device",t.client_loaded,"Contiguous media loaded ahead of your position.")}${fact(live?"Behind stream live edge":"Buffering interruptions",live?t.live_edge:t.stalls,live?"Behind the latest available media; not broadcast delay.":"Player-reported interruptions for this playback; intentional pauses excluded.")}</div>`+
    (live?`<div class="pi-explanation"><strong>Tuner reception</strong><p>${esc(t.reception||"Not reported")}</p></div>`:"");
}
function playbackInfoMarkup(mode,rows,healthWord,healthStatus){
  if(mode==="mini") return `<div class="pi-compact" data-stats-overview></div>`;
  if(mode==="standard") return `<div data-stats-overview></div>`;
  const groups=[
    ["Picture & sound",["SOURCE","NOW DECODING"]],
    ["Buffer & delivery",["BUFFERING / DELIVERY","NETWORK"]],
    ["Server work",["SERVER"]],
    ["Session & history",["PLAYBACK","SURFACE","PREPARED SWITCH"]]
  ];
  return `<p class="pi-help">Source, stream and player observations are separate. Unavailable is not zero.</p>`+groups.map(([name,sections],index)=>{
    const selected=rows.filter(row=>sections.includes(row.section));
    if(!selected.length) return "";
    return `<details class="pi-group" data-stats-group="${index}" ${index===0?'open':''}><summary>${esc(name)}</summary>`+selected.map(row=>
      `<div class="pi-row" data-stats-id="${row.id}"><div><b>${esc(row.label)}</b><small>${esc(playbackInfoHelp(row.id))}</small><small data-stats-note="${row.id}"><span class="v"></span></small></div><span class="v"></span></div>`
    ).join("")+`</details>`;
  }).join("");
}
function playbackInfoHelp(id){
  return ({stream_format:"Stream or manifest metadata; not a player picture measurement.",device_audio:"Speaker or HDMI output only when reported; never inferred from track metadata.",decode_resolution:"Attached player measurement; never inferred from source size.",source_resolution:"Original file metadata.",decode_audio:"Selected stream track; not the device's audio output.",client_loaded:"Contiguous media loaded ahead on this device.",server_ready:"Complete media ahead on the server; separate from the device buffer.",delivery_rate:"Server-completed responses; not confirmed client receipt.",observed_rate:"Player estimate during transfers; bursty by design.",stream_rate:"Stream bitrate; not connection speed.",delivered:"Bytes in server-completed responses, not proof of playback.",stalls:"Player interruptions for this playback, excluding intentional pauses.",status:"Server work state; separate from whether the picture is playing.",status_age:"Age of the most recent server response.",production_actual:"Encoder progress ahead of demand; not loaded video.",production_target:"Pacing policy, not a measurement.",http_wait:"Server responses waiting for publication; not player stalls."})[id]||"";
}
function statsToneMapPeak(health){
  if(!health||!Number.isFinite(Number(health.tone_map_peak_nits))||!health.tone_map_peak_source)return null;
  const nits=Math.round(Number(health.tone_map_peak_nits));
  if(nits<=0)return null;
  const source=String(health.tone_map_peak_source).toLowerCase();
  const provenance=source==="cll"?"source MaxCLL":source==="mdcv"?"source mastering metadata":source==="default"?"policy default":null;
  return provenance?`Tone-map peak ${nits.toLocaleString("en-US")} nits · ${provenance}`:null;
}
function patchPlaybackInfoRows(body,mode,rows,healthWord,healthStatus){
  const schema=`${mode}:`+rows.map(row=>`${row.id}:${row.placement}`).join("|");
  if(body.dataset.statsSchema!==schema){
    const open=new Set([...body.querySelectorAll("details[open]")].map(node=>node.dataset.statsGroup));
    const sameMode=(body.dataset.statsSchema||"").startsWith(`${mode}:`);
    body.innerHTML=playbackInfoMarkup(mode,rows,healthWord,healthStatus);
    if(sameMode) body.querySelectorAll("details").forEach(node=>node.open=open.has(node.dataset.statsGroup));
    body.dataset.statsSchema=schema;
  }
  for(const row of rows){
    const main=body.querySelector(`[data-stats-id="${row.id}"]`);
    if(main){
      const value=main.querySelector(":scope > .v"); if(value&&value.textContent!==row.value) value.textContent=row.value;
      main.classList.toggle("stat-good",row.tone==="good");
      main.classList.toggle("stat-warn",row.tone==="warn");
      main.classList.toggle("stat-bad",row.tone==="bad");
      main.classList.toggle("stat-muted",row.tone==="muted");
    }
    const noteRow=body.querySelector(`[data-stats-note="${row.id}"]`),note=noteRow&&noteRow.querySelector(".v");
    if(note&&note.textContent!==(row.note||"")) note.textContent=row.note||"";
    if(noteRow) noteRow.hidden=row.note==null;
  }
  const notes=body.querySelector(".snotes");
  if(notes) notes.hidden=!rows.some(row=>row.placement==="notes"||row.note!=null);
  const pill=body.querySelector("[data-stats-health]");
  if(pill){ pill.title=healthStatus; const value=pill.querySelector(".v"); if(value) value.textContent=healthWord; }
}
let STATS_TIMER=null;
let STATS_MODE="standard";
try{
  const saved=localStorage.getItem("plurx_stats_mode");
  if(["mini","standard","details","debug"].includes(saved)) STATS_MODE=saved;
}catch(e){}
function applyStatsMode(){
  const ov=document.getElementById("statsov"); if(!ov) return;
  ov.dataset.mode=STATS_MODE;
  const title=document.getElementById("statstitle"); if(title) title.textContent="Playback info";
  ov.querySelectorAll("[data-stats-mode]").forEach(button=>{
    const selected=button.dataset.statsMode===STATS_MODE;
    button.classList.toggle("on",selected);
    button.setAttribute("aria-checked",selected?"true":"false");
  });
}
function setStatsMode(mode){
  if(!["mini","standard","details","debug"].includes(mode)) return;
  STATS_MODE=mode;
  try{ localStorage.setItem("plurx_stats_mode",mode); }catch(e){}
  applyStatsMode();
  updateStats();
  positionStats();
}
function toggleStats(){
  const ov=document.getElementById("statsov"); if(!ov) return;
  const on=!ov.classList.contains("on");
  ov.classList.toggle("on",on);
  // On a phone these two panels each need most of the screen, so they take
  // turns rather than stacking (which left neither one dismissible). A mouse
  // gets both — see toggleMenu.
  const m=document.getElementById("pmenu");
  if(on && coarsePointer() && m&&m.classList.contains("on")) closeMenu(false);
  const pl=document.getElementById("player"); if(pl) pl.classList.toggle("statson",on);
  const sb=document.getElementById("statsbtn");
  if(sb){ sb.classList.toggle("on",on); sb.setAttribute("aria-expanded",on?"true":"false"); }
  clearInterval(STATS_TIMER); STATS_TIMER=null;
  if(on){
    if(PLAYER) PLAYER._statsOpener=sb;
    applyStatsMode(); updateStats(); positionStats(); STATS_TIMER=setInterval(updateStats,1000);
    const selected=ov.querySelector('[data-stats-mode][aria-checked="true"]')||ov.querySelector("[data-stats-mode]");
    if(selected) selected.focus({preventScroll:true});
  }
  else {
    ov.style.top=""; ov.style.maxHeight="";
    const opener=(PLAYER&&PLAYER._statsOpener)||sb; if(opener&&opener.focus) opener.focus({preventScroll:true});
    if(PLAYER) PLAYER._statsOpener=null;
  }
  // The panel that just appeared (or left) changes where the menu belongs, and
  // a menu open across that change is anchored to the old geometry — same
  // reason the resize listener re-runs it.
  if(m && m.classList.contains("on")) positionMenu(m.dataset.kind, m.classList.contains("low"));
  playerActivity();
}
// The top button row wraps to two or three lines on a phone, so the panel's
// ceiling isn't a constant — measure it, or the readout parks itself over the
// player's own Close button. Desktop keeps the stylesheet's fixed offset.
function positionStats(){
  const ov=document.getElementById("statsov"); if(!ov||!ov.classList.contains("on")) return;
  if(!matchMedia("(pointer:coarse)").matches){ ov.style.top=""; ov.style.maxHeight=""; return; }
  const bar=document.getElementById("pbar");
  const y=bar? Math.round(bar.getBoundingClientRect().bottom)+8 : 0;
  ov.style.top = y>0? y+"px" : "";
  // Cap the height against the transport's real top rather than pinning the
  // panel's bottom edge there: a short readout should be a short panel, which
  // on a tablet is the difference between a card and a wall.
  const tr=document.getElementById("ptimeline")||document.getElementById("ptransport");
  const floor=tr? Math.round(tr.getBoundingClientRect().top)-10 : 0;
  ov.style.maxHeight = (y>0&&floor>y)? (floor-y)+"px" : "";
}
window.addEventListener("resize",positionStats,{passive:true});
window.addEventListener("orientationchange",()=>setTimeout(positionStats,150));
function clockFromSec(s){ s=Math.max(0,Math.round(s)); const h=Math.floor(s/3600),m=Math.floor(s%3600/60),ss=s%60;
  return (h?h+":":"")+String(m).padStart(h?2:1,"0")+":"+String(ss).padStart(2,"0"); }
function hdrLabel(h){ return ({hdr10:"HDR10",hlg:"HLG",dolby_vision:"Dolby Vision"})[h]||(h?h.toUpperCase():""); }
function statsRunwayTone(seconds,suspended){
  if(suspended) return "good";
  if(seconds==null) return "muted";
  if(seconds<1.5) return "bad";
  if(seconds<5) return "warn";
  return "good";
}
function statsIdleTone(milliseconds,suspended){
  if(suspended) return "good";
  if(milliseconds==null) return "muted";
  if(milliseconds>20000) return "bad";
  if(milliseconds>8000) return "warn";
  return "";
}
function statsRateTone(rate,ahead,suspended,final){
  if(suspended||final) return "good";
  if(rate==null) return "muted";
  if(rate<.65&&(ahead||0)<2) return "bad";
  if(rate<1&&(ahead||0)<10) return "warn";
  return "good";
}
function vodServerState(health){
  if(!health) return "VOD HLS · Waiting for demand";
  switch(health.producer_state){
    case "running": return "VOD HLS · Materializing";
    case "held":
      switch(health.producer_hold){
        case "ahead": return "VOD HLS · Holding ahead window";
        case "working_set": return "VOD HLS · Holding working set";
        case "no_room": return "VOD HLS · Holding · no room";
        default: return "VOD HLS · Holding";
      }
    case "complete": return "VOD HLS · Complete";
    case "failed": return "VOD HLS · Producer failed";
    default: return "VOD HLS · Waiting for demand";
  }
}
function vodHealthWord(health){
  if(!health) return "VOD";
  switch(health.producer_state){
    case "running": return "Filling";
    case "held": return "Held";
    case "complete": return "Complete";
    case "failed": return "Failed";
    default: return "VOD";
  }
}
function playbackModeName(player){
  if(player&&player.vod) return "VOD HLS";
  if(player&&player.sessionId) return "Live HLS";
  return "Progressive file";
}
function playbackModeDetail(player){
  if(player&&player.vod) return "VOD HLS · fixed, seekable timeline";
  if(player&&player.sessionId) return "Live HLS · growing recovery timeline";
  return "Progressive file · byte-range delivery";
}
function statsCount(value){
  return String(Math.max(0,Math.trunc(Number(value)||0))).replace(/\B(?=(\d{3})+(?!\d))/g," ");
}
function statsServerReady(health){
  if(!health||health.server_ready_state==null) return {value:"Unavailable",state:"Unavailable"};
  const state=String(health.server_ready_state).toLowerCase();
  if(state==="missing") return {value:"0.0 s",state:"Missing"};
  if(state==="ready"&&Number.isFinite(Number(health.server_ready_seconds))){
    return {value:`${Math.max(0,Number(health.server_ready_seconds)).toFixed(1)} s`,state:"Ready"};
  }
  return {value:"Unavailable",state:"Unavailable"};
}
function statsHttpWait(health){
  if(!health||health.http_wait_count==null) return null;
  const count=Math.max(0,Math.trunc(Number(health.http_wait_count)||0));
  const note=count>0?[
    health.http_wait_oldest_ms==null?null:`oldest ${Math.max(0,Math.round(health.http_wait_oldest_ms))} ms`,
    health.http_wait_segment==null?null:`segment ${health.http_wait_segment}`
  ].filter(Boolean).join(" · ")||null:null;
  return {value:`${count} active`,note};
}
function playbackWaitCopy(runwaySeconds,httpWaitCount){
  const runway=Number.isFinite(Number(runwaySeconds))?Math.max(0,Number(runwaySeconds)):0;
  const waits=httpWaitCount==null?null:Math.max(0,Math.trunc(Number(httpWaitCount)||0));
  const waitText=waits==null?"server wait state unavailable":waits===0?"no server HTTP waits":
    `${waits} server HTTP ${waits===1?"wait":"waits"}`;
  return {title:"Buffering…",detail:`${runway.toFixed(1)} s client loaded · ${waitText}`};
}
// The wait sentence as it is NOW. The raise can only say what was true the
// instant the wait began — which is always "0.0 s client loaded", and on a
// cold open "server wait state unavailable" too — so the sampling tick hands
// the render a fresh one twice a second. Null when this player does not own
// the attached element: a predecessor's runway is not this wait's runway.
//
// The server count is only a reading while it is fresh: health is polled every
// two seconds while the Playback info panel is open or a media wait is live,
// and a sample older than PLAYBACK_WAIT_HEALTH_MAX_AGE_MS says "unavailable"
// rather than repeating a count from before the wait began.
const PLAYBACK_WAIT_HEALTH_MAX_AGE_MS=5000;
function playbackWaitLiveDetail(v,p,now){
  if(!v||!p||PLAYER!==p||!playbackOwnsAttachedMedia(p)) return null;
  let runway=0;
  try{ runway=bufferRunway(v); }catch(e){}
  const at=now==null?performance.now():now;
  const fresh=p.health&&p.healthObservedAt!=null&&at-p.healthObservedAt<=PLAYBACK_WAIT_HEALTH_MAX_AGE_MS;
  return playbackWaitCopy(runway,fresh?p.health.http_wait_count:null).detail;
}
// Is a media wait on screen? The health poll runs for it as well as for the
// panel, so the wait's server count is a current one.
function playbackWaitSurfaceLive(){
  const surface=PLAYBACK_SURFACE.surface;
  return !!(surface&&surface.source==="media_waiting");
}
// The half-second sampling tick: resample the wait sentence, then run the
// presenter step that paints it.
function playbackSamplingTick(v,p){
  renderPlaybackSurface.waitDetail=playbackWaitLiveDetail(v,p);
  playbackProgressTick(v,p);
  updatePlayerMediaSession(v,p);
}
function playbackStatsTelemetry(){
  const p=PLAYER||{},v=playbackOwnsAttachedMedia(PLAYER)?document.getElementById("video"):null,s=p.source||{},h=p.health||null;
  const audio=p.audio&&p.audio[p.curAudio];
  const encoder=(h&&h.encoder)||p.encoder||null;
  const rung=p.autoHeight?`${p.autoHeight}p`:null;
  const method=p.vod?"Transcode · cached":p.method==="direct_play"?"Direct play":
    p.method==="transcode"?["Transcode",encoder,rung].filter(Boolean).join(" · "):"Remux";
  const total=pbTotalSec(),position=pbShownSec();
  const range=playerRangeBadge(s);
  const switchLedger=preparedSwitchLedger(PLAYER);
  const sourceVideo=[s.video_codec&&String(s.video_codec).toUpperCase(),s.profile,
    s.bit_depth?`${s.bit_depth}-bit`:null,s.hdr_format||(s.hdr?hdrLabel(s.hdr):null)].filter(Boolean).join(" · ")||null;
  const sourceAudio=audio?[audio.codec&&String(audio.codec).toUpperCase(),
    audio.channels?fmtChannels(audio.channels):null,
    audio.language?(langName(audio.language)||audio.language):null,
    p.audio&&p.audio.length>1?`+${p.audio.length-1} tracks`:null].filter(Boolean).join(" · "):null;
  let clientLoadedSeconds=null;
  if(v) try{ clientLoadedSeconds=bufferRunway(v); }catch(e){}
  let frames=null,droppedCount=null,frameRate=p._fpsVal==null?null:p._fpsVal;
  try{
    const quality=v&&v.getVideoPlaybackQuality&&v.getVideoPlaybackQuality();
    if(quality){
      droppedCount=quality.droppedVideoFrames;
      frames=`${statsCount(quality.droppedVideoFrames)} / ${statsCount(quality.totalVideoFrames)} frames`;
      const now=performance.now(),previous=p._statsFrames;
      if(v&&!v.paused&&!v.seeking&&previous&&now-previous.at>=950){
        const measured=(quality.totalVideoFrames-previous.total)/((now-previous.at)/1000);
        p._fpsVal=frameRate=frameRate==null?measured:frameRate*0.5+measured*0.5;
      }
      if(!previous||now-previous.at>=950) p._statsFrames={total:quality.totalVideoFrames,at:now};
    }
  }catch(e){}
  const subtitle=p.curSub>=0
    ?subLabelFor((p.subs||[]).find(track=>track.index===p.curSub),p.curSub):"Off";
  const subtitleDelivery=p.burnedSub!=null?"burned":p.curSub>=0?"native":null;
  const stalls=p.stalls||0,stallKinds=p.stallsByKind||{};
  const stallParts=[stallKinds.supply?`${stallKinds.supply} supply`:null,
    stallKinds.decode?`${stallKinds.decode} decode`:null,
    stallKinds.presentation?`${stallKinds.presentation} presentation`:null].filter(Boolean);
  // Read off the presenter, not off a DOM class: the class is the projection,
  // and the model is what it is a projection OF.
  const surface=PLAYBACK_SURFACE.surface;
  const playbackState=!v?"Waiting for player":PlaybackPolicy.surfaceEntersFailedRouting(surface)?"Failed":
    v&&v.ended?"Ended":v&&v.paused?"Paused":v&&v.readyState<3?"Buffering":"Playing";
  const stale=h&&!h.final&&(h.delivered_idle_ms||0)>8000;
  const delivery=h&&h.delivered_bps!=null?`${fmtMbps(h.delivered_bps)}${stale?" · idle":""}`:"Measuring";
  const status=p.vod?vodServerState(h):h?(h.suspended?"Holding buffer":"Active"):"No server-side session";
  const healthWord=p.vod?vodHealthWord(h):h?(h.suspended?"Held":"Active"):"Direct";
  const speed=h&&h.recent_speed!=null?h.recent_speed:h&&h.speed!=null?h.speed:null;
  let serverAheadNote=null;
  if(h&&h.suspended){
    const release=h.hold_reason==="time"&&h.resume_below_seconds!=null?`time release ≤${h.resume_below_seconds} s`:
      (h.resume_below_bytes!=null?`${h.hold_reason||"buffer"} release ≤${fmtBytes(h.resume_below_bytes)}`:"held");
    serverAheadNote=[release,h.suspend_count?`${h.suspend_count} suspends`:null].filter(Boolean).join(" · ");
  }
  const level=p.hls&&p.hls.levels&&p.hls.levels[p.hls.currentLevel];
  const controlStatus=p.controlReporter&&p.controlReporter.status&&p.controlReporter.status();
  const controlHealth=h||{};
  const leaseMode=PlaybackPolicy.controlLeaseMode(controlHealth.lease_mode,p.vod,controlStatus&&controlStatus.accepted_sequence);
  const lease=PlaybackPolicy.controlLeasePresentation(leaseMode);
  const control=controlStatus?(controlStatus.accepted_sequence
    ?`${lease.ownership} · accepted #${controlStatus.accepted_sequence}`:`${lease.ownership} · connecting`):null;
  const controlNote=controlStatus&&STATS_MODE==="debug"?
    `${(controlStatus.next_exchange_ms/1000).toFixed(1)} s cadence · ${Math.round(controlStatus.lease_timeout_ms/1000)} s ${lease.label} lease`+
      (p.controlLastError?` · ${p.controlLastError.message}`:""):null;
  const ready=statsServerReady(h),httpWait=statsHttpWait(h);
  const presentationAge=p.presentationAdvancedAt==null?null:
    Math.max(0,Math.round(performance.now()-p.presentationAdvancedAt));
  const presentation=playbackState==="Playing"?"Advancing":playbackState;
  return {
    method,playback_mode:playbackModeDetail(p),position:`${clockFromSec(position)} / ${total>0?clockFromSec(total):"—"}`+
      (v&&v.playbackRate&&v.playbackRate!==1?` · ${v.playbackRate}×`:""),
    reason:p.reasons&&p.reasons.length?p.reasons.join(" · "):null,
    build:buildLabel(),transport:p.hls?(p.method==="transcode"?"Segmented MPEG-TS · MediaSource":"Segmented fMP4 · MediaSource"):
      (p.copyHls||p.sessionId)?"Segmented HLS":"Progressive file · range requests",
    file_id:p.fileId==null?null:String(p.fileId),session:p.sessionId||p.streamId||null,
    switched:p.rescuedNote||((p.abr&&p.abr.switches)||[]).map(item=>`${item.from} → ${item.to} · ${item.reason}`).join(" · ")||null,
    source_video:sourceVideo,source_resolution:s.width&&s.height?`${s.width}×${s.height}`:null,
    source_bitrate:s.bitrate?fmtMbps(s.bitrate):null,container:s.container?String(s.container).toUpperCase():null,
    source_audio:sourceAudio,source_file:s.filename||p.filename||null,
    av_offset:`${p.aoffset||0} ms`,av_offset_note:p.declared&&p.declared!==(p.aoffset||0)?`container ${p.declared>0?"+":""}${p.declared} ms`:null,
    decode_resolution:v&&v.videoWidth>0&&v.videoHeight>0?`${v.videoWidth}×${v.videoHeight}`:"Not reported",
    dynamic_range:range?range.panel:null,dynamic_range_mini:range?range.text:null,
    dynamic_range_note:statsToneMapPeak(h),
    stream_format:level?[level.width>0&&level.height>0?`${level.width}×${level.height}`:null,level.videoCodec].filter(Boolean).join(" · ")||"Not reported":"Not reported",
    device_audio:"Not reported",decode_audio:null,
    frames,frames_tone:droppedCount==null?"muted":droppedCount===0?"good":droppedCount<3?"warn":"bad",
    frame_rate:frameRate==null?null:`${frameRate.toFixed(1)} fps`,player_state:playbackState,
    decoder:p.decodeInfo?(p.decodeInfo.hw?"hardware":"software · this machine has no GPU path for this stream"):null,
    stalls:`${stalls}${stallParts.length?` (${stallParts.join(" · ")})`:""}`,
    subtitles:subtitle,subtitles_note:subtitleDelivery,
    source_read:"Unavailable",server_ready:ready.value,ready_state:ready.state,
    ready_anchor:h&&h.server_ready_anchor_ms!=null?`${Math.round(h.server_ready_anchor_ms)} ms`:null,
    ready_end:h&&h.server_ready_end_ms!=null?`${Math.round(h.server_ready_end_ms)} ms`:null,
    later_ready:h&&h.server_next_ready_start_ms!=null&&h.server_next_ready_end_ms!=null?
      `${Math.round(h.server_next_ready_start_ms)}–${Math.round(h.server_next_ready_end_ms)} ms`:null,
    http_wait:httpWait&&httpWait.value,http_wait_note:httpWait&&httpWait.note,
    client_loaded:clientLoadedSeconds==null?"Not reported":`${Math.max(0,clientLoadedSeconds).toFixed(1)} s`,
    client_loaded_tone:statsRunwayTone(clientLoadedSeconds,false),
    presentation,presentation_tone:playbackState==="Playing"?"good":
      playbackState==="Buffering"?"warn":playbackState==="Failed"?"bad":"",
    presentation_age:presentationAge==null?null:`${presentationAge} ms`,
    presentation_age_tone:presentationAge==null?"muted":presentationAge>8000?"bad":presentationAge>2000?"warn":"good",
    delivery_rate:delivery,observed_rate:p.hls&&p.hls.bandwidthEstimate?fmtMbps(p.hls.bandwidthEstimate):null,
    stream_rate:level&&level.bitrate?fmtMbps(level.bitrate):null,
    delivered:h&&h.delivered_bytes!=null?(fmtBytes(h.delivered_bytes)||"0 B"):null,
    delivery_idle:h&&h.delivered_idle_ms!=null?`${Math.round(h.delivered_idle_ms)} ms`:null,
    status_age:h&&p.healthObservedAt!=null?`${Math.max(0,Math.round(performance.now()-p.healthObservedAt))} ms`:null,
    started_in:p.ttffMs!=null?`${(p.ttffMs/1000).toFixed(1)} s`:null,
    status,encoder,encode_speed:speed==null?null:`${speed.toFixed(2)}×${h&&h.recent_speed==null?" (avg)":""}`,
    production_actual:h&&h.production_ahead_seconds!=null?`${Math.max(0,h.production_ahead_seconds).toFixed(1)} s`:null,
    production_target:h&&h.production_target_seconds!=null?`${Math.max(0,h.production_target_seconds).toFixed(1)} s`:null,
    producer_state:h&&h.producer_state?String(h.producer_state):null,
    fetch_reserve:h&&h.ahead_seconds!=null?`${Math.max(0,h.ahead_seconds).toFixed(1)} s`:null,
    fetch_reserve_note:serverAheadNote,ahead_bytes:h&&h.ahead_bytes!=null?(fmtBytes(h.ahead_bytes)||"0 B"):null,
    produced:h&&h.out_time_ms!=null?clockFromSec(h.out_time_ms/1000):null,
    pacing:h&&h.readrate!=null?`${Number(h.readrate).toFixed(2)}×`:null,
    held:h? (h.suspended?"Yes":"No"):null,hold_reason:h&&h.hold_reason||null,
    suspend_count:h&&h.suspend_count!=null?String(h.suspend_count):null,
    demand_window:h&&h.production_policy==="explicit_demand"?
      `${h.production_ahead_seconds==null?"waiting for publication":`${h.production_ahead_seconds} s produced ahead`}`+
        (h.production_target_seconds==null?"":` · target ${h.production_target_seconds} s`):null,
    request_idle:h&&h.request_idle_seconds!=null?`${Number(h.request_idle_seconds).toFixed(1)} s`:
      h&&h.request_idle_ms!=null?`${(h.request_idle_ms/1000).toFixed(1)} s`:null,
    last_request:h&&h.last_request||null,playlist:h&&h.playlist||null,
    published_end:h&&h.published_end_ms!=null?`${Math.round(h.published_end_ms)} ms`:null,
    fetched_end:h&&h.fetched_end_ms!=null?`${Math.round(h.fetched_end_ms)} ms`:null,
    control,control_note:controlNote,
    // SURFACE (contract §5): what is drawn over the picture, why, about which
    // generation, and the last sixteen — so "what was that overlay" has an
    // answer from inside the product rather than from a log file on a headless
    // box.
    surface_kind:(surface&&surface.kind)||"none",
    surface_class:(surface&&surface.class)||null,
    surface_source:(surface&&surface.source)||null,
    surface_ids:surface&&surface.class
      ?`${surface.attached==null?"—":surface.attached} / ${surface.intent==null?"no intent":surface.intent}`:null,
    surface_history:playbackSurfaceHistoryText(),
    // PREPARED SWITCH (M3): what the last prepared commit in this player
    // measured. Observations, never verdicts.
    switch_frames:switchLedger.frames,
    switch_audio:switchLedger.audio,
    switch_visible_in:switchLedger.visible,
    surface_kind_tone:surface&&surface.kind==="blocking"?"bad":
      surface&&surface.kind==="banner"?"warn":surface&&surface.kind==="indicator"?"warn":"muted",
    _health_word:healthWord,_health_status:status
  };
}
function updateStats(){
  const ov=document.getElementById("statsov"); if(!ov||!ov.classList.contains("on")) return;
  const body=document.getElementById("statsbody"); if(!body) return;
  const telemetry=playbackStatsTelemetry();
  const contractRows=playbackInfoRows(STATS_MODE,telemetry);
  patchPlaybackInfoRows(body,STATS_MODE,contractRows,telemetry._health_word,telemetry._health_status);
  const overview=body.querySelector("[data-stats-overview]");
  if(overview) overview.innerHTML=STATS_MODE==="mini"
    ?[["Playing resolution",telemetry.decode_resolution],["Playback",telemetry.player_state],["Buffered on device",telemetry.client_loaded]].map(([label,value])=>`<div class="pi-fact"><span class="pi-label">${esc(label)}</span><strong>${esc(value||"Not reported")}</strong></div>`).join("")
    :playbackInfoOverview(telemetry);
  ov.classList.toggle("onecol",!!body.querySelector(".statscols.one"));
}
// Poll the session's server-side health while the stats overlay is open, and
// only then: this is a diagnostic readout, not telemetry the server needs, and
// a poll that ran regardless would keep an abandoned session's numbers moving
// for no one. The endpoint deliberately does not count as activity, so this
// can't keep a session alive past the idle reaper either.
async function pollSessionHealth(force){
  const p=PLAYER;
  if(!playbackOwnsAttachedMedia(p)) return;
  const session=p.sessionId, stream=p.streamId, attachment=p.mediaAttachment;
  const current=()=>playbackOwnsAttachedMedia(p)&&p.sessionId===session&&p.streamId===stream
    &&p.mediaAttachment===attachment;
  const ov=document.getElementById("statsov");
  if(!force&&(!ov||!ov.classList.contains("on"))&&!playbackWaitSurfaceLive()) return;
  // Two shapes of stream, one question. An HLS session answers from the
  // transcode manager; a progressive remux answers from its own registry (see
  // progressive.rs) — that path is Chrome's whole remux experience, and until
  // it could answer, the overlay's Server section simply wasn't there. Direct
  // play has neither, because there is no server-side process to report on:
  // a file is being read, and the browser is the only one who knows how fast.
  const url = p.sessionId? `/hls/${p.sessionId}/status`
            : p.streamId? `/stream/${p.streamId}/status` : null;
  if(!url){
    if(p.health||p.healthObservedAt!=null){ p.health=null; p.healthObservedAt=null; updateStats(); }
    return;
  }
  try{
    const h=await api(url);
    if(current()) {
      p.health=h;
      p.healthObservedAt=performance.now();
      // No surface is raised or restated here: a fault is raised by the event
      // that caused it. A live media wait reads this sample through the
      // sampling tick (playbackWaitLiveDetail), which is why the poll also
      // runs while one is on screen.
      updateStats();
    }
  }catch(e){
    // A stream that has finished deregisters, and a short film's remux finishes
    // long before the viewer does. Blanking the panel at that moment is exactly
    // the vanishing-section problem this work exists to fix — so keep the last
    // numbers, which are the real final ones, and say that is what they are.
    if(!current()) return;
    // §3.3 row 18: a stats poll that could not answer is a panel, not a
    // playback — the picture is untouched and the numbers on screen are the
    // real final ones. Raised on the transition only, so a poll that keeps
    // failing every two seconds does not become a ring of its own.
    if(p.health && !p.health.final){
      p.health.final=true;
      raisePlaybackSurface("log_only",{attached:playbackSurfaceGeneration(p)});
      updateStats();
    }
    else if(!p.health) p.health=null;
  }
}
setInterval(()=>{ pollSessionHealth().catch(()=>{}); }, 2000);
// How long a paused player may stay silent before it beats anyway, even with
// nothing to report. Chosen from the shortest deadline any reader of this route
// holds: Trakt removes a session whose last beat is older than `IDLE_PAUSE`
// (150 s, `crates/plurxd/src/trakt.rs`) and its sweep runs once a minute, so a
// floor of 60 s cannot let a session fall out of that map.
const PAUSED_BEAT_FLOOR_MS=60000;
async function reportProgress(fileId, ended, attachedOwner){
  const p=attachedOwner||PLAYER;
  if(!p||p.fileId!==fileId||(!attachedOwner&&!playbackOwnsAttachedMedia(p)))return;
  if(p.libraryChannel)return;
  const video=document.getElementById("video");
  const posMs=Math.round((p.bookOffset||0)+((p.offset||0)+ (video.currentTime||0))*1000);
  // A zero beat needs a witness; a beat with a position in it does not.
  //
  // When a session never publishes a playlist the element sits at readyState 0
  // with currentTime 0 — and on a VOD or direct timeline `offset` is 0 as well,
  // so the whole reported position is 0. The server takes it, and the resume
  // point the viewer earned is gone: every later open starts at the beginning,
  // for good. Measured on a production node on 2026-09-22, one unplayable
  // 26-second startup fires five or six of these. The offset routes escaped it
  // only by accident, because their `offset` carries the position — so the
  // condition is about the number being reported, not about the route, and a
  // predecessor's real playhead still reaches the server at close.
  //
  // The witness is the CURRENT attachment having reached a timeline, not the
  // player having played at some point. A stall retry and a quality reopen both
  // reuse the player object while putting the element back to zero, and both
  // reset `offset` to 0 on the way, so evidence carried across an attachment
  // would wave exactly those through — the same bug, one button press later.
  const attachment=p.mediaAttachment||null;
  if(attachment&&video&&video.readyState>=1) p.timelineAttachment=attachment;
  if(posMs<=0&&(!attachment||p.timelineAttachment!==attachment)) return;
  // The file's probed duration is the truth. Only direct play's own
  // video.duration is the whole file: a progressive remux's grows as ffmpeg
  // writes, and an offset HLS stream's covers the tail alone. Reporting either
  // as the runtime marked films watched minutes in. With nothing trustworthy,
  // send none — the server then keeps the duration it probed at scan time, and
  // declines to auto-mark if it has none either.
  const durMs = p.bookDuration || p.knownDur
    || ((p.method==='direct_play' && video.duration && isFinite(video.duration))
        ? Math.round(video.duration*1000) : null);
  if(!ITEM_FOR_FILE[fileId]) return;
  // F-web-12. A paused player beats every five seconds for as long as it is
  // left open, repeating one position nobody has moved. What is dropped here is
  // the REPEAT, not the beat, because three readers of this route care about
  // when a beat arrives and not only about the number in it:
  //
  //   `crates/plurxd/src/delivery.rs` — a direct play has no session of its own
  //     anywhere on the server, and `DirectPlays` prunes at `IDLE_TIMEOUT`
  //     (30 s). A viewer who paused with the film already buffered issues no
  //     further range requests, so this beat is the only thing keeping them on
  //     the Activity page. Direct play is exempt and keeps every beat.
  //   `crates/plurxd/src/trakt.rs` — `sweep_loop` REMOVES a session whose last
  //     beat is older than `IDLE_PAUSE` (150 s) and scrobbles a pause; a
  //     removed session never scrobbles its stop, so the watched flip would be
  //     lost for anyone who paused for three minutes. `PAUSED_BEAT_FLOOR_MS`
  //     keeps one beat a minute going out so that never happens.
  //   `crates/plurxd/src/progress.rs` — the ten-second commit coalescer. It
  //     bounds the STORE write, not the request, and is indifferent to cadence.
  //
  // The plan (§3.6 item 3) asked for an unconditional skip on an unchanged
  // paused position. The Trakt sweep is why this one has a floor instead.
  const beatAt=performance.now();
  if(!ended && video && video.paused && p.method!=='direct_play'
     && p.lastBeatMs===posMs && p.lastBeatAttachment===attachment
     && p.lastBeatAt!=null && beatAt-p.lastBeatAt<PAUSED_BEAT_FLOOR_MS) return;
  // Recorded before the await so two beats in one window do not both post, and
  // UNrecorded if the post failed — a beat the server never received must not
  // suppress the next one, or a close that follows a failed beat would take the
  // resume point down with it.
  p.lastBeatMs=posMs; p.lastBeatAt=beatAt; p.lastBeatAttachment=attachment;
  try{ await api(`/items/${ITEM_FOR_FILE[fileId]}/progress`,{method:"POST",body:{position_ms:ended?(durMs||posMs):posMs,duration_ms:durMs}}); }
  catch(e){ p.lastBeatMs=null; p.lastBeatAt=null; }
}
function closePlayer(options={}){
  if(WATCH_CLOSE_PROMISE)return WATCH_CLOSE_PROMISE;
  const closingWatch=watchDetach();
  const closingGeneration=WATCH_GENERATION;
  const closingRoute=location.hash;
  let settleClose;
  const closingPromise=new Promise(resolve=>{settleClose=resolve;});
  WATCH_CLOSE_PROMISE=closingPromise;
  // Fence a full play() still awaiting its decision/session. PLAYER identity
  // alone cannot do that because close intentionally keeps the object around.
  PLAY_OPEN_GATE.invalidate();
  const progressPlayer=play.pendingIntent?play.pendingIntent.predecessor:
    play.failedPreparation?play.failedPreparation.predecessor:(PLAYER?.mediaPredecessor||PLAYER);
  const finalProgress=progressPlayer?.fileId?reportProgress(progressPlayer.fileId,false,progressPlayer):Promise.resolve();
  if(PLAYER&&PLAYER.libraryChannel&&!PLAYER.libraryChannelReplacing){
    LIBRARY_CHANNEL_RETURN={channelId:PLAYER.libraryChannel.channel_id};
    LIBRARY_CHANNEL_TUNE.stop();
  }
  beginPlaybackPreparation.active?.cancel();
  beginPlaybackPreparation.active?.finish();
  play.failedPreparation=null;
  play.pendingIntent=null;
  retirePlaybackPredecessor(PLAYER);
  const video=document.getElementById("video");
  const player=document.getElementById("player");
  const opener=PLAYER&&PLAYER._opener;
  const openerClick=PLAYER&&PLAYER._openerClick;
  const closingFileId=PLAYER&&PLAYER.fileId;
  if(player){ player.classList.remove("audio","lc-following"); player.style.backgroundImage=""; }
  // First, before anything is torn down: give the screen back. Hiding `#modal`
  // sets `display:none` on an ancestor, which draws nothing but does not end
  // the browser's fullscreen — `#player` stays the fullscreen element, so the
  // page keeps the whole display with an empty box on it. A desktop has Esc and
  // the habit of pressing it; an iPad has neither, only Safari's own ✕ in the
  // corner, and it reads as the app having hung. Every exit runs through here,
  // including the automatic one when a film ends with nothing queued after it.
  exitPresentationModes();
  // The lock screen and the media keys belong to a player that is open. Left
  // installed they would keep offering a transport for a film that is gone.
  clearPlayerMediaSession();
  supersedePlaybackControlIntent(PLAYER);
  // Bump the generation so a seek or audio switch still awaiting its
  // hls/start finds itself superseded and doesn't attach a stream to a player
  // the user has already closed. Closing doesn't replace PLAYER, so the
  // identity check alone would have let it through.
  if(PLAYER) PLAYER._seekToken=(PLAYER._seekToken||0)+1;
  // Hand the encoder back now rather than letting the idle reaper find it a
  // minute later. An AirPlay target fetching this same session keeps it alive
  // by fetching; a browser that has closed the player is not going to.
  // Destroy before releasing, as every other release site does. Both
  // statements are in one synchronous turn so nothing could observe the old
  // order, but a client that declares the hls.js release class is claiming
  // destroy-before-DELETE at *every* one of its release sites, and a claim
  // that happens to be true by scheduling is not one worth making.
  const closingSessionId=PLAYER&&PLAYER.sessionId;
  if(PLAYER&&PLAYER.hls) teardownHls();
  if(PLAYER) PLAYER.sessionId=null;
  if(closingSessionId) releaseSession(closingSessionId);
  // Disarm the pending seek as well as clearing it: a skip's self-commit is a
  // timer, and PLAYER survives the close, so an armed one would have fired
  // seekTo on a player the viewer had already left.
  if(PLAYER){ PLAYER.aoffset=0; PLAYER._seekDragging=false; PLAYER.controlSeek=null;
    PLAYER.wantsPlayback=false; PLAYER.pendingOpenAttempt=null; PLAYER.pendingMediaChange=null;
    PLAYER.inFlightChangeKey=null;
    cancelPendingSeek(); }
  // Same reason, same scope: the track choice was this playback's, and this
  // playback is over. PLAYER survives the close, so leaving `preplay` set would
  // make playbackSelection() keep matching on the next cold start of the same
  // file — replaying it would reuse these tracks while the detail screen's
  // pickers, which loadItem() has just reset, say Default, and a viewer who
  // changed a picker would be overruled by a choice they had moved on from.
  if(PLAYER) PLAYER.preplay=null;
  stopPlayerTimers();
  if(PLAYER&&PLAYER.controlFrameCancel) PLAYER.controlFrameCancel();
  clearInterval(STATS_TIMER); STATS_TIMER=null; teardownHls();
  // The attempt is over, so every fault about it is about nothing. §3.6: the
  // model resets with the player.
  resetPlaybackSurface();
  const ov=document.getElementById("statsov"); if(ov){ ov.classList.remove("on"); ov.style.top=""; }
  const pm=document.getElementById("pmenu"); if(pm) pm.classList.remove("on","low");
  if(PLAYER&&PLAYER._menuOpener&&PLAYER._menuOpener.setAttribute) PLAYER._menuOpener.setAttribute("aria-expanded","false");
  const sb=document.getElementById("statsbtn"); if(sb){ sb.classList.remove("on"); if(sb.setAttribute) sb.setAttribute("aria-expanded","false"); }
  const sk=document.getElementById("pskip"); if(sk){ sk.innerHTML=""; sk.dataset.k=""; }
  const pl=document.getElementById("player"); if(pl) pl.classList.remove("idle","statson");
  [...video.querySelectorAll("track")].forEach(t=>t.remove());
  resetMediaSource(video);
  document.getElementById("modal").classList.remove("open");
  const app=document.getElementById("app"); if(app) app.inert=false;
  const restoreFocus=()=>{
    const buttons=document.querySelectorAll?Array.from(document.querySelectorAll("button")):[];
    const target=buttons.find(button=>openerClick&&button.getAttribute("onclick")===openerClick)||
      buttons.find(button=>closingFileId!=null&&(button.getAttribute("onclick")||"").includes(`play(${closingFileId},`))||
      (opener&&opener.isConnected?opener:null)||document.getElementById("main");
    if(target&&target.focus) target.focus({preventScroll:true});
  };
  const current=()=>WATCH_CLOSE_PROMISE===closingPromise&&WATCH_GENERATION===closingGeneration&&!WATCH&&location.hash===closingRoute;
  Promise.resolve(finalProgress).then(async()=>{
    if(options.routeLeave||!current())return;
    const id=closingWatch?(closingWatch.accepted||(closingWatch.page?exactWireId(closingWatch.page.item):null)):
      (location.hash.startsWith("#/item/")?location.hash.split("/")[2]:null);
    if(id){await viewItem(id,current);if(current())restoreFocus();}
    else if(current())restoreFocus();
  }).catch(()=>{}).finally(settleClose);
  return closingPromise;
}
// Keyboard while the player is open: 'i' toggles stats, Esc closes (unless the
// native fullscreen is up — then Esc belongs to the browser's exit-fullscreen).
// The lightbox owns the keyboard while it's up (the player modal can't be open
// at the same time — a photo never opens the player).
window.addEventListener("keydown",e=>{
  if(!lightboxOpen()) return;
  if(e.key==="Escape"){ e.preventDefault(); closeLightbox(); }
  else if(e.key==="ArrowLeft"){ e.preventDefault(); lbStep(-1); }
  else if(e.key==="ArrowRight"){ e.preventDefault(); lbStep(1); }
});
// player-input-adapter:begin — the only place in the player that reads e.key
function playerContractInput(e,state){
  if(e.key==="Escape") return "back";
  // Back, as a television sends it. Escape is a keyboard's answer and no TV
  // remote produces it: Tizen reports 10009, webOS 461, and the Chromium-based
  // TV browsers (Fire TV Silk, Android TV) name the key `GoBack`, with
  // `BrowserBack` on desktop Chromium's own back key. Without these the page
  // navigated away from the player instead of closing it, which on a TV means
  // leaving the app. The two numeric codes have no `key` name of their own,
  // which is why both spellings are read.
  if(e.key==="GoBack"||e.key==="BrowserBack") return "back";
  if(e.keyCode===10009||e.keyCode===461) return "back";
  if(e.key==="ArrowLeft") return "left";
  if(e.key==="ArrowRight") return "right";
  if(e.key==="ArrowUp") return "up";
  if(e.key==="ArrowDown") return "down";
  if(e.key==="Enter") return "select";
  if(e.key===" ") return state==="timeline"||state==="scrub"?"select":"play_pause";
  if(e.key==="k"||e.key==="K") return "play_pause";
  if(e.key==="j"||e.key==="J") return "skip_back";
  if(e.key==="l"||e.key==="L") return "skip_forward";
  return null;
}
function playerHotkey(e,state){
  const onButton=(e.target&&e.target.tagName||"")==="BUTTON";
  // `role="slider"` promises Home/End (WAI-ARIA APG). The contract has no row
  // for either, so they behave as the pattern requires and no more: jump the
  // pending position to an end, and commit it the way Select would.
  if((state==="timeline"||state==="scrub")&&(e.key==="Home"||e.key==="End")){
    const total=pbTotalSec(); if(!(total>0)) return true;
    if(PLAYER) PLAYER._seekPending=e.key==="Home"?0:total;
    commitPendingSeek(); playerActivity(); e.preventDefault(); return true;
  }
  if((e.key==="i"||e.key==="I")&&!e.repeat){ toggleStats(); playerActivity(); e.preventDefault(); return true; }
  if(!onButton&&(e.key==="f"||e.key==="F")&&!e.repeat){ toggleFullscreen(); playerActivity(); e.preventDefault(); return true; }
  if(!onButton&&(e.key==="c"||e.key==="C")&&!e.repeat){ cycleSub(); playerActivity(); e.preventDefault(); return true; }
  // Tab is the browser's, but a focus ring the viewer cannot see is worse than
  // no focus ring: reveal the chrome and let the move happen.
  if(e.key==="Tab"){ playerActivity(); return false; }
  return false;
}
function applyPlayerOutcome(outcome,ctx={}){
  const direction=ctx.direction;
  switch(outcome){
    case "reveal": {
      playerActivity();
      const last=document.getElementById((PLAYER&&PLAYER._lastFocusedControl)||"pbplay")||document.getElementById("pbplay");
      if(last) last.focus({preventScroll:true});
      return true;
    }
    case "focus_transport": {
      const last=document.getElementById((PLAYER&&PLAYER._lastFocusedControl)||"pbplay")||document.getElementById("pbplay");
      if(last) last.focus({preventScroll:true});
      return true;
    }
    case "focus_marker_or_ignore": {
      const marker=document.querySelector("#pskip button"); if(marker) marker.focus({preventScroll:true});
      return !!marker;
    }
    case "skip":
      if(ctx.key==="ArrowLeft"||ctx.key==="ArrowRight")
        nudgeKeyboard(direction==="left"?-10:10,ctx.key);
      else nudge(direction==="left"||direction==="skip_back"?-10:10);
      return true;
    case "preview": {
      const total=pbTotalSec(); if(!(total>0)) return true;
      const step=PlaybackPolicy.previewStepSeconds(ctx.repeatCount||0);
      const sign=direction==="left"?-1:1;
      const base=PLAYER._seekPending!=null?PLAYER._seekPending:pbPosSec();
      // A preview waits for Select. If the pending position arrived from a
      // skip it is still carrying that skip's self-commit — drop it.
      clearPendingSeekTimer();
      PLAYER._seekPending=Math.max(0,Math.min(total,base+sign*step));
      pbTick(); playerActivity(); return true;
    }
    case "commit":
      commitPendingSeek(); return true;
    case "cancel":
      cancelPendingSeek(); playerActivity(); return true;
    case "cancel_then_focus_transport":
      cancelPendingSeek(); return applyPlayerOutcome("focus_transport",ctx);
    case "cancel_then_focus_marker_or_ignore":
      cancelPendingSeek(); return applyPlayerOutcome("focus_marker_or_ignore",ctx);
    case "commit_then_toggle_play": {
      commitPendingSeek(); togglePlay(); return true;
    }
    case "toggle_play": togglePlay(); return true;
    case "close_menu": closeMenu(); return true;
    case "close_info": toggleStats(); return true;
    case "hide": {
      const player=document.getElementById("player"); if(player) player.classList.add("idle");
      // `focusin` has already recorded which control this was, so `reveal`
      // can put it back. Blur, or the browser keeps focus on a control the
      // viewer can no longer see (and `hidden` would be a lie).
      const active=document.activeElement;
      if(active&&active!==document.body&&player&&player.contains(active)&&active.blur) active.blur();
      return true;
    }
    case "return_browser": watchReturnBrowser(); return true;
    case "exit": closePlayer(); return true;
    case "toggle_chrome": {
      const player=document.getElementById("player");
      if(player&&player.classList.contains("idle")) playerActivity(); else if(player) player.classList.add("idle");
      return true;
    }
    case "menu_focus": {
      const menu=document.getElementById("pmenu");
      const open=menu&&menu.classList.contains("on")?menu:document.getElementById("statsov");
      if(!open||!open.classList.contains("on")) return false;
      const items=Array.from(new Set(Array.from(
        open.querySelectorAll('button:not([disabled]),summary,[data-stats-mode]:not([disabled])'))));
      if(!items.length) return false;
      const at=items.indexOf(document.activeElement);
      const forward=direction==="down"||direction==="right";
      const next=at<0?(forward?0:items.length-1):(at+(forward?1:-1)+items.length)%items.length;
      items[next].focus();
      return true;
    }
    case "focus_row":
    case "activate":
    case "ignore":
      return false;
    default: throw new Error(`unknown player outcome ${outcome}`);
  }
}
let PLAYER_REPEAT_KEY=null;
let PLAYER_REPEAT_COUNT=0;
function handlePlayerKeydown(e){
  if(!document.getElementById("modal").classList.contains("open")) return;
  if(e.key==="Escape"&&isFullscreenAnywhere()) return;
  if(WATCH&&WATCH.mode!=="full"&&!document.getElementById("player").contains(e.target))return;
  if(e.ctrlKey||e.metaKey||e.altKey) return;
  if(/^(INPUT|TEXTAREA|SELECT)$/.test((e.target&&e.target.tagName)||"")) return;
  const state=playerInputState();
  const input=playerContractInput(e,state);
  if(input==null){ playerHotkey(e,state); return; }
  if(PLAYER_REPEAT_KEY!==e.key){ PLAYER_REPEAT_KEY=e.key; PLAYER_REPEAT_COUNT=0; }
  else if(e.repeat) PLAYER_REPEAT_COUNT+=1;
  const outcome=watchRouteInput(state,input);
  const consumed=applyPlayerOutcome(outcome,{repeat:e.repeat,repeatCount:PLAYER_REPEAT_COUNT,
    direction:input,key:e.key});
  if(consumed) e.preventDefault();
  return consumed;
}
window.addEventListener("keydown",handlePlayerKeydown);
// Only the key that owns the ladder resets it: releasing Shift mid-hold used
// to drop the acceleration back to the first rung.
window.addEventListener("keyup",e=>{
  PLAYER_SEEK_GESTURE.release(e.key,PLAYER);
  if(e.key===PLAYER_REPEAT_KEY){ PLAYER_REPEAT_KEY=null; PLAYER_REPEAT_COUNT=0; }
});
// Blur commits the target already shown on the scrubber. Close and attachment
// replacement call cancelPendingSeek(), which cancel instead because ownership
// has ended.
window.addEventListener("blur",()=>{ PLAYER_SEEK_GESTURE.blur(PLAYER); });
// Both panels are dialogs while they are open, so Tab cycles inside them
// rather than walking out into chrome the viewer cannot see. Arrow movement
// inside them belongs to `menu_focus` above — one answer, one place.
["pmenu","statsov"].forEach(id=>document.getElementById(id).addEventListener("keydown",e=>{
  if(e.key!=="Tab"||!e.currentTarget.classList.contains("on")) return;
  const items=Array.from(e.currentTarget.querySelectorAll('button:not([disabled]),summary,[data-stats-mode]:not([disabled])'));
  if(!items.length) return;
  const first=items[0],last=items[items.length-1];
  if(e.shiftKey&&document.activeElement===first){ e.preventDefault(); last.focus(); }
  else if(!e.shiftKey&&document.activeElement===last){ e.preventDefault(); first.focus(); }
}));
document.getElementById("player").addEventListener("pointerdown",e=>{
  const menu=document.getElementById("pmenu"),opener=PLAYER&&PLAYER._menuOpener;
  if(!menu.classList.contains("on")||menu.contains(e.target)||(opener&&(opener===e.target||opener.contains(e.target)))) return;
  if(e.target===document.getElementById("video")&&PLAYER) PLAYER._menuDismissedByPointer=true;
  closeMenu();
});
document.getElementById("player").addEventListener("focusin",e=>{
  const target=e.target;
  if(!PLAYER||!target||!target.id||!target.closest("#pbar,#ptimeline,#ptransport,#pskip")) return;
  PLAYER._lastFocusedControl=target.id;
});
// The other transport a viewer reaches for: the OS media keys, a headset
// button, the lock screen, the notification shade. A browser delivers none of
// those as keys — they arrive as MediaSession actions — so they are decoded
// here beside the keyboard rather than in a second adapter.
//
// Feature-detected twice over: once for `mediaSession` itself, and once per
// action, because setting a handler a browser does not implement throws
// `NotSupportedError` and one unknown action would otherwise cost every
// action after it.
function setPlayerMediaAction(action,handler){
  try{ navigator.mediaSession.setActionHandler(action,handler); }catch(e){}
}
// A MediaSession command is one more producer of a contract input, so it asks
// the same table the keys ask (§5): a blocking prompt (`failed`) ignores a
// headset press, a pending seek (`scrub`) is committed before the play state
// changes, and an open menu or panel keeps ignoring skips. Calling
// `togglePlay` or `nudge` directly, whatever state the player was in, is what
// the table used to be bypassed by.
//
// What the OS transport adds is idempotence. `play` and `pause` are separate
// handlers, and each only toggles when the viewer's INTENT differs from what
// was asked — `playerWantsPlayback`, never the element's `paused`. A pending
// open and every reattach leave the element paused while the viewer still
// wants the film playing; judged on `paused`, a `play` pressed in that window
// flipped the intent to pause and the stream attached paused.
function playerMediaPlayPause(wanted){
  const outcome=watchRouteInput(playerInputState(),"play_pause");
  if(outcome!=="toggle_play"&&outcome!=="commit_then_toggle_play") return false;
  if(outcome==="commit_then_toggle_play") commitPendingSeek();
  if(playerWantsPlayback(document.getElementById("video"))!==wanted) togglePlay();
  return true;
}
function playerMediaSkip(input,seconds){
  if(watchRouteInput(playerInputState(),input)!=="skip") return false;
  nudge(input==="skip_back"?-seconds:seconds);
  return true;
}
// Next is the next EPISODE, and the handler exists only while there can be one
// the viewer asked for: autoplay-next on, and not a title already known to be
// something other than an episode. A browser draws a Next control for any
// action that has a handler, so a handler that does nothing is a button that
// does nothing. `setAutoNext` re-syncs it when the setting changes mid-film.
function playerNextTrackOffered(){
  const kind=PLAYER&&PLAYER.meta&&PLAYER.meta.kind;
  return autoNextOn()&&(!kind||kind==="episode");
}
function syncPlayerNextTrack(){
  if(!installPlayerMediaSession.installed||!("mediaSession" in navigator)) return;
  setPlayerMediaAction("nexttrack",playerNextTrackOffered()
    ?()=>{ if(playerNextTrackOffered()) playNextEpisode(); }:null);
}
function installPlayerMediaSession(){
  if(!("mediaSession" in navigator)) return false;
  setPlayerMediaAction("play",()=>{ playerMediaPlayPause(true); });
  setPlayerMediaAction("pause",()=>{ playerMediaPlayPause(false); });
  setPlayerMediaAction("seekbackward",d=>{ playerMediaSkip("skip_back",Math.abs((d&&d.seekOffset)||10)); });
  setPlayerMediaAction("seekforward",d=>{ playerMediaSkip("skip_forward",Math.abs((d&&d.seekOffset)||10)); });
  installPlayerMediaSession.installed=true;
  syncPlayerNextTrack();
  return true;
}
function clearPlayerMediaSession(){
  installPlayerMediaSession.installed=false;
  if(!("mediaSession" in navigator)) return;
  for(const action of ["play","pause","seekbackward","seekforward","nexttrack"])
    setPlayerMediaAction(action,null);
  if(navigator.mediaSession.playbackState!==undefined) navigator.mediaSession.playbackState="none";
}
// Position, from the sampling tick the player already runs (500 ms). No new
// timer: a second clock for the lock screen is a second thing to keep in step
// with the first. Everything here is guarded because `setPositionState` throws
// on a non-finite duration or a position past it, which a stream whose
// duration is still growing produces routinely.
function updatePlayerMediaSession(v,p){
  if(!v||!p||!("mediaSession" in navigator)) return;
  // The intent, not `v.paused`: during a reattach the element is paused and the
  // viewer is not, and the OS would otherwise show Play and send `play` for a
  // press the viewer meant as pause.
  if(navigator.mediaSession.playbackState!==undefined)
    navigator.mediaSession.playbackState=playerWantsPlayback(v)?"playing":"paused";
  if(!navigator.mediaSession.setPositionState) return;
  const duration=pbTotalSec(),position=pbShownSec();
  if(!(duration>0)||!isFinite(duration)||!(position>=0)||position>duration) return;
  try{ navigator.mediaSession.setPositionState({duration,position,playbackRate:v.playbackRate||1}); }catch(e){}
}
// player-input-adapter:end
// map fileId → itemId, filled by viewItem so progress posts to the right item
const ITEM_FOR_FILE={};
