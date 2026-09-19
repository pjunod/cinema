"use strict";
// ---- dynamic range: source vs delivered vs rendered ------------------------
// MEDIA-BADGES-PLAN.md §2. The chip above answers "what is this file?"; during
// playback the viewer is asking "what am I getting?", and those two answers
// differ constantly — a DV disc remux tone-mapped to an SDR H.264 transcode
// still showed a full-colour "DV P7".
//
// Asked LIVE, not read from PLAY_CAPS: the probe's copy is the boot-time
// answer the server was given, and a window dragged from an HDR laptop panel
// to an SDR external monitor changes what is being rendered without changing
// one thing about the session. A browser too old for matchMedia answers no —
// which is exactly what the server was told, so the two stay consistent.
function displayIsHdr(){
  try{ return !!(window.matchMedia && window.matchMedia("(dynamic-range: high)").matches); }
  catch(e){ return false; }
}
// The coarse grade of a source, in the server's own vocabulary, so source and
// `delivered_dynamic_range` compare by string equality. hdrChip accepts a file
// whose `hdr` was never set but whose `hdr_format` names a grade, so this has
// to read both.
function sourceDynamicRange(f){
  const s=f.hdr_format||"";
  if(f.hdr==='dolby_vision'||/dolby/i.test(s)) return 'dolby_vision';
  if(f.hdr) return f.hdr;
  if(/hlg/i.test(s)) return 'hlg';
  return s?'hdr10':null;                                 // "HDR10+" / "HDR10"
}
// The source's Dolby Vision profile as a number.
//
// Column first, label second — the same order `playback::dolby_vision_profile`
// asks on the server, and it has to be the same order: the delivered profile
// beside it is derived from the column, so reading the label here would let a
// row whose two disagree invent an arrow ("DV P7 → DV P8") over a stream
// nothing converted. The label stays as the fallback for rows scanned before
// the columns existed.
//
// Null means no profile is known — a pre-column row, or a file that is not
// Dolby Vision — in which case there is nothing to compare against and the
// chip stays as it was rather than guessing.
function sourceDolbyVisionProfile(f){
  const column=f&&f.dolby_vision&&f.dolby_vision.profile;
  if(column) return Number(column);
  const m=/profile\s*(\d+)/i.exec((f&&f.hdr_format)||"");
  return m?Number(m[1]):null;
}
const RANGE_SHORT={dolby_vision:'DV',hdr10:'HDR10',hlg:'HLG',sdr:'SDR'};
const RANGE_LONG={dolby_vision:'Dolby Vision',hdr10:'HDR10',hlg:'HLG',sdr:'SDR'};
// The server's reason string already says *why* ("Dolby Vision metadata
// removed for this device; compatible HDR base kept") — better than anything
// this file could invent. Only borrow one that is actually about the picture.
function dynamicRangeReason(){
  try{
    if(typeof PLAYER==='undefined'||!PLAYER||!PLAYER.reasons) return null;
    return PLAYER.reasons.find(r=>r&&/dolby vision|hdr/i.test(r))||null;
  }catch(e){ return null; }
}
// Pure: (source file, delivered grade from the server, live display answer) →
// the chip, or null when the source is SDR (no grade to report on).
//
//   {cls, text, base, arrow, full, aria, panel, off, source, rendered}
//
// `delivered` falsy — no session yet, an old server, or a session the store
// could not resolve (§3.2) — degrades to exactly today's source-only chip.
function dynamicRangeBadge(f, delivered, displayHdr, deliveredDvProfile){
  f=f||{};
  const chip=hdrChip(f); if(!chip) return null;
  const source=sourceDynamicRange(f);
  const lit={cls:chip.cls, text:chip.text, base:chip.text, arrow:null, full:chip.full,
    aria:chip.full||chip.text, panel:RANGE_LONG[source]||chip.text,
    off:false, source, rendered:null};
  if(!delivered) return lit;                             // today's chip, exactly
  // Delivered bits are necessary, not sufficient: HDR10 on an SDR monitor is
  // delivered HDR and rendered SDR.
  const displayLoss = delivered!=='sdr' && !displayHdr;
  const rendered = displayLoss ? 'sdr' : delivered;
  if(rendered===source){
    // Same grade, different profile: the Profile 7 → 8.1 conversion
    // (PLAYBACK-CAPS-V2-PLAN §4.8). Neither half dims — the base layer is
    // copied byte for byte, nothing is re-encoded, and what reaches this
    // browser really is Dolby Vision — but the profile on screen is not the
    // profile on disk, and a chip that said plain `DV P7` would be describing
    // the file rather than the picture (MEDIA-BADGES-PLAN §2.3).
    const sourceDv=sourceDolbyVisionProfile(f);
    if(source==='dolby_vision' && deliveredDvProfile && sourceDv && deliveredDvProfile!==sourceDv){
      const arrow=`DV P${deliveredDvProfile}`;
      return {cls:chip.cls, text:`${chip.text} → ${arrow}`, base:chip.text, arrow,
        full:`${chip.full} — playing as Dolby Vision Profile ${deliveredDvProfile} `
          +`(converted for this browser; the HDR10-compatible base layer is copied untouched)`,
        aria:`Dolby Vision Profile ${sourceDv}, playing as Dolby Vision Profile ${deliveredDvProfile}`,
        panel:`Dolby Vision Profile ${deliveredDvProfile} — converted for this browser`,
        off:false, source, rendered};
    }
    return {...lit, rendered, panel:`${RANGE_LONG[source]} (rendering)`};
  }
  const note = displayLoss ? "this browser did not expose HDR presentation"
    : (dynamicRangeReason()
      || (rendered==='sdr' ? `tone-mapped from ${RANGE_LONG[source]||'the source grade'}`
        : source==='dolby_vision' ? "Dolby Vision removed for this browser"
        : `delivered as ${RANGE_LONG[rendered]||rendered}`));
  const arrow=RANGE_SHORT[rendered]||String(rendered).toUpperCase();
  const long=RANGE_LONG[rendered]||arrow;
  return {cls:chip.cls, text:`${chip.text} → ${arrow}`, base:chip.text, arrow,
    full:`${chip.full} — playing as ${long} (${note})`,
    aria:`${RANGE_LONG[source]||chip.text}, playing as ${long}`,
    panel:`${long} — ${note}`, off:true, source, rendered};
}
// The badge for whatever this player is doing right now.
//
// One reader of `PLAYER`, so the four surfaces that paint the chip — the fact
// badges, the play overlay, the stats panel and the debug ledger — cannot pass
// `dynamicRangeBadge` three of its four arguments between them. Dropping one
// silently degrades the chip to the state it had before that argument existed,
// which is a correct-looking badge for the wrong delivery.
function playerRangeBadge(f){
  return dynamicRangeBadge(f, PLAYER.deliveredRange, displayIsHdr(), PLAYER.deliveredDvProfile);
}
function premiumAudio(f){ const cs=(f.audio_streams||[]).map(a=>(a.codec||'').toLowerCase());
  if(cs.some(c=>c.includes('truehd'))) return 'TrueHD';
  if(cs.some(c=>c.includes('dts'))) return 'DTS';
  if(cs.some(c=>c.includes('eac3'))) return 'DD+';
  return null; }
function specBadges(f){
  const b=[];
  const r=resLabel(f.width,f.height); if(r) b.push(`<span class="vbadge res">${mediaIcon('resolution')}${r}</span>`);
  const c=codecLabel(f.video_codec); if(c) b.push(`<span class="vbadge codec">${mediaIcon('video')}${esc(c)}</span>`);
  if(f.bit_depth>=10) b.push(`<span class="vbadge depth">${f.bit_depth}-bit</span>`);
  const h=hdrChip(f); if(h) b.push(`<span class="vbadge ${h.cls}" title="${esc(h.full||h.text)}">${mediaIcon('dynamic')}${esc(h.text)}</span>`);
  // Keep the established movie badge policy (only premium audio) while giving
  // an audio-only source a useful primary-codec badge.
  const au=premiumAudio(f)||(!f.video_codec&&(f.audio_streams||[])[0]&&codecLabel(f.audio_streams[0].codec));
  if(au) b.push(`<span class="vbadge audio">${mediaIcon('audio')}${esc(au)}</span>`);
  return b.join("");
}
function playbackAudioChip(a){
  if(!a) return null;
  const codec=(a.codec||'').toLowerCase(), title=a.title||'';
  let mark='', full='';
  if(/atmos/i.test(title)){ mark='ATMOS'; full='Dolby Atmos'; }
  else if(codec==='eac3'||codec==='e-ac-3'){ mark='DD+'; full='Dolby Digital Plus'; }
  else if(codec==='ac3'||codec==='ac-3'){ mark='DD'; full='Dolby Digital'; }
  else if(codec==='truehd'){ mark='TRUEHD'; full='Dolby TrueHD'; }
  else if(codec==='dts'||codec==='dca'){ mark='DTS'; full='DTS'; }
  else if(codec){ mark=codec.toUpperCase(); full=mark; }
  if(!mark) return null;
  const channels=fmtChannels(a.channels), text=[mark,channels].filter(Boolean).join(' ');
  return {text,full:[full,channels].filter(Boolean).join(' ')};
}
function playerFactBadges(){
  if(!PLAYER) return '';
  const s=PLAYER.source||{}, b=[];
  const r=resLabel(s.width,s.height); if(r) b.push(`<span class="vbadge res">${mediaIcon('resolution')}${r}</span>`);
  // Source vs delivered vs rendered, not source alone — this row is on screen
  // during playback, where "what am I getting?" is the question. The detail
  // page's specBadges() deliberately stays source-only: there is no session to
  // report on there.
  const h=playerRangeBadge(s);
  if(h) b.push(`<span class="vbadge ${h.cls}${h.off?' off':''}" title="${esc(h.full||h.text)}" aria-label="${
    esc(h.aria||h.text)}">${mediaIcon('dynamic')}<span class="vsrc">${esc(h.base)}</span>${
    h.arrow?`<span class="varrow">→ ${esc(h.arrow)}</span>`:''}</span>`);
  const au=playbackAudioChip(PLAYER.audio&&PLAYER.audio[PLAYER.curAudio]);
  if(au) b.push(`<span class="vbadge audio" title="${esc(au.full)}">${mediaIcon('audio')}${esc(au.text)}</span>`);
  return b.join('');
}
