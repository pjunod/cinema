"use strict";
// ---- detail helpers -------------------------------------------------------
function itemKindLabel(it){
  return it.kind==='episode'&&it.episode_number!=null?`episode ${it.episode_number}`:it.kind||'';
}
function fmtChannels(n){ return n===8?"7.1":n===7?"6.1":n===6?"5.1":n===2?"2.0":n===1?"Mono":(n?n+"ch":""); }
function fmtMbps(bps){ return bps? (bps>=1000000? (bps/1000000).toFixed(bps>=10000000?0:1)+" Mb/s" : Math.round(bps/1000)+" kb/s") : ""; }
function fmtBytes(n){ if(n==null||!Number.isFinite(n)||n<0) return ""; const u=["B","KB","MB","GB","TB"]; let i=0; while(n>=1000&&i<u.length-1){ n/=1000; i++; } return (i?n.toFixed(n>=10?0:1):Math.round(n))+" "+u[i]; }
function audioLabel(a){ return [a.codec&&a.codec.toUpperCase(), fmtChannels(a.channels), a.language].filter(Boolean).join(" · "); }
// Apache-2.0 Google Material icon paths. Inline SVG keeps the self-hosted web
// client usable without a font or CDN connection.
function mediaIcon(kind){
  const paths={
    resolution:'M21 3H3c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h5v2h8v-2h5c1.1 0 2-.9 2-2V5c0-1.1-.9-2-2-2zm0 14H3V5h18v12z',
    video:'M18 4l2 4h-3l-2-4h-2l2 4h-3l-2-4H8l2 4H7L5 4H4c-1.1 0-1.99.9-1.99 2L2 18c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V4h-4z',
    dynamic:'m19 9 1.25-2.75L23 5l-2.75-1.25L19 1l-1.25 2.75L15 5l2.75 1.25L19 9zm-7.5.5L9 4 6.5 9.5 1 12l5.5 2.5L9 20l2.5-5.5L17 12l-5.5-2.5zM19 15l-1.25 2.75L15 19l2.75 1.25L19 23l1.25-2.75L23 19l-2.75-1.25L19 15z',
    audio:'M7 18h2V6H7v12zm-4-4h2v-4H3v4zm8 8h2V2h-2v20zm4-4h2V6h-2v12zm4-8v4h2v-4h-2z'
  };
  return `<svg class="material-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="${paths[kind]||paths.video}"></path></svg>`;
}
// Video-type badges from a file's probed specs. hdr_format carries the rich
// label (Dolby Vision profile etc.); DV gets its own gold badge.
// Use both edges so portrait metadata such as 1080×1920 stays 1080p,
// while a cropped 3840×1608 scope encode still identifies as 2160p.
function resLabel(w,h){ w=w||0; h=h||0;
  const short=w&&h?Math.min(w,h):(h||w), long=w&&h?Math.max(w,h):short;
  if(long>=3200||short>=1700) return '2160p';
  if(long>=2300||short>=1300) return '1440p';
  if(long>=1600||short>=900) return '1080p';
  if(long>=1100||short>=650) return '720p';
  if(long>=700||short>=400) return '480p';
  return short?short+'p':null; }
function codecLabel(c){ if(!c) return null; return ({hevc:'HEVC',h265:'HEVC',h264:'H.264',avc1:'H.264',av1:'AV1',vp9:'VP9',vc1:'VC-1',mpeg2video:'MPEG-2'})[c.toLowerCase()]||c.toUpperCase(); }
// Badges stay terse: "Dolby Vision · Profile 7 (HDR10-compatible)" → "DV P7"
// (the spelled-out string lives in the Video detail row and the hover title).
function hdrChip(f){
  const s=f.hdr_format||"", cls=(f.hdr==='dolby_vision'?'dv':'hdr');
  if(f.hdr==='dolby_vision'||/dolby/i.test(s)){
    const p=sourceDolbyVisionProfile(f);
    return {cls:'dv', text:p?('DV P'+p):'DV', full:s||'Dolby Vision'};
  }
  if(s) return {cls, text:s, full:s};                    // HDR10+ / HDR10 / HLG are already short
  if(f.hdr) return {cls, text:hdrLabel(f.hdr), full:hdrLabel(f.hdr)};
  return null; }

// ---- Offload: Curator deleted the files to free disk; the title stays ----
// Badge for a card or a page. A container is "offloaded" only when every leaf
// is; a partly offloaded one carries a count instead (`rollup.offloaded`).
function offloadedBadge(it){
  if(!it) return "";
  if(it.offloaded) return `<span class="offbadge" title="Curator removed the files to free disk space">Offloaded</span>`;
  const n=it.rollup&&it.rollup.offloaded;
  return n>0?`<span class="offbadge" title="Curator removed the files of ${n} of these to free disk space">${n} offloaded</span>`:"";
}
// What replaces Play on an offloaded title. Only admins can ask Curator for it
// back, so everyone else just sees where it stands. The answer and its state
// are shown inline, in the same block as the button.
function offloadActions(it){
  const id=exactWireId(it);
  const ask=ME&&ME.is_admin
    ? `<button class="btnplay" id="offask" onclick="getItBack('${id}')">Get it back</button>` : "";
  return `<div class="actions offactions">${ask}</div>
    <div class="offstatus" id="offstatus" role="status" aria-live="polite">Offloaded · the files were removed to free disk space</div>`;
}
// A partly offloaded show or season: the same button, for the leaves that are.
function offloadContainerActions(it){
  const n=it&&it.rollup&&it.rollup.offloaded;
  if(!(n>0)||!(ME&&ME.is_admin)) return "";
  const id=exactWireId(it);
  return `<div class="actions offactions"><button class="ghost" id="offask" onclick="getItBack('${id}')">Get ${n} back</button></div>
    <div class="offstatus" id="offstatus" role="status" aria-live="polite"></div>`;
}
function offloadLine(a){
  if(!a) return "";
  return a.message||a.state||"";
}
async function getItBack(id){
  const btn=document.getElementById("offask"), line=document.getElementById("offstatus");
  if(btn) btn.disabled=true;
  if(line) line.textContent="Asking Curator…";
  try{
    const a=await api(`/items/${id}/get-it-back`,{method:"POST",body:{}});
    if(line) line.textContent="Get it back: "+offloadLine(a);
    watchOffload(id);
  }catch(e){
    if(line) line.textContent=(e&&e.message)||"Curator could not be asked.";
  }
  if(btn) btn.disabled=false;
}
// While an offloaded title's page is open, read Curator's state for it. The
// read is a pass-through: nothing is stored, and the loop ends with the page.
// The page body is inserted after the renderer that calls this returns, so the
// first read is deferred a tick and a missing status line is tolerated a few
// times before the loop gives up. It polls every 15 s while the page is
// visible, and a title that is no longer offloaded (409 not_offloaded, or
// Curator says on_disk) re-renders the page.
let OFFWATCH=null;
const OFFWATCH_MS=15000;
function stopOffloadWatch(){
  if(!OFFWATCH) return;
  clearInterval(OFFWATCH.timer);
  document.removeEventListener("visibilitychange",OFFWATCH.onVisible);
  OFFWATCH=null;
}
function watchOffload(id){
  if(OFFWATCH&&OFFWATCH.id===id) return;
  stopOffloadWatch();
  const watch={id,misses:0,busy:false};
  const tick=async()=>{
    if(OFFWATCH!==watch||document.hidden||watch.busy) return;
    const line=document.getElementById("offstatus");
    if(!line||!location.hash.endsWith("/item/"+id)){
      if(++watch.misses>=3) stopOffloadWatch();
      return;
    }
    watch.misses=0; watch.busy=true;
    try{
      const a=await api(`/items/${id}/offload-status`);
      if(OFFWATCH!==watch) return;
      if(a.state==="on_disk"){ stopOffloadWatch(); render(); return; }
      line.textContent="Offloaded · "+offloadLine(a);
    }catch(e){
      if(OFFWATCH!==watch) return;
      if(e&&e.code==="not_offloaded"){ stopOffloadWatch(); render(); return; }
      line.textContent="Offloaded · curator unreachable";
    }finally{ watch.busy=false; }
  };
  watch.onVisible=()=>{ if(!document.hidden) tick(); };
  watch.tick=tick;
  watch.timer=setInterval(tick,OFFWATCH_MS);
  document.addEventListener("visibilitychange",watch.onVisible);
  OFFWATCH=watch;
  setTimeout(tick,0);
}
