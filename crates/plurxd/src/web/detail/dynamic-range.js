"use strict";
// ---- dynamic range: source, delivered format and display evidence ----------
// MEDIA-BADGES-PLAN.md §2. The chip above answers "what is this file?"; during
// playback the viewer is asking "what am I getting?", and those two answers
// differ constantly — a DV disc remux tone-mapped to an SDR H.264 transcode
// still showed a full-colour "DV P7".
//
// Asked live for the badge's capability note; negotiation keeps its existing
// boot-time answer. CSS dynamic-range describes capability, not whether HDR
// mode is active or the actual HDMI/display output. A missing query means the
// browser did not report HDR capability, not an observed SDR rendering path.
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
// (Source file, server-reported delivery, live browser capability) → badge.
// Arrows describe the delivered grade/profile; CSS never supplies a rendered
// grade. Ordinary browsers expose no observation of the actual display output.
// Missing delivery keeps the source-only chip rather than inventing a stream.
function dynamicRangeBadge(f, delivered, displayHdr, deliveredDvProfile){
  f=f||{};
  const chip=hdrChip(f); if(!chip) return null;
  const source=sourceDynamicRange(f);
  const lit={cls:chip.cls, text:chip.text, base:chip.text, arrow:null, full:chip.full,
    aria:chip.full||chip.text, panel:RANGE_LONG[source]||chip.text,
    off:false, source, rendered:null};
  if(!delivered) return lit;
  const capability=displayHdr?"browser reports HDR capability":"browser has not reported HDR capability";
  const output="display output unverified";
  const grade=RANGE_LONG[delivered]||String(delivered).toUpperCase();
  const profile=delivered==='dolby_vision'&&deliveredDvProfile
    ?` Profile ${deliveredDvProfile}`:"";
  const delivery=`Delivered ${grade}${profile}`;
  const evidence=`${delivery} · ${output} · ${capability}`;
  if(delivered===source){
    const sourceDv=sourceDolbyVisionProfile(f);
    if(source==='dolby_vision'&&deliveredDvProfile&&sourceDv&&deliveredDvProfile!==sourceDv){
      const arrow=`DV P${deliveredDvProfile}`;
      return {...lit,text:`${chip.text} → ${arrow}`,arrow,
        full:`${chip.full} — ${evidence} (converted for this browser)`,
        aria:`Dolby Vision Profile ${sourceDv}; ${evidence}`,
        panel:`${evidence} — converted for this browser`};
    }
    return {...lit,full:`${chip.full||chip.text} — ${evidence}`,
      aria:`${chip.full||chip.text}; ${evidence}`,panel:evidence};
  }
  // A server delivery change can explain a conversion; a CSS answer cannot.
  const note=dynamicRangeReason()||(delivered==='sdr'
    ?`tone-mapped from ${RANGE_LONG[source]||'the source grade'}`
    :source==='dolby_vision'?"Dolby Vision removed for this browser":null);
  const arrow=RANGE_SHORT[delivered]||String(delivered).toUpperCase();
  return {...lit,text:`${chip.text} → ${arrow}`,arrow,
    full:`${chip.full||chip.text} — ${evidence}${note?` (${note})`:""}`,
    aria:`${RANGE_LONG[source]||chip.text}; ${evidence}`,
    panel:`${evidence}${note?` — ${note}`:""}`,off:true};
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
