"use strict";
// ---- detail helpers -------------------------------------------------------
function fmtChannels(n){ return n===8?"7.1":n===7?"6.1":n===6?"5.1":n===2?"2.0":n===1?"Mono":(n?n+"ch":""); }
function fmtMbps(bps){ return bps? (bps>=1000000? (bps/1000000).toFixed(bps>=10000000?0:1)+" Mb/s" : Math.round(bps/1000)+" kb/s") : ""; }
function fmtBytes(n){ if(!n) return ""; const u=["B","KB","MB","GB","TB"]; let i=0; while(n>=1000&&i<u.length-1){ n/=1000; i++; } return (i?n.toFixed(n>=10?0:1):Math.round(n))+" "+u[i]; }
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
